#![cfg(feature = "gmat-frames")]
//! GMAT-touching tests for [`av_orbital::frame_gmat::GmatBodyFixedRotation`]
//! (`docs/native-dynamics-plan.md` milestone N1). Gated on the whole file (not per-test) so
//! `cargo test -p av-orbital --no-default-features` compiles this into an empty test binary
//! rather than failing to link `gmat-sys` at all.
//!
//! Mirrors `crates/gmat-sys/tests/convert_rotation.rs`'s own conventions (the same
//! `engine_lock()`/`Gmat::setup` boilerplate, the same "measure and print, `--nocapture`"
//! style for the perf test) -- read that file first if this one is confusing.
use std::path::PathBuf;
use std::time::Instant;

use av_cdm::time::Tai;
use av_orbital::cof;
use av_orbital::frame::BodyFixedRotation;
use av_orbital::frame_gmat::GmatBodyFixedRotation;
use av_orbital::gravity;
use gmat_sys::Gmat;

/// An arbitrary but fixed TAI instant (2027-01-14-ish) used by every test in this file -- not
/// otherwise meaningful, just held constant so every test's own epoch handling is comparable.
const TAI_NS: i64 = 1_800_000_000_000_000_000;

fn adapter(namespace: &str) -> GmatBodyFixedRotation {
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    GmatBodyFixedRotation::new(gmat, "Earth", namespace).expect("GmatBodyFixedRotation::new")
}

fn km_state(pos_m: [f64; 3]) -> [f64; 6] {
    [pos_m[0] / 1000.0, pos_m[1] / 1000.0, pos_m[2] / 1000.0, 0.0, 0.0, 0.0]
}

/// **The rotation's direction, proved rather than assumed.** `rotation.apply(pos)` (this
/// crate's own adapter) must agree with a fully independent path: `Gmat::convert` called
/// directly on the SAME two `CoordinateSystem` objects the adapter itself built (exposed via
/// `inertial_cs_name`/`fixed_cs_name` for exactly this purpose).
#[test]
fn rotation_matches_gmat_convert_for_a_position_vector() {
    let _engine = gmat_sys::engine_lock();
    let rot = adapter("RotDir1");
    let pos = [7_000_000.0, 1_000_000.0, 2_000_000.0]; // SI metres, not on any symmetry axis

    let rotation = rot.inertial_to_fixed(TAI_NS).expect("rotation");
    let got = rotation.apply(pos);

    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup (idempotent)");
    let epoch_a1mjd = Tai::from_nanos(TAI_NS).to_a1_mjd();
    let want_km = gmat.convert(epoch_a1mjd, &km_state(pos), rot.inertial_cs_name(), rot.fixed_cs_name()).expect("Gmat::convert");
    let want = [want_km[0] * 1000.0, want_km[1] * 1000.0, want_km[2] * 1000.0];

    let max_abs = (0..3).map(|i| (got[i] - want[i]).abs()).fold(0.0_f64, f64::max);
    eprintln!("[rotation direction] max abs diff vs Gmat::convert = {max_abs:.3e} m (position scale {:.3e} m)", pos[0].hypot(pos[1]).hypot(pos[2]));
    assert!(max_abs < 1e-6, "GmatBodyFixedRotation::apply disagrees with a direct Gmat::convert call by {max_abs:e} m");
}

/// **`r_dot` is consistent with `r`.** Central finite difference of `rotation.r` at three
/// nearby epochs, compared against the middle epoch's own `rotation.r_dot` -- mirrors
/// `crates/gmat-sys/tests/convert_rotation.rs::rotation_dot_matches_a_finite_difference_of_
/// the_rotation_matrix` exactly, through this crate's own adapter rather than the raw shim
/// call.
#[test]
fn rotation_dot_matches_a_finite_difference_of_the_rotation_matrix() {
    let _engine = gmat_sys::engine_lock();
    let rot = adapter("RotFd1");
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
    eprintln!("[r_dot finite-difference] max abs error = {max_abs_err:.3e} (h={h_s} s), max |r_dot| = {max_abs_rdot:.3e} (Earth's own rotation rate, ~7.29e-5 rad/s)");
    assert!(max_abs_rdot > 1e-6, "sanity: r_dot should be of order Earth's rotation rate; got max abs {max_abs_rdot:e} -- suspect a zeroed r_dot");
    assert!(max_abs_err < 1e-6, "r_dot disagrees with a finite-difference derivative of r by {max_abs_err:e} (bound 1e-6)");
}

