//! M24.4c (`docs/sil-plan.md`'s M24 milestone: "identical port traffic, posix container against
//! Renode, under lockstep"; `docs/open-questions.md` questions 145, 153, 156, 157, 164) --
//! closes M24: the `"controller"` instance of `drms/demo_attitude_control.*.yaml`'s attitude
//! control loop, run once bound (via `crates/av-kernel/tests/drm_attitude_control_cfs.rs`'s own
//! `BINDING_KIND_CONTAINER`/`ContainerBinding.image` path) to the real `altavista-cfs-lockstep`
//! posix-container image, and once bound (M13.2's `container.address`-only already-running-
//! process path -- `drms/demo_attitude_control_controller_renode.system.yaml`'s own header
//! comment) to the real, root-cause-fixed RTEMS 6 `zynqmp_rpu_lock_step` `core-cpu1.exe` running
//! under Renode, fronted by `crates/av-lockstep-shim` + `third_party/renode/M24_4b/
//! renode_bridge.py` (`third_party/renode/M24_4b_REPORT.md`) as plain child processes.
//!
//! **This is a cross-binding comparison, not `drm_attitude_control_cfs.rs`'s own within-binding
//! determinism test** (two separately spawned posix containers against the identical DRM). Both
//! runs here execute the byte-identical `services/cfs/apps/adcs` C source -- only the OSAL/
//! platform underneath differs (question 147: "same app source, different OSAL; the P2
//! identical-traffic criterion becomes a portability test"). See `third_party/renode/
//! M24_4c_REPORT.md` for the full design rationale, wall-clock budget reasoning (stated before
//! measuring), and the measured result.
//!
//! **The identical-traffic criterion, as asserted (question 145; q171-d).** Both halves run with
//! `RunConfig.products_dir` set, so the executor writes each run's `port_traffic.pb`
//! (`PortTrafficLog`, every FRAMED/BYTE_STREAM frame carried: instance, port, direction, epoch,
//! payload bytes, step sequence) and reports its SHA-256 as `RunProducts.port_traffic_hash`. The
//! two whole-file hashes are not equal and cannot be: besides the records the sidecar carries the
//! run's own `run_id` (twice), `provenance.config_hash` (the DRM hash) and
//! `provenance.attributes["sos_configuration_hash"]` (a `SosConfiguration` whose `"controller"`
//! is bound to a Docker image in one run and to a `container.address` in the other). So the test
//! asserts, in this order: both hashes non-empty and equal to the SHA-256 of the file written;
//! every decoded record equal, in order (998 records over the 100 steps: 6 ports); the same
//! records re-encoded alone hash identically; and the rest of the sidecar equal after
//! blanking exactly those named fields ([`SIDECAR_PROVENANCE_EXCLUSIONS`]). `products.events` is
//! compared field by field with exactly [`EVENT_PROVENANCE_EXCLUSIONS`] excluded.
//!
//! **Wall time, measured** (Apple silicon, `core-cpu1.exe` from the 2026-10-05 rebuild; the
//! bridge grants every STEP `STEP_MIN_VIRTUAL_S = 10 s` of virtual time in 0.5 s chunks, which
//! dominates):
//!
//! - full grants (`AV_BRIDGE_STEP_EARLY_STOP=0`): 100 STEPs in 2153 s of Renode run, 2187 s (36.4
//!   min) for the whole test (posix half 14.6 s, bridge ready 18.1 s);
//! - early stop (the test's default: the bridge stops granting a STEP's virtual time as soon as
//!   the whole reply is buffered on the hook socket, between 0.5 s chunks; `AV_BRIDGE_STEP_EARLY_STOP`
//!   in the environment overrides it): 280 s of Renode run, 305 s (5.1 min) for the whole test.
//!
//! **The A/B that justifies the early stop** (same 10 s arc, same ELF, same run ids, 2026-10-05):
//! the Renode sidecar `port_traffic.pb` is byte-identical between the two runs (SHA-256
//! `8a16007f...7fff` both, 83694 bytes, 998 records), the records-only hash is
//! `8e518964...8fd2` in all four sidecars, and the truth pointing error at t = 10 s is
//! 1.823926917407e-1 rad in all four. (The *posix* whole-file hash differs between those two
//! runs, `a514ea92...` against `36329922...`, and that is the sidecar's own
//! `sos_configuration_hash`: the posix `SosConfiguration` embedded the throwaway registry's
//! ephemeral port in `ContainerBinding.image`; nothing about the traffic. Since question 239 the
//! registry is published on a fixed port, so the reference and the posix whole-file hash are
//! the same in every run: `drm_attitude_control_cfs.rs`'s
//! `the_port_traffic_hash_is_the_same_across_two_separately_started_registries` proves it.) The guest is
//! tick-driven (the cFE clock follows the lockstep tick, not Renode's virtual time), so the
//! virtual time granted after the reply is idle: the PC trace of the full-grant run shows one
//! distinct PC (`0x4004bb32`, the idle loop) for 99 of its 100 STEPs (all but STEP 1) -- and the
//! same 99 in the early-stop run.
//!
//! The run holds the host-wide docker-test lock (question 207) for the posix half only: the
//! lock, the stale-resource prune, the throwaway registry with its image tag and the posix run's
//! managed container all live in that half's own scope, and are dropped (registry guards first,
//! then the lock) before `spawn_renode_bridge` is called. The Renode half uses no Docker at all
//! and runs without the lock, so other tracks' Docker-gated work is not blocked for its 5-7
//! minutes. The test prints a "docker-test lock released" line and a "Renode half starting" line
//! so the order is visible in its own output. The GMAT engine lock is held for the whole body.
//!
//! **What is compared, and what is deliberately excluded.** Mirrors
//! `drm_attitude_control_cfs.rs::byte_identical_run_products_across_two_separately_spawned_cfs_containers`:
//! every trajectory's `samples`/`event_ids`/`state_space_id`/segment
//! `start_tai_ns`/`end_tai_ns`/`dynamics_model`, plus `products.events` and `products.scores`.
//! `dynamics_hash` is compared for the native (`"attitude"`/`"star_tracker"`/`"imu"`) segments
//! (same GMAT settings both runs, unaffected by how `"controller"` is bound) but excluded for
//! `"controller"`'s own segment, for the same reason the CFS test excludes it there:
//! `ModelInfo::settings_hash` folds in `container.address`, which is *inherently* different
//! between a Docker-published ephemeral port and the shim's own ephemeral gRPC port -- pure local
//! plumbing no port message or trajectory sample ever carries. `RunProducts.provenance` (top
//! level and per-trajectory) is excluded too: it embeds `run_id`/`sos_hash`/`drm_hash`/`sys_hash`,
//! which are deliberately different strings between the two runs (they bind two different
//! `SystemDefinition`s), not a determinism signal.
//!
//! **FSW internal task order: recorded, not asserted** (question 145's own decision). Neither
//! this test nor anything it reads observes cFE/RTEMS task-scheduling order at all -- only
//! `RunProducts`, the port boundary, is compared. `renode_bridge.py`'s own stdout (captured to
//! this test's own scratch directory, see [`RENODE_LOG_DISCLOSURE`] below) is the closest thing
//! to an internal log this test touches, and it is never asserted against, only left on disk for
//! a human to read if the comparison below ever fails.

