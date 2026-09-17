//! Cannonball solar radiation pressure with a conical (umbra + penumbra) shadow model
//! (`docs/native-dynamics-plan.md` milestone N3, "N3's solar radiation pressure -- cannonball
//! SRP with a conical shadow model"; ADR-002's third amendment).
//!
//! # The formula
//!
//! `a = nu * P_srp * Cr * (A/m) * (AU^2 / |d|^2) * d_hat`, where `d = r - r_sun` is the
//! spacecraft-to-Sun-relative vector **in the sense GMAT uses** (see below), `A` is `SRPArea`
//! (m^2), `m` the spacecraft's total mass (kg), `Cr` the reflectivity coefficient, `AU` the
//! reference distance the inverse-square law is normalised to, `P_srp` the solar radiation
//! pressure at that reference distance (N/m^2), and `nu` in `[0, 1]` the illuminated fraction
//! (1 = full sun, 0 = full umbra) [`illumination_fraction`] computes.
//!
//! This module reproduces GMAT's own `SolarRadiationPressure::GetDerivatives`
//! (`SrpModel = "Spherical"`, `third_party/gmat-src/src/base/forcemodel/
//! SolarRadiationPressure.cpp`), read from GMAT's own source rather than from memory, term for
//! term:
//!
//! ```text
//! sunSat        = state - cbSunVector         // = r_sc - r_sun  (GMAT's own "d")
//! forceVector   = sunSat / |sunSat|            // = d_hat
//! distancefactor = (nominalSun / |sunSat|)^2   // = (AU / |d|)^2
//! mag           = percentSun * fluxPressure * distancefactor / mass[i]   // 1/(m s^2)
//! mag          *= cr[i] * area[i]                                        // m/s^2
//! deriv[3..6]   = mag * forceVector
//! ```
//!
//! (`state`, `cbSunVector` are both relative to the CENTRAL body -- Earth, this module's sole
//! occulter -- so `sunSat` is exactly this module's `d = r - r_sun`; `mass[i]` is the
//! spacecraft's `GetRealParameter("TotalMass")`, `cr[i]` its `GetRealParameter("Cr")`,
//! `area[i]` its `GetRealParameter("SRPArea")`, confirmed by reading
//! `SolarRadiationPressure.cpp`'s own `SetRealParameter`/`SetSpaceObject` bodies, not assumed --
//! `TotalMass`, not `DryMass`, matters when the spacecraft carries fuel; `TotalMass == DryMass`
//! when it does not, as in every golden this crate's tests use).
//!
//! # Every constant, and where it was read
//!
//! GMAT exposes every constant this formula needs as a real parameter on a constructed,
//! `Initialize()`d `SolarRadiationPressure` force object (`SolarRadiationPressure::
//! GetRealParameter`, confirmed against `IsParameterReadOnly`/`PARAMETER_TEXT` in the same
//! source file) -- read here off a live GMAT R2026a instance, not assumed from memory or a
//! published constant table:
//!
//! | Constant | GMAT field | Value (this module's default) | Source |
//! |---|---|---|---|
//! | Solar flux at 1 AU | `Flux` | 1367.0 W/m^2 | live probe, `SolarRadiationPressure`'s own C++ constructor default (`flux(1367.0)`, "IERS 1996") |
//! | Solar radiation pressure at 1 AU | `Flux_Pressure` | 4.559821181358738e-06 N/m^2 | live probe -- read DIRECTLY, not re-derived from `Flux` and a speed-of-light constant of this crate's own: GMAT already computes `fluxPressure = flux / GmatPhysicalConstants::c` and exposes the RESULT, so this module never needs its own copy of the speed of light at all (`c = 299792458.0` m/s exactly, `GmatPhysicalConstants::c`, is a cross-check only: `1367.0 / 299792458.0 = 4.559821181358738e-06`, bit-identical to the probed value -- not itself read by this module) |
//! | Reference distance (1 AU) | `Nominal_Sun` | 149,597,870.691 km | live probe -- bit-identical to the DE405 ephemeris file's own `AU` constant `crate::de::DeEphemeris::au_km` exposes (both the IAU 1976 AU), read independently here off the SRP force itself per this task's own rule |
//! | Sun's equatorial radius (shadow cone geometry) | `SunRadius` | 695,990.0 km | live probe -- set at `Initialize()` from `Sun.EquatorialRadius`, `GmatDefaults.hpp`'s own `STAR_EQUATORIAL_RADIUS` |
//! | Occulting/central body's equatorial radius | `BodyRadius` | 6,378.1363 km | live probe -- set at `Initialize()` from `Earth.EquatorialRadius`; Earth-specific (this module's own "one occulter" scope, below) |
//!
//! The exact probe (a small script under this task's own scratchpad, printing each field via
//! `SolarRadiationPressure::real_parameter` through `crates/gmat-sys`) and its output are
//! recorded in this task's own report. [`SrpConstants::gmat_earth_defaults`] hardcodes these
//! five values as named `const`s, in this module (GMAT-free, no `gmat-sys` dependency) --
//! exactly `crate::tdb`'s own precedent for GMAT's `TimeSystemConverter` constants (that
//! module's own doc comment: "Source of the coefficients: GMAT's own `TimeSystemConverter`
//! singleton, read directly off a live GMAT instance"). Where GMAT does NOT expose a constant
//! this formula needs -- the speed of light itself -- this module never reads or re-derives
//! it, for exactly the reason the `Flux_Pressure` row above states.
//!
//! # Scope: one occulter, Earth
//!
//! This module models Earth (the central body) as the SOLE occulting body, matching this
//! task's own brief ("Do not generalise to multiple occulters this round"). GMAT's own
//! `SolarRadiationPressure` supports `ExtraShadowBodies` (the Moon, say, casting its own
//! shadow on a cislunar spacecraft); this module does not, and [`SrpConstants::body_radius_m`]
//! is documented as the CENTRAL body's own radius for exactly that reason -- a future N-round
//! task generalising this would add a `Vec` of occulters, not change this module's existing
//! single-occulter shape.
//!
//! # The conical shadow model, and its shape for N4
//!
//! [`illumination_fraction`] is the EXACT conical (umbra + penumbra) shadow function GMAT's
//! own `ShadowState::FindShadowState`/`GetPercentSunInPenumbra`
//! (`third_party/gmat-src/src/base/solarsys/ShadowState.cpp`) implement -- Montenbruck & Gill,
//! *Satellite Orbits*, sec. 3.4.2's "shadow function": the apparent angular radii of the Sun
//! (`a = asin(SunRadius / |SC-to-Sun|)`) and of the occulting body (`b = asin(BodyRadius /
//! |SC-to-body|)`) as seen from the spacecraft, and their apparent angular separation (`c =
//! acos(-(SC-to-body direction) . (SC-to-Sun direction))`), decide four regions in this exact
//! order (matching GMAT's own branch order bit-for-bit, not merely the outcome):
//!
//! ```text
//! a + b <= c              -> full sun            (nu = 1)
//! c < b - a                -> umbra               (nu = 0)          [only reachable when b > a]
//! |a - b| < c AND a+b > c  -> penumbra             (nu = the circular-lens-overlap fraction, below)
//! otherwise (c <= a - b)   -> annular ("antumbra") (nu = 1 - (b/a)^2)  [only reachable when a > b]
//! ```
//!
//! In the penumbra, `nu` is the fraction of the solar disc NOT covered by the occulting disc --
//! the standard circular-lens-overlap area formula (Montenbruck & Gill eq. 3.92-3.94, GMAT's
//! own `GetPercentSunInPenumbra`): with `x = (c^2 + a^2 - b^2) / (2c)`, `y = sqrt(a^2 - x^2)`,
//! the OVERLAP area of the two discs (Sun-disc radius `a`, body-disc radius `b`, centres `c`
//! apart) is `area = a^2 acos(x/a) + b^2 acos((c-x)/b) - c y`, and `nu = 1 - area / (pi a^2)`.
//! This is continuous across BOTH region boundaries (`c = a+b` and `c = |a-b|`) by construction
//! (the overlap area is a continuous function of the three side lengths of the geometry, and
//! goes to 0 at `c = a+b` and to the full smaller-disc area at `c = |a-b|`, matching the
//! full-sun and umbra/antumbra branches' own values exactly at each boundary) -- measured, not
//! merely argued, by `tests::illumination_fraction_is_continuous_across_both_boundaries`, below,
//! and by `srp_goldens.rs`'s own penumbra sweep against this crate's other continuity tests'
//! established pattern (`gravity.rs`'s pole-continuity test).
//!
//! **The shape this leaves for N4's partials (this module does NOT implement them -- that is
//! N4's own task).** ADR-002's third amendment records, from GMAT's own source, exactly what
//! GMAT itself does: `SolarRadiationPressure` (spherical) fills the A-matrix's position block
//! ANALYTICALLY, velocity block correctly zero, but "SRP inside a penumbra has its shadow
//! partials omitted, GMAT warning at initialization" -- i.e. GMAT differentiates the cannonball
//! term `nu * K * d_hat / |d|^2` with respect to spacecraft position TREATING `nu` AS A FROZEN
//! CONSTANT (never differentiating [`illumination_fraction`] itself), even when `nu` is
//! genuinely position-dependent (in the penumbra). This module is shaped to match that exactly:
//! [`illumination_fraction`] is `f64`-only (never generic, never differentiated) and
//! [`cannonball_acceleration`] is generic over `T: `[`crate::dual::GravScalar`] (mirroring
//! `gravity.rs`'s own `sh_acceleration<T: GravScalar>` shape exactly) over ONLY the spacecraft
//! position `r_sc` -- `nu` and the Sun's position are both plain `f64` inputs, lifted via
//! `T::constant` inside. N4 seeds `r_sc` with `Dual3` (as `gravity.rs`'s own partials do) and
//! gets `d(nu * K * d_hat/|d|^2) / d(r_sc)` exactly, with `nu` frozen at whatever
//! [`illumination_fraction`] returned for the base point -- bit-for-bit GMAT's own documented
//! omission, not an approximation of it.
//!
//! # No panics, typed errors throughout
//!
//! Every function here returns [`SrpError`] rather than panicking on any input a DRM could
//! supply (this crate's own rule) -- non-finite positions, non-positive mass/area/radii, and
//! the degenerate zero-distance cases GMAT's own C++ guards against with an ad hoc fallback
//! (`if (sunDistance == 0.0) sunDistance = 1.0;`) are all typed errors here instead (this
//! module's own choice, not a reproduction of GMAT's silent fallback -- see [`SrpError`]'s own
//! doc comment).
//!
//! # GMAT-free
//!
//! Like `gravity.rs`/`third_body.rs`, this module has no `gmat-sys` dependency and builds and
//! unit-tests under `cargo test -p av-orbital --no-default-features` -- every constant above is
//! a plain `f64` (read off GMAT once, by a human, and hardcoded with a doc comment citing the
//! read), never a live GMAT call.

