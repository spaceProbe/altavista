//! The Jacchia-Roberts atmosphere density model, ported from GMAT's own
//! `JacchiaRobertsAtmosphere`/`AtmosphereModel` C++ source (`docs/native-dynamics-plan.md`
//! milestone N3, task 3b: "implementing GMAT's own `JacchiaRobertsAtmosphere`") --
//! `third_party/gmat-src/src/base/solarsys/JacchiaRobertsAtmosphere.cpp`/`.hpp` and
//! `AtmosphereModel.cpp`/`.hpp`, read directly (this crate's own rule: GMAT's source, not a
//! textbook, is the authority for what GMAT actually computes), term for term, constant for
//! constant. This is the largest single module this task adds; see "What this module does
//! NOT do", at the end of this doc comment, for the one piece ported with a named,
//! documented gap rather than guessed.
//!
//! # Shape: exospheric temperature, then a height-banded density profile
//!
//! [`exotherm`] computes the local exospheric temperature (and, for height < 125 km, three
//! auxiliary polynomial roots the density formulas below need) from the spacecraft's
//! position, the Sun's position, the geomagnetic index and the minimum global exospheric
//! temperature -- `JacchiaRobertsAtmosphere::exotherm`, ported unchanged (every named
//! constant, every polynomial, in the SAME order GMAT's own C++ evaluates them, including the
//! Horner-form polynomial evaluations, which is why floating-point agreement with GMAT's own
//! `GetDerivatives` is measured to be at the acceleration floor rather than merely "close" --
//! see this crate's own report). [`density_kg_m3`] then picks ONE of four formulas by
//! altitude band, exactly GMAT's own `JacchiaRoberts` private method's `if`/`else if` chain:
//!
//! - `height <= 90 km`: the constant [`RHO_ZERO`] (GMAT: "JR is turned off below 100 km
//!   altitude" is enforced one level up, in [`density_kg_m3`] itself, matching
//!   `JacchiaRobertsAtmosphere::Density`'s own hard error for `height <= 100.0`; the `<= 90`
//!   branch inside `JacchiaRoberts` itself is therefore dead code in GMAT for any input that
//!   reaches it, and is kept here only because [`raw_density_g_cm3`] is also exercised
//!   directly by this module's own tests below 100 km, mirroring GMAT's own structure exactly
//!   rather than pruning a branch GMAT itself never removed).
//! - `90 < height < 100 km`: [`rho_100`] (`rho_100` in GMAT).
//! - `100 <= height <= 125 km`: [`rho_125`] (`rho_125`).
//! - `125 < height <= 2500 km`: [`rho_high`] (`rho_high`), which additionally needs the
//!   exospheric temperature AT 500 km (a second [`exotherm`] call, exactly as GMAT computes
//!   `t_500` before `temperature`).
//! - `height > 2500 km`: zero.
//!
//! Every raw density is then multiplied by [`rho_cor`], GMAT's own geomagnetic/semiannual/
//! seasonal-latitudinal correction factor, and the g/cm^3 result is converted to kg/m^3 by
//! the SAME `1.0e3` factor `JacchiaRobertsAtmosphere::Density` applies.
//!
//! # Units: kilometres internally, matching GMAT's own hardcoded thresholds
//!
//! Unlike [`crate::gravity`]/[`crate::srp`] (SI throughout), this module's INTERNAL functions
//! ([`exotherm`], [`rho_100`], [`rho_125`], [`rho_cor`], [`rho_high`]) work in the SAME units
//! GMAT's own C++ hardcodes them in: kilometres (every altitude threshold -- 90, 100, 125,
//! 500, 2500 -- and [`CB_POLAR_RADIUS_KM_EARTH_DEFAULT`]/[`geodetic_height_lat_km`]'s own
//! `cbRadius` are km-valued constants copied VERBATIM from GMAT's own source; converting them
//! to metres would require re-deriving every polynomial coefficient's own implicit units,
//! which is exactly the kind of silent-rewrite risk this crate's own `cof.rs` doc warns
//! against). [`density_kg_m3`], the public SI entry point, converts its metre-valued inputs
//! to kilometres at the boundary and converts nothing else -- mirroring `crate::cof`'s own
//! "convert at the boundary, match the source everywhere else" precedent.
//!
//! # The rotating-atmosphere relative velocity is NOT this module's job
//!
//! This module computes DENSITY ONLY, at a body-fixed position and an epoch --
//! `JacchiaRobertsAtmosphere::Density`'s own scope exactly. The `v - omega x r` relative
//! velocity `DragForce::Accelerate` forms around the density this function returns is
//! [`crate::drag`]'s job (see that module's own doc comment for exactly how GMAT forms it,
//! read from `DragForce.cpp`).
//!
//! # Geodetic height and latitude: the SAME body-fixed rotation gravity.rs already uses
//!
//! `AtmosphereModel::CalculateGeodetics` converts the spacecraft's MJ2000Eq position into the
//! CENTRAL BODY'S OWN BODY-FIXED frame (`CoordinateConverter::Convert(..., cbFixed)`) before
//! applying Vallado's iterative oblate-spheroid geodetic algorithm (Vallado, 4th ed.,
//! Algorithm 12) -- this module does NOT duplicate that frame conversion: [`density_kg_m3`]
//! takes the spacecraft's BODY-FIXED position as a parameter (`r_sc_body_fixed_km`), which
//! `crate::model::EarthGravityModel::derivatives` already computes once per call via its own
//! bound `R: BodyFixedRotation` for the gravity term -- the identical rotation, reused here
//! rather than a second, independent conversion this crate would have to keep in sync with
//! it. [`geodetic_height_lat_km`] is [`AtmosphereModel::CalculateGeodetics`]'s OWN iterative
//! algorithm, ported unchanged (the `while (delta > geodeticTolerance)` loop, `geodeticTolerance
//! = 1.0e-7` radians).
//!
//! **The Sun-angle geometry ([`exotherm`]'s `hour_angle`/`sun_dec`), by contrast, uses the
//! UN-ROTATED (inertial, central-body-relative MJ2000Eq) spacecraft and Sun positions** --
//! confirmed by reading `DragForce::Accelerate`/`GetDensity`: `atmos->Density(state, ...)`
//! passes `theState` (the ODEModel's own raw Cartesian state, MJ2000Eq, never rotated) straight
//! through, and `JacchiaRobertsAtmosphere::exotherm`'s own doc comment labels its `sun`/
//! `space_craft` parameters "TOD GCI" (True-of-Date Geocentric Inertial) -- a doc-comment
//! holdover from the original Swingby port this class states it was ported from (this class's
//! own file header: "ported from the Swingby/Windows source"), NOT something the actual C++
//! call chain performs: no coordinate-system conversion of `theState`/`sunLoc` happens anywhere
//! between `DragForce::GetDensity` and `exotherm`. This module follows the CODE, not the stale
//! comment: `sun_dec`/`hour_angle` are computed from whatever inertial-ish frame the caller's
//! position/Sun vectors are already in (MJ2000Eq for this crate, since that is
//! `EarthGravityModel`'s own state frame), which is self-consistent (both vectors in the same
//! frame is all the geometry needs) and differs from a genuine True-of-Date rotation by at
//! most a few arcseconds of precession/nutation -- utterly negligible next to this
//! semi-empirical model's own accuracy.
//!
//! # The Sun vector: geocentric, not heliocentric (inferred, not read from missing source)
//!
//! `DragForce::GetDensity` sets `sunLoc = sun->GetState(when)` with NO subtraction of the
//! central body's own state (`cbLoc`, separately captured but never used by
//! `JacchiaRobertsAtmosphere`) -- and `DragForce`'s own DEFAULT `sunLoc` (its constructor,
//! before any propagation) is `[2.65e7, -1.32757e8, -5.75566e7]` km, magnitude ~1.476e8 km,
//! i.e. ~1 AU -- a GEOCENTRIC Sun distance, not a near-zero SSB-relative one. This module
//! therefore takes `r_sun_km` as the geocentric (Earth-to-Sun) vector, matching
//! `crate::de::DeEphemeris::geocentric_position_km2(DeBody::Sun, ...)` -- the SAME vector
//! [`crate::srp`] already uses for the Sun. `GmatBase::GetState`'s exact default reference
//! frame is not independently confirmed here (GMAT's `CelestialBody.cpp`/`SpacePoint.cpp` are
//! not read by this task), so this is a documented INFERENCE from the constructor default's
//! own magnitude and the confirmed absence of any `cbLoc` subtraction, not a source-verified
//! fact -- named here as exactly that, per this task's own rule to state what was and was not
//! confirmed.
//!
//! # What this module does NOT do (a named gap, not a guess)
//!
//! **File-based (CSSI) F10.7/F10.7A/Kp selection is [`crate::weather`]'s job, and even there
//! is a documented approximation** (see that module's own doc, "What this reader could NOT
//! verify"): GMAT's `SolarFluxReader` class (`LoadFluxData`/`GetInputs`/`PrepareKpData`) --
//! which resolves an epoch to exactly one day's record and one of its eight 3-hourly `Kp`
//! slots, with whatever processing-latency lag GMAT applies -- has NO source file in this
//! repository's mirrored `third_party/gmat-src` tree (`find third_party/gmat-src -iname
//! "*SolarFlux*"` returns nothing). [`WeatherInputs`] therefore accepts a caller-resolved
//! `f107`/`f107a`/`kp` triple directly (GMAT's own CONSTANT defaults via
//! [`crate::weather::ConstantWeather::gmat_defaults`], or a caller-chosen file-derived
//! triple via [`crate::weather::SpaceWeatherFile`]) rather than resolving an epoch to a file
//! row itself -- this module never guesses the day-offset/slot-selection convention GMAT's
//! own (unavailable) `SolarFluxReader` implements.

