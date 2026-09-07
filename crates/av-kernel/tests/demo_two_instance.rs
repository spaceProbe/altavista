//! M17.3's required acceptance tests (`docs/open-questions.md` question 123: "a two-instance
//! DRM fixture with a real fault on one instance and a maneuver on the other becomes a golden
//! and is the demo bundle"). `drms/demo_two_instance.{drm,sos,system}.yaml` bind two
//! `BINDING_KIND_MODEL` instances -- `demo_flt` and `demo_mvr` -- to the same real GMAT-bound
//! "golden LEO dynamics" (`leo_demo_sys`, field-for-field the same vehicle and JGM2 8x8 +
//! Sun/Moon force model as `drms/leo_1day_golden.system.yaml`'s own `leo_sys`):
//!
//! * `demo_flt` carries a real `FAULT_TARGET_KIND_DYNAMICS` fault mid-run
//!   (`force_model.gravity_order`, 8 -> 0 -- a genuine, physically measurable change to the
//!   propagated force model, not the synthetic `"accel.x"` fixture every other fault test in
//!   this crate uses).
//! * `demo_mvr` carries the same 20 m/s prograde VNB burn `leo_1day_maneuver_vnb` pins.
//! * `demo_mvr` declares `initial_covariance` (the golden's own P0); `demo_flt` does not.
//! * `demo_two_instance.drm.yaml` declares one `Objective` scoring `output.demo_flt.rmag@end`.
//!
//! * `demo_two_instance.sos.yaml` also declares a THIRD instance, `demo_ctrl`
//!   (`drms/demo_two_instance_ctrl.system.yaml`, a native range-condition controller) and a
//!   `SosConfiguration.connections` chain `demo_mvr -> demo_ctrl -> demo_flt` (M18.3 opened the
//!   SIGNAL path, `docs/open-questions.md` question 126; M19.4, question 131, closes the
//!   escalation M18.3 itself recorded -- "range magnitude fed into Cd on an instance with no
//!   drag" -- by routing the command through a controller that evaluates a declared range
//!   condition, and by giving `demo_flt`'s own force model real atmospheric drag): `demo_mvr`
//!   emits its own live `output.rmag` on `cd_cmd_out`; `demo_ctrl` consumes it and, the first
//!   native step it rises to or past a declared threshold near `demo_mvr`'s own apoapsis, emits
//!   a single latched drag-sail `Cd` command (`220.0`, ~100x `demo_flt`'s own baseline `Cd =
//!   2.2`) that `demo_flt` consumes into its own `Cd`
//!   (`crate::drm::binding::GMAT_WRITABLE_PARAMETERS`). `demo_flt`'s own force model now
//!   genuinely includes drag (`force_model.drag_model = "JacchiaRoberts"`, the packaged CSSI
//!   space-weather file this repository's own GMAT install ships), so this command visibly
//!   changes its propagated arc -- `demo_two_instance_signal_port_delivers_a_drag_sail_command_
//!   that_measurably_changes_the_arc` below checks the delivered value AND the resulting
//!   position divergence, not merely that the DRM loads and runs; a small, exact, enumerable
//!   port-command-event count (this task's own regression fix -- the pre-M19.4 "consume every
//!   step, forever" wiring produced ~72,000 events over this same window) is checked by
//!   `demo_two_instance_produces_a_small_number_of_port_command_events_not_thousands`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::OnceLock;

use av_cdm::pb::{
    frame_definition, AxesKind, Binding, BindingKind, DesignReferenceMission, DrmOptions, EventKind, Fault, FaultTargetKind, ModelBinding, Parameter, Scenario, ScenarioEvent, SosConfiguration,
    SystemDefinition, SystemInstance, Unit,
};
use av_kernel::drm::{execute, hash, schema, DrmError, RunConfig, RunProducts};
use gmat_sys::Gmat;
use serde::Deserialize;

fn drms_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../drms").join(name)
}

/// **A GMAT object-namespace hazard, discovered here, fixed at the source by M18.4
/// (`docs/open-questions.md` question 127).** Through M18.3, reusing an identical `SystemInstance
/// .name` bound to a real `"gmat.*"` system for TWO INDEPENDENT `execute()` calls anywhere in this
/// test binary's process (same `#[test]` function or a different one -- both reproduced while
/// diagnosing this) reliably failed with a GMAT error ("Attempted to add a GravityField force to
/// the force model for the body Earth, but there is already a GravityField force in place for
/// that body") -- confirmed with a minimal reproduction: two single-instance, no-fault-no-maneuver
/// `execute()` calls, both naming the instance `"demo_flt"` and nothing else different -- the
/// second failed; renaming only the second instance to `"demo_flt_2"` made both succeed. GMAT's
/// configuration manager is process-global, and every GMAT object `crate::drm::binding::
/// materialize_gmat` constructed was named from `name_suffix` alone (instance name + segment
/// index) -- unique *within* one `execute()` call, but not across two. Nothing before this
/// crate's own bystander-invariance methodology (inherently needing an alone run AND a together
/// run using the SAME instance names, to make the comparison meaningful) ever exercised reusing a
/// `"gmat.*"`-bound name at all, so the hazard went undiscovered until this file.
///
/// **M18.4 fixes this at the source** (`crate::drm::binding::materialize_gmat`'s own doc comment
/// has the full account): every GMAT object name now also folds in `gmat_ns`, unique per
/// `execute()` **invocation** (`crate::drm::executor::gmat_execution_namespace`), never
/// `name_suffix` alone -- so reusing `"demo_flt"`/`"demo_mvr"` across independent `execute()`
/// calls in this same test binary is no longer a hazard at all (`tests/drm_executor.rs::
/// running_the_identical_drm_twice_in_one_process_produces_byte_identical_products` is the direct,
/// minimal proof). This file still routes every use of the real committed fixture's own literal
/// instance names (`"demo_flt"`/`"demo_mvr"`) through [`together_products`] below (a `OnceLock`,
/// shared by every test that needs the real committed DRM's own result) rather than a fresh
/// `execute()` call per `#[test]` function -- not to avoid a collision any more, but because
/// re-running the real ~2 h LEO arc through GMAT repeatedly, once per test function, would be
/// needless work for a result every one of them can share.
///
/// **Caller must already hold `gmat_sys::engine_lock()`.** This function does NOT take the
/// lock itself -- `std::sync::Mutex` (what `engine_lock()` wraps) is not reentrant, and every
/// caller in this file already takes `_engine = gmat_sys::engine_lock()` at the top of its own
/// `#[test]` function per this crate's existing convention (`tests/restart_invariance.rs` etc.),
/// so a caller that happened to be the very first to reach this function's own `execute()` call
/// (via `OnceLock::get_or_init`) would deadlock against itself if this function tried to lock
/// again.
fn together_products() -> &'static RunProducts {
    static ONCE: OnceLock<RunProducts> = OnceLock::new();
    ONCE.get_or_init(|| {
        let (drm, sos, systems) = load_demo_bundle();
        let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
        let cfg = RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: "test-demo-together".to_string(), error_mode: Default::default() };
        execute(cfg).expect("the two-instance demo DRM executes end to end")
    })
}

fn load_demo_bundle() -> (DesignReferenceMission, SosConfiguration, BTreeMap<String, SystemDefinition>) {
    let drm = schema::parse_drm_yaml(&std::fs::read_to_string(drms_path("demo_two_instance.drm.yaml")).unwrap()).expect("DRM parses");
    let sos = schema::parse_sos_yaml(&std::fs::read_to_string(drms_path("demo_two_instance.sos.yaml")).unwrap()).expect("SosConfiguration parses");
    let sys = schema::parse_system_definition_yaml(&std::fs::read_to_string(drms_path("demo_two_instance.system.yaml")).unwrap()).expect("SystemDefinition parses");
    // M19.4 (question 131): `demo_ctrl`'s own native SystemDefinition, a separate file (like
    // `leo_demo_sys` above) since `schema::parse_system_definition_yaml` parses exactly one
    // `SystemDefinition` per document.
    let ctrl_sys = schema::parse_system_definition_yaml(&std::fs::read_to_string(drms_path("demo_two_instance_ctrl.system.yaml")).unwrap()).expect("demo_ctrl's own SystemDefinition parses");
    let mut systems = BTreeMap::new();
    systems.insert(sys.id.clone(), sys);
    systems.insert(ctrl_sys.id.clone(), ctrl_sys);
    (drm, sos, systems)
}

#[derive(Deserialize)]
struct Golden {
    state_final_demo_flt: Vec<f64>,
    rmag_at_end_flt_m: f64,
    state_post_burn_demo_mvr: Vec<f64>,
    state_final_demo_mvr: Vec<f64>,
    maneuver_epoch_s: f64,
    /// demo_flt's own tolerance (M19.4, question 131): 1.0 m, matching `gmat_port_cd_command
    /// .json`'s own tolerance for the identical class of comparison (a SIGNAL-commanded Cd
    /// change on a drag-inclusive force model) -- see `goldens/gen_demo_two_instance.py`'s own
    /// `--tolerance-m` help text for the sub-second command-epoch-determination sensitivity this
    /// is sized against. Loosened from the pre-M19.4 golden's shared 0.05 m specifically for
    /// demo_flt (disclosed here and in the report); demo_mvr's own comparison keeps that tighter
    /// bound unchanged -- see `tolerance_m_demo_mvr` below.
    tolerance_m: f64,
    tolerance_mps: f64,
    /// demo_mvr's own tolerance: UNCHANGED by M19.4 (demo_mvr's own physics -- no drag, no
    /// SIGNAL command -- are untouched by this task).
    tolerance_m_demo_mvr: f64,
    tolerance_mps_demo_mvr: f64,
}

