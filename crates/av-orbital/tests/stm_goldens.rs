#![cfg(feature = "gmat-frames")]
//! N4 acceptance (`docs/native-dynamics-plan.md`, Priority 3): the native 42-state STM against
//! `goldens/leo_1day_jgm2_8x8_sunmoon.json`'s own `stm` block -- GMAT's own
//! `PropagationStateManager("STM")` propagation of the identical arc (JGM2 8x8 + Luna + Sun, no
//! drag/SRP, one day), the reference this file compares against (ADR-002's second amendment).
//!
//! Mirrors `tests/thirdbody_goldens.rs`'s own fixture pattern exactly (same `Golden` epoch/
//! force-model deserialisation, same `build_native_model`), extended to also deserialise the
//! golden's own `stm` block and to drive [`EarthGravityModel::step_with_stm`] instead of
//! [`EarthGravityModel::step`].
//!
//! **This test file never modifies `goldens/leo_1day_jgm2_8x8_sunmoon.json` or its recorded
//! `tolerance_m`/`tolerance_mps`** (that golden's own tolerance pins the `gmat-sys` depth-2
//! path, not this native depth-3 path). Every tolerance this file asserts at is this file's own,
//! measured below and recorded just above the measured value, per this task's own rule.
use std::path::PathBuf;
use std::time::Instant;

use av_cdm::time::Tai;
use av_dynamics::DynamicsModel;
use av_orbital::cof;
use av_orbital::de::{DeBody, DeEphemeris};
use av_orbital::frame_gmat::GmatBodyFixedRotation;
use av_orbital::stm::det6;
use av_orbital::{EarthGravityModel, EarthGravityModelInfo};
use gmat_sys::Gmat;
use serde::Deserialize;

#[derive(Deserialize)]
struct Golden {
    epoch_a1mjd: f64,
    force_model: ForceModelCfg,
    duration_s: f64,
    initial_state: Vec<f64>,
    final_state: Vec<f64>,
    stm: StmBlock,
}

#[derive(Deserialize)]
struct ForceModelCfg {
    central_body: String,
    gravity: GravityCfg,
    point_masses: Vec<String>,
}

#[derive(Deserialize)]
struct GravityCfg {
    file: String,
    degree: i32,
    order: i32,
}

/// One entry of the `stm` block's own `golden_comparison_tolerance` object (question 230's
/// stm-block follow-up, `goldens/gen_leo_1day.py`'s own `STM_TOLERANCE`). `unit`/`source` are
/// read (and asserted non-empty) only to prove the field parses and to disclose provenance; each
/// check itself only needs `value`.
#[derive(Deserialize)]
struct GoldenComparisonToleranceEntry {
    value: f64,
    unit: String,
    source: String,
}

/// The `stm` block's own six named tolerances -- deliberately nested inside `stm`, not at this
/// golden's own top level, where they would be confusable with `tolerance_m`/`tolerance_mps`
/// (the `gmat-sys` depth-2 path's tolerances, a different comparison entirely; see this file's
/// own module doc comment).
#[derive(Deserialize)]
struct StmGoldenComparisonTolerance {
    traj_tolerance_m: GoldenComparisonToleranceEntry,
    traj_tolerance_mps: GoldenComparisonToleranceEntry,
    stm_max_abs_tolerance: GoldenComparisonToleranceEntry,
    stm_max_rel_tolerance: GoldenComparisonToleranceEntry,
    det_phi_tolerance: GoldenComparisonToleranceEntry,
    cov_rel_tolerance: GoldenComparisonToleranceEntry,
}

#[derive(Deserialize)]
struct StmBlock {
    final_stm: Vec<f64>,
    det_phi_t1: f64,
    p0_si: Vec<f64>,
    cov_t1_si: Vec<f64>,
    // Option, not a bare field with `#[serde(default)]`: absence must fail loudly and by name
    // (below), never silently fall back to a default tolerance (round 1's own review lesson,
    // restated by question 230) -- mirrors crates/av-kernel/tests/expr_goldens.rs's own
    // established shape for this exact field.
    golden_comparison_tolerance: Option<StmGoldenComparisonTolerance>,
}

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../goldens/leo_1day_jgm2_8x8_sunmoon.json")
}

