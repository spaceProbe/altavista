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
use std::fmt;
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
///
/// Kept as a plain bool for every pre-R6.3 call site (M15.3's original shape); see
/// [`docker_daemon_status`] for the typed, question-194 replacement that distinguishes *why*.
pub fn docker_available() -> bool {
    docker_daemon_status().is_ok()
}

// ------------------------------------------------------------------------------------------
// Question 194 (M23.4/round 5-6): "the cFS container tests completed in 0.17s with no image
// present and reported success" -- a green kernel suite did not prove the SIL path ran, because
// the pre-R6.3 shape was `println!("SKIPPED ...")` then `return`, and `cargo test`'s default
// runner captures a PASSING test's `println!`/`eprintln!` output and never prints it without
// `--nocapture` (measured directly, this crate's own R6_3_REPORT.md section 1: a raw write via
// `std::io::stderr().write_all(...)` remains visible where `println!`/`eprintln!` do not,
// because libtest's output-capture hook is wired into the `print!`/`eprintln!` macros'
// `io::_print`/`io::_eprint` helper functions, never into `Stdout`/`Stderr`'s own `Write` impl).
// Decided by the lead: every Docker- or image-gated test either runs against the image or
// prints a VISIBLE SKIPPED with the reason; the gating helper returns a typed reason (not a
// bare `String`) and the test asserts on what the helper actually announced.
// ------------------------------------------------------------------------------------------

/// Why a Docker- or image-gated test cannot run against the real thing right now. Every variant
/// renders (via [`fmt::Display`]/[`DockerGateReason::message`]) to a human-readable line naming
/// the resource and how to obtain it -- at least as informative as the pre-R6.3 printed strings
/// (`cfs_image_unavailable_reason`/`renode_unavailable_reason`'s own hand-formatted `String`s,
/// which this type now standardizes without losing any of their detail).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DockerGateReason {
    /// The `docker` binary itself could not even be launched (not installed / not on `PATH`).
    DockerNotInstalled,
    /// `docker` launched but `docker info` returned non-zero -- the CLI exists, the daemon does
    /// not answer it (not running, permission denied, etc.). `detail` is `docker info`'s own
    /// trimmed stderr, never swallowed (this workspace's "refuse, typed, never silent" rule,
    /// already the convention for every [`DockerError`] variant above).
    DockerDaemonUnreachable { detail: String },
    /// The daemon answered, but `docker image inspect <image_ref>` did not resolve -- the
    /// declared image is not built locally. `build_hint` names the concrete command a human (or
    /// this repository's own build script) runs to produce it.
    ImageNotBuilt { image_ref: String, build_hint: String },
    /// A required file this gated test's own non-Docker half needs (a cross-built ELF, the
    /// Renode binary, a bridge script, ...) is missing from the working tree.
    RequiredFileMissing { what: String, path: String },
    /// A non-Docker prerequisite this gated test's own toolchain needs is unavailable (e.g. no
    /// repo-local `.venv` and no `python3` with `protobuf` importable on `PATH`, for
    /// `crates/av-lockstep-shim/tests/end_to_end_kernel_path.rs`'s own Python reference peer).
    /// `hint` names how to obtain it.
    PrerequisiteUnavailable { what: String, hint: String },
}

impl DockerGateReason {
    /// The human-readable message every [`fmt::Display`] impl below renders verbatim.
    pub fn message(&self) -> String {
        match self {
            Self::DockerNotInstalled => "`docker` is not installed or not on PATH".to_string(),
            Self::DockerDaemonUnreachable { detail } => {
                if detail.is_empty() {
                    "`docker info` failed -- the Docker daemon is not reachable".to_string()
                } else {
                    format!("`docker info` failed -- the Docker daemon is not reachable: {detail}")
                }
            }
            Self::ImageNotBuilt { image_ref, build_hint } => format!("image {image_ref:?} is not built locally -- {build_hint}"),
            Self::RequiredFileMissing { what, path } => format!("{what} is missing at {path}"),
            Self::PrerequisiteUnavailable { what, hint } => format!("{what} is unavailable -- {hint}"),
        }
    }
}

