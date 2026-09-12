//! The durable, file-backed, append-only command ledger, chained per partition (ADR-004:
//! "for the engine and command services the durable log itself is the ledger, chained per
//! partition"; `docs/aiplane-plan.md` A1). The partition key is the entity id: every record
//! belonging to one entity lives in exactly one file, in strictly increasing `seq` order,
//! and a different entity's records never interleave into the same chain.
//!
//! # On-disk framing
//!
//! One file per partition, under the ledger directory this crate is opened with. Each file
//! is a sequence of length-prefixed frames: a big-endian `u32` byte length, then exactly
//! that many bytes of one `altavista.v1.LedgerRecord` ([`av_cdm::pb::LedgerRecord`]) encoded
//! by `prost`. `prost`'s encoding is already deterministic for a fixed message value (fixed
//! field emission order; every `map<..>` field anywhere in `altavista.v1` -- including
//! `PolicyInputRate.counts_by_class`, reachable through `LedgerRecord.decision.rate` -- is
//! generated as a `BTreeMap`, `av-cdm/build.rs`'s `.btree_map(["."])`, never a `HashMap`
//! whose iteration order is unspecified), so two calls that build an equal `LedgerRecord`
//! always encode to the same bytes.
//!
//! # Hash chain
//!
//! Mirrors `envelope.proto`'s `SignedBatch.prev_hash`/`hash` convention exactly, restated on
//! `LedgerRecord` itself (see that message's own doc comment in `authority.proto`):
//!
//! - `prev_hash` is the previous record's own `hash` in this partition, or the literal
//!   ASCII bytes of `"GENESIS"` for a partition's first record.
//! - `hash` is `SHA-256(prev_hash || record_body_bytes)`, computed with the **`openssl`**
//!   crate (`openssl::sha::sha256`, ADR-004's crypto rule -- no `sha2`, no `ring`), where
//!   `record_body_bytes` is the same deterministic `prost` encoding described above, of this
//!   record with `prev_hash` and `hash` themselves left at their protobuf zero value (an
//!   empty `bytes` field) -- proto3 omits an empty scalar/bytes field from the wire
//!   entirely, so this is exactly "the canonical encoding of the record excluding
//!   `prev_hash`/`hash`", not an approximation of it.
//!
//! # Determinism (this milestone's acceptance criterion)
//!
//! [`Ledger::append`] never reads the wall clock, never generates a random id, and never
//! generates any id at all -- every id in a [`av_cdm::pb::LedgerRecord`] (`command_id`,
//! `decision_id` inside an attached `PolicyDecision`) is supplied by the caller, and every
//! epoch comes from the injected [`crate::clock::Clock`]. Appending the same sequence of
//! transitions with the same injected clock to two independent, fresh ledger directories
//! therefore produces byte-identical partition files -- proven by this module's own
//! `two_fresh_ledgers_given_the_same_input_produce_byte_identical_files` test.
//!
//! # `verify()`
//!
//! [`Ledger::verify`] walks a partition's file straight from disk, independent of any
//! in-memory chain state this process may hold (a fresh `Ledger` handle over the same
//! directory verifies identically), recomputing every record's `hash` and checking every
//! `prev_hash` link. Its result is shaped exactly like `altavista.v1.ChainVerification`
//! (`envelope.proto`) -- this module reuses that generated type rather than defining a
//! second one, setting `producer_id` to the partition name.
//!
//! Recovery on [`Ledger::open`]/first append to an already-populated partition
//! (`recover_chain_state`) is deliberately **not** tolerant of a truncated/corrupt tail the
//! way `av-dynamics-service`'s `EvidenceLog::open` is: that log is a best-effort side
//! channel that falls back to starting a fresh chain rather than refusing to serve an RPC;
//! this ledger *is* the durable record ADR-004 calls "the ledger itself" -- a truncated tail
//! here is treated as a real fault (an `io::Error`), not silently patched over by starting a
//! second, unrelated chain in the same file's future appends. `verify()` remains the tool
//! that reports a corrupt *interior* record as a finding rather than a hard error, since a
//! `verify` call must always answer, even about a ledger no `append` will ever touch again.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use av_cdm::pb::{ChainVerification, CommandState, CommandTransition, LedgerRecord, PolicyDecision};
use openssl::sha::sha256;
use prost::Message;

/// The suite's ledger convention (`envelope.proto`'s `SignedBatch.prev_hash` doc comment,
/// restated on `LedgerRecord`): a partition's first record's `prev_hash` is this literal
/// string's ASCII bytes, not a hash of anything.
pub const GENESIS: &[u8] = b"GENESIS";

#[derive(Debug, Clone)]
struct ChainState {
    next_seq: u64,
    last_hash: Vec<u8>,
}

