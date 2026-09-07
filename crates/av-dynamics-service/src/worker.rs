//! GMAT is a process-wide singleton and is **not thread-safe**
//! (`gmat_sys::engine_lock()`'s own doc comment; `docs/adr/002-dynamics-contract.md`'s
//! amendment: "all handles are `!Send`; parallelism is by process"). `gmat_sys::Object` /
//! `DerivativeModel` hold raw pointers into GMAT's configuration, so the type system
//! already refuses to let them cross a thread boundary on their own -- but a `tonic`
//! service method is an `async fn` that `tokio`'s multi-threaded runtime may poll from any
//! worker thread, so this server needs its own explicit rule, not just the compiler's.
//!
//! **The rule here**: exactly one dedicated `std::thread` ("the GMAT worker") ever touches
//! GMAT, for the life of the process. It is spawned once ([`WorkerHandle::spawn`]), builds
//! this server's one force-model/spacecraft configuration ([`Worker::build`], matching
//! `gmat_service.model.GmatModel.warm_up`'s "load once, reuse forever" contract) before the
//! server starts accepting requests, then loops forever taking [`Job`]s off an
//! `std::sync::mpsc` channel and running each to completion before taking the next --
//! serializing every GMAT touch by construction, the same guarantee
//! `gmat_service.server.serve`'s `futures.ThreadPoolExecutor(max_workers=1)` gives on the
//! Python side, achieved here with a plain OS thread instead of an executor because this
//! server's async layer (`tonic`) is not itself a thread pool GMAT work could be pinned
//! to -- it is `tokio`'s, which this crate must NOT let touch GMAT.
//!
//! [`WorkerHandle::run`] is the one way an `async fn` (a tonic RPC handler) reaches the
//! worker thread: it packages a closure as a boxed [`Job`], sends it down the channel, and
//! awaits a `tokio::sync::oneshot` reply the worker thread fills in after running the
//! closure. The closure itself runs entirely on the worker thread with `&mut Worker` in
//! hand, so it can call straight into `gmat_sys`/`av_dynamics` without any `Send` bound on
//! GMAT's own types -- only the *inputs* captured by the closure (plain owned `Vec<f64>`,
//! `i64`, ...) and the *output* sent back need to be `Send`, which every CDM value already
//! is.
//!
//! `WorkerHandle::spawn` acquires and holds [`gmat_sys::engine_lock()`] for the entire
//! life of the worker thread (not per-call): this process has exactly one thread that will
//! ever call into GMAT, so there is no contention to serialize against *within* this
//! process, but holding the lock for the process's whole run documents and enforces that
//! invariant mechanically -- a future change that accidentally spawned a second
//! GMAT-touching thread in this binary would block on the lock forever (a hang, not
//! silently corrupted GMAT global state), which is the failure mode this crate wants.
use std::collections::BTreeMap;
use std::sync::mpsc;
use std::sync::Arc;

use av_cdm::pb::{ModelCapability, ModelInfo};
use av_dynamics::stm::StmAugmented;
use av_dynamics::DynamicsModel;
use gmat_sys::model::{GmatModel, GmatModelInfo};
use gmat_sys::{Gmat, GmatError, Object};

use crate::config;

/// A unit of GMAT work, run to completion on the worker thread with `&mut Worker` in hand.
/// `Send` because it crosses the `std::sync::mpsc` channel from the caller's (tokio) thread
/// to the worker thread -- see the module doc for why the closure's *body* need not be.
type Job = Box<dyn FnOnce(&mut Worker) + Send + 'static>;

/// Matches `gmat_service.model._write_back`'s exact field sequence (`DateFormat`, `Epoch`,
/// `CoordinateSystem`, `DisplayStateType`, then the six Cartesian fields) -- the same order
/// that module's own comment notes is shared, independently, by
/// `altavista/scenario.py`'s `Spacecraft._write_back` and `goldens/gen_leo_1day.py`.
fn write_back(obj: &Object, epoch_a1mjd: f64, state_km: [f64; 6]) -> Result<(), GmatError> {
    obj.set_str("DateFormat", "A1ModJulian")?;
    // Rust's `f64` `Display` prints the shortest decimal that round-trips back to the
    // exact same `f64` (unlike a fixed-precision format), matching Python's `repr(float)`
    // closely enough for this purpose: a value GMAT parses back to the same epoch.
    obj.set_str("Epoch", &format!("{epoch_a1mjd}"))?;
    obj.set_str("CoordinateSystem", config::FRAME_ID)?;
    obj.set_str("DisplayStateType", "Cartesian")?;
    for (field, v) in ["X", "Y", "Z", "VX", "VY", "VZ"].iter().zip(state_km) {
        obj.set_real(field, v)?;
    }
    Ok(())
}

