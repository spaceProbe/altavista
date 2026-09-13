//! Milestone E4's own acceptance test list, the pure/no-wire half: "a run's measurements
//! arrive at the ingest byte for byte, with the batch count and the chain head pinned for
//! the demo DRM" -- everything this file can prove without a live `EdgeIngest` server (the
//! wire half lives in `crates/av-ingest/tests/plugin_wire.rs`).
//!
//! Fixture: `tests/fixtures/ground_segment/{run_products.pb,port_traffic.pb}`, committed,
//! produced once by `cargo test -p av-kernel --test generate_e4a_ground_segment_fixture --
//! --ignored --nocapture` against `drms/demo_ground_segment*.yaml` -- see that file's own
//! module doc for the exact command and `tests/fixtures/ground_segment/README.md` for the
//! pinned hashes. No network, no GMAT call, no filesystem write anywhere in this file
//! itself -- only reads of the two committed files (question 154: offline and
//! deterministic).
//!
//! # Question 205: the `av_kernel` cross-check moved out of this file
//!
//! `av-edge` may not build `gmat-sys` (question 205's ruling), so the one test this file
//! used to carry that named `av_kernel` at all --
//! `decoded_measurements_match_av_kernel_codec_element_for_element`, cross-checking
//! `crate::plugin::packet::decode_numeric_fields` against `av_kernel::codec::decode_packet`
//! byte for byte -- is now `crates/av-kernel/tests/edge_plugin_codec_crosscheck.rs`, with
//! `av-edge` a dev-dependency of `av-kernel` there instead of the reverse. Everything else
//! in this file needs no `av_kernel` and stayed put; `decoded_positions_match_the_flight_
//! instances_truth_trajectory_within_tolerance` below reads `RunProducts.trajectories` (a
//! plain protobuf field of this crate's own fixture, decoded via `av_edge::pb`), which was
//! never an `av_kernel` dependency.

use std::collections::BTreeMap;
use std::path::PathBuf;

use av_edge::hash;
use av_edge::pb;
use av_edge::plugin::{self, BatchBuilder, BatchingRule, MeasurementSource, Pacing, PluginConfig, PortTrafficSource};
use av_edge::sign;

const TEST_KEY_PEM: &[u8] = include_bytes!("fixtures/test_signing_key.pem");
const TEST_PUB_PEM: &[u8] = include_bytes!("fixtures/test_signing_key.pub.pem");

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ground_segment")
}

fn load_run_products() -> pb::RunProducts {
    let bytes = std::fs::read(fixtures_dir().join("run_products.pb")).expect("reading run_products.pb");
    <pb::RunProducts as prost::Message>::decode(bytes.as_slice()).expect("run_products.pb decodes as altavista.v1.RunProducts")
}

fn load_verified_port_traffic_log(expected_hash: &str) -> pb::PortTrafficLog {
    let bytes = std::fs::read(fixtures_dir().join("port_traffic.pb")).expect("reading port_traffic.pb");
    plugin::verify_port_traffic_log(&bytes, expected_hash).expect("the committed port_traffic.pb must match RunProducts.port_traffic_hash")
}

/// `drms/demo_ground_segment_flight.system.yaml`'s own declared `flight_tm_out_codec`,
/// transcribed field for field from that YAML (apid 500, no secondary header, 24
/// user-data bytes, three FLOAT64 fields `x`/`y`/`z` at bit offsets 0/64/128, scale 1.0,
/// offset 0.0) -- this test's own ground truth for "what this DRM actually declared",
/// independent of anything `crate::plugin` itself does with it.
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

/// A 3x3 diagonal covariance, SPD, declared for this test -- not the fixture's own truth
/// (this DRM never declared a noise model for its telemetry; this is this *plugin's* own
/// declared measurement-noise assumption, an ordinary configuration knob).
const NOISE_R: [f64; 9] = [100.0, 0.0, 0.0, 0.0, 100.0, 0.0, 0.0, 0.0, 100.0];

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
        noise_r: NOISE_R.to_vec(),
        label_bytes: PluginConfig::encode_label(&label),
        clearance: "CUI".to_string(),
        leaf_fingerprint_sha256: String::new(),
        batching: BatchingRule::PerEpoch,
        pacing: Pacing::AsFastAsPossible,
    }
}

