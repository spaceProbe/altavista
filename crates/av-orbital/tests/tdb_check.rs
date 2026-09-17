//! Measures [`av_orbital::tdb`] against GMAT's own `TimeSystemConverter`
//! (`docs/native-dynamics-plan.md` milestone N2, step 3), via the fixture
//! `goldens/tdb_check.json` (`goldens/gen_tdb_check.py`). No `gmat-frames` feature gate: this
//! test reads a committed JSON fixture, not a live GMAT instance, so it runs in the
//! `--no-default-features` build too (`av_orbital::tdb` has no GMAT dependency).
//!
//! The golden was regenerated with `refJd=2_430_000.0` (`TimeSystemConverter::Convert`'s own
//! default, `GmatTimeConstants::JD_JAN_5_1941`) after `docs/reports/gmat-tdb-phase/REPORT.md`
//! found that the previous revision's `refJd=0.0` call -- not GMAT -- produced a
//! ~288.6879-degree phase error in the old golden (see that report, and `crate::tdb`'s own
//! module doc, "An earlier revision believed GMAT's converter carried a phase error"). Per
//! that finding, **nothing in this file may assert a disagreement with GMAT** -- the old
//! disagreement it used to assert was itself the artifact.
use std::path::PathBuf;

use av_cdm::time::Tai;
use av_orbital::tdb::tai_ns_to_tdb_minus_tt_seconds;
use serde::Deserialize;

#[derive(Deserialize)]
struct TdbCheck {
    /// The tolerance (seconds) this file asserts against, measured and recorded by
    /// `goldens/gen_tdb_check.py --tolerance-s` at regeneration time -- round 1's decision 3:
    /// the tolerance committed with a golden is the tolerance in force, read from the file,
    /// never a copy kept in this test.
    tolerance_s: f64,
    epochs: Vec<EpochEntry>,
    year_scan: Vec<YearScanEntry>,
}

#[derive(Deserialize)]
struct EpochEntry {
    epoch_a1mjd: f64,
    tai_mjd: f64,
    tt_mjd: f64,
    tdb_minus_tt_s: f64,
    fraction_of_arc: f64,
}

#[derive(Deserialize)]
struct YearScanEntry {
    epoch_a1mjd: f64,
    tdb_minus_tt_s: f64,
    days_from_epoch_a1mjd_0: f64,
}

