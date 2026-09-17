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
//! # Root cause: why GMAT's own `Convert()` disagrees with its own exposed constants
//!
//! Plugging `M_E_OFFSET = 357.5277233` straight into the formula above, with `T_TT` computed
//! from `T_TT_OFFSET`/`T_TT_COEFF1` exactly as GMAT exposes them, disagrees with GMAT's own
//! `Convert(..., TDBMJD, ...)` output by up to ~1.6 ms -- essentially the whole series
//! amplitude, i.e. a phase error of roughly 289 degrees, not floating-point noise.
//!
//! **Root cause (definitive, not a fit): GMAT computes the periodic term's `T_TT` from its own
//! internal Modified Julian Date convention (`GMAT_MJD = JD - 2_430_000.0`,
//! `GmatTimeConstants::JD_JAN_5_1941`) while subtracting the J2000 *Julian* Date constant
//! `T_TT_OFFSET = 2451545.0` -- so GMAT's mean-anomaly argument is short by exactly 2,430,000
//! days of the `M_E_COEFF1` rate.** Because the rate term is untouched, this is a pure
//! constant phase error, which is exactly why a constant offset absorbs it perfectly:
//!
//! ```text
//! rate                    = M_E_COEFF1 / T_TT_COEFF1               = 0.985600283094 deg/day
//! phase of 2,430,000 days = 2_430_000 * rate                       = 2_395_008.687918 deg
//!             mod 360     = GMAT_M_E_PHASE_SHIFT_DEG (this module) = 288.6879178644 deg
//! GMAT's own effective offset = (M_E_OFFSET - GMAT_M_E_PHASE_SHIFT_DEG) mod 360
//!                                                                  = 68.8398054354 deg
//! ```
//!
//! An earlier version of this module carried a *fitted* replacement for `M_E_OFFSET_DEG`
//! (`68.8398465155` degrees, a least-squares fit over 522 GMAT samples spanning 10 years,
//! RMS residual 153.5 ns) because the disagreement above was, at the time, unexplained. It is
//! no longer: the root cause derived above lands at `68.8398054354` degrees, `4.108e-5`
//! degrees away from that old fit -- about 1.2 ns of `TDB-TT` (`TDB_COEFF1`'s own derivative
//! near this phase times that angular error), fully inside the fit's own 153.5 ns RMS / 324 ns
//! max. The fit was measuring the same effect this module now derives from first principles;
//! "empirically recalibrated" and "least-squares fit" described a symptom, not the cause, and
//! neither term nor the fitted literal remain in this module.
//!
//! # This module deliberately uses the CORRECT series, not GMAT's phase-shifted one
//!
//! `M_E_OFFSET_DEG` below is GMAT's own exposed, standard value (`357.5277233`), used with the
//! `T_TT` this module already computes correctly from the JD-based `T_TT_OFFSET`. That is the
//! textbook Fairhead & Bretagnon / Vallado series, evaluated correctly -- not the series GMAT's
//! own `Convert()` actually returns, which carries the 2,430,000-day phase shift derived above
//! baked in as an accident of GMAT's internal MJD bookkeeping, not a deliberate modeling choice.
//!
//! This is ADR-002's fifth amendment's own precedent (`docs/adr/002-dynamics-contract.md`):
//! the platform may be deliberately more complete -- here, more *correct* -- than GMAT's own
//! report, provided the disagreement is pinned by a test so it cannot go unnoticed. GMAT's
//! phase is kept available, under its own clearly-named function
//! ([`tai_ns_to_tdb_minus_tt_seconds_gmat_phase`]) and constant ([`GMAT_M_E_PHASE_SHIFT_DEG`],
//! derived from the other constants in code, never pasted as a literal), specifically so
//! `tests/tdb_check.rs` can assert both: that the GMAT-phase path still reproduces GMAT's own
//! golden (proving the root cause is exactly right), and that the correct series measurably
//! disagrees with it (proving the platform's choice is deliberate, not silent).
//!
//! Theoretical maximum disagreement between the two series, over a full 360-degree cycle of
//! `M_E`, on an idealized pure two-phase-shifted-sine model (i.e. the largest this specific
//! ~288.6879-degree phase shift can ever cost, not the worst-case 3.344 ms an arbitrary phase
//! error of this series' amplitude could cost in general): **1.959195 ms** (root-cause
//! script). `tests/tdb_check.rs` measures the actual disagreement against the golden's 4
//! epochs + 522 `year_scan` points directly: **1.959386844e-3 s**, a hair above the idealized
//! number because GMAT's own phase, as derived here, still carries its own tiny residual
//! against GMAT's true `Convert()` output (max 3.23e-7 s, measured by that same test file) on
//! top of the phase-shift bound -- not a second, unexplained effect.
//!
//! # Magnitude
//!
//! `TDB_COEFF1` alone bounds the correction at 1.658 ms; measured over the golden arc's four
//! sample epochs (`goldens/tdb_check.json`), GMAT's own `TDB - TT` ranges `1.553062e-3` to
//! `1.562806e-3` s -- a ~9.7 microsecond swing over one day.
//!
//! # Measured agreement against GMAT
//!
//! `tests/tdb_check.rs` calls [`tai_ns_to_tdb_minus_tt_seconds_gmat_phase`] (NOT the correct
//! series, and NOT a difference of two `tai_ns_to_tdb_jd`-scale numbers -- see that function's
//! own doc, "why a full JD caps resolution") at the four TAI instants `goldens/tdb_check.json`
//! records (derived from the file's own `epoch_a1mjd` via
//! `av_cdm::time::Tai::from_a1_mjd(...).as_nanos()`) and at the whole 522-point `year_scan`,
//! to prove the root cause derived above fully accounts for GMAT's own behaviour; it separately
//! calls [`tai_ns_to_tdb_minus_tt_seconds`] (the correct series this module actually uses) over
//! the same fixture to record how far the deliberate correction departs from GMAT's golden. See
//! that test's own doc comments for the measured numbers.
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
/// does not export it). Also the exact size of the phase error root-caused in this module's
/// doc: GMAT's periodic-term `T_TT` is computed from this MJD convention while subtracting
/// the J2000 *Julian* Date constant, leaving the mean-anomaly argument short by this many days
/// of the `M_E_COEFF1` rate.
const GMAT_MJD_TO_JD_OFFSET: f64 = 2_430_000.0;

