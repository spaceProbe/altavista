//! Requirement 4: a chain of one thousand batches built, signed and verified end to end
//! through both [`ChainVerifier`] (live, stateful acceptance) and [`walk_chain`] (pure,
//! after-the-fact); then the same chain with one byte tampered somewhere in the middle,
//! proving the walker locates the exact tampered sequence.

use av_edge::chain::{walk_chain, ChainVerifier};
use av_edge::policy::ProducerPolicy;
use av_edge::{hash, pb, sign, verify};

const TEST_KEY_PEM: &[u8] = include_bytes!("fixtures/test_signing_key.pem");
const TEST_PUB_PEM: &[u8] = include_bytes!("fixtures/test_signing_key.pub.pem");

const CHAIN_LEN: u64 = 1000;

/// Builds `CHAIN_LEN` signed, correctly chained batches for one producer, each carrying
/// one measurement whose value encodes its own sequence number (so a tampered batch is
/// easy to construct against a specific, known sequence).
fn build_chain(signing_key: &openssl::ec::EcKey<openssl::pkey::Private>) -> Vec<pb::MeasurementBatch> {
    let mut batches = Vec::with_capacity(CHAIN_LEN as usize);
    let mut prev_hash = hash::GENESIS.to_vec();
    for seq in 1..=CHAIN_LEN {
        let mut batch = pb::MeasurementBatch {
            producer_id: "bulk-producer".to_string(),
            sequence: seq,
            label: Some(pb::Label { marking: "UNCLASSIFIED".to_string(), caveats: vec![] }),
            measurements: vec![pb::Measurement { measurement_id: format!("m{seq}"), z: vec![seq as f64], sensor_id: "bulk-sensor".to_string(), ..Default::default() }],
            batch_tai_ns: 1_000_000_000 + seq as i64,
            ..Default::default()
        };
        sign::sign_batch(&mut batch, &prev_hash, signing_key).unwrap();
        prev_hash = batch.batch_hash.clone();
        batches.push(batch);
    }
    batches
}

#[test]
fn a_thousand_batch_chain_is_accepted_end_to_end_through_the_chain_verifier() {
    let signing_key = sign::load_signing_key(TEST_KEY_PEM).unwrap();
    let verify_key = verify::load_verifying_key(TEST_PUB_PEM).unwrap();
    let policy = ProducerPolicy::new("bulk-producer", "UNCLASSIFIED", vec![], vec!["UNCLASSIFIED".to_string()], "UNCLASSIFIED", i64::MAX).unwrap();

    let batches = build_chain(&signing_key);
    assert_eq!(batches.len(), CHAIN_LEN as usize);

    let mut verifier = ChainVerifier::new();
    for batch in &batches {
        let verdict = verifier.submit(batch, &policy, &verify_key, 1_000_000_000 + CHAIN_LEN as i64);
        assert!(verdict.accepted, "sequence {} was rejected: {verdict:?}", batch.sequence);
    }
    let counters = verifier.counters("bulk-producer").unwrap();
    assert_eq!(counters.accepted, CHAIN_LEN);
    assert_eq!(counters.chain_head, batches.last().unwrap().batch_hash);
}

#[test]
fn a_thousand_batch_chain_walks_clean_when_untampered() {
    let signing_key = sign::load_signing_key(TEST_KEY_PEM).unwrap();
    let verify_key = verify::load_verifying_key(TEST_PUB_PEM).unwrap();
    let batches = build_chain(&signing_key);

    let result = walk_chain("bulk-producer", &batches, &verify_key);
    assert!(result.ok, "{result:?}");
    assert_eq!(result.checked, CHAIN_LEN);
}

#[test]
fn walk_chain_locates_a_single_tampered_byte_in_the_middle_of_a_thousand_batches() {
    let signing_key = sign::load_signing_key(TEST_KEY_PEM).unwrap();
    let verify_key = verify::load_verifying_key(TEST_PUB_PEM).unwrap();
    let mut batches = build_chain(&signing_key);

    // Sanity: untampered, the full chain walks clean.
    assert!(walk_chain("bulk-producer", &batches, &verify_key).ok);

    // Tamper exactly one bit somewhere in the middle: flip the low bit of one
    // measurement's `z` value in the batch at sequence 500, without touching its
    // batch_hash/signature (exactly what a bit flip on disk or in flight would do).
    const TAMPERED_SEQUENCE: u64 = 500;
    let idx = (TAMPERED_SEQUENCE - 1) as usize;
    assert_eq!(batches[idx].sequence, TAMPERED_SEQUENCE);
    let z = &mut batches[idx].measurements[0].z[0];
    *z = f64::from_bits(z.to_bits() ^ 1);

    let result = walk_chain("bulk-producer", &batches, &verify_key);
    assert!(!result.ok, "the walker must detect the tampered batch");
    assert_eq!(result.broken_at_sequence, TAMPERED_SEQUENCE, "the walker must locate the exact tampered sequence");
    assert_eq!(result.checked, TAMPERED_SEQUENCE - 1, "every batch before the tamper must still check out clean");
}
