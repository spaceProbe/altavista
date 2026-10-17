//! Re-handshaking with a peer that rebooted (question 245 (6)), through the real `av-edge-board`
//! binary, the kernel's own gRPC clients (`av_lockstep`), a recording `cmd:` power channel and a
//! fake guest on loopback UDP that **really loses its session**. **No board is involved**, and
//! nothing here is a container: the fake guest's "reboot" is a flag file the power channel
//! writes. The real cFS container behind the real service is `tests/cfs_container_restart.rs`.
//!
//! The fake guest is stricter than `common::UdpGuest`: it has the lockstep-local v1 session the
//! real guest (`io_lockstep_app.c`) has. After a reboot it is **down** (a datagram that arrives is
//! dropped and counted, as the cFS relay drops datagrams while no guest is connected), then it
//! waits for HELLO, answers once, and then expects BIND; a second HELLO where BIND is expected,
//! or a STEP/RESET before BIND, is recorded as a violation (the real guest would exit).
//!
//! - A declared reboot (`--power-cycle-reboots-peer`): the power cycle is followed by a
//!   re-handshake (HELLO re-sent at the interval until the guest, down for 700 ms, answers), the
//!   response says so, the kernel's second BIND and RESET and later STEPs are answered, the guest
//!   saw no protocol violation, and the signed log holds the one chain
//!   `BIND, STEP.., POWER_CYCLE, LINK_REHANDSHAKE, BIND, RESET, STEP.., SHUTDOWN`.
//! - A guest that never returns: the power cycle is a typed failure naming the deadline, within a
//!   bounded wall time; the link is then lost (every exchange is `UNAVAILABLE` and nothing more
//!   reaches the guest), the failure is in the log, and a Shutdown still ends the process.
//! - A guest that answers with another protocol version is refused at once, not retried.
//! - Noise (a frame that is not a HELLO) in place of an answer is not an answer: HELLO is re-sent.
//! - Without the declaration nothing changes: no HELLO is re-sent, no record is added.
//! - The new flags' usage errors.
mod common;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use av_edge::board_log::BoardIoKind;
use av_lockstep::board_edge::{BlockingBoardEdgeClient, BoardPowerCycleFailure, BoardPowerCycleOutcome, PowerCycleRequest};
use av_lockstep::{BlockingLockstepClient, LockstepResetRequest, LockstepShutdownRequest};
use av_lockstep_shim::framing::{encode_frame, encode_hello, FrameType};
use common::*;

// ---------------------------------------------------------------------------------------
// A guest that reboots
// ---------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Phase {
    Down,
    #[default]
    AwaitHello,
    AwaitBind,
    Running,
}

#[derive(Default)]
struct GuestLog {
    phase: Phase,
    boots: usize,
    /// Datagrams that arrived while the guest was down (dropped, as a relay with no guest does).
    dropped_while_down: usize,
    hellos_answered: usize,
    binds: usize,
    steps: Vec<u64>,
    resets: usize,
    errors_received: usize,
    shutdown_seen: bool,
    /// Frames the real guest would have failed on.
    violations: Vec<String>,
    /// Total datagrams received, whatever the phase.
    datagrams: usize,
    // The guest's own machinery (private to the fake).
    brain: Brain,
    up_at: Option<Instant>,
    pending_noise: bool,
    /// Where the last datagram came from (the service).
    service: Option<std::net::SocketAddr>,
}

/// Act on a reboot flag the power channel wrote, and bring the guest up once its down time is over.
fn refresh(g: &mut GuestLog, flag: &std::path::Path, opts: &Mutex<GuestOptions>, socket: &std::net::UdpSocket) {
    // The channel writes the flag atomically (a temporary file, then `mv`), and an empty read is
    // still ignored: a half-written flag must never be read as a delay.
    if let Some(text) = std::fs::read_to_string(flag).ok().filter(|t| !t.trim().is_empty()) {
        let _ = std::fs::remove_file(flag);
        g.boots += 1;
        g.phase = Phase::Down;
        g.up_at = match text.trim() {
            "never" => None,
            ms => Some(Instant::now() + Duration::from_millis(ms.parse().expect("the channel wrote a delay in ms or `never`"))),
        };
        g.brain = Brain::default();
        g.pending_noise = opts.lock().unwrap().noise_before_first_answer;
        if opts.lock().unwrap().stale_frame_at_boot {
            if let Some(service) = g.service {
                socket.send_to(&encode_frame(FrameType::StepDone, &[9, 9, 9]), service).unwrap();
            }
        }
    }
    if g.phase == Phase::Down && g.up_at.is_some_and(|t| Instant::now() >= t) {
        g.phase = Phase::AwaitHello;
    }
}

