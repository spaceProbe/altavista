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

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use av_cdm::pb::{ChainVerification, Command, CommandProposal, CommandState, CommandTransition, LedgerRecord, PolicyDecision};
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

/// Identifies the `Command` a [`Ledger::append`] call is about: the four `LedgerRecord`
/// fields that come straight from the `Command` itself rather than from the `transition`/
/// `decision`/`clock` this crate is appending because of. Grouped into one struct (A1.3,
/// added alongside `idempotency_key`) so `append` itself has one parameter here instead of
/// four bare `&str`s -- clippy's `too_many_arguments` lint would otherwise flag `append`
/// once `idempotency_key` joined `partition`/`command_id`/`command_class`, and this crate's
/// rule against lint-suppressing attributes on hand-written items means the fix is grouping
/// the arguments, never silencing the lint in place.
#[derive(Debug, Clone, Copy)]
pub struct CommandMeta<'a> {
    /// The entity id -- the ledger partition key.
    pub partition: &'a str,
    pub command_id: &'a str,
    pub command_class: &'a str,
    /// `Command.idempotency_key`, empty when the command declared none.
    pub idempotency_key: &'a str,
}

impl<'a> CommandMeta<'a> {
    pub fn new(partition: &'a str, command_id: &'a str, command_class: &'a str, idempotency_key: &'a str) -> Self {
        Self { partition, command_id, command_class, idempotency_key }
    }
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

