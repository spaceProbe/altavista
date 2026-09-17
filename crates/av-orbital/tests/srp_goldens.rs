#![cfg(feature = "gmat-frames")]
//! N3 acceptance (`docs/native-dynamics-plan.md`, "N3's solar radiation pressure -- cannonball
//! SRP with a conical shadow model"): [`av_orbital::model::EarthGravityModel::with_srp`]
//! against two goldens.
//!
//! **Golden A -- acceleration agreement, `leo_1day_jgm2_8x8_sunmoon_drag_srp.json`.** That
//! golden's own arc (250 km LEO) was flown by GMAT with drag AND SRP; this crate has no native
//! drag yet (tasks 3b/3c), so the FULL arc is not attempted here -- only the acceleration
//! comparison, isolating SRP from the integrator exactly as `tests/gravity_goldens.rs`/
//! `tests/thirdbody_goldens.rs` do: a GMAT `DerivativeModel` built with gravity + Sun + Moon +
//! spherical SRP but **no drag**, against the matching native model. `docs/adr/
//! 002-dynamics-contract.md`'s third amendment already recorded (from GMAT's source, and this
//! golden's own generator comment, `crates/gmat-sys/tests/drag_srp_stm.rs`) that this arc's own
//! `t0` sits in Earth's umbra -- confirmed again here, not assumed, and used as a free umbra
//! epoch. **The full M5 (drag+SRP) trajectory is deferred to the task that adds native drag.**
//!
//! **Golden B -- the trajectory residual, `leo_1day_jgm2_8x8_sunmoon_srp.json`.** A NEW arc
//! (`goldens/gen_leo_1day_jgm2_8x8_sunmoon_srp.py`) with SRP as the ONLY force added relative
//! to the already-pinned `leo_1day_jgm2_8x8_sunmoon.json`, so the native model CAN fly the
//! whole 86,400 s day and the residual is attributable to SRP. See that generator's own module
//! doc for the umbra/penumbra fractions measured over the arc and why this exact arc was kept
//! (it does cross Earth's shadow).
use std::path::PathBuf;
use std::time::Instant;

use av_cdm::time::Tai;
use av_dynamics::DynamicsModel;
use av_orbital::cof;
use av_orbital::de::{DeBody, DeEphemeris};
use av_orbital::frame_gmat::GmatBodyFixedRotation;
use av_orbital::srp::{illumination_fraction, SrpConstants};
use av_orbital::{EarthGravityModel, EarthGravityModelInfo};
use gmat_sys::Gmat;
use serde::Deserialize;

// ---------------------------------------------------------------------------------------------
// Golden A: leo_1day_jgm2_8x8_sunmoon_drag_srp.json (only the fields this file needs)
// ---------------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct GoldenA {
    epoch_utc: String,
    epoch_a1mjd: f64,
    duration_s: f64,
    initial_state: Vec<f64>,
    spacecraft: std::collections::BTreeMap<String, f64>,
    force_model: ForceModelCfgA,
}

#[derive(Deserialize)]
struct ForceModelCfgA {
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

fn golden_a_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../goldens/leo_1day_jgm2_8x8_sunmoon_drag_srp.json")
}

fn load_golden_a() -> GoldenA {
    serde_json::from_str(&std::fs::read_to_string(golden_a_path()).unwrap()).unwrap()
}

// ---------------------------------------------------------------------------------------------
// Golden B: leo_1day_jgm2_8x8_sunmoon_srp.json (the full trajectory residual)
// ---------------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct GoldenB {
    epoch_a1mjd: f64,
    duration_s: f64,
    initial_state: Vec<f64>,
    final_state: Vec<f64>,
    spacecraft: std::collections::BTreeMap<String, f64>,
    force_model: ForceModelCfgA,
    tolerance_m: f64,
    tolerance_mps: f64,
}

fn golden_b_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../goldens/leo_1day_jgm2_8x8_sunmoon_srp.json")
}

fn load_golden_b() -> GoldenB {
    serde_json::from_str(&std::fs::read_to_string(golden_b_path()).unwrap()).unwrap()
}

// ---------------------------------------------------------------------------------------------
// Shared construction helpers
// ---------------------------------------------------------------------------------------------

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