use crate::frame::BodyFixedRotation;

/// Every way this module's functions can fail. Typed throughout -- no panic on any input a
/// DRM could supply (this crate's own rule).
#[derive(Debug, Clone, thiserror::Error, PartialEq)]
pub enum JacchiaRobertsError {
    /// The spacecraft position was not finite.
    #[error("spacecraft position is not finite: {0:?}")]
    NonFinitePosition([f64; 3]),
    /// The Sun position was not finite.
    #[error("Sun position is not finite: {0:?}")]
    NonFiniteSunPosition([f64; 3]),
    /// GMAT's own `JacchiaRobertsAtmosphere::Density`: "The Jacchia-Roberts atmosphere model
    /// is not available for altitudes below 100 km" -- a hard `AtmosphereException` in GMAT,
    /// reproduced here as a typed error rather than a panic.
    #[error("the Jacchia-Roberts atmosphere model is not available below 100 km altitude (computed height {0} km)")]
    BelowMinimumAltitude(f64),
    /// `exotherm`'s own guard (`JacchiaRobertsAtmosphere::exotherm`): the spacecraft and Sun
    /// are numerically too close to colinear in the equatorial (x,y) plane for the hour-angle
    /// computation's own denominators to be meaningful (GMAT's own `AtmosphereException`:
    /// "Numerical precision error ... denominator is too close to 0.0").
    #[error("numerical precision error forming the hour angle: spacecraft/Sun geometry is too close to degenerate")]
    DegenerateHourAngleGeometry,
    /// `F107`/`F107A`/`Kp` must be finite (physically F10.7/F10.7A are positive and Kp is in
    /// `[0, 9]`, but this module only rejects a non-finite value, which would silently poison
    /// the temperature computation).
    #[error("weather inputs must be finite, got F107={f107} F107A={f107a} Kp={kp}")]
    InvalidWeather { f107: f64, f107a: f64, kp: f64 },
    /// The central body's equatorial radius/flattening must be finite and physically sane
    /// (radius positive, flattening in `[0, 1)`).
    #[error("central-body geodetic parameters invalid: equatorial radius {equatorial_radius_km} km, flattening {flattening}")]
    InvalidCentralBody { equatorial_radius_km: f64, flattening: f64 },
    /// The body-fixed rotation this call needed failed (propagated from a `BodyFixedRotation`
    /// implementation, e.g. an uninitialised GMAT coordinate system).
    #[error("body-fixed rotation failed: {0}")]
    Rotation(String),
    /// The computed density was not finite despite every input passing the checks above.
    #[error("computed density is not finite: {0}")]
    NonFiniteResult(f64),
}

// ---------------------------------------------------------------------------------------------
// GMAT's own constants, copied verbatim from JacchiaRobertsAtmosphere.cpp (see this module's
// own doc comment: this module's internal functions work in the SAME km/degree-Kelvin units
// GMAT's own source hardcodes these in).
// ---------------------------------------------------------------------------------------------

/// Low-altitude (<= 90 km) density, g/cm^3 (GMAT's own comment says "g/cm**2", a typo this
/// module does not repeat -- the value and its use in [`density_kg_m3`]'s final `* 1.0e3`
/// conversion to kg/m^3 are only dimensionally consistent as g/cm^3).
pub const RHO_ZERO: f64 = 3.46e-9;
/// Temperature at 90 km, degrees Kelvin.
pub const TZERO: f64 = 183.0;
/// Earth surface gravitational acceleration, m/s^2 (GMAT's own value, not this crate's
/// `crate::gravity` constant -- kept independent, matching this module's "copied verbatim"
/// rule).
pub const G_ZERO: f64 = 9.80665;
/// Gas constant, joules/(degK-mole).
pub const GAS_CON: f64 = 8.31432;
/// Avogadro's number.
pub const AVOGADRO: f64 = 6.022045e23;

