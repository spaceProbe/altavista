//! The MSISE-90 atmosphere density model (task 3c, deliverable 2: `docs/native-dynamics-plan.md`
//! N3's own wording asks for "Jacchia-Roberts and MSISE-00" -- **GMAT R2026a does not ship
//! NRLMSISE-00.** Verified directly, not assumed: `strings` over the shipped
//! `GMAT R2026a/bin/GMAT-R2026a_Beta.app/Contents/Frameworks/libGmatBase.dylib` yields the class
//! symbols `JacchiaRobertsAtmosphere`, `Msise90Atmosphere`, `ExponentialAtmosphere`,
//! `SimpleExponentialAtmosphere` and the string `MarsGRAM2005` -- **no `NRLMSISE`/`Nrlmsise`/
//! `MSISE00` symbol of any kind anywhere in the library.** The mirrored source tree confirms it
//! structurally: `third_party/gmat-src/src/base/solarsys/` contains `Msise90Atmosphere.cpp`/
//! `.hpp` plus the model's own numerical core, `msise90_sub.for` (2205 lines of 1990-vintage
//! FORTRAN, function names `GTD6`/`GTS6`/`GLOBE6`, the MSIS-90 naming convention -- NRLMSISE-00's
//! own 2001 revision renamed these `GTD7`/`GTS7`/`GLOBE7S`, which do not appear anywhere in this
//! tree) and `msise90_sub.c` (the SAME algorithm, f2c-translated -- the form GMAT actually links
//! and executes). **The manager's decision, implemented here: build MSISE90, the model GMAT
//! actually has**, per charter decision 222(d) (every force needs a GMAT golden on this host,
//! and a native NRLMSISE-00 port would have no GMAT reference to pin against at all). This
//! module's own source of truth is `msise90_sub.c` -- GMAT's own shipped C, not the archival
//! `.for` file and not a published NRLMSISE-00/MSIS-90 listing recalled from memory, matching
//! [`crate::jacchia_roberts`]'s identical rule.
//!
//! # Scope: mass=48 (total density), constant switches, daily Ap, altitude >= za (~122.8 km)
//!
//! GMAT's own `Msise90Atmosphere::Density` (`Msise90Atmosphere.cpp`) calls `gtd6_` with **`mass
//! = 48`** always (GMAT's own hardcoded `mass = 48;`, "MASS 48 FOR ALL" per `msise90_sub.c`'s own
//! header comment) and reads back only `xden[5]` (`D(6)`, TOTAL MASS DENSITY) -- never a
//! per-species number density, never temperature. This module therefore implements ONLY the
//! `mass == 48` code path (every other `MASS` branch in `gtd6_`/`gts6_` -- `ghp6_`, the
//! individual-species-only branches -- is unreachable from GMAT's own wrapper and is not ported).
//!
//! **Switches (`SW`/`SWC`, GMAT's own `TSELEC`).** GMAT's wrapper never calls `TSELEC` itself, so
//! `gtd6_`'s own hardcoded default (`static real sv[25] = {1.,1.,...,1.}`, all-on) is what always
//! runs -- confirmed by reading `gtd6_`'s own initialized-data block. `TSELEC`'s own logic
//! (`msise90_sub.c`) is `sw[i] = fmod(sv[i], 2)` and `swc[i] = 1 if |sv[i]| is 1 or 2 else 0`:
//! with every `sv[i] = 1.0`, `sw[i] = 1.0` and `swc[i] = 1.0` for EVERY `i`, unconditionally. This
//! module hardcodes that result rather than porting `TSELEC` itself: every `if (sw[k] == 0)`
//! branch in `msise90_sub.c` is therefore DEAD for GMAT's own usage (never taken), so this port
//! always computes every term those branches guard -- a verified simplification, not a guess.
//!
//! **`AP`: a single scalar (`SW(9) = 1`, "daily AP" mode), not the 7-element hourly array.**
//! `gtd6_`'s own adjustment (`if (ap[i] != ap[0]) sv[8] = -1.0`) only flips to the hourly-array
//! mode (`SW(9) = -1`) when the 7 elements differ -- and GMAT's own `AtmosphereModel::GetInputs`
//! (`ap[i] = constantAp` for every `i` in constant-flux mode, confirmed by reading
//! `AtmosphereModel.cpp`) always sets all 7 elements EQUAL, so `SW(9)` stays `+1` and `GLOBE6`'s
//! own `SW(9) == -1` branch (the complex 7-term exponential-decay smoothing over `AP(2..7)`,
//! `msise90_sub.c` lines ~1934-2068) is dead code for every arc this crate flies -- not ported;
//! only the `SW(9) == 1` "daily AP" formula (`apd = AP(1) - 4`, `apdf` from `P(44)`/`P(45)`) is.
//! **`AP` is the geomagnetic AMPLITUDE, not `Kp`** -- GMAT's own `AtmosphereModel::ConvertKpToAp`
//! (table-lookup method, the class's own default, `kpApConversion = 0`) converts the DRM's `Kp`
//! to `Ap` before MSISE90 ever sees it; [`kp_to_ap`] ports that table verbatim, and for this
//! crate's own constant-weather default (`Kp = 3.0`) it resolves to `Ap = 15.0` exactly, read
//! from the live table (case index 9, the table's own documented default row).
//!
//! **Altitude floor: `za` (GMAT's own `PDL(41)`, ~122.8 km -- read from the data, not
//! hand-typed), a DELIBERATE scope boundary, not a hard technical wall.** `gtd6_` itself only
//! calls `GTS6` (this module's own scope) for altitudes `>= 72.5 km` (`ZN2(1)`), handing
//! anything below that to a SEPARATE mesosphere/troposphere blend (`DENSM`, a different
//! function with its own temperature-node fit) this module does not port -- so a DRM asking for
//! density below 72.5 km would get the wrong answer from this module regardless of the floor
//! below. **`DENSU` itself -- the function [`densu`] ports -- is implemented in FULL**, both its
//! `alt >= za` closed-form Bates branch and its `alt < za` cubic-spline extension (needing
//! `GLOB6S`/`SPLINE`/`SPLINT`/`SPLINI`, all ported): this is NOT optional even when the
//! SPACECRAFT's own altitude is above `za` -- `GTS6`'s own turbopause "mixed density at Zlb"
//! sub-calls pass `DENSU` a species-specific REFERENCE altitude (`ZH04`/`ZH28`/... ~100-110 km)
//! that is virtually always below `za` regardless of where the spacecraft actually is, so the
//! spline branch is exercised on nearly every call this module makes, even at LEO altitudes of
//! several hundred km. **An earlier draft of this port implemented only the `alt >= za` branch,
//! incorrectly reasoning it was dead code below this module's own floor -- it produced `NaN` at
//! every altitude this module's own test ladder tried, caught immediately (before ever reaching
//! a GMAT comparison) and root-caused by tracing exactly which `densu` call produced it; see
//! this task's own report.** With `DENSU` correct, this module's own floor is kept at `za`
//! anyway -- not because the code requires it (it is now believed correct some way below `za`
//! too, down to `GTS6`'s own 72.5 km dispatch boundary), but because this task's own goldens and
//! tests only exercise altitudes `>= 150 km`, and extending the CLAIMED-correct floor to 72.5 km
//! without measuring agreement there (`gtd6_`'s own mesosphere/`DENSM` dispatch at exactly
//! `ZN2(1)` is a real boundary this module has not independently verified) would be exactly the
//! kind of unmeasured extrapolation this task's own rule warns against ("a correct partial with
//! a named gap is worth far more than a guessed whole") -- [`MsiseError::BelowMinimumAltitude`]
//! below `za`, matching [`crate::jacchia_roberts`]'s identical "typed error below the model's
//! own valid floor" pattern (that module's floor is GMAT's own EXPLICIT 100 km
//! `AtmosphereException`; this module's is a conservative, DELIBERATELY-drawn floor, not a
//! technical one). A DRM needing MSISE90 density between 72.5 km and `za` would need this
//! module's own floor lifted and measured against GMAT there first -- not attempted here.
//! # Weather: reuses `crate::jacchia_roberts::WeatherInputs`, `Kp` converted to `Ap` internally
//!
//! [`density_kg_m3`] takes the SAME [`crate::jacchia_roberts::WeatherInputs`] (`f107`/`f107a`/
//! `kp`) [`crate::drag::DragBinding`]/[`crate::model::EarthGravityModel::with_drag`] already use
//! for Jacchia-Roberts -- "an atmosphere choice beside Jacchia-Roberts" (this task's own brief)
//! means the SAME weather type, not a second one a DRM would have to pick between. [`kp_to_ap`]
//! does the `Kp -> Ap` conversion this module alone needs.
//!
//! # Geodetics: reuses `crate::jacchia_roberts`'s own body-fixed height/latitude
//!
//! `Msise90Atmosphere::Density` calls the SAME `AtmosphereModel::CalculateGeodetics` (base-class)
//! method `JacchiaRobertsAtmosphere::Density` does -- confirmed by reading `AtmosphereModel.cpp`:
//! identical Vallado iterative oblate-spheroid algorithm, identical `1.0e-7` radian tolerance --
//! so [`density_kg_m3`] reuses [`crate::jacchia_roberts::geodetic_height_lat_km`] rather than a
//! second, independent port of the same algorithm (this module adds only the LONGITUDE this
//! model additionally needs for local solar time, `atan2(y_bf, x_bf)`, `CalculateGeodetics`'s own
//! formula, read from the same function).
//!
//! # Epoch: GMAT's own (non-calendar) year/day-of-year formula, not a Gregorian calendar
//!
//! `AtmosphereModel::GetInputs` derives `IYD`/`SEC` from a UTC epoch expressed as GMAT's own
//! `A1MJD`-style day count from `JD_JAN_5_1941` (`2,430,000.0`) by its OWN formula --
//! `yearOffset = floor((epoch+5.5)/365.25)`, `year = 1941+yearOffset`, `doy = floor(epoch) -
//! floor(yearOffset*365.25) + 5` -- NOT a real Gregorian calendar breakdown (`DAYS_PER_YEAR =
//! 365.25` is GMAT's own constant, `GmatConstants.hpp`). [`year_doy_sod`] ports this formula
//! verbatim, read from `AtmosphereModel.cpp` directly, term for term -- a genuine GMAT-ism, not a
//! standard calendar function this crate could get from any date library.
//!
//! # No `gmat-sys` dependency, no panics
//!
//! Like [`crate::jacchia_roberts`], this module has no `gmat-sys` dependency (builds and
//! unit-tests under `cargo test -p av-orbital --no-default-features`) and returns [`MsiseError`]
//! rather than panicking on any input a DRM could supply.