use crate::dual::GravScalar;

/// Every way this module's functions can fail -- typed throughout, no panics on any input a
/// DRM could supply (this crate's own rule). Several variants cover degenerate geometry GMAT's
/// own C++ papers over with a silent fallback (`sunDistance == 0.0 -> 1.0`,
/// `SolarRadiationPressure.cpp`) -- this module reports those as errors instead, since they
/// only arise for physically nonsensical input (a spacecraft exactly at the Sun's position, or
/// exactly at the occulting body's centre) that no real arc this crate's goldens exercise ever
/// reaches.
#[derive(Debug, Clone, Copy, thiserror::Error, PartialEq)]
pub enum SrpError {
    /// The spacecraft position was not finite (NaN or infinite).
    #[error("spacecraft position is not finite: {0:?}")]
    NonFiniteScPosition([f64; 3]),
    /// The Sun's position (relative to the central body) was not finite.
    #[error("Sun position is not finite: {0:?}")]
    NonFiniteSunPosition([f64; 3]),
    /// `SRPArea` must be positive and finite.
    #[error("SRP area must be positive and finite, got {0} m^2")]
    InvalidArea(f64),
    /// The spacecraft's total mass must be positive and finite.
    #[error("spacecraft mass must be positive and finite, got {0} kg")]
    InvalidMass(f64),
    /// `Cr` must be finite (physically it is usually in `[0, 2]`, but this module does not
    /// reject an unusual value outside that range -- only a non-finite one, which would
    /// silently poison the acceleration).
    #[error("Cr must be finite, got {0}")]
    InvalidCr(f64),
    /// The Sun's radius (for the shadow cone) must be positive and finite.
    #[error("Sun radius must be positive and finite, got {0} m")]
    InvalidSunRadius(f64),
    /// The occulting body's radius must be finite and non-negative (zero is allowed -- a
    /// point-mass occulter simply never casts a shadow, since `body_radius_m >=
    /// sat_to_body_dist` and the `asin` branch both degrade gracefully to "never in shadow").
    #[error("occulting-body radius must be non-negative and finite, got {0} m")]
    InvalidBodyRadius(f64),
    /// The central body's own distance to the Sun was exactly zero, OR the spacecraft's
    /// distance to the Sun was exactly zero AND the spacecraft is on the central body's DARK
    /// side (`r_sc . r_sun_hat <= 0`) -- the shadow geometry (an apparent angular radius,
    /// `asin(radius / distance)`) is undefined at zero distance. The far more common
    /// zero-Sun-distance case (`r_sc == r_sun` exactly) is caught earlier, by the sunny-side
    /// shortcut returning a well-defined `Ok(1.0)` instead (see
    /// `tests::spacecraft_exactly_at_sun_returns_a_finite_answer_not_a_panic`'s own doc comment
    /// for why, and `tests::spacecraft_at_sun_still_produces_a_finite_srp_acceleration` for why
    /// that is still safe end to end). GMAT's own C++ papers over the spacecraft-to-Sun case
    /// with a silent `sunDistance = 1.0` fallback; this module reports it as a typed error
    /// instead where it is actually reachable.
    #[error("Sun distance is exactly zero; shadow/SRP geometry is undefined")]
    DegenerateSunDistance,
    /// The spacecraft's distance to the occulting body's centre was exactly zero.
    #[error("spacecraft is exactly at the occulting body's centre; shadow geometry is undefined")]
    DegenerateBodyDistance,
    /// The computed acceleration was not finite despite every input passing the checks above
    /// (the last-resort guard: no path to a silent NaN/Inf output survives this module).
    #[error("computed SRP acceleration is not finite: {0:?}")]
    NonFiniteResult([f64; 3]),
}

