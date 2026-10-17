//! Reading Job and Pod JSON: what happened to the one pod a job ever gets.
//!
//! Everything here is a pure function of API objects (`serde_json::Value`s), so it is unit-tested
//! against recorded JSON from this host's k3s (`tests/fixtures/k8s/`) and needs no cluster.
//!
//! # The decision, in order ([`assess`])
//!
//! 1. **More than one pod ever** (the poll saw two names, or the Job's own counters
//!    `failed + succeeded + active + uncounted` exceed one, see [`job_pod_count`]): [`K8sError::Retried`],
//!    whatever the second pod did. `backoffLimit: 0` should make this impossible; this is the check
//!    that notices if it is not, and a pod that came and went between two polls still shows in the
//!    Job's counters.
//! 2. The Job's `Complete` condition: success (a Complete Job whose container did not exit 0 is a
//!    contradiction and is reported, not trusted).
//! 3. A pod that cannot start (a waiting container in `ErrImageNeverPull`, `CreateContainerError`,
//!    ... or a pod that failed before any container started, e.g. `OutOfcpu`): [`K8sError::NeverStarted`],
//!    at once; the Job is not waited on.
//! 4. The Job's `FailureTarget` / `Failed` condition with reason `DeadlineExceeded`:
//!    [`K8sError::DeadlineExceeded`] (checked before the pod, because the Job controller kills the
//!    pod to enforce the deadline and that pod looks exactly like a deleted one).
//! 5. The Job's `Failed` condition otherwise: classify the pod ([`classify_failed_pod`]).
//! 6. Otherwise nothing is decided yet. A pod that has already failed while the Job is not yet
//!    `Failed` is **waited on**, not reported: the Job object is the authority that no retry is
//!    coming, and waiting is what lets a second pod (a `backoffLimit` above 0, which the Job
//!    controller starts after its own back-off of about ten seconds) be seen and refused.
//!
//! # Exit codes, signals and kills
//!
//! Kubernetes reports `exitCode`, `reason` (`Error`, `OOMKilled`, `Completed`, `StartError`,
//! ...), `signal` (only when the runtime reports one: this host's `cri-dockerd` does not) and
//! `message`. Following the shell convention, an exit code above 128 is read as death by signal
//! `code - 128` ([`K8sError::Killed`]); a program that itself calls `exit 137` is
//! indistinguishable from a SIGKILL and is reported as one. `OOMKilled` is always a kill.

use std::collections::BTreeMap;

use serde_json::Value;

use super::api::ApiStatus;
use super::kubeconfig::KubeconfigError;
use crate::k8s::manifest::CONTAINER_NAME;
use av_cdm::pb;

/// Waiting reasons that mean the container will not start without outside help. (Kubelet retries
/// some of them, e.g. `CreateContainerError`; for a batch job that must either run or fail
/// visibly, failing fast and naming the reason is the useful answer.)
pub const NEVER_STARTED_WAITING_REASONS: &[&str] = &[
    "ErrImageNeverPull",
    "ErrImagePull",
    "ImagePullBackOff",
    "InvalidImageName",
    "ImageInspectError",
    "RegistryUnavailable",
    "CreateContainerConfigError",
    "CreateContainerError",
    "RunContainerError",
];

/// Terminated reasons that mean the runtime could not start the process (a command that does not
/// exist or cannot be executed), as opposed to the process starting and exiting.
pub const START_FAILURE_TERMINATED_REASONS: &[&str] = &["StartError", "ContainerCannotRun", "CreateContainerError", "CreateContainerConfigError", "RunContainerError"];

/// The terminated state of the job's container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Terminated {
    pub exit_code: i32,
    pub reason: String,
    pub signal: Option<i32>,
    pub message: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
}

