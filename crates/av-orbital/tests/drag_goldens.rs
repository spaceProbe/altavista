#![cfg(feature = "gmat-frames")]
//! Task 3b acceptance (`docs/native-dynamics-plan.md`, "N3's drag -- the space-weather reader,
//! the drag force, and the Jacchia-Roberts atmosphere"):
//! [`av_orbital::model::EarthGravityModel::with_drag`] against GMAT.
//!
//! **Density comparison, `density_vs_gmat_at_tabulated_altitudes`.** GMAT's own
//! `JacchiaRobertsAtmosphere` is the most direct reference available on this host (this
//! task's own instruction) -- driven here via `DragForce::GetDerivatives` with gravity
//! reduced to pure point-mass (`GravityField` degree/order 0/0, so the two-body acceleration
//! can be subtracted ANALYTICALLY, isolating drag's own acceleration, from which density is
//! solved algebraically given the known `Cd`/`A`/`m`/`v_rel`) -- GMAT exposes no direct
//! `AtmosphereModel::Density` binding through the Python API or `gmat-sys`'s own shim, so this
//! is the documented fallback this task's own brief names ("if it cannot be driven directly,
//! GMAT's drag acceleration with everything else zeroed").
//!
//! **Acceleration agreement, `acceleration_agreement_against_get_derivatives`.** JGM2 8x8 +
//! `DragForce`/`JacchiaRoberts` (the golden's own force model) at four epochs, native vs
//! GMAT's own `GetDerivatives`, isolating the integrator exactly as `tests/srp_goldens.rs`/
//! `tests/gravity_goldens.rs` do.
//!
//! **Trajectory residual, `trajectory_residual_against_golden`.** The full one-day arc,
//! native `Dopri5` vs GMAT's own `PrinceDormand78`, at the tolerance
//! `goldens/leo_400km_jacchia_roberts.json` itself records.
use std::path::PathBuf;
use std::time::Instant;

use av_cdm::time::Tai;
use av_dynamics::DynamicsModel;
use av_orbital::cof;
use av_orbital::de::{DeBody, DeEphemeris};
use av_orbital::frame::{BodyFixedRotation, Rotation};
use av_orbital::frame_gmat::GmatBodyFixedRotation;
use av_orbital::jacchia_roberts::{density_kg_m3, CentralBodyGeodetics, WeatherInputs};
use av_orbital::weather::{ConstantWeather, SpaceWeatherFile};
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
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../goldens/leo_400km_jacchia_roberts.json")
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

/// This test file's own no-op body-fixed rotation (mirrors `model.rs`'s own test mock) -- used
/// only where a genuine `GmatBodyFixedRotation` handle is not needed (the density-vs-GMAT
/// comparison below evaluates purely EQUATORIAL positions, where the geodetic latitude is
/// exactly zero under ANY rotation about the z-axis, so an identity rotation gives the SAME
/// geodetic height/latitude a true body-fixed rotation would -- see that test's own comment).
struct IdentityRotation;
impl BodyFixedRotation for IdentityRotation {
    type Error = std::convert::Infallible;
    fn inertial_to_fixed(&self, _t_tai_ns: i64) -> Result<Rotation, Self::Error> {
        Ok(Rotation { r: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]], r_dot: [[0.0; 3]; 3] })
    }
}

// ---------------------------------------------------------------------------------------------
// Density comparison: GMAT's DragForce, gravity reduced to pure point-mass so the drag
// acceleration (and hence the density GMAT used) can be recovered by subtraction.
// ---------------------------------------------------------------------------------------------

fn build_gmat_drag_only_model(gmat: &Gmat, epoch_utc: &str, r_km_mag: f64, v_km_s: f64, namespace: &str) -> (gmat_sys::DerivativeModel, f64, f64) {
    let sat = gmat.construct("Spacecraft", &format!("N3DSat{namespace}")).unwrap();
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

    let fm = gmat.construct("ForceModel", &format!("N3DFM{namespace}")).unwrap();
    fm.set_str("CentralBody", "Earth").unwrap();
    let grav = gmat.construct("GravityField", &format!("N3DGrav{namespace}")).unwrap();
    grav.set_str("BodyName", "Earth").unwrap();
    grav.set_str("PotentialFile", "JGM2.cof").unwrap();
    grav.set_int("Degree", 0).unwrap();
    grav.set_int("Order", 0).unwrap();
    fm.add_force(&grav).unwrap();
    let mu_km3_s2 = grav.real_parameter("Mu").unwrap();

    let df = gmat.construct("DragForce", &format!("N3DDrag{namespace}")).unwrap();
    df.set_str("AtmosphereModel", "JacchiaRoberts").unwrap();
    df.set_str("HistoricWeatherSource", "ConstantFluxAndGeoMag").unwrap();
    df.set_str("PredictedWeatherSource", "ConstantFluxAndGeoMag").unwrap();
    df.set_real("F107", 150.0).unwrap();
    df.set_real("F107A", 150.0).unwrap();
    df.set_real("MagneticIndex", 3.0).unwrap();
    let atmos = gmat.construct("JacchiaRoberts", &format!("N3DAtmos{namespace}")).unwrap();
    df.set_reference(&atmos).unwrap();
    fm.add_force(&df).unwrap();

    gmat.initialize().unwrap();
    let model = gmat.derivative_model(&fm, &sat).expect("derivative model");
    (model, mu_km3_s2, 0.0)
}