use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::net::TcpStream;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use av_cdm::pb::{Binding, BindingKind, ContainerBinding, DesignReferenceMission, Event, Fault, Parameter, PortTrafficLog, SosConfiguration, SystemDefinition};
use av_kernel::drm::{execute, hash, schema, RunConfig, RunProducts};
use av_lockstep::docker::{lock_docker_tests, prune_stale_test_resources, test_label_args, test_run_id, DockerGateReason};
use prost::Message;
use gmat_sys::Gmat;

/// Not a real item -- just an anchor for the module doc comment's own cross-reference above.
#[allow(dead_code)]
const RENODE_LOG_DISCLOSURE: () = ();

// ------------------------------------------------------------------------------------------
// Fixture loading -- mirrors drm_attitude_control_cfs.rs's own helpers exactly (same truth/
// star tracker/IMU fixtures, same base SosConfiguration/DRM).
// ------------------------------------------------------------------------------------------

fn drms_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../drms").join(name)
}
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}
fn read(name: &str) -> String {
    std::fs::read_to_string(drms_path(name)).unwrap_or_else(|e| panic!("reading drms/{name}: {e}"))
}
fn load_system(stem: &str) -> SystemDefinition {
    let mut sys = schema::parse_system_definition_yaml(&read(&format!("{stem}.system.yaml"))).unwrap_or_else(|e| panic!("{stem}.system.yaml: {e}"));
    sys.hash = hash::canonical_system_hash(&sys);
    sys
}

fn load_native_fixtures_and_base_sos() -> (SystemDefinition, SystemDefinition, SystemDefinition, SosConfiguration) {
    let truth = load_system("demo_attitude_control_truth");
    let star = load_system("demo_attitude_control_startracker");
    let imu = load_system("demo_attitude_control_imu");
    let base_sos = schema::parse_sos_yaml(&read("demo_attitude_control.sos.yaml")).expect("native SosConfiguration parses");
    (truth, star, imu, base_sos)
}

/// Rebinds `base_sos`'s `"controller"` instance to `BINDING_KIND_CONTAINER` pointed at
/// `controller_system_id`, with `container_binding` (either a Docker `ContainerBinding{image,
/// image_digest}` for the posix path, or `ContainerBinding::default()` for the Renode
/// `container.address`-only path -- `binding::parse_container_spec`'s own mutual-exclusion rule).
fn container_sos(id: &str, base_sos: &SosConfiguration, controller_system_id: &str, container_binding: ContainerBinding) -> SosConfiguration {
    let mut sos = base_sos.clone();
    sos.id = id.to_string();
    sos.name = format!("{} (M24.4c: controller rebound to BINDING_KIND_CONTAINER)", base_sos.name);
    let mut found = false;
    for inst in sos.instances.iter_mut() {
        if inst.name == "controller" {
            found = true;
            inst.system_id = controller_system_id.to_string();
            inst.step_rate_hz = 10.0;
            inst.binding = Some(Binding { kind: BindingKind::Container as i32, config: Some(av_cdm::pb::binding::Config::Container(container_binding.clone())) });
        }
    }
    assert!(found, "demo_attitude_control.sos.yaml must declare a \"controller\" instance to rebind");
    sos.hash = hash::canonical_sos_hash(&sos);
    sos
}

/// A DRM over `duration_s` seconds at the fixture's own 10 Hz, no objectives/measures (neither
/// container path has an `output.controller.*` to reference) -- every assertion instead reads
/// `RunProducts.trajectories`/`.events` directly. Identical to
/// `drm_attitude_control_cfs.rs::container_drm` (copied, not imported: `tests/` binaries are not
/// a library this crate exposes).
fn container_drm(id: &str, sos_id: &str, duration_s: i64, faults: Vec<Fault>) -> DesignReferenceMission {
    let mut drm = schema::parse_drm_yaml(&read("demo_attitude_control.drm.yaml")).expect("native DRM parses");
    drm.id = id.to_string();
    drm.sos_configuration_id = sos_id.to_string();
    drm.objectives.clear();
    drm.measures.clear();
    {
        let scenario = drm.scenario.as_mut().expect("native DRM declares a scenario");
        scenario.end_tai_ns = scenario.start_tai_ns + duration_s * 1_000_000_000;
        scenario.faults = faults;
        scenario.seeds.insert("controller".to_string(), 42);
    }
    drm.hash = hash::canonical_drm_hash(&drm);
    drm
}

fn run_config<'a>(gmat: &'a Gmat, drm: &'a DesignReferenceMission, sos: &'a SosConfiguration, systems: &'a BTreeMap<String, SystemDefinition>, run_id: &str, products_dir: PathBuf) -> RunConfig<'a> {
    RunConfig { gmat, drm, sos, systems, run_id: run_id.to_string(), error_mode: Default::default(), products_dir: Some(products_dir), replay: None, command_source: None }
}

fn systems_map(sysvec: &[&SystemDefinition]) -> BTreeMap<String, SystemDefinition> {
    sysvec.iter().map(|s| (s.id.clone(), (*s).clone())).collect()
}

// ------------------------------------------------------------------------------------------
// Posix-container plumbing -- identical to drm_attitude_control_cfs.rs's own, PLUS question
// 156 housekeeping (prune_stale_test_resources/test_label_args/test_run_id) that file does not
// yet use (disclosed in third_party/renode/M24_4c_REPORT.md, not silently fixed there: this
// task owns crates/av-kernel/tests/drm_attitude_control_renode.rs, not that pre-existing file).
// ------------------------------------------------------------------------------------------

const CFS_LOCAL_IMAGE: &str = "altavista-cfs-lockstep:local";
const CFS_LOCAL_IMAGE_BUILD_HINT: &str = "run `docker build -f services/cfs/Dockerfile -t altavista-cfs-lockstep:local .` from the repository root once";

/// Question 194: typed, not a bare `String` -- see `crates/av-lockstep/src/docker.rs`'s
/// `DockerGateReason`/`image_gate_status`.
fn cfs_image_unavailable_reason() -> Option<av_lockstep::docker::DockerGateReason> {
    av_lockstep::docker::image_gate_status(CFS_LOCAL_IMAGE, CFS_LOCAL_IMAGE_BUILD_HINT).err()
}

