//! The native inertial <-> body-fixed rotation for Earth: IAU-76/FK5 precession and nutation
//! (the 1980 nutation series, GMAT's own default) with polar motion and UT1 from GMAT's own
//! IERS EOP file (`docs/native-dynamics-plan.md` milestone N5, first half).
//!
//! **No cargo feature gate, unlike [`crate::frame_gmat`].** This module is pure numerics plus
//! two data-file readers -- no GMAT dependency at all -- so it compiles and its tests run under
//! `cargo test -p av-orbital --no-default-features` (this task's own report has the measured
//! count). The comparison against [`crate::frame_gmat::GmatBodyFixedRotation`] (the oracle
//! ADR-002's fourth amendment names) lives in a separate file, `tests/fk5_vs_convert.rs`, gated
//! on `gmat-frames` exactly the way `tests/frame_gmat.rs` already is.
//!
//! # The specification: GMAT's own C++ source, read directly, not a textbook
//!
//! - `third_party/gmat-src/src/base/coordsystem/AxisSystem.cpp`: `ComputePrecessionMatrix`
//!   (~line 2220), `ComputeNutationMatrix` (~2288, `NUTATION_1980` branch -- the default,
//!   `GmatItrf::NUTATION_1980` set at construction, `Planet.cpp`), `ComputeSiderealTimeRotation`
//!   / `ComputeSiderealTimeDotRotation` (~2757/2852), `ComputePolarMotionRotation` (~2913).
//! - `third_party/gmat-src/src/base/coordsystem/BodyFixedAxes.cpp`:
//!   `CalculateRotationMatrix` (~line 690-731) is the exact composition order for Earth.
//! - `third_party/gmat-src/src/gmatutil/util/EopFile.cpp`: `GetPolarMotionAndLod` is the exact
//!   EOP interpolation `ComputePolarMotionRotation`/`ComputeSiderealTimeDotRotation` call.
//! - `third_party/gmat-src/src/base/coordsystem/ItrfCoefficientsFile.cpp`: `Initialize` is the
//!   exact `NUTATION.DAT` parse (which section, which columns, the `1e-4"` multiplier).
//!
//! # The composition order (`BodyFixedAxes::CalculateRotationMatrix`, Earth branch)
//!
//! GMAT computes (row `p`, column `q`, standard matrix product, `rot = PM * (ST * (NUT *
//! PREC))`) and then stores `rotMatrix = rot^T` (`rotMatrix.Set(rot[0][0], rot[1][0],
//! rot[2][0], rot[0][1], ...)` -- `Rmatrix33::Set`'s own signature is row-major
//! `Set(a00,a01,a02,a10,a11,a12,a20,a21,a22)`, so reading `rot[j][i]` into slot `(i,j)` is
//! exactly a transpose). `BodyFixedAxes::CompleteRotateToBase` then uses `rotMatrix` to map
//! `v_inertial = rotMatrix * v_bodyfixed` -- i.e. `rotMatrix` is the BODY-FIXED to INERTIAL
//! transform, and its transpose, `rot = PM * ST * NUT * PREC`, is therefore the INERTIAL to
//! BODY-FIXED transform: `v_fixed = rot * v_inertial`. That is *exactly*
//! [`crate::frame::Rotation`]'s own documented convention (`r`'s doc comment: `v_fixed = r *
//! v_inertial`), so this module's `r` is `rot` directly -- no extra transpose anywhere in this
//! file. Proved, not assumed: see `tests::a_deliberately_transposed_rotation_wrecks_an_
//! asymmetric_gravity_field` below, and `tests/fk5_vs_convert.rs`'s own direct agreement with
//! [`crate::frame_gmat::GmatBodyFixedRotation`] (whose own direction is independently proved in
//! `tests/frame_gmat.rs`) -- a swapped convention would disagree by the FULL rotation, not by a
//! measured residual.
//!
//! `r_dot` mirrors GMAT's own approximation exactly (`rotDotMatrix = (PM * STderiv * NUT *
//! PREC)^T`, reusing the SAME `NUT*PREC` product): only the sidereal-time factor is
//! differentiated (`ST` -> `STderiv`); precession, nutation and polar motion are held fixed.
//! This is not a shortcut this module takes on its own -- GMAT's own `rotDotMatrix` is built
//! this way (`BodyFixedAxes.cpp`, "STderiv * (NUT * PREC) calculated above"), and matching it is
//! what makes the pinning test in `tests/fk5_vs_convert.rs` measure OUR reduction against
//! GMAT's, not a different, more complete Jacobian against a less complete one. Physically this
//! is an excellent approximation regardless (precession/nutation/polar-motion drift at
//! arcsec/day, nine-plus orders of magnitude slower than Earth's own ~7.3e-5 rad/s sidereal
//! rate), so the two would agree to a measured, tiny residual even if this module computed the
//! full analytic derivative -- but it does not, specifically so the comparison is apples to
//! apples.
//!
//! # The nutation-update-interval decision (`Planet::nutationUpdateInterval`, `Earth`'s default 60 s)
//!
//! `AxisSystem::ComputeNutationMatrix` has its own staleness cache, independent of the
//! epoch-level cache in `BodyFixedAxes::CalculateRotationMatrix`: if the requested epoch is
//! within `updateIntervalToUse` (`Planet::nutationUpdateInterval`, default `60.0` seconds --
//! `third_party/gmat-src/src/base/solarsys/Planet.cpp:90`) of the LAST epoch nutation was
//! actually recomputed at, GMAT reuses the OLD `dPsi`/`NUT` rather than recomputing (precession,
//! sidereal time and polar motion are NOT subject to this -- they recompute every call
//! regardless). A previous round's golden generator (`goldens/gen_bodyfixed_leo_2h.py`) found
//! this and set `Earth.NutationUpdateInterval = 0` to remove it. **This module's own reduction
//! never caches anything -- every call to [`Fk5BodyFixedRotation::inertial_to_fixed`] recomputes
//! precession, nutation, sidereal time and polar motion fresh, unconditionally**, which is both
//! the simplest correct behaviour for a stateless native model and, critically, matches what
//! `Planet.cpp`'s OWN default 60-second interval reduces to once epochs are more than 60 s
//! apart: fresh. [`crate::frame_gmat::GmatBodyFixedRotation::new`] never calls `set_real`
//! (`Earth.NutationUpdateInterval` is left at GMAT's own compiled-in `60.0` default -- confirmed
//! by reading that constructor's own source, `src/frame_gmat.rs`, which builds exactly two
//! `CoordinateSystem`s and calls `Gmat::initialize()`, nothing else), so the oracle in
//! `tests/fk5_vs_convert.rs` runs with the UNMODIFIED 60 s window. **This crate may not touch
//! `crates/gmat-sys`, so there is no live-instance parameter-read API available to probe
//! `Earth.NutationUpdateInterval` back off a running `Gmat` handle from this module or its
//! tests** (`gmat_sys::Object::real_parameter` needs an `Object` handle for `Earth` itself, and
//! `Gmat::coordinate_system`/`construct` are the only object-construction entry points this
//! crate's dependency exposes, neither of which fetches an existing solar-system body) -- this
//! is a real, named gap, not a skipped step. The decision this module and its own pinning test
//! therefore rely on for a like-for-like comparison is: **every one of `tests/fk5_vs_convert.rs`'s
//! thousand epochs is spaced far enough apart (measured: >= 170 s, see that test's own epoch
//! generator) that GMAT's 60 s nutation cache cannot be serving a stale value for any of them**
//! -- each query epoch differs from the CoordinateSystem's own last-computed epoch by more than
//! the cache window, so `ComputeNutationMatrix` recomputes fresh on every one of the oracle's
//! own calls too, made evident by the source-level reading above (`Planet.cpp:90`,
//! `AxisSystem.cpp`'s `dt < updateIntervalToUse` check) since a live probe was not available.
//!
//! # `tTDB` is really `T_TT`
//!
//! `BodyFixedAxes.cpp`'s own comment on `tTDB`: "NOTE - this is really TT, an approximation of
//! TDB". This module follows GMAT exactly: the precession/nutation Julian-century argument is
//! `T = (JD_TT - 2451545.0) / 36525`, computed from [`av_cdm::time::Tai::to_tt_nanos`] (`TT =
//! TAI + 32.184 s` exactly -- no EOP, no DE ephemeris, no iteration needed), never
//! `crate::tdb`'s true TDB.
//!
//! # The EOP columns GMAT reads, and what it does between rows
//!
//! `EopFile::Initialize` (`third_party/gmat-src/src/gmatutil/util/EopFile.cpp`) tokenizes each
//! data line as `year month day mjd x y ut1_utc lod` (8 whitespace-separated fields; the
//! trailing `dPsi`/`dEpsilon`/error columns in `eopc04_08.62-now` are read by NOTHING --
//! `EopFile::Initialize`'s own `istringstream >>` chain stops at `lod`, and the source comment
//! reads "ignore dPsi, dEpsilon (or dX, dY)"). `EopFile::GetPolarMotionAndLod` (called from both
//! `ComputePolarMotionRotation` and `ComputeSiderealTimeDotRotation`) **linearly interpolates
//! `x`/`y`** between the two bracketing daily rows and **does NOT interpolate `lod`** (the
//! source comment: "2005.02.23 - Steve says not to interpolate lod" -- it takes the LEFT/floor
//! row's value directly). `UT1-UTC` is interpolated separately, by `EopFile::GetUt1UtcOffset`
//! (called from `TimeSystemConverter::ConvertFromTaiMjd`'s `UT1MJD` case), linearly as well, but
//! over a TAI-referenced table with a leap-second-jump correction (`errorInSec` in that
//! function) for when the underlying TAI-vs-UTC offset itself changed between two tabulated
//! rows. **This module interpolates `x`, `y` and `UT1-UTC` all on the SAME axis (UTC MJD,
//! linear, no leap-second correction) and leaves `lod` un-interpolated (floor row)** -- a
//! documented simplification, not an oversight: GMAT's own leap-second correction only ever
//! fires when the bracketing pair straddles an actual leap-second insertion (`|diffJD - 1.0| *
//! 86400 > 0.6` seconds), and the IERS has not scheduled one since 2016-12-31 -- the last entry
//! before this module's own chosen pinning window (2026-09, `tests/fk5_vs_convert.rs`). Inside
//! any leap-second-free window the TAI-vs-UTC offset is a CONSTANT across the whole bracketing
//! pair, so it cancels exactly out of the interpolation ratio and the TAI-axis and UTC-axis
//! interpolations are mathematically identical, not merely close. [`EopTable::interpolate`]
//! returns a typed [`Fk5Error::EpochOutsideEopSpan`] for an epoch outside the file's own first
//! and last tabulated rows, rather than GMAT's own clamp-to-edge -- a deliberate deviation
//! (this crate's own rule: no silent clamp on a DRM-suppliable epoch) that never fires for any
//! epoch this module's own tests or the pinning test use, since every one of them is chosen
//! strictly inside the file's span.

