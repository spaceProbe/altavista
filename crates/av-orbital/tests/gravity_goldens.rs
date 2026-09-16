#![cfg(feature = "gmat-frames")]
//! Golden acceptance tests for [`av_orbital::EarthGravityModel`] against the two new N1
//! goldens `leo_1day_jgm2_8x8` and `leo_6h_egm96_70x70` (`docs/native-dynamics-plan.md`
//! milestone N1): the native model's trajectory residual against GMAT's own propagator, and
//! -- to isolate the force-model difference from the integrator's own cost -- the
//! acceleration-level agreement against GMAT's own `GetDerivatives` at several epochs, for
//! the identical state and force model.
//!
//! Both goldens have no third bodies, no drag, no SRP, no relativity and no tides -- exactly
//! N1's own scope (point-mass/spherical-harmonic gravity only) -- so unlike
//! `leo_1day_jgm2_8x8_sunmoon.json` (the P0 golden, which needs the Sun and Moon this crate
//! does not yet model, N2's scope) these can be pinned by [`av_orbital::EarthGravityModel`]
//! directly.
//!
//! Gated on the whole file (not per-test), matching `tests/frame_gmat.rs`'s own convention,
//! so `cargo test -p av-orbital --no-default-features` compiles this into an empty test
//! binary rather than failing to link `gmat-sys` at all.
use std::path::PathBuf;
use std::time::Instant;

use av_cdm::time::Tai;
use av_dynamics::DynamicsModel;
use av_orbital::cof;
use av_orbital::frame_gmat::GmatBodyFixedRotation;
use av_orbital::{EarthGravityModel, EarthGravityModelInfo};
use gmat_sys::Gmat;
use serde::Deserialize;

#[derive(Deserialize)]
struct Golden {
    name: String,
    epoch_utc: String,
    epoch_a1mjd: f64,
    spacecraft: std::collections::BTreeMap<String, f64>,
    force_model: ForceModelCfg,
    duration_s: f64,
    initial_state: Vec<f64>,
    final_state: Vec<f64>,
    /// The tolerance the golden itself records, which is the tolerance actually in force
    /// (ADR-002's goldens rule: the tolerance is committed WITH the golden). Both goldens were
    /// regenerated through their own scripts, with the reason recorded in the file, once the
    /// residual below had been measured -- so these fields are the measured tolerance, not a
    /// generation default, and a test never carries a second copy of the number that could
    /// drift away from the file's.
    tolerance_m: f64,
    tolerance_mps: f64,
}

#[derive(Deserialize)]
struct ForceModelCfg {
    central_body: String,
    gravity: GravityCfg,
}

#[derive(Deserialize)]
struct GravityCfg {
    file: String,
    degree: i32,
    order: i32,
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../goldens").join(format!("{name}.json"))
}

fn load_golden(name: &str) -> Golden {
    serde_json::from_str(&std::fs::read_to_string(golden_path(name)).unwrap()).unwrap()
}

fn gravity_path(file_name: &str) -> PathBuf {
    cof::locate_gmat_root().expect("GMAT_ROOT set (this task's own environment rule)").join("data/gravity/earth").join(file_name)
}

/// SI metres/seconds from the golden's native km/km-s.
fn km_state_to_m(state_km: &[f64]) -> [f64; 6] {
    [
        state_km[0] * 1e3, state_km[1] * 1e3, state_km[2] * 1e3,
        state_km[3] * 1e3, state_km[4] * 1e3, state_km[5] * 1e3,
    ]
}

fn build_native_model(golden: &Golden, namespace: &str) -> EarthGravityModel<GmatBodyFixedRotation> {
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let rotation = GmatBodyFixedRotation::new(gmat, &golden.force_model.central_body, namespace).expect("GmatBodyFixedRotation::new");
    EarthGravityModel::new(
        &gravity_path(&golden.force_model.gravity.file),
        golden.force_model.gravity.degree as usize,
        golden.force_model.gravity.order as usize,
        &golden.force_model.central_body,
        rotation,
        EarthGravityModelInfo {
            id: format!("native.orbital.{}", golden.name),
            version: env!("CARGO_PKG_VERSION").to_string(),
            goldens: vec![golden.name.clone()],
        },
    )
    .expect("EarthGravityModel construction")
}

