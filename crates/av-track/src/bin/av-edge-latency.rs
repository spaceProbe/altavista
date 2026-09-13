//! The latency harness (`docs/edge-plan.md` milestone E5's own instruction: "a binary, not
//! a test assertion"). Runs the **whole** path over a real wire -- plugin batch-build,
//! `EdgeIngestService` (real `tonic` server, ephemeral loopback port), the durable
//! per-partition log, this crate's own `LogPartitionConsumer`, and the engine bridge -- and
//! prints one machine-readable JSON report: batch count, latency min/max/p50/p99, the
//! track-vs-truth comparison, the declared `TrackConfig`'s own content hash, and the host
//! state this binary can see about itself.
//!
//! **This binary asserts no latency threshold.** A contended timing result is not a
//! result (this milestone's own instruction) -- it prints the numbers and exits `0`
//! whenever the *functional* path succeeded (every batch accepted, the chain verifies, the
//! comparison stayed within [`av_track::compare::POSITION_TOLERANCE_M`]); a slow run is
//! still a successful run of this binary, just with worse numbers in its own report.
//!
//! # The two instants this measures, restated concretely
//!
//! See `av_track::latency`'s own module doc for the full argument; concretely, in this
//! binary's own `main`, per batch: **emit** = `Instant::now()` read immediately before
//! `client.submit_batches(vec![batch])` is called; **accept** = `Instant::now()` read
//! immediately after that call's `Result` is available and confirmed `accepted`. Only this
//! binary ever reads `Instant::now()` in this whole path -- `av-edge`, `av-ingest`, and
//! this crate's own library code all stay clock-free (question 199).
//!
//! # This binary's own clock is real; `av-ingest`'s injected clock is not
//!
//! `EdgeIngestService` is constructed with a fixed clock closure (`|| NOW`, `NOW` being
//! the fixture's own declared start epoch) purely so the service's *staleness* check has
//! something declared to compare against -- it never affects the latency measurement
//! itself, which is entirely a client-side `Instant` measurement around the RPC call.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use av_edge::pb;
use av_edge::plugin::{BatchBuilder, BatchingRule, PluginConfig, Pacing, PortTrafficSource};
use av_edge::{hash, sign};
use av_ingest::pb::edge_ingest_server::EdgeIngestServer;
use av_ingest::service::{EdgeIngestConfig, EdgeIngestService};
use av_ingest_client::EdgeIngestClient;
use av_track::bridge::EngineBridge;
use av_track::compare::compare_to_truth;
use av_track::consumer::{LogPartitionConsumer, MeasurementConsumer, StartOffset};
use av_track::latency::{summarize, LatencySample};
use tonic::transport::Server;

/// The fixture's own start epoch (`crates/av-edge/tests/fixtures/ground_segment/
/// README.md`) -- restated here as `NOW`, exactly `crates/av-ingest/tests/plugin_wire.rs`'s
/// own identical constant, for the same reason: `EdgeIngestConfig`'s staleness check needs
/// a declared "now" to compare batch epochs against, and the fixture's own start is the
/// one this whole 900 s run never exceeds by more than a few minutes either direction.
const NOW: i64 = 1_767_225_637_000_000_000;

const TEST_KEY_PEM: &[u8] = include_bytes!("../../../av-edge/tests/fixtures/test_signing_key.pem");
const TEST_PUB_PEM: &[u8] = include_bytes!("../../../av-edge/tests/fixtures/test_signing_key.pub.pem");

fn fixtures_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../av-edge/tests/fixtures/ground_segment")
}

/// Identical to `crates/av-ingest/tests/plugin_wire.rs::flight_codec`/`config` -- this
/// binary's own restatement, for the same reason that file's own comment gives: no shared
/// test-utility crate exists between `av-ingest` and this crate.
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

