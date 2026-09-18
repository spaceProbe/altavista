#![cfg(feature = "gmat-frames")]
//! Task 3c acceptance, deliverable 2 (`docs/native-dynamics-plan.md`, "the second atmosphere"):
//! [`av_orbital::model::EarthGravityModel::with_drag`] with [`av_orbital::msise90`] against
//! GMAT's own `Msise90Atmosphere` -- mirrors `tests/drag_goldens.rs`'s own three-test pattern
//! exactly (density comparison via point-mass-gravity subtraction, four-epoch acceleration
//! agreement, full-arc trajectory residual), same structure, atmosphere swapped.
//!
//! **Density comparison, `density_vs_gmat_at_tabulated_altitudes`.** GMAT's own `Msise90Atmosphere`
//! exposes no direct `Density` binding through `gmat-sys`'s own shim (identical situation to
//! Jacchia-Roberts -- see `tests/drag_goldens.rs`'s own module doc), so this drives it via
//! `DragForce::GetDerivatives` with gravity reduced to pure point-mass, subtracting the KNOWN
//! two-body acceleration analytically to solve for the density GMAT's own `DragForce` used.
//!
//! **This module's own altitude floor (`za`, ~122.8 km -- `crate::msise90`'s own doc comment)
//! is comfortably below every altitude this file tests (150-1200 km).**
use std::path::PathBuf;
use std::time::Instant;

use av_cdm::time::Tai;
use av_dynamics::DynamicsModel;
use av_orbital::cof;
use av_orbital::de::DeEphemeris;
use av_orbital::frame_gmat::GmatBodyFixedRotation;
use av_orbital::jacchia_roberts::{CentralBodyGeodetics, WeatherInputs};
use av_orbital::msise90::density_kg_m3;
use av_orbital::weather::{ConstantWeather, SpaceWeatherFile};
use av_orbital::{AtmosphereChoice, EarthGravityModel, EarthGravityModelInfo};
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
    weather_readback: std::collections::BTreeMap<String, serde_json::Value>,
    weather_file_sha256: String,
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

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../goldens/leo_400km_msise90.json")
}

fn load_golden() -> Golden {
    serde_json::from_str(&std::fs::read_to_string(golden_path()).unwrap()).unwrap()
}

fn gravity_path(file_name: &str) -> PathBuf {
    cof::locate_gmat_root().expect("GMAT_ROOT set").join("data/gravity/earth").join(file_name)
}

fn weather_path() -> PathBuf {
    av_orbital::weather::locate_gmat_root().expect("GMAT_ROOT set").join("data/atmosphere/earth/SpaceWeather-All-v1.2.txt")
}

fn km_state_to_m(state_km: &[f64]) -> [f64; 6] {
    [state_km[0] * 1e3, state_km[1] * 1e3, state_km[2] * 1e3, state_km[3] * 1e3, state_km[4] * 1e3, state_km[5] * 1e3]
}

// Unlike `tests/drag_goldens.rs` (Jacchia-Roberts), this file does NOT use an `IdentityRotation`
// mock anywhere -- see `density_vs_gmat_at_tabulated_altitudes`'s own doc comment for exactly
// why an identity rotation is wrong for MSISE90 (it depends on body-fixed LONGITUDE, which JR
// does not).

// ---------------------------------------------------------------------------------------------
// Density comparison: GMAT's DragForce/MSISE90, gravity reduced to pure point-mass so the drag
// acceleration (and hence the density GMAT used) can be recovered by subtraction.
// ---------------------------------------------------------------------------------------------