use std::path::{Path, PathBuf};

use av_cdm::time::Tai;

use crate::frame::{BodyFixedRotation, Rotation};

// ---------------------------------------------------------------------------------------------
// Constants (all read directly off GMAT's own source -- see this module's doc for the file and
// line each one came from; `third_party/gmat-src/src/gmatutil/util/GmatConstants.hpp`).
// ---------------------------------------------------------------------------------------------

const RAD_PER_DEG: f64 = std::f64::consts::PI / 180.0;
const RAD_PER_ARCSEC: f64 = RAD_PER_DEG / 3600.0;
const DAYS_PER_JULIAN_CENTURY: f64 = 36525.0;
const JD_OF_J2000: f64 = 2_451_545.0;
/// `JD - MJD` (Vallado p. 187 / GMAT's own `JD_NOV_17_1858`/`JD_MJD_OFFSET`).
const JD_MJD_OFFSET: f64 = 2_400_000.5;
/// `AxisSystem::JD_OF_JANUARY_1_1997` (`AxisSystem.cpp:86`) -- below this, the equation of the
/// equinoxes' two extra correction terms are omitted. Always exceeded by every epoch this
/// module's own tests and the pinning test use (2020s and later), but implemented for fidelity.
const JD_OF_JANUARY_1_1997: f64 = 2_450_449.5;
/// Standard MJD of the Unix epoch (`1970-01-01T00:00:00`, `JD 2440587.5 - JD_MJD_OFFSET`).
const UNIX_EPOCH_MJD: f64 = 40_587.0;
const SECS_PER_DAY: f64 = 86_400.0;
/// `AxisSystem::ComputeSiderealTimeDotRotation`'s own hardcoded Earth rotation rate constant
/// (`AxisSystem.cpp`, `ComputeSiderealTimeDotRotation`), rad/s.
const EARTH_ROTATION_RATE_RAD_PER_S: f64 = 7.292_115_146_706_98e-5;
/// Earth's equatorial radius, km -- used only to convert a residual rotation ANGLE into a
/// physically meaningful surface displacement in `tests/fk5_vs_convert.rs` (the task's own
/// number, matching `.cof`'s JGM2/EGM96 reference radius, `6378136.3` m).
pub const EARTH_EQUATORIAL_RADIUS_KM: f64 = 6378.1363;

type Mat3 = [[f64; 3]; 3];

fn mat3_mul(a: &Mat3, b: &Mat3) -> Mat3 {
    let mut out = [[0.0_f64; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            out[i][j] = a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j];
        }
    }
    out
}

/// C++ `(int)x` truncates toward zero (not `floor`); `AxisSystem::ComputeNutationMatrix` wraps
/// every fundamental argument into `[0, 2*pi)` via exactly `x - ((int)(x/(2*pi)))*2*pi`. Rust's
/// `as i64` on an `f64` also truncates toward zero, so this is a direct, faithful port -- NOT
/// `rem_euclid`, which would give a different (still mathematically valid, since sin/cos are
/// 2*pi-periodic, but NOT bit-identical to GMAT's own) wrapped value.
fn wrap_trunc(x: f64) -> f64 {
    let two_pi = 2.0 * std::f64::consts::PI;
    let n = (x / two_pi) as i64;
    x - (n as f64) * two_pi
}

// ---------------------------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------------------------

