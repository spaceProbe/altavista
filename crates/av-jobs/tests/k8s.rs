//! Question 245, task heavy-ct-2a: the real-cluster proof that `av_jobs::k8s::K8sJobExecutor` runs
//! a job as a Kubernetes Job -- one pod, never retried -- on this host's k3s (Colima, one node,
//! Docker runtime), and that every way a pod can end comes back as a typed, named failure.
//!
//! # Gating, locking, cleanup
//!
//! Every test is gated on (a) Docker up and the pinned probe image present under its recorded
//! digest (`CONTAINER_EXECUTOR_IMAGE_DIGEST.md`, the same record `tests/container.rs` uses; the
//! pods run `alpine@<digest>` with `imagePullPolicy: Never`, nothing here pulls) and (b) the
//! `colima` kubeconfig context loading and its API server answering. A gate failure is a visible
//! `SKIPPED` line (a hard failure under `AV_REQUIRE_DOCKER_TESTS=1`), never a silent pass. Pods are
//! containers on the same Docker the other docker-gated tests use, so each test takes
//! `lock_docker_tests` (this file never runs under `with_docker_lock.py`: it takes the lock itself).
//! Each test creates its own namespace `av-heavy-ct-<run id>` labelled `av.test=1` and deletes it in
//! a guard; pods carry `av.test=1`/`av.test.run_id` too. Nothing is pruned that this test did not
//! create.
//!
//! # The lock is held for the whole life of every pod (cross-track hazard)
//!
//! The pods' `k8s_POD_*` sandboxes on the shared Docker carry `av.test=1`, and every native
//! docker-gated test runs `prune_stale_test_resources` (removing every container with that label)
//! under `lock_docker_tests`. A pod outliving our hold on the lock could be deleted mid-run by a
//! native test. So the label stays (it lets a crashed run's leftovers be swept) and the lock covers
//! the pods' whole life: `setup` takes it before anything exists and `Env` releases it last, after
//! the namespace guard has deleted the namespace and waited until the API and Docker show no pod or
//! container of the run (`run_containers`). `a_test_pod_exists_only_while_the_docker_lock_is_held`
//! proves it: containers of the run exist during, none after the lock is released. Measured cost: a
//! test holds the lock for its whole duration (about 10 to 60 s each, 7 minutes for the file), and
//! teardown adds a few seconds because Docker removes a deleted pod's sandbox after the API has
//! already forgotten the pod.
//!
//! # What "several pods at once" means here
//!
//! `several_pods_run_at_once_on_the_one_node` runs four jobs from four threads against ONE node.
//! It proves the executor and the cluster run several pods concurrently, from the pods' own
//! `status.startTime` and the container's `startedAt`/`finishedAt` that the kubelet recorded. It
//! says nothing about several hosts: the `hostPath` data plane is single-node by construction
//! (`av_jobs::k8s` module doc).
//!
//! # Fixtures
//!
//! With `AV_K8S_FIXTURE_DIR=<dir>` set, each scenario also writes the last Job and Pod JSON it
//! observed (minus `managedFields`) to `<dir>/<scenario>.job.json` / `.pod.json`; the committed
//! files under `tests/fixtures/k8s/` were recorded that way, and `src/k8s/status.rs`'s fixture tests
//! replay them.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use av_cdm::pb;
use av_jobs::k8s::manifest::{build_job, label_value, JobParams};
use av_jobs::k8s::status::{self, K8sError, PodTrack};
use av_jobs::k8s::{ApiError, K8sApi, K8sConfig, K8sJobExecutor, Kubeconfig, RunReport};
use av_jobs::runner::JobInput;
use av_lockstep::docker::{announce_gate_skip, lock_docker_tests, recorded_digest_gate, test_run_id, DockerGateReason, DockerTestLock};
use serde_json::Value;

const CONTAINER_IMAGE: &str = "alpine";

// ------------------------------------------------------------------------------------------
// Gating (copied in shape from tests/container.rs).
// ------------------------------------------------------------------------------------------

fn fenced_block_after(text: &str, marker: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let marker_idx = lines.iter().position(|l| l.contains(marker))?;
    let fence_start = marker_idx + lines[marker_idx..].iter().position(|l| l.trim() == "```")?;
    let fence_end = fence_start + 1 + lines[fence_start + 1..].iter().position(|l| l.trim() == "```")?;
    let content = lines[fence_start + 1..fence_end].join("\n").trim().to_string();
    if content.is_empty() {
        None
    } else {
        Some(content)
    }
}

/// `(registry reference, recorded id)` from `CONTAINER_EXECUTOR_IMAGE_DIGEST.md`.
fn parse_image_digest_md() -> (String, String) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/CONTAINER_EXECUTOR_IMAGE_DIGEST.md");
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("could not read {path:?}: {e}"));
    let image_ref = fenced_block_after(&text, "Registry reference").unwrap_or_else(|| panic!("no 'Registry reference' block in {path:?}"));
    let recorded_id = fenced_block_after(&text, "docker image inspect").unwrap_or_else(|| panic!("no 'docker image inspect' block in {path:?}"));
    assert_eq!(
        image_ref.rsplit_once('@').map(|(name, digest)| (name == CONTAINER_IMAGE, digest)),
        Some((true, recorded_id.as_str())),
        "the record must be `{CONTAINER_IMAGE}@<digest>` with <digest> equal to the recorded id; got {image_ref:?} / {recorded_id:?}"
    );
    (image_ref, recorded_id)
}

/// Where per-run scratch directories go: under `$HOME` (the node mounts `$HOME` at the same path),
/// in this repository's already-ignored `.av-test-tmp/`.
fn scratch_root() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.av-test-tmp/av-jobs-k8s-test");
    fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("could not create scratch root {dir:?}: {e}"));
    dir
}

/// Deletes the test's namespace, waits (bounded) for it to be gone from the API, then waits for every
/// Docker container of the run (workloads and `k8s_POD_*` sandboxes) to be gone, force-removing any
/// straggler that carries THIS run's `av.test.run_id` (never anything else).
struct NamespaceGuard {
    api: K8sApi,
    name: String,
    run_id: String,
}

impl Drop for NamespaceGuard {
    fn drop(&mut self) {
        let _ = self.api.delete_namespace(&self.name);
        let end = Instant::now() + Duration::from_secs(90);
        let mut gone = false;
        while Instant::now() < end {
            if matches!(self.api.get_namespace(&self.name), Err(e) if e.is_not_found()) {
                gone = true;
                break;
            }
            thread::sleep(Duration::from_millis(500));
        }
        if !gone {
            eprintln!("WARNING: namespace {} still present 90 s after deletion", self.name);
        }
        let leftovers = || run_containers(&self.run_id, &self.name);
        let end = Instant::now() + Duration::from_secs(60);
        while Instant::now() < end && !leftovers().is_empty() {
            thread::sleep(Duration::from_millis(500));
        }
        for name in leftovers() {
            eprintln!("WARNING: container {name} of run {} outlived its namespace; removing it", self.run_id);
            let _ = Command::new("docker").args(["rm", "-f", &name]).output();
        }
    }
}

/// The network posture a test's executor makes an explicit choice of (`K8sJobExecutor::new` refuses a
/// config that makes neither): the gate (production), or the documented opt-in to the start-up window.
#[derive(Clone, Copy)]
enum Net {
    Gate,
    AcceptWindow,
}

