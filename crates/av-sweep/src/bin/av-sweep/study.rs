//! Study mode: the parent. Never initializes GMAT -- every sample runs in its own child process
//! (`main.rs`'s own module doc comment: GMAT is a process-global singleton, so exactly one DRM
//! run happens per process).
//!
//! ## Two kinds of refusal, deliberately different in scope
//!
//! [`preflight`] checks everything that would fail *identically for every single sample*
//! (a tampered sweep/DRM/SOS/system, `sweep.drm_id` not matching the DRM, `monte_carlo_draws ==
//! 0`, `monte_carlo_draws > 1` with no declared `Scenario.seeds`) -- these are refused ONCE,
//! fatally, before anything is written, exactly the way `crate::sample::sample_config` itself
//! would refuse them, but without silently producing a "study" of zero samples (a `draws == 0`
//! sweep would otherwise iterate `0..0` and write an empty, successful-looking result) or of N
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

use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::Duration;

use av_cdm::pb;
use av_kernel::drm::ExecutionErrorMode;
use av_sweep::store::{FileStudyStore, StudyStore};
use av_sweep::{json, SampleConfig};
use prost::Message;

use crate::cli::StudyArgs;

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

/// [`SampleConfig::seeds`]'s single-`uint64` projection onto `SweepSample.seed` -- the lead's own
/// decision (this task's own brief): a `BTreeMap` iterates key-sorted already, so `.next()`
/// alone gives "the one key", "the lexicographically first of several keys", or (empty map)
/// nothing, uniformly, in all three declared cases at once. The full per-key map always still
/// lives in the sample's own `drm.pb` (its `Scenario.seeds`), which `run.proto`'s single-field
/// `SweepSample.seed` cannot itself express -- disclosed here and in this crate's own final
/// report, not silently narrowed.
fn projected_seed(seeds: &BTreeMap<String, u64>) -> u64 {
    seeds.values().next().copied().unwrap_or(0)
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

/// One sample currently running as a child process, carrying everything [`finalize`] needs once
/// it exits.
struct Running {
    point_index: u32,
    draw_index: u32,
    run_id: String,
    config_hash: String,
    axis_values: BTreeMap<String, f64>,
    seed: u64,
    sample_dir: PathBuf,
    stderr_path: PathBuf,
    out_path: PathBuf,
    child: Child,
}

/// Spawns one sample's child (`exe --run-sample ...`), stderr redirected straight to
/// `<sample_dir>/stderr.txt` (a real file, not a pipe read by this process) -- this task's own
/// brief allows no new dependency and no threads for the worker pool; redirecting the child's
/// own stderr fd to a file sidesteps the classic "unread pipe fills up and the child blocks"
/// deadlock without needing a reader thread at all, and satisfies "stderr.txt ... always
/// written" for free (the file exists, even empty, from the moment the child is spawned).
fn spawn_sample(exe: &Path, inputs: &SampleInputPaths, cfg: &SampleConfig, run_id: &str, error_mode: ExecutionErrorMode, gmat_startup: Option<&str>, sample_dir: &Path) -> Result<Running, String> {
    let out_path = sample_dir.join("run_products.pb");
    let stderr_path = sample_dir.join("stderr.txt");
    let stderr_file = std::fs::File::create(&stderr_path).map_err(|e| format!("creating {}: {e}", stderr_path.display()))?;

    let mut cmd = Command::new(exe);
    cmd.arg("--run-sample").arg("--drm-pb").arg(&inputs.drm_pb).arg("--sos-pb").arg(&inputs.sos_pb);
    for sp in &inputs.system_pbs {
        cmd.arg("--system-pb").arg(sp);
    }
    cmd.arg("--run-id").arg(run_id).arg("--error-mode").arg(if error_mode == ExecutionErrorMode::Sampled { "sampled" } else { "nominal" }).arg("--out").arg(&out_path);
    if let Some(s) = gmat_startup {
        cmd.arg("--gmat-startup").arg(s);
    }
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::from(stderr_file));

    let child = cmd.spawn().map_err(|e| format!("spawning sample child for run_id {run_id:?}: {e}"))?;
    Ok(Running {
        point_index: cfg.point_index,
        draw_index: cfg.draw_index,
        run_id: run_id.to_string(),
        config_hash: cfg.config_hash.clone(),
        axis_values: cfg.axis_values.clone(),
        seed: projected_seed(&cfg.seeds),
        sample_dir: sample_dir.to_path_buf(),
        stderr_path,
        out_path,
        child,
    })
}

