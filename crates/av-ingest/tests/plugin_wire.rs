//! Milestone E4's own acceptance test list, the wire half: "a run's measurements arrive at
//! the ingest byte for byte, with the batch count and the chain head pinned for the demo
//! DRM" -- through a real `EdgeIngest` server on an ephemeral loopback port (the pure,
//! no-wire half lives in `crates/av-edge/tests/plugin_replay.rs`, which this file
//! deliberately does not duplicate the decode-correctness assertions of -- see that file for
//! the byte-for-byte-against-`av_kernel::codec` cross-check and the decoded-vs-truth
//! deviation measurement).
//!
//! This is the headline artifact of E4a: the plugin's own signed batches announce and
//! submit against a real server, every batch is accepted, `GetEvidence` agrees with the
//! pinned batch count with every rejection counter at zero, `VerifyLedger`'s own chain head
//! matches the pinned value, and the durable per-partition log -- read back off disk,
//! independent of any in-memory state -- holds the exact measurements the plugin's source
//! produced, byte for byte.
//!
//! Placed here, not on `av-edge`'s own crate: `av-edge` must stay transport-free (this
//! crate's own task brief), so a wire test naming a real `EdgeIngestServer`/`EdgeIngestClient`
//! belongs wherever the dependency graph already allows both -- `crates/av-ingest` already
//! depends on `av-edge` and already dev-depends on `av-ingest-client` (`tests/wire_evidence.rs`
//! established this same pattern for E3b; this file follows it).
//!
//! No Docker, no network beyond this test's own ephemeral loopback socket (question 154).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use av_edge::pb;
use av_edge::plugin::{self, BatchBuilder, BatchingRule, MeasurementSource, Pacing, PluginConfig, PortTrafficSource};
use av_edge::{hash, sign};
use av_ingest::pb::edge_ingest_server::EdgeIngestServer;
use av_ingest::service::{EdgeIngestConfig, EdgeIngestService};
use av_ingest_client::EdgeIngestClient;
use tonic::transport::Server;

const TEST_KEY_PEM: &[u8] = include_bytes!("fixtures/test_signing_key.pem");

const EXPECTED_BATCH_COUNT: usize = 900;
/// Must agree with `crates/av-edge/tests/plugin_replay.rs`'s own identical constant --
/// both are the same 900-batch chain, built from the same committed fixture, the same
/// `PluginConfig`, and the same test signing key. Duplicated (not shared via a common
/// crate) because these two files live in two different crates with no existing shared
/// test-utility crate between them; `crates/av-edge/tests/fixtures/ground_segment/
/// README.md` is the one place both are recorded together.
const EXPECTED_CHAIN_HEAD_HEX: &str = "d1d80d0b6cc9228aaa7479864b8a89f18be88382fb3772bd3c4cc3d7c2dc2698";

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../av-edge/tests/fixtures/ground_segment")
}

fn load_run_products() -> pb::RunProducts {
    let bytes = std::fs::read(fixtures_dir().join("run_products.pb")).expect("reading run_products.pb");
    <pb::RunProducts as prost::Message>::decode(bytes.as_slice()).expect("decodes")
}

fn load_verified_log(expected_hash: &str) -> pb::PortTrafficLog {
    let bytes = std::fs::read(fixtures_dir().join("port_traffic.pb")).expect("reading port_traffic.pb");
    plugin::verify_port_traffic_log(&bytes, expected_hash).expect("hash-verified sidecar")
}

