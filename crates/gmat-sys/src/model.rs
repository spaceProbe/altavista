//! `GmatModel`: the [`av_dynamics::DynamicsModel`] binding over [`crate::DerivativeModel`]
//! (ADR-002 depth 2).
//!
//! **This module is the km<->m and A1MJD<->TAI boundary.** Everything above `GmatModel`
//! (`av-dynamics`'s trait, `av-kernel`'s scheduler and clock) works in SI metres, metres per
//! second and TAI nanoseconds; everything below it (`crate::DerivativeModel`, GMAT's own
//! propagators and coordinate systems) works in kilometres, kilometres per second and A.1
//! Modified Julian Date. Every crossing goes through `av_cdm::units` (the km/m helpers) and
//! `av_cdm::Tai` (the A1MJD/TAI helpers) -- ADR-001's rule that a unit bug has exactly one
//! place to hide -- and nowhere else in this binding converts units or epochs by hand.
//!
//! ## Threading
//!
//! GMAT is a process-wide singleton and is not thread-safe (`crate::engine_lock()`,
//! module-level docs on [`crate::DerivativeModel`]). `GmatModel` wraps a
//! [`crate::DerivativeModel`], which holds a raw pointer into GMAT's configuration, so it is
//! `!Send` and `!Sync` automatically -- the type system already refuses to let a `GmatModel`
//! cross a thread boundary, without this module adding anything. That does **not** make
//! `GmatModel` safe to construct or call concurrently with any other GMAT access on the same
//! thread's call stack from a *different* `GmatModel`/`Object`/`DerivativeModel` bound to the
//! same process: the caller must still hold `crate::engine_lock()` for the lifetime of any
//! sequence of calls that must not interleave with another thread's GMAT access (exactly the
//! existing convention `tests/leo_golden.rs` and `tests/stm_spike.rs` follow).
use std::cell::RefCell;
use std::collections::BTreeMap;

use av_cdm::pb::{ModelCapability, ModelInfo};
use av_cdm::time::Tai;
use av_cdm::units;
use av_dynamics::{decode_signal, AppliedCommand, DynamicsModel, Inbox, Outbox, StepResult, StmStepResult};

use crate::{DerivativeModel, GmatError};

/// A named output [`GmatModel::step`] populates in `StepResult.outputs` (`docs/open-
/// questions.md` question 99, M11.1; question 95's second half, M10.2, first added it):
/// position magnitude from the model's own central body, SI metres -- GMAT's own
/// `<Spacecraft>.<Body>.RMAG` real parameter, read *through the shim* off the model's bound
/// spacecraft after its propagated state has been written back into it, not recomputed in
/// Rust. See [`GmatModel::step`]'s own doc comment for exactly how.
///
/// **Was computed here in Rust through M10.2** (`sqrt(x^2 + y^2 + z^2)` on the propagated
/// state) because `crates/gmat-sys/shim/gmatffi.{h,cpp}` exposed no `GetRealParameter`-style
/// getter and `DerivativeModel` retained no handle to the bound `Spacecraft` object at all
/// (question 95's escalation). Question 99 closed that gap: the shim now exposes
/// `gmatffi_get_real_parameter`/`gmatffi_model_spacecraft`
/// ([`crate::DerivativeModel::real_parameter`]/[`crate::DerivativeModel::
/// sync_spacecraft_cartesian_km`]), so this value is GMAT's own answer, read through GMAT's own
/// parameter subsystem -- verified, not merely asserted, against a genuine GMAT `ReportFile` (a
/// script run, `SolverIterations = Current`, never `Execute()` through the object API -- see
/// this crate's README) for the golden arc this module is pinned against; see
/// `crates/av-kernel/tests/drm_executor.rs::drm_rmag_output_matches_a_genuine_gmat_reportfile`
/// for the comparison and the measured agreement (now sub-micrometre, tighter than the 4 µm
/// M10.2 measured between the Rust-derived value and that same `ReportFile` -- see that test's
/// doc comment for the exact number).
pub const OUTPUT_RMAG: &str = "rmag";

/// A second named output, added alongside `OUTPUT_RMAG` at question 99/M11.1 specifically to
/// prove [`GmatModel::step`]'s real-parameter read is not merely reproducing arithmetic Rust
/// could already do on the propagated state: GMAT's `Cd` (drag coefficient) is a spacecraft
/// property this crate never sees or stores anywhere in Rust -- it is set once, in C++, by
/// whatever built the bound `Spacecraft` (e.g. `av-kernel`'s `binding::materialize_gmat`, from
/// a `SystemDefinition`'s `spacecraft.Cd` parameter), and does not depend on the propagated
/// state at all. Reading it back correctly after a step, through the same
/// [`crate::DerivativeModel::real_parameter`] call `OUTPUT_RMAG` uses, demonstrates the call
/// genuinely reaches into GMAT's own object rather than reflecting a value Rust already had.
pub const OUTPUT_CD: &str = "cd";

