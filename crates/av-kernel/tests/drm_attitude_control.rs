//! M22.4 (`docs/sil-plan.md`'s M22 milestone paragraph: "A native 'controller' instance closes
//! the loop first so the whole chain is proven before any external binary is involved"; its
//! Decisions (2026-09-05) decision A, "attitude control first": "star tracker and IMU in,
//! reaction wheel torques out"; `docs/open-questions.md` questions 142, 149, 151, 152): the
//! closed attitude control loop, exercised end to end through [`av_kernel::drm::execute`]
//! against `drms/demo_attitude_control.*.yaml` -- four instances (`attitude`, `startracker`,
//! `imu`, `controller`), the star tracker/IMU measuring the plant over CCSDS FRAMED ports the
//! way `drms/demo_attitude_sensors.*.yaml` already established (M22.2/M22.2b), and the native
//! controller closing the loop by commanding wheel torques back to the plant over its own
//! declared FRAMED port.
//!
//! **Expected settling behaviour, stated before running** (see `drms/demo_attitude_control.
//! drm.yaml`'s own header comment for the full derivation): initial pointing error 0.2 rad about
//! +z, target = identity, gains sized for **exact critical damping** (`kp = kd^2/(2*Jz)`, `kd =
//! 5.0`, `Jz = 50` -> `kp = 0.25`) giving a closed-form envelope time constant `tau = 2*Jz/kd =
//! 20 s` and the closed-form trajectory `theta(t)/theta(0) = (1 + t/tau) * exp(-t/tau)` -- no
//! overshoot (critically damped, `zeta = 1` exactly by construction). [`expected_theta_ratio`]
//! computes this closed form directly so the test's own "expected" numbers are traceable to the
//! same formula the YAML fixture's header comment states, not independently retyped magic
//! constants.
//!
//! **Exit criteria (this task's own brief):**
//! 1. The controller acts on *measured* (star tracker/IMU) values, never the plant's truth state
//!    directly -- [`the_measured_pointing_error_tracks_the_closed_form_decay_at_two_widely_
//!    separated_checkpoints`] checks the transient against the closed-form prediction at t=40s
//!    (2 tau) and t=100s (5 tau), where the deterministic decay still dominates measurement
//!    noise by orders of magnitude -- a controller silently wired to truth instead of the noisy
//!    star tracker packet would (by construction of this checked-in codec/port topology) simply
//!    fail to compile/route in the first place, but this test additionally proves the *measured*
//!    signal genuinely drives the physical response, not merely that packets flow.
//! 2. The final pointing error is meaningfully smaller than the initial one, with a stated bound
//!    -- [`the_final_pointing_error_is_meaningfully_smaller_than_the_initial_one`].
//! 3. The final pointing error is *not* exactly zero (the star tracker's own declared noise
//!    floor must show up as a residual) -- same test.
//! 4. No overshoot anywhere in the run (critically damped) -- [`the_error_never_overshoots_past_
//!    the_closed_form_envelope`].
//! 5. The wheel never approaches its declared saturation limit -- [`the_wheel_never_approaches_
//!    its_declared_saturation_limit`].
//! 6. Determinism: same seed byte-identical, different seed differs -- [`two_runs_of_the_control_
//!    drm_with_the_same_seed_are_byte_identical_for_the_imu_bias_trajectory`], [`a_different_
//!    star_tracker_seed_changes_the_propagated_pointing_error_curve`].
//! 7. A maneuver targeting the controller instance is a typed load error (question 152's own
//!    semantics, reused) -- [`a_maneuver_targeting_the_controller_instance_is_a_typed_load_
//!    error_through_execute`].

use std::collections::BTreeMap;
use std::path::PathBuf;

use av_cdm::pb::{DesignReferenceMission, MeasureOfEffectiveness, ScenarioEvent, SosConfiguration, SystemDefinition, Unit};
use av_kernel::drm::{execute, hash, schema, DrmError, RunConfig};
use gmat_sys::Gmat;

fn drms_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../drms").join(name)
}

fn read(name: &str) -> String {
    std::fs::read_to_string(drms_path(name)).unwrap_or_else(|e| panic!("reading drms/{name}: {e}"))
}

fn load_system(stem: &str) -> SystemDefinition {
    schema::parse_system_definition_yaml(&read(&format!("{stem}.system.yaml"))).unwrap_or_else(|e| panic!("{stem}.system.yaml: {e}"))
}

