//! A3.1 (`docs/aiplane-plan.md`'s A3 milestone): acceptance evidence for `crate::drm::
//! command_source::ExternalCommandSource` -- the kernel side of the AI-plane command path.
//! Drives `drms/demo_external_command.drm.yaml` (zero declared `command` `Scenario.event`s --
//! every command in these tests comes from a `RecordingCommandSource`, never a DRM-declared
//! one) over the unchanged `drms/demo_command.sos.yaml` topology (`ground` dispatches,
//! `flight` consumes `accel_scale` and acks).
//!
//! Evidence covered here (see `crate::drm::command_source`'s own module doc comment for the
//! contract each test exercises):
//! 2. A deadline that expires undispatched -- `deadline_expires_before_dispatch_and_the_command_
//!    never_reaches_the_wire`.
//! 3. The boundary nanosecond, both sides -- `the_boundary_nanosecond_both_sides`.
//! 4. A duplicate idempotency key -- `a_duplicate_idempotency_key_is_refused_and_never_
//!    reaches_the_wire_twice`.
//! 5. A structural refusal -- `a_structural_refusal_is_reported_never_swallowed`.
//! 7. Determinism -- `two_runs_over_the_same_source_are_byte_identical`.
//! Plus a full happy-path dispatch proving `ACK_LEVEL_EDGE`/`ACK_LEVEL_ASSET_EXECUTED` and a
//! real physical effect (the `ACK_LEVEL_ASSET_RECEIVED` third level needs a framed consumer
//! that emits a decode-time ack -- `crates/av-kernel/tests/drm_attitude_command.rs`, the
//! attitude controller's own new mode-command port, covers that level).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use av_cdm::pb::{Command, DesignReferenceMission, EventKind, Provenance, SosConfiguration, SystemDefinition};
use av_kernel::drm::command_source::{decide_disposition, pack_double_value, CommandOutcome, Disposition, RecordingCommandSource, RefusalReason};
use av_kernel::drm::{execute, schema, RunConfig};
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

fn load_bundle() -> (DesignReferenceMission, SosConfiguration, BTreeMap<String, SystemDefinition>) {
    let drm = schema::parse_drm_yaml(&read("demo_external_command.drm.yaml")).expect("DRM parses");
    let sos = schema::parse_sos_yaml(&read("demo_command.sos.yaml")).expect("SosConfiguration parses");
    let flight = load_system("demo_command_flight");
    let ground = load_system("demo_command_ground");
    let mut systems = BTreeMap::new();
    systems.insert(flight.id.clone(), flight);
    systems.insert(ground.id.clone(), ground);
    (drm, sos, systems)
}

const START_TAI_NS: i64 = 1_700_000_000_000_000_000;
const RUN_END_TAI_NS: i64 = 1_700_000_100_000_000_000;

/// A well-formed `Command` targeting `flight`'s own `accel_scale`, dispatched from `ground` --
/// the identical `attributes["from"]` convention a DRM-declared `command` event already uses.
fn accel_scale_command(id: &str, idempotency_key: &str, value: f64, not_before_tai_ns: i64, deadline_tai_ns: i64) -> Command {
    Command {
        id: id.to_string(),
        idempotency_key: idempotency_key.to_string(),
        entity_id: "flight".to_string(),
        command_class: "demo".to_string(),
        hazardous: false,
        payload: Some(pack_double_value(value)),
        deadline_tai_ns,
        not_before_tai_ns,
        provenance: Some(Provenance { attributes: BTreeMap::from([("from".to_string(), "ground".to_string())]), ..Default::default() }),
        ..Default::default()
    }
}

fn scratch_dir(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("external-command-source-test-{}-{label}-{n}", std::process::id()));
    assert!(!dir.exists(), "scratch dir {dir:?} must not already exist");
    dir
}