/// GMAT's own `SolarRadiationPressure` force constants, read off a live GMAT R2026a instance
/// (see this module's own doc comment for the exact field names, values and probe). All in SI
/// (metres, N/m^2), matching this crate's own convention (`lib.rs`'s "Units" section) --
/// converted from GMAT's own km-based fields at [`SrpConstants::gmat_earth_defaults`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SrpConstants {
    /// Solar radiation pressure at the reference distance, N/m^2 (`Flux_Pressure`).
    pub flux_pressure_n_m2: f64,
    /// The reference distance (1 AU) the inverse-square law is normalised to, metres
    /// (`Nominal_Sun`, km in GMAT).
    pub reference_distance_m: f64,
    /// The Sun's equatorial radius, metres (`SunRadius`).
    pub sun_radius_m: f64,
    /// The occulting (central) body's equatorial radius, metres (`BodyRadius`) -- Earth only,
    /// this module's scope (see this module's own doc, "Scope").
    pub body_radius_m: f64,
}

/// GMAT's own `SolarRadiationPressure` force defaults, W/m^2 (`Flux` -- IERS 1996, GMAT's own
/// C++ constructor default), read off a live GMAT R2026a instance. Not itself used by
/// [`SrpConstants`] (which carries `Flux_Pressure` directly, already divided by the speed of
/// light -- see this module's own doc comment) -- kept here only as the human-readable
/// cross-check value this module's doc table cites.
pub const GMAT_SOLAR_FLUX_W_M2: f64 = 1367.0;
/// `Flux_Pressure`, N/m^2 -- read DIRECTLY off GMAT (see this module's own doc comment for why
/// this module never derives it from [`GMAT_SOLAR_FLUX_W_M2`] and a speed-of-light constant of
/// its own).
pub const GMAT_FLUX_PRESSURE_N_M2: f64 = 4.559_821_181_358_738e-06;
/// `Nominal_Sun`, km -- the reference distance (1 AU) SRP's inverse-square law is normalised
/// to. Bit-identical to the DE405 ephemeris file's own `AU` constant
/// (`crate::de::DeEphemeris::au_km`) -- both the IAU 1976 AU -- but read independently here,
/// off the SRP force itself, per this task's own rule.
pub const GMAT_NOMINAL_SUN_KM: f64 = 149_597_870.691;
/// `SunRadius`, km -- the Sun's equatorial radius GMAT's SRP force uses for the shadow cone.
pub const GMAT_SUN_RADIUS_KM: f64 = 695_990.0;
/// `BodyRadius`, km -- Earth's equatorial radius (the occulting/central body, this module's
/// sole scope).
pub const GMAT_EARTH_BODY_RADIUS_KM: f64 = 6_378.136_3;