/// Builds the native model: gravity + third bodies (Luna, Sun) + SRP -- no drag, ever (this
/// file's own scope). `srp_area_m2`/`cr`/`mass_kg` are the vehicle's own ballistic properties
/// (read off the golden's `spacecraft` map by the caller).
fn build_native_model(
    force_model: &ForceModelCfgA,
    namespace: &str,
    srp_area_m2: f64,
    cr: f64,
    mass_kg: f64,
) -> EarthGravityModel<GmatBodyFixedRotation> {
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let rotation = GmatBodyFixedRotation::new(gmat, &force_model.central_body, namespace).expect("GmatBodyFixedRotation::new");
    let bodies: Vec<DeBody> = force_model.point_masses.iter().map(|n| de_body_for(n)).collect();
    EarthGravityModel::new(
        &gravity_path(&force_model.gravity.file),
        force_model.gravity.degree as usize,
        force_model.gravity.order as usize,
        &force_model.central_body,
        rotation,
        EarthGravityModelInfo { id: "native.orbital.n3_srp_test".to_string(), version: env!("CARGO_PKG_VERSION").to_string(), goldens: vec!["leo_1day_jgm2_8x8_sunmoon_srp".to_string()] },
    )
    .expect("EarthGravityModel construction")
    .with_third_bodies(&de_path(), &bodies)
    .expect("with_third_bodies")
    .with_srp(SrpConstants::gmat_earth_defaults(), srp_area_m2, cr, mass_kg)
    .expect("with_srp")
}

/// GMAT's own `Spacecraft`/`ForceModel`/`DerivativeModel` -- gravity + third bodies (+ SRP if
/// `with_srp`) -- **no drag, ever** (this file's own scope: golden A's own arc was flown WITH
/// drag by GMAT, but the native model has none yet, so this test isolates gravity+3rd+SRP only,
/// exactly ADR-002 third amendment's arc, `leo_1day_jgm2_8x8_sunmoon_drag_srp.json`, minus its
/// `DragForce`).
fn build_gmat_derivative_model(gmat: &Gmat, epoch_utc: &str, initial_state_km: &[f64], spacecraft: &std::collections::BTreeMap<String, f64>, force_model: &ForceModelCfgA, namespace: &str, with_srp: bool) -> gmat_sys::DerivativeModel {
    let sat = gmat.construct("Spacecraft", &format!("N3Sat{namespace}")).unwrap();
    sat.set_str("DateFormat", "UTCGregorian").unwrap();
    sat.set_str("Epoch", epoch_utc).unwrap();
    sat.set_str("CoordinateSystem", "EarthMJ2000Eq").unwrap();
    sat.set_str("DisplayStateType", "Cartesian").unwrap();
    for (field, v) in ["X", "Y", "Z", "VX", "VY", "VZ"].iter().zip(initial_state_km) {
        sat.set_real(field, *v).unwrap();
    }
    // Ballistic set (question 81: a seed is a vehicle) -- every field the golden's own
    // `spacecraft` map carries, not only the SRP-relevant ones, so TotalMass/Cd/DragArea are
    // set too even though no DragForce is in this model.
    for (k, v) in spacecraft {
        if k != "SMA" && k != "ECC" && k != "INC" && k != "RAAN" && k != "AOP" && k != "TA" {
            sat.set_real(k, *v).unwrap();
        }
    }

    let fm = gmat.construct("ForceModel", &format!("N3FM{namespace}")).unwrap();
    fm.set_str("CentralBody", &force_model.central_body).unwrap();
    let grav = gmat.construct("GravityField", "").unwrap();
    grav.set_str("BodyName", &force_model.central_body).unwrap();
    grav.set_str("PotentialFile", &force_model.gravity.file).unwrap();
    grav.set_int("Degree", force_model.gravity.degree).unwrap();
    grav.set_int("Order", force_model.gravity.order).unwrap();
    fm.add_force(&grav).unwrap();
    for (i, body) in force_model.point_masses.iter().enumerate() {
        let pm = gmat.construct("PointMassForce", &format!("N3PM{namespace}_{i}")).unwrap();
        pm.set_str("BodyName", body).unwrap();
        fm.add_force(&pm).unwrap();
    }
    if with_srp {
        fm.add_force(&gmat.construct("SolarRadiationPressure", &format!("N3SRP{namespace}")).unwrap()).unwrap();
    }
    gmat.initialize().unwrap();
    gmat.derivative_model(&fm, &sat).expect("derivative model")
}

