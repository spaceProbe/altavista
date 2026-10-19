//! Study mode: the parent. Never initializes GMAT -- every sample runs in its own child process
//! (`main.rs`'s own module doc comment: GMAT is a process-global singleton, so exactly one DRM
//! run happens per process).
//!
//! ## Two kinds of refusal, deliberately different in scope
//!
//! [`preflight`] checks everything that would fail *identically for every single sample*
//! (a tampered sweep/DRM/SOS/system, `sweep.drm_id` not matching the DRM, `monte_carlo_draws ==
//! 0`, `monte_carlo_draws > 1` with no declared `Scenario.seeds`, and -- question 192(d) --
//! `sweep.dispersed` with no declared `Scenario.seeds`) -- these are refused ONCE, fatally,
//! before anything is written, exactly the way `av_sweep::sample::sample_config` itself would
//! refuse them, but without silently producing a "study" of zero samples (a `draws == 0` sweep
//! would otherwise iterate `0..0` and write an empty, successful-looking result) or of N
//! byte-identical failures (every other structural mismatch).
//!
//! Everything [`crate::sample::sample_config`] can still refuse per grid point --
//! `UnknownInstance`/`UnknownSystem`/`UndeclaredParameter`/`StringValuedParameter`/
//! `ParameterOutOfBounds` -- is genuinely per-sample (an axis value can be in bounds at one grid
//! point and out of bounds at another), so those are recorded as a failed [`pb::SweepSample`]
//! and the study continues; no child is ever spawned for a sample that never got a valid
//! configuration to write.
//!
//! ## F2 additions: aggregates and the optional study store
//!
//! Once every sample has been recorded, [`run_study`] computes per-`(point, score)` aggregates
//! (`av_sweep::aggregate::aggregate`, `docs/feasibility-plan.md`'s F2 milestone) and fills
//! `SweepResults.aggregates` -- F1b always left this empty. A `SweepError::MixedPassCriterion`
//! from that call is treated as fatal to the study (like every other structural, sweep-wide
//! refusal in [`preflight`]): it means the same score name was declared an Objective in one draw
//! and a MeasureOfEffectiveness in another, which cannot happen from a single, unchanging DRM.
//!
//! `--store-dir <dir>` is a new, additional, OPTIONAL destination (F2's own brief: "Do NOT change
//! the existing `--out-dir` layout"). When given, the finished `SweepResults` is also written
//! through `av_sweep::store::FileStudyStore` rooted at that directory
//! (`<store-dir>/<sweep_id>/{sweep_results.pb,sweep_results.json,samples.jsonl}`) -- this is the
//! platform's own read path for a study (`docs/feasibility-plan.md`: "nothing reads the store
//! except through that trait"), distinct from `--out-dir`, which remains the per-sample
//! reproduction workspace (`drm.pb`/`sos.pb`/`sys_*.pb`/`stderr.txt`/`run_products.pb` per
//! sample) that F1b's own tests and this task's per-sample replay both depend on unchanged. When
//! `--store-dir` is omitted, no store directory is created or written at all.
//!
//! Question 245 adds a second additional, optional destination, selected by
//! `--store-clickhouse <url> --clickhouse-database <db> --clickhouse-user <user>
//! [--clickhouse-password-env <VAR>] [--clickhouse-ca-file <pem>]`: the same finished
//! `SweepResults` is also written through `av_sweep::clickhouse::ClickHouseStudyStore`. Both
//! stores may be given; each gets the same message. The ClickHouse store is opened (connected and
//! its schema verified, or migrated when it has no tables yet) **before the first sample runs**,
//! so a wrong URL, password variable, CA or schema fails in seconds, not after the study.
//!
//! ## Question 192 additions: `seeds`, event axes and `dispersed`
//!
//! `SweepSample.seed` (one projected value) no longer exists -- [`Running`] now carries the
//! sample's full `BTreeMap<String, u64>` (`SampleConfig::seeds`), and both success
//! ([`finalize`]) and failure ([`failed_sample`]) write the whole map into
//! `SweepSample.seeds`. The old single-value projection (`projected_seed`, F1b) is gone
//! entirely, not merely unused: `run.proto`'s own `seed` field is `reserved` now (question
//! 192(b)), so keeping a helper that used to feed it would be documentation for a shape that no
//! longer exists.
//!
//! **`error_mode` selection** (question 192(d)): `ExecutionErrorMode::Sampled` when
//! `sweep.dispersed || sweep.monte_carlo_draws > 1`, `Nominal` otherwise -- widened from F1b's
//! "`Sampled` iff `monte_carlo_draws > 1`" rule, which could not express a dispersed *single*
//! draw per point. [`preflight`] mirrors `av_sweep::sample::sample_config`'s own
//! `DispersedWithoutSeeds` refusal (`sweep.dispersed` with empty `Scenario.seeds`) the same way
//! it already mirrors `DrawsAboveOneWithoutSeeds`.
//!
//! ## Question 245 (heavy-ct-2b): `--executor`, and one `finalize` for both
//!
//! `--executor process` (the default) is the loop above, unchanged. `container` and `k8s`
//! ([`Dispatch::Jobs`]) hand the queue to `jobs::run_jobs`, which runs each sample as one `av-jobs`
//! job and writes the job's output as `run_products.pb` and its log as `stderr.txt` in the sample
//! directory. [`finalize`] is then the single place a sample becomes its `SweepSample`, for a
//! child's exit ([`Ended::Exit`]) and for a job's result ([`Ended::Job`]) alike, which is what
//! makes the study record the same shape whatever ran the samples. The executor is opened (image
//! present under its digest, cluster preconditions met) before the first sample runs, like the
//! ClickHouse store; a failed job is a failed sample, never a retry and never fatal.

use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::Duration;

use av_cdm::pb;
use av_kernel::drm::ExecutionErrorMode;
use av_sweep::store::{open_store, FileStudyStore, StoreBackend, StudyStore};
use av_sweep::{json, SampleConfig};
use prost::Message;

use crate::cli::{Dispatch, StudyArgs};
use crate::jobs;

/// The maximum number of stderr bytes a failed sample's own [`pb::SweepSample::error`] message
/// quotes -- this task's own brief: "cap the stderr at 4096 bytes and say explicitly in the
/// message when it was truncated".
const MAX_STDERR_BYTES: usize = 4096;

fn read_to_string(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))
}

fn load_systems(paths: &[PathBuf]) -> Result<BTreeMap<String, pb::SystemDefinition>, String> {
    let mut systems = BTreeMap::new();
    for path in paths {
        let sys = av_kernel::drm::schema::parse_system_definition_yaml(&read_to_string(path)?).map_err(|e| format!("{}: {e}", path.display()))?;
        // The id becomes `sys_<id>.pb` in every sample directory (`write_sample_inputs`): refused
        // here, once, before anything is written.
        av_cdm::path_id::validate_path_component_id(&sys.id).map_err(|e| format!("{}: SystemDefinition.id {:?} cannot name a sample file (sys_<id>.pb): {e}", path.display(), sys.id))?;
        if let Some(_prev) = systems.insert(sys.id.clone(), sys) {
            return Err(format!("two --system files declare the same SystemDefinition.id (last one: {})", path.display()));
        }
    }
    Ok(systems)
}

/// Everything this study needs before any output is written -- see the module doc comment's
/// "Two kinds of refusal" section for exactly which failures belong here (structural, sweep-wide)
/// versus in the per-sample loop (per grid point).
struct Loaded {
    sweep: pb::ParameterSweep,
    sweep_hash: String,
    drm: pb::DesignReferenceMission,
    sos: pb::SosConfiguration,
    systems: BTreeMap<String, pb::SystemDefinition>,
}

