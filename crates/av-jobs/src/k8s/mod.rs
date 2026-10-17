//! Question 245 (heavy container round), task heavy-ct-2a: the `JOB_EXECUTOR_KIND_CONTAINER`
//! [`crate::runner::Executor`] that runs a job as a Kubernetes `batch/v1` Job -- one pod, never
//! retried -- instead of a local `docker run` ([`crate::container::ContainerExecutor`], whose
//! contract this one keeps: the same image gate, hardening, labels, data flow and cleanup).
//!
//! # Shape
//!
//! * [`kubeconfig`]: reads `~/.kube/config` (or `$KUBECONFIG`), one context, **client certificate
//!   authentication only**, decoded into memory; every other way to authenticate, a non-HTTPS
//!   server, and every switch that would weaken TLS verification is refused with a typed error.
//! * [`api`]: the API client over `av-http1` (blocking, OpenSSL, hostname/IP-SAN verification
//!   always on): create / get / delete Job, list / delete pods, pod log, nodes, namespaces; error
//!   replies typed from the Kubernetes `Status` (`code`, `reason`, `message`).
//! * [`manifest`]: the Job as a pure function of its parameters (every field unit-tested).
//! * [`status`]: reading Job and Pod JSON into one decision (unit-tested against JSON recorded from
//!   this host's k3s, `tests/fixtures/k8s/`).
//! * this module: [`K8sConfig`], [`K8sJobExecutor`] (`Sync`: several jobs can run at once from
//!   several threads, each its own Job and pod), [`RunReport`], and the failure table below.
//!
//! # The data plane is single-node
//!
//! Inputs are written to `<scratch>/in/000`, `001`, ... and `<scratch>/out` is created mode
//! `0o777`; both are `hostPath` volumes (`/av-job/in` read-only, `/av-job/out` read-write), the same
//! contract as the Docker executor. That works only because of two facts the executor checks or
//! the operator must keep true: **the cluster has exactly one node that can run the pod** (the pod
//! is pinned with `nodeName`; [`K8sJobExecutor::new`] refuses a cluster with more than one node
//! unless one is named) and **the scratch root is a directory the node sees at the same path** (on
//! Colima, `$HOME` is mounted into the VM at the same path; [`K8sJobExecutor::new`] refuses a
//! scratch root outside `$HOME`, where a bind mount silently becomes an empty directory). Several
//! pods at once are several pods on **one** node, not several hosts. Spreading jobs across hosts
//! needs the object store (claim check) as the data plane instead of `hostPath`: pods would fetch
//! their inputs and put their outputs by hash, and the executor would not touch a filesystem at
//! all. That is a different executor; this one does not pretend to be it.
//!
//! Likewise the image gate ([`crate::container::digest_gate`]) asks the *local Docker* whether
//! `<image>@<digest>` is present, and the pod runs `<image>@<digest>` with `imagePullPolicy:
//! Never`. That is only a statement about the node when the node's container runtime *is* that
//! Docker (Colima's k3s runs with `--docker`); [`K8sJobExecutor::new`] refuses a node whose
//! `containerRuntimeVersion` is not `docker://...`.
//!
//! # Network: `--network none` has no pod equivalent, so this is a policy plus a gate
//!
//! `ContainerExecutor` runs with `--network none`. A pod cannot say that: it always has a pod
//! network. The `securityContext` carries everything else of
//! [`crate::container::HARDENING_RUN_FLAGS`] (see [`manifest`]). For the network, two things:
//!
//! 1. **A default-deny `NetworkPolicy`** (`av-job-default-deny`, [`manifest::network_policy`]):
//!    `podSelector` `av.job=1` (every pod this module creates), `policyTypes` Ingress and Egress,
//!    no rules -- nothing in, nothing out, no DNS, no API server. [`K8sJobExecutor::new`] ensures it
//!    in the executor's namespace: it creates the policy if absent and refuses, typed
//!    ([`K8sError::Preflight`]), a same-named policy whose spec differs (an operator's allow rule is
//!    neither overwritten nor trusted as a deny). Chosen over a policy per Job because one
//!    namespace-wide policy cannot be forgotten, leaked or deleted half-way through a Job.
//! 2. **A network gate** ([`K8sConfig::network_gate`], [`manifest::NetworkGateParams`]), required
//!    unless the caller explicitly opts out (below):
//!    an init container, under the same labels and policy, that retries a TCP connect to the cluster's
//!    own API-server ClusterIP and lets the workload start only once a connect *fails*; if egress is
//!    still open after `within` it exits 1 and the pod fails with `NeverStarted
//!    (InitContainerFailed(net-gate))` before the workload runs. It fails closed.
//!
//! **Measured on this host's k3s (v1.35, flannel, kube-router policy controller): the policy is
//! enforced, but not from the pod's first instant.** A pod labelled `av.job=1` could still connect for
//! about the first one to two seconds (`tests/k8s.rs::the_default_deny_policy_is_enforced_on_this_node_after_a_start_up_window`:
//! attempts 0-1 connected, every later one -- and every one of the second target -- was refused, while
//! the identical unlabelled control pod connected 40 of 40 times to both targets; ingress to a
//! selected pod was refused too). So **without the gate a job's first ~1-2 s have a network**; with
//! it, none of 24 connection attempts in 3 real executor jobs got through, and with the policy
//! removed the gate refused to run the workload at all (`without_enforcement_the_gate_fails_closed...`).
//!
//! **Fail closed by default.** [`K8sJobExecutor::new`] refuses (typed [`K8sError::Preflight`], before
//! connecting) a config with no `network_gate` unless the caller sets
//! [`K8sConfig::accept_startup_network_window`] -- documented as "the job's first ~1-2 s run with
//! network on this CNI (measured)" -- so the unsafe mode is a visible choice, never the default. The
//! gate image is the caller's, pinned by digest (it needs `nc`, which this module cannot choose),
//! and passes the same `crate::container::digest_gate` as the job image: at `new()` (absent or wrong
//! digest is refused with the gate's own typed failure, before connecting) and again before every
//! run (an image can be garbage-collected between runs). Open items: (a) the gate proves *egress to the API server's ClusterIP* is
//! closed, not egress to every destination -- it relies on the policy being namespace-wide default
//! deny, which [`K8sJobExecutor::new`] verifies; (b) a CNI other than this host's would need the
//! measurement repeated; (c) the gate costs one to two seconds per job.
//!
//! # Shared Docker: label plus lock (a cross-track hazard)
//!
//! This host's pods are containers on the same Docker as every other docker-gated test, and a pod
//! labelled `av.test=1` puts that label on its `k8s_POD_*` pause sandbox. The native tests'
//! `prune_stale_test_resources` removes every container with that label, so a *test* that creates
//! pods holds `lock_docker_tests` for the pods' whole lifetime -- from before the first Job is
//! created until its namespace is deleted and its containers are gone -- and keeps the label so a
//! crashed run's leftovers can still be swept. The executor itself takes no lock (production jobs
//! carry no `av.test` label); `tests/k8s.rs` documents and proves the test-side contract.
//!
//! # Lifecycle of one job ([`K8sJobExecutor::run`])
//!
//! 1. the image gate; the scratch directory (`in/`, `out/` mode `0o777`, one file per input);
//! 2. `POST` the Job (a name made unique per run);
//! 3. poll the Job and its pods (by `batch.kubernetes.io/job-name`) until [`status::assess`]
//!    decides, or until `deadline + margin` (the Job's own `activeDeadlineSeconds` is the
//!    authority; the margin only bounds the executor's patience with the control plane);
//!    **the pod's log is captured** (capped, truncation marked) the moment the pod is terminal and,
//!    failing that, before the Job is deleted;
//! 4. on exit 0 every regular file under `out/` becomes a [`crate::runner::JobOutput`], sorted;
//! 5. the Job is deleted with `propagationPolicy: Foreground` and waited for until it is gone (so
//!    its pod is gone), and the scratch directory removed -- on **every** path, by an explicit
//!    teardown whose result is in the report and, as a backstop for panics and early returns, a
//!    `Drop` guard.
//!
//! **Never silently retried.** The manifest says `backoffLimit: 0` and `podReplacementPolicy:
//! Failed`; the executor additionally refuses to accept a second pod (or a Job counter above one)
//! whatever it did ([`K8sError::Retried`]), and waits for the Job to be `Failed` rather than
//! reporting a failed pod early, so a retry that the manifest failed to prevent would be seen.
//!
//! # Failure table (no proto change: every case reuses an existing `JobFailureKind`)
//!
//! | [`K8sError`]                                     | `JobFailureKind`            | `exit_code` | when |
//! |--------------------------------------------------|-----------------------------|-------------|------|
//! | `ImageGate`                                      | the gate's own (`EXECUTOR_UNAVAILABLE` / `EXECUTOR_START_FAILED`) | the gate's | image absent / wrong digest / empty |
//! | `ExitedNonZero { code }`                         | `NONZERO_EXIT`              | `code`      | the command exited non-zero |
//! | `Killed { exit_code, signal, reason }`           | `NONZERO_EXIT`              | `exit_code` | exit > 128 (death by signal), `OOMKilled`, a runtime-reported signal; e.g. `docker kill` of the container |
//! | `DeadlineExceeded`                               | `NONZERO_EXIT`              | the container's, else `-1` | the Job's `activeDeadlineSeconds` expired |
//! | `NeverStarted { reason }`                        | `EXECUTOR_START_FAILED`     | 0           | `ErrImageNeverPull`, `CreateContainerError`, `StartError` (command not found), `OutOfcpu`, ... |
//! | `PodDeleted`, `JobDeleted`                       | `EXECUTOR_UNAVAILABLE`      | 0           | the pod / Job was deleted by someone else mid-run |
//! | `Retried { pods }`                               | `EXECUTOR_UNAVAILABLE`      | 0           | more than one pod ever existed |
//! | `ApiUnreachable`, `ApiRefused`, `ApiBadReply`, `Kubeconfig`, `Preflight`, `Unexpected` | `EXECUTOR_UNAVAILABLE` | 0 | the control plane could not be used |
//! | `Scratch`                                        | `EXECUTOR_START_FAILED`     | 0           | the scratch directory / inputs could not be written |
//! | `Output`                                         | `OUTPUT_REJECTED`           | 0           | the outputs could not be read |
//!
//! `JobFailure.detail` always starts `K8S executor:`, names the job, the error (which carries the
//! Kubernetes `reason`), the pod names, and the tail of the pod's log.

