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
//! This module resolves a v1 `frame_id` to a `spoore_cdm::Frame` two ways, and callers pick
//! the one that fits ([`resolve_frame_id`] tries both in order):
//!
//! 1. **Encoding, and decoding a `frame_id` this adapter minted itself.** Each `Frame` maps
//!    to a **fixed, well-known** `frame_id` string that names the axes kind and a
//!    placeholder origin ([`frame_to_frame_id`] / [`frame_id_to_frame`]). The mapping is
//!    total and stable in both directions -- but a `frame_id` produced here is *not*, by
//!    itself, a `FrameDefinition` registered anywhere (in particular `Enu`/`Ned`/`Body` have
//!    no recorded geodetic point or reference entity). A caller that needs a real, usable
//!    frame must register one and remap. This is a recorded limitation of the `spoore.v0`
//!    shape, not a silent approximation.
//! 2. **Decoding a `frame_id` that names a real registry entry.** [`frame_from_definition`]
//!    resolves a `pb::FrameDefinition` (looked up by id from a caller-supplied registry, in
//!    [`resolve_frame_id`]) onto a `spoore_cdm::Frame` by its declared **origin and axes
//!    kind** (ADR-001's registry already carries both). This is the direction a DRM- or
//!    viewer-authored frame id (which lives in `altavista.frames.FrameRegistry`'s namespace,
//!    not this adapter's fixed five) must go through. It is deliberately narrower than the
//!    encoding direction: only `AXES_KIND_BODY_FIXED`/`AXES_KIND_ENU`/`AXES_KIND_NED` about
//!    the body `"Earth"`, and `AXES_KIND_PLATFORM_BODY`, have a `spoore.v0` counterpart at
//!    all, and a body-fixed/ENU/NED frame about a body that is *not* Earth is refused rather
//!    than silently mapped onto the Earth-only `Ecef`/`Enu`/`Ned` variants -- `spoore_cdm::
//!    Frame::Ecef` means *Earth*-centred Earth-fixed specifically, and a Mars body-fixed
//!    frame is not that, just because both happen to be "body-fixed". Every other axes kind
//!    (`ICRF`, `MJ2000_EQ`, `MJ2000_EC`, `RIC`, `VNB`, `VVLH`, `UNSPECIFIED`, and any variant
//!    added later that this module does not name) has no `spoore.v0` counterpart and is
//!    refused the same way.
//!
//! [`resolve_frame_id`] is the combination a caller with a registry (however small) should
//! use: look `frame_id` up in the registry first (direction 2), and fall back to the fixed
//! five (direction 1) only when the registry does not name it, so nothing that resolves
//! today through the literal path stops resolving.
//!
//! **The one asymmetry that remains, and the one that was removed.** Four of the five
//! `spoore_cdm::Frame` variants are reachable from a registry entry by origin and axes;
//! `AXES_KIND_LOCAL_CARTESIAN` ("Frameless cartesian space (simulation and unit tests)",
//! `core.proto`'s own words) resolves to `Frame::LocalCartesian`, deliberately without
//! consulting `origin`, since a frameless frame has no meaningful one. An earlier draft of
//! this module refused that axes kind, which would have meant a producer that properly
//! *registered* its frameless simulation frame was rejected where one that passed the bare
//! `sim.local_cartesian` literal was accepted -- the two paths must agree, and a test now
//! pins that they do. What genuinely has no registry counterpart is the reverse direction:
//! [`frame_to_frame_id`] still yields a fixed, well-known id string and not a registrable
//! `FrameDefinition`, for the reason the paragraphs above give.
//!
//! | `spoore_cdm::Frame` | v1 `frame_id`           | `AxesKind` (documentation only)   | origin (documentation only)              |
//! |----------------------|---------------------------|-------------------------------------|---------------------------------------------|
//! | `Ecef`                | `earth.ecef`               | `AXES_KIND_BODY_FIXED`               | body `"Earth"`                                |
//! | `Enu`                  | `earth.enu.generic`        | `AXES_KIND_ENU`                      | body `"Earth"`, no geodetic point recorded    |
//! | `Ned`                  | `earth.ned.generic`        | `AXES_KIND_NED`                      | body `"Earth"`, no geodetic point recorded    |
//! | `Body`                 | `platform.body.generic`    | `AXES_KIND_PLATFORM_BODY`            | no `reference_entity_id` recorded             |
//! | `LocalCartesian`       | `sim.local_cartesian`      | `AXES_KIND_LOCAL_CARTESIAN`          | none (frameless)                              |
//!
//! ## Registry resolution ([`frame_from_definition`] / [`resolve_frame_id`])
//!
//! | declared `origin`        | declared `axes`              | resolves to    |
//! |---------------------------|-------------------------------|-----------------|
//! | `body("Earth")`            | `AXES_KIND_BODY_FIXED`          | `Frame::Ecef`     |
//! | `body("Earth")`            | `AXES_KIND_ENU`                 | `Frame::Enu`      |
//! | `body("Earth")`            | `AXES_KIND_NED`                 | `Frame::Ned`      |
//! | any                         | `AXES_KIND_PLATFORM_BODY`       | `Frame::Body`     |
//! | any                         | `AXES_KIND_LOCAL_CARTESIAN`     | `Frame::LocalCartesian` |
//! | `body(` anything but `"Earth"` `)` | `AXES_KIND_BODY_FIXED`/`ENU`/`NED` | `Error::UnmappableFrame` |
//! | any                         | `ICRF`/`MJ2000_EQ`/`MJ2000_EC`/`RIC`/`VNB`/`VVLH`/`UNSPECIFIED`/unknown | `Error::UnmappableFrame` |
//! | (not found in the registry) | --                              | falls back to [`frame_id_to_frame`] |

