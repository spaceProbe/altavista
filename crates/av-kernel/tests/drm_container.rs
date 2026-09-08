//! `BINDING_KIND_CONTAINER` acceptance tests (M13.2, `docs/open-questions.md` question 107):
//!
//! 1. Byte-identical `RunProducts` across two runs against two independently spawned
//!    `services/lockstep-ref` processes (`byte_identical_run_products_across_two_runs`).
//! 2. A fixture that lies about `reached_tai_ns` is caught, and the run stops with the typed
//!    protocol error (`a_fixture_that_lies_about_reached_tai_ns_is_caught`).
//! 3. `lockstep_capable = false` refusal (`bind_refusal_when_lockstep_capable_is_false`) and
//!    the port-set-mismatch refusal (`bind_refusal_on_a_port_set_mismatch`).
//! 4. A `sequence` mismatch is caught (`a_sequence_mismatch_is_caught`).
//!
//! Also (not individually required): a plain end-to-end happy path proving `named_outputs`
//! reach a `MeasureOfEffectiveness` and `LockstepBindResponse.binding_hash` reaches
//! `Trajectory.provenance.attributes` (`container_binding_runs_end_to_end_and_records_
//! binding_hash_and_named_outputs`), and a DYNAMICS-fault-on-a-container-instance refusal
//! (`a_dynamics_fault_on_a_container_instance_is_refused`).
//!
//! **M16.2 (`docs/open-questions.md` question 120):** a container power cycle moved from
//! `FAULT_TARGET_KIND_DYNAMICS` to `FAULT_TARGET_KIND_HARDWARE` --
//! `a_power_cycle_fault_on_a_container_instance_resets_the_integrator_and_the_run_continues`
//! (required, moved unchanged in its assertions, only its fixture's `target_kind` changed), the
//! retired DYNAMICS/`"power_cycle"` shape is now a typed load error
//! (`a_dynamics_fault_of_kind_power_cycle_is_refused_as_a_typed_load_error`, required), a
//! HARDWARE fault naming a model instance is a typed refusal, not a silent drop
//! (`a_hardware_fault_on_a_model_instance_is_refused_as_unsupported`, required), and (not
//! individually required, but closing the same silent-drop gap) a HARDWARE fault naming a
//! container with any `kind` other than `"power_cycle"` is refused too
//! (`a_hardware_fault_with_an_unsupported_kind_on_a_container_instance_is_refused`).
//!
//! Every test here spawns `services/lockstep-ref` as a **local subprocess** (`python -m
//! lockstep_ref`, this repository's own `.venv`) -- no Docker, matching the task brief
//! ("Tests spawn it as a local subprocess -- no Docker in CI") -- **except** the two M16.2 typed-
//! refusal tests above whose fault is refused at load, before `binding::materialize_container`
//! would ever dial an address: `a_hardware_fault_on_a_model_instance_is_refused_as_unsupported`
//! spawns none at all (no container instance is even declared), and
//! `a_hardware_fault_with_an_unsupported_kind_on_a_container_instance_is_refused` allocates a
//! port but never dials it. Readiness is awaited by
//! retrying a real `av_lockstep::BlockingLockstepClient::connect_plaintext` (not a bare
//! sleep) until it succeeds or a deadline passes; every spawned child is killed in a `Drop`
//! guard so a failing assertion (an early return via `?`/`panic!`) can never leak a listening
//! subprocess -- see `crates/av-kernel/README.md`'s note about a previous worker leaving a
//! server running.
//!
//! No GMAT install is needed for any test here (`RunConfig.gmat` is still constructed,
//! unconditionally, the same way every other GMAT-free-but-`RunConfig`-shaped test in this
//! crate already does -- `Gmat::setup` is cheap after the first call in a process), so every
//! test still takes `gmat_sys::engine_lock()` first, per this repository's convention.

use std::collections::BTreeMap;
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use av_cdm::pb::{
    Binding, BindingKind, Connection, ContainerBinding, DesignReferenceMission, DrmOptions, EventKind, Fault, FaultTargetKind, MeasureOfEffectiveness, ModelBinding, Parameter, Port,
    PortDirection, PortKind, Scenario, SosConfiguration, StateSpace, SystemDefinition, SystemInstance, Unit,
};
use av_kernel::drm::binding::ContainerError;
use av_kernel::drm::{execute, hash, DrmError, RunConfig};
use av_lockstep::docker::{prune_stale_test_resources, test_label_args, test_run_id};
use gmat_sys::Gmat;

// ------------------------------------------------------------------------------------------
// Subprocess plumbing
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

/// An ephemeral localhost port, free at the moment of the check (bind-then-close; a small,
/// unavoidable race, adequate for a test fixture -- the same trick `tests/test_gmat_service.py`
/// uses on the Python side).
fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port");
    listener.local_addr().unwrap().port()
}

/// Kills the spawned `lockstep_ref` subprocess on drop, so any test failure (an early `?`
/// propagation or a panicking assertion) can never leak a listening server -- this repository's
/// own environment note: "Kill any subprocess you spawn before reporting."
struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

const READY_TIMEOUT: Duration = Duration::from_secs(30);