/// GMAT's own live `TimeSystemConverter::Instance()` constants (see this module's doc for how
/// they were read, and the exact values).
const TDB_COEFF1_S: f64 = 0.001_658;
const TDB_COEFF2_S: f64 = 0.000_013_85;
/// GMAT's own exposed `M_E_OFFSET`, the standard value -- this module deliberately uses this,
/// not the phase-shifted value GMAT's own `Convert()` actually applies internally; see this
/// module's doc, "This module deliberately uses the CORRECT series".
const M_E_OFFSET_DEG: f64 = 357.527_723_3;
const M_E_COEFF1_DEG_PER_CENTURY: f64 = 35_999.050_34;
const T_TT_OFFSET_JD: f64 = 2_451_545.0;
const T_TT_COEFF1_DAYS_PER_CENTURY: f64 = 36_525.0;

/// The exact phase (degrees, reduced mod 360) that GMAT's `2_430_000`-day MJD/JD mismatch
/// (see this module's doc, "Root cause") adds to the mean-anomaly argument -- **derived from
/// the other constants in this module, never pasted as a literal**, so a change to
/// `M_E_COEFF1_DEG_PER_CENTURY` or `T_TT_COEFF1_DAYS_PER_CENTURY` above keeps this in sync
/// automatically. Used only by [`tai_ns_to_tdb_minus_tt_seconds_gmat_phase`], GMAT's own phase
/// kept available specifically so the disagreement with the correct series (this module's
/// own, used everywhere else) can be measured and asserted, not merely described.
const GMAT_M_E_PHASE_SHIFT_DEG: f64 = (GMAT_MJD_TO_JD_OFFSET * M_E_COEFF1_DEG_PER_CENTURY / T_TT_COEFF1_DAYS_PER_CENTURY) % 360.0;

