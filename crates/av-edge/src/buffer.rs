//! Milestone E6 (`docs/edge-plan.md`): "the edge buffers signed batches in a local file
//! log for hours, replays them in order on reconnect, and the ingest deduplicates by
//! (producer, sequence) so a replay never double-counts."
//!
//! This module owns the two halves that charter needs on the **edge** side (the ingest's
//! own dedup already exists and is untouched -- `crate::chain::ChainVerifier::submit`'s
//! `seen_sequences` check, `pb::BatchRejection::Duplicate`):
//!
//! - [`EdgeBuffer`] -- a durable, append-only, file-backed log of already-*signed*
//!   `pb::MeasurementBatch`es, plus a small sidecar file recording how far a reconnect has
//!   confirmed delivery (the "ack watermark").
//! - [`BatchSink`] / [`UplinkDriver`] -- the trait a caller submits batches through, and
//!   the pure state machine that buffers while a `BatchSink` reports the link down and
//!   replays the buffer in order on the first step after it comes back.
//!
//! Both live in this one file rather than two: [`UplinkDriver`] is a thin ~40-line state
//! machine expressed entirely in terms of [`EdgeBuffer`]'s own four public operations
//! (`append`/`replay_from`/`ack_watermark`/`persist_ack_watermark`), and every type below
//! is either tiny or already documented at length -- splitting them across files would add
//! a second module boundary with nothing on either side of it to actually separate.
//!
//! # Record framing -- mirrors `crates/av-ingest/src/log.rs::PartitionLog`, deliberately
//!
//! `av-edge` must never depend on `av-ingest` (that crate depends on this one; the reverse
//! would cycle -- this crate's own task brief), so this module cannot reuse `PartitionLog`
//! directly and instead duplicates its record layout on purpose, so a torn final record
//! (a power cut mid-`write`) is detected and excluded here exactly as it already is there.
//! Concretely, byte for byte, **identical** to `PartitionLog`'s own framing:
//!
//! ```text
//! +----------------------+----------------------------+---------------------------+
//! | payload_len: u32 LE  | record_hash: [u8; 32]      | payload: payload_len bytes|
//! | (4 bytes)            | (32 bytes, raw, not hex)   |                           |
//! +----------------------+----------------------------+---------------------------+
//! ```
//!
//! `payload` is `prost::Message::encode_to_vec` of the signed `MeasurementBatch` as a
//! whole (signature included); `record_hash` is `SHA-256(prev_record_hash || payload)`,
//! chained from [`hash::GENESIS`] for the first record -- via [`hash::compute_batch_hash`],
//! reused directly rather than re-implemented, because this module lives in the *same*
//! crate as `crate::hash` (no cycle to avoid here at all: `crate::hash` has no dependency
//! on anything transport- or filesystem-shaped). [`EdgeBuffer::open`] scans the whole file
//! from byte 0 and, exactly like `PartitionLog::open`, discards and reports (never
//! silently keeps) a trailing record that is either physically incomplete (fewer than 36
//! header bytes or fewer than the declared `payload_len` payload bytes remain) or
//! structurally complete but content-corrupt (stored hash disagrees with the recomputed
//! one, or the payload does not decode as a `MeasurementBatch`) -- see `scan_frames`'s own
//! doc comment, copied from `PartitionLog`'s identical function with only the module path
//! changed.
//!
//! # Deliberate deviations from `PartitionLog`, and why
//!
//! 1. **One file, not one-file-per-`shard_key`.** `PartitionLog` partitions by
//!    `shard_key` because the *ingest* interleaves many producers into shared partitions
//!    (spoore's scaling model). An edge buffer belongs to exactly one producer replaying
//!    its own chain to exactly one uplink, so [`EdgeBuffer::open`] takes a single `path`
//!    the caller names directly (a plugin's own config, not a shard key needing
//!    sanitisation against directory traversal -- there is no multi-tenant directory to
//!    escape here).
//! 2. **A second, small sidecar file for the ack watermark**, living at `path` with
//!    `.ack` appended to its file name ([`watermark_path_for`]). `PartitionLog` has no
//!    analogous concept -- the ingest never needs to remember "how much of my own output
//!    has some downstream reader confirmed", only what it itself has durably accepted.
//!    The edge side needs exactly that memory, so a reconnect does not have to resend a
//!    buffer's entire history every time (this module's own task brief) -- see
//!    "The ack watermark, and why a corrupt one degrades safely" below.
//! 3. **`replay_from` assumes one producer's monotonically increasing `sequence`.**
//!    `PartitionLog::verify` answers "is this file's own record chain intact"; this
//!    module's analogous walk ([`EdgeBuffer::replay_from`]) additionally decodes each
//!    record and filters by `MeasurementBatch.sequence`, because the watermark this module
//!    persists *is* a sequence number -- meaningful only because a single `EdgeBuffer`
//!    never mixes more than one producer's chain (unlike a partition file, which routinely
//!    does).
//!
//! # The ack watermark, and why a corrupt one degrades safely
//!
//! [`EdgeBuffer::persist_ack_watermark`] overwrites the sidecar file with exactly 8 bytes
//! (a little-endian `u64`), `sync_all`ed before returning -- the same durability contract
//! [`EdgeBuffer::append`] gives the main log. [`EdgeBuffer::open`] reads it back and
//! requires the file to be **exactly** 8 bytes; anything else (missing entirely: a fresh
//! buffer, not an error; any other length: a torn write, caught between this file's own
//! `write_all` and `sync_all`) is treated as "no confirmed watermark yet" (defaults to
//! `0`) and *reported*, never silently substituted -- [`EdgeBuffer::open`]'s returned
//! [`RecoveryReport::ack_watermark_defaulted`] is `Some(reason)` exactly when this
//! happened. Defaulting to `0` rather than refusing to open at all is a deliberate safety
//! direction, not a convenience: understating how much has been confirmed only ever causes
//! [`EdgeBuffer::replay_from`] to hand back a *longer* prefix than strictly necessary on
//! the next reconnect, and every batch in that prefix that was already accepted comes back
//! from the ingest as `pb::BatchRejection::Duplicate` (`crate::chain::ChainVerifier`'s own
//! `seen_sequences` check) -- absorbed by the duplicate counter, never lost, never
//! double-counted as accepted twice. Overstating it (impossible here, since a torn write
//! can only ever produce *fewer* than 8 well-formed bytes, never a plausible-looking wrong
//! value) would be the genuinely dangerous direction, since it could skip replaying a
//! batch that was never actually confirmed -- this module's format and recovery rule are
//! built so that direction cannot arise from an ordinary crash.
//!
//! # `UplinkDriver`: buffer-while-down, drain-before-send, and one commit per drain
//!
//! [`UplinkDriver::step`] is given one outgoing batch and the caller's injected
//! `now_tai_ns` per call, and only ever does one of two things with that batch: hands it
//! to the [`BatchSink`] directly, or appends it to the [`EdgeBuffer`] because the sink just
//! reported [`SinkOutcome::LinkDown`]. The one piece of behaviour worth calling out
//! explicitly: **on the first step after a down period, the driver drains the entire
//! existing backlog -- in append order, via [`EdgeBuffer::replay_from`] -- before it ever
//! attempts the new batch it was just given**, and it persists the ack watermark **once,
//! only after the whole backlog it read at the start of that drain came back delivered
//! with no interruption** -- not once per replayed record. Committing per record would
//! mean one extra `fsync` per replayed batch, repeating exactly the cost this platform's
//! own E5 status section already measured and named ("fsync-bound durable append", not
//! transport-bound); committing once per successful drain avoids that at the one cost this
//! design accepts on purpose: if the link drops again *in the middle* of a drain, every
//! batch that drain already had delivered (and the ingest already accepted) gets replayed
//! again the next time a drain completes, because the watermark was never advanced past
//! them. That is not a bug this module works around -- it is the literal, realistic
//! version of "the ack watermark was not yet persisted when the link dropped"
//! (`docs/edge-plan.md`'s own E6 test list), and it is exactly what makes
//! `pb::BatchRejection::Duplicate` (and `RejectionCounters.duplicate_count`, never
//! `accepted`) the counter that absorbs it, deterministically and testably.
//!
//! # Where the two `BatchSink` implementations this module needs actually live
//!
//! [`BatchSink`] is the one abstraction [`UplinkDriver`] is generic over, specifically so
//! the same driver can be pointed at either an in-process `av_ingest::Ingest` or a real
//! `av-ingest-client::EdgeIngestClient` over the actual gRPC wire. Neither implementation
//! lives in this crate: `av-edge` must stay free of both a transport dependency and a
//! dependency on `av-ingest` (this crate's own long-standing rule, `crate`'s own module
//! doc), so the `Ingest`-backed impl lives in `crates/av-ingest`'s own tests (that crate
//! already depends on this one, so implementing this crate's trait there is an ordinary,
//! non-cyclic impl-in-a-downstream-crate, same direction as every other
//! `av_edge`-consuming test in this workspace) and the gRPC-backed impl lives in
//! `crates/av-ingest/tests/e6_wire_disconnect.rs` (that crate's own dev-dependency on
//! `av-ingest-client` already brings in `tonic`; `av-edge` gains nothing new).
//!
//! # No clock, no sleep -- verbatim, in this module too
//!
//! Every "now" [`UplinkDriver::step`] or [`crate::chain::ChainVerifier::submit`] ever sees
//! is a plain `i64` the caller supplies. Nothing in this module reads a clock or sleeps;
//! `grep -n "Instant::now\|SystemTime::now\|sleep" crates/av-edge/src/buffer.rs` finds
//! nothing (this crate's own task brief asks for exactly that grep, reproduced verbatim in
//! the worker's own final report).

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::hash;
use crate::pb;

