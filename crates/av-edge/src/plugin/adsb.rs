//! The second plugin: an ADS-B replay from a committed, synthetic CSV sample (question
//! 200(a); `docs/edge-plan.md` milestone E4's "second plugin after the first one lands").
//!
//! # What this plugin replays, and why
//!
//! Real ADS-B (1090ES) airborne position reports (`DF17`, type codes 9-18/20-22) are
//! broadcast in the clear by every cooperative transponder; a ground receiver decodes each
//! message into an aircraft identity, a reception time, and a position. This source reads
//! exactly that shape from a CSV a real feed decoder (`dump1090`, `pyModeS`, the OpenSky
//! Network REST API, ...) would already have produced -- **not** raw Mode S bits, and
//! **not** a live receiver: `tests/fixtures/adsb/sample.csv` is small, synthetic, and
//! committed (question 154: no network, no recorded live feed), with generation and every
//! pinned number documented in `tests/fixtures/adsb/README.md`.
//!
//! ## Column shape: what is modeled, and what is deliberately left out
//!
//! `icao24,utc_unix_s,lat_deg,lon_deg,geo_alt_m` -- five columns, one row per received
//! position report:
//!
//! - `icao24`: the transponder's 24-bit ICAO address, six hex digits -- carried on
//!   [`pb::Measurement::entity_hint`] ("a cooperative transponder id resolved through the
//!   catalog", `core.proto`'s own doc comment for that field, verbatim the ADS-B case).
//! - `utc_unix_s`: the receiver's UTC reception timestamp, integer Unix-epoch seconds
//!   (matching the granularity OpenSky Network's own REST API reports it at). Real ADS-B
//!   messages carry no absolute timestamp of their own -- the *receiver* stamps each
//!   message on arrival -- so this column models the receiver's clock, not a wire field.
//! - `lat_deg`/`lon_deg`: geodetic WGS-84 latitude/longitude, degrees, **already CPR
//!   decoded**. A real DF17 position message splits latitude/longitude across an
//!   even/odd pair of Compact Position Reporting (CPR) frames that must be reconciled
//!   before a coordinate exists at all; that reconciliation is a separate, well-tested
//!   decoding step (what `pyModeS`/`dump1090` already do) and is not this source's
//!   concern -- this column is the CPR decoder's *output*, exactly like `lat_deg`/
//!   `lon_deg` on any real decoder's own record shape.
//! - `geo_alt_m`: **geometric** altitude (height above the WGS-84 ellipsoid, HAE),
//!   metres -- not barometric. This is a deliberate choice, not an oversight: [`
//!   geodetic_to_ecef_m`]'s ellipsoidal transform takes a height *above the ellipsoid* as
//!   its `height_m` input, and barometric altitude (referenced to standard atmospheric
//!   pressure, not the WGS-84 ellipsoid) is not that -- treating a barometric reading as
//!   an ellipsoidal height would silently corrupt every measurement's `z` component by
//!   whatever the geoid/QNH offset happens to be at that place and day. Most ADS-B
//!   position messages in the wild (type codes 9-18) actually carry barometric altitude;
//!   geometric altitude needs type codes 20-22 or a separate GNSS-height sub-message. This
//!   source models the geometric-altitude case specifically because it is the physically
//!   correct input to the transform below, and says so here rather than silently treating
//!   barometric input as if it were geometric.
//!
//! Deliberately **not** modeled (a real DF17 stream carries all of this; this replay
//! source does not need it to exercise the measurement/batch/chain path): callsign/
//! identification messages (TC 1-4), CPR's own raw even/odd fields and the odd/even
//! parity bit, velocity messages (TC 19), NIC/NACp/SIL integrity and accuracy category
//! fields (referenced only qualitatively below, to justify [`PluginConfig::noise_r`]'s
//! declared value -- never read from a row), squawk code, surveillance status,
//! capability flags, DF11 all-call replies, and Mode S CRC/parity (a real receiver
//! already discards a message that fails its own CRC before a decoder ever sees it, so
//! there is nothing left to check by the time a row reaches this source).
//!
//! ## Parsing: no new external crate
//!
//! Hand-rolled: split each line on `,`, trim, parse. `std` is enough for five
//! fixed-position, unquoted, comma-free fields over a few dozen rows -- adding a `csv`
//! crate for this would be the exact "new external crate" this round's binding rules
//! refuse (`crate`'s own module doc's "SHA-256 only ... no new external crate at all").
//!
//! ## Frame: WGS-84 geodetic -> Earth-fixed Cartesian (ECEF)
//!
//! [`geodetic_to_ecef_m`] is the same closed-form WGS-84 ellipsoidal transform
//! `av_kernel::drm::ground::geodetic_to_ecef_m` implements (`N = a / sqrt(1 - e^2
//! sin^2(lat))`, `x = (N+h) cos(lat) cos(lon)`, `y = (N+h) cos(lat) sin(lon)`, `z =
//! (N(1-e^2)+h) sin(lat)`) -- **reproduced here, not imported**, because `av-edge` may not
//! depend on `av-kernel` at all (question 205's ruling; `crate::plugin::packet`'s own
//! module doc has the history). `crates/av-kernel/tests/edge_plugin_adsb_ecef_crosscheck.rs`
//! cross-checks this function's output against `av_kernel::drm::ground::geodetic_to_ecef_m`
//! for every row of the committed fixture and prints the worst deviation -- the same
//! precedent `edge_plugin_codec_crosscheck.rs` set for the CCSDS decoder (question 205).
//!
//! `z = [x, y, z]` is emitted under [`FRAME_ID`], a `frame_id` this module mints itself --
//! **not** `av_cdm::spoore_v0::frame`'s fixed literal `"earth.ecef"` (that string is only
//! ever produced by encoding a native `spoore_cdm::Frame::Ecef` value, never something a
//! producer should hard-code). `crates/av-cdm/tests/adsb_frame_resolution.rs` proves
//! [`FRAME_ID`] instead resolves through the real, registry-driven path
//! (`av_cdm::spoore_v0::frame::frame_from_definition`, via `measurement_from_pb_with_
//! frames`): a caller that registers a `FrameDefinition` naming [`FRAME_ID`] with
//! `origin = body("Earth")` and `axes = AXES_KIND_BODY_FIXED` gets back
//! `spoore_cdm::Frame::Ecef`, exactly the frame this module's own `z` values are computed
//! in.
//!
//! ## Time: UTC in the CSV, TAI on the wire
//!
//! `epoch_ns` on every `altavista.v1.Measurement` is TAI (ADR-001 "Time"); a real ADS-B
//! receiver's own clock is UTC. This module converts through `av_cdm::time::Tai::
//! from_utc_nanos` (the platform's one leap-second-table conversion, `crates/av-cdm/src/
//! time.rs`) -- never a hand-rolled fixed offset, and never a TAI value smuggled into the
//! CSV to dodge the conversion (this round's binding rule, verbatim).
//!
//! ## Label and clearance: an open-air broadcast, not a sensitive source
//!
//! ADS-B 1090ES is transmitted in the clear, receivable by anyone with a cheap SDR --
//! there is nothing to protect by marking it as if it were. This module's own test
//! `PluginConfig` (`tests/fixtures/adsb/README.md`'s own pinned config) declares
//! `label.marking = "UNCLASSIFIED"` and `clearance = "UNCLASSIFIED"` (`core.proto`'s own
//! `Label.marking` doc comment lists `"UNCLASSIFIED"` as one of exactly three worked
//! examples, alongside `"CUI"`/`"CUI//SP-EXPT"` -- the first plugin's own fixture used
//! the latter for a different, more sensitive simulated asset), with no caveats.
//!
//! ## Noise: declared, not derived
//!
//! [`PluginConfig::noise_r`] here is an isotropic 3x3 diagonal, `(25 m)^2` per ECEF axis
//! -- a **declared** measurement-noise assumption, not a value computed from this
//! fixture's synthetic rows (which carry no noise at all; they are exact analytic
//! positions, `tests/fixtures/adsb/README.md`'s own generation formula). 25 m 1-sigma is a
//! round, conservative number in the range real-world ADS-B position quality figures
//! (NACp/NIC categories, DO-260B) put a "good" report in -- tens of metres, not the
//! sub-metre precision a dedicated tracking radar might declare. A real deployment would
//! read a report's own NACp/NIC field and pick a tighter or looser covariance per report;
//! this module does not, since the fixture models none of that (see this doc's own "not
//! modeled" list above).
//!
//! ## `PluginConfig` reused unchanged: knobs this source does not read
//!
//! Per this round's binding rule, [`PluginConfig`] is the *same* struct
//! [`crate::plugin::PortTrafficSource`] uses, unmodified. Several of its fields --
//! `instance`, `port`, `direction`, `codec_bytes` -- exist for a `PortTrafficLog`-backed
//! source and this one never reads them; [`PluginConfig::validate`] still requires
//! `instance`/`port` non-empty and `codec_bytes` to decode as a `pb::PacketCodec`, so this
//! module's own test config supplies harmless placeholder values for them (a trivial,
//! empty-fields `PacketCodec`, `instance = "adsb"`, `port = "csv_replay"`) rather than
//! forking `PluginConfig` into a narrower, ADS-B-only shape. `component_fields` **is**
//! read, but only to check its shape: [`AdsbCsvSource::from_csv`] requires it be exactly
//! `["x", "y", "z"]` (this source always emits a 3-component ECEF `z`, computed directly
//! from `lat_deg`/`lon_deg`/`geo_alt_m` rather than looked up by field name the way
//! `PortTrafficSource` looks up packet fields), refusing otherwise
//! ([`PluginError::AdsbComponentFieldsMismatch`]) rather than silently emitting a `z`
//! whose length disagrees with `noise_r`'s own `component_fields.len()^2` shape.
//!
//! ## Same batch path, unchanged
//!
//! [`AdsbCsvSource`] implements [`MeasurementSource`] exactly like
//! [`crate::plugin::PortTrafficSource`] does; [`crate::plugin::BatchBuilder`], `crate::
//! hash`, `crate::sign` and `crate::chain` are not touched by this module at all --
//! there is exactly one hashing/signing/chaining scheme in this crate, reused, not
//! forked.

