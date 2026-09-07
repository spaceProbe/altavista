//! "lockstep-local v1": the length-prefixed frame protocol this shim speaks over a Unix
//! socket to whatever flight software is bound (M23.1, `docs/open-questions.md` question
//! 153). The byte-exact layout is documented in `services/cfs/README.md`; this module is
//! that document's one implementation, and `tests/frame_bytes.rs`'s hand-pinned tests pin
//! this module's output against bytes computed independently from the documented spec (not
//! round-tripped through this module's own encoder -- see that file's module doc comment
//! for why that distinction matters).
//!
//! ## Summary (see the README for the full account)
//!
//! Every frame is `[length: u32 LE][frame_type: u8][payload: (length - 1) bytes]`.
//! `length` does **not** include itself -- it is `1 + payload.len()`. Little-endian
//! throughout, deliberately: this channel never leaves one host (unlike this platform's
//! CCSDS framing elsewhere, which is big-endian on the wire because it is designed to
//! cross a real RF/network link) and its own `PortMessage.payload` SIGNAL encoding is
//! already little-endian (`lockstep.proto`'s own doc comment, matched by
//! `av_dynamics::encode_signal` and `lockstep_ref.server.encode_signal`) -- see the README
//! for the rest of this reasoning.
//!
//! `HELLO` and `ERROR` payloads are this module's own small hand-rolled binary layouts
//! (`proto/altavista/v1/lockstep.proto` is read-only and has no messages for either).
//! Every other frame's payload is the unmodified `prost::Message::encode_to_vec()` of the
//! matching `av_cdm::pb::Lockstep*`/`PortMessage` type already defined by the proto --
//! `PortMessage` is carried exactly as `lockstep.proto` already carries it, embedded in its
//! parent's `inputs`/`outputs` field, with no re-encoding of its own.
use std::fmt;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// "lockstep-local v1" -- bumped whenever a wire-incompatible change is made to this
/// module's framing or any payload layout. `PeerLink::handshake` refuses to proceed past a
/// mismatch (see that module).
pub const PROTOCOL_VERSION: u16 = 1;

/// The `HELLO` frame's magic: four ASCII bytes, deliberately human-legible in a hex dump
/// (`hexdump -C` prints `41 56 4c 31  |AVL1|`) rather than an opaque integer constant.
pub const HELLO_MAGIC: [u8; 4] = *b"AVL1";

/// An upper bound on one frame's total on-wire length (the `length` field's own declared
/// value, i.e. `1 + payload.len()`). Not part of the protocol's own contract with a peer --
/// a real bound process should never need a single step's inputs/outputs to approach this --
/// but `read_frame` enforces it so a garbled or malicious `length` field fails as a typed
/// [`FrameError::TooLarge`] instead of driving an unbounded allocation. 16 MiB is generous
/// for any one step's batch of `PortMessage`s in this platform's declared use cases while
/// still catching an obviously-corrupt length (a flipped byte order, a stray frame_type
/// byte misread as part of the length) long before it becomes a multi-gigabyte `Vec`.
pub const MAX_FRAME_LEN: u32 = 16 * 1024 * 1024;

/// One lockstep-local v1 frame kind. `code()`/`from_code()` are this enum's own wire
/// encoding (a single byte -- endianness does not apply to a one-byte field).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FrameType {
    /// Version handshake, sent by the shim immediately after accepting the peer's
    /// connection (see `PeerLink::handshake`'s doc comment for why the shim speaks
    /// first), and expected back once from the peer.
    Hello,
    /// Shim -> peer: payload is a `LockstepBindRequest`.
    Bind,
    /// Peer -> shim: payload is a `LockstepBindResponse`.
    BindAck,
    /// Shim -> peer: payload is a `LockstepStepRequest`.
    Step,
    /// Peer -> shim: payload is a `LockstepStepResponse`.
    StepDone,
    /// Shim -> peer: payload is a `LockstepResetRequest`.
    Reset,
    /// Peer -> shim: payload is a `LockstepResetResponse`.
    ResetAck,
    /// Shim -> peer: payload is a `LockstepShutdownRequest`.
    Shutdown,
    /// Peer -> shim: payload is a `LockstepShutdownResponse`.
    ShutdownAck,
    /// Either direction: a typed protocol error, sent instead of the expected response/ack
    /// so the receiver fails loudly rather than misparsing whatever comes next as if it
    /// were the expected frame.
    Error,
}

