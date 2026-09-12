//! Requirement 1 (`docs/edge-plan.md` milestone E3, this round's "through the ingest's
//! public API" reading -- see `av_ingest`'s own `lib.rs` module doc): every one of the
//! nine `BatchRejection` kinds -- the eight `av_edge::chain::ChainVerifier` already
//! produces, plus this crate's own `SHARD_MISMATCH` -- is produced by exactly one
//! deliberate corruption of an otherwise-valid batch, through `av_ingest::ingest::Ingest::
//! submit`'s own public API (never a wire -- this crate builds no gRPC service; see the
//! crate's `lib.rs` module doc), and asserts both that exactly one counter moved and that
//! the batch's own partition file did not grow by a single byte.
//!
//! This is exercised entirely in-process against a temp directory this test module owns
//! (never the repository, never any other test's directory).

use std::path::{Path, PathBuf};

use av_edge::policy::ProducerPolicy;
use av_edge::{hash, pb, sign, verify};
use av_ingest::ingest::{Ingest, IngestOutcome, Signer};
use openssl::ec::EcKey;
use openssl::pkey::{Private, Public};

const TEST_KEY_PEM: &[u8] = include_bytes!("fixtures/test_signing_key.pem");
const TEST_PUB_PEM: &[u8] = include_bytes!("fixtures/test_signing_key.pub.pem");

fn tmp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("av-ingest-test-rejections-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

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

fn valid_batch(sequence: u64, prev_hash: &[u8], now_tai_ns: i64, shard_key: &str, signing_key: &EcKey<Private>) -> pb::MeasurementBatch {
    let mut batch = pb::MeasurementBatch {
        producer_id: "producer-1".to_string(),
        sequence,
        label: Some(pb::Label { marking: "CUI".to_string(), caveats: vec!["SP-EXPT".to_string()] }),
        batch_tai_ns: now_tai_ns,
        shard_key: shard_key.to_string(),
        measurements: vec![pb::Measurement { measurement_id: "m1".to_string(), shard_key: shard_key.to_string(), ..Default::default() }],
        ..Default::default()
    };
    sign::sign_batch(&mut batch, prev_hash, signing_key).unwrap();
    batch
}

fn partition_file_len(dir: &Path, shard_key: &str) -> u64 {
    let path = dir.join(format!("{shard_key}.avlog"));
    std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0)
}

/// Sets up one `Ingest` with `producer-1` registered, submits one genuine first batch
/// (accepted), and returns the ingest, that accepted batch (for its `batch_hash`), the
/// keys, the log directory, and the shard key every test below uses. `name` must be
/// unique per caller (this test binary runs `#[test]` functions in parallel by default,
/// on separate threads within the same process/pid, so a shared directory name here would
/// let two tests race on the very same partition file).
fn setup(now_tai_ns: i64, name: &str) -> (Ingest, pb::MeasurementBatch, EcKey<Private>, EcKey<Public>, PathBuf, String) {
    let dir = tmp_dir(name);
    let (signing_key, verify_key) = keys();
    let mut ingest = Ingest::new(&dir, None);
    ingest.register_producer(policy());

    let shard_key = "shard-a".to_string();
    let b1 = valid_batch(1, hash::GENESIS, now_tai_ns, &shard_key, &signing_key);
    let outcome = ingest.submit(&b1, Signer::Key(verify_key.clone()), now_tai_ns).unwrap();
    assert!(outcome.accepted(), "{outcome:?}");

    (ingest, b1, signing_key, verify_key, dir, shard_key)
}

/// Submits `batch` and asserts it was rejected as `expected`, that exactly that counter
/// moved (all nine tracked independently: eight from `producer_counters` plus this
/// crate's own `shard_mismatch_count`), and that the partition file's byte length is
/// unchanged from `before_len`.
fn assert_single_rejection(
    ingest: &mut Ingest,
    batch: &pb::MeasurementBatch,
    verify_key: &EcKey<Public>,
    now_tai_ns: i64,
    dir: &Path,
    shard_key: &str,
    expected: pb::BatchRejection,
) {
    let before = ingest.producer_counters("producer-1");
    let before_shard_mismatch = ingest.shard_mismatch_count("producer-1");
    let before_len = partition_file_len(dir, shard_key);

    let outcome = ingest.submit(batch, Signer::Key(verify_key.clone()), now_tai_ns).unwrap();
    let IngestOutcome::Verdict(verdict) = outcome else { panic!("expected a Verdict outcome, got {outcome:?}") };
    assert!(!verdict.accepted, "{verdict:?}");
    assert_eq!(verdict.rejection, expected as i32, "{verdict:?}");

    let after = ingest.producer_counters("producer-1");
    let after_shard_mismatch = ingest.shard_mismatch_count("producer-1");
    assert_eq!(after.accepted, before.accepted, "accepted must not change");

    let fields: [(&str, u64, u64, pb::BatchRejection); 9] = [
        ("unsigned_count", before.unsigned_count, after.unsigned_count, pb::BatchRejection::Unsigned),
        ("bad_signature_count", before.bad_signature_count, after.bad_signature_count, pb::BatchRejection::BadSignature),
        ("chain_gap_count", before.chain_gap_count, after.chain_gap_count, pb::BatchRejection::ChainGap),
        ("chain_break_count", before.chain_break_count, after.chain_break_count, pb::BatchRejection::ChainBreak),
        ("mislabeled_count", before.mislabeled_count, after.mislabeled_count, pb::BatchRejection::Mislabeled),
        ("over_clearance_count", before.over_clearance_count, after.over_clearance_count, pb::BatchRejection::OverClearance),
        ("stale_count", before.stale_count, after.stale_count, pb::BatchRejection::Stale),
        ("duplicate_count", before.duplicate_count, after.duplicate_count, pb::BatchRejection::Duplicate),
        ("shard_mismatch_count", before_shard_mismatch, after_shard_mismatch, pb::BatchRejection::ShardMismatch),
    ];
    for (name, before_v, after_v, kind) in fields {
        if kind == expected {
            assert_eq!(after_v, before_v + 1, "{name} must increment by exactly one");
        } else {
            assert_eq!(after_v, before_v, "{name} must not change");
        }
    }

    let after_len = partition_file_len(dir, shard_key);
    assert_eq!(after_len, before_len, "a rejected batch must never grow the partition file");
}

