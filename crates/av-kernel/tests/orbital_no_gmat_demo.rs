//! N6 (`docs/native-dynamics-plan.md`, `docs/open-questions.md` question 230): the demo/golden
//! LEO arc (the same physics `drms/leo_1day_golden.system.yaml`/`goldens/
//! leo_1day_jgm2_8x8_sunmoon.json` declare -- JGM2 8x8 Earth gravity, Sun + Moon point masses,
//! one day, `EarthMJ2000Eq`) propagates end to end through [`av_kernel::drm::execute`] with the
//! native `"orbital."`-dispatched model selected instead of the `"gmat."` one, in the
//! `--no-default-features` build -- N6's own exit criterion ("the demo DRM propagates one day
//! with the native model alone in a kernel built without GMAT").
//!
//! This file is deliberately its own, new `SystemDefinition`/`SosConfiguration`/
//! `DesignReferenceMission` (built directly in Rust, hashed here, not loaded from `drms/`)
//! rather than a copy of `drms/leo_1day_golden.*.yaml` with one field changed: `parse_orbital_
//! spec` requires a Cartesian initial state (no GMAT to convert the golden's own Keplerian
//! elements), so the six `spacecraft.SMA/ECC/INC/RAAN/AOP/TA` parameters are replaced by
//! `spacecraft.X/Y/Z/VX/VY/VZ`, computed from the IDENTICAL orbital elements
//! (`goldens/leo_1day_jgm2_8x8_sunmoon.json`'s own `initial_state`: 6878 km / 0.001 / 51.6 deg /
//! 30 deg / 0 / 0) by a standalone Keplerian -> Cartesian conversion below -- deliberately NOT
//! reusing `av_orbital` or any GMAT call to do that conversion, so this test does not validate
//! the native model against itself. `force_model.central_body`/`.gravity_file`/`.gravity_degree`/
//! `.gravity_order`/`.point_masses` and `spacecraft.CoordinateSystem` are copied verbatim from
//! `drms/leo_1day_golden.system.yaml` -- the same string values a `"gmat."`-dispatched instance
//! would declare, N6's own "identical parameters" requirement.
//!
//! This test compiles and runs in EITHER feature state (no `required-features` entry in
//! `Cargo.toml`): under the default (`gmat` on) build it still selects the native model (proving
//! `ModelKind::Orbital` is not somehow gated on), and under `--no-default-features` it is the
//! test whose own binary this task's report runs `otool -L` against.

use std::collections::BTreeMap;

use av_cdm::pb::{Binding, BindingKind, DesignReferenceMission, DrmOptions, ModelBinding, Parameter, Scenario, SosConfiguration, StateComponent, StateSpace, SystemDefinition, SystemInstance, Unit};
use av_kernel::drm::{execute, hash, RunConfig};

fn param(name: &str, value: f64) -> Parameter {
    Parameter { name: name.to_string(), value, ..Default::default() }
}
fn sparam(name: &str, s: &str) -> Parameter {
    Parameter { name: name.to_string(), string_value: s.to_string(), ..Default::default() }
}
fn hashed_system(mut sys: SystemDefinition) -> SystemDefinition {
    sys.hash = hash::canonical_system_hash(&sys);
    sys
}
fn hashed_sos(mut sos: SosConfiguration) -> SosConfiguration {
    sos.hash = hash::canonical_sos_hash(&sos);
    sos
}
fn hashed_drm(mut drm: DesignReferenceMission) -> DesignReferenceMission {
    drm.hash = hash::canonical_drm_hash(&drm);
    drm
}

