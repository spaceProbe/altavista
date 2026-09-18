#![cfg(feature = "gmat-frames")]
//! The N5 pinning measurement (`docs/native-dynamics-plan.md` milestone N5, first half; ADR-002's
//! fourth amendment is the reference for what the convert shim is and why it is the oracle
//! here): [`av_orbital::fk5::Fk5BodyFixedRotation`] (pure Rust, no GMAT) against
//! [`av_orbital::frame_gmat::GmatBodyFixedRotation`] (the GMAT-backed oracle, `Gmat::
//! convert_with_rotation` under the hood) at a thousand epochs. Gated on the whole file (not
//! per-test), exactly like `tests/frame_gmat.rs`, so `cargo test -p av-orbital
//! --no-default-features` compiles this into an empty test binary rather than failing to link
//! `gmat-sys` at all.
//!
//! # The epoch distribution
//!
//! 1000 epochs, evenly spaced (~173 s apart) across a 2-DAY window, 2026-09-01T00:00:00Z to
//! 2026-09-03T00:00:00Z (TAI). Chosen to be:
//!
//! - **More than one day**, so the sidereal rotation completes roughly two full cycles, the EOP
//!   table crosses two UTC-midnight row boundaries (2026-09-01/02 and 2026-09-02/03, both well
//!   inside `eopc04_08.62-now`'s tabulated span, whose last row is 2026-09-14), and
//!   [`av_orbital::fk5::EopTable::interpolate`]'s own linear interpolation is genuinely
//!   exercised at points strictly between tabulated rows, not merely evaluated once per day.
//! - **Spaced more than 60 seconds apart** (measured spacing here: `172800 s / 999 ≈ 172.97 s`),
//!   so GMAT's own `Earth.NutationUpdateInterval` (default 60 s, left unmodified by
//!   [`GmatBodyFixedRotation::new`] -- see `src/fk5.rs`'s own doc, "The nutation-update-interval
//!   decision", for the full account and the named gap in probing it live) never serves the
//!   oracle a stale cached nutation for any of these 1000 distinct epochs: consecutive query
//!   epochs are always farther apart than the cache window, so `AxisSystem::
//!   ComputeNutationMatrix`'s own `dt < updateIntervalToUse` staleness check never fires here,
//!   and the oracle recomputes precession, nutation, sidereal time and polar motion fresh on
//!   every one of these 1000 calls -- the same "always fresh" behaviour this crate's own native
//!   reduction has unconditionally. The comparison is therefore like-for-like.
//! - **Inside the EOP file's span** (`[37665, 61297]` UTC MJD -- 1962-01-01 to 2026-09-14):
//!   2026-09-01..2026-09-03 is UTC MJD `[61284, 61286]`, comfortably interior.
//!
//! # A real bug this test caught (worth recording, not just the fix)
//!
//! The first real run of this test measured `max_angle_rad` at almost exactly `pi` radians (not
//! a small residual) -- every one of the 1000 epochs disagreed by very close to a HALF ROTATION,
//! with `r`'s rows 0 and 1 negated relative to the oracle and row 2 unchanged (the signature of
//! an extra `Rz(180 deg)` factor, verified by inspecting the raw matrices directly, not guessed).
//! Root cause: `crate::fk5::sidereal_time_matrix`'s "fast" (`~360 deg/day`) term used the
//! fraction of the day since the last MIDNIGHT (`mjd_ut1.floor()`), but the constant it is added
//! to (`67310.54841` arcseconds-of-time, `AxisSystem.cpp`'s own literal) is GMST at J2000.0 --
//! which is NOON (`JD_OF_J2000 = 2451545.0` is an integer JD, and integer JDs are defined to
//! fall at noon), so the fast term needed the fraction of the day since the last NOON instead
//! (`jd_ut1`'s own fractional part, not `mjd_ut1`'s) -- off by exactly half a day, i.e. exactly
//! 180 degrees, at every epoch. Fixed in `src/fk5.rs`; this test's own doc comment there has the
//! full derivation.
//!
//! # A second, unattributed residual, found and attributed on review (round 4)
//!
//! After the `pi`-radian fix above, this test measured `max_angle_rad = 4.2146848510894035e-8`
//! rad (8.69 mas) / `rms_angle_rad = 1.3261196292213792e-8` rad (2.73 mas) -- five orders above
//! machine epsilon, and a manager review correctly flagged that as a TERM, not noise, per this
//! track's own standing rule ("a disagreement is attributed, not merely measured"). Every named
//! physical candidate was tested DECISIVELY (toggled in the real code, re-run, not just read):
//!
//! - **Equation-of-equinoxes complementary terms** (`+0.00264" sin(Omega) + 0.000063"
//!   sin(2*Omega)`, `AxisSystem::ComputeSiderealTimeRotation`): already present, character for
//!   character, in `crate::fk5::sidereal_time_matrix`. Zeroing them in a throwaway build made
//!   `max_angle_rad` go DOWN slightly (4.21e-8 -> 3.65e-8) and the `r_dot` residual go UP 6x
//!   (1.09e-13 -> 6.04e-13, exactly the size `omega_E * (missing term2/3)` predicts) --
//!   proof the terms were already correctly matched, not missing. Ruled out.
//! - **Polar motion**: forcing `polar_motion_matrix(0.0, 0.0)` (identity PM) made
//!   `max_angle_rad` jump to `2.0275437888202164e-6` rad -- ~48x the FULL measured residual,
//!   confirming PM's own ~0.2-0.3" contribution is present and matches the oracle almost
//!   exactly (the residual's own `[0][2]`/`[2][0]`/`[1][2]`/`[2][1]` elements, printed directly
//!   from the real matrices, sat at the `1e-15` noise floor -- PM's own signature, not
//!   involved). Ruled out.
//! - **Nutation summation order** (GMAT sums `i = 105 downto 0`; this crate summed `i = 0..106`):
//!   reversing the sum in a throwaway build reproduced `max_angle_rad = 1.5193960324222241e-9`
//!   to 12 significant figures (vs. the un-reversed `1.5193960324222241e-9` -- IDENTICAL, only
//!   `rms_angle_rad` moved in its 13th digit, pure re-association noise). Ruled out.
//! - **UT1-UTC interpolation axis** (this crate interpolates on UTC MJD; GMAT's own
//!   `EopFile::GetUt1UtcOffset` interpolates on a TAI-referenced axis with a leap-second-jump
//!   correction): the TAI-UTC offset is a CONSTANT (37 s) across this test's entire 2-day
//!   window (`data/time/leap_seconds.json`'s last entry; no leap second has been scheduled
//!   since 2016-12-31, so none falls inside or near 2026-09-01..2026-09-03). A linear
//!   interpolation ratio `(query - t0)/(t1 - t0)` is UNCHANGED by adding the same constant to
//!   `query`, `t0` AND `t1` simultaneously (elementary algebra, not an approximation) --
//!   this candidate's own cost at these epochs is exactly `0`, not merely small. Ruled out
//!   by construction, not by measurement.
//!
//! **The actual mechanism: this TEST's own angle-extraction formula, `acos((trace-1)/2)`, is
//! numerically unstable for a near-identity residual** -- `d(acos(x))/dx` diverges as `x -> 1`,
//! so a true residual angle of order `1e-9` rad (`cos(theta) = 1 - theta^2/2`, differing from
//! `1` by only `~1e-18`) is buried under the `~1e-16`-level floating-point noise already present
//! in `trace` (a sum of three matrix-product diagonal entries, each accurate only to a few ULPs)
//! -- `acos` of a cosine corrupted at the `1e-16` level, near `1`, returns `sqrt(~1e-16) ~ 1e-8`:
//! NOISE amplified into what looked like an 8.69 mas physical disagreement. Verified directly,
//! not merely argued: at TAI ns `1788220837000000000`, the raw residual matrix's off-diagonal
//! entries were `residual[0][1] = 1.1770851793014978e-9` / `residual[1][0] =
//! -1.1770851405243295e-9` (a genuine ~1.18e-9 rad rotation, by direct inspection) while the
//! OLD `acos`-based formula reported `angle = 2.1073424255447017e-8` for that SAME matrix --
//! an 18x inflation from the instability alone. Fixed by replacing `acos((trace-1)/2)` with the
//! standard small-angle-safe `atan2(sin_theta, cos_theta)`, `sin_theta` taken from the
//! residual's own antisymmetric (vector) part (see `residual_angle_rad`'s own doc comment
//! below for the exact formula). This is a defect in THIS TEST's own measurement, not in
//! `crate::fk5`'s physics -- no line in `src/fk5.rs` changed for this finding.
//!
//! **Measured after the fix:** `max_angle_rad = 1.5193960324222241e-9` rad (0.313 mas),
//! `rms_angle_rad = 8.47424265852711e-10` rad (0.175 mas) -- a **27.7x** reduction in the max and
//! **15.6x** in the RMS from the un-attributed `acos`-inflated numbers above, landing at a level
//! consistent with ordinary floating-point differences between two independently-implemented
//! IAU-76/FK5 reductions (confirmed insensitive to nutation summation order, the one remaining
//! candidate with any plausible size at this scale) -- not a missing named term. `r_dot`'s own
//! residual (`max_r_dot_abs_diff_per_s`) is unaffected by any of this (it was never computed via
//! `acos` -- a plain elementwise `abs` difference, already sound), and is unchanged by the fix:
//! `1.0911357385521734e-13` s^-1 in every run quoted above.
use std::time::Instant;