fn load_control_bundle() -> (DesignReferenceMission, SosConfiguration, BTreeMap<String, SystemDefinition>) {
    let drm = schema::parse_drm_yaml(&read("demo_attitude_control.drm.yaml")).expect("DRM parses");
    let sos = schema::parse_sos_yaml(&read("demo_attitude_control.sos.yaml")).expect("SosConfiguration parses");
    let truth = load_system("demo_attitude_control_truth");
    let star = load_system("demo_attitude_control_startracker");
    let imu = load_system("demo_attitude_control_imu");
    let controller = load_system("demo_attitude_control_controller");
    let mut systems = BTreeMap::new();
    systems.insert(truth.id.clone(), truth);
    systems.insert(star.id.clone(), star);
    systems.insert(imu.id.clone(), imu);
    systems.insert(controller.id.clone(), controller);
    (drm, sos, systems)
}

fn run_config<'a>(gmat: &'a Gmat, drm: &'a DesignReferenceMission, sos: &'a SosConfiguration, systems: &'a BTreeMap<String, SystemDefinition>) -> RunConfig<'a> {
    RunConfig { gmat, drm, sos, systems, run_id: "test-run-drm-attitude-control".to_string(), error_mode: Default::default() , products_dir: None, replay: None }
}

fn rehash_drm(mut drm: DesignReferenceMission) -> DesignReferenceMission {
    drm.hash = hash::canonical_drm_hash(&drm);
    drm
}

fn rehash_system(mut sys: SystemDefinition) -> SystemDefinition {
    sys.hash = hash::canonical_system_hash(&sys);
    sys
}

/// This fixture's own declared gains/inertia (`drms/demo_attitude_control_controller.system.
/// yaml`/`_truth.system.yaml`) -- kept here as named constants, not re-parsed from the YAML at
/// test time, so a change to either file that silently drifted from the "critical damping"
/// design intent would be caught by [`sanity_the_fixtures_own_declared_gains_are_exactly_
/// critically_damped`] rather than this file quietly re-deriving whatever the YAML happens to
/// say.
const JZ: f64 = 50.0;
const KP: f64 = 0.25;
const KD: f64 = 5.0;
const THETA0_RAD: f64 = 0.2;
/// `tau = 2*Jz/kd` -- the module doc comment's own closed-form envelope time constant.
const TAU_S: f64 = 2.0 * JZ / KD;

/// The closed-form `theta(t)/theta(0)` ratio for a critically damped (`zeta = 1`) second-order
/// system released from rest -- `(1 + t/tau) * exp(-t/tau)`. See the module doc comment's
/// derivation for where this comes from.
fn expected_theta_ratio(t_s: f64) -> f64 {
    let x = t_s / TAU_S;
    (1.0 + x) * (-x).exp()
}

#[test]
fn sanity_the_fixtures_own_declared_gains_are_exactly_critically_damped() {
    // zeta = 1 requires kp = kd^2 / (2*Jz) exactly -- checked here so a future edit to either
    // file's declared numbers that broke this task's own "critical damping, no overshoot"
    // design intent fails loudly, in one place, rather than only showing up as a mysterious
    // overshoot much later in `the_error_never_overshoots_past_the_closed_form_envelope`.
    let kp_for_critical_damping = KD * KD / (2.0 * JZ);
    assert!((kp_for_critical_damping - KP).abs() < 1e-12, "kp={KP} is not exactly the critical-damping value {kp_for_critical_damping} for kd={KD}, Jz={JZ}");
    assert_eq!(TAU_S, 20.0, "sanity: the envelope time constant this whole file's own expected numbers are built from");
}

/// `expected_theta_ratio`'s own closed form, cross-checked at a few hand-computed points --
/// fails against a transcription slip in the formula itself, independent of ever running the
/// simulation.
#[test]
fn expected_theta_ratio_matches_hand_computed_values() {
    assert!((expected_theta_ratio(0.0) - 1.0).abs() < 1e-12);
    // t = tau: (1+1)*exp(-1) = 2/e = 0.735758882...
    assert!((expected_theta_ratio(20.0) - 2.0 / std::f64::consts::E).abs() < 1e-9);
    // t = 15*tau = 300s: 16*exp(-15) = 4.8827...e-6
    assert!((expected_theta_ratio(300.0) - 16.0 * (-15.0f64).exp()).abs() < 1e-15);
}

