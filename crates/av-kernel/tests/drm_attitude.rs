//! M22.1b (`docs/open-questions.md` questions 151/152, decided by the lead; `docs/sil-plan.md`'s
//! M22 milestone paragraph): the attitude dynamics model M22.1 built
//! (`crate::drm::attitude::AttitudeWheelsModel`) wired into the DRM binding path so a real DRM
//! can use it, exercised end to end through [`av_kernel::drm::execute`].
//!
//! Exit criteria (task brief):
//! 1. A DRM declares an attitude-propagating instance and runs through `execute()` --
//!    [`a_torque_free_precession_drm_runs_through_execute_and_matches_the_closed_form`] and
//!    [`a_wheel_limit_fault_drm_runs_through_execute`].
//! 2. Sampling uses `SampleKind` and slerp per ADR-005 sec 3 --
//!    [`run_products_from_a_real_attitude_run_slerp_the_quaternion_and_linearly_interpolate_
//!    the_rest`], [`an_unclassifiable_component_in_an_attitude_shaped_state_space_is_a_typed_
//!    refusal`].
//! 3. The torque-free precession pin reproduces through the full executor path --
//!    [`a_torque_free_precession_drm_runs_through_execute_and_matches_the_closed_form`].
//! 4. A fault that halves a wheel limit changes the arc measurably, by a magnitude derived and
//!    stated before running -- [`a_dynamics_fault_halving_a_wheel_limit_changes_the_arc_by_the_
//!    precomputed_magnitude`].
//!
//! Also (question 152's other two semantics, each its own deliberate arm, reproduced through
//! `execute()` too, not just at the `classify_binding`/`AnyModel` unit level already covered in
//! `crates/av-kernel/src/drm/{binding,fault}.rs`'s own test modules):
//! [`a_maneuver_event_targeting_an_attitude_only_instance_is_a_typed_load_error_through_execute`],
//! [`covariance_requested_against_an_attitude_instance_is_a_typed_refusal_through_execute`].
//!
//! Every test here constructs a real `gmat_sys::Gmat` handle (`RunConfig.gmat`) even though
//! none of these DRMs ever bind a `"gmat."` instance -- `execute` takes `&Gmat` unconditionally
//! (ADR-002, GMAT is a process-wide singleton) -- and takes `gmat_sys::engine_lock()` first, per
//! this repository's existing convention (`tests/drm_executor.rs`'s own module doc comment).

use std::collections::BTreeMap;
use std::path::PathBuf;

use av_cdm::pb::{DesignReferenceMission, ScenarioEvent, SosConfiguration, SystemDefinition};
use av_kernel::drm::{execute, hash, schema, DrmError, RunConfig};
use av_kernel::interpolate::{self, ComponentClass, InterpolationError};
use gmat_sys::Gmat;

fn drms_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../drms").join(name)
}

fn load_bundle(stem: &str) -> (DesignReferenceMission, SosConfiguration, BTreeMap<String, SystemDefinition>) {
    let drm = schema::parse_drm_yaml(&std::fs::read_to_string(drms_path(&format!("{stem}.drm.yaml"))).unwrap()).expect("DRM parses");
    let sos = schema::parse_sos_yaml(&std::fs::read_to_string(drms_path(&format!("{stem}.sos.yaml"))).unwrap()).expect("SosConfiguration parses");
    let sys = schema::parse_system_definition_yaml(&std::fs::read_to_string(drms_path(&format!("{stem}.system.yaml"))).unwrap()).expect("SystemDefinition parses");
    let mut systems = BTreeMap::new();
    systems.insert(sys.id.clone(), sys);
    (drm, sos, systems)
}

fn run_config<'a>(gmat: &'a Gmat, drm: &'a DesignReferenceMission, sos: &'a SosConfiguration, systems: &'a BTreeMap<String, SystemDefinition>) -> RunConfig<'a> {
    RunConfig { gmat, drm, sos, systems, run_id: "test-run-drm-attitude".to_string(), error_mode: Default::default() , products_dir: None }
}

