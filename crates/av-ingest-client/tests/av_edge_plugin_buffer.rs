//! Question 207's `--buffer-dir` requirement: a run of the REAL, built `av-edge-plugin`
//! binary with `--buffer-dir` produces the identical batch count and chain head to a run
//! without it, and the buffer file this flag produces exists on disk afterwards and reads
//! back correctly through `av_edge::buffer::EdgeBuffer::open`/`record_count`.
//!
//! Lives in `crates/av-ingest-client/tests/`, not `crates/av-ingest/tests/`, for the same
//! reason `av_edge_plugin_binary.rs`/`av_edge_plugin_identity.rs` do:
//! `env!("CARGO_BIN_EXE_av-edge-plugin")` is only set for integration tests of the crate
//! that declares that `[[bin]]` target.
//!
//! No certificate is in this file's own scope at all (E1's own no-cert path, exactly
//! `av_edge_plugin_binary.rs`'s own server wiring) -- `--buffer-dir` is orthogonal to
//! `--client-cert`/`--trust-anchor`, and this file's only job is proving the buffer wiring
//! itself is transparent to the plugin's own observable output.
//!
//! **Why the buffer file's own expected record count is zero**: `av_edge::buffer::
//! UplinkDriver::step` only ever calls `EdgeBuffer::append` from inside its own
//! `SinkOutcome::LinkDown` branch (`crates/av-edge/src/buffer.rs`'s own doc comment) --
//! neither this test's server nor this binary's own `PluginBatchSink` (`src/bin/
//! av-edge-plugin.rs`) ever reports the link down, so an uninterrupted run leaves the
//! buffer file real, on disk, and opened by `EdgeBuffer::open` (which itself creates it),
//! but with zero records appended -- exactly what E6 promises ("capture-only while
//! disconnected": nothing to capture when nothing ever disconnects). This is the
//! literal, correct behaviour, not a shortfall in this test.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;

use av_edge::buffer::EdgeBuffer;
use av_edge::pb;
use av_edge::plugin::{BatchingRule, Pacing, PluginConfig};
use av_ingest::pb::edge_ingest_server::EdgeIngestServer;
use av_ingest::server::bind_loopback;
use av_ingest::service::{EdgeIngestConfig, EdgeIngestService};
use tonic::transport::Server;

fn av_edge_fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../av-edge/tests/fixtures")
}

/// Identical to `av_edge_plugin_binary.rs`/`av_edge_plugin_identity.rs`'s own
/// `plugin_config` -- same committed fixture, same codec, same 900-measurement shape.
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
    let dir = std::env::temp_dir().join(format!("av-edge-plugin-buffer-test-{name}-{}", std::process::id()));
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

/// The fixture's own `RunProducts.provenance.created_tai_ns` -- mirrors
/// `av_edge_plugin_binary.rs::SERVER_CLOCK_TAI_NS`'s own identical comment: fixed well
/// before the run's own start so "a batch from the future is never stale" keeps every
/// batch accepted.
const SERVER_CLOCK_TAI_NS: i64 = 0;

