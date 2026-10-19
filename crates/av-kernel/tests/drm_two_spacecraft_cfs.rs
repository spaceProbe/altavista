//! Native-ct-1 (`docs/open-questions.md` question 245 (5)): **two spacecraft, each closed through its
//! own `altavista-cfs-lockstep` container, in one run**, generalising the single-container proofs of
//! `tests/drm_attitude_control_cfs.rs` (M23.4) to an RPO pair.
//!
//! **The DRM** (`drms/demo_two_spacecraft.{drm,sos}.yaml`; read their headers): an RPO pair,
//! `chaser` and `target`. Each owns the M22.4 attitude loop (truth plant, star tracker, IMU,
//! controller; the system definitions are reused unchanged, two instances of each), a native flight
//! computer that broadcasts its Earth-fixed position as FRAMED telemetry
//! (`drms/demo_ground_segment_flight.system.yaml`, unchanged) and a native thruster that consumes a
//! FRAMED telecommand, acknowledges it and reports its own position and the throttle in force on a
//! second FRAMED OUT port (`drms/demo_two_spacecraft_thruster.system.yaml`; question 254 item 7). A
//! ground segment of two station instances (one tracking channel per spacecraft; kept as recorded,
//! although a station now handles several senders: question 254, the test
//! `both_spacecraft_on_one_ground_station_are_each_handled_in_full` over
//! `drms/demo_two_spacecraft_one_station.*`) and a
//! dispatch instance receives both spacecraft's telemetry and
//! sends one telecommand, to the chaser only. In the container runs the two `*_controller`
//! instances are rebound to `BINDING_KIND_CONTAINER`, each its own container started from the same
//! image by digest through the fixed-port throwaway registry
//! ([`cfs_common::push_cfs_image_to_local_registry`]).
//!
//! **Why the telecommand reaches a native thruster, not the cFS controller or the flight computer.**
//! The cFS image accepts no input port but `startracker_in` and `imu_in`
//! (`services/cfs/apps/io_lockstep/fsw/src/io_lockstep_port_table.c`; `handle_step` skips any other
//! port) and `services/cfs/apps/adcs` has no command handler, so the cFS side is not changed and
//! not commanded. The position telemetry the ground stations decode comes from the flight computer
//! instance; the commanded thruster is a second instance whose own telemetry (apid 702) is a
//! different packet. Through task 6 a system could declare only one FRAMED OUT port
//! (`binding::resolve_sensor_output`), so the thruster could not report anything beside its ack;
//! `binding::resolve_constant_accel_emit_port` now binds a named port among several, and the
//! thruster's `tm_out` carries the throttle it is commanded to
//! ([`the_chaser_thruster_reports_the_throttle_change_on_a_second_framed_port`]).
//!
//! **Expected behaviour, stated before running** (derivations in the DRM header): the critically
//! damped closed form `theta(t) = theta0 (1 + t/tau) exp(-t/tau)`, `tau = 20 s`, gives
//! `theta(30 s) = 0.5578254 * theta0`: 0.1115651 rad for the chaser (`theta0 = 0.2`), 0.0669390 rad
//! for the target (`theta0 = 0.12`). The two differ by 40%, so a controller driving the other
//! spacecraft's plant cannot meet the 15% per-spacecraft tracking bound
//! ([`a_cross_wired_pair_fails_the_tracking_proof`] runs exactly that and requires the proof to
//! fail). The first torque each container returns is `kp * q_z` of its OWN spacecraft's star
//! tracker (`0.25 * sin(theta0/2)`: 0.02496 N m for the chaser, 0.01499 N m for the target),
//! which the port traffic checks directly.
//!
//! **Sharing.** The docker-gated runs are expensive, so the two separately started runs that proof
//! 1 needs are done once ([`pair_runs`], under the docker-test lock, inside a `OnceLock`) and the
//! proofs that only read products (determinism, tracking, ground telemetry, telecommand routing,
//! labels, cleanup) read that result; the proofs that need further runs (replay, perturbations,
//! failed runs) take the lock themselves. No test holds the docker lock while calling [`pair_runs`].

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::JoinHandle;
use std::time::Duration;

use av_cdm::pb::{Binding, BindingKind, ContainerBinding, DesignReferenceMission, EventKind, Parameter, PortDirection, PortTrafficLog, PortTrafficRecord, SosConfiguration, SystemDefinition};
use av_kernel::codec::{decode_packet, ApidMap, DecodedPacket, FieldValue};
use av_kernel::drm::replay::ReplayConfig;
use av_kernel::drm::{execute, hash, schema, DrmError, ExternalCommandSource, RunConfig, RunProducts};
use av_lockstep::docker::{lock_docker_tests, prune_stale_test_resources, test_run_id, DockerTestLock};
use gmat_sys::Gmat;
use prost::Message;

mod cfs_common;
use cfs_common::{load_system, push_cfs_image_to_local_registry, read, sha256_hex, skip_if_cfs_image_unavailable, truth_pointing_error_rad, TRUTH_QW, TRUTH_QZ};

// ------------------------------------------------------------------------------------------
// Constants of the fixture (drms/demo_two_spacecraft.*.yaml headers carry the derivations).
// ------------------------------------------------------------------------------------------

const SPACECRAFT: [&str; 2] = ["chaser", "target"];
const CONTAINER_PARTS: [&str; 2] = ["chaser_controller", "target_controller"];
/// The run's window: 30 s = 1.5 tau at the fixture's 10 Hz.
const DURATION_S: i64 = 30;
const START_TAI_NS: i64 = 1_767_225_637_000_000_000;
const COMMAND_TAI_NS: i64 = START_TAI_NS + 15_000_000_000;
const COMMANDED_ACCEL_SCALE: f64 = 4.0;
/// Initial pointing error about +z, rad (`chaser_attitude` keeps the system's; `target_attitude`
/// overrides `attitude.q0.z/w`).
const THETA0_CHASER_RAD: f64 = 0.2;
const THETA0_TARGET_RAD: f64 = 0.12;
const TAU_S: f64 = 20.0;
const KP: f64 = 0.25;

fn theta0_rad(sc: &str) -> f64 {
    if sc == "chaser" {
        THETA0_CHASER_RAD
    } else {
        THETA0_TARGET_RAD
    }
}

/// `theta(t)/theta(0)` of the critically damped loop (`drms/demo_attitude_control.drm.yaml`'s
/// derivation, re-derived here like `drm_attitude_control_cfs.rs` does).
fn expected_theta_ratio(t_s: f64) -> f64 {
    (1.0 + t_s / TAU_S) * (-t_s / TAU_S).exp()
}

/// The labels the kernel attaches to every container it starts (question 245 (5), decision 2 of
/// the native-ct-1 report): `av.run_id` (`RunConfig.run_id`), `av.instance` (the instance name),
/// and whatever the SoS declares as `container.label.<key>`.
const LABEL_RUN_ID: &str = "av.run_id";
const LABEL_INSTANCE: &str = "av.instance";
/// The test marker `prune_stale_test_resources` sweeps by; declared by the test's SoS, never read by
/// the kernel from its environment.
const LABEL_TEST: &str = "av.test";

// ------------------------------------------------------------------------------------------
// Fixture loading.
// ------------------------------------------------------------------------------------------

fn load_systems(controller_stem: &str) -> BTreeMap<String, SystemDefinition> {
    let mut systems = BTreeMap::new();
    // `demo_command_flight` is the one-station SoS's thruster (unchanged, task 6); `demo_two_spacecraft_
    // thruster` is this SoS's (question 254 item 7: a second FRAMED OUT port).
    for stem in ["demo_attitude_control_truth", "demo_attitude_control_startracker", "demo_attitude_control_imu", controller_stem, "demo_ground_segment_flight", "demo_command_flight", "demo_two_spacecraft_thruster", "demo_ground_segment_ground", "demo_command_ground"] {
        let sys = load_system(stem);
        systems.insert(sys.id.clone(), sys);
    }
    systems
}
fn native_systems() -> BTreeMap<String, SystemDefinition> {
    load_systems("demo_attitude_control_controller")
}
fn container_systems() -> BTreeMap<String, SystemDefinition> {
    load_systems("demo_attitude_control_controller_cfs")
}

fn base_sos() -> SosConfiguration {
    let mut sos = schema::parse_sos_yaml(&read("demo_two_spacecraft.sos.yaml")).expect("demo_two_spacecraft.sos.yaml parses");
    sos.hash = hash::canonical_sos_hash(&sos);
    sos
}

/// The native SoS under a scratch id (both controllers `BINDING_KIND_MODEL`).
fn native_sos(id: &str) -> SosConfiguration {
    let mut sos = base_sos();
    sos.id = id.to_string();
    sos.hash = hash::canonical_sos_hash(&sos);
    sos
}

/// How a container-bound SoS deviates from the fixture's intended form (all defaults = intended).
#[derive(Clone, Debug)]
struct ContainerVariant {
    /// `container.seed_key` of the chaser's and the target's controller.
    seed_keys: [String; 2],
    /// Swap the two `wheel_torque_out` connections' receivers (chaser controller -> target plant and
    /// vice versa): the negative control for the tracking proof.
    cross_wire: bool,
    /// Declare `container.label.av.test = 1` on both controllers, so `prune_stale_test_resources`
    /// can sweep what the kernel started.
    test_marker_label: bool,
}
impl Default for ContainerVariant {
    fn default() -> Self {
        Self { seed_keys: ["chaser_controller".to_string(), "target_controller".to_string()], cross_wire: false, test_marker_label: true }
    }
}

fn string_param(name: &str, value: &str) -> Parameter {
    Parameter { name: name.to_string(), string_value: value.to_string(), ..Default::default() }
}

/// [`base_sos`] with both `*_controller` instances rebound from `BINDING_KIND_MODEL` (the native
/// controller) to `BINDING_KIND_CONTAINER`: the image by digest, the cFS-bound system, 10 Hz, a seed
/// key each, and the variant's deviations.
fn container_sos(id: &str, image: &str, image_digest: &str, variant: &ContainerVariant) -> SosConfiguration {
    let mut sos = base_sos();
    sos.id = id.to_string();
    rebind_controllers_to_containers(sos, image, image_digest, variant)
}

/// The rebinding of [`container_sos`] for any SoS that declares the two `*_controller` instances (the
/// one-station SoS too, question 254): `sos.id` is the caller's.
fn rebind_controllers_to_containers(mut sos: SosConfiguration, image: &str, image_digest: &str, variant: &ContainerVariant) -> SosConfiguration {
    for (i, name) in CONTAINER_PARTS.iter().enumerate() {
        let inst = sos.instances.iter_mut().find(|inst| inst.name == *name).unwrap_or_else(|| panic!("the SoS must declare {name}"));
        inst.system_id = "attitude_control_controller_cfs_sys".to_string();
        inst.step_rate_hz = 10.0;
        inst.binding = Some(Binding { kind: BindingKind::Container as i32, config: Some(av_cdm::pb::binding::Config::Container(ContainerBinding { image: image.to_string(), image_digest: image_digest.to_string(), ..Default::default() })) });
        inst.parameter_overrides.push(string_param("container.seed_key", &variant.seed_keys[i]));
        if variant.test_marker_label {
            inst.parameter_overrides.push(string_param(&format!("container.label.{LABEL_TEST}"), "1"));
        }
    }
    if variant.cross_wire {
        for c in sos.connections.iter_mut().filter(|c| c.from_port == "wheel_torque_out") {
            c.to_instance = if c.to_instance == "chaser_attitude" { "target_attitude".to_string() } else { "chaser_attitude".to_string() };
        }
    }
    sos.hash = hash::canonical_sos_hash(&sos);
    sos
}

fn two_sc_drm(id: &str, sos_id: &str, duration_s: i64) -> DesignReferenceMission {
    let mut drm = schema::parse_drm_yaml(&read("demo_two_spacecraft.drm.yaml")).expect("demo_two_spacecraft.drm.yaml parses");
    drm.id = id.to_string();
    drm.sos_configuration_id = sos_id.to_string();
    drm.scenario.as_mut().expect("scenario").end_tai_ns = START_TAI_NS + duration_s * 1_000_000_000;
    drm.hash = hash::canonical_drm_hash(&drm);
    drm
}

fn run_config<'a>(gmat: &'a Gmat, drm: &'a DesignReferenceMission, sos: &'a SosConfiguration, systems: &'a BTreeMap<String, SystemDefinition>, run_id: &str, products_dir: Option<PathBuf>) -> RunConfig<'a> {
    RunConfig { gmat, drm, sos, systems, run_id: run_id.to_string(), error_mode: Default::default(), products_dir, replay: None, command_source: None }
}

/// A fresh scratch directory under the OS temp dir, unique per call within this process (the same
/// convention as `drm_attitude_control_cfs.rs::replay_scratch_dir`).
fn scratch_dir(label: &str) -> PathBuf {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("av-kernel-two-sc-{}-{label}-{n}", std::process::id()));
    assert!(!dir.exists(), "scratch dir {dir:?} must not already exist");
    dir
}

/// The same three calls, in the same order, as `drm_attitude_control_cfs.rs::lock_and_prune_docker_
/// tests` (question 207/156): the host-wide lock, the sweep of whatever an interrupted run left,
/// this run's id.
fn lock_and_prune_docker_tests() -> (DockerTestLock, String) {
    let lock = lock_docker_tests();
    prune_stale_test_resources(&lock);
    let run_id = test_run_id();
    (lock, run_id)
}

