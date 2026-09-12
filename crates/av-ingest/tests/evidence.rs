//! Requirement 8: `av_ingest::evidence::evidence`/`verify_all` report the chain heads and
//! counters that the partitions and pipeline actually hold, asserted against
//! independently computed values (never by re-deriving them through the same code path
//! under test).

use std::path::PathBuf;

use av_edge::policy::ProducerPolicy;
use av_edge::{hash, pb, sign, verify};
use av_ingest::evidence;
use av_ingest::ingest::{Ingest, Signer};
use openssl::ec::EcKey;
use openssl::pkey::{Private, Public};

fn tmp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("av-ingest-test-evidence-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn keys() -> (EcKey<Private>, EcKey<Public>) {
    (
        sign::load_signing_key(include_bytes!("fixtures/test_signing_key.pem")).unwrap(),
        verify::load_verifying_key(include_bytes!("fixtures/test_signing_key.pub.pem")).unwrap(),
    )
}

fn policy(producer_id: &str) -> ProducerPolicy {
    ProducerPolicy::new(producer_id, "CUI", vec![], vec!["UNCLASSIFIED".to_string(), "CUI".to_string()], "CUI", 5_000_000_000).unwrap()
}

fn signed(producer: &str, sequence: u64, prev: &[u8], shard: &str, tai_ns: i64, key: &EcKey<Private>) -> pb::MeasurementBatch {
    let mut b = pb::MeasurementBatch {
        producer_id: producer.to_string(),
        sequence,
        batch_tai_ns: tai_ns,
        label: Some(pb::Label { marking: "CUI".to_string(), caveats: vec![] }),
        shard_key: shard.to_string(),
        measurements: vec![pb::Measurement { measurement_id: format!("{producer}-{sequence}"), shard_key: shard.to_string(), ..Default::default() }],
        ..Default::default()
    };
    sign::sign_batch(&mut b, prev, key).unwrap();
    b
}