/// Builds a GMAT `DerivativeModel` bound to the IDENTICAL force model as the golden (same
/// gravity file/degree/order, same central body, no third bodies/drag/SRP) and a spacecraft
/// seeded with the golden's own Keplerian elements -- mirrors `crates/gmat-sys/tests/
/// leo_golden.rs`'s own construction exactly, so `model.state()` reproduces `golden.
/// initial_state` (checked below) the same way that test checks it.
fn build_gmat_derivative_model(gmat: &Gmat, golden: &Golden, namespace: &str) -> gmat_sys::DerivativeModel {
    let sat = gmat.construct("Spacecraft", &format!("AccelSat{namespace}")).unwrap();
    sat.set_str("DateFormat", "UTCGregorian").unwrap();
    sat.set_str("Epoch", &golden.epoch_utc).unwrap();
    sat.set_str("CoordinateSystem", "EarthMJ2000Eq").unwrap();
    sat.set_str("DisplayStateType", "Keplerian").unwrap();
    for (k, v) in &golden.spacecraft {
        sat.set_real(k, *v).unwrap();
    }
    let fm = gmat.construct("ForceModel", &format!("AccelFM{namespace}")).unwrap();
    fm.set_str("CentralBody", &golden.force_model.central_body).unwrap();
    let grav = gmat.construct("GravityField", "").unwrap();
    grav.set_str("BodyName", &golden.force_model.central_body).unwrap();
    grav.set_str("PotentialFile", &golden.force_model.gravity.file).unwrap();
    grav.set_int("Degree", golden.force_model.gravity.degree).unwrap();
    grav.set_int("Order", golden.force_model.gravity.order).unwrap();
    fm.add_force(&grav).unwrap();
    gmat.initialize().unwrap();
    gmat.derivative_model(&fm, &sat).expect("derivative model")
}

/// Measures the position/velocity residual between the native model's own `Dopri5`-integrated
/// trajectory (seeded from the golden's `initial_state`, its own default integrator settings
/// -- `rtol = atol = 1e-12`, `initial_step = 30 s`, `max_step = 600 s`) and the golden's
/// `final_state` (GMAT's own `PrinceDormand78` propagation at `Accuracy = 1e-13`), prints it,
/// then returns it for the caller to assert against a tolerance set from the measurement --
/// this task's own rule: measure first, set the tolerance just above the measured value,
/// never the reverse.
fn measure_trajectory_residual(golden_name: &str, namespace: &str) -> (f64, f64, f64) {
    let _engine = gmat_sys::engine_lock();
    let golden = load_golden(golden_name);
    let model = build_native_model(&golden, namespace);

    let x0 = km_state_to_m(&golden.initial_state);
    let x1_golden = km_state_to_m(&golden.final_state);
    let t0_tai_ns = Tai::from_a1_mjd(golden.epoch_a1mjd).as_nanos();
    let dt_ns = (golden.duration_s * 1e9).round() as i64;

    let start = Instant::now();
    let result = model.step(&x0, t0_tai_ns, &[], dt_ns).expect("step");
    let wall_s = start.elapsed().as_secs_f64();

    let dr = (0..3).map(|i| (result.state[i] - x1_golden[i]).powi(2)).sum::<f64>().sqrt();
    let dv = (3..6).map(|i| (result.state[i] - x1_golden[i]).powi(2)).sum::<f64>().sqrt();
    eprintln!(
        "[{golden_name}] native Dopri5 vs GMAT PrinceDormand78 over {} s: position residual = {dr:.6e} m, velocity residual = {dv:.6e} m/s, wall time {wall_s:.3} s",
        golden.duration_s
    );
    (dr, dv, wall_s)
}