impl SrpConstants {
    /// GMAT's own `SolarRadiationPressure` defaults for Earth as the occulting/central body,
    /// converted to SI from this module's own `GMAT_*_KM`/`GMAT_FLUX_PRESSURE_N_M2` constants
    /// -- see this module's own doc comment for exactly where each was read.
    pub fn gmat_earth_defaults() -> Self {
        Self {
            flux_pressure_n_m2: GMAT_FLUX_PRESSURE_N_M2,
            reference_distance_m: GMAT_NOMINAL_SUN_KM * 1_000.0,
            sun_radius_m: GMAT_SUN_RADIUS_KM * 1_000.0,
            body_radius_m: GMAT_EARTH_BODY_RADIUS_KM * 1_000.0,
        }
    }
}

/// The DRM's ballistic SRP properties for one spacecraft (question 81: "a seed is a vehicle") --
/// `SRPArea`/`Cr`/total mass, SI (m^2, dimensionless, kg). `mass_kg` is the vehicle's TOTAL
/// mass (GMAT's own `TotalMass`, which equals `DryMass` when there is no fuel tank) -- see this
/// module's own doc comment for why (`SolarRadiationPressure.cpp`'s own `mass = sc-
/// >GetRealParameter("TotalMass")`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SrpProperties {
    pub cr: f64,
    pub area_m2: f64,
    pub mass_kg: f64,
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn norm(a: [f64; 3]) -> f64 {
    dot(a, a).sqrt()
}

fn check_finite_point(p: [f64; 3]) -> bool {
    p.iter().all(|v| v.is_finite())
}

/// The circular-lens-overlap fraction inside a penumbra -- Montenbruck & Gill eq. 3.92-3.94,
/// GMAT's own `ShadowState::GetPercentSunInPenumbra`. `psunrad`/`pcbrad` are the Sun's/the
/// occulting body's apparent angular radii (radians, both `> 0`), `c` the apparent angular
/// separation between their centres (radians). Returned value is the fraction of the solar
/// disc NOT covered -- see this module's own doc comment for the exact formula and why it is
/// continuous at both of this function's own callers' boundaries.
fn percent_sun_in_penumbra(psunrad: f64, pcbrad: f64, c: f64) -> f64 {
    let a2 = psunrad * psunrad;
    let b2 = pcbrad * pcbrad;
    let x = (c * c + a2 - b2) / (2.0 * c);
    let y = (a2 - x * x).max(0.0).sqrt();
    let area = a2 * (x / psunrad).clamp(-1.0, 1.0).acos() + b2 * ((c - x) / pcbrad).clamp(-1.0, 1.0).acos() - c * y;
    1.0 - area / (std::f64::consts::PI * a2)
}

/// The illuminated fraction `nu` in `[0, 1]` at spacecraft position `r_sc_m` (relative to the
/// occulting/central body), given the Sun's position `r_sun_m` (relative to the SAME body) and
/// the two bodies' radii -- GMAT's own conical shadow model, exactly (see this module's own doc
/// comment for the branch order and the penumbra formula). `f64`-only, never generic/
/// differentiated -- see this module's own doc, "The conical shadow model, and its shape for
/// N4", for why.
pub fn illumination_fraction(r_sc_m: [f64; 3], r_sun_m: [f64; 3], sun_radius_m: f64, body_radius_m: f64) -> Result<f64, SrpError> {
    if !check_finite_point(r_sc_m) {
        return Err(SrpError::NonFiniteScPosition(r_sc_m));
    }
    if !check_finite_point(r_sun_m) {
        return Err(SrpError::NonFiniteSunPosition(r_sun_m));
    }
    if !(sun_radius_m.is_finite() && sun_radius_m > 0.0) {
        return Err(SrpError::InvalidSunRadius(sun_radius_m));
    }
    if !(body_radius_m.is_finite() && body_radius_m >= 0.0) {
        return Err(SrpError::InvalidBodyRadius(body_radius_m));
    }

    let r_sun_mag = norm(r_sun_m);
    if r_sun_mag == 0.0 {
        return Err(SrpError::DegenerateSunDistance);
    }
    let unitsun = [r_sun_m[0] / r_sun_mag, r_sun_m[1] / r_sun_mag, r_sun_m[2] / r_sun_mag];
    let rdotsun = dot(r_sc_m, unitsun);
    if rdotsun > 0.0 {
        // Sunny side of the central body is always fully lit (GMAT's own shortcut -- the
        // shadow is always on the anti-sun side of the occulting body).
        return Ok(1.0);
    }

    let sat_to_sun = [-(r_sc_m[0] - r_sun_m[0]), -(r_sc_m[1] - r_sun_m[1]), -(r_sc_m[2] - r_sun_m[2])];
    let sat_to_sun_dist = norm(sat_to_sun);
    let sat_to_body_dist = norm(r_sc_m);
    if sat_to_sun_dist == 0.0 {
        return Err(SrpError::DegenerateSunDistance);
    }
    if sat_to_body_dist == 0.0 {
        return Err(SrpError::DegenerateBodyDistance);
    }

    if sun_radius_m >= sat_to_sun_dist {
        return Ok(1.0);
    }
    if body_radius_m >= sat_to_body_dist {
        return Ok(0.0);
    }

    let a = (sun_radius_m / sat_to_sun_dist).clamp(-1.0, 1.0).asin();
    let b = (body_radius_m / sat_to_body_dist).clamp(-1.0, 1.0).asin();

    let unit_body_to_sat = [r_sc_m[0] / sat_to_body_dist, r_sc_m[1] / sat_to_body_dist, r_sc_m[2] / sat_to_body_dist];
    let unit_sat_to_sun = [sat_to_sun[0] / sat_to_sun_dist, sat_to_sun[1] / sat_to_sun_dist, sat_to_sun[2] / sat_to_sun_dist];
    let cos_c = (-dot(unit_body_to_sat, unit_sat_to_sun)).clamp(-1.0, 1.0);
    let c = cos_c.acos();

    let nu = if a + b <= c {
        1.0
    } else if c < b - a {
        0.0
    } else if (a - b).abs() < c && a + b > c {
        percent_sun_in_penumbra(a, b, c)
    } else {
        1.0 - (b * b) / (a * a)
    };
    Ok(nu.clamp(0.0, 1.0))
}

