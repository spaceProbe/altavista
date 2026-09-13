//! Requirement 5 (question 202's charter): `GetEvidence`/`VerifyLedger` over the wire, and
//! the hand-rolled `GET /admin/api/evidence`/`.../verify` HTTP surface, return the same
//! content the in-process `evidence::evidence`/`evidence::verify_all` return -- asserted
//! on content, not merely on status.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use av_edge::{hash, pb, sign};
use av_ingest::admin::AdminState;
use av_ingest::pb::edge_ingest_server::EdgeIngestServer;
use av_ingest::service::{EdgeIngestConfig, EdgeIngestService};
use av_ingest_client::EdgeIngestClient;
use openssl::ec::EcKey;
use openssl::pkey::{Private, Public};
use tonic::transport::Server;

fn tmp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("av-ingest-test-wire-evidence-{name}-{}", std::process::id()));
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

async fn poll_until_ready(addr: SocketAddr) {
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
    loop {
        if tokio::net::TcpStream::connect(addr).await.is_ok() {
            return;
        }
        if tokio::time::Instant::now() > deadline {
            panic!("nothing at {addr} became ready within the deadline");
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
    }
}

/// Starts both the gRPC `EdgeIngestService` and the `/admin/api/evidence` HTTP surface,
/// sharing exactly one `Ingest` (`EdgeIngestService::ingest_handle`) -- see
/// `crate::service::Handshake`'s own module doc for why that sharing is even possible.
async fn start_grpc_and_admin(dir: &std::path::Path, config: EdgeIngestConfig) -> (EdgeIngestClient, SocketAddr, Arc<EdgeIngestService>) {
    let service = Arc::new(EdgeIngestService::new(dir, None, config, Arc::new(|| NOW)));

    let grpc_listener = av_ingest::server::bind_loopback("127.0.0.1:0").await.unwrap();
    let grpc_addr = grpc_listener.local_addr().unwrap();
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(grpc_listener);
    let served = service.clone();
    tokio::spawn(async move {
        let _ = Server::builder().add_service(EdgeIngestServer::from_arc(served)).serve_with_incoming(incoming).await;
    });

    let admin_state = Arc::new(AdminState { ingest: service.ingest_handle() });
    let admin_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let admin_addr = admin_listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = av_ingest::admin::serve_on(admin_listener, admin_state).await;
    });

    poll_until_ready(grpc_addr).await;
    poll_until_ready(admin_addr).await;

    let client = EdgeIngestClient::connect_plaintext_addr(grpc_addr).await.unwrap();
    (client, admin_addr, service)
}

fn manifest(producer_id: &str) -> pb::PluginManifest {
    pb::PluginManifest {
        producer_id: producer_id.to_string(),
        label: Some(pb::Label { marking: "CUI".to_string(), caveats: vec![] }),
        clearance: "CUI".to_string(),
        shard_keys: vec!["shard-a".to_string()],
        ..Default::default()
    }
}

