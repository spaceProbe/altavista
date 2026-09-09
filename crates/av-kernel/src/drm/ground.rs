//! The ground segment as a system (M25.1, `docs/sil-plan.md`'s M25 milestone: "ground segment as
//! a system, CCSDS telecommands from DRM command events"; `docs/open-questions.md` questions 10,
//! 108, 149, 156, 157, 164).
//!
//! ## Scope of this batch, mirroring `crate::drm::sensors`'/`crate::drm::attitude`'s own notes
//!
//! [`GroundStationModel`] is a real, directly constructible, directly testable
//! [`av_dynamics::DynamicsModel`]: a native ground-station instance with no propagated physical
//! state (`state_dim() == 0`, exactly like `crate::drm::sensors::StarTrackerModel`/`crate::drm::
//! controller::AttitudeControllerModel` -- an instantaneous geometric transform has nothing to
//! integrate), wired end to end into `super::binding::classify_binding`/`AnyModel`, `crate::
//! registry::ModelRegistry`, and `super::fault::apply_dynamics_fault` -- **not** left half-wired
//! the way `crate::drm::attitude`/`crate::drm::sensors` were through their own first batch (see
//! those modules' own "Scope" notes): the lead's standing rule for this task explicitly requires
//! `classify_binding`/`AnyModel`/`ModelRegistry` wiring with deliberate arms, so this module does
//! it in the same change that builds the model, not a follow-up.
//!
//! ## The site representation: `Geodetic`, not an invented one (question 10)
//!
//! `docs/open-questions.md` question 10 already mandates ENU/NED at an origin as a day-one
//! frame, and `core.proto`'s `FrameDefinition.origin_geodetic` ("Required for ENU / NED") already
//! carries a ground station's declared position as a `Geodetic` (body, latitude_rad,
//! longitude_rad, height_m). This module's own [`GroundStationSpec`] carries the *same* four
//! numbers as `"station.*"` instance parameters (`station.body`, `station.latitude_rad`, `.
//! longitude_rad`, `.height_m`) -- not a second, competing representation. **Why not a live
//! `FrameDefinition` lookup at model-construction time**: every other native model's own spec
//! parser (`parse_star_tracker_spec`, `parse_attitude_spec`, ...) is handed only the instance's
//! flat, effective `BTreeMap<String, Parameter>` (`binding::classify_binding`'s own
//! `effective_parameters`) -- `Scenario.frames` is not threaded through that call at all (frames
//! are realized elsewhere, `executor::collect_frames`/`registry_default_frame`, question 124).
//! Threading `Scenario.frames` into every native model constructor to support one instance kind
//! is a wider change than this task's scope justifies; instead, a DRM authoring this instance
//! declares *both* the `"station.*"` parameters this model reads directly *and* a `FrameDefinition`
//! (`axes: AXES_KIND_ENU`, `origin_geodetic` carrying the identical four numbers, `platform_id`
//! naming this instance) in `Scenario.frames`, for the CDM/viewer's own frame graph -- see
//! `drms/demo_ground_segment.drm.yaml`'s own `scenario.frames` entry. The two are kept in
//! agreement by construction (the fixture author writes the same numbers twice, not derives one
//! from the other), the same "additive, not a live coupling" tradeoff `crate::drm::sensors`'s own
//! module doc comment makes for the truth link (a generic `Scenario.frames`-consuming model
//! constructor is real future work, not invented here).
//!
//! ## Topocentric geometry: closed-form, GMAT-free, matching the site's own declared axes
//!
//! [`geodetic_to_ecef_m`]/[`ecef_to_topocentric`]/[`elevation_rad`] implement the standard WGS84
//! ellipsoidal geodetic-to-ECEF transform and the local East-North-Up (`AXES_KIND_ENU`) rotation
//! -- pure Rust, no GMAT dependency, matching every other native model's own "GMAT-free, cheap"
//! contract (`crate::drm::sensors`'s own module doc comment). **The wire convention this module
//! declares for `tm_in`: the decoded x/y/z is the target's own Earth-fixed (body-fixed, ECEF)
//! Cartesian position, metres.** Choosing Earth-fixed (not inertial) as the wire convention means
//! this module never has to model Earth's own rotation (GMST/UT1) to compute a topocentric
//! elevation angle -- that conversion is already part of the dynamics contract (ADR-002's fourth
//! amendment, `gmat_sys::Gmat::convert`) on the *producing* side, so a real flight instance's own
//! trajectory is emitted in `AXES_KIND_BODY_FIXED` once that producer declares it, exactly the
//! way `docs/adr/002-dynamics-contract.md`'s fourth amendment already establishes for any other
//! frame. See [`ecef_to_topocentric`]'s own doc comment for the elevation formula itself, and the
//! acceptance-pin test (`crates/av-kernel/tests/ground_contact_gmat.rs`) for the GMAT `ContactLocator`
//! comparison this geometry is pinned against.
//!
//! ## Contact windows and the elevation mask
//!
//! [`GroundStationModel::step_with_ports`] decodes the latest `tm_in` packet each step (`Inbox::
//! last_on_port`, question 108's own delivery-order guarantee), computes the topocentric
//! elevation angle, and compares it against the declared `station.elevation_mask_rad`. A rising
//! edge (not visible -> visible) sends one FRAMED telecommand packet on `tc_out` (an "AOS"
//! acknowledgment, demonstrating the router-mediated link this module's own doc comment's "link"
//! section covers) and reports the transition as an `av_dynamics::AppliedCommand` on the reserved
//! synthetic port [`CONTACT_TRANSITION_PORT`] (never a real declared `Port` name, so it can never
//! collide with router-delivered traffic) -- `executor::run_shared_group`'s own applied-command
//! drain (question 130's existing mechanism) turns each one into a real `EVENT_KIND_CONTACT_START`/
//! `EVENT_KIND_CONTACT_END` CDM event (`super::events::contact_event`), the same "an applied
//! command becomes a CDM event, once, at the point it is actually applied" pipeline `EVENT_KIND_
//! PORT_COMMAND` already uses -- see `super::events`'s own module doc comment for exactly why the
//! reserved port name, not a new field on `av_dynamics::AppliedCommand`, is the seam: growing that
//! struct would ripple through every `DynamicsModel` in this workspace for one model's own need.
//!
//! ## Link model: the router's own latency, not reimplemented
//!
//! Bullet 1's "a link with the existing latency model" is satisfied structurally, not by any code
//! in this module: `tm_in`/`tc_out` are ordinary `PORT_KIND_FRAMED` ports with a declared
//! `PortTiming.latency_ns`, and the `Connection.link_model = "latency"` a fixture's own `.sos.yaml`
//! declares is resolved by `crate::router::Router::effective_latency_ns` exactly like every other
//! FRAMED connection in this workspace (`crate::router`'s own module doc comment, "Link model").
//! This module never computes or reimplements a latency figure of its own.
//!
//! ## Fault / maneuver / covariance (the lead's "decide and state" requirement)
//!
//! - **DYNAMICS fault**: ADR-005 sec 5's general rule ("DYNAMICS sets a model parameter through
//!   the model's declared parameter interface") applies unchanged -- `super::fault::
//!   apply_ground_station_target` lets a fault retarget `station.elevation_mask_rad` (a mission
//!   author lowering/raising a site's usable horizon mid-run, e.g. modeling a antenna outage
//!   raising the effective mask) or `station.latitude_rad`/`.longitude_rad`/`.height_m` (a mobile
//!   or relocated ground asset) -- every `"station.*"` field this module's own spec declares is
//!   writable, the same "every declared parameter is a valid DYNAMICS fault target" contract
//!   `crate::drm::sensors`'s own `apply_star_tracker_target`/`apply_imu_target` already establish.
//! - **Maneuver**: `GroundStationModel::state_dim() == 0`, not the 6-component Cartesian
//!   position/velocity shape a dv jump requires -- `executor.rs`'s existing `DrmError::
//!   ManeuverTargetNotSixDimensional` refusal already generically catches any non-6 state (the
//!   `last_state.as_slice().try_into()` conversion fails), and this module's own `BindingPlan::
//!   GroundStation(_)` is additionally named explicitly in `executor.rs`'s own maneuver-boundary
//!   `matches!` guard, for parity with `BindingPlan::Imu`/`StarTracker`/`Controller`'s own explicit
//!   listing there ("naming it explicitly here proves the refusal is deliberate, not an accident
//!   of the generic check" -- that guard's own comment, M22.4).
//! - **Covariance**: refused, the same way every other native model in this workspace refuses it
//!   -- `GroundStationModel::stm_capable()` is unconditionally `false` (no closed-form state
//!   transition matrix; a fixed geodetic point has no propagated state to carry an STM at all),
//!   so `executor::run_covariance_instance`'s existing generic `DrmError::ModelNotStmCapable`
//!   refusal applies with no ground-station-specific code.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::fmt;