pub mod api;
pub mod kubeconfig;
pub mod manifest;
pub mod status;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use av_cdm::pb;
use av_http1::Timeouts;
use serde_json::Value;

pub use api::{ApiError, ApiStatus, K8sApi};
pub use kubeconfig::{Kubeconfig, KubeconfigError};
pub use manifest::Resources;
pub use status::K8sError;

use crate::container::{digest_gate, CONTAINER_INPUT_DIR, CONTAINER_OUTPUT_DIR};
use crate::runner::{Executor, JobInput, JobOutput};
use manifest::{build_job, dns_fragment, network_policy, network_policy_matches, JobParams, NetworkGateParams, CONTAINER_NAME, NETWORK_POLICY_NAME};
use status::{assess, assess_at_bound, pod_is_terminal, pod_name, terminated, PodTrack};

/// Everything a [`K8sJobExecutor`] is configured with. Fields are public: build with
/// [`K8sConfig::new`] and adjust.
#[derive(Debug, Clone)]
pub struct K8sConfig {
    pub kubeconfig: Kubeconfig,
    /// The namespace the Jobs are created in. It must already exist.
    pub namespace: String,
    /// The node the pods are pinned to. `None`: use the cluster's only node, and refuse if there is
    /// more than one.
    pub node_name: Option<String>,
    /// Where per-run scratch directories are created. Must be under `$HOME` (see the module doc).
    pub scratch_root: PathBuf,
    /// Labels added to the Job and its pods, in addition to `av.job=1` and `av.job.id=<id>`.
    pub extra_labels: BTreeMap<String, String>,
    pub resources: Resources,
    /// The Job's `activeDeadlineSeconds` (rounded up to whole seconds, at least one).
    pub deadline: Duration,
    /// Added to `deadline` to bound how long the executor polls.
    pub margin: Duration,
    pub poll_interval: Duration,
    /// The pod's `terminationGracePeriodSeconds`.
    pub termination_grace: Duration,
    /// The Job's `ttlSecondsAfterFinished`: a backstop that removes a finished Job even if this
    /// process died before its own teardown. The executor deletes its Job itself long before.
    pub ttl_after_finished: Duration,
    /// The pod log is requested with at most this many bytes ...
    pub log_cap_bytes: usize,
    /// ... and at most this many trailing lines.
    pub log_max_lines: usize,
    pub timeouts: Timeouts,
    /// How many consecutive API polls may fail on transport errors before the job is failed.
    pub poll_error_budget: u32,
    /// The network-isolation gate (see [`manifest::NetworkGateParams`]): the safe configuration, and
    /// the only one [`K8sJobExecutor::new`] accepts without an explicit opt-in. The gate image is
    /// chosen by the caller and pinned by digest; it passes the same `digest_gate` as the job image,
    /// at `new()` and again before every run.
    pub network_gate: Option<NetworkGate>,
    /// **Fails closed by default.** With no `network_gate`, [`K8sJobExecutor::new`] refuses
    /// (`Preflight`) unless this is `true`. Setting it says: "the job's first ~1-2 s run with network
    /// on this CNI (measured)" -- the policy controller closes the pod's network only after the pod
    /// has started. An unsafe mode is a visible choice, never the default. Ignored when a gate is set.
    pub accept_startup_network_window: bool,
}

