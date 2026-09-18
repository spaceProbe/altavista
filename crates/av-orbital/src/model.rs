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
//! # The state transition matrix (N4)
//!
//! Through N3, this module's own doc stated the STM absent by design; N4 (`docs/
//! native-dynamics-plan.md`) is exactly the task that changes that. [`EarthGravityModel::
//! stm_capable`] now always returns `true` (this model is always fully configured to deliver
//! an STM -- unlike `gmat_sys::model::GmatModel`, which withholds the capability for a force
//! model that includes GMAT's `RelativisticCorrection` stub or a penumbra arc, ADR-002's third
//! amendment, this model has no such gap: every force it can be configured with fills its own
//! A-matrix block honestly, analytically or by a measured finite difference -- see
//! [`EarthGravityModel::acceleration_partials`]'s own doc comment for exactly which), and
//! [`EarthGravityModel::describe`] advertises `ModelCapability::Stm` accordingly.
//! [`EarthGravityModel::stm_derivatives`] fills `d(Phi)/dt = A(t) Phi` (row-major, index
//! `6 + row*6 + col`, ADR-002's second amendment) via `crate::stm::stm_rate`; the trait's own
//! default `step_with_stm` (seeding `Phi(t0,t0) = I`, the exact identity) is not overridden --
//! this model has no named side output `step_with_stm` would need to add, unlike
//! `GmatModel::step_with_stm`'s `rmag`/`Cd` reads.
use std::collections::BTreeMap;
use std::path::Path;

use av_cdm::pb::{ModelCapability, ModelInfo};
use av_dynamics::{integrate::Dopri5, DecodeErrorOccurrence, DynamicsModel, SensorFaultEffectDrain};