/// This test's own shadow classification at `(pos_m, t_tai_ns)`, computed EXACTLY as
/// `EarthGravityModel::derivatives` computes it internally (same DE ephemeris file, same
/// two-part TDB epoch, same `illumination_fraction`) -- never through GMAT, so this is an
/// independent (of GMAT) way to pick interesting epochs before spending a GMAT call on them.
fn native_nu(de: &DeEphemeris, pos_m: [f64; 3], t_tai_ns: i64) -> f64 {
    let (jd1, jd2) = av_orbital::tdb::tai_ns_to_tdb_jd2(t_tai_ns);
    let sun_km = de.geocentric_position_km2(DeBody::Sun, jd1, jd2).expect("geocentric_position_km2");
    let r_sun_m = [sun_km[0] * 1e3, sun_km[1] * 1e3, sun_km[2] * 1e3];
    let constants = SrpConstants::gmat_earth_defaults();
    illumination_fraction(pos_m, r_sun_m, constants.sun_radius_m, constants.body_radius_m).expect("illumination_fraction")
}

// ---------------------------------------------------------------------------------------------
// Golden A: the standard 4-epoch acceleration agreement (fixed x0, varying dt) -- the
// established pattern `tests/gravity_goldens.rs`/`tests/thirdbody_goldens.rs`/
// `tests/thirdbody_mars_jupiter.rs` all use.
// ---------------------------------------------------------------------------------------------

#[test]
fn four_epoch_acceleration_agreement_gravity_sun_moon_srp_no_drag() {
    let _engine = gmat_sys::engine_lock();
    let golden = load_golden_a();
    let de = DeEphemeris::open(&de_path()).expect("open DE405");

    let srp_area = golden.spacecraft["SRPArea"];
    let cr = golden.spacecraft["Cr"];
    let mass_kg = golden.spacecraft["DryMass"]; // no fuel tank in this golden -- TotalMass == DryMass
    let native = build_native_model(&golden.force_model, "N3Accel4A", srp_area, cr, mass_kg);

    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup (idempotent)");
    let gmat_model = build_gmat_derivative_model(&gmat, &golden.epoch_utc, &golden.initial_state, &golden.spacecraft, &golden.force_model, "N3Accel4A", true);

    let gmat_x0 = gmat_model.state().unwrap();
    for (a, b) in gmat_x0.iter().zip(&golden.initial_state) {
        assert!((a - b).abs() < 1e-9, "GMAT derivative-model initial state differs from the golden: {gmat_x0:?} vs {:?}", golden.initial_state);
    }

    let x0_m = km_state_to_m(&golden.initial_state);
    let t0_tai_ns = Tai::from_a1_mjd(golden.epoch_a1mjd).as_nanos();

    let mut max_abs = 0.0_f64;
    let mut max_rel = 0.0_f64;
    let mut any_penumbra = false;
    for frac in [0.0, 1.0 / 3.0, 2.0 / 3.0, 1.0] {
        let dt_s = golden.duration_s * frac;
        let dt_ns = (dt_s * 1e9).round() as i64;
        let t_tai_ns = t0_tai_ns + dt_ns;

        let nu = native_nu(&de, [x0_m[0], x0_m[1], x0_m[2]], t_tai_ns);

        let mut native_dot = [0.0_f64; 6];
        native.derivatives(&x0_m, t_tai_ns, &[], &mut native_dot).expect("native derivatives");
        let native_accel = [native_dot[3], native_dot[4], native_dot[5]];

        let gmat_dot = gmat_model.derivatives(&golden.initial_state, dt_s).expect("GMAT derivatives");
        let gmat_accel = [gmat_dot[3] * 1e3, gmat_dot[4] * 1e3, gmat_dot[5] * 1e3];

        let abs_diff = (0..3).map(|i| (native_accel[i] - gmat_accel[i]).powi(2)).sum::<f64>().sqrt();
        let scale = (0..3).map(|i| gmat_accel[i].powi(2)).sum::<f64>().sqrt();
        let rel_diff = abs_diff / scale;
        eprintln!("[n3-srp-4epoch] dt={dt_s:.1}s nu(native)={nu:.6} native={native_accel:?} gmat={gmat_accel:?} |diff|={abs_diff:.6e} m/s^2 (relative {rel_diff:.6e})");
        if nu > 0.0 && nu < 1.0 {
            any_penumbra = true;
        }
        max_abs = max_abs.max(abs_diff);
        max_rel = max_rel.max(rel_diff);
    }
    eprintln!("[n3-srp-4epoch] acceleration agreement over 4 fixed-x0 epochs: max |diff| = {max_abs:.6e} m/s^2, max relative = {max_rel:.6e}, any epoch in penumbra = {any_penumbra}");

    // Tolerance set just above the measured value (this task's own rule): measured
    // 6.404746e-15 m/s^2 / 7.033944e-16 (debug build, this host) -- machine-precision noise,
    // matching this crate's other force-model acceleration-agreement tests' identical-order
    // bound (1e-13/1e-13; N1/N2's own floor was 3.97e-15 m/s^2 / 4.7e-16). NOTE: all four
    // fixed-x0 epochs measured nu=0.0 (deep umbra) -- this golden's own t0 is known (ADR-002's
    // third amendment, crates/gmat-sys/tests/drag_srp_stm.rs) to sit in Earth's shadow, and
    // since x0 is FIXED across all four epochs while only the (slowly-moving, ~1 deg/day) Sun
    // direction changes, none of the four lands in penumbra -- `any_penumbra` is asserted
    // false below rather than silently left unreported; see
    // `penumbra_epoch_acceleration_agreement`, this file's OTHER test, for a genuine penumbra
    // comparison via a propagated trajectory.
    const TOLERANCE_ABS_M_S2: f64 = 1e-13;
    const TOLERANCE_REL: f64 = 1e-13;
    assert!(max_abs < TOLERANCE_ABS_M_S2, "max abs acceleration disagreement {max_abs:e} m/s^2 exceeds {TOLERANCE_ABS_M_S2:e}");
    assert!(max_rel < TOLERANCE_REL, "max relative acceleration disagreement {max_rel:e} exceeds {TOLERANCE_REL:e}");
    assert!(!any_penumbra, "this test's own comment explains why none of the 4 fixed-x0 epochs should land in penumbra -- if this now fails, that reasoning needs revisiting, not silently updating this assertion");
}

