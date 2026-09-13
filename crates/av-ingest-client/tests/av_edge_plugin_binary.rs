//! A smoke test of the actual **built** `av-edge-plugin` binary (`src/bin/av-edge-plugin.rs`)
//! -- not just the `av_edge::plugin` library it wraps. Spins up a real `EdgeIngestService`
//! in-process on an ephemeral loopback port (exactly `crates/av-ingest/tests/
//! wire_evidence.rs`'s own pattern), writes a `PluginConfig` JSON file, runs the built
//! binary as a subprocess pointed at the committed ground-segment fixture and that config,
//! and asserts its printed JSON summary and exit code.
//!
//! `crates/av-edge/tests/plugin_replay.rs` and `crates/av-ingest/tests/plugin_wire.rs`
//! already prove the *library* end to end; this file's only additional job is proving the
//! *binary* (argument parsing, file loading, the pacing loop, the JSON summary shape) is
//! wired correctly -- so it deliberately reuses the same fixture/config/key and repeats the
//! same pinned numbers rather than inventing new ones.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;

use av_edge::pb;
use av_edge::plugin::{BatchingRule, Pacing, PluginConfig};
use av_ingest::pb::edge_ingest_server::EdgeIngestServer;
use av_ingest::server::bind_loopback;
use av_ingest::service::{EdgeIngestConfig, EdgeIngestService};
use tonic::transport::Server;

const EXPECTED_BATCH_COUNT: u64 = 900;
/// Must agree with `crates/av-edge/tests/plugin_replay.rs`/`crates/av-ingest/tests/
/// plugin_wire.rs`'s own identical constant -- same fixture, same config, same key.
const EXPECTED_CHAIN_HEAD_HEX: &str = "d1d80d0b6cc9228aaa7479864b8a89f18be88382fb3772bd3c4cc3d7c2dc2698";

fn av_edge_fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../av-edge/tests/fixtures")
}

/// Identical to `crates/av-edge/tests/plugin_replay.rs`/`crates/av-ingest/tests/
/// plugin_wire.rs`'s own `flight_codec`/`config` -- see those files for exactly where
/// every value comes from (`drms/demo_ground_segment_flight.system.yaml`).
fn plugin_config() -> PluginConfig {
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
    let codec = pb::PacketCodec {
        id: "flight_tm_out_codec".to_string(),
        apid: 500,
        is_command: false,
        secondary_header_bytes: 0,
        user_data_bytes: 24,
        description: "own Earth-fixed Cartesian position telemetry, encoded (M25.1)".to_string(),
        fields: vec![f("x", 0), f("y", 64), f("z", 128)],
    };
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

fn tmp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("av-edge-plugin-bin-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
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

/// The fixture's own `RunProducts.provenance.created_tai_ns` -- the server's injected
/// clock just needs to be no later than the earliest batch epoch (staleness policy: "a
/// batch from the future is never stale"), so this test's own server clock is fixed well
/// before the run's own start.
const SERVER_CLOCK_TAI_NS: i64 = 0;

async fn start_server(dir: &std::path::Path, verify_key: openssl::ec::EcKey<openssl::pkey::Public>, producer_id: &str) -> SocketAddr {
    let mut cfg = EdgeIngestConfig::new(vec!["UNCLASSIFIED".to_string(), "CUI".to_string()], 10_000_000_000_000_000);
    cfg.require_client_certificate = false;
    cfg.verify_keys.insert(producer_id.to_string(), verify_key);
    let service = Arc::new(EdgeIngestService::new(dir, None, cfg, Arc::new(|| SERVER_CLOCK_TAI_NS)));

    let listener = bind_loopback("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
    tokio::spawn(async move {
        let _ = Server::builder().add_service(EdgeIngestServer::from_arc(service)).serve_with_incoming(incoming).await;
    });
    poll_until_ready(addr).await;
    addr
}

// `flavor = "multi_thread"`: this test both runs the in-process `EdgeIngestServer` on a
// `tokio::spawn`ed task AND blocks synchronously in `std::process::Command::output()`
// waiting for the subprocess to finish. `#[tokio::test]`'s DEFAULT flavor is single-
// threaded, so that blocking call would starve the very same thread the spawned server
// task needs polled on -- the subprocess would then hang forever waiting for a response
// from a server that can never run again, deadlocking the test (confirmed directly: an
// earlier draft without this attribute hung indefinitely; killed by hand after 60s+ of
// both processes sitting at 0% CPU). `crates/av-ingest/tests/wire_evidence.rs`'s own
// `get` helper doc comment documents the identical hazard class for a blocking
// `std::net::TcpStream` read on that crate's default single-threaded runtime.
#[tokio::test(flavor = "multi_thread")]
async fn the_built_binary_replays_the_fixture_and_prints_the_pinned_summary() {
    let cfg = plugin_config();
    let verify_key = av_edge::verify::load_verifying_key(&std::fs::read(av_edge_fixtures_dir().join("test_signing_key.pub.pem")).unwrap()).unwrap();
    let dir = tmp_dir("log");
    let addr = start_server(&dir, verify_key, &cfg.producer_id).await;

    let config_path = dir.join("plugin_config.json");
    std::fs::write(&config_path, serde_json::to_vec_pretty(&cfg).unwrap()).unwrap();

    let bin = env!("CARGO_BIN_EXE_av-edge-plugin");
    let output = Command::new(bin)
        .arg("--run-products")
        .arg(av_edge_fixtures_dir().join("ground_segment/run_products.pb"))
        .arg("--port-traffic")
        .arg(av_edge_fixtures_dir().join("ground_segment/port_traffic.pb"))
        .arg("--plugin-config")
        .arg(&config_path)
        .arg("--signing-key")
        .arg(av_edge_fixtures_dir().join("test_signing_key.pem"))
        .arg("--endpoint")
        .arg(addr.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("running the built av-edge-plugin binary");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "av-edge-plugin exited non-zero: status={:?}\nstdout={stdout}\nstderr={stderr}", output.status);

    let summary: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("stdout was not valid JSON: {e}\nstdout={stdout}"));
    assert_eq!(summary["batch_count"], EXPECTED_BATCH_COUNT);
    assert_eq!(summary["measurement_count"], EXPECTED_BATCH_COUNT);
    assert_eq!(summary["any_rejected"], false);
    assert_eq!(summary["chain_head_hex"], EXPECTED_CHAIN_HEAD_HEX);
    let verdicts = summary["verdicts"].as_array().expect("verdicts array");
    assert_eq!(verdicts.len(), EXPECTED_BATCH_COUNT as usize);
    assert!(verdicts.iter().all(|v| v["accepted"] == true), "every verdict must be accepted");

    let _ = std::fs::remove_dir_all(&dir);
}
