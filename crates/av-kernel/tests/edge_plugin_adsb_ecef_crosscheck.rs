//! Cross-checks `av_edge::plugin::adsb::geodetic_to_ecef_m` -- the WGS-84 geodetic-to-ECEF
//! transform the second plugin (question 200(a)) uses to convert every ADS-B CSV row's
//! `lat_deg`/`lon_deg`/`geo_alt_m` into a `Measurement.z` -- against this crate's own
//! `av_kernel::drm::ground::geodetic_to_ecef_m`, over every row of the committed fixture.
//!
//! This is the same precedent `edge_plugin_codec_crosscheck.rs` set for the CCSDS decoder
//! (question 205): `av-edge` may not depend on `av-kernel` at all, so it *reproduces* the
//! standard WGS-84 closed-form transform rather than importing this crate's copy
//! (`crates/av-edge/src/plugin/adsb.rs`'s own module doc has the formula and the
//! reasoning); this file is what proves the two independent copies still agree, byte for
//! byte in the sense that matters -- numerically, to a stated tolerance -- for every row a
//! real replay would ever convert. `av-kernel` is allowed to dev-depend on `av-edge`
//! (`crates/av-kernel/Cargo.toml`'s own comment on that entry, extended by question 205);
//! the reverse is not true, which is exactly why this test lives here and not in
//! `crates/av-edge/tests/`.
//!
//! No network, no GMAT call, no filesystem write -- only a read of the committed CSV fixture
//! (question 154: offline and deterministic).

use std::path::PathBuf;

use av_edge::plugin::adsb::AdsbCsvSource;
use av_edge::plugin::{BatchingRule, MeasurementSource, Pacing, PluginConfig};
use av_kernel::drm::ground::geodetic_to_ecef_m as kernel_geodetic_to_ecef_m;

/// `av-edge`'s own fixture directory, read from `crates/av-kernel`'s own
/// `CARGO_MANIFEST_DIR` -- the same relative-path convention
/// `edge_plugin_codec_crosscheck.rs::fixtures_dir` already established.
fn fixture_csv_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../av-edge/tests/fixtures/adsb/sample.csv")
}

/// Reads the committed fixture and drops its trailing, deliberately-invalid last line (an
/// out-of-range latitude -- `crates/av-edge/tests/fixtures/adsb/README.md`'s own pinned
/// row), the same "valid prefix" convention `crates/av-edge/tests/plugin_replay_adsb.rs::
/// valid_fixture_csv_bytes` uses, so this file's own row-by-row parse (below) only ever
/// sees rows `av_edge::plugin::adsb::AdsbCsvSource` itself would accept.
fn valid_rows() -> Vec<(String, i64, f64, f64, f64)> {
    let text = std::fs::read_to_string(fixture_csv_path()).expect("reading tests/fixtures/adsb/sample.csv");
    let mut lines: Vec<&str> = text.lines().collect();
    let last = lines.pop().expect("fixture has data lines");
    assert_eq!(last, "a00001,1700000075,95.5000,-117.7750,10075.0", "the fixture's own documented bad row must be its last line");

    let mut rows = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        if idx == 0 {
            continue; // header
        }
        let fields: Vec<&str> = line.trim().split(',').collect();
        assert_eq!(fields.len(), 5, "line {}: {:?}", idx + 1, line);
        let icao24 = fields[0].to_string();
        let utc_unix_s: i64 = fields[1].parse().unwrap();
        let lat_deg: f64 = fields[2].parse().unwrap();
        let lon_deg: f64 = fields[3].parse().unwrap();
        let geo_alt_m: f64 = fields[4].parse().unwrap();
        rows.push((icao24, utc_unix_s, lat_deg, lon_deg, geo_alt_m));
    }
    rows
}

fn placeholder_codec_bytes() -> Vec<u8> {
    PluginConfig::encode_codec(&av_edge::pb::PacketCodec {
        id: "unused-adsb-placeholder".to_string(),
        apid: 0,
        is_command: false,
        secondary_header_bytes: 0,
        user_data_bytes: 0,
        description: String::new(),
        fields: vec![],
    })
}

