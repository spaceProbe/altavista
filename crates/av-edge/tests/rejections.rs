//! Requirement 3: every one of the eight `BatchRejection` kinds, each produced by
//! exactly one deliberate corruption of an otherwise-valid batch, asserting both the
//! verdict's rejection kind and that only that kind's counter incremented.
//!
//! Requirement 6: the documented check order (`av_edge::chain`'s module doc) -- a batch
//! carrying two defects at once yields the earlier one in that order.
//!
//! Every test here starts from [`valid_chain_of_two`] (a producer's first batch, already
//! accepted, so a *second* batch can be built and corrupted against real chain state)
//! and changes exactly one thing about the second batch before submitting it.

use av_edge::chain::ChainVerifier;
use av_edge::policy::ProducerPolicy;
use av_edge::{hash, pb, sign, verify};
use openssl::ec::EcKey;
use openssl::pkey::{Private, Public};

const TEST_KEY_PEM: &[u8] = include_bytes!("fixtures/test_signing_key.pem");
const TEST_PUB_PEM: &[u8] = include_bytes!("fixtures/test_signing_key.pub.pem");

fn keys() -> (EcKey<Private>, EcKey<Public>) {
    (sign::load_signing_key(TEST_KEY_PEM).unwrap(), verify::load_verifying_key(TEST_PUB_PEM).unwrap())
}

fn policy() -> ProducerPolicy {
    ProducerPolicy::new(
        "producer-1",
        "CUI",
        vec!["SP-EXPT".to_string()],
        vec!["UNCLASSIFIED".to_string(), "CUI".to_string(), "SECRET".to_string()],
        "CUI",
        5_000_000_000, // 5s max age
    )
    .unwrap()
}

/// A well-formed, correctly-labelled, on-time batch at the given `sequence`/`prev_hash`,
/// signed with the test key -- the "valid except for whatever the caller changes next"
/// starting point for every corruption test below.
fn valid_batch(sequence: u64, prev_hash: &[u8], now_tai_ns: i64, signing_key: &EcKey<Private>) -> pb::MeasurementBatch {
    let mut batch = pb::MeasurementBatch {
        producer_id: "producer-1".to_string(),
        sequence,
        label: Some(pb::Label { marking: "CUI".to_string(), caveats: vec!["SP-EXPT".to_string()] }),
        batch_tai_ns: now_tai_ns,
        ..Default::default()
    };
    sign::sign_batch(&mut batch, prev_hash, signing_key).unwrap();
    batch
}

/// Accepts one genuine first batch (sequence 1) so every test has real, accepted chain
/// state to corrupt a *second* batch against. Returns the verifier (with that state),
/// the accepted first batch (for its `batch_hash`), the keys and the policy.
fn valid_chain_of_one(now_tai_ns: i64) -> (ChainVerifier, pb::MeasurementBatch, EcKey<Private>, EcKey<Public>, ProducerPolicy) {
    let (signing_key, verify_key) = keys();
    let policy = policy();
    let mut verifier = ChainVerifier::new();
    let b1 = valid_batch(1, hash::GENESIS, now_tai_ns, &signing_key);
    let v1 = verifier.submit(&b1, &policy, &verify_key, now_tai_ns);
    assert!(v1.accepted, "setup batch must be accepted: {v1:?}");
    (verifier, b1, signing_key, verify_key, policy)
}

/// Asserts that submitting `batch` yields exactly `expected` and that exactly that
/// rejection's counter incremented by one relative to `before`, with every other counter
/// (including `accepted`) unchanged.
fn assert_single_rejection(
    verifier: &mut ChainVerifier,
    batch: &pb::MeasurementBatch,
    policy: &ProducerPolicy,
    verify_key: &EcKey<Public>,
    now_tai_ns: i64,
    expected: pb::BatchRejection,
) {
    let before = verifier.counters("producer-1").cloned().unwrap();
    let verdict = verifier.submit(batch, policy, verify_key, now_tai_ns);
    assert!(!verdict.accepted, "{verdict:?}");
    assert_eq!(verdict.rejection, expected as i32, "{verdict:?}");

    let after = verifier.counters("producer-1").unwrap();
    assert_eq!(after.accepted, before.accepted, "accepted must not change");
    let fields: [(&str, u64, u64, pb::BatchRejection); 8] = [
        ("unsigned_count", before.unsigned_count, after.unsigned_count, pb::BatchRejection::Unsigned),
        ("bad_signature_count", before.bad_signature_count, after.bad_signature_count, pb::BatchRejection::BadSignature),
        ("chain_gap_count", before.chain_gap_count, after.chain_gap_count, pb::BatchRejection::ChainGap),
        ("chain_break_count", before.chain_break_count, after.chain_break_count, pb::BatchRejection::ChainBreak),
        ("mislabeled_count", before.mislabeled_count, after.mislabeled_count, pb::BatchRejection::Mislabeled),
        ("over_clearance_count", before.over_clearance_count, after.over_clearance_count, pb::BatchRejection::OverClearance),
        ("stale_count", before.stale_count, after.stale_count, pb::BatchRejection::Stale),
        ("duplicate_count", before.duplicate_count, after.duplicate_count, pb::BatchRejection::Duplicate),
    ];
    for (name, before_v, after_v, kind) in fields {
        if kind == expected {
            assert_eq!(after_v, before_v + 1, "{name} must increment by exactly one");
        } else {
            assert_eq!(after_v, before_v, "{name} must not change");
        }
    }
}

