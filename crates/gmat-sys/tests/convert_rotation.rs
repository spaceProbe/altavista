//! `Gmat::convert_with_rotation` (M21.4, `docs/open-questions.md` question 138, ADR-002's fourth
//! amendment's "Amendment 2026-09-04 (fourth)"): the sibling of `Gmat::convert` that returns the
//! 3x3 rotation matrix and its time derivative (`R`/`Rdot`) the *same* `CoordinateConverter::
//! Convert` call computed, reached through `gmatffi_convert_state_and_rotation`.
//!
//! Four things this file pins:
//!
//! - the returned `state_km` matches plain `Gmat::convert`'s own output for the identical
//!   inputs (the new function must not compute the state differently while it's at it);
//! - `rotation_dot` genuinely matches a **finite-difference derivative of `rotation` sampled at
//!   nearby epochs** -- an independent numerical check that touches nothing but `rotation`
//!   itself (already golden-pinned by `convert.rs`'s own `EarthBodyFixed` test), so it cannot
//!   pass by `rotation_dot` merely being self-consistent with whatever `rotation` the same call
//!   produced;
//! - the resulting 6x6 Jacobian `[[R,0],[Rdot,R]]`, applied as `M P M^T` to
//!   `goldens/covariance_bodyfixed_leo_2h.json`'s declared `P0`, matches GMAT's own
//!   `OrbitErrorCovariance` `ReportFile` in `EarthFixed` **in the position block only** (rows/
//!   cols 0-2) -- the one block a missing `Rdot` term cannot affect;
//! - and, measured rather than assumed, that GMAT's own report does NOT match the same `M P M^T`
//!   in the velocity or position-velocity cross blocks -- proving `OrbitData::
//!   GetCovarianceRmat66`'s block-diagonal `[[R,0],[0,R]]` shortcut (see
//!   `goldens/gen_covariance_bodyfixed_leo_2h.py`'s own module doc comment) is not a correct
//!   reference for the full transform, and that this repository's own implementation must not
//!   (and, per the third bullet's disagreement, does not) imitate it. The cross block's
//!   disagreement is linear in Earth's own rotation rate (`R P_pos Rdot^T`, measured ~1.3e-6);
//!   the velocity block's is quadratic (`Rdot P_pos Rdot^T`, measured ~9e-11) -- both are real,
//!   non-noise disagreements, just at very different scales (see the last test's own doc
//!   comment for the arithmetic).
use gmat_sys::Gmat;
use serde::Deserialize;
use std::path::PathBuf;
use std::time::Instant;

#[derive(Deserialize)]
struct CovarianceGolden {
    report_last_row: ReportLastRow,
    p0_km: P0,
    cov_bodyfixed_gmat_report_km_row_major_6x6: Vec<f64>,
}

#[derive(Deserialize)]
struct P0 {
    row_major_6x6: Vec<f64>,
}

#[derive(Deserialize)]
struct ReportLastRow {
    epoch_a1mjd: f64,
    state_mj2000eq_km: [f64; 6],
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../goldens").join(format!("{name}.json"))
}

fn load_covariance_golden() -> CovarianceGolden {
    serde_json::from_str(&std::fs::read_to_string(golden_path("covariance_bodyfixed_leo_2h")).unwrap()).unwrap()
}

/// Build the 6x6 block matrix `[[R,0],[Rdot,R]]` (row-major) from `rotation`/`rotation_dot`.
fn block6(rotation: &[f64; 9], rotation_dot: &[f64; 9]) -> [f64; 36] {
    let mut m = [0.0; 36];
    for i in 0..3 {
        for j in 0..3 {
            m[i * 6 + j] = rotation[i * 3 + j]; // top-left: R
            m[i * 6 + (j + 3)] = 0.0; // top-right: 0
            m[(i + 3) * 6 + j] = rotation_dot[i * 3 + j]; // bottom-left: Rdot
            m[(i + 3) * 6 + (j + 3)] = rotation[i * 3 + j]; // bottom-right: R
        }
    }
    m
}