const CON_C: [f64; 5] = [-89284375.0, 3542400.0, -52687.5, 340.5, -0.8];
const CON_L: [f64; 5] = [0.1031445e5, 0.2341230e1, 0.1579202e-2, -0.1252487e-5, 0.2462708e-9];
const MZERO: f64 = 28.82678;
const M_CON: [f64; 7] = [-435093.363387, 28275.5646391, -765.33466108, 11.043387545, -0.08958790995, 0.00038737586, -0.000000697444];
const S_CON: [f64; 6] = [3144902516.672729, -123774885.4832917, 1816141.096520398, -11403.31079489267, 24.36498612105595, 0.008957502869707995];
const S_BETA: [f64; 6] = [-52864482.17910969, -16632.50847336828, -1.308252378125, 0.0, 0.0, 0.0];
const OMEGA: f64 = -0.94585589;
const ZETA_CON: [f64; 7] = [0.1985549e-10, -0.1833490e-14, 0.1711735e-17, -0.1021474e-20, 0.3727894e-24, -0.7734110e-28, 0.7026942e-32];
const MOL_MASS: [f64; 6] = [28.0134, 39.948, 4.0026, 31.9988, 15.9994, 1.00797];
const NUM_DENS: [f64; 5] = [0.78110, 0.93432e-2, 0.61471e-5, 0.161778, 0.95544e-1];
#[rustfmt::skip]
const CON_DEN: [[f64; 7]; 5] = [
    [0.1093155e2, 0.1186783e-2, -0.1677341e-5, 0.1420228e-8, -0.7139785e-12, 0.1969715e-15, -0.2296182e-19],
    [0.8049405e1, 0.2382822e-2, -0.3391366e-5, 0.2909714e-8, -0.1481702e-11, 0.4127600e-15, -0.4837461e-19],
    [0.7646886e1, -0.4383486e-3, 0.4694319e-6, -0.2894886e-9, 0.9451989e-13, -0.1270838e-16, 0.0],
    [0.9924237e1, 0.1600311e-2, -0.2274761e-5, 0.1938454e-8, -0.9782183e-12, 0.2698450e-15, -0.3131808e-19],
    [0.1097083e2, 0.6118742e-4, -0.1165003e-6, 0.9239354e-10, -0.3490739e-13, 0.5116298e-17, 0.0],
];

/// Earth's own equatorial radius and flattening, GMAT's own `Earth.EquatorialRadius`/
/// `Earth.Flattening`, read off a live GMAT R2026a instance (this crate's own precedent, e.g.
/// `crate::srp::GMAT_EARTH_BODY_RADIUS_KM`) -- `equatorial_radius_km` is bit-identical to that
/// constant; `flattening` (`0.0033527`) and the derived `polar_radius_km`
/// (`equatorial_radius_km * (1 - flattening)`, confirmed against the live probe's own
/// `earth.GetRealParameter("PolarRadius")` = `6356.75232242699`) are new to this module.
pub const EARTH_EQUATORIAL_RADIUS_KM: f64 = 6_378.136_3;
pub const EARTH_FLATTENING: f64 = 0.0033527;
/// `JacchiaRobertsAtmosphere`'s own hardcoded fallback (its constructor, before
/// `SetCentralBody` is ever called) -- this module's default matches
/// [`EARTH_EQUATORIAL_RADIUS_KM`] * (1 - [`EARTH_FLATTENING`]) to 5 decimal places
/// (`6356.75232...` vs `6356.766`; GMAT's own fallback is a slightly different, older
/// constant that a real run never actually uses, since `SetCentralBody` always overwrites it
/// before any `Density` call -- kept here only as a documented historical curiosity, NOT used
/// by [`density_kg_m3`], which always derives the polar radius from the two constants above).
pub const CB_POLAR_RADIUS_KM_EARTH_DEFAULT: f64 = 6_356.766;

/// The central body's geodetic shape parameters this module needs -- Earth's own values via
/// [`CentralBodyGeodetics::earth_defaults`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CentralBodyGeodetics {
    pub equatorial_radius_km: f64,
    pub flattening: f64,
}

impl CentralBodyGeodetics {
    pub fn earth_defaults() -> Self {
        Self { equatorial_radius_km: EARTH_EQUATORIAL_RADIUS_KM, flattening: EARTH_FLATTENING }
    }

    fn polar_radius_km(&self) -> f64 {
        self.equatorial_radius_km * (1.0 - self.flattening)
    }
}

/// The resolved F10.7/F10.7A/Kp triple [`density_kg_m3`] needs -- either GMAT's own CONSTANT
/// defaults ([`crate::weather::ConstantWeather::gmat_defaults`]) or a caller-resolved,
/// file-derived triple (see this module's own doc comment, "What this module does NOT do").
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WeatherInputs {
    pub f107: f64,
    pub f107a: f64,
    pub kp: f64,
}

impl From<crate::weather::ConstantWeather> for WeatherInputs {
    fn from(c: crate::weather::ConstantWeather) -> Self {
        Self { f107: c.f107, f107a: c.f107a, kp: c.kp }
    }
}

fn check_finite3(p: [f64; 3]) -> bool {
    p.iter().all(|v| v.is_finite())
}

/// Vallado's iterative oblate-spheroid geodetic algorithm -- `AtmosphereModel::
/// CalculateGeodetics`'s own body, ported unchanged (`geodeticTolerance = 1.0e-7` radians,
/// the SAME convergence bound). `r_body_fixed_km` is the spacecraft position in the central
/// body's OWN body-fixed frame (see this module's own doc comment). Returns `(height_km,
/// geodetic_latitude_rad)`.
pub fn geodetic_height_lat_km(r_body_fixed_km: [f64; 3], cb: &CentralBodyGeodetics) -> (f64, f64) {
    let rxy = (r_body_fixed_km[0] * r_body_fixed_km[0] + r_body_fixed_km[1] * r_body_fixed_km[1]).sqrt();
    let mut geo_lat = r_body_fixed_km[2].atan2(rxy);
    let ecc2 = cb.flattening * (2.0 - cb.flattening);
    let geodetic_tolerance = 1.0e-7;
    let mut delta = 1.0_f64;
    while delta > geodetic_tolerance {
        let oldlat = geo_lat;
        let sinlat = oldlat.sin();
        let c_factor = cb.equatorial_radius_km / (1.0 - ecc2 * sinlat * sinlat).sqrt();
        geo_lat = (r_body_fixed_km[2] + c_factor * ecc2 * sinlat).atan2(rxy);
        delta = (geo_lat - oldlat).abs();
    }
    let sinlat = geo_lat.sin();
    let c_factor = cb.equatorial_radius_km / (1.0 - ecc2 * sinlat * sinlat).sqrt();
    let height = rxy / geo_lat.cos() - c_factor;
    (height, geo_lat)
}

