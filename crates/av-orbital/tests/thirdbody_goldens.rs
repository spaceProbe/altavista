#![cfg(feature = "gmat-frames")]
//! N2 acceptance: [`av_orbital::EarthGravityModel::with_third_bodies`] against the EXISTING
//! P0 golden `leo_1day_jgm2_8x8_sunmoon.json` (JGM2 8x8 + Luna + Sun, one day) --
//! `docs/native-dynamics-plan.md` milestone N2, step 4. Mirrors `tests/gravity_goldens.rs`'s
//! own pattern exactly (acceleration-level agreement against GMAT's `GetDerivatives` first,
//! to isolate the force model from the integrator; trajectory residual second).
//!
//! **This test file never modifies `goldens/leo_1day_jgm2_8x8_sunmoon.json` or its recorded
//! `tolerance_m`/`tolerance_mps`** (that golden's own 0.05 m / 5e-5 m/s pins the `gmat-sys`
//! depth-2 path, not this native depth-3 path -- see this crate's N2 report). The native
//! residual is measured and recorded in THIS file's own constants, set just above the
//! measured value, per this task's own rule.
use std::path::PathBuf;
use std::time::Instant;

use av_cdm::time::Tai;
use av_dynamics::DynamicsModel;
use av_orbital::cof;
use av_orbital::de::{DeBody, DeEphemeris};
use av_orbital::frame_gmat::GmatBodyFixedRotation;
use av_orbital::{EarthGravityModel, EarthGravityModelInfo};
use gmat_sys::Gmat;
use serde::Deserialize;

#[derive(Deserialize)]
struct Golden {
    epoch_a1mjd: f64,
    epoch_utc: String,
    force_model: ForceModelCfg,
    duration_s: f64,
    initial_state: Vec<f64>,
    final_state: Vec<f64>,
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

fn km_state_to_m(state_km: &[f64]) -> [f64; 6] {
    [state_km[0] * 1e3, state_km[1] * 1e3, state_km[2] * 1e3, state_km[3] * 1e3, state_km[4] * 1e3, state_km[5] * 1e3]
}

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
        EarthGravityModelInfo { id: "native.orbital.n2_thirdbody_test".to_string(), version: env!("CARGO_PKG_VERSION").to_string(), goldens: vec!["leo_1day_jgm2_8x8_sunmoon".to_string()] },
    )
    .expect("EarthGravityModel construction")
    .with_third_bodies(&de_path(), &bodies)
    .expect("with_third_bodies")
}

/// Mirrors `tests/gravity_goldens.rs::build_gmat_derivative_model`, extended with
/// `PointMassForce` objects for every body the golden's `point_masses` names (Luna, Sun) --
/// the identical construction `goldens/gen_leo_1day.py::_add_forces` uses.
fn build_gmat_derivative_model(gmat: &Gmat, golden: &Golden, namespace: &str) -> gmat_sys::DerivativeModel {
    let sat = gmat.construct("Spacecraft", &format!("N2AccelSat{namespace}")).unwrap();
    sat.set_str("DateFormat", "UTCGregorian").unwrap();
    sat.set_str("Epoch", &golden.epoch_utc).unwrap();
    sat.set_str("CoordinateSystem", "EarthMJ2000Eq").unwrap();
    // The golden's own initial_state is Cartesian (post-conversion); set it directly rather
    // than round-tripping through Keplerian elements the golden file does not itself carry
    // for this force model's spacecraft record (unlike tests/gravity_goldens.rs, which reads
    // `golden.spacecraft`'s Keplerian fields -- leo_1day_jgm2_8x8_sunmoon.json's `spacecraft`
    // block is the STM spacecraft's ballistic-only record, not orbital elements, so this test
    // seeds Cartesian state directly instead).
    sat.set_str("DisplayStateType", "Cartesian").unwrap();
    for (field, v) in ["X", "Y", "Z", "VX", "VY", "VZ"].iter().zip(&golden.initial_state) {
        sat.set_real(field, *v).unwrap();
    }
    let fm = gmat.construct("ForceModel", &format!("N2AccelFM{namespace}")).unwrap();
    fm.set_str("CentralBody", &golden.force_model.central_body).unwrap();
    let grav = gmat.construct("GravityField", "").unwrap();
    grav.set_str("BodyName", &golden.force_model.central_body).unwrap();
    grav.set_str("PotentialFile", &golden.force_model.gravity.file).unwrap();
    grav.set_int("Degree", golden.force_model.gravity.degree).unwrap();
    grav.set_int("Order", golden.force_model.gravity.order).unwrap();
    fm.add_force(&grav).unwrap();
    for (i, body) in golden.force_model.point_masses.iter().enumerate() {
        let pm = gmat.construct("PointMassForce", &format!("N2PM{namespace}_{i}")).unwrap();
        pm.set_str("BodyName", body).unwrap();
        fm.add_force(&pm).unwrap();
    }
    gmat.initialize().unwrap();
    gmat.derivative_model(&fm, &sat).expect("derivative model")
}

