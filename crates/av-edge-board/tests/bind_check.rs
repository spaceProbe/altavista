//! The Bind-time board check, through the real binary and the kernel's gRPC client, against
//! a fake guest on loopback UDP (no board): matching parameters are forwarded stripped (the
//! guest's BIND bytes equal the container path's encoding without them); a mismatched device
//! or edge node, or a Bind lacking the board parameters (one or both), is refused with
//! `lockstep_capable = false` and nothing reaches the guest.
mod common;

use std::collections::BTreeMap;
use std::time::Duration;

use av_edge::board::{BIND_PARAM_EDGE_NODE_ID, BIND_PARAM_PORT_DEVICE};
use av_lockstep::{BlockingLockstepClient, LockstepShutdownRequest};
use common::*;
use prost::Message;

fn params(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn start(tag: &str, guest: &UdpGuest) -> (Service, String) {
    let device = format!("udp://127.0.0.1:{}", guest.addr.port());
    let mut service = Service::spawn(tag, &["--port-device", &device, "--edge-node-id", "edge-udp"]);
    service.wait_ready(Duration::from_secs(20));
    (service, device)
}

#[test]
fn matching_parameters_are_forwarded_stripped_and_equal_the_container_paths_bytes() {
    let instance_params = params(&[("gain", "2.5"), ("mode", "safe")]);

    // The board path: the same request plus the two board parameters.
    let guest_b = UdpGuest::spawn(Brain::default());
    let (mut service_b, device) = start("bind-match", &guest_b);
    // (Different spellings of one device compare equal in canonical form: unit-tested in
    // `service::tests` and `av_edge::board::tests`.)
    let mut with_board = instance_params.clone();
    with_board.insert(BIND_PARAM_EDGE_NODE_ID.to_string(), "edge-udp".to_string());
    with_board.insert(BIND_PARAM_PORT_DEVICE.to_string(), device.clone());
    let mut client = BlockingLockstepClient::connect_plaintext(&service_b.grpc_addr).unwrap();
    let resp = client.bind(client_bind_request(&with_board)).unwrap();
    assert!(resp.lockstep_capable, "{:?}", resp.refusal_reason);
    client.shutdown(LockstepShutdownRequest { run_id: RUN_ID.into() }).unwrap();
    service_b.wait_exit(Duration::from_secs(15));
    assert!(service_b.stderr().contains("board parameters match this link; stripped them"), "{}", service_b.stderr());
    service_b.save_evidence("bind_match.stderr.txt");
    let forwarded = guest_b.brain.lock().unwrap().binds.clone();
    guest_b.finish();
    assert_eq!(forwarded.len(), 1);

    // The guest's BIND bytes: identical to the container path's, and to an independent
    // encoding of the request without the board parameters.
    // The container path's encoding of the same request (no board parameters), built independently.
    assert_eq!(forwarded[0], container_path_bind(&instance_params).encode_to_vec(), "the BIND bytes with the board parameters stripped must equal the container path's");
    let decoded = av_cdm::pb::LockstepBindRequest::decode(forwarded[0].as_slice()).unwrap();
    assert_eq!(decoded.parameters, instance_params, "only the instance's own parameters reach the guest");
    println!("BIND-BYTES forwarded={} bytes, equal to the container path's independent encoding", forwarded[0].len());
}

#[test]
fn a_mismatched_device_or_edge_node_is_refused_and_nothing_is_forwarded() {
    let guest = UdpGuest::spawn(Brain::default());
    let (mut service, device) = start("bind-mismatch", &guest);
    let mut client = BlockingLockstepClient::connect_plaintext(&service.grpc_addr).unwrap();
    let other_device = format!("udp://127.0.0.1:{}", guest.addr.port() as u32 % 60_000 + 1);
    assert_ne!(other_device, device);

    let cases: Vec<(&str, BTreeMap<String, String>, Vec<&str>)> = vec![
        ("device", params(&[(BIND_PARAM_EDGE_NODE_ID, "edge-udp"), (BIND_PARAM_PORT_DEVICE, &other_device)]), vec![&other_device, &device, "port device differs"]),
        ("serial instead of udp", params(&[(BIND_PARAM_EDGE_NODE_ID, "edge-udp"), (BIND_PARAM_PORT_DEVICE, "/dev/ttyUSB0@115200")]), vec!["/dev/ttyUSB0@115200", &device]),
        ("edge node", params(&[(BIND_PARAM_EDGE_NODE_ID, "edge-other"), (BIND_PARAM_PORT_DEVICE, &device)]), vec!["edge-other", "edge-udp", "edge node id differs"]),
        ("only one parameter", params(&[(BIND_PARAM_EDGE_NODE_ID, "edge-udp")]), vec!["only one of the two board parameters"]),
        ("only the other parameter", params(&[(BIND_PARAM_PORT_DEVICE, &device)]), vec!["only one of the two board parameters", &device]),
        // The lead's ruling (question 242, hilprep-4): a board link is always checked, so a Bind
        // with neither parameter is refused too, not forwarded unchecked.
        ("neither parameter", params(&[("gain", "2.5")]), vec!["neither board parameter is present", BIND_PARAM_EDGE_NODE_ID, BIND_PARAM_PORT_DEVICE, &device, "edge-udp"]),
        ("no parameters at all", params(&[]), vec!["neither board parameter is present"]),
    ];
    for (name, p, must_name) in cases {
        let resp = client.bind(client_bind_request(&p)).unwrap_or_else(|e| panic!("{name}: a refusal is a response, not an RPC error: {e}"));
        assert!(!resp.lockstep_capable, "{name}: must be refused");
        for needle in must_name {
            assert!(resp.refusal_reason.contains(needle), "{name}: the refusal reason must name {needle:?}: {}", resp.refusal_reason);
        }
        println!("REFUSED[{name}] {}", resp.refusal_reason);
        assert!(guest.brain.lock().unwrap().binds.is_empty(), "{name}: nothing may be forwarded to the board");
        assert!(guest.brain.lock().unwrap().frames.len() == 1, "{name}: the guest saw only the handshake's HELLO, got {:?}", guest.brain.lock().unwrap().frames);
    }
    // The service is not wedged by refusals: a matching Bind then reaches the board.
    let good = params(&[(BIND_PARAM_EDGE_NODE_ID, "edge-udp"), (BIND_PARAM_PORT_DEVICE, &device)]);
    assert!(client.bind(client_bind_request(&good)).unwrap().lockstep_capable);
    assert_eq!(guest.brain.lock().unwrap().binds.len(), 1);
    client.shutdown(LockstepShutdownRequest { run_id: RUN_ID.into() }).unwrap();
    service.wait_exit(Duration::from_secs(15));
    service.save_evidence("bind_mismatch.stderr.txt");
    guest.finish();
}
