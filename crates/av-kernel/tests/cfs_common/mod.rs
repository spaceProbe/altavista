//! Shared test support for the cFS-container tests (`tests/drm_attitude_control_cfs.rs` and
//! `tests/drm_attitude_control_renode.rs`; question 245 (7), `docs/sil-plan.md`'s 2026-10-07
//! "Found, not fixed" 4): the Docker plumbing that pushes the already-built cFS image to a
//! throwaway local registry, the image override and gate, the fixture loaders and the truth
//! pointing-error helpers. Moved here from the two files, which each carried a copy.
//!
//! Each test binary uses a different subset, so the module allows `dead_code` as
//! `tests/drm_board_common/mod.rs` does.
#![allow(dead_code)]

use std::path::PathBuf;
use std::process::Command;

use av_cdm::pb::{DesignReferenceMission, Fault, SystemDefinition};
use av_kernel::drm::{hash, schema, RunProducts};
use av_lockstep::docker::{test_label_args, DockerGateReason};

// ------------------------------------------------------------------------------------------
// Fixture loading -- mirrors crates/av-kernel/tests/drm_attitude_control.rs's own helpers.
// ------------------------------------------------------------------------------------------

pub fn drms_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../drms").join(name)
}
pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}
pub fn read(name: &str) -> String {
    std::fs::read_to_string(drms_path(name)).unwrap_or_else(|e| panic!("reading drms/{name}: {e}"))
}
pub fn load_system(stem: &str) -> SystemDefinition {
    let mut sys = schema::parse_system_definition_yaml(&read(&format!("{stem}.system.yaml"))).unwrap_or_else(|e| panic!("{stem}.system.yaml: {e}"));
    sys.hash = hash::canonical_system_hash(&sys);
    sys
}

/// A DRM over `duration_s` seconds at the fixture's own 10 Hz, no objectives/measures (the
/// container path has no `output.controller.*` to reference -- see
/// `drm_attitude_control_cfs.rs`'s module doc comment) -- every assertion instead reads
/// `RunProducts.trajectories`/`.events` directly.
pub fn container_drm(id: &str, sos_id: &str, duration_s: i64, faults: Vec<Fault>) -> DesignReferenceMission {
    let mut drm = schema::parse_drm_yaml(&read("demo_attitude_control.drm.yaml")).expect("native DRM parses");
    drm.id = id.to_string();
    drm.sos_configuration_id = sos_id.to_string();
    drm.objectives.clear();
    drm.measures.clear();
    {
        let scenario = drm.scenario.as_mut().expect("native DRM declares a scenario");
        scenario.end_tai_ns = scenario.start_tai_ns + duration_s * 1_000_000_000;
        scenario.faults = faults;
        // `drms/demo_attitude_control_controller_cfs.system.yaml`'s own `container.seed_key =
        // "controller"` (ADR-004 "seeds are logged inputs" applied uniformly to every
        // container-bound instance, question 107 -- unused by services/cfs/apps/adcs today,
        // which draws no randomness, but still a required declared seed).
        scenario.seeds.insert("controller".to_string(), 42);
    }
    drm.hash = hash::canonical_drm_hash(&drm);
    drm
}

// ------------------------------------------------------------------------------------------
// Docker plumbing -- the image is already built (services/cfs/Dockerfile's own top comment has
// the exact `docker build`/prebuild-the-shim commands); the tests only tag/push it to a
// throwaway, loopback-only local `registry:2` (never a real/external registry -- the same
// pattern `crates/av-kernel/tests/drm_container.rs::docker_image_lifecycle_through_execute_...`
// already establishes) so `av_lockstep::docker::ManagedContainer::pull_and_run`'s own `docker
// pull <image>@<digest>` has something real to resolve -- no network beyond loopback, and never
// a rebuild, at test time (this task's own environment rule).
// ------------------------------------------------------------------------------------------

pub const CFS_LOCAL_IMAGE: &str = "altavista-cfs-lockstep:local";
/// hilprep-6: `AV_CFS_TEST_IMAGE` names another local image to run these tests against (default
/// [`CFS_LOCAL_IMAGE`], unchanged). It exists so the app code can be tested from a run-scoped
/// test-only tag built from a working tree without touching `altavista-cfs-lockstep:local`, the
/// image whose digest `services/cfs/IMAGE_DIGEST.md` records. Honoured by both cFS test files
/// (native-ct-6: the Renode test's posix half used to ignore it).
pub const CFS_IMAGE_OVERRIDE_ENV: &str = "AV_CFS_TEST_IMAGE";