/// Configuration of the network-isolation gate init container: the image it runs (it needs `nc`)
/// and how long it waits for egress to close before failing the pod. The probe target is the
/// cluster's own API server (`default/kubernetes`' ClusterIP), resolved at start-up.
#[derive(Debug, Clone)]
pub struct NetworkGate {
    /// Bare image reference, e.g. `"alpine"`; checked with the same digest gate as job images.
    pub image: String,
    /// `"sha256:..."`.
    pub image_digest: String,
    /// How long the gate waits for a connect to fail before it fails the pod.
    pub within: Duration,
}

impl K8sConfig {
    pub fn new(kubeconfig: Kubeconfig, namespace: impl Into<String>, scratch_root: impl Into<PathBuf>) -> K8sConfig {
        K8sConfig {
            kubeconfig,
            namespace: namespace.into(),
            node_name: None,
            scratch_root: scratch_root.into(),
            extra_labels: BTreeMap::new(),
            resources: Resources::default(),
            deadline: Duration::from_secs(300),
            margin: Duration::from_secs(30),
            poll_interval: Duration::from_millis(250),
            termination_grace: Duration::from_secs(5),
            ttl_after_finished: Duration::from_secs(600),
            log_cap_bytes: 64 * 1024,
            log_max_lines: 1000,
            timeouts: Timeouts::default(),
            poll_error_budget: 5,
            network_gate: None,
            accept_startup_network_window: false,
        }
    }
}

/// The captured pod log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogCapture {
    pub pod: String,
    /// The log text; when `truncated`, it carries a marker line saying how.
    pub text: String,
    pub truncated: bool,
}

/// Timestamps the cluster itself recorded for the pod (RFC 3339, one-second resolution).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PodTimes {
    pub pod_start_time: Option<String>,
    pub container_started_at: Option<String>,
    pub container_finished_at: Option<String>,
}

/// What one job run produced, beyond the `Executor` result.
#[derive(Debug)]
pub struct RunReport {
    pub job_id: String,
    pub job_name: String,
    pub namespace: String,
    /// The outputs (every file under `out/`, sorted) on exit 0, else why not.
    pub result: Result<Vec<JobOutput>, K8sError>,
    /// Every pod name ever observed for the Job (exactly one for a healthy run).
    pub pods: Vec<String>,
    pub log: Option<LogCapture>,
    /// Why there is no log, if there is none.
    pub log_error: Option<String>,
    pub pod_times: Option<PodTimes>,
    /// Wall time from the start of `run` to the end of teardown.
    pub elapsed: Duration,
    /// The last Job and pod objects observed (for diagnostics and for recording fixtures).
    pub final_job: Option<Value>,
    pub final_pod: Option<Value>,
    /// Whether the Job (and with it the pod) and the scratch directory were removed.
    pub cleanup: Result<(), String>,
}

impl RunReport {
    /// The `JobFailure` for a failed run (`None` for a successful one): the error per the table in
    /// the module doc, the pod names, and the tail of the captured log.
    pub fn job_failure(&self) -> Option<pb::JobFailure> {
        let tail = self.log.as_ref().map(|l| log_tail(&l.text));
        self.result.as_ref().err().map(|e| e.to_failure(&self.job_id, &self.pods, tail.as_deref()))
    }

    /// The `Executor` view of this report.
    pub fn into_executor_result(self) -> Result<Vec<JobOutput>, pb::JobFailure> {
        if let Some(failure) = self.job_failure() {
            return Err(failure);
        }
        self.result.map_err(|e| e.to_failure(&self.job_id, &self.pods, None))
    }
}

impl K8sError {
    /// The `JobFailure` for this error, per the table in the [`crate::k8s`] module doc.
    pub fn to_failure(&self, job_id: &str, pods: &[String], log_tail: Option<&str>) -> pb::JobFailure {
        use pb::JobFailureKind as K;
        let (kind, exit_code) = match self {
            K8sError::ImageGate(f) => {
                // The gate's own failure, passed through unchanged.
                return f.clone();
            }
            K8sError::ExitedNonZero { code, .. } => (K::NonzeroExit, *code),
            K8sError::Killed { exit_code, .. } => (K::NonzeroExit, *exit_code),
            K8sError::DeadlineExceeded { exit_code, .. } => (K::NonzeroExit, exit_code.unwrap_or(-1)),
            K8sError::NeverStarted { .. } | K8sError::Scratch(_) => (K::ExecutorStartFailed, 0),
            K8sError::Output(_) => (K::OutputRejected, 0),
            K8sError::PodDeleted { .. }
            | K8sError::JobDeleted
            | K8sError::Retried { .. }
            | K8sError::ApiUnreachable { .. }
            | K8sError::ApiRefused { .. }
            | K8sError::ApiBadReply { .. }
            | K8sError::Kubeconfig(_)
            | K8sError::Preflight(_)
            | K8sError::Unexpected(_) => (K::ExecutorUnavailable, 0),
        };
        let mut detail = format!("K8S executor: job {job_id:?}: {self}");
        if !pods.is_empty() {
            detail.push_str(&format!("; pod(s) {pods:?}"));
        }
        if let Some(tail) = log_tail.filter(|t| !t.is_empty()) {
            detail.push_str(&format!("; log tail: {tail:?}"));
        }
        pb::JobFailure { kind: kind as i32, detail, exit_code }
    }
}