/// The cannonball SRP acceleration's own core term, `nu * K * d_hat / |d|^2` where `K =
/// flux_pressure_n_m2 * Cr * Area / mass * reference_distance_m^2` and `d = r_sc - r_sun`,
/// generic over `T: GravScalar` **only in `r_sc_m`** -- mirrors `gravity.rs`'s own
/// `sh_acceleration<T: GravScalar>` shape exactly, so N4 can seed `r_sc_m` with `Dual3` and get
/// the position-block partial by forward-mode automatic differentiation (`nu`/`r_sun_m`/`k` are
/// all plain `f64`, lifted via `T::constant` -- see this module's own doc comment, "The conical
/// shadow model, and its shape for N4", for why `nu` is a frozen scalar here rather than itself
/// differentiated).
pub fn cannonball_acceleration<T: GravScalar>(r_sc_m: [T; 3], r_sun_m: [f64; 3], nu: f64, k: f64) -> [T; 3] {
    let r_sun_t = [T::constant(r_sun_m[0]), T::constant(r_sun_m[1]), T::constant(r_sun_m[2])];
    let d = [r_sc_m[0] - r_sun_t[0], r_sc_m[1] - r_sun_t[1], r_sc_m[2] - r_sun_t[2]];
    let d2 = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
    let d_mag = d2.sqrt();
    let d3 = d2 * d_mag;
    let scale = T::constant(nu * k);
    [d[0] * scale / d3, d[1] * scale / d3, d[2] * scale / d3]
}

/// The full cannonball SRP acceleration at spacecraft position `r_sc_m` (relative to the
/// occulting/central body), Sun position `r_sun_m` (relative to the SAME body), GMAT's own SRP
/// constants and this vehicle's ballistic properties -- computes `nu` via
/// [`illumination_fraction`] then the core term via [`cannonball_acceleration`], matching this
/// module's own documented formula exactly. No panics: every input this crate's own rule names
/// (non-finite positions, non-positive mass/area, non-finite `Cr`) is a typed [`SrpError`], and
/// the output is checked finite as a last-resort guard.
pub fn srp_acceleration(r_sc_m: [f64; 3], r_sun_m: [f64; 3], constants: &SrpConstants, props: &SrpProperties) -> Result<[f64; 3], SrpError> {
    if !(props.area_m2.is_finite() && props.area_m2 > 0.0) {
        return Err(SrpError::InvalidArea(props.area_m2));
    }
    if !(props.mass_kg.is_finite() && props.mass_kg > 0.0) {
        return Err(SrpError::InvalidMass(props.mass_kg));
    }
    if !props.cr.is_finite() {
        return Err(SrpError::InvalidCr(props.cr));
    }

    let nu = illumination_fraction(r_sc_m, r_sun_m, constants.sun_radius_m, constants.body_radius_m)?;
    let k = srp_k(constants, props);
    let accel = cannonball_acceleration::<f64>(r_sc_m, r_sun_m, nu, k);
    if !check_finite_point(accel) {
        return Err(SrpError::NonFiniteResult(accel));
    }
    Ok(accel)
}

/// `K = flux_pressure_n_m2 * Cr * Area / mass * reference_distance_m^2` -- the scale factor
/// [`cannonball_acceleration`] takes, factored out of [`srp_acceleration`] (N4,
/// `crate::stm`'s own position-partial computation needs the identical `k` that produced a
/// given acceleration, not a second, independently-written copy of this formula that could
/// drift from it).
pub fn srp_k(constants: &SrpConstants, props: &SrpProperties) -> f64 {
    constants.flux_pressure_n_m2 * props.cr * props.area_m2 / props.mass_kg * constants.reference_distance_m * constants.reference_distance_m
}

#[cfg(test)]
mod tests {
    use super::*;

