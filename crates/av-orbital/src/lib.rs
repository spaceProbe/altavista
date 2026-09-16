//! N1's GMAT-free numerical core (`docs/native-dynamics-plan.md`, milestone N1; ADR-002's
//! depth 3, "native Rust models, with GMAT as the oracle").
//!
//! This crate is the *first half* of N1: the pure, GMAT-free numerical pieces of a native
//! Earth gravity force model, each pinned against a closed-form or published reference and
//! fully tested on its own. It deliberately does **not** implement `DynamicsModel`, does
//! **not** rotate anything between frames, and does **not** touch GMAT or `gmat-sys` -- a
//! second worker adds the body-fixed-to-inertial rotation through the frame registry's
//! convert shim, wires this crate into `DynamicsModel`, and adds the GMAT goldens.
//!
//! Three pieces:
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
//!
//! # Units
//!
//! Every public function in this crate works in SI: metres, seconds, and `m^3/s^2` for `mu`.
//! GMAT's own `.cof` files are *themselves* already stored in SI (`data/gravity/earth/*.cof`'s
//! `POTFIELD` record: `mu = 3.986004415e14` m^3/s^2, reference radius `6.3781363e6` m -- see
//! [`cof`]'s module doc for how this was verified), so [`cof::read_earth_gravity`] performs no
//! unit conversion at all; it is documented here because the rest of this workspace's GMAT
//! shim (`crates/gmat-sys`) and goldens work in kilometres, and the next worker's frame-
//! rotation half is the boundary where that conversion must happen, not this crate.
//!
//! # Dependencies
//!
//! Only `thiserror` (typed errors, no panics on malformed input) and `openssl` (SHA-256 over
//! the system OpenSSL, ADR-004's crypto rule -- never `sha2`, never any other crypto crate;
//! used solely by the `.cof` data-pack hash test in `src/cof.rs`). No `av-dynamics`
//! dependency: this half of N1 never touches `DynamicsModel`, `ErasedModel` or CDM state
//! types, so there is nothing in `av-dynamics` for it to use (see `Cargo.toml`'s doc comment
//! for the full reasoning). No `gmat-sys`, so a workspace build that omits `gmat-sys`
//! (no GMAT linked) still builds this crate.

pub mod cof;
pub mod dual;
pub mod gravity;
pub mod legendre;

pub use cof::{CofError, GravityModel};
pub use gravity::{point_mass_acceleration, point_mass_partials, spherical_harmonic_gravity};
pub use legendre::NormalizedLegendre;
