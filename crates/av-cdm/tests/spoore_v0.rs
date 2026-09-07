//! Integration tests for `av_cdm::spoore_v0`, exercised through the crate's public API only
//! (unlike `src/spoore_v0/mod.rs`'s unit tests, which also cover error-path details from
//! inside the module). This file is the "Required tests" checklist from the M1.1 task:
//! round-trip identity, the epoch shift across a leap-second boundary, byte-identical wire
//! compatibility with spoore.v0 for all four message types, and frame-mapping totality.

use std::collections::BTreeMap;

use av_cdm::pb;
use av_cdm::spoore_v0;
use prost::Message;

fn state(mean: &[f64], epoch_ns: i64) -> spoore_cdm::GaussianState {
    let n = mean.len();
    let mut cov = vec![0.0; n * n];
    for i in 0..n {
        cov[i * n + i] = 1.0;
    }
    spoore_cdm::GaussianState::from_slices(mean, &cov, "cv", spoore_cdm::Epoch::from_nanos(epoch_ns)).unwrap()
}

// ---------------------------------------------------------------------------
// Round trip: spoore -> v1 -> spoore is the identity, for all four message types.
// ---------------------------------------------------------------------------

#[test]
fn gaussian_state_round_trip_is_identity() {
    let native = state(&[1.0, 2.0, 3.0], 1_700_000_000_000_000_000);
    let back = spoore_v0::gaussian_state_from_pb(&spoore_v0::gaussian_state_to_pb(&native)).unwrap();
    assert_eq!(back, native);
}

#[test]
fn belief_round_trip_is_identity() {
    let native = spoore_cdm::Belief::new(vec![
        spoore_cdm::MixtureComponent::new("a", 0.4, state(&[1.0], 10)),
        spoore_cdm::MixtureComponent::new("b", 0.6, state(&[2.0], 20)),
    ])
    .unwrap();
    let back = spoore_v0::belief_from_pb(&spoore_v0::belief_to_pb(&native)).unwrap();
    assert_eq!(back, native);
}

#[test]
fn measurement_round_trip_is_identity() {
    let native = spoore_cdm::Measurement::from_slices(
        "m1",
        &[1.0, 2.0],
        &[1.0, 0.0, 0.0, 1.0],
        spoore_cdm::Epoch::from_nanos(1_700_000_000_000_000_000),
        "sensor_a",
    )
    .unwrap()
    .with_shard_key("cell_3")
    .with_frame(spoore_cdm::Frame::Ecef)
    .with_meta(BTreeMap::from([("k".to_string(), "v".to_string())]));
    let back = spoore_v0::measurement_from_pb(&spoore_v0::measurement_to_pb(&native)).unwrap();
    assert_eq!(back, native);
}

#[test]
fn innovation_round_trip_is_identity() {
    let native = spoore_cdm::Innovation::new(
        nalgebra::DVector::from_row_slice(&[1.0, -1.0]),
        nalgebra::DMatrix::identity(2, 2),
        -1.2,
        0.7,
    );
    let back = spoore_v0::innovation_from_pb(&spoore_v0::innovation_to_pb(&native)).unwrap();
    assert_eq!(back, native);
}

// ---------------------------------------------------------------------------
// Epoch shift: right direction, right amount, including a leap-second boundary.
// ---------------------------------------------------------------------------

#[test]
fn epoch_shift_adds_the_tai_utc_offset_going_to_v1() {
    // 1970-01-01T00:00:10Z UTC-ish magnitude is irrelevant; pick a date safely inside the
    // table's span with a known offset: 2010-06-01 (between the 2009 and 2012 boundaries,
    // offset 34 s).
    let utc_ns = 1_275_350_400_000_000_000_i64; // 2010-06-01T00:00:00Z
    let native = state(&[0.0], utc_ns);
    let p = spoore_v0::gaussian_state_to_pb(&native);
    assert_eq!(p.epoch_ns, utc_ns + 34_000_000_000, "TAI = UTC + 34s in force in mid-2010");
    assert!(p.epoch_ns > utc_ns, "TAI must run ahead of UTC");
}

#[test]
fn epoch_shift_straddles_the_2006_leap_second_boundary() {
    // 2006-01-01T00:00:00Z: offset steps from 32 to 33.
    let boundary = 1_136_073_600_000_000_000_i64;
    let one_utc_second_before = state(&[0.0], boundary - 1_000_000_000);
    let at_boundary = state(&[0.0], boundary);

    let p_before = spoore_v0::gaussian_state_to_pb(&one_utc_second_before);
    let p_at = spoore_v0::gaussian_state_to_pb(&at_boundary);

    assert_eq!(p_before.epoch_ns, boundary - 1_000_000_000 + 32_000_000_000);
    assert_eq!(p_at.epoch_ns, boundary + 33_000_000_000);
    // The real TAI gap is 2 s (1 nominal UTC second plus the inserted leap second).
    assert_eq!(p_at.epoch_ns - p_before.epoch_ns, 2_000_000_000);

    // And both still round-trip back to their original spoore-scale states.
    assert_eq!(
        spoore_v0::gaussian_state_from_pb(&p_before).unwrap(),
        one_utc_second_before
    );
    assert_eq!(spoore_v0::gaussian_state_from_pb(&p_at).unwrap(), at_boundary);
}