fn docker_cmd(args: &[&str]) -> String {
    let output = Command::new("docker").args(args).current_dir(repo_root()).output().unwrap_or_else(|e| panic!("could not launch `docker {args:?}`: {e}"));
    if !output.status.success() {
        panic!("`docker {args:?}` failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

struct DockerContainerGuard(String);
impl Drop for DockerContainerGuard {
    fn drop(&mut self) {
        let _ = Command::new("docker").args(["rm", "-f", &self.0]).output();
    }
}
struct DockerImageGuard(String);
impl Drop for DockerImageGuard {
    fn drop(&mut self) {
        let _ = Command::new("docker").args(["rmi", "-f", &self.0]).output();
    }
}

/// Tags and pushes the already-built `altavista-cfs-lockstep:local` to a throwaway loopback-only
/// local registry, labeled per question 156 (`test_label_args`) so a killed test's own registry
/// container is swept by [`prune_stale_test_resources`] on the *next* run even if this run's own
/// `Drop` guards never get to fire. Returns `(image_ref, real_digest, guards)`.
///
/// Question 239: the registry is published on the FIXED loopback port [`FIXED_REGISTRY_PORT`]
/// (the same port and the same rules as `drm_attitude_control_cfs.rs`'s helper of the same name),
/// so `ContainerBinding.image` -- hashed with the whole `SosConfiguration` into
/// `sos_configuration_hash` (ADR-005 section 7) -- is the same string in every run. The
/// docker-test lock, held by the caller, serialises the test-owned registries on that port;
/// anything else holding it is reported, never worked around with another port.
fn push_cfs_image_to_local_registry(run_id: &str) -> (String, String, (DockerContainerGuard, DockerImageGuard)) {
    let labels = test_label_args(run_id);
    let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();

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
    // new one) -- the throwaway *registry container* above is this helper's own expensive/
    // stateful leaked resource (question 156's actual four-hour incident was a running
    // container, not a dangling tag), and that one is labeled.
    docker_cmd(&["tag", CFS_LOCAL_IMAGE, &tagged]);
    let image_guard = DockerImageGuard(tagged.clone());
    // The registry container has only just been started: retry the push briefly until the
    // registry process inside it accepts connections.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let out = Command::new("docker").args(["push", &tagged]).current_dir(repo_root()).output().unwrap_or_else(|e| panic!("could not launch `docker push {tagged}`: {e}"));
        if out.status.success() {
            break;
        }
        if Instant::now() >= deadline {
            panic!("`docker push {tagged}` did not succeed within 30 s: {}", String::from_utf8_lossy(&out.stderr));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    // The digest from the RepoDigests entry for THIS reference, not index 0 (the image may carry
    // RepoDigests from other registries).
    let repo_digests = docker_cmd(&["inspect", "--format={{range .RepoDigests}}{{println .}}{{end}}", &tagged]);
    let prefix = format!("{image_ref}@");
    let digest = repo_digests.lines().find_map(|l| l.strip_prefix(prefix.as_str())).filter(|d| d.starts_with("sha256:")).unwrap_or_else(|| panic!("a {prefix}sha256:... RepoDigests entry, got {repo_digests:?}")).to_string();
    (image_ref, digest, (registry_guard, image_guard))
}

/// The loopback port the throwaway registry is published on: the same value as
/// `drm_attitude_control_cfs.rs`'s `FIXED_REGISTRY_PORT` (the two files serialise on the
/// docker-test lock). Below every OS ephemeral range and away from 5000 (macOS AirPlay Receiver).
const FIXED_REGISTRY_PORT: u16 = 19031;

/// Why a test-owned registry could not be started on [`FIXED_REGISTRY_PORT`]. Never recovered by
/// choosing another port, which would change `ContainerBinding.image` and the
/// `sos_configuration_hash`.
#[derive(Debug)]
enum FixedRegistryError {
    /// Something still accepts connections on the port after the grace period.
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
/// test-owned registry removed a moment ago releases the port shortly after.
fn wait_for_fixed_registry_port_free() -> Result<(), FixedRegistryError> {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], FIXED_REGISTRY_PORT));
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_err() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(FixedRegistryError::PortOccupied { port: FIXED_REGISTRY_PORT });
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

// ------------------------------------------------------------------------------------------
// Renode plumbing -- av-lockstep-shim + renode_bridge.py as plain child processes (M13.2's
// container.address-only path; no Docker, no ContainerBinding.image, for this half of the
// comparison).
// ------------------------------------------------------------------------------------------

/// `AV_RENODE_BIN` (optional) overrides the Renode binary, so a worktree that does not carry the
/// gitignored portable build can still run this test; unset, the path is unchanged.
fn renode_bin() -> PathBuf {
    if let Some(p) = std::env::var_os("AV_RENODE_BIN") {
        return PathBuf::from(p);
    }
    repo_root().join("third_party/renode/renode-1.16.1-osx-arm64/Renode.app/Contents/MacOS/renode")
}
fn renode_platform() -> PathBuf {
    repo_root().join("third_party/renode/platforms/cpus/zynqmp.repl")
}
/// `AV_RENODE_CORE_CPU1_EXE` (optional) overrides the cross-built RTEMS ELF, so a rebuilt ELF kept
/// outside the main tree can be run without being copied into it; unset, the path is unchanged.
fn renode_elf() -> PathBuf {
    if let Some(p) = std::env::var_os("AV_RENODE_CORE_CPU1_EXE") {
        return PathBuf::from(p);
    }
    repo_root().join("third_party/cfs/build-rtems_zynqmp/exe/cpu1/core-cpu1.exe")
}
fn renode_bridge_script() -> PathBuf {
    repo_root().join("third_party/renode/M24_4b/renode_bridge.py")
}
fn venv_python() -> PathBuf {
    repo_root().join(".venv/bin/python3")
}

/// `None` iff every file this comparison's Renode half needs is present: M15.3's own convention
/// (see `cfs_image_unavailable_reason` above) -- a printed, visible skip reason, never a silent
/// `#[ignore]`. Existence-only (matching `cfs_image_unavailable_reason`'s own shallow depth, not
/// a deep functional probe) -- a missing/broken file inside one of these that still passes this
/// check surfaces instead as a real, loud failure when the run itself is attempted. Question
/// 194: typed (`DockerGateReason::RequiredFileMissing`), not a bare `String`.
fn renode_unavailable_reason() -> Option<av_lockstep::docker::DockerGateReason> {
    for (path, what) in [
        (renode_bin(), "the Renode binary (fetch-renode.sh, or set AV_RENODE_BIN to another path)"),
        (renode_platform(), "this repository's own zynqmp.repl platform file"),
        (renode_elf(), "the cross-built RTEMS core-cpu1.exe (third_party/rtems-container/build-cfs-cross.sh, or set AV_RENODE_CORE_CPU1_EXE to another path)"),
        (renode_bridge_script(), "third_party/renode/M24_4b/renode_bridge.py"),
        (venv_python(), "the repo-local .venv python3 (needed for renode_bridge.py's altavista.pb protobuf stubs)"),
    ] {
        if !path.is_file() {
            return Some(av_lockstep::docker::DockerGateReason::RequiredFileMissing { what: what.to_string(), path: path.display().to_string() });
        }
    }
    None
}

/// `env!("CARGO_BIN_EXE_av-lockstep-shim")` only works for a binary target of the *same*
/// package as the integration test (confirmed directly: adding a path dev-dependency on
/// `av-lockstep-shim` alone does not make Cargo set that variable for `av-kernel`'s own tests --
/// only building it does). Cargo places every compiled artifact for one build (test binaries
/// under `target/<profile>/deps/`, ordinary binaries directly under `target/<profile>/`) in the
/// same target directory, so this test's own executable path (`std::env::current_exe()`) names
/// exactly the `<profile>` this dev-dependency was built under -- walking up out of `deps/` (if
/// present) and back down to the `av-lockstep-shim` binary name finds the real, just-built
/// artifact without guessing a profile.
fn shim_bin_path() -> PathBuf {
    let mut dir = std::env::current_exe().expect("this test's own executable path").parent().expect("a parent directory").to_path_buf();
    if dir.ends_with("deps") {
        dir.pop();
    }
    let candidate = dir.join("av-lockstep-shim");
    assert!(candidate.is_file(), "expected the av-lockstep-shim binary at {} (built via this crate's own [dev-dependencies] entry) -- run `cargo build -p av-lockstep-shim` first if invoking this test binary directly", candidate.display());
    candidate
}

fn free_tcp_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port").local_addr().unwrap().port()
}

fn wait_until<F: FnMut() -> bool>(timeout: Duration, mut ready: F) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if ready() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

/// Kills and reaps a plain child (the shim -- it has no children of its own) on drop.
struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// `renode_bridge.py` owns its own Renode subprocess internally and only tears it down
/// gracefully (`machine Reset`-free `quit` over the monitor, then `proc.wait()`) inside its own
/// `main()`'s `finally:` block -- which a plain `SIGKILL` of the bridge's own Python process
/// (what `ChildGuard` does) never reaches, orphaning Renode. Spawned in its own process group
/// (`process_group(0)`), this guard signals the *whole group* on drop instead of just the one
/// child PID, so a panicking assertion mid-comparison cannot leave a Renode process running --
/// the same class of leak question 156's own amendment describes for Docker containers, applied
/// here to a native subprocess tree Docker is not involved in at all.
struct ProcessGroupGuard(Child);
impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        let pgid = self.0.id();
        let _ = Command::new("kill").args(["-TERM", &format!("-{pgid}")]).output();
        if !wait_exited_within(&mut self.0, Duration::from_secs(5)) {
            let _ = Command::new("kill").args(["-KILL", &format!("-{pgid}")]).output();
        }
        let _ = self.0.wait();
    }
}
/// A `Child` has no built-in timed wait; this is a short, bounded best-effort grace period after
/// `SIGTERM` before [`ProcessGroupGuard`]'s own `Drop` impl escalates to `SIGKILL` -- never
/// blocks longer than `timeout`, and a `try_wait` error is treated the same as "still running"
/// (fall through to the `SIGKILL` escalation, never a panic inside a `Drop` impl).
fn wait_exited_within(child: &mut Child, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(_) => return false,
        }
    }
    false
}

