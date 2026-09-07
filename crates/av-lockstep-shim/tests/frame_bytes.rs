//! Hand-pinned byte tests for "lockstep-local v1" (M23.1 brief: "a hand-pinned frame test:
//! at least one test asserting a complete frame's exact bytes, hand-computed from your
//! documented layout, **not** round-tripped through your own encoder").
//!
//! Every `expected` byte array below was derived independently of `src/framing.rs`'s own
//! `encode_frame`/`encode_hello`/`encode_error` -- by hand, from the documented header
//! layout (`[length: u32 LE][frame_type: u8][payload]`, length excludes itself) and the
//! standard protobuf wire format (varint field tags/values, length-delimited strings) for
//! the frames whose payload is a real `av_cdm::pb` message. The derivation for each test is
//! shown in that test's own comment so a reviewer can re-derive it without running
//! anything. This is what `encode_frame`/`encode_hello`/`encode_error` are checked
//! *against*; calling them and asserting the result equals itself (a round trip) would not
//! catch a self-consistent wrong layout -- the exact gap M22.3's CCSDS work found and
//! closed, named directly in this task's brief.
//!
//! What each test fails against (a wrong implementation that would still pass a pure
//! round-trip test): a byte reordered within the header, big-endian used instead of
//! little-endian anywhere, the length field including itself (off-by-four), a frame_type
//! code transposed with another, or (for `LockstepShutdownRequest`) treating `run_id` as
//! any field number/wire type other than the one `lockstep.proto` actually declares.
use av_cdm::pb::LockstepShutdownRequest;
use av_lockstep_shim::framing::{encode_error, encode_frame, encode_hello, ErrorCode, FrameType};
use prost::Message;

/// `HELLO` frame: magic `"AVL1"` + `protocol_version = 1` (u16 LE).
///
/// Derivation:
/// - payload = `[0x41, 0x56, 0x4C, 0x31]` (ASCII "AVL1") ++ `[0x01, 0x00]` (1u16 LE) = 6 bytes
/// - frame_type = `HELLO` = `0x01`
/// - length = 1 (frame_type byte) + 6 (payload) = 7 -> LE u32 = `[0x07, 0x00, 0x00, 0x00]`
/// - frame = length ++ frame_type ++ payload
#[test]
fn hello_frame_matches_hand_computed_bytes() {
    let expected: [u8; 11] = [0x07, 0x00, 0x00, 0x00, 0x01, 0x41, 0x56, 0x4C, 0x31, 0x01, 0x00];
    let actual = encode_frame(FrameType::Hello, &encode_hello(1));
    assert_eq!(actual, expected, "actual={actual:02x?}");
}

/// `SHUTDOWN` frame carrying `LockstepShutdownRequest { run_id: "run-1" }`.
///
/// Derivation (protobuf wire format, `lockstep.proto`'s `LockstepShutdownRequest.run_id`
/// is field 1, type `string`):
/// - tag byte = `(field_number << 3) | wire_type` = `(1 << 3) | 2` (length-delimited) = `0x0A`
/// - length of "run-1" (5 ASCII bytes) as a varint = `0x05`
/// - "run-1" bytes = `0x72 0x75 0x6E 0x2D 0x31` ('r' 'u' 'n' '-' '1')
/// - protobuf payload = `[0x0A, 0x05, 0x72, 0x75, 0x6E, 0x2D, 0x31]` (7 bytes)
/// - frame_type = `SHUTDOWN` = `0x08`
/// - length = 1 + 7 = 8 -> LE u32 = `[0x08, 0x00, 0x00, 0x00]`
#[test]
fn shutdown_frame_matches_hand_computed_bytes() {
    let expected: [u8; 12] = [0x08, 0x00, 0x00, 0x00, 0x08, 0x0A, 0x05, 0x72, 0x75, 0x6E, 0x2D, 0x31];
    let request = LockstepShutdownRequest { run_id: "run-1".to_string() };
    let actual = encode_frame(FrameType::Shutdown, &request.encode_to_vec());
    assert_eq!(actual, expected, "actual={actual:02x?}");
}

