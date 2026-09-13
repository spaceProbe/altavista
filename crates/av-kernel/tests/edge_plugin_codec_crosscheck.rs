//! Question 205's ruling: `av-edge` may not build `gmat-sys`, so the one GMAT-dependent test
//! in `crates/av-edge/tests/plugin_replay.rs` (E4's own acceptance test list) moved here --
//! everything else in that file needs no `av_kernel` and stayed put (confirmed by grep: this
//! was the only test function in that file naming `av_kernel` at all).
//!
//! This is the one place in this crate's own test suite that dev-depends on `av-edge`
//! (`crates/av-kernel/Cargo.toml`'s own comment on the entry). All this file proves is that
//! `av_edge::plugin::packet::decode_numeric_fields` -- `av-edge`'s own thin adapter over
//! `av_codec::decode_packet`, not a from-scratch reimplementation any more (question 205;
//! see `crates/av-edge/src/plugin/packet.rs`'s module doc) -- still agrees, element for
//! element, with `av_kernel::codec::decode_packet` (the *same* `av_codec` crate, re-exported
//! here as `codec` -- `crates/av-kernel/src/lib.rs`) for every one of this fixture's 900
//! records. That agreement is the whole reason `av-edge`'s plugin and `av-kernel` can each
//! decode the identical CCSDS packets while depending on neither `gmat-sys` nor each other.
//!
//! # Why a kernel test reads an `av-edge` fixture
//!
//! `crates/av-edge/tests/fixtures/ground_segment/{run_products.pb,port_traffic.pb}` is
//! `av-edge`'s own committed artifact (see that directory's own `README.md` for the
//! generation command and pinned hashes) -- this file only borrows it, read-only, by an
//! explicit relative path from THIS crate's own `CARGO_MANIFEST_DIR`
//! (`crates/av-kernel`), exactly the way `tests/generate_e4a_ground_segment_fixture.rs`'s
//! own `av_edge_fixture_dir` already walks from `crates/av-kernel` across to
//! `crates/av-edge/tests/fixtures/ground_segment` to *write* that same directory. No network,
//! no GMAT call, no filesystem write anywhere in this file -- only reads of the two
//! committed files (question 154: offline and deterministic).

use std::collections::BTreeMap;
use std::path::PathBuf;

use av_edge::pb;
use av_edge::plugin::{self, BatchingRule, MeasurementSource, Pacing, PluginConfig, PortTrafficSource};

/// `av-edge`'s own fixture directory, read from `crates/av-kernel`'s own
/// `CARGO_MANIFEST_DIR` -- see this file's module doc's "Why a kernel test reads an
/// `av-edge` fixture" section.
fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../av-edge/tests/fixtures/ground_segment")
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
/// offset 0.0) -- identical to `crates/av-edge/tests/plugin_replay.rs`'s own copy (this
/// test's ground truth for "what this DRM actually declared" did not change with the move).
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

/// `drms/demo_ground_segment.drm.yaml` runs 900s at 1Hz -- one `PortTrafficRecord` per
/// second, all matched, none dropped (`crates/av-edge/tests/plugin_replay.rs`'s own
/// `matching_record_count_is_nine_hundred_one_per_second_of_the_ninehundred_second_run`
/// establishes this; this file only needs the same count to size its own assertion).
const EXPECTED_BATCH_COUNT: usize = 900;

/// Cross-check against `av_kernel::codec::decode_packet` directly -- proves
/// `av_edge::plugin::packet::decode_numeric_fields` (this plugin's own adapter over
/// `av_codec`, question 205 -- see that module's own doc comment) agrees with the real,
/// shipped decoder byte for byte, element for element, for every one of this fixture's 900
/// records. Moved here, verbatim in substance, from `crates/av-edge/tests/plugin_replay.rs`
/// (this file's own module doc has the reason): this is the one place in this task's own
/// test suite that is allowed to depend on `av-kernel` (a dev-dependency, here in
/// `av-kernel`'s own test suite rather than `av-edge`'s, now that `av-edge` may not build
/// `gmat-sys` at all).
#[test]
fn decoded_measurements_match_av_kernel_codec_element_for_element() {
    let run_products = load_run_products();
    let log = load_verified_port_traffic_log(&run_products.port_traffic_hash);
    let cfg = config();
    let source = PortTrafficSource::from_log(&log, &cfg).expect("source builds");

    let apid_map = av_kernel::codec::validate_system_packet_codecs(&[flight_codec()]).expect("codec validates");
    let mut truth_by_epoch: BTreeMap<i64, (f64, f64, f64)> = BTreeMap::new();
    for record in &log.records {
        if record.instance != "flight" || record.port != "tm_out" || record.direction != pb::PortDirection::Out as i32 {
            continue;
        }
        let decoded = av_kernel::codec::decode_packet(&apid_map, &record.payload).expect("av_kernel::codec decodes the same packet");
        let get = |name: &str| match decoded.fields.get(name).expect("field present") {
            av_kernel::codec::FieldValue::Numeric(v) => *v,
            av_kernel::codec::FieldValue::Bytes(_) => panic!("x/y/z are FLOAT64, never BYTES"),
        };
        truth_by_epoch.insert(record.tai_ns, (get("x"), get("y"), get("z")));
    }

    assert_eq!(source.groups().len(), truth_by_epoch.len(), "same number of decoded epochs on both sides");
    let mut compared = 0usize;
    for (epoch, measurements) in source.groups() {
        assert_eq!(measurements.len(), 1, "one measurement per epoch for this fixture");
        let m = &measurements[0];
        let (tx, ty, tz) = truth_by_epoch[epoch];
        assert_eq!(m.z, vec![tx, ty, tz], "epoch {epoch}: av_edge::plugin's own decode must equal av_kernel::codec's, element for element");
        compared += 1;
    }
    assert_eq!(compared, EXPECTED_BATCH_COUNT);
    println!("cross-checked {compared} decoded records against av_kernel::codec::decode_packet, byte for byte");
}
