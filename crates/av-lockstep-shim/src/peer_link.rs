//! `PeerLink`: the shim's side of a "lockstep-local v1" connection to one bound
//! flight-software peer (M23.1). Owns the Unix socket, performs the version handshake, and
//! implements `bind`/`step`/`reset`/`shutdown` as request-then-response exchanges over it --
//! independent of `tonic`/gRPC entirely, so it can be (and is, in `tests/`) driven directly
//! against an in-memory `UnixStream::pair()` "fake peer" without spinning up a real gRPC
//! server, a real filesystem socket, or the Python reference peer.
//!
//! ## Enforcement this module owns
//!
//! - **Exactly one outstanding step** (`step_outstanding`, an `AtomicBool`): a second
//!   `step()` call while the first has not yet returned gets an immediate
//!   [`ProtocolError::StepAlreadyOutstanding`] -- it never touches the socket, so it can
//!   never queue behind or interleave with the first call's own frame exchange.
//! - **Sequence numbers checked in both directions this proto defines an echo for** --
//!   `Step`'s and `Reset`'s responses both carry back the `sequence` the request carried;
//!   a mismatch is [`ProtocolError::SequenceMismatch`], naming both the sent and echoed
//!   values.
//! - **`reached_tai_ns` must equal `until_tai_ns`** (`lockstep.proto`'s own doc comment:
//!   "anything else is a protocol error") -- checked here, not left to the caller.
//! - **A version-handshake mismatch fails loudly**: [`PeerLink::handshake`] sends the peer
//!   an `ERROR` frame and returns `Err` rather than proceeding to `Bind` with a peer that
//!   may not parse this shim's frames correctly.
//!
//! ## Who speaks first
//!
//! The shim always **listens** on the Unix socket (see `src/bin/av-lockstep-shim.rs`) and
//! the flight-software peer connects to it -- mirroring the real deployment, where the
//! shim is started by the container/orchestrator first and the flight software's I/O app
//! connects to an address it is handed at boot (and, in M24, the Renode bridge process
//! plays the identical "already listening" role). Given that, the shim also **speaks
//! first** once a connection is accepted: it sends its own `HELLO` immediately rather than
//! waiting on a peer that might otherwise be waiting on it too. See
//! `services/cfs/README.md` for the full rationale.
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use av_cdm::pb::{LockstepBindRequest, LockstepBindResponse, LockstepResetRequest, LockstepResetResponse, LockstepShutdownRequest, LockstepShutdownResponse, LockstepStepRequest, LockstepStepResponse};
use prost::Message;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::Mutex;

use crate::framing::{self, decode_error, decode_hello, encode_error, encode_hello, ErrorCode, FrameError, FrameType, HELLO_MAGIC, PROTOCOL_VERSION};

