//! M25.2 (`docs/sil-plan.md`'s M25 milestone: "DRM command events become CDM `Command`s, framed
//! as CCSDS telecommands, delivered through the router with the link model; ... acknowledged by
//! the flight software's telemetry"): the standing scope rule's own "one DRM fixture instantiates
//! it through `execute()`" requirement for `crate::drm::binding::ConstantAccelModel::
//! consume_framed`/`.ack_framed` and `crate::drm::command`, exercised against
//! `drms/demo_command.*.yaml` (`command_demo_ground_sys` -- a non-physical dispatch instance,
//! `crate::drm::executor::run_shared_group`'s own new command-dispatch code hands the encoded
//! CCSDS telecommand straight to `crate::router::Router::deliver` -- connected over two real
//! FRAMED `crate::router::Router` links, each with a declared, non-zero latency, to
//! `command_demo_flight_sys`, a real `ConstantAccelModel` consuming the telecommand and sending a
//! real ack telemetry packet back).
//!
//! **Expected result, stated before running** (`drms/demo_command.drm.yaml`'s own header comment
//! has the pipeline diagram): exactly 5 `EVENT_KIND_COMMAND_TRANSITION` events for the one
//! declared command ("cmd1"), in `CommandState` order (PROPOSED, CHECKED, AUTHORIZED, DISPATCHED,
//! ACKED -- the REAL enum, never an invented "APPROVED"/"EXECUTED" state, `CHECKED`/`AUTHORIZED`
//! never skipped); exactly one `EVENT_KIND_PORT_COMMAND` event on `flight` naming field
//! `"accel_scale"`, value `3.0`; `flight`'s own final propagated position measurably differs from
//! the uncommanded (`accel_scale` never applied) case, computed from the REAL observed
//! `applied_tai_ns` the `PORT_COMMAND` event itself reports (never a hand-predicted epoch --
//! mirrors `crates/av-kernel/tests/demo_two_instance.rs`'s own methodology for its drag-sail
//! command, `assert_command_epoch_diverges`).
//!
//! Fails against an implementation that never wires `kind == "command"` into `crate::drm::schema`/
//! `crate::drm::executor::execute` at all (the DRM would be refused before propagation starts,
//! `DrmError::UnsupportedScenarioEventKind`); that classifies but never actually calls `crate::
//! router::Router::deliver` (no command would ever reach `flight`'s own `Inbox`, `accel_scale`
//! would stay `1.0` forever, and the position comparison below would show no divergence at all);
//! or whose `ConstantAccelModel::consume_framed`/`.ack_framed` decode never actually runs (the
//! `EVENT_KIND_PORT_COMMAND`/ACKED events would never appear).

use std::collections::BTreeMap;
use std::path::PathBuf;

use av_cdm::pb::{DesignReferenceMission, EventKind, Scenario, SosConfiguration, SystemDefinition};
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

fn load_command_bundle() -> (DesignReferenceMission, SosConfiguration, BTreeMap<String, SystemDefinition>) {
    let drm = schema::parse_drm_yaml(&read("demo_command.drm.yaml")).expect("DRM parses");
    let sos = schema::parse_sos_yaml(&read("demo_command.sos.yaml")).expect("SosConfiguration parses");
    let flight = load_system("demo_command_flight");
    let ground = load_system("demo_command_ground");
    let mut systems = BTreeMap::new();
    systems.insert(flight.id.clone(), flight);
    systems.insert(ground.id.clone(), ground);
    (drm, sos, systems)
}

fn run_config<'a>(gmat: &'a Gmat, drm: &'a DesignReferenceMission, sos: &'a SosConfiguration, systems: &'a BTreeMap<String, SystemDefinition>, run_id: &str) -> RunConfig<'a> {
    RunConfig { gmat, drm, sos, systems, run_id: run_id.to_string(), error_mode: Default::default() , products_dir: None }
}

const START_TAI_NS: i64 = 1_700_000_000_000_000_000;
const COMMAND_TAI_NS: i64 = 1_700_000_050_000_000_000;

