//! Hilprep-2b (question 242): a board-bound run replayed with the board replaced by its signed
//! edge I/O log, with the log's chain head and record count pinned in the run's products.
//!
//! **Everything here is a stand-in result. No ZCU104 (or any board) is involved**: the board is a
//! fake lockstep-local guest on a loopback UDP socket behind the real `av-edge-board` binary
//! (`tests/drm_board_common`), answering every STEP with a non-empty, step-dependent wheel-torque
//! packet (a deterministic function of the step's inputs, encoded with the controller system's
//! declared codec) so a replay from the log is not vacuous.
//!
//! The decisive test runs the attitude-control DRM with the controller bound as a board, real
//! time, 5 s, with a `power_cycle` fault at 2 s, then:
//! - replays it **from the edge log** with the edge service and the guest alive but never dialled
//!   (the service has exited after the run's SHUTDOWN; the guest's frame count, the service's log
//!   and the power-control fake's call count are unchanged),
//! - replays it from the run's own `port_traffic.pb` (the explicit container-style choice),
//! - perturbs the log (one output byte, with and without re-signing) and refuses truncations,
//!   rewrites, torn tails, wrong certificates and wrong runs,
//! - and repeats the edge-log replay through the `av-run` binary on the command line.
//!
//! Comparison contract: `board_replay::strip_replay_exclusions` removes exactly the fields
//! `pacing::WALL_CLOCK_DEPENDENT` names; everything else is compared with `==`.
mod drm_board_common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use av_cdm::pb::{self, DesignReferenceMission, PortDirection, PortTrafficLog, SosConfiguration, SystemDefinition};
use av_edge::board_log::{encode_frame, seal, verify_bytes, BoardIoKind, BoardIoRecord, LogSigner, LogVerifier, HEADER_LEN};
use av_kernel::codec::{decode_packet, encode_packet, validate_system_packet_codecs, FieldValue};
use av_kernel::drm::board_replay::{out_frames_of_log, strip_replay_exclusions, ReplaySource};
use av_kernel::drm::replay::ReplayConfig;
use av_kernel::drm::{execute_with_board_replay, hash, schema, BoardLogPin, BoardLogReplay, BoardReplayRefusal, DrmError, RunConfig, RunProducts};
use drm_board_common::*;
use prost::Message;

const RUN_ID: &str = "board-replay-run";
const START_TAI_NS: i64 = 1_767_225_637_000_000_000;
const EDGE_NODE: &str = "zcu104-test";
const BOARD_SYSTEM: &str = "demo_attitude_control_controller_board";

// ------------------------------------------------------------------------------------------
// The fake guest's answer: a step-dependent torque packet
// ------------------------------------------------------------------------------------------

/// The guest mirrors the native control law (`tau_k = kp*qv_k + kd*omega_k`, kp 0.25, kd 5) over
/// the measurements it was handed, holding the last ones between deliveries, and answers with the
/// wheel-torque packet in the controller system's declared codec. Odd sequences carry the
/// output's own epoch, 50 ms inside the STEP (before its end); even ones carry none (0, `NO_MESSAGE_EPOCH`), so both
/// branches of the epoch mapping occur in the log. `named_outputs["torque_norm"]` is the torque's
/// norm.
fn torque_answer() -> StepAnswer {
    let sys = load_system(BOARD_SYSTEM);
    let apid_map = validate_system_packet_codecs(&sys.packet_codecs).expect("the controller system's codecs are valid");
    let torque_codec = sys.packet_codecs.iter().find(|c| c.apid == 300).expect("the wheel torque codec").clone();
    let held: Mutex<([f64; 4], [f64; 3])> = Mutex::new(([0.0, 0.0, 0.0, 1.0], [0.0; 3]));
    Arc::new(move |req| {
        let mut held = held.lock().unwrap();
        for m in &req.inputs {
            let d = decode_packet(&apid_map, &m.payload).unwrap_or_else(|e| panic!("fake guest: undecodable input on {}: {e:?}", m.port));
            let num = |name: &str| match d.fields.get(name) {
                Some(FieldValue::Numeric(v)) => *v,
                other => panic!("fake guest: field {name} of apid {}: {other:?}", d.apid),
            };
            match m.port.as_str() {
                "startracker_in" => held.0 = [num("qx"), num("qy"), num("qz"), num("qw")],
                "imu_in" => held.1 = [num("wx"), num("wy"), num("wz")],
                other => panic!("fake guest: input on unexpected port {other}"),
            }
        }
        let (q, w) = (held.0, held.1);
        let sign = if q[3] < 0.0 { -1.0 } else { 1.0 };
        let tau: Vec<f64> = (0..3).map(|k| 0.25 * sign * q[k] + 5.0 * w[k]).collect();
        let mut values = BTreeMap::new();
        for (k, name) in ["tau_1", "tau_2", "tau_3"].iter().enumerate() {
            values.insert(name.to_string(), FieldValue::Numeric(tau[k]));
        }
        let payload = encode_packet(&torque_codec, req.sequence as u16, &[], &values).expect("the torque packet encodes");
        let tai_ns = if req.sequence % 2 == 1 { req.until_tai_ns - 50_000_000 } else { 0 };
        let norm = (tau[0] * tau[0] + tau[1] * tau[1] + tau[2] * tau[2]).sqrt();
        (vec![pb::PortMessage { port: "wheel_torque_out".to_string(), tai_ns, payload }], BTreeMap::from([("torque_norm".to_string(), norm)]))
    })
}

// ------------------------------------------------------------------------------------------
// The bundle: YAML files on disk, parsed (the live run, the replays and `av-run` all use these)
// ------------------------------------------------------------------------------------------

struct Bundle {
    drm_path: PathBuf,
    sos_path: PathBuf,
    system_paths: Vec<PathBuf>,
    drm: DesignReferenceMission,
    sos: SosConfiguration,
    systems: BTreeMap<String, SystemDefinition>,
}