/// Standard Keplerian -> Cartesian conversion (Vallado's own `R = R3(-Omega) R1(-i) R3(-omega)`
/// composition), written out here independently of `av_orbital`/GMAT so this test's own input
/// state is not derived from the code under test. `mu_m3_s2` is JGM2's own value (`3.986004415e14
/// m^3/s^2`, `docs/native-dynamics-plan.md`'s "Units" section: the same value `data/gravity/
/// earth/JGM2.cof`'s own `POTFIELD` record carries, so this is exactly the `mu` both the GMAT-
/// and native-bound instances actually propagate under -- not a textbook constant that happens
/// to be close). Angles in degrees (GMAT's own convention, matching `drms/leo_1day_golden.
/// system.yaml`'s `spacecraft.SMA/ECC/INC/RAAN/AOP/TA`), `sma_km` in kilometres; returns
/// `[X, Y, Z, VX, VY, VZ]` in km / km/s (GMAT's own `spacecraft.*` unit convention).
fn keplerian_to_cartesian_km(sma_km: f64, ecc: f64, inc_deg: f64, raan_deg: f64, aop_deg: f64, ta_deg: f64, mu_m3_s2: f64) -> [f64; 6] {
    let inc = inc_deg.to_radians();
    let raan = raan_deg.to_radians();
    let aop = aop_deg.to_radians();
    let ta = ta_deg.to_radians();
    let a_m = sma_km * 1000.0;
    let p = a_m * (1.0 - ecc * ecc);
    let r = p / (1.0 + ecc * ta.cos());
    let r_pf = [r * ta.cos(), r * ta.sin()];
    let h = (mu_m3_s2 * p).sqrt();
    let v_pf = [-(mu_m3_s2 / h) * ta.sin(), (mu_m3_s2 / h) * (ecc + ta.cos())];

    let (cr, sr) = (raan.cos(), raan.sin());
    let (ci, si) = (inc.cos(), inc.sin());
    let (co, so) = (aop.cos(), aop.sin());
    let r11 = cr * co - sr * so * ci;
    let r12 = -cr * so - sr * co * ci;
    let r21 = sr * co + cr * so * ci;
    let r22 = -sr * so + cr * co * ci;
    let r31 = so * si;
    let r32 = co * si;
    let rotate = |v: [f64; 2]| -> [f64; 3] { [r11 * v[0] + r12 * v[1], r21 * v[0] + r22 * v[1], r31 * v[0] + r32 * v[1]] };
    let pos_m = rotate(r_pf);
    let vel_ms = rotate(v_pf);
    [pos_m[0] / 1000.0, pos_m[1] / 1000.0, pos_m[2] / 1000.0, vel_ms[0] / 1000.0, vel_ms[1] / 1000.0, vel_ms[2] / 1000.0]
}

/// Sanity-checks [`keplerian_to_cartesian_km`] against the closed-form circular-orbit special
/// case (`ecc = 0`, `ta = 0`, `inc = 0`, `raan = 0`, `aop = 0`): the position is exactly
/// `[a, 0, 0]` and the velocity exactly `[0, sqrt(mu/a), 0]` -- fails against a sign error or a
/// swapped row/column in the rotation composition, which the golden-orbit case below (a non-zero
/// inclination and RAAN) would not by itself distinguish from a merely-different-but-plausible
/// wrong answer.
#[test]
fn keplerian_to_cartesian_matches_the_circular_equatorial_closed_form() {
    let mu = 3.986004415e14_f64;
    let a_km = 7000.0;
    let state = keplerian_to_cartesian_km(a_km, 0.0, 0.0, 0.0, 0.0, 0.0, mu);
    let want_v = (mu / (a_km * 1000.0)).sqrt() / 1000.0;
    assert!((state[0] - a_km).abs() < 1e-9, "x = {}", state[0]);
    assert!(state[1].abs() < 1e-9, "y = {}", state[1]);
    assert!(state[2].abs() < 1e-9, "z = {}", state[2]);
    assert!(state[3].abs() < 1e-9, "vx = {}", state[3]);
    assert!((state[4] - want_v).abs() < 1e-9, "vy = {} want {}", state[4], want_v);
    assert!(state[5].abs() < 1e-9, "vz = {}", state[5]);
}

