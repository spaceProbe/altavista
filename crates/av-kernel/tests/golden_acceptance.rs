//! The kernel's acceptance test (M2.1): run the golden LEO arc
//! (`goldens/leo_1day_jgm2_8x8_sunmoon.json`) through `av_kernel::Kernel` at 10 Hz output
//! sampling, driving a real `gmat_sys::model::GmatModel`, and check the result against the
//! golden's own recorded tolerance. Takes `gmat_sys::engine_lock()` first, per this
//! repository's existing convention for any test touching GMAT.
//!
//! This is a dev-dependency-only use of `gmat-sys` from `av-kernel`: the crate's own library
//! code (`src/`) never depends on GMAT (see `src/lib.rs`'s module doc).
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

use av_cdm::pb::Interpolation;
use av_dynamics::{DynamicsModel, StmAugmented};
use av_kernel::Kernel;
use gmat_sys::model::{GmatModel, GmatModelInfo};
use gmat_sys::Gmat;
use serde::Deserialize;

#[derive(Deserialize)]
struct Gravity {
    file: String,
    degree: i32,
    order: i32,
}

#[derive(Deserialize)]
struct ForceModel {
    central_body: String,
    gravity: Gravity,
    point_masses: Vec<String>,
}

/// M3.2: GMAT's own 42-state (Cartesian + STM) propagation of the same arc
/// (`goldens/gen_leo_1day.py`'s `"stm"` block) -- the reference `av-kernel`'s independently
/// integrated STM (via `gmat-sys` depth 2) is compared against.
#[derive(Deserialize)]
struct Stm {
    final_stm: Vec<f64>,
    max_abs_identity_error_t0: f64,
    det_phi_t1: f64,
    p0_si: Vec<f64>,
    cov_t1_si: Vec<f64>,
}

#[derive(Deserialize)]
struct Golden {
    name: String,
    epoch_utc: String,
    spacecraft: BTreeMap<String, f64>,
    force_model: ForceModel,
    duration_s: f64,
    initial_state: Vec<f64>,
    final_state: Vec<f64>,
    tolerance_m: f64,
    tolerance_mps: f64,
    stm: Stm,
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../goldens").join(format!("{name}.json"))
}