/// Builds one `ForceModel` (JGM2 8x8 + Luna/Sun point masses, no drag, no SRP -- matching
/// `config::settings()`/the golden exactly) bound to a fresh `Spacecraft` named with
/// `suffix`, and returns the two objects `Gmat::derivative_model`/`derivative_model_with_stm`
/// need. A representative, never-propagated Cartesian state seeds the spacecraft (mirrors
/// `gmat_service.model.GmatModel._build_derivatives_engine`'s own comment: this state is
/// "never actually propagated, only used to build the PropagationStateManager/ForceModel
/// graph" -- every real call passes its own state into `derivatives`/`step`).
fn build_force_model_and_sat(gmat: &Gmat, suffix: &str) -> Result<(Object, Object), GmatError> {
    let sat = gmat.construct("Spacecraft", &format!("av_dynsvc_sat_{suffix}"))?;
    write_back(&sat, config::REFERENCE_EPOCH_A1MJD, [7000.0, 0.0, 0.0, 0.0, 7.5, 0.0])?;

    let fm = gmat.construct("ForceModel", &format!("av_dynsvc_fm_{suffix}"))?;
    fm.set_str("CentralBody", "Earth")?;
    // Empty names for GravityField/PointMassForce: the same convention
    // `crates/gmat-sys/tests/leo_golden.rs` already exercises (including constructing two
    // independent force models with third-body forces in one process --
    // `two_models_in_one_process_do_not_interfere`), so this is a proven-safe pattern, not
    // a new assumption about GMAT's auto-naming.
    let grav = gmat.construct("GravityField", "")?;
    grav.set_str("BodyName", "Earth")?;
    grav.set_str("PotentialFile", "JGM2.cof")?;
    grav.set_int("Degree", 8)?;
    grav.set_int("Order", 8)?;
    fm.add_force(&grav)?;
    for body in ["Luna", "Sun"] {
        let pm = gmat.construct("PointMassForce", "")?;
        pm.set_str("BodyName", body)?;
        fm.add_force(&pm)?;
    }
    gmat.initialize()?;
    Ok((fm, sat))
}

/// Everything the GMAT worker thread owns: the two bound models this server ever needs.
/// ADR-002 depth 2 needs no separate "Step" vs. "Propagate" vs. "Derivatives" object triple
/// the way `gmat_service.model.GmatModel` (depth 1, GMAT's own stateful `Propagator`) does:
/// `gmat_sys::model::GmatModel::derivatives` is a pure function of `(state, dt)` -- it
/// never mutates the bound `Spacecraft`'s own internal state (`crates/gmat-sys/tests/
/// leo_golden.rs`'s `two_models_in_one_process_do_not_interfere` measures exactly this) --
/// so one bound model safely serves every RPC that does not need the STM, and a second one
/// (the 42-state, STM-augmented model) serves the one that does.
pub struct Worker {
    /// dimension 6: `Derivatives`, `Step`, `Propagate(covariance=false)`.
    pub model: GmatModel,
    /// dimension 42 (STM-augmented, via `av_dynamics::stm::StmAugmented`): `Propagate
    /// (covariance=true)` only.
    pub stm_model: StmAugmented<GmatModel>,
}

