//! E5's own acceptance test list: "tracks within tolerance for the demo DRM, with the
//! actual measured error printed" (question 148) -- and, in the same file, since both need
//! the identical pipeline: "two runs over the same log produce identical tracks (same ids,
//! same states)".
//!
//! Entirely offline (question 154): the committed `crates/av-edge/tests/fixtures/
//! ground_segment/{run_products.pb,port_traffic.pb}` fixture, an in-process `av_ingest::
//! ingest::Ingest` (no socket -- `crate`'s own module doc explains why this still exercises
//! "the log is the ledger," not a shortcut around it), this crate's own `LogPartitionConsumer`,
//! and the engine bridge.

use std::path::PathBuf;

use av_edge::pb;
use av_edge::plugin::{BatchBuilder, BatchingRule, MeasurementSource, PluginConfig, Pacing, PortTrafficSource};
use av_edge::policy::ProducerPolicy;
use av_edge::sign;
use av_ingest::ingest::{Ingest, Signer};
use av_track::bridge::EngineBridge;
use av_track::compare::compare_to_truth;
use av_track::config::demo_ground_segment_config;
use av_track::consumer::{LogPartitionConsumer, MeasurementConsumer, StartOffset};
use spoore_cdm::TrackUpdate;

const TEST_KEY_PEM: &[u8] = include_bytes!("../../av-edge/tests/fixtures/test_signing_key.pem");
const TEST_PUB_PEM: &[u8] = include_bytes!("../../av-edge/tests/fixtures/test_signing_key.pub.pem");

const EXPECTED_MEASUREMENT_COUNT: usize = 900;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../av-edge/tests/fixtures/ground_segment")
}

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
    let track_cfg = demo_ground_segment_config();
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

