//! Requirement 5: two runs over the same batches into two different directories produce
//! byte-identical partition files. Compares SHA-256 of each partition file across the two
//! runs -- a difference would mean either the record framing or the canonical
//! `MeasurementBatch` encoding is not actually a deterministic function of the batches'
//! field values (e.g. a `HashMap`-ordered field sneaking non-determinism into the
//! protobuf encoding, or a live clock/random value leaking into the log itself), which
//! would break every downstream consumer that expects two independent ingests of the same
//! evidence to agree byte for byte (ADR-004's determinism rule).
//!
//! The batches themselves are built and **signed exactly once** before either run: ECDSA
//! signatures are not byte-reproducible run to run (`av_edge`'s own module doc -- OpenSSL's
//! ECDSA uses a random nonce per signing operation), so re-signing per run would make the
//! *inputs* themselves differ, which is not what this test is about. What must be
//! deterministic is what `av_ingest::log` does with a fixed set of already-signed batches.

use std::path::PathBuf;

use av_edge::{hash, pb, sign};
use av_ingest::ingest::{Ingest, Signer};
use openssl::ec::EcKey;
use openssl::pkey::{Private, Public};
use openssl::sha::sha256;

fn tmp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("av-ingest-test-determinism-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn keys() -> (EcKey<Private>, EcKey<Public>) {
    (
        sign::load_signing_key(include_bytes!("fixtures/test_signing_key.pem")).unwrap(),
        av_edge::verify::load_verifying_key(include_bytes!("fixtures/test_signing_key.pub.pem")).unwrap(),
    )
}

fn policy(producer_id: &str) -> av_edge::policy::ProducerPolicy {
    av_edge::policy::ProducerPolicy::new(producer_id, "CUI", vec![], vec!["UNCLASSIFIED".to_string(), "CUI".to_string()], "CUI", 10_000_000_000).unwrap()
}

/// Builds and signs, once, a fixed interleaved set of batches from two producers across
/// two shard keys -- the exact fixture both runs below submit unchanged.
fn build_fixed_batches(key: &EcKey<Private>) -> Vec<pb::MeasurementBatch> {
    let mk = |producer: &str, sequence: u64, prev: &[u8], shard: &str| -> pb::MeasurementBatch {
        let mut b = pb::MeasurementBatch {
            producer_id: producer.to_string(),
            sequence,
            batch_tai_ns: 1_000 + sequence as i64,
            label: Some(pb::Label { marking: "CUI".to_string(), caveats: vec![] }),
            shard_key: shard.to_string(),
            measurements: vec![pb::Measurement { measurement_id: format!("{producer}-{sequence}"), shard_key: shard.to_string(), z: vec![sequence as f64], ..Default::default() }],
            ..Default::default()
        };
        sign::sign_batch(&mut b, prev, key).unwrap();
        b
    };

    let a1 = mk("producer-a", 1, hash::GENESIS, "shard-x");
    let a2 = mk("producer-a", 2, &a1.batch_hash, "shard-y");
    let b1 = mk("producer-b", 1, hash::GENESIS, "shard-x");
    let a3 = mk("producer-a", 3, &a2.batch_hash, "shard-x");
    let b2 = mk("producer-b", 2, &b1.batch_hash, "shard-y");
    vec![a1, b1, a2, b2, a3]
}

fn run_ingest(dir: &std::path::Path, batches: &[pb::MeasurementBatch], verify_key: &EcKey<Public>) {
    let mut ingest = Ingest::new(dir, None);
    ingest.register_producer(policy("producer-a"));
    ingest.register_producer(policy("producer-b"));
    for b in batches {
        let outcome = ingest.submit(b, Signer::Key(verify_key.clone()), b.batch_tai_ns).unwrap();
        assert!(outcome.accepted(), "fixture batch must be accepted in both runs: {outcome:?}");
    }
}

fn sha256_hex_of_file(path: &std::path::Path) -> String {
    let bytes = std::fs::read(path).unwrap();
    hash::hex_encode(&sha256(&bytes))
}

#[test]
fn two_independent_runs_over_the_same_batches_produce_byte_identical_partition_files() {
    let (signing_key, verify_key) = keys();
    let batches = build_fixed_batches(&signing_key);

    let dir_a = tmp_dir("run-a");
    let dir_b = tmp_dir("run-b");
    run_ingest(&dir_a, &batches, &verify_key);
    run_ingest(&dir_b, &batches, &verify_key);

    for shard in ["shard-x", "shard-y"] {
        let file_a = dir_a.join(format!("{shard}.avlog"));
        let file_b = dir_b.join(format!("{shard}.avlog"));
        assert!(file_a.exists(), "run A must have created {shard}.avlog");
        assert!(file_b.exists(), "run B must have created {shard}.avlog");

        let hash_a = sha256_hex_of_file(&file_a);
        let hash_b = sha256_hex_of_file(&file_b);
        assert_eq!(
            hash_a, hash_b,
            "partition {shard}: two independent runs over the identical, already-signed batch fixture produced different SHA-256 file hashes ({hash_a} vs {hash_b}) -- \
             a difference here means something in the record framing or the canonical MeasurementBatch encoding is not a pure function of the batches' own field values \
             (e.g. HashMap-ordered iteration, or a live clock/random value leaking into what gets written), which would break every downstream consumer that expects two \
             independent ingests of the same evidence to agree byte for byte"
        );

        // Also compare raw bytes directly, for a failure message that does not require
        // trusting the hash function itself if the hashes ever disagreed.
        assert_eq!(std::fs::read(&file_a).unwrap(), std::fs::read(&file_b).unwrap(), "partition {shard}: raw bytes must also be identical, not merely hash-equal");
    }
}