struct RenodeHandles {
    _shim: ChildGuard,
    _bridge: ProcessGroupGuard,
    grpc_addr: String,
}

/// Spawns `av-lockstep-shim` (the real compiled binary, built by Cargo for this test run) and
/// `renode_bridge.py` (owning the real Renode process and the real, root-cause-fixed
/// `core-cpu1.exe`), waits for the shim's `LockstepService` to actually be serving (which only
/// happens after the shim's Unix-socket accept + lockstep-local handshake with the bridge
/// completes -- i.e., after Renode has booted far enough for `IO_LOCKSTEP`'s `do_handshake()` to
/// answer the bridge's injected `HELLO`), and returns the ready address plus both processes'
/// cleanup guards. `scratch_dir` holds the Unix socket, the bridge's own Renode monitor log, and
/// (optionally) a UART0 transcript -- all left on disk, never asserted against (this test's own
/// "task order recorded, not asserted" rule).
fn spawn_renode_bridge(scratch_dir: &std::path::Path) -> RenodeHandles {
    std::fs::create_dir_all(scratch_dir).expect("create the Renode scratch dir");
    let socket_path = scratch_dir.join("lockstep.sock");
    let grpc_port = free_tcp_port();
    let grpc_addr = format!("127.0.0.1:{grpc_port}");

    let shim_bin = shim_bin_path();
    let shim_stdout = std::fs::File::create(scratch_dir.join("shim_stdout.log")).expect("create shim_stdout.log");
    let shim_stderr = std::fs::File::create(scratch_dir.join("shim_stderr.log")).expect("create shim_stderr.log");
    let shim = Command::new(&shim_bin)
        .arg("--socket-path")
        .arg(&socket_path)
        .arg("--grpc-addr")
        .arg(&grpc_addr)
        .stdout(Stdio::from(shim_stdout))
        .stderr(Stdio::from(shim_stderr))
        .spawn()
        .unwrap_or_else(|e| panic!("spawn av-lockstep-shim: {e}"));
    let mut shim_guard = ChildGuard(shim);
    if !wait_until(Duration::from_secs(10), || socket_path.exists()) {
        let status = shim_guard.0.try_wait().ok().flatten();
        panic!("av-lockstep-shim never created its Unix socket at {} (process status: {status:?})", socket_path.display());
    }

    let monitor_port = free_tcp_port();
    let uart_port = free_tcp_port();
    let renode_log = scratch_dir.join("renode_monitor.log");
    let uart0_log = scratch_dir.join("uart0.log");
    // M24.4f (third_party/renode/M24_4f_REPORT.md): an independent `CreateFileBackend` on uart1,
    // attached BESIDE the new `CharReceived` hook (which is now the only read source), captured
    // for a post-run byte-for-byte cross-check -- the same acceptance bar M24.4e's own
    // `CreateFileBackend` proof used, run here through the real production stack instead of a
    // synthetic driver. `renode_bridge.py`'s own `main()` prints "M24.4F_VERIFY PASS/FAIL" to its
    // stdout log (captured below) once the run completes.
    let uart1_raw_log = scratch_dir.join("uart1_raw_crosscheck.log");

    let mut bridge_cmd = Command::new(venv_python());
    bridge_cmd
        .arg(renode_bridge_script())
        .arg("--shim-socket")
        .arg(&socket_path)
        .arg("--elf")
        .arg(renode_elf())
        .arg("--platform")
        .arg(renode_platform())
        .arg("--renode-bin")
        .arg(renode_bin())
        .arg("--monitor-port")
        .arg(monitor_port.to_string())
        .arg("--uart-port")
        .arg(uart_port.to_string())
        .arg("--uart0-log")
        .arg(&uart0_log)
        .arg("--uart1-raw-log")
        .arg(&uart1_raw_log)
        .arg("--renode-log")
        .arg(&renode_log)
        // q171-d: stop granting a STEP virtual time once its reply is buffered (see the doc
        // comment at the top of this file for the A/B that shows the port traffic is unchanged);
        // `AV_BRIDGE_STEP_EARLY_STOP=0` in the environment restores the full 10 s grant.
        .env("AV_BRIDGE_STEP_EARLY_STOP", std::env::var("AV_BRIDGE_STEP_EARLY_STOP").unwrap_or_else(|_| "1".to_string()))
        .stdout(Stdio::from(std::fs::File::create(scratch_dir.join("bridge_stdout.log")).expect("create bridge_stdout.log")))
        .stderr(Stdio::from(std::fs::File::create(scratch_dir.join("bridge_stderr.log")).expect("create bridge_stderr.log")))
        .process_group(0); // its own group -- ProcessGroupGuard signals the group, Renode included
    let bridge = bridge_cmd.spawn().unwrap_or_else(|e| panic!("spawn renode_bridge.py: {e}"));
    let bridge_guard = ProcessGroupGuard(bridge);

    // materialize_container's container.address path never retries a failed connect/Bind (see
    // this file's own module doc comment) -- so wait here, not there, for the whole chain
    // (Renode process start -> platform+ELF load -> boot to the IO_LOCKSTEP HELLO-wait point ->
    // bridge's own HELLO relay) to be genuinely ready, via a real connect probe, not a guess.
    let ready = wait_until(Duration::from_secs(180), || match TcpStream::connect(&grpc_addr) {
        Ok(_) => true,
        Err(e) if e.kind() == ErrorKind::ConnectionRefused => false,
        Err(_) => false,
    });
    if !ready {
        panic!(
            "av-lockstep-shim's LockstepService never became ready on {grpc_addr} within 180s (renode_bridge.py may have failed -- see {}, {}, {})",
            renode_log.display(),
            scratch_dir.join("bridge_stdout.log").display(),
            scratch_dir.join("bridge_stderr.log").display()
        );
    }
    println!("Renode scratch dir (socket, monitor/uart0 logs, shim+bridge stdout/stderr): {}", scratch_dir.display());

    RenodeHandles { _shim: shim_guard, _bridge: bridge_guard, grpc_addr }
}