/// Everything that can go wrong on a lockstep-local v1 connection above the raw framing
/// layer (see [`FrameError`] for below it). Every variant names the concrete values
/// involved -- never a bare "protocol error" string -- so a caller (and a test) can match
/// on exactly what went wrong.
#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error(transparent)]
    Frame(#[from] FrameError),
    #[error("decoding a protobuf payload: {0}")]
    Decode(#[from] prost::DecodeError),
    #[error("version handshake mismatch: this shim speaks lockstep-local v{ours}, the peer sent v{peer}")]
    VersionMismatch { ours: u16, peer: u16 },
    #[error("bad HELLO magic: expected {expected:02x?}, got {got:02x?}")]
    BadHelloMagic { expected: [u8; 4], got: [u8; 4] },
    #[error("sequence mismatch on {op}: this shim sent sequence {sent}, the peer echoed {echoed}")]
    SequenceMismatch { op: &'static str, sent: u64, echoed: u64 },
    #[error("reached_tai_ns mismatch: requested until_tai_ns={until}, the peer reported reached_tai_ns={reached}")]
    ReachedTaiMismatch { until: i64, reached: i64 },
    #[error("a Step (sequence {outstanding_sequence}) is already outstanding; a second Step (sequence {attempted_sequence}) before the first is answered is a protocol error, not a queue")]
    StepAlreadyOutstanding { outstanding_sequence: u64, attempted_sequence: u64 },
    #[error("expected a {expected} frame from the peer, got {got}")]
    UnexpectedFrameType { expected: FrameType, got: FrameType },
    #[error("the peer reported a protocol error: {0}")]
    PeerError(String),
}

/// Best-effort: send `err` to the peer as an `ERROR` frame before this side gives up on the
/// connection. Failure to even send the error frame is deliberately swallowed here -- the
/// caller is already about to return the original `err`, and a broken socket that can't
/// carry an outgoing `ERROR` frame either isn't going to carry anything else either; the
/// original typed error is what matters to the caller, not whether the best-effort notice
/// made it out.
async fn send_error<W: AsyncWrite + Unpin>(w: &mut W, err: &ProtocolError) {
    let (code, expected, actual) = match err {
        ProtocolError::VersionMismatch { ours, peer } => (ErrorCode::VersionMismatch, i64::from(*ours), i64::from(*peer)),
        ProtocolError::BadHelloMagic { .. } => (ErrorCode::BadHelloMagic, 0, 0),
        ProtocolError::SequenceMismatch { sent, echoed, .. } => (ErrorCode::SequenceMismatch, *sent as i64, *echoed as i64),
        ProtocolError::StepAlreadyOutstanding { outstanding_sequence, attempted_sequence } => (ErrorCode::StepAlreadyOutstanding, *outstanding_sequence as i64, *attempted_sequence as i64),
        ProtocolError::ReachedTaiMismatch { until, reached } => (ErrorCode::ReachedTaiMismatch, *until, *reached),
        ProtocolError::UnexpectedFrameType { expected, got } => (ErrorCode::UnexpectedFrameType, i64::from(expected.code()), i64::from(got.code())),
        ProtocolError::Frame(_) | ProtocolError::Decode(_) => (ErrorCode::MalformedFrame, 0, 0),
        ProtocolError::PeerError(_) => (ErrorCode::Other, 0, 0),
    };
    let payload = encode_error(code, expected, actual, &err.to_string());
    let _ = framing::write_frame(w, FrameType::Error, &payload).await;
}

fn decode_peer_error(payload: &[u8]) -> ProtocolError {
    match decode_error(payload) {
        Ok((code, expected, actual, message)) => ProtocolError::PeerError(format!("{message} (code={code:?}, expected={expected}, actual={actual})")),
        Err(e) => ProtocolError::PeerError(format!("(and the ERROR frame itself was malformed: {e})")),
    }
}

/// One live lockstep-local v1 connection to a bound peer, past the version handshake.
pub struct PeerLink<S> {
    stream: Mutex<S>,
    step_outstanding: AtomicBool,
    outstanding_step_sequence: AtomicU64,
}

impl<S: AsyncRead + AsyncWrite + Unpin> PeerLink<S> {
    /// Performs the lockstep-local v1 handshake (shim speaks first -- see this module's
    /// doc comment) and returns a ready `PeerLink`, or the typed mismatch after having
    /// already sent the peer an `ERROR` frame explaining why.
    pub async fn handshake(mut stream: S) -> Result<Self, ProtocolError> {
        framing::write_frame(&mut stream, FrameType::Hello, &encode_hello(PROTOCOL_VERSION)).await?;

        let (frame_type, payload) = framing::read_frame(&mut stream).await?;
        if frame_type != FrameType::Hello {
            let err = ProtocolError::UnexpectedFrameType { expected: FrameType::Hello, got: frame_type };
            send_error(&mut stream, &err).await;
            return Err(err);
        }
        let (magic, peer_version) = decode_hello(&payload)?;
        if magic != HELLO_MAGIC {
            let err = ProtocolError::BadHelloMagic { expected: HELLO_MAGIC, got: magic };
            send_error(&mut stream, &err).await;
            return Err(err);
        }
        if peer_version != PROTOCOL_VERSION {
            let err = ProtocolError::VersionMismatch { ours: PROTOCOL_VERSION, peer: peer_version };
            send_error(&mut stream, &err).await;
            return Err(err);
        }

        Ok(Self {
            stream: Mutex::new(stream),
            step_outstanding: AtomicBool::new(false),
            outstanding_step_sequence: AtomicU64::new(0),
        })
    }

    pub async fn bind(&self, request: LockstepBindRequest) -> Result<LockstepBindResponse, ProtocolError> {
        let mut stream = self.stream.lock().await;
        framing::write_frame(&mut *stream, FrameType::Bind, &request.encode_to_vec()).await?;
        let (frame_type, payload) = framing::read_frame(&mut *stream).await?;
        match frame_type {
            FrameType::BindAck => Ok(LockstepBindResponse::decode(payload.as_slice())?),
            FrameType::Error => Err(decode_peer_error(&payload)),
            got => Err(ProtocolError::UnexpectedFrameType { expected: FrameType::BindAck, got }),
        }
    }

    /// See this module's doc comment's "Enforcement this module owns" section: this is
    /// where "exactly one outstanding step" and both `Step`-related protocol checks live.
    pub async fn step(&self, request: LockstepStepRequest) -> Result<LockstepStepResponse, ProtocolError> {
        if self.step_outstanding.swap(true, Ordering::AcqRel) {
            let outstanding_sequence = self.outstanding_step_sequence.load(Ordering::Acquire);
            return Err(ProtocolError::StepAlreadyOutstanding { outstanding_sequence, attempted_sequence: request.sequence });
        }
        self.outstanding_step_sequence.store(request.sequence, Ordering::Release);

        // Always clears the flag on the way out, success or error -- a `step()` that
        // returned early on a protocol error must not leave the connection permanently
        // wedged believing a step is still outstanding.
        struct ClearOnDrop<'a>(&'a AtomicBool);
        impl Drop for ClearOnDrop<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Release);
            }
        }
        let _clear_on_drop = ClearOnDrop(&self.step_outstanding);

        let sent_sequence = request.sequence;
        let until_tai_ns = request.until_tai_ns;

        let mut stream = self.stream.lock().await;
        framing::write_frame(&mut *stream, FrameType::Step, &request.encode_to_vec()).await?;
        let (frame_type, payload) = framing::read_frame(&mut *stream).await?;
        let response = match frame_type {
            FrameType::StepDone => LockstepStepResponse::decode(payload.as_slice())?,
            FrameType::Error => return Err(decode_peer_error(&payload)),
            got => return Err(ProtocolError::UnexpectedFrameType { expected: FrameType::StepDone, got }),
        };

        if response.sequence != sent_sequence {
            return Err(ProtocolError::SequenceMismatch { op: "step", sent: sent_sequence, echoed: response.sequence });
        }
        // lockstep.proto's own doc comment: "reached_tai_ns must equal until_tai_ns;
        // anything else is a protocol error."
        if response.reached_tai_ns != until_tai_ns {
            return Err(ProtocolError::ReachedTaiMismatch { until: until_tai_ns, reached: response.reached_tai_ns });
        }
        Ok(response)
    }

    pub async fn reset(&self, request: LockstepResetRequest) -> Result<LockstepResetResponse, ProtocolError> {
        let sent_sequence = request.sequence;
        let mut stream = self.stream.lock().await;
        framing::write_frame(&mut *stream, FrameType::Reset, &request.encode_to_vec()).await?;
        let (frame_type, payload) = framing::read_frame(&mut *stream).await?;
        let response = match frame_type {
            FrameType::ResetAck => LockstepResetResponse::decode(payload.as_slice())?,
            FrameType::Error => return Err(decode_peer_error(&payload)),
            got => return Err(ProtocolError::UnexpectedFrameType { expected: FrameType::ResetAck, got }),
        };
        if response.sequence != sent_sequence {
            return Err(ProtocolError::SequenceMismatch { op: "reset", sent: sent_sequence, echoed: response.sequence });
        }
        Ok(response)
    }

    pub async fn shutdown(&self, request: LockstepShutdownRequest) -> Result<LockstepShutdownResponse, ProtocolError> {
        let mut stream = self.stream.lock().await;
        framing::write_frame(&mut *stream, FrameType::Shutdown, &request.encode_to_vec()).await?;
        let (frame_type, payload) = framing::read_frame(&mut *stream).await?;
        match frame_type {
            FrameType::ShutdownAck => Ok(LockstepShutdownResponse::decode(payload.as_slice())?),
            FrameType::Error => Err(decode_peer_error(&payload)),
            got => Err(ProtocolError::UnexpectedFrameType { expected: FrameType::ShutdownAck, got }),
        }
    }
}
