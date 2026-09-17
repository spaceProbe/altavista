//! The native orbital force model behind ADR-002's depth 3, "native Rust models, with GMAT as
//! the oracle" (`docs/native-dynamics-plan.md`, milestone N1).
//!
//! Built in two halves. The first (the GMAT-free numerical core, still true of every module
//! below except [`frame_gmat`]): a `.cof` gravity-file reader, a normalised associated
//! Legendre recursion stable to degree 70, and point-mass/spherical-harmonic gravity
//! acceleration with analytic partials in the body-fixed frame -- pinned against closed-form
//! or published references and fully tested on its own, touching neither `DynamicsModel` nor
//! GMAT. The second (this task): the body-fixed-to-inertial rotation through the frame
//! registry's GMAT-validated `convert` shim (charter decision 222(c)), the
//! `av_dynamics::DynamicsModel` binding for a single spacecraft, and the two-body analytic
//! golden that needs no GMAT at all.
//!
//! Five pieces:
//!
//! - [`cof`]: a reader for GMAT's own `.cof` Earth gravity-coefficient files (`JGM2`,
//!   `JGM3`, `EGM96`, `EGM96low`, `JGM2F70`, all shipped under
//!   `$GMAT_ROOT/data/gravity/earth/`), returning `mu`, the reference radius and the
//!   fully-normalised `C`/`S` coefficient arrays to a requested degree and order.
//! - [`legendre`]: the fully-normalised associated Legendre functions `P̄_nm(sin φ)` used in
//!   spherical-harmonic gravity, by a recursion that stays numerically stable to degree 70
//!   and well beyond (never forming an unnormalised intermediate value).
//! - [`gravity`]: point-mass and spherical-harmonic gravity acceleration in the body-fixed
//!   frame, plus the exact 3x3 partial-derivative (gravity-gradient) matrix, computed by a
//!   pole-free (Cunningham/Gottlieb) recursion in body-fixed Cartesian coordinates -- no
//!   latitude, no longitude, no `1/cos(latitude)` term anywhere, so the poles are not a
//!   special case.
//! - [`frame`]: [`frame::BodyFixedRotation`], the trait [`model::EarthGravityModel`] is
//!   generic over, and [`frame::Rotation`], its result -- always compiled, no GMAT
//!   dependency, the seam N5's native frame reduction will implement against instead of
//!   [`frame_gmat::GmatBodyFixedRotation`].
//! - [`frame_gmat`] (behind the `gmat-frames` cargo feature, default-on):
//!   [`frame_gmat::GmatBodyFixedRotation`], the GMAT-backed [`frame::BodyFixedRotation`] --
//!   see that module's own doc comment for the exact `Gmat::convert_with_rotation` call.
//! - [`model`]: [`model::EarthGravityModel`], the `av_dynamics::DynamicsModel` implementation
//!   for one spacecraft under Earth point-mass/spherical-harmonic gravity.
//!
//! N2 (`docs/native-dynamics-plan.md`, third bodies) adds three more, all GMAT-free (no
//! `gmat-frames` gate, so `--no-default-features` still carries them):
//!
//! - [`de`]: [`de::DeEphemeris`], a reader for GMAT's own JPL DE binary planetary/lunar
//!   ephemeris files (`$GMAT_ROOT/data/planetary_ephem/de/leDE*.4xx`) -- Chebyshev evaluation
//!   of a body's position relative to Earth at a TDB epoch, and each body's `mu` from the
//!   file's own constants. See that module's doc for which file GMAT actually uses (measured,
//!   not assumed) and every header cross-check.
//! - [`tdb`]: TAI -> TDB (Barycentric Dynamical Time, the scale DE ephemerides are tabulated
//!   in), sourced from GMAT's own `TimeSystemConverter` constants and measured against GMAT's
//!   own conversion -- see that module's doc. Deliberately NOT added to `av_cdm::time::Tai`
//!   (this crate's own brief: `av-cdm` is shared with other tracks and not this round's to
//!   extend).
//! - [`third_body`]: [`third_body::third_body_acceleration`], the numerically robust
//!   ("Battin") form of the third-body point-mass perturbation, derived and verified in that
//!   module's own doc comment rather than quoted from memory.
//!
//! [`model::EarthGravityModel::with_third_bodies`] wires all three together: additive to N1
//! (a model with no third bodies bound behaves bit-for-bit as before -- see that method's own
//! doc comment).
//!
//! N3 (`docs/native-dynamics-plan.md`, solar radiation pressure) adds one more, also GMAT-free:
//!
//! - [`srp`]: cannonball SRP with a conical (umbra + penumbra) shadow model, Earth as the sole
//!   occulting body -- see that module's own doc for the formula, every constant and where it
//!   was read off a live GMAT instance, and the shape it leaves for N4's partials.
//!
//! [`model::EarthGravityModel::with_srp`] wires it in, additive to N1/N2 (a model with no SRP
//! bound behaves bit-for-bit as before) and REQUIRES [`model::EarthGravityModel::
//! with_third_bodies`] to already be configured -- see that method's own doc comment for why.
//!
//! # Units
//!
//! Every public function in [`cof`]/[`legendre`]/[`gravity`] works in SI: metres, seconds, and
//! `m^3/s^2` for `mu`. GMAT's own `.cof` files are *themselves* already stored in SI
//! (`data/gravity/earth/*.cof`'s `POTFIELD` record: `mu = 3.986004415e14` m^3/s^2, reference
//! radius `6.3781363e6` m -- see [`cof`]'s module doc for how this was verified), so
//! [`cof::read_earth_gravity`] performs no unit conversion at all. [`model::EarthGravityModel`]
//! carries the same SI convention all the way out to `DynamicsModel::derivatives`/`state` --
//! see [`model`]'s own module doc, "Units", for exactly how this was matched to
//! `gmat_sys::model::GmatModel`'s SI boundary.
//!
//! # Dependencies
//!
//! `thiserror` (typed errors, no panics on malformed input); `openssl` (SHA-256 over the
//! system OpenSSL, ADR-004's crypto rule -- never `sha2`, never any other crypto crate; used by
//! the `.cof` data-pack hash test in `src/cof.rs` AND, as of this task, by
//! [`model::EarthGravityModel::new`]'s own gravity-file digest that feeds
//! `describe().settings_hash`); `av-dynamics` (the `DynamicsModel` trait, `Dopri5`,
//! `settings_hash` -- itself GMAT-free, so depending on it does not compromise the
//! `--no-default-features` build below); `av-cdm` (`ModelInfo`/`ModelCapability`, `Tai` for the
//! TAI<->A.1 epoch conversion `frame_gmat` needs -- also GMAT-free). `gmat-sys` is an
//! OPTIONAL dependency, pulled in only by the `gmat-frames` feature (default-on): `cargo build
//! -p av-orbital --no-default-features` never touches it, and [`cof`]/[`legendre`]/[`gravity`]/
//! [`frame`]/[`model`] (everything except [`frame_gmat`] itself) still build and unit-test
//! cleanly without it -- see this crate's own N1 report for the verified command and output.

pub mod cof;
pub mod de;
pub mod dual;
pub mod frame;
#[cfg(feature = "gmat-frames")]
pub mod frame_gmat;
pub mod gravity;
pub mod legendre;
pub mod model;
pub mod srp;
pub mod tdb;
pub mod third_body;

pub use cof::{CofError, GravityModel};
pub use de::{DeBody, DeEphemeris, DeError};
pub use frame::{BodyFixedRotation, Rotation};
#[cfg(feature = "gmat-frames")]
pub use frame_gmat::GmatBodyFixedRotation;
pub use gravity::{point_mass_acceleration, point_mass_partials, spherical_harmonic_gravity};
pub use legendre::NormalizedLegendre;
pub use model::{EarthGravityModel, EarthGravityModelInfo, OrbitalModelError};
pub use srp::{SrpConstants, SrpError, SrpProperties};
pub use third_body::third_body_acceleration;
