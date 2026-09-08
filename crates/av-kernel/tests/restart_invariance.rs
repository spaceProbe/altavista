//! M14.4 required test: multi-instance restart invariance, measured directly rather than
//! asserted by symmetry.
//!
//! M14.1 (`docs/open-questions.md` question 109) generalized the fault/maneuver boundary split
//! from one `BINDING_KIND_MODEL` instance to N: at every boundary, every currently active model
//! instance is re-materialized from its own last physical state, whether or not it is the
//! boundary's own target (`av_kernel::drm::executor::run_shared_group`'s own doc comment,
//! "Faults and maneuvers: the split now applies to the whole kernel run"). That function's own
//! doc comment says plainly: **"No required or existing test in this crate exercises a DRM with
//! two or more `BINDING_KIND_MODEL` instances where only some of them carry a fault/maneuver" --
//! restart-invariance for an unaffected, coarser-period instance at a boundary is asserted by
//! symmetry with the single-instance case, not separately measured against a golden.** This file
//! is the first thing in this crate to actually measure it, for two shapes:
//!
//! 1. Two `BINDING_KIND_MODEL` instances, one carrying a `FAULT_TARGET_KIND_DYNAMICS` fault and
//!    later a maneuver, the other untouched
//!    ([`two_model_instances_only_one_faulted_and_maneuvered_matches_each_single_instance_run`]).
//! 2. The same, with a `BINDING_KIND_CONTAINER` instance running alongside the faulted+maneuvered
//!    model instance instead of (in addition to) the untouched one
//!    ([`a_container_instance_alongside_a_faulted_model_instance_matches_each_single_instance_run`]).
//!
//! Both compare [`physical_shape`] (entity_id, state_space_id, interpolation, samples --
//! everything about a `Trajectory` that describes the physics itself) between the untouched (or
//! container) instance's single-instance run and its own trajectory inside the combined run.
//! `config_hash`/`provenance` are excluded because they necessarily differ between a
//! single-instance `SosConfiguration` and a multi-instance one sharing the same instance, the
//! same exclusion `tests/drm_shared_run.rs::two_unconnected_instances_match_what_each_produces_
//! run_alone` already established for the *unaffected* no-fault case this file extends to the
//! fault/maneuver case.
//!
//! **What each test would catch.** If `run_shared_group`'s "re-materialize every other active
//! instance too" path (the doc comment's own "generalizes that same, already-accepted mechanism
//! from one instance to every active one") had a bug specific to N > 1 -- e.g. an off-by-one in
//! which instance's own `x0`/plan gets updated at a boundary, a stale `handle` reused across
//! spans for the wrong instance, or the untouched instance accidentally inheriting the target's
//! own re-bound plan -- the untouched instance's own `samples` in the combined run would
//! silently diverge from its single-instance run (wrong physics from the boundary onward, not a
//! crash), which only a direct byte-for-byte comparison against a real single-instance run (not
//! an assertion that the run merely "completed" or "looks plausible") can catch. The faulted
//! instance's own comparison additionally guards against the reverse bug -- the *target*
//! instance somehow being perturbed by the presence of another instance in the shared kernel run
//! it would not have been perturbed by alone.
//!
//! **Restart invariance did NOT fully hold when this file was first written, and this is where
//! that was found, not asserted away.** `samples` (the actual physical state at every output
//! tick) were already byte-identical either way, for both the untouched model instance and the
//! untouched container instance -- genuine restart invariance at the level ADR-005 cares about (a
//! re-materialized model propagates forward identically to one that was never split). But
//! `Trajectory.segments` was **not** invariant: `run_shared_group` re-materializes *every*
//! currently active instance at *every* boundary (that function's own doc comment: "every other
//! active model instance is re-materialized from its own unchanged plan and its own
//! continuously-sampled state"), so an untouched instance alongside a two-boundary (fault, then
//! maneuver) target instance used to end up with **three** `TrajectorySegment` entries instead of
//! the one segment its own single-instance run produces -- even though its own `dynamics_hash`
//! was identical across all three (proving its own dynamics configuration genuinely never
//! changed; only the bookkeeping fragmented).
//!
//! **M15.1 (`docs/open-questions.md` question 115) fixes this.** `av_kernel::drm::executor::
//! merge_adjacent_segments` now collapses adjacent segments of one instance when their
//! `dynamics_hash` is equal AND the boundary between them was not a maneuver on that instance --
//! exactly the untouched-bystander case this file measures. Both tests below were upgraded from
//! asserting only `samples` (`physical_shape`) plus the old three-segments-one-hash finding to
//! asserting `segments` themselves byte-identical between the alone and together runs
//! (`assert_eq!(traj_*_alone.segments, traj_*_together.segments)`), for both the untouched
//! participant AND the faulted+maneuvered target -- restart invariance now holds at both levels
//! this file checks. See `crates/av-kernel/src/drm/executor.rs`'s own module doc comment's
//! "Segment merge across an unaffected boundary" section for the merge rule itself, and the proof
//! that a DYNAMICS fault really does change `dynamics_hash` (so a fault boundary is never wrongly
//! merged). This file uses native `"accel.*"` bindings for both the target and the untouched model
//! instance -- through M18.3, a GMAT-bound bystander was a disclosed *exception* to this merge
//! (its own `dynamics_hash` baked in its instantaneous Cartesian state and so differed at every
//! re-materialization regardless of whether anything was actually reconfigured, `docs/open-
//! questions.md` question 127's second half). **M18.4 closes that exception**: `binding::
//! gmat_settings` no longer hashes a GMAT-bound instance's own state, so the identical merge rule
//! this file already measures for a native binding now holds for a real GMAT-bound bystander too
//! -- measured directly, not merely assumed, by `tests/demo_two_instance.rs::
//! demo_two_instance_bystander_invariance_against_real_single_instance_gmat_runs`.