/// Auxiliary results from [`exotherm`] that [`rho_100`]/[`rho_125`] need in addition to the
/// exospheric temperature itself -- GMAT's own `root1`/`root2`/`x_root`/`y_root`/`tx` member
/// variables, only meaningful when this call's `height < 125.0` (GMAT's own guard: the root
/// block is inside `if (height <= 125.0)`; [`rho_high`] (height > 125) never reads these
/// fields, only `t_infinity`/`tx`/`sum`).
#[derive(Debug, Clone, Copy)]
pub struct ExothermResult {
    pub exotemp: f64,
    pub t_infinity: f64,
    pub tx: f64,
    /// The `CON_L` polynomial evaluated at `t_infinity`, GMAT's own `sum` -- only set
    /// (nonzero-meaningful) when `height > 125.0`; [`rho_100`]/[`rho_125`] do not read it.
    pub sum_con_l: f64,
    pub root1: f64,
    pub root2: f64,
    pub x_root: f64,
    pub y_root: f64,
}

/// Newton's method on a complex polynomial, seeded from `guess` -- GMAT's own `roots`
/// (`JacchiaRobertsAtmosphere::roots`), ported unchanged (same convergence bound `1e-14`,
/// same iteration form). `a` is the polynomial's real coefficients, LOWEST degree first
/// (`a[0]` is the constant term), degree `a.len() - 1`.
fn newton_root(a: &[f64], guess: (f64, f64)) -> (f64, f64) {
    let n1 = a.len() - 1;
    let n2 = n1 - 1;
    let mut z = guess;
    loop {
        let mut cb = (a[n1], 0.0_f64);
        let mut cc = (a[n1], 0.0_f64);
        for i in 0..=n2 {
            let j = n2 - i;
            let temp = (z.0 * cb.0 - z.1 * cb.1) + a[j];
            cb.1 = z.0 * cb.1 + z.1 * cb.0;
            cb.0 = temp;
            if j != 0 {
                let temp2 = (z.0 * cc.0 - z.1 * cc.1) + cb.0;
                cc.1 = (z.0 * cc.1 + z.1 * cc.0) + cb.1;
                cc.0 = temp2;
            }
        }
        let zs = z;
        let denom = cc.0 * cc.0 + cc.1 * cc.1;
        z.0 -= (cb.0 * cc.0 + cb.1 * cc.1) / denom;
        z.1 += (cb.0 * cc.1 - cb.1 * cc.0) / denom;
        let mut dif = ((zs.0 - z.0) / zs.0).abs();
        if zs.1 != 0.0 {
            dif += ((zs.1 - z.1) / zs.1).abs();
        }
        if dif <= 1.0e-14 {
            break;
        }
    }
    z
}

/// Synthetic division by `(z - root)`, real coefficients -- GMAT's own `deflate_polynomial`,
/// ported unchanged. `c` is lowest-degree-first, length `n`; returns the length-`(n-1)`
/// quotient, also lowest-degree-first.
fn deflate_polynomial(c: &[f64], root: f64) -> Vec<f64> {
    let n = c.len();
    let mut c_new = vec![0.0_f64; n - 1];
    let mut sum = c[n - 1];
    for i in (0..=(n - 2)).rev() {
        let save = c[i];
        c_new[i] = sum;
        sum = save + sum * root;
    }
    c_new
}

/// `JacchiaRobertsAtmosphere::exotherm`, ported unchanged: the local exospheric temperature
/// at `height_km`, from the spacecraft's and Sun's (both UN-rotated, see this module's own
/// doc comment) positions, the geomagnetic index `kp` and minimum global exospheric
/// temperature `xtemp_k` (GMAT's own `geo.tkp`/`geo.xtemp`, computed by [`density_kg_m3`]
/// from [`WeatherInputs`] via the SAME `379.0 + 3.24*F107A + 1.3*(F107-F107A)` formula
/// `JacchiaRobertsAtmosphere.cpp`'s own weather-selection `switch` uses for every branch).
///
/// `sun_dec_rad`/`geo_lat_rad` are GMAT's own `exotherm` PARAMETERS (its real C++ signature
/// is `exotherm(space_craft, sun, geo, height, sun_dec, geo_lat)` -- the caller,
/// `JacchiaRoberts()`, computes both ONCE per `Density()` call and passes the SAME values into
/// every `exotherm` invocation that call makes (both the `height=500` and the actual-height
/// calls [`rho_high`] needs) -- `sun_dec` from the un-rotated Sun vector, `geo_lat` from the
/// class member `geoLat` a PRIOR `CalculateGeodetics` call (the body-fixed one) set. This
/// function therefore does NOT recompute either internally (an earlier version of this port
/// did, incorrectly reusing the un-rotated position for `geo_lat`, which would have made the
/// "geodetic latitude" spin with Earth's rotation in the inertial frame -- caught before
/// being exercised by any test, by re-reading GMAT's own `exotherm` signature rather than
/// trusting a first draft).
pub fn exotherm(r_sc_km: [f64; 3], r_sun_km: [f64; 3], kp: f64, xtemp_k: f64, height_km: f64, sun_dec_rad: f64, geo_lat_rad: f64) -> Result<ExothermResult, JacchiaRobertsError> {
    let sun_denom = (r_sun_km[0] * r_sun_km[0] + r_sun_km[1] * r_sun_km[1]).sqrt();
    let cross_denom = (r_sun_km[0] * r_sc_km[1] - r_sun_km[1] * r_sc_km[0]).abs();
    let cos_denom = (r_sc_km[0] * r_sc_km[0] + r_sc_km[1] * r_sc_km[1]).sqrt();
    const REAL_TOL: f64 = 1.0e-15; // GmatRealConstants::REAL_TOL, third_party/gmat-src/src/gmatutil/util/GmatConstants.hpp
    if cross_denom < REAL_TOL || cos_denom < REAL_TOL {
        return Err(JacchiaRobertsError::DegenerateHourAngleGeometry);
    }

    let error_tolerance = 1.0e-14_f64;
    let cos_alpha = (r_sun_km[0] * r_sc_km[0] + r_sun_km[1] * r_sc_km[1]) / (sun_denom * cos_denom);
    let hour_angle = if cos_alpha >= 1.0 - error_tolerance {
        0.0
    } else if cos_alpha <= -1.0 + error_tolerance {
        ((r_sun_km[0] * r_sc_km[1] - r_sun_km[1] * r_sc_km[0]) / cross_denom) * std::f64::consts::PI
    } else {
        ((r_sun_km[0] * r_sc_km[1] - r_sun_km[1] * r_sc_km[0]) / cross_denom) * cos_alpha.clamp(-1.0, 1.0).acos()
    };

    let sun_dec = sun_dec_rad;
    let geo_lat = geo_lat_rad;

    let theta = 0.5 * (geo_lat + sun_dec).abs();
    let eta = 0.5 * (geo_lat - sun_dec).abs();
    let mut tau = hour_angle - 0.645_771_823_25 + 0.104_719_755_12 * (hour_angle + 0.750_491_578_36).sin();
    if tau < -std::f64::consts::PI {
        tau += 2.0 * std::f64::consts::PI;
    } else if tau > std::f64::consts::PI {
        tau -= 2.0 * std::f64::consts::PI;
    }
    let th22 = theta.sin().powf(2.2);
    let t1 = xtemp_k * (1.0 + 0.3 * (th22 + (0.5 * tau).cos().powi(3) * (eta.cos().powf(2.2) - th22)));
    let expkp = kp.exp();

    let t_infinity = if height_km < 200.0 { t1 + 14.0 * kp + 0.02 * expkp } else { t1 + 28.0 * kp + 0.03 * expkp };
    let tx = 371.6678 + 0.0518806 * t_infinity - 294.3505 * (-0.00216222 * t_infinity).exp();

    let exotemp;
    let mut sum_con_l = 0.0;
    if height_km < 125.0 {
        let mut sum = CON_C[4];
        for i in (0..=3).rev() {
            sum = CON_C[i] + sum * height_km;
        }
        exotemp = tx + (tx - TZERO) * sum / 1.500625e6;
    } else if height_km > 125.0 {
        let mut sum = CON_L[4];
        for i in (0..=3).rev() {
            sum = CON_L[i] + sum * t_infinity;
        }
        sum_con_l = sum;
        exotemp = t_infinity - (t_infinity - tx) * (-(tx - TZERO) / (t_infinity - tx) * (height_km - 125.0) / 35.0 * sum / (CentralBodyGeodetics::earth_defaults().polar_radius_km() + height_km)).exp();
    } else {
        exotemp = tx;
    }

    let (mut root1, mut root2, mut x_root, mut y_root) = (0.0, 0.0, 0.0, 0.0);
    if height_km <= 125.0 {
        let mut c_star = [CON_C[0] + 1_500_625.0 * tx / (tx - TZERO), CON_C[1], CON_C[2], CON_C[3], CON_C[4]];
        let r1 = newton_root(&c_star, (125.0, 0.0));
        root1 = r1.0;
        let deflated1 = deflate_polynomial(&c_star, root1);
        c_star[..4].copy_from_slice(&deflated1);

        let r2 = newton_root(&c_star[..4], (200.0, 0.0));
        root2 = r2.0;
        let deflated2 = deflate_polynomial(&c_star[..4], root2);
        c_star[..3].copy_from_slice(&deflated2);

        let r3 = newton_root(&c_star[..3], (10.0, 125.0));
        x_root = r3.0;
        y_root = r3.1.abs();
    }

    Ok(ExothermResult { exotemp, t_infinity, tx, sum_con_l, root1, root2, x_root, y_root })
}

