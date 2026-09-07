//! Every protocol-violation scenario the M23.1 brief calls out by name, exercised against
//! `PeerLink` directly over an in-memory `tokio::net::UnixStream::pair()` "fake peer" --
//! no real socket file, no subprocess, no Python. Each test plays the fake peer just
//! enough to trigger exactly one violation and asserts the specific typed
//! [`ProtocolError`] variant (naming the concrete values involved), never a bare "it
//! errored".
use std::sync::Arc;

use av_cdm::pb::{LockstepResetRequest, LockstepResetResponse, LockstepStepRequest, LockstepStepResponse};
use av_lockstep_shim::framing::{self, decode_error, encode_hello, ErrorCode, FrameType};
use av_lockstep_shim::peer_link::ProtocolError;
use av_lockstep_shim::PeerLink;
use prost::Message;
use tokio::net::UnixStream;

/// Drives a valid handshake to completion: `shim` speaks first (per `PeerLink::handshake`'s
/// own contract), `peer` reads it and answers with a correct `HELLO`. Returns the ready
/// `PeerLink` plus the still-open peer-side stream for the rest of the test to drive.
async fn handshake_ok(shim: UnixStream, mut peer: UnixStream) -> (PeerLink<UnixStream>, UnixStream) {
    let shim_fut = PeerLink::handshake(shim);
    let peer_fut = async {
        let (frame_type, payload) = framing::read_frame(&mut peer).await.expect("peer read HELLO");
        assert_eq!(frame_type, FrameType::Hello);
        let (magic, version) = framing::decode_hello(&payload).unwrap();
        assert_eq!(magic, framing::HELLO_MAGIC);
        assert_eq!(version, framing::PROTOCOL_VERSION);
        framing::write_frame(&mut peer, FrameType::Hello, &encode_hello(framing::PROTOCOL_VERSION)).await.unwrap();
    };
    let (link, ()) = tokio::join!(shim_fut, peer_fut);
    (link.expect("handshake should succeed"), peer)
}

// ---------------------------------------------------------------------------------------
// "A version-handshake mismatch fails loudly."
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn version_handshake_mismatch_fails_loudly() {
    let (shim, mut peer) = UnixStream::pair().unwrap();

    let shim_fut = PeerLink::handshake(shim);
    let peer_fut = async {
        let (frame_type, _payload) = framing::read_frame(&mut peer).await.expect("peer read HELLO");
        assert_eq!(frame_type, FrameType::Hello);
        // Answers with a HELLO declaring a version this shim does not speak.
        framing::write_frame(&mut peer, FrameType::Hello, &encode_hello(99)).await.unwrap();
        // "fails loudly": the shim must tell the peer why, not just silently drop the
        // connection -- assert the very next frame is a typed ERROR naming both versions.
        let (frame_type, payload) = framing::read_frame(&mut peer).await.expect("peer read ERROR");
        assert_eq!(frame_type, FrameType::Error);
        let (code, expected, actual, _message) = decode_error(&payload).unwrap();
        assert_eq!(code, ErrorCode::VersionMismatch);
        assert_eq!(expected, i64::from(framing::PROTOCOL_VERSION));
        assert_eq!(actual, 99);
    };
    let (shim_result, ()) = tokio::join!(shim_fut, peer_fut);

    match shim_result {
        Err(ProtocolError::VersionMismatch { ours, peer: peer_version }) => {
            assert_eq!(ours, framing::PROTOCOL_VERSION);
            assert_eq!(peer_version, 99);
        }
        Ok(_) => panic!("expected Err(ProtocolError::VersionMismatch), got Ok"),
        Err(other) => panic!("expected ProtocolError::VersionMismatch, got {other}"),
    }
}

