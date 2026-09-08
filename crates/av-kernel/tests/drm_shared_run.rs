//! M14.1's required acceptance tests (`docs/open-questions.md` question 109: "the executor runs
//! every instance of a `SosConfiguration` in one shared kernel run (`run_with_ports`), the
//! router delivering between them"):
//!
//! 1. End to end: a native SIGNAL producer instance, a `BINDING_KIND_CONTAINER` instance
//!    (`services/lockstep-ref`) that integrates it, and a native consumer that reads the
//!    container's own output -- all in one DRM -- with an `output.*` expression scoring the
//!    consumer ([`a_native_producer_a_container_and_a_native_consumer_run_end_to_end_in_one_drm`]).
//! 2. Byte-identical `RunProducts` across two runs of that same DRM, against two independently
//!    spawned `services/lockstep-ref` processes ([`byte_identical_run_products_across_two_runs`]).
//! 3. Two instances with no connection between them, run together in one shared kernel run,
//!    produce exactly what each produces run alone as its own single-instance `SosConfiguration`
//!    -- the "nothing existing changes" proof for the multi-instance case
//!    ([`two_unconnected_instances_match_what_each_produces_run_alone`]).
//!
//! The byte-identical *golden* assertions the task brief also requires (the golden DRM, both
//! maneuver DRMs, the fault-split score golden) are not duplicated here -- they are
//! `tests/drm_executor.rs::drm_matches_the_golden_arc`, `tests/drm_maneuver.rs::drm_matches_the_
//! maneuver_golden_{vnb,ric}_burn`, and `tests/expr_goldens.rs::fault_split_accel_scores_match_
//! the_pinned_golden`, all of which already run through `av_kernel::drm::execute`'s new shared
//! kernel run path (every model instance in this crate's other test files that does not request
//! `DrmOptions.covariance` now does) and, unmodified, still pass -- see this crate's own
//! `README.md` for the numbers.
//!
//! Tests 1/2 spawn `services/lockstep-ref` as a local subprocess exactly like
//! `tests/drm_container.rs` does -- see that file's own module doc comment for the subprocess
//! plumbing convention (`ChildGuard`, a real readiness poll, killed on drop) this file repeats
//! rather than shares (each integration test binary is its own compilation unit).

use std::collections::BTreeMap;
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use av_cdm::pb::{
    Binding, BindingKind, Connection, ContainerBinding, DesignReferenceMission, DrmOptions, MeasureOfEffectiveness, ModelBinding, Parameter, Port, PortDirection, PortKind, Scenario,
    SosConfiguration, SystemDefinition, SystemInstance, Trajectory, Unit,
};
use av_kernel::drm::{execute, hash, RunConfig};
use gmat_sys::Gmat;

// ------------------------------------------------------------------------------------------
// Subprocess plumbing (mirrors tests/drm_container.rs -- see that file's own doc comment).
// ------------------------------------------------------------------------------------------

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

// ------------------------------------------------------------------------------------------
// Shared fixture helpers.
// ------------------------------------------------------------------------------------------