/// Re-hash a `DesignReferenceMission` after a test has mutated it in memory (never used to
/// bypass `execute`'s own tamper check -- the opposite: this is what keeps a deliberately
/// mutated fixture variant, e.g. "the baseline arc with the fault removed", honestly
/// self-consistent, exactly the way a real DRM author would after editing a YAML file and
/// re-running `cargo run -p av-kernel --example drm_hash`).
fn rehash(mut drm: DesignReferenceMission) -> DesignReferenceMission {
    drm.hash = hash::canonical_drm_hash(&drm);
    drm
}

// =============================================================================================
// Exit criteria 1 + 3: torque-free axisymmetric precession, end to end.
// =============================================================================================

/// Reproduces `crate::drm::attitude::tests::torque_free_axisymmetric_precession_matches_the_
/// closed_form` (M22.1's own isolated golden, `max_transverse_err` measured there at 2.55e-14)
/// through the full `av_kernel::drm::execute` path instead of calling `AttitudeWheelsModel::
/// step` directly -- proving `"attitude."` dispatch (`classify_binding` -> `BindingPlan::
/// Attitude` -> `ModelRegistry::construct_attitude` -> `AnyModel::Attitude` ->
/// `HeteroKernel::run_with_ports`) reaches the identical physics, not merely that the isolated
/// unit does. Fails against an implementation that never wires `"attitude."` dispatch at all
/// (the DRM would be refused before propagation ever starts) or one whose `AnyModel::Attitude`
/// arm silently reaches a trait default instead of delegating (the propagated arc would not
/// match the closed form at all -- see `crate::drm::binding::tests::any_model_step_delegates_
/// to_the_attitude_variant` for the same proof one layer down, against `AnyModel::step`
/// directly).
#[test]
fn a_torque_free_precession_drm_runs_through_execute_and_matches_the_closed_form() {
    let _engine = gmat_sys::engine_lock();
    let (drm, sos, systems) = load_bundle("demo_attitude_precession");
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let products = execute(run_config(&gmat, &drm, &sos, &systems)).expect("the torque-free precession DRM executes end to end");

    let traj = products.trajectories.get("att1").expect("instance att1 produced a trajectory");
    // 30 one-second output ticks over a 30 s scenario at 1 Hz -> 31 samples (t=0..=30 s),
    // every one of them SampleKind::Native (default_step_rate_hz == sample_interval_s == 1.0).
    assert_eq!(traj.samples.len(), 31, "{:?}", traj.samples.iter().map(|s| s.tai_ns).collect::<Vec<_>>());
    assert!(traj.samples.iter().all(|s| s.kind == av_cdm::pb::SampleKind::Native as i32), "every output tick lands exactly on this instance's own 1 Hz native step");

    let (jt, jz) = (100.0_f64, 50.0_f64);
    let (omega_x0, omega_y0, omega_z0) = (0.05_f64, 0.03_f64, 0.2_f64);
    let lambda = omega_z0 * (jz - jt) / jt;
    assert!((lambda - (-0.1)).abs() < 1e-12, "lambda sanity check: {lambda}");

    let mut max_wz_err = 0.0_f64;
    let mut max_transverse_err = 0.0_f64;
    for sample in &traj.samples {
        let t = (sample.tai_ns - drm.scenario.as_ref().unwrap().start_tai_ns) as f64 * 1e-9;
        let want_wz = omega_z0;
        let want_wx = omega_x0 * (lambda * t).cos() - omega_y0 * (lambda * t).sin();
        let want_wy = omega_x0 * (lambda * t).sin() + omega_y0 * (lambda * t).cos();
        // State layout: [q_x, q_y, q_z, q_w, body_rate_x, body_rate_y, body_rate_z].
        max_wz_err = max_wz_err.max((sample.mean[6] - want_wz).abs());
        max_transverse_err = max_transverse_err.max(((sample.mean[4] - want_wx).powi(2) + (sample.mean[5] - want_wy).powi(2)).sqrt());
    }
    // Same disclosed measurement M22.1's own isolated golden makes (max_wz_err = 0.0 exactly,
    // max_transverse_err = 2.55e-14) -- these tolerances are the identical safety margins that
    // golden already discloses (1e-12/1e-9), reproduced through this full executor path rather
    // than loosened for it.
    assert!(max_wz_err < 1e-12, "omega_z drifted by {max_wz_err} through execute() -- should be exactly constant");
    assert!(max_transverse_err < 1e-9, "transverse rate deviated from the closed form by {max_transverse_err} through execute()");
}