    /// Appends one record for [`CommandMeta::command_id`]'s `transition` in
    /// [`CommandMeta::partition`]'s chain. `decision` is `Some` for the transition produced
    /// by policy evaluation at CHECKED (A1.2): `COMMAND_STATE_CHECKED` when the policy
    /// allowed, `COMMAND_STATE_REJECTED` when it denied -- a denial must be as reproducible
    /// from the ledger as an approval, so it carries the same `PolicyDecision`, not a lesser
    /// record. `None` for every other transition, including a `COMMAND_STATE_REJECTED` that
    /// did not come from a policy decision. This is the caller's responsibility; this method
    /// does not itself inspect `transition.state`. [`CommandMeta::command_class`]/
    /// [`CommandMeta::idempotency_key`] are `Command.command_class`/`Command.
    /// idempotency_key` at the time of this transition -- carried on every record (see
    /// [`Self::count_proposed_by_class_in_window`]/[`Self::scan_dispatched_idempotency_keys`]
    /// for why); `idempotency_key` empty means the command declared none (this method does
    /// not itself enforce uniqueness; that is `crate::service::CommandAuthorityServiceImpl::
    /// dispatch`'s job, in memory *and* now rebuilt from this ledger at construction).
    ///
    /// `command` (A1.3-round-2, question 203(a)) is the full `Command` exactly as it stood
    /// after this transition -- the same value the caller already produced by driving
    /// [`crate::state`]'s edge functions, cloned onto `LedgerRecord.command` verbatim, never
    /// reassembled from `meta`/`transition`/`decision`. `None` only in tests that exercise
    /// this ledger's hash-chain/rate/idempotency behaviour without a real `Command` to hand
    /// (see [`Self::scan_commands`]'s own doc for how a `None` record is treated on rebuild);
    /// every production call site (`crate::authority::check_command`, `crate::service::
    /// CommandAuthorityServiceImpl::append_and_commit_index`) always passes `Some`. Every epoch
    /// and every id in the returned record -- including every field nested inside `command` --
    /// is exactly what the caller supplied or what `clock` reported at the moment of the call;
    /// this parameter reads no wall clock and generates no id of its own, so it does not
    /// disturb the module doc's "Determinism" section.
    pub fn append(
        &self,
        meta: CommandMeta<'_>,
        transition: CommandTransition,
        decision: Option<PolicyDecision>,
        command: Option<&Command>,
        proposal: Option<CommandProposal>,
        clock: &dyn crate::clock::Clock,
    ) -> io::Result<LedgerRecord> {
        // The self-evidence invariant `LedgerRecord.command`'s own doc comment states
        // (`authority.proto`, field 11): the attached `Command` is the POST-transition value,
        // so its `state` is this transition's state and its `transitions` end with this exact
        // transition. Documenting that and pinning it in one hand-built fixture is not enough
        // -- nothing would stop a future call site attaching a stale or mismatched `Command`,
        // and the ledger would then durably record a disagreement that `Self::scan_commands`
        // would hand straight back to `Query` with no trace anything was wrong. Checked here
        // so every append the crate's own suite performs -- the gRPC integration tests
        // included, which is what actually exercises the two production call sites
        // (`crate::authority::check_command`, `crate::service::CommandAuthorityServiceImpl::
        // append_and_commit_index`) -- proves its caller upheld it.
        //
        // R3.5a (manager's review): these were `debug_assert_eq!`, which the compiler REMOVES
        // from a release build -- so the invariant round 2's review installed, and which round
        // 2's status and the lead's acceptance both record as "checked in `Ledger::append`",
        // held only in the test binaries and vanished in the shipped `av-command` binary and
        // in the container image, which are exactly where a durable, self-contradictory ledger
        // record would actually matter. A guarantee that exists under `cargo test` and not in
        // production is the same "failure that leaves no trace" shape this track keeps finding,
        // one level up. They are now real checks in every profile, and they REFUSE the append
        // (`InvalidInput`) rather than panicking a running service: the record is never written,
        // the caller's own `map_err(ServiceError::Io)` turns it into a counted refusal, and the
        // ledger cannot contain the disagreement at all.
        if let Some(command) = command {
            if command.state != transition.state {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "LedgerRecord.command must be the POST-transition Command: its state {} disagrees with the transition being recorded ({}) for command_id {}",
                        command.state, transition.state, meta.command_id
                    ),
                ));
            }
            if command.transitions.last() != Some(&transition) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "LedgerRecord.command must be the POST-transition Command: its last transition is not the transition being recorded for command_id {}",
                        meta.command_id
                    ),
                ));
            }
        }
        // R3.5a: the identical self-evidence discipline as `command` above, now for
        // `LedgerRecord.proposal` (`authority.proto` field 12): it is self-evidence about
        // THIS record, not a caller's independently-supplied claim, so a `proposal` whose own
        // `command.id` disagrees with `meta.command_id`, or one attached to a transition that
        // is not `PROPOSED`, is a bug in the caller this method must catch -- documenting the
        // invariant on the proto field alone (as `LedgerRecord.command`'s own field 11
        // originally was, before round 2's review) would leave nothing to stop a future call
        // site attaching a stale/mismatched proposal, and the ledger would then durably record
        // a disagreement `Self::scan_proposals` would hand straight back to `Query` with no
        // trace anything was wrong.
        // Real checks in every profile, for the same reason as `command`'s above.
        if let Some(proposal) = &proposal {
            let proposal_command_id = proposal.command.as_ref().map(|c| c.id.as_str()).unwrap_or("");
            if proposal_command_id != meta.command_id {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "LedgerRecord.proposal must be self-evidence about this record: its own command.id {proposal_command_id:?} disagrees with the record's command_id {}",
                        meta.command_id
                    ),
                ));
            }
            if transition.state != CommandState::Proposed as i32 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "LedgerRecord.proposal is set on the PROPOSED record and only there; this append call attached one to a transition at state {} for command_id {}",
                        transition.state, meta.command_id
                    ),
                ));
            }
        }
        let partition = meta.partition;
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
        // the hash and the fields written to disk are provably the same ten values.
        let mut record = LedgerRecord {
            seq,
            partition: partition.to_string(),
            prev_hash: Vec::new(),
            hash: Vec::new(),
            tai_ns,
            command_id: meta.command_id.to_string(),
            transition: Some(transition),
            decision,
            command_class: meta.command_class.to_string(),
            idempotency_key: meta.idempotency_key.to_string(),
            command: command.cloned(),
            proposal,
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

    /// Every non-empty `idempotency_key` carried by a `COMMAND_STATE_DISPATCHED` record,
    /// across **every partition** this ledger has a file for -- not scoped to one partition,
    /// since a duplicate-dispatch refusal (`crate::service::CommandAuthorityServiceImpl::
    /// dispatch`) is a service-wide guarantee, not a per-entity one (`command.proto`'s own
    /// doc comment on `Command.idempotency_key`, "the edge never dispatches the same key
    /// twice", names no partition scope). Read straight from disk (like [`Self::verify`] and
    /// [`Self::partitions`]), independent of any in-memory state -- this is exactly what
    /// [`crate::service::CommandAuthorityServiceImpl::new`] calls once, at construction, to
    /// rebuild its in-process duplicate-dispatch guard so that guarantee survives a process
    /// restart rather than living only in RAM (the defect this method exists to close).
    ///
    /// A record with an empty `idempotency_key` is never collected (matches `crate::
    /// service`'s own reading of an empty key as "this command opts out of deduplication");
    /// a key that appears on more than one `DISPATCHED` record collapses to one set member,
    /// since the caller only ever needs "was this key dispatched at all", never a count.
    pub fn scan_dispatched_idempotency_keys(&self) -> io::Result<BTreeSet<String>> {
        let mut keys = BTreeSet::new();
        if !self.dir.exists() {
            return Ok(keys);
        }
        let mut paths: Vec<PathBuf> = std::fs::read_dir(&self.dir)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|ext| ext == "ledger"))
            .collect();
        paths.sort();
        for path in paths {
            let mut file = File::open(&path)?;
            while let Some(record) = read_frame(&mut file)? {
                if record.idempotency_key.is_empty() {
                    continue;
                }
                let is_dispatched = record.transition.as_ref().is_some_and(|t| t.state == CommandState::Dispatched as i32);
                if is_dispatched {
                    keys.insert(record.idempotency_key.clone());
                }
            }
        }
        Ok(keys)
    }

    /// Rebuilds the latest known `Command` for every `command_id` this ledger has ever
    /// recorded a transition for, across **every partition** -- read straight from disk (like
    /// [`Self::verify`]/[`Self::partitions`]/[`Self::scan_dispatched_idempotency_keys`]),
    /// independent of any in-memory state. This is exactly what `crate::service::
    /// CommandAuthorityServiceImpl::new` calls once, at construction, to rebuild its `commands`
    /// index before serving a single RPC -- the same construction-time discipline
    /// [`Self::scan_dispatched_idempotency_keys`] already established for the duplicate-
    /// dispatch guard, now closing question 203(a)'s "`Query` does not survive a restart" gap.
    ///
    /// Each partition's own file is read frame-by-frame in `seq` order (append-only, so this
    /// is also chronological order); a record whose `command` (`LedgerRecord.command`,
    /// A1.3-round-2) is present overwrites whatever this scan already held for that
    /// `command_id`, so the map ends up holding each command's *last* recorded snapshot -- its
    /// current state, not its first. A record with no `command` attached (only possible for a
    /// record a test appended with `command: None` -- every production append always attaches
    /// one, see [`Self::append`]'s own doc) is skipped rather than overwriting a real snapshot
    /// with nothing: this scan only ever improves what it already knows about a `command_id`,
    /// never erases it.
    pub fn scan_commands(&self) -> io::Result<BTreeMap<String, Command>> {
        let mut commands: BTreeMap<String, Command> = BTreeMap::new();
        if !self.dir.exists() {
            return Ok(commands);
        }
        let mut paths: Vec<PathBuf> = std::fs::read_dir(&self.dir)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|ext| ext == "ledger"))
            .collect();
        paths.sort();
        for path in paths {
            let mut file = File::open(&path)?;
            while let Some(record) = read_frame(&mut file)? {
                if let Some(command) = record.command {
                    commands.insert(record.command_id.clone(), command);
                }
            }
        }
        Ok(commands)
    }

    /// Rebuilds every `command_id`'s original `CommandProposal` (rationale + evidence ids) --
    /// R3.5a, `docs/aiplane-plan.md` milestone A5's "proposals with their rationale and
    /// evidence" -- from the ledger alone, exactly the same construction-time discipline
    /// [`Self::scan_commands`]/[`Self::scan_dispatched_idempotency_keys`] already establish:
    /// read straight from disk, independent of any in-memory state, across every partition,
    /// in `seq` (chronological) order.
    ///
    /// [`LedgerRecord::proposal`] is set on exactly one record per `command_id` -- the
    /// `PROPOSED` record, [`Self::append`]'s own invariant above -- so, unlike
    /// [`Self::scan_commands`]'s "last one wins" rule, a later record can never overwrite an
    /// earlier `command_id`'s proposal: `insert` only ever happens once per id (the `PROPOSED`
    /// record is always the first record `Self::append` will ever see for a fresh
    /// `command_id`, since `propose` is this crate's state machine's only entry point). A
    /// record with no attached `proposal` (every non-`PROPOSED` record, and any `PROPOSED`
    /// record a test appended with `proposal: None` -- every production `Propose` call
    /// attaches one, see `crate::service::CommandAuthorityServiceImpl::propose`) is skipped.
    pub fn scan_proposals(&self) -> io::Result<BTreeMap<String, CommandProposal>> {
        let mut proposals: BTreeMap<String, CommandProposal> = BTreeMap::new();
        if !self.dir.exists() {
            return Ok(proposals);
        }
        let mut paths: Vec<PathBuf> = std::fs::read_dir(&self.dir)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|ext| ext == "ledger"))
            .collect();
        paths.sort();
        for path in paths {
            let mut file = File::open(&path)?;
            while let Some(record) = read_frame(&mut file)? {
                if let Some(proposal) = record.proposal {
                    proposals.entry(record.command_id.clone()).or_insert(proposal);
                }
            }
        }
        Ok(proposals)
    }

    /// Rebuilds every `command_id`'s [`PolicyDecision`] (R3.5a: "the decision is already on
    /// the `CHECKED`/`REJECTED` ledger record ... so a console that arrives later can never
    /// see why a command was checked or rejected") from the ledger alone -- the identical
    /// disk-only, every-partition, `seq`-order discipline as [`Self::scan_proposals`]/
    /// [`Self::scan_commands`] above.
    ///
    /// `LedgerRecord.decision` (`authority.proto` field 8) is set on at most one record per
    /// `command_id` in this crate's own production paths (the `CHECKED` or `REJECTED` record
    /// produced by `crate::authority::check_command`'s one policy evaluation -- a command is
    /// never re-checked once it leaves `PROPOSED`), so "first one wins" (`or_insert`, matching
    /// [`Self::scan_proposals`]'s own rule) and "last one wins" agree in practice; `or_insert`
    /// is chosen anyway, for the same reason as `scan_proposals`, rather than `insert`, since
    /// this scan makes no attempt to detect or repair a ledger that (only by a bug elsewhere)
    /// recorded more than one.
    pub fn scan_decisions(&self) -> io::Result<BTreeMap<String, PolicyDecision>> {
        let mut decisions: BTreeMap<String, PolicyDecision> = BTreeMap::new();
        if !self.dir.exists() {
            return Ok(decisions);
        }
        let mut paths: Vec<PathBuf> = std::fs::read_dir(&self.dir)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|ext| ext == "ledger"))
            .collect();
        paths.sort();
        for path in paths {
            let mut file = File::open(&path)?;
            while let Some(record) = read_frame(&mut file)? {
                if let Some(decision) = record.decision {
                    decisions.entry(record.command_id.clone()).or_insert(decision);
                }
            }
        }
        Ok(decisions)
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
        let record = ledger.append(CommandMeta::new("sat-1", "cmd-1", "burn", ""), transition(CommandState::Proposed, 1_000), None, None, None, &clock).unwrap();
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
        let r1 = ledger.append(CommandMeta::new("sat-1", "cmd-1", "burn", ""), transition(CommandState::Proposed, 0), None, None, None, &clock).unwrap();
        clock.advance(1);
        let r2 = ledger.append(CommandMeta::new("sat-1", "cmd-1", "burn", ""), transition(CommandState::Checked, 1), None, None, None, &clock).unwrap();
        clock.advance(1);
        let r3 = ledger.append(CommandMeta::new("sat-1", "cmd-1", "burn", ""), transition(CommandState::Authorized, 2), None, None, None, &clock).unwrap();
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
        let a1 = ledger.append(CommandMeta::new("sat-a", "cmd-a", "burn", ""), transition(CommandState::Proposed, 0), None, None, None, &clock).unwrap();
        let b1 = ledger.append(CommandMeta::new("sat-b", "cmd-b", "burn", ""), transition(CommandState::Proposed, 0), None, None, None, &clock).unwrap();
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
            ledger.append(CommandMeta::new("sat-1", "cmd-1", "burn", ""), transition(CommandState::Proposed, i), None, None, None, &clock).unwrap();
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
            ledger.append(CommandMeta::new("sat-1", "cmd-1", "burn", ""), transition(CommandState::Proposed, i), None, None, None, &clock).unwrap();
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
            ledger.append(CommandMeta::new("sat-1", "cmd-1", "burn", ""), transition(CommandState::Proposed, i), None, None, None, &clock).unwrap();
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
            ledger.append(CommandMeta::new("sat-1", "cmd-1", "burn", ""), transition(CommandState::Proposed, i), None, None, None, &clock).unwrap();
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
            ledger.append(CommandMeta::new("sat-1", "cmd-1", "burn", ""), transition(CommandState::Proposed, 0), None, None, None, &clock).unwrap();
            ledger.append(CommandMeta::new("sat-1", "cmd-1", "burn", ""), transition(CommandState::Checked, 1), None, None, None, &clock).unwrap();
        }
        let ledger2 = Ledger::open(&dir).unwrap();
        let clock = TestClock::new(2);
        let r3 = ledger2.append(CommandMeta::new("sat-1", "cmd-1", "burn", ""), transition(CommandState::Authorized, 2), None, None, None, &clock).unwrap();
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
        ledger.append(CommandMeta::new("sat/weird name", "cmd-1", "burn", ""), transition(CommandState::Proposed, 0), None, None, None, &clock).unwrap();
        let last = ledger.append(CommandMeta::new("sat/weird name", "cmd-1", "burn", ""), transition(CommandState::Checked, 1), None, None, None, &clock).unwrap();

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
        // `command` (the new field this test also proves is deterministic) is attached on
        // the CHECKED transition, non-default in enough places (a `transitions` entry, a
        // non-empty `label`) that a non-deterministic encoding of it would be caught here too.
        let checked_command = Command {
            id: "cmd-1".to_string(),
            entity_id: "sat-1".to_string(),
            command_class: "burn".to_string(),
            idempotency_key: "idem-1".to_string(),
            state: CommandState::Checked as i32,
            transitions: vec![transition(CommandState::Proposed, 1_000), transition(CommandState::Checked, 1_000)],
            label: Some(av_cdm::pb::Label { marking: "CUI".to_string(), caveats: vec!["NOFORN".to_string()] }),
            ..Command::default()
        };
        let sequence: Vec<(CommandMeta<'_>, CommandTransition, Option<PolicyDecision>, Option<Command>)> = vec![
            (CommandMeta::new("sat-1", "cmd-1", "burn", "idem-1"), transition(CommandState::Proposed, 1_000), None, None),
            (CommandMeta::new("sat-1", "cmd-1", "burn", "idem-1"), transition(CommandState::Checked, 1_000), Some(decision), Some(checked_command)),
            (CommandMeta::new("sat-1", "cmd-1", "burn", "idem-1"), transition(CommandState::Authorized, 1_500), None, None),
            (CommandMeta::new("sat-2", "cmd-2", "burn", ""), transition(CommandState::Proposed, 2_000), None, None),
        ];

        for ledger in [&ledger_a, &ledger_b] {
            let clock = TestClock::new(1_000);
            for (meta, transition, decision, command) in &sequence {
                clock.set(transition.tai_ns);
                ledger.append(*meta, transition.clone(), decision.clone(), command.as_ref(), None, &clock).unwrap();
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
        ledger.append(CommandMeta::new("sat-1", "cmd-1", "burn", ""), transition(CommandState::Proposed, 1_000), None, None, None, &clock).unwrap();
        clock.set(1_100);
        ledger.append(CommandMeta::new("sat-1", "cmd-1", "burn", ""), transition(CommandState::Checked, 1_100), None, None, None, &clock).unwrap();
        clock.set(1_200);
        ledger.append(CommandMeta::new("sat-1", "cmd-2", "burn", ""), transition(CommandState::Proposed, 1_200), None, None, None, &clock).unwrap();
        clock.set(1_300);
        ledger.append(CommandMeta::new("sat-1", "cmd-3", "mode", ""), transition(CommandState::Proposed, 1_300), None, None, None, &clock).unwrap();

        // A "burn" submission long before the window opens -- must not be counted. `append`
        // takes its record's own `tai_ns` from the clock's current value, not from the
        // `CommandTransition.tai_ns` the `transition()` helper embeds -- so the clock must be
        // set back explicitly, not just given a transition struct that says "0".
        clock.set(0);
        ledger.append(CommandMeta::new("sat-1", "cmd-0", "burn", ""), transition(CommandState::Proposed, 0), None, None, None, &clock).unwrap();
        clock.set(1_300);

        // A different partition's submission -- must not leak into sat-1's count.
        ledger.append(CommandMeta::new("sat-2", "cmd-9", "burn", ""), transition(CommandState::Proposed, 1_200), None, None, None, &clock).unwrap();

        let counts = ledger.count_proposed_by_class_in_window("sat-1", 1_500, 1_000).unwrap();
        assert_eq!(counts.get("burn").copied(), Some(2), "{counts:?}");
        assert_eq!(counts.get("mode").copied(), Some(1), "{counts:?}");
        assert_eq!(counts.len(), 2, "CHECKED is not counted, and the out-of-window/other-partition records are not counted: {counts:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// [`Ledger::scan_dispatched_idempotency_keys`] is what
    /// `crate::service::CommandAuthorityServiceImpl::new` calls to rebuild its duplicate-
    /// dispatch guard from the durable ledger, across a restart -- this is the acceptance
    /// property for that fix, exercised here at the ledger layer directly (through the real
    /// `append` path, a `TestClock`, no sleeping): only `COMMAND_STATE_DISPATCHED` records
    /// with a **non-empty** key are collected, across **every** partition, and a key
    /// appearing on more than one `DISPATCHED` record collapses to one set member.
    #[test]
    fn scan_dispatched_idempotency_keys_collects_only_non_empty_keys_from_dispatched_records_across_every_partition() {
        let dir = tmp_dir("scan-idempotency");
        let ledger = Ledger::open(&dir).unwrap();
        let clock = TestClock::new(0);

        // sat-1: PROPOSED then DISPATCHED with a real key -- collected.
        ledger.append(CommandMeta::new("sat-1", "cmd-1", "burn", "idem-a"), transition(CommandState::Proposed, 0), None, None, None, &clock).unwrap();
        ledger.append(CommandMeta::new("sat-1", "cmd-1", "burn", "idem-a"), transition(CommandState::Dispatched, 1), None, None, None, &clock).unwrap();

        // sat-1: a second command, also PROPOSED, but never DISPATCHED -- its key must not
        // appear even though it carries one (only DISPATCHED records count).
        ledger.append(CommandMeta::new("sat-1", "cmd-2", "burn", "idem-b"), transition(CommandState::Proposed, 2), None, None, None, &clock).unwrap();

        // sat-2 (a different partition): DISPATCHED with an empty key -- must not be
        // collected (empty means "opts out of deduplication", crate::service's own reading).
        ledger.append(CommandMeta::new("sat-2", "cmd-3", "mode", ""), transition(CommandState::Dispatched, 3), None, None, None, &clock).unwrap();

        // sat-2: a second DISPATCHED record reusing "idem-a" -- proves cross-partition
        // collection (idempotency keys are a service-wide, not per-entity, guarantee) and
        // that a repeated key collapses to one set member.
        ledger.append(CommandMeta::new("sat-2", "cmd-4", "mode", "idem-a"), transition(CommandState::Dispatched, 4), None, None, None, &clock).unwrap();

        let keys = ledger.scan_dispatched_idempotency_keys().unwrap();
        assert_eq!(keys, BTreeSet::from(["idem-a".to_string()]), "{keys:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **The self-evidence property question 203(a) step 2 asks for.** `LedgerRecord.command`
    /// (field 11) is the `Command` exactly as the caller already produced it by driving the
    /// state machine -- so its own `transitions` list is populated (never cleared), and its
    /// *last* transition must be identical to the record's own `transition` field, and its own
    /// `state` must be the transition's `state`. This is asserted directly (not merely assumed
    /// from how `append` is implemented), over a real appended-then-decoded record, so a future
    /// change that let the two drift apart would fail here first.
    #[test]
    fn the_attached_command_transitions_and_state_agree_with_the_records_own_transition() {
        let dir = tmp_dir("command-self-evidence");
        let ledger = Ledger::open(&dir).unwrap();
        let clock = TestClock::new(1_000);

        let t = transition(CommandState::Checked, 1_000);
        let command = Command {
            id: "cmd-1".to_string(),
            entity_id: "sat-1".to_string(),
            command_class: "burn".to_string(),
            state: CommandState::Checked as i32,
            transitions: vec![transition(CommandState::Proposed, 500), t.clone()],
            ..Command::default()
        };

        let record = ledger.append(CommandMeta::new("sat-1", "cmd-1", "burn", ""), t.clone(), None, Some(&command), None, &clock).unwrap();

        let attached = record.command.expect("this append call attached a command");
        assert_eq!(attached.state, record.transition.as_ref().unwrap().state, "Command.state must equal the record's own transition state");
        assert_eq!(
            attached.transitions.last().cloned(),
            record.transition.clone(),
            "the attached Command's last transition must be identical to the record's own transition -- a disagreement here is exactly the trap this field's own doc comment warns against"
        );
        assert_eq!(attached.transitions.len(), 2, "the full transition history is carried, not cleared");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **The acceptance property for [`Ledger::scan_commands`]** (question 203(a)'s fix):
    /// rebuilds each `command_id`'s *latest* `Command` snapshot from the ledger alone, across
    /// every partition, taking the last-written snapshot per id (a later transition's `Command`
    /// supersedes an earlier one) and skipping a record with no attached `Command` (only
    /// possible here because this test's own `cmd-none` append deliberately passes `None`,
    /// simulating a record written before this field existed) without erasing what an earlier
    /// record already established.
    #[test]
    fn scan_commands_rebuilds_the_latest_snapshot_per_command_id_across_every_partition() {
        let dir = tmp_dir("scan-commands");
        let ledger = Ledger::open(&dir).unwrap();
        let clock = TestClock::new(0);

        // Every attached Command here is the POST-transition value, because that is the
        // invariant `Ledger::append` itself now checks (`LedgerRecord.command`'s doc comment,
        // `authority.proto` field 11): its `state` is the transition's state and its
        // `transitions` end with that exact transition.
        let t_proposed = transition(CommandState::Proposed, 0);
        let proposed = Command {
            id: "cmd-1".to_string(),
            entity_id: "sat-1".to_string(),
            command_class: "burn".to_string(),
            state: CommandState::Proposed as i32,
            transitions: vec![t_proposed.clone()],
            ..Command::default()
        };
        ledger.append(CommandMeta::new("sat-1", "cmd-1", "burn", ""), t_proposed, None, Some(&proposed), None, &clock).unwrap();
        let t_checked = transition(CommandState::Checked, 1);
        let checked = Command {
            state: CommandState::Checked as i32,
            transitions: vec![proposed.transitions[0].clone(), t_checked.clone()],
            ..proposed.clone()
        };
        ledger.append(CommandMeta::new("sat-1", "cmd-1", "burn", ""), t_checked, None, Some(&checked), None, &clock).unwrap();

        // A different partition, a different command_id -- must appear in the rebuilt map too.
        let t_other = transition(CommandState::Proposed, 2);
        let other = Command {
            id: "cmd-2".to_string(),
            entity_id: "sat-2".to_string(),
            command_class: "mode".to_string(),
            state: CommandState::Proposed as i32,
            transitions: vec![t_other.clone()],
            ..Command::default()
        };
        ledger.append(CommandMeta::new("sat-2", "cmd-2", "mode", ""), t_other, None, Some(&other), None, &clock).unwrap();

        // A record with no attached Command at all -- must not appear, and must not be able to
        // erase a real snapshot (there is none for "cmd-none" to erase here, but this proves
        // scan_commands does not panic or insert a default Command for it).
        ledger.append(CommandMeta::new("sat-1", "cmd-none", "burn", ""), transition(CommandState::Proposed, 3), None, None, None, &clock).unwrap();

        let commands = ledger.scan_commands().unwrap();
        assert_eq!(commands.len(), 2, "{commands:?}");
        assert_eq!(commands.get("cmd-1").unwrap().state, CommandState::Checked as i32, "the latest snapshot wins, not the first");
        assert_eq!(commands.get("cmd-2").unwrap().state, CommandState::Proposed as i32);
        assert!(!commands.contains_key("cmd-none"), "a record with no attached Command must not appear in the rebuilt map");

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn proposal_for(command: &Command, rationale: &str, evidence_ids: &[&str]) -> CommandProposal {
        CommandProposal { command: Some(command.clone()), rationale: rationale.to_string(), evidence_ids: evidence_ids.iter().map(|s| s.to_string()).collect() }
    }

    /// **R3.5a's own acceptance property for [`Ledger::scan_proposals`]**: the rationale and
    /// evidence ids attached to a `PROPOSED` record survive a rebuild from disk alone, across
    /// every partition -- and a command that only ever reached `PROPOSED` still has its
    /// proposal on the map (this scan does not require a later transition to exist).
    #[test]
    fn scan_proposals_rebuilds_the_rationale_and_evidence_ids_from_the_proposed_record() {
        let dir = tmp_dir("scan-proposals");
        let ledger = Ledger::open(&dir).unwrap();
        let clock = TestClock::new(0);

        let t_proposed = transition(CommandState::Proposed, 0);
        let proposed = Command { id: "cmd-1".to_string(), entity_id: "sat-1".to_string(), command_class: "burn".to_string(), state: CommandState::Proposed as i32, transitions: vec![t_proposed.clone()], ..Command::default() };
        let proposal = proposal_for(&proposed, "scored radius drifted past threshold", &["run-1/query-1", "run-1/query-2"]);
        ledger.append(CommandMeta::new("sat-1", "cmd-1", "burn", ""), t_proposed, None, Some(&proposed), Some(proposal.clone()), &clock).unwrap();

        // Advances past PROPOSED -- Ledger::append's own invariant means no second `proposal`
        // is ever attached here (production callers never try); this proves scan_proposals
        // still finds the first (and only) one, unerased by a later transition of the same
        // command_id.
        let t_checked = transition(CommandState::Checked, 1);
        let checked = Command { state: CommandState::Checked as i32, transitions: vec![proposed.transitions[0].clone(), t_checked.clone()], ..proposed.clone() };
        ledger.append(CommandMeta::new("sat-1", "cmd-1", "burn", ""), t_checked, None, Some(&checked), None, &clock).unwrap();

        // A different partition, a different command_id, no proposal attached at all -- must
        // simply be absent from the rebuilt map, never a default/empty entry.
        let t_other = transition(CommandState::Proposed, 2);
        let other = Command { id: "cmd-2".to_string(), entity_id: "sat-2".to_string(), command_class: "mode".to_string(), state: CommandState::Proposed as i32, transitions: vec![t_other.clone()], ..Command::default() };
        ledger.append(CommandMeta::new("sat-2", "cmd-2", "mode", ""), t_other, None, Some(&other), None, &clock).unwrap();

        let proposals = ledger.scan_proposals().unwrap();
        assert_eq!(proposals.len(), 1, "{proposals:?}");
        let recovered = proposals.get("cmd-1").expect("cmd-1's proposal must be recovered");
        assert_eq!(recovered.rationale, "scored radius drifted past threshold");
        assert_eq!(recovered.evidence_ids, vec!["run-1/query-1".to_string(), "run-1/query-2".to_string()]);
        assert!(!proposals.contains_key("cmd-2"), "a command with no attached proposal must not appear");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A second [`Ledger`] handle over the SAME directory -- i.e. a process restart -- gets
    /// the rationale back exactly like [`Self::scan_commands`]'s own restart proof
    /// (`ledger_survives_reopen_and_continues_the_same_chain` above), the acceptance property
    /// the brief asks for directly ("a test builds a second service over the same directory
    /// and gets the rationale back").
    #[test]
    fn scan_proposals_survives_a_reopen_of_the_same_ledger_directory() {
        let dir = tmp_dir("scan-proposals-reopen");
        {
            let ledger = Ledger::open(&dir).unwrap();
            let clock = TestClock::new(0);
            let t = transition(CommandState::Proposed, 0);
            let command = Command { id: "cmd-1".to_string(), entity_id: "sat-1".to_string(), command_class: "mode".to_string(), state: CommandState::Proposed as i32, transitions: vec![t.clone()], ..Command::default() };
            let proposal = proposal_for(&command, "restart-survival rationale", &["evidence-a"]);
            ledger.append(CommandMeta::new("sat-1", "cmd-1", "mode", ""), t, None, Some(&command), Some(proposal), &clock).unwrap();
        }
        let reopened = Ledger::open(&dir).unwrap();
        let proposals = reopened.scan_proposals().unwrap();
        assert_eq!(proposals.get("cmd-1").expect("survives reopen").rationale, "restart-survival rationale");
        assert_eq!(proposals.get("cmd-1").unwrap().evidence_ids, vec!["evidence-a".to_string()]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **The invariant `Ledger::append` enforces on `proposal`** (R3.5a, mirroring
    /// `LedgerRecord.command`'s identical invariant): a `proposal` whose own `command.id`
    /// disagrees with the record's `command_id` is caught here, not trusted.
    ///
    /// Asserted as a returned `Err`, not a panic (the manager's R3.5a review): both invariants
    /// were `debug_assert_eq!`, which a release build removes entirely, so the guarantee held
    /// only under `cargo test`. Asserting an `Err` is what makes this test prove something
    /// about the SHIPPED binary as well as this one -- and it also asserts the record was not
    /// written, which a panicking check could never establish.
    #[test]
    fn append_refuses_a_proposal_whose_command_id_disagrees_with_the_records_own_command_id() {
        let dir = tmp_dir("proposal-invariant-command-id");
        let ledger = Ledger::open(&dir).unwrap();
        let clock = TestClock::new(0);
        let t = transition(CommandState::Proposed, 0);
        let command = Command { id: "cmd-1".to_string(), entity_id: "sat-1".to_string(), command_class: "mode".to_string(), state: CommandState::Proposed as i32, transitions: vec![t.clone()], ..Command::default() };
        // The proposal's own nested command.id ("cmd-WRONG") disagrees with meta.command_id
        // ("cmd-1") below.
        let mismatched = Command { id: "cmd-WRONG".to_string(), ..command.clone() };
        let proposal = proposal_for(&mismatched, "reason", &[]);
        let err = ledger
            .append(CommandMeta::new("sat-1", "cmd-1", "mode", ""), t, None, Some(&command), Some(proposal), &clock)
            .expect_err("a mismatched proposal must be refused");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "{err}");
        assert!(err.to_string().contains("disagrees with the record's command_id"), "{err}");
        assert!(ledger.scan_commands().unwrap().is_empty(), "a refused append must write no record at all");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A `proposal` attached to a non-`PROPOSED` transition is refused the same way, and in
    /// every build profile -- see the previous test's own doc for why that distinction matters.
    #[test]
    fn append_refuses_a_proposal_attached_to_a_non_proposed_record() {
        let dir = tmp_dir("proposal-invariant-state");
        let ledger = Ledger::open(&dir).unwrap();
        let clock = TestClock::new(0);
        let t = transition(CommandState::Checked, 0); // not PROPOSED
        let command = Command { id: "cmd-1".to_string(), entity_id: "sat-1".to_string(), command_class: "mode".to_string(), state: CommandState::Checked as i32, transitions: vec![t.clone()], ..Command::default() };
        let proposal = proposal_for(&command, "reason", &[]);
        let err = ledger
            .append(CommandMeta::new("sat-1", "cmd-1", "mode", ""), t, None, Some(&command), Some(proposal), &clock)
            .expect_err("a proposal on a non-PROPOSED record must be refused");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "{err}");
        assert!(err.to_string().contains("is set on the PROPOSED record and only there"), "{err}");
        assert!(ledger.scan_commands().unwrap().is_empty(), "a refused append must write no record at all");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **The acceptance property for [`Ledger::scan_decisions`]** (R3.5a): the `PolicyDecision`
    /// attached to a `CHECKED`/`REJECTED` record survives a rebuild from disk alone, keyed by
    /// `command_id`, for both outcomes -- a denial is as reproducible from the ledger as an
    /// approval (mirroring `LedgerRecord.decision`'s own doc comment).
    #[test]
    fn scan_decisions_rebuilds_the_decision_for_both_checked_and_rejected_records() {
        let dir = tmp_dir("scan-decisions");
        let ledger = Ledger::open(&dir).unwrap();
        let clock = TestClock::new(0);

        let allow_decision = PolicyDecision { decision_id: "decision-allow".to_string(), allow: true, policy_hash: "hash-1".to_string(), reasons: vec!["admitted".to_string()], matched_rule_path: "data.altavista.authority.allow".to_string(), evaluated_tai_ns: 0, input: None };
        let t_checked = transition(CommandState::Checked, 0);
        ledger.append(CommandMeta::new("sat-1", "cmd-1", "mode", ""), t_checked, Some(allow_decision.clone()), None, None, &clock).unwrap();

        let deny_decision = PolicyDecision { decision_id: "decision-deny".to_string(), allow: false, policy_hash: "hash-1".to_string(), reasons: vec!["command_class payload is not admitted by policy".to_string()], matched_rule_path: "data.altavista.authority.deny".to_string(), evaluated_tai_ns: 1, input: None };
        let t_rejected = transition(CommandState::Rejected, 1);
        ledger.append(CommandMeta::new("sat-1", "cmd-2", "payload", ""), t_rejected, Some(deny_decision.clone()), None, None, &clock).unwrap();

        // A PROPOSED record with no decision at all -- must not appear.
        ledger.append(CommandMeta::new("sat-1", "cmd-3", "mode", ""), transition(CommandState::Proposed, 2), None, None, None, &clock).unwrap();

        let decisions = ledger.scan_decisions().unwrap();
        assert_eq!(decisions.len(), 2, "{decisions:?}");
        assert_eq!(decisions.get("cmd-1").unwrap().decision_id, "decision-allow");
        assert!(decisions.get("cmd-1").unwrap().allow);
        assert_eq!(decisions.get("cmd-2").unwrap().decision_id, "decision-deny");
        assert!(!decisions.get("cmd-2").unwrap().allow);
        assert!(!decisions.contains_key("cmd-3"), "a command with no policy decision must not appear");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