fn read_port_traffic_log(path: &Path) -> av_cdm::pb::PortTrafficLog {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("reading {path:?}: {e}"));
    <av_cdm::pb::PortTrafficLog as prost::Message>::decode(bytes.as_slice()).unwrap_or_else(|e| panic!("{path:?} did not decode as a PortTrafficLog: {e}"))
}

// =================================================================================================
// Happy path: dispatch, EDGE + ASSET_EXECUTED acks, real physical effect.
// =================================================================================================

#[test]
fn a_command_through_the_external_source_dispatches_acks_edge_and_executed_and_has_a_real_physical_effect() {
    let _engine = gmat_sys::engine_lock();
    let (drm, sos, systems) = load_bundle();
    let commanded_at = START_TAI_NS + 20_000_000_000; // t = 20s, well inside [t0, run_end)
    let source = RecordingCommandSource::new(vec![accel_scale_command("ext1", "ext1-key", 3.0, commanded_at, 0)]);
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let products = execute(RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: "test-ext-cmd-happy".to_string(), error_mode: Default::default(), products_dir: None, replay: None, command_source: Some(&source) }).expect("run executes end to end");

    // -- Every real state transition, sourced externally, still runs the real state machine. --
    let transitions: Vec<_> = products.events.iter().filter(|e| e.kind == EventKind::CommandTransition as i32 && e.reference_id == "ext1").collect();
    let states: Vec<&str> = transitions.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(states, vec!["COMMAND_STATE_PROPOSED", "COMMAND_STATE_CHECKED", "COMMAND_STATE_AUTHORIZED", "COMMAND_STATE_DISPATCHED", "COMMAND_STATE_ACKED"], "{transitions:#?}");
    assert_eq!(transitions[3].tai_ns, commanded_at, "DISPATCHED must land exactly at the command's own not_before epoch");
    assert!(transitions[4].tai_ns > commanded_at, "ACKED must land strictly after DISPATCHED -- real, non-zero router latency");

    // -- Every outcome the source itself was told about. --
    let outcomes = source.outcomes();
    assert!(outcomes.contains(&CommandOutcome::Dispatched { id: "ext1".to_string(), epoch_tai_ns: commanded_at, seq: 0 }), "{outcomes:#?}");
    assert!(outcomes.contains(&CommandOutcome::Acked { id: "ext1".to_string(), level: av_cdm::pb::AckLevel::Edge, epoch_tai_ns: commanded_at }), "ACK_LEVEL_EDGE must be reported at the dispatch epoch: {outcomes:#?}");
    let executed = outcomes.iter().find(|o| matches!(o, CommandOutcome::Acked { level: av_cdm::pb::AckLevel::AssetExecuted, .. })).unwrap_or_else(|| panic!("ACK_LEVEL_ASSET_EXECUTED must be reported: {outcomes:#?}"));
    let CommandOutcome::Acked { epoch_tai_ns: applied_tai_ns, .. } = *executed else { unreachable!() };
    assert!(applied_tai_ns > commanded_at, "asset-executed ack must land strictly after dispatch (real router latency)");

    // -- The real, physically meaningful applied command on `flight` (mirrors
    // crates/av-kernel/tests/drm_command.rs's own identical closed-form methodology). --
    let port_commands: Vec<_> = products.events.iter().filter(|e| e.kind == EventKind::PortCommand as i32 && e.entity_id == "flight").collect();
    assert_eq!(port_commands.len(), 1, "{port_commands:#?}");
    assert_eq!(port_commands[0].values.get("value").copied(), Some(3.0));
    assert_eq!(port_commands[0].tai_ns, applied_tai_ns, "the event's own applied_tai_ns must match what was reported as ASSET_EXECUTED");

    let traj = products.trajectories.get("flight").expect("flight produces a trajectory");
    let last = traj.samples.last().expect("flight has at least one sample");
    let end_tai_ns = last.tai_ns;
    let t_apply_s = (applied_tai_ns - START_TAI_NS) as f64 / 1e9;
    let t_end_s = (end_tai_ns - START_TAI_NS) as f64 / 1e9;
    let base_a = 1.0_f64; // demo_command_flight.system.yaml's own declared accel.z
    let commanded_scale = 3.0_f64;
    let v_at_apply = base_a * t_apply_s;
    let z_at_apply = 0.5 * base_a * t_apply_s * t_apply_s;
    let dt2 = t_end_s - t_apply_s;
    let a2 = base_a * commanded_scale;
    let expected_z_end = z_at_apply + v_at_apply * dt2 + 0.5 * a2 * dt2 * dt2;
    assert!((last.mean[2] - expected_z_end).abs() < 1e-6, "expected {expected_z_end}, got {}", last.mean[2]);
    let uncommanded_z_end = 0.5 * base_a * t_end_s * t_end_s;
    let divergence = (last.mean[2] - uncommanded_z_end).abs();
    assert!(divergence > 1.0, "the command must have a real, non-vacuous physical effect; measured only {divergence} m");
}

