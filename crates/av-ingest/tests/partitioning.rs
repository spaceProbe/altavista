//! Requirement 6: batches from two producers, interleaved across two `shard_key`s, land
//! in the right partition files; each partition's own record-hash chain verifies
//! independently (`PartitionLog::verify`), and each producer's own per-producer signature
//! chain verifies independently (`av_edge::chain::walk_chain`) -- the "two chains,
//! deliberately" property `av_ingest::log`'s own module doc documents: one partition can,
//! and here does, interleave batches from several producers, and one producer's own
//! batches can, and here do, land across several partitions.

use std::path::PathBuf;

use av_edge::chain::walk_chain;
use av_edge::policy::ProducerPolicy;
use av_edge::{hash, pb, sign, verify};
use av_ingest::ingest::{Ingest, Signer};
use openssl::ec::EcKey;
use openssl::pkey::{Private, Public};

fn tmp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("av-ingest-test-partitioning-{name}-{}", std::process::id()));
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
    ProducerPolicy::new(producer_id, "CUI", vec![], vec!["UNCLASSIFIED".to_string(), "CUI".to_string()], "CUI", 10_000_000_000).unwrap()
}

fn signed(producer: &str, sequence: u64, prev: &[u8], shard: &str, key: &EcKey<Private>) -> pb::MeasurementBatch {
    let mut b = pb::MeasurementBatch {
        producer_id: producer.to_string(),
        sequence,
        batch_tai_ns: 1_000,
        label: Some(pb::Label { marking: "CUI".to_string(), caveats: vec![] }),
        shard_key: shard.to_string(),
        measurements: vec![pb::Measurement { measurement_id: format!("{producer}-{sequence}"), shard_key: shard.to_string(), ..Default::default() }],
        ..Default::default()
    };
    sign::sign_batch(&mut b, prev, key).unwrap();
    b
}

#[test]
fn interleaved_producers_and_shards_land_in_the_right_files_and_both_chains_verify_independently() {
    let dir = tmp_dir("interleaved");
    let (signing_key, verify_key) = keys();
    let mut ingest = Ingest::new(&dir, None);
    ingest.register_producer(policy("producer-a"));
    ingest.register_producer(policy("producer-b"));

    // Producer A: shard-x, shard-y, shard-x (its own chain spans two partitions).
    // Producer B: shard-y, shard-x, shard-y (interleaved with A in both partitions).
    let a1 = signed("producer-a", 1, hash::GENESIS, "shard-x", &signing_key);
    let b1 = signed("producer-b", 1, hash::GENESIS, "shard-y", &signing_key);
    let a2 = signed("producer-a", 2, &a1.batch_hash, "shard-y", &signing_key);
    let b2 = signed("producer-b", 2, &b1.batch_hash, "shard-x", &signing_key);
    let a3 = signed("producer-a", 3, &a2.batch_hash, "shard-x", &signing_key);
    let b3 = signed("producer-b", 3, &b2.batch_hash, "shard-y", &signing_key);

    let mut a_chain = vec![a1.clone(), a2.clone(), a3.clone()];
    let mut b_chain = vec![b1.clone(), b2.clone(), b3.clone()];

    for b in [&a1, &b1, &a2, &b2, &a3, &b3] {
        let outcome = ingest.submit(b, Signer::Key(verify_key.clone()), 1_000).unwrap();
        assert!(outcome.accepted(), "{outcome:?}");
    }

    // --- Partition placement: shard-x holds {a1, b2, a3}; shard-y holds {b1, a2, b3}. ---
    let shard_x_batches = decode_all(&dir.join("shard-x.avlog"));
    let shard_y_batches = decode_all(&dir.join("shard-y.avlog"));

    assert_eq!(shard_x_batches.len(), 3, "shard-x must hold exactly the 3 batches declared for it");
    assert_eq!(shard_y_batches.len(), 3, "shard-y must hold exactly the 3 batches declared for it");
    for batch in &shard_x_batches {
        assert_eq!(batch.shard_key, "shard-x");
    }
    for batch in &shard_y_batches {
        assert_eq!(batch.shard_key, "shard-y");
    }
    assert_eq!(shard_x_batches.iter().map(|b| (b.producer_id.clone(), b.sequence)).collect::<Vec<_>>(), vec![("producer-a".to_string(), 1), ("producer-b".to_string(), 2), ("producer-a".to_string(), 3)]);
    assert_eq!(shard_y_batches.iter().map(|b| (b.producer_id.clone(), b.sequence)).collect::<Vec<_>>(), vec![("producer-b".to_string(), 1), ("producer-a".to_string(), 2), ("producer-b".to_string(), 3)]);

    // --- Each partition's own record-hash chain verifies independently. ---
    for (shard_key, log) in ingest.partitions() {
        let verification = log.verify().unwrap();
        assert!(verification.ok, "partition {shard_key} must verify clean: {verification:?}");
        assert_eq!(verification.checked, 3);
    }

    // --- Each producer's own per-producer signature chain also verifies independently,
    // via av_edge::chain::walk_chain over that producer's batches *in the order that
    // producer emitted them* -- entirely unaffected by which partitions they landed in. ---
    a_chain.sort_by_key(|b| b.sequence);
    b_chain.sort_by_key(|b| b.sequence);
    let a_verification = walk_chain("producer-a", &a_chain, &verify_key);
    let b_verification = walk_chain("producer-b", &b_chain, &verify_key);
    assert!(a_verification.ok, "{a_verification:?}");
    assert_eq!(a_verification.checked, 3);
    assert!(b_verification.ok, "{b_verification:?}");
    assert_eq!(b_verification.checked, 3);
}

fn decode_all(path: &std::path::Path) -> Vec<pb::MeasurementBatch> {
    let bytes = std::fs::read(path).unwrap();
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos < bytes.len() {
        let payload_len = u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
        let payload_start = pos + 36;
        let payload_end = payload_start + payload_len;
        let batch: pb::MeasurementBatch = prost::Message::decode(&bytes[payload_start..payload_end]).unwrap();
        out.push(batch);
        pos = payload_end;
    }
    out
}