/// Spawn `python -m lockstep_ref --port <port>` with `extra_env` applied on top of this
/// process's own environment, and block until it accepts a real plaintext lockstep connection
/// (a genuine readiness poll via `av_lockstep::BlockingLockstepClient::connect_plaintext`, not
/// a bare `sleep`) or `READY_TIMEOUT` passes.
fn spawn_lockstep_ref(port: u16, extra_env: &[(&str, &str)]) -> ChildGuard {
    let mut cmd = Command::new(python());
    cmd.args(["-m", "lockstep_ref", "--port", &port.to_string()]);
    cmd.current_dir(lockstep_ref_dir());
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
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
// Fixture: one container-bound "sig" instance, three 1 Hz steps over a 3 s scenario.
// ------------------------------------------------------------------------------------------

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

fn signal_port(name: &str, direction: PortDirection) -> Port {
    Port { name: name.to_string(), kind: PortKind::Signal as i32, direction: direction as i32, schema: "signal".to_string(), timing: None, interface_class: String::new() }
}

/// `in_port_name` lets a caller deliberately mismatch what `lockstep-ref` expects (default
/// `"in"`) -- see [`bind_refusal_on_a_port_set_mismatch`].
fn container_system(id: &str, in_port_name: &str, extra_params: Vec<Parameter>) -> SystemDefinition {
    let mut parameters = vec![sparam("container.address", "PLACEHOLDER"), sparam("container.seed_key", "sig_seed"), Parameter { name: "output.integral".to_string(), unit: Unit::Unspecified as i32, ..Default::default() }];
    parameters.extend(extra_params);
    hashed_system(SystemDefinition {
        id: id.to_string(),
        dynamics_model: String::new(), // unused by a container binding -- see binding.rs's module doc comment
        ports: vec![signal_port(in_port_name, PortDirection::In), signal_port("out", PortDirection::Out)],
        state_space_id: "container.none".to_string(),
        state_space: Some(StateSpace { id: "container.none".to_string(), components: vec![], frame_id: String::new() }),
        parameters,
        ..Default::default()
    })
}

fn container_system_with_address(id: &str, address: &str, in_port_name: &str) -> SystemDefinition {
    let mut sys = container_system(id, in_port_name, vec![]);
    sys.parameters = sys.parameters.into_iter().map(|p| if p.name == "container.address" { sparam("container.address", address) } else { p }).collect();
    hashed_system(sys)
}

fn container_instance(name: &str, system_id: &str) -> SystemInstance {
    SystemInstance {
        name: name.to_string(),
        system_id: system_id.to_string(),
        binding: Some(Binding { kind: BindingKind::Container as i32, config: Some(av_cdm::pb::binding::Config::Container(ContainerBinding::default())) }),
        step_rate_hz: 1.0, // 1 Hz -> period_ns = 1_000_000_000
        ..Default::default()
    }
}

const SCENARIO_DURATION_S: i64 = 3;

fn container_drm(sos_id: &str, instance_name: &str, sys: SystemDefinition, seeds: BTreeMap<String, u64>) -> (SystemDefinition, SosConfiguration, DesignReferenceMission) {
    let sos = hashed_sos(SosConfiguration { id: sos_id.to_string(), instances: vec![container_instance(instance_name, &sys.id)], ..Default::default() });
    let drm = hashed_drm(DesignReferenceMission {
        id: format!("{sos_id}_drm"),
        sos_configuration_id: sos_id.to_string(),
        scenario: Some(Scenario { start_tai_ns: 0, end_tai_ns: SCENARIO_DURATION_S * 1_000_000_000, seeds, ..Default::default() }),
        options: Some(DrmOptions { default_step_rate_hz: 1.0, sample_interval_s: 1.0, ..Default::default() }),
        measures: vec![MeasureOfEffectiveness { name: "final_integral".to_string(), expression: format!("output.{instance_name}.integral@end"), unit: Unit::Unspecified as i32 }],
        ..Default::default()
    });
    (sys, sos, drm)
}

fn run_config<'a>(gmat: &'a Gmat, drm: &'a DesignReferenceMission, sos: &'a SosConfiguration, systems: &'a BTreeMap<String, SystemDefinition>, run_id: &str) -> RunConfig<'a> {
    RunConfig { gmat, drm, sos, systems, run_id: run_id.to_string(), error_mode: Default::default() , products_dir: None }
}

fn one_system_map(sys: &SystemDefinition) -> BTreeMap<String, SystemDefinition> {
    BTreeMap::from([(sys.id.clone(), sys.clone())])
}

// ------------------------------------------------------------------------------------------
// 1. Happy path: named outputs and binding_hash reach RunProducts/Provenance.
// ------------------------------------------------------------------------------------------

#[test]
fn container_binding_runs_end_to_end_and_records_binding_hash_and_named_outputs() {
    let _engine = gmat_sys::engine_lock();
    let port = free_port();
    let _server = spawn_lockstep_ref(port, &[]);

    let sys = container_system_with_address("sig_sys", &format!("127.0.0.1:{port}"), "in");
    let (sys, sos, drm) = container_drm("sig_sos", "sig", sys, BTreeMap::from([("sig_seed".to_string(), 42)]));
    let systems = one_system_map(&sys);

    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let products = execute(run_config(&gmat, &drm, &sos, &systems, "test-run-container-happy")).expect("container DRM executes end to end");

    let traj = products.trajectories.get("sig").expect("the \"sig\" instance produced a trajectory");
    // 3 steps at 1 Hz over a 3 s scenario -> 4 samples: t=0,1,2,3 s.
    assert_eq!(traj.samples.len(), 4, "{:?}", traj.samples.iter().map(|s| s.tai_ns).collect::<Vec<_>>());
    assert!(traj.samples.iter().all(|s| s.mean.is_empty()), "a container-bound instance carries no physical state in this batch");
    assert_eq!(traj.segments.len(), 1);
    assert_eq!(traj.segments[0].dynamics_depth, "container-lockstep");

    // No SIGNAL input was ever sent (no cross-instance routing is wired in this batch -- see
    // binding.rs's own module doc comment), so lockstep-ref's own integral stays exactly 0.0
    // -- this is still a real, meaningful end-to-end value: the named output really did flow
    // LockstepStepResponse.named_outputs -> StepResult.outputs -> NamedOutputSeries ->
    // ExprRunProducts -> the evaluated MeasureOfEffectiveness.
    let score = products.scores.get("final_integral").expect("final_integral measure evaluated");
    assert_eq!(score.value, 0.0);
    assert_eq!(score.passed, None, "a MeasureOfEffectiveness never has a pass/fail concept");

    // Question 107's "binding_hash into provenance".
    let prov = traj.provenance.as_ref().expect("finish_trajectory always attaches a Provenance");
    let recorded_hash = prov.attributes.get("container_binding_hash").expect("container_binding_hash attribute present");
    assert_eq!(recorded_hash.len(), 64, "lockstep-ref's own binding_hash is a hex-encoded SHA-256");

    // Lifecycle events only (no faults/maneuvers declared).
    assert_eq!(products.events.len(), 2, "{:?}", products.events);
    assert_eq!(products.events[0].name, "run_start");
    assert_eq!(products.events[1].name, "run_end");
}

// ------------------------------------------------------------------------------------------
// 2. Byte-identical RunProducts across two independent runs.
// ------------------------------------------------------------------------------------------

#[test]
fn byte_identical_run_products_across_two_runs() {
    let _engine = gmat_sys::engine_lock();

    // The identical DRM/SosConfiguration/SystemDefinition (down to `container.address`: both
    // runs use the same port, so the artifacts genuinely hash identically -- not merely
    // "equivalent" configs), run twice against two *independently started* lockstep-ref
    // processes (the second spawned only after the first is fully shut down, so they never
    // share a port). What this test pins is that two separate process instances, given the
    // identical ordered Bind/Step sequence, produce byte-identical `RunProducts` -- the
    // determinism the task brief's "Required tests" section names first.
    let port = free_port();
    let sys = container_system_with_address("det_sys", &format!("127.0.0.1:{port}"), "in");
    let (sys, sos, drm) = container_drm("det_sos", "sig", sys, BTreeMap::from([("sig_seed".to_string(), 42)]));
    let systems = one_system_map(&sys);

    let server_a = spawn_lockstep_ref(port, &[]);
    let gmat_a = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let products_a = execute(run_config(&gmat_a, &drm, &sos, &systems, "test-run-det")).expect("run A executes");
    drop(server_a);

    let server_b = spawn_lockstep_ref(port, &[]);
    let gmat_b = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let products_b = execute(run_config(&gmat_b, &drm, &sos, &systems, "test-run-det")).expect("run B executes");
    drop(server_b);

    assert_eq!(products_a.trajectories, products_b.trajectories, "trajectories must be byte-identical across two independent lockstep-ref processes given the identical Bind/Step sequence");
    assert_eq!(products_a.events, products_b.events);
    assert_eq!(products_a.scores, products_b.scores);
    assert_eq!(products_a.provenance, products_b.provenance);
}

// ------------------------------------------------------------------------------------------
// 3. lockstep_capable = false refusal, and the port-set-mismatch refusal.
// ------------------------------------------------------------------------------------------

#[test]
fn bind_refusal_when_lockstep_capable_is_false() {
    let _engine = gmat_sys::engine_lock();
    let port = free_port();
    let _server = spawn_lockstep_ref(port, &[("LOCKSTEP_REF_REFUSE", "1")]);

    let sys = container_system_with_address("refuse_sys", &format!("127.0.0.1:{port}"), "in");
    let (sys, sos, drm) = container_drm("refuse_sos", "sig", sys, BTreeMap::from([("sig_seed".to_string(), 42)]));
    let systems = one_system_map(&sys);

    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let err = execute(run_config(&gmat, &drm, &sos, &systems, "test-run-refuse")).unwrap_err();
    match err {
        DrmError::ContainerRefused { instance, reason } => {
            assert_eq!(instance, "sig");
            assert!(reason.contains("refused for testing"), "{reason:?}");
        }
        other => panic!("expected DrmError::ContainerRefused, got {other:?}"),
    }
}

#[test]
fn bind_refusal_on_a_port_set_mismatch() {
    let _engine = gmat_sys::engine_lock();
    let port = free_port();
    // A normally-configured server (still expects ports named "in"/"out")...
    let _server = spawn_lockstep_ref(port, &[]);

    // ...but this SystemDefinition declares its input port as "wrong_name" instead -- Bind's
    // own port-set validation (lockstep_ref/server.py) must refuse it.
    let sys = container_system_with_address("mismatch_sys", &format!("127.0.0.1:{port}"), "wrong_name");
    let (sys, sos, drm) = container_drm("mismatch_sos", "sig", sys, BTreeMap::from([("sig_seed".to_string(), 42)]));
    let systems = one_system_map(&sys);

    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let err = execute(run_config(&gmat, &drm, &sos, &systems, "test-run-mismatch")).unwrap_err();
    match err {
        DrmError::ContainerRefused { instance, reason } => {
            assert_eq!(instance, "sig");
            assert!(reason.to_lowercase().contains("port"), "expected a port-set-mismatch reason, got {reason:?}");
        }
        other => panic!("expected DrmError::ContainerRefused, got {other:?}"),
    }
}

// ------------------------------------------------------------------------------------------
// 4. A fixture that lies about reached_tai_ns is caught, and a sequence mismatch is caught.
// ------------------------------------------------------------------------------------------

#[test]
fn a_fixture_that_lies_about_reached_tai_ns_is_caught() {
    let _engine = gmat_sys::engine_lock();
    let port = free_port();
    // Lies on the 2nd Step call (of 3): reports until_tai_ns + 1 instead of until_tai_ns.
    let _server = spawn_lockstep_ref(port, &[("LOCKSTEP_REF_LIE_REACHED_AT_STEP", "2")]);

    let sys = container_system_with_address("lie_reached_sys", &format!("127.0.0.1:{port}"), "in");
    let (sys, sos, drm) = container_drm("lie_reached_sos", "sig", sys, BTreeMap::from([("sig_seed".to_string(), 42)]));
    let systems = one_system_map(&sys);

    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let err = execute(run_config(&gmat, &drm, &sos, &systems, "test-run-lie-reached")).unwrap_err();
    match err {
        DrmError::ContainerProtocol { instance, source: ContainerError::ReachedTaiMismatch { expected, got } } => {
            assert_eq!(instance, "sig");
            assert_eq!(got, expected + 1, "the fixture lies by exactly +1 ns");
        }
        other => panic!("expected DrmError::ContainerProtocol(ReachedTaiMismatch), got {other:?}"),
    }
}

#[test]
fn a_sequence_mismatch_is_caught() {
    let _engine = gmat_sys::engine_lock();
    let port = free_port();
    // Lies on the 2nd Step call (of 3): echoes sequence + 1 instead of the sent sequence.
    let _server = spawn_lockstep_ref(port, &[("LOCKSTEP_REF_LIE_SEQUENCE_AT_STEP", "2")]);

    let sys = container_system_with_address("lie_seq_sys", &format!("127.0.0.1:{port}"), "in");
    let (sys, sos, drm) = container_drm("lie_seq_sos", "sig", sys, BTreeMap::from([("sig_seed".to_string(), 42)]));
    let systems = one_system_map(&sys);

    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let err = execute(run_config(&gmat, &drm, &sos, &systems, "test-run-lie-sequence")).unwrap_err();
    match err {
        DrmError::ContainerProtocol { instance, source: ContainerError::SequenceMismatch { expected, got } } => {
            assert_eq!(instance, "sig");
            assert_eq!(got, expected + 1, "the fixture lies by exactly +1");
        }
        other => panic!("expected DrmError::ContainerProtocol(SequenceMismatch), got {other:?}"),
    }
}

// ------------------------------------------------------------------------------------------
// Also: a DYNAMICS fault naming a container-bound instance is refused, never silently ignored.
// ------------------------------------------------------------------------------------------

#[test]
fn a_dynamics_fault_on_a_container_instance_is_refused() {
    let _engine = gmat_sys::engine_lock();
    let port = free_port();
    let _server = spawn_lockstep_ref(port, &[]);

    let sys = container_system_with_address("fault_sys", &format!("127.0.0.1:{port}"), "in");
    let (sys, sos, mut drm) = container_drm("fault_sos", "sig", sys, BTreeMap::from([("sig_seed".to_string(), 42)]));
    drm.scenario.as_mut().unwrap().faults = vec![av_cdm::pb::Fault {
        id: "f1".to_string(),
        instance: "sig".to_string(),
        target_kind: av_cdm::pb::FaultTargetKind::Dynamics as i32,
        tai_ns: 1_000_000_000,
        kind: "parameter".to_string(),
        ..Default::default()
    }];
    drm.hash = hash::canonical_drm_hash(&drm);
    let systems = one_system_map(&sys);

    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let err = execute(run_config(&gmat, &drm, &sos, &systems, "test-run-container-fault")).unwrap_err();
    assert!(matches!(err, DrmError::ContainerFaultsOrManeuversNotSupported { ref instance } if instance == "sig"), "{err:?}");
}

// ------------------------------------------------------------------------------------------
// 5. M14.4: a container period coarser than the sample interval is zero-order held, not
//    Hermite-interpolated and not refused (`DrmError::ContainerPeriodExceedsSampleInterval`
//    lifted).
// ------------------------------------------------------------------------------------------

fn container_instance_with_rate(name: &str, system_id: &str, step_rate_hz: f64) -> SystemInstance {
    SystemInstance {
        name: name.to_string(),
        system_id: system_id.to_string(),
        binding: Some(Binding { kind: BindingKind::Container as i32, config: Some(av_cdm::pb::binding::Config::Container(ContainerBinding::default())) }),
        step_rate_hz,
        ..Default::default()
    }
}

/// M14.4 required test: a 300 ms container step period under a 100 ms sample interval
/// (`output_period_ns` does **not** evenly divide `period_ns` -- exactly the shape the old
/// `DrmError::ContainerPeriodExceedsSampleInterval` restriction refused) now succeeds end to
/// end, and every output tick strictly between two of the container's own native steps is a
/// recorded zero-order hold (ADR-005 sec 3), never Hermite interpolation and never silently
/// indistinguishable from a fresh native sample: `held`, `held`, `fresh` at t = 100, 200, 300 ms
/// (and the same pattern repeating through 900 ms).
///
/// **What this test would fail against:**
/// 1. The old restriction still in place -- `execute()` returns `Err(DrmError::
///    ContainerPeriodExceedsSampleInterval { .. })` instead of `Ok(RunProducts)`, and the
///    `.expect(...)` below panics immediately.
/// 2. The restriction lifted but `HeteroKernel::run_with_ports` left calling plain
///    `HeteroScheduler::sample` unconditionally for a container -- the first output tick
///    strictly between two native steps (100 ms) reaches `crate::interpolate::hermite_velocity`
///    with a 0-length pair, which panics (`assert!(s0.len() >= 6)`), so this test would abort
///    rather than complete.
/// 3. An implementation that computes the right *trajectory* (trivially easy, since a
///    container's own `mean` is always empty either way) but never records which ticks were
///    zero-order held on `TrajectorySample.kind` (question 116) -- every sample would read back
///    `SampleKind::Unspecified`/`Native` regardless of which output ticks were actually held, and
///    the `held`/`want_held` equality assertion below would fail; a held sample would then be
///    silently indistinguishable from a fresh one, exactly what this task's brief forbids.
#[test]
fn a_container_period_coarser_than_the_sample_interval_is_zero_order_held_not_interpolated() {
    let _engine = gmat_sys::engine_lock();
    let port = free_port();
    let _server = spawn_lockstep_ref(port, &[]);

    let sys = container_system_with_address("hold_sys", &format!("127.0.0.1:{port}"), "in");
    // 1e9 / 3e8 Hz round-trips back to exactly a 300 ms period_ns
    // (`executor::execute`'s `(1e9 / step_rate_hz).round() as i64`).
    let step_rate_hz = 1_000_000_000.0 / 300_000_000.0;
    let instance = container_instance_with_rate("sig", &sys.id, step_rate_hz);
    let sos = hashed_sos(SosConfiguration { id: "hold_sos".to_string(), instances: vec![instance], ..Default::default() });
    let drm = hashed_drm(DesignReferenceMission {
        id: "hold_drm".to_string(),
        sos_configuration_id: sos.id.clone(),
        scenario: Some(Scenario { start_tai_ns: 0, end_tai_ns: 900_000_000, seeds: BTreeMap::from([("sig_seed".to_string(), 42)]), ..Default::default() }),
        options: Some(DrmOptions { default_step_rate_hz: step_rate_hz, sample_interval_s: 0.1, ..Default::default() }),
        ..Default::default()
    });
    let systems = one_system_map(&sys);

    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let products =
        execute(run_config(&gmat, &drm, &sos, &systems, "test-run-container-hold")).expect("a container period coarser than the sample interval must now succeed, not be refused");

    let traj = products.trajectories.get("sig").expect("the \"sig\" instance produced a trajectory");
    // 100 ms ticks over a 900 ms scenario -> 10 samples: 0, 100, .., 900.
    assert_eq!(traj.samples.len(), 10, "{:?}", traj.samples.iter().map(|s| s.tai_ns).collect::<Vec<_>>());
    assert!(traj.samples.iter().all(|s| s.mean.is_empty()), "a container-bound instance still carries no physical state, held or fresh");

    // M15.2 (question 116): held/fresh now rides `TrajectorySample.kind` directly, not the old
    // `held_sample_tai_ns` provenance attribute (deleted).
    let held: std::collections::BTreeSet<i64> =
        traj.samples.iter().filter(|s| s.kind == av_cdm::pb::SampleKind::Held as i32).map(|s| s.tai_ns).collect();
    let want_held: std::collections::BTreeSet<i64> = [100_000_000, 200_000_000, 400_000_000, 500_000_000, 700_000_000, 800_000_000].into_iter().collect();
    assert_eq!(held, want_held, "exactly the ticks strictly between two of the container's own 300 ms native steps must be recorded held: held, held, fresh, repeating");
    // The container's own real native steps (0, 300, 600, 900 ms) must be NATIVE, never HELD.
    for fresh_tai_ns in [0_i64, 300_000_000, 600_000_000, 900_000_000] {
        let s = traj.samples.iter().find(|s| s.tai_ns == fresh_tai_ns).expect("recorded");
        assert_eq!(
            s.kind,
            av_cdm::pb::SampleKind::Native as i32,
            "{fresh_tai_ns} ns is one of the container's own real native steps, must be recorded NATIVE, not HELD"
        );
    }
}

// ------------------------------------------------------------------------------------------
// 6. M15.3 (docs/open-questions.md question 118), moved from FAULT_TARGET_KIND_DYNAMICS to
//    FAULT_TARGET_KIND_HARDWARE by M16.2 (question 120): a power-cycle fault on a container
//    instance calls Reset at the fault epoch, the reference process's own integrator resets,
//    the run continues, and a FAULT event is emitted -- required test, end to end. Assertions
//    are unchanged from M15.3; only the fixture's `target_kind` changed.
// ------------------------------------------------------------------------------------------

fn model_binding(system_id: &str) -> Binding {
    Binding { kind: BindingKind::Model as i32, config: Some(av_cdm::pb::binding::Config::Model(ModelBinding { model_id: system_id.to_string() })) }
}

/// A native `ConstantAccelModel`-classified `SystemDefinition` (zero acceleration, zero initial
/// state -- physics is not what this test is about) that emits a constant SIGNAL on its own
/// `"out"` port every step (`"port.emit"`/`"port.emit_value"`, M14.1 question 109) -- mirrors
/// `tests/drm_shared_run.rs`'s own `native_system` helper (each integration test binary is its
/// own compilation unit, so this crate's convention is to repeat rather than share fixtures).
fn emitter_system(id: &str, emit_value: f64) -> SystemDefinition {
    hashed_system(SystemDefinition {
        id: id.to_string(),
        dynamics_model: "native.constant_accel".to_string(),
        state_space_id: "gmat.orbital.cartesian6".to_string(),
        ports: vec![signal_port("out", PortDirection::Out)],
        parameters: vec![
            sparam("frame_id", "test.frame"),
            Parameter { name: "state.px".to_string(), value: 0.0, ..Default::default() },
            Parameter { name: "state.py".to_string(), value: 0.0, ..Default::default() },
            Parameter { name: "state.pz".to_string(), value: 0.0, ..Default::default() },
            Parameter { name: "state.vx".to_string(), value: 0.0, ..Default::default() },
            Parameter { name: "state.vy".to_string(), value: 0.0, ..Default::default() },
            Parameter { name: "state.vz".to_string(), value: 0.0, ..Default::default() },
            sparam("port.emit", "out"),
            Parameter { name: "port.emit_value".to_string(), value: emit_value, ..Default::default() },
        ],
        ..Default::default()
    })
}
fn emitter_instance(name: &str, system_id: &str) -> SystemInstance {
    SystemInstance { name: name.to_string(), system_id: system_id.to_string(), binding: Some(model_binding(system_id)), step_rate_hz: 1.0, ..Default::default() }
}

/// **What a wrong implementation fails this test against.**
///
/// This is deliberately *not* a test that would also pass against an executor that silently
/// dropped the fault: an implementation that never calls `Reset` at all (or calls it but the
/// reference process's own handler did not really zero the integral, or the fault were simply
/// ignored) would still let `"sig"`'s own integral accumulate *monotonically* for the whole 5 s
/// run, landing on 20.0 at `t = end`, not 10.0 -- see the worked timeline below. An
/// implementation that refuses the fault outright (the pre-M15.3 behaviour,
/// `DrmError::ContainerFaultsOrManeuversNotSupported`) fails the `.expect("...")` immediately,
/// before any of that. An implementation that calls `Reset` but never emits a FAULT event fails
/// the `events` assertion. **M16.2 (question 120) adds one more wrong implementation this same
/// test now catches**: one that left `fault::is_container_power_cycle` keyed on
/// `FAULT_TARGET_KIND_DYNAMICS` (the M15.3 shape) instead of moving it to `_HARDWARE` -- since
/// this fixture's fault is `FAULT_TARGET_KIND_HARDWARE` now, that implementation would match
/// neither of `run_shared_group`'s two boundary-collection arms, silently drop the fault, and
/// produce the same wrong monotonic 20.0 the "never calls Reset" case above does. Any of these
/// failure modes is exactly what this test is required to catch (`docs/open-questions.md`
/// question 118's own "Reset end to end" required test, carried over unchanged by question 120).
///
/// **Worked timeline** (1 Hz, `emitter` -> `sig`, `HeteroScheduler::advance_to_with_ports`
/// delivers a message at the receiver's *next* step, per question 108's own rule -- the same
/// one-step delay `tests/drm_shared_run.rs`'s own worked timeline documents): with
/// `EMIT_VALUE = 5.0`, absent any fault `sig`'s own `integral` at t = 1..5 s would be
/// `0, 5, 10, 15, 20` (each step adds the *previous* step's own delivered emission times a 1 s
/// dt). The `f1` fault fires at exactly `t = 3 s`, immediately after the span `[0, 3]` completes
/// (its own integral there, 10.0, is unaffected -- `Reset` fires *after* that span, not
/// retroactively) and before the span `[3, 5]` starts: `sig`'s own integral is zeroed, but the
/// router's already-queued message (emitted by `emitter` at `t = 3 s`) is untouched by a
/// *container's own* reset, so the step from `t = 3` to `t = 4` still delivers it, landing the
/// post-reset integral at `5.0` (not `15.0`), and the step to `t = 5` adds one more `5.0`,
/// landing at exactly `10.0` (not `20.0`) at `t = end`.
#[test]
fn a_power_cycle_fault_on_a_container_instance_resets_the_integrator_and_the_run_continues() {
    let _engine = gmat_sys::engine_lock();
    let port = free_port();
    let _server = spawn_lockstep_ref(port, &[]);

    const EMIT_VALUE: f64 = 5.0;
    const DURATION_S: i64 = 5;
    const FAULT_TAI_NS: i64 = 3_000_000_000;

    let emitter_sys = emitter_system("pc_emitter_sys", EMIT_VALUE);
    let container_sys = container_system_with_address("pc_container_sys", &format!("127.0.0.1:{port}"), "in");

    let sos = hashed_sos(SosConfiguration {
        id: "pc_sos".to_string(),
        instances: vec![emitter_instance("emitter", &emitter_sys.id), container_instance("sig", &container_sys.id)],
        connections: vec![Connection { from_instance: "emitter".to_string(), from_port: "out".to_string(), to_instance: "sig".to_string(), to_port: "in".to_string(), link_model: String::new() }],
        ..Default::default()
    });
    let mut drm = hashed_drm(DesignReferenceMission {
        id: "pc_drm".to_string(),
        sos_configuration_id: sos.id.clone(),
        scenario: Some(Scenario { start_tai_ns: 0, end_tai_ns: DURATION_S * 1_000_000_000, seeds: BTreeMap::from([("sig_seed".to_string(), 42)]), ..Default::default() }),
        options: Some(DrmOptions { default_step_rate_hz: 1.0, sample_interval_s: 1.0, ..Default::default() }),
        measures: vec![MeasureOfEffectiveness { name: "final_integral".to_string(), expression: "output.sig.integral@end".to_string(), unit: Unit::Unspecified as i32 }],
        ..Default::default()
    });
    // M16.2 (question 120): the one fixture change -- FAULT_TARGET_KIND_HARDWARE, not
    // FAULT_TARGET_KIND_DYNAMICS. Every assertion below this fixture is unchanged from M15.3.
    drm.scenario.as_mut().unwrap().faults = vec![Fault {
        id: "f1".to_string(),
        instance: "sig".to_string(),
        target_kind: FaultTargetKind::Hardware as i32,
        tai_ns: FAULT_TAI_NS,
        kind: "power_cycle".to_string(),
        ..Default::default()
    }];
    drm.hash = hash::canonical_drm_hash(&drm);

    let mut systems = BTreeMap::new();
    systems.insert(emitter_sys.id.clone(), emitter_sys);
    systems.insert(container_sys.id.clone(), container_sys);

    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let products = execute(run_config(&gmat, &drm, &sos, &systems, "test-run-power-cycle")).expect("a power-cycle fault on a container instance must now succeed, not be refused");

    // The observable state a real Reset produces: the integrator's own accumulated value
    // dropped back, not the monotonically-growing 20.0 an executor that never called Reset (or
    // never really zeroed anything) would have produced -- see this test's own doc comment.
    let score = products.scores.get("final_integral").expect("final_integral measure evaluated");
    assert!((score.value - 10.0).abs() < 1e-9, "expected the post-reset integral 10.0 (see this test's own worked timeline), got {}", score.value);

    // A FAULT event was actually emitted for f1, naming the container instance it targeted --
    // an implementation that calls Reset but never records the event fails this.
    let fault_events: Vec<_> = products.events.iter().filter(|e| e.kind == EventKind::Fault as i32).collect();
    assert_eq!(fault_events.len(), 1, "{:?}", products.events);
    assert_eq!(fault_events[0].name, "f1");
    assert_eq!(fault_events[0].entity_id, "sig");
    assert_eq!(fault_events[0].tai_ns, FAULT_TAI_NS);

    // The run really did continue past the fault: samples cover the whole 5 s scenario, not
    // just the pre-fault span.
    let traj = products.trajectories.get("sig").expect("the \"sig\" instance produced a trajectory");
    assert_eq!(traj.samples.len(), (DURATION_S + 1) as usize, "{:?}", traj.samples.iter().map(|s| s.tai_ns).collect::<Vec<_>>());
}

// ------------------------------------------------------------------------------------------
// 6b. M16.2 (docs/open-questions.md question 120): the M15.3-era interim shape -- a
//     FAULT_TARGET_KIND_DYNAMICS fault of kind == "power_cycle" -- is now a typed load error,
//     not a silently-accepted alternative to FAULT_TARGET_KIND_HARDWARE. Required test.
// ------------------------------------------------------------------------------------------

/// **What a wrong implementation fails this test against.** Against the pre-M16.2 (M15.3)
/// implementation, this exact fixture is the *positive* case: `fault::is_container_power_cycle`
/// matched `FAULT_TARGET_KIND_DYNAMICS`/`"power_cycle"` naming a container instance, so
/// `execute()` would run this to completion and return `Ok(RunProducts)` -- the
/// `.unwrap_err()` below would panic. An implementation that moved `is_container_power_cycle`
/// to HARDWARE (closing that gap) but added no dedicated check for the retired DYNAMICS shape
/// would instead treat this fault as an ordinary DYNAMICS fault naming a container instance and
/// return `Err(DrmError::ContainerFaultsOrManeuversNotSupported)` -- a real refusal, but the
/// wrong *named* one; the `matches!` below is specifically `PowerCycleFaultMustTargetHardware`,
/// so that implementation still fails this test, which is the point: the interim shape gets its
/// own explicit, nameable refusal rather than folding into the generic container-fault message.
#[test]
fn a_dynamics_fault_of_kind_power_cycle_is_refused_as_a_typed_load_error() {
    let _engine = gmat_sys::engine_lock();
    let port = free_port();
    let _server = spawn_lockstep_ref(port, &[]);

    let sys = container_system_with_address("legacy_pc_sys", &format!("127.0.0.1:{port}"), "in");
    let (sys, sos, mut drm) = container_drm("legacy_pc_sos", "sig", sys, BTreeMap::from([("sig_seed".to_string(), 42)]));
    drm.scenario.as_mut().unwrap().faults = vec![Fault {
        id: "f1".to_string(),
        instance: "sig".to_string(),
        target_kind: FaultTargetKind::Dynamics as i32,
        tai_ns: 1_000_000_000,
        kind: "power_cycle".to_string(),
        ..Default::default()
    }];
    drm.hash = hash::canonical_drm_hash(&drm);
    let systems = one_system_map(&sys);

    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let err = execute(run_config(&gmat, &drm, &sos, &systems, "test-run-legacy-power-cycle")).unwrap_err();
    assert!(
        matches!(err, DrmError::PowerCycleFaultMustTargetHardware { ref fault_id, ref instance } if fault_id == "f1" && instance == "sig"),
        "{err:?}"
    );
}

// ------------------------------------------------------------------------------------------
// 6c. M16.2 (docs/open-questions.md question 120): a FAULT_TARGET_KIND_HARDWARE fault naming a
//     BINDING_KIND_MODEL instance has no meaning yet (only a container's own power cycle does)
//     -- handled deliberately as a typed refusal, not silently dropped. Required test.
// ------------------------------------------------------------------------------------------

/// **What a wrong implementation fails this test against.** `run_shared_group`'s own
/// boundary-collection loop has exactly two arms: `target_kind == Dynamics` naming a
/// `BINDING_KIND_MODEL` instance, or `fault::is_container_power_cycle` naming a
/// `BINDING_KIND_CONTAINER` instance. A `FAULT_TARGET_KIND_HARDWARE` fault naming a *model*
/// instance matches neither arm -- an implementation that never added this load-time check
/// would let `execute()` return `Ok(RunProducts)` with no error and no FAULT event at all, the
/// fault simply vanishing (the `.unwrap_err()` below would panic against that implementation).
/// This mirrors exactly the "silently dropped" failure mode this crate's fault handling
/// elsewhere always refuses explicitly (see `fault`'s own module doc comment's PORT/SENSOR
/// "Integration note" for the one remaining case that still isn't).
#[test]
fn a_hardware_fault_on_a_model_instance_is_refused_as_unsupported() {
    let _engine = gmat_sys::engine_lock();

    let sys = emitter_system("hw_model_sys", 1.0);
    let sos = hashed_sos(SosConfiguration { id: "hw_model_sos".to_string(), instances: vec![emitter_instance("veh", &sys.id)], ..Default::default() });
    let mut drm = hashed_drm(DesignReferenceMission {
        id: "hw_model_drm".to_string(),
        sos_configuration_id: sos.id.clone(),
        scenario: Some(Scenario { start_tai_ns: 0, end_tai_ns: 5_000_000_000, ..Default::default() }),
        options: Some(DrmOptions { default_step_rate_hz: 1.0, sample_interval_s: 1.0, ..Default::default() }),
        ..Default::default()
    });
    drm.scenario.as_mut().unwrap().faults = vec![Fault {
        id: "f1".to_string(),
        instance: "veh".to_string(),
        target_kind: FaultTargetKind::Hardware as i32,
        tai_ns: 2_000_000_000,
        kind: "power_cycle".to_string(),
        ..Default::default()
    }];
    drm.hash = hash::canonical_drm_hash(&drm);
    let systems = one_system_map(&sys);

    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let err = execute(run_config(&gmat, &drm, &sos, &systems, "test-run-hardware-model")).unwrap_err();
    assert!(
        matches!(err, DrmError::HardwareFaultNotSupportedOnInstance { ref fault_id, ref instance } if fault_id == "f1" && instance == "veh"),
        "{err:?}"
    );
}

// ------------------------------------------------------------------------------------------
// 6d. M16.2 (docs/open-questions.md question 120): a FAULT_TARGET_KIND_HARDWARE fault naming a
//     container instance with any kind other than "power_cycle" (a Renode/board shape this
//     crate has no runtime for against a container) is refused by name, not silently dropped.
//     Not individually required, but closes the same silent-drop gap the two tests above close.
// ------------------------------------------------------------------------------------------

/// **What a wrong implementation fails this test against.** Without this check,
/// `run_shared_group`'s own boundary-collection loop's `fault::is_container_power_cycle` arm
/// requires `kind == "power_cycle"`, so a HARDWARE fault naming a container with any other kind
/// (here, `"board_reset"`) matches neither boundary-collection arm and is silently dropped --
/// `execute()` would return `Ok(RunProducts)` with no error and no FAULT event, and the
/// `.unwrap_err()` below would panic. This fixture never dials the container's own address (the
/// refusal fires during load-time validation, before `binding::materialize_container` is ever
/// called), so no `lockstep-ref` subprocess is spawned for this test.
#[test]
fn a_hardware_fault_with_an_unsupported_kind_on_a_container_instance_is_refused() {
    let _engine = gmat_sys::engine_lock();
    let port = free_port(); // never dialed -- see this test's own doc comment.

    let sys = container_system_with_address("hw_kind_sys", &format!("127.0.0.1:{port}"), "in");
    let (sys, sos, mut drm) = container_drm("hw_kind_sos", "sig", sys, BTreeMap::from([("sig_seed".to_string(), 42)]));
    drm.scenario.as_mut().unwrap().faults = vec![Fault {
        id: "f1".to_string(),
        instance: "sig".to_string(),
        target_kind: FaultTargetKind::Hardware as i32,
        tai_ns: 1_000_000_000,
        kind: "board_reset".to_string(),
        ..Default::default()
    }];
    drm.hash = hash::canonical_drm_hash(&drm);
    let systems = one_system_map(&sys);

    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let err = execute(run_config(&gmat, &drm, &sos, &systems, "test-run-hardware-kind")).unwrap_err();
    assert!(
        matches!(err, DrmError::HardwareFaultKindNotSupported { ref fault_id, ref instance, ref kind } if fault_id == "f1" && instance == "sig" && kind == "board_reset"),
        "{err:?}"
    );
}

// ------------------------------------------------------------------------------------------
// 7. M15.3 (docs/open-questions.md question 118): the Docker image lifecycle through the real
//    `execute()` entry point -- `ContainerBinding.image`/`image_digest` (never `container.
//    address`) pulled by digest from a throwaway loopback-only local registry, run, Bind over
//    loopback, and stopped + removed automatically once the run ends. Gated on `docker info`;
//    a skip prints its reason (best-effort -- see `crates/av-lockstep/tests/docker_lifecycle.rs`'s
//    own module doc comment for the disclosed `cargo test` stdout-capture limitation this
//    shares, and `tests/test_lockstep_ref.py` for the verified-visible pytest-side equivalent).
// ------------------------------------------------------------------------------------------

fn docker_cmd(args: &[&str]) -> String {
    let output = std::process::Command::new("docker").args(args).current_dir(repo_root()).output().unwrap_or_else(|e| panic!("could not launch `docker {args:?}`: {e}"));
    if !output.status.success() {
        panic!("`docker {args:?}` failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}
struct DockerContainerGuard(String);
impl Drop for DockerContainerGuard {
    fn drop(&mut self) {
        let _ = std::process::Command::new("docker").args(["rm", "-f", &self.0]).output();
    }
}
struct DockerImageGuard(String);
impl Drop for DockerImageGuard {
    fn drop(&mut self) {
        let _ = std::process::Command::new("docker").args(["rmi", "-f", &self.0]).output();
    }
}

/// A `BINDING_KIND_CONTAINER` `SystemDefinition` for the Docker image-lifecycle path: no
/// `container.address` at all (mutually exclusive with `ContainerBinding.image` --
/// `binding::parse_container_spec`'s own refusal), just `container.seed_key` plus the declared
/// named output.
fn docker_container_system(id: &str) -> SystemDefinition {
    hashed_system(SystemDefinition {
        id: id.to_string(),
        dynamics_model: String::new(),
        ports: vec![signal_port("in", PortDirection::In), signal_port("out", PortDirection::Out)],
        state_space_id: "container.none".to_string(),
        state_space: Some(StateSpace { id: "container.none".to_string(), components: vec![], frame_id: String::new() }),
        parameters: vec![sparam("container.seed_key", "sig_seed"), Parameter { name: "output.integral".to_string(), unit: Unit::Unspecified as i32, ..Default::default() }],
        ..Default::default()
    })
}
fn docker_container_instance(name: &str, system_id: &str, image: &str, image_digest: &str) -> SystemInstance {
    SystemInstance {
        name: name.to_string(),
        system_id: system_id.to_string(),
        binding: Some(Binding { kind: BindingKind::Container as i32, config: Some(av_cdm::pb::binding::Config::Container(ContainerBinding { image: image.to_string(), image_digest: image_digest.to_string(), ..Default::default() })) }),
        step_rate_hz: 1.0,
        ..Default::default()
    }
}

/// **What this test would fail against.** An `execute()` that never pulls/runs the declared
/// image at all (the pre-M15.3 state, where `ContainerBinding.image`/`image_digest` were parsed
/// by nobody -- `binding.rs`'s own pre-M15.3 module doc comment) would refuse this DRM outright
/// (`DrmError::MissingParameter` on the now-absent `container.address`, or `DrmError::
/// UnknownParameter` on `image`/`image_digest` having nowhere to go) -- the `.expect(...)` below
/// panics immediately. An implementation that runs the container but never stops/removes it at
/// `Shutdown` leaves a container behind, caught by the `docker ps -a` assertion at the end (a
/// leaked container is exactly the failure mode question 118's "stop and remove" rule exists to
/// prevent). This is a lighter-weight companion to `crates/av-lockstep/tests/docker_lifecycle.rs`'s
/// own low-level `ManagedContainer` test (which additionally proves `binding_hash` varies with
/// the digest) -- this one proves the same lifecycle is actually wired into `classify_binding`/
/// `materialize_container`/`execute()`, not just that the standalone module works in isolation.
#[test]
fn docker_image_lifecycle_through_execute_pulls_by_digest_runs_binds_and_removes_on_shutdown() {
    if !av_lockstep::docker::docker_available() {
        println!("SKIPPED docker_image_lifecycle_through_execute_...: `docker info` failed or docker is not installed (best-effort visibility only -- see this test's own doc comment).");
        return;
    }
    let _engine = gmat_sys::engine_lock();

    // Question 156's amendment: sweep whatever a previous, interrupted run left behind (its
    // own `Drop` guards never ran if that run was killed) before this test creates anything.
    // See `crates/av-lockstep/tests/docker_lifecycle.rs`'s own `DOCKER_TEST_LOCK` doc comment
    // for why a same-binary concurrent test could otherwise race this: this file has exactly
    // one Docker-using `#[test]`, so no equivalent lock is needed here yet.
    prune_stale_test_resources();
    let run_id = test_run_id();
    let labels = test_label_args(&run_id);
    let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();

    let local_tag = "lockstep-ref:av-kernel-docker-lifecycle-test";
    let mut build_args = vec!["build", "-f", "services/lockstep-ref/Dockerfile", "-t", local_tag];
    build_args.extend(label_refs.iter().copied());
    build_args.push(".");
    docker_cmd(&build_args);
    let _local_image_guard = DockerImageGuard(local_tag.to_string());

    let mut registry_args = vec!["run", "-d", "-p", "127.0.0.1::5000"];
    registry_args.extend(label_refs.iter().copied());
    registry_args.push("registry:2");
    let registry_id = docker_cmd(&registry_args);
    let _registry_guard = DockerContainerGuard(registry_id.clone());
    let registry_port_line = docker_cmd(&["port", &registry_id, "5000"]);
    let registry_port: u16 = registry_port_line.lines().next().and_then(|l| l.rsplit(':').next()).and_then(|p| p.parse().ok()).expect("a numeric host port from `docker port`");
    let pushed_ref = format!("127.0.0.1:{registry_port}/lockstep-ref:test");
    docker_cmd(&["tag", local_tag, &pushed_ref]);
    let _pushed_image_guard = DockerImageGuard(pushed_ref.clone());
    docker_cmd(&["push", &pushed_ref]);
    let repo_digests = docker_cmd(&["inspect", "--format={{index .RepoDigests 0}}", &pushed_ref]);
    let real_digest = repo_digests.rsplit('@').next().filter(|d| d.starts_with("sha256:")).expect("a @sha256:... RepoDigests entry").to_string();
    let image_ref = format!("127.0.0.1:{registry_port}/lockstep-ref");

    let sys = docker_container_system("docker_e2e_sys");
    let (sys, sos, drm) = container_drm("docker_e2e_sos", "sig", sys, BTreeMap::from([("sig_seed".to_string(), 42)]));
    let sos = SosConfiguration { instances: vec![docker_container_instance("sig", &sys.id, &image_ref, &real_digest)], ..sos };
    let sos = hashed_sos(sos);
    let mut drm = drm;
    drm.sos_configuration_id = sos.id.clone();
    drm.hash = hash::canonical_drm_hash(&drm);
    let systems = one_system_map(&sys);

    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let products = execute(run_config(&gmat, &drm, &sos, &systems, "test-run-docker-e2e")).expect("the Docker image-lifecycle path must run this DRM end to end");

    let traj = products.trajectories.get("sig").expect("the \"sig\" instance produced a trajectory");
    let prov = traj.provenance.as_ref().expect("finish_trajectory always attaches a Provenance");
    let recorded_hash = prov.attributes.get("container_binding_hash").expect("container_binding_hash attribute present");
    assert_eq!(recorded_hash.len(), 64, "lockstep-ref's own binding_hash is a hex-encoded SHA-256");

    // Question 118's own "Shutdown then stop and remove on run end": by the time execute() has
    // returned, no container started from this run's own image remains, running or stopped.
    let ps = docker_cmd(&["ps", "-a", "-q", "--filter", &format!("ancestor={pushed_ref}")]);
    assert!(ps.is_empty(), "a container from {pushed_ref:?} is still present after the run ended: {ps:?}");
}
