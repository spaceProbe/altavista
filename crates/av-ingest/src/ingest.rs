//! The accept/reject pipeline: wires E1's `av_edge::chain::ChainVerifier` and E2's
//! `av_edge::identity` verification to [`crate::log::PartitionLog`] appends
//! (`docs/edge-plan.md` milestone E3, "the ingest as a library" half of it -- E3b, the
//! gRPC/mTLS wire, is deliberately not built here; see this crate's `lib.rs` module doc).
//!
//! # Where `SHARD_MISMATCH` sits, and why
//!
//! [`Ingest::submit`] runs these checks in this fixed order:
//!
//! 1. **Identity** (E2, only if a certificate was presented -- see [`Signer`]):
//!    `av_edge::identity::verify_identity` against this `Ingest`'s own `TrustAnchors`. A
//!    refusal here is reported as [`IngestOutcome::IdentityRejected`], counted in this
//!    `Ingest`'s own `IdentityCounters`, and the batch is never looked at again -- nothing
//!    below this step ever runs, because there is not yet even a trusted public key to
//!    check its signature against.
//! 2. **`SHARD_MISMATCH`** (E3, new this round): `batch.shard_key` must sanitise to a
//!    valid partition name ([`crate::log::sanitize_shard_key`]) *and* agree with every one
//!    of `batch.measurements[*].shard_key`. Checked immediately after identity and
//!    strictly *before* `av_edge::chain::ChainVerifier` ever sees the batch, for two
//!    reasons. First, mechanically: which partition a batch would even land in is a
//!    property of `shard_key` alone, entirely independent of whatever
//!    `av_edge::chain::ChainVerifier`'s eight checks decide, so there is no reason to
//!    spend a signature verification, a chain-linkage check, or a label check on a batch
//!    this pipeline already knows it can never durably place anywhere. Second, and more
//!    importantly for correctness: `ChainVerifier::submit` is not a pure query -- it
//!    *mutates* this producer's chain state on both an accept and a reject (advances
//!    `last_accepted_sequence`/`chain_head` on accept; bumps a `RejectionCounters` field
//!    either way). If `SHARD_MISMATCH` were checked *after* `ChainVerifier::submit`
//!    instead of before it, a batch that `ChainVerifier` accepted (correctly signed, in
//!    order, correctly labelled, not stale) but that named an unwritable or
//!    inconsistent `shard_key` would already have advanced that producer's chain state by
//!    the time this pipeline discovered it could not actually be appended anywhere --
//!    leaving the in-memory chain one batch ahead of what the durable log (the actual
//!    ledger, ADR-004) holds, which is exactly the kind of state-vs-ledger drift this
//!    track's evidence rules exist to make impossible. Checking `SHARD_MISMATCH` first
//!    means a batch that fails it never reaches `ChainVerifier::submit` at all, so no
//!    chain state ever advances for a batch that cannot be durably placed.
//! 3. **`av_edge::chain::ChainVerifier::submit`** -- E1's own eight checks
//!    (`UNSIGNED`/`BAD_SIGNATURE`/`CHAIN_GAP`/`CHAIN_BREAK`/`MISLABELED`/
//!    `OVER_CLEARANCE`/`STALE`/`DUPLICATE`), in its own documented fixed order
//!    (`av_edge::chain`'s module doc) -- entirely unchanged by this crate.
//! 4. **Append**, only if `ChainVerifier::submit` accepted, to the partition
//!    `batch.shard_key` names (already known-valid from step 2).
//! 5. Return the verdict, having moved exactly one counter either way.
//!
//! `SHARD_MISMATCH` therefore sits *outside* `av_edge::chain`'s own nine-way match
//! entirely (that crate's `ChainVerifier` never produces it -- see `av_edge::chain::
//! bump_counter`'s own doc comment on its `ShardMismatch` arm) and is not one of the
//! `ChainVerifier`'s eight mutually-ordered checks; it is a zeroth-and-a-half gate this
//! crate runs on the batch's declared partition before `ChainVerifier` is ever invoked.

use std::collections::HashMap;
use std::path::PathBuf;

