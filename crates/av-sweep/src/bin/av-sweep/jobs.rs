//! Study mode's job dispatch (question 245, heavy-ct-2b): `--executor container|k8s` turns each
//! sample into one `av-jobs` job instead of one child process.
//!
//! # What a sample job is
//!
//! One [`pb::JobSpec`] per sample (`executor: JOB_EXECUTOR_KIND_CONTAINER`, the sample image by
//! digest, `job_id` the sample's run id) whose inputs are the three kinds of file
//! [`crate::study::write_sample_inputs`] wrote -- `drm.pb`, `sos.pb`, then `sys_<id>.pb` in id order
//! -- as [`JobInput`]s in that order (the executor stages them as `/av-job/in/000`, `001`, ...), and
//! whose command is exactly the child process's command line, pointed at the job's mount points:
//!
//! ```text
//! /usr/local/bin/av-sweep --run-sample --drm-pb /av-job/in/000 --sos-pb /av-job/in/001
//!     --system-pb /av-job/in/002 [...] --run-id <run id> --error-mode nominal|sampled
//!     --out /av-job/out/run_products.pb
//! ```
//!
//! The one output file becomes `<sample dir>/run_products.pb` and the job's log
//! `<sample dir>/stderr.txt`; [`crate::study::finalize`] then turns the sample into its
//! `SweepSample` exactly as it does for a child process (the same decode check, the same record),
//! so the study record has one shape whatever ran the sample.
//!
//! # Inputs are hash-verified before a job is ever created
//!
//! `av-jobs`' `Runner` verifies every input against its `AssetRef.sha256` before an executor sees
//! it; study mode drives the executors directly, so it does the same here ([`prepare_job`]): the
//! expected digest of each input is computed from the in-memory message (`SampleConfig`) that
//! `write_sample_inputs` encoded, the bytes are read back from the sample directory, and a
//! mismatch is a typed `INPUT_HASH_MISMATCH` failure for that sample -- recorded as a failed
//! sample, with no job created for it.
//!
//! # Never retried
//!
//! A failed job is a failed sample ([`crate::study`]'s `failed_sample`, error text from
//! [`describe_failure`]: the failure kind, exit code and detail, which for k8s carries the
//! Kubernetes reason, the pod names and the log tail, and for a container the exit status and the
//! log tail, or why there is no log). The job's log -- the pod's, or the container's stdout and
//! stderr, capped the same way -- is each sample's `stderr.txt`; a log that could not be read is
//! written as `<no ... log: reason>`, never left empty. Nothing here retries; the k8s executor
//! additionally refuses a second pod for a job (`K8sError::Retried`).
//!
//! # Concurrency
//!
//! [`run_jobs`] runs up to `--workers` jobs at once from that many threads over one shared
//! backend (`K8sJobExecutor` is `Sync`: each call is its own Job and pod). On k3s those are several
//! pods on the one node, not several hosts (`av_jobs::k8s`' module doc: the `hostPath` data plane is
//! single-node).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use av_cdm::pb;
use av_jobs::container::{ContainerExecutor, CONTAINER_INPUT_DIR, CONTAINER_OUTPUT_DIR};
use av_jobs::k8s::{K8sConfig, K8sJobExecutor, Kubeconfig, NetworkGate, Resources};
use av_jobs::runner::{JobInput, JobOutput};
use av_kernel::drm::ExecutionErrorMode;
use av_sweep::{sha256_hex, SampleConfig};

use crate::cli::{ImageRef, JobsArgs, JobsTarget, NetworkPosture};
use crate::study::{finalize, write_sample_inputs, Ended, SampleInputPaths, SampleRun};

/// Where the sample image puts the binary (`services/sweep/Dockerfile`'s `COPY` destination; a
/// test keeps the two from drifting apart). `command[0]` of every sample job. The image has no
/// ENTRYPOINT: `docker run <image> <command...>` appends the command to one and a Kubernetes
/// `command` replaces it, so with none the one command line, naming the binary in full, runs the
/// same under both executors (measured the hard way: with an ENTRYPOINT, docker ran
/// `av-sweep /usr/local/bin/av-sweep --run-sample ...`).
pub const SAMPLE_BINARY: &str = "/usr/local/bin/av-sweep";

/// The one output file a sample job writes, under the job's output mount.
pub const SAMPLE_OUTPUT_NAME: &str = "run_products.pb";