struct Env {
    run_id: String,
    namespace: String,
    recorded_id: String,
    kubeconfig: Kubeconfig,
    api: K8sApi,
    labels: BTreeMap<String, String>,
    /// Dropped first (fields drop in declaration order) ...
    _ns: NamespaceGuard,
    /// ... and the lock last: no pod of this run outlives it. See `setup`.
    _lock: DockerTestLock,
}

impl Env {
    fn config(&self, net: Net) -> K8sConfig {
        let mut cfg = K8sConfig::new(self.kubeconfig.clone(), self.namespace.clone(), scratch_root());
        cfg.extra_labels = self.labels.clone();
        cfg.deadline = Duration::from_secs(90);
        match net {
            // The production posture: the workload starts only once the pod's egress is demonstrably closed.
            Net::Gate => cfg.network_gate = Some(av_jobs::k8s::NetworkGate { image: CONTAINER_IMAGE.to_string(), image_digest: self.recorded_id.clone(), within: Duration::from_secs(20) }),
            // The explicit, documented opt-in: the job's first ~1-2 s run with network (measured).
            Net::AcceptWindow => cfg.accept_startup_network_window = true,
        }
        cfg.margin = Duration::from_secs(30);
        // Small requests so four pods fit this node together with k3s's own.
        cfg.resources = av_jobs::k8s::Resources { cpu_request: "20m".into(), cpu_limit: "250m".into(), memory_request: "8Mi".into(), memory_limit: "64Mi".into() };
        cfg
    }

    fn executor(&self, net: Net) -> K8sJobExecutor {
        K8sJobExecutor::new(self.config(net)).unwrap_or_else(|e| panic!("K8sJobExecutor::new: {e}"))
    }

    fn spec(&self, name: &str, command: &[&str]) -> pb::JobSpec {
        pb::JobSpec {
            job_id: format!("{name}-{}", self.run_id),
            kind: "k8s-probe".to_string(),
            label: Some(pb::Label { marking: "CUI".to_string(), caveats: vec![] }),
            requested_tai_ns: 1,
            executor: pb::JobExecutorKind::Container as i32,
            command: command.iter().map(|s| s.to_string()).collect(),
            container_image: CONTAINER_IMAGE.to_string(),
            container_image_digest: self.recorded_id.clone(),
            ..Default::default()
        }
    }

    fn pods_of(&self, job_id: &str) -> Vec<Value> {
        self.api.list_pods(&self.namespace, &format!("av.job.id={}", label_value(job_id))).unwrap()
    }

    /// The Job and its pod are really gone, asked of the API (not of the report), and so is the
    /// scratch directory.
    fn assert_torn_down(&self, report: &RunReport) {
        assert!(report.cleanup.is_ok(), "cleanup failed: {:?}", report.cleanup);
        let job = self.api.get_job(&self.namespace, &report.job_name);
        assert!(matches!(&job, Err(e) if e.is_not_found()), "Job {} still exists: {job:?}", report.job_name);
        let pods = self.pods_of(&report.job_id);
        assert!(pods.is_empty(), "pods of {} still exist: {:?}", report.job_id, pods.iter().map(|p| p["metadata"]["name"].clone()).collect::<Vec<_>>());
        // The scratch directory is named exactly like the Job.
        let leftovers: Vec<_> = fs::read_dir(scratch_root()).unwrap().filter_map(|e| e.ok()).filter(|e| e.file_name().to_string_lossy() == report.job_name.as_str()).collect();
        assert!(leftovers.is_empty(), "scratch directory left behind: {leftovers:?}");
    }
}

/// Names of every container on the host's Docker (running or not, sandbox `k8s_POD_*` included)
/// carrying `label` (`key` or `key=value`).
fn docker_containers_with(label: &str) -> Vec<String> {
    let out = Command::new("docker").args(["ps", "-a", "--filter", &format!("label={label}"), "--format", "{{.Names}}"]).output().expect("docker ps");
    String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).filter(|l| !l.is_empty()).collect()
}

/// Every container of one run: the pause sandboxes carry the pod's labels (`av.test.run_id`), the
/// workload and init containers only Kubernetes' own (`io.kubernetes.pod.namespace`), so both are
/// asked for.
fn run_containers(run_id: &str, namespace: &str) -> Vec<String> {
    let mut all = docker_containers_with(&format!("av.test.run_id={run_id}"));
    all.extend(docker_containers_with(&format!("io.kubernetes.pod.namespace={namespace}")));
    all.sort();
    all.dedup();
    all
}

/// `None` (after a visible SKIPPED line) when Docker, the image or the cluster is not available.
///
/// # The lock contract (cross-track hazard)
///
/// A pod's `k8s_POD_*` pause sandbox on the shared Docker carries the pod's labels, `av.test=1`
/// among them (measured: the workload and init containers carry only Kubernetes' own labels, but
/// pruning a sandbox kills the pod). The native team's `prune_stale_test_resources`
/// runs, under `lock_docker_tests`, in every native docker-gated test and removes every container
/// with that label. So a pod of ours that outlived our hold on the lock could be deleted from under
/// us by a native test (and a native test would be slowed or confused by ours). The contract:
/// **the label stays (it is what lets a crashed run's leftovers be swept), and the lock is held for
/// the whole life of every pod** -- from before the first Job is created until the namespace is
/// deleted and its containers are gone. `Env` enforces it structurally: `setup` takes the lock
/// first, `Env` owns it as its LAST field, and fields drop in declaration order, so the
/// namespace guard (delete the namespace, wait for it to be gone, wait for the run's containers to
/// be gone from Docker, force-remove any straggler of this run id) always runs before the lock is
/// released, on success or on a panic.
fn setup(test_name: &str) -> Option<Env> {
    let lock = lock_docker_tests();
    let (image_ref, recorded_id) = parse_image_digest_md();
    if let Err(reason) = recorded_digest_gate(&image_ref, &recorded_id) {
        announce_gate_skip(test_name, &reason);
        return None;
    }
    let kubeconfig = match Kubeconfig::load_default(Some("colima")) {
        Ok(k) => k,
        Err(e) => {
            announce_gate_skip(test_name, &DockerGateReason::PrerequisiteUnavailable { what: "the `colima` kubeconfig context".into(), hint: e.to_string() });
            return None;
        }
    };
    let api = match K8sApi::new(&kubeconfig, av_http1::Timeouts::uniform(Duration::from_secs(10))) {
        Ok(a) => a,
        Err(e) => {
            announce_gate_skip(test_name, &DockerGateReason::PrerequisiteUnavailable { what: "a Kubernetes API client".into(), hint: e.to_string() });
            return None;
        }
    };
    if let Err(e) = api.list_nodes() {
        announce_gate_skip(test_name, &DockerGateReason::PrerequisiteUnavailable { what: "the k3s API server".into(), hint: e.to_string() });
        return None;
    }
    let run_id = test_run_id();
    let namespace = format!("av-heavy-ct-{run_id}");
    let labels = BTreeMap::from([("av.test".to_string(), "1".to_string()), ("av.test.run_id".to_string(), run_id.clone())]);
    // Before anything of this run exists: none of its containers on Docker, no namespace of its name.
    assert!(docker_containers_with(&format!("av.test.run_id={run_id}")).is_empty());
    assert!(matches!(api.get_namespace(&namespace), Err(e) if e.is_not_found()));
    // Any k8s container labelled av.test at this point (the lock is ours) is a leftover of an earlier
    // run: reported, not touched (it may not be ours; the native prune sweeps it).
    let stale: Vec<String> = docker_containers_with("av.test").into_iter().filter(|n| n.starts_with("k8s_")).collect();
    if !stale.is_empty() {
        eprintln!("NOTE: {} k8s container(s) labelled av.test already present before this run: {stale:?}", stale.len());
    }
    api.create_namespace(&namespace, &labels).unwrap_or_else(|e| panic!("creating namespace {namespace}: {e}"));
    let guard = NamespaceGuard { api: api.clone(), name: namespace.clone(), run_id: run_id.clone() };
    Some(Env { run_id, namespace, recorded_id, kubeconfig, api, labels, _ns: guard, _lock: lock })
}