fn drms(name: &str) -> PathBuf {
    repo_root().join("drms").join(name)
}

fn with_hash(text: &str, hash: &str) -> String {
    let mut out: Vec<String> = text.lines().map(str::to_string).collect();
    let line = out.iter().rposition(|l| l.starts_with("hash:")).expect("a top-level hash line");
    out[line] = format!("hash: \"{hash}\"");
    out.join("\n") + "\n"
}

fn replace_once(text: &str, old: &str, new: &str) -> String {
    assert!(text.matches(old).count() == 1, "expected exactly one {old:?} in the template");
    text.replacen(old, new, 1)
}

/// The attitude-control DRM with `controller` bound as a board at `device` (every port of it), its
/// edge service at `edge_address`, an optional `power_cycle` fault at `fault_s` seconds, and the
/// plant's wheel momentum as a score (and, with `named_output_score`, the guest's `torque_norm`
/// named output as a second one).
fn write_bundle(dir: &Path, device: &str, edge_address: &str, power_control: &str, arc_s: i64, fault_s: Option<i64>, named_output_score: bool) -> Bundle {
    std::fs::create_dir_all(dir).unwrap();
    // The controller's system: the board variant with the service's address (and a declared output).
    let mut sys = std::fs::read_to_string(drms("demo_attitude_control_controller_board.system.yaml")).unwrap();
    sys = replace_once(&sys, "string_value: \"127.0.0.1:50081\"", &format!("string_value: \"{edge_address}\""));
    if named_output_score {
        sys = replace_once(&sys, "    string_value: controller\nprovenance:", "    string_value: controller\n  - name: output.torque_norm\n    unit: UNIT_NEWTON_METER\nprovenance:");
    }
    let sys_parsed = schema::parse_system_definition_yaml(&with_hash(&sys, "")).unwrap();
    let sys = with_hash(&sys, &hash::canonical_system_hash(&sys_parsed));
    let sys_path = dir.join("controller_board.system.yaml");
    std::fs::write(&sys_path, &sys).unwrap();

    // The SoS: the model one with the controller instance replaced by a board instance.
    let mut sos = std::fs::read_to_string(drms("demo_attitude_control.sos.yaml")).unwrap();
    sos = replace_once(&sos, "id: attitude_control_sos\n", "id: attitude_control_board_replay_sos\n");
    let ports = ["startracker_in", "imu_in", "wheel_torque_out"].iter().map(|p| format!("          {p}: \"{device}\"\n")).collect::<String>();
    sos = replace_once(
        &sos,
        "  - name: controller\n    system_id: attitude_control_controller_sys\n    binding:\n      kind: BINDING_KIND_MODEL\n      model:\n        model_id: attitude_control_controller_sys\n",
        &format!(
            "  - name: controller\n    system_id: attitude_control_controller_board_sys\n    step_rate_hz: 10.0\n    binding:\n      kind: BINDING_KIND_BOARD\n      board:\n        edge_node_id: {EDGE_NODE}\n        power_control: \"{power_control}\"\n        port_devices:\n{ports}"
        ),
    );
    let sos_parsed = schema::parse_sos_yaml(&with_hash(&sos, "")).unwrap();
    let sos = with_hash(&sos, &hash::canonical_sos_hash(&sos_parsed));
    let sos_path = dir.join("board.sos.yaml");
    std::fs::write(&sos_path, &sos).unwrap();

    // The DRM.
    let faults = match fault_s {
        Some(s) => format!("  faults:\n    - id: pc1\n      instance: controller\n      target_kind: FAULT_TARGET_KIND_HARDWARE\n      kind: power_cycle\n      tai_ns: {}\n", START_TAI_NS + s * 1_000_000_000),
        None => String::new(),
    };
    // A score read off the reproduced fault event: it exists only if the replay reproduces
    // the `fault:pc1` event of the board's power cycle.
    let mut measures = String::new();
    if fault_s.is_some() {
        measures.push_str("  - name: power_cycle_fault_time\n    expression: \"event.pc1.t\"\n    unit: UNIT_SECOND\n");
    }
    if named_output_score {
        measures.push_str("  - name: controller_torque_norm_at_end\n    expression: \"output.controller.torque_norm@end\"\n    unit: UNIT_NEWTON_METER\n");
    }
    let drm = format!(
        "id: attitude_control_board_replay_drm\nversion: \"1\"\nname: Board-bound attitude control, replayed from the edge log (hilprep-2b)\nsos_configuration_id: attitude_control_board_replay_sos\nscenario:\n  start_tai_ns: {START_TAI_NS}\n  end_tai_ns: {}\n{faults}  seeds:\n    controller: 42\nmeasures:\n{measures}options:\n  covariance: false\n  default_step_rate_hz: 10.0\n  sample_interval_s: 1.0\n  real_time: false\n  nearest_spd_projection: false\n  accept_missing_stm_terms: false\nprovenance:\n  author_kind: AUTHOR_KIND_AGENT\n  tool: \"drm_board_replay test\"\nhash: \"\"\n",
        START_TAI_NS + arc_s * 1_000_000_000
    );
    let drm_parsed = schema::parse_drm_yaml(&drm).unwrap();
    let drm = with_hash(&drm, &hash::canonical_drm_hash(&drm_parsed));
    let drm_path = dir.join("board.drm.yaml");
    std::fs::write(&drm_path, &drm).unwrap();

    let system_paths = vec![drms("demo_attitude_control_truth.system.yaml"), drms("demo_attitude_control_startracker.system.yaml"), drms("demo_attitude_control_imu.system.yaml"), sys_path];
    let systems: BTreeMap<String, SystemDefinition> = system_paths
        .iter()
        .map(|p| {
            let s = schema::parse_system_definition_yaml(&std::fs::read_to_string(p).unwrap()).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
            (s.id.clone(), s)
        })
        .collect();
    Bundle {
        drm: schema::parse_drm_yaml(&std::fs::read_to_string(&drm_path).unwrap()).unwrap(),
        sos: schema::parse_sos_yaml(&std::fs::read_to_string(&sos_path).unwrap()).unwrap(),
        drm_path,
        sos_path,
        system_paths,
        systems,
    }
}

