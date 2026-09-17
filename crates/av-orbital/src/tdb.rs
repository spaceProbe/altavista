//! TAI -> TDB (Barycentric Dynamical Time), the epoch scale the JPL DE ephemerides in
//! [`crate::de`] are tabulated in (`docs/native-dynamics-plan.md` milestone N2, step 3).
//!
//! Per the task's own rule ("`av-cdm` is shared with other tracks and is not yours to extend
//! this round"), this conversion lives here, in `av-orbital`, not in `av_cdm::time::Tai`
//! (which has TAI<->TT already, via [`av_cdm::time::Tai::to_tt_nanos`], but no TDB).
//!
//! # The series
//!
//! `TDB = TT + TDB_COEFF1 * sin(M_E) + TDB_COEFF2 * sin(2 M_E)`, where `M_E` is the (mean
//! anomaly-like) argument `M_E = M_E_OFFSET_DEG + M_E_COEFF1 * T_TT` degrees, `T_TT = (JD_TT -
//! T_TT_OFFSET) / T_TT_COEFF1` Julian centuries of TT since the `T_TT_OFFSET` epoch. This is
//! the classical truncated Fairhead & Bretagnon series shape (as quoted in, e.g., the
//! Astronomical Almanac and Vallado's *Fundamentals of Astrodynamics and Applications*).
//!
//! **Source of the coefficients: GMAT's own `TimeSystemConverter` singleton**, read directly
//! off a live GMAT instance (`gmat.TimeSystemConverter.Instance()`'s own `TDB_COEFF1`,
//! `TDB_COEFF2`, `M_E_OFFSET`, `M_E_COEFF1`, `T_TT_OFFSET`, `T_TT_COEFF1` properties --
//! `goldens/gen_tdb_check.py` records the exact values alongside the epochs it pins):
//!
//! ```text
//! TDB_COEFF1  = 0.001658            (seconds)
//! TDB_COEFF2  = 0.00001385          (seconds)   ( == 1.385e-05 )
//! M_E_OFFSET  = 357.5277233         (degrees)
//! M_E_COEFF1  = 35999.05034         (degrees / Julian century)
//! T_TT_OFFSET = 2451545.0           (JD -- J2000.0)
//! T_TT_COEFF1 = 36525.0             (days / Julian century)
//! ```
//!
//! # This module reproduces GMAT's own `TimeSystemConverter` output
//!
//! `M_E_OFFSET_DEG` below is GMAT's own exposed, standard value (`357.5277233`), used with the
//! `T_TT` this module computes from the JD-based `T_TT_OFFSET` -- the textbook Fairhead &
//! Bretagnon / Vallado series, evaluated as GMAT itself evaluates it. `tests/tdb_check.rs`
//! measures this module's output against `goldens/tdb_check.json` (regenerated GMAT
//! `TimeSystemConverter::Convert()` output, `refJd=2_430_000.0`) at the golden's 4 epochs and
//! its 522-point `year_scan`; see that test file's own doc comments for the measured RMS and
//! max, and the golden's own `tolerance_s` field for the tolerance in force.
//!
//! What is left of the disagreement is the GOLDEN'S resolution, not a model difference, and
//! that was root-caused rather than described: the golden records `tdb_minus_tt_s` as the
//! difference of two MJD-magnitude doubles GMAT returns, whose own ULP is `2^-38` days
//! (3.1432e-7 s) below the 32768-day binade and twice that above it, and **every one of the
//! 526 fixture residuals is at most 0.531 of its own epoch's ULP** -- a single rounding of a
//! quantity that is otherwise exact, asserted as such by that test. The one genuine difference
//! between this module and GMAT is smaller still: GMAT forms the series argument from the TAI
//! modified Julian date (`ConvertFromTaiMjd`'s `origValue`) and this module from the TT one,
//! worth at most **10.80 ns** of `TDB - TT` (computed directly from `TT - A.1` and the
//! `M_E_COEFF1` rate, not fitted). TT is the argument the series is defined on, so this module
//! keeps it.
//!
//! # An earlier revision believed GMAT's converter carried a phase error -- it did not
//!
//! An earlier revision of this module carried a "Root cause" narrative claiming GMAT's own
//! `Convert(..., TDBMJD, ...)` computes the periodic term's `T_TT` from its own internal
//! Modified Julian Date convention while subtracting the J2000 *Julian* Date constant,
//! producing a ~288.6879-degree phase error, and kept a second function
//! (`tai_ns_to_tdb_minus_tt_seconds_gmat_phase`) and constant (`GMAT_M_E_PHASE_SHIFT_DEG`) that
//! reproduced that phase-shifted series so the "disagreement" could be asserted by a test.
//!
//! That was wrong, and it was our own bug, not GMAT's: `docs/reports/gmat-tdb-phase/REPORT.md`
//! reproduces `TimeSystemConverter::Convert()` directly against a live GMAT (`Sat.TDBModJulian`
//! read from a plain GMAT script, and GMAT R2026a's own source at `third_party/gmat-src`) and
//! finds that the 288.6879178644-degree phase shift is produced only by calling
//! `Convert(origValue, fromType, toType, refJd=0.0)` -- not `Convert()`'s own default
//! (`GmatTimeConstants::JD_JAN_5_1941` = 2,430,000.0) and not anything any call site inside
//! GMAT's own R2026a source passes. `goldens/gen_tdb_check.py`'s previous revision passed that
//! `0.0` explicitly; this module's "Root cause" section, the `_gmat_phase` function and
//! constant it kept alive, and the golden and test built on that call, all encoded the same
//! artifact. `tai_ns_to_tdb_jd`, the function this crate's production code actually calls, was
//! never affected: it always used GMAT's own exposed `M_E_OFFSET` with a correctly-formed
//! `T_TT` (see below), the same computation GMAT's own `Sat.TDBModJulian` and
//! `DeFile::GetPosVel()` perform.
//!
//! # Magnitude
//!
//! `TDB_COEFF1` alone bounds the correction at 1.658 ms; measured over the golden arc's four
//! sample epochs (`goldens/tdb_check.json`), GMAT's own `TDB - TT` ranges from about
//! `-7.98e-5` to `-5.09e-5` s over that one day -- see that file for the exact recorded values.
//!
//! # Precision: why a full JD caps resolution, and why that is fine here
//!
//! [`tai_ns_to_tdb_jd`] returns a ~2.46e6-magnitude `f64`. At that magnitude, `f64`'s own
//! representable resolution (its ULP, `2.46e6 * 2^-52`) is ~5.46e-10 days = **~47
//! microseconds** -- a hard ceiling on how precisely the periodic correction (never larger
//! than 1.7 ms) can actually be RECOVERED from the returned value, no matter how carefully
//! the addition that produced it was ordered (this was discovered, not assumed: an early
//! version of `tests/tdb_check.rs` differenced two `tai_ns_to_tdb_jd`-scale numbers directly
//! and measured exactly this ~10-30 microsecond noise floor against GMAT's own MJD-scale
//! report, which has ~500x better resolution at its own, ~31,000-magnitude, scale). Measured
//! impact on this crate's actual use ([`crate::de::DeEphemeris::geocentric_position_km`]'s
//! `jd_tdb` argument): the Moon moves at ~1 km/s, so a 47-microsecond epoch uncertainty is a
//! ~5 cm position uncertainty; propagated through the third-body acceleration formula's own
//! sensitivity (`d(accel)/d(distance) ~ 2*accel/distance`, `accel ~ 3e-6` m/s^2 at lunar
//! distance `~3.844e8` m), a 5 cm position error contributes on the order of `1e-16` m/s^2 of
//! acceleration error -- seven orders of magnitude below this crate's own N2 acceleration
//! tolerance. So the ceiling is real, measured, and does not matter for what this module is
//! actually used for; [`tai_ns_to_tdb_minus_tt_seconds`] exists as the escape hatch for a
//! caller (this module's own tests) that needs the correction at its own, much better,
//! resolution.
//!
//! # TT, from TAI, without touching `av_cdm::time`
//!
//! `av_cdm::time::Tai` has `to_tt_nanos` (`TT = TAI + 32.184 s` exactly) but its own
//! TAI<->A.1 Julian-date conversion (`to_a1_mjd`) is the only public entry point into GMAT's
//! Modified Julian Date convention (`GMAT_MJD = JD - 2_430_000.0`, `av_cdm::time`'s own
//! documented constant). This module reaches a TT-scale MJD from `to_a1_mjd` by applying the
//! **fixed** `TT - A.1` offset directly (`32.184 - 0.034_381_7` seconds -- both exact
//! constants, `av_cdm::time`'s own doc names them), rather than re-deriving the
//! Unix-epoch-relative MJD constant a second time in this crate.
use av_cdm::time::Tai;