#[test]
fn unsigned_batch_is_rejected_and_counted() {
    let now = 1_000;
    let (mut ingest, b1, signing_key, verify_key, dir, shard_key) = setup(now, "unsigned");
    let mut b2 = valid_batch(2, &b1.batch_hash, now, &shard_key, &signing_key);
    b2.signature.clear();
    assert_single_rejection(&mut ingest, &b2, &verify_key, now, &dir, &shard_key, pb::BatchRejection::Unsigned);
}

#[test]
fn bad_signature_is_rejected_and_counted() {
    let now = 1_000;
    let (mut ingest, b1, signing_key, verify_key, dir, shard_key) = setup(now, "bad-signature");
    let mut b2 = valid_batch(2, &b1.batch_hash, now, &shard_key, &signing_key);
    b2.signature[0] ^= 0x01;
    assert_single_rejection(&mut ingest, &b2, &verify_key, now, &dir, &shard_key, pb::BatchRejection::BadSignature);
}

#[test]
fn chain_gap_is_rejected_and_counted() {
    let now = 1_000;
    let (mut ingest, b1, signing_key, verify_key, dir, shard_key) = setup(now, "chain-gap");
    let b3 = valid_batch(3, &b1.batch_hash, now, &shard_key, &signing_key); // skips sequence 2
    assert_single_rejection(&mut ingest, &b3, &verify_key, now, &dir, &shard_key, pb::BatchRejection::ChainGap);
}

#[test]
fn chain_break_is_rejected_and_counted() {
    let now = 1_000;
    let (mut ingest, _b1, signing_key, verify_key, dir, shard_key) = setup(now, "chain-break");
    let wrong_prev = vec![0x42u8; 32];
    let b2 = valid_batch(2, &wrong_prev, now, &shard_key, &signing_key);
    assert_single_rejection(&mut ingest, &b2, &verify_key, now, &dir, &shard_key, pb::BatchRejection::ChainBreak);
}

#[test]
fn mislabeled_batch_is_rejected_and_counted() {
    let now = 1_000;
    let (mut ingest, b1, signing_key, verify_key, dir, shard_key) = setup(now, "mislabeled");
    let mut b2 = pb::MeasurementBatch {
        producer_id: "producer-1".to_string(),
        sequence: 2,
        label: Some(pb::Label { marking: "SECRET".to_string(), caveats: vec!["SP-EXPT".to_string()] }),
        batch_tai_ns: now,
        shard_key: shard_key.clone(),
        measurements: vec![pb::Measurement { measurement_id: "m1".to_string(), shard_key: shard_key.clone(), ..Default::default() }],
        ..Default::default()
    };
    sign::sign_batch(&mut b2, &b1.batch_hash, &signing_key).unwrap();
    assert_single_rejection(&mut ingest, &b2, &verify_key, now, &dir, &shard_key, pb::BatchRejection::Mislabeled);
}

#[test]
fn over_clearance_batch_is_rejected_and_counted() {
    let now = 1_000;
    let dir = tmp_dir("over-clearance");
    let (signing_key, verify_key) = keys();
    let mut ingest = Ingest::new(&dir, None);
    // A policy whose declared emit marking (SECRET) is itself ranked above its own
    // clearance (CUI): the label matches what the producer declares, but the declaration
    // itself is over clearance.
    let over_clearance_policy =
        ProducerPolicy::new("producer-1", "SECRET", vec![], vec!["UNCLASSIFIED".to_string(), "CUI".to_string(), "SECRET".to_string()], "CUI", 5_000_000_000).unwrap();
    ingest.register_producer(over_clearance_policy);

    let shard_key = "shard-a".to_string();
    let mut b1 = pb::MeasurementBatch {
        producer_id: "producer-1".to_string(),
        sequence: 1,
        label: Some(pb::Label { marking: "SECRET".to_string(), caveats: vec![] }),
        batch_tai_ns: now,
        shard_key: shard_key.clone(),
        measurements: vec![pb::Measurement { measurement_id: "m1".to_string(), shard_key: shard_key.clone(), ..Default::default() }],
        ..Default::default()
    };
    sign::sign_batch(&mut b1, hash::GENESIS, &signing_key).unwrap();

    assert_single_rejection(&mut ingest, &b1, &verify_key, now, &dir, &shard_key, pb::BatchRejection::OverClearance);
}