// ------------------------------------------------------------------------------------------
// Effective parameters (system parameter, overridden by the instance's `parameter_overrides`),
// so the expectations are computed from the fixture, not retyped.
// ------------------------------------------------------------------------------------------

fn effective_param(systems: &BTreeMap<String, SystemDefinition>, sos: &SosConfiguration, instance: &str, name: &str) -> f64 {
    let inst = sos.instances.iter().find(|i| i.name == instance).unwrap_or_else(|| panic!("instance {instance}"));
    if let Some(p) = inst.parameter_overrides.iter().find(|p| p.name == name) {
        return p.value;
    }
    systems[&inst.system_id].parameters.iter().find(|p| p.name == name).unwrap_or_else(|| panic!("parameter {name} of {instance}")).value
}

/// The initial pointing error the SoS declares for `<sc>_attitude`: `2 * asin(q0.z)` (rotation
/// about +z only).
fn declared_initial_error_rad(systems: &BTreeMap<String, SystemDefinition>, sos: &SosConfiguration, sc: &str) -> f64 {
    let inst = format!("{sc}_attitude");
    let (qx, qy, qz, qw) = (effective_param(systems, sos, &inst, "attitude.q0.x"), effective_param(systems, sos, &inst, "attitude.q0.y"), effective_param(systems, sos, &inst, "attitude.q0.z"), effective_param(systems, sos, &inst, "attitude.q0.w"));
    assert!(qx == 0.0 && qy == 0.0, "the fixture's initial error is a pure rotation about +z");
    assert!((qz * qz + qw * qw - 1.0).abs() < 1e-14, "q0 must be a unit quaternion");
    2.0 * qz.asin()
}

/// Closed-form position of a flight computer at `t_s` seconds after the start, with the commanded
/// `accel_scale` `scale` in effect from `t_apply_s` (`None`: never commanded).
struct FlightKinematics {
    x0: [f64; 3],
    v0: [f64; 3],
    a: [f64; 3],
}
impl FlightKinematics {
    fn of(systems: &BTreeMap<String, SystemDefinition>, sos: &SosConfiguration, instance: &str) -> Self {
        let p = |n: &str| effective_param(systems, sos, instance, n);
        Self { x0: [p("state.px"), p("state.py"), p("state.pz")], v0: [p("state.vx"), p("state.vy"), p("state.vz")], a: [p("accel.x"), p("accel.y"), p("accel.z")] }
    }
    fn position(&self, t_s: f64, command: Option<(f64, f64)>) -> [f64; 3] {
        let base = |t: f64, k: usize| self.x0[k] + self.v0[k] * t + 0.5 * self.a[k] * t * t;
        match command {
            Some((t_apply, scale)) if t_s > t_apply => {
                let mut out = [0.0; 3];
                for (k, o) in out.iter_mut().enumerate() {
                    let v_apply = self.v0[k] + self.a[k] * t_apply;
                    let dt = t_s - t_apply;
                    *o = base(t_apply, k) + v_apply * dt + 0.5 * scale * self.a[k] * dt * dt;
                }
                out
            }
            _ => [base(t_s, 0), base(t_s, 1), base(t_s, 2)],
        }
    }
}

// ------------------------------------------------------------------------------------------
// Truth-based pointing error per spacecraft (the cFS container exposes no output.* scalar; see
// drm_attitude_control_cfs.rs's module doc comment).
// ------------------------------------------------------------------------------------------

fn truth_theta_at_end(products: &RunProducts, sc: &str) -> f64 {
    let traj = products.trajectories.get(&format!("{sc}_attitude")).unwrap_or_else(|| panic!("{sc}_attitude produced a trajectory"));
    let mean = &traj.samples.last().expect("at least one sample").mean;
    let from_asin = truth_pointing_error_rad(mean);
    let from_acos = 2.0 * mean[TRUTH_QW].clamp(-1.0, 1.0).acos();
    assert!((from_asin - from_acos).abs() < 1e-9, "{sc}: asin- and acos-derived pointing error must agree for a unit quaternion: {from_asin} vs {from_acos}");
    from_asin
}

fn truth_qz_series(products: &RunProducts, sc: &str) -> Vec<(i64, f64)> {
    products.trajectories[&format!("{sc}_attitude")].samples.iter().map(|s| (s.tai_ns, s.mean[TRUTH_QZ])).collect()
}

/// One spacecraft's row of the tracking proof.
#[derive(Debug, Clone)]
struct TrackingRow {
    sc: &'static str,
    cfs_theta: f64,
    native_theta: f64,
    expected_theta: f64,
}

/// The pointing proof of `drm_attitude_control_cfs.rs`, per spacecraft, as a predicate so a
/// negative control can require it to FAIL: the native run within 10% of the closed form; the
/// container run converged (>= 1.5x reduction), not exactly zero, within 15% of the native run,
/// with no overshoot. `Err` names the first violated bound.
fn check_tracking(cfs: &RunProducts, native: &RunProducts, theta0: impl Fn(&str) -> f64) -> Result<Vec<TrackingRow>, String> {
    let mut rows = Vec::new();
    for sc in SPACECRAFT {
        let theta0 = theta0(sc);
        let expected = theta0 * expected_theta_ratio(DURATION_S as f64);
        let (cfs_theta, native_theta) = (truth_theta_at_end(cfs, sc), truth_theta_at_end(native, sc));
        rows.push(TrackingRow { sc, cfs_theta, native_theta, expected_theta: expected });
        if (native_theta - expected).abs() / expected >= 0.10 {
            return Err(format!("{sc}: native theta({DURATION_S}s)={native_theta} rad is not within 10% of the closed form {expected} rad"));
        }
        if cfs_theta.abs() >= theta0 / 1.5 {
            return Err(format!("{sc}: container-bound loop did not converge by 1.5x from {theta0} rad: {cfs_theta} rad"));
        }
        if cfs_theta.abs() <= 1e-9 {
            return Err(format!("{sc}: container-bound pointing error is exactly zero"));
        }
        let rel = (cfs_theta - native_theta).abs() / native_theta.abs();
        if rel >= 0.15 {
            return Err(format!("{sc}: container-bound theta={cfs_theta} rad is {:.1}% from the native {native_theta} rad (bound 15%)", rel * 100.0));
        }
        let series = truth_qz_series(cfs, sc);
        let q0 = series.first().expect("at least one sample").1.abs();
        if let Some((t, qz)) = series.iter().find(|(_, qz)| qz.abs() > q0 + 1e-6) {
            return Err(format!("{sc}: q_z overshoots its initial magnitude {q0} (got {qz} at t_ns={t})"));
        }
    }
    Ok(rows)
}

// ------------------------------------------------------------------------------------------
// Port traffic.
// ------------------------------------------------------------------------------------------

fn decode_log(bytes: &[u8]) -> PortTrafficLog {
    PortTrafficLog::decode(bytes).expect("port_traffic.pb decodes")
}

/// Lowercase-hex SHA-256 of the log's records alone, the way `drm_attitude_control_renode.rs`
/// defines its "records-only hash" (the whole-file hash also covers the log's run id and provenance).
fn records_only_hash(log: &PortTrafficLog) -> String {
    sha256_hex(&PortTrafficLog { records: log.records.clone(), ..Default::default() }.encode_to_vec())
}

fn records<'a>(log: &'a PortTrafficLog, instance: &str, port: &str, direction: PortDirection) -> Vec<&'a PortTrafficRecord> {
    log.records.iter().filter(|r| r.instance == instance && r.port == port && r.direction == direction as i32).collect()
}

/// `(instance, port, direction) -> record count`.
fn record_counts(log: &PortTrafficLog) -> BTreeMap<(String, String, i32), usize> {
    let mut counts = BTreeMap::new();
    for r in &log.records {
        *counts.entry((r.instance.clone(), r.port.clone(), r.direction)).or_insert(0) += 1;
    }
    counts
}

fn apid_map(systems: &BTreeMap<String, SystemDefinition>) -> ApidMap {
    let mut map = ApidMap::new();
    for sys in systems.values() {
        for codec in &sys.packet_codecs {
            if let Some(prev) = map.get(&codec.apid) {
                assert_eq!(prev.fields.iter().map(|f| (&f.name, f.bit_offset, f.bit_width)).collect::<Vec<_>>(), codec.fields.iter().map(|f| (&f.name, f.bit_offset, f.bit_width)).collect::<Vec<_>>(), "apid {} is declared with two layouts", codec.apid);
            }
            map.insert(codec.apid, codec.clone());
        }
    }
    map
}

fn decode(map: &ApidMap, rec: &PortTrafficRecord) -> DecodedPacket {
    decode_packet(map, &rec.payload).unwrap_or_else(|e| panic!("record {}:{} @ {} does not decode: {e:?}", rec.instance, rec.port, rec.tai_ns))
}

fn num(pkt: &DecodedPacket, field: &str) -> f64 {
    match pkt.fields.get(field) {
        Some(FieldValue::Numeric(v)) => *v,
        other => panic!("field {field} of apid {}: {other:?}", pkt.apid),
    }
}

// ------------------------------------------------------------------------------------------
// Docker state: snapshots, the label watcher.
// ------------------------------------------------------------------------------------------

fn docker_lines(args: &[&str]) -> Vec<String> {
    match Command::new("docker").args(args).output() {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect(),
        _ => Vec::new(),
    }
}

/// What `docker` holds at one instant, as sets, so a before/after pair can be diffed.
#[derive(Debug, Clone)]
struct DockerSnapshot {
    /// Every container id (`docker ps -aq --no-trunc`), running or not.
    containers: BTreeSet<String>,
    /// `repository:tag` of every image.
    image_refs: BTreeSet<String>,
    /// Every volume name, and the dangling ones (no container references them).
    volumes: BTreeSet<String>,
    dangling_volumes: BTreeSet<String>,
    networks: BTreeSet<String>,
}
impl DockerSnapshot {
    fn take() -> Self {
        Self {
            containers: docker_lines(&["ps", "-aq", "--no-trunc"]).into_iter().collect(),
            image_refs: docker_lines(&["images", "--format", "{{.Repository}}:{{.Tag}}"]).into_iter().collect(),
            volumes: docker_lines(&["volume", "ls", "-q"]).into_iter().collect(),
            dangling_volumes: docker_lines(&["volume", "ls", "-q", "-f", "dangling=true"]).into_iter().collect(),
            networks: docker_lines(&["network", "ls", "-q", "--no-trunc"]).into_iter().collect(),
        }
    }
}

/// Container ids (running or not) carrying `label` (`key` or `key=value`).
fn containers_with_label(label: &str) -> Vec<String> {
    docker_lines(&["ps", "-aq", "--no-trunc", "--filter", &format!("label={label}")])
}

/// Container ids (running or not) created from the image `ancestor` (id, name or reference).
fn containers_from_image(ancestor: &str) -> Vec<String> {
    docker_lines(&["ps", "-aq", "--no-trunc", "--filter", &format!("ancestor={ancestor}")])
}

/// One container the kernel started, as the label watcher saw it while the run was live.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SeenContainer {
    id: String,
    run_id: String,
    instance: String,
    test: String,
    image: String,
}

struct LabelWatcher {
    stop: Arc<AtomicBool>,
    handle: JoinHandle<BTreeMap<String, SeenContainer>>,
}
impl LabelWatcher {
    /// Polls `docker ps --filter label=av.run_id=<run_id>` every 40 ms until [`LabelWatcher::finish`],
    /// remembering every distinct container it sees with the labels it carried.
    fn start(run_id: &str) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let filter = format!("label={LABEL_RUN_ID}={run_id}");
        let format = format!("{{{{.ID}}}}|{{{{.Label \"{LABEL_RUN_ID}\"}}}}|{{{{.Label \"{LABEL_INSTANCE}\"}}}}|{{{{.Label \"{LABEL_TEST}\"}}}}|{{{{.Image}}}}");
        let handle = std::thread::spawn(move || {
            let mut seen: BTreeMap<String, SeenContainer> = BTreeMap::new();
            loop {
                let stopping = flag.load(Ordering::Relaxed);
                for line in docker_lines(&["ps", "--no-trunc", "--filter", &filter, "--format", &format]) {
                    let parts: Vec<&str> = line.split('|').collect();
                    if parts.len() == 5 {
                        seen.insert(parts[0].to_string(), SeenContainer { id: parts[0].to_string(), run_id: parts[1].to_string(), instance: parts[2].to_string(), test: parts[3].to_string(), image: parts[4].to_string() });
                    }
                }
                if stopping {
                    break;
                }
                std::thread::sleep(Duration::from_millis(40));
            }
            seen
        });
        Self { stop, handle }
    }
    fn finish(self) -> Vec<SeenContainer> {
        self.stop.store(true, Ordering::Relaxed);
        self.handle.join().expect("label watcher thread").into_values().collect()
    }
}

/// Removes the anonymous volumes `registry:2` (which declares `VOLUME /var/lib/registry`) leaves
/// behind: `docker rm -f` without `-v` (what `cfs_common::DockerContainerGuard` and
/// `prune_stale_test_resources` run) keeps a container's anonymous volumes. Declared BEFORE the
/// registry guards so it drops AFTER them.
struct RegistryVolumeGuard(Vec<String>);
impl Drop for RegistryVolumeGuard {
    fn drop(&mut self) {
        for v in &self.0 {
            let _ = Command::new("docker").args(["volume", "rm", v]).output();
        }
    }
}
/// The named/anonymous volumes mounted into the container `container_id`.
fn container_volumes(container_id: &str) -> Vec<String> {
    docker_lines(&["inspect", "--format", "{{range .Mounts}}{{if eq .Type \"volume\"}}{{println .Name}}{{end}}{{end}}", container_id])
}

