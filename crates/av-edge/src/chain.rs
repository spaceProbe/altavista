//! The per-producer chain verifier ([`ChainVerifier`]) and the pure chain walker
//! ([`walk_chain`]) -- the two ways this crate checks a producer's hash chain, for two
//! different callers.
//!
//! [`ChainVerifier`] is what an ingest boundary (E3) runs live, one incoming batch at a
//! time, against mutable per-producer state it owns; [`walk_chain`] is what a replay or
//! an audit tool runs after the fact, over an already-assembled slice of one producer's
//! batches, with no state of its own at all (mirroring
//! `crates/av-dynamics-service/src/evidence.rs::EvidenceLog::verify`'s relationship to
//! `EvidenceLog::record` -- one appends and evolves state, the other independently
//! re-derives the same verdict by walking the data from scratch).
//!
//! # `ChainVerifier`'s check order
//!
//! [`ChainVerifier::submit`] checks each incoming batch in this fixed order, stopping at
//! the first defect (ADR-004: exactly one rejection reason per rejected batch):
//!
//! 1. **UNSIGNED** -- `signature` is empty. Cheapest possible check, and every later
//!    check either needs a valid signature to trust the batch's content, or is moot
//!    without one.
//! 2. **BAD_SIGNATURE** -- the signature does not verify, or `batch_hash` does not equal
//!    the hash recomputed from the batch's own content. Checked before anything that
//!    reads the batch's *content* (sequence, `prev_hash`, label, epoch), because an
//!    unsigned-or-forged batch's content cannot be trusted to mean anything yet -- a
//!    batch that fails this check might report a perfectly plausible sequence number or
//!    label while actually being forged, so nothing past this point should be allowed to
//!    "win" over it.
//! 3. **CHAIN_GAP** / **CHAIN_BREAK** / **DUPLICATE** -- the three ways `sequence` and
//!    `prev_hash` can disagree with this producer's chain state. These three are
//!    *mutually exclusive by construction*, not merely by convention: given the last
//!    accepted sequence `last`, exactly one of `sequence > last + 1` (GAP),
//!    `sequence == last + 1` (checked for a BREAK via `prev_hash`), or `sequence <=
//!    last` (checked for a DUPLICATE, since accepted sequences form a contiguous run
//!    from a producer's first accepted sequence through `last`) can hold for any given
//!    `sequence`, so there is no real ordering question *among* these three -- they are
//!    one conceptual step, checked together.
//! 4. **MISLABELED** / **OVER_CLEARANCE** -- [`crate::policy::ProducerPolicy::
//!    classify_label`], which is likewise structured so at most one of these two can ever
//!    apply to a given label (see that function's own doc comment).
//! 5. **STALE** -- checked last: a batch's age is the one property that does not bear on
//!    whether the batch is genuine, in order, or correctly labelled, so every other
//!    defect is diagnosed first even though staleness is often the cheapest possible
//!    check. This ordering is deliberately opposite to "cheapest first" for exactly this
//!    reason -- a batch that is both forged *and* old should be reported as forged, not
//!    as merely late.
//!
//! `tests/rejections.rs`'s `check_order_*` tests construct a batch with two simultaneous
//! defects and assert the earlier one in this list wins.

use std::collections::{HashMap, HashSet};

use openssl::ec::EcKeyRef;
use openssl::pkey::Public;

use crate::hash;
use crate::pb;
use crate::policy::ProducerPolicy;
use crate::verify::{self, VerifyError};

/// Per-producer state [`ChainVerifier`] owns: the last accepted sequence and its
/// `batch_hash` (folded into `counters.chain_head` -- see that field's own doc comment in
/// `edge.proto`, which is exactly what this mirrors), every sequence ever accepted (for
/// [`pb::BatchRejection::Duplicate`] detection), and this producer's running
/// [`pb::RejectionCounters`].
#[derive(Debug, Clone)]
struct ProducerState {
    last_accepted_sequence: Option<u64>,
    seen_sequences: HashSet<u64>,
    counters: pb::RejectionCounters,
}

impl ProducerState {
    fn new(producer_id: &str) -> Self {
        Self {
            last_accepted_sequence: None,
            seen_sequences: HashSet::new(),
            counters: pb::RejectionCounters { producer_id: producer_id.to_string(), chain_head: hash::GENESIS.to_vec(), ..Default::default() },
        }
    }
}

