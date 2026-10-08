//! The board I/O log (question 242 (a)), through the real `av-edge-board` binary and the
//! kernel's own `LockstepService` client, against the fake guests of hilprep-3b (a UDP
//! socket, a pseudo-terminal). **No board is involved.**
//!
//! What is shown:
//! - every exchange of a session (Bind, 5 Steps, Reset, Shutdown) is a record of a log that
//!   verifies (chain, sequence, signatures, certificate fingerprint);
//! - each STEP record's `outputs` / `named_outputs` equal, byte for byte, what the client
//!   received, and its `inputs` equal what the client sent and what the guest decoded;
//! - the BIND record's request is the board-parameter-stripped one, byte-equal to the BIND
//!   frame payload the guest received;
//! - **ordering**: immediately after each `Step` returns to the client, an independent reader
//!   (a fresh read and full verification of the file) already holds that step's record. This
//!   establishes write-before-reply for the real binary; that the write is followed by an
//!   `fsync` before the reply is established in `tests/io_log_inprocess.rs` (a sink that
//!   records write and sync events and delays the sync) and in the writer's unit tests;
//! - the startup refusals (no log flags, an existing log, a wrong-curve key, a key that does
//!   not match its certificate), and that a failed startup leaves no empty log behind;
//! - a failed exchange is logged with its error, and the log keeps verifying;
//! - the `av-edge-board-log` tool's output.
mod common;

use std::collections::BTreeMap;
use std::process::Command;
use std::time::Duration;

use av_edge::board::{BIND_PARAM_EDGE_NODE_ID, BIND_PARAM_PORT_DEVICE};
use av_edge::board_log::{BoardIoKind, BoardIoRecord, VerifiedLog};
use av_lockstep::{BlockingLockstepClient, LockstepResetRequest, LockstepShutdownRequest};
use common::*;
use prost::Message;

fn now_ns() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as i64
}

fn save(name: &str, bytes: &[u8]) {
    if let Ok(dir) = std::env::var("AV_EDGE_BOARD_EVIDENCE_DIR") {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(std::path::PathBuf::from(dir).join(name), bytes);
    }
}

fn kind(r: &BoardIoRecord) -> BoardIoKind {
    BoardIoKind::try_from(r.kind).unwrap()
}

/// Run the session of `drive_full_run` against `service` and check the log after every call.
/// Returns the final verified log and the BIND frame payload the client's Bind forwarded.
fn logged_session(service: &Service, device: &str, edge_node: &str) -> VerifiedLog {
    let instance_params: BTreeMap<String, String> = [("gain".to_string(), "2.5".to_string())].into();
    let mut sent_params = instance_params.clone();
    sent_params.insert(BIND_PARAM_EDGE_NODE_ID.to_string(), edge_node.to_string());
    sent_params.insert(BIND_PARAM_PORT_DEVICE.to_string(), device.to_string());

    let t0 = now_ns();
    let mut client = BlockingLockstepClient::connect_plaintext(&service.grpc_addr).expect("connect to av-edge-board");

    // ---- BIND ----
    let bind = client.bind(client_bind_request(&sent_params)).expect("Bind RPC");
    assert!(bind.lockstep_capable);
    let log = service.read_io_log();
    assert_eq!(log.records.len(), 1, "the BIND record is on disk when Bind returns");
    let r = &log.records[0];
    assert_eq!((kind(r), r.sequence, r.run_id.as_str(), r.instance.as_str()), (BoardIoKind::Bind, 1, RUN_ID, INSTANCE));
    assert_eq!(r.bind_request.as_ref().unwrap().parameters, instance_params, "the logged BIND is the one forwarded: the board parameters are stripped");
    assert_eq!(r.bind_response.as_ref().unwrap().encode_to_vec(), bind.encode_to_vec(), "the board's BIND_ACK, as the client received it");
    assert_eq!(r.producer_id, edge_node);
    assert_eq!(r.prev_hash, av_edge::hash::GENESIS);

    // ---- STEPs ----
    let mut expected_records = 1usize;
    for (sequence, len) in [(1u64, 300usize), (2, 400), (3, 1000), (4, 40)] {
        let req = step_request(sequence, len);
        let t_step = std::time::Instant::now();
        let resp = client.step(req.clone()).unwrap_or_else(|e| panic!("Step {sequence}: {e}"));
        println!("STEP-LATENCY seq={sequence} {:?} (client side, includes the record write and fsync)", t_step.elapsed());
        check_step_reply(sequence, len, &resp);
        expected_records += 1;
        check_step_record(service, expected_records, &req, &resp, t0);
    }

    // ---- RESET ----
    let reset = client.reset(LockstepResetRequest { sequence: 5, tai_ns: 4 * BASE_PERIOD_NS, reason: "power_cycle".to_string() }).expect("Reset RPC");
    assert_eq!(reset.sequence, 5);
    expected_records += 1;
    let log = service.read_io_log();
    assert_eq!(log.records.len(), expected_records);
    let r = log.records.last().unwrap();
    assert_eq!((kind(r), r.lockstep_sequence, r.reset_tai_ns, r.reset_reason.as_str()), (BoardIoKind::Reset, 5, 4 * BASE_PERIOD_NS, "power_cycle"));

    // ---- a STEP after the reset ----
    let req = step_request(6, 700);
    let resp = client.step(req.clone()).expect("Step after Reset");
    check_step_reply(6, 700, &resp);
    expected_records += 1;
    check_step_record(service, expected_records, &req, &resp, t0);

    // ---- SHUTDOWN ----
    client.shutdown(LockstepShutdownRequest { run_id: RUN_ID.to_string() }).expect("Shutdown RPC");
    expected_records += 1;
    let log = service.read_io_log();
    assert_eq!(log.records.len(), expected_records, "the SHUTDOWN record is on disk when Shutdown returns");
    let r = log.records.last().unwrap();
    assert_eq!((kind(r), r.run_id.as_str()), (BoardIoKind::Shutdown, RUN_ID));
    log
}