/// `JobSpec.kind` of a sample job (informational; the executors dispatch on `executor`).
pub const SAMPLE_JOB_KIND: &str = "av-sweep-sample";

/// What one job run came to, whichever backend ran it.
pub struct JobOutcome {
    pub result: Result<Vec<JobOutput>, pb::JobFailure>,
    /// The job's log, for `stderr.txt` (`None`: the backend has none to give).
    pub log: Option<String>,
    /// One line about where it ran (the pod and the times the cluster recorded), for the study's
    /// own stderr; the evidence that several pods ran at once.
    pub placement: Option<String>,
    /// A teardown problem that did not change the result.
    pub warning: Option<String>,
}

/// One way to run a sample job. `Sync`: [`run_jobs`] calls it from several threads.
pub trait JobBackend: Sync {
    fn run(&self, spec: &pb::JobSpec, inputs: &[JobInput]) -> JobOutcome;
}

/// `--executor k8s`: [`K8sJobExecutor::run`], keeping the report's log and placement.
pub struct K8sBackend(pub K8sJobExecutor);

impl JobBackend for K8sBackend {
    fn run(&self, spec: &pb::JobSpec, inputs: &[JobInput]) -> JobOutcome {
        let report = self.0.run(spec, inputs);
        let log = report.log.as_ref().map(|l| l.text.clone()).or_else(|| report.log_error.as_ref().map(|e| format!("<no pod log: {e}>\n")));
        let placement = report.pods.first().map(|pod| {
            let t = report.pod_times.clone().unwrap_or_default();
            format!(
                "pod {pod} (namespace {}) startTime={} container startedAt={} finishedAt={}",
                report.namespace,
                t.pod_start_time.as_deref().unwrap_or("-"),
                t.container_started_at.as_deref().unwrap_or("-"),
                t.container_finished_at.as_deref().unwrap_or("-")
            )
        });
        let warning = report.cleanup.as_ref().err().map(|e| format!("teardown of job {}: {e}", report.job_name));
        // A failure carries the Kubernetes reason, the pod names and the log tail (`K8sError::to_failure`).
        JobOutcome { result: report.into_executor_result(), log, placement, warning }
    }
}

/// `--executor container`: [`ContainerExecutor::run`], keeping the report's log (the container's
/// stdout and stderr, capped like the k8s pod log) for `stderr.txt`; a log that could not be read is
/// said so in `stderr.txt`, never left empty.
pub struct ContainerBackend(pub ContainerExecutor);

impl JobBackend for ContainerBackend {
    fn run(&self, spec: &pb::JobSpec, inputs: &[JobInput]) -> JobOutcome {
        let report = self.0.run(spec, inputs);
        let log = report.log.as_ref().map(|l| l.text.clone()).or_else(|| report.log_error.as_ref().map(|e| format!("<no container log: {e}>\n")));
        let placement = report.container.as_ref().map(|c| format!("container {c}"));
        JobOutcome { result: report.result, log, placement, warning: None }
    }
}

fn failure(kind: pb::JobFailureKind, detail: String) -> pb::JobFailure {
    pb::JobFailure { kind: kind as i32, detail, exit_code: 0 }
}

/// The failure as a sample's error text: kind, exit code, then the executor's own detail (for
/// k8s: the Kubernetes reason, the pod names and the log tail).
pub fn describe_failure(f: &pb::JobFailure) -> String {
    let kind = pb::JobFailureKind::try_from(f.kind).map(|k| k.as_str_name().to_string()).unwrap_or_else(|_| format!("unknown kind {}", f.kind));
    format!("sample job failed ({kind}, exit code {}): {}", f.exit_code, f.detail)
}

/// The command a sample job runs: the child process's own command line, at the job's mount points.
pub fn sample_command(run_id: &str, error_mode: ExecutionErrorMode, system_count: usize) -> Vec<String> {
    let input = |i: usize| format!("{CONTAINER_INPUT_DIR}/{i:03}");
    let mut cmd = vec![SAMPLE_BINARY.to_string(), "--run-sample".into(), "--drm-pb".into(), input(0), "--sos-pb".into(), input(1)];
    for i in 0..system_count {
        cmd.push("--system-pb".into());
        cmd.push(input(2 + i));
    }
    cmd.extend([
        "--run-id".into(),
        run_id.to_string(),
        "--error-mode".into(),
        if error_mode == ExecutionErrorMode::Sampled { "sampled" } else { "nominal" }.into(),
        "--out".into(),
        format!("{CONTAINER_OUTPUT_DIR}/{SAMPLE_OUTPUT_NAME}"),
    ]);
    cmd
}

