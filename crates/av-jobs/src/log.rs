//! The durable, hash-chained, file-backed job queue log -- this crate's copy of the pattern
//! `crates/av-ingest/src/log.rs::PartitionLog` already established for the edge ingest
//! ledger (ADR-004: "for the engine and command services the durable log itself is the
//! ledger, chained per partition"; `docs/heavy-plan.md` H3: "a durable, hash-chained
//! file-backed queue (the pattern of the ingest log; a broker later, ADR-003)"). One
//! append-only file per named queue, sanitised so a queue `name` can never escape
//! [`JobLog::open`]'s own directory.
//!
//! # Record framing (byte for byte -- a future reader needs nothing but this comment)
//!
//! A queue file is a flat sequence of records, back to back, with **no** file-level header
//! and **no** trailing padding. Each record is:
//!
//! ```text
//! +----------------------+----------------------------+---------------------------+
//! | payload_len: u32 LE  | record_hash: [u8; 32]      | payload: payload_len bytes|
//! | (4 bytes)            | (32 bytes, raw, not hex)   |                           |
//! +----------------------+----------------------------+---------------------------+
//! ```
//!
//! - `payload_len` is a little-endian `u32`: the exact byte length of `payload` that follows
//!   this record's own 36-byte fixed header (`4 + 32`).
//! - `payload` is `prost::Message::encode_to_vec` of an `altavista.v1.JobLogRecord` **as a
//!   whole** -- either its `submitted` (`JobSpec`) or `completed` (`JobCompletion`) oneof
//!   variant, encoded exactly as the crate that appended it built the message. Every
//!   `map<..>` field anywhere in `av_cdm::pb` (`JobSpec.parameters` included) is generated as
//!   a `BTreeMap` (`av-cdm/build.rs`'s `.btree_map(["."])`), so this encoding is a
//!   deterministic function of the record's field values, which is what makes both this
//!   log's own bytes reproducible run to run and `JobCompletion.spec_sha256` a meaningful
//!   fingerprint of a `JobSpec`'s contents.
//! - `record_hash` is `SHA-256(prev_record_hash_bytes || payload)` -- via
//!   [`crate::hash::chain_hash`], reimplemented rather than depending on `av-edge` (see
//!   `crate::hash`'s own module doc for why). `prev_record_hash_bytes` is the literal ASCII
//!   bytes of [`crate::hash::GENESIS`] for a queue's first record, or else the *previous
//!   record's own* 32 raw `record_hash` bytes -- the exact convention
//!   `av_edge::hash`/`crates/av-dynamics-service/src/evidence.rs` both already use for their
//!   own chains, applied here to this crate's own, independent chain.
//!
//! To parse a queue file by hand: read 4 bytes (LE `u32`) for `payload_len`, read the next
//! 32 bytes as `record_hash`, read `payload_len` more bytes as `payload`, decode `payload` as
//! an `altavista.v1.JobLogRecord`, and repeat from the next byte until EOF. A file that ends
//! with fewer than 36 bytes remaining, or with fewer than `payload_len` payload bytes
//! remaining, ends mid-record -- see "Crash recovery" below.
//!
//! # Durability: what `sync_all` does and does not guarantee
//!
//! [`JobLog::append`] `write_all`s the full frame, then `flush`es and `sync_all`s the file
//! handle before returning `Ok`, so **the frame's own bytes are on durable storage** (survive
//! a process crash or an OS crash that does not also lose the disk itself) by the time a
//! caller sees a successful append. This does **not** guarantee: (a) that the directory
//! entry for a *newly created* queue file is itself durable -- that needs a separate `fsync`
//! on the containing directory, which this module does not perform, so a crash immediately
//! after a queue file's first-ever creation could in principle lose the whole file on some
//! filesystems/OSes, not just its last record; (b) anything about concurrent writers --
//! `JobLog` guards its own file handle and tip state with a `Mutex` so two appends from
//! *this one process* never interleave their frames, but two separate processes appending to
//! the same path is not a supported configuration (matching `crates/av-ingest/src/
//! log.rs::PartitionLog`'s and `av-dynamics-service/src/evidence.rs`'s own
//! "single-writer-process log" caveat); (c) anything about the underlying storage device's
//! own write cache -- `sync_all` asks the OS to flush to the device, which is as far as a
//! userspace process can reach.
//!
//! # Crash recovery
//!
//! [`JobLog::open`] always scans the whole file from byte 0 before doing anything else. Two,
//! and only two, things ever cause it to discard bytes and truncate:
//!
//! 1. **A torn tail** -- the file ends with fewer than 36 bytes remaining (an incomplete
//!    header) or with fewer than the declared `payload_len` payload bytes remaining (an
//!    incomplete payload). This is what a crash between `write_all` and the next `sync_all`
//!    looks like: every record before it was already durable (see above), and only the very
//!    last, in-flight append can be short.
//! 2. **A corrupt trailing record** -- the file's *very last* record is structurally
//!    complete (a full 36-byte header plus exactly `payload_len` payload bytes are present)
//!    but its `record_hash` does not match what is recomputed from the previous record's own
//!    hash and this record's payload, or its payload does not even decode as a
//!    `JobLogRecord` at all. This covers a torn write that happens to leave the right
//!    *number* of bytes (e.g. a pre-allocated block that was never actually written) rather
//!    than a short file.
//!
//! Either way, [`JobLog::open`] reports exactly how many bytes were discarded and why (a
//! [`RecoveryReport`]), then truncates the file (`File::set_len`) to the byte offset where
//! the last good record ends, so the next [`JobLog::append`] extends a chain that is valid
//! from GENESIS all the way to its new tip -- never silently kept (the bad tail is gone from
//! the file), never silently dropped (the caller is told).
//!
//! **A corrupt record anywhere else (not the last one) is never touched by `open`.** If a
//! record in the middle of the file has been tampered with -- payload edited, hash left
//! stale -- `open` leaves every byte exactly as it found them (there is nothing "torn" about
//! a file whose length and per-record framing are both entirely intact) and [`JobLog::
//! verify`] is what reports it, as a broken chain at that record's index, precisely because
//! that is a tamper indication rather than an ordinary, expected consequence of a crash
//! mid-write, and truncating it away would destroy the very evidence a reviewer needs to see.
//!
//! # `verify`
//!
//! [`JobLog::verify`] re-reads the file from disk (independent of any in-memory tip state
//! `append`/`open` maintain -- mirroring `crates/av-ingest/src/log.rs::PartitionLog::verify`'s
//! own "walks the file straight from disk" contract) and returns `av_cdm::pb::
//! ChainVerification`, reused as-is rather than declaring a second struct with the same shape
//! (`producer_id`/`ok`/`checked`/`broken_at_sequence`/`detail`) -- here `producer_id` names
//! this queue's own `name` and `broken_at_sequence` names the 1-based **record index** within
//! this queue file.

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use av_cdm::pb;

