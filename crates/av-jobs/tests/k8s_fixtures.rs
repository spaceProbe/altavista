//! Question 245, heavy-ct-2a: `av_jobs::k8s::status` replayed against Job and Pod JSON that this
//! host's k3s (v1.35, Docker runtime) really produced. No cluster, no Docker: the files under
//! `tests/fixtures/k8s/` were recorded by `tests/k8s.rs` with `AV_K8S_FIXTURE_DIR` set (see that
//! file's module doc), one `<scenario>.job.json` and `<scenario>.pod.json` per scenario, taken at the
//! last poll before the executor decided.
//!
//! What the recordings taught, kept here as assertions: a docker-killed pod and a pod deleted through
//! the API both end `exit 137, reason Error` (only `deletionTimestamp` tells them apart); a missing
//! command is `exit 127, reason ContainerCannotRun`; the Job controller kills the pod to enforce a
//! deadline (the pod is still `Running` with a `deletionTimestamp`, the Job `FailureTarget` with reason
//! `DeadlineExceeded`) and at that moment the single pod is `failed: 1` AND `terminating: 1`.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use av_cdm::pb;
use av_jobs::k8s::status::{assess, job_condition, job_pod_count, pod_name, pod_phase, terminated, waiting, K8sError, PodTrack};
use serde_json::{json, Value};