/// Fixed per-record header size: 4 bytes little-endian `payload_len` + 32 raw
/// `record_hash` bytes -- identical to `crates/av-ingest/src/log.rs::HEADER_LEN`. See this
/// module's doc for exactly why this framing is duplicated rather than shared.
const HEADER_LEN: usize = 4 + 32;

/// What can go wrong opening, appending to, or replaying an [`EdgeBuffer`]. One variant
/// per real failure (this crate's own standing convention, `crate::sign::SigningError`'s
/// identical framing) -- never a panic on anything a caller controls.
#[derive(Debug, thiserror::Error)]
pub enum BufferError {
    /// An underlying filesystem operation failed (open, read, write, `sync_all`,
    /// `set_len`) on either the main log file or its `.ack` sidecar.
    #[error("I/O error on edge buffer path {path}: {source}")]
    Io { path: String, #[source] source: std::io::Error },
    /// A payload (an encoded, signed `MeasurementBatch`) is too large for this record
    /// framing's 32-bit length field.
    #[error("payload of {len} bytes does not fit in this record framing's 32-bit length field")]
    PayloadTooLarge { len: usize },
    /// [`EdgeBuffer::replay_from`] found a record that is not even structurally complete
    /// (an incomplete header or payload at EOF) -- unreachable immediately after
    /// [`EdgeBuffer::open`] (which already truncated away any torn *trailing* record), so
    /// this can only mean the file was modified on disk after this `EdgeBuffer` opened it.
    #[error("edge buffer {path} record {index}: torn/incomplete record ({detail})")]
    TornRecord { path: String, index: u64, detail: String },
    /// A record's stored `record_hash` does not match what is recomputed from the
    /// previous record's hash and this record's own payload -- tampering after the fact,
    /// never an ordinary crash (see this module's doc, "Record framing").
    #[error("edge buffer {path} record {index}: stored digest disagrees with its recomputed content ({detail})")]
    DigestMismatch { path: String, index: u64, detail: String },
    /// A record's payload does not decode as `altavista.v1.MeasurementBatch` at all.
    #[error("edge buffer {path} record {index}: payload does not decode as altavista.v1.MeasurementBatch ({detail})")]
    Decode { path: String, index: u64, detail: String },
}

impl BufferError {
    fn io(path: &Path, source: std::io::Error) -> Self {
        BufferError::Io { path: path.display().to_string(), source }
    }
}

/// What [`EdgeBuffer::open`] had to recover from, if anything -- returned so a caller (or
/// test) asserts on the *reported* recovery, never merely infers it from the resulting
/// state (`crates/av-ingest/src/log.rs::RecoveryReport`'s identical framing, extended here
/// to also cover the ack-watermark sidecar -- see this module's doc, "The ack watermark,
/// and why a corrupt one degrades safely").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryReport {
    /// Bytes discarded from the end of the main log file (0 iff nothing was discarded
    /// there -- this type is only ever constructed when at least one of its two fields is
    /// non-trivial).
    pub discarded_log_bytes: u64,
    /// Human-readable reason for `discarded_log_bytes`, `Some` iff that field is nonzero.
    pub log_reason: Option<String>,
    /// `Some(reason)` iff the `.ack` sidecar existed but was not exactly 8 bytes, so its
    /// value was treated as absent (defaulted to watermark `0`) rather than trusted.
    pub ack_watermark_defaulted: Option<String>,
}