fn preflight(args: &StudyArgs) -> Result<Loaded, String> {
    let sweep = av_sweep::parse_sweep_yaml(&read_to_string(&args.sweep)?).map_err(|e| format!("{}: {e}", args.sweep.display()))?;
    let sweep_hash = av_sweep::verify_sweep_hash(&sweep).map_err(|e| format!("{}: {e}", args.sweep.display()))?;
    let drm = av_kernel::drm::schema::parse_drm_yaml(&read_to_string(&args.drm)?).map_err(|e| format!("{}: {e}", args.drm.display()))?;
    let sos = av_kernel::drm::schema::parse_sos_yaml(&read_to_string(&args.sos)?).map_err(|e| format!("{}: {e}", args.sos.display()))?;
    let systems = load_systems(&args.systems)?;

    // Structural, sweep-wide checks -- see the module doc comment. Each mirrors a check
    // `crate::sample::sample_config` itself makes (same message text, via the same
    // av_kernel::drm::hash / av_sweep::SweepError types), just run once, up front.
    av_kernel::drm::hash::verify_drm_hash(&drm).map_err(|e| format!("{}: {e}", args.drm.display()))?;
    av_kernel::drm::hash::verify_sos_hash(&sos).map_err(|e| format!("{}: {e}", args.sos.display()))?;
    for (id, sys) in &systems {
        av_kernel::drm::hash::verify_system_hash(id, sys).map_err(|e| format!("system {id:?}: {e}"))?;
    }
    if sweep.drm_id != drm.id {
        return Err(format!("ParameterSweep.drm_id {:?} does not match DesignReferenceMission.id {:?}", sweep.drm_id, drm.id));
    }
    if sweep.monte_carlo_draws == 0 {
        return Err("ParameterSweep.monte_carlo_draws is 0; a study must declare at least one draw".to_string());
    }
    let seeds_empty = drm.scenario.as_ref().map(|s| s.seeds.is_empty()).unwrap_or(true);
    if sweep.monte_carlo_draws > 1 && seeds_empty {
        return Err(format!("ParameterSweep.monte_carlo_draws={} but Scenario.seeds is empty; every draw would be identical", sweep.monte_carlo_draws));
    }
    // Question 192(d): the mirror of av_sweep::sample::sample_config's own DispersedWithoutSeeds
    // check (same message, via that same SweepError type's Display, not a hand-duplicated
    // string) -- a dispersed draw with nothing to sample from is refused once, up front, the same
    // way the monte_carlo_draws check just above is.
    if sweep.dispersed && seeds_empty {
        return Err(format!("{}: {}", args.drm.display(), av_sweep::SweepError::DispersedWithoutSeeds));
    }

    Ok(Loaded { sweep, sweep_hash, drm, sos, systems })
}

/// The `.pb` files [`write_sample_inputs`] wrote for one sample -- exactly the bytes
/// `av_sweep::sample_config_hash` covers (this task's own brief; proven by
/// `tests::written_sample_files_recompute_the_same_config_hash`).
pub struct SampleInputPaths {
    pub drm_pb: PathBuf,
    pub sos_pb: PathBuf,
    pub system_pbs: Vec<PathBuf>,
}

/// Writes one sample's per-sample DRM/SOS/`SystemDefinition`s under `sample_dir` (created if
/// needed), in the exact on-disk layout this task's brief specifies. Pure I/O, no GMAT --
/// factored out of the worker-pool loop specifically so it is unit-testable on its own (this
/// crate's own `tests::written_sample_files_recompute_the_same_config_hash`).
pub fn write_sample_inputs(sample_dir: &Path, cfg: &SampleConfig) -> io::Result<SampleInputPaths> {
    // `sys_<id>.pb` names a file from a `SystemDefinition.id` (untrusted YAML): every id must be a
    // path component (`av_cdm::path_id`) before the directory or any file is created. `load_systems`
    // refuses the same ids earlier; this keeps the function safe for any other caller.
    for id in cfg.systems.keys() {
        av_cdm::path_id::validate_path_component_id(id).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, format!("SystemDefinition.id {id:?} cannot name a sample file (sys_<id>.pb): {e}")))?;
    }
    std::fs::create_dir_all(sample_dir)?;
    let drm_pb = sample_dir.join("drm.pb");
    std::fs::write(&drm_pb, cfg.drm.encode_to_vec())?;
    let sos_pb = sample_dir.join("sos.pb");
    std::fs::write(&sos_pb, cfg.sos.encode_to_vec())?;
    let mut system_pbs = Vec::new();
    // cfg.systems is a BTreeMap -- already id-sorted, matching av_sweep::sample_config_hash's
    // own iteration order.
    for (id, sys) in &cfg.systems {
        let p = sample_dir.join(format!("sys_{id}.pb"));
        std::fs::write(&p, sys.encode_to_vec())?;
        system_pbs.push(p);
    }
    Ok(SampleInputPaths { drm_pb, sos_pb, system_pbs })
}

fn describe_exit(status: &ExitStatus) -> String {
    format!("{status}")
}

/// The child's captured stderr (already-flushed-to-disk file, see [`spawn_sample`]), capped at
/// [`MAX_STDERR_BYTES`] and explicitly marked when truncated -- this task's own brief: "cap the
/// stderr at 4096 bytes and say explicitly in the message when it was truncated".
fn read_capped_stderr(path: &Path) -> String {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => return format!("<could not read stderr file {}: {e}>", path.display()),
    };
    if bytes.is_empty() {
        return "<empty>".to_string();
    }
    if bytes.len() <= MAX_STDERR_BYTES {
        String::from_utf8_lossy(&bytes).into_owned()
    } else {
        format!("{} [truncated to {MAX_STDERR_BYTES} bytes; {} byte(s) total]", String::from_utf8_lossy(&bytes[..MAX_STDERR_BYTES]), bytes.len())
    }
}

/// Everything [`finalize`] needs to know about one sample once whatever ran it has ended: the
/// identity the `SweepSample` record carries and where its files are. Shared by both ways of
/// running a sample (a child process, a job) -- question 245 factored this out of what used to be
/// the child-process-only `Running`, so the record has one shape whatever ran the sample.
pub struct SampleRun {
    pub point_index: u32,
    pub draw_index: u32,
    pub run_id: String,
    pub config_hash: String,
    pub axis_values: BTreeMap<String, f64>,
    pub seeds: BTreeMap<String, u64>,
    pub sample_dir: PathBuf,
    pub stderr_path: PathBuf,
    pub out_path: PathBuf,
}

impl SampleRun {
    pub fn new(cfg: &SampleConfig, run_id: &str, sample_dir: &Path) -> SampleRun {
        SampleRun {
            point_index: cfg.point_index,
            draw_index: cfg.draw_index,
            run_id: run_id.to_string(),
            config_hash: cfg.config_hash.clone(),
            axis_values: cfg.axis_values.clone(),
            seeds: cfg.seeds.clone(),
            sample_dir: sample_dir.to_path_buf(),
            stderr_path: sample_dir.join("stderr.txt"),
            out_path: sample_dir.join("run_products.pb"),
        }
    }
}

/// One sample currently running as a child process.
struct Running {
    run: SampleRun,
    child: Child,
}

/// How a sample ended: its child process exited, or its job finished (`Err`: the typed failure).
pub enum Ended {
    Exit(ExitStatus),
    Job(Result<(), pb::JobFailure>),
}