/// SIGNAL port participation for a [`GmatModel`] (M18.3, `docs/open-questions.md` question
/// 126): closes the refusal `crate::drm::binding::parse_gmat_spec`'s own `"port.*"` parameters
/// used to hit unconditionally (`av_kernel`'s `drms/README.md` "What is NOT here" section, the
/// escalation this task resolves). Mirrors `av_kernel::drm::binding::ConstantAccelSpec`'s own
/// `emit`/`consume_port` shape, adapted to a GMAT-bound instance's two real capabilities:
///
/// - `emit`: `(port name, output key)` -- every step, after [`GmatModel::step`] runs, whatever
///   value it wrote into `StepResult.outputs[output_key]` is sent as a SIGNAL on `port name`.
///   `output_key` must be one of this model's own named outputs ([`OUTPUT_RMAG`]/[`OUTPUT_CD`]
///   -- the only two keys [`GmatModel::step`]/[`GmatModel::step_with_stm`] ever populate), never
///   an arbitrary string -- validated by `parse_gmat_spec` at load time, not here (this type
///   carries an already-validated pair, exactly like `ConstantAccelSpec::emit`).
/// - `consume`: `(port name, GMAT field name)` -- every step, the *last* message on `port name`
///   in the `Inbox` (question 108's own delivery-order guarantee: last among ties on this port
///   is the most recently emitted) is decoded as a SIGNAL value and written into the bound
///   spacecraft's own named real field through [`crate::DerivativeModel::set_real_parameter`]
///   **before** that step's own integration runs, so the commanded value is in effect for every
///   sub-step [`GmatModel::derivatives`] takes within it -- not merely visible starting next
///   step. `field name` must be one of `av_kernel::drm::binding`'s own declared-writable
///   allowlist (`Cd` today) -- validated by `parse_gmat_spec` at load time, not here.
#[derive(Debug, Clone, Default)]
pub struct GmatPortConfig {
    pub emit: Option<(String, String)>,
    pub consume: Option<(String, String)>,
}

/// Static description fields for a [`GmatModel`], independent of the bound spacecraft/force
/// model objects. `crate::Object` is a configured-but-opaque GMAT handle (ADR-002's shim
/// exposes no introspection over what forces were added to a `ForceModel`), so the caller
/// that built the force model and spacecraft is the only one who can state what it actually
/// configured; `GmatModel` does not attempt to recover it from `DerivativeModel`.
#[derive(Debug, Clone)]
pub struct GmatModelInfo {
    /// Stable id, e.g. `"gmat.earth.jgm2_8x8.sun_moon"` (`ModelInfo.id`).
    pub id: String,
    /// GMAT version this model was bound against, e.g. `"R2026a"`.
    pub version: String,
    /// `ModelInfo.state_space_id`: the registered state space this model's 6-vector is in.
    pub state_space_id: String,
    /// `ModelInfo.frame_id`: the frame the state is expressed in (the `CoordinateSystem` the
    /// spacecraft was configured with, e.g. `"EarthMJ2000Eq"`).
    pub frame_id: String,
    /// Names of the goldens (under `goldens/`) this exact configuration is pinned against.
    pub goldens: Vec<String>,
    /// Whether the bound `ForceModel` includes `RelativisticCorrection` (`docs/open-
    /// questions.md` question 82, `docs/adr/002-dynamics-contract.md`'s third amendment):
    /// `RelativisticCorrection::GetDerivatives` fills its A-matrix/STM contribution with an
    /// unconditional zero -- read directly in `third_party/gmat-src/src/base/forcemodel/
    /// RelativisticCorrection.cpp`, both the `fillSTM` and `fillAMatrix` branches write a
    /// zeroed buffer -- so it is a stub, not a physically-absent term, and a force model that
    /// includes it does not actually have the STM capability `GmatModel` would otherwise
    /// declare from `model.dimension() == 42`. `GmatModel` cannot discover this on its own
    /// (this struct's own doc comment: the shim exposes no introspection over what forces were
    /// added to a `ForceModel`), so the caller that built the force model states it directly,
    /// the same way it already states `goldens`. Default via `Default`/an explicit `false` is
    /// the common case (no relativistic correction in the model); this field has no effect on
    /// `derivatives`/`stm_derivatives` themselves, only on the declared capability
    /// ([`GmatModel::stm_capable`]/[`av_dynamics::DynamicsModel::describe`]).
    pub has_relativistic_correction: bool,
}