#[test]
fn the_ground_issued_command_drm_runs_through_execute_and_reaches_acked() {
    let _engine = gmat_sys::engine_lock();
    let (drm, sos, systems) = load_command_bundle();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let products = execute(run_config(&gmat, &drm, &sos, &systems, "test-run-drm-command")).expect("the ground-issued-command DRM executes end to end");

    // -----------------------------------------------------------------------------------
    // The REAL five-state machine, in order, all for the one declared command ("cmd1").
    // -----------------------------------------------------------------------------------
    let transitions: Vec<_> = products.events.iter().filter(|e| e.kind == EventKind::CommandTransition as i32 && e.reference_id == "cmd1").collect();
    let states: Vec<&str> = transitions.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(
        states,
        vec!["COMMAND_STATE_PROPOSED", "COMMAND_STATE_CHECKED", "COMMAND_STATE_AUTHORIZED", "COMMAND_STATE_DISPATCHED", "COMMAND_STATE_ACKED"],
        "exactly the real CommandState machine, in order -- CHECKED/AUTHORIZED never skipped, no invented APPROVED/EXECUTED state; got {transitions:#?}"
    );
    // PROPOSED/CHECKED/AUTHORIZED land at (or within 2 ns of) scenario start; DISPATCHED lands
    // exactly at the declared command epoch; ACKED lands strictly after (real router latency
    // elapsed both ways, never zero, never instantaneous).
    assert_eq!(transitions[0].tai_ns, START_TAI_NS);
    assert!(transitions[2].tai_ns < COMMAND_TAI_NS, "AUTHORIZED must land before the command's own declared dispatch epoch");
    assert_eq!(transitions[3].tai_ns, COMMAND_TAI_NS, "DISPATCHED must land exactly at the command's own declared epoch, not the (later) arrival epoch");
    assert!(transitions[4].tai_ns > COMMAND_TAI_NS, "ACKED must land strictly after DISPATCHED -- real, non-zero router latency elapsed on both hops (dispatch, then ack)");

    // -----------------------------------------------------------------------------------
    // The real, physically meaningful EVENT_KIND_PORT_COMMAND application on `flight`.
    // -----------------------------------------------------------------------------------
    let port_commands: Vec<_> = products.events.iter().filter(|e| e.kind == EventKind::PortCommand as i32 && e.entity_id == "flight").collect();
    assert_eq!(port_commands.len(), 1, "exactly one applied command on flight; got {port_commands:#?}");
    let applied = port_commands[0];
    assert_eq!(applied.name, "accel_scale");
    assert_eq!(applied.values.get("value").copied(), Some(3.0));
    let applied_tai_ns = applied.tai_ns;
    assert!(applied_tai_ns > COMMAND_TAI_NS, "the command can only be applied after real router latency delivers it");
    // The ACKED transition must land at exactly the same epoch the command was actually applied
    // (ConstantAccelModel::step_with_ports's own "apply, then ack, same step" contract -- proven
    // directly, unit-level, by drm::binding::tests::consume_framed_applies_within_the_same_step_
    // reports_it_and_sends_an_ack).
    assert_eq!(transitions[4].tai_ns, applied_tai_ns, "ACKED must land at the exact epoch the command was actually applied");

    // -----------------------------------------------------------------------------------
    // The physical effect: real, measurable, computed from the REAL observed applied_tai_ns
    // (never a hand-predicted epoch -- crates/av-kernel/tests/demo_two_instance.rs's own
    // methodology for its own drag-sail command).
    // -----------------------------------------------------------------------------------
    let traj = products.trajectories.get("flight").expect("flight produces a trajectory");
    let last = traj.samples.last().expect("flight has at least one sample");
    let end_tai_ns = last.tai_ns;

    let t_apply_s = (applied_tai_ns - START_TAI_NS) as f64 / 1e9;
    let t_end_s = (end_tai_ns - START_TAI_NS) as f64 / 1e9;
    let base_a = 1.0_f64; // drms/demo_command_flight.system.yaml's own declared accel.z
    let commanded_scale = 3.0_f64; // this DRM's own declared command value
    // Phase 1: [0, t_apply), a = base_a. Phase 2: [t_apply, t_end], a = base_a * commanded_scale.
    // Closed-form double integrator, exact for a piecewise-constant acceleration (RK4 is exact
    // for a polynomial of degree <= 3, and each phase's own position is degree 2 in time).
    let v_at_apply = base_a * t_apply_s;
    let z_at_apply = 0.5 * base_a * t_apply_s * t_apply_s;
    let dt2 = t_end_s - t_apply_s;
    let a2 = base_a * commanded_scale;
    let expected_z_end = z_at_apply + v_at_apply * dt2 + 0.5 * a2 * dt2 * dt2;

    eprintln!("[drm_command] applied_tai_ns={applied_tai_ns} (t={t_apply_s}s), end_tai_ns={end_tai_ns} (t={t_end_s}s), expected pos_z={expected_z_end}, actual pos_z={}", last.mean[2]);
    assert!((last.mean[2] - expected_z_end).abs() < 1e-6, "flight's own final pos_z must match the closed-form piecewise-constant-acceleration prediction; expected {expected_z_end}, got {}", last.mean[2]);

    // Sanity: this is not a vacuous "nothing changed" comparison -- the commanded arc must
    // measurably diverge from the never-commanded baseline (accel_scale stays 1.0 the whole run).
    let uncommanded_z_end = 0.5 * base_a * t_end_s * t_end_s;
    let divergence = (last.mean[2] - uncommanded_z_end).abs();
    eprintln!("[drm_command] commanded-vs-never-commanded final pos_z divergence = {divergence} m");
    assert!(divergence > 1.0, "the accel_scale command must have a real, non-vacuous effect; measured only {divergence} m of divergence");
}

