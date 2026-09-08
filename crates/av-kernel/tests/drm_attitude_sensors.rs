//! M22.2b (`docs/open-questions.md` questions 142/149/151/152, decided by the lead;
//! `docs/sil-plan.md`'s M22 milestone paragraph and its Decisions (2026-09-05) decision A,
//! "attitude control first"): the star tracker and IMU sensor models M22.2 built
//! (`crate::drm::sensors::StarTrackerModel`/`ImuModel`) wired into the DRM binding path so a
//! real DRM can use them, exercised end to end through [`av_kernel::drm::execute`] against
//! `drms/demo_attitude_sensors.*.yaml` -- the same fixture set
//! `crates/av-kernel/tests/drm_attitude_sensors_fixture.rs` already checks at load time only.
//!
//! **Topology.** One `"attitude."`-dispatched truth instance (`attitude`, torque-free
//! axisymmetric precession, identical physics to `drms/demo_attitude_precession.system.yaml`),
//! feeding both a `"startracker."`-dispatched instance (`startracker`) and a
//! `"imu."`-dispatched instance (`imu`) over the seven `crate::drm::sensors::TRUTH_PORT_NAMES`
//! SIGNAL ports declared as fourteen `Connection`s in `demo_attitude_sensors.sos.yaml`. Both
//! sensors declare a `PORT_KIND_FRAMED`/`schema: "ccsds.spp"` OUT port and a matching
//! `PacketCodec`; `crates/av-kernel/tests/drm_attitude_sensors_fixture.rs` already proves both
//! codecs are independently valid (`crate::codec::validate_codec`) and that the fourteen
//! connections build a real `crate::router::Router` against the declared ports -- this file
//! proves the same topology actually *runs*.
//!
//! **What `RunProducts` can and cannot observe about FRAMED port traffic (read before assuming a
//! test here decodes a raw CCSDS packet).** `av_kernel::drm::execute`'s `RunProducts` carries
//! per-instance `Trajectory`s and named-output series (`docs/open-questions.md` question 95);
//! it does not carry raw `crate::router::Router`-mediated port traffic at all (only the router
//! itself sees message bytes, transiently, mid-run) -- see `crate::drm::executor::RunProducts`'s
//! own doc comment. `StarTrackerModel`/`ImuModel::step_with_ports` (M22.2b addendum) additionally
//! expose the CCSDS sequence-count field of the *last* packet pushed onto their own declared
//! FRAMED port this step as `StepResult.outputs["seq"]`, reachable as
//! `output.<instance>.seq@<time>` (question 95's second half) precisely so this file can prove,
//! through the public `execute()` entry point, that a real packet left on that port at the
//! declared rate. The packet's own bytes/fields are already proven correct at the unit level by
//! `crate::drm::sensors::tests::attitude_instance_is_measured_by_both_sensors_through_the_real_
//! router` (real `crate::router::Router`, real `Connection`/`Port` declarations, real
//! `codec::encode_packet`/`decode_star_tracker` round trip) -- not re-proven here, since
//! `RunProducts` has no surface to observe it through in the first place.
//!
//! **Exit criteria (task brief):**
//! 1. A DRM declares an attitude instance plus both sensors and runs through `execute()` --
//!    [`the_sensors_drm_runs_through_execute_and_emits_at_the_declared_rate`].
//! 2. Declared update rates are honoured through the full executor path, using a sensor rate
//!    (2 Hz) that differs from the kernel step (1 Hz), with the exact expected emission count
//!    stated before measuring -- same test: see its own doc comment for the derivation.
//! 3. Two runs with the same seed are byte-identical; a different seed differs; both asserted --
//!    [`two_runs_of_the_sensors_drm_with_the_same_seed_are_byte_identical_for_the_imu_bias_
//!    trajectory`], [`a_different_imu_seed_changes_the_propagated_bias_trajectory`].
//! 4. A maneuver targeting a sensor instance is a typed load error (question 152's own semantics,
//!    reused: "a maneuver event targeting an attitude-only instance is a typed load error
//!    because it has no translational state" -- `crate::drm::executor::execute`'s own boundary
//!    loop refuses `BindingPlan::Imu`/`BindingPlan::StarTracker` explicitly, by variant, *before*
//!    its generic six-component-length check even runs, because `ImuModel::state_dim() == 6` is
//!    also six and would otherwise silently fool that generic check) --
//!    [`a_maneuver_targeting_the_imu_instance_is_a_typed_load_error_through_execute`] (the
//!    discriminating case: fails against an implementation that dropped the explicit
//!    `BindingPlan::Imu`/`BindingPlan::StarTracker` guard and relied on the generic length check
//!    alone, since that generic check alone cannot distinguish a 6-component bias-random-walk
//!    state from a 6-component Cartesian one -- exactly `docs/sil-plan.md`'s "this defect class
//!    has recurred three times" pattern, one level up), plus
//!    [`a_maneuver_targeting_the_star_tracker_instance_is_a_typed_load_error_through_execute`]
//!    (the zero-dimensional case, for parity with the IMU test above).
//!
//! **Not reproduced here (deliberate, disclosed scope call):** a covariance-request refusal
//! against a sensor instance specifically, through `execute()`. `crate::drm::executor::
//! run_covariance_instance`'s refusal (`DrmError::ModelNotStmCapable`) is generic over every
//! `AnyModel` variant and is already proven live through `execute()` for `AnyModel::Attitude`
//! (`crates/av-kernel/tests/drm_attitude.rs::covariance_requested_against_an_attitude_instance_
//! is_a_typed_refusal_through_execute`); `AnyModel::StarTracker`/`AnyModel::Imu::stm_capable()`
//! being unconditionally `false` is independently proven at the unit level
//! (`crate::registry::tests::construct_star_tracker_builds_a_usable_zero_dimensional_handle`/
//! `construct_imu_builds_a_usable_six_dimensional_handle_with_zero_initial_bias`, both asserting
//! `!handle.stm_capable()`) and at the `AnyModel` delegation level
//! (`crate::drm::binding::tests::any_model_stm_capable_delegates_to_the_star_tracker_variant`/
//! `..._to_the_imu_variant`). Since `demo_attitude_sensors.sos.yaml`'s own instance order places
//! the attitude-bound `attitude` instance first, a covariance request against this exact fixture
//! would only re-exercise the already-proven `Attitude` arm's refusal (`run_covariance_instance`
//! iterates `SosConfiguration.instances` in declared order and returns on the first refusal) --
//! reaching the sensor arm specifically would need a *different*, sensor-instance-first DRM
//! fixture built solely to re-prove a generic code path against a variant already proven
//! `!stm_capable()` two other ways. Judged not worth a new fixture; disclosed rather than
//! silently skipped.
//!
//! Every test here constructs a real `gmat_sys::Gmat` handle (`RunConfig.gmat`) even though none
//! of these DRMs ever bind a `"gmat."` instance -- `execute` takes `&Gmat` unconditionally
//! (ADR-002, GMAT is a process-wide singleton) -- and takes `gmat_sys::engine_lock()` first, per
//! this repository's existing convention (`tests/drm_executor.rs`'s own module doc comment,
//! `tests/drm_attitude.rs`'s own copy of it).

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