use std::collections::BTreeMap;

use crate::pb;
use crate::plugin::{MeasurementSource, PluginConfig, PluginError};

/// The `frame_id` this module emits every [`pb::Measurement::frame_id`] under. Deliberately
/// **not** `av_cdm::spoore_v0::frame::ECEF` ("earth.ecef", the fixed literal that adapter's
/// *encoding* direction produces) -- see this module's own doc comment's "Frame" section
/// for why a producer must mint and register its own id and go through the registry-driven
/// resolution path instead of hard-coding that literal.
pub const FRAME_ID: &str = "adsb.earth_ecef";

/// WGS-84 semi-major axis, metres -- the same value `av_kernel::drm::ground::WGS84_A_M`
/// pins (both are the WGS-84 defining constant, reproduced independently on each side of
/// the question-205 crate boundary; see this module's own doc comment).
pub const WGS84_A_M: f64 = 6_378_137.0;
/// WGS-84 flattening -- the same value `av_kernel::drm::ground::WGS84_F` pins.
pub const WGS84_F: f64 = 1.0 / 298.257223563;

/// Geodetic (ellipsoidal) latitude/longitude/height -> Earth-fixed (ECEF) Cartesian
/// position, metres. The standard closed-form WGS-84 transform -- see this module's own
/// doc comment for the formula and the independent cross-check this is pinned against.
pub fn geodetic_to_ecef_m(lat_rad: f64, lon_rad: f64, height_m: f64) -> [f64; 3] {
    let e2 = WGS84_F * (2.0 - WGS84_F);
    let sin_lat = lat_rad.sin();
    let n = WGS84_A_M / (1.0 - e2 * sin_lat * sin_lat).sqrt();
    let x = (n + height_m) * lat_rad.cos() * lon_rad.cos();
    let y = (n + height_m) * lat_rad.cos() * lon_rad.sin();
    let z = (n * (1.0 - e2) + height_m) * sin_lat;
    [x, y, z]
}