fn build_gmat_msise90_only_model(gmat: &Gmat, epoch_utc: &str, r_km_mag: f64, v_km_s: f64, namespace: &str) -> (gmat_sys::DerivativeModel, f64, f64) {
    let sat = gmat.construct("Spacecraft", &format!("N3MSat{namespace}")).unwrap();
    sat.set_str("DateFormat", "UTCGregorian").unwrap();
    sat.set_str("Epoch", epoch_utc).unwrap();
    sat.set_str("CoordinateSystem", "EarthMJ2000Eq").unwrap();
    sat.set_str("DisplayStateType", "Cartesian").unwrap();
    sat.set_real("X", r_km_mag).unwrap();
    sat.set_real("Y", 0.0).unwrap();
    sat.set_real("Z", 0.0).unwrap();
    sat.set_real("VX", 0.0).unwrap();
    sat.set_real("VY", v_km_s).unwrap();
    sat.set_real("VZ", 0.0).unwrap();
    sat.set_real("DryMass", 500.0).unwrap();
    sat.set_real("Cd", 2.2).unwrap();
    sat.set_real("DragArea", 5.0).unwrap();
    sat.set_real("SRPArea", 5.0).unwrap();
    sat.set_real("Cr", 1.8).unwrap();

    let fm = gmat.construct("ForceModel", &format!("N3MFM{namespace}")).unwrap();
    fm.set_str("CentralBody", "Earth").unwrap();
    let grav = gmat.construct("GravityField", &format!("N3MGrav{namespace}")).unwrap();
    grav.set_str("BodyName", "Earth").unwrap();
    grav.set_str("PotentialFile", "JGM2.cof").unwrap();
    grav.set_int("Degree", 0).unwrap();
    grav.set_int("Order", 0).unwrap();
    fm.add_force(&grav).unwrap();
    let mu_km3_s2 = grav.real_parameter("Mu").unwrap();

    let df = gmat.construct("DragForce", &format!("N3MDrag{namespace}")).unwrap();
    df.set_str("AtmosphereModel", "MSISE90").unwrap();
    df.set_str("HistoricWeatherSource", "ConstantFluxAndGeoMag").unwrap();
    df.set_str("PredictedWeatherSource", "ConstantFluxAndGeoMag").unwrap();
    df.set_real("F107", 150.0).unwrap();
    df.set_real("F107A", 150.0).unwrap();
    df.set_real("MagneticIndex", 3.0).unwrap();
    let atmos = gmat.construct("MSISE90", &format!("N3MAtmos{namespace}")).unwrap();
    df.set_reference(&atmos).unwrap();
    fm.add_force(&df).unwrap();

    gmat.initialize().unwrap();
    let model = gmat.derivative_model(&fm, &sat).expect("derivative model");
    (model, mu_km3_s2, 0.0)
}

/// Solves for the density GMAT's own `DragForce`/MSISE90 used at this call -- identical inverse
/// formula to `tests/drag_goldens.rs`'s own `gmat_implied_density`.
fn gmat_implied_density(model: &gmat_sys::DerivativeModel, state_km: &[f64], dt_s: f64, mu_km3_s2: f64, cd: f64, area_m2: f64, mass_kg: f64) -> f64 {
    let dot = model.derivatives(state_km, dt_s).expect("GMAT derivatives");
    let r_km = [state_km[0], state_km[1], state_km[2]];
    let rmag_km = (r_km[0] * r_km[0] + r_km[1] * r_km[1] + r_km[2] * r_km[2]).sqrt();
    let two_body_km_s2 = [-mu_km3_s2 * r_km[0] / rmag_km.powi(3), -mu_km3_s2 * r_km[1] / rmag_km.powi(3), -mu_km3_s2 * r_km[2] / rmag_km.powi(3)];
    let drag_only_km_s2 = [dot[3] - two_body_km_s2[0], dot[4] - two_body_km_s2[1], dot[5] - two_body_km_s2[2]];
    let amag_m_s2 = (drag_only_km_s2[0].powi(2) + drag_only_km_s2[1].powi(2) + drag_only_km_s2[2].powi(2)).sqrt() * 1e3;

    let omega = av_orbital::drag::EARTH_ANGULAR_VELOCITY_RAD_S;
    let r_m = [r_km[0] * 1e3, r_km[1] * 1e3, r_km[2] * 1e3];
    let v_m = [state_km[3] * 1e3, state_km[4] * 1e3, state_km[5] * 1e3];
    let vrel = av_orbital::drag::relative_velocity(r_m, v_m, omega);
    let vrelmag = (vrel[0] * vrel[0] + vrel[1] * vrel[1] + vrel[2] * vrel[2]).sqrt();
    amag_m_s2 / (0.5 * cd * area_m2 / mass_kg * vrelmag * vrelmag)
}