use openssl::ec::EcKey;
use openssl::pkey::Public;

use av_edge::chain::ChainVerifier;
use av_edge::identity::{self, IdentityCounters, IdentityRejection, TrustAnchors};
use av_edge::pb;
use av_edge::policy::ProducerPolicy;

use crate::log::{self, LogError, PartitionLog, RecoveryReport};

/// How [`Ingest::submit`] should establish the public key a batch's signature is checked
/// against -- see this module's doc for exactly where this fits in the check order.
pub enum Signer<'a> {
    /// No certificate in the loop (E1's own path): the caller supplies the public key
    /// directly, e.g. a pinned per-producer key from configuration.
    Key(EcKey<Public>),
    /// A seccert-issued leaf was presented on this connection (E2): verified against this
    /// `Ingest`'s own [`TrustAnchors`] before anything else runs; the certificate's own
    /// public key is what the batch's signature is then checked against.
    Certificate { leaf_pem: &'a [u8], chain_pem: Option<&'a [u8]> },
}

/// The result of one [`Ingest::submit`] call. Two disjoint outcomes because identity
/// refusals and `av_edge::pb::BatchRejection` kinds are counted in two entirely separate
/// counter sets (`IdentityCounters` vs. per-producer `RejectionCounters`) -- see this
/// module's doc and `crate::evidence`'s own module doc for how both get folded into one
/// evidence surface.
#[derive(Debug)]
pub enum IngestOutcome {
    /// Step 1 refused the presented certificate; the batch was never looked at.
    IdentityRejected(IdentityRejection),
    /// `batch.producer_id` has no registered [`ProducerPolicy`] -- refused before its
    /// `shard_key` is even inspected, since `ChainVerifier::submit` cannot run without a
    /// policy to check the label/staleness against.
    UnknownProducer(String),
    /// Steps 2-5 ran to completion: either `SHARD_MISMATCH` (constructed here, since
    /// `av_edge::chain::ChainVerifier` never produces it) or whatever
    /// `ChainVerifier::submit` itself returned.
    Verdict(pb::BatchVerdict),
}

impl IngestOutcome {
    /// `true` iff this outcome represents an accepted, appended batch.
    pub fn accepted(&self) -> bool {
        matches!(self, IngestOutcome::Verdict(v) if v.accepted)
    }
}

/// What can go wrong at the I/O layer while submitting a batch -- distinct from
/// [`IngestOutcome`], which covers every *expected* business-logic result (identity
/// refusal, unknown producer, any of the nine rejection kinds, or acceptance). This type
/// is reserved for failures `Ingest` itself cannot classify as a verdict at all.
#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    /// [`Signer::Certificate`] was used, but this `Ingest` was constructed with no
    /// [`TrustAnchors`] to verify it against.
    #[error("a certificate was presented but this Ingest has no configured TrustAnchors")]
    NoTrustAnchors,
    #[error(transparent)]
    Log(#[from] LogError),
}

/// The ingest pipeline: one directory of per-partition logs, one `av_edge::chain::
/// ChainVerifier` (shared across every producer, per that type's own design), a registry
/// of per-producer policies, optional identity trust anchors, and every counter this
/// crate itself owns (identity counters and `SHARD_MISMATCH` counts -- the other eight
/// rejection kinds live inside `ChainVerifier` itself and are read through it).
///
/// No `#[derive(Debug)]`: `av_edge::identity::TrustAnchors` (an `openssl::x509::store::
/// X509Store` wrapper) does not itself implement `Debug`, so this type implements it by
/// hand below, naming everything except `anchors` itself.
pub struct Ingest {
    log_dir: PathBuf,
    chain: ChainVerifier,
    policies: HashMap<String, ProducerPolicy>,
    anchors: Option<TrustAnchors>,
    identity_counters: IdentityCounters,
    shard_mismatch_counts: HashMap<String, u64>,
    partitions: HashMap<String, PartitionLog>,
}

