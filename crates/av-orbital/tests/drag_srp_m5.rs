#![cfg(feature = "gmat-frames")]
//! Task 3c (`docs/native-dynamics-plan.md`, "the combined drag+SRP arc, then the second
//! atmosphere"): the full M5 arc -- [`av_orbital::model::EarthGravityModel`] with gravity +
//! Sun + Moon + [`av_orbital::drag`]/[`av_orbital::jacchia_roberts`] + [`av_orbital::srp`] all
//! bound together, flown the full 86,400 s against `goldens/
//! leo_1day_jgm2_8x8_sunmoon_drag_srp.json` -- the golden `tests/srp_goldens.rs`'s own module
//! doc named as deferred ("This crate has no native drag yet (tasks 3b/3c) ... the FULL arc is
//! not attempted here"). `tests/srp_goldens.rs`'s `four_epoch_acceleration_agreement_gravity_
//! sun_moon_srp_no_drag` already isolated gravity+3rd+SRP (no drag) against this SAME golden's
//! initial state at machine precision (6.4e-15 m/s^2); this file adds the missing drag term and
//! flies the whole arc.
//!
//! **This golden's own recorded `tolerance_m`/`tolerance_mps` (0.05 m / 5e-5 m/s) pin the
//! `gmat-sys` FFI path** (round 1's own measurement, referenced by N3's task brief: "the
//! `gmat-sys` shim path flies it to 7 mm position and 3 mm/s velocity") -- **never read or
//! asserted against here**. This file measures the NATIVE model's own residual and asserts at a
//! bound derived from that measurement alone, per this task's own rule ("assert at a bound you
//! measured ... never loosen a tolerance to make a test pass").
//!
//! **Finding: the native residual is much larger than the shim path's 7 mm.** Measured on this
//! host: 68.3 m position / 0.0803 m/s velocity over the full day -- about four orders of
//! magnitude above the FFI path. The dominant attributable cause is the Jacchia-Roberts density
//! disagreement against GMAT's own atmosphere (`tests/drag_goldens.rs`, ~2e-4 relative at this
//! golden's own ~250 km altitude), scaled by this orbit's own drag acceleration -- see
//! `m5_full_arc_trajectory_residual_measured`'s own doc comment for the full arithmetic and an
//! internal consistency check (`dv/dr` matches this orbit's own mean motion to ~1%, the
//! signature of a coherent secular phase error rather than integrator noise). Roughly 70% of
//! the position residual is explained this way; the remainder is recorded as an open gap, not
//! chased further within this task's own effort budget.
//!
//! **Round 3 (question 227) root-caused the Jacchia-Roberts disagreement further** --
//! `tests/drag_goldens.rs`'s own module doc now records the full investigation: the
//! disagreement is NOT a monotonic amplification but a smoothly SIGN-FLIPPING function of
//! altitude, tightly correlated with the Atomic-Oxygen-to-Helium composition crossover
//! (independently confirmed, O dominant below ~900 km, He dominant above ~1000 km) -- this
//! golden's own ~250 km altitude sits deep in the O-dominant, single-signed-bias regime, so the
//! ~2e-4-relative estimate above (and this arc's own 68.3 m/0.0803 m/s residual) is UNCHANGED
//! by that finding. The code itself (`exotherm`/`rho_high`/`rho_cor`) was re-verified
//! line-for-line against GMAT's own source and independently re-derived in Python with
//! identical results; a definitive single-line attribution of the sign-flip's own origin was
//! NOT reached within round 3's effort budget (see `tests/drag_goldens.rs` for exactly what was
//! ruled out and the remaining path). One fix DID land from this round:
//! `crate::jacchia_roberts::exotherm`/`raw_density_g_cm3` now thread the caller's own
//! `CentralBodyGeodetics` through instead of hardcoding Earth's defaults internally -- a real
//! correctness gap, but numerically a no-op for this (Earth-default) golden, so it does not
//! move this arc's own measured residual either.
//!
//! **Weather.** This golden's own generator (`goldens/gen_leo_1day.py`, `_add_forces`) never
//! calls `SetField("F107", ...)` etc. on the `DragForce` it builds -- so the arc was flown at
//! GMAT's own `DragForce` CONSTANT-flux defaults (F10.7=F10.7A=150, Kp=3 --
//! `crate::weather::ConstantWeather::gmat_defaults`, read from GMAT's own source and confirmed
//! live; see `crate::weather`'s own module doc). Both the native model and the GMAT comparison
//! model below use that SAME triple, set explicitly (never relying on either side's implicit
//! default silently drifting out of sync with the other).
use std::path::PathBuf;
use std::time::Instant;

