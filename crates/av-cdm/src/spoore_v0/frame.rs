//! `spoore_cdm::Frame` <-> `altavista.v1` `frame_id` (ADR-001 "Frames",
//! `proto/README.md` "Compatibility").
//!
//! `spoore.v0`'s `Frame` is a plain enum: five Earth-local axes kinds, with no notion of
//! *which* platform or geodetic point they are centered on. `altavista.v1` frames are
//! registry entries (`FrameDefinition`): an `id`, an origin (a body, a platform, or an
//! entity), an `AxesKind`, and for local frames a `Geodetic` origin. spoore's messages never
//! carried that extra information, so there is no way to recover a genuinely registrable
//! `FrameDefinition` from a bare `Frame` value alone.
//!
//! This adapter instead maps each `Frame` to a **fixed, well-known** `frame_id` string that
//! names the axes kind and a placeholder origin. The mapping is total and stable in both
//! directions -- but a `frame_id` produced here is *not*, by itself, a `FrameDefinition`
//! registered anywhere (in particular `Enu`/`Ned`/`Body` have no recorded geodetic point or
//! reference entity). A caller that needs a real, usable frame must register one and remap.
//! This is a recorded limitation of the `spoore.v0` shape, not a silent approximation.
//!
//! | `spoore_cdm::Frame` | v1 `frame_id`           | `AxesKind` (documentation only)   | origin (documentation only)              |
//! |----------------------|---------------------------|-------------------------------------|---------------------------------------------|
//! | `Ecef`                | `earth.ecef`               | `AXES_KIND_BODY_FIXED`               | body `"Earth"`                                |
//! | `Enu`                  | `earth.enu.generic`        | `AXES_KIND_ENU`                      | body `"Earth"`, no geodetic point recorded    |
//! | `Ned`                  | `earth.ned.generic`        | `AXES_KIND_NED`                      | body `"Earth"`, no geodetic point recorded    |
//! | `Body`                 | `platform.body.generic`    | `AXES_KIND_PLATFORM_BODY`            | no `reference_entity_id` recorded             |
//! | `LocalCartesian`       | `sim.local_cartesian`      | `AXES_KIND_LOCAL_CARTESIAN`          | none (frameless)                              |

use spoore_cdm::Frame;

use super::error::Error;

pub const ECEF: &str = "earth.ecef";
pub const ENU: &str = "earth.enu.generic";
pub const NED: &str = "earth.ned.generic";
pub const BODY: &str = "platform.body.generic";
pub const LOCAL_CARTESIAN: &str = "sim.local_cartesian";

/// Every `frame_id` this adapter recognizes, for [`Error::UnknownFrameId`].
pub const KNOWN_FRAME_IDS: &[&str] = &[ECEF, ENU, NED, BODY, LOCAL_CARTESIAN];

/// `spoore_cdm::Frame` -> v1 `frame_id`. Total: every `Frame` variant has an entry.
pub fn frame_to_frame_id(frame: Frame) -> &'static str {
    match frame {
        Frame::Ecef => ECEF,
        Frame::Enu => ENU,
        Frame::Ned => NED,
        Frame::Body => BODY,
        Frame::LocalCartesian => LOCAL_CARTESIAN,
    }
}

/// v1 `frame_id` -> `spoore_cdm::Frame`. Fallible: an unrecognized id is a typed error,
/// never a silent default (e.g. never quietly falling back to `LocalCartesian`).
pub fn frame_id_to_frame(frame_id: &str) -> Result<Frame, Error> {
    match frame_id {
        ECEF => Ok(Frame::Ecef),
        ENU => Ok(Frame::Enu),
        NED => Ok(Frame::Ned),
        BODY => Ok(Frame::Body),
        LOCAL_CARTESIAN => Ok(Frame::LocalCartesian),
        other => Err(Error::UnknownFrameId {
            frame_id: other.to_string(),
            known: KNOWN_FRAME_IDS,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_FRAMES: [Frame; 5] = [
        Frame::Ecef,
        Frame::Enu,
        Frame::Ned,
        Frame::Body,
        Frame::LocalCartesian,
    ];

    #[test]
    fn every_frame_round_trips_through_its_frame_id() {
        for frame in ALL_FRAMES {
            let id = frame_to_frame_id(frame);
            assert_eq!(frame_id_to_frame(id).unwrap(), frame, "frame_id {id:?}");
        }
    }

    #[test]
    fn frame_ids_are_pairwise_distinct() {
        let ids: std::collections::BTreeSet<_> = ALL_FRAMES.iter().copied().map(frame_to_frame_id).collect();
        assert_eq!(ids.len(), ALL_FRAMES.len());
    }

    #[test]
    fn unknown_frame_id_is_a_typed_error_not_a_default() {
        let err = frame_id_to_frame("no.such.frame").unwrap_err();
        match err {
            Error::UnknownFrameId { frame_id, known } => {
                assert_eq!(frame_id, "no.such.frame");
                assert_eq!(known, KNOWN_FRAME_IDS);
            }
            other => panic!("expected UnknownFrameId, got {other:?}"),
        }
    }

    #[test]
    fn empty_frame_id_is_rejected_rather_than_defaulted() {
        // Proto3's zero-value string default. A message that never set frame_id is exactly
        // the case that must not silently resolve to some frame.
        assert!(frame_id_to_frame("").is_err());
    }
}