    const AU_M: f64 = GMAT_NOMINAL_SUN_KM * 1_000.0;
    const SUN_RADIUS_M: f64 = GMAT_SUN_RADIUS_KM * 1_000.0;
    const EARTH_RADIUS_M: f64 = GMAT_EARTH_BODY_RADIUS_KM * 1_000.0;
    const LEO_R_M: f64 = 6_878_000.0;

    fn r_sun_along_x() -> [f64; 3] {
        [AU_M, 0.0, 0.0]
    }

    /// A spacecraft on the sunny side (positive x, same direction as the Sun) is always in
    /// full sun, exactly 1.0.
    #[test]
    fn full_sun_on_the_sunny_side_is_exactly_one() {
        let r_sc = [LEO_R_M, 0.0, 0.0];
        let nu = illumination_fraction(r_sc, r_sun_along_x(), SUN_RADIUS_M, EARTH_RADIUS_M).unwrap();
        assert_eq!(nu, 1.0);
    }

    /// A spacecraft directly behind Earth on the anti-sun axis, well within the umbra cone at
    /// LEO altitude, is exactly 0.0 (deterministic branch, not merely close).
    #[test]
    fn deep_umbra_directly_behind_earth_is_exactly_zero() {
        let r_sc = [-LEO_R_M, 0.0, 0.0];
        let nu = illumination_fraction(r_sc, r_sun_along_x(), SUN_RADIUS_M, EARTH_RADIUS_M).unwrap();
        assert_eq!(nu, 0.0);
    }

    /// Sweep the spacecraft around a LEO-radius circle from deep umbra, through the penumbra,
    /// into full sun -- exactly the shape round 1's pole-continuity test
    /// (`gravity.rs::continuous_across_the_pole`) uses: measure the largest single-step change
    /// relative to the median step over a dense sweep, print it, and assert it is not an
    /// outlier. Also asserts monotonicity (illumination never decreases as the spacecraft
    /// sweeps away from the anti-sun point) and the two exact endpoints (1.0 in full sun, 0.0
    /// in deep umbra).
    #[test]
    fn illumination_fraction_is_continuous_across_both_boundaries() {
        let r_sun = r_sun_along_x();
        let n_steps = 4000;
        // theta=0 is directly behind Earth (deep umbra); theta grows toward full sun. At LEO
        // altitude Earth's OWN apparent angular radius from the spacecraft is ~68 deg
        // (asin(EARTH_RADIUS_M / LEO_R_M)), so the umbra/penumbra boundary (c = b - a) and the
        // penumbra/full-sun boundary (c = b + a) both sit close to that ~68 deg orbital angle,
        // not near theta=0 -- an EARLIER version of this test used theta_max=0.02 rad (~1.1
        // deg) and measured the sweep staying in exact umbra the entire way (caught by this
        // test's own `nus.last() == 1.0` assertion failing, not assumed correct); 1.3 rad
        // (~74.5 deg) clears both boundaries with margin (the Sun's own apparent radius `a` is
        // tiny, ~0.267 deg, so the penumbra band itself is only ~0.53 deg / ~0.0093 rad wide,
        // comfortably resolved by this sweep's ~3.25e-4 rad step).
        let theta_max = 1.3_f64; // radians
        let mut nus = Vec::with_capacity(n_steps + 1);
        for i in 0..=n_steps {
            let theta = theta_max * (i as f64) / (n_steps as f64);
            // Stay in the x-z plane, sweeping from directly behind Earth (-x) outward.
            let x = -LEO_R_M * theta.cos();
            let z = LEO_R_M * theta.sin();
            let r_sc = [x, 0.0, z];
            let nu = illumination_fraction(r_sc, r_sun, SUN_RADIUS_M, EARTH_RADIUS_M).unwrap();
            assert!(nu.is_finite(), "non-finite nu at theta={theta}");
            assert!((0.0..=1.0).contains(&nu), "nu={nu} outside [0,1] at theta={theta}");
            nus.push(nu);
        }
        assert_eq!(nus[0], 0.0, "sweep must start in exact deep umbra");
        assert_eq!(*nus.last().unwrap(), 1.0, "sweep must end in exact full sun (theta_max too small if this fails)");

        // Monotonicity: illumination never decreases along this outward sweep (allow a tiny
        // floating-point epsilon).
        for w in nus.windows(2) {
            assert!(w[1] >= w[0] - 1e-12, "illumination decreased: {} -> {}", w[0], w[1]);
        }

        // Continuity, measured on a SECOND, narrow sweep tightly bracketing both boundaries --
        // computed once, printed here: `a` (Sun's apparent radius) = 0.0046524 rad, `b`
        // (Earth's, at this radius) = 1.1871989 rad, so the umbra/penumbra boundary `c=b-a` is
        // at 1.1825465 rad and the penumbra/full-sun boundary `c=b+a` is at 1.1918513 rad. The
        // WIDE sweep above (0..1.3 rad) is dominated by a huge flat region of EXACT umbra
        // (theta from 0 to ~1.18 rad, where every consecutive step is EXACTLY 0.0) -- measured,
        // not assumed: an earlier version of this test computed the continuity ratio over that
        // wide sweep directly and got `median_step=0`, `ratio=inf` (most of a uniform sweep of
        // 4000 steps over 1.3 rad falls in that flat region, not near either boundary), which is
        // an artifact of where the samples land, not evidence of a discontinuity. Narrowing the
        // sweep to bracket ONLY the transition region (with margin on both sides, so a similar
        // flat-region artifact cannot recur at THIS window's own edges) fixes that: a dense,
        // uniform sweep here has comparably-sized steps throughout the interesting region,
        // exactly round 1's own `continuous_across_the_pole` measurement shape.
        let narrow_lo = 1.15_f64;
        let narrow_hi = 1.22_f64;
        let narrow_steps = 4000;
        let mut narrow_nus = Vec::with_capacity(narrow_steps + 1);
        for i in 0..=narrow_steps {
            let theta = narrow_lo + (narrow_hi - narrow_lo) * (i as f64) / (narrow_steps as f64);
            let x = -LEO_R_M * theta.cos();
            let z = LEO_R_M * theta.sin();
            let nu = illumination_fraction([x, 0.0, z], r_sun, SUN_RADIUS_M, EARTH_RADIUS_M).unwrap();
            narrow_nus.push(nu);
        }
        assert_eq!(narrow_nus[0], 0.0, "narrow window's own lower margin must still be exact umbra");
        assert_eq!(*narrow_nus.last().unwrap(), 1.0, "narrow window's own upper margin must already be exact full sun");

        // Only NONZERO steps enter the median (the narrow window still has a short flat run at
        // each of its own two margins, by construction above) -- filtering out exact-zero steps
        // is what makes the median meaningful here, not an attempt to hide a real gap: every
        // step, zero or not, is still checked against `max_step` for the ratio itself.
        let all_steps: Vec<f64> = narrow_nus.windows(2).map(|w| (w[1] - w[0]).abs()).collect();
        let max_step = all_steps.iter().cloned().fold(0.0_f64, f64::max);
        let mut nonzero_steps: Vec<f64> = all_steps.iter().cloned().filter(|s| *s > 0.0).collect();
        nonzero_steps.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert!(!nonzero_steps.is_empty(), "narrow window found no nonzero step at all -- window too narrow to bracket the transition");
        let median_step = nonzero_steps[nonzero_steps.len() / 2];
        let ratio = max_step / median_step;
        println!(
            "n3-srp-penumbra-continuity: max_step={max_step:e} median_step={median_step:e} ratio={ratio:e} \
             over {narrow_steps} steps ({} nonzero) bracketing theta in [{narrow_lo}, {narrow_hi}] rad",
            nonzero_steps.len()
        );
        assert!(ratio < 50.0, "penumbra sweep's largest step is anomalously larger than typical: ratio={ratio:e}");
    }

