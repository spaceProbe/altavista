//! R4.1a (`docs/open-questions.md` question 178, ADR-005 sec 5, `crate::router`'s own module doc
//! comment's "Port fault runtime" section): the `drop`/`delay` PORT fault runtime, exercised end
//! to end through `execute()` against `drms/demo_command_port_drop.drm.yaml`/
//! `demo_command_port_delay.drm.yaml` -- both `drms/demo_command.drm.yaml`-derived (identical
//! `sos_configuration_id`/systems, `drms/demo_command.sos.yaml`/`_flight.system.yaml`/
//! `_ground.system.yaml` reused UNCHANGED), each adding exactly one `FAULT_TARGET_KIND_PORT`
//! fault on the EMITTING port of the command dispatch (`instance="ground"`, `target="cmd_out"`).
//!
//! ## Hypotheses, stated BEFORE running (mirrors `tests/port_traffic_sidecar.rs`'s own module doc
//! comment's method)
//!
//! `demo_command` is a 100 s, 1 Hz run (`start_tai_ns = 1_700_000_000_000_000_000`,
//! `output_period_ns = 1_000_000_000`) with `ground.cmd_out -> flight.cmd_in` carrying 3 s of
//! declared connection latency (`drms/demo_command.sos.yaml`'s own header comment). One `command`
//! `Scenario.event` dispatches at `tai_ns = 1_700_000_050_000_000_000` (t = 50 s).
//!
//! **Test 1 (drop).** `demo_command_port_drop.drm.yaml`'s own fault (`drop_ground_cmd`, window
//! `[50s, 51s)`, `rate` unset -> `1.0`) covers the dispatch instant and nothing else in this run.
//! Predicted: the dispatch's own OUT `PortTrafficRecord` on `ground.cmd_out` still appears (the
//! emitter genuinely emitted); no IN record on `flight.cmd_in`, no delivery, so `flight` never
//! applies `accel_scale` and never sends an ack -- `RunProducts.events` carries exactly 4
//! `EVENT_KIND_COMMAND_TRANSITION` events for `"cmd1"` (PROPOSED, CHECKED, AUTHORIZED, DISPATCHED
//! -- never ACKED), zero `EVENT_KIND_PORT_COMMAND` events, 4 `EVENT_KIND_LIFECYCLE` events (2
//! instances x run_start/run_end), and exactly one `EVENT_KIND_FAULT` event
//! (`reference_id = "drop_ground_cmd"`, at `tai_ns = 1_700_000_050_000_000_000` -- the ONE frame
//! the fault actually dropped). **Predicted total: 4 + 4 + 1 = 9 events; 1 PortTrafficLog
//! record.**
//!
//! **Test 2 (delay).** `demo_command_port_delay.drm.yaml`'s own fault (`delay_ground_cmd`,
//! persistent, `delay_s = 1.0` -- exactly one native step) adds 1 s to `ground.cmd_out ->
//! flight.cmd_in`'s own already-declared 3 s latency. `tests/port_traffic_sidecar.rs`'s own
//! module doc comment already measured the UNFAULTED case: `AppliedCommand.applied_tai_ns =
//! dispatch + 2s`, ack emitted at `dispatch + 3s`. Predicted: the faulted run's own
//! `applied_tai_ns` and ack emission epoch are each EXACTLY one 1 s output period later --
//! `dispatch + 3s` and `dispatch + 4s` respectively -- never zero steps (the fault would have no
//! effect) and never two (the delay is 1 s, one period, not 2). This test measures the UNFAULTED
//! baseline itself, live, in-process (never a hand-copied constant from another test file), and
//! asserts the faulted run's own real epochs equal `baseline + output_period_ns` exactly.
//!
//! ## What this file deliberately does NOT duplicate
//!
//! "A PORT fault naming a non-FRAMED port" is pinned at the `crate::router::Router` level
//! (`crates/av-kernel/src/router.rs::tests::install_port_faults_refuses_a_non_framed_port_target`)
//! rather than here: `demo_command`'s own two systems (`drms/demo_command_flight.system.yaml`/
//! `_ground.system.yaml`) declare ONLY FRAMED ports (`cmd_out`/`cmd_in`/`ack_out`/`ack_in`) -- the
//! "cheapest honest vehicle" instruction this task was given means reusing `demo_command`'s own
//! systems, not inventing a SIGNAL/CDM port on them (or a third system) purely to exercise a case
//! `Router`'s own unit test already proves directly and completely, with the identical validation
//! code path `execute()` itself calls.
//!
//! The `Router`-level "two PORT faults on two different ports draw from independent seeded
//! substreams" proof (`router.rs::tests::
//! two_port_faults_on_different_ports_draw_from_independent_seeded_substreams`, which reconstructs
//! 25 candidate frames per port by hand against the real PCG64 algorithm) is the RIGOROUS pinning
//! of question 178's rule 6 -- `demo_command` emits only ONE frame per port for the whole run (one
//! command dispatch, one ack), too few candidate frames to meaningfully demonstrate an "own
//! substream" property through `execute()` alone. This file's own `two_port_faults_wired_through_
//! execute_do_not_interfere` test below is the qualitative, real-run-shaped companion: two PORT
//! faults, two different ports, one run, both apply correctly and independently.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use av_cdm::pb::{DesignReferenceMission, EventKind, Fault, FaultTargetKind, PortDirection, PortTrafficLog, SosConfiguration, SystemDefinition};
use av_kernel::drm::{execute, hash, schema, DrmError, RunConfig};
use av_kernel::router::RouterError;
use gmat_sys::Gmat;
use prost::Message as _;

