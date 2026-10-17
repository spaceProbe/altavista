# Recorded k3s Job and Pod objects

Real `batch/v1` Job and `v1` Pod JSON from this host's k3s (v1.35.0, Docker runtime), one pair per
scenario: the last poll before `av_jobs::k8s::K8sJobExecutor` decided (`managedFields` removed).
Recorded with `AV_K8S_FIXTURE_DIR=<dir> scripts/dev/cargo-slot test -p av-jobs --test k8s`;
replayed, with no cluster, by `tests/k8s_fixtures.rs`.

| scenario          | what happened                                                          |
|-------------------|------------------------------------------------------------------------|
| `success`         | exit 0; Job `Complete`                                                 |
| `exit7`           | `exit 7`; Job `Failed` (`BackoffLimitExceeded`)                        |
| `missing_command` | command not in the image: exit 127, `ContainerCannotRun`               |
| `docker_kill`     | `docker kill` of the pod's container: exit 137, `Error`                |
| `pod_deleted`     | pod deleted through the API mid-run: exit 137, `deletionTimestamp` set |
| `deadline`        | `activeDeadlineSeconds` 5: Job `FailureTarget` (`DeadlineExceeded`), pod being killed |
| `never_pull`      | image not on the node, `imagePullPolicy: Never`: `ErrImageNeverPull`  |
| `gate_closed`     | the network gate (init container) found egress still open: exit 1, workload never started |