/// One sample's job and its hash-verified inputs, or the typed failure that stops it before any
/// job is created.
///
/// The expected SHA-256 of each input is that of the message `write_sample_inputs` encoded (from
/// `cfg`, in memory); the bytes are what the sample directory holds now. They must agree.
pub fn prepare_job(cfg: &SampleConfig, paths: &SampleInputPaths, run_id: &str, error_mode: ExecutionErrorMode, image: &ImageRef) -> Result<(pb::JobSpec, Vec<JobInput>), pb::JobFailure> {
    use prost::Message;
    let mut wanted: Vec<(&Path, Vec<u8>)> = vec![(paths.drm_pb.as_path(), cfg.drm.encode_to_vec()), (paths.sos_pb.as_path(), cfg.sos.encode_to_vec())];
    // `cfg.systems` is a BTreeMap, `paths.system_pbs` follows its order (write_sample_inputs).
    for (path, (_, sys)) in paths.system_pbs.iter().zip(cfg.systems.iter()) {
        wanted.push((path.as_path(), sys.encode_to_vec()));
    }
    if wanted.len() != 2 + paths.system_pbs.len() {
        return Err(failure(pb::JobFailureKind::InputMissing, format!("{} system input file(s) written for {} SystemDefinition(s)", paths.system_pbs.len(), cfg.systems.len())));
    }
    let mut asset_refs = Vec::with_capacity(wanted.len());
    let mut inputs = Vec::with_capacity(wanted.len());
    for (path, encoded) in wanted {
        let expected = sha256_hex(&encoded);
        let bytes = std::fs::read(path).map_err(|e| failure(pb::JobFailureKind::InputMissing, format!("reading job input {}: {e}", path.display())))?;
        let actual = sha256_hex(&bytes);
        if actual != expected {
            return Err(failure(
                pb::JobFailureKind::InputHashMismatch,
                format!("job input {}: recorded sha256 {expected} does not match the bytes in the sample directory (sha256 {actual}); no job was created", path.display()),
            ));
        }
        let asset = pb::AssetRef {
            uri: format!("sample-input://{run_id}/{}", path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()),
            sha256: expected,
            size_bytes: bytes.len() as u64,
            media_type: "application/x-protobuf".to_string(),
            ..Default::default()
        };
        asset_refs.push(asset.clone());
        inputs.push(JobInput { asset, bytes });
    }
    let spec = pb::JobSpec {
        job_id: run_id.to_string(),
        kind: SAMPLE_JOB_KIND.to_string(),
        inputs: asset_refs,
        executor: pb::JobExecutorKind::Container as i32,
        command: sample_command(run_id, error_mode, paths.system_pbs.len()),
        container_image: image.name.clone(),
        container_image_digest: image.digest.clone(),
        ..Default::default()
    };
    Ok((spec, inputs))
}

/// Runs one prepared sample (or records why it could not be prepared) and finalizes it: the job's
/// output is written as `run_products.pb` and its log as `stderr.txt` in the sample directory, and
/// [`finalize`] decodes it into the `SweepSample`. Never panics and never retries.
pub fn run_one(backend: &dyn JobBackend, run: SampleRun, prepared: Result<(pb::JobSpec, Vec<JobInput>), pb::JobFailure>) -> pb::SweepSample {
    let began = Instant::now();
    let ended = match prepared {
        Err(f) => {
            let _ = std::fs::write(&run.stderr_path, format!("{}\n", f.detail));
            Err(f)
        }
        Ok((spec, inputs)) => {
            let outcome = backend.run(&spec, &inputs);
            // The log first: a failed sample's error quotes stderr.txt.
            let _ = std::fs::write(&run.stderr_path, outcome.log.as_deref().unwrap_or(""));
            if let Some(w) = &outcome.warning {
                eprintln!("av-sweep: WARNING: {}: {w}", run.run_id);
            }
            let line = outcome.placement.as_deref().unwrap_or("no placement recorded");
            let result = match outcome.result {
                Ok(outputs) => store_output(&run, outputs),
                Err(f) => Err(f),
            };
            eprintln!("av-sweep: sample {} {} after {:.1}s: {line}", run.run_id, if result.is_ok() { "job succeeded" } else { "job FAILED" }, began.elapsed().as_secs_f64());
            result
        }
    };
    finalize(run, Ended::Job(ended))
}