/// Loads `demo_attitude_control_controller_renode.system.yaml` and rewrites its
/// `container.address` placeholder to wherever this run's own shim actually bound its ephemeral
/// gRPC port -- exactly the YAML's own header comment's documented contract.
fn load_renode_controller_system(address: &str) -> SystemDefinition {
    let mut sys = load_system("demo_attitude_control_controller_renode");
    sys.parameters = sys
        .parameters
        .into_iter()
        .map(|p| if p.name == "container.address" { Parameter { string_value: address.to_string(), ..p } } else { p })
        .collect();
    sys.hash = hash::canonical_system_hash(&sys);
    sys
}

// ------------------------------------------------------------------------------------------
// Truth-based pointing error -- reads drms/demo_attitude_control_truth.system.yaml's own
// q_x/q_y/q_z/q_w state directly off RunProducts (see drm_attitude_control_cfs.rs's own module
// doc comment for why truth, not output.controller.pointing_error_rad, which the container path
// never populates regardless of binding kind).
// ------------------------------------------------------------------------------------------

const TRUTH_QX: usize = 0;
const TRUTH_QY: usize = 1;
const TRUTH_QZ: usize = 2;
// TRUTH_QW (index 3) is unused here: drm_attitude_control_cfs.rs's own acos-based cross-check
// against it is not repeated in this file -- this file's own comparison (two independently
// built binaries agreeing on the same quaternion, byte for byte) already subsumes it.

fn truth_pointing_error_rad(mean: &[f64]) -> f64 {
    let (qx, qy, qz) = (mean[TRUTH_QX], mean[TRUTH_QY], mean[TRUTH_QZ]);
    let vnorm = (qx * qx + qy * qy + qz * qz).sqrt();
    2.0 * vnorm.clamp(-1.0, 1.0).asin()
}
fn truth_pointing_error_rad_at_end(products: &RunProducts) -> f64 {
    let traj = products.trajectories.get("attitude").expect("the \"attitude\" truth instance produced a trajectory");
    let last = traj.samples.last().expect("at least one sample");
    truth_pointing_error_rad(&last.mean)
}

// ------------------------------------------------------------------------------------------
// Port-traffic sidecar and event comparison.
// ------------------------------------------------------------------------------------------