/// `TT - A.1`, seconds, exact (both are fixed offsets from TAI: `TT = TAI + 32.184 s`,
/// `A1 = TAI + 0.034_381_7 s`, `av_cdm::time`'s own documented constants).
const TT_MINUS_A1_SECONDS: f64 = 32.184 - 0.034_381_7;

/// GMAT's own `GMAT_MJD = JD - 2_430_000.0` convention (`av_cdm::time`'s own doc comment
/// names the identical constant; restated here rather than imported because `av_cdm::time`
/// does not export it), and also `TimeSystemConverter::Convert`'s own default `refJd`
/// (`GmatTimeConstants::JD_JAN_5_1941`, see this module's doc).
const GMAT_MJD_TO_JD_OFFSET: f64 = 2_430_000.0;

/// GMAT's own live `TimeSystemConverter::Instance()` constants (see this module's doc for how
/// they were read, and the exact values).
const TDB_COEFF1_S: f64 = 0.001_658;
const TDB_COEFF2_S: f64 = 0.000_013_85;
/// GMAT's own exposed `M_E_OFFSET`, the standard value; see this module's doc, "This module
/// reproduces GMAT's own `TimeSystemConverter` output".
const M_E_OFFSET_DEG: f64 = 357.527_723_3;
const M_E_COEFF1_DEG_PER_CENTURY: f64 = 35_999.050_34;
const T_TT_OFFSET_JD: f64 = 2_451_545.0;
const T_TT_COEFF1_DAYS_PER_CENTURY: f64 = 36_525.0;