/// Pure, per-producer chain state plus the one live-ingest check `crate::policy` does not
/// itself perform (chain linkage). Holds no clock, no keys, no I/O -- every batch it
/// checks, every policy it checks against, and every "now" it checks staleness against
/// are all supplied by the caller on each call to [`ChainVerifier::submit`], so this type
/// is exactly as testable as any other pure value in this crate (question 199: there is
/// no clock read anywhere in this struct).
#[derive(Debug, Default)]
pub struct ChainVerifier {
    state: HashMap<String, ProducerState>,
}

fn bump_counter(counters: &mut pb::RejectionCounters, rejection: pb::BatchRejection) {
    match rejection {
        // Never reached from `submit` (which only ever passes a concrete rejection kind
        // here), kept so this match stays exhaustive if `BatchRejection` grows a variant.
        pb::BatchRejection::Unspecified => {}
        pb::BatchRejection::Unsigned => counters.unsigned_count += 1,
        pb::BatchRejection::BadSignature => counters.bad_signature_count += 1,
        pb::BatchRejection::ChainGap => counters.chain_gap_count += 1,
        pb::BatchRejection::ChainBreak => counters.chain_break_count += 1,
        pb::BatchRejection::Mislabeled => counters.mislabeled_count += 1,
        pb::BatchRejection::OverClearance => counters.over_clearance_count += 1,
        pb::BatchRejection::Stale => counters.stale_count += 1,
        pb::BatchRejection::Duplicate => counters.duplicate_count += 1,
    }
}

impl ChainVerifier {
    pub fn new() -> Self {
        Self::default()
    }

    /// This producer's running counters, or `None` if [`ChainVerifier::submit`] has never
    /// been called for it. A fresh producer's counters (once it exists) start at all
    /// zeros with `chain_head` equal to [`hash::GENESIS`], per `edge.proto`'s
    /// `RejectionCounters.chain_head` doc comment.
    pub fn counters(&self, producer_id: &str) -> Option<&pb::RejectionCounters> {
        self.state.get(producer_id).map(|s| &s.counters)
    }