#[test]
fn unsigned_batch_is_rejected_and_counted() {
    let now = 1_000;
    let (mut verifier, b1, signing_key, verify_key, policy) = valid_chain_of_one(now);
    let mut b2 = valid_batch(2, &b1.batch_hash, now, &signing_key);
    b2.signature.clear();
    assert_single_rejection(&mut verifier, &b2, &policy, &verify_key, now, pb::BatchRejection::Unsigned);
}

#[test]
fn bad_signature_is_rejected_and_counted() {
    let now = 1_000;
    let (mut verifier, b1, signing_key, verify_key, policy) = valid_chain_of_one(now);
    let mut b2 = valid_batch(2, &b1.batch_hash, now, &signing_key);
    b2.signature[0] ^= 0x01;
    assert_single_rejection(&mut verifier, &b2, &policy, &verify_key, now, pb::BatchRejection::BadSignature);
}

#[test]
fn chain_gap_is_rejected_and_counted() {
    let now = 1_000;
    let (mut verifier, b1, signing_key, verify_key, policy) = valid_chain_of_one(now);
    // Sequence 3 instead of 2: skips a batch.
    let b3 = valid_batch(3, &b1.batch_hash, now, &signing_key);
    assert_single_rejection(&mut verifier, &b3, &policy, &verify_key, now, pb::BatchRejection::ChainGap);
}

#[test]
fn chain_break_is_rejected_and_counted() {
    let now = 1_000;
    let (mut verifier, _b1, signing_key, verify_key, policy) = valid_chain_of_one(now);
    // Correct sequence (2), but chained from a prev_hash that is not b1's real hash.
    let wrong_prev = vec![0x42u8; 32];
    let b2 = valid_batch(2, &wrong_prev, now, &signing_key);
    assert_single_rejection(&mut verifier, &b2, &policy, &verify_key, now, pb::BatchRejection::ChainBreak);
}

#[test]
fn mislabeled_batch_is_rejected_and_counted() {
    let now = 1_000;
    let (mut verifier, b1, signing_key, verify_key, policy) = valid_chain_of_one(now);
    let mut b2 = pb::MeasurementBatch {
        producer_id: "producer-1".to_string(),
        sequence: 2,
        label: Some(pb::Label { marking: "SECRET".to_string(), caveats: vec!["SP-EXPT".to_string()] }),
        batch_tai_ns: now,
        ..Default::default()
    };
    sign::sign_batch(&mut b2, &b1.batch_hash, &signing_key).unwrap();
    assert_single_rejection(&mut verifier, &b2, &policy, &verify_key, now, pb::BatchRejection::Mislabeled);
}

#[test]
fn over_clearance_batch_is_rejected_and_counted() {
    let now = 1_000;
    let (mut verifier, b1, signing_key, verify_key, _default_policy) = valid_chain_of_one(now);
    // A policy whose declared emit marking (SECRET) is itself ranked above its own
    // clearance (CUI) -- the label matches what the producer declares, but that
    // declaration is itself over clearance.
    let over_clearance_policy =
        ProducerPolicy::new("producer-1", "SECRET", vec![], vec!["UNCLASSIFIED".to_string(), "CUI".to_string(), "SECRET".to_string()], "CUI", 5_000_000_000).unwrap();
    let mut b2 = pb::MeasurementBatch {
        producer_id: "producer-1".to_string(),
        sequence: 2,
        label: Some(pb::Label { marking: "SECRET".to_string(), caveats: vec![] }),
        batch_tai_ns: now,
        ..Default::default()
    };
    sign::sign_batch(&mut b2, &b1.batch_hash, &signing_key).unwrap();
    assert_single_rejection(&mut verifier, &b2, &over_clearance_policy, &verify_key, now, pb::BatchRejection::OverClearance);
}