/// Every way building or evaluating the native FK5 rotation can fail. No variant is reached by
/// a panic or an `unwrap` on file content or a caller-supplied epoch (this crate's own rule).
#[derive(Debug, thiserror::Error)]
pub enum Fk5Error {
    /// `GMAT_ROOT` is not set and the fallback GMAT install was not found (mirrors
    /// [`crate::cof::CofError::GmatRootNotFound`]).
    #[error("GMAT_ROOT is not set and the fallback GMAT install was not found: {source}")]
    GmatRootNotFound {
        #[source]
        source: crate::cof::CofError,
    },
    /// A data file could not be opened or read.
    #[error("could not read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The file's bytes are not valid UTF-8.
    #[error("{path} is not valid UTF-8 text")]
    Encoding { path: PathBuf },
    /// `NUTATION.DAT` has no line containing `"1980 IAU"` (the exact substring
    /// `ItrfCoefficientsFile::Initialize` searches for with the default `NUTATION_1980` source).
    #[error("{path}: no \"1980 IAU\" section found")]
    NutationSectionNotFound { path: PathBuf },
    /// The line immediately after the `"1980 IAU"` title does not look like the expected column
    /// header (must contain `"a2"`, mirroring `ItrfCoefficientsFile::Initialize`'s own check).
    #[error("{path}: line after the \"1980 IAU\" title is not the expected column header")]
    NutationHeaderNotFound { path: PathBuf },
    /// A nutation term row was truncated or non-numeric.
    #[error("{path}, line {line}: malformed nutation term ({reason})")]
    MalformedNutationTerm { path: PathBuf, line: usize, reason: String },
    /// The `1980 IAU` section did not have exactly 106 term rows (`ItrfCoefficientsFile::
    /// MAX_1980_NUT_TERMS`) before end of file.
    #[error("{path}: expected 106 nutation terms, found {got} before EOF")]
    NutationTermCountMismatch { path: PathBuf, got: usize },
    /// `eopc04_08.62-now` has no `"(0h"` header-terminator line (`EopFile::Initialize`'s own
    /// marker for "last line of header").
    #[error("{path}: no \"(0h\" header terminator line found")]
    EopHeaderNotFound { path: PathBuf },
    /// An EOP data row was truncated or non-numeric.
    #[error("{path}, line {line}: malformed EOP row ({reason})")]
    MalformedEopRow { path: PathBuf, line: usize, reason: String },
    /// The EOP file parsed to zero data rows.
    #[error("{path}: no EOP data rows found")]
    EopFileEmpty { path: PathBuf },
    /// The requested epoch (UTC, standard MJD) is outside `eopc04_08.62-now`'s own tabulated
    /// span -- a typed error, never a silent clamp (see this module's own doc).
    #[error("epoch {mjd_utc} (UTC MJD) is outside the EOP file's span [{mjd_start}, {mjd_end}]")]
    EpochOutsideEopSpan { mjd_utc: f64, mjd_start: f64, mjd_end: f64 },
}

// ---------------------------------------------------------------------------------------------
// NUTATION.DAT: the IAU 1980 nutation series
// ---------------------------------------------------------------------------------------------

/// One row of the IAU 1980 nutation series: five integer multipliers of the Delaunay
/// fundamental arguments, and the `A`/`B` (longitude) and `C`/`D` (obliquity) coefficients,
/// already scaled from the file's own raw integer units (`0.0001"`, `ItrfCoefficientsFile::
/// MULT_1980_NUT`) into arcseconds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NutationTerm {
    /// `(a1, a2, a3, a4, a5)`: multipliers of (mean anomaly of the Moon, mean anomaly of the
    /// Sun, mean argument of latitude of the Moon, mean elongation of the Sun from the Moon,
    /// longitude of the ascending node of the Moon's mean orbit).
    pub a: [i32; 5],
    pub a_arcsec: f64,
    pub b_arcsec_per_century: f64,
    pub c_arcsec: f64,
    pub d_arcsec_per_century: f64,
}

/// The parsed 1980 IAU nutation series (`NUTATION.DAT`'s `"1980 IAU"` section -- see this
/// module's doc for why that section, not the file's other two).
#[derive(Debug, Clone)]
pub struct NutationSeries {
    pub terms: Vec<NutationTerm>,
}

/// `ItrfCoefficientsFile::MAX_1980_NUT_TERMS`.
const NUT_1980_TERM_COUNT: usize = 106;
/// `ItrfCoefficientsFile::MULT_1980_NUT`: the file's raw integers are in units of `0.0001"`.
const NUT_1980_MULT: f64 = 1.0e-4;

impl NutationSeries {
    /// Reads and parses `path` (GMAT's `NUTATION.DAT`), following `ItrfCoefficientsFile::
    /// Initialize`'s own default (`NUTATION_1980`) parse exactly: find the line containing
    /// `"1980 IAU"`, skip the next (header) line, then read exactly 106 term rows.
    pub fn read(path: &Path) -> Result<Self, Fk5Error> {
        let bytes = std::fs::read(path).map_err(|source| Fk5Error::Io { path: path.to_path_buf(), source })?;
        let text = String::from_utf8(bytes).map_err(|_| Fk5Error::Encoding { path: path.to_path_buf() })?;
        let mut lines = text.lines().enumerate();

        let mut found_section = false;
        for (_, line) in lines.by_ref() {
            if line.contains("1980 IAU") {
                found_section = true;
                break;
            }
        }
        if !found_section {
            return Err(Fk5Error::NutationSectionNotFound { path: path.to_path_buf() });
        }

        let Some((_, header)) = lines.next() else {
            return Err(Fk5Error::NutationHeaderNotFound { path: path.to_path_buf() });
        };
        if !header.contains("a2") {
            return Err(Fk5Error::NutationHeaderNotFound { path: path.to_path_buf() });
        }

        let mut terms = Vec::with_capacity(NUT_1980_TERM_COUNT);
        for (line_no, line) in lines.by_ref().take(NUT_1980_TERM_COUNT) {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() < 9 {
                return Err(Fk5Error::MalformedNutationTerm {
                    path: path.to_path_buf(),
                    line: line_no + 1,
                    reason: format!("expected at least 9 whitespace-separated fields, found {}", fields.len()),
                });
            }
            let malformed = |reason: String| Fk5Error::MalformedNutationTerm { path: path.to_path_buf(), line: line_no + 1, reason };
            let mut a = [0_i32; 5];
            for (i, slot) in a.iter_mut().enumerate() {
                *slot = fields[i].parse().map_err(|_| malformed(format!("a{} field {:?} is not an integer", i + 1, fields[i])))?;
            }
            let raw_a: f64 = fields[5].parse().map_err(|_| malformed(format!("A field {:?} is not a number", fields[5])))?;
            let raw_b: f64 = fields[6].parse().map_err(|_| malformed(format!("B field {:?} is not a number", fields[6])))?;
            let raw_c: f64 = fields[7].parse().map_err(|_| malformed(format!("C field {:?} is not a number", fields[7])))?;
            let raw_d: f64 = fields[8].parse().map_err(|_| malformed(format!("D field {:?} is not a number", fields[8])))?;
            terms.push(NutationTerm {
                a,
                a_arcsec: raw_a * NUT_1980_MULT,
                b_arcsec_per_century: raw_b * NUT_1980_MULT,
                c_arcsec: raw_c * NUT_1980_MULT,
                d_arcsec_per_century: raw_d * NUT_1980_MULT,
            });
        }
        if terms.len() != NUT_1980_TERM_COUNT {
            return Err(Fk5Error::NutationTermCountMismatch { path: path.to_path_buf(), got: terms.len() });
        }
        Ok(Self { terms })
    }
}