#[derive(Clone, Copy)]
struct GuestOptions {
    /// The version the guest's HELLO carries.
    hello_version: u16,
    /// After each boot, answer the first HELLO with a stale STEP_DONE frame instead of a HELLO.
    noise_before_first_answer: bool,
    /// At each boot, send the service a late STEP_DONE frame of the session that just died.
    stale_frame_at_boot: bool,
}

impl Default for GuestOptions {
    fn default() -> Self {
        Self { hello_version: 1, noise_before_first_answer: false, stale_frame_at_boot: false }
    }
}

struct RebootingGuest {
    log: Arc<Mutex<GuestLog>>,
    options: Arc<Mutex<GuestOptions>>,
    addr: std::net::SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl RebootingGuest {
    /// `flag` is the file the power channel writes: its content is the milliseconds the guest stays
    /// down, or `never`.
    fn spawn(flag: PathBuf, options: GuestOptions) -> Self {
        let options = Arc::new(Mutex::new(options));
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.set_read_timeout(Some(Duration::from_millis(10))).unwrap();
        let addr = socket.local_addr().unwrap();
        let log = Arc::new(Mutex::new(GuestLog::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (l, st, opts) = (log.clone(), stop.clone(), options.clone());
        let thread = std::thread::spawn(move || {
            let mut buf = vec![0u8; 65_536];
            while !st.load(Ordering::Relaxed) {
                refresh(&mut l.lock().unwrap(), &flag, &opts, &socket);
                let (n, src) = match socket.recv_from(&mut buf) {
                    Ok(x) => x,
                    Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => continue,
                    Err(e) => panic!("rebooting guest recv: {e}"),
                };
                let mut g = l.lock().unwrap();
                // The reboot flag is written before the power channel exits, which is before the
                // service sends its next HELLO: look at it again now, so a datagram that arrived while
                // this thread was blocked in `recv_from` is met by the guest as it is after the reboot.
                refresh(&mut g, &flag, &opts, &socket);
                g.service = Some(src);
                g.datagrams += 1;
                if g.phase == Phase::Down {
                    g.dropped_while_down += 1;
                    continue;
                }
                let mut dg = buf[..n].to_vec();
                let Some((ty, payload)) = take_frame(&mut dg) else {
                    g.violations.push(format!("a datagram of {n} bytes that is not a frame"));
                    continue;
                };
                if !dg.is_empty() {
                    g.violations.push("bytes after a frame in one datagram".to_string());
                }
                match (g.phase, ty) {
                    (_, FrameType::Error) => g.errors_received += 1,
                    (Phase::AwaitHello, FrameType::Hello) => {
                        if g.pending_noise {
                            g.pending_noise = false;
                            // A late frame of the dead session where the HELLO answer should be.
                            socket.send_to(&encode_frame(FrameType::StepDone, &[1, 2, 3]), src).unwrap();
                        } else {
                            let version = opts.lock().unwrap().hello_version;
                            socket.send_to(&encode_frame(FrameType::Hello, &encode_hello(version)), src).unwrap();
                            g.hellos_answered += 1;
                            if version == 1 {
                                g.phase = Phase::AwaitBind;
                            }
                        }
                    }
                    (Phase::AwaitBind, FrameType::Bind) => {
                        g.binds += 1;
                        let (replies, _) = g.brain.on_frame(ty, &payload);
                        for r in replies {
                            socket.send_to(&r, src).unwrap();
                        }
                        g.phase = Phase::Running;
                    }
                    (Phase::Running, FrameType::Step | FrameType::Reset | FrameType::Shutdown) => {
                        let (replies, done) = g.brain.on_frame(ty, &payload);
                        match ty {
                            FrameType::Step => {
                                let last = *g.brain.step_sequences.last().unwrap();
                                g.steps.push(last)
                            }
                            FrameType::Reset => g.resets += 1,
                            _ => g.shutdown_seen = done,
                        }
                        for r in replies {
                            socket.send_to(&r, src).unwrap();
                        }
                    }
                    (phase, ty) => g.violations.push(format!("{ty} in phase {phase:?}")),
                }
            }
        });
        Self { log, options, addr, stop, thread: Some(thread) }
    }

    /// Change how the guest behaves from its next HELLO / reboot on.
    fn set_options(&self, options: GuestOptions) {
        *self.options.lock().unwrap() = options;
    }

    fn snapshot<R>(&self, f: impl FnOnce(&GuestLog) -> R) -> R {
        f(&self.log.lock().unwrap())
    }

    fn finish(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            t.join().unwrap();
        }
    }
}

impl Drop for RebootingGuest {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------------------------------
// A power channel that "reboots" the guest
// ---------------------------------------------------------------------------------------

/// A `cmd:` executable that appends its argv to `calls.txt` and, unless its `mode` file says
/// `none`, writes the `mode` content (milliseconds, or `never`; default 0) to `reboot.flag` beside
/// itself: the signal the fake guest reboots on.
struct RebootChannel {
    dir: PathBuf,
}

impl RebootChannel {
    fn create(tag: &str, mode: &str) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("av-edge-board-reboot-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("power-cycle");
        std::fs::write(
            &path,
            "#!/bin/sh\n\
             d=$(dirname \"$0\")\n\
             echo \"$@\" >> \"$d/calls.txt\"\n\
             mode=0\n\
             [ -f \"$d/mode\" ] && mode=$(cat \"$d/mode\")\n\
             [ \"$mode\" = none ] && exit 0\n\
             echo \"$mode\" > \"$d/reboot.flag.tmp\" && mv \"$d/reboot.flag.tmp\" \"$d/reboot.flag\"\n\
             [ -f \"$d/slow\" ] && sleep 0.4\n\
             exit 0\n",
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(dir.join("mode"), mode).unwrap();
        Self { dir }
    }

    fn uri(&self) -> String {
        format!("cmd:{}", self.dir.join("power-cycle").display())
    }

    fn flag(&self) -> PathBuf {
        self.dir.join("reboot.flag")
    }

    fn calls(&self) -> usize {
        std::fs::read_to_string(self.dir.join("calls.txt")).map(|t| t.lines().count()).unwrap_or(0)
    }
}

impl Drop for RebootChannel {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

// ---------------------------------------------------------------------------------------
// Driving the service
// ---------------------------------------------------------------------------------------

fn start(tag: &str, guest: &RebootingGuest, channel: &RebootChannel, extra: &[&str]) -> Service {
    let device = format!("udp://127.0.0.1:{}", guest.addr.port());
    let uri = channel.uri();
    let mut args = vec!["--port-device", device.as_str(), "--edge-node-id", "edge-udp", "--power-control", uri.as_str()];
    args.extend_from_slice(extra);
    let mut service = Service::spawn(tag, &args);
    service.wait_ready(Duration::from_secs(20));
    service
}

fn power_cycle_request(channel: &RebootChannel) -> PowerCycleRequest {
    PowerCycleRequest { run_id: RUN_ID.into(), instance: INSTANCE.into(), fault_id: "pc1".into(), tai_ns: 3 * BASE_PERIOD_NS, edge_node_id: "edge-udp".into(), power_control: channel.uri() }
}

fn kinds(service: &Service) -> Vec<BoardIoKind> {
    service.read_io_log().records.iter().map(|r| BoardIoKind::try_from(r.kind).unwrap()).collect()
}

const REBOOT_FLAGS: [&str; 5] = ["--power-cycle-reboots-peer", "--rehandshake-interval-ms", "200", "--rehandshake-timeout-ms", "6000"];

/// The headline: BIND, three STEPs, a power cycle that reboots the guest for 700 ms, the
/// re-handshake, the kernel's second BIND, RESET, three more STEPs, SHUTDOWN.
#[test]
fn a_rebooted_guest_is_rehandshaken_rebound_and_the_run_continues() {
    let channel = RebootChannel::create("ok", "700");
    let guest = RebootingGuest::spawn(channel.flag(), GuestOptions::default());
    let mut service = start("rehandshake-ok", &guest, &channel, &REBOOT_FLAGS);
    let params = service.bind_params(&BTreeMap::new());
    let mut lock = BlockingLockstepClient::connect_plaintext(&service.grpc_addr).unwrap();
    assert!(lock.bind(client_bind_request(&params)).unwrap().lockstep_capable);
    for seq in 1..=3u64 {
        check_step_reply(seq, 100 + seq as usize, &lock.step(step_request(seq, 100 + seq as usize)).unwrap());
    }
    assert_eq!(guest.snapshot(|g| (g.hellos_answered, g.binds, g.steps.clone())), (1, 1, vec![1, 2, 3]));

    let mut edge = BlockingBoardEdgeClient::connect_plaintext(&service.grpc_addr).unwrap();
    let t0 = Instant::now();
    let resp = edge.power_cycle(power_cycle_request(&channel)).expect("PowerCycle RPC");
    let wall = t0.elapsed();
    println!("RESPONSE after {wall:?}: {resp:?}");
    assert_eq!((resp.outcome, resp.failure), (BoardPowerCycleOutcome::Performed as i32, BoardPowerCycleFailure::Unspecified as i32), "{resp:?}");
    assert!(resp.peer_rehandshaken, "{resp:?}");
    assert_eq!(resp.rehandshake_timeout_ms, 6000);
    // Down 700 ms, HELLO every 200 ms starting at once: the HELLOs at 0, 200, 400 and 600 ms (at
    // least three of them) were lost; the one that was answered came after the guest was up.
    assert!((4..=6).contains(&resp.rehandshake_hellos_sent), "{} HELLOs", resp.rehandshake_hellos_sent);
    assert!(resp.rehandshake_duration_ns >= 700_000_000 && resp.rehandshake_duration_ns < 2_000_000_000, "{} ns", resp.rehandshake_duration_ns);
    assert_eq!(channel.calls(), 1);
    let (boots, dropped, answered, violations) = guest.snapshot(|g| (g.boots, g.dropped_while_down, g.hellos_answered, g.violations.clone()));
    assert_eq!(boots, 1);
    assert!(dropped >= 3, "HELLOs sent while the guest was down are lost, not queued: {dropped}");
    assert_eq!(answered, 2, "the initial HELLO and exactly one after the reboot");
    assert!(violations.is_empty(), "the guest saw: {violations:?}");

    // The kernel's side: the same Bind request again, then RESET, then STEPs.
    let bind = lock.bind(client_bind_request(&params)).unwrap();
    assert!(bind.lockstep_capable, "{bind:?}");
    lock.reset(LockstepResetRequest { sequence: 4, tai_ns: 3 * BASE_PERIOD_NS, reason: "fault:pc1".into() }).unwrap();
    for seq in 5..=7u64 {
        check_step_reply(seq, 50 + seq as usize, &lock.step(step_request(seq, 50 + seq as usize)).unwrap());
    }
    lock.shutdown(LockstepShutdownRequest { run_id: RUN_ID.into() }).unwrap();
    service.wait_exit(Duration::from_secs(15));
    let (binds, steps, resets, violations, shutdown) = guest.snapshot(|g| (g.binds, g.steps.clone(), g.resets, g.violations.clone(), g.shutdown_seen));
    // (the fake's counters span both sessions: BIND twice, STEPs 1-3 before the reboot, 5-7 after)
    assert_eq!((binds, steps, resets, shutdown), (2, vec![1, 2, 3, 5, 6, 7], 1, true), "the rebooted guest's own session");
    assert!(violations.is_empty(), "{violations:?}");

    // One chain, in order.
    use BoardIoKind::*;
    assert_eq!(kinds(&service), [Bind, Step, Step, Step, PowerCycle, LinkRehandshake, Bind, Reset, Step, Step, Step, Shutdown]);
    let log = service.read_io_log();
    let (bind1, bind2, rehandshake) = (&log.records[0], &log.records[6], &log.records[5]);
    assert_eq!(bind1.bind_request, bind2.bind_request, "the same Bind request, board parameters already stripped");
    assert!(bind2.bind_response.as_ref().unwrap().lockstep_capable && bind2.error.is_empty());
    assert_eq!((rehandshake.run_id.as_str(), rehandshake.instance.as_str(), rehandshake.reset_tai_ns, rehandshake.reset_reason.as_str(), rehandshake.error.as_str()), (RUN_ID, INSTANCE, 3 * BASE_PERIOD_NS, "fault:pc1", ""));
    assert_eq!((rehandshake.rehandshake_hellos_sent, rehandshake.rehandshake_timeout_ms), (resp.rehandshake_hellos_sent, 6000));
    assert!(rehandshake.request_written_unix_ns > 0 && rehandshake.response_read_unix_ns - rehandshake.request_written_unix_ns >= 700_000_000, "{rehandshake:?}");
    assert!(rehandshake.inputs.is_empty() && rehandshake.outputs.is_empty() && rehandshake.bind_request.is_none());

    // The reader tool understands the new record.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_av-edge-board-log")).arg(&service.io_log).arg("--cert").arg(&service.identity.cert_pem).arg("--records").output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    println!("{text}");
    assert!(out.status.success(), "{text}");
    assert!(text.contains("verification: OK\n") && text.contains("link_rehandshakes: 1\n") && text.contains("binds: 2\n") && text.contains("kind=LINK_REHANDSHAKE"), "{text}");
    service.save_evidence("rehandshake_ok.stderr.txt");
    guest.finish();
}

/// A late frame of the dead session that is already queued when the re-handshake starts is
/// discarded before the first HELLO, so it is never mistaken for the peer's answer: the guest is
/// sent no ERROR frame and the first HELLO is the one answered.
#[test]
fn a_late_frame_of_the_dead_session_is_discarded_before_the_first_hello() {
    let channel = RebootChannel::create("stale", "0");
    // The channel takes 400 ms to return after signalling the reboot, so the guest's stale frame
    // is long queued at the service's socket when the service starts the re-handshake.
    std::fs::write(channel.dir.join("slow"), "").unwrap();
    let guest = RebootingGuest::spawn(channel.flag(), GuestOptions::default());
    let mut service = start("rehandshake-stale", &guest, &channel, &["--power-cycle-reboots-peer", "--rehandshake-interval-ms", "300", "--rehandshake-timeout-ms", "5000"]);
    let params = service.bind_params(&BTreeMap::new());
    let mut lock = BlockingLockstepClient::connect_plaintext(&service.grpc_addr).unwrap();
    assert!(lock.bind(client_bind_request(&params)).unwrap().lockstep_capable);
    guest.set_options(GuestOptions { stale_frame_at_boot: true, ..Default::default() });
    let mut edge = BlockingBoardEdgeClient::connect_plaintext(&service.grpc_addr).unwrap();
    let resp = edge.power_cycle(power_cycle_request(&channel)).unwrap();
    println!("RESPONSE {resp:?}");
    assert!(resp.peer_rehandshaken && resp.rehandshake_hellos_sent == 1, "{resp:?}");
    assert_eq!(guest.snapshot(|g| (g.errors_received, g.hellos_answered, g.violations.len())), (0, 2, 0), "no ERROR frame: the stale frame was discarded, not read as an answer");
    assert!(service.stderr().contains("discarded 1 queued item(s) first"), "{}", service.stderr());
    assert!(lock.bind(client_bind_request(&params)).unwrap().lockstep_capable);
    check_step_reply(1, 33, &lock.step(step_request(1, 33)).unwrap());
    let _ = service.child.kill();
    guest.finish();
}

/// The guest never returns: a typed failure naming the deadline, bounded wall time, the link lost,
/// nothing more written, the failure logged, and a Shutdown still ends the process.
#[test]
fn a_guest_that_never_returns_fails_the_power_cycle_at_the_deadline_and_the_link_stays_lost() {
    let channel = RebootChannel::create("never", "never");
    let guest = RebootingGuest::spawn(channel.flag(), GuestOptions::default());
    let mut service = start("rehandshake-never", &guest, &channel, &["--power-cycle-reboots-peer", "--rehandshake-interval-ms", "300", "--rehandshake-timeout-ms", "1500"]);
    let params = service.bind_params(&BTreeMap::new());
    let mut lock = BlockingLockstepClient::connect_plaintext(&service.grpc_addr).unwrap();
    assert!(lock.bind(client_bind_request(&params)).unwrap().lockstep_capable);
    check_step_reply(1, 64, &lock.step(step_request(1, 64)).unwrap());

    let mut edge = BlockingBoardEdgeClient::connect_plaintext(&service.grpc_addr).unwrap();
    let t0 = Instant::now();
    let resp = edge.power_cycle(power_cycle_request(&channel)).expect("PowerCycle RPC");
    let wall = t0.elapsed();
    println!("RESPONSE after {wall:?}: {resp:?}");
    assert_eq!((resp.outcome, resp.failure), (BoardPowerCycleOutcome::Failed as i32, BoardPowerCycleFailure::RehandshakeTimeout as i32), "{resp:?}");
    assert!(!resp.peer_rehandshaken);
    assert!(resp.detail.contains("1500 ms") && resp.detail.contains("HELLO"), "{}", resp.detail);
    assert_eq!(resp.rehandshake_timeout_ms, 1500);
    assert!((4..=6).contains(&resp.rehandshake_hellos_sent), "{} HELLOs at 0, 300, 600, 900, 1200 ms", resp.rehandshake_hellos_sent);
    assert!(wall >= Duration::from_millis(1500) && wall < Duration::from_secs(6), "bounded wall time: {wall:?}");

    // The link is lost: each exchange is refused before anything is written to the board.
    let datagrams_before = guest.snapshot(|g| g.datagrams);
    let status = lock.step(step_request(2, 64)).expect_err("a lost link refuses STEP");
    assert!(status.to_string().contains("lost") && status.to_string().contains("1500 ms"), "{status}");
    assert!(lock.bind(client_bind_request(&params)).is_err());
    assert!(lock.reset(LockstepResetRequest { sequence: 3, tai_ns: 3 * BASE_PERIOD_NS, reason: "fault:pc1".into() }).is_err());
    assert_eq!(guest.snapshot(|g| g.datagrams), datagrams_before, "nothing more was written to the board");
    assert!(guest.snapshot(|g| g.violations.is_empty()));

    // Shutdown has nobody to tell, is logged, and ends the process.
    assert!(lock.shutdown(LockstepShutdownRequest { run_id: RUN_ID.into() }).is_err());
    service.wait_exit(Duration::from_secs(15));
    use BoardIoKind::*;
    assert_eq!(kinds(&service), [Bind, Step, PowerCycle, LinkRehandshake, Shutdown]);
    let log = service.read_io_log();
    let rehandshake = &log.records[3];
    assert!(rehandshake.error.contains("no peer answered") && rehandshake.error.contains("1500 ms"), "{:?}", rehandshake.error);
    assert_eq!(rehandshake.rehandshake_hellos_sent, resp.rehandshake_hellos_sent);
    assert!(log.records[4].error.contains("lost"), "{:?}", log.records[4].error);
    service.save_evidence("rehandshake_never.stderr.txt");
    guest.finish();
}

/// A peer that answers with another protocol version is refused at once, with the typed failure.
#[test]
fn a_peer_with_another_protocol_version_is_refused_at_once() {
    let channel = RebootChannel::create("badversion", "0");
    let guest = RebootingGuest::spawn(channel.flag(), GuestOptions::default());
    let mut service = start("rehandshake-version", &guest, &channel, &["--power-cycle-reboots-peer", "--rehandshake-interval-ms", "1000", "--rehandshake-timeout-ms", "5000"]);
    let mut lock = BlockingLockstepClient::connect_plaintext(&service.grpc_addr).unwrap();
    assert!(lock.bind(client_bind_request(&service.bind_params(&BTreeMap::new()))).unwrap().lockstep_capable);
    // The guest reboots into firmware that speaks version 2.
    guest.set_options(GuestOptions { hello_version: 2, ..Default::default() });
    let mut edge = BlockingBoardEdgeClient::connect_plaintext(&service.grpc_addr).unwrap();
    let t0 = Instant::now();
    let resp = edge.power_cycle(power_cycle_request(&channel)).unwrap();
    let wall = t0.elapsed();
    println!("RESPONSE after {wall:?}: {resp:?}");
    assert_eq!((resp.outcome, resp.failure), (BoardPowerCycleOutcome::Failed as i32, BoardPowerCycleFailure::RehandshakeRefused as i32), "{resp:?}");
    assert!(resp.detail.contains("version") && !resp.peer_rehandshaken, "{}", resp.detail);
    assert_eq!(resp.rehandshake_hellos_sent, 1, "refused at the first answer, not retried");
    assert!(wall < Duration::from_millis(900), "refused at once, well before the 1000 ms interval: {wall:?}");
    assert_eq!(guest.snapshot(|g| g.errors_received), 1, "the guest was told why with an ERROR frame");
    let _ = service.child.kill();
    guest.finish();
}

/// A late frame of the dead session where the HELLO answer should be is not an answer.
#[test]
fn noise_in_place_of_an_answer_is_not_an_answer_and_hello_is_resent() {
    let channel = RebootChannel::create("noise", "0");
    let guest = RebootingGuest::spawn(channel.flag(), GuestOptions::default());
    let mut service = start("rehandshake-noise", &guest, &channel, &["--power-cycle-reboots-peer", "--rehandshake-interval-ms", "300", "--rehandshake-timeout-ms", "5000"]);
    let params = service.bind_params(&BTreeMap::new());
    let mut lock = BlockingLockstepClient::connect_plaintext(&service.grpc_addr).unwrap();
    assert!(lock.bind(client_bind_request(&params)).unwrap().lockstep_capable);
    // The startup handshake is done (HELLO once, answered). From the next reboot on, the guest
    // answers the first HELLO with a late frame of the dead session instead.
    guest.set_options(GuestOptions { noise_before_first_answer: true, ..Default::default() });
    let mut edge = BlockingBoardEdgeClient::connect_plaintext(&service.grpc_addr).unwrap();
    let resp = edge.power_cycle(power_cycle_request(&channel)).unwrap();
    println!("RESPONSE {resp:?}");
    assert!(resp.peer_rehandshaken && resp.rehandshake_hellos_sent == 2, "{resp:?}");
    assert_eq!(guest.snapshot(|g| (g.errors_received, g.hellos_answered)), (1, 2), "the unexpected frame was met with an ERROR frame and the next HELLO was answered");
    assert!(lock.bind(client_bind_request(&params)).unwrap().lockstep_capable);
    check_step_reply(1, 33, &lock.step(step_request(1, 33)).unwrap());
    let _ = service.child.kill();
    guest.finish();
}

/// Without the declaration nothing changes: no HELLO is re-sent, no record is added.
#[test]
fn without_the_declaration_a_power_cycle_re_sends_nothing() {
    let channel = RebootChannel::create("undeclared", "none");
    let guest = RebootingGuest::spawn(channel.flag(), GuestOptions::default());
    let mut service = start("rehandshake-undeclared", &guest, &channel, &[]);
    let params = service.bind_params(&BTreeMap::new());
    let mut lock = BlockingLockstepClient::connect_plaintext(&service.grpc_addr).unwrap();
    assert!(lock.bind(client_bind_request(&params)).unwrap().lockstep_capable);
    check_step_reply(1, 64, &lock.step(step_request(1, 64)).unwrap());
    let mut edge = BlockingBoardEdgeClient::connect_plaintext(&service.grpc_addr).unwrap();
    let resp = edge.power_cycle(power_cycle_request(&channel)).unwrap();
    assert_eq!(resp.outcome, BoardPowerCycleOutcome::Performed as i32);
    assert!(!resp.peer_rehandshaken && resp.rehandshake_hellos_sent == 0 && resp.rehandshake_duration_ns == 0 && resp.rehandshake_timeout_ms == 0, "{resp:?}");
    lock.reset(LockstepResetRequest { sequence: 2, tai_ns: 3 * BASE_PERIOD_NS, reason: "fault:pc1".into() }).unwrap();
    check_step_reply(3, 64, &lock.step(step_request(3, 64)).unwrap());
    lock.shutdown(LockstepShutdownRequest { run_id: RUN_ID.into() }).unwrap();
    service.wait_exit(Duration::from_secs(15));
    use BoardIoKind::*;
    assert_eq!(kinds(&service), [Bind, Step, PowerCycle, Reset, Step, Shutdown]);
    assert_eq!(guest.snapshot(|g| (g.hellos_answered, g.datagrams, g.violations.len())), (1, 1 + 1 + 1 + 1 + 1 + 1, 0), "HELLO, BIND, STEP, RESET, STEP, SHUTDOWN and nothing else");
    guest.finish();
}

/// The control for the headline: the same reboot with the declaration left out. The service does
/// not re-handshake, so the kernel's BIND reaches a guest that is waiting for a HELLO, which the
/// real guest would fail on (`do_handshake` reads a HELLO first): the fake records the violation
/// and the BIND is never answered.
#[test]
fn without_the_declaration_a_rebooted_guest_is_never_told_and_the_bind_goes_unanswered() {
    let channel = RebootChannel::create("control", "0");
    let guest = RebootingGuest::spawn(channel.flag(), GuestOptions::default());
    let mut service = start("rehandshake-control", &guest, &channel, &[]);
    let params = service.bind_params(&BTreeMap::new());
    let mut lock = BlockingLockstepClient::connect_plaintext(&service.grpc_addr).unwrap();
    assert!(lock.bind(client_bind_request(&params)).unwrap().lockstep_capable);
    check_step_reply(1, 64, &lock.step(step_request(1, 64)).unwrap());
    let mut edge = BlockingBoardEdgeClient::connect_plaintext(&service.grpc_addr).unwrap();
    let resp = edge.power_cycle(power_cycle_request(&channel)).unwrap();
    assert!(resp.outcome == BoardPowerCycleOutcome::Performed as i32 && !resp.peer_rehandshaken);
    let addr = service.grpc_addr.clone();
    let rebind = std::thread::spawn(move || {
        let mut lock = BlockingLockstepClient::connect_plaintext(&addr).unwrap();
        lock.bind(client_bind_request(&params)).map_err(|e| e.to_string())
    });
    wait_until(Duration::from_secs(10), || guest.snapshot(|g| !g.violations.is_empty()));
    let violations = guest.snapshot(|g| g.violations.clone());
    println!("guest violations: {violations:?}");
    assert_eq!(violations, vec!["BIND(0x02) in phase AwaitHello".to_string()]);
    std::thread::sleep(Duration::from_millis(500));
    assert!(!rebind.is_finished(), "the BIND is never answered");
    let _ = service.child.kill();
    let _ = service.child.wait();
    assert!(rebind.join().unwrap().is_err(), "the BIND ends in an error once the service is gone");
    guest.finish();
}

fn wait_until(timeout: Duration, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !done() {
        assert!(Instant::now() < deadline, "not true within {timeout:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn the_rehandshake_flags_have_usage_errors() {
    let channel = RebootChannel::create("usage", "none");
    let uri = channel.uri();
    for (args, needle) in [
        (vec!["--power-cycle-reboots-peer"], "needs --power-control"),
        (vec!["--power-control", uri.as_str(), "--rehandshake-timeout-ms", "5000"], "only apply with --power-cycle-reboots-peer"),
        (vec!["--power-control", uri.as_str(), "--rehandshake-interval-ms", "500"], "only apply with --power-cycle-reboots-peer"),
        (vec!["--power-control", uri.as_str(), "--power-cycle-reboots-peer", "--rehandshake-interval-ms", "5000", "--rehandshake-timeout-ms", "2000"], "longer than"),
        (vec!["--power-control", uri.as_str(), "--power-cycle-reboots-peer", "--rehandshake-interval-ms", "5"], "outside 100..=60000"),
        (vec!["--power-control", uri.as_str(), "--power-cycle-reboots-peer", "--rehandshake-timeout-ms", "50"], "outside 200..=600000"),
        (vec!["--power-control", uri.as_str(), "--power-cycle-reboots-peer", "--rehandshake-timeout-ms", "soon"], "not a number"),
    ] {
        let guest = RebootingGuest::spawn(channel.flag(), GuestOptions::default());
        let device = format!("udp://127.0.0.1:{}", guest.addr.port());
        let mut base = vec!["--port-device", device.as_str(), "--edge-node-id", "e"];
        base.extend(args.iter().copied());
        let mut s = Service::spawn("rehandshake-usage", &base);
        let status = s.wait_exit(Duration::from_secs(10));
        assert_eq!(status.code(), Some(2), "{args:?}: {}", s.stderr());
        assert!(s.stderr().contains(needle), "{args:?}: {}", s.stderr());
        assert_eq!(guest.snapshot(|g| g.datagrams), 0, "{args:?}: the board was never touched");
        guest.finish();
    }
}
