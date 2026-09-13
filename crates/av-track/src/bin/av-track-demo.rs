//! Builds an `altavista.v1.RunProducts` carrying the demo ground-segment fixture's own
//! truth `Trajectory` ("flight") **and** the track this crate's engine bridge produces from
//! it ("flight_track"), for `tests/test_track_viewer.py` to `POST /api/cdm/run` and assert
//! both entities are published side by side (E5's own viewer deliverable).
//!
//! Entirely offline (question 154): the accepted batches are built in-process against a
//! temporary `av_ingest::ingest::Ingest` (never a real socket -- see `crate`'s own module
//! doc for why that is still "the log is the ledger," not a shortcut around it), read back
//! through this crate's own `LogPartitionConsumer`, and run through the engine bridge --
//! exactly the same pipeline `src/bin/av-edge-latency.rs` drives over a real wire, minus
//! the wire itself and the latency instrument.
//!
//! `--out PATH` writes the resulting `RunProducts` (binary protobuf) to `PATH`;
//! `tests/test_track_viewer.py` builds this binary once (module-scoped, `cargo build -p
//! av-track --bin av-track-demo`, the same "cargo build, fail loudly" pattern `tests/
//! test_cdm_run.py::av_run_bin` already uses) and runs it with a `tmp_path`-scoped `--out`.

use std::path::PathBuf;

use av_edge::pb;
use av_edge::plugin::{BatchBuilder, BatchingRule, PluginConfig, Pacing, PortTrafficSource};
use av_edge::policy::ProducerPolicy;
use av_edge::sign;
use av_ingest::ingest::{Ingest, Signer};
use av_track::bridge::EngineBridge;
use av_track::consumer::{LogPartitionConsumer, MeasurementConsumer, StartOffset};

const TEST_KEY_PEM: &[u8] = include_bytes!("../../../av-edge/tests/fixtures/test_signing_key.pem");
const TEST_PUB_PEM: &[u8] = include_bytes!("../../../av-edge/tests/fixtures/test_signing_key.pub.pem");

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../av-edge/tests/fixtures/ground_segment")
}

/// Identical to `crates/av-ingest/tests/plugin_wire.rs::flight_codec`/`config` and `src/
/// bin/av-edge-latency.rs`'s own restatement -- see either's own comment for why this is
/// duplicated rather than shared.
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

fn parse_out_arg() -> PathBuf {
    let mut args = std::env::args();
    let _argv0 = args.next();
    while let Some(flag) = args.next() {
        if flag == "--out" {
            return PathBuf::from(args.next().expect("--out requires a value"));
        }
    }
    panic!("usage: av-track-demo --out PATH");
}

fn main() {
    let out_path = parse_out_arg();

    let run_products_bytes = std::fs::read(fixtures_dir().join("run_products.pb")).expect("reading run_products.pb");
    let mut run_products = <pb::RunProducts as prost::Message>::decode(run_products_bytes.as_slice()).expect("decodes as RunProducts");
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

    // --- Accept every batch through a real (in-process, no socket) Ingest pipeline. ------
    let dir = std::env::temp_dir().join(format!("av-track-demo-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut ingest = Ingest::new(dir.as_path(), None);
    ingest.register_producer(ProducerPolicy::new(cfg.producer_id.clone(), "CUI", vec![], vec!["UNCLASSIFIED".to_string(), "CUI".to_string()], "CUI", 10_000_000_000_000).expect("policy builds"));
    for batch in &batches {
        let outcome = ingest.submit(batch, Signer::Key(verify_key.clone()), run_products.provenance.as_ref().map(|p| p.created_tai_ns).unwrap_or(0)).expect("submit");
        assert!(outcome.accepted(), "batch (sequence {}) was rejected: {outcome:?}", batch.sequence);
    }

    // --- Read back through this crate's own consumer, and drive the engine bridge. ------
    let (mut consumer, recovery) = LogPartitionConsumer::open(&dir, &cfg.shard_key, StartOffset::Earliest).expect("opening and chain-verifying the partition");
    assert!(recovery.is_none(), "{recovery:?}");
    let mut measurements = Vec::with_capacity(consumer.len());
    while let Some(received) = consumer.poll_measurement() {
        measurements.push(received.value);
    }

    let track_cfg = av_track::config::demo_ground_segment_config();
    let mut bridge = EngineBridge::new(&track_cfg).expect("building the engine bridge");
    let updates = bridge.run(&measurements).expect("running the engine bridge");

    let comparison = av_track::compare::compare_to_truth(&updates, run_products.trajectories.get("flight").expect("truth trajectory present"));
    eprintln!("{}", comparison.summary_line());
    assert!(comparison.within_tolerance(), "{}", comparison.summary_line());

    // --- Build the "flight_track" Trajectory and add it beside "flight" (the truth). ----
    let mut samples: Vec<pb::TrajectorySample> = updates
        .iter()
        .map(|u| {
            let gs = av_cdm::spoore_v0::gaussian_state_to_pb(&u.fused_state);
            pb::TrajectorySample { tai_ns: gs.epoch_ns, mean: gs.mean, cov: gs.cov, kind: pb::SampleKind::Native as i32 }
        })
        .collect();
    samples.sort_by_key(|s| s.tai_ns);

    let track_trajectory = pb::Trajectory {
        id: "flight_track".to_string(),
        entity_id: "flight_track".to_string(),
        state_space_id: "air_3d".to_string(),
        frame_id: track_cfg.frame_id.clone(),
        interpolation: pb::Interpolation::HermiteVelocity as i32,
        samples,
        segments: vec![],
        event_ids: vec![],
        label: None,
        provenance: Some(pb::Provenance {
            // No `AuthorKind::Derived` exists (`proto/altavista/v1/core.proto`'s own four
            // variants: Unspecified/Human/Agent/Service/External) -- `Service` is the
            // closest fit for "a platform service computed this from other data," and is
            // additive-only against that proto's own declared enum (this track owns no
            // proto file but `edge.proto` -- `core.proto` is read-only here).
            author_kind: pb::AuthorKind::Service as i32,
            principal: "av-track".to_string(),
            tool: "av-track-demo".to_string(),
            created_tai_ns,
            run_id: run_products.run_id.clone(),
            ..Default::default()
        }),
        config_hash: av_edge::hash::hex_encode(&track_cfg.config_hash()),
    };
    run_products.trajectories.insert("flight_track".to_string(), track_trajectory);

    let bytes = <pb::RunProducts as prost::Message>::encode_to_vec(&run_products);
    std::fs::write(&out_path, &bytes).expect("writing --out");
    println!("{{\"out\": {out_path:?}, \"bytes\": {}, \"trajectories\": {}}}", bytes.len(), run_products.trajectories.len());

    let _ = std::fs::remove_dir_all(&dir);
}