/// Solves for the density GMAT's own `DragForce` used at this call, given the KNOWN two-body
/// (pure point-mass) acceleration to subtract and the KNOWN ballistic/rotation inputs --
/// `rho = |a_drag| / (0.5 * Cd * A/m * |v_rel|^2)`, the inverse of this crate's own
/// `crate::drag::drag_acceleration` formula.
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
        // A throwaway mirror spacecraft's own epoch readback -- the same pattern this crate's
        // other tests use to get GMAT's own A1MJD for a UTCGregorian string.
        let sat = gmat.construct("Spacecraft", "N3DEpochMirror").unwrap();
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
        // Real epoch readback: build a full derivative model so GMAT actually computes and
        // exposes the A1MJD it resolved "01 Jan 2026 00:00:00.000" UTC to.
        let fm = gmat.construct("ForceModel", "N3DEpochFM").unwrap();
        fm.set_str("CentralBody", "Earth").unwrap();
        let grav = gmat.construct("GravityField", "N3DEpochGrav").unwrap();
        grav.set_str("BodyName", "Earth").unwrap();
        grav.set_str("PotentialFile", "JGM2.cof").unwrap();
        grav.set_int("Degree", 0).unwrap();
        grav.set_int("Order", 0).unwrap();
        fm.add_force(&grav).unwrap();
        let model = gmat.derivative_model(&fm, &sat).expect("derivative model");
        model.epoch_a1mjd()
    };
    let t_tai_ns = Tai::from_a1_mjd(t0_a1mjd).as_nanos();

    let de = DeEphemeris::open(&DeEphemeris::locate_gmat_root().expect("GMAT_ROOT set").join("data/planetary_ephem/de/leDE1941.405")).expect("open DE405");
    let (jd1, jd2) = av_orbital::tdb::tai_ns_to_tdb_jd2(t_tai_ns);
    let sun_km_de = de.geocentric_position_km2(DeBody::Sun, jd1, jd2).expect("Sun position");
    let sun_m = [sun_km_de[0] * 1e3, sun_km_de[1] * 1e3, sun_km_de[2] * 1e3];

    let cb = CentralBodyGeodetics::earth_defaults();
    let weather = WeatherInputs::from(ConstantWeather::gmat_defaults());
    let mu = 398_600.441_5_f64; // Earth mu, km^3/s^2 (read back per-call below too)

    let mut max_abs_rel = 0.0_f64;
    let mut max_log10_rel = 0.0_f64;
    for (i, alt_km) in [150.0, 200.0, 300.0, 400.0, 500.0, 700.0, 900.0, 1200.0].into_iter().enumerate() {
        let r_km_mag = cb.equatorial_radius_km + alt_km;
        let v_km_s = (mu / r_km_mag).sqrt();
        let (model, mu_readback, _) = build_gmat_drag_only_model(&gmat, epoch_utc, r_km_mag, v_km_s, &format!("A{i}"));
        let state_km = [r_km_mag, 0.0, 0.0, 0.0, v_km_s, 0.0];
        let rho_gmat = gmat_implied_density(&model, &state_km, 0.0, mu_readback, 2.2, 5.0, 500.0);

        let r_m = [r_km_mag * 1e3, 0.0, 0.0];
        let rho_native = density_kg_m3(r_m, sun_m, &IdentityRotation, t_tai_ns, &weather, &cb).expect("native density");

        let rel = (rho_native - rho_gmat).abs() / rho_gmat;
        eprintln!("[n3-density] alt={alt_km:.0}km rho_gmat={rho_gmat:e} rho_native={rho_native:e} relative_diff={rel:e}");
        max_abs_rel = max_abs_rel.max(rel);
        max_log10_rel = max_log10_rel.max((rho_native.log10() - rho_gmat.log10()).abs());
    }
    eprintln!("[n3-density] max relative disagreement over 8 altitudes (150-1200 km): {max_abs_rel:e}; max |log10| disagreement: {max_log10_rel:e}");

    // Tolerance set just above the measured value (this task's own rule): measured max
    // relative disagreement 1.407224e-3 (0.14%) at 1200 km, the largest of the 8 altitudes
    // tested. UNCHANGED by round 3's own fix (`exotherm`/`raw_density_g_cm3` now thread the
    // caller's own `CentralBodyGeodetics` through instead of hardcoding Earth's defaults
    // internally -- numerically a no-op for this golden, which IS Earth at those defaults, but
    // closes a real correctness gap for any other central body).
    //
    // Round 3's own root-cause investigation (question 227), continuing round 2's: the fine
    // altitude sweep `density_vs_gmat_fine_altitude_sweep_and_weather_sensitivity` (#[ignore]d,
    // ~120 points, 130-2400 km) shows the disagreement is NOT monotonic -- it grows from ~3e-5
    // at 130 km to a LOCAL PEAK of ~6.1e-4 (positive: native > GMAT) around 620-650 km, falls
    // through a SIGN FLIP near 870 km (measured 870 km: -5.9e-6, effectively zero), then grows
    // to a second, larger, OPPOSITE-SIGN plateau of ~-1.5e-3 (native < GMAT) from ~1300-2000 km
    // (the tail beyond ~2000 km is numerically noisy -- density there is ~1e-16 kg/m^3, close to
    // the acceleration-subtraction recovery method's own floor). A companion species-composition
    // diagnostic (independently re-derived in Python from `JacchiaRobertsAtmosphere.cpp`,
    // matching this crate's own `rho_high` bit-for-bit) shows Atomic Oxygen (i=4) dominates the
    // total density below ~900 km and Helium (i=2) dominates above ~1000 km -- the SAME band the
    // sign flip falls in, strongly suggesting a SPECIES-DIFFERENTIAL mechanism.
    //
    // Ruled out, with justification (this task's own rule -- "say exactly what you ruled out,
    // how"):
    //   - Every term/constant/evaluation-order in `exotherm`/`rho_high`/`rho_cor`: re-verified
    //     line-for-line against `JacchiaRobertsAtmosphere.cpp` AND independently re-derived in
    //     Python (a FRESH transcription, not copy-pasted from this port) -- both match this
    //     crate's own Rust output exactly at every altitude/species tested.
    //   - Any UNIFORM (non-species-differential) shared-input error (e.g. a small mismatch in
    //     `t_infinity`/`tx`/`sum`/`cbPolarRadius`, from geometry or weather): PROVEN incapable of
    //     producing a sign flip, both analytically and by direct numerical perturbation --
    //     `base = (t_infinity-temperature)/(t_infinity-tx)` lies in (0,1) for every species at
    //     every altitude > 125 km, so `d ln(term_i) = gamma_i * d ln(base)` has the SAME sign for
    //     every species (gamma_i > 0 always); a Python perturbation of the shared `t1`
    //     (hence `t_infinity`) input by 0.01-1 K, at every altitude in the sweep, produced a
    //     same-signed relative shift throughout (e.g. +4.7e-4 at 150 km to +6.4e-4 at 700 km to
    //     +3.4e-4 at 2000 km for a +0.1 K perturbation, at NO altitude negative) -- ruling out
    //     `t_infinity`/`tx`/`sum`/`cbPolarRadius` (and hence the previously-hardcoded-central-body
    //     bug this round fixed) as the sign-flip's own source.
    //   - F10.7/F10.7A/MagneticIndex as the dominant driver: the weather-sensitivity half of the
    //     fine-sweep test varies F10.7 150->250 (t_infinity swings by several hundred K) and Kp
    //     3->6 at fixed altitude (400 km, 700 km) -- the relative disagreement barely moves
    //     (400 km: 3.9e-4 baseline -> 3.0e-4 at high F10.7 -> 3.3e-4 at high Kp; 700 km: 5.7e-4 ->
    //     5.7e-4 -> 5.4e-4), ruling out the geomagnetic/xtemp terms as the dominant mechanism.
    //   - Helium's own unique `f` correction (the only OTHER species-differential term in
    //     `rho_high`): negligible at this test's geo_lat=0 geometry -- `sin(pi/4-0)^3-0.35355 ~
    //     3.4e-6` (GMAT's own literal `0.35355` is itself an approximation to `sin(pi/4)^3`, a
    //     property of GMAT's OWN source, not this port), giving `f ~ 1.0000049`, six orders of
    //     magnitude too small to matter.
    //   - Floating-point roundoff in this test's own acceleration-subtraction recovery: at the
    //     870 km crossover, drag acceleration is ~8e-9 m/s^2 against a two-body term of ~7.6
    //     m/s^2 (ratio ~1e-9) -- seven orders of magnitude above the ~1e-16 double-precision
    //     floor, so roundoff cannot explain the crossover (though it likely explains the noisy
    //     tail past ~2000 km, where density approaches the recovery method's own floor).
    //
    // THE DECISIVE EXPERIMENT (this task's own reviewer): the sign-flip is species-differential
    // (established above) and a shared/uniform input error is PROVEN incapable of producing one
    // (established above), so the residual must decompose as `e(h) = sum_i w_i(h)*eps_i`,
    // `w_i(h) = rho_i(h)/rho_total(h)` the per-species weight `rho_high_species_terms` exposes
    // and `eps_i` a per-species CONSTANT relative error. Fit by ordinary least squares (`scratch
    // fit_species.py`, `.venv`'s own `numpy`) over all 114 points of the fine sweep's own species
    // breakdown that have BOTH a weight and a measured `e(h)` -- Argon's own weight never exceeds
    // 0.3% anywhere (UNIDENTIFIABLE, excluded per this task's own rule); Nitrogen and Molecular
    // Oxygen's weights are 96.8% correlated across this altitude range (collinear -- the
    // 6-species fit is ILL-CONDITIONED, condition number ~1.7e4, and gives an unstable,
    // untrustworthy split between them). The WELL-CONDITIONED reduced fit (Nitrogen, Helium,
    // Atomic Oxygen only -- condition number 5.3) explains 90.7% of the disagreement's own RMS
    // (residual RMS 9.9e-5 against `e(h)`'s own RMS 1.07e-3), over the full 130-2000 km clean
    // range (the >2000 km recovery-floor-noise tail excluded, per this task's own reviewer):
    //
    //     eps_N2 = -4.515e-4   (Nitrogen very slightly LOW, native < GMAT)
    //     eps_He = -1.492e-3   (Helium the LARGEST bias, LOW, native < GMAT)
    //     eps_O  = +6.743e-4   (Atomic Oxygen HIGH, native > GMAT)
    //
    // This is DEFINITIVE evidence the residual is a set of near-constant, OPPOSITE-SIGNED
    // per-species biases (Helium negative, Oxygen positive) dominating below/above the
    // composition crossover respectively -- exactly what produces the observed sign flip, and
    // NOT reproducible by any shared upstream quantity: a Python perturbation of the ONE shared
    // input (`t1`/`t_infinity`) gives the SAME SIGN at every altitude (proven above), so it
    // cannot produce opposite-signed `eps_He`/`eps_O`. Yet EVERY line, constant and evaluation
    // order that could produce a Helium- or Oxygen-specific bias was re-verified, again, against
    // `JacchiaRobertsAtmosphere.cpp` at full precision: `CON_DEN[2]`/`CON_DEN[4]` (Helium's/
    // Oxygen's own density polynomials) are digit-for-digit identical; `MOL_MASS[2]`/`MOL_MASS[4]`
    // are identical; Helium's OWN unique code (`exp1 -= 0.38`, matching bit-for-bit) and its `f`
    // correction (re-measured at this sweep's OWN real `sun_dec` at the golden's Jan-1 epoch,
    // ~-0.40 rad near winter solstice, not the earlier illustrative 0.4 rad: `f = 10^(2.16e-6) ~
    // 1.0000050`, still six orders of magnitude too small to explain `eps_He = -1.49e-3`) are
    // both confirmed correct; Oxygen has NO species-specific code at all (`i=4` takes the exact
    // same code path as `i=0,1,3`), so a genuine, isolated Oxygen-only defect would require an
    // Oxygen-specific WRONG CONSTANT this task could not find given every candidate constant
    // checked out. The fit's own 9.3% unexplained residual (not pure noise -- it has smooth,
    // altitude-correlated structure, e.g. a further +2e-5 drift across 1900-2000 km) is
    // consistent with a residual, smaller, altitude-DEPENDENT term this constant-per-species
    // model does not capture (candidates un-eliminated: a species-differential SENSITIVITY,
    // rather than a species-CONSTANT bias, to a shared but unattributed `t_infinity`-level input
    // discrepancy -- ruled OUT as the WHOLE explanation by the sign argument above, but not ruled
    // out as a smaller residual contributor).
    //
    // Definitive conclusion, per this task's own rule ("say exactly what you ruled out, how, and
    // what the remaining path is"): the sign-flip mechanism IS pinned down quantitatively (a
    // near-constant, opposite-signed Helium/Oxygen bias, fit at 90.7% quality with a
    // well-conditioned system) -- but NO single differing line of code was found that explains
    // either bias, after exhaustive, repeated, bit-for-bit verification of every candidate
    // location this task's own brief and two independent reviews named. The remaining path, not
    // reached within this round's effort budget: instrument GMAT's OWN `rho_high` (a local,
    // throwaway build, print the six `r[i]` terms it computes) and diff them directly against
    // this crate's own `rho_high_species_terms` at matched inputs -- the one comparison this
    // task could not make without modifying/rebuilding GMAT's own source, which is out of this
    // worker's file scope (`third_party/gmat-src` is read-only here). A second, smaller,
    // independently-confirmed defect was found on the way (see `crate::drag`'s own module doc,
    // "Which omega, and whether it is the full body-fixed rotation"): GMAT's `DragForce` does NOT
    // use a fixed scalar Earth-rotation rate for the drag corotation term, as this crate's own
    // `drag.rs` doc previously and incorrectly claimed -- `JacchiaRobertsAtmosphere::Density`
    // recomputes the true rotation-derived angular velocity on every call via
    // `AtmosphereModel::BuildAngularVelocity`. Measured impact of that gap is order 1e-5 to 1e-6
    // relative (several orders of magnitude below this residual), confirmed NOT the dominant
    // driver here, but real and now documented.
    const TOLERANCE_REL: f64 = 2e-3;
    assert!(max_abs_rel < TOLERANCE_REL, "max relative density disagreement {max_abs_rel:e} exceeds {TOLERANCE_REL:e}");
}