use crate::frame::BodyFixedRotation;
use crate::jacchia_roberts::{geodetic_height_lat_km, CentralBodyGeodetics, WeatherInputs};
use crate::msise90_data::{LOWER6, PARM6};

// ---------------------------------------------------------------------------------------------
// GMAT's own coefficient sub-arrays, sliced at the SAME flat offsets `msise90_sub.c` uses (see
// `msise90_data.rs`'s own doc comment) -- only the slices this module's own reduced scope (see
// this module's own doc comment) actually reads.
// ---------------------------------------------------------------------------------------------
// Plain `fn`s, not `const`s: slicing a `[f64; N]` by a range is not yet a stable `const`
// operation (indexing by a single literal is -- see `ZA_KM` below), so these sub-array VIEWS
// are computed at runtime (a trivial slice-of-a-static, not a re-copy).
fn ptm() -> &'static [f64] {
    &LOWER6[0..10]
}
fn pdm() -> &'static [f64] {
    &LOWER6[10..90]
}
fn pt() -> &'static [f64] {
    &PARM6[0..150]
}
fn pd() -> &'static [f64] {
    &PARM6[150..1500]
}
fn ps() -> &'static [f64] {
    &PARM6[1500..1650]
}
fn pdl() -> &'static [f64] {
    &PARM6[1650..1700]
}
fn ptl() -> &'static [f64] {
    &PARM6[1700..2100]
}
fn pma() -> &'static [f64] {
    &PARM6[2100..3100]
}

/// GMAT's own `GTS6`'s lower thermosphere joining ("Bates") altitude, `PDL(41)` --
/// `gts3c_1.za = parm6_1.pdl[40];` in `msise90_sub.c`, read from the data (not hand-typed) --
/// see this module's own doc comment, "Altitude floor".
pub fn za_km() -> f64 {
    PARM6[1650 + 40]
}

const DGTR: f64 = 0.0174533;
const DR: f64 = 0.0172142;
const HR: f64 = 0.2618;
const SR: f64 = 7.2722e-5;
const RGAS: f64 = 831.4;

/// Every way this module's functions can fail. Typed throughout -- no panic on any input a DRM
/// could supply (this crate's own rule).
#[derive(Debug, Clone, thiserror::Error, PartialEq)]
pub enum MsiseError {
    /// The spacecraft position was not finite.
    #[error("spacecraft position is not finite: {0:?}")]
    NonFinitePosition([f64; 3]),
    /// This module's own derived floor (see this module's own doc comment, "Altitude floor") --
    /// NOT GMAT's own explicit floor (GMAT's `GTS6` itself has no altitude floor above the
    /// mesosphere; this is where THIS module's own reduced scope stops applying).
    #[error("MSISE90 (this port's own scope) is not available below za = {za} km altitude (computed height {height} km) -- see crate::msise90's own module doc, \"Altitude floor\"")]
    BelowMinimumAltitude { height: f64, za: f64 },
    /// `F107`/`F107A`/`Kp` must be finite.
    #[error("weather inputs must be finite, got F107={f107} F107A={f107a} Kp={kp}")]
    InvalidWeather { f107: f64, f107a: f64, kp: f64 },
    /// The central body's equatorial radius/flattening must be finite and physically sane.
    #[error("central-body geodetic parameters invalid: equatorial radius {equatorial_radius_km} km, flattening {flattening}")]
    InvalidCentralBody { equatorial_radius_km: f64, flattening: f64 },
    /// The body-fixed rotation this call needed failed.
    #[error("body-fixed rotation failed: {0}")]
    Rotation(String),
    /// The computed density was not finite despite every input passing the checks above.
    #[error("computed density is not finite: {0}")]
    NonFiniteResult(f64),
}

/// GMAT's own `AtmosphereModel::ConvertKpToAp`, table-lookup method (`kpApConversion = 0`, the
/// class's own default -- `AtmosphereModel.cpp`'s own constructor, `kpApConversion (0)`), ported
/// verbatim: Vallado 3rd edition Table 8-3, `index = (Integer)((kp + .01) * 3)`. Returns `15.0`
/// (the table's own default row, `case 9`/`default`) for any index this table does not name.
pub fn kp_to_ap(kp: f64) -> f64 {
    let index = ((kp + 0.01) * 3.0) as i64;
    match index {
        0 => 0.0,
        1 => 2.0,
        2 => 3.0,
        3 => 4.0,
        4 => 5.0,
        5 => 6.0,
        6 => 7.0,
        7 => 9.0,
        8 => 12.0,
        10 => 18.0,
        11 => 22.0,
        12 => 27.0,
        13 => 32.0,
        14 => 39.0,
        15 => 48.0,
        16 => 56.0,
        17 => 67.0,
        18 => 80.0,
        19 => 94.0,
        20 => 111.0,
        21 => 132.0,
        22 => 154.0,
        23 => 179.0,
        24 => 207.0,
        25 => 236.0,
        26 => 300.0,
        27 => 400.0,
        _ => 15.0, // case 9 and every other unlisted index -- GMAT's own "default: ap = 15.0"
    }
}

/// The "daily AP" smoothed magnetic-activity term (`GLOBE6`'s own `SW(9) == 1` branch, `apd`/
/// `apdf`), factored out so [`gts6_mass48`] can reproduce the SAME value [`globe6`]'s own `TLB`
/// call leaves in GMAT's shared `lpoly` struct, for [`tn1_nodes_and_tgn1_2`]/[`glob6s`] to reuse
/// explicitly (see that function's own doc comment for exactly why).
fn apdf_daily(ap_daily: f64, p44_raw: f64, p45: f64) -> f64 {
    let apd = ap_daily - 4.0;
    let p44 = if p44_raw < 0.0 { 1e-5 } else { p44_raw };
    apd + (p45 - 1.0) * (apd + ((-p44 * apd).exp() - 1.0) / p44)
}

/// GMAT's own `glatf_`: latitude-variable gravity (cm/s^2) and effective Earth radius (km) --
/// ported unchanged.
fn glatf(lat_deg: f64) -> (f64, f64) {
    let c2 = (2.0 * DGTR * lat_deg).cos();
    let gv = (1.0 - c2 * 0.0026373) * 980.616;
    let reff = gv * 2.0 / (c2 * 2.27e-9 + 3.085462e-6) * 1e-5;
    (gv, reff)
}