fn plugin_config() -> PluginConfig {
    let codec = flight_codec();
    let label = pb::Label { marking: "CUI".to_string(), caveats: vec![] };
    let track_cfg = av_track::config::demo_ground_segment_config();
    PluginConfig {
        producer_id: "demo-ground-segment-flight-plugin".to_string(),
        plugin_version: "0.1.0".to_string(),
        instance: "flight".to_string(),
        port: "tm_out".to_string(),
        direction: pb::PortDirection::Out as i32,
        codec_bytes: PluginConfig::encode_codec(&codec),
        component_fields: vec!["x".to_string(), "y".to_string(), "z".to_string()],
        frame_id: track_cfg.frame_id.clone(),
        sensor_id: track_cfg.sensor_id.clone(),
        measurement_id: "flight_position".to_string(),
        shard_key: track_cfg.shard_key.clone(),
        noise_r: vec![100.0, 0.0, 0.0, 0.0, 100.0, 0.0, 0.0, 0.0, 100.0],
        label_bytes: PluginConfig::encode_label(&label),
        clearance: "CUI".to_string(),
        leaf_fingerprint_sha256: String::new(),
        batching: BatchingRule::PerEpoch,
        pacing: Pacing::AsFastAsPossible,
    }
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

#[tokio::main]
async fn main() {
    let run_products_bytes = std::fs::read(fixtures_dir().join("run_products.pb")).expect("reading run_products.pb");
    let run_products = <pb::RunProducts as prost::Message>::decode(run_products_bytes.as_slice()).expect("decodes as RunProducts");
    let port_traffic_bytes = std::fs::read(fixtures_dir().join("port_traffic.pb")).expect("reading port_traffic.pb");
    let log = av_edge::plugin::verify_port_traffic_log(&port_traffic_bytes, &run_products.port_traffic_hash).expect("hash-verified sidecar");

    let cfg = plugin_config();
    let source = PortTrafficSource::from_log(&log, &cfg).expect("source builds");
    let signing_key = sign::load_signing_key(TEST_KEY_PEM).expect("loading test signing key");
    let verify_key = av_edge::verify::load_verifying_key(TEST_PUB_PEM).expect("loading test verifying key");
    let builder = BatchBuilder::new(cfg.batching).expect("batching rule builds");
    let created_tai_ns = run_products.provenance.as_ref().map(|p| p.created_tai_ns).unwrap_or(0);
    let provenance = cfg.batch_provenance(&run_products.run_id, created_tai_ns);
    let batches = builder.build_batches_for_config(&source, &cfg, &provenance, &signing_key).expect("building/signing batches");
    let batch_count = batches.len();
    let measurement_count: usize = batches.iter().map(|b| b.measurements.len()).sum();

    // --- The real wire: a genuine EdgeIngestService on an ephemeral loopback port. ------
    let dir = std::env::temp_dir().join(format!("av-edge-latency-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut server_cfg = EdgeIngestConfig::new(vec!["UNCLASSIFIED".to_string(), "CUI".to_string()], 10_000_000_000_000);
    server_cfg.require_client_certificate = false;
    server_cfg.verify_keys.insert(cfg.producer_id.clone(), verify_key);
    let service = Arc::new(EdgeIngestService::new(dir.as_path(), None, server_cfg, Arc::new(|| NOW)));

    let listener = av_ingest::server::bind_loopback("127.0.0.1:0").await.expect("binding loopback");
    let addr = listener.local_addr().expect("local_addr");
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
    let served = service.clone();
    tokio::spawn(async move {
        let _ = Server::builder().add_service(EdgeIngestServer::from_arc(served)).serve_with_incoming(incoming).await;
    });
    poll_until_ready(addr).await;

    let mut client = EdgeIngestClient::connect_plaintext_addr(addr).await.expect("connecting");
    let manifest = cfg.manifest().expect("manifest builds");
    let ack = client.announce(manifest).await.expect("Announce RPC");
    assert!(ack.accepted, "Announce was refused: {ack:?}");

    // --- The one clock in this whole path: per-batch emit/accept Instants. --------------
    let clock_zero = Instant::now();
    let mut samples: Vec<LatencySample> = Vec::with_capacity(batch_count);
    for batch in &batches {
        let emit_ns = (Instant::now() - clock_zero).as_nanos() as u64;
        let verdicts = client.submit_batches(vec![batch.clone()]).await.expect("Submit RPC");
        let accept_ns = (Instant::now() - clock_zero).as_nanos() as u64;
        assert_eq!(verdicts.len(), 1);
        assert!(verdicts[0].accepted, "batch (sequence {}) was rejected: {:?}", batch.sequence, verdicts[0]);
        samples.push(LatencySample { emit_ns, accept_ns });
    }
    let latency_report = summarize(&samples).expect("at least one batch was submitted");

    // --- The consumer, over the same durable log the wire just wrote. -------------------
    let (mut consumer, recovery) = LogPartitionConsumer::open(&dir, &cfg.shard_key, StartOffset::Earliest).expect("opening and chain-verifying the partition this run just wrote");
    assert!(recovery.is_none(), "a clean, uninterrupted run must never need crash recovery: {recovery:?}");
    let mut measurements = Vec::with_capacity(consumer.len());
    while let Some(received) = consumer.poll_measurement() {
        measurements.push(received.value);
    }
    assert_eq!(measurements.len(), measurement_count);

    // --- The engine bridge, and the comparison against the fixture's own truth. ---------
    let track_cfg = av_track::config::demo_ground_segment_config();
    let mut bridge = EngineBridge::new(&track_cfg).expect("building the engine bridge");
    let updates = bridge.run(&measurements).expect("running the engine bridge");
    let truth = run_products.trajectories.get("flight").expect("the flight instance must have a truth Trajectory");
    let comparison = compare_to_truth(&updates, truth);
    eprintln!("{}", comparison.summary_line());

    let _ = std::fs::remove_dir_all(&dir);

    // --- The report. ---------------------------------------------------------------------
    let host = serde_json::json!({
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "logical_cpus": std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0),
        "quiescence_verified_by_this_binary": false,
    });
    let report = serde_json::json!({
        "batch_count": batch_count,
        "measurement_count": measurement_count,
        "chain_head_hex": hash::hex_encode(&batches.last().unwrap().batch_hash),
        "latency_ns": {
            "count": latency_report.count,
            "min": latency_report.min_ns,
            "max": latency_report.max_ns,
            "p50": latency_report.p50_ns,
            "p99": latency_report.p99_ns,
        },
        "track_config_hash_hex": hash::hex_encode(&track_cfg.config_hash()),
        "track_comparison": {
            "compared_epochs": comparison.series.len(),
            "unmatched_updates": comparison.unmatched_updates,
            "max_error_m": comparison.max_error_m,
            "p50_error_m": comparison.p50_error_m,
            "p99_error_m": comparison.p99_error_m,
            "tolerance_m": av_track::compare::POSITION_TOLERANCE_M,
            "within_tolerance": comparison.within_tolerance(),
        },
        "host": host,
        "note": "this binary asserts no latency threshold (E5's own instruction); host quiescence was NOT verified by this binary itself -- the caller must check `ps` separately before treating these numbers as a clean measurement",
    });
    println!("{report}");
}