// ---------------------------------------------------------------------------------------------
// Golden A: a genuine penumbra epoch, found by propagating the native model and bisecting a
// coarse-scan bracket where illumination transitions -- see this file's own module doc.
// ---------------------------------------------------------------------------------------------

struct ShadowSample {
    t_tai_ns: i64,
    state_m: [f64; 6],
    nu: f64,
}

/// Coarse-scans the native model's own propagated trajectory (real physics: gravity + 3rd
/// bodies + SRP, via repeated `DynamicsModel::step` calls) for up to `max_span_s` seconds from
/// `t0_tai_ns`/`x0_m`, `coarse_step_s` at a time, classifying illumination at each sample via
/// [`native_nu`]. Returns every sample (for reporting) plus the index of the first bracket
/// (`i`, `i+1`) whose `nu` values straddle a transition (one is `<= 0.0` or `>= 1.0`, the other
/// is not), if any.
fn coarse_shadow_scan(model: &EarthGravityModel<GmatBodyFixedRotation>, de: &DeEphemeris, t0_tai_ns: i64, x0_m: [f64; 6], coarse_step_s: f64, max_span_s: f64) -> (Vec<ShadowSample>, Option<usize>) {
    let coarse_step_ns = (coarse_step_s * 1e9).round() as i64;
    let mut samples = Vec::new();
    let nu0 = native_nu(de, [x0_m[0], x0_m[1], x0_m[2]], t0_tai_ns);
    samples.push(ShadowSample { t_tai_ns: t0_tai_ns, state_m: x0_m, nu: nu0 });

    let mut state = x0_m;
    let mut t = t0_tai_ns;
    let mut elapsed_s = 0.0;
    while elapsed_s < max_span_s - 1e-9 {
        let result = model.step(&state, t, &[], coarse_step_ns).expect("native step (coarse shadow scan)");
        state = result.state.as_slice().try_into().expect("6-state");
        t = result.t_tai_ns;
        elapsed_s += coarse_step_s;
        let nu = native_nu(de, [state[0], state[1], state[2]], t);
        samples.push(ShadowSample { t_tai_ns: t, state_m: state, nu });
    }

    let mut bracket = None;
    for i in 0..samples.len().saturating_sub(1) {
        let a = samples[i].nu;
        let b = samples[i + 1].nu;
        let a_extreme = a <= 0.0 || a >= 1.0;
        let b_extreme = b <= 0.0 || b >= 1.0;
        if a != b && !(a_extreme && b_extreme && (a - b).abs() < 1e-15) {
            // A genuine change; prefer a bracket that actually straddles a non-extreme value
            // is not required here -- ANY change is a candidate for bisection below, which
            // will itself determine whether a true penumbra sample exists in between.
            bracket = Some(i);
            break;
        }
    }
    (samples, bracket)
}