/// Build the one-instance, GMAT-free-selectable DRM: `"orbital.jgm2_8x8"` (`ModelKind::Orbital`,
/// N6) bound to the golden LEO physics' own gravity file/degree/order/point masses/coordinate
/// system, seeded with the golden's own orbital elements converted to Cartesian.
fn orbital_demo_bundle() -> (DesignReferenceMission, SosConfiguration, BTreeMap<String, SystemDefinition>) {
    let mu_jgm2 = 3.986004415e14_f64;
    let x0_km = keplerian_to_cartesian_km(6878.0, 0.001, 51.6, 30.0, 0.0, 0.0, mu_jgm2);

    let sys = hashed_system(SystemDefinition {
        id: "leo_orbital_sys".to_string(),
        version: "1".to_string(),
        name: "LEO JGM2 8x8 + Sun/Moon, native orbital model (N6)".to_string(),
        dynamics_model: "orbital.jgm2_8x8_sun_moon".to_string(),
        state_space_id: "gmat.orbital.cartesian6".to_string(),
        state_space: Some(StateSpace {
            id: "gmat.orbital.cartesian6".to_string(),
            components: vec![
                StateComponent { label: "pos_x".to_string(), unit: Unit::Meter as i32 },
                StateComponent { label: "pos_y".to_string(), unit: Unit::Meter as i32 },
                StateComponent { label: "pos_z".to_string(), unit: Unit::Meter as i32 },
                StateComponent { label: "vel_x".to_string(), unit: Unit::MeterPerSecond as i32 },
                StateComponent { label: "vel_y".to_string(), unit: Unit::MeterPerSecond as i32 },
                StateComponent { label: "vel_z".to_string(), unit: Unit::MeterPerSecond as i32 },
            ],
            ..Default::default()
        }),
        parameters: vec![
            // Identical string/numeric values to `drms/leo_1day_golden.system.yaml`'s own
            // `force_model.*`/`spacecraft.CoordinateSystem` -- N6's "select the native model by
            // name beside the GMAT model, with identical parameters."
            sparam("force_model.central_body", "Earth"),
            sparam("force_model.gravity_file", "JGM2.cof"),
            param("force_model.gravity_degree", 8.0),
            param("force_model.gravity_order", 8.0),
            sparam("force_model.point_masses", "Luna,Sun"),
            sparam("spacecraft.CoordinateSystem", "EarthMJ2000Eq"),
            sparam("spacecraft.DisplayStateType", "Cartesian"),
            param("spacecraft.X", x0_km[0]),
            param("spacecraft.Y", x0_km[1]),
            param("spacecraft.Z", x0_km[2]),
            param("spacecraft.VX", x0_km[3]),
            param("spacecraft.VY", x0_km[4]),
            param("spacecraft.VZ", x0_km[5]),
        ],
        ..Default::default()
    });

    let sos = hashed_sos(SosConfiguration {
        id: "leo_orbital_sos".to_string(),
        version: "1".to_string(),
        name: "LEO golden physics, native orbital instance (N6)".to_string(),
        instances: vec![SystemInstance {
            name: "leo_orbital".to_string(),
            system_id: "leo_orbital_sys".to_string(),
            binding: Some(Binding { kind: BindingKind::Model as i32, config: Some(av_cdm::pb::binding::Config::Model(ModelBinding { model_id: "leo_orbital_sys".to_string() })) }),
            // A 300 s native step (matching av_orbital::model::EarthGravityModel's own
            // Dopri5::default().max_step -- docs/native-dynamics-plan.md's own "Integrator
            // settings" section) over the golden's full one-day span is 288 steps -- enough to
            // resolve the LEO period (~5580 s) many times over without the ~864,000 calls a
            // literal copy of the golden's own 10 Hz/0.1 s sampling would need for a test whose
            // job is "propagates and is physically plausible," not golden-grade fidelity.
            step_rate_hz: 1.0 / 300.0,
            ..Default::default()
        }],
        ..Default::default()
    });

    let drm = hashed_drm(DesignReferenceMission {
        id: "drm_leo_orbital_demo".to_string(),
        version: "1".to_string(),
        name: "LEO one day, JGM2 8x8 + Sun/Moon, native orbital model (N6 no-GMAT demo)".to_string(),
        sos_configuration_id: "leo_orbital_sos".to_string(),
        // The identical epoch/duration `drms/leo_1day_golden.drm.yaml` declares (2026-01-01T00:
        // 00:00Z, one day) -- see that file's own header comment for the leap-second derivation.
        scenario: Some(Scenario { start_tai_ns: 1_767_225_637_000_000_000, end_tai_ns: 1_767_312_037_000_000_000, ..Default::default() }),
        options: Some(DrmOptions { default_step_rate_hz: 1.0 / 300.0, sample_interval_s: 300.0, ..Default::default() }),
        ..Default::default()
    });

    let mut systems = BTreeMap::new();
    systems.insert(sys.id.clone(), sys);
    (drm, sos, systems)
}