use crate::hash;

/// Fixed per-record header size: 4 bytes little-endian `payload_len` + 32 raw `record_hash`
/// bytes. See the module doc's "Record framing" section.
const HEADER_LEN: usize = 4 + 32;

/// What can go wrong opening, appending to, or reading a job queue log. Every variant is a
/// refusal a caller must handle, never a panic.
#[derive(Debug, thiserror::Error)]
pub enum JobLogError {
    /// `name` cannot be turned into a safe file name under the log directory -- empty, a
    /// bare `.`/`..` path-traversal component, or containing a path separator or NUL byte.
    #[error("queue name {name:?} is not a valid job log name ({reason}) -- refusing to write outside the log directory")]
    InvalidName { name: String, reason: &'static str },
    /// A payload (an encoded `JobLogRecord`) is too large for this record framing's 32-bit
    /// length field.
    #[error("payload of {len} bytes does not fit in this record framing's 32-bit length field")]
    PayloadTooLarge { len: usize },
    /// An underlying filesystem operation failed (open, read, write, `sync_all`, `set_len`).
    #[error("I/O error on job log {path}: {source}")]
    Io { path: String, #[source] source: std::io::Error },
    /// A record's payload (or its structural framing) could not be decoded as a
    /// `JobLogRecord` -- returned by [`JobLog::read_all`], which (unlike [`JobLog::verify`])
    /// must actually decode every record it reads back.
    #[error("record {index} in job log {path} does not decode as a JobLogRecord: {detail}")]
    Decode { path: String, index: u64, detail: String },
}

impl JobLogError {
    fn io(path: &Path, source: std::io::Error) -> Self {
        JobLogError::Io { path: path.display().to_string(), source }
    }
}

/// What [`JobLog::open`] discarded from the tail of an existing queue file, and why --
/// returned so a caller can assert on the *reported* discard, not only on the resulting
/// file's end state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryReport {
    /// Bytes removed from the end of the file (0 is never reported -- this type is only ever
    /// constructed when something was actually discarded).
    pub discarded_bytes: u64,
    /// Human-readable reason: which of the two recovery cases (see the module doc's "Crash
    /// recovery" section) this was, with the concrete numbers involved.
    pub reason: String,
}

/// Sanitises `name` into a file name confined to the log directory: refuses (as
/// [`JobLogError::InvalidName`]) an empty name, a bare `.`/`..` component, or any name
/// containing `/`, `\`, or a NUL byte -- the identical rule `crates/av-ingest/src/
/// log.rs::sanitize_shard_key` applies to its own partition keys. On success, returns the
/// file name (not a full path) to join onto the log directory -- since the sanitised name can
/// never contain a separator, `dir.join(name)` can never resolve outside `dir`.
pub fn sanitize_log_name(name: &str) -> Result<String, JobLogError> {
    if name.is_empty() {
        return Err(JobLogError::InvalidName { name: name.to_string(), reason: "empty" });
    }
    if name == "." || name == ".." {
        return Err(JobLogError::InvalidName { name: name.to_string(), reason: "is itself a path-traversal component (\".\" or \"..\")" });
    }
    if name.chars().any(|c| c == '/' || c == '\\' || c == '\0') {
        return Err(JobLogError::InvalidName { name: name.to_string(), reason: "contains a path separator or a NUL byte" });
    }
    Ok(format!("{name}.avjobs"))
}

/// One structurally-complete record found while scanning a file's bytes: byte offsets only.
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
/// payload, and its payload decodes as a `JobLogRecord`). Returns every frame found (with the
/// last one dropped if it failed that content check) plus, if anything was discarded, the
/// byte offset the file should be truncated to and why. Never inspects any record's content
/// except the very last one -- see the module doc for why a bad *middle* record is
/// deliberately left for [`JobLog::verify`] to report instead.
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
        let recomputed = hash::chain_hash(&prev_hash, payload);
        let decodes = <pb::JobLogRecord as prost::Message>::decode(payload).is_ok();
        if recomputed != last.stored_hash || !decodes {
            let discard_pos = last.start;
            let discarded = (len - discard_pos) as u64;
            let mut kept = frames;
            kept.pop();
            return (
                kept,
                Some((discard_pos, format!("trailing record's content is corrupt ({discarded} byte(s)): stored hash does not match its recomputed content, or the payload does not decode as a JobLogRecord"))),
            );
        }
    }
    (frames, None)
}