/// One structurally-complete record found while scanning a file's bytes -- byte offsets
/// only, mirroring `crates/av-ingest/src/log.rs::FrameRef` exactly.
struct FrameRef {
    start: usize,
    stored_hash: [u8; 32],
    payload_start: usize,
    payload_end: usize,
}

/// Walks `bytes` from the start, splitting it into structurally-complete records, then --
/// only if the scan reached a clean EOF with at least one record -- checks whether the
/// **last** record's own content is valid. Returns every frame found (with the last one
/// dropped if it failed that content check) plus, if anything was discarded, the byte
/// offset to truncate to and why. A byte-for-byte copy of
/// `crates/av-ingest/src/log.rs::scan_frames`'s own logic (see this module's doc for why
/// it is duplicated rather than shared) with only the module path of `hash`/`pb` changed.
fn scan_frames(bytes: &[u8]) -> (Vec<FrameRef>, Option<(usize, String)>) {
    let len = bytes.len();
    let mut frames: Vec<FrameRef> = Vec::new();
    let mut pos = 0usize;

    loop {
        if pos == len {
            break;
        }
        let remaining = len - pos;
        if remaining < HEADER_LEN {
            return (frames, Some((pos, format!("incomplete record header: {remaining} byte(s) remain at offset {pos}, need {HEADER_LEN}"))));
        }
        let payload_len = u32::from_le_bytes(bytes[pos..pos + 4].try_into().expect("4-byte slice")) as usize;
        let mut stored_hash = [0u8; 32];
        stored_hash.copy_from_slice(&bytes[pos + 4..pos + HEADER_LEN]);
        let payload_start = pos + HEADER_LEN;
        let payload_end = payload_start + payload_len;
        if payload_end > len {
            return (
                frames,
                Some((pos, format!("incomplete record payload: declared length {payload_len}, only {} byte(s) remain after the header at offset {pos}", len - payload_start))),
            );
        }
        frames.push(FrameRef { start: pos, stored_hash, payload_start, payload_end });
        pos = payload_end;
    }

    if let Some(last) = frames.last() {
        let prev_hash: Vec<u8> = if frames.len() >= 2 { frames[frames.len() - 2].stored_hash.to_vec() } else { hash::GENESIS.to_vec() };
        let payload = &bytes[last.payload_start..last.payload_end];
        let recomputed = hash::compute_batch_hash(&prev_hash, payload);
        let decodes = <pb::MeasurementBatch as prost::Message>::decode(payload).is_ok();
        if recomputed != last.stored_hash || !decodes {
            let discard_pos = last.start;
            let discarded = (len - discard_pos) as u64;
            let mut kept = frames;
            kept.pop();
            return (
                kept,
                Some((discard_pos, format!("trailing record's content is corrupt ({discarded} byte(s)): stored hash does not match its recomputed content, or the payload does not decode as a MeasurementBatch"))),
            );
        }
    }
    (frames, None)
}

fn truncate_file(path: &Path, len: u64) -> Result<(), BufferError> {
    let f = OpenOptions::new().write(true).open(path).map_err(|e| BufferError::io(path, e))?;
    f.set_len(len).map_err(|e| BufferError::io(path, e))
}

/// The `.ack` sidecar's path for a given main-log `path` -- appended to the full file
/// name (not a sibling with the log's own extension replaced), so `foo.buflog` gets
/// `foo.buflog.ack`, never `foo.ack`.
fn watermark_path_for(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".ack");
    PathBuf::from(name)
}