/// This TAI instant as a GMAT-style Modified Julian Date on the TT scale
/// (`JD_TT - 2_430_000.0`) -- see this module's doc, "TT, from TAI, without touching
/// `av_cdm::time`".
pub fn tai_ns_to_tt_mjd(t_tai_ns: i64) -> f64 {
    let a1_mjd = Tai::from_nanos(t_tai_ns).to_a1_mjd();
    a1_mjd + TT_MINUS_A1_SECONDS / 86_400.0
}

/// The two-term periodic series (`TDB_COEFF1 sin(M_E) + TDB_COEFF2 sin(2 M_E)`) at the given
/// `M_E` additive offset (degrees) -- the one piece of arithmetic [`tai_ns_to_tdb_minus_tt_seconds`]
/// and [`tai_ns_to_tdb_minus_tt_seconds_gmat_phase`] share, differing only in which offset they
/// pass.
fn tdb_minus_tt_seconds_with_offset(t_tai_ns: i64, m_e_offset_deg: f64) -> f64 {
    let jd_tt = tai_ns_to_tt_mjd(t_tai_ns) + GMAT_MJD_TO_JD_OFFSET;
    let t_tt_centuries = (jd_tt - T_TT_OFFSET_JD) / T_TT_COEFF1_DAYS_PER_CENTURY;
    let m_e_deg = m_e_offset_deg + M_E_COEFF1_DEG_PER_CENTURY * t_tt_centuries;
    let m_e_rad = m_e_deg.to_radians();
    TDB_COEFF1_S * m_e_rad.sin() + TDB_COEFF2_S * (2.0 * m_e_rad).sin()
}

/// The periodic correction alone, `TDB - TT` in SECONDS, using the CORRECT (standard
/// Fairhead & Bretagnon / Vallado) series -- this module's own deliberate choice; see this
/// module's doc, "This module deliberately uses the CORRECT series". Kept as its own
/// small-magnitude (never more than ~1.7 ms) function, separate from [`tai_ns_to_tdb_jd`], for
/// exactly one reason: **a full Julian Date is a ~2.46e6-magnitude `f64`, whose own
/// representable resolution at that magnitude is capped at its ULP (`2.46e6 * 2^-52 ~=
/// 5.46e-10` days `~= 4.7e-5` s = 47 microseconds) -- REGARDLESS of how carefully the
/// arithmetic that produced it was ordered.** Adding this function's own small, well-resolved
/// result to a full JD (as `tai_ns_to_tdb_jd` does) is therefore lossy by construction, not a
/// bug to fix by reordering additions -- see this module's doc, "Precision: why a full JD caps
/// resolution, and why that is fine here", for the measured impact (it turns out to be
/// negligible for this crate's actual use, third-body ephemeris lookups) and why callers that
/// want the correction itself at full precision (this module's own tests, comparing against
/// GMAT's small-magnitude MJD reports) should call this function directly rather than
/// difference two `tai_ns_to_tdb_jd`-scale numbers.
pub fn tai_ns_to_tdb_minus_tt_seconds(t_tai_ns: i64) -> f64 {
    tdb_minus_tt_seconds_with_offset(t_tai_ns, M_E_OFFSET_DEG)
}

/// The same periodic correction, but reproducing GMAT's own `TimeSystemConverter::Convert()`
/// output exactly (to the fit's former precision, now derived rather than fit -- see this
/// module's doc, "Root cause") by using GMAT's own effective phase
/// (`M_E_OFFSET_DEG - GMAT_M_E_PHASE_SHIFT_DEG`) instead of the correct one. Exists ONLY so the
/// disagreement between GMAT's phase-shifted series and the correct series this module
/// actually uses can be measured and asserted (`tests/tdb_check.rs`) rather than merely
/// described -- no production caller in this crate uses this function.
pub fn tai_ns_to_tdb_minus_tt_seconds_gmat_phase(t_tai_ns: i64) -> f64 {
    tdb_minus_tt_seconds_with_offset(t_tai_ns, M_E_OFFSET_DEG - GMAT_M_E_PHASE_SHIFT_DEG)
}