/// This TAI instant as a GMAT-style Modified Julian Date on the TT scale
/// (`JD_TT - 2_430_000.0`) -- see this module's doc, "TT, from TAI, without touching
/// `av_cdm::time`".
pub fn tai_ns_to_tt_mjd(t_tai_ns: i64) -> f64 {
    let a1_mjd = Tai::from_nanos(t_tai_ns).to_a1_mjd();
    a1_mjd + TT_MINUS_A1_SECONDS / 86_400.0
}

/// The periodic correction alone, `TDB - TT` in SECONDS -- the two-term periodic series
/// (`TDB_COEFF1 sin(M_E) + TDB_COEFF2 sin(2 M_E)`) using GMAT's own `M_E_OFFSET` and a
/// correctly-formed JD-based `T_TT` -- see this module's doc, "This module reproduces GMAT's
/// own `TimeSystemConverter` output". Kept as its own small-magnitude (never more than ~1.7
/// ms) function, separate from [`tai_ns_to_tdb_jd`], for exactly one reason: **a full Julian
/// Date is a ~2.46e6-magnitude `f64`, whose own representable resolution at that magnitude is
/// capped at its ULP (`2.46e6 * 2^-52 ~= 5.46e-10` days `~= 4.7e-5` s = 47 microseconds) --
/// REGARDLESS of how carefully the arithmetic that produced it was ordered.** Adding this
/// function's own small, well-resolved result to a full JD (as `tai_ns_to_tdb_jd` does) is
/// therefore lossy by construction, not a bug to fix by reordering additions -- see this
/// module's doc, "Precision: why a full JD caps resolution, and why that is fine here", for
/// the measured impact (it turns out to be negligible for this crate's actual use, third-body
/// ephemeris lookups) and why callers that want the correction itself at full precision (this
/// module's own tests, comparing against GMAT's small-magnitude MJD reports) should call this
/// function directly rather than difference two `tai_ns_to_tdb_jd`-scale numbers.
pub fn tai_ns_to_tdb_minus_tt_seconds(t_tai_ns: i64) -> f64 {
    let jd_tt = tai_ns_to_tt_mjd(t_tai_ns) + GMAT_MJD_TO_JD_OFFSET;
    let t_tt_centuries = (jd_tt - T_TT_OFFSET_JD) / T_TT_COEFF1_DAYS_PER_CENTURY;
    let m_e_deg = M_E_OFFSET_DEG + M_E_COEFF1_DEG_PER_CENTURY * t_tt_centuries;
    let m_e_rad = m_e_deg.to_radians();
    TDB_COEFF1_S * m_e_rad.sin() + TDB_COEFF2_S * (2.0 * m_e_rad).sin()
}