/// Why a job failed, in this executor's own terms. See the module doc of [`crate::k8s`] for the
/// table converting each to a `JobFailureKind`.
#[derive(Debug, thiserror::Error)]
pub enum K8sError {
    #[error("kubeconfig refused: {0}")]
    Kubeconfig(#[from] KubeconfigError),
    #[error("cluster preconditions not met: {0}")]
    Preflight(String),
    /// The image gate's own typed refusal (`EXECUTOR_UNAVAILABLE` / `EXECUTOR_START_FAILED`).
    #[error("image gate: {}", .0.detail)]
    ImageGate(pb::JobFailure),
    #[error("scratch directory problem: {0}")]
    Scratch(String),
    #[error("Kubernetes API unreachable during {op}: {detail}")]
    ApiUnreachable { op: &'static str, detail: String },
    #[error("Kubernetes API refused {op}: {status}")]
    ApiRefused { op: &'static str, status: ApiStatus },
    #[error("Kubernetes API sent an unusable reply to {op}: {detail}")]
    ApiBadReply { op: &'static str, detail: String },
    #[error("the container exited with status {code}")]
    ExitedNonZero { code: i32, reason: String },
    #[error("the container was killed (exit {exit_code}, signal {signal:?}, Kubernetes reason {reason:?})")]
    Killed { exit_code: i32, signal: Option<i32>, reason: String },
    #[error("the pod {pod:?} was deleted while the job ran (container state: {container})")]
    PodDeleted { pod: String, container: String },
    #[error("the Job object was deleted while the job ran")]
    JobDeleted,
    /// `exit_code` is the container's, if it had terminated by the time it was looked at.
    #[error("the job did not finish within its deadline ({detail})")]
    DeadlineExceeded { detail: String, exit_code: Option<i32> },
    #[error("the container never started: {reason}: {message}")]
    NeverStarted { reason: String, message: String },
    #[error("more than one pod existed for the job ({pods:?}); a Kubernetes retry is refused")]
    Retried { pods: Vec<String> },
    #[error("unexpected Job/Pod state: {0}")]
    Unexpected(String),
    #[error("reading the job's outputs: {0}")]
    Output(String),
}

/// The latest snapshot of one pod the poll has seen, and whether it has since disappeared.
#[derive(Debug, Clone)]
pub struct PodTrack {
    pub last: Value,
    pub gone: bool,
}

pub fn pod_name(pod: &Value) -> &str {
    pod["metadata"]["name"].as_str().unwrap_or_default()
}

pub fn pod_phase(pod: &Value) -> &str {
    pod["status"]["phase"].as_str().unwrap_or("Unknown")
}

fn job_container_status(pod: &Value) -> Option<&Value> {
    let all = pod["status"]["containerStatuses"].as_array()?;
    all.iter().find(|c| c["name"].as_str() == Some(CONTAINER_NAME)).or_else(|| all.first())
}

/// The job container's terminated state, if it has terminated.
pub fn terminated(pod: &Value) -> Option<Terminated> {
    let t = &job_container_status(pod)?["state"]["terminated"];
    if !t.is_object() {
        return None;
    }
    Some(Terminated {
        exit_code: t["exitCode"].as_i64().and_then(|c| i32::try_from(c).ok()).unwrap_or(-1),
        reason: t["reason"].as_str().unwrap_or_default().to_string(),
        signal: t["signal"].as_i64().and_then(|s| i32::try_from(s).ok()).filter(|s| *s != 0),
        message: t["message"].as_str().unwrap_or_default().to_string(),
        started_at: t["startedAt"].as_str().map(str::to_string),
        finished_at: t["finishedAt"].as_str().map(str::to_string),
    })
}

/// The job container's waiting `(reason, message)`, if it is waiting.
pub fn waiting(pod: &Value) -> Option<(String, String)> {
    let w = &job_container_status(pod)?["state"]["waiting"];
    if !w.is_object() {
        return None;
    }
    Some((w["reason"].as_str().unwrap_or_default().to_string(), w["message"].as_str().unwrap_or_default().to_string()))
}

impl Default for Terminated {
    fn default() -> Self {
        Terminated { exit_code: -1, reason: String::new(), signal: None, message: String::new(), started_at: None, finished_at: None }
    }
}

/// An init container (the network gate) that terminated non-zero: `(name, state)`.
pub fn init_failure(pod: &Value) -> Option<(String, Terminated)> {
    for c in pod["status"]["initContainerStatuses"].as_array()? {
        let t = &c["state"]["terminated"];
        if t["exitCode"].as_i64().is_some_and(|code| code != 0) {
            return Some((
                c["name"].as_str().unwrap_or_default().to_string(),
                Terminated {
                    exit_code: t["exitCode"].as_i64().and_then(|x| i32::try_from(x).ok()).unwrap_or(-1),
                    reason: t["reason"].as_str().unwrap_or_default().to_string(),
                    signal: None,
                    message: t["message"].as_str().unwrap_or_default().to_string(),
                    started_at: t["startedAt"].as_str().map(str::to_string),
                    finished_at: t["finishedAt"].as_str().map(str::to_string),
                },
            ));
        }
    }
    None
}

/// A waiting container, init or main, whose reason means it will not start by itself.
pub fn never_started_waiting(pod: &Value) -> Option<(String, String)> {
    let statuses = ["initContainerStatuses", "containerStatuses"].into_iter().filter_map(|k| pod["status"][k].as_array()).flatten();
    for c in statuses {
        let w = &c["state"]["waiting"];
        let reason = w["reason"].as_str().unwrap_or_default();
        if NEVER_STARTED_WAITING_REASONS.contains(&reason) {
            return Some((reason.to_string(), w["message"].as_str().unwrap_or_default().to_string()));
        }
    }
    None
}

/// Whether the pod's container has finished one way or another (the log is final).
pub fn pod_is_terminal(pod: &Value) -> bool {
    matches!(pod_phase(pod), "Succeeded" | "Failed") || terminated(pod).is_some()
}

/// A condition of the Job that is `True`: `(type, reason, message)` for the first of the given
/// types, in the order given.
pub fn job_condition<'a>(job: &'a Value, types: &[&str]) -> Option<(&'a str, &'a str, &'a str)> {
    let conditions = job["status"]["conditions"].as_array()?;
    for wanted in types {
        for c in conditions {
            if c["type"].as_str() == Some(wanted) && c["status"].as_str() == Some("True") {
                return Some((c["type"].as_str().unwrap_or_default(), c["reason"].as_str().unwrap_or_default(), c["message"].as_str().unwrap_or_default()));
            }
        }
    }
    None
}

/// How many pods the Job's own status says it has had: `failed + succeeded + active` plus the
/// pods still waiting to be counted. For a one-pod job this is 0 before the pod exists and 1
/// afterwards, forever. **`terminating` is deliberately not added**: on this cluster's real
/// deadline-exceeded Job (`tests/fixtures/k8s/deadline.job.json`) the single pod is `failed: 1`
/// and `terminating: 1` at once, so adding it double-counts (the first version of this function
/// did, and reported a retry for a job with one pod). A second pod is always visible as `active`
/// (or, once finished, `failed`/`succeeded`), and `podReplacementPolicy: Failed` means the
/// controller starts none while the first is still terminating.
pub fn job_pod_count(job: &Value) -> u64 {
    let s = &job["status"];
    let n = |k: &str| s[k].as_u64().unwrap_or(0);
    let uncounted = |k: &str| s["uncountedTerminatedPods"][k].as_array().map_or(0, |a| a.len() as u64);
    n("failed") + n("succeeded") + n("active") + uncounted("failed") + uncounted("succeeded")
}

fn container_summary(pod: &Value) -> String {
    match (terminated(pod), waiting(pod)) {
        (Some(t), _) => format!("terminated exit {} reason {:?}", t.exit_code, t.reason),
        (None, Some((r, _))) => format!("waiting {r:?}"),
        _ => format!("phase {}", pod_phase(pod)),
    }
}

/// What a failed Job's pod tells: see the module doc. `pod` is `None` when no pod was ever seen.
pub fn classify_failed_pod(pod: Option<&PodTrack>) -> K8sError {
    let Some(track) = pod else {
        return K8sError::Unexpected("the Job failed but no pod was ever observed".to_string());
    };
    let p = &track.last;
    let name = pod_name(p).to_string();
    let t = terminated(p);
    if p["metadata"]["deletionTimestamp"].is_string() || (track.gone && t.is_none()) {
        return K8sError::PodDeleted { pod: name, container: container_summary(p) };
    }
    match t {
        Some(t) if START_FAILURE_TERMINATED_REASONS.contains(&t.reason.as_str()) => K8sError::NeverStarted { reason: t.reason, message: t.message },
        Some(t) if t.reason == "OOMKilled" || t.exit_code > 128 || t.signal.is_some() => {
            let signal = t.signal.or_else(|| (t.exit_code > 128).then_some(t.exit_code - 128));
            K8sError::Killed { exit_code: t.exit_code, signal, reason: t.reason }
        }
        Some(t) if t.exit_code != 0 => K8sError::ExitedNonZero { code: t.exit_code, reason: t.reason },
        Some(t) => K8sError::Unexpected(format!("the Job failed but its container exited 0 (reason {:?}, message {:?})", t.reason, t.message)),
        None if init_failure(p).is_some() => {
            let (name, t) = init_failure(p).unwrap_or_default();
            K8sError::NeverStarted { reason: format!("InitContainerFailed({name})"), message: format!("exit {} {}: {}", t.exit_code, t.reason, t.message) }
        }
        None => {
            let reason = p["status"]["reason"].as_str().filter(|r| !r.is_empty()).unwrap_or("PodFailedBeforeStart").to_string();
            let message = p["status"]["message"].as_str().unwrap_or_default().to_string();
            K8sError::NeverStarted { reason, message }
        }
    }
}

/// One step of the decision in the module doc: `None` while nothing is decided yet.
pub fn assess(job: &Value, pods: &BTreeMap<String, PodTrack>) -> Option<Result<(), K8sError>> {
    let counted = job_pod_count(job);
    if pods.len() > 1 || counted > 1 {
        return Some(Err(K8sError::Retried { pods: pods.keys().cloned().collect() }));
    }
    let track = pods.values().next();

    if job_condition(job, &["Complete"]).is_some() {
        return Some(match track.and_then(|t| terminated(&t.last)) {
            Some(t) if t.exit_code != 0 => Err(K8sError::Unexpected(format!("the Job is Complete but its container exited {}", t.exit_code))),
            _ => Ok(()),
        });
    }

    if let Some(track) = track {
        if !track.gone {
            if let Some((reason, message)) = never_started_waiting(&track.last) {
                return Some(Err(K8sError::NeverStarted { reason, message }));
            }
            if pod_phase(&track.last) == "Failed" && terminated(&track.last).is_none() {
                return Some(Err(classify_failed_pod(Some(track))));
            }
        }
    }

    if let Some((_, reason, message)) = job_condition(job, &["FailureTarget", "Failed"]) {
        if reason == "DeadlineExceeded" {
            let exit_code = track.and_then(|t| terminated(&t.last)).map(|t| t.exit_code);
            return Some(Err(K8sError::DeadlineExceeded { detail: message.to_string(), exit_code }));
        }
    }
    if job_condition(job, &["Failed"]).is_some() {
        return Some(Err(classify_failed_pod(track)));
    }
    None
}

/// The decision when the executor's own poll bound is reached with the Job still undecided: a pod
/// that has failed is reported as itself, anything else is a deadline overrun.
pub fn assess_at_bound(pods: &BTreeMap<String, PodTrack>, bound_description: &str) -> K8sError {
    match pods.values().next() {
        Some(t) if pod_is_terminal(&t.last) && terminated(&t.last).is_some_and(|x| x.exit_code != 0) => classify_failed_pod(Some(t)),
        _ => K8sError::DeadlineExceeded { detail: format!("no outcome by the executor's poll bound ({bound_description})"), exit_code: None },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn track(pod: Value) -> BTreeMap<String, PodTrack> {
        BTreeMap::from([(pod_name(&pod).to_string(), PodTrack { last: pod, gone: false })])
    }

    fn pod(name: &str, phase: &str, state: Value) -> Value {
        json!({"metadata": {"name": name}, "status": {"phase": phase, "containerStatuses": [{"name": "job", "state": state}]}})
    }

    fn job(conditions: Value, counts: Value) -> Value {
        let mut status = counts;
        status["conditions"] = conditions;
        json!({"status": status})
    }

    fn cond(t: &str, reason: &str) -> Value {
        json!([{"type": t, "status": "True", "reason": reason, "message": format!("{t}/{reason}")}])
    }

    fn terminated_state(code: i64, reason: &str) -> Value {
        json!({"terminated": {"exitCode": code, "reason": reason, "message": "m", "startedAt": "2026-10-17T00:00:01Z", "finishedAt": "2026-10-17T00:00:02Z"}})
    }

    #[test]
    fn nothing_is_decided_while_the_pod_runs() {
        let p = track(pod("p1", "Running", json!({"running": {}})));
        assert!(assess(&job(json!([]), json!({"active": 1})), &p).is_none());
        assert!(assess(&json!({}), &BTreeMap::new()).is_none());
    }

    #[test]
    fn a_complete_job_is_success_unless_the_container_disagrees() {
        let ok = track(pod("p1", "Succeeded", terminated_state(0, "Completed")));
        assert!(matches!(assess(&job(cond("Complete", ""), json!({"succeeded": 1})), &ok), Some(Ok(()))));
        let bad = track(pod("p1", "Succeeded", terminated_state(3, "Error")));
        assert!(matches!(assess(&job(cond("Complete", ""), json!({"succeeded": 1})), &bad), Some(Err(K8sError::Unexpected(_)))));
    }

    #[test]
    fn a_failed_pod_is_waited_on_until_the_job_says_failed() {
        let p = track(pod("p1", "Failed", terminated_state(7, "Error")));
        assert!(assess(&job(json!([]), json!({"failed": 1})), &p).is_none(), "the Job is not Failed yet: wait, a retry might be coming");
        match assess(&job(cond("Failed", "BackoffLimitExceeded"), json!({"failed": 1})), &p) {
            Some(Err(K8sError::ExitedNonZero { code: 7, .. })) => {}
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn exit_codes_above_128_and_oom_are_kills_with_the_signal_derived() {
        let failed = job(cond("Failed", "BackoffLimitExceeded"), json!({"failed": 1}));
        match assess(&failed, &track(pod("p1", "Failed", terminated_state(137, "Error")))) {
            Some(Err(K8sError::Killed { exit_code: 137, signal: Some(9), .. })) => {}
            other => panic!("{other:?}"),
        }
        match assess(&failed, &track(pod("p1", "Failed", terminated_state(1, "OOMKilled")))) {
            Some(Err(K8sError::Killed { reason, .. })) => assert_eq!(reason, "OOMKilled"),
            other => panic!("{other:?}"),
        }
        let with_signal = json!({"terminated": {"exitCode": 1, "reason": "Error", "signal": 15}});
        match assess(&failed, &track(pod("p1", "Failed", with_signal))) {
            Some(Err(K8sError::Killed { signal: Some(15), .. })) => {}
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_pod_with_a_deletion_timestamp_or_a_vanished_pod_is_deleted_not_failed() {
        let failed = job(cond("Failed", "BackoffLimitExceeded"), json!({"failed": 1}));
        let mut p = pod("p1", "Running", json!({"running": {}}));
        p["metadata"]["deletionTimestamp"] = json!("2026-10-17T00:00:03Z");
        assert!(matches!(assess(&failed, &track(p)), Some(Err(K8sError::PodDeleted { .. }))));
        let gone = BTreeMap::from([("p1".to_string(), PodTrack { last: pod("p1", "Running", json!({"running": {}})), gone: true })]);
        assert!(matches!(assess(&failed, &gone), Some(Err(K8sError::PodDeleted { .. }))));
        assert!(matches!(assess(&failed, &BTreeMap::new()), Some(Err(K8sError::Unexpected(_)))));
    }

    #[test]
    fn the_deadline_is_recognised_before_the_pod_the_controller_killed_for_it() {
        let mut p = pod("p1", "Running", json!({"running": {}}));
        p["metadata"]["deletionTimestamp"] = json!("2026-10-17T00:00:03Z");
        for t in ["FailureTarget", "Failed"] {
            match assess(&job(cond(t, "DeadlineExceeded"), json!({"failed": 1})), &track(p.clone())) {
                Some(Err(K8sError::DeadlineExceeded { .. })) => {}
                other => panic!("{t}: {other:?}"),
            }
        }
        // FailureTarget for any other reason is not yet an outcome: wait for Failed.
        assert!(assess(&job(cond("FailureTarget", "BackoffLimitExceeded"), json!({"active": 1})), &track(pod("p1", "Running", json!({"running": {}})))).is_none());
    }

    #[test]
    fn a_container_that_cannot_start_is_never_started_at_once() {
        for reason in NEVER_STARTED_WAITING_REASONS {
            let p = track(pod("p1", "Pending", json!({"waiting": {"reason": reason, "message": "no"}})));
            match assess(&job(json!([]), json!({"active": 1})), &p) {
                Some(Err(K8sError::NeverStarted { reason: r, .. })) => assert_eq!(r, *reason),
                other => panic!("{reason}: {other:?}"),
            }
        }
        // Waiting in ContainerCreating is just slow.
        assert!(assess(&job(json!([]), json!({"active": 1})), &track(pod("p1", "Pending", json!({"waiting": {"reason": "ContainerCreating"}})))).is_none());
        // A pod the kubelet rejected at admission.
        let rejected = json!({"metadata": {"name": "p1"}, "status": {"phase": "Failed", "reason": "OutOfcpu", "message": "Node didn't have enough resource: cpu"}});
        match assess(&job(json!([]), json!({"failed": 1})), &track(rejected)) {
            Some(Err(K8sError::NeverStarted { reason, .. })) => assert_eq!(reason, "OutOfcpu"),
            other => panic!("{other:?}"),
        }
        // A runtime that could not exec the command.
        let failed = job(cond("Failed", "BackoffLimitExceeded"), json!({"failed": 1}));
        match assess(&failed, &track(pod("p1", "Failed", terminated_state(128, "StartError")))) {
            Some(Err(K8sError::NeverStarted { reason, .. })) => assert_eq!(reason, "StartError"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_second_pod_or_a_job_counter_above_one_is_a_retry_whatever_it_did() {
        let two = BTreeMap::from([
            ("p1".to_string(), PodTrack { last: pod("p1", "Failed", terminated_state(137, "Error")), gone: false }),
            ("p2".to_string(), PodTrack { last: pod("p2", "Running", json!({"running": {}})), gone: false }),
        ]);
        match assess(&job(json!([]), json!({"failed": 1, "active": 1})), &two) {
            Some(Err(K8sError::Retried { pods })) => assert_eq!(pods, vec!["p1", "p2"]),
            other => panic!("{other:?}"),
        }
        // Only one pod name was ever polled, but the Job's own counters saw two.
        let one = track(pod("p2", "Succeeded", terminated_state(0, "Completed")));
        assert!(matches!(assess(&job(cond("Complete", ""), json!({"failed": 1, "succeeded": 1})), &one), Some(Err(K8sError::Retried { .. }))));
        let uncounted = json!({"status": {"failed": 1, "uncountedTerminatedPods": {"failed": ["uid-2"]}}});
        assert_eq!(job_pod_count(&uncounted), 2);
        assert_eq!(job_pod_count(&json!({"status": {"active": 1}})), 1);
        assert_eq!(job_pod_count(&json!({})), 0);
    }

    #[test]
    fn the_poll_bound_reports_a_failed_pod_as_itself_and_anything_else_as_a_deadline() {
        let failed = track(pod("p1", "Failed", terminated_state(7, "Error")));
        assert!(matches!(assess_at_bound(&failed, "x"), K8sError::ExitedNonZero { code: 7, .. }));
        let running = track(pod("p1", "Running", json!({"running": {}})));
        assert!(matches!(assess_at_bound(&running, "x"), K8sError::DeadlineExceeded { .. }));
        assert!(matches!(assess_at_bound(&BTreeMap::new(), "x"), K8sError::DeadlineExceeded { .. }));
    }
}
