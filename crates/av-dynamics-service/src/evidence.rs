//! Evidence log (spoore ADR-005 pattern, ADR-002 "Evidence, not re-execution", ADR-004's
//! evidence rules): one JSON object per successful RPC response, appended to a local
//! JSONL file, so a replay can read the recorded answer instead of re-running GMAT.
//!
//! **Mirrors `services/gmat-service/gmat_service/evidence.py`'s record shape exactly**
//! (same nine keys, same meaning), so a replay tool reads either language's log the same
//! way -- but hashes with the **`openssl` crate** (`openssl::sha::sha256`, backed by
//! `openssl-sys` -> the system/Homebrew `libssl`/`libcrypto`), not `sha2` and not `ring`.
//! This is deliberate, not an oversight: M6.2's brief is specifically to prove the
//! OpenSSL-backed hashing path in a second place beyond `crates/av-grpc`'s TLS stack, even
//! though SHA-256 itself is not a security-sensitive operation here (`av-dynamics`'s own
//! `settings_hash` already uses pure-Rust `sha2` for the *same kind* of non-cryptographic
//! fingerprint, and that is equally correct -- ADR-004's rule is about what protects the
//! platform's real trust boundary, not "ban sha2 everywhere").
//!
//! # Hash chain (ADR-004 M7.3, `proto/altavista/v1/envelope.proto`'s `SignedBatch` convention)
//!
//! Every record additionally carries `seq`, `prev_hash` and `hash`, chained exactly the
//! way `SignedBatch.prev_hash`/`SignedBatch.hash` are documented in `envelope.proto`:
//!
//! - `prev_hash` is the previous record's own `hash` (hex), or the literal string
//!   `"GENESIS"` for the first record in the log (the suite's ledger convention, verbatim).
//! - `hash` is `SHA-256(prev_hash_bytes || body_bytes)` hex-encoded, where `prev_hash_bytes`
//!   is the ASCII bytes of the literal `"GENESIS"` for the first record or else the 32 raw
//!   bytes the previous record's hex `hash` decodes to, and `body_bytes` is the canonical
//!   (sorted-key) JSON encoding of this record's own `epoch`/`method`/`request_hash`/
//!   `response_hash`/`run_id`/`seq`/`settings_hash` fields -- i.e. everything except
//!   `prev_hash`/`hash` themselves, playing the role `SignedBatch.batch` plays in the proto
//!   convention.
//!
//! [`EvidenceLog::verify`] walks the file straight from disk (independent of any in-memory
//! state) and recomputes every record's `hash`, so a record whose *content* was edited
//! without recomputing its `hash` -- or whose `prev_hash`/`seq` was tampered with -- is
//! detected and reported by `seq`, mirroring `altavista.v1.ChainVerification`'s
//! `ok`/`checked`/`broken_at_sequence`/`detail` field shape.
//!
//! **Not cross-language byte-identical (a deliberate, documented gap, not a bug):**
//! `serde_json::to_vec` emits compact JSON (`{"epoch":123,...}`, no separators), while
//! Python's `json.dumps(body, sort_keys=True)` on the `gmat_service.evidence` side emits
//! `", "`/`": "` separators by default (`{"epoch": 123, ...}`). `body_bytes` therefore
//! differs byte-for-byte between the two languages for logically-identical field values, so
//! **`hash`/`chain_hash` values are never comparable across a Rust log and a Python log**,
//! even though both implement the identical GENESIS/prev_hash/seq *algorithm*. Each
//! language's own log is independently, fully self-verifying (`verify()` only ever compares
//! against records written by the same process's own `record()`), which is all a replay or
//! an assessor's chain-of-custody check over *one* evidence file needs -- this is the exact
//! same "each is a pure, stable function of its own message, not claimed byte-identical
//! across languages" caveat this module doc already states for `hash_message`'s protobuf
//! encoding, just also true of this JSON body encoding.
//!
//! # Format
//!
//! One JSON object per line, UTF-8, newline-terminated, **keys sorted** (matching
//! Python's `json.dumps(entry, sort_keys=True)` byte for byte in intent, achieved here by
//! declaring [`EvidenceEntry`]'s fields in alphabetical order -- `serde_json`'s struct
//! serializer writes fields in declaration order, so an alphabetically-declared struct
//! produces alphabetically-sorted JSON keys without needing to round-trip through
//! `serde_json::Value`'s `BTreeMap`-backed `Map` representation):
//!
//! ```json
//! {
//!   "epoch":          <int64>   TAI nanoseconds this record was written (wall clock,
//!                                converted via av_cdm::time::Tai::from_utc_nanos) -- the
//!                                record's own creation time, not the request's simulated
//!                                epoch (already inside request_hash).
//!   "hash":           <str>     SHA-256 hex of (prev_hash || body) -- see "Hash chain" above.
//!   "method":         <str>     "Describe" | "Derivatives" | "Step" | "Propagate" | "Solve"
//!   "prev_hash":      <str>     previous record's "hash", or "GENESIS" for the first record.
//!   "request_hash":   <str>     SHA-256 hex of the request protobuf's `encode_to_vec()`
//!   "response_hash":  <str>     SHA-256 hex of the response protobuf's `encode_to_vec()`
//!   "run_id":         <str>     one id per server process (see crate::worker)
//!   "seq":            <uint64>  1-based, monotonic per evidence file (per `SignedBatch.sequence`)
//!   "settings_hash":  <str>     config::settings_hash() at record time
//! }
//! ```
//!
//! `prost::Message::encode_to_vec()` is prost's normal, deterministic encoding: every
//! `repeated` field already has a fixed emission order (declaration order, unaffected by
//! how the caller built the message) and every `map<..>` field in `altavista.v1` is
//! generated as a `BTreeMap` (`build.rs`'s `.btree_map(["."])`, ADR-004's determinism
//! rule), so two calls with an equal message always encode identically -- the same
//! functional guarantee `SerializeToString(deterministic=True)` gives on the Python side,
//! even though the two are different implementations (prost vs. the C++ protobuf runtime)
//! and are not claimed to produce byte-identical output *across* languages -- only that
//! each is a pure, stable function of its own message, which is all a replay needs.
//!
//! Not itself a multi-writer-process-safe log (plain file append, `Mutex`-guarded against
//! two RPCs on this process racing each other) -- one server process owns one evidence
//! file, exactly like `gmat_service.evidence.EvidenceLog`.