/// This TAI instant as a full Julian Date on the TDB scale (Barycentric Dynamical Time --
/// the scale [`crate::de::DeEphemeris`] expects), via [`tai_ns_to_tdb_minus_tt_seconds`] added
/// to TT. See that function's own doc for why the RETURNED full-JD value is capped to ~47
/// microsecond resolution no matter how this addition is ordered.
pub fn tai_ns_to_tdb_jd(t_tai_ns: i64) -> f64 {
    let jd_tt = tai_ns_to_tt_mjd(t_tai_ns) + GMAT_MJD_TO_JD_OFFSET;
    let tdb_minus_tt_s = tai_ns_to_tdb_minus_tt_seconds(t_tai_ns);
    jd_tt + tdb_minus_tt_s / 86_400.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The periodic term's own bound: `TDB - TT` must never exceed `TDB_COEFF1 +
    /// TDB_COEFF2` in magnitude (this module's own doc: "Magnitude" -- ~1.67 ms), for any
    /// epoch, since the series is a sum of two bounded sinusoids.
    #[test]
    fn tdb_minus_tt_is_bounded_by_the_series_amplitude() {
        for t_tai_ns in [0_i64, 1_700_000_000_000_000_000, -500_000_000_000_000_000, 5_000_000_000_000_000_000] {
            let jd_tdb = tai_ns_to_tdb_jd(t_tai_ns);
            let jd_tt = tai_ns_to_tt_mjd(t_tai_ns) + GMAT_MJD_TO_JD_OFFSET;
            let diff_s = (jd_tdb - jd_tt) * 86_400.0;
            println!("n2-tdb-bound: t_tai_ns={t_tai_ns} tdb-tt={diff_s:e} s");
            assert!(diff_s.abs() <= TDB_COEFF1_S + TDB_COEFF2_S + 1e-12, "TDB-TT {diff_s} exceeds the series' own amplitude bound");
        }
    }

    /// `tai_ns_to_tt_mjd` must be exactly `to_a1_mjd() + TT_MINUS_A1_SECONDS/86400` -- a
    /// direct restatement of the function's own body, guarding only against the constant
    /// `TT_MINUS_A1_SECONDS` silently drifting from `32.184 - 0.034_381_7` under a future
    /// edit (independent of any GMAT fixture).
    #[test]
    fn tt_offset_matches_the_fixed_tt_minus_a1_constant() {
        let t_tai_ns = 1_700_000_000_123_456_789_i64;
        assert!((TT_MINUS_A1_SECONDS - 32.149_618_3).abs() < 1e-9, "TT-A1 must be 32.184 - 0.0343817 = 32.1496183 s exactly");
        let expected_tt_mjd = Tai::from_nanos(t_tai_ns).to_a1_mjd() + TT_MINUS_A1_SECONDS / 86_400.0;
        assert_eq!(tai_ns_to_tt_mjd(t_tai_ns), expected_tt_mjd);
    }

    /// `M_E` (the series' own argument) must vary continuously and monotonically with time
    /// over a short span (no wraparound bug); a basic sanity check independent of any GMAT
    /// fixture.
    #[test]
    fn tdb_varies_smoothly_over_a_day() {
        let t0 = 1_700_000_000_000_000_000_i64;
        let day_ns = 86_400_000_000_000_i64;
        let jd0 = tai_ns_to_tdb_jd(t0);
        let jd1 = tai_ns_to_tdb_jd(t0 + day_ns);
        let delta_days = jd1 - jd0;
        println!("n2-tdb-day-delta: {delta_days} days for a 1-day TAI step");
        // Must be within a few ms of exactly 1.0 day (the periodic term's own derivative is
        // tiny -- see the amplitude bound above).
        assert!((delta_days - 1.0).abs() < 1e-4);
    }
}
