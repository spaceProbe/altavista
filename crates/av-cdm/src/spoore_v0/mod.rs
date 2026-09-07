//! `spoore.v0` <-> `altavista.v1` conversions (ADR-001 "Compatibility", `proto/README.md`).
//!
//! `spoore-cdm`'s hand-written native types (`GaussianState`, `Belief`/`MixtureComponent`,
//! `Measurement`, `Innovation` -- never spoore's own generated `spoore_cdm::proto` types)
//! convert to and from this crate's [`crate::pb`] `altavista.v1` types. The direction
//! matters, exactly as it does inside spoore-cdm's own `proto` module:
//!
//! - **native -> `pb` is infallible** (a plain function returning `pb::T`, not `Result`):
//!   `spoore_cdm`'s constructors already enforce every invariant (finite, symmetric SPD
//!   covariance, normalized weights, ...) that a `pb::T` cannot express, so re-encoding a
//!   value that already holds them cannot fail.
//! - **`pb` -> native is fallible** (`Result<T, Error>`): the bytes came from outside, so
//!   validation happens here, once, at this boundary.
//!
//! Two semantic changes happen at this boundary, both from ADR-001:
//!
//! 1. **Epoch scale.** `spoore_cdm::Epoch` is Unix-scale nanoseconds; `spoore-cdm`'s own
//!    docs call it "nanoseconds since the Unix epoch" without naming a scale, and this
//!    adapter treats it as UTC (POSIX civil-time) per ADR-001's "Consequences": *"today
//!    spoore neither states nor depends on a scale ... until [an upstream PR lands],
//!    `av-cdm` shifts on ingest and egress"*. `altavista.v1.epoch_ns` is TAI. Native ->
//!    `pb` adds the TAI-UTC offset in force at that instant (via [`crate::time::Tai`]);
//!    `pb` -> native subtracts it.
//! 2. **Frame.** spoore's `Frame` enum maps to a fixed v1 `frame_id` string -- see
//!    [`frame`] for the table and its limitations.
//!
//! `mod tests` below (round trips, byte-identical wire compatibility with the *un-shifted*
//! shared fields, the epoch shift across a leap-second boundary) is the required-tests list
//! from the task; `crates/av-cdm/tests/spoore_v0.rs` repeats the cross-cutting ones as
//! integration tests so they also run as `cargo test --workspace` black-box checks.

pub mod error;
pub mod frame;

pub use error::{Error, Result};

use nalgebra::DMatrix;

use crate::pb;
use crate::time::Tai;