// ------------------------------------------------------------------------------------------
// The shared pair of separately started runs.
// ------------------------------------------------------------------------------------------

/// Everything one container run leaves behind that the proofs read.
struct ContainerRun {
    products: RunProducts,
    sidecar: Vec<u8>,
}

struct PairRuns {
    systems: BTreeMap<String, SystemDefinition>,
    sos: SosConfiguration,
    drm: DesignReferenceMission,
    run_a: ContainerRun,
    run_b: ContainerRun,
    /// The containers the kernel had running while run A was live, with their labels.
    seen_during_run_a: Vec<SeenContainer>,
    /// The label/ancestry queries, taken under the lock right after teardown.
    leftovers: Leftovers,
    docker_before: DockerSnapshot,
    docker_after: DockerSnapshot,
    /// Volumes the registries created and the guard removed.
    registry_volumes: Vec<String>,
    kernel_run_id: String,
}

const PAIR_RUN_ID: &str = "test-run-two-sc-det";

/// One run with the sidecar written to `dir` (read back, then `dir` removed). `RunProducts.
/// provenance.attributes["port_traffic_uri"]` names that directory, so two runs that must have
/// byte-identical products are given the same `dir` (one after the other).
fn execute_container_run(gmat: &Gmat, drm: &DesignReferenceMission, sos: &SosConfiguration, systems: &BTreeMap<String, SystemDefinition>, run_id: &str, dir: &std::path::Path, label: &str) -> ContainerRun {
    let products = execute(run_config(gmat, drm, sos, systems, run_id, Some(dir.to_path_buf()))).unwrap_or_else(|e| panic!("run {label} must execute end to end against two real cFS containers: {e:?}"));
    let sidecar = std::fs::read(dir.join("port_traffic.pb")).unwrap_or_else(|e| panic!("reading run {label}'s port_traffic.pb: {e}"));
    let _ = std::fs::remove_dir_all(dir);
    ContainerRun { products, sidecar }
}

fn pair_runs() -> &'static PairRuns {
    static PAIR: OnceLock<Result<PairRuns, String>> = OnceLock::new();
    match PAIR.get_or_init(|| std::panic::catch_unwind(compute_pair_runs).map_err(|p| p.downcast_ref::<String>().cloned().or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_else(|| "panic with a non-string payload".to_string()))) {
        Ok(p) => p,
        Err(msg) => panic!("the shared pair of container runs failed: {msg}"),
    }
}

fn compute_pair_runs() -> PairRuns {
    let _engine = gmat_sys::engine_lock();
    let (_lock, docker_run_id) = lock_and_prune_docker_tests();
    let systems = container_systems();
    let cfs_image_id = docker_lines(&["inspect", "--format", "{{.Id}}", &cfs_common::cfs_image()]).into_iter().next().expect("the cFS image is present (the gate checked)");
    let docker_before = DockerSnapshot::take();

    let registry_volumes;
    let (sos, drm, run_a, run_b, seen_during_run_a);
    {
        // Dropped last (declared first): the registries' anonymous volumes.
        let mut volume_guard = RegistryVolumeGuard(Vec::new());
        let (image, digest, registry_guards) = push_cfs_image_to_local_registry(&docker_run_id);
        volume_guard.0 = container_volumes(&registry_guards.0 .0);
        registry_volumes = volume_guard.0.clone();
        sos = container_sos("two_spacecraft_cfs_sos", &image, &digest, &ContainerVariant::default());
        drm = two_sc_drm("two_spacecraft_cfs_det_drm", &sos.id, DURATION_S);

        let dir = scratch_dir("pair");
        let gmat_a = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
        let watcher = LabelWatcher::start(PAIR_RUN_ID);
        let result_a = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| execute_container_run(&gmat_a, &drm, &sos, &systems, PAIR_RUN_ID, &dir, "run-a")));
        seen_during_run_a = watcher.finish();
        run_a = result_a.unwrap_or_else(|p| std::panic::resume_unwind(p));
        // Run A is over and the registry is still up: the kernel's containers must already be gone.
        assert!(containers_with_label(&format!("{LABEL_RUN_ID}={PAIR_RUN_ID}")).is_empty(), "no container carrying {LABEL_RUN_ID}={PAIR_RUN_ID} may remain after run A");

        let gmat_b = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
        run_b = execute_container_run(&gmat_b, &drm, &sos, &systems, PAIR_RUN_ID, &dir, "run-b");
        drop(registry_guards);
        drop(volume_guard);
    }
    let docker_after = DockerSnapshot::take();
    let leftovers = Leftovers::take(&docker_before, &cfs_image_id, PAIR_RUN_ID);
    PairRuns { systems, sos, drm, run_a, run_b, seen_during_run_a, leftovers, docker_before, docker_after, registry_volumes, kernel_run_id: PAIR_RUN_ID.to_string() }
}

// ------------------------------------------------------------------------------------------
// Proof 1: two separately started runs give identical products and port traffic, per container.
// ------------------------------------------------------------------------------------------

#[test]
fn two_separately_started_runs_give_identical_port_traffic_and_products() {
    if skip_if_cfs_image_unavailable("two_separately_started_runs_give_identical_port_traffic_and_products") {
        return;
    }
    let p = pair_runs();
    let (a, b) = (&p.run_a, &p.run_b);
    let (log_a, log_b) = (decode_log(&a.sidecar), decode_log(&b.sidecar));

    // The sidecar, per container instance: every IN and OUT record, in order.
    for container in CONTAINER_PARTS {
        let mine = |log: &PortTrafficLog| log.records.iter().filter(|r| r.instance == container).cloned().collect::<Vec<_>>();
        let (ra, rb) = (mine(&log_a), mine(&log_b));
        let ins = ra.iter().filter(|r| r.direction == PortDirection::In as i32).count();
        let outs = ra.iter().filter(|r| r.direction == PortDirection::Out as i32).count();
        println!("{container}: {} records ({ins} IN, {outs} OUT) per run", ra.len());
        assert!(ins > 0 && outs > 0, "{container}: both IN and OUT records are expected, got {ins} IN and {outs} OUT");
        assert_eq!(ra, rb, "{container}: the decoded IN and OUT records of the two runs must be equal, in order");
    }
    assert_eq!(record_counts(&log_a), record_counts(&log_b), "per (instance, port, direction) record counts");
    for ((instance, port, direction), n) in record_counts(&log_a) {
        println!("  {instance}:{port} {} x{n}", if direction == PortDirection::In as i32 { "IN " } else { "OUT" });
    }
    assert_eq!(log_a.records, log_b.records, "the whole decoded record list must be equal");
    assert_eq!(records_only_hash(&log_a), records_only_hash(&log_b));
    println!("records-only hash: {} ({} records)", records_only_hash(&log_a), log_a.records.len());
    println!("whole-file port_traffic_hash: A={} B={} ({} bytes)", a.products.port_traffic_hash, b.products.port_traffic_hash, a.sidecar.len());
    assert_eq!(a.products.port_traffic_hash, sha256_hex(&a.sidecar), "run A's port_traffic_hash must be the SHA-256 of the file it wrote");
    assert_eq!(b.products.port_traffic_hash, sha256_hex(&b.sidecar));
    assert_eq!(a.products.port_traffic_hash, b.products.port_traffic_hash, "the whole-file port_traffic_hash must be identical across the two runs");
    assert_eq!(a.sidecar, b.sidecar, "the two port_traffic.pb files must be byte-identical");

    // The whole products, nothing excluded: with the stable container ModelInfo (native-ct-3) the
    // container segments' dynamics_hash is part of the comparison.
    assert_eq!(a.products.trajectories.keys().collect::<Vec<_>>(), b.products.trajectories.keys().collect::<Vec<_>>());
    for (name, ta) in &a.products.trajectories {
        let tb = &b.products.trajectories[name];
        assert_eq!(ta.samples, tb.samples, "{name}: samples");
        assert_eq!(ta.event_ids, tb.event_ids, "{name}: event_ids");
        assert_eq!(ta.state_space_id, tb.state_space_id, "{name}");
        assert_eq!(ta.segments, tb.segments, "{name}: segments (dynamics_model/hash/depth included)");
        assert_eq!(ta.provenance, tb.provenance, "{name}: provenance");
    }
    assert_eq!(a.products.events, b.products.events);
    assert_eq!(a.products.scores, b.products.scores);
    assert_eq!(a.products.provenance, b.products.provenance);
    assert_eq!(a.products.dropped_in_flight_messages, b.products.dropped_in_flight_messages);

    // The per-container provenance is keyed per instance: both containers report a binding hash.
    for container in CONTAINER_PARTS {
        let prov = a.products.trajectories[container].provenance.as_ref().expect("container trajectory provenance");
        let binding_hash = prov.attributes.get("container_binding_hash").unwrap_or_else(|| panic!("{container}: container_binding_hash"));
        println!("{container}: container_binding_hash={binding_hash}");
    }
}

/// The two containers are two distinct processes fed by their OWN spacecraft: the first torque
/// each returns is `kp * q_z` of its own star tracker's measurement, and the traffic never shows
/// one container's frames on the other's path.
#[test]
fn each_container_is_fed_by_and_commands_its_own_spacecraft() {
    if skip_if_cfs_image_unavailable("each_container_is_fed_by_and_commands_its_own_spacecraft") {
        return;
    }
    let p = pair_runs();
    let log = decode_log(&p.run_a.sidecar);
    let map = apid_map(&p.systems);

    let mut first_tau3 = BTreeMap::new();
    for sc in SPACECRAFT {
        let controller = format!("{sc}_controller");
        let attitude = format!("{sc}_attitude");
        let theta0 = declared_initial_error_rad(&p.systems, &p.sos, sc);
        assert!((theta0 - theta0_rad(sc)).abs() < 1e-12, "{sc}: the SoS declares theta0={theta0}, the test expects {}", theta0_rad(sc));

        // IN: the star tracker frames carry THIS spacecraft's attitude (q_z ~ sin(theta0/2)).
        let star_in = records(&log, &controller, "startracker_in", PortDirection::In);
        assert!(!star_in.is_empty(), "{controller} receives star tracker frames");
        let qz0 = num(&decode(&map, star_in[0]), "qz");
        assert!((qz0 - (theta0 / 2.0).sin()).abs() < 1e-3, "{controller}: the first star tracker frame carries q_z={qz0}, expected ~sin({theta0}/2)={}", (theta0 / 2.0).sin());

        // OUT: the first torque is kp * q_z of its own measurement.
        let outs = records(&log, &controller, "wheel_torque_out", PortDirection::Out);
        assert!(outs.len() >= (DURATION_S * 10) as usize - 5, "{controller}: one torque command per 10 Hz step expected, got {}", outs.len());
        let tau3 = num(&decode(&map, outs[0]), "tau_3");
        // tau_z = kp * qv_z (+ kd * omega_z, ~0 at rest): drms/demo_attitude_control.drm.yaml's header.
        let expected = KP * (theta0 / 2.0).sin();
        assert!((tau3 - expected).abs() < 2e-4, "{controller}: first tau_3={tau3} N m, expected kp*sin(theta0/2)={expected}");
        first_tau3.insert(sc, tau3);

        // The torque frames reach the plant of the same spacecraft, byte for byte, and no other.
        let plant_in: BTreeSet<Vec<u8>> = records(&log, &attitude, "wheel_torque_in", PortDirection::In).iter().map(|r| r.payload.clone()).collect();
        let own_out: BTreeSet<Vec<u8>> = outs.iter().map(|r| r.payload.clone()).collect();
        let other = if sc == "chaser" { "target" } else { "chaser" };
        let other_out: BTreeSet<Vec<u8>> = records(&log, &format!("{other}_controller"), "wheel_torque_out", PortDirection::Out).iter().map(|r| r.payload.clone()).collect();
        assert!(!plant_in.is_empty(), "{attitude} receives torque frames");
        assert!(plant_in.is_subset(&own_out), "{attitude}: every torque frame it receives must be one its own controller sent");
        assert!(plant_in.is_disjoint(&other_out), "{attitude}: no torque frame of the other spacecraft's controller may reach it");
    }
    assert!(first_tau3["chaser"].abs() / first_tau3["target"].abs() > 1.5, "the loops are distinguishable at the first command: chaser {} vs target {}", first_tau3["chaser"], first_tau3["target"]);
}

// ------------------------------------------------------------------------------------------
// Proof 2: each container's loop tracks its native counterpart (and the closed form).
// ------------------------------------------------------------------------------------------

fn run_native_pair(label: &str, sos: &SosConfiguration) -> RunProducts {
    let _engine = gmat_sys::engine_lock();
    let systems = native_systems();
    let drm = two_sc_drm(&format!("two_spacecraft_native_{label}_drm"), &sos.id, DURATION_S);
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    execute(run_config(&gmat, &drm, sos, &systems, &format!("test-run-two-sc-native-{label}"), None)).unwrap_or_else(|e| panic!("the native two-spacecraft run {label} must execute: {e:?}"))
}