/// GMAT's own year/day-of-year/seconds-of-day derivation, `AtmosphereModel::GetInputs`, ported
/// verbatim -- see this module's own doc comment, "Epoch". `epoch_days_since_1941` is UTC,
/// expressed as GMAT's own day count from `JD_JAN_5_1941` (`2,430,000.0`) -- i.e. the SAME
/// day-count convention as `Tai::to_a1_mjd`, but for the UTC time scale, not A.1.
fn year_doy_sod(epoch_days_since_1941: f64) -> (i64, i64, f64) {
    const DAYS_PER_YEAR: f64 = 365.25;
    const SECS_PER_DAY: f64 = 86_400.0;
    let epoch = epoch_days_since_1941;
    let i_epoch = epoch.trunc() as i64;
    let year_offset = ((epoch + 5.5) / DAYS_PER_YEAR).trunc() as i64;
    let year = 1941 + year_offset;
    let mut doy = i_epoch - (year_offset as f64 * DAYS_PER_YEAR).trunc() as i64 + 5;
    let mut sod = SECS_PER_DAY * (epoch - i_epoch as f64 + 0.5);
    if sod < 0.0 {
        sod += SECS_PER_DAY;
        doy -= 1;
    }
    if sod > SECS_PER_DAY {
        sod -= SECS_PER_DAY;
        doy += 1;
    }
    (year, doy, sod)
}

/// GMAT's own `SPLINE` (Numerical Recipes cubic-spline second-derivative setup), ported
/// unchanged, 0-based throughout (`x[i]`/`y[i]` here are Fortran `X(i+1)`/`Y(i+1)`; every
/// formula below was independently re-derived from the 1-based source term for term to confirm
/// the index mapping, not merely transcribed -- see this task's own report).
fn spline(x: &[f64], y: &[f64], yp1: f64, ypn: f64) -> Vec<f64> {
    let n = x.len();
    let mut y2 = vec![0.0_f64; n];
    let mut u = vec![0.0_f64; n - 1];
    if yp1 > 9.9e29 {
        y2[0] = 0.0;
        u[0] = 0.0;
    } else {
        y2[0] = -0.5;
        u[0] = 3.0 / (x[1] - x[0]) * ((y[1] - y[0]) / (x[1] - x[0]) - yp1);
    }
    for i in 1..(n - 1) {
        let sig = (x[i] - x[i - 1]) / (x[i + 1] - x[i - 1]);
        let p = sig * y2[i - 1] + 2.0;
        y2[i] = (sig - 1.0) / p;
        u[i] = (((y[i + 1] - y[i]) / (x[i + 1] - x[i]) - (y[i] - y[i - 1]) / (x[i] - x[i - 1])) * 6.0 / (x[i + 1] - x[i - 1]) - sig * u[i - 1]) / p;
    }
    let (qn, un) = if ypn > 9.9e29 {
        (0.0, 0.0)
    } else {
        (0.5, 3.0 / (x[n - 1] - x[n - 2]) * (ypn - (y[n - 1] - y[n - 2]) / (x[n - 1] - x[n - 2])))
    };
    y2[n - 1] = (un - qn * u[n - 2]) / (qn * y2[n - 2] + 1.0);
    for k in (0..n - 1).rev() {
        y2[k] = y2[k] * y2[k + 1] + u[k];
    }
    y2
}

/// GMAT's own `SPLINT` (cubic-spline interpolated value), ported unchanged, 0-based.
fn splint(xa: &[f64], ya: &[f64], y2a: &[f64], x: f64) -> f64 {
    let n = xa.len();
    let mut klo = 0usize;
    let mut khi = n - 1;
    while khi - klo > 1 {
        let k = (khi + klo) / 2;
        if xa[k] > x {
            khi = k;
        } else {
            klo = k;
        }
    }
    let h = xa[khi] - xa[klo];
    let a = (xa[khi] - x) / h;
    let b = (x - xa[klo]) / h;
    a * ya[klo] + b * ya[khi] + ((a * a * a - a) * y2a[klo] + (b * b * b - b) * y2a[khi]) * h * h / 6.0
}

/// GMAT's own `SPLINI` (cubic-spline integral from `xa[0]` to `x`), ported unchanged, 0-based.
fn splini(xa: &[f64], ya: &[f64], y2a: &[f64], x: f64) -> f64 {
    let n = xa.len();
    let mut yi = 0.0_f64;
    let mut klo = 0usize;
    let mut khi = 1usize;
    while x > xa[klo] && khi < n {
        let xx = if khi < n - 1 { x.min(xa[khi]) } else { x };
        let h = xa[khi] - xa[klo];
        let a = (xa[khi] - xx) / h;
        let b = (xx - xa[klo]) / h;
        let a2 = a * a;
        let b2 = b * b;
        yi += ((1.0 - a2) * ya[klo] / 2.0 + b2 * ya[khi] / 2.0 + ((-(a2 * a2 + 1.0) / 4.0 + a2 / 2.0) * y2a[klo] + (b2 * b2 / 4.0 - b2 / 2.0) * y2a[khi]) * h * h / 6.0) * h;
        klo += 1;
        khi += 1;
    }
    yi
}

/// The per-call geophysical/epoch inputs every [`globe6`]/[`glob6s`] call within ONE
/// `Density()` evaluation shares (only the coefficient slice `p` differs call to call) --
/// bundled into one struct so [`globe6`]/[`glob6s`]/[`tn1_nodes_and_tgn1_2`]/[`gts6_mass48`]
/// stay under clippy's argument-count lint without an `#[allow]` (this crate's own rule), not
/// because GMAT's own source groups them this way (it does not; this is this port's own,
/// additive organisation of the identical inputs).
#[derive(Clone, Copy)]
struct Geophysical {
    day: f64,
    sec: f64,
    lat_deg: f64,
    long_deg: f64,
    tloc: f64,
    f107a: f64,
    f107: f64,
    ap_daily: f64,
}

/// GMAT's own `GLOB6S` ("version of GLOBE for lower atmosphere"), ported from `msise90_sub.c`'s
/// `glob6s_`, with the identical simplifications [`globe6`]'s own doc comment names (every
/// switch on, `SW(9) == 1` only). `apdf` is the "daily AP" smoothed term -- see
/// [`tn1_nodes_and_tgn1_2`]'s own doc comment for exactly which `P(44)`/`P(45)` it is computed
/// from (GLOB6S itself does not recompute it; it reads the SAME shared value the LAST `GLOBE6`
/// call before it left behind -- `GTS6`'s own call order, matched here by the caller passing
/// the identical value through explicitly rather than via shared state).
fn glob6s(geo: &Geophysical, apdf: f64, parr: &[f64]) -> f64 {
    let day = geo.day;
    let long_deg = geo.long_deg;
    let tloc = geo.tloc;
    let f107a = geo.f107a;
    let p = |i: usize| parr[i - 1];
    let plg = legendre(geo.lat_deg);
    let (stloc, ctloc) = (HR * tloc).sin_cos();
    let (s2tloc, c2tloc) = (HR * 2.0 * tloc).sin_cos();
    let (s3tloc, c3tloc) = (HR * 3.0 * tloc).sin_cos();
    let clong = (DGTR * long_deg).cos();
    let slong = (DGTR * long_deg).sin();
    let dfa = f107a - 150.0;
    let cd32 = (DR * (day - p(32))).cos();
    let cd18 = (DR * 2.0 * (day - p(18))).cos();
    let cd14 = (DR * (day - p(14))).cos();
    let cd39 = (DR * 2.0 * (day - p(39))).cos();

    let mut t = [0.0_f64; 14];
    t[0] = p(22) * dfa;
    t[1] = p(2) * plg[2] + p(3) * plg[4] + p(23) * plg[6] + p(27) * plg[1] + p(28) * plg[3] + p(29) * plg[5];
    t[2] = (p(19) + p(48) * plg[2] + p(30) * plg[4]) * cd32;
    t[3] = (p(16) + p(17) * plg[2] + p(31) * plg[4]) * cd18;
    t[4] = (p(10) * plg[1] + p(11) * plg[3] + p(36) * plg[5]) * cd14;
    t[5] = p(38) * plg[1] * cd39;

    let t71 = p(12) * plg[11] * cd14;
    let t72 = p(13) * plg[11] * cd14;
    t[6] = (p(4) * plg[10] + p(5) * plg[12] + t71) * ctloc + (p(7) * plg[10] + p(8) * plg[12] + t72) * stloc;

    let t81 = (p(24) * plg[21] + p(47) * plg[23]) * cd14;
    let t82 = (p(34) * plg[21] + p(49) * plg[23]) * cd14;
    t[7] = (p(6) * plg[20] + p(42) * plg[22] + t81) * c2tloc + (p(9) * plg[20] + p(43) * plg[22] + t82) * s2tloc;

    t[13] = p(40) * plg[30] * s3tloc + p(41) * plg[30] * c3tloc;

    // MAGNETIC ACTIVITY: SW(9) == 1 branch only (the SW(9) == -1 branch, needing `apt`, is dead
    // for GMAT's own usage -- see this module's own doc comment).
    t[8] = apdf * (p(33) + p(46) * plg[2]);

    // LONGITUDINAL (SW(9), SW(10) always on; `long > -1000` always).
    t[10] = (plg[1] * (p(81) * (DR * (day - p(82))).cos() + p(86) * (DR * 2.0 * (day - p(87))).cos()) + 1.0 + p(84) * (DR * (day - p(85))).cos() + p(88) * (DR * 2.0 * (day - p(89))).cos())
        * ((p(65) * plg[11] + p(66) * plg[13] + p(67) * plg[15] + p(75) * plg[10] + p(76) * plg[12] + p(77) * plg[14]) * clong + (p(91) * plg[11] + p(92) * plg[13] + p(93) * plg[15] + p(78) * plg[10] + p(79) * plg[12] + p(80) * plg[14]) * slong);

    t.iter().sum::<f64>()
}

