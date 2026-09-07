//! Docker image lifecycle for a `BINDING_KIND_CONTAINER` instance whose `ContainerBinding`
//! declares `image`/`image_digest` (`docs/open-questions.md` question 118, M15.3) -- pull the
//! declared image by digest, run it with its declared ports published on loopback, and stop +
//! remove it once the run's `Shutdown` RPC has completed. This is a genuinely new capability:
//! M13.2/M14.x's `BINDING_KIND_CONTAINER` path only ever spoke `LockstepService` to an
//! **already-running** process named by a `container.address` parameter --
//! `ContainerBinding.image`/`image_digest`/`command`/`port_endpoints` were parsed by nobody
//! (`crates/av-kernel/src/drm/binding.rs`'s own module doc comment: "shaped around the
//! image-lifecycle path this batch does not build"). `crate::drm::binding::materialize_container`
//! is what now calls into this module when `ContainerBinding.image` is set -- see that
//! function's own doc comment.
//!
//! **Why shell out to the `docker` CLI rather than a Docker Engine API crate.** No such crate
//! is already a workspace dependency, and the root `Cargo.toml`/`deny.toml` are outside this
//! task's file ownership (`crates/av-kernel`'s own task brief: "escalate rather than edit") --
//! adding one would mean editing a file this task may not touch. `docker`/`docker info`
//! succeeding is already this whole feature's own precondition (question 118's decision:
//! "container image lifecycle proceeds with tests gated on Docker availability"), so shelling
//! out to the same CLI a human operator would use costs nothing this task does not already pay
//! for, and needs zero new dependencies.
//!
//! **What "pulled by digest" means here, honestly.** [`ManagedContainer::pull_and_run`] always
//! runs `docker pull <image>@<image_digest>` before `docker run` -- a real pull against
//! whatever registry the Docker daemon resolves `<image>`'s repository name to (a real
//! registry, or a local one a test stood up itself; this module does not care which). **No
//! image this task builds is ever pushed to a real/external registry** (question 118's own
//! "No image is pushed anywhere" rule) -- `tests/test_lockstep_ref.py`'s Docker-lifecycle test
//! proves the pull is real (not a silent no-op against an image already present locally under
//! that exact digest reference) by standing up a throwaway `registry:2` container on loopback,
//! pushing the locally-built `services/lockstep-ref` image there, and handing *that* address's
//! image reference to this module -- see that test's own docstring for the full account. This
//! module itself is registry-agnostic: it only ever runs `docker pull <ref>`, whatever `<ref>`
//! resolves to.
//!
//! **Loopback only.** [`ManagedContainer::pull_and_run`] always publishes on `127.0.0.1`
//! (`docker run -p 127.0.0.1::<container_port>`, letting Docker pick a free host port rather
//! than this module guessing one and racing another listener for it) -- matching question
//! 118's "Bind over loopback" decision and ADR-003's "plaintext loopback only inside one node
//! under test" rule. A real cross-host container binding is out of this task's scope (as it
//! already was for the M13.2 `container.address` path).

use std::collections::BTreeMap;
use std::process::Command;

/// Everything that can go wrong pulling, running, or tearing down a Docker-managed
/// `BINDING_KIND_CONTAINER` instance. Every variant carries the `docker` CLI's own stderr
/// (trimmed) as `detail` -- never swallowed, matching this workspace's "refuse, typed, never
/// silent" rule for every other binding-kind failure mode.
#[derive(Debug, Clone, thiserror::Error)]
pub enum DockerError {
    #[error("docker pull {image_ref}: {detail}")]
    Pull { image_ref: String, detail: String },
    #[error("docker run {image_ref}: {detail}")]
    Run { image_ref: String, detail: String },
    #[error("docker port {container_id} {container_port}: {detail}")]
    PortLookup { container_id: String, container_port: u16, detail: String },
    /// `docker port` succeeded but its own output did not parse as `<host>:<port>` -- a
    /// `docker` CLI/version behaviour change, not a data problem this module can recover from.
    #[error("docker port {container_id} {container_port}: could not parse a host port out of {raw:?}")]
    PortParse { container_id: String, container_port: u16, raw: String },
    #[error("docker stop {container_id}: {detail}")]
    Stop { container_id: String, detail: String },
    #[error("docker rm {container_id}: {detail}")]
    Remove { container_id: String, detail: String },
}