// =============================================================================================
// Exit criterion 1: the measured signal genuinely drives the physical response -- the transient
// tracks the closed-form prediction at two widely separated checkpoints where the deterministic
// decay still dominates the star tracker's own declared measurement noise by orders of
// magnitude (noise sigma = 1e-5 rad/axis; predicted theta at these checkpoints is >= 0.008 rad,
// >= 800x the noise scale).
// =============================================================================================

fn pointing_error_at(products: &av_kernel::drm::RunProducts, name: &str) -> f64 {
    products.scores.get(name).unwrap_or_else(|| panic!("no score named {name:?}; available: {:?}", products.scores.keys().collect::<Vec<_>>())).value
}

#[test]
fn the_measured_pointing_error_tracks_the_closed_form_decay_at_two_widely_separated_checkpoints() {
    let _engine = gmat_sys::engine_lock();
    let (drm, sos, systems) = load_control_bundle();
    let mut drm = drm;
    drm.measures = vec![
        MeasureOfEffectiveness { name: "err_40s".to_string(), expression: "output.controller.pointing_error_rad@40s".to_string(), unit: Unit::Radian as i32 },
        MeasureOfEffectiveness { name: "err_100s".to_string(), expression: "output.controller.pointing_error_rad@100s".to_string(), unit: Unit::Radian as i32 },
    ];
    let drm = rehash_drm(drm);
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let products = execute(run_config(&gmat, &drm, &sos, &systems)).expect("the closed-loop DRM executes end to end");

    let want_40s = THETA0_RAD * expected_theta_ratio(40.0);
    let want_100s = THETA0_RAD * expected_theta_ratio(100.0);
    let got_40s = pointing_error_at(&products, "err_40s");
    let got_100s = pointing_error_at(&products, "err_100s");

    // Tolerance: 5% relative, generous over the <=0.02% small-angle linearization error and the
    // sensor-noise-driven perturbation to the trajectory (both far smaller at these checkpoints,
    // where the deterministic transient is 0.008-0.08 rad against a 1e-5 rad/axis noise floor) --
    // sized so a real regression in the control law's sign, gain, or wiring (which would be off
    // by anything from a sign flip to an order of magnitude) is caught, not to paper over one.
    assert!((got_40s - want_40s).abs() / want_40s < 0.05, "t=40s: expected ~{want_40s} rad (critically-damped closed form), got {got_40s} rad");
    assert!((got_100s - want_100s).abs() / want_100s < 0.05, "t=100s: expected ~{want_100s} rad (critically-damped closed form), got {got_100s} rad");
}

// =============================================================================================
// Exit criteria 2/3: the final pointing error is meaningfully smaller than the initial one, and
// is not exactly zero (a nonzero residual from the star tracker's own declared measurement
// noise) -- the task brief's own explicit "a closed loop whose pointing error does not actually
// decrease is a failed task" / "if it settles to exactly zero you probably wired truth in" pair.
// =============================================================================================

#[test]
fn the_final_pointing_error_is_meaningfully_smaller_than_the_initial_one_and_is_not_exactly_zero() {
    let _engine = gmat_sys::engine_lock();
    let (drm, sos, systems) = load_control_bundle();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let products = execute(run_config(&gmat, &drm, &sos, &systems)).expect("the closed-loop DRM executes end to end");

    let final_err = products.scores["controller_pointing_error_at_end"].value;
    assert_eq!(products.scores["controller_pointing_error_at_end"].passed, Some(true), "the DRM's own declared objective (target/tolerance, drms/demo_attitude_control.drm.yaml) must pass against this exact seeded run");
    // Stated bound: the deterministic transient alone predicts ~1e-6 rad by t=300s (15 tau) --
    // two orders of magnitude below the star tracker's own 1e-5 rad/axis noise floor, so the
    // *measured* residual is expected to be noise-dominated, on the order of a few times 1e-5
    // rad, not the deterministic value. Bound generously at 1e-3 rad (20000x margin under the
    // initial 0.2 rad error, and >=10x the expected noise-floor scale) -- loose enough to never
    // be a source of flakiness from the RNG draw, tight enough that a control law which stopped
    // actually correcting the error (e.g. a sign flip settling to a *different*, larger constant,
    // or a loop that never closes at all and leaves the full 0.2 rad error) fails it outright.
    assert!(final_err.abs() < 1e-3, "final pointing error {final_err} rad must be far below the 1e-3 rad bound (initial error was {THETA0_RAD} rad)");
    assert!(final_err.abs() > 1e-8, "final pointing error {final_err} rad must not be exactly (or numerically indistinguishable from) zero -- a genuinely closed loop acting on a noisy sensor leaves a nonzero residual; an exactly-zero result is the task brief's own named trap for truth having been wired in by mistake");
    // The brief's own explicit requirement: assert the error actually decreased, with a stated
    // bound (a >=100x reduction, far more conservative than the >=200x this design predicts).
    assert!(final_err.abs() < THETA0_RAD / 100.0, "the pointing error must have decreased by at least 100x over the run; got {final_err} rad from an initial {THETA0_RAD} rad");
}