// =============================================================================================
// Exit criterion 2: SampleKind/slerp per ADR-005 sec 3, against this run's own real samples.
// =============================================================================================

/// Takes two adjacent `SampleKind::Native` samples from a real attitude `Trajectory` (the same
/// run as the test above) and interpolates their midpoint through `crate::interpolate::
/// interpolate_by_state_space` -- ADR-005 sec 3's own declared-`StateSpace`-aware interpolation
/// contract (`crate::drm::attitude`'s own module doc comment: "the same class M22.1 must place
/// its own components in"). Proves the quaternion group is placed under `ComponentClass::
/// Quaternion` (slerped, unit-norm) and the body-rate group under `ComponentClass::LinearScalar`
/// (plain linear -- exact at the midpoint), never the reverse or some other class. Fails against
/// a state space that mislabels the quaternion group (e.g. keeps `wheel_h_<n>`'s pre-question-
/// 151 `UNIT_NEWTON_METER`, which does not affect this test, or a hypothetical regression that
/// swapped the quaternion/body-rate component order): `interpolate_by_state_space` would either
/// refuse (`InterpolationError::UnclassifiableComponent`) or silently linearly blend the
/// quaternion (never unit norm at the midpoint of two different orientations).
#[test]
fn run_products_from_a_real_attitude_run_slerp_the_quaternion_and_linearly_interpolate_the_rest() {
    let _engine = gmat_sys::engine_lock();
    let (drm, sos, systems) = load_bundle("demo_attitude_precession");
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let products = execute(run_config(&gmat, &drm, &sos, &systems)).expect("executes end to end");
    let traj = products.trajectories.get("att1").unwrap();
    let space = systems.values().next().unwrap().state_space.clone().unwrap();

    // Samples 10 and 11 (t = 10 s, 11 s) -- well after the run start, both real Native samples.
    let s0 = &traj.samples[10];
    let s1 = &traj.samples[11];
    let t_mid = (s0.tai_ns + s1.tai_ns) / 2;
    let mid = interpolate::interpolate_by_state_space(&space, s0.tai_ns, &s0.mean, s1.tai_ns, &s1.mean, t_mid).expect("a well-formed attitude state space must interpolate cleanly");

    // Quaternion group: unit norm at the midpoint (slerp's own contract -- a naive linear blend
    // of two distinct unit quaternions is essentially never unit norm).
    let q_mid_norm = (mid[0] * mid[0] + mid[1] * mid[1] + mid[2] * mid[2] + mid[3] * mid[3]).sqrt();
    assert!((q_mid_norm - 1.0).abs() < 1e-9, "slerped quaternion must stay unit norm, got |q| = {q_mid_norm}");

    // The naive linear blend (what a bug that dropped the Quaternion class down to LinearScalar
    // would produce instead) is NOT unit norm here, by a wide margin -- so this is a real,
    // discriminating check, not a coincidence of these particular endpoints.
    let linear_q_norm = {
        let lin: Vec<f64> = (0..4).map(|i| 0.5 * (s0.mean[i] + s1.mean[i])).collect();
        (lin[0] * lin[0] + lin[1] * lin[1] + lin[2] * lin[2] + lin[3] * lin[3]).sqrt()
    };
    assert!((linear_q_norm - 1.0).abs() > 1e-6, "sanity: the naive linear blend must NOT already be unit norm, got {linear_q_norm} (otherwise this test cannot discriminate slerp from linear)");

    // Body rate: LinearScalar, exact arithmetic mean at the midpoint.
    for (i, mid_i) in mid.iter().enumerate().take(7).skip(4) {
        let want = 0.5 * (s0.mean[i] + s1.mean[i]);
        assert!((mid_i - want).abs() < 1e-12, "body_rate component {i} at the midpoint = {mid_i}, want the exact linear mean {want}");
    }
}