/// `m * p * m^T`, row-major 6x6 throughout (mirrors `av_dynamics::propagate_covariance`'s own
/// convention, reimplemented here rather than pulled in as a dependency -- `gmat-sys` has no
/// dependency on `av-dynamics` in the other direction and this crate's own tests stay
/// self-contained).
fn mat6_congruence(m: &[f64; 36], p: &[f64]) -> [f64; 36] {
    assert_eq!(p.len(), 36);
    let mut tmp = [0.0; 36];
    for i in 0..6 {
        for j in 0..6 {
            let mut s = 0.0;
            for k in 0..6 {
                s += m[i * 6 + k] * p[k * 6 + j];
            }
            tmp[i * 6 + j] = s;
        }
    }
    let mut out = [0.0; 36];
    for i in 0..6 {
        for j in 0..6 {
            let mut s = 0.0;
            for k in 0..6 {
                s += tmp[i * 6 + k] * m[j * 6 + k]; // * m^T
            }
            out[i * 6 + j] = s;
        }
    }
    out
}

fn block3_max_abs_diff(a: &[f64; 36], b: &[f64], row0: usize, col0: usize) -> f64 {
    let mut max_diff = 0.0f64;
    for i in 0..3 {
        for j in 0..3 {
            let av = a[(row0 + i) * 6 + (col0 + j)];
            let bv = b[(row0 + i) * 6 + (col0 + j)];
            max_diff = max_diff.max((av - bv).abs());
        }
    }
    max_diff
}

/// **Required pin (part 1/2): the new function's `state_km` output matches plain `Gmat::
/// convert`'s own output.** Fails against an implementation that computes the converted state
/// differently (e.g. a stale/second `CoordinateConverter`, or a transcription bug) while adding
/// the rotation output.
#[test]
fn convert_with_rotation_state_output_matches_plain_convert() {
    let _engine = gmat_sys::engine_lock();
    let golden = load_covariance_golden();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");

    gmat.coordinate_system("RotStateMj2000Eq", "Earth", "MJ2000Eq").unwrap();
    gmat.coordinate_system("RotStateBodyFixed", "Earth", "BodyFixed").unwrap();
    gmat.initialize().unwrap();

    let epoch = golden.report_last_row.epoch_a1mjd;
    let state_in = golden.report_last_row.state_mj2000eq_km;

    let plain = gmat.convert(epoch, &state_in, "RotStateMj2000Eq", "RotStateBodyFixed").unwrap();
    let with_rotation = gmat.convert_with_rotation(epoch, &state_in, "RotStateMj2000Eq", "RotStateBodyFixed").unwrap();

    for (i, (p, w)) in plain.iter().zip(with_rotation.state_km.iter()).enumerate() {
        assert!(
            (p - w).abs() < 1e-12,
            "component {i}: plain convert = {p}, convert_with_rotation = {w} (must be bit-for-bit-equivalent, same underlying CoordinateConverter::Convert call)"
        );
    }
}

/// **Required pin (part 2/2): `rotation_dot` matches a finite-difference derivative of
/// `rotation`.** Independent of `OrbitErrorCovariance`/GMAT's own covariance report entirely --
/// this only ever calls `convert_with_rotation` at three nearby epochs and differentiates its
/// own `rotation` output numerically. `h = 0.2` s is small relative to both the orbital period
/// (~5600 s) and 1/(Earth's rotation rate) (~13750 s), so the central-difference truncation
/// error is negligible next to the `1e-6` bound used below (bounded by `O(h^2 * d^3R/dt^3)`,
/// itself bounded by Earth's own rotation rate cubed times `h^2` -- many orders of magnitude
/// below `1e-6` for `h = 0.2` s).
///
/// Fails against: a `rotation_dot` that is wrong in sign, magnitude, zero, or otherwise not the
/// genuine time derivative of `rotation` (e.g. a copy-paste of `rotation` itself, or a matrix
/// read from the wrong `Rmatrix33` accessor).
#[test]
fn rotation_dot_matches_a_finite_difference_of_the_rotation_matrix() {
    let _engine = gmat_sys::engine_lock();
    let golden = load_covariance_golden();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");

    gmat.coordinate_system("RotFdMj2000Eq", "Earth", "MJ2000Eq").unwrap();
    gmat.coordinate_system("RotFdBodyFixed", "Earth", "BodyFixed").unwrap();
    gmat.initialize().unwrap();

    let epoch = golden.report_last_row.epoch_a1mjd;
    let state_in = golden.report_last_row.state_mj2000eq_km;
    // A.1 Modified Julian is in days; h_s seconds -> days.
    let h_s = 0.2;
    let h_days = h_s / 86400.0;

    let at = |e: f64| gmat.convert_with_rotation(e, &state_in, "RotFdMj2000Eq", "RotFdBodyFixed").unwrap();
    let minus = at(epoch - h_days);
    let mid = at(epoch);
    let plus = at(epoch + h_days);

    let mut max_abs_err = 0.0f64;
    let mut max_abs_rdot = 0.0f64;
    for i in 0..9 {
        let fd = (plus.rotation[i] - minus.rotation[i]) / (2.0 * h_s);
        let err = (fd - mid.rotation_dot[i]).abs();
        max_abs_err = max_abs_err.max(err);
        max_abs_rdot = max_abs_rdot.max(mid.rotation_dot[i].abs());
    }
    eprintln!(
        "[rotation_dot finite-difference] max abs error = {max_abs_err:.3e} (h={h_s} s), max |rotation_dot| = {max_abs_rdot:.3e} (Earth's own rotation rate, ~7.29e-5 rad/s)"
    );
    assert!(max_abs_rdot > 1e-6, "sanity: rotation_dot should be of order Earth's rotation rate (~7.29e-5), got max abs {max_abs_rdot:.3e} -- suspect a zeroed or unpopulated rotation_dot");
    assert!(max_abs_err < 1e-6, "rotation_dot disagrees with a finite-difference derivative of rotation by {max_abs_err:.3e} (bound 1e-6)");
}