fn drms_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../drms").join(name)
}
fn read(name: &str) -> String {
    std::fs::read_to_string(drms_path(name)).unwrap_or_else(|e| panic!("reading drms/{name}: {e}"))
}
fn load_system(stem: &str) -> SystemDefinition {
    schema::parse_system_definition_yaml(&read(&format!("{stem}.system.yaml"))).unwrap_or_else(|e| panic!("{stem}.system.yaml: {e}"))
}
fn load_command_systems() -> BTreeMap<String, SystemDefinition> {
    let flight = load_system("demo_command_flight");
    let ground = load_system("demo_command_ground");
    let mut systems = BTreeMap::new();
    systems.insert(flight.id.clone(), flight);
    systems.insert(ground.id.clone(), ground);
    systems
}
fn load_command_sos() -> SosConfiguration {
    schema::parse_sos_yaml(&read("demo_command.sos.yaml")).expect("SosConfiguration parses")
}
fn load_drm(name: &str) -> DesignReferenceMission {
    schema::parse_drm_yaml(&read(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}
fn rehash_drm(mut drm: DesignReferenceMission) -> DesignReferenceMission {
    drm.hash = hash::canonical_drm_hash(&drm);
    drm
}

fn run_config<'a>(gmat: &'a Gmat, drm: &'a DesignReferenceMission, sos: &'a SosConfiguration, systems: &'a BTreeMap<String, SystemDefinition>, run_id: &str, products_dir: Option<PathBuf>) -> RunConfig<'a> {
    RunConfig { gmat, drm, sos, systems, run_id: run_id.to_string(), error_mode: Default::default(), products_dir, replay: None }
}

/// A fresh, unique scratch directory (mirrors `tests/port_traffic_sidecar.rs`'s own
/// `scratch_dir` -- `cargo test` runs this file's own tests in parallel threads of one process,
/// so PID alone is not enough).
fn scratch_dir(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("port-faults-test-{}-{label}-{n}", std::process::id()));
    assert!(!dir.exists(), "scratch dir {dir:?} must not already exist");
    dir
}

fn read_port_traffic_log(path: &std::path::Path) -> PortTrafficLog {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("reading {path:?}: {e}"));
    PortTrafficLog::decode(bytes.as_slice()).unwrap_or_else(|e| panic!("{path:?} did not decode as a PortTrafficLog: {e}"))
}

const START_TAI_NS: i64 = 1_700_000_000_000_000_000;
const COMMAND_TAI_NS: i64 = 1_700_000_050_000_000_000;
const OUTPUT_PERIOD_NS: i64 = 1_000_000_000;

