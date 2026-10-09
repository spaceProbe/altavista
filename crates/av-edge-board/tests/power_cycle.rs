//! `BoardEdgeService.PowerCycle` through the real `av-edge-board` binary and the kernel's own
//! client (`av_lockstep::board_edge`), against a fake guest on loopback UDP and a recording
//! `cmd:` fake (question 242 (c), hilprep-4). **No board is involved.**
//!
//! - A power cycle runs the configured executable once, as a child of the edge service, with the
//!   documented argv, and is recorded as a `POWER_CYCLE` record in the signed I/O log between the
//!   STEP and RESET records around it.
//! - A request whose declared channel or edge node differs from the service's, or a service with
//!   no channel, is refused and nothing runs (and nothing is logged).
//! - A non-zero exit, a spawn failure and a timeout are typed failures; the timed-out child is
//!   really dead.
//! - A malformed request is `INVALID_ARGUMENT`; `gpio://` and a malformed channel are startup
//!   errors.
mod common;

use std::collections::BTreeMap;
use std::time::Duration;

use av_edge::board_log::BoardIoKind;
use av_lockstep::board_edge::{BlockingBoardEdgeClient, BoardPowerCycleFailure, BoardPowerCycleOutcome, BoardPowerCycleRefusal, PowerCycleRequest};
use av_lockstep::{BlockingLockstepClient, LockstepResetRequest, LockstepShutdownRequest};
use common::*;

fn start(tag: &str, guest: &UdpGuest, extra: &[&str]) -> Service {
    let device = format!("udp://127.0.0.1:{}", guest.addr.port());
    let mut args = vec!["--port-device", device.as_str(), "--edge-node-id", "edge-udp"];
    args.extend_from_slice(extra);
    let mut service = Service::spawn(tag, &args);
    service.wait_ready(Duration::from_secs(20));
    service
}

fn request(channel: &str) -> PowerCycleRequest {
    PowerCycleRequest { run_id: RUN_ID.into(), instance: INSTANCE.into(), fault_id: "pc1".into(), tai_ns: 4 * BASE_PERIOD_NS, edge_node_id: "edge-udp".into(), power_control: channel.into() }
}

fn kinds(service: &Service) -> Vec<BoardIoKind> {
    service.read_io_log().records.iter().map(|r| BoardIoKind::try_from(r.kind).unwrap()).collect()
}

/// A whole session: Bind, steps, power cycle, RESET, steps, Shutdown. The channel runs once with
/// the right argv, as the service's child; the log shows the operation in place.
#[test]
fn a_power_cycle_runs_the_channel_once_as_the_services_child_and_is_logged_in_order() {
    let channel = FakeChannel::create("ok");
    let guest = UdpGuest::spawn(Brain::default());
    let mut service = start("power-ok", &guest, &["--power-control", &channel.uri()]);
    let mut lock = BlockingLockstepClient::connect_plaintext(&service.grpc_addr).unwrap();
    assert!(lock.bind(client_bind_request(&service.bind_params(&BTreeMap::new()))).unwrap().lockstep_capable);
    check_step_reply(1, 300, &lock.step(step_request(1, 300)).unwrap());

    let mut edge = BlockingBoardEdgeClient::connect_plaintext(&service.grpc_addr).expect("BoardEdgeService is served on the lockstep address");
    let resp = edge.power_cycle(request(&channel.uri())).expect("PowerCycle RPC");
    println!("RESPONSE {resp:?}");
    assert_eq!(resp.outcome, BoardPowerCycleOutcome::Performed as i32, "{resp:?}");
    assert_eq!(resp.power_control, channel.uri());
    assert!(resp.duration_ns > 0 && resp.started_unix_ns > 0 && resp.finished_unix_ns >= resp.started_unix_ns);
    assert!(resp.detail.is_empty() && resp.stderr_tail.is_empty());

    let calls = channel.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(FakeChannel::argv(&calls[0]), ["power-cycle", "--edge-node-id", "edge-udp", "--instance", INSTANCE, "--fault-id", "pc1", "--tai-ns", &(4 * BASE_PERIOD_NS).to_string()]);
    assert_eq!(calls[0]["ppid"].as_u64().unwrap() as u32, service.child.id(), "the channel is the edge service's child");
    assert_eq!(calls[0]["cwd"].as_str(), Some("/"));

    lock.reset(LockstepResetRequest { sequence: 2, tai_ns: 4 * BASE_PERIOD_NS, reason: "fault:pc1".into() }).unwrap();
    check_step_reply(3, 40, &lock.step(step_request(3, 40)).unwrap());
    lock.shutdown(LockstepShutdownRequest { run_id: RUN_ID.into() }).unwrap();
    service.wait_exit(Duration::from_secs(15));

    assert_eq!(kinds(&service), [BoardIoKind::Bind, BoardIoKind::Step, BoardIoKind::PowerCycle, BoardIoKind::Reset, BoardIoKind::Step, BoardIoKind::Shutdown]);
    let log = service.read_io_log();
    let pc = &log.records[2];
    assert_eq!((pc.run_id.as_str(), pc.instance.as_str(), pc.reset_tai_ns, pc.reset_reason.as_str(), pc.error.as_str()), (RUN_ID, INSTANCE, 4 * BASE_PERIOD_NS, "fault:pc1", ""));
    assert_eq!((pc.request_written_unix_ns, pc.response_read_unix_ns), (resp.started_unix_ns, resp.finished_unix_ns), "the record carries the channel's own instants");
    assert!(pc.inputs.is_empty() && pc.outputs.is_empty() && pc.bind_request.is_none());
    println!("LOG kinds {:?}", kinds(&service));
    service.save_evidence("power_ok.stderr.txt");
    guest.finish();
}