/// Acceleration-level agreement, isolated from the integrator (this task's own rule, and
/// `tests/gravity_goldens.rs`'s own convention): identical state and identical force model
/// (JGM2 8x8 + Luna + Sun), four epochs across the arc (t0, +1/3, +2/3, +duration).
fn measure_acceleration_agreement(namespace: &str) -> (f64, f64) {
    let _engine = gmat_sys::engine_lock();
    let golden = load_golden();
    let native = build_native_model(&golden, namespace);
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup (idempotent)");
    let gmat_model = build_gmat_derivative_model(&gmat, &golden, namespace);

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
        let gmat_accel = [gmat_dot[3] * 1e3, gmat_dot[4] * 1e3, gmat_dot[5] * 1e3];

        let abs_diff = (0..3).map(|i| (native_accel[i] - gmat_accel[i]).powi(2)).sum::<f64>().sqrt();
        let scale = (0..3).map(|i| gmat_accel[i].powi(2)).sum::<f64>().sqrt();
        let rel_diff = abs_diff / scale;
        eprintln!("[n2-thirdbody] accel @ dt={dt_s:.1}s: native={native_accel:?} gmat={gmat_accel:?} |diff|={abs_diff:.6e} m/s^2 (relative {rel_diff:.6e})");
        max_abs = max_abs.max(abs_diff);
        max_rel = max_rel.max(rel_diff);
    }
    eprintln!("[n2-thirdbody] acceleration agreement over 4 epochs: max |diff| = {max_abs:.6e} m/s^2, max relative = {max_rel:.6e}");
    (max_abs, max_rel)
}

fn measure_trajectory_residual(namespace: &str) -> (f64, f64, f64) {
    let _engine = gmat_sys::engine_lock();
    let golden = load_golden();
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
    eprintln!("[n2-thirdbody] native Dopri5 (JGM2 8x8 + Luna + Sun) vs GMAT PrinceDormand78 over {} s: position residual = {dr:.6e} m, velocity residual = {dv:.6e} m/s, wall time {wall_s:.3} s", golden.duration_s);
    (dr, dv, wall_s)
}

/// Acceleration-level agreement -- run first, per this task's own rule ("by far the fastest
/// way to find the cause" of any large trajectory residual). **Measured** (debug build,
/// `--nocapture`), ORIGINALLY (fitted `M_E_OFFSET`, `crate::tdb`'s own now-deleted constant):
/// max abs disagreement 1.206205e-14 m/s^2, max relative 1.426677e-15 over the four epochs.
/// **Re-measured after `crate::tdb` switched to the correct TDB series** (GMAT's own exposed
/// `M_E_OFFSET`, deliberately disagreeing with GMAT's own epoch by up to ~1.96 ms, moving the
/// Moon by ~2 m and the Sun by ~59 m at DE-lookup time -- `crate::tdb`'s own module doc, "Root
/// cause"): max abs disagreement 3.972128e-15 m/s^2, max relative 4.698162e-16 -- if anything
/// slightly SMALLER than before, confirming that a ~2 m / ~59 m position shift at lunar/solar
/// distance is many orders of magnitude below what this acceleration comparison can resolve
/// (the same order N1's own point-mass-only acceleration checks measured,
/// `leo_1day_jgm2_8x8_acceleration_agreement`'s own 3.972066e-15 m/s^2 / 4.698088e-16). This
/// is the evidence the third-body force model (DE ephemeris reader, TAI->TDB conversion,
/// Battin third-body formula, and each body's `mu`) is correct to the last bit that matters,
/// UNCHANGED by the TDB series correction -- no root-cause hunt was needed for this golden,
/// before or after.
#[test]
fn leo_1day_jgm2_8x8_sunmoon_acceleration_agreement() {
    let (max_abs, max_rel) = measure_acceleration_agreement("N2AccelA");
    // Tolerance set just above the measured value (this task's own rule): measured
    // 1.206205e-14 m/s^2 / 1.426677e-15, matching N1's own two goldens' identical-order
    // tolerance (1e-13 / 1e-13).
    const TOLERANCE_ABS_M_S2: f64 = 1e-13;
    const TOLERANCE_REL: f64 = 1e-13;
    assert!(max_abs < TOLERANCE_ABS_M_S2, "max abs acceleration disagreement {max_abs:e} m/s^2 exceeds {TOLERANCE_ABS_M_S2:e}");
    assert!(max_rel < TOLERANCE_REL, "max relative acceleration disagreement {max_rel:e} exceeds {TOLERANCE_REL:e}");
}