/// A [`DynamicsModel`] wrapping a [`DerivativeModel`] -- either the plain 6-state Cartesian
/// model (`Gmat::derivative_model`) or the 42-state Cartesian+STM model
/// (`Gmat::derivative_model_with_stm`, ADR-002 second amendment). Either way,
/// [`DynamicsModel::state_dim`] always reports 6: the STM (when present) is a *declared
/// capability* ([`DynamicsModel::stm_capable`]) reached through
/// [`DynamicsModel::stm_derivatives`], not a change to the model's own physical state space
/// -- a caller that never asks for covariance sees exactly the same 6-state contract either
/// way.
///
/// Wrapping the 42-state model does not change what `derivatives` computes: `GetDerivatives`
/// on the 42-state model was measured bit-identical to the plain 6-state model in its first 6
/// components (ADR-002 amendment, `docs/teamlog/adr-002-amendment-draft-stm.md`), so
/// `GmatModel::derivatives` pads the caller's 6-state input with an identity STM block before
/// calling into GMAT and simply discards GMAT's returned STM-derivative block -- the physical
/// answer is identical to what the plain 6-state model would have returned; see `derivatives`
/// below for exactly how.
pub struct GmatModel {
    model: DerivativeModel,
    /// `model.dimension() == 42` (STM requested) vs `== 6` (plain). Cached rather than
    /// re-derived from `model.dimension()` at every call only for readability at call sites.
    /// This reflects the physical shape of the bound `DerivativeModel` only -- whether the
    /// *declared capability* [`GmatModel::stm_capable`] agrees also depends on
    /// `capability_withheld` below (question 82).
    stm: bool,
    /// `info.has_relativistic_correction && !accept_missing_stm_terms` at construction time
    /// (question 82): when `true`, [`GmatModel::stm_capable`] reports `false` regardless of
    /// `stm`, so a well-behaved caller (which checks `stm_capable()` before ever calling
    /// [`DynamicsModel::stm_derivatives`] or wrapping this model in `av_dynamics::StmAugmented`,
    /// which itself panics on a model whose `stm_capable()` is `false`) never reaches GMAT's
    /// zeroed-out `RelativisticCorrection` STM block believing it means something. Fixed at
    /// construction, not re-checked per call, because the DRM's `accept_missing_stm_terms`
    /// acknowledgement is a property of *this model being built at all*, not of any one request.
    capability_withheld: bool,
    info: GmatModelInfo,
    settings_hash: String,
    /// M18.3: SIGNAL emit/consume wiring, empty (both `None`) unless [`GmatModel::with_ports`]
    /// was called -- see [`GmatPortConfig`]'s own doc comment and [`GmatModel::step_with_ports`].
    /// Deliberately excluded from `settings_hash`: port wiring is fixed for an instance's whole
    /// life (it comes from `SystemDefinition.parameters`, never touched by a fault/maneuver
    /// re-binding), exactly like the `"output.*"` declaration this crate's own `parse_gmat_spec`
    /// already keeps out of `GmatSystemSpec`/`gmat_settings` -- so including it here would add
    /// nothing a re-materialization could ever legitimately change.
    ports: GmatPortConfig,
    /// M20.3 (`docs/open-questions.md` question 137, closing the M19.3 "one event per step"
    /// finding): the last value [`GmatModel::step_with_ports`]'s own `consume` path actually
    /// wrote via `set_real_parameter`, keyed by the commanded field name -- `RefCell` because
    /// `step_with_ports` takes `&self` (mirroring `self.model`'s own interior FFI mutability,
    /// not a new pattern this struct introduces), and a `BTreeMap` per this crate's own
    /// determinism rule (ADR-004) even though [`GmatPortConfig::consume`] names at most one
    /// field today, so this is never more than a single entry in practice.
    ///
    /// **Deliberately scoped to *this* `GmatModel` instance, not anything longer-lived.** A
    /// fault or maneuver boundary re-materializes a brand-new `GmatModel` from the plan's own
    /// spec (`av_kernel::drm::fault::rebind_gmat_spec_at_state`,
    /// `av_kernel::drm::binding::materialize_gmat`), which resets a previously-commanded field
    /// (e.g. `Cd`) back to its spec value -- so a cache that survived re-materialization would
    /// wrongly judge the next command "unchanged" (it matches what was cached from before the
    /// rebind) and suppress its event, even though the live object's value really did revert.
    /// Being a field on this struct, constructed fresh by every `GmatModel::new` call
    /// (`crate::registry::ModelRegistry::construct_gmat`, called once per initial
    /// materialization and once per active instance at every boundary), this cache cannot
    /// survive a re-materialization even by accident: the new `GmatModel` starts with an empty
    /// map, so the first command applied after a rebind is always judged a first application
    /// and always emits -- see [`GmatModel::step_with_ports`]'s own doc comment for where this
    /// is read/written.
    last_applied: RefCell<BTreeMap<String, f64>>,
}