/// N6's own exit criterion, and this task's deliverable (c): the demo DRM propagates one day
/// with the native model alone, in the `--no-default-features` build (and, incidentally, in the
/// default build too -- this test carries no `required-features`, so both feature states run
/// it). Asserts a real trajectory: a plausible sample count, a LEO-band position magnitude
/// throughout, and energy/semi-major-axis conservation to a measured, printed tolerance -- never
/// merely `is_ok()` or a non-empty vector.
#[test]
fn demo_drm_propagates_one_day_with_the_native_orbital_model_alone() {
    // `docs/open-questions.md` question 230: `RunConfig.gmat` is `#[cfg(feature = "gmat")]` --
    // present (and a real, live handle) when this test builds with the default features, absent
    // under `--no-default-features`. Constructing a real `Gmat` handle even when this run's own
    // instance never dispatches to it mirrors `tests/drm_executor.rs`'s own documented
    // convention ("every test here constructs a real gmat_sys::Gmat handle ... even where the
    // DRM under test ... [needs no] GMAT call") -- kept here only for the default build; the
    // whole point of this test is that the `--no-default-features` build needs none of it.
    #[cfg(feature = "gmat")]
    let _engine = gmat_sys::engine_lock();
    #[cfg(feature = "gmat")]
    let gmat = gmat_sys::Gmat::setup(&gmat_sys::Gmat::default_startup_file()).expect("GMAT setup (default-feature build only)");

    let (drm, sos, systems) = orbital_demo_bundle();
    let cfg = RunConfig {
        #[cfg(feature = "gmat")]
        gmat: &gmat,
        drm: &drm,
        sos: &sos,
        systems: &systems,
        run_id: "test-orbital-no-gmat-demo".to_string(),
        error_mode: Default::default(),
        products_dir: None,
        replay: None,
        command_source: None,
    };
    let products = execute(cfg).expect("the native-orbital-model DRM must execute end to end with no GMAT feature required");

    let traj = products.trajectories.get("leo_orbital").expect("the \"leo_orbital\" instance produced a trajectory");
    assert_eq!(traj.segments.len(), 1, "no faults declared: exactly one dynamics segment");

    // A plausible sample count: 288 native steps at 300 s over 86,400 s, +/- the first/last
    // boundary samples HeteroKernel's own output grid always includes.
    assert!(traj.samples.len() >= 288 && traj.samples.len() <= 290, "expected roughly 288 samples (one every 300 s over one day), got {}", traj.samples.len());

    // Every sample's own position magnitude stays in the LEO band (a plausible trajectory, not
    // a NaN/zero/escaped one) -- SMA 6878 km +/- the modest oblateness/third-body perturbation
    // this arc is known (from the golden) to produce, generously bounded at +/- 200 km.
    let mut r_min = f64::MAX;
    let mut r_max = f64::MIN;
    for s in &traj.samples {
        assert_eq!(s.mean.len(), 6, "gmat.orbital.cartesian6 is always a 6-element Cartesian state");
        assert!(s.mean.iter().all(|v| v.is_finite()), "every component of every sample must be finite: {:?}", s.mean);
        let r = (s.mean[0].powi(2) + s.mean[1].powi(2) + s.mean[2].powi(2)).sqrt();
        r_min = r_min.min(r);
        r_max = r_max.max(r);
    }
    let sma_m = 6_878_000.0;
    eprintln!("[orbital_no_gmat_demo] {} samples; r_min = {r_min:.3} m, r_max = {r_max:.3} m (SMA = {sma_m} m)", traj.samples.len());
    assert!((r_min - sma_m).abs() < 200_000.0, "r_min {r_min} m too far from the declared SMA {sma_m} m");
    assert!((r_max - sma_m).abs() < 200_000.0, "r_max {r_max} m too far from the declared SMA {sma_m} m");

    // Two-body specific energy `eps = v^2/2 - mu/r` (SMA `a = -mu/(2*eps)`) conserved across the
    // run within the perturbation this arc's own gravity/third-body forces genuinely add --
    // measured and printed, not asserted from a paper. `av_dynamics::settings_hash`'s own
    // `mu` is JGM2's -- the identical constant `orbital_demo_bundle` derived the initial state
    // from, so this is a check of THIS run's own conservation, not a mismatched reference.
    let mu = 3.986004415e14_f64;
    let first = traj.samples.first().expect("at least one sample");
    let last = traj.samples.last().expect("at least one sample");
    let sma_from_state = |s: &av_cdm::pb::TrajectorySample| -> f64 {
        let r = (s.mean[0].powi(2) + s.mean[1].powi(2) + s.mean[2].powi(2)).sqrt();
        let v2 = s.mean[3].powi(2) + s.mean[4].powi(2) + s.mean[5].powi(2);
        let eps = v2 / 2.0 - mu / r;
        -mu / (2.0 * eps)
    };
    let sma_first = sma_from_state(first);
    let sma_last = sma_from_state(last);
    let sma_drift_relative = (sma_last - sma_first).abs() / sma_first;
    eprintln!("[orbital_no_gmat_demo] SMA(first) = {sma_first:.6} m, SMA(last) = {sma_last:.6} m, relative drift = {sma_drift_relative:e}");
    // Third-body/oblateness perturbation over one day genuinely changes the osculating SMA at
    // the sub-percent level (this is not a two-body arc) -- 1% is generous headroom above that,
    // while still catching a genuinely broken/escaping/decaying propagation (which would drift
    // by orders of magnitude more, or produce a NaN caught by the finite check above).
    assert!(sma_drift_relative < 1e-2, "osculating SMA drifted by {:.3}% over one day -- not a plausible bound orbit", sma_drift_relative * 100.0);

    // `RunProducts`/`Trajectory` provenance carries the DRM's own hash and this run's own id --
    // proves this really went through the real `execute()` pipeline, not a stub.
    assert_eq!(traj.config_hash, drm.hash);
    assert_eq!(products.provenance.run_id, "test-orbital-no-gmat-demo");
}