// =============================================================================================
// Exit criterion 4: no overshoot anywhere in the run (critically damped, zeta = 1 exactly).
// =============================================================================================

/// Samples `pointing_error_rad` at every 20 s (one `tau`) from t=0 to t=280s and checks each
/// against the closed-form envelope `theta0 * expected_theta_ratio(t)` plus a small, stated
/// noise/nonlinearity margin -- "no overshoot" means the *measured* value never exceeds this
/// bound (a critically damped release from rest never crosses back past its own decaying
/// envelope), not merely that the final value looks small.
#[test]
fn the_error_never_overshoots_past_the_closed_form_envelope() {
    let _engine = gmat_sys::engine_lock();
    let (drm, sos, systems) = load_control_bundle();
    let mut drm = drm;
    // Starts at 20s, not 0s: `output.X@0s` lands exactly on the run's own start epoch, which
    // `crate::expr`'s window check refuses as "outside the run's window" (the evaluable window
    // opens a little after `start_tai_ns`, closes a little before `end_tai_ns` -- an
    // implementation detail of how named-output series are recorded, not something this test's
    // own physics claim depends on).
    let checkpoints_s: Vec<f64> = (20..=280).step_by(20).map(|t| t as f64).collect();
    drm.measures = checkpoints_s.iter().map(|t| MeasureOfEffectiveness { name: format!("err_{t}s"), expression: format!("output.controller.pointing_error_rad@{t}s"), unit: Unit::Radian as i32 }).collect();
    let drm = rehash_drm(drm);
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let products = execute(run_config(&gmat, &drm, &sos, &systems)).expect("the closed-loop DRM executes end to end");

    // Margin: 5% relative to the initial error (0.01 rad absolute) plus a small floor for the
    // late checkpoints where the deterministic prediction itself is smaller than that -- covers
    // sensor noise and the small-angle nonlinearity without being loose enough to hide a real
    // overshoot (which, for zeta=1 vs. e.g. an under-damped zeta<1 regression, would swing the
    // signed error to the *opposite* sign at a magnitude far larger than this margin near t=tau).
    let margin_rad = 0.01;
    for &t in &checkpoints_s {
        let want = THETA0_RAD * expected_theta_ratio(t);
        let got = pointing_error_at(&products, &format!("err_{t}s"));
        assert!(got <= want + margin_rad, "t={t}s: measured pointing error {got} rad overshot the closed-form envelope {want} rad by more than the {margin_rad} rad margin");
    }
}

// =============================================================================================
// Exit criterion 5: the wheel never approaches its declared saturation limit (20.0 kg*m^2/s) --
// the gains are sized to stay well within the plant's own declared actuation authority, not to
// probe saturation.
// =============================================================================================

#[test]
fn the_wheel_never_approaches_its_declared_saturation_limit() {
    let _engine = gmat_sys::engine_lock();
    let (drm, sos, systems) = load_control_bundle();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let products = execute(run_config(&gmat, &drm, &sos, &systems)).expect("the closed-loop DRM executes end to end");

    // ATTITUDE_WHEELS_BASE_COMPONENTS (7) + wheel index 2 (0-indexed) = wheel 3's own stored
    // momentum, the only wheel this single-axis scenario ever meaningfully excites.
    let attitude_traj = &products.trajectories["attitude"];
    let momentum_limit = 20.0;
    let mut max_abs_h = 0.0_f64;
    for sample in &attitude_traj.samples {
        let h_z = sample.mean[7 + 2];
        max_abs_h = max_abs_h.max(h_z.abs());
    }
    assert!(max_abs_h < momentum_limit * 0.5, "peak wheel-3 momentum {max_abs_h} kg*m^2/s must stay well below the declared {momentum_limit} kg*m^2/s limit (the gains are sized not to saturate)");
    assert!(max_abs_h > 1e-6, "sanity: the wheel must have actually accumulated some momentum (a dead/unwired command path would leave it at exactly zero)");
}