impl GmatModel {
    /// `settings` should describe everything that affects `derivatives`'s output -- central
    /// body, gravity file/degree/order, point masses, integrator tolerances -- so
    /// `describe().settings_hash` is a pure function of what actually determines the physics
    /// rather than merely a label. See [`av_dynamics::settings_hash`] for the hash itself
    /// (`BTreeMap` keeps it independent of the caller's insertion order).
    ///
    /// `model` may come from either `Gmat::derivative_model` (dimension 6, no STM capability)
    /// or `Gmat::derivative_model_with_stm` (dimension 42, `stm_capable() == true`, unless
    /// withheld below).
    ///
    /// `accept_missing_stm_terms` is the DRM's acknowledgement (`DrmOptions
    /// .accept_missing_stm_terms`, `proto/altavista/v1/system.proto`, question 82) that a
    /// covariance request against a force model whose STM omits a real term (today:
    /// `info.has_relativistic_correction`) is acceptable anyway. When `info
    /// .has_relativistic_correction` is `true` and this is `false` (the default a caller should
    /// pass unless it has actually read that acknowledgement off a DRM), [`GmatModel::
    /// stm_capable`] reports `false` even for a 42-state `model` -- the platform declaring the
    /// capability absent, per the ADR-002 third amendment's decision, rather than silently
    /// returning a covariance built from a zeroed A-matrix block. This is a description-time
    /// decision, not a per-request one: the request-time "typed error unless
    /// accept_missing_stm_terms" gmat-service applies (`gmat_service.model.GmatModel
    /// .propagate_covariance`) is the analogous check at that depth's own request boundary,
    /// checking the same underlying fact through its own config rather than through this type.
    ///
    /// # Panics
    ///
    /// If `model.dimension()` is neither 6 nor 42.
    pub fn new(model: DerivativeModel, info: GmatModelInfo, settings: &BTreeMap<String, String>, accept_missing_stm_terms: bool) -> Self {
        let dim = model.dimension();
        assert!(
            dim == 6 || dim == 42,
            "GmatModel wraps a 6-state Cartesian model (Gmat::derivative_model) or a 42-state \
             Cartesian+STM model (Gmat::derivative_model_with_stm); got dimension {dim}"
        );
        let settings_hash = av_dynamics::settings_hash(settings);
        let capability_withheld = info.has_relativistic_correction && !accept_missing_stm_terms;
        Self { model, stm: dim == 42, capability_withheld, info, settings_hash, ports: GmatPortConfig::default(), last_applied: RefCell::new(BTreeMap::new()) }
    }

    /// Attach SIGNAL port wiring (M18.3, question 126) -- a separate builder method rather than
    /// a new [`GmatModel::new`] parameter, so every existing caller (`av-kernel`'s `executor.rs`/
    /// `golden_acceptance.rs`, `av-dynamics-service::worker`, this crate's own `tests/model_stm.rs`)
    /// needed no change: the default [`GmatPortConfig`] (both fields `None`) makes
    /// [`GmatModel::step_with_ports`] behave exactly like the trait's own default (plain `step`,
    /// empty `Outbox`) for every model that never calls this.
    pub fn with_ports(mut self, ports: GmatPortConfig) -> Self {
        self.ports = ports;
        self
    }

    /// The bound model's own epoch (`crate::DerivativeModel::epoch_a1mjd`), as TAI
    /// nanoseconds -- the `t0` every `derivatives`/`step` call's `t_tai_ns` is measured
    /// against, since `DerivativeModel::derivatives_into` itself takes a `dt_seconds` offset
    /// from this same epoch.
    pub fn epoch_tai_ns(&self) -> i64 {
        Tai::from_a1_mjd(self.model.epoch_a1mjd()).as_nanos()
    }

    /// The bound model's state at its own epoch, converted to SI metres / metres-per-second
    /// -- the natural initial state for a caller driving this model through
    /// [`DynamicsModel::step`]. Only the physical (first 6) elements: when `model` came from
    /// `Gmat::derivative_model_with_stm`, `crate::DerivativeModel::state` returns 42 elements
    /// (the trailing 36 being `Phi(t0,t0) = I`, per the ADR-002 second amendment), which this
    /// method deliberately does not expose -- `GmatModel::state_dim()` is always 6.
    pub fn initial_state_si(&self) -> Result<[f64; 6], GmatError> {
        let full = self.model.state()?;
        let km6: [f64; 6] = full[0..6].try_into().expect("DerivativeModel state has at least 6 elements");
        Ok(units::state_km_to_m(km6))
    }
}

impl DynamicsModel for GmatModel {
    type Error = GmatError;

    fn state_dim(&self) -> usize {
        6
    }

    fn derivatives(&self, state: &[f64], t_tai_ns: i64, _controls: &[f64], state_dot: &mut [f64]) -> Result<(), GmatError> {
        assert_eq!(state.len(), 6, "GmatModel is a 6-state model");
        assert_eq!(state_dot.len(), 6, "GmatModel is a 6-state model");

        let state_m: [f64; 6] = state.try_into().unwrap();
        let state_km = units::state_m_to_km(state_m);

        let dt_s = (t_tai_ns - self.epoch_tai_ns()) as f64 * 1e-9;
        let dot_km6 = if self.stm {
            // The bound DerivativeModel is 42-state (STM requested); GetDerivatives on it was
            // measured bit-identical to the plain 6-state model in its first 6 components
            // regardless of what STM block it is fed (ADR-002 amendment), so pad the caller's
            // 6-state input with an identity STM block, call through, and keep only the
            // physical block -- the STM-derivative block GMAT also filled is simply unused
            // here (this is the plain 6-state contract, not `stm_derivatives`).
            let mut padded = vec![0.0; 42];
            padded[0..6].copy_from_slice(&state_km);
            for i in 0..6 {
                padded[6 + i * 6 + i] = 1.0;
            }
            let dot42 = self.model.derivatives(&padded, dt_s)?;
            let dot6: [f64; 6] = dot42[0..6].try_into().expect("first 6 elements of a 42-state derivative");
            dot6
        } else {
            let dot_km = self.model.derivatives(&state_km, dt_s)?;
            dot_km.try_into().expect("6-state DerivativeModel reports a 6-element derivative")
        };

        // `units::state_km_to_m` scales index 0..3 and 3..6 by the same factor
        // (`M_PER_KM = 1000`) regardless of what those indices mean physically -- for a plain
        // state it is "position, velocity"; here it is "velocity, acceleration". Both are
        // still just km -> m on each 3-vector, so reusing the same conversion is exact, not a
        // coincidental reuse of a position/velocity-labelled function on the wrong thing.
        let dot_m = units::state_km_to_m(dot_km6);
        state_dot.copy_from_slice(&dot_m);
        Ok(())
    }