// =================================================================================================
// Evidence 2 + 3: deadline expiry, and the boundary nanosecond both sides.
// =================================================================================================

/// Evidence 2: a deadline that passes before the command's own `not_before` is EXPIRED,
/// reported with the deadline and the kernel epoch that passed it, and -- checked against the
/// recorded port traffic itself, not merely an absent event -- never reaches the wire at all.
#[test]
fn deadline_expires_before_dispatch_and_the_command_never_reaches_the_wire() {
    let _engine = gmat_sys::engine_lock();
    let (drm, sos, systems) = load_bundle();
    let not_before = START_TAI_NS + 50_000_000_000;
    let deadline = not_before - 1_000_000_000; // one full second before the would-be dispatch
    let source = RecordingCommandSource::new(vec![accel_scale_command("expiring1", "exp-key", 3.0, not_before, deadline)]);
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let dir = scratch_dir("deadline");
    let products = execute(RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: "test-ext-cmd-deadline".to_string(), error_mode: Default::default(), products_dir: Some(dir.clone()), replay: None, command_source: Some(&source) }).expect("run executes end to end (a refused command never aborts the run)");

    let outcomes = source.outcomes();
    assert_eq!(outcomes, vec![CommandOutcome::Expired { id: "expiring1".to_string(), deadline_tai_ns: deadline, kernel_epoch_tai_ns: not_before }], "{outcomes:#?}");

    // No DISPATCHED/ACKED transition, and no PROPOSED/CHECKED/AUTHORIZED either -- an expired
    // external command was never even considered a ParsedCommand, unlike a DRM-declared one.
    let transitions: Vec<_> = products.events.iter().filter(|e| e.kind == EventKind::CommandTransition as i32 && e.reference_id == "expiring1").collect();
    assert!(transitions.is_empty(), "an expired command must produce no state-machine transitions at all: {transitions:#?}");

    // The critical assertion: no command frame ever reached the wire -- checked against the
    // recorded port traffic itself (`PortTrafficLog`), not against an absent event.
    let log = read_port_traffic_log(&dir.join("port_traffic.pb"));
    assert!(log.records.is_empty(), "an expired command must leave zero port-traffic records (this fixture declares no other command source): {:#?}", log.records);
    std::fs::remove_dir_all(&dir).ok();
}

