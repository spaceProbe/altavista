//! `BoardBinding.power_control` as an edge-service operation, wired to the power-cycle fault
//! (question 242 (c), hilprep-4), **against stand-ins only**: the real `av-edge-board` binary in
//! front of a fake lockstep-local guest on a loopback UDP socket, with a recording `cmd:` fake
//! (`drm_board_common::FakeChannel`) as the edge node's power control channel. No board, no Renode,
//! no Docker.
//!
//! What is proven: a `FAULT_TARGET_KIND_HARDWARE` / `power_cycle` fault on a board instance makes
//! the kernel ask the edge service over `BoardEdgeService.PowerCycle`; the edge service (not the
//! kernel) runs the channel exactly once with the right argv; the fake guest then sees a `RESET`
//! with the fault's reason; the board I/O log holds a `POWER_CYCLE` record right before it; the
//! outcome is an event; the run completes. A refused or failed power cycle ends the run with the
//! edge service's reason and no `RESET`. Load-time refusals need no connection.
//!
//! Runs in both feature states of `av-kernel` (nothing here needs GMAT).
mod drm_board_common;

use av_cdm::pb::{Fault, FaultTargetKind};
use av_edge::board_log::BoardIoKind;
use av_kernel::drm::power::{is_power_cycle_event, PowerControlError};
use av_kernel::drm::DrmError;
use av_kernel::pacing::is_wall_clock_event;
use drm_board_common::*;

const START_TAI_NS: i64 = 1_767_225_637_000_000_000;
/// The fault's epoch: one second into the run, on the 100 ms grid (the tenth step's boundary).
const FAULT_TAI_NS: i64 = START_TAI_NS + 1_000_000_000;
const ARC_S: i64 = 3;
const STEPS: u64 = 30;

fn power_cycle_fault(kind: &str) -> Fault {
    Fault { id: "pc1".to_string(), instance: CONTROLLER.to_string(), target_kind: FaultTargetKind::Hardware as i32, kind: kind.to_string(), tai_ns: FAULT_TAI_NS, ..Default::default() }
}

fn power_arg(channel: &FakeChannel) -> Vec<String> {
    vec!["--power-control".to_string(), channel.uri()]
}