/// The last 15 lines, at most 1500 bytes, of a captured log (for `JobFailure.detail`).
fn log_tail(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let joined = lines[lines.len().saturating_sub(15)..].join("\n");
    if joined.len() <= 1500 {
        return joined;
    }
    let mut start = joined.len() - 1500;
    while !joined.is_char_boundary(start) {
        start += 1;
    }
    joined[start..].to_string()
}

static RUN_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A per-run unique suffix: wall-clock nanoseconds plus a process-wide counter, in hex.
fn unique_suffix() -> String {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("{:x}{:x}", nanos & 0xffff_ffff_ffff, RUN_COUNTER.fetch_add(1, Ordering::Relaxed))
}

/// The Kubernetes Jobs [`Executor`]. See the module doc.
#[derive(Debug)]
pub struct K8sJobExecutor {
    api: K8sApi,
    server: String,
    namespace: String,
    node_name: String,
    scratch_root: PathBuf,
    extra_labels: BTreeMap<String, String>,
    resources: Resources,
    deadline_seconds: u64,
    margin: Duration,
    poll_interval: Duration,
    termination_grace_seconds: u64,
    ttl_after_finished_seconds: u64,
    log_cap_bytes: usize,
    log_max_lines: usize,
    poll_error_budget: u32,
    network_gate: Option<NetworkGateParams>,
    /// `(image, digest)` of the gate image, re-checked by the digest gate before every run.
    gate_image: Option<(String, String)>,
}

fn api_err(op: &'static str, e: ApiError) -> K8sError {
    match e {
        ApiError::Refused(status) => K8sError::ApiRefused { op, status },
        ApiError::BadBody { detail, .. } => K8sError::ApiBadReply { op, detail },
        other => K8sError::ApiUnreachable { op, detail: other.to_string() },
    }
}

impl K8sJobExecutor {
    /// Connects, and checks what the data plane depends on: the namespace exists, the node
    /// selection is unambiguous (**more than one node and none named is refused**), the chosen
    /// node is `Ready` and runs Docker, the scratch root is under `$HOME`, the default-deny
    /// `NetworkPolicy` exists (created if absent, a differing one refused) and, if configured, the
    /// network gate's image passes the digest gate and the API service address is resolved.
    ///
    /// **Fails closed on the network:** a config with no [`K8sConfig::network_gate`] and no
    /// [`K8sConfig::accept_startup_network_window`] is refused, typed ([`K8sError::Preflight`]),
    /// before any connection is made; a gate image that is absent or under the wrong digest is refused
    /// with the digest gate's own [`K8sError::ImageGate`], also before connecting. (The gate image is
    /// checked again before every run, since an image can be garbage-collected between runs.)
    pub fn new(config: K8sConfig) -> Result<K8sJobExecutor, K8sError> {
        if config.network_gate.is_none() && !config.accept_startup_network_window {
            return Err(K8sError::Preflight(
                "no network gate is configured and accept_startup_network_window is false: without the gate the job's first ~1-2 s run with \
                 network on this CNI (measured), because the NetworkPolicy is enforced only after the pod has started; configure `network_gate` \
                 (a digest-pinned image with `nc`) or set `accept_startup_network_window` to accept the window explicitly"
                    .to_string(),
            ));
        }
        let gate_image_ref = match &config.network_gate {
            Some(g) => Some(digest_gate(&g.image, &g.image_digest).map_err(K8sError::ImageGate)?),
            None => None,
        };
        let gate_image = config.network_gate.as_ref().map(|g| (g.image.clone(), g.image_digest.clone()));
        let api =K8sApi::new(&config.kubeconfig, config.timeouts).map_err(|e| api_err("connect", e))?;
        let nodes = api.list_nodes().map_err(|e| api_err("list nodes", e))?;
        let node_name = select_node(&nodes, config.node_name.as_deref())?;
        api.get_namespace(&config.namespace).map_err(|e| match e {
            ApiError::Refused(s) if s.code == 404 => K8sError::Preflight(format!("namespace {:?} does not exist", config.namespace)),
            other => api_err("get namespace", other),
        })?;
        ensure_network_policy(&api, &config.namespace)?;
        let network_gate = match &config.network_gate {
            None => None,
            Some(g) => {
                let image_ref = gate_image_ref.expect("gate_image_ref is Some whenever the gate is");
                let svc = api.get_service("default", "kubernetes").map_err(|e| api_err("get service default/kubernetes", e))?;
                let ip = svc["spec"]["clusterIP"].as_str().filter(|s| !s.is_empty() && *s != "None");
                let port = svc["spec"]["ports"][0]["port"].as_u64().and_then(|p| u16::try_from(p).ok());
                let (Some(ip), Some(port)) = (ip, port) else {
                    return Err(K8sError::Preflight(format!("service default/kubernetes has no usable ClusterIP and port to probe: {}", svc["spec"])));
                };
                Some(NetworkGateParams { image_ref, probe_ip: ip.to_string(), probe_port: port, within_seconds: g.within.as_secs().max(1) })
            }
        };
        let scratch_root = canonical_scratch_root(&config.scratch_root)?;
        Ok(K8sJobExecutor {
            api,
            server: config.kubeconfig.server.clone(),
            namespace: config.namespace,
            node_name,
            scratch_root,
            extra_labels: config.extra_labels,
            resources: config.resources,
            deadline_seconds: config.deadline.as_secs_f64().ceil().max(1.0) as u64,
            margin: config.margin,
            poll_interval: config.poll_interval,
            termination_grace_seconds: config.termination_grace.as_secs(),
            ttl_after_finished_seconds: config.ttl_after_finished.as_secs(),
            log_cap_bytes: config.log_cap_bytes,
            log_max_lines: config.log_max_lines,
            poll_error_budget: config.poll_error_budget,
            network_gate,
            gate_image,
        })
    }