impl Ingest {
    /// A fresh (or resumed -- partitions are recovered lazily, on first use, by
    /// [`PartitionLog::open`]) ingest rooted at `log_dir`. `anchors` is `None` for a
    /// deployment that never presents certificates at this layer (every [`Signer`] used
    /// with it must then be [`Signer::Key`]); [`Ingest::submit`] refuses
    /// [`Signer::Certificate`] with [`IngestError::NoTrustAnchors`] otherwise.
    pub fn new(log_dir: impl Into<PathBuf>, anchors: Option<TrustAnchors>) -> Self {
        Self {
            log_dir: log_dir.into(),
            chain: ChainVerifier::new(),
            policies: HashMap::new(),
            anchors,
            identity_counters: IdentityCounters::new(),
            shard_mismatch_counts: HashMap::new(),
            partitions: HashMap::new(),
        }
    }

    /// Registers (or replaces) `policy` for `policy.producer_id`. A producer with no
    /// registered policy is refused as [`IngestOutcome::UnknownProducer`] -- there is
    /// deliberately no implicit default policy, since accepting a batch under an
    /// unconfigured label/clearance would be silent policy drift.
    pub fn register_producer(&mut self, policy: ProducerPolicy) {
        self.policies.insert(policy.producer_id.clone(), policy);
    }

    /// Submits one batch through the full pipeline (see this module's doc for the check
    /// order). `now_tai_ns` is the caller-injected clock -- there is no live clock read
    /// anywhere in this crate (question 199), exactly like `av_edge::chain::
    /// ChainVerifier::submit` and `av_edge::identity::verify_identity`.
    pub fn submit(&mut self, batch: &pb::MeasurementBatch, signer: Signer<'_>, now_tai_ns: i64) -> Result<IngestOutcome, IngestError> {
        // Step 1: identity, only if a certificate was presented.
        let verify_key: EcKey<Public> = match signer {
            Signer::Key(k) => k,
            Signer::Certificate { leaf_pem, chain_pem } => {
                let anchors = self.anchors.as_ref().ok_or(IngestError::NoTrustAnchors)?;
                match identity::verify_identity(leaf_pem, chain_pem, anchors, now_tai_ns, &mut self.identity_counters) {
                    Ok(verified) => verified.public_key,
                    Err(e) => return Ok(IngestOutcome::IdentityRejected(e)),
                }
            }
        };

        // Step 2: SHARD_MISMATCH -- checked, and counted, before ChainVerifier ever sees
        // this batch (see this module's doc for why).
        let sanitize_result = log::sanitize_shard_key(&batch.shard_key);
        let measurement_mismatch = batch.measurements.iter().find(|m| m.shard_key != batch.shard_key);
        if sanitize_result.is_err() || measurement_mismatch.is_some() {
            *self.shard_mismatch_counts.entry(batch.producer_id.clone()).or_insert(0) += 1;
            let detail = match (&sanitize_result, measurement_mismatch) {
                (Err(e), _) => format!("batch shard_key {:?} is not usable as a partition key: {e}", batch.shard_key),
                (Ok(_), Some(m)) => format!(
                    "measurement {:?} declares shard_key {:?}, disagreeing with this batch's own declared shard_key {:?}",
                    m.measurement_id, m.shard_key, batch.shard_key
                ),
                (Ok(_), None) => unreachable!("this branch is only reached when sanitize failed or a mismatch was found"),
            };
            return Ok(IngestOutcome::Verdict(pb::BatchVerdict {
                accepted: false,
                rejection: pb::BatchRejection::ShardMismatch as i32,
                producer_id: batch.producer_id.clone(),
                sequence: batch.sequence,
                batch_hash: batch.batch_hash.clone(),
                detail,
            }));
        }
        let partition_name = batch.shard_key.clone();

        // Step 3: the producer must be registered, then E1's own ChainVerifier.
        let Some(policy) = self.policies.get(&batch.producer_id).cloned() else {
            return Ok(IngestOutcome::UnknownProducer(batch.producer_id.clone()));
        };
        let verdict = self.chain.submit(batch, &policy, &verify_key, now_tai_ns);

        // Step 4: append only accepted batches.
        if verdict.accepted {
            let log = self.partition_for(&partition_name)?;
            log.append(batch)?;
        }

        // Step 5.
        Ok(IngestOutcome::Verdict(verdict))
    }

