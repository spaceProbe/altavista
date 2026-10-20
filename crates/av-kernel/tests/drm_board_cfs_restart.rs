//! Question 245 (6), phase B: a board-bound run whose board **really reboots mid-run**, against the
//! real cFS posix container behind the real `av-edge-board`, restarted for real.
//!
//! **A `docker restart` of the container stands in for a board reboot. No board is involved.**
//!
//! # The setup, and why the edge service runs in the Colima VM
//!
//! `drms/demo_attitude_control` with `"controller"` bound as `BINDING_KIND_BOARD` (the board
//! variant of the controller system, the same one `drm_board_*.rs` use) to `altavista-cfs-lockstep`
//! (its `io_lockstep` app unchanged) started with `LOCKSTEP_TRANSPORT=udp`: the relay in the
//! container carries the guest's lockstep-local stream over UDP to `av-edge-board`. **On this host
//! the edge service runs inside the Colima VM**, because this Colima's `portForwarder: ssh` carries
//! TCP only (`~/.colima/default/colima.yaml`; the Lima host agent logs `Forwarding TCP from ...` for
//! each published TCP port and nothing for UDP), so a datagram from the macOS host never reaches a
//! container's published UDP port (measured: `crates/av-edge-board/README.md`, "Docker on macOS").
//! The VM stands in for the edge node the board hangs off. The kernel (this test) stays on the host and
//! dials the service's gRPC over TCP, which Lima forwards: `board.edge_address` is the host-forwarded
//! `127.0.0.1:<grpc port>`, and the VM-side service has `--port-device udp://127.0.0.1:<published udp
//! port>`. The Linux build comes from `services/cfs/build-edge-board.sh` (`target-docker-linux/
//! edge-board/av-edge-board`, or `AV_EDGE_BOARD_LINUX`); the service's I/O log, identity, power
//! fixture and stderr live under `$HOME/av-kernel-cfs-restart-*-stage-<pid>/` (the VM mounts `$HOME`
//! at the same path). The service's power channel is the VM's own `docker restart` (or, in the
//! never-returns scenario, `docker stop`).
//!
//! # What is declared before the run, and measured after
//!
//! The scenario: 8 s of the attitude-control DRM at 10 Hz (80 STEPs), a `HARDWARE`/`power_cycle`
//! fault on `controller` at 3 s (after STEP 30). **Expected, stated before the first run:** the signed
//! log holds `BIND, 30 x STEP, POWER_CYCLE, LINK_REHANDSHAKE, BIND, RESET, 50 x STEP, SHUTDOWN` = 86
//! records; the lockstep sequence is 1..=30 for the STEPs, 31 for the RESET and 32..=81 for the rest;
//! the run takes about 8 s of paced wall time plus the restart (about 2 to 3 s, during which the paced
//! schedule falls behind and then catches up), so roughly 11 to 14 s, and the whole test (container,
//! service, run, replay) well under two minutes. **The controller's outputs resume within
//! [`DECLARED_RESUME_STEPS`] = 3 steps after the RESET** (the first `wheel_torque_out` after the
//! fault's epoch is at most 3 steps later), and nearly every later step has one (at least 50 - 3 of
//! the 50). That number is a declaration made before measuring: a fresh cFE boot has the guest's
//! one-time "warm-up" behaviour (`IO_LOCKSTEP: wheel_torque_out produced no output within 2000 ms`), so
//! it is not assumed equal to the 2 steps `drm_attitude_control_cfs.rs` allows after a RESET of a
//! running guest.
//!
//! # The scenarios (each takes the docker lock itself and must never be wrapped)
//!
//! 1. **restart and replay**: the run completes; the controller resumes; the gap is in the events and
//!    the signed log (decoded and listed); the run replays from the signed log and equals the live run
//!    after the named exclusions.
//! 2. **never returns**: the power channel stops the container instead of restarting it. The edge
//!    service gives up at its declared deadline and the run fails with the typed
//!    `DrmError::BoardPowerCycle(RehandshakeFailed)` naming the instance, the fault and the deadline,
//!    within a bounded wall time, leaving nothing running.
//! 3. **the service does not re-handshake**: the DRM declares the reboot but the service was not
//!    started with `--power-cycle-reboots-peer`: a typed `RehandshakeExpected` at once, before any RESET
//!    is sent to a guest that is waiting for a HELLO. And with nothing declared on either side the
//!    RESET reaches a guest that is waiting for a HELLO and the run fails by `board.step_timeout_ms`.
mod drm_board_common;

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use av_cdm::pb::{self, Fault, FaultTargetKind, PortDirection, PortTrafficLog};
use av_edge::board_log::{BoardIoKind, LogVerifier, VerifiedLog};
use av_kernel::drm::board_replay::{strip_replay_exclusions, ReplaySource};
use av_kernel::drm::power::PowerControlError;
use av_kernel::drm::{execute_with_board_replay, BoardLogPin, BoardLogReplay, DrmError, RunConfig, RunProducts};
use av_lockstep::docker::{announce_gate_skip, docker_daemon_status, local_image_id, lock_docker_tests, prune_stale_test_resources, test_label_args, test_run_id, DockerGateReason, DockerTestLock};
use drm_board_common::*;
use prost::Message;

