//! M15.1 required test (`docs/open-questions.md` question 115): a maneuver on an instance always
//! keeps its own boundary, even in the one case where the merge rule's other half -- "adjacent
//! segments merge when their `dynamics_hash` is equal" -- would, on its own, wrongly allow a
//! merge.
//!
//! `av_kernel::drm::executor::apply_dv_to_state`/`materialize_plan_at_boundary` never touch an
//! instance's own `cur_plan` at a maneuver boundary (only its physical state) -- see
//! `crate::drm::executor::run_shared_group`'s own doc comment's "Faults and maneuvers" section.
//! So a maneuver that happens to follow a DYNAMICS fault (the fault having already changed the
//! instance's own settings once) re-materializes with the *same* settings the fault-changed
//! segment already had: `TrajectorySegment.dynamics_hash` genuinely comes out identical either
//! side of a maneuver boundary in that shape. If `merge_adjacent_segments` merged on hash equality
//! alone, it would collapse those two segments into one, silently erasing the recorded velocity
//! discontinuity from `Trajectory.segments` (the samples would still show the jump, but nothing
//! in `segments` would mark where the dynamics configuration in effect at that instant changed
//! hands from one materialization to the next, and a caller reading only `segments` to find
//! maneuver boundaries -- e.g. to know how many independent GMAT/native re-materializations
//! actually happened -- would miss one). [`maneuver_never_merges_its_own_boundary_even_when_the_
//! dynamics_hash_is_unchanged`] proves the merge rule's maneuver check is load-bearing on its own,
//! not merely a restatement of the hash check, by exhibiting exactly the case where the two halves
//! of the rule would disagree if the maneuver check were dropped.
//!
//! This crate's own `merge_adjacent_segments_tests` module (`src/drm/executor.rs`) already proves
//! the same claim at the unit level, directly, with no GMAT/kernel machinery involved
//! (`identical_hash_across_a_maneuver_boundary_still_does_not_merge`,
//! `a_fault_then_a_maneuver_back_to_the_same_hash_still_keeps_three_segments`). This file proves
//! it end to end, through the real `execute()` path this task's decision actually governs, with a
//! real (native, GMAT-free) fault and a real maneuver.

use std::collections::BTreeMap;

use av_cdm::pb::{
    Binding, BindingKind, DesignReferenceMission, DrmOptions, Fault, FaultTargetKind, ModelBinding, Parameter, Scenario, ScenarioEvent, SosConfiguration, SystemDefinition, SystemInstance,
};
use av_kernel::drm::{execute, hash, RunConfig};
use gmat_sys::Gmat;

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

const FAULT_TAI_NS: i64 = 1_000_000_000;
const MANEUVER_TAI_NS: i64 = 1_500_000_000;
const END_TAI_NS: i64 = 2_000_000_000;