use std::fs::{File, OpenOptions};
use std::io::{BufRead as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use openssl::sha::sha256;
use prost::Message;
use serde::{Deserialize, Serialize};

use av_cdm::time::Tai;

/// The suite's ledger convention (`envelope.proto`'s `SignedBatch.prev_hash` doc comment,
/// verbatim): the first record's `prev_hash` is this literal string, not a hash of
/// anything -- its ASCII bytes are what gets hashed with the first record's own body.
pub const GENESIS: &str = "GENESIS";

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

/// SHA-256 hex of `msg`'s `prost` encoding, via the `openssl` crate. See the module doc
/// for exactly what "deterministic" means here and why `openssl`, not `sha2`.
pub fn hash_message<M: Message>(msg: &M) -> String {
    hex_encode(&sha256(&msg.encode_to_vec()))
}

fn now_tai_ns() -> i64 {
    let utc_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before the Unix epoch")
        .as_nanos() as i64;
    Tai::from_utc_nanos(utc_ns).as_nanos()
}

/// Field order is deliberate -- see the module doc's "Format" section.
#[derive(Debug, Serialize)]
struct EvidenceEntry<'a> {
    epoch: i64,
    hash: &'a str,
    method: &'a str,
    prev_hash: &'a str,
    request_hash: &'a str,
    response_hash: &'a str,
    run_id: &'a str,
    seq: u64,
    settings_hash: &'a str,
}