/// Same as `build_gmat_drag_only_model` but with caller-chosen weather, for the discriminating
/// experiment this task's own brief names ("vary F107/F107A/MagneticIndex ... the cheapest
/// decisive experiment you have").
fn build_gmat_drag_only_model_weather(gmat: &Gmat, epoch_utc: &str, r_km_mag: f64, v_km_s: f64, namespace: &str, weather: WeatherInputs) -> (gmat_sys::DerivativeModel, f64, f64) {
    let WeatherInputs { f107, f107a, kp } = weather;
    let sat = gmat.construct("Spacecraft", &format!("N3DWSat{namespace}")).unwrap();
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

    let fm = gmat.construct("ForceModel", &format!("N3DWFM{namespace}")).unwrap();
    fm.set_str("CentralBody", "Earth").unwrap();
    let grav = gmat.construct("GravityField", &format!("N3DWGrav{namespace}")).unwrap();
    grav.set_str("BodyName", "Earth").unwrap();
    grav.set_str("PotentialFile", "JGM2.cof").unwrap();
    grav.set_int("Degree", 0).unwrap();
    grav.set_int("Order", 0).unwrap();
    fm.add_force(&grav).unwrap();
    let mu_km3_s2 = grav.real_parameter("Mu").unwrap();

    let df = gmat.construct("DragForce", &format!("N3DWDrag{namespace}")).unwrap();
    df.set_str("AtmosphereModel", "JacchiaRoberts").unwrap();
    df.set_str("HistoricWeatherSource", "ConstantFluxAndGeoMag").unwrap();
    df.set_str("PredictedWeatherSource", "ConstantFluxAndGeoMag").unwrap();
    df.set_real("F107", f107).unwrap();
    df.set_real("F107A", f107a).unwrap();
    df.set_real("MagneticIndex", kp).unwrap();
    let atmos = gmat.construct("JacchiaRoberts", &format!("N3DWAtmos{namespace}")).unwrap();
    df.set_reference(&atmos).unwrap();
    fm.add_force(&df).unwrap();

    gmat.initialize().unwrap();
    let model = gmat.derivative_model(&fm, &sat).expect("derivative model");
    (model, mu_km3_s2, 0.0)
}