/// Deliverable (a)'s own proof, restated as a positive assertion (companion to this crate's own
/// report, which runs `otool -L` on this exact test's compiled binary): the native orbital
/// model's own `ModelInfo.depth` is `"native"`, never `"gmat-ffi"`/`"gmat-api"` -- so a consumer
/// reading `RunProducts`/`TrajectorySegment` back can tell, from data alone, that this run's own
/// dynamics never touched GMAT, regardless of which cargo feature built the kernel that ran it.
#[test]
fn orbital_instance_segment_reports_native_depth_not_gmat() {
    #[cfg(feature = "gmat")]
    let _engine = gmat_sys::engine_lock();
    #[cfg(feature = "gmat")]
    let gmat = gmat_sys::Gmat::setup(&gmat_sys::Gmat::default_startup_file()).expect("GMAT setup (default-feature build only)");

    let (drm, sos, systems) = orbital_demo_bundle();
    let cfg = RunConfig {
        #[cfg(feature = "gmat")]
        gmat: &gmat,
        drm: &drm,
        sos: &sos,
        systems: &systems,
        run_id: "test-orbital-depth-check".to_string(),
        error_mode: Default::default(),
        products_dir: None,
        replay: None,
        command_source: None,
    };
    let products = execute(cfg).expect("executes");
    let traj = products.trajectories.get("leo_orbital").expect("instance ran");
    assert_eq!(traj.segments.len(), 1);
    assert_eq!(traj.segments[0].dynamics_depth, "native", "the native orbital model's own ModelInfo.depth must be \"native\", never a GMAT depth string, regardless of which cargo feature built this kernel");
}

