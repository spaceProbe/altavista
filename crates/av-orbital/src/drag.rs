//! The drag acceleration itself: `a = -0.5 * (Cd * A / m) * rho * |v_rel| * v_rel`, with the
//! **rotating-atmosphere** relative velocity `v_rel = v - omega x r` (`docs/
//! native-dynamics-plan.md` milestone N3, task 3b), reproducing GMAT's own
//! `DragForce::Accelerate`/`BuildPrefactors`
//! (`third_party/gmat-src/src/base/forcemodel/DragForce.cpp`), read from GMAT's own source
//! rather than from memory or a textbook, term for term:
//!
//! ```text
//! vRelative[0] = theState[3] - (angVel[1]*theState[2] - angVel[2]*theState[1])
//! vRelative[1] = theState[4] - (angVel[2]*theState[0] - angVel[0]*theState[2])
//! vRelative[2] = theState[5] - (angVel[0]*theState[1] - angVel[1]*theState[0])
//! vRelMag      = |vRelative|
//! factor       = prefactor * density                    // prefactor = -500 * Cd * A / m
//! accel        = factor * vRelMag * vRelative
//! ```
//!
//! (`DragForce::Accelerate`, the `hasWindModel == false` branch -- this crate has no wind
//! model, matching every existing golden's own force model). `-500` is GMAT's own
//! `BuildPrefactors`: `prefactor[i] = -500.0 * dragCoeff[i] * area[i] / mass[i]`, its own
//! comment stating exactly why: `"-0.5 * Cd * A / m"`, scaled by `*1000` because GMAT's own
//! state is km/km-s while density is kg/m^3 (`"Prefactor is scaled to account for density in
//! kg/m^3 (*1000/2)"`). This crate is SI throughout (`crate::model`'s own "Units" doc
//! section), so the formula this module implements is the UN-scaled textbook form directly:
//! `a[m/s^2] = -0.5 * (Cd*A/m) * rho[kg/m^3] * |v_rel|[m/s] * v_rel[m/s]` -- dimensionally
//! identical to GMAT's `-500 * ... ` once GMAT's own km/km-s state and `*1000` factor are
//! accounted for (`-0.5 * 1000 = -500`, verified by unit analysis in this module's own doc,
//! not merely asserted).
//!
//! # Which omega, and whether it is the full body-fixed rotation
//!
//! **A single SCALAR rate about the z-axis, `angVel = [0, 0, 7.292115855e-5]` rad/s -- NOT
//! the full 3x3 body-fixed rotation [`crate::frame::BodyFixedRotation`] uses for gravity.**
//! `AtmosphereModel`'s own constructor (`third_party/gmat-src/src/base/solarsys/
//! AtmosphereModel.cpp`) hardcodes exactly this: `angVel[0] = 0.0; angVel[1] = 0.0; angVel[2]
//! = 7.29211585530e-5;` ("Default to nominal Earth angular velocity") and `DragForce` reads
//! this SAME three-element array (`atmos->GetAngularVelocity()`) as its own `angVel` pointer
//! -- there is no further update anywhere in `DragForce::Accelerate`'s own call path that
//! would replace it with a true (precessing/nutating/polar-motion-corrected) body angular
//! velocity vector. **And the cross product is formed directly on `theState[0..3]`, the
//! spacecraft's INERTIAL (central-body MJ2000Eq) position -- never a body-fixed one.** This
//! is the classic "rotating atmosphere" approximation (a uniformly-rotating Earth with a
//! fixed axis and fixed rate, applied directly in the inertial frame, exactly as if
//! `EarthMJ2000Eq` and `EarthFixed` shared the same z-axis and differed only by
//! Greenwich-hour-angle-rate rotation) -- distinct from, and simpler than, the true
//! [`crate::frame::BodyFixedRotation`] this crate's gravity term uses (which the density
//! evaluation itself DOES need, for the geodetic height/latitude that determines the
//! atmosphere's height-banded density profile -- see [`crate::jacchia_roberts`]'s own doc
//! comment, "Geodetic height and latitude"). [`EARTH_ANGULAR_VELOCITY_RAD_S`] below is this
//! same GMAT constant, and [`relative_velocity`] forms the cross product on the caller's
//! inertial position/velocity directly, matching this reading exactly.
//!
//! # Whether density is evaluated at the body-fixed position
//!
//! Yes -- `DragForce::GetDensity` passes the raw (inertial) `theState` into
//! `atmos->Density(...)`, but `JacchiaRobertsAtmosphere::Density` itself converts that
//! position into the central body's body-fixed frame internally
//! (`AtmosphereModel::CalculateGeodetics`'s own `CoordinateConverter::Convert(..., cbFixed)`)
//! before computing the geodetic height/latitude the density profile is keyed on -- see
//! [`crate::jacchia_roberts`]'s own doc comment for the full account. So density IS evaluated
//! at the true body-fixed position (correctly capturing atmospheric rotation for the DENSITY
//! itself), while the relative-velocity cross product above uses the SIMPLER scalar-omega
//! approximation, evaluated in the inertial frame -- two different fidelities for two
//! different terms of the same force, exactly as GMAT's own source shows, not a simplification
//! this crate introduced.
//!
//! # Shape for N4: generic in VELOCITY, not position (a deliberate, documented choice)
//!
//! ADR-002's third amendment already recorded, from GMAT's own source, that `DragForce` fills
//! BOTH the position and velocity blocks of the A-matrix by FINITE-DIFFERENCING its own
//! acceleration internally (`GetDerivatives`, "GMAT finite-differences its own acceleration
//! internally") -- unlike `SolarRadiationPressure`, which fills its position block
//! analytically. So, unlike [`crate::srp::cannonball_acceleration`] (generic in position
//! only, mirroring `gravity.rs`'s own shape), this module makes [`drag_acceleration`] generic
//! in **`v_rel_m_s` (velocity) only** -- `T:` [`crate::dual::GravScalar`] -- while position
//! (via `omega x r`, folded into `v_rel_m_s` by the CALLER, [`relative_velocity`], which stays
//! `f64`-only) and density stay frozen `f64` constants, lifted via `T::constant` inside. This
//! is a deliberate choice, not a limitation copied from `srp.rs`: [`crate::dual::Dual3`]
//! carries exactly three tangent directions, and this crate's existing convention
//! (`gravity.rs`, `srp.rs`) always seeds all three with the SAME kind of quantity (position);
//! reusing that shape for velocity here gives N4 the VELOCITY block of the A-matrix
//! ANALYTICALLY (`d(accel)/d(v)`, which [`drag_acceleration`]'s own `|v_rel|*v_rel` term makes
//! exact and cheap via forward-mode AD) for free, while the POSITION block -- which needs
//! density's own altitude/latitude dependence, a much larger and non-analytic term this
//! module deliberately does NOT differentiate (matching this module's own "density stays a
//! frozen `f64`" rule) -- is left to N4 to finite-difference, EXACTLY as GMAT itself does for
//! that block (so a finite-difference position block here does not fall short of GMAT's own
//! fidelity, only matches it). A future `Dual6` (position AND velocity, six tangent
//! directions) would let N4 get both blocks analytically at once; this module does not build
//! one, since GMAT's own precedent (finite-differencing the WHOLE A-matrix, not analytically
//! deriving either block) means a `Dual6` would exceed what this task's own golden needs to
//! validate against, not merely match it.
//!
//! # No panics, typed errors throughout
//!
//! Every function here returns [`DragError`] rather than panicking on any input a DRM could
//! supply (this crate's own rule) -- non-finite state, non-positive mass/area, non-finite
//! `Cd`, and a non-finite computed result are all typed errors.
//!
//! # GMAT-free
//!
//! Like `srp.rs`/`gravity.rs`, this module has no `gmat-sys` dependency and builds and
//! unit-tests under `cargo test -p av-orbital --no-default-features`.

