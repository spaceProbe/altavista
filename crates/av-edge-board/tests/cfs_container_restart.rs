//! The real cFS posix container behind the real `av-edge-board` over UDP, restarted for real
//! (question 245 (6)). Docker-gated; takes the host-wide docker-test lock itself
//! (`av_lockstep::docker::lock_docker_tests`) and must never be wrapped in `with_docker_lock.py`.
//!
//! **`docker restart` of the container stands in for a board reboot. No board is involved.**
//!
//! The setup: `altavista-cfs-lockstep` (its `io_lockstep` app unchanged) started with
//! `LOCKSTEP_TRANSPORT=udp`, so `services/cfs/container-entrypoint.sh` runs `av-lockstep-relay`
//! where the shim would run; the relay carries the guest's lockstep-local stream over UDP, one
//! whole frame per datagram, to `av-edge-board` (`udp://127.0.0.1:<fixed published port>`). The
//! service has `--power-control cmd:<fixture>` whose power cycle is a real `docker restart` of that
//! container, and `--power-cycle-reboots-peer`. The driver is the kernel's own
//! `BlockingLockstepClient`, on the host.
//!
//! # Where the edge service runs, and why
//!
//! **On this host (Colima, `vmType: vz`, `portForwarder: ssh`) the edge service runs inside the
//! Colima VM, not on the host.** `~/.colima/default/colima.yaml` says of that forwarder "ssh is more
//! stable but supports only TCP", and the Lima host agent logs `Forwarding TCP from 127.0.0.1:N` for
//! every published TCP port and nothing for a published UDP port: a datagram sent from the host to
//! the container's published UDP port is never delivered (measured; crates/av-edge-board/README.md,
//! "Docker on macOS"). Inside the VM the same exchange works. So the test launches the Linux build of
//! `av-edge-board` (`services/cfs/build-edge-board.sh`, default output
//! `target-docker-linux/edge-board/av-edge-board`, or `AV_EDGE_BOARD_LINUX`) through `colima ssh`, with
//! its I/O log, signing identity, power-cycle fixture and stderr under `$HOME/<name>-stage/` (the VM
//! mounts `$HOME` at the same path). The VM stands in for the edge node the board hangs off: the
//! kernel stays on the host and dials the service's gRPC address over TCP, which Lima forwards (a
//! guest `127.0.0.1:<port>` listener appears on the host's `127.0.0.1:<port>`). The `docker restart`
//! the power channel runs is also the VM's `docker`. The VM-side process is stopped at the end of the
//! test (and when it panics) and checked gone with `pgrep`; it also runs under `timeout`, so it cannot
//! outlive its cap.
//!
//! A host where UDP does reach the published port (Linux Docker, or a Colima with a UDP-capable port
//! forwarder) runs the host build of the service instead; the test chooses by sending one datagram
//! from the host to the published port and seeing the relay count it. A host where neither works is a
//! visible SKIPPED naming both reasons (a panic under `AV_REQUIRE_DOCKER_TESTS=1`).
//!
//! # The run
//!
//! HELLO (startup), BIND, three STEPs, `PowerCycle` (a real `docker restart`: the cFS process and the
//! relay die, the container comes back, the guest connects to the relay again and waits for a HELLO),
//! the service's re-handshake, BIND again with the same request, RESET, STEPs answered. Evidence: the
//! HELLO reply time and the HELLOs sent (logged by the service), how many HELLOs the rebooted
//! container dropped before its guest was listening (logged by the relay), the guest's own event log
//! (`bound, entering step loop` twice, never a handshake/BIND failure), the signed log's records, and
//! that nothing the test started is left (no container with this run's label, no VM-side process).
//!
//! Gate: Docker, and the image (`AV_CFS_TEST_IMAGE`, default `altavista-cfs-lockstep:local`) **with the
//! relay in it**.
mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use av_edge::board_log::BoardIoKind;
use av_lockstep::board_edge::{BlockingBoardEdgeClient, BoardPowerCycleOutcome, PowerCycleRequest};
use av_lockstep::docker::{announce_gate_skip, docker_daemon_status, local_image_id, lock_docker_tests, prune_stale_test_resources, test_label_args, test_run_id, DockerGateReason};
use av_lockstep::{BlockingLockstepClient, LockstepResetRequest, LockstepShutdownRequest, LockstepStepRequest};
use common::*;

