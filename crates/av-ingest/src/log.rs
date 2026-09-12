//! The durable, per-partition, file-backed log that **is** the ledger (ADR-004: "for the
//! engine and command services the durable log itself is the ledger, chained per
//! partition"; `docs/edge-plan.md` milestone E3). One append-only file per partition,
//! named from the batch's own `shard_key` (`proto/altavista/v1/edge.proto`'s new field
//! 11), sanitised so a `shard_key` can never escape [`PartitionLog::open`]'s own
//! directory.
//!
//! # Record framing (byte for byte -- a future reader needs nothing but this comment)
//!
//! A partition file is a flat sequence of records, back to back, with **no** file-level
//! header and **no** trailing padding. Each record is:
//!
//! ```text
//! +----------------------+----------------------------+---------------------------+
//! | payload_len: u32 LE  | record_hash: [u8; 32]      | payload: payload_len bytes|
//! | (4 bytes)            | (32 bytes, raw, not hex)   |                           |
//! +----------------------+----------------------------+---------------------------+
//! ```
//!
//! - `payload_len` is a little-endian `u32`: the exact byte length of `payload` that
//!   follows this record's own 36-byte fixed header (`4 + 32`).
//! - `payload` is `prost::Message::encode_to_vec` of the accepted `altavista.v1.
//!   MeasurementBatch` **as a whole** (every field, including its own `batch_hash`/
//!   `signature`/`prev_hash` -- this is the full signed batch as accepted, not the
//!   hash-body encoding `av_edge::hash::canonical_body_bytes` computes for the
//!   *producer's own* per-message signature). Every `map<..>` field anywhere in
//!   `av_cdm::pb` is generated as a `BTreeMap` (`av-cdm/build.rs`'s `.btree_map(["."])`),
//!   so this encoding is a deterministic function of the batch's field values, which is
//!   what makes this log's bytes reproducible run to run (see the crate's determinism
//!   test).
//! - `record_hash` is `SHA-256(prev_record_hash_bytes || payload)` -- via
//!   `av_edge::hash::compute_batch_hash`, reused directly rather than re-implementing the
//!   same one-line primitive a second time in this crate. `prev_record_hash_bytes` is the
//!   literal ASCII bytes of [`av_edge::hash::GENESIS`] for a partition's first record, or
//!   else the *previous record's own* 32 raw `record_hash` bytes -- the exact convention
//!   `av_edge::hash`/`crates/av-dynamics-service/src/evidence.rs` both already use for
//!   their own chains, applied here to a second, independent chain (see "Two chains,
//!   deliberately" below).
//!
//! To parse a partition file by hand: read 4 bytes (LE `u32`) for `payload_len`, read the
//! next 32 bytes as `record_hash`, read `payload_len` more bytes as `payload`, decode
//! `payload` as an `altavista.v1.MeasurementBatch`, and repeat from the next byte until
//! EOF. A file that ends with fewer than 36 bytes remaining, or with fewer than
//! `payload_len` payload bytes remaining, ends mid-record -- see "Crash recovery" below.
//!
//! # Two chains, deliberately
//!
//! This is a **second, independent** hash chain from the one `av_edge::chain::
//! ChainVerifier`/`av_edge::sign` already maintain per *producer* (`MeasurementBatch.
//! prev_hash`/`batch_hash`, signed with the producer's own key). That per-producer chain
//! proves "this exact sequence of batches came from this one producer, in this order, and
//! nothing between them was ever seen." It says nothing about *where a batch landed on
//! disk*, because a partition (`shard_key`) can, and normally does, interleave batches
//! from several different producers -- `crates/av-ingest`'s whole partitioning scheme
//! depends on that being true (spoore's scaling model, `Measurement.shard_key`'s own doc
//! comment). This log's own `record_hash` chain instead proves "this exact sequence of
//! *records*, from however many different producers, is exactly what this partition file
//! has held from its first record to this one, and nothing in between was removed,
//! reordered, or altered after being appended" -- a property the per-producer chain
//! cannot express at all, since no single producer's chain spans more than its own
//! batches. Both chains are checked independently: `av_edge::chain::ChainVerifier`/
//! `av_edge::chain::walk_chain` for a producer's own signed sequence,
//! [`PartitionLog::verify`] for this file's own record sequence. A tamper that only
//! touched this log's on-disk bytes (never re-signing anything) breaks the partition chain
//! but leaves every batch's own producer-chain signature intact; a producer that forged or
//! reordered its own batches before ever reaching the ingest breaks the producer chain
//! but (if the forged batch was accepted) leaves the partition chain -- which only ever
//! sees what was actually appended -- looking perfectly consistent. Catching either kind
//! of tamper needs both chains checked, which is why this crate keeps them separate
//! rather than trying to fold one into the other.
//!
//! # Durability: what `sync_all` does and does not guarantee
//!
//! [`PartitionLog::append`] `write_all`s the full frame, then `flush`es and `sync_all`s
//! the file handle before returning `Ok`, so **the frame's own bytes are on durable
//! storage** (survive a process crash or an OS crash that does not also lose the disk
//! itself) by the time a caller sees a successful append. This does **not** guarantee:
//! (a) that the directory entry for a *newly created* partition file is itself durable --
//! that needs a separate `fsync` on the containing directory, which this module does not
//! perform, so a crash immediately after a partition file's first-ever creation could in
//! principle lose the whole file on some filesystems/OSes, not just its last record; (b)
//! anything about concurrent writers -- `PartitionLog` guards its own file handle and tip
//! state with a `Mutex` so two appends from *this one process* never interleave their
//! frames, but two separate processes appending to the same path is not a supported
//! configuration (matching `av-dynamics-service/src/evidence.rs`'s own "single-writer-
//! process log" caveat); (c) that the underlying storage device's own write cache is
//! itself durable -- `sync_all` asks the OS to flush to the device, which is as far as a
//! userspace process can reach.
//!
//! # Crash recovery
//!
//! [`PartitionLog::open`] always scans the whole file from byte 0 before doing anything
//! else. Two, and only two, things ever cause it to discard bytes and truncate:
//!
//! 1. **A torn tail** -- the file ends with fewer than 36 bytes remaining (an incomplete
//!    header) or with fewer than the declared `payload_len` payload bytes remaining (an
//!    incomplete payload). This is what a crash between `write_all` and the next
//!    `sync_all` looks like: every record before it was already durable (see above), and
//!    only the very last, in-flight append can be short.
//! 2. **A corrupt trailing record** -- the file's *very last* record is structurally
//!    complete (a full 36-byte header plus exactly `payload_len` payload bytes are
//!    present) but its `record_hash` does not match what is recomputed from the previous
//!    record's own hash and this record's payload, or its payload does not even decode as
//!    a `MeasurementBatch` at all. This covers a torn write that happens to leave the
//!    right *number* of bytes (e.g. a pre-allocated block that was never actually written)
//!    rather than a short file.
//!
//! Either way, [`PartitionLog::open`] reports exactly how many bytes were discarded and
//! why (a [`RecoveryReport`]), then truncates the file (`File::set_len`) to the byte
//! offset where the last good record ends, so the next [`PartitionLog::append`] extends a
//! chain that is valid from GENESIS all the way to its new tip -- never silently kept
//! (the bad tail is gone from the file), never silently dropped (the caller is told).
//!
//! **A corrupt record anywhere else (not the last one) is never touched by `open`.** If a
//! record in the middle of the file has been tampered with -- payload edited, hash left
//! stale -- `open` leaves every byte exactly as it found them (there is nothing "torn"
//! about a file whose length and per-record framing are both entirely intact) and
//! [`PartitionLog::verify`] is what reports it, as a broken chain at that record's index,
//! precisely because that is a tamper indication rather than an ordinary, expected
//! consequence of a crash mid-write, and truncating it away would destroy the very
//! evidence a reviewer needs to see.
//!
//! # `verify`
//!
//! [`PartitionLog::verify`] re-reads the file from disk (independent of any in-memory tip
//! state `append`/`open` maintain -- mirroring `av-dynamics-service/src/evidence.rs::
//! EvidenceLog::verify`'s own "walks the file straight from disk" contract) and returns
//! `av_cdm::pb::ChainVerification`, reused as-is rather than declaring a second struct
//! with the same shape (`producer_id`/`ok`/`checked`/`broken_at_sequence`/`detail`) --
//! here `producer_id` names this partition's own `shard_key` and `broken_at_sequence`
//! names the 1-based **record index** within this partition file (not any producer's own
//! `sequence` field, which this log does not interpret at all).

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use av_edge::hash;
use av_edge::pb;

