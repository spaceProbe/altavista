//! Requirement 4: a corrupt record in the *middle* of a partition file -- distinct from
//! the torn-tail case in `tests/crash_recovery.rs` -- is reported by
//! `PartitionLog::verify` as a chain break, and `PartitionLog::open`'s own recovery scan
//! truncates nothing at all (the file's length and every record's framing stay entirely
//! intact; only this one record's *content* was tampered with).

use std::path::PathBuf;

use av_edge::{hash, pb, sign};
use av_ingest::log::PartitionLog;
use openssl::ec::EcKey;
use openssl::pkey::Private;

const TEST_KEY_PEM: &[u8] = include_bytes!("fixtures/test_signing_key.pem");

fn tmp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("av-ingest-test-tamper-{name}-{}", std::process::id()));
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
        measurements: vec![pb::Measurement {
            measurement_id: format!("m{sequence}"),
            shard_key: shard_key.to_string(),
            z: vec![1.0, 2.0, 3.0],
            sensor_id: format!("sensor-{sequence}"),
            ..Default::default()
        }],
        ..Default::default()
    };
    sign::sign_batch(&mut b, prev_hash, key).unwrap();
    b
}

#[test]
fn a_corrupt_middle_record_is_reported_as_a_chain_break_and_nothing_is_truncated() {
    let dir = tmp_dir("middle-corruption");
    let key = signing_key();
    let shard_key = "shard-a";

    let (log, _) = PartitionLog::open(&dir, shard_key).unwrap();
    let b1 = batch(1, hash::GENESIS, shard_key, &key);
    let b2 = batch(2, &b1.batch_hash, shard_key, &key);
    let b3 = batch(3, &b2.batch_hash, shard_key, &key);
    let b4 = batch(4, &b3.batch_hash, shard_key, &key);
    for b in [&b1, &b2, &b3, &b4] {
        log.append(b).unwrap();
    }
    assert_eq!(log.record_count(), 4);
    let path = log.path().to_path_buf();
    drop(log);

    let original_bytes = std::fs::read(&path).unwrap();
    let original_len = original_bytes.len() as u64;

    // Corrupt record #2's payload content in place, without changing the file's total
    // length: find record 2's payload region and flip one byte inside it. Record
    // boundaries are computed independently here (mirroring the module doc's framing,
    // not calling into `PartitionLog`'s own internals) so this test does not merely
    // assume the implementation agrees with itself.
    const HEADER_LEN: usize = 36;
    let p1_len = prost::Message::encode_to_vec(&b1).len();
    let p2_len = prost::Message::encode_to_vec(&b2).len();
    let record2_payload_start = HEADER_LEN + p1_len + HEADER_LEN;
    let mut tampered_bytes = original_bytes.clone();
    // Flip a byte roughly in the middle of record 2's payload (well clear of its own
    // 36-byte header, so the framing -- payload_len and record_hash -- is untouched;
    // only the payload content itself is corrupted, leaving record 2's stored hash
    // stale relative to its own now-different content).
    let flip_at = record2_payload_start + (p2_len / 2);
    tampered_bytes[flip_at] ^= 0xFF;
    std::fs::write(&path, &tampered_bytes).unwrap();
    assert_eq!(tampered_bytes.len() as u64, original_len, "the tamper must not change the file's total length");

    // Reopen: recovery must NOT truncate anything -- the file is structurally complete
    // throughout (only content changed, not framing), so this is not a torn tail.
    let (log2, report) = PartitionLog::open(&dir, shard_key).unwrap();
    assert!(report.is_none(), "a middle-record content tamper must never be reported/truncated by open() -- that is verify()'s job: {report:?}");
    assert_eq!(log2.record_count(), 4, "open() must not have dropped any record");
    assert_eq!(std::fs::metadata(&path).unwrap().len(), original_len, "open() must not have truncated the file at all");

    // verify() must independently detect the break, at record 2, and report it as a
    // content tamper rather than as any kind of truncation.
    let verification = log2.verify().unwrap();
    assert!(!verification.ok, "verify() must detect the tampered middle record");
    assert_eq!(verification.broken_at_sequence, 2, "verify() must name record index 2 -- the exact record that was tampered with");
    assert_eq!(verification.checked, 1, "record 1 (before the break) must still verify as good");
    assert!(verification.detail.contains("tampered"), "{}", verification.detail);

    // The file on disk must still be byte-identical to the tampered version -- verify()
    // is read-only and must not have "fixed" or otherwise touched anything.
    assert_eq!(std::fs::read(&path).unwrap(), tampered_bytes, "verify() must never modify the file it reads");
}
