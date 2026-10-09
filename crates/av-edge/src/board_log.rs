//! The board's I/O log (question 242 (a); ADR-005 section 4: "`BOARD`: ... every input and
//! output logged as a signed batch (ADR-004) so the rest of the run replays"; architecture.md
//! section 3: "A HIL run logs the board's I/O on the durable log so everything else still
//! replays"). Written on the edge side by `crates/av-edge-board`, read back by the replay and
//! by the `av-edge-board-log` tool.
//!
//! A [`pb::BoardIoRecord`] is one exchange between the edge node and the board's flight
//! software (BIND, STEP, RESET, SHUTDOWN; a power-cycle kind is reserved). It carries the
//! port payloads exactly as they crossed the link, so a replay can substitute the log for the
//! board. `MeasurementBatch` cannot carry port frames, hence the separate record.
//!
//! # The chain and the signature
//!
//! The definition of `edge.proto`'s header comment, applied to `BoardIoRecord`:
//!
//! 1. **body bytes** = `prost` encoding of the record with `record_hash` and `signature`
//!    cleared ([`canonical_body_bytes`]); `prev_hash` is left in, so it is signed content.
//! 2. **`prev_hash`** = the ASCII bytes of [`GENESIS`] for the first record, else the
//!    previous record's 32 raw `record_hash` bytes.
//! 3. **`record_hash`** = `SHA-256(prev_hash || body bytes)` ([`compute_record_hash`], via
//!    `openssl`, reusing `crate::hash`).
//! 4. **`signature`** = ECDSA P-384 over the 32 `record_hash` bytes, DER ([`seal`], reusing
//!    `crate::sign::sign_digest`). OpenSSL's ECDSA nonce is random, so the signature (and
//!    with it the encoded record and the file) is not byte-reproducible; `record_hash` is.
//!
//! The record also carries its position (`sequence`: 1, 2, 3, ... with no gap), the
//! signer's certificate fingerprint and the link configuration hash, all signed.
//!
//! # The file
//!
//! A flat sequence of frames, no file header, the framing of `av-ingest`'s `PartitionLog` and
//! [`crate::buffer::EdgeBuffer`]:
//!
//! ```text
//! +----------------------+----------------------------+---------------------------+
//! | payload_len: u32 LE  | record_hash: [u8; 32]      | payload: payload_len bytes|
//! +----------------------+----------------------------+---------------------------+
//! ```
//!
//! `payload` is the whole encoded, signed record. **One deliberate difference from those
//! logs:** they chain a second, unsigned hash over the encoded payload; here the 32 header
//! bytes are the record's own `record_hash`, so there is a single chain, and it is the signed
//! one. The verifier checks the header copy against the record's field, recomputes the hash
//! from the body, and requires `payload` to equal the re-encoding of the decoded record
//! (so bytes the decoder would ignore, or a non-canonical encoding, are a defect, not
//! silently accepted).
//!
//! # Durability: the ordering contract
//!
//! [`BoardIoLogWriter::append`] writes one frame with a single `write_all` and then `fsync`s
//! (`File::sync_all`: `fcntl(F_FULLFSYNC)` on macOS, `fsync(2)` on Linux; see
//! `av-ingest/src/log.rs` for the cost difference) **before it returns**. The edge service
//! returns a STEP's response to the kernel only after `append` has returned `Ok`, so a
//! response the kernel has seen is always preceded by its durable record. Creating the file
//! also `fsync`s the directory, so the file's existence is durable too. If any step of an
//! append fails the writer is poisoned: every later append fails with
//! [`BoardLogError::Poisoned`], because the run must not continue unlogged.
//!
//! The file is created with `create_new`: an existing file is refused
//! ([`BoardLogError::Exists`]); one log per service run.
//!
//! # Reading and verifying
//!
//! [`verify_bytes`] / [`read_log`] walk the file from byte 0 and check, per record and in
//! this order: framing ([`BoardLogError::FrameTooLarge`]), decoding
//! ([`BoardLogError::Undecodable`], [`BoardLogError::NonCanonicalEncoding`]), the header hash
//! against the record's field ([`BoardLogError::FrameHashMismatch`]), the record hash against
//! its content ([`BoardLogError::RecordHashMismatch`]), the chain link
//! ([`BoardLogError::ChainBreak`]: a removed or swapped record), a constant producer, the
//! sequence ([`BoardLogError::SequenceGap`]), the signer fingerprint when the verifier was
//! built from a certificate ([`BoardLogError::SignerMismatch`]: a wrong certificate), and the
//! signature ([`BoardLogError::Unsigned`], [`BoardLogError::MalformedSignature`],
//! [`BoardLogError::SignatureInvalid`]). The first defect is returned with its 1-based record
//! index; nothing is repaired and the file is never modified.
//!
//! **A torn tail is not an error but a typed recovery outcome** ([`TornTail`] in
//! [`VerifiedLog::recovery`]): only the *final* frame can be torn, and only physically (fewer
//! than 36 header bytes, or fewer payload bytes than declared). Everything before it is
//! verified and returned. This is stricter than `PartitionLog`, which also discards a
//! complete-but-corrupt last record: here a complete last record that does not verify is a
//! defect, because with a signed log that case is as likely tampering as a crash.
//!
//! What the chain cannot show: records removed from the *end* of the log (the chain head is
//! the remaining record's hash and verifies). The chain head, record count and last sequence
//! ([`LogSummary`]) are what a run's manifest should pin.

use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use openssl::ec::EcKey;
use openssl::hash::MessageDigest;
use openssl::pkey::{PKey, Private, Public};
use openssl::x509::X509;
use prost::Message as _;

use crate::hash::{self, GENESIS};
use crate::sign::{self, SigningError};
use crate::verify::{self, VerifyError};

pub use crate::pb::{BoardIoKind, BoardIoRecord};

/// Bytes of the fixed frame header: `payload_len` (4) + `record_hash` (32).
pub const HEADER_LEN: usize = 4 + 32;
/// The largest payload a frame may declare (the lockstep-local frame limit, 16 MiB). A larger
/// declared length is a defect, never a torn tail, so a garbled length cannot hide records.
pub const MAX_PAYLOAD_LEN: usize = 16 * 1024 * 1024;