/// Auxiliary `u`/`w`/`v`/`x_star`/`roots_2` quantities shared by [`rho_100`]/[`rho_125`] --
/// functions of `exo`'s own roots and the central body's polar radius alone (GMAT's own
/// identical block, duplicated verbatim in both `rho_100` and `rho_125`; factored out once
/// here since the two callers' own formulas are otherwise unrelated).
struct RootFunctions {
    roots_2: f64,
    x_star: f64,
    v: f64,
    u: [f64; 2],
    w: [f64; 2],
}

fn root_functions(exo: &ExothermResult, cb_polar_km: f64) -> RootFunctions {
    let roots_2 = exo.x_root * exo.x_root + exo.y_root * exo.y_root;
    let x_star = -2.0 * exo.root1 * exo.root2 * cb_polar_km * (cb_polar_km * cb_polar_km + 2.0 * cb_polar_km * exo.x_root + roots_2);
    let v = (cb_polar_km + exo.root1) * (cb_polar_km + exo.root2) * (cb_polar_km * cb_polar_km + 2.0 * cb_polar_km * exo.x_root + roots_2);
    let u0 = (exo.root1 - exo.root2) * (exo.root1 + cb_polar_km).powi(2) * (exo.root1 * exo.root1 - 2.0 * exo.root1 * exo.x_root + roots_2);
    let u1 = (exo.root1 - exo.root2) * (exo.root2 + cb_polar_km).powi(2) * (exo.root2 * exo.root2 - 2.0 * exo.root2 * exo.x_root + roots_2);
    let w0 = exo.root1 * exo.root2 * cb_polar_km * (cb_polar_km + exo.root1) * (cb_polar_km + roots_2 / exo.root1);
    let w1 = exo.root1 * exo.root2 * cb_polar_km * (cb_polar_km + exo.root2) * (cb_polar_km + roots_2 / exo.root2);
    RootFunctions { roots_2, x_star, v, u: [u0, u1], w: [w0, w1] }
}

fn eval_poly_desc(coeffs_lowest_first: &[f64], z: f64) -> f64 {
    // Horner form, HIGHEST-degree-first evaluation to mirror GMAT's own `for (s_poly = b[5],
    // i=4; i>=0; i--) s_poly = s_poly*root + b[i]` loops exactly.
    let n = coeffs_lowest_first.len();
    let mut s = coeffs_lowest_first[n - 1];
    for i in (0..=(n - 2)).rev() {
        s = s * z + coeffs_lowest_first[i];
    }
    s
}

/// `JacchiaRobertsAtmosphere::rho_100`: raw density (g/cm^3) between 90 and 100 km, ported
/// unchanged. `exo` must have been computed for the SAME `height_km` (its `root1`/`root2`/
/// `x_root`/`y_root`/`tx` are what this formula needs).
pub fn rho_100(height_km: f64, exo: &ExothermResult, cb: &CentralBodyGeodetics) -> f64 {
    let cb_polar_km = cb.polar_radius_km();
    let m_poly = eval_poly_desc(&M_CON, height_km);
    let b: [f64; 6] = std::array::from_fn(|i| S_CON[i] + S_BETA[i] * exo.tx / (exo.tx - TZERO));
    let rf = root_functions(exo, cb_polar_km);

    let s_poly_root1 = eval_poly_desc(&b, exo.root1);
    let p2 = s_poly_root1 / rf.u[0];
    let s_poly_root2 = eval_poly_desc(&b, exo.root2);
    let p3 = -s_poly_root2 / rf.u[1];
    let s_poly_neg_cb = eval_poly_desc(&b, -cb_polar_km);
    let p5 = s_poly_neg_cb / rf.v;
    let p4 = (b[0] - exo.root1 * exo.root2 * cb_polar_km * cb_polar_km * (b[4] + b[5] * (2.0 * exo.x_root + exo.root1 + exo.root2 - cb_polar_km)) + rf.w[0] * p2 + rf.w[1] * p3 - exo.root1 * exo.root2 * b[5] * cb_polar_km * rf.roots_2 + exo.root1 * exo.root2 * (cb_polar_km * cb_polar_km - rf.roots_2) * p5) / rf.x_star;
    let p1 = b[5] - 2.0 * p4 - p3 - p2;
    let p6 = b[4] + b[5] * (2.0 * exo.x_root + exo.root1 + exo.root2 - cb_polar_km) - p5 - 2.0 * (exo.x_root + cb_polar_km) * p4 - (exo.root2 + cb_polar_km) * p3 - (exo.root1 + cb_polar_km) * p2;

    let log_f1 = p1 * ((height_km + cb_polar_km) / (90.0 + cb_polar_km)).ln() + p2 * ((height_km - exo.root1) / (90.0 - exo.root1)).ln() + p3 * ((height_km - exo.root2) / (90.0 - exo.root2)).ln()
        + p4 * ((height_km * height_km - 2.0 * exo.x_root * height_km + rf.roots_2) / (8100.0 - 180.0 * exo.x_root + rf.roots_2)).ln();
    let f2 = (height_km - 90.0) * (M_CON[6] + p5 / ((height_km + cb_polar_km) * (90.0 + cb_polar_km)))
        + p6 * (exo.y_root * (height_km - 90.0) / (exo.y_root * exo.y_root + (height_km - exo.x_root) * (90.0 - exo.x_root))).atan() / exo.y_root;

    let factor_k = -G_ZERO / (GAS_CON * (exo.tx - TZERO));

    RHO_ZERO * TZERO * m_poly * (factor_k * (log_f1 + f2)).exp() / (MZERO * exo.exotemp)
}