use av_cdm::time::Tai;
use av_dynamics::DynamicsModel;
use av_orbital::cof;
use av_orbital::de::{DeBody, DeEphemeris};
use av_orbital::frame_gmat::GmatBodyFixedRotation;
use av_orbital::jacchia_roberts::WeatherInputs;
use av_orbital::srp::SrpConstants;
use av_orbital::weather::ConstantWeather;
use av_orbital::{EarthGravityModel, EarthGravityModelInfo};
use gmat_sys::Gmat;
use serde::Deserialize;

#[derive(Deserialize)]
struct Golden {
    epoch_utc: String,
    epoch_a1mjd: f64,
    duration_s: f64,
    initial_state: Vec<f64>,
    final_state: Vec<f64>,
    spacecraft: std::collections::BTreeMap<String, f64>,
    force_model: ForceModelCfg,
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
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../goldens/leo_1day_jgm2_8x8_sunmoon_drag_srp.json")
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

/// Builds the FULL native model: gravity + third bodies (Luna, Sun) + SRP + drag
/// (Jacchia-Roberts, GMAT's own constant-flux defaults) -- the complete M5 force model.
fn build_native_model(force_model: &ForceModelCfg, namespace: &str, spacecraft: &std::collections::BTreeMap<String, f64>) -> EarthGravityModel<GmatBodyFixedRotation> {
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let rotation = GmatBodyFixedRotation::new(gmat, &force_model.central_body, namespace).expect("GmatBodyFixedRotation::new");
    let bodies: Vec<DeBody> = force_model.point_masses.iter().map(|n| de_body_for(n)).collect();
    let mass_kg = spacecraft["DryMass"]; // no fuel tank in this golden -- TotalMass == DryMass
    let weather = WeatherInputs::from(ConstantWeather::gmat_defaults());
    EarthGravityModel::new(
        &gravity_path(&force_model.gravity.file),
        force_model.gravity.degree as usize,
        force_model.gravity.order as usize,
        &force_model.central_body,
        rotation,
        EarthGravityModelInfo { id: "native.orbital.m5_drag_srp_test".to_string(), version: env!("CARGO_PKG_VERSION").to_string(), goldens: vec!["leo_1day_jgm2_8x8_sunmoon_drag_srp".to_string()] },
    )
    .expect("EarthGravityModel construction")
    .with_third_bodies(&de_path(), &bodies)
    .expect("with_third_bodies")
    .with_srp(SrpConstants::gmat_earth_defaults(), spacecraft["SRPArea"], spacecraft["Cr"], mass_kg)
    .expect("with_srp")
    .with_drag(av_orbital::AtmosphereChoice::JacchiaRoberts, weather, spacecraft["DragArea"], spacecraft["Cd"], mass_kg)
    .expect("with_drag")
}

/// GMAT's own `Spacecraft`/`ForceModel`/`DerivativeModel` for the SAME full force model:
/// gravity + Luna + Sun (as `PointMassForce`, matching the golden's own generator) + spherical
/// `SolarRadiationPressure` + `DragForce`/`JacchiaRoberts` at GMAT's own constant-flux defaults,
/// set explicitly (see this file's own module doc, "Weather").
fn build_gmat_derivative_model(gmat: &Gmat, epoch_utc: &str, initial_state_km: &[f64], spacecraft: &std::collections::BTreeMap<String, f64>, force_model: &ForceModelCfg, namespace: &str) -> gmat_sys::DerivativeModel {
    let sat = gmat.construct("Spacecraft", &format!("N3M5Sat{namespace}")).unwrap();
    sat.set_str("DateFormat", "UTCGregorian").unwrap();
    sat.set_str("Epoch", epoch_utc).unwrap();
    sat.set_str("CoordinateSystem", "EarthMJ2000Eq").unwrap();
    sat.set_str("DisplayStateType", "Cartesian").unwrap();
    for (field, v) in ["X", "Y", "Z", "VX", "VY", "VZ"].iter().zip(initial_state_km) {
        sat.set_real(field, *v).unwrap();
    }
    // Ballistic set (question 81: a seed is a vehicle) -- every field the golden's own
    // `spacecraft` map carries except the Keplerian elements, which conflict with this
    // spacecraft's own `DisplayStateType = Cartesian` outside a mission sequence (matching
    // `tests/srp_goldens.rs`/`tests/drag_goldens.rs`'s own identical filter).
    for (k, v) in spacecraft {
        if k != "SMA" && k != "ECC" && k != "INC" && k != "RAAN" && k != "AOP" && k != "TA" {
            sat.set_real(k, *v).unwrap();
        }
    }

    let fm = gmat.construct("ForceModel", &format!("N3M5FM{namespace}")).unwrap();
    fm.set_str("CentralBody", &force_model.central_body).unwrap();
    let grav = gmat.construct("GravityField", "").unwrap();
    grav.set_str("BodyName", &force_model.central_body).unwrap();
    grav.set_str("PotentialFile", &force_model.gravity.file).unwrap();
    grav.set_int("Degree", force_model.gravity.degree).unwrap();
    grav.set_int("Order", force_model.gravity.order).unwrap();
    fm.add_force(&grav).unwrap();
    for (i, body) in force_model.point_masses.iter().enumerate() {
        let pm = gmat.construct("PointMassForce", &format!("N3M5PM{namespace}_{i}")).unwrap();
        pm.set_str("BodyName", body).unwrap();
        fm.add_force(&pm).unwrap();
    }
    fm.add_force(&gmat.construct("SolarRadiationPressure", &format!("N3M5SRP{namespace}")).unwrap()).unwrap();

    let df = gmat.construct("DragForce", &format!("N3M5Drag{namespace}")).unwrap();
    df.set_str("AtmosphereModel", "JacchiaRoberts").unwrap();
    df.set_str("HistoricWeatherSource", "ConstantFluxAndGeoMag").unwrap();
    df.set_str("PredictedWeatherSource", "ConstantFluxAndGeoMag").unwrap();
    df.set_real("F107", 150.0).unwrap();
    df.set_real("F107A", 150.0).unwrap();
    df.set_real("MagneticIndex", 3.0).unwrap();
    let atmos = gmat.construct("JacchiaRoberts", &format!("N3M5Atmos{namespace}")).unwrap();
    df.set_reference(&atmos).unwrap();
    fm.add_force(&df).unwrap();

    gmat.initialize().unwrap();
    gmat.derivative_model(&fm, &sat).expect("derivative model")
}

// ---------------------------------------------------------------------------------------------
// Acceleration agreement: the full 5-force model, native vs GetDerivatives, at 4 epochs.
// ---------------------------------------------------------------------------------------------

#[test]
fn five_force_acceleration_agreement_against_get_derivatives() {
    let _engine = gmat_sys::engine_lock();
    let golden = load_golden();
    let native = build_native_model(&golden.force_model, "N3M5Accel4", &golden.spacecraft);

    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup (idempotent)");
    let gmat_model = build_gmat_derivative_model(&gmat, &golden.epoch_utc, &golden.initial_state, &golden.spacecraft, &golden.force_model, "N3M5Accel4");

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
        eprintln!("[n3c-m5-4epoch] dt={dt_s:.1}s native={native_accel:?} gmat={gmat_accel:?} |diff|={abs_diff:.6e} m/s^2 (relative {rel_diff:.6e})");
        max_abs = max_abs.max(abs_diff);
        max_rel = max_rel.max(rel_diff);
    }
    eprintln!("[n3c-m5-4epoch] five-force acceleration agreement over 4 epochs: max |diff| = {max_abs:.6e} m/s^2, max relative = {max_rel:.6e}");

