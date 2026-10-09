//! Hilprep-2b regression: a **plain container** (`BINDING_KIND_CONTAINER`, not a board) with a
//! `HARDWARE`/`power_cycle` fault, replayed through the container replay path
//! (`RunConfig.replay`, M25.4b).
//!
//! Before hilprep-2b the replayed container was a model span carrying a placeholder
//! `ConstantAccel` plan, so at the fault's boundary the executor sent the HARDWARE fault down the
//! DYNAMICS path (`fault::apply_dynamics_fault` on a plan it does not belong to), never emitted
//! the `fault:<id>` event for the replayed instance, and re-materialized the instance from the
//! placeholder plan at every boundary (changing its `ModelInfo` after the first span). This test
//! pins the fixed behaviour: the replay runs, `fault:<id>` is present, and the replayed products
//! equal the live run's except the fields a replayed container cannot reproduce (its segment's
//! `dynamics_*` and the `container_binding_hash` provenance attribute, as `tests/replay.rs` and
//! `drm_attitude_control_cfs.rs` already name them).
//!
//! The live half needs a container peer without Docker: `services/lockstep-ref` as a local Python
//! subprocess, exactly as `tests/drm_container.rs` spawns it (the stand-in that reaches the same
//! executor code; no Docker, no board).
use std::collections::BTreeMap;
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use av_cdm::pb::{self, Binding, BindingKind, Connection, ContainerBinding, DesignReferenceMission, DrmOptions, EventKind, Fault, FaultTargetKind, ModelBinding, Parameter, Port, PortDirection, PortKind, Scenario, SosConfiguration, StateSpace, SystemDefinition, SystemInstance};
use av_kernel::drm::replay::ReplayConfig;
use av_kernel::drm::{execute, hash, DrmError, RunConfig, RunProducts};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn_lockstep_ref() -> (ChildGuard, u16) {
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let child = Command::new(repo_root().join(".venv/bin/python"))
        .args(["-m", "lockstep_ref", "--port", &port.to_string()])
        .current_dir(repo_root().join("services/lockstep-ref"))
        .spawn()
        .expect("spawn `python -m lockstep_ref`");
    let guard = ChildGuard(child);
    let address = format!("127.0.0.1:{port}");
    let deadline = Instant::now() + Duration::from_secs(30);
    while av_lockstep::BlockingLockstepClient::connect_plaintext(&address).is_err() {
        assert!(Instant::now() < deadline, "lockstep_ref on {address} not ready");
        std::thread::sleep(Duration::from_millis(50));
    }
    (guard, port)
}

fn sparam(name: &str, s: &str) -> Parameter {
    Parameter { name: name.to_string(), string_value: s.to_string(), ..Default::default() }
}

fn signal_port(name: &str, direction: PortDirection) -> Port {
    Port { name: name.to_string(), kind: PortKind::Signal as i32, direction: direction as i32, schema: "signal".to_string(), timing: None, interface_class: String::new() }
}

fn hashed_system(mut sys: SystemDefinition) -> SystemDefinition {
    sys.hash = hash::canonical_system_hash(&sys);
    sys
}

struct Scene {
    drm: DesignReferenceMission,
    sos: SosConfiguration,
    systems: BTreeMap<String, SystemDefinition>,
}

/// An `emitter` (native, constant SIGNAL on `out`) wired to a container `sig` (lockstep-ref at
/// `port`), 5 s at 1 Hz, with a HARDWARE `power_cycle` fault on `sig` at 3 s. No scores: a
/// port-traffic replay cannot reproduce a container's named outputs.
fn scene(port: u16) -> Scene {
    let emitter = hashed_system(SystemDefinition {
        id: "pc_emitter_sys".to_string(),
        dynamics_model: "native.constant_accel".to_string(),
        state_space_id: "gmat.orbital.cartesian6".to_string(),
        ports: vec![signal_port("out", PortDirection::Out)],
        parameters: vec![
            sparam("frame_id", "test.frame"),
            Parameter { name: "state.px".to_string(), ..Default::default() },
            Parameter { name: "state.py".to_string(), ..Default::default() },
            Parameter { name: "state.pz".to_string(), ..Default::default() },
            Parameter { name: "state.vx".to_string(), ..Default::default() },
            Parameter { name: "state.vy".to_string(), ..Default::default() },
            Parameter { name: "state.vz".to_string(), ..Default::default() },
            sparam("port.emit", "out"),
            Parameter { name: "port.emit_value".to_string(), value: 5.0, ..Default::default() },
        ],
        ..Default::default()
    });
    let container = hashed_system(SystemDefinition {
        id: "pc_container_sys".to_string(),
        ports: vec![signal_port("in", PortDirection::In), signal_port("out", PortDirection::Out)],
        state_space_id: "container.none".to_string(),
        state_space: Some(StateSpace { id: "container.none".to_string(), components: vec![], frame_id: String::new() }),
        parameters: vec![sparam("container.address", &format!("127.0.0.1:{port}")), sparam("container.seed_key", "sig_seed")],
        ..Default::default()
    });
    let mut sos = SosConfiguration {
        id: "pc_replay_sos".to_string(),
        instances: vec![
            SystemInstance {
                name: "emitter".to_string(),
                system_id: emitter.id.clone(),
                binding: Some(Binding { kind: BindingKind::Model as i32, config: Some(pb::binding::Config::Model(ModelBinding { model_id: emitter.id.clone() })) }),
                step_rate_hz: 1.0,
                ..Default::default()
            },
            SystemInstance {
                name: "sig".to_string(),
                system_id: container.id.clone(),
                binding: Some(Binding { kind: BindingKind::Container as i32, config: Some(pb::binding::Config::Container(ContainerBinding::default())) }),
                step_rate_hz: 1.0,
                ..Default::default()
            },
        ],
        connections: vec![Connection { from_instance: "emitter".to_string(), from_port: "out".to_string(), to_instance: "sig".to_string(), to_port: "in".to_string(), link_model: String::new() }],
        ..Default::default()
    };
    sos.hash = hash::canonical_sos_hash(&sos);
    let mut drm = DesignReferenceMission {
        id: "pc_replay_drm".to_string(),
        sos_configuration_id: sos.id.clone(),
        scenario: Some(Scenario {
            start_tai_ns: 0,
            end_tai_ns: 5_000_000_000,
            seeds: BTreeMap::from([("sig_seed".to_string(), 42)]),
            faults: vec![Fault { id: "f1".to_string(), instance: "sig".to_string(), target_kind: FaultTargetKind::Hardware as i32, tai_ns: 3_000_000_000, kind: "power_cycle".to_string(), ..Default::default() }],
            ..Default::default()
        }),
        options: Some(DrmOptions { default_step_rate_hz: 1.0, sample_interval_s: 1.0, ..Default::default() }),
        ..Default::default()
    };
    drm.hash = hash::canonical_drm_hash(&drm);
    Scene { drm, sos, systems: BTreeMap::from([(emitter.id.clone(), emitter), (container.id.clone(), container)]) }
}

