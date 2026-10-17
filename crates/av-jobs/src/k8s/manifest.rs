//! The `batch/v1` Job manifest for one `JobSpec`, as `serde_json::Value`.
//!
//! The manifest is the whole safety argument for a Kubernetes job, so it is a pure function of its
//! parameters with every field asserted by a unit test (`tests` below), and the hardening is
//! derived from the same list the Docker executor uses: a test walks
//! [`crate::container::HARDENING_RUN_FLAGS`] and fails on any flag it has no `securityContext`
//! equivalent for, so the Python declaration, the Docker executor and this manifest cannot drift
//! apart silently.
//!
//! | Docker (`ContainerExecutor`)           | Pod (`securityContext`, container level)            |
//! |----------------------------------------|-----------------------------------------------------|
//! | `--user 10001:10001`                   | `runAsUser`/`runAsGroup` 10001, `runAsNonRoot`      |
//! | `--read-only`                          | `readOnlyRootFilesystem: true`                      |
//! | `--cap-drop ALL`                       | `capabilities.drop: ["ALL"]`                        |
//! | `--security-opt no-new-privileges`     | `allowPrivilegeEscalation: false`                   |
//! | (default seccomp profile, not set)     | `seccompProfile.type: RuntimeDefault`               |
//! | `--network none`                       | **not expressible here** (see the module doc of `k8s`) |
//! | `-v in:/av-job/in:ro`, `-v out:...`    | `hostPath` volumes (`type: Directory`), `in` mounted `readOnly` |
//!
//! Everything else the manifest pins: one pod (`completions: 1`, `parallelism: 1`), no retry
//! (`backoffLimit: 0`) and no replacement pod while the first is still terminating
//! (`podReplacementPolicy: Failed`), `restartPolicy: Never`, an `activeDeadlineSeconds`, the pod
//! bound to a named node (`nodeName`, so the `hostPath` data plane is the same directory the
//! executor wrote), no service-account token and no service-link environment, the image as
//! `<image>@<digest>` with `imagePullPolicy: Never` (the cluster never pulls: the digest gate
//! already proved the image is present on this host's Docker, which is the node's runtime), and
//! declared CPU/memory requests and limits.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::{json, Value};

use crate::container::{CONTAINER_INPUT_DIR, CONTAINER_OUTPUT_DIR, JOB_LABEL_KEY, JOB_LABEL_VALUE};

/// The container's name inside the pod (also the `container=` of the log request).
pub const CONTAINER_NAME: &str = "job";

/// The fixed non-root uid and gid ([`crate::container::CONTAINER_UID_GID`], split).
pub const RUN_AS_ID: u64 = 10001;

/// CPU and memory the pod declares (Kubernetes quantity strings, e.g. `"50m"`, `"64Mi"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resources {
    pub cpu_request: String,
    pub cpu_limit: String,
    pub memory_request: String,
    pub memory_limit: String,
}

impl Default for Resources {
    fn default() -> Self {
        Resources { cpu_request: "50m".into(), cpu_limit: "500m".into(), memory_request: "32Mi".into(), memory_limit: "256Mi".into() }
    }
}

/// Everything the manifest is a function of.
#[derive(Debug, Clone)]
pub struct JobParams<'a> {
    pub job_name: &'a str,
    pub namespace: &'a str,
    /// `JobSpec.job_id`, verbatim.
    pub job_id: &'a str,
    /// `<image>@<digest>`, as returned by the digest gate.
    pub image_ref: &'a str,
    /// `JobSpec.command`; empty means the image's own entrypoint, as `docker run` would.
    pub command: &'a [String],
    pub node_name: &'a str,
    /// Absolute host paths of the per-run input and output directories.
    pub in_dir: &'a Path,
    pub out_dir: &'a Path,
    pub extra_labels: &'a BTreeMap<String, String>,
    pub resources: &'a Resources,
    pub deadline_seconds: u64,
    pub termination_grace_seconds: u64,
    pub ttl_after_finished_seconds: u64,
    /// The network-isolation gate init container, if configured (see [`NetworkGateParams`]).
    pub network_gate: Option<&'a NetworkGateParams>,
}

