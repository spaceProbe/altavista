//! Requirement 2 (question 202's charter): the plugin manifest handshake, through the
//! wire. A good `Announce`/`Submit` handshake; `Submit` without a prior `Announce`
//! refused; a conflicting second `Announce` refused; the chain head `ManifestAck` returns
//! matches what the ledger holds.
//!
//! This deployment configuration runs with `require_client_certificate: false` (this
//! file's own concern is the manifest handshake's *own* logic, not identity -- see
//! `tests/wire_identity.rs` for the forwarded-certificate path), so every producer's
//! verifying key comes from `EdgeIngestConfig::verify_keys` -- E1's own no-certificate-
//! in-the-loop convention, unchanged by the wire.

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

fn tmp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("av-ingest-test-wire-manifest-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn keys() -> (EcKey<Private>, EcKey<Public>) {
    (
        sign::load_signing_key(include_bytes!("fixtures/test_signing_key.pem")).unwrap(),
        av_edge::verify::load_verifying_key(include_bytes!("fixtures/test_signing_key.pub.pem")).unwrap(),
    )
}

const NOW: i64 = 1_000_000_000;

fn config_with_verify_key(producer_id: &str, key: EcKey<Public>) -> EdgeIngestConfig {
    let mut config = EdgeIngestConfig::new(vec!["UNCLASSIFIED".to_string(), "CUI".to_string()], 5_000_000_000);
    config.require_client_certificate = false;
    config.verify_keys.insert(producer_id.to_string(), key);
    config
}

/// Starts an `EdgeIngestService` on an ephemeral loopback port and returns a connected
/// [`EdgeIngestClient`] plus the service handle (for `evidence_snapshot`/`verify_snapshot`
/// assertions independent of the wire). The server task is detached (aborted when the
/// test process exits) -- matching this crate's other tests' "one temp directory per test,
/// ephemeral port, no fixed port" hygiene rule.
async fn start_server(dir: &std::path::Path, config: EdgeIngestConfig) -> (EdgeIngestClient, Arc<EdgeIngestService>) {
    let service = Arc::new(EdgeIngestService::new(dir, None, config, Arc::new(|| NOW)));
    let listener = av_ingest::server::bind_loopback("127.0.0.1:0").await.expect("127.0.0.1:0 must bind");
    let addr: SocketAddr = listener.local_addr().unwrap();
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);

    let served = service.clone();
    tokio::spawn(async move {
        let _ = Server::builder().add_service(EdgeIngestServer::from_arc(served)).serve_with_incoming(incoming).await;
    });

    // Poll until the port actually accepts connections -- never a bare sleep (test
    // hygiene): a fresh TCP connect attempt either succeeds (server ready) or fails fast
    // (ECONNREFUSED), so this loop converges in well under the deadline on any sane host.
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
    loop {
        if tokio::net::TcpStream::connect(addr).await.is_ok() {
            break;
        }
        if tokio::time::Instant::now() > deadline {
            panic!("server at {addr} never became ready within the deadline");
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
    }

    let client = EdgeIngestClient::connect_plaintext_addr(addr).await.expect("connecting to a just-verified-ready server must succeed");
    (client, service)
}

fn manifest(producer_id: &str) -> pb::PluginManifest {
    pb::PluginManifest {
        producer_id: producer_id.to_string(),
        plugin_version: "1.0.0".to_string(),
        output_schemas: vec![pb::MeasurementSchema { measurement_id: "m1".to_string(), sensor_id: "s1".to_string(), z_len: 3 }],
        frame_ids: vec!["ICRF".to_string()],
        label: Some(pb::Label { marking: "CUI".to_string(), caveats: vec![] }),
        clearance: "CUI".to_string(),
        shard_keys: vec!["shard-a".to_string()],
        leaf_fingerprint_sha256: String::new(),
    }
}

#[tokio::test]
async fn a_good_handshake_is_accepted_and_the_chain_head_starts_at_genesis() {
    let dir = tmp_dir("good-handshake");
    let (_signing_key, verify_key) = keys();
    let (mut client, _service) = start_server(&dir, config_with_verify_key("producer-1", verify_key)).await;

    let ack = client.announce(manifest("producer-1")).await.expect("Announce must succeed over the wire");
    assert!(ack.accepted, "{ack:?}");
    assert_eq!(ack.refusal, pb::ManifestRefusal::Unspecified as i32);
    assert_eq!(ack.chain_head, hash::GENESIS, "a producer with no prior accepted batch must chain from GENESIS");
}