#[test]
fn density_vs_gmat_at_tabulated_altitudes() {
    let _engine = gmat_sys::engine_lock();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let epoch_utc = "01 Jan 2026 00:00:00.000";
    let t0_a1mjd = {
        let sat = gmat.construct("Spacecraft", "N3MEpochMirror").unwrap();
        sat.set_str("DateFormat", "UTCGregorian").unwrap();
        sat.set_str("Epoch", epoch_utc).unwrap();
        sat.set_str("CoordinateSystem", "EarthMJ2000Eq").unwrap();
        sat.set_str("DisplayStateType", "Cartesian").unwrap();
        sat.set_real("X", 7000.0).unwrap();
        sat.set_real("Y", 0.0).unwrap();
        sat.set_real("Z", 0.0).unwrap();
        sat.set_real("VX", 0.0).unwrap();
        sat.set_real("VY", 7.0).unwrap();
        sat.set_real("VZ", 0.0).unwrap();
        gmat.initialize().unwrap();
        let fm = gmat.construct("ForceModel", "N3MEpochFM").unwrap();
        fm.set_str("CentralBody", "Earth").unwrap();
        let grav = gmat.construct("GravityField", "N3MEpochGrav").unwrap();
        grav.set_str("BodyName", "Earth").unwrap();
        grav.set_str("PotentialFile", "JGM2.cof").unwrap();
        grav.set_int("Degree", 0).unwrap();
        grav.set_int("Order", 0).unwrap();
        fm.add_force(&grav).unwrap();
        let model = gmat.derivative_model(&fm, &sat).expect("derivative model");
        model.epoch_a1mjd()
    };
    let t_tai_ns = Tai::from_a1_mjd(t0_a1mjd).as_nanos();

    let cb = CentralBodyGeodetics::earth_defaults();
    let weather = WeatherInputs::from(ConstantWeather::gmat_defaults());
    let mu = 398_600.441_5_f64;
    // A REAL body-fixed rotation, NOT `IdentityRotation` -- unlike Jacchia-Roberts (whose own
    // `density_vs_gmat_at_tabulated_altitudes`, `tests/drag_goldens.rs`, correctly uses
    // `IdentityRotation`: JR's hour-angle geometry runs on the UN-rotated inertial Sun/
    // spacecraft vectors, so any z-axis rotation leaves it unchanged), MSISE90's density
    // genuinely depends on body-fixed LONGITUDE (`crate::msise90`'s own local-solar-time term,
    // `stl = sod/3600 + long/15`) -- an EARLIER version of this test used `IdentityRotation`
    // here by copying `drag_goldens.rs`'s own pattern uncritically, and measured 4.5%-73%
    // relative disagreement growing with altitude; switching to the true rotation (Earth's own
    // Greenwich hour angle at this epoch, which `IdentityRotation` implicitly pretends is zero)
    // collapses that to the value actually recorded below -- see this task's own report for the
    // full before/after comparison, which is itself the evidence this was a TEST bug, not a
    // density-formula bug (the golden's own full-arc trajectory residual, computed through the
    // SAME `crate::msise90::density_kg_m3` call path but via the real
    // `GmatBodyFixedRotation` `with_drag` uses, was never affected: 3.7 m over 86,400 s).
    let gmat_for_rotation = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup (idempotent)");
    let rotation = GmatBodyFixedRotation::new(gmat_for_rotation, "Earth", "N3MDensity").expect("GmatBodyFixedRotation::new");

    let mut max_abs_rel = 0.0_f64;
    let mut max_log10_rel = 0.0_f64;
    for (i, alt_km) in [150.0, 200.0, 300.0, 400.0, 500.0, 700.0, 900.0, 1200.0].into_iter().enumerate() {
        let r_km_mag = cb.equatorial_radius_km + alt_km;
        let v_km_s = (mu / r_km_mag).sqrt();
        let (model, mu_readback, _) = build_gmat_msise90_only_model(&gmat, epoch_utc, r_km_mag, v_km_s, &format!("A{i}"));
        let state_km = [r_km_mag, 0.0, 0.0, 0.0, v_km_s, 0.0];
        let rho_gmat = gmat_implied_density(&model, &state_km, 0.0, mu_readback, 2.2, 5.0, 500.0);

        let r_m = [r_km_mag * 1e3, 0.0, 0.0];
        let rho_native = density_kg_m3(r_m, &rotation, t_tai_ns, &weather, &cb).expect("native density");

        let rel = (rho_native - rho_gmat).abs() / rho_gmat;
        eprintln!("[n3c-msise90-density] alt={alt_km:.0}km rho_gmat={rho_gmat:e} rho_native={rho_native:e} relative_diff={rel:e}");
        max_abs_rel = max_abs_rel.max(rel);
        max_log10_rel = max_log10_rel.max((rho_native.log10() - rho_gmat.log10()).abs());
    }
    eprintln!("[n3c-msise90-density] max relative disagreement over 8 altitudes (150-1200 km): {max_abs_rel:e}; max |log10| disagreement: {max_log10_rel:e}");

    // Tolerance set just above the measured value (this task's own rule): measured max relative
    // disagreement 1.537684e-6 at 900 km (debug build, rustc 1.97.0, macOS 26.6.2 arm64, this
    // host, 2026-09-17) -- essentially floating-point noise, THREE orders of magnitude tighter
    // than Jacchia-Roberts's own equivalent measurement (`tests/drag_goldens.rs`, max 1.4e-3 at
    // 1200 km). This tight an agreement, with the correct body-fixed rotation (see this test's
    // own doc comment above for the bug that masked it), is itself strong evidence this port's
    // own `GLOBE6`/`GLOB6S`/`DENSU`/`DNET`/`CCOR`/coefficient-table transcription is correct,
    // not merely plausible.
    const TOLERANCE_REL: f64 = 1e-5;
    assert!(max_abs_rel < TOLERANCE_REL, "max relative density disagreement {max_abs_rel:e} exceeds {TOLERANCE_REL:e}");
}