use std::collections::BTreeMap;
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use av_cdm::pb::{
    Binding, BindingKind, ContainerBinding, DesignReferenceMission, DrmOptions, Fault, FaultTargetKind, ModelBinding, Parameter, Port, PortDirection, PortKind, Scenario, ScenarioEvent,
    SosConfiguration, StateSpace, SystemDefinition, SystemInstance, Trajectory,
};
use av_kernel::drm::{execute, hash, RunConfig};
use gmat_sys::Gmat;

// ------------------------------------------------------------------------------------------
// Shared fixture helpers (mirrors tests/drm_shared_run.rs / tests/drm_maneuver.rs).
// ------------------------------------------------------------------------------------------

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
fn run_config<'a>(gmat: &'a Gmat, drm: &'a DesignReferenceMission, sos: &'a SosConfiguration, systems: &'a BTreeMap<String, SystemDefinition>, run_id: &str) -> RunConfig<'a> {
    RunConfig { gmat, drm, sos, systems, run_id: run_id.to_string(), error_mode: Default::default() , products_dir: None, replay: None }
}
fn one_system_map(sys: &SystemDefinition) -> BTreeMap<String, SystemDefinition> {
    BTreeMap::from([(sys.id.clone(), sys.clone())])
}

/// A native (GMAT-free) `"accel.x"`/`"accel.y"`/`"accel.z"` binding with a zero initial state --
/// identical shape to `tests/drm_maneuver.rs::accel_system`/`tests/drm_shared_run.rs::
/// accel_system`, parametrized on `id` and the constant acceleration so two instances can carry
/// different, independently checkable physics.
fn accel_system(id: &str, a: [f64; 3]) -> SystemDefinition {
    hashed_system(SystemDefinition {
        id: id.to_string(),
        dynamics_model: "native.constant_accel".to_string(),
        state_space_id: "gmat.orbital.cartesian6".to_string(),
        parameters: vec![
            param("accel.x", a[0]),
            param("accel.y", a[1]),
            param("accel.z", a[2]),
            sparam("frame_id", "test.frame"),
            param("state.px", 0.0),
            param("state.py", 0.0),
            param("state.pz", 0.0),
            param("state.vx", 0.0),
            param("state.vy", 0.0),
            param("state.vz", 0.0),
        ],
        ..Default::default()
    })
}