fn golden() -> Golden {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../goldens/demo_two_instance.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn dr_dv(sample: &[f64], golden_state_km: &[f64]) -> (f64, f64) {
    let golden_si = av_cdm::units::state_km_to_m(<[f64; 6]>::try_from(golden_state_km).expect("6-element state"));
    let dr = (0..3).map(|i| (sample[i] - golden_si[i]).powi(2)).sum::<f64>().sqrt();
    let dv = (3..6).map(|i| (sample[i] - golden_si[i]).powi(2)).sum::<f64>().sqrt();
    (dr, dv)
}

// ==========================================================================================
// 1. The real, committed two-instance DRM matches the golden AND the altavista maneuver path,
//    and its declared Objective scores correctly.
// ==========================================================================================

/// Required tests: "the two-instance run matches the golden" and "the Objective scores and its
/// `passed` is what you expect". Fails against: a DRM executor that mis-binds either instance
/// to the wrong system, applies the fault/maneuver at the wrong epoch or to the wrong
/// instance, or an `Objective` evaluator that reads the wrong instance's own `output.rmag` or
/// computes `passed` from the wrong comparison (`|value - target| <= tolerance` is
/// `Objective`'s own contract -- see `crates/av-kernel/src/expr`'s module doc comment).
#[test]
fn demo_two_instance_matches_the_golden_and_scores_its_objective() {
    let _engine = gmat_sys::engine_lock();
    let (drm, _sos, _systems) = load_demo_bundle();
    let g = golden();
    let products = together_products();

    let traj_flt = products.trajectories.get("demo_flt").expect("demo_flt produced a trajectory");
    let traj_mvr = products.trajectories.get("demo_mvr").expect("demo_mvr produced a trajectory");

    // demo_flt: final state matches the golden's own faulted arc (altavista's own
    // ForceModel(Order=0) reconstruction, not a hand re-derivation).
    let last_flt = traj_flt.samples.last().expect("demo_flt has at least one sample");
    let (dr, dv) = dr_dv(&last_flt.mean, &g.state_final_demo_flt);
    eprintln!("[demo_two_instance] demo_flt final: |dr| = {dr:.4} m (tol {}), |dv| = {dv:.3e} m/s (tol {})", g.tolerance_m, g.tolerance_mps);
    assert!(dr < g.tolerance_m, "demo_flt final position error {dr} m exceeds tolerance {} m", g.tolerance_m);
    assert!(dv < g.tolerance_mps, "demo_flt final velocity error {dv} m/s exceeds tolerance {} m/s", g.tolerance_mps);

    // demo_mvr: sample at the burn epoch and the final sample match the golden's own VNB burn
    // path (altavista.scenario.Scenario.maneuver, frame="VNB" -- the same reference
    // implementation goldens/gen_leo_1day_maneuver_vnb.py already pins). demo_mvr's own physics
    // are untouched by M19.4 (no drag, no SIGNAL command), so this keeps the tight pre-M19.4
    // tolerance (`tolerance_m_demo_mvr`), not the loosened `tolerance_m` demo_flt now uses.
    let maneuver_tai_ns = drm.scenario.as_ref().unwrap().start_tai_ns + (g.maneuver_epoch_s as i64) * 1_000_000_000;
    let at_burn = traj_mvr.samples.iter().find(|s| s.tai_ns == maneuver_tai_ns).expect("a demo_mvr sample at the burn epoch");
    let (dr_post, dv_post) = dr_dv(&at_burn.mean, &g.state_post_burn_demo_mvr);
    eprintln!("[demo_two_instance] demo_mvr post-burn: |dr| = {dr_post:.4} m (tol {}), |dv| = {dv_post:.3e} m/s (tol {})", g.tolerance_m_demo_mvr, g.tolerance_mps_demo_mvr);
    assert!(dr_post < g.tolerance_m_demo_mvr, "demo_mvr post-burn position error {dr_post} m exceeds tolerance {} m", g.tolerance_m_demo_mvr);
    assert!(dv_post < g.tolerance_mps_demo_mvr, "demo_mvr post-burn velocity error {dv_post} m/s exceeds tolerance {} m/s", g.tolerance_mps_demo_mvr);

    let last_mvr = traj_mvr.samples.last().expect("demo_mvr has at least one sample");
    let (dr_final, dv_final) = dr_dv(&last_mvr.mean, &g.state_final_demo_mvr);
    eprintln!("[demo_two_instance] demo_mvr final: |dr| = {dr_final:.4} m (tol {}), |dv| = {dv_final:.3e} m/s (tol {})", g.tolerance_m_demo_mvr, g.tolerance_mps_demo_mvr);
    assert!(dr_final < g.tolerance_m_demo_mvr, "demo_mvr final position error {dr_final} m exceeds tolerance {} m", g.tolerance_m_demo_mvr);
    assert!(dv_final < g.tolerance_mps_demo_mvr, "demo_mvr final velocity error {dv_final} m/s exceeds tolerance {} m/s", g.tolerance_mps_demo_mvr);

    // The declared Objective: output.demo_flt.rmag@end, target/tolerance taken straight from
    // the golden's own recorded rmag_at_end_flt_m (drms/demo_two_instance.drm.yaml's own
    // header comment records the exact value and where it came from).
    let score = products.scores.get("demo_flt_rmag_at_end").expect("the declared Objective evaluated");
    let rmag_err = (score.value - g.rmag_at_end_flt_m).abs();
    eprintln!("[demo_two_instance] Objective demo_flt_rmag_at_end: value {} m, golden {} m, |err| {rmag_err:.6e} m", score.value, g.rmag_at_end_flt_m);
    assert!(rmag_err < g.tolerance_m, "output.demo_flt.rmag@end disagrees with the golden by {rmag_err} m");
    assert_eq!(score.passed, Some(true), "the declared target/tolerance were set from this same golden value, so the Objective must pass");
    assert_eq!(score.unit, Unit::Meter);
}

// ==========================================================================================
// 1b. The SIGNAL chain (M18.3, question 126; M19.4, question 131) delivers a real, physically
//     meaningful drag-sail Cd command through the native range-condition controller, closing
//     the escalation this file's own module doc comment used to record: "range magnitude fed
//     into Cd on an instance with no drag."
// ==========================================================================================

/// **Fails against a DRM that merely loads, or against the pre-M19.4 physically-meaningless
/// wiring.** Checks the delivered *value* (220.0, `demo_ctrl`'s own declared `port.emit_value`
/// -- not `demo_mvr`'s live `rmag`, which is what the pre-M19.4 wiring delivered and question 131
/// itself named the defect) AND that `demo_flt`'s own force model actually has drag, by measuring
/// a real, non-vacuous position divergence against a structurally identical run whose Cd is never
/// commanded. Fails against: `parse_gmat_spec`/`parse_constant_accel_spec` refusing the new
/// parameters (the DRM would not even load); `demo_ctrl`'s own condition never firing (`demo_flt_
/// cd_at_end` would stay `2.2`); the pre-M19.4 direct `demo_mvr` -> `demo_flt` wiring (this
/// fixture's own `SosConfiguration.connections` would only have one entry, not two, and `demo_flt
/// _cd_at_end` would equal `demo_mvr`'s own rmag, not `220.0`); or `demo_flt`'s own force model
/// missing drag (the arc would not measurably change even though Cd genuinely moved -- exactly
/// M18.3's own disclosed "no propagated physical effect" case this task closes).
#[test]
fn demo_two_instance_signal_port_delivers_a_drag_sail_command_that_measurably_changes_the_arc() {
    let _engine = gmat_sys::engine_lock();
    let products = together_products();

    // At most the very last emission on each hop can end up in flight (question 108/110: it
    // becomes available only at its own emission epoch, and nothing steps the receiver again
    // after the run's last shared step to drain it). Two hops (demo_mvr -> demo_ctrl -> demo_flt)
    // means at most 2 messages may still be in flight when the run ends.
    assert!(products.dropped_in_flight_messages <= 2, "at most the final emission on each of the two hops may still be in flight when the run ends; got {}", products.dropped_in_flight_messages);

    let cd_score = products.scores.get("demo_flt_cd_at_end").expect("the declared demo_flt_cd_at_end MeasureOfEffectiveness evaluated");
    eprintln!("[demo_two_instance] SIGNAL chain: demo_flt.cd@end = {} (unitless)", cd_score.value);
    assert!((cd_score.value - 220.0).abs() < 1e-9, "demo_flt's own Cd must equal demo_ctrl's declared port.emit_value (220.0) exactly, not demo_mvr's live rmag (the pre-M19.4, physically-meaningless wiring question 131 closes); got {}", cd_score.value);
    assert_eq!(cd_score.unit, Unit::Dimensionless);

    // Real, non-vacuous physical effect: measured against a structurally identical run
    // (identical fault, identical drag, NO SIGNAL wiring at all) whose Cd stays at the declared
    // baseline the whole time.
    let (_drm, sos, systems) = load_demo_bundle();
    let real_flt = sos.instances.iter().find(|i| i.name == "demo_flt").expect("demo_flt instance");
    let scenario = Scenario { start_tai_ns: START_TAI_NS, end_tai_ns: END_TAI_NS, faults: vec![demo_fault_named("demo_flt_uncommanded")], ..Default::default() };
    let (sos_nc, drm_nc) = one_instance_drm("demo_flt_uncommanded", model_instance("demo_flt_uncommanded", vec![], real_flt.parameter_overrides.clone()), scenario, demo_options());
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let cfg_nc = RunConfig { gmat: &gmat, drm: &drm_nc, sos: &sos_nc, systems: &systems, run_id: "test-demo-flt-uncommanded".to_string(), error_mode: Default::default() };
    let products_nc = execute(cfg_nc).expect("demo_flt, run alone with the identical fault and drag but no SIGNAL wiring, executes");
    let traj_nc = products_nc.trajectories.get("demo_flt_uncommanded").unwrap();
    let traj_commanded = products.trajectories.get("demo_flt").unwrap();
    let last_nc = traj_nc.samples.last().unwrap();
    let last_commanded = traj_commanded.samples.last().unwrap();
    let dr = (0..3).map(|i| (last_nc.mean[i] - last_commanded.mean[i]).powi(2)).sum::<f64>().sqrt();
    eprintln!("[demo_two_instance] SIGNAL chain: commanded-vs-uncommanded final position divergence = {dr:.4} m (expect low tens of meters, this file's own module doc comment)");
    assert!(dr > 10.0, "the drag-sail Cd command must measurably change demo_flt's own propagated arc; measured only {dr} m of divergence");
    assert!(dr < 1000.0, "measured divergence {dr} m is implausibly large for this task's own expected order of magnitude (low tens of meters) -- check for a units or configuration error before trusting this number");
}

/// **The size regression this task fixes (M19.4, question 131): the pre-M19.4 direct `demo_mvr
/// -> demo_flt` wiring (consumed every native step, forever) produced 71,999 port-command events
/// over this same 7200 s window -- measured directly while diagnosing the regression this task's
/// own brief names. The lead's decision (a command issued when a declared range condition holds)
/// reduces this to exactly 1.** Fails against: any implementation that emits/consumes
/// unconditionally every step (would reproduce the ~72,000-event regression); one that re-emits
/// every step the condition remains true rather than latching (would emit a burst of events for
/// as long as `demo_mvr`'s own rmag stays above the declared threshold, tens to hundreds of
/// events depending on the window, not exactly one); or one that resets its latch at a
/// re-materialization boundary while the condition is already true (would emit more than once --
/// see `crate::drm::binding::ConstantAccelModel`'s own doc comment on why this demo's own
/// threshold is deliberately chosen to avoid that).
#[test]
fn demo_two_instance_produces_a_small_number_of_port_command_events_not_thousands() {
    let _engine = gmat_sys::engine_lock();
    let products = together_products();
    let port_commands: Vec<&av_cdm::pb::Event> = products.events.iter().filter(|e| e.kind == EventKind::PortCommand as i32).collect();
    eprintln!("[demo_two_instance] total events = {}, EVENT_KIND_PORT_COMMAND events = {}", products.events.len(), port_commands.len());
    assert_eq!(port_commands.len(), 1, "this fixture's own edge-triggered, latched range condition must produce exactly one port-command event over the whole 7200 s run, not the pre-M19.4 regression's 71,999 (every-step consumption) or some other non-vacuous-but-wrong count; got {port_commands:#?}");

    let cmd = port_commands[0];
    assert_eq!(cmd.tai_ns, COMMAND_TAI_NS, "the single port-command event must fire at the independently-derived, deterministic epoch demo_ctrl's own declared range condition crosses (goldens/gen_demo_two_instance.py's own _find_command_epoch_s agrees with this constant to within 0.2 s -- see COMMAND_TAI_NS's own doc comment)");
    assert_eq!(cmd.entity_id, "demo_flt", "the command is applied to demo_flt (the consumer), not demo_ctrl (the sender) or demo_mvr (the original data source)");
    assert_eq!(cmd.name, "Cd", "the commanded field is Cd, GMAT_WRITABLE_PARAMETERS's own declared-writable target");
    assert_eq!(cmd.reference_id, "cd_cmd_in", "the port name is demo_flt's own declared consume port");
    assert_eq!(cmd.values.get("value").copied(), Some(220.0), "the commanded value is demo_ctrl's own declared port.emit_value (220.0), not demo_mvr's live rmag");
    let provenance = cmd.provenance.as_ref().expect("EVENT_KIND_PORT_COMMAND carries provenance (question 130: instance, parameter, value, epoch, sender)");
    assert_eq!(provenance.attributes.get("sender").map(String::as_str), Some("demo_ctrl"), "the sender attribution must name demo_ctrl (the actual emitter), not demo_mvr (the original data source two hops upstream)");
    assert_eq!(provenance.attributes.get("instance").map(String::as_str), Some("demo_flt"));
    assert_eq!(provenance.attributes.get("parameter").map(String::as_str), Some("Cd"));

    // The dynamics_hash / configuration-hash contract (question 130): the command bypasses the
    // hashed spec by design, so demo_flt's own dynamics_hash must be the SAME before and after
    // the command epoch (no segment opens per command) -- checked structurally by
    // demo_two_instance_bystander_invariance_against_real_single_instance_gmat_runs's own segment
    // assertion (`segment counts: demo_flt alone=2 together=2`); referenced here rather than
    // re-checked, since that test is the one that actually builds the alone-run comparison this
    // needs.
}

/// M20.1 (`docs/open-questions.md` question 133) gave `demo_ctrl` (a native controller with
/// no physical trajectory of its own) its own, non-physical state space -- six unitless
/// scalars, `native.controller.scalar6` -- so it stopped being rendered as a spacecraft.
/// **M21.3 (question 141, decided by the lead, closing question 133's own escalation) goes
/// one step further: `demo_ctrl` now declares an EMPTY state space, and the producer emits NO
/// trajectory entry for it at all** -- not a zero-width one, not an empty-sample one, nothing
/// in `RunProducts.trajectories` for `demo_ctrl` -- while its own lifecycle
/// (`run_start`/`run_end`) and port-command events remain on the timeline, proven end to end
/// through the real committed fixture and the real executor, not merely a synthetic unit
/// test.
///
/// Fails against the pre-M21.3 fixture (`state_space_id == "native.controller.scalar6"`, six
/// still-present unitless scalars -- M20.1's own compromise, forced by
/// `ConstantAccelModel::state_dim()` being a fixed 6 regardless of what a fixture declared),
/// which would still produce a `demo_ctrl` trajectory entry with six-component samples; also
/// fails against an implementation that declares the empty state space but still emits an
/// entry (a zero-width `Trajectory`, or one with empty-`mean` samples) instead of omitting the
/// map entry entirely, and against one that (while fixing the trajectory) drops `demo_ctrl`'s
/// own events too -- the brief's explicit "skip for rendering, keep on the timeline" split.
#[test]
fn demo_ctrl_declares_an_empty_state_space_and_emits_no_trajectory_but_keeps_its_events() {
    let _engine = gmat_sys::engine_lock();
    let (_drm, _sos, systems) = load_demo_bundle();
    let ctrl_sys = systems.get("demo_ctrl_sys").expect("demo_ctrl's own SystemDefinition is loaded");
    assert_eq!(ctrl_sys.state_space_id, av_kernel::trajectory::NATIVE_CONTROLLER_EMPTY_ID, "demo_ctrl must declare the empty state space, not the pre-M21.3 six-scalar one");
    let declared_space = ctrl_sys.state_space.clone().expect("demo_ctrl declares its own state_space explicitly");
    assert!(declared_space.components.is_empty(), "demo_ctrl's own declared state space must have zero components; got {:?}", declared_space.components);

    let products = together_products();
    assert!(
        !products.trajectories.contains_key("demo_ctrl"),
        "demo_ctrl (empty declared state space) must have NO entry in RunProducts.trajectories at all -- got {:?}",
        products.trajectories.get("demo_ctrl")
    );
    // The other two, genuinely physical instances are unaffected -- this is not a bug that
    // silently dropped every trajectory.
    assert!(products.trajectories.contains_key("demo_flt"));
    assert!(products.trajectories.contains_key("demo_mvr"));

    // demo_ctrl's own events are still on the timeline, entity-tagged independently of
    // `trajectories` -- its run_start/run_end lifecycle pair, AND the one port_command event
    // `demo_two_instance_produces_a_small_number_of_port_command_events_not_thousands` already
    // pins the exact epoch/value of (checked again here, narrowly, just for entity presence).
    let ctrl_lifecycle: Vec<&av_cdm::pb::Event> = products.events.iter().filter(|e| e.kind == EventKind::Lifecycle as i32 && e.entity_id == "demo_ctrl").collect();
    assert_eq!(ctrl_lifecycle.len(), 2, "demo_ctrl must still produce its own run_start/run_end lifecycle events even though it emits no trajectory; got {ctrl_lifecycle:#?}");
    let port_commands_from_ctrl: Vec<&av_cdm::pb::Event> = products
        .events
        .iter()
        .filter(|e| e.kind == EventKind::PortCommand as i32 && e.provenance.as_ref().and_then(|p| p.attributes.get("sender")).map(String::as_str) == Some("demo_ctrl"))
        .collect();
    assert_eq!(port_commands_from_ctrl.len(), 1, "demo_ctrl's own sent port-command event must still be present; got {port_commands_from_ctrl:#?}");
}

// ==========================================================================================
// 2. Bystander invariance against real single-instance GMAT runs -- the first time this crate
//    measures it with real GMAT dynamics rather than a synthetic "accel.*" fixture (see
//    tests/restart_invariance.rs's own module doc comment, and this file's own module doc
//    comment's "disclosed non-merging case" citation from executor.rs).
// ==========================================================================================

fn hashed_sos(mut sos: SosConfiguration) -> SosConfiguration {
    sos.hash = hash::canonical_sos_hash(&sos);
    sos
}
fn hashed_drm(mut drm: DesignReferenceMission) -> DesignReferenceMission {
    drm.hash = hash::canonical_drm_hash(&drm);
    drm
}
fn hashed_system(mut sys: SystemDefinition) -> SystemDefinition {
    sys.hash = hash::canonical_system_hash(&sys);
    sys
}
/// `parameter_overrides`: M19.4 (question 131) -- `demo_flt`'s own real committed
/// `parameter_overrides` (drag_*/port.consume) must be replayed here too whenever a test builds
/// a standalone comparison instance meant to share `demo_flt`'s own dynamics: unlike a `port.*`
/// override (a no-op with no connection declared to feed it), `force_model.drag_*` is a genuine,
/// unconditional force-model configuration change -- an alone-run `demo_flt` missing it would
/// propagate an entirely different (drag-free) trajectory from t=0, not merely "the same physics
/// without the SIGNAL command," making any physical-sample comparison against the together run
/// meaningless. Callers pass the real instance's own `parameter_overrides` (never hand-retyped
/// literals) so this can never silently drift from what `demo_two_instance.sos.yaml` actually
/// declares.
fn model_instance(name: &str, initial_covariance: Vec<f64>, parameter_overrides: Vec<Parameter>) -> SystemInstance {
    SystemInstance {
        name: name.to_string(),
        system_id: "leo_demo_sys".to_string(),
        binding: Some(Binding { kind: BindingKind::Model as i32, config: Some(av_cdm::pb::binding::Config::Model(ModelBinding { model_id: "leo_demo_sys".to_string() })) }),
        step_rate_hz: 10.0,
        initial_covariance,
        parameter_overrides,
        ..Default::default()
    }
}
fn one_instance_drm(id_prefix: &str, instance: SystemInstance, scenario: Scenario, options: DrmOptions) -> (SosConfiguration, DesignReferenceMission) {
    let sos = hashed_sos(SosConfiguration { id: format!("{id_prefix}_sos"), instances: vec![instance], ..Default::default() });
    let drm = hashed_drm(DesignReferenceMission { id: format!("{id_prefix}_drm"), sos_configuration_id: sos.id.clone(), scenario: Some(scenario), options: Some(options), ..Default::default() });
    (sos, drm)
}

const START_TAI_NS: i64 = 1_767_225_637_000_000_000;
const FAULT_TAI_NS: i64 = START_TAI_NS + 1800 * 1_000_000_000;
const MANEUVER_TAI_NS: i64 = START_TAI_NS + 5400 * 1_000_000_000;
const END_TAI_NS: i64 = START_TAI_NS + 7200 * 1_000_000_000;
/// M19.4 (question 131): the epoch `demo_ctrl`'s own declared range condition
/// (`drms/demo_two_instance_ctrl.system.yaml`) actually fires at, against the real committed
/// fixture -- a deterministic consequence of `demo_mvr`'s own propagated `rmag` crossing the
/// declared `condition.threshold_m` (ADR-004: seeded, deterministic, no wall clock), not a
/// hand-picked value. Pinned by `demo_two_instance_produces_a_small_number_of_port_command_events_
/// not_thousands` below (which reads it off the real `EVENT_KIND_PORT_COMMAND` event rather than
/// assuming this constant is still correct) -- if this ever drifts (a YAML edit, a GMAT version
/// change, ...), that test fails loudly by name rather than this constant silently going stale.
const COMMAND_TAI_NS: i64 = START_TAI_NS + 6207 * 1_000_000_000 + 400_000_000; // 6207.4 s

/// `instance` names whichever `SystemInstance` this fault should target -- the literal
/// `"demo_flt"` for a DRM built around the real committed fixture's own name, or a renamed
/// stand-in (e.g. `"demo_flt_solo"`) for an alone-run comparison fixture built in this file
/// (see [`together_products`]'s own doc comment for why those are ever renamed at all).
fn demo_fault_named(instance: &str) -> Fault {
    Fault {
        id: "fault1".to_string(),
        tai_ns: FAULT_TAI_NS,
        target_kind: FaultTargetKind::Dynamics as i32,
        instance: instance.to_string(),
        target: "force_model.gravity_order".to_string(),
        kind: "parameter".to_string(),
        params: BTreeMap::from([("value".to_string(), 0.0)]),
        ..Default::default()
    }
}
/// See [`demo_fault_named`]'s own doc comment.
fn demo_maneuver_named(instance: &str) -> ScenarioEvent {
    ScenarioEvent {
        id: "burn1".to_string(),
        tai_ns: MANEUVER_TAI_NS,
        kind: "maneuver".to_string(),
        instance: instance.to_string(),
        values: BTreeMap::from([("dv_x".to_string(), 20.0), ("dv_y".to_string(), 0.0), ("dv_z".to_string(), 0.0)]),
        attributes: BTreeMap::from([("frame_id".to_string(), "AXES_KIND_VNB".to_string())]),
        execution_error: None,
    }
}
fn demo_options() -> DrmOptions {
    DrmOptions { covariance: false, default_step_rate_hz: 10.0, sample_interval_s: 60.0, ..Default::default() }
}

/// Loosest possible non-vacuous bound: a real physics regression (a wrong re-bind, a dropped
/// fault/maneuver effect, an off-by-one epoch) would show up at the metre or millimetre level,
/// not at the bound this constant sets. See [`diff_or_panic`]'s own doc comment for where this
/// number comes from -- measured, not fitted to make anything pass.
const RESTART_ULP_TOLERANCE: f64 = 1e-6;

/// Compares `alone` and `together`'s own physical samples (`state_space_id`/`interpolation`,
/// every `TrajectorySample`'s `tai_ns`/`kind`/`cov` exactly, `mean` to
/// [`RESTART_ULP_TOLERANCE`]), and panics on the FIRST divergence exceeding that bound with a
/// precise diagnostic (sample index, epoch, which component, the two values, `|delta|`)
/// instead of dumping both entire sample lists -- what this task's own honesty requirement
/// asks for ("report both trajectories and diagnose... which sample epoch, which component")
/// if restart invariance does not hold for a real GMAT-bound bystander.
///
/// **Why a tolerance at all, when `tests/restart_invariance.rs`'s native-model bystander is
/// asserted bit-for-bit exact.** A GMAT-bound re-materialization (`fault::
/// rebind_gmat_spec_at_state` + `binding::materialize_gmat`) rebuilds a real GMAT `Spacecraft`
/// from the Rust-side Cartesian state through a genuine SI-metre -> km -> GMAT-internal -> back
/// round trip and a fresh `Initialize()`/`GetDerivatives` call, and GMAT's own internal state
/// representation is not perfectly round-trip-neutral at the last bit -- unlike a native
/// `ConstantAccelModel` re-materialization (a trivial copy of the same `f64` state, hence exactly
/// bit-identical, `tests/restart_invariance.rs`'s own bar). Measured for the drag-free case
/// (`demo_mvr`, bystander to `demo_flt`'s own fault at t = 1800 s): worst observed `|delta|` =
/// 2.27e-13 on a ~1252 m/s velocity component, i.e. one ULP of an `f64` (relative error ~1.8e-16,
/// `f64::EPSILON` is ~2.22e-16). `RESTART_ULP_TOLERANCE` (1e-6, absolute, SI) is set roughly 1e6x
/// looser than that -- generous enough to never mask a real regression (which would show up at
/// metres/millimetres, ADR-005's own scale for "something actually changed"), tight enough that
/// nothing but floating-point noise could pass it. **Measured again for the drag-inclusive case**
/// (`demo_flt`, bystander to `demo_mvr`'s own maneuver at t = 5400 s, M19.4/question 131): still
/// bit-identical to the same 1e-6 bound all the way through the re-materialization itself and for
/// nearly 900 s afterward -- the drag/atmosphere-model reinitialization this task adds is, like
/// gravity/point-mass reinitialization, restart-invariant to the same floating-point-noise scale.
/// `RESTART_ULP_TOLERANCE` itself is therefore UNCHANGED by this task (still 1e-6, not loosened).
///
/// **What genuinely diverges, and why comparing the whole window would be wrong, not merely
/// tight.** `demo_flt`'s own SIGNAL-commanded drag-sail `Cd` change (M19.4) fires once, at
/// `COMMAND_TAI_NS`, deterministically (`drms/demo_two_instance_ctrl.system.yaml`'s own declared
/// range condition) -- but ONLY in the together run: the alone run below declares no
/// `SosConfiguration.connections` at all, so it can never receive that command, and legitimately
/// keeps propagating at the un-commanded `Cd = 2.2` for the rest of the window. From
/// `COMMAND_TAI_NS` onward the two runs are NOT bystanders of each other any more -- they are
/// deliberately, physically different scenarios (exactly question 131's own "the command visibly
/// changes the arc" requirement) -- so `stop_before_tai_ns`, when given, restricts the
/// bit-for-bit comparison to samples strictly before it; [`assert_command_epoch_diverges`] below
/// checks the OTHER half explicitly (a real, growing, non-vacuous difference), so this split
/// narrows the comparison's scope honestly rather than silently ignoring the back half.
fn diff_or_panic(label: &str, alone: &av_cdm::pb::Trajectory, together: &av_cdm::pb::Trajectory, context: &str, stop_before_tai_ns: Option<i64>) {
    assert_eq!(alone.state_space_id, together.state_space_id, "{label}: state_space_id differs {context}");
    assert_eq!(alone.interpolation, together.interpolation, "{label}: interpolation differs {context}");
    assert_eq!(alone.samples.len(), together.samples.len(), "{label}: sample count differs {context}");
    let mut max_delta = 0.0_f64;
    let mut divergent_from: Option<usize> = None;
    let mut compared = 0usize;
    for (i, (a, t)) in alone.samples.iter().zip(together.samples.iter()).enumerate() {
        assert_eq!(a.tai_ns, t.tai_ns, "{label}: sample {i}'s own tai_ns differs {context}");
        if let Some(stop) = stop_before_tai_ns {
            if a.tai_ns >= stop {
                break;
            }
        }
        compared += 1;
        assert_eq!(a.kind, t.kind, "{label}: sample {i}'s own SampleKind differs {context}");
        assert_eq!(a.cov, t.cov, "{label}: sample {i}'s own cov differs {context}");
        assert_eq!(a.mean.len(), t.mean.len(), "{label}: sample {i}'s own mean length differs {context}");
        let mut worst = 0.0_f64;
        let mut worst_component = usize::MAX;
        for (c, (av, tv)) in a.mean.iter().zip(t.mean.iter()).enumerate() {
            let d = (av - tv).abs();
            if d > worst {
                worst = d;
                worst_component = c;
            }
        }
        if worst > max_delta {
            max_delta = worst;
        }
        if worst > 0.0 && divergent_from.is_none() {
            divergent_from = Some(i);
        }
        if worst > RESTART_ULP_TOLERANCE {
            panic!(
                "{label}: FIRST divergence {context} EXCEEDING {RESTART_ULP_TOLERANCE:e} at sample index {i} (tai_ns {}): kind alone={:?} together={:?}; \
                 mean alone={:?} together={:?}; worst component index {worst_component} |delta| = {worst:.6e}",
                a.tai_ns, a.kind, t.kind, a.mean, t.mean
            );
        }
    }
    assert!(compared > 0, "{label}: stop_before_tai_ns left nothing to compare {context}");
    eprintln!(
        "[demo_two_instance] {label} {context}: {compared} samples compared, max |delta| = {max_delta:.6e} (tolerance {RESTART_ULP_TOLERANCE:e}), first non-zero divergence at sample index {divergent_from:?}"
    );
}

/// The other half of [`diff_or_panic`]'s own split (M19.4, question 131): from `command_tai_ns`
/// onward, `alone` (never commanded) and `together` (commanded at `command_tai_ns`) must
/// genuinely, increasingly diverge -- proves the split above is not silently hiding a "nothing
/// actually differs" bug (e.g. a command that decodes but never actually reaches `set_real_
/// parameter`, or reaches it but the force model has no drag to make it matter) by simply never
/// looking at that half. Checks strictly INCREASING divergence at the two latest samples,
/// stronger than a bare "final divergence is nonzero" bound.
fn assert_command_epoch_diverges(label: &str, alone: &av_cdm::pb::Trajectory, together: &av_cdm::pb::Trajectory, command_tai_ns: i64, min_final_divergence_m: f64) {
    let dr = |a: &av_cdm::pb::TrajectorySample, t: &av_cdm::pb::TrajectorySample| (0..3).map(|c| (a.mean[c] - t.mean[c]).powi(2)).sum::<f64>().sqrt();
    let after: Vec<(f64, f64)> = alone
        .samples
        .iter()
        .zip(together.samples.iter())
        .filter(|(a, _)| a.tai_ns >= command_tai_ns)
        .map(|(a, t)| ((a.tai_ns - command_tai_ns) as f64 / 1e9, dr(a, t)))
        .collect();
    assert!(after.len() >= 2, "{label}: need at least 2 post-command samples to check growth, got {}", after.len());
    let (t_mid, d_mid) = after[after.len() / 2];
    let (t_last, d_last) = *after.last().unwrap();
    eprintln!("[demo_two_instance] {label} post-command divergence: t+{t_mid:.1}s -> {d_mid:.4} m, t+{t_last:.1}s -> {d_last:.4} m (must strictly grow and exceed {min_final_divergence_m} m)");
    assert!(d_last > d_mid, "{label}: post-command divergence must keep growing, not plateau or shrink: {d_mid} m at t+{t_mid:.1}s vs {d_last} m at t+{t_last:.1}s");
    assert!(d_last > min_final_divergence_m, "{label}: post-command divergence {d_last} m at end of window must exceed {min_final_divergence_m} m (the drag-sail command must have a real, non-vacuous effect)");
}

/// Required test: bystander invariance against *real single-instance GMAT runs*, both
/// directions (`demo_flt` is the bystander to `demo_mvr`'s own maneuver boundary; `demo_mvr` is
/// the bystander to `demo_flt`'s own fault boundary). Fails against: an off-by-one in
/// `run_shared_group`'s "re-materialize every other active instance too" boundary path that
/// silently perturbs the untouched instance's own physical samples, or a regression to the
/// *target* instance's own physics from another instance merely sharing the kernel run.
///
/// **A GMAT object-namespace hazard, discovered here and fixed at the source by M18.4
/// (`docs/open-questions.md` question 127) -- see [`together_products`]'s own doc comment and
/// `binding::materialize_gmat`'s own doc comment for the full account.** Through M18.3, the
/// alone-run comparison instances below had to be renamed (`demo_flt_solo`/`demo_mvr_solo`),
/// never the literal `demo_flt`/`demo_mvr` the together-run's own committed `SosConfiguration`
/// uses, because those two literal names are constructed through GMAT exactly once in this whole
/// binary, via the shared [`together_products`] -- a second, independent `execute()` call
/// reusing either name (this function's own two alone runs, below) would have collided.
/// **M18.4 makes that safe** (`executor::gmat_execution_namespace` namespaces every GMAT object
/// by `execute()` invocation, not by instance name), so the alone runs below now use the
/// identical literal `"demo_flt"`/`"demo_mvr"` names too -- deliberately, not merely because it
/// is now allowed: it is what makes the `assert_eq!` on full `TrajectorySegment`s further down
/// (`name`/`dynamics_model` included, not merely `.len()`) a meaningful, exact comparison rather
/// than one that would fail on the instance label alone. The together run itself still comes from
/// the shared [`together_products`] (constructed at most once in this whole binary) rather than a
/// fresh `execute()` call in this function, purely to avoid re-running the real ~2 h LEO arc
/// redundantly. `demo_flt_nofault` (the closed-form counterfactual further down) keeps its own
/// distinct name -- nothing compares its segments against the together run's, so there is nothing
/// for a shared name to buy there.
///
/// **What this test also measures: `merge_adjacent_segments` (M15.1, question 115; M18.4 closes
/// it for GMAT) collapses a GMAT-bound bystander's own segments back to its single-instance run's
/// shape.** Through M18.3, `executor.rs`'s own module doc comment disclosed that it did NOT, for a
/// GMAT-bound bystander specifically (its `dynamics_hash` baked in its own instantaneous Cartesian
/// state via `fault::rebind_gmat_spec_at_state`, which differed at every re-materialization
/// regardless of whether anything was actually reconfigured) -- measured here against a real GMAT
/// run for the first time, and found true. M18.4 fixes the root cause (`binding::gmat_settings`
/// excludes exactly those six state fields from what it hashes), so this test now asserts the
/// segments as an exact match rather than merely reporting whichever shape it observed.
#[test]
fn demo_two_instance_bystander_invariance_against_real_single_instance_gmat_runs() {
    let _engine = gmat_sys::engine_lock();
    let (_drm, sos, systems) = load_demo_bundle();
    // M19.4 (question 131): the real committed instances' own `parameter_overrides` -- see
    // `model_instance`'s own doc comment for why `demo_flt`'s drag_* overrides specifically must
    // be replayed here.
    let real_flt = sos.instances.iter().find(|i| i.name == "demo_flt").expect("demo_flt instance");
    let real_mvr = sos.instances.iter().find(|i| i.name == "demo_mvr").expect("demo_mvr instance");

    // M18.4 (question 127): the alone-run instances are named the IDENTICAL `"demo_flt"`/
    // `"demo_mvr"` the together-run's own committed fixture uses -- through M18.3 these had to be
    // renamed (`"demo_flt_solo"`/`"demo_mvr_solo"`) to avoid the GMAT object-namespace collision
    // `together_products`'s own doc comment describes; `executor::gmat_execution_namespace` makes
    // that collision impossible now, and reusing the literal names here is deliberate: it is what
    // makes the physical-sample comparison below (`diff_or_panic`) a meaningful one instead of one
    // that would fail on an instance label alone. Note the segment `assert_eq!` further down
    // deliberately compares `(start_tai_ns, end_tai_ns, dynamics_hash, dynamics_depth)` and NOT the
    // full `TrajectorySegment`: `name`/`dynamics_model` carry a materialization index that legitimately
    // differs alone vs. together -- see that assertion's own comment for why.
    let scenario_flt_alone = Scenario { start_tai_ns: START_TAI_NS, end_tai_ns: END_TAI_NS, faults: vec![demo_fault_named("demo_flt")], ..Default::default() };
    let scenario_mvr_alone = Scenario { start_tai_ns: START_TAI_NS, end_tai_ns: END_TAI_NS, events: vec![demo_maneuver_named("demo_mvr")], ..Default::default() };
    let (sos_flt, drm_flt) = one_instance_drm("demo_flt_alone", model_instance("demo_flt", vec![], real_flt.parameter_overrides.clone()), scenario_flt_alone, demo_options());
    let (sos_mvr, drm_mvr) = one_instance_drm("demo_mvr_alone", model_instance("demo_mvr", vec![], real_mvr.parameter_overrides.clone()), scenario_mvr_alone, demo_options());

    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let products_together = together_products();
    let cfg_flt = RunConfig { gmat: &gmat, drm: &drm_flt, sos: &sos_flt, systems: &systems, run_id: "test-demo-flt-alone".to_string(), error_mode: Default::default() };
    let products_flt_alone = execute(cfg_flt).expect("demo_flt, run alone with the same fault as the together run's own demo_flt, executes -- reusing the identical gmat.*-bound instance name in a second execute() call is exactly M18.4's own fix");
    let cfg_mvr = RunConfig { gmat: &gmat, drm: &drm_mvr, sos: &sos_mvr, systems: &systems, run_id: "test-demo-mvr-alone".to_string(), error_mode: Default::default() };
    let products_mvr_alone = execute(cfg_mvr).expect("demo_mvr, run alone with the same maneuver as demo_mvr, executes");

    let traj_flt_alone = products_flt_alone.trajectories.get("demo_flt").unwrap();
    let traj_flt_together = products_together.trajectories.get("demo_flt").unwrap();
    let traj_mvr_alone = products_mvr_alone.trajectories.get("demo_mvr").unwrap();
    let traj_mvr_together = products_together.trajectories.get("demo_mvr").unwrap();

    // Sanity: the fault/maneuver actually happened (real boundaries, not a vacuous
    // "nothing to compare" case).
    assert_eq!(traj_flt_alone.segments.len(), 2, "one DYNAMICS fault -> two dynamics segments");
    assert_eq!(traj_mvr_alone.segments.len(), 2, "one maneuver -> two segments (a maneuver boundary is always kept)");

    // The claim this test exists to measure: byte-identical PHYSICAL samples, alone vs.
    // together, for both instances (each is a bystander to the other's own boundary at least
    // once: demo_flt is the bystander at demo_mvr's maneuver epoch; demo_mvr is the bystander
    // at demo_flt's fault epoch). Diagnosed sample-by-sample, rather than dumped whole, per this
    // task's own instruction: "report both trajectories and diagnose... which sample epoch,
    // which component" if this does NOT hold for a real two-instance run.
    //
    // demo_mvr's own comparison is unrestricted (`None`): nothing about demo_mvr's own dynamics
    // is ever SIGNAL-commanded, so it stays a real bystander for the WHOLE window. demo_flt's own
    // comparison stops strictly before `COMMAND_TAI_NS` (M19.4, question 131) -- see
    // `diff_or_panic`'s own doc comment for why comparing past that point would be comparing two
    // legitimately different scenarios, not measuring bystander invariance.
    diff_or_panic("demo_flt", traj_flt_alone, traj_flt_together, "alongside demo_mvr's own maneuver boundary, before the drag-sail command fires", Some(COMMAND_TAI_NS));
    diff_or_panic("demo_mvr", traj_mvr_alone, traj_mvr_together, "alongside demo_flt's own fault boundary", None);
    // The other half: from COMMAND_TAI_NS on, demo_flt's alone/together arcs must genuinely,
    // increasingly diverge (never commanded vs. commanded) -- proves the restriction above is not
    // silently hiding a "nothing actually differs" bug. 10 m is two orders of magnitude below this
    // task's own expected order-of-magnitude estimate (tens of meters, this file's own module doc
    // comment) and comfortably above anything floating-point noise could produce.
    assert_command_epoch_diverges("demo_flt", traj_flt_alone, traj_flt_together, COMMAND_TAI_NS, 10.0);

    // The segment-merge question (M18.4, `docs/open-questions.md` question 127's second half,
    // closing question 115 for a real GMAT-bound instance): STRUCTURAL restart invariance, not
    // merely physical. `binding::gmat_settings` no longer hashes a GMAT-bound instance's own
    // initial-or-instantaneous state representation (`fault::CARTESIAN_FIELDS`/`KEPLERIAN_FIELDS`/
    // `DISPLAY_STATE_TYPE_FIELD`), so a GMAT-bound bystander's `dynamics_hash` is genuinely
    // unchanged across a boundary that never touched its own dynamics, and `executor::
    // merge_adjacent_segments` now collapses it back to its single-instance run's own segment
    // shape -- exactly the bar `tests/restart_invariance.rs` already held native-model bystanders
    // to.
    //
    // Compared on `(start_tai_ns, end_tai_ns, dynamics_hash, dynamics_depth)` -- everything about a
    // segment that describes *what physically happened* -- not the full `TrajectorySegment`
    // struct: `name`/`dynamics_model` are `"{instance}_{materialization index}"`-shaped
    // (`binding::materialize_gmat`'s own doc comment: `name_suffix` must stay exactly what it was
    // before this task, or every existing golden's recorded `dynamics_model` string would drift),
    // so a surviving post-merge segment's own index still counts every raw materialization that
    // ever happened before it, merged away or not -- `demo_mvr`'s own post-maneuver segment is
    // materialization index 1 alone (one prior materialization: its own start) but index 2 together
    // (two prior materializations: its own start, then the bystander re-bind at `demo_flt`'s fault,
    // which merges away) even once the *hash* correctly agrees they describe identical dynamics.
    // That index drift is an accepted, inherent consequence of a stable `name_suffix` scheme, not a
    // structural difference this test is about -- exactly why `diff_or_panic` above already
    // excludes the analogous `entity_id` label from the physical-sample comparison. Equality on the
    // fields below, not merely "did not shrink": fails against the pre-M18.4 hash (which baked in
    // state) exactly as it would have reported "CONFIRMED" here before this task -- three segments
    // for the bystander instead of the one its own alone run produced, and a different
    // `dynamics_hash` on top even after this task if only `CARTESIAN_FIELDS` (not also
    // `KEPLERIAN_FIELDS`/`DISPLAY_STATE_TYPE_FIELD`) were excluded (measured directly while
    // building this fix: `demo_mvr` still failed to merge with only the Cartesian fields excluded).
    fn structural_shape(segments: &[av_cdm::pb::TrajectorySegment]) -> Vec<(i64, i64, &str, &str)> {
        segments.iter().map(|s| (s.start_tai_ns, s.end_tai_ns, s.dynamics_hash.as_str(), s.dynamics_depth.as_str())).collect()
    }
    eprintln!(
        "[demo_two_instance] segment counts: demo_flt alone={} together={}; demo_mvr alone={} together={}",
        traj_flt_alone.segments.len(),
        traj_flt_together.segments.len(),
        traj_mvr_alone.segments.len(),
        traj_mvr_together.segments.len()
    );
    assert_eq!(
        structural_shape(&traj_flt_together.segments),
        structural_shape(&traj_flt_alone.segments),
        "demo_flt's own segments (bystander to demo_mvr's maneuver boundary) must now match its single-instance run's own boundaries/hashes/depths exactly"
    );
    assert_eq!(
        structural_shape(&traj_mvr_together.segments),
        structural_shape(&traj_mvr_alone.segments),
        "demo_mvr's own segments (bystander to demo_flt's fault boundary) must now match its single-instance run's own boundaries/hashes/depths exactly"
    );

    // Closed-form: demo_flt's own fault really did change the propagated physics -- compare
    // against a counterfactual with NO fault declared at all over the identical window. A
    // DYNAMICS fault application that silently no-ops (e.g. only updates a settings hash but
    // never actually changes what gets propagated) would make these two runs bit-identical
    // (or agree to ~1e-9 m, RESTART_ULP_TOLERANCE's own floating-point-noise scale); measured
    // divergence is 721.3 m (dropping gravity Order from 8 to 0 for two hours of a LEO orbit),
    // so a 100 m bound safely separates "the fault did something real" from both floating-point
    // noise and the golden's own five-order-of-magnitude-tighter 0.05 m tolerance for a
    // *correct* comparison.
    let scenario_flt_no_fault = Scenario { start_tai_ns: START_TAI_NS, end_tai_ns: END_TAI_NS, ..Default::default() };
    // M19.4: the same drag overrides as `real_flt` (this counterfactual isolates the FAULT's own
    // effect; if it dropped drag too, the comparison would conflate the fault effect with the
    // drag-vs-no-drag effect instead).
    let (sos_flt_nf, drm_flt_nf) = one_instance_drm("demo_flt_no_fault", model_instance("demo_flt_nofault", vec![], real_flt.parameter_overrides.clone()), scenario_flt_no_fault, demo_options());
    let cfg_nf = RunConfig { gmat: &gmat, drm: &drm_flt_nf, sos: &sos_flt_nf, systems: &systems, run_id: "test-demo-flt-no-fault".to_string(), error_mode: Default::default() };
    let products_nf = execute(cfg_nf).expect("demo_flt_nofault, run alone with NO fault, executes");
    let traj_nf = products_nf.trajectories.get("demo_flt_nofault").unwrap();
    let last_faulted = traj_flt_alone.samples.last().unwrap();
    let last_unfaulted = traj_nf.samples.last().unwrap();
    let dr_fault_effect = (0..3).map(|i| (last_faulted.mean[i] - last_unfaulted.mean[i]).powi(2)).sum::<f64>().sqrt();
    eprintln!("[demo_two_instance] fault effect (faulted vs. unfaulted final position): |dr| = {dr_fault_effect:.1} m");
    assert!(dr_fault_effect > 100.0, "the DYNAMICS fault (force_model.gravity_order 8->0) must produce a real, measurable trajectory difference, not a no-op; got only {dr_fault_effect} m");
}

// ==========================================================================================
// 3. Covariance is declared on demo_mvr only -- and the asymmetry is load-bearing, not
//    decorative.
// ==========================================================================================

/// Required test: "Covariance is present on the requested instance and absent on the other."
/// Fails against: a `demo_two_instance.sos.yaml` that accidentally declares
/// `initial_covariance` on both instances (or neither), AND against an executor that silently
/// tolerates a missing `initial_covariance` once `DrmOptions.covariance` is requested globally
/// (rather than refusing by name, `DrmError::MissingInitialCovariance`) -- proving the
/// declared absence on `demo_flt` is a real, checked precondition, not merely an unread field.
#[test]
fn covariance_is_declared_on_demo_mvr_only_and_the_asymmetry_is_load_bearing() {
    let _engine = gmat_sys::engine_lock();
    let (_drm, sos, systems) = load_demo_bundle();

    let demo_flt = sos.instances.iter().find(|i| i.name == "demo_flt").expect("demo_flt instance");
    let demo_mvr = sos.instances.iter().find(|i| i.name == "demo_mvr").expect("demo_mvr instance");
    assert!(demo_flt.initial_covariance.is_empty(), "demo_flt must NOT declare initial_covariance (mutually exclusive with its own DYNAMICS fault)");
    assert_eq!(demo_mvr.initial_covariance.len(), 36, "demo_mvr must declare the golden's own 6x6 P0");
    // Exactly the golden P0 (drms/leo_1day_golden.sos.yaml's own "leo" instance): diag(10000,
    // 10000, 10000, 0.01, 0.01, 0.01).
    let expected_diag = [10000.0, 10000.0, 10000.0, 0.01, 0.01, 0.01];
    for (i, expected_i) in expected_diag.iter().enumerate() {
        for j in 0..6 {
            let v = demo_mvr.initial_covariance[i * 6 + j];
            let expected = if i == j { *expected_i } else { 0.0 };
            assert_eq!(v, expected, "P0[{i}][{j}]");
        }
    }

    // Attempting options.covariance = true against a DRM shaped exactly like the committed one
    // (no faults declared here, so the fault-exclusion rule --
    // DrmError::CovarianceWithFaultsNotSupported -- is not what this test is isolating) must be
    // refused BY NAME, naming the fault-less-here instance specifically, because ITS declared
    // covariance is genuinely absent. The two instances are renamed
    // (`demo_flt_covprobe`/`demo_mvr_covprobe`) rather than reusing the literal `demo_flt`/
    // `demo_mvr` from the loaded `sos` -- historically (through M18.3) because those two literal
    // names are constructed through GMAT exactly once in this whole binary, via
    // `together_products`, and this probe's own `execute()` call -- in a different `#[test]`
    // function -- would have been a second, colliding construction if it reused them; M18.4 (see
    // [`together_products`]'s own doc comment) makes reusing them safe, but the rename is kept
    // (harmless either way) so this probe's own `DrmError::MissingInitialCovariance{instance}`
    // assertion below reads unambiguously against its own renamed instance. The renamed clone
    // keeps every other field (system_id, binding, step_rate_hz, initial_covariance) identical to
    // the real committed instances, so the asymmetry under test is the real one.
    let mut probe_instances = sos.instances.clone();
    probe_instances[0].name = "demo_flt_covprobe".to_string();
    probe_instances[1].name = "demo_mvr_covprobe".to_string();
    let sos_probe = hashed_sos(SosConfiguration { id: "demo_two_instance_cov_probe_sos".to_string(), instances: probe_instances, ..Default::default() });
    let scenario = Scenario { start_tai_ns: START_TAI_NS, end_tai_ns: START_TAI_NS + 100 * 1_000_000_000, ..Default::default() };
    let options = DrmOptions { covariance: true, default_step_rate_hz: 10.0, sample_interval_s: 10.0, ..Default::default() };
    let drm_cov = hashed_drm(DesignReferenceMission { id: "demo_two_instance_cov_probe_drm".to_string(), sos_configuration_id: sos_probe.id.clone(), scenario: Some(scenario), options: Some(options), ..Default::default() });

    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let cfg = RunConfig { gmat: &gmat, drm: &drm_cov, sos: &sos_probe, systems: &systems, run_id: "test-demo-cov-probe".to_string(), error_mode: Default::default() };
    let err = execute(cfg).expect_err("global covariance against this sos must be refused: demo_flt_covprobe has no declared initial_covariance");
    match err {
        DrmError::MissingInitialCovariance { instance } => {
            assert_eq!(instance, "demo_flt_covprobe", "the refusal must name the instance that actually lacks covariance, not demo_mvr_covprobe (which HAS it)")
        }
        other => panic!("expected DrmError::MissingInitialCovariance{{instance: \"demo_flt_covprobe\"}}, got {other:?}"),
    }

    // And, positively: demo_mvr's OWN declared covariance is not merely present but
    // functional -- a single-instance covariance-enabled DRM around a demo_mvr-shaped instance
    // (its own maneuver included, since covariance + a maneuver on the SAME instance is
    // supported -- see tests/gates_execution_error.rs) actually propagates real covariance end
    // to end. Renamed to `demo_mvr_covcheck` for the same GMAT-object-namespace reason above.
    let scenario_mvr_cov = Scenario { start_tai_ns: START_TAI_NS, end_tai_ns: MANEUVER_TAI_NS, events: vec![demo_maneuver_named("demo_mvr_covcheck")], ..Default::default() };
    let options_mvr_cov = DrmOptions { covariance: true, default_step_rate_hz: 10.0, sample_interval_s: 60.0, ..Default::default() };
    let (sos_mvr_cov, drm_mvr_cov) = one_instance_drm("demo_mvr_cov", model_instance("demo_mvr_covcheck", demo_mvr.initial_covariance.clone(), vec![]), scenario_mvr_cov, options_mvr_cov);
    let cfg_mvr_cov = RunConfig { gmat: &gmat, drm: &drm_mvr_cov, sos: &sos_mvr_cov, systems: &systems, run_id: "test-demo-mvr-cov".to_string(), error_mode: Default::default() };
    let products_mvr_cov = execute(cfg_mvr_cov).expect("demo_mvr's own declared covariance propagates end to end (with its own maneuver applied)");
    let traj_mvr_cov = products_mvr_cov.trajectories.get("demo_mvr_covcheck").unwrap();
    let last = traj_mvr_cov.samples.last().expect("at least one sample");
    assert!(!last.cov.is_empty(), "demo_mvr's declared initial_covariance must actually propagate to a real, non-empty cov on the final sample");
}

// ==========================================================================================
// 4. ICRF on the wire (M18.1, `docs/open-questions.md` questions 10/124).
// ==========================================================================================

/// Required test: "the ingested demo run offers ICRF in the frame list a viewer would build."
/// Fails against: the pre-M18.1 loader (`Scenario.frames` refused outright, so this DRM would
/// not even parse) and against an executor that returns only `collect_frames`'s own
/// declared-plus-referenced set without the question 10 mandatory-frame augmentation (
/// `EarthBodyFixed` would then be silently absent, since neither `demo_flt` nor `demo_mvr` ever
/// propagates in it).
#[test]
fn demo_two_instance_run_products_frames_include_icrf_and_the_mandatory_central_body_frames() {
    let _engine = gmat_sys::engine_lock();
    let products = together_products();
    let by_id: BTreeMap<&str, &av_cdm::pb::FrameDefinition> = products.frames.iter().map(|f| (f.id.as_str(), f)).collect();

    // Declared explicitly in demo_two_instance.drm.yaml's own Scenario.frames (question 124) --
    // its own author-supplied description must survive untouched (proves the declared entry,
    // not a re-derived one, reached the wire).
    let icrf = by_id.get("EarthICRF").expect("EarthICRF must be present: declared explicitly in demo_two_instance.drm.yaml");
    assert_eq!(icrf.origin, Some(frame_definition::Origin::Body("Earth".to_string())));
    assert_eq!(icrf.axes, AxesKind::Icrf as i32);
    assert_eq!(
        icrf.description,
        "International Celestial Reference Frame (ICRF): an inertial frame whose axes are fixed to distant quasar positions, origin at Earth's centre of mass.",
        "the description must be a human description of the frame itself (question 136), not the pre-M20.1 process note"
    );

    // Both instances' own actual propagation frame (spacecraft.CoordinateSystem =
    // EarthMJ2000Eq, demo_two_instance.system.yaml) must still be present, unchanged -- "do not
    // drop or rename an instance's own propagation frame while adding the mandatory ones."
    let mj2000eq = by_id.get("EarthMJ2000Eq").expect("EarthMJ2000Eq: both instances' own propagation frame");
    assert_eq!(mj2000eq.axes, AxesKind::Mj2000Eq as i32);

    // Question 10's mandatory body-fixed frame for the central body -- present even though
    // NEITHER instance ever propagates in it (mandatory, not merely referenced).
    let body_fixed = by_id.get("EarthBodyFixed").expect("EarthBodyFixed: question 10's mandatory frame for the central body, Earth");
    assert_eq!(body_fixed.axes, AxesKind::BodyFixed as i32);
    assert_eq!(body_fixed.origin, Some(frame_definition::Origin::Body("Earth".to_string())));
}

/// Required test: "a declared `Scenario.frames` entry wins over the derived registry default,
/// now exercised end to end through the loader rather than only as a unit test" (the unit test
/// this exercises the loader path around is `executor::frame_registry_tests::
/// collect_frames_prefers_a_declared_scenario_frame_over_the_registry_default`). This DRM/
/// SosConfiguration are real YAML text run through `schema::parse_drm_yaml`/`parse_sos_yaml`
/// (never a `pb::Scenario`/`SosConfiguration` value built directly in Rust), so a regression in
/// the YAML->pb typed conversion itself (not just the in-memory `collect_frames` precedence)
/// would also be caught here. Fails against: the pre-M18.1 loader (refuses any non-empty
/// `Scenario.frames`) and against an executor whose declared-vs-derived precedence is reversed
/// or coincidental (e.g. "last write wins" happening to keep the derived one because of
/// insertion order).
#[test]
fn declared_scenario_frame_wins_over_the_registry_default_through_the_real_loader() {
    let _engine = gmat_sys::engine_lock();
    let (_drm, _sos, systems) = load_demo_bundle();

    let drm_yaml = r#"
id: icrf_declared_wins_drm
sos_configuration_id: icrf_declared_wins_sos
scenario:
  start_tai_ns: 1767225637000000000
  end_tai_ns: 1767225757000000000
  frames:
    - id: EarthMJ2000Eq
      body: Earth
      axes: AXES_KIND_MJ2000_EQ
      description: "author-declared, not derived (end-to-end loader probe, M18.1)"
options:
  covariance: false
  default_step_rate_hz: 10.0
  sample_interval_s: 60.0
hash: ""
"#;
    let sos_yaml = r#"
id: icrf_declared_wins_sos
instances:
  - name: icrf_declared_wins_probe
    system_id: leo_demo_sys
    binding:
      kind: BINDING_KIND_MODEL
      model:
        model_id: leo_demo_sys
    step_rate_hz: 10.0
hash: ""
"#;
    let mut drm = schema::parse_drm_yaml(drm_yaml).expect("DRM parses through the real loader, Scenario.frames included");
    let mut sos = schema::parse_sos_yaml(sos_yaml).expect("SosConfiguration parses through the real loader");
    assert_eq!(drm.scenario.as_ref().unwrap().frames.len(), 1, "the loader must not drop the declared frame");
    sos.hash = hash::canonical_sos_hash(&sos);
    drm.hash = hash::canonical_drm_hash(&drm);

    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let cfg = RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: "test-declared-frame-wins".to_string(), error_mode: Default::default() };
    let products = execute(cfg).expect("executes end to end with a declared Scenario.frames entry");

    let mj2000eq = products.frames.iter().find(|f| f.id == "EarthMJ2000Eq").expect("EarthMJ2000Eq present (both declared AND this instance's own propagation frame)");
    assert_eq!(
        mj2000eq.description, "author-declared, not derived (end-to-end loader probe, M18.1)",
        "the DECLARED Scenario.frames entry must win over collect_frames's own derived registry default for the identical id"
    );
}