fn potfield_line(n: usize, m: usize, mu: f64, radius: f64) -> String {
    format!("POTFIELD{n:>3}{m:>3}  1 {mu:e} {radius:e} 1.0")
}

fn recoef_line(n: usize, m: usize, c: f64, s: f64) -> String {
    format!("RECOEF{n:>5}{m:>3}   {:>21}{:>21}", format!("{c:e}"), format!("{s:e}"))
}

/// Writes a synthetic, deliberately asymmetric (large C22/S22) degree/order-2 `.cof`-format
/// file to a temp path and returns it -- `crate::cof::read_earth_gravity`'s own fixed-column
/// format (see that module's doc comment), hand-built rather than copied from a real GMAT file
/// so the coefficient values are under this test's own control. C22/S22 (0.02/0.015, fully
/// normalised) are roughly four orders of magnitude larger than Earth's real ~1.8e-6, chosen
/// so a transposed rotation's error is unmistakably visible against this field's own signal,
/// not merely detectable in the noise.
fn write_synthetic_asymmetric_cof() -> PathBuf {
    let mu = 3.986004415e14;
    let radius = 6_378_136.3;
    let mut content = String::new();
    content.push_str("CCCCC synthetic asymmetric test field (av-orbital tests/frame_gmat.rs) CCCCC\n");
    content.push_str(&potfield_line(2, 2, mu, radius));
    content.push('\n');
    content.push_str(&recoef_line(2, 0, -4.84165371736e-4, 0.0));
    content.push('\n');
    content.push_str(&recoef_line(2, 1, 0.0, 0.0));
    content.push('\n');
    content.push_str(&recoef_line(2, 2, 0.02, 0.015));
    content.push('\n');
    let path = std::env::temp_dir().join(format!("av_orbital_synthetic_asym_{}_{:?}.cof", std::process::id(), std::thread::current().id()));
    std::fs::write(&path, content).expect("write synthetic .cof");
    path
}