fn model_instance(name: &str, system_id: &str, step_rate_hz: f64) -> SystemInstance {
    SystemInstance {
        name: name.to_string(),
        system_id: system_id.to_string(),
        binding: Some(Binding { kind: BindingKind::Model as i32, config: Some(av_cdm::pb::binding::Config::Model(ModelBinding { model_id: system_id.to_string() })) }),
        step_rate_hz,
        ..Default::default()
    }
}

const FAULT_TAI_NS: i64 = 1_000_000_000;
const MANEUVER_TAI_NS: i64 = 1_500_000_000;
const END_TAI_NS: i64 = 2_000_000_000;

/// A `FAULT_TARGET_KIND_DYNAMICS` fault at [`FAULT_TAI_NS`] changing `accel.x` from 1.0 (the
/// instance's own declared initial value) to 5.0, naming `instance`.
fn fault_on(instance: &str) -> Fault {
    Fault { id: "f1".to_string(), tai_ns: FAULT_TAI_NS, target_kind: FaultTargetKind::Dynamics as i32, instance: instance.to_string(), target: "accel.x".to_string(), kind: "parameter".to_string(), params: BTreeMap::from([("value".to_string(), 5.0)]), ..Default::default() }
}

/// A `"maneuver"` `ScenarioEvent` at [`MANEUVER_TAI_NS`], 5 m/s along inertial `y`, naming
/// `instance` -- fires strictly after `fault_on`'s own boundary, so the run this produces has
/// three dynamics segments (`[0, fault)`, `[fault, maneuver)`, `[maneuver, end]`).
fn maneuver_on(instance: &str) -> ScenarioEvent {
    ScenarioEvent {
        id: "burn1".to_string(),
        tai_ns: MANEUVER_TAI_NS,
        kind: "maneuver".to_string(),
        instance: instance.to_string(),
        values: BTreeMap::from([("dv_x".to_string(), 0.0), ("dv_y".to_string(), 5.0), ("dv_z".to_string(), 0.0)]),
        attributes: BTreeMap::from([("frame_id".to_string(), "AXES_KIND_ICRF".to_string())]),
        execution_error: None,
    }
}

/// `with_fault_and_maneuver` selects whether `instance` carries [`fault_on`]/[`maneuver_on`] --
/// `false` builds a plain, boundary-free scenario over the identical `[0, END_TAI_NS]` window.
fn scenario_for(instance: &str, with_fault_and_maneuver: bool) -> Scenario {
    let mut s = Scenario { start_tai_ns: 0, end_tai_ns: END_TAI_NS, ..Default::default() };
    if with_fault_and_maneuver {
        s.faults = vec![fault_on(instance)];
        s.events = vec![maneuver_on(instance)];
    }
    s
}

fn build_drm(sos_id: &str, drm_id: &str, instances: Vec<SystemInstance>, scenario: Scenario) -> (SosConfiguration, DesignReferenceMission) {
    let sos = hashed_sos(SosConfiguration { id: sos_id.to_string(), instances, ..Default::default() });
    let drm = hashed_drm(DesignReferenceMission {
        id: drm_id.to_string(),
        sos_configuration_id: sos.id.clone(),
        scenario: Some(scenario),
        options: Some(DrmOptions { default_step_rate_hz: 10.0, sample_interval_s: 0.1, ..Default::default() }),
        ..Default::default()
    });
    (sos, drm)
}