// -----------------------------------------------------------------------------------------
// Pinned numbers -- E4's own acceptance test list, verbatim: "the batch count and the
// chain head pinned for the demo DRM."
// -----------------------------------------------------------------------------------------

/// `drms/demo_ground_segment.drm.yaml` runs 900s at 1Hz; `ConstantAccelModel` broadcasts
/// its own position on `tm_out` every step -- one `PortTrafficRecord` per second, all OUT,
/// all from `"flight"`, none dropped (confirmed by this file's own `matching_record_count`
/// test below before this constant is trusted).
const EXPECTED_BATCH_COUNT: usize = 900;

/// `BatchBuilder::build_batches`'s own final `batch_hash`, hex, for the full 900-batch
/// chain built from [`config`] against the committed fixture and `tests/fixtures/
/// test_signing_key.pem` -- computed once (`chain_head_is_pinned_for_the_demo_drm` below
/// prints it) and pinned here. A content hash, not a signature: deterministic across runs
/// (unlike the ECDSA signature bytes themselves -- `av_edge`'s own crate module doc).
const EXPECTED_CHAIN_HEAD_HEX: &str = "d1d80d0b6cc9228aaa7479864b8a89f18be88382fb3772bd3c4cc3d7c2dc2698";

fn signing_key() -> openssl::ec::EcKey<openssl::pkey::Private> {
    sign::load_signing_key(TEST_KEY_PEM).unwrap()
}
fn verify_key() -> openssl::ec::EcKey<openssl::pkey::Public> {
    av_edge::verify::load_verifying_key(TEST_PUB_PEM).unwrap()
}

fn build_source_and_batches() -> (pb::RunProducts, PortTrafficSource, Vec<pb::MeasurementBatch>) {
    let run_products = load_run_products();
    let log = load_verified_port_traffic_log(&run_products.port_traffic_hash);
    let cfg = config();
    let source = PortTrafficSource::from_log(&log, &cfg).expect("source builds from the verified log");
    let builder = BatchBuilder::new(cfg.batching).unwrap();
    let provenance = cfg.batch_provenance(&run_products.run_id, run_products.provenance.as_ref().unwrap().created_tai_ns);
    let key = signing_key();
    let batches = builder.build_batches_for_config(&source, &cfg, &provenance, &key).expect("batches build and sign");
    (run_products, source, batches)
}

#[test]
fn matching_record_count_is_nine_hundred_one_per_second_of_the_ninehundred_second_run() {
    let (_run_products, source, _batches) = build_source_and_batches();
    let total_measurements: usize = source.groups().iter().map(|(_, ms)| ms.len()).sum();
    assert_eq!(source.groups().len(), EXPECTED_BATCH_COUNT, "one group per recorded epoch under PerEpoch batching");
    assert_eq!(total_measurements, EXPECTED_BATCH_COUNT, "exactly one measurement per second, none dropped, none doubled");
}

#[test]
fn batch_count_and_chain_head_are_pinned_for_the_demo_drm() {
    let (_run_products, _source, batches) = build_source_and_batches();
    assert_eq!(batches.len(), EXPECTED_BATCH_COUNT);

    let head_hex = hash::hex_encode(&batches.last().unwrap().batch_hash);
    println!("chain head (batch_hash of the last batch), hex: {head_hex}");
    assert_eq!(head_hex, EXPECTED_CHAIN_HEAD_HEX, "the demo DRM's own chain head must be pinned, not merely printed");
}

#[test]
fn walk_chain_verifies_the_whole_emitted_chain() {
    let (_run_products, _source, batches) = build_source_and_batches();
    let vk = verify_key();
    let result = av_edge::chain::walk_chain("demo-ground-segment-flight-plugin", &batches, &vk);
    assert!(result.ok, "{result:?}");
    assert_eq!(result.checked, EXPECTED_BATCH_COUNT as u64);
}