#[test]
fn each_containers_loop_tracks_its_native_counterpart() {
    if skip_if_cfs_image_unavailable("each_containers_loop_tracks_its_native_counterpart") {
        return;
    }
    let p = pair_runs();
    let native = run_native_pair("tracking", &native_sos("two_spacecraft_native_tracking_sos"));
    let rows = check_tracking(&p.run_a.products, &native, theta0_rad).unwrap_or_else(|e| panic!("the tracking proof failed: {e}"));
    for r in &rows {
        println!("{}: closed form theta(30s)={:.6} rad, native={:.6} rad, container={:.6} rad (|container-native|/native={:.2}%)", r.sc, r.expected_theta, r.native_theta, r.cfs_theta, (r.cfs_theta - r.native_theta).abs() / r.native_theta * 100.0);
    }
    // The two spacecraft really are different loops in this very run.
    let (c, t) = (&rows[0], &rows[1]);
    assert!((c.cfs_theta - t.cfs_theta).abs() / c.cfs_theta > 0.3, "the chaser ({}) and target ({}) end states must differ by >30%", c.cfs_theta, t.cfs_theta);
    assert!((c.native_theta - t.native_theta).abs() / c.native_theta > 0.3);
}

/// The native run itself against the closed form with no tolerance tuned to pass: also run on its
/// own, Docker-free, so it executes where the cFS image is unavailable.
#[test]
fn the_native_two_spacecraft_run_follows_the_closed_form() {
    let sos = native_sos("two_spacecraft_native_closed_form_sos");
    let systems = native_systems();
    for sc in SPACECRAFT {
        assert!((declared_initial_error_rad(&systems, &sos, sc) - theta0_rad(sc)).abs() < 1e-12);
    }
    let native = run_native_pair("closed-form", &sos);
    for sc in SPACECRAFT {
        let expected = theta0_rad(sc) * expected_theta_ratio(DURATION_S as f64);
        let theta = truth_theta_at_end(&native, sc);
        println!("native {sc}: theta({DURATION_S}s)={theta:.6} rad, closed form {expected:.6} rad ({:.3}%)", (theta - expected) / expected * 100.0);
        assert!((theta - expected).abs() / expected < 0.10, "{sc}: native theta={theta} vs closed form {expected}");
    }
}

// ------------------------------------------------------------------------------------------
// The thruster's second FRAMED OUT port (question 254, native open items, task 7).
// ------------------------------------------------------------------------------------------

/// What each spacecraft's thruster reports on `tm_out` (apid 702, a second FRAMED OUT port beside
/// `ack_out`; connected to nothing, so it is read from the port-traffic log's OUT records):
/// one packet per 1 Hz step, carrying the thruster's own position (the closed form from the REAL
/// applied epoch, as its trajectory does) and the throttle (`accel_scale`) in force. The chaser's
/// reads 1.0 up to the applied epoch and [`COMMANDED_ACCEL_SCALE`] after it (exactly one change); the
/// target's, never commanded, reads 1.0 throughout. Nothing is received on any `tm_out`.
fn check_thruster_throttle_telemetry(products: &RunProducts, log: &PortTrafficLog, systems: &BTreeMap<String, SystemDefinition>, sos: &SosConfiguration) {
    let map = apid_map(systems);
    let applied: Vec<_> = products.events.iter().filter(|e| e.kind == EventKind::PortCommand as i32 && e.name == "accel_scale").collect();
    assert_eq!(applied.len(), 1, "exactly one applied accel_scale command");
    let t_apply_ns = applied[0].tai_ns;
    let t_apply_s = (t_apply_ns - START_TAI_NS) as f64 / 1e9;
    for sc in SPACECRAFT {
        let thruster = format!("{sc}_thruster");
        let out = records(log, &thruster, "tm_out", PortDirection::Out);
        assert_eq!(out.len(), DURATION_S as usize, "{thruster}: one throttle packet per 1 Hz step");
        assert!(records(log, &thruster, "tm_out", PortDirection::In).is_empty(), "{thruster}: tm_out is connected to nothing");
        let kin = FlightKinematics::of(systems, sos, &thruster);
        let command = if sc == "chaser" { Some((t_apply_s, COMMANDED_ACCEL_SCALE)) } else { None };
        let (mut before, mut after, mut worst) = (Vec::new(), Vec::new(), 0.0_f64);
        for r in &out {
            let pkt = decode(&map, r);
            assert_eq!(pkt.apid, 702, "{thruster}: the throttle telemetry packet");
            // A packet stamped after the applied epoch comes from a step that applied the command.
            (if r.tai_ns > t_apply_ns { &mut after } else { &mut before }).push(num(&pkt, "accel_scale"));
            let want = kin.position((r.tai_ns - START_TAI_NS) as f64 / 1e9, command);
            for (k, f) in ["x", "y", "z"].iter().enumerate() {
                worst = worst.max((num(&pkt, f) - want[k]).abs());
            }
        }
        println!("{thruster}: tm_out throttle {} packets at {:?}, {} at {:?} (applied at t={t_apply_s} s); worst |position - closed form| = {worst:.3e} m", before.len(), before.first(), after.len(), after.first());
        assert!(worst < 1e-6, "{thruster}: the telemetry position follows the closed form (worst {worst} m)");
        assert!(before.iter().all(|v| *v == 1.0), "{thruster}: the throttle before the command is the old value 1.0: {before:?}");
        if sc == "chaser" {
            assert!(!before.is_empty() && !after.is_empty(), "the chaser has packets on both sides of the command");
            assert!(after.iter().all(|v| *v == COMMANDED_ACCEL_SCALE), "the chaser's throttle after the command is {COMMANDED_ACCEL_SCALE}: {after:?}");
        } else {
            assert!(after.iter().all(|v| *v == 1.0), "the target's throttle never changes: {after:?}");
        }
    }
}

/// [`run_native_pair`] with the port-traffic log read back.
fn run_native_pair_logged(label: &str, sos: &SosConfiguration) -> (RunProducts, PortTrafficLog) {
    let _engine = gmat_sys::engine_lock();
    let systems = native_systems();
    let drm = two_sc_drm(&format!("two_spacecraft_native_{label}_drm"), &sos.id, DURATION_S);
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let dir = scratch_dir(label);
    let products = execute(run_config(&gmat, &drm, sos, &systems, &format!("test-run-two-sc-native-{label}"), Some(dir.clone()))).unwrap_or_else(|e| panic!("the native two-spacecraft run {label} must execute: {e:?}"));
    let sidecar = std::fs::read(dir.join("port_traffic.pb")).expect("port_traffic.pb");
    let _ = std::fs::remove_dir_all(&dir);
    (products, decode_log(&sidecar))
}

/// The thruster's second FRAMED OUT port, Docker-free: the chaser's throttle telemetry shows the
/// telecommand's change (old value before, new value after), the target's does not move, and the
/// ack on the first FRAMED OUT port is unaffected. Perturbation (a clone whose model writes a
/// constant into the field): the before/after assertions fail.
#[test]
fn the_chaser_thruster_reports_the_throttle_change_on_a_second_framed_port() {
    let sos = native_sos("two_spacecraft_native_throttle_sos");
    let (products, log) = run_native_pair_logged("throttle", &sos);
    check_thruster_throttle_telemetry(&products, &log, &native_systems(), &sos);
    assert_eq!(records(&log, "chaser_thruster", "ack_out", PortDirection::Out).len(), 1, "the ack still goes out on its own port");
    assert_eq!(records(&log, "target_thruster", "ack_out", PortDirection::Out).len(), 0);
}

/// A declaration that cannot be resolved stays a typed, load-time refusal naming the instance:
/// the thruster system with a second telemetry-shaped codec (two codecs fit `tm_out`, nothing says
/// which) is refused by `execute` before any step, as `DrmError::SensorPortConfiguration`.
#[test]
fn an_ambiguous_second_framed_out_port_declaration_is_refused_with_a_typed_error() {
    let sos = native_sos("two_spacecraft_native_ambiguous_sos");
    let mut systems = native_systems();
    let thruster = systems.get_mut("two_spacecraft_thruster_sys").expect("the thruster system");
    let mut second = thruster.packet_codecs.iter().find(|c| c.apid == 702).expect("the telemetry codec").clone();
    (second.id, second.apid) = ("thruster_tm_out_codec_2".to_string(), 703);
    thruster.packet_codecs.push(second);
    thruster.hash = hash::canonical_system_hash(thruster);
    let _engine = gmat_sys::engine_lock();
    let drm = two_sc_drm("two_spacecraft_native_ambiguous_drm", &sos.id, DURATION_S);
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let err = execute(run_config(&gmat, &drm, &sos, &systems, "test-run-two-sc-native-ambiguous", None)).expect_err("two codecs fitting tm_out must be refused");
    match err {
        DrmError::SensorPortConfiguration { instance, reason } => {
            assert!(instance.ends_with("_thruster"), "the refusal names a thruster instance: {instance}");
            assert!(reason.contains("found 2"), "{reason}");
        }
        other => panic!("expected DrmError::SensorPortConfiguration, got {other:?}"),
    }
}

// ------------------------------------------------------------------------------------------
// Both spacecraft on ONE ground station (question 254, native open items, task 6).
// ------------------------------------------------------------------------------------------

/// The one-station DRM's own window, `drms/demo_two_spacecraft_one_station.drm.yaml`: 31 s, one second
/// longer than the two-station DRM's [`DURATION_S`], so that every frame each spacecraft sends in its
/// first 30 s is handled (a frame sent at `s` is available at `s + 100 ms` and handled by the station
/// step ending at `s + 1 s`; the frame sent at 31 s is the one still in flight when the window closes).
const ONE_STATION_DURATION_S: i64 = 31;
/// Telemetry frames each spacecraft sends over that window (one per 1 Hz flight step).
const FRAMES_SENT_PER_SPACECRAFT: usize = ONE_STATION_DURATION_S as usize;
/// The frames each spacecraft sends in its first 30 s: all of them are handled.
const FRAMES_FIRST_30_S: usize = 30;

/// `systems` plus the station system of the one-station DRM (`drms/demo_ground_segment_ground_multi.
/// system.yaml`, which declares `output.frames_handled`).
fn with_one_station_ground(mut systems: BTreeMap<String, SystemDefinition>) -> BTreeMap<String, SystemDefinition> {
    let ground = load_system("demo_ground_segment_ground_multi");
    systems.insert(ground.id.clone(), ground);
    systems
}

/// `drms/demo_two_spacecraft_one_station.sos.yaml`, both controllers `BINDING_KIND_MODEL`. `without`:
/// leave that spacecraft's telemetry connection out (in memory), so the station has one sender -- the
/// baseline each spacecraft's own count is read from.
fn one_station_sos(id: &str, without: Option<&str>) -> SosConfiguration {
    let mut sos = schema::parse_sos_yaml(&read("demo_two_spacecraft_one_station.sos.yaml")).expect("demo_two_spacecraft_one_station.sos.yaml parses");
    sos.id = id.to_string();
    if let Some(sc) = without {
        let flight = format!("{sc}_flight");
        sos.connections.retain(|c| !(c.from_instance == flight && c.from_port == "tm_out"));
    }
    sos.hash = hash::canonical_sos_hash(&sos);
    sos
}

fn one_station_drm(id: &str, sos_id: &str) -> DesignReferenceMission {
    let mut drm = schema::parse_drm_yaml(&read("demo_two_spacecraft_one_station.drm.yaml")).expect("demo_two_spacecraft_one_station.drm.yaml parses");
    let sc = drm.scenario.as_ref().expect("scenario");
    assert_eq!(sc.end_tai_ns - sc.start_tai_ns, ONE_STATION_DURATION_S * 1_000_000_000, "the DRM file declares the {ONE_STATION_DURATION_S} s window this test is written for");
    drm.id = id.to_string();
    drm.sos_configuration_id = sos_id.to_string();
    drm.hash = hash::canonical_drm_hash(&drm);
    drm
}

/// A native run of the one-station DRM (the station is independent of how the controllers are bound).
fn one_station_run(label: &str, without: Option<&str>) -> (RunProducts, PortTrafficLog, BTreeMap<String, SystemDefinition>, SosConfiguration) {
    let systems = with_one_station_ground(native_systems());
    let sos = one_station_sos("two_spacecraft_one_station_sos", without);
    let drm = one_station_drm("two_spacecraft_one_station_drm", &sos.id);
    let dir = scratch_dir(label);

    let _engine = gmat_sys::engine_lock();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    // One run id for every call (the id is in every event's provenance): two runs compare equal.
    let products = execute(run_config(&gmat, &drm, &sos, &systems, "test-run-two-sc-one-station", Some(dir.clone()))).unwrap_or_else(|e| panic!("the one-station two-spacecraft run {label} must execute: {e:?}"));
    let sidecar = std::fs::read(dir.join("port_traffic.pb")).expect("port_traffic.pb");
    let _ = std::fs::remove_dir_all(&dir);
    (products, decode_log(&sidecar), systems, sos)
}

fn declared_port_latency_ns(systems: &BTreeMap<String, SystemDefinition>, sos: &SosConfiguration, instance: &str, port: &str) -> i64 {
    let inst = sos.instances.iter().find(|i| i.name == instance).unwrap_or_else(|| panic!("instance {instance}"));
    let p = systems[&inst.system_id].ports.iter().find(|p| p.name == port).unwrap_or_else(|| panic!("port {port} of {instance}"));
    p.timing.as_ref().map(|t| t.latency_ns).unwrap_or(0)
}