/// The decisive end-to-end proof.
#[test]
fn a_power_cycle_fault_runs_the_edge_services_channel_once_then_resets_the_guest() {
    let _serial = serial();
    let channel = FakeChannel::create("e2e");
    let guest = FakeGuest::spawn(GuestPlan::default());
    let device = udp_device(&guest);
    let service = Service::start_with("power", &device, "zcu104-test", &power_arg(&channel));
    let scene = Scene::new(&device, "zcu104-test", &service.grpc_addr, &[], ARC_S, vec![power_cycle_fault("power_cycle")]).with_power_control(&channel.uri());

    let products = scene.run().unwrap_or_else(|e| panic!("the run: {e}\nservice stderr:\n{}", service.stderr()));

    // 1. The channel ran exactly once, with the documented argv, as a child of the EDGE SERVICE.
    let calls = channel.calls();
    assert_eq!(calls.len(), 1, "exactly one power cycle: {calls:?}");
    let argv = FakeChannel::argv(&calls[0]);
    println!("CHANNEL CALL {}", calls[0]);
    assert_eq!(
        argv,
        ["power-cycle", "--edge-node-id", "zcu104-test", "--instance", CONTROLLER, "--fault-id", "pc1", "--tai-ns", &FAULT_TAI_NS.to_string()],
        "argv"
    );
    let ppid = calls[0]["ppid"].as_u64().unwrap() as u32;
    assert_eq!(ppid, service.pid(), "the channel's parent is the edge service, not the kernel");
    assert_ne!(ppid, std::process::id(), "the kernel process (this test) never executed the channel");
    assert_eq!(calls[0]["cwd"].as_str(), Some("/"));

    // 2. The fake guest saw one RESET right after, with the fault's reason, at the fault's epoch.
    {
        let seen = guest.seen.lock().unwrap();
        assert_eq!(seen.resets.len(), 1, "one RESET: {:?}", seen.resets);
        assert_eq!(seen.resets[0].reason, "fault:pc1");
        assert_eq!(seen.resets[0].tai_ns, FAULT_TAI_NS);
        // 3. The run completed: every step reached the guest once, in order, around the RESET. The
        //    RESET takes the next lockstep sequence number (11) like any request, so the steps
        //    are 1..=10 and 12..=31, and the sequence is unbroken: no step lost, none repeated.
        assert_eq!(seen.resets[0].sequence, 11);
        assert_eq!(seen.step_sequences, (1..=10).chain(12..=STEPS + 1).collect::<Vec<_>>());
    }
    let pacing = products.pacing.as_ref().expect("a board run is paced");
    assert_eq!(pacing.ticks_paced, STEPS);
    println!("PACING {pacing:?}");
    println!("OVERRUN EVENTS {:?}", products.events.iter().filter(|e| av_kernel::pacing::is_overrun_event(e)).map(|e| (e.tai_ns, e.values.clone())).collect::<Vec<_>>());

    // 4. The events: the container-style fault event, and the outcome event (wall-clock dependent).
    let fault_event = products.events.iter().find(|e| e.id == "fault:pc1").expect("the fault event");
    assert_eq!((fault_event.entity_id.as_str(), fault_event.tai_ns), (CONTROLLER, FAULT_TAI_NS));
    let outcome = products.events.iter().find(|e| is_power_cycle_event(e)).expect("the power-cycle outcome event");
    println!("OUTCOME EVENT {outcome:?}");
    assert_eq!((outcome.id.as_str(), outcome.entity_id.as_str(), outcome.tai_ns, outcome.reference_id.as_str()), ("marker:power_cycle:pc1", CONTROLLER, FAULT_TAI_NS, "pc1"));
    assert_eq!(outcome.values["performed"], 1.0);
    assert!(outcome.values["duration_ns"] > 0.0, "the edge node measured the channel's duration");
    assert!(outcome.detail.contains(&channel.uri()), "{}", outcome.detail);
    assert!(is_wall_clock_event(outcome), "a replay excludes it with the pacing overruns");
    assert!(!is_wall_clock_event(fault_event), "the fault event is deterministic and compared");
    assert_eq!(products.events.iter().filter(|e| is_power_cycle_event(e)).count(), 1);

    // 5. The board I/O log shows the operation between the STEP records either side of it, right
    //    before the RESET (the signed, hash-chained log verifies end to end).
    let log = service.read_io_log();
    let kinds: Vec<BoardIoKind> = log.records.iter().map(|r| BoardIoKind::try_from(r.kind).unwrap()).collect();
    let at = kinds.iter().position(|k| *k == BoardIoKind::PowerCycle).expect("a POWER_CYCLE record");
    assert_eq!(kinds.iter().filter(|k| **k == BoardIoKind::PowerCycle).count(), 1);
    assert_eq!(kinds[at + 1], BoardIoKind::Reset, "the RESET follows the power cycle");
    assert_eq!(kinds[at - 1], BoardIoKind::Step, "a STEP precedes it");
    let pc = &log.records[at];
    assert_eq!((pc.run_id.as_str(), pc.instance.as_str(), pc.reset_tai_ns, pc.reset_reason.as_str(), pc.error.as_str()), ("test-drm-board", CONTROLLER, FAULT_TAI_NS, "fault:pc1", ""));
    assert!(pc.request_written_unix_ns > 0 && pc.response_read_unix_ns >= pc.request_written_unix_ns);
    assert_eq!(log.records[at + 1].reset_reason, "fault:pc1");
    println!("IO LOG kinds around the power cycle: {:?}", &kinds[at - 1..=at + 2]);
    assert!(service.stderr().contains("PowerCycle Performed"), "{}", service.stderr());
    guest.finish();
}