/// **M18.1 escalated this; M19.1 (question 128, ADR-002's fourth amendment) closes it.** The
/// gap M18.1 found and measured (kept below, unchanged, for the historical record): through
/// M18.3, `spacecraft.CoordinateSystem` only labelled `Trajectory.frame_id` -- it never changed
/// the propagated Cartesian numbers `gmat_sys::DerivativeModel`/`GmatModel` actually read, which
/// always came from `PropagationStateManager`'s own raw internal state buffer (the integration
/// frame, `{central_body}MJ2000Eq`), regardless of what `Spacecraft.CoordinateSystem` named.
/// Measured three independent ways while diagnosing this (a throwaway diagnostic, not
/// committed):
///   1. A GMAT *script* reporting the identical spacecraft's state as both
///      `Golden.EarthICRF.X` and `Golden.EarthMJ2000Eq.X` (two Parameter objects, one script
///      run) shows a real ~1.4 m difference at LEO -- the actual ICRF/MJ2000Eq frame bias is a
///      genuine, non-negligible effect at this altitude, not experimental noise.
///   2. The bare object API (`Construct`/`SetField`/`GetRealParameter("X")`, no script engine)
///      shows `spacecraft.CoordinateSystem` has **zero** effect on the read-back Cartesian
///      state: `EarthMJ2000Eq`, `EarthMJ2000Ec` (a ~23.4 degree obliquity -- unmistakable if any
///      conversion at all were applied) and `EarthICRF` all read back byte-identical.
///   3. `av-kernel`'s own `"EarthICRF"`-labeled run disagreed with `goldens/icrf_leo_2h.json`'s
///      real ICRF numbers by 1.330 m -- matching measurement 1's frame-bias magnitude almost
///      exactly, confirming av-kernel's own propagated numbers were silently still
///      `EarthMJ2000Eq`-equivalent despite the `"EarthICRF"` label.
///
/// M19.1 closes it with exactly the fix M18.1 identified as the real one: GMAT's
/// `CoordinateConverter::Convert` (`third_party/gmat-src/src/base/coordsystem/
/// CoordinateConverter.hpp`), reached through a new `gmat-sys` shim call
/// (`gmatffi_convert_state`/`gmat_sys::Gmat::convert`) and applied to every sample of a
/// `"gmat."`-bound trajectory whose declared frame differs from its own integration frame
/// (`crate::drm::executor::convert_gmat_trajectory_to_declared_frame`, called from `execute`
/// itself -- see that function's own doc comment). `icrf_leo_2h_numeric_conversion_matches_the_
/// genuine_gmat_reportfile`, below, is the numeric comparison this doc comment used to say could
/// not yet be built: `icrf_products()`'s own final trajectory sample, genuinely converted (no
/// longer merely labelled) into `EarthICRF`, now agrees with `goldens/icrf_leo_2h.json`'s
/// real, GMAT-`ReportFile`-computed reference to this repository's usual golden tolerance.
///
/// A clone of `leo_demo_sys` (the real committed `demo_two_instance.system.yaml`) with
/// `spacecraft.CoordinateSystem` overridden to `"EarthICRF"` -- otherwise field-for-field
/// identical (same vehicle, same JGM2 8x8 + Sun/Moon force model), matching
/// `goldens/gen_icrf_leo_2h.py`'s own script exactly. `id` is renamed so it can coexist in the
/// same `systems` map as the real `leo_demo_sys` (`load_demo_bundle`'s own map, reused here
/// rather than rebuilt) without colliding.
fn icrf_system_definition(base: &SystemDefinition) -> SystemDefinition {
    let mut sys = base.clone();
    sys.id = "leo_demo_sys_icrf".to_string();
    for p in sys.parameters.iter_mut() {
        if p.name == "spacecraft.CoordinateSystem" {
            assert_eq!(p.string_value, "EarthMJ2000Eq", "sanity: overriding a value other than the expected base");
            p.string_value = "EarthICRF".to_string();
        }
    }
    hashed_system(sys)
}