    /// At exactly 1 AU FROM THE SUN, nu=1 (full sun, chosen point off the anti-sun axis): the
    /// acceleration must equal `flux_pressure * Cr * Area / mass` exactly (the `(AU/|d|)^2`
    /// factor is 1 only when `|d| = |r_sc - r_sun|` itself is exactly `AU_M` -- NOT when `r_sc`
    /// alone is `AU_M` from the origin, which an earlier version of this test conflated: with
    /// `r_sun=[AU,0,0]` and `r_sc=[0,AU,0]`, `|d| = AU*sqrt(2)`, giving a measured factor of
    /// exactly 0.5, not 1 -- caught by this test's own printed `computed`/`expected` values
    /// disagreeing by exactly 2x, not assumed correct).
    #[test]
    fn cannonball_acceleration_matches_the_formula_at_one_au_full_sun() {
        let constants = SrpConstants::gmat_earth_defaults();
        let props = SrpProperties { cr: 1.8, area_m2: 5.0, mass_kg: 500.0 };
        // Spacecraft at [AU, AU, 0]: `d = r_sc - r_sun = [0, AU, 0]`, so `|d| == AU_M` exactly
        // (off the anti-sun axis, so this is a genuine "full sun by the sunny-side shortcut"
        // case, not the trivial `r_sc == r_sun` degenerate one).
        let r_sun = [AU_M, 0.0, 0.0];
        let r_sc = [AU_M, AU_M, 0.0];
        let a = srp_acceleration(r_sc, r_sun, &constants, &props).unwrap();
        let expected_mag = constants.flux_pressure_n_m2 * props.cr * props.area_m2 / props.mass_kg;
        let mag = norm(a);
        println!("n3-srp-1au-mag: computed={mag:e} expected={expected_mag:e}");
        assert!((mag - expected_mag).abs() / expected_mag < 1e-12);
        // Direction: away from the Sun, i.e. d = r_sc - r_sun has a negative x-component and
        // positive y-component; a must point the same way as d.
        let d = [r_sc[0] - r_sun[0], r_sc[1] - r_sun[1], r_sc[2] - r_sun[2]];
        let dmag = norm(d);
        for i in 0..3 {
            assert!((a[i] / mag - d[i] / dmag).abs() < 1e-12, "component {i}: acceleration not parallel to d");
        }
    }

    #[test]
    fn nan_position_is_a_typed_error_not_a_panic() {
        let constants = SrpConstants::gmat_earth_defaults();
        let props = SrpProperties { cr: 1.8, area_m2: 5.0, mass_kg: 500.0 };
        let err = srp_acceleration([f64::NAN, 0.0, 0.0], [AU_M, 0.0, 0.0], &constants, &props).unwrap_err();
        assert!(matches!(err, SrpError::NonFiniteScPosition(_)));
    }

    #[test]
    fn infinite_sun_position_is_a_typed_error() {
        let constants = SrpConstants::gmat_earth_defaults();
        let props = SrpProperties { cr: 1.8, area_m2: 5.0, mass_kg: 500.0 };
        let err = srp_acceleration([LEO_R_M, 0.0, 0.0], [f64::INFINITY, 0.0, 0.0], &constants, &props).unwrap_err();
        assert!(matches!(err, SrpError::NonFiniteSunPosition(_)));
    }