fn load() -> TdbCheck {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../goldens/tdb_check.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn tai_ns_for_a1mjd(a1mjd: f64) -> i64 {
    Tai::from_a1_mjd(a1mjd).as_nanos()
}

/// **The measurement that matters.** [`tai_ns_to_tdb_minus_tt_seconds`] must reproduce the
/// regenerated `goldens/tdb_check.json`'s own `tdb_minus_tt_s`, at its 4 fixture epochs AND
/// its 522-point `year_scan` (526 points total) -- the golden's own recorded `tolerance_s` is
/// the bound asserted, never a value copied from a paper or left at a default.
///
/// **Measured** (this test's own `eprintln` output, reproduced here for the record): over
/// the 4 fixture epochs alone, max = 1.171e-7 s (117.1 ns); over the 522-point `year_scan`
/// alone, RMS = 1.465e-7 s (146.5 ns), max = 3.218e-7 s (321.8 ns); combined (526 points),
/// RMS = 1.461e-7 s (146.1 ns), max = 3.218e-7 s (321.8 ns). This residual is NOT the old
/// phase artifact (that was ~1.6 ms, four orders of magnitude larger).
///
/// **What the residual IS, root-caused rather than described (manager review, round 2).** It
/// is the GOLDEN'S OWN f64 resolution, not a disagreement between the two series. GMAT returns
/// `tt_mjd` and `tdb_mjd` as separate MJD-magnitude doubles (~3.1e4 to 3.5e4) and
/// `goldens/gen_tdb_check.py` records their difference, so the recorded `tdb_minus_tt_s`
/// carries up to one ULP of that magnitude: `2^-38` days = 3.1432e-7 s below the 32768-day
/// binade boundary, twice that above it. Measured over all 526 fixture points: **every single
/// residual is at most 0.531 ULP of its own epoch's MJD magnitude** (the assertion below pins
/// exactly that), i.e. within a single rounding of a quantity that is otherwise exact. The
/// genuine model gap is separately computable and four and a half times smaller than even one
/// ULP: GMAT forms the series argument from the TAI MJD (`ConvertFromTaiMjd`'s `origValue`)
/// while this module forms it from the TT MJD, worth `(TT - A.1) / 86400 / 36525` centuries of
/// the `M_E_COEFF1` rate -- **10.80 ns at most**, computed directly, not fitted. So this
/// module's series and GMAT's agree to the golden's representable resolution, and the
/// ULP-ratio assertion below is the measurement that says so; `goldens/tdb_check.json`'s own
/// `tolerance_s` (5e-7 s) is the recorded absolute bound, asserted as well.
#[test]
fn native_tdb_matches_the_regenerated_golden() {
    let fixture = load();
    assert!(fixture.year_scan.len() > 100, "the year_scan fixture should have hundreds of points; got {}", fixture.year_scan.len());

    // One ULP of the golden's own recorded MJD magnitude, in seconds -- the resolution floor
    // `tdb_minus_tt_s` was recorded at (see this test's own doc comment, "What the residual
    // IS"). `f64::next_up` is not available on this toolchain's stable surface, so the ULP is
    // taken from the exponent directly, which is exact for a normal f64.
    fn ulp_seconds(mjd: f64) -> f64 {
        let exponent = mjd.abs().log2().floor() as i32;
        2.0_f64.powi(exponent - 52) * 86_400.0
    }
    // The largest residual any fixture point shows, as a fraction of its OWN epoch's ULP.
    let mut worst_ulp_ratio = 0.0_f64;

    let mut epoch_max_s = 0.0_f64;
    for e in &fixture.epochs {
        let t_tai_ns = tai_ns_for_a1mjd(e.epoch_a1mjd);
        let native_s = tai_ns_to_tdb_minus_tt_seconds(t_tai_ns);
        let diff_s = (native_s - e.tdb_minus_tt_s).abs();
        worst_ulp_ratio = worst_ulp_ratio.max(diff_s / ulp_seconds(e.tt_mjd));
        eprintln!(
            "n2-tdb-agreement: fraction_of_arc={:.3} native_tdb_minus_tt={native_s:.12e} gmat_tdb_minus_tt={:.12e} diff={diff_s:e} s",
            e.fraction_of_arc, e.tdb_minus_tt_s
        );
        epoch_max_s = epoch_max_s.max(diff_s);
    }

    let mut year_scan_sum_sq_s2 = 0.0_f64;
    let mut year_scan_max_s = 0.0_f64;
    for e in &fixture.year_scan {
        let t_tai_ns = tai_ns_for_a1mjd(e.epoch_a1mjd);
        let native_s = tai_ns_to_tdb_minus_tt_seconds(t_tai_ns);
        let diff_s = (native_s - e.tdb_minus_tt_s).abs();
        year_scan_sum_sq_s2 += diff_s * diff_s;
        year_scan_max_s = year_scan_max_s.max(diff_s);
        worst_ulp_ratio = worst_ulp_ratio.max(diff_s / ulp_seconds(e.epoch_a1mjd));
        if diff_s > fixture.tolerance_s {
            eprintln!("n2-tdb-year-scan-outlier: day={} native={native_s:e} gmat={:e} diff={diff_s:e} s", e.days_from_epoch_a1mjd_0, e.tdb_minus_tt_s);
        }
    }
    let year_scan_rms_s = (year_scan_sum_sq_s2 / fixture.year_scan.len() as f64).sqrt();

    let n_total = fixture.epochs.len() + fixture.year_scan.len();
    let combined_sum_sq_s2 = year_scan_sum_sq_s2
        + fixture
            .epochs
            .iter()
            .map(|e| {
                let t_tai_ns = tai_ns_for_a1mjd(e.epoch_a1mjd);
                let diff_s = tai_ns_to_tdb_minus_tt_seconds(t_tai_ns) - e.tdb_minus_tt_s;
                diff_s * diff_s
            })
            .sum::<f64>();
    let combined_rms_s = (combined_sum_sq_s2 / n_total as f64).sqrt();
    let combined_max_s = epoch_max_s.max(year_scan_max_s);

    eprintln!(
        "n2-tdb-agreement: epochs_max={epoch_max_s:e} s ({:.3} ns) year_scan_n={} year_scan_rms={year_scan_rms_s:e} s ({:.3} ns) year_scan_max={year_scan_max_s:e} s ({:.3} ns)",
        epoch_max_s * 1e9,
        fixture.year_scan.len(),
        year_scan_rms_s * 1e9,
        year_scan_max_s * 1e9,
    );
    eprintln!(
        "n2-tdb-agreement: combined n={n_total} rms={combined_rms_s:e} s ({:.3} ns) max={combined_max_s:e} s ({:.3} ns) tolerance_s={:e} s (from goldens/tdb_check.json)",
        combined_rms_s * 1e9,
        combined_max_s * 1e9,
        fixture.tolerance_s,
    );

    assert!(
        combined_max_s < fixture.tolerance_s,
        "native tai_ns_to_tdb_minus_tt_seconds disagrees with the regenerated golden by {combined_max_s:e} s, exceeding the golden's own recorded tolerance_s={:e} s",
        fixture.tolerance_s
    );

    // The tighter, more meaningful pin (this test's own doc comment, "What the residual IS"):
    // every residual is inside ONE ULP of the golden's own recorded MJD magnitude, i.e. the
    // two series agree to the resolution the golden was recorded at. Measured worst ratio
    // 0.531; asserted below 1.0 so a real series disagreement -- which would have to exceed a
    // whole ULP somewhere across 526 points -- cannot hide inside the 5e-7 s absolute bound.
    eprintln!("n2-tdb-agreement: worst residual / own-epoch ULP = {worst_ulp_ratio:.4} (1.0 is the golden's own recording resolution)");
    assert!(
        worst_ulp_ratio < 1.0,
        "a residual reached {worst_ulp_ratio:.4} of its own epoch's f64 ULP: that is larger than the golden's recording resolution can explain, so the two series genuinely disagree"
    );
}

/// TAI/TT agreement with the golden: [`av_orbital::tdb::tai_ns_to_tt_mjd`] (this crate's own
/// TT conversion, a **fixed** `TT - A.1` offset applied to `av_cdm::time::Tai::to_a1_mjd`)
/// reproduces the golden's own `tt_mjd`, and the golden's own `tai_mjd` is reproduced from
/// `av_cdm::time`'s own documented, equally fixed `A1 = TAI + 0.034_381_7 s` constant. Both
/// TAI and TT are fixed offsets from A.1, independent of `refJd` -- this test, together with
/// the direct before/after comparison recorded when this golden was regenerated with
/// `refJd=2_430_000.0` in place of the previous revision's `refJd=0.0`, confirms by
/// measurement (not assumption) that TAI and TT did not move when `refJd` changed; only TDB
/// did.
#[test]
fn tai_and_tt_conversions_agree_with_the_golden() {
    /// `A1 = TAI + 0.034_381_7 s` exactly (`av_cdm::time`'s own documented constant, restated
    /// here rather than imported because `av_cdm::time` does not export it as such).
    const A1_MINUS_TAI_S: f64 = 0.034_381_7;

    let fixture = load();
    let mut max_tt_diff_days = 0.0_f64;
    let mut max_tai_diff_days = 0.0_f64;
    for e in &fixture.epochs {
        let t_tai_ns = tai_ns_for_a1mjd(e.epoch_a1mjd);
        let tt_mjd = av_orbital::tdb::tai_ns_to_tt_mjd(t_tai_ns);
        let tai_mjd_expected = e.epoch_a1mjd - A1_MINUS_TAI_S / 86_400.0;
        let tt_diff_days = (tt_mjd - e.tt_mjd).abs();
        let tai_diff_days = (tai_mjd_expected - e.tai_mjd).abs();
        eprintln!(
            "n2-tdb-tai-tt: fraction_of_arc={:.3} tt_mjd_native={tt_mjd:.12} tt_mjd_gmat={:.12} tt_diff={tt_diff_days:e} days tai_mjd_expected={tai_mjd_expected:.12} tai_mjd_gmat={:.12} tai_diff={tai_diff_days:e} days",
            e.fraction_of_arc, e.tt_mjd, e.tai_mjd
        );
        max_tt_diff_days = max_tt_diff_days.max(tt_diff_days);
        max_tai_diff_days = max_tai_diff_days.max(tai_diff_days);
    }
    eprintln!("n2-tdb-tai-tt: max_tt_diff={max_tt_diff_days:e} days max_tai_diff={max_tai_diff_days:e} days");
    // f64 resolution at this ~31000-magnitude MJD scale is ~3.6e-12 days (its own ULP); allow
    // generous margin above that floor.
    const TOLERANCE_DAYS: f64 = 1e-9;
    assert!(max_tt_diff_days < TOLERANCE_DAYS, "native TT MJD disagrees with the golden's tt_mjd by {max_tt_diff_days:e} days, exceeding {TOLERANCE_DAYS:e} days");
    assert!(max_tai_diff_days < TOLERANCE_DAYS, "expected TAI MJD disagrees with the golden's tai_mjd by {max_tai_diff_days:e} days, exceeding {TOLERANCE_DAYS:e} days");
}

/// [`av_orbital::tdb::tai_ns_to_tdb_jd`] itself, sanity-checked against the small-magnitude
/// correction it is built from ([`tai_ns_to_tdb_minus_tt_seconds`]) -- not to the same
/// precision as that function (`crate::tdb`'s own "Precision" doc section explains the ~47
/// microsecond ceiling a full-JD-magnitude `f64` imposes), but loosely, to catch a gross error
/// (wrong sign, wrong magnitude, wrong units, or the wrong series wired in) in the one
/// function this crate's third-body code actually calls.
#[test]
fn native_tdb_jd_is_consistent_with_the_small_magnitude_correction() {
    let fixture = load();
    let mut max_diff_s = 0.0_f64;
    for e in fixture.year_scan.iter().step_by(37) {
        let t_tai_ns = tai_ns_for_a1mjd(e.epoch_a1mjd);
        let jd_tdb = av_orbital::tdb::tai_ns_to_tdb_jd(t_tai_ns);
        let jd_tt = av_orbital::tdb::tai_ns_to_tt_mjd(t_tai_ns) + 2_430_000.0;
        let full_jd_diff_s = (jd_tdb - jd_tt) * 86_400.0;
        let correct_s = tai_ns_to_tdb_minus_tt_seconds(t_tai_ns);
        let diff_s = (full_jd_diff_s - correct_s).abs();
        max_diff_s = max_diff_s.max(diff_s);
    }
    eprintln!("n2-tdb-jd-consistency: max diff over the sampled year_scan = {max_diff_s:e} s");
    // ~47 microseconds is the theoretical full-JD ULP ceiling (crate::tdb's own doc, measured
    // there); allow some margin for the intermediate roundings this path also does.
    const TOLERANCE_S: f64 = 2e-4;
    assert!(max_diff_s < TOLERANCE_S, "tai_ns_to_tdb_jd disagrees with the small-magnitude correction by {max_diff_s:e} s, exceeding the {TOLERANCE_S:e} s full-JD precision ceiling");
}