/// The three-instance `demo_attitude_sensors` bundle: the attitude truth source plus both
/// sensors, keyed by their own `SystemDefinition.id` (matching `demo_attitude_sensors.sos.yaml`'s
/// own `system_id` references).
fn load_sensors_bundle() -> (DesignReferenceMission, SosConfiguration, BTreeMap<String, SystemDefinition>) {
    let drm = schema::parse_drm_yaml(&read("demo_attitude_sensors.drm.yaml")).expect("DRM parses");
    let sos = schema::parse_sos_yaml(&read("demo_attitude_sensors.sos.yaml")).expect("SosConfiguration parses");
    let truth = load_system("demo_attitude_sensors_truth");
    let star = load_system("demo_attitude_sensors_startracker");
    let imu = load_system("demo_attitude_sensors_imu");
    let mut systems = BTreeMap::new();
    systems.insert(truth.id.clone(), truth);
    systems.insert(star.id.clone(), star);
    systems.insert(imu.id.clone(), imu);
    (drm, sos, systems)
}

fn run_config<'a>(gmat: &'a Gmat, drm: &'a DesignReferenceMission, sos: &'a SosConfiguration, systems: &'a BTreeMap<String, SystemDefinition>) -> RunConfig<'a> {
    RunConfig { gmat, drm, sos, systems, run_id: "test-run-drm-attitude-sensors".to_string(), error_mode: Default::default() , products_dir: None, replay: None }
}