#[test]
fn kernel_at_10hz_matches_the_golden_arc() {
    let _engine = gmat_sys::engine_lock();
    let golden: Golden = serde_json::from_str(&std::fs::read_to_string(golden_path("leo_1day_jgm2_8x8_sunmoon")).unwrap()).unwrap();

    // --- Build the same GMAT force model / spacecraft as crates/gmat-sys/tests/leo_golden.rs
    // (this test intentionally duplicates that setup rather than sharing it -- av-kernel and
    // gmat-sys are separate crates, gmat-sys's tests/ directory isn't a library others can
    // depend on, and this file's job is to exercise the *kernel*, not to prove GMAT setup
    // works a second time). ---
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let sat = gmat.construct("Spacecraft", "KernelAcceptanceSat").unwrap();
    sat.set_str("DateFormat", "UTCGregorian").unwrap();
    sat.set_str("Epoch", &golden.epoch_utc).unwrap();
    sat.set_str("CoordinateSystem", "EarthMJ2000Eq").unwrap();
    sat.set_str("DisplayStateType", "Keplerian").unwrap();
    for (k, v) in &golden.spacecraft {
        sat.set_real(k, *v).unwrap();
    }
    let fm = gmat.construct("ForceModel", "KernelAcceptanceFM").unwrap();
    fm.set_str("CentralBody", &golden.force_model.central_body).unwrap();
    let grav = gmat.construct("GravityField", "").unwrap();
    grav.set_str("BodyName", &golden.force_model.central_body).unwrap();
    grav.set_str("PotentialFile", &golden.force_model.gravity.file).unwrap();
    grav.set_int("Degree", golden.force_model.gravity.degree).unwrap();
    grav.set_int("Order", golden.force_model.gravity.order).unwrap();
    fm.add_force(&grav).unwrap();
    for body in &golden.force_model.point_masses {
        let pm = gmat.construct("PointMassForce", "").unwrap();
        pm.set_str("BodyName", body).unwrap();
        fm.add_force(&pm).unwrap();
    }
    gmat.initialize().unwrap();
    let derivative_model = gmat.derivative_model(&fm, &sat).expect("derivative model");
    assert_eq!(derivative_model.dimension(), 6);

    // --- Wrap it as a DynamicsModel (the km<->m / A1MJD<->TAI boundary happens inside
    // GmatModel, nowhere in this test). ---
    let mut settings = BTreeMap::new();
    settings.insert("central_body".to_string(), golden.force_model.central_body.clone());
    settings.insert("gravity_file".to_string(), golden.force_model.gravity.file.clone());
    settings.insert("gravity_degree".to_string(), golden.force_model.gravity.degree.to_string());
    settings.insert("gravity_order".to_string(), golden.force_model.gravity.order.to_string());
    settings.insert("point_masses".to_string(), golden.force_model.point_masses.join(","));
    settings.insert("integrator".to_string(), "av_dynamics::integrate::Dopri5 rtol=atol=1e-12".to_string());

    let info = GmatModelInfo {
        id: "gmat.earth.jgm2_8x8.sun_moon".to_string(),
        version: "R2026a".to_string(),
        state_space_id: "gmat.orbital.cartesian6".to_string(),
        frame_id: "EarthMJ2000Eq".to_string(),
        goldens: vec![golden.name.clone()],
        has_relativistic_correction: false,
    };
    let model = GmatModel::new(derivative_model, info, &settings, /*accept_missing_stm_terms=*/ false);

    // GmatModel's own epoch and initial state (SI), read before the model is moved into the
    // kernel below -- this is the model's own bound state, not a re-derivation from the
    // golden JSON's recorded values.
    let t0_tai_ns = model.epoch_tai_ns();
    let x0_si = model.initial_state_si().expect("initial state");

    // Cross-check against the golden's own recorded initial_state (km), same tolerance
    // leo_golden.rs uses for the same comparison.
    let golden_x0_si = av_cdm::units::state_km_to_m(<[f64; 6]>::try_from(golden.initial_state.as_slice()).expect("6-element initial_state"));
    for (a, b) in x0_si.iter().zip(golden_x0_si.iter()) {
        assert!((a - b).abs() < 1e-6, "GmatModel's initial state differs from golden: {x0_si:?} vs {golden_x0_si:?}");
    }

    // --- Drive it through the kernel at 10 Hz (ADR-002 default kernel dynamics rate). ---
    let output_period_ns: i64 = 100_000_000; // 10 Hz
    let mut kernel: Kernel<GmatModel> = Kernel::new(output_period_ns);
    kernel.register_system("leo", output_period_ns, model, t0_tai_ns, x0_si.to_vec());

    let duration_ns = (golden.duration_s * 1e9).round() as i64;
    let end_tai_ns = t0_tai_ns + duration_ns;

    let start = Instant::now();
    let trajectories = kernel.run(t0_tai_ns, end_tai_ns).expect("kernel run");
    let elapsed = start.elapsed();

    let traj = trajectories.get("leo").expect("the \"leo\" system produced a trajectory");
    let expected_samples = (duration_ns / output_period_ns) as usize + 1;
    assert_eq!(traj.samples.len(), expected_samples, "sample count at 10 Hz over the golden's duration");
    assert_eq!(traj.interpolation, Interpolation::HermiteVelocity as i32);
    assert_eq!(traj.samples.first().unwrap().tai_ns, t0_tai_ns);
    let last = traj.samples.last().unwrap();
    assert_eq!(last.tai_ns, end_tai_ns);

    // --- Compare the kernel's final sample against the golden's final_state, in SI, at the
    // golden's own declared tolerance -- the acceptance criterion. ---
    let golden_final_si = av_cdm::units::state_km_to_m(<[f64; 6]>::try_from(golden.final_state.as_slice()).expect("6-element final_state"));
    let dr = (0..3).map(|i| (last.mean[i] - golden_final_si[i]).powi(2)).sum::<f64>().sqrt();
    let dv = (3..6).map(|i| (last.mean[i] - golden_final_si[i]).powi(2)).sum::<f64>().sqrt();

    eprintln!(
        "[av-kernel acceptance] {} 10 Hz samples over {:.1} s of golden arc, {:.1} s wall time; |dr| = {dr:.4} m (tol {}), |dv| = {dv:.3e} m/s (tol {})",
        traj.samples.len(),
        golden.duration_s,
        elapsed.as_secs_f64(),
        golden.tolerance_m,
        golden.tolerance_mps
    );

    assert!(dr < golden.tolerance_m, "kernel position error {dr} m exceeds golden tolerance {} m", golden.tolerance_m);
    assert!(dv < golden.tolerance_mps, "kernel velocity error {dv} m/s exceeds golden tolerance {} m/s", golden.tolerance_mps);
}