use av_cdm::pb::{self, ModelInfo, Parameter};
use av_dynamics::{AppliedCommand, DynamicsModel, Inbox, Outbox, StepResult};

use crate::codec::{self, CodecError, FieldValue};

// ============================================================================================
// Topocentric geometry: WGS84 geodetic -> ECEF, ECEF -> local ENU, elevation angle.
// ============================================================================================

/// WGS84 semi-major axis, metres.
pub const WGS84_A_M: f64 = 6_378_137.0;
/// WGS84 flattening.
pub const WGS84_F: f64 = 1.0 / 298.257223563;

/// Geodetic (ellipsoidal) latitude/longitude/height -> Earth-fixed (ECEF) Cartesian position,
/// metres. Standard closed-form transform (Vallado, *Fundamentals of Astrodynamics*, or any WGS84
/// reference): `N = a / sqrt(1 - e^2 sin^2(lat))`, `x = (N+h) cos(lat) cos(lon)`, `y = (N+h)
/// cos(lat) sin(lon)`, `z = (N(1-e^2)+h) sin(lat)`. GMAT's own `GroundStation` resource realizes
/// `HorizonReference = Ellipsoid` (WGS84) identically -- see this module's own doc comment and the
/// acceptance-pin test for the cross-check.
pub fn geodetic_to_ecef_m(lat_rad: f64, lon_rad: f64, height_m: f64) -> [f64; 3] {
    let e2 = WGS84_F * (2.0 - WGS84_F);
    let sin_lat = lat_rad.sin();
    let n = WGS84_A_M / (1.0 - e2 * sin_lat * sin_lat).sqrt();
    let x = (n + height_m) * lat_rad.cos() * lon_rad.cos();
    let y = (n + height_m) * lat_rad.cos() * lon_rad.sin();
    let z = (n * (1.0 - e2) + height_m) * sin_lat;
    [x, y, z]
}

/// Rotate an ECEF displacement (`target_ecef_m - site_ecef_m`) into the site's own local
/// East-North-Up basis (`AXES_KIND_ENU`, `core.proto`) using the site's geodetic latitude/
/// longitude -- the standard topocentric rotation matrix (row `i` of the rotation is the ECEF
/// components of local basis vector `i`):
/// ```text
/// east  = -sin(lon) dx + cos(lon) dy
/// north = -sin(lat) cos(lon) dx - sin(lat) sin(lon) dy + cos(lat) dz
/// up    =  cos(lat) cos(lon) dx + cos(lat) sin(lon) dy + sin(lat) dz
/// ```
/// Returns `[east, north, up]`, metres. `AXES_KIND_NED` (`core.proto`) is `[north, east, -up]` of
/// this same triple, not a second computation.
pub fn ecef_to_topocentric(site_lat_rad: f64, site_lon_rad: f64, site_ecef_m: [f64; 3], target_ecef_m: [f64; 3]) -> [f64; 3] {
    let d = [target_ecef_m[0] - site_ecef_m[0], target_ecef_m[1] - site_ecef_m[1], target_ecef_m[2] - site_ecef_m[2]];
    let (sin_lat, cos_lat) = (site_lat_rad.sin(), site_lat_rad.cos());
    let (sin_lon, cos_lon) = (site_lon_rad.sin(), site_lon_rad.cos());
    let east = -sin_lon * d[0] + cos_lon * d[1];
    let north = -sin_lat * cos_lon * d[0] - sin_lat * sin_lon * d[1] + cos_lat * d[2];
    let up = cos_lat * cos_lon * d[0] + cos_lat * sin_lon * d[1] + sin_lat * d[2];
    [east, north, up]
}