/// Identical to `crates/av-edge/tests/plugin_replay.rs::flight_codec`/`config` -- see that
/// file's own comments for exactly where every value comes from
/// (`drms/demo_ground_segment_flight.system.yaml`).
fn flight_codec() -> pb::PacketCodec {
    let f = |name: &str, bit_offset: u32| pb::PacketField {
        name: name.to_string(),
        bit_offset,
        bit_width: 64,
        r#type: pb::PacketFieldType::Float64 as i32,
        unit: pb::Unit::Meter as i32,
        scale: 1.0,
        offset: 0.0,
        target: String::new(),
    };
    pb::PacketCodec {
        id: "flight_tm_out_codec".to_string(),
        apid: 500,
        is_command: false,
        secondary_header_bytes: 0,
        user_data_bytes: 24,
        description: "own Earth-fixed Cartesian position telemetry, encoded (M25.1)".to_string(),
        fields: vec![f("x", 0), f("y", 64), f("z", 128)],
    }
}

fn config() -> PluginConfig {
    let codec = flight_codec();
    let label = pb::Label { marking: "CUI".to_string(), caveats: vec![] };
    PluginConfig {
        producer_id: "demo-ground-segment-flight-plugin".to_string(),
        plugin_version: "0.1.0".to_string(),
        instance: "flight".to_string(),
        port: "tm_out".to_string(),
        direction: pb::PortDirection::Out as i32,
        codec_bytes: PluginConfig::encode_codec(&codec),
        component_fields: vec!["x".to_string(), "y".to_string(), "z".to_string()],
        frame_id: "earth_fixed_demo_frame".to_string(),
        sensor_id: "ground-segment-flight".to_string(),
        measurement_id: "flight_position".to_string(),
        shard_key: "ground-segment-demo".to_string(),
        noise_r: vec![100.0, 0.0, 0.0, 0.0, 100.0, 0.0, 0.0, 0.0, 100.0],
        label_bytes: PluginConfig::encode_label(&label),
        clearance: "CUI".to_string(),
        leaf_fingerprint_sha256: String::new(),
        batching: BatchingRule::PerEpoch,
        pacing: Pacing::AsFastAsPossible,
    }
}

const NOW: i64 = 1_767_225_637_000_000_000; // the fixture's own start_tai_ns