impl fmt::Display for DockerGateReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message())
    }
}

/// The typed replacement for a bare `docker_available()` bool: `Ok(())` iff `docker info`
/// actually succeeds, `Err(reason)` naming which of the two ways that can fail (not installed,
/// vs installed but the daemon will not answer) -- the two cases the pre-R6.3 helpers already
/// conflated into one `bool`/`String`.
pub fn docker_daemon_status() -> Result<(), DockerGateReason> {
    match Command::new("docker").arg("info").output() {
        Err(_) => Err(DockerGateReason::DockerNotInstalled),
        Ok(output) if !output.status.success() => Err(DockerGateReason::DockerDaemonUnreachable { detail: String::from_utf8_lossy(&output.stderr).trim().to_string() }),
        Ok(_) => Ok(()),
    }
}

/// Env var (question 194 item 5): overrides the image reference [`image_gate_status`] looks
/// for, so a test can prove the "image absent" branch deterministically by pointing this at a
/// name that cannot exist -- without ever touching, retagging, or removing a real tag (question
/// 185's amendment: "a test suite must never retag, push or remove an image tag it did not
/// create in that run").
pub const IMAGE_OVERRIDE_ENV: &str = "AV_DOCKER_TEST_IMAGE_OVERRIDE";

/// `default_image_ref`, unless [`IMAGE_OVERRIDE_ENV`] is set to a non-empty value, in which
/// case that value is used instead.
pub fn resolve_image_ref(default_image_ref: &str) -> String {
    std::env::var(IMAGE_OVERRIDE_ENV).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| default_image_ref.to_string())
}

/// `Ok(())` iff the image [`resolve_image_ref`] names (`default_image_ref`, or
/// [`IMAGE_OVERRIDE_ENV`]'s override) can actually be run right now: the daemon is reachable
/// AND `docker image inspect` resolves it. `build_hint` is folded into
/// [`DockerGateReason::ImageNotBuilt`] verbatim -- pass the exact command that builds the image.
pub fn image_gate_status(default_image_ref: &str, build_hint: &str) -> Result<(), DockerGateReason> {
    docker_daemon_status()?;
    let image_ref = resolve_image_ref(default_image_ref);
    let ok = Command::new("docker").args(["image", "inspect", &image_ref, "--format={{.Id}}"]).output().map(|o| o.status.success()).unwrap_or(false);
    if !ok {
        return Err(DockerGateReason::ImageNotBuilt { image_ref, build_hint: build_hint.to_string() });
    }
    Ok(())
}

/// Env var (question 194 item 4): when set to a truthy value, [`announce_gate_skip`] does not
/// skip at all -- it panics instead, turning what would otherwise be an invisible-by-default
/// pass into a hard failure, so a gate that is SUPPOSED to cover the container/image path can
/// demand it rather than silently accept a skip. Precedent: `AV_CFS_RUN_REPRO_BUILD`
/// (`services/cfs/tests/test_image_reproducibility.py`), the existing opt-in this repeats the
/// shape of on the Rust side.
pub const REQUIRE_DOCKER_TESTS_ENV: &str = "AV_REQUIRE_DOCKER_TESTS";

/// The one parser of a truthy flag value: `1`/`true`/`yes`, trimmed, case-insensitive. Pure,
/// so it is testable without touching the process environment (question 199).
fn truthy(value: &str) -> bool {
    matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes")
}

fn truthy_env(name: &str) -> bool {
    std::env::var(name).map(|v| truthy(&v)).unwrap_or(false)
}

/// `true` iff [`REQUIRE_DOCKER_TESTS_ENV`] is set to `1`/`true`/`yes` (case-insensitive).
pub fn require_docker_tests() -> bool {
    truthy_env(REQUIRE_DOCKER_TESTS_ENV)
}