/// **Acceleration-level agreement**, isolated from any integration error (this task's own
/// rule: "that is by far the fastest way to find the cause"). For the IDENTICAL state
/// (`golden.initial_state`, unchanged -- only the epoch varies) and the IDENTICAL force
/// model, compares [`av_orbital::model::EarthGravityModel::derivatives`]'s acceleration
/// (`state_dot[3..6]`) against GMAT's own `DerivativeModel::derivatives`'s acceleration, at
/// four epochs spanning the arc (t0, +1/3, +2/3, +duration) -- fixing the position and
/// varying only the epoch isolates the body-fixed ROTATION's epoch-dependence from any
/// position-dependence, mirroring `tests/frame_gmat.rs`'s own fixed-position,
/// varying-epoch convention. Returns the max absolute (m/s^2) and max relative acceleration
/// disagreement over the four epochs.
fn measure_acceleration_agreement(golden_name: &str, namespace: &str) -> (f64, f64) {
    let _engine = gmat_sys::engine_lock();
    let golden = load_golden(golden_name);
    let native = build_native_model(&golden, namespace);
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup (idempotent)");
    let gmat_model = build_gmat_derivative_model(&gmat, &golden, namespace);

    // Sanity: the GMAT derivative model's own initial state reproduces the golden's, exactly
    // as `crates/gmat-sys/tests/leo_golden.rs` checks for the sunmoon golden.
    let gmat_x0 = gmat_model.state().unwrap();
    for (a, b) in gmat_x0.iter().zip(&golden.initial_state) {
        assert!((a - b).abs() < 1e-9, "GMAT derivative-model initial state differs from the golden: {gmat_x0:?} vs {:?}", golden.initial_state);
    }

    let x0_m = km_state_to_m(&golden.initial_state);
    let t0_tai_ns = Tai::from_a1_mjd(golden.epoch_a1mjd).as_nanos();

    let mut max_abs = 0.0_f64;
    let mut max_rel = 0.0_f64;
    for frac in [0.0, 1.0 / 3.0, 2.0 / 3.0, 1.0] {
        let dt_s = golden.duration_s * frac;
        let dt_ns = (dt_s * 1e9).round() as i64;
        let t_tai_ns = t0_tai_ns + dt_ns;

        let mut native_dot = [0.0_f64; 6];
        native.derivatives(&x0_m, t_tai_ns, &[], &mut native_dot).expect("native derivatives");
        let native_accel = [native_dot[3], native_dot[4], native_dot[5]];

        let gmat_dot = gmat_model.derivatives(&golden.initial_state, dt_s).expect("GMAT derivatives");
        let gmat_accel = [gmat_dot[3] * 1e3, gmat_dot[4] * 1e3, gmat_dot[5] * 1e3]; // km/s^2 -> m/s^2

        let abs_diff = (0..3).map(|i| (native_accel[i] - gmat_accel[i]).powi(2)).sum::<f64>().sqrt();
        let scale = (0..3).map(|i| gmat_accel[i].powi(2)).sum::<f64>().sqrt();
        let rel_diff = abs_diff / scale;
        eprintln!(
            "[{golden_name}] accel @ dt={dt_s:.1}s: native={native_accel:?} gmat={gmat_accel:?} |diff|={abs_diff:.6e} m/s^2 (relative {rel_diff:.6e})"
        );
        max_abs = max_abs.max(abs_diff);
        max_rel = max_rel.max(rel_diff);
    }
    eprintln!("[{golden_name}] acceleration agreement over 4 epochs: max |diff| = {max_abs:.6e} m/s^2, max relative = {max_rel:.6e}");
    (max_abs, max_rel)
}

/// The P0 spike's own reference point (ADR-002, first amendment): our `Dopri5` driving
/// GMAT's OWN derivatives cost 5.7 mm of position residual over one day -- what the
/// integrator alone costs. This crate's own residual is that cost PLUS every difference
/// between the native force model and GMAT's, so it is compared against, never asserted
/// equal to, this reference.
const P0_SPIKE_INTEGRATOR_ONLY_RESIDUAL_M: f64 = 5.7e-3;

/// Golden A: `leo_1day_jgm2_8x8` -- JGM2 8x8, no third bodies, one day. **Measured** (debug
/// build, `--nocapture`): position residual 4.557868e-3 m, velocity residual 5.050580e-6
/// m/s -- 0.8x the P0 spike's 5.7 mm/day integrator-only reference, i.e. essentially THE SAME
/// order of magnitude as "the integrator alone", not larger. `leo_1day_jgm2_8x8_acceleration_
/// agreement` (below) confirms why: the force model itself agrees with GMAT's own
/// `GetDerivatives` to ~4e-15 m/s^2 absolute (~5e-16 relative, machine-precision noise), so
/// this trajectory residual is essentially ALL integrator-family disagreement (this model's
/// `Dopri5` default, rtol=atol=1e-12, vs GMAT's `PrinceDormand78` at 1e-13 -- two different
/// Runge-Kutta pairs at similar-but-not-identical tolerances), not a force-model defect.
/// Tolerance set just above the measured value (this task's own rule), never loosened without
/// a measurement.
#[test]
fn leo_1day_jgm2_8x8_trajectory_residual() {
    let (dr, dv, _wall_s) = measure_trajectory_residual("leo_1day_jgm2_8x8", "TrajA");
    eprintln!(
        "[leo_1day_jgm2_8x8] residual vs P0 spike's 5.7 mm/day integrator-only reference: {:.1}x",
        dr / P0_SPIKE_INTEGRATOR_ONLY_RESIDUAL_M
    );
    // The tolerance in force is the one the golden itself records (6e-3 m / 6e-6 m/s, set from
    // the measured 4.557868e-3 m / 5.050580e-6 m/s when the golden was regenerated through
    // `goldens/gen_leo_1day_jgm2_8x8.py` with that reason). Read, never re-typed here.
    let golden = load_golden("leo_1day_jgm2_8x8");
    assert!(dr < golden.tolerance_m, "position residual {dr:e} m exceeds the golden's recorded tolerance {:e} m", golden.tolerance_m);
    assert!(dv < golden.tolerance_mps, "velocity residual {dv:e} m/s exceeds the golden's recorded tolerance {:e} m/s", golden.tolerance_mps);
}