macro_rules! env_or_skip {
    ($name:expr) => {
        match setup($name) {
            Some(env) => env,
            None => return,
        }
    };
}

// ------------------------------------------------------------------------------------------
// Helpers: pod watching, killing, time, fixtures.
// ------------------------------------------------------------------------------------------

/// Polls the pods of a job every 100 ms from a separate thread and records every pod name it ever
/// sees: independent evidence of how many pods existed, whatever the executor reports.
struct PodWatcher {
    stop: Arc<AtomicBool>,
    handle: thread::JoinHandle<BTreeSet<String>>,
}

impl PodWatcher {
    fn start(api: K8sApi, namespace: String, job_id: &str) -> PodWatcher {
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let selector = format!("av.job.id={}", label_value(job_id));
        let handle = thread::spawn(move || {
            let mut seen = BTreeSet::new();
            while !stop2.load(Ordering::Relaxed) {
                if let Ok(pods) = api.list_pods(&namespace, &selector) {
                    seen.extend(pods.iter().filter_map(|p| p["metadata"]["name"].as_str().map(str::to_string)));
                }
                thread::sleep(Duration::from_millis(100));
            }
            seen
        });
        PodWatcher { stop, handle }
    }

    fn finish(self) -> BTreeSet<String> {
        self.stop.store(true, Ordering::Relaxed);
        self.handle.join().unwrap()
    }
}

/// Waits (polling the API) until the job's pod has a running container; returns the pod name.
fn wait_for_running_pod(api: &K8sApi, namespace: &str, job_id: &str, within: Duration) -> String {
    let selector = format!("av.job.id={}", label_value(job_id));
    let end = Instant::now() + within;
    loop {
        for pod in api.list_pods(namespace, &selector).unwrap_or_default() {
            if pod["status"]["containerStatuses"][0]["state"]["running"].is_object() {
                return pod["metadata"]["name"].as_str().unwrap().to_string();
            }
        }
        assert!(Instant::now() < end, "no running pod for {job_id} within {within:?}");
        thread::sleep(Duration::from_millis(100));
    }
}