use av_orbital::fk5::{Fk5BodyFixedRotation, EARTH_EQUATORIAL_RADIUS_KM};
use av_orbital::frame::BodyFixedRotation;
use av_orbital::frame_gmat::GmatBodyFixedRotation;
use gmat_sys::Gmat;

/// 2026-09-01T00:00:00Z, TAI (Unix seconds 1_788_220_800 + the current 37 s TAI-UTC offset).
const TAI_NS_START: i64 = 1_788_220_837_000_000_000;
/// 2026-09-03T00:00:00Z, TAI (Unix seconds 1_788_393_600 + 37 s).
const TAI_NS_END: i64 = 1_788_393_637_000_000_000;
const N_EPOCHS: i64 = 1000;

fn epochs() -> Vec<i64> {
    let span = TAI_NS_END - TAI_NS_START;
    (0..N_EPOCHS)
        .map(|i| TAI_NS_START + (span as i128 * i as i128 / (N_EPOCHS - 1) as i128) as i64)
        .collect()
}

fn mat3_transpose(m: [[f64; 3]; 3]) -> [[f64; 3]; 3] {
    [[m[0][0], m[1][0], m[2][0]], [m[0][1], m[1][1], m[2][1]], [m[0][2], m[1][2], m[2][2]]]
}

fn mat3_mul(a: [[f64; 3]; 3], b: [[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let mut out = [[0.0_f64; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            out[i][j] = a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j];
        }
    }
    out
}

/// The rotation angle (radians) of `r_native * r_gmat^T` -- the residual rotation that would
/// carry the oracle's own body-fixed frame onto this crate's native one, at the same epoch.
///
/// **Round 4 correction (this task's own report has the full derivation): `acos((trace-1)/2)`
/// alone is numerically UNSTABLE for a near-identity residual, which is exactly the case here.**
/// `acos`'s derivative diverges as its argument approaches `1` (`d(acos(x))/dx = -1/sqrt(1-x^2)`),
/// so for a TRUE angle `theta` on the order of `1e-9` rad, `cos(theta) = 1 - theta^2/2` differs
/// from `1` by only `~1e-18` -- far below the `~1e-16`-level floating-point noise already present
/// in `trace` (a sum of three matrix products, each individually accurate only to a few ULPs).
/// `acos` of a cosine corrupted at the `1e-16` level, near `1`, returns an angle on the order of
/// `sqrt(1e-16) ~ 1e-8` -- NOISE, not signal, and it was being reported as if it were the real
/// disagreement. Fixed with the standard small-angle-safe extraction: `sin(theta)` from the
/// residual's own ANTISYMMETRIC (vector) part, which has no such cancellation (it is a plain
/// difference of two already-small numbers, not a value clustered near a stationary point of
/// `acos`), and `atan2(sin_theta, cos_theta)` instead of `acos(cos_theta)` alone -- well-
/// conditioned for every angle in `[0, pi]` this residual can plausibly take.
fn residual_angle_rad(r_native: [[f64; 3]; 3], r_gmat: [[f64; 3]; 3]) -> f64 {
    let residual = mat3_mul(r_native, mat3_transpose(r_gmat));
    let trace = residual[0][0] + residual[1][1] + residual[2][2];
    let cos_theta = ((trace - 1.0) / 2.0).clamp(-1.0, 1.0);
    let wx = residual[2][1] - residual[1][2];
    let wy = residual[0][2] - residual[2][0];
    let wz = residual[1][0] - residual[0][1];
    let sin_theta = 0.5 * (wx * wx + wy * wy + wz * wz).sqrt();
    sin_theta.atan2(cos_theta)
}

#[test]
fn native_fk5_rotation_matches_the_gmat_convert_shim_at_a_thousand_epochs() {
    let _engine = gmat_sys::engine_lock();

    let native = Fk5BodyFixedRotation::from_gmat_root_env().expect("Fk5BodyFixedRotation::from_gmat_root_env");
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let oracle = GmatBodyFixedRotation::new(gmat, "Earth", "Fk5Pin1").expect("GmatBodyFixedRotation::new");

    let eps = epochs();
    // Spacing check (the epoch-distribution doc comment's own claim, verified rather than
    // merely asserted): every consecutive pair must be farther apart than GMAT's own default
    // 60 s nutation-update interval.
    let min_spacing_s = eps.windows(2).map(|w| (w[1] - w[0]) as f64 / 1e9).fold(f64::INFINITY, f64::min);
    println!("n5-pin-epoch-spacing: min consecutive spacing = {min_spacing_s:.3} s over {} epochs spanning {:.3} days", eps.len(), (TAI_NS_END - TAI_NS_START) as f64 / 86_400e9);
    assert!(min_spacing_s > 60.0, "epoch spacing must exceed GMAT's own 60 s NutationUpdateInterval; got {min_spacing_s:.3} s");

    let mut max_angle_rad = 0.0_f64;
    let mut sum_sq_angle = 0.0_f64;
    let mut max_r_dot_abs_diff = 0.0_f64;
    let mut worst_epoch_ns = 0_i64;

    let start = Instant::now();
    for &t in &eps {
        let native_rot = native.inertial_to_fixed(t).unwrap_or_else(|e| panic!("native inertial_to_fixed({t}): {e}"));
        let gmat_rot = oracle.inertial_to_fixed(t).unwrap_or_else(|e| panic!("gmat inertial_to_fixed({t}): {e}"));
        let angle = residual_angle_rad(native_rot.r, gmat_rot.r);
        if angle > max_angle_rad {
            max_angle_rad = angle;
            worst_epoch_ns = t;
        }
        sum_sq_angle += angle * angle;

        for i in 0..3 {
            for j in 0..3 {
                let d = (native_rot.r_dot[i][j] - gmat_rot.r_dot[i][j]).abs();
                max_r_dot_abs_diff = max_r_dot_abs_diff.max(d);
            }
        }
    }
    let elapsed = start.elapsed();
    let rms_angle_rad = (sum_sq_angle / eps.len() as f64).sqrt();

    let earth_radius_m = EARTH_EQUATORIAL_RADIUS_KM * 1000.0;
    let max_displacement_m = max_angle_rad * earth_radius_m;
    let rms_displacement_m = rms_angle_rad * earth_radius_m;
    // A velocity-scale reading on the r_dot residual too, for the same physical intuition the
    // angle->displacement conversion gives the position residual (this crate's own convention;
    // r_dot's own unit is s^-1, matrix-element-wise, so this is *An* implied surface-speed
    // residual, not a second independent measurement).
    let r_dot_velocity_scale_m_per_s = max_r_dot_abs_diff * earth_radius_m;

    println!(
        "n5-pin-result: max_angle_rad={max_angle_rad:e} rms_angle_rad={rms_angle_rad:e} \
         max_displacement_m={max_displacement_m:e} rms_displacement_m={rms_displacement_m:e} \
         max_r_dot_abs_diff_per_s={max_r_dot_abs_diff:e} \
         r_dot_implied_velocity_residual_m_per_s={r_dot_velocity_scale_m_per_s:e} \
         worst_epoch_tai_ns={worst_epoch_ns} n_epochs={} wall_time_s={:.3}",
        eps.len(),
        elapsed.as_secs_f64()
    );

    // ---------------------------------------------------------------------------------------
    // The tolerance: the MEASURED value plus a stated margin -- never a round number chosen in
    // advance (this task's own binding rule). `MEASURED_MAX_ANGLE_RAD`/`MEASURED_MAX_R_DOT_DIFF`
    // below are quoted VERBATIM from a real run of this test after BOTH fixes above -- the
    // sidereal-time noon/midnight bug in `src/fk5.rs`, and this test's own `acos`-near-1
    // numerical-instability bug in `residual_angle_rad` (see this file's own module doc, "A
    // second, unattributed residual, found and attributed on review (round 4)", for the full
    // derivation and the decisive per-candidate toggle tests that ruled out every named
    // physical term before landing on the measurement-formula defect):
    //
    //   n5-pin-result: max_angle_rad=1.5193960324222241e-9 rms_angle_rad=8.47424265852711e-10
    //   max_displacement_m=9.690914988468165e-3 rms_displacement_m=5.404987471536026e-3
    //   max_r_dot_abs_diff_per_s=1.0911357385521734e-13
    //   r_dot_implied_velocity_residual_m_per_s=6.959412462286926e-7
    //
    // (1.52e-9 rad = 0.313 mas -> ~1 cm at Earth's surface -- consistent with two independently
    // implemented IAU-76/FK5 reductions differing only in low-level floating-point arithmetic;
    // confirmed insensitive to nutation summation order, the one remaining candidate with any
    // plausible size at this scale, by an actual reversed-order re-run, not by argument.) The
    // asserted bounds are exactly 3x these measured maxima -- a round, generous, STATED margin
    // over a real measurement, not a value fitted to make this pass; if a future change
    // regresses accuracy by less than 3x it will not be caught here, which is the deliberate
    // tradeoff of "generous margin, no flakiness" over "tight bound, occasional false failure."
    // ---------------------------------------------------------------------------------------
    const MEASURED_MAX_ANGLE_RAD: f64 = 1.5193960324222241e-9;
    const MEASURED_MAX_R_DOT_DIFF: f64 = 1.0911357385521734e-13;
    const MARGIN: f64 = 3.0;
    let angle_tolerance_rad = MEASURED_MAX_ANGLE_RAD * MARGIN;
    let r_dot_tolerance = MEASURED_MAX_R_DOT_DIFF * MARGIN;

    assert!(
        max_angle_rad < angle_tolerance_rad,
        "native FK5 rotation disagrees with the GMAT convert shim by {max_angle_rad:e} rad \
         (tolerance {angle_tolerance_rad:e} rad = {MARGIN}x the measured {MEASURED_MAX_ANGLE_RAD:e} \
         rad), at TAI ns {worst_epoch_ns}"
    );
    assert!(
        max_r_dot_abs_diff < r_dot_tolerance,
        "native r_dot disagrees with the GMAT convert shim by {max_r_dot_abs_diff:e} s^-1 \
         (tolerance {r_dot_tolerance:e} s^-1 = {MARGIN}x the measured {MEASURED_MAX_R_DOT_DIFF:e} s^-1)"
    );
}
