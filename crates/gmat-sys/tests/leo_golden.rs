//! The P0 spike's exit criterion (ADR-002): our integrator over GMAT's `GetDerivatives`,
//! called from Rust, lands within tolerance of GMAT's own propagator on the pinned golden arc.
use gmat_sys::integrate::Dopri5;
use gmat_sys::Gmat;
use serde::Deserialize;
use std::path::PathBuf;
use std::time::Instant;

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

#[derive(Deserialize)]
struct Golden {
    name: String,
    epoch_utc: String,
    spacecraft: std::collections::BTreeMap<String, f64>,
    force_model: ForceModel,
    duration_s: f64,
    initial_state: Vec<f64>,
    final_state: Vec<f64>,
    tolerance_m: f64,
    tolerance_mps: f64,
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../goldens").join(format!("{name}.json"))
}

#[test]
fn rust_integrator_over_gmat_derivatives_matches_gmat_propagator() {
    let _engine = gmat_sys::engine_lock();
    let golden: Golden = serde_json::from_str(&std::fs::read_to_string(golden_path("leo_1day_jgm2_8x8_sunmoon")).unwrap()).unwrap();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");

    let sat = gmat.construct("Spacecraft", "SpikeSat").unwrap();
    sat.set_str("DateFormat", "UTCGregorian").unwrap();
    sat.set_str("Epoch", &golden.epoch_utc).unwrap();
    sat.set_str("CoordinateSystem", "EarthMJ2000Eq").unwrap();
    sat.set_str("DisplayStateType", "Keplerian").unwrap();
    for (k, v) in &golden.spacecraft {
        sat.set_real(k, *v).unwrap();
    }
    let fm = gmat.construct("ForceModel", "SpikeFM").unwrap();
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
    let model = gmat.derivative_model(&fm, &sat).expect("derivative model");
    assert_eq!(model.dimension(), 6);

    let x0 = model.state().unwrap();
    for (a, b) in x0.iter().zip(&golden.initial_state) {
        assert!((a - b).abs() < 1e-9, "initial state differs from golden: {x0:?} vs {:?}", golden.initial_state);
    }

    // Layout check: d(pos)/dt == velocity.
    let d0 = model.derivatives(&x0, 0.0).unwrap();
    for i in 0..3 {
        assert!((d0[i] - x0[3 + i]).abs() < 1e-12);
    }

    let start = Instant::now();
    let (x1, stats) = Dopri5::default()
        .integrate(|t, x, out| model.derivatives_into(x, t, out), &x0, 0.0, golden.duration_s)
        .expect("integration");
    let elapsed = start.elapsed();

    let dr = (0..3).map(|i| (x1[i] - golden.final_state[i]).powi(2)).sum::<f64>().sqrt() * 1e3;
    let dv = (3..6).map(|i| (x1[i] - golden.final_state[i]).powi(2)).sum::<f64>().sqrt() * 1e3;
    eprintln!(
        "[gmat-sys spike] {}: {} steps, {} rejected, {} GetDerivatives calls in {:.3} s ({:.1} us/call); |dr| = {:.4} m, |dv| = {:.3e} m/s",
        golden.name, stats.steps, stats.rejected, stats.evaluations, elapsed.as_secs_f64(),
        1e6 * elapsed.as_secs_f64() / stats.evaluations as f64, dr, dv
    );
    assert!(dr < golden.tolerance_m, "position error {dr} m exceeds tolerance {} m", golden.tolerance_m);
    assert!(dv < golden.tolerance_mps, "velocity error {dv} m/s exceeds tolerance {} m/s", golden.tolerance_mps);
}

#[test]
fn two_models_in_one_process_do_not_interfere() {
    let _engine = gmat_sys::engine_lock();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let build = |name: &str, sma: f64, inc: f64, with_third_bodies: bool| {
        let sat = gmat.construct("Spacecraft", &format!("Re{name}")).unwrap();
        sat.set_str("DateFormat", "UTCGregorian").unwrap();
        sat.set_str("Epoch", "01 Jan 2026 00:00:00.000").unwrap();
        sat.set_str("CoordinateSystem", "EarthMJ2000Eq").unwrap();
        sat.set_str("DisplayStateType", "Keplerian").unwrap();
        sat.set_real("SMA", sma).unwrap();
        sat.set_real("ECC", 0.001).unwrap();
        sat.set_real("INC", inc).unwrap();
        let fm = gmat.construct("ForceModel", &format!("ReFM{name}")).unwrap();
        fm.set_str("CentralBody", "Earth").unwrap();
        let grav = gmat.construct("GravityField", "").unwrap();
        grav.set_str("BodyName", "Earth").unwrap();
        grav.set_str("PotentialFile", "JGM2.cof").unwrap();
        grav.set_int("Degree", 4).unwrap();
        grav.set_int("Order", 4).unwrap();
        fm.add_force(&grav).unwrap();
        if with_third_bodies {
            let pm = gmat.construct("PointMassForce", "").unwrap();
            pm.set_str("BodyName", "Luna").unwrap();
            fm.add_force(&pm).unwrap();
        }
        gmat.initialize().unwrap();
        gmat.derivative_model(&fm, &sat).unwrap()
    };
    let a = build("A", 6878.0, 51.6, true);
    let b = build("B", 7200.0, 98.0, false);
    let xa = a.state().unwrap();
    let xb = b.state().unwrap();
    let da1 = a.derivatives(&xa, 0.0).unwrap();
    let _ = b.derivatives(&xb, 0.0).unwrap();
    let _ = b.derivatives(&xb, 3600.0).unwrap();
    let da2 = a.derivatives(&xa, 0.0).unwrap();
    assert_eq!(da1, da2, "model A's derivative changed after calls on model B");
    // third-body motion makes A time-dependent; B (gravity only) barely so
    let da_dt = a.derivatives(&xa, 6.0 * 3600.0).unwrap();
    let change: f64 = (3..6).map(|i| (da_dt[i] - da1[i]).powi(2)).sum::<f64>().sqrt();
    assert!(change > 1e-9, "dt not honoured by third-body force: change {change}");
}