/// See [`together_products`]'s own doc comment for the GMAT object-namespace hazard this
/// mirrors: `"icrf_probe"` is a `"gmat.*"`-bound `SystemInstance` name constructed through GMAT
/// at most once in this whole test binary, via this `OnceLock`, shared by both tests below.
fn icrf_products() -> &'static RunProducts {
    static ONCE: OnceLock<RunProducts> = OnceLock::new();
    ONCE.get_or_init(|| {
        let (_drm, _sos, systems) = load_demo_bundle();
        let icrf_sys = icrf_system_definition(systems.get("leo_demo_sys").expect("leo_demo_sys loaded"));
        let mut all_systems = systems;
        all_systems.insert(icrf_sys.id.clone(), icrf_sys);

        let instance = SystemInstance {
            name: "icrf_probe".to_string(),
            system_id: "leo_demo_sys_icrf".to_string(),
            binding: Some(Binding { kind: BindingKind::Model as i32, config: Some(av_cdm::pb::binding::Config::Model(ModelBinding { model_id: "leo_demo_sys_icrf".to_string() })) }),
            step_rate_hz: 10.0,
            ..Default::default()
        };
        let scenario = Scenario { start_tai_ns: START_TAI_NS, end_tai_ns: END_TAI_NS, ..Default::default() };
        let (sos, drm) = one_instance_drm("icrf_probe", instance, scenario, demo_options());

        let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
        let cfg = RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &all_systems, run_id: "test-icrf-probe".to_string(), error_mode: Default::default() };
        execute(cfg).expect("the ICRF-bound probe instance executes end to end")
    })
}