/// `JacchiaRobertsAtmosphere::rho_125`: raw density (g/cm^3) between 100 and 125 km, ported
/// unchanged.
pub fn rho_125(height_km: f64, exo: &ExothermResult, cb: &CentralBodyGeodetics) -> f64 {
    let cb_polar_km = cb.polar_radius_km();
    let rho_prime = eval_poly_desc(&ZETA_CON, exo.t_infinity);
    let t_100 = exo.tx + OMEGA * (exo.tx - TZERO);
    let rf = root_functions(exo, cb_polar_km);

    let q2 = 1.0 / rf.u[0];
    let q3 = -1.0 / rf.u[1];
    let q5 = 1.0 / rf.v;
    let q4 = (1.0 + rf.w[0] * q2 + rf.w[1] * q3 + exo.root1 * exo.root2 * (cb_polar_km * cb_polar_km - rf.roots_2) * q5) / rf.x_star;
    let q1 = -2.0 * q4 - q3 - q2;
    let q6 = -q5 - 2.0 * (exo.x_root + cb_polar_km) * q4 - (exo.root2 + cb_polar_km) * q3 - (exo.root1 + cb_polar_km) * q2;

    let log_f3 = q1 * ((height_km + cb_polar_km) / (100.0 + cb_polar_km)).ln() + q2 * ((height_km - exo.root1) / (100.0 - exo.root1)).ln() + q3 * ((height_km - exo.root2) / (100.0 - exo.root2)).ln()
        + q4 * ((height_km * height_km - 2.0 * exo.x_root * height_km + rf.roots_2) / (1.0e4 - 200.0 * exo.x_root + rf.roots_2)).ln();
    let f4 = (height_km - 100.0) * q5 / ((height_km + cb_polar_km) * (100.0 + cb_polar_km)) + q6 * (exo.y_root * (height_km - 100.0) / (exo.y_root * exo.y_root + (height_km - exo.x_root) * (100.0 - exo.x_root))).atan() / exo.y_root;

    let factor_k = -1_500_625.0 * G_ZERO * cb_polar_km * cb_polar_km / (GAS_CON * CON_C[4] * (exo.tx - TZERO));

    let mut rho_sum = 0.0;
    for i in 0..=4 {
        let mut rhoi = MOL_MASS[i] * NUM_DENS[i] * (MOL_MASS[i] * factor_k * (f4 + log_f3)).exp();
        if i == 2 {
            rhoi *= (t_100 / exo.exotemp).powf(-0.38);
        }
        rho_sum += rhoi;
    }
    rho_sum * rho_prime * t_100 / exo.exotemp
}

/// `JacchiaRobertsAtmosphere::rho_cor`: the geomagnetic/semiannual/seasonal-latitudinal
/// correction factor, ported unchanged. `gmat_mjd_days` is GMAT's own 2,430,000.0-based
/// Modified Julian Date -- this module's caller ([`density_kg_m3`]) passes
/// `crate::tdb::tai_ns_to_tt_mjd`'s result (TT, not UTC -- see this module's own doc comment
/// for why that sub-minute difference is negligible for this correction's own ~365-day
/// period).
pub fn rho_cor(height_km: f64, gmat_mjd_days: f64, geo_lat_rad: f64, kp: f64) -> f64 {
    let geo_cor = if height_km < 200.0 { 0.012 * kp + 0.000012 * kp.exp() } else { 0.0 };

    let f = (5.876e-7 * height_km.powf(2.331) + 0.06328) * (-0.002868 * height_km).exp();
    let day_58 = (gmat_mjd_days - 6204.5) / 365.2422;
    let tausa = day_58 + 0.09544 * ((0.5 * (1.0 + (2.0 * std::f64::consts::PI * day_58 + 6.035).sin())).powf(1.65) - 0.5);
    let alpha = (4.0 * std::f64::consts::PI * tausa + 4.259).sin();
    let g = 0.02835 + (0.3817 + 0.17829 * (2.0 * std::f64::consts::PI * tausa + 4.137).sin()) * alpha;
    let semian_cor = f * g;

    let sin_lat = geo_lat_rad.sin();
    let eta_lat = (2.0 * std::f64::consts::PI * day_58 + 1.72).sin() * sin_lat * sin_lat.abs();
    let slat_cor = 0.014 * (height_km - 90.0) * eta_lat * (-0.0013 * (height_km - 90.0) * (height_km - 90.0)).exp();

    10.0_f64.powf(geo_cor + semian_cor + slat_cor)
}

/// `JacchiaRobertsAtmosphere::rho_high`: raw density (g/cm^3) between 125 and 2500 km, ported
/// unchanged. `exo` is [`exotherm`] evaluated at `height_km` itself (its own `t_infinity`/
/// `tx`/`sum_con_l` are what this needs); `t_500_k` is the exospheric temperature at 500 km
/// (a SEPARATE [`exotherm`] call, GMAT's own `t_500 = exotherm(..., 500.0, ...)`).
pub fn rho_high(height_km: f64, exo: &ExothermResult, t_500_k: f64, sun_dec_rad: f64, geo_lat_rad: f64, cb: &CentralBodyGeodetics) -> f64 {
    let cb_polar_km = cb.polar_radius_km();
    let mut rho_out = 0.0;
    for i in 0..=5 {
        let mut di = 0.0;
        if i <= 4 {
            let log_di = eval_poly_desc(&CON_DEN[i], exo.t_infinity);
            di = 10.0_f64.powf(log_di) / AVOGADRO;
        }
        let polar125 = cb_polar_km + 125.0;
        let gamma = 35.0 * MOL_MASS[i] * G_ZERO * cb_polar_km * cb_polar_km * (exo.t_infinity - exo.tx) / (GAS_CON * exo.sum_con_l * exo.t_infinity * (exo.tx - TZERO) * polar125);
        let mut exp1 = 1.0 + gamma;
        let mut f = 1.0;
        if i == 2 {
            exp1 -= 0.38;
            let sign = if sun_dec_rad >= 0.0 { 1.0 } else { -1.0 };
            f = 4.9914 * sun_dec_rad.abs() * ((0.25 * std::f64::consts::PI - 0.5 * geo_lat_rad * sign).sin().powi(3) - 0.35355) / std::f64::consts::PI;
            f = 10.0_f64.powf(f);
        }
        if height_km > 500.0 && i == 5 {
            let r = MOL_MASS[5] * 10.0_f64.powf(73.13 - (39.4 - 5.5 * t_500_k.log10()) * t_500_k.log10()) * (t_500_k / exo.exotemp).powf(exp1) * ((exo.t_infinity - exo.exotemp) / (exo.t_infinity - t_500_k)).powf(gamma) / AVOGADRO;
            rho_out += r;
        } else if i <= 4 {
            let r = f * MOL_MASS[i] * di * (exo.tx / exo.exotemp).powf(exp1) * ((exo.t_infinity - exo.exotemp) / (exo.t_infinity - exo.tx)).powf(gamma);
            rho_out += r;
        }
    }
    rho_out
}