/// Everything that can go wrong writing, reading or verifying a board I/O log. The
/// verification defects carry the 1-based index of the record they were found at.
#[derive(Debug, thiserror::Error)]
pub enum BoardLogError {
    #[error("I/O error on board I/O log {path}: {source}")]
    Io { path: String, #[source] source: io::Error },
    #[error("board I/O log {path} already exists: refusing to append to it (one log per service run; appending would splice a second run into the first run's chain, and a crashed run's torn tail would end up mid-file). Move or archive it first.")]
    Exists { path: String },
    #[error("the signing certificate does not parse as an X.509 PEM certificate: {0}")]
    InvalidCertificate(String),
    #[error("the signing certificate's public key is not the signing key's public key")]
    KeyCertificateMismatch,
    #[error(transparent)]
    Signing(#[from] SigningError),
    #[error(transparent)]
    VerifyingKey(#[from] VerifyError),
    #[error("an encoded record of {len} bytes exceeds the {MAX_PAYLOAD_LEN}-byte frame limit")]
    PayloadTooLarge { len: usize },
    #[error("the board I/O log writer is unusable after an earlier failure ({reason}); the run must not continue unlogged")]
    Poisoned { reason: String },

    #[error("record {index}: the frame declares a {len}-byte payload, over the {MAX_PAYLOAD_LEN}-byte limit (a damaged length, not a torn tail)")]
    FrameTooLarge { index: u64, len: u64 },
    #[error("record {index}: the payload does not decode as a BoardIoRecord: {detail}")]
    Undecodable { index: u64, detail: String },
    #[error("record {index}: the payload is not the canonical encoding of the record it decodes to (bytes the decoder ignores, or a non-canonical encoding)")]
    NonCanonicalEncoding { index: u64 },
    #[error("record {index}: the frame header's hash differs from the record's own record_hash field")]
    FrameHashMismatch { index: u64 },
    #[error("record {index}: record_hash does not equal SHA-256(prev_hash || body): the record's content was altered")]
    RecordHashMismatch { index: u64 },
    #[error("record {index}: prev_hash does not link to the previous record (a record was removed, inserted or reordered)")]
    ChainBreak { index: u64 },
    #[error("record {index}: producer_id is {got:?} but the log's first record names {expected:?}")]
    ProducerChanged { index: u64, expected: String, got: String },
    #[error("record {index}: sequence is {got} but {expected} was expected")]
    SequenceGap { index: u64, expected: u64, got: u64 },
    #[error("record {index}: signed by certificate {got:?} but the verifying certificate's fingerprint is {expected:?}")]
    SignerMismatch { index: u64, expected: String, got: String },
    #[error("record {index}: unsigned (empty signature)")]
    Unsigned { index: u64 },
    #[error("record {index}: the signature is not a well-formed DER ECDSA signature: {detail}")]
    MalformedSignature { index: u64, detail: String },
    #[error("record {index}: the signature does not verify against the given key")]
    SignatureInvalid { index: u64 },
}

impl BoardLogError {
    fn io(path: &Path, source: io::Error) -> Self {
        BoardLogError::Io { path: path.display().to_string(), source }
    }
}

// ---------------------------------------------------------------------------------------
// Hashing, signing, framing (pure)
// ---------------------------------------------------------------------------------------

/// Step 1 of the definition: the record's encoding with `record_hash` and `signature` cleared.
pub fn canonical_body_bytes(record: &BoardIoRecord) -> Vec<u8> {
    let mut cleared = record.clone();
    cleared.record_hash.clear();
    cleared.signature.clear();
    cleared.encode_to_vec()
}

/// Steps 2 and 3: `SHA-256(record.prev_hash || body)`.
pub fn compute_record_hash(record: &BoardIoRecord) -> [u8; 32] {
    hash::compute_batch_hash(&record.prev_hash, &canonical_body_bytes(record))
}

/// A signing identity: a P-384 key and the fingerprint of its certificate.
pub struct LogSigner {
    key: EcKey<Private>,
    cert_sha256: String,
}

impl LogSigner {
    /// Build from a PEM private key (EC P-384; anything else is refused) and the PEM
    /// certificate that carries the matching public key. The certificate is not chained to a
    /// root here (that is the verifier's trust decision); only the fingerprint is taken and
    /// the key/certificate pairing is checked.
    pub fn from_pem(key_pem: &[u8], cert_pem: &[u8]) -> Result<Self, BoardLogError> {
        let key = sign::load_signing_key(key_pem)?;
        let cert = X509::from_pem(cert_pem).map_err(|e| BoardLogError::InvalidCertificate(e.to_string()))?;
        let cert_key = cert.public_key().map_err(|e| BoardLogError::InvalidCertificate(e.to_string()))?;
        let own = PKey::from_ec_key(key.clone()).map_err(|e| BoardLogError::InvalidCertificate(e.to_string()))?;
        if !cert_key.public_eq(&own) {
            return Err(BoardLogError::KeyCertificateMismatch);
        }
        let digest = cert.digest(MessageDigest::sha256()).map_err(|e| BoardLogError::InvalidCertificate(e.to_string()))?;
        Ok(Self { key, cert_sha256: hash::hex_encode(&digest) })
    }

    /// Lowercase hex SHA-256 of the certificate's DER encoding.
    pub fn cert_sha256(&self) -> &str {
        &self.cert_sha256
    }
}

/// Steps 3 and 4: set `record_hash` from the record's current `prev_hash` and sign it. Every
/// other field must already be set.
pub fn seal(record: &mut BoardIoRecord, signer: &LogSigner) -> Result<(), SigningError> {
    record.record_hash.clear();
    record.signature.clear();
    let digest = compute_record_hash(record);
    record.signature = sign::sign_digest(&signer.key, &digest)?;
    record.record_hash = digest.to_vec();
    Ok(())
}

/// One frame: `[payload_len u32 LE][record_hash][payload]`.
pub fn encode_frame(record: &BoardIoRecord) -> Result<Vec<u8>, BoardLogError> {
    let payload = record.encode_to_vec();
    if payload.len() > MAX_PAYLOAD_LEN {
        return Err(BoardLogError::PayloadTooLarge { len: payload.len() });
    }
    let mut frame = Vec::with_capacity(HEADER_LEN + payload.len());
    frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    frame.extend_from_slice(&record.record_hash);
    frame.extend_from_slice(&payload);
    Ok(frame)
}

// ---------------------------------------------------------------------------------------
// The writer
// ---------------------------------------------------------------------------------------

/// Where frames go. The file implements it; tests substitute a recording or failing sink to
/// check the write-then-sync ordering and the poisoning.
pub trait DurableSink: Send {
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()>;
    /// Make everything written so far durable (`fsync`).
    fn sync(&mut self) -> io::Result<()>;
}

impl DurableSink for File {
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        Write::write_all(self, bytes)
    }
    fn sync(&mut self) -> io::Result<()> {
        self.sync_all()
    }
}


/// What a successful [`BoardIoLogWriter::append`] reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppendReceipt {
    /// The record's position in the log (1-based).
    pub sequence: u64,
    /// The record's hash: the new chain head.
    pub record_hash: [u8; 32],
    /// Bytes of the frame written.
    pub frame_bytes: usize,
}

/// Where a log ends: the record count, the chain head (the last record's hash, or [`GENESIS`]
/// for an empty log) and who signs it. A hash chain cannot show that records were removed from
/// its end, so a run's products pin these (the kernel asks the edge service for them,
/// `BoardEdgeService.BoardIoLogHead`) and a replay compares the log it is given with them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogHead {
    pub records: u64,
    pub chain_head: Vec<u8>,
    pub signer_cert_sha256: String,
    pub link_config_sha256: String,
    pub producer_id: String,
}