async fn start_server(dir: &std::path::Path, verify_key: openssl::ec::EcKey<openssl::pkey::Public>) -> (SocketAddr, Arc<EdgeIngestService>) {
    let mut cfg = EdgeIngestConfig::new(vec!["UNCLASSIFIED".to_string(), "CUI".to_string()], 10_000_000_000_000_000);
    cfg.require_client_certificate = false;
    cfg.verify_keys.insert(plugin_config().producer_id, verify_key);
    let service = Arc::new(EdgeIngestService::new(dir, None, cfg, Arc::new(|| SERVER_CLOCK_TAI_NS)));

    let listener = bind_loopback("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
    let served = service.clone();
    tokio::spawn(async move {
        let _ = Server::builder().add_service(EdgeIngestServer::from_arc(served)).serve_with_incoming(incoming).await;
    });
    poll_until_ready(addr).await;
    (addr, service)
}

fn run_plugin(config_path: &std::path::Path, signing_key_path: &std::path::Path, endpoint: &str, buffer_dir: Option<&std::path::Path>) -> std::process::Output {
    let bin = env!("CARGO_BIN_EXE_av-edge-plugin");
    let mut cmd = Command::new(bin);
    cmd.arg("--run-products")
        .arg(av_edge_fixtures_dir().join("ground_segment/run_products.pb"))
        .arg("--port-traffic")
        .arg(av_edge_fixtures_dir().join("ground_segment/port_traffic.pb"))
        .arg("--plugin-config")
        .arg(config_path)
        .arg("--signing-key")
        .arg(signing_key_path)
        .arg("--endpoint")
        .arg(endpoint);
    if let Some(d) = buffer_dir {
        cmd.arg("--buffer-dir").arg(d);
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).output().expect("running the built av-edge-plugin binary")
}

// `flavor = "multi_thread"`: identical reasoning to `av_edge_plugin_binary.rs`'s own doc
// comment on its one test -- this test both runs the in-process `EdgeIngestServer` on a
// `tokio::spawn`ed task and blocks synchronously in `Command::output()` (twice, here).
#[tokio::test(flavor = "multi_thread")]
async fn buffer_dir_produces_identical_output_to_no_buffer_and_leaves_a_real_readable_buffer_file() {
    let verify_key = av_edge::verify::load_verifying_key(&std::fs::read(av_edge_fixtures_dir().join("test_signing_key.pub.pem")).unwrap()).unwrap();
    let signing_key_path = av_edge_fixtures_dir().join("test_signing_key.pem");

    // Run 1: no --buffer-dir at all -- today's exact, unchanged behaviour.
    let dir1 = tmp_dir("no-buffer");
    let (addr1, _service1) = start_server(&dir1.join("ingest"), verify_key.clone()).await;
    let config1 = dir1.join("plugin_config.json");
    std::fs::write(&config1, serde_json::to_vec_pretty(&plugin_config()).unwrap()).unwrap();
    let output1 = run_plugin(&config1, &signing_key_path, &addr1.to_string(), None);
    let stdout1 = String::from_utf8_lossy(&output1.stdout);
    let stderr1 = String::from_utf8_lossy(&output1.stderr);
    assert!(output1.status.success(), "no-buffer run failed: stdout={stdout1}\nstderr={stderr1}");
    let summary1: serde_json::Value = serde_json::from_str(stdout1.trim()).unwrap();

    // Verify no buffer file/directory of any kind appeared anywhere under dir1 -- "no
    // buffer, no new file, anywhere" (`src/bin/av-edge-plugin.rs`'s own module doc).
    let entries_before: Vec<_> = std::fs::read_dir(&dir1).unwrap().map(|e| e.unwrap().file_name()).collect();

    // Run 2: --buffer-dir set, against a FRESH server instance (so the two runs' own
    // partition logs/ledgers cannot interfere with each other).
    let dir2 = tmp_dir("with-buffer");
    let (addr2, _service2) = start_server(&dir2.join("ingest"), verify_key).await;
    let config2 = dir2.join("plugin_config.json");
    std::fs::write(&config2, serde_json::to_vec_pretty(&plugin_config()).unwrap()).unwrap();
    let buffer_dir = dir2.join("buffer");
    let output2 = run_plugin(&config2, &signing_key_path, &addr2.to_string(), Some(&buffer_dir));
    let stdout2 = String::from_utf8_lossy(&output2.stdout);
    let stderr2 = String::from_utf8_lossy(&output2.stderr);
    assert!(output2.status.success(), "--buffer-dir run failed: stdout={stdout2}\nstderr={stderr2}");
    let summary2: serde_json::Value = serde_json::from_str(stdout2.trim()).unwrap();

    // The headline property: identical batch count and chain head, with and without the
    // buffer.
    assert_eq!(summary1["batch_count"], summary2["batch_count"], "batch_count must be identical with and without --buffer-dir");
    assert_eq!(summary1["measurement_count"], summary2["measurement_count"]);
    assert_eq!(summary1["chain_head_hex"], summary2["chain_head_hex"], "chain_head_hex must be identical with and without --buffer-dir");
    assert_eq!(summary1["any_rejected"], false);
    assert_eq!(summary2["any_rejected"], false);
    assert_eq!(summary1["batch_count"], 900);

    assert!(entries_before.iter().all(|f| f != "buffer"), "the no-buffer run must never create anything named like a buffer directory: {entries_before:?}");

    // The buffer file itself: real, on disk, and readable back through EdgeBuffer::open,
    // with the expected (zero -- see this file's own module doc) record count, since
    // nothing ever disconnected during this run.
    let buffer_file = buffer_dir.join(format!("{}.buflog", plugin_config().producer_id));
    assert!(buffer_file.exists(), "the buffer file must exist on disk after a --buffer-dir run: {buffer_file:?}");
    let (edge_buffer, recovery) = EdgeBuffer::open(&buffer_file).expect("EdgeBuffer::open must read back the plugin's own buffer file");
    assert!(recovery.is_none(), "an uninterrupted run must need no recovery: {recovery:?}");
    assert_eq!(edge_buffer.record_count(), 0, "nothing ever disconnected, so the durable buffer must hold zero records -- see this file's own module doc for why this is the correct, not a degenerate, outcome");

    let out_dir = std::path::Path::new("/private/tmp/claude-501/-Users-probe-code-AltaVista/ed238b37-d349-42f1-956b-22807101027f/scratchpad/r4/t3a");
    let _ = std::fs::create_dir_all(out_dir);
    std::fs::write(out_dir.join("test_e_no_buffer_stdout.json"), stdout1.as_bytes()).unwrap();
    std::fs::write(out_dir.join("test_e_with_buffer_stdout.json"), stdout2.as_bytes()).unwrap();
    std::fs::write(
        out_dir.join("test_e_buffer_readback.txt"),
        format!("buffer_file={buffer_file:?}\nrecord_count={}\ntip_hash={:02x?}\nack_watermark={}\n", edge_buffer.record_count(), edge_buffer.tip_hash(), edge_buffer.ack_watermark()),
    )
    .unwrap();
}