#[tokio::test]
async fn submit_without_announce_is_refused() {
    let dir = tmp_dir("submit-without-announce");
    let (_signing_key, verify_key) = keys();
    let (mut client, _service) = start_server(&dir, config_with_verify_key("producer-1", verify_key)).await;

    let mut batch = pb::MeasurementBatch {
        producer_id: "producer-1".to_string(),
        sequence: 1,
        label: Some(pb::Label { marking: "CUI".to_string(), caveats: vec![] }),
        batch_tai_ns: NOW,
        shard_key: "shard-a".to_string(),
        ..Default::default()
    };
    let (signing_key, _) = keys();
    sign::sign_batch(&mut batch, hash::GENESIS, &signing_key).unwrap();

    let err = client.submit_batches(vec![batch]).await.unwrap_err();
    assert_eq!(err.code(), tonic::Code::FailedPrecondition, "{err:?}");
}

#[tokio::test]
async fn a_conflicting_second_manifest_is_refused() {
    let dir = tmp_dir("conflicting-manifest");
    let (_signing_key, verify_key) = keys();
    let (mut client, _service) = start_server(&dir, config_with_verify_key("producer-1", verify_key)).await;

    let ack1 = client.announce(manifest("producer-1")).await.unwrap();
    assert!(ack1.accepted, "{ack1:?}");

    let mut conflicting = manifest("producer-1");
    conflicting.clearance = "SECRET".to_string(); // this deployment's ladder does not even have SECRET
    let ack2 = client.announce(conflicting.clone()).await.unwrap();
    assert!(!ack2.accepted, "{ack2:?}");
    // clearance is checked BEFORE the mismatch check in this service's own documented
    // order (`av_ingest::service`'s module doc, step 4 before step 5), so this
    // particular conflict surfaces as CLEARANCE_ABOVE_LADDER, not MANIFEST_MISMATCH --
    // a genuinely different-but-still-valid manifest is what step 5 exists to catch.
    assert_eq!(ack2.refusal, pb::ManifestRefusal::ClearanceAboveLadder as i32, "{ack2:?}");

    // A manifest that IS on the ladder, but differs from the first accepted one, must
    // hit MANIFEST_MISMATCH instead.
    let mut different_but_valid = manifest("producer-1");
    different_but_valid.plugin_version = "2.0.0".to_string();
    let ack3 = client.announce(different_but_valid).await.unwrap();
    assert!(!ack3.accepted, "{ack3:?}");
    assert_eq!(ack3.refusal, pb::ManifestRefusal::ManifestMismatch as i32, "{ack3:?}");

    // Re-announcing the EXACT same, already-accepted manifest is idempotent, not refused.
    let ack4 = client.announce(manifest("producer-1")).await.unwrap();
    assert!(ack4.accepted, "a repeat of the exact same already-accepted manifest must be idempotent: {ack4:?}");
}

#[tokio::test]
async fn the_chain_head_in_manifest_ack_matches_what_the_ledger_holds_after_batches() {
    let dir = tmp_dir("chain-head-matches-ledger");
    let (signing_key, verify_key) = keys();
    let (mut client, service) = start_server(&dir, config_with_verify_key("producer-1", verify_key)).await;

    client.announce(manifest("producer-1")).await.unwrap();

    let mut b1 = pb::MeasurementBatch {
        producer_id: "producer-1".to_string(),
        sequence: 1,
        label: Some(pb::Label { marking: "CUI".to_string(), caveats: vec![] }),
        batch_tai_ns: NOW,
        shard_key: "shard-a".to_string(),
        ..Default::default()
    };
    sign::sign_batch(&mut b1, hash::GENESIS, &signing_key).unwrap();
    let verdicts = client.submit_batches(vec![b1.clone()]).await.unwrap();
    assert_eq!(verdicts.len(), 1);
    assert!(verdicts[0].accepted, "{:?}", verdicts[0]);

    // Reconnecting and announcing again must report the batch's own hash as the chain
    // head -- exactly what the in-process producer_counters (via the shared service
    // handle) independently reports.
    let ack = client.announce(manifest("producer-1")).await.unwrap();
    assert!(ack.accepted, "{ack:?}");
    assert_eq!(ack.chain_head, b1.batch_hash, "ManifestAck.chain_head must match the last accepted batch's own hash");

    let independent = service.evidence_snapshot();
    let counters = independent.producers.get("producer-1").expect("producer-1 must have counters after one accepted batch");
    assert_eq!(counters.chain_head, b1.batch_hash, "the in-process evidence snapshot must agree with what ManifestAck reported over the wire");
}