// ---------------------------------------------------------------------------------------------
// Acceleration agreement: JGM2 8x8 + DragForce/MSISE90, native vs GetDerivatives.
// ---------------------------------------------------------------------------------------------

fn build_native_model(force_model: &ForceModelCfg, namespace: &str, drag_area_m2: f64, cd: f64, mass_kg: f64, weather: &std::collections::BTreeMap<String, serde_json::Value>) -> EarthGravityModel<GmatBodyFixedRotation> {
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let rotation = GmatBodyFixedRotation::new(gmat, &force_model.central_body, namespace).expect("GmatBodyFixedRotation::new");
    let weather_inputs = WeatherInputs { f107: weather["F107"].as_f64().unwrap(), f107a: weather["F107A"].as_f64().unwrap(), kp: weather["MagneticIndex"].as_f64().unwrap() };
    EarthGravityModel::new(
        &gravity_path(&force_model.gravity.file),
        force_model.gravity.degree as usize,
        force_model.gravity.order as usize,
        &force_model.central_body,
        rotation,
        EarthGravityModelInfo { id: "native.orbital.n3c_msise90_test".to_string(), version: env!("CARGO_PKG_VERSION").to_string(), goldens: vec!["leo_400km_msise90".to_string()] },
    )
    .expect("EarthGravityModel construction")
    // An EMPTY third-body list, deliberately -- see `tests/drag_goldens.rs`'s own identical
    // comment: this golden's own force model has no point-mass perturbations; `with_drag` only
    // needs the bound `DeEphemeris` HANDLE, and for MSISE90 not even that (no Sun position is
    // used -- `crate::msise90`'s own module doc), but `with_third_bodies` is still required
    // first (`AtmosphereChoice`'s own doc comment: one invariant, not a per-atmosphere case).
    .with_third_bodies(&DeEphemeris::locate_gmat_root().expect("GMAT_ROOT set").join("data/planetary_ephem/de/leDE1941.405"), &[])
    .expect("with_third_bodies (empty list)")
    .with_drag(AtmosphereChoice::Msise90, weather_inputs, drag_area_m2, cd, mass_kg)
    .expect("with_drag")
}

fn build_gmat_derivative_model(gmat: &Gmat, epoch_utc: &str, initial_state_km: &[f64], spacecraft: &std::collections::BTreeMap<String, f64>, force_model: &ForceModelCfg, namespace: &str) -> gmat_sys::DerivativeModel {
    let sat = gmat.construct("Spacecraft", &format!("N3MAccel{namespace}")).unwrap();
    sat.set_str("DateFormat", "UTCGregorian").unwrap();
    sat.set_str("Epoch", epoch_utc).unwrap();
    sat.set_str("CoordinateSystem", "EarthMJ2000Eq").unwrap();
    sat.set_str("DisplayStateType", "Cartesian").unwrap();
    for (field, v) in ["X", "Y", "Z", "VX", "VY", "VZ"].iter().zip(initial_state_km) {
        sat.set_real(field, *v).unwrap();
    }
    for (k, v) in spacecraft {
        if k != "SMA" && k != "ECC" && k != "INC" && k != "RAAN" && k != "AOP" && k != "TA" {
            sat.set_real(k, *v).unwrap();
        }
    }

    let fm = gmat.construct("ForceModel", &format!("N3MAccelFM{namespace}")).unwrap();
    fm.set_str("CentralBody", &force_model.central_body).unwrap();
    let grav = gmat.construct("GravityField", "").unwrap();
    grav.set_str("BodyName", &force_model.central_body).unwrap();
    grav.set_str("PotentialFile", &force_model.gravity.file).unwrap();
    grav.set_int("Degree", force_model.gravity.degree).unwrap();
    grav.set_int("Order", force_model.gravity.order).unwrap();
    fm.add_force(&grav).unwrap();

    let df = gmat.construct("DragForce", &format!("N3MAccelDrag{namespace}")).unwrap();
    df.set_str("AtmosphereModel", "MSISE90").unwrap();
    df.set_str("HistoricWeatherSource", "ConstantFluxAndGeoMag").unwrap();
    df.set_str("PredictedWeatherSource", "ConstantFluxAndGeoMag").unwrap();
    df.set_real("F107", 150.0).unwrap();
    df.set_real("F107A", 150.0).unwrap();
    df.set_real("MagneticIndex", 3.0).unwrap();
    let atmos = gmat.construct("MSISE90", &format!("N3MAccelAtmos{namespace}")).unwrap();
    df.set_reference(&atmos).unwrap();
    fm.add_force(&df).unwrap();

    gmat.initialize().unwrap();
    gmat.derivative_model(&fm, &sat).expect("derivative model")
}

