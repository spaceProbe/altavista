//! Requirement 3: crash mid-append. A run of batches is appended, then the partition file
//! is truncated *inside* its last record to simulate a torn write. Reopening detects the
//! partial record, **reports** the discarded byte count (asserted on the returned
//! `RecoveryReport`, not merely inferred from the end state -- a silent recovery is
//! exactly the defect this test exists to catch), the chain verifies clean to the last
//! complete record, and a subsequent append continues the chain and verifies.

use std::path::PathBuf;

use av_edge::{hash, pb, sign};
use av_ingest::log::PartitionLog;
use openssl::ec::EcKey;
use openssl::pkey::Private;

const TEST_KEY_PEM: &[u8] = include_bytes!("fixtures/test_signing_key.pem");

fn tmp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("av-ingest-test-crash-recovery-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn signing_key() -> EcKey<Private> {
    sign::load_signing_key(TEST_KEY_PEM).unwrap()
}

fn batch(sequence: u64, prev_hash: &[u8], shard_key: &str, key: &EcKey<Private>) -> pb::MeasurementBatch {
    let mut b = pb::MeasurementBatch {
        producer_id: "producer-1".to_string(),
        sequence,
        batch_tai_ns: 1_000,
        shard_key: shard_key.to_string(),
        measurements: vec![pb::Measurement { measurement_id: format!("m{sequence}"), shard_key: shard_key.to_string(), z: vec![1.0, 2.0, 3.0], ..Default::default() }],
        ..Default::default()
    };
    sign::sign_batch(&mut b, prev_hash, key).unwrap();
    b
}

#[test]
fn crash_mid_append_is_detected_reported_excluded_and_recovery_continues_the_chain() {
    let dir = tmp_dir("torn-tail");
    let key = signing_key();
    let shard_key = "shard-a";

    // Write three genuine records.
    let (log, report) = PartitionLog::open(&dir, shard_key).unwrap();
    assert!(report.is_none(), "a fresh partition must recover cleanly with no report");
    let b1 = batch(1, hash::GENESIS, shard_key, &key);
    let b2 = batch(2, &b1.batch_hash, shard_key, &key);
    let b3 = batch(3, &b2.batch_hash, shard_key, &key);
    log.append(&b1).unwrap();
    log.append(&b2).unwrap();
    log.append(&b3).unwrap();
    assert_eq!(log.record_count(), 3);
    let path = log.path().to_path_buf();
    drop(log);

    let full_len = std::fs::metadata(&path).unwrap().len();

    // Simulate a crash mid-write of the third record: truncate to somewhere strictly
    // inside record 3's own bytes (after records 1 and 2 are fully durable). Record 3's
    // payload is non-empty (it carries a Measurement with real fields), so truncating 10
    // bytes off the end lands inside record 3's payload, never touching records 1/2.
    let torn_len = full_len - 10;
    {
        let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        f.set_len(torn_len).unwrap();
    }
    assert_eq!(std::fs::metadata(&path).unwrap().len(), torn_len);

    // Reopen: recovery must detect the torn tail, report it, and truncate to the last
    // complete record (record 2's own end).
    let (log2, report2) = PartitionLog::open(&dir, shard_key).unwrap();
    let report2 = report2.expect("recovery must report a discard for a torn trailing record");
    assert_eq!(report2.discarded_bytes, torn_len - record2_end(&b1, &b2) as u64, "reported discard must be exactly the torn record's own bytes, not more and not less");
    assert!(report2.reason.contains("incomplete"), "the reason must say the tail was incomplete: {}", report2.reason);

    assert_eq!(log2.record_count(), 2, "only the two complete records must remain after recovery");
    // The recovered tip must be the *partition log's own* record_hash for record 2 -- an
    // independent chain from `b2.batch_hash` (that batch's own per-producer signing
    // hash; see log.rs's module doc, "Two chains, deliberately"), computed here the same
    // way `log.rs` documents: SHA-256(prev_record_hash || payload), chained from GENESIS.
    let payload1 = prost::Message::encode_to_vec(&b1);
    let record1_hash = hash::compute_batch_hash(hash::GENESIS, &payload1);
    let payload2 = prost::Message::encode_to_vec(&b2);
    let record2_hash = hash::compute_batch_hash(&record1_hash, &payload2);
    assert_eq!(log2.tip_hash(), record2_hash.to_vec(), "the recovered tip must be record 2's own partition-log record_hash");
    assert_eq!(std::fs::metadata(&path).unwrap().len(), record2_end(&b1, &b2) as u64, "the file must be physically truncated to the last complete record");

    let verify_before_append = log2.verify().unwrap();
    assert!(verify_before_append.ok, "{verify_before_append:?}");
    assert_eq!(verify_before_append.checked, 2);

    // A subsequent append continues the chain from the recovered tip and verifies clean.
    let b3_retry = batch(3, &b2.batch_hash, shard_key, &key);
    log2.append(&b3_retry).unwrap();
    assert_eq!(log2.record_count(), 3);
    let verify_after = log2.verify().unwrap();
    assert!(verify_after.ok, "{verify_after:?}");
    assert_eq!(verify_after.checked, 3);
}

/// Recomputes the byte offset where record 2 ends, independently of `PartitionLog`'s own
/// internals, so the test's "exactly how many bytes were discarded" assertion is checked
/// against a value computed the same way the module doc describes the framing, not
/// against whatever `PartitionLog` itself happens to report elsewhere.
fn record2_end(b1: &pb::MeasurementBatch, b2: &pb::MeasurementBatch) -> usize {
    const HEADER_LEN: usize = 36;
    let p1 = prost::Message::encode_to_vec(b1);
    let p2 = prost::Message::encode_to_vec(b2);
    HEADER_LEN + p1.len() + HEADER_LEN + p2.len()
}

#[test]
fn crash_recovery_on_a_single_record_file_discards_everything_and_starts_from_genesis() {
    let dir = tmp_dir("single-record-torn");
    let key = signing_key();
    let shard_key = "shard-a";

    let (log, _) = PartitionLog::open(&dir, shard_key).unwrap();
    let b1 = batch(1, hash::GENESIS, shard_key, &key);
    log.append(&b1).unwrap();
    let path = log.path().to_path_buf();
    drop(log);

    let full_len = std::fs::metadata(&path).unwrap().len();
    let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    f.set_len(full_len - 5).unwrap();
    drop(f);

    let (log2, report) = PartitionLog::open(&dir, shard_key).unwrap();
    let report = report.expect("a torn single record must still be reported");
    assert_eq!(report.discarded_bytes, full_len - 5);
    assert_eq!(log2.record_count(), 0);
    assert_eq!(log2.tip_hash(), hash::GENESIS);

    let b1_retry = batch(1, hash::GENESIS, shard_key, &key);
    log2.append(&b1_retry).unwrap();
    let verify = log2.verify().unwrap();
    assert!(verify.ok, "{verify:?}");
    assert_eq!(verify.checked, 1);
}
