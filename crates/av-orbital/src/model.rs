//! [`EarthGravityModel`]: the `av_dynamics::DynamicsModel` binding for a single spacecraft
//! under Earth point-mass/spherical-harmonic gravity (`docs/native-dynamics-plan.md`
//! milestone N1, ADR-002 depth 3).
//!
//! **Generic over the rotation, not over GMAT** (charter decision 222(c)): this type is
//! `EarthGravityModel<R: crate::frame::BodyFixedRotation>`, never tied to
//! [`crate::frame_gmat::GmatBodyFixedRotation`] by name, so N5's native rotation is a drop-in
//! `R` with no change to this module -- see `crate::frame`'s module doc.
//!
//! # Units (matched to `gmat_sys::model::GmatModel`, established by reading that module)
//!
//! `crates/gmat-sys/src/model.rs`'s own module doc states the boundary explicitly: "Everything
//! above `GmatModel` ... works in SI metres, metres per second and TAI nanoseconds", and
//! `GmatModel::derivatives`'s body confirms it in code -- the caller's `state`/`state_dot`
//! slices are SI (`units::state_m_to_km`/`state_km_to_m` convert to/from GMAT's own km
//! immediately at the call boundary, nowhere else). This model matches that exactly: `state`/
//! `state_dot` here are SI metres, metres/second, metres/second^2 throughout, with NO unit
//! conversion at all (unlike `GmatModel`, which must cross the km<->m boundary because GMAT's
//! own propagators are km-native; this model's own numerical core, `crate::cof`/`crate::
//! gravity`, is SI-native by construction -- see `crate::cof`'s own module doc: "GMAT's own
//! `.cof` files are *themselves* already stored in SI"). `tests/units_match_gmat_model.rs`
//! pins this: a state vector run through both `GmatModel::derivatives` (6-state, point mass
//! only, so the two models' physics agree) and this model's `derivatives` must agree on what a
//! `[7e6, 0, 0, 0, 7.5e3, 0]`-shaped state (SI position/velocity magnitudes for a LEO orbit,
//! not km) means -- see that test for the exact comparison, and this task's own report for the
//! measured agreement. `state_space_id` is likewise matched: `"gmat.orbital.cartesian6"`,
//! [`GmatModel`]'s own state-space id, so the native and GMAT-backed models are interchangeable
//! per DRM (charter 222(a)) at exactly the state-space and unit level a DRM cares about.
//!
//! # Frame: no rotation of the state itself
//!
//! `state`'s position and velocity are BOTH always inertial (the frame named in `describe()
//! .frame_id`, `"{central_body}MJ2000Eq"` -- the same naming `GmatModel`'s own `frame_id`
//! uses for its integration frame). Only the FORCE EVALUATION crosses into the body-fixed
//! frame, once per `derivatives` call: rotate the inertial position into body-fixed
//! (`R * pos`), evaluate `crate::gravity::spherical_harmonic_gravity` there, rotate the
//! resulting acceleration back to inertial (`R^T * accel`, exact -- see
//! `crate::frame::Rotation::apply_transpose`'s own doc comment for why the transpose is exact
//! rather than an approximation). `R`'s own time derivative (`Rotation::r_dot`) is never used
//! here: nothing in this model differentiates a quantity IN the rotating frame (which is where
//! a Coriolis/centrifugal term would come from) -- the state's own dynamics (`d(pos)/dt =
//! vel`, `d(vel)/dt = accel`) are expressed entirely in the inertial frame throughout, and the
//! body-fixed frame exists only as a coordinate change the force law is evaluated in at each
//! instant, not a frame the state itself is ever expressed in.
//!
//! # What this model does NOT do (by design, per this task's own brief)
//!
//! No state transition matrix (N4): [`EarthGravityModel::stm_capable`] is the trait's own
//! `false` default, left un-overridden, and [`EarthGravityModel::describe`] never advertises
//! `MODEL_CAPABILITY_STM` -- both honestly declare the capability absent, mirroring
//! `gmat_sys::model::GmatModel`'s identical pattern for a model built without the STM.
use std::collections::BTreeMap;
use std::path::Path;

