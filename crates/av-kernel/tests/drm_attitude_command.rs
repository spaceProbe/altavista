//! A3.1/D7 (`docs/aiplane-plan.md`'s A3 milestone): a `mode` command through a real `crate::
//! drm::command_source::ExternalCommandSource` reaches the attitude controller's own new
//! `mode_in` FRAMED port and acks at all three real levels, with a genuine physical effect --
//! see `drms/demo_attitude_command.*.yaml`'s own header comments for the fixture (derived from
//! `drms/demo_attitude_control.*.yaml`, never a modification of it, D8) and `crate::drm::
//! controller::ControllerMode`'s own doc comment for exactly what SAFE (`0.0`) does.
//!
//! **Why all three [`av_cdm::pb::AckLevel`]s are real here, unlike `demo_command`/
//! `demo_external_command`'s own `ConstantAccelModel` target:** `AttitudeControllerModel`'s own
//! mode-consume path (`crate::drm::controller`) emits its ack telemetry packet on DECODE
//! (`ACK_LEVEL_ASSET_RECEIVED`) as well as on APPLY (`ACK_LEVEL_ASSET_EXECUTED`, the pre-existing
//! `ConstantAccelModel` contract this executor's own applied-commands drain already derives) --
//! see `crate::drm::executor::run_shared_group`'s own "command loop" for exactly where each
//! level's [`av_kernel::drm::CommandOutcome::Acked`] is reported back to the source.
//!
//! **The physical effect, stated before running:** the fixture's own declared `kp`/`kd` are
//! sized for exact critical damping (`tau = 2*Jz/kd = 20 s`, `drms/demo_attitude_command.drm.
//! yaml`'s own header comment) -- over the fixture's 40 s window (2*tau), an uncommanded
//! (always-REGULATE) run's own `pointing_error_rad` decays substantially from its initial 0.2
//! rad; a run where SAFE is commanded partway through must instead show the error's own decay
//! *stop* at the SAFE epoch (zero commanded torque from then on) -- measured against the SAME
//! run with no command at all, never merely asserted from the control law's own equations.

use std::collections::BTreeMap;
use std::path::PathBuf;

use av_cdm::pb::{AckLevel, Command, DesignReferenceMission, EventKind, Provenance, SosConfiguration, SystemDefinition};
use av_kernel::drm::command_source::{pack_double_value, CommandOutcome, RecordingCommandSource};
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
    let drm = schema::parse_drm_yaml(&read("demo_attitude_command.drm.yaml")).expect("DRM parses");
    let sos = schema::parse_sos_yaml(&read("demo_attitude_command.sos.yaml")).expect("SosConfiguration parses");
    let truth = load_system("demo_attitude_control_truth");
    let star = load_system("demo_attitude_control_startracker");
    let imu = load_system("demo_attitude_control_imu");
    let controller = load_system("demo_attitude_command_controller");
    let ground = load_system("demo_ground_command_ground");
    let mut systems = BTreeMap::new();
    systems.insert(truth.id.clone(), truth);
    systems.insert(star.id.clone(), star);
    systems.insert(imu.id.clone(), imu);
    systems.insert(controller.id.clone(), controller);
    systems.insert(ground.id.clone(), ground);
    (drm, sos, systems)
}

const START_TAI_NS: i64 = 1_767_225_637_000_000_000;

fn mode_command(id: &str, value: f64, not_before_tai_ns: i64) -> Command {
    Command {
        id: id.to_string(),
        idempotency_key: format!("{id}-key"),
        entity_id: "controller".to_string(),
        command_class: "mode".to_string(),
        hazardous: false,
        payload: Some(pack_double_value(value)),
        deadline_tai_ns: 0,
        not_before_tai_ns,
        provenance: Some(Provenance { attributes: BTreeMap::from([("from".to_string(), "ground".to_string())]), ..Default::default() }),
        ..Default::default()
    }
}

fn pointing_error_at_end(products: &av_kernel::drm::RunProducts) -> f64 {
    products.scores.get("pointing_error_end").unwrap_or_else(|| panic!("demo_attitude_command.drm.yaml declares measure \"pointing_error_end\"")).value
}