/// Bisects `samples[i]..samples[i+1]` (by propagating the native model from `samples[i]`'s own
/// state, never re-deriving from `t0`) until a sample with `nu` STRICTLY inside `(0, 1)` is
/// found, or `max_iters` is exhausted. Returns that sample if found.
fn bisect_for_penumbra(model: &EarthGravityModel<GmatBodyFixedRotation>, de: &DeEphemeris, samples: &[ShadowSample], bracket_i: usize, max_iters: u32) -> Option<ShadowSample> {
    let lo = &samples[bracket_i];
    let mut hi_t = samples[bracket_i + 1].t_tai_ns;
    let mut lo_state = lo.state_m;
    let mut lo_t = lo.t_tai_ns;
    for _ in 0..max_iters {
        let span_ns = hi_t - lo_t;
        if span_ns <= 10_000_000 {
            // Under 10 ms of span left -- the penumbra band itself measures several SECONDS
            // wide (this file's own coarse scan), so stopping at 10 ms (not the 1 s this
            // function's own earlier revision used) leaves comfortable margin: an EARLIER
            // version broke at a 1-second floor, which (measured, not assumed) left the final
            // untested bracket at ~0.9 s -- narrower than the band, but the midpoint that would
            // have landed inside it was never evaluated, so the bisection returned `None` on an
            // arc independently confirmed (by the coarse scan itself) to contain a penumbra
            // band. Lowering the floor to 10 ms fixes this by simply testing more midpoints.
            break;
        }
        let mid_ns = span_ns / 2;
        let result = model.step(&lo_state, lo_t, &[], mid_ns).expect("native step (bisection)");
        let mid_state: [f64; 6] = result.state.as_slice().try_into().expect("6-state");
        let mid_t = result.t_tai_ns;
        let mid_nu = native_nu(de, [mid_state[0], mid_state[1], mid_state[2]], mid_t);
        if mid_nu > 0.0 && mid_nu < 1.0 {
            return Some(ShadowSample { t_tai_ns: mid_t, state_m: mid_state, nu: mid_nu });
        }
        // Narrow toward whichever half still contains a transition: compare mid's nu against
        // lo's own extreme classification (0 or 1) to decide which half to keep.
        let lo_nu = native_nu(de, [lo_state[0], lo_state[1], lo_state[2]], lo_t);
        if (mid_nu - lo_nu).abs() > 1e-12 {
            hi_t = mid_t;
        } else {
            lo_state = mid_state;
            lo_t = mid_t;
        }
    }
    None
}