    /// Checks one incoming `batch` against `policy` and this producer's chain state,
    /// using `now_tai_ns` (supplied by the caller -- never read from any clock here) for
    /// the staleness check, and `verify_key` to check its signature. Updates this
    /// producer's [`pb::RejectionCounters`] by exactly one count either way, and returns
    /// the resulting [`pb::BatchVerdict`]. See the module doc for the fixed check order.
    pub fn submit(&mut self, batch: &pb::MeasurementBatch, policy: &ProducerPolicy, verify_key: &EcKeyRef<Public>, now_tai_ns: i64) -> pb::BatchVerdict {
        let state = self.state.entry(batch.producer_id.clone()).or_insert_with(|| ProducerState::new(&batch.producer_id));

        let rejection_and_detail: Option<(pb::BatchRejection, String)> = 'check: {
            // 1. UNSIGNED
            if batch.signature.is_empty() {
                break 'check Some((pb::BatchRejection::Unsigned, "signature field is empty".to_string()));
            }
            // 2. BAD_SIGNATURE (signature invalid, or batch_hash disagrees with the
            // batch's own recomputed content hash).
            match verify::verify_batch(batch, verify_key) {
                Ok(_) => {}
                Err(VerifyError::Unsigned) => {
                    // verify_batch's own empty-signature check; unreachable given the
                    // check above, kept only so this match need not special-case it away.
                    break 'check Some((pb::BatchRejection::Unsigned, "signature field is empty".to_string()));
                }
                Err(e) => break 'check Some((pb::BatchRejection::BadSignature, e.to_string())),
            }
            // 3. CHAIN_GAP / CHAIN_BREAK / DUPLICATE.
            if let Some(last) = state.last_accepted_sequence {
                if state.seen_sequences.contains(&batch.sequence) {
                    break 'check Some((pb::BatchRejection::Duplicate, format!("sequence {} was already accepted for producer {:?}", batch.sequence, batch.producer_id)));
                }
                if batch.sequence > last + 1 {
                    break 'check Some((
                        pb::BatchRejection::ChainGap,
                        format!("expected sequence {}, found {} -- one or more batches between them were never seen", last + 1, batch.sequence),
                    ));
                }
                if batch.sequence < last + 1 {
                    // Accepted sequences form a contiguous run from this producer's first
                    // accepted sequence through `last` (each accepted batch is checked
                    // against exactly `last + 1`), so a sequence below that range that is
                    // not itself a recorded duplicate cannot arise from this verifier's
                    // own bookkeeping -- classified as a gap defensively rather than left
                    // to panic on an input this verifier did not itself produce.
                    break 'check Some((pb::BatchRejection::ChainGap, format!("sequence {} does not extend or replay this producer's chain (last accepted: {last})", batch.sequence)));
                }
                if batch.prev_hash != state.counters.chain_head {
                    break 'check Some((pb::BatchRejection::ChainBreak, format!("sequence {}: prev_hash does not match the last accepted batch's own hash", batch.sequence)));
                }
            } else if batch.prev_hash != state.counters.chain_head {
                // state.counters.chain_head is GENESIS here (ProducerState::new).
                break 'check Some((pb::BatchRejection::ChainBreak, format!("producer {:?}'s first batch must chain from \"GENESIS\"", batch.producer_id)));
            }
            // 4. MISLABELED / OVER_CLEARANCE.
            let label = batch.label.clone().unwrap_or_default();
            if let Some(rejection) = policy.classify_label(&label) {
                break 'check Some((rejection, format!("label marking={:?} caveats={:?} rejected as {rejection:?}", label.marking, label.caveats)));
            }
            // 5. STALE.
            if policy.is_stale(batch.batch_tai_ns, now_tai_ns) {
                break 'check Some((
                    pb::BatchRejection::Stale,
                    format!("batch_tai_ns={} is older than max_age_ns={} relative to now_tai_ns={now_tai_ns}", batch.batch_tai_ns, policy.max_age_ns),
                ));
            }
            None
        };

        let batch_hash = batch.batch_hash.clone();
        match rejection_and_detail {
            Some((rejection, detail)) => {
                bump_counter(&mut state.counters, rejection);
                pb::BatchVerdict { accepted: false, rejection: rejection as i32, producer_id: batch.producer_id.clone(), sequence: batch.sequence, batch_hash, detail }
            }
            None => {
                state.last_accepted_sequence = Some(batch.sequence);
                state.seen_sequences.insert(batch.sequence);
                state.counters.accepted += 1;
                state.counters.chain_head = batch.batch_hash.clone();
                pb::BatchVerdict {
                    accepted: true,
                    rejection: pb::BatchRejection::Unspecified as i32,
                    producer_id: batch.producer_id.clone(),
                    sequence: batch.sequence,
                    batch_hash,
                    detail: "accepted".to_string(),
                }
            }
        }
    }
}