/// The channel failing ends the run with the edge service's typed reason; no RESET is sent.
#[test]
fn a_failing_channel_aborts_the_run_with_the_edge_services_reason_and_sends_no_reset() {
    let _serial = serial();
    let channel = FakeChannel::create("fail");
    channel.set_mode("fail");
    let guest = FakeGuest::spawn(GuestPlan::default());
    let device = udp_device(&guest);
    let service = Service::start_with("power-fail", &device, "zcu104-test", &power_arg(&channel));
    let scene = Scene::new(&device, "zcu104-test", &service.grpc_addr, &[], ARC_S, vec![power_cycle_fault("power_cycle")]).with_power_control(&channel.uri());

    let err = scene.run().expect_err("a failed power cycle ends the run");
    println!("ABORTED: {err}");
    match (&err, err.power_control_error()) {
        (DrmError::BoardPowerCycle { instance, fault_id, .. }, Some(PowerControlError::Failed { failure, exit_status, detail, stderr_tail, .. })) => {
            assert_eq!((instance.as_str(), fault_id.as_str()), (CONTROLLER, "pc1"));
            assert_eq!(failure, "BOARD_POWER_CYCLE_FAILURE_EXIT_STATUS");
            assert_eq!(*exit_status, 3);
            assert!(detail.contains("status 3") && stderr_tail.contains("relay stuck"), "{detail} / {stderr_tail}");
        }
        other => panic!("expected BoardPowerCycle(Failed), got {other:?}"),
    }
    assert!(err.to_string().contains("relay stuck"), "the display carries the service's reason: {err}");
    assert_eq!(channel.calls().len(), 1);
    let seen = guest.seen.lock().unwrap();
    assert!(seen.resets.is_empty(), "no RESET after a power cycle that did not happen");
    assert_eq!(seen.step_sequences, (1..=10).collect::<Vec<_>>(), "the run stopped at the fault's boundary");
    drop(seen);
    // The failed operation was started, so it is in the log, with its error.
    let log = service.read_io_log();
    let pc = log.records.iter().find(|r| r.kind == BoardIoKind::PowerCycle as i32).expect("the failed power cycle is logged");
    assert!(pc.error.contains("ExitStatus") && pc.error.contains("status 3"), "{:?}", pc.error);
    guest.finish();
}

/// A channel that outlives the edge service's timeout is killed, and the run aborts with TIMEOUT.
#[test]
fn a_channel_that_hangs_is_killed_by_the_edge_service_and_aborts_the_run() {
    let _serial = serial();
    let channel = FakeChannel::create("hang");
    channel.set_mode("sleep");
    let guest = FakeGuest::spawn(GuestPlan::default());
    let device = udp_device(&guest);
    let mut args = power_arg(&channel);
    args.extend(["--power-timeout-ms".to_string(), "700".to_string()]);
    let service = Service::start_with("power-hang", &device, "zcu104-test", &args);
    let scene = Scene::new(&device, "zcu104-test", &service.grpc_addr, &[], ARC_S, vec![power_cycle_fault("power_cycle")]).with_power_control(&channel.uri());

    let err = scene.run().expect_err("a hung channel ends the run");
    assert!(matches!(err.power_control_error(), Some(PowerControlError::Failed { failure, .. }) if failure == "BOARD_POWER_CYCLE_FAILURE_TIMEOUT"), "{err:?}");
    let pid: i32 = std::fs::read_to_string(channel.dir.join("sleep.pid")).expect("the fake wrote its pid").trim().parse().unwrap();
    // `kill -0` only probes existence: it fails once the process is gone.
    let alive = std::process::Command::new("/bin/kill").args(["-0", &pid.to_string()]).stderr(std::process::Stdio::null()).status().unwrap().success();
    assert!(!alive, "the hung channel (pid {pid}) is really dead");
    assert!(guest.seen.lock().unwrap().resets.is_empty());
    guest.finish();
}