    #[test]
    fn zero_mass_is_a_typed_error_not_a_panic() {
        let constants = SrpConstants::gmat_earth_defaults();
        let props = SrpProperties { cr: 1.8, area_m2: 5.0, mass_kg: 0.0 };
        let err = srp_acceleration([LEO_R_M, 0.0, 0.0], r_sun_along_x(), &constants, &props).unwrap_err();
        assert!(matches!(err, SrpError::InvalidMass(_)));
    }

    #[test]
    fn negative_area_is_a_typed_error_not_a_panic() {
        let constants = SrpConstants::gmat_earth_defaults();
        let props = SrpProperties { cr: 1.8, area_m2: -5.0, mass_kg: 500.0 };
        let err = srp_acceleration([LEO_R_M, 0.0, 0.0], r_sun_along_x(), &constants, &props).unwrap_err();
        assert!(matches!(err, SrpError::InvalidArea(_)));
    }

    #[test]
    fn nonfinite_cr_is_a_typed_error_not_a_panic() {
        let constants = SrpConstants::gmat_earth_defaults();
        let props = SrpProperties { cr: f64::NAN, area_m2: 5.0, mass_kg: 500.0 };
        let err = srp_acceleration([LEO_R_M, 0.0, 0.0], r_sun_along_x(), &constants, &props).unwrap_err();
        assert!(matches!(err, SrpError::InvalidCr(_)));
    }

    /// `r_sc == r_sun` (the spacecraft exactly at the Sun's own position) ALWAYS trips the
    /// sunny-side shortcut first (`rdotsun = r_sun . (r_sun/|r_sun|) = |r_sun| > 0` whenever
    /// `r_sun` itself is nonzero -- checked separately by
    /// [`spacecraft_at_sun_still_produces_a_finite_srp_acceleration`], below), so
    /// [`SrpError::DegenerateSunDistance`]'s OWN `sat_to_sun_dist == 0.0` guard inside
    /// `illumination_fraction` is UNREACHABLE from this exact input (an earlier version of this
    /// test asserted the opposite and failed, not assumed correct) -- this returns `Ok(1.0)`,
    /// a well-defined, finite (if physically degenerate) answer, never a panic or a NaN.
    #[test]
    fn spacecraft_exactly_at_sun_returns_a_finite_answer_not_a_panic() {
        let r_sun = r_sun_along_x();
        let nu = illumination_fraction(r_sun, r_sun, SUN_RADIUS_M, EARTH_RADIUS_M).unwrap();
        assert_eq!(nu, 1.0, "the sunny-side shortcut fires before any zero-distance check is reached");
    }

    /// The genuinely dangerous combination -- `illumination_fraction` reporting `nu=1.0` for a
    /// degenerate `r_sc == r_sun` (above) COULD, if nothing downstream guarded it, feed a
    /// division by `|d|^3 = 0` into `cannonball_acceleration` and produce a silent NaN/Inf.
    /// [`srp_acceleration`]'s own last-resort finite check on its output is what actually
    /// prevents that -- checked here end to end, at the public entry point this crate's
    /// production code (`crate::model::EarthGravityModel::derivatives`) actually calls.
    #[test]
    fn spacecraft_at_sun_still_produces_a_finite_srp_acceleration() {
        let constants = SrpConstants::gmat_earth_defaults();
        let props = SrpProperties { cr: 1.8, area_m2: 5.0, mass_kg: 500.0 };
        let r_sun = r_sun_along_x();
        let err = srp_acceleration(r_sun, r_sun, &constants, &props).unwrap_err();
        assert!(matches!(err, SrpError::NonFiniteResult(_)), "expected the top-level finite-output guard to catch this, got {err:?}");
    }

    #[test]
    fn spacecraft_exactly_at_central_body_center_is_a_typed_error() {
        let err = illumination_fraction([0.0, 0.0, 0.0], r_sun_along_x(), SUN_RADIUS_M, EARTH_RADIUS_M).unwrap_err();
        // rdotsun == 0 here (dot of the zero vector with anything is 0), so this falls through
        // to the dark-side branch and hits the body-distance-zero guard.
        assert!(matches!(err, SrpError::DegenerateBodyDistance));
    }

    /// `cannonball_acceleration` run with `T = Dual3` (N4's own future entry point) must agree,
    /// in its VALUE component, with the plain `f64` path -- a structural check that the generic
    /// shape actually produces the same numbers, not a partials correctness claim (N4's own
    /// job).
    #[test]
    fn dual_path_value_matches_f64_path() {
        use crate::dual::Dual3;
        let r_sun = r_sun_along_x();
        let r_sc = [0.0, AU_M, 0.0];
        let nu = 1.0;
        let k = 4.559_821_181_358_738e-06 * 1.8 * 5.0 / 500.0 * AU_M * AU_M;
        let a_f64 = cannonball_acceleration::<f64>(r_sc, r_sun, nu, k);
        let r_sc_dual = [Dual3::variable(r_sc[0], 0), Dual3::variable(r_sc[1], 1), Dual3::variable(r_sc[2], 2)];
        let a_dual = cannonball_acceleration::<Dual3>(r_sc_dual, r_sun, nu, k);
        for i in 0..3 {
            assert!((a_dual[i].v - a_f64[i]).abs() < 1e-9 * a_f64[i].abs().max(1.0), "component {i}: dual={} f64={}", a_dual[i].v, a_f64[i]);
        }
    }
}