/// Exactly one output file is the contract (`run_products.pb`); it is written where a child
/// process would have written it. Zero or several is a typed rejection, not a guess.
fn store_output(run: &SampleRun, outputs: Vec<JobOutput>) -> Result<(), pb::JobFailure> {
    let [output] = outputs.as_slice() else {
        return Err(failure(pb::JobFailureKind::OutputRejected, format!("the sample job produced {} output file(s); exactly one ({SAMPLE_OUTPUT_NAME}) is the contract", outputs.len())));
    };
    std::fs::write(&run.out_path, &output.bytes).map_err(|e| failure(pb::JobFailureKind::OutputRejected, format!("writing {}: {e}", run.out_path.display())))
}

/// Runs every queued sample as a job, up to `workers` at once, and returns the finished samples
/// (in completion order; the caller sorts). A failure to write a sample's inputs is fatal to the
/// study exactly as in process mode: the first one stops the other workers and is returned.
pub fn run_jobs(
    backend: &dyn JobBackend,
    workers: usize,
    queue: VecDeque<SampleConfig>,
    out_dir: &Path,
    sweep_id: &str,
    error_mode: ExecutionErrorMode,
    image: &ImageRef,
) -> Result<Vec<pb::SweepSample>, String> {
    let queue = Mutex::new(queue);
    let done: Mutex<Vec<pb::SweepSample>> = Mutex::new(Vec::new());
    let fatal: Mutex<Option<String>> = Mutex::new(None);
    let threads = workers.max(1).min(queue.lock().map(|q| q.len()).unwrap_or(1).max(1));
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| loop {
                if fatal.lock().map(|f| f.is_some()).unwrap_or(true) {
                    return;
                }
                let Some(cfg) = queue.lock().ok().and_then(|mut q| q.pop_front()) else { return };
                let sample_dir = out_dir.join(format!("sample_p{}_d{}", cfg.point_index, cfg.draw_index));
                let run_id = format!("{sweep_id}_p{}_d{}", cfg.point_index, cfg.draw_index);
                let paths = match write_sample_inputs(&sample_dir, &cfg) {
                    Ok(p) => p,
                    Err(e) => {
                        if let Ok(mut f) = fatal.lock() {
                            f.get_or_insert(format!("writing sample inputs for p{} d{}: {e}", cfg.point_index, cfg.draw_index));
                        }
                        return;
                    }
                };
                let run = SampleRun::new(&cfg, &run_id, &sample_dir);
                let prepared = prepare_job(&cfg, &paths, &run_id, error_mode, image);
                let sample = run_one(backend, run, prepared);
                if let Ok(mut d) = done.lock() {
                    d.push(sample);
                }
            });
        }
    });
    if let Some(msg) = fatal.into_inner().ok().flatten() {
        return Err(msg);
    }
    done.into_inner().map_err(|_| "a sample worker thread panicked while holding the results".to_string())
}