fn execute_run(b: &Bundle, run_id: &str, products_dir: Option<PathBuf>, replay: Option<ReplayConfig>, boards: &[BoardLogReplay]) -> Result<RunProducts, DrmError> {
    #[cfg(feature = "gmat")]
    let _engine = gmat_sys::engine_lock();
    #[cfg(feature = "gmat")]
    let gmat = gmat_sys::Gmat::setup(&gmat_sys::Gmat::default_startup_file()).expect("GMAT setup (default-feature build only)");
    execute_with_board_replay(
        RunConfig {
            #[cfg(feature = "gmat")]
            gmat: &gmat,
            drm: &b.drm,
            sos: &b.sos,
            systems: &b.systems,
            run_id: run_id.to_string(),
            error_mode: Default::default(),
            products_dir,
            replay,
            command_source: None,
        },
        boards,
    )
}

// ------------------------------------------------------------------------------------------
// The live run
// ------------------------------------------------------------------------------------------

struct Live {
    dir: PathBuf,
    bundle: Bundle,
    products: RunProducts,
    wire: pb::RunProducts,
    products_dir: PathBuf,
    log_path: PathBuf,
    cert_pem: Vec<u8>,
    key_pem: Vec<u8>,
    sidecar_copy: PathBuf,
    live_pb: PathBuf,
    // Kept alive through the replays, to prove they never dial anything.
    guest: FakeGuest,
    service: Service,
    channel: FakeChannel,
    wall_s: f64,
}

impl Live {
    fn pin(&self) -> BoardLogPin {
        BoardLogPin::from_trajectories(&self.products.trajectories, CONTROLLER).expect("the live run pinned its board log")
    }
    fn replay_of(&self, log_path: &Path, expected: BoardLogPin) -> BoardLogReplay {
        BoardLogReplay { instance: CONTROLLER.to_string(), log_path: log_path.to_path_buf(), certificate_pem: self.cert_pem.clone(), expected }
    }
    fn log_bytes(&self) -> Vec<u8> {
        std::fs::read(&self.log_path).unwrap()
    }
    fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let p = self.dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    }
    fn replay(&self, log: &Path, expected: BoardLogPin) -> Result<RunProducts, DrmError> {
        execute_run(&self.bundle, RUN_ID, Some(self.products_dir.clone()), None, &[self.replay_of(log, expected)])
    }
    fn replay_refusal(&self, what: &str, log: &Path, expected: BoardLogPin) -> BoardReplayRefusal {
        let before = self.guest.seen.lock().unwrap().frames.len();
        let err = self.replay(log, expected).expect_err(what);
        println!("REFUSED ({what}): {err}");
        assert_eq!(self.guest.seen.lock().unwrap().frames.len(), before, "{what}: nothing was dialled");
        match err {
            DrmError::BoardReplay { instance, refusal } => {
                assert_eq!(instance, CONTROLLER);
                *refusal
            }
            other => panic!("{what}: expected DrmError::BoardReplay, got {other:?}"),
        }
    }
}

fn keep() -> bool {
    std::env::var_os("AV_KEEP_BOARD_REPLAY").is_some()
}