use spoore_cdm::Frame;

use super::error::Error;
use crate::pb;

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

/// Every `AxesKind` this module names, for rendering an arbitrary `axes: i32` into an error
/// message via [`pb::AxesKind::as_str_name`] -- there is no generated `TryFrom<i32>` for
/// `pb::AxesKind` in this workspace (see `crates/av-kernel/src/drm/executor.rs`'s own
/// `f.axes == pb::AxesKind::X as i32` convention), so this is a linear search over the
/// enum's own values rather than a fallible conversion.
const ALL_AXES_KINDS: &[pb::AxesKind] = &[
    pb::AxesKind::Unspecified,
    pb::AxesKind::Icrf,
    pb::AxesKind::Mj2000Eq,
    pb::AxesKind::Mj2000Ec,
    pb::AxesKind::BodyFixed,
    pb::AxesKind::Enu,
    pb::AxesKind::Ned,
    pb::AxesKind::PlatformBody,
    pb::AxesKind::Ric,
    pb::AxesKind::Vnb,
    pb::AxesKind::Vvlh,
    pb::AxesKind::LocalCartesian,
];

/// Renders a `FrameDefinition.axes` value for an error message: its proto enum name
/// (`AXES_KIND_...`) when recognized, or the raw integer when it is not (a value no build of
/// this proto has ever declared).
fn axes_description(axes: i32) -> String {
    ALL_AXES_KINDS
        .iter()
        .find(|kind| **kind as i32 == axes)
        .map(|kind| kind.as_str_name().to_string())
        .unwrap_or_else(|| format!("AXES_KIND(unrecognized value {axes})"))
}

/// Renders a `FrameDefinition.origin` value for an error message.
fn origin_description(def: &pb::FrameDefinition) -> String {
    match &def.origin {
        Some(pb::frame_definition::Origin::Body(body)) => format!("body({body:?})"),
        Some(pb::frame_definition::Origin::PlatformId(id)) => format!("platform_id({id:?})"),
        Some(pb::frame_definition::Origin::EntityId(id)) => format!("entity_id({id:?})"),
        None => "<unset>".to_string(),
    }
}

fn unmappable(def: &pb::FrameDefinition) -> Error {
    Error::UnmappableFrame {
        frame_id: def.id.clone(),
        origin: origin_description(def),
        axes: axes_description(def.axes),
    }
}