    // Tolerance set just above the measured value (this task's own rule): measured
    // max |diff| = 7.659216e-9 m/s^2, max relative = 8.411642e-10 over the 4 epochs (debug
    // build, rustc 1.97.0, macOS 26.6.2 arm64, this host, 2026-09-17). This is ~6 orders of
    // magnitude above the gravity+3rd+SRP-only floor measured on this SAME golden's initial
    // state by `tests/srp_goldens.rs::four_epoch_acceleration_agreement_gravity_sun_moon_srp_
    // no_drag` (6.404746e-15 m/s^2 / 7.033944e-16 relative) -- i.e. drag, not the other four
    // forces, dominates this disagreement entirely, consistent with this task's own report:
    // the Jacchia-Roberts density disagreement (`tests/drag_goldens.rs`, ~1.3e-4 relative at
    // 200 km, ~2.6e-4 at 300 km, so ~2.0e-4 interpolated at this golden's own ~250 km altitude)
    // times this orbit's own drag acceleration magnitude (~6.45e-5 m/s^2, computed from the
    // interpolated density and this orbit's circular speed) predicts a drag-acceleration
    // disagreement of ~1.29e-8 m/s^2 -- the same order of magnitude as the 7.66e-9 m/s^2
    // measured here (see this task's own report for the full arithmetic).
    const TOLERANCE_ABS_M_S2: f64 = 1.2e-8;
    const TOLERANCE_REL: f64 = 1.2e-9;
    assert!(max_abs < TOLERANCE_ABS_M_S2, "max abs acceleration disagreement {max_abs:e} m/s^2 exceeds {TOLERANCE_ABS_M_S2:e}");
    assert!(max_rel < TOLERANCE_REL, "max relative acceleration disagreement {max_rel:e} exceeds {TOLERANCE_REL:e}");
}