#[tokio::test]
async fn bad_hello_magic_also_fails_loudly() {
    let (shim, mut peer) = UnixStream::pair().unwrap();

    let shim_fut = PeerLink::handshake(shim);
    let peer_fut = async {
        let (frame_type, _payload) = framing::read_frame(&mut peer).await.expect("peer read HELLO");
        assert_eq!(frame_type, FrameType::Hello);
        // A correctly-versioned HELLO but with the wrong magic -- must be caught before
        // the version is even inspected, and must not be misparsed as anything else.
        let mut bad_payload = encode_hello(framing::PROTOCOL_VERSION);
        bad_payload[0] = b'X';
        framing::write_frame(&mut peer, FrameType::Hello, &bad_payload).await.unwrap();
        let (frame_type, payload) = framing::read_frame(&mut peer).await.expect("peer read ERROR");
        assert_eq!(frame_type, FrameType::Error);
        let (code, ..) = decode_error(&payload).unwrap();
        assert_eq!(code, ErrorCode::BadHelloMagic);
    };
    let (shim_result, ()) = tokio::join!(shim_fut, peer_fut);
    match shim_result {
        Err(ProtocolError::BadHelloMagic { .. }) => {}
        Ok(_) => panic!("expected Err(ProtocolError::BadHelloMagic), got Ok"),
        Err(other) => panic!("expected ProtocolError::BadHelloMagic, got {other}"),
    }
}