// =================================================================================================
// Test 1: drop
// =================================================================================================

/// See the module doc comment's "Test 1 (drop)" section for the predicted shape, stated before
/// running. Fails against an implementation that: never suppresses delivery at all (ACKED would
/// still appear); suppresses the OUT record too (question 178 rule 4's "keep the OUT, gate the
/// IN" is the whole point); or never emits the FAULT event (rule 7).
#[test]
fn demo_command_port_drop_suppresses_delivery_keeps_the_out_record_and_emits_one_fault_event() {
    let _engine = gmat_sys::engine_lock();
    let drm = load_drm("demo_command_port_drop.drm.yaml");
    let sos = load_command_sos();
    let systems = load_command_systems();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let dir = scratch_dir("drop");

    let products = execute(run_config(&gmat, &drm, &sos, &systems, "test-port-fault-drop", Some(dir.clone()))).expect("demo_command_port_drop executes end to end");

    // -- Events -----------------------------------------------------------------------------
    let transitions: Vec<&str> = products.events.iter().filter(|e| e.kind == EventKind::CommandTransition as i32 && e.reference_id == "cmd1").map(|e| e.name.as_str()).collect();
    assert_eq!(transitions, vec!["COMMAND_STATE_PROPOSED", "COMMAND_STATE_CHECKED", "COMMAND_STATE_AUTHORIZED", "COMMAND_STATE_DISPATCHED"], "no ACKED: the command never reached flight");

    let port_commands: Vec<_> = products.events.iter().filter(|e| e.kind == EventKind::PortCommand as i32).collect();
    assert!(port_commands.is_empty(), "flight must never have applied accel_scale: {port_commands:#?}");

    let fault_events: Vec<_> = products.events.iter().filter(|e| e.kind == EventKind::Fault as i32).collect();
    assert_eq!(fault_events.len(), 1, "{fault_events:#?}");
    assert_eq!(fault_events[0].reference_id, "drop_ground_cmd");
    assert_eq!(fault_events[0].name, "drop_ground_cmd");
    assert_eq!(fault_events[0].tai_ns, COMMAND_TAI_NS, "the fault's own first (and only) applied frame is the dispatch itself");
    assert_eq!(fault_events[0].entity_id, "ground", "the fault's own instance, not flight");

    let lifecycle: Vec<_> = products.events.iter().filter(|e| e.kind == EventKind::Lifecycle as i32).collect();
    assert_eq!(lifecycle.len(), 4, "2 instances x run_start/run_end: {lifecycle:#?}");

    assert_eq!(products.events.len(), 4 + 4 + 1, "predicted total, stated before running: {:#?}", products.events);

    // -- PortTrafficLog sidecar ---------------------------------------------------------------
    let log = read_port_traffic_log(&dir.join("port_traffic.pb"));
    assert_eq!(log.records.len(), 1, "OUT only -- no IN, no ack at all: {:#?}", log.records);
    let rec = &log.records[0];
    assert_eq!(rec.direction, PortDirection::Out as i32);
    assert_eq!(rec.instance, "ground");
    assert_eq!(rec.port, "cmd_out");
    assert_eq!(rec.tai_ns, COMMAND_TAI_NS);

    // No in-flight message was ever queued for this drop (it never reached `pending` at all) --
    // distinct from a message merely never arriving in time.
    assert_eq!(products.dropped_in_flight_messages, 0, "a dropped PORT-fault frame is never queued at all, so it cannot also count as \"in-flight at run end\"");

    let _ = std::fs::remove_dir_all(&dir);
}

// =================================================================================================
// Test 2: delay -- counts steps, derived before measuring (question 178 rule 5)
// =================================================================================================