/// Immediately after a Step returned: a fresh independent read of the file holds exactly
/// `expected_records` records, the last of which is this step, byte for byte.
fn check_step_record(service: &Service, expected_records: usize, req: &av_lockstep::LockstepStepRequest, resp: &av_lockstep::LockstepStepResponse, t0: i64) {
    let log = service.read_io_log();
    assert_eq!(log.records.len(), expected_records, "step {}: its record must be on disk by the time the reply is back", req.sequence);
    let r = log.records.last().unwrap();
    assert_eq!(kind(r), BoardIoKind::Step);
    assert_eq!((r.lockstep_sequence, r.until_tai_ns), (req.sequence, req.until_tai_ns));
    assert_eq!(r.inputs, req.inputs, "inputs as sent to the board");
    assert_eq!(r.outputs, resp.outputs, "outputs: what the board returned and the client received");
    assert_eq!(r.outputs[0].encode_to_vec(), resp.outputs[0].encode_to_vec());
    assert_eq!(r.named_outputs, resp.named_outputs);
    assert!(r.error.is_empty());
    assert!(r.request_written_unix_ns >= t0 && r.response_read_unix_ns >= r.request_written_unix_ns && r.response_read_unix_ns <= now_ns(), "link instants: t0={t0} written={} read={}", r.request_written_unix_ns, r.response_read_unix_ns);
}

fn run_cli(service: &Service, evidence_name: &str, extra: &[&str]) -> (std::process::ExitStatus, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_av-edge-board-log")).arg(&service.io_log).arg("--cert").arg(&service.identity.cert_pem).args(extra).output().unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    save(evidence_name, stdout.as_bytes());
    (out.status, stdout)
}

fn assert_final_log(service: &Service, log: &VerifiedLog, guest_bind_payload: &[u8], cli_name: &str) {
    assert_eq!(log.records.len(), 1 + 4 + 1 + 1 + 1);
    assert!(log.recovery.is_none());
    // The guest-received BIND frame payload equals the logged forwarded request.
    assert_eq!(log.records[0].bind_request.as_ref().unwrap().encode_to_vec(), guest_bind_payload, "the logged BIND is byte-equal to what the guest received");
    let fp = &log.records[0].signer_cert_sha256;
    assert_eq!(fp.len(), 64);
    assert!(log.records.iter().all(|r| &r.signer_cert_sha256 == fp && r.link_config_sha256 == log.records[0].link_config_sha256));

    let (status, out) = run_cli(service, cli_name, &["--records"]);
    println!("{out}");
    assert!(status.success(), "{status}\n{out}");
    for line in ["verification: OK\n", "records: 8\n", "binds: 1\n", "steps: 5\n", "resets: 1\n", "shutdowns: 1\n", "failed_exchanges: 0\n", "first_epoch_tai_ns: 100000000\n", "last_epoch_tai_ns: 600000000\n", &format!("run_ids: {RUN_ID}\n"), &format!("chain_head: {}\n", av_edge::hash::hex_encode(&log.chain_head))] {
        assert!(out.contains(line), "CLI output lacks {line:?}:\n{out}");
    }
    assert!(out.contains(&format!("signer_cert_sha256: {fp}\n")));
}

