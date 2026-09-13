//! Requirement 1 (`docs/edge-plan.md` milestone E3's own headline test, question 202):
//! every one of the **nine** rejection kinds -- `UNSIGNED`, `BAD_SIGNATURE`, `CHAIN_GAP`,
//! `CHAIN_BREAK`, `MISLABELED`, `OVER_CLEARANCE`, `STALE`, `DUPLICATE`, `SHARD_MISMATCH`
//! -- is produced **through the wire** (a real `tonic` client dialling a real, ephemeral-
//! port `EdgeIngest` server; never the in-process `Ingest::submit` call `tests/
//! rejections.rs` already covers) by one deliberate corruption each, each one landing in
//! the right `RejectionCounters` field read back over `GetEvidence`.
//!
//! Mirrors `tests/rejections.rs`'s own structure (a `setup()` producing one already-
//! accepted first batch, then one corruption per test derived from it) with everything
//! from `Announce` onward going over a real loopback socket.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use av_edge::{hash, pb, sign};
use av_ingest::pb::edge_ingest_server::EdgeIngestServer;
use av_ingest::service::{EdgeIngestConfig, EdgeIngestService};
use av_ingest_client::EdgeIngestClient;
use openssl::ec::EcKey;
use openssl::pkey::{Private, Public};
use tonic::transport::Server;

const NOW: i64 = 1_000;
const MAX_AGE_NS: i64 = 5_000_000_000; // 5s, matching tests/rejections.rs's own policy.

fn tmp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("av-ingest-test-wire-rejections-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn keys() -> (EcKey<Private>, EcKey<Public>) {
    (
        sign::load_signing_key(include_bytes!("fixtures/test_signing_key.pem")).unwrap(),
        av_edge::verify::load_verifying_key(include_bytes!("fixtures/test_signing_key.pub.pem")).unwrap(),
    )
}