/// Re-hash a `DesignReferenceMission` after a test has mutated it in memory -- mirrors
/// `tests/drm_attitude.rs::rehash`'s own doc comment (not a way to bypass `execute`'s tamper
/// check; the opposite: keeps a deliberately mutated in-memory fixture variant honestly
/// self-consistent).
fn rehash_drm(mut drm: DesignReferenceMission) -> DesignReferenceMission {
    drm.hash = hash::canonical_drm_hash(&drm);
    drm
}

/// The `SystemDefinition` counterpart of [`rehash_drm`] -- used by the different-seed test to
/// keep a mutated `imu.seed` parameter's own declared hash honest, exactly the pattern
/// `tests/drm_attitude.rs::execute_refuses_a_wheel_momentum_component_declared_with_the_torque_
/// unit` already uses for a mutated `SystemDefinition`.
fn rehash_system(mut sys: SystemDefinition) -> SystemDefinition {
    sys.hash = hash::canonical_system_hash(&sys);
    sys
}

// =============================================================================================
// Exit criteria 1 + 2: the DRM runs through execute(), and both sensors emit at their own
// declared 2 Hz rate (not the kernel's 1 Hz step rate) -- the exact expected count derived here.
// =============================================================================================

/// **Expected count, derived before measuring.** `demo_attitude_sensors.drm.yaml` runs 6 s
/// (`1767225637000000000` to `1767225643000000000`) at a 1 Hz kernel step (`default_step_rate_
/// hz`); both sensors declare `*.update_rate_hz = 2.0` (a 500 ms period), which does not divide
/// evenly into whole kernel steps but does divide evenly into the 6 s run: `6 s / 0.5 s = 12`
/// scheduled emissions per sensor, spread two-per-kernel-step (due at `k*0.5s` for
/// `k=1..=12`) -- the identical count `crate::drm::sensors::tests::attitude_instance_is_measured_
/// by_both_sensors_through_the_real_router` already established directly against a real
/// `crate::router::Router` for this exact same topology and rates.
///
/// Each `StepResult.outputs["seq"]` records the *zero-indexed* CCSDS sequence number of the last
/// packet pushed *this step* (`crate::drm::sensors::StarTrackerModel::step_with_ports`'s own doc
/// comment) -- so with 12 total emissions numbered `0..=11`, the value `output.<instance>.
/// seq@end` resolves to after the run's final step is **11**, not 12. Both facts (12 total
/// emissions; last recorded sequence number 11) are asserted below.
///
/// Fails against an implementation that never wires `"startracker."`/`"imu."` dispatch into
/// `crate::registry::kind_for` at all (the DRM would be refused before propagation starts, the
/// same way M22.2's own disclosed gap left this fixture only load-time-testable) or one whose
/// `AnyModel::StarTracker`/`AnyModel::Imu::step_with_ports` arm silently reaches a trait default
/// instead of delegating (no packets would ever be scheduled, and the `imu`/`startracker`
/// `output.seq` series would never gain an entry at all -- `execute()` would fail resolving the
/// declared `MeasureOfEffectiveness` rather than reporting a wrong count).
#[test]
fn the_sensors_drm_runs_through_execute_and_emits_at_the_declared_rate() {
    let _engine = gmat_sys::engine_lock();
    let (drm, sos, systems) = load_sensors_bundle();
    let mut drm = drm;
    drm.measures = vec![
        MeasureOfEffectiveness { name: "st_seq_at_end".to_string(), expression: "output.startracker.seq@end".to_string(), unit: Unit::Dimensionless as i32 },
        MeasureOfEffectiveness { name: "imu_seq_at_end".to_string(), expression: "output.imu.seq@end".to_string(), unit: Unit::Dimensionless as i32 },
    ];
    let drm = rehash_drm(drm);
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let products = execute(run_config(&gmat, &drm, &sos, &systems)).expect("the three-instance sensors DRM executes end to end");

    // `execute()`'s own M21.3/M22.2b convention (`crate::drm::executor::execute`'s own
    // `emits_no_trajectory` match): an instance whose materialized model has zero propagated
    // physical state emits no `Trajectory` entry at all, not an empty/zero-width one --
    // `StarTrackerModel::state_dim() == 0` always, so "startracker" is deliberately absent here,
    // exactly like a dim-0 native `ConstantAccel` instance already is. Only "attitude" (7+
    // components) and "imu" (the 6-component bias random walk) produce trajectories.
    assert_eq!(products.trajectories.len(), 2, "attitude and imu each produce a trajectory; startracker deliberately does not (state_dim() == 0)");
    assert!(products.trajectories.contains_key("attitude"));
    assert!(!products.trajectories.contains_key("startracker"), "a state_dim() == 0 instance emits no trajectory entry at all (executor.rs's own emits_no_trajectory rule)");
    assert!(products.trajectories.contains_key("imu"));

    // The IMU's own bias-random-walk state is 6-dimensional and genuinely propagated.
    let imu_traj = &products.trajectories["imu"];
    assert!(imu_traj.samples.iter().all(|s| s.mean.len() == 6), "ImuModel::state_dim() == 6 (the bias random walk)");

    let st_score = &products.scores["st_seq_at_end"];
    let imu_score = &products.scores["imu_seq_at_end"];
    assert_eq!(st_score.unit, Unit::Dimensionless);
    assert_eq!(st_score.passed, None, "a MeasureOfEffectiveness never carries pass/fail");
    assert_eq!(st_score.value, 11.0, "star tracker: 12 total emissions (0..=11) over the 6s run, last recorded seq = 11");
    assert_eq!(imu_score.value, 11.0, "imu: 12 total emissions (0..=11) over the 6s run, last recorded seq = 11");
}