pub fn cfs_image() -> String {
    std::env::var(CFS_IMAGE_OVERRIDE_ENV).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| CFS_LOCAL_IMAGE.to_string())
}

pub const CFS_LOCAL_IMAGE_BUILD_HINT: &str = "run `docker build -f services/cfs/Dockerfile -t altavista-cfs-lockstep:local .` from the repository root once (services/cfs/Dockerfile's own top comment has the av-lockstep-shim prebuild step this needs first)";

/// Question 194: typed, not a bare `String` -- see `crates/av-lockstep/src/docker.rs`'s
/// `DockerGateReason`/`image_gate_status`. `None` iff [`cfs_image`] is present locally; `hint` is
/// the build hint printed when it is not (each test file keeps its own wording).
pub fn cfs_image_unavailable_reason(hint: &str) -> Option<DockerGateReason> {
    av_lockstep::docker::image_gate_status(&cfs_image(), hint).err()
}

/// Question 194 (M23.4 round 5-6): the pre-R6.3 shape here was `println!("SKIPPED ...")` then
/// `return` -- invisible in a plain `cargo test` run (`cargo test`'s default runner never
/// prints a PASSING test's `println!` output without `--nocapture`, measured directly in
/// `crates/av-lockstep/R6_3_REPORT.md` section 1), which is exactly how these four tests once
/// reported `ok` in 0.17s/37.84s with no image present. Now routes through
/// `av_lockstep::docker::image_gate_status` (a typed `DockerGateReason`, not a bare `String`)
/// and `announce_gate_skip` (a real stderr write, never `println!`/`eprintln!`), and asserts on
/// the text the helper actually announced -- a test body that finds no image and merely
/// `return`s, with nothing asserted, is exactly the defect question 194 requires fixed. Returns
/// `true` iff the calling test must skip.
pub fn skip_if_cfs_image_unavailable(test_name: &str) -> bool {
    match cfs_image_unavailable_reason(CFS_LOCAL_IMAGE_BUILD_HINT) {
        None => false,
        Some(reason) => {
            let line = av_lockstep::docker::announce_gate_skip(test_name, &reason);
            assert!(line.starts_with("SKIPPED ") && line.contains(test_name), "the gate helper must announce a visible skip line naming this test: {line:?}");
            true
        }
    }
}