fn tmp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("av-ingest-test-plugin-wire-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

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

/// Starts a real `EdgeIngestService` on an ephemeral loopback port, with no certificate in
/// the loop (`require_client_certificate = false`) -- E1's own no-certificate-in-the-loop
/// path, exactly like `crates/av-ingest/tests/wire_evidence.rs`'s own fixture, since E2
/// identity issuance is out of this task's own scope (this task builds on top of E1/E2/E3,
/// it does not re-test them).
async fn start_server(dir: &std::path::Path, verify_key: openssl::ec::EcKey<openssl::pkey::Public>, producer_id: &str) -> (EdgeIngestClient, SocketAddr, Arc<EdgeIngestService>) {
    let mut cfg = EdgeIngestConfig::new(vec!["UNCLASSIFIED".to_string(), "CUI".to_string()], 10_000_000_000_000);
    cfg.require_client_certificate = false;
    cfg.verify_keys.insert(producer_id.to_string(), verify_key);
    let service = Arc::new(EdgeIngestService::new(dir, None, cfg, Arc::new(|| NOW)));

    let listener = av_ingest::server::bind_loopback("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
    let served = service.clone();
    tokio::spawn(async move {
        let _ = Server::builder().add_service(EdgeIngestServer::from_arc(served)).serve_with_incoming(incoming).await;
    });
    poll_until_ready(addr).await;

    let client = EdgeIngestClient::connect_plaintext_addr(addr).await.unwrap();
    (client, addr, service)
}

/// Parses `crates/av-ingest/src/log.rs`'s own documented on-disk record framing directly
/// (payload_len: u32 LE, record_hash: [u8; 32], payload: MeasurementBatch bytes), returning
/// every record's raw payload bytes in file order -- this test's own independent read of
/// the durable log, deliberately not going through `PartitionLog`'s in-memory API at all,
/// so "read the durable log back" means what it says: bytes actually on disk.
fn read_partition_payloads(path: &std::path::Path) -> Vec<Vec<u8>> {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos < bytes.len() {
        let payload_len = u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
        let payload_start = pos + 4 + 32;
        let payload_end = payload_start + payload_len;
        out.push(bytes[payload_start..payload_end].to_vec());
        pos = payload_end;
    }
    out
}

#[tokio::test]
async fn the_plugins_batches_are_accepted_over_the_wire_byte_for_byte_and_the_chain_head_matches() {
    let run_products = load_run_products();
    let log = load_verified_log(&run_products.port_traffic_hash);
    let cfg = config();
    let source = PortTrafficSource::from_log(&log, &cfg).expect("source builds");
    assert_eq!(source.groups().len(), EXPECTED_BATCH_COUNT);

    let signing_key = sign::load_signing_key(TEST_KEY_PEM).unwrap();
    let verify_key = av_edge::verify::load_verifying_key(include_bytes!("fixtures/test_signing_key.pub.pem")).unwrap();
    let builder = BatchBuilder::new(cfg.batching).unwrap();
    // Must match crates/av-edge/tests/plugin_replay.rs's own `created_tai_ns` exactly (the
    // RunProducts' own provenance creation time, not this test's `NOW` -- `NOW` is only
    // this server's own injected staleness clock) -- provenance is part of the canonical
    // body bytes, so the two files must agree on it to build the identical pinned chain.
    let provenance = cfg.batch_provenance(&run_products.run_id, run_products.provenance.as_ref().unwrap().created_tai_ns);
    let batches = builder.build_batches_for_config(&source, &cfg, &provenance, &signing_key).unwrap();
    assert_eq!(batches.len(), EXPECTED_BATCH_COUNT);
    assert_eq!(hash::hex_encode(&batches.last().unwrap().batch_hash), EXPECTED_CHAIN_HEAD_HEX, "plugin_replay.rs and this file must build the identical chain from the identical fixture/config/key");

    let dir = tmp_dir("accepted");
    let (mut client, _addr, service) = start_server(&dir, verify_key, &cfg.producer_id).await;

    let manifest = cfg.manifest().unwrap();
    let ack = client.announce(manifest).await.unwrap();
    assert!(ack.accepted, "{ack:?}");
    assert_eq!(ack.chain_head, hash::GENESIS, "a fresh producer's chain head is GENESIS before any batch is submitted");

    let verdicts = client.submit_batches(batches.clone()).await.unwrap();
    assert_eq!(verdicts.len(), EXPECTED_BATCH_COUNT);
    for (i, v) in verdicts.iter().enumerate() {
        assert!(v.accepted, "batch {i} (sequence {}) was rejected: {v:?}", batches[i].sequence);
    }

    // GetEvidence: accepted == pinned batch count, every rejection counter zero.
    let evidence = client.get_evidence().await.unwrap();
    assert_eq!(evidence.accepted_total, EXPECTED_BATCH_COUNT as u64);
    assert_eq!(evidence.rejected_total, 0);
    let counters = evidence.producers.get(&cfg.producer_id).expect("producer must be present");
    assert_eq!(counters.accepted, EXPECTED_BATCH_COUNT as u64);
    for (name, value) in [
        ("unsigned_count", counters.unsigned_count),
        ("bad_signature_count", counters.bad_signature_count),
        ("chain_gap_count", counters.chain_gap_count),
        ("chain_break_count", counters.chain_break_count),
        ("mislabeled_count", counters.mislabeled_count),
        ("over_clearance_count", counters.over_clearance_count),
        ("stale_count", counters.stale_count),
        ("duplicate_count", counters.duplicate_count),
        ("shard_mismatch_count", counters.shard_mismatch_count),
    ] {
        assert_eq!(value, 0, "rejection counter {name} must be zero -- every batch was accepted");
    }
    assert_eq!(hash::hex_encode(&counters.chain_head), EXPECTED_CHAIN_HEAD_HEX);

    // VerifyLedger: this partition's own (second, independent -- crate::log's own module
    // doc, "Two chains, deliberately") record_hash chain, over the wire, is intact.
    let verify_response = client.verify_ledger().await.unwrap();
    let partition = verify_response.partitions.get(&cfg.shard_key).expect("the shard must be present");
    assert!(partition.ok, "{partition:?}");
    assert_eq!(partition.checked, EXPECTED_BATCH_COUNT as u64);

    // The evidence surface's own partition chain_head (av_ingest::evidence's own map) --
    // independent read path from VerifyLedger, must agree on record_count, and its own
    // chain_head must equal what this test can independently recompute from the exact
    // batches it submitted: SHA-256(prev_record_hash || batch.encode_to_vec()), chained
    // from GENESIS -- crate::log's own record-framing contract (`crates/av-ingest/src/
    // log.rs`'s module doc), which is deliberately a SECOND, different chain from the
    // per-producer batch_hash chain (`EXPECTED_CHAIN_HEAD_HEX`) checked just above: that
    // one chains `canonical_body_bytes` (batch_hash/signature cleared) per producer; this
    // one chains the batch's own full encoded bytes (signature included) per partition,
    // interleaving every producer that writes to it.
    let ingest_evidence = client.get_evidence().await.unwrap();
    let partition_evidence = ingest_evidence.partitions.get(&cfg.shard_key).expect("partition evidence present");
    let mut expected_record_chain_head = hash::GENESIS.to_vec();
    for batch in &batches {
        let payload = prost::Message::encode_to_vec(batch);
        expected_record_chain_head = hash::compute_batch_hash(&expected_record_chain_head, &payload).to_vec();
    }
    assert_eq!(partition_evidence.chain_head, expected_record_chain_head, "the partition's own record_hash chain head must match this test's own independent recomputation");
    assert_eq!(partition_evidence.record_count, EXPECTED_BATCH_COUNT as u64);

    // "arrive at the ingest byte for byte": read the durable log straight off disk
    // (independent of any in-memory Ingest state -- see read_partition_payloads' own doc)
    // and assert every record's payload bytes equal exactly what this plugin submitted.
    let log_path = {
        let ingest_handle = service.ingest_handle();
        let ingest = ingest_handle.lock().unwrap();
        let path = ingest.partitions().find(|(k, _)| k.as_str() == cfg.shard_key).expect("partition open").1.path().to_path_buf();
        path
    };
    let payloads = read_partition_payloads(&log_path);
    assert_eq!(payloads.len(), EXPECTED_BATCH_COUNT);
    for (i, (payload, batch)) in payloads.iter().zip(batches.iter()).enumerate() {
        let expected = prost::Message::encode_to_vec(batch);
        assert_eq!(payload, &expected, "record {i}: the durable log's payload bytes must equal exactly what this plugin submitted, byte for byte");
        let decoded = <pb::MeasurementBatch as prost::Message>::decode(payload.as_slice()).expect("decodes");
        assert_eq!(decoded.measurements, batch.measurements, "record {i}: measurements read back from disk must equal the source's own measurements, byte for byte");
    }

    // And cross-check the measurements read back from disk against the source's own
    // groups directly, one more time, independent of `batches` -- proving the round trip
    // is real, not merely "batches[i] == batches[i]".
    let all_source_measurements: Vec<pb::Measurement> = source.groups().iter().flat_map(|(_, ms)| ms.iter().cloned()).collect();
    assert_eq!(all_source_measurements.len(), EXPECTED_BATCH_COUNT);
    let all_logged_measurements: Vec<pb::Measurement> = payloads
        .iter()
        .flat_map(|p| <pb::MeasurementBatch as prost::Message>::decode(p.as_slice()).unwrap().measurements)
        .collect();
    assert_eq!(all_logged_measurements, all_source_measurements, "the durable log's measurements must equal the plugin source's own measurements, byte for byte, in order");

    let _ = std::fs::remove_dir_all(&dir);
}