/// Golden A's acceleration-level agreement -- isolates the force-model/rotation difference
/// from the integrator entirely (this task's own rule: measure this FIRST, it is "by far the
/// fastest way to find the cause" of any large trajectory residual). **Measured**: max abs
/// disagreement 3.972066e-15 m/s^2, max relative 4.698088e-16, over four epochs spanning the
/// arc (t0, +1/3, +2/3, +1 day) at the fixed initial position -- machine-precision agreement
/// (f64 has ~2.2e-16 relative precision per operation; a ~5e5-term recursion accumulating to
/// ~5e-16 relative is exactly the rounding-error scale expected, not a modelling difference).
/// This is the evidence that no root-cause hunt was needed for this golden: the force model
/// and rotation are correct to the last bit that matters, and the trajectory residual above is
/// the integrator's own cost.
#[test]
fn leo_1day_jgm2_8x8_acceleration_agreement() {
    let (max_abs, max_rel) = measure_acceleration_agreement("leo_1day_jgm2_8x8", "AccelA");
    const TOLERANCE_ABS_M_S2: f64 = 1e-13; // measured 3.972066e-15 m/s^2
    const TOLERANCE_REL: f64 = 1e-13; // measured 4.698088e-16
    assert!(max_abs < TOLERANCE_ABS_M_S2, "max abs acceleration disagreement {max_abs:e} m/s^2 exceeds {TOLERANCE_ABS_M_S2:e}");
    assert!(max_rel < TOLERANCE_REL, "max relative acceleration disagreement {max_rel:e} exceeds {TOLERANCE_REL:e}");
}

/// Golden B: `leo_6h_egm96_70x70` -- EGM96 truncated to 70x70, no third bodies, six hours.
/// Expensive (this task's own cost note: ~5,000 terms per evaluation, tens of thousands of
/// evaluations over six hours) but **measured** debug-build wall time for the `step` call
/// alone was 35.863 s, well under the ~180 s threshold this task's own cost note sets, so
/// this test runs unconditionally (not `#[ignore]`d) -- see this crate's N1 report for the
/// full timing. **Measured** residual: position 2.742649e-4 m, velocity 3.043023e-7 m/s over
/// six hours -- smaller in absolute terms than golden A's one-day residual (a shorter arc has
/// less time for the integrator-family disagreement to accumulate), 0.05x the P0 spike's
/// 5.7 mm/DAY reference (not a fair direct ratio, since this arc is 1/4 the duration, but
/// still far below it in absolute terms). Confirmed by the acceleration-agreement test below
/// to be integrator cost, not a force-model defect, exactly as golden A.
#[test]
fn leo_6h_egm96_70x70_trajectory_residual() {
    let (dr, dv, _wall_s) = measure_trajectory_residual("leo_6h_egm96_70x70", "TrajB");
    eprintln!(
        "[leo_6h_egm96_70x70] residual vs P0 spike's 5.7 mm/day integrator-only reference (6h, not 1 day): {:.1}x",
        dr / P0_SPIKE_INTEGRATOR_ONLY_RESIDUAL_M
    );
    // As golden A: the tolerance in force is the golden's own recorded 5e-4 m / 5e-7 m/s, set
    // from the measured 2.742649e-4 m / 3.043023e-7 m/s when the golden was regenerated through
    // `goldens/gen_leo_6h_egm96_70x70.py` with that reason.
    let golden = load_golden("leo_6h_egm96_70x70");
    assert!(dr < golden.tolerance_m, "position residual {dr:e} m exceeds the golden's recorded tolerance {:e} m", golden.tolerance_m);
    assert!(dv < golden.tolerance_mps, "velocity residual {dv:e} m/s exceeds the golden's recorded tolerance {:e} m/s", golden.tolerance_mps);
}

/// **Measured**: max abs disagreement 3.316145e-14 m/s^2, max relative 3.922327e-15 -- about
/// 8x golden A's absolute disagreement (consistent with EGM96 70x70's ~5,000-term recursion
/// accumulating more floating-point rounding than JGM2 8x8's ~45-term one), still
/// machine-precision noise, not a modelling difference.
#[test]
fn leo_6h_egm96_70x70_acceleration_agreement() {
    let (max_abs, max_rel) = measure_acceleration_agreement("leo_6h_egm96_70x70", "AccelB");
    const TOLERANCE_ABS_M_S2: f64 = 1e-13; // measured 3.316145e-14 m/s^2
    const TOLERANCE_REL: f64 = 1e-13; // measured 3.922327e-15
    assert!(max_abs < TOLERANCE_ABS_M_S2, "max abs acceleration disagreement {max_abs:e} m/s^2 exceeds {TOLERANCE_ABS_M_S2:e}");
    assert!(max_rel < TOLERANCE_REL, "max relative acceleration disagreement {max_rel:e} exceeds {TOLERANCE_REL:e}");
}