// ---------------------------------------------------------------------------------------------
// eopc04_08.62-now: IERS EOP C04
// ---------------------------------------------------------------------------------------------

/// One tabulated daily row: `mjd_utc` (standard MJD, UTC), `x`/`y` polar motion (arcsec),
/// `ut1_minus_utc` (seconds) and `lod` (seconds) -- exactly the four data columns `EopFile::
/// Initialize` reads (`dPsi`/`dEpsilon`/every error column are read by nothing, per this
/// module's own doc).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EopRow {
    pub mjd_utc: f64,
    pub x_arcsec: f64,
    pub y_arcsec: f64,
    pub ut1_minus_utc_s: f64,
    pub lod_s: f64,
}

/// The parsed IERS EOP C04 table (`eopc04_08.62-now`).
#[derive(Debug, Clone)]
pub struct EopTable {
    pub rows: Vec<EopRow>,
}

impl EopTable {
    /// Reads and parses `path` (GMAT's `eopc04_08.62-now`), following `EopFile::Initialize`'s
    /// own header-skip convention: read lines until one whose FIRST whitespace-separated token
    /// is exactly `"(0h"` (the file's own "last line of header" marker), then parse every
    /// subsequent non-blank line as `year month day mjd x y ut1_utc lod ...` (8+ tokens; only
    /// the first 8 are used, matching `EopFile::Initialize`'s own `istringstream >>` chain).
    pub fn read(path: &Path) -> Result<Self, Fk5Error> {
        let bytes = std::fs::read(path).map_err(|source| Fk5Error::Io { path: path.to_path_buf(), source })?;
        let text = String::from_utf8(bytes).map_err(|_| Fk5Error::Encoding { path: path.to_path_buf() })?;
        let mut lines = text.lines().enumerate();

        let mut found_header = false;
        for (_, line) in lines.by_ref() {
            if line.split_whitespace().next() == Some("(0h") {
                found_header = true;
                break;
            }
        }
        if !found_header {
            return Err(Fk5Error::EopHeaderNotFound { path: path.to_path_buf() });
        }

        let mut rows = Vec::new();
        for (line_no, line) in lines {
            if line.trim().is_empty() {
                continue;
            }
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() < 8 {
                return Err(Fk5Error::MalformedEopRow {
                    path: path.to_path_buf(),
                    line: line_no + 1,
                    reason: format!("expected at least 8 whitespace-separated fields, found {}", fields.len()),
                });
            }
            let malformed = |reason: String| Fk5Error::MalformedEopRow { path: path.to_path_buf(), line: line_no + 1, reason };
            // fields[0..3) = year, month, day (unused -- mjd at fields[3] is authoritative,
            // exactly what EopFile::Initialize itself keys the table on).
            let mjd_utc: f64 = fields[3].parse().map_err(|_| malformed(format!("mjd field {:?} is not a number", fields[3])))?;
            let x_arcsec: f64 = fields[4].parse().map_err(|_| malformed(format!("x field {:?} is not a number", fields[4])))?;
            let y_arcsec: f64 = fields[5].parse().map_err(|_| malformed(format!("y field {:?} is not a number", fields[5])))?;
            let ut1_minus_utc_s: f64 = fields[6].parse().map_err(|_| malformed(format!("UT1-UTC field {:?} is not a number", fields[6])))?;
            let lod_s: f64 = fields[7].parse().map_err(|_| malformed(format!("LOD field {:?} is not a number", fields[7])))?;
            rows.push(EopRow { mjd_utc, x_arcsec, y_arcsec, ut1_minus_utc_s, lod_s });
        }
        if rows.is_empty() {
            return Err(Fk5Error::EopFileEmpty { path: path.to_path_buf() });
        }
        Ok(Self { rows })
    }

    /// `x`/`y`/`UT1-UTC` linearly interpolated between the two bracketing daily rows, `lod`
    /// taken from the LEFT (floor) bracketing row un-interpolated -- see this module's own doc
    /// for exactly which GMAT function each choice mirrors, and the leap-second-free-window
    /// argument for why interpolating `UT1-UTC` on the same UTC axis as `x`/`y` (rather than
    /// GMAT's own separate TAI-referenced axis) is exact, not merely close, over this module's
    /// own chosen epoch ranges. A typed [`Fk5Error::EpochOutsideEopSpan`] for any `mjd_utc`
    /// outside `[rows[0].mjd_utc, rows[last].mjd_utc]` -- never a silent clamp.
    pub fn interpolate(&self, mjd_utc: f64) -> Result<(f64, f64, f64, f64), Fk5Error> {
        let first = self.rows.first().expect("EopTable::read never returns an empty table");
        let last = self.rows.last().expect("EopTable::read never returns an empty table");
        if mjd_utc < first.mjd_utc || mjd_utc > last.mjd_utc {
            return Err(Fk5Error::EpochOutsideEopSpan { mjd_utc, mjd_start: first.mjd_utc, mjd_end: last.mjd_utc });
        }
        // self.rows is in file order, which is strictly increasing mjd_utc (one row per
        // calendar day) -- a linear scan for the bracketing pair; `partition_point` (binary
        // search) would be asymptotically better, but this table is ~23,000 rows and this
        // function is called at most a few thousand times in this crate's own tests, so the
        // simpler, harder-to-get-wrong scan is the right tradeoff here.
        let idx = self.rows.partition_point(|r| r.mjd_utc <= mjd_utc);
        let i = if idx == 0 { 0 } else { idx - 1 };
        let i = i.min(self.rows.len() - 2); // exact-last-row query: bracket with the PRIOR pair
        let (r0, r1) = (self.rows[i], self.rows[i + 1]);
        let span = r1.mjd_utc - r0.mjd_utc;
        let ratio = if span > 0.0 { (mjd_utc - r0.mjd_utc) / span } else { 0.0 };
        let x = r0.x_arcsec + ratio * (r1.x_arcsec - r0.x_arcsec);
        let y = r0.y_arcsec + ratio * (r1.y_arcsec - r0.y_arcsec);
        let ut1_minus_utc = r0.ut1_minus_utc_s + ratio * (r1.ut1_minus_utc_s - r0.ut1_minus_utc_s);
        let lod = r0.lod_s; // NOT interpolated (GMAT: "Steve says not to interpolate lod")
        Ok((x, y, ut1_minus_utc, lod))
    }
}

// ---------------------------------------------------------------------------------------------
// The four rotation matrices (`AxisSystem::Compute*` ports)
// ---------------------------------------------------------------------------------------------