const EDGE_NODE: &str = "edge-cfs-kernel";
const RUN_ID: &str = "test-drm-board-cfs-restart";
const DEFAULT_IMAGE: &str = "altavista-cfs-lockstep:local";
const IMAGE_ENV: &str = "AV_CFS_TEST_IMAGE";
const LINUX_BINARY_ENV: &str = "AV_EDGE_BOARD_LINUX";
const RELAY_UDP_PORT: u16 = 50090;
const VM_DOCKER: &str = "/usr/bin/docker";
const VM_SERVICE_CAP_SECS: u32 = 1500;
const DURATION_S: i64 = 8;
const FAULT_S: i64 = 3;
const STEP_NS: i64 = 100_000_000;
/// Declared before the first run (module doc): the controller's output resumes within this many steps of
/// the fault's epoch, and all but this many of the later steps have one.
const DECLARED_RESUME_STEPS: i64 = 3;

// ------------------------------------------------------------------------------------------
// Docker, Colima and the VM-side service
// ------------------------------------------------------------------------------------------

fn docker(args: &[&str]) -> Result<String, String> {
    let out = Command::new("docker").args(args).output().map_err(|e| format!("could not launch docker: {e}"))?;
    if !out.status.success() {
        return Err(format!("docker {args:?} failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Removes the container at drop.
struct Container {
    name: String,
}

impl Container {
    fn logs(&self) -> String {
        let out = Command::new("docker").args(["logs", &self.name]).output().expect("docker logs");
        format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
    }
}

impl Drop for Container {
    fn drop(&mut self) {
        let _ = Command::new("docker").args(["rm", "-f", "-v", &self.name]).output();
    }
}

fn colima(args: &[&str]) -> (bool, String) {
    match Command::new("colima").arg("ssh").arg("--").args(args).output() {
        Ok(o) => (o.status.success(), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)).trim().to_string()),
        Err(e) => (false, format!("could not launch colima: {e}")),
    }
}

fn linux_binary() -> PathBuf {
    std::env::var(LINUX_BINARY_ENV).ok().filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| repo_root().canonicalize().unwrap().join("target-docker-linux/edge-board/av-edge-board"))
}

/// Why the in-VM service cannot be used here, if it cannot.
fn vm_path_unavailable() -> Option<String> {
    let bin = linux_binary();
    if !bin.is_file() {
        return Some(format!("the Linux av-edge-board is missing at {} (build it with services/cfs/build-edge-board.sh, or set {LINUX_BINARY_ENV})", bin.display()));
    }
    let (ok, out) = colima(&[bin.to_str().unwrap(), "--help"]);
    if !out.contains("usage: av-edge-board") {
        return Some(format!("the Colima VM could not run {} ({}): {out}", bin.display(), if ok { "ran, but printed no usage" } else { "colima ssh failed" }));
    }
    None
}

fn image_has_relay(image: &str) -> bool {
    Command::new("docker").args(["run", "--rm", "--entrypoint", "test"]).args(test_label_args(&test_run_id())).args([image, "-x", "/cfs/av-lockstep-relay"]).output().map(|o| o.status.success()).unwrap_or(false)
}

fn write_script(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, format!("#!/bin/sh\n{body}")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// The VM-side edge service: the `colima ssh` client process, its control directory (`launch.sh`, the
/// pid file, `stop.sh`) and its stage directory. Dropping it stops the VM-side process (checked
/// separately by the tests with `pgrep`), then removes both directories.
struct VmService {
    ssh: Child,
    ctl: PathBuf,
    stage: PathBuf,
}

impl VmService {
    fn stderr(&self) -> String {
        std::fs::read_to_string(self.stage.join("service.stderr")).unwrap_or_default()
    }
    fn wait_exit(&mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.ssh.try_wait().unwrap().is_some() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    }
}

impl Drop for VmService {
    fn drop(&mut self) {
        let stop = self.ctl.join("stop.sh");
        if stop.exists() {
            let _ = colima(&[stop.to_str().unwrap()]);
        }
        let _ = self.ssh.kill();
        let _ = self.ssh.wait();
        let _ = std::fs::remove_dir_all(&self.ctl);
        let _ = std::fs::remove_dir_all(&self.stage);
    }
}

/// Is a VM-side process whose command line contains `unique_arg` running? (The `[x]` trick keeps
/// `pgrep -f` from matching the shell that runs it.)
fn vm_process_running(unique_arg: &str) -> bool {
    let pattern = format!("[{}]{}", &unique_arg[..1], &unique_arg[1..]);
    colima(&["pgrep", "-f", &pattern]).0
}

// ------------------------------------------------------------------------------------------
// One scenario's rig
// ------------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Channel {
    /// `docker restart -t 2 <container>`: the stand-in for a board reboot.
    Restart,
    /// `docker stop -t 1 <container>`: the peer never comes back.
    Stop,
}

struct Scenario {
    tag: &'static str,
    channel: Channel,
    /// `av-edge-board --power-cycle-reboots-peer` and its interval and deadline, ms.
    service_rehandshake: Option<(u64, u64)>,
    /// `board.rehandshake_timeout_ms` in the DRM, if declared.
    declare_ms: Option<u64>,
    /// `board.step_timeout_ms`.
    step_timeout_ms: u64,
    port_offset: u16,
}

struct Rig {
    // Field order is the drop order: the VM-side service, then the container, then the docker lock.
    vm: VmService,
    container: Container,
    lock: DockerTestLock,
    run_id: String,
    io_log: PathBuf,
    cert_pem: PathBuf,
    grpc_addr: String,
    scene: Scene,
}

impl Rig {
    /// `None` after a visible SKIPPED when this host cannot run the scenario.
    fn start(test_name: &str, s: &Scenario) -> Option<Rig> {
        let lock = lock_docker_tests();
        if let Err(reason) = docker_daemon_status() {
            announce_gate_skip(test_name, &reason);
            return None;
        }
        let image = std::env::var(IMAGE_ENV).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| DEFAULT_IMAGE.to_string());
        if local_image_id(&image).is_err() {
            announce_gate_skip(test_name, &DockerGateReason::ImageNotBuilt { image_ref: image, build_hint: "build it with services/cfs/build-shim.sh then `docker build -f services/cfs/Dockerfile -t <tag> .`, and point AV_CFS_TEST_IMAGE at it".to_string() });
            return None;
        }
        if !image_has_relay(&image) {
            announce_gate_skip(test_name, &DockerGateReason::PrerequisiteUnavailable { what: format!("/cfs/av-lockstep-relay in image {image}"), hint: "rebuild the image with services/cfs/build-shim.sh and `docker build` (question 245 (6) added the relay), or set AV_CFS_TEST_IMAGE to a run-scoped tag that has it".to_string() });
            return None;
        }
        if let Some(why) = vm_path_unavailable() {
            announce_gate_skip(
                test_name,
                &DockerGateReason::PrerequisiteUnavailable {
                    what: "the edge service inside the Colima VM (this test's path to the container's published UDP port)".to_string(),
                    hint: format!("Colima's `portForwarder: ssh` forwards TCP only, so the macOS host cannot reach a published UDP port and the service runs in the VM: {why}"),
                },
            );
            return None;
        }
        prune_stale_test_resources(&lock);

        let run_id = test_run_id();
        let labels = test_label_args(&run_id);
        let pid = std::process::id();
        let udp_port: u16 = 20_000 + (pid % 9_000) as u16 + s.port_offset;
        let name = format!("av-kernel-cfs-restart-{}-{pid}", s.tag);
        let publish = format!("127.0.0.1:{udp_port}:{RELAY_UDP_PORT}/udp");
        let mut args: Vec<&str> = vec!["run", "-d", "--name", &name, "-p", &publish, "--sysctl", "fs.mqueue.msg_max=256", "--sysctl", "fs.mqueue.msgsize_max=65536", "-e", "LOCKSTEP_TRANSPORT=udp"];
        args.extend(labels.iter().map(String::as_str));
        args.push(&image);
        docker(&args).unwrap_or_else(|e| panic!("starting the cFS container: {e}"));
        let container = Container { name: name.clone() };
        println!("[{}] container {name} from {image} ({:?}), UDP published at 127.0.0.1:{udp_port}", s.tag, local_image_id(&image));
        let t0 = Instant::now();
        while !container.logs().contains("guest connected (session 1)") {
            assert!(t0.elapsed() < Duration::from_secs(60), "the guest never connected to the relay:\n{}", container.logs());
            std::thread::sleep(Duration::from_millis(100));
        }

        // The stage and control directories (under $HOME, which the VM mounts at the same path).
        let home = PathBuf::from(std::env::var("HOME").expect("HOME"));
        let stage = home.join(format!("av-kernel-cfs-restart-{}-stage-{pid}", s.tag));
        let ctl = home.join(format!("av-kernel-cfs-restart-{}-ctl-{pid}", s.tag));
        for d in [&stage, &ctl] {
            let _ = std::fs::remove_dir_all(d);
            std::fs::create_dir_all(d).unwrap();
        }
        let (key_pem, cert_pem) = write_identity(&stage);
        let io_log = stage.join("io.log");
        let channel = stage.join("power-cycle");
        let verb = match s.channel {
            Channel::Restart => "restart -t 2",
            Channel::Stop => "stop -t 1",
        };
        write_script(&channel, &format!("d=$(dirname \"$0\")\necho \"$@\" >> \"$d/calls.txt\"\nexec {VM_DOCKER} {verb} {name}\n"));
        let channel_uri = format!("cmd:{}", channel.display());
        let device = format!("udp://127.0.0.1:{udp_port}");
        let grpc_port = free_tcp_port();
        let grpc_addr = format!("127.0.0.1:{grpc_port}");
        let mut flags: Vec<String> = ["--port-device", device.as_str(), "--edge-node-id", EDGE_NODE, "--power-control", channel_uri.as_str(), "--power-timeout-ms", "120000", "--handshake-timeout-ms", "30000", "--grpc-addr", grpc_addr.as_str()].iter().map(|s| s.to_string()).collect();
        flags.extend(["--io-log", io_log.to_str().unwrap(), "--signing-key", key_pem.to_str().unwrap(), "--signing-cert", cert_pem.to_str().unwrap()].iter().map(|s| s.to_string()));
        if let Some((interval, deadline)) = s.service_rehandshake {
            flags.extend(["--power-cycle-reboots-peer".to_string(), "--rehandshake-interval-ms".to_string(), interval.to_string(), "--rehandshake-timeout-ms".to_string(), deadline.to_string()]);
        }
        let launch = ctl.join("launch.sh");
        write_script(&launch, &format!("echo $$ > {ctl}/service.pid\nexec timeout {VM_SERVICE_CAP_SECS} {bin} {flags} 2> {stage}/service.stderr\n", ctl = ctl.display(), stage = stage.display(), bin = linux_binary().display(), flags = flags.join(" ")));
        write_script(&ctl.join("stop.sh"), &format!("pid=$(cat {ctl}/service.pid 2>/dev/null) || exit 0\n# TERM the service (a child of `timeout`) and `timeout` itself, wait for them to go (the service drains its\n# in-flight RPCs for up to 3 s first), and only then KILL what is left, children included.\npkill -TERM -P \"$pid\" 2>/dev/null\nkill -TERM \"$pid\" 2>/dev/null\ni=0\nwhile kill -0 \"$pid\" 2>/dev/null && [ $i -lt 10 ]; do sleep 1; i=$((i+1)); done\npkill -KILL -P \"$pid\" 2>/dev/null\nkill -KILL \"$pid\" 2>/dev/null\nexit 0\n", ctl = ctl.display()));
        let ssh = Command::new("colima").arg("ssh").arg("--").arg(&launch).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().expect("spawn colima ssh");
        let vm = VmService { ssh, ctl, stage };
        // Ready: the host reaches the service's gRPC over Lima's TCP forwarder, and the service says it listens.
        let t0 = Instant::now();
        loop {
            if std::net::TcpStream::connect(&grpc_addr).is_ok() && vm.stderr().contains("LockstepService listening") {
                break;
            }
            assert!(t0.elapsed() < Duration::from_secs(90), "the VM-side av-edge-board was not reachable at {grpc_addr} within 90 s; its stderr:\n{}", vm.stderr());
            std::thread::sleep(Duration::from_millis(100));
        }
        println!("[{}] VM-side av-edge-board serving {grpc_addr} (Lima-forwarded); {}", s.tag, vm.stderr().lines().find(|l| l.contains("handshake complete")).unwrap_or("(handshake line pending)"));

        let mut extra: Vec<(&str, f64)> = vec![("board.step_timeout_ms", s.step_timeout_ms as f64)];
        if let Some(ms) = s.declare_ms {
            extra.push(("board.rehandshake_timeout_ms", ms as f64));
        }
        let probe = Scene::new(&device, EDGE_NODE, &grpc_addr, &[], DURATION_S, vec![]);
        let fault_tai_ns = probe.start_tai_ns() + FAULT_S * 1_000_000_000;
        let fault = Fault { id: "pc1".to_string(), instance: CONTROLLER.to_string(), target_kind: FaultTargetKind::Hardware as i32, kind: "power_cycle".to_string(), tai_ns: fault_tai_ns, ..Default::default() };
        let scene = Scene::new(&device, EDGE_NODE, &grpc_addr, &extra, DURATION_S, vec![fault]).with_power_control(&channel_uri);
        Some(Rig { vm, container, lock, run_id, io_log, cert_pem, grpc_addr, scene })
    }

    fn run(&self, products_dir: Option<PathBuf>, boards: &[BoardLogReplay]) -> Result<RunProducts, DrmError> {
        execute_scene(&self.scene, products_dir, boards)
    }

    fn fault_tai_ns(&self) -> i64 {
        self.scene.start_tai_ns() + FAULT_S * 1_000_000_000
    }

    fn read_log(&self) -> VerifiedLog {
        let verifier = LogVerifier::from_pem(&std::fs::read(&self.cert_pem).unwrap()).unwrap();
        av_edge::board_log::read_log(&self.io_log, &verifier).unwrap_or_else(|e| panic!("the I/O log must verify: {e}"))
    }

    /// Stop everything in order while still holding the docker lock, and check nothing is left: no
    /// VM-side `av-edge-board` for this rig's gRPC address, no container with this run's label.
    fn finish(self) {
        let Rig { vm, container, lock, run_id, grpc_addr, .. } = self;
        drop(vm);
        drop(container);
        std::thread::sleep(Duration::from_millis(500));
        assert!(!vm_process_running(&format!("--grpc-addr {grpc_addr}")), "the VM-side av-edge-board for {grpc_addr} is still running");
        let left = docker(&["ps", "-aq", "--filter", &format!("label=av.test.run_id={run_id}")]).unwrap();
        assert!(left.is_empty(), "containers of this run remain: {left}");
        println!("NOTHING LEFT: no VM-side av-edge-board for {grpc_addr}, no container labelled av.test.run_id={run_id}");
        record_local_image_id();
        drop(lock);
    }
}