fn load(scenario: &str, kind: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/k8s/{scenario}.{kind}.json"));
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("could not read fixture {path:?}: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("fixture {path:?} is not JSON: {e}"))
}

fn tracks(pod: &Value) -> BTreeMap<String, PodTrack> {
    BTreeMap::from([(pod_name(pod).to_string(), PodTrack { last: pod.clone(), gone: false })])
}

fn decide(scenario: &str) -> Result<(), K8sError> {
    let (job, pod) = (load(scenario, "job"), load(scenario, "pod"));
    assess(&job, &tracks(&pod)).unwrap_or_else(|| panic!("{scenario}: no decision from a recorded terminal state"))
}

fn failure(e: &K8sError) -> pb::JobFailure {
    e.to_failure("job", &[], None)
}

#[test]
fn a_recorded_success_is_success_with_one_pod_counted() {
    let (job, pod) = (load("success", "job"), load("success", "pod"));
    assert!(job_condition(&job, &["Complete"]).is_some());
    assert_eq!(job_pod_count(&job), 1);
    assert_eq!(pod_phase(&pod), "Succeeded");
    assert_eq!(terminated(&pod).unwrap().exit_code, 0);
    assert!(matches!(decide("success"), Ok(())));
}

#[test]
fn a_recorded_non_zero_exit_is_exit_7() {
    let (job, pod) = (load("exit7", "job"), load("exit7", "pod"));
    assert_eq!(job_condition(&job, &["Failed"]).map(|c| c.1), Some("BackoffLimitExceeded"));
    assert_eq!(job_pod_count(&job), 1);
    let t = terminated(&pod).unwrap();
    assert_eq!((t.exit_code, t.reason.as_str()), (7, "Error"));
    let err = decide("exit7").unwrap_err();
    assert!(matches!(err, K8sError::ExitedNonZero { code: 7, .. }), "{err:?}");
    let f = failure(&err);
    assert_eq!((f.kind, f.exit_code), (pb::JobFailureKind::NonzeroExit as i32, 7));
}

#[test]
fn a_recorded_missing_command_is_a_container_that_never_started() {
    let t = terminated(&load("missing_command", "pod")).unwrap();
    assert_eq!((t.exit_code, t.reason.as_str()), (127, "ContainerCannotRun"));
    assert!(t.message.contains("no such file or directory"), "{}", t.message);
    let err = decide("missing_command").unwrap_err();
    match &err {
        K8sError::NeverStarted { reason, message } => {
            assert_eq!(reason, "ContainerCannotRun");
            assert!(message.contains("/no/such/binary-av-k8s-test"), "{message}");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(failure(&err).kind, pb::JobFailureKind::ExecutorStartFailed as i32);
}

#[test]
fn a_recorded_docker_kill_is_killed_by_sigkill_and_a_recorded_api_delete_is_a_deleted_pod() {
    // Same exit state ...
    let killed = terminated(&load("docker_kill", "pod")).unwrap();
    let deleted = terminated(&load("pod_deleted", "pod")).unwrap();
    assert_eq!((killed.exit_code, killed.reason.as_str()), (137, "Error"));
    assert_eq!((deleted.exit_code, deleted.reason.as_str()), (137, "Error"));
    // ... told apart only by the deletion timestamp.
    assert!(load("docker_kill", "pod")["metadata"]["deletionTimestamp"].is_null());
    assert!(load("pod_deleted", "pod")["metadata"]["deletionTimestamp"].is_string());

    let err = decide("docker_kill").unwrap_err();
    assert!(matches!(err, K8sError::Killed { exit_code: 137, signal: Some(9), .. }), "{err:?}");
    let f = failure(&err);
    assert_eq!((f.kind, f.exit_code), (pb::JobFailureKind::NonzeroExit as i32, 137));

    let err = decide("pod_deleted").unwrap_err();
    assert!(matches!(err, K8sError::PodDeleted { .. }), "{err:?}");
    assert_eq!(failure(&err).kind, pb::JobFailureKind::ExecutorUnavailable as i32);

    // Neither shows a second pod.
    assert_eq!(job_pod_count(&load("docker_kill", "job")), 1);
    assert_eq!(job_pod_count(&load("pod_deleted", "job")), 1);
}

#[test]
fn a_recorded_deadline_is_a_deadline_and_its_one_pod_is_not_mistaken_for_two() {
    let (job, pod) = (load("deadline", "job"), load("deadline", "pod"));
    assert_eq!(job_condition(&job, &["FailureTarget"]).map(|c| c.1), Some("DeadlineExceeded"));
    // The pod the controller is killing for the deadline looks like a deleted one ...
    assert!(pod["metadata"]["deletionTimestamp"].is_string());
    assert_eq!(pod_phase(&pod), "Running");
    // ... and the Job counts it as failed and terminating at once. Counting both is the bug that
    // reported a retry for this very recording.
    assert_eq!((job["status"]["failed"].as_u64(), job["status"]["terminating"].as_u64()), (Some(1), Some(1)));
    assert_eq!(job_pod_count(&job), 1);
    let err = decide("deadline").unwrap_err();
    assert!(matches!(err, K8sError::DeadlineExceeded { .. }), "{err:?}");
    let f = failure(&err);
    assert_eq!((f.kind, f.exit_code), (pb::JobFailureKind::NonzeroExit as i32, -1));
}

#[test]
fn a_recorded_unpullable_image_is_never_started_before_the_job_says_anything() {
    let (job, pod) = (load("never_pull", "job"), load("never_pull", "pod"));
    assert!(job_condition(&job, &["Failed", "FailureTarget", "Complete"]).is_none(), "the Job itself had not failed: {}", job["status"]);
    let (reason, message) = waiting(&pod).unwrap();
    assert_eq!(reason, "ErrImageNeverPull");
    assert!(message.contains("is not present with pull policy of Never"), "{message}");
    let err = decide("never_pull").unwrap_err();
    assert!(matches!(&err, K8sError::NeverStarted { reason, .. } if reason == "ErrImageNeverPull"), "{err:?}");
    assert_eq!(failure(&err).kind, pb::JobFailureKind::ExecutorStartFailed as i32);
}

#[test]
fn a_recorded_mid_run_state_decides_nothing() {
    // The deadline recording with the controller's verdict and the deletion removed is a pod that is
    // simply running.
    let mut job = load("deadline", "job");
    job["status"] = json!({"active": 1, "ready": 1, "terminating": 0, "uncountedTerminatedPods": {}});
    let mut pod = load("deadline", "pod");
    pod["metadata"].as_object_mut().unwrap().remove("deletionTimestamp");
    assert_eq!(pod_phase(&pod), "Running");
    assert!(assess(&job, &tracks(&pod)).is_none());
}

#[test]
fn a_second_pod_after_a_recorded_kill_is_a_retry_whatever_the_second_pod_did() {
    let mut job = load("docker_kill", "job");
    let first = load("docker_kill", "pod");
    let mut second = first.clone();
    second["metadata"]["name"] = json!("av-job-retry-pod-2");
    second["status"] = json!({"phase": "Running", "containerStatuses": [{"name": "job", "state": {"running": {"startedAt": "2026-10-17T14:32:00Z"}}}]});
    // What a Job with backoffLimit 1 looks like once the controller has started the replacement.
    job["status"] = json!({"failed": 1, "active": 1, "ready": 1, "terminating": 0});
    let mut both = tracks(&first);
    both.extend(tracks(&second));
    match assess(&job, &both) {
        Some(Err(K8sError::Retried { pods })) => assert_eq!(pods.len(), 2, "{pods:?}"),
        other => panic!("{other:?}"),
    }
    // The same Job status with only the first pod ever polled (the second came and went between
    // two polls) is still a retry, from the Job's own counters.
    match assess(&job, &tracks(&first)) {
        Some(Err(K8sError::Retried { .. })) => {}
        other => panic!("{other:?}"),
    }
    // Even a second pod that succeeds does not make the job a success.
    job["status"] = json!({"failed": 1, "succeeded": 1, "conditions": [{"type": "Complete", "status": "True"}]});
    let mut done = tracks(&first);
    let mut ok = second.clone();
    ok["status"] = json!({"phase": "Succeeded", "containerStatuses": [{"name": "job", "state": {"terminated": {"exitCode": 0, "reason": "Completed"}}}]});
    done.extend(tracks(&ok));
    assert!(matches!(assess(&job, &done), Some(Err(K8sError::Retried { .. }))));
}

#[test]
fn a_recorded_failed_network_gate_is_a_container_that_never_started_before_the_job_says_anything() {
    // The gate (an init container) exited 1 because egress stayed open; the workload container never
    // started (still waiting), the pod is Failed, and the Job itself still says `active: 1`.
    let (job, pod) = (load("gate_closed", "job"), load("gate_closed", "pod"));
    assert_eq!(job["status"]["active"], 1);
    assert!(job_condition(&job, &["Failed", "FailureTarget", "Complete"]).is_none());
    assert_eq!(pod_phase(&pod), "Failed");
    assert!(terminated(&pod).is_none() && waiting(&pod).is_some(), "the workload container never ran");
    let (name, t) = av_jobs::k8s::status::init_failure(&pod).expect("the init container failed");
    assert_eq!((name.as_str(), t.exit_code), ("net-gate", 1));
    let err = decide("gate_closed").unwrap_err();
    assert!(matches!(&err, K8sError::NeverStarted { reason, .. } if reason == "InitContainerFailed(net-gate)"), "{err:?}");
    assert_eq!(failure(&err).kind, pb::JobFailureKind::ExecutorStartFailed as i32);
}