use av_cdm::pb::{ModelCapability, ModelInfo};
use av_dynamics::{integrate::Dopri5, DecodeErrorOccurrence, DynamicsModel, SensorFaultEffectDrain};

use crate::cof::{self, CofError, GravityModel};
use crate::frame::BodyFixedRotation;
use crate::gravity;

/// Every way [`EarthGravityModel::new`]/[`EarthGravityModel::derivatives`] can fail. Typed
/// throughout -- no panic on any input a DRM could supply (this task's own rule).
///
/// Generic over `E` (the bound `R: BodyFixedRotation`'s own `Error` type) for the identical
/// reason `av_dynamics::DynamicsModel::Error` is unconstrained rather than fixed to one
/// concrete type: [`crate::frame_gmat::GmatBodyFixedRotation`]'s `gmat_sys::GmatError` and an
/// eventual N5 native rotation's own domain error must both be usable here without either
/// needing a conversion to the other.
#[derive(Debug, thiserror::Error)]
pub enum OrbitalModelError<E: std::fmt::Debug + std::fmt::Display> {
    /// The gravity file could not be read (for the SHA-256 that goes into `settings_hash` --
    /// [`EarthGravityModel::new`] reads the file's own bytes independently of
    /// [`cof::read_earth_gravity`], which does not itself return them).
    #[error("could not read gravity file {path} to compute its settings-hash digest: {source}")]
    GravityFileIo {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// [`cof::read_earth_gravity`] itself failed (a missing file, a malformed record, a
    /// degree/order the file does not contain -- see [`CofError`]'s own variants).
    #[error("gravity file could not be parsed: {0}")]
    Gravity(#[source] CofError),
    /// `derivatives`/`stm_derivatives` was called with a state (or output) slice that is not
    /// exactly 6 elements -- this model's `state_dim()` is always 6, so any other length is a
    /// caller bug reported as a typed error, never a panic or an out-of-bounds index.
    #[error("EarthGravityModel is a 6-state model; got a slice of length {0}")]
    BadStateLen(usize),
    /// The `R: BodyFixedRotation` this model was built with failed to produce a rotation for
    /// the requested epoch (e.g. `gmat_sys::GmatError` for an uninitialised or unregistered
    /// coordinate system, propagated from [`crate::frame_gmat::GmatBodyFixedRotation`]).
    #[error("body-fixed rotation failed: {0}")]
    Rotation(E),
}

/// Caller-supplied, static description fields for an [`EarthGravityModel`], independent of the
/// bound gravity file/rotation -- mirrors `gmat_sys::model::GmatModelInfo`'s identical shape
/// and identical reasoning (this type has no way to recover a human-meaningful id/version/
/// golden list from the gravity file or rotation alone, so the caller that knows what it built
/// states them directly).
#[derive(Debug, Clone)]
pub struct EarthGravityModelInfo {
    /// Stable id, e.g. `"native.orbital.earth_point_mass"`, `"native.orbital.earth_jgm2_8x8"`
    /// (`ModelInfo.id`; the proto's own doc comment names `"native.orbital.two_body_j2"` as an
    /// example of this exact convention).
    pub id: String,
    /// This model's own version label (this crate has no GMAT version to report --
    /// `env!("CARGO_PKG_VERSION")` of `av-orbital` is the natural choice for a caller that has
    /// no more specific label of its own).
    pub version: String,
    /// Names of the goldens (under `goldens/`) this exact configuration is pinned against.
    pub goldens: Vec<String>,
}

/// A [`DynamicsModel`] for one spacecraft under Earth point-mass/spherical-harmonic gravity,
/// generic over its own body-fixed rotation source `R` -- see this module's own doc comment.
pub struct EarthGravityModel<R: BodyFixedRotation> {
    gravity: GravityModel,
    rotation: R,
    frame_id: String,
    info: EarthGravityModelInfo,
    settings_hash: String,
}

fn hex_sha256(bytes: &[u8]) -> String {
    let digest = openssl::sha::sha256(bytes);
    let mut s = String::with_capacity(64);
    for b in digest {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

impl<R: BodyFixedRotation> EarthGravityModel<R> {
    /// Reads `gravity_file_path` (a GMAT `.cof` file, via [`cof::read_earth_gravity`],
    /// truncated to `max_degree`/`max_order`) and builds `describe().settings_hash` (via
    /// [`av_dynamics::settings_hash`]) over the gravity file's own name and SHA-256 digest,
    /// `max_degree`/`max_order`, `mu`, the reference radius, `central_body`, the resulting
    /// inertial `frame_id`, and `rotation`'s own integrator settings (`Dopri5::default()` --
    /// see [`EarthGravityModel::integrator`]'s own doc comment for why this model never
    /// deviates from it) -- so `settings_hash` is a pure function of everything that actually
    /// determines this model's physics, exactly `gmat_sys::model::GmatModel::new`'s own stated
    /// goal for its analogous hash.
    ///
    /// `central_body` sets `describe().frame_id` to `"{central_body}MJ2000Eq"` -- the caller
    /// is responsible for constructing `rotation` against the SAME body (this type has no way
    /// to verify that on its own; see [`crate::frame_gmat::GmatBodyFixedRotation::new`]'s own
    /// `central_body` parameter, which the caller should pass the identical string to).
    pub fn new(
        gravity_file_path: &Path,
        max_degree: usize,
        max_order: usize,
        central_body: &str,
        rotation: R,
        info: EarthGravityModelInfo,
    ) -> Result<Self, OrbitalModelError<R::Error>> {
        let bytes = std::fs::read(gravity_file_path)
            .map_err(|source| OrbitalModelError::GravityFileIo { path: gravity_file_path.to_path_buf(), source })?;
        let file_sha256 = hex_sha256(&bytes);
        let gravity = cof::read_earth_gravity(gravity_file_path, max_degree, max_order).map_err(OrbitalModelError::Gravity)?;
        let file_name = gravity_file_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| gravity_file_path.display().to_string());
        let frame_id = format!("{central_body}MJ2000Eq");
        let integrator = Dopri5::default();

        let mut settings = BTreeMap::new();
        settings.insert("gravity_file_name".to_string(), file_name);
        settings.insert("gravity_file_sha256".to_string(), file_sha256);
        settings.insert("degree".to_string(), max_degree.to_string());
        settings.insert("order".to_string(), max_order.to_string());
        settings.insert("mu_m3_per_s2".to_string(), format!("{:.17e}", gravity.mu()));
        settings.insert("reference_radius_m".to_string(), format!("{:.17e}", gravity.reference_radius()));
        settings.insert("central_body".to_string(), central_body.to_string());
        settings.insert("frame_id".to_string(), frame_id.clone());
        settings.insert("integrator_rtol".to_string(), format!("{:.17e}", integrator.rtol));
        settings.insert("integrator_atol".to_string(), format!("{:.17e}", integrator.atol));
        settings.insert("integrator_initial_step_s".to_string(), format!("{:.17e}", integrator.initial_step));
        settings.insert("integrator_max_step_s".to_string(), format!("{:.17e}", integrator.max_step));
        let settings_hash = av_dynamics::settings_hash(&settings);

        Ok(Self { gravity, rotation, frame_id, info, settings_hash })
    }

    /// The bound gravity model (degree, order, `mu`, reference radius, coefficients) -- for a
    /// caller (or test) that wants to inspect what this model actually loaded.
    pub fn gravity_model(&self) -> &GravityModel {
        &self.gravity
    }
}

impl<R: BodyFixedRotation> DynamicsModel for EarthGravityModel<R> {
    type Error = OrbitalModelError<R::Error>;

    fn state_dim(&self) -> usize {
        6
    }

    /// `[d(pos)/dt; d(vel)/dt] = [vel; accel]`, `accel` from `crate::gravity::
    /// spherical_harmonic_gravity` evaluated in the body-fixed frame and rotated back -- see
    /// this module's own doc comment ("Frame: no rotation of the state itself") for the exact
    /// steps and why only ONE call to `rotation.inertial_to_fixed` is needed. `d(pos)/dt ==
    /// vel` EXACTLY (a plain copy, never derived from anything gravity-model-dependent) --
    /// `tests/units_match_gmat_model.rs`'s `derivatives_first_three_components_equal_velocity_
    /// exactly` pins this, matching ADR-002's first amendment's recorded layout fact for
    /// `GmatModel` ("derivative of position equals velocity").
    fn derivatives(&self, state: &[f64], t_tai_ns: i64, _controls: &[f64], state_dot: &mut [f64]) -> Result<(), Self::Error> {
        if state.len() != 6 {
            return Err(OrbitalModelError::BadStateLen(state.len()));
        }
        if state_dot.len() != 6 {
            return Err(OrbitalModelError::BadStateLen(state_dot.len()));
        }
        let pos_inertial = [state[0], state[1], state[2]];
        let vel_inertial = [state[3], state[4], state[5]];

        let rotation = self.rotation.inertial_to_fixed(t_tai_ns).map_err(OrbitalModelError::Rotation)?;
        let pos_fixed = rotation.apply(pos_inertial);
        let (accel_fixed, _partials_fixed) = gravity::spherical_harmonic_gravity(pos_fixed, &self.gravity);
        let accel_inertial = rotation.apply_transpose(accel_fixed);

        state_dot[0..3].copy_from_slice(&vel_inertial);
        state_dot[3..6].copy_from_slice(&accel_inertial);
        Ok(())
    }

    /// `av_cdm::pb::ModelInfo`, filled the way `gmat_sys::model::GmatModel::describe` fills it
    /// (see this module's own "Units" doc section for how `state_space_id` was matched) --
    /// `depth` is `"native"` (the proto's own doc comment: `"gmat-api"`, `"gmat-ffi"`,
    /// `"native"`, or an external engine name; `GmatModel`'s own depth is `"gmat-ffi"`).
    /// `capabilities` never includes `MODEL_CAPABILITY_STM` (see this module's own "What this
    /// model does NOT do" doc section).
    fn describe(&self) -> ModelInfo {
        ModelInfo {
            id: self.info.id.clone(),
            version: self.info.version.clone(),
            state_space_id: "gmat.orbital.cartesian6".to_string(),
            frame_id: self.frame_id.clone(),
            controls: vec![],
            capabilities: vec![
                ModelCapability::Derivatives as i32,
                ModelCapability::Step as i32,
                ModelCapability::Deterministic as i32,
            ],
            depth: "native".to_string(),
            settings_hash: self.settings_hash.clone(),
            goldens: self.info.goldens.clone(),
        }
    }

    // `integrator()` is left at the trait's own default (`Dopri5::default()`, `rtol = atol =
    // 1e-12`, `initial_step = 30 s`, `max_step = 600 s`) -- this task's own brief: "start from
    // Dopri5::default() and only deviate with a measured reason." No measurement this task ran
    // (the two-body golden's own residual, see `tests/twobody_golden.rs`, and the energy/
    // angular-momentum drift both measured within this default) gave a reason to deviate; see
    // this task's own report for the exact numbers.

    // This model never emits telemetry mapped to a CDM measurement (mirrors
    // `gmat_sys::model::GmatModel::last_measurements`'s identical, identically-reasoned
    // override).
    fn last_measurements(&self) -> Vec<av_cdm::pb::Measurement> {
        Vec::new()
    }

    // No SENSOR fault runtime (mirrors `GmatModel::drain_sensor_fault_effect`'s identical
    // override).
    fn drain_sensor_fault_effect(&self) -> Option<SensorFaultEffectDrain> {
        None
    }

    // Never decodes a FRAMED frame (mirrors `GmatModel::drain_decode_errors`'s identical
    // override).
    fn drain_decode_errors(&self) -> Vec<DecodeErrorOccurrence> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::Rotation;

    /// A no-op rotation (`r` = identity, `r_dot` = zero) -- a physically valid, if
    /// uninteresting, `BodyFixedRotation` for any test that does not care which body-fixed
    /// frame is used (e.g. anything under a point-mass or purely-zonal field, both of which
    /// are invariant to the choice, so an identity rotation introduces no error at all -- see
    /// `crate::frame`'s own module doc on why a transpose bug would NOT show up under such a
    /// field, which is exactly why this mock is confined to tests that do not need to catch
    /// one; `tests/frame_gmat.rs`'s asymmetric-field test is what actually proves the
    /// direction, against the real GMAT-backed rotation).
    pub(crate) struct IdentityRotation;
    impl BodyFixedRotation for IdentityRotation {
        type Error = std::convert::Infallible;
        fn inertial_to_fixed(&self, _t_tai_ns: i64) -> Result<Rotation, Self::Error> {
            Ok(Rotation { r: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]], r_dot: [[0.0; 3]; 3] })
        }
    }

    fn jgm2_path() -> std::path::PathBuf {
        cof::locate_gmat_root().expect("GMAT_ROOT set (this task's own environment rule)").join("data/gravity/earth/JGM2.cof")
    }

    fn point_mass_model() -> EarthGravityModel<IdentityRotation> {
        EarthGravityModel::new(
            &jgm2_path(),
            0,
            0,
            "Earth",
            IdentityRotation,
            EarthGravityModelInfo { id: "native.orbital.test_point_mass".to_string(), version: "test".to_string(), goldens: vec![] },
        )
        .expect("point-mass model construction")
    }

    #[test]
    fn state_dim_is_six() {
        assert_eq!(point_mass_model().state_dim(), 6);
    }

    #[test]
    fn derivatives_first_three_components_equal_velocity_exactly() {
        let model = point_mass_model();
        let state = [7_000_000.0, 500_000.0, -200_000.0, -1_200.0, 7_400.0, 300.0];
        let mut dot = [0.0; 6];
        model.derivatives(&state, 1_800_000_000_000_000_000, &[], &mut dot).unwrap();
        assert_eq!(dot[0..3], state[3..6], "d(pos)/dt must equal velocity EXACTLY (ADR-002 first amendment's layout fact)");
    }

    #[test]
    fn degree_zero_matches_the_closed_form_point_mass_acceleration_through_the_full_model() {
        let model = point_mass_model();
        let pos = [7_000_000.0, 500_000.0, -200_000.0];
        let state = [pos[0], pos[1], pos[2], -1_200.0, 7_400.0, 300.0];
        let mut dot = [0.0; 6];
        model.derivatives(&state, 1_800_000_000_000_000_000, &[], &mut dot).unwrap();
        let expect = gravity::point_mass_acceleration(pos, model.gravity_model().mu());
        for i in 0..3 {
            assert!((dot[3 + i] - expect[i]).abs() < 1e-15 * expect[i].abs().max(1.0), "component {i}: {} vs {}", dot[3 + i], expect[i]);
        }
    }

    #[test]
    fn describe_reports_native_depth_gmat_orbital_state_space_and_no_stm_capability() {
        let model = point_mass_model();
        let info = model.describe();
        assert_eq!(info.depth, "native");
        assert_eq!(info.state_space_id, "gmat.orbital.cartesian6");
        assert_eq!(info.frame_id, "EarthMJ2000Eq");
        assert_eq!(info.settings_hash.len(), 64, "hex-encoded SHA-256 is 64 chars");
        assert!(!info.capabilities.contains(&(ModelCapability::Stm as i32)), "N4 (the STM) is out of scope for this model; describe() must not advertise it");
        assert!(!model.stm_capable(), "stm_capable() must honestly report false (the trait's own un-overridden default)");
    }

    #[test]
    fn a_wrong_length_state_is_a_typed_error_not_a_panic() {
        let model = point_mass_model();
        let mut dot = [0.0; 6];
        let err = model.derivatives(&[0.0; 5], 0, &[], &mut dot).unwrap_err();
        assert!(matches!(err, OrbitalModelError::BadStateLen(5)));
    }

    #[test]
    fn settings_hash_changes_when_degree_changes() {
        let model0 = point_mass_model();
        let model2 = EarthGravityModel::new(
            &jgm2_path(),
            2,
            2,
            "Earth",
            IdentityRotation,
            EarthGravityModelInfo { id: "native.orbital.test_2x2".to_string(), version: "test".to_string(), goldens: vec![] },
        )
        .unwrap();
        assert_ne!(model0.describe().settings_hash, model2.describe().settings_hash);
    }
}