fn failed_sample(r: &Running, error: String) -> pb::SweepSample {
    pb::SweepSample {
        point_index: r.point_index,
        draw_index: r.draw_index,
        axis_values: r.axis_values.clone(),
        seed: r.seed,
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
fn finalize(r: Running, status: ExitStatus) -> pb::SweepSample {
    if status.success() {
        match std::fs::read(&r.out_path) {
            Ok(bytes) => match pb::RunProducts::decode(bytes.as_slice()) {
                Ok(products) => {
                    let products_uri = std::fs::canonicalize(&r.sample_dir).map(|p| p.display().to_string()).unwrap_or_else(|_| r.sample_dir.display().to_string());
                    return pb::SweepSample {
                        point_index: r.point_index,
                        draw_index: r.draw_index,
                        axis_values: r.axis_values,
                        seed: r.seed,
                        run_id: r.run_id,
                        config_hash: r.config_hash,
                        scores: products.scores,
                        products_uri,
                        error: String::new(),
                    };
                }
                Err(e) => {
                    let msg = format!("sample child exited 0 but {} did not decode as altavista.v1.RunProducts: {e}; stderr: {}", r.out_path.display(), read_capped_stderr(&r.stderr_path));
                    return failed_sample(&r, msg);
                }
            },
            Err(e) => {
                let msg = format!("sample child exited 0 but its output at {} could not be read: {e}; stderr: {}", r.out_path.display(), read_capped_stderr(&r.stderr_path));
                return failed_sample(&r, msg);
            }
        }
    }
    let msg = format!("sample child {}; stderr: {}", describe_exit(&status), read_capped_stderr(&r.stderr_path));
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
    let loaded = preflight(&args)?;
    let points = av_sweep::expand_grid(&loaded.sweep).map_err(|e| format!("{}: {e}", args.sweep.display()))?;
    let draws = loaded.sweep.monte_carlo_draws;
    let error_mode = if draws > 1 { ExecutionErrorMode::Sampled } else { ExecutionErrorMode::Nominal };

    std::fs::create_dir_all(&args.out_dir).map_err(|e| format!("creating {}: {e}", args.out_dir.display()))?;

    let (mut results, mut queue) = build_samples(&loaded, &points, draws);

    let mut running: Vec<Running> = Vec::new();
    let mut failed_count = 0usize;
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
                    let sample = finalize(r, status);
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
                    let msg = format!("polling sample child (run_id {:?}) failed: {e}", r.run_id);
                    results.push(failed_sample(&r, msg));
                    failed_count += 1;
                    progressed = true;
                }
            }
        }
        if !progressed {
            std::thread::sleep(Duration::from_millis(20));
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

    #[test]
    fn projected_seed_picks_the_lexicographically_first_key_or_zero() {
        assert_eq!(projected_seed(&BTreeMap::new()), 0, "no keys -> 0");
        let one = BTreeMap::from([("only".to_string(), 777u64)]);
        assert_eq!(projected_seed(&one), 777, "exactly one key -> that key's value");
        let many = BTreeMap::from([("zzz".to_string(), 999u64), ("aaa".to_string(), 111u64), ("mmm".to_string(), 555u64)]);
        assert_eq!(projected_seed(&many), 111, "more than one key -> the lexicographically first key's value");
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
    /// itself, 4 samples (2 points x 2 draws), sorted `(point, draw)`, each with a non-empty
    /// `error`, empty `scores`, and empty `products_uri` -- the shared shape every one of this
    /// gap's three branches (non-zero exit, exited 0 with no output, exited 0 with undecodable
    /// output) must produce, on top of each branch's own real-cause text (checked by the caller).
    fn assert_all_four_failed_and_study_completed(out_dir: &Path) -> pb::SweepResults {
        let results = read_results(out_dir);
        assert_eq!(results.samples.len(), 4, "2 points x 2 draws = 4 samples, all recorded despite every child failing");
        let pairs: Vec<(u32, u32)> = results.samples.iter().map(|s| (s.point_index, s.draw_index)).collect();
        assert_eq!(pairs, vec![(0, 0), (0, 1), (1, 0), (1, 1)], "samples must still be present and sorted by (point, draw), not just however the worker pool finished them");
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

        let results = assert_all_four_failed_and_study_completed(&out_dir);
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

        let results = assert_all_four_failed_and_study_completed(&out_dir);
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

        let results = assert_all_four_failed_and_study_completed(&out_dir);
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
}