/// Resolves a real `FrameDefinition` registry entry onto a `spoore_cdm::Frame`, by its
/// declared `origin` and `axes` -- never by its `id` string (an id is just a label; two
/// registries could use different ids for the same physical frame, or the same id for
/// different ones before validation, so only origin+axes are trustworthy here).
///
/// See this module's own doc table for the full mapping and every refusal case. In
/// particular: **a body-fixed (or ENU/NED) frame about a body other than `"Earth"` is
/// always [`Error::UnmappableFrame`], never `Frame::Ecef`/`Enu`/`Ned`** -- those variants
/// name Earth specifically, and treating "body-fixed" alone as sufficient would silently
/// hand a Mars-fixed measurement to a consumer that reads it as Earth-fixed.
pub fn frame_from_definition(def: &pb::FrameDefinition) -> Result<Frame, Error> {
    let is_earth_body = matches!(&def.origin, Some(pb::frame_definition::Origin::Body(body)) if body == "Earth");

    if def.axes == pb::AxesKind::BodyFixed as i32 {
        return if is_earth_body { Ok(Frame::Ecef) } else { Err(unmappable(def)) };
    }
    if def.axes == pb::AxesKind::Enu as i32 {
        return if is_earth_body { Ok(Frame::Enu) } else { Err(unmappable(def)) };
    }
    if def.axes == pb::AxesKind::Ned as i32 {
        return if is_earth_body { Ok(Frame::Ned) } else { Err(unmappable(def)) };
    }
    if def.axes == pb::AxesKind::PlatformBody as i32 {
        // `Frame::Body` records no origin of its own (this module's top doc table: "no
        // `reference_entity_id` recorded"), so which oneof variant `origin` uses does not
        // change the result -- unlike body-fixed/ENU/NED, there is no Earth-only variant of
        // `Frame::Body` to guard against conflating with a non-Earth one.
        return Ok(Frame::Body);
    }

    if def.axes == pb::AxesKind::LocalCartesian as i32 {
        // `core.proto`'s own doc comment for this value is "Frameless cartesian space
        // (simulation and unit tests)", which is exactly what `spoore_cdm::Frame::
        // LocalCartesian` means, and this module's own top-doc table has paired the two
        // since round 1. Whatever `origin` a frameless frame happens to declare (usually
        // nothing) cannot change that, so this arm is deliberately origin-independent --
        // unlike body-fixed/ENU/NED, there is no Earth-only variant to conflate it with.
        return Ok(Frame::LocalCartesian);
    }

    // `ICRF`, `MJ2000_EQ`, `MJ2000_EC`, `RIC`, `VNB`, `VVLH`, `UNSPECIFIED`, and any future
    // axes kind: no `spoore.v0` counterpart, so a typed refusal rather than a guess.
    Err(unmappable(def))
}