/// Everything about a `Trajectory` that describes the physics itself -- see this file's own
/// module doc comment for why `config_hash`/`Provenance` (necessarily different between a
/// single-instance `SosConfiguration` and a multi-instance one sharing the same instance) are
/// excluded, and why `segments` is compared *separately*, explicitly, rather than folded in here
/// (before M15.1 it was not invariant at all; after M15.1 it merges back to byte-identical, but
/// each test still asserts it as its own explicit comparison -- see the module doc comment's own
/// "M15.1 ... fixes this" section).
fn physical_shape(traj: &Trajectory) -> (String, String, i32, &Vec<av_cdm::pb::TrajectorySample>) {
    (traj.entity_id.clone(), traj.state_space_id.clone(), traj.interpolation, &traj.samples)
}

// ==========================================================================================
// 1. Two model instances, only one carrying a fault then a maneuver.
// ==========================================================================================

/// Required test. See this file's own module doc comment for exactly what a divergence here
/// would mean and what it would catch.
#[test]
fn two_model_instances_only_one_faulted_and_maneuvered_matches_each_single_instance_run() {
    let _engine = gmat_sys::engine_lock();
    let sys_a = accel_system("restart_a_sys", [1.0, 0.0, 0.0]);
    let sys_b = accel_system("restart_b_sys", [0.0, 2.0, 0.0]);

    let (sos_a, drm_a) = build_drm("restart_a_sos", "restart_a_drm", vec![model_instance("a", &sys_a.id, 10.0)], scenario_for("a", true));
    let (sos_b, drm_b) = build_drm("restart_b_sos", "restart_b_drm", vec![model_instance("b", &sys_b.id, 10.0)], scenario_for("b", false));
    let (sos_ab, drm_ab) = build_drm("restart_ab_sos", "restart_ab_drm", vec![model_instance("a", &sys_a.id, 10.0), model_instance("b", &sys_b.id, 10.0)], scenario_for("a", true));

    let systems_a = one_system_map(&sys_a);
    let systems_b = one_system_map(&sys_b);
    let systems_ab: BTreeMap<String, SystemDefinition> = BTreeMap::from([(sys_a.id.clone(), sys_a.clone()), (sys_b.id.clone(), sys_b.clone())]);

    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let products_a = execute(run_config(&gmat, &drm_a, &sos_a, &systems_a, "test-restart-a-alone")).expect("the faulted+maneuvered instance, run alone, executes");
    let products_b = execute(run_config(&gmat, &drm_b, &sos_b, &systems_b, "test-restart-b-alone")).expect("the untouched instance, run alone, executes");
    let products_ab = execute(run_config(&gmat, &drm_ab, &sos_ab, &systems_ab, "test-restart-ab-together")).expect("both instances together execute");

    // Sanity: the fault+maneuver actually happened (not a vacuous "nothing to compare" case).
    let traj_a_alone = products_a.trajectories.get("a").expect("instance a's own trajectory");
    let traj_a_together = products_ab.trajectories.get("a").expect("instance a's own trajectory in the combined run");
    assert_eq!(traj_a_alone.segments.len(), 3, "fault then maneuver -> three dynamics segments");
    assert_eq!(traj_a_together.segments.len(), 3, "identical boundary structure in the combined run");

    // The claim this test exists to measure, for the UNTOUCHED instance: its own physical
    // samples (position/velocity at every output tick) must be byte-identical whether it runs
    // alone or alongside a faulted+maneuvered instance.
    let traj_b_alone = products_b.trajectories.get("b").expect("instance b's own trajectory");
    let traj_b_together = products_ab.trajectories.get("b").expect("instance b's own trajectory in the combined run");
    assert_eq!(
        physical_shape(traj_b_alone),
        physical_shape(traj_b_together),
        "the UNTOUCHED instance's own physical samples must be byte-identical whether it runs alone or alongside a faulted+maneuvered instance -- \
         if this fails, report both traj_b_alone.samples and traj_b_together.samples below and diagnose where they first diverge (which sample \
         epoch, which component) rather than only reporting the failure"
    );

    // M15.1 (question 115): before the segment merge, instance b's own `segments` was NOT
    // invariant -- re-materialized (and so re-segmented) at every one of instance a's own
    // boundaries even though its own dynamics configuration never changed, ending up with three
    // segments sharing one identical `dynamics_hash` instead of the alone run's single segment.
    // The merge fixes this: asserted here as full byte-identity, not merely a matching hash.
    assert_eq!(traj_b_alone.segments.len(), 1, "run alone, instance b's own dynamics never changes -> one segment");
    assert_eq!(
        traj_b_together.segments, traj_b_alone.segments,
        "M15.1: the UNTOUCHED instance's own segments must merge back down to byte-identical with its single-instance run (no maneuver of its own, and its own dynamics_hash never actually changes) -- got {:?}",
        traj_b_together.segments
    );

    // And, symmetrically, the FAULTED+MANEUVERED instance must match its own single-instance run
    // -- both its physical samples and its own segment structure (three segments either way: its
    // own fault changes dynamics_hash, and its own maneuver keeps its boundary regardless of
    // hash, so nothing here is eligible to merge -- see tests/segment_merge.rs for that rule
    // proven directly).
    assert_eq!(
        physical_shape(traj_a_alone),
        physical_shape(traj_a_together),
        "the faulted+maneuvered instance's own physical trajectory must match its single-instance run, whether or not another instance shares the kernel run"
    );
    assert_eq!(traj_a_together.segments, traj_a_alone.segments, "the faulted+maneuvered instance's own segments must also match its single-instance run -- got {:?}", traj_a_together.segments);

    // Closed-form check that instance a's own physics really did what fault_on/maneuver_on
    // declare (proves the comparison above is pinning something real, not two equally-wrong
    // runs that happen to agree): x(1s) = 0.5*1*1^2 = 0.5, vx(1s) = 1.0 (fault only touches
    // accel.x's *future* effect, not the already-integrated velocity); after the fault
    // accel.x = 5.0; at the maneuver (1.5s, 0.5s after the fault) vy jumps by 5.0.
    let at_fault = traj_a_together.samples.iter().find(|s| s.tai_ns == FAULT_TAI_NS).expect("a sample at the fault epoch");
    assert!((at_fault.mean[0] - 0.5).abs() < 1e-9, "x(1s) = {}", at_fault.mean[0]);
    assert!((at_fault.mean[3] - 1.0).abs() < 1e-9, "vx(1s) = {}", at_fault.mean[3]);
    let at_maneuver = traj_a_together.samples.iter().find(|s| s.tai_ns == MANEUVER_TAI_NS).expect("a sample at the maneuver epoch");
    assert!((at_maneuver.mean[4] - 5.0).abs() < 1e-9, "vy jumps to 5.0 at the post-burn sample = {}", at_maneuver.mean[4]);

    // Instance b's own closed-form check: accel = [0, 2, 0], never faulted or maneuvered.
    let b_last = traj_b_together.samples.last().unwrap();
    assert!((b_last.mean[1] - (0.5 * 2.0 * 2.0f64.powi(2))).abs() < 1e-9, "y(2s) = 0.5*2*2^2 = 4.0, got {}", b_last.mean[1]);
    assert!(b_last.mean[0].abs() < 1e-9, "x must stay exactly 0 (no x acceleration for instance b): {}", b_last.mean[0]);
}