/// The three-component `component_fields` shape this source requires
/// (`crate`'s own module doc, "`component_fields` is read, but only to check its shape").
const REQUIRED_COMPONENT_FIELDS: [&str; 3] = ["x", "y", "z"];

fn parse_icao24(line: usize, raw: &str) -> Result<String, PluginError> {
    if raw.len() == 6 && raw.chars().all(|c| c.is_ascii_hexdigit()) {
        Ok(raw.to_string())
    } else {
        Err(PluginError::AdsbInvalidIcao24 { line, value: raw.to_string() })
    }
}

fn parse_number<T: std::str::FromStr>(line: usize, field: &'static str, raw: &str) -> Result<T, PluginError> {
    raw.parse::<T>().map_err(|_| PluginError::AdsbInvalidField { line, field, value: raw.to_string() })
}

/// This milestone's second [`MeasurementSource`]: a small, committed, synthetic ADS-B CSV
/// sample (`tests/fixtures/adsb/sample.csv`; see that directory's own `README.md`),
/// converted row by row into Earth-fixed Cartesian position `Measurement`s and grouped by
/// TAI epoch -- see this module's own doc comment for exactly what is modeled, converted,
/// and refused.
#[derive(Debug, Clone)]
pub struct AdsbCsvSource {
    groups: Vec<(i64, Vec<pb::Measurement>)>,
}