/// Resolves `frame_id` to a `spoore_cdm::Frame`, preferring a real registry entry
/// ([`frame_from_definition`]) and falling back to this adapter's own fixed five
/// ([`frame_id_to_frame`]) when `frame_id` is not in `registry` -- so a caller that starts
/// passing a non-empty registry never regresses a `frame_id` that already resolved through
/// the literal path (an empty `registry` reduces exactly to [`frame_id_to_frame`]).
pub fn resolve_frame_id(frame_id: &str, registry: &[pb::FrameDefinition]) -> Result<Frame, Error> {
    match registry.iter().find(|def| def.id == frame_id) {
        Some(def) => frame_from_definition(def),
        None => frame_id_to_frame(frame_id),
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

    fn earth_body_fixed(id: &str) -> pb::FrameDefinition {
        pb::FrameDefinition {
            id: id.to_string(),
            origin: Some(pb::frame_definition::Origin::Body("Earth".to_string())),
            axes: pb::AxesKind::BodyFixed as i32,
            ..Default::default()
        }
    }

    // ---------------------------------------------------------------------
    // Every fixed id, through the registry path and through the literal path, with
    // `LocalCartesian` pinned to resolve identically either way.
    // ---------------------------------------------------------------------

    #[test]
    fn registry_resolves_earth_body_fixed_to_ecef() {
        let def = earth_body_fixed("some.registered.frame");
        assert_eq!(frame_from_definition(&def).unwrap(), Frame::Ecef);

        let registry = [def];
        assert_eq!(resolve_frame_id("some.registered.frame", &registry).unwrap(), Frame::Ecef);
    }

    #[test]
    fn registry_resolves_earth_enu_to_enu() {
        let def = pb::FrameDefinition {
            id: "some.enu.frame".to_string(),
            origin: Some(pb::frame_definition::Origin::Body("Earth".to_string())),
            axes: pb::AxesKind::Enu as i32,
            ..Default::default()
        };
        assert_eq!(frame_from_definition(&def).unwrap(), Frame::Enu);

        let registry = [def];
        assert_eq!(resolve_frame_id("some.enu.frame", &registry).unwrap(), Frame::Enu);
    }

    #[test]
    fn registry_resolves_earth_ned_to_ned() {
        let def = pb::FrameDefinition {
            id: "some.ned.frame".to_string(),
            origin: Some(pb::frame_definition::Origin::Body("Earth".to_string())),
            axes: pb::AxesKind::Ned as i32,
            ..Default::default()
        };
        assert_eq!(frame_from_definition(&def).unwrap(), Frame::Ned);

        let registry = [def];
        assert_eq!(resolve_frame_id("some.ned.frame", &registry).unwrap(), Frame::Ned);
    }

    #[test]
    fn registry_resolves_platform_body_to_body() {
        let def = pb::FrameDefinition {
            id: "some.platform.frame".to_string(),
            origin: Some(pb::frame_definition::Origin::PlatformId("ground-1".to_string())),
            axes: pb::AxesKind::PlatformBody as i32,
            ..Default::default()
        };
        assert_eq!(frame_from_definition(&def).unwrap(), Frame::Body);

        let registry = [def];
        assert_eq!(resolve_frame_id("some.platform.frame", &registry).unwrap(), Frame::Body);
    }

    #[test]
    fn platform_body_also_resolves_with_an_entity_id_origin() {
        let def = pb::FrameDefinition {
            id: "some.entity.platform.frame".to_string(),
            origin: Some(pb::frame_definition::Origin::EntityId("sat-1".to_string())),
            axes: pb::AxesKind::PlatformBody as i32,
            ..Default::default()
        };
        assert_eq!(frame_from_definition(&def).unwrap(), Frame::Body);
    }

    #[test]
    fn local_cartesian_resolves_through_both_the_literal_id_and_a_registry_entry() {
        assert_eq!(resolve_frame_id(LOCAL_CARTESIAN, &[]).unwrap(), Frame::LocalCartesian);

        // A registry entry declaring AXES_KIND_LOCAL_CARTESIAN ("Frameless cartesian space",
        // `core.proto`) resolves the same way the literal id does. The two paths must agree:
        // a DRM that properly *registers* its frameless simulation frame must not be refused
        // where an unregistered literal id is accepted.
        let def = pb::FrameDefinition {
            id: "some.local.frame".to_string(),
            axes: pb::AxesKind::LocalCartesian as i32,
            ..Default::default()
        };
        assert_eq!(frame_from_definition(&def).unwrap(), Frame::LocalCartesian);
        assert_eq!(resolve_frame_id("some.local.frame", &[def]).unwrap(), Frame::LocalCartesian);
    }

    #[test]
    fn a_registered_local_cartesian_frame_agrees_with_the_literal_id_path() {
        // The regression this pair of paths exists to prevent: `sim.local_cartesian` (the
        // fixed id) and a registry entry with AXES_KIND_LOCAL_CARTESIAN must land on the same
        // `Frame`, so the adapter's answer does not depend on whether the producer bothered
        // to register the frame.
        let def = pb::FrameDefinition { id: LOCAL_CARTESIAN.to_string(), axes: pb::AxesKind::LocalCartesian as i32, ..Default::default() };
        assert_eq!(resolve_frame_id(LOCAL_CARTESIAN, &[def]).unwrap(), resolve_frame_id(LOCAL_CARTESIAN, &[]).unwrap());
    }

    // ---------------------------------------------------------------------
    // The typed error, one refusal class per test.
    // ---------------------------------------------------------------------

    #[test]
    fn body_fixed_about_a_non_earth_body_is_unmappable_not_ecef() {
        let def = pb::FrameDefinition {
            id: "mars.body.fixed".to_string(),
            origin: Some(pb::frame_definition::Origin::Body("Mars".to_string())),
            axes: pb::AxesKind::BodyFixed as i32,
            ..Default::default()
        };
        let err = frame_from_definition(&def).unwrap_err();
        let msg = err.to_string();
        match &err {
            Error::UnmappableFrame { frame_id, origin, axes } => {
                assert_eq!(frame_id, "mars.body.fixed");
                assert!(origin.contains("Mars"), "{origin:?}");
                assert_eq!(axes, "AXES_KIND_BODY_FIXED");
            }
            other => panic!("expected UnmappableFrame, got {other:?}"),
        }
        assert!(msg.contains("mars.body.fixed"));
    }

    #[test]
    fn enu_about_a_non_earth_body_is_unmappable() {
        let def = pb::FrameDefinition {
            id: "luna.enu.frame".to_string(),
            origin: Some(pb::frame_definition::Origin::Body("Luna".to_string())),
            axes: pb::AxesKind::Enu as i32,
            ..Default::default()
        };
        let err = frame_from_definition(&def).unwrap_err();
        assert!(matches!(err, Error::UnmappableFrame { .. }));
        assert!(format!("{err}").contains("luna.enu.frame"));
    }

    #[test]
    fn ned_about_a_non_earth_body_is_unmappable() {
        let def = pb::FrameDefinition {
            id: "luna.ned.frame".to_string(),
            origin: Some(pb::frame_definition::Origin::Body("Luna".to_string())),
            axes: pb::AxesKind::Ned as i32,
            ..Default::default()
        };
        let err = frame_from_definition(&def).unwrap_err();
        assert!(matches!(err, Error::UnmappableFrame { .. }));
        assert!(format!("{err}").contains("luna.ned.frame"));
    }

    #[test]
    fn ric_axes_is_unmappable() {
        let def = pb::FrameDefinition {
            id: "rel.ric.frame".to_string(),
            origin: Some(pb::frame_definition::Origin::EntityId("target-1".to_string())),
            axes: pb::AxesKind::Ric as i32,
            reference_entity_id: "target-1".to_string(),
            reference_body: "Earth".to_string(),
            ..Default::default()
        };
        let err = frame_from_definition(&def).unwrap_err();
        let msg = err.to_string();
        match &err {
            Error::UnmappableFrame { frame_id, axes, .. } => {
                assert_eq!(frame_id, "rel.ric.frame");
                assert_eq!(axes, "AXES_KIND_RIC");
            }
            other => panic!("expected UnmappableFrame, got {other:?}"),
        }
        assert!(msg.contains("rel.ric.frame"));
    }

    #[test]
    fn icrf_axes_is_unmappable() {
        let def = pb::FrameDefinition {
            id: "earth.icrf".to_string(),
            origin: Some(pb::frame_definition::Origin::Body("Earth".to_string())),
            axes: pb::AxesKind::Icrf as i32,
            ..Default::default()
        };
        let err = frame_from_definition(&def).unwrap_err();
        assert!(matches!(err, Error::UnmappableFrame { .. }), "{err:?}");
        assert!(format!("{err}").contains("earth.icrf"));
    }

    #[test]
    fn mj2000_eq_axes_is_unmappable() {
        let def = pb::FrameDefinition {
            id: "earth.mj2000eq".to_string(),
            origin: Some(pb::frame_definition::Origin::Body("Earth".to_string())),
            axes: pb::AxesKind::Mj2000Eq as i32,
            ..Default::default()
        };
        let err = frame_from_definition(&def).unwrap_err();
        assert!(matches!(err, Error::UnmappableFrame { .. }), "{err:?}");
        assert!(format!("{err}").contains("earth.mj2000eq"));
    }

    #[test]
    fn unspecified_axes_is_unmappable() {
        let def = pb::FrameDefinition {
            id: "no.axes.declared".to_string(),
            origin: Some(pb::frame_definition::Origin::Body("Earth".to_string())),
            axes: pb::AxesKind::Unspecified as i32,
            ..Default::default()
        };
        let err = frame_from_definition(&def).unwrap_err();
        let msg = err.to_string();
        match &err {
            Error::UnmappableFrame { frame_id, axes, .. } => {
                assert_eq!(frame_id, "no.axes.declared");
                assert_eq!(axes, "AXES_KIND_UNSPECIFIED");
            }
            other => panic!("expected UnmappableFrame, got {other:?}"),
        }
        assert!(msg.contains("no.axes.declared"));
    }

    #[test]
    fn an_id_in_neither_the_registry_nor_the_fixed_set_is_unknown_frame_id() {
        let registry = [earth_body_fixed("some.other.frame")];
        let err = resolve_frame_id("totally.unregistered", &registry).unwrap_err();
        let msg = err.to_string();
        match &err {
            Error::UnknownFrameId { frame_id, .. } => assert_eq!(frame_id, "totally.unregistered"),
            other => panic!("expected UnknownFrameId, got {other:?}"),
        }
        assert!(msg.contains("totally.unregistered"));
    }

    #[test]
    fn resolve_frame_id_falls_back_to_the_literal_path_when_absent_from_the_registry() {
        // A registry that does not name ECEF's literal id must not block the literal path.
        let registry = [earth_body_fixed("some.other.frame")];
        assert_eq!(resolve_frame_id(ECEF, &registry).unwrap(), Frame::Ecef);
    }

    #[test]
    fn resolve_frame_id_prefers_the_registry_over_a_same_named_literal_id_collision() {
        // If a registry entry happens to reuse one of the fixed ids' own spelling, the
        // registry entry wins (this is the "decode a registry entry" direction; the literal
        // fallback exists only for ids the registry does not mention at all).
        let def = pb::FrameDefinition {
            id: ENU.to_string(),
            origin: Some(pb::frame_definition::Origin::Body("Earth".to_string())),
            axes: pb::AxesKind::BodyFixed as i32,
            ..Default::default()
        };
        let registry = [def];
        assert_eq!(resolve_frame_id(ENU, &registry).unwrap(), Frame::Ecef);
    }
}