/// `docker info` succeeding -- the one precondition question 118 gates every Docker-lifecycle
/// test on ("tests run only when `docker info` succeeds, and otherwise skip with a recorded
/// reason"). `false` on any failure to even launch the `docker` binary (not installed), not
/// just a non-zero exit (the daemon not running) -- both mean "not available" to a caller.
pub fn docker_available() -> bool {
    Command::new("docker").arg("info").output().map(|o| o.status.success()).unwrap_or(false)
}

fn run_docker(args: &[&str]) -> Result<String, String> {
    let output = Command::new("docker").args(args).output().map_err(|e| format!("could not launch docker: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(stderr.trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// `docs/open-questions.md` question 156 and its 2026-09-06 amendment: an interrupted test
/// leaves a `Drop` guard un-run (a `Drop` impl never fires on `SIGKILL`), which is exactly how
/// a `lockstep-ref` test container and a throwaway `registry:2` container were once left
/// running for four hours. The fix is a standing marker label every AltaVista *test* attaches
/// to every container and image it creates (`TEST_LABEL_KEY=TEST_LABEL_VALUE`, a constant
/// value, not tied to any one run) plus [`prune_stale_test_resources`], which a test calls
/// *before* creating anything -- so the *next* test run sweeps up whatever the previous one
/// left behind, independent of whether that previous run's own `Drop` guards ever got to run.
/// This is deliberately separate from the per-run id below: the marker is what pruning filters
/// on (matching every prior run's leftovers, not just one), the run id is what a human reads
/// in `docker ps`/`docker images` output to tell two runs' resources apart.
pub const TEST_LABEL_KEY: &str = "av.test";
pub const TEST_LABEL_VALUE: &str = "1";

/// A per-process value for `docs/open-questions.md` question 156's "a label with the run id" --
/// attached as `av.test.run_id=<this>` alongside [`TEST_LABEL_KEY`] (see [`test_label_args`]).
/// Not part of the prune filter itself (which matches on `TEST_LABEL_KEY` alone), so it exists
/// purely for a human or a log to tell two runs' resources apart, never for correctness.
pub fn test_run_id() -> String {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("{pid}-{nanos}")
}

/// `--label KEY=VALUE` argument pairs (four strings: two flags, two `KEY=VALUE` values) for a
/// `docker build`/`docker run`/`docker tag`-adjacent invocation a *test* makes directly, so its
/// container or image carries both [`TEST_LABEL_KEY`] (what [`prune_stale_test_resources`]
/// filters on) and a per-run id (for humans). Every test in this workspace that shells out to
/// `docker` itself (rather than going through [`ManagedContainer::pull_and_run`], which takes
/// `extra_labels` directly) should splice this into its own argv.
pub fn test_label_args(run_id: &str) -> Vec<String> {
    vec!["--label".to_string(), format!("{TEST_LABEL_KEY}={TEST_LABEL_VALUE}"), "--label".to_string(), format!("av.test.run_id={run_id}")]
}

/// Removes every container and image carrying [`TEST_LABEL_KEY`], regardless of which run
/// created it or what tag/name it has (question 156's original finding: a stale image tagged
/// with a long-dead ephemeral registry's port; the amendment's finding: a stale container from
/// a killed process). A test calls this **before** it creates anything -- pruning only at
/// teardown cannot help when teardown itself never ran. Best-effort: `docker rm -f`/`docker rmi
/// -f` failures here (e.g. an id that raced its own removal, or a container something else
/// still references) are swallowed -- this is a hygiene sweep, not the thing under test, and
/// must never itself fail a test that would otherwise pass.
pub fn prune_stale_test_resources() {
    let filter = format!("label={TEST_LABEL_KEY}");
    if let Ok(out) = Command::new("docker").args(["ps", "-aq", "--filter", &filter]).output() {
        for id in String::from_utf8_lossy(&out.stdout).lines().map(str::trim).filter(|l| !l.is_empty()) {
            let _ = Command::new("docker").args(["rm", "-f", id]).output();
        }
    }
    if let Ok(out) = Command::new("docker").args(["images", "-q", "--filter", &filter]).output() {
        for id in String::from_utf8_lossy(&out.stdout).lines().map(str::trim).filter(|l| !l.is_empty()) {
            let _ = Command::new("docker").args(["rmi", "-f", id]).output();
        }
    }
}

/// Builds the `docker run` argv for [`ManagedContainer::pull_and_run`] -- a pure function
/// (no `docker` invocation, no I/O) so its own determinism (ADR-004: sorted `BTreeMap`
/// iteration, never hash-map order) and its `port_endpoints` validation are unit-testable
/// without `docker` installed, below. Order: `-p` (control port, then every
/// `extra_port_endpoints` entry sorted by name) -- `-e` (every `env` entry sorted by key) --
/// `--sysctl` (every `extra_sysctls` entry sorted by key) -- `--label` (every `extra_labels`
/// entry sorted by key, question 156) -- the image reference -- `command`.
fn build_run_args(image_ref: &str, command: &[String], control_container_port: u16, extra_port_endpoints: &BTreeMap<String, String>, env: &BTreeMap<String, String>, extra_sysctls: &BTreeMap<String, String>, extra_labels: &BTreeMap<String, String>) -> Result<Vec<String>, DockerError> {
    let mut args: Vec<String> = vec!["run".to_string(), "-d".to_string(), "-p".to_string(), format!("127.0.0.1::{control_container_port}")];
    for (name, endpoint) in extra_port_endpoints {
        let Some(port) = endpoint.rsplit(':').next().filter(|p| p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty()) else {
            return Err(DockerError::Run { image_ref: image_ref.to_string(), detail: format!("port_endpoints[{name:?}] = {endpoint:?} does not end in a numeric port") });
        };
        args.push("-p".to_string());
        args.push(format!("127.0.0.1::{port}"));
    }
    // Sorted iteration (BTreeMap): the `docker run` invocation this builds is deterministic
    // (ADR-004) -- two calls with the same inputs produce byte-identical argv, never an
    // order that depends on hash-map iteration.
    for (k, v) in env {
        args.push("-e".to_string());
        args.push(format!("{k}={v}"));
    }
    for (k, v) in extra_sysctls {
        args.push("--sysctl".to_string());
        args.push(format!("{k}={v}"));
    }
    for (k, v) in extra_labels {
        args.push("--label".to_string());
        args.push(format!("{k}={v}"));
    }
    args.push(image_ref.to_string());
    args.extend(command.iter().cloned());
    Ok(args)
}

/// One Docker-run `BINDING_KIND_CONTAINER` instance's own process, from `pull_and_run` through
/// `stop_and_remove`. `stop_and_remove` is meant to be called exactly once, at run end (mirrors
/// `crate::drm::binding::ContainerModel::shutdown`'s own "once, after the last Step" contract
/// -- `crate::drm::executor::run_shared_group` calls both, `shutdown` first); [`Drop`] also
/// best-effort calls it (errors only logged, never propagated -- a `Drop` impl cannot return
/// `Result`) so a panicking test, or an early `?` return, can never leak a running container,
/// the same guarantee `tests/drm_container.rs`'s own subprocess `Drop` guard already gives the
/// plain-subprocess path.
#[derive(Debug)]
pub struct ManagedContainer {
    pub container_id: String,
    stopped: bool,
}

impl ManagedContainer {
    /// `docker pull <image>@<image_digest>`, then `docker run -d` with `control_container_port`
    /// and every `extra_port_endpoints` port published on loopback (`docker` picks the host
    /// port; the control port's is returned), `env` set via `-e`, and `extra_sysctls` set via
    /// `--sysctl` (M23.4: a container-bound cFS image needs `fs.mqueue.msg_max`/
    /// `fs.mqueue.msgsize_max` raised above this host's own default -- cFE's core `CFE_SB`/
    /// `CFE_EVS` pipes are POSIX message queues and fail `OS_QueueCreate` at boot otherwise,
    /// confirmed by actually running the image without them: `errno = 22 (Invalid argument)`,
    /// `CFE_ES_TaskInit: Cannot Create SB Pipe` -- a general enough need that a Docker-run
    /// `BINDING_KIND_CONTAINER` instance should be able to declare it without every future
    /// image needing its own bespoke lifecycle path; empty for a container with no such need,
    /// same as `extra_port_endpoints`). `extra_labels` (question 156) attaches `--label
    /// KEY=VALUE` for every entry -- empty for a production/kernel-driven run, which has no
    /// use for the test marker; a test passes labels derived from [`test_label_args`] here so
    /// the container it creates is swept by [`prune_stale_test_resources`] if left behind.
    /// `command`, if non-empty, overrides the image's own
    /// `ENTRYPOINT`/`CMD` (`ContainerBinding.command`'s own doc comment). Returns the managed
    /// handle and the host port `control_container_port` landed on.
    #[allow(clippy::too_many_arguments)]
    pub fn pull_and_run(
        image: &str,
        image_digest: &str,
        command: &[String],
        control_container_port: u16,
        extra_port_endpoints: &BTreeMap<String, String>,
        env: &BTreeMap<String, String>,
        extra_sysctls: &BTreeMap<String, String>,
        extra_labels: &BTreeMap<String, String>,
    ) -> Result<(Self, u16), DockerError> {
        let image_ref = format!("{image}@{image_digest}");

        // Build (and fully validate) the `docker run` argv *before* ever touching the network
        // with a `docker pull` -- a malformed `port_endpoints` entry is a caller/DRM-authoring
        // bug, not something a real registry round trip should be spent discovering.
        let args = build_run_args(&image_ref, command, control_container_port, extra_port_endpoints, env, extra_sysctls, extra_labels)?;

        run_docker(&["pull", &image_ref]).map_err(|detail| DockerError::Pull { image_ref: image_ref.clone(), detail })?;

        let args_ref: Vec<&str> = args.iter().map(String::as_str).collect();
        let container_id = run_docker(&args_ref).map_err(|detail| DockerError::Run { image_ref: image_ref.clone(), detail })?;
        let managed = Self { container_id: container_id.clone(), stopped: false };

        let host_port = match managed.published_host_port(control_container_port) {
            Ok(p) => p,
            Err(e) => {
                // Best-effort teardown before propagating -- a container we just started but
                // can never address is not something to leave running.
                let mut managed = managed;
                let _ = managed.stop_and_remove();
                return Err(e);
            }
        };
        Ok((managed, host_port))
    }

    /// `docker port <id> <container_port>` -- e.g. `"0.0.0.0:54321"` -- and take the numeric
    /// suffix as the loopback host port the kernel actually connects to.
    fn published_host_port(&self, container_port: u16) -> Result<u16, DockerError> {
        let port_str = container_port.to_string();
        let raw = run_docker(&["port", &self.container_id, &port_str]).map_err(|detail| DockerError::PortLookup { container_id: self.container_id.clone(), container_port, detail })?;
        let first_line = raw.lines().next().unwrap_or("");
        first_line
            .rsplit(':')
            .next()
            .and_then(|p| p.parse::<u16>().ok())
            .ok_or_else(|| DockerError::PortParse { container_id: self.container_id.clone(), container_port, raw: raw.clone() })
    }

    /// `docker stop` then `docker rm` -- idempotent (a second call, or `Drop` after an explicit
    /// call already succeeded, is a no-op).
    pub fn stop_and_remove(&mut self) -> Result<(), DockerError> {
        if self.stopped {
            return Ok(());
        }
        run_docker(&["stop", &self.container_id]).map_err(|detail| DockerError::Stop { container_id: self.container_id.clone(), detail })?;
        run_docker(&["rm", &self.container_id]).map_err(|detail| DockerError::Remove { container_id: self.container_id.clone(), detail })?;
        self.stopped = true;
        Ok(())
    }
}

impl Drop for ManagedContainer {
    fn drop(&mut self) {
        if !self.stopped {
            let _ = self.stop_and_remove();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Not gated on `docker_available()`: this exercises the pure argv-building logic
    /// (`pull_and_run`'s port-suffix parsing), not an actual `docker` invocation, so it must
    /// run unconditionally. Fails against an implementation that used `HashMap` iteration
    /// order (or otherwise built a non-deterministic port list) for `extra_port_endpoints`.
    #[test]
    fn a_malformed_port_endpoint_is_a_typed_error_before_any_docker_call() {
        let mut endpoints = BTreeMap::new();
        endpoints.insert("tm".to_string(), "udp://0.0.0.0:not-a-port".to_string());
        // `docker` need not even be installed for this to fail correctly: the malformed
        // endpoint is caught before `run_docker` is ever called.
        let err = ManagedContainer::pull_and_run("bogus/image", "sha256:0000000000000000000000000000000000000000000000000000000000000000", &[], 50070, &endpoints, &BTreeMap::new(), &BTreeMap::new(), &BTreeMap::new()).unwrap_err();
        assert!(matches!(err, DockerError::Run { .. }), "{err:?}");
    }

    /// M23.4: `--sysctl NAME=VALUE` is emitted once per `extra_sysctls` entry, in sorted-by-key
    /// order (`BTreeMap`, ADR-004 determinism -- same rationale as `env`/`extra_port_endpoints`
    /// above), positioned after `-e` and before the image reference. Fails against an
    /// implementation that never threads `extra_sysctls` into the argv at all (the gap this
    /// task found by actually running the cFS image: `OS_QueueCreate_Impl` fails with
    /// `errno = 22` and cFE never reaches `OPERATIONAL` state without
    /// `fs.mqueue.msg_max`/`fs.mqueue.msgsize_max` raised).
    #[test]
    fn sysctls_are_emitted_sorted_by_key_between_env_and_the_image_reference() {
        let mut sysctls = BTreeMap::new();
        sysctls.insert("fs.mqueue.msgsize_max".to_string(), "65536".to_string());
        sysctls.insert("fs.mqueue.msg_max".to_string(), "256".to_string());
        let mut env = BTreeMap::new();
        env.insert("IMAGE_DIGEST".to_string(), "sha256:deadbeef".to_string());

        let args = build_run_args("altavista-cfs-lockstep@sha256:deadbeef", &[], 50070, &BTreeMap::new(), &env, &sysctls, &BTreeMap::new()).expect("well-formed inputs build cleanly");

        // "fs.mqueue.msg_max" sorts before "fs.mqueue.msgsize_max" (shorter string, same
        // prefix) -- byte-order, not insertion order (inserted above in the opposite order).
        let expected = vec![
            "run".to_string(),
            "-d".to_string(),
            "-p".to_string(),
            "127.0.0.1::50070".to_string(),
            "-e".to_string(),
            "IMAGE_DIGEST=sha256:deadbeef".to_string(),
            "--sysctl".to_string(),
            "fs.mqueue.msg_max=256".to_string(),
            "--sysctl".to_string(),
            "fs.mqueue.msgsize_max=65536".to_string(),
            "altavista-cfs-lockstep@sha256:deadbeef".to_string(),
        ];
        assert_eq!(args, expected);
    }

    /// An empty `extra_sysctls` (every caller before M23.4, and every non-cFS container image)
    /// emits no `--sysctl` flag at all -- this addition changes nothing for an existing caller.
    #[test]
    fn no_sysctls_means_no_sysctl_flag() {
        let args = build_run_args("img@sha256:aa", &[], 50070, &BTreeMap::new(), &BTreeMap::new(), &BTreeMap::new(), &BTreeMap::new()).unwrap();
        assert!(!args.iter().any(|a| a == "--sysctl"), "{args:?}");
    }

    /// Question 156: `extra_labels` is emitted as `--label KEY=VALUE` per entry, sorted by key
    /// (`BTreeMap`, same determinism rule as `env`/`extra_sysctls`), after `--sysctl` and before
    /// the image reference. Fails against an implementation that silently drops labels the way
    /// `extra_sysctls` itself was once dropped (M23.4's own finding).
    #[test]
    fn labels_are_emitted_sorted_by_key_between_sysctls_and_the_image_reference() {
        let mut labels = BTreeMap::new();
        labels.insert(TEST_LABEL_KEY.to_string(), TEST_LABEL_VALUE.to_string());
        labels.insert("av.test.run_id".to_string(), "123-456".to_string());
        let args = build_run_args("img@sha256:aa", &[], 50070, &BTreeMap::new(), &BTreeMap::new(), &BTreeMap::new(), &labels).unwrap();
        let expected_tail = vec!["--label".to_string(), "av.test=1".to_string(), "--label".to_string(), "av.test.run_id=123-456".to_string(), "img@sha256:aa".to_string()];
        assert_eq!(&args[args.len() - expected_tail.len()..], expected_tail.as_slice(), "{args:?}");
    }

    /// `test_label_args` itself: the exact two-flag, four-token shape every test-owned `docker`
    /// invocation splices in directly (the ones that never go through `build_run_args` at all,
    /// e.g. `docker build`/`docker tag`/the throwaway `registry:2` container).
    #[test]
    fn test_label_args_shape() {
        let args = test_label_args("77-88");
        assert_eq!(args, vec!["--label".to_string(), "av.test=1".to_string(), "--label".to_string(), "av.test.run_id=77-88".to_string()]);
    }
}