/// What a run of the one-station DRM with BOTH spacecraft must show, native or container-bound alike
/// (the station and the flight computers are native in both). Returns the station's own count,
/// `output.ground.frames_handled@end`.
///
/// - **Sent and carried:** each flight sends 31 frames; the router carries all 31 + 31 to `ground.tm_in`.
/// - **Available:** a frame sent at `s` is available at `s` plus the declared link latency (the
///   flight's `tm_out` plus the station's `tm_in`, read from the systems, 100 ms); of each spacecraft's
///   31, the 30 sent in the first 30 s are available before the window closes and the 31st (sent at
///   31 s) is still in flight.
/// - **Handled:** the station's count is 60 = 30 + 30, so no available frame was dropped, and, since a
///   spacecraft cannot have more frames handled than were available, 30 from each.
/// - **Per sender:** one CONTACT_START each, attributed, unique ids, no decode errors; one AOS ack each.
fn check_one_station(products: &RunProducts, log: &PortTrafficLog, systems: &BTreeMap<String, SystemDefinition>, sos: &SosConfiguration) -> f64 {
    let map = apid_map(systems);
    let end_ns = START_TAI_NS + ONE_STATION_DURATION_S * 1_000_000_000;
    let latency_ns = declared_port_latency_ns(systems, sos, "chaser_flight", "tm_out") + declared_port_latency_ns(systems, sos, "ground", "tm_in");
    assert_eq!(latency_ns, 100_000_000, "the declared link latency");

    // --- Sent and carried: 31 frames from each, told apart by payload (the streams are distinct).
    let delivered = records(log, "ground", "tm_in", PortDirection::In);
    let mut sent_by: BTreeMap<&str, BTreeSet<Vec<u8>>> = BTreeMap::new();
    for sc in SPACECRAFT {
        let out = records(log, &format!("{sc}_flight"), "tm_out", PortDirection::Out);
        assert_eq!(out.len(), FRAMES_SENT_PER_SPACECRAFT, "{sc}_flight sends one position packet per 1 Hz step");
        sent_by.insert(sc, out.iter().map(|r| r.payload.clone()).collect());
    }
    assert!(sent_by["chaser"].is_disjoint(&sent_by["target"]), "the two telemetry streams are distinct");
    assert_eq!(delivered.len(), 2 * FRAMES_SENT_PER_SPACECRAFT, "the router carries every frame of both spacecraft to the one station port");
    println!("dropped in flight at the end of the run (all instances): {}", products.dropped_in_flight_messages);

    // --- Available before the window closed, per sender.
    let mut available = BTreeMap::new();
    for sc in SPACECRAFT {
        let mine: Vec<&&PortTrafficRecord> = delivered.iter().filter(|r| sent_by[sc].contains(&r.payload)).collect();
        let in_time = mine.iter().filter(|r| r.tai_ns + latency_ns <= end_ns).count();
        let in_flight: Vec<f64> = mine.iter().filter(|r| r.tai_ns + latency_ns > end_ns).map(|r| (r.tai_ns - START_TAI_NS) as f64 / 1e9).collect();
        println!("{sc}: sent {}, carried {}, available before the window closed {in_time}, still in flight at the end: sent at {in_flight:?} s", FRAMES_SENT_PER_SPACECRAFT, mine.len());
        assert_eq!(mine.len(), FRAMES_SENT_PER_SPACECRAFT, "{sc}: all 31 frames are carried to the station");
        assert_eq!(in_time, FRAMES_FIRST_30_S, "{sc}: the 30 frames sent in the first 30 s are available before the window closes");
        assert_eq!(in_flight, vec![ONE_STATION_DURATION_S as f64], "{sc}: only the frame sent at 31 s is still in flight");
        available.insert(sc, in_time as f64);
    }

    // --- Handled: the station's own count.
    let handled = products.scores.get("ground_frames_handled").expect("the DRM's ground_frames_handled measure is scored").value;
    println!("ground.frames_handled@end = {handled} (available: chaser {}, target {})", available["chaser"], available["target"]);
    assert_eq!(handled, available["chaser"] + available["target"], "the station handled every frame that reached it, from both spacecraft: 30 + 30");
    assert_eq!(handled, (2 * FRAMES_FIRST_30_S) as f64);

    // --- Per sender: one CONTACT_START each (state is kept per sender: with one shared state the
    //     second spacecraft's rise would be invisible), none ended, and no frame was undecodable.
    let mut starters = BTreeMap::new();
    for e in products.events.iter().filter(|e| e.entity_id == "ground") {
        if e.kind == EventKind::ContactStart as i32 || e.kind == EventKind::ContactEnd as i32 {
            let sender = e.provenance.as_ref().and_then(|p| p.attributes.get("sender")).cloned().unwrap_or_default();
            println!("ground: {} sender={sender:?} at t={} s id={}", e.name, (e.tai_ns - START_TAI_NS) as f64 / 1e9, e.id);
            assert_eq!(e.kind, EventKind::ContactStart as i32, "the pass does not set inside the window: {e:?}");
            assert!(starters.insert(sender, e.tai_ns).is_none(), "one CONTACT_START per sender");
            assert!(e.detail.contains(&e.provenance.as_ref().unwrap().attributes["sender"]), "the event detail names the sender: {}", e.detail);
        }
    }
    assert_eq!(starters.keys().map(String::as_str).collect::<Vec<_>>(), vec!["chaser_flight", "target_flight"], "each spacecraft's rise is reported, attributed to it");
    for (sender, tai_ns) in &starters {
        let t = (tai_ns - START_TAI_NS) as f64 / 1e9;
        assert!((6.0..=8.0).contains(&t), "{sender}: AOS expected at 7 s (closed-form mask crossing 5.19 s target / 5.59 s chaser; the first frame above the mask is sent at 6 s, arrives at 6.1 s, is handled by the step ending at 7 s), got {t}");
    }
    let distinct_ids: BTreeSet<&str> = products.events.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(distinct_ids.len(), products.events.len(), "every event id is unique, two contact starts at one epoch included");
    assert!(products.events.iter().all(|e| !e.name.starts_with("decode_error")), "no telemetry frame is undecodable");

    // --- The station acknowledges each AOS on tc_out: one ack per sender's rising edge, sequenced.
    let acks = records(log, "ground", "tc_out", PortDirection::Out);
    let seqs: Vec<u16> = acks.iter().map(|r| decode(&map, r).sequence_count).collect();
    println!("ground.tc_out: {} AOS acknowledgments, sequence counts {seqs:?}", acks.len());
    assert_eq!(seqs, vec![0, 1], "one AOS acknowledgment per spacecraft, in sequence");
    handled
}

/// **Question 254, task 6 (native).** Both spacecraft send their telemetry to ONE ground station and
/// the station handles every frame of each: 30 of the 30 frames each sends in the first 30 s (the 31 s
/// window leaves only each spacecraft's 31st frame in flight, [`check_one_station`]). Before the
/// station processed every frame on its port (`GroundStationModel::step_with_ports` read only
/// `Inbox::last_on_port`), two frames reaching `tm_in` in one step left only the last handled, so
/// 30 of the 60 and none of the chaser's. The port-traffic log records what the router CARRIED (31 +
/// 31 frames either way), so the proof reads the station's own count (`output.ground.frames_handled@
/// end`, the DRM's `ground_frames_handled` measure), and compares it with what each spacecraft alone
/// gets handled (the same DRM with the other's telemetry connection left out: one sender).
#[test]
fn both_spacecraft_on_one_ground_station_are_each_handled_in_full() {
    let (products, log, systems, sos) = one_station_run("handled", None);
    let handled = check_one_station(&products, &log, &systems, &sos);

    let mut alone = BTreeMap::new();
    for sc in SPACECRAFT {
        let other = if sc == "chaser" { "target" } else { "chaser" };
        let (solo, ..) = one_station_run(&format!("solo-{sc}"), Some(other));
        alone.insert(sc, solo.scores["ground_frames_handled"].value);
    }
    println!("ground.frames_handled@end: {handled} with both spacecraft; chaser alone {}, target alone {}", alone["chaser"], alone["target"]);
    assert_eq!((alone["chaser"], alone["target"]), (FRAMES_FIRST_30_S as f64, FRAMES_FIRST_30_S as f64), "alone, each spacecraft gets 30 of its first 30 frames handled");
    assert_eq!(handled, alone["chaser"] + alone["target"], "sharing a station loses nothing: both together are handled as often as each alone (before the fix: only the last frame of each step, half)");
}

/// The same run twice gives byte-identical products and port traffic: the per-sender handling is
/// deterministic (the router's order, not hash order), the property question 108 exists for.
#[test]
fn the_one_station_run_is_deterministic() {
    let (a, log_a, ..) = one_station_run("det-a", None);
    let (b, log_b, ..) = one_station_run("det-b", None);
    // (`assert!`, not `assert_eq!`: a mismatch must not print two whole event lists.)
    assert!(a.events == b.events, "events differ between two runs of the same DRM");
    assert!(a.scores == b.scores, "scores differ between two runs of the same DRM");
    assert!(log_a.records == log_b.records, "port traffic differs between two runs of the same DRM");
    for (name, ta) in &a.trajectories {
        assert!(ta.samples == b.trajectories[name].samples, "{name}: samples differ between two runs");
    }
}

/// **Question 254, task 6 (container-bound).** The two-spacecraft DRM of this file, each controller
/// through its own cFS container exactly as the two-station tests bind them, with both spacecraft on
/// ONE ground station: the same delivered/available/handled counts and per-sender CONTACT_STARTs as the
/// native run ([`check_one_station`]), and each container really ran (both were seen live by their
/// labels, both returned torque frames). Takes the engine and docker locks as the other docker tests
/// do, and leaves nothing behind.
#[test]
fn both_spacecraft_through_their_own_containers_on_one_ground_station_are_each_handled_in_full() {
    if skip_if_cfs_image_unavailable("both_spacecraft_through_their_own_containers_on_one_ground_station_are_each_handled_in_full") {
        return;
    }
    const RUN_ID: &str = "test-run-two-sc-one-station-cfs";
    let _engine = gmat_sys::engine_lock();
    let (lock, docker_run_id) = lock_and_prune_docker_tests();
    let systems = with_one_station_ground(container_systems());
    let cfs_image_id = docker_lines(&["inspect", "--format", "{{.Id}}", &cfs_common::cfs_image()]).into_iter().next().expect("the cFS image is present (the gate checked)");
    let before = DockerSnapshot::take();
    let registry_volumes;
    let (sos, run, seen);
    {
        let mut volume_guard = RegistryVolumeGuard(Vec::new());
        let (image, digest, registry_guards) = push_cfs_image_to_local_registry(&docker_run_id);
        volume_guard.0 = container_volumes(&registry_guards.0 .0);
        registry_volumes = volume_guard.0.clone();
        sos = rebind_controllers_to_containers(one_station_sos("two_spacecraft_one_station_cfs_sos", None), &image, &digest, &ContainerVariant::default());
        let drm = one_station_drm("two_spacecraft_one_station_cfs_drm", &sos.id);
        let dir = scratch_dir("one-station-cfs");
        let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
        let watcher = LabelWatcher::start(RUN_ID);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| execute_container_run(&gmat, &drm, &sos, &systems, RUN_ID, &dir, "one-station")));
        seen = watcher.finish();
        run = result.unwrap_or_else(|p| std::panic::resume_unwind(p));
        drop(registry_guards);
        drop(volume_guard);
    }
    let after = DockerSnapshot::take();
    let leftovers = Leftovers::take(&before, &cfs_image_id, RUN_ID);
    drop(lock);

    let log = decode_log(&run.sidecar);
    assert_eq!(seen.iter().map(|c| c.instance.as_str()).collect::<BTreeSet<_>>(), BTreeSet::from(["chaser_controller", "target_controller"]), "both controllers ran in their own containers: {seen:?}");
    for controller in CONTAINER_PARTS {
        assert!(!records(&log, controller, "wheel_torque_out", PortDirection::Out).is_empty(), "{controller} returned torque frames");
    }
    check_one_station(&run.products, &log, &systems, &sos);
    assert_nothing_left(&before, &after, &leftovers, RUN_ID, &registry_volumes);
}

// ------------------------------------------------------------------------------------------
// Proof 4: the ground segment receives both spacecraft; the telecommand reaches only the chaser.
// ------------------------------------------------------------------------------------------