/// M3.2: `Kernel<StmAugmented<GmatModel>>::run_with_covariance` over the same golden arc,
/// wrapping a 42-state (`Gmat::derivative_model_with_stm`) `GmatModel` instead of the plain
/// 6-state one above, pinned against `goldens/leo_1day_jgm2_8x8_sunmoon.json`'s `"stm"` block
/// (GMAT's own 42-state propagation of the same arc -- the reference, per the ADR-002 second
/// amendment's design constraint: the kernel integrates its own STM via `GetDerivatives`, it
/// never reads GMAT's `Spacecraft` STM back after the fact).
///
/// Registered at `period_ns = MaxStep = 600 s` (not the 10 Hz output rate the plain test
/// above uses) rather than an unrelated performance shortcut: `run_with_covariance` requires
/// a covariance-requesting system's period to equal the kernel's own output rate exactly
/// (`crate::kernel::Kernel::run_with_covariance`'s doc comment -- `hermite_velocity` has no
/// interpolation contract for the STM block, so any mismatch would silently pass-through
/// interpolate a physically meaningless Phi), and 600 s is this golden's own `MaxStep` --
/// a natural, honest choice of "the rate this system is declared at", not tuned to make the
/// test fast. It happens to also be far cheaper than replaying at 10 Hz.
#[test]
fn kernel_covariance_matches_the_golden_stm_and_propagated_cov() {
    let _engine = gmat_sys::engine_lock();
    let golden: Golden = serde_json::from_str(&std::fs::read_to_string(golden_path("leo_1day_jgm2_8x8_sunmoon")).unwrap()).unwrap();

    // Self-consistency, no GMAT call needed: `av_dynamics::propagate_covariance` fed the
    // golden's own recorded Phi(t0,t1) (`goldens/gen_leo_1day.py`'s `_propagate_covariance`,
    // an independent Python implementation of the same three-line formula) must reproduce the
    // golden's own recorded `cov_t1_si` -- this cross-checks the Rust matrix math directly
    // against Python-computed reference values sharing nothing but the formula and GMAT's own
    // STM, so a row/column-major bug in either implementation would show up here even before
    // the full kernel run below is driven.
    let (cov_from_golden_stm, _asym) = av_dynamics::propagate_covariance(&golden.stm.final_stm, &golden.stm.p0_si, 6);
    for (a, b) in cov_from_golden_stm.iter().zip(golden.stm.cov_t1_si.iter()) {
        let scale = b.abs().max(1.0);
        assert!((a - b).abs() / scale < 1e-6, "av_dynamics::propagate_covariance(golden.stm.final_stm, golden.stm.p0_si) disagrees with the golden's own recorded cov_t1_si: {a} vs {b}");
    }

    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let sat = gmat.construct("Spacecraft", "KernelCovarianceSat").unwrap();
    sat.set_str("DateFormat", "UTCGregorian").unwrap();
    sat.set_str("Epoch", &golden.epoch_utc).unwrap();
    sat.set_str("CoordinateSystem", "EarthMJ2000Eq").unwrap();
    sat.set_str("DisplayStateType", "Keplerian").unwrap();
    for (k, v) in &golden.spacecraft {
        sat.set_real(k, *v).unwrap();
    }
    let fm = gmat.construct("ForceModel", "KernelCovarianceFM").unwrap();
    fm.set_str("CentralBody", &golden.force_model.central_body).unwrap();
    let grav = gmat.construct("GravityField", "").unwrap();
    grav.set_str("BodyName", &golden.force_model.central_body).unwrap();
    grav.set_str("PotentialFile", &golden.force_model.gravity.file).unwrap();
    grav.set_int("Degree", golden.force_model.gravity.degree).unwrap();
    grav.set_int("Order", golden.force_model.gravity.order).unwrap();
    fm.add_force(&grav).unwrap();
    for body in &golden.force_model.point_masses {
        let pm = gmat.construct("PointMassForce", "").unwrap();
        pm.set_str("BodyName", body).unwrap();
        fm.add_force(&pm).unwrap();
    }
    gmat.initialize().unwrap();
    let derivative_model = gmat.derivative_model_with_stm(&fm, &sat).expect("42-state derivative model");
    assert_eq!(derivative_model.dimension(), 42);

    let mut settings = BTreeMap::new();
    settings.insert("central_body".to_string(), golden.force_model.central_body.clone());
    settings.insert("gravity_file".to_string(), golden.force_model.gravity.file.clone());
    settings.insert("gravity_degree".to_string(), golden.force_model.gravity.degree.to_string());
    settings.insert("gravity_order".to_string(), golden.force_model.gravity.order.to_string());
    settings.insert("point_masses".to_string(), golden.force_model.point_masses.join(","));
    settings.insert("stm".to_string(), "true".to_string());
    let info = GmatModelInfo {
        id: "gmat.earth.jgm2_8x8.sun_moon.stm".to_string(),
        version: "R2026a".to_string(),
        state_space_id: "gmat.orbital.cartesian6".to_string(),
        frame_id: "EarthMJ2000Eq".to_string(),
        goldens: vec![golden.name.clone()],
        has_relativistic_correction: false,
    };
    let model = GmatModel::new(derivative_model, info, &settings, /*accept_missing_stm_terms=*/ false);
    assert!(model.stm_capable());
    let t0_tai_ns = model.epoch_tai_ns();
    let x0_si = model.initial_state_si().expect("initial state");

    // Independent identity check on the *unwrapped* model, before StmAugmented::new consumes
    // it: Phi(t0,t0) = I via a zero-duration step_with_stm, matching the golden's own
    // `max_abs_identity_error_t0`.
    let zero = model.step_with_stm(&x0_si, t0_tai_ns, &[], 0).expect("zero-duration step_with_stm");
    let max_abs_identity_error_t0 = (0..6)
        .flat_map(|row| (0..6).map(move |col| (row, col)))
        .map(|(row, col)| (zero.phi[row * 6 + col] - if row == col { 1.0 } else { 0.0 }).abs())
        .fold(0.0_f64, f64::max);
    eprintln!("[av-kernel covariance] Phi(t0,t0) max abs identity error: ours = {max_abs_identity_error_t0:.3e}, golden's = {:.3e}", golden.stm.max_abs_identity_error_t0);
    assert_eq!(max_abs_identity_error_t0, 0.0, "Phi(t0,t0) must be the exact identity");

    // --- Drive it through the kernel at period_ns = MaxStep (600 s -- see the doc comment
    // above for why), requesting covariance. ---
    let period_ns: i64 = 600_000_000_000;
    let mut kernel: Kernel<StmAugmented<GmatModel>> = Kernel::new(period_ns);
    let wrapped = StmAugmented::new(model);
    kernel.register_system("leo", period_ns, wrapped, t0_tai_ns, StmAugmented::<GmatModel>::seed(&x0_si));

    let duration_ns = (golden.duration_s * 1e9).round() as i64;
    let end_tai_ns = t0_tai_ns + duration_ns;
    let mut p0 = BTreeMap::new();
    p0.insert("leo".to_string(), golden.stm.p0_si.clone());

    let start = Instant::now();
    let trajectories = kernel.run_with_covariance(t0_tai_ns, end_tai_ns, 6, &p0, false).expect("kernel run_with_covariance");
    let elapsed = start.elapsed();

    let traj = trajectories.get("leo").expect("the \"leo\" system produced a trajectory");
    let expected_samples = (duration_ns / period_ns) as usize + 1;
    assert_eq!(traj.samples.len(), expected_samples);
    let last = traj.samples.last().unwrap();
    assert_eq!(last.tai_ns, end_tai_ns);
    assert_eq!(last.mean.len(), 6, "mean must be truncated back to the physical 6-state, not the 42-state augmented one");
    assert_eq!(last.cov.len(), 36);

    // --- Position/velocity at t1, same tolerance as the plain golden. ---
    let golden_final_si = av_cdm::units::state_km_to_m(<[f64; 6]>::try_from(golden.final_state.as_slice()).expect("6-element final_state"));
    let dr = (0..3).map(|i| (last.mean[i] - golden_final_si[i]).powi(2)).sum::<f64>().sqrt();
    let dv = (3..6).map(|i| (last.mean[i] - golden_final_si[i]).powi(2)).sum::<f64>().sqrt();
    assert!(dr < golden.tolerance_m, "kernel position error {dr} m exceeds golden tolerance {} m", golden.tolerance_m);
    assert!(dv < golden.tolerance_mps, "kernel velocity error {dv} m/s exceeds golden tolerance {} m/s", golden.tolerance_mps);

    // --- Covariance at t1 vs the golden's own P(t1) = Phi(t0,t1) P0 Phi(t0,t1)^T. ---
    let cov_err: f64 = last.cov.iter().zip(golden.stm.cov_t1_si.iter()).map(|(a, b)| (a - b).powi(2)).sum::<f64>().sqrt();
    // Scale: the golden's own covariance entries range from ~1e2 to ~1e9 (SI, position
    // variance compounded over a full day of many-body dynamics dominates); a relative bound
    // against the Frobenius norm of the golden's own covariance is the honest comparison here,
    // not a fixed absolute tolerance picked to make this pass.
    let golden_cov_norm: f64 = golden.stm.cov_t1_si.iter().map(|v| v.powi(2)).sum::<f64>().sqrt();
    let cov_rel_err = cov_err / golden_cov_norm;
    eprintln!(
        "[av-kernel covariance] {} samples over {:.1} s at period {:.0} s, {:.3} s wall time; \
         |dr|={dr:.4} m, |dv|={dv:.3e} m/s; covariance Frobenius error {cov_err:.6e} (rel {cov_rel_err:.3e}); \
         det(Phi) golden={:.12}",
        traj.samples.len(),
        golden.duration_s,
        period_ns as f64 * 1e-9,
        elapsed.as_secs_f64(),
        golden.stm.det_phi_t1,
    );
    assert!(cov_rel_err < 1e-6, "covariance relative error {cov_rel_err:.3e} exceeds 1e-6 of the golden's own covariance norm");

    // Covariance hygiene (docs/open-questions.md question 80): `run_with_covariance` already
    // ran every sample's `cov` through `av_cdm::covariance::check_spd_row_major` internally
    // (it would have returned `Err` above otherwise) -- this re-check on the final sample is
    // an explicit, independent confirmation at the test level that a real, GMAT-propagated
    // covariance from this golden passes the hygiene bar, not just an assumption that
    // `run_with_covariance` succeeding implies it.
    let diag = av_cdm::covariance::check_spd_row_major(&last.cov, 6, "golden_acceptance final sample")
        .expect("the kernel's own propagated P(t1) must pass the SPD hygiene check");
    eprintln!(
        "[av-kernel covariance] final sample smallest Cholesky-diagonal^2 proxy: {:.6e} \
         (upper bound on the true smallest eigenvalue; see CholeskyDiagnostics::min_cholesky_diag_sq)",
        diag.min_cholesky_diag_sq
    );

    // "A system never named in p0 gets no covariance, even under run_with_covariance"
    // (question 11) is pinned with the *per-system* case (one system named in p0, one not,
    // within a single run_with_covariance call) by av-kernel's synthetic, GMAT-free unit test
    // `kernel::tests::run_with_covariance_matches_the_closed_form_rotation_and_leaves_
    // unrequested_systems_empty` -- not repeated here with a second full GMAT binding, which
    // would only re-prove the same kernel-level code path at a much higher cost.
}