/// GMAT's own lower-thermosphere temperature nodes `TN1(2..5)` and `TGN1(2)` -- the block
/// `GTS6` itself computes ONCE per `Density()` call (`msise90_sub.c`'s own `if (*alt < 300.f)`/
/// `else` branches, both ported here) and reuses for EVERY species' [`densu`] call below `za`
/// this Density() evaluation needs. `TN1(1)`/`TGN1(1)` are NOT computed here -- [`densu`] itself
/// computes them fresh, from ITS OWN call's Bates temperature at `za` (see [`densu`]'s own doc
/// comment); they are not a Fortran-`SAVE` carryover this module needs to reproduce, contrary to
/// an earlier, incorrect reading of this port's own scope (see this task's own report).
///
/// `apdf` is threaded through from the SAME value the `TLB` `GLOBE6` call ([`gts6_mass48`]'s own
/// `tlb` computation, `P(44)`/`P(45)` from `PD[450..600]`, i.e. `PD(494)`/`PD(495)`) leaves
/// behind in GMAT's own shared `lpoly` struct -- `GLOB6S` itself never recomputes `apdf`, it
/// only reads whatever the immediately preceding `GLOBE6` call last set (`msise90_sub.c`'s own
/// call order: `TINF`, `G0`, `TLB` via `GLOBE6`, THEN the `TN1`/`TGN1` nodes via `GLOB6S`) --
/// this module reproduces that exact data dependency EXPLICITLY (a parameter), not implicitly
/// (shared mutable state), which is why this function takes `apdf` rather than recomputing it.
fn tn1_nodes_and_tgn1_2(alt_km: f64, geo: &Geophysical, apdf: f64) -> ([f64; 4], f64) {
    let gs = |p: &[f64]| glob6s(geo, apdf, p);
    if alt_km < 300.0 {
        let tn1_2 = ptm()[6] * ptl()[0] / (1.0 - gs(&ptl()[0..100]));
        let tn1_3 = ptm()[2] * ptl()[100] / (1.0 - gs(&ptl()[100..200]));
        let tn1_4 = ptm()[7] * ptl()[200] / (1.0 - gs(&ptl()[200..300]));
        let tn1_5 = ptm()[4] * ptl()[300] / (1.0 - gs(&ptl()[300..400]));
        let denom = ptm()[4] * ptl()[300];
        let tgn1_2 = ptm()[8] * pma()[800] * (gs(&pma()[800..900]) + 1.0) * tn1_5 * tn1_5 / (denom * denom);
        ([tn1_2, tn1_3, tn1_4, tn1_5], tgn1_2)
    } else {
        let tn1_2 = ptm()[6] * ptl()[0];
        let tn1_3 = ptm()[2] * ptl()[100];
        let tn1_4 = ptm()[7] * ptl()[200];
        let tn1_5 = ptm()[4] * ptl()[300];
        let denom = ptm()[4] * ptl()[300];
        let tgn1_2 = ptm()[8] * pma()[800] * tn1_5 * tn1_5 / (denom * denom);
        ([tn1_2, tn1_3, tn1_4, tn1_5], tgn1_2)
    }
}

/// GMAT's own `A1MJD`-style day count from `JD_JAN_5_1941`, for the UTC instant simultaneous
/// with `t_tai_ns` -- the SAME day-count convention `av_cdm::time::Tai::to_a1_mjd` uses for the
/// A.1 scale (`GMAT_MJD_AT_UNIX_EPOCH = 10_587.5`, that module's own private constant,
/// duplicated here for the UTC scale -- matching `crate::jacchia_roberts`'s own "copied
/// verbatim, kept independent" rule for constants shared with another module).
fn utc_mjd_1941(t_tai_ns: i64) -> f64 {
    const GMAT_MJD_AT_UNIX_EPOCH: f64 = 10_587.5;
    const NS_PER_DAY: f64 = 86_400_000_000_000.0;
    let utc_ns = av_cdm::time::Tai::from_nanos(t_tai_ns).to_utc_nanos();
    GMAT_MJD_AT_UNIX_EPOCH + (utc_ns as f64) / NS_PER_DAY
}

/// `GTD6`'s own Legendre polynomials, `msise90_sub.c`'s own flat `plg[36]` indices (NOT
/// pointer-adjusted in the source -- these indices are copied unchanged, no `-1` shift, unlike
/// the `p(k)` helper [`globe6`] uses for its own POINTER-adjusted `p` parameter).
fn legendre(lat_deg: f64) -> [f64; 36] {
    let mut plg = [0.0_f64; 36];
    let c = (lat_deg * DGTR).sin();
    let s = (lat_deg * DGTR).cos();
    let c2 = c * c;
    let c4 = c2 * c2;
    let s2 = s * s;
    plg[1] = c;
    plg[2] = (c2 * 3.0 - 1.0) * 0.5;
    plg[3] = (c * 5.0 * c2 - c * 3.0) * 0.5;
    plg[4] = (c4 * 35.0 - c2 * 30.0 + 3.0) / 8.0;
    plg[5] = (c2 * 63.0 * c2 * c - c2 * 70.0 * c + c * 15.0) / 8.0;
    plg[6] = (c * 11.0 * plg[5] - plg[4] * 5.0) / 6.0;
    plg[10] = s;
    plg[11] = c * 3.0 * s;
    plg[12] = (c2 * 5.0 - 1.0) * 1.5 * s;
    plg[13] = (c2 * 7.0 * c - c * 3.0) * 2.5 * s;
    plg[14] = (c4 * 21.0 - c2 * 14.0 + 1.0) * 1.875 * s;
    plg[15] = (c * 11.0 * plg[14] - plg[13] * 6.0) / 5.0;
    plg[20] = s2 * 3.0;
    plg[21] = s2 * 15.0 * c;
    plg[22] = (c2 * 7.0 - 1.0) * 7.5 * s2;
    plg[23] = c * 3.0 * plg[22] - plg[21] * 2.0;
    plg[24] = (c * 11.0 * plg[23] - plg[22] * 7.0) / 4.0;
    plg[25] = (c * 13.0 * plg[24] - plg[23] * 8.0) / 5.0;
    plg[30] = s2 * 15.0 * s;
    plg[31] = s2 * 105.0 * s * c;
    plg[32] = (c * 9.0 * plg[31] - plg[30] * 7.0) / 2.0;
    plg[33] = (c * 11.0 * plg[32] - plg[31] * 8.0) / 3.0;
    plg
}