use crate::cof::{self, CofError, GravityModel};
use crate::de::{DeBody, DeEphemeris, DeError};
use crate::dual::{Dual3, GravScalar};
use crate::frame::BodyFixedRotation;
use crate::gravity;
use crate::stm::{rotate_gradient_body_to_inertial, stm_rate};
use crate::third_body::{third_body_acceleration, third_body_partials};

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
    /// `derivatives` was called with a state (or output) slice that is not exactly 6 elements
    /// -- this model's `state_dim()` is always 6, so any other length is a caller bug reported
    /// as a typed error, never a panic or an out-of-bounds index.
    #[error("EarthGravityModel is a 6-state model; got a slice of length {0}")]
    BadStateLen(usize),
    /// N4: `stm_derivatives`/`stm_capable`'s own augmented state (or output) slice was not
    /// exactly 42 elements (`state_dim() + state_dim()^2 = 6 + 36`) -- mirrors
    /// [`OrbitalModelError::BadStateLen`]'s identical reasoning for the plain 6-state case.
    #[error("EarthGravityModel's STM-augmented state is 42 elements (6 + 6x6); got a slice of length {0}")]
    BadAugmentedStateLen(usize),
    /// The `R: BodyFixedRotation` this model was built with failed to produce a rotation for
    /// the requested epoch (e.g. `gmat_sys::GmatError` for an uninitialised or unregistered
    /// coordinate system, propagated from [`crate::frame_gmat::GmatBodyFixedRotation`]).
    #[error("body-fixed rotation failed: {0}")]
    Rotation(E),
    /// The DE ephemeris file could not be read (for the SHA-256 that goes into
    /// `settings_hash`, mirroring [`OrbitalModelError::GravityFileIo`]) -- N2's third-body
    /// support, [`EarthGravityModel::with_third_bodies`].
    #[error("could not read DE ephemeris file {path} to compute its settings-hash digest: {source}")]
    DeFileIo {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// A [`crate::de::DeEphemeris`] lookup failed (a malformed file, an out-of-range epoch, or
    /// an unknown `mu` constant -- see [`DeError`]'s own variants) -- either while building a
    /// third-body configuration or while evaluating `derivatives` at an epoch the bound DE
    /// file does not cover.
    #[error("third-body ephemeris lookup failed: {0}")]
    De(#[source] DeError),
    /// [`EarthGravityModel::with_srp`] was called before [`EarthGravityModel::
    /// with_third_bodies`] -- N3's SRP binding reuses that method's own bound `DeEphemeris`
    /// handle for the Sun's position (see [`EarthGravityModel::with_srp`]'s own doc comment for
    /// why), so it requires third bodies to already be configured.
    #[error("with_srp requires with_third_bodies to be configured first (the Sun's position comes from that bound DE ephemeris)")]
    SrpRequiresThirdBodies,
    /// N3's [`crate::srp::srp_acceleration`] failed (a non-finite state, or a degenerate
    /// zero-distance geometry -- see [`crate::srp::SrpError`]'s own variants).
    #[error("SRP acceleration failed: {0}")]
    Srp(#[source] crate::srp::SrpError),
    /// [`EarthGravityModel::with_drag`] was called before [`EarthGravityModel::
    /// with_third_bodies`] -- task 3b's drag binding reuses that method's own bound
    /// `DeEphemeris` handle for the Sun's position (the diurnal exospheric-temperature bulge
    /// `crate::jacchia_roberts::exotherm` needs), mirroring [`EarthGravityModel::with_srp`]'s
    /// own identical requirement and identical reasoning.
    #[error("with_drag requires with_third_bodies to be configured first (the Sun's position comes from that bound DE ephemeris)")]
    DragRequiresThirdBodies,
    /// [`crate::jacchia_roberts::density_kg_m3`] failed (a non-finite state, an altitude at or
    /// below GMAT's own 100 km floor, or a degenerate hour-angle geometry).
    #[error("Jacchia-Roberts density failed: {0}")]
    JacchiaRoberts(#[source] crate::jacchia_roberts::JacchiaRobertsError),
    /// [`crate::msise90::density_kg_m3`] failed (a non-finite state, or an altitude below this
    /// crate's own MSISE90 floor -- see that module's own doc comment, "Altitude floor").
    #[error("MSISE90 density failed: {0}")]
    Msise90(#[source] crate::msise90::MsiseError),
    /// [`crate::drag::drag_acceleration`] failed (a non-finite state, or a non-positive
    /// mass/area -- see [`crate::drag::DragError`]'s own variants).
    #[error("drag acceleration failed: {0}")]
    Drag(#[source] crate::drag::DragError),
}

/// Task 3c (`docs/native-dynamics-plan.md`): which atmosphere [`EarthGravityModel::with_drag`]
/// evaluates density with -- "wire it into `with_drag` as an atmosphere choice beside
/// Jacchia-Roberts" (this task's own brief). Both variants share the SAME
/// [`crate::jacchia_roberts::WeatherInputs`] (`f107`/`f107a`/`kp`) -- [`crate::msise90`]'s own
/// module doc explains why MSISE90 needs no second weather type of its own (`Kp` is converted to
/// `Ap` internally, matching GMAT's own `AtmosphereModel::ConvertKpToAp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtmosphereChoice {
    /// [`crate::jacchia_roberts`] -- ported from GMAT's own `JacchiaRobertsAtmosphere`.
    JacchiaRoberts,
    /// [`crate::msise90`] -- ported from GMAT's own `Msise90Atmosphere` (the model GMAT R2026a
    /// actually ships; see that module's own doc comment for why NRLMSISE-00, the plan's own
    /// original wording, was not built instead).
    Msise90,
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

/// One resolved third body: which DE body it is and its standard gravitational parameter
/// (m^3/s^2, read from the DE file's own `GM*`/`GMB`/`EMRAT` constants -- see [`crate::de`]'s
/// module doc), precomputed once at [`EarthGravityModel::with_third_bodies`] time rather than
/// re-read from the ephemeris on every `derivatives` call.
struct ThirdBody {
    body: DeBody,
    name: String,
    mu: f64,
}

/// N2's third-body configuration: the DE ephemeris this model reads Sun/Moon/planet
/// positions from, and the resolved bodies to perturb by (see [`EarthGravityModel::
/// with_third_bodies`]).
struct ThirdBodies {
    ephemeris: DeEphemeris,
    bodies: Vec<ThirdBody>,
}

/// A [`DynamicsModel`] for one spacecraft under Earth point-mass/spherical-harmonic gravity,
/// generic over its own body-fixed rotation source `R` -- see this module's own doc comment.
///
/// **N2 addition (`docs/native-dynamics-plan.md`):** optionally, third-body point-mass
/// perturbations from [`EarthGravityModel::with_third_bodies`] -- entirely additive
/// (`third_bodies: None` reproduces N1's exact behaviour bit-for-bit, since `derivatives`
/// below only touches the extra term when it is `Some`), and GMAT-free (the DE reader
/// [`crate::de`] and the TAI->TDB conversion [`crate::tdb`] both have no `gmat-sys`
/// dependency -- see this crate's own `lib.rs` module doc, "Dependencies"), so this addition
/// does not change the `--no-default-features` build's own scope at all.
pub struct EarthGravityModel<R: BodyFixedRotation> {
    gravity: GravityModel,
    rotation: R,
    frame_id: String,
    info: EarthGravityModelInfo,
    settings_hash: String,
    third_bodies: Option<ThirdBodies>,
    srp: Option<SrpBinding>,
    drag: Option<DragBinding>,
}

/// N3's SRP configuration: the constants GMAT's own `SolarRadiationPressure` force uses and
/// this vehicle's ballistic properties (see [`EarthGravityModel::with_srp`]).
struct SrpBinding {
    constants: crate::srp::SrpConstants,
    props: crate::srp::SrpProperties,
}

/// Task 3b's drag configuration: the weather inputs [`crate::jacchia_roberts::density_kg_m3`]
/// needs and this vehicle's ballistic drag properties (see [`EarthGravityModel::with_drag`]).
struct DragBinding {
    atmosphere: AtmosphereChoice,
    weather: crate::jacchia_roberts::WeatherInputs,
    props: crate::drag::DragProperties,
}

/// N4: `(da_dr, da_dv)`, the acceleration's own 3x3 position/velocity partial-derivative
/// blocks -- see [`EarthGravityModel::acceleration_partials`]'s own doc comment. Named so its
/// return type reads clearly (clippy's own `type_complexity` lint).
type AccelPartials = ([[f64; 3]; 3], [[f64; 3]; 3]);

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

        Ok(Self { gravity, rotation, frame_id, info, settings_hash, third_bodies: None, srp: None, drag: None })
    }

    /// The bound gravity model (degree, order, `mu`, reference radius, coefficients) -- for a
    /// caller (or test) that wants to inspect what this model actually loaded.
    pub fn gravity_model(&self) -> &GravityModel {
        &self.gravity
    }

    /// N2: adds third-body point-mass perturbations (Sun, Moon, or any other
    /// [`DeBody`]) from the DE ephemeris file at `de_ephemeris_path`, evaluated at TDB (via
    /// [`crate::tdb::tai_ns_to_tdb_jd`]) on every `derivatives` call. Each body's `mu` is
    /// resolved once here (from the DE file's own constants -- see [`crate::de::DeEphemeris::
    /// mu_si`]), not re-read per call. `settings_hash` is recomputed to additionally cover the
    /// DE file's own name and SHA-256 digest and the bound body list, so two models that
    /// differ only in their third-body configuration report different hashes (the identical
    /// rule [`EarthGravityModel::new`]'s own doc states for the gravity file).
    ///
    /// A builder consumed by value (`self -> Self`) rather than `&mut self`, matching this
    /// type's other construction-time-only configuration (there is no `without_third_bodies`
    /// -- a model is built once, per `av_dynamics::DynamicsModel`'s own contract, and never
    /// reconfigured after that).
    pub fn with_third_bodies(mut self, de_ephemeris_path: &Path, bodies: &[DeBody]) -> Result<Self, OrbitalModelError<R::Error>> {
        let de_bytes = std::fs::read(de_ephemeris_path)
            .map_err(|source| OrbitalModelError::DeFileIo { path: de_ephemeris_path.to_path_buf(), source })?;
        let de_sha256 = hex_sha256(&de_bytes);
        let de_file_name = de_ephemeris_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| de_ephemeris_path.display().to_string());

        let ephemeris = DeEphemeris::open(de_ephemeris_path).map_err(OrbitalModelError::De)?;
        let mut resolved = Vec::with_capacity(bodies.len());
        for &body in bodies {
            let mu = ephemeris.mu_si(body).map_err(OrbitalModelError::De)?;
            resolved.push(ThirdBody { body, name: format!("{body:?}"), mu });
        }

        let mut settings = BTreeMap::new();
        settings.insert("base_settings_hash".to_string(), self.settings_hash.clone());
        settings.insert("de_file_name".to_string(), de_file_name);
        settings.insert("de_file_sha256".to_string(), de_sha256);
        settings.insert("third_bodies".to_string(), resolved.iter().map(|b| b.name.clone()).collect::<Vec<_>>().join(","));
        self.settings_hash = av_dynamics::settings_hash(&settings);

        self.third_bodies = Some(ThirdBodies { ephemeris, bodies: resolved });
        Ok(self)
    }

    /// N3 (`docs/native-dynamics-plan.md`): adds cannonball solar-radiation-pressure
    /// acceleration with a conical (umbra + penumbra) shadow -- see [`crate::srp`]'s own module
    /// doc for the exact formula, the shadow model, and every constant `constants` should carry
    /// (read off a live GMAT instance; [`crate::srp::SrpConstants::gmat_earth_defaults`]
    /// supplies the measured values this crate's own goldens use). Central body is always
    /// Earth, the sole occulter this round.
    ///
    /// **Requires [`EarthGravityModel::with_third_bodies`] to already be configured** --
    /// `Err(OrbitalModelError::SrpRequiresThirdBodies)` otherwise. This is a deliberate choice
    /// between two options the task brief left open ("`with_srp` requires third bodies to have
    /// been configured, or it carries its own ephemeris handle; pick one, and say in the doc
    /// comment why"): reusing [`EarthGravityModel::with_third_bodies`]'s own bound
    /// `DeEphemeris` handle means this model has exactly ONE DE ephemeris source of truth --
    /// one file, one SHA-256 in `settings_hash` -- rather than opening a second, independent DE
    /// file handle that could (in principle, if a caller pointed the two calls at different
    /// paths) silently diverge from the third-body path's own ephemeris. The Sun's position for
    /// SRP is looked up independently of WHICH bodies are in the third-body list (a DRM asking
    /// for gravity + SRP with no Sun third-body perturbation is not forbidden), but through the
    /// SAME [`crate::de::DeEphemeris`] handle.
    ///
    /// `area_m2`/`cr`/`mass_kg` are the DRM's `SRPArea`/`Cr`/total mass (question 81: "a seed
    /// is a vehicle") -- `mass_kg` should be the vehicle's TOTAL mass (GMAT's own `TotalMass`,
    /// which equals `DryMass` when there is no fuel tank), matching `SolarRadiationPressure::
    /// GetDerivatives`'s own `mass = sc->GetRealParameter("TotalMass")`
    /// (`third_party/gmat-src/src/base/forcemodel/SolarRadiationPressure.cpp`) -- see
    /// [`crate::srp`]'s own module doc for the full term-by-term correspondence.
    ///
    /// A builder consumed by value, mirroring [`EarthGravityModel::with_third_bodies`]'s own
    /// shape exactly: `settings_hash` is recomputed to additionally cover every SRP constant
    /// and ballistic property, so two models differing only in their SRP configuration report
    /// different hashes; a model that never calls this method behaves bit-for-bit as before
    /// (`derivatives`'s own `if let Some(srp)` block is a complete no-op when `self.srp` is
    /// `None`) -- see `tests::derivatives_without_srp_matches_gravity_plus_third_body_directly`
    /// for the test that pins this.
    pub fn with_srp(mut self, constants: crate::srp::SrpConstants, area_m2: f64, cr: f64, mass_kg: f64) -> Result<Self, OrbitalModelError<R::Error>> {
        if self.third_bodies.is_none() {
            return Err(OrbitalModelError::SrpRequiresThirdBodies);
        }

        let mut settings = BTreeMap::new();
        settings.insert("base_settings_hash".to_string(), self.settings_hash.clone());
        settings.insert("srp_flux_pressure_n_m2".to_string(), format!("{:.17e}", constants.flux_pressure_n_m2));
        settings.insert("srp_reference_distance_m".to_string(), format!("{:.17e}", constants.reference_distance_m));
        settings.insert("srp_sun_radius_m".to_string(), format!("{:.17e}", constants.sun_radius_m));
        settings.insert("srp_body_radius_m".to_string(), format!("{:.17e}", constants.body_radius_m));
        settings.insert("srp_area_m2".to_string(), format!("{:.17e}", area_m2));
        settings.insert("srp_cr".to_string(), format!("{:.17e}", cr));
        settings.insert("srp_mass_kg".to_string(), format!("{:.17e}", mass_kg));
        self.settings_hash = av_dynamics::settings_hash(&settings);

        self.srp = Some(SrpBinding { constants, props: crate::srp::SrpProperties { cr, area_m2, mass_kg } });
        Ok(self)
    }

    /// Task 3b (`docs/native-dynamics-plan.md` milestone N3): adds Jacchia-Roberts atmospheric
    /// drag -- see [`crate::jacchia_roberts`]'s own module doc for the density model
    /// (ported from GMAT's own `JacchiaRobertsAtmosphere`) and [`crate::drag`]'s own module doc
    /// for the acceleration formula and the rotating-atmosphere relative velocity (read from
    /// GMAT's `DragForce.cpp`, term for term). Central body is always Earth, matching
    /// [`EarthGravityModel::with_srp`]'s identical "Earth as sole occulter/atmosphere" scope
    /// this round.
    ///
    /// **Requires [`EarthGravityModel::with_third_bodies`] to already be configured** --
    /// `Err(OrbitalModelError::DragRequiresThirdBodies)` otherwise -- for the identical reason
    /// [`EarthGravityModel::with_srp`] does (see that method's own doc comment): the Sun's
    /// position the diurnal exospheric-temperature bulge needs comes from the SAME bound
    /// `DeEphemeris` handle, so this model has exactly one DE ephemeris source of truth.
    ///
    /// `atmosphere` selects [`crate::jacchia_roberts`] or [`crate::msise90`] -- task 3c's own
    /// addition, "an atmosphere choice beside Jacchia-Roberts" (see [`AtmosphereChoice`]'s own
    /// doc comment). `weather` is [`crate::jacchia_roberts::WeatherInputs`] -- either GMAT's own
    /// CONSTANT defaults (`WeatherInputs::from(`[`crate::weather::ConstantWeather::
    /// gmat_defaults`]`())`, what GMAT's `DragForce` actually uses unless a DRM configures a
    /// weather file -- see [`crate::weather`]'s own module doc) or a caller-resolved,
    /// file-derived triple -- the SAME triple regardless of `atmosphere` (see
    /// [`AtmosphereChoice`]'s own doc comment for why MSISE90 needs no second weather type).
    /// `area_m2`/`cd`/`mass_kg` are the DRM's `DragArea`/`Cd`/total mass (question 81: "a seed
    /// is a vehicle") -- `mass_kg` should be the vehicle's TOTAL mass, matching
    /// `DragForce::BuildPrefactors`'s own `mass[i] = sc->GetRealParameter(massID)` (`TotalMass`
    /// in every golden this crate's tests use).
    ///
    /// A builder consumed by value, mirroring [`EarthGravityModel::with_srp`]'s own shape
    /// exactly: `settings_hash` is recomputed to additionally cover the atmosphere choice, every
    /// weather input and ballistic property, so two models differing only in their drag
    /// configuration report different hashes; a model that never calls this method behaves
    /// bit-for-bit as before (`derivatives`'s own `if let Some(drag)` block is a complete no-op
    /// when `self.drag` is `None`) -- see
    /// `tests::derivatives_without_drag_matches_gravity_plus_third_body_directly` for the test
    /// that pins this.
    pub fn with_drag(mut self, atmosphere: AtmosphereChoice, weather: crate::jacchia_roberts::WeatherInputs, area_m2: f64, cd: f64, mass_kg: f64) -> Result<Self, OrbitalModelError<R::Error>> {
        if self.third_bodies.is_none() {
            return Err(OrbitalModelError::DragRequiresThirdBodies);
        }

        let mut settings = BTreeMap::new();
        settings.insert("base_settings_hash".to_string(), self.settings_hash.clone());
        settings.insert("drag_atmosphere".to_string(), format!("{atmosphere:?}"));
        settings.insert("drag_weather_f107".to_string(), format!("{:.17e}", weather.f107));
        settings.insert("drag_weather_f107a".to_string(), format!("{:.17e}", weather.f107a));
        settings.insert("drag_weather_kp".to_string(), format!("{:.17e}", weather.kp));
        settings.insert("drag_area_m2".to_string(), format!("{:.17e}", area_m2));
        settings.insert("drag_cd".to_string(), format!("{:.17e}", cd));
        settings.insert("drag_mass_kg".to_string(), format!("{:.17e}", mass_kg));
        self.settings_hash = av_dynamics::settings_hash(&settings);

        self.drag = Some(DragBinding { atmosphere, weather, props: crate::drag::DragProperties { cd, area_m2, mass_kg } });
        Ok(self)
    }

    /// N4: the drag acceleration alone (no gravity, no third body, no SRP) at inertial
    /// `pos_inertial`/`vel_inertial` and epoch `t_tai_ns` -- exactly the `if let Some(drag)`
    /// block of [`EarthGravityModel::derivatives`], factored out so [`EarthGravityModel::
    /// acceleration_partials`] can call it in isolation (both to seed the analytic velocity
    /// block via `Dual3` and to finite-difference the position block -- see that method's own
    /// doc comment). `Ok([0.0; 3])` when no drag is configured, matching `derivatives`'s own
    /// "a complete no-op when `self.drag` is `None`" contract.
    fn drag_acceleration_only(&self, pos_inertial: [f64; 3], vel_inertial: [f64; 3], t_tai_ns: i64) -> Result<[f64; 3], OrbitalModelError<R::Error>> {
        let Some(drag) = &self.drag else {
            return Ok([0.0, 0.0, 0.0]);
        };
        let tb = self.third_bodies.as_ref().expect("with_drag requires with_third_bodies (enforced at construction)");
        let cb = crate::jacchia_roberts::CentralBodyGeodetics::earth_defaults();
        let rho = match drag.atmosphere {
            AtmosphereChoice::JacchiaRoberts => {
                let (jd1, jd2) = crate::tdb::tai_ns_to_tdb_jd2(t_tai_ns);
                let sun_km = tb.ephemeris.geocentric_position_km2(DeBody::Sun, jd1, jd2).map_err(OrbitalModelError::De)?;
                let r_sun_m = [sun_km[0] * 1e3, sun_km[1] * 1e3, sun_km[2] * 1e3];
                crate::jacchia_roberts::density_kg_m3(pos_inertial, r_sun_m, &self.rotation, t_tai_ns, &drag.weather, &cb).map_err(OrbitalModelError::JacchiaRoberts)?
            }
            AtmosphereChoice::Msise90 => crate::msise90::density_kg_m3(pos_inertial, &self.rotation, t_tai_ns, &drag.weather, &cb).map_err(OrbitalModelError::Msise90)?,
        };
        crate::drag::drag_acceleration(pos_inertial, vel_inertial, rho, crate::drag::EARTH_ANGULAR_VELOCITY_RAD_S, &drag.props).map_err(OrbitalModelError::Drag)
    }

    /// N4 (ADR-002 second/third amendments): the acceleration's own 3x3 position (`da_dr`) and
    /// velocity (`da_dv`) partial-derivative blocks, `da_dr[i][j] = d(accel_i)/d(pos_j)`,
    /// `da_dv[i][j] = d(accel_i)/d(vel_j)`, the SUM of every configured force's own
    /// contribution -- see `crate::stm`'s own module doc for why the blocks simply add.
    ///
    /// | Force | position block | velocity block | how |
    /// |---|---|---|---|
    /// | gravity | filled | zero | analytic: `spherical_harmonic_gravity`'s own returned 3x3 gradient, rotated body-fixed -> inertial by `R^T G R` (`crate::stm::rotate_gradient_body_to_inertial`) |
    /// | third bodies | filled | zero | analytic: `crate::third_body::third_body_partials` (forward-mode AD, `Dual3`) |
    /// | SRP (spherical) | filled (if configured) | exactly zero (if configured) | analytic: `crate::srp::cannonball_acceleration` seeded with `Dual3` in position; velocity block never touched (stays zero), matching ADR-002's third amendment's own measured `SolarRadiationPressure` behaviour, including the shadow (`nu`) partials being OMITTED because `nu` is frozen `f64` (`crate::srp`'s own doc, "Shape for N4") |
    /// | drag | filled (if configured) | filled (if configured) | position: CENTRAL FINITE DIFFERENCE of [`EarthGravityModel::drag_acceleration_only`] (matching ADR-002's third amendment's own finding that GMAT's `DragForce` finite-differences its own acceleration for BOTH blocks; this model only finite-differences the block it cannot get analytically). velocity: analytic, `crate::drag::drag_acceleration_core` seeded with `Dual3` in `v_rel` (`d(v_rel)/d(v) = I`, since `v_rel = v - omega x r` is affine in `v` with `r` held fixed, so seeding `v` directly is exact -- see this method's own body) |
    ///
    /// A force never configured (`self.srp`/`self.drag` both `None`) contributes nothing --
    /// `da_dr`/`da_dv` are exactly gravity's own analytic blocks plus whichever third bodies
    /// are bound, matching `derivatives`'s own "no-op when not configured" contract.
    fn acceleration_partials(&self, pos_inertial: [f64; 3], vel_inertial: [f64; 3], t_tai_ns: i64) -> Result<AccelPartials, OrbitalModelError<R::Error>> {
        let rotation = self.rotation.inertial_to_fixed(t_tai_ns).map_err(OrbitalModelError::Rotation)?;
        let pos_fixed = rotation.apply(pos_inertial);
        let (_accel_fixed, g_fixed) = gravity::spherical_harmonic_gravity(pos_fixed, &self.gravity);
        let mut da_dr = rotate_gradient_body_to_inertial(&rotation, g_fixed);
        let mut da_dv = [[0.0_f64; 3]; 3];

        if let Some(tb) = &self.third_bodies {
            let (jd1, jd2) = crate::tdb::tai_ns_to_tdb_jd2(t_tai_ns);
            for third in &tb.bodies {
                let d_km = tb.ephemeris.geocentric_position_km2(third.body, jd1, jd2).map_err(OrbitalModelError::De)?;
                let d_m = [d_km[0] * 1e3, d_km[1] * 1e3, d_km[2] * 1e3];
                let p = third_body_partials(pos_inertial, d_m, third.mu);
                for (row, p_row) in da_dr.iter_mut().zip(p.iter()) {
                    for (v, pv) in row.iter_mut().zip(p_row.iter()) {
                        *v += *pv;
                    }
                }
            }
        }

        if let Some(srp) = &self.srp {
            let tb = self.third_bodies.as_ref().expect("with_srp requires with_third_bodies (enforced at construction)");
            let (jd1, jd2) = crate::tdb::tai_ns_to_tdb_jd2(t_tai_ns);
            let sun_km = tb.ephemeris.geocentric_position_km2(DeBody::Sun, jd1, jd2).map_err(OrbitalModelError::De)?;
            let r_sun_m = [sun_km[0] * 1e3, sun_km[1] * 1e3, sun_km[2] * 1e3];
            let nu = crate::srp::illumination_fraction(pos_inertial, r_sun_m, srp.constants.sun_radius_m, srp.constants.body_radius_m).map_err(OrbitalModelError::Srp)?;
            let k = crate::srp::srp_k(&srp.constants, &srp.props);
            let dual_pos = [Dual3::variable(pos_inertial[0], 0), Dual3::variable(pos_inertial[1], 1), Dual3::variable(pos_inertial[2], 2)];
            let a_dual = crate::srp::cannonball_acceleration(dual_pos, r_sun_m, nu, k);
            for (row, comp) in da_dr.iter_mut().zip(a_dual.iter()) {
                for (v, dv) in row.iter_mut().zip(comp.d.iter()) {
                    *v += *dv;
                }
            }
            // Velocity block: SRP has no velocity dependence at all -- da_dv untouched (stays
            // whatever it already was, zero unless drag, below, also contributes).
        }

        if let Some(drag) = &self.drag {
            let k = -0.5 * drag.props.cd * drag.props.area_m2 / drag.props.mass_kg;
            let omega_z = crate::drag::EARTH_ANGULAR_VELOCITY_RAD_S;
            // v_rel = [v_x + omega_z*r_y, v_y - omega_z*r_x, v_z] -- affine in v with r fixed,
            // so seeding v directly with Dual3 gives d(v_rel)/d(v) = I exactly (the omega x r
            // term, depending only on r, contributes nothing to any tangent here).
            let v_rel_dual = [
                Dual3::variable(vel_inertial[0], 0) + Dual3::constant(omega_z * pos_inertial[1]),
                Dual3::variable(vel_inertial[1], 1) - Dual3::constant(omega_z * pos_inertial[0]),
                Dual3::variable(vel_inertial[2], 2),
            ];
            let cb = crate::jacchia_roberts::CentralBodyGeodetics::earth_defaults();
            let tb = self.third_bodies.as_ref().expect("with_drag requires with_third_bodies (enforced at construction)");
            let rho = match drag.atmosphere {
                AtmosphereChoice::JacchiaRoberts => {
                    let (jd1, jd2) = crate::tdb::tai_ns_to_tdb_jd2(t_tai_ns);
                    let sun_km = tb.ephemeris.geocentric_position_km2(DeBody::Sun, jd1, jd2).map_err(OrbitalModelError::De)?;
                    let r_sun_m = [sun_km[0] * 1e3, sun_km[1] * 1e3, sun_km[2] * 1e3];
                    crate::jacchia_roberts::density_kg_m3(pos_inertial, r_sun_m, &self.rotation, t_tai_ns, &drag.weather, &cb).map_err(OrbitalModelError::JacchiaRoberts)?
                }
                AtmosphereChoice::Msise90 => crate::msise90::density_kg_m3(pos_inertial, &self.rotation, t_tai_ns, &drag.weather, &cb).map_err(OrbitalModelError::Msise90)?,
            };
            let a_dual = crate::drag::drag_acceleration_core(v_rel_dual, rho, k);
            for (row, comp) in da_dv.iter_mut().zip(a_dual.iter()) {
                for (v, dv) in row.iter_mut().zip(comp.d.iter()) {
                    *v += *dv;
                }
            }

            // Position block: central finite difference of the FULL drag acceleration
            // (density's altitude/latitude dependence AND the omega x r term's own dependence
            // on r) -- matching ADR-002's third amendment's own finding that GMAT itself
            // finite-differences this block. `DRAG_POSITION_FD_STEP_M` is a measured constant
            // (this crate's own report: the sweep that chose it) -- see this module's own
            // `tests::drag_position_fd_step_size_sweep`.
            let h = DRAG_POSITION_FD_STEP_M;
            for j in 0..3 {
                let mut plus = pos_inertial;
                let mut minus = pos_inertial;
                plus[j] += h;
                minus[j] -= h;
                let a_plus = self.drag_acceleration_only(plus, vel_inertial, t_tai_ns)?;
                let a_minus = self.drag_acceleration_only(minus, vel_inertial, t_tai_ns)?;
                for i in 0..3 {
                    da_dr[i][j] += (a_plus[i] - a_minus[i]) / (2.0 * h);
                }
            }
        }

        Ok((da_dr, da_dv))
    }
}

/// N4: the central finite-difference step size for drag's position block (metres) -- measured,
/// not assumed, by sweeping `h` from 0.5 m to 1e4 m and comparing each estimate against a
/// Richardson-extrapolated reference built from a much finer step
/// (`tests::drag_position_fd_step_size_sweep` prints the full sweep; the exact numbers are also
/// recorded in this crate's own N4 report). **Measured** (debug build, this host, the N4 LEO
/// test state, `--nocapture`): the sweep's own best point is `h=0.5 m`
/// (`rel_vs_richardson=8.716e-7`), essentially flat out to `h=10 m` (`9.165e-7` there) before
/// truncation error grows monotonically with `h` past that (`3.330e-5` at `h=1000 m`, `3.263e-3`
/// at `h=10000 m`) -- no round-off "far side" of the V was reached even at the finest swept
/// step, because the drag acceleration at this test's ~647 km altitude is tiny (Frobenius scale
/// `1.317e-12` m/s^2/m) and correspondingly forgiving of round-off. `h = 1.0 m` sits inside that
/// flat, near-optimal region (`rel_vs_richardson=8.852e-7`, indistinguishable in practice from
/// the sweep's own best value) and matches the rest of this crate's own analytic-partial-test
/// convention (`gravity.rs`/`third_body.rs`'s own `h = 1.0` m finite differences) at a
/// comparable LEO position scale (~6.9e6-7.0e6 m, relative step ~1.4e-7), which is not a
/// coincidence: both are trading the same two error sources (truncation error growing with `h`,
/// cancellation/round-off error growing as `h` shrinks) on comparably-scaled quantities.
pub const DRAG_POSITION_FD_STEP_M: f64 = 1.0;

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
        let mut accel_inertial = rotation.apply_transpose(accel_fixed);

        // N2: third-body point-mass perturbations, evaluated directly in the inertial frame
        // (no rotation involved -- see crate::third_body's own module doc for the formula).
        //
        // Round 2 (task 2b): uses the two-part TDB epoch (`crate::tdb::tai_ns_to_tdb_jd2`) and
        // `DeEphemeris::geocentric_position_km2`, not the single-`f64` `tai_ns_to_tdb_jd`/
        // `geocentric_position_km` this call used through round 1 -- root-caused as the
        // source of the ten-epoch Mars/Jupiter ephemeris disagreement (`crate::tdb`'s module
        // doc, "Precision"; `crate::de`'s `geocentric_position_km2` doc comment).
        if let Some(tb) = &self.third_bodies {
            let (jd1, jd2) = crate::tdb::tai_ns_to_tdb_jd2(t_tai_ns);
            for third in &tb.bodies {
                let d_km = tb.ephemeris.geocentric_position_km2(third.body, jd1, jd2).map_err(OrbitalModelError::De)?;
                let d_m = [d_km[0] * 1e3, d_km[1] * 1e3, d_km[2] * 1e3];
                let a = third_body_acceleration(pos_inertial, d_m, third.mu);
                accel_inertial[0] += a[0];
                accel_inertial[1] += a[1];
                accel_inertial[2] += a[2];
            }
        }

        // N3: cannonball SRP with a conical shadow -- see crate::srp's own module doc.
        // `with_srp` refuses construction unless `third_bodies` is already `Some` (this
        // module's own invariant, enforced only at `with_srp` time -- see that method's own
        // doc comment for why), so `self.third_bodies` is guaranteed `Some` here whenever
        // `self.srp` is; the `.expect` below documents that invariant rather than trusting
        // caller input (mirrors this crate's existing structural-invariant `.expect`s, e.g.
        // `crate::de`'s `"checked length"`).
        if let Some(srp) = &self.srp {
            let tb = self.third_bodies.as_ref().expect("with_srp requires with_third_bodies (enforced at construction)");
            let (jd1, jd2) = crate::tdb::tai_ns_to_tdb_jd2(t_tai_ns);
            let sun_km = tb.ephemeris.geocentric_position_km2(DeBody::Sun, jd1, jd2).map_err(OrbitalModelError::De)?;
            let r_sun_m = [sun_km[0] * 1e3, sun_km[1] * 1e3, sun_km[2] * 1e3];
            let a = crate::srp::srp_acceleration(pos_inertial, r_sun_m, &srp.constants, &srp.props).map_err(OrbitalModelError::Srp)?;
            accel_inertial[0] += a[0];
            accel_inertial[1] += a[1];
            accel_inertial[2] += a[2];
        }

        // Task 3b/3c: drag, either atmosphere -- see crate::jacchia_roberts's, crate::msise90's
        // and crate::drag's own module docs. `with_drag` refuses construction unless
        // `third_bodies` is already `Some`, the identical invariant `with_srp` enforces (see
        // that block's own comment, above, for why the `.expect` here is safe) -- kept uniform
        // across BOTH atmospheres even though MSISE90 itself needs no Sun position (see
        // `AtmosphereChoice`'s own doc comment): one invariant, not a per-atmosphere special
        // case.
        if let Some(drag) = &self.drag {
            let tb = self.third_bodies.as_ref().expect("with_drag requires with_third_bodies (enforced at construction)");
            let cb = crate::jacchia_roberts::CentralBodyGeodetics::earth_defaults();
            let rho = match drag.atmosphere {
                AtmosphereChoice::JacchiaRoberts => {
                    let (jd1, jd2) = crate::tdb::tai_ns_to_tdb_jd2(t_tai_ns);
                    let sun_km = tb.ephemeris.geocentric_position_km2(DeBody::Sun, jd1, jd2).map_err(OrbitalModelError::De)?;
                    let r_sun_m = [sun_km[0] * 1e3, sun_km[1] * 1e3, sun_km[2] * 1e3];
                    crate::jacchia_roberts::density_kg_m3(pos_inertial, r_sun_m, &self.rotation, t_tai_ns, &drag.weather, &cb).map_err(OrbitalModelError::JacchiaRoberts)?
                }
                AtmosphereChoice::Msise90 => crate::msise90::density_kg_m3(pos_inertial, &self.rotation, t_tai_ns, &drag.weather, &cb).map_err(OrbitalModelError::Msise90)?,
            };
            let a = crate::drag::drag_acceleration(pos_inertial, vel_inertial, rho, crate::drag::EARTH_ANGULAR_VELOCITY_RAD_S, &drag.props).map_err(OrbitalModelError::Drag)?;
            accel_inertial[0] += a[0];
            accel_inertial[1] += a[1];
            accel_inertial[2] += a[2];
        }

        state_dot[0..3].copy_from_slice(&vel_inertial);
        state_dot[3..6].copy_from_slice(&accel_inertial);
        Ok(())
    }

    /// `av_cdm::pb::ModelInfo`, filled the way `gmat_sys::model::GmatModel::describe` fills it
    /// (see this module's own "Units" doc section for how `state_space_id` was matched) --
    /// `depth` is `"native"` (the proto's own doc comment: `"gmat-api"`, `"gmat-ffi"`,
    /// `"native"`, or an external engine name; `GmatModel`'s own depth is `"gmat-ffi"`).
    /// `capabilities` includes `ModelCapability::Stm` (N4, see this module's own "The state
    /// transition matrix" doc section) -- always, since [`EarthGravityModel::stm_capable`]
    /// always returns `true`.
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
                ModelCapability::Stm as i32,
            ],
            depth: "native".to_string(),
            settings_hash: self.settings_hash.clone(),
            goldens: self.info.goldens.clone(),
        }
    }

    /// N4: always `true` -- see this module's own "The state transition matrix" doc section
    /// for why this model, unlike `gmat_sys::model::GmatModel`, never has a reason to withhold
    /// the capability.
    fn stm_capable(&self) -> bool {
        true
    }

    /// N4 (ADR-002 second amendment): `state_dot` for the STM-augmented state -- index `0..6`
    /// is the physical state (delegates to [`EarthGravityModel::derivatives`] directly, so it
    /// is bit-identical to what a plain `derivatives` call on the same input would compute, the
    /// trait's own documented requirement), index `6 + row*6 + col` is `d(Phi)/dt` element
    /// `(row, col)`, via [`crate::stm::stm_rate`] fed by [`EarthGravityModel::
    /// acceleration_partials`]'s own `da_dr`/`da_dv`.
    fn stm_derivatives(&self, augmented_state: &[f64], t_tai_ns: i64, controls: &[f64], augmented_state_dot: &mut [f64]) -> Result<(), Self::Error> {
        if augmented_state.len() != 42 {
            return Err(OrbitalModelError::BadAugmentedStateLen(augmented_state.len()));
        }
        if augmented_state_dot.len() != 42 {
            return Err(OrbitalModelError::BadAugmentedStateLen(augmented_state_dot.len()));
        }
        let (state6, phi) = augmented_state.split_at(6);

        let mut state_dot6 = [0.0_f64; 6];
        self.derivatives(state6, t_tai_ns, controls, &mut state_dot6)?;
        augmented_state_dot[0..6].copy_from_slice(&state_dot6);

        let pos_inertial = [state6[0], state6[1], state6[2]];
        let vel_inertial = [state6[3], state6[4], state6[5]];
        let (da_dr, da_dv) = self.acceleration_partials(pos_inertial, vel_inertial, t_tai_ns)?;
        let phi_dot = stm_rate(da_dr, da_dv, phi);
        augmented_state_dot[6..42].copy_from_slice(&phi_dot);
        Ok(())
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
    fn describe_reports_native_depth_gmat_orbital_state_space_and_stm_capability() {
        // N4: this model always declares the STM capability -- see `model.rs`'s own "The
        // state transition matrix" doc section for why (unlike this test's own pre-N4 name,
        // this model has no `RelativisticCorrection`-style gap that would ever force it false).
        let model = point_mass_model();
        let info = model.describe();
        assert_eq!(info.depth, "native");
        assert_eq!(info.state_space_id, "gmat.orbital.cartesian6");
        assert_eq!(info.frame_id, "EarthMJ2000Eq");
        assert_eq!(info.settings_hash.len(), 64, "hex-encoded SHA-256 is 64 chars");
        assert!(info.capabilities.contains(&(ModelCapability::Stm as i32)), "N4: describe() must advertise ModelCapability::Stm");
        assert!(model.stm_capable(), "N4: stm_capable() must report true");
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

    fn de_path() -> std::path::PathBuf {
        crate::de::DeEphemeris::locate_gmat_root().expect("GMAT_ROOT set").join("data/planetary_ephem/de/leDE1941.405")
    }

    fn point_mass_model_with_third_bodies() -> EarthGravityModel<IdentityRotation> {
        point_mass_model().with_third_bodies(&de_path(), &[DeBody::Moon, DeBody::Sun]).expect("with_third_bodies")
    }

    /// [`EarthGravityModel::with_srp`]'s own documented invariant: it refuses construction
    /// unless [`EarthGravityModel::with_third_bodies`] was already called.
    #[test]
    fn with_srp_without_third_bodies_is_a_typed_error_not_a_panic() {
        // `.unwrap_err()` needs `Self: Debug` on the `Ok` side, which `EarthGravityModel<R>`
        // does not implement (it carries a `GravityModel` with no meaningful `Debug` of its
        // own -- see this crate's other tests' identical avoidance of `.unwrap()`/`.expect()`
        // on a `Result<EarthGravityModel<_>, _>`'s `Ok` side); match directly instead.
        match point_mass_model().with_srp(crate::srp::SrpConstants::gmat_earth_defaults(), 5.0, 1.8, 500.0) {
            Err(err) => assert!(matches!(err, OrbitalModelError::SrpRequiresThirdBodies)),
            Ok(_) => panic!("expected with_srp to refuse a model with no third_bodies bound"),
        }
    }

    #[test]
    fn settings_hash_changes_when_srp_is_added() {
        let without_srp = point_mass_model_with_third_bodies();
        let hash_without = without_srp.describe().settings_hash;
        let with_srp = point_mass_model_with_third_bodies()
            .with_srp(crate::srp::SrpConstants::gmat_earth_defaults(), 5.0, 1.8, 500.0)
            .expect("with_srp");
        assert_ne!(hash_without, with_srp.describe().settings_hash);
    }

    /// N3's own required test: "a model built without SRP is bit-for-bit unchanged." A model
    /// with `third_bodies` bound but `with_srp` NEVER called must produce EXACTLY the same
    /// `derivatives` output as gravity + third-body acceleration computed directly (bypassing
    /// `EarthGravityModel` entirely) -- i.e. the new `if let Some(srp)` block in `derivatives`
    /// is a complete no-op when `self.srp` is `None`, proven by recomputing the expected answer
    /// through an entirely independent code path (mirrors `degree_zero_matches_the_closed_
    /// form_point_mass_acceleration_through_the_full_model`'s own pattern, above).
    #[test]
    fn derivatives_without_srp_matches_gravity_plus_third_body_directly() {
        let model = point_mass_model_with_third_bodies();
        let pos = [7_000_000.0, 500_000.0, -200_000.0];
        let vel = [-1_200.0, 7_400.0, 300.0];
        let state = [pos[0], pos[1], pos[2], vel[0], vel[1], vel[2]];
        let t_tai_ns = 1_800_000_000_000_000_000_i64;
        let mut dot = [0.0; 6];
        model.derivatives(&state, t_tai_ns, &[], &mut dot).unwrap();

        // Independently: gravity (IdentityRotation, so body-fixed == inertial) + third-body
        // Moon/Sun, computed directly against the SAME DE file/TDB conversion this model uses
        // internally, never through EarthGravityModel.
        let (accel_gravity, _) = gravity::spherical_harmonic_gravity(pos, model.gravity_model());
        let de = crate::de::DeEphemeris::open(&de_path()).expect("open DE405");
        let (jd1, jd2) = crate::tdb::tai_ns_to_tdb_jd2(t_tai_ns);
        let mut expect = accel_gravity;
        for body in [DeBody::Moon, DeBody::Sun] {
            let d_km = de.geocentric_position_km2(body, jd1, jd2).expect("geocentric_position_km2");
            let d_m = [d_km[0] * 1e3, d_km[1] * 1e3, d_km[2] * 1e3];
            let mu = de.mu_si(body).expect("mu_si");
            let a = third_body_acceleration(pos, d_m, mu);
            expect[0] += a[0];
            expect[1] += a[1];
            expect[2] += a[2];
        }

        assert_eq!(&dot[3..6], &expect[..], "a model with no SRP bound must be bit-for-bit gravity + third-body acceleration alone");
    }

    /// The SAME comparison, but WITH `with_srp` bound, confirms SRP actually changes the
    /// output (a sanity check that the wiring above is not silently inert) -- the counterpart
    /// to the bit-for-bit test above.
    #[test]
    fn derivatives_with_srp_differs_from_gravity_plus_third_body_alone() {
        let without_srp = point_mass_model_with_third_bodies();
        let with_srp = point_mass_model_with_third_bodies()
            .with_srp(crate::srp::SrpConstants::gmat_earth_defaults(), 5.0, 1.8, 500.0)
            .expect("with_srp");
        let pos = [7_000_000.0, 500_000.0, -200_000.0];
        let vel = [-1_200.0, 7_400.0, 300.0];
        let state = [pos[0], pos[1], pos[2], vel[0], vel[1], vel[2]];
        let t_tai_ns = 1_800_000_000_000_000_000_i64;
        let mut dot_without = [0.0; 6];
        let mut dot_with = [0.0; 6];
        without_srp.derivatives(&state, t_tai_ns, &[], &mut dot_without).unwrap();
        with_srp.derivatives(&state, t_tai_ns, &[], &mut dot_with).unwrap();
        assert_ne!(&dot_without[3..6], &dot_with[3..6], "with_srp must actually change the acceleration");
    }

    /// [`EarthGravityModel::with_drag`]'s own documented invariant: it refuses construction
    /// unless [`EarthGravityModel::with_third_bodies`] was already called -- mirrors
    /// `with_srp_without_third_bodies_is_a_typed_error_not_a_panic`, above.
    #[test]
    fn with_drag_without_third_bodies_is_a_typed_error_not_a_panic() {
        let weather = crate::jacchia_roberts::WeatherInputs::from(crate::weather::ConstantWeather::gmat_defaults());
        match point_mass_model().with_drag(AtmosphereChoice::JacchiaRoberts, weather, 5.0, 2.2, 500.0) {
            Err(err) => assert!(matches!(err, OrbitalModelError::DragRequiresThirdBodies)),
            Ok(_) => panic!("expected with_drag to refuse a model with no third_bodies bound"),
        }
    }

    #[test]
    fn settings_hash_changes_when_drag_is_added() {
        let weather = crate::jacchia_roberts::WeatherInputs::from(crate::weather::ConstantWeather::gmat_defaults());
        let without_drag = point_mass_model_with_third_bodies();
        let hash_without = without_drag.describe().settings_hash;
        let with_drag = point_mass_model_with_third_bodies().with_drag(AtmosphereChoice::JacchiaRoberts, weather, 5.0, 2.2, 500.0).expect("with_drag");
        assert_ne!(hash_without, with_drag.describe().settings_hash);
    }

    /// Task 3b's own required test: "a model built without drag is bit-for-bit unchanged." A
    /// model with `third_bodies` bound but `with_drag` NEVER called must produce EXACTLY the
    /// same `derivatives` output as gravity + third-body acceleration computed directly --
    /// mirrors `derivatives_without_srp_matches_gravity_plus_third_body_directly`'s own
    /// pattern exactly (the new `if let Some(drag)` block in `derivatives` is a complete no-op
    /// when `self.drag` is `None`).
    #[test]
    fn derivatives_without_drag_matches_gravity_plus_third_body_directly() {
        let model = point_mass_model_with_third_bodies();
        let pos = [7_000_000.0, 500_000.0, -200_000.0];
        let vel = [-1_200.0, 7_400.0, 300.0];
        let state = [pos[0], pos[1], pos[2], vel[0], vel[1], vel[2]];
        let t_tai_ns = 1_800_000_000_000_000_000_i64;
        let mut dot = [0.0; 6];
        model.derivatives(&state, t_tai_ns, &[], &mut dot).unwrap();

        let (accel_gravity, _) = gravity::spherical_harmonic_gravity(pos, model.gravity_model());
        let de = crate::de::DeEphemeris::open(&de_path()).expect("open DE405");
        let (jd1, jd2) = crate::tdb::tai_ns_to_tdb_jd2(t_tai_ns);
        let mut expect = accel_gravity;
        for body in [DeBody::Moon, DeBody::Sun] {
            let d_km = de.geocentric_position_km2(body, jd1, jd2).expect("geocentric_position_km2");
            let d_m = [d_km[0] * 1e3, d_km[1] * 1e3, d_km[2] * 1e3];
            let mu = de.mu_si(body).expect("mu_si");
            let a = third_body_acceleration(pos, d_m, mu);
            expect[0] += a[0];
            expect[1] += a[1];
            expect[2] += a[2];
        }

        assert_eq!(&dot[3..6], &expect[..], "a model with no drag bound must be bit-for-bit gravity + third-body acceleration alone");
    }

    /// The SAME comparison, but WITH `with_drag` bound, confirms drag actually changes the
    /// output -- the counterpart to the bit-for-bit test above.
    #[test]
    fn derivatives_with_drag_differs_from_gravity_plus_third_body_alone() {
        let weather = crate::jacchia_roberts::WeatherInputs::from(crate::weather::ConstantWeather::gmat_defaults());
        let without_drag = point_mass_model_with_third_bodies();
        let with_drag = point_mass_model_with_third_bodies().with_drag(AtmosphereChoice::JacchiaRoberts, weather, 5.0, 2.2, 500.0).expect("with_drag");
        let pos = [7_000_000.0, 500_000.0, -200_000.0];
        let vel = [-1_200.0, 7_400.0, 300.0];
        let state = [pos[0], pos[1], pos[2], vel[0], vel[1], vel[2]];
        let t_tai_ns = 1_800_000_000_000_000_000_i64;
        let mut dot_without = [0.0; 6];
        let mut dot_with = [0.0; 6];
        without_drag.derivatives(&state, t_tai_ns, &[], &mut dot_without).unwrap();
        with_drag.derivatives(&state, t_tai_ns, &[], &mut dot_with).unwrap();
        assert_ne!(&dot_without[3..6], &dot_with[3..6], "with_drag must actually change the acceleration");
    }

    // ============================================================================
    // N4: the state transition matrix (`docs/native-dynamics-plan.md`, ADR-002's second and
    // third amendments). Every test below is GMAT-free (`IdentityRotation`), so it runs under
    // `cargo test -p av-orbital --no-default-features` too -- the rotation direction itself is
    // proved separately, and GMAT-free, in `crate::stm`'s own tests
    // (`rotates_r_transpose_g_r_not_the_reversed_form`); these tests exercise the A-matrix
    // assembly and integration end to end.
    // ============================================================================

    /// JGM2 8x8 (a real, non-trivial gravity gradient) + Moon/Sun third bodies, no SRP/drag --
    /// the same force-model shape `goldens/leo_1day_jgm2_8x8_sunmoon.json`'s own `stm` block
    /// was generated against (Priority 3's golden), used here for the GMAT-free FD-STM/
    /// symplectic/Liouville tests below (which do not need to match GMAT, only be internally
    /// consistent).
    fn jgm2_8x8_model_with_third_bodies() -> EarthGravityModel<IdentityRotation> {
        EarthGravityModel::new(
            &jgm2_path(),
            8,
            8,
            "Earth",
            IdentityRotation,
            EarthGravityModelInfo { id: "native.orbital.test_n4_jgm2_8x8".to_string(), version: "test".to_string(), goldens: vec![] },
        )
        .expect("EarthGravityModel construction")
        .with_third_bodies(&de_path(), &[DeBody::Moon, DeBody::Sun])
        .expect("with_third_bodies")
    }

    /// A representative bound LEO state (position ~7e6 m, speed ~7.5 km/s), reused from this
    /// module's own `derivatives_first_three_components_equal_velocity_exactly`.
    const N4_LEO_STATE: [f64; 6] = [7_000_000.0, 500_000.0, -200_000.0, -1_200.0, 7_400.0, 300.0];
    const N4_T0_TAI_NS: i64 = 1_800_000_000_000_000_000;

    #[test]
    fn n4_phi_t0_t0_is_the_exact_identity() {
        let model = jgm2_8x8_model_with_third_bodies();
        let result = model.step_with_stm(&N4_LEO_STATE, N4_T0_TAI_NS, &[], 0).unwrap();
        assert_eq!(result.phi, {
            let mut id = vec![0.0; 36];
            for i in 0..6 {
                id[i * 6 + i] = 1.0;
            }
            id
        });
    }

    /// **The STM against a finite-difference STM, step size measured by a sweep, not
    /// assumed** (N4's own required test). Perturbs each of the six initial-state components
    /// by `h` (scaled per-block: position components by a position-scale `h`, velocity
    /// components by a velocity-scale `h` -- `h` itself swept over decades, holding the ratio
    /// fixed at each sweep point, `h_vel = h_pos * (|v0|/|r0|)`, so "step size" below means one
    /// number that sets both consistently), propagates `+h`/`-h` with the model's own plain
    /// `step`, and central-differences. Reports the full sweep and asserts at the h minimising
    /// the disagreement, per this task's own rule ("measure, then record").
    #[test]
    fn stm_matches_a_finite_difference_stm_at_a_measured_step_size() {
        let model = jgm2_8x8_model_with_third_bodies();
        let dt_ns: i64 = 1_800_000_000_000; // 1800 s = 30 min
        let stm_result = model.step_with_stm(&N4_LEO_STATE, N4_T0_TAI_NS, &[], dt_ns).unwrap();
        let phi = &stm_result.phi;

        let r0_scale = (N4_LEO_STATE[0].powi(2) + N4_LEO_STATE[1].powi(2) + N4_LEO_STATE[2].powi(2)).sqrt();
        let v0_scale = (N4_LEO_STATE[3].powi(2) + N4_LEO_STATE[4].powi(2) + N4_LEO_STATE[5].powi(2)).sqrt();

        let mut best_rel = f64::INFINITY;
        let mut best_h_frac = 0.0_f64;
        let sweeps: [f64; 9] = [1e-2, 1e-3, 1e-4, 1e-5, 1e-6, 1e-7, 1e-8, 1e-9, 1e-10];
        for &h_frac in &sweeps {
            let h_pos = h_frac * r0_scale;
            let h_vel = h_frac * v0_scale;
            let mut fd = [0.0_f64; 36]; // fd[i*6+j] = d(x1_i)/d(x0_j)
            for j in 0..6 {
                let h = if j < 3 { h_pos } else { h_vel };
                let mut plus = N4_LEO_STATE;
                let mut minus = N4_LEO_STATE;
                plus[j] += h;
                minus[j] -= h;
                let r_plus = model.step(&plus, N4_T0_TAI_NS, &[], dt_ns).unwrap();
                let r_minus = model.step(&minus, N4_T0_TAI_NS, &[], dt_ns).unwrap();
                for i in 0..6 {
                    fd[i * 6 + j] = (r_plus.state[i] - r_minus.state[i]) / (2.0 * h);
                }
            }
            let mut max_rel = 0.0_f64;
            let mut max_abs = 0.0_f64;
            for k in 0..36 {
                let abs_err = (phi[k] - fd[k]).abs();
                let rel_err = abs_err / phi[k].abs().max(1e-9);
                max_abs = max_abs.max(abs_err);
                max_rel = max_rel.max(rel_err);
            }
            eprintln!("n4-stm-fd-sweep: h_frac={h_frac:e} (h_pos={h_pos:e} m, h_vel={h_vel:e} m/s) max_abs={max_abs:e} max_rel={max_rel:e}");
            if max_rel < best_rel {
                best_rel = max_rel;
                best_h_frac = h_frac;
            }
        }
        eprintln!("n4-stm-fd-best: h_frac={best_h_frac:e} max_rel={best_rel:e}");
        // MEASURED (debug build, this host, `--nocapture`): the sweep bottoms out at
        // h_frac=1e-5 (h_pos=70.2 m, h_vel=0.075 m/s -- both a relative step of 1e-5 against
        // this state's own |r0|~7.02e6 m / |v0|~7.50e3 m/s), max relative disagreement
        // 4.048241612427058e-9 -- truncation error dominates above this h (max_rel grows to
        // 4.1e-3 at h_frac=1e-2), cancellation/round-off error dominates below it (max_rel
        // grows to 5.5e-4 at h_frac=1e-10), the classic finite-difference U-shape. Tolerance
        // set just above the measured best value (this task's own rule): 1e-7, three orders of
        // magnitude of margin.
        assert!(best_rel < 1e-7, "best-of-sweep max relative STM/finite-difference-STM disagreement {best_rel:e} exceeds 1e-7");
    }

    /// **The symplectic property of the two-body STM** (N4's own required test): `Phi^T J Phi
    /// = J`, `J` the standard symplectic form for `[r; v]` phase space (`J = [[0, I], [-I,
    /// 0]]`), on a PURE two-body (point-mass-only, no third bodies/drag/SRP -- the conservative,
    /// Hamiltonian case this property is exact for; drag is dissipative and is NOT symplectic,
    /// ADR-002's third amendment's own `det Phi = 0.99553` on a dissipative arc is exactly the
    /// contrasting data point) arc. Measures the max deviation and asserts at the measured
    /// value, per this task's own rule.
    #[test]
    fn two_body_stm_satisfies_the_symplectic_property() {
        let model = point_mass_model(); // degree 0, order 0 -- pure two-body, no third bodies
        let dt_ns: i64 = 3_600_000_000_000; // 3600 s = 1 h
        let stm_result = model.step_with_stm(&N4_LEO_STATE, N4_T0_TAI_NS, &[], dt_ns).unwrap();
        let phi = &stm_result.phi;

        // J = [[0, I], [-I, 0]], 6x6 row-major.
        let mut j = [0.0_f64; 36];
        for i in 0..3 {
            j[i * 6 + (i + 3)] = 1.0;
            j[(i + 3) * 6 + i] = -1.0;
        }

        // phi_t_j = Phi^T J Phi, 6x6 row-major matrix products, done directly (this is a
        // one-off test computation, not something `crate::stm` needs to expose generally).
        let matmul6 = |a: &[f64], b: &[f64]| -> [f64; 36] {
            let mut out = [0.0_f64; 36];
            for row in 0..6 {
                for col in 0..6 {
                    let mut s = 0.0;
                    for k in 0..6 {
                        s += a[row * 6 + k] * b[k * 6 + col];
                    }
                    out[row * 6 + col] = s;
                }
            }
            out
        };
        let mut phi_t = [0.0_f64; 36];
        for row in 0..6 {
            for col in 0..6 {
                phi_t[row * 6 + col] = phi[col * 6 + row];
            }
        }
        let tmp = matmul6(&phi_t, &j);
        let result = matmul6(&tmp, phi);

        let mut max_dev = 0.0_f64;
        for k in 0..36 {
            max_dev = max_dev.max((result[k] - j[k]).abs());
        }
        eprintln!("n4-symplectic: max |Phi^T J Phi - J| = {max_dev:e} over {dt_ns} ns two-body arc");
        // MEASURED (debug build, this host): 5.872545116858419e-9 over a 3600 s two-body arc.
        // Tolerance set just above it (this task's own rule): 1e-7.
        assert!(max_dev < 1e-7, "Phi^T J Phi - J max deviation {max_dev:e} exceeds 1e-7 on a pure two-body (symplectic) arc");
    }

    /// `det(Phi)` on a conservative (no drag) arc stays near 1 (Liouville) -- measured here on
    /// the SAME force-model shape as `goldens/leo_1day_jgm2_8x8_sunmoon.json`'s own `stm`
    /// block (JGM2 8x8 + Moon + Sun, no drag/SRP), over a shorter (1-hour, GMAT-free) arc so
    /// this unit test stays fast; the full one-day comparison against that golden's own
    /// recorded `det_phi_t1` is `tests/stm_goldens.rs`'s job (Priority 3, `gmat-frames`-gated).
    #[test]
    fn det_phi_stays_near_one_on_a_conservative_arc() {
        let model = jgm2_8x8_model_with_third_bodies();
        let dt_ns: i64 = 3_600_000_000_000; // 3600 s = 1 h
        let stm_result = model.step_with_stm(&N4_LEO_STATE, N4_T0_TAI_NS, &[], dt_ns).unwrap();
        let det = crate::stm::det6(&stm_result.phi);
        eprintln!("n4-det-phi-conservative: det(Phi) = {det:.12} (1h JGM2 8x8 + Moon + Sun arc)");
        // MEASURED (debug build, this host): det(Phi) = 1.000000000001, |det-1| = 1e-12.
        // Tolerance set just above it (this task's own rule): 1e-9.
        assert!((det - 1.0).abs() < 1e-9, "det(Phi) = {det}, expected within 1e-9 of 1 on a conservative arc");
    }

    /// **`StmAugmented` wraps the native model and propagates** (this task's own required
    /// test, Priority 1's own "show that the model satisfies the contract `Kernel::
    /// run_with_covariance` relies on"): drives `EarthGravityModel` through `av_dynamics::
    /// StmAugmented` exactly as `av-kernel`'s covariance path does (`StmAugmented::seed`, then
    /// `step`), and checks `Phi(t0,t0)` is the exact identity through THAT path (not just the
    /// unwrapped model's own `step_with_stm`, `n4_phi_t0_t0_is_the_exact_identity` above) plus
    /// that a real (non-zero `dt`) step composes correctly.
    #[test]
    fn stm_augmented_wraps_the_native_model_and_propagates() {
        let model = jgm2_8x8_model_with_third_bodies();
        let seed = av_dynamics::StmAugmented::<EarthGravityModel<IdentityRotation>>::seed(&N4_LEO_STATE);
        let wrapped = av_dynamics::StmAugmented::new(model);

        // Phi(t0,t0) through StmAugmented::step with dt=0.
        let zero = wrapped.step(&seed, N4_T0_TAI_NS, &[], 0).unwrap();
        let (_mean0, phi0) = zero.state.split_at(6);
        for i in 0..6 {
            for j in 0..6 {
                let want = if i == j { 1.0 } else { 0.0 };
                assert_eq!(phi0[i * 6 + j], want, "Phi(t0,t0) through StmAugmented::step must be the exact identity: [{i}][{j}]");
            }
        }

        // A real step: StmAugmented's own composed Phi(t0, t0+dt) must match the unwrapped
        // model's own step_with_stm answer directly (one native period, so composition against
        // the identity seed is exact -- mirrors `av-dynamics`'s own
        // `stm_augmented_step_delegates_to_step_with_stm_and_carries_its_outputs` precedent).
        let dt_ns: i64 = 1_800_000_000_000;
        let stepped = wrapped.step(&seed, N4_T0_TAI_NS, &[], dt_ns).unwrap();
        let (mean1, phi1) = stepped.state.split_at(6);

        let direct = wrapped.inner().step_with_stm(&N4_LEO_STATE, N4_T0_TAI_NS, &[], dt_ns).unwrap();
        for (i, (got, want)) in mean1.iter().zip(direct.state.iter()).enumerate() {
            let rel = (got - want).abs() / want.abs().max(1.0);
            assert!(rel < 1e-9, "StmAugmented::step's own mean disagrees with step_with_stm directly: component {i}, rel={rel:e}");
        }
        for (k, (got, want)) in phi1.iter().zip(direct.phi.iter()).enumerate() {
            let rel = (got - want).abs() / want.abs().max(1e-6);
            assert!(rel < 1e-9, "StmAugmented::step's own Phi disagrees with step_with_stm directly: index {k}, rel={rel:e}");
        }
    }

    /// SRP's velocity block must be EXACTLY zero (N4's own required pin, ADR-002's third
    /// amendment: "`SolarRadiationPressure` ... velocity block correctly zero") -- a model
    /// with SRP configured but no drag has `da_dv` entirely from SRP (nothing else touches
    /// it), so this checks the full `acceleration_partials` output, not merely
    /// `cannonball_acceleration`'s own isolated `Dual3` seeding.
    #[test]
    fn srp_velocity_block_is_exactly_zero() {
        let model = point_mass_model_with_third_bodies()
            .with_srp(crate::srp::SrpConstants::gmat_earth_defaults(), 5.0, 1.8, 500.0)
            .expect("with_srp");
        let pos = [N4_LEO_STATE[0], N4_LEO_STATE[1], N4_LEO_STATE[2]];
        let vel = [N4_LEO_STATE[3], N4_LEO_STATE[4], N4_LEO_STATE[5]];
        let (_da_dr, da_dv) = model.acceleration_partials(pos, vel, N4_T0_TAI_NS).expect("acceleration_partials");
        for row in da_dv {
            for v in row {
                assert_eq!(v, 0.0, "SRP's own velocity block must be EXACTLY zero, not merely small");
            }
        }
    }

    /// **Drag's position-block finite-difference step size, measured by a sweep against a
    /// Richardson-extrapolated reference, not assumed** (N4's own required measurement).
    /// [`DRAG_POSITION_FD_STEP_M`] is a fixed constant, so this test does not re-derive it (a
    /// constant cannot depend on a test's own runtime measurement) -- it is the record of the
    /// sweep that justified the value chosen, exactly per this task's own rule ("record the
    /// step size you chose and the measurement that justifies it").
    ///
    /// The reference: `D(h) = (a(pos+h) - a(pos-h)) / 2h` is second-order accurate
    /// (`D(h) = f'(x) + c*h^2 + O(h^4)`), so Richardson extrapolation at a FINE base step,
    /// `R = (4*D(h0) - D(2*h0))/3`, cancels the leading `h^2` term and is `O(h0^4)` accurate --
    /// a strictly better reference than any single central-difference estimate in the sweep,
    /// obtained from the SAME [`EarthGravityModel::drag_acceleration_only`] function the sweep
    /// itself calls (no independent analytic drag-position-partial exists to check against;
    /// this is the standard numerical-differentiation technique for exactly that situation).
    /// `h0` is chosen well below the coarser sweep points swept against it, so the two are not
    /// circular.
    #[test]
    fn drag_position_fd_step_size_sweep() {
        let weather = crate::jacchia_roberts::WeatherInputs::from(crate::weather::ConstantWeather::gmat_defaults());
        let model = point_mass_model_with_third_bodies().with_drag(AtmosphereChoice::JacchiaRoberts, weather, 5.0, 2.2, 500.0).expect("with_drag");
        let pos = [N4_LEO_STATE[0], N4_LEO_STATE[1], N4_LEO_STATE[2]];
        let vel = [N4_LEO_STATE[3], N4_LEO_STATE[4], N4_LEO_STATE[5]];

        let compute = |h: f64| -> [[f64; 3]; 3] {
            let mut out = [[0.0_f64; 3]; 3];
            for j in 0..3 {
                let mut plus = pos;
                let mut minus = pos;
                plus[j] += h;
                minus[j] -= h;
                let a_plus = model.drag_acceleration_only(plus, vel, N4_T0_TAI_NS).expect("drag_acceleration_only(+h)");
                let a_minus = model.drag_acceleration_only(minus, vel, N4_T0_TAI_NS).expect("drag_acceleration_only(-h)");
                for i in 0..3 {
                    out[i][j] = (a_plus[i] - a_minus[i]) / (2.0 * h);
                }
            }
            out
        };

        let h0 = 0.05; // m -- fine base step for the Richardson reference
        let d_h0 = compute(h0);
        let d_2h0 = compute(2.0 * h0);
        let mut reference = [[0.0_f64; 3]; 3];
        for i in 0..3 {
            for j in 0..3 {
                reference[i][j] = (4.0 * d_h0[i][j] - d_2h0[i][j]) / 3.0;
            }
        }
        let ref_scale = reference.iter().flatten().map(|v| v * v).sum::<f64>().sqrt();
        eprintln!("n4-drag-fd-richardson-reference: h0={h0} reference_frobenius_scale={ref_scale:e}");

        let sweeps: [f64; 10] = [0.5, 1.0, 2.0, 5.0, 10.0, 50.0, 100.0, 500.0, 1_000.0, 10_000.0];
        let mut best_h = 0.0_f64;
        let mut best_rel = f64::INFINITY;
        for &h in &sweeps {
            let d = compute(h);
            let mut diff_sq = 0.0_f64;
            for i in 0..3 {
                for j in 0..3 {
                    diff_sq += (d[i][j] - reference[i][j]).powi(2);
                }
            }
            let rel = diff_sq.sqrt() / ref_scale;
            eprintln!("n4-drag-fd-sweep: h={h:e} m, max_component={:e}, rel_vs_richardson={rel:e}", d.iter().flatten().fold(0.0_f64, |m, v| m.max(v.abs())));
            if rel < best_rel {
                best_rel = rel;
                best_h = h;
            }
        }
        eprintln!("n4-drag-fd-best: h={best_h:e} m, rel_vs_richardson={best_rel:e}; DRAG_POSITION_FD_STEP_M is set to {DRAG_POSITION_FD_STEP_M:e} m -- see this crate's own N4 report for the full sweep table and host state.");
        // The sweep is diagnostic (records the measurement `DRAG_POSITION_FD_STEP_M` was
        // chosen from, per this task's own rule) rather than a pass/fail gate on the constant
        // itself -- `DRAG_POSITION_FD_STEP_M` is fixed at compile time, not derived from this
        // run. What IS asserted: the best-of-sweep estimate agrees with the independent
        // Richardson reference to a tight relative tolerance, confirming the sweep genuinely
        // found a low-error region (not merely the least-bad of a uniformly poor set).
        assert!(best_rel < 1e-4, "even the best-of-sweep finite-difference estimate disagrees with the Richardson-extrapolated reference by {best_rel:e} -- suspect the drag acceleration is not smooth enough here for any fixed step to work well");
    }

    /// Drag's ANALYTIC velocity block (`Dual3`-seeded `drag_acceleration_core`, inside
    /// [`EarthGravityModel::acceleration_partials`]) against a plain central finite difference
    /// of [`EarthGravityModel::drag_acceleration_only`] with respect to velocity (position
    /// held fixed) -- an independent cross-check that the analytic block is correct, not merely
    /// self-consistent with itself.
    #[test]
    fn drag_velocity_block_matches_a_finite_difference_of_drag_acceleration_only() {
        let weather = crate::jacchia_roberts::WeatherInputs::from(crate::weather::ConstantWeather::gmat_defaults());
        let model = point_mass_model_with_third_bodies().with_drag(AtmosphereChoice::JacchiaRoberts, weather, 5.0, 2.2, 500.0).expect("with_drag");
        let pos = [N4_LEO_STATE[0], N4_LEO_STATE[1], N4_LEO_STATE[2]];
        let vel = [N4_LEO_STATE[3], N4_LEO_STATE[4], N4_LEO_STATE[5]];

        let (_da_dr, da_dv_analytic) = model.acceleration_partials(pos, vel, N4_T0_TAI_NS).expect("acceleration_partials");

        let h = 1e-3; // m/s; |v0| ~ 7.5e3 m/s, relative step ~1.3e-7
        let mut fd = [[0.0_f64; 3]; 3];
        for j in 0..3 {
            let mut plus = vel;
            let mut minus = vel;
            plus[j] += h;
            minus[j] -= h;
            let a_plus = model.drag_acceleration_only(pos, plus, N4_T0_TAI_NS).expect("drag_acceleration_only(+h)");
            let a_minus = model.drag_acceleration_only(pos, minus, N4_T0_TAI_NS).expect("drag_acceleration_only(-h)");
            for i in 0..3 {
                fd[i][j] = (a_plus[i] - a_minus[i]) / (2.0 * h);
            }
        }

        let scale = da_dv_analytic.iter().flatten().map(|v| v * v).sum::<f64>().sqrt();
        let mut diff_sq = 0.0_f64;
        for i in 0..3 {
            for j in 0..3 {
                diff_sq += (da_dv_analytic[i][j] - fd[i][j]).powi(2);
            }
        }
        let rel = diff_sq.sqrt() / scale;
        eprintln!("n4-drag-velocity-block-fd-check: rel_diff={rel:e} h={h} scale={scale:e}");
        assert!(rel < 1e-6, "analytic drag velocity block disagrees with a finite difference of drag_acceleration_only by {rel:e} relative");
    }

    /// A model with NEITHER SRP nor drag configured has `da_dv` exactly zero (gravity and
    /// third bodies have no velocity dependence at all) -- the baseline
    /// `srp_velocity_block_is_exactly_zero` and drag's own velocity-block tests (`tests/
    /// stm_goldens.rs`, Priority 2/3) are contrasted against.
    #[test]
    fn gravity_and_third_body_only_velocity_block_is_exactly_zero() {
        let model = jgm2_8x8_model_with_third_bodies();
        let pos = [N4_LEO_STATE[0], N4_LEO_STATE[1], N4_LEO_STATE[2]];
        let vel = [N4_LEO_STATE[3], N4_LEO_STATE[4], N4_LEO_STATE[5]];
        let (_da_dr, da_dv) = model.acceleration_partials(pos, vel, N4_T0_TAI_NS).expect("acceleration_partials");
        for row in da_dv {
            for v in row {
                assert_eq!(v, 0.0);
            }
        }
    }
}