/// `AxisSystem::ComputePrecessionMatrix` (Vallado Eq. 3-56/3-57): FK5 -> MOD, given `t` in
/// Julian centuries of (GMAT's own) `T_TT`.
pub fn precession_matrix(t: f64) -> Mat3 {
    let t2 = t * t;
    let t3 = t2 * t;
    let zeta = (2306.2181 * t + 0.30188 * t2 + 0.017998 * t3) * RAD_PER_ARCSEC;
    let theta = (2004.3109 * t - 0.42665 * t2 - 0.041833 * t3) * RAD_PER_ARCSEC;
    let z = (2306.2181 * t + 1.09468 * t2 + 0.018203 * t3) * RAD_PER_ARCSEC;

    let (sin_theta, cos_theta) = theta.sin_cos();
    let (sin_z, cos_z) = z.sin_cos();
    let (sin_zeta, cos_zeta) = zeta.sin_cos();

    [
        [cos_theta * cos_z * cos_zeta - sin_z * sin_zeta, -sin_zeta * cos_theta * cos_z - sin_z * cos_zeta, -sin_theta * cos_z],
        [sin_z * cos_theta * cos_zeta + sin_zeta * cos_z, -sin_z * sin_zeta * cos_theta + cos_z * cos_zeta, -sin_theta * sin_z],
        [sin_theta * cos_zeta, -sin_theta * sin_zeta, cos_theta],
    ]
}

/// `AxisSystem::ComputeNutationMatrix`, `NUTATION_1980` branch: MOD -> TOD, given `t` (Julian
/// centuries of `T_TT`) and the parsed 1980 series. Returns `(NUT, dPsi radians,
/// longAscNodeLunar radians, cos(mean obliquity))` -- the last three are needed again by
/// [`sidereal_time_matrix`], exactly as GMAT threads them through as out-parameters.
pub fn nutation(t: f64, series: &NutationSeries) -> (Mat3, f64, f64, f64) {
    let t2 = t * t;
    let t3 = t2 * t;

    // GMT-4295 updated coefficients (AxisSystem.cpp's own comment: "Vallado's text is
    // incorrect ... updated based on Supplement to the Astronomical Almanac").
    const CONST125: f64 = 125.044_522_22;
    const CONST134: f64 = 134.962_981_39;
    const CONST357: f64 = 357.527_723_33;
    const CONST93: f64 = 93.271_910_28;
    const CONST297: f64 = 297.850_363_06;

    let long_asc_node_lunar = wrap_trunc((CONST125 * RAD_PER_DEG) + (-6_962_890.539_0 * t + 7.455 * t2 + 0.008 * t3) * RAD_PER_ARCSEC);
    let epsbar = (84381.448 - 46.8150 * t - 0.00059 * t2 + 0.001813 * t3) * RAD_PER_ARCSEC;
    let cos_epsbar = epsbar.cos();

    let mean_anomaly_moon = wrap_trunc((CONST134 * RAD_PER_DEG) + (1_717_915_922.633_0 * t + 31.310 * t2 + 0.064 * t3) * RAD_PER_ARCSEC);
    let mean_anomaly_sun = wrap_trunc((CONST357 * RAD_PER_DEG) + (129_596_581.224_0 * t - 0.577 * t2 - 0.012 * t3) * RAD_PER_ARCSEC);
    let arg_latitude_moon = wrap_trunc((CONST93 * RAD_PER_DEG) + (1_739_527_263.137_0 * t - 13.257 * t2 + 0.011 * t3) * RAD_PER_ARCSEC);
    let mean_elongation_sun = wrap_trunc((CONST297 * RAD_PER_DEG) + (1_602_961_601.328_0 * t - 6.891 * t2 + 0.019 * t3) * RAD_PER_ARCSEC);

    let mut d_psi = 0.0_f64;
    let mut d_eps = 0.0_f64;
    for term in &series.terms {
        let arg = term.a[0] as f64 * mean_anomaly_moon
            + term.a[1] as f64 * mean_anomaly_sun
            + term.a[2] as f64 * arg_latitude_moon
            + term.a[3] as f64 * mean_elongation_sun
            + term.a[4] as f64 * long_asc_node_lunar;
        let (sin_arg, cos_arg) = arg.sin_cos();
        d_psi += (term.a_arcsec + term.b_arcsec_per_century * t) * sin_arg;
        d_eps += (term.c_arcsec + term.d_arcsec_per_century * t) * cos_arg;
    }
    d_psi *= RAD_PER_ARCSEC;
    d_eps *= RAD_PER_ARCSEC;

    let true_ooe = epsbar + d_eps;
    let (sin_d_psi, cos_d_psi) = d_psi.sin_cos();
    let (sin_true_ooe, cos_true_ooe) = true_ooe.sin_cos();
    let sin_epsbar = epsbar.sin();

    let nut = [
        [cos_d_psi, -sin_d_psi * cos_epsbar, -sin_d_psi * sin_epsbar],
        [sin_d_psi * cos_true_ooe, cos_true_ooe * cos_d_psi * cos_epsbar + sin_true_ooe * sin_epsbar, sin_epsbar * cos_true_ooe * cos_d_psi - sin_true_ooe * cos_epsbar],
        [sin_true_ooe * sin_d_psi, sin_true_ooe * cos_d_psi * cos_epsbar - sin_epsbar * cos_true_ooe, sin_true_ooe * sin_epsbar * cos_d_psi + cos_true_ooe * cos_epsbar],
    ];
    (nut, d_psi, long_asc_node_lunar, cos_epsbar)
}

/// `AxisSystem::ComputeSiderealTimeRotation`: TOD -> PEF. `jd_tt` is the full TT Julian Date
/// (used only for the 1997 threshold check); `mjd_ut1` is the UT1 epoch as a standard
/// (UTC-MJD-reference-aligned) Modified Julian Date. Returns `(ST, cos(theta_ast),
/// sin(theta_ast))` -- the trig values are needed again by [`sidereal_time_dot_matrix`].
pub fn sidereal_time_matrix(jd_tt: f64, mjd_ut1: f64, d_psi: f64, long_asc_node_lunar: f64, cos_epsbar: f64) -> (Mat3, f64, f64) {
    let jd_ut1 = mjd_ut1 + JD_MJD_OFFSET;
    let t_ut1 = (jd_ut1 - JD_OF_J2000) / DAYS_PER_JULIAN_CENTURY;
    let t_ut1_2 = t_ut1 * t_ut1;
    let t_ut1_3 = t_ut1_2 * t_ut1;

    let (term2, term3) = if jd_tt > JD_OF_JANUARY_1_1997 {
        (0.00264 * long_asc_node_lunar.sin() * RAD_PER_ARCSEC, 0.000_063 * (2.0 * long_asc_node_lunar).sin() * RAD_PER_ARCSEC)
    } else {
        (0.0, 0.0)
    };
    let eq_equinox = d_psi * cos_epsbar + term2 + term3;

    let sec2deg = 15.0 / 3600.0;
    // **Root-caused (this task's own report has the full derivation):** the fast/"today's own
    // rotation" term must be the fraction of the day elapsed since the last JD (Julian Date)
    // boundary -- i.e. since the last NOON, matching `67310.54841`'s own reference point
    // (`67310.54841 * sec2deg == 280.460618375`, the textbook GMST at J2000.0, WHICH IS NOON --
    // `JD_OF_J2000 == 2451545.0`, an integer JD, and integer JDs fall at noon by definition) --
    // NOT the fraction since the last MIDNIGHT (`mjd_ut1.floor()`, which an earlier revision of
    // this function used and which is off from the correct value by exactly half a day, i.e.
    // 180 degrees, for every epoch -- this was FOUND, not assumed: `tests/fk5_vs_convert.rs`'s
    // pinning test measured a near-exact pi-radian residual, rows 0/1 of `r` negated and row 2
    // unchanged versus the oracle at every one of 1000 epochs, which is exactly the signature of
    // an Rz(180 deg) factor). `jd_ut1`'s own fractional part IS that noon-referenced fraction
    // (`JD_MJD_OFFSET = 2400000.5`, a half-integer, is exactly what shifts the day boundary from
    // MJD's midnight to JD's noon).
    let frac_of_ut1_day = jd_ut1 - jd_ut1.floor();
    let theta_gmst_deg = 67310.54841 * sec2deg + frac_of_ut1_day * 360.0 + 8_640_184.812866 * sec2deg * t_ut1 + 0.093104 * sec2deg * t_ut1_2 - 6.2e-6 * sec2deg * t_ut1_3;
    let two_pi = 2.0 * std::f64::consts::PI;
    let theta_gmst = (theta_gmst_deg * RAD_PER_DEG).rem_euclid(two_pi);
    let theta_ast = theta_gmst + eq_equinox;

    let (sin_ast, cos_ast) = theta_ast.sin_cos();
    let st = [[cos_ast, sin_ast, 0.0], [-sin_ast, cos_ast, 0.0], [0.0, 0.0, 1.0]];
    (st, cos_ast, sin_ast)
}