/// Announces a gated test's inability to run against the real thing, in a way genuinely visible
/// in a plain `cargo test` run (this module's own R6_3_REPORT.md section 1: a raw stderr write,
/// never `println!`/`eprintln!`, which are captured and invisible for a passing test). Returns
/// the exact line it wrote, so the caller can assert on it (question 194: "the helper returns
/// the announced text, and the test asserts it printed") -- the old defect this replaces was a
/// test body that found no image and merely returned, with nothing asserted at all.
///
/// If [`require_docker_tests`] is set, this never returns normally: it panics, turning the skip
/// into a hard failure (question 194 item 4).
#[track_caller]
pub fn announce_gate_skip(test_name: &str, reason: &DockerGateReason) -> String {
    announce_gate_skip_with(require_docker_tests(), test_name, reason)
}

/// [`announce_gate_skip`] with the require flag passed in rather than read from the process
/// environment (question 199): the public entry point reads the environment exactly once;
/// tests of the panic path call this directly and never mutate `std::env`, because a test
/// that sets a process-wide variable races every parallel test that reads it.
#[track_caller]
pub(crate) fn announce_gate_skip_with(require: bool, test_name: &str, reason: &DockerGateReason) -> String {
    if require {
        panic!("{REQUIRE_DOCKER_TESTS_ENV}=1 is set and {test_name} cannot run against the real thing: {reason}");
    }
    let line = format!("SKIPPED {test_name}: {reason}\n");
    write_real_stderr(&line);
    line
}

/// Same contract as [`announce_gate_skip`], for a gated test whose own readiness depends on
/// more than one independent precondition (e.g. a cross-binding comparison test that needs both
/// a posix-container image AND a set of Renode files) -- joins every reason's own `.message()`
/// with `"; "` into one line. Panics instead of returning under [`require_docker_tests`],
/// exactly like the single-reason form.
#[track_caller]
pub fn announce_gate_skip_multi(test_name: &str, reasons: &[DockerGateReason]) -> String {
    announce_gate_skip_multi_with(require_docker_tests(), test_name, reasons)
}

/// See [`announce_gate_skip_with`].
#[track_caller]
pub(crate) fn announce_gate_skip_multi_with(require: bool, test_name: &str, reasons: &[DockerGateReason]) -> String {
    let joined = reasons.iter().map(DockerGateReason::message).collect::<Vec<_>>().join("; ");
    if require {
        panic!("{REQUIRE_DOCKER_TESTS_ENV}=1 is set and {test_name} cannot run against the real thing: {joined}");
    }
    let line = format!("SKIPPED {test_name}: {joined}\n");
    write_real_stderr(&line);
    line
}