/// The raw (pre-[`rho_cor`]) density in g/cm^3 at `height_km`, dispatching to the four
/// altitude-banded formulas exactly as `JacchiaRobertsAtmosphere::JacchiaRoberts` does. Needs
/// a SECOND [`exotherm`] call at 500 km when `height_km > 125.0` (for [`rho_high`]'s own
/// `t_500`), exactly as GMAT computes it. `sun_dec_rad`/`geo_lat_rad` are computed ONCE by the
/// caller ([`density_kg_m3`]) and passed through unchanged to every `exotherm` call, matching
/// GMAT's own `JacchiaRoberts()` (see [`exotherm`]'s own doc comment).
fn raw_density_g_cm3(height_km: f64, r_sc_km: [f64; 3], r_sun_km: [f64; 3], kp: f64, xtemp_k: f64, sun_dec_rad: f64, geo_lat_rad: f64) -> Result<f64, JacchiaRobertsError> {
    let cb = CentralBodyGeodetics::earth_defaults();
    if height_km <= 90.0 {
        return Ok(RHO_ZERO);
    }
    if height_km < 100.0 {
        let exo = exotherm(r_sc_km, r_sun_km, kp, xtemp_k, height_km, sun_dec_rad, geo_lat_rad)?;
        return Ok(rho_100(height_km, &exo, &cb));
    }
    if height_km <= 125.0 {
        let exo = exotherm(r_sc_km, r_sun_km, kp, xtemp_k, height_km, sun_dec_rad, geo_lat_rad)?;
        return Ok(rho_125(height_km, &exo, &cb));
    }
    if height_km <= 2500.0 {
        let exo_500 = exotherm(r_sc_km, r_sun_km, kp, xtemp_k, 500.0, sun_dec_rad, geo_lat_rad)?;
        let t_500_k = exo_500.exotemp;
        let exo = exotherm(r_sc_km, r_sun_km, kp, xtemp_k, height_km, sun_dec_rad, geo_lat_rad)?;
        return Ok(rho_high(height_km, &exo, t_500_k, sun_dec_rad, geo_lat_rad, &cb));
    }
    Ok(0.0)
}