/// Reads the ack watermark from `path`. `Ok((0, None))` for a missing file (a fresh
/// buffer, not a recovery); `Ok((0, Some(reason)))` for a present-but-wrong-length file
/// (see this module's doc, "The ack watermark, and why a corrupt one degrades safely");
/// `Ok((value, None))` for a well-formed one.
fn read_watermark(path: &Path) -> Result<(u64, Option<String>), BufferError> {
    match std::fs::read(path) {
        Ok(bytes) if bytes.len() == 8 => {
            let value = u64::from_le_bytes(bytes[0..8].try_into().expect("checked len == 8"));
            Ok((value, None))
        }
        Ok(bytes) => Ok((
            0,
            Some(format!(
                "ack watermark file {} has {} byte(s), expected exactly 8 -- treating as absent (sequence 0): a replay will resend a possibly-already-accepted prefix, which the ingest's own (producer, sequence) dedup absorbs as a duplicate, never as lost data",
                path.display(),
                bytes.len()
            )),
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok((0, None)),
        Err(e) => Err(BufferError::io(path, e)),
    }
}

#[derive(Debug)]
struct Tip {
    hash: Vec<u8>,
    record_count: u64,
}

/// One producer's durable, append-only, hash-chained edge-side buffer of already-signed
/// batches, plus its own persisted ack watermark. See this module's doc for the exact
/// record framing, the deliberate deviations from `PartitionLog`, and the watermark's
/// recovery contract.
#[derive(Debug)]
pub struct EdgeBuffer {
    path: PathBuf,
    watermark_path: PathBuf,
    file: Mutex<File>,
    tip: Mutex<Tip>,
    ack_watermark: Mutex<u64>,
}

impl EdgeBuffer {
    /// Opens (creating if necessary) the buffer file at `path`, recovering from any torn
    /// or corrupt trailing record first, and reads back the `.ack` sidecar next to it.
    /// Returns the buffer plus `Some(RecoveryReport)` iff either the log or the watermark
    /// needed recovering -- mirroring `PartitionLog::open`'s "the caller checks this
    /// return value, never the log's state after the fact" contract.
    pub fn open(path: &Path) -> Result<(Self, Option<RecoveryReport>), BufferError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| BufferError::io(parent, e))?;
            }
        }

        let bytes = std::fs::read(path).unwrap_or_default(); // a missing file reads as empty -- a fresh buffer.
        let (frames, discard) = scan_frames(&bytes);

        let log_recovery = if let Some((discard_pos, reason)) = discard {
            let discarded_bytes = (bytes.len() - discard_pos) as u64;
            truncate_file(path, discard_pos as u64)?;
            Some((discarded_bytes, reason))
        } else {
            None
        };

        let (tip_hash, record_count) = match frames.last() {
            Some(f) => (f.stored_hash.to_vec(), frames.len() as u64),
            None => (hash::GENESIS.to_vec(), 0),
        };

        let file = OpenOptions::new().create(true).append(true).open(path).map_err(|e| BufferError::io(path, e))?;

        let watermark_path = watermark_path_for(path);
        let (ack_watermark, watermark_defaulted) = read_watermark(&watermark_path)?;

        let report = if log_recovery.is_some() || watermark_defaulted.is_some() {
            Some(RecoveryReport {
                discarded_log_bytes: log_recovery.as_ref().map(|(b, _)| *b).unwrap_or(0),
                log_reason: log_recovery.map(|(_, r)| r),
                ack_watermark_defaulted: watermark_defaulted,
            })
        } else {
            None
        };

        Ok((Self { path: path.to_path_buf(), watermark_path, file: Mutex::new(file), tip: Mutex::new(Tip { hash: tip_hash, record_count }), ack_watermark: Mutex::new(ack_watermark) }, report))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// This buffer's current chain head (the last appended record's own `record_hash`),
    /// or [`hash::GENESIS`] if nothing has been appended yet.
    pub fn tip_hash(&self) -> Vec<u8> {
        self.tip.lock().unwrap_or_else(|p| p.into_inner()).hash.clone()
    }

    /// Number of records this buffer currently holds on disk.
    pub fn record_count(&self) -> u64 {
        self.tip.lock().unwrap_or_else(|p| p.into_inner()).record_count
    }

    /// The last confirmed-delivered sequence, per the persisted `.ack` sidecar (`0` if
    /// nothing has ever been confirmed, or if the sidecar was corrupt at open -- see this
    /// module's doc).
    pub fn ack_watermark(&self) -> u64 {
        *self.ack_watermark.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Appends `batch` (already signed by the caller) as one new record, chained from
    /// this buffer's current tip. Durable (flushed and `sync_all`'d) before returning
    /// `Ok`, exactly like `PartitionLog::append`.
    pub fn append(&self, batch: &pb::MeasurementBatch) -> Result<(), BufferError> {
        let payload = prost::Message::encode_to_vec(batch);
        if payload.len() > u32::MAX as usize {
            return Err(BufferError::PayloadTooLarge { len: payload.len() });
        }

        let mut tip = self.tip.lock().unwrap_or_else(|p| p.into_inner());
        let record_hash = hash::compute_batch_hash(&tip.hash, &payload);

        let mut frame = Vec::with_capacity(HEADER_LEN + payload.len());
        frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        frame.extend_from_slice(&record_hash);
        frame.extend_from_slice(&payload);

        {
            let mut f = self.file.lock().unwrap_or_else(|p| p.into_inner());
            f.write_all(&frame).map_err(|e| BufferError::io(&self.path, e))?;
            f.flush().map_err(|e| BufferError::io(&self.path, e))?;
            f.sync_all().map_err(|e| BufferError::io(&self.path, e))?;
        }

        tip.hash = record_hash.to_vec();
        tip.record_count += 1;
        Ok(())
    }

    /// Re-reads this buffer's file from disk (independent of any in-memory tip state) and
    /// returns every batch whose own `sequence` is strictly greater than
    /// `sequence_watermark`, decoded, in append order -- never re-sorted. Verifies this
    /// file's own record chain while walking it (mirroring `PartitionLog::verify`'s
    /// independent from-scratch re-derivation): a structurally torn record, a digest that
    /// disagrees with its recomputed content, or a payload that fails to decode are each a
    /// distinct, typed [`BufferError`] rather than a partial or silently-wrong result.
    /// Should not occur on a buffer this crate's own `EdgeBuffer::open`/`append` produced
    /// (a crash's own torn tail is already excluded by `open`); reachable only if the file
    /// was modified on disk by something other than this `EdgeBuffer` after it opened.
    pub fn replay_from(&self, sequence_watermark: u64) -> Result<Vec<pb::MeasurementBatch>, BufferError> {
        let bytes = std::fs::read(&self.path).map_err(|e| BufferError::io(&self.path, e))?;
        let len = bytes.len();
        let mut pos = 0usize;
        let mut expected_prev: Vec<u8> = hash::GENESIS.to_vec();
        let mut index: u64 = 0;
        let mut out = Vec::new();

        while pos < len {
            index += 1;
            let remaining = len - pos;
            if remaining < HEADER_LEN {
                return Err(BufferError::TornRecord { path: self.path.display().to_string(), index, detail: format!("incomplete header at EOF ({remaining} byte(s) remain)") });
            }
            let payload_len = u32::from_le_bytes(bytes[pos..pos + 4].try_into().expect("4-byte slice")) as usize;
            let stored_hash = &bytes[pos + 4..pos + HEADER_LEN];
            let payload_start = pos + HEADER_LEN;
            let payload_end = payload_start + payload_len;
            if payload_end > len {
                return Err(BufferError::TornRecord { path: self.path.display().to_string(), index, detail: format!("incomplete payload at EOF (declared {payload_len} byte(s))") });
            }
            let payload = &bytes[payload_start..payload_end];
            let recomputed = hash::compute_batch_hash(&expected_prev, payload);
            if recomputed.as_slice() != stored_hash {
                return Err(BufferError::DigestMismatch {
                    path: self.path.display().to_string(),
                    index,
                    detail: "stored record hash does not match its recomputed content -- the buffer was tampered with after being appended".to_string(),
                });
            }
            let decoded = <pb::MeasurementBatch as prost::Message>::decode(payload).map_err(|e| BufferError::Decode { path: self.path.display().to_string(), index, detail: e.to_string() })?;
            expected_prev = recomputed.to_vec();
            if decoded.sequence > sequence_watermark {
                out.push(decoded);
            }
            pos = payload_end;
        }
        Ok(out)
    }

    /// Durably overwrites the `.ack` sidecar with `sequence` (8 raw little-endian bytes,
    /// flushed and `sync_all`'d before returning `Ok`) and updates this buffer's own
    /// in-memory watermark to match. See this module's doc for the recovery contract a
    /// torn write here degrades into, and [`UplinkDriver`]'s own doc for exactly when this
    /// is (and is not) called.
    pub fn persist_ack_watermark(&self, sequence: u64) -> Result<(), BufferError> {
        let bytes = sequence.to_le_bytes();
        let mut f = OpenOptions::new().write(true).create(true).truncate(true).open(&self.watermark_path).map_err(|e| BufferError::io(&self.watermark_path, e))?;
        f.write_all(&bytes).map_err(|e| BufferError::io(&self.watermark_path, e))?;
        f.flush().map_err(|e| BufferError::io(&self.watermark_path, e))?;
        f.sync_all().map_err(|e| BufferError::io(&self.watermark_path, e))?;
        *self.ack_watermark.lock().unwrap_or_else(|p| p.into_inner()) = sequence;
        Ok(())
    }
}