/// E5's own precondition (this task's brief, verbatim): the decoded positions must
/// correspond to the run's own truth trajectory. Measures and PRINTS the actual maximum
/// deviation, rather than merely asserting it is small, so the manager can see the real
/// number, not just a pass/fail.
#[test]
fn decoded_positions_match_the_flight_instances_truth_trajectory_within_tolerance() {
    let run_products = load_run_products();
    let log = load_verified_port_traffic_log(&run_products.port_traffic_hash);
    let cfg = config();
    let source = PortTrafficSource::from_log(&log, &cfg).expect("source builds");

    let trajectory = run_products.trajectories.get("flight").expect("the flight instance must have a truth Trajectory (state_dim() == 6)");
    let mut truth_by_epoch: BTreeMap<i64, [f64; 3]> = BTreeMap::new();
    for sample in &trajectory.samples {
        truth_by_epoch.insert(sample.tai_ns, [sample.mean[0], sample.mean[1], sample.mean[2]]);
    }

    const TOLERANCE_M: f64 = 1e-6;
    let mut max_deviation_m: f64 = 0.0;
    let mut compared = 0usize;
    for (epoch, measurements) in source.groups() {
        let Some(truth) = truth_by_epoch.get(epoch) else { continue };
        let m = &measurements[0];
        let dx = m.z[0] - truth[0];
        let dy = m.z[1] - truth[1];
        let dz = m.z[2] - truth[2];
        let deviation = (dx * dx + dy * dy + dz * dz).sqrt();
        max_deviation_m = max_deviation_m.max(deviation);
        compared += 1;
    }
    assert!(compared > 0, "at least one epoch must have a matching truth sample to compare against");
    println!("decoded-position-vs-truth-trajectory maximum deviation over {compared} matched epochs: {max_deviation_m:e} m (tolerance {TOLERANCE_M:e} m)");
    assert!(max_deviation_m <= TOLERANCE_M, "max deviation {max_deviation_m} m exceeds the {TOLERANCE_M} m tolerance -- E5 cannot work if this does not hold");
}

/// E4's own required test: "a zero-measurement group still produces a valid, signed,
/// chain-advancing batch." A hand-built `MeasurementSource`, not the fixture -- the real
/// fixture never has an empty group (every second has exactly one measurement), so this
/// is deliberately synthetic.
struct FixedGroups(Vec<(i64, Vec<pb::Measurement>)>);
impl MeasurementSource for FixedGroups {
    fn groups(&self) -> &[(i64, Vec<pb::Measurement>)] {
        &self.0
    }
}

#[test]
fn a_zero_measurement_group_still_produces_a_valid_signed_chain_advancing_batch() {
    let cfg = config();
    let source = FixedGroups(vec![(1_000, vec![]), (2_000, vec![pb::Measurement { measurement_id: "flight_position".to_string(), z: vec![1.0, 2.0, 3.0], epoch_ns: 2_000, ..Default::default() }])]);
    let builder = BatchBuilder::new(BatchingRule::PerEpoch).unwrap();
    let provenance = cfg.batch_provenance("run-x", 0);
    let key = signing_key();
    let batches = builder.build_batches_for_config(&source, &cfg, &provenance, &key).unwrap();
    assert_eq!(batches.len(), 2);
    assert!(batches[0].measurements.is_empty(), "the first batch is the zero-measurement heartbeat");
    assert!(!batches[0].signature.is_empty(), "a zero-measurement batch must still be signed");
    assert_eq!(batches[0].prev_hash, hash::GENESIS, "the first batch of a producer always chains from GENESIS");
    assert_eq!(batches[1].prev_hash, batches[0].batch_hash, "the chain must still advance across a zero-measurement batch");

    let vk = verify_key();
    let result = av_edge::chain::walk_chain(&cfg.producer_id, &batches, &vk);
    assert!(result.ok, "{result:?}");
    assert_eq!(result.checked, 2);
}

