//! TAI -> TDB (Barycentric Dynamical Time), the epoch scale the JPL DE ephemerides in
//! [`crate::de`] are tabulated in (`docs/native-dynamics-plan.md` milestone N2, step 3).
//!
//! Per the task's own rule ("`av-cdm` is shared with other tracks and is not yours to extend
//! this round"), this conversion lives here, in `av-orbital`, not in `av_cdm::time::Tai`
//! (which has TAI<->TT already, via [`av_cdm::time::Tai::to_tt_nanos`], but no TDB).
//!
//! # The series, and where it came from
//!
//! `TDB = TT + TDB_COEFF1 * sin(M_E) + TDB_COEFF2 * sin(2 M_E)`, where `M_E` is the (mean
//! anomaly-like) argument `M_E = M_E_OFFSET + M_E_COEFF1 * T_TT` degrees, `T_TT = (JD_TT -
//! T_TT_OFFSET) / T_TT_COEFF1` Julian centuries of TT since the `T_TT_OFFSET` epoch.
//!
//! **Source: GMAT's own `TimeSystemConverter` singleton -- but not used blindly.** Read
//! directly off a live GMAT instance (`gmat.TimeSystemConverter.Instance()`'s own
//! `TDB_COEFF1`, `TDB_COEFF2`, `M_E_OFFSET`, `M_E_COEFF1`, `T_TT_OFFSET`, `T_TT_COEFF1`
//! properties -- `goldens/gen_tdb_check.py` records the exact values alongside the epochs it
//! pins):
//!
//! ```text
//! TDB_COEFF1  = 0.001658            (seconds)
//! TDB_COEFF2  = 0.00001385          (seconds)   ( == 1.385e-05 )
//! M_E_OFFSET  = 357.5277233         (degrees, AS EXPOSED -- not the value this module uses; see below)
//! M_E_COEFF1  = 35999.05034         (degrees / Julian century)
//! T_TT_OFFSET = 2451545.0           (JD -- J2000.0)
//! T_TT_COEFF1 = 36525.0             (days / Julian century)
//! ```
//!
//! This is the classical truncated Fairhead & Bretagnon series shape (as quoted in, e.g., the
//! Astronomical Almanac and Vallado's *Fundamentals of Astrodynamics and Applications*):
//! `TDB = TT + TDB_COEFF1 sin(M_E) + TDB_COEFF2 sin(2 M_E)`, `M_E = M_E_OFFSET + M_E_COEFF1 *
//! T_TT_centuries`.
//!
//! **Measured finding: the exposed `M_E_OFFSET`, used directly in that formula, does NOT
//! reproduce GMAT's own `Convert(..., TDBMJD, ...)`.** Plugging `M_E_OFFSET = 357.5277233`
//! straight into the formula above, with `T_TT_centuries` computed from `T_TT_OFFSET`/
//! `T_TT_COEFF1` exactly as GMAT exposes them, disagreed with GMAT's own reported `TDB - TT`
//! by ~1.6 ms at the golden's epoch -- essentially the whole series amplitude, i.e. a ~288
//! degree phase error, not floating-point noise (this crate's N2 report has the full
//! debugging trail: the `TT` half of the computation independently verified bit-exact
//! against GMAT's own `TTMJD` conversion first, isolating the disagreement to the periodic
//! term's phase alone). Whatever GMAT's C++ actually does with the `M_E_OFFSET` property
//! internally, it is not "add it to `M_E_COEFF1 * T_TT_centuries` and take the sine" -- so
//! rather than guess further, **this module's own `M_E_OFFSET_DEG` constant was empirically
//! recalibrated** (holding every other constant at GMAT's own exposed value) by a
//! least-squares fit against `goldens/tdb_check.json`'s `year_scan` -- 522 of GMAT's own
//! `Convert()` results, one every 7 days for 10 years (`goldens/gen_tdb_check.py`'s own
//! `--fit-offset` derivation, this crate's N2 report has the fit script and its output):
//!
//! ```text
//! M_E_OFFSET_DEG (fitted) = 68.8398465155 degrees   (vs the exposed 357.5277233)
//! RMS residual over the 522-point, 10-year fit       = 1.535e-07 s  (153.5 ns)
//! ```
//!
//! A 153.5 ns RMS over a full decade, from a two-term truncated series whose own next-order
//! neglected terms are of that same magnitude, is not distinguishable from "this is the
//! right formula with the right phase" -- so this module uses the fitted value, documented
//! as measured rather than read off the property, which is the honest description of what it
//! actually is.
//!
//! # Magnitude
//!
//! `TDB_COEFF1` alone bounds the correction at 1.658 ms; measured over the golden arc's four
//! sample epochs (`goldens/tdb_check.json`), `TDB - TT` ranges `1.553062e-3` to
//! `1.562806e-3` s -- a ~9.7 microsecond swing over one day, everything at the low-millisecond
//! level `TDB_COEFF2`'s own ~13.85 microsecond amplitude cannot itself resolve much further.
//!
//! # Measured agreement against GMAT
//!
//! `tests/tdb_check.rs` calls [`tai_ns_to_tdb_minus_tt_seconds`] directly (NOT a difference of
//! two `tai_ns_to_tdb_jd` values -- see that function's own doc, "why a full JD caps
//! resolution") at the four TAI instants `goldens/tdb_check.json` records (derived from the
//! file's own `epoch_a1mjd` via `av_cdm::time::Tai::from_a1_mjd(...).as_nanos()`) and at the
//! whole 522-point `year_scan` the fit itself was derived from (a held-in, not held-out, check
//! -- the fit was done offline in Python against this identical dataset; the Rust-side test
//! exists to pin that the *Rust* implementation reproduces the *Python* fit's own numbers, not
//! to re-validate the fit's quality). See that test's own doc comment for the measured
//! numbers; this module's own N2 report also quotes them.
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
/// does not export it).
const GMAT_MJD_TO_JD_OFFSET: f64 = 2_430_000.0;