/// Required test. See this file's own module doc comment for exactly what a merge-on-hash-alone
/// implementation would do wrong here, and why this specific fixture (a fault immediately
/// followed by a maneuver, on the *same* instance, with nothing else changing the dynamics
/// configuration between the two) is the one shape that actually exercises the difference between
/// "merge on hash equality" and "merge on hash equality AND no own maneuver."
#[test]
fn a_maneuver_never_merges_its_own_boundary_even_when_the_dynamics_hash_is_unchanged() {
    let _engine = gmat_sys::engine_lock();

    let sys = hashed_system(SystemDefinition {
        id: "segmerge_sys".to_string(),
        dynamics_model: "native.constant_accel".to_string(),
        state_space_id: "gmat.orbital.cartesian6".to_string(),
        parameters: vec![
            param("accel.x", 1.0),
            param("accel.y", 0.0),
            param("accel.z", 0.0),
            sparam("frame_id", "test.frame"),
            param("state.px", 0.0),
            param("state.py", 0.0),
            param("state.pz", 0.0),
            param("state.vx", 0.0),
            param("state.vy", 0.0),
            param("state.vz", 0.0),
        ],
        ..Default::default()
    });
    let sos = hashed_sos(SosConfiguration {
        id: "segmerge_sos".to_string(),
        instances: vec![SystemInstance {
            name: "veh".to_string(),
            system_id: sys.id.clone(),
            binding: Some(Binding { kind: BindingKind::Model as i32, config: Some(av_cdm::pb::binding::Config::Model(ModelBinding { model_id: sys.id.clone() })) }),
            step_rate_hz: 10.0,
            ..Default::default()
        }],
        ..Default::default()
    });
    let drm = hashed_drm(DesignReferenceMission {
        id: "segmerge_drm".to_string(),
        sos_configuration_id: sos.id.clone(),
        scenario: Some(Scenario {
            start_tai_ns: 0,
            end_tai_ns: END_TAI_NS,
            // Fault at 1 s changes accel.x (1.0 -> 5.0): dynamics_hash changes, so segment 0 and
            // segment 1 must not merge -- this is `tests/drm_executor.rs::a_dynamics_fault_
            // splits_the_run_into_two_segments_with_continuous_state`'s own already-required
            // claim, reused here as the setup for the real test below, not re-asserted for its
            // own sake.
            faults: vec![Fault {
                id: "f1".to_string(),
                tai_ns: FAULT_TAI_NS,
                target_kind: FaultTargetKind::Dynamics as i32,
                instance: "veh".to_string(),
                target: "accel.x".to_string(),
                kind: "parameter".to_string(),
                params: BTreeMap::from([("value".to_string(), 5.0)]),
                ..Default::default()
            }],
            // Maneuver at 1.5 s: touches only state (a velocity jump), never `cur_plan` --
            // segment 1's (post-fault) and segment 2's (post-maneuver) settings are therefore
            // identical, so their dynamics_hash comes out equal, the exact case this test exists
            // to exercise.
            events: vec![ScenarioEvent {
                id: "burn1".to_string(),
                tai_ns: MANEUVER_TAI_NS,
                kind: "maneuver".to_string(),
                instance: "veh".to_string(),
                values: BTreeMap::from([("dv_x".to_string(), 0.0), ("dv_y".to_string(), 5.0), ("dv_z".to_string(), 0.0)]),
                attributes: BTreeMap::from([("frame_id".to_string(), "AXES_KIND_ICRF".to_string())]),
                execution_error: None,
            }],
            ..Default::default()
        }),
        options: Some(DrmOptions { default_step_rate_hz: 10.0, sample_interval_s: 0.1, ..Default::default() }),
        ..Default::default()
    });

    let mut systems = BTreeMap::new();
    systems.insert(sys.id.clone(), sys);
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let cfg = RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: "test-segment-merge-maneuver".to_string(), error_mode: Default::default() , products_dir: None };
    let products = execute(cfg).expect("fault-then-maneuver DRM executes end to end");

    let traj = products.trajectories.get("veh").expect("instance produced a trajectory");

    // The claim this test exists to pin: three segments survive, not two (which a
    // merge-on-hash-alone implementation would produce by collapsing segments 1 and 2) and not
    // one (which an implementation that merged unconditionally, ignoring dynamics_hash entirely,
    // would produce).
    assert_eq!(traj.segments.len(), 3, "fault then maneuver on the SAME instance -> three segments must survive -- got {:?}", traj.segments);

    // Segment 0 (pre-fault) vs segment 1 (post-fault): the fault changed accel.x, so the hash
    // must differ -- this is the reason segments 0/1 do not merge, verified directly rather than
    // assumed (question 115's own instruction).
    assert_ne!(traj.segments[0].dynamics_hash, traj.segments[1].dynamics_hash, "the fault's changed accel.x must change the settings hash -- segment 0 vs segment 1");

    // Segment 1 (post-fault) vs segment 2 (post-maneuver): the maneuver changed nothing about the
    // dynamics configuration (only the state), so the hash is genuinely UNCHANGED here -- this is
    // the load-bearing assertion: if this ever becomes `assert_ne!`, the fixture has stopped
    // exercising the maneuver check at all and this test should be revisited.
    assert_eq!(
        traj.segments[1].dynamics_hash, traj.segments[2].dynamics_hash,
        "a maneuver must not change the settings hash -- segment 1 (post-fault) and segment 2 (post-maneuver) must share the identical dynamics_hash, proving the fixture actually exercises \
         the maneuver-keeps-its-boundary rule and not merely the hash-differs rule"
    );

    // And yet, despite that identical hash, segments 1 and 2 are NOT merged -- the boundary
    // between them is a maneuver on this same instance, which the merge rule keeps unconditionally
    // (`docs/open-questions.md` question 115: "a maneuver on the instance itself always keeps its
    // boundary"). This is what `assert_eq!(traj.segments.len(), 3, ...)` above already proves; the
    // segment bounds are checked explicitly here too so a future change that merges 1 and 2 into
    // one wide `[FAULT_TAI_NS, END_TAI_NS]` segment (same `dynamics_hash`, so `segments.len()`
    // could stay 3 only by coincidence in some other broken shape) cannot pass by accident.
    assert_eq!(traj.segments[1].start_tai_ns, FAULT_TAI_NS);
    assert_eq!(traj.segments[1].end_tai_ns, MANEUVER_TAI_NS);
    assert_eq!(traj.segments[2].start_tai_ns, MANEUVER_TAI_NS);
    assert_eq!(traj.segments[2].end_tai_ns, END_TAI_NS);

    // Closed-form sanity that the maneuver's own velocity jump really happened (not a vacuous
    // "three segments, but nothing actually discontinuous" case): vy jumps by 5.0 m/s exactly at
    // MANEUVER_TAI_NS, the post-burn sample kept at that epoch.
    let at_maneuver = traj.samples.iter().find(|s| s.tai_ns == MANEUVER_TAI_NS).expect("a sample at the maneuver epoch");
    assert!((at_maneuver.mean[4] - 5.0).abs() < 1e-9, "vy jumps to 5.0 at the post-burn sample = {}", at_maneuver.mean[4]);
}