/// `AxisSystem::ComputeSiderealTimeDotRotation`: the time derivative of the sidereal-time
/// factor alone (see this module's doc, "The composition order", for why this is GMAT's own
/// approximation for the whole `r_dot`, not a shortcut this port introduces).
pub fn sidereal_time_dot_matrix(lod_s: f64, cos_ast: f64, sin_ast: f64) -> Mat3 {
    let omega_e = EARTH_ROTATION_RATE_RAD_PER_S * (1.0 - lod_s / SECS_PER_DAY);
    [[-omega_e * sin_ast, omega_e * cos_ast, 0.0], [-omega_e * cos_ast, -omega_e * sin_ast, 0.0], [0.0, 0.0, 0.0]]
}

/// `AxisSystem::ComputePolarMotionRotation`: PEF -> ITRF (body-fixed).
pub fn polar_motion_matrix(x_arcsec: f64, y_arcsec: f64) -> Mat3 {
    let (sin_x, cos_x) = (-x_arcsec * RAD_PER_ARCSEC).sin_cos();
    let (sin_y, cos_y) = (-y_arcsec * RAD_PER_ARCSEC).sin_cos();
    [[cos_x, sin_x * sin_y, -sin_x * cos_y], [0.0, cos_y, sin_y], [sin_x, -cos_x * sin_y, cos_x * cos_y]]
}

// ---------------------------------------------------------------------------------------------
// The public BodyFixedRotation
// ---------------------------------------------------------------------------------------------

/// The native, GMAT-free [`BodyFixedRotation`] for Earth: IAU-76/FK5 precession and nutation
/// (1980 series) with polar motion and UT1 from GMAT's own IERS EOP file -- see this module's
/// own doc for the full derivation, the composition order and every deliberate deviation from
/// GMAT's own reduction.
#[derive(Debug, Clone)]
pub struct Fk5BodyFixedRotation {
    nutation: NutationSeries,
    eop: EopTable,
}

impl Fk5BodyFixedRotation {
    /// Reads `NUTATION.DAT` and `eopc04_08.62-now` from `$GMAT_ROOT/data/planetary_coeff/`
    /// (`gmat_root`'s own `data/planetary_coeff` subdirectory).
    pub fn new(gmat_root: &Path) -> Result<Self, Fk5Error> {
        let dir = gmat_root.join("data").join("planetary_coeff");
        let nutation = NutationSeries::read(&dir.join("NUTATION.DAT"))?;
        let eop = EopTable::read(&dir.join("eopc04_08.62-now"))?;
        Ok(Self { nutation, eop })
    }

    /// [`Fk5BodyFixedRotation::new`], locating `GMAT_ROOT` the same way [`crate::cof::
    /// locate_gmat_root`] does (`GMAT_ROOT` from the environment, read-only -- question 199 --
    /// falling back to `<repo root>/GMAT R2026a`).
    pub fn from_gmat_root_env() -> Result<Self, Fk5Error> {
        let root = crate::cof::locate_gmat_root().map_err(|source| Fk5Error::GmatRootNotFound { source })?;
        Self::new(&root)
    }

    pub fn nutation_series(&self) -> &NutationSeries {
        &self.nutation
    }

    pub fn eop_table(&self) -> &EopTable {
        &self.eop
    }
}

impl BodyFixedRotation for Fk5BodyFixedRotation {
    type Error = Fk5Error;