fn load_golden() -> Golden {
    serde_json::from_str(&std::fs::read_to_string(golden_path()).unwrap()).unwrap()
}

fn gravity_path(file_name: &str) -> PathBuf {
    cof::locate_gmat_root().expect("GMAT_ROOT set").join("data/gravity/earth").join(file_name)
}

fn de_path() -> PathBuf {
    DeEphemeris::locate_gmat_root().expect("GMAT_ROOT set").join("data/planetary_ephem/de/leDE1941.405")
}

fn de_body_for(name: &str) -> DeBody {
    match name {
        "Luna" => DeBody::Moon,
        "Sun" => DeBody::Sun,
        other => panic!("golden names a point mass this test does not know how to map: {other}"),
    }
}

/// Question 230's stm-block follow-up: extract one tolerance entry's `value`, after asserting
/// `unit`/`source` are non-empty (a tolerance whose provenance is blank records nothing) --
/// mirrors `crates/av-kernel/tests/expr_goldens.rs`'s own established check for this exact
/// pattern.
fn check_tol(entry: &GoldenComparisonToleranceEntry, name: &str) -> f64 {
    assert!(!entry.unit.is_empty(), "stm.golden_comparison_tolerance.{name}.unit must name the unit the bound is in");
    assert!(!entry.source.is_empty(), "stm.golden_comparison_tolerance.{name}.source must say where the number came from");
    entry.value
}

fn km_state_to_m(state_km: &[f64]) -> [f64; 6] {
    [state_km[0] * 1e3, state_km[1] * 1e3, state_km[2] * 1e3, state_km[3] * 1e3, state_km[4] * 1e3, state_km[5] * 1e3]
}

/// Mirrors `tests/thirdbody_goldens.rs::build_native_model` exactly.
fn build_native_model(golden: &Golden, namespace: &str) -> EarthGravityModel<GmatBodyFixedRotation> {
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let rotation = GmatBodyFixedRotation::new(gmat, &golden.force_model.central_body, namespace).expect("GmatBodyFixedRotation::new");
    let bodies: Vec<DeBody> = golden.force_model.point_masses.iter().map(|n| de_body_for(n)).collect();
    EarthGravityModel::new(
        &gravity_path(&golden.force_model.gravity.file),
        golden.force_model.gravity.degree as usize,
        golden.force_model.gravity.order as usize,
        &golden.force_model.central_body,
        rotation,
        EarthGravityModelInfo { id: "native.orbital.n4_stm_golden_test".to_string(), version: env!("CARGO_PKG_VERSION").to_string(), goldens: vec!["leo_1day_jgm2_8x8_sunmoon".to_string()] },
    )
    .expect("EarthGravityModel construction")
    .with_third_bodies(&de_path(), &bodies)
    .expect("with_third_bodies")
}