/// GMAT's own `GLOBE6`: the fractional variation ("G(L)") function underlying every one of
/// `GTS6`'s per-species/temperature terms -- ported from `msise90_sub.c`'s `globe6_`, with the
/// `SW(9) == -1` (hourly AP) branch dropped (dead code for GMAT's own usage -- see this
/// module's own doc comment) and every `if (sw[k] == 0)` branch always taken as "on" (`sw[k] =
/// 1.0` unconditionally -- also see this module's own doc comment). `p(k)` mirrors the SOURCE's
/// own POINTER-adjusted 1-based indexing exactly (`p[k]` in `msise90_sub.c`, post its own `--p;`
/// adjustment, is this function's `p(k)`) -- every numeral below matches a `msise90_sub.c` line
/// directly, not re-derived.
fn globe6(geo: &Geophysical, parr: &[f64]) -> f64 {
    let Geophysical { day, sec, lat_deg, long_deg, tloc, f107a, f107, ap_daily } = *geo;
    let p = |i: usize| parr[i - 1];
    let plg = legendre(lat_deg);
    let (stloc, ctloc) = (HR * tloc).sin_cos();
    let (s2tloc, c2tloc) = (HR * 2.0 * tloc).sin_cos();
    let (s3tloc, c3tloc) = (HR * 3.0 * tloc).sin_cos();
    let clong = (DGTR * long_deg).cos();
    let slong = (DGTR * long_deg).sin();
    let cd14 = (DR * (day - p(14))).cos();
    let cd18 = (DR * 2.0 * (day - p(18))).cos();
    let cd32 = (DR * (day - p(32))).cos();
    let cd39 = (DR * 2.0 * (day - p(39))).cos();

    let df = f107 - f107a;
    let dfa = f107a - 150.0;
    let mut t = [0.0_f64; 14];
    t[0] = p(20) * df + p(21) * df * df + p(22) * dfa + p(30) * (dfa * dfa);
    let f1 = (p(48) * dfa + p(20) * df + p(21) * df * df) + 1.0;
    let f2 = (p(50) * dfa + p(20) * df + p(21) * df * df) + 1.0;
    t[1] = p(2) * plg[2] + p(3) * plg[4] + p(23) * plg[6] + p(15) * plg[2] * dfa + p(27) * plg[1];
    t[2] = p(19) * cd32;
    t[3] = (p(16) + p(17) * plg[2]) * cd18;
    t[4] = f1 * (p(10) * plg[1] + p(11) * plg[3]) * cd14;
    t[5] = p(38) * plg[1] * cd39;

    let t71 = p(12) * plg[11] * cd14;
    let t72 = p(13) * plg[11] * cd14;
    t[6] = f2 * ((p(4) * plg[10] + p(5) * plg[12] + p(28) * plg[14] + t71) * ctloc + (p(7) * plg[10] + p(8) * plg[12] + p(29) * plg[14] + t72) * stloc);

    let t81 = (p(24) * plg[21] + p(36) * plg[23]) * cd14;
    let t82 = (p(34) * plg[21] + p(37) * plg[23]) * cd14;
    t[7] = f2 * ((p(6) * plg[20] + p(42) * plg[22] + t81) * c2tloc + (p(9) * plg[20] + p(43) * plg[22] + t82) * s2tloc);

    t[13] = f2 * ((p(40) * plg[30] + (p(94) * plg[31] + p(47) * plg[33]) * cd14) * s3tloc + (p(41) * plg[30] + (p(95) * plg[31] + p(49) * plg[33]) * cd14) * c3tloc);

    // MAGNETIC ACTIVITY BASED ON DAILY AP (SW(9) = +1 always -- see this module's own doc
    // comment, "AP: a single scalar").
    let apdf = apdf_daily(ap_daily, p(44), p(45));
    t[8] = apdf * (p(33) + p(46) * plg[2] + p(35) * plg[4] + (p(101) * plg[1] + p(102) * plg[3] + p(103) * plg[5]) * cd14 + (p(122) * plg[10] + p(123) * plg[12] + p(124) * plg[14]) * (HR * (tloc - p(125))).cos());

    // LONGITUDINAL, UT AND MIXED UT/LONG, UT/LONG MAGNETIC ACTIVITY -- SW(10) is always on and
    // `long > -1000` always (real geodetic longitude, never GMAT's own sentinel), so these are
    // never skipped (see this module's own doc comment).
    t[9] = 0.0; // T(10) is never set by GLOBE6 itself (see this module's own doc comment)
    t[10] = (p(81) * dfa + 1.0)
        * ((p(65) * plg[11] + p(66) * plg[13] + p(67) * plg[15] + p(104) * plg[10] + p(105) * plg[12] + p(106) * plg[14] + (p(110) * plg[10] + p(111) * plg[12] + p(112) * plg[14]) * cd14) * clong
            + (p(91) * plg[11] + p(92) * plg[13] + p(93) * plg[15] + p(107) * plg[10] + p(108) * plg[12] + p(109) * plg[14] + (p(113) * plg[10] + p(114) * plg[12] + p(115) * plg[14]) * cd14) * slong);

    t[11] = (p(96) * plg[1] + 1.0) * (p(82) * dfa + 1.0) * (p(120) * plg[1] * cd14 + 1.0) * ((p(69) * plg[1] + p(70) * plg[3] + p(71) * plg[5]) * (SR * (sec - p(72))).cos());
    t[11] += (p(77) * plg[21] + p(78) * plg[23] + p(79) * plg[25]) * (SR * (sec - p(80)) + DGTR * 2.0 * long_deg).cos() * (p(138) * dfa + 1.0);

    t[12] = apdf * (p(121) * plg[1] + 1.0) * ((p(61) * plg[11] + p(62) * plg[13] + p(63) * plg[15]) * (DGTR * (long_deg - p(64))).cos())
        + apdf * (p(116) * plg[10] + p(117) * plg[12] + p(118) * plg[14]) * cd14 * (DGTR * (long_deg - p(119))).cos()
        + apdf * (p(84) * plg[1] + p(85) * plg[3] + p(86) * plg[5]) * (SR * (sec - p(76))).cos();

    p(31) + t.iter().sum::<f64>()
}

/// Every quantity [`densu`] needs that is SHARED across every species' call within one
/// `Density()` evaluation (`tinf`/`tlb`/`zlb`/`s2` from [`gts6_mass48`]'s own TINF/G0/TLB
/// `GLOBE6` calls; `za`/`zn1`/`tn1_2to5`/`tgn1_2` from [`tn1_nodes_and_tgn1_2`]; `re`/`gsurf`
/// from [`glatf`]) -- bundled to keep [`densu`]'s own argument count under clippy's lint
/// without an `#[allow]` (this crate's own rule). `zn1` is `[za, 110, 100, 90, 72.5]`
/// (`ZN1(1..5)`); `tn1_2to5`/`tgn1_2` are `TN1(2..5)`/`TGN1(2)` -- `TN1(1)`/`TGN1(1)` are NOT
/// here, [`densu`] computes them itself (see that function's own doc comment).
struct DensuContext {
    tinf: f64,
    tlb: f64,
    zlb: f64,
    s2: f64,
    re: f64,
    gsurf: f64,
    za: f64,
    zn1: [f64; 5],
    tn1_2to5: [f64; 4],
    tgn1_2: f64,
}

/// The cubic-spline state [`densu`]'s own `alt < za` branch builds and both its `TZ` computation
/// and its below-`za` density correction need -- a named struct (not a tuple) purely to keep
/// [`densu`]'s own local state under clippy's type-complexity lint without an `#[allow]` (this
/// crate's own rule).
struct BelowZaSpline {
    t1: f64,
    xs: [f64; 5],
    ys: [f64; 5],
    y2: Vec<f64>,
    zg: f64,
    zgdif: f64,
}