fn tmp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("av-track-engine-accuracy-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// Runs the full offline pipeline once (build batches -> in-process Ingest -> this crate's
/// own consumer -> engine bridge) and returns every `TrackUpdate` produced plus the run's
/// own truth `Trajectory`.
fn run_once(dir_name: &str) -> (Vec<TrackUpdate>, pb::Trajectory) {
    let run_products_bytes = std::fs::read(fixtures_dir().join("run_products.pb")).expect("reading run_products.pb");
    let run_products = <pb::RunProducts as prost::Message>::decode(run_products_bytes.as_slice()).expect("decodes");
    let port_traffic_bytes = std::fs::read(fixtures_dir().join("port_traffic.pb")).expect("reading port_traffic.pb");
    let log = av_edge::plugin::verify_port_traffic_log(&port_traffic_bytes, &run_products.port_traffic_hash).expect("hash-verified sidecar");

    let cfg = plugin_config();
    let source = PortTrafficSource::from_log(&log, &cfg).expect("source builds");
    assert_eq!(source.groups().len(), EXPECTED_MEASUREMENT_COUNT);

    let signing_key = sign::load_signing_key(TEST_KEY_PEM).unwrap();
    let verify_key = av_edge::verify::load_verifying_key(TEST_PUB_PEM).unwrap();
    let builder = BatchBuilder::new(cfg.batching).unwrap();
    let created_tai_ns = run_products.provenance.as_ref().map(|p| p.created_tai_ns).unwrap_or(0);
    let provenance = cfg.batch_provenance(&run_products.run_id, created_tai_ns);
    let batches = builder.build_batches_for_config(&source, &cfg, &provenance, &signing_key).unwrap();
    assert_eq!(batches.len(), EXPECTED_MEASUREMENT_COUNT);

    let dir = tmp_dir(dir_name);
    let mut ingest = Ingest::new(dir.as_path(), None);
    ingest.register_producer(ProducerPolicy::new(cfg.producer_id.clone(), "CUI", vec![], vec!["UNCLASSIFIED".to_string(), "CUI".to_string()], "CUI", 10_000_000_000_000).unwrap());
    for batch in &batches {
        let outcome = ingest.submit(batch, Signer::Key(verify_key.clone()), created_tai_ns).unwrap();
        assert!(outcome.accepted(), "{outcome:?}");
    }

    let (mut consumer, recovery) = LogPartitionConsumer::open(&dir, &cfg.shard_key, StartOffset::Earliest).unwrap();
    assert!(recovery.is_none());
    assert_eq!(consumer.len(), EXPECTED_MEASUREMENT_COUNT);
    let mut measurements = Vec::with_capacity(consumer.len());
    while let Some(received) = consumer.poll_measurement() {
        measurements.push(received.value);
    }

    let mut bridge = EngineBridge::for_demo().unwrap();
    let updates = bridge.run(&measurements).unwrap();

    let truth = run_products.trajectories.get("flight").expect("the flight instance must have a truth Trajectory").clone();

    let _ = std::fs::remove_dir_all(&dir);
    (updates, truth)
}

#[test]
fn tracks_are_within_the_pinned_tolerance_for_the_demo_drm_with_the_measured_error_printed() {
    let (updates, truth) = run_once("tolerance");
    assert_eq!(updates.len(), EXPECTED_MEASUREMENT_COUNT, "one TrackUpdate per scan for a single, always-associated target over the whole 900 s run");

    let report = compare_to_truth(&updates, &truth);
    // Question 148: the actual measured error, printed, not only asserted against.
    println!("{}", report.summary_line());
    assert_eq!(report.unmatched_updates, 0, "every one of this fixture's own 900 scans has a matching truth sample at its exact epoch");
    assert!(report.within_tolerance(), "{}", report.summary_line());
}

#[test]
fn two_runs_over_the_same_log_produce_identical_tracks_same_ids_same_states() {
    let (updates_a, _) = run_once("determinism-a");
    let (updates_b, _) = run_once("determinism-b");

    assert_eq!(updates_a.len(), updates_b.len());
    for (a, b) in updates_a.iter().zip(updates_b.iter()) {
        assert_eq!(a.track_id, b.track_id, "track ids must be identical run to run (no wall clock, no hash-seeded randomness anywhere in this path)");
        assert_eq!(a.epoch, b.epoch);
        assert_eq!(a.fused_state, b.fused_state, "fused state must be bit-identical: spoore_engine::Shard is a pure function of its ordered input (ADR-004; that crate's own shard.rs module doc)");
        assert_eq!(a.event, b.event);
        assert_eq!(a.track_score, b.track_score);
    }
}

#[test]
fn exactly_one_track_exists_for_this_single_target_zero_clutter_scenario() {
    let (updates, _) = run_once("single-track");
    let track_ids: std::collections::BTreeSet<&str> = updates.iter().map(|u| u.track_id.as_str()).collect();
    assert_eq!(track_ids.len(), 1, "this comparison's own validity (crate::compare's module doc) depends on there never being more than one live track to compare against the one truth trajectory");
}

/// Question 207 (round 4): closes the gap round 3 recorded in `docs/edge-plan.md` --
/// `TrackConfig::config_hash()` for the demo fixture and E5's track-vs-truth accuracy
/// numbers were read out of `av-edge-latency`'s own JSON report by hand and asserted
/// nowhere. Both are pinned here, against the demo DRM's real 900-scan run, not a
/// third code path -- this file's own [`run_once`] and `av_track::config::
/// demo_ground_segment_config` are the same two entry points `crates/av-track/src/bin/
/// av-edge-latency.rs` uses for the identical numbers.
///
/// # The config hash
///
/// Asserted **exactly** (a hash is exact or it is nothing) against
/// `7a2d33df306ae9861161e1c64eb2cf79c67e7657e419b93fa9365105a53bb06b` -- the value round 2
/// first recorded (after it moved from round 1's `63160880518fab9a2bfe42b801328abd8de794a
/// c1a9c419a6a2f39fa408c3b19` when `TrackConfig` gained two declared fields) and round 3
/// re-confirmed. Round 4 re-measured it fresh, before writing this assertion (question 148:
/// measure first, pin second), and it had not moved.
///
/// # The three error statistics: bounds, not exact values, and why
///
/// Round 4 ran the demo fixture twice end to end (`cargo run -p av-track --bin
/// av-edge-latency`, same binary, same host, two separate process invocations) and
/// `max_error_m`/`p50_error_m`/`p99_error_m` came back **bit-identical** across both runs --
/// unsurprising, since this pipeline has no wall clock and no hash-seeded randomness
/// anywhere in it (the same reason `two_runs_over_the_same_log_produce_identical_tracks_
/// same_ids_same_states` above holds for the full `fused_state`). An exact assertion would
/// therefore be honest for *this host's own build* specifically. It is deliberately not used
/// anyway: `crate::compare`'s own module doc already explains why `POSITION_TOLERANCE_M`
/// itself stays generous rather than pinned tight -- "ordinary `f64` non-associativity across
/// a different compiler/BLAS/platform" is a real risk even when this exact binary, on this
/// exact host, reproduces itself bit-for-bit run to run. A bounded assertion is the honest
/// middle ground: tight enough to actually catch a regression, loose enough to survive a
/// toolchain or platform this repository does not control.
///
/// Each bound, and the class of regression it exists to catch:
/// - **`max_error_m`**: measured `0.0018749988892797183` m. Pinned band `(1e-4, 1e-2)` m --
///   about 19x below and 5x above the measured value, and still three orders of magnitude
///   under `POSITION_TOLERANCE_M` (`1.0` m, the only guard this value had before this test).
///   *Upper* bound: catches a genuine tracking regression (a model-mismatch bug, a
///   mis-wired sensor index, a units error) long before it could silently hide under the old
///   1.0 m tolerance. *Lower* bound: catches the regression class an upper-bound-only check
///   can never see -- a comparison that has quietly stopped doing real work (matching a
///   track against itself, silently dropping the hard epochs from the matched set) and so
///   reports an implausibly small error instead of a genuine one.
/// - **`p50_error_m`**: measured `5.587935447692871e-9` m. Pinned band `(1e-10, 1e-7)` m.
///   *Lower* bound sits just above this pipeline's own genuine `f64`-rounding noise floor for
///   ECEF-scale positions (magnitude ~1e6-1e7 m, `f64`'s own ~15-17 significant decimal
///   digits implying an absolute floor on the order of `1e-10`-`1e-9` m): a `p50` at or below
///   that floor would mean the comparison stopped measuring anything real, the same
///   vacuous-success failure mode as `max_error_m`'s own lower bound. *Upper* bound (~18x
///   measured) still catches a genuine convergence regression while leaving room for
///   ordinary platform noise at this near-zero scale.
/// - **`p99_error_m`**: measured `0.00008517093334593117` m. Pinned band `(1e-6, 1e-3)` m --
///   about 85x below and 12x above the measured value, sized to `p99`'s own measured order of
///   magnitude (between `p50`'s and `max`'s) rather than reused from either sibling bound,
///   for the same upper/lower reasons as `max_error_m` above.
///
/// # Matched/unmatched counts
///
/// `900` matched epochs (`EXPECTED_MEASUREMENT_COUNT`, this file's own existing constant)
/// and `0` unmatched, asserted as exact integers, never a bound -- these are counts, not
/// floating-point measurements; an off-by-one here is always a real bug, never platform
/// noise.
#[test]
fn config_hash_and_error_statistics_are_pinned_question_207() {
    const EXPECTED_CONFIG_HASH_HEX: &str = "7a2d33df306ae9861161e1c64eb2cf79c67e7657e419b93fa9365105a53bb06b";

    let cfg = demo_ground_segment_config();
    let hash_hex = av_edge::hash::hex_encode(&cfg.config_hash());
    assert_eq!(
        hash_hex, EXPECTED_CONFIG_HASH_HEX,
        "TrackConfig::config_hash() for the demo fixture's own configuration drifted: measured {hash_hex}, pinned {EXPECTED_CONFIG_HASH_HEX} (question 207)"
    );

    let (updates, truth) = run_once("pinned-accuracy");
    let report = compare_to_truth(&updates, &truth);
    // Question 148: the actual measured error, printed at full precision, not only asserted
    // against and not only the 3-decimal `summary_line()` a human reads at a glance.
    println!("{}", report.summary_line());
    println!(
        "config_hash={hash_hex} matched={} unmatched={} max_error_m={:?} p50_error_m={:?} p99_error_m={:?}",
        report.series.len(),
        report.unmatched_updates,
        report.max_error_m,
        report.p50_error_m,
        report.p99_error_m,
    );

    assert_eq!(
        report.series.len(),
        EXPECTED_MEASUREMENT_COUNT,
        "matched-epoch count drifted: measured {}, pinned {EXPECTED_MEASUREMENT_COUNT}",
        report.series.len()
    );
    assert_eq!(report.unmatched_updates, 0, "unmatched_updates drifted: measured {}, pinned 0", report.unmatched_updates);

    const MAX_LO: f64 = 1e-4;
    const MAX_HI: f64 = 1e-2;
    assert!(
        report.max_error_m > MAX_LO && report.max_error_m < MAX_HI,
        "max_error_m out of its pinned band ({MAX_LO}, {MAX_HI}) m: measured {} m (round 2/3 recorded ~0.00187 m)",
        report.max_error_m
    );

    const P50_LO: f64 = 1e-10;
    const P50_HI: f64 = 1e-7;
    assert!(
        report.p50_error_m > P50_LO && report.p50_error_m < P50_HI,
        "p50_error_m out of its pinned band ({P50_LO}, {P50_HI}) m: measured {} m (round 2/3 recorded ~5.6e-9 m)",
        report.p50_error_m
    );

    const P99_LO: f64 = 1e-6;
    const P99_HI: f64 = 1e-3;
    assert!(
        report.p99_error_m > P99_LO && report.p99_error_m < P99_HI,
        "p99_error_m out of its pinned band ({P99_LO}, {P99_HI}) m: measured {} m (round 2/3 recorded ~8.5e-5 m)",
        report.p99_error_m
    );
}