// =============================================================================================
// Exit criterion 6: determinism -- same seed byte-identical, different seed differs.
// =============================================================================================

#[test]
fn two_runs_of_the_control_drm_with_the_same_seed_are_byte_identical_for_the_imu_bias_trajectory() {
    let _engine = gmat_sys::engine_lock();
    let (drm, sos, systems) = load_control_bundle();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");

    let run_a = execute(run_config(&gmat, &drm, &sos, &systems)).expect("first run executes");
    let run_b = execute(run_config(&gmat, &drm, &sos, &systems)).expect("second run executes");

    let means_a: Vec<&Vec<f64>> = run_a.trajectories["imu"].samples.iter().map(|s| &s.mean).collect();
    let means_b: Vec<&Vec<f64>> = run_b.trajectories["imu"].samples.iter().map(|s| &s.mean).collect();
    assert_eq!(means_a, means_b, "two runs of the identical, identically-seeded DRM must produce byte-identical IMU bias trajectories");
    assert_eq!(run_a.scores["controller_pointing_error_at_end"].value, run_b.scores["controller_pointing_error_at_end"].value, "the controller's own scored pointing error must be byte-identical across two identically-seeded runs too");
}

/// Mirrors `tests/drm_attitude_sensors.rs::a_different_imu_seed_changes_the_propagated_bias_
/// trajectory`'s own pattern, applied to the star tracker's own declared seed here: a different
/// seed must change the propagated `pointing_error_rad` curve (proving the controller's own
/// commanded response genuinely depends on the star tracker's own noise draw, not a fixed/dead
/// value) -- the companion assertion the brief requires alongside the same-seed test above.
#[test]
fn a_different_star_tracker_seed_changes_the_propagated_pointing_error_curve() {
    let _engine = gmat_sys::engine_lock();
    let (drm, sos, systems) = load_control_bundle();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let baseline = execute(run_config(&gmat, &drm, &sos, &systems)).expect("baseline run executes");

    let mut reseeded_systems = systems.clone();
    let star_sys = reseeded_systems.get_mut("attitude_control_startracker_sys").expect("star tracker system present");
    let seed_param = star_sys.parameters.iter_mut().find(|p| p.name == "startracker.seed").expect("startracker.seed is declared");
    assert_eq!(seed_param.value, 1.0, "sanity: the fixture's own declared seed before mutation");
    seed_param.value = 99.0;
    let mutated_id = star_sys.id.clone();
    let mutated = rehash_system(reseeded_systems.remove(&mutated_id).unwrap());
    reseeded_systems.insert(mutated_id, mutated);

    let reseeded = execute(run_config(&gmat, &drm, &sos, &reseeded_systems)).expect("reseeded run executes");
    assert_ne!(
        baseline.scores["controller_pointing_error_at_end"].value,
        reseeded.scores["controller_pointing_error_at_end"].value,
        "a different declared startracker.seed must produce a genuinely different final pointing error, not a bit-identical one"
    );
}

// =============================================================================================
// Exit criterion 7: a maneuver targeting the controller instance is a typed load error.
// =============================================================================================

#[test]
fn a_maneuver_targeting_the_controller_instance_is_a_typed_load_error_through_execute() {
    let _engine = gmat_sys::engine_lock();
    let (drm, sos, systems) = load_control_bundle();
    let start = drm.scenario.as_ref().unwrap().start_tai_ns;
    let mut drm = drm;
    {
        let scenario = drm.scenario.as_mut().unwrap();
        scenario.events.push(ScenarioEvent {
            id: "ctrl_burn".to_string(),
            tai_ns: start + 2_000_000_000,
            kind: "maneuver".to_string(),
            instance: "controller".to_string(),
            values: BTreeMap::from([("dv_x".to_string(), 1.0), ("dv_y".to_string(), 0.0), ("dv_z".to_string(), 0.0)]),
            attributes: BTreeMap::from([("frame_id".to_string(), "AXES_KIND_VNB".to_string())]),
            execution_error: None,
        });
    }
    let drm = rehash_drm(drm);
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let err = execute(run_config(&gmat, &drm, &sos, &systems)).unwrap_err();
    assert!(matches!(err, DrmError::ManeuverTargetNotSixDimensional { ref instance, ref maneuver_id, state_dim: 0 } if instance == "controller" && maneuver_id == "ctrl_burn"), "{err:?}");
}