fn truncate_file(path: &Path, len: u64) -> Result<(), JobLogError> {
    let f = OpenOptions::new().write(true).open(path).map_err(|e| JobLogError::io(path, e))?;
    f.set_len(len).map_err(|e| JobLogError::io(path, e))
}

#[derive(Debug)]
struct Tip {
    hash: Vec<u8>,
    record_count: u64,
}

/// One job queue's durable, append-only, hash-chained log file. See the module doc for the
/// exact record framing and the recovery/verify contracts.
#[derive(Debug)]
pub struct JobLog {
    name: String,
    path: PathBuf,
    file: Mutex<File>,
    tip: Mutex<Tip>,
}

impl JobLog {
    /// Opens (creating if necessary) the queue file for `name` under `dir`, recovering from
    /// any torn or corrupt trailing record first (see the module doc). Returns the log plus
    /// `Some(RecoveryReport)` iff anything was discarded -- a caller that wants to know "did
    /// this reopen have to recover from something" checks this return value, never the log's
    /// own state after the fact.
    pub fn open(dir: &Path, name: &str) -> Result<(Self, Option<RecoveryReport>), JobLogError> {
        let filename = sanitize_log_name(name)?;
        std::fs::create_dir_all(dir).map_err(|e| JobLogError::io(dir, e))?;
        let path = dir.join(&filename);

        let bytes = std::fs::read(&path).unwrap_or_default(); // a missing file reads as empty -- a fresh queue.
        let (frames, discard) = scan_frames(&bytes);

        let report = if let Some((discard_pos, reason)) = discard {
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

        let file = OpenOptions::new().create(true).append(true).open(&path).map_err(|e| JobLogError::io(&path, e))?;
        Ok((Self { name: name.to_string(), path, file: Mutex::new(file), tip: Mutex::new(Tip { hash: tip_hash, record_count }) }, report))
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// This queue's current chain head (the last appended record's own `record_hash`, 32
    /// raw bytes), or [`crate::hash::GENESIS`] if nothing has been appended yet.
    pub fn tip_hash(&self) -> Vec<u8> {
        self.tip.lock().unwrap_or_else(|p| p.into_inner()).hash.clone()
    }

    /// Number of records this queue currently holds on disk.
    pub fn record_count(&self) -> u64 {
        self.tip.lock().unwrap_or_else(|p| p.into_inner()).record_count
    }

    /// Appends `record` as one new record, chained from this queue's current tip. Durable
    /// (flushed and `sync_all`'d) before returning `Ok` -- see the module doc's "Durability"
    /// section for exactly what that does and does not guarantee.
    pub fn append(&self, record: &pb::JobLogRecord) -> Result<(), JobLogError> {
        let payload = prost::Message::encode_to_vec(record);
        if payload.len() > u32::MAX as usize {
            return Err(JobLogError::PayloadTooLarge { len: payload.len() });
        }

        let mut tip = self.tip.lock().unwrap_or_else(|p| p.into_inner());
        let record_hash = hash::chain_hash(&tip.hash, &payload);

        let mut frame = Vec::with_capacity(HEADER_LEN + payload.len());
        frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        frame.extend_from_slice(&record_hash);
        frame.extend_from_slice(&payload);

        {
            let mut f = self.file.lock().unwrap_or_else(|p| p.into_inner());
            f.write_all(&frame).map_err(|e| JobLogError::io(&self.path, e))?;
            f.flush().map_err(|e| JobLogError::io(&self.path, e))?;
            f.sync_all().map_err(|e| JobLogError::io(&self.path, e))?;
        }

        tip.hash = record_hash.to_vec();
        tip.record_count += 1;
        Ok(())
    }

    /// Independently re-derives this queue's chain validity by re-reading the file from disk
    /// (never from `self.tip`'s in-memory state), stopping at the first defect -- see the
    /// module doc's "`verify`" section for exactly what `producer_id`/`broken_at_sequence`
    /// mean for a job log. Does **not** decode any record's payload as a `JobLogRecord`
    /// (that is [`JobLog::read_all`]'s job) -- this function only ever re-derives the hash
    /// chain, exactly like `crates/av-ingest/src/log.rs::PartitionLog::verify`.
    pub fn verify(&self) -> Result<pb::ChainVerification, JobLogError> {
        let bytes = std::fs::read(&self.path).map_err(|e| JobLogError::io(&self.path, e))?;
        let len = bytes.len();
        let mut pos = 0usize;
        let mut expected_prev: Vec<u8> = hash::GENESIS.to_vec();
        let mut checked: u64 = 0;
        let mut idx: u64 = 0;

        loop {
            if pos == len {
                return Ok(pb::ChainVerification { producer_id: self.name.clone(), ok: true, checked, broken_at_sequence: 0, detail: "chain intact".to_string() });
            }
            idx += 1;
            let remaining = len - pos;
            if remaining < HEADER_LEN {
                return Ok(pb::ChainVerification {
                    producer_id: self.name.clone(),
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
                    producer_id: self.name.clone(),
                    ok: false,
                    checked,
                    broken_at_sequence: idx,
                    detail: format!("record {idx}: incomplete payload at EOF (declared {payload_len} byte(s))"),
                });
            }
            let payload = &bytes[payload_start..payload_end];
            let recomputed = hash::chain_hash(&expected_prev, payload);
            if recomputed.as_slice() != stored_hash {
                return Ok(pb::ChainVerification {
                    producer_id: self.name.clone(),
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

    /// Re-reads the file from disk and decodes every record as a `JobLogRecord`, in order --
    /// what [`crate::queue::JobQueue`] and this crate's own tests use to replay the chain.
    /// Does not itself check the hash chain (that is [`JobLog::verify`]'s job); a record
    /// whose payload does not even decode is a [`JobLogError::Decode`], naming its 1-based
    /// index.
    pub fn read_all(&self) -> Result<Vec<pb::JobLogRecord>, JobLogError> {
        let bytes = std::fs::read(&self.path).map_err(|e| JobLogError::io(&self.path, e))?;
        let len = bytes.len();
        let mut pos = 0usize;
        let mut idx: u64 = 0;
        let mut out = Vec::new();

        while pos < len {
            idx += 1;
            let remaining = len - pos;
            if remaining < HEADER_LEN {
                return Err(JobLogError::Decode { path: self.path.display().to_string(), index: idx, detail: format!("incomplete record header at EOF ({remaining} byte(s) remain)") });
            }
            let payload_len = u32::from_le_bytes(bytes[pos..pos + 4].try_into().expect("4-byte slice")) as usize;
            let payload_start = pos + HEADER_LEN;
            let payload_end = payload_start + payload_len;
            if payload_end > len {
                return Err(JobLogError::Decode { path: self.path.display().to_string(), index: idx, detail: format!("incomplete record payload at EOF (declared {payload_len} byte(s))") });
            }
            let payload = &bytes[payload_start..payload_end];
            let record = <pb::JobLogRecord as prost::Message>::decode(payload)
                .map_err(|e| JobLogError::Decode { path: self.path.display().to_string(), index: idx, detail: e.to_string() })?;
            out.push(record);
            pos = payload_end;
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A directory under the OS temp dir, unique to this test process and this call --
    /// never derived from the system clock (this crate's own binding rule: clocks are
    /// injected, never read inside library code; a test-only unique-name helper stays
    /// consistent with that rather than carving out an exception). Removed on drop, so a
    /// real filesystem is exercised (`RecoveryReport`/`verify` need real file I/O, never a
    /// mock) without leaving litter behind across test runs.
    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let n = COUNTER.fetch_add(1, Ordering::SeqCst);
            let dir = std::env::temp_dir().join(format!("av-jobs-log-test-{tag}-{}-{n}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("create temp dir");
            Self(dir)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn spec_record(job_id: &str) -> pb::JobLogRecord {
        pb::JobLogRecord { event: Some(pb::job_log_record::Event::Submitted(pb::JobSpec { job_id: job_id.to_string(), kind: "echo".to_string(), requested_tai_ns: 1, ..Default::default() })) }
    }

    fn completion_record(job_id: &str) -> pb::JobLogRecord {
        pb::JobLogRecord { event: Some(pb::job_log_record::Event::Completed(pb::JobCompletion { job_id: job_id.to_string(), ok: true, ..Default::default() })) }
    }

    // -- sanitize_log_name -------------------------------------------------------------

    #[test]
    fn sanitize_log_name_refuses_empty_dot_and_dotdot() {
        assert!(matches!(sanitize_log_name(""), Err(JobLogError::InvalidName { .. })));
        assert!(matches!(sanitize_log_name("."), Err(JobLogError::InvalidName { .. })));
        assert!(matches!(sanitize_log_name(".."), Err(JobLogError::InvalidName { .. })));
    }

    #[test]
    fn sanitize_log_name_refuses_separators_and_nul() {
        assert!(matches!(sanitize_log_name("a/b"), Err(JobLogError::InvalidName { .. })));
        assert!(matches!(sanitize_log_name("a\\b"), Err(JobLogError::InvalidName { .. })));
        assert!(matches!(sanitize_log_name("a\0b"), Err(JobLogError::InvalidName { .. })));
    }

    #[test]
    fn sanitize_log_name_confines_the_file_to_the_directory() {
        let dir = TempDir::new("traversal");
        let err = JobLog::open(dir.path(), "../escape").unwrap_err();
        assert!(matches!(err, JobLogError::InvalidName { .. }), "{err:?}");
        // No file was created outside dir.path() -- nothing to assert on disk beyond the
        // typed refusal itself, since a name containing '/' never reaches std::fs at all.
    }

    // -- basic open/append/verify --------------------------------------------------------

    #[test]
    fn fresh_log_starts_at_genesis_with_zero_records() {
        let dir = TempDir::new("fresh");
        let (log, report) = JobLog::open(dir.path(), "q").unwrap();
        assert!(report.is_none());
        assert_eq!(log.record_count(), 0);
        assert_eq!(log.tip_hash(), hash::GENESIS.to_vec());
    }

    #[test]
    fn append_extends_the_chain_and_verify_reports_it_intact() {
        let dir = TempDir::new("append");
        let (log, _) = JobLog::open(dir.path(), "q").unwrap();
        for i in 0..5 {
            log.append(&spec_record(&format!("job-{i}"))).unwrap();
        }
        assert_eq!(log.record_count(), 5);
        let v = log.verify().unwrap();
        assert!(v.ok, "{v:?}");
        assert_eq!(v.checked, 5);
        assert_eq!(v.broken_at_sequence, 0);
    }

    #[test]
    fn read_all_decodes_every_record_in_order() {
        let dir = TempDir::new("readall");
        let (log, _) = JobLog::open(dir.path(), "q").unwrap();
        log.append(&spec_record("job-a")).unwrap();
        log.append(&completion_record("job-a")).unwrap();
        let records = log.read_all().unwrap();
        assert_eq!(records.len(), 2);
        assert!(matches!(&records[0].event, Some(pb::job_log_record::Event::Submitted(s)) if s.job_id == "job-a"));
        assert!(matches!(&records[1].event, Some(pb::job_log_record::Event::Completed(c)) if c.job_id == "job-a"));
    }

    fn payload_len_of(record: &pb::JobLogRecord) -> u64 {
        prost::Message::encode_to_vec(record).len() as u64
    }

    // -- acceptance evidence item 1: a crash mid-append leaves the queue verifiable -----

    /// Writes three records, then truncates the file so that exactly `bytes_of_last_record`
    /// bytes of the THIRD record remain present (0 discards the whole record; a value
    /// between 0 and `HEADER_LEN` lands inside the header; a value between `HEADER_LEN` and
    /// the record's own full length lands inside the payload). Returns the queue directory
    /// (kept alive by the caller), the record's own full framed length, and
    /// `bytes_of_last_record` unchanged, so callers can assert `discarded_bytes` against a
    /// value derived independently of `JobLog::open`'s own implementation: it is exactly
    /// the number of bytes of the third record that were present in the truncated file (the
    /// scan can only ever discard what full record framing it can see is broken, i.e.
    /// everything from that record's own start to EOF -- never more, never less).
    fn write_three_then_tear_the_third(dir: &Path, bytes_of_third_record: u64) -> u64 {
        let (log, _) = JobLog::open(dir, "q").unwrap();
        for i in 0..3 {
            log.append(&spec_record(&format!("job-{i}"))).unwrap();
        }
        let rec_len = HEADER_LEN as u64 + payload_len_of(&spec_record("job-0"));
        // All three records encode to the same length (identical-shape JobSpecs, job_id
        // strings all the same length) -- confirmed directly against the file, not assumed.
        assert_eq!(log.path().metadata().unwrap().len(), 3 * rec_len, "this test's own premise: three equal-length records");
        assert!(bytes_of_third_record < rec_len, "must actually be torn, not a complete third record");
        let torn_len = 2 * rec_len + bytes_of_third_record;
        let f = OpenOptions::new().write(true).open(log.path()).unwrap();
        f.set_len(torn_len).unwrap();
        rec_len
    }

    #[test]
    fn recovers_from_a_torn_tail_inside_the_header() {
        let dir = TempDir::new("torn-header");
        // 20 bytes of the third record survive -- less than HEADER_LEN (36), so the tear
        // lands inside the fixed 4+32-byte header itself, before any payload byte at all.
        let bytes_present = 20u64;
        assert!(bytes_present < HEADER_LEN as u64);
        write_three_then_tear_the_third(dir.path(), bytes_present);

        let (log, report) = JobLog::open(dir.path(), "q").unwrap();
        let report = report.expect("a torn header must be reported, never silently kept");
        assert_eq!(report.discarded_bytes, bytes_present, "everything present of the torn third record is discarded, no more and no less");
        assert!(report.reason.contains("incomplete"), "{report:?}");
        assert!(report.reason.contains("header"), "{report:?}");

        let v = log.verify().unwrap();
        assert!(v.ok, "{v:?}");
        assert_eq!(v.checked, 2, "the torn third record must be gone, leaving the first two intact");

        log.append(&spec_record("job-recovered")).unwrap();
        let v = log.verify().unwrap();
        assert!(v.ok, "{v:?}");
        assert_eq!(v.checked, 3, "a further append must extend a chain valid from GENESIS to its new tip");
    }

    #[test]
    fn recovers_from_a_torn_tail_inside_the_payload() {
        let dir = TempDir::new("torn-payload");
        // HEADER_LEN + 4 bytes of the third record survive -- past the full 36-byte header
        // (so payload_len/record_hash both parse cleanly), but short of its full payload.
        let bytes_present = HEADER_LEN as u64 + 4;
        let rec_len = write_three_then_tear_the_third(dir.path(), bytes_present);
        assert!(bytes_present < rec_len, "this test's own premise: still short of the full record");

        let (log, report) = JobLog::open(dir.path(), "q").unwrap();
        let report = report.expect("a torn payload must be reported, never silently kept");
        assert_eq!(report.discarded_bytes, bytes_present, "everything present of the torn third record is discarded, no more and no less");
        assert!(report.reason.contains("incomplete"), "{report:?}");
        assert!(report.reason.contains("payload"), "{report:?}");

        let v = log.verify().unwrap();
        assert!(v.ok, "{v:?}");
        assert_eq!(v.checked, 2, "the torn third record must be gone, leaving the first two intact");

        log.append(&spec_record("job-recovered")).unwrap();
        let v = log.verify().unwrap();
        assert!(v.ok, "{v:?}");
        assert_eq!(v.checked, 3, "a further append must extend a chain valid from GENESIS to its new tip");
    }

    #[test]
    fn recovers_from_a_corrupt_trailing_record() {
        let dir = TempDir::new("corrupt-trailing");
        let path;
        {
            let (log, _) = JobLog::open(dir.path(), "q").unwrap();
            for i in 0..3 {
                log.append(&spec_record(&format!("job-{i}"))).unwrap();
            }
            path = log.path().to_path_buf();
        }
        // Flip a byte inside the LAST record's payload, leaving the file's length and
        // per-record framing (payload_len, the 36-byte header) entirely intact -- a
        // structurally-complete but content-corrupt trailing record, the second recovery
        // case (not a torn tail).
        let mut bytes = std::fs::read(&path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xFF;
        std::fs::write(&path, &bytes).unwrap();

        let (log, report) = JobLog::open(dir.path(), "q").unwrap();
        let report = report.expect("a corrupt trailing record must be reported, never silently kept");
        assert!(report.reason.contains("corrupt"), "{report:?}");

        let v = log.verify().unwrap();
        assert!(v.ok, "{v:?}");
        assert_eq!(v.checked, 2, "the corrupted third record must be gone, leaving the first two intact");
    }

    // -- acceptance evidence item 2: a corrupt MIDDLE record is left alone, and reported by verify --

    #[test]
    fn a_corrupt_middle_record_is_not_truncated_and_is_reported_by_verify() {
        let dir = TempDir::new("corrupt-middle");
        let path;
        {
            let (log, _) = JobLog::open(dir.path(), "q").unwrap();
            for i in 0..4 {
                log.append(&spec_record(&format!("job-{i}"))).unwrap();
            }
            path = log.path().to_path_buf();
        }
        let original = std::fs::read(&path).unwrap();
        let original_len = original.len();

        // Locate record 2 (1-based) by walking the framing by hand, then flip a byte in
        // its payload -- never touching record 1, 3 or 4's own bytes.
        let mut pos = 0usize;
        let mut frame_bounds = Vec::new();
        while pos < original.len() {
            let payload_len = u32::from_le_bytes(original[pos..pos + 4].try_into().unwrap()) as usize;
            let payload_start = pos + HEADER_LEN;
            let payload_end = payload_start + payload_len;
            frame_bounds.push((pos, payload_start, payload_end));
            pos = payload_end;
        }
        assert_eq!(frame_bounds.len(), 4);
        let (_, p2_start, _) = frame_bounds[1];

        let mut corrupted = original.clone();
        corrupted[p2_start] ^= 0xFF; // flip the first payload byte of record 2 (1-based)

        std::fs::write(&path, &corrupted).unwrap();

        let (log, report) = JobLog::open(dir.path(), "q").unwrap();
        assert!(report.is_none(), "a corrupt MIDDLE record must never be truncated by open(): {report:?}");
        assert_eq!(std::fs::metadata(&path).unwrap().len() as usize, original_len, "not one byte of the file may be removed");

        let v = log.verify().unwrap();
        assert!(!v.ok, "verify() must report the chain broken");
        assert_eq!(v.broken_at_sequence, 2, "record 2 (1-based) is the one that was tampered with");
        assert_eq!(v.checked, 1, "only record 1 verified clean before the break was found");
    }

    // -- acceptance evidence item 6 lives in crate::queue's own tests (JobQueue::submit) --

    // -- acceptance evidence item 7: the chain framing is pinned -----------------------

    /// A byte-for-byte golden: two known `JobLogRecord`s, appended under a fixed sequence,
    /// asserted against a tip hash computed **independently of [`hash::chain_hash`]** --
    /// never by calling the function under test.
    ///
    /// Derivation (recorded here so the pin can be re-derived by a reviewer without running
    /// this crate's own code): the two records below were built once, `prost::Message::
    /// encode_to_vec`'d (prost's wire encoding is not the function under test -- it is a
    /// third-party, independently-tested library, exactly as trustworthy here as it is
    /// everywhere else this codebase already depends on it), and the resulting bytes printed
    /// as hex:
    ///
    /// - record 1 payload (`Submitted(JobSpec { job_id: "job-1", kind: "echo",
    ///   requested_tai_ns: 1000, .. })`): `0a100a056a6f622d3112046563686f30e807`
    /// - record 2 payload (`Completed(JobCompletion { job_id: "job-1", spec_sha256:
    ///   "deadbeef", ok: true, started_tai_ns: 1000, finished_tai_ns: 2000, .. })`):
    ///   `12190a056a6f622d311208646561646265656630e80738d00f4001`
    ///
    /// Those two hex strings were then hashed independently, **twice, with two different
    /// tools** (`openssl(1)` and Python's `hashlib`, agreeing byte for byte), per the
    /// documented rule `record_hash = SHA-256(prev_record_hash_bytes || payload)`:
    ///
    /// ```text
    /// GENESIS_HEX=$(printf 'GENESIS' | xxd -p | tr -d '\n')          # 47454e45534953
    /// RH1=$( (echo -n "$GENESIS_HEX$PAYLOAD1_HEX" | xxd -r -p) | openssl dgst -sha256 -r)
    /// # RH1 = a8fef9a7a024931132e878f67ece21654414873ecf6f8dc05d0f6ade1eb96403
    /// RH2=$( (echo -n "$RH1$PAYLOAD2_HEX" | xxd -r -p) | openssl dgst -sha256 -r)
    /// # RH2 (== the tip hash asserted below) =
    /// #   4ea2cf4a603eb57eb583c6fee4ca7dbce13ccc70548109404fefef4aed10c241
    /// ```
    #[test]
    fn tip_hash_is_pinned_to_an_independently_derived_golden() {
        let dir = TempDir::new("golden");
        let (log, _) = JobLog::open(dir.path(), "q").unwrap();

        let record1 = pb::JobLogRecord {
            event: Some(pb::job_log_record::Event::Submitted(pb::JobSpec { job_id: "job-1".to_string(), kind: "echo".to_string(), requested_tai_ns: 1000, ..Default::default() })),
        };
        let record2 = pb::JobLogRecord {
            event: Some(pb::job_log_record::Event::Completed(pb::JobCompletion {
                job_id: "job-1".to_string(),
                spec_sha256: "deadbeef".to_string(),
                ok: true,
                started_tai_ns: 1000,
                finished_tai_ns: 2000,
                ..Default::default()
            })),
        };

        // Pin the exact wire bytes too, so a future prost/proto change that silently
        // altered the encoding would fail loudly here rather than only in the tip-hash
        // assertion below.
        assert_eq!(hash::hex_encode(&prost::Message::encode_to_vec(&record1)), "0a100a056a6f622d3112046563686f30e807");
        assert_eq!(hash::hex_encode(&prost::Message::encode_to_vec(&record2)), "12190a056a6f622d311208646561646265656630e80738d00f4001");

        log.append(&record1).unwrap();
        log.append(&record2).unwrap();

        let expected_tip_hex = "4ea2cf4a603eb57eb583c6fee4ca7dbce13ccc70548109404fefef4aed10c241";
        assert_eq!(hash::hex_encode(&log.tip_hash()), expected_tip_hex);

        // verify() must independently agree too (it re-derives the whole chain from disk).
        let v = log.verify().unwrap();
        assert!(v.ok, "{v:?}");
        assert_eq!(v.checked, 2);
    }
}