/// Trajectory residual against the EXISTING P0 golden's `final_state` -- the golden file and
/// its own `tolerance_m`/`tolerance_mps` are read only for `initial_state`/`final_state`/
/// `epoch_a1mjd`/`duration_s` (never for a tolerance: this test's tolerance is its own,
/// declared below, measured, and never the golden's `gmat-sys`-path 0.05 m). **Measured**,
/// ORIGINALLY (fitted `M_E_OFFSET`): position residual 4.542078e-3 m, velocity residual
/// 5.032951e-6 m/s over one day -- essentially IDENTICAL to N1's own no-third-body residual on
/// the same arc (`leo_1day_jgm2_8x8`'s own 4.557868e-3 m / 5.050580e-6 m/s, actually very
/// slightly SMALLER). **Re-measured after `crate::tdb` switched to the correct TDB series**
/// (see the acceleration-agreement test above for why the ~2 m / ~59 m Moon/Sun position shift
/// this causes is expected to be undetectable here too): position residual 4.547920e-3 m,
/// velocity residual 5.039387e-6 m/s -- a ~0.13% change, still comfortably inside this test's
/// own tolerance below and still the same order as N1's own no-third-body residual, confirming
/// the acceleration-agreement test's own conclusion: this residual is integrator-family
/// disagreement (this model's `Dopri5` default vs GMAT's `PrinceDormand78` at
/// `Accuracy=1e-13`), not a force-model defect, UNCHANGED in kind by the TDB series
/// correction. No further root-cause chain was needed (see this crate's N2 report), and this
/// test's own tolerance (below) did not need to move.
#[test]
fn leo_1day_jgm2_8x8_sunmoon_trajectory_residual() {
    let (dr, dv, _wall_s) = measure_trajectory_residual("N2TrajA");
    // This test's OWN tolerance (never goldens/leo_1day_jgm2_8x8_sunmoon.json's 0.05 m, which
    // pins the gmat-sys depth-2 path) -- originally set just above the measured native
    // residual with the fitted M_E_OFFSET (4.542078e-3 m / 5.032951e-6 m/s); re-measured after
    // the TDB series correction at 4.547920e-3 m / 5.039387e-6 m/s, still comfortably under
    // this same tolerance, so it was NOT moved -- matching N1's own leo_1day_jgm2_8x8 golden's
    // identical-order recorded tolerance (6e-3 m / 6e-6 m/s).
    const TOLERANCE_M: f64 = 6e-3;
    const TOLERANCE_MPS: f64 = 6e-6;
    eprintln!("[n2-thirdbody] native residual vs this test's own tolerance: {dr:e} m / {TOLERANCE_M:e} m, {dv:e} m/s / {TOLERANCE_MPS:e} m/s");
    assert!(dr < TOLERANCE_M, "position residual {dr:e} m exceeds this test's own {TOLERANCE_M:e} m tolerance");
    assert!(dv < TOLERANCE_MPS, "velocity residual {dv:e} m/s exceeds this test's own {TOLERANCE_MPS:e} m/s tolerance");
}