/// The public, SI entry point: atmospheric density in kg/m^3 at spacecraft position
/// `r_sc_m` (central-body-relative, whatever inertial frame the caller's state uses -- see
/// this module's own doc comment on why the un-rotated frame is correct here), given the
/// Sun's position `r_sun_m` in the SAME frame, this call's own body-fixed rotation `rotation`
/// (for the geodetic height/latitude only -- see "Geodetic height and latitude" above), the
/// epoch `t_tai_ns`, weather inputs and central-body shape.
///
/// GMAT's own hard floor: [`JacchiaRobertsError::BelowMinimumAltitude`] if the computed
/// height is `<= 100.0` km (`JacchiaRobertsAtmosphere::Density`'s own `AtmosphereException`).
pub fn density_kg_m3<R: BodyFixedRotation>(r_sc_m: [f64; 3], r_sun_m: [f64; 3], rotation: &R, t_tai_ns: i64, weather: &WeatherInputs, cb: &CentralBodyGeodetics) -> Result<f64, JacchiaRobertsError> {
    if !check_finite3(r_sc_m) {
        return Err(JacchiaRobertsError::NonFinitePosition(r_sc_m));
    }
    if !check_finite3(r_sun_m) {
        return Err(JacchiaRobertsError::NonFiniteSunPosition(r_sun_m));
    }
    if !(weather.f107.is_finite() && weather.f107a.is_finite() && weather.kp.is_finite()) {
        return Err(JacchiaRobertsError::InvalidWeather { f107: weather.f107, f107a: weather.f107a, kp: weather.kp });
    }
    if !(cb.equatorial_radius_km.is_finite() && cb.equatorial_radius_km > 0.0 && cb.flattening.is_finite() && (0.0..1.0).contains(&cb.flattening)) {
        return Err(JacchiaRobertsError::InvalidCentralBody { equatorial_radius_km: cb.equatorial_radius_km, flattening: cb.flattening });
    }

    let r_sc_km = [r_sc_m[0] / 1000.0, r_sc_m[1] / 1000.0, r_sc_m[2] / 1000.0];
    let r_sun_km = [r_sun_m[0] / 1000.0, r_sun_m[1] / 1000.0, r_sun_m[2] / 1000.0];

    let rot = rotation.inertial_to_fixed(t_tai_ns).map_err(|e| JacchiaRobertsError::Rotation(e.to_string()))?;
    let r_sc_body_fixed_km = rot.apply(r_sc_km);
    // GMAT computes geoLat/height together, once, off the body-fixed position
    // (CalculateGeodetics) -- reused for BOTH height_km (the altitude-band dispatch below)
    // and geo_lat_rad (exotherm's/rho_cor's own parameter), exactly one call.
    let (height_km, geo_lat_rad) = geodetic_height_lat_km(r_sc_body_fixed_km, cb);

    if height_km <= 100.0 {
        return Err(JacchiaRobertsError::BelowMinimumAltitude(height_km));
    }

    // geo.xtemp = 379.0 + 3.24*F107A + 1.3*(F107-F107A); geo.tkp = Kp -- the CONSTANT-mode
    // formula every branch of JacchiaRobertsAtmosphere.cpp's own weather-selection switch uses
    // (see this module's own doc comment: file-mode day/slot selection is crate::weather's,
    // not this function's, job).
    let xtemp_k = 379.0 + 3.24 * weather.f107a + 1.3 * (weather.f107 - weather.f107a);
    let kp = weather.kp;

    // sun_dec, computed ONCE (GMAT's own JacchiaRoberts() computes it once per Density() call
    // and passes it unchanged into every exotherm call -- see exotherm's own doc comment),
    // from the un-rotated Sun vector.
    let sun_dec_rad = r_sun_km[2].atan2((r_sun_km[0] * r_sun_km[0] + r_sun_km[1] * r_sun_km[1]).sqrt());

    let raw_g_cm3 = raw_density_g_cm3(height_km, r_sc_km, r_sun_km, kp, xtemp_k, sun_dec_rad, geo_lat_rad)?;

    let gmat_mjd_days = crate::tdb::tai_ns_to_tt_mjd(t_tai_ns);
    let corrected_g_cm3 = raw_g_cm3 * rho_cor(height_km, gmat_mjd_days, geo_lat_rad, kp);

    let density_kg_m3 = corrected_g_cm3 * 1.0e3;
    if !density_kg_m3.is_finite() {
        return Err(JacchiaRobertsError::NonFiniteResult(density_kg_m3));
    }
    Ok(density_kg_m3)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::Rotation;

    struct IdentityRotation;
    impl BodyFixedRotation for IdentityRotation {
        type Error = std::convert::Infallible;
        fn inertial_to_fixed(&self, _t_tai_ns: i64) -> Result<Rotation, Self::Error> {
            Ok(Rotation { r: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]], r_dot: [[0.0; 3]; 3] })
        }
    }

    const AU_KM: f64 = 149_597_870.691;

    fn sun_km() -> [f64; 3] {
        [AU_KM * 0.9, AU_KM * 0.3, AU_KM * 0.1]
    }

    #[test]
    fn density_decreases_with_altitude_in_the_high_altitude_band() {
        let cb = CentralBodyGeodetics::earth_defaults();
        let weather = WeatherInputs::from(crate::weather::ConstantWeather::gmat_defaults());
        let mut last = f64::INFINITY;
        for alt_km in [200.0, 400.0, 600.0, 800.0, 1000.0] {
            let r_km = alt_km + cb.equatorial_radius_km;
            let r_sc_m = [r_km * 1000.0, 0.0, 0.0];
            let rho = density_kg_m3(r_sc_m, sun_km_m(), &IdentityRotation, 1_800_000_000_000_000_000, &weather, &cb).unwrap();
            assert!(rho > 0.0 && rho.is_finite(), "density at {alt_km} km must be positive and finite, got {rho}");
            assert!(rho < last, "density must decrease with altitude: {alt_km} km gave {rho}, previous was {last}");
            last = rho;
        }
    }

    fn sun_km_m() -> [f64; 3] {
        let s = sun_km();
        [s[0] * 1000.0, s[1] * 1000.0, s[2] * 1000.0]
    }

    #[test]
    fn below_100km_is_a_typed_error_not_a_panic() {
        let cb = CentralBodyGeodetics::earth_defaults();
        let weather = WeatherInputs::from(crate::weather::ConstantWeather::gmat_defaults());
        let r_sc_m = [(cb.equatorial_radius_km + 50.0) * 1000.0, 0.0, 0.0];
        let err = density_kg_m3(r_sc_m, sun_km_m(), &IdentityRotation, 0, &weather, &cb).unwrap_err();
        assert!(matches!(err, JacchiaRobertsError::BelowMinimumAltitude(_)));
    }

    #[test]
    fn nan_position_is_a_typed_error_not_a_panic() {
        let cb = CentralBodyGeodetics::earth_defaults();
        let weather = WeatherInputs::from(crate::weather::ConstantWeather::gmat_defaults());
        let err = density_kg_m3([f64::NAN, 0.0, 0.0], sun_km_m(), &IdentityRotation, 0, &weather, &cb).unwrap_err();
        assert!(matches!(err, JacchiaRobertsError::NonFinitePosition(_)));
    }

    #[test]
    fn nonfinite_weather_is_a_typed_error_not_a_panic() {
        let cb = CentralBodyGeodetics::earth_defaults();
        let weather = WeatherInputs { f107: f64::NAN, f107a: 150.0, kp: 3.0 };
        let r_sc_m = [(cb.equatorial_radius_km + 400.0) * 1000.0, 0.0, 0.0];
        let err = density_kg_m3(r_sc_m, sun_km_m(), &IdentityRotation, 0, &weather, &cb).unwrap_err();
        assert!(matches!(err, JacchiaRobertsError::InvalidWeather { .. }));
    }

    #[test]
    fn geodetic_height_matches_a_simple_case_at_the_equator() {
        // At the equator with a spherical (flattening=0) body, geodetic height/latitude
        // reduce to the trivial geocentric case: height = r - equatorial_radius, lat = 0.
        let cb = CentralBodyGeodetics { equatorial_radius_km: 6378.1363, flattening: 0.0 };
        let (h, lat) = geodetic_height_lat_km([7378.1363, 0.0, 0.0], &cb);
        assert!((h - 1000.0).abs() < 1e-6, "height={h}");
        assert!(lat.abs() < 1e-9, "lat={lat}");
    }

    #[test]
    fn higher_kp_increases_density_at_a_fixed_altitude() {
        // A higher geomagnetic index heats the exosphere (t_infinity += 14*kp/28*kp + ...),
        // which must increase density at a fixed high altitude (rho_high's own exponential
        // temperature dependence) -- a physical sanity check, not a GMAT comparison.
        let cb = CentralBodyGeodetics::earth_defaults();
        let r_sc_m = [(cb.equatorial_radius_km + 500.0) * 1000.0, 0.0, 0.0];
        let quiet = WeatherInputs { f107: 150.0, f107a: 150.0, kp: 0.0 };
        let storm = WeatherInputs { f107: 150.0, f107a: 150.0, kp: 6.0 };
        let rho_quiet = density_kg_m3(r_sc_m, sun_km_m(), &IdentityRotation, 0, &quiet, &cb).unwrap();
        let rho_storm = density_kg_m3(r_sc_m, sun_km_m(), &IdentityRotation, 0, &storm, &cb).unwrap();
        println!("n3-jr-kp-sensitivity: rho(Kp=0)={rho_quiet:e} rho(Kp=6)={rho_storm:e}");
        assert!(rho_storm > rho_quiet, "higher Kp must increase density at 500 km: quiet={rho_quiet:e} storm={rho_storm:e}");
    }

    #[test]
    fn higher_f107_increases_density_at_a_fixed_altitude() {
        let cb = CentralBodyGeodetics::earth_defaults();
        let r_sc_m = [(cb.equatorial_radius_km + 500.0) * 1000.0, 0.0, 0.0];
        let low = WeatherInputs { f107: 90.0, f107a: 90.0, kp: 3.0 };
        let high = WeatherInputs { f107: 250.0, f107a: 250.0, kp: 3.0 };
        let rho_low = density_kg_m3(r_sc_m, sun_km_m(), &IdentityRotation, 0, &low, &cb).unwrap();
        let rho_high_v = density_kg_m3(r_sc_m, sun_km_m(), &IdentityRotation, 0, &high, &cb).unwrap();
        println!("n3-jr-f107-sensitivity: rho(F10.7=90)={rho_low:e} rho(F10.7=250)={rho_high_v:e}");
        assert!(rho_high_v > rho_low, "higher F10.7 must increase density at 500 km: low={rho_low:e} high={rho_high_v:e}");
    }
}