#[test]
fn a_request_the_service_cannot_honour_is_refused_and_nothing_runs_or_is_logged() {
    let channel = FakeChannel::create("refuse");
    let other = FakeChannel::create("refuse-other");
    let guest = UdpGuest::spawn(Brain::default());
    let service = start("power-refuse", &guest, &["--power-control", &channel.uri()]);
    let mut edge = BlockingBoardEdgeClient::connect_plaintext(&service.grpc_addr).unwrap();

    let mut wrong_node = request(&channel.uri());
    wrong_node.edge_node_id = "edge-other".into();
    let cases: Vec<(&str, PowerCycleRequest, BoardPowerCycleRefusal, Vec<String>)> = vec![
        ("different channel", request(&other.uri()), BoardPowerCycleRefusal::ChannelMismatch, vec![other.uri(), channel.uri()]),
        ("no channel declared", request(""), BoardPowerCycleRefusal::ChannelMismatch, vec![channel.uri()]),
        ("reserved gpio", request("gpio://17"), BoardPowerCycleRefusal::ChannelMismatch, vec!["gpio://17".into()]),
        ("different edge node", wrong_node, BoardPowerCycleRefusal::EdgeNodeMismatch, vec!["edge-other".into(), "edge-udp".into()]),
    ];
    for (name, req, refusal, must_name) in cases {
        let resp = edge.power_cycle(req).unwrap_or_else(|e| panic!("{name}: a refusal is a response, not an RPC error: {e}"));
        assert_eq!((resp.outcome, resp.refusal), (BoardPowerCycleOutcome::Refused as i32, refusal as i32), "{name}: {resp:?}");
        for needle in must_name {
            assert!(resp.detail.contains(&needle), "{name}: the reason names {needle:?}: {}", resp.detail);
        }
        assert_eq!((resp.duration_ns, resp.started_unix_ns), (0, 0), "{name}: nothing was started");
        println!("REFUSED[{name}] {}", resp.detail);
    }
    assert!(channel.calls().is_empty() && other.calls().is_empty(), "no executable ran");
    assert!(!service.io_log.exists() || kinds(&service).is_empty(), "a refusal is not logged: {:?}", kinds(&service));

    // A request that is malformed is an invalid argument, not a refusal.
    for mutate in [(|r: &mut PowerCycleRequest| r.fault_id.clear()) as fn(&mut PowerCycleRequest), |r| r.instance.clear(), |r| r.instance = "a\0b".into()] {
        let mut r = request(&channel.uri());
        mutate(&mut r);
        let status = edge.power_cycle(r).expect_err("malformed");
        assert_eq!(status.code(), tonic::Code::InvalidArgument, "{status}");
    }
    assert!(channel.calls().is_empty());
    guest.finish();
}

#[test]
fn a_service_with_no_channel_refuses_every_request() {
    let channel = FakeChannel::create("nochan");
    let guest = UdpGuest::spawn(Brain::default());
    let service = start("power-nochan", &guest, &[]);
    assert!(service.stderr().contains("no power control channel"), "{}", service.stderr());
    let mut edge = BlockingBoardEdgeClient::connect_plaintext(&service.grpc_addr).unwrap();
    for declared in ["", &channel.uri()] {
        let resp = edge.power_cycle(request(declared)).unwrap();
        assert_eq!((resp.outcome, resp.refusal), (BoardPowerCycleOutcome::Refused as i32, BoardPowerCycleRefusal::NoChannel as i32), "{declared:?}: {resp:?}");
        assert!(resp.detail.contains("no power-control channel"), "{}", resp.detail);
        assert_eq!(resp.power_control, "");
    }
    assert!(channel.calls().is_empty());
    guest.finish();
}