fn live_run(tag: &str, arc_s: i64, fault_s: Option<i64>, named_output_score: bool) -> Live {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("drm-board-replay-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let channel = FakeChannel::create(tag);
    let guest = FakeGuest::spawn(GuestPlan { answer: Some(torque_answer()), ..GuestPlan::default() });
    let device = udp_device(&guest);
    let service = Service::start_with(tag, &device, EDGE_NODE, &["--power-control".to_string(), channel.uri()]);
    let bundle = write_bundle(&dir, &device, &service.grpc_addr, &channel.uri(), arc_s, fault_s, named_output_score);
    let products_dir = dir.join("products");
    let t0 = std::time::Instant::now();
    let products = execute_run(&bundle, RUN_ID, Some(products_dir.clone()), None, &[]).unwrap_or_else(|e| panic!("the live run: {e}\nservice stderr:\n{}", service.stderr()));
    let wall_s = t0.elapsed().as_secs_f64();
    // The service exits after the SHUTDOWN; its log, certificate and key stay in its directory.
    let log_path = dir.join("io.log");
    std::fs::copy(service.dir.join("io.log"), &log_path).unwrap();
    let cert_pem = std::fs::read(service.dir.join("edge.cert.pem")).unwrap();
    let key_pem = std::fs::read(service.dir.join("edge.key.pem")).unwrap();
    std::fs::write(dir.join("edge.cert.pem"), &cert_pem).unwrap();
    let sidecar_copy = dir.join("live_port_traffic.pb");
    std::fs::copy(products_dir.join("port_traffic.pb"), &sidecar_copy).unwrap();
    let wire = products.to_proto();
    let live_pb = dir.join("live.pb");
    std::fs::write(&live_pb, wire.encode_to_vec()).unwrap();
    Live { dir, bundle, products, wire, products_dir, log_path, cert_pem, key_pem, sidecar_copy, live_pb, guest, service, channel, wall_s }
}

impl Drop for Live {
    fn drop(&mut self) {
        if !keep() {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

// ------------------------------------------------------------------------------------------
// Log surgery (the test holds the signing key, as the edge node does)
// ------------------------------------------------------------------------------------------

fn frames_of(bytes: &[u8]) -> Vec<(usize, usize)> {
    let mut out = vec![];
    let mut o = 0;
    while o < bytes.len() {
        let len = u32::from_le_bytes(bytes[o..o + 4].try_into().unwrap()) as usize;
        out.push((o, o + HEADER_LEN + len));
        o += HEADER_LEN + len;
    }
    out
}

/// Re-chain and re-sign every record from `from` on with the genuine key; the bytes of the log.
fn reseal(live: &Live, mut records: Vec<BoardIoRecord>, from: usize) -> Vec<u8> {
    let signer = LogSigner::from_pem(&live.key_pem, &live.cert_pem).unwrap();
    for i in from..records.len() {
        records[i].prev_hash = if i == 0 { av_edge::hash::GENESIS.to_vec() } else { records[i - 1].record_hash.clone() };
        seal(&mut records[i], &signer).unwrap();
    }
    records.iter().flat_map(|r| encode_frame(r).unwrap()).collect()
}

fn verified(live: &Live, bytes: &[u8]) -> av_edge::board_log::VerifiedLog {
    verify_bytes(bytes, &LogVerifier::from_pem(&live.cert_pem).unwrap()).expect("the log verifies")
}

/// Index of the `n`th STEP record with a non-empty output.
fn nth_step_with_output(records: &[BoardIoRecord], n: usize) -> usize {
    records.iter().enumerate().filter(|(_, r)| r.kind == BoardIoKind::Step as i32 && !r.outputs.is_empty()).nth(n).map(|(i, _)| i).expect("enough steps")
}

fn throwaway_certificate() -> (Vec<u8>, Vec<u8>) {
    use openssl::asn1::Asn1Time;
    use openssl::bn::BigNum;
    use openssl::ec::{EcGroup, EcKey};
    use openssl::hash::MessageDigest;
    use openssl::nid::Nid;
    use openssl::pkey::PKey;
    use openssl::x509::{X509Builder, X509NameBuilder};
    let key = EcKey::generate(&EcGroup::from_curve_name(Nid::SECP384R1).unwrap()).unwrap();
    let pkey = PKey::from_ec_key(key).unwrap();
    let mut name = X509NameBuilder::new().unwrap();
    name.append_entry_by_nid(Nid::COMMONNAME, "someone-else").unwrap();
    let name = name.build();
    let mut b = X509Builder::new().unwrap();
    b.set_version(2).unwrap();
    b.set_serial_number(&BigNum::from_u32(7).unwrap().to_asn1_integer().unwrap()).unwrap();
    b.set_subject_name(&name).unwrap();
    b.set_issuer_name(&name).unwrap();
    b.set_pubkey(&pkey).unwrap();
    b.set_not_before(&Asn1Time::days_from_now(0).unwrap()).unwrap();
    b.set_not_after(&Asn1Time::days_from_now(30).unwrap()).unwrap();
    b.sign(&pkey, MessageDigest::sha384()).unwrap();
    (b.build().to_pem().unwrap(), pkey.public_key_to_pem().unwrap())
}

// ------------------------------------------------------------------------------------------
// Comparison
// ------------------------------------------------------------------------------------------

fn stripped(products: &RunProducts, source: ReplaySource) -> pb::RunProducts {
    let mut wire = products.to_proto();
    strip_replay_exclusions(&mut wire, CONTROLLER, source);
    wire
}

fn assert_same_products(what: &str, live: &pb::RunProducts, replay: &pb::RunProducts) {
    assert_eq!(live.trajectories, replay.trajectories, "{what}: trajectories");
    assert_eq!(live.events, replay.events, "{what}: events");
    assert_eq!(live.measurements, replay.measurements, "{what}: measurements");
    assert_eq!(live.scores, replay.scores, "{what}: scores");
    assert_eq!(live.provenance, replay.provenance, "{what}: provenance");
    assert_eq!(live.frames, replay.frames, "{what}: frames");
    assert_eq!(live.dropped_in_flight_messages, replay.dropped_in_flight_messages, "{what}: dropped");
    assert_eq!(live.port_traffic_hash, replay.port_traffic_hash, "{what}: port_traffic_hash");
    assert_eq!(live, replay, "{what}: the whole RunProducts");
}

/// Print every excluded field with both values.
fn print_excluded(what: &str, live: &pb::RunProducts, replay: &pb::RunProducts, source: ReplaySource) {
    println!("EXCLUDED FIELDS ({what}):");
    println!("  RunProducts.pacing: live {:?} | replay {:?}", live.pacing.as_ref().map(|p| (p.mode, p.ticks_paced, p.overrun_count, p.worst_overrun_ns)), replay.pacing.as_ref().map(|p| p.ticks_paced));
    let overruns = |p: &pb::RunProducts| p.events.iter().filter(|e| e.id.starts_with("marker:pacing:overrun:")).count();
    println!("  overrun events: live {} | replay {}", overruns(live), overruns(replay));
    let outcome = |p: &pb::RunProducts| p.events.iter().find(|e| e.id.starts_with("marker:power_cycle:")).map(|e| (e.detail.clone(), e.values.get("duration_ns").copied()));
    println!("  power-cycle outcome event (detail, values[duration_ns]): live {:?} | replay {:?}", outcome(live), outcome(replay));
    if source == ReplaySource::PortTraffic {
        let t = |p: &pb::RunProducts| p.trajectories.get(CONTROLLER).map(|t| t.segments.iter().map(|s| (s.dynamics_model.clone(), s.dynamics_hash.clone(), s.dynamics_depth.clone())).collect::<Vec<_>>());
        println!("  board segments (dynamics_model, dynamics_hash, dynamics_depth): live {:?} | replay {:?}", t(live), t(replay));
        let a = |p: &pb::RunProducts| p.trajectories.get(CONTROLLER).and_then(|t| t.provenance.clone()).map(|p| p.attributes);
        println!("  board provenance attributes: live {:?} | replay {:?}", a(live), a(replay));
    } else {
        println!("  (a replay from the edge log reproduces the board segment's dynamics_* and the binding-hash provenance exactly: not excluded)");
    }
}

fn out_records(sidecar: &[u8]) -> Vec<(String, i64, Vec<u8>)> {
    let log = PortTrafficLog::decode(sidecar).unwrap();
    log.records.iter().filter(|r| r.instance == CONTROLLER && r.direction == PortDirection::Out as i32).map(|r| (r.port.clone(), r.tai_ns, r.payload.clone())).collect()
}

// ------------------------------------------------------------------------------------------
// The decisive test
// ------------------------------------------------------------------------------------------

#[test]
fn a_board_bound_run_replays_from_its_signed_edge_log_and_from_port_traffic() {
    let _serial = serial();
    println!("STAND-IN: a fake lockstep-local guest on loopback UDP behind the real av-edge-board; no ZCU104 or other board is involved");
    let live = live_run("main", 5, Some(2), false);
    let pin = live.pin();
    println!("LIVE RUN wall time {:.3} s; pacing {:?}", live.wall_s, live.products.pacing);

    // ---- The live run is not vacuous.
    let pacing = live.products.pacing.as_ref().expect("a board run is paced");
    assert_eq!(pacing.ticks_paced, 50);
    let log_bytes = live.log_bytes();
    let log = verified(&live, &log_bytes);
    assert!(log.recovery.is_none());
    let kinds: Vec<BoardIoKind> = log.records.iter().map(|r| BoardIoKind::try_from(r.kind).unwrap()).collect();
    assert_eq!(kinds.first(), Some(&BoardIoKind::Bind));
    assert_eq!(kinds.last(), Some(&BoardIoKind::Shutdown));
    assert_eq!(kinds.iter().filter(|k| **k == BoardIoKind::Step).count(), 50);
    assert_eq!(kinds.iter().filter(|k| **k == BoardIoKind::PowerCycle).count(), 1);
    assert_eq!(live.channel.calls().len(), 1, "the live run called the power control once");
    let live_out = out_records(&std::fs::read(&live.sidecar_copy).unwrap());
    let distinct: std::collections::BTreeSet<&Vec<u8>> = live_out.iter().map(|(_, _, p)| p).collect();
    assert_eq!(live_out.len(), 50, "one wheel-torque frame per step");
    assert!(distinct.len() > 40 && live_out.iter().all(|(_, _, p)| !p.is_empty()), "non-empty, step-dependent outputs: {} distinct of {}", distinct.len(), live_out.len());
    println!("LIVE scores {:?}", live.products.scores);
    assert_eq!(live.products.scores.len(), 1, "the fault-time score");
    let wheel_h = |p: &RunProducts| p.trajectories["attitude"].samples.last().unwrap().mean[7];
    let h3 = wheel_h(&live.products);
    println!("LIVE plant wheel_h_1 at the end = {h3:e}");
    assert!(h3 != 0.0, "the board's torques moved the plant's wheel momentum");

    // ---- The pin (item 1b): head, count, signer, all from the edge service, in the products.
    assert_eq!(pin.records as usize, log.records.len() - 1, "the pin is taken before the SHUTDOWN record");
    assert_eq!(pin.chain_head, av_edge::hash::hex_encode(&log.records[pin.records as usize - 1].record_hash));
    assert_eq!(pin.signer_cert_sha256, LogVerifier::from_pem(&live.cert_pem).unwrap().cert_sha256().unwrap());
    assert_eq!(log.records[pin.records as usize].prev_hash, log.records[pin.records as usize - 1].record_hash, "the SHUTDOWN record chains from the pinned head");
    println!("PIN chain_head={} records={} signer={}", pin.chain_head, pin.records, pin.signer_cert_sha256);
    let attrs = &live.products.trajectories[CONTROLLER].provenance.as_ref().unwrap().attributes;
    for key in ["binding_kind", "board_binding_hash", "board_link_hash", "board_io_log_chain_head", "board_io_log_records", "board_io_log_signer_cert_sha256"] {
        assert!(attrs.contains_key(key), "{key} in {attrs:?}");
    }

    // ---- The mapping of STEP outputs to recorded epochs, record for record.
    let from_log: Vec<(String, i64, Vec<u8>)> = out_frames_of_log(&log.records).into_iter().map(|f| (f.port, f.tai_ns, f.payload)).collect();
    assert_eq!(from_log, live_out, "the OUT frames the log converts to are exactly the live run's own port_traffic.pb OUT records for the controller");
    let epochs_given: usize = log.records.iter().filter(|r| r.kind == BoardIoKind::Step as i32).flat_map(|r| r.outputs.iter()).filter(|m| m.tai_ns == 0).count();
    println!("MAPPING: {} OUT frames equal record for record; {epochs_given} logged outputs carried no epoch of their own (recorded at the STEP's end), {} carried one", from_log.len(), from_log.len() - epochs_given);
    assert!(epochs_given > 0 && epochs_given < from_log.len(), "both branches of the mapping occur");

    // ---- Replay from the edge log: no board, no service, no guest dialled, lockstep.
    assert!(std::net::TcpStream::connect(&live.service.grpc_addr).is_err(), "the edge service has exited: a replay that dialled it would fail");
    let guest_frames = live.guest.seen.lock().unwrap().frames.len();
    let service_records = live.service.read_io_log().records.len();
    let t0 = std::time::Instant::now();
    let replayed = live.replay(&live.log_path, pin.clone()).unwrap_or_else(|e| panic!("the replay from the edge log: {e}"));
    println!("REPLAY FROM EDGE LOG wall time {:.3} s (live {:.3} s)", t0.elapsed().as_secs_f64(), live.wall_s);
    assert_eq!(live.guest.seen.lock().unwrap().frames.len(), guest_frames, "the replay sent the guest nothing");
    assert_eq!(live.service.read_io_log().records.len(), service_records, "the replay logged nothing");
    assert_eq!(live.channel.calls().len(), 1, "the power control was NOT called in the replay");
    assert!(replayed.pacing.is_none() && !replayed.events.iter().any(av_kernel::pacing::is_overrun_event), "a replay is lockstep: no PacingReport, no overrun events");
    let live_wire = live.wire.clone();
    let replay_wire = replayed.to_proto();
    print_excluded("edge-log replay", &live_wire, &replay_wire, ReplaySource::EdgeLog);
    let mut a = live_wire.clone();
    strip_replay_exclusions(&mut a, CONTROLLER, ReplaySource::EdgeLog);
    let b = stripped(&replayed, ReplaySource::EdgeLog);
    assert_same_products("edge-log replay", &a, &b);
    // What is not excluded for the edge log: the board segment and its binding provenance, the pin, the fault event.
    assert_eq!(live_wire.trajectories[CONTROLLER], replay_wire.trajectories[CONTROLLER], "the board trajectory, segments and provenance (binding hashes and the pin) are identical");
    assert_eq!(replay_wire.trajectories[CONTROLLER].segments[0].dynamics_model, "board.controller");
    assert_eq!(live_wire.events.iter().find(|e| e.id == "fault:pc1"), replay_wire.events.iter().find(|e| e.id == "fault:pc1"));
    assert!(live_wire.events.iter().any(|e| e.id == "marker:power_cycle:pc1") && replay_wire.events.iter().any(|e| e.id == "marker:power_cycle:pc1"), "the outcome event exists in both");
    assert_eq!(live_wire.scores, replay_wire.scores);
    // The replay's own sidecar (written over the live one at the same path) is byte-identical.
    assert_eq!(std::fs::read(live.products_dir.join("port_traffic.pb")).unwrap(), std::fs::read(&live.sidecar_copy).unwrap(), "the replay's port_traffic.pb is byte-identical to the live run's");
    assert_eq!(out_records(&std::fs::read(live.products_dir.join("port_traffic.pb")).unwrap()), live_out);

    // ---- Replay from port_traffic.pb: the explicit container-style choice for a board.
    let replay_cfg = ReplayConfig { log_path: live.sidecar_copy.clone(), expected_hash: live.products.port_traffic_hash.clone(), instances: vec![CONTROLLER.to_string()] };
    let by_traffic = execute_run(&live.bundle, RUN_ID, Some(live.products_dir.clone()), Some(replay_cfg), &[]).unwrap_or_else(|e| panic!("the port_traffic.pb replay: {e}"));
    assert_eq!(live.guest.seen.lock().unwrap().frames.len(), guest_frames);
    assert_eq!(live.channel.calls().len(), 1);
    assert!(by_traffic.pacing.is_none());
    let traffic_wire = by_traffic.to_proto();
    print_excluded("port_traffic.pb replay", &live_wire, &traffic_wire, ReplaySource::PortTraffic);
    let mut a = live_wire.clone();
    strip_replay_exclusions(&mut a, CONTROLLER, ReplaySource::PortTraffic);
    assert_same_products("port_traffic.pb replay", &a, &stripped(&by_traffic, ReplaySource::PortTraffic));
    assert!(live_wire.trajectories[CONTROLLER].segments[0].dynamics_hash != traffic_wire.trajectories[CONTROLLER].segments[0].dynamics_hash, "a port-traffic replay does not reproduce the board segment's hash: the exclusion is real");

    // ---- The default (no instance named) includes the board, and needs its edge log.
    let default_cfg = ReplayConfig { log_path: live.sidecar_copy.clone(), expected_hash: live.products.port_traffic_hash.clone(), instances: vec![] };
    match execute_run(&live.bundle, RUN_ID, Some(live.products_dir.clone()), Some(default_cfg.clone()), &[]) {
        Err(DrmError::BoardReplay { refusal, .. }) => assert!(matches!(*refusal, BoardReplayRefusal::NeedsEdgeLog), "{refusal:?}"),
        other => panic!("the default replay set includes the board; without its log: {other:?}"),
    }
    let by_default = execute_run(&live.bundle, RUN_ID, Some(live.products_dir.clone()), Some(default_cfg), &[live.replay_of(&live.log_path, pin.clone())]).expect("default set with the board's log");
    let mut a = live_wire.clone();
    strip_replay_exclusions(&mut a, CONTROLLER, ReplaySource::EdgeLog);
    assert_same_products("default replay set, board from its log", &a, &stripped(&by_default, ReplaySource::EdgeLog));

    // ---- The truncations, rewrites and defects: each a typed refusal before anything binds.
    let frames = frames_of(&log_bytes);
    let n = frames.len();
    assert_eq!(n as u64, pin.records + 1);
    // Cut at the last record (the SHUTDOWN): the chain still verifies; the pin's closing record is gone.
    let p = live.write("cut_last.log", &log_bytes[..frames[n - 1].0]);
    assert!(matches!(live.replay_refusal("cut at the SHUTDOWN", &p, pin.clone()), BoardReplayRefusal::ShutdownMissing { pinned_records } if pinned_records == pin.records));
    // Cut at the last two records (the final STEP and the SHUTDOWN): the pinned count is not reached.
    let p = live.write("cut_two.log", &log_bytes[..frames[n - 2].0]);
    assert!(matches!(live.replay_refusal("cut by its last STEP and SHUTDOWN", &p, pin.clone()), BoardReplayRefusal::Truncated { pinned_records, log_records } if pinned_records == pin.records && log_records == pin.records - 1));
    // Cut in half: still a verifying prefix.
    let p = live.write("cut_half.log", &log_bytes[..frames[n / 2].0]);
    assert!(matches!(live.replay_refusal("cut in half", &p, pin.clone()), BoardReplayRefusal::Truncated { .. }));
    // A torn tail.
    let p = live.write("torn.log", &log_bytes[..log_bytes.len() - 5]);
    assert!(matches!(live.replay_refusal("torn tail", &p, pin.clone()), BoardReplayRefusal::TornTail { .. }));
    // A tampered record, not re-signed: the record hash no longer matches its content.
    let mut tampered = log_bytes.clone();
    let k = nth_step_with_output(&log.records, 9);
    let at = (frames[k].0 + HEADER_LEN..frames[k].1).find(|&i| tampered[i..].starts_with(&log.records[k].outputs[0].payload)).expect("the payload is in the frame");
    tampered[at + 8] ^= 0x40;
    let p = live.write("tampered.log", &tampered);
    assert!(matches!(live.replay_refusal("one output byte changed, not re-signed", &p, pin.clone()), BoardReplayRefusal::LogDefect { source: av_edge::board_log::BoardLogError::RecordHashMismatch { index } } if index == k as u64 + 1));
    // The same byte changed and the chain re-signed with the genuine key: it verifies, but it is not the log the run pinned.
    let mut forged = log.records.clone();
    forged[k].outputs[0].payload[8] ^= 0x40;
    let forged_bytes = reseal(&live, forged, k);
    let forged_path = live.write("forged.log", &forged_bytes);
    let forged_log = verified(&live, &forged_bytes);
    assert!(matches!(live.replay_refusal("one output byte changed and re-signed", &forged_path, pin.clone()), BoardReplayRefusal::ChainHeadMismatch { .. }));
    // Anything appended after the SHUTDOWN, genuinely chained and signed.
    let mut longer = log.records.clone();
    longer.push(BoardIoRecord { kind: BoardIoKind::Step as i32, run_id: RUN_ID.to_string(), instance: CONTROLLER.to_string(), producer_id: longer[0].producer_id.clone(), signer_cert_sha256: longer[0].signer_cert_sha256.clone(), sequence: n as u64 + 1, lockstep_sequence: 999, ..Default::default() });
    let p = live.write("longer.log", &reseal(&live, longer, n));
    assert!(matches!(live.replay_refusal("a record appended after the SHUTDOWN", &p, pin.clone()), BoardReplayRefusal::UnpinnedTail { .. }));
    // The wrong certificate, and a bare public key.
    let (other_cert, other_pub) = throwaway_certificate();
    let mut wrong = live.replay_of(&live.log_path, pin.clone());
    wrong.certificate_pem = other_cert;
    let err = execute_run(&live.bundle, RUN_ID, Some(live.products_dir.clone()), None, &[wrong]).expect_err("an unknown certificate");
    println!("REFUSED (unknown certificate): {err}");
    assert!(matches!(err, DrmError::BoardReplay { ref refusal, .. } if matches!(**refusal, BoardReplayRefusal::CertificateNotThePinned { .. })));
    let mut bare = live.replay_of(&live.log_path, pin.clone());
    bare.certificate_pem = other_pub;
    let err = execute_run(&live.bundle, RUN_ID, Some(live.products_dir.clone()), None, &[bare]).expect_err("a bare key");
    assert!(matches!(err, DrmError::BoardReplay { ref refusal, .. } if matches!(**refusal, BoardReplayRefusal::CertificateUnusable { .. })), "{err}");
    // The wrong run.
    let err = execute_run(&live.bundle, "some-other-run", Some(live.products_dir.clone()), None, &[live.replay_of(&live.log_path, pin.clone())]).expect_err("a log of another run");
    println!("REFUSED (wrong run id): {err}");
    assert!(matches!(err, DrmError::BoardReplay { ref refusal, .. } if matches!(**refusal, BoardReplayRefusal::WrongRun { field: "run_id", .. })));
    // A pin that is not the log's: the head differs.
    let mut other_pin = pin.clone();
    other_pin.chain_head = "0".repeat(64);
    assert!(matches!(live.replay_refusal("a different pinned head", &live.log_path, other_pin), BoardReplayRefusal::ChainHeadMismatch { .. }));
    let mut shorter_pin = pin.clone();
    shorter_pin.records -= 1;
    assert!(matches!(live.replay_refusal("a different pinned count", &live.log_path, shorter_pin), BoardReplayRefusal::ChainHeadMismatch { .. } | BoardReplayRefusal::UnpinnedTail { .. }));
    // No refusal ran anything: still no new guest frames, no power control call.
    assert_eq!(live.guest.seen.lock().unwrap().frames.len(), guest_frames);
    assert_eq!(live.channel.calls().len(), 1);

    // ---- The replay really uses the log: the forged log, with the pin its forger recomputed, replays and the products differ.
    let forged_pin = BoardLogPin::of_log(&forged_log).unwrap();
    assert_ne!(forged_pin, pin);
    let perturbed = live.replay(&forged_path, forged_pin).unwrap_or_else(|e| panic!("the perturbed log with its own pin replays: {e}"));
    let h3_forged = wheel_h(&perturbed);
    println!("PERTURBED REPLAY (one output byte of STEP record {} changed, re-signed, its own pin): plant wheel_h_1 at the end {h3:e} live -> {h3_forged:e}", k + 1);
    assert_ne!(h3, h3_forged, "the plant's state depends on the log");
    assert_ne!(stripped(&perturbed, ReplaySource::EdgeLog), a, "the perturbed replay's products differ from the live run's");
    assert_ne!(perturbed.trajectories["attitude"], live.products.trajectories["attitude"], "the plant's trajectory differs");

    // ---- The same edge-log replay through av-run on the command line.
    let cli_dir = live.products_dir.clone();
    let out = cli_dir.join("cli_replay.pb");
    let av_run = av_run_bin();
    let mut cmd = std::process::Command::new(&av_run);
    cmd.arg("--drm").arg(&live.bundle.drm_path).arg("--sos").arg(&live.bundle.sos_path);
    for s in &live.bundle.system_paths {
        cmd.arg("--system").arg(s);
    }
    cmd.args(["--run-id", RUN_ID]).arg("--out").arg(&out).arg("--replay-board-log").arg(format!("{CONTROLLER}={}", live.log_path.display())).arg("--board-log-cert").arg(live.dir.join("edge.cert.pem")).arg("--board-log-pins").arg(&live.live_pb);
    let output = cmd.output().expect("run av-run");
    println!("av-run stderr:\n{}", String::from_utf8_lossy(&output.stderr));
    assert!(output.status.success(), "av-run replay failed: {}", String::from_utf8_lossy(&output.stderr));
    let cli_wire = pb::RunProducts::decode(std::fs::read(&out).unwrap().as_slice()).unwrap();
    print_excluded("av-run edge-log replay", &live_wire, &cli_wire, ReplaySource::EdgeLog);
    let mut a = live_wire.clone();
    strip_replay_exclusions(&mut a, CONTROLLER, ReplaySource::EdgeLog);
    let mut c = cli_wire.clone();
    strip_replay_exclusions(&mut c, CONTROLLER, ReplaySource::EdgeLog);
    assert_same_products("av-run edge-log replay", &a, &c);
    assert!(cli_wire.pacing.is_none());
    // And av-run refuses the truncated log, naming why, with a failing exit status.
    let refused = std::process::Command::new(&av_run)
        .arg("--drm").arg(&live.bundle.drm_path).arg("--sos").arg(&live.bundle.sos_path)
        .args(live.bundle.system_paths.iter().flat_map(|s| ["--system".as_ref(), s.as_os_str()]))
        .args(["--run-id", RUN_ID]).arg("--out").arg(cli_dir.join("cli_refused.pb"))
        .arg("--replay-board-log").arg(format!("{CONTROLLER}={}", live.dir.join("cut_two.log").display()))
        .arg("--board-log-cert").arg(live.dir.join("edge.cert.pem")).arg("--board-log-pins").arg(&live.live_pb)
        .output()
        .unwrap();
    assert!(!refused.status.success());
    let stderr = String::from_utf8_lossy(&refused.stderr).to_string();
    println!("av-run refusal: {stderr}");
    assert!(stderr.contains("cut short"), "{stderr}");
}

// ------------------------------------------------------------------------------------------
// Named outputs: the edge log reproduces what a port-traffic replay cannot
// ------------------------------------------------------------------------------------------

#[test]
fn the_edge_log_reproduces_the_boards_named_outputs_scores_which_a_port_traffic_replay_cannot() {
    let _serial = serial();
    println!("STAND-IN: a fake lockstep-local guest on loopback UDP behind the real av-edge-board; no ZCU104 or other board is involved");
    let live = live_run("named", 2, None, true);
    let pin = live.pin();
    let torque_norm = live.products.scores["controller_torque_norm_at_end"].value;
    println!("LIVE score controller_torque_norm_at_end = {torque_norm:e}");
    assert!(torque_norm > 0.0, "the guest's named output reached the score");
    assert!(live.products.trajectories[CONTROLLER].provenance.as_ref().unwrap().attributes.contains_key("board_io_log_chain_head"));
    // No faults: no POWER_CYCLE record, and the replay needs none.
    let replayed = live.replay(&live.log_path, pin).unwrap_or_else(|e| panic!("replay: {e}"));
    assert_eq!(replayed.scores["controller_torque_norm_at_end"], live.products.scores["controller_torque_norm_at_end"], "the named output score reproduces from the edge log");
    let mut a = live.wire.clone();
    strip_replay_exclusions(&mut a, CONTROLLER, ReplaySource::EdgeLog);
    assert_same_products("named-output replay", &a, &stripped(&replayed, ReplaySource::EdgeLog));
    // The same score cannot be evaluated from port_traffic.pb: the named output is not in it.
    let cfg = ReplayConfig { log_path: live.sidecar_copy.clone(), expected_hash: live.products.port_traffic_hash.clone(), instances: vec![CONTROLLER.to_string()] };
    let err = execute_run(&live.bundle, RUN_ID, Some(live.products_dir.clone()), Some(cfg), &[]).expect_err("the named output is not in port_traffic.pb");
    println!("port_traffic.pb replay of the same DRM: {err}");
    assert!(matches!(err, DrmError::InvalidExpression { .. }), "{err:?}");
}

// ------------------------------------------------------------------------------------------
// The av-run binary
// ------------------------------------------------------------------------------------------

fn av_run_bin() -> PathBuf {
    let mut dir = std::env::current_exe().unwrap().parent().unwrap().to_path_buf();
    if dir.ends_with("deps") {
        dir.pop();
    }
    let candidate = dir.join("av-run");
    let mut sources = Vec::new();
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else {
                out.push(path);
            }
        }
    }
    for krate in ["av-run", "av-kernel", "av-edge", "av-lockstep", "av-cdm", "av-codec", "av-dynamics", "av-grpc", "av-orbital", "av-command"] {
        walk(&repo_root().join("crates").join(krate).join("src"), &mut sources);
    }
    walk(&repo_root().join(EDGE_BOARD_SOURCE_PROTO_DIR), &mut sources);
    assert!(candidate.is_file(), "the av-run binary {} does not exist: build it with `scripts/dev/cargo-slot build -p av-run`", candidate.display());
    let built = std::fs::metadata(&candidate).unwrap().modified().unwrap();
    let newest = sources.iter().map(|p| (std::fs::metadata(p).unwrap().modified().unwrap(), p)).max_by_key(|(t, _)| *t).unwrap();
    assert!(built >= newest.0, "the av-run binary is older than {}: rebuild it with `scripts/dev/cargo-slot build -p av-run` and rerun", newest.1.display());
    candidate
}