impl AdsbCsvSource {
    /// Parses `csv_bytes` (the exact bytes of a `icao24,utc_unix_s,lat_deg,lon_deg,
    /// geo_alt_m` file, header row first) against `config`, producing one `Measurement`
    /// per data row (`z` = this row's own WGS-84-to-ECEF conversion, `r` = `config.
    /// noise_r` verbatim, `entity_hint` = this row's own `icao24`), grouped by TAI epoch
    /// (a `BTreeMap` keyed by the converted `epoch_ns`, so rows sharing an epoch --
    /// several aircraft reported at the same reception second -- land in one group, and
    /// the CSV's own row order never matters, per `crate::plugin::PortTrafficSource::
    /// from_log`'s identical convention).
    ///
    /// Every field this needs beyond the CSV itself comes from `config` -- change
    /// `config.measurement_id`/`.sensor_id`/`.frame_id`/... and this same function
    /// replays under a different declared identity with no code change, exactly like
    /// `PortTrafficSource::from_log`.
    pub fn from_csv(csv_bytes: &[u8], config: &PluginConfig) -> Result<Self, PluginError> {
        config.validate()?;
        if config.component_fields != REQUIRED_COMPONENT_FIELDS {
            return Err(PluginError::AdsbComponentFieldsMismatch { actual: config.component_fields.clone() });
        }
        let text = std::str::from_utf8(csv_bytes).map_err(|e| PluginError::AdsbInvalidUtf8 { message: e.to_string() })?;

        let mut by_epoch: BTreeMap<i64, Vec<pb::Measurement>> = BTreeMap::new();
        for (idx, raw_line) in text.lines().enumerate() {
            let line = idx + 1;
            if line == 1 {
                continue; // header: "icao24,utc_unix_s,lat_deg,lon_deg,geo_alt_m"
            }
            let trimmed = raw_line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let fields: Vec<&str> = trimmed.split(',').collect();
            if fields.len() != 5 {
                return Err(PluginError::AdsbColumnCount { line, expected: 5, actual: fields.len() });
            }
            let icao24 = parse_icao24(line, fields[0].trim())?;
            let utc_unix_s: i64 = parse_number(line, "utc_unix_s", fields[1].trim())?;
            let lat_deg: f64 = parse_number(line, "lat_deg", fields[2].trim())?;
            let lon_deg: f64 = parse_number(line, "lon_deg", fields[3].trim())?;
            let geo_alt_m: f64 = parse_number(line, "geo_alt_m", fields[4].trim())?;

            if !(-90.0..=90.0).contains(&lat_deg) {
                return Err(PluginError::AdsbLatitudeOutOfRange { line, lat_deg });
            }
            if !(-180.0..=180.0).contains(&lon_deg) {
                return Err(PluginError::AdsbLongitudeOutOfRange { line, lon_deg });
            }

            let utc_ns = utc_unix_s * 1_000_000_000;
            let epoch_tai_ns = av_cdm::time::Tai::from_utc_nanos(utc_ns).as_nanos();
            let ecef = geodetic_to_ecef_m(lat_deg.to_radians(), lon_deg.to_radians(), geo_alt_m);

            let measurement = pb::Measurement {
                measurement_id: config.measurement_id.clone(),
                z: ecef.to_vec(),
                r: config.noise_r.clone(),
                epoch_ns: epoch_tai_ns,
                sensor_id: config.sensor_id.clone(),
                shard_key: config.shard_key.clone(),
                meta: BTreeMap::new(),
                frame_id: config.frame_id.clone(),
                entity_hint: icao24,
            };
            by_epoch.entry(epoch_tai_ns).or_default().push(measurement);
        }
        Ok(Self { groups: by_epoch.into_iter().collect() })
    }
}

