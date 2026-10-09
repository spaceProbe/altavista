//! The UDP transport against fake guests on loopback sockets (no board): the same exchange as
//! the pty test, one frame per datagram; a datagram carrying half a frame, one from a foreign
//! address, and a lost HELLO are each shown handled as specified.
mod common;

use std::collections::BTreeMap;
use std::time::Duration;

use av_lockstep::{BlockingLockstepClient, LockstepShutdownRequest};
use av_lockstep_shim::framing::{encode_frame, FrameType};
use common::*;

fn start(tag: &str, guest: &UdpGuest) -> Service {
    let device = format!("udp://127.0.0.1:{}", guest.addr.port());
    let mut service = Service::spawn(tag, &["--port-device", &device, "--edge-node-id", "edge-udp"]);
    service.wait_ready(Duration::from_secs(20));
    service
}

#[test]
fn a_full_run_over_udp_one_frame_per_datagram() {
    let guest = UdpGuest::spawn(Brain::default());
    let mut service = start("udp-full", &guest);
    let bind = drive_full_run(&service.grpc_addr, &service.bind_params(&BTreeMap::new()));
    assert_eq!(bind.version, "fake-guest-1");
    let status = service.wait_exit(Duration::from_secs(15));
    assert!(status.success(), "{status}; stderr:\n{}", service.stderr());
    service.save_evidence("udp_full.stderr.txt");
    let stderr = service.stderr();
    assert!(stderr.contains("UDP datagrams: 9 accepted, 9 sent, 0 dropped from foreign addresses, 0 malformed"), "{stderr}");
    {
        let b = guest.brain.lock().unwrap();
        assert_eq!(b.step_sequences, [1, 2, 3, 4, 6]);
        assert_eq!(b.step_input_bytes, [300, 400, 1000, 40, 700]);
        assert_eq!(b.framing_violations, 0, "every datagram the guest received was exactly one frame");
        assert_eq!(b.frames.len(), 1 + 1 + 5 + 1 + 1);
    }
    guest.finish();
}

#[test]
fn a_foreign_datagram_is_dropped_and_counted_and_half_a_frame_is_a_typed_failure() {
    // The guest answers STEP 3 with only the first half of its STEP_DONE frame.
    let guest = UdpGuest::spawn(Brain { half_reply_for_sequences: vec![3], ..Brain::default() });
    let mut service = start("udp-faults", &guest);
    let mut client = BlockingLockstepClient::connect_plaintext(&service.grpc_addr).unwrap();
    assert!(client.bind(client_bind_request(&service.bind_params(&BTreeMap::new()))).unwrap().lockstep_capable);

    // 1. A forged "reply" from an address that is not the configured peer, queued at the
    //    service's socket before the real STEP: it must be skipped, not taken as the answer.
    let service_addr = guest.service_addr.lock().unwrap().expect("the service's HELLO reached the guest");
    let forger = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    assert_ne!(forger.local_addr().unwrap(), guest.addr);
    let forged = LockstepStepResponseForged::frame(1);
    forger.send_to(&forged, service_addr).unwrap();
    std::thread::sleep(Duration::from_millis(100));
    let resp = client.step(step_request(1, 300)).expect("STEP 1 must be answered by the real peer, not the forger");
    check_step_reply(1, 300, &resp);

    // 2. Half a frame: the STEP fails with the typed datagram error, the stream is not desynchronised ...
    let err = client.step(step_request(3, 300)).expect_err("a half-frame reply must fail the step");
    let msg = err.message().to_string();
    assert!(msg.contains("half a frame is not accepted") && msg.contains("declared"), "typed datagram error expected, got: {msg}");
    // ... and the next exchange works.
    let resp = client.step(step_request(4, 300)).expect("the link stays usable after a malformed datagram");
    check_step_reply(4, 300, &resp);

    client.shutdown(LockstepShutdownRequest { run_id: RUN_ID.to_string() }).unwrap();
    let status = service.wait_exit(Duration::from_secs(15));
    assert!(status.success(), "{status}");
    let stderr = service.stderr();
    service.save_evidence("udp_faults.stderr.txt");
    assert!(stderr.contains("dropped a ") && stderr.contains("which is not the configured peer"), "{stderr}");
    // Accepted: HELLO, BIND_ACK, STEP_DONE#1, STEP_DONE#4, SHUTDOWN_ACK; sent: HELLO, BIND, STEP 1, 3, 4, SHUTDOWN; the half-frame reply is the 1 malformed.
    assert!(stderr.contains("UDP datagrams: 5 accepted, 6 sent, 1 dropped from foreign addresses, 1 malformed"), "{stderr}");
    guest.finish();
}

/// A syntactically valid STEP_DONE frame for `sequence`, as a forger would send.
struct LockstepStepResponseForged;
impl LockstepStepResponseForged {
    fn frame(sequence: u64) -> Vec<u8> {
        use prost::Message;
        let resp = av_cdm::pb::LockstepStepResponse { sequence, reached_tai_ns: sequence as i64 * BASE_PERIOD_NS, outputs: vec![], named_outputs: BTreeMap::from([("sum".to_string(), -1.0)]) };
        encode_frame(FrameType::StepDone, &resp.encode_to_vec())
    }
}

#[test]
fn a_lost_hello_is_a_typed_handshake_timeout_not_a_retry() {
    // A socket that receives the HELLO and never answers.
    let silent = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    silent.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let device = format!("udp://127.0.0.1:{}", silent.local_addr().unwrap().port());
    let mut service = Service::spawn("udp-silent", &["--port-device", &device, "--edge-node-id", "e", "--handshake-timeout-ms", "800"]);
    let status = service.wait_exit(Duration::from_secs(10));
    assert_eq!(status.code(), Some(1));
    assert!(service.stderr().contains("did not complete within 800 ms") && service.stderr().contains("never retried"), "{}", service.stderr());
    // Exactly one HELLO datagram was sent, and no second one followed (HELLO is never retried).
    let mut buf = [0u8; 64];
    let (n, _) = silent.recv_from(&mut buf).unwrap();
    assert_eq!(n, 11, "HELLO is a 4-byte length, a type byte and 6 payload bytes");
    silent.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
    assert!(silent.recv_from(&mut buf).is_err(), "a second HELLO was sent");
}

#[test]
fn an_unresolvable_or_wrong_peer_family_is_refused_at_open() {
    let mut s = Service::spawn("udp-nohost", &["--port-device", "udp://no-such-host.invalid:5000", "--edge-node-id", "e"]);
    let status = s.wait_exit(Duration::from_secs(20));
    assert_eq!(status.code(), Some(1));
    assert!(s.stderr().contains("opening UDP link to no-such-host.invalid:5000"), "{}", s.stderr());
}