const TEST_NAME: &str = "cfs_container_restart::the_real_cfs_container_is_rehandshaken_and_rebound_after_a_docker_restart";
const DEFAULT_IMAGE: &str = "altavista-cfs-lockstep:local";
const IMAGE_ENV: &str = "AV_CFS_TEST_IMAGE";
const LINUX_BINARY_ENV: &str = "AV_EDGE_BOARD_LINUX";
const RELAY_UDP_PORT: u16 = 50090;
/// The VM-side `docker` (Ubuntu's package) and the cap the VM-side service runs under.
const VM_DOCKER: &str = "/usr/bin/docker";
const VM_SERVICE_CAP_SECS: u32 = 1500;

fn docker(args: &[&str]) -> Result<String, String> {
    let out = Command::new("docker").args(args).output().map_err(|e| format!("could not launch docker: {e}"))?;
    if !out.status.success() {
        return Err(format!("docker {args:?} failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Removes the container at drop, whatever happened.
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
        let _ = Command::new("docker").args(["rm", "-f", &self.name]).output();
    }
}

fn wait_for(what: &str, timeout: Duration, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !done() {
        assert!(Instant::now() < deadline, "timed out after {timeout:?} waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn image_has_relay(image: &str) -> bool {
    Command::new("docker").args(["run", "--rm", "--entrypoint", "test"]).args(test_label_args(&test_run_id())).args([image, "-x", "/cfs/av-lockstep-relay"]).output().map(|o| o.status.success()).unwrap_or(false)
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

fn linux_binary() -> PathBuf {
    std::env::var(LINUX_BINARY_ENV).ok().filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| repo_root().join("target-docker-linux/edge-board/av-edge-board"))
}

/// Run `command` inside the Colima VM and return (success, combined output).
fn colima(args: &[&str]) -> (bool, String) {
    match Command::new("colima").arg("ssh").arg("--").args(args).output() {
        Ok(o) => (o.status.success(), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)).trim().to_string()),
        Err(e) => (false, format!("could not launch colima: {e}")),
    }
}

/// Why the in-VM service cannot be used here, if it cannot.
fn vm_path_unavailable() -> Option<String> {
    let bin = linux_binary();
    if !bin.is_file() {
        return Some(format!("the Linux av-edge-board is missing at {} (build it with services/cfs/build-edge-board.sh, or set {LINUX_BINARY_ENV})", bin.display()));
    }
    let (ok, out) = colima(&[bin.to_str().unwrap(), "--help"]);
    // `--help` is a usage error (status 2) that prints the usage text: that proves the VM can run it.
    if !out.contains("usage: av-edge-board") {
        return Some(format!("the Colima VM could not run {} ({}): {out}", bin.display(), if ok { "ran, but printed no usage" } else { "colima ssh failed" }));
    }
    None
}

fn write_script(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, format!("#!/bin/sh\n{body}")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Owns the VM-side service's control directory (`launch.sh`, the pid file, `stop.sh`) and stops
/// the service at drop, then removes the directory. It is separate from the service's stage
/// directory, which `Service` removes at its own drop, so the stop script is still there when the
/// guard runs, in either drop order. Checked, not trusted: the test also looks for the process.
struct VmService {
    ctl: PathBuf,
}

impl Drop for VmService {
    fn drop(&mut self) {
        let stop = self.ctl.join("stop.sh");
        if stop.exists() {
            let _ = colima(&[stop.to_str().unwrap()]);
        }
        let _ = std::fs::remove_dir_all(&self.ctl);
    }
}

/// Is a VM-side process whose command line has this unique argument still running?
fn vm_process_running(unique_arg: &str) -> bool {
    // The `[x]` trick keeps `pgrep -f` from matching the shell that runs it.
    let pattern = format!("[{}]{}", &unique_arg[..1], &unique_arg[1..]);
    colima(&["pgrep", "-f", &pattern]).0
}

#[test]
fn the_real_cfs_container_is_rehandshaken_and_rebound_after_a_docker_restart() {
    let lock = lock_docker_tests();
    if let Err(reason) = docker_daemon_status() {
        announce_gate_skip(TEST_NAME, &reason);
        return;
    }
    let image = std::env::var(IMAGE_ENV).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| DEFAULT_IMAGE.to_string());
    if local_image_id(&image).is_err() {
        announce_gate_skip(TEST_NAME, &DockerGateReason::ImageNotBuilt { image_ref: image.clone(), build_hint: "build it with services/cfs/build-shim.sh then `docker build -f services/cfs/Dockerfile -t <tag> .`, and point AV_CFS_TEST_IMAGE at it".to_string() });
        return;
    }
    if !image_has_relay(&image) {
        announce_gate_skip(TEST_NAME, &DockerGateReason::PrerequisiteUnavailable { what: format!("/cfs/av-lockstep-relay in image {image}"), hint: "rebuild the image with services/cfs/build-shim.sh and `docker build` (question 245 (6) added the relay), or set AV_CFS_TEST_IMAGE to a run-scoped tag that has it".to_string() });
        return;
    }
    prune_stale_test_resources(&lock);

    let run_id = test_run_id();
    let labels = test_label_args(&run_id);
    // A fixed published UDP port (an ephemeral one can change when the container restarts), from a
    // range below both the macOS and the Linux ephemeral ranges.
    let host_port: u16 = 20_000 + (std::process::id() % 9_000) as u16;
    let name = format!("av-edge-board-cfs-restart-{}", std::process::id());
    let publish = format!("127.0.0.1:{host_port}:{RELAY_UDP_PORT}/udp");
    let mut args: Vec<&str> = vec!["run", "-d", "--name", &name, "-p", &publish, "--sysctl", "fs.mqueue.msg_max=256", "--sysctl", "fs.mqueue.msgsize_max=65536", "-e", "LOCKSTEP_TRANSPORT=udp"];
    args.extend(labels.iter().map(String::as_str));
    args.push(&image);
    docker(&args).unwrap_or_else(|e| panic!("starting the cFS container: {e}"));
    let container = Container { name: name.clone() };
    println!("container {name} from {image} ({:?}), UDP published at 127.0.0.1:{host_port}", local_image_id(&image));

    // The relay is listening and the guest (io_lockstep) has connected to it.
    wait_for("the guest to connect to the relay", Duration::from_secs(60), || container.logs().contains("guest connected (session 1)"));

    // Where can the edge service reach the published UDP port from? Send the relay a one-byte (so
    // malformed, and logged by the relay) datagram from the host.
    let host_udp_works = {
        let probe = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let mut reached = false;
        for _ in 0..4 {
            probe.send_to(&[0u8], ("127.0.0.1", host_port)).unwrap();
            std::thread::sleep(Duration::from_millis(300));
            if container.logs().contains("dropped a malformed datagram") {
                reached = true;
                break;
            }
        }
        reached
    };
    let vm_unavailable = if host_udp_works { None } else { vm_path_unavailable() };
    if !host_udp_works {
        if let Some(why) = vm_unavailable {
            let reason = DockerGateReason::PrerequisiteUnavailable {
                what: "a path from the edge service to the container's published UDP port: from the host, UDP does not reach it; in the Docker VM, it is unavailable".to_string(),
                hint: format!(
                    "(1) host: Colima's default `portForwarder: ssh` forwards TCP only (its host agent logs `Forwarding TCP from ...` for each published TCP port and nothing for UDP), so use a Docker VM whose forwarder carries UDP or a Linux Docker host; (2) VM: {why}"
                ),
            };
            drop(container);
            announce_gate_skip(TEST_NAME, &reason);
            return;
        }
    }
    let in_vm = !host_udp_works;
    println!("edge service runs {}", if in_vm { "INSIDE the Colima VM (host->container UDP is not forwarded by this Colima), gRPC reached over Lima's TCP forwarding" } else { "on the host (UDP reaches the published port)" });

    // --- the stage directory: identity, I/O log, power fixture, stderr (under $HOME so the VM sees it) ---
    let home = PathBuf::from(std::env::var("HOME").expect("HOME"));
    let stage = home.join(format!("av-edge-board-cfs-restart-stage-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&stage);
    std::fs::create_dir_all(&stage).unwrap();
    let identity = make_identity(&stage, "edge-test");
    let io_log = stage.join("io.log");
    let stderr_path = stage.join("service.stderr");
    let docker_path = if in_vm {
        VM_DOCKER.to_string()
    } else {
        let p = Command::new("which").arg("docker").output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
        assert!(p.starts_with('/'), "docker resolved to {p:?}");
        p
    };
    let channel = stage.join("power-cycle");
    write_script(&channel, &format!("d=$(dirname \"$0\")\necho \"$@\" >> \"$d/calls.txt\"\nexec {docker_path} restart -t 2 {name}\n"));
    let channel_uri = format!("cmd:{}", channel.display());
    let device = format!("udp://127.0.0.1:{host_port}");
    let grpc_port = free_tcp_port();
    let grpc_addr = format!("127.0.0.1:{grpc_port}");
    let service_flags: Vec<String> = [
        "--port-device",
        device.as_str(),
        "--edge-node-id",
        "edge-cfs",
        "--power-control",
        channel_uri.as_str(),
        "--power-timeout-ms",
        "120000",
        "--power-cycle-reboots-peer",
        "--rehandshake-interval-ms",
        "1000",
        "--rehandshake-timeout-ms",
        "120000",
        "--handshake-timeout-ms",
        "30000",
        "--grpc-addr",
        grpc_addr.as_str(),
        "--io-log",
        io_log.to_str().unwrap(),
        "--signing-key",
        identity.key_pem.to_str().unwrap(),
        "--signing-cert",
        identity.cert_pem.to_str().unwrap(),
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let mut board_params = BTreeMap::new();
    board_params.insert(av_edge::board::BIND_PARAM_EDGE_NODE_ID.to_string(), "edge-cfs".to_string());
    board_params.insert(av_edge::board::BIND_PARAM_PORT_DEVICE.to_string(), av_edge::board::parse_port_device(&device).unwrap().canonical());

    let _vm_guard;
    let mut service = if in_vm {
        let bin = linux_binary();
        let ctl = home.join(format!("av-edge-board-cfs-restart-ctl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&ctl);
        std::fs::create_dir_all(&ctl).unwrap();
        _vm_guard = VmService { ctl: ctl.clone() };
        let launch = ctl.join("launch.sh");
        write_script(&launch, &format!("echo $$ > {ctl}/service.pid\nexec timeout {VM_SERVICE_CAP_SECS} {bin} {flags} 2> {stage}/service.stderr\n", ctl = ctl.display(), stage = stage.display(), bin = bin.display(), flags = service_flags.join(" ")));
        write_script(&ctl.join("stop.sh"), &format!("pid=$(cat {ctl}/service.pid 2>/dev/null) || exit 0\n# TERM the service (a child of `timeout`) and `timeout` itself, wait for them to go (the service drains its\n# in-flight RPCs for up to 3 s first), and only then KILL what is left, children included.\npkill -TERM -P \"$pid\" 2>/dev/null\nkill -TERM \"$pid\" 2>/dev/null\ni=0\nwhile kill -0 \"$pid\" 2>/dev/null && [ $i -lt 10 ]; do sleep 1; i=$((i+1)); done\npkill -KILL -P \"$pid\" 2>/dev/null\nkill -KILL \"$pid\" 2>/dev/null\nexit 0\n", ctl = ctl.display()));
        let child = Command::new("colima").arg("ssh").arg("--").arg(&launch).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().expect("spawn colima ssh");
        Service::from_parts(child, grpc_addr.clone(), stderr_path, stage.clone(), identity, io_log, board_params)
    } else {
        let stderr = std::fs::File::create(&stderr_path).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_av-edge-board")).args(&service_flags).stdout(Stdio::null()).stderr(Stdio::from(stderr)).spawn().expect("spawn av-edge-board");
        Service::from_parts(child, grpc_addr.clone(), stderr_path, stage.clone(), identity, io_log, board_params)
    };
    // The host reaches the service's gRPC over TCP (for the VM, through Lima's port forwarder).
    service.wait_ready(Duration::from_secs(90));
    let params = service.bind_params(&BTreeMap::new());
    let mut lock = BlockingLockstepClient::connect_plaintext(&service.grpc_addr).unwrap();
    assert!(lock.bind(client_bind_request(&params)).unwrap().lockstep_capable);

    let step = |lock: &mut BlockingLockstepClient, seq: u64| {
        let until = seq as i64 * BASE_PERIOD_NS;
        let r = lock.step(LockstepStepRequest { sequence: seq, until_tai_ns: until, inputs: Vec::new() }).unwrap_or_else(|e| panic!("STEP {seq}: {e}"));
        assert_eq!((r.sequence, r.reached_tai_ns), (seq, until));
    };
    for seq in 1..=3 {
        step(&mut lock, seq);
    }

    // The power cycle: a real `docker restart`, then the service's re-handshake.
    let mut edge = BlockingBoardEdgeClient::connect_plaintext(&service.grpc_addr).unwrap();
    let t0 = Instant::now();
    let resp = edge
        .power_cycle(PowerCycleRequest { run_id: RUN_ID.into(), instance: INSTANCE.into(), fault_id: "pc1".into(), tai_ns: 3 * BASE_PERIOD_NS, edge_node_id: "edge-cfs".into(), power_control: channel_uri.clone() })
        .expect("PowerCycle RPC");
    println!("PowerCycle took {:?}: {resp:?}", t0.elapsed());
    assert_eq!(resp.outcome, BoardPowerCycleOutcome::Performed as i32, "{resp:?}");
    assert!(resp.peer_rehandshaken, "{resp:?}");
    assert!(resp.rehandshake_hellos_sent >= 1 && resp.rehandshake_duration_ns > 0, "{resp:?}");

    // Same Bind request again, then RESET at the fault's epoch, then the run continues.
    assert!(lock.bind(client_bind_request(&params)).unwrap().lockstep_capable);
    lock.reset(LockstepResetRequest { sequence: 4, tai_ns: 3 * BASE_PERIOD_NS, reason: "fault:pc1".into() }).unwrap();
    for seq in 4..=8 {
        step(&mut lock, seq);
    }
    lock.shutdown(LockstepShutdownRequest { run_id: RUN_ID.into() }).unwrap();
    service.wait_exit(Duration::from_secs(60));

    // Evidence.
    let cfs_log = container.logs();
    let service_stderr = service.stderr();
    println!("---- container log ----\n{cfs_log}\n---- av-edge-board stderr ({}) ----\n{service_stderr}", if in_vm { "in the Colima VM" } else { "on the host" });
    assert_eq!(cfs_log.matches("guest connected (session 1)").count(), 2, "the relay (a new process after the restart) saw its guest connect twice in all: {cfs_log}");
    assert!(!cfs_log.contains("handshake failed") && !cfs_log.contains("BIND failed") && !cfs_log.contains("unexpected frame_type"), "the guest reported a failure:\n{cfs_log}");
    assert_eq!(cfs_log.matches("bound, entering step loop").count(), 2, "the guest bound twice, once per container life:\n{cfs_log}");
    assert!(service_stderr.contains("handshake complete (") && service_stderr.contains("re-handshake: HELLO #") && service_stderr.contains("answered"), "{service_stderr}");
    for line in service_stderr.lines().filter(|l| l.contains("handshake complete (") || l.contains("re-handshake: HELLO #") || l.contains("re-handshake after the power cycle")) {
        println!("MEASURED {line}");
    }
    use BoardIoKind::*;
    let kinds: Vec<BoardIoKind> = service.read_io_log().records.iter().map(|r| BoardIoKind::try_from(r.kind).unwrap()).collect();
    let mut expected = vec![Bind, Step, Step, Step, PowerCycle, LinkRehandshake, Bind, Reset];
    expected.extend([Step; 5]);
    expected.push(Shutdown);
    assert_eq!(kinds, expected);
    service.save_evidence("cfs_container_restart.stderr.txt");

    // Nothing left behind: the VM-side service exited by itself after the Shutdown, the container is
    // removed, and no container of this run remains.
    if in_vm {
        std::thread::sleep(Duration::from_millis(500));
        assert!(!vm_process_running(&format!("--grpc-addr {grpc_addr}")), "the VM-side av-edge-board is still running after the test");
    }
    drop(container);
    let left = docker(&["ps", "-aq", "--filter", &format!("label=av.test.run_id={run_id}")]).unwrap();
    assert!(left.is_empty(), "containers of this run remain: {left}");
}