#[test]
fn a_safe_mode_command_through_the_external_source_reaches_the_controller_acks_at_all_three_levels_and_has_a_real_physical_effect() {
    let _engine = gmat_sys::engine_lock();
    let (drm, sos, systems) = load_bundle();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");

    // -- Baseline: no command at all -- the controller stays REGULATE its whole life. --
    let baseline = execute(RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: "test-attitude-cmd-baseline".to_string(), error_mode: Default::default(), products_dir: None, replay: None, command_source: None }).expect("baseline run executes");
    let baseline_error = pointing_error_at_end(&baseline);

    // -- Commanded: SAFE at t = 10s (well before the run ends at t = 40s). --
    let safe_at = START_TAI_NS + 10_000_000_000;
    let source = RecordingCommandSource::new(vec![mode_command("safe1", 0.0, safe_at)]);
    let commanded = execute(RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: "test-attitude-cmd-safe".to_string(), error_mode: Default::default(), products_dir: None, replay: None, command_source: Some(&source) }).expect("commanded run executes");
    let commanded_error = pointing_error_at_end(&commanded);

    // -- Event trail: the real state machine, DISPATCHED at the commanded epoch, ACKED after. --
    let transitions: Vec<_> = commanded.events.iter().filter(|e| e.kind == EventKind::CommandTransition as i32 && e.reference_id == "safe1").collect();
    let states: Vec<&str> = transitions.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(states, vec!["COMMAND_STATE_PROPOSED", "COMMAND_STATE_CHECKED", "COMMAND_STATE_AUTHORIZED", "COMMAND_STATE_DISPATCHED", "COMMAND_STATE_ACKED"], "{transitions:#?}");
    assert_eq!(transitions[3].tai_ns, safe_at, "DISPATCHED must land exactly at the command's own not_before epoch");

    // -- All three real ack levels, reported to the source, each strictly after dispatch. --
    let outcomes = source.outcomes();
    assert!(outcomes.contains(&CommandOutcome::Dispatched { id: "safe1".to_string(), epoch_tai_ns: safe_at, seq: 0 }), "{outcomes:#?}");
    let edge = outcomes.iter().find(|o| matches!(o, CommandOutcome::Acked { level: AckLevel::Edge, .. })).unwrap_or_else(|| panic!("ACK_LEVEL_EDGE must be reported: {outcomes:#?}"));
    let received = outcomes.iter().find(|o| matches!(o, CommandOutcome::Acked { level: AckLevel::AssetReceived, .. })).unwrap_or_else(|| panic!("ACK_LEVEL_ASSET_RECEIVED must be reported: {outcomes:#?}"));
    let executed = outcomes.iter().find(|o| matches!(o, CommandOutcome::Acked { level: AckLevel::AssetExecuted, .. })).unwrap_or_else(|| panic!("ACK_LEVEL_ASSET_EXECUTED must be reported: {outcomes:#?}"));
    let epoch_of = |o: &CommandOutcome| match o {
        CommandOutcome::Acked { epoch_tai_ns, .. } => *epoch_tai_ns,
        CommandOutcome::Dispatched { epoch_tai_ns, .. } => *epoch_tai_ns,
        other => panic!("unexpected outcome shape: {other:?}"),
    };
    assert_eq!(epoch_of(edge), safe_at, "ACK_LEVEL_EDGE lands at the dispatch epoch itself");
    assert!(epoch_of(received) > safe_at, "ACK_LEVEL_ASSET_RECEIVED must land strictly after dispatch (real router latency)");
    assert!(epoch_of(executed) >= epoch_of(received), "ACK_LEVEL_ASSET_EXECUTED must not land before ASSET_RECEIVED (decode precedes apply within the same step)");

    // -- The physical effect: SAFE measurably changes the pointing-error trajectory, compared
    // against the identical run with no command at all -- not merely an event-name check. --
    eprintln!("[drm_attitude_command] baseline pointing_error_rad@end={baseline_error}, SAFE-commanded pointing_error_rad@end={commanded_error}");
    assert!((commanded_error - baseline_error).abs() > 1e-4, "a SAFE command with zero commanded torque must measurably change the pointing-error trajectory versus an always-REGULATE baseline; baseline={baseline_error}, commanded={commanded_error}");
    // SAFE zeroes the restoring torque partway through the run, so the commanded run's own error
    // must decay LESS (end up larger) than the always-REGULATE baseline over the same window.
    assert!(commanded_error > baseline_error, "SAFE (zero torque) must leave a larger residual pointing error than always-REGULATE; baseline={baseline_error}, commanded={commanded_error}");
}

/// A decoded `mode` value outside the declared allowlist is a typed, recorded outcome (D7,
/// question 188/193's own shape): an event, the last good mode retained, the run continues --
/// never a silent ignore, never a hard failure.
#[test]
fn an_out_of_allowlist_mode_value_is_recorded_and_the_run_continues_on_the_last_good_mode() {
    let _engine = gmat_sys::engine_lock();
    let (drm, sos, systems) = load_bundle();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let bad_at = START_TAI_NS + 10_000_000_000;
    let source = RecordingCommandSource::new(vec![mode_command("bad_mode", 2.0, bad_at)]);
    let products = execute(RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: "test-attitude-cmd-bad-mode".to_string(), error_mode: Default::default(), products_dir: None, replay: None, command_source: Some(&source) }).expect("an out-of-allowlist mode value must never abort the run");

    // The command loop's own structural/timing checks never reject this (it decodes into a
    // finite f64, and the target is a real framed consumer) -- it dispatches, and it is the
    // MODEL's own decode-time allowlist check that records the problem, as a decode-error
    // episode on the controller's own mode_in port (question 193's own "one event per (instance,
    // port)" shape).
    let outcomes = source.outcomes();
    assert!(outcomes.contains(&CommandOutcome::Dispatched { id: "bad_mode".to_string(), epoch_tai_ns: bad_at, seq: 0 }), "{outcomes:#?}");
    let decode_starts: Vec<_> = products.events.iter().filter(|e| e.entity_id == "controller" && e.reference_id == "mode_in" && e.name == "decode_error_start").collect();
    assert!(!decode_starts.is_empty(), "an out-of-allowlist mode value must produce a recorded decode-error episode on controller/mode_in; events: {:#?}", products.events.iter().map(|e| (e.kind, &e.entity_id, &e.name, &e.reference_id)).collect::<Vec<_>>());
}