fn run(scene: &Scene, products_dir: &std::path::Path, replay: Option<ReplayConfig>) -> Result<RunProducts, DrmError> {
    #[cfg(feature = "gmat")]
    let _engine = gmat_sys::engine_lock();
    #[cfg(feature = "gmat")]
    let gmat = gmat_sys::Gmat::setup(&gmat_sys::Gmat::default_startup_file()).expect("GMAT setup (default-feature build only)");
    execute(RunConfig {
        #[cfg(feature = "gmat")]
        gmat: &gmat,
        drm: &scene.drm,
        sos: &scene.sos,
        systems: &scene.systems,
        run_id: "pc-container-replay".to_string(),
        error_mode: Default::default(),
        products_dir: Some(products_dir.to_path_buf()),
        replay,
        command_source: None,
    })
}

/// The container replay's named exclusions (`tests/replay.rs`): the replayed instance's segment
/// `dynamics_*` and its `container_binding_hash` provenance attribute.
fn strip_container_exclusions(mut wire: pb::RunProducts) -> pb::RunProducts {
    if let Some(t) = wire.trajectories.get_mut("sig") {
        for s in t.segments.iter_mut() {
            s.dynamics_model.clear();
            s.dynamics_hash.clear();
            s.dynamics_depth.clear();
        }
        if let Some(p) = t.provenance.as_mut() {
            p.attributes.remove("container_binding_hash");
        }
    }
    wire
}

#[test]
fn a_replayed_plain_container_with_a_power_cycle_fault_keeps_its_fault_event_and_matches_the_live_run() {
    let (_server, port) = spawn_lockstep_ref();
    let scene = scene(port);
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("drm-container-replay-pc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);

    let live = run(&scene, &dir, None).unwrap_or_else(|e| panic!("the live container run: {e}"));
    let live_fault: Vec<_> = live.events.iter().filter(|e| e.kind == EventKind::Fault as i32).collect();
    assert_eq!((live_fault.len(), live_fault[0].id.as_str(), live_fault[0].entity_id.as_str(), live_fault[0].tai_ns), (1, "fault:f1", "sig", 3_000_000_000));
    let log_copy = dir.join("live_port_traffic.pb");
    std::fs::copy(dir.join("port_traffic.pb"), &log_copy).unwrap();

    // Both selections reach the fixed code: the default set (every container-classified
    // instance) and the instance named explicitly.
    for instances in [vec![], vec!["sig".to_string()]] {
        let cfg = ReplayConfig { log_path: log_copy.clone(), expected_hash: live.port_traffic_hash.clone(), instances: instances.clone() };
        let replayed = run(&scene, &dir, Some(cfg)).unwrap_or_else(|e| panic!("the replay (instances {instances:?}) must run: {e}"));
        let fault: Vec<_> = replayed.events.iter().filter(|e| e.kind == EventKind::Fault as i32).collect();
        assert_eq!(fault.len(), 1, "the replay reproduces the fault event: {:?}", replayed.events);
        assert_eq!((fault[0].id.as_str(), fault[0].entity_id.as_str(), fault[0].tai_ns), ("fault:f1", "sig", 3_000_000_000));
        assert_eq!(live_fault[0], fault[0]);
        let (a, b) = (strip_container_exclusions(live.to_proto()), strip_container_exclusions(replayed.to_proto()));
        assert_eq!(a.trajectories["sig"].segments.len(), 1, "the live container's segments merge across the boundary");
        assert_eq!(b.trajectories["sig"].segments.len(), 1, "so do the replayed instance's: its ModelInfo is the same in every span");
        assert_eq!(a.trajectories, b.trajectories);
        assert_eq!(a.events, b.events);
        assert_eq!(a, b, "the replayed products equal the live run's except the named container exclusions");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