/// "The same config replayed twice produces byte-identical batch bodies" -- signatures
/// differ (ECDSA nonces are random), so this compares `hash::canonical_body_bytes` and
/// `batch_hash`, never `signature`.
#[test]
fn replaying_the_same_config_twice_produces_byte_identical_batch_bodies() {
    let (run_products_a, source_a, batches_a) = build_source_and_batches();
    let (run_products_b, source_b, batches_b) = build_source_and_batches();
    assert_eq!(run_products_a.port_traffic_hash, run_products_b.port_traffic_hash);
    assert_eq!(source_a.groups(), source_b.groups(), "decoding is a pure function of the same inputs");
    assert_eq!(batches_a.len(), batches_b.len());
    for (a, b) in batches_a.iter().zip(batches_b.iter()) {
        assert_eq!(hash::canonical_body_bytes(a), hash::canonical_body_bytes(b), "canonical body bytes must be byte-identical across replays");
        assert_eq!(a.batch_hash, b.batch_hash, "batch_hash is a deterministic function of the body");
        // Signatures are NOT expected to match -- OpenSSL's ECDSA nonce is random.
    }
    // At least prove signatures really can differ, so this test does not vacuously pass
    // against an implementation that (bug) made signing deterministic by accident -- not
    // required by anything, but a real ECDSA signer will essentially never collide.
    let any_signature_differs = batches_a.iter().zip(batches_b.iter()).any(|(a, b)| a.signature != b.signature);
    assert!(any_signature_differs, "expected at least one signature to differ across two independent signing calls (ECDSA nonces are random)");
}

/// `PluginConfig`'s own manifest and the batches it signs must never be able to drift
/// apart -- both are derived from the same config.
#[test]
fn manifest_and_batches_are_derived_from_the_same_config() {
    let cfg = config();
    let manifest = cfg.manifest().unwrap();
    assert_eq!(manifest.producer_id, cfg.producer_id);
    assert_eq!(manifest.clearance, cfg.clearance);
    assert_eq!(manifest.shard_keys, vec![cfg.shard_key.clone()]);
    assert_eq!(manifest.label.unwrap(), cfg.label().unwrap());
    assert_eq!(manifest.output_schemas[0].measurement_id, cfg.measurement_id);
    assert_eq!(manifest.output_schemas[0].z_len as usize, cfg.component_fields.len());

    let (_run_products, _source, batches) = build_source_and_batches();
    for batch in &batches {
        assert_eq!(batch.label.as_ref().unwrap(), &cfg.label().unwrap());
        assert_eq!(batch.shard_key, cfg.shard_key);
        for m in &batch.measurements {
            assert_eq!(m.shard_key, cfg.shard_key, "Measurement.shard_key must agree with MeasurementBatch.shard_key (BATCH_REJECTION_SHARD_MISMATCH)");
        }
    }
}

#[test]
fn config_hash_is_deterministic_and_sensitive_to_content() {
    let cfg = config();
    let mut other = cfg.clone();
    other.shard_key = "a-different-shard".to_string();
    assert_eq!(cfg.config_hash(), cfg.config_hash(), "hashing the same config twice must agree");
    assert_ne!(cfg.config_hash(), other.config_hash(), "changing a declared field must change the hash");
}

#[test]
fn pacing_due_at_is_pure_and_never_sleeps() {
    // AsFastAsPossible: always immediately due, regardless of epoch spacing.
    let afap = Pacing::AsFastAsPossible;
    assert_eq!(afap.due_at(0, 1_000, 1_000), 0);
    assert_eq!(afap.due_at(41, 1_000, 900_000_000_000), 0);

    // RealTime{scale: 1.0}: reproduces the original cadence exactly.
    let rt1 = Pacing::RealTime { scale: 1.0 };
    assert_eq!(rt1.due_at(0, 1_000, 1_000), 0);
    assert_eq!(rt1.due_at(1, 1_000, 1_000_000_001_000), 1_000_000_000_000);

    // RealTime{scale: 2.0}: twice as fast -- half the wall-clock offset.
    let rt2 = Pacing::RealTime { scale: 2.0 };
    assert_eq!(rt2.due_at(1, 1_000, 1_000_000_001_000), 500_000_000_000);
}

#[test]
fn validate_refuses_a_noise_matrix_that_is_not_spd() {
    let mut cfg = config();
    cfg.noise_r = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0]; // not SPD
    assert!(cfg.validate().is_err());
}

#[test]
fn validate_refuses_a_zero_sized_batching_rule() {
    let mut cfg = config();
    cfg.batching = BatchingRule::PerNMeasurements(0);
    assert!(cfg.validate().is_err());
}