/// Required test: "a test that the mandatory frames appear even when the instance propagated in
/// something else." `icrf_probe` propagates in `EarthICRF`, not the ordinary `EarthMJ2000Eq`
/// every other fixture in this crate uses -- `EarthMJ2000Eq` and `EarthBodyFixed` must still
/// both be present, purely from question 10's mandatory-frame rule, since nothing in this run's
/// own trajectory ever references either of them. Fails against an implementation that derives
/// the mandatory frames from `Trajectory.frame_id` (would only ever add frames for the frame(s)
/// actually used, missing exactly the two this test checks) rather than from the run's own
/// `GmatSystemSpec.central_body`.
#[test]
fn mandatory_frames_appear_even_when_the_instance_propagated_in_icrf_not_mj2000eq() {
    let _engine = gmat_sys::engine_lock();
    let products = icrf_products();
    let traj = products.trajectories.get("icrf_probe").unwrap();
    assert_eq!(traj.frame_id, "EarthICRF", "sanity: this instance's own propagation frame really is ICRF, not MJ2000Eq");

    let ids: Vec<&str> = products.frames.iter().map(|f| f.id.as_str()).collect();
    assert!(ids.contains(&"EarthICRF"), "the instance's own propagation frame must still be present: {ids:?}");
    assert!(ids.contains(&"EarthMJ2000Eq"), "question 10's mandatory MJ2000Eq frame must be present even though nothing propagated in it: {ids:?}");
    assert!(ids.contains(&"EarthBodyFixed"), "question 10's mandatory body-fixed frame must be present even though nothing propagated in it: {ids:?}");

    let mj2000eq = products.frames.iter().find(|f| f.id == "EarthMJ2000Eq").unwrap();
    assert_eq!(mj2000eq.axes, AxesKind::Mj2000Eq as i32);
    assert_eq!(mj2000eq.origin, Some(frame_definition::Origin::Body("Earth".to_string())));
}