#[test]
fn the_ground_receives_both_spacecraft_and_the_telecommand_reaches_only_the_chaser() {
    if skip_if_cfs_image_unavailable("the_ground_receives_both_spacecraft_and_the_telecommand_reaches_only_the_chaser") {
        return;
    }
    let p = pair_runs();
    let products = &p.run_a.products;
    let log = decode_log(&p.run_a.sidecar);
    let map = apid_map(&p.systems);

    // --- The telecommand: five transitions for cmd1, one applied command, on the chaser only.
    let states: Vec<&str> = products.events.iter().filter(|e| e.kind == EventKind::CommandTransition as i32 && e.reference_id == "cmd1").map(|e| e.name.as_str()).collect();
    assert_eq!(states, vec!["COMMAND_STATE_PROPOSED", "COMMAND_STATE_CHECKED", "COMMAND_STATE_AUTHORIZED", "COMMAND_STATE_DISPATCHED", "COMMAND_STATE_ACKED"]);
    // (The attitude plants also report one PORT_COMMAND per received torque frame, field `tau_mag`.)
    let port_commands: Vec<_> = products.events.iter().filter(|e| e.kind == EventKind::PortCommand as i32 && e.name == "accel_scale").collect();
    println!("accel_scale PORT_COMMAND events: {:?}", port_commands.iter().map(|e| (&e.entity_id, &e.name, e.values.get("value"), e.tai_ns)).collect::<Vec<_>>());
    assert_eq!(port_commands.len(), 1, "exactly one applied accel_scale command in the whole run: {port_commands:#?}");
    // The torque PORT_COMMANDs name their sender: each plant applies only its own controller's.
    for sc in SPACECRAFT {
        let plant = format!("{sc}_attitude");
        let applied: Vec<_> = products.events.iter().filter(|e| e.kind == EventKind::PortCommand as i32 && e.entity_id == plant).collect();
        assert!(!applied.is_empty(), "{plant} applied torque commands");
        let wanted = format!("(sender \"{sc}_controller\")");
        assert!(applied.iter().all(|e| e.detail.contains(&wanted)), "{plant}: every applied torque must come from {sc}_controller");
    }
    let applied = port_commands[0];
    assert_eq!((applied.entity_id.as_str(), applied.name.as_str(), applied.values.get("value").copied()), ("chaser_thruster", "accel_scale", Some(COMMANDED_ACCEL_SCALE)));
    assert!(applied.tai_ns > COMMAND_TAI_NS, "applied only after the declared link latency");
    assert!(products.events.iter().all(|e| !(e.kind == EventKind::PortCommand as i32 && e.entity_id == "target_thruster")), "no PORT_COMMAND on the target");
    let t_apply_s = (applied.tai_ns - START_TAI_NS) as f64 / 1e9;

    // --- The wire: dispatched once, delivered to the chaser only; one ack, from the chaser only.
    assert_eq!(records(&log, "ground_cmd", "cmd_out", PortDirection::Out).len(), 1, "the dispatch instance sends one telecommand");
    assert_eq!(records(&log, "chaser_thruster", "cmd_in", PortDirection::In).len(), 1, "the chaser's thruster receives it");
    assert_eq!(records(&log, "target_thruster", "cmd_in", PortDirection::In).len(), 0, "the target's thruster receives nothing on cmd_in");
    assert_eq!(records(&log, "chaser_thruster", "ack_out", PortDirection::Out).len(), 1, "the chaser acknowledges");
    assert_eq!(records(&log, "target_thruster", "ack_out", PortDirection::Out).len(), 0, "the target sends no ack");
    assert_eq!(records(&log, "ground_cmd", "ack_in", PortDirection::In).len(), 1, "the ground receives the chaser's ack");
    let cmd_in = decode(&map, records(&log, "chaser_thruster", "cmd_in", PortDirection::In)[0]);
    assert_eq!((cmd_in.apid, cmd_in.is_command, num(&cmd_in, "value")), (700, true, COMMANDED_ACCEL_SCALE));
    let ack = decode(&map, records(&log, "chaser_thruster", "ack_out", PortDirection::Out)[0]);
    assert_eq!((ack.apid, num(&ack, "cmd_seq") as u16), (701, cmd_in.sequence_count), "the ack names the command it acknowledges");

    // --- The thruster's second FRAMED OUT port reports the throttle change (task 7), here too.
    check_thruster_throttle_telemetry(products, &log, &p.systems, &p.sos);

    // --- The physical effect, per spacecraft, against the closed form from the REAL applied epoch:
    //     the chaser's thruster integrates 4x the thrust after it; the target's never changes.
    for sc in SPACECRAFT {
        let thruster = format!("{sc}_thruster");
        let kin = FlightKinematics::of(&p.systems, &p.sos, &thruster);
        let command = if sc == "chaser" { Some((t_apply_s, COMMANDED_ACCEL_SCALE)) } else { None };
        let mut worst: f64 = 0.0;
        for s in &products.trajectories[&thruster].samples {
            let want = kin.position((s.tai_ns - START_TAI_NS) as f64 / 1e9, command);
            for (k, w) in want.iter().enumerate() {
                worst = worst.max((s.mean[k] - w).abs());
            }
        }
        let last = products.trajectories[&thruster].samples.last().unwrap();
        let uncommanded = kin.position((last.tai_ns - START_TAI_NS) as f64 / 1e9, None);
        let moved = (0..3).map(|k| (last.mean[k] - uncommanded[k]).powi(2)).sum::<f64>().sqrt();
        println!("{thruster}: worst |trajectory - closed form| = {worst:.3e} m; final displacement from the uncommanded path = {moved:.4} m");
        assert!(worst < 1e-6, "{thruster}: must follow the closed form (worst {worst} m)");
        if sc == "chaser" {
            assert!(moved > 5.0, "the telecommand must move the chaser's thruster trajectory measurably ({moved} m)");
        } else {
            assert!(moved < 1e-9, "the target's thruster is untouched by the telecommand ({moved} m)");
        }
    }

    // --- Telemetry: both spacecraft reach their station over FRAMED links, with their own position.
    for sc in SPACECRAFT {
        let flight = format!("{sc}_flight");
        let station = format!("ground_{sc}");
        let kin = FlightKinematics::of(&p.systems, &p.sos, &flight);
        let out = records(&log, &flight, "tm_out", PortDirection::Out);
        let inn = records(&log, &station, "tm_in", PortDirection::In);
        println!("{sc}: {} telemetry packets sent, {} received by {station}", out.len(), inn.len());
        assert_eq!(out.len(), DURATION_S as usize, "{flight}: one position packet per 1 Hz step");
        assert!(!inn.is_empty() && inn.len() <= out.len(), "{station} receives {sc}'s telemetry ({} of {})", inn.len(), out.len());
        let sent: BTreeSet<Vec<u8>> = out.iter().map(|r| r.payload.clone()).collect();
        assert!(inn.iter().all(|r| sent.contains(&r.payload)), "{station}: every received packet is one {flight} sent");
        let other = if sc == "chaser" { "target" } else { "chaser" };
        let other_sent: BTreeSet<Vec<u8>> = records(&log, &format!("{other}_flight"), "tm_out", PortDirection::Out).iter().map(|r| r.payload.clone()).collect();
        assert!(sent.is_disjoint(&other_sent), "the two spacecraft's telemetry streams are distinct");
        // Every packet is the coasting closed-form position at its epoch.
        let mut worst: f64 = 0.0;
        for r in &out {
            let t = (r.tai_ns - START_TAI_NS) as f64 / 1e9;
            let pkt = decode(&map, r);
            let want = kin.position(t, None);
            for (k, f) in ["x", "y", "z"].iter().enumerate() {
                worst = worst.max((num(&pkt, f) - want[k]).abs());
            }
        }
        println!("{sc}: worst |telemetry - closed form| = {worst:.3e} m");
        assert!(worst < 1e-5, "{sc}: telemetry must follow the closed form (worst {worst} m)");
    }

    // --- Contacts: both stations see their spacecraft rise once, inside the window.
    for sc in SPACECRAFT {
        let station = format!("ground_{sc}");
        let starts: Vec<_> = products.events.iter().filter(|e| e.entity_id == station && e.kind == EventKind::ContactStart as i32).collect();
        let ends = products.events.iter().filter(|e| e.entity_id == station && e.kind == EventKind::ContactEnd as i32).count();
        assert_eq!((starts.len(), ends), (1, 0), "{station}: one CONTACT_START and no CONTACT_END");
        let t = (starts[0].tai_ns - START_TAI_NS) as f64 / 1e9;
        println!("{station}: CONTACT_START at t={t} s");
        assert!((5.0..=8.0).contains(&t), "{station}: AOS expected ~6 s (closed-form mask crossing at 5.19 s chaser/5.59 s ... sampled at 1 Hz), got {t}");
    }
}

// ------------------------------------------------------------------------------------------
// Proof 3: a Docker-free replay of both containers reproduces the products.
// ------------------------------------------------------------------------------------------

#[test]
fn replaying_both_containers_from_the_recorded_traffic_reproduces_the_products_docker_free() {
    if skip_if_cfs_image_unavailable("replaying_both_containers_from_the_recorded_traffic_reproduces_the_products_docker_free") {
        return;
    }
    let p = pair_runs();
    let real = &p.run_a.products;
    let dir = scratch_dir("replay");
    std::fs::create_dir_all(&dir).expect("scratch dir");
    std::fs::write(dir.join("port_traffic.pb"), &p.run_a.sidecar).expect("write the recorded log");

    let _engine = gmat_sys::engine_lock();
    // `instances: vec![]`: question 175's "every BINDING_KIND_CONTAINER instance" default, which
    // here must resolve to both controllers. No docker lock, no registry, no container is touched.
    let replay = ReplayConfig { log_path: dir.join("port_traffic.pb"), expected_hash: real.port_traffic_hash.clone(), instances: vec![] };
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let cfg = RunConfig { replay: Some(replay), ..run_config(&gmat, &p.drm, &p.sos, &p.systems, &p.kernel_run_id, None) };
    let replayed = execute(cfg).expect("the Docker-free replay of both containers must execute");
    let _ = std::fs::remove_dir_all(&dir);

    for name in CONTAINER_PARTS {
        let inst = p.sos.instances.iter().find(|i| i.name == name).unwrap();
        assert_eq!(inst.binding.as_ref().unwrap().kind, BindingKind::Container as i32, "{name} still declares BINDING_KIND_CONTAINER");
    }
    assert_eq!(real.trajectories.keys().collect::<Vec<_>>(), replayed.trajectories.keys().collect::<Vec<_>>());
    for (name, tr) in &real.trajectories {
        let tp = &replayed.trajectories[name];
        let is_container = CONTAINER_PARTS.contains(&name.as_str());
        assert_eq!(tr.samples, tp.samples, "{name}: samples");
        assert_eq!(tr.event_ids, tp.event_ids, "{name}: event_ids");
        assert_eq!(tr.state_space_id, tp.state_space_id, "{name}");
        assert_eq!(tr.segments.len(), tp.segments.len(), "{name}: segment count");
        for (sr, sp) in tr.segments.iter().zip(tp.segments.iter()) {
            assert_eq!((sr.start_tai_ns, sr.end_tai_ns), (sp.start_tai_ns, sp.end_tai_ns), "{name}");
            // Nothing excluded (question 254 item 8): the sidecar records each container's Bind version, so the
            // replayed container segments carry the live dynamics_model/hash/depth.
            assert_eq!((&sr.dynamics_model, &sr.dynamics_hash, &sr.dynamics_depth), (&sp.dynamics_model, &sp.dynamics_hash, &sp.dynamics_depth), "{name}");
            if is_container {
                assert!(!sr.dynamics_hash.is_empty(), "{name}: a container segment carries a dynamics_hash");
            }
        }
        let (pr, pp) = (tr.provenance.as_ref().expect("provenance"), tp.provenance.as_ref().expect("provenance"));
        if is_container {
            let strip = |p: &av_cdm::pb::Provenance| {
                let mut p = p.clone();
                p.attributes.remove("container_binding_hash");
                p
            };
            assert_eq!(strip(pr), strip(pp), "{name}: provenance except container_binding_hash");
        } else {
            assert_eq!(pr, pp, "{name}: provenance");
        }
    }
    assert_eq!(real.events, replayed.events, "no event missing or extra");
    assert_eq!(real.scores, replayed.scores);
    assert_eq!(real.dropped_in_flight_messages, replayed.dropped_in_flight_messages);
}

// ------------------------------------------------------------------------------------------
// Proof 5: labels, and nothing left behind.
// ------------------------------------------------------------------------------------------

#[test]
fn both_containers_carry_the_kernels_labels_while_the_run_is_live() {
    if skip_if_cfs_image_unavailable("both_containers_carry_the_kernels_labels_while_the_run_is_live") {
        return;
    }
    let p = pair_runs();
    let mut by_instance: BTreeMap<&str, &SeenContainer> = BTreeMap::new();
    for c in &p.seen_during_run_a {
        println!("seen: {c:?}");
        by_instance.insert(c.instance.as_str(), c);
    }
    assert_eq!(p.seen_during_run_a.len(), 2, "exactly two containers carried {LABEL_RUN_ID}={} during run A: {:?}", p.kernel_run_id, p.seen_during_run_a);
    assert_eq!(by_instance.keys().copied().collect::<Vec<_>>(), vec!["chaser_controller", "target_controller"]);
    let ids: BTreeSet<&str> = p.seen_during_run_a.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids.len(), 2, "two distinct containers");
    for c in &p.seen_during_run_a {
        assert_eq!(c.run_id, p.kernel_run_id, "{LABEL_RUN_ID}");
        assert_eq!(c.test, "1", "{LABEL_TEST}: declared by the test's SoS");
    }
}

#[test]
fn a_finished_pair_of_runs_leaves_nothing_behind() {
    if skip_if_cfs_image_unavailable("a_finished_pair_of_runs_leaves_nothing_behind") {
        return;
    }
    let p = pair_runs();
    assert_nothing_left(&p.docker_before, &p.docker_after, &p.leftovers, &p.kernel_run_id, &p.registry_volumes);
}

/// What the label and image-ancestry queries find right after a run's registry is torn down. Taken
/// while the docker-test lock is still held (the host is shared: a later query can see another
/// team's labelled containers, which are not this run's leftovers).
#[derive(Debug, Clone)]
struct Leftovers {
    /// Containers still carrying `av.run_id=<run id>` (the kernel's).
    by_run: Vec<String>,
    /// Containers still carrying the `av.test` marker that were not there after the pre-flight sweep.
    by_marker: Vec<String>,
    /// Containers created from the cFS image or `registry:2` since the before-snapshot.
    new_from_images: Vec<(String, Vec<String>)>,
}
impl Leftovers {
    fn take(before: &DockerSnapshot, cfs_image_id: &str, kernel_run_id: &str) -> Self {
        Self {
            by_run: containers_with_label(&format!("{LABEL_RUN_ID}={kernel_run_id}")),
            by_marker: containers_with_label(LABEL_TEST).into_iter().filter(|c| !before.containers.contains(c)).collect(),
            new_from_images: [cfs_image_id, "registry:2"].iter().map(|image| (image.to_string(), containers_from_image(image).into_iter().filter(|c| !before.containers.contains(c)).collect())).collect(),
        }
    }
}