// -----------------------------------------------------------------------------------------
// BatchSink / UplinkDriver
// -----------------------------------------------------------------------------------------

/// What happened when [`UplinkDriver`] asked a [`BatchSink`] to submit one batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SinkOutcome<V> {
    /// The sink is reachable: `V` is whatever verdict it returned, accepted or any typed
    /// rejection -- **including** a duplicate verdict, which is exactly what a replayed,
    /// already-accepted batch is expected to come back as (see this module's own doc);
    /// that is not a failure this variant needs to distinguish from an ordinary accept.
    Delivered(V),
    /// The link is down: the batch this call was given was **not** delivered and must be
    /// durably buffered by the caller.
    LinkDown,
}

/// What [`UplinkDriver`] submits batches through, and how it learns "the link is down".
/// The one abstraction this driver is generic over -- see this module's own doc,
/// "Where the two `BatchSink` implementations this module needs actually live", for why
/// neither of this crate's own two implementations (an in-process `av_ingest::Ingest`, a
/// real gRPC `EdgeIngestClient`) is defined in this crate.
pub trait BatchSink {
    type Verdict;
    type Error: std::fmt::Debug;

    /// Attempts to submit one batch at the caller-injected `now_tai_ns`.
    /// `Ok(SinkOutcome::LinkDown)` means "not delivered, buffer it and try again later" --
    /// never an `Err`, since a down link is an ordinary condition [`UplinkDriver`] handles
    /// by buffering, not a failure it needs to propagate. `Err` is reserved for a failure
    /// buffering cannot help with (e.g. the sink rejects the request as malformed).
    fn submit(&mut self, batch: &pb::MeasurementBatch, now_tai_ns: i64) -> Result<SinkOutcome<Self::Verdict>, Self::Error>;
}

/// What one [`UplinkDriver::step`] call did.
#[derive(Debug)]
pub enum StepOutcome<V> {
    /// The batch this call was given was sent directly (not through the buffer).
    /// `replayed` holds the verdict for every backlog batch this call drained first --
    /// non-empty exactly on the first successful step after a down period.
    Delivered { replayed: Vec<V>, verdict: V },
    /// The batch this call was given was appended to the durable buffer, not sent --
    /// either the link was already down, or it dropped again partway through draining
    /// the backlog. `replayed` holds the verdict for every backlog batch that *did* get
    /// delivered before that happened (see this module's doc: the ack watermark is
    /// deliberately not advanced for those in this case).
    Buffered { replayed: Vec<V> },
}

/// A driver error: either the [`EdgeBuffer`] itself failed (I/O, corruption), or the
/// [`BatchSink`] returned a genuine `Err` (not a `LinkDown` outcome, which is not an
/// error at all -- see [`BatchSink::submit`]'s own doc comment).
#[derive(Debug, thiserror::Error)]
pub enum DriverError<E: std::fmt::Debug> {
    #[error("edge buffer error: {0}")]
    Buffer(#[from] BufferError),
    #[error("batch sink error: {0:?}")]
    Sink(E),
}

/// The pure state machine described in this module's own doc comment ("`UplinkDriver`:
/// buffer-while-down, drain-before-send, and one commit per drain"): given a [`BatchSink`]
/// and an [`EdgeBuffer`], turns a sequence of `step` calls -- one outgoing batch and one
/// caller-injected clock value each -- into either direct delivery or durable buffering,
/// replaying the buffer in order before ever sending anything new once the link is back.
pub struct UplinkDriver<S: BatchSink> {
    sink: S,
    buffer: EdgeBuffer,
    was_down: bool,
}

impl<S: BatchSink> UplinkDriver<S> {
    pub fn new(sink: S, buffer: EdgeBuffer) -> Self {
        Self { sink, buffer, was_down: false }
    }

    /// Mutable access to the underlying sink -- so a caller (a test cutting the link) can
    /// reach a `BatchSink`'s own extra, non-trait methods (e.g. a real gRPC sink's
    /// `disconnect`/`reconnect`) between `step` calls without this driver needing to know
    /// anything about them itself.
    pub fn sink_mut(&mut self) -> &mut S {
        &mut self.sink
    }

    /// Read-only access to the underlying buffer -- so a caller/test can inspect
    /// `record_count`/`ack_watermark`/`tip_hash` without this driver re-exposing each one.
    pub fn buffer(&self) -> &EdgeBuffer {
        &self.buffer
    }

    /// Whether this driver currently considers the link down (i.e. the most recent
    /// `submit` this driver made returned [`SinkOutcome::LinkDown`] and no full drain has
    /// since succeeded).
    pub fn was_down(&self) -> bool {
        self.was_down
    }