// =============================================================================================
// Exit criterion 3: determinism -- same seed byte-identical, different seed differs, both
// asserted (the brief's own explicit "the first alone would pass a model that ignores the seed"
// warning).
// =============================================================================================

/// Two `execute()` runs of the identical, unmutated bundle produce byte-identical IMU
/// trajectories -- `ImuModel`'s only randomness is a `Pcg64` seeded once, at construction, from
/// the declared `imu.seed` parameter (`crate::drm::sensors`'s own module doc comment, trap 4),
/// so re-materializing the identical spec from the identical hashed `SystemDefinition` must
/// reproduce the identical bias-random-walk draws every time. Compares every sample's `mean`
/// vector (the IMU's own propagated bias state, `[bias_gyro_x,y,z, bias_accel_x,y,z]`)
/// element-for-element -- not merely the final sample -- so a regression that only fixed the
/// *final* value by coincidence while drawing a genuinely different sequence in between would
/// still be caught. Fails against an implementation that reseeds from wall-clock time, a
/// per-process counter, or any other non-reproducible source instead of the declared seed.
#[test]
fn two_runs_of_the_sensors_drm_with_the_same_seed_are_byte_identical_for_the_imu_bias_trajectory() {
    let _engine = gmat_sys::engine_lock();
    let (drm, sos, systems) = load_sensors_bundle();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");

    let run_a = execute(run_config(&gmat, &drm, &sos, &systems)).expect("first run executes");
    let run_b = execute(run_config(&gmat, &drm, &sos, &systems)).expect("second run executes");

    let means_a: Vec<&Vec<f64>> = run_a.trajectories["imu"].samples.iter().map(|s| &s.mean).collect();
    let means_b: Vec<&Vec<f64>> = run_b.trajectories["imu"].samples.iter().map(|s| &s.mean).collect();
    assert_eq!(means_a.len(), means_b.len());
    assert!(!means_a.is_empty(), "sanity: the run must actually produce samples");
    assert_eq!(means_a, means_b, "two runs of the identical, identically-seeded DRM must produce byte-identical IMU bias trajectories");

    // Sanity companion (not the point of this test, but guards against a vacuous pass): the bias
    // random walk actually moved away from its zero initial condition by the end of the run --
    // an all-zero trajectory would trivially satisfy the equality check above for the wrong
    // reason (a dead RNG, not a working deterministic one).
    let last = run_a.trajectories["imu"].samples.last().unwrap();
    assert!(last.mean.iter().any(|&v| v != 0.0), "the bias random walk must have actually advanced away from zero by the end of a 6s run");
}