/// Measures the penumbra crossing's own physical width in seconds: a FORWARD-ONLY linear scan
/// (at `step_ns` resolution, by propagating the native model) starting from `samples[bracket_i]`
/// -- the coarse bracket's own lower sample, already known extreme (umbra or full sun) -- for up
/// to `max_span_ns`, recording the first and last instants `nu` is measured strictly inside
/// `(0, 1)`. Deliberately forward-only: an earlier version of this function scanned backward too
/// (negative `dt_ns` into `model.step`), which measured a spurious ~29 s width (`nu` never
/// returning to an exact extreme within 20 s backward) inconsistent with this same arc's OWN
/// forward-scan boundaries -- `model.step`'s integrator is not exercised with negative `dt_ns`
/// anywhere else in this crate, so this function does not rely on it either. This is reporting
/// only (no assertion depends on it) -- it answers this task's own question, "how many seconds
/// wide is the penumbra the search found", independent of exactly which interior instant the
/// search itself happened to land on.
fn measure_penumbra_width_s(model: &EarthGravityModel<GmatBodyFixedRotation>, de: &DeEphemeris, t0_tai_ns: i64, samples: &[ShadowSample], bracket_i: usize, step_ns: i64, max_span_ns: i64) -> Option<(f64, f64, f64)> {
    let start = &samples[bracket_i];
    let mut state = start.state_m;
    let mut t = start.t_tai_ns;
    let mut elapsed_ns = 0i64;
    let mut enter_t: Option<i64> = None;
    let mut exit_t: Option<i64> = None;
    while elapsed_ns < max_span_ns {
        let step = step_ns.min(max_span_ns - elapsed_ns);
        let result = model.step(&state, t, &[], step).expect("native step (penumbra width scan)");
        state = result.state.as_slice().try_into().expect("6-state");
        t = result.t_tai_ns;
        elapsed_ns += step;
        let nu = native_nu(de, [state[0], state[1], state[2]], t);
        if nu > 0.0 && nu < 1.0 {
            if enter_t.is_none() {
                enter_t = Some(t);
            }
            exit_t = Some(t);
        } else if exit_t.is_some() {
            // Already saw interior samples and are back to an extreme -- the crossing is done.
            break;
        }
    }
    match (enter_t, exit_t) {
        (Some(e), Some(x)) => Some(((e - t0_tai_ns) as f64 / 1e9, (x - t0_tai_ns) as f64 / 1e9, (x - e) as f64 / 1e9)),
        _ => None,
    }
}