/// `PathBuf` for a `goldens/<name>.json` fixture -- the M19.1 (question 128) frame-conversion
/// pins' own helper, named distinctly from any other `golden_path`-shaped helper this file may
/// already carry so it cannot collide with one.
fn frame_conversion_golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../goldens").join(format!("{name}.json"))
}

/// The only field the M19.1 frame-conversion goldens (`icrf_leo_2h.json`/`bodyfixed_leo_2h.json`,
/// both `goldens/gen_icrf_leo_2h.py`-shaped) this end-to-end pipeline test needs: GMAT's own
/// `ReportFile`-computed final state in the golden's own frame. This test deliberately does NOT
/// use the golden's own (tight, isolated-conversion) `tolerance_m`/`tolerance_mps` fields -- see
/// [`PIPELINE_TOLERANCE_M`]'s own doc comment for why a looser, separately-justified bound
/// applies here instead.
#[derive(serde::Deserialize)]
struct FrameConversionGolden {
    state_final_km: [f64; 6],
}

/// This end-to-end pipeline test's own tolerance -- deliberately looser than `icrf_leo_2h.json`'s
/// own `tolerance_m`/`tolerance_mps` (0.0001 m / 7.837e-8 m/s), because unlike
/// `crates/gmat-sys/tests/convert.rs`'s direct, isolated `Gmat::convert` pins (which feed GMAT's
/// OWN `PrinceDormand78`-propagated `EarthMJ2000Eq` state straight into `convert`, no
/// re-propagation at all), `icrf_products()` propagates the arc through THIS crate's own
/// `av_dynamics::integrate::Dopri5` over `GetDerivatives` before converting it -- the same
/// Dopri5-vs-PrinceDormand78 integrator divergence `crates/gmat-sys/tests/leo_golden.rs`'s own
/// 86400 s arc already accepts at 0.05 m / 5e-5 m/s (`goldens/leo_1day_jgm2_8x8_sunmoon.json`'s
/// own `tolerance_m`/`tolerance_mps`), scaled down for this arc's much shorter 7200 s duration.
/// Measured here: ~1.06e-3 m / ~1.17e-6 m/s -- both comfortably under this bound, and both
/// orders of magnitude below the ~1.33 m frame-bias-scale error the pre-M19.1 defect this task
/// fixes would have produced (this doc comment's own history section, measurement 3).
const PIPELINE_TOLERANCE_M: f64 = 0.01;
const PIPELINE_TOLERANCE_MPS: f64 = 1e-5;