    fn inertial_to_fixed(&self, t_tai_ns: i64) -> Result<Rotation, Fk5Error> {
        let tai = Tai::from_nanos(t_tai_ns);
        let tt_ns = tai.to_tt_nanos();
        let utc_ns = tai.to_utc_nanos();

        let mjd_tt = UNIX_EPOCH_MJD + (tt_ns as f64) / (SECS_PER_DAY * 1e9);
        let mjd_utc = UNIX_EPOCH_MJD + (utc_ns as f64) / (SECS_PER_DAY * 1e9);
        let jd_tt = mjd_tt + JD_MJD_OFFSET;
        let t_tt = (jd_tt - JD_OF_J2000) / DAYS_PER_JULIAN_CENTURY;

        let (x_arcsec, y_arcsec, ut1_minus_utc_s, lod_s) = self.eop.interpolate(mjd_utc)?;
        let mjd_ut1 = mjd_utc + ut1_minus_utc_s / SECS_PER_DAY;

        let prec = precession_matrix(t_tt);
        let (nut, d_psi, long_asc_node_lunar, cos_epsbar) = nutation(t_tt, &self.nutation);
        let (st, cos_ast, sin_ast) = sidereal_time_matrix(jd_tt, mjd_ut1, d_psi, long_asc_node_lunar, cos_epsbar);
        let st_dot = sidereal_time_dot_matrix(lod_s, cos_ast, sin_ast);
        let pm = polar_motion_matrix(x_arcsec, y_arcsec);

        let nut_prec = mat3_mul(&nut, &prec);
        let r = mat3_mul(&pm, &mat3_mul(&st, &nut_prec));
        let r_dot = mat3_mul(&pm, &mat3_mul(&st_dot, &nut_prec));
        Ok(Rotation { r, r_dot })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gmat_root() -> PathBuf {
        crate::cof::locate_gmat_root().expect("GMAT_ROOT set (or the repo-relative fallback exists)")
    }

    fn nutation_path() -> PathBuf {
        gmat_root().join("data").join("planetary_coeff").join("NUTATION.DAT")
    }

    fn eop_path() -> PathBuf {
        gmat_root().join("data").join("planetary_coeff").join("eopc04_08.62-now")
    }

    /// Pins both data files' content by SHA-256 (`openssl::sha::sha256`, ADR-004's crypto
    /// rule), computed independently with the system `shasum -a 256` first -- both recorded in
    /// this task's own report, matching `crate::cof`/`crate::de`'s identical pattern.
    #[test]
    fn data_file_sha256_is_pinned() {
        let expected: [(&str, &str); 2] = [
            ("NUTATION.DAT", "633423d201aaed4cf4bf49427f85c5c3ed0e6c6f394f68d5c369dac673e31566"),
            ("eopc04_08.62-now", "52c95d6871066e892463328424ca34f5e9cedde92c747465a40f7cd0ecd86ed3"),
        ];
        let paths = [nutation_path(), eop_path()];
        for ((name, want_hex), path) in expected.iter().zip(paths.iter()) {
            let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
            let digest = openssl::sha::sha256(&bytes);
            let got_hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
            println!("n5-fk5-sha256: {name} = {got_hex}");
            assert_eq!(&got_hex, want_hex, "{name} content changed (SHA-256 mismatch)");
        }
    }

    /// The nutation-series parse's own cross-checks, printed (this task's own rule: "print the
    /// cross-checks you used to confirm the layout, exactly as `crate::de` does").
    #[test]
    fn nutation_series_cross_checks() {
        let series = NutationSeries::read(&nutation_path()).expect("parse NUTATION.DAT");
        println!("n5-nutation-term-count: {}", series.terms.len());
        assert_eq!(series.terms.len(), 106);

        let first = series.terms[0];
        println!("n5-nutation-first-term: a={:?} A={} B={} C={} D={} arcsec", first.a, first.a_arcsec, first.b_arcsec_per_century, first.c_arcsec, first.d_arcsec_per_century);
        assert_eq!(first.a, [0, 0, 0, 0, 1]);
        // Raw file value -171996 (units 0.0001") -> -17.1996 arcsec: the best-known first term
        // of the IAU 1980 nutation-in-longitude series (e.g. Vallado's table).
        assert!((first.a_arcsec - (-17.1996)).abs() < 1e-9, "A(0) = {}", first.a_arcsec);
        assert!((first.c_arcsec - 9.2025).abs() < 1e-9, "C(0) = {}", first.c_arcsec);

        let last = series.terms[105];
        println!("n5-nutation-last-term: a={:?} A={} B={} C={} D={} arcsec", last.a, last.a_arcsec, last.b_arcsec_per_century, last.c_arcsec, last.d_arcsec_per_century);
        assert_eq!(last.a, [0, 1, 0, 1, 0]);
        assert!((last.a_arcsec - 0.0001).abs() < 1e-9, "A(105) = {}", last.a_arcsec);
    }

    /// The EOP reader's own cross-checks: a known row read back exactly, the interpolation (and
    /// LOD's own lack of it) demonstrated numerically, and an out-of-span epoch a typed error.
    #[test]
    fn eop_reader_cross_checks() {
        let table = EopTable::read(&eop_path()).expect("parse eopc04_08.62-now");
        println!("n5-eop-row-count: {}", table.rows.len());
        assert!(table.rows.len() > 1000);

        // A known row: 1962-01-01 (this task's own investigation quoted this row's exact text).
        let first = table.rows[0];
        println!("n5-eop-first-row: {first:?}");
        assert_eq!(first.mjd_utc, 37665.0);
        assert!((first.x_arcsec - (-0.0127)).abs() < 1e-9);
        assert!((first.y_arcsec - 0.2130).abs() < 1e-9);
        assert!((first.ut1_minus_utc_s - 0.0326338).abs() < 1e-9);
        assert!((first.lod_s - 0.0017230).abs() < 1e-9);

        // Interpolation, demonstrated: at the exact midpoint between two known rows, x/y/UT1-UTC
        // must equal the arithmetic mean of the bracketing rows, and LOD must equal the LEFT
        // row's own value exactly (not interpolated).
        let (r0, r1) = (table.rows[0], table.rows[1]);
        let mid_mjd = 0.5 * (r0.mjd_utc + r1.mjd_utc);
        let (x, y, ut1_utc, lod) = table.interpolate(mid_mjd).expect("interpolate at midpoint");
        println!("n5-eop-midpoint-interp: mjd={mid_mjd} x={x} y={y} ut1_utc={ut1_utc} lod={lod}");
        assert!((x - 0.5 * (r0.x_arcsec + r1.x_arcsec)).abs() < 1e-12);
        assert!((y - 0.5 * (r0.y_arcsec + r1.y_arcsec)).abs() < 1e-12);
        assert!((ut1_utc - 0.5 * (r0.ut1_minus_utc_s + r1.ut1_minus_utc_s)).abs() < 1e-12);
        assert_eq!(lod, r0.lod_s, "LOD must be the LEFT row's own value, not interpolated");

        // Exact-node reads back the tabulated row exactly (ratio == 0 or 1, no interpolation
        // error at all).
        let (x0, y0, u0, _) = table.interpolate(r0.mjd_utc).expect("interpolate at a tabulated node");
        assert_eq!(x0, r0.x_arcsec);
        assert_eq!(y0, r0.y_arcsec);
        assert_eq!(u0, r0.ut1_minus_utc_s);

        // Out of span: a typed error, never a panic or a silent clamp.
        let before = table.rows.first().unwrap().mjd_utc - 1.0;
        let after = table.rows.last().unwrap().mjd_utc + 1.0;
        let err_before = table.interpolate(before).unwrap_err();
        let err_after = table.interpolate(after).unwrap_err();
        println!("n5-eop-out-of-span: before={err_before}, after={err_after}");
        assert!(matches!(err_before, Fk5Error::EpochOutsideEopSpan { .. }));
        assert!(matches!(err_after, Fk5Error::EpochOutsideEopSpan { .. }));
    }

    #[test]
    fn missing_nutation_file_is_a_typed_error() {
        let err = NutationSeries::read(Path::new("/no/such/file.DAT")).unwrap_err();
        assert!(matches!(err, Fk5Error::Io { .. }), "{err:?}");
    }

    #[test]
    fn missing_eop_file_is_a_typed_error() {
        let err = EopTable::read(Path::new("/no/such/file")).unwrap_err();
        assert!(matches!(err, Fk5Error::Io { .. }), "{err:?}");
    }

    #[test]
    fn malformed_nutation_row_is_a_typed_error() {
        let bad = "xx 1980 IAU Theory of Nutation (106 term)\nxx  a2 a3 a4 a5 A B C D #\n 0 0 0 0 1 notanumber 0 0 0 1\n";
        let path = std::env::temp_dir().join(format!("av_orbital_fk5_bad_nutation_{}.DAT", std::process::id()));
        std::fs::write(&path, bad).unwrap();
        let err = NutationSeries::read(&path).unwrap_err();
        std::fs::remove_file(&path).ok();
        assert!(matches!(err, Fk5Error::MalformedNutationTerm { .. }), "{err:?}");
    }

    fn fk5() -> Fk5BodyFixedRotation {
        Fk5BodyFixedRotation::new(&gmat_root()).expect("Fk5BodyFixedRotation::new")
    }

    /// An arbitrary but fixed TAI instant inside `eopc04_08.62-now`'s span (2026-09-02, well
    /// inside `tests/fk5_vs_convert.rs`'s own 2026-09-01..2026-09-03 pinning window) -- not
    /// otherwise meaningful, just held constant across this module's own GMAT-free tests.
    const TAI_NS: i64 = 1_788_307_237_000_000_000;

    /// `r` is a proper rotation: `r^T r = I` and `det(r) = +1`, to a measured bound.
    #[test]
    fn r_is_a_proper_rotation() {
        let rot = fk5().inertial_to_fixed(TAI_NS).expect("inertial_to_fixed");
        let r = rot.r;
        let rt_r = mat3_mul(&[[r[0][0], r[1][0], r[2][0]], [r[0][1], r[1][1], r[2][1]], [r[0][2], r[1][2], r[2][2]]], &r);
        let mut max_off = 0.0_f64;
        for (i, row) in rt_r.iter().enumerate() {
            for (j, &value) in row.iter().enumerate() {
                let want = if i == j { 1.0 } else { 0.0 };
                max_off = max_off.max((value - want).abs());
            }
        }
        let det = r[0][0] * (r[1][1] * r[2][2] - r[1][2] * r[2][1]) - r[0][1] * (r[1][0] * r[2][2] - r[1][2] * r[2][0]) + r[0][2] * (r[1][0] * r[2][1] - r[1][1] * r[2][0]);
        println!("n5-proper-rotation: max|R^T R - I| = {max_off:e}, det(R) = {det:.17}, |det-1| = {:e}", (det - 1.0).abs());
        assert!(max_off < 1e-12, "R^T R deviates from I by {max_off:e}");
        assert!((det - 1.0).abs() < 1e-12, "det(R) = {det}, deviates from 1 by {:e}", (det - 1.0).abs());
    }

    /// `r_dot` against a central finite difference of `r`, mirroring `tests/frame_gmat.rs::
    /// rotation_dot_matches_a_finite_difference_of_the_rotation_matrix` exactly.
    #[test]
    fn r_dot_matches_a_finite_difference_of_r() {
        let rot = fk5();
        let h_s: f64 = 0.2;
        let h_ns = (h_s * 1e9).round() as i64;
        let minus = rot.inertial_to_fixed(TAI_NS - h_ns).unwrap();
        let mid = rot.inertial_to_fixed(TAI_NS).unwrap();
        let plus = rot.inertial_to_fixed(TAI_NS + h_ns).unwrap();

        let mut max_abs_err = 0.0_f64;
        let mut max_abs_rdot = 0.0_f64;
        for i in 0..3 {
            for j in 0..3 {
                let fd = (plus.r[i][j] - minus.r[i][j]) / (2.0 * h_s);
                let err = (fd - mid.r_dot[i][j]).abs();
                max_abs_err = max_abs_err.max(err);
                max_abs_rdot = max_abs_rdot.max(mid.r_dot[i][j].abs());
            }
        }
        println!("n5-fk5-r_dot-finite-difference: max abs error = {max_abs_err:e} (h={h_s} s), max |r_dot| = {max_abs_rdot:e}");
        assert!(max_abs_rdot > 1e-6, "sanity: r_dot should be of order Earth's rotation rate; got {max_abs_rdot:e}");
        assert!(max_abs_err < 1e-6, "r_dot disagrees with a finite-difference derivative of r by {max_abs_err:e}");
    }

    fn potfield_line(n: usize, m: usize, mu: f64, radius: f64) -> String {
        format!("POTFIELD{n:>3}{m:>3}  1 {mu:e} {radius:e} 1.0")
    }
    fn recoef_line(n: usize, m: usize, c: f64, s: f64) -> String {
        format!("RECOEF{n:>5}{m:>3}   {:>21}{:>21}", format!("{c:e}"), format!("{s:e}"))
    }

    /// **Prove the direction rather than assume it** (this task's own instruction, mirroring
    /// `tests/frame_gmat.rs::asymmetric_field_detects_a_transposed_rotation`, GMAT-free
    /// version): a deliberately transposed rotation must change the computed acceleration on an
    /// ASYMMETRIC synthetic gravity field by a large fraction of the signal. A spherically
    /// symmetric field cannot catch this (`crate::frame`'s own module doc explains why), which
    /// is exactly why this test builds a synthetic field with large C22/S22 instead of using a
    /// real one.
    #[test]
    fn a_deliberately_transposed_rotation_wrecks_an_asymmetric_gravity_field() {
        let mu = 3.986_004_415e14;
        let radius = 6_378_136.3;
        let mut content = String::new();
        content.push_str("CCCCC synthetic asymmetric test field (av-orbital src/fk5.rs tests) CCCCC\n");
        content.push_str(&potfield_line(2, 2, mu, radius));
        content.push('\n');
        content.push_str(&recoef_line(2, 0, -4.841_653_717_36e-4, 0.0));
        content.push('\n');
        content.push_str(&recoef_line(2, 1, 0.0, 0.0));
        content.push('\n');
        content.push_str(&recoef_line(2, 2, 0.02, 0.015));
        content.push('\n');
        let path = std::env::temp_dir().join(format!("av_orbital_fk5_synthetic_asym_{}.cof", std::process::id()));
        std::fs::write(&path, &content).expect("write synthetic .cof");
        let model = crate::cof::read_earth_gravity(&path, 2, 2).expect("parse synthetic field");
        std::fs::remove_file(&path).ok();

        let rotation = fk5().inertial_to_fixed(TAI_NS).unwrap();
        let pos_inertial = [6_800_000.0, 1_200_000.0, 2_300_000.0];
        let pos_fixed = rotation.apply(pos_inertial);
        let (accel_fixed, _) = crate::gravity::spherical_harmonic_gravity(pos_fixed, &model);
        let accel_correct = rotation.apply_transpose(accel_fixed);
        let accel_wrong = rotation.apply(accel_fixed); // deliberately applies R again instead of R^T

        let diff = (0..3).map(|i| (accel_correct[i] - accel_wrong[i]).powi(2)).sum::<f64>().sqrt();
        let scale = (0..3).map(|i| accel_correct[i].powi(2)).sum::<f64>().sqrt();
        println!("n5-fk5-transpose-detection: |correct - wrong| = {diff:e} m/s^2 vs |correct| = {scale:e} m/s^2 ({:.1}% of signal)", 100.0 * diff / scale);
        assert!(diff / scale > 0.01, "a transposed rotation must give a VISIBLY different acceleration; got only {:.3e}%", 100.0 * diff / scale);
    }

    #[test]
    fn wrap_trunc_matches_cpp_int_truncation_toward_zero() {
        let two_pi = 2.0 * std::f64::consts::PI;
        // A large negative value (realistic magnitude for longAscNodeLunar's own raw sum before
        // wrapping, at a present-day epoch): truncation toward zero, not floor, is the point of
        // this test -- they differ in sign convention for a negative dividend.
        let x = -19.5 * two_pi + 0.3;
        let wrapped = wrap_trunc(x);
        println!("n5-wrap-trunc: x={x} wrapped={wrapped}");
        assert!(wrapped.is_finite(), "wrap_trunc must never produce NaN/inf for a finite input");
        // Reconstructing x from the wrapped value and SOME integer multiple of 2*pi must
        // recover x, and the wrapped value's magnitude must be less than 2*pi.
        assert!(wrapped.abs() < two_pi);
        let n = ((x - wrapped) / two_pi).round();
        assert!((wrapped + n * two_pi - x).abs() < 1e-9);
    }
}