    /// One step: submit `batch` at `now_tai_ns`, or buffer it if the link is down -- and,
    /// if a down period just ended, drain the whole existing backlog first. See this
    /// module's own doc comment for exactly what "drain" means and when the ack watermark
    /// does and does not advance.
    pub fn step(&mut self, batch: &pb::MeasurementBatch, now_tai_ns: i64) -> Result<StepOutcome<S::Verdict>, DriverError<S::Error>> {
        let mut replayed = Vec::new();

        if self.was_down {
            let backlog = self.buffer.replay_from(self.buffer.ack_watermark())?;
            for buffered in &backlog {
                match self.sink.submit(buffered, now_tai_ns).map_err(DriverError::Sink)? {
                    SinkOutcome::Delivered(v) => replayed.push(v),
                    SinkOutcome::LinkDown => {
                        // Still down (or down again): `batch` itself must also wait its
                        // turn, and the watermark stays exactly where it was, even though
                        // `replayed` may be non-empty -- see this module's doc for why
                        // that is deliberate, not a bug.
                        self.buffer.append(batch)?;
                        return Ok(StepOutcome::Buffered { replayed });
                    }
                }
            }
            // The whole backlog read at the start of this drain came back delivered with
            // no interruption: commit the watermark once, for the whole drain, not once
            // per record (this module's doc explains the fsync-cost reasoning).
            if let Some(last) = backlog.last() {
                self.buffer.persist_ack_watermark(last.sequence)?;
            }
            self.was_down = false;
        }

        match self.sink.submit(batch, now_tai_ns).map_err(DriverError::Sink)? {
            SinkOutcome::Delivered(v) => Ok(StepOutcome::Delivered { replayed, verdict: v }),
            SinkOutcome::LinkDown => {
                self.was_down = true;
                self.buffer.append(batch)?;
                Ok(StepOutcome::Buffered { replayed })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openssl::ec::EcKey;
    use openssl::pkey::Private;

    const TEST_KEY_PEM: &[u8] = include_bytes!("../tests/fixtures/test_signing_key.pem");

    fn signing_key() -> EcKey<Private> {
        crate::sign::load_signing_key(TEST_KEY_PEM).unwrap()
    }

    fn batch(sequence: u64, prev_hash: &[u8], key: &EcKey<Private>) -> pb::MeasurementBatch {
        let mut b = pb::MeasurementBatch { producer_id: "edge-buffer-test".to_string(), sequence, batch_tai_ns: 1_000 + sequence as i64, shard_key: "buf-shard".to_string(), ..Default::default() };
        crate::sign::sign_batch(&mut b, prev_hash, key).unwrap();
        b
    }

    fn tmp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("av-edge-buffer-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("producer.buflog")
    }

    #[test]
    fn open_on_a_fresh_path_reports_no_recovery_and_starts_at_genesis() {
        let path = tmp_path("fresh");
        let (buffer, report) = EdgeBuffer::open(&path).unwrap();
        assert!(report.is_none(), "{report:?}");
        assert_eq!(buffer.record_count(), 0);
        assert_eq!(buffer.tip_hash(), hash::GENESIS.to_vec());
        assert_eq!(buffer.ack_watermark(), 0);
    }

    #[test]
    fn append_then_replay_from_returns_every_record_in_append_order() {
        let path = tmp_path("append-replay");
        let key = signing_key();
        let (buffer, _) = EdgeBuffer::open(&path).unwrap();

        let b1 = batch(1, hash::GENESIS, &key);
        let b2 = batch(2, &b1.batch_hash, &key);
        let b3 = batch(3, &b2.batch_hash, &key);
        buffer.append(&b1).unwrap();
        buffer.append(&b2).unwrap();
        buffer.append(&b3).unwrap();
        assert_eq!(buffer.record_count(), 3);

        let replayed = buffer.replay_from(0).unwrap();
        assert_eq!(replayed, vec![b1.clone(), b2.clone(), b3.clone()], "replay_from(0) must return every record, in the exact order it was appended");
    }

    #[test]
    fn replay_from_excludes_records_at_or_below_the_watermark() {
        let path = tmp_path("watermark-filter");
        let key = signing_key();
        let (buffer, _) = EdgeBuffer::open(&path).unwrap();
        let b1 = batch(1, hash::GENESIS, &key);
        let b2 = batch(2, &b1.batch_hash, &key);
        let b3 = batch(3, &b2.batch_hash, &key);
        buffer.append(&b1).unwrap();
        buffer.append(&b2).unwrap();
        buffer.append(&b3).unwrap();

        assert_eq!(buffer.replay_from(1).unwrap(), vec![b2.clone(), b3.clone()]);
        assert_eq!(buffer.replay_from(3).unwrap(), Vec::<pb::MeasurementBatch>::new());
    }

    #[test]
    fn ack_watermark_persists_and_recovers_across_reopen() {
        let path = tmp_path("watermark-recover");
        let key = signing_key();
        {
            let (buffer, _) = EdgeBuffer::open(&path).unwrap();
            let b1 = batch(1, hash::GENESIS, &key);
            buffer.append(&b1).unwrap();
            assert_eq!(buffer.ack_watermark(), 0);
            buffer.persist_ack_watermark(1).unwrap();
            assert_eq!(buffer.ack_watermark(), 1);
        }
        // A fresh EdgeBuffer over the same path (simulating a process restart) must
        // recover the persisted watermark, not default it back to 0.
        let (reopened, report) = EdgeBuffer::open(&path).unwrap();
        assert!(report.is_none(), "a clean watermark file must not be reported as a recovery: {report:?}");
        assert_eq!(reopened.ack_watermark(), 1);
    }

    #[test]
    fn open_recovers_from_a_torn_final_record_and_reports_it() {
        let path = tmp_path("torn-tail");
        let key = signing_key();
        let (b1, b2, full_len) = {
            let (buffer, _) = EdgeBuffer::open(&path).unwrap();
            let b1 = batch(1, hash::GENESIS, &key);
            let b2 = batch(2, &b1.batch_hash, &key);
            buffer.append(&b1).unwrap();
            buffer.append(&b2).unwrap();
            assert_eq!(buffer.record_count(), 2);
            (b1, b2, std::fs::metadata(&path).unwrap().len())
        };

        // Simulate a crash mid-write of a third record: append one more genuine record,
        // then truncate a few bytes off the very end -- landing inside that third
        // record's own payload, never touching records 1/2 (mirrors
        // `crates/av-ingest/tests/crash_recovery.rs`'s identical technique).
        {
            let (buffer, _) = EdgeBuffer::open(&path).unwrap();
            let b3 = batch(3, &b2.batch_hash, &key);
            buffer.append(&b3).unwrap();
        };
        let full_len_with_b3 = std::fs::metadata(&path).unwrap().len();
        assert!(full_len_with_b3 > full_len, "record 3 must have added bytes");
        let torn_len = full_len_with_b3 - 10;
        assert!(torn_len > full_len, "the truncation point must still land inside record 3, not touch records 1/2");
        {
            let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            f.set_len(torn_len).unwrap();
        }

        let (recovered, report) = EdgeBuffer::open(&path).unwrap();
        let report = report.expect("a torn trailing record must be reported");
        assert_eq!(report.discarded_log_bytes, torn_len - full_len, "must discard exactly the truncated record 3's own remaining bytes");
        assert!(report.log_reason.as_ref().unwrap().contains("incomplete"), "{report:?}");
        assert!(report.ack_watermark_defaulted.is_none(), "no watermark file exists in this test, so nothing should be reported about it: {report:?}");

        assert_eq!(recovered.record_count(), 2, "only the two complete records must survive");
        assert_eq!(std::fs::metadata(&path).unwrap().len(), full_len, "the file must be physically truncated back to the last complete record");

        // The surviving prefix must still replay, in order, unaffected by the torn record
        // that came after it.
        let replayed = recovered.replay_from(0).unwrap();
        assert_eq!(replayed, vec![b1, b2], "the surviving prefix must replay in order after recovering from a torn tail");
    }

    #[test]
    fn a_record_tampered_with_after_append_is_a_digest_mismatch_not_a_silent_skip() {
        let path = tmp_path("tampered-mid-file");
        let key = signing_key();
        {
            let (buffer, _) = EdgeBuffer::open(&path).unwrap();
            let b1 = batch(1, hash::GENESIS, &key);
            let b2 = batch(2, &b1.batch_hash, &key);
            let b3 = batch(3, &b2.batch_hash, &key);
            buffer.append(&b1).unwrap();
            buffer.append(&b2).unwrap();
            buffer.append(&b3).unwrap();
            assert_eq!(buffer.record_count(), 3);
        }

        // Flip one byte inside the FIRST record's own payload -- not the length prefix,
        // not the digest, and not the last record (which would be indistinguishable from
        // a torn tail): this is tampering after a successful append, and must be surfaced
        // as such, never treated like a crash's torn tail.
        let mut bytes = std::fs::read(&path).unwrap();
        let first_payload_len = u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize;
        assert!(first_payload_len > 0, "the first record's payload must be non-empty for this test to tamper with it");
        let tamper_at = HEADER_LEN; // the first byte of record 1's own payload.
        bytes[tamper_at] ^= 0xFF;
        std::fs::write(&path, &bytes).unwrap();

        let (reopened, report) = EdgeBuffer::open(&path).unwrap();
        // `open` (via `scan_frames`) only re-validates the content of the LAST record --
        // exactly like `PartitionLog::open`. A tampered record that is not the trailing
        // one is not a torn tail and must not be reported or silently discarded as one.
        assert!(report.is_none(), "tampering with a record that is not the trailing one must not be treated like a torn tail: {report:?}");
        assert_eq!(reopened.record_count(), 3, "open must not silently drop the tampered record either");

        let err = reopened.replay_from(0).expect_err("a mid-file digest mismatch must fail replay outright, never silently skip the tampered record or return a short/partial list of the good ones");
        let rendered = err.to_string();
        match &err {
            BufferError::DigestMismatch { path: err_path, index, detail } => {
                assert_eq!(*index, 1, "the FIRST record must be identified as the one that failed, not some other index");
                assert_eq!(err_path, &path.display().to_string());
                assert!(!detail.is_empty());
            }
            other => panic!("expected BufferError::DigestMismatch, got {other:?}"),
        }
        assert!(rendered.contains("record 1"), "the Display string must identify which record failed: {rendered}");
        assert!(rendered.contains(&path.display().to_string()), "the Display string must identify which file failed: {rendered}");
    }

    #[test]
    fn a_corrupt_length_ack_file_is_defaulted_to_zero_and_reported() {
        let path = tmp_path("watermark-corrupt");
        let key = signing_key();
        let (buffer, _) = EdgeBuffer::open(&path).unwrap();
        let b1 = batch(1, hash::GENESIS, &key);
        buffer.append(&b1).unwrap();
        buffer.persist_ack_watermark(1).unwrap();
        drop(buffer);

        // Corrupt the sidecar: truncate it to 3 bytes (a torn watermark write).
        let watermark_path = watermark_path_for(&path);
        {
            let f = std::fs::OpenOptions::new().write(true).open(&watermark_path).unwrap();
            f.set_len(3).unwrap();
        }

        let (reopened, report) = EdgeBuffer::open(&path).unwrap();
        let report = report.expect("a corrupt-length watermark file must be reported");
        assert_eq!(report.discarded_log_bytes, 0, "the main log itself is untouched by this corruption");
        assert!(report.ack_watermark_defaulted.is_some(), "{report:?}");
        assert_eq!(reopened.ack_watermark(), 0, "a corrupt watermark must default to 0, the safe (extra-replay) direction, never a fabricated nonzero value");
    }

    // -------------------------------------------------------------------------------------
    // UplinkDriver, against a minimal in-memory fake sink (no real Ingest -- av-edge
    // cannot depend on av-ingest; see this module's doc for where the real, Ingest-backed
    // and gRPC-backed sinks live instead). This fake only records what it was told and
    // reports LinkDown on a caller-controlled schedule -- it exists purely to pin down
    // this driver's own control flow (buffer-while-down, drain-before-send, one commit per
    // uninterrupted drain), independent of any real ingest semantics.
    // -------------------------------------------------------------------------------------

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct FakeVerdict(u64); // just the sequence, so tests can assert on delivery order.

    /// `up_budget = Some(k)`: the next `k` submit calls succeed, then this sink reports
    /// `LinkDown` until the budget is changed again. `None`: always up.
    struct FakeSink {
        delivered: Vec<u64>,
        up_budget: Option<usize>,
    }

    impl FakeSink {
        fn new() -> Self {
            Self { delivered: Vec::new(), up_budget: None }
        }
    }

    impl BatchSink for FakeSink {
        type Verdict = FakeVerdict;
        type Error = String;

        fn submit(&mut self, batch: &pb::MeasurementBatch, _now_tai_ns: i64) -> Result<SinkOutcome<FakeVerdict>, String> {
            if let Some(budget) = self.up_budget {
                if budget == 0 {
                    return Ok(SinkOutcome::LinkDown);
                }
                self.up_budget = Some(budget - 1);
            }
            self.delivered.push(batch.sequence);
            Ok(SinkOutcome::Delivered(FakeVerdict(batch.sequence)))
        }
    }

    fn driver_with_fake_sink(name: &str) -> (UplinkDriver<FakeSink>, EcKey<Private>) {
        let path = tmp_path(name);
        let (buffer, _) = EdgeBuffer::open(&path).unwrap();
        (UplinkDriver::new(FakeSink::new(), buffer), signing_key())
    }

    #[test]
    fn step_sends_directly_while_the_link_stays_up() {
        let (mut driver, key) = driver_with_fake_sink("driver-up");
        let b1 = batch(1, hash::GENESIS, &key);
        let outcome = driver.step(&b1, 1_000).unwrap();
        match outcome {
            StepOutcome::Delivered { replayed, verdict } => {
                assert!(replayed.is_empty());
                assert_eq!(verdict, FakeVerdict(1));
            }
            other => panic!("expected Delivered, got {other:?}"),
        }
        assert_eq!(driver.buffer().record_count(), 0, "nothing should ever touch the buffer while the link stays up");
    }

    #[test]
    fn step_buffers_while_down_and_drains_in_order_before_sending_the_new_batch() {
        let (mut driver, key) = driver_with_fake_sink("driver-drain");
        let b1 = batch(1, hash::GENESIS, &key);
        let b2 = batch(2, &b1.batch_hash, &key);
        let b3 = batch(3, &b2.batch_hash, &key);

        driver.sink_mut().up_budget = Some(0); // down from the start
        let out1 = driver.step(&b1, 1_000).unwrap();
        assert!(matches!(out1, StepOutcome::Buffered { .. }));
        let out2 = driver.step(&b2, 1_001).unwrap();
        assert!(matches!(out2, StepOutcome::Buffered { .. }));
        assert_eq!(driver.buffer().record_count(), 2, "both down-period batches must be durably buffered");
        assert_eq!(driver.buffer().ack_watermark(), 0, "nothing has ever been delivered yet, so the watermark must still be 0");

        driver.sink_mut().up_budget = None; // fully back up
        let out3 = driver.step(&b3, 1_002).unwrap();
        match out3 {
            StepOutcome::Delivered { replayed, verdict } => {
                assert_eq!(replayed, vec![FakeVerdict(1), FakeVerdict(2)], "the backlog must drain in append order before batch 3 is sent");
                assert_eq!(verdict, FakeVerdict(3));
            }
            other => panic!("expected Delivered, got {other:?}"),
        }
        assert_eq!(driver.sink_mut().delivered, vec![1, 2, 3], "delivery order at the sink must be exactly append order, then the new batch");
        assert_eq!(driver.buffer().ack_watermark(), 2, "a fully successful drain must commit the watermark to the last backlog sequence");
    }

    #[test]
    fn an_interrupted_drain_does_not_advance_the_watermark_and_replays_its_own_delivered_prefix_again() {
        let (mut driver, key) = driver_with_fake_sink("driver-interrupted-drain");
        let b1 = batch(1, hash::GENESIS, &key);
        let b2 = batch(2, &b1.batch_hash, &key);
        let b3 = batch(3, &b2.batch_hash, &key);

        driver.sink_mut().up_budget = Some(0);
        driver.step(&b1, 1_000).unwrap();
        driver.step(&b2, 1_001).unwrap();
        assert_eq!(driver.buffer().record_count(), 2);

        // "Reconnect", but only long enough to deliver the first backlog record before
        // dropping again mid-drain -- the literal, realistic version of "the ack
        // watermark was not yet persisted when the link dropped" (docs/edge-plan.md's own
        // E6 test list).
        driver.sink_mut().up_budget = Some(1);
        let out = driver.step(&b3, 1_002).unwrap();
        match out {
            StepOutcome::Buffered { replayed } => assert_eq!(replayed, vec![FakeVerdict(1)], "exactly one backlog record must have been delivered before the interruption"),
            other => panic!("expected Buffered (interrupted drain), got {other:?}"),
        }
        assert_eq!(driver.buffer().ack_watermark(), 0, "an interrupted drain must NOT advance the watermark, even though record 1 was, in fact, delivered");
        assert_eq!(driver.buffer().record_count(), 3, "batch 3 must also have been buffered, since the link was still down when its own turn came");

        // Fully reconnect: this drain replays the WHOLE backlog again, including record
        // 1 -- which the sink already saw once. A real ingest would answer that with
        // BatchRejection::Duplicate; this fake sink has no such concept, so this test
        // only asserts what this module itself guarantees: the same record is handed to
        // the sink again, in order, and the drain completes and commits this time.
        driver.sink_mut().up_budget = None;
        driver.sink_mut().delivered.clear();
        let final_batch = batch(4, &b3.batch_hash, &key);
        let out2 = driver.step(&final_batch, 1_003).unwrap();
        match out2 {
            StepOutcome::Delivered { replayed, .. } => assert_eq!(replayed, vec![FakeVerdict(1), FakeVerdict(2), FakeVerdict(3)], "the second, uninterrupted drain must replay the ENTIRE backlog again, record 1 included"),
            other => panic!("expected Delivered, got {other:?}"),
        }
        assert_eq!(driver.buffer().ack_watermark(), 3, "the now-uninterrupted drain must finally commit the watermark");
    }
}