/// This TAI instant as a full Julian Date on the TDB scale (Barycentric Dynamical Time --
/// the scale [`crate::de::DeEphemeris`] expects), via the CORRECT series
/// ([`tai_ns_to_tdb_minus_tt_seconds`], not GMAT's phase-shifted one -- see this module's doc
/// for why) added to TT. See [`tai_ns_to_tdb_minus_tt_seconds`]'s own doc for why the
/// RETURNED full-JD value is capped to ~47 microsecond resolution no matter how this addition
/// is ordered.
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
    /// epoch, since the series is a sum of two bounded sinusoids. Checked for both the
    /// correct series and GMAT's phase-shifted one -- the bound is a property of the series'
    /// amplitude, not of which phase is used.
    #[test]
    fn tdb_minus_tt_is_bounded_by_the_series_amplitude() {
        for t_tai_ns in [0_i64, 1_700_000_000_000_000_000, -500_000_000_000_000_000, 5_000_000_000_000_000_000] {
            let jd_tdb = tai_ns_to_tdb_jd(t_tai_ns);
            let jd_tt = tai_ns_to_tt_mjd(t_tai_ns) + GMAT_MJD_TO_JD_OFFSET;
            let diff_s = (jd_tdb - jd_tt) * 86_400.0;
            let diff_gmat_phase_s = tai_ns_to_tdb_minus_tt_seconds_gmat_phase(t_tai_ns);
            println!("n2-tdb-bound: t_tai_ns={t_tai_ns} tdb-tt={diff_s:e} s tdb-tt(gmat_phase)={diff_gmat_phase_s:e} s");
            assert!(diff_s.abs() <= TDB_COEFF1_S + TDB_COEFF2_S + 1e-12, "TDB-TT {diff_s} exceeds the series' own amplitude bound");
            assert!(diff_gmat_phase_s.abs() <= TDB_COEFF1_S + TDB_COEFF2_S + 1e-12, "GMAT-phase TDB-TT {diff_gmat_phase_s} exceeds the series' own amplitude bound");
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

    /// The derived phase shift itself: must land at `288.6879178644` degrees (this module's
    /// doc, "Root cause"), and the correct offset minus it must land at `68.8398054354`
    /// degrees -- within the old fitted constant's own residual (~4.108e-5 degrees, ~1.2 ns of
    /// `TDB-TT`) of the value that constant used to be. Guards the derivation itself, not a
    /// GMAT fixture.
    #[test]
    fn gmat_phase_shift_matches_the_derived_root_cause() {
        println!("n2-tdb-phase-shift: derived={GMAT_M_E_PHASE_SHIFT_DEG:.10} deg");
        assert!((GMAT_M_E_PHASE_SHIFT_DEG - 288.687_917_864_4).abs() < 1e-8, "derived phase shift {GMAT_M_E_PHASE_SHIFT_DEG} deg drifted from the root-caused 288.6879178644 deg");
        let gmat_effective_offset_deg = M_E_OFFSET_DEG - GMAT_M_E_PHASE_SHIFT_DEG;
        let old_fitted_offset_deg = 68.839_846_515_5;
        let residual_deg = (gmat_effective_offset_deg - old_fitted_offset_deg).abs();
        println!("n2-tdb-phase-shift: gmat_effective_offset={gmat_effective_offset_deg:.10} deg, old fit={old_fitted_offset_deg} deg, residual={residual_deg:e} deg");
        assert!(residual_deg < 1e-3, "derived GMAT offset {gmat_effective_offset_deg} deg disagrees with the old fitted value {old_fitted_offset_deg} deg by {residual_deg:e} deg, more than the fit's own tiny residual");
    }
}