/// Lowercase-hex SHA-256 (`openssl`, already a dependency of this crate; `sha2` is banned
/// workspace-wide), the same function the executor hashes the sidecar with.
fn sha256_hex(bytes: &[u8]) -> String {
    openssl::sha::sha256(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

fn read_sidecar(dir: &std::path::Path) -> (Vec<u8>, PortTrafficLog) {
    let path = dir.join("port_traffic.pb");
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    let log = PortTrafficLog::decode(bytes.as_slice()).unwrap_or_else(|e| panic!("{} did not decode as a PortTrafficLog: {e}", path.display()));
    (bytes, log)
}

/// What differs between the two sidecars for reasons that are not port traffic. The whole-file
/// hashes cannot be equal across the two bindings: besides the records, the sidecar carries the
/// run's own `run_id` (and the same inside `provenance`), `provenance.config_hash` (the DRM hash;
/// the two DRMs differ in id and `sos_configuration_id`) and
/// `provenance.attributes["sos_configuration_hash"]` (the hash of a `SosConfiguration` whose
/// `"controller"` instance is bound to a Docker image in one run and to a `container.address` in
/// the other, so it differs by construction). Nothing else may differ.
const SIDECAR_PROVENANCE_EXCLUSIONS: &str = "PortTrafficLog.run_id, provenance.run_id, provenance.config_hash, provenance.attributes[\"sos_configuration_hash\"]";

fn assert_port_traffic_identical(posix: &RunProducts, posix_dir: &std::path::Path, renode: &RunProducts, renode_dir: &std::path::Path) {
    let (posix_bytes, posix_log) = read_sidecar(posix_dir);
    let (renode_bytes, renode_log) = read_sidecar(renode_dir);
    println!("port_traffic_hash: posix={} renode={}", posix.port_traffic_hash, renode.port_traffic_hash);
    println!("port_traffic.pb: posix {} bytes / {} records, renode {} bytes / {} records", posix_bytes.len(), posix_log.records.len(), renode_bytes.len(), renode_log.records.len());
    assert!(!posix.port_traffic_hash.is_empty() && !renode.port_traffic_hash.is_empty(), "both runs were given a products_dir, so both must report a non-empty port_traffic_hash");
    assert_eq!(posix.port_traffic_hash, sha256_hex(&posix_bytes), "the posix run's port_traffic_hash must be the SHA-256 of the port_traffic.pb it wrote");
    assert_eq!(renode.port_traffic_hash, sha256_hex(&renode_bytes), "the Renode run's port_traffic_hash must be the SHA-256 of the port_traffic.pb it wrote");
    assert!(!posix_log.records.is_empty(), "the posix run must have recorded port traffic");
    let mut per_port: BTreeMap<(String, String, i32), (usize, usize)> = BTreeMap::new();
    for rec in &posix_log.records {
        let e = per_port.entry((rec.instance.clone(), rec.port.clone(), rec.direction)).or_default();
        e.0 += 1;
        e.1 += rec.payload.len();
    }
    for ((instance, port, direction), (n, bytes)) in &per_port {
        println!("  port traffic {instance}.{port} dir={direction}: {n} records, {bytes} payload bytes");
    }

    // The traffic itself: every record (instance, port, direction, tai_ns, payload bytes, step
    // sequence), in order, byte for byte.
    assert_eq!(posix_log.records.len(), renode_log.records.len(), "port traffic record count");
    for (i, (p, r)) in posix_log.records.iter().zip(renode_log.records.iter()).enumerate() {
        assert_eq!(p, r, "port traffic record {i} differs (instance/port/direction/tai_ns/payload/sequence): posix {p:?} vs renode {r:?}");
    }
    // The same traffic as one hash: the sidecar re-encoded with only its records.
    let records_only = |log: &PortTrafficLog| sha256_hex(&PortTrafficLog { records: log.records.clone(), ..Default::default() }.encode_to_vec());
    let (posix_records_hash, renode_records_hash) = (records_only(&posix_log), records_only(&renode_log));
    println!("port traffic records-only hash: posix={posix_records_hash} renode={renode_records_hash}");
    assert_eq!(posix_records_hash, renode_records_hash, "the records-only hash of the port traffic must be identical");

    // The sidecar's remaining fields, with each excluded field named (see the constant).
    let strip = |log: &PortTrafficLog| {
        let mut log = log.clone();
        log.run_id.clear();
        let prov = log.provenance.as_mut().expect("the sidecar carries the run's provenance");
        prov.run_id.clear();
        prov.config_hash.clear();
        prov.attributes.remove("sos_configuration_hash");
        log
    };
    assert_eq!(strip(&posix_log), strip(&renode_log), "the sidecars must be identical except {SIDECAR_PROVENANCE_EXCLUSIONS}");
    if posix.port_traffic_hash == renode.port_traffic_hash {
        println!("port_traffic_hash is equal as it stands");
    } else {
        println!("port_traffic_hash differs only through {SIDECAR_PROVENANCE_EXCLUSIONS}; the records are byte-identical");
    }
}

/// The `products.events` fields that are provenance of the run, not part of what happened, found
/// by decoding both runs' events (q171-d): every event's `provenance.config_hash` (the DRM hash;
/// the two DRMs differ in id and `sos_configuration_id`) and `provenance.run_id`; and, on the
/// `"controller"` instance's own `run_start`/`run_end` events only, `provenance.attributes[
/// "system_definition_hash"]` and `["system_definition_id"]` (the controller is a different
/// `SystemDefinition` per binding: `..._controller_cfs` against `..._controller_renode`). Every
/// other field of every event is compared exactly (the `run_start` `detail` is identical: it does
/// not carry the container address).
const EVENT_PROVENANCE_EXCLUSIONS: &str = "provenance.config_hash, provenance.run_id, and the controller instance's provenance.attributes[\"system_definition_hash\"/\"system_definition_id\"]";

/// Field-by-field differences between two event lists, outside [`EVENT_PROVENANCE_EXCLUSIONS`];
/// empty means identical. Names the event, the field and both values, so a failure is a diagnosis.
fn event_differences(posix: &[Event], renode: &[Event]) -> Vec<String> {
    let mut out = Vec::new();
    if posix.len() != renode.len() {
        out.push(format!("event count: posix {} vs renode {}", posix.len(), renode.len()));
    }
    for (i, (p, r)) in posix.iter().zip(renode.iter()).enumerate() {
        let mut field = |name: &str, a: String, b: String| {
            if a != b {
                out.push(format!("event[{i}] {:?}: {name}: posix={a} renode={b}", p.id));
            }
        };
        field("id", format!("{:?}", p.id), format!("{:?}", r.id));
        field("entity_id", format!("{:?}", p.entity_id), format!("{:?}", r.entity_id));
        field("tai_ns", p.tai_ns.to_string(), r.tai_ns.to_string());
        field("kind", format!("{:?}", p.kind), format!("{:?}", r.kind));
        field("name", format!("{:?}", p.name), format!("{:?}", r.name));
        field("detail", format!("{:?}", p.detail), format!("{:?}", r.detail));
        field("values", format!("{:?}", p.values), format!("{:?}", r.values));
        field("frame_id", format!("{:?}", p.frame_id), format!("{:?}", r.frame_id));
        field("reference_id", format!("{:?}", p.reference_id), format!("{:?}", r.reference_id));
        field("label", format!("{:?}", p.label), format!("{:?}", r.label));
        match (&p.provenance, &r.provenance) {
            (Some(pp), Some(rp)) => {
                field("provenance.author_kind", format!("{:?}", pp.author_kind), format!("{:?}", rp.author_kind));
                field("provenance.principal", format!("{:?}", pp.principal), format!("{:?}", rp.principal));
                field("provenance.tool", format!("{:?}", pp.tool), format!("{:?}", rp.tool));
                field("provenance.data_pack_hash", format!("{:?}", pp.data_pack_hash), format!("{:?}", rp.data_pack_hash));
                field("provenance.dataset_hash", format!("{:?}", pp.dataset_hash), format!("{:?}", rp.dataset_hash));
                field("provenance.created_tai_ns", pp.created_tai_ns.to_string(), rp.created_tai_ns.to_string());
                let attrs = |a: &std::collections::BTreeMap<String, String>| {
                    let mut a = a.clone();
                    if p.entity_id == "controller" {
                        a.remove("system_definition_hash");
                        a.remove("system_definition_id");
                    }
                    format!("{a:?}")
                };
                let (pa, ra) = (attrs(&pp.attributes), attrs(&rp.attributes));
                field("provenance.attributes", pa, ra);
            }
            (a, b) => field("provenance presence", format!("{}", a.is_some()), format!("{}", b.is_some())),
        }
    }
    out
}

// ------------------------------------------------------------------------------------------
// The comparison itself.
// ------------------------------------------------------------------------------------------

/// 100 steps at the fixture's 10 Hz: the same arc as the container determinism test
/// (`drm_attitude_control_cfs.rs`'s `DETERMINISM_DURATION_S`), which is the arc question 145's
/// identical-traffic criterion is stated over.
const COMPARISON_DURATION_S: i64 = 10;

/// The opt-in variable: a run takes tens of minutes of wall time (see the test's doc comment).
const RENODE_OPT_IN_ENV: &str = "AV_RENODE_TESTS";
/// Measured wall time of the whole test (see the test's doc comment).
const RENODE_WALL_TIME_HINT: &str = "5 to 7 minutes (measured 305 s and 409 s; 36 minutes with AV_BRIDGE_STEP_EARLY_STOP=0)";
/// Set to `1` to leave the scratch directory (sidecars, bridge frame log, monitor log) on disk after
/// a passing run, so its evidence can be inspected or copied.
const RENODE_KEEP_SCRATCH_ENV: &str = "AV_RENODE_KEEP_SCRATCH";

/// `None` iff the long Renode run was explicitly asked for. Typed
/// (`DockerGateReason::PrerequisiteUnavailable`, the variant for "a non-Docker prerequisite is
/// unavailable"), so the skip is announced the same way as the image and file reasons.
fn renode_opt_in_unavailable_reason() -> Option<DockerGateReason> {
    if std::env::var(RENODE_OPT_IN_ENV).as_deref() == Ok("1") {
        return None;
    }
    Some(DockerGateReason::PrerequisiteUnavailable {
        what: format!("the opt-in {RENODE_OPT_IN_ENV}=1 (not set)"),
        hint: format!("set {RENODE_OPT_IN_ENV}=1 to run it; a {COMPARISON_DURATION_S} s arc ({} steps) takes {RENODE_WALL_TIME_HINT} of wall time", COMPARISON_DURATION_S * 10),
    })
}

#[test]
fn byte_identical_port_traffic_between_posix_container_and_renode() {
    let mut reasons = Vec::new();
    if let Some(r) = cfs_image_unavailable_reason() {
        reasons.push(r);
    }
    if let Some(r) = renode_unavailable_reason() {
        reasons.push(r);
    }
    if let Some(r) = renode_opt_in_unavailable_reason() {
        reasons.push(r);
    }
    if !reasons.is_empty() {
        // Question 194: typed reasons, a real visible skip (never println!/eprintln!, which
        // cargo test's default runner captures and never prints for a passing test -- measured
        // in crates/av-lockstep/R6_3_REPORT.md section 1), and the test asserts on what the
        // helper actually announced -- not merely a return with nothing asserted.
        let line = av_lockstep::docker::announce_gate_skip_multi("byte_identical_port_traffic_between_posix_container_and_renode", &reasons);
        assert!(
            line.starts_with("SKIPPED ") && line.contains("byte_identical_port_traffic_between_posix_container_and_renode"),
            "the gate helper must announce a visible skip line naming this test: {line:?}"
        );
        return;
    }
    run_byte_identical_port_traffic_between_posix_container_and_renode();
}

fn run_byte_identical_port_traffic_between_posix_container_and_renode() {
    let _engine = gmat_sys::engine_lock();
    let (truth, star, imu, base_sos) = load_native_fixtures_and_base_sos();

    // --- Posix-container half (ContainerBinding.image path, digest-pulled). ---
    let scratch_dir = PathBuf::from(format!("/tmp/av-renode-m24c-{}", std::process::id()));
    let posix_products_dir = scratch_dir.join("products_posix");
    let renode_products_dir = scratch_dir.join("products_renode");

    // Question 207: the host-wide docker-test lock guards this half, and only this half. A
    // different worktree's own docker-gated `cargo test`/`pytest` process racing this daemon-wide
    // prune sweep is exactly what round 3's own gate measured failing elsewhere in this workspace
    // (crates/av-lockstep/tests/docker_lifecycle.rs's own doc comment has the full account).
    // `prune_stale_test_resources` requires proof (a `&DockerTestLock` parameter) that the caller
    // already holds this lock. The Renode half below uses no Docker at all (the shim and
    // `renode_bridge.py` are plain child processes), so everything Docker -- the lock, the
    // prune, the throwaway registry and its image tag, the posix run's managed container -- is
    // created and dropped inside this block, and nothing but plain value (the posix
    // `RunProducts`, an owned struct) leaves it. The lock is therefore free for the 5-7 minutes
    // of the Renode half (it used to be held across them).
    let products_posix = {
        let lock = lock_docker_tests();
        let lock_taken = Instant::now();

        // Question 156's amendment: sweep whatever a previous, interrupted run left behind (its
        // own `Drop` guards never ran if that run was killed) before this test creates anything.
        prune_stale_test_resources(&lock);
        let run_id = test_run_id();

        let t0 = Instant::now();
        let (image, digest, registry_guards) = push_cfs_image_to_local_registry(&run_id);
        let controller_cfs = load_system("demo_attitude_control_controller_cfs");
        let posix_systems = systems_map(&[&truth, &star, &imu, &controller_cfs]);
        let posix_sos = container_sos("attitude_control_m24c_posix_sos", &base_sos, &controller_cfs.id, ContainerBinding { image: image.clone(), image_digest: digest.clone(), ..Default::default() });
        let posix_drm = container_drm("attitude_control_m24c_posix_drm", &posix_sos.id, COMPARISON_DURATION_S, vec![]);
        let gmat_posix = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
        // The posix run's managed container is stopped when the run's models drop, i.e. by the
        // time this call returns -- still inside the lock.
        let products = execute(run_config(&gmat_posix, &posix_drm, &posix_sos, &posix_systems, "test-run-m24c-posix", posix_products_dir.clone())).expect("the posix-container run must execute end to end");
        let elapsed = t0.elapsed();
        println!("posix-container run: {COMPARISON_DURATION_S}s @ 10Hz in {elapsed:.2?} wall time");

        // Explicit drop order: the throwaway registry container and its image tag first (their
        // `Drop`s run `docker rm -f` / `docker rmi -f`), THEN the lock, so no Docker resource of
        // this test outlives the lock that protects it from other trees' daemon-wide prunes.
        drop(registry_guards);
        drop(lock);
        println!("docker-test lock released after {:.2?} (registry guards dropped first); no Docker resource of this test remains", lock_taken.elapsed());
        products
    };

    // --- Renode half (container.address-only, already-running-process path). ---
    // No Docker, and no docker-test lock: both were released at the end of the block above.
    println!("Renode half starting (docker-test lock not held)");
    let t1 = Instant::now();
    let renode = spawn_renode_bridge(&scratch_dir);
    let renode_ready_elapsed = t1.elapsed();
    println!("Renode bridge ready (boot + HELLO/BIND handshake path primed) in {renode_ready_elapsed:.2?} wall time");

    let controller_renode = load_renode_controller_system(&renode.grpc_addr);
    let renode_systems = systems_map(&[&truth, &star, &imu, &controller_renode]);
    let renode_sos = container_sos("attitude_control_m24c_renode_sos", &base_sos, &controller_renode.id, ContainerBinding::default());
    let renode_drm = container_drm("attitude_control_m24c_renode_drm", &renode_sos.id, COMPARISON_DURATION_S, vec![]);
    let gmat_renode = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let t2 = Instant::now();
    let products_renode = execute(run_config(&gmat_renode, &renode_drm, &renode_sos, &renode_systems, "test-run-m24c-renode", renode_products_dir.clone())).expect("the Renode-bound run must execute end to end through the real RTEMS/cFE/IO_LOCKSTEP guest");
    let renode_run_elapsed = t2.elapsed();
    println!("Renode-bound run: {COMPARISON_DURATION_S}s @ 10Hz in {renode_run_elapsed:.2?} wall time ({} steps)", COMPARISON_DURATION_S * 10);

    let posix_theta = truth_pointing_error_rad_at_end(&products_posix);
    let renode_theta = truth_pointing_error_rad_at_end(&products_renode);
    println!(
        "truth-based pointing error at t={COMPARISON_DURATION_S}s: posix={posix_theta:.12e} rad, renode={renode_theta:.12e} rad, |diff|={:.3e} rad",
        (posix_theta - renode_theta).abs()
    );

    // ---------------------------------------------------------------------------------------
    // The identical-traffic criterion (question 145), first and strictest: the executor's own
    // port-traffic sidecar of each run (`port_traffic.pb`, hashed into `port_traffic_hash`).
    // ---------------------------------------------------------------------------------------
    assert_port_traffic_identical(&products_posix, &posix_products_dir, &products_renode, &renode_products_dir);

    // ---------------------------------------------------------------------------------------
    // The exit criterion: per-step byte comparison of port traffic. Recorded (never asserted):
    // any FSW-internal task-scheduling order -- this reads RunProducts only. See this file's
    // module doc comment for exactly which fields are excluded, and why.
    // ---------------------------------------------------------------------------------------
    let mut posix_names: Vec<_> = products_posix.trajectories.keys().collect();
    let mut renode_names: Vec<_> = products_renode.trajectories.keys().collect();
    posix_names.sort();
    renode_names.sort();
    assert_eq!(posix_names, renode_names, "the same entities must be present in both the posix-container and the Renode run");

    for name in posix_names {
        let traj_posix = &products_posix.trajectories[name];
        let traj_renode = &products_renode.trajectories[name];
        assert_eq!(traj_posix.samples, traj_renode.samples, "instance {name:?}: samples must be byte-identical between the posix-container and the Renode run (same DRM/seed, same adcs_control.c source)");
        assert_eq!(traj_posix.event_ids, traj_renode.event_ids, "instance {name:?}: event_ids (the ordered port-traffic record, question 145) must be byte-identical");
        assert_eq!(traj_posix.state_space_id, traj_renode.state_space_id, "instance {name:?}: state_space_id");
        assert_eq!(traj_posix.segments.len(), traj_renode.segments.len(), "instance {name:?}: segment count must match");
        for (seg_posix, seg_renode) in traj_posix.segments.iter().zip(traj_renode.segments.iter()) {
            assert_eq!(seg_posix.start_tai_ns, seg_renode.start_tai_ns, "instance {name:?}: segment start_tai_ns");
            assert_eq!(seg_posix.end_tai_ns, seg_renode.end_tai_ns, "instance {name:?}: segment end_tai_ns");
            assert_eq!(seg_posix.dynamics_model, seg_renode.dynamics_model, "instance {name:?}: segment dynamics_model");
            if name != "controller" {
                // Native (GMAT) instances: same declared settings both runs, unaffected by how
                // "controller" happens to be bound -- dynamics_hash SHOULD genuinely match.
                assert_eq!(seg_posix.dynamics_hash, seg_renode.dynamics_hash, "instance {name:?}: segment dynamics_hash (a native GMAT model's settings hash must be identical regardless of the controller's own binding kind)");
            }
            // "controller"'s own dynamics_hash deliberately not compared -- see this file's own
            // module doc comment (folds in container.address, inherently different).
        }
    }
    let event_diffs = event_differences(&products_posix.events, &products_renode.events);
    println!("products.events: {} posix, {} renode; differences outside {EVENT_PROVENANCE_EXCLUSIONS}: {}", products_posix.events.len(), products_renode.events.len(), event_diffs.len());
    assert!(event_diffs.is_empty(), "products.events must be byte-identical except {EVENT_PROVENANCE_EXCLUSIONS}; first differences:\n{}", event_diffs.iter().take(20).cloned().collect::<Vec<_>>().join("\n"));
    // `event_differences` names fields for the diagnosis; this is the completeness check, over
    // whole `Event` values with only the named fields blanked, so a field added to `Event` later
    // is compared without anyone having to list it.
    let strip_events = |events: &[Event]| -> Vec<Event> {
        events
            .iter()
            .map(|e| {
                let mut e = e.clone();
                let controller = e.entity_id == "controller";
                if let Some(p) = e.provenance.as_mut() {
                    p.config_hash.clear();
                    p.run_id.clear();
                    if controller {
                        p.attributes.remove("system_definition_hash");
                        p.attributes.remove("system_definition_id");
                    }
                }
                e
            })
            .collect()
    };
    assert_eq!(strip_events(&products_posix.events), strip_events(&products_renode.events), "products.events must be identical as whole values except {EVENT_PROVENANCE_EXCLUSIONS}");
    assert_eq!(products_posix.scores, products_renode.scores, "products.scores must be byte-identical (both empty -- no measures declared)");
    assert_eq!(products_posix.dropped_in_flight_messages, products_renode.dropped_in_flight_messages, "dropped_in_flight_messages must match (both runs end cleanly at the same scenario length)");
    assert_eq!(products_posix.frames, products_renode.frames, "frames (registry defaults + declared Scenario.frames) must be byte-identical -- neither depends on the controller's own binding kind");

    println!("PASS: RunProducts port traffic is byte-identical between the posix-container and Renode-bound \"controller\" over {COMPARISON_DURATION_S}s @ 10Hz ({} steps)", COMPARISON_DURATION_S * 10);

    // M24.4d housekeeping: every prior assertion above passed, so `scratch_dir` (the Unix
    // socket, the bridge's own monitor/uart0 logs, and the shim+bridge stdout/stderr) has done
    // its one job -- helping a human diagnose a FAILURE -- and is no longer needed. Removed only
    // here, after every assertion, so a failing run (an early `panic!`/`assert_eq!` unwind, which
    // never reaches this line) still leaves its own diagnostics on disk exactly as before; this
    // is what actually fixes the three pre-existing `/tmp/av-renode-m24c-*` directories'-worth of
    // leftover scratch dirs `M24_4d_REPORT.md` found and removed by hand, rather than merely
    // disclosing the leak once more. Best-effort: a removal failure here must never turn an
    // otherwise-passing test red.
    if std::env::var(RENODE_KEEP_SCRATCH_ENV).as_deref() == Ok("1") {
        println!("{RENODE_KEEP_SCRATCH_ENV}=1: scratch dir kept at {}", scratch_dir.display());
    } else {
        let _ = std::fs::remove_dir_all(&scratch_dir);
    }
}