/// Evidence 3: the boundary nanosecond, both sides -- `now_tai_ns >= expiry_tai_ns` (the
/// identical convention `crate::av_command::oidc`/`.authz` already use). One run, two commands:
/// deadline exactly at the dispatch epoch (EXPIRED) and deadline one nanosecond after it
/// (dispatched) -- proves the boundary through the real executor, not only the unit-level
/// `decide_disposition` (already pinned in `crate::drm::command_source`'s own test module).
#[test]
fn the_boundary_nanosecond_both_sides_through_the_real_executor() {
    let _engine = gmat_sys::engine_lock();
    let (drm, sos, systems) = load_bundle();
    let not_before = START_TAI_NS + 50_000_000_000;
    let at_boundary = accel_scale_command("boundary_exact", "b1", 2.0, not_before, not_before);
    let one_ns_after = accel_scale_command("boundary_ok", "b2", 2.0, not_before, not_before + 1);
    let source = RecordingCommandSource::new(vec![at_boundary, one_ns_after]);
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    execute(RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: "test-ext-cmd-boundary".to_string(), error_mode: Default::default(), products_dir: None, replay: None, command_source: Some(&source) }).expect("run executes end to end");

    let outcomes = source.outcomes();
    assert!(outcomes.contains(&CommandOutcome::Expired { id: "boundary_exact".to_string(), deadline_tai_ns: not_before, kernel_epoch_tai_ns: not_before }), "deadline == dispatch epoch must be refused: {outcomes:#?}");
    assert!(outcomes.contains(&CommandOutcome::Dispatched { id: "boundary_ok".to_string(), epoch_tai_ns: not_before, seq: 0 }), "one nanosecond after the dispatch epoch must not be refused: {outcomes:#?}");
    assert!(!outcomes.iter().any(|o| matches!(o, CommandOutcome::Dispatched { id, .. } if id == "boundary_exact")), "the exactly-at-boundary command must never be dispatched: {outcomes:#?}");
}

/// The same boundary, restated as the pure unit-level decision this whole test file's own
/// integration proof is built on top of -- see `crate::drm::command_source`'s own test module
/// for the exhaustive version; this is a second, independent statement of the identical claim.
#[test]
fn decide_disposition_agrees_with_the_integration_test_above() {
    let not_before = START_TAI_NS + 50_000_000_000;
    assert_eq!(decide_disposition(not_before, not_before, START_TAI_NS, RUN_END_TAI_NS), Disposition::Expired { deadline_tai_ns: not_before, kernel_epoch_tai_ns: not_before });
    assert_eq!(decide_disposition(not_before, not_before + 1, START_TAI_NS, RUN_END_TAI_NS), Disposition::Dispatch { epoch_tai_ns: not_before });
}

// =================================================================================================
// Evidence 4: a duplicate idempotency key.
// =================================================================================================

/// Evidence 4: a duplicate idempotency key is refused, reported, and counted -- and the second
/// frame never reaches the wire (checked against the recorded port traffic).
#[test]
fn a_duplicate_idempotency_key_is_refused_and_never_reaches_the_wire_twice() {
    let _engine = gmat_sys::engine_lock();
    let (drm, sos, systems) = load_bundle();
    let t1 = START_TAI_NS + 10_000_000_000;
    let t2 = START_TAI_NS + 20_000_000_000;
    let first = accel_scale_command("dup_first", "shared-key", 2.0, t1, 0);
    let second = accel_scale_command("dup_second", "shared-key", 4.0, t2, 0);
    let source = RecordingCommandSource::new(vec![first, second]);
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let dir = scratch_dir("duplicate");
    execute(RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: "test-ext-cmd-duplicate".to_string(), error_mode: Default::default(), products_dir: Some(dir.clone()), replay: None, command_source: Some(&source) }).expect("run executes end to end");

    let outcomes = source.outcomes();
    assert!(outcomes.contains(&CommandOutcome::Dispatched { id: "dup_first".to_string(), epoch_tai_ns: t1, seq: 0 }), "{outcomes:#?}");
    assert!(outcomes.contains(&CommandOutcome::DuplicateIdempotencyKey { id: "dup_second".to_string(), idempotency_key: "shared-key".to_string() }), "{outcomes:#?}");
    assert!(!outcomes.iter().any(|o| matches!(o, CommandOutcome::Dispatched { id, .. } if id == "dup_second")), "the duplicate must never be dispatched: {outcomes:#?}");

    let log = read_port_traffic_log(&dir.join("port_traffic.pb"));
    let cmd_out_records: Vec<_> = log.records.iter().filter(|r| r.port == "cmd_out").collect();
    assert_eq!(cmd_out_records.len(), 1, "exactly one command frame must reach the wire, never two: {:#?}", log.records);
    std::fs::remove_dir_all(&dir).ok();
}