#[test]
fn a_udp_session_is_logged_exchange_by_exchange_and_each_record_precedes_its_reply() {
    let guest = UdpGuest::spawn(Brain::default());
    let device = format!("udp://127.0.0.1:{}", guest.addr.port());
    let mut service = Service::spawn("iolog-udp", &["--port-device", &device, "--edge-node-id", "edge-udp"]);
    service.wait_ready(Duration::from_secs(20));
    let log = logged_session(&service, &device, "edge-udp");
    let status = service.wait_exit(Duration::from_secs(15));
    assert!(status.success(), "{status}; stderr:\n{}", service.stderr());
    service.save_evidence("iolog_udp.stderr.txt");
    assert!(service.stderr().contains("board I/O log") && service.stderr().contains("8 records, chain head "), "{}", service.stderr());
    let payload = guest.brain.lock().unwrap().binds[0].clone();
    assert_final_log(&service, &log, &payload, "iolog_udp.cli.txt");
    guest.finish();
}

#[test]
fn a_serial_session_over_a_pty_is_logged_the_same_way() {
    let pty = open_pty();
    let master_fd = {
        use std::os::fd::AsRawFd;
        pty.master.as_raw_fd()
    };
    let guest = SerialGuest::spawn(master_fd);
    let device = format!("{}@115200", pty.slave_path);
    let mut service = Service::spawn("iolog-pty", &["--port-device", &device, "--edge-node-id", "edge-pty"]);
    service.wait_ready(Duration::from_secs(20));
    let log = logged_session(&service, &device, "edge-pty");
    let status = service.wait_exit(Duration::from_secs(15));
    assert!(status.success(), "{status}; stderr:\n{}", service.stderr());
    service.save_evidence("iolog_pty.stderr.txt");
    let payload = guest.brain.lock().unwrap().binds[0].clone();
    guest.finish();
    assert_final_log(&service, &log, &payload, "iolog_pty.cli.txt");
    // The serial STEP records show the line's own wire time between write and read: a 305-byte
    // STEP at 115 200 baud takes ~26 ms to write, so the instants (taken at the end of the write
    // and the end of the read) are never equal, and the log's first STEP shows a plausible span.
    let step = log.records.iter().find(|r| kind(r) == BoardIoKind::Step).unwrap();
    assert!(step.response_read_unix_ns > step.request_written_unix_ns);
}

