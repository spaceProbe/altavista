//! Per-call cost of GMAT's GetDerivatives through the shim, and the re-initialization cost
//! of building a derivative model. Run: cargo run --release -p gmat-sys --example bench
use gmat_sys::Gmat;
use std::time::Instant;

fn main() {
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let t0 = Instant::now();
    let sat = gmat.construct("Spacecraft", "BenchSat").unwrap();
    sat.set_str("DateFormat", "UTCGregorian").unwrap();
    sat.set_str("Epoch", "01 Jan 2026 00:00:00.000").unwrap();
    sat.set_str("CoordinateSystem", "EarthMJ2000Eq").unwrap();
    sat.set_str("DisplayStateType", "Keplerian").unwrap();
    sat.set_real("SMA", 6878.0).unwrap();
    let fm = gmat.construct("ForceModel", "BenchFM").unwrap();
    fm.set_str("CentralBody", "Earth").unwrap();
    let (deg, name) = (8, "JGM2.cof");
    let grav = gmat.construct("GravityField", "").unwrap();
    grav.set_str("BodyName", "Earth").unwrap();
    grav.set_str("PotentialFile", name).unwrap();
    grav.set_int("Degree", deg).unwrap();
    grav.set_int("Order", deg).unwrap();
    fm.add_force(&grav).unwrap();
    for body in ["Luna", "Sun"] {
        let pm = gmat.construct("PointMassForce", "").unwrap();
        pm.set_str("BodyName", body).unwrap();
        fm.add_force(&pm).unwrap();
    }
    gmat.initialize().unwrap();
    let model = gmat.derivative_model(&fm, &sat).unwrap();
    println!("model build (construct + initialize + bind): {:.1} ms", t0.elapsed().as_secs_f64() * 1e3);

    let x = model.state().unwrap();
    let mut out = vec![0.0; 6];
    let n = 200_000;
    let t1 = Instant::now();
    for i in 0..n {
        model.derivatives_into(&x, (i % 1000) as f64, &mut out).unwrap();
    }
    let dt = t1.elapsed().as_secs_f64();
    println!("GetDerivatives (8x8 gravity + Sun + Moon): {n} calls in {dt:.3} s = {:.2} us/call = {:.0} kHz", 1e6 * dt / n as f64, n as f64 / dt / 1e3);

    // --- 42-state (STM) variant, same force model, for the M1.4 STM spike's per-call cost. ---
    let sat42 = gmat.construct("Spacecraft", "BenchSatStm").unwrap();
    sat42.set_str("DateFormat", "UTCGregorian").unwrap();
    sat42.set_str("Epoch", "01 Jan 2026 00:00:00.000").unwrap();
    sat42.set_str("CoordinateSystem", "EarthMJ2000Eq").unwrap();
    sat42.set_str("DisplayStateType", "Keplerian").unwrap();
    sat42.set_real("SMA", 6878.0).unwrap();
    let fm42 = gmat.construct("ForceModel", "BenchFMStm").unwrap();
    fm42.set_str("CentralBody", "Earth").unwrap();
    let grav42 = gmat.construct("GravityField", "").unwrap();
    grav42.set_str("BodyName", "Earth").unwrap();
    grav42.set_str("PotentialFile", "JGM2.cof").unwrap();
    grav42.set_int("Degree", 8).unwrap();
    grav42.set_int("Order", 8).unwrap();
    fm42.add_force(&grav42).unwrap();
    for body in ["Luna", "Sun"] {
        let pm = gmat.construct("PointMassForce", "").unwrap();
        pm.set_str("BodyName", body).unwrap();
        fm42.add_force(&pm).unwrap();
    }
    gmat.initialize().unwrap();
    let t2 = Instant::now();
    let model42 = gmat.derivative_model_with_stm(&fm42, &sat42).unwrap();
    println!("42-state model build (construct + initialize + bind): {:.1} ms", t2.elapsed().as_secs_f64() * 1e3);
    assert_eq!(model42.dimension(), 42);

    let x42 = model42.state().unwrap();
    let mut out42 = vec![0.0; 42];
    let t3 = Instant::now();
    for i in 0..n {
        model42.derivatives_into(&x42, (i % 1000) as f64, &mut out42).unwrap();
    }
    let dt42 = t3.elapsed().as_secs_f64();
    println!("GetDerivatives, 42-state (8x8 gravity + Sun + Moon + STM): {n} calls in {dt42:.3} s = {:.2} us/call = {:.0} kHz", 1e6 * dt42 / n as f64, n as f64 / dt42 / 1e3);
    println!("42-state / 6-state per-call cost ratio: {:.2}x", dt42 / dt);

    // --- Gmat::convert per-call cost (ADR-002's fourth amendment, question 128, M19.1's own
    // spike rule: "measure and disclose the cost per call"). EarthMJ2000Eq -> EarthICRF, the
    // cheapest real case (a near-constant frame bias, no per-call Earth-rotation recompute) --
    // EarthBodyFixed's own cost is reported separately below since it is a genuinely different,
    // more expensive case (a time-varying rotation GMAT must at least check for staleness on
    // every call, see goldens/gen_bodyfixed_leo_2h.py's own doc comment on
    // Planet::nutationUpdateInterval). ---
    let mj2000eq = gmat.coordinate_system("BenchMj2000Eq", "Earth", "MJ2000Eq").unwrap();
    let icrf = gmat.coordinate_system("BenchIcrf", "Earth", "ICRF").unwrap();
    let body_fixed = gmat.coordinate_system("BenchBodyFixed", "Earth", "BodyFixed").unwrap();
    gmat.initialize().unwrap();
    let _ = (&mj2000eq, &icrf, &body_fixed); // kept alive by GMAT's own configuration manager
    let state_km: [f64; 6] = x.as_slice().try_into().expect("6-state model reports a 6-element state");
    let n_convert = 100_000;

    let t4 = Instant::now();
    for i in 0..n_convert {
        gmat.convert(31041.5 + (i % 1000) as f64 * 1e-6, &state_km, "BenchMj2000Eq", "BenchIcrf").unwrap();
    }
    let dt_icrf = t4.elapsed().as_secs_f64();
    let us_per_call_icrf = 1e6 * dt_icrf / n_convert as f64;
    println!(
        "Gmat::convert, EarthMJ2000Eq -> EarthICRF: {n_convert} calls in {dt_icrf:.3} s = {:.3} us/call = {:.0} kHz",
        us_per_call_icrf,
        n_convert as f64 / dt_icrf / 1e3
    );

    let t5 = Instant::now();
    for i in 0..n_convert {
        gmat.convert(31041.5 + (i % 1000) as f64 * 1e-6, &state_km, "BenchMj2000Eq", "BenchBodyFixed").unwrap();
    }
    let dt_bf = t5.elapsed().as_secs_f64();
    let us_per_call_bf = 1e6 * dt_bf / n_convert as f64;
    println!(
        "Gmat::convert, EarthMJ2000Eq -> EarthBodyFixed: {n_convert} calls in {dt_bf:.3} s = {:.3} us/call = {:.0} kHz ({:.2}x the EarthICRF cost)",
        us_per_call_bf,
        n_convert as f64 / dt_bf / 1e3,
        us_per_call_bf / us_per_call_icrf
    );

    // Cost added to emitting a full trajectory: one convert() call per sample
    // (`crate::drm::executor::convert_gmat_trajectory_to_declared_frame`, M19.1) -- reported for
    // a few representative sample counts rather than one arbitrary number, since this scales
    // linearly and a caller's own DRM may sample at any rate.
    for samples in [1_440usize, 7_200, 86_400] {
        println!(
            "  cost added to a {samples}-sample trajectory: {:.2} ms (EarthICRF) / {:.2} ms (EarthBodyFixed)",
            us_per_call_icrf * samples as f64 / 1e3,
            us_per_call_bf * samples as f64 / 1e3
        );
    }
}