/// A pure, stateless walk of `batches` (already-assembled, in order, for one producer):
/// checks each batch's `prev_hash` against the previous batch's own recomputed hash (or
/// [`hash::GENESIS`] for the first), then that batch's own `batch_hash`/`signature`
/// self-consistency via [`verify::verify_batch`], and returns the first defect found --
/// mirroring `av-dynamics-service/src/evidence.rs::EvidenceLog::verify`'s independent,
/// from-scratch re-derivation of a chain's validity, and `altavista.v1.ChainVerification`'s
/// own field shape (`ok`/`checked`/`broken_at_sequence`/`detail`), reused directly here
/// rather than a second, crate-local struct with the same fields.
///
/// Unlike [`ChainVerifier`], this performs no label or staleness check and updates no
/// counters -- it answers exactly one question, "is this chain's hashing and signing
/// intact", which is what a replay or an audit tool needs after the fact, independent of
/// whatever counters an ingest boundary already accumulated while first receiving these
/// batches live.
pub fn walk_chain(producer_id: &str, batches: &[pb::MeasurementBatch], verify_key: &EcKeyRef<Public>) -> pb::ChainVerification {
    let mut expected_prev: Vec<u8> = hash::GENESIS.to_vec();
    let mut checked: u64 = 0;

    for batch in batches {
        if batch.prev_hash != expected_prev {
            return pb::ChainVerification {
                producer_id: producer_id.to_string(),
                ok: false,
                checked,
                broken_at_sequence: batch.sequence,
                detail: format!("sequence {}: prev_hash does not match the previous batch's own hash", batch.sequence),
            };
        }
        match verify::verify_batch(batch, verify_key) {
            Ok(recomputed) => {
                expected_prev = recomputed.to_vec();
                checked += 1;
            }
            Err(VerifyError::Unsigned) => {
                return pb::ChainVerification { producer_id: producer_id.to_string(), ok: false, checked, broken_at_sequence: batch.sequence, detail: format!("sequence {}: unsigned", batch.sequence) };
            }
            Err(VerifyError::HashMismatch) => {
                return pb::ChainVerification {
                    producer_id: producer_id.to_string(),
                    ok: false,
                    checked,
                    broken_at_sequence: batch.sequence,
                    detail: format!("sequence {}: batch_hash does not match its recomputed content hash -- the batch was tampered with after being signed", batch.sequence),
                };
            }
            Err(e) => {
                return pb::ChainVerification { producer_id: producer_id.to_string(), ok: false, checked, broken_at_sequence: batch.sequence, detail: format!("sequence {}: {e}", batch.sequence) };
            }
        }
    }
    pb::ChainVerification { producer_id: producer_id.to_string(), ok: true, checked, broken_at_sequence: 0, detail: "chain intact".to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sign;

    const TEST_KEY_PEM: &[u8] = include_bytes!("../tests/fixtures/test_signing_key.pem");
    const TEST_PUB_PEM: &[u8] = include_bytes!("../tests/fixtures/test_signing_key.pub.pem");

    fn policy() -> ProducerPolicy {
        ProducerPolicy::new("p1", "UNCLASSIFIED", vec![], vec!["UNCLASSIFIED".to_string(), "CUI".to_string()], "CUI", 10_000_000_000).unwrap()
    }

    fn signed_batch(sequence: u64, prev_hash: &[u8], signing_key: &openssl::ec::EcKey<openssl::pkey::Private>) -> pb::MeasurementBatch {
        let mut batch = pb::MeasurementBatch {
            producer_id: "p1".to_string(),
            sequence,
            label: Some(pb::Label { marking: "UNCLASSIFIED".to_string(), caveats: vec![] }),
            batch_tai_ns: 1_000,
            ..Default::default()
        };
        sign::sign_batch(&mut batch, prev_hash, signing_key).unwrap();
        batch
    }

    #[test]
    fn submit_accepts_a_valid_chain_of_three_batches() {
        let signing_key = sign::load_signing_key(TEST_KEY_PEM).unwrap();
        let verify_key = crate::verify::load_verifying_key(TEST_PUB_PEM).unwrap();
        let mut verifier = ChainVerifier::new();
        let policy = policy();

        let b1 = signed_batch(1, hash::GENESIS, &signing_key);
        let v1 = verifier.submit(&b1, &policy, &verify_key, 1_000);
        assert!(v1.accepted, "{v1:?}");

        let b2 = signed_batch(2, &b1.batch_hash, &signing_key);
        let v2 = verifier.submit(&b2, &policy, &verify_key, 1_000);
        assert!(v2.accepted, "{v2:?}");

        let b3 = signed_batch(3, &b2.batch_hash, &signing_key);
        let v3 = verifier.submit(&b3, &policy, &verify_key, 1_000);
        assert!(v3.accepted, "{v3:?}");

        let counters = verifier.counters("p1").unwrap();
        assert_eq!(counters.accepted, 3);
        assert_eq!(counters.chain_head, b3.batch_hash);
    }

    #[test]
    fn walk_chain_reports_ok_for_an_intact_chain() {
        let signing_key = sign::load_signing_key(TEST_KEY_PEM).unwrap();
        let verify_key = crate::verify::load_verifying_key(TEST_PUB_PEM).unwrap();
        let b1 = signed_batch(1, hash::GENESIS, &signing_key);
        let b2 = signed_batch(2, &b1.batch_hash, &signing_key);
        let result = walk_chain("p1", &[b1, b2], &verify_key);
        assert!(result.ok, "{result:?}");
        assert_eq!(result.checked, 2);
    }

    #[test]
    fn walk_chain_locates_a_broken_prev_hash_link() {
        let signing_key = sign::load_signing_key(TEST_KEY_PEM).unwrap();
        let verify_key = crate::verify::load_verifying_key(TEST_PUB_PEM).unwrap();
        let b1 = signed_batch(1, hash::GENESIS, &signing_key);
        let mut b2 = signed_batch(2, &b1.batch_hash, &signing_key);
        b2.prev_hash = vec![0u8; 32]; // sever the link without re-signing
        let result = walk_chain("p1", &[b1, b2], &verify_key);
        assert!(!result.ok);
        assert_eq!(result.broken_at_sequence, 2);
        assert_eq!(result.checked, 1);
    }
}
