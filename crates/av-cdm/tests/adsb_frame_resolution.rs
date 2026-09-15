//! Proves the edge track's second plugin (question 200(a), `crates/av-edge/src/plugin/
//! adsb.rs`) does not hard-code a `spoore.v0` literal frame id: its declared `frame_id`,
//! `"adsb.earth_ecef"` (transcribed here verbatim from `av_edge::plugin::adsb::FRAME_ID` --
//! this crate cannot depend on `av-edge`, the dependency runs the other way, so the string
//! is duplicated with a comment tying the two together, exactly the way `crates/av-kernel/
//! tests/edge_plugin_codec_crosscheck.rs::flight_codec` already transcribes `av-edge`'s own
//! fixture codec field for field), resolves through the real, registry-driven path --
//! `av_cdm::spoore_v0::frame::frame_from_definition`, via `measurement_from_pb_with_frames`
//! -- to `spoore_cdm::Frame::Ecef`, given an Earth-origin `AXES_KIND_BODY_FIXED`
//! `FrameDefinition`. It deliberately does **not** go through `av_cdm::spoore_v0::frame::
//! frame_id_to_frame`'s fixed five literal ids (`"earth.ecef"` and friends) at all: an
//! unregistered `"adsb.earth_ecef"` id must fail to resolve, proving direction 2 (registry)
//! is really what is being exercised, not an accidental match against direction 1's fixed
//! literal path.

use av_cdm::pb;
use av_cdm::spoore_v0::{self, frame};

/// `av_edge::plugin::adsb::FRAME_ID` -- see this file's own module doc for why the string
/// is duplicated here rather than imported.
const ADSB_FRAME_ID: &str = "adsb.earth_ecef";

fn adsb_frame_definition() -> pb::FrameDefinition {
    pb::FrameDefinition {
        id: ADSB_FRAME_ID.to_string(),
        origin: Some(pb::frame_definition::Origin::Body("Earth".to_string())),
        axes: pb::AxesKind::BodyFixed as i32,
        description: "ADS-B replay source's own Earth-fixed ECEF frame (question 200(a))".to_string(),
        ..Default::default()
    }
}

fn adsb_measurement() -> pb::Measurement {
    pb::Measurement {
        measurement_id: "adsb_position".to_string(),
        z: vec![1_000_000.0, 2_000_000.0, 3_000_000.0],
        r: vec![625.0, 0.0, 0.0, 0.0, 625.0, 0.0, 0.0, 0.0, 625.0],
        epoch_ns: 1_700_000_000_000_000_000,
        sensor_id: "adsb-receiver".to_string(),
        shard_key: "adsb-demo".to_string(),
        meta: Default::default(),
        frame_id: ADSB_FRAME_ID.to_string(),
        entity_hint: "a00001".to_string(),
    }
}

/// `frame_from_definition` directly, on the exact `FrameDefinition` shape the ADS-B
/// plugin declares -- resolves to `Frame::Ecef` because the origin is `body("Earth")` and
/// the axes are `AXES_KIND_BODY_FIXED` (`av_cdm::spoore_v0::frame`'s own registry-
/// resolution table).
#[test]
fn adsb_frame_definition_resolves_to_ecef() {
    let def = adsb_frame_definition();
    assert_eq!(frame::frame_from_definition(&def).unwrap(), spoore_cdm::Frame::Ecef);
}

/// The full path a real ingest/consumer boundary would use:
/// `measurement_from_pb_with_frames`, given a registry containing the ADS-B plugin's own
/// declared `FrameDefinition`, resolves a `Measurement` carrying that `frame_id` onto a
/// native `spoore_cdm::Measurement` whose `.frame()` is `Frame::Ecef`.
#[test]
fn measurement_from_pb_with_frames_resolves_the_adsb_frame_id_through_the_registry() {
    let registry = vec![adsb_frame_definition()];
    let m = adsb_measurement();
    let native = spoore_v0::measurement_from_pb_with_frames(&m, &registry).expect("resolves through the registry");
    assert_eq!(native.frame(), spoore_cdm::Frame::Ecef);
}

/// The registry-less path (`measurement_from_pb`, equivalently `measurement_from_pb_with_
/// frames` with an empty registry) must NOT resolve `"adsb.earth_ecef"` -- it is not one of
/// `frame_id_to_frame`'s fixed five literals. This is what proves the test above is really
/// exercising the registry-driven path and not merely re-deriving a result the fixed-five
/// fallback would have produced anyway.
#[test]
fn without_a_registered_frame_definition_the_adsb_frame_id_does_not_resolve() {
    let m = adsb_measurement();
    let err = spoore_v0::measurement_from_pb(&m).unwrap_err();
    match err {
        spoore_v0::Error::UnknownFrameId { frame_id, .. } => assert_eq!(frame_id, ADSB_FRAME_ID),
        other => panic!("expected UnknownFrameId, got {other:?}"),
    }
}

/// A body-fixed frame declared about a body other than Earth must still be refused, even
/// under the ADS-B plugin's own id -- `Frame::Ecef` means Earth-centred Earth-fixed
/// specifically (`frame.rs`'s own module doc), and this guards against a future edit that
/// accidentally drops the Earth-origin check.
#[test]
fn a_body_fixed_frame_about_a_non_earth_body_under_the_same_id_is_refused() {
    let mut def = adsb_frame_definition();
    def.origin = Some(pb::frame_definition::Origin::Body("Mars".to_string()));
    let err = frame::frame_from_definition(&def).unwrap_err();
    assert!(matches!(err, spoore_v0::Error::UnmappableFrame { .. }), "{err:?}");
}