/// **Required pin (question 128, M19.1): "the LEO golden arc converted to `EarthICRF` matches
/// GMAT's own `ReportFile` in that coordinate system."** `icrf_products()`'s own `"icrf_probe"`
/// trajectory is propagated (in the integration frame, `EarthMJ2000Eq`) and then genuinely
/// converted, sample by sample, to its declared frame (`EarthICRF`) by `crate::drm::executor::
/// convert_gmat_trajectory_to_declared_frame` -- this test compares its *final* sample against
/// `goldens/icrf_leo_2h.json`'s own GMAT-`ReportFile`-computed reference for the identical arc
/// (same vehicle/orbit/force-model/epoch/duration, see that golden's own generator doc comment),
/// to [`PIPELINE_TOLERANCE_M`]/[`PIPELINE_TOLERANCE_MPS`] (see that constant's own doc comment
/// for why this is looser than the golden's own tight `tolerance_m`/`tolerance_mps` -- those are
/// met directly, tightly, by `crates/gmat-sys/tests/convert.rs`'s own isolated pins instead).
///
/// Fails against the pre-M19.1 implementation this module doc comment's history section
/// describes: `traj.frame_id == "EarthICRF"` while the numbers underneath stayed silently
/// `EarthMJ2000Eq`-equivalent, disagreeing with this golden by ~1.33 m (measurement 3) -- two
/// orders of magnitude past [`PIPELINE_TOLERANCE_M`] -- also fails against an implementation
/// that converts with the wrong unit convention (a silent km/m mix, ADR-002's own spike-rule
/// warning: still self-consistent on a round trip, but wrong by a factor of 1000 against this
/// genuine external reference).
#[test]
fn icrf_conversion_matches_the_genuine_gmat_reportfile() {
    let _engine = gmat_sys::engine_lock();
    let golden: FrameConversionGolden = serde_json::from_str(&std::fs::read_to_string(frame_conversion_golden_path("icrf_leo_2h")).unwrap()).unwrap();
    let products = icrf_products();
    let traj = products.trajectories.get("icrf_probe").unwrap();
    assert_eq!(traj.frame_id, "EarthICRF");
    let last = traj.samples.last().expect("icrf_probe has at least one sample");

    let dr_m = (0..3).map(|i| (last.mean[i] - golden.state_final_km[i] * 1000.0).powi(2)).sum::<f64>().sqrt();
    let dv_mps = (3..6).map(|i| (last.mean[i] - golden.state_final_km[i] * 1000.0).powi(2)).sum::<f64>().sqrt();
    eprintln!("[icrf_conversion] |dr| = {dr_m:.6e} m (tol {PIPELINE_TOLERANCE_M}), |dv| = {dv_mps:.6e} m/s (tol {PIPELINE_TOLERANCE_MPS})");
    assert!(dr_m < PIPELINE_TOLERANCE_M, "EarthICRF position error {dr_m} m exceeds tolerance {PIPELINE_TOLERANCE_M} m");
    assert!(dv_mps < PIPELINE_TOLERANCE_MPS, "EarthICRF velocity error {dv_mps} m/s exceeds tolerance {PIPELINE_TOLERANCE_MPS} m/s");
}