/// GMAT's own `DENSU`, ported in FULL -- both the `alt >= za` closed-form Bates-profile branch
/// AND the `alt < za` cubic-spline extension (`SPLINE`/`SPLINT`/`SPLINI`). **An earlier draft of
/// this port implemented only the `alt >= za` branch**, reasoning that every altitude this
/// module's own floor admits (`>= za`) would only ever reach that branch -- WRONG: [`gts6_mass48`]
/// calls this function with the SPECIES' OWN TURBOPAUSE REFERENCE altitude (`ZH04`/`ZH28`/...,
/// GMAT's own `PDM`-derived values, ~100-110 km) for the "mixed density at Zlb" sub-calls, which
/// is virtually always BELOW `za` regardless of the spacecraft's own altitude -- confirmed the
/// hard way: `NaN` at every altitude this module's own test ladder tried (the missing branch's
/// own `ln()`/`.powf()` calls on uninitialised zeros), root-caused by tracing exactly which
/// `densu` call produced it (see this task's own report). `TN1(1)`/`TGN1(1)` (the temperature/
/// gradient AT `za` itself) are computed HERE, fresh, from THIS call's own Bates temperature --
/// re-reading `densu_`'s own source shows this precisely: they are a LOCAL by-product of the
/// `alt < za` branch's own first few lines, not an externally supplied or Fortran-`SAVE`-carried
/// value, contrary to this port's own earlier (incorrect) reading of its scope.
fn densu(alt_km: f64, dlb: f64, xm: f64, alpha: f64, ctx: &DensuContext) -> f64 {
    let za = ctx.za;
    let z = alt_km.max(za);
    let zg2 = (z - ctx.zlb) * (ctx.re + ctx.zlb) / (ctx.re + z);
    let tt = ctx.tinf - (ctx.tinf - ctx.tlb) * (-ctx.s2 * zg2).exp();
    let ta = tt;
    let mut tz = tt;

    // Below-za spline state -- populated (and needed) only when alt_km < za.
    let mut below_za: Option<BelowZaSpline> = None;
    if alt_km < za {
        let tgn1_1 = (ctx.tinf - ta) * ctx.s2 * ((ctx.re + ctx.zlb) / (ctx.re + za)).powi(2);
        let tn1_1 = ta;
        let z1 = za;
        let z2 = ctx.zn1[4]; // ZN1(MN1) = ZN1(5) = 72.5
        let t1 = tn1_1;
        let t2 = ctx.tn1_2to5[3]; // TN1(5)
        let z_clamped = alt_km.max(z2);
        let zg = (z_clamped - z1) * (ctx.re + z1) / (ctx.re + z_clamped);
        let zgdif = (z2 - z1) * (ctx.re + z1) / (ctx.re + z2);
        let tn1_full = [tn1_1, ctx.tn1_2to5[0], ctx.tn1_2to5[1], ctx.tn1_2to5[2], ctx.tn1_2to5[3]];
        let mut xs = [0.0_f64; 5];
        let mut ys = [0.0_f64; 5];
        for k in 0..5 {
            xs[k] = (ctx.zn1[k] - z1) * (ctx.re + z1) / (ctx.re + ctx.zn1[k]) / zgdif;
            ys[k] = 1.0 / tn1_full[k];
        }
        let yd1 = -tgn1_1 / (t1 * t1) * zgdif;
        let yd2 = -ctx.tgn1_2 / (t2 * t2) * zgdif * ((ctx.re + z2) / (ctx.re + z1)).powi(2);
        let y2 = spline(&xs, &ys, yd1, yd2);
        let x = zg / zgdif;
        let y = splint(&xs, &ys, &y2, x);
        tz = 1.0 / y;
        below_za = Some(BelowZaSpline { t1, xs, ys, y2, zg, zgdif });
    }

    if xm == 0.0 {
        return tz;
    }

    let glb = ctx.gsurf / (ctx.zlb / ctx.re + 1.0).powi(2);
    let gamma = xm * glb / (ctx.s2 * RGAS * ctx.tinf);
    let mut expl = (-ctx.s2 * gamma * zg2).exp();
    if expl > 50.0 || tt <= 0.0 {
        expl = 50.0;
    }
    let mut densa = dlb * (ctx.tlb / tt).powf(alpha + 1.0 + gamma) * expl;
    if alt_km >= za {
        return densa;
    }

    let BelowZaSpline { t1, xs, ys, y2, zg, zgdif } = below_za.expect("populated above whenever alt_km < za");
    let glb2 = ctx.gsurf / (za / ctx.re + 1.0).powi(2);
    let gamm = xm * glb2 * zgdif / RGAS;
    let x = zg / zgdif;
    let yi = splini(&xs, &ys, &y2, x);
    let mut expl2 = gamm * yi;
    if expl2 > 50.0 || tz <= 0.0 {
        expl2 = 50.0;
    }
    densa *= (t1 / tz).powf(alpha + 1.0) * (-expl2).exp();
    densa
}

/// GMAT's own `DNET`: turbopause correction (root-mean density blend of the diffusive and fully
/// mixed profiles), ported unchanged. GMAT's own error path (`dd`/`dm` non-positive, a
/// diagnostic `WRITE` in the FORTRAN) is replaced with the SAME fallback GMAT's own code falls
/// through to afterward (`dd=1` when both are zero, then the `dm==0`/`dd==0` branches) --
/// matching the source's own eventual behaviour, never a panic.
fn dnet(dd_in: f64, dm: f64, zhm: f64, xmm: f64, xm: f64) -> f64 {
    let a = zhm / (xmm - xm);
    let mut dd = dd_in;
    if !(dm > 0.0 && dd > 0.0) {
        if dd == 0.0 && dm == 0.0 {
            dd = 1.0;
        }
        if dm == 0.0 {
            return dd;
        }
        if dd == 0.0 {
            return dm;
        }
    }
    let ylog = a * (dm / dd).ln();
    if ylog < -10.0 {
        return dd;
    }
    if ylog > 10.0 {
        return dm;
    }
    dd * (ylog.exp() + 1.0).powf(1.0 / a)
}

/// GMAT's own `CCOR`: chemistry/dissociation correction, ported unchanged.
fn ccor(alt: f64, r: f64, h1: f64, zh: f64) -> f64 {
    let e = (alt - zh) / h1;
    let ret = if e > 70.0 {
        0.0
    } else if e < -70.0 {
        r
    } else {
        r / (e.exp() + 1.0)
    };
    ret.exp()
}

/// This vehicle's central-body geodetic parameters and this call's resolved weather -- see
/// [`density_kg_m3`].
struct Gts6Result {
    /// D(1)=He, D(2)=O, D(3)=N2, D(4)=O2, D(5)=Ar, D(6)=total mass (g/cm^3), D(7)=H, D(8)=N --
    /// GMAT's own `D(1..8)` output, 0-based here.
    d: [f64; 8],
}