/// The subset of an [`EvidenceEntry`] that is hashed as `envelope.proto`'s `SignedBatch.batch`
/// role: everything except `prev_hash`/`hash` themselves, which are concatenated/produced
/// around this, not folded into it. Field order (alphabetical) is exactly what
/// [`EvidenceEntry`] would serialize for the same fields, so `hash_of` here and the
/// `EvidenceEntry` on disk agree byte-for-byte on what "the body" means.
#[derive(Debug, Serialize, Deserialize)]
struct EvidenceBody<'a> {
    epoch: i64,
    method: &'a str,
    request_hash: &'a str,
    response_hash: &'a str,
    run_id: &'a str,
    seq: u64,
    settings_hash: &'a str,
}

/// `SHA-256(prev_hash_bytes || body_bytes)` hex-encoded -- see the module doc's "Hash
/// chain" section. `prev_hash` is `"GENESIS"` (hashed as its literal ASCII bytes) for the
/// first record, or else a 64-char hex `hash` from the previous record (hashed as the 32
/// raw bytes it decodes to). Returns `None` only if `prev_hash` is neither `"GENESIS"` nor
/// valid hex -- i.e. the log itself is already malformed at the caller's read site.
fn chain_hash(prev_hash: &str, body: &EvidenceBody<'_>) -> Option<String> {
    let prev_bytes = if prev_hash == GENESIS { GENESIS.as_bytes().to_vec() } else { hex_decode(prev_hash)? };
    let body_bytes = serde_json::to_vec(body).expect("EvidenceBody always serializes");
    let mut buf = prev_bytes;
    buf.extend_from_slice(&body_bytes);
    Some(hex_encode(&sha256(&buf)))
}

/// Result of [`EvidenceLog::verify`], field-for-field matching
/// `altavista.v1.ChainVerification` (`proto/altavista/v1/envelope.proto`) minus
/// `producer_id`, which that message has and a single-writer evidence file does not need
/// (one file is already scoped to one run).
#[derive(Debug, Clone, Serialize)]
pub struct ChainVerification {
    pub ok: bool,
    pub checked: u64,
    /// `Some(seq)` -- the 1-based sequence number the break was detected at -- iff `!ok`.
    pub broken_at_seq: Option<u64>,
    pub detail: String,
}

/// In-memory chain state so `record()` need not re-read the file on every call; recovered
/// from disk in [`EvidenceLog::open`] so a server restart continues the same chain rather
/// than silently starting a second one (a second `"GENESIS"` in one file would itself look
/// like tampering to [`EvidenceLog::verify`], which is exactly the failure mode this
/// recovery step avoids).
#[derive(Debug)]
struct ChainState {
    next_seq: u64,
    last_hash: String,
}

#[derive(Debug)]
pub struct EvidenceLog {
    path: PathBuf,
    file: Mutex<File>,
    chain: Mutex<ChainState>,
}