/// Elevation angle above the local horizontal, radians (`asin(up / |enu|)`), `[-pi/2, pi/2]`.
/// `enu` is an [`ecef_to_topocentric`] result. Returns `0.0` (on the horizon, neither positive
/// nor negative) for a target exactly at the site (`|enu| == 0`) rather than `NaN` -- a
/// degenerate input a real fixture never produces, handled rather than propagating a silent NaN.
pub fn elevation_rad(enu: [f64; 3]) -> f64 {
    let horiz = (enu[0] * enu[0] + enu[1] * enu[1]).sqrt();
    if horiz == 0.0 && enu[2] == 0.0 {
        return 0.0;
    }
    enu[2].atan2(horiz)
}

/// One (site, target) elevation angle at one epoch -- `geodetic_to_ecef_m`, then
/// `ecef_to_topocentric`, then `elevation_rad`, composed as the single call
/// [`GroundStationModel::step_with_ports`] and the acceptance-pin test both use, so there is
/// exactly one place the three-step geometry is chained.
pub fn elevation_of(site: &GroundStationSpec, target_ecef_m: [f64; 3]) -> f64 {
    let site_ecef = geodetic_to_ecef_m(site.latitude_rad, site.longitude_rad, site.height_m);
    let enu = ecef_to_topocentric(site.latitude_rad, site.longitude_rad, site_ecef, target_ecef_m);
    elevation_rad(enu)
}

/// One contact window: `[start_tai_ns, end_tai_ns)`, both endpoints linearly interpolated between
/// the bracketing samples that crossed `elevation_mask_rad` (see [`contact_windows`]'s own doc
/// comment for the interpolation and its accuracy).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContactWindow {
    pub start_tai_ns: i64,
    pub end_tai_ns: i64,
}

/// Scan a time-ordered series of `(tai_ns, elevation_rad)` samples for the intervals where
/// elevation is at or above `elevation_mask_rad` -- a pure function of already-computed elevation
/// samples, used both by [`GroundStationModel`] (fed one sample per step) and the acceptance-pin
/// test (fed a whole GMAT-reported arc at once), so there is exactly one contact-window algorithm
/// in this module.
///
/// **Interpolation, not "nearest sample".** Elevation crosses the mask between two samples at
/// essentially the local rate of change near the horizon (a smooth, nearly-linear function of
/// time over one short sampling interval for a LEO pass); this function linearly interpolates the
/// crossing epoch between the bracketing pair, `t = t0 + (mask - e0)/(e1 - e0) * (t1 - t0)`,
/// rather than snapping to whichever sample happened to land on the correct side. This is what
/// keeps the reported AOS/LOS epoch's error small compared to the raw sample spacing -- see the
/// acceptance-pin test's own doc comment for the measured error against GMAT's `ContactLocator`
/// and the tolerance that measurement justifies.
///
/// An arc that starts already above the mask opens a window at the first sample (no earlier
/// crossing to interpolate); an arc that ends still above the mask closes a window at the last
/// sample, for the same reason. `samples` must be sorted by `tai_ns`, ascending, strictly
/// increasing (a caller bug otherwise -- `debug_assert!`ed, not defended against at runtime, the
/// same precondition style `crate::interpolate` uses elsewhere in this crate).
pub fn contact_windows(elevation_mask_rad: f64, samples: &[(i64, f64)]) -> Vec<ContactWindow> {
    debug_assert!(samples.windows(2).all(|w| w[0].0 < w[1].0), "contact_windows requires strictly increasing, sorted tai_ns samples");
    let mut windows = Vec::new();
    let mut open: Option<i64> = None;
    if let Some(&(t0, e0)) = samples.first() {
        if e0 >= elevation_mask_rad {
            open = Some(t0);
        }
    }
    for pair in samples.windows(2) {
        let (t0, e0) = pair[0];
        let (t1, e1) = pair[1];
        let above0 = e0 >= elevation_mask_rad;
        let above1 = e1 >= elevation_mask_rad;
        if !above0 && above1 {
            // Rising edge: interpolate the AOS epoch between t0 (below) and t1 (at/above).
            let frac = (elevation_mask_rad - e0) / (e1 - e0);
            let t_cross = t0 + ((t1 - t0) as f64 * frac).round() as i64;
            open = Some(t_cross);
        } else if above0 && !above1 {
            // Falling edge: interpolate the LOS epoch between t0 (at/above) and t1 (below).
            let frac = (elevation_mask_rad - e0) / (e1 - e0);
            let t_cross = t0 + ((t1 - t0) as f64 * frac).round() as i64;
            if let Some(start) = open.take() {
                windows.push(ContactWindow { start_tai_ns: start, end_tai_ns: t_cross });
            }
        }
    }
    if let (Some(start), Some(&(t_last, _))) = (open, samples.last()) {
        windows.push(ContactWindow { start_tai_ns: start, end_tai_ns: t_last });
    }
    windows
}

// ============================================================================================
// Typed parameter parsing (mirrors crate::drm::sensors::SensorSpecError's own contract).
// ============================================================================================

/// Everything [`parse_ground_station_spec`]/[`GroundStationModel::new`] can refuse -- a local,
/// dedicated error type, mirroring `crate::drm::sensors::SensorSpecError`'s own reasoning (wrapped
/// by `super::DrmError::InvalidGroundStationSpec` at the `classify_binding` call site, not
/// re-derived as a parallel set of `DrmError` variants).
#[derive(Debug, Clone, PartialEq)]
pub enum GroundSpecError {
    MissingParameter { name: String },
    UnknownParameter { name: String },
    InvalidParameter { name: String, reason: String },
    Codec(CodecError),
}

impl fmt::Display for GroundSpecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GroundSpecError::MissingParameter { name } => write!(f, "missing required ground station parameter {name:?}"),
            GroundSpecError::UnknownParameter { name } => write!(f, "unrecognized ground station parameter {name:?}"),
            GroundSpecError::InvalidParameter { name, reason } => write!(f, "ground station parameter {name:?} is invalid: {reason}"),
            GroundSpecError::Codec(e) => write!(f, "declared PacketCodec is invalid: {e}"),
        }
    }
}
impl std::error::Error for GroundSpecError {}