    /// Lazily opens (recovering from disk if needed) the partition for `shard_key`,
    /// caching it for subsequent calls. `shard_key` is assumed already validated by
    /// [`Ingest::submit`]'s own step 2 -- [`PartitionLog::open`] re-validates it anyway
    /// (defence in depth), so a caller that reaches this with a bad key still gets a
    /// typed [`LogError`], never a write outside the log directory.
    fn partition_for(&mut self, shard_key: &str) -> Result<&PartitionLog, IngestError> {
        if !self.partitions.contains_key(shard_key) {
            let (log, _report) = PartitionLog::open(&self.log_dir, shard_key)?;
            self.partitions.insert(shard_key.to_string(), log);
        }
        Ok(self.partitions.get(shard_key).expect("just inserted or already present"))
    }

    /// Opens (recovering if needed) the partition for `shard_key` and returns any
    /// [`RecoveryReport`] that recovery produced -- the one entry point a caller (or this
    /// crate's own crash-recovery tests) uses to *observe* a recovery, since
    /// [`Ingest::submit`]'s own lazy [`Ingest::partition_for`] discards that report after
    /// folding it into the cached partition.
    pub fn open_partition(&mut self, shard_key: &str) -> Result<Option<RecoveryReport>, IngestError> {
        if self.partitions.contains_key(shard_key) {
            return Ok(None); // already open in this process; nothing to recover from here.
        }
        let (log, report) = PartitionLog::open(&self.log_dir, shard_key)?;
        self.partitions.insert(shard_key.to_string(), log);
        Ok(report)
    }

    /// Every producer this `Ingest` has a registered policy for.
    pub fn producer_ids(&self) -> impl Iterator<Item = &String> {
        self.policies.keys()
    }

    /// This producer's `RejectionCounters` (via `av_edge::chain::ChainVerifier::
    /// counters`), or the all-zero, `GENESIS`-headed default a registered-but-never-
    /// submitted producer has -- matching `ChainVerifier`'s own "a fresh producer's
    /// counters start at all zeros" contract, extended to a producer this `Ingest` knows
    /// about but `ChainVerifier` has not seen yet.
    pub fn producer_counters(&self, producer_id: &str) -> pb::RejectionCounters {
        self.chain.counters(producer_id).cloned().unwrap_or_else(|| pb::RejectionCounters {
            producer_id: producer_id.to_string(),
            chain_head: av_edge::hash::GENESIS.to_vec(),
            ..Default::default()
        })
    }

    /// This producer's `SHARD_MISMATCH` count (kept outside `av_edge::pb::
    /// RejectionCounters` -- see this module's own doc and `crate::log`'s doc for why).
    pub fn shard_mismatch_count(&self, producer_id: &str) -> u64 {
        self.shard_mismatch_counts.get(producer_id).copied().unwrap_or(0)
    }

    /// The aggregate identity counters (`av_edge::identity::IdentityCounters`) -- not
    /// per-producer, matching that type's own flat-aggregate design.
    pub fn identity_counters(&self) -> IdentityCounters {
        self.identity_counters
    }

    /// Every partition this `Ingest` currently has open, keyed by `shard_key`.
    pub fn partitions(&self) -> impl Iterator<Item = (&String, &PartitionLog)> {
        self.partitions.iter()
    }

    pub fn log_dir(&self) -> &std::path::Path {
        &self.log_dir
    }
}

impl std::fmt::Debug for Ingest {
    /// Hand-written because `av_edge::identity::TrustAnchors` does not implement `Debug`
    /// -- names every field except `anchors` itself (reported only as present/absent).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ingest")
            .field("log_dir", &self.log_dir)
            .field("policies", &self.policies.keys().collect::<Vec<_>>())
            .field("anchors_configured", &self.anchors.is_some())
            .field("identity_counters", &self.identity_counters)
            .field("shard_mismatch_counts", &self.shard_mismatch_counts)
            .field("partitions", &self.partitions.keys().collect::<Vec<_>>())
            .finish()
    }
}