    fn describe(&self) -> ModelInfo {
        let mut capabilities = vec![ModelCapability::Derivatives as i32, ModelCapability::Step as i32, ModelCapability::Deterministic as i32];
        if self.stm_capable() {
            capabilities.push(ModelCapability::Stm as i32);
        }
        ModelInfo {
            id: self.info.id.clone(),
            version: self.info.version.clone(),
            state_space_id: self.info.state_space_id.clone(),
            frame_id: self.info.frame_id.clone(),
            // No control inputs are modeled at this binding yet (no maneuvers/thrust wired
            // through GmatModel::derivatives) -- left empty rather than invented.
            controls: vec![],
            capabilities,
            depth: "gmat-ffi".to_string(),
            settings_hash: self.settings_hash.clone(),
            goldens: self.info.goldens.clone(),
        }
    }

    /// Overrides [`DynamicsModel::step`]'s default (empty-`outputs`) implementation to populate
    /// `StepResult.outputs` with [`OUTPUT_RMAG`] and [`OUTPUT_CD`] (question 99, M11.1) --
    /// otherwise identical to the default: the same [`av_dynamics::integrate::Dopri5`]
    /// integration over [`DynamicsModel::derivatives`], from `t_tai_ns` to `t_tai_ns + dt_ns`.
    /// Duplicated here rather than calling a shared helper so this crate's `av-dynamics`
    /// dependency (a file this task does not own beyond `src/lib.rs`) needed no change.
    ///
    /// After integrating to `x1` (SI metres/metres-per-second), the propagated position and
    /// velocity are written back into the bound spacecraft's own Cartesian fields
    /// ([`crate::DerivativeModel::sync_spacecraft_cartesian_km`], km/km-s -- GMAT's native
    /// units for these fields) -- required because this crate's own integrator never calls
    /// GMAT's `PropagationStateManager::MapVectorToObjects`, so nothing else keeps the bound
    /// `Spacecraft`'s fields in sync with `x1`. The propagated epoch (`t_tai_ns + dt_ns`,
    /// converted to A1MJD) is written back first, through
    /// [`crate::DerivativeModel::sync_spacecraft_epoch_a1mjd`] (question 105: before this, the
    /// spacecraft's epoch was never updated after construction, silently stale for any
    /// epoch-dependent GMAT parameter -- see that method's doc comment for why epoch goes first
    /// and why the order is a no-op for `"RMAG"`/`"Cd"` specifically). `"RMAG"` and `"Cd"` are
    /// then read back through [`crate::DerivativeModel::real_parameter`] -- GMAT's own
    /// real-parameter subsystem, not Rust arithmetic -- and converted: `RMAG` is km, converted
    /// to SI metres (`av_cdm::units::km_to_m`) to match `OUTPUT_RMAG`'s declared unit; `Cd` is
    /// already dimensionless and crosses unconverted.
    fn step(&self, state: &[f64], t_tai_ns: i64, controls: &[f64], dt_ns: i64) -> Result<StepResult, GmatError> {
        let dt_s = dt_ns as f64 * 1e-9;
        let (x1, _stats) = self.integrator().integrate(
            |t_rel_s, x, out| {
                let t_ns = t_tai_ns + (t_rel_s * 1e9).round() as i64;
                self.derivatives(x, t_ns, controls, out)
            },
            state,
            0.0,
            dt_s,
        )?;
        let state_m: [f64; 6] = x1[0..6].try_into().expect("integrated state has 6 elements");
        let state_km = units::state_m_to_km(state_m);
        let t1_tai_ns = t_tai_ns + dt_ns;
        self.model.sync_spacecraft_epoch_a1mjd(Tai::from_nanos(t1_tai_ns).to_a1_mjd())?;
        self.model.sync_spacecraft_cartesian_km(&state_km)?;
        let rmag_km = self.model.real_parameter("RMAG")?;
        let cd = self.model.real_parameter("Cd")?;
        let mut outputs = BTreeMap::new();
        outputs.insert(OUTPUT_RMAG.to_string(), units::km_to_m(rmag_km));
        outputs.insert(OUTPUT_CD.to_string(), cd);
        Ok(StepResult { state: x1, t_tai_ns: t1_tai_ns, outputs })
    }