/// Spawns one sample's child (`exe --run-sample ...`), stderr redirected straight to
/// `<sample_dir>/stderr.txt` (a real file, not a pipe read by this process) -- this task's own
/// brief allows no new dependency and no threads for the worker pool; redirecting the child's
/// own stderr fd to a file sidesteps the classic "unread pipe fills up and the child blocks"
/// deadlock without needing a reader thread at all, and satisfies "stderr.txt ... always
/// written" for free (the file exists, even empty, from the moment the child is spawned).
fn spawn_sample(exe: &Path, inputs: &SampleInputPaths, cfg: &SampleConfig, run_id: &str, error_mode: ExecutionErrorMode, gmat_startup: Option<&str>, sample_dir: &Path) -> Result<Running, String> {
    let run = SampleRun::new(cfg, run_id, sample_dir);
    let (out_path, stderr_path) = (&run.out_path, &run.stderr_path);
    let stderr_file = std::fs::File::create(stderr_path).map_err(|e| format!("creating {}: {e}", stderr_path.display()))?;

    let mut cmd = Command::new(exe);
    cmd.arg("--run-sample").arg("--drm-pb").arg(&inputs.drm_pb).arg("--sos-pb").arg(&inputs.sos_pb);
    for sp in &inputs.system_pbs {
        cmd.arg("--system-pb").arg(sp);
    }
    cmd.arg("--run-id").arg(run_id).arg("--error-mode").arg(if error_mode == ExecutionErrorMode::Sampled { "sampled" } else { "nominal" }).arg("--out").arg(out_path);
    if let Some(s) = gmat_startup {
        cmd.arg("--gmat-startup").arg(s);
    }
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::from(stderr_file));

    let child = cmd.spawn().map_err(|e| format!("spawning sample child for run_id {run_id:?}: {e}"))?;
    Ok(Running { run, child })
}

fn failed_sample(r: &SampleRun, error: String) -> pb::SweepSample {
    pb::SweepSample {
        point_index: r.point_index,
        draw_index: r.draw_index,
        axis_values: r.axis_values.clone(),
        seeds: r.seeds.clone(),
        run_id: r.run_id.clone(),
        config_hash: r.config_hash.clone(),
        scores: BTreeMap::new(),
        products_uri: String::new(),
        error,
    }
}

/// Turns one finished child into its [`pb::SweepSample`] record -- success (real decodable
/// `RunProducts` at `r.out_path`) or failure (non-zero exit, missing output, or output that does
/// not decode), per this task's own brief. Never panics, never propagates a fatal `Err`: a
/// failed sample is a value, not a control-flow error, which is exactly what makes "a failed
/// sample never aborts the study" true by construction rather than by a caller remembering to
/// catch something.
///
/// Question 245: the one place a sample becomes its record, for a child process ([`Ended::Exit`])
/// and for a job ([`Ended::Job`], the output already written to `r.out_path` and the log to
/// `r.stderr_path` by `jobs::run_one`) alike -- the study record has one shape whatever ran the
/// sample. A child's messages are unchanged byte for byte; a job's say "job" and carry the typed
/// failure (kind, exit code, the executor's detail).
pub fn finalize(r: SampleRun, ended: Ended) -> pb::SweepSample {
    let (who, ok) = match &ended {
        Ended::Exit(status) => ("child exited 0", status.success()),
        Ended::Job(result) => ("job succeeded", result.is_ok()),
    };
    if ok {
        match std::fs::read(&r.out_path) {
            Ok(bytes) => match pb::RunProducts::decode(bytes.as_slice()) {
                Ok(products) => {
                    let products_uri = std::fs::canonicalize(&r.sample_dir).map(|p| p.display().to_string()).unwrap_or_else(|_| r.sample_dir.display().to_string());
                    return pb::SweepSample {
                        point_index: r.point_index,
                        draw_index: r.draw_index,
                        axis_values: r.axis_values,
                        seeds: r.seeds,
                        run_id: r.run_id,
                        config_hash: r.config_hash,
                        scores: products.scores,
                        products_uri,
                        error: String::new(),
                    };
                }
                Err(e) => {
                    let msg = format!("sample {who} but {} did not decode as altavista.v1.RunProducts: {e}; stderr: {}", r.out_path.display(), read_capped_stderr(&r.stderr_path));
                    return failed_sample(&r, msg);
                }
            },
            Err(e) => {
                let msg = format!("sample {who} but its output at {} could not be read: {e}; stderr: {}", r.out_path.display(), read_capped_stderr(&r.stderr_path));
                return failed_sample(&r, msg);
            }
        }
    }
    let msg = match &ended {
        Ended::Exit(status) => format!("sample child {}; stderr: {}", describe_exit(status), read_capped_stderr(&r.stderr_path)),
        Ended::Job(Err(f)) => format!("{}; stderr: {}", jobs::describe_failure(f), read_capped_stderr(&r.stderr_path)),
        // Not reachable (a succeeded job takes the branch above); a message, not a panic.
        Ended::Job(Ok(())) => "sample job succeeded but was not finalized as a success".to_string(),
    };
    failed_sample(&r, msg)
}

/// Builds every (point, draw) sample's configuration, partitioned into immediate failures
/// (recorded as a [`pb::SweepSample`] with `error` set, nothing else touched) and a queue of
/// runnable [`SampleConfig`]s -- see the module doc comment's "Two kinds of refusal" section for
/// exactly which failures land in which bucket. Pure, no GMAT, no child process: factored out of
/// [`run_study`] specifically so the "a per-point failure never aborts the rest of the study" bit
/// is unit-testable on its own (`tests::a_per_point_failure_is_recorded_and_the_rest_of_the_batch_still_builds`),
/// without needing a real child process or GMAT to prove it.
fn build_samples(loaded: &Loaded, points: &[av_sweep::GridPoint], draws: u32) -> (Vec<pb::SweepSample>, VecDeque<SampleConfig>) {
    let mut results = Vec::new();
    let mut queue = VecDeque::new();
    for point in points {
        for draw in 0..draws {
            match av_sweep::sample_config(&loaded.sweep, &loaded.sweep_hash, &loaded.drm, &loaded.sos, &loaded.systems, point.point_index, draw) {
                Ok(cfg) => queue.push_back(cfg),
                Err(e) => {
                    let run_id = format!("{}_p{}_d{draw}", loaded.sweep.id, point.point_index);
                    results.push(pb::SweepSample { point_index: point.point_index, draw_index: draw, run_id, error: e.to_string(), ..Default::default() });
                }
            }
        }
    }
    (results, queue)
}

pub fn run_study(args: StudyArgs, exe: &Path) -> Result<(), String> {
    run_study_with(args, exe, None)
}