/// An init container that holds the workload back until the pod's egress is demonstrably closed.
///
/// The default-deny `NetworkPolicy` is enforced by the cluster's policy controller *after* the pod
/// starts: on this host's k3s, measured, a pod labelled `av.job=1` could still connect for the first
/// one to two seconds (`tests/k8s.rs`). Without a gate the workload's first instructions run with an
/// open network. The gate closes that: it runs in the same pod (so under the same labels and
/// policy), repeatedly tries a TCP connect to `probe_ip:probe_port` (the API server's ClusterIP),
/// exits 0 as soon as a connect **fails**, and exits 1 -- failing the pod before the workload ever
/// starts -- if the connect still succeeds after `within_seconds`. It fails closed: a cluster whose
/// controller does not enforce policy never runs the job. It needs `nc` in `image_ref`, so it uses
/// a configured, digest-pinned image rather than the job's own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkGateParams {
    /// `<image>@<digest>` of an image with busybox-style `nc -z -w`.
    pub image_ref: String,
    pub probe_ip: String,
    pub probe_port: u16,
    pub within_seconds: u64,
}

/// The gate's shell script (exposed so a test can assert what it does).
pub fn network_gate_script(g: &NetworkGateParams) -> String {
    format!(
        "end=$(( $(date +%s) + {within} )); \
         while [ $(date +%s) -lt $end ]; do nc -z -w 1 {ip} {port} || exit 0; sleep 0.2; done; \
         echo 'av net-gate: egress to {ip}:{port} still open after {within} s; refusing to run the job' >&2; exit 1",
        within = g.within_seconds,
        ip = g.probe_ip,
        port = g.probe_port
    )
}

/// The container-level `securityContext` shared by the workload and the gate.
fn hardened_security_context() -> Value {
    json!({
        "runAsNonRoot": true,
        "runAsUser": RUN_AS_ID,
        "runAsGroup": RUN_AS_ID,
        "readOnlyRootFilesystem": true,
        "allowPrivilegeEscalation": false,
        "capabilities": {"drop": ["ALL"]},
        "seccompProfile": {"type": "RuntimeDefault"},
    })
}

/// The init container's name (also what the status reader recognises).
pub const GATE_CONTAINER_NAME: &str = "net-gate";

/// A valid label value derived from `s`: characters outside `[A-Za-z0-9_.-]` become `_`, at most
/// 63 characters, beginning and ending alphanumeric. The verbatim value goes in the
/// `av.job.id.full` annotation.
pub fn label_value(s: &str) -> String {
    let cleaned: String = s.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '_' }).take(63).collect();
    let trimmed = cleaned.trim_matches(|c: char| !c.is_ascii_alphanumeric());
    if trimmed.is_empty() {
        "job".to_string()
    } else {
        trimmed.to_string()
    }
}

/// A DNS-1123 label fragment derived from `s`: lower-case alphanumerics and `-`, at most `max`
/// characters, beginning and ending alphanumeric (`"job"` if nothing is left).
pub fn dns_fragment(s: &str, max: usize) -> String {
    let cleaned: String = s.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).take(max).collect();
    let trimmed = cleaned.trim_matches('-');
    if trimmed.is_empty() {
        "job".to_string()
    } else {
        trimmed.to_string()
    }
}

/// The name of the default-deny `NetworkPolicy` every executor namespace carries.
pub const NETWORK_POLICY_NAME: &str = "av-job-default-deny";

/// The default-deny `NetworkPolicy` that stands in for `docker run --network none`: it selects
/// every pod labelled `av.job=1` (the label this module puts on all of its pods) and lists both
/// `Ingress` and `Egress` in `policyTypes` with no rules, so nothing may enter or leave -- no
/// cluster IPs, no DNS, no API server. Whether the cluster's network-policy controller actually
/// *enforces* it is a property of the cluster, proven (or not) by `tests/k8s.rs`, not assumed.
pub fn network_policy(namespace: &str) -> Value {
    json!({
        "apiVersion": "networking.k8s.io/v1",
        "kind": "NetworkPolicy",
        "metadata": {
            "name": NETWORK_POLICY_NAME,
            "namespace": namespace,
            "labels": {JOB_LABEL_KEY: JOB_LABEL_VALUE},
        },
        "spec": {
            "podSelector": {"matchLabels": {JOB_LABEL_KEY: JOB_LABEL_VALUE}},
            "policyTypes": ["Ingress", "Egress"],
        },
    })
}