/// The append-only, signing, `fsync`ing writer of one board I/O log. Not `Clone`; one per
/// service run.
pub struct BoardIoLogWriter {
    sink: Box<dyn DurableSink>,
    path: PathBuf,
    signer: LogSigner,
    producer_id: String,
    link_config_sha256: String,
    tip: Vec<u8>,
    next_sequence: u64,
    failed: Option<String>,
}

impl BoardIoLogWriter {
    /// Create the log at `path`, which must not exist ([`BoardLogError::Exists`]), and make the
    /// directory entry durable. `producer_id` is the edge node id; `link_config_sha256` the
    /// board link's config hash (`BoardLink::config_hash_hex`), stamped on every record.
    pub fn create(path: &Path, producer_id: &str, link_config_sha256: &str, signer: LogSigner) -> Result<Self, BoardLogError> {
        let file = match OpenOptions::new().write(true).create_new(true).open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Err(BoardLogError::Exists { path: path.display().to_string() }),
            Err(e) => return Err(BoardLogError::io(path, e)),
        };
        let dir = match path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        };
        File::open(dir).and_then(|d| d.sync_all()).map_err(|e| BoardLogError::io(dir, e))?;
        Ok(Self::with_sink(path, Box::new(file), producer_id, link_config_sha256, signer))
    }

    /// A writer over any sink (tests). `path` is only used in messages.
    pub fn with_sink(path: &Path, sink: Box<dyn DurableSink>, producer_id: &str, link_config_sha256: &str, signer: LogSigner) -> Self {
        Self { sink, path: path.to_path_buf(), signer, producer_id: producer_id.to_string(), link_config_sha256: link_config_sha256.to_string(), tip: GENESIS.to_vec(), next_sequence: 1, failed: None }
    }

    /// Complete `draft` (producer, sequence, `prev_hash`, signer, link hash), sign it, write
    /// its frame and `fsync`. Returns only once the record is durable. Any failure poisons
    /// the writer.
    pub fn append(&mut self, mut draft: BoardIoRecord) -> Result<AppendReceipt, BoardLogError> {
        if let Some(reason) = &self.failed {
            return Err(BoardLogError::Poisoned { reason: reason.clone() });
        }
        let result = self.append_inner(&mut draft);
        if let Err(e) = &result {
            self.failed = Some(e.to_string());
        }
        result
    }

    fn append_inner(&mut self, draft: &mut BoardIoRecord) -> Result<AppendReceipt, BoardLogError> {
        draft.producer_id = self.producer_id.clone();
        draft.sequence = self.next_sequence;
        draft.prev_hash = self.tip.clone();
        draft.signer_cert_sha256 = self.signer.cert_sha256.clone();
        draft.link_config_sha256 = self.link_config_sha256.clone();
        seal(draft, &self.signer)?;
        let frame = encode_frame(draft)?;
        self.sink.write_all(&frame).map_err(|e| BoardLogError::io(&self.path, e))?;
        self.sink.sync().map_err(|e| BoardLogError::io(&self.path, e))?;
        let mut record_hash = [0u8; 32];
        record_hash.copy_from_slice(&draft.record_hash);
        self.tip = draft.record_hash.clone();
        self.next_sequence += 1;
        Ok(AppendReceipt { sequence: draft.sequence, record_hash, frame_bytes: frame.len() })
    }

    /// Records durably appended so far.
    pub fn records_written(&self) -> u64 {
        self.next_sequence - 1
    }

    /// The current chain head ([`GENESIS`] before the first record).
    pub fn chain_head(&self) -> &[u8] {
        &self.tip
    }

    /// Whether an earlier failure made the writer unusable.
    pub fn failure(&self) -> Option<&str> {
        self.failed.as_deref()
    }

    /// Where the log ends right now: what a run's products pin ([`LogHead`]).
    pub fn head(&self) -> LogHead {
        LogHead { records: self.records_written(), chain_head: self.tip.clone(), signer_cert_sha256: self.signer.cert_sha256.clone(), link_config_sha256: self.link_config_sha256.clone(), producer_id: self.producer_id.clone() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

// ---------------------------------------------------------------------------------------
// The reader / verifier
// ---------------------------------------------------------------------------------------

/// What a log is verified against: a P-384 public key, and the signing certificate's
/// fingerprint when the key was given as a certificate.
pub struct LogVerifier {
    key: EcKey<Public>,
    cert_sha256: Option<String>,
}

impl LogVerifier {
    /// From a PEM certificate (then every record's `signer_cert_sha256` must equal its
    /// fingerprint) or a bare P-384 public key PEM (then only the signature is checked).
    pub fn from_pem(pem: &[u8]) -> Result<Self, BoardLogError> {
        let key = verify::load_verifying_key(pem)?;
        let cert_sha256 = match X509::from_pem(pem) {
            Ok(cert) => Some(hash::hex_encode(&cert.digest(MessageDigest::sha256()).map_err(|e| BoardLogError::InvalidCertificate(e.to_string()))?)),
            Err(_) => None,
        };
        Ok(Self { key, cert_sha256 })
    }

    pub fn cert_sha256(&self) -> Option<&str> {
        self.cert_sha256.as_deref()
    }
}

/// How the final frame was torn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TornKind {
    /// Fewer than [`HEADER_LEN`] bytes remained.
    IncompleteHeader { have: usize },
    /// The header was complete; `have` of the `declared` payload bytes were present.
    IncompletePayload { declared: usize, have: usize },
}

/// The typed recovery outcome for a log whose last frame is physically incomplete (a crash
/// between `write` and `fsync`). The records before it are intact and returned; nothing was
/// modified. A writer that wanted to continue would truncate the file to `offset` first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TornTail {
    /// Byte offset where the torn frame begins (= bytes of intact records).
    pub offset: u64,
    pub discarded_bytes: u64,
    pub kind: TornKind,
}