/// GMAT's own `GTS6` (`gts6_0_` in `msise90_sub.c`), restricted to `MASS == 48` (total density)
/// and `alt >= za` -- see this module's own doc comment for exactly which branches this drops
/// and why each is verified dead for GMAT's own usage, not guessed. `alt_km` is body-fixed
/// geodetic height; `lat_deg`/`long_deg` geodetic latitude/longitude (degrees); `tloc` local
/// apparent solar time (hours); `day` day-of-year (from [`year_doy_sod`]); `sec` UT seconds of
/// day; `ap_daily` the resolved geomagnetic amplitude ([`kp_to_ap`]).
fn gts6_mass48(alt_km: f64, geo: &Geophysical, gsurf: f64, re: f64) -> Gts6Result {
    let g = |p: &[f64]| globe6(geo, p);

    // TINF (SW(16) always on -- `csw_1.sw[15]`, i.e. SW(16) 1-based).
    let tinf = ptm()[0] * pt()[0] * (g(pt()) + 1.0);
    // GRADIENT (SW(19) always on).
    let g0 = ptm()[3] * ps()[0] * (g(ps()) + 1.0);
    // TLB (SW(17) always on).
    let tlb = ptm()[1] * (g(&pd()[450..600]) + 1.0) * pd()[450];
    let s2 = g0 / (tinf - tlb);
    let zlb = ptm()[5];
    let xmm = pdm()[24];

    // GMAT's own shared `lpoly.apdf`, as the TLB call above left it (`P(44)`/`P(45)` from
    // `PD[450..600]`, i.e. `PD(494)`/`PD(495)`, 1-based) -- see [`tn1_nodes_and_tgn1_2`]'s own
    // doc comment for exactly why this is threaded through explicitly.
    let apdf_shared = apdf_daily(geo.ap_daily, pd()[450 + 43], pd()[450 + 44]);
    let (tn1_2to5, tgn1_2) = tn1_nodes_and_tgn1_2(alt_km, geo, apdf_shared);
    let ctx = DensuContext { tinf, tlb, zlb, s2, re, gsurf, za: za_km(), zn1: [za_km(), 110.0, 100.0, 90.0, 72.5], tn1_2to5, tgn1_2 };

    let mut d = [0.0_f64; 8];

    // Turbopause height variation (SW(5) always on).
    let zhf = pdl()[49] * (pdl()[24] * (DGTR * geo.lat_deg).sin() * (DR * (geo.day - pt()[13])).cos() + 1.0);

    // **** N2 DENSITY **** (always computed first -- MASS==48 bypasses gtd6_'s own early
    // "alt > altl(6)" skip, per `msise90_sub.c`'s own `if (*mass != 28 && *mass != 48)` guard).
    let g28 = g(&pd()[300..450]);
    let db28 = pdm()[20] * g28.exp() * pd()[300];
    d[2] = densu(alt_km, db28, 28.0, 0.0, &ctx);
    let zh28 = pdm()[22] * zhf;
    let zhm28 = pdm()[23] * pdl()[30];
    let xmd = 28.0 - xmm;
    let b28 = densu(zh28, db28, xmd, -1.0, &ctx);
    if alt_km <= 160.0 {
        let dm28 = densu(alt_km, b28, xmm, 0.0, &ctx);
        d[2] = dnet(d[2], dm28, zhm28, xmm, 28.0);
    }

    // **** HE DENSITY ****
    let g4 = g(pd());
    let db04 = pdm()[0] * g4.exp() * pd()[0];
    d[0] = densu(alt_km, db04, 4.0, -0.4, &ctx);
    if alt_km <= 200.0 {
        let zh04 = pdm()[2];
        let b04 = densu(zh04, db04, 4.0 - xmm, -1.4, &ctx);
        let dm04 = densu(alt_km, b04, xmm, 0.0, &ctx);
        d[0] = dnet(d[0], dm04, zhm28, xmm, 4.0);
        let rl = (b28 * pdm()[1] / b04).ln();
        let zc04 = pdm()[4] * pdl()[25];
        let hc04 = pdm()[5] * pdl()[26];
        d[0] *= ccor(alt_km, rl, hc04, zc04);
    }

    // **** O DENSITY ****
    let g16 = g(&pd()[150..300]);
    let db16 = pdm()[10] * g16.exp() * pd()[150];
    d[1] = densu(alt_km, db16, 16.0, 0.0, &ctx);
    if alt_km <= 400.0 {
        let zh16 = pdm()[12];
        let b16 = densu(zh16, db16, 16.0 - xmm, -1.0, &ctx);
        let dm16 = densu(alt_km, b16, xmm, 0.0, &ctx);
        d[1] = dnet(d[1], dm16, zhm28, xmm, 16.0);
        let rl = (b28 * pdm()[11] * pdl()[41].abs() / b16).ln();
        let hc16 = pdm()[15] * pdl()[28];
        let zc16 = pdm()[14] * pdl()[27];
        d[1] *= ccor(alt_km, rl, hc16, zc16);
        let hcc16 = pdm()[17] * pdl()[38];
        let zcc16 = pdm()[16] * pdl()[37];
        let rc16 = pdm()[13] * pdl()[39];
        d[1] *= ccor(alt_km, rc16, hcc16, zcc16);
    }

    // **** O2 DENSITY ****
    let g32 = g(&pd()[600..750]);
    let db32 = pdm()[30] * g32.exp() * pd()[600];
    d[3] = densu(alt_km, db32, 32.0, 0.0, &ctx);
    if alt_km <= 200.0 {
        let zh32 = pdm()[32];
        let b32 = densu(zh32, db32, 32.0 - xmm, -1.0, &ctx);
        let dm32 = densu(alt_km, b32, xmm, 0.0, &ctx);
        d[3] = dnet(d[3], dm32, zhm28, xmm, 32.0);
        let rl = (b28 * pdm()[31] / b32).ln();
        let hc32 = pdm()[35] * pdl()[32];
        let zc32 = pdm()[34] * pdl()[31];
        d[3] *= ccor(alt_km, rl, hc32, zc32);
    }

    // **** AR DENSITY ****
    let g40 = g(&pd()[750..900]);
    let db40 = pdm()[40] * g40.exp() * pd()[750];
    d[4] = densu(alt_km, db40, 40.0, 0.0, &ctx);
    if alt_km <= 240.0 {
        let zh40 = pdm()[42];
        let b40 = densu(zh40, db40, 40.0 - xmm, -1.0, &ctx);
        let dm40 = densu(alt_km, b40, xmm, 0.0, &ctx);
        d[4] = dnet(d[4], dm40, zhm28, xmm, 40.0);
        let rl = (b28 * pdm()[41] / b40).ln();
        let hc40 = pdm()[45] * pdl()[34];
        let zc40 = pdm()[44] * pdl()[33];
        d[4] *= ccor(alt_km, rl, hc40, zc40);
    }

    // **** HYDROGEN DENSITY ****
    let g1 = g(&pd()[900..1050]);
    let db01 = pdm()[50] * g1.exp() * pd()[900];
    d[6] = densu(alt_km, db01, 1.0, -0.4, &ctx);
    if alt_km <= 320.0 {
        let zh01 = pdm()[52];
        let b01 = densu(zh01, db01, 1.0 - xmm, -1.4, &ctx);
        let dm01 = densu(alt_km, b01, xmm, 0.0, &ctx);
        d[6] = dnet(d[6], dm01, zhm28, xmm, 1.0);
        let rl = (b28 * pdm()[51] * pdl()[42].abs() / b01).ln();
        let hc01 = pdm()[55] * pdl()[36];
        let zc01 = pdm()[54] * pdl()[35];
        d[6] *= ccor(alt_km, rl, hc01, zc01);
        let hcc01 = pdm()[57] * pdl()[44];
        let zcc01 = pdm()[56] * pdl()[43];
        let rc01 = pdm()[53] * pdl()[45];
        d[6] *= ccor(alt_km, rc01, hcc01, zcc01);
    }

    // **** ATOMIC NITROGEN DENSITY ****
    let g14 = g(&pd()[1050..1200]);
    let db14 = pdm()[60] * g14.exp() * pd()[1050];
    d[7] = densu(alt_km, db14, 14.0, 0.0, &ctx);
    if alt_km <= 450.0 {
        let zh14 = pdm()[62];
        let b14 = densu(zh14, db14, 14.0 - xmm, -1.0, &ctx);
        let dm14 = densu(alt_km, b14, xmm, 0.0, &ctx);
        d[7] = dnet(d[7], dm14, zhm28, xmm, 14.0);
        let rl = (b28 * pdm()[61] * pdl()[2].abs() / b14).ln();
        let hc14 = pdm()[65] * pdl()[1];
        let zc14 = pdm()[64] * pdl()[0];
        d[7] *= ccor(alt_km, rl, hc14, zc14);
        let hcc14 = pdm()[67] * pdl()[4];
        let zcc14 = pdm()[66] * pdl()[3];
        let rc14 = pdm()[63] * pdl()[5];
        d[7] *= ccor(alt_km, rc14, hcc14, zcc14);
    }

    // TOTAL MASS DENSITY (g/cm^3) -- GMAT's own `D(6) = (D(1)*4+D(2)*16+D(3)*28+D(4)*32+D(5)*40
    // +D(7)+D(8)*14)*1.66e-24`.
    d[5] = (d[0] * 4.0 + d[1] * 16.0 + d[2] * 28.0 + d[3] * 32.0 + d[4] * 40.0 + d[6] + d[7] * 14.0) * 1.66e-24;

    Gts6Result { d }
}