/// **Required pin: the position block of `M P M^T` (M = [[R,0],[Rdot,R]], built from
/// `convert_with_rotation`'s own output) matches GMAT's own `OrbitErrorCovariance` `ReportFile`
/// in `EarthFixed`, for `goldens/covariance_bodyfixed_leo_2h.json`'s declared `P0`.** The
/// position block never involves `Rdot` (`(M P)M^T` top-left block is exactly `R P_pos R^T`,
/// see this crate's own `block6`/`mat6_congruence`), so GMAT's block-diagonal-only report is
/// exactly correct here regardless of the missing `Rdot` term -- this is the pin that proves
/// `rotation` itself is extracted correctly (right epoch, right body/axes, right row/column
/// convention), independent of the `Rdot` question the next test covers.
///
/// Fails against: a `rotation` transcribed with a row/column swap, the wrong `CoordinateSystem`
/// pair, or the wrong epoch (any of which would still produce *a* rotation, but not the one
/// GMAT's own, independently-computed `OrbitErrorCovariance` Parameter used for the identical
/// conversion).
#[test]
fn convert_with_rotation_position_block_matches_gmat_reportfile_orbiterrorcovariance() {
    let _engine = gmat_sys::engine_lock();
    let golden = load_covariance_golden();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");

    gmat.coordinate_system("RotPosMj2000Eq", "Earth", "MJ2000Eq").unwrap();
    gmat.coordinate_system("RotPosBodyFixed", "Earth", "BodyFixed").unwrap();
    gmat.initialize().unwrap();

    let epoch = golden.report_last_row.epoch_a1mjd;
    let state_in = golden.report_last_row.state_mj2000eq_km;
    let out = gmat.convert_with_rotation(epoch, &state_in, "RotPosMj2000Eq", "RotPosBodyFixed").unwrap();

    let m6 = block6(&out.rotation, &out.rotation_dot);
    let computed = mat6_congruence(&m6, &golden.p0_km.row_major_6x6);

    let pos_diff = block3_max_abs_diff(&computed, &golden.cov_bodyfixed_gmat_report_km_row_major_6x6, 0, 0);
    // P0's position variances are O(1e-2) km^2 (see gen_covariance_bodyfixed_leo_2h.py) -- 1e-12
    // relative to that scale.
    let scale = 0.04; // largest declared position variance (200 m)^2 = 0.04 km^2
    eprintln!("[covariance position block] max abs diff vs GMAT's own EarthFixed report = {pos_diff:.3e} (scale {scale}, relative {:.3e})", pos_diff / scale);
    assert!(pos_diff / scale < 1e-9, "position block relative error {:.3e} exceeds 1e-9 -- rotation itself may be wrong (independent of the Rdot question)", pos_diff / scale);
}

