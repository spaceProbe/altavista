//! Round-trip of every "lockstep-local v1" frame type (M23.1 brief). Each test writes a
//! frame with `encode_frame`, reads it back with `read_frame` over an in-memory
//! `tokio::net::UnixStream::pair()`, and asserts the decoded frame type and payload match
//! what was sent -- this is a *different* property than `tests/frame_bytes.rs`'s
//! hand-pinned tests (which pin exact bytes against an independent derivation): this file
//! is what catches an asymmetric bug (e.g. `read_frame` reading one byte short, or
//! `write_frame` and `read_frame` disagreeing about which end the length field measures
//! from) that a purely one-sided byte-pinning test would not exercise, while the pinned
//! tests catch a self-consistent wrong layout this round-trip test alone could never see.
use av_cdm::pb::{LockstepBindRequest, LockstepBindResponse, LockstepResetRequest, LockstepResetResponse, LockstepShutdownRequest, LockstepShutdownResponse, LockstepStepRequest, LockstepStepResponse, PortMessage};
use av_lockstep_shim::framing::{decode_error, decode_hello, encode_error, encode_hello, read_frame, write_frame, ErrorCode, FrameType};
use prost::Message;
use tokio::net::UnixStream;

async fn round_trip_bytes(frame_type: FrameType, payload: &[u8]) -> (FrameType, Vec<u8>) {
    let (mut a, mut b) = UnixStream::pair().expect("UnixStream::pair");
    write_frame(&mut a, frame_type, payload).await.expect("write_frame");
    read_frame(&mut b).await.expect("read_frame")
}

#[tokio::test]
async fn hello_round_trips() {
    let sent = encode_hello(1);
    let (frame_type, payload) = round_trip_bytes(FrameType::Hello, &sent).await;
    assert_eq!(frame_type, FrameType::Hello);
    let (magic, version) = decode_hello(&payload).unwrap();
    assert_eq!(magic, *b"AVL1");
    assert_eq!(version, 1);
}

#[tokio::test]
async fn bind_round_trips() {
    let request = LockstepBindRequest {
        run_id: "run-1".to_string(),
        instance: "adcs".to_string(),
        ports: vec![],
        start_tai_ns: 0,
        base_period_ns: 1_000_000_000,
        step_period_ns: 1_000_000_000,
        seed: 42,
        parameters: Default::default(),
    };
    let (frame_type, payload) = round_trip_bytes(FrameType::Bind, &request.encode_to_vec()).await;
    assert_eq!(frame_type, FrameType::Bind);
    assert_eq!(LockstepBindRequest::decode(payload.as_slice()).unwrap(), request);
}

#[tokio::test]
async fn bind_ack_round_trips() {
    let response = LockstepBindResponse { lockstep_capable: true, binding_hash: "a".repeat(64), version: "shim/0.1".to_string(), refusal_reason: String::new() };
    let (frame_type, payload) = round_trip_bytes(FrameType::BindAck, &response.encode_to_vec()).await;
    assert_eq!(frame_type, FrameType::BindAck);
    assert_eq!(LockstepBindResponse::decode(payload.as_slice()).unwrap(), response);
}

#[tokio::test]
async fn step_round_trips_with_a_port_message() {
    let request = LockstepStepRequest {
        sequence: 7,
        until_tai_ns: 2_000_000_000,
        inputs: vec![PortMessage { port: "in".to_string(), tai_ns: 1_000_000_000, payload: 3.5f64.to_le_bytes().to_vec() }],
    };
    let (frame_type, payload) = round_trip_bytes(FrameType::Step, &request.encode_to_vec()).await;
    assert_eq!(frame_type, FrameType::Step);
    assert_eq!(LockstepStepRequest::decode(payload.as_slice()).unwrap(), request);
}

#[tokio::test]
async fn step_done_round_trips_with_named_outputs() {
    let mut named_outputs = std::collections::BTreeMap::new();
    named_outputs.insert("integral".to_string(), 12.5);
    let response = LockstepStepResponse {
        sequence: 7,
        reached_tai_ns: 2_000_000_000,
        outputs: vec![PortMessage { port: "out".to_string(), tai_ns: 2_000_000_000, payload: 12.5f64.to_le_bytes().to_vec() }],
        named_outputs,
    };
    let (frame_type, payload) = round_trip_bytes(FrameType::StepDone, &response.encode_to_vec()).await;
    assert_eq!(frame_type, FrameType::StepDone);
    assert_eq!(LockstepStepResponse::decode(payload.as_slice()).unwrap(), response);
}

#[tokio::test]
async fn reset_round_trips() {
    let request = LockstepResetRequest { sequence: 3, tai_ns: 5_000_000_000, reason: "power_cycle".to_string() };
    let (frame_type, payload) = round_trip_bytes(FrameType::Reset, &request.encode_to_vec()).await;
    assert_eq!(frame_type, FrameType::Reset);
    assert_eq!(LockstepResetRequest::decode(payload.as_slice()).unwrap(), request);
}

#[tokio::test]
async fn reset_ack_round_trips() {
    let response = LockstepResetResponse { sequence: 3 };
    let (frame_type, payload) = round_trip_bytes(FrameType::ResetAck, &response.encode_to_vec()).await;
    assert_eq!(frame_type, FrameType::ResetAck);
    assert_eq!(LockstepResetResponse::decode(payload.as_slice()).unwrap(), response);
}

#[tokio::test]
async fn shutdown_round_trips() {
    let request = LockstepShutdownRequest { run_id: "run-1".to_string() };
    let (frame_type, payload) = round_trip_bytes(FrameType::Shutdown, &request.encode_to_vec()).await;
    assert_eq!(frame_type, FrameType::Shutdown);
    assert_eq!(LockstepShutdownRequest::decode(payload.as_slice()).unwrap(), request);
}

#[tokio::test]
async fn shutdown_ack_round_trips() {
    let response = LockstepShutdownResponse {};
    let (frame_type, payload) = round_trip_bytes(FrameType::ShutdownAck, &response.encode_to_vec()).await;
    assert_eq!(frame_type, FrameType::ShutdownAck);
    assert_eq!(LockstepShutdownResponse::decode(payload.as_slice()).unwrap(), response);
}

#[tokio::test]
async fn error_round_trips() {
    let sent = encode_error(ErrorCode::ReachedTaiMismatch, 100, 101, "reached_tai_ns off by one");
    let (frame_type, payload) = round_trip_bytes(FrameType::Error, &sent).await;
    assert_eq!(frame_type, FrameType::Error);
    let (code, expected, actual, message) = decode_error(&payload).unwrap();
    assert_eq!(code, ErrorCode::ReachedTaiMismatch);
    assert_eq!(expected, 100);
    assert_eq!(actual, 101);
    assert_eq!(message, "reached_tai_ns off by one");
}
