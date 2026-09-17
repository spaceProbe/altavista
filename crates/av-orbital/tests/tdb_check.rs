//! Measures [`av_orbital::tdb::tai_ns_to_tdb_jd`] against GMAT's own `TimeSystemConverter`
//! (`docs/native-dynamics-plan.md` milestone N2, step 3), via the fixture
//! `goldens/tdb_check.json` (`goldens/gen_tdb_check.py`). No `gmat-frames` feature gate: this
//! test reads a committed JSON fixture, not a live GMAT instance, so it runs in the
//! `--no-default-features` build too (`av_orbital::tdb` has no GMAT dependency).
use std::path::PathBuf;

use av_cdm::time::Tai;
use av_orbital::tdb::tai_ns_to_tdb_minus_tt_seconds;
use serde::Deserialize;

#[derive(Deserialize)]
struct TdbCheck {
    epochs: Vec<EpochEntry>,
    year_scan: Vec<YearScanEntry>,
}

#[derive(Deserialize)]
struct EpochEntry {
    epoch_a1mjd: f64,
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

/// **Measured**: at all four epochs across the golden arc, this module's native
/// `tai_ns_to_tdb_minus_tt_seconds` (the small-magnitude correction, NOT a difference of two
/// `tai_ns_to_tdb_jd`-scale numbers -- see that function's own doc, "why a full JD caps
/// resolution", for why the distinction matters) reproduces GMAT's own
/// `TimeSystemConverter::Convert(..., TDBMJD, ...)` minus `Convert(..., TTMJD, ...)` to
/// sub-microsecond precision -- see the printed `n2-tdb-agreement` lines in this crate's N2
/// report for the exact per-epoch numbers.
#[test]
fn native_tdb_matches_gmats_timesystemconverter() {
    let fixture = load();
    let mut max_diff_s = 0.0_f64;
    for e in &fixture.epochs {
        let t_tai_ns = tai_ns_for_a1mjd(e.epoch_a1mjd);
        let native_s = tai_ns_to_tdb_minus_tt_seconds(t_tai_ns);
        let gmat_s = e.tdb_minus_tt_s;
        let diff_s = (native_s - gmat_s).abs();
        eprintln!("n2-tdb-agreement: fraction_of_arc={:.3} native_tdb_minus_tt={native_s:.12e} gmat_tdb_minus_tt={gmat_s:.12e} diff={diff_s:e} s", e.fraction_of_arc);
        max_diff_s = max_diff_s.max(diff_s);
    }
    eprintln!("n2-tdb-agreement: max diff over {} epochs = {:e} s", fixture.epochs.len(), max_diff_s);
    // Set from the measured worst case (this crate's N2 report quotes the exact number),
    // comfortably above it: the empirically fit M_E_OFFSET (crate::tdb's own "Measured
    // finding") reproduces GMAT to well under a microsecond at these four epochs.
    const TOLERANCE_S: f64 = 1e-6;
    assert!(max_diff_s < TOLERANCE_S, "native TDB-TT disagrees with GMAT's own TimeSystemConverter by {max_diff_s:e} s, exceeding {TOLERANCE_S:e} s");
}

/// The empirical-calibration set itself (`crate::tdb`'s own "Measured finding": `M_E_OFFSET`
/// was fit in Python against exactly this 522-point, 10-year dataset, RMS 1.535e-07 s). This
/// test pins that the RUST implementation reproduces the SAME numbers the Python fit used --
/// i.e. that `tdb.rs`'s constant and formula were transcribed correctly from the fit, not a
/// re-validation of the fit's own quality (which is a Python-side, offline measurement
/// recorded in this crate's N2 report and in `goldens/gen_tdb_check.py`'s own module doc).
#[test]
fn native_tdb_matches_the_522_point_year_scan_the_offset_was_fit_from() {
    let fixture = load();
    assert!(fixture.year_scan.len() > 100, "the year_scan fixture should have hundreds of points; got {}", fixture.year_scan.len());
    let mut sum_sq = 0.0_f64;
    let mut max_abs = 0.0_f64;
    for e in &fixture.year_scan {
        let t_tai_ns = tai_ns_for_a1mjd(e.epoch_a1mjd);
        let native_s = tai_ns_to_tdb_minus_tt_seconds(t_tai_ns);
        let diff_s = (native_s - e.tdb_minus_tt_s).abs();
        sum_sq += diff_s * diff_s;
        max_abs = max_abs.max(diff_s);
        if diff_s > 1e-6 {
            eprintln!("n2-tdb-year-scan-outlier: day={} native={native_s:e} gmat={:e} diff={diff_s:e} s", e.days_from_epoch_a1mjd_0, e.tdb_minus_tt_s);
        }
    }
    let rms = (sum_sq / fixture.year_scan.len() as f64).sqrt();
    eprintln!("n2-tdb-year-scan: n={} rms={rms:e} s max_abs={max_abs:e} s", fixture.year_scan.len());
    // Measured (this crate's N2 report): RMS 1.535e-07 s over the whole 10-year span (matches
    // the Python fit's own reported RMS, confirming the Rust transcription is correct); set
    // just above the measured worst-case single-point deviation.
    const TOLERANCE_RMS_S: f64 = 5e-7;
    const TOLERANCE_MAX_S: f64 = 5e-6;
    assert!(rms < TOLERANCE_RMS_S, "RMS {rms:e} s exceeds {TOLERANCE_RMS_S:e} s over the year_scan");
    assert!(max_abs < TOLERANCE_MAX_S, "max abs {max_abs:e} s exceeds {TOLERANCE_MAX_S:e} s over the year_scan");
}

/// [`av_orbital::tdb::tai_ns_to_tdb_jd`] itself, sanity-checked against the SAME year_scan --
/// not to the same precision as the function above (this module's own "Precision" doc section
/// explains the ~47 microsecond ceiling a full-JD-magnitude `f64` imposes), but loosely, to
/// catch a gross error (wrong sign, wrong magnitude, wrong units) in the one function this
/// crate's third-body code actually calls.
#[test]
fn native_tdb_jd_is_consistent_with_the_small_magnitude_correction() {
    let fixture = load();
    let mut max_diff_s = 0.0_f64;
    for e in fixture.year_scan.iter().step_by(37) {
        let t_tai_ns = tai_ns_for_a1mjd(e.epoch_a1mjd);
        let jd_tdb = av_orbital::tdb::tai_ns_to_tdb_jd(t_tai_ns);
        let jd_tt = av_orbital::tdb::tai_ns_to_tt_mjd(t_tai_ns) + 2_430_000.0;
        let full_jd_diff_s = (jd_tdb - jd_tt) * 86_400.0;
        let diff_s = (full_jd_diff_s - e.tdb_minus_tt_s).abs();
        max_diff_s = max_diff_s.max(diff_s);
    }
    eprintln!("n2-tdb-jd-consistency: max diff over the sampled year_scan = {max_diff_s:e} s");
    // ~47 microseconds is the theoretical full-JD ULP ceiling (this module's own doc); allow
    // some margin for the intermediate roundings this path also does.
    const TOLERANCE_S: f64 = 2e-4;
    assert!(max_diff_s < TOLERANCE_S, "tai_ns_to_tdb_jd disagrees with the small-magnitude correction by {max_diff_s:e} s, exceeding the {TOLERANCE_S:e} s full-JD precision ceiling");
}