/// A binding whose `power_control` differs from the service's own configuration is refused by the
/// service: nothing is run, the run aborts with CHANNEL_MISMATCH naming both.
#[test]
fn a_binding_whose_power_control_differs_from_the_services_is_refused_and_nothing_runs() {
    let _serial = serial();
    let channel = FakeChannel::create("mismatch");
    let other = FakeChannel::create("mismatch-other");
    let guest = FakeGuest::spawn(GuestPlan::default());
    let device = udp_device(&guest);
    let service = Service::start_with("power-mismatch", &device, "zcu104-test", &power_arg(&channel));
    let scene = Scene::new(&device, "zcu104-test", &service.grpc_addr, &[], ARC_S, vec![power_cycle_fault("power_cycle")]).with_power_control(&other.uri());

    let err = scene.run().expect_err("a mismatched channel is refused");
    println!("REFUSED: {err}");
    match err.power_control_error() {
        Some(PowerControlError::Refused { refusal, detail }) => {
            assert_eq!(refusal, "BOARD_POWER_CYCLE_REFUSAL_CHANNEL_MISMATCH");
            assert!(detail.contains(&other.uri()) && detail.contains(&channel.uri()), "{detail}");
        }
        other => panic!("expected BoardPowerCycle(Refused), got {other:?}"),
    }
    assert!(channel.calls().is_empty() && other.calls().is_empty(), "neither executable ran");
    assert!(guest.seen.lock().unwrap().resets.is_empty());
    guest.finish();
}

/// An edge service with no `--power-control` refuses a request for a channel the binding declares.
#[test]
fn an_edge_service_with_no_channel_refuses_and_nothing_runs() {
    let _serial = serial();
    let channel = FakeChannel::create("nochannel");
    let guest = FakeGuest::spawn(GuestPlan::default());
    let device = udp_device(&guest);
    let service = Service::start("power-nochannel", &device, "zcu104-test");
    let scene = Scene::new(&device, "zcu104-test", &service.grpc_addr, &[], ARC_S, vec![power_cycle_fault("power_cycle")]).with_power_control(&channel.uri());

    let err = scene.run().expect_err("no channel on the service");
    assert!(matches!(err.power_control_error(), Some(PowerControlError::Refused { refusal, .. }) if refusal == "BOARD_POWER_CYCLE_REFUSAL_NO_CHANNEL"), "{err:?}");
    assert!(channel.calls().is_empty());
    assert!(guest.seen.lock().unwrap().resets.is_empty());
    guest.finish();
}

/// Load-time: no `power_control` on the binding and a power-cycle fault is a typed refusal before
/// any connection (nothing listens at the edge address, so a connection attempt would be
/// `BoardConnect`).
#[test]
fn a_power_cycle_fault_without_a_power_control_channel_is_refused_at_load() {
    let _serial = serial();
    let port = free_tcp_port();
    let scene = Scene::new("udp://127.0.0.1:9", "zcu104-test", &format!("127.0.0.1:{port}"), &[], ARC_S, vec![power_cycle_fault("power_cycle")]);
    let err = scene.run().expect_err("no channel");
    assert!(matches!(&err, DrmError::BoardPowerCycleNeedsChannel { fault_id, instance } if fault_id == "pc1" && instance == CONTROLLER), "{err:?}");
    println!("REFUSED: {err}");
}

/// Load-time: a malformed `power_control` is refused even when no fault uses it; `gpio://` is the
/// reserved refusal.
#[test]
fn a_malformed_or_reserved_power_control_is_refused_at_load_even_without_a_fault() {
    let _serial = serial();
    let port = free_tcp_port();
    for bad in ["gpio://17", "cmd:relative/path", "cmd:/a b", "ssh:host", "power"] {
        let scene = Scene::new("udp://127.0.0.1:9", "zcu104-test", &format!("127.0.0.1:{port}"), &[], 1, vec![]).with_power_control(bad);
        let err = scene.run().expect_err("a bad power_control is refused");
        assert!(matches!(&err, DrmError::BoardPowerControl { instance, .. } if instance == CONTROLLER), "{bad:?}: {err:?}");
        println!("REFUSED[{bad}] {err}");
    }
    let scene = Scene::new("udp://127.0.0.1:9", "zcu104-test", &format!("127.0.0.1:{port}"), &[], 1, vec![]).with_power_control("gpio://17");
    assert!(scene.run().unwrap_err().to_string().contains("reserved"));
}