#[test]
fn stale_batch_is_rejected_and_counted() {
    let now = 1_000;
    let (mut ingest, b1, signing_key, verify_key, dir, shard_key) = setup(now, "stale");
    let old_epoch = now - 10_000_000_000; // max_age_ns is 5s
    let b2 = valid_batch(2, &b1.batch_hash, old_epoch, &shard_key, &signing_key);
    assert_single_rejection(&mut ingest, &b2, &verify_key, now, &dir, &shard_key, pb::BatchRejection::Stale);
}

#[test]
fn duplicate_batch_is_rejected_and_counted() {
    let now = 1_000;
    let (mut ingest, b1, _signing_key, verify_key, dir, shard_key) = setup(now, "duplicate");
    assert_single_rejection(&mut ingest, &b1, &verify_key, now, &dir, &shard_key, pb::BatchRejection::Duplicate);
}

#[test]
fn shard_mismatch_batch_is_rejected_and_counted() {
    let now = 1_000;
    let (mut ingest, b1, signing_key, verify_key, dir, shard_key) = setup(now, "shard-mismatch");
    // Sequence 2, correctly chained and labelled, but a measurement whose own shard_key
    // disagrees with the batch's own declared shard_key.
    let mut b2 = pb::MeasurementBatch {
        producer_id: "producer-1".to_string(),
        sequence: 2,
        label: Some(pb::Label { marking: "CUI".to_string(), caveats: vec!["SP-EXPT".to_string()] }),
        batch_tai_ns: now,
        shard_key: shard_key.clone(),
        measurements: vec![pb::Measurement { measurement_id: "m1".to_string(), shard_key: "shard-b".to_string(), ..Default::default() }],
        ..Default::default()
    };
    sign::sign_batch(&mut b2, &b1.batch_hash, &signing_key).unwrap();
    assert_single_rejection(&mut ingest, &b2, &verify_key, now, &dir, &shard_key, pb::BatchRejection::ShardMismatch);
}

// --- Documented check order: SHARD_MISMATCH is checked before ChainVerifier ever runs ---

/// A batch that is BOTH shard-mismatched AND would otherwise be a chain gap (sequence 3,
/// skipping 2): `av_ingest::ingest`'s module doc says SHARD_MISMATCH is checked first, so
/// it must win, and -- critically -- the producer's chain state must not have advanced at
/// all (asserted by resubmitting a genuine sequence-2 batch afterwards and seeing it
/// accepted, which would be impossible if the gapped submission had already consumed
/// sequence 2 as "last accepted").
#[test]
fn shard_mismatch_is_checked_before_chain_verifier_and_never_touches_chain_state() {
    let now = 1_000;
    let (mut ingest, b1, signing_key, verify_key, dir, shard_key) = setup(now, "check-order");
    let mut b3 = pb::MeasurementBatch {
        producer_id: "producer-1".to_string(),
        sequence: 3, // would be CHAIN_GAP if this reached ChainVerifier
        label: Some(pb::Label { marking: "CUI".to_string(), caveats: vec!["SP-EXPT".to_string()] }),
        batch_tai_ns: now,
        shard_key: shard_key.clone(),
        measurements: vec![pb::Measurement { measurement_id: "m1".to_string(), shard_key: "wrong-shard".to_string(), ..Default::default() }],
        ..Default::default()
    };
    sign::sign_batch(&mut b3, &b1.batch_hash, &signing_key).unwrap();

    let outcome = ingest.submit(&b3, Signer::Key(verify_key.clone()), now).unwrap();
    let IngestOutcome::Verdict(verdict) = &outcome else { panic!("{outcome:?}") };
    assert_eq!(verdict.rejection, pb::BatchRejection::ShardMismatch as i32, "SHARD_MISMATCH must win over CHAIN_GAP: {verdict:?}");
    assert_eq!(ingest.producer_counters("producer-1").chain_gap_count, 0, "chain_gap_count must not move -- ChainVerifier must never have seen this batch");

    // Chain state must be exactly as it was after b1: a genuine sequence-2 batch must
    // still be accepted next, proving `last_accepted_sequence` never advanced past 1.
    let b2 = valid_batch(2, &b1.batch_hash, now, &shard_key, &signing_key);
    let outcome2 = ingest.submit(&b2, Signer::Key(verify_key), now).unwrap();
    assert!(outcome2.accepted(), "a genuine sequence-2 batch must still be accepted: {outcome2:?}");

    let _ = dir; // kept for symmetry with the other tests' signature; unused here.
}