    /// The API client (shared by tests that inspect the cluster independently of a run).
    pub fn api(&self) -> &K8sApi {
        &self.api
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// The node every pod is pinned to.
    pub fn node_name(&self) -> &str {
        &self.node_name
    }

    /// The API server this executor talks to, as the kubeconfig names it.
    pub fn server(&self) -> &str {
        &self.server
    }

    /// Runs one job and returns everything about it. [`Executor::execute`] is this plus a
    /// conversion to `Result<Vec<JobOutput>, JobFailure>`.
    pub fn run(&self, spec: &pb::JobSpec, inputs: &[JobInput]) -> RunReport {
        let started = Instant::now();
        let suffix = unique_suffix();
        let job_name = format!("av-job-{}-{suffix}", dns_fragment(&spec.job_id, 24));
        let mut report = RunReport {
            job_id: spec.job_id.clone(),
            job_name: job_name.clone(),
            namespace: self.namespace.clone(),
            result: Err(K8sError::Unexpected("run did not finish".to_string())),
            pods: Vec::new(),
            log: None,
            log_error: None,
            pod_times: None,
            elapsed: Duration::ZERO,
            final_job: None,
            final_pod: None,
            cleanup: Ok(()),
        };
        let image_ref = match digest_gate(&spec.container_image, &spec.container_image_digest) {
            Ok(r) => r,
            Err(f) => {
                report.result = Err(K8sError::ImageGate(f));
                report.elapsed = started.elapsed();
                return report;
            }
        };
        // The network gate's image too: it can have been garbage-collected since `new()`.
        if let Some((image, digest)) = &self.gate_image {
            if let Err(f) = digest_gate(image, digest) {
                report.result = Err(K8sError::ImageGate(f));
                report.elapsed = started.elapsed();
                return report;
            }
        }
        let scratch = self.scratch_root.join(format!("av-job-{}-{suffix}", dns_fragment(&spec.job_id, 24)));
        let mut guard = RunGuard { api: self.api.clone(), namespace: self.namespace.clone(), job_name, job_may_exist: false, scratch };
        let result = self.drive(spec, inputs, &image_ref, &mut guard, &mut report);
        report.cleanup = guard.teardown();
        report.result = result;
        report.elapsed = started.elapsed();
        report
    }

    fn drive(&self, spec: &pb::JobSpec, inputs: &[JobInput], image_ref: &str, guard: &mut RunGuard, report: &mut RunReport) -> Result<Vec<JobOutput>, K8sError> {
        let in_dir = guard.scratch.join("in");
        let out_dir = guard.scratch.join("out");
        fs::create_dir_all(&in_dir).map_err(|e| K8sError::Scratch(format!("creating {in_dir:?}: {e}")))?;
        fs::create_dir_all(&out_dir).map_err(|e| K8sError::Scratch(format!("creating {out_dir:?}: {e}")))?;
        // 0o777: the fixed non-root uid the pod runs as must be able to write here whichever host
        // user created it (the same reason as ContainerExecutor).
        fs::set_permissions(&out_dir, fs::Permissions::from_mode(0o777)).map_err(|e| K8sError::Scratch(format!("chmod {out_dir:?}: {e}")))?;
        for (i, input) in inputs.iter().enumerate() {
            let path = in_dir.join(format!("{i:03}"));
            fs::write(&path, &input.bytes).map_err(|e| K8sError::Scratch(format!("writing {path:?}: {e}")))?;
        }

        let manifest = build_job(&JobParams {
            job_name: &guard.job_name,
            namespace: &self.namespace,
            job_id: &spec.job_id,
            image_ref,
            command: &spec.command,
            node_name: &self.node_name,
            in_dir: &in_dir,
            out_dir: &out_dir,
            extra_labels: &self.extra_labels,
            resources: &self.resources,
            deadline_seconds: self.deadline_seconds,
            termination_grace_seconds: self.termination_grace_seconds,
            ttl_after_finished_seconds: self.ttl_after_finished_seconds,
            network_gate: self.network_gate.as_ref(),
        });
        // Set before the request: if the reply is lost the Job may exist anyway, and the guard
        // must try to delete it. A definitive refusal from the API server (403, 409, 422, ...)
        // means no Job of ours was created -- and for a 409 the name is somebody else's, which
        // must never be deleted.
        guard.job_may_exist = true;
        if let Err(e) = self.api.create_job(&self.namespace, &manifest) {
            if matches!(e, ApiError::Refused(_)) {
                guard.job_may_exist = false;
            }
            return Err(api_err("create job", e));
        }

        let decision = self.poll_until_decided(guard, report);

        // The log, if the terminal pod has not already given it up: best effort, once.
        if report.log.is_none() && report.log_error.is_none() {
            if let Some(name) = report.pods.first().cloned() {
                self.capture_log(&name, report);
            }
        }
        if let Some(pod) = &report.final_pod {
            report.pod_times = Some(pod_times(pod));
        }
        decision?;

        let mut paths: Vec<PathBuf> = fs::read_dir(&out_dir)
            .map_err(|e| K8sError::Output(format!("reading {out_dir:?}: {e}")))?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|p| p.is_file())
            .collect();
        paths.sort();
        let mut outputs = Vec::with_capacity(paths.len());
        for path in paths {
            let bytes = fs::read(&path).map_err(|e| K8sError::Output(format!("reading {path:?}: {e}")))?;
            outputs.push(JobOutput { bytes, media_type: "application/octet-stream".to_string(), manifest: false });
        }
        Ok(outputs)
    }

    /// Polls Job and pods until [`assess`] decides, the control plane fails, or the bound passes.
    fn poll_until_decided(&self, guard: &RunGuard, report: &mut RunReport) -> Result<(), K8sError> {
        let bound = Duration::from_secs(self.deadline_seconds) + self.margin;
        let begun = Instant::now();
        let selector = format!("batch.kubernetes.io/job-name={}", guard.job_name);
        let mut tracks: BTreeMap<String, PodTrack> = BTreeMap::new();
        let mut consecutive_errors = 0u32;
        loop {
            // Job first, then pods: the pods are then at least as new as the Job, so a Job that
            // already says Failed is read together with the pod state that led to it.
            let polled = self
                .api
                .get_job(&self.namespace, &guard.job_name)
                .map_err(|e| if e.is_not_found() { K8sError::JobDeleted } else { api_err("get job", e) })
                .and_then(|job| self.api.list_pods(&self.namespace, &selector).map(|pods| (job, pods)).map_err(|e| api_err("list pods", e)));
            match polled {
                Ok((job, pods)) => {
                    consecutive_errors = 0;
                    let listed: BTreeSet<String> = pods.iter().map(|p| pod_name(p).to_string()).collect();
                    for track in tracks.values_mut() {
                        if !listed.contains(pod_name(&track.last)) {
                            track.gone = true;
                        }
                    }
                    for pod in pods {
                        tracks.insert(pod_name(&pod).to_string(), PodTrack { last: pod, gone: false });
                    }
                    report.pods = tracks.keys().cloned().collect();
                    report.final_job = Some(job.clone());
                    report.final_pod = tracks.values().next().map(|t| t.last.clone());
                    if report.log.is_none() && report.log_error.is_none() {
                        if let Some(t) = tracks.values().find(|t| !t.gone && pod_is_terminal(&t.last)) {
                            let name = pod_name(&t.last).to_string();
                            self.capture_log(&name, report);
                        }
                    }
                    if let Some(decision) = assess(&job, &tracks) {
                        return decision;
                    }
                }
                Err(K8sError::ApiUnreachable { op, detail }) => {
                    consecutive_errors += 1;
                    if consecutive_errors > self.poll_error_budget {
                        return Err(K8sError::ApiUnreachable { op, detail });
                    }
                }
                Err(e) => return Err(e),
            }
            if begun.elapsed() >= bound {
                return Err(assess_at_bound(&tracks, &format!("{} s deadline + {} s margin", self.deadline_seconds, self.margin.as_secs())));
            }
            std::thread::sleep(self.poll_interval);
        }
    }