pub fn docker_cmd(args: &[&str]) -> String {
    let output = Command::new("docker").args(args).current_dir(repo_root()).output().unwrap_or_else(|e| panic!("could not launch `docker {args:?}`: {e}"));
    if !output.status.success() {
        panic!("`docker {args:?}` failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

pub struct DockerContainerGuard(pub String);
impl Drop for DockerContainerGuard {
    fn drop(&mut self) {
        let _ = Command::new("docker").args(["rm", "-f", &self.0]).output();
    }
}
pub struct DockerImageGuard(pub String);
impl Drop for DockerImageGuard {
    fn drop(&mut self) {
        let _ = Command::new("docker").args(["rmi", "-f", &self.0]).output();
    }
}

/// Tags and pushes the already-built `altavista-cfs-lockstep:local` (or the [`cfs_image`]
/// override) to a throwaway local registry (loopback-only -- `docker run -p
/// 127.0.0.1:<FIXED_REGISTRY_PORT>:5000 registry:2`, a FIXED port so the image reference is the
/// same in every run, question 239), labeled per question 156 (`test_label_args`) so a killed
/// test's own registry container is swept by `prune_stale_test_resources` on the *next* run even
/// if this run's own `Drop` guards never get to fire. Returns `(image_ref, real_digest, guards)`
/// -- `real_digest` is Docker's own, recovered from `docker inspect` after the push (never
/// invented/assumed, matching `av_lockstep::docker`'s own module doc comment's "what pulled by
/// digest means here, honestly").
///
/// **Why the pushed `:test` tag itself is not, and must never be, labeled -- proved, not just
/// asserted (round 5).** `docker tag` has no `--label` flag: it creates an alias (a second
/// `repository:tag` reference), never a new image object. Confirmed directly on this host:
/// `docker image inspect altavista-cfs-lockstep:local --format '{{.Id}}'` and
/// `docker image inspect <the tag this function pushes> --format '{{.Id}}'` print the IDENTICAL
/// `sha256:...` id (captured in this task's own round-5 report). That id is the same persistent,
/// host-owned `altavista-cfs-lockstep:local` build artifact [`cfs_image_unavailable_reason`]
/// requires present before any of the tests even attempt to run -- so labeling the pushed tag
/// would label that SAME image object. `prune_stale_test_resources`'s image half removes a
/// labeled image by reference where it can, but falls back to bare-ID removal for a dangling
/// image, and (question 194 item 6's own measured finding, `crates/av-lockstep/R6_3_REPORT.md`
/// section 4) `docker rmi -f <IMAGE ID>` is documented, by-design Docker behaviour that strips
/// EVERY repository:tag pointing at that id in one call -- so even an indirect path to that
/// bare-ID fallback would risk deleting `:local` along with the labeled tag. Labeling the tag is
/// therefore not merely unnecessary but actively dangerous: it would make the very next test
/// run's own pre-flight prune capable of destroying this host's cFS build artifact. The registry
/// CONTAINER below is this helper's own actual expensive/stateful leaked resource (question
/// 156's real four-hour incident was a running container, not a dangling tag) -- and that one
/// alone is labeled.
pub fn push_cfs_image_to_local_registry(run_id: &str) -> (String, String, (DockerContainerGuard, DockerImageGuard)) {
    let labels = test_label_args(run_id);
    let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();

    // Question 239 / sil-plan "Found, not fixed" 1: the registry is published on the FIXED
    // loopback port `FIXED_REGISTRY_PORT`, never an ephemeral one, so the `ContainerBinding.image`
    // string -- hashed with the whole `SosConfiguration` into the run's provenance and the
    // `port_traffic.pb` sidecar (ADR-005 section 7: the canonical hash covers every field but
    // `hash`) -- is the same string in every run, whichever throwaway registry container serves
    // it. The docker-test lock (held by every caller) serialises the registries, so at most one
    // test-owned registry holds the port at a time; whatever else holds it is reported, never
    // worked around.
    wait_for_fixed_registry_port_free().unwrap_or_else(|e| panic!("{e}"));
    let port_mapping = format!("127.0.0.1:{FIXED_REGISTRY_PORT}:5000");
    let mut registry_args = vec!["run", "-d", "-p", port_mapping.as_str()];
    registry_args.extend(label_refs.iter().copied());
    registry_args.push("registry:2");
    let output = Command::new("docker").args(&registry_args).current_dir(repo_root()).output().unwrap_or_else(|e| panic!("could not launch `docker {registry_args:?}`: {e}"));
    if !output.status.success() {
        panic!("{}", FixedRegistryError::StartFailed { port: FIXED_REGISTRY_PORT, detail: String::from_utf8_lossy(&output.stderr).trim().to_string() });
    }
    let registry_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let registry_guard = DockerContainerGuard(registry_id);
    let image_ref = format!("127.0.0.1:{FIXED_REGISTRY_PORT}/altavista-cfs-lockstep");
    let tagged = format!("{image_ref}:test");
    // `docker tag` has no `--label` flag (it creates an alias to an existing image object, not a
    // new one) -- see this function's own doc comment above for the proof that this tag and
    // CFS_LOCAL_IMAGE resolve to the same id, and why that makes labeling it dangerous rather
    // than merely impossible.
    docker_cmd(&["tag", &cfs_image(), &tagged]);
    let image_guard = DockerImageGuard(tagged.clone());
    // The registry container has only just been started: retry the push briefly until the
    // registry process inside it accepts connections.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let out = Command::new("docker").args(["push", &tagged]).current_dir(repo_root()).output().unwrap_or_else(|e| panic!("could not launch `docker push {tagged}`: {e}"));
        if out.status.success() {
            break;
        }
        if std::time::Instant::now() >= deadline {
            panic!("`docker push {tagged}` did not succeed within 30 s: {}", String::from_utf8_lossy(&out.stderr));
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    // The digest from the RepoDigests entry for THIS reference, not index 0 (the image may carry
    // RepoDigests from other registries).
    let repo_digests = docker_cmd(&["inspect", "--format={{range .RepoDigests}}{{println .}}{{end}}", &tagged]);
    let prefix = format!("{image_ref}@");
    let digest = repo_digests.lines().find_map(|l| l.strip_prefix(prefix.as_str())).filter(|d| d.starts_with("sha256:")).unwrap_or_else(|| panic!("a {prefix}sha256:... RepoDigests entry, got {repo_digests:?}")).to_string();
    (image_ref, digest, (registry_guard, image_guard))
}

/// The loopback port the throwaway registries are published on. Fixed (not an ephemeral
/// `-p 127.0.0.1::5000`) so `ContainerBinding.image` is a stable string -- see
/// [`push_cfs_image_to_local_registry`]. Chosen below every OS ephemeral range (macOS 49152+,
/// Linux 32768+) and away from 5000 (macOS AirPlay Receiver holds it).
pub const FIXED_REGISTRY_PORT: u16 = 19031;

/// Why a test-owned registry could not be started on [`FIXED_REGISTRY_PORT`]. Never recovered by
/// choosing another port: a different port would change `ContainerBinding.image` and, with it,
/// the run's `sos_configuration_hash`.
#[derive(Debug)]
pub enum FixedRegistryError {
    /// Something is still listening on the port after the grace period (a registry this test
    /// owns is removed by its guard and by the labeled-container prune; this is something else).
    PortOccupied { port: u16 },
    /// `docker run` refused to start the registry on the port.
    StartFailed { port: u16, detail: String },
}
impl std::fmt::Display for FixedRegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FixedRegistryError::PortOccupied { port } => write!(f, "FixedRegistryError::PortOccupied: 127.0.0.1:{port} is in use by something that is not this test's registry; free it (this test never falls back to another port, which would change ContainerBinding.image and the sos_configuration_hash)"),
            FixedRegistryError::StartFailed { port, detail } => write!(f, "FixedRegistryError::StartFailed: `docker run` could not publish the registry on 127.0.0.1:{port}: {detail}"),
        }
    }
}

/// Waits (up to 15 s) for nothing to accept connections on `127.0.0.1:FIXED_REGISTRY_PORT`: a
/// previous test-owned registry that was just removed releases the port a moment later.
pub fn wait_for_fixed_registry_port_free() -> Result<(), FixedRegistryError> {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], FIXED_REGISTRY_PORT));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        if std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(300)).is_err() {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(FixedRegistryError::PortOccupied { port: FIXED_REGISTRY_PORT });
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
}