/// Whether an existing policy's `spec` says the same as `desired`'s: the same selector, the same
/// set of policy types, and no allow rules in either direction (absent and empty are the same).
pub fn network_policy_matches(existing: &Value, desired: &Value) -> bool {
    let types = |v: &Value| -> BTreeSet<String> { v["spec"]["policyTypes"].as_array().map(|a| a.iter().filter_map(|t| t.as_str().map(str::to_string)).collect()).unwrap_or_default() };
    let no_rules = |v: &Value, k: &str| v["spec"][k].is_null() || v["spec"][k].as_array().is_some_and(|a| a.is_empty());
    existing["spec"]["podSelector"] == desired["spec"]["podSelector"]
        && types(existing) == types(desired)
        && ["ingress", "egress"].iter().all(|k| no_rules(existing, k) && no_rules(desired, k))
}

/// The Job manifest.
pub fn build_job(p: &JobParams<'_>) -> Value {
    let mut labels: BTreeMap<String, String> = p.extra_labels.clone();
    labels.insert(JOB_LABEL_KEY.to_string(), JOB_LABEL_VALUE.to_string());
    labels.insert("av.job.id".to_string(), label_value(p.job_id));
    let annotations = json!({ "av.job.id.full": p.job_id });

    let mut container = json!({
        "name": CONTAINER_NAME,
        "image": p.image_ref,
        "imagePullPolicy": "Never",
        "resources": {
            "requests": {"cpu": p.resources.cpu_request, "memory": p.resources.memory_request},
            "limits": {"cpu": p.resources.cpu_limit, "memory": p.resources.memory_limit},
        },
        "securityContext": hardened_security_context(),
        "volumeMounts": [
            {"name": "in", "mountPath": CONTAINER_INPUT_DIR, "readOnly": true},
            {"name": "out", "mountPath": CONTAINER_OUTPUT_DIR},
        ],
    });
    if !p.command.is_empty() {
        container["command"] = json!(p.command);
    }

    let mut job = json!({
        "apiVersion": "batch/v1",
        "kind": "Job",
        "metadata": {
            "name": p.job_name,
            "namespace": p.namespace,
            "labels": labels,
            "annotations": annotations,
        },
        "spec": {
            "completions": 1,
            "parallelism": 1,
            "backoffLimit": 0,
            "podReplacementPolicy": "Failed",
            "activeDeadlineSeconds": p.deadline_seconds,
            "ttlSecondsAfterFinished": p.ttl_after_finished_seconds,
            "template": {
                "metadata": {"labels": labels, "annotations": annotations},
                "spec": {
                    "restartPolicy": "Never",
                    "nodeName": p.node_name,
                    "automountServiceAccountToken": false,
                    "enableServiceLinks": false,
                    "terminationGracePeriodSeconds": p.termination_grace_seconds,
                    "containers": [container],
                    "volumes": [
                        {"name": "in", "hostPath": {"path": p.in_dir.to_string_lossy(), "type": "Directory"}},
                        {"name": "out", "hostPath": {"path": p.out_dir.to_string_lossy(), "type": "Directory"}},
                    ],
                },
            },
        },
    });
    if let Some(gate) = p.network_gate {
        job["spec"]["template"]["spec"]["initContainers"] = json!([{
            "name": GATE_CONTAINER_NAME,
            "image": gate.image_ref,
            "imagePullPolicy": "Never",
            "command": ["/bin/sh", "-c", network_gate_script(gate)],
            "resources": {
                "requests": {"cpu": "10m", "memory": "8Mi"},
                "limits": {"cpu": "100m", "memory": "32Mi"},
            },
            "securityContext": hardened_security_context(),
        }]);
    }
    job
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn params<'a>(command: &'a [String], extra: &'a BTreeMap<String, String>, resources: &'a Resources, in_dir: &'a Path, out_dir: &'a Path) -> JobParams<'a> {
        JobParams {
            job_name: "av-job-probe-1",
            namespace: "av-heavy-ct-x",
            job_id: "probe/1",
            image_ref: "alpine@sha256:294b683cb724975bec92580e1e685676bd4b50bda910ddb8c51d4cabeaec77e6",
            command,
            node_name: "colima",
            in_dir,
            out_dir,
            extra_labels: extra,
            resources,
            deadline_seconds: 90,
            termination_grace_seconds: 5,
            ttl_after_finished_seconds: 600,
            network_gate: None,
        }
    }

    fn gate() -> NetworkGateParams {
        NetworkGateParams { image_ref: "alpine@sha256:294b683cb724975bec92580e1e685676bd4b50bda910ddb8c51d4cabeaec77e6".into(), probe_ip: "10.43.0.1".into(), probe_port: 443, within_seconds: 20 }
    }

    #[test]
    fn without_a_gate_there_is_no_init_container_and_with_one_it_is_hardened_and_never_pulled() {
        assert!(manifest()["spec"]["template"]["spec"].get("initContainers").is_none());
        let command = vec!["/bin/true".to_string()];
        let (extra, resources) = (BTreeMap::new(), Resources::default());
        let g = gate();
        let mut p = params(&command, &extra, &resources, Path::new("/Users/probe/i"), Path::new("/Users/probe/o"));
        p.network_gate = Some(&g);
        let m = build_job(&p);
        let inits = m["spec"]["template"]["spec"]["initContainers"].as_array().unwrap();
        assert_eq!(inits.len(), 1);
        let init = &inits[0];
        assert_eq!(init["name"], GATE_CONTAINER_NAME);
        assert_eq!(init["image"], g.image_ref.as_str());
        assert_eq!(init["imagePullPolicy"], "Never");
        assert_eq!(init["securityContext"], m["spec"]["template"]["spec"]["containers"][0]["securityContext"], "the gate runs under the same hardening as the workload");
        let script = init["command"][2].as_str().unwrap();
        // Exits 0 as soon as a connect fails; exits 1 (failing the pod) if it never does.
        assert!(script.contains("nc -z -w 1 10.43.0.1 443 || exit 0") && script.contains("exit 1") && script.contains("+ 20 )"), "{script}");
        // The workload's own container is unchanged by the gate.
        assert_eq!(m["spec"]["template"]["spec"]["containers"].as_array().unwrap().len(), 1);
    }

    fn manifest() -> Value {
        let command = vec!["/bin/sh".to_string(), "-c".to_string(), "exit 7".to_string()];
        let extra = BTreeMap::from([("av.test".to_string(), "1".to_string()), ("av.test.run_id".to_string(), "r1".to_string())]);
        let resources = Resources { cpu_request: "10m".into(), cpu_limit: "200m".into(), memory_request: "16Mi".into(), memory_limit: "64Mi".into() };
        build_job(&params(&command, &extra, &resources, &PathBuf::from("/Users/probe/s/in"), &PathBuf::from("/Users/probe/s/out")))
    }

    #[test]
    fn the_job_is_one_pod_with_no_retry_and_a_deadline() {
        let m = manifest();
        assert_eq!((m["apiVersion"].as_str(), m["kind"].as_str()), (Some("batch/v1"), Some("Job")));
        assert_eq!(m["metadata"]["name"], "av-job-probe-1");
        assert_eq!(m["metadata"]["namespace"], "av-heavy-ct-x");
        let spec = &m["spec"];
        assert_eq!(spec["completions"], 1);
        assert_eq!(spec["parallelism"], 1);
        assert_eq!(spec["backoffLimit"], 0);
        assert_eq!(spec["podReplacementPolicy"], "Failed");
        assert_eq!(spec["activeDeadlineSeconds"], 90);
        assert_eq!(spec["ttlSecondsAfterFinished"], 600);
        let pod = &spec["template"]["spec"];
        assert_eq!(pod["restartPolicy"], "Never");
        assert_eq!(pod["nodeName"], "colima");
        assert_eq!(pod["automountServiceAccountToken"], false);
        assert_eq!(pod["enableServiceLinks"], false);
        assert_eq!(pod["terminationGracePeriodSeconds"], 5);
        assert_eq!(pod["containers"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn the_image_is_a_digest_reference_that_is_never_pulled() {
        let c = &manifest()["spec"]["template"]["spec"]["containers"][0];
        assert_eq!(c["image"], "alpine@sha256:294b683cb724975bec92580e1e685676bd4b50bda910ddb8c51d4cabeaec77e6");
        assert_eq!(c["imagePullPolicy"], "Never");
        assert_eq!(c["name"], CONTAINER_NAME);
        assert_eq!(c["command"], json!(["/bin/sh", "-c", "exit 7"]));
    }

    #[test]
    fn an_empty_command_leaves_the_image_entrypoint_alone() {
        let extra = BTreeMap::new();
        let r = Resources::default();
        let m = build_job(&params(&[], &extra, &r, Path::new("/Users/probe/i"), Path::new("/Users/probe/o")));
        assert!(m["spec"]["template"]["spec"]["containers"][0].get("command").is_none());
    }

    #[test]
    fn resources_are_declared_as_requests_and_limits() {
        let c = &manifest()["spec"]["template"]["spec"]["containers"][0];
        assert_eq!(c["resources"]["requests"], json!({"cpu": "10m", "memory": "16Mi"}));
        assert_eq!(c["resources"]["limits"], json!({"cpu": "200m", "memory": "64Mi"}));
    }

    #[test]
    fn the_security_context_expresses_the_hardening() {
        let sc = &manifest()["spec"]["template"]["spec"]["containers"][0]["securityContext"];
        assert_eq!(sc["runAsNonRoot"], true);
        assert_eq!((sc["runAsUser"].as_u64(), sc["runAsGroup"].as_u64()), (Some(10001), Some(10001)));
        assert_eq!(sc["readOnlyRootFilesystem"], true);
        assert_eq!(sc["allowPrivilegeEscalation"], false);
        assert_eq!(sc["capabilities"], json!({"drop": ["ALL"]}));
        assert_eq!(sc["seccompProfile"], json!({"type": "RuntimeDefault"}));
        assert_eq!(format!("{RUN_AS_ID}:{RUN_AS_ID}"), crate::container::CONTAINER_UID_GID);
    }

    /// Every docker hardening flag has a securityContext equivalent, and the list has no flag this
    /// manifest does not know: a new flag in `altavista/container_hardening.py` (mirrored in
    /// `HARDENING_RUN_FLAGS` by the container module's own parity test) fails here until the
    /// manifest is taught it.
    #[test]
    fn every_docker_hardening_flag_has_a_security_context_equivalent() {
        let sc = &manifest()["spec"]["template"]["spec"]["containers"][0]["securityContext"];
        let flags = crate::container::HARDENING_RUN_FLAGS;
        let mut i = 0;
        while i < flags.len() {
            match flags[i] {
                "--read-only" => assert_eq!(sc["readOnlyRootFilesystem"], true),
                "--cap-drop" => {
                    assert_eq!(flags[i + 1], "ALL");
                    assert_eq!(sc["capabilities"]["drop"], json!(["ALL"]));
                    i += 1;
                }
                "--security-opt" => {
                    assert_eq!(flags[i + 1], "no-new-privileges");
                    assert_eq!(sc["allowPrivilegeEscalation"], false);
                    i += 1;
                }
                other => panic!("HARDENING_RUN_FLAGS has {other:?}, which the Kubernetes manifest does not translate"),
            }
            i += 1;
        }
    }

    #[test]
    fn the_data_plane_is_two_host_paths_the_input_one_read_only() {
        let pod = &manifest()["spec"]["template"]["spec"];
        assert_eq!(pod["volumes"][0], json!({"name": "in", "hostPath": {"path": "/Users/probe/s/in", "type": "Directory"}}));
        assert_eq!(pod["volumes"][1], json!({"name": "out", "hostPath": {"path": "/Users/probe/s/out", "type": "Directory"}}));
        let mounts = &pod["containers"][0]["volumeMounts"];
        assert_eq!(mounts[0], json!({"name": "in", "mountPath": "/av-job/in", "readOnly": true}));
        assert_eq!(mounts[1], json!({"name": "out", "mountPath": "/av-job/out"}));
        assert!(mounts[1].get("readOnly").is_none());
    }

    #[test]
    fn labels_are_on_the_job_and_the_pod_template_with_extras_and_the_verbatim_id_annotated() {
        let m = manifest();
        for labels in [&m["metadata"]["labels"], &m["spec"]["template"]["metadata"]["labels"]] {
            assert_eq!(labels["av.job"], "1");
            assert_eq!(labels["av.job.id"], "probe_1", "a job id that is not a valid label value is sanitised");
            assert_eq!(labels["av.test"], "1");
            assert_eq!(labels["av.test.run_id"], "r1");
        }
        assert_eq!(m["metadata"]["annotations"]["av.job.id.full"], "probe/1");
        assert_eq!(m["spec"]["template"]["metadata"]["annotations"]["av.job.id.full"], "probe/1");
    }

    #[test]
    fn extra_labels_cannot_override_the_fixed_ones() {
        let command = vec![];
        let extra = BTreeMap::from([("av.job".to_string(), "0".to_string()), ("av.job.id".to_string(), "other".to_string())]);
        let r = Resources::default();
        let m = build_job(&params(&command, &extra, &r, Path::new("/Users/probe/i"), Path::new("/Users/probe/o")));
        assert_eq!(m["metadata"]["labels"]["av.job"], "1");
        assert_eq!(m["metadata"]["labels"]["av.job.id"], "probe_1");
    }

    #[test]
    fn the_network_policy_denies_everything_for_every_pod_the_executor_labels() {
        let p = network_policy("av-ns");
        assert_eq!((p["apiVersion"].as_str(), p["kind"].as_str()), (Some("networking.k8s.io/v1"), Some("NetworkPolicy")));
        assert_eq!(p["metadata"]["name"], NETWORK_POLICY_NAME);
        assert_eq!(p["metadata"]["namespace"], "av-ns");
        // It selects exactly the label build_job puts on every pod ...
        assert_eq!(p["spec"]["podSelector"], json!({"matchLabels": {"av.job": "1"}}));
        let job = manifest();
        assert_eq!(job["spec"]["template"]["metadata"]["labels"]["av.job"], "1");
        // ... in both directions, with no allow rule at all.
        assert_eq!(p["spec"]["policyTypes"], json!(["Ingress", "Egress"]));
        assert!(p["spec"].get("ingress").is_none() && p["spec"].get("egress").is_none());
    }

    #[test]
    fn a_policy_matches_only_if_it_denies_the_same_pods_in_the_same_directions_with_no_rules() {
        let want = network_policy("a");
        assert!(network_policy_matches(&network_policy("b"), &want), "the namespace is not part of the spec");
        let mut reordered = want.clone();
        reordered["spec"]["policyTypes"] = json!(["Egress", "Ingress"]);
        reordered["spec"]["egress"] = json!([]);
        assert!(network_policy_matches(&reordered, &want), "order and empty rule lists do not matter");
        let mut egress_only = want.clone();
        egress_only["spec"]["policyTypes"] = json!(["Egress"]);
        assert!(!network_policy_matches(&egress_only, &want));
        let mut other_selector = want.clone();
        other_selector["spec"]["podSelector"] = json!({});
        assert!(!network_policy_matches(&other_selector, &want));
        let mut allows = want.clone();
        allows["spec"]["egress"] = json!([{"to": [{"ipBlock": {"cidr": "0.0.0.0/0"}}]}]);
        assert!(!network_policy_matches(&allows, &want), "an allow rule is not default-deny");
        let mut allows_in = want.clone();
        allows_in["spec"]["ingress"] = json!([{}]);
        assert!(!network_policy_matches(&allows_in, &want));
    }

    #[test]
    fn label_values_and_dns_fragments_are_always_valid() {
        assert_eq!(label_value("job-1"), "job-1");
        assert_eq!(label_value("/a b/"), "a_b");
        assert_eq!(label_value("___"), "job");
        assert_eq!(label_value(&"x".repeat(100)).len(), 63);
        assert_eq!(label_value(&format!("{}-", "y".repeat(62))), "y".repeat(62), "a value cut to 63 characters is re-trimmed");
        assert_eq!(dns_fragment("Job_ID/1", 24), "job-id-1");
        assert_eq!(dns_fragment("!!!", 24), "job");
        assert_eq!(dns_fragment(&"a".repeat(40), 24).len(), 24);
        assert_eq!(dns_fragment("ab-cd", 3), "ab", "a fragment cut at a hyphen is re-trimmed");
    }
}