impl MeasurementSource for AdsbCsvSource {
    fn groups(&self) -> &[(i64, Vec<pb::Measurement>)] {
        &self.groups
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::{BatchingRule, Pacing};

    fn placeholder_codec_bytes() -> Vec<u8> {
        PluginConfig::encode_codec(&pb::PacketCodec {
            id: "unused-adsb-placeholder".to_string(),
            apid: 0,
            is_command: false,
            secondary_header_bytes: 0,
            user_data_bytes: 0,
            description: "AdsbCsvSource does not read a PacketCodec at all -- this exists only to satisfy PluginConfig::validate()'s decode check, per crate::plugin::adsb's own module doc.".to_string(),
            fields: vec![],
        })
    }

    fn label_bytes() -> Vec<u8> {
        PluginConfig::encode_label(&pb::Label { marking: "UNCLASSIFIED".to_string(), caveats: vec![] })
    }

    fn base_config() -> PluginConfig {
        PluginConfig {
            producer_id: "test-adsb-replay-plugin".to_string(),
            plugin_version: "0.1.0".to_string(),
            instance: "adsb".to_string(),
            port: "csv_replay".to_string(),
            direction: pb::PortDirection::Out as i32,
            codec_bytes: placeholder_codec_bytes(),
            component_fields: vec!["x".to_string(), "y".to_string(), "z".to_string()],
            frame_id: FRAME_ID.to_string(),
            sensor_id: "adsb-receiver".to_string(),
            measurement_id: "adsb_position".to_string(),
            shard_key: "adsb-demo".to_string(),
            noise_r: vec![625.0, 0.0, 0.0, 0.0, 625.0, 0.0, 0.0, 0.0, 625.0],
            label_bytes: label_bytes(),
            clearance: "UNCLASSIFIED".to_string(),
            leaf_fingerprint_sha256: String::new(),
            batching: BatchingRule::PerEpoch,
            pacing: Pacing::AsFastAsPossible,
        }
    }

    const CSV_HEADER: &str = "icao24,utc_unix_s,lat_deg,lon_deg,geo_alt_m\n";

    #[test]
    fn parses_two_aircraft_at_the_same_epoch_into_one_group() {
        let csv = format!("{CSV_HEADER}a00001,1700000000,34.0000,-118.0000,10000.0\na00002,1700000000,40.5000,-73.5000,9500.0\n");
        let source = AdsbCsvSource::from_csv(csv.as_bytes(), &base_config()).expect("valid csv parses");
        assert_eq!(source.groups().len(), 1, "both rows share one epoch");
        assert_eq!(source.groups()[0].1.len(), 2, "two aircraft at that epoch");
        let icaos: std::collections::BTreeSet<_> = source.groups()[0].1.iter().map(|m| m.entity_hint.clone()).collect();
        assert_eq!(icaos, ["a00001", "a00002"].into_iter().map(String::from).collect());
    }

    #[test]
    fn row_order_in_the_csv_does_not_matter() {
        let forward = format!("{CSV_HEADER}a00001,1700000000,34.0000,-118.0000,10000.0\na00001,1700000005,34.0100,-117.9850,10005.0\n");
        let backward = format!("{CSV_HEADER}a00001,1700000005,34.0100,-117.9850,10005.0\na00001,1700000000,34.0000,-118.0000,10000.0\n");
        let a = AdsbCsvSource::from_csv(forward.as_bytes(), &base_config()).unwrap();
        let b = AdsbCsvSource::from_csv(backward.as_bytes(), &base_config()).unwrap();
        assert_eq!(a.groups(), b.groups(), "grouping by epoch is independent of file order");
        assert!(a.groups()[0].0 < a.groups()[1].0, "groups themselves come out epoch-ascending");
    }

    #[test]
    fn ecef_conversion_matches_a_hand_computed_value_at_the_equator_and_prime_meridian() {
        // lat=0, lon=0, height=0 -> the point sits exactly on the WGS-84 x-axis at radius
        // WGS84_A_M (the semi-major axis), by construction of the ellipsoidal transform.
        let z = geodetic_to_ecef_m(0.0, 0.0, 0.0);
        assert!((z[0] - WGS84_A_M).abs() < 1e-6, "{z:?}");
        assert!(z[1].abs() < 1e-9, "{z:?}");
        assert!(z[2].abs() < 1e-9, "{z:?}");
    }

    #[test]
    fn refuses_a_row_with_the_wrong_column_count() {
        let csv = format!("{CSV_HEADER}a00001,1700000000,34.0000,-118.0000\n"); // missing geo_alt_m
        let err = AdsbCsvSource::from_csv(csv.as_bytes(), &base_config()).unwrap_err();
        assert!(matches!(err, PluginError::AdsbColumnCount { line: 2, expected: 5, actual: 4 }), "{err:?}");
    }

    #[test]
    fn refuses_a_malformed_icao24() {
        let csv = format!("{CSV_HEADER}not-hex,1700000000,34.0000,-118.0000,10000.0\n");
        let err = AdsbCsvSource::from_csv(csv.as_bytes(), &base_config()).unwrap_err();
        assert!(matches!(err, PluginError::AdsbInvalidIcao24 { line: 2, .. }), "{err:?}");
    }

    #[test]
    fn refuses_a_non_numeric_field() {
        let csv = format!("{CSV_HEADER}a00001,1700000000,not-a-number,-118.0000,10000.0\n");
        let err = AdsbCsvSource::from_csv(csv.as_bytes(), &base_config()).unwrap_err();
        assert!(matches!(err, PluginError::AdsbInvalidField { line: 2, field: "lat_deg", .. }), "{err:?}");
    }

    #[test]
    fn refuses_an_out_of_range_latitude() {
        let csv = format!("{CSV_HEADER}a00001,1700000000,95.5000,-118.0000,10000.0\n");
        let err = AdsbCsvSource::from_csv(csv.as_bytes(), &base_config()).unwrap_err();
        assert!(matches!(err, PluginError::AdsbLatitudeOutOfRange { line: 2, lat_deg } if lat_deg == 95.5), "{err:?}");
    }

    #[test]
    fn refuses_an_out_of_range_longitude() {
        let csv = format!("{CSV_HEADER}a00001,1700000000,34.0000,-190.0000,10000.0\n");
        let err = AdsbCsvSource::from_csv(csv.as_bytes(), &base_config()).unwrap_err();
        assert!(matches!(err, PluginError::AdsbLongitudeOutOfRange { line: 2, lon_deg } if lon_deg == -190.0), "{err:?}");
    }

    #[test]
    fn refuses_invalid_utf8() {
        let mut csv = CSV_HEADER.as_bytes().to_vec();
        csv.extend_from_slice(b"a00001,1700000000,34.0000,-118.0000,\xFF\xFE\n");
        let err = AdsbCsvSource::from_csv(&csv, &base_config()).unwrap_err();
        assert!(matches!(err, PluginError::AdsbInvalidUtf8 { .. }), "{err:?}");
    }

    #[test]
    fn refuses_a_component_fields_shape_other_than_xyz() {
        let mut cfg = base_config();
        cfg.component_fields = vec!["x".to_string(), "y".to_string()];
        cfg.noise_r = vec![625.0, 0.0, 0.0, 625.0];
        let csv = format!("{CSV_HEADER}a00001,1700000000,34.0000,-118.0000,10000.0\n");
        let err = AdsbCsvSource::from_csv(csv.as_bytes(), &cfg).unwrap_err();
        assert!(matches!(err, PluginError::AdsbComponentFieldsMismatch { .. }), "{err:?}");
    }

    #[test]
    fn converts_utc_to_tai_through_the_shared_leap_second_table() {
        let csv = format!("{CSV_HEADER}a00001,1700000000,34.0000,-118.0000,10000.0\n");
        let source = AdsbCsvSource::from_csv(csv.as_bytes(), &base_config()).unwrap();
        let expected_tai_ns = av_cdm::time::Tai::from_utc_nanos(1_700_000_000_000_000_000).as_nanos();
        assert_eq!(source.groups()[0].0, expected_tai_ns);
        // Sanity: TAI is ahead of UTC by the leap-second offset in force (37s in 2023),
        // never equal to the raw UTC nanosecond count -- proves this is a real table
        // lookup, not a pass-through.
        assert_ne!(expected_tai_ns, 1_700_000_000_000_000_000, "TAI must differ from UTC by the leap-second offset");
    }
}