    fn capture_log(&self, pod: &str, report: &mut RunReport) {
        // A pod whose network gate failed never started the workload: its reason is in the gate's log.
        let container = report.final_pod.as_ref().and_then(status::init_failure).map(|(name, _)| name).unwrap_or_else(|| CONTAINER_NAME.to_string());
        match self.api.pod_log(&self.namespace, pod, &container,self.log_max_lines as u64 + 1, self.log_cap_bytes as u64 + 1) {
            Ok(bytes) => report.log = Some(shape_log(pod, &bytes, self.log_cap_bytes, self.log_max_lines)),
            Err(e) => report.log_error = Some(format!("pod {pod}: {e}")),
        }
    }
}

/// Applies the caps: more than `max_lines` lines keeps the last `max_lines`; more than `cap_bytes`
/// bytes keeps the first `cap_bytes` (the API's `limitBytes` keeps the start of the tail window).
/// A marker line says which happened.
fn shape_log(pod: &str, raw: &[u8], cap_bytes: usize, max_lines: usize) -> LogCapture {
    if raw.len() > cap_bytes {
        let mut cut = cap_bytes;
        while cut > 0 && (raw[cut] & 0xC0) == 0x80 {
            cut -= 1;
        }
        let mut text = String::from_utf8_lossy(&raw[..cut]).into_owned();
        text.push_str(&format!("\n[av: log truncated at {cap_bytes} bytes (of the last {max_lines} lines)]\n"));
        return LogCapture { pod: pod.to_string(), text, truncated: true };
    }
    let text = String::from_utf8_lossy(raw).into_owned();
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    if lines.len() > max_lines {
        let kept: String = lines[lines.len() - max_lines..].concat();
        return LogCapture { pod: pod.to_string(), text: format!("[av: log truncated: only the last {max_lines} lines kept]\n{kept}"), truncated: true };
    }
    LogCapture { pod: pod.to_string(), text, truncated: false }
}

fn pod_times(pod: &Value) -> PodTimes {
    let t = terminated(pod);
    PodTimes {
        pod_start_time: pod["status"]["startTime"].as_str().map(str::to_string),
        container_started_at: t.as_ref().and_then(|t| t.started_at.clone()).or_else(|| {
            pod["status"]["containerStatuses"][0]["state"]["running"]["startedAt"].as_str().map(str::to_string)
        }),
        container_finished_at: t.and_then(|t| t.finished_at),
    }
}

/// Makes sure the namespace carries the default-deny `NetworkPolicy` ([`network_policy`]): creates
/// it if absent, accepts an existing one with the same spec, and **refuses, typed, a same-named
/// policy that differs** (an operator's allow rule must not be silently overwritten, nor silently
/// trusted as deny). Two executors starting at once may both try to create it: the loser's 409 is
/// resolved by reading the winner's.
fn ensure_network_policy(api: &K8sApi, namespace: &str) -> Result<(), K8sError> {
    let desired = network_policy(namespace);
    let check = |existing: &Value| {
        if network_policy_matches(existing, &desired) {
            Ok(())
        } else {
            Err(K8sError::Preflight(format!(
                "NetworkPolicy {NETWORK_POLICY_NAME:?} already exists in namespace {namespace:?} but is not the default-deny the executor needs \
                 (podSelector av.job=1, policyTypes Ingress+Egress, no rules): existing spec {}",
                existing["spec"]
            )))
        }
    };
    match api.get_network_policy(namespace, NETWORK_POLICY_NAME) {
        Ok(existing) => check(&existing),
        Err(e) if e.is_not_found() => match api.create_network_policy(namespace, &desired) {
            Ok(_) => Ok(()),
            Err(e) if e.is_conflict() => check(&api.get_network_policy(namespace, NETWORK_POLICY_NAME).map_err(|e| api_err("get network policy", e))?),
            Err(e) => Err(api_err("create network policy", e)),
        },
        Err(e) => Err(api_err("get network policy", e)),
    }
}

/// Picks the node: the named one (it must exist), or the only one (more than one is refused).
/// The node must be `Ready` and run Docker.
fn select_node(nodes: &[Value], named: Option<&str>) -> Result<String, K8sError> {
    let names: Vec<&str> = nodes.iter().filter_map(|n| n["metadata"]["name"].as_str()).collect();
    let node = match named {
        Some(want) => nodes
            .iter()
            .find(|n| n["metadata"]["name"].as_str() == Some(want))
            .ok_or_else(|| K8sError::Preflight(format!("node {want:?} is not in the cluster (nodes: {names:?})")))?,
        None => match nodes {
            [] => return Err(K8sError::Preflight("the cluster has no nodes".to_string())),
            [only] => only,
            _ => {
                return Err(K8sError::Preflight(format!(
                    "the cluster has {} nodes ({names:?}) and none was named; the hostPath data plane is single-node, so the node must be named",
                    nodes.len()
                )))
            }
        },
    };
    let name = node["metadata"]["name"].as_str().unwrap_or_default().to_string();
    let ready = node["status"]["conditions"].as_array().is_some_and(|cs| cs.iter().any(|c| c["type"] == "Ready" && c["status"] == "True"));
    if !ready {
        return Err(K8sError::Preflight(format!("node {name:?} is not Ready")));
    }
    let runtime = node["status"]["nodeInfo"]["containerRuntimeVersion"].as_str().unwrap_or_default();
    if !runtime.starts_with("docker://") {
        return Err(K8sError::Preflight(format!(
            "node {name:?} runs {runtime:?}, not Docker; the image gate checks the local Docker, so it proves nothing about this node's images"
        )));
    }
    Ok(name)
}