/// **Required, measured disclosure: GMAT's own `OrbitErrorCovariance` report does NOT match the
/// full `[[R,0],[Rdot,R]]` transform in the velocity or position-velocity cross blocks**, for
/// the identical `P0`/epoch/rotation the previous test already showed agrees in the position
/// block. This is the direct evidence `OrbitData::GetCovarianceRmat66` builds only the
/// block-diagonal `[[R,0],[0,R]]` transform (never calling `GetLastRotationDotMatrix()`) --
/// GMAT's own report is measurably NOT ground truth for a rotating target frame, which is why
/// this repository's own `av-kernel` executor pins against `R P Rᵀ` built from GMAT's own
/// reported rotation (the REQUIRED PIN's "otherwise" branch) rather than against this report
/// (see this task's own M21.4 report for the full account).
///
/// This test also doubles as a regression guard on this repository's OWN implementation: if
/// `block6`/`convert_with_rotation` ever regressed to a block-diagonal-only (Rdot-omitting)
/// transform, the disagreement asserted below would vanish and this test would fail --
/// `cov_bodyfixed_gmat_report_km_row_major_6x6`'s cross block is *exactly* zero (not merely
/// small), so a correct, Rdot-including implementation must disagree with it by a large,
/// measured margin, not by noise.
#[test]
fn full_transform_disagrees_with_gmat_reportfile_in_velocity_and_cross_blocks_due_to_missing_rdot() {
    let _engine = gmat_sys::engine_lock();
    let golden = load_covariance_golden();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");

    gmat.coordinate_system("RotXMj2000Eq", "Earth", "MJ2000Eq").unwrap();
    gmat.coordinate_system("RotXBodyFixed", "Earth", "BodyFixed").unwrap();
    gmat.initialize().unwrap();

    let epoch = golden.report_last_row.epoch_a1mjd;
    let state_in = golden.report_last_row.state_mj2000eq_km;
    let out = gmat.convert_with_rotation(epoch, &state_in, "RotXMj2000Eq", "RotXBodyFixed").unwrap();

    let m6 = block6(&out.rotation, &out.rotation_dot);
    let computed = mat6_congruence(&m6, &golden.p0_km.row_major_6x6);

    // GMAT's own report has an EXACTLY zero cross block (OrbitData::GetCovarianceRmat66 never
    // writes it) -- so the full transform's cross block, if Rdot is genuinely included, is
    // itself the disagreement magnitude.
    let cross_diff = block3_max_abs_diff(&computed, &golden.cov_bodyfixed_gmat_report_km_row_major_6x6, 0, 3);
    let vel_diff = block3_max_abs_diff(&computed, &golden.cov_bodyfixed_gmat_report_km_row_major_6x6, 3, 3);

    eprintln!("[covariance cross/velocity blocks] max abs diff vs GMAT's own (Rdot-omitting) EarthFixed report: cross = {cross_diff:.3e}, velocity = {vel_diff:.3e}");
    // GMAT's own report puts an EXACT 0.0 in both blocks. The full transform's CROSS block is
    // `R * P_pos * Rdot^T`, linear in Rdot: magnitude ~ P_pos * |Rdot| ~ 0.04 km^2 * 5.6e-5
    // rad/s (Earth's rotation rate, measured by the finite-difference test above) ~ 2e-6 --
    // matches the ~1.3e-6 this test actually measures, well above a 1e-9 floor. The VELOCITY
    // block correction is `Rdot * P_pos * Rdot^T`, QUADRATIC in Rdot (both factors are Rdot, not
    // one): magnitude ~ P_pos * |Rdot|^2 ~ 0.04 * (5.6e-5)^2 ~ 1.3e-10 -- three to four orders of
    // magnitude smaller than the cross block precisely because it is second-order in the
    // rotation rate, not first. A 1e-12 floor (still ~50x the position block's own ~1e-17
    // same-computation agreement, i.e. far above float noise) is the honest bound for this
    // block, not the same 1e-9 the linear cross block supports.
    assert!(cross_diff > 1e-9, "cross block disagreement with GMAT's own report is only {cross_diff:.3e} -- expected a large, Rdot-sized (linear, ~1e-6) disagreement; this implementation may have silently regressed to a block-diagonal (Rdot-omitting) transform");
    assert!(vel_diff > 1e-12, "velocity block disagreement with GMAT's own report is only {vel_diff:.3e} -- expected a small but non-negligible, Rdot^2-sized (~1e-10) disagreement; this implementation may have silently regressed to a block-diagonal (Rdot-omitting) transform");
}