#[test]
fn penumbra_epoch_acceleration_agreement() {
    let _engine = gmat_sys::engine_lock();
    let golden = load_golden_a();
    let de = DeEphemeris::open(&de_path()).expect("open DE405");

    let srp_area = golden.spacecraft["SRPArea"];
    let cr = golden.spacecraft["Cr"];
    let mass_kg = golden.spacecraft["DryMass"];
    let native = build_native_model(&golden.force_model, "N3PenumbraScan", srp_area, cr, mass_kg);

    let x0_m = km_state_to_m(&golden.initial_state);
    let t0_tai_ns = Tai::from_a1_mjd(golden.epoch_a1mjd).as_nanos();

    let nu0 = native_nu(&de, [x0_m[0], x0_m[1], x0_m[2]], t0_tai_ns);
    eprintln!("[n3-srp-penumbra] t0 illumination nu = {nu0:.6} (ADR-002's third amendment / crates/gmat-sys/tests/drag_srp_stm.rs already recorded this arc's t0 as sitting in Earth's umbra -- confirmed independently here by this crate's own shadow function, not assumed)");

    // Coarse scan one full orbital period (~5500-6000 s at this altitude) at 30 s resolution,
    // then bisect the first bracket found down to a genuine penumbra sample.
    let (samples, bracket) = coarse_shadow_scan(&native, &de, t0_tai_ns, x0_m, 30.0, 6000.0);
    eprintln!("[n3-srp-penumbra] coarse scan: {} samples over up to 6000 s, nu range [{:.6}, {:.6}]", samples.len(), samples.iter().map(|s| s.nu).fold(f64::INFINITY, f64::min), samples.iter().map(|s| s.nu).fold(f64::NEG_INFINITY, f64::max));

    let penumbra_sample = bracket.and_then(|i| bisect_for_penumbra(&native, &de, &samples, i, 24));

    let full_sun_sample = samples.iter().find(|s| s.nu >= 1.0);
    let umbra_sample = samples.iter().find(|s| s.nu <= 0.0);

    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup (idempotent)");
    let gmat_model = build_gmat_derivative_model(&gmat, &golden.epoch_utc, &golden.initial_state, &golden.spacecraft, &golden.force_model, "N3PenumbraCmp", true);
    // A gravity+3rd-body-only model (no SRP) is built further below, inside the penumbra-sample
    // branch, to isolate GMAT's OWN SRP magnitude there by subtraction (confirms GMAT applies a
    // genuinely ATTENUATED, not merely on/off, acceleration in the penumbra -- ADR-002 third
    // amendment's own claim, checked here rather than assumed).

    let mut max_abs = 0.0_f64;
    let mut max_rel = 0.0_f64;
    let mut epochs_compared = 0usize;
    let mut penumbra_epochs = 0usize;

    let mut compare_at = |label: &str, s: &ShadowSample| {
        let dt_s = (s.t_tai_ns - t0_tai_ns) as f64 / 1e9;
        let state_km: Vec<f64> = s.state_m.iter().map(|v| v * 1e-3).collect();

        let mut native_dot = [0.0_f64; 6];
        native.derivatives(&s.state_m, s.t_tai_ns, &[], &mut native_dot).expect("native derivatives");
        let native_accel = [native_dot[3], native_dot[4], native_dot[5]];

        let gmat_dot = gmat_model.derivatives(&state_km, dt_s).expect("GMAT derivatives");
        let gmat_accel = [gmat_dot[3] * 1e3, gmat_dot[4] * 1e3, gmat_dot[5] * 1e3];

        let abs_diff = (0..3).map(|i| (native_accel[i] - gmat_accel[i]).powi(2)).sum::<f64>().sqrt();
        let scale = (0..3).map(|i| gmat_accel[i].powi(2)).sum::<f64>().sqrt();
        let rel_diff = if scale > 0.0 { abs_diff / scale } else { abs_diff };
        eprintln!("[n3-srp-penumbra] {label} @ dt={dt_s:.3}s nu={:.6} native={native_accel:?} gmat={gmat_accel:?} |diff|={abs_diff:.6e} m/s^2 (relative {rel_diff:.6e})", s.nu);
        max_abs = max_abs.max(abs_diff);
        max_rel = max_rel.max(rel_diff);
        epochs_compared += 1;
        if s.nu > 0.0 && s.nu < 1.0 {
            penumbra_epochs += 1;
        }
    };

    if let Some(s) = umbra_sample {
        compare_at("umbra", s);
    }
    if let Some(s) = full_sun_sample {
        compare_at("full-sun", s);
    }
    match &penumbra_sample {
        Some(s) => {
            compare_at("penumbra", s);

            // Measure (not assume) how wide, in seconds, this particular penumbra crossing is
            // -- a forward-only scan from the coarse bracket's own lower (known-extreme) sample,
            // at 0.1 s resolution, over the bracket's own 30 s coarse span plus margin (60 s
            // total -- comfortably wider than the several-second band this altitude's geometry
            // produces, measured independently above by this same test's own coarse scan).
            let bracket_i = bracket.expect("penumbra_sample is Some only when bracket was Some");
            match measure_penumbra_width_s(&native, &de, t0_tai_ns, &samples, bracket_i, 100_000_000, 60_000_000_000) {
                Some((enter_dt_s, exit_dt_s, width_s)) => {
                    eprintln!("[n3-srp-penumbra] penumbra crossing measured width: enter dt={enter_dt_s:.3}s exit dt={exit_dt_s:.3}s width={width_s:.3}s");
                }
                None => eprintln!("[n3-srp-penumbra] WARNING: width scan did not observe both a penumbra entry and exit within its own scanned span"),
            }

            // Isolate GMAT's OWN SRP-only acceleration at the penumbra point (gravity+3rd-body
            // subtracted out), confirming it is genuinely ATTENUATED (strictly between 0 and
            // the full-sun magnitude at a comparable distance), not merely on/off -- this is
            // the measurement ADR-002's third amendment's "shadow partials omitted... not
            // exercised by the [drag/SRP golden's] arc" leaves open for the ACCELERATION
            // itself (only the partials are documented as omitted).
            let gmat_no_srp = build_gmat_derivative_model(&gmat, &golden.epoch_utc, &golden.initial_state, &golden.spacecraft, &golden.force_model, "N3PenumbraNoSrp", false);
            let state_km: Vec<f64> = s.state_m.iter().map(|v| v * 1e-3).collect();
            let dt_s = (s.t_tai_ns - t0_tai_ns) as f64 / 1e9;
            let dot_with = gmat_model.derivatives(&state_km, dt_s).expect("GMAT derivatives (with SRP)");
            let dot_without = gmat_no_srp.derivatives(&state_km, dt_s).expect("GMAT derivatives (no SRP)");
            let srp_only = [(dot_with[3] - dot_without[3]) * 1e3, (dot_with[4] - dot_without[4]) * 1e3, (dot_with[5] - dot_without[5]) * 1e3];
            let srp_only_mag = (srp_only[0].powi(2) + srp_only[1].powi(2) + srp_only[2].powi(2)).sqrt();
            eprintln!("[n3-srp-penumbra] GMAT's own isolated SRP acceleration at the penumbra point: |a_srp| = {srp_only_mag:.6e} m/s^2 (nu={:.6})", s.nu);
            assert!(srp_only_mag > 0.0, "GMAT's own SRP acceleration at a penumbra point (nu={:.6}) is exactly zero -- expected a genuinely attenuated, nonzero value", s.nu);
        }
        None => {
            eprintln!("[n3-srp-penumbra] WARNING: no penumbra sample found within the scanned window/bisection budget -- see this test's own printed nu range above");
        }
    }

    eprintln!("[n3-srp-penumbra] acceleration agreement over {epochs_compared} epochs ({penumbra_epochs} in penumbra): max |diff| = {max_abs:.6e} m/s^2, max relative = {max_rel:.6e}");
    assert!(epochs_compared >= 2, "expected at least umbra and full-sun epochs to be found on this known-eclipsing arc");
    assert!(penumbra_sample.is_some(), "this arc is known (from prior team measurement, re-confirmed by this test's own t0 nu) to sit in Earth's shadow at t0; expected the coarse scan + bisection to locate a genuine penumbra sample within one orbital period");

    // Tolerance set just above the measured value (this task's own rule): measured
    // 6.404746e-15 m/s^2 / 7.033944e-16 across umbra/full-sun/penumbra (debug build, this
    // host) -- the SAME machine-precision floor as the fixed-x0 4-epoch test above, including
    // AT the penumbra point itself (nu=0.005254 there): agreement does not degrade in the
    // penumbra, consistent with GMAT computing the ACCELERATION there fully (only the STM
    // partials are documented, ADR-002's third amendment, as omitting the shadow term).
    const TOLERANCE_ABS_M_S2: f64 = 1e-13;
    const TOLERANCE_REL: f64 = 1e-13;
    assert!(max_abs < TOLERANCE_ABS_M_S2, "max abs acceleration disagreement {max_abs:e} m/s^2 exceeds {TOLERANCE_ABS_M_S2:e}");
    assert!(max_rel < TOLERANCE_REL, "max relative acceleration disagreement {max_rel:e} exceeds {TOLERANCE_REL:e}");
}