/// Re-runs with `imu.seed` mutated to a different value (42 instead of the fixture's declared 2)
/// and re-hashed (mirrors `tests/drm_attitude.rs::execute_refuses_a_wheel_momentum_component_
/// declared_with_the_torque_unit`'s own "mutate a loaded SystemDefinition, recompute its own
/// hash" pattern) produce a genuinely different IMU bias trajectory -- the companion assertion
/// the brief is explicit is required alongside the same-seed test above: a model that silently
/// ignores the declared seed (e.g. always draws from a fixed internal state) would pass the
/// same-seed test above and *also* incorrectly pass if this test only checked "the run
/// succeeds" -- it must check the physical output actually changed.
#[test]
fn a_different_imu_seed_changes_the_propagated_bias_trajectory() {
    let _engine = gmat_sys::engine_lock();
    let (drm, sos, systems) = load_sensors_bundle();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");

    let baseline = execute(run_config(&gmat, &drm, &sos, &systems)).expect("baseline run executes");

    let mut reseeded_systems = systems.clone();
    let imu_sys = reseeded_systems.get_mut("attitude_sensors_imu_sys").expect("imu system present");
    let seed_param = imu_sys.parameters.iter_mut().find(|p| p.name == "imu.seed").expect("imu.seed is declared");
    assert_eq!(seed_param.value, 2.0, "sanity: the fixture's own declared seed before mutation");
    seed_param.value = 42.0;
    let mutated_id = imu_sys.id.clone();
    let mutated = rehash_system(reseeded_systems.remove(&mutated_id).unwrap());
    reseeded_systems.insert(mutated_id, mutated);

    let reseeded = execute(run_config(&gmat, &drm, &sos, &reseeded_systems)).expect("reseeded run executes");

    let baseline_means: Vec<&Vec<f64>> = baseline.trajectories["imu"].samples.iter().map(|s| &s.mean).collect();
    let reseeded_means: Vec<&Vec<f64>> = reseeded.trajectories["imu"].samples.iter().map(|s| &s.mean).collect();
    assert_ne!(baseline_means, reseeded_means, "a different declared imu.seed must produce a genuinely different bias-random-walk trajectory, not a bit-identical one");
}

// =============================================================================================
// Exit criterion 4: a maneuver targeting a sensor instance is a typed load error, reproduced
// through execute() -- the IMU case is the discriminating one (state_dim() == 6 too).
// =============================================================================================

/// **The discriminating case.** `ImuModel::state_dim() == 6` -- the identical width a real
/// six-component Cartesian position/velocity state has -- so `crate::drm::executor::execute`'s
/// boundary loop must refuse a maneuver targeting `BindingPlan::Imu` *by variant*, explicitly,
/// before its generic "is this length 6" check ever runs (that generic check, run alone, would
/// happily accept a 6-long IMU bias vector and apply a translational dv jump to a bias state --
/// physically meaningless, and silently wrong rather than refused). Fails against an
/// implementation that deletes the explicit `matches!(span.cur_plan, BindingPlan::Imu(_) |
/// BindingPlan::StarTracker(_))` guard and relies on the generic six-component length check
/// alone: the maneuver would be silently *applied* (mutating the bias state via
/// `apply_dv_to_state`) instead of refused, and this test's `unwrap_err()` would panic on an
/// `Ok(_)` instead.
#[test]
fn a_maneuver_targeting_the_imu_instance_is_a_typed_load_error_through_execute() {
    let _engine = gmat_sys::engine_lock();
    let (drm, sos, systems) = load_sensors_bundle();
    let start = drm.scenario.as_ref().unwrap().start_tai_ns;
    let mut drm = drm;
    {
        let scenario = drm.scenario.as_mut().unwrap();
        scenario.events.push(ScenarioEvent {
            id: "imu_burn".to_string(),
            tai_ns: start + 2_000_000_000, // t=2s, on the 1 Hz sample grid
            kind: "maneuver".to_string(),
            instance: "imu".to_string(),
            values: BTreeMap::from([("dv_x".to_string(), 1.0), ("dv_y".to_string(), 0.0), ("dv_z".to_string(), 0.0)]),
            attributes: BTreeMap::from([("frame_id".to_string(), "AXES_KIND_VNB".to_string())]),
            execution_error: None,
        });
    }
    let drm = rehash_drm(drm);
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let err = execute(run_config(&gmat, &drm, &sos, &systems)).unwrap_err();
    assert!(
        matches!(err, DrmError::ManeuverTargetNotSixDimensional { ref instance, ref maneuver_id, state_dim: 6 } if instance == "imu" && maneuver_id == "imu_burn"),
        "{err:?}"
    );
}