impl EvidenceLog {
    pub fn open(path: impl Into<PathBuf>) -> std::io::Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let chain = Self::recover_chain_state(&path)?;
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self { path, file: Mutex::new(file), chain: Mutex::new(chain) })
    }

    /// Reads the last non-empty line of an existing evidence file (if any) to recover
    /// `next_seq`/`last_hash` -- a fresh/missing file starts a new chain at `seq = 1`,
    /// `prev_hash = "GENESIS"`. Deliberately tolerant of a malformed last line (falls back
    /// to a fresh chain rather than refusing to start the server): `verify()` is the tool
    /// that reports a malformed log as a finding, not `open()`.
    fn recover_chain_state(path: &Path) -> std::io::Result<ChainState> {
        let fresh = ChainState { next_seq: 1, last_hash: GENESIS.to_string() };
        let Ok(file) = File::open(path) else { return Ok(fresh) };
        let mut last_line: Option<String> = None;
        for line in std::io::BufReader::new(file).lines() {
            let line = line?;
            if !line.trim().is_empty() {
                last_line = Some(line);
            }
        }
        let Some(line) = last_line else { return Ok(fresh) };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else { return Ok(fresh) };
        let (Some(seq), Some(hash)) = (value.get("seq").and_then(|v| v.as_u64()), value.get("hash").and_then(|v| v.as_str()))
        else {
            return Ok(fresh);
        };
        Ok(ChainState { next_seq: seq + 1, last_hash: hash.to_string() })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The current chain head (`"hash"` of the most recently written record), or
    /// `"GENESIS"` if the log is still empty -- what `/admin/api/evidence` reports.
    pub fn chain_head(&self) -> String {
        self.chain.lock().unwrap_or_else(|p| p.into_inner()).last_hash.clone()
    }

    /// Number of records written so far (`next_seq - 1`).
    pub fn entry_count(&self) -> u64 {
        self.chain.lock().unwrap_or_else(|p| p.into_inner()).next_seq - 1
    }

    /// Appends one record for a successful RPC. `settings_hash`/`run_id` are read fresh
    /// from the caller (rather than cached at construction) so a future multi-model server
    /// could report the settings hash that actually produced `response`, matching
    /// `gmat_service.service.DynamicsServiceServicer._record`'s own per-call reads.
    pub fn record<Req: Message, Resp: Message>(
        &self,
        method: &str,
        request: &Req,
        response: &Resp,
        settings_hash: &str,
        run_id: &str,
    ) {
        let epoch = now_tai_ns();
        let request_hash = hash_message(request);
        let response_hash = hash_message(response);

        // Chain state and the file are locked together for this whole append so two RPCs on
        // this process never interleave a seq/hash pair with the wrong body (see the module
        // doc: this is a single-writer-process log, not a multi-writer-safe one).
        let mut chain = self.chain.lock().unwrap_or_else(|p| p.into_inner());
        let seq = chain.next_seq;
        let prev_hash = chain.last_hash.clone();
        let body = EvidenceBody {
            epoch,
            method,
            request_hash: &request_hash,
            response_hash: &response_hash,
            run_id,
            seq,
            settings_hash,
        };
        let hash = chain_hash(&prev_hash, &body).expect("prev_hash recovered from this log's own chain state is always well-formed");

        let entry = EvidenceEntry {
            epoch,
            hash: &hash,
            method,
            prev_hash: &prev_hash,
            request_hash: &request_hash,
            response_hash: &response_hash,
            run_id,
            seq,
            settings_hash,
        };
        let line = serde_json::to_string(&entry).expect("EvidenceEntry always serializes");
        let mut f = self.file.lock().unwrap_or_else(|p| p.into_inner());
        // A write failure here (disk full, permissions) is a real operational problem, but
        // this server's whole point is to answer the RPC that already succeeded -- logging
        // is best-effort exactly like the RPC's own success does not depend on it; matches
        // `gmat_service.evidence.EvidenceLog.record`, which likewise lets a write error
        // propagate as an unhandled exception rather than silently dropping the record, so
        // failures are loud (not swallowed) either way. `expect` here, not a swallowed
        // `Result`: a corrupt/undiagnosed evidence log is worse than a crashed process.
        writeln!(f, "{line}").expect("evidence log write failed");
        drop(f);
        chain.next_seq = seq + 1;
        chain.last_hash = hash;
    }

    /// Walks the evidence file straight from disk (independent of the in-memory
    /// [`ChainState`] `record()` maintains) and recomputes every record's chain link, so
    /// this detects tampering that happened to the file itself (e.g. by hand, or by a
    /// process other than this one), not just a bug in `record()`. Stops and reports at the
    /// first broken record -- see [`ChainVerification`]'s doc for the field shape.
    pub fn verify(&self) -> std::io::Result<ChainVerification> {
        let Ok(file) = File::open(&self.path) else {
            return Ok(ChainVerification { ok: true, checked: 0, broken_at_seq: None, detail: "no evidence file yet".to_string() });
        };
        let mut expected_prev = GENESIS.to_string();
        let mut expected_seq: u64 = 1;
        let mut checked: u64 = 0;

        for (line_no, line) in std::io::BufReader::new(file).lines().enumerate() {
            let line_no = line_no + 1;
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let value: serde_json::Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(e) => {
                    return Ok(ChainVerification {
                        ok: false,
                        checked,
                        broken_at_seq: None,
                        detail: format!("line {line_no}: invalid JSON ({e})"),
                    })
                }
            };
            let entry: Result<FullEntry, _> = serde_json::from_value(value);
            let Ok(entry) = entry else {
                return Ok(ChainVerification {
                    ok: false,
                    checked,
                    broken_at_seq: None,
                    detail: format!("line {line_no}: record is missing one of the required evidence fields"),
                });
            };
            if entry.seq != expected_seq {
                return Ok(ChainVerification {
                    ok: false,
                    checked,
                    broken_at_seq: Some(entry.seq),
                    detail: format!("line {line_no}: expected seq {expected_seq}, found seq {}", entry.seq),
                });
            }
            if entry.prev_hash != expected_prev {
                return Ok(ChainVerification {
                    ok: false,
                    checked,
                    broken_at_seq: Some(entry.seq),
                    detail: format!("seq {}: prev_hash {:?} does not match the previous record's hash {:?}", entry.seq, entry.prev_hash, expected_prev),
                });
            }
            let body = EvidenceBody {
                epoch: entry.epoch,
                method: &entry.method,
                request_hash: &entry.request_hash,
                response_hash: &entry.response_hash,
                run_id: &entry.run_id,
                seq: entry.seq,
                settings_hash: &entry.settings_hash,
            };
            let Some(recomputed) = chain_hash(&expected_prev, &body) else {
                return Ok(ChainVerification {
                    ok: false,
                    checked,
                    broken_at_seq: Some(entry.seq),
                    detail: format!("seq {}: prev_hash is not valid hex", entry.seq),
                });
            };
            if recomputed != entry.hash {
                return Ok(ChainVerification {
                    ok: false,
                    checked,
                    broken_at_seq: Some(entry.seq),
                    detail: format!(
                        "seq {}: recorded hash does not match its recomputed content hash -- the record was tampered with after being written",
                        entry.seq
                    ),
                });
            }
            expected_prev = entry.hash;
            expected_seq = entry.seq + 1;
            checked += 1;
        }
        Ok(ChainVerification { ok: true, checked, broken_at_seq: None, detail: "chain intact".to_string() })
    }
}