/// Issues one `GET` against the admin HTTP surface and parses its JSON body. **Must** use
/// `tokio::net::TcpStream`, not `std::net::TcpStream` -- an earlier draft used the
/// blocking standard-library socket directly from inside an `async fn` test body, which
/// hung indefinitely: `#[tokio::test]`'s default runtime is single-threaded, so a
/// synchronous, blocking `read_to_string` call never yields back to that one thread's
/// executor, and the `tokio::spawn`ed `admin::serve_on` task this same thread also owns
/// then never gets polled to actually accept the connection this function just opened --
/// a genuine deadlock, root-caused to exactly that (confirmed by observing the test hang
/// past its 120s harness timeout and killing it by hand), not a flaky timing issue.
async fn get(addr: SocketAddr, path: &str) -> serde_json::Value {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let mut stream = tokio::net::TcpStream::connect(addr).await.expect("connecting to the admin HTTP surface must succeed");
    stream.write_all(format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.expect("reading the admin HTTP response must succeed");
    let body_start = response.find("\r\n\r\n").expect("a well-formed HTTP response has a blank-line body separator") + 4;
    serde_json::from_str(&response[body_start..]).expect("the admin body must be valid JSON")
}

#[tokio::test]
async fn get_evidence_over_the_wire_matches_the_in_process_evidence_surface() {
    let dir = tmp_dir("get-evidence-matches");
    let (signing_key, verify_key) = keys();
    let mut config = EdgeIngestConfig::new(vec!["UNCLASSIFIED".to_string(), "CUI".to_string()], 5_000_000_000);
    config.require_client_certificate = false;
    config.verify_keys.insert("producer-1".to_string(), verify_key);
    let (mut client, admin_addr, service) = start_grpc_and_admin(&dir, config).await;

    client.announce(manifest("producer-1")).await.unwrap();
    let mut b1 = pb::MeasurementBatch {
        producer_id: "producer-1".to_string(),
        sequence: 1,
        label: Some(pb::Label { marking: "CUI".to_string(), caveats: vec![] }),
        batch_tai_ns: NOW,
        shard_key: "shard-a".to_string(),
        measurements: vec![pb::Measurement { measurement_id: "m1".to_string(), shard_key: "shard-a".to_string(), ..Default::default() }],
        ..Default::default()
    };
    sign::sign_batch(&mut b1, hash::GENESIS, &signing_key).unwrap();
    let verdicts = client.submit_batches(vec![b1.clone()]).await.unwrap();
    assert!(verdicts[0].accepted, "{:?}", verdicts[0]);

    // Independent, in-process ground truth: the exact function the acceptance test names.
    let ingest_handle = service.ingest_handle();
    let json = {
        let ingest = ingest_handle.lock().unwrap_or_else(|p| p.into_inner());
        av_ingest::evidence::evidence(&ingest)
    };

    // 1. The gRPC GetEvidence RPC.
    let wire = client.get_evidence().await.unwrap();
    assert_eq!(wire.accepted_total, json["accepted_total"].as_u64().unwrap());
    assert_eq!(wire.rejected_total, json["rejected_total"].as_u64().unwrap());
    let wire_counters = wire.producers.get("producer-1").expect("producer-1 must be present over the wire");
    assert_eq!(wire_counters.accepted, json["producers"]["producer-1"]["accepted"].as_u64().unwrap());
    assert_eq!(wire_counters.shard_mismatch_count, json["producers"]["producer-1"]["shard_mismatch_count"].as_u64().unwrap());
    assert_eq!(hash::hex_encode(&wire_counters.chain_head), json["producers"]["producer-1"]["chain_head"].as_str().unwrap());
    let wire_partition = wire.partitions.get("shard-a").expect("shard-a must be present over the wire");
    assert_eq!(hash::hex_encode(&wire_partition.chain_head), json["partitions"]["shard-a"]["chain_head"].as_str().unwrap());
    assert_eq!(wire_partition.record_count, json["partitions"]["shard-a"]["record_count"].as_u64().unwrap());

    // 2. The gRPC VerifyLedger RPC, against evidence::verify_all directly (not the JSON
    // shape, since VerifyResponse's own message type is ChainVerification already).
    let independent_verify = {
        let ingest = ingest_handle.lock().unwrap_or_else(|p| p.into_inner());
        av_ingest::evidence::verify_all(&ingest)
    };
    let wire_verify = client.verify_ledger().await.unwrap();
    let independent_shard_a = &independent_verify["shard-a"];
    let wire_shard_a = wire_verify.partitions.get("shard-a").unwrap();
    assert_eq!(wire_shard_a.ok, independent_shard_a.ok);
    assert_eq!(wire_shard_a.checked, independent_shard_a.checked);
    assert!(independent_shard_a.ok, "{independent_shard_a:?}");

    // 3. The hand-rolled GET /admin/api/evidence -- byte-for-byte the same JSON `evidence`
    // itself produces (same function, called from admin.rs -- see that module's doc), so
    // this is really proving admin.rs actually calls it correctly end to end over HTTP.
    let admin_json = get(admin_addr, "/admin/api/evidence").await;
    assert_eq!(admin_json, json, "GET /admin/api/evidence must return exactly evidence::evidence()'s own JSON");

    // 4. GET /admin/api/evidence/verify: same shard-a content as VerifyLedger reported.
    let admin_verify = get(admin_addr, "/admin/api/evidence/verify").await;
    assert_eq!(admin_verify["shard-a"]["ok"], serde_json::json!(true));
    assert_eq!(admin_verify["shard-a"]["checked"], serde_json::json!(wire_shard_a.checked));
}

#[tokio::test]
async fn admin_evidence_surface_refuses_non_get_and_unknown_paths() {
    let dir = tmp_dir("admin-404-405");
    let (_signing_key, verify_key) = keys();
    let mut config = EdgeIngestConfig::new(vec!["UNCLASSIFIED".to_string()], 5_000_000_000);
    config.require_client_certificate = false;
    config.verify_keys.insert("producer-1".to_string(), verify_key);
    let (_client, admin_addr, _service) = start_grpc_and_admin(&dir, config).await;

    let body = get(admin_addr, "/not/a/real/path").await;
    assert_eq!(body["error"], serde_json::json!("not found"));

    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let mut stream = tokio::net::TcpStream::connect(admin_addr).await.unwrap();
    stream.write_all(b"POST /admin/api/evidence HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").await.unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    assert!(response.starts_with("HTTP/1.1 405"), "{response}");
}