/// `docker image inspect <name>@<digest>` must report exactly that digest as the image id -- the
/// same content-address check the executors make per run (`av_jobs::container::digest_gate`),
/// made once up front so a study over an absent or wrong image is refused before it records a
/// whole grid of identical failures. Nothing is ever pulled.
fn require_local_image(image: &ImageRef, what: &str) -> Result<(), String> {
    let pinned = format!("{}@{}", image.name, image.digest);
    let output = std::process::Command::new("docker")
        .args(["image", "inspect", &pinned, "--format", "{{.Id}}"])
        .output()
        .map_err(|e| format!("could not launch docker to inspect the {what} {pinned:?}: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "the {what} {pinned:?} is not present locally (this tool never pulls; load or build it first): {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let actual = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if actual != image.digest {
        return Err(format!("the {what} {pinned:?} is present locally as {actual}, not {}; refused rather than trusted", image.digest));
    }
    Ok(())
}

/// Opens the backend a study's `--executor` selected, after the cheap checks: the image (and for
/// k8s the network gate's image) is present locally under its digest, the scratch root exists, and
/// for k8s the cluster passes the executor's own preconditions (namespace exists, one Ready Docker
/// node, default-deny `NetworkPolicy`, fail-closed network). Any refusal is fatal to the study.
pub fn open_backend(jobs: &JobsArgs, out_dir: &Path) -> Result<Box<dyn JobBackend>, String> {
    require_local_image(&jobs.image, "sample image")?;
    let scratch_root: PathBuf = jobs.scratch_root.clone().unwrap_or_else(|| out_dir.join(".job-scratch"));
    std::fs::create_dir_all(&scratch_root).map_err(|e| format!("creating the job scratch root {}: {e}", scratch_root.display()))?;
    match &jobs.target {
        JobsTarget::Container => Ok(Box::new(ContainerBackend(ContainerExecutor::new(scratch_root, jobs.labels.clone())))),
        JobsTarget::K8s(k) => {
            let kubeconfig = Kubeconfig::load_default(k.context.as_deref()).map_err(|e| format!("kubeconfig: {e}"))?;
            let mut config = K8sConfig::new(kubeconfig, k.namespace.clone(), scratch_root);
            config.extra_labels = jobs.labels.clone();
            let mut resources = Resources::default();
            if let Some(cpu) = &k.cpu_request {
                resources.cpu_request = cpu.clone();
            }
            if let Some(mem) = &k.memory_request {
                resources.memory_request = mem.clone();
            }
            config.resources = resources;
            if let Some(secs) = k.deadline_secs {
                config.deadline = Duration::from_secs(secs);
            }
            match &k.network {
                NetworkPosture::Gate { image, within_secs } => {
                    require_local_image(image, "network gate image")?;
                    config.network_gate = Some(NetworkGate { image: image.name.clone(), image_digest: image.digest.clone(), within: Duration::from_secs(*within_secs) });
                }
                NetworkPosture::AcceptStartupWindow => config.accept_startup_network_window = true,
            }
            let executor = K8sJobExecutor::new(config).map_err(|e| format!("opening the k8s executor (namespace {:?}): {e}", k.namespace))?;
            Ok(Box::new(K8sBackend(executor)))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use prost::Message;

    use super::*;

    const IMG_DIGEST: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";

    fn image() -> ImageRef {
        ImageRef { name: "av-sweep-sample".to_string(), digest: IMG_DIGEST.to_string() }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("av-sweep-jobs-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Every sample of the heavy-ct-2b fixture (`drms/leo_orbital_native_sweep.*`): 4 points x 3 draws.
    fn leo_configs() -> Vec<SampleConfig> {
        let drms = |f: &str| std::fs::read_to_string(format!("{}/../../drms/{f}", env!("CARGO_MANIFEST_DIR"))).unwrap_or_else(|e| panic!("reading {f}: {e}"));
        let sweep = av_sweep::parse_sweep_yaml(&drms("leo_orbital_native_sweep.sweep.yaml")).unwrap();
        let sweep_hash = av_sweep::verify_sweep_hash(&sweep).expect("the committed sweep hash verifies");
        let drm = av_kernel::drm::schema::parse_drm_yaml(&drms("leo_orbital_native_sweep.drm.yaml")).unwrap();
        av_kernel::drm::hash::verify_drm_hash(&drm).expect("the committed DRM hash verifies");
        let sos = av_kernel::drm::schema::parse_sos_yaml(&drms("leo_orbital_native_sweep.sos.yaml")).unwrap();
        let sys = av_kernel::drm::schema::parse_system_definition_yaml(&drms("leo_1day_orbital_native.system.yaml")).unwrap();
        let systems = BTreeMap::from([(sys.id.clone(), sys)]);
        let points = av_sweep::expand_grid(&sweep).unwrap();
        points.iter().flat_map(|p| (0..sweep.monte_carlo_draws).map(move |d| (p.point_index, d))).map(|(p, d)| av_sweep::sample_config(&sweep, &sweep_hash, &drm, &sos, &systems, p, d).unwrap()).collect()
    }

    fn products(run_id: &str, value: f64) -> Vec<u8> {
        let score = pb::ScoreResult { name: "s".to_string(), value, unit: pb::Unit::Meter as i32, passed: None };
        pb::RunProducts { run_id: run_id.to_string(), scores: BTreeMap::from([("s".to_string(), score)]), ..Default::default() }.encode_to_vec()
    }

    /// A backend that records every spec it was asked to run and answers from a closure.
    struct Fake<F: Fn(&pb::JobSpec, &[JobInput]) -> JobOutcome + Sync> {
        calls: Mutex<Vec<(pb::JobSpec, Vec<JobInput>)>>,
        answer: F,
    }
    impl<F: Fn(&pb::JobSpec, &[JobInput]) -> JobOutcome + Sync> Fake<F> {
        fn new(answer: F) -> Self {
            Fake { calls: Mutex::new(Vec::new()), answer }
        }
    }
    impl<F: Fn(&pb::JobSpec, &[JobInput]) -> JobOutcome + Sync> JobBackend for Fake<F> {
        fn run(&self, spec: &pb::JobSpec, inputs: &[JobInput]) -> JobOutcome {
            self.calls.lock().unwrap().push((spec.clone(), inputs.to_vec()));
            (self.answer)(spec, inputs)
        }
    }
    fn ok_outcome(spec: &pb::JobSpec) -> JobOutcome {
        JobOutcome { result: Ok(vec![JobOutput { bytes: products(&spec.job_id, 42.0), media_type: "application/octet-stream".into(), manifest: false }]), log: Some("a log line\n".into()), placement: Some("pod fake".into()), warning: None }
    }

    #[test]
    fn the_sample_binary_path_is_the_one_the_image_copies_it_to() {
        let dockerfile = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../services/sweep/Dockerfile")).expect("services/sweep/Dockerfile");
        assert!(dockerfile.lines().any(|l| l.starts_with("COPY ") && l.trim_end().ends_with(SAMPLE_BINARY)), "the Dockerfile must COPY the binary to {SAMPLE_BINARY}");
    }

    #[test]
    fn the_command_is_the_child_command_line_at_the_job_mount_points() {
        let cmd = sample_command("run1", ExecutionErrorMode::Sampled, 2);
        assert_eq!(
            cmd,
            [
                "/usr/local/bin/av-sweep", "--run-sample", "--drm-pb", "/av-job/in/000", "--sos-pb", "/av-job/in/001", "--system-pb", "/av-job/in/002", "--system-pb", "/av-job/in/003", "--run-id", "run1", "--error-mode",
                "sampled", "--out", "/av-job/out/run_products.pb"
            ]
        );
        assert_eq!(sample_command("r", ExecutionErrorMode::Nominal, 1)[11], "nominal");
    }

    #[test]
    fn a_job_spec_pins_the_image_by_digest_and_its_inputs_by_their_own_hashes() {
        let cfg = &leo_configs()[0];
        let dir = temp_dir("spec");
        let paths = write_sample_inputs(&dir, cfg).unwrap();
        let (spec, inputs) = prepare_job(cfg, &paths, "leo_p0_d0", ExecutionErrorMode::Sampled, &image()).expect("prepared");
        assert_eq!((spec.job_id.as_str(), spec.kind.as_str()), ("leo_p0_d0", SAMPLE_JOB_KIND));
        assert_eq!(spec.executor, pb::JobExecutorKind::Container as i32);
        assert_eq!((spec.container_image.as_str(), spec.container_image_digest.as_str()), ("av-sweep-sample", IMG_DIGEST));
        assert_eq!(spec.command, sample_command("leo_p0_d0", ExecutionErrorMode::Sampled, 1));
        // drm, sos, then the one system -- in that order, each recorded under the hash of its bytes.
        let names: Vec<&str> = spec.inputs.iter().map(|a| a.uri.rsplit('/').next().unwrap()).collect();
        assert_eq!(names, ["drm.pb", "sos.pb", "sys_leo_orbital_native_sys.pb"]);
        for (asset, input) in spec.inputs.iter().zip(&inputs) {
            assert_eq!(asset.sha256, sha256_hex(&input.bytes));
            assert_eq!(asset.size_bytes as usize, input.bytes.len());
            assert_eq!(&input.asset, asset);
        }
        assert_eq!(inputs[0].bytes, std::fs::read(&paths.drm_pb).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The perturbation the brief names: one sample's input differs from what its AssetRef says.
    /// The sample is recorded as failed with the typed kind and the reason, and the backend is
    /// never asked to run anything for it (no job, no pod, no container).
    #[test]
    fn an_input_that_differs_from_its_recorded_hash_fails_typed_and_no_job_is_created() {
        let cfgs = leo_configs();
        let dir = temp_dir("tamper");
        let paths = write_sample_inputs(&dir, &cfgs[0]).unwrap();
        let mut sos = std::fs::read(&paths.sos_pb).unwrap();
        *sos.last_mut().unwrap() ^= 0x01; // one flipped bit between write and dispatch
        std::fs::write(&paths.sos_pb, sos).unwrap();

        let prepared = prepare_job(&cfgs[0], &paths, "leo_p0_d0", ExecutionErrorMode::Sampled, &image());
        let f = prepared.as_ref().expect_err("a tampered input must be refused");
        assert_eq!(f.kind, pb::JobFailureKind::InputHashMismatch as i32, "{f:?}");
        assert!(f.detail.contains("sos.pb") && f.detail.contains("does not match") && f.detail.contains("no job was created"), "{}", f.detail);

        let backend = Fake::new(|spec, _| ok_outcome(spec));
        let sample = run_one(&backend, SampleRun::new(&cfgs[0], "leo_p0_d0", &dir), prepared);
        assert!(backend.calls.lock().unwrap().is_empty(), "the backend must never be asked to run a sample whose input failed verification");
        assert!(sample.scores.is_empty() && sample.products_uri.is_empty());
        assert!(sample.error.contains("JOB_FAILURE_KIND_INPUT_HASH_MISMATCH") && sample.error.contains("does not match"), "{}", sample.error);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_input_is_a_typed_failure_too() {
        let cfgs = leo_configs();
        let dir = temp_dir("missing");
        let paths = write_sample_inputs(&dir, &cfgs[0]).unwrap();
        std::fs::remove_file(&paths.drm_pb).unwrap();
        let f = prepare_job(&cfgs[0], &paths, "r", ExecutionErrorMode::Nominal, &image()).expect_err("refused");
        assert_eq!(f.kind, pb::JobFailureKind::InputMissing as i32);
        assert!(f.detail.contains("drm.pb"), "{}", f.detail);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_succeeded_job_writes_run_products_and_stderr_where_a_child_would_and_finalizes_the_same_way() {
        let cfgs = leo_configs();
        let dir = temp_dir("ok");
        let paths = write_sample_inputs(&dir, &cfgs[0]).unwrap();
        let backend = Fake::new(|spec, _| ok_outcome(spec));
        let prepared = prepare_job(&cfgs[0], &paths, "leo_p0_d0", ExecutionErrorMode::Sampled, &image());
        let sample = run_one(&backend, SampleRun::new(&cfgs[0], "leo_p0_d0", &dir), prepared);
        assert_eq!(sample.error, "");
        assert_eq!(sample.scores["s"].value, 42.0);
        assert_eq!((sample.point_index, sample.draw_index, sample.run_id.as_str()), (0, 0, "leo_p0_d0"));
        assert_eq!(sample.config_hash, cfgs[0].config_hash);
        assert_eq!(sample.seeds, cfgs[0].seeds);
        assert_eq!(sample.axis_values, cfgs[0].axis_values);
        assert_eq!(sample.products_uri, std::fs::canonicalize(&dir).unwrap().display().to_string());
        assert_eq!(pb::RunProducts::decode(std::fs::read(dir.join("run_products.pb")).unwrap().as_slice()).unwrap().run_id, "leo_p0_d0");
        assert_eq!(std::fs::read_to_string(dir.join("stderr.txt")).unwrap(), "a log line\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_job_is_a_failed_sample_naming_its_kind_exit_code_detail_and_log_and_is_run_once() {
        let cfgs = leo_configs();
        let dir = temp_dir("failed");
        let paths = write_sample_inputs(&dir, &cfgs[0]).unwrap();
        let backend = Fake::new(|spec, _| JobOutcome {
            result: Err(pb::JobFailure { kind: pb::JobFailureKind::NonzeroExit as i32, detail: format!("K8S executor: job {:?}: the container was killed (exit 137); pod(s) [\"p1\"]; log tail: \"last words\"", spec.job_id), exit_code: 137 }),
            log: Some("the pod's own log\nlast words\n".into()),
            placement: None,
            warning: None,
        });
        let prepared = prepare_job(&cfgs[0], &paths, "leo_p0_d0", ExecutionErrorMode::Sampled, &image());
        let sample = run_one(&backend, SampleRun::new(&cfgs[0], "leo_p0_d0", &dir), prepared);
        assert_eq!(backend.calls.lock().unwrap().len(), 1, "a failed job is never retried");
        assert!(sample.scores.is_empty() && sample.products_uri.is_empty());
        for want in ["JOB_FAILURE_KIND_NONZERO_EXIT", "exit code 137", "was killed", "pod(s)", "last words", "the pod's own log"] {
            assert!(sample.error.contains(want), "error lacks {want:?}: {}", sample.error);
        }
        assert!(!dir.join("run_products.pb").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_job_that_succeeds_with_zero_or_two_outputs_or_garbage_is_a_failed_sample() {
        let cfgs = leo_configs();
        for (name, outputs, want) in [
            ("none", vec![], "produced 0 output file(s)"),
            ("two", vec![JobOutput { bytes: vec![1], media_type: String::new(), manifest: false }, JobOutput { bytes: vec![2], media_type: String::new(), manifest: false }], "produced 2 output file(s)"),
            ("garbage", vec![JobOutput { bytes: vec![0xff, 0xff, 0xff, b'x'], media_type: String::new(), manifest: false }], "did not decode as altavista.v1.RunProducts"),
        ] {
            let dir = temp_dir(&format!("out-{name}"));
            let paths = write_sample_inputs(&dir, &cfgs[0]).unwrap();
            let backend = Fake::new(move |_, _| JobOutcome { result: Ok(outputs.clone()), log: None, placement: None, warning: None });
            let prepared = prepare_job(&cfgs[0], &paths, "r", ExecutionErrorMode::Sampled, &image());
            let sample = run_one(&backend, SampleRun::new(&cfgs[0], "r", &dir), prepared);
            assert!(!sample.error.is_empty() && sample.error.contains(want), "{name}: {}", sample.error);
            assert!(sample.scores.is_empty());
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// Up to `--workers` jobs at once, from threads over one shared backend: with 3 workers and 12
    /// samples of ~80 ms each, the backend sees exactly 3 in flight at its peak (never 4, never 1),
    /// runs every sample exactly once, and every sample comes back.
    #[test]
    fn run_jobs_runs_up_to_workers_jobs_at_once_each_sample_once() {
        let cfgs = leo_configs();
        assert_eq!(cfgs.len(), 12);
        let in_flight = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let backend = Fake::new(|spec, _| {
            let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(now, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(80));
            in_flight.fetch_sub(1, Ordering::SeqCst);
            ok_outcome(spec)
        });
        let dir = temp_dir("pool");
        let samples = run_jobs(&backend, 3, cfgs.into_iter().collect(), &dir, "leo", ExecutionErrorMode::Sampled, &image()).expect("runs");
        assert_eq!(samples.len(), 12);
        assert!(samples.iter().all(|s| s.error.is_empty()), "{:?}", samples.iter().map(|s| &s.error).collect::<Vec<_>>());
        assert_eq!(peak.load(Ordering::SeqCst), 3, "peak jobs in flight");
        let mut ids: Vec<String> = backend.calls.lock().unwrap().iter().map(|(s, _)| s.job_id.clone()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 12, "every sample ran exactly once");
        assert_eq!(ids[0], "leo_p0_d0");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn describe_failure_names_an_unknown_kind_rather_than_hiding_it() {
        let text = describe_failure(&pb::JobFailure { kind: 999, detail: "d".into(), exit_code: 3 });
        assert!(text.contains("unknown kind 999") && text.contains("exit code 3") && text.ends_with(": d"), "{text}");
    }

    /// No Docker: a job refused before any container exists has no log, and `stderr.txt` says why
    /// instead of being left empty (the container log itself is proved against Docker in
    /// `tests/k8s_sweep.rs` and `av-jobs`' `tests/container.rs`).
    #[test]
    fn a_container_job_with_no_log_says_why_in_stderr_txt() {
        let backend = ContainerBackend(ContainerExecutor::new(std::env::temp_dir(), BTreeMap::new()));
        let outcome = backend.run(&pb::JobSpec { job_id: "j".into(), ..Default::default() }, &[]);
        assert!(outcome.result.is_err());
        let log = outcome.log.expect("a reason, never None");
        assert!(log.starts_with("<no container log: no container was created") && log.ends_with(">\n"), "{log}");
        assert_eq!(outcome.placement, None);
    }
}