/// `crate::drm::attitude::attitude_wheels_state_space`'s own declared shape classifies cleanly
/// (already proven generically in `crate::trajectory`'s own test module) -- this test instead
/// proves the *refusal* half of ADR-005 sec 3 ("a component that cannot be placed is a typed
/// refusal") specifically for the attitude shape: a component whose declared unit this crate
/// does not recognize at all (`UNIT_UNSPECIFIED`) is refused, never silently held or dropped.
#[test]
fn an_unclassifiable_component_in_an_attitude_shaped_state_space_is_a_typed_refusal() {
    let mut space = av_kernel::trajectory::attitude_wheels_state_space("test.attitude_bad", 0);
    space.components[4].unit = av_cdm::pb::Unit::Unspecified as i32; // body_rate_x, corrupted
    let s0 = vec![0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0];
    let s1 = vec![0.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0];
    let err = interpolate::interpolate_by_state_space(&space, 0, &s0, 1_000_000_000, &s1, 500_000_000).unwrap_err();
    assert!(matches!(err, InterpolationError::UnclassifiableComponent { index: 4, .. }), "{err:?}");
}

/// Sanity companion to the two tests above: `crate::interpolate::classify` places the attitude
/// shape's own groups exactly where the SLERP/linear tests above assume they are (index 0-3
/// Quaternion, 4-6 LinearScalar) -- if this ever disagreed, the two tests above would still
/// pass or fail for the wrong reason.
#[test]
fn attitude_state_space_classifies_quaternion_then_linear_scalar_body_rate() {
    let space = av_kernel::trajectory::attitude_wheels_state_space("test.attitude", 0);
    let groups = interpolate::classify(&space).unwrap();
    assert_eq!(groups, vec![(0, 4, ComponentClass::Quaternion), (4, 1, ComponentClass::LinearScalar), (5, 1, ComponentClass::LinearScalar), (6, 1, ComponentClass::LinearScalar)]);
}

// =============================================================================================
// Exit criterion 4: a fault halving a wheel limit changes the arc measurably.
// =============================================================================================

/// Sanity: the wheel-fault DRM itself runs end to end (exit criterion 1, second fixture) and
/// splits into two segments at the fault epoch, continuous in state (question 87's own DYNAMICS
/// fault contract).
#[test]
fn a_wheel_limit_fault_drm_runs_through_execute() {
    let _engine = gmat_sys::engine_lock();
    let (drm, sos, systems) = load_bundle("demo_attitude_wheel_fault");
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let products = execute(run_config(&gmat, &drm, &sos, &systems)).expect("the wheel-fault DRM executes end to end");
    let traj = products.trajectories.get("att_wheel").unwrap();
    assert_eq!(traj.segments.len(), 2, "one DYNAMICS fault must split the run into exactly two segments");
    // Continuity: the last sample of segment 0 and the first of segment 1 (both at the fault
    // epoch, t=0.5s) must carry the identical physical state -- only the dynamics configuration
    // (the wheel's own momentum_limit) changed, not the state.
    let fault_tai_ns = drm.scenario.as_ref().unwrap().faults[0].tai_ns;
    let at_fault: Vec<&av_cdm::pb::TrajectorySample> = traj.samples.iter().filter(|s| s.tai_ns == fault_tai_ns).collect();
    assert_eq!(at_fault.len(), 1, "exactly one recorded sample at the fault epoch (segments share their boundary sample)");
}