impl ChainState {
    fn genesis() -> Self {
        Self { next_seq: 1, last_hash: GENESIS.to_vec() }
    }
}

/// Reads one length-prefixed frame from `file` at its current position. `Ok(None)` means a
/// clean end of file exactly at a frame boundary; a length prefix present without its full
/// body is a real `io::Error` (`UnexpectedEof`), never silently treated as "no more frames".
fn read_frame(file: &mut File) -> io::Result<Option<LedgerRecord>> {
    let mut len_buf = [0u8; 4];
    match file.read_exact(&mut len_buf) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut body = vec![0u8; len];
    file.read_exact(&mut body)?;
    let record = LedgerRecord::decode(body.as_slice()).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    Ok(Some(record))
}

/// `SHA-256(prev_hash || body)` where `body` is `record`'s deterministic `prost` encoding
/// with `prev_hash`/`hash` cleared first -- see the module doc's "Hash chain" section.
/// Takes the whole `record` (rather than its individual fields, which would need seven
/// parameters -- `seq`, `partition`, `tai_ns`, `command_id`, `transition`, `decision`, plus
/// `prev_hash` itself) and clears `record.prev_hash`/`record.hash` internally: this
/// function's contract is that it **always ignores** whatever `prev_hash`/`hash` `record`
/// already carries -- documented here explicitly, not left as an implicit "the caller must
/// remember to pass them empty". `prev_hash` is a separate, required argument precisely so
/// a caller always states which chain link a hash is being computed against, rather than
/// trusting a value already sitting on `record`.
fn compute_hash(prev_hash: &[u8], record: &LedgerRecord) -> Vec<u8> {
    let body = LedgerRecord { prev_hash: Vec::new(), hash: Vec::new(), ..record.clone() };
    let body_bytes = body.encode_to_vec();
    let mut buf = Vec::with_capacity(prev_hash.len() + body_bytes.len());
    buf.extend_from_slice(prev_hash);
    buf.extend_from_slice(&body_bytes);
    sha256(&buf).to_vec()
}

/// Encodes `partition` into a safe, **injective** filename: the SHA-256 hex digest of the
/// partition name's UTF-8 bytes, plus the fixed `.ledger` suffix. Content-addressed rather
/// than character-escaped on purpose -- an earlier character-escaping scheme (alphanumerics/
/// `-`/`_`/`.` pass through, everything else becomes `_XXXX_`) was not injective, because
/// the escape character itself (`_`) was in the passthrough set: `"a/b"` and `"a_002f_b"`
/// both sanitized to `"a_002f_b.ledger"`, which would have silently interleaved two
/// partitions' records into one chain -- exactly the failure ADR-004's per-partition
/// chaining exists to prevent. A hex digest has no such escape-alphabet problem (its output
/// alphabet, `[0-9a-f]`, contains no character this function's own encoding could ever
/// re-introduce a collision through) and trivially satisfies the directory-escape property
/// (`/` and `..` can never appear in a hex digest or in the literal `.ledger` suffix, so the
/// result can never leave the ledger directory) -- both properties are asserted by this
/// module's own tests. Not required to be invertible -- callers that need the true
/// partition name back read it out of the file's own first record (see
/// [`Ledger::partitions`]), never out of the filename.
fn sanitize_partition_filename(partition: &str) -> String {
    format!("{}.ledger", hex_encode(&sha256(partition.as_bytes())))
}