/// **The full N4/Priority-3 comparison**, in one test so the (expensive, ~1 day at 1e-12 over
/// the 42-state augmented vector) integration runs exactly once: STM element agreement,
/// `det(Phi)` against the golden's own recorded value, the propagated covariance's agreement
/// with `cov_t1_si` (through `av_dynamics::propagate_covariance`, the SAME symmetrization path
/// `av-kernel`'s own covariance tests use), and the trajectory residual for context.
#[test]
fn native_stm_matches_leo_1day_jgm2_8x8_sunmoon_golden() {
    let _engine = gmat_sys::engine_lock();
    let golden = load_golden();
    let model = build_native_model(&golden, "N4Stm1");

    // Question 230's stm-block follow-up: read all six tolerances from the golden's own "stm"
    // block (goldens/gen_leo_1day.py writes them), never bare Rust constants -- a reader that
    // silently fell back to a default tolerance here would pin nothing (round 1's own review
    // lesson, restated by question 230).
    let tol = golden.stm.golden_comparison_tolerance.as_ref().unwrap_or_else(|| {
        panic!(
            "goldens/leo_1day_jgm2_8x8_sunmoon.json's stm block is missing golden_comparison_tolerance -- regenerate it with goldens/gen_leo_1day.py (question 230: this field must be present, never defaulted)"
        )
    });
    let traj_tolerance_m = check_tol(&tol.traj_tolerance_m, "traj_tolerance_m");
    let traj_tolerance_mps = check_tol(&tol.traj_tolerance_mps, "traj_tolerance_mps");
    let stm_max_abs_tolerance = check_tol(&tol.stm_max_abs_tolerance, "stm_max_abs_tolerance");
    let stm_max_rel_tolerance = check_tol(&tol.stm_max_rel_tolerance, "stm_max_rel_tolerance");
    let det_phi_tolerance = check_tol(&tol.det_phi_tolerance, "det_phi_tolerance");
    let cov_rel_tolerance = check_tol(&tol.cov_rel_tolerance, "cov_rel_tolerance");

    let x0 = km_state_to_m(&golden.initial_state);
    let x1_golden = km_state_to_m(&golden.final_state);
    let t0_tai_ns = Tai::from_a1_mjd(golden.epoch_a1mjd).as_nanos();
    let dt_ns = (golden.duration_s * 1e9).round() as i64;

    // Phi(t0,t0) must be the exact identity through this exact model/path, mirroring the
    // golden's own recorded `max_abs_identity_error_t0 = 0.0`.
    let zero = model.step_with_stm(&x0, t0_tai_ns, &[], 0).expect("zero-duration step_with_stm");
    assert_eq!(zero.phi, {
        let mut id = vec![0.0; 36];
        for i in 0..6 {
            id[i * 6 + i] = 1.0;
        }
        id
    });

    let start = Instant::now();
    let result = model.step_with_stm(&x0, t0_tai_ns, &[], dt_ns).expect("step_with_stm over the full arc");
    let wall_s = start.elapsed().as_secs_f64();

    // --- Trajectory residual, for context (this file's own tolerance, mirroring
    // tests/thirdbody_goldens.rs's own 6e-3 m / 6e-6 m/s -- requesting the STM must not change
    // the physical trajectory at all, since stm_derivatives's own 0..6 block is bit-identical
    // to derivatives, see model.rs's own doc comment). ---
    let dr = (0..3).map(|i| (result.state[i] - x1_golden[i]).powi(2)).sum::<f64>().sqrt();
    let dv = (3..6).map(|i| (result.state[i] - x1_golden[i]).powi(2)).sum::<f64>().sqrt();
    eprintln!(
        "[n4-stm-golden] trajectory residual vs golden final_state: {dr:e} m / {dv:e} m/s, wall time {wall_s:.3} s, tolerance {traj_tolerance_m:e} m / {traj_tolerance_mps:e} m/s"
    );
    // MEASURED: 2.7646366405146665e-3 m / 3.063141046268765e-6 m/s (debug build, this host).
    // Tolerance (read from the golden above) set just above it, matching
    // `tests/thirdbody_goldens.rs`'s own identical-order (never loosened) convention.
    assert!(dr < traj_tolerance_m, "position residual {dr:e} m exceeds the golden's own recorded {traj_tolerance_m:e} m tolerance");
    assert!(dv < traj_tolerance_mps, "velocity residual {dv:e} m/s exceeds the golden's own recorded {traj_tolerance_mps:e} m/s tolerance");

    // --- STM element agreement: final_stm is unit-invariant (position/velocity both scaled by
    // the identical km<->m factor, so d(pos)/d(pos0), d(pos)/d(vel0), etc. all carry the SAME
    // numeric value whether GMAT's own km-internal state or this model's SI state is used --
    // stated and used here, not merely assumed: p0_si/cov_t1_si are SI while final_stm is
    // compared directly with no unit conversion, exactly as `crates/av-kernel/tests/
    // golden_acceptance.rs`'s own `kernel_covariance_matches_the_golden_stm_and_propagated_cov`
    // does for the gmat-sys depth-2 path). ---
    let mut max_abs = 0.0_f64;
    let mut max_rel = 0.0_f64;
    for (got, want) in result.phi.iter().zip(golden.stm.final_stm.iter()) {
        let abs_err = (got - want).abs();
        let rel_err = abs_err / want.abs().max(1e-9);
        max_abs = max_abs.max(abs_err);
        max_rel = max_rel.max(rel_err);
    }
    eprintln!(
        "[n4-stm-golden] STM (36 elements) vs GMAT's own final_stm: max abs = {max_abs:e}, max relative = {max_rel:e}, tolerance {stm_max_abs_tolerance:e} / {stm_max_rel_tolerance:e}"
    );
    // MEASURED (debug build, this host, the golden's own arc, `--nocapture`): max abs
    // 6.392269142452278e-5, max relative 5.58039616036866e-7 -- essentially the SAME order as
    // round 1's own gmat-sys depth-2 reference point (ADR-002 second amendment: STM max abs
    // 6.3e-5, max relative 5.5e-7, driven by GMAT's OWN A-matrix), even though this native path
    // uses an entirely independently-computed A-matrix (this crate's own gravity-gradient/
    // third-body partials, never GMAT's). Tolerance (read from the golden above) set just above
    // the measured value, per this task's own rule -- NEVER loosened past what was measured.
    assert!(max_abs < stm_max_abs_tolerance, "STM max abs disagreement {max_abs:e} exceeds the golden's own recorded {stm_max_abs_tolerance:e} tolerance");
    assert!(max_rel < stm_max_rel_tolerance, "STM max relative disagreement {max_rel:e} exceeds the golden's own recorded {stm_max_rel_tolerance:e} tolerance");

    // --- det(Phi) (Liouville) against the golden's own recorded value. ---
    let det = det6(&result.phi);
    let det_err = (det - golden.stm.det_phi_t1).abs();
    eprintln!(
        "[n4-stm-golden] det(Phi): ours = {det:.12}, golden's (GMAT) = {:.12}, |diff| = {det_err:e}, tolerance {det_phi_tolerance:e}",
        golden.stm.det_phi_t1
    );
    // MEASURED: ours = 1.000000000103, golden's (GMAT) = 1.000000000091, |diff| =
    // 1.262279170077818e-11. Tolerance (read from the golden above) set just above it.
    assert!(det_err < det_phi_tolerance, "det(Phi) disagreement {det_err:e} exceeds the golden's own recorded {det_phi_tolerance:e} tolerance");
    // A separate, fixed Liouville sanity floor (not part of the moved golden_comparison_tolerance
    // six -- this bounds det(Phi) itself against 1.0, not the native-vs-GMAT disagreement the
    // assertion above bounds).
    assert!((det - 1.0).abs() < 1e-9, "det(Phi) = {det} is not near 1 on this conservative arc (Liouville)");

    // --- Propagated covariance, through av_dynamics::propagate_covariance (the SAME
    // symmetrization path av-kernel's own covariance tests use -- ADR-002's own rule that the
    // symmetrization is explicit and shared, not reimplemented per caller). ---
    let (cov, asym) = av_dynamics::propagate_covariance(&result.phi, &golden.stm.p0_si, 6);
    let cov_err: f64 = cov.iter().zip(golden.stm.cov_t1_si.iter()).map(|(a, b)| (a - b).powi(2)).sum::<f64>().sqrt();
    let golden_cov_norm: f64 = golden.stm.cov_t1_si.iter().map(|v| v.powi(2)).sum::<f64>().sqrt();
    let cov_rel_err = cov_err / golden_cov_norm;
    eprintln!(
        "[n4-stm-golden] propagated covariance vs golden cov_t1_si: Frobenius error = {cov_err:e} (relative {cov_rel_err:e}), pre-symmetrization asymmetry = {asym:e}, tolerance {cov_rel_tolerance:e}"
    );
    // MEASURED: Frobenius error 8.45315391856248e-1 (relative 5.748873148206185e-10). Tolerance
    // (read from the golden above) set just above it.
    assert!(cov_rel_err < cov_rel_tolerance, "covariance relative error {cov_rel_err:e} exceeds the golden's own recorded {cov_rel_tolerance:e} tolerance");
}