/// Flatten a matrix to row-major order, matching the wire contract (nalgebra stores
/// column-major internally). Mirrors `spoore_cdm::proto::to_row_major` exactly -- both
/// sides of this adapter share the same wire convention, and the covariances involved are
/// symmetric so the distinction is usually invisible, but relying on that would silently
/// corrupt the first non-symmetric matrix (e.g. a cross-covariance) anyone adds.
fn to_row_major(m: &DMatrix<f64>) -> Vec<f64> {
    let mut out = Vec::with_capacity(m.len());
    for i in 0..m.nrows() {
        for j in 0..m.ncols() {
            out.push(m[(i, j)]);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// GaussianState
// ---------------------------------------------------------------------------

/// `spoore_cdm::GaussianState` -> `pb::GaussianState`. Infallible; shifts the epoch from
/// spoore's Unix/UTC scale onto TAI.
pub fn gaussian_state_to_pb(native: &spoore_cdm::GaussianState) -> pb::GaussianState {
    pb::GaussianState {
        mean: native.mean().iter().copied().collect(),
        cov: to_row_major(native.cov()),
        state_space_id: native.state_space_id().to_string(),
        epoch_ns: Tai::from_utc_nanos(native.epoch().as_nanos()).as_nanos(),
    }
}

/// `pb::GaussianState` -> `spoore_cdm::GaussianState`. Fallible: proto3 cannot express the
/// Gaussian invariants, validated here by `GaussianState::from_slices`. Shifts the epoch
/// from TAI back onto spoore's Unix/UTC scale.
pub fn gaussian_state_from_pb(p: &pb::GaussianState) -> Result<spoore_cdm::GaussianState> {
    let utc_ns = Tai::from_nanos(p.epoch_ns).to_utc_nanos();
    let native = spoore_cdm::GaussianState::from_slices(
        &p.mean,
        &p.cov,
        p.state_space_id.clone(),
        spoore_cdm::Epoch::from_nanos(utc_ns),
    )?;
    Ok(native)
}

// ---------------------------------------------------------------------------
// Belief / MixtureComponent
// ---------------------------------------------------------------------------

pub fn mixture_component_to_pb(native: &spoore_cdm::MixtureComponent) -> pb::MixtureComponent {
    pb::MixtureComponent {
        hypothesis_label: native.hypothesis_label.clone(),
        weight: native.weight,
        state: Some(gaussian_state_to_pb(&native.state)),
    }
}

pub fn mixture_component_from_pb(p: &pb::MixtureComponent) -> Result<spoore_cdm::MixtureComponent> {
    let state = p.state.as_ref().ok_or(Error::MissingField {
        message: "MixtureComponent",
        field: "state",
    })?;
    Ok(spoore_cdm::MixtureComponent::new(
        p.hypothesis_label.clone(),
        p.weight,
        gaussian_state_from_pb(state)?,
    ))
}

pub fn belief_to_pb(native: &spoore_cdm::Belief) -> pb::Belief {
    pb::Belief {
        components: native.components().iter().map(mixture_component_to_pb).collect(),
    }
}

/// `pb::Belief` -> `spoore_cdm::Belief`. Uses `Belief::new` (not `from_unnormalized`): a
/// decoded belief asserts its weights already sum to 1, exactly as spoore-cdm's own proto
/// module does -- rescaling here would hide a sender that dropped a component in transit.
pub fn belief_from_pb(p: &pb::Belief) -> Result<spoore_cdm::Belief> {
    let components = p
        .components
        .iter()
        .map(mixture_component_from_pb)
        .collect::<Result<Vec<_>>>()?;
    Ok(spoore_cdm::Belief::new(components)?)
}

// ---------------------------------------------------------------------------
// Measurement
// ---------------------------------------------------------------------------

/// `spoore_cdm::Measurement` -> `pb::Measurement`. Infallible; shifts the epoch and maps
/// `frame()` to a v1 `frame_id` (see [`frame`]). `entity_hint` is new in v1 and left empty:
/// spoore carries no equivalent.
pub fn measurement_to_pb(native: &spoore_cdm::Measurement) -> pb::Measurement {
    pb::Measurement {
        measurement_id: native.id().to_string(),
        z: native.z().iter().copied().collect(),
        r: to_row_major(native.r()),
        epoch_ns: Tai::from_utc_nanos(native.epoch().as_nanos()).as_nanos(),
        sensor_id: native.sensor_id().to_string(),
        shard_key: native.shard_key().to_string(),
        meta: native
            .meta()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        frame_id: frame::frame_to_frame_id(native.frame()).to_string(),
        entity_hint: String::new(),
    }
}

/// `pb::Measurement` -> `spoore_cdm::Measurement`. Fallible: validated by
/// `Measurement::from_slices`, and `frame_id` must resolve through [`frame::frame_id_to_frame`]
/// -- an empty or unrecognized `frame_id` is a typed error, never a silent default frame.
pub fn measurement_from_pb(p: &pb::Measurement) -> Result<spoore_cdm::Measurement> {
    let utc_ns = Tai::from_nanos(p.epoch_ns).to_utc_nanos();
    let frame = frame::frame_id_to_frame(&p.frame_id)?;
    Ok(spoore_cdm::Measurement::from_slices(
        p.measurement_id.clone(),
        &p.z,
        &p.r,
        spoore_cdm::Epoch::from_nanos(utc_ns),
        p.sensor_id.clone(),
    )?
    .with_shard_key(p.shard_key.clone())
    .with_frame(frame)
    .with_meta(p.meta.clone()))
}

// ---------------------------------------------------------------------------
// Innovation
// ---------------------------------------------------------------------------

/// `spoore_cdm::Innovation` -> `pb::Innovation`. Infallible.
pub fn innovation_to_pb(native: &spoore_cdm::Innovation) -> pb::Innovation {
    pb::Innovation {
        nu: native.nu().iter().copied().collect(),
        s: to_row_major(native.s()),
        log_likelihood: native.log_likelihood(),
        nis: native.nis(),
    }
}

/// `pb::Innovation` -> `spoore_cdm::Innovation`. `Innovation::new` itself performs no
/// validation (its caller owns the arithmetic; see its doc comment) -- the one thing this
/// adapter still owes the input is checking `s`'s length is `nu.len()^2` before reshaping
/// it into a matrix, since that reshape would otherwise panic on malformed data.
pub fn innovation_from_pb(p: &pb::Innovation) -> Result<spoore_cdm::Innovation> {
    let m = p.nu.len();
    if p.s.len() != m * m {
        return Err(Error::InnovationDimensionMismatch {
            nu_dim: m,
            s_len: p.s.len(),
        });
    }
    Ok(spoore_cdm::Innovation::new(
        nalgebra::DVector::from_row_slice(&p.nu),
        DMatrix::from_row_slice(m, m, &p.s),
        p.log_likelihood,
        p.nis,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_state(epoch_ns: i64) -> spoore_cdm::GaussianState {
        spoore_cdm::GaussianState::from_slices(
            &[7_000_000.0, 0.0, 0.0, 0.0, 7_500.0, 0.0],
            &[
                100.0, 0.0, 0.0, 0.0, 0.0, 0.0, //
                0.0, 100.0, 0.0, 0.0, 0.0, 0.0, //
                0.0, 0.0, 100.0, 0.0, 0.0, 0.0, //
                0.0, 0.0, 0.0, 1.0, 0.0, 0.0, //
                0.0, 0.0, 0.0, 0.0, 1.0, 0.0, //
                0.0, 0.0, 0.0, 0.0, 0.0, 1.0,
            ],
            "cv_3d",
            spoore_cdm::Epoch::from_nanos(epoch_ns),
        )
        .unwrap()
    }

    // -- GaussianState -------------------------------------------------------------------

    #[test]
    fn gaussian_state_round_trips_mean_cov_space_and_epoch() {
        let native = sample_state(1_700_000_000_000_000_000);
        let p = gaussian_state_to_pb(&native);
        let back = gaussian_state_from_pb(&p).unwrap();
        assert_eq!(back, native);
    }

    #[test]
    fn gaussian_state_epoch_shift_is_the_tai_utc_offset_in_force() {
        // 2020-01-01-ish UTC ns, table offset 37 s throughout.
        let utc_ns = 1_577_836_800_000_000_000_i64;
        let native = sample_state(utc_ns);
        let p = gaussian_state_to_pb(&native);
        assert_eq!(p.epoch_ns, utc_ns + 37_000_000_000);
    }

    #[test]
    fn gaussian_state_epoch_shift_straddles_a_leap_second_boundary() {
        // 1999-01-01T00:00:00Z UTC: offset steps from 31 to 32 exactly here.
        let boundary_utc_ns = 915_148_800_000_000_000_i64;
        let before = sample_state(boundary_utc_ns - 1_000_000_000);
        let after = sample_state(boundary_utc_ns);

        let p_before = gaussian_state_to_pb(&before);
        let p_after = gaussian_state_to_pb(&after);

        assert_eq!(p_before.epoch_ns, boundary_utc_ns - 1_000_000_000 + 31_000_000_000);
        assert_eq!(p_after.epoch_ns, boundary_utc_ns + 32_000_000_000);
        // TAI is continuous: the gap between the two epochs is exactly 1 s, even though the
        // UTC-side gap (also 1 s) crossed a leap second where the offset itself changed by 1 s.
        assert_eq!(p_after.epoch_ns - p_before.epoch_ns, 2_000_000_000);

        assert_eq!(gaussian_state_from_pb(&p_before).unwrap(), before);
        assert_eq!(gaussian_state_from_pb(&p_after).unwrap(), after);
    }

    #[test]
    fn gaussian_state_missing_state_space_dimension_is_a_typed_error() {
        let p = pb::GaussianState {
            mean: vec![1.0, 2.0],
            cov: vec![1.0], // not 4 elements
            state_space_id: "s".into(),
            epoch_ns: 0,
        };
        assert!(matches!(
            gaussian_state_from_pb(&p),
            Err(Error::Spoore(spoore_cdm::CdmError::DimensionMismatch { .. }))
        ));
    }

    // -- Belief / MixtureComponent --------------------------------------------------------

    #[test]
    fn belief_round_trips() {
        let native = spoore_cdm::Belief::new(vec![
            spoore_cdm::MixtureComponent::new("a", 0.25, sample_state(1_000)),
            spoore_cdm::MixtureComponent::new("b", 0.75, sample_state(2_000)),
        ])
        .unwrap();
        let p = belief_to_pb(&native);
        assert_eq!(belief_from_pb(&p).unwrap(), native);
    }

    #[test]
    fn belief_missing_component_state_is_a_typed_error() {
        let p = pb::Belief {
            components: vec![pb::MixtureComponent {
                hypothesis_label: "a".into(),
                weight: 1.0,
                state: None,
            }],
        };
        assert!(matches!(
            belief_from_pb(&p),
            Err(Error::MissingField {
                message: "MixtureComponent",
                field: "state"
            })
        ));
    }

    // -- Measurement -----------------------------------------------------------------------

    fn sample_measurement(epoch_ns: i64) -> spoore_cdm::Measurement {
        spoore_cdm::Measurement::from_slices(
            "m1",
            &[10.0, 20.0],
            &[1.0, 0.0, 0.0, 1.0],
            spoore_cdm::Epoch::from_nanos(epoch_ns),
            "radar_0",
        )
        .unwrap()
        .with_shard_key("cell_7")
        .with_frame(spoore_cdm::Frame::Enu)
        .with_meta(std::collections::BTreeMap::from([(
            "source".to_string(),
            "ingest_a".to_string(),
        )]))
    }

    #[test]
    fn measurement_round_trips_including_frame_and_meta() {
        let native = sample_measurement(1_700_000_000_000_000_000);
        let p = measurement_to_pb(&native);
        assert_eq!(p.frame_id, frame::ENU);
        assert_eq!(measurement_from_pb(&p).unwrap(), native);
    }

    #[test]
    fn measurement_unknown_frame_id_is_a_typed_error() {
        let mut p = measurement_to_pb(&sample_measurement(0));
        p.frame_id = "bogus".to_string();
        assert!(matches!(
            measurement_from_pb(&p),
            Err(Error::UnknownFrameId { .. })
        ));
    }

    // -- Innovation --------------------------------------------------------------------------

    #[test]
    fn innovation_round_trips() {
        let native = spoore_cdm::Innovation::new(
            nalgebra::DVector::from_row_slice(&[0.5, -0.5]),
            nalgebra::DMatrix::identity(2, 2),
            -2.5,
            0.5,
        );
        let p = innovation_to_pb(&native);
        let back = innovation_from_pb(&p).unwrap();
        assert_eq!(back, native);
    }

    #[test]
    fn innovation_dimension_mismatch_is_a_typed_error_not_a_panic() {
        let p = pb::Innovation {
            nu: vec![1.0, 2.0],
            s: vec![1.0], // not 4 elements
            log_likelihood: 0.0,
            nis: 0.0,
        };
        assert!(matches!(
            innovation_from_pb(&p),
            Err(Error::InnovationDimensionMismatch { nu_dim: 2, s_len: 1 })
        ));
    }

    // -- Byte-identical wire compatibility with spoore.v0 ------------------------------------
    //
    // A v1 GaussianState/Measurement/Belief/Innovation carrying only the fields spoore.v0
    // also has (i.e. built with the v1-only fields left at their proto3 defaults) encodes to
    // bytes identical to the corresponding spoore.v0 message, since both share the same
    // field numbers and wire types (proto/README.md "Compatibility"). This is a structural
    // check of the message *shapes*, independent of the epoch-shift semantics tested above.

    #[test]
    fn gaussian_state_wire_bytes_match_spoore_v0() {
        use prost::Message;

        let v1 = pb::GaussianState {
            mean: vec![1.0, 2.0, 3.0],
            cov: vec![4.0, 0.0, 0.0, 0.0, 4.0, 0.0, 0.0, 0.0, 4.0],
            state_space_id: "cv_3d".to_string(),
            epoch_ns: 1_700_000_000_123_456_789,
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
    fn measurement_wire_bytes_match_spoore_v0_when_frame_id_is_unset() {
        use prost::Message;

        let v1 = pb::Measurement {
            measurement_id: "m1".to_string(),
            z: vec![10.0, 20.0],
            r: vec![1.0, 0.0, 0.0, 1.0],
            epoch_ns: 42,
            sensor_id: "radar_0".to_string(),
            shard_key: "cell_7".to_string(),
            meta: std::collections::BTreeMap::from([("k".to_string(), "v".to_string())]),
            // Left at proto3 defaults: `frame` is reserved in v1 (cannot be set at all), and
            // `frame_id` (new in v1, field 9) is left empty so it costs no bytes on the wire,
            // matching spoore's default-valued (Unspecified) `frame` (field 7) costing none.
            frame_id: String::new(),
            entity_hint: String::new(),
        };
        let spoore = spoore_cdm::proto::Measurement {
            measurement_id: v1.measurement_id.clone(),
            z: v1.z.clone(),
            r: v1.r.clone(),
            epoch_ns: v1.epoch_ns,
            sensor_id: v1.sensor_id.clone(),
            shard_key: v1.shard_key.clone(),
            frame: 0, // FRAME_UNSPECIFIED: proto3 default, not serialized.
            // spoore-cdm's own build.rs does not use prost-build's `.btree_map(..)` option
            // (av-cdm's does, ADR-004 determinism), so `spoore_cdm::proto::Measurement.meta`
            // is a `HashMap`, not the `BTreeMap` `pb::Measurement.meta` is. The wire format
            // is identical either way (a map field is a sequence of key/value sub-messages;
            // this test's single entry makes iteration order moot), but the two generated
            // struct types genuinely differ, so an explicit re-collect is required here.
            meta: v1.meta.clone().into_iter().collect(),
        };
        assert_eq!(v1.encode_to_vec(), spoore.encode_to_vec());
    }

    #[test]
    fn belief_wire_bytes_match_spoore_v0() {
        use prost::Message;

        let v1 = pb::Belief {
            components: vec![pb::MixtureComponent {
                hypothesis_label: "root".to_string(),
                weight: 1.0,
                state: Some(pb::GaussianState {
                    mean: vec![1.0],
                    cov: vec![1.0],
                    state_space_id: "s".to_string(),
                    epoch_ns: 5,
                }),
            }],
        };
        let spoore = spoore_cdm::proto::Belief {
            components: vec![spoore_cdm::proto::MixtureComponent {
                hypothesis_label: "root".to_string(),
                weight: 1.0,
                state: Some(spoore_cdm::proto::GaussianState {
                    mean: vec![1.0],
                    cov: vec![1.0],
                    state_space_id: "s".to_string(),
                    epoch_ns: 5,
                }),
            }],
        };
        assert_eq!(v1.encode_to_vec(), spoore.encode_to_vec());
    }

    #[test]
    fn innovation_wire_bytes_match_spoore_v0() {
        use prost::Message;

        let v1 = pb::Innovation {
            nu: vec![0.5, -0.5],
            s: vec![1.0, 0.0, 0.0, 1.0],
            log_likelihood: -2.5,
            nis: 0.5,
        };
        let spoore = spoore_cdm::proto::Innovation {
            nu: v1.nu.clone(),
            s: v1.s.clone(),
            log_likelihood: v1.log_likelihood,
            nis: v1.nis,
        };
        assert_eq!(v1.encode_to_vec(), spoore.encode_to_vec());
    }
}