/// **Expected magnitude, stated before running** (see `drms/demo_attitude_wheel_fault.drm.yaml`'s
/// own header comment for the full derivation from golden 5's closed form): at T = 2.0 s,
/// baseline omega_x = -0.08 rad/s (h_w = 0.8, never saturates against limit=1.0), faulted
/// omega_x = -0.05 rad/s (h_w saturates at the halved limit=0.5 by t=1.25s) -- an expected
/// difference of +0.03 rad/s (h_w differs by -0.3 N*m*s). Both are five to six orders of
/// magnitude above this integrator's own numerical precision at this scale (`crate::drm::
/// attitude`'s own goldens measure ~1e-10 residuals here), so a bit-identical or noise-scale
/// difference between the two arcs below is a failure, not a pass.
#[test]
fn a_dynamics_fault_halving_a_wheel_limit_changes_the_arc_by_the_precomputed_magnitude() {
    let _engine = gmat_sys::engine_lock();
    let (faulted_drm, sos, systems) = load_bundle("demo_attitude_wheel_fault");
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");

    let faulted = execute(run_config(&gmat, &faulted_drm, &sos, &systems)).expect("faulted DRM executes");
    let faulted_traj = faulted.trajectories.get("att_wheel").unwrap();
    let faulted_final = faulted_traj.samples.last().unwrap();

    // The baseline (unfaulted) arc: the identical DRM with Scenario.faults cleared, re-hashed
    // (see `rehash`'s own doc comment -- this is not bypassing the tamper check, it is
    // constructing a second, honestly self-consistent DRM in memory rather than committing a
    // second near-duplicate YAML file whose only difference from demo_attitude_wheel_fault.drm.
    // yaml is an empty `faults: []`).
    let mut baseline_drm = faulted_drm.clone();
    baseline_drm.scenario.as_mut().unwrap().faults.clear();
    let baseline_drm = rehash(baseline_drm);
    let baseline = execute(run_config(&gmat, &baseline_drm, &sos, &systems)).expect("baseline DRM executes");
    let baseline_traj = baseline.trajectories.get("att_wheel").unwrap();
    let baseline_final = baseline_traj.samples.last().unwrap();

    // State layout: [q_x,q_y,q_z,q_w, body_rate_x,body_rate_y,body_rate_z, wheel_h_1].
    let (h_baseline, wx_baseline) = (baseline_final.mean[7], baseline_final.mean[4]);
    let (h_faulted, wx_faulted) = (faulted_final.mean[7], faulted_final.mean[4]);

    assert!((h_baseline - 0.8).abs() < 1e-6, "baseline h_w(2s) = {h_baseline}, want ~0.8 (never saturates)");
    assert!((h_faulted - 0.5).abs() < 1e-6, "faulted h_w(2s) = {h_faulted}, want ~0.5 (saturated at the halved limit)");
    assert!((wx_baseline - (-0.08)).abs() < 1e-6, "baseline omega_x(2s) = {wx_baseline}, want ~-0.08");
    assert!((wx_faulted - (-0.05)).abs() < 1e-6, "faulted omega_x(2s) = {wx_faulted}, want ~-0.05");

    let delta_wx = wx_faulted - wx_baseline;
    let delta_h = h_faulted - h_baseline;
    assert!((delta_wx - 0.03).abs() < 1e-6, "delta(omega_x) = {delta_wx}, want +0.03 rad/s (the precomputed magnitude)");
    assert!((delta_h - (-0.3)).abs() < 1e-6, "delta(h_w) = {delta_h}, want -0.3 N*m*s");
    // The vacuous-test guard the brief explicitly calls out: bit-identical arcs is a failure.
    assert_ne!(faulted_final.mean, baseline_final.mean, "a wheel-limit fault that actually fires must change the final state -- bit-identical arcs means the fault had no effect");
}

// =============================================================================================
// Question 152's other two semantics, reproduced through execute() (not only at the unit level
// already covered in crate::drm::{binding,fault}'s own test modules).
// =============================================================================================