/// **The transpose-detection test.** A spherically symmetric field (point mass, or any purely
/// zonal field) cannot catch a rotation sign/transpose bug -- `crate::frame`'s own module doc
/// explains why. This test uses a deliberately ASYMMETRIC field (large synthetic C22/S22, via
/// [`write_synthetic_asymmetric_cof`]) specifically so a transposed rotation gives a visibly
/// different acceleration, and separately cross-checks the CORRECT value against a fully
/// independent double-`Gmat::convert` path (never reusing this adapter's own `R`/`R^T` at all).
#[test]
fn asymmetric_field_detects_a_transposed_rotation() {
    let _engine = gmat_sys::engine_lock();
    let rot = adapter("RotAsym1");

    let path = write_synthetic_asymmetric_cof();
    let model = cof::read_earth_gravity(&path, 2, 2).expect("read synthetic field");
    std::fs::remove_file(&path).ok();

    let pos_inertial = [6_800_000.0, 1_200_000.0, 2_300_000.0];
    let rotation = rot.inertial_to_fixed(TAI_NS).unwrap();

    let pos_fixed = rotation.apply(pos_inertial);
    let (accel_fixed, _) = gravity::spherical_harmonic_gravity(pos_fixed, &model);
    // The CORRECT inverse: R^T (crate::frame::Rotation::apply_transpose), exactly what
    // `av_orbital::model::EarthGravityModel::derivatives` itself uses.
    let accel_correct = rotation.apply_transpose(accel_fixed);
    // The deliberately WRONG variant: apply R again instead of R^T -- the classic
    // transpose/inverse mixup this task's own brief warns is "the single most likely defect".
    let accel_wrong = rotation.apply(accel_fixed);

    let diff = (0..3).map(|i| (accel_correct[i] - accel_wrong[i]).powi(2)).sum::<f64>().sqrt();
    let scale = (0..3).map(|i| accel_correct[i].powi(2)).sum::<f64>().sqrt();
    eprintln!(
        "[asymmetric field, transpose detection] |correct - wrong| = {diff:.3e} m/s^2 vs |correct| = {scale:.3e} m/s^2 ({:.1}% of signal)",
        100.0 * diff / scale
    );
    assert!(
        diff / scale > 0.01,
        "a transposed rotation must give a VISIBLY different acceleration on this asymmetric field; got only {:.3e}% difference -- suspect the field is not actually asymmetric enough, or apply/apply_transpose are accidentally identical",
        100.0 * diff / scale
    );

    // Independent cross-check of the CORRECT value: a fresh forward Gmat::convert (inertial ->
    // fixed) for the position, then a SEPARATE fresh inverse Gmat::convert (fixed -> inertial)
    // for the resulting acceleration -- never this adapter's own single cached R/R^T.
    let gmat = Gmat::setup(&Gmat::default_startup_file()).unwrap();
    let epoch_a1mjd = Tai::from_nanos(TAI_NS).to_a1_mjd();
    let pos_fixed_km_independent = gmat.convert(epoch_a1mjd, &km_state(pos_inertial), rot.inertial_cs_name(), rot.fixed_cs_name()).unwrap();
    let pos_fixed_independent = [pos_fixed_km_independent[0] * 1000.0, pos_fixed_km_independent[1] * 1000.0, pos_fixed_km_independent[2] * 1000.0];
    let (accel_fixed_independent, _) = gravity::spherical_harmonic_gravity(pos_fixed_independent, &model);
    let accel_back_km = gmat.convert(epoch_a1mjd, &km_state(accel_fixed_independent), rot.fixed_cs_name(), rot.inertial_cs_name()).unwrap();
    let accel_independent = [accel_back_km[0] * 1000.0, accel_back_km[1] * 1000.0, accel_back_km[2] * 1000.0];

    let max_abs = (0..3).map(|i| (accel_correct[i] - accel_independent[i]).abs()).fold(0.0_f64, f64::max);
    eprintln!(
        "[asymmetric field, independent cross-check] max abs diff vs a SEPARATE forward+inverse Gmat::convert pair = {max_abs:.3e} m/s^2 (relative {:.3e})",
        max_abs / scale
    );
    assert!(max_abs / scale < 1e-6, "the model's R/R^T-based acceleration disagrees with an independently double-converted reference by {:.3e} relative", max_abs / scale);
}

/// ADR-002's spike rule: measure and disclose the per-call cost, mirroring
/// `crates/gmat-sys/tests/convert_rotation.rs::measure_the_per_call_cost_of_convert_with_
/// rotation_vs_plain_convert`'s own style. Not a pass/fail gate -- a disclosed number
/// (`--nocapture`). Distinct epochs never hit [`GmatBodyFixedRotation`]'s single-entry cache;
/// an identical epoch, repeated, hits it every call after the first -- both measured here so
/// the cache's actual benefit is a number, not a claim.
#[test]
fn measure_the_per_call_cost_of_inertial_to_fixed() {
    let _engine = gmat_sys::engine_lock();
    let rot = adapter("RotPerf1");
    const N: i64 = 2000;

    let _ = rot.inertial_to_fixed(TAI_NS).unwrap(); // warm-up: pay any one-time AxisSystem setup cost outside the timed loop

    let start = Instant::now();
    for i in 0..N {
        std::hint::black_box(rot.inertial_to_fixed(TAI_NS + i).unwrap());
    }
    let distinct_us = 1e6 * start.elapsed().as_secs_f64() / N as f64;

    let start2 = Instant::now();
    for _ in 0..N {
        std::hint::black_box(rot.inertial_to_fixed(TAI_NS).unwrap());
    }
    let cached_us = 1e6 * start2.elapsed().as_secs_f64() / N as f64;

    eprintln!(
        "[GmatBodyFixedRotation::inertial_to_fixed perf, debug build, {N} calls each] \
         distinct epochs (cache miss every call): {distinct_us:.3} us/call; \
         identical epoch (cache hit every call after the first): {cached_us:.3} us/call \
         ({:.1}x)",
        distinct_us / cached_us.max(1e-9)
    );
}