/// This task's own brief: "a silent skip that produces a trajectory labelled with a frame it is
/// not in is the worst possible defect this change could introduce." `av_kernel::drm::binding::
/// parse_orbital_spec` claims (its own doc comment, and `classify_binding`'s `ModelKind::Orbital`
/// arm) that a declared `spacecraft.CoordinateSystem` other than this instance's own integration
/// frame (`"{central_body}MJ2000Eq"`) is refused at classify time, before any propagation --
/// proven here, not merely read: corrupt [`orbital_demo_bundle`]'s own correct
/// `"EarthMJ2000Eq"` to a plausible-but-wrong GMAT AxisSystem name (`"EarthMJ2000Ec"`, the
/// ecliptic-not-equatorial sibling -- exactly the shape a copy-paste or typo would produce) and
/// assert `execute` returns `Err(DrmError::UnsupportedCoordinateSystem)`, never `Ok` with a
/// trajectory silently mislabelled (or silently propagated in the wrong frame).
#[test]
fn orbital_instance_declaring_a_non_integration_frame_coordinate_system_is_refused_at_classify_time() {
    #[cfg(feature = "gmat")]
    let _engine = gmat_sys::engine_lock();
    #[cfg(feature = "gmat")]
    let gmat = gmat_sys::Gmat::setup(&gmat_sys::Gmat::default_startup_file()).expect("GMAT setup (default-feature build only)");

    let (drm, sos, mut systems) = orbital_demo_bundle();
    let mut sys = systems.remove("leo_orbital_sys").expect("declared by orbital_demo_bundle");
    let mut found = false;
    for p in sys.parameters.iter_mut() {
        if p.name == "spacecraft.CoordinateSystem" {
            assert_eq!(p.string_value, "EarthMJ2000Eq", "orbital_demo_bundle's own declared frame must be the correct one before this test corrupts it");
            p.string_value = "EarthMJ2000Ec".to_string();
            found = true;
        }
    }
    assert!(found, "orbital_demo_bundle must declare spacecraft.CoordinateSystem");
    let sys = hashed_system(sys);
    systems.insert(sys.id.clone(), sys);

    let cfg = RunConfig {
        #[cfg(feature = "gmat")]
        gmat: &gmat,
        drm: &drm,
        sos: &sos,
        systems: &systems,
        run_id: "test-orbital-wrong-frame".to_string(),
        error_mode: Default::default(),
        products_dir: None,
        replay: None,
        command_source: None,
    };
    let err = execute(cfg).expect_err(
        "a \"orbital.\"-dispatched instance declaring a non-integration-frame spacecraft.CoordinateSystem must be refused at classify time, never silently executed with a mislabelled (or wrongly propagated) trajectory",
    );
    match err {
        av_kernel::drm::DrmError::UnsupportedCoordinateSystem { declared, integration_frame, .. } => {
            assert_eq!(declared, "EarthMJ2000Ec");
            assert_eq!(integration_frame, "EarthMJ2000Eq");
        }
        other => panic!("expected DrmError::UnsupportedCoordinateSystem, got {other:?}"),
    }
}