impl FrameType {
    pub const fn code(self) -> u8 {
        match self {
            FrameType::Hello => 0x01,
            FrameType::Bind => 0x02,
            FrameType::BindAck => 0x03,
            FrameType::Step => 0x04,
            FrameType::StepDone => 0x05,
            FrameType::Reset => 0x06,
            FrameType::ResetAck => 0x07,
            FrameType::Shutdown => 0x08,
            FrameType::ShutdownAck => 0x09,
            FrameType::Error => 0xFF,
        }
    }

    pub const fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            0x01 => FrameType::Hello,
            0x02 => FrameType::Bind,
            0x03 => FrameType::BindAck,
            0x04 => FrameType::Step,
            0x05 => FrameType::StepDone,
            0x06 => FrameType::Reset,
            0x07 => FrameType::ResetAck,
            0x08 => FrameType::Shutdown,
            0x09 => FrameType::ShutdownAck,
            0xFF => FrameType::Error,
            _ => return None,
        })
    }
}

impl fmt::Display for FrameType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            FrameType::Hello => "HELLO",
            FrameType::Bind => "BIND",
            FrameType::BindAck => "BIND_ACK",
            FrameType::Step => "STEP",
            FrameType::StepDone => "STEP_DONE",
            FrameType::Reset => "RESET",
            FrameType::ResetAck => "RESET_ACK",
            FrameType::Shutdown => "SHUTDOWN",
            FrameType::ShutdownAck => "SHUTDOWN_ACK",
            FrameType::Error => "ERROR",
        };
        write!(f, "{name}(0x{:02X})", self.code())
    }
}

/// Everything that can go wrong at the framing layer itself (below `PeerLink`'s own
/// protocol-semantics errors in `peer_link.rs`) -- a malformed frame, never a silent
/// retry or a best-effort reparse.
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("frame declares length {got} bytes, over the {max} byte limit")]
    TooLarge { max: u32, got: u32 },
    #[error("unknown frame type byte 0x{0:02x}")]
    UnknownFrameType(u8),
    #[error("truncated frame: declared length {declared} but the payload was incomplete")]
    Truncated { declared: u32 },
    #[error("the peer closed the connection cleanly between frames")]
    PeerClosed,
    #[error("malformed HELLO payload: {0}")]
    MalformedHello(String),
    #[error("malformed ERROR payload: {0}")]
    MalformedError(String),
    #[error("I/O error on the lockstep-local socket: {0}")]
    Io(#[from] std::io::Error),
}

/// Pure, synchronous frame encoder -- no I/O. This is the one function the hand-pinned
/// tests in `tests/frame_bytes.rs` check bytes *against*; those tests' own expected byte
/// arrays are computed independently (by hand, from the documented protobuf wire format
/// plus this module's own header layout), never produced by calling this function and
/// asserting a round trip.
pub fn encode_frame(frame_type: FrameType, payload: &[u8]) -> Vec<u8> {
    let length: u32 = 1u32
        .checked_add(payload.len().try_into().expect("payload larger than u32::MAX"))
        .expect("frame length overflows u32");
    let mut out = Vec::with_capacity(4 + payload.len() + 1);
    out.extend_from_slice(&length.to_le_bytes());
    out.push(frame_type.code());
    out.extend_from_slice(payload);
    out
}

/// Write one frame and flush -- the async counterpart to [`encode_frame`], used by every
/// real caller (tests included) that actually owns a socket.
pub async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, frame_type: FrameType, payload: &[u8]) -> Result<(), FrameError> {
    let buf = encode_frame(frame_type, payload);
    w.write_all(&buf).await?;
    w.flush().await?;
    Ok(())
}

/// Read exactly one frame. A clean disconnect between frames (EOF on the very first byte
/// of the length prefix) is [`FrameError::PeerClosed`]; an EOF partway through a frame
/// (a dropped/mis-ordered frame, never silently retried) is [`FrameError::Truncated`].
pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> Result<(FrameType, Vec<u8>), FrameError> {
    let mut len_buf = [0u8; 4];
    match r.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Err(FrameError::PeerClosed),
        Err(e) => return Err(FrameError::Io(e)),
    }
    let length = u32::from_le_bytes(len_buf);
    if length == 0 {
        // Every frame carries at least the one frame_type byte.
        return Err(FrameError::Truncated { declared: 0 });
    }
    if length > MAX_FRAME_LEN {
        return Err(FrameError::TooLarge { max: MAX_FRAME_LEN, got: length });
    }
    let mut rest = vec![0u8; length as usize];
    r.read_exact(&mut rest).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            FrameError::Truncated { declared: length }
        } else {
            FrameError::Io(e)
        }
    })?;
    let frame_type = FrameType::from_code(rest[0]).ok_or(FrameError::UnknownFrameType(rest[0]))?;
    Ok((frame_type, rest[1..].to_vec()))
}