/// Builds a 42-state (Cartesian + STM), STM-capable `GmatModel` of the golden's own arc, exactly
/// like `kernel_covariance_matches_the_golden_stm_and_propagated_cov`'s own setup -- factored out
/// so `kernel_covariance_at_a_coarser_instance_period_matches_the_fine_one_at_shared_epochs`
/// below can build two independent instances (fine/coarse) without duplicating the ~30-line GMAT
/// object construction twice inline. `name_suffix` disambiguates GMAT's process-global
/// configuration manager (this module's own doc comment's "GMAT object naming" convention).
fn build_covariance_model(golden: &Golden, gmat: &Gmat, name_suffix: &str) -> GmatModel {
    let sat = gmat.construct("Spacecraft", &format!("KernelCovariance{name_suffix}Sat")).unwrap();
    sat.set_str("DateFormat", "UTCGregorian").unwrap();
    sat.set_str("Epoch", &golden.epoch_utc).unwrap();
    sat.set_str("CoordinateSystem", "EarthMJ2000Eq").unwrap();
    sat.set_str("DisplayStateType", "Keplerian").unwrap();
    for (k, v) in &golden.spacecraft {
        sat.set_real(k, *v).unwrap();
    }
    let fm = gmat.construct("ForceModel", &format!("KernelCovariance{name_suffix}FM")).unwrap();
    fm.set_str("CentralBody", &golden.force_model.central_body).unwrap();
    let grav = gmat.construct("GravityField", "").unwrap();
    grav.set_str("BodyName", &golden.force_model.central_body).unwrap();
    grav.set_str("PotentialFile", &golden.force_model.gravity.file).unwrap();
    grav.set_int("Degree", golden.force_model.gravity.degree).unwrap();
    grav.set_int("Order", golden.force_model.gravity.order).unwrap();
    fm.add_force(&grav).unwrap();
    for body in &golden.force_model.point_masses {
        let pm = gmat.construct("PointMassForce", "").unwrap();
        pm.set_str("BodyName", body).unwrap();
        fm.add_force(&pm).unwrap();
    }
    gmat.initialize().unwrap();
    let derivative_model = gmat.derivative_model_with_stm(&fm, &sat).expect("42-state derivative model");
    assert_eq!(derivative_model.dimension(), 42);

    let mut settings = BTreeMap::new();
    settings.insert("central_body".to_string(), golden.force_model.central_body.clone());
    settings.insert("gravity_file".to_string(), golden.force_model.gravity.file.clone());
    settings.insert("gravity_degree".to_string(), golden.force_model.gravity.degree.to_string());
    settings.insert("gravity_order".to_string(), golden.force_model.gravity.order.to_string());
    settings.insert("point_masses".to_string(), golden.force_model.point_masses.join(","));
    settings.insert("stm".to_string(), "true".to_string());
    let info = GmatModelInfo {
        id: format!("gmat.earth.jgm2_8x8.sun_moon.stm.{name_suffix}"),
        version: "R2026a".to_string(),
        state_space_id: "gmat.orbital.cartesian6".to_string(),
        frame_id: "EarthMJ2000Eq".to_string(),
        goldens: vec![golden.name.clone()],
        has_relativistic_correction: false,
    };
    GmatModel::new(derivative_model, info, &settings, /*accept_missing_stm_terms=*/ false)
}