/// Parsed, typed parameters for [`GroundStationModel`] -- built by [`parse_ground_station_spec`].
/// Parameter vocabulary (a name matching none of these is [`GroundSpecError::UnknownParameter`]):
/// - `station.body` (required, non-empty): central body name as GMAT/the frame registry knows it
///   (`"Earth"`, ...) -- mirrors `core.proto`'s `Geodetic.body`.
/// - `station.latitude_rad` / `station.longitude_rad` (required, finite, latitude in
///   `[-pi/2, pi/2]`): the site's geodetic position -- mirrors `Geodetic.latitude_rad`/
///   `.longitude_rad`.
/// - `station.height_m` (required, finite): height above the reference ellipsoid, metres --
///   mirrors `Geodetic.height_m`.
/// - `station.elevation_mask_rad` (required, `[0, pi/2)`): the minimum elevation angle this site
///   considers a target visible -- GMAT's `GroundStation.MinimumElevationAngle`'s own analogue.
#[derive(Debug, Clone, PartialEq)]
pub struct GroundStationSpec {
    pub body: String,
    pub latitude_rad: f64,
    pub longitude_rad: f64,
    pub height_m: f64,
    pub elevation_mask_rad: f64,
}

pub fn parse_ground_station_spec(params: &BTreeMap<String, Parameter>) -> Result<GroundStationSpec, GroundSpecError> {
    let mut body: Option<String> = None;
    let mut latitude_rad = None;
    let mut longitude_rad = None;
    let mut height_m = None;
    let mut elevation_mask_rad = None;
    for (name, p) in params {
        match name.as_str() {
            "station.body" => body = Some(p.string_value.clone()),
            "station.latitude_rad" => latitude_rad = Some(p.value),
            "station.longitude_rad" => longitude_rad = Some(p.value),
            "station.height_m" => height_m = Some(p.value),
            "station.elevation_mask_rad" => elevation_mask_rad = Some(p.value),
            // M25.1 (question 95's `"output.<name>"` convention, reused verbatim from
            // `parse_star_tracker_spec`/`parse_gmat_spec`/`parse_constant_accel_spec`): declares
            // this instance exposes `output.<instance>.<name>@time`, names no field of this spec.
            _ if name.starts_with("output.") => {}
            other => return Err(GroundSpecError::UnknownParameter { name: other.to_string() }),
        }
    }
    let body = body.filter(|b| !b.is_empty()).ok_or_else(|| GroundSpecError::MissingParameter { name: "station.body".to_string() })?;
    let latitude_rad = latitude_rad.ok_or_else(|| GroundSpecError::MissingParameter { name: "station.latitude_rad".to_string() })?;
    if !(latitude_rad.is_finite() && (-std::f64::consts::FRAC_PI_2..=std::f64::consts::FRAC_PI_2).contains(&latitude_rad)) {
        return Err(GroundSpecError::InvalidParameter { name: "station.latitude_rad".to_string(), reason: format!("must be finite and within [-pi/2, pi/2], got {latitude_rad}") });
    }
    let longitude_rad = longitude_rad.ok_or_else(|| GroundSpecError::MissingParameter { name: "station.longitude_rad".to_string() })?;
    if !longitude_rad.is_finite() {
        return Err(GroundSpecError::InvalidParameter { name: "station.longitude_rad".to_string(), reason: format!("must be finite, got {longitude_rad}") });
    }
    let height_m = height_m.ok_or_else(|| GroundSpecError::MissingParameter { name: "station.height_m".to_string() })?;
    if !height_m.is_finite() {
        return Err(GroundSpecError::InvalidParameter { name: "station.height_m".to_string(), reason: format!("must be finite, got {height_m}") });
    }
    let elevation_mask_rad = elevation_mask_rad.ok_or_else(|| GroundSpecError::MissingParameter { name: "station.elevation_mask_rad".to_string() })?;
    if !(elevation_mask_rad.is_finite() && (0.0..std::f64::consts::FRAC_PI_2).contains(&elevation_mask_rad)) {
        return Err(GroundSpecError::InvalidParameter { name: "station.elevation_mask_rad".to_string(), reason: format!("must be finite and within [0, pi/2), got {elevation_mask_rad}") });
    }
    Ok(GroundStationSpec { body, latitude_rad, longitude_rad, height_m, elevation_mask_rad })
}

// ============================================================================================
// FRAMED ports: telemetry in (position), telecommand out (AOS acknowledgment).
// ============================================================================================