fn param(name: &str, value: f64) -> Parameter {
    Parameter { name: name.to_string(), value, ..Default::default() }
}
fn sparam(name: &str, s: &str) -> Parameter {
    Parameter { name: name.to_string(), string_value: s.to_string(), ..Default::default() }
}
fn signal_port(name: &str, direction: PortDirection) -> Port {
    Port { name: name.to_string(), kind: PortKind::Signal as i32, direction: direction as i32, ..Default::default() }
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
fn model_binding(system_id: &str) -> Binding {
    Binding { kind: BindingKind::Model as i32, config: Some(av_cdm::pb::binding::Config::Model(ModelBinding { model_id: system_id.to_string() })) }
}
fn run_config<'a>(gmat: &'a Gmat, drm: &'a DesignReferenceMission, sos: &'a SosConfiguration, systems: &'a BTreeMap<String, SystemDefinition>, run_id: &str) -> RunConfig<'a> {
    RunConfig { gmat, drm, sos, systems, run_id: run_id.to_string(), error_mode: Default::default() , products_dir: None, replay: None }
}

/// A native `ConstantAccelModel`-classified `SystemDefinition` with zero acceleration and a
/// zero initial state -- physics is not what these tests are about, only port delivery -- plus
/// whatever `extra_parameters`/`ports` the caller adds (the `port.emit`/`port.emit_value`/
/// `port.consume` vocabulary, `crates/av-kernel/src/drm/binding.rs`'s own doc comment on
/// `ConstantAccelModel`).
fn native_system(id: &str, ports: Vec<Port>, extra_parameters: Vec<Parameter>) -> SystemDefinition {
    let mut parameters = vec![
        sparam("frame_id", "test.frame"),
        param("state.px", 0.0),
        param("state.py", 0.0),
        param("state.pz", 0.0),
        param("state.vx", 0.0),
        param("state.vy", 0.0),
        param("state.vz", 0.0),
    ];
    parameters.extend(extra_parameters);
    hashed_system(SystemDefinition { id: id.to_string(), dynamics_model: "native.constant_accel".to_string(), state_space_id: "gmat.orbital.cartesian6".to_string(), ports, parameters, ..Default::default() })
}

fn native_instance(name: &str, system_id: &str, step_rate_hz: f64) -> SystemInstance {
    SystemInstance { name: name.to_string(), system_id: system_id.to_string(), binding: Some(model_binding(system_id)), step_rate_hz, ..Default::default() }
}

// ==========================================================================================
// 1/2. A native SIGNAL producer -> a container -> a native consumer, all in one DRM.
// ==========================================================================================

const SCENARIO_DURATION_S: i64 = 3;
/// The constant value the producer emits every step -- see the fixture's own worked-out
/// timeline (below) for why `output.consumer.received@end` must resolve to exactly this.
const EMIT_VALUE: f64 = 3.0;

fn container_system(address: &str) -> SystemDefinition {
    hashed_system(SystemDefinition {
        id: "e2e_container_sys".to_string(),
        dynamics_model: String::new(), // unused by a container binding -- binding.rs's own module doc comment
        ports: vec![signal_port("in", PortDirection::In), signal_port("out", PortDirection::Out)],
        state_space_id: "container.none".to_string(),
        state_space: Some(av_cdm::pb::StateSpace { id: "container.none".to_string(), components: vec![], frame_id: String::new() }),
        parameters: vec![sparam("container.address", address), sparam("container.seed_key", "sig_seed")],
        ..Default::default()
    })
}
fn container_instance(name: &str, system_id: &str) -> SystemInstance {
    SystemInstance {
        name: name.to_string(),
        system_id: system_id.to_string(),
        binding: Some(Binding { kind: BindingKind::Container as i32, config: Some(av_cdm::pb::binding::Config::Container(ContainerBinding::default())) }),
        step_rate_hz: 1.0,
        ..Default::default()
    }
}

/// Builds the three-instance DRM: `producer` (native, emits `EMIT_VALUE` as a SIGNAL on its own
/// `"out"` port every step) -> `container` (`services/lockstep-ref`, sums its `"in"` port and
/// re-emits the running integral on its own `"out"` port, question 107) -> `consumer` (native,
/// records the latest SIGNAL on its own `"in"` port as `StepResult.outputs["received"]`, exposed
/// as `output.consumer.received`).
///
/// **Worked-out timeline** (1 Hz, all three instances, `HeteroScheduler::advance_to_with_ports`
/// steps ties in sorted instance-name order -- `"consumer"` < `"container"` < `"producer"`, so
/// each step at a shared native tick only ever sees what was queued *before* that tick, per
/// `docs/open-questions.md` question 108's "delivered at the receiver's next step" rule):
///
/// | t (s) | consumer sees (in)   | container sees (in) | container emits (out) | producer emits (out) |
/// |-------|----------------------|----------------------|------------------------|------------------------|
/// | 1     | (nothing queued yet) | (nothing queued yet) | 0.0 (integral so far)  | `EMIT_VALUE`           |
/// | 2     | 0.0 (from t=1)       | `EMIT_VALUE` (t=1)   | `EMIT_VALUE` (1 s * v)  | `EMIT_VALUE`           |
/// | 3     | `EMIT_VALUE` (t=2)   | `EMIT_VALUE` (t=2)   | `2*EMIT_VALUE`          | `EMIT_VALUE`           |
///
/// so `consumer`'s own `"received"` output at `t = 3 s` (`scenario.end_tai_ns`, an exact native
/// step -- no interpolation) is exactly `EMIT_VALUE`, asserted below via `output.consumer.
/// received@end`.
fn end_to_end_drm(address: &str, run_seed: u64) -> (BTreeMap<String, SystemDefinition>, SosConfiguration, DesignReferenceMission) {
    let producer_sys = native_system("e2e_producer_sys", vec![signal_port("out", PortDirection::Out)], vec![sparam("port.emit", "out"), param("port.emit_value", EMIT_VALUE)]);
    let consumer_sys = native_system(
        "e2e_consumer_sys",
        vec![signal_port("in", PortDirection::In)],
        vec![sparam("port.consume", "in"), Parameter { name: "output.received".to_string(), unit: Unit::Unspecified as i32, ..Default::default() }],
    );
    let container_sys = container_system(address);

    let sos = hashed_sos(SosConfiguration {
        id: "e2e_sos".to_string(),
        instances: vec![native_instance("producer", &producer_sys.id, 1.0), container_instance("container", &container_sys.id), native_instance("consumer", &consumer_sys.id, 1.0)],
        connections: vec![
            Connection { from_instance: "producer".to_string(), from_port: "out".to_string(), to_instance: "container".to_string(), to_port: "in".to_string(), link_model: String::new() },
            Connection { from_instance: "container".to_string(), from_port: "out".to_string(), to_instance: "consumer".to_string(), to_port: "in".to_string(), link_model: String::new() },
        ],
        ..Default::default()
    });
    let drm = hashed_drm(DesignReferenceMission {
        id: "e2e_drm".to_string(),
        sos_configuration_id: sos.id.clone(),
        scenario: Some(Scenario { start_tai_ns: 0, end_tai_ns: SCENARIO_DURATION_S * 1_000_000_000, seeds: BTreeMap::from([("sig_seed".to_string(), run_seed)]), ..Default::default() }),
        options: Some(DrmOptions { default_step_rate_hz: 1.0, sample_interval_s: 1.0, ..Default::default() }),
        measures: vec![MeasureOfEffectiveness { name: "consumer_received".to_string(), expression: "output.consumer.received@end".to_string(), unit: Unit::Unspecified as i32 }],
        ..Default::default()
    });

    let mut systems = BTreeMap::new();
    systems.insert(producer_sys.id.clone(), producer_sys);
    systems.insert(consumer_sys.id.clone(), consumer_sys);
    systems.insert(container_sys.id.clone(), container_sys);
    (systems, sos, drm)
}

/// Required test: a native SIGNAL producer, a container instance, and a native consumer, all in
/// one DRM, with an `output.*` expression scoring the consumer -- proves the shared kernel run
/// (`av_kernel::drm::executor::run_shared_group`) actually delivers a real message end to end
/// through `crate::router::Router` between three differently-bound instances (native -> container
/// -> native), not merely that each instance runs.
#[test]
fn a_native_producer_a_container_and_a_native_consumer_run_end_to_end_in_one_drm() {
    let _engine = gmat_sys::engine_lock();
    let port = free_port();
    let _server = spawn_lockstep_ref(port);
    let (systems, sos, drm) = end_to_end_drm(&format!("127.0.0.1:{port}"), 42);

    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let products = execute(run_config(&gmat, &drm, &sos, &systems, "test-run-e2e")).expect("the three-instance DRM executes end to end");

    let score = products.scores.get("consumer_received").expect("consumer_received measure evaluated");
    assert_eq!(score.value, EMIT_VALUE, "the consumer's own \"received\" output must be the value the producer emitted, having actually crossed through the container -- see end_to_end_drm's own worked timeline");
    assert_eq!(score.passed, None, "a MeasureOfEffectiveness never has a pass/fail concept");

    // Every instance produced a real trajectory (question 93/95's usual shape), and the run's
    // own provenance records the shared-kernel run mode (M14.1's disclosed-limitation
    // attribute -- see build_run_provenance's own doc comment).
    assert!(products.trajectories.contains_key("producer"));
    assert!(products.trajectories.contains_key("container"));
    assert!(products.trajectories.contains_key("consumer"));
    assert_eq!(products.provenance.attributes.get("kernel_run_mode").map(String::as_str), Some("shared"));
}

/// Required test: byte-identical `RunProducts` across two runs of the same three-instance DRM,
/// against two independently spawned `services/lockstep-ref` processes (mirrors `tests/drm_
/// container.rs::byte_identical_run_products_across_two_runs`, extended to the full
/// producer/container/consumer chain -- proves the shared kernel run's own port delivery is
/// exactly as deterministic as the container's own isolated case already was).
#[test]
fn byte_identical_run_products_across_two_runs() {
    let _engine = gmat_sys::engine_lock();
    let port = free_port();
    let (systems, sos, drm) = end_to_end_drm(&format!("127.0.0.1:{port}"), 7);

    let server_a = spawn_lockstep_ref(port);
    let gmat_a = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let products_a = execute(run_config(&gmat_a, &drm, &sos, &systems, "test-run-e2e-det")).expect("run A executes");
    drop(server_a);

    let server_b = spawn_lockstep_ref(port);
    let gmat_b = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let products_b = execute(run_config(&gmat_b, &drm, &sos, &systems, "test-run-e2e-det")).expect("run B executes");
    drop(server_b);

    assert_eq!(products_a.trajectories, products_b.trajectories, "trajectories must be byte-identical across two independent runs of the same three-instance DRM");
    assert_eq!(products_a.events, products_b.events);
    assert_eq!(products_a.scores, products_b.scores);
    assert_eq!(products_a.provenance, products_b.provenance);
}

// ==========================================================================================
// 3. Two instances with no connection between them match what each produces run alone.
// ==========================================================================================

fn accel_system(id: &str, a: [f64; 3]) -> SystemDefinition {
    native_system(id, vec![], vec![param("accel.x", a[0]), param("accel.y", a[1]), param("accel.z", a[2])])
}

fn one_instance_drm(sos_id: &str, instance_name: &str, sys: &SystemDefinition) -> (SosConfiguration, DesignReferenceMission) {
    let sos = hashed_sos(SosConfiguration { id: sos_id.to_string(), instances: vec![native_instance(instance_name, &sys.id, 10.0)], ..Default::default() });
    let drm = hashed_drm(DesignReferenceMission {
        id: format!("{sos_id}_drm"),
        sos_configuration_id: sos.id.clone(),
        scenario: Some(Scenario { start_tai_ns: 0, end_tai_ns: 2_000_000_000, ..Default::default() }),
        options: Some(DrmOptions { default_step_rate_hz: 10.0, sample_interval_s: 0.1, ..Default::default() }),
        ..Default::default()
    });
    (sos, drm)
}

/// The parts of a `Trajectory` that describe the physics itself -- everything a run-identity
/// field (`config_hash`, `Provenance`, both necessarily different between a single-instance
/// `SosConfiguration` and a two-instance one sharing the same instance) does not touch. Used to
/// compare "this instance's own trajectory" across two DRMs with different hashes but the same
/// declared physics.
fn physical_shape(traj: &Trajectory) -> (String, String, i32, &Vec<av_cdm::pb::TrajectorySample>, &Vec<av_cdm::pb::TrajectorySegment>) {
    (traj.entity_id.clone(), traj.state_space_id.clone(), traj.interpolation, &traj.samples, &traj.segments)
}

/// Required test ("nothing existing changes" for the multi-instance case): two native instances
/// with different closed-form accelerations and *no* `SosConfiguration.connections` between
/// them, run together in one shared kernel run
/// (`av_kernel::drm::executor::run_shared_group`), produce -- per instance -- exactly the same
/// samples/segments a `SosConfiguration` naming that instance *alone* would have produced. This
/// is the direct multi-instance generalization of every existing single-instance golden already
/// passing unchanged (`tests/drm_executor.rs::drm_matches_the_golden_arc` and friends): those
/// prove N=1 is unaffected by M14.1's collapse; this proves N=2 with no wiring between them is
/// exactly N independent N=1 runs, not merely "close" or "similar."
#[test]
fn two_unconnected_instances_match_what_each_produces_run_alone() {
    let _engine = gmat_sys::engine_lock();
    let sys_a = accel_system("shared_run_a_sys", [1.0, 0.0, 0.0]);
    let sys_b = accel_system("shared_run_b_sys", [0.0, 2.0, 0.0]);

    let (sos_a, drm_a) = one_instance_drm("shared_run_a_sos", "a", &sys_a);
    let (sos_b, drm_b) = one_instance_drm("shared_run_b_sos", "b", &sys_b);
    let sos_ab = hashed_sos(SosConfiguration { id: "shared_run_ab_sos".to_string(), instances: vec![native_instance("a", &sys_a.id, 10.0), native_instance("b", &sys_b.id, 10.0)], ..Default::default() });
    let drm_ab = hashed_drm(DesignReferenceMission {
        id: "shared_run_ab_drm".to_string(),
        sos_configuration_id: sos_ab.id.clone(),
        scenario: Some(Scenario { start_tai_ns: 0, end_tai_ns: 2_000_000_000, ..Default::default() }),
        options: Some(DrmOptions { default_step_rate_hz: 10.0, sample_interval_s: 0.1, ..Default::default() }),
        ..Default::default()
    });

    let systems_a: BTreeMap<String, SystemDefinition> = BTreeMap::from([(sys_a.id.clone(), sys_a.clone())]);
    let systems_b: BTreeMap<String, SystemDefinition> = BTreeMap::from([(sys_b.id.clone(), sys_b.clone())]);
    let systems_ab: BTreeMap<String, SystemDefinition> = BTreeMap::from([(sys_a.id.clone(), sys_a.clone()), (sys_b.id.clone(), sys_b.clone())]);

    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let products_a = execute(run_config(&gmat, &drm_a, &sos_a, &systems_a, "test-run-shared-a-alone")).expect("instance a alone executes");
    let products_b = execute(run_config(&gmat, &drm_b, &sos_b, &systems_b, "test-run-shared-b-alone")).expect("instance b alone executes");
    let products_ab = execute(run_config(&gmat, &drm_ab, &sos_ab, &systems_ab, "test-run-shared-ab-together")).expect("both instances together, unconnected, execute");

    assert_eq!(products_ab.trajectories.len(), 2, "both instances produced a trajectory in the combined run");
    let traj_a_alone = products_a.trajectories.get("a").expect("instance a's own trajectory");
    let traj_a_together = products_ab.trajectories.get("a").expect("instance a's own trajectory in the combined run");
    assert_eq!(physical_shape(traj_a_alone), physical_shape(traj_a_together), "instance a's own physical trajectory must be bit-identical whether it runs alone or alongside an unconnected instance b");

    let traj_b_alone = products_b.trajectories.get("b").expect("instance b's own trajectory");
    let traj_b_together = products_ab.trajectories.get("b").expect("instance b's own trajectory in the combined run");
    assert_eq!(physical_shape(traj_b_alone), physical_shape(traj_b_together), "instance b's own physical trajectory must be bit-identical whether it runs alone or alongside an unconnected instance a");

    // Every event each instance produced alone (its own run_start/run_end lifecycle pair) is
    // produced identically in the combined run too -- events are already (epoch, id)-sorted
    // (`super::events::epoch_id_order`), so the combined run's own event list is simply the
    // interleaving of both instances' own event lists.
    assert_eq!(products_a.events.len(), 2);
    assert_eq!(products_ab.events.len(), 4, "{:?}", products_ab.events);
    for e in &products_a.events {
        assert!(products_ab.events.iter().any(|e2| e2.name == e.name && e2.entity_id == e.entity_id && e2.tai_ns == e.tai_ns), "event {e:?} from the alone run must also appear in the combined run");
    }
}