// ---------------------------------------------------------------------------
// Byte-identical serialization: a v1 message carrying only fields spoore.v0 also has
// encodes identically to the corresponding spoore.v0 message.
// ---------------------------------------------------------------------------

#[test]
fn gaussian_state_bytes_match_spoore_v0() {
    let v1 = pb::GaussianState {
        mean: vec![7.0e6, 0.0, 0.0],
        cov: vec![1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
        state_space_id: "cv_3d".to_string(),
        epoch_ns: 1_700_000_000_000_000_000,
    };
    let spoore = spoore_cdm::proto::GaussianState {
        mean: v1.mean.clone(),
        cov: v1.cov.clone(),
        state_space_id: v1.state_space_id.clone(),
        epoch_ns: v1.epoch_ns,
    };
    assert_eq!(v1.encode_to_vec(), spoore.encode_to_vec());
}

#[test]
fn measurement_bytes_match_spoore_v0_when_v1_only_fields_are_default() {
    let v1 = pb::Measurement {
        measurement_id: "m1".to_string(),
        z: vec![1.0, 2.0],
        r: vec![1.0, 0.0, 0.0, 1.0],
        epoch_ns: 99,
        sensor_id: "s1".to_string(),
        shard_key: "shard".to_string(),
        meta: BTreeMap::new(),
        frame_id: String::new(),   // v1-only, field 9; left default (empty, unserialized).
        entity_hint: String::new(), // v1-only, field 10; left default.
    };
    let spoore = spoore_cdm::proto::Measurement {
        measurement_id: v1.measurement_id.clone(),
        z: v1.z.clone(),
        r: v1.r.clone(),
        epoch_ns: v1.epoch_ns,
        sensor_id: v1.sensor_id.clone(),
        shard_key: v1.shard_key.clone(),
        frame: 0,
        meta: Default::default(),
    };
    assert_eq!(v1.encode_to_vec(), spoore.encode_to_vec());
}

#[test]
fn belief_bytes_match_spoore_v0() {
    let component = |label: &str, weight: f64| pb::MixtureComponent {
        hypothesis_label: label.to_string(),
        weight,
        state: Some(pb::GaussianState {
            mean: vec![1.0],
            cov: vec![1.0],
            state_space_id: "s".to_string(),
            epoch_ns: 0,
        }),
    };
    let v1 = pb::Belief {
        components: vec![component("a", 1.0)],
    };
    let spoore_component = spoore_cdm::proto::MixtureComponent {
        hypothesis_label: "a".to_string(),
        weight: 1.0,
        state: Some(spoore_cdm::proto::GaussianState {
            mean: vec![1.0],
            cov: vec![1.0],
            state_space_id: "s".to_string(),
            epoch_ns: 0,
        }),
    };
    let spoore = spoore_cdm::proto::Belief {
        components: vec![spoore_component],
    };
    assert_eq!(v1.encode_to_vec(), spoore.encode_to_vec());
}

#[test]
fn innovation_bytes_match_spoore_v0() {
    let v1 = pb::Innovation {
        nu: vec![1.0, -1.0],
        s: vec![2.0, 0.0, 0.0, 2.0],
        log_likelihood: -3.1,
        nis: 1.4,
    };
    let spoore = spoore_cdm::proto::Innovation {
        nu: v1.nu.clone(),
        s: v1.s.clone(),
        log_likelihood: v1.log_likelihood,
        nis: v1.nis,
    };
    assert_eq!(v1.encode_to_vec(), spoore.encode_to_vec());
}

// ---------------------------------------------------------------------------
// Frame mapping: total and round-tripping.
// ---------------------------------------------------------------------------

#[test]
fn every_spoore_frame_has_a_frame_id_and_round_trips() {
    for frame in [
        spoore_cdm::Frame::Ecef,
        spoore_cdm::Frame::Enu,
        spoore_cdm::Frame::Ned,
        spoore_cdm::Frame::Body,
        spoore_cdm::Frame::LocalCartesian,
    ] {
        let id = spoore_v0::frame::frame_to_frame_id(frame);
        assert!(!id.is_empty());
        assert_eq!(spoore_v0::frame::frame_id_to_frame(id).unwrap(), frame);
    }
}

#[test]
fn an_unregistered_frame_id_is_a_typed_error() {
    assert!(spoore_v0::frame::frame_id_to_frame("not.a.real.frame").is_err());
}