/// Creates and canonicalises the scratch root and requires it to be under (the canonical) `$HOME`.
fn canonical_scratch_root(root: &Path) -> Result<PathBuf, K8sError> {
    fs::create_dir_all(root).map_err(|e| K8sError::Preflight(format!("creating scratch root {root:?}: {e}")))?;
    let canonical = fs::canonicalize(root).map_err(|e| K8sError::Preflight(format!("resolving scratch root {root:?}: {e}")))?;
    let home = std::env::var_os("HOME").ok_or_else(|| K8sError::Preflight("$HOME is not set; cannot check the scratch root is shared with the node".to_string()))?;
    let home = fs::canonicalize(&home).map_err(|e| K8sError::Preflight(format!("resolving $HOME {home:?}: {e}")))?;
    if !canonical.starts_with(&home) {
        return Err(K8sError::Preflight(format!(
            "scratch root {canonical:?} is outside $HOME ({home:?}); only $HOME is mounted into the node, so a hostPath there would silently be an empty directory"
        )));
    }
    Ok(canonical)
}

/// Removes the Job and the scratch directory on every path: explicitly via [`RunGuard::teardown`]
/// (whose result goes in the report), and on drop as a backstop for panics and early returns.
struct RunGuard {
    api: K8sApi,
    namespace: String,
    job_name: String,
    job_may_exist: bool,
    scratch: PathBuf,
}

impl RunGuard {
    /// Deletes the Job (Foreground), waits for it to be gone (so its pod is), removes the scratch.
    fn teardown(&mut self) -> Result<(), String> {
        let mut problems: Vec<String> = Vec::new();
        if self.job_may_exist {
            match self.api.delete_job(&self.namespace, &self.job_name) {
                Ok(()) => {}
                Err(e) if e.is_not_found() => {}
                Err(e) => problems.push(format!("deleting Job {}: {e}", self.job_name)),
            }
            let patience = Instant::now() + Duration::from_secs(60);
            loop {
                match self.api.get_job(&self.namespace, &self.job_name) {
                    Err(e) if e.is_not_found() => break,
                    Ok(_) => {}
                    Err(e) => {
                        problems.push(format!("waiting for Job {} to go: {e}", self.job_name));
                        break;
                    }
                }
                if Instant::now() > patience {
                    problems.push(format!("Job {} still present 60 s after deletion", self.job_name));
                    break;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            if problems.is_empty() {
                self.job_may_exist = false;
            }
        }
        match fs::remove_dir_all(&self.scratch) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => problems.push(format!("removing {:?}: {e}", self.scratch)),
        }
        if problems.is_empty() {
            Ok(())
        } else {
            Err(problems.join("; "))
        }
    }
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        if self.job_may_exist {
            let _ = self.api.delete_job(&self.namespace, &self.job_name);
        }
        let _ = fs::remove_dir_all(&self.scratch);
    }
}

impl Executor for K8sJobExecutor {
    fn execute(&self, spec: &pb::JobSpec, inputs: &[JobInput]) -> Result<Vec<JobOutput>, pb::JobFailure> {
        self.run(spec, inputs).into_executor_result()
    }
}