#[test]
fn the_service_refuses_to_run_without_a_log_or_with_a_bad_identity_or_an_existing_log() {
    let guest = UdpGuest::spawn(Brain::default());
    let device = format!("udp://127.0.0.1:{}", guest.addr.port());
    let base = ["--port-device", device.as_str(), "--edge-node-id", "edge-udp"];

    // No log flags at all, then each one missing in turn: usage error (status 2), named.
    let mut s = Service::spawn_with("iolog-nolog", &base, false);
    let status = s.wait_exit(Duration::from_secs(10));
    assert_eq!(status.code(), Some(2), "{}", s.stderr());
    assert!(s.stderr().contains("--io-log is required") && s.stderr().contains("without a durable"), "{}", s.stderr());
    for (missing, args) in [("--signing-key", ["--io-log", "x.log", "--signing-cert", "c.pem"]), ("--signing-cert", ["--io-log", "x.log", "--signing-key", "k.pem"])] {
        let mut a: Vec<&str> = base.to_vec();
        a.extend(args);
        let mut s = Service::spawn_with("iolog-partial", &a, false);
        assert_eq!(s.wait_exit(Duration::from_secs(10)).code(), Some(2));
        assert!(s.stderr().contains(&format!("{missing} is required")), "{}", s.stderr());
    }

    // An existing log is refused, and left byte-for-byte as it was.
    let mut s = Service::spawn_with("iolog-exists", &base, false);
    s.child.kill().unwrap();
    let _ = s.child.wait();
    std::fs::write(&s.io_log, b"previous run").unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_av-edge-board"));
    cmd.args(base).arg("--grpc-addr").arg(format!("127.0.0.1:{}", free_tcp_port())).arg("--io-log").arg(&s.io_log).arg("--signing-key").arg(&s.identity.key_pem).arg("--signing-cert").arg(&s.identity.cert_pem);
    let out = cmd.output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("already exists") && stderr.contains("one log per service run"), "{stderr}");
    assert_eq!(std::fs::read(&s.io_log).unwrap(), b"previous run");
    println!("EXISTING-LOG-REFUSAL {}", stderr.trim());

    // A key on the wrong curve, and a key that is not the certificate's: refused before any log is created.
    let other = make_identity(&s.dir, "other");
    let p256 = openssl::ec::EcKey::generate(&openssl::ec::EcGroup::from_curve_name(openssl::nid::Nid::X9_62_PRIME256V1).unwrap()).unwrap();
    let p256_path = s.dir.join("p256.key.pem");
    std::fs::write(&p256_path, p256.private_key_to_pem().unwrap()).unwrap();
    let fresh_log = s.dir.join("fresh.log");
    for (what, key, cert, needle) in [("wrong curve", &p256_path, &s.identity.cert_pem, "not P-384"), ("mismatch", &other.key_pem, &s.identity.cert_pem, "not the signing key's public key")] {
        let out = Command::new(env!("CARGO_BIN_EXE_av-edge-board"))
            .args(base)
            .arg("--grpc-addr")
            .arg(format!("127.0.0.1:{}", free_tcp_port()))
            .arg("--io-log")
            .arg(&fresh_log)
            .arg("--signing-key")
            .arg(key)
            .arg("--signing-cert")
            .arg(cert)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        assert_eq!(out.status.code(), Some(1), "{what}: {stderr}");
        assert!(stderr.contains(needle), "{what}: {stderr}");
        assert!(!fresh_log.exists(), "{what}: no log may be created for a bad identity");
        println!("IDENTITY-REFUSAL[{what}] {}", stderr.trim());
    }
    assert!(guest.brain.lock().unwrap().frames.is_empty(), "no refusal touched the board");
    guest.finish();
}

#[test]
fn a_startup_that_fails_after_the_log_was_created_leaves_no_empty_log_behind() {
    // Nobody answers on this pty, so the handshake times out after the log was created.
    let pty = open_pty();
    let device = format!("{}@115200", pty.slave_path);
    let mut s = Service::spawn("iolog-cleanup", &["--port-device", &device, "--edge-node-id", "e", "--handshake-timeout-ms", "600"]);
    let status = s.wait_exit(Duration::from_secs(10));
    assert_eq!(status.code(), Some(1));
    assert!(s.stderr().contains("board I/O log created at") && s.stderr().contains("did not complete within 600 ms"), "{}", s.stderr());
    assert!(!s.io_log.exists(), "the empty log of a failed startup must not block the next start");
    drop(pty);
}

#[test]
fn a_failed_exchange_is_logged_with_its_error_and_the_log_keeps_verifying() {
    // The guest answers STEP 2 with half a frame: a typed failure of that exchange.
    let guest = UdpGuest::spawn(Brain { half_reply_for_sequences: vec![2], ..Brain::default() });
    let device = format!("udp://127.0.0.1:{}", guest.addr.port());
    let mut service = Service::spawn("iolog-fail", &["--port-device", &device, "--edge-node-id", "edge-udp"]);
    service.wait_ready(Duration::from_secs(20));
    let mut client = BlockingLockstepClient::connect_plaintext(&service.grpc_addr).unwrap();
    assert!(client.bind(client_bind_request(&service.bind_params(&BTreeMap::new()))).unwrap().lockstep_capable);
    check_step_reply(1, 300, &client.step(step_request(1, 300)).unwrap());
    let err = client.step(step_request(2, 300)).expect_err("a half-frame reply fails the step");
    let log = service.read_io_log();
    assert_eq!(log.records.len(), 3, "the failed exchange has its record by the time the error is back");
    let r = log.records.last().unwrap();
    assert_eq!((kind(r), r.lockstep_sequence), (BoardIoKind::Step, 2));
    assert!(r.error.contains("half a frame is not accepted"), "{:?}", r.error);
    assert!(err.message().contains("half a frame is not accepted"));
    assert_eq!(r.inputs.len(), 1, "what was sent is still recorded");
    assert!(r.outputs.is_empty() && r.named_outputs.is_empty());
    println!("FAILED-EXCHANGE-RECORD error={:?}", r.error);
    check_step_reply(3, 300, &client.step(step_request(3, 300)).expect("the link stays usable"));
    client.shutdown(LockstepShutdownRequest { run_id: RUN_ID.to_string() }).unwrap();
    service.wait_exit(Duration::from_secs(15));
    let log = service.read_io_log();
    assert_eq!(log.records.len(), 5);
    assert_eq!(log.summary().failed_exchanges, 1);
    let (status, out) = run_cli(&service, "iolog_failed_exchange.cli.txt", &[]);
    assert!(status.success() && out.contains("failed_exchanges: 1\n"), "{out}");
    guest.finish();
}