/// One partition's summary, as reported by [`Ledger::partitions`] (and, in turn, by
/// `/admin/api/evidence`).
#[derive(Debug, Clone)]
pub struct PartitionSummary {
    pub partition: String,
    /// Hex-encoded chain head (the most recent record's `hash`), or the literal string
    /// `"GENESIS"` for a partition file with no records yet.
    pub chain_head: String,
    pub records: u64,
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Durable, file-backed, append-only, chained-per-partition command ledger. See the module
/// doc for the on-disk framing and hash chain.
pub struct Ledger {
    dir: PathBuf,
    chains: Mutex<BTreeMap<String, ChainState>>,
}

impl Ledger {
    pub fn open(dir: impl Into<PathBuf>) -> io::Result<Self> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)?;
        Ok(Self { dir, chains: Mutex::new(BTreeMap::new()) })
    }

    fn partition_path(&self, partition: &str) -> PathBuf {
        self.dir.join(sanitize_partition_filename(partition))
    }

    /// Reads a partition's chain state (`next_seq`, `last_hash`) straight from disk by
    /// scanning every frame -- used the first time this process touches a partition
    /// (`append`'s cache miss). See the module doc for why a truncated tail here is a real
    /// `io::Error`, not a silent fallback to a fresh chain.
    fn recover_chain_state(path: &Path) -> io::Result<ChainState> {
        if !path.exists() {
            return Ok(ChainState::genesis());
        }
        let mut file = File::open(path)?;
        let mut state = ChainState::genesis();
        let mut any = false;
        while let Some(record) = read_frame(&mut file)? {
            any = true;
            state.next_seq = record.seq + 1;
            state.last_hash = record.hash;
        }
        if !any {
            return Ok(ChainState::genesis());
        }
        Ok(state)
    }

    /// Appends one record for `command_id`'s `transition` in `partition`'s chain.
    /// `decision` is `Some` for the transition produced by policy evaluation at CHECKED
    /// (A1.2): `COMMAND_STATE_CHECKED` when the policy allowed, `COMMAND_STATE_REJECTED` when
    /// it denied -- a denial must be as reproducible from the ledger as an approval, so it
    /// carries the same `PolicyDecision`, not a lesser record. `None` for every other
    /// transition, including a `COMMAND_STATE_REJECTED` that did not come from a policy
    /// decision. This is the caller's responsibility; this method does not itself inspect
    /// `transition.state`. `command_class` is `Command.command_class` at the time of this
    /// transition -- carried on every record (see [`Self::count_proposed_by_class_in_window`]
    /// for why). Every epoch and every id in the returned record is exactly what the caller
    /// supplied or what `clock` reported at the moment of the call -- see the module doc's
    /// "Determinism" section.
    pub fn append(
        &self,
        partition: &str,
        command_id: &str,
        command_class: &str,
        transition: CommandTransition,
        decision: Option<PolicyDecision>,
        clock: &dyn crate::clock::Clock,
    ) -> io::Result<LedgerRecord> {
        let path = self.partition_path(partition);
        let mut chains = self.chains.lock().unwrap_or_else(|p| p.into_inner());
        if !chains.contains_key(partition) {
            let recovered = Self::recover_chain_state(&path)?;
            chains.insert(partition.to_string(), recovered);
        }
        let state = chains.get_mut(partition).expect("just inserted or already present");

        let seq = state.next_seq;
        let prev_hash = state.last_hash.clone();
        let tai_ns = clock.now_tai_ns();

        // Built with prev_hash/hash still empty: compute_hash clears them anyway (see its
        // own doc), but building the real record shape up front means the fields fed to
        // the hash and the fields written to disk are provably the same eight values.
        let mut record = LedgerRecord {
            seq,
            partition: partition.to_string(),
            prev_hash: Vec::new(),
            hash: Vec::new(),
            tai_ns,
            command_id: command_id.to_string(),
            transition: Some(transition),
            decision,
            command_class: command_class.to_string(),
        };
        let hash = compute_hash(&prev_hash, &record);
        record.prev_hash = prev_hash.clone();
        record.hash = hash.clone();

        let bytes = record.encode_to_vec();
        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
        file.write_all(&(bytes.len() as u32).to_be_bytes())?;
        file.write_all(&bytes)?;
        file.flush()?;

        state.next_seq = seq + 1;
        state.last_hash = hash;
        Ok(record)
    }

    /// Walks `partition`'s file straight from disk, independent of any in-memory chain
    /// state, recomputing every hash and every `prev_hash` link. See the module doc.
    pub fn verify(&self, partition: &str) -> io::Result<ChainVerification> {
        let path = self.partition_path(partition);
        let mut file = match File::open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Ok(ChainVerification { producer_id: partition.to_string(), ok: true, checked: 0, broken_at_sequence: 0, detail: "no ledger file yet".to_string() });
            }
            Err(e) => return Err(e),
        };

        let mut expected_prev = GENESIS.to_vec();
        let mut expected_seq: u64 = 1;
        let mut checked: u64 = 0;

        loop {
            let frame = read_frame(&mut file);
            let record = match frame {
                Ok(Some(r)) => r,
                Ok(None) => break,
                Err(e) => {
                    return Ok(ChainVerification {
                        producer_id: partition.to_string(),
                        ok: false,
                        checked,
                        broken_at_sequence: expected_seq,
                        detail: format!("frame at expected seq {expected_seq}: {e}"),
                    });
                }
            };
            if record.seq != expected_seq {
                return Ok(ChainVerification {
                    producer_id: partition.to_string(),
                    ok: false,
                    checked,
                    broken_at_sequence: record.seq,
                    detail: format!("expected seq {expected_seq}, found seq {}", record.seq),
                });
            }
            if record.partition != partition {
                return Ok(ChainVerification {
                    producer_id: partition.to_string(),
                    ok: false,
                    checked,
                    broken_at_sequence: record.seq,
                    detail: format!(
                        "seq {}: record's own partition field is {:?}, not the partition being verified {:?} -- this file's records were not all written for the same partition",
                        record.seq, record.partition, partition
                    ),
                });
            }
            if record.prev_hash != expected_prev {
                return Ok(ChainVerification {
                    producer_id: partition.to_string(),
                    ok: false,
                    checked,
                    broken_at_sequence: record.seq,
                    detail: format!("seq {}: prev_hash does not match the previous record's hash", record.seq),
                });
            }
            let recomputed = compute_hash(&expected_prev, &record);
            if recomputed != record.hash {
                return Ok(ChainVerification {
                    producer_id: partition.to_string(),
                    ok: false,
                    checked,
                    broken_at_sequence: record.seq,
                    detail: format!("seq {}: recorded hash does not match its recomputed content hash -- the record was tampered with after being written", record.seq),
                });
            }
            expected_prev = record.hash.clone();
            expected_seq = record.seq + 1;
            checked += 1;
        }

        Ok(ChainVerification { producer_id: partition.to_string(), ok: true, checked, broken_at_sequence: 0, detail: "chain intact".to_string() })
    }

    /// Counts, within the trailing window `(as_of_tai_ns - window_ns, as_of_tai_ns]`, the
    /// records in `partition`'s chain whose transition is `COMMAND_STATE_PROPOSED` --
    /// deliberately not any other state -- grouped by each record's own `command_class`.
    ///
    /// `COMMAND_STATE_PROPOSED` is "the state whose arrival defines a command was submitted"
    /// (`crate::rate`'s module doc states this same rule; restated here because this is where
    /// it is enforced): [`crate::state::propose`] is this crate's state machine's *only*
    /// entry point (its own module doc: "the machine's entry point... rather than advancing
    /// an already-started one") and the only transition [`crate::state::CommandError::
    /// AlreadyStarted`] guarantees can never be re-emitted for the same `Command` -- so
    /// counting `PROPOSED` arrivals counts *submissions*, once each, never a re-count from a
    /// command's later transitions (`CHECKED`, `AUTHORIZED`, ...) which are advances of a
    /// command already counted, not new submissions. Counting any other state (e.g.
    /// `CHECKED`) would double up work already reflected by counting `PROPOSED`, and would
    /// even undercount a command a policy denies before ever reaching `CHECKED`.
    ///
    /// Read straight from disk (like [`Self::verify`]), independent of any in-memory chain
    /// state -- used by [`crate::rate::LedgerRateSource`] to answer `PolicyInputRate.
    /// counts_by_class` from real history rather than a caller-supplied guess.
    pub fn count_proposed_by_class_in_window(
        &self,
        partition: &str,
        as_of_tai_ns: i64,
        window_ns: i64,
    ) -> io::Result<BTreeMap<String, u64>> {
        let path = self.partition_path(partition);
        let mut counts: BTreeMap<String, u64> = BTreeMap::new();
        let mut file = match File::open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(counts),
            Err(e) => return Err(e),
        };
        let window_start = as_of_tai_ns.saturating_sub(window_ns);
        while let Some(record) = read_frame(&mut file)? {
            if record.tai_ns <= window_start || record.tai_ns > as_of_tai_ns {
                continue;
            }
            let is_proposed = record
                .transition
                .as_ref()
                .is_some_and(|t| t.state == CommandState::Proposed as i32);
            if is_proposed {
                *counts.entry(record.command_class.clone()).or_insert(0) += 1;
            }
        }
        Ok(counts)
    }

    /// Every partition with a ledger file on disk, with its true partition name (read back
    /// from each file's own first record -- never guessed from the sanitized filename),
    /// chain head and record count. Used by `/admin/api/evidence`.
    pub fn partitions(&self) -> io::Result<Vec<PartitionSummary>> {
        let mut out = Vec::new();
        if !self.dir.exists() {
            return Ok(out);
        }
        let mut paths: Vec<PathBuf> = std::fs::read_dir(&self.dir)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|ext| ext == "ledger"))
            .collect();
        paths.sort();
        for path in paths {
            let mut file = File::open(&path)?;
            let mut partition: Option<String> = None;
            let mut records: u64 = 0;
            let mut last_hash = GENESIS.to_vec();
            while let Some(record) = read_frame(&mut file)? {
                if partition.is_none() {
                    partition = Some(record.partition.clone());
                }
                records = record.seq;
                last_hash = record.hash;
            }
            let Some(partition) = partition else { continue }; // empty file: nothing recorded yet
            out.push(PartitionSummary { partition, chain_head: hex_encode(&last_hash), records });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::TestClock;
    use av_cdm::pb::{AckLevel, CommandState};

    fn transition(state: CommandState, tai_ns: i64) -> CommandTransition {
        CommandTransition { state: state as i32, tai_ns, principal: "operator".to_string(), reason: "reason".to_string(), ack_level: AckLevel::Unspecified as i32, delegation_id: String::new() }
    }

    fn tmp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("av-command-ledger-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// The directory-escape property `sanitize_partition_filename`'s doc comment claims:
    /// the sanitized filename never contains a path separator or a `..` traversal segment,
    /// for a table of adversarial partition names -- so a partition name can never resolve
    /// outside the ledger directory.
    #[test]
    fn sanitize_partition_filename_never_escapes_the_ledger_directory() {
        for name in ["a/b", "../x", "a_002f_b", "_", "a_b", "x\u{00e9}", "\u{1F600}", ""] {
            let sanitized = sanitize_partition_filename(name);
            assert!(!sanitized.contains('/'), "{name:?} -> {sanitized:?}");
            assert!(!sanitized.contains(".."), "{name:?} -> {sanitized:?}");
            assert!(sanitized.ends_with(".ledger"), "{name:?} -> {sanitized:?}");
        }
    }

    /// **The injectivity test** (defect 2 of the A1.1 review): an earlier character-escaping
    /// scheme (alphanumerics/`-`/`_`/`.` pass through, everything else -> `_XXXX_`) let `_`
    /// pass through unescaped while also using `_` as its own escape character, so
    /// `"a/b"` and `"a_002f_b"` both sanitized to the same filename -- two distinct
    /// partitions would then have silently interleaved their records into one chain. This
    /// asserts that specific collision pair is now distinct, plus a wider table of
    /// adversarial names (including the empty string and a non-ASCII one) all sanitize to
    /// pairwise-distinct filenames.
    #[test]
    fn sanitize_partition_filename_is_injective_over_adversarial_names() {
        assert_ne!(
            sanitize_partition_filename("a/b"),
            sanitize_partition_filename("a_002f_b"),
            "the exact collision the old character-escaping scheme produced"
        );

        let names = ["a/b", "a_002f_b", "_", "a_b", "../x", "x", "x\u{00e9}", "sat-1", "sat-2", ""];
        let sanitized: Vec<String> = names.iter().map(|n| sanitize_partition_filename(n)).collect();
        let mut deduped = sanitized.clone();
        deduped.sort();
        deduped.dedup();
        assert_eq!(sanitized.len(), deduped.len(), "sanitize_partition_filename collided on distinct names: {sanitized:?}");
    }

    #[test]
    fn first_record_in_a_partition_chains_from_genesis() {
        let dir = tmp_dir("genesis");
        let ledger = Ledger::open(&dir).unwrap();
        let clock = TestClock::new(1_000);
        let record = ledger.append("sat-1", "cmd-1", "burn", transition(CommandState::Proposed, 1_000), None, &clock).unwrap();
        assert_eq!(record.seq, 1);
        assert_eq!(record.prev_hash, GENESIS);
        assert_eq!(record.hash.len(), 32);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn later_records_chain_prev_hash_to_the_previous_records_hash() {
        let dir = tmp_dir("chain");
        let ledger = Ledger::open(&dir).unwrap();
        let clock = TestClock::new(0);
        let r1 = ledger.append("sat-1", "cmd-1", "burn", transition(CommandState::Proposed, 0), None, &clock).unwrap();
        clock.advance(1);
        let r2 = ledger.append("sat-1", "cmd-1", "burn", transition(CommandState::Checked, 1), None, &clock).unwrap();
        clock.advance(1);
        let r3 = ledger.append("sat-1", "cmd-1", "burn", transition(CommandState::Authorized, 2), None, &clock).unwrap();
        assert_eq!(r2.prev_hash, r1.hash);
        assert_eq!(r3.prev_hash, r2.hash);
        assert_ne!(r1.hash, r2.hash);
        assert_ne!(r2.hash, r3.hash);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn different_partitions_get_independent_chains() {
        let dir = tmp_dir("partitions");
        let ledger = Ledger::open(&dir).unwrap();
        let clock = TestClock::new(0);
        let a1 = ledger.append("sat-a", "cmd-a", "burn", transition(CommandState::Proposed, 0), None, &clock).unwrap();
        let b1 = ledger.append("sat-b", "cmd-b", "burn", transition(CommandState::Proposed, 0), None, &clock).unwrap();
        assert_eq!(a1.seq, 1);
        assert_eq!(b1.seq, 1, "a second partition's first record also starts at seq 1");
        assert_eq!(a1.prev_hash, GENESIS);
        assert_eq!(b1.prev_hash, GENESIS);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_reports_ok_on_an_untampered_chain() {
        let dir = tmp_dir("verify-ok");
        let ledger = Ledger::open(&dir).unwrap();
        let clock = TestClock::new(0);
        for i in 0..5 {
            ledger.append("sat-1", "cmd-1", "burn", transition(CommandState::Proposed, i), None, &clock).unwrap();
        }
        let result = ledger.verify("sat-1").unwrap();
        assert!(result.ok, "{result:?}");
        assert_eq!(result.checked, 5);
        assert_eq!(result.broken_at_sequence, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_reports_ok_for_a_partition_with_no_file_yet() {
        let dir = tmp_dir("verify-missing");
        let ledger = Ledger::open(&dir).unwrap();
        let result = ledger.verify("never-appended").unwrap();
        assert!(result.ok);
        assert_eq!(result.checked, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The tamper-detection test: decodes one record, mutates its content (`command_id`)
    /// while leaving its own recorded `hash`/`prev_hash` bytes exactly as originally
    /// written, re-encodes and rewrites the file -- exactly the effect a hand edit or a
    /// bit-flip inside the body would have -- and proves `verify()` flags the chain as
    /// broken and names the exact `seq` it broke at.
    #[test]
    fn verify_detects_a_tampered_record_body_and_reports_its_sequence_number() {
        let dir = tmp_dir("tamper-body");
        let ledger = Ledger::open(&dir).unwrap();
        let clock = TestClock::new(0);
        for i in 0..4 {
            ledger.append("sat-1", "cmd-1", "burn", transition(CommandState::Proposed, i), None, &clock).unwrap();
        }
        assert!(ledger.verify("sat-1").unwrap().ok, "sanity: untampered chain verifies clean");

        let path = ledger.partition_path("sat-1");
        let mut file = File::open(&path).unwrap();
        let mut records = Vec::new();
        while let Some(r) = read_frame(&mut file).unwrap() {
            records.push(r);
        }
        drop(file);
        assert_eq!(records.len(), 4);
        assert_eq!(records[2].seq, 3);
        records[2].command_id = "tampered-command-id".to_string(); // hash/prev_hash left untouched

        let mut rebuilt = Vec::new();
        for r in &records {
            let bytes = r.encode_to_vec();
            rebuilt.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
            rebuilt.extend_from_slice(&bytes);
        }
        std::fs::write(&path, &rebuilt).unwrap();

        let result = ledger.verify("sat-1").unwrap();
        assert!(!result.ok, "verify must detect the tampered record");
        assert_eq!(result.broken_at_sequence, 3, "must name the exact sequence number the chain broke at");
        assert_eq!(result.checked, 2, "records before the break (seq 1, 2) still verify as good");
        assert!(result.detail.contains("tampered"), "{}", result.detail);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A record whose `prev_hash` was rewritten to point somewhere else (not the body) is
    /// also caught, reported at that record's own `seq`.
    #[test]
    fn verify_detects_a_severed_prev_hash_link() {
        let dir = tmp_dir("tamper-prev-hash");
        let ledger = Ledger::open(&dir).unwrap();
        let clock = TestClock::new(0);
        for i in 0..3 {
            ledger.append("sat-1", "cmd-1", "burn", transition(CommandState::Proposed, i), None, &clock).unwrap();
        }
        let path = ledger.partition_path("sat-1");
        let mut file = File::open(&path).unwrap();
        let r1 = read_frame(&mut file).unwrap().unwrap();
        let mut r2 = read_frame(&mut file).unwrap().unwrap();
        drop(file);
        assert_eq!(r2.seq, 2);
        r2.prev_hash = vec![0u8; 32];
        assert_ne!(r2.prev_hash, r1.hash);

        // Rewrite the file: frame 1 unchanged, frame 2 replaced (same length is not
        // required -- rebuild the whole file from the in-memory frames plus the trailing
        // untouched third frame).
        let mut file = File::open(&path).unwrap();
        let _ = read_frame(&mut file).unwrap();
        let _ = read_frame(&mut file).unwrap();
        let r3 = read_frame(&mut file).unwrap().unwrap();
        drop(file);

        let mut rebuilt = Vec::new();
        for r in [&r1, &r2, &r3] {
            let bytes = r.encode_to_vec();
            rebuilt.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
            rebuilt.extend_from_slice(&bytes);
        }
        std::fs::write(&path, &rebuilt).unwrap();

        let result = ledger.verify("sat-1").unwrap();
        assert!(!result.ok);
        assert_eq!(result.broken_at_sequence, 2);
        assert!(result.detail.contains("prev_hash"), "{}", result.detail);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **Hardens a defect the A1.1 review found:** `verify()` never checked that a decoded
    /// record's own `partition` field matches the partition being verified. Without that
    /// check, a record that was correctly hash-chained for a *different* partition (or a
    /// whole file copied/misfiled under the wrong partition's name) would still pass every
    /// existing check here -- the recomputed hash matches (it is computed over the record's
    /// own body, whatever that body's `partition` field says) and `prev_hash` still points at
    /// the previous record's real `hash` -- and `verify()` would wrongly report `ok: true`.
    /// This test tampers only the *last* record's `partition` field and recomputes that one
    /// record's own `hash` to match (so the hash chain alone stays fully self-consistent,
    /// isolating the assertion to the new partition check, not a hash mismatch it would also
    /// trip): `verify()` must still report the chain broken, at that record's exact `seq`.
    #[test]
    fn verify_detects_a_record_whose_partition_field_does_not_match() {
        let dir = tmp_dir("tamper-partition");
        let ledger = Ledger::open(&dir).unwrap();
        let clock = TestClock::new(0);
        for i in 0..3 {
            ledger.append("sat-1", "cmd-1", "burn", transition(CommandState::Proposed, i), None, &clock).unwrap();
        }
        assert!(ledger.verify("sat-1").unwrap().ok, "sanity: untampered chain verifies clean");

        let path = ledger.partition_path("sat-1");
        let mut file = File::open(&path).unwrap();
        let mut records = Vec::new();
        while let Some(r) = read_frame(&mut file).unwrap() {
            records.push(r);
        }
        drop(file);
        assert_eq!(records.len(), 3);

        // Tamper the last record's partition field, then recompute *its own* hash (over its
        // own prev_hash, unchanged) so the hash chain itself stays internally consistent --
        // the only thing wrong is which partition this record claims to belong to.
        let last = records.len() - 1;
        let prev_hash = records[last].prev_hash.clone();
        records[last].partition = "sat-2".to_string();
        records[last].hash = compute_hash(&prev_hash, &records[last]);

        let mut rebuilt = Vec::new();
        for r in &records {
            let bytes = r.encode_to_vec();
            rebuilt.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
            rebuilt.extend_from_slice(&bytes);
        }
        std::fs::write(&path, &rebuilt).unwrap();

        let result = ledger.verify("sat-1").unwrap();
        assert!(!result.ok, "verify must detect the partition-field mismatch");
        assert_eq!(result.broken_at_sequence, 3, "must name the exact sequence number the mismatch is at");
        assert_eq!(result.checked, 2, "records before the mismatched one still verify as good");
        assert!(result.detail.contains("partition"), "{}", result.detail);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ledger_survives_reopen_and_continues_the_same_chain() {
        let dir = tmp_dir("reopen");
        {
            let ledger = Ledger::open(&dir).unwrap();
            let clock = TestClock::new(0);
            ledger.append("sat-1", "cmd-1", "burn", transition(CommandState::Proposed, 0), None, &clock).unwrap();
            ledger.append("sat-1", "cmd-1", "burn", transition(CommandState::Checked, 1), None, &clock).unwrap();
        }
        let ledger2 = Ledger::open(&dir).unwrap();
        let clock = TestClock::new(2);
        let r3 = ledger2.append("sat-1", "cmd-1", "burn", transition(CommandState::Authorized, 2), None, &clock).unwrap();
        assert_eq!(r3.seq, 3, "recovered chain state must count the records already on disk");
        let result = ledger2.verify("sat-1").unwrap();
        assert!(result.ok, "{result:?}");
        assert_eq!(result.checked, 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn partitions_reports_the_true_partition_name_chain_head_and_count() {
        let dir = tmp_dir("summary");
        let ledger = Ledger::open(&dir).unwrap();
        let clock = TestClock::new(0);
        ledger.append("sat/weird name", "cmd-1", "burn", transition(CommandState::Proposed, 0), None, &clock).unwrap();
        let last = ledger.append("sat/weird name", "cmd-1", "burn", transition(CommandState::Checked, 1), None, &clock).unwrap();

        let summaries = ledger.partitions().unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].partition, "sat/weird name", "the true partition name, not the sanitized filename");
        assert_eq!(summaries[0].records, 2);
        assert_eq!(summaries[0].chain_head, hex_encode(&last.hash));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **The acceptance test for this milestone.** The same sequence of transitions, with
    /// the same injected clock, appended to two independent, fresh ledger directories, must
    /// produce byte-identical partition files -- proving no wall clock, no randomness and no
    /// generated id anywhere in the append path.
    #[test]
    fn two_fresh_ledgers_given_the_same_input_produce_byte_identical_files() {
        let dir_a = tmp_dir("determinism-a");
        let dir_b = tmp_dir("determinism-b");
        let ledger_a = Ledger::open(&dir_a).unwrap();
        let ledger_b = Ledger::open(&dir_b).unwrap();

        let decision = PolicyDecision {
            decision_id: "decision-1".to_string(),
            allow: true,
            policy_hash: "abc123".to_string(),
            reasons: vec!["command_class burn is admitted".to_string()],
            matched_rule_path: "data.altavista.command.allow".to_string(),
            evaluated_tai_ns: 1_000,
            input: None,
        };
        let sequence: Vec<(&str, &str, &str, CommandTransition, Option<PolicyDecision>)> = vec![
            ("sat-1", "cmd-1", "burn", transition(CommandState::Proposed, 1_000), None),
            ("sat-1", "cmd-1", "burn", transition(CommandState::Checked, 1_000), Some(decision)),
            ("sat-1", "cmd-1", "burn", transition(CommandState::Authorized, 1_500), None),
            ("sat-2", "cmd-2", "burn", transition(CommandState::Proposed, 2_000), None),
        ];

        for ledger in [&ledger_a, &ledger_b] {
            let clock = TestClock::new(1_000);
            for (partition, command_id, command_class, transition, decision) in &sequence {
                clock.set(transition.tai_ns);
                ledger.append(partition, command_id, command_class, transition.clone(), decision.clone(), &clock).unwrap();
            }
        }

        let path_a_1 = ledger_a.partition_path("sat-1");
        let path_b_1 = ledger_b.partition_path("sat-1");
        let path_a_2 = ledger_a.partition_path("sat-2");
        let path_b_2 = ledger_b.partition_path("sat-2");
        let bytes_a_1 = std::fs::read(&path_a_1).unwrap();
        let bytes_b_1 = std::fs::read(&path_b_1).unwrap();
        let bytes_a_2 = std::fs::read(&path_a_2).unwrap();
        let bytes_b_2 = std::fs::read(&path_b_2).unwrap();

        assert_eq!(bytes_a_1, bytes_b_1, "partition sat-1 must be byte-identical across the two fresh ledgers");
        assert_eq!(bytes_a_2, bytes_b_2, "partition sat-2 must be byte-identical across the two fresh ledgers");
        assert!(!bytes_a_1.is_empty());
        assert!(!bytes_a_2.is_empty());

        let _ = std::fs::remove_dir_all(&dir_a);
        let _ = std::fs::remove_dir_all(&dir_b);
    }

    /// [`Ledger::count_proposed_by_class_in_window`] counts only `COMMAND_STATE_PROPOSED`
    /// records (not `CHECKED`/`AUTHORIZED`/... of the same commands, which would double-count
    /// a submission already counted at its `PROPOSED` arrival), grouped by `command_class`,
    /// and only those whose `tai_ns` falls inside `(as_of - window_ns, as_of]` -- exercised
    /// with a `TestClock` driving each append's epoch explicitly, no sleeping.
    #[test]
    fn count_proposed_by_class_in_window_counts_only_proposed_records_inside_the_window() {
        let dir = tmp_dir("rate-count");
        let ledger = Ledger::open(&dir).unwrap();
        let clock = TestClock::new(0);

        // Two "burn" submissions and one "mode" submission, all inside the window.
        clock.set(1_000);
        ledger.append("sat-1", "cmd-1", "burn", transition(CommandState::Proposed, 1_000), None, &clock).unwrap();
        clock.set(1_100);
        ledger.append("sat-1", "cmd-1", "burn", transition(CommandState::Checked, 1_100), None, &clock).unwrap();
        clock.set(1_200);
        ledger.append("sat-1", "cmd-2", "burn", transition(CommandState::Proposed, 1_200), None, &clock).unwrap();
        clock.set(1_300);
        ledger.append("sat-1", "cmd-3", "mode", transition(CommandState::Proposed, 1_300), None, &clock).unwrap();

        // A "burn" submission long before the window opens -- must not be counted. `append`
        // takes its record's own `tai_ns` from the clock's current value, not from the
        // `CommandTransition.tai_ns` the `transition()` helper embeds -- so the clock must be
        // set back explicitly, not just given a transition struct that says "0".
        clock.set(0);
        ledger.append("sat-1", "cmd-0", "burn", transition(CommandState::Proposed, 0), None, &clock).unwrap();
        clock.set(1_300);

        // A different partition's submission -- must not leak into sat-1's count.
        ledger.append("sat-2", "cmd-9", "burn", transition(CommandState::Proposed, 1_200), None, &clock).unwrap();

        let counts = ledger.count_proposed_by_class_in_window("sat-1", 1_500, 1_000).unwrap();
        assert_eq!(counts.get("burn").copied(), Some(2), "{counts:?}");
        assert_eq!(counts.get("mode").copied(), Some(1), "{counts:?}");
        assert_eq!(counts.len(), 2, "CHECKED is not counted, and the out-of-window/other-partition records are not counted: {counts:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