/// M13.3, "Golden" requirement 4: **the existing covariance golden at a coarser instance period
/// matches the fine one at shared epochs, to a stated tolerance.**
///
/// Two independent `Kernel<StmAugmented<GmatModel>>` runs over the *same* golden arc and the
/// *same* declared `P0`, differing only in the covariance-requesting instance's own native step:
/// "fine" registers at `period_ns = 600 s` (the golden's own `MaxStep`, matching
/// `kernel_covariance_matches_the_golden_stm_and_propagated_cov`'s own equal-rate setup exactly
/// -- both `period_ns` and the kernel's own `output_period_ns` are 600 s there), "coarse"
/// registers at `period_ns = 1200 s` (double) while its kernel still samples every 600 s
/// (`output_period_ns = 600 s`, unchanged) -- exactly M13.3's lifted restriction exercised
/// end-to-end against a real GMAT-propagated day-long arc, not just the synthetic closed-form
/// unit tests in `kernel::tests`.
///
/// **M15.2 (question 116), required test.** This is also this crate's real-GMAT-driven
/// coverage of `TrajectorySample.kind`: the fine run's own period equals its `output_period_ns`
/// exactly, so every one of its samples must be NATIVE; the coarse run's samples must be NATIVE
/// exactly at the epochs shared with the fine run (its own 1200 s native grid) and INTERPOLATED
/// at every other output tick. Fails against an implementation that never threads a
/// classification onto the wire (`kind` reads back `SampleKind::Unspecified`, 0, everywhere) or
/// one that stamps every sample NATIVE regardless of grid alignment -- the latter is exactly the
/// "kind test that would still pass if every sample were stamped NATIVE" case the coarse-run's
/// off-grid `INTERPOLATED` assertion below is written to catch.
///
/// **What is checked, and why it is the honest comparison.** At every output tick that is a
/// multiple of 1200 s (0, 1200, ..., 86400 s -- 73 of the coarse run's 145 samples), `av_kernel::
/// kernel::covariance` must return `Some` for the coarse run and must match the fine run's own
/// `cov` at that identical epoch; at every other output tick (a multiple of 600 s but not of
/// 1200 s), it must return `None` for the coarse run while the fine run's is `Some` there (600 s
/// is on *its* own native grid) -- the "an output epoch not on the instance's grid produces no
/// covariance sample" contract (M13.3 requirement 3), proven against real data, not just a
/// synthetic model. (Question 111, M14.3: this used to read `CovarianceAvailability::Available`/
/// `UnavailableAtThisSample`, classified off an all-NaN sentinel -- both are deleted; `covariance`
/// reads a plain `Option` off the wire sample instead, and there is no NaN anywhere left to
/// classify.)
///
/// **The tolerance, derived before this test was ever run, not fitted to its result.** `Phi(0,
/// 1200s)` is mathematically identical whichever way it is computed (STM composition is exact:
/// `Phi(0,t2) = Phi(t1,t2) Phi(0,t1)` for any `t1` between `t0` and `t2`) -- but the *fine* run
/// computes it as the product of **two** independently-reseeded 600 s local STMs
/// (`av_dynamics::StmAugmented::step`'s own per-native-period reseed-and-compose scheme, see
/// that function's doc comment) while the *coarse* run computes it as **one** continuous 1200 s
/// integration reseeded once. Both use the identical `Dopri5` integrator at the identical
/// declared tolerance (`rtol = atol = 1e-12`, this model's own `"integrator"` setting), but the
/// adaptive step-size sequence Dopri5 actually chooses differs between "one 1200 s integration"
/// and "two composed 600 s integrations" (the same "numerically, this is a different scheme"
/// effect `StmAugmented::step`'s own doc comment already documents for the *plain-vs-covariance*
/// path comparison, applied here to *fine-vs-coarse* instead). At `rtol = atol = 1e-12`, the
/// *local* truncation error per accepted step is bounded at that level; over the tens to
/// low-hundreds of adaptive steps a 600-1200 s span of two-body-dominated LEO dynamics takes,
/// engineering practice bounds accumulated relative error at a small multiple of the local
/// tolerance, not by orders of magnitude -- consistent with what this same codebase already
/// measures for a *harder* comparison (`kernel_covariance_matches_the_golden_stm_and_propagated_
/// cov`'s own Frobenius relative error against an **independently generated** (different
/// language, different GMAT invocation) reference STM: 3.086e-11, per that test's own measured
/// output and `crates/av-kernel/README.md`'s "Covariance" section). This fine-vs-coarse
/// comparison shares the *same* kernel, the *same* Rust integration code, and the *same* GMAT
/// process, differing only in reseed cadence, so its discrepancy is expected to be of the same
/// order or smaller. The bound below (`1e-6` relative to the fine run's own covariance norm) is
/// therefore the same bound every other covariance-vs-reference comparison in this codebase
/// already uses for exactly this `rtol = atol = 1e-12` reasoning (`golden_acceptance.rs`,
/// `tests/drm_executor.rs`) -- not loosened, not invented for this test, and the measured value
/// (expected far below it, like every other use of this bound) is printed via `eprintln!` for
/// the record, exactly like the other covariance tests already do.
#[test]
fn kernel_covariance_at_a_coarser_instance_period_matches_the_fine_one_at_shared_epochs() {
    let _engine = gmat_sys::engine_lock();
    let golden: Golden = serde_json::from_str(&std::fs::read_to_string(golden_path("leo_1day_jgm2_8x8_sunmoon")).unwrap()).unwrap();

    let duration_ns = (golden.duration_s * 1e9).round() as i64;
    let output_period_ns: i64 = 600_000_000_000; // 600 s, shared by both runs
    let fine_period_ns: i64 = 600_000_000_000; // 600 s: matches the golden's own MaxStep
    let coarse_period_ns: i64 = 1_200_000_000_000; // 1200 s: double -- M13.3's lifted restriction

    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");

    let fine_model = build_covariance_model(&golden, &gmat, "Fine");
    let t0_tai_ns = fine_model.epoch_tai_ns();
    let x0_si = fine_model.initial_state_si().expect("initial state");
    let end_tai_ns = t0_tai_ns + duration_ns;
    let mut p0 = BTreeMap::new();
    p0.insert("leo".to_string(), golden.stm.p0_si.clone());

    let mut fine_kernel: Kernel<StmAugmented<GmatModel>> = Kernel::new(output_period_ns);
    fine_kernel.register_system("leo", fine_period_ns, StmAugmented::new(fine_model), t0_tai_ns, StmAugmented::<GmatModel>::seed(&x0_si));
    let fine_start = Instant::now();
    let fine_trajectories = fine_kernel.run_with_covariance(t0_tai_ns, end_tai_ns, 6, &p0, false).expect("fine kernel run_with_covariance");
    let fine_elapsed = fine_start.elapsed();
    let fine_samples = &fine_trajectories["leo"].samples;

    let coarse_model = build_covariance_model(&golden, &gmat, "Coarse");
    // Same epoch/initial state as the fine model -- both derived from the identical golden
    // spacecraft configuration, so this cross-check (rather than assuming) that the two really
    // do start from the same place.
    let coarse_t0_tai_ns = coarse_model.epoch_tai_ns();
    let coarse_x0_si = coarse_model.initial_state_si().expect("initial state");
    assert_eq!(coarse_t0_tai_ns, t0_tai_ns, "fine and coarse models must share the same epoch");
    for (a, b) in coarse_x0_si.iter().zip(x0_si.iter()) {
        assert!((a - b).abs() < 1e-6, "fine and coarse models must share the same initial state: {coarse_x0_si:?} vs {x0_si:?}");
    }

    let mut coarse_kernel: Kernel<StmAugmented<GmatModel>> = Kernel::new(output_period_ns);
    coarse_kernel.register_system("leo", coarse_period_ns, StmAugmented::new(coarse_model), t0_tai_ns, StmAugmented::<GmatModel>::seed(&x0_si));
    let coarse_start = Instant::now();
    let coarse_trajectories = coarse_kernel.run_with_covariance(t0_tai_ns, end_tai_ns, 6, &p0, false).expect("coarse kernel run_with_covariance");
    let coarse_elapsed = coarse_start.elapsed();
    let coarse_samples = &coarse_trajectories["leo"].samples;

    assert_eq!(fine_samples.len(), coarse_samples.len(), "both kernels share the same output_period_ns, so both trajectories must have the same sample count");
    assert_eq!(fine_samples.len(), (duration_ns / output_period_ns) as usize + 1);

    let mut shared_epoch_count = 0usize;
    let mut off_grid_count = 0usize;
    let mut max_rel_err = 0.0f64;
    for (fs, cs) in fine_samples.iter().zip(coarse_samples.iter()) {
        assert_eq!(fs.tai_ns, cs.tai_ns);
        let on_coarse_grid = (cs.tai_ns - t0_tai_ns) % coarse_period_ns == 0;

        // The fine run is registered at exactly its kernel's own output_period_ns, so every one
        // of its samples is on its own native grid -- covariance present everywhere, unchanged
        // from `kernel_covariance_matches_the_golden_stm_and_propagated_cov`'s own contract.
        // (Question 111, M14.3: `av_kernel::kernel::covariance_state`/`CovarianceAvailability`
        // are deleted -- classifying a NaN sentinel this crate no longer ever writes -- replaced
        // by `av_kernel::kernel::covariance`, which reads `Option<&[f64]>` directly off the wire
        // sample.)
        assert!(av_kernel::kernel::covariance(fs).is_some(), "tai_ns {}: fine run's own native grid is every output tick", fs.tai_ns);
        // M15.2 (question 116): the fine run's own period equals output_period_ns exactly, so
        // every one of its samples lands on its own native grid too -- NATIVE at every tick,
        // never INTERPOLATED. Fails against an implementation that never threads a
        // classification onto the wire at all (`kind` would read back `SampleKind::Unspecified`,
        // 0) or one that stamps every sample INTERPOLATED regardless of grid alignment.
        assert_eq!(fs.kind, av_cdm::pb::SampleKind::Native as i32, "tai_ns {}: the fine run's own 600 s grid is every output tick, must be NATIVE", fs.tai_ns);

        if on_coarse_grid {
            shared_epoch_count += 1;
            assert!(av_kernel::kernel::covariance(cs).is_some(), "tai_ns {}: on the coarse instance's own 1200 s native grid", cs.tai_ns);
            // M15.2 (question 116): real GMAT propagation, not the synthetic closed-form model
            // `kernel::tests::run_with_covariance_populates_native_at_the_instance_grid_and_
            // interpolated_between` already covers -- an epoch on the coarse instance's own
            // 1200 s native grid must read back NATIVE.
            assert_eq!(cs.kind, av_cdm::pb::SampleKind::Native as i32, "tai_ns {}: on the coarse instance's own 1200 s native grid, must be NATIVE", cs.tai_ns);
            let err: f64 = fs.cov.iter().zip(cs.cov.iter()).map(|(a, b)| (a - b).powi(2)).sum::<f64>().sqrt();
            let norm: f64 = fs.cov.iter().map(|v| v.powi(2)).sum::<f64>().sqrt();
            let rel_err = if norm > 0.0 { err / norm } else { err };
            max_rel_err = max_rel_err.max(rel_err);
        } else {
            off_grid_count += 1;
            // Question 111: an unavailable sample is now `None`/an empty wire `cov`, never the
            // old all-NaN sentinel -- there is only one requested system ("leo") in this test,
            // so `None` here is unambiguous ("unavailable at this sample", not "not requested").
            assert!(
                av_kernel::kernel::covariance(cs).is_none(),
                "tai_ns {}: off the coarse instance's own 1200 s native grid -- must carry no covariance, not a stale value",
                cs.tai_ns
            );
            // M15.2 (question 116): strictly between two of the coarse instance's own native
            // steps -- must be INTERPOLATED, never NATIVE and never left at the zero-value
            // UNSPECIFIED. This is the assertion an implementation that stamped every sample
            // NATIVE regardless of grid alignment would fail (the exact "kind test that would
            // still pass if every sample were stamped NATIVE" trap the task brief calls out).
            assert_eq!(cs.kind, av_cdm::pb::SampleKind::Interpolated as i32, "tai_ns {}: strictly between two of the coarse instance's own native steps, must be INTERPOLATED", cs.tai_ns);
        }
    }
    assert_eq!(shared_epoch_count, (duration_ns / coarse_period_ns) as usize + 1, "every multiple of 1200 s over the golden's 86400 s duration");
    assert_eq!(off_grid_count, fine_samples.len() - shared_epoch_count);

    eprintln!(
        "[av-kernel covariance] fine (600 s, {:.3} s wall) vs coarse (1200 s, {:.3} s wall) over the golden's {:.0} s arc: \
         {shared_epoch_count} shared (Available) epochs, {off_grid_count} coarse-off-grid (Unavailable) epochs; \
         max covariance relative error at shared epochs: {max_rel_err:.6e} (bound 1e-6, derived from rtol=atol=1e-12 -- see this test's own doc comment)",
        fine_elapsed.as_secs_f64(),
        coarse_elapsed.as_secs_f64(),
        golden.duration_s,
    );
    assert!(max_rel_err < 1e-6, "fine-vs-coarse covariance relative error {max_rel_err:.3e} exceeds the 1e-6 bound derived in this test's own doc comment");
}