/// The fixed field layout every ground-station telemetry-in `PacketCodec` this module
/// builds/expects uses: three big-endian IEEE-754 `binary64` fields, `x,y,z` -- Earth-fixed
/// (body-fixed) Cartesian position, metres (see this module's own doc comment, "Topocentric
/// geometry", for why body-fixed is the declared wire convention).
pub fn ground_tm_packet_codec(id: &str, apid: u32) -> pb::PacketCodec {
    let f = |name: &str, bit_offset: u32| pb::PacketField { name: name.to_string(), bit_offset, bit_width: 64, r#type: pb::PacketFieldType::Float64 as i32, unit: pb::Unit::Meter as i32, scale: 1.0, offset: 0.0, target: String::new() };
    pb::PacketCodec { id: id.to_string(), apid, is_command: false, secondary_header_bytes: 0, user_data_bytes: 24, fields: vec![f("x", 0), f("y", 64), f("z", 128)], description: "ground station telemetry-in: target Earth-fixed Cartesian position (M25.1)".to_string() }
}

/// The fixed field layout every ground-station telecommand-out `PacketCodec` this module
/// builds/expects uses: one big-endian `UINT32` field, `seq` -- the AOS acknowledgment's own
/// sequence count (also the CCSDS primary header's own `sequence_count`, redundantly carried in
/// the user data too so a decoder need not separately track primary-header state to see it).
pub fn ground_tc_packet_codec(id: &str, apid: u32) -> pb::PacketCodec {
    let seq_field = pb::PacketField { name: "seq".to_string(), bit_offset: 0, bit_width: 32, r#type: pb::PacketFieldType::Uint as i32, unit: pb::Unit::Dimensionless as i32, scale: 1.0, offset: 0.0, target: String::new() };
    pb::PacketCodec { id: id.to_string(), apid, is_command: true, secondary_header_bytes: 0, user_data_bytes: 4, fields: vec![seq_field], description: "ground station telecommand-out: AOS acknowledgment (M25.1)".to_string() }
}

/// Fixed conventional FRAMED port names a `"ground."`-dispatched instance's own `SystemDefinition`
/// declares -- `super::binding::resolve_ground_ports` looks these up by name, mirroring `crate::
/// drm::controller`'s own `CONTROLLER_STARTRACKER_IN_PORT`/etc. convention.
pub const GROUND_TM_IN_PORT: &str = "tm_in";
pub const GROUND_TC_OUT_PORT: &str = "tc_out";

/// Reserved synthetic "port" name [`GroundStationModel::step_with_ports`] uses to report a
/// contact transition as an `av_dynamics::AppliedCommand` (see this module's own doc comment,
/// "Contact windows and the elevation mask"). Never a real declared `Port.name`, and never
/// pushed through an `Outbox` at all -- `av_dynamics::AppliedCommand` is a side channel
/// `crate::schedule::HeteroScheduler::advance_to_with_ports` drains directly (question 130's own
/// existing mechanism), entirely separate from `crate::router::Router`'s own message delivery, so
/// this name can never collide with a real router-delivered message regardless of what a fixture
/// happens to name its own ports.
pub const CONTACT_TRANSITION_PORT: &str = "__contact_state__";
/// [`AppliedCommand::value`] convention on [`CONTACT_TRANSITION_PORT`]: contact acquired.
pub const CONTACT_START_VALUE: f64 = 1.0;
/// [`AppliedCommand::value`] convention on [`CONTACT_TRANSITION_PORT`]: contact lost.
pub const CONTACT_END_VALUE: f64 = 0.0;

/// A native ground-station `av_dynamics::DynamicsModel`: no propagated physical state
/// (`state_dim() == 0` -- a fixed geodetic site with an instantaneous visibility transform has
/// nothing to integrate, exactly like `crate::drm::sensors::StarTrackerModel`/`crate::drm::
/// controller::AttitudeControllerModel`). `type Error = std::convert::Infallible`: every physical
/// check already happened in [`parse_ground_station_spec`]/[`Self::new`] (latitude/elevation-mask
/// range, both codecs' required fields), so `step_with_ports` itself cannot fail.
#[derive(Debug)]
pub struct GroundStationModel {
    spec: GroundStationSpec,
    tm_codec: pb::PacketCodec,
    tm_port: String,
    tc_codec: pb::PacketCodec,
    tc_port: String,
    seq: Cell<u16>,
    in_contact: Cell<bool>,
    last_elevation_rad: Cell<f64>,
    info: ModelInfo,
}

impl GroundStationModel {
    /// `tm_codec`/`tc_codec` must declare (at least) the fields [`ground_tm_packet_codec`]/
    /// [`ground_tc_packet_codec`] build -- checked here, and separately validated via
    /// `crate::codec::validate_codec`, exactly like `StarTrackerModel::new`'s own two checks.
    pub fn new(spec: GroundStationSpec, tm_codec: pb::PacketCodec, tm_port: String, tc_codec: pb::PacketCodec, tc_port: String, model_id: &str) -> Result<Self, GroundSpecError> {
        codec::validate_codec(&tm_codec).map_err(GroundSpecError::Codec)?;
        codec::validate_codec(&tc_codec).map_err(GroundSpecError::Codec)?;
        for name in ["x", "y", "z"] {
            if !tm_codec.fields.iter().any(|f| f.name == name) {
                return Err(GroundSpecError::InvalidParameter { name: "station.tm_codec".to_string(), reason: format!("declared telemetry-in PacketCodec {:?} is missing required field {name:?}", tm_codec.id) });
            }
        }
        if !tc_codec.fields.iter().any(|f| f.name == "seq") {
            return Err(GroundSpecError::InvalidParameter { name: "station.tc_codec".to_string(), reason: format!("declared telecommand-out PacketCodec {:?} is missing required field \"seq\"", tc_codec.id) });
        }
        let mut settings = BTreeMap::new();
        settings.insert("body".to_string(), spec.body.clone());
        settings.insert("latitude_rad".to_string(), format!("{:.17e}", spec.latitude_rad));
        settings.insert("longitude_rad".to_string(), format!("{:.17e}", spec.longitude_rad));
        settings.insert("height_m".to_string(), format!("{:.17e}", spec.height_m));
        settings.insert("elevation_mask_rad".to_string(), format!("{:.17e}", spec.elevation_mask_rad));
        settings.insert("tm_port".to_string(), tm_port.clone());
        settings.insert("tm_apid".to_string(), tm_codec.apid.to_string());
        settings.insert("tc_port".to_string(), tc_port.clone());
        settings.insert("tc_apid".to_string(), tc_codec.apid.to_string());
        let settings_hash = av_dynamics::settings_hash(&settings);
        let info = ModelInfo { id: model_id.to_string(), version: "1".to_string(), state_space_id: format!("{model_id}.no_state"), frame_id: String::new(), settings_hash, depth: "native".to_string(), ..Default::default() };
        Ok(Self { spec, tm_codec, tm_port, tc_codec, tc_port, seq: Cell::new(0), in_contact: Cell::new(false), last_elevation_rad: Cell::new(f64::NAN), info })
    }

    /// This instance's own declared spec -- exposed for tests and for `super::fault::
    /// apply_ground_station_target`'s own re-materialization path.
    pub fn spec(&self) -> &GroundStationSpec {
        &self.spec
    }
}

impl DynamicsModel for GroundStationModel {
    type Error = std::convert::Infallible;

    fn state_dim(&self) -> usize {
        0
    }
    fn derivatives(&self, state: &[f64], _t_tai_ns: i64, _controls: &[f64], out: &mut [f64]) -> Result<(), Self::Error> {
        // Never reached in practice: `step`/`step_with_ports` are both overridden below and
        // never call this (there is no ODE here -- an instantaneous visibility transform has
        // nothing to integrate). An honest no-op, not `unimplemented!()`, purely to satisfy the
        // trait -- mirrors `StarTrackerModel::derivatives`'s identical note.
        debug_assert!(state.is_empty() && out.is_empty());
        Ok(())
    }
    fn describe(&self) -> ModelInfo {
        self.info.clone()
    }
    fn step(&self, state: &[f64], t_tai_ns: i64, controls: &[f64], dt_ns: i64) -> Result<StepResult, Self::Error> {
        Ok(self.step_with_ports(state, t_tai_ns, controls, dt_ns, &Inbox::empty())?.0)
    }

    /// Decode the latest `tm_in` packet in `inbox` (if any -- `Inbox::last_on_port`, question
    /// 108's own "most recently emitted, on ties" order), compute this step's own topocentric
    /// elevation angle ([`elevation_of`]), and compare it against `spec.elevation_mask_rad`. On a
    /// rising edge, encodes and pushes one `tc_out` AOS-acknowledgment packet and reports the
    /// transition via [`CONTACT_TRANSITION_PORT`] (see this module's own doc comment, "Contact
    /// windows and the elevation mask"); a falling edge reports the transition the same way with
    /// no outbound packet (there is nothing to acknowledge on loss of contact). No message this
    /// step (`inbox` carries nothing on `tm_port`) leaves `in_contact`/`last_elevation_rad`
    /// unchanged -- a gap in telemetry is not itself a loss-of-contact event.
    fn step_with_ports(&self, state: &[f64], t_tai_ns: i64, _controls: &[f64], dt_ns: i64, inbox: &Inbox) -> Result<(StepResult, Outbox, Vec<AppliedCommand>), Self::Error> {
        debug_assert!(state.is_empty());
        let end = t_tai_ns + dt_ns;
        let mut outbox = Outbox::new();
        let mut outputs = BTreeMap::new();
        let mut applied = Vec::new();

        if let Some((msg, _sender)) = inbox.last_on_port(&self.tm_port) {
            if let Ok(decoded) = codec::decode_packet(&{ let mut m = codec::ApidMap::new(); m.insert(self.tm_codec.apid, self.tm_codec.clone()); m }, &msg.payload) {
                let get = |name: &str| match decoded.fields.get(name) {
                    Some(FieldValue::Numeric(v)) => *v,
                    _ => f64::NAN,
                };
                let target_ecef_m = [get("x"), get("y"), get("z")];
                if target_ecef_m.iter().all(|v| v.is_finite()) {
                    let elevation = elevation_of(&self.spec, target_ecef_m);
                    self.last_elevation_rad.set(elevation);
                    outputs.insert("elevation_rad".to_string(), elevation);
                    let now_visible = elevation >= self.spec.elevation_mask_rad;
                    let was_visible = self.in_contact.get();
                    if now_visible && !was_visible {
                        self.in_contact.set(true);
                        applied.push(AppliedCommand { port: CONTACT_TRANSITION_PORT.to_string(), field: String::new(), value: CONTACT_START_VALUE, applied_tai_ns: end });
                        let seq = self.seq.get();
                        self.seq.set(seq.wrapping_add(1) & 0x3FFF);
                        let mut values = BTreeMap::new();
                        values.insert("seq".to_string(), FieldValue::Numeric(seq as f64));
                        let payload = codec::encode_packet(&self.tc_codec, seq, &[], &values).expect(
                            "GroundStationModel's declared tc_codec always carries the \"seq\" UINT32 field this call supplies, pre-validated at construction (validate_codec) -- encode_packet can only fail for a missing/mistyped/out-of-range field, none of which can happen here",
                        );
                        outbox.push(self.tc_port.clone(), end, payload);
                    } else if !now_visible && was_visible {
                        self.in_contact.set(false);
                        applied.push(AppliedCommand { port: CONTACT_TRANSITION_PORT.to_string(), field: String::new(), value: CONTACT_END_VALUE, applied_tai_ns: end });
                    }
                }
            }
        }
        outputs.insert("in_contact".to_string(), if self.in_contact.get() { 1.0 } else { 0.0 });
        Ok((StepResult { state: Vec::new(), t_tai_ns: end, outputs }, outbox, applied))
    }

    // Neither `tm_codec` nor `tc_codec` declares a `PacketField.target` (both built above with
    // `target: String::new()`) -- this model decodes telemetry for its own elevation/contact
    // logic only, never through `crate::codec::measurements_from_field_values`, so it never
    // produces a CDM measurement.
    fn last_measurements(&self) -> Vec<av_cdm::pb::Measurement> {
        Vec::new()
    }
    // No SENSOR fault runtime.
    fn drain_sensor_fault_effect(&self) -> Option<av_dynamics::SensorFaultEffectDrain> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ----------------------------------------------------------------------------------
    // Geometry: overhead / horizon / behind-earth sanity checks.
    // ----------------------------------------------------------------------------------

    /// The ellipsoidal surface-normal "up" direction at a geodetic latitude/longitude --
    /// `[cos(lat)cos(lon), cos(lat)sin(lon), sin(lat)]`, exactly the `up` row [`ecef_to_topocentric`]
    /// itself uses. **Not** the geocentric radial direction (`site_ecef / |site_ecef|`) except at
    /// the equator or a pole -- WGS84's flattening means "straight up" (the direction height_m is
    /// measured along) and "away from Earth's center" differ by the deflection of the vertical
    /// (about 0.16 degrees at 28.5 degrees latitude, enough to fail a tight elevation-angle
    /// assertion) -- this is exactly the bug the overhead test below caught on first write when it
    /// used the geocentric direction instead: 89.839 degrees measured, not 90.
    fn ellipsoidal_up_unit(lat_rad: f64, lon_rad: f64) -> [f64; 3] {
        [lat_rad.cos() * lon_rad.cos(), lat_rad.cos() * lon_rad.sin(), lat_rad.sin()]
    }

    /// A target directly above the site (same lat/lon, greater height, along the ellipsoidal
    /// surface normal -- [`ellipsoidal_up_unit`]) is at 90 degrees elevation -- fails against a
    /// transposed or mis-signed rotation matrix, which would report some other angle for the one
    /// case with an unambiguous right answer.
    #[test]
    fn a_target_directly_overhead_is_at_ninety_degrees_elevation() {
        let site = GroundStationSpec { body: "Earth".to_string(), latitude_rad: 28.5_f64.to_radians(), longitude_rad: (-80.6_f64).to_radians(), height_m: 0.0, elevation_mask_rad: 0.0 };
        let site_ecef = geodetic_to_ecef_m(site.latitude_rad, site.longitude_rad, site.height_m);
        let up_unit = ellipsoidal_up_unit(site.latitude_rad, site.longitude_rad);
        let target = [site_ecef[0] + 500_000.0 * up_unit[0], site_ecef[1] + 500_000.0 * up_unit[1], site_ecef[2] + 500_000.0 * up_unit[2]];
        let el = elevation_of(&site, target);
        assert!((el - std::f64::consts::FRAC_PI_2).abs() < 1e-9, "expected 90 deg, got {} deg", el.to_degrees());
    }

    /// A target on the site's own local horizontal plane (east of the site, same up-component)
    /// is at 0 degrees elevation -- fails against a formula that omits the `up` component or
    /// mis-scales the horizontal magnitude.
    #[test]
    fn a_target_on_the_local_horizontal_plane_is_at_zero_degrees_elevation() {
        let site = GroundStationSpec { body: "Earth".to_string(), latitude_rad: 0.0, longitude_rad: 0.0, height_m: 0.0, elevation_mask_rad: 0.0 };
        let site_ecef = geodetic_to_ecef_m(site.latitude_rad, site.longitude_rad, site.height_m);
        // At the equator/prime-meridian, local east is +y, north is +z, up is +x: a target
        // purely offset in +y is exactly on the local horizontal.
        let target = [site_ecef[0], site_ecef[1] + 100_000.0, site_ecef[2]];
        let el = elevation_of(&site, target);
        assert!(el.abs() < 1e-9, "expected 0 deg, got {} deg", el.to_degrees());
    }

    // ----------------------------------------------------------------------------------
    // contact_windows: pure function.
    // ----------------------------------------------------------------------------------

    /// A simple rise-and-set pass produces exactly one window, with linearly interpolated
    /// endpoints -- fails against an implementation that snaps to the nearest sample instead of
    /// interpolating (would report exactly `0`/`40` here, not the interpolated `5`/`35`).
    #[test]
    fn contact_windows_interpolates_a_single_rise_and_set_pass() {
        // Elevation rises linearly from -10 to +10 over [0,10], holds, falls linearly from +10
        // to -10 over [30,40]. Mask = 0.
        let samples = vec![(0i64, -10.0f64.to_radians()), (10, 10.0f64.to_radians()), (20, 10.0f64.to_radians()), (30, 10.0f64.to_radians()), (40, -10.0f64.to_radians())];
        let windows = contact_windows(0.0, &samples);
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].start_tai_ns, 5);
        assert_eq!(windows[0].end_tai_ns, 35);
    }

    /// An arc that starts already above the mask opens a window at the first sample (no earlier
    /// crossing exists to interpolate) -- fails against an implementation that requires a rising
    /// edge to ever open a window at all.
    #[test]
    fn contact_windows_open_at_the_first_sample_when_already_above_mask() {
        let samples = vec![(0i64, 10.0f64.to_radians()), (10, 10.0f64.to_radians()), (20, -10.0f64.to_radians())];
        let windows = contact_windows(0.0, &samples);
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].start_tai_ns, 0);
    }

    /// No crossing at all (always below mask) produces zero windows.
    #[test]
    fn contact_windows_reports_nothing_when_always_below_mask() {
        let samples = vec![(0i64, -10.0f64.to_radians()), (10, -5.0f64.to_radians()), (20, -1.0f64.to_radians())];
        assert!(contact_windows(0.0, &samples).is_empty());
    }

    // ----------------------------------------------------------------------------------
    // parse_ground_station_spec.
    // ----------------------------------------------------------------------------------

    fn valid_params() -> BTreeMap<String, Parameter> {
        let mut p = BTreeMap::new();
        p.insert("station.body".to_string(), Parameter { name: "station.body".to_string(), string_value: "Earth".to_string(), ..Default::default() });
        p.insert("station.latitude_rad".to_string(), Parameter { name: "station.latitude_rad".to_string(), value: 0.5, ..Default::default() });
        p.insert("station.longitude_rad".to_string(), Parameter { name: "station.longitude_rad".to_string(), value: -1.4, ..Default::default() });
        p.insert("station.height_m".to_string(), Parameter { name: "station.height_m".to_string(), value: 10.0, ..Default::default() });
        p.insert("station.elevation_mask_rad".to_string(), Parameter { name: "station.elevation_mask_rad".to_string(), value: 0.1745, ..Default::default() });
        p
    }

    #[test]
    fn parse_ground_station_spec_accepts_a_valid_set() {
        let spec = parse_ground_station_spec(&valid_params()).expect("valid");
        assert_eq!(spec.body, "Earth");
        assert!((spec.elevation_mask_rad - 0.1745).abs() < 1e-12);
    }

    #[test]
    fn parse_ground_station_spec_refuses_an_unknown_parameter() {
        let mut p = valid_params();
        p.insert("station.bogus".to_string(), Parameter { name: "station.bogus".to_string(), value: 1.0, ..Default::default() });
        match parse_ground_station_spec(&p) {
            Err(GroundSpecError::UnknownParameter { name }) => assert_eq!(name, "station.bogus"),
            other => panic!("expected UnknownParameter, got {other:?}"),
        }
    }

    #[test]
    fn parse_ground_station_spec_refuses_an_out_of_range_elevation_mask() {
        let mut p = valid_params();
        p.insert("station.elevation_mask_rad".to_string(), Parameter { name: "station.elevation_mask_rad".to_string(), value: 2.0, ..Default::default() });
        match parse_ground_station_spec(&p) {
            Err(GroundSpecError::InvalidParameter { name, .. }) => assert_eq!(name, "station.elevation_mask_rad"),
            other => panic!("expected InvalidParameter, got {other:?}"),
        }
    }

    // ----------------------------------------------------------------------------------
    // GroundStationModel::step_with_ports.
    // ----------------------------------------------------------------------------------

    fn test_model() -> GroundStationModel {
        let spec = GroundStationSpec { body: "Earth".to_string(), latitude_rad: 28.5_f64.to_radians(), longitude_rad: (-80.6_f64).to_radians(), height_m: 0.0, elevation_mask_rad: 10.0_f64.to_radians() };
        let tm_codec = ground_tm_packet_codec("tm_test", 400);
        let tc_codec = ground_tc_packet_codec("tc_test", 401);
        GroundStationModel::new(spec, tm_codec, "tm_in".to_string(), tc_codec, "tc_out".to_string(), "ground.test").expect("valid construction")
    }

    fn tm_message(model: &GroundStationModel, target_ecef_m: [f64; 3], tai_ns: i64) -> av_dynamics::PortMessage {
        let mut values = BTreeMap::new();
        values.insert("x".to_string(), FieldValue::Numeric(target_ecef_m[0]));
        values.insert("y".to_string(), FieldValue::Numeric(target_ecef_m[1]));
        values.insert("z".to_string(), FieldValue::Numeric(target_ecef_m[2]));
        let payload = codec::encode_packet(&model.tm_codec, 0, &[], &values).unwrap();
        av_dynamics::PortMessage { port: "tm_in".to_string(), tai_ns, payload }
    }

    /// A target overhead (well above the mask) on the first step reports `EVENT`-worthy contact:
    /// `in_contact` becomes true and exactly one `AppliedCommand` on `CONTACT_TRANSITION_PORT`
    /// with `CONTACT_START_VALUE` is returned, plus one encoded packet on `tc_out` -- fails
    /// against an implementation that never reports the rising edge, or that emits the ack packet
    /// unconditionally regardless of visibility.
    #[test]
    fn step_with_ports_reports_contact_start_and_sends_an_ack_when_a_target_rises_above_the_mask() {
        let model = test_model();
        let site_ecef = geodetic_to_ecef_m(model.spec.latitude_rad, model.spec.longitude_rad, model.spec.height_m);
        let up_unit = ellipsoidal_up_unit(model.spec.latitude_rad, model.spec.longitude_rad);
        let overhead = [site_ecef[0] + 500_000.0 * up_unit[0], site_ecef[1] + 500_000.0 * up_unit[1], site_ecef[2] + 500_000.0 * up_unit[2]];
        let msg = tm_message(&model, overhead, 0);
        let inbox = Inbox::new(vec![msg]);
        let (result, outbox, applied) = model.step_with_ports(&[], 0, &[], 1_000_000_000, &inbox).unwrap();
        assert_eq!(applied.len(), 1);
        assert_eq!(applied[0].port, CONTACT_TRANSITION_PORT);
        assert_eq!(applied[0].value, CONTACT_START_VALUE);
        assert_eq!(outbox.messages().len(), 1);
        assert_eq!(outbox.messages()[0].port, "tc_out");
        assert_eq!(result.outputs.get("in_contact"), Some(&1.0));
    }

    /// A target below the horizon never triggers a transition and never sends an ack -- fails
    /// against an implementation that emits on every step regardless of the elevation mask.
    #[test]
    fn step_with_ports_reports_no_contact_when_a_target_is_below_the_mask() {
        let model = test_model();
        let site_ecef = geodetic_to_ecef_m(model.spec.latitude_rad, model.spec.longitude_rad, model.spec.height_m);
        // A target on the opposite side of the Earth: well below the local horizon.
        let far_side = [-site_ecef[0] * 1.1, -site_ecef[1] * 1.1, -site_ecef[2] * 1.1];
        let msg = tm_message(&model, far_side, 0);
        let inbox = Inbox::new(vec![msg]);
        let (result, outbox, applied) = model.step_with_ports(&[], 0, &[], 1_000_000_000, &inbox).unwrap();
        assert!(applied.is_empty());
        assert!(outbox.messages().is_empty());
        assert_eq!(result.outputs.get("in_contact"), Some(&0.0));
    }

    /// A rise followed by a set (two steps) reports both `CONTACT_START_VALUE` and
    /// `CONTACT_END_VALUE`, in order, and sends exactly one ack (only on the rising edge) --
    /// fails against an implementation that either double-reports a steady-state contact every
    /// step or never reports the falling edge.
    #[test]
    fn step_with_ports_reports_contact_end_on_the_following_step_once_the_target_sets() {
        let model = test_model();
        let site_ecef = geodetic_to_ecef_m(model.spec.latitude_rad, model.spec.longitude_rad, model.spec.height_m);
        let up_unit = ellipsoidal_up_unit(model.spec.latitude_rad, model.spec.longitude_rad);
        let overhead = [site_ecef[0] + 500_000.0 * up_unit[0], site_ecef[1] + 500_000.0 * up_unit[1], site_ecef[2] + 500_000.0 * up_unit[2]];
        let far_side = [-site_ecef[0] * 1.1, -site_ecef[1] * 1.1, -site_ecef[2] * 1.1];

        let inbox1 = Inbox::new(vec![tm_message(&model, overhead, 0)]);
        let (_r1, _o1, applied1) = model.step_with_ports(&[], 0, &[], 1_000_000_000, &inbox1).unwrap();
        assert_eq!(applied1.len(), 1);
        assert_eq!(applied1[0].value, CONTACT_START_VALUE);

        let inbox2 = Inbox::new(vec![tm_message(&model, far_side, 1_000_000_000)]);
        let (_r2, o2, applied2) = model.step_with_ports(&[], 1_000_000_000, &[], 1_000_000_000, &inbox2).unwrap();
        assert_eq!(applied2.len(), 1);
        assert_eq!(applied2[0].value, CONTACT_END_VALUE);
        assert!(o2.messages().is_empty(), "no ack on a falling edge");
    }

    /// `state_dim() == 0` and `stm_capable() == false` -- the two "decide and state" facts this
    /// module's own doc comment commits to (no propagated state, no covariance capability).
    #[test]
    fn ground_station_model_declares_zero_state_dim_and_no_stm_capability() {
        let model = test_model();
        assert_eq!(model.state_dim(), 0);
        assert!(!model.stm_capable());
    }
}