// ==========================================================================================
// 2. A container instance alongside a faulted+maneuvered model instance.
// ==========================================================================================
// Subprocess plumbing mirrors tests/drm_container.rs's own module doc comment (own copy: each
// integration test binary is its own compilation unit).

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}
fn lockstep_ref_dir() -> PathBuf {
    repo_root().join("services").join("lockstep-ref")
}
fn python() -> PathBuf {
    repo_root().join(".venv").join("bin").join("python")
}
fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port");
    listener.local_addr().unwrap().port()
}
struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
const READY_TIMEOUT: Duration = Duration::from_secs(30);
fn spawn_lockstep_ref(port: u16) -> ChildGuard {
    let mut cmd = Command::new(python());
    cmd.args(["-m", "lockstep_ref", "--port", &port.to_string()]);
    cmd.current_dir(lockstep_ref_dir());
    let child = cmd.spawn().unwrap_or_else(|e| panic!("failed to spawn `{} -m lockstep_ref`: {e}", python().display()));
    let guard = ChildGuard(child);
    let address = format!("127.0.0.1:{port}");
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        if av_lockstep::BlockingLockstepClient::connect_plaintext(&address).is_ok() {
            return guard;
        }
        if Instant::now() > deadline {
            panic!("lockstep_ref subprocess on {address} did not become ready within {READY_TIMEOUT:?}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn signal_port(name: &str, direction: PortDirection) -> Port {
    Port { name: name.to_string(), kind: PortKind::Signal as i32, direction: direction as i32, schema: "signal".to_string(), timing: None, interface_class: String::new() }
}
/// A container-bound `SystemDefinition` at 1 Hz (matching this test's own `sample_interval_s`,
/// so the container's own period neither exercises nor is confounded by M14.4's item (a) -- this
/// test is about restart invariance across a boundary, not the zero-order-hold restriction, so
/// it deliberately keeps the container on a 1:1 native/output grid, the same shape `tests/
/// drm_container.rs`'s own fixtures already use).
fn container_system(id: &str, address: &str) -> SystemDefinition {
    hashed_system(SystemDefinition {
        id: id.to_string(),
        dynamics_model: String::new(),
        ports: vec![signal_port("in", PortDirection::In), signal_port("out", PortDirection::Out)],
        state_space_id: "container.none".to_string(),
        state_space: Some(StateSpace { id: "container.none".to_string(), components: vec![], frame_id: String::new() }),
        parameters: vec![sparam("container.address", address), sparam("container.seed_key", "sig_seed")],
        ..Default::default()
    })
}
fn container_instance(name: &str, system_id: &str) -> SystemInstance {
    SystemInstance { name: name.to_string(), system_id: system_id.to_string(), binding: Some(Binding { kind: BindingKind::Container as i32, config: Some(av_cdm::pb::binding::Config::Container(ContainerBinding::default())) }), step_rate_hz: 10.0, ..Default::default() }
}

/// Required test (M14.4's own "Same again for a container instance alongside a faulted model
/// instance"). Identical shape to the model/model test above, except the untouched participant
/// is a real `BINDING_KIND_CONTAINER` instance (`services/lockstep-ref`) instead of a second
/// native model -- proving the same restart-invariance claim across a fault/maneuver boundary
/// that never targets the container holds for the shared-run mechanism's *other* instance kind
/// too, not just for two `BINDING_KIND_MODEL` instances.
#[test]
fn a_container_instance_alongside_a_faulted_model_instance_matches_each_single_instance_run() {
    let _engine = gmat_sys::engine_lock();
    let port = free_port();

    let sys_a = accel_system("restart_c_a_sys", [1.0, 0.0, 0.0]);
    let sys_c = container_system("restart_c_sys", &format!("127.0.0.1:{port}"));
    let seeds = BTreeMap::from([("sig_seed".to_string(), 42u64)]);
    let mut scenario_a = scenario_for("a", true);
    scenario_a.seeds = seeds.clone();
    let mut scenario_c = Scenario { start_tai_ns: 0, end_tai_ns: END_TAI_NS, ..Default::default() };
    scenario_c.seeds = seeds.clone();
    let mut scenario_ac = scenario_for("a", true);
    scenario_ac.seeds = seeds;

    let (sos_a, drm_a) = build_drm("restart_c_a_sos", "restart_c_a_drm", vec![model_instance("a", &sys_a.id, 10.0)], scenario_a);
    let (sos_c, drm_c) = build_drm("restart_c_c_sos", "restart_c_c_drm", vec![container_instance("cont", &sys_c.id)], scenario_c);
    let (sos_ac, drm_ac) = build_drm("restart_c_ac_sos", "restart_c_ac_drm", vec![model_instance("a", &sys_a.id, 10.0), container_instance("cont", &sys_c.id)], scenario_ac);

    let systems_a = one_system_map(&sys_a);
    let systems_c = one_system_map(&sys_c);
    let systems_ac: BTreeMap<String, SystemDefinition> = BTreeMap::from([(sys_a.id.clone(), sys_a.clone()), (sys_c.id.clone(), sys_c.clone())]);

    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");

    let server_a = spawn_lockstep_ref(port);
    let products_a = execute(run_config(&gmat, &drm_a, &sos_a, &systems_a, "test-restart-c-a-alone")).expect("the faulted+maneuvered model instance, run alone, executes");
    drop(server_a);

    let server_c = spawn_lockstep_ref(port);
    let products_c = execute(run_config(&gmat, &drm_c, &sos_c, &systems_c, "test-restart-c-c-alone")).expect("the container instance, run alone, executes");
    drop(server_c);

    let server_ac = spawn_lockstep_ref(port);
    let products_ac = execute(run_config(&gmat, &drm_ac, &sos_ac, &systems_ac, "test-restart-c-ac-together")).expect("the model and container instances together execute");
    drop(server_ac);

    let traj_a_alone = products_a.trajectories.get("a").expect("instance a's own trajectory");
    let traj_a_together = products_ac.trajectories.get("a").expect("instance a's own trajectory in the combined run");
    assert_eq!(traj_a_alone.segments.len(), 3, "fault then maneuver -> three dynamics segments");
    assert_eq!(
        physical_shape(traj_a_alone),
        physical_shape(traj_a_together),
        "the faulted+maneuvered model instance's own physical trajectory must match its single-instance run even with a container instance sharing the kernel run"
    );
    assert_eq!(traj_a_together.segments, traj_a_alone.segments, "the faulted+maneuvered instance's own segments must also match its single-instance run -- got {:?}", traj_a_together.segments);

    let traj_c_alone = products_c.trajectories.get("cont").expect("the container instance's own trajectory");
    let traj_c_together = products_ac.trajectories.get("cont").expect("the container instance's own trajectory in the combined run");
    assert_eq!(
        physical_shape(traj_c_alone),
        physical_shape(traj_c_together),
        "the UNTOUCHED container instance's own physical samples must be byte-identical whether it runs alone or alongside a faulted+maneuvered model instance -- \
         if this fails, report both traj_c_alone.samples and traj_c_together.samples below and diagnose where they first diverge rather than only reporting the failure"
    );
    // A container carries no physical state either way (`mean` always empty) -- the assertion
    // above is really about `samples.len()`/`tai_ns` (did the boundary split leak into the
    // container's own output-tick sequence?) and `entity_id`/`state_space_id`.
    assert!(traj_c_together.samples.iter().all(|s| s.mean.is_empty()));

    // M15.1 (question 115): the same fix as test 1, for a container instance instead of a second
    // model instance -- before the merge, `segments` fragmented from one to three even though the
    // container was never a fault/maneuver boundary's own target and its own `dynamics_hash`
    // never changes; now it merges back to byte-identical with the single-instance run.
    assert_eq!(traj_c_alone.segments.len(), 1, "run alone, the container is never split -> one segment");
    assert_eq!(
        traj_c_together.segments, traj_c_alone.segments,
        "M15.1: the UNTOUCHED container instance's own segments must merge back down to byte-identical with its single-instance run -- got {:?}",
        traj_c_together.segments
    );
}