#[test]
fn the_tool_reports_tampering_a_wrong_certificate_and_a_torn_tail_with_distinct_exit_codes() {
    let guest = UdpGuest::spawn(Brain::default());
    let device = format!("udp://127.0.0.1:{}", guest.addr.port());
    let mut service = Service::spawn("iolog-tool", &["--port-device", &device, "--edge-node-id", "edge-udp"]);
    service.wait_ready(Duration::from_secs(20));
    let _ = logged_session(&service, &device, "edge-udp");
    service.wait_exit(Duration::from_secs(15));
    guest.finish();
    let bytes = std::fs::read(&service.io_log).unwrap();
    let cli = |path: &std::path::Path, cert: &std::path::Path| {
        let out = Command::new(env!("CARGO_BIN_EXE_av-edge-board-log")).arg(path).arg("--cert").arg(cert).output().unwrap();
        (out.status.code(), String::from_utf8(out.stdout).unwrap())
    };
    assert_eq!(cli(&service.io_log, &service.identity.cert_pem).0, Some(0));
    // Also verifies against the bare public key.
    assert_eq!(cli(&service.io_log, &service.identity.pub_pem).0, Some(0));

    // A flipped byte near the end of the file (inside the last record).
    let tampered = service.dir.join("tampered.log");
    let mut b = bytes.clone();
    let n = b.len();
    b[n - 20] ^= 0x01;
    std::fs::write(&tampered, &b).unwrap();
    let (code, out) = cli(&tampered, &service.identity.cert_pem);
    assert_eq!(code, Some(1));
    assert!(out.contains("verification: FAILED: record 8:"), "{out}");
    println!("TOOL-TAMPERED {}", out.lines().find(|l| l.starts_with("verification")).unwrap());

    // A different certificate.
    let other = make_identity(&service.dir, "other");
    let (code, out) = cli(&service.io_log, &other.cert_pem);
    assert_eq!(code, Some(1));
    assert!(out.contains("verification: FAILED: record 1: signed by certificate"), "{out}");
    println!("TOOL-WRONG-CERT {}", out.lines().find(|l| l.starts_with("verification")).unwrap());
    let (code, out) = cli(&service.io_log, &other.pub_pem);
    assert_eq!(code, Some(1));
    assert!(out.contains("record 1: the signature does not verify"), "{out}");

    // A torn tail: truncated mid-record. Exit 3, the intact records still counted.
    let torn = service.dir.join("torn.log");
    std::fs::write(&torn, &bytes[..bytes.len() - 100]).unwrap();
    let (code, out) = cli(&torn, &service.identity.cert_pem);
    assert_eq!(code, Some(3), "{out}");
    assert!(out.contains("verification: OK, TORN TAIL") && out.contains("records: 7\n") && out.contains("torn_tail: offset="), "{out}");
    println!("TOOL-TORN {}", out.lines().filter(|l| l.starts_with("verification") || l.starts_with("records") || l.starts_with("torn_tail")).collect::<Vec<_>>().join(" | "));
    save("iolog_tool_torn.cli.txt", out.as_bytes());

    // Usage.
    let out = Command::new(env!("CARGO_BIN_EXE_av-edge-board-log")).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
}