fn docker(args: &[&str]) -> String {
    let out = Command::new("docker").args(args).output().unwrap_or_else(|e| panic!("could not launch docker {args:?}: {e}"));
    assert!(out.status.success(), "docker {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Epoch seconds of an RFC 3339 `YYYY-MM-DDTHH:MM:SSZ` timestamp.
fn epoch(ts: &str) -> i64 {
    let n = |r: std::ops::Range<usize>| ts[r].parse::<i64>().unwrap_or_else(|_| panic!("bad timestamp {ts:?}"));
    assert!(ts.len() >= 20 && ts.ends_with('Z'), "unexpected timestamp shape {ts:?}");
    days_from_civil(n(0..4), n(5..7), n(8..10)) * 86400 + n(11..13) * 3600 + n(14..16) * 60 + n(17..19)
}

/// Writes the last Job/Pod JSON of a scenario when `AV_K8S_FIXTURE_DIR` is set.
fn capture(scenario: &str, report: &RunReport) {
    let Ok(dir) = std::env::var("AV_K8S_FIXTURE_DIR") else { return };
    fs::create_dir_all(&dir).unwrap();
    for (suffix, value) in [("job", &report.final_job), ("pod", &report.final_pod)] {
        if let Some(v) = value {
            let mut v = v.clone();
            if let Some(meta) = v["metadata"].as_object_mut() {
                meta.remove("managedFields");
            }
            fs::write(Path::new(&dir).join(format!("{scenario}.{suffix}.json")), serde_json::to_string_pretty(&v).unwrap() + "\n").unwrap();
        }
    }
}

fn failure_of(report: &RunReport) -> pb::JobFailure {
    report.job_failure().expect("expected a failure, got outputs")
}

// ------------------------------------------------------------------------------------------
// The proofs.
// ------------------------------------------------------------------------------------------

#[test]
fn a_real_job_reads_its_inputs_writes_outputs_and_leaves_nothing_behind() {
    let env = env_or_skip!("a_real_job_reads_its_inputs_writes_outputs_and_leaves_nothing_behind");
    let executor = env.executor(Net::Gate);
    assert_eq!(executor.node_name(), "colima");

    let inputs = vec![
        JobInput { asset: pb::AssetRef::default(), bytes: b"hello k8s\n".to_vec() },
        JobInput { asset: pb::AssetRef::default(), bytes: b"second input\n".to_vec() },
    ];
    // Reads both inputs, writes two outputs (names chosen so sorted order differs from creation
    // order), and prints what the kernel says about the container's own hardening.
    let script = "echo \"uid=$(id -u) gid=$(id -g)\"; grep -E 'NoNewPrivs|Seccomp:|CapEff' /proc/self/status; \
                  (echo x > /rootprobe) 2>/dev/null; echo root_write_exit=$?; \
                  (echo x > /av-job/in/probe) 2>/dev/null; echo in_write_exit=$?; \
                  tr a-z A-Z < /av-job/in/000 > /av-job/out/zz-upper; cat /av-job/in/001 /av-job/in/000 > /av-job/out/aa-cat; \
                  echo stderr-line >&2; echo done";
    let spec = env.spec("real", &["/bin/sh", "-c", script]);
    let report = executor.run(&spec, &inputs);
    capture("success", &report);

    let outputs = report.result.as_ref().unwrap_or_else(|e| panic!("job failed: {e}; log: {:?}", report.log));
    assert_eq!(outputs.len(), 2, "{outputs:?}");
    assert_eq!(outputs[0].bytes, b"second input\nhello k8s\n", "aa-cat sorts first");
    assert_eq!(outputs[1].bytes, b"HELLO K8S\n", "zz-upper sorts second");

    let log = report.log.as_ref().expect("log captured");
    assert!(!log.truncated, "{log:?}");
    for want in ["uid=10001 gid=10001", "NoNewPrivs:\t1", "Seccomp:\t2", "CapEff:\t0000000000000000", "stderr-line", "done"] {
        assert!(log.text.contains(want), "log lacks {want:?}:\n{}", log.text);
    }
    assert!(!log.text.contains("root_write_exit=0") && log.text.contains("root_write_exit="), "root fs must be read-only:\n{}", log.text);
    assert!(!log.text.contains("in_write_exit=0") && log.text.contains("in_write_exit="), "input mount must be read-only:\n{}", log.text);
    println!("\n--- k8s hardening proof (from the pod's own log) ---\n{}---", log.text);

    // The pod the cluster actually ran: one pod, bound to the node, pinned image never pulled.
    assert_eq!(report.pods.len(), 1, "{:?}", report.pods);
    let pod = report.final_pod.as_ref().unwrap();
    assert_eq!(pod["spec"]["nodeName"], "colima");
    assert_eq!(pod["spec"]["containers"][0]["image"].as_str().unwrap(), format!("alpine@{}", env.recorded_id));
    assert_eq!(pod["spec"]["containers"][0]["imagePullPolicy"], "Never");
    assert!(pod["status"]["containerStatuses"][0]["imageID"].as_str().unwrap().contains(&env.recorded_id), "{}", pod["status"]["containerStatuses"][0]["imageID"]);
    assert_eq!(pod["metadata"]["labels"]["av.job"], "1");
    assert_eq!(pod["metadata"]["labels"]["av.test.run_id"], env.run_id.as_str());
    assert_eq!(pod["status"]["phase"], "Succeeded");
    let job = report.final_job.as_ref().unwrap();
    assert_eq!(job["spec"]["backoffLimit"], 0);
    assert_eq!(job["status"]["succeeded"], 1);
    assert!(job["status"]["failed"].is_null() || job["status"]["failed"] == 0, "{}", job["status"]);

    env.assert_torn_down(&report);
}

#[test]
fn several_pods_run_at_once_on_the_one_node() {
    let env = env_or_skip!("several_pods_run_at_once_on_the_one_node");
    let executor = env.executor(Net::Gate);
    const JOBS: usize = 4;
    // Each pod sleeps long enough that, run one after another, they could not overlap.
    let script = "echo started-$$; sleep 8; echo finished; echo ok > /av-job/out/result";
    let begun = Instant::now();
    let reports: Vec<RunReport> = thread::scope(|s| {
        let handles: Vec<_> = (0..JOBS)
            .map(|i| {
                let spec = env.spec(&format!("par{i}"), &["/bin/sh", "-c", script]);
                let executor = &executor;
                s.spawn(move || executor.run(&spec, &[]))
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let wall = begun.elapsed();

    let mut intervals = Vec::new();
    for (i, r) in reports.iter().enumerate() {
        let outputs = r.result.as_ref().unwrap_or_else(|e| panic!("job {i} failed: {e}"));
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].bytes, b"ok\n");
        assert_eq!(r.pods.len(), 1);
        let t = r.pod_times.as_ref().expect("pod times");
        let (start, fin) = (epoch(t.container_started_at.as_ref().unwrap()), epoch(t.container_finished_at.as_ref().unwrap()));
        println!("job {i}: pod {} startTime={} container startedAt={} finishedAt={} (ran {} s)", r.pods[0], t.pod_start_time.as_deref().unwrap(), t.container_started_at.as_deref().unwrap(), t.container_finished_at.as_deref().unwrap(), fin - start);
        assert!(fin - start >= 8, "job {i} ran {} s", fin - start);
        intervals.push((start, fin));
        env.assert_torn_down(r);
    }
    let latest_start = intervals.iter().map(|i| i.0).max().unwrap();
    let earliest_finish = intervals.iter().map(|i| i.1).min().unwrap();
    println!("latest container start {latest_start}, earliest container finish {earliest_finish}; wall {wall:?} for {JOBS} jobs of 8 s each (serial would be >= {} s)", JOBS * 8);
    assert!(latest_start < earliest_finish, "the four containers did not all run at the same moment: {intervals:?}");
    assert!(wall < Duration::from_secs((JOBS as u64) * 8), "wall time {wall:?} is not shorter than running them one after another");
    // Four distinct pods on the one node.
    let pods: BTreeSet<_> = reports.iter().map(|r| r.pods[0].clone()).collect();
    assert_eq!(pods.len(), JOBS);
    for r in &reports {
        assert_eq!(r.final_pod.as_ref().unwrap()["spec"]["nodeName"], "colima");
    }
}

#[test]
fn a_non_zero_exit_a_missing_command_and_a_refused_image_are_typed() {
    let env = env_or_skip!("a_non_zero_exit_a_missing_command_and_a_refused_image_are_typed");
    let executor = env.executor(Net::Gate);

    // -- exit 7 ---------------------------------------------------------------------------------
    let report = executor.run(&env.spec("exit7", &["/bin/sh", "-c", "echo about-to-fail; echo some-error >&2; exit 7"]), &[]);
    capture("exit7", &report);
    let failure = failure_of(&report);
    println!("exit 7 -> {failure:?}");
    assert_eq!(failure.kind, pb::JobFailureKind::NonzeroExit as i32);
    assert_eq!(failure.exit_code, 7);
    assert!(failure.detail.contains("exited with status 7") && failure.detail.contains("some-error"), "{}", failure.detail);
    assert!(matches!(report.result, Err(K8sError::ExitedNonZero { code: 7, .. })));
    assert_eq!(report.pods.len(), 1);
    assert_eq!(report.final_job.as_ref().unwrap()["status"]["failed"], 1);
    env.assert_torn_down(&report);

    // -- a command that does not exist in the image -------------------------------------------------
    let report = executor.run(&env.spec("nocmd", &["/no/such/binary-av-k8s-test"]), &[]);
    capture("missing_command", &report);
    let failure = failure_of(&report);
    println!("missing command -> {failure:?}");
    assert_eq!(failure.kind, pb::JobFailureKind::ExecutorStartFailed as i32, "{failure:?}");
    assert!(matches!(&report.result, Err(K8sError::NeverStarted { .. })), "{:?}", report.result);
    assert!(failure.detail.contains("never started") && failure.detail.contains("/no/such/binary-av-k8s-test"), "{}", failure.detail);
    assert_eq!(report.pods.len(), 1);
    env.assert_torn_down(&report);

    // -- an image the digest gate refuses: no Job is ever created --------------------------------------
    let mut spec = env.spec("badimage", &["/bin/echo", "would have run"]);
    spec.container_image_digest = format!("sha256:{}", "0".repeat(64));
    let report = executor.run(&spec, &[]);
    let failure = failure_of(&report);
    println!("refused image -> {failure:?}");
    assert_eq!(failure.kind, pb::JobFailureKind::ExecutorUnavailable as i32);
    assert!(failure.detail.contains("refused rather than trusted") && failure.detail.contains(&spec.container_image_digest), "{}", failure.detail);
    assert!(report.pods.is_empty() && report.final_job.is_none(), "a refused image must not reach the cluster");
    assert!(env.pods_of(&spec.job_id).is_empty());
    assert!(matches!(env.api.get_job(&env.namespace, &report.job_name), Err(e) if e.is_not_found()));
}

#[test]
fn a_chatty_pods_log_is_capped_by_lines_and_by_bytes_and_marked() {
    let env = env_or_skip!("a_chatty_pods_log_is_capped_by_lines_and_by_bytes_and_marked");
    let script = "i=0; while [ $i -lt 3000 ]; do echo line-$i; i=$((i+1)); done; echo last-line; exit 3";

    // By lines: only the last 50 survive, and the first line says so.
    let mut cfg = env.config(Net::Gate);
    cfg.log_max_lines = 50;
    let report = K8sJobExecutor::new(cfg).unwrap().run(&env.spec("chatty1", &["/bin/sh", "-c", script]), &[]);
    let log = report.log.as_ref().unwrap_or_else(|| panic!("no log: {:?}", report.log_error));
    assert!(log.truncated, "{log:?}");
    assert!(log.text.starts_with("[av: log truncated: only the last 50 lines kept]\n"), "{}", &log.text[..80.min(log.text.len())]);
    let kept: Vec<&str> = log.text.lines().skip(1).collect();
    assert_eq!(kept.len(), 50, "{kept:?}");
    assert_eq!((kept[0], kept[48], kept[49]), ("line-2951", "line-2999", "last-line"));
    // The failure detail carries only the short tail of it.
    let failure = failure_of(&report);
    assert_eq!((failure.kind, failure.exit_code), (pb::JobFailureKind::NonzeroExit as i32, 3));
    assert!(failure.detail.contains("last-line") && !failure.detail.contains("line-2951"), "{}", failure.detail);
    env.assert_torn_down(&report);

    // By bytes: the cap holds and the marker is appended.
    let mut cfg = env.config(Net::Gate);
    cfg.log_cap_bytes = 100;
    let report = K8sJobExecutor::new(cfg).unwrap().run(&env.spec("chatty2", &["/bin/sh", "-c", script]), &[]);
    let log = report.log.as_ref().unwrap_or_else(|| panic!("no log: {:?}", report.log_error));
    assert!(log.truncated && log.text.contains("[av: log truncated at 100 bytes"), "{log:?}");
    assert!(log.text.len() < 100 + 100, "{} bytes", log.text.len());
    env.assert_torn_down(&report);
}

#[test]
fn a_job_past_its_deadline_is_a_deadline_failure() {
    let env = env_or_skip!("a_job_past_its_deadline_is_a_deadline_failure");
    let mut cfg = env.config(Net::Gate);
    // Long enough for the network gate (an init container, a second or two) to finish and the workload to start: with a shorter one the deadline can expire while the pod is still initialising, and then there is no workload log.
    cfg.deadline = Duration::from_secs(15);
    let executor = K8sJobExecutor::new(cfg).unwrap();
    let begun = Instant::now();
    let report = executor.run(&env.spec("slow", &["/bin/sh", "-c", "echo before-sleep; sleep 120"]), &[]);
    let took = begun.elapsed();
    capture("deadline", &report);
    let failure = failure_of(&report);
    println!("deadline -> {failure:?} after {took:?}");
    assert!(matches!(report.result, Err(K8sError::DeadlineExceeded { .. })), "{:?}", report.result);
    assert_eq!(failure.kind, pb::JobFailureKind::NonzeroExit as i32);
    assert!(failure.detail.contains("deadline"), "{}", failure.detail);
    assert!(took < Duration::from_secs(40), "a 15 s deadline took {took:?}");
    assert!(report.log.as_ref().is_some_and(|l| l.text.contains("before-sleep")), "the log of the overrunning pod is still captured: {:?} / {:?}", report.log, report.log_error);
    assert_eq!(report.pods.len(), 1);
    assert_eq!(report.final_job.as_ref().unwrap()["spec"]["activeDeadlineSeconds"], 15);
    env.assert_torn_down(&report);
}

#[test]
fn a_pod_whose_container_is_docker_killed_mid_run_fails_the_job_once_without_a_retry() {
    let env = env_or_skip!("a_pod_whose_container_is_docker_killed_mid_run_fails_the_job_once_without_a_retry");
    let executor = env.executor(Net::Gate);
    let spec = env.spec("dockerkill", &["/bin/sh", "-c", "echo running; sleep 120"]);
    let watcher = PodWatcher::start(env.api.clone(), env.namespace.clone(), &spec.job_id);

    let (report, killed) = thread::scope(|s| {
        let killer = s.spawn(|| {
            let pod = wait_for_running_pod(&env.api, &env.namespace, &spec.job_id, Duration::from_secs(60));
            // The kubelet's container for this pod, found by the labels the Docker runtime sets.
            let id = docker(&["ps", "-q", "--filter", &format!("label=io.kubernetes.pod.name={pod}"), "--filter", "label=io.kubernetes.container.name=job"]);
            assert!(!id.is_empty() && !id.contains('\n'), "expected exactly one container for pod {pod}, got {id:?}");
            docker(&["kill", &id]);
            (pod, id)
        });
        let report = executor.run(&spec, &[]);
        (report, killer.join().unwrap())
    });
    let seen = watcher.finish();
    capture("docker_kill", &report);
    let failure = failure_of(&report);
    println!("docker kill of container {} in pod {} -> {failure:?}\npods ever seen by an independent watcher: {seen:?}", killed.1, killed.0);

    assert!(matches!(&report.result, Err(K8sError::Killed { exit_code: 137, signal: Some(9), .. })), "{:?}", report.result);
    assert_eq!(failure.kind, pb::JobFailureKind::NonzeroExit as i32);
    assert_eq!(failure.exit_code, 137);
    assert!(failure.detail.contains("killed") && failure.detail.contains("137"), "{}", failure.detail);
    // Exactly one pod ever existed, by the executor's account and by the independent watcher's.
    assert_eq!(report.pods, vec![killed.0.clone()]);
    assert_eq!(seen, BTreeSet::from([killed.0.clone()]), "a second pod appeared: that is a Kubernetes retry");
    let job = report.final_job.as_ref().unwrap();
    assert_eq!(job["spec"]["backoffLimit"], 0);
    assert_eq!(job["status"]["failed"], 1);
    assert!(job["status"]["conditions"].as_array().unwrap().iter().any(|c| c["type"] == "Failed" && c["status"] == "True"), "{}", job["status"]);
    env.assert_torn_down(&report);
}

#[test]
fn a_pod_deleted_through_the_api_mid_run_fails_the_job_once_without_a_retry() {
    let env = env_or_skip!("a_pod_deleted_through_the_api_mid_run_fails_the_job_once_without_a_retry");
    let executor = env.executor(Net::Gate);
    let spec = env.spec("podelete", &["/bin/sh", "-c", "echo running; sleep 120"]);
    let watcher = PodWatcher::start(env.api.clone(), env.namespace.clone(), &spec.job_id);

    let (report, deleted) = thread::scope(|s| {
        let deleter = s.spawn(|| {
            let pod = wait_for_running_pod(&env.api, &env.namespace, &spec.job_id, Duration::from_secs(60));
            env.api.delete_pod(&env.namespace, &pod, 0).unwrap();
            pod
        });
        let report = executor.run(&spec, &[]);
        (report, deleter.join().unwrap())
    });
    let seen = watcher.finish();
    capture("pod_deleted", &report);
    let failure = failure_of(&report);
    println!("API delete of pod {deleted} -> {failure:?}\npods ever seen by an independent watcher: {seen:?}");

    assert!(matches!(&report.result, Err(K8sError::PodDeleted { pod, .. }) if *pod == deleted), "{:?}", report.result);
    assert_eq!(failure.kind, pb::JobFailureKind::ExecutorUnavailable as i32);
    assert!(failure.detail.contains("deleted") && failure.detail.contains(&deleted), "{}", failure.detail);
    assert_eq!(report.pods, vec![deleted.clone()]);
    assert_eq!(seen, BTreeSet::from([deleted.clone()]), "a second pod appeared: that is a Kubernetes retry");
    env.assert_torn_down(&report);
}

/// A container that cannot start for a reason the Job controller does not hide: the digest gate
/// cannot produce this (it proves the image is present), so a Job is created directly. The live
/// Job/Pod JSON is then read with the same `assess` the executor uses.
#[test]
fn an_image_the_node_will_not_pull_is_never_started_on_live_cluster_json() {
    let env = env_or_skip!("an_image_the_node_will_not_pull_is_never_started_on_live_cluster_json");
    // The API refuses a hostPath containing `..`; the executor canonicalises, so this does too.
    let root = fs::canonicalize(scratch_root()).unwrap();
    let in_dir = root.join(format!("raw-{}-in", env.run_id));
    let out_dir = root.join(format!("raw-{}-out", env.run_id));
    fs::create_dir_all(&in_dir).unwrap();
    fs::create_dir_all(&out_dir).unwrap();
    // Removed on every path, a failed assertion included.
    struct RmDirs(Vec<PathBuf>);
    impl Drop for RmDirs {
        fn drop(&mut self) {
            for d in &self.0 {
                let _ = fs::remove_dir_all(d);
            }
        }
    }
    let _rm = RmDirs(vec![in_dir.clone(), out_dir.clone()]);
    let job_name = format!("av-raw-{}", env.run_id.replace('-', ""));
    let absent = format!("alpine@sha256:{}", "0".repeat(64));
    let resources = av_jobs::k8s::Resources::default();
    let manifest = build_job(&JobParams {
        job_name: &job_name,
        namespace: &env.namespace,
        job_id: "raw-never-pull",
        image_ref: &absent,
        command: &["/bin/true".to_string()],
        node_name: "colima",
        in_dir: &in_dir,
        out_dir: &out_dir,
        extra_labels: &env.labels,
        resources: &resources,
        deadline_seconds: 60,
        termination_grace_seconds: 1,
        ttl_after_finished_seconds: 300,
        network_gate: None,
    });
    env.api.create_job(&env.namespace, &manifest).unwrap();
    let selector = format!("batch.kubernetes.io/job-name={job_name}");
    let end = Instant::now() + Duration::from_secs(60);
    let outcome = loop {
        let job = env.api.get_job(&env.namespace, &job_name).unwrap();
        let pods = env.api.list_pods(&env.namespace, &selector).unwrap();
        let tracks: BTreeMap<String, PodTrack> = pods.iter().map(|p| (status::pod_name(p).to_string(), PodTrack { last: p.clone(), gone: false })).collect();
        if let Some(decision) = status::assess(&job, &tracks) {
            if let Ok(dir) = std::env::var("AV_K8S_FIXTURE_DIR") {
                fs::create_dir_all(&dir).unwrap();
                for (suffix, mut v) in [("job", job.clone()), ("pod", pods[0].clone())] {
                    v["metadata"].as_object_mut().unwrap().remove("managedFields");
                    fs::write(Path::new(&dir).join(format!("never_pull.{suffix}.json")), serde_json::to_string_pretty(&v).unwrap() + "\n").unwrap();
                }
            }
            break decision;
        }
        assert!(Instant::now() < end, "no decision for an unpullable image within 60 s");
        thread::sleep(Duration::from_millis(250));
    };
    let _ = env.api.delete_job(&env.namespace, &job_name);
    println!("unpullable image -> {outcome:?}");
    match outcome {
        Err(K8sError::NeverStarted { reason, message }) => {
            assert_eq!(reason, "ErrImageNeverPull");
            assert!(message.contains("Container image") && message.contains("is not present with pull policy of Never"), "{message}");
        }
        other => panic!("expected NeverStarted(ErrImageNeverPull), got {other:?}"),
    }
}

#[test]
fn the_api_client_types_a_refusal_from_the_live_server() {
    let env = env_or_skip!("the_api_client_types_a_refusal_from_the_live_server");
    match env.api.get_job(&env.namespace, "no-such-job") {
        Err(ApiError::Refused(s)) => {
            assert_eq!((s.code, s.reason.as_str()), (404, "NotFound"));
            assert!(s.message.contains("no-such-job"), "{s:?}");
        }
        other => panic!("{other:?}"),
    }
    // A second create under an existing name is a 409 AlreadyExists.
    match env.api.create_namespace(&env.namespace, &env.labels) {
        Err(ApiError::Refused(s)) => assert_eq!((s.code, s.reason.as_str()), (409, "AlreadyExists")),
        other => panic!("{other:?}"),
    }
    // An invalid manifest is a 422 Invalid with the field named.
    let bad = serde_json::json!({"apiVersion": "batch/v1", "kind": "Job", "metadata": {"name": "Bad_Name"}, "spec": {}});
    match env.api.create_job(&env.namespace, &bad) {
        Err(ApiError::Refused(s)) => {
            assert_eq!((s.code, s.reason.as_str()), (422, "Invalid"));
            println!("422 -> {s}");
        }
        other => panic!("{other:?}"),
    }
}

// ------------------------------------------------------------------------------------------
// Network: the default-deny NetworkPolicy really is enforced on this node.
// ------------------------------------------------------------------------------------------

/// A bare pod (not a Job) running `command` with the pinned image, on the one node.
fn bare_pod(env: &Env, name: &str, labels: &[(&str, &str)], script: &str) -> Value {
    let mut all: BTreeMap<String, String> = env.labels.clone();
    for (k, v) in labels {
        all.insert(k.to_string(), v.to_string());
    }
    serde_json::json!({
        "apiVersion": "v1", "kind": "Pod",
        "metadata": {"name": name, "namespace": env.namespace, "labels": all},
        "spec": {
            "restartPolicy": "Never", "nodeName": "colima",
            "automountServiceAccountToken": false, "enableServiceLinks": false, "terminationGracePeriodSeconds": 1,
            "containers": [{
                "name": "job", "image": format!("alpine@{}", env.recorded_id), "imagePullPolicy": "Never",
                "command": ["/bin/sh", "-c", script],
                "resources": {"requests": {"cpu": "10m", "memory": "8Mi"}, "limits": {"cpu": "250m", "memory": "64Mi"}},
            }],
        },
    })
}

fn wait_pod<F: Fn(&Value) -> bool>(api: &K8sApi, ns: &str, name: &str, within: Duration, done: F) -> Value {
    let end = Instant::now() + within;
    loop {
        let pod = api.get_pod(ns, name).unwrap();
        if done(&pod) {
            return pod;
        }
        assert!(Instant::now() < end, "pod {name} not in the wanted state within {within:?}: {}", pod["status"]);
        thread::sleep(Duration::from_millis(200));
    }
}

/// Runs the probe script in a bare pod and returns its log once the pod has finished.
fn run_probe(env: &Env, name: &str, labels: &[(&str, &str)], script: &str) -> String {
    env.api.create_pod(&env.namespace, &bare_pod(env, name, labels, script)).unwrap();
    wait_pod(&env.api, &env.namespace, name, Duration::from_secs(240), |p| matches!(p["status"]["phase"].as_str(), Some("Succeeded" | "Failed")));
    String::from_utf8(env.api.pod_log(&env.namespace, name, "job", 100, 8192).unwrap()).unwrap()
}

fn start_server(env: &Env, name: &str, labels: &[(&str, &str)]) -> String {
    let mut l = vec![("role", "probe-server")];
    l.extend_from_slice(labels);
    // busybox has no httpd; `nc -lk -e` accepts connections persistently, which is all `nc -z` needs.
    let script = "exec nc -lk -p 8080 -e /bin/true";
    env.api.create_pod(&env.namespace, &bare_pod(env, name, &l, script)).unwrap();
    let pod = wait_pod(&env.api, &env.namespace, name, Duration::from_secs(60), |p| p["status"]["containerStatuses"][0]["state"]["running"].is_object() && p["status"]["podIP"].is_string());
    // Give nc a moment to bind (it is the pod's only process; a failed bind would end the pod).
    thread::sleep(Duration::from_secs(1));
    pod["status"]["podIP"].as_str().unwrap().to_string()
}

/// `nc -z` to each target; prints `<ip>:<port> rc=<n>` lines (rc 0 = connected).
fn probe_script(targets: &[(&str, u16)]) -> String {
    targets.iter().map(|(ip, port)| format!("nc -z -w 3 {ip} {port}; echo \"{ip}:{port} rc=$?\";")).collect::<Vec<_>>().join(" ")
}

fn rc_of(log: &str, target: &str) -> i32 {
    let line = log.lines().find(|l| l.starts_with(&format!("{target} rc="))).unwrap_or_else(|| panic!("no result for {target} in log:\n{log}"));
    line.rsplit('=').next().unwrap().trim().parse().unwrap()
}

/// `(rc of each attempt)` from a log of `attempt<i> <ip>:<port> rc=<n>` lines for one target.
fn attempts(log: &str, target: &str) -> Vec<i32> {
    log.lines().filter(|l| l.contains(&format!("{target} rc="))).map(|l| l.rsplit('=').next().unwrap().trim().parse().unwrap()).collect()
}

/// The control and the labelled pod run the same loop: connect every 0.5 s for ~20 s.
fn loop_script(ip: &str, port: u16) -> String {
    format!("i=0; while [ $i -lt 40 ]; do nc -z -w 1 {ip} {port}; echo \"attempt$i {ip}:{port} rc=$?\"; i=$((i+1)); sleep 0.5; done")
}

#[test]
fn the_default_deny_policy_is_enforced_on_this_node_after_a_start_up_window() {
    let env = env_or_skip!("the_default_deny_policy_is_enforced_on_this_node_after_a_start_up_window");
    // The executor ensures the policy exists in its namespace (and a second one accepts it as is).
    let _executor = env.executor(Net::AcceptWindow);
    let _second = env.executor(Net::AcceptWindow);
    let policy = env.api.get_network_policy(&env.namespace, "av-job-default-deny").expect("the executor created the policy");
    assert_eq!(policy["spec"]["podSelector"]["matchLabels"]["av.job"], "1");
    assert_eq!(policy["spec"]["policyTypes"], serde_json::json!(["Ingress", "Egress"]));
    println!("policy: {}", policy["spec"]);

    // Targets: a pod in this namespace the policy does not select (nc listener), and the API server's ClusterIP.
    let server_ip = start_server(&env, "server-open", &[]);
    let server_key = format!("{server_ip}:8080");
    let api_key = "10.43.0.1:443";

    // CONTROL: a pod the policy does not select connects, every attempt, to both targets.
    let control_pod = run_probe(&env, "client-open", &[], &format!("{}; {}", loop_script(&server_ip, 8080), loop_script("10.43.0.1", 443)));
    let (c_srv, c_api) = (attempts(&control_pod, &server_key), attempts(&control_pod, api_key));
    println!("--- control (no av.job label): server {} attempts, all rc=0: {}; API ClusterIP {} attempts, all rc=0: {}", c_srv.len(), c_srv.iter().all(|r| *r == 0), c_api.len(), c_api.iter().all(|r| *r == 0));
    assert!(c_srv.len() == 40 && c_srv.iter().all(|r| *r == 0), "the control must always reach the server, or the probe proves nothing: {c_srv:?}");
    assert!(c_api.len() == 40 && c_api.iter().all(|r| *r == 0), "the control must always reach the API server ClusterIP: {c_api:?}");

    // TREATMENT: the same pod with the label the policy selects. Egress is denied, after a start-up window.
    let denied = run_probe(&env, "client-denied", &[("av.job", "1")], &format!("{}; {}", loop_script(&server_ip, 8080), loop_script("10.43.0.1", 443)));
    for key in [&server_key, &api_key.to_string()] {
        let rcs = attempts(&denied, key);
        let first_denied = rcs.iter().position(|r| *r != 0);
        println!("--- labelled av.job=1: {key}: rc per attempt {rcs:?}; first denied attempt: {first_denied:?}");
        assert_eq!(rcs.len(), 40);
        let first = first_denied.unwrap_or_else(|| panic!("egress to {key} was never denied: the policy is not enforced"));
        assert!(rcs[first..].iter().all(|r| *r != 0), "once denied, always denied ({key}): {rcs:?}");
        assert!(first <= 20, "enforcement took longer than 10 s to arrive ({key}): {rcs:?}");
    }

    // INGRESS: an unselected client, after the window, cannot reach a selected server but still reaches the unselected one.
    let guarded_ip = start_server(&env, "server-guarded", &[("av.job", "1")]);
    let ingress = run_probe(&env, "client-ingress", &[], &format!("sleep 12; {} {}", probe_script(&[(guarded_ip.as_str(), 8080)]), probe_script(&[(server_ip.as_str(), 8080)])));
    println!("--- ingress, after 12 s: unselected client -> selected server / unselected server ---\n{ingress}");
    assert_ne!(rc_of(&ingress, &format!("{guarded_ip}:8080")), 0, "ingress to a selected pod must be denied:\n{ingress}");
    assert_eq!(rc_of(&ingress, &server_key), 0, "the unselected server stays reachable:\n{ingress}");
}

#[test]
fn with_the_network_gate_a_real_executor_jobs_pod_never_has_a_network() {
    let env = env_or_skip!("with_the_network_gate_a_real_executor_jobs_pod_never_has_a_network");
    let executor = env.executor(Net::Gate); // env.config(Net::Gate) sets the gate
    let server_ip = start_server(&env, "server-open", &[]);
    let server_key = format!("{server_ip}:8080");
    let api_key = "10.43.0.1:443";
    // Probe from the workload's very first instruction, repeatedly, to both targets.
    let probe = |i: u32| format!("nc -z -w 1 {server_ip} 8080; echo \"attempt{i} {server_key} rc=$?\"; nc -z -w 1 10.43.0.1 443; echo \"attempt{i} {api_key} rc=$?\";");
    let script = format!("{} {} {} sleep 1; {} echo done", probe(0), probe(1), probe(2), probe(3));
    // Three jobs in a row, so a lucky timing cannot pass for the guarantee.
    for n in 0..3 {
        let report = executor.run(&env.spec(&format!("netjob{n}"), &["/bin/sh", "-c", &script]), &[]);
        assert!(report.result.is_ok(), "{:?} / {:?}", report.result, report.log);
        let log = report.log.as_ref().unwrap().text.clone();
        println!("--- executor job {n} (gate on) ---\n{log}");
        for key in [&server_key, &api_key.to_string()] {
            let rcs = attempts(&log, key);
            assert_eq!(rcs.len(), 4, "{log}");
            assert!(rcs.iter().all(|r| *r != 0), "the workload reached {key} (rc per attempt {rcs:?}):\n{log}");
        }
        // The gate ran first and exited 0.
        let pod = report.final_pod.as_ref().unwrap();
        assert_eq!(pod["status"]["initContainerStatuses"][0]["name"], "net-gate");
        assert_eq!(pod["status"]["initContainerStatuses"][0]["state"]["terminated"]["exitCode"], 0);
        env.assert_torn_down(&report);
    }
}

#[test]
fn without_enforcement_the_gate_fails_closed_and_the_workload_never_runs() {
    let env = env_or_skip!("without_enforcement_the_gate_fails_closed_and_the_workload_never_runs");
    let mut cfg = env.config(Net::Gate);
    cfg.network_gate.as_mut().unwrap().within = Duration::from_secs(6);
    let executor = K8sJobExecutor::new(cfg).unwrap();
    // Take the policy away (it is this test's own, in this test's own namespace): the cluster now
    // has nothing to enforce, which is what a cluster without a policy controller looks like to the pod.
    env.api.delete_network_policy(&env.namespace, "av-job-default-deny").unwrap();
    thread::sleep(Duration::from_secs(5)); // let the controller drop the rules the earlier pods' labels had
    let report = executor.run(&env.spec("openjob", &["/bin/sh", "-c", "echo WORKLOAD-RAN; echo x > /av-job/out/o"]), &[]);
    capture("gate_closed", &report);
    let failure = failure_of(&report);
    println!("{failure:?}\ngate log: {:?}", report.log);
    assert!(matches!(&report.result, Err(K8sError::NeverStarted { reason, .. }) if reason == "InitContainerFailed(net-gate)"), "{:?}", report.result);
    assert_eq!(failure.kind, pb::JobFailureKind::ExecutorStartFailed as i32);
    let log = report.log.as_ref().expect("the gate's own log is captured").text.clone();
    assert!(log.contains("still open"), "{log}");
    assert!(!log.contains("WORKLOAD-RAN"));
    let pod = report.final_pod.as_ref().unwrap();
    assert!(pod["status"]["containerStatuses"][0]["state"]["waiting"].is_object(), "the workload container never started: {}", pod["status"]["containerStatuses"]);
    env.assert_torn_down(&report);
}

#[test]
fn a_test_pod_exists_only_while_the_docker_lock_is_held() {
    let env = env_or_skip!("a_test_pod_exists_only_while_the_docker_lock_is_held");
    let (run_id, namespace, api) = (env.run_id.clone(), env.namespace.clone(), env.api.clone());
    let label = format!("av.test.run_id={run_id}");

    // BEFORE: setup took the lock first and asserted this run had no container and no namespace.
    // DURING: pods of this run are on the shared Docker, sandboxes included, carrying av.test=1 -- the
    // very containers the native team's prune would delete if they outlived the lock.
    let _server_ip = start_server(&env, "server-held", &[]);
    let report = env.executor(Net::Gate).run(&env.spec("held", &["/bin/sh", "-c", "sleep 1"]), &[]);
    assert!(report.result.is_ok(), "{:?}", report.result);
    let labelled = docker_containers_with(&label);
    let during = run_containers(&run_id, &namespace);
    println!("while the lock is held: labelled av.test.run_id: {labelled:?}\nall containers of the namespace: {during:?}");
    assert!(labelled.iter().any(|n| n.starts_with("k8s_POD_server-held")), "the pause sandbox carries the label: {labelled:?}");
    assert!(during.iter().any(|n| n.starts_with("k8s_job_server-held")), "the workload container exists: {during:?}");
    assert!(!docker_containers_with("av.test=1").is_empty());
    assert!(api.get_namespace(&namespace).is_ok());

    // AFTER: dropping Env deletes the namespace and waits for the containers, then releases the lock.
    drop(env);
    assert!(matches!(api.get_namespace(&namespace), Err(e) if e.is_not_found()), "the namespace outlived the lock");
    assert!(api.list_pods(&namespace, "av.job=1").map(|p| p.is_empty()).unwrap_or(true));
    let after = run_containers(&run_id, &namespace);
    println!("after the lock is released: {after:?}");
    assert!(after.is_empty(), "containers of the run outlived the lock: {after:?}");
    assert!(docker_containers_with("av.test").iter().all(|n| !n.contains(&run_id)));
}

#[test]
fn the_executor_fails_closed_on_the_network_unless_a_gate_or_an_explicit_opt_in_is_configured() {
    let env = env_or_skip!("the_executor_fails_closed_on_the_network_unless_a_gate_or_an_explicit_opt_in_is_configured");

    // No gate and no opt-in: refused, typed, naming the field and the measured window.
    let mut cfg = env.config(Net::AcceptWindow);
    cfg.accept_startup_network_window = false;
    match K8sJobExecutor::new(cfg) {
        Err(K8sError::Preflight(m)) => assert!(m.contains("accept_startup_network_window") && m.contains("first ~1-2 s run with network"), "{m}"),
        other => panic!("{other:?}"),
    }

    // A gate image under the wrong digest: refused by the same digest gate as the job image.
    let mut cfg = env.config(Net::Gate);
    cfg.network_gate.as_mut().unwrap().image_digest = format!("sha256:{}", "0".repeat(64));
    match K8sJobExecutor::new(cfg) {
        Err(K8sError::ImageGate(f)) => {
            assert_eq!(f.kind, pb::JobFailureKind::ExecutorUnavailable as i32);
            assert!(f.detail.contains("refused rather than trusted"), "{}", f.detail);
        }
        other => panic!("{other:?}"),
    }
    // ... and an absent gate image likewise.
    let mut cfg = env.config(Net::Gate);
    cfg.network_gate.as_mut().unwrap().image = "av-jobs-no-such-gate-image".to_string();
    assert!(matches!(K8sJobExecutor::new(cfg), Err(K8sError::ImageGate(_))));

    // The explicit opt-in runs a job (with the window, by choice).
    let executor = env.executor(Net::AcceptWindow);
    let report = executor.run(&env.spec("optin", &["/bin/sh", "-c", "echo ok > /av-job/out/r"]), &[]);
    assert_eq!(report.result.as_ref().unwrap_or_else(|e| panic!("{e}"))[0].bytes, b"ok\n");
    assert!(report.final_pod.as_ref().unwrap()["spec"].get("initContainers").is_none(), "the opt-in mode has no gate");
    env.assert_torn_down(&report);

    // The gate configured runs, with the gate init container ahead of the workload.
    let executor = env.executor(Net::Gate);
    let report = executor.run(&env.spec("gated", &["/bin/sh", "-c", "echo ok > /av-job/out/r"]), &[]);
    assert_eq!(report.result.as_ref().unwrap_or_else(|e| panic!("{e}"))[0].bytes, b"ok\n");
    assert_eq!(report.final_pod.as_ref().unwrap()["spec"]["initContainers"][0]["name"], "net-gate");
    env.assert_torn_down(&report);
    // (Fail-closed when egress is NOT blocked: without_enforcement_the_gate_fails_closed_and_the_workload_never_runs.)
}