/// The before/after comparison: no container, image tag, volume or network created by the test or
/// the kernel remains. Containers are attributed by label and by image ancestry (a kernel container
/// that lost its labels would still be found by its image); the rest is the set difference against
/// the before-snapshot, restricted where the host is shared with other work (volumes: dangling ones
/// only, since a volume a running container references is somebody's live state).
fn assert_nothing_left(before: &DockerSnapshot, after: &DockerSnapshot, leftovers: &Leftovers, kernel_run_id: &str, registry_volumes: &[String]) {
    assert!(leftovers.by_run.is_empty(), "containers labelled {LABEL_RUN_ID}={kernel_run_id} remain: {:?}", leftovers.by_run);
    assert!(leftovers.by_marker.is_empty(), "containers labelled {LABEL_TEST} created since the before-snapshot remain: {:?}", leftovers.by_marker);
    for (image, new) in &leftovers.new_from_images {
        assert!(new.is_empty(), "containers created from {image} since the before-snapshot remain: {new:?}");
    }
    let new_containers: Vec<_> = after.containers.difference(&before.containers).collect();
    let new_refs: Vec<_> = after.image_refs.difference(&before.image_refs).filter(|r| r.contains("127.0.0.1:") || r.contains("native-ct")).collect();
    let new_dangling: Vec<_> = after.dangling_volumes.difference(&before.dangling_volumes).collect();
    let new_volumes: Vec<_> = after.volumes.difference(&before.volumes).filter(|v| !after.dangling_volumes.contains(*v)).collect();
    let new_networks: Vec<_> = after.networks.difference(&before.networks).collect();
    println!("docker diff (before -> after the runs): new containers {new_containers:?}; new test image refs {new_refs:?}; new dangling volumes {new_dangling:?}; new in-use volumes {new_volumes:?}; new networks {new_networks:?}; registry volumes the guard removed {registry_volumes:?}");
    println!("docker counts after: {} containers, {} image refs, {} volumes ({} dangling), {} networks", after.containers.len(), after.image_refs.len(), after.volumes.len(), after.dangling_volumes.len(), after.networks.len());
    assert!(new_refs.is_empty(), "image tags created by the test remain: {new_refs:?}");
    assert!(new_dangling.is_empty(), "dangling volumes created since the before-snapshot remain: {new_dangling:?}");
    assert!(new_networks.is_empty(), "networks created since the before-snapshot remain: {new_networks:?}");
}

// ------------------------------------------------------------------------------------------
// Failed runs: an early error and a panic after both containers started, and a sweep that finds
// the kernel's containers by label (a killed test's leftovers).
// ------------------------------------------------------------------------------------------

/// A source whose `poll` (called once, after both containers are bound) panics.
struct PanickingSource;
impl ExternalCommandSource for PanickingSource {
    fn poll(&self, _now_tai_ns: i64) -> Vec<av_cdm::pb::Command> {
        panic!("deliberate panic after both containers started");
    }
    fn report(&self, _outcome: av_kernel::drm::CommandOutcome) {}
}

/// A source whose `poll` does what the NEXT test run's pre-flight does: the label sweep
/// `prune_stale_test_resources`, while both kernel containers are running.
struct SweepingSource<'a> {
    lock: &'a DockerTestLock,
    seen_before_sweep: std::sync::Mutex<Vec<String>>,
    seen_after_sweep: std::sync::Mutex<Vec<String>>,
    run_id: String,
}
impl ExternalCommandSource for SweepingSource<'_> {
    fn poll(&self, _now_tai_ns: i64) -> Vec<av_cdm::pb::Command> {
        let filter = format!("{LABEL_RUN_ID}={}", self.run_id);
        *self.seen_before_sweep.lock().unwrap() = containers_with_label(&filter);
        prune_stale_test_resources(self.lock);
        *self.seen_after_sweep.lock().unwrap() = containers_with_label(&filter);
        Vec::new()
    }
    fn report(&self, _outcome: av_kernel::drm::CommandOutcome) {}
}

struct FailedRunFixture {
    systems: BTreeMap<String, SystemDefinition>,
    sos: SosConfiguration,
    drm: DesignReferenceMission,
}

/// Runs `body` with a registry up, the fixture built for `run_id` (the DRM edited by `drm_edit`),
/// a before-snapshot taken and the docker lock held (handed to `body`); `body` returns the
/// containers its label watcher saw. Then tears the registry down, requires that both container
/// instances were seen (the failure really came after both started) and that nothing remains.
fn with_failed_run_harness(run_id: &str, drm_edit: impl FnOnce(&mut DesignReferenceMission), body: impl FnOnce(&DockerTestLock, &FailedRunFixture, &Gmat) -> Vec<SeenContainer>) {
    let _engine = gmat_sys::engine_lock();
    let (lock, docker_run_id) = lock_and_prune_docker_tests();
    let systems = container_systems();
    let cfs_image_id = docker_lines(&["inspect", "--format", "{{.Id}}", &cfs_common::cfs_image()]).into_iter().next().expect("the cFS image is present");
    let before = DockerSnapshot::take();
    let registry_volumes;
    let seen;
    {
        let mut volume_guard = RegistryVolumeGuard(Vec::new());
        let (image, digest, registry_guards) = push_cfs_image_to_local_registry(&docker_run_id);
        volume_guard.0 = container_volumes(&registry_guards.0 .0);
        registry_volumes = volume_guard.0.clone();
        let sos = container_sos("two_spacecraft_cfs_failed_sos", &image, &digest, &ContainerVariant::default());
        let mut drm = two_sc_drm("two_spacecraft_cfs_failed_drm", &sos.id, DURATION_S);
        drm_edit(&mut drm);
        drm.hash = hash::canonical_drm_hash(&drm);
        let fixture = FailedRunFixture { systems, sos, drm };
        let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
        seen = body(&lock, &fixture, &gmat);
        drop(registry_guards);
        drop(volume_guard);
    }
    let after = DockerSnapshot::take();
    let leftovers = Leftovers::take(&before, &cfs_image_id, run_id);
    println!("containers seen during the failed run: {seen:?}");
    assert_eq!(seen.iter().map(|c| c.instance.as_str()).collect::<BTreeSet<_>>(), BTreeSet::from(["chaser_controller", "target_controller"]), "both containers were started before the run failed");
    assert_nothing_left(&before, &after, &leftovers, run_id, &registry_volumes);
}

#[test]
fn a_run_that_fails_after_both_containers_started_leaves_nothing_behind() {
    if skip_if_cfs_image_unavailable("a_run_that_fails_after_both_containers_started_leaves_nothing_behind") {
        return;
    }
    const RUN_ID: &str = "test-run-two-sc-early-error";
    with_failed_run_harness(
        RUN_ID,
        // A command whose target (an attitude plant) is no FRAMED consumer: refused at dispatch,
        // which happens after every container is bound.
        |drm| drm.scenario.as_mut().unwrap().events[0].instance = "chaser_attitude".to_string(),
        |_lock, f, gmat| {
            let watcher = LabelWatcher::start(RUN_ID);
            let err = execute(run_config(gmat, &f.drm, &f.sos, &f.systems, RUN_ID, None)).err();
            let seen = watcher.finish();
            assert!(matches!(err, Some(DrmError::CommandTargetNotFramedConsumer { ref instance, .. }) if instance == "chaser_attitude"), "{err:?}");
            seen
        },
    );
}

#[test]
fn a_panic_after_both_containers_started_leaves_nothing_behind() {
    if skip_if_cfs_image_unavailable("a_panic_after_both_containers_started_leaves_nothing_behind") {
        return;
    }
    const RUN_ID: &str = "test-run-two-sc-panic";
    with_failed_run_harness(
        RUN_ID,
        |_| {},
        |_lock, f, gmat| {
            let watcher = LabelWatcher::start(RUN_ID);
            let source = PanickingSource;
            let cfg = RunConfig { command_source: Some(&source), ..run_config(gmat, &f.drm, &f.sos, &f.systems, RUN_ID, None) };
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| execute(cfg)));
            let seen = watcher.finish();
            let payload = outcome.expect_err("the run must have panicked");
            let msg = payload.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| payload.downcast_ref::<String>().cloned()).unwrap_or_default();
            assert!(msg.contains("deliberate panic"), "{msg}");
            seen
        },
    );
}

/// A killed test cannot run its `Drop` guards; the next run's `prune_stale_test_resources` is what
/// removes what it left. Here the sweep runs while the kernel's containers are live (the next run's
/// pre-flight racing a stale pair): it must find both by the marker the SoS declared, the run then
/// fails (its peers are gone), and nothing remains.
#[test]
fn the_stale_resource_sweep_finds_and_removes_the_kernels_containers() {
    if skip_if_cfs_image_unavailable("the_stale_resource_sweep_finds_and_removes_the_kernels_containers") {
        return;
    }
    const RUN_ID: &str = "test-run-two-sc-sweep";
    with_failed_run_harness(RUN_ID, |_| {}, |lock, f, gmat| {
        let watcher = LabelWatcher::start(RUN_ID);
        let source = SweepingSource { lock, seen_before_sweep: Default::default(), seen_after_sweep: Default::default(), run_id: RUN_ID.to_string() };
        let cfg = RunConfig { command_source: Some(&source), ..run_config(gmat, &f.drm, &f.sos, &f.systems, RUN_ID, None) };
        let result = execute(cfg);
        let seen = watcher.finish();
        let before = source.seen_before_sweep.lock().unwrap().clone();
        let after = source.seen_after_sweep.lock().unwrap().clone();
        println!("labelled kernel containers before the sweep: {before:?}; after: {after:?}; run result: {:?}", result.as_ref().err());
        assert_eq!(before.len(), 2, "both kernel containers are live and labelled when the sweep starts");
        assert!(after.is_empty(), "the sweep must remove both kernel containers by their {LABEL_TEST} marker, left {after:?}");
        assert!(result.is_err(), "a run whose containers were swept from under it must fail, not continue");
        seen
    });
}

// ------------------------------------------------------------------------------------------
// Perturbations (proof 6).
// ------------------------------------------------------------------------------------------

/// Cross-wire the two `wheel_torque_out` connections and require the tracking proof to FAIL: the
/// chaser's controller drives the target's plant and vice versa, so each plant follows the other
/// loop's command. A proof that passed here would not tell the two containers apart.
#[test]
fn a_cross_wired_pair_fails_the_tracking_proof() {
    if skip_if_cfs_image_unavailable("a_cross_wired_pair_fails_the_tracking_proof") {
        return;
    }
    let engine = gmat_sys::engine_lock();
    let (lock, docker_run_id) = lock_and_prune_docker_tests();
    let systems = container_systems();
    let variant = ContainerVariant { cross_wire: true, ..Default::default() };
    let (image, digest, registry_guards) = push_cfs_image_to_local_registry(&docker_run_id);
    let volume_guard = RegistryVolumeGuard(container_volumes(&registry_guards.0 .0));
    let sos = container_sos("two_spacecraft_cfs_cross_sos", &image, &digest, &variant);
    let drm = two_sc_drm("two_spacecraft_cfs_cross_drm", &sos.id, DURATION_S);
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let crossed = execute(run_config(&gmat, &drm, &sos, &systems, "test-run-two-sc-cross", None)).expect("the cross-wired container pair still executes");
    drop(registry_guards);
    drop(volume_guard);
    // `run_native_pair` takes the engine lock itself.
    drop(lock);
    drop(engine);

    // Against the properly wired native pair (the reference the proof compares to).
    let native = run_native_pair("cross-reference", &native_sos("two_spacecraft_native_cross_reference_sos"));
    let verdict = check_tracking(&crossed, &native, theta0_rad);
    for sc in SPACECRAFT {
        println!("cross-wired container {sc}: theta(30s)={:.6} rad; wired native {:.6} rad; closed form {:.6} rad", truth_theta_at_end(&crossed, sc), truth_theta_at_end(&native, sc), theta0_rad(sc) * expected_theta_ratio(30.0));
    }
    let err = verdict.expect_err("the tracking proof must reject a cross-wired pair");
    println!("tracking proof verdict on the cross-wired pair: {err}");
    // And the native run cross-wired against the closed form (no container involved).
    let mut native_cross = native_sos("two_spacecraft_native_cross_sos");
    for c in native_cross.connections.iter_mut().filter(|c| c.from_port == "wheel_torque_out") {
        c.to_instance = if c.to_instance == "chaser_attitude" { "target_attitude".to_string() } else { "chaser_attitude".to_string() };
    }
    native_cross.hash = hash::canonical_sos_hash(&native_cross);
    let native_crossed = run_native_pair("cross", &native_cross);
    let native_verdict = check_tracking(&native_crossed, &native, theta0_rad);
    println!("native cross-wired against the wired native: {native_verdict:?}");
    assert!(native_verdict.is_err(), "the native cross-wired pair must also fail the proof against the wired reference");
    // The closed form alone also rejects it (the native-vs-closed-form leg of the proof).
    let off_closed_form = SPACECRAFT.iter().any(|sc| {
        let expected = theta0_rad(sc) * expected_theta_ratio(30.0);
        (truth_theta_at_end(&native_crossed, sc) - expected).abs() / expected >= 0.10
    });
    assert!(off_closed_form, "a cross-wired native pair must leave the closed form by more than 10% for at least one spacecraft");
}