/// `HELLO` payload: `[magic: 4 bytes]["AVL1"][version: u16 LE]`. Not protobuf -- see this
/// module's doc comment for why (no proto message exists for the handshake, and adding one
/// would mean editing the read-only `proto/` tree).
pub fn encode_hello(version: u16) -> Vec<u8> {
    let mut out = Vec::with_capacity(6);
    out.extend_from_slice(&HELLO_MAGIC);
    out.extend_from_slice(&version.to_le_bytes());
    out
}

pub fn decode_hello(payload: &[u8]) -> Result<([u8; 4], u16), FrameError> {
    if payload.len() != 6 {
        return Err(FrameError::MalformedHello(format!("expected exactly 6 bytes, got {}", payload.len())));
    }
    let mut magic = [0u8; 4];
    magic.copy_from_slice(&payload[0..4]);
    let version = u16::from_le_bytes([payload[4], payload[5]]);
    Ok((magic, version))
}

/// `ERROR` payload's own typed reason code -- distinct from [`FrameType`] (that is the
/// outer frame's kind; this is what went wrong *inside* an `ERROR` frame).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    VersionMismatch,
    BadHelloMagic,
    SequenceMismatch,
    StepAlreadyOutstanding,
    ReachedTaiMismatch,
    UnexpectedFrameType,
    MalformedFrame,
    Other,
}

impl ErrorCode {
    pub const fn code(self) -> u8 {
        match self {
            ErrorCode::VersionMismatch => 1,
            ErrorCode::SequenceMismatch => 2,
            ErrorCode::StepAlreadyOutstanding => 3,
            ErrorCode::ReachedTaiMismatch => 4,
            ErrorCode::UnexpectedFrameType => 5,
            ErrorCode::MalformedFrame => 6,
            ErrorCode::BadHelloMagic => 7,
            ErrorCode::Other => 255,
        }
    }

    pub const fn from_code(code: u8) -> Self {
        match code {
            1 => ErrorCode::VersionMismatch,
            2 => ErrorCode::SequenceMismatch,
            3 => ErrorCode::StepAlreadyOutstanding,
            4 => ErrorCode::ReachedTaiMismatch,
            5 => ErrorCode::UnexpectedFrameType,
            6 => ErrorCode::MalformedFrame,
            7 => ErrorCode::BadHelloMagic,
            _ => ErrorCode::Other,
        }
    }
}

/// `ERROR` payload: `[code: u8][expected: i64 LE][actual: i64 LE][message_len: u16 LE]
/// [message: message_len UTF-8 bytes]`. `expected`/`actual` are `0` when a variant has no
/// natural pair of values to name (e.g. [`ErrorCode::BadHelloMagic`], whose two byte
/// strings are described in `message` instead, not force-fit into two `i64`s).
pub fn encode_error(code: ErrorCode, expected: i64, actual: i64, message: &str) -> Vec<u8> {
    let msg_bytes = message.as_bytes();
    let msg_len: u16 = msg_bytes.len().try_into().expect("error message longer than u16::MAX bytes");
    let mut out = Vec::with_capacity(1 + 8 + 8 + 2 + msg_bytes.len());
    out.push(code.code());
    out.extend_from_slice(&expected.to_le_bytes());
    out.extend_from_slice(&actual.to_le_bytes());
    out.extend_from_slice(&msg_len.to_le_bytes());
    out.extend_from_slice(msg_bytes);
    out
}

pub fn decode_error(payload: &[u8]) -> Result<(ErrorCode, i64, i64, String), FrameError> {
    const HEADER_LEN: usize = 1 + 8 + 8 + 2;
    if payload.len() < HEADER_LEN {
        return Err(FrameError::MalformedError(format!("payload is only {} bytes, need at least {HEADER_LEN}", payload.len())));
    }
    let code = ErrorCode::from_code(payload[0]);
    let expected = i64::from_le_bytes(payload[1..9].try_into().unwrap());
    let actual = i64::from_le_bytes(payload[9..17].try_into().unwrap());
    let msg_len = u16::from_le_bytes([payload[17], payload[18]]) as usize;
    let msg_bytes = payload
        .get(HEADER_LEN..HEADER_LEN + msg_len)
        .ok_or_else(|| FrameError::MalformedError(format!("declared message_len {msg_len} runs past the payload's {} bytes", payload.len())))?;
    let message = String::from_utf8_lossy(msg_bytes).into_owned();
    Ok((code, expected, actual, message))
}