/// The one place this module writes directly to the process's real stderr file descriptor,
/// bypassing libtest's output capture (see [`announce_gate_skip`]'s own doc comment). Uses
/// `std::io::Write::write_all` on the `Stderr` handle directly -- never the `eprintln!`/
/// `eprint!` macros, which route through `io::_eprint` and ARE captured.
fn write_real_stderr(text: &str) {
    use std::io::Write;
    let mut stderr = std::io::stderr();
    let _ = stderr.write_all(text.as_bytes());
    let _ = stderr.flush();
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
///
/// **Question 194 item 6 / question 185's amendment.** The image half used to be `docker images
/// -q --filter label=... | xargs docker rmi -f <ID>` -- removal **by bare image ID**. Measured
/// directly (this crate's own R6_3_REPORT.md section 4): `docker rmi -f <IMAGE ID>` is
/// documented, by-design Docker behaviour that removes *every* repository:tag reference
/// pointing at that image ID in one call, not only the one reference this sweep is tracking --
/// confirmed with a real two-tag probe image losing both tags to a single such call. So a
/// labelled test image that ever carries a second tag (from any source, not only this sweep's
/// own operations) would have that second tag silently destroyed too -- exactly what question
/// 185's amendment forbids ("never retag, push or remove an image tag it did not create in that
/// run"). Fixed by removing **by reference** (`<repository>:<tag>`) instead: see
/// [`image_removal_targets`], the pure function this delegates to, for the exact rule (and its
/// own doc comment for why a truly dangling image is the one case that still falls back to its
/// bare ID -- safely, because it has no other tag to strip). This is a fix for the *latent*
/// hazard the manager's own probe demonstrated, not a claim about the cause of this host's
/// separately-investigated vanished images (question 194's own record: that cause was a
/// bulk host-level image prune, unrelated to this function).
pub fn prune_stale_test_resources() {
    let filter = format!("label={TEST_LABEL_KEY}");
    if let Ok(out) = Command::new("docker").args(["ps", "-aq", "--filter", &filter]).output() {
        for id in String::from_utf8_lossy(&out.stdout).lines().map(str::trim).filter(|l| !l.is_empty()) {
            let _ = Command::new("docker").args(["rm", "-f", id]).output();
        }
    }
    if let Ok(out) = Command::new("docker").args(["images", "--filter", &filter, "--format", "{{.ID}}\t{{.Repository}}:{{.Tag}}"]).output() {
        for target in image_removal_targets(&String::from_utf8_lossy(&out.stdout)) {
            let _ = Command::new("docker").args(["rmi", "-f", &target]).output();
        }
    }
}

/// Parses `docker images --filter label=<TEST_LABEL_KEY> --format '{{.ID}}\t{{.Repository}}:
/// {{.Tag}}'` output into the exact strings [`prune_stale_test_resources`] passes to `docker
/// rmi -f` -- pure and `docker`-free, so it is unit-testable without Docker installed, the same
/// way [`build_run_args`] below is.
///
/// **The rule (question 194 item 6):** remove by REFERENCE (`<repository>:<tag>`), one call per
/// distinct reference, never by bare image ID -- `docker rmi -f <ID>` is documented to remove
/// *every* tag pointing at that image in a single call, which can strip a tag this sweep never
/// itself examined. The one exception is a truly dangling row (`<none>:<none>`, or an empty
/// repo:tag field): it has no reference to remove by at all, so it falls back to its own ID --
/// safe there, and only there, because by definition no *other* tag can be stripped from an
/// image that has none.
fn image_removal_targets(images_tsv: &str) -> Vec<String> {
    let mut targets: Vec<String> = Vec::new();
    for line in images_tsv.lines() {
        let mut parts = line.splitn(2, '\t');
        let id = parts.next().unwrap_or("").trim();
        let repo_tag = parts.next().unwrap_or("").trim();
        if id.is_empty() {
            continue;
        }
        let target = if repo_tag.is_empty() || repo_tag == "<none>:<none>" { id.to_string() } else { repo_tag.to_string() };
        if !targets.contains(&target) {
            targets.push(target);
        }
    }
    targets
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

    // ---------------------------------------------------------------------------------------
    // Question 194 item 6: image_removal_targets -- the pure function prune_stale_test_
    // resources' image half now delegates to. Docker-free, unconditional.
    // ---------------------------------------------------------------------------------------

    /// Mirrors the manager's own two-tag probe (R6.3 report section 4, reproduced against this
    /// host's real Docker for the measurement that motivated this fix): one image ID, two
    /// distinct tag rows (exactly what `docker images --filter label=... --format ...` returns
    /// for a labelled image carrying two tags). **What a wrong implementation fails this
    /// against:** collapsing to the shared bare ID (the pre-fix shape -- `docker rmi -f <ID>`,
    /// which is documented to strip *every* tag on that image in one call) returns a
    /// single-element `["363952b07d42"]` here instead of the two references -- executed and
    /// recorded as a real failure in this crate's own R6_3_REPORT.md section 4, then restored.
    #[test]
    fn image_removal_targets_removes_by_reference_not_a_shared_bare_id() {
        let tsv = "363952b07d42\tav-r63-probe-labeled:tag1\n363952b07d42\tav-r63-probe-labeled:tag2\n";
        let targets = image_removal_targets(tsv);
        assert_eq!(
            targets,
            vec!["av-r63-probe-labeled:tag1".to_string(), "av-r63-probe-labeled:tag2".to_string()],
            "must remove each tag BY REFERENCE, one call per distinct reference, never collapsed to the shared bare image ID -- \
             docker rmi -f <ID> removes every tag on that image in one call (see this module's own doc comment on prune_stale_test_resources)"
        );
    }

    /// A dangling (untagged) labelled image -- `<none>:<none>` -- has no reference to remove by
    /// at all, so this is the one case that still falls back to its own ID. Safe only there:
    /// with no tag, there is nothing else a bare-ID `docker rmi -f` could strip.
    #[test]
    fn image_removal_targets_falls_back_to_bare_id_only_for_a_dangling_image() {
        let tsv = "abc123def456\t<none>:<none>\n";
        assert_eq!(image_removal_targets(tsv), vec!["abc123def456".to_string()]);
    }

    /// Two entirely separate labelled images (different IDs, one tag each) -- the ordinary
    /// case question 156 was built for -- both get their own single-tag reference, in the order
    /// `docker images` reported them, with no accidental deduplication across different images
    /// that happen to share a repository name substring or similar.
    #[test]
    fn image_removal_targets_handles_multiple_distinct_images() {
        let tsv = "aaa111\t127.0.0.1:54321/altavista-cfs-lockstep:test\nbbb222\tlockstep-ref:av-kernel-docker-lifecycle-test\n";
        assert_eq!(image_removal_targets(tsv), vec!["127.0.0.1:54321/altavista-cfs-lockstep:test".to_string(), "lockstep-ref:av-kernel-docker-lifecycle-test".to_string()]);
    }

    /// Blank lines (the trailing newline `docker images --format` output always carries, and a
    /// possible fully-empty invocation with nothing matching the filter) never turn into a
    /// removal target.
    #[test]
    fn image_removal_targets_ignores_blank_lines() {
        assert_eq!(image_removal_targets(""), Vec::<String>::new());
        assert_eq!(image_removal_targets("\n\n"), Vec::<String>::new());
    }

    // ---------------------------------------------------------------------------------------
    // Question 194 items 1-5: DockerGateReason / docker_daemon_status / image_gate_status /
    // resolve_image_ref / announce_gate_skip / require_docker_tests. Every one of these is
    // testable without a live Docker daemon except docker_daemon_status/image_gate_status
    // themselves, which shell out for real (still fine: no image/daemon state is touched).
    // ---------------------------------------------------------------------------------------

    /// `DockerGateReason::message()` names the resource and how to obtain it, for every
    /// variant -- at least as informative as the pre-R6.3 hand-formatted strings this replaces
    /// (`cfs_image_unavailable_reason`'s own `format!("{CFS_LOCAL_IMAGE:?} is not built locally
    /// -- run ...")` shape, reproduced here through the typed constructor instead).
    #[test]
    fn every_docker_gate_reason_variant_names_the_resource_and_how_to_fix_it() {
        assert_eq!(DockerGateReason::DockerNotInstalled.message(), "`docker` is not installed or not on PATH");
        assert!(DockerGateReason::DockerDaemonUnreachable { detail: "Cannot connect to the Docker daemon".to_string() }.message().contains("Cannot connect to the Docker daemon"));
        let image_not_built = DockerGateReason::ImageNotBuilt { image_ref: "altavista-cfs-lockstep:local".to_string(), build_hint: "run `docker build -f services/cfs/Dockerfile -t altavista-cfs-lockstep:local .`".to_string() };
        let msg = image_not_built.message();
        assert!(msg.contains("altavista-cfs-lockstep:local"), "{msg:?}");
        assert!(msg.contains("docker build"), "{msg:?}");
        let missing_file = DockerGateReason::RequiredFileMissing { what: "the Renode binary".to_string(), path: "/some/path".to_string() };
        assert_eq!(missing_file.message(), "the Renode binary is missing at /some/path");
        let missing_prereq = DockerGateReason::PrerequisiteUnavailable { what: "a working Python + protobuf".to_string(), hint: "run the README's Python setup".to_string() };
        assert_eq!(missing_prereq.message(), "a working Python + protobuf is unavailable -- run the README's Python setup");
        // Display must render the same text (announce_gate_skip formats reasons through
        // Display, via `format!("SKIPPED {test_name}: {reason}")`).
        assert_eq!(image_not_built.to_string(), image_not_built.message());
    }

    /// `resolve_image_ref` (question 194 item 5): the override env var, when set to a
    /// non-empty value, wins over the default -- exactly what lets a test point at a name that
    /// cannot exist to prove the "image absent" branch deterministically, without touching a
    /// real tag. Empty-string is treated as unset (an accidentally-exported-but-empty var must
    /// not silently make every image-gated test look for an empty image reference).
    #[test]
    fn resolve_image_ref_prefers_a_non_empty_override_and_ignores_an_empty_one() {
        // std::env is process-global; run serially within this test via a small critical
        // section (save/restore) so it cannot race the other env-reading tests in this file
        // under `cargo test`'s default multi-threaded runner.
        let _guard = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = std::env::var(IMAGE_OVERRIDE_ENV).ok();

        std::env::remove_var(IMAGE_OVERRIDE_ENV);
        assert_eq!(resolve_image_ref("altavista-cfs-lockstep:local"), "altavista-cfs-lockstep:local");

        std::env::set_var(IMAGE_OVERRIDE_ENV, "this-image-reference-cannot-possibly-exist:av-r63-probe");
        assert_eq!(resolve_image_ref("altavista-cfs-lockstep:local"), "this-image-reference-cannot-possibly-exist:av-r63-probe");

        std::env::set_var(IMAGE_OVERRIDE_ENV, "");
        assert_eq!(resolve_image_ref("altavista-cfs-lockstep:local"), "altavista-cfs-lockstep:local", "an empty override must be treated as unset");

        match saved {
            Some(v) => std::env::set_var(IMAGE_OVERRIDE_ENV, v),
            None => std::env::remove_var(IMAGE_OVERRIDE_ENV),
        }
    }

    /// `image_gate_status`, pointed at [`IMAGE_OVERRIDE_ENV`] set to a name that cannot exist,
    /// deterministically exercises the "image absent" branch -- proving that branch without
    /// ever touching, retagging, or removing a real image tag (question 185's amendment).
    /// Requires a real, reachable Docker daemon (this host has one; if it does not, this test
    /// itself is skipped visibly through the same mechanism it is testing -- see its own body).
    #[test]
    fn image_gate_status_reports_image_not_built_for_a_deliberately_nonexistent_override() {
        let _guard = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        if docker_daemon_status().is_err() {
            // Consistent with every other gated test in this workspace: this unit test needs a
            // real daemon to distinguish "image absent" from "daemon absent" at all, so it
            // skips through the very mechanism under test rather than failing on a host with no
            // Docker.
            let line = announce_gate_skip("image_gate_status_reports_image_not_built_for_a_deliberately_nonexistent_override", &docker_daemon_status().unwrap_err());
            assert!(line.starts_with("SKIPPED "));
            return;
        }
        let saved = std::env::var(IMAGE_OVERRIDE_ENV).ok();
        std::env::set_var(IMAGE_OVERRIDE_ENV, "this-image-reference-cannot-possibly-exist:av-r63-probe");
        let result = image_gate_status("altavista-cfs-lockstep:local", "run `docker build ...`");
        match saved {
            Some(v) => std::env::set_var(IMAGE_OVERRIDE_ENV, v),
            None => std::env::remove_var(IMAGE_OVERRIDE_ENV),
        }
        match result {
            Err(DockerGateReason::ImageNotBuilt { image_ref, .. }) => {
                assert_eq!(image_ref, "this-image-reference-cannot-possibly-exist:av-r63-probe");
            }
            other => panic!("expected DockerGateReason::ImageNotBuilt for a deliberately nonexistent override, got {other:?}"),
        }
    }

    /// `announce_gate_skip` returns the exact text it wrote (question 194: "the helper returns
    /// the announced text, and the test asserts it printed") -- the return value is what a real
    /// gated test asserts on; the actual real-stderr visibility of that same text is
    /// established empirically in this crate's own R6_3_REPORT.md section 1, and covered end to
    /// end by `crates/av-lockstep/tests/docker_lifecycle.rs`'s own visibility-proof test (a
    /// subprocess `cargo test` whose real captured stdout is inspected directly).
    #[test]
    fn announce_gate_skip_returns_the_line_it_announced() {
        let reason = DockerGateReason::ImageNotBuilt { image_ref: "some/image:tag".to_string(), build_hint: "run the build script".to_string() };
        let line = announce_gate_skip("some_test_name", &reason);
        assert_eq!(line, "SKIPPED some_test_name: image \"some/image:tag\" is not built locally -- run the build script\n");
    }

    /// `announce_gate_skip_multi` joins every reason's own message with `"; "` -- the shape
    /// `drm_attitude_control_renode.rs`'s own two-precondition (posix-container image + Renode
    /// files) test needs, now through the shared helper instead of a hand-rolled `Vec<String>`
    /// join.
    #[test]
    fn announce_gate_skip_multi_joins_every_reason() {
        let reasons = vec![
            DockerGateReason::ImageNotBuilt { image_ref: "altavista-cfs-lockstep:local".to_string(), build_hint: "run the build script".to_string() },
            DockerGateReason::RequiredFileMissing { what: "the Renode binary".to_string(), path: "/some/path".to_string() },
        ];
        let line = announce_gate_skip_multi("some_cross_binding_test", &reasons);
        assert_eq!(
            line,
            "SKIPPED some_cross_binding_test: image \"altavista-cfs-lockstep:local\" is not built locally -- run the build script; the Renode binary is missing at /some/path\n"
        );
    }

    /// `AV_REQUIRE_DOCKER_TESTS=1` turns `announce_gate_skip` into a panic instead of a skip
    /// (question 194 item 4). **What this fails against:** an implementation that ignores the
    /// env var (the pre-item-4 shape) would return the announced-text `String` here instead of
    /// unwinding, and `catch_unwind` would observe `Ok(_)`, not `Err(_)`.
    ///
    /// Question 199: this test used to `set_var(AV_REQUIRE_DOCKER_TESTS, "1")` on the process
    /// and restore it afterwards. The two announce tests above read that variable and do not
    /// hold `ENV_TEST_LOCK`, so under `cargo test`'s parallel runner they intermittently ran
    /// inside that window and panicked with "AV_REQUIRE_DOCKER_TESTS=1 is set" (reproduced in
    /// the lead's clone gate, 2026-09-10). The require flag is now injected instead.
    #[test]
    fn require_docker_tests_turns_a_skip_into_a_panic() {
        let reason = DockerGateReason::DockerNotInstalled;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| announce_gate_skip_with(true, "some_required_test", &reason)));
        assert!(result.is_err(), "the require flag must turn announce_gate_skip into a panic, not a returned skip line");
        let multi = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| announce_gate_skip_multi_with(true, "some_required_multi_test", &[reason])));
        assert!(multi.is_err(), "the require flag must turn announce_gate_skip_multi into a panic too");
    }

    /// The flag parser itself, without the environment: exactly `1`/`true`/`yes` (trimmed,
    /// case-insensitive) are truthy.
    #[test]
    fn truthy_flag_parsing() {
        for v in ["1", "true", "yes", " TRUE ", "Yes"] {
            assert!(truthy(v), "{v:?} must be truthy");
        }
        for v in ["", "0", "false", "no", "on", "2"] {
            assert!(!truthy(v), "{v:?} must not be truthy");
        }
    }

    /// Serializes the small handful of tests above that mutate process-wide env vars
    /// (`IMAGE_OVERRIDE_ENV`/`REQUIRE_DOCKER_TESTS_ENV`) -- `cargo test` runs `#[test]`s
    /// concurrently by default, and `std::env::set_var`/`remove_var` are process-global.
    static ENV_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