/// A fully verified log (every record present passed every check).
#[derive(Debug, Clone)]
pub struct VerifiedLog {
    pub records: Vec<BoardIoRecord>,
    /// The last record's `record_hash`, or [`GENESIS`] for an empty log.
    pub chain_head: Vec<u8>,
    /// Bytes covered by the intact records.
    pub bytes_verified: u64,
    pub recovery: Option<TornTail>,
}

/// The counts and bounds `av-edge-board-log` prints.
#[derive(Debug, Clone, PartialEq)]
pub struct LogSummary {
    pub records: u64,
    pub binds: u64,
    pub steps: u64,
    pub resets: u64,
    pub shutdowns: u64,
    pub power_cycles: u64,
    /// Records whose `error` is non-empty.
    pub failed_exchanges: u64,
    /// `until_tai_ns` of the first and last STEP record (the simulation epochs).
    pub first_epoch_tai_ns: Option<i64>,
    pub last_epoch_tai_ns: Option<i64>,
    pub first_request_written_unix_ns: Option<i64>,
    pub last_response_read_unix_ns: Option<i64>,
    pub producer_id: String,
    pub signer_cert_sha256: String,
    pub link_config_sha256: String,
    pub run_ids: BTreeSet<String>,
    pub chain_head_hex: String,
    pub bytes_verified: u64,
}

impl VerifiedLog {
    pub fn summary(&self) -> LogSummary {
        let mut s = LogSummary {
            records: self.records.len() as u64,
            binds: 0,
            steps: 0,
            resets: 0,
            shutdowns: 0,
            power_cycles: 0,
            failed_exchanges: 0,
            first_epoch_tai_ns: None,
            last_epoch_tai_ns: None,
            first_request_written_unix_ns: None,
            last_response_read_unix_ns: None,
            producer_id: self.records.first().map(|r| r.producer_id.clone()).unwrap_or_default(),
            signer_cert_sha256: self.records.first().map(|r| r.signer_cert_sha256.clone()).unwrap_or_default(),
            link_config_sha256: self.records.first().map(|r| r.link_config_sha256.clone()).unwrap_or_default(),
            run_ids: BTreeSet::new(),
            chain_head_hex: if self.records.is_empty() { "GENESIS".to_string() } else { hash::hex_encode(&self.chain_head) },
            bytes_verified: self.bytes_verified,
        };
        for r in &self.records {
            match BoardIoKind::try_from(r.kind).unwrap_or(BoardIoKind::Unspecified) {
                BoardIoKind::Bind => s.binds += 1,
                BoardIoKind::Step => {
                    s.steps += 1;
                    s.first_epoch_tai_ns.get_or_insert(r.until_tai_ns);
                    s.last_epoch_tai_ns = Some(r.until_tai_ns);
                }
                BoardIoKind::Reset => s.resets += 1,
                BoardIoKind::Shutdown => s.shutdowns += 1,
                BoardIoKind::PowerCycle => s.power_cycles += 1,
                BoardIoKind::Unspecified => {}
            }
            if !r.error.is_empty() {
                s.failed_exchanges += 1;
            }
            if !r.run_id.is_empty() {
                s.run_ids.insert(r.run_id.clone());
            }
            if r.request_written_unix_ns != 0 {
                s.first_request_written_unix_ns.get_or_insert(r.request_written_unix_ns);
            }
            if r.response_read_unix_ns != 0 {
                s.last_response_read_unix_ns = Some(r.response_read_unix_ns);
            }
        }
        s
    }
}