#[test]
fn evidence_reports_the_chain_heads_and_counters_the_partitions_and_pipeline_actually_hold() {
    let dir = tmp_dir("surface");
    let (signing_key, verify_key) = keys();
    let mut ingest = Ingest::new(&dir, None);
    ingest.register_producer(policy("producer-a"));
    ingest.register_producer(policy("producer-b"));

    let now = 1_000;
    let a1 = signed("producer-a", 1, hash::GENESIS, "shard-x", now, &signing_key);
    let a2 = signed("producer-a", 2, &a1.batch_hash, "shard-x", now, &signing_key);
    let b1 = signed("producer-b", 1, hash::GENESIS, "shard-y", now, &signing_key);

    for b in [&a1, &a2, &b1] {
        let outcome = ingest.submit(b, Signer::Key(verify_key.clone()), now).unwrap();
        assert!(outcome.accepted(), "{outcome:?}");
    }

    // One deliberate rejection each, for two different kinds, so the evidence surface's
    // counters have something other than zero/accepted to report.
    let mut a3_bad_sig = signed("producer-a", 3, &a2.batch_hash, "shard-x", now, &signing_key);
    a3_bad_sig.signature[0] ^= 0x01;
    let outcome = ingest.submit(&a3_bad_sig, Signer::Key(verify_key.clone()), now).unwrap();
    assert!(!outcome.accepted());

    let mut b2_shard_mismatch = pb::MeasurementBatch {
        producer_id: "producer-b".to_string(),
        sequence: 2,
        batch_tai_ns: now,
        label: Some(pb::Label { marking: "CUI".to_string(), caveats: vec![] }),
        shard_key: "shard-y".to_string(),
        measurements: vec![pb::Measurement { measurement_id: "producer-b-2".to_string(), shard_key: "shard-z".to_string(), ..Default::default() }],
        ..Default::default()
    };
    sign::sign_batch(&mut b2_shard_mismatch, &b1.batch_hash, &signing_key).unwrap();
    let outcome = ingest.submit(&b2_shard_mismatch, Signer::Key(verify_key), now).unwrap();
    assert!(!outcome.accepted());

    // --- Independently-computed expectations. ---
    let expected_shard_x_head = hash::hex_encode(&a2.batch_hash_via_partition_chain(&a1));
    let expected_shard_y_head = hash::hex_encode(&b1.batch_hash_via_partition_chain_genesis());

    let value = evidence::evidence(&ingest);
    let obj = value.as_object().expect("evidence() must return a JSON object");

    // Top-level keys are present and the map is genuinely an object with sorted-looking,
    // stable keys (BTreeMap-backed) -- not asserting on serde_json's iteration order
    // directly (Value::Object internally may or may not preserve insertion order
    // depending on features), but on the actual values, computed independently below.
    assert_eq!(obj["accepted_total"], serde_json::json!(3), "3 batches were accepted across both producers");
    assert_eq!(obj["rejected_total"], serde_json::json!(2), "1 bad-signature + 1 shard-mismatch rejection");

    let partitions = obj["partitions"].as_object().unwrap();
    assert_eq!(partitions["shard-x"]["record_count"], serde_json::json!(2));
    assert_eq!(partitions["shard-x"]["chain_head"], serde_json::json!(expected_shard_x_head));
    assert_eq!(partitions["shard-y"]["record_count"], serde_json::json!(1));
    assert_eq!(partitions["shard-y"]["chain_head"], serde_json::json!(expected_shard_y_head));

    let producers = obj["producers"].as_object().unwrap();
    assert_eq!(producers["producer-a"]["accepted"], serde_json::json!(2));
    assert_eq!(producers["producer-a"]["bad_signature_count"], serde_json::json!(1));
    assert_eq!(producers["producer-a"]["shard_mismatch_count"], serde_json::json!(0));
    assert_eq!(producers["producer-b"]["accepted"], serde_json::json!(1));
    assert_eq!(producers["producer-b"]["shard_mismatch_count"], serde_json::json!(1));
    assert_eq!(producers["producer-b"]["bad_signature_count"], serde_json::json!(0));

    let identity = obj["identity"].as_object().unwrap();
    assert_eq!(identity["accepted"], serde_json::json!(0), "no Signer::Certificate was ever used in this test, so identity counters must be all zero");

    // --- verify_all(): one ChainVerification per partition, matching what
    // PartitionLog::verify itself reports independently for each. ---
    let verifications = evidence::verify_all(&ingest);
    assert_eq!(verifications.len(), 2);
    for (shard_key, log) in ingest.partitions() {
        let direct = log.verify().unwrap();
        let via_surface = &verifications[shard_key];
        assert_eq!(via_surface.ok, direct.ok);
        assert_eq!(via_surface.checked, direct.checked);
        assert!(direct.ok, "{direct:?}");
    }
}

/// Independent (from `av_ingest::log`'s own internals) recomputation of a partition's
/// record_hash chain, used only to build this test's own expected values -- calls
/// `av_edge::hash::compute_batch_hash` directly (the same primitive `log.rs` documents
/// using), never `PartitionLog` itself, so this is a genuinely independent check.
trait IndependentPartitionHash {
    fn batch_hash_via_partition_chain(&self, prev_batch: &pb::MeasurementBatch) -> [u8; 32];
    fn batch_hash_via_partition_chain_genesis(&self) -> [u8; 32];
}

impl IndependentPartitionHash for pb::MeasurementBatch {
    fn batch_hash_via_partition_chain(&self, prev_batch: &pb::MeasurementBatch) -> [u8; 32] {
        let prev_payload = prost::Message::encode_to_vec(prev_batch);
        let prev_record_hash = hash::compute_batch_hash(hash::GENESIS, &prev_payload);
        let payload = prost::Message::encode_to_vec(self);
        hash::compute_batch_hash(&prev_record_hash, &payload)
    }

    fn batch_hash_via_partition_chain_genesis(&self) -> [u8; 32] {
        let payload = prost::Message::encode_to_vec(self);
        hash::compute_batch_hash(hash::GENESIS, &payload)
    }
}