/// "A maneuver event targeting an attitude-only instance is a typed load error because it has
/// no translational state" -- reproduced through the real `execute()` path: `ManeuverTargetNot
/// SixDimensional`'s own array-length guard (`executor::execute`'s boundary loop) fires for the
/// genuinely 7-dimensional attitude state, not merely asserted about at the `classify_binding`/
/// `apply_dynamics_fault` unit level.
#[test]
fn a_maneuver_event_targeting_an_attitude_only_instance_is_a_typed_load_error_through_execute() {
    let _engine = gmat_sys::engine_lock();
    let (drm, sos, systems) = load_bundle("demo_attitude_precession");
    let start = drm.scenario.as_ref().unwrap().start_tai_ns;
    let mut drm = drm;
    {
        let scenario = drm.scenario.as_mut().unwrap();
        scenario.events.push(ScenarioEvent {
            id: "att_burn".to_string(),
            tai_ns: start + 10_000_000_000, // t=10s, on the 1 Hz sample grid
            kind: "maneuver".to_string(),
            instance: "att1".to_string(),
            values: BTreeMap::from([("dv_x".to_string(), 1.0), ("dv_y".to_string(), 0.0), ("dv_z".to_string(), 0.0)]),
            attributes: BTreeMap::from([("frame_id".to_string(), "AXES_KIND_VNB".to_string())]),
            execution_error: None,
        });
    }
    let drm = rehash(drm);
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let err = execute(run_config(&gmat, &drm, &sos, &systems)).unwrap_err();
    assert!(
        matches!(err, DrmError::ManeuverTargetNotSixDimensional { ref instance, ref maneuver_id, state_dim: 7 } if instance == "att1" && maneuver_id == "att_burn"),
        "{err:?}"
    );
}

/// "Covariance for attitude is a typed refusal this batch (declared, not silent)" -- reproduced
/// through `execute()`: `run_covariance_instance` materializes a real `AttitudeWheelsModel`
/// (`AnyModel::Attitude::stm_capable()` is always `false`, question 152) and refuses with the
/// same generic `DrmError::ModelNotStmCapable` a native `ConstantAccelModel` covariance request
/// already gets -- no attitude-specific covariance code path exists anywhere in this crate.
#[test]
fn covariance_requested_against_an_attitude_instance_is_a_typed_refusal_through_execute() {
    let _engine = gmat_sys::engine_lock();
    let (drm, sos, systems) = load_bundle("demo_attitude_precession");
    let mut drm = drm;
    drm.options.as_mut().unwrap().covariance = true;
    let drm = rehash(drm);
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let err = execute(run_config(&gmat, &drm, &sos, &systems)).unwrap_err();
    assert!(matches!(err, DrmError::ModelNotStmCapable { ref instance } if instance == "att1"), "{err:?}");
}

// =============================================================================================
// Question 151: a wheel-momentum component declared with the torque unit is a typed load error,
// reproduced through the real `classify_binding` entry point `execute()` itself calls (Pass 1).
// =============================================================================================

/// Mutates a loaded, otherwise-valid `SystemDefinition` to reintroduce M22.1's own retired
/// `UNIT_NEWTON_METER` wheel-momentum labelling and confirms `execute()` refuses the whole DRM
/// at load, before any propagation. Uses the wheel-fault fixture (it actually declares a wheel;
/// the precession fixture declares none).
#[test]
fn execute_refuses_a_wheel_momentum_component_declared_with_the_torque_unit() {
    let _engine = gmat_sys::engine_lock();
    let (drm, sos, mut systems) = load_bundle("demo_attitude_wheel_fault");
    let sys = systems.get_mut("attitude_wheel_fault_sys").unwrap();
    sys.state_space.as_mut().unwrap().components[7].unit = av_cdm::pb::Unit::NewtonMeter as i32; // wheel_h_1
    sys.hash = hash::canonical_system_hash(sys);
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let err = execute(run_config(&gmat, &drm, &sos, &systems)).unwrap_err();
    assert!(matches!(err, DrmError::InvalidAttitudeSpec { ref instance, ref reason } if instance == "att_wheel" && reason.contains("UNIT_NEWTON_METER")), "{err:?}");
}