/// Diagnostic-only (this task's own root-cause investigation, question 227/N3 round 3): a much
/// finer altitude sweep than the 8-point ladder `density_vs_gmat_at_tabulated_altitudes` checks,
/// to see the SHAPE of the relative-disagreement growth (a kink at 500 km implicates hydrogen; a
/// smooth curve does not), plus a weather-sensitivity experiment (varying F107/F107A moves
/// `t_infinity` only; varying MagneticIndex moves the geomagnetic correction only) -- "the
/// cheapest decisive experiment you have" per this task's own brief. `#[ignore]`d: this is a
/// one-off investigation aid, not a golden-backed regression test, and it constructs ~120 GMAT
/// objects (slow). Run with `cargo test -p av-orbital --features gmat-frames --test drag_goldens
/// -- --ignored density_vs_gmat_fine_altitude_sweep_and_weather_sensitivity --nocapture`.
#[test]
#[ignore]
fn density_vs_gmat_fine_altitude_sweep_and_weather_sensitivity() {
    let _engine = gmat_sys::engine_lock();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let epoch_utc = "01 Jan 2026 00:00:00.000";
    let t0_a1mjd = {
        let sat = gmat.construct("Spacecraft", "N3DFEpochMirror").unwrap();
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
        let fm = gmat.construct("ForceModel", "N3DFEpochFM").unwrap();
        fm.set_str("CentralBody", "Earth").unwrap();
        let grav = gmat.construct("GravityField", "N3DFEpochGrav").unwrap();
        grav.set_str("BodyName", "Earth").unwrap();
        grav.set_str("PotentialFile", "JGM2.cof").unwrap();
        grav.set_int("Degree", 0).unwrap();
        grav.set_int("Order", 0).unwrap();
        fm.add_force(&grav).unwrap();
        let model = gmat.derivative_model(&fm, &sat).expect("derivative model");
        model.epoch_a1mjd()
    };
    let t_tai_ns = Tai::from_a1_mjd(t0_a1mjd).as_nanos();

    let de = DeEphemeris::open(&DeEphemeris::locate_gmat_root().expect("GMAT_ROOT set").join("data/planetary_ephem/de/leDE1941.405")).expect("open DE405");
    let (jd1, jd2) = av_orbital::tdb::tai_ns_to_tdb_jd2(t_tai_ns);
    let sun_km_de = de.geocentric_position_km2(DeBody::Sun, jd1, jd2).expect("Sun position");
    let sun_m = [sun_km_de[0] * 1e3, sun_km_de[1] * 1e3, sun_km_de[2] * 1e3];

    let cb = CentralBodyGeodetics::earth_defaults();
    let mu = 398_600.441_5_f64;

    // --- Part 1: fine altitude sweep, 20 km steps, 130-2400 km -- PLUS, for every point in the
    // rho_high band (height > 125 km), the six per-species partial densities
    // (`rho_high_species_terms`), for the species-differential decomposition e(h) = sum_i
    // w_i(h)*eps_i this task's own root-cause review asked for (question 227's own reviewer
    // message: "fit it").
    eprintln!("[n3-fine-sweep] alt_km rho_gmat rho_native relative_diff");
    let weather = WeatherInputs::from(ConstantWeather::gmat_defaults());
    let xtemp_k = 379.0 + 3.24 * weather.f107a + 1.3 * (weather.f107 - weather.f107a);
    let sun_dec_rad = sun_m[2].atan2((sun_m[0] * sun_m[0] + sun_m[1] * sun_m[1]).sqrt());
    let mut idx = 0;
    let mut alt_km = 130.0_f64;
    while alt_km <= 2400.0 {
        let r_km_mag = cb.equatorial_radius_km + alt_km;
        let v_km_s = (mu / r_km_mag).sqrt();
        let (model, mu_readback, _) = build_gmat_drag_only_model(&gmat, epoch_utc, r_km_mag, v_km_s, &format!("Sweep{idx}"));
        let state_km = [r_km_mag, 0.0, 0.0, 0.0, v_km_s, 0.0];
        let rho_gmat = gmat_implied_density(&model, &state_km, 0.0, mu_readback, 2.2, 5.0, 500.0);
        let r_m = [r_km_mag * 1e3, 0.0, 0.0];
        let rho_native = density_kg_m3(r_m, sun_m, &IdentityRotation, t_tai_ns, &weather, &cb).expect("native density");
        let signed_rel = (rho_native - rho_gmat) / rho_gmat;
        let rel = signed_rel.abs();
        eprintln!("[n3-fine-sweep] {alt_km:.0} {rho_gmat:e} {rho_native:e} {rel:e} signed={signed_rel:e}");

        // Per-species breakdown -- same r_sc_km/r_sun_km/geo_lat_rad=0 geometry density_kg_m3
        // used internally (IdentityRotation, equatorial spacecraft), reusing the SAME
        // sun_dec_rad/xtemp_k/kp this sweep's own native call resolved.
        if alt_km > 125.0 {
            let r_sc_km = [r_km_mag, 0.0, 0.0];
            let r_sun_km = [sun_m[0] / 1e3, sun_m[1] / 1e3, sun_m[2] / 1e3];
            let geom = av_orbital::jacchia_roberts::SunGeometry { sun_dec_rad, geo_lat_rad: 0.0 };
            let exo_500 = av_orbital::jacchia_roberts::exotherm(r_sc_km, r_sun_km, weather.kp, xtemp_k, 500.0, geom, &cb).expect("exotherm@500");
            let exo = av_orbital::jacchia_roberts::exotherm(r_sc_km, r_sun_km, weather.kp, xtemp_k, alt_km, geom, &cb).expect("exotherm@alt");
            let terms = av_orbital::jacchia_roberts::rho_high_species_terms(alt_km, &exo, exo_500.exotemp, sun_dec_rad, 0.0, &cb);
            eprintln!("[n3-species] {alt_km:.0} N2={:e} Ar={:e} He={:e} O2={:e} O={:e} H={:e}", terms[0], terms[1], terms[2], terms[3], terms[4], terms[5]);
        }
        idx += 1;
        alt_km += 20.0;
    }

    // --- Part 2: weather-sensitivity, at two fixed altitudes (400 km, below hydrogen's 500 km
    // cutoff; 700 km, above it) ---
    for &probe_alt_km in &[400.0_f64, 700.0] {
        let r_km_mag = cb.equatorial_radius_km + probe_alt_km;
        let v_km_s = (mu / r_km_mag).sqrt();
        let state_km = [r_km_mag, 0.0, 0.0, 0.0, v_km_s, 0.0];
        let r_m = [r_km_mag * 1e3, 0.0, 0.0];

        for &(label, f107, f107a, kp) in &[("baseline", 150.0_f64, 150.0_f64, 3.0_f64), ("high-F107", 250.0, 250.0, 3.0), ("high-Kp", 150.0, 150.0, 6.0)] {
            let w = WeatherInputs { f107, f107a, kp };
            let (model, mu_readback, _) = build_gmat_drag_only_model_weather(&gmat, epoch_utc, r_km_mag, v_km_s, &format!("W{}{}", probe_alt_km as i64, label.replace('-', "")), w);
            let rho_gmat = gmat_implied_density(&model, &state_km, 0.0, mu_readback, 2.2, 5.0, 500.0);
            let rho_native = density_kg_m3(r_m, sun_m, &IdentityRotation, t_tai_ns, &w, &cb).expect("native density");
            let rel = (rho_native - rho_gmat).abs() / rho_gmat;
            eprintln!("[n3-weather-sens] alt={probe_alt_km:.0}km case={label} F107={f107} F107A={f107a} Kp={kp} rho_gmat={rho_gmat:e} rho_native={rho_native:e} relative_diff={rel:e}");
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Acceleration agreement: JGM2 8x8 + DragForce/JacchiaRoberts, native vs GetDerivatives.
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
        EarthGravityModelInfo { id: "native.orbital.n3b_drag_test".to_string(), version: env!("CARGO_PKG_VERSION").to_string(), goldens: vec!["leo_400km_jacchia_roberts".to_string()] },
    )
    .expect("EarthGravityModel construction")
    // An EMPTY third-body list, deliberately: this golden's own force model has NO point-mass
    // perturbations (`force_model.point_masses == []`), so passing any DeBody here would add
    // an acceleration GMAT's own arc never applied. `with_drag` only needs the BOUND
    // `DeEphemeris` HANDLE (for the Sun's position, the diurnal exospheric-temperature bulge)
    // -- `with_third_bodies`'s own `bodies` list controls PERTURBATIONS separately (see
    // `EarthGravityModel::derivatives`'s own `for third in &tb.bodies` loop, which iterates
    // zero times here), matching `with_srp`'s identical precedent in `tests/srp_goldens.rs`
    // whenever a golden's own force model omits Sun/Luna as point masses.
    .with_third_bodies(&DeEphemeris::locate_gmat_root().expect("GMAT_ROOT set").join("data/planetary_ephem/de/leDE1941.405"), &[])
    .expect("with_third_bodies (empty list -- only the DE ephemeris handle is needed, for the Sun's position)")
    .with_drag(av_orbital::AtmosphereChoice::JacchiaRoberts, weather_inputs, drag_area_m2, cd, mass_kg)
    .expect("with_drag")
}

fn build_gmat_derivative_model(gmat: &Gmat, epoch_utc: &str, initial_state_km: &[f64], spacecraft: &std::collections::BTreeMap<String, f64>, force_model: &ForceModelCfg, namespace: &str) -> gmat_sys::DerivativeModel {
    let sat = gmat.construct("Spacecraft", &format!("N3DAccel{namespace}")).unwrap();
    sat.set_str("DateFormat", "UTCGregorian").unwrap();
    sat.set_str("Epoch", epoch_utc).unwrap();
    sat.set_str("CoordinateSystem", "EarthMJ2000Eq").unwrap();
    sat.set_str("DisplayStateType", "Cartesian").unwrap();
    for (field, v) in ["X", "Y", "Z", "VX", "VY", "VZ"].iter().zip(initial_state_km) {
        sat.set_real(field, *v).unwrap();
    }
    // Ballistic set (question 81: a seed is a vehicle) -- every field the golden's own
    // `spacecraft` map carries EXCEPT the Keplerian elements, which conflict with this
    // spacecraft's own `DisplayStateType = Cartesian` (GMAT: "you have set orbital state
    // elements not contained in the same state type" outside a mission sequence) -- matching
    // `tests/srp_goldens.rs`'s own identical filter.
    for (k, v) in spacecraft {
        if k != "SMA" && k != "ECC" && k != "INC" && k != "RAAN" && k != "AOP" && k != "TA" {
            sat.set_real(k, *v).unwrap();
        }
    }

    let fm = gmat.construct("ForceModel", &format!("N3DAccelFM{namespace}")).unwrap();
    fm.set_str("CentralBody", &force_model.central_body).unwrap();
    let grav = gmat.construct("GravityField", "").unwrap();
    grav.set_str("BodyName", &force_model.central_body).unwrap();
    grav.set_str("PotentialFile", &force_model.gravity.file).unwrap();
    grav.set_int("Degree", force_model.gravity.degree).unwrap();
    grav.set_int("Order", force_model.gravity.order).unwrap();
    fm.add_force(&grav).unwrap();

    let df = gmat.construct("DragForce", &format!("N3DAccelDrag{namespace}")).unwrap();
    df.set_str("AtmosphereModel", "JacchiaRoberts").unwrap();
    df.set_str("HistoricWeatherSource", "ConstantFluxAndGeoMag").unwrap();
    df.set_str("PredictedWeatherSource", "ConstantFluxAndGeoMag").unwrap();
    df.set_real("F107", 150.0).unwrap();
    df.set_real("F107A", 150.0).unwrap();
    df.set_real("MagneticIndex", 3.0).unwrap();
    let atmos = gmat.construct("JacchiaRoberts", &format!("N3DAccelAtmos{namespace}")).unwrap();
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
    let native = build_native_model(&golden.force_model, "N3DAccel4", drag_area, cd, mass_kg, &golden.weather_readback);

    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup (idempotent)");
    let gmat_model = build_gmat_derivative_model(&gmat, &golden.epoch_utc, &golden.initial_state, &golden.spacecraft, &golden.force_model, "N3DAccel4");

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
        eprintln!("[n3-drag-4epoch] dt={dt_s:.1}s native={native_accel:?} gmat={gmat_accel:?} |diff|={abs_diff:.6e} m/s^2 (relative {rel_diff:.6e})");
        max_abs = max_abs.max(abs_diff);
        max_rel = max_rel.max(rel_diff);
    }
    eprintln!("[n3-drag-4epoch] acceleration agreement over 4 epochs: max |diff| = {max_abs:.6e} m/s^2, max relative = {max_rel:.6e}");

    // The gravity-only floor (N1/N2) is ~4e-15 m/s^2 / ~5e-16 relative; drag is a much larger,
    // less analytically clean force (the whole atmosphere model, including density's own
    // sensitivity to the geodetic-height iteration), so this bound is set above that floor,
    // just above the MEASURED value (this task's own rule): measured 3.855085e-10 m/s^2 max
    // abs / 4.436967e-11 max relative over the 4 epochs (debug build, this host) -- still five
    // orders of magnitude tighter than the density comparison's own ~1e-3 relative agreement,
    // because the four epochs here are all near the SAME altitude/velocity regime the golden's
    // own arc starts at, where density's altitude-sensitivity does not yet dominate.
    const TOLERANCE_ABS_M_S2: f64 = 8e-10;
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
    let model = build_native_model(&golden.force_model, "N3DTraj", drag_area, cd, mass_kg, &golden.weather_readback);

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
        "[n3-drag-trajectory] native Dopri5 (JGM2 8x8 + DragForce/JacchiaRoberts) vs GMAT PrinceDormand78 over {} s: position residual = {dr:.6e} m, velocity residual = {dv:.6e} m/s, wall time {wall_s:.3} s",
        golden.duration_s
    );
    eprintln!("[n3-drag-trajectory] residual vs the golden's own recorded tolerance: {dr:e} m / {:e} m, {dv:e} m/s / {:e} m/s", golden.tolerance_m, golden.tolerance_mps);
    assert!(dr < golden.tolerance_m, "position residual {dr:e} m exceeds the golden's own recorded {:e} m tolerance", golden.tolerance_m);
    assert!(dv < golden.tolerance_mps, "velocity residual {dv:e} m/s exceeds the golden's own recorded {:e} m/s tolerance", golden.tolerance_mps);
}

// ---------------------------------------------------------------------------------------------
// The golden's own recorded weather-file SHA-256 matches the live file (a second pin, at the
// golden level rather than only crate::weather's own unit test).
// ---------------------------------------------------------------------------------------------

#[test]
fn golden_records_the_live_weather_file_sha256() {
    let golden = load_golden();
    let f = SpaceWeatherFile::open(&weather_path()).expect("open CSSI file");
    assert_eq!(golden.weather_file_sha256, f.sha256(), "the golden's own recorded weather_file_sha256 does not match the live file's SHA-256");
}