/// ADR-002's spike rule: measure and disclose the per-call cost of the new rotation call,
/// mirroring the style M19.1's own `Gmat::convert` measurement used (plain wall-clock loop,
/// `eprintln!`, `--nocapture`) -- not a strict pass/fail gate, a disclosed number. Run:
/// `cargo test -p gmat-sys --test convert_rotation -- --nocapture measure_the_per_call_cost`.
#[test]
fn measure_the_per_call_cost_of_convert_with_rotation_vs_plain_convert() {
    let _engine = gmat_sys::engine_lock();
    let golden = load_covariance_golden();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");

    gmat.coordinate_system("RotPerfMj2000Eq", "Earth", "MJ2000Eq").unwrap();
    gmat.coordinate_system("RotPerfBodyFixed", "Earth", "BodyFixed").unwrap();
    gmat.coordinate_system("RotPerfIcrf", "Earth", "ICRF").unwrap();
    gmat.initialize().unwrap();

    let epoch = golden.report_last_row.epoch_a1mjd;
    let state_in = golden.report_last_row.state_mj2000eq_km;
    const N: u32 = 2000;

    let time_it = |from: &str, to: &str, with_rotation: bool| -> f64 {
        // Warm-up: the first conversion to/from a given AxisSystem pays a one-time setup cost
        // (e.g. ICRF's own bias-rotation-vs-FK5 computation, `CoordinateConverter::
        // RotationMatrixFromICRFToFK5`, cached thereafter) -- excluded from the timed loop so
        // this measures steady-state per-call cost, not cold-start setup.
        let _ = gmat.convert(epoch, &state_in, from, to).unwrap();
        let _ = gmat.convert_with_rotation(epoch, &state_in, from, to).unwrap();
        let start = Instant::now();
        if with_rotation {
            for _ in 0..N {
                std::hint::black_box(gmat.convert_with_rotation(epoch, &state_in, from, to).unwrap());
            }
        } else {
            for _ in 0..N {
                std::hint::black_box(gmat.convert(epoch, &state_in, from, to).unwrap());
            }
        }
        1e6 * start.elapsed().as_secs_f64() / N as f64
    };

    let plain_bf = time_it("RotPerfMj2000Eq", "RotPerfBodyFixed", false);
    let rot_bf = time_it("RotPerfMj2000Eq", "RotPerfBodyFixed", true);
    let plain_icrf = time_it("RotPerfMj2000Eq", "RotPerfIcrf", false);
    let rot_icrf = time_it("RotPerfMj2000Eq", "RotPerfIcrf", true);

    eprintln!(
        "[convert_with_rotation perf, debug build, {N} calls each]\n\
         \x20 body-fixed: plain convert {plain_bf:.3} us/call, convert_with_rotation {rot_bf:.3} us/call (+{:.3} us, {:.1}%)\n\
         \x20 ICRF:       plain convert {plain_icrf:.3} us/call, convert_with_rotation {rot_icrf:.3} us/call (+{:.3} us, {:.1}%)",
        rot_bf - plain_bf, 100.0 * (rot_bf - plain_bf) / plain_bf,
        rot_icrf - plain_icrf, 100.0 * (rot_icrf - plain_icrf) / plain_icrf,
    );

    // Per-trajectory cost this adds to emitting a covariance trajectory: one extra
    // convert_with_rotation call in place of a plain convert, per covariance-bearing sample --
    // av-kernel's own `convert_gmat_trajectory_to_declared_frame` calls this once per sample
    // whose `cov` is non-empty (never per non-covariance sample, and never twice per sample).
    for samples in [145_usize, 1000, 8760] {
        eprintln!(
            "[convert_with_rotation perf] {samples} covariance-bearing samples (e.g. a {}-sample trajectory): +{:.3} ms total (body-fixed), +{:.3} ms total (ICRF), vs. what a mean-only (plain convert) conversion of the same trajectory already costs",
            samples,
            (rot_bf - plain_bf) * samples as f64 / 1000.0,
            (rot_icrf - plain_icrf) * samples as f64 / 1000.0,
        );
    }
}

/// A missing or uninitialized `CoordinateSystem` name is a typed `GmatError` for the new
/// function too, never a crash and never a silent identity conversion -- `gmatffi_convert_
/// state_and_rotation` reuses the same `lookup_initialized_coordinate_system` helper `convert.
/// rs`'s equivalent test already exercises for `gmatffi_convert_state`, but this proves the new
/// call site actually reaches it (shared logic does not guarantee a correct new wiring).
#[test]
fn convert_with_rotation_refuses_an_unregistered_coordinate_system_name() {
    let _engine = gmat_sys::engine_lock();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    gmat.coordinate_system("RotRefuseReal", "Earth", "MJ2000Eq").unwrap();
    gmat.initialize().unwrap();

    let state = [7000.0, 0.0, 0.0, 0.0, 0.0, 7.5];
    let err = gmat.convert_with_rotation(31041.5, &state, "RotRefuseReal", "NoSuchCoordinateSystemAtAllForRotation").unwrap_err();
    assert!(err.message.contains("not registered"), "expected a \"not registered\" message, got {err:?}");
}