#[test]
fn a_non_zero_exit_is_a_typed_failure_with_its_stderr_and_is_logged_with_its_error() {
    let channel = FakeChannel::create("fail");
    channel.set_mode("fail");
    let guest = UdpGuest::spawn(Brain::default());
    let service = start("power-fail", &guest, &["--power-control", &channel.uri()]);
    let mut edge = BlockingBoardEdgeClient::connect_plaintext(&service.grpc_addr).unwrap();
    let resp = edge.power_cycle(request(&channel.uri())).unwrap();
    println!("FAILED {resp:?}");
    assert_eq!((resp.outcome, resp.failure, resp.exit_status, resp.signal), (BoardPowerCycleOutcome::Failed as i32, BoardPowerCycleFailure::ExitStatus as i32, 3, 0));
    assert!(resp.detail.contains("status 3") && resp.stderr_tail == "relay stuck\n", "{resp:?}");
    assert!(resp.duration_ns > 0);
    assert_eq!(channel.calls().len(), 1);
    let log = service.read_io_log();
    assert_eq!(log.records.len(), 1);
    assert!(log.records[0].error.contains("ExitStatus") && log.records[0].error.contains("status 3"), "{:?}", log.records[0].error);
    guest.finish();
}

#[test]
fn a_missing_executable_is_a_spawn_failure() {
    let guest = UdpGuest::spawn(Brain::default());
    let service = start("power-nospawn", &guest, &["--power-control", "cmd:/no/such/power-cycle"]);
    let mut edge = BlockingBoardEdgeClient::connect_plaintext(&service.grpc_addr).unwrap();
    let resp = edge.power_cycle(request("cmd:/no/such/power-cycle")).unwrap();
    assert_eq!((resp.outcome, resp.failure), (BoardPowerCycleOutcome::Failed as i32, BoardPowerCycleFailure::Spawn as i32), "{resp:?}");
    assert!(resp.detail.contains("/no/such/power-cycle"), "{}", resp.detail);
    guest.finish();
}

#[test]
fn a_channel_that_outlives_the_timeout_is_killed_and_reported() {
    let channel = FakeChannel::create("hang");
    channel.set_mode("sleep");
    let guest = UdpGuest::spawn(Brain::default());
    let service = start("power-hang", &guest, &["--power-control", &channel.uri(), "--power-timeout-ms", "600"]);
    let mut edge = BlockingBoardEdgeClient::connect_plaintext(&service.grpc_addr).unwrap();
    let t0 = std::time::Instant::now();
    let resp = edge.power_cycle(request(&channel.uri())).unwrap();
    let wall = t0.elapsed();
    assert_eq!((resp.outcome, resp.failure), (BoardPowerCycleOutcome::Failed as i32, BoardPowerCycleFailure::Timeout as i32), "{resp:?}");
    assert!(resp.detail.contains("600 ms"), "{}", resp.detail);
    assert!(wall >= Duration::from_millis(600) && wall < Duration::from_secs(8), "{wall:?}");
    let pid: i32 = std::fs::read_to_string(channel.dir.join("sleep.pid")).unwrap().trim().parse().unwrap();
    assert!(!FakeChannel::is_alive(pid), "the timed-out child (pid {pid}) is really dead");
    println!("TIMEOUT after {wall:?}, pid {pid} dead");
    guest.finish();
}

#[test]
fn a_reserved_or_malformed_channel_or_timeout_is_a_startup_error() {
    for (args, needle) in [
        (vec!["--power-control", "gpio://17"], "reserved"),
        (vec!["--power-control", "cmd:relative"], "relative"),
        (vec!["--power-control", "http://x"], "unknown scheme"),
        (vec!["--power-timeout-ms", "5"], "outside 10..=600000"),
        (vec!["--power-timeout-ms", "soon"], "not a number"),
    ] {
        let guest = UdpGuest::spawn(Brain::default());
        let device = format!("udp://127.0.0.1:{}", guest.addr.port());
        let mut base = vec!["--port-device", device.as_str(), "--edge-node-id", "e"];
        base.extend(args.iter().copied());
        let mut s = Service::spawn("power-startup", &base);
        let status = s.wait_exit(Duration::from_secs(10));
        assert!(!status.success(), "{args:?}");
        assert!(s.stderr().contains(needle), "{args:?}: {}", s.stderr());
        assert!(guest.service_addr.lock().unwrap().is_none(), "{args:?}: the board was never touched");
        guest.finish();
    }
}