/// Fixed per-record header size: 4 bytes little-endian `payload_len` + 32 raw
/// `record_hash` bytes. See the module doc's "Record framing" section.
const HEADER_LEN: usize = 4 + 32;

/// What can go wrong opening, appending to, or verifying a partition log. Every variant
/// is a refusal a caller must handle, never a panic (ADR-004: "everything rejected is
/// counted, never silent" -- applied here to the log itself, not only to a batch's own
/// verdict).
#[derive(Debug, thiserror::Error)]
pub enum LogError {
    /// `shard_key` cannot be turned into a safe file name under the log directory --
    /// empty, a bare `.`/`..` path-traversal component, or containing a path separator or
    /// NUL byte. A typed refusal, never a file written somewhere else (this crate's own
    /// path-traversal test asserts exactly that).
    #[error("shard_key {shard_key:?} is not a valid partition key ({reason}) -- refusing to write outside the log directory")]
    InvalidShardKey { shard_key: String, reason: &'static str },
    /// A payload (an encoded `MeasurementBatch`) is too large for this record framing's
    /// 32-bit length field. Not expected to be reachable by any batch this platform's
    /// producers actually build, but refused explicitly rather than silently truncating
    /// or wrapping the length.
    #[error("payload of {len} bytes does not fit in this record framing's 32-bit length field")]
    PayloadTooLarge { len: usize },
    /// An underlying filesystem operation failed (open, read, write, `sync_all`,
    /// `set_len`). `path` is included so a caller sees which partition file was affected
    /// without threading it through separately.
    #[error("I/O error on partition log {path}: {source}")]
    Io { path: String, #[source] source: std::io::Error },
}

impl LogError {
    fn io(path: &Path, source: std::io::Error) -> Self {
        LogError::Io { path: path.display().to_string(), source }
    }
}

/// What [`PartitionLog::open`] discarded from the tail of an existing partition file, and
/// why -- returned so a caller can assert on the *reported* discard, not only on the
/// resulting file's end state (a silent recovery is exactly the defect this type exists
/// to make impossible to overlook: there is no way to open a log whose tail needed
/// truncating without this value being handed back).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryReport {
    /// Bytes removed from the end of the file (0 is never reported -- this type is only
    /// ever constructed when something was actually discarded).
    pub discarded_bytes: u64,
    /// Human-readable reason: which of the two recovery cases (see the module doc's
    /// "Crash recovery" section) this was, with the concrete numbers involved.
    pub reason: String,
}

/// Sanitises `shard_key` into a file name confined to the log directory: refuses (as
/// [`LogError::InvalidShardKey`]) an empty key, a bare `.`/`..` component, or any key
/// containing `/`, `\`, or a NUL byte -- which between them rule out every way a path
/// string can name a file outside the directory it is joined onto (an embedded `../` is
/// caught by the separator check before the `..` component is ever reached). On success,
/// returns the file name (not a full path) to join onto the log directory -- since the
/// sanitised name can never contain a separator, `dir.join(name)` can never resolve
/// outside `dir` regardless of what `dir` itself is.
pub fn sanitize_shard_key(shard_key: &str) -> Result<String, LogError> {
    if shard_key.is_empty() {
        return Err(LogError::InvalidShardKey { shard_key: shard_key.to_string(), reason: "empty" });
    }
    if shard_key == "." || shard_key == ".." {
        return Err(LogError::InvalidShardKey { shard_key: shard_key.to_string(), reason: "is itself a path-traversal component (\".\" or \"..\")" });
    }
    if shard_key.chars().any(|c| c == '/' || c == '\\' || c == '\0') {
        return Err(LogError::InvalidShardKey { shard_key: shard_key.to_string(), reason: "contains a path separator or a NUL byte" });
    }
    Ok(format!("{shard_key}.avlog"))
}

/// One structurally-complete record found while scanning a file's bytes: byte offsets
/// only (no owned payload), since most callers of [`scan_frames`] only need the last
/// frame's payload.
struct FrameRef {
    start: usize,
    stored_hash: [u8; 32],
    payload_start: usize,
    payload_end: usize,
}

/// Walks `bytes` from the start, splitting it into structurally-complete records (see the
/// module doc's "Record framing"), then -- only if the scan reached a clean EOF with at
/// least one record -- checks whether the **last** record's own content is valid (its
/// `record_hash` matches what is recomputed from the previous record's hash and its own
/// payload, and its payload decodes as a `MeasurementBatch`). Returns every frame found
/// (with the last one dropped if it failed that content check) plus, if anything was
/// discarded, the byte offset the file should be truncated to and why. Never inspects any
/// record's content except the very last one -- see the module doc for why a bad *middle*
/// record is deliberately left for [`PartitionLog::verify`] to report instead.
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

fn truncate_file(path: &Path, len: u64) -> Result<(), LogError> {
    let f = OpenOptions::new().write(true).open(path).map_err(|e| LogError::io(path, e))?;
    f.set_len(len).map_err(|e| LogError::io(path, e))
}

#[derive(Debug)]
struct Tip {
    hash: Vec<u8>,
    record_count: u64,
}

/// One partition's durable, append-only, hash-chained log file. See the module doc for
/// the exact record framing and the recovery/verify contracts.
#[derive(Debug)]
pub struct PartitionLog {
    shard_key: String,
    path: PathBuf,
    file: Mutex<File>,
    tip: Mutex<Tip>,
}

impl PartitionLog {
    /// Opens (creating if necessary) the partition file for `shard_key` under `dir`,
    /// recovering from any torn or corrupt trailing record first (see the module doc).
    /// Returns the log plus `Some(RecoveryReport)` iff anything was discarded -- a caller
    /// that wants to know "did this reopen have to recover from something" checks this
    /// return value, never the log's own state after the fact.
    pub fn open(dir: &Path, shard_key: &str) -> Result<(Self, Option<RecoveryReport>), LogError> {
        let filename = sanitize_shard_key(shard_key)?;
        std::fs::create_dir_all(dir).map_err(|e| LogError::io(dir, e))?;
        let path = dir.join(&filename);

        let bytes = std::fs::read(&path).unwrap_or_default(); // a missing file reads as empty -- a fresh partition.
        let (frames, discard) = scan_frames(&bytes);

        let report = if let Some((discard_pos, reason)) = discard {
            // `discard` is only ever `Some` when `bytes` is non-empty (an empty/missing
            // file trivially has no records to discard), so the file this truncates
            // always exists on disk already.
            let discarded_bytes = (bytes.len() - discard_pos) as u64;
            truncate_file(&path, discard_pos as u64)?;
            Some(RecoveryReport { discarded_bytes, reason })
        } else {
            None
        };

        let (tip_hash, record_count) = match frames.last() {
            Some(f) => (f.stored_hash.to_vec(), frames.len() as u64),
            None => (hash::GENESIS.to_vec(), 0),
        };

        let file = OpenOptions::new().create(true).append(true).open(&path).map_err(|e| LogError::io(&path, e))?;
        Ok((Self { shard_key: shard_key.to_string(), path, file: Mutex::new(file), tip: Mutex::new(Tip { hash: tip_hash, record_count }) }, report))
    }