use crate::dual::GravScalar;

/// GMAT's own default Earth angular velocity, rad/s -- `AtmosphereModel`'s own constructor
/// (`angVel[2] = 7.29211585530e-5`), see this module's own doc comment for exactly how
/// `DragForce` uses it (a scalar z-axis rate, not the full body-fixed rotation).
pub const EARTH_ANGULAR_VELOCITY_RAD_S: f64 = 7.292_115_855_30e-5;

/// Every way this module's functions can fail. Typed throughout -- no panic on any input a
/// DRM could supply.
#[derive(Debug, Clone, Copy, thiserror::Error, PartialEq)]
pub enum DragError {
    /// The spacecraft position was not finite.
    #[error("spacecraft position is not finite: {0:?}")]
    NonFinitePosition([f64; 3]),
    /// The spacecraft velocity was not finite.
    #[error("spacecraft velocity is not finite: {0:?}")]
    NonFiniteVelocity([f64; 3]),
    /// `DragArea` must be positive and finite.
    #[error("drag area must be positive and finite, got {0} m^2")]
    InvalidArea(f64),
    /// The spacecraft's total mass must be positive and finite.
    #[error("spacecraft mass must be positive and finite, got {0} kg")]
    InvalidMass(f64),
    /// `Cd` must be finite.
    #[error("Cd must be finite, got {0}")]
    InvalidCd(f64),
    /// The density passed in was not finite or was negative (a negative density is always
    /// unphysical, unlike `Cr` in `srp.rs`, which this crate deliberately leaves unconstrained
    /// in sign).
    #[error("density must be non-negative and finite, got {0} kg/m^3")]
    InvalidDensity(f64),
    /// The computed acceleration was not finite despite every input passing the checks above.
    #[error("computed drag acceleration is not finite: {0:?}")]
    NonFiniteResult([f64; 3]),
}