/// `ERROR` frame: `SequenceMismatch`, expected(sent)=5, actual(echoed)=7, message
/// `"seq mismatch"` (this module's own hand-rolled, non-protobuf layout).
///
/// Derivation:
/// - code = `SEQUENCE_MISMATCH` = `0x02`
/// - expected = `5i64` LE = `[0x05, 0, 0, 0, 0, 0, 0, 0]`
/// - actual = `7i64` LE = `[0x07, 0, 0, 0, 0, 0, 0, 0]`
/// - message = "seq mismatch", 12 ASCII bytes; message_len = `12u16` LE = `[0x0C, 0x00]`
/// - payload = code ++ expected ++ actual ++ message_len ++ message = 1+8+8+2+12 = 31 bytes
/// - frame_type = `ERROR` = `0xFF`
/// - length = 1 + 31 = 32 -> LE u32 = `[0x20, 0x00, 0x00, 0x00]`
#[test]
fn error_frame_matches_hand_computed_bytes() {
    #[rustfmt::skip]
    let expected: [u8; 36] = [
        0x20, 0x00, 0x00, 0x00, // length = 32
        0xFF,                   // frame_type = ERROR
        0x02,                   // code = SEQUENCE_MISMATCH
        0x05, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // expected = 5i64 LE
        0x07, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // actual = 7i64 LE
        0x0C, 0x00,             // message_len = 12u16 LE
        b's', b'e', b'q', b' ', b'm', b'i', b's', b'm', b'a', b't', b'c', b'h',
    ];
    let payload = encode_error(ErrorCode::SequenceMismatch, 5, 7, "seq mismatch");
    let actual = encode_frame(FrameType::Error, &payload);
    assert_eq!(actual, expected, "actual={actual:02x?}");
}

/// `STEP` frame carrying `LockstepStepRequest { sequence: 1, until_tai_ns: 1_000_000_000,
/// inputs: [PortMessage { port: "in", payload: <2.0f64 LE> }] }` -- the frame type most
/// directly answering the brief's "how a PortMessage is carried" question: embedded,
/// unmodified, as the parent message's own protobuf field (`lockstep.proto`'s
/// `LockstepStepRequest.inputs`), not re-encoded into any format of this module's own.
///
/// Derivation (protobuf wire format; field numbers from `lockstep.proto`:
/// `LockstepStepRequest{sequence=1(uint64), until_tai_ns=2(int64), inputs=3(repeated
/// PortMessage)}`, `PortMessage{port=1(string), tai_ns=2(int64), payload=3(bytes)}` --
/// `tai_ns` is the proto3 default `0` here and so is omitted entirely, the same rule
/// `prost` itself follows for proto3 scalar fields):
/// - `PortMessage.port` = "in": tag `(1<<3)|2=0x0A`, len `0x02`, bytes `0x69 0x6E` ("in")
/// - `PortMessage.payload` = 2.0f64 LE = `00 00 00 00 00 00 00 40`: tag `(3<<3)|2=0x1A`,
///   len `0x08`, then those 8 bytes
/// - `PortMessage` bytes = `0A 02 69 6E 1A 08 00 00 00 00 00 00 00 40` (14 bytes)
/// - `LockstepStepRequest.sequence` = 1: tag `(1<<3)|0=0x08` (varint), value `0x01`
/// - `LockstepStepRequest.until_tai_ns` = 1_000_000_000: tag `(2<<3)|0=0x10` (varint),
///   value as a varint = `80 94 EB DC 03` (1_000_000_000 = 0x3B9ACA00; split into 7-bit
///   groups LSB-first with the continuation bit set on every group but the last)
/// - `LockstepStepRequest.inputs[0]` = the `PortMessage` above: tag `(3<<3)|2=0x1A`
///   (length-delimited), len `0x0E` (14), then the 14 `PortMessage` bytes
/// - `LockstepStepRequest` bytes = `08 01 10 80 94 EB DC 03 1A 0E <14 PortMessage bytes>`
///   = 2 + 6 + 2 + 14 = 24 bytes
/// - frame_type = `STEP` = `0x04`; length = 1 + 24 = 25 -> LE u32 = `19 00 00 00`
#[test]
fn step_frame_with_a_port_message_matches_hand_computed_bytes() {
    #[rustfmt::skip]
    let expected: [u8; 29] = [
        0x19, 0x00, 0x00, 0x00, // length = 25
        0x04,                   // frame_type = STEP
        0x08, 0x01,             // sequence = 1 (varint)
        0x10, 0x80, 0x94, 0xEB, 0xDC, 0x03, // until_tai_ns = 1_000_000_000 (varint)
        0x1A, 0x0E,             // inputs[0]: length-delimited, 14 bytes
        0x0A, 0x02, b'i', b'n', // PortMessage.port = "in"
        0x1A, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x40, // PortMessage.payload = 2.0f64 LE
    ];

    let port_message = av_cdm::pb::PortMessage { port: "in".to_string(), tai_ns: 0, payload: 2.0f64.to_le_bytes().to_vec() };
    let request = av_cdm::pb::LockstepStepRequest {
        sequence: 1,
        until_tai_ns: 1_000_000_000,
        inputs: vec![port_message],
    };
    let actual = encode_frame(FrameType::Step, &request.encode_to_vec());
    assert_eq!(actual, expected, "actual={actual:02x?}");
}