    /// `true` when `model` (see [`GmatModel::new`]) came from `Gmat::derivative_model_with_stm`
    /// (dimension 42) **and** the capability was not withheld at construction (question 82:
    /// `info.has_relativistic_correction && !accept_missing_stm_terms`) -- the declared
    /// capability [`av_dynamics::DynamicsModel::stm_capable`] asks for. A 42-state model built
    /// from a force model that includes `RelativisticCorrection`, without the DRM's
    /// `accept_missing_stm_terms` acknowledgement, reports `false` here even though `model
    /// .dimension() == 42` -- GMAT would still fill the STM block (with `RelativisticCorrection`
    /// contributing an unconditional zero, not an error), but the platform declares the
    /// capability absent rather than let a caller build a covariance from it unknowingly.
    fn stm_capable(&self) -> bool {
        self.stm && !self.capability_withheld
    }

    /// `state_dot` for the STM-augmented state (ADR-002 second amendment): index `0..6` is the
    /// physical state, index `6 + row*6 + col` is `d(Phi)/dt` element `(row, col)` (row-major,
    /// matching GMAT's own `Spacecraft::GetRmatrixParameter("STM")` layout, verified in the STM
    /// spike). Units: the physical block is SI in and out, exactly like `derivatives`; the STM
    /// block needs **no unit conversion** at all -- position and velocity both scale km<->m by
    /// the identical factor 1000, so for a state perturbation `dx_SI = 1000 * dx_km` (uniformly
    /// across all 6 components), `Phi` in the two bases are numerically identical (`Phi_SI =
    /// (1000 I) Phi_km (1000 I)^-1 = Phi_km` since the scaling is a scalar multiple of the
    /// identity), and the same cancellation applies to `A(t) = d(state_dot)/d(state)` and
    /// therefore to `d(Phi)/dt = A Phi`. This is a mathematical fact about the specific,
    /// *uniform* km<->m rescaling used everywhere in this crate, not an assumption -- see the
    /// crate README for the derivation.
    ///
    /// # Panics
    ///
    /// If `self.stm` is `false` (i.e. `model` came from `Gmat::derivative_model`, not
    /// `derivative_model_with_stm`) -- see [`av_dynamics::DynamicsModel::stm_derivatives`]'s
    /// panic note: callers must check `stm_capable()` first.
    fn stm_derivatives(&self, augmented_state: &[f64], t_tai_ns: i64, _controls: &[f64], augmented_state_dot: &mut [f64]) -> Result<(), GmatError> {
        assert!(self.stm, "GmatModel::stm_derivatives called but stm_capable() is false (model came from Gmat::derivative_model, not derivative_model_with_stm)");
        assert_eq!(augmented_state.len(), 42, "GmatModel's STM-augmented state is 42 elements (6 + 6x6)");
        assert_eq!(augmented_state_dot.len(), 42);

        let state_m: [f64; 6] = augmented_state[0..6].try_into().unwrap();
        let state_km = units::state_m_to_km(state_m);
        let mut padded_km = vec![0.0; 42];
        padded_km[0..6].copy_from_slice(&state_km);
        // The STM block is unit-invariant (see the doc comment above) -- pass it through as-is.
        padded_km[6..42].copy_from_slice(&augmented_state[6..42]);

        let dt_s = (t_tai_ns - self.epoch_tai_ns()) as f64 * 1e-9;
        let dot42_km = self.model.derivatives(&padded_km, dt_s)?;

        let dot_km6: [f64; 6] = dot42_km[0..6].try_into().expect("first 6 elements of a 42-state derivative");
        let dot_m6 = units::state_km_to_m(dot_km6);
        augmented_state_dot[0..6].copy_from_slice(&dot_m6);
        // d(Phi)/dt is likewise unit-invariant -- copied through unchanged.
        augmented_state_dot[6..42].copy_from_slice(&dot42_km[6..42]);
        Ok(())
    }