/// This vehicle's ballistic drag properties (question 81: "a seed is a vehicle") --
/// `DragArea`/`Cd`/total mass, SI (m^2, dimensionless, kg). `mass_kg` is GMAT's own
/// `mass[i]` (`DragForce::BuildPrefactors`'s own `mass[i] = sc->GetRealParameter(massID)`,
/// which reads whichever mass parameter the force model bound -- `TotalMass` in every
/// golden this crate's tests use, matching [`crate::srp::SrpProperties`]'s identical
/// convention).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DragProperties {
    pub cd: f64,
    pub area_m2: f64,
    pub mass_kg: f64,
}

/// `v_rel = v - omega x r`, GMAT's own rotating-atmosphere relative velocity -- `omega =
/// [0, 0, omega_z_rad_s]` (see this module's own doc comment for why this is a scalar z-axis
/// rate, not the full body-fixed rotation), applied directly to the caller's INERTIAL
/// position `r_m`/velocity `v_m` (never rotated into body-fixed first -- matching
/// `DragForce::Accelerate`'s own `theState[0..6]`, which is the ODEModel's raw inertial
/// state). With `omega_x = omega_y = 0`, `omega x r = (-omega_z*r_y, omega_z*r_x, 0)`
/// (`DragForce::Accelerate`'s own general 3-component cross product, specialised here to the
/// scalar-z-rate case this module's own doc comment establishes GMAT actually uses -- the two
/// terms this specialisation drops, `angVel[1]*theState[2]` and `angVel[0]*theState[2]`, are
/// each multiplied by an `angVel` component that is always exactly zero in GMAT's own default,
/// confirmed live).
pub fn relative_velocity(r_m: [f64; 3], v_m: [f64; 3], omega_z_rad_s: f64) -> [f64; 3] {
    [v_m[0] + omega_z_rad_s * r_m[1], v_m[1] - omega_z_rad_s * r_m[0], v_m[2]]
}

/// The drag acceleration's own core term, `-0.5*(Cd*A/m)*rho * |v_rel| * v_rel`, generic over
/// `T: GravScalar` **only in `v_rel_m_s`** -- see this module's own doc comment, "Shape for
/// N4", for why velocity (not position) is the differentiated quantity here. `rho_kg_m3`/`k =
/// -0.5*Cd*A/m` are both plain `f64`, lifted via `T::constant` inside.
pub fn drag_acceleration_core<T: GravScalar>(v_rel_m_s: [T; 3], rho_kg_m3: f64, k: f64) -> [T; 3] {
    let vmag2 = v_rel_m_s[0] * v_rel_m_s[0] + v_rel_m_s[1] * v_rel_m_s[1] + v_rel_m_s[2] * v_rel_m_s[2];
    let vmag = vmag2.sqrt();
    let scale = T::constant(k * rho_kg_m3);
    [v_rel_m_s[0] * vmag * scale, v_rel_m_s[1] * vmag * scale, v_rel_m_s[2] * vmag * scale]
}