async fn poll_until_ready(addr: SocketAddr) {
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
    loop {
        if tokio::net::TcpStream::connect(addr).await.is_ok() {
            return;
        }
        if tokio::time::Instant::now() > deadline {
            panic!("server at {addr} never became ready within the deadline");
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
    }
}

async fn start_server(dir: &std::path::Path, config: EdgeIngestConfig) -> (EdgeIngestClient, Arc<EdgeIngestService>) {
    let service = Arc::new(EdgeIngestService::new(dir, None, config, Arc::new(|| NOW)));
    let listener = av_ingest::server::bind_loopback("127.0.0.1:0").await.expect("127.0.0.1:0 must bind");
    let addr = listener.local_addr().unwrap();
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
    let served = service.clone();
    tokio::spawn(async move {
        let _ = Server::builder().add_service(EdgeIngestServer::from_arc(served)).serve_with_incoming(incoming).await;
    });
    poll_until_ready(addr).await;
    let client = EdgeIngestClient::connect_plaintext_addr(addr).await.expect("connecting to a just-verified-ready server must succeed");
    (client, service)
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

fn manifest(clearance: &str, marking: &str) -> pb::PluginManifest {
    pb::PluginManifest {
        producer_id: "producer-1".to_string(),
        label: Some(pb::Label { marking: marking.to_string(), caveats: vec!["SP-EXPT".to_string()] }),
        clearance: clearance.to_string(),
        shard_keys: vec!["shard-a".to_string()],
        ..Default::default()
    }
}

/// Sets up one server with `producer-1` announced (label CUI/SP-EXPT, clearance CUI),
/// submits one genuine first batch (accepted), and returns the client, service, that
/// accepted batch (for its `batch_hash`), the signing key, and the shard key every test
/// below uses. `name` must be unique per caller -- `#[tokio::test]` functions run
/// concurrently by default, on separate ports (ephemeral, never fixed) but sharing the
/// host's `/tmp`, so a shared temp-directory name here would let two tests race on the
/// same partition file.
async fn setup(name: &str) -> (EdgeIngestClient, Arc<EdgeIngestService>, pb::MeasurementBatch, EcKey<Private>, String) {
    let dir = tmp_dir(name);
    let (signing_key, verify_key) = keys();
    let mut config = EdgeIngestConfig::new(vec!["UNCLASSIFIED".to_string(), "CUI".to_string(), "SECRET".to_string()], MAX_AGE_NS);
    config.require_client_certificate = false;
    config.verify_keys.insert("producer-1".to_string(), verify_key);
    let (mut client, service) = start_server(&dir, config).await;

    let ack = client.announce(manifest("CUI", "CUI")).await.unwrap();
    assert!(ack.accepted, "{ack:?}");

    let shard_key = "shard-a".to_string();
    let b1 = valid_batch(1, hash::GENESIS, NOW, &shard_key, &signing_key);
    let verdicts = client.submit_batches(vec![b1.clone()]).await.unwrap();
    assert!(verdicts[0].accepted, "{:?}", verdicts[0]);

    (client, service, b1, signing_key, shard_key)
}

/// Submits `batch` and asserts it was rejected as `expected` over the wire, that exactly
/// that counter moved (all nine tracked independently, read back over `GetEvidence`), and
/// that the partition's own `record_count` (also read over `GetEvidence`) did not grow.
/// Both counter reads go over the wire (`GetEvidence`), never through the in-process
/// `EdgeIngestService::evidence_snapshot`: this file's claim is that the nine rejection
/// kinds are produced *and observed* across a real socket, and an in-process read here
/// would leave the observation half of that claim untested. (`tests/wire_evidence.rs`
/// separately proves the wire and in-process surfaces agree on content.)
async fn assert_single_rejection_over_the_wire(client: &mut EdgeIngestClient, batch: &pb::MeasurementBatch, shard_key: &str, expected: pb::BatchRejection) {
    let before = client.get_evidence().await.expect("GetEvidence over the wire");
    let before_counters = before.producers.get("producer-1").cloned().unwrap_or_default();
    let before_record_count = before.partitions.get(shard_key).map(|p| p.record_count).unwrap_or(0);

    let verdicts = client.submit_batches(vec![batch.clone()]).await.expect("Submit itself must not fail at the RPC level for a business-logic rejection");
    assert_eq!(verdicts.len(), 1);
    let verdict = &verdicts[0];
    assert!(!verdict.accepted, "{verdict:?}");
    assert_eq!(verdict.rejection, expected as i32, "{verdict:?}");

    let after = client.get_evidence().await.expect("GetEvidence over the wire");
    let after_counters = after.producers.get("producer-1").cloned().unwrap_or_default();
    let after_record_count = after.partitions.get(shard_key).map(|p| p.record_count).unwrap_or(0);
    assert_eq!(after_counters.accepted, before_counters.accepted, "accepted must not change");

    let fields: [(&str, u64, u64, pb::BatchRejection); 9] = [
        ("unsigned_count", before_counters.unsigned_count, after_counters.unsigned_count, pb::BatchRejection::Unsigned),
        ("bad_signature_count", before_counters.bad_signature_count, after_counters.bad_signature_count, pb::BatchRejection::BadSignature),
        ("chain_gap_count", before_counters.chain_gap_count, after_counters.chain_gap_count, pb::BatchRejection::ChainGap),
        ("chain_break_count", before_counters.chain_break_count, after_counters.chain_break_count, pb::BatchRejection::ChainBreak),
        ("mislabeled_count", before_counters.mislabeled_count, after_counters.mislabeled_count, pb::BatchRejection::Mislabeled),
        ("over_clearance_count", before_counters.over_clearance_count, after_counters.over_clearance_count, pb::BatchRejection::OverClearance),
        ("stale_count", before_counters.stale_count, after_counters.stale_count, pb::BatchRejection::Stale),
        ("duplicate_count", before_counters.duplicate_count, after_counters.duplicate_count, pb::BatchRejection::Duplicate),
        ("shard_mismatch_count", before_counters.shard_mismatch_count, after_counters.shard_mismatch_count, pb::BatchRejection::ShardMismatch),
    ];
    for (name, before_v, after_v, kind) in fields {
        if kind == expected {
            assert_eq!(after_v, before_v + 1, "{name} must increment by exactly one over the wire");
        } else {
            assert_eq!(after_v, before_v, "{name} must not change over the wire");
        }
    }
    assert_eq!(after_record_count, before_record_count, "a rejected batch must never grow the partition's own record_count");
}

#[tokio::test]
async fn unsigned_batch_is_rejected_and_counted_over_the_wire() {
    let (mut client, _service, b1, signing_key, shard_key) = setup("unsigned").await;
    let mut b2 = valid_batch(2, &b1.batch_hash, NOW, &shard_key, &signing_key);
    b2.signature.clear();
    assert_single_rejection_over_the_wire(&mut client, &b2, &shard_key, pb::BatchRejection::Unsigned).await;
}

#[tokio::test]
async fn bad_signature_is_rejected_and_counted_over_the_wire() {
    let (mut client, _service, b1, signing_key, shard_key) = setup("bad-signature").await;
    let mut b2 = valid_batch(2, &b1.batch_hash, NOW, &shard_key, &signing_key);
    b2.signature[0] ^= 0x01;
    assert_single_rejection_over_the_wire(&mut client, &b2, &shard_key, pb::BatchRejection::BadSignature).await;
}

#[tokio::test]
async fn chain_gap_is_rejected_and_counted_over_the_wire() {
    let (mut client, _service, b1, signing_key, shard_key) = setup("chain-gap").await;
    let b3 = valid_batch(3, &b1.batch_hash, NOW, &shard_key, &signing_key); // skips sequence 2
    assert_single_rejection_over_the_wire(&mut client, &b3, &shard_key, pb::BatchRejection::ChainGap).await;
}

#[tokio::test]
async fn chain_break_is_rejected_and_counted_over_the_wire() {
    let (mut client, _service, _b1, signing_key, shard_key) = setup("chain-break").await;
    let wrong_prev = vec![0x42u8; 32];
    let b2 = valid_batch(2, &wrong_prev, NOW, &shard_key, &signing_key);
    assert_single_rejection_over_the_wire(&mut client, &b2, &shard_key, pb::BatchRejection::ChainBreak).await;
}

#[tokio::test]
async fn mislabeled_batch_is_rejected_and_counted_over_the_wire() {
    let (mut client, _service, b1, signing_key, shard_key) = setup("mislabeled").await;
    let mut b2 = pb::MeasurementBatch {
        producer_id: "producer-1".to_string(),
        sequence: 2,
        label: Some(pb::Label { marking: "SECRET".to_string(), caveats: vec!["SP-EXPT".to_string()] }),
        batch_tai_ns: NOW,
        shard_key: shard_key.clone(),
        measurements: vec![pb::Measurement { measurement_id: "m1".to_string(), shard_key: shard_key.clone(), ..Default::default() }],
        ..Default::default()
    };
    sign::sign_batch(&mut b2, &b1.batch_hash, &signing_key).unwrap();
    assert_single_rejection_over_the_wire(&mut client, &b2, &shard_key, pb::BatchRejection::Mislabeled).await;
}

#[tokio::test]
async fn over_clearance_batch_is_rejected_and_counted_over_the_wire() {
    // A distinct manifest from `setup()`'s own: this producer declares it emits under
    // SECRET while its own clearance is only CUI -- the declaration itself is over
    // clearance, exactly mirroring `tests/rejections.rs::over_clearance_batch_is_rejected_
    // and_counted`'s own policy shape.
    let dir = tmp_dir("over-clearance");
    let (signing_key, verify_key) = keys();
    let mut config = EdgeIngestConfig::new(vec!["UNCLASSIFIED".to_string(), "CUI".to_string(), "SECRET".to_string()], MAX_AGE_NS);
    config.require_client_certificate = false;
    config.verify_keys.insert("producer-1".to_string(), verify_key);
    let (mut client, _service) = start_server(&dir, config).await;

    let ack = client.announce(manifest("CUI", "SECRET")).await.unwrap();
    assert!(ack.accepted, "{ack:?}");

    let shard_key = "shard-a".to_string();
    // caveats must match `manifest()`'s own declared ["SP-EXPT"] exactly -- a mismatched
    // caveat set would itself be MISLABELED (checked before OVER_CLEARANCE in
    // `av_edge::policy::ProducerPolicy::classify_label`'s own documented order), which is
    // not the defect this test means to isolate.
    let mut b1 = pb::MeasurementBatch {
        producer_id: "producer-1".to_string(),
        sequence: 1,
        label: Some(pb::Label { marking: "SECRET".to_string(), caveats: vec!["SP-EXPT".to_string()] }),
        batch_tai_ns: NOW,
        shard_key: shard_key.clone(),
        measurements: vec![pb::Measurement { measurement_id: "m1".to_string(), shard_key: shard_key.clone(), ..Default::default() }],
        ..Default::default()
    };
    sign::sign_batch(&mut b1, hash::GENESIS, &signing_key).unwrap();

    assert_single_rejection_over_the_wire(&mut client, &b1, &shard_key, pb::BatchRejection::OverClearance).await;
}

#[tokio::test]
async fn stale_batch_is_rejected_and_counted_over_the_wire() {
    let (mut client, _service, b1, signing_key, shard_key) = setup("stale").await;
    let old_epoch = NOW - 10_000_000_000; // max_age_ns is 5s
    let b2 = valid_batch(2, &b1.batch_hash, old_epoch, &shard_key, &signing_key);
    assert_single_rejection_over_the_wire(&mut client, &b2, &shard_key, pb::BatchRejection::Stale).await;
}

#[tokio::test]
async fn duplicate_batch_is_rejected_and_counted_over_the_wire() {
    let (mut client, _service, b1, _signing_key, shard_key) = setup("duplicate").await;
    assert_single_rejection_over_the_wire(&mut client, &b1, &shard_key, pb::BatchRejection::Duplicate).await;
}

#[tokio::test]
async fn shard_mismatch_batch_is_rejected_and_counted_over_the_wire() {
    let (mut client, _service, b1, signing_key, shard_key) = setup("shard-mismatch").await;
    let mut b2 = pb::MeasurementBatch {
        producer_id: "producer-1".to_string(),
        sequence: 2,
        label: Some(pb::Label { marking: "CUI".to_string(), caveats: vec!["SP-EXPT".to_string()] }),
        batch_tai_ns: NOW,
        shard_key: shard_key.clone(),
        measurements: vec![pb::Measurement { measurement_id: "m1".to_string(), shard_key: "shard-b".to_string(), ..Default::default() }],
        ..Default::default()
    };
    sign::sign_batch(&mut b2, &b1.batch_hash, &signing_key).unwrap();
    assert_single_rejection_over_the_wire(&mut client, &b2, &shard_key, pb::BatchRejection::ShardMismatch).await;
}