#[test]
fn stale_batch_is_rejected_and_counted() {
    let now = 1_000;
    let (mut verifier, b1, signing_key, verify_key, policy) = valid_chain_of_one(now);
    // max_age_ns is 5s (5_000_000_000ns); make the second batch's own epoch far older
    // than that relative to `now`.
    let old_epoch = now - 10_000_000_000;
    let b2 = valid_batch(2, &b1.batch_hash, old_epoch, &signing_key);
    assert_single_rejection(&mut verifier, &b2, &policy, &verify_key, now, pb::BatchRejection::Stale);
}

#[test]
fn duplicate_batch_is_rejected_and_counted() {
    let now = 1_000;
    let (mut verifier, b1, _signing_key, verify_key, policy) = valid_chain_of_one(now);
    // Resubmit the exact same, already-accepted first batch.
    assert_single_rejection(&mut verifier, &b1, &policy, &verify_key, now, pb::BatchRejection::Duplicate);
}

// --- Requirement 6: documented check order -----------------------------------------

/// UNSIGNED (step 1) must win over STALE (step 5) even though both defects are present.
#[test]
fn check_order_unsigned_wins_over_stale() {
    let now = 1_000;
    let (mut verifier, b1, signing_key, verify_key, policy) = valid_chain_of_one(now);
    let old_epoch = now - 10_000_000_000;
    let mut b2 = valid_batch(2, &b1.batch_hash, old_epoch, &signing_key);
    b2.signature.clear();
    let verdict = verifier.submit(&b2, &policy, &verify_key, now);
    assert_eq!(verdict.rejection, pb::BatchRejection::Unsigned as i32, "{verdict:?}");
}

/// BAD_SIGNATURE (step 2) must win over CHAIN_GAP (step 3): a batch whose signature does
/// not verify at all is diagnosed as forged, not merely as skipping a sequence number.
#[test]
fn check_order_bad_signature_wins_over_chain_gap() {
    let now = 1_000;
    let (mut verifier, b1, signing_key, verify_key, policy) = valid_chain_of_one(now);
    // Sequence 3 (a gap) AND a corrupted signature.
    let mut b3 = valid_batch(3, &b1.batch_hash, now, &signing_key);
    b3.signature[0] ^= 0x01;
    let verdict = verifier.submit(&b3, &policy, &verify_key, now);
    assert_eq!(verdict.rejection, pb::BatchRejection::BadSignature as i32, "{verdict:?}");
}

/// CHAIN_GAP (step 3) must win over STALE (step 5): chain linkage is diagnosed before
/// the batch's age is even considered.
#[test]
fn check_order_chain_gap_wins_over_stale() {
    let now = 1_000;
    let (mut verifier, b1, signing_key, verify_key, policy) = valid_chain_of_one(now);
    let old_epoch = now - 10_000_000_000;
    // Sequence 3 (a gap) AND an epoch far older than max_age_ns.
    let b3 = valid_batch(3, &b1.batch_hash, old_epoch, &signing_key);
    let verdict = verifier.submit(&b3, &policy, &verify_key, now);
    assert_eq!(verdict.rejection, pb::BatchRejection::ChainGap as i32, "{verdict:?}");
}

/// MISLABELED (step 4) must win over STALE (step 5).
#[test]
fn check_order_mislabeled_wins_over_stale() {
    let now = 1_000;
    let (mut verifier, b1, signing_key, verify_key, policy) = valid_chain_of_one(now);
    let old_epoch = now - 10_000_000_000;
    let mut b2 = pb::MeasurementBatch {
        producer_id: "producer-1".to_string(),
        sequence: 2,
        label: Some(pb::Label { marking: "SECRET".to_string(), caveats: vec![] }),
        batch_tai_ns: old_epoch,
        ..Default::default()
    };
    sign::sign_batch(&mut b2, &b1.batch_hash, &signing_key).unwrap();
    let verdict = verifier.submit(&b2, &policy, &verify_key, now);
    assert_eq!(verdict.rejection, pb::BatchRejection::Mislabeled as i32, "{verdict:?}");
}