/// Verify the log bytes in `bytes` against `verifier`. See the module doc for the checks and
/// their order. Pure.
pub fn verify_bytes(bytes: &[u8], verifier: &LogVerifier) -> Result<VerifiedLog, BoardLogError> {
    let mut records: Vec<BoardIoRecord> = Vec::new();
    let mut tip: Vec<u8> = GENESIS.to_vec();
    let mut offset = 0usize;
    let mut recovery = None;
    while offset < bytes.len() {
        let index = records.len() as u64 + 1;
        let rest = &bytes[offset..];
        if rest.len() < HEADER_LEN {
            recovery = Some(TornTail { offset: offset as u64, discarded_bytes: rest.len() as u64, kind: TornKind::IncompleteHeader { have: rest.len() } });
            break;
        }
        let declared = u32::from_le_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
        if declared > MAX_PAYLOAD_LEN {
            return Err(BoardLogError::FrameTooLarge { index, len: declared as u64 });
        }
        let have = rest.len() - HEADER_LEN;
        if have < declared {
            recovery = Some(TornTail { offset: offset as u64, discarded_bytes: rest.len() as u64, kind: TornKind::IncompletePayload { declared, have } });
            break;
        }
        let header_hash = &rest[4..HEADER_LEN];
        let payload = &rest[HEADER_LEN..HEADER_LEN + declared];

        let record = BoardIoRecord::decode(payload).map_err(|e| BoardLogError::Undecodable { index, detail: e.to_string() })?;
        if record.encode_to_vec() != payload {
            return Err(BoardLogError::NonCanonicalEncoding { index });
        }
        if header_hash != record.record_hash.as_slice() {
            return Err(BoardLogError::FrameHashMismatch { index });
        }
        let recomputed = compute_record_hash(&record);
        if record.record_hash.as_slice() != recomputed.as_slice() {
            return Err(BoardLogError::RecordHashMismatch { index });
        }
        if record.prev_hash != tip {
            return Err(BoardLogError::ChainBreak { index });
        }
        if let Some(first) = records.first() {
            if first.producer_id != record.producer_id {
                return Err(BoardLogError::ProducerChanged { index, expected: first.producer_id.clone(), got: record.producer_id.clone() });
            }
        }
        if record.sequence != index {
            return Err(BoardLogError::SequenceGap { index, expected: index, got: record.sequence });
        }
        if let Some(expected) = &verifier.cert_sha256 {
            if &record.signer_cert_sha256 != expected {
                return Err(BoardLogError::SignerMismatch { index, expected: expected.clone(), got: record.signer_cert_sha256.clone() });
            }
        }
        verify::verify_digest(&verifier.key, &recomputed, &record.signature).map_err(|e| match e {
            VerifyError::Unsigned => BoardLogError::Unsigned { index },
            VerifyError::MalformedSignature(detail) => BoardLogError::MalformedSignature { index, detail },
            VerifyError::SignatureInvalid => BoardLogError::SignatureInvalid { index },
            other => BoardLogError::VerifyingKey(other),
        })?;

        tip = record.record_hash.clone();
        offset += HEADER_LEN + declared;
        records.push(record);
    }
    let bytes_verified = recovery.as_ref().map(|t: &TornTail| t.offset).unwrap_or(bytes.len() as u64);
    Ok(VerifiedLog { records, chain_head: tip, bytes_verified, recovery })
}