// ---------------------------------------------------------------------------------------------
// The full M5 arc: native Dopri5 vs the golden's own GMAT-flown final_state, 86,400 s.
// ---------------------------------------------------------------------------------------------

#[test]
fn m5_full_arc_trajectory_residual_measured() {
    let _engine = gmat_sys::engine_lock();
    let golden = load_golden();
    let model = build_native_model(&golden.force_model, "N3M5Traj", &golden.spacecraft);

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
        "[n3c-m5-trajectory] native Dopri5 (JGM2 8x8 + Luna + Sun + drag + SRP) vs GMAT PrinceDormand78 over {} s: position residual = {dr:.6e} m, velocity residual = {dv:.6e} m/s, wall time {wall_s:.3} s",
        golden.duration_s
    );
    eprintln!("[n3c-m5-trajectory] for reference only, NEVER asserted against: the gmat-sys FFI path's own golden-recorded tolerance is {:e} m / {:e} m/s (round 1's measured 7 mm / 3 mm/s residual)", 0.05_f64, 5e-5_f64);

    // Measured, not assumed (this task's own rule: "assert at a bound you measured, recorded
    // in the test's own doc comment with the host state"). Measured on this host (debug build,
    // rustc 1.97.0, macOS 26.6.2 arm64, 2026-09-17): position residual = 6.831606e1 m
    // (68.3 m), velocity residual = 8.029063e-2 m/s (0.0803 m/s) over the full 86,400 s arc --
    // FOUR ORDERS OF MAGNITUDE larger than the gmat-sys FFI path's own 7 mm / 3 mm/s (this
    // golden's own `tolerance_m`/`tolerance_mps`, 0.05 m / 5e-5 m/s, printed above for
    // reference only and never asserted against here -- it pins that DIFFERENT code path, per
    // this task's own rule).
    //
    // Root cause, as far as this task's own effort budget reaches (see this task's own report
    // for the full arithmetic): the dominant candidate is the Jacchia-Roberts density
    // disagreement `tests/drag_goldens.rs` already measured against GMAT's own atmosphere
    // (~1.3e-4 relative at 200 km, ~2.6e-4 at 300 km -- interpolating to ~2.0e-4 at this
    // golden's own ~250 km altitude). Scaling that by this orbit's own drag-acceleration
    // magnitude (~6.45e-5 m/s^2, from the interpolated density and this orbit's ~7.755 km/s
    // circular speed) predicts a drag-acceleration DISAGREEMENT of ~1.29e-8 m/s^2 -- the same
    // order of magnitude the four-epoch acceleration-agreement test above actually measures
    // (7.66e-9 m/s^2 at t0). A small, CONSTANT drag-acceleration bias integrated as a
    // free-particle displacement (0.5*delta_a*T^2) over T=86400 s predicts ~48 m of position
    // error -- ~70% of the 68.3 m measured, the same order of magnitude. The two residuals are
    // also internally CONSISTENT with each other as a coherent along-track/phase error (not
    // noise): this orbit's own mean motion omega = 2*pi/T_orbit ~ 1.170e-3 rad/s (T_orbit ~
    // 5372 s at this ~6628 km SMA), and dv/dr = 0.0803/68.32 = 1.176e-3 rad/s matches omega to
    // within 1% -- exactly the dv ~ omega*dr signature of a small secular semi-major-axis/
    // phase drift accumulated over the ~16 orbits this arc covers, consistent with a
    // SYSTEMATIC (density-model) bias rather than random per-step integrator noise. The
    // remaining ~30% of the position residual (and the full velocity residual, which this
    // free-particle scaling does not itself predict, though the omega*dr consistency check
    // above does) is NOT chased further here -- recorded as an open gap, per this task's own
    // rule ("record the measured residual and root-cause as far as you can get").
    //
    // This bound is set just above the measured value (this task's own rule), never loosened
    // past what was actually observed, and is NOT the golden's own 0.05 m tolerance (which
    // pins the gmat-sys FFI path, not this native path).
    const TOLERANCE_M: f64 = 1.1e2;
    const TOLERANCE_MPS: f64 = 1.3e-1;
    assert!(dr < TOLERANCE_M, "position residual {dr:e} m exceeds the measured-plus-margin bound {TOLERANCE_M:e} m");
    assert!(dv < TOLERANCE_MPS, "velocity residual {dv:e} m/s exceeds the measured-plus-margin bound {TOLERANCE_MPS:e} m/s");
}