/// See the module doc comment's "Test 2 (delay)" section. Fails against an implementation that:
/// ignores `params["delay_s"]` entirely (0 steps later, not 1); doubles it (2 steps later, not
/// 1); or applies it as a REPLACEMENT for the connection's own declared latency rather than an
/// addition (would show a nonsensical epoch, likely earlier than the unfaulted baseline).
///
/// **Measured, not assumed: `EVENT_KIND_COMMAND_TRANSITION`'s own ACKED `tai_ns` equals
/// `EVENT_KIND_PORT_COMMAND`'s own `applied_tai_ns`, NOT the ack packet's own real emission
/// epoch** (`tests/drm_command.rs::the_ground_issued_command_drm_runs_through_execute_and_reaches_
/// acked`'s own `assert_eq!(transitions[4].tai_ns, applied_tai_ns, ...)` already established this
/// -- `ConstantAccelModel::step_with_ports`'s "apply, then ack, same step" contract records ACKED
/// at the step's own START epoch, the same epoch `PORT_COMMAND` uses, one period BEFORE the ack's
/// own real `Router`-recorded OUT/IN emission epoch). This test's own step-count proof therefore
/// uses `PORT_COMMAND.applied_tai_ns` (the step-quantized epoch) for the "exactly N steps later"
/// claim, and separately reads the ack's own REAL emission epoch straight off the PortTrafficLog
/// sidecar (`ack_out`'s own OUT record `tai_ns`) rather than off any `Event`.
#[test]
fn demo_command_port_delay_moves_delivery_exactly_one_step_later_than_the_unfaulted_run() {
    let _engine = gmat_sys::engine_lock();
    let systems = load_command_systems();
    let sos = load_command_sos();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");

    // The unfaulted baseline, measured live (not copied from another test file's own numbers).
    let baseline_drm = load_drm("demo_command.drm.yaml");
    let baseline_dir = scratch_dir("delay-baseline");
    let baseline = execute(run_config(&gmat, &baseline_drm, &sos, &systems, "test-port-fault-delay-baseline", Some(baseline_dir.clone()))).expect("baseline demo_command executes");
    let baseline_applied = baseline.events.iter().find(|e| e.kind == EventKind::PortCommand as i32 && e.entity_id == "flight").expect("baseline applies accel_scale exactly once");
    let baseline_ack_transition = baseline.events.iter().find(|e| e.kind == EventKind::CommandTransition as i32 && e.reference_id == "cmd1" && e.name == "COMMAND_STATE_ACKED").expect("baseline reaches ACKED");
    // Sanity against the independently-derived numbers `tests/port_traffic_sidecar.rs`'s own
    // module doc comment already measured for this identical unfaulted fixture.
    assert_eq!(baseline_applied.tai_ns, COMMAND_TAI_NS + 2 * OUTPUT_PERIOD_NS);
    assert_eq!(baseline_ack_transition.tai_ns, baseline_applied.tai_ns, "ACKED lands at the same step-start epoch PORT_COMMAND does, not the ack's own later emission epoch");
    let baseline_log = read_port_traffic_log(&baseline_dir.join("port_traffic.pb"));
    let baseline_ack_emission = baseline_log.records.iter().find(|r| r.port == "ack_out").expect("baseline emits an ack").tai_ns;
    assert_eq!(baseline_ack_emission, COMMAND_TAI_NS + 3 * OUTPUT_PERIOD_NS, "the ack's own REAL emission epoch, off the sidecar");

    // N = delay_ns / period_ns = 1_000_000_000 / 1_000_000_000 = 1 step -- stated before
    // measuring the faulted run, derived from the fixture's own declared params["delay_s"] (1.0
    // s) and this run's own output_period_ns (1.0 s), both plain fixture facts.
    let delay_ns: i64 = 1_000_000_000;
    let n_steps = delay_ns / OUTPUT_PERIOD_NS;
    assert_eq!(n_steps, 1, "sanity on this test's own arithmetic");

    let delayed_drm = load_drm("demo_command_port_delay.drm.yaml");
    let dir = scratch_dir("delay");
    let faulted = execute(run_config(&gmat, &delayed_drm, &sos, &systems, "test-port-fault-delay", Some(dir.clone()))).expect("demo_command_port_delay executes end to end");
    let faulted_applied = faulted.events.iter().find(|e| e.kind == EventKind::PortCommand as i32 && e.entity_id == "flight").expect("the faulted run still eventually applies accel_scale (delay, not drop)");
    let faulted_log = read_port_traffic_log(&dir.join("port_traffic.pb"));
    let faulted_ack_emission = faulted_log.records.iter().find(|r| r.port == "ack_out").expect("the faulted run still emits an ack").tai_ns;

    assert_eq!(faulted_applied.tai_ns, baseline_applied.tai_ns + n_steps * OUTPUT_PERIOD_NS, "delivery must land exactly {n_steps} step(s) later than the unfaulted run, not 0 and not 2");
    assert_eq!(faulted_ack_emission, baseline_ack_emission + n_steps * OUTPUT_PERIOD_NS, "the ack's own REAL emission epoch shifts by the identical {n_steps} step(s)");
    assert_eq!(faulted_applied.values.get("value").copied(), Some(3.0), "the command's own real, physical effect is unchanged -- only ITS TIMING moved");

    // Exactly one EVENT_KIND_FAULT event too, at the dispatch epoch (delay_ground_cmd is
    // persistent from run start, so its own first candidate frame is this run's one and only
    // command dispatch).
    let fault_events: Vec<_> = faulted.events.iter().filter(|e| e.kind == EventKind::Fault as i32).collect();
    assert_eq!(fault_events.len(), 1, "{fault_events:#?}");
    assert_eq!(fault_events[0].reference_id, "delay_ground_cmd");
    assert_eq!(fault_events[0].tai_ns, COMMAND_TAI_NS);

    // PortTrafficLog: both OUT and IN records for the dispatch keep the EMISSION epoch (the
    // module doc comment's own "PortTrafficRecord.tai_ns is always the emission epoch" rule --
    // the fault delay never appears there, only in the delivered PortMessage's own availability,
    // which is not directly observable from the sidecar).
    let cmd_records: Vec<_> = faulted_log.records.iter().filter(|r| r.port == "cmd_out" || r.port == "cmd_in").collect();
    assert_eq!(cmd_records.len(), 2, "{cmd_records:#?}");
    assert!(cmd_records.iter().all(|r| r.tai_ns == COMMAND_TAI_NS), "a delay fault must never appear on PortTrafficRecord.tai_ns, only on the delivered message's own availability: {cmd_records:#?}");

    let _ = std::fs::remove_dir_all(&baseline_dir);
    let _ = std::fs::remove_dir_all(&dir);
}