// ------------------------------------------------------------------------------------------
// Truth-based pointing error (drm_attitude_control_cfs.rs's module doc comment explains why
// truth, not output.controller.pointing_error_rad).
// ------------------------------------------------------------------------------------------

/// `drms/demo_attitude_control_truth.system.yaml`'s own declared state-space order:
/// `[q_x, q_y, q_z, q_w, body_rate_x, body_rate_y, body_rate_z, wheel_h_1, wheel_h_2, wheel_h_3]`.
pub const TRUTH_QX: usize = 0;
pub const TRUTH_QY: usize = 1;
pub const TRUTH_QZ: usize = 2;
pub const TRUTH_QW: usize = 3;

/// `2*asin(|q_v|)` -- the exact formula `crate::drm::controller::AttitudeControllerModel`'s own
/// `pointing_error_rad` output uses (`2.0 * vnorm.asin()`), applied here to the *truth*
/// quaternion instead of a measured one. `target_q` is the identity in every fixture these tests
/// use, so no `q_err = target_q^-1 (x) q` composition is needed -- the state IS the error.
pub fn truth_pointing_error_rad(mean: &[f64]) -> f64 {
    let (qx, qy, qz) = (mean[TRUTH_QX], mean[TRUTH_QY], mean[TRUTH_QZ]);
    let vnorm = (qx * qx + qy * qy + qz * qz).sqrt();
    2.0 * vnorm.clamp(-1.0, 1.0).asin()
}

/// The `"attitude"` truth instance's mean state at its last sample.
pub fn truth_last_mean(products: &RunProducts) -> &[f64] {
    let traj = products.trajectories.get("attitude").expect("the \"attitude\" truth instance produced a trajectory");
    let last = traj.samples.last().expect("at least one sample");
    &last.mean
}

/// [`truth_pointing_error_rad`] at the last sample, with no further check (the cFS test file wraps
/// it with an `acos` cross-check of its own; the Renode test compares two runs byte for byte,
/// which already subsumes it).
pub fn truth_pointing_error_rad_at_end(products: &RunProducts) -> f64 {
    truth_pointing_error_rad(truth_last_mean(products))
}

/// Lowercase-hex SHA-256 (`openssl`, a dependency of this crate; `sha2` is banned workspace-wide),
/// the same function the executor hashes the sidecar with.
pub fn sha256_hex(bytes: &[u8]) -> String {
    openssl::sha::sha256(bytes).iter().map(|b| format!("{b:02x}")).collect()
}