/// [`run_study`], with an optional job backend injected (unit tests drive the job dispatch with a
/// fake one; `None` opens the real executor `--executor` selected). Ignored for
/// [`Dispatch::Process`].
fn run_study_with(args: StudyArgs, exe: &Path, injected: Option<&dyn jobs::JobBackend>) -> Result<(), String> {
    // Question 245: a GMAT startup file has nothing to be given to in a build without GMAT (and
    // every child would refuse it): refused once, here, before any sample runs.
    #[cfg(not(feature = "gmat"))]
    if args.gmat_startup.is_some() {
        return Err(crate::sample_mode::GMAT_STARTUP_REFUSAL.to_string());
    }
    let loaded = preflight(&args)?;
    // Question 254 item 1: the sweep YAML's `id` becomes `<store-dir>/<id>/`; the file store would
    // refuse a bad one only after the whole study ran, so say so now, before any sample does.
    if args.store_dir.is_some() {
        av_cdm::path_id::validate_path_component_id(&loaded.sweep.id)
            .map_err(|e| format!("{}: ParameterSweep.id {:?} cannot name a study directory under --store-dir: {e}", args.sweep.display(), loaded.sweep.id))?;
    }
    // Question 245: open the ClickHouse store first (see the write below for why).
    let clickhouse_store = match &args.clickhouse {
        None => None,
        Some(config) => {
            let label = format!("the ClickHouse study store at {} (database {})", config.url, config.database);
            let store = open_store(StoreBackend::ClickHouse(config.clone())).map_err(|e| format!("opening {label}: {e}"))?;
            Some((label, store))
        }
    };
    let points = av_sweep::expand_grid(&loaded.sweep).map_err(|e| format!("{}: {e}", args.sweep.display()))?;
    let draws = loaded.sweep.monte_carlo_draws;
    // Question 192(d): Sampled when the sweep declares more than one draw OR when it forces
    // dispersion for a single draw (`dispersed: true`); Nominal only when neither applies.
    let error_mode = if loaded.sweep.dispersed || draws > 1 { ExecutionErrorMode::Sampled } else { ExecutionErrorMode::Nominal };

    std::fs::create_dir_all(&args.out_dir).map_err(|e| format!("creating {}: {e}", args.out_dir.display()))?;

    let (mut results, mut queue) = build_samples(&loaded, &points, draws);

    let mut failed_count = 0usize;
    match &args.dispatch {
        // Today's behaviour, unchanged: up to `--workers` children of this binary at a time.
        Dispatch::Process => {
            let mut running: Vec<Running> = Vec::new();
            loop {
                while running.len() < args.workers as usize {
                    let Some(cfg) = queue.pop_front() else { break };
                    let sample_dir = args.out_dir.join(format!("sample_p{}_d{}", cfg.point_index, cfg.draw_index));
                    let run_id = format!("{}_p{}_d{}", loaded.sweep.id, cfg.point_index, cfg.draw_index);
                    let inputs = write_sample_inputs(&sample_dir, &cfg).map_err(|e| format!("writing sample inputs for p{} d{}: {e}", cfg.point_index, cfg.draw_index))?;
                    let r = spawn_sample(exe, &inputs, &cfg, &run_id, error_mode, args.gmat_startup.as_deref(), &sample_dir)?;
                    running.push(r);
                }
                if running.is_empty() {
                    break;
                }

                let mut progressed = false;
                let mut i = 0;
                while i < running.len() {
                    match running[i].child.try_wait() {
                        Ok(Some(status)) => {
                            let r = running.remove(i);
                            let sample = finalize(r.run, Ended::Exit(status));
                            if !sample.error.is_empty() {
                                failed_count += 1;
                            }
                            results.push(sample);
                            progressed = true;
                        }
                        Ok(None) => i += 1,
                        Err(e) => {
                            // A `try_wait` I/O error is rare (bad pid, platform quirk) but must not
                            // abort the study either -- recorded the same as any other sample failure.
                            let r = running.remove(i);
                            let msg = format!("polling sample child (run_id {:?}) failed: {e}", r.run.run_id);
                            results.push(failed_sample(&r.run, msg));
                            failed_count += 1;
                            progressed = true;
                        }
                    }
                }
                if !progressed {
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        }
        // Question 245: one job per sample through an av-jobs executor (jobs.rs), up to
        // `--workers` at once.
        Dispatch::Jobs(j) => {
            let opened;
            let backend: &dyn jobs::JobBackend = match injected {
                Some(b) => b,
                None => {
                    opened = jobs::open_backend(j, &args.out_dir)?;
                    opened.as_ref()
                }
            };
            let finished = jobs::run_jobs(backend, args.workers as usize, queue, &args.out_dir, &loaded.sweep.id, error_mode, &j.image)?;
            failed_count += finished.iter().filter(|s| !s.error.is_empty()).count();
            results.extend(finished);
            // The default scratch root, now empty (the executors remove each job's directory); an
            // operator-chosen --job-scratch-root is left alone.
            if j.scratch_root.is_none() {
                let _ = std::fs::remove_dir(args.out_dir.join(".job-scratch"));
            }
        }
    }

    results.sort_by_key(|s| (s.point_index, s.draw_index));

    // F2: per-(point, score) aggregates across draws -- see this module's own doc comment's "F2
    // additions" section. A MixedPassCriterion refusal here is fatal to the whole study, the same
    // way every other structural, sweep-wide refusal in `preflight` is: it names a real data
    // inconsistency (the same score declared an Objective in one draw and a
    // MeasureOfEffectiveness in another), not a per-sample condition to record and move past.
    let aggregates = av_sweep::aggregate(&results).map_err(|e| format!("computing per-point aggregates: {e}"))?;

    let sweep_results = pb::SweepResults {
        sweep_id: loaded.sweep.id.clone(),
        sweep_hash: loaded.sweep_hash.clone(),
        drm_hash: loaded.drm.hash.clone(), // verified equal to its own canonical hash in preflight()
        samples: results,
        aggregates,
        provenance: Some(pb::Provenance {
            author_kind: pb::AuthorKind::Agent as i32,
            tool: "av-sweep".to_string(),
            run_id: loaded.sweep.id.clone(),
            config_hash: loaded.sweep_hash.clone(),
            // Deliberately not a wall-clock read -- matches av_kernel::drm::executor's own
            // Provenance.created_tai_ns convention (this crate's own task brief: "products carry
            // no wall clock, which is what makes byte-identical re-runs possible").
            created_tai_ns: 0,
            ..Default::default()
        }),
    };

    let pb_path = args.out_dir.join("sweep_results.pb");
    std::fs::write(&pb_path, sweep_results.encode_to_vec()).map_err(|e| format!("writing {}: {e}", pb_path.display()))?;
    let json_path = args.out_dir.join("sweep_results.json");
    std::fs::write(&json_path, json::sweep_results_to_json(&sweep_results)).map_err(|e| format!("writing {}: {e}", json_path.display()))?;

    // F2: the study store is an ADDITIONAL, OPTIONAL destination -- see this module's own doc
    // comment's "F2 additions" section for why --out-dir itself is untouched. Nothing is written
    // under any store directory unless --store-dir was actually given.
    if let Some(store_dir) = &args.store_dir {
        let mut store = FileStudyStore::new(store_dir);
        let location = store.put_study(&sweep_results).map_err(|e| format!("writing the study store at {}: {e}", store_dir.display()))?;
        eprintln!("av-sweep: study store written to {location}");
    }
    // Question 245: the ClickHouse store, opened (connected, schema verified) BEFORE any sample
    // ran, so an unreachable server or a mismatching schema fails in seconds rather than after
    // the whole study. It gets the same SweepResults as the file store.
    if let Some((label, mut store)) = clickhouse_store {
        let location = store.put_study(&sweep_results).map_err(|e| format!("writing study {:?} to {label}: {e}", sweep_results.sweep_id))?;
        eprintln!("av-sweep: study written to {location}");
    }

    eprintln!("av-sweep: study {:?}: {} sample(s), {failed_count} failed", loaded.sweep.id, sweep_results.samples.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn fixture_drm() -> pb::DesignReferenceMission {
        let yaml = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../drms/demo_two_instance.drm.yaml")).expect("read demo_two_instance.drm.yaml");
        av_kernel::drm::schema::parse_drm_yaml(&yaml).expect("parses")
    }
    fn fixture_sos() -> pb::SosConfiguration {
        let yaml = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../drms/demo_two_instance.sos.yaml")).expect("read demo_two_instance.sos.yaml");
        av_kernel::drm::schema::parse_sos_yaml(&yaml).expect("parses")
    }
    fn fixture_systems() -> BTreeMap<String, pb::SystemDefinition> {
        let mut m = BTreeMap::new();
        for f in ["demo_two_instance.system.yaml", "demo_two_instance_ctrl.system.yaml"] {
            let yaml = std::fs::read_to_string(format!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../drms/{}"), f)).expect("read system yaml");
            let sys = av_kernel::drm::schema::parse_system_definition_yaml(&yaml).expect("parses");
            m.insert(sys.id.clone(), sys);
        }
        m
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("av-sweep-study-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Fast, no GMAT: `write_sample_inputs` writes exactly the bytes
    /// `av_sweep::sample_config_hash` covers -- decode the written files back and recompute the
    /// hash from them, rather than trusting a description of what was written.
    #[test]
    fn written_sample_files_recompute_the_same_config_hash() {
        let drm = fixture_drm();
        let sos = fixture_sos();
        let systems = fixture_systems();
        let sweep = pb::ParameterSweep { id: "sweep_test".to_string(), drm_id: "demo_two_instance_drm".to_string(), monte_carlo_draws: 1, ..Default::default() };
        let sweep_hash = av_sweep::canonical_sweep_hash(&sweep);
        let cfg = av_sweep::sample_config(&sweep, &sweep_hash, &drm, &sos, &systems, 0, 0).expect("samples");

        let dir = temp_dir("hash-recompute");
        let inputs = write_sample_inputs(&dir, &cfg).expect("writes");

        let written_drm = pb::DesignReferenceMission::decode(std::fs::read(&inputs.drm_pb).unwrap().as_slice()).unwrap();
        let written_sos = pb::SosConfiguration::decode(std::fs::read(&inputs.sos_pb).unwrap().as_slice()).unwrap();
        let mut written_systems = BTreeMap::new();
        for p in &inputs.system_pbs {
            let sys = pb::SystemDefinition::decode(std::fs::read(p).unwrap().as_slice()).unwrap();
            written_systems.insert(sys.id.clone(), sys);
        }
        let recomputed = av_sweep::sample_config_hash(&written_drm, &written_sos, &written_systems);
        assert_eq!(recomputed, cfg.config_hash, "the bytes actually written must be exactly what config_hash covers");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `write_sample_inputs` names system files `sys_<id>.pb`, one per system, id-sorted (the
    /// BTreeMap's own iteration order) -- proves the on-disk layout this task's brief specifies,
    /// not just the hash.
    #[test]
    fn written_sample_files_use_the_documented_layout() {
        let drm = fixture_drm();
        let sos = fixture_sos();
        let systems = fixture_systems();
        let sweep = pb::ParameterSweep { id: "sweep_test".to_string(), drm_id: "demo_two_instance_drm".to_string(), monte_carlo_draws: 1, ..Default::default() };
        let sweep_hash = av_sweep::canonical_sweep_hash(&sweep);
        let cfg = av_sweep::sample_config(&sweep, &sweep_hash, &drm, &sos, &systems, 0, 0).expect("samples");

        let dir = temp_dir("layout");
        let inputs = write_sample_inputs(&dir, &cfg).expect("writes");
        assert_eq!(inputs.drm_pb, dir.join("drm.pb"));
        assert_eq!(inputs.sos_pb, dir.join("sos.pb"));
        let names: Vec<String> = inputs.system_pbs.iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, vec!["sys_demo_ctrl_sys.pb".to_string(), "sys_leo_demo_sys.pb".to_string()], "id-sorted, sys_<id>.pb naming");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Question 254 item 1: a `SystemDefinition.id` that is not a path component never reaches a
    /// `join`. The sample directory is pre-seeded with `sys_x/`, so an unchecked id of
    /// `x/../../evil` would resolve to `<parent of the sample dir>/evil.pb` and be written there;
    /// instead `write_sample_inputs` refuses it (and `/abs`, `..`, `a\b`, NUL, the empty id) as
    /// `InvalidInput` before creating anything, and the whole tree around it is unchanged.
    #[test]
    fn a_system_id_that_is_not_a_path_component_is_refused_before_any_file_is_written() {
        fn tree(dir: &Path) -> BTreeMap<String, String> {
            fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
                for entry in std::fs::read_dir(dir).unwrap() {
                    let entry = entry.unwrap();
                    let meta = std::fs::symlink_metadata(entry.path()).unwrap();
                    let rel = entry.path().strip_prefix(base).unwrap().display().to_string();
                    if meta.is_dir() {
                        out.insert(rel, "dir".to_string());
                        walk(base, &entry.path(), out);
                    } else {
                        out.insert(rel, format!("{} bytes", meta.len()));
                    }
                }
            }
            let mut out = BTreeMap::new();
            walk(dir, dir, &mut out);
            out
        }
        let sweep = pb::ParameterSweep { id: "sweep_test".to_string(), drm_id: "demo_two_instance_drm".to_string(), monte_carlo_draws: 1, ..Default::default() };
        let sweep_hash = av_sweep::canonical_sweep_hash(&sweep);
        let good = av_sweep::sample_config(&sweep, &sweep_hash, &fixture_drm(), &fixture_sos(), &fixture_systems(), 0, 0).expect("samples");

        let parent = temp_dir("system-id-traversal");
        let sample_dir = parent.join("sample_p0_d0");
        std::fs::create_dir_all(sample_dir.join("sys_x")).unwrap();
        let before = tree(&parent);
        for bad in ["x/../../evil", "../x", "..", "/abs/under/parent", "a\\b", "a\0b", "", "%2e%2e%2fx"] {
            let mut cfg = good.clone();
            let mut sys = cfg.systems.values().next().unwrap().clone();
            sys.id = bad.to_string();
            cfg.systems = BTreeMap::from([(bad.to_string(), sys)]);
            let Err(err) = write_sample_inputs(&sample_dir, &cfg) else { panic!("{bad:?} must be refused") };
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{bad:?}: {err}");
            assert!(err.to_string().contains("cannot name a sample file"), "{err}");
            assert_eq!(tree(&parent), before, "{bad:?}: something was created");
            assert!(!parent.join("evil.pb").exists());
        }
        // The real ids still write.
        write_sample_inputs(&sample_dir, &good).expect("valid ids still write");
        assert!(sample_dir.join("sys_leo_demo_sys.pb").is_file());
        let _ = std::fs::remove_dir_all(&parent);
    }

    /// Fast, no GMAT, no child process: a sweep with two grid points where only the SECOND
    /// point's axis value is out of its declared bound must record exactly one failed sample
    /// (point 1) while point 0's own sample still builds successfully -- proof that a per-point
    /// `sample_config` failure is recorded, not fatal, and does not stop the rest of the batch
    /// from being built. This is the orchestration-level half of "a bad sample never aborts the
    /// study"; `tests/fixture_study.rs`'s own
    /// `a_sample_whose_inputs_are_invalid_is_recorded_with_a_typed_error_and_does_not_abort_the_study`
    /// is the complementary, GMAT-adjacent half (a genuinely bad CHILD input fails fast, before
    /// GMAT, with the real cause named) -- see that test's own doc comment for why the two are
    /// split rather than combined into one expensive end-to-end run.
    #[test]
    fn a_per_point_failure_is_recorded_and_the_rest_of_the_batch_still_builds() {
        let drm = fixture_drm();
        let mut sos = fixture_sos();
        {
            let inst = sos.instances.iter_mut().find(|i| i.name == "demo_flt").unwrap();
            inst.parameter_overrides.push(pb::Parameter { name: "test.bounded".to_string(), value: 5.0, min: 0.0, max: 10.0, ..Default::default() });
        }
        sos.hash = av_kernel::drm::hash::canonical_sos_hash(&sos);
        let systems = fixture_systems();
        let sweep = pb::ParameterSweep {
            id: "batch_test_sweep".to_string(),
            drm_id: "demo_two_instance_drm".to_string(),
            monte_carlo_draws: 1,
            axes: vec![pb::SweepAxis { instance: "demo_flt".to_string(), parameter: "test.bounded".to_string(), values: vec![5.0, 999.0], ..Default::default() }],
            ..Default::default()
        };
        let sweep_hash = av_sweep::canonical_sweep_hash(&sweep);
        let loaded = Loaded { sweep, sweep_hash, drm, sos, systems };
        let points = av_sweep::expand_grid(&loaded.sweep).expect("2 explicit values -> 2 points");
        assert_eq!(points.len(), 2);

        let (results, queue) = build_samples(&loaded, &points, 1);

        assert_eq!(results.len(), 1, "exactly one immediate failure (point 1's out-of-bound value)");
        assert_eq!(results[0].point_index, 1);
        assert!(results[0].error.contains("outside its declared bound"), "{}", results[0].error);
        assert!(results[0].scores.is_empty());
        assert!(results[0].config_hash.is_empty());

        assert_eq!(queue.len(), 1, "point 0's own sample must still have been built successfully");
        assert_eq!(queue.front().unwrap().point_index, 0);
    }

    /// Writes a tiny, executable shell script (`#!/bin/sh` + `body`) under `dir`, named `name`,
    /// and marks it executable -- a stand-in "child" for [`run_study`]'s own `exe` parameter,
    /// which exists specifically so this crate can drive a whole study's worker-pool/`finalize`
    /// machinery without ever touching GMAT (this task's own brief: `run_study(args, exe)` takes
    /// the child executable as a parameter to make exactly this cheap).
    fn write_fake_exe(dir: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("writing fake exe script");
        let mut perms = std::fs::metadata(&path).expect("stat fake exe script").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).expect("chmod +x fake exe script");
        path
    }

    /// `StudyArgs` for a real 2x2 study (`drms/demo_two_instance_sweep.{drm,sweep}.yaml` +
    /// `demo_two_instance.sos.yaml` + both system yamls -- the same fixture
    /// `tests/fixture_study.rs` drives end to end with a real `av-sweep` binary) with `out_dir`
    /// under the caller's own temp dir. No GMAT: whichever fake `exe` the caller passes to
    /// [`run_study`] alongside these args is what actually "runs" each sample.
    fn study_args_for_fake_child(out_dir: &Path) -> StudyArgs {
        StudyArgs {
            sweep: PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../drms/demo_two_instance_sweep.sweep.yaml")),
            drm: PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../drms/demo_two_instance_sweep.drm.yaml")),
            sos: PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../drms/demo_two_instance.sos.yaml")),
            systems: vec![
                PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../drms/demo_two_instance.system.yaml")),
                PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../drms/demo_two_instance_ctrl.system.yaml")),
            ],
            out_dir: out_dir.to_path_buf(),
            workers: 2,
            gmat_startup: None,
            store_dir: None,
            clickhouse: None,
            dispatch: Dispatch::Process,
        }
    }

    /// Decodes `sweep_results.pb` from `out_dir`, asserting it and its `.json` twin both exist
    /// (this task's own brief: both must be written even when every sample fails).
    fn read_results(out_dir: &Path) -> pb::SweepResults {
        let pb_path = out_dir.join("sweep_results.pb");
        let json_path = out_dir.join("sweep_results.json");
        assert!(pb_path.exists(), "sweep_results.pb must be written even when every sample fails");
        assert!(json_path.exists(), "sweep_results.json must be written even when every sample fails");
        let bytes = std::fs::read(&pb_path).expect("reading sweep_results.pb");
        pb::SweepResults::decode(bytes.as_slice()).expect("decoding sweep_results.pb")
    }

    /// Every recorded sample of a study where every child failed: `Ok(())` from `run_study`
    /// itself, 8 samples (4 points x 2 draws, question 192(c) widened the grid from 2 to 4
    /// points), sorted `(point, draw)`, each with a non-empty `error`, empty `scores`, and empty
    /// `products_uri` -- the shared shape every one of this gap's three branches (non-zero exit,
    /// exited 0 with no output, exited 0 with undecodable output) must produce, on top of each
    /// branch's own real-cause text (checked by the caller).
    fn assert_all_eight_failed_and_study_completed(out_dir: &Path) -> pb::SweepResults {
        let results = read_results(out_dir);
        assert_eq!(results.samples.len(), 8, "4 points x 2 draws = 8 samples, all recorded despite every child failing");
        let pairs: Vec<(u32, u32)> = results.samples.iter().map(|s| (s.point_index, s.draw_index)).collect();
        assert_eq!(
            pairs,
            vec![(0, 0), (0, 1), (1, 0), (1, 1), (2, 0), (2, 1), (3, 0), (3, 1)],
            "samples must still be present and sorted by (point, draw), not just however the worker pool finished them"
        );
        for s in &results.samples {
            assert!(!s.error.is_empty(), "p{} d{}: a failed child must leave a non-empty SweepSample.error", s.point_index, s.draw_index);
            assert!(s.scores.is_empty(), "p{} d{}: a failed sample must carry no scores: {:?}", s.point_index, s.draw_index, s.scores);
            assert!(s.products_uri.is_empty(), "p{} d{}: a failed sample must carry no products_uri, got {:?}", s.point_index, s.draw_index, s.products_uri);
        }
        results
    }

    /// Branch 1 of `finalize`: the child exits non-zero. `run_study` must still return `Ok(())`
    /// (a failed sample is never fatal to the study), and every sample's `error` must name the
    /// REAL cause -- both the exit status and the child's own recognizable stderr text, not just
    /// "failed".
    #[test]
    fn a_failing_child_is_recorded_with_its_exit_status_and_stderr_and_the_study_still_completes() {
        let dir = temp_dir("failing-child");
        let exe = write_fake_exe(&dir, "fake_exe.sh", "echo 'FAKE_CHILD_STDERR_MARKER: intentional failure for av-sweep study.rs coverage test' 1>&2\nexit 7");

        let out_dir = dir.join("out");
        let result = run_study(study_args_for_fake_child(&out_dir), &exe);
        assert!(result.is_ok(), "a failed sample must never be fatal to the study: {result:?}");

        let results = assert_all_eight_failed_and_study_completed(&out_dir);
        for s in &results.samples {
            assert!(s.error.contains("exit status: 7"), "p{} d{}: error must name the real exit status: {}", s.point_index, s.draw_index, s.error);
            assert!(
                s.error.contains("FAKE_CHILD_STDERR_MARKER"),
                "p{} d{}: error must name the real cause via the child's own captured stderr: {}",
                s.point_index,
                s.draw_index,
                s.error
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Branch 2 of `finalize`: the child exits 0 but never writes its `--out` file at all (crashed
    /// after GMAT teardown, killed mid-write-setup, etc.). Distinguish from branch 3 (writes
    /// garbage) -- this one never creates the file.
    #[test]
    fn a_child_that_exits_zero_without_writing_output_is_recorded_with_its_real_cause_and_the_study_still_completes() {
        let dir = temp_dir("no-output-child");
        let exe = write_fake_exe(&dir, "fake_exe.sh", "exit 0");

        let out_dir = dir.join("out");
        let result = run_study(study_args_for_fake_child(&out_dir), &exe);
        assert!(result.is_ok(), "a failed sample must never be fatal to the study: {result:?}");

        let results = assert_all_eight_failed_and_study_completed(&out_dir);
        for s in &results.samples {
            assert!(
                s.error.contains("could not be read"),
                "p{} d{}: error must name the real cause (no output file was ever written), not just \"failed\": {}",
                s.point_index,
                s.draw_index,
                s.error
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Branch 3 of `finalize`: the child exits 0 and writes SOMETHING to `--out`, but it is not a
    /// decodable `altavista.v1.RunProducts` (truncated write, wrong message type, corrupted
    /// bytes). The child receives `--out` as a normal argument, so the fake script here scans its
    /// own `"$@"` for it and writes 3 bytes whose leading byte (`0xFF`) is an invalid protobuf
    /// wire type (`0xFF & 0x7 == 7`; valid wire types are 0-5) -- guaranteed to fail
    /// `prost::Message::decode` on its very first tag, not just "happen to" fail today.
    #[test]
    fn a_child_that_writes_undecodable_output_is_recorded_with_its_real_cause_and_the_study_still_completes() {
        let dir = temp_dir("garbage-output-child");
        let script = r#"out=""
prev=""
for arg in "$@"; do
  if [ "$prev" = "--out" ]; then
    out="$arg"
  fi
  prev="$arg"
done
printf '\377\377\377garbage-not-a-valid-protobuf-message' > "$out"
exit 0"#;
        let exe = write_fake_exe(&dir, "fake_exe.sh", script);

        let out_dir = dir.join("out");
        let result = run_study(study_args_for_fake_child(&out_dir), &exe);
        assert!(result.is_ok(), "a failed sample must never be fatal to the study: {result:?}");

        let results = assert_all_eight_failed_and_study_completed(&out_dir);
        for s in &results.samples {
            assert!(
                s.error.contains("did not decode as altavista.v1.RunProducts"),
                "p{} d{}: error must name the real cause (undecodable output), not just \"failed\": {}",
                s.point_index,
                s.draw_index,
                s.error
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A job backend that finishes every job with a decodable `RunProducts` carrying one score
    /// (derived from the job's own id), except the one job id it is told to fail.
    struct ScriptedBackend {
        fail_suffix: &'static str,
        runs: std::sync::Mutex<Vec<String>>,
    }
    impl jobs::JobBackend for ScriptedBackend {
        fn run(&self, spec: &pb::JobSpec, _inputs: &[av_jobs::runner::JobInput]) -> jobs::JobOutcome {
            self.runs.lock().unwrap().push(spec.job_id.clone());
            if spec.job_id.ends_with(self.fail_suffix) {
                let f = pb::JobFailure { kind: pb::JobFailureKind::NonzeroExit as i32, detail: format!("scripted failure of {}; log tail: \"the tail\"", spec.job_id), exit_code: 137 };
                return jobs::JobOutcome { result: Err(f), log: Some("the whole log\nthe tail\n".into()), placement: None, warning: None };
            }
            let value = spec.job_id.len() as f64;
            let score = pb::ScoreResult { name: "m".to_string(), value, unit: pb::Unit::Meter as i32, passed: None };
            let bytes = pb::RunProducts { run_id: spec.job_id.clone(), scores: BTreeMap::from([("m".to_string(), score)]), ..Default::default() }.encode_to_vec();
            jobs::JobOutcome { result: Ok(vec![av_jobs::runner::JobOutput { bytes, media_type: String::new(), manifest: false }]), log: Some("ok\n".into()), placement: None, warning: None }
        }
    }

    /// Question 245: `--executor container|k8s` records the SAME study shape as child processes --
    /// 8 samples sorted `(point, draw)`, successes with scores and a `products_uri`, the one failed
    /// job a failed sample naming its kind, exit code and log tail, no sample run twice, per-point
    /// aggregates computed, `run_products.pb`/`stderr.txt` in every sample directory, and no scratch
    /// directory left behind.
    #[test]
    fn a_job_dispatched_study_records_the_same_shape_and_a_failed_job_is_one_failed_sample() {
        let dir = temp_dir("jobs-dispatch");
        let out_dir = dir.join("out");
        let mut args = study_args_for_fake_child(&out_dir);
        args.workers = 3;
        let image = crate::cli::ImageRef { name: "av-sweep-sample".to_string(), digest: format!("sha256:{}", "1".repeat(64)) };
        args.dispatch = Dispatch::Jobs(Box::new(crate::cli::JobsArgs { target: crate::cli::JobsTarget::Container, image, scratch_root: None, labels: BTreeMap::new() }));
        let backend = ScriptedBackend { fail_suffix: "_p1_d0", runs: std::sync::Mutex::new(Vec::new()) };
        // `exe` is never spawned in this dispatch mode.
        let result = run_study_with(args, Path::new("/no/such/exe"), Some(&backend));
        assert!(result.is_ok(), "a failed job must never be fatal to the study: {result:?}");

        let results = read_results(&out_dir);
        assert_eq!(results.samples.len(), 8);
        let pairs: Vec<(u32, u32)> = results.samples.iter().map(|s| (s.point_index, s.draw_index)).collect();
        assert_eq!(pairs, vec![(0, 0), (0, 1), (1, 0), (1, 1), (2, 0), (2, 1), (3, 0), (3, 1)]);
        let failed: Vec<&pb::SweepSample> = results.samples.iter().filter(|s| !s.error.is_empty()).collect();
        assert_eq!(failed.len(), 1);
        assert_eq!((failed[0].point_index, failed[0].draw_index), (1, 0));
        for want in ["JOB_FAILURE_KIND_NONZERO_EXIT", "exit code 137", "scripted failure of demo_two_instance_sweep_p1_d0", "the tail"] {
            assert!(failed[0].error.contains(want), "error lacks {want:?}: {}", failed[0].error);
        }
        assert!(failed[0].scores.is_empty() && failed[0].products_uri.is_empty());
        for s in results.samples.iter().filter(|s| s.error.is_empty()) {
            assert_eq!(s.scores["m"].value, s.run_id.len() as f64);
            assert!(s.products_uri.ends_with(&format!("sample_p{}_d{}", s.point_index, s.draw_index)), "{}", s.products_uri);
            assert!(out_dir.join(format!("sample_p{}_d{}", s.point_index, s.draw_index)).join("run_products.pb").is_file());
        }
        assert!(out_dir.join("sample_p1_d0").join("stderr.txt").is_file());
        assert_eq!(std::fs::read_to_string(out_dir.join("sample_p1_d0").join("stderr.txt")).unwrap(), "the whole log\nthe tail\n");
        // Point 1 has one failed draw and one good one; its aggregate counts the good one only.
        assert!(!results.aggregates.is_empty());
        // Each of the 8 samples was run once, and none twice.
        let mut runs = backend.runs.lock().unwrap().clone();
        runs.sort();
        runs.dedup();
        assert_eq!(runs.len(), 8);
        assert_eq!(backend.runs.lock().unwrap().len(), 8, "no sample is run twice");
        assert!(!out_dir.join(".job-scratch").exists(), "the default scratch root is not left behind");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn capped_stderr_marks_truncation_and_reports_the_real_content() {
        let dir = temp_dir("stderr-cap");
        let short_path = dir.join("short.txt");
        std::fs::write(&short_path, b"a real error message").unwrap();
        let short = read_capped_stderr(&short_path);
        assert_eq!(short, "a real error message");

        let long_path = dir.join("long.txt");
        let long_content = vec![b'x'; MAX_STDERR_BYTES + 500];
        std::fs::write(&long_path, &long_content).unwrap();
        let capped = read_capped_stderr(&long_path);
        assert!(capped.starts_with(&"x".repeat(100)), "must keep the real (leading) content");
        assert!(capped.contains("truncated to 4096 bytes"), "must say explicitly that it was truncated: {capped}");
        assert!(capped.contains(&(MAX_STDERR_BYTES + 500).to_string()), "must name the real total size: {capped}");

        let empty_path = dir.join("empty.txt");
        std::fs::write(&empty_path, b"").unwrap();
        assert_eq!(read_capped_stderr(&empty_path), "<empty>");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// F2: `--store-dir` is additional and optional -- given, the study store's own files
    /// (`sweep_results.pb`/`.json`/`samples.jsonl` under `<store-dir>/<sweep_id>/`) must exist
    /// after the study runs; omitted, no store directory is created at all, and `--out-dir`'s own
    /// layout is unaffected either way. Uses the same fake-child machinery as the tests above (no
    /// GMAT needed): the study store only cares about the FINISHED `SweepResults`, not about how
    /// each sample's child behaved.
    #[test]
    fn study_mode_writes_the_store_when_store_dir_is_given_and_not_otherwise() {
        let dir = temp_dir("store-dir-wiring");
        let exe = write_fake_exe(&dir, "fake_exe.sh", "exit 7"); // every sample fails; irrelevant to this test

        // Without --store-dir: no store directory appears anywhere near out_dir.
        let out_dir_no_store = dir.join("out-no-store");
        let result = run_study(study_args_for_fake_child(&out_dir_no_store), &exe);
        assert!(result.is_ok(), "{result:?}");
        assert!(out_dir_no_store.join("sweep_results.pb").exists(), "--out-dir's own layout is unaffected either way");
        let would_be_store_dir = dir.join("store-not-requested");
        assert!(!would_be_store_dir.exists(), "no store directory must be created when --store-dir was never given");

        // With --store-dir: the store's own three files exist under <store-dir>/<sweep_id>/.
        let out_dir_with_store = dir.join("out-with-store");
        let store_dir = dir.join("store");
        let mut args = study_args_for_fake_child(&out_dir_with_store);
        args.store_dir = Some(store_dir.clone());
        let result = run_study(args, &exe);
        assert!(result.is_ok(), "{result:?}");
        assert!(out_dir_with_store.join("sweep_results.pb").exists(), "--out-dir's own layout is unaffected either way");

        let study_dir = store_dir.join("demo_two_instance_sweep");
        assert!(study_dir.join("sweep_results.pb").exists());
        assert!(study_dir.join("sweep_results.json").exists());
        assert!(study_dir.join("samples.jsonl").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Writes a `ParameterSweep` YAML with a correctly-computed `hash:` field -- `preflight`
    /// (via `av_sweep::verify_sweep_hash`) refuses a mismatched one, so every test below that
    /// hand-authors a sweep needs a real hash, not a placeholder.
    fn write_hashed_sweep_yaml(path: &Path, body_without_hash: &str) {
        let with_empty_hash = format!("{body_without_hash}\nhash: \"\"\n");
        let parsed = av_sweep::parse_sweep_yaml(&with_empty_hash).expect("the fixture body itself must parse");
        let hash = av_sweep::canonical_sweep_hash(&parsed);
        let with_real_hash = format!("{body_without_hash}\nhash: {hash:?}\n");
        std::fs::write(path, with_real_hash).expect("writing the hashed sweep YAML");
    }

    /// Question 192(d): `preflight`'s own mirror of `av_sweep::sample::sample_config`'s
    /// `DispersedWithoutSeeds` refusal -- a `dispersed: true` sweep over a DRM with no
    /// `Scenario.seeds` (`demo_two_instance.drm.yaml`, the non-sweep fixture) is refused once, up
    /// front, before any sample is built. Fails against an implementation that added the check
    /// only to `sample_config` and forgot `preflight`'s own mirror (this crate's own standing
    /// rule: two copies of the same check can drift apart -- `preflight` hand-duplicates
    /// `sample_config`'s message text specifically so this is caught if they ever disagree).
    #[test]
    fn preflight_refuses_a_dispersed_sweep_with_no_declared_seeds() {
        let dir = temp_dir("preflight-dispersed-without-seeds");
        let sweep_path = dir.join("dispersed.sweep.yaml");
        write_hashed_sweep_yaml(&sweep_path, "id: dispersed_test_sweep\ndrm_id: demo_two_instance_drm\naxes: []\nmonte_carlo_draws: 1\ndispersed: true");

        let args = StudyArgs {
            sweep: sweep_path,
            drm: PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../drms/demo_two_instance.drm.yaml")), // declares no Scenario.seeds at all
            sos: PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../drms/demo_two_instance.sos.yaml")),
            systems: vec![
                PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../drms/demo_two_instance.system.yaml")),
                PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../drms/demo_two_instance_ctrl.system.yaml")),
            ],
            out_dir: dir.join("out"),
            workers: 1,
            gmat_startup: None,
            store_dir: None,
            clickhouse: None,
            dispatch: Dispatch::Process,
        };
        // Not `.unwrap_err()`: that requires the `Ok` side (`Loaded`) to implement `Debug` for
        // its own panic message, which it does not (mirrors `store.rs`'s own tests, which use
        // the same explicit-match style for the same reason) -- matched explicitly instead.
        let err = match preflight(&args) {
            Err(e) => e,
            Ok(_) => panic!("a dispersed sweep with no declared Scenario.seeds must be refused"),
        };
        assert!(err.contains("dispersed is true but Scenario.seeds is empty"), "{err}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A `dispersed: true`, `monte_carlo_draws: 1` sweep over a DRM that DOES declare
    /// `Scenario.seeds` (`demo_two_instance_sweep.drm.yaml`, its own `burn_seed` key) must
    /// therefore pass `preflight` and select `ExecutionErrorMode::Sampled` for its one child --
    /// proven by having the fake child record its own argv, so this checks what `study.rs`
    /// actually told the child to do, not merely that the study completed. Fails against an
    /// implementation that kept the old `draws > 1` rule verbatim (`dispersed` ignored): such an
    /// implementation would tell this single-draw child `--error-mode nominal`.
    #[test]
    fn a_dispersed_single_draw_sweep_selects_sampled_error_mode_for_its_child() {
        let dir = temp_dir("dispersed-error-mode");
        let sweep_path = dir.join("dispersed.sweep.yaml");
        write_hashed_sweep_yaml(&sweep_path, "id: dispersed_single_draw_sweep\ndrm_id: demo_two_instance_sweep_drm\naxes: []\nmonte_carlo_draws: 1\ndispersed: true");

        // Fake child: append its own argv (one arg per line) to <sample_dir>/argv.txt (derived
        // from --out's own directory) and exit 7 -- irrelevant here whether it "succeeds"; this
        // test only inspects what study.rs told the child to do.
        let script = r#"out=""
prev=""
for arg in "$@"; do
  if [ "$prev" = "--out" ]; then
    out="$arg"
  fi
  prev="$arg"
done
dir=$(dirname "$out")
: > "$dir/argv.txt"
for arg in "$@"; do
  echo "$arg" >> "$dir/argv.txt"
done
exit 7"#;
        let exe = write_fake_exe(&dir, "fake_exe.sh", script);

        let out_dir = dir.join("out");
        let args = StudyArgs {
            sweep: sweep_path,
            drm: PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../drms/demo_two_instance_sweep.drm.yaml")),
            sos: PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../drms/demo_two_instance.sos.yaml")),
            systems: vec![
                PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../drms/demo_two_instance.system.yaml")),
                PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../drms/demo_two_instance_ctrl.system.yaml")),
            ],
            out_dir: out_dir.clone(),
            workers: 1,
            gmat_startup: None,
            store_dir: None,
            clickhouse: None,
            dispatch: Dispatch::Process,
        };
        let result = run_study(args, &exe);
        assert!(result.is_ok(), "a failed sample must never be fatal to the study: {result:?}");

        let argv_path = out_dir.join("sample_p0_d0").join("argv.txt");
        let argv = std::fs::read_to_string(&argv_path).unwrap_or_else(|e| panic!("reading {}: {e}", argv_path.display()));
        let args_list: Vec<&str> = argv.lines().collect();
        let error_mode_idx = args_list.iter().position(|a| *a == "--error-mode").expect("--error-mode must have been passed to the child");
        assert_eq!(args_list[error_mode_idx + 1], "sampled", "a dispersed single-draw sweep must select Sampled, not Nominal: argv={args_list:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