/// D6: an empty idempotency key is never a duplicate of another empty one -- two commands with
/// no declared key both dispatch.
#[test]
fn two_commands_with_an_empty_idempotency_key_are_not_duplicates_of_each_other() {
    let _engine = gmat_sys::engine_lock();
    let (drm, sos, systems) = load_bundle();
    let t1 = START_TAI_NS + 10_000_000_000;
    let t2 = START_TAI_NS + 20_000_000_000;
    let first = accel_scale_command("nokey_first", "", 2.0, t1, 0);
    let second = accel_scale_command("nokey_second", "", 4.0, t2, 0);
    let source = RecordingCommandSource::new(vec![first, second]);
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    execute(RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: "test-ext-cmd-empty-key".to_string(), error_mode: Default::default(), products_dir: None, replay: None, command_source: Some(&source) }).expect("run executes end to end");

    let outcomes = source.outcomes();
    assert!(outcomes.contains(&CommandOutcome::Dispatched { id: "nokey_first".to_string(), epoch_tai_ns: t1, seq: 0 }), "{outcomes:#?}");
    assert!(outcomes.contains(&CommandOutcome::Dispatched { id: "nokey_second".to_string(), epoch_tai_ns: t2, seq: 1 }), "an empty key must never collide with another empty key: {outcomes:#?}");
}

// =================================================================================================
// Evidence 5: a structural refusal is reported, not swallowed.
// =================================================================================================

/// Evidence 5: `ground` classifies to a native `ConstantAccelModel` that declares no
/// `port.consume_framed` at all -- the easy structural refusal (mirrors `crates/av-kernel/
/// tests/drm_command.rs::a_command_targeting_an_instance_with_no_consume_framed_port_is_a_
/// typed_refusal`'s own non-source version).
#[test]
fn a_structural_refusal_is_reported_never_swallowed() {
    let _engine = gmat_sys::engine_lock();
    let (drm, sos, systems) = load_bundle();
    let mut bad = accel_scale_command("bad_target", "bad-key", 2.0, START_TAI_NS + 10_000_000_000, 0);
    bad.entity_id = "ground".to_string(); // ground never declares consume_framed
    let source = RecordingCommandSource::new(vec![bad]);
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    execute(RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: "test-ext-cmd-bad-target".to_string(), error_mode: Default::default(), products_dir: None, replay: None, command_source: Some(&source) }).expect("a structural refusal must never abort the whole run");

    let outcomes = source.outcomes();
    assert_eq!(outcomes, vec![CommandOutcome::Refused { id: "bad_target".to_string(), reason: RefusalReason::TargetNotFramedConsumer { instance: "ground".to_string() } }], "{outcomes:#?}");
}

/// A second structural refusal shape: an unknown target instance entirely.
#[test]
fn an_unknown_target_instance_is_a_reported_structural_refusal() {
    let _engine = gmat_sys::engine_lock();
    let (drm, sos, systems) = load_bundle();
    let mut bad = accel_scale_command("unknown_target", "uk-key", 2.0, START_TAI_NS + 10_000_000_000, 0);
    bad.entity_id = "no_such_instance".to_string();
    let source = RecordingCommandSource::new(vec![bad]);
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    execute(RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: "test-ext-cmd-unknown-target".to_string(), error_mode: Default::default(), products_dir: None, replay: None, command_source: Some(&source) }).expect("run executes end to end");

    let outcomes = source.outcomes();
    assert_eq!(outcomes, vec![CommandOutcome::Refused { id: "unknown_target".to_string(), reason: RefusalReason::UnknownTarget { instance: "no_such_instance".to_string() } }], "{outcomes:#?}");
}