    /// Overrides [`DynamicsModel::step_with_stm`]'s default to populate [`StmStepResult
    /// .outputs`] with [`OUTPUT_RMAG`] and [`OUTPUT_CD`] (question 101, M11.2) -- the
    /// `step_with_stm` sibling of [`GmatModel::step`]'s own override (question 99, M11.1),
    /// needed so a covariance run (which steps through `av_dynamics::StmAugmented::step`, which
    /// as of M11.2 delegates to *this* method for exactly this reason) reports the same named
    /// output set a plain run does -- `docs/open-questions.md` question 101's "product sets must
    /// not depend on the run mode." Otherwise identical to the trait's default `step_with_stm`:
    /// the same augmented `[state; vec(Phi)]` integration via [`DynamicsModel::stm_derivatives`],
    /// seeding `Phi(t0,t0) = I`.
    ///
    /// After integrating to `aug1` (SI metres/metres-per-second in the physical block), the
    /// propagated epoch (`t_tai_ns + dt_ns`, question 105) is written back first through
    /// [`crate::DerivativeModel::sync_spacecraft_epoch_a1mjd`], then the propagated position and
    /// velocity -- `aug1`'s own first six elements, from *this same* integration, never a value
    /// read from anywhere else -- are written back into the bound spacecraft's Cartesian fields
    /// ([`crate::DerivativeModel::sync_spacecraft_cartesian_km`]), exactly like
    /// [`GmatModel::step`] does, and `"RMAG"`/`"Cd"` are then read back through
    /// [`crate::DerivativeModel::real_parameter`]. **Measured, not assumed, that this write-back
    /// does not interact with the already-completed STM integration**: the sync happens strictly
    /// *after* `self.integrator().integrate` returns (the STM block is fully computed by then --
    /// nothing above reads the spacecraft's Cartesian fields or epoch mid-integration, so writing
    /// to them afterwards cannot perturb a computation that already finished), confirmed by
    /// `crates/av-kernel/tests/golden_acceptance.rs::
    /// kernel_covariance_matches_the_golden_stm_and_propagated_cov` still matching the
    /// GMAT-generated STM/covariance golden at its existing tolerance with the epoch write-back
    /// wired into the covariance path (`av_dynamics::StmAugmented::step`) -- see this task's own
    /// report for the measured numbers.
    fn step_with_stm(&self, state: &[f64], t_tai_ns: i64, controls: &[f64], dt_ns: i64) -> Result<StmStepResult, GmatError> {
        let n = self.state_dim();
        let mut aug0 = vec![0.0; n + n * n];
        aug0[0..n].copy_from_slice(state);
        for i in 0..n {
            aug0[n + i * n + i] = 1.0;
        }
        let dt_s = dt_ns as f64 * 1e-9;
        let (aug1, _stats) = self.integrator().integrate(
            |t_rel_s, x, out| {
                let t_ns = t_tai_ns + (t_rel_s * 1e9).round() as i64;
                self.stm_derivatives(x, t_ns, controls, out)
            },
            &aug0,
            0.0,
            dt_s,
        )?;
        let (state1, phi) = aug1.split_at(n);
        let state1_arr: [f64; 6] = state1.try_into().expect("6-state physical block of the STM-augmented state");
        let state_km = units::state_m_to_km(state1_arr);
        let t1_tai_ns = t_tai_ns + dt_ns;
        self.model.sync_spacecraft_epoch_a1mjd(Tai::from_nanos(t1_tai_ns).to_a1_mjd())?;
        self.model.sync_spacecraft_cartesian_km(&state_km)?;
        let rmag_km = self.model.real_parameter("RMAG")?;
        let cd = self.model.real_parameter("Cd")?;
        let mut outputs = BTreeMap::new();
        outputs.insert(OUTPUT_RMAG.to_string(), units::km_to_m(rmag_km));
        outputs.insert(OUTPUT_CD.to_string(), cd);
        Ok(StmStepResult { state: state1.to_vec(), phi: phi.to_vec(), t_tai_ns: t1_tai_ns, outputs })
    }