/// After every docker-gated run: `:local`'s id (the round's host rule); printed for the log.
fn record_local_image_id() {
    println!("LOCAL IMAGE ID after this docker-gated run: {:?}", docker(&["image", "inspect", "altavista-cfs-lockstep:local", "--format", "{{.Id}}"]));
}

fn execute_scene(scene: &Scene, products_dir: Option<PathBuf>, boards: &[BoardLogReplay]) -> Result<RunProducts, DrmError> {
    #[cfg(feature = "gmat")]
    let _engine = gmat_sys::engine_lock();
    #[cfg(feature = "gmat")]
    let gmat = gmat_sys::Gmat::setup(&gmat_sys::Gmat::default_startup_file()).expect("GMAT setup (default-feature build only)");
    execute_with_board_replay(
        RunConfig {
            #[cfg(feature = "gmat")]
            gmat: &gmat,
            drm: &scene.drm,
            sos: &scene.sos,
            systems: &scene.systems,
            run_id: RUN_ID.to_string(),
            error_mode: Default::default(),
            products_dir,
            replay: None,
            command_source: None,
        },
        boards,
    )
}

fn scratch(tag: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("drm-board-cfs-restart-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn kinds_of(log: &VerifiedLog) -> Vec<BoardIoKind> {
    log.records.iter().map(|r| BoardIoKind::try_from(r.kind).unwrap()).collect()
}

fn stripped(products: &RunProducts) -> pb::RunProducts {
    let mut wire = products.to_proto();
    strip_replay_exclusions(&mut wire, CONTROLLER, ReplaySource::EdgeLog);
    wire
}

// ------------------------------------------------------------------------------------------
// 1. The board reboots mid-run: the run completes, the controller resumes, the gap is logged, the replay equals
// ------------------------------------------------------------------------------------------

const MAIN: &str = "drm_board_cfs_restart::a_board_that_reboots_mid_run_is_rehandshaken_rebound_resumes_and_replays_from_its_signed_log";

#[test]
fn a_board_that_reboots_mid_run_is_rehandshaken_rebound_resumes_and_replays_from_its_signed_log() {
    let _serial = serial();
    println!("STAND-IN: `docker restart` of the cFS container stands in for a board reboot; the edge service runs inside the Colima VM; no board is involved");
    let Some(mut rig) = Rig::start(MAIN, &Scenario { tag: "main", channel: Channel::Restart, service_rehandshake: Some((1000, 60_000)), declare_ms: Some(60_000), step_timeout_ms: 10_000, port_offset: 0 }) else { return };
    let dir = scratch("main");
    let products_dir = dir.join("products");

    // ---- The live run.
    let t0 = Instant::now();
    let products = rig.run(Some(products_dir.clone()), &[]).unwrap_or_else(|e| panic!("the live run: {e}\nservice stderr:\n{}", rig.vm.stderr()));
    let wall = t0.elapsed();
    println!("LIVE RUN wall time {:.2} s (8 s of paced time plus the restart); pacing {:?}", wall.as_secs_f64(), products.pacing.as_ref().map(|p| (p.mode, p.ticks_paced, p.overrun_count, p.worst_overrun_ns)));
    assert_eq!(products.pacing.as_ref().expect("a board run is paced").ticks_paced, (DURATION_S * 10) as u64);
    assert!(rig.vm.wait_exit(Duration::from_secs(30)), "the VM-side service exits after the SHUTDOWN");

    // ---- (1) The controller resumes after the re-Bind and RESET, within the declared number of steps.
    let sidecar = std::fs::read(products_dir.join("port_traffic.pb")).expect("port_traffic.pb");
    let traffic = PortTrafficLog::decode(sidecar.as_slice()).unwrap();
    let fault_tai = rig.fault_tai_ns();
    let outs: Vec<i64> = traffic.records.iter().filter(|r| r.instance == CONTROLLER && r.port == "wheel_torque_out" && r.direction == PortDirection::Out as i32).map(|r| r.tai_ns).collect();
    let before = outs.iter().filter(|t| **t <= fault_tai).count();
    let after: Vec<i64> = outs.iter().copied().filter(|t| *t > fault_tai).collect();
    let steps_after = (DURATION_S - FAULT_S) * 10;
    let first_after = after.first().copied();
    let resumed_after_steps = first_after.map(|t| (t - fault_tai) / STEP_NS);
    println!("OUTPUTS: {before} controller outputs in the {} steps up to the fault; after the fault, first at +{resumed_after_steps:?} steps (declared at most {DECLARED_RESUME_STEPS}), {} outputs over the {steps_after} steps after it (declared at least {})", FAULT_S * 10, after.len(), steps_after - DECLARED_RESUME_STEPS);
    assert!(first_after.is_some_and(|t| t <= fault_tai + DECLARED_RESUME_STEPS * STEP_NS), "the controller must publish again within {DECLARED_RESUME_STEPS} steps of the fault (first output after it: {first_after:?}, fault epoch {fault_tai})");
    assert!(after.len() as i64 >= steps_after - DECLARED_RESUME_STEPS, "nearly every later step has an output: {} of {steps_after}", after.len());
    assert!(before as i64 >= FAULT_S * 10 - DECLARED_RESUME_STEPS, "and so did the steps before the fault ({before} of {})", FAULT_S * 10);

    // ---- (2) The gap is in the events and in the signed log.
    let fault_event = products.events.iter().find(|e| e.id == "fault:pc1").expect("the fault event");
    let outcome = products.events.iter().find(|e| e.id == "marker:power_cycle:pc1").expect("the power-cycle outcome event");
    println!("EVENT {fault_event:?}\nEVENT {outcome:?}");
    assert_eq!((fault_event.tai_ns, outcome.tai_ns), (fault_tai, fault_tai));
    assert_eq!(outcome.values["performed"], 1.0);
    assert_eq!(outcome.values["rehandshaken"], 1.0, "the outcome event carries the re-handshake");
    assert!(outcome.values["rehandshake_hellos_sent"] >= 1.0 && outcome.values["rehandshake_duration_ns"] > 0.0);
    assert!(outcome.detail.contains("re-handshook"), "{}", outcome.detail);
    let log = rig.read_log();
    let kinds = kinds_of(&log);
    use BoardIoKind::*;
    let mut expected = vec![Bind];
    expected.extend([Step; 30]);
    expected.extend([PowerCycle, LinkRehandshake, Bind, Reset]);
    expected.extend([Step; 50]);
    expected.push(Shutdown);
    assert_eq!(kinds, expected, "the signed log: BIND, 30 STEPs, POWER_CYCLE, LINK_REHANDSHAKE, BIND, RESET, 50 STEPs, SHUTDOWN (86 records)");
    assert_eq!(log.records.len(), 86);
    println!("SIGNED LOG, records around the gap:");
    for r in &log.records[30..37] {
        println!(
            "  seq={:>2} kind={:<16} lockstep_seq={:>2} until_tai_ns={} reset_tai_ns={} hellos={} timeout_ms={} written_unix_ns={} read_unix_ns={} error={:?} hash={}",
            r.sequence,
            BoardIoKind::try_from(r.kind).unwrap().as_str_name().trim_start_matches("BOARD_IO_KIND_"),
            r.lockstep_sequence,
            r.until_tai_ns,
            r.reset_tai_ns,
            r.rehandshake_hellos_sent,
            r.rehandshake_timeout_ms,
            r.request_written_unix_ns,
            r.response_read_unix_ns,
            r.error,
            av_edge::hash::hex_encode(&r.record_hash[..6])
        );
    }
    let (bind1, rh, bind2, reset) = (&log.records[0], &log.records[32], &log.records[33], &log.records[34]);
    assert_eq!(bind1.bind_request, bind2.bind_request, "the same Bind request again");
    assert!(bind2.bind_response.as_ref().unwrap().lockstep_capable);
    assert_eq!((rh.error.as_str(), rh.reset_reason.as_str(), rh.reset_tai_ns), ("", "fault:pc1", fault_tai));
    assert_eq!((reset.reset_reason.as_str(), reset.lockstep_sequence), ("fault:pc1", 31));
    assert_eq!(log.records[35].lockstep_sequence, 32);
    assert_eq!(log.records[84].lockstep_sequence, 81);

    // The guest's own side of it.
    let cfs = rig.container.logs();
    assert_eq!(cfs.matches("bound, entering step loop").count(), 2, "the guest bound once per container life:\n{cfs}");
    assert!(!cfs.contains("handshake failed") && !cfs.contains("BIND failed") && !cfs.contains("unexpected frame_type"), "{cfs}");
    let stderr = rig.vm.stderr();
    for line in stderr.lines().filter(|l| l.contains("handshake complete (") || l.contains("re-handshake")) {
        println!("MEASURED {line}");
    }

    // ---- (3) The replay from the signed log equals the live run after the named exclusions.
    let log_copy = dir.join("io.log");
    std::fs::copy(&rig.io_log, &log_copy).unwrap();
    let cert = std::fs::read(&rig.cert_pem).unwrap();
    let pin = BoardLogPin::from_trajectories(&products.trajectories, CONTROLLER).expect("the live run pinned its board log");
    assert_eq!(pin.records, 85, "the pin is taken before the SHUTDOWN record");
    let live_calls = std::fs::read_to_string(rig.vm_stage_calls()).unwrap_or_default().lines().count();
    let t0 = Instant::now();
    let replayed = execute_scene(&rig.scene, Some(products_dir.clone()), &[BoardLogReplay { instance: CONTROLLER.to_string(), log_path: log_copy.clone(), certificate_pem: cert.clone(), expected: pin.clone() }]).unwrap_or_else(|e| panic!("the replay from the signed log: {e}"));
    println!("REPLAY FROM THE SIGNED LOG wall time {:.2} s (no board, no service, no container dialled)", t0.elapsed().as_secs_f64());
    assert_eq!(std::fs::read_to_string(rig.vm_stage_calls()).unwrap_or_default().lines().count(), live_calls, "the replay never called the power control");
    assert!(replayed.pacing.is_none(), "a replay is lockstep");
    let live_wire = products.to_proto();
    let replay_wire = replayed.to_proto();
    let live_outcome = live_wire.events.iter().find(|e| e.id == "marker:power_cycle:pc1").unwrap();
    let replay_outcome = replay_wire.events.iter().find(|e| e.id == "marker:power_cycle:pc1").expect("the replay reproduces the outcome event");
    println!("EXCLUDED (live | replay): outcome.values {:?} | {:?}", live_outcome.values, replay_outcome.values);
    println!("EXCLUDED (live | replay): pacing {:?} | {:?}; overrun events {} | {}", live_wire.pacing.as_ref().map(|p| p.ticks_paced), replay_wire.pacing.as_ref().map(|p| p.ticks_paced), live_wire.events.iter().filter(|e| e.id.starts_with("marker:pacing:overrun:")).count(), replay_wire.events.iter().filter(|e| e.id.starts_with("marker:pacing:overrun:")).count());
    assert_eq!(live_outcome.values["rehandshaken"], replay_outcome.values["rehandshaken"], "rehandshaken is compared, not excluded");
    let (a, b) = (stripped(&products), stripped(&replayed));
    assert_eq!(a.trajectories, b.trajectories, "trajectories");
    assert_eq!(a.events, b.events, "events");
    assert_eq!(a.scores, b.scores, "scores");
    assert_eq!(a.provenance, b.provenance, "provenance");
    assert_eq!(a.port_traffic_hash, b.port_traffic_hash, "port_traffic_hash");
    assert_eq!(a, b, "the whole RunProducts after the named exclusions");
    assert_eq!(std::fs::read(products_dir.join("port_traffic.pb")).unwrap(), sidecar, "the replay's port_traffic.pb is byte-identical to the live run's");

    rig.finish();
    let _ = std::fs::remove_dir_all(&dir);
}

impl Rig {
    /// The power-channel fixture's call log (the VM-side script appends its argv to it).
    fn vm_stage_calls(&self) -> PathBuf {
        self.vm.stage.join("calls.txt")
    }
}

// ------------------------------------------------------------------------------------------
// 2. The peer never comes back
// ------------------------------------------------------------------------------------------

const NEVER: &str = "drm_board_cfs_restart::a_peer_that_never_comes_back_fails_the_run_with_the_typed_error_at_the_deadline";

#[test]
fn a_peer_that_never_comes_back_fails_the_run_with_the_typed_error_at_the_deadline() {
    let _serial = serial();
    const DEADLINE_MS: u64 = 4_000;
    let Some(rig) = Rig::start(NEVER, &Scenario { tag: "never", channel: Channel::Stop, service_rehandshake: Some((500, DEADLINE_MS)), declare_ms: Some(DEADLINE_MS), step_timeout_ms: 10_000, port_offset: 11 }) else { return };
    // Declared before running: the fault is 3 s in; the channel (`docker stop -t 1`) takes about a second;
    // the service gives up 4 s after its first HELLO. The run therefore ends about 8 s after it started,
    // and in any case within FAULT_S + the channel + the deadline + 20 s of slack.
    let bound = Duration::from_secs(FAULT_S as u64 + 10 + DEADLINE_MS / 1000 + 20);
    let t0 = Instant::now();
    let err = rig.run(None, &[]).expect_err("a peer that never comes back ends the run");
    let wall = t0.elapsed();
    println!("ABORTED after {:.2} s (bound {bound:?}): {err}", wall.as_secs_f64());
    assert!(wall < bound, "bounded wall time: {wall:?}");
    match (&err, err.power_control_error()) {
        (DrmError::BoardPowerCycle { instance, fault_id, .. }, Some(PowerControlError::RehandshakeFailed { failure, timeout_ms, hellos_sent, detail })) => {
            assert_eq!((instance.as_str(), fault_id.as_str()), (CONTROLLER, "pc1"));
            assert_eq!(failure, "BOARD_POWER_CYCLE_FAILURE_REHANDSHAKE_TIMEOUT");
            assert_eq!(*timeout_ms as u64, DEADLINE_MS);
            assert!((6..=9).contains(hellos_sent), "HELLOs at 0, 500, ... 3500 ms: {hellos_sent}");
            assert!(detail.contains("4000 ms"), "{detail}");
        }
        other => panic!("expected BoardPowerCycle(RehandshakeFailed), got {other:?}"),
    }
    let text = err.to_string();
    assert!(text.contains(CONTROLLER) && text.contains("pc1") && text.contains("4000 ms"), "the error names the instance, the fault and the deadline: {text}");
    // The signed log: the power cycle and the failed re-handshake; no second BIND, no RESET.
    let log = rig.read_log();
    let kinds = kinds_of(&log);
    println!("SIGNED LOG tail: {:?}", &kinds[kinds.len() - 3..]);
    assert_eq!(&kinds[kinds.len() - 2..], [BoardIoKind::PowerCycle, BoardIoKind::LinkRehandshake]);
    assert!(log.records.last().unwrap().error.contains("no peer answered"), "{:?}", log.records.last().unwrap().error);
    assert_eq!(kinds.iter().filter(|k| **k == BoardIoKind::Bind).count(), 1);
    assert!(!kinds.contains(&BoardIoKind::Reset), "no RESET was sent to a peer that was never re-bound");
    rig.finish();
}

// ------------------------------------------------------------------------------------------
// 3. Without the service's re-handshake the restart run fails
// ------------------------------------------------------------------------------------------

const NO_REHANDSHAKE: &str = "drm_board_cfs_restart::without_the_services_rehandshake_the_restart_run_fails_typed";

#[test]
fn without_the_services_rehandshake_the_restart_run_fails_typed() {
    let _serial = serial();
    // (a) The DRM declares the reboot; the service was not told: refused at once, nothing sent to the guest.
    {
        let Some(rig) = Rig::start(NO_REHANDSHAKE, &Scenario { tag: "norh-declared", channel: Channel::Restart, service_rehandshake: None, declare_ms: Some(30_000), step_timeout_ms: 10_000, port_offset: 23 }) else { return };
        let t0 = Instant::now();
        let err = rig.run(None, &[]).expect_err("the declared re-handshake did not happen");
        println!("(a) DECLARED, SERVICE NOT TOLD: aborted after {:.2} s: {err}", t0.elapsed().as_secs_f64());
        assert!(matches!(err.power_control_error(), Some(PowerControlError::RehandshakeExpected { declared_timeout_ms: 30_000 })), "{err:?}");
        let text = err.to_string();
        assert!(text.contains(CONTROLLER) && text.contains("pc1") && text.contains("--power-cycle-reboots-peer"), "{text}");
        let kinds = kinds_of(&rig.read_log());
        assert_eq!(kinds.last(), Some(&BoardIoKind::PowerCycle));
        assert!(!kinds.contains(&BoardIoKind::Reset) && !kinds.contains(&BoardIoKind::LinkRehandshake), "no RESET was sent to a guest waiting for a HELLO: {:?}", &kinds[kinds.len().saturating_sub(3)..]);
        rig.finish();
    }
    // (b) Nothing declared on either side: the RESET reaches a rebooted guest that is waiting for a HELLO.
    //     The guest cannot answer, and the run fails by `board.step_timeout_ms` (a short one here).
    {
        let Some(rig) = Rig::start(NO_REHANDSHAKE, &Scenario { tag: "norh-undeclared", channel: Channel::Restart, service_rehandshake: None, declare_ms: None, step_timeout_ms: 4_000, port_offset: 37 }) else { return };
        let t0 = Instant::now();
        let err = rig.run(None, &[]).expect_err("a RESET to a guest that was never re-handshaken cannot be answered");
        let wall = t0.elapsed();
        println!("(b) NOTHING DECLARED: aborted after {:.2} s: {err}", wall.as_secs_f64());
        assert!(matches!(err, DrmError::ContainerProtocol { .. }) && err.to_string().contains("Reset") && err.to_string().contains("no reply within 4000 ms"), "{err}");
        assert!(wall < Duration::from_secs(FAULT_S as u64 + 4 + 30), "{wall:?}");
        rig.finish();
    }
}