/// Read `path` and [`verify_bytes`] it. Never modifies the file.
pub fn read_log(path: &Path, verifier: &LogVerifier) -> Result<VerifiedLog, BoardLogError> {
    let bytes = std::fs::read(path).map_err(|e| BoardLogError::io(path, e))?;
    verify_bytes(&bytes, verifier)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use openssl::asn1::Asn1Time;
    use openssl::bn::BigNum;
    use openssl::ec::EcGroup;
    use openssl::nid::Nid;
    use openssl::x509::{X509Builder, X509NameBuilder};

    use super::*;
    use crate::pb;

    /// A fresh P-384 key and a self-signed certificate for it: (key PEM, cert PEM, bare public key PEM).
    fn identity(cn: &str) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let key = EcKey::generate(&EcGroup::from_curve_name(Nid::SECP384R1).unwrap()).unwrap();
        let pkey = PKey::from_ec_key(key.clone()).unwrap();
        let mut name = X509NameBuilder::new().unwrap();
        name.append_entry_by_nid(Nid::COMMONNAME, cn).unwrap();
        let name = name.build();
        let mut b = X509Builder::new().unwrap();
        b.set_version(2).unwrap();
        b.set_serial_number(&BigNum::from_u32(1).unwrap().to_asn1_integer().unwrap()).unwrap();
        b.set_subject_name(&name).unwrap();
        b.set_issuer_name(&name).unwrap();
        b.set_pubkey(&pkey).unwrap();
        b.set_not_before(&Asn1Time::days_from_now(0).unwrap()).unwrap();
        b.set_not_after(&Asn1Time::days_from_now(30).unwrap()).unwrap();
        b.sign(&pkey, MessageDigest::sha384()).unwrap();
        (key.private_key_to_pem().unwrap(), b.build().to_pem().unwrap(), pkey.public_key_to_pem().unwrap())
    }

    /// A sink that records the order of writes and syncs and can be told to fail.
    #[derive(Clone, Default)]
    struct Recording {
        events: Arc<Mutex<Vec<String>>>,
        bytes: Arc<Mutex<Vec<u8>>>,
        fail_sync_at: Arc<Mutex<Option<usize>>>,
    }
    impl DurableSink for Recording {
        fn write_all(&mut self, b: &[u8]) -> io::Result<()> {
            self.bytes.lock().unwrap().extend_from_slice(b);
            self.events.lock().unwrap().push("write".into());
            Ok(())
        }
        fn sync(&mut self) -> io::Result<()> {
            let n = self.events.lock().unwrap().iter().filter(|e| *e == "sync").count();
            if *self.fail_sync_at.lock().unwrap() == Some(n) {
                return Err(io::Error::other("injected sync failure"));
            }
            self.events.lock().unwrap().push("sync".into());
            Ok(())
        }
    }

    fn step_draft(seq: u64) -> BoardIoRecord {
        BoardIoRecord {
            run_id: "run-1".into(),
            instance: "obc".into(),
            kind: BoardIoKind::Step as i32,
            lockstep_sequence: seq,
            until_tai_ns: seq as i64 * 100,
            inputs: vec![pb::PortMessage { port: "in".into(), tai_ns: 0, payload: vec![1, 2, 3, seq as u8] }],
            outputs: vec![pb::PortMessage { port: "out".into(), tai_ns: seq as i64 * 100, payload: vec![9, seq as u8] }],
            named_outputs: [("sum".to_string(), seq as f64)].into(),
            request_written_unix_ns: 1_000 + seq as i64,
            response_read_unix_ns: 2_000 + seq as i64,
            ..Default::default()
        }
    }

    /// A log of `n` records (BIND, then n-1 STEPs) as bytes, plus the verifier and its parts.
    fn sample(n: u64) -> (Vec<u8>, LogVerifier, Vec<u8>, Vec<u8>) {
        let (key, cert, pubkey) = identity("edge-test");
        let rec = Recording::default();
        let mut w = BoardIoLogWriter::with_sink(Path::new("mem"), Box::new(rec.clone()), "edge-1", "ab".repeat(32).as_str(), LogSigner::from_pem(&key, &cert).unwrap());
        for i in 1..=n {
            let draft = if i == 1 { BoardIoRecord { kind: BoardIoKind::Bind as i32, run_id: "run-1".into(), ..Default::default() } } else { step_draft(i) };
            w.append(draft).unwrap();
        }
        let bytes = rec.bytes.lock().unwrap().clone();
        (bytes, LogVerifier::from_pem(&cert).unwrap(), cert, pubkey)
    }

    /// Byte ranges `(start, end)` of each frame in `bytes`.
    fn frames(bytes: &[u8]) -> Vec<(usize, usize)> {
        let mut out = vec![];
        let mut o = 0;
        while o < bytes.len() {
            let len = u32::from_le_bytes(bytes[o..o + 4].try_into().unwrap()) as usize;
            out.push((o, o + HEADER_LEN + len));
            o += HEADER_LEN + len;
        }
        out
    }

    #[test]
    fn sign_verify_round_trip_with_chain_sequence_and_summary() {
        let (bytes, verifier, _, pubkey) = sample(5);
        let log = verify_bytes(&bytes, &verifier).unwrap();
        assert_eq!(log.records.len(), 5);
        assert!(log.recovery.is_none());
        assert_eq!(log.records[0].prev_hash, GENESIS);
        for w in log.records.windows(2) {
            assert_eq!(w[1].prev_hash, w[0].record_hash);
        }
        for (i, r) in log.records.iter().enumerate() {
            assert_eq!(r.sequence, i as u64 + 1);
            assert_eq!(r.producer_id, "edge-1");
            assert_eq!(r.link_config_sha256, "ab".repeat(32));
        }
        let s = log.summary();
        assert_eq!((s.records, s.binds, s.steps), (5, 1, 4));
        assert_eq!((s.first_epoch_tai_ns, s.last_epoch_tai_ns), (Some(200), Some(500)));
        assert_eq!(s.chain_head_hex, hash::hex_encode(&log.records[4].record_hash));
        assert_eq!(s.bytes_verified, bytes.len() as u64);
        // The same log verifies against the bare public key (no fingerprint check).
        let by_key = LogVerifier::from_pem(&pubkey).unwrap();
        assert!(by_key.cert_sha256().is_none());
        assert_eq!(verify_bytes(&bytes, &by_key).unwrap().records.len(), 5);
        // An empty file is a valid empty log.
        let empty = verify_bytes(&[], &verifier).unwrap();
        assert!(empty.records.is_empty() && empty.chain_head == GENESIS && empty.recovery.is_none());
    }

    #[test]
    fn record_hash_is_deterministic_and_excludes_hash_and_signature() {
        let mut r = step_draft(3);
        let h = compute_record_hash(&r);
        r.record_hash = vec![7; 32];
        r.signature = vec![8; 70];
        assert_eq!(compute_record_hash(&r), h);
        r.prev_hash = GENESIS.to_vec();
        assert_ne!(compute_record_hash(&r), h, "prev_hash is signed content");
    }

    #[test]
    fn a_flipped_payload_byte_is_detected() {
        let (bytes, verifier, _, _) = sample(4);
        let (s, e) = frames(&bytes)[1];
        // Flip a byte inside the second record's port payload (the last bytes before the signature
        // fields are not stable; search for the distinctive payload instead).
        let at = (s + HEADER_LEN..e).find(|&i| bytes[i..].starts_with(&[1, 2, 3, 2])).expect("the inputs payload is in the record");
        let mut bad = bytes.clone();
        bad[at + 3] ^= 0x01;
        assert!(matches!(verify_bytes(&bad, &verifier), Err(BoardLogError::RecordHashMismatch { index: 2 })), "{:?}", verify_bytes(&bad, &verifier).err());
        // Flipping the stored record_hash inside the payload disagrees with the frame header.
        let hash_pos = (s + HEADER_LEN..e).find(|&i| bytes[i..].starts_with(&bytes[s + 4..s + HEADER_LEN])).unwrap();
        let mut bad = bytes.clone();
        bad[hash_pos] ^= 0x01;
        assert!(matches!(verify_bytes(&bad, &verifier), Err(BoardLogError::FrameHashMismatch { index: 2 })));
        // Flipping the header's copy instead is the same defect.
        let mut bad = bytes.clone();
        bad[s + 4] ^= 0x01;
        assert!(matches!(verify_bytes(&bad, &verifier), Err(BoardLogError::FrameHashMismatch { index: 2 })));
    }

    #[test]
    fn a_flipped_signature_byte_is_detected() {
        let (bytes, verifier, _, _) = sample(3);
        let log = verify_bytes(&bytes, &verifier).unwrap();
        let sig = &log.records[1].signature;
        let (s, e) = frames(&bytes)[1];
        let at = (s + HEADER_LEN..e).find(|&i| bytes[i..].starts_with(sig)).unwrap();
        let mut bad = bytes.clone();
        bad[at + sig.len() - 1] ^= 0x01; // the last byte of s
        assert!(matches!(verify_bytes(&bad, &verifier), Err(BoardLogError::SignatureInvalid { index: 2 })), "{:?}", verify_bytes(&bad, &verifier).err());
        // A damaged DER header is malformed, not merely invalid.
        let mut bad = bytes.clone();
        bad[at] ^= 0xff;
        assert!(matches!(verify_bytes(&bad, &verifier), Err(BoardLogError::MalformedSignature { index: 2, .. })), "{:?}", verify_bytes(&bad, &verifier).err());
    }

    #[test]
    fn a_removed_record_and_swapped_records_are_chain_breaks() {
        let (bytes, verifier, _, _) = sample(4);
        let f = frames(&bytes);
        let slice = |i: usize| bytes[f[i].0..f[i].1].to_vec();
        // Remove record 2 (a middle record): record 3 no longer links.
        let removed: Vec<u8> = [slice(0), slice(2), slice(3)].concat();
        assert!(matches!(verify_bytes(&removed, &verifier), Err(BoardLogError::ChainBreak { index: 2 })));
        // Remove the first record: record 2 does not link from GENESIS.
        let no_first: Vec<u8> = [slice(1), slice(2), slice(3)].concat();
        assert!(matches!(verify_bytes(&no_first, &verifier), Err(BoardLogError::ChainBreak { index: 1 })));
        // Swap records 2 and 3.
        let swapped: Vec<u8> = [slice(0), slice(2), slice(1), slice(3)].concat();
        assert!(matches!(verify_bytes(&swapped, &verifier), Err(BoardLogError::ChainBreak { index: 2 })));
        // Removing the last record is NOT detectable by the chain (documented): the prefix verifies.
        let no_last: Vec<u8> = [slice(0), slice(1), slice(2)].concat();
        assert_eq!(verify_bytes(&no_last, &verifier).unwrap().records.len(), 3);
    }

    #[test]
    fn a_wrong_certificate_or_key_is_refused_with_the_right_error() {
        let (bytes, _, _, _) = sample(3);
        let (_, other_cert, other_pub) = identity("someone-else");
        let by_cert = LogVerifier::from_pem(&other_cert).unwrap();
        assert!(matches!(verify_bytes(&bytes, &by_cert), Err(BoardLogError::SignerMismatch { index: 1, .. })));
        let by_key = LogVerifier::from_pem(&other_pub).unwrap();
        assert!(matches!(verify_bytes(&bytes, &by_key), Err(BoardLogError::SignatureInvalid { index: 1 })));
    }

    #[test]
    fn a_re_signed_forgery_with_another_key_is_refused_by_the_certificate_check() {
        // An attacker rewrites record 2 and re-signs the chain from there with their own key;
        // the fingerprint they stamp (their cert) is not the verifier's.
        let (bytes, verifier, _, _) = sample(3);
        let (k2, c2, _) = identity("attacker");
        let signer2 = LogSigner::from_pem(&k2, &c2).unwrap();
        let log = verify_bytes(&bytes, &verifier).unwrap();
        let mut forged = log.records[1].clone();
        forged.outputs[0].payload = vec![0xde, 0xad];
        forged.signer_cert_sha256 = signer2.cert_sha256().to_string();
        seal(&mut forged, &signer2).unwrap();
        let f = frames(&bytes);
        let tampered: Vec<u8> = [bytes[..f[1].0].to_vec(), encode_frame(&forged).unwrap()].concat();
        assert!(matches!(verify_bytes(&tampered, &verifier), Err(BoardLogError::SignerMismatch { index: 2, .. })));
        // Claiming the genuine fingerprint instead fails on the signature.
        forged.signer_cert_sha256 = verifier.cert_sha256().unwrap().to_string();
        seal(&mut forged, &signer2).unwrap();
        let tampered: Vec<u8> = [bytes[..f[1].0].to_vec(), encode_frame(&forged).unwrap()].concat();
        assert!(matches!(verify_bytes(&tampered, &verifier), Err(BoardLogError::SignatureInvalid { index: 2 })));
    }

    #[test]
    fn a_torn_tail_is_a_typed_recovery_with_earlier_records_intact() {
        let (bytes, verifier, _, _) = sample(4);
        let f = frames(&bytes);
        // Cut mid-payload of record 4.
        let cut = f[3].0 + HEADER_LEN + 5;
        let log = verify_bytes(&bytes[..cut], &verifier).unwrap();
        assert_eq!(log.records.len(), 3);
        let torn = log.recovery.clone().expect("a torn tail is reported");
        assert_eq!(torn.offset, f[3].0 as u64);
        assert_eq!(torn.discarded_bytes, (cut - f[3].0) as u64);
        assert!(matches!(torn.kind, TornKind::IncompletePayload { have: 5, .. }));
        assert_eq!(log.bytes_verified, f[3].0 as u64);
        assert_eq!(log.chain_head, log.records[2].record_hash);
        // Cut mid-header.
        let log = verify_bytes(&bytes[..f[3].0 + 10], &verifier).unwrap();
        assert_eq!(log.records.len(), 3);
        assert!(matches!(log.recovery.unwrap().kind, TornKind::IncompleteHeader { have: 10 }));
        // A cut exactly at a record boundary is a clean log.
        let log = verify_bytes(&bytes[..f[2].1], &verifier).unwrap();
        assert!(log.recovery.is_none() && log.records.len() == 3);
        // A complete but corrupt last record is a defect, not a recovery.
        let mut bad = bytes.clone();
        let last = bytes.len() - 3;
        bad[last] ^= 0xff;
        assert!(verify_bytes(&bad, &verifier).is_err());
    }

    #[test]
    fn a_damaged_length_is_not_mistaken_for_a_torn_tail() {
        let (mut bytes, verifier, _, _) = sample(2);
        let s = frames(&bytes)[1].0;
        bytes[s..s + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(verify_bytes(&bytes, &verifier), Err(BoardLogError::FrameTooLarge { index: 2, .. })));
    }

    #[test]
    fn bytes_the_decoder_ignores_are_a_defect() {
        let (bytes, verifier, _, _) = sample(2);
        let f = frames(&bytes);
        // Append an unknown field (number 99, varint 1) to record 2's payload and fix the length.
        let mut payload = bytes[f[1].0 + HEADER_LEN..f[1].1].to_vec();
        payload.extend_from_slice(&[0x98, 0x06, 0x01]);
        let mut frame = (payload.len() as u32).to_le_bytes().to_vec();
        frame.extend_from_slice(&bytes[f[1].0 + 4..f[1].0 + HEADER_LEN]);
        frame.extend_from_slice(&payload);
        let tampered: Vec<u8> = [bytes[..f[1].0].to_vec(), frame].concat();
        assert!(matches!(verify_bytes(&tampered, &verifier), Err(BoardLogError::NonCanonicalEncoding { index: 2 })));
    }

    #[test]
    fn a_sequence_gap_with_a_valid_chain_is_reported() {
        let (key, cert, _) = identity("edge-test");
        let signer = LogSigner::from_pem(&key, &cert).unwrap();
        let verifier = LogVerifier::from_pem(&cert).unwrap();
        let mut r = step_draft(1);
        r.producer_id = "edge-1".into();
        r.sequence = 2; // should be 1
        r.prev_hash = GENESIS.to_vec();
        r.signer_cert_sha256 = signer.cert_sha256().to_string();
        seal(&mut r, &signer).unwrap();
        assert!(matches!(verify_bytes(&encode_frame(&r).unwrap(), &verifier), Err(BoardLogError::SequenceGap { index: 1, expected: 1, got: 2 })));
    }

    #[test]
    fn the_writer_syncs_after_every_write_before_returning() {
        let (key, cert, _) = identity("edge-test");
        let rec = Recording::default();
        let mut w = BoardIoLogWriter::with_sink(Path::new("mem"), Box::new(rec.clone()), "edge-1", "00", LogSigner::from_pem(&key, &cert).unwrap());
        for i in 1..=3 {
            w.append(step_draft(i)).unwrap();
            let ev = rec.events.lock().unwrap().clone();
            assert_eq!(ev.len() as u64, 2 * i, "write+sync per record, both done when append returns: {ev:?}");
            assert_eq!(ev[ev.len() - 2..], ["write", "sync"]);
        }
        assert_eq!(w.records_written(), 3);
    }

    #[test]
    fn a_failed_append_poisons_the_writer_and_leaves_a_verifiable_prefix() {
        let (key, cert, _) = identity("edge-test");
        let verifier = LogVerifier::from_pem(&cert).unwrap();
        let rec = Recording::default();
        *rec.fail_sync_at.lock().unwrap() = Some(2); // the third sync fails
        let mut w = BoardIoLogWriter::with_sink(Path::new("mem"), Box::new(rec.clone()), "edge-1", "00", LogSigner::from_pem(&key, &cert).unwrap());
        w.append(step_draft(1)).unwrap();
        w.append(step_draft(2)).unwrap();
        assert!(matches!(w.append(step_draft(3)), Err(BoardLogError::Io { .. })));
        assert!(w.failure().unwrap().contains("injected sync failure"));
        assert!(matches!(w.append(step_draft(4)), Err(BoardLogError::Poisoned { .. })));
        assert_eq!(w.records_written(), 2, "the failed record does not count");
        // What reached the sink verifies (the unsynced third frame was written whole here).
        let log = verify_bytes(&rec.bytes.lock().unwrap(), &verifier).unwrap();
        assert!(log.records.len() >= 2);
    }

    #[test]
    fn the_writer_reports_where_the_log_ends() {
        let (key, cert, _) = identity("edge-test");
        let verifier = LogVerifier::from_pem(&cert).unwrap();
        let rec = Recording::default();
        let mut w = BoardIoLogWriter::with_sink(Path::new("mem"), Box::new(rec.clone()), "edge-1", "cd".repeat(32).as_str(), LogSigner::from_pem(&key, &cert).unwrap());
        let empty = w.head();
        assert_eq!((empty.records, empty.chain_head), (0, GENESIS.to_vec()), "an empty log ends at GENESIS");
        for i in 1..=3 {
            w.append(step_draft(i)).unwrap();
        }
        let head = w.head();
        let log = verify_bytes(&rec.bytes.lock().unwrap(), &verifier).unwrap();
        assert_eq!(head.records, 3);
        assert_eq!(head.chain_head, log.records[2].record_hash, "the head is the last record's own hash");
        assert_eq!(head.chain_head, log.chain_head);
        assert_eq!((head.signer_cert_sha256.as_str(), head.producer_id.as_str()), (verifier.cert_sha256().unwrap(), "edge-1"));
        assert_eq!(head.link_config_sha256, "cd".repeat(32));
    }

    #[test]
    fn create_refuses_an_existing_file_and_writes_a_verifiable_file() {
        let dir = std::env::temp_dir().join(format!("av-edge-board-log-unit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("io.log");
        let _ = std::fs::remove_file(&path);
        let (key, cert, _) = identity("edge-test");
        let mut w = BoardIoLogWriter::create(&path, "edge-1", "00", LogSigner::from_pem(&key, &cert).unwrap()).unwrap();
        w.append(step_draft(1)).unwrap();
        w.append(step_draft(2)).unwrap();
        let err = BoardIoLogWriter::create(&path, "edge-1", "00", LogSigner::from_pem(&key, &cert).unwrap()).err().unwrap();
        assert!(matches!(err, BoardLogError::Exists { .. }));
        assert!(err.to_string().contains("one log per service run"));
        let log = read_log(&path, &LogVerifier::from_pem(&cert).unwrap()).unwrap();
        assert_eq!(log.records.len(), 2);
        assert_eq!(log.chain_head, w.chain_head());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_signer_refuses_a_key_certificate_mismatch_a_wrong_curve_and_a_bad_certificate() {
        let (k1, _, _) = identity("a");
        let (_, c2, _) = identity("b");
        assert!(matches!(LogSigner::from_pem(&k1, &c2), Err(BoardLogError::KeyCertificateMismatch)));
        let p256 = EcKey::generate(&EcGroup::from_curve_name(Nid::X9_62_PRIME256V1).unwrap()).unwrap().private_key_to_pem().unwrap();
        assert!(matches!(LogSigner::from_pem(&p256, &c2), Err(BoardLogError::Signing(SigningError::WrongCurve { .. }))));
        assert!(matches!(LogSigner::from_pem(&k1, b"nope"), Err(BoardLogError::InvalidCertificate(_))));
    }
}