    /// Overrides [`DynamicsModel::step_with_ports`]'s default (which ignores `inbox` and always
    /// returns an empty `Outbox`) -- M18.3, `docs/open-questions.md` question 126, closing the
    /// gap `drms/README.md`'s "What is NOT here" section named: through this task, a GMAT-bound
    /// instance could be neither an honest SIGNAL sender nor receiver.
    ///
    /// **Consume, then step, then emit** -- in that order:
    /// 1. If [`GmatPortConfig::consume`] is set, the *last* message on that port in `inbox`
    ///    (question 108's delivery order: last among ties on one port is the most recently
    ///    emitted -- [`Inbox::last_on_port`] is the one place that selection lives) is decoded
    ///    ([`av_dynamics::decode_signal`]) and written into the bound spacecraft's named field
    ///    via [`crate::DerivativeModel::set_real_parameter`] **before** [`GmatModel::step`]
    ///    runs -- so the commanded value is what every `GetDerivatives` sub-step within *this*
    ///    `step_with_ports` call sees, not merely what a later step would see. A message that
    ///    fails to decode, or no message on that port at all, leaves the field untouched (the
    ///    same "no message, no effect" contract `ConstantAccelModel::step_with_ports` already
    ///    uses for its own `consume_port`) -- and reports nothing in the returned
    ///    `Vec<AppliedCommand>` either: "applied" is the bar (`docs/open-questions.md` question
    ///    130), not "a message merely arrived."
    ///
    ///    **M20.3 (question 137) narrows "applied" further: changed, or first.** `set_real_
    ///    parameter` above is still called on every decoded message, every step, unconditionally
    ///    -- this is required physics, not a reporting decision, so it never changes here. What
    ///    changed is whether that write is *reported* in `Vec<AppliedCommand>`: only when
    ///    `value` differs, by exact bit equality (never a tolerance -- a command stream is a
    ///    deterministic product of the run, so "unchanged" means bit-identical), from the value
    ///    `self.last_applied` has recorded for `field_name` so far, or when `field_name` has
    ///    never been recorded at all (the first application always emits). This is what turns a
    ///    10 Hz stream commanding the same value for two hours into a handful of events instead
    ///    of 71,999 (M19.3's own measurement) while a genuinely rate-commanded stream, whose
    ///    value changes every step, stays fully representable -- every change is still recorded.
    ///    See this struct's own `last_applied` field doc comment for why this cache is scoped to
    ///    *this* `GmatModel` instance and therefore cannot survive a fault/maneuver
    ///    re-materialization (a fresh instance starts with an empty cache, so the first command
    ///    after a rebind is always judged a first application, even if its value happens to
    ///    equal what was cached before the rebind -- the live object's own value really did
    ///    revert to the rebuilt spec at that boundary, so treating it as "unchanged" would claim
    ///    an event log entry for a value that was never actually reapplied).
    ///
    ///    **Question 130 (M19.3): this write bypasses `settings_hash`.** `self.model
    ///    .set_real_parameter` reconfigures the live GMAT object directly -- it is not a
    ///    `GmatSystemSpec` field `av_kernel::drm::binding::gmat_settings` ever hashes, so two
    ///    runs with an identical `dynamics_hash` can now propagate different physics if their
    ///    command streams differ (M18.3 measured 891.8 m of arc divergence from exactly this on
    ///    a drag-inclusive fixture, `crates/gmat-sys/tests/gmat_port_cd_command.rs`). The fix is
    ///    not to fold the commanded value into the hash (the lead's decision explicitly rejects
    ///    that -- `dynamics_hash` stays the configuration hash, and no segment opens per
    ///    command): it is to report every value this method actually writes, in the returned
    ///    `Vec<AppliedCommand>`, so `av_kernel::schedule::HeteroScheduler`/`crate::drm::executor`
    ///    can record it as a real `EVENT_KIND_PORT_COMMAND` CDM event instead -- "equal
    ///    `dynamics_hash` means equal configuration; equal configuration plus equal command
    ///    events means equal dynamics."
    /// 2. [`GmatModel::step`] runs unchanged -- identical physics, identical `OUTPUT_RMAG`/
    ///    `OUTPUT_CD` readback, whether or not a message was just applied.
    /// 3. If [`GmatPortConfig::emit`] is set, `result.outputs[output_key]` (guaranteed present:
    ///    `output_key` is always [`OUTPUT_RMAG`] or [`OUTPUT_CD`], both of which [`GmatModel::
    ///    step`] always populates) is sent as a SIGNAL on the declared port, timestamped at this
    ///    step's own result epoch -- mirroring `ConstantAccelModel::step_with_ports`'s own
    ///    `Outbox::push_signal` call.
    fn step_with_ports(&self, state: &[f64], t_tai_ns: i64, controls: &[f64], dt_ns: i64, inbox: &Inbox) -> Result<(StepResult, Outbox, Vec<AppliedCommand>), GmatError> {
        let mut applied = Vec::new();
        if let Some((port_name, field_name)) = &self.ports.consume {
            if let Some((msg, _sender)) = inbox.last_on_port(port_name) {
                if let Some(value) = decode_signal(&msg.payload) {
                    // The write always happens, unconditionally -- this is the physics, and
                    // M20.3 (question 137) never touches it. Only whether it is *reported*
                    // below depends on `last_applied` -- see this method's own doc comment.
                    self.model.set_real_parameter(field_name, value)?;
                    let mut last_applied = self.last_applied.borrow_mut();
                    let changed_or_first = last_applied.get(field_name) != Some(&value);
                    if changed_or_first {
                        last_applied.insert(field_name.clone(), value);
                        applied.push(AppliedCommand { port: port_name.clone(), field: field_name.clone(), value, applied_tai_ns: t_tai_ns });
                    }
                }
            }
        }
        let result = self.step(state, t_tai_ns, controls, dt_ns)?;
        let mut outbox = Outbox::new();
        if let Some((port_name, output_key)) = &self.ports.emit {
            if let Some(value) = result.outputs.get(output_key) {
                outbox.push_signal(port_name.clone(), result.t_tai_ns, *value);
            }
        }
        Ok((result, outbox, applied))
    }

    /// Question 173 (M25.3) never wired a GMAT-backed model into telemetry-to-`Measurement`
    /// mapping -- only the two native sensor models (`StarTrackerModel`/`ImuModel`,
    /// `av_kernel::drm::sensors`) declare a `PacketCodec` with a non-empty `PacketField.target`.
    /// A `GmatModel` instance never produces a CDM measurement.
    fn last_measurements(&self) -> Vec<av_cdm::pb::Measurement> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_tai_ns_and_describe_do_not_need_gmat_state_beyond_construction() {
        // Compile-time / type check only: GmatModelInfo and the settings-hash plumbing are
        // exercised without touching GMAT at all (no engine_lock, no Gmat::setup), matching
        // this crate's convention that only tests which actually call into GMAT take the
        // lock. A GmatModel itself cannot be constructed without a real DerivativeModel, so
        // the GMAT-touching behavior (derivatives, describe on a real model) is covered by
        // av-kernel's golden acceptance test instead.
        let info = GmatModelInfo {
            id: "gmat.test".to_string(),
            version: "R2026a".to_string(),
            state_space_id: "test.space".to_string(),
            frame_id: "EarthMJ2000Eq".to_string(),
            goldens: vec!["leo_1day_jgm2_8x8_sunmoon".to_string()],
            has_relativistic_correction: false,
        };
        let mut settings = BTreeMap::new();
        settings.insert("central_body".to_string(), "Earth".to_string());
        let hash = av_dynamics::settings_hash(&settings);
        assert_eq!(hash.len(), 64);
        // info itself round-trips through Debug/Clone without needing GMAT.
        let cloned = info.clone();
        assert_eq!(format!("{cloned:?}"), format!("{info:?}"));
    }
}