/// The full drag acceleration at spacecraft position `r_m`/velocity `v_m` (both inertial,
/// central-body-relative), atmospheric density `rho_kg_m3` (already evaluated at this
/// instant -- [`crate::jacchia_roberts::density_kg_m3`]'s job, not this function's), Earth's
/// rotation rate and this vehicle's ballistic properties. Computes `v_rel` via
/// [`relative_velocity`] then the core term via [`drag_acceleration_core`], matching this
/// module's own documented formula exactly. No panics: every input this crate's own rule
/// names is a typed [`DragError`], and the output is checked finite as a last-resort guard.
pub fn drag_acceleration(r_m: [f64; 3], v_m: [f64; 3], rho_kg_m3: f64, omega_z_rad_s: f64, props: &DragProperties) -> Result<[f64; 3], DragError> {
    if !r_m.iter().all(|v| v.is_finite()) {
        return Err(DragError::NonFinitePosition(r_m));
    }
    if !v_m.iter().all(|v| v.is_finite()) {
        return Err(DragError::NonFiniteVelocity(v_m));
    }
    if !(props.area_m2.is_finite() && props.area_m2 > 0.0) {
        return Err(DragError::InvalidArea(props.area_m2));
    }
    if !(props.mass_kg.is_finite() && props.mass_kg > 0.0) {
        return Err(DragError::InvalidMass(props.mass_kg));
    }
    if !props.cd.is_finite() {
        return Err(DragError::InvalidCd(props.cd));
    }
    if !(rho_kg_m3.is_finite() && rho_kg_m3 >= 0.0) {
        return Err(DragError::InvalidDensity(rho_kg_m3));
    }

    let v_rel = relative_velocity(r_m, v_m, omega_z_rad_s);
    let k = -0.5 * props.cd * props.area_m2 / props.mass_kg;
    let accel = drag_acceleration_core::<f64>(v_rel, rho_kg_m3, k);
    if !accel.iter().all(|v| v.is_finite()) {
        return Err(DragError::NonFiniteResult(accel));
    }
    Ok(accel)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEO_R_M: f64 = 6_878_000.0;
    const LEO_V_M_S: f64 = 7_612.0; // roughly circular-orbit speed at this radius

    fn props() -> DragProperties {
        DragProperties { cd: 2.2, area_m2: 5.0, mass_kg: 500.0 }
    }

    /// At zero rotation rate, `v_rel == v` exactly.
    #[test]
    fn relative_velocity_with_zero_omega_equals_velocity_exactly() {
        let r = [LEO_R_M, 0.0, 0.0];
        let v = [0.0, LEO_V_M_S, 100.0];
        let v_rel = relative_velocity(r, v, 0.0);
        assert_eq!(v_rel, v);
    }

    /// A spacecraft on the +x axis with Earth rotating about +z: `omega x r = [0,
    /// omega*r, 0]`, so `v_rel = v - [0, omega*r, 0]` -- checked against the textbook cross
    /// product computed independently.
    #[test]
    fn relative_velocity_matches_the_textbook_cross_product() {
        let r = [LEO_R_M, 0.0, 0.0];
        let v = [0.0, LEO_V_M_S, 0.0];
        let omega = EARTH_ANGULAR_VELOCITY_RAD_S;
        let v_rel = relative_velocity(r, v, omega);
        // omega x r = (0,0,omega) x (r,0,0) = (0*0 - omega*0, omega*r - 0*0, 0*0 - 0*r) = (0, omega*r, 0)
        let expected = [0.0, LEO_V_M_S - omega * LEO_R_M, 0.0];
        for i in 0..3 {
            assert!((v_rel[i] - expected[i]).abs() < 1e-9, "component {i}: {} vs {}", v_rel[i], expected[i]);
        }
    }

    /// The core acceleration must be antiparallel to `v_rel` (drag always opposes relative
    /// motion) and have magnitude `0.5*Cd*A/m*rho*|v_rel|^2`.
    #[test]
    fn drag_acceleration_opposes_relative_velocity_with_the_right_magnitude() {
        let r = [LEO_R_M, 0.0, 0.0];
        let v = [0.0, LEO_V_M_S, 0.0];
        let rho = 1e-12; // kg/m^3, a plausible ~400 km density
        let p = props();
        let a = drag_acceleration(r, v, rho, 0.0, &p).unwrap();
        let v_rel = relative_velocity(r, v, 0.0);
        let vmag = (v_rel[0] * v_rel[0] + v_rel[1] * v_rel[1] + v_rel[2] * v_rel[2]).sqrt();
        let expected_mag = 0.5 * p.cd * p.area_m2 / p.mass_kg * rho * vmag * vmag;
        let mag = (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt();
        println!("n3-drag-mag: computed={mag:e} expected={expected_mag:e}");
        assert!((mag - expected_mag).abs() / expected_mag < 1e-12);
        for i in 0..3 {
            assert!(a[i] / mag - (-v_rel[i] / vmag) < 1e-12, "component {i} not antiparallel to v_rel");
        }
    }

    #[test]
    fn zero_density_gives_zero_acceleration_exactly() {
        let r = [LEO_R_M, 0.0, 0.0];
        let v = [0.0, LEO_V_M_S, 0.0];
        let a = drag_acceleration(r, v, 0.0, EARTH_ANGULAR_VELOCITY_RAD_S, &props()).unwrap();
        assert_eq!(a, [0.0, 0.0, 0.0]);
    }

    #[test]
    fn negative_density_is_a_typed_error_not_a_panic() {
        let r = [LEO_R_M, 0.0, 0.0];
        let v = [0.0, LEO_V_M_S, 0.0];
        let err = drag_acceleration(r, v, -1.0, 0.0, &props()).unwrap_err();
        assert!(matches!(err, DragError::InvalidDensity(_)));
    }

    #[test]
    fn nan_position_is_a_typed_error_not_a_panic() {
        let err = drag_acceleration([f64::NAN, 0.0, 0.0], [0.0, LEO_V_M_S, 0.0], 1e-12, 0.0, &props()).unwrap_err();
        assert!(matches!(err, DragError::NonFinitePosition(_)));
    }

    #[test]
    fn zero_mass_is_a_typed_error_not_a_panic() {
        let p = DragProperties { cd: 2.2, area_m2: 5.0, mass_kg: 0.0 };
        let err = drag_acceleration([LEO_R_M, 0.0, 0.0], [0.0, LEO_V_M_S, 0.0], 1e-12, 0.0, &p).unwrap_err();
        assert!(matches!(err, DragError::InvalidMass(_)));
    }

    /// `drag_acceleration_core` run with `T = Dual3` (N4's own future entry point, seeded on
    /// VELOCITY, not position -- see this module's own doc, "Shape for N4") must agree, in
    /// its VALUE component, with the plain `f64` path.
    #[test]
    fn dual_path_value_matches_f64_path_seeded_on_velocity() {
        use crate::dual::Dual3;
        let v_rel = [100.0, -7000.0, 50.0];
        let rho = 1e-12;
        let k = -0.5 * 2.2 * 5.0 / 500.0;
        let a_f64 = drag_acceleration_core::<f64>(v_rel, rho, k);
        let v_rel_dual = [Dual3::variable(v_rel[0], 0), Dual3::variable(v_rel[1], 1), Dual3::variable(v_rel[2], 2)];
        let a_dual = drag_acceleration_core::<Dual3>(v_rel_dual, rho, k);
        for i in 0..3 {
            assert!((a_dual[i].v - a_f64[i]).abs() < 1e-9 * a_f64[i].abs().max(1.0), "component {i}: dual={} f64={}", a_dual[i].v, a_f64[i]);
        }
    }
}