    pub fn shard_key(&self) -> &str {
        &self.shard_key
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// This partition's current chain head (the last appended record's own `record_hash`,
    /// 32 raw bytes), or [`hash::GENESIS`] if nothing has been appended yet.
    pub fn tip_hash(&self) -> Vec<u8> {
        self.tip.lock().unwrap_or_else(|p| p.into_inner()).hash.clone()
    }

    /// Number of records this partition currently holds on disk.
    pub fn record_count(&self) -> u64 {
        self.tip.lock().unwrap_or_else(|p| p.into_inner()).record_count
    }

    /// Appends `batch` as one new record, chained from this partition's current tip.
    /// Durable (flushed and `sync_all`'d) before returning `Ok` -- see the module doc's
    /// "Durability" section for exactly what that does and does not guarantee. Does not
    /// itself check `batch.shard_key` against `self.shard_key` -- that is
    /// `crate::ingest::Ingest::submit`'s job (this type only knows the partition it *is*,
    /// not what any particular batch claims to belong to).
    pub fn append(&self, batch: &pb::MeasurementBatch) -> Result<(), LogError> {
        let payload = prost::Message::encode_to_vec(batch);
        if payload.len() > u32::MAX as usize {
            return Err(LogError::PayloadTooLarge { len: payload.len() });
        }

        let mut tip = self.tip.lock().unwrap_or_else(|p| p.into_inner());
        let record_hash = hash::compute_batch_hash(&tip.hash, &payload);

        let mut frame = Vec::with_capacity(HEADER_LEN + payload.len());
        frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        frame.extend_from_slice(&record_hash);
        frame.extend_from_slice(&payload);

        {
            let mut f = self.file.lock().unwrap_or_else(|p| p.into_inner());
            f.write_all(&frame).map_err(|e| LogError::io(&self.path, e))?;
            f.flush().map_err(|e| LogError::io(&self.path, e))?;
            f.sync_all().map_err(|e| LogError::io(&self.path, e))?;
        }

        tip.hash = record_hash.to_vec();
        tip.record_count += 1;
        Ok(())
    }

    /// Independently re-derives this partition's chain validity by re-reading the file
    /// from disk (never from `self.tip`'s in-memory state), stopping at the first defect
    /// -- see the module doc's "`verify`" section for exactly what `producer_id`/
    /// `broken_at_sequence` mean for a partition log.
    pub fn verify(&self) -> Result<pb::ChainVerification, LogError> {
        let bytes = std::fs::read(&self.path).map_err(|e| LogError::io(&self.path, e))?;
        let len = bytes.len();
        let mut pos = 0usize;
        let mut expected_prev: Vec<u8> = hash::GENESIS.to_vec();
        let mut checked: u64 = 0;
        let mut idx: u64 = 0;

        loop {
            if pos == len {
                return Ok(pb::ChainVerification { producer_id: self.shard_key.clone(), ok: true, checked, broken_at_sequence: 0, detail: "chain intact".to_string() });
            }
            idx += 1;
            let remaining = len - pos;
            if remaining < HEADER_LEN {
                return Ok(pb::ChainVerification {
                    producer_id: self.shard_key.clone(),
                    ok: false,
                    checked,
                    broken_at_sequence: idx,
                    detail: format!("record {idx}: incomplete header at EOF ({remaining} byte(s) remain)"),
                });
            }
            let payload_len = u32::from_le_bytes(bytes[pos..pos + 4].try_into().expect("4-byte slice")) as usize;
            let stored_hash = &bytes[pos + 4..pos + HEADER_LEN];
            let payload_start = pos + HEADER_LEN;
            let payload_end = payload_start + payload_len;
            if payload_end > len {
                return Ok(pb::ChainVerification {
                    producer_id: self.shard_key.clone(),
                    ok: false,
                    checked,
                    broken_at_sequence: idx,
                    detail: format!("record {idx}: incomplete payload at EOF (declared {payload_len} byte(s))"),
                });
            }
            let payload = &bytes[payload_start..payload_end];
            let recomputed = hash::compute_batch_hash(&expected_prev, payload);
            if recomputed.as_slice() != stored_hash {
                return Ok(pb::ChainVerification {
                    producer_id: self.shard_key.clone(),
                    ok: false,
                    checked,
                    broken_at_sequence: idx,
                    detail: format!("record {idx}: stored hash does not match its recomputed content hash -- the record was tampered with after being appended"),
                });
            }
            expected_prev = recomputed.to_vec();
            checked += 1;
            pos = payload_end;
        }
    }
}