/// Owned deserialization target for one JSONL line in [`EvidenceLog::verify`] -- an owned
/// twin of [`EvidenceEntry`] (which borrows, for zero-copy writing) since `verify` reads
/// each record back from a freshly-parsed `serde_json::Value`.
#[derive(Debug, Deserialize)]
struct FullEntry {
    epoch: i64,
    hash: String,
    method: String,
    prev_hash: String,
    request_hash: String,
    response_hash: String,
    run_id: String,
    seq: u64,
    settings_hash: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use av_cdm::pb::DescribeRequest;

    #[test]
    fn hash_message_is_stable_and_hex() {
        let req = DescribeRequest { model_id: "x".to_string() };
        let a = hash_message(&req);
        let b = hash_message(&req);
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn hash_message_differs_for_different_messages() {
        let a = hash_message(&DescribeRequest { model_id: "a".to_string() });
        let b = hash_message(&DescribeRequest { model_id: "b".to_string() });
        assert_ne!(a, b);
    }

    #[test]
    fn record_appends_one_sorted_key_json_line_per_call() {
        let dir = std::env::temp_dir().join(format!("av-dynsvc-evidence-test-{}", std::process::id()));
        let path = dir.join("evidence.jsonl");
        let _ = std::fs::remove_dir_all(&dir);
        let log = EvidenceLog::open(&path).unwrap();
        let req = DescribeRequest { model_id: "m".to_string() };
        let resp = DescribeRequest { model_id: "m".to_string() };
        log.record("Describe", &req, &resp, "settingshash", "run123");
        log.record("Describe", &req, &resp, "settingshash", "run123");

        let contents = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 2, "one JSONL line per record() call");

        let value: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        let obj = value.as_object().unwrap();
        let keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        assert_eq!(keys, sorted, "evidence record fields must already be key-sorted in the raw text");
        assert_eq!(obj["method"], "Describe");
        assert_eq!(obj["run_id"], "run123");
        assert_eq!(obj["settings_hash"], "settingshash");
        assert!(obj["request_hash"].as_str().unwrap().len() == 64);

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn tmp_log_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("av-dynsvc-evidence-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("evidence.jsonl")
    }

    #[test]
    fn first_record_chains_from_the_genesis_sentinel() {
        let path = tmp_log_path("genesis");
        let log = EvidenceLog::open(&path).unwrap();
        assert_eq!(log.chain_head(), GENESIS, "an empty log's chain head is the GENESIS sentinel");
        assert_eq!(log.entry_count(), 0);

        let req = DescribeRequest { model_id: "m".to_string() };
        log.record("Describe", &req, &req, "settingshash", "run1");

        let contents = std::fs::read_to_string(&path).unwrap();
        let entry: serde_json::Value = serde_json::from_str(contents.lines().next().unwrap()).unwrap();
        assert_eq!(entry["seq"], 1);
        assert_eq!(entry["prev_hash"], GENESIS, "the first record's prev_hash is the literal GENESIS string");
        assert_eq!(entry["hash"].as_str().unwrap().len(), 64);
        assert_eq!(log.chain_head(), entry["hash"].as_str().unwrap());
        assert_eq!(log.entry_count(), 1);

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn later_records_chain_prev_hash_to_the_previous_records_hash() {
        let path = tmp_log_path("chain");
        let log = EvidenceLog::open(&path).unwrap();
        let req = DescribeRequest { model_id: "m".to_string() };
        log.record("Describe", &req, &req, "settingshash", "run1");
        log.record("Describe", &req, &req, "settingshash", "run1");
        log.record("Describe", &req, &req, "settingshash", "run1");

        let contents = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<serde_json::Value> = contents.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(lines.len(), 3);
        for (i, entry) in lines.iter().enumerate() {
            assert_eq!(entry["seq"], (i as u64) + 1);
        }
        assert_eq!(lines[1]["prev_hash"], lines[0]["hash"]);
        assert_eq!(lines[2]["prev_hash"], lines[1]["hash"]);
        // Every record's hash is unique -- a chain that collapsed to one repeated hash would
        // silently hide tampering.
        assert_ne!(lines[0]["hash"], lines[1]["hash"]);
        assert_ne!(lines[1]["hash"], lines[2]["hash"]);

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn verify_reports_ok_on_an_untampered_chain() {
        let path = tmp_log_path("verify-ok");
        let log = EvidenceLog::open(&path).unwrap();
        let req = DescribeRequest { model_id: "m".to_string() };
        for _ in 0..5 {
            log.record("Describe", &req, &req, "settingshash", "run1");
        }
        let result = log.verify().unwrap();
        assert!(result.ok, "{result:?}");
        assert_eq!(result.checked, 5);
        assert_eq!(result.broken_at_seq, None);

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn verify_reports_the_empty_log_as_ok() {
        let path = tmp_log_path("verify-empty");
        let log = EvidenceLog::open(&path).unwrap();
        let result = log.verify().unwrap();
        assert!(result.ok);
        assert_eq!(result.checked, 0);
    }

    /// **The tamper-detection test.** Writes several genuine records, then edits one
    /// record's *content* on disk (its `method` field) without recomputing that record's
    /// `hash` -- exactly what an attacker or a bit-flip would do -- and proves `verify()`
    /// both flags the chain as broken and names the exact `seq` it broke at.
    #[test]
    fn verify_detects_a_tampered_record_and_reports_its_sequence_number() {
        let path = tmp_log_path("tamper");
        let log = EvidenceLog::open(&path).unwrap();
        let req = DescribeRequest { model_id: "m".to_string() };
        for _ in 0..4 {
            log.record("Describe", &req, &req, "settingshash", "run1");
        }
        // Sanity: untampered, this chain verifies clean.
        assert!(log.verify().unwrap().ok);

        let contents = std::fs::read_to_string(&path).unwrap();
        let mut lines: Vec<String> = contents.lines().map(str::to_string).collect();
        assert_eq!(lines.len(), 4);

        // Tamper with record #3 (seq=3): change its "method" field but leave "hash"
        // untouched, so the recorded hash no longer matches the record's own content.
        let mut tampered: serde_json::Value = serde_json::from_str(&lines[2]).unwrap();
        assert_eq!(tampered["seq"], 3);
        tampered["method"] = serde_json::Value::String("Propagate".to_string());
        lines[2] = serde_json::to_string(&tampered).unwrap();
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();

        let result = log.verify().unwrap();
        assert!(!result.ok, "verify must detect the tampered record");
        assert_eq!(result.broken_at_seq, Some(3), "verify must name the exact sequence number the chain broke at");
        assert_eq!(result.checked, 2, "records before the break (seq 1, 2) still verify as good");
        assert!(result.detail.contains("tampered"), "{}", result.detail);

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// A record whose `prev_hash` was rewritten to point somewhere else (not just its body)
    /// is also caught, and reported as breaking at that record's own `seq` too -- covering
    /// the other half of the chain link `verify()` checks, not just content tampering.
    #[test]
    fn verify_detects_a_severed_prev_hash_link() {
        let path = tmp_log_path("severed-link");
        let log = EvidenceLog::open(&path).unwrap();
        let req = DescribeRequest { model_id: "m".to_string() };
        for _ in 0..3 {
            log.record("Describe", &req, &req, "settingshash", "run1");
        }
        let contents = std::fs::read_to_string(&path).unwrap();
        let mut lines: Vec<String> = contents.lines().map(str::to_string).collect();

        let mut tampered: serde_json::Value = serde_json::from_str(&lines[1]).unwrap();
        assert_eq!(tampered["seq"], 2);
        tampered["prev_hash"] = serde_json::Value::String("0".repeat(64));
        lines[1] = serde_json::to_string(&tampered).unwrap();
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();

        let result = log.verify().unwrap();
        assert!(!result.ok);
        assert_eq!(result.broken_at_seq, Some(2));
        assert!(result.detail.contains("prev_hash"), "{}", result.detail);

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn evidence_log_survives_reopen_and_continues_the_same_chain() {
        let path = tmp_log_path("reopen");
        {
            let log = EvidenceLog::open(&path).unwrap();
            let req = DescribeRequest { model_id: "m".to_string() };
            log.record("Describe", &req, &req, "settingshash", "run1");
            log.record("Describe", &req, &req, "settingshash", "run1");
        }
        // Reopen (simulating a server restart) and append one more record.
        let log2 = EvidenceLog::open(&path).unwrap();
        assert_eq!(log2.entry_count(), 2, "recovered chain state must count the records already on disk");
        let req = DescribeRequest { model_id: "m".to_string() };
        log2.record("Describe", &req, &req, "settingshash", "run1");

        let result = log2.verify().unwrap();
        assert!(result.ok, "{result:?}");
        assert_eq!(result.checked, 3);

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