/// Load-time: any other HARDWARE kind on a board is refused by kind, as for a container.
#[test]
fn another_hardware_kind_on_a_board_is_refused_at_load() {
    let _serial = serial();
    let channel = FakeChannel::create("kind");
    let port = free_tcp_port();
    let scene = Scene::new("udp://127.0.0.1:9", "zcu104-test", &format!("127.0.0.1:{port}"), &[], ARC_S, vec![power_cycle_fault("board_reset")]).with_power_control(&channel.uri());
    let err = scene.run().expect_err("only power_cycle is supported");
    assert!(matches!(&err, DrmError::HardwareFaultKindNotSupported { fault_id, instance, kind } if fault_id == "pc1" && instance == CONTROLLER && kind == "board_reset"), "{err:?}");
}

/// A run with a channel declared but no fault never asks the edge service to run anything.
#[test]
fn a_run_without_a_power_cycle_fault_never_runs_the_channel() {
    let _serial = serial();
    let channel = FakeChannel::create("idle");
    let guest = FakeGuest::spawn(GuestPlan::default());
    let device = udp_device(&guest);
    let service = Service::start_with("power-idle", &device, "zcu104-test", &power_arg(&channel));
    let scene = Scene::new(&device, "zcu104-test", &service.grpc_addr, &[], 1, vec![]).with_power_control(&channel.uri());
    let products = scene.run().unwrap_or_else(|e| panic!("the run: {e}\n{}", service.stderr()));
    assert!(channel.calls().is_empty());
    assert!(!products.events.iter().any(is_power_cycle_event));
    assert!(guest.seen.lock().unwrap().resets.is_empty());
    guest.finish();
}

/// The harness refuses to spawn a missing or stale `av-edge-board` binary, naming the fix
/// (`assert_binary_is_fresh`; the real check on the real binary is `edge_board_bin`).
#[test]
fn a_missing_or_stale_edge_board_binary_panics_naming_the_rebuild_command() {
    use std::time::{Duration, SystemTime};
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("drm-board-stale-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (binary, source) = (dir.join("av-edge-board"), dir.join("src.rs"));
    std::fs::write(&binary, b"bin").unwrap();
    std::fs::write(&source, b"src").unwrap();
    let set = |p: &std::path::Path, t: SystemTime| std::fs::File::options().write(true).open(p).unwrap().set_modified(t).unwrap();
    let message = |binary: &std::path::Path, sources: &[std::path::PathBuf]| -> String {
        let (b, s) = (binary.to_path_buf(), sources.to_vec());
        let payload = std::panic::catch_unwind(move || assert_binary_is_fresh(&b, &s)).expect_err("must panic");
        payload.downcast_ref::<String>().cloned().unwrap_or_default()
    };
    let now = SystemTime::now();

    // Missing.
    let m = message(&dir.join("nope"), std::slice::from_ref(&source));
    assert!(m.contains("does not exist") && m.contains(REBUILD_EDGE_BOARD), "{m}");
    // Older than a source.
    set(&binary, now - Duration::from_secs(100));
    set(&source, now);
    let m = message(&binary, std::slice::from_ref(&source));
    assert!(m.contains("older than") && m.contains("src.rs") && m.contains("STALE") && m.contains(REBUILD_EDGE_BOARD), "{m}");
    assert!(m.contains("crates/av-edge-board") && m.contains("crates/av-edge"), "it says what was scanned: {m}");
    println!("STALE MESSAGE: {m}");
    // Fresh passes (binary as new as, or newer than, every source).
    set(&binary, now + Duration::from_secs(5));
    assert_binary_is_fresh(&binary, std::slice::from_ref(&source));
    set(&binary, now);
    assert_binary_is_fresh(&binary, &[source]);
    let _ = std::fs::remove_dir_all(&dir);
}