/// The star tracker counterpart -- `StarTrackerModel::state_dim() == 0`, so (unlike the IMU case
/// above) the generic length check alone would also catch this one; kept for parity with the
/// IMU test and to prove `BindingPlan::StarTracker` is named in the same explicit guard, not
/// just `BindingPlan::Imu`.
#[test]
fn a_maneuver_targeting_the_star_tracker_instance_is_a_typed_load_error_through_execute() {
    let _engine = gmat_sys::engine_lock();
    let (drm, sos, systems) = load_sensors_bundle();
    let start = drm.scenario.as_ref().unwrap().start_tai_ns;
    let mut drm = drm;
    {
        let scenario = drm.scenario.as_mut().unwrap();
        scenario.events.push(ScenarioEvent {
            id: "st_burn".to_string(),
            tai_ns: start + 2_000_000_000,
            kind: "maneuver".to_string(),
            instance: "startracker".to_string(),
            values: BTreeMap::from([("dv_x".to_string(), 1.0), ("dv_y".to_string(), 0.0), ("dv_z".to_string(), 0.0)]),
            attributes: BTreeMap::from([("frame_id".to_string(), "AXES_KIND_VNB".to_string())]),
            execution_error: None,
        });
    }
    let drm = rehash_drm(drm);
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let err = execute(run_config(&gmat, &drm, &sos, &systems)).unwrap_err();
    assert!(
        matches!(err, DrmError::ManeuverTargetNotSixDimensional { ref instance, ref maneuver_id, state_dim: 0 } if instance == "startracker" && maneuver_id == "st_burn"),
        "{err:?}"
    );
}

// =============================================================================================
// Question 173 (M25.3): the required negative proof, using this file's own real, committed,
// unmodified fixture -- "nothing is synthesized for a packet the codec does not map."
// =============================================================================================

/// **This fixture's own `packet_codecs` (`demo_attitude_sensors_startracker.system.yaml`/
/// `_imu.system.yaml`) declare no `PacketField.target` on any field** (M22.2 predates question
/// 173) -- confirmed by reading both files, not assumed. Both sensors genuinely emit FRAMED
/// telemetry every declared period (the test above already proves 12 real packets each, via
/// `output.<instance>.seq@end`), so this is a real, non-trivial negative case: telemetry exists,
/// and still maps to nothing, because the codec says to map nothing.
///
/// **Fails against an implementation that invents a measurement anyway** -- e.g. one that falls
/// back to the codec's own `id` (`"st_meas_codec"`/`"imu_meas_codec"`) as a measurement id when
/// no field declares a `target`, or one that maps every `Numeric` field regardless of whether its
/// `target` is set. Either bug would make `RunProducts.measurements` non-empty here; the correct
/// implementation leaves it exactly empty.
#[test]
fn existing_sensor_telemetry_with_no_declared_target_produces_no_measurement() {
    let _engine = gmat_sys::engine_lock();
    let star = load_system("demo_attitude_sensors_startracker");
    let imu = load_system("demo_attitude_sensors_imu");
    assert!(star.packet_codecs.iter().all(|c| c.fields.iter().all(|f| f.target.is_empty())), "fixture assumption: no star tracker field declares a target");
    assert!(imu.packet_codecs.iter().all(|c| c.fields.iter().all(|f| f.target.is_empty())), "fixture assumption: no IMU field declares a target");

    let (drm, sos, systems) = load_sensors_bundle();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let products = execute(run_config(&gmat, &drm, &sos, &systems)).expect("the sensors DRM executes");

    assert!(
        products.measurements.is_empty(),
        "a codec that declares no PacketField.target must produce no Measurement at all, even though both sensors genuinely emitted real telemetry this run: got {:?}",
        products.measurements
    );
}