/// The public, SI entry point: MSISE90 atmospheric density in kg/m^3 at spacecraft position
/// `r_sc_m` (central-body-relative, whatever inertial frame the caller's state uses), given
/// this call's own body-fixed rotation `rotation` (for the geodetic height/latitude/longitude),
/// the epoch `t_tai_ns`, weather inputs (`Kp` converted to `Ap` internally -- see this module's
/// own doc comment) and central-body shape.
///
/// This module's own derived floor: [`MsiseError::BelowMinimumAltitude`] if the computed height
/// is below `za` ([`za_km`], ~122.8 km) -- see this module's own doc comment, "Altitude floor".
pub fn density_kg_m3<R: BodyFixedRotation>(r_sc_m: [f64; 3], rotation: &R, t_tai_ns: i64, weather: &WeatherInputs, cb: &CentralBodyGeodetics) -> Result<f64, MsiseError> {
    if !r_sc_m.iter().all(|v| v.is_finite()) {
        return Err(MsiseError::NonFinitePosition(r_sc_m));
    }
    if !(weather.f107.is_finite() && weather.f107a.is_finite() && weather.kp.is_finite()) {
        return Err(MsiseError::InvalidWeather { f107: weather.f107, f107a: weather.f107a, kp: weather.kp });
    }
    if !(cb.equatorial_radius_km.is_finite() && cb.equatorial_radius_km > 0.0 && cb.flattening.is_finite() && (0.0..1.0).contains(&cb.flattening)) {
        return Err(MsiseError::InvalidCentralBody { equatorial_radius_km: cb.equatorial_radius_km, flattening: cb.flattening });
    }

    let r_sc_km = [r_sc_m[0] / 1000.0, r_sc_m[1] / 1000.0, r_sc_m[2] / 1000.0];
    let rot = rotation.inertial_to_fixed(t_tai_ns).map_err(|e| MsiseError::Rotation(e.to_string()))?;
    let r_bf_km = rot.apply(r_sc_km);
    let (height_km, geo_lat_rad) = geodetic_height_lat_km(r_bf_km, cb);
    if height_km < za_km() {
        return Err(MsiseError::BelowMinimumAltitude { height: height_km, za: za_km() });
    }
    // `AtmosphereModel::CalculateGeodetics`'s own longitude formula (`atan2(state[1],
    // state[0])`, degrees, put in [-180,180] -- atan2's own range already lies in (-180,180]
    // degrees, so no further wrapping is needed) -- see this module's own doc comment.
    let long_deg = r_bf_km[1].atan2(r_bf_km[0]).to_degrees();
    let lat_deg = geo_lat_rad.to_degrees();

    let (gsurf, re) = glatf(lat_deg);
    let epoch = utc_mjd_1941(t_tai_ns);
    let (year, doy, sod) = year_doy_sod(epoch);
    let day = (year * 1000 + doy) as f64 % 1000.0; // GMAT's own `r_mod(&yrd, 1000.0)`, == doy
    let stl = sod / 3600.0 + long_deg / 15.0;
    let ap_daily = kp_to_ap(weather.kp);

    let geo = Geophysical { day, sec: sod, lat_deg, long_deg, tloc: stl, f107a: weather.f107a, f107: weather.f107, ap_daily };
    let result = gts6_mass48(height_km, &geo, gsurf, re);
    // GMAT's own wrapper: `density[i] = xden[5] * 1000.0;` -- g/cm^3 -> kg/m^3.
    let density_kg_m3 = result.d[5] * 1000.0;
    if !density_kg_m3.is_finite() {
        return Err(MsiseError::NonFiniteResult(density_kg_m3));
    }
    Ok(density_kg_m3)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::Rotation;
    use crate::weather::ConstantWeather;

    struct IdentityRotation;
    impl BodyFixedRotation for IdentityRotation {
        type Error = std::convert::Infallible;
        fn inertial_to_fixed(&self, _t_tai_ns: i64) -> Result<Rotation, Self::Error> {
            Ok(Rotation { r: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]], r_dot: [[0.0; 3]; 3] })
        }
    }

    #[test]
    fn kp_to_ap_matches_gmats_own_default_row_for_kp_3() {
        // GMAT's own DragForce default Kp=3.0 -> table index 9 -> the table's own "default: ap
        // = 15.0" row (this crate's own report cross-checks this against a live GMAT read-back).
        assert_eq!(kp_to_ap(3.0), 15.0);
    }

    #[test]
    fn kp_to_ap_matches_the_table_at_a_few_other_rows() {
        assert_eq!(kp_to_ap(0.0), 0.0);
        assert_eq!(kp_to_ap(9.0), 400.0);
    }

    #[test]
    fn za_km_is_the_expected_measured_value() {
        // PDL(41), extracted mechanically from msise90_sub.c -- see msise90_data.rs.
        let za = za_km();
        assert!((za - 122.807).abs() < 1e-3, "za_km()={za}");
    }

    #[test]
    fn density_decreases_with_altitude_above_za() {
        let cb = CentralBodyGeodetics::earth_defaults();
        let weather = WeatherInputs::from(ConstantWeather::gmat_defaults());
        let mut last = f64::INFINITY;
        for alt_km in [150.0, 200.0, 400.0, 700.0, 1200.0] {
            let r_km = alt_km + cb.equatorial_radius_km;
            let r_sc_m = [r_km * 1000.0, 0.0, 0.0];
            let rho = density_kg_m3(r_sc_m, &IdentityRotation, 1_800_000_000_000_000_000, &weather, &cb).unwrap();
            assert!(rho > 0.0 && rho.is_finite(), "density at {alt_km} km must be positive and finite, got {rho}");
            assert!(rho < last, "density must decrease with altitude: {alt_km} km gave {rho}, previous was {last}");
            last = rho;
        }
    }

    #[test]
    fn below_za_is_a_typed_error_not_a_panic() {
        let cb = CentralBodyGeodetics::earth_defaults();
        let weather = WeatherInputs::from(ConstantWeather::gmat_defaults());
        let r_sc_m = [(cb.equatorial_radius_km + 90.0) * 1000.0, 0.0, 0.0];
        let err = density_kg_m3(r_sc_m, &IdentityRotation, 0, &weather, &cb).unwrap_err();
        assert!(matches!(err, MsiseError::BelowMinimumAltitude { .. }));
    }

    #[test]
    fn nan_position_is_a_typed_error_not_a_panic() {
        let cb = CentralBodyGeodetics::earth_defaults();
        let weather = WeatherInputs::from(ConstantWeather::gmat_defaults());
        let err = density_kg_m3([f64::NAN, 0.0, 0.0], &IdentityRotation, 0, &weather, &cb).unwrap_err();
        assert!(matches!(err, MsiseError::NonFinitePosition(_)));
    }

    #[test]
    fn nonfinite_weather_is_a_typed_error_not_a_panic() {
        let cb = CentralBodyGeodetics::earth_defaults();
        let weather = WeatherInputs { f107: f64::NAN, f107a: 150.0, kp: 3.0 };
        let r_sc_m = [(cb.equatorial_radius_km + 400.0) * 1000.0, 0.0, 0.0];
        let err = density_kg_m3(r_sc_m, &IdentityRotation, 0, &weather, &cb).unwrap_err();
        assert!(matches!(err, MsiseError::InvalidWeather { .. }));
    }

    #[test]
    fn higher_f107_increases_density_at_a_fixed_altitude() {
        let cb = CentralBodyGeodetics::earth_defaults();
        let r_sc_m = [(cb.equatorial_radius_km + 500.0) * 1000.0, 0.0, 0.0];
        let low = WeatherInputs { f107: 90.0, f107a: 90.0, kp: 3.0 };
        let high = WeatherInputs { f107: 250.0, f107a: 250.0, kp: 3.0 };
        let rho_low = density_kg_m3(r_sc_m, &IdentityRotation, 0, &low, &cb).unwrap();
        let rho_high = density_kg_m3(r_sc_m, &IdentityRotation, 0, &high, &cb).unwrap();
        println!("n3c-msise90-f107-sensitivity: rho(F10.7=90)={rho_low:e} rho(F10.7=250)={rho_high:e}");
        assert!(rho_high > rho_low, "higher F10.7 must increase density at 500 km: low={rho_low:e} high={rho_high:e}");
    }

    #[test]
    fn year_doy_sod_matches_a_hand_worked_example() {
        // 2026-01-01T00:00:00 UTC via Tai/A1MJD: cross-checked against the golden's own
        // epoch_a1mjd for leo_400km_jacchia_roberts-style arcs ("01 Jan 2026 00:00:00.000" UTC
        // resolves to A1MJD ~31041.50 in this crate's own existing goldens -- UTC differs from
        // A1 by ~34.4 ms, negligible at whole-day resolution). year_offset =
        // trunc((31041.5+5.5)/365.25) = trunc(85.006...) = 85 -> year=2026; doy = trunc(31041.5)
        // - trunc(85*365.25) + 5 = 31041 - 31046 + 5 = 0; sod = 86400*(31041.5-31041+0.5) =
        // 86400*1.0 = 86400.0 EXACTLY -- GMAT's own guard is `sod > SECS_PER_DAY` (strictly
        // greater), and 86400.0 is not strictly greater than 86400.0, so it does NOT roll over:
        // GMAT's own formula genuinely reports "day 0" with sod=86400.0 at this exact midnight
        // boundary (verified by hand arithmetic above, not a bug this port introduced -- a
        // faithful port reproduces GMAT's own formula's own boundary behaviour, not a "nicer"
        // one). day=0/sod=86400 and day=1/sod=0 are the physically identical instant either way
        // (this module's own `day` input only feeds smooth trig/polynomial phase terms, so the
        // distinction is immaterial to the density this module ultimately returns).
        let (year, doy, sod) = year_doy_sod(31041.5);
        assert_eq!(year, 2026);
        assert_eq!(doy, 0);
        assert!((sod - 86400.0).abs() < 1e-6, "sod={sod}");
    }
}