/// Both controllers on the same `container.seed_key`: what changes? The cFS ADCS app draws no
/// randomness, so the port traffic is identical to the separated-key run; what changes is the SoS
/// (hence the sos_configuration_hash and the whole-file port_traffic_hash). Pinned as measured.
#[test]
fn a_shared_seed_key_changes_the_configuration_hash_but_not_the_port_traffic() {
    if skip_if_cfs_image_unavailable("a_shared_seed_key_changes_the_configuration_hash_but_not_the_port_traffic") {
        return;
    }
    let p = pair_runs();
    let _engine = gmat_sys::engine_lock();
    let (_lock, docker_run_id) = lock_and_prune_docker_tests();
    let (image, digest, registry_guards) = push_cfs_image_to_local_registry(&docker_run_id);
    let volume_guard = RegistryVolumeGuard(container_volumes(&registry_guards.0 .0));
    let variant = ContainerVariant { seed_keys: ["chaser_controller".to_string(), "chaser_controller".to_string()], ..Default::default() };
    let sos = container_sos("two_spacecraft_cfs_shared_seed_sos", &image, &digest, &variant);
    let drm = two_sc_drm("two_spacecraft_cfs_shared_seed_drm", &sos.id, DURATION_S);
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let run = execute_container_run(&gmat, &drm, &sos, &container_systems(), "test-run-two-sc-shared-seed", &scratch_dir("shared-seed"), "shared-seed");
    drop(registry_guards);
    drop(volume_guard);
    let (log_shared, log_sep) = (decode_log(&run.sidecar), decode_log(&p.run_a.sidecar));
    println!("records-only hash: shared-seed-key {} vs separated {}", records_only_hash(&log_shared), records_only_hash(&log_sep));
    println!("sos_configuration_hash: shared {:?} vs separated {:?}", run.products.provenance.attributes.get("sos_configuration_hash"), p.run_a.products.provenance.attributes.get("sos_configuration_hash"));
    println!("whole-file port_traffic_hash: shared {} vs separated {}", run.products.port_traffic_hash, p.run_a.products.port_traffic_hash);
    assert_eq!(log_shared.records, log_sep.records, "the ADCS app draws no randomness: the records do not depend on the seed key");
    assert_ne!(run.products.provenance.attributes.get("sos_configuration_hash"), p.run_a.products.provenance.attributes.get("sos_configuration_hash"), "the declared configuration differs");
    assert_ne!(run.products.port_traffic_hash, p.run_a.products.port_traffic_hash, "the whole-file hash embeds the configuration hash");
    assert_eq!(run.products.trajectories["chaser_attitude"].samples, p.run_a.products.trajectories["chaser_attitude"].samples);
}

/// The determinism comparison of proof 1 can fail: change one declared input of the CHASER only (its
/// star tracker's noise seed) and the chaser container's recorded frames differ from the pair's, the
/// whole-file hash differs, and the target container's frames are still byte-identical (the two
/// loops are independent processes fed by independent sensors).
#[test]
fn a_changed_input_shows_in_the_comparison_for_its_own_container_only() {
    if skip_if_cfs_image_unavailable("a_changed_input_shows_in_the_comparison_for_its_own_container_only") {
        return;
    }
    let p = pair_runs();
    let _engine = gmat_sys::engine_lock();
    let (_lock, docker_run_id) = lock_and_prune_docker_tests();
    let (image, digest, registry_guards) = push_cfs_image_to_local_registry(&docker_run_id);
    let volume_guard = RegistryVolumeGuard(container_volumes(&registry_guards.0 .0));
    let mut sos = container_sos("two_spacecraft_cfs_perturbed_sos", &image, &digest, &ContainerVariant::default());
    let st = sos.instances.iter_mut().find(|i| i.name == "chaser_startracker").unwrap();
    st.parameter_overrides.push(Parameter { name: "startracker.seed".to_string(), value: 9.0, ..Default::default() });
    sos.hash = hash::canonical_sos_hash(&sos);
    let drm = two_sc_drm("two_spacecraft_cfs_perturbed_drm", &sos.id, DURATION_S);
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let run = execute_container_run(&gmat, &drm, &sos, &container_systems(), PAIR_RUN_ID, &scratch_dir("perturbed"), "perturbed");
    drop(registry_guards);
    drop(volume_guard);
    let (perturbed, reference) = (decode_log(&run.sidecar), decode_log(&p.run_a.sidecar));
    let of = |log: &PortTrafficLog, instance: &str| log.records.iter().filter(|r| r.instance == instance).cloned().collect::<Vec<_>>();
    println!("chaser_controller records equal: {}; target_controller records equal: {}; whole-file hash equal: {}", of(&perturbed, "chaser_controller") == of(&reference, "chaser_controller"), of(&perturbed, "target_controller") == of(&reference, "target_controller"), run.products.port_traffic_hash == p.run_a.products.port_traffic_hash);
    assert_ne!(of(&perturbed, "chaser_controller"), of(&reference, "chaser_controller"), "the chaser's containers sees different star tracker frames");
    assert_eq!(of(&perturbed, "target_controller"), of(&reference, "target_controller"), "the target's container is untouched by a change to the chaser's sensor");
    assert_ne!(run.products.port_traffic_hash, p.run_a.products.port_traffic_hash);
    assert_eq!(run.products.trajectories["target_attitude"].samples, p.run_a.products.trajectories["target_attitude"].samples);
    assert_ne!(run.products.trajectories["chaser_attitude"].samples, p.run_a.products.trajectories["chaser_attitude"].samples);
}

/// The Docker-free replay really plays each container's OWN recorded frames: swap the two
/// containers' recorded torque commands in the log (hash recomputed, so the log verifies) and both
/// plants' trajectories change.
#[test]
fn a_replay_of_a_log_with_the_two_containers_torques_swapped_changes_both_plants() {
    if skip_if_cfs_image_unavailable("a_replay_of_a_log_with_the_two_containers_torques_swapped_changes_both_plants") {
        return;
    }
    let p = pair_runs();
    let mut log = decode_log(&p.run_a.sidecar);
    let is_torque_out = |r: &PortTrafficRecord, who: &str| r.instance == who && r.port == "wheel_torque_out" && r.direction == PortDirection::Out as i32;
    let chaser_idx: Vec<usize> = (0..log.records.len()).filter(|&i| is_torque_out(&log.records[i], "chaser_controller")).collect();
    let target_idx: Vec<usize> = (0..log.records.len()).filter(|&i| is_torque_out(&log.records[i], "target_controller")).collect();
    assert_eq!(chaser_idx.len(), target_idx.len());
    for (&c, &t) in chaser_idx.iter().zip(&target_idx) {
        let swapped = log.records[c].payload.clone();
        log.records[c].payload = log.records[t].payload.clone();
        log.records[t].payload = swapped;
    }
    let bytes = log.encode_to_vec();
    let dir = scratch_dir("replay-swapped");
    std::fs::create_dir_all(&dir).expect("scratch dir");
    std::fs::write(dir.join("port_traffic.pb"), &bytes).expect("write the doctored log");
    let _engine = gmat_sys::engine_lock();
    let replay = ReplayConfig { log_path: dir.join("port_traffic.pb"), expected_hash: sha256_hex(&bytes), instances: vec![] };
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let cfg = RunConfig { replay: Some(replay), ..run_config(&gmat, &p.drm, &p.sos, &p.systems, &p.kernel_run_id, None) };
    let swapped = execute(cfg).expect("the doctored log still replays");
    let _ = std::fs::remove_dir_all(&dir);
    let real = &p.run_a.products;
    let (chaser, target) = (truth_theta_at_end(&swapped, "chaser"), truth_theta_at_end(&swapped, "target"));
    println!("swapped replay: chaser theta {chaser:.6} rad (real {:.6}), target theta {target:.6} rad (real {:.6})", truth_theta_at_end(real, "chaser"), truth_theta_at_end(real, "target"));
    assert_ne!(swapped.trajectories["chaser_attitude"].samples, real.trajectories["chaser_attitude"].samples);
    assert_ne!(swapped.trajectories["target_attitude"].samples, real.trajectories["target_attitude"].samples);
}

/// Question 254 item 8: the sidecar records each container's Bind version
/// (`container_bind_version.<instance>`), and the Docker-free replay rebuilds `dynamics_hash` from it.
/// A log that lacks the attribute for a container the replay must rebuild is refused with a typed error
/// (no fallback to a synthetic hash); a log that carries a WRONG version replays, to a different
/// `dynamics_hash` for that container only. Both doctored copies are re-hashed so the hash check passes.
#[test]
fn a_replay_log_without_a_bind_version_is_refused_and_a_wrong_one_changes_that_containers_dynamics_hash() {
    if skip_if_cfs_image_unavailable("a_replay_log_without_a_bind_version_is_refused_and_a_wrong_one_changes_that_containers_dynamics_hash") {
        return;
    }
    let p = pair_runs();
    let real = &p.run_a.products;
    let key = |who: &str| format!("container_bind_version.{who}");
    let recorded = decode_log(&p.run_a.sidecar);
    let attrs = &recorded.provenance.as_ref().expect("the sidecar has provenance").attributes;
    for who in CONTAINER_PARTS {
        println!("recorded {} = {:?}", key(who), attrs.get(&key(who)));
        assert!(attrs.get(&key(who)).is_some_and(|v| !v.is_empty()), "{who}: the sidecar records the Bind version");
    }
    assert_eq!(attrs.keys().filter(|k| k.starts_with("container_bind_version.")).count(), CONTAINER_PARTS.len(), "container instances only");
    let replay_of = |label: &str, doctor: &dyn Fn(&mut BTreeMap<String, String>)| {
        let mut log = recorded.clone();
        doctor(&mut log.provenance.as_mut().unwrap().attributes);
        let bytes = log.encode_to_vec();
        let dir = scratch_dir(label);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        std::fs::write(dir.join("port_traffic.pb"), &bytes).expect("write the doctored log");
        let _engine = gmat_sys::engine_lock();
        let replay = ReplayConfig { log_path: dir.join("port_traffic.pb"), expected_hash: sha256_hex(&bytes), instances: vec![] };
        let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
        let result = execute(RunConfig { replay: Some(replay), ..run_config(&gmat, &p.drm, &p.sos, &p.systems, &p.kernel_run_id, None) });
        let _ = std::fs::remove_dir_all(&dir);
        result
    };
    // Missing: typed refusal naming the instance.
    let refused = replay_of("replay-no-version", &|a| {
        a.remove(&key("chaser_controller"));
    });
    println!("log without the chaser's version: {:?}", refused.as_ref().err());
    assert!(matches!(refused, Err(DrmError::ReplayLogLacksBindVersion { ref instance }) if instance == "chaser_controller"), "got {:?}", refused.as_ref().err());
    // Wrong: replays, and only the chaser container's dynamics_hash moves.
    let wrong = replay_of("replay-wrong-version", &|a| {
        a.insert(key("chaser_controller"), "io_lockstep/not-the-live-version".to_string());
    })
    .expect("a log with a wrong Bind version still replays");
    let hash_of = |products: &RunProducts, who: &str| products.trajectories[who].segments.iter().map(|s| s.dynamics_hash.clone()).collect::<Vec<_>>();
    println!("chaser_controller dynamics_hash: live {:?} wrong-version replay {:?}", hash_of(real, "chaser_controller"), hash_of(&wrong, "chaser_controller"));
    assert_ne!(hash_of(real, "chaser_controller"), hash_of(&wrong, "chaser_controller"), "a wrong Bind version must change the replayed dynamics_hash");
    assert_eq!(hash_of(real, "target_controller"), hash_of(&wrong, "target_controller"), "the other container is untouched");
    assert_eq!(real.trajectories["chaser_attitude"].samples, wrong.trajectories["chaser_attitude"].samples, "the version is identity only; the physics is the recorded traffic");
}

/// The telecommand's destination is the topology: connect the dispatch instance to the TARGET's
/// thruster instead and the applied command (and the trajectory change) moves to the target, with
/// nothing on the chaser. Docker-free (native controllers).
#[test]
fn rerouting_the_telecommand_connection_moves_the_command_to_the_target() {
    let mut sos = native_sos("two_spacecraft_native_rerouted_sos");
    for c in sos.connections.iter_mut() {
        if c.to_instance == "chaser_thruster" && c.to_port == "cmd_in" {
            c.to_instance = "target_thruster".to_string();
        }
        if c.from_instance == "chaser_thruster" && c.from_port == "ack_out" {
            c.from_instance = "target_thruster".to_string();
        }
    }
    sos.hash = hash::canonical_sos_hash(&sos);
    let products = run_native_pair("rerouted", &sos);
    let applied: Vec<_> = products.events.iter().filter(|e| e.kind == EventKind::PortCommand as i32 && e.name == "accel_scale").map(|e| e.entity_id.as_str()).collect();
    println!("accel_scale applied on: {applied:?}");
    assert_eq!(applied, vec!["target_thruster"], "the command follows the connection");
    let systems = native_systems();
    for (sc, moved_expected) in [("chaser", false), ("target", true)] {
        let thruster = format!("{sc}_thruster");
        let kin = FlightKinematics::of(&systems, &sos, &thruster);
        let last = products.trajectories[&thruster].samples.last().unwrap();
        let un = kin.position((last.tai_ns - START_TAI_NS) as f64 / 1e9, None);
        let moved = (0..3).map(|k| (last.mean[k] - un[k]).powi(2)).sum::<f64>().sqrt();
        assert_eq!(moved > 5.0, moved_expected, "{thruster}: displacement from the uncommanded path {moved} m");
    }
}