/// **The `CommandTargetNotFramedConsumer` typed refusal (M25.2): a `command` event naming a
/// target that never declared `port.consume_framed` must be refused, never silently dropped.**
/// Fails against an implementation that dispatches anyway (encoding a packet nothing ever
/// decodes) or that panics on the missing codec instead of returning this typed error.
#[test]
fn a_command_targeting_an_instance_with_no_consume_framed_port_is_a_typed_refusal() {
    let _engine = gmat_sys::engine_lock();
    let (mut drm, sos, systems) = load_command_bundle();
    {
        let scenario = drm.scenario.as_mut().expect("scenario present");
        scenario.events[0].instance = "ground".to_string(); // ground never declares consume_framed
    }
    drm.hash = hash::canonical_drm_hash(&drm);
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let err = execute(run_config(&gmat, &drm, &sos, &systems, "test-run-drm-command-bad-target")).unwrap_err();
    assert!(matches!(err, DrmError::CommandTargetNotFramedConsumer { ref id, ref instance } if id == "cmd1" && instance == "ground"), "{err:?}");
}

/// A `command` event's `attributes["from"]` naming an instance not in this `SosConfiguration` is
/// refused at load, before any GMAT call or propagation -- mirrors `DrmError::
/// UnknownManeuverInstance`'s own existing "typed, up front" contract for the sibling `kind`.
#[test]
fn a_command_naming_an_unknown_sender_is_a_typed_refusal() {
    let _engine = gmat_sys::engine_lock();
    let (mut drm, sos, systems) = load_command_bundle();
    {
        let scenario = drm.scenario.as_mut().expect("scenario present");
        scenario.events[0].attributes.insert("from".to_string(), "no_such_instance".to_string());
    }
    drm.hash = hash::canonical_drm_hash(&drm);
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let err = execute(run_config(&gmat, &drm, &sos, &systems, "test-run-drm-command-bad-sender")).unwrap_err();
    assert!(matches!(err, DrmError::UnknownCommandSender { ref id, ref sender, .. } if id == "cmd1" && sender == "no_such_instance"), "{err:?}");
}

/// A bare `pb::Scenario` built directly (bypassing `crate::drm::schema`'s own YAML loader)
/// still refuses an unrecognized `ScenarioEvent.kind` -- proves the run-time `command::parse`
/// call in `executor::execute` is not merely load-time cosmetics (mirrors this crate's own
/// existing "the typed contract is not bypassable" note on the identical `maneuver` check).
#[test]
fn scenario_events_of_an_unrecognized_kind_are_still_refused_at_run_time() {
    let _engine = gmat_sys::engine_lock();
    let (mut drm, sos, systems) = load_command_bundle();
    {
        let scenario: &mut Scenario = drm.scenario.as_mut().expect("scenario present");
        scenario.events[0].kind = "not_a_real_kind".to_string();
    }
    drm.hash = hash::canonical_drm_hash(&drm);
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let err = execute(run_config(&gmat, &drm, &sos, &systems, "test-run-drm-command-bad-kind")).unwrap_err();
    assert!(matches!(err, DrmError::UnsupportedScenarioEventKind { ref kind, .. } if kind == "not_a_real_kind"), "{err:?}");
}