fn config() -> PluginConfig {
    PluginConfig {
        producer_id: "demo-adsb-replay-plugin".to_string(),
        plugin_version: "0.1.0".to_string(),
        instance: "adsb".to_string(),
        port: "csv_replay".to_string(),
        direction: av_edge::pb::PortDirection::Out as i32,
        codec_bytes: placeholder_codec_bytes(),
        component_fields: vec!["x".to_string(), "y".to_string(), "z".to_string()],
        frame_id: av_edge::plugin::adsb::FRAME_ID.to_string(),
        sensor_id: "adsb-receiver".to_string(),
        measurement_id: "adsb_position".to_string(),
        shard_key: "adsb-demo".to_string(),
        noise_r: vec![625.0, 0.0, 0.0, 0.0, 625.0, 0.0, 0.0, 0.0, 625.0],
        label_bytes: PluginConfig::encode_label(&av_edge::pb::Label { marking: "UNCLASSIFIED".to_string(), caveats: vec![] }),
        clearance: "UNCLASSIFIED".to_string(),
        leaf_fingerprint_sha256: String::new(),
        batching: BatchingRule::PerEpoch,
        pacing: Pacing::AsFastAsPossible,
    }
}

/// Cross-checks `av_edge::plugin::adsb::geodetic_to_ecef_m`'s actual output (via a real
/// `AdsbCsvSource::from_csv` run over the committed fixture) against `av_kernel::drm::
/// ground::geodetic_to_ecef_m`, called here directly on the same rows -- parsed
/// independently in this file (`valid_rows`, above) rather than reusing `av_edge`'s own
/// parser, so this is a genuine cross-check of the *transform*, not a tautology that only
/// proves `av_edge` calls itself consistently. Every measurement's `entity_hint` carries
/// its own `icao24` (`crate::plugin::adsb`'s own module doc) and every epoch's ordering
/// within `AdsbCsvSource::groups()` is ascending TAI, so rows are matched back to this
/// file's own by-row ground truth by `(icao24, epoch_tai_ns)`.
#[test]
fn adsb_ecef_conversion_matches_av_kernel_geodetic_to_ecef_m_within_tolerance() {
    let rows = valid_rows();
    assert_eq!(rows.len(), 45, "3 aircraft x 15 epochs, the bad trailing row excluded");

    let cfg = config();
    let csv_bytes = {
        // Rebuild the exact "valid prefix" bytes AdsbCsvSource is fed -- same rows,
        // same file, just without the bad trailing line (see valid_rows' own doc).
        let text = std::fs::read_to_string(fixture_csv_path()).unwrap();
        let mut lines: Vec<&str> = text.lines().collect();
        lines.pop();
        let mut out = lines.join("\n");
        out.push('\n');
        out.into_bytes()
    };
    let source = AdsbCsvSource::from_csv(&csv_bytes, &cfg).expect("the fixture's 45 valid rows parse");

    use std::collections::BTreeMap;
    use av_cdm::time::Tai;
    let mut by_icao_epoch: BTreeMap<(String, i64), [f64; 3]> = BTreeMap::new();
    for (_, measurements) in source.groups() {
        for m in measurements {
            by_icao_epoch.insert((m.entity_hint.clone(), m.epoch_ns), [m.z[0], m.z[1], m.z[2]]);
        }
    }
    assert_eq!(by_icao_epoch.len(), 45);

    let mut max_deviation_m: f64 = 0.0;
    let mut compared = 0usize;
    for (icao24, utc_unix_s, lat_deg, lon_deg, geo_alt_m) in &rows {
        let epoch_tai_ns = Tai::from_utc_nanos(utc_unix_s * 1_000_000_000).as_nanos();
        let kernel_ecef = kernel_geodetic_to_ecef_m(lat_deg.to_radians(), lon_deg.to_radians(), *geo_alt_m);
        let edge_ecef = by_icao_epoch.get(&(icao24.clone(), epoch_tai_ns)).unwrap_or_else(|| panic!("no av_edge measurement for icao24={icao24:?} epoch_tai_ns={epoch_tai_ns}"));

        let dx = kernel_ecef[0] - edge_ecef[0];
        let dy = kernel_ecef[1] - edge_ecef[1];
        let dz = kernel_ecef[2] - edge_ecef[2];
        let deviation = (dx * dx + dy * dy + dz * dz).sqrt();
        max_deviation_m = max_deviation_m.max(deviation);
        compared += 1;
    }

    const TOLERANCE_M: f64 = 1e-6;
    println!("adsb WGS-84 geodetic-to-ECEF cross-check: compared {compared} rows against av_kernel::drm::ground::geodetic_to_ecef_m");
    println!("adsb WGS-84 geodetic-to-ECEF cross-check: worst deviation = {max_deviation_m:e} m (tolerance {TOLERANCE_M:e} m)");
    assert_eq!(compared, 45);
    assert!(max_deviation_m <= TOLERANCE_M, "max deviation {max_deviation_m} m exceeds the {TOLERANCE_M} m tolerance -- av_edge::plugin::adsb::geodetic_to_ecef_m disagrees with av_kernel::drm::ground::geodetic_to_ecef_m");
}