#[test]
fn acceleration_agreement_against_get_derivatives() {
    let _engine = gmat_sys::engine_lock();
    let golden = load_golden();

    let drag_area = golden.spacecraft["DragArea"];
    let cd = golden.spacecraft["Cd"];
    let mass_kg = golden.spacecraft["DryMass"];
    let native = build_native_model(&golden.force_model, "N3MAccel4", drag_area, cd, mass_kg, &golden.weather_readback);

    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup (idempotent)");
    let gmat_model = build_gmat_derivative_model(&gmat, &golden.epoch_utc, &golden.initial_state, &golden.spacecraft, &golden.force_model, "N3MAccel4");

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
        eprintln!("[n3c-msise90-4epoch] dt={dt_s:.1}s native={native_accel:?} gmat={gmat_accel:?} |diff|={abs_diff:.6e} m/s^2 (relative {rel_diff:.6e})");
        max_abs = max_abs.max(abs_diff);
        max_rel = max_rel.max(rel_diff);
    }
    eprintln!("[n3c-msise90-4epoch] acceleration agreement over 4 epochs: max |diff| = {max_abs:.6e} m/s^2, max relative = {max_rel:.6e}");

    // Tolerance set just above the measured value (this task's own rule): measured max |diff| =
    // 3.784511e-10 m/s^2, max relative = 4.355740e-11 over the 4 epochs (debug build, rustc
    // 1.97.0, macOS 26.6.2 arm64, this host, 2026-09-17) -- machine-precision noise, the same
    // order as this crate's other force-model acceleration-agreement floors.
    const TOLERANCE_ABS_M_S2: f64 = 1e-9;
    const TOLERANCE_REL: f64 = 1e-10;
    assert!(max_abs < TOLERANCE_ABS_M_S2, "max abs acceleration disagreement {max_abs:e} m/s^2 exceeds {TOLERANCE_ABS_M_S2:e}");
    assert!(max_rel < TOLERANCE_REL, "max relative acceleration disagreement {max_rel:e} exceeds {TOLERANCE_REL:e}");
}

// ---------------------------------------------------------------------------------------------
// Trajectory residual over the full arc.
// ---------------------------------------------------------------------------------------------

#[test]
fn trajectory_residual_against_golden() {
    let _engine = gmat_sys::engine_lock();
    let golden = load_golden();

    let drag_area = golden.spacecraft["DragArea"];
    let cd = golden.spacecraft["Cd"];
    let mass_kg = golden.spacecraft["DryMass"];
    let model = build_native_model(&golden.force_model, "N3MTraj", drag_area, cd, mass_kg, &golden.weather_readback);

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
        "[n3c-msise90-trajectory] native Dopri5 (JGM2 8x8 + DragForce/MSISE90) vs GMAT PrinceDormand78 over {} s: position residual = {dr:.6e} m, velocity residual = {dv:.6e} m/s, wall time {wall_s:.3} s",
        golden.duration_s
    );
    eprintln!("[n3c-msise90-trajectory] residual vs the golden's own recorded tolerance: {dr:e} m / {:e} m, {dv:e} m/s / {:e} m/s", golden.tolerance_m, golden.tolerance_mps);
    assert!(dr < golden.tolerance_m, "position residual {dr:e} m exceeds the golden's own recorded {:e} m tolerance", golden.tolerance_m);
    assert!(dv < golden.tolerance_mps, "velocity residual {dv:e} m/s exceeds the golden's own recorded {:e} m/s tolerance", golden.tolerance_mps);
}

// ---------------------------------------------------------------------------------------------
// The golden's own recorded weather-file SHA-256 matches the live file.
// ---------------------------------------------------------------------------------------------

#[test]
fn golden_records_the_live_weather_file_sha256() {
    let golden = load_golden();
    let f = SpaceWeatherFile::open(&weather_path()).expect("open CSSI file");
    assert_eq!(golden.weather_file_sha256, f.sha256(), "the golden's own recorded weather_file_sha256 does not match the live file's SHA-256");
}