/// The container-side mount points, re-exported so a caller writing a command need not reach into
/// [`crate::container`].
pub const INPUT_DIR: &str = CONTAINER_INPUT_DIR;
pub const OUTPUT_DIR: &str = CONTAINER_OUTPUT_DIR;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn node(name: &str, ready: bool, runtime: &str) -> Value {
        json!({"metadata": {"name": name}, "status": {"conditions": [{"type": "Ready", "status": if ready {"True"} else {"False"}}], "nodeInfo": {"containerRuntimeVersion": runtime}}})
    }

    #[test]
    fn the_executor_is_send_and_sync_so_threads_can_share_it() {
        fn is<T: Send + Sync>() {}
        is::<K8sJobExecutor>();
    }

    #[test]
    fn one_node_is_chosen_two_are_refused_unless_one_is_named() {
        assert_eq!(select_node(&[node("colima", true, "docker://29.5.2")], None).unwrap(), "colima");
        let two = [node("a", true, "docker://29"), node("b", true, "docker://29")];
        match select_node(&two, None) {
            Err(K8sError::Preflight(m)) => assert!(m.contains("2 nodes") && m.contains("must be named"), "{m}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(select_node(&two, Some("b")).unwrap(), "b");
        assert!(matches!(select_node(&two, Some("c")), Err(K8sError::Preflight(_))));
        assert!(matches!(select_node(&[], None), Err(K8sError::Preflight(_))));
    }

    #[test]
    fn a_node_that_is_not_ready_or_not_docker_is_refused() {
        assert!(matches!(select_node(&[node("n", false, "docker://29")], None), Err(K8sError::Preflight(m)) if m.contains("not Ready")));
        assert!(matches!(select_node(&[node("n", true, "containerd://2.0")], None), Err(K8sError::Preflight(m)) if m.contains("not Docker")));
    }

    /// A parseable kubeconfig whose PEM is not real: enough for `K8sJobExecutor::new`'s checks that
    /// happen before it connects (a real connection attempt then fails at `Client::new`).
    fn dummy_config() -> K8sConfig {
        let b64 = |s: &str| openssl::base64::encode_block(s.as_bytes());
        let cert = "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----\n";
        let key = "-----BEGIN EC PRIVATE KEY-----\nMHcC\n-----END EC PRIVATE KEY-----\n";
        let yaml = format!(
            "current-context: t\nclusters:\n- name: c\n  cluster:\n    server: https://127.0.0.1:6443\n    certificate-authority-data: {}\n\
             contexts:\n- name: t\n  context: {{cluster: c, user: u}}\nusers:\n- name: u\n  user:\n    client-certificate-data: {}\n    client-key-data: {}\n",
            b64(cert),
            b64(cert),
            b64(key)
        );
        K8sConfig::new(Kubeconfig::parse(&yaml, None, None).unwrap(), "ns", std::env::temp_dir())
    }

    #[test]
    fn no_gate_and_no_opt_in_is_refused_before_anything_connects() {
        let cfg = dummy_config();
        assert!(cfg.network_gate.is_none() && !cfg.accept_startup_network_window, "the defaults are the safe ones");
        match K8sJobExecutor::new(cfg) {
            Err(K8sError::Preflight(m)) => assert!(m.contains("accept_startup_network_window") && m.contains("first ~1-2 s run with network"), "{m}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_opt_in_gets_past_the_network_check_and_only_then_fails_to_connect() {
        let mut cfg = dummy_config();
        cfg.accept_startup_network_window = true;
        match K8sJobExecutor::new(cfg) {
            Err(K8sError::ApiUnreachable { op: "connect", .. }) => {}
            other => panic!("expected to get as far as connecting (and fail on the dummy PEM), got {other:?}"),
        }
    }

    #[test]
    fn a_gate_image_that_is_absent_or_under_the_wrong_digest_is_refused_by_the_digest_gate_before_connecting() {
        let mut cfg = dummy_config();
        cfg.network_gate = Some(NetworkGate {
            image: "av-jobs-no-such-image-for-the-network-gate-test".to_string(),
            image_digest: format!("sha256:{}", "0".repeat(64)),
            within: Duration::from_secs(5),
        });
        match K8sJobExecutor::new(cfg) {
            Err(K8sError::ImageGate(f)) => {
                assert_eq!(f.kind, pb::JobFailureKind::ExecutorUnavailable as i32, "{f:?}");
                assert!(f.detail.contains("not present locally"), "{}", f.detail);
            }
            other => panic!("{other:?}"),
        }
        // An empty digest is the gate's own start-failed refusal.
        let mut cfg = dummy_config();
        cfg.network_gate = Some(NetworkGate { image: "alpine".to_string(), image_digest: String::new(), within: Duration::from_secs(5) });
        assert!(matches!(K8sJobExecutor::new(cfg), Err(K8sError::ImageGate(f)) if f.kind == pb::JobFailureKind::ExecutorStartFailed as i32));
    }

    #[test]
    fn a_scratch_root_outside_home_is_refused() {
        let outside = std::env::temp_dir().join(format!("av-jobs-k8s-scratch-test-{}", std::process::id()));
        let home = fs::canonicalize(std::env::var_os("HOME").unwrap()).unwrap();
        let r = canonical_scratch_root(&outside);
        let _ = fs::remove_dir_all(&outside);
        if fs::canonicalize(std::env::temp_dir()).unwrap().starts_with(&home) {
            // $TMPDIR is under $HOME on this machine: nothing outside to test with.
            assert!(r.is_ok());
        } else {
            assert!(matches!(r, Err(K8sError::Preflight(m)) if m.contains("outside $HOME")));
        }
    }

    #[test]
    fn failures_map_to_existing_kinds_per_the_table() {
        use pb::JobFailureKind as K;
        let f = |e: K8sError| e.to_failure("j", &["p1".to_string()], Some("boom"));
        let cases: Vec<(K8sError, K, i32)> = vec![
            (K8sError::ExitedNonZero { code: 7, reason: "Error".into() }, K::NonzeroExit, 7),
            (K8sError::Killed { exit_code: 137, signal: Some(9), reason: "Error".into() }, K::NonzeroExit, 137),
            (K8sError::DeadlineExceeded { detail: "d".into(), exit_code: Some(143) }, K::NonzeroExit, 143),
            (K8sError::DeadlineExceeded { detail: "d".into(), exit_code: None }, K::NonzeroExit, -1),
            (K8sError::NeverStarted { reason: "ErrImageNeverPull".into(), message: "m".into() }, K::ExecutorStartFailed, 0),
            (K8sError::Scratch("s".into()), K::ExecutorStartFailed, 0),
            (K8sError::Output("o".into()), K::OutputRejected, 0),
            (K8sError::PodDeleted { pod: "p1".into(), container: "c".into() }, K::ExecutorUnavailable, 0),
            (K8sError::JobDeleted, K::ExecutorUnavailable, 0),
            (K8sError::Retried { pods: vec!["p1".into(), "p2".into()] }, K::ExecutorUnavailable, 0),
            (K8sError::Preflight("x".into()), K::ExecutorUnavailable, 0),
            (K8sError::ApiUnreachable { op: "get job", detail: "d".into() }, K::ExecutorUnavailable, 0),
            (K8sError::ApiRefused { op: "create job", status: ApiStatus { code: 403, reason: "Forbidden".into(), message: "no".into() } }, K::ExecutorUnavailable, 0),
        ];
        for (err, kind, code) in cases {
            let label = format!("{err:?}");
            let failure = f(err);
            assert_eq!((failure.kind, failure.exit_code), (kind as i32, code), "{label}");
            assert!(failure.detail.starts_with("K8S executor: job \"j\""), "{}", failure.detail);
            assert!(failure.detail.contains("p1") && failure.detail.contains("log tail: \"boom\""), "{}", failure.detail);
        }
        // The gate's refusal is passed through untouched.
        let gate = pb::JobFailure { kind: K::ExecutorUnavailable as i32, detail: "CONTAINER executor: gone".into(), exit_code: 0 };
        assert_eq!(f(K8sError::ImageGate(gate.clone())), gate);
    }

    #[test]
    fn the_log_is_capped_by_bytes_and_lines_with_a_marker() {
        let small = shape_log("p", b"a\nb\n", 100, 10);
        assert_eq!((small.text.as_str(), small.truncated), ("a\nb\n", false));
        let many = shape_log("p", b"1\n2\n3\n4\n", 100, 2);
        assert!(many.truncated && many.text.starts_with("[av: log truncated: only the last 2 lines kept]\n3\n4\n"), "{many:?}");
        let big = shape_log("p", &[b'x'; 50], 10, 5);
        assert!(big.truncated && big.text.starts_with("xxxxxxxxxx\n[av: log truncated at 10 bytes"), "{big:?}");
        // A cut inside a multi-byte character backs up to the boundary.
        let utf8 = shape_log("p", "aé".repeat(10).as_bytes(), 4, 5);
        assert!(utf8.text.starts_with("aé") && utf8.truncated, "{utf8:?}");
    }

    #[test]
    fn the_log_tail_is_bounded_in_lines_and_bytes() {
        let text: String = (0..40).map(|i| format!("line {i}\n")).collect();
        let tail = log_tail(&text);
        assert!(tail.starts_with("line 25") && tail.ends_with("line 39"), "{tail}");
        assert!(log_tail(&"y".repeat(5000)).len() <= 1500);
        assert_eq!(log_tail(""), "");
    }

    #[test]
    fn run_names_are_unique_and_valid_dns_labels() {
        let a = unique_suffix();
        let b = unique_suffix();
        assert_ne!(a, b);
        let name = format!("av-job-{}-{a}", dns_fragment("Some Job/ID", 24));
        assert!(name.len() <= 63 && name.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-'), "{name}");
    }
}