// =================================================================================================
// Test 3: byte-identical determinism (question 178's own required test)
// =================================================================================================

/// The SAME faulted DRM, executed twice in one process, must produce byte-identical encoded
/// `RunProducts` AND byte-identical `port_traffic.pb` bytes -- compared as raw `Vec<u8>`, never
/// field-by-field, never a hash of a hash. Uses the drop fixture (rate defaults to 1.0 but still
/// draws every candidate frame -- question 178 rule 6 -- so this also indirectly proves that draw
/// is itself reproducible).
///
/// **Both runs share one `products_dir`, deliberately** (the second call's own `port_traffic.pb`
/// overwrites the first's): `RunProducts.provenance.attributes["port_traffic_uri"]` embeds the
/// literal directory path (`executor::execute`'s own "Port traffic sidecar" doc section), so two
/// DIFFERENT scratch directories would make the encoded `RunProducts` differ for a reason that has
/// nothing to do with this task's own determinism claim -- measured directly: an earlier version
/// of this test used two distinct directories and failed on exactly that byte range, not on
/// anything fault-related. `port_traffic.pb`'s own bytes are read back into memory immediately
/// after each run, before the next run can overwrite the file on disk.
#[test]
fn the_same_faulted_drm_executed_twice_produces_byte_identical_run_products_and_port_traffic() {
    let _engine = gmat_sys::engine_lock();
    let drm = load_drm("demo_command_port_drop.drm.yaml");
    let sos = load_command_sos();
    let systems = load_command_systems();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");

    let dir = scratch_dir("det");
    let products_a = execute(run_config(&gmat, &drm, &sos, &systems, "test-port-fault-det", Some(dir.clone()))).expect("run A executes");
    let log_bytes_a = std::fs::read(dir.join("port_traffic.pb")).expect("port_traffic.pb A");
    let products_b = execute(run_config(&gmat, &drm, &sos, &systems, "test-port-fault-det", Some(dir.clone()))).expect("run B executes");
    let log_bytes_b = std::fs::read(dir.join("port_traffic.pb")).expect("port_traffic.pb B");

    let bytes_a = products_a.to_proto().encode_to_vec();
    let bytes_b = products_b.to_proto().encode_to_vec();
    assert_eq!(bytes_a, bytes_b, "encoded RunProducts must be byte-identical across two runs of the same faulted DRM");
    assert!(!bytes_a.is_empty());
    assert_eq!(log_bytes_a, log_bytes_b, "encoded port_traffic.pb must be byte-identical across two runs of the same faulted DRM");
    assert!(!log_bytes_a.is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

// =================================================================================================
// Test 4: distinct typed load errors, refused before any binding or GMAT call
// =================================================================================================

/// A PORT fault naming an undeclared port is `DrmError::Router(RouterError::
/// UndeclaredPortFaultTarget)` -- checked before any binding/GMAT call (the baseline run below
/// never touches GMAT for a REAL propagation of the faulted case; only the unfaulted sanity
/// baseline does, proving the fixture itself is otherwise fine).
#[test]
fn a_port_fault_naming_an_undeclared_port_is_a_typed_load_error() {
    let _engine = gmat_sys::engine_lock();
    let sos = load_command_sos();
    let systems = load_command_systems();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");

    let mut drm = load_drm("demo_command.drm.yaml");
    {
        let scenario = drm.scenario.as_mut().expect("scenario");
        scenario.seeds.insert("f_bad_port".to_string(), 1);
        scenario.faults.push(Fault {
            id: "f_bad_port".to_string(),
            tai_ns: START_TAI_NS,
            duration_ns: 0,
            target_kind: FaultTargetKind::Port as i32,
            instance: "ground".to_string(),
            target: "does_not_exist".to_string(),
            kind: "drop".to_string(),
            ..Default::default()
        });
    }
    let drm = rehash_drm(drm);
    let err = execute(run_config(&gmat, &drm, &sos, &systems, "test-port-fault-undeclared", None)).expect_err("an undeclared port target must be a typed load refusal");
    assert!(
        matches!(&err, DrmError::Router(RouterError::UndeclaredPortFaultTarget { fault_id, instance, port }) if fault_id == "f_bad_port" && instance == "ground" && port == "does_not_exist"),
        "{err:?}"
    );
}

/// A PORT fault whose `kind` is outside ADR-005 section 5's own vocabulary entirely is
/// `DrmError::UnknownPortFaultKind`.
#[test]
fn a_port_fault_naming_an_unknown_kind_is_a_typed_load_error() {
    let _engine = gmat_sys::engine_lock();
    let sos = load_command_sos();
    let systems = load_command_systems();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");

    let mut drm = load_drm("demo_command.drm.yaml");
    {
        let scenario = drm.scenario.as_mut().expect("scenario");
        scenario.seeds.insert("f_bad_kind".to_string(), 1);
        scenario.faults.push(Fault {
            id: "f_bad_kind".to_string(),
            tai_ns: START_TAI_NS,
            duration_ns: 0,
            target_kind: FaultTargetKind::Port as i32,
            instance: "ground".to_string(),
            target: "cmd_out".to_string(),
            kind: "not_a_real_kind".to_string(),
            ..Default::default()
        });
    }
    let drm = rehash_drm(drm);
    let err = execute(run_config(&gmat, &drm, &sos, &systems, "test-port-fault-unknown-kind", None)).expect_err("an unknown PORT fault kind must be a typed load refusal");
    assert!(matches!(&err, DrmError::UnknownPortFaultKind { fault_id, instance, kind } if fault_id == "f_bad_kind" && instance == "ground" && kind == "not_a_real_kind"), "{err:?}");
}

/// A `"corrupt"`/`"duplicate"` PORT fault (R4.1b's own not-yet-implemented scope) is
/// `DrmError::PortFaultKindNotYetSupported`, distinct from `UnknownPortFaultKind`.
#[test]
fn a_port_fault_naming_corrupt_or_duplicate_is_a_distinct_typed_load_error_naming_r4_1b() {
    let _engine = gmat_sys::engine_lock();
    let sos = load_command_sos();
    let systems = load_command_systems();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");

    for kind in ["corrupt", "duplicate"] {
        let mut drm = load_drm("demo_command.drm.yaml");
        {
            let scenario = drm.scenario.as_mut().expect("scenario");
            scenario.seeds.insert("f_unimplemented".to_string(), 1);
            scenario.faults.push(Fault {
                id: "f_unimplemented".to_string(),
                tai_ns: START_TAI_NS,
                duration_ns: 0,
                target_kind: FaultTargetKind::Port as i32,
                instance: "ground".to_string(),
                target: "cmd_out".to_string(),
                kind: kind.to_string(),
                ..Default::default()
            });
        }
        let drm = rehash_drm(drm);
        let err = execute(run_config(&gmat, &drm, &sos, &systems, "test-port-fault-r4-1b", None)).expect_err(&format!("kind {kind:?} must still be a typed load refusal"));
        assert!(matches!(&err, DrmError::PortFaultKindNotYetSupported { fault_id, instance, kind: k } if fault_id == "f_unimplemented" && instance == "ground" && k == kind), "kind {kind:?}: {err:?}");
    }
}

/// A PORT fault with no matching `Scenario.seeds` entry is `DrmError::MissingFaultSeed` (the
/// crate-wide variant, reused -- question 178 rule 6) -- even though `rate` was never declared
/// (defaults to 1.0): the seed check does not wait to see whether a `"rate"` was ever written.
#[test]
fn a_port_fault_missing_its_scenario_seed_is_a_typed_load_error() {
    let _engine = gmat_sys::engine_lock();
    let sos = load_command_sos();
    let systems = load_command_systems();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");

    let mut drm = load_drm("demo_command.drm.yaml");
    {
        let scenario = drm.scenario.as_mut().expect("scenario");
        // Deliberately no `scenario.seeds` entry for "f_no_seed".
        scenario.faults.push(Fault {
            id: "f_no_seed".to_string(),
            tai_ns: START_TAI_NS,
            duration_ns: 0,
            target_kind: FaultTargetKind::Port as i32,
            instance: "ground".to_string(),
            target: "cmd_out".to_string(),
            kind: "drop".to_string(),
            ..Default::default()
        });
    }
    let drm = rehash_drm(drm);
    let err = execute(run_config(&gmat, &drm, &sos, &systems, "test-port-fault-no-seed", None)).expect_err("a PORT fault with no Scenario.seeds entry must be a typed load refusal, even at the implicit default rate 1.0");
    assert!(matches!(&err, DrmError::MissingFaultSeed { fault_id } if fault_id == "f_no_seed"), "{err:?}");
}

// =================================================================================================
// Test 5 (qualitative, execute()-level companion to the rigorous Router-level proof): two PORT
// faults on two different ports in one run do not interfere.
// =================================================================================================

/// Two PORT drop faults, one on `ground.cmd_out` and one on `flight.ack_out`, both with
/// `rate == 1.0` (so the outcome is deterministic regardless of the actual draw -- `uniform() <
/// 1.0` is always true) -- both apply, independently, in the SAME run: the command dispatch is
/// dropped (no IN on `flight.cmd_in`) AND, because `flight` never even receives the command, the
/// ack fault never gets a candidate frame to apply to at all (nothing to drop -- `flight` never
/// emits an ack in the first place). This is a real, if unglamorous, cross-check that installing
/// a SECOND fault does not perturb the first (the rigorous, many-candidate-frame independence
/// proof is `router.rs`'s own unit test -- see the module doc comment's "What this file
/// deliberately does NOT duplicate" section).
#[test]
fn two_port_faults_wired_through_execute_do_not_interfere() {
    let _engine = gmat_sys::engine_lock();
    let sos = load_command_sos();
    let systems = load_command_systems();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");

    let mut drm = load_drm("demo_command.drm.yaml");
    {
        let scenario = drm.scenario.as_mut().expect("scenario");
        scenario.seeds.insert("f_cmd_drop".to_string(), 111);
        scenario.seeds.insert("f_ack_drop".to_string(), 222);
        scenario.faults.push(Fault {
            id: "f_cmd_drop".to_string(),
            tai_ns: START_TAI_NS,
            duration_ns: 0,
            target_kind: FaultTargetKind::Port as i32,
            instance: "ground".to_string(),
            target: "cmd_out".to_string(),
            kind: "drop".to_string(),
            ..Default::default()
        });
        scenario.faults.push(Fault {
            id: "f_ack_drop".to_string(),
            tai_ns: START_TAI_NS,
            duration_ns: 0,
            target_kind: FaultTargetKind::Port as i32,
            instance: "flight".to_string(),
            target: "ack_out".to_string(),
            kind: "drop".to_string(),
            ..Default::default()
        });
    }
    let drm = rehash_drm(drm);
    let products = execute(run_config(&gmat, &drm, &sos, &systems, "test-port-fault-two-faults", None)).expect("both PORT faults install and apply cleanly together");

    // The command dispatch fault applied (real effect: no ACKED, no PORT_COMMAND).
    assert!(!products.events.iter().any(|e| e.kind == EventKind::PortCommand as i32), "{:#?}", products.events);
    let cmd_fault = products.events.iter().find(|e| e.kind == EventKind::Fault as i32 && e.reference_id == "f_cmd_drop").expect("f_cmd_drop must have applied");
    assert_eq!(cmd_fault.tai_ns, COMMAND_TAI_NS);

    // The ack-side fault installed cleanly (no load error) but never got a candidate frame to
    // apply to -- flight never even emits an ack once the command itself never arrived.
    assert!(!products.events.iter().any(|e| e.kind == EventKind::Fault as i32 && e.reference_id == "f_ack_drop"), "f_ack_drop had nothing to apply to and must not fabricate an event: {:#?}", products.events);
}

// =================================================================================================
// Rule 9: a PORT fault's own epoch need not land on the output sampling grid.
// =================================================================================================

/// `DrmError::FaultEpochNotOnSampleGrid` (the DYNAMICS/HARDWARE check) does NOT apply to a PORT
/// fault -- `crate::router`'s own module doc comment's "Epoch grid" section. This fault's own
/// `tai_ns` (49.5 s) is deliberately off the 1 s grid; its window (`[49.5s, 51.5s)`) still covers
/// the real dispatch epoch (50 s), so the run must both LOAD and genuinely apply the fault.
#[test]
fn a_port_fault_off_the_sample_grid_still_loads_and_applies() {
    let _engine = gmat_sys::engine_lock();
    let sos = load_command_sos();
    let systems = load_command_systems();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");

    let mut drm = load_drm("demo_command.drm.yaml");
    {
        let scenario = drm.scenario.as_mut().expect("scenario");
        scenario.seeds.insert("f_off_grid".to_string(), 1);
        scenario.faults.push(Fault {
            id: "f_off_grid".to_string(),
            tai_ns: COMMAND_TAI_NS - 500_000_000, // 49.5 s -- half a step off the 1 Hz grid
            duration_ns: 2_000_000_000,           // window [49.5s, 51.5s), covers the 50s dispatch
            target_kind: FaultTargetKind::Port as i32,
            instance: "ground".to_string(),
            target: "cmd_out".to_string(),
            kind: "drop".to_string(),
            ..Default::default()
        });
    }
    let drm = rehash_drm(drm);
    let products = execute(run_config(&gmat, &drm, &sos, &systems, "test-port-fault-off-grid", None)).expect("a PORT fault's own epoch need not land on the output sampling grid");

    let fault_events: Vec<_> = products.events.iter().filter(|e| e.kind == EventKind::Fault as i32).collect();
    assert_eq!(fault_events.len(), 1, "{fault_events:#?}");
    assert_eq!(fault_events[0].reference_id, "f_off_grid");
    assert_eq!(fault_events[0].tai_ns, COMMAND_TAI_NS, "applied at the real dispatch epoch, the fault's own window merely needed to cover it");
}