// ---------------------------------------------------------------------------------------
// "reached_tai_ns != until_tai_ns from the peer is surfaced as a protocol error."
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn reached_tai_ns_mismatch_from_the_peer_is_a_protocol_error() {
    let (shim, peer) = UnixStream::pair().unwrap();
    let (link, mut peer) = handshake_ok(shim, peer).await;

    let request = LockstepStepRequest { sequence: 1, until_tai_ns: 1_000_000_000, inputs: vec![] };
    let step_fut = link.step(request.clone());
    let peer_fut = async {
        let (frame_type, payload) = framing::read_frame(&mut peer).await.unwrap();
        assert_eq!(frame_type, FrameType::Step);
        let req = LockstepStepRequest::decode(payload.as_slice()).unwrap();
        // Lies about reached_tai_ns -- lockstep.proto's own doc comment: "anything other
        // than until_tai_ns is a protocol error."
        let response = LockstepStepResponse { sequence: req.sequence, reached_tai_ns: req.until_tai_ns + 1, outputs: vec![], named_outputs: Default::default() };
        framing::write_frame(&mut peer, FrameType::StepDone, &response.encode_to_vec()).await.unwrap();
    };
    let (result, ()) = tokio::join!(step_fut, peer_fut);

    match result {
        Err(ProtocolError::ReachedTaiMismatch { until, reached }) => {
            assert_eq!(until, 1_000_000_000);
            assert_eq!(reached, 1_000_000_001);
        }
        other => panic!("expected ProtocolError::ReachedTaiMismatch, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------------------
// "A sequence-number mismatch in each direction is a typed error naming both values" --
// the two directions this proto defines an echoed sequence for: Step and Reset.
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn sequence_mismatch_on_step_is_a_typed_error_naming_both_values() {
    let (shim, peer) = UnixStream::pair().unwrap();
    let (link, mut peer) = handshake_ok(shim, peer).await;

    let request = LockstepStepRequest { sequence: 5, until_tai_ns: 1_000_000_000, inputs: vec![] };
    let step_fut = link.step(request.clone());
    let peer_fut = async {
        let (frame_type, payload) = framing::read_frame(&mut peer).await.unwrap();
        assert_eq!(frame_type, FrameType::Step);
        let req = LockstepStepRequest::decode(payload.as_slice()).unwrap();
        // Echoes the wrong sequence back.
        let response = LockstepStepResponse { sequence: req.sequence + 1, reached_tai_ns: req.until_tai_ns, outputs: vec![], named_outputs: Default::default() };
        framing::write_frame(&mut peer, FrameType::StepDone, &response.encode_to_vec()).await.unwrap();
    };
    let (result, ()) = tokio::join!(step_fut, peer_fut);

    match result {
        Err(ProtocolError::SequenceMismatch { op, sent, echoed }) => {
            assert_eq!(op, "step");
            assert_eq!(sent, 5);
            assert_eq!(echoed, 6);
        }
        other => panic!("expected ProtocolError::SequenceMismatch, got {other:?}"),
    }
}

#[tokio::test]
async fn sequence_mismatch_on_reset_is_a_typed_error_naming_both_values() {
    let (shim, peer) = UnixStream::pair().unwrap();
    let (link, mut peer) = handshake_ok(shim, peer).await;

    let request = LockstepResetRequest { sequence: 9, tai_ns: 3_000_000_000, reason: "power_cycle".to_string() };
    let reset_fut = link.reset(request.clone());
    let peer_fut = async {
        let (frame_type, payload) = framing::read_frame(&mut peer).await.unwrap();
        assert_eq!(frame_type, FrameType::Reset);
        let req = LockstepResetRequest::decode(payload.as_slice()).unwrap();
        let response = LockstepResetResponse { sequence: req.sequence + 100 };
        framing::write_frame(&mut peer, FrameType::ResetAck, &response.encode_to_vec()).await.unwrap();
    };
    let (result, ()) = tokio::join!(reset_fut, peer_fut);

    match result {
        Err(ProtocolError::SequenceMismatch { op, sent, echoed }) => {
            assert_eq!(op, "reset");
            assert_eq!(sent, 9);
            assert_eq!(echoed, 109);
        }
        other => panic!("expected ProtocolError::SequenceMismatch, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------------------
// "Exactly one outstanding step -- a second step before the first is answered is a
// protocol error, not a queue."
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn a_second_step_while_one_is_outstanding_is_a_protocol_error_not_a_queue() {
    let (shim, peer) = UnixStream::pair().unwrap();
    let (link, mut peer) = handshake_ok(shim, peer).await;
    let link = Arc::new(link);

    // The first Step is sent by a background task; the fake peer (driven directly by
    // this test, below) reads the resulting STEP frame but deliberately does not answer
    // it yet -- it stays outstanding until this test explicitly completes it further
    // down. `read_frame` below only returns once the spawned task has actually written
    // the frame, so this has no race: it is not possible to observe the second call's
    // rejection before the first call is genuinely outstanding.
    let first = {
        let link = Arc::clone(&link);
        tokio::spawn(async move { link.step(LockstepStepRequest { sequence: 1, until_tai_ns: 1_000_000_000, inputs: vec![] }).await })
    };
    let (frame_type, payload) = framing::read_frame(&mut peer).await.unwrap();
    assert_eq!(frame_type, FrameType::Step);
    assert_eq!(LockstepStepRequest::decode(payload.as_slice()).unwrap().sequence, 1);

    // The second Step, sequence 2, arrives while sequence 1 is still outstanding -- and
    // must be rejected immediately (no queueing): if it queued, this `.await` would hang
    // forever, since the fake peer above has not answered sequence 1's STEP yet.
    let second_result = link.step(LockstepStepRequest { sequence: 2, until_tai_ns: 2_000_000_000, inputs: vec![] }).await;
    match second_result {
        Err(ProtocolError::StepAlreadyOutstanding { outstanding_sequence, attempted_sequence }) => {
            assert_eq!(outstanding_sequence, 1);
            assert_eq!(attempted_sequence, 2);
        }
        other => panic!("expected ProtocolError::StepAlreadyOutstanding, got {other:?}"),
    }

    // Now let the first Step actually complete, proving the flag correctly clears and
    // the connection is not left wedged by the rejected second attempt.
    framing::write_frame(&mut peer, FrameType::StepDone, &LockstepStepResponse { sequence: 1, reached_tai_ns: 1_000_000_000, outputs: vec![], named_outputs: Default::default() }.encode_to_vec())
        .await
        .unwrap();
    let first_result = first.await.unwrap();
    assert!(first_result.is_ok(), "{first_result:?}");

    // And a third Step, after the first has fully completed, must succeed normally --
    // the earlier rejection must not have left `step_outstanding` stuck `true`.
    let third_fut = link.step(LockstepStepRequest { sequence: 3, until_tai_ns: 3_000_000_000, inputs: vec![] });
    let peer_fut = async {
        let (frame_type, payload) = framing::read_frame(&mut peer).await.unwrap();
        assert_eq!(frame_type, FrameType::Step);
        let req = LockstepStepRequest::decode(payload.as_slice()).unwrap();
        let response = LockstepStepResponse { sequence: req.sequence, reached_tai_ns: req.until_tai_ns, outputs: vec![], named_outputs: Default::default() };
        framing::write_frame(&mut peer, FrameType::StepDone, &response.encode_to_vec()).await.unwrap();
    };
    let (third_result, ()) = tokio::join!(third_fut, peer_fut);
    assert!(third_result.is_ok(), "{third_result:?}");
}