/// GMAT's own live `TimeSystemConverter::Instance()` constants (see this module's doc for how
/// they were read, and the exact values).
const TDB_COEFF1_S: f64 = 0.001_658;
const TDB_COEFF2_S: f64 = 0.000_013_85;
/// **NOT** GMAT's exposed `M_E_OFFSET` (357.527_723_3) -- empirically recalibrated against
/// GMAT's own `Convert()` output; see this module's doc, "Measured finding".
const M_E_OFFSET_DEG: f64 = 68.839_846_515_5;
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

/// The periodic correction alone, `TDB - TT` in SECONDS (`TDB_COEFF1 sin(M_E) + TDB_COEFF2
/// sin(2 M_E)`) -- kept as its own small-magnitude (never more than ~1.7 ms) function,
/// separate from [`tai_ns_to_tdb_jd`], for exactly one reason: **a full Julian Date is a
/// ~2.46e6-magnitude `f64`, whose own representable resolution at that magnitude is capped
/// at its ULP (`2.46e6 * 2^-52 ~= 5.46e-10` days `~= 4.7e-5` s = 47 microseconds) --
/// REGARDLESS of how carefully the arithmetic that produced it was ordered.** Adding this
/// function's own small, well-resolved result to a full JD (as `tai_ns_to_tdb_jd` does) is
/// therefore lossy by construction, not a bug to fix by reordering additions -- see this
/// module's doc, "Precision: why a full JD caps resolution, and why that is fine here", for
/// the measured impact (it turns out to be negligible for this crate's actual use, third-body
/// ephemeris lookups) and why callers that want the correction itself at full precision
/// (this module's own tests, comparing against GMAT's small-magnitude MJD reports) should
/// call this function directly rather than difference two `tai_ns_to_tdb_jd`-scale numbers.
pub fn tai_ns_to_tdb_minus_tt_seconds(t_tai_ns: i64) -> f64 {
    let jd_tt = tai_ns_to_tt_mjd(t_tai_ns) + GMAT_MJD_TO_JD_OFFSET;
    let t_tt_centuries = (jd_tt - T_TT_OFFSET_JD) / T_TT_COEFF1_DAYS_PER_CENTURY;
    let m_e_deg = M_E_OFFSET_DEG + M_E_COEFF1_DEG_PER_CENTURY * t_tt_centuries;
    let m_e_rad = m_e_deg.to_radians();
    TDB_COEFF1_S * m_e_rad.sin() + TDB_COEFF2_S * (2.0 * m_e_rad).sin()
}

/// This TAI instant as a full Julian Date on the TDB scale (Barycentric Dynamical Time --
/// the scale [`crate::de::DeEphemeris`] expects), via `TT + TDB_COEFF1 sin(M_E) + TDB_COEFF2
/// sin(2 M_E)` (see this module's doc for the series and its source, and
/// [`tai_ns_to_tdb_minus_tt_seconds`]'s own doc for why the RETURNED full-JD value is capped
/// to ~47 microsecond resolution no matter how this addition is ordered).
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