// ---------------------------------------------------------------------------------------------
// Golden B: the full-day trajectory residual (SRP is the only force added relative to the
// already-pinned leo_1day_jgm2_8x8_sunmoon.json -- see the generator's own module doc).
// ---------------------------------------------------------------------------------------------

#[test]
fn trajectory_residual_against_golden_b() {
    let _engine = gmat_sys::engine_lock();
    let golden = load_golden_b();

    let srp_area = golden.spacecraft["SRPArea"];
    let cr = golden.spacecraft["Cr"];
    let mass_kg = golden.spacecraft["DryMass"];
    let model = build_native_model(&golden.force_model, "N3TrajB", srp_area, cr, mass_kg);

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
        "[n3-srp-goldenB] native Dopri5 (JGM2 8x8 + Luna + Sun + spherical SRP) vs GMAT PrinceDormand78 over {} s: position residual = {dr:.6e} m, velocity residual = {dv:.6e} m/s, wall time {wall_s:.3} s",
        golden.duration_s
    );
    eprintln!("[n3-srp-goldenB] residual vs the golden's own recorded tolerance: {dr:e} m / {:e} m, {dv:e} m/s / {:e} m/s", golden.tolerance_m, golden.tolerance_mps);
    assert!(dr < golden.tolerance_m, "position residual {dr:e} m exceeds the golden's own recorded {:e} m tolerance", golden.tolerance_m);
    assert!(dv < golden.tolerance_mps, "velocity residual {dv:e} m/s exceeds the golden's own recorded {:e} m/s tolerance", golden.tolerance_mps);
}