impl Worker {
    /// Builds both models and the `Describe` `ModelInfo` `WorkerHandle::spawn` needs.
    /// Idempotent only in the sense `gmat_sys::Gmat::setup` itself is (a `Once`) -- calling
    /// this **twice in one process** would `Construct` a second, identically-named
    /// `Spacecraft`/`ForceModel` pair and is not attempted anywhere in this crate; it runs
    /// exactly once, on the GMAT worker thread, at process startup.
    fn build() -> Result<(Worker, ModelInfo), GmatError> {
        let gmat = Gmat::setup(&Gmat::default_startup_file())?;

        let (fm6, sat6) = build_force_model_and_sat(&gmat, "plain")?;
        let dm6 = gmat.derivative_model(&fm6, &sat6)?;
        assert_eq!(dm6.dimension(), 6);

        let (fm42, sat42) = build_force_model_and_sat(&gmat, "stm")?;
        let dm42 = gmat.derivative_model_with_stm(&fm42, &sat42)?;
        assert_eq!(dm42.dimension(), 42);

        let settings: BTreeMap<String, String> = config::settings();
        let info = GmatModelInfo {
            id: config::MODEL_ID.to_string(),
            version: config::GMAT_VERSION.to_string(),
            state_space_id: config::STATE_SPACE_ID.to_string(),
            frame_id: config::FRAME_ID.to_string(),
            goldens: vec![config::GOLDEN_NAME.to_string()],
            // This server's fixed force model (JGM2 8x8 + Luna/Sun point masses) never adds
            // RelativisticCorrection -- see config.rs's module doc; question 82's gate is
            // wired (GmatModel::stm_capable reads it) but never trips for this model today.
            has_relativistic_correction: false,
        };
        let model = GmatModel::new(dm6, info.clone(), &settings, false);
        let model_stm = GmatModel::new(dm42, info, &settings, false);
        assert!(model_stm.stm_capable(), "the 42-state model must declare STM capability");
        let stm_model = StmAugmented::new(model_stm);

        // `Describe`'s ModelInfo, fully formed once here (Send + Clone, plain owned
        // Rust/CDM types) so the tonic service can answer `Describe` without a worker-thread
        // round trip at all -- see crate::service's own doc comment. `model.describe()`
        // (the *plain*, 6-state model `av_dynamics::DynamicsModel::describe()`) reports only
        // Derivatives/Step/Deterministic -- its own `stm_capable()` is `false` by
        // construction (dimension 6, no STM block). Two capabilities are added by hand:
        // `Propagate` is this *service*'s own RPC, layered above the `DynamicsModel` trait
        // (ADR-002: "propagate ... belongs above step"), matching
        // `gmat_service.service._capabilities()`'s equivalent addition; `Stm` reflects that
        // `Propagate(covariance=true)` is served by the *separate* `stm_model` above, whose
        // own `stm_capable()` is the actual source of truth (question 82's gate: `false` only
        // if a future force model sets `has_relativistic_correction` without
        // `accept_missing_stm_terms` -- see that field's doc comment).
        let mut model_info = model.describe();
        model_info.capabilities.push(ModelCapability::Propagate as i32);
        if stm_model.inner().stm_capable() {
            model_info.capabilities.push(ModelCapability::Stm as i32);
        }

        Ok((Worker { model, stm_model }, model_info))
    }
}

/// The tonic-service-facing side of the GMAT worker thread: a channel to submit [`Job`]s
/// on, plus the immutable, already-computed values `Describe` needs (no round trip).
#[derive(Clone)]
pub struct WorkerHandle {
    tx: mpsc::Sender<Job>,
    pub model_info: Arc<ModelInfo>,
    pub settings_hash: Arc<String>,
}

impl WorkerHandle {
    /// Spawns the dedicated GMAT worker thread, runs [`Worker::build`] on it (so the very
    /// first `gmatpy`-equivalent call this process ever makes happens there, not on a
    /// caller's tokio thread), and blocks the calling thread until that warm-up completes
    /// -- mirroring `gmat_service.server.serve`'s `executor.submit(servicer.warm_up)
    /// .result()` before `server.start()`.
    pub fn spawn() -> Result<WorkerHandle, GmatError> {
        let (tx, rx) = mpsc::channel::<Job>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<ModelInfo, GmatError>>();

        std::thread::Builder::new()
            .name("gmat-worker".to_string())
            .spawn(move || {
                // Held for the life of this thread -- see the module doc's last paragraph.
                let _engine = gmat_sys::engine_lock();
                match Worker::build() {
                    Ok((mut worker, model_info)) => {
                        if ready_tx.send(Ok(model_info)).is_err() {
                            return; // spawn()'s caller already gave up; nothing to serve.
                        }
                        for job in rx {
                            job(&mut worker);
                        }
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                    }
                }
            })
            .expect("failed to spawn the gmat-worker thread");

        let model_info = ready_rx.recv().expect("gmat-worker thread died before reporting warm-up status")?;
        Ok(WorkerHandle { tx, model_info: Arc::new(model_info), settings_hash: Arc::new(config::settings_hash()) })
    }

    /// Runs `f` on the GMAT worker thread and returns its result. See the module doc.
    pub async fn run<T, F>(&self, f: F) -> T
    where
        T: Send + 'static,
        F: FnOnce(&mut Worker) -> T + Send + 'static,
    {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        let job: Job = Box::new(move |w| {
            let _ = reply_tx.send(f(w));
        });
        self.tx.send(job).expect("gmat-worker thread is no longer running");
        reply_rx.await.expect("gmat-worker dropped the reply channel (thread panicked?)")
    }
}