// **Required pin (question 128, M19.1) for `EarthBodyFixed`: pinned directly at the
// `gmat_sys::Gmat::convert` layer, not through this crate's full DRM/executor pipeline.**
// `crates/gmat-sys/tests/convert.rs::convert_matches_the_genuine_gmat_reportfile_for_
// earthbodyfixed` is that pin (against `goldens/bodyfixed_leo_2h.json`), and its own doc
// comment has the full account of why a genuine, pre-existing GMAT modeling restriction
// discovered while wiring this fixture makes an ICRF-shaped end-to-end test here impossible for
// `EarthBodyFixed` specifically: GMAT refuses `DisplayStateType = Keplerian` combined with a
// non-inertial `spacecraft.CoordinateSystem` ("orbital state elements not contained in the same
// state type", confirmed empirically -- constructing a `bodyfixed_probe` instance the same way
// `icrf_system_definition` builds `icrf_probe`, just with `"EarthBodyFixed"` instead of
// `"EarthICRF"`, fails at `materialize_gmat` with exactly that GMAT error), and
// `demo_two_instance.system.yaml`'s `leo_demo_sys` (this repository's shared golden LEO
// vehicle, reused by every fixture in this file) declares `DisplayStateType = Keplerian`. A
// `"gmat."`-bound instance CAN declare `spacecraft.CoordinateSystem = "EarthBodyFixed"` (Step 4,
// `binding::parse_gmat_spec` accepts it, and `convert_gmat_trajectory_to_declared_frame` uses
// the identical code path ICRF does, parametrized only by axes) provided its own
// `DisplayStateType` is `Cartesian` -- just not `leo_demo_sys` as declared. The time-varying
// rotation `convert_gmat_trajectory_to_declared_frame`'s epoch argument needs to get right (see
// `docs/adr/002-dynamics-contract.md`'s fourth amendment: "if your epoch handling is wrong,
// ICRF may still pass while body-fixed fails") is exercised by the `gmat-sys`-level pin
// instead, which calls the identical `Gmat::convert` this executor function calls, at a real,
// non-trivial epoch, and is not subject to this Keplerian/non-inertial restriction at all
// (it operates on a bare Cartesian state vector, no `Spacecraft` object involved).

/// **Required pin: a round trip `from -> to -> from` is the identity to 1e-9 m.** Takes
/// `icrf_products()`'s own final (already-converted) `EarthICRF` sample as a real, converged
/// state, converts it to `EarthMJ2000Eq` and straight back to `EarthICRF` through two direct
/// `gmat_sys::Gmat::convert` calls at the sample's own epoch, and checks the result against the
/// original value. Fails against a conversion whose forward and inverse rotations are not
/// genuine inverses of each other (e.g. a sign error, or the two calls disagreeing on which
/// `CoordinateSystem` is `from`/`to`) -- note this test alone cannot catch a silent km/m mix or a
/// silently-wrong epoch (either would cancel out identically across a round trip), which is
/// exactly why the golden comparisons above exist too; see `crates/gmat-sys/tests/convert.rs`
/// for the same round-trip property pinned directly against the shim, independent of av-kernel.
#[test]
fn icrf_round_trip_through_mj2000eq_is_the_identity_to_1e_minus_9_m() {
    let _engine = gmat_sys::engine_lock();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let icrf_traj = icrf_products().trajectories.get("icrf_probe").unwrap();
    let last = icrf_traj.samples.last().expect("icrf_probe has at least one sample");
    let epoch_a1mjd = av_cdm::time::Tai::from_nanos(last.tai_ns).to_a1_mjd();
    let original_icrf_km = av_cdm::units::state_m_to_km(last.mean[0..6].try_into().unwrap());

    gmat.coordinate_system("RoundTripIcrf", "Earth", "ICRF").unwrap();
    gmat.coordinate_system("RoundTripMj2000Eq", "Earth", "MJ2000Eq").unwrap();
    gmat.initialize().unwrap();
    let mj2000eq_km = gmat.convert(epoch_a1mjd, &original_icrf_km, "RoundTripIcrf", "RoundTripMj2000Eq").unwrap();
    let back_icrf_km = gmat.convert(epoch_a1mjd, &mj2000eq_km, "RoundTripMj2000Eq", "RoundTripIcrf").unwrap();

    let dr_m = (0..3).map(|i| (back_icrf_km[i] - original_icrf_km[i]).powi(2)).sum::<f64>().sqrt() * 1000.0;
    let dv_mps = (3..6).map(|i| (back_icrf_km[i] - original_icrf_km[i]).powi(2)).sum::<f64>().sqrt() * 1000.0;
    eprintln!("[round_trip] |dr| = {dr_m:.3e} m, |dv| = {dv_mps:.3e} m/s (both must be < 1e-9 m)");
    assert!(dr_m < 1e-9, "round-trip position residual {dr_m} m exceeds 1e-9 m");
    assert!(dv_mps < 1e-9, "round-trip velocity residual {dv_mps} m/s exceeds 1e-9 m/s");
}

// ==========================================================================================
// 5. `FrameDefinition.fixed_rotation_q` on the wire (M19.2, `docs/open-questions.md` question
//    129, ADR-002's fourth amendment): the demo bundle's own committed `EarthICRF` (declared in
//    `demo_two_instance.drm.yaml`) must carry a filled, unit `fixed_rotation_q`; `EarthMJ2000Eq`
//    (the reference itself) and `EarthBodyFixed` (genuinely time-varying -- Earth rotates) must
//    not.
// ==========================================================================================

/// Required test: "ICRF against MJ2000Eq is the case that must come out filled." Fails against
/// the pre-M19.2 executor, which always leaves `fixed_rotation_q` empty
/// (`crates/av-kernel/src/drm/schema.rs`'s own `RawFrameDefinition::into_pb` comment: "filled by
/// the producer through the dynamics contract's convert (M19.2)" -- before this task landed,
/// nothing did) -- this test would see `fixed_rotation_q.is_empty()` instead of a length-4,
/// unit-norm value.
#[test]
fn demo_two_instance_earthicrf_fixed_rotation_q_is_filled_and_unit() {
    let _engine = gmat_sys::engine_lock();
    let products = together_products();
    let icrf = products.frames.iter().find(|f| f.id == "EarthICRF").expect("EarthICRF present");
    assert_eq!(icrf.fixed_rotation_q.len(), 4, "EarthICRF's fixed_rotation_q must be filled (question 129: ICRF against MJ2000Eq is the frame bias, a constant rotation)");
    let [w, x, y, z] = <[f64; 4]>::try_from(icrf.fixed_rotation_q.clone()).unwrap();
    let norm = (w * w + x * x + y * y + z * z).sqrt();
    eprintln!("[fixed_rotation_q] EarthICRF: q = [w={w}, x={x}, y={y}, z={z}], norm = {norm}");
    assert!((norm - 1.0).abs() < 1e-9, "EarthICRF's fixed_rotation_q must be a unit quaternion; norm = {norm}");
}

/// Required test: `AXES_KIND_MJ2000_EQ` is the reference `fill_fixed_rotations` measures every
/// other body-axes frame against -- there is nothing to measure it against itself, so it must
/// stay empty (mirrors `parent_frame_id`'s own "" root convention for it, question 76). Fails
/// against an implementation that computes a trivial self-vs-self identity rotation and fills it
/// anyway (harmless numerically, but a wire value some future consumer could misinterpret as "an
/// explicit fixed rotation was declared here").
#[test]
fn demo_two_instance_earthmj2000eq_fixed_rotation_q_stays_empty() {
    let _engine = gmat_sys::engine_lock();
    let products = together_products();
    let mj2000eq = products.frames.iter().find(|f| f.id == "EarthMJ2000Eq").expect("EarthMJ2000Eq present");
    assert!(mj2000eq.fixed_rotation_q.is_empty(), "EarthMJ2000Eq is the reference frame itself; fixed_rotation_q must stay empty, got {:?}", mj2000eq.fixed_rotation_q);
}

/// Required test: "Body-fixed... stay empty -- Earth rotates, so body-fixed is time-varying by
/// construction; if your constancy check ever marks a body-fixed frame as fixed, that is a bug
/// in the check" (this task's own brief). Fails against exactly that bug: an implementation that
/// runs the same fixed-rotation fill against every body-axes frame indiscriminately (e.g.
/// forgetting to restrict the candidate axes kinds to ICRF/MJ2000Ec) would either crash (Earth's
/// rotation over 10 s does not round-trip through a single fixed quaternion the way this test
/// would still accept) or, worse, silently fill a wildly wrong "fixed" rotation for a frame that
/// visibly spins once a day.
#[test]
fn demo_two_instance_earthbodyfixed_fixed_rotation_q_stays_empty() {
    let _engine = gmat_sys::engine_lock();
    let products = together_products();
    let body_fixed = products.frames.iter().find(|f| f.id == "EarthBodyFixed").expect("EarthBodyFixed present");
    assert!(body_fixed.fixed_rotation_q.is_empty(), "EarthBodyFixed rotates with Earth; fixed_rotation_q must stay empty, got {:?}", body_fixed.fixed_rotation_q);
}