/// A third structural refusal shape: an unknown sender (`provenance.attributes["from"]`).
#[test]
fn an_unknown_sender_is_a_reported_structural_refusal() {
    let _engine = gmat_sys::engine_lock();
    let (drm, sos, systems) = load_bundle();
    let mut bad = accel_scale_command("unknown_sender", "us-key", 2.0, START_TAI_NS + 10_000_000_000, 0);
    bad.provenance = Some(Provenance { attributes: BTreeMap::from([("from".to_string(), "no_such_ground".to_string())]), ..Default::default() });
    let source = RecordingCommandSource::new(vec![bad]);
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    execute(RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: "test-ext-cmd-unknown-sender".to_string(), error_mode: Default::default(), products_dir: None, replay: None, command_source: Some(&source) }).expect("run executes end to end");

    let outcomes = source.outcomes();
    assert_eq!(outcomes, vec![CommandOutcome::Refused { id: "unknown_sender".to_string(), reason: RefusalReason::UnknownSender { sender: "no_such_ground".to_string() } }], "{outcomes:#?}");
}

/// A fourth structural refusal shape: a malformed payload (not a packed `DoubleValue` at all).
#[test]
fn a_malformed_payload_is_a_reported_structural_refusal() {
    let _engine = gmat_sys::engine_lock();
    let (drm, sos, systems) = load_bundle();
    let mut bad = accel_scale_command("malformed", "mf-key", 2.0, START_TAI_NS + 10_000_000_000, 0);
    bad.payload = None;
    let source = RecordingCommandSource::new(vec![bad]);
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    execute(RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: "test-ext-cmd-malformed".to_string(), error_mode: Default::default(), products_dir: None, replay: None, command_source: Some(&source) }).expect("run executes end to end");

    let outcomes = source.outcomes();
    assert_eq!(outcomes.len(), 1, "{outcomes:#?}");
    assert!(matches!(&outcomes[0], CommandOutcome::Refused { id, reason: RefusalReason::MalformedPayload { .. } } if id == "malformed"), "{outcomes:#?}");
}

// =================================================================================================
// Evidence 7: determinism.
// =================================================================================================

/// Two runs over a source with the same contents produce byte-identical `RunProducts` --
/// mirrors `crates/av-kernel/tests/restart_invariance.rs`/`faults_determinism.rs`'s own
/// `prost::Message::encode_to_vec` + `assert_eq!` methodology exactly.
#[test]
fn two_runs_over_the_same_source_are_byte_identical() {
    let _engine = gmat_sys::engine_lock();
    let (drm, sos, systems) = load_bundle();
    let commanded_at = START_TAI_NS + 20_000_000_000;
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");

    let source_a = RecordingCommandSource::new(vec![accel_scale_command("det1", "det-key", 3.0, commanded_at, 0)]);
    let products_a = execute(RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: "test-ext-cmd-det".to_string(), error_mode: Default::default(), products_dir: None, replay: None, command_source: Some(&source_a) }).expect("run A executes");

    let source_b = RecordingCommandSource::new(vec![accel_scale_command("det1", "det-key", 3.0, commanded_at, 0)]);
    let products_b = execute(RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: "test-ext-cmd-det".to_string(), error_mode: Default::default(), products_dir: None, replay: None, command_source: Some(&source_b) }).expect("run B executes");

    let bytes_a = prost::Message::encode_to_vec(&products_a.to_proto());
    let bytes_b = prost::Message::encode_to_vec(&products_b.to_proto());
    assert_eq!(bytes_a, bytes_b, "two runs over the same source contents must produce byte-identical RunProducts");
    assert_eq!(source_a.outcomes(), source_b.outcomes(), "and byte-identical reported outcomes");
}
