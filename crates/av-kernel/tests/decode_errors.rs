//! `docs/open-questions.md` question 188 (R5.2): every FRAMED consumer records a typed
//! `decode_error` event and continues on its own last good input instead of aborting the run when
//! it receives an undecodable frame -- exercised end to end through [`av_kernel::drm::execute`]
//! against `drms/demo_attitude_control_port_corrupt.drm.yaml`.
//!
//! ## R6.2 (question 193): re-derived for the start/end episode shape, not loosened
//!
//! R5.2's own shape emitted one `decode_error` event PER undecodable frame -- 300 of them for
//! this exact fixture (see "Why this exact window..." below), which question 193 (team 2, R5.2's
//! own escalation) flagged as unbounded over a longer or persistent fault. The lead's decision:
//! decode errors follow the fault-event shape questions 137/186(c) already established for
//! PORT/SENSOR faults -- one `decode_error_start` event at the first undecodable frame per
//! (instance, port), one matching `decode_error_end` when decoding resumes, carrying
//! `values["frames_affected"]`.
//!
//! **Stated before measuring (per this task's own standing rule):** against this identical
//! fixture, the new shape must produce EXACTLY **2** `decode_error_*` events on `controller`
//! (one `decode_error_start`, one `decode_error_end` -- a single episode, since the corrupt fault
//! is a single contiguous `[5s, 35s)` window with no gap in the controller's own failed decode
//! attempts inside it), with `decode_error_end.values["frames_affected"] == 300.0` -- the
//! IDENTICAL 300 the R5.2 shape counted one event per, now folded into one episode's own total,
//! per the SAME derivation "Why this exact window..." below already gives (30 s / 0.1 s
//! controller decode attempts, all rejected). `decode_error_start.tai_ns` is predicted to be
//! EXACTLY `FAULT_START_TAI_NS` (t=5s, the fault window's own declared start, which already lands
//! exactly on the controller's 0.1 s decode grid, so the very first candidate attempt inside the
//! window is also the first REAL attempt) and `decode_error_end.tai_ns` EXACTLY `FAULT_END_TAI_NS`
//! (t=35s, the window's own declared, half-open end -- the router's own `corrupt` fault no longer
//! applies there, per `[start, end)` semantics, so the controller's first post-window decode
//! attempt, which also lands exactly on the 0.1 s grid, succeeds immediately, closing the episode
//! at that exact epoch with `resumed == true`). This is the router-level 600/controller-level 300
//! derivation immediately below, read through the episode lens, not re-derived from scratch.
//! **Measured**: matches exactly -- see [`corrupt_startracker_run_completes_with_exactly_the_
//! predicted_decode_error_event_count`]'s own assertions.
//!
//! ## Why this exact window and corruption shape (stated before running, per this task's own
//! instruction)
//!
//! `AttitudeControllerModel::step_with_ports` only ever decodes the LAST star tracker message in
//! its own `Inbox` each call (`av_dynamics::Inbox::last_on_port`) -- the star tracker's declared
//! 20 Hz emission rate against the controller's own declared 10 Hz update rate means exactly two
//! star tracker frames land in the controller's `Inbox` every controller step (every 0.1 s), and
//! only the LATER one (whose own emission epoch is an exact multiple of the controller's own 0.1 s
//! period) is ever actually handed to `codec::decode_packet` -- the earlier one is silently never
//! even attempted, corrupted or not. A PERSISTENT (whole-run) corrupt fault would therefore still
//! draw ~6000 independent corruptions at the router (20 Hz * 300 s) but produce an UNBOUNDED
//! number of `decode_error` events (one per rejected frame, question 188's own literal wording) --
//! exactly the risk this task's own instructions say to escalate, not build. `drms/
//! demo_attitude_control_port_corrupt.drm.yaml` instead uses a BOUNDED 30 s window
//! (`[start+5s, start+35s)`, reusing `drms/demo_attitude_control_startracker_dropout.drm.yaml`'s
//! own proven window) with a DECLARED `corrupt_mask` (0xFF, not the no-mask random bit-flip) --
//! deterministic, so the rejected-frame count is exact and hand-computable, not seed-dependent:
//!
//! - The router draws 30 s * 20 Hz = **600** candidate frames in the window; every one is
//!   corrupted (a declared mask always applies at `rate == 1.0`, the fixture's own default) --
//!   `EVENT_KIND_FAULT` for `corrupt_startracker` carries `values["frames_affected"] == 600.0`.
//! - The controller only ever attempts to decode one of every two (the later one, landing exactly
//!   on its own 0.1 s step boundary) -- 30 s / 0.1 s = **300** decode ATTEMPTS inside the window,
//!   every one of them corrupted (deterministic mask, not a per-frame coin flip), so **exactly
//!   300** is the predicted, and asserted, `decode_error` event count on `controller`.
//!
//! ## The headline divergence derivation (stated before measuring)
//!
//! An undecodable frame leaves `AttitudeControllerModel::last_star_q` untouched -- so from the
//! fault's own start epoch onward, the controller's own belief of the star tracker's quaternion is
//! frozen at whatever it last successfully decoded (the frame from just before the window opens),
//! while `last_imu_omega` keeps updating live (the IMU is unfaulted). This is STRUCTURALLY THE
//! SAME mechanism `crate::drm::sensors::StarTrackerFaultEffect::Freeze` (and, closer still,
//! `Dropout`) already produces one layer upstream -- `tests/sensor_faults.rs`'s own headline test
//! (`dropout_fixture_true_pointing_error_diverges_below_the_unfaulted_baseline_...`) derives the
//! IDENTICAL linearization for the IDENTICAL fixture topology, gains (`kp=0.25`, `kd=5.0`,
//! `Jz=50`, `tau=20s`) and window start (`t=5s`): freezing `qv_z` at its own t=5s value
//! (`qv_z0 = sin(theta(5)/2) ~= theta(5)/2`, `theta(5) = 0.2*1.25*exp(-0.25) = 0.194701` rad from
//! `demo_attitude_control.drm.yaml`'s own closed form) while `omega_z` stays live gives a
//! first-order relaxation with a CONSTANT forcing term that "overdrives" the decay relative to the
//! baseline's own live, shrinking `qv_z` -- the faulted TRUE pointing error is predicted to end up
//! SMALLER than the baseline's own trajectory during the window, growing from roughly the SAME
//! order of magnitude that derivation measured (~0.003 rad at t=20s, ~0.019 rad by t=35s), then
//! reconverging to the same ~1e-4 rad noise-floor order of magnitude by run end (265 s, >13 time
//! constants, of real feedback remain after the window closes). This test predicts the SAME
//! direction and SAME rough magnitude for exactly that reason (identical physics, one layer
//! downstream), NOT a re-derivation from scratch -- but does not assume bit-identical numbers: the
//! frozen epoch/value here is whatever the controller's own last good decode was just before t=5s
//! (a slightly different noise realization than the star tracker's own R5.1a fixture, and drawn
//! from the "corrupt" fault's own independent seeded stream, never the "dropout"/"freeze" one), so
//! the measured numbers below are real, not copied.
//!
//! **Measured** (both runs executed in
//! [`corrupt_fixture_true_pointing_error_diverges_below_the_unfaulted_baseline_during_the_window_and_reconverges_by_run_end`],
//! real noise and discretization included): t=5s baseline-faulted = 0.0 exactly (bit-identical,
//! confirming nothing has diverged before the fault epoch); t=20s gap = 2.7332e-3 rad (predicted
//! ~0.0028, within 3%); t=35s gap = 1.9061e-2 rad (predicted ~0.0191, within 0.2%); t=300s gap =
//! 1.6131e-7 rad (both fully re-settled, even closer than `tests/sensor_faults.rs`'s own
//! -2.3807e-5 rad, since this fault's own window closes at the identical epoch but the
//! post-window RNG streams here are corrupt-fault-seeded, not dropout-fault-seeded, so the exact
//! residual is a different, but equally tiny, draw). The linearization lands within a few percent
//! of the real, noisy measurement at both in-window epochs, confirming the "same mechanism, one
//! layer downstream" claim above is not merely asserted but actually what the divergence
//! assertions below measure -- **the 600/300 event-count derivation and the divergence
//! measurement were BOTH confirmed correct on the first real run** (no window/parameter
//! adjustment was needed, unlike R5.1a's own first attempt at this fixture family, which needed a
//! manager-review correction pass -- see `R5_1A_REPORT.md` section 8).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use av_cdm::pb::{DesignReferenceMission, EventKind, PortTrafficLog, SosConfiguration, SystemDefinition};
use av_kernel::drm::{execute, schema, RunConfig};
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
fn load_drm(name: &str) -> DesignReferenceMission {
    schema::parse_drm_yaml(&read(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}
fn run_config<'a>(gmat: &'a Gmat, drm: &'a DesignReferenceMission, sos: &'a SosConfiguration, systems: &'a BTreeMap<String, SystemDefinition>, run_id: &str, products_dir: Option<PathBuf>) -> RunConfig<'a> {
    RunConfig { gmat, drm, sos, systems, run_id: run_id.to_string(), error_mode: Default::default(), products_dir, replay: None }
}
fn scratch_dir(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("decode-errors-test-{}-{label}-{n}", std::process::id()));
    assert!(!dir.exists(), "scratch dir {dir:?} must not already exist");
    dir
}
fn read_port_traffic_log(path: &std::path::Path) -> PortTrafficLog {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("reading {path:?}: {e}"));
    PortTrafficLog::decode(bytes.as_slice()).unwrap_or_else(|e| panic!("{path:?} did not decode as a PortTrafficLog: {e}"))
}

const CONTROL_START_TAI_NS: i64 = 1_767_225_637_000_000_000;
const FAULT_START_TAI_NS: i64 = CONTROL_START_TAI_NS + 5_000_000_000;
const FAULT_END_TAI_NS: i64 = CONTROL_START_TAI_NS + 35_000_000_000;
/// Predicted, before running (see this file's own module doc comment's "R6.2" section): ONE
/// decode-error episode on this fixture (a single contiguous corrupt window, no gap in the
/// controller's own failed decode attempts inside it) -- `decode_error_start` + `decode_error_end`.
const EXPECTED_DECODE_ERROR_EPISODE_EVENTS: usize = 2;
/// Predicted, before running: the SAME 300 the pre-R6.2 per-occurrence shape counted one event
/// per (30 s / 0.1 s controller step, this file's own "Why this exact window..." section), now
/// folded into the one episode's own `decode_error_end.values["frames_affected"]`.
const EXPECTED_DECODE_ERROR_FRAMES_AFFECTED: f64 = 300.0;
/// Predicted, before running: 30 s * 20 Hz star tracker rate -- every candidate frame the router
/// draws, corrupted (a declared `corrupt_mask` always applies at `rate == 1.0`). Unchanged by
/// R6.2 -- this is the router's OWN, unrelated PORT-fault `frames_affected`, question 186(c)'s
/// existing accounting, not the controller-level decode-error count above.
const EXPECTED_FRAMES_AFFECTED: f64 = 600.0;

fn load_control_bundle(drm_name: &str) -> (DesignReferenceMission, SosConfiguration, BTreeMap<String, SystemDefinition>) {
    let drm = load_drm(drm_name);
    let sos = schema::parse_sos_yaml(&read("demo_attitude_control.sos.yaml")).expect("SosConfiguration parses");
    let truth = load_system("demo_attitude_control_truth");
    let star = load_system("demo_attitude_control_startracker");
    let imu = load_system("demo_attitude_control_imu");
    let controller = load_system("demo_attitude_control_controller");
    let mut systems = BTreeMap::new();
    systems.insert(truth.id.clone(), truth);
    systems.insert(star.id.clone(), star);
    systems.insert(imu.id.clone(), imu);
    systems.insert(controller.id.clone(), controller);
    (drm, sos, systems)
}

/// The TRUE physical pointing error at one output sample -- identical to `tests/
/// sensor_faults.rs::true_pointing_error_at`'s own function (duplicated, not shared, since these
/// are two separate integration test binaries -- `cargo test` gives each its own crate root, so
/// there is no cheap "own module" to share a private helper from without a third crate).
fn true_pointing_error_rad(mean: &[f64]) -> f64 {
    let (qx, qy, qz, qw) = (mean[0], mean[1], mean[2], mean[3]);
    2.0 * (qx * qx + qy * qy + qz * qz).sqrt().atan2(qw)
}
fn true_pointing_error_at(products: &av_kernel::drm::RunProducts, tai_ns: i64) -> f64 {
    let traj = &products.trajectories["attitude"];
    let sample = traj.samples.iter().find(|s| s.tai_ns == tai_ns).unwrap_or_else(|| panic!("no attitude sample at tai_ns={tai_ns}; available epochs: {:?}", traj.samples.iter().map(|s| s.tai_ns).collect::<Vec<_>>()));
    true_pointing_error_rad(&sample.mean)
}

// =================================================================================================
// The headline acceptance test: the run completes, with the event recorded exactly (question
// 188's own literal "one event per undecodable frame" wording), instead of aborting.
// =================================================================================================

/// **The core of question 188/193's own acceptance bar.** `execute()` returns `Ok` (never a
/// propagated `CodecError`, never a panic) against a DRM whose star tracker port is persistently
/// corrupted for 30 s -- through R5.1b this exact shape (a `"corrupt"` PORT fault on `startracker.
/// st_meas` in this closed loop) would have hard-failed the whole run
/// (`AttitudeControllerModel::step_with_ports`'s own pre-R5.2 `?` propagation, `drms/
/// demo_attitude_control_port_duplicate.drm.yaml`'s own header comment records exactly why R4.1b
/// had to substitute `"duplicate"` for `"corrupt"` on this topology). Event count asserted
/// EXACTLY (this file's own module doc comment states the derivation and the numbers -- 2 events,
/// `frames_affected == 300` -- before this test runs) -- never "at least one," per this task's
/// own explicit instruction.
#[test]
fn corrupt_startracker_run_completes_with_exactly_the_predicted_decode_error_event_count() {
    let _engine = gmat_sys::engine_lock();
    let (drm, sos, systems) = load_control_bundle("demo_attitude_control_port_corrupt.drm.yaml");
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");

    let products = execute(run_config(&gmat, &drm, &sos, &systems, "test-decode-errors-corrupt-startracker", None)).expect("a corrupted star-tracker frame must never abort the run (question 188)");

    let decode_error_events: Vec<_> = products.events.iter().filter(|e| e.kind == EventKind::Fault as i32 && (e.name == "decode_error_start" || e.name == "decode_error_end")).collect();
    assert_eq!(
        decode_error_events.len(),
        EXPECTED_DECODE_ERROR_EPISODE_EVENTS,
        "expected exactly {EXPECTED_DECODE_ERROR_EPISODE_EVENTS} decode_error_start/_end events (one episode -- this file's own module doc comment states the derivation), got {}: {:#?}",
        decode_error_events.len(),
        decode_error_events.iter().map(|e| (e.name.as_str(), e.tai_ns)).collect::<Vec<_>>()
    );
    assert!(decode_error_events.iter().all(|e| e.entity_id == "controller"), "every decode_error_start/_end event must be attributed to the receiving instance, controller");
    assert!(decode_error_events.iter().all(|e| e.reference_id == "startracker_in"), "every decode_error_start/_end event must name the receiving port");

    let start = decode_error_events.iter().find(|e| e.name == "decode_error_start").expect("exactly one decode_error_start, asserted above");
    let end = decode_error_events.iter().find(|e| e.name == "decode_error_end").expect("exactly one decode_error_end, asserted above");
    assert!(start.provenance.as_ref().unwrap().attributes.get("codec_error").is_some_and(|s| !s.is_empty()), "decode_error_start must carry a real, non-empty codec error text");
    // Predicted exactly, before measuring (this file's own module doc comment's "R6.2" section):
    // the fault window's own declared start/end epochs both already land on the controller's 0.1s
    // decode grid, so the episode's own first-failure and first-resumed epochs coincide with them
    // exactly.
    assert_eq!(start.tai_ns, FAULT_START_TAI_NS, "decode_error_start must land at the fault window's own declared start, t=5s");
    assert_eq!(end.tai_ns, FAULT_END_TAI_NS, "decode_error_end must land at the fault window's own declared (half-open) end, t=35s -- the first post-window decode attempt, which succeeds immediately");
    assert_eq!(end.values.get("frames_affected"), Some(&EXPECTED_DECODE_ERROR_FRAMES_AFFECTED), "decode_error_end.values[\"frames_affected\"] must be exactly 300 -- the SAME count the pre-R6.2 per-occurrence shape produced one event each for");
    assert!(end.detail.contains("resumed") && !end.detail.contains("never resumed"), "the episode genuinely closed on a real resumption, not a run-end timeout: {:?}", end.detail);
    assert_ne!(start.id, end.id, "start and end must never collide on id");

    // The run genuinely completed to the end, not merely "did not panic" -- a real trajectory
    // exists through the declared end epoch (mirrors `true_pointing_error_at`'s own "attitude"
    // lookup below -- the plant's own trajectory, not the zero-state-dim controller's, which
    // `RunProducts.trajectories` does not carry an entry for).
    let traj = &products.trajectories["attitude"];
    assert!(traj.samples.iter().any(|s| s.tai_ns == CONTROL_START_TAI_NS + 300_000_000_000), "the attitude plant's own trajectory must reach the run's declared end epoch");
}

/// The router's own PORT fault event (`corrupt_startracker`) carries `frames_affected == 600` --
/// every candidate frame in the 30 s window, corrupted (question 186(c)'s existing PORT-fault
/// accounting, unrelated to and unchanged by question 188 -- checked here only to confirm this
/// fixture's own two layers (router-level corruption count vs. controller-level decode-error
/// count) are the two DIFFERENT, both-real numbers this file's own module doc comment predicts,
/// not the same number read twice).
#[test]
fn corrupt_startracker_router_level_fault_event_frames_affected_is_600() {
    let _engine = gmat_sys::engine_lock();
    let (drm, sos, systems) = load_control_bundle("demo_attitude_control_port_corrupt.drm.yaml");
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let products = execute(run_config(&gmat, &drm, &sos, &systems, "test-decode-errors-frames-affected", None)).expect("the corrupted run executes");

    let fault_events: Vec<_> = products.events.iter().filter(|e| e.kind == EventKind::Fault as i32 && e.reference_id == "corrupt_startracker").collect();
    assert_eq!(fault_events.len(), 1, "exactly one EVENT_KIND_FAULT event for the PORT fault itself (question 186(c): one per fault, at first real effect): {fault_events:#?}");
    let got = fault_events[0].values.get("frames_affected").copied().unwrap_or_else(|| panic!("no frames_affected in {:?}", fault_events[0].values));
    assert_eq!(got, EXPECTED_FRAMES_AFFECTED, "the router corrupts every one of the 600 candidate frames in the window (30s * 20Hz), regardless of how many the controller ever actually attempts to decode (300)");
}

// =================================================================================================
// R6.2 (question 193's own run-end rule, the manager's decision): a decode-error episode still
// open when the run ends.
// =================================================================================================

/// **Stated before running (see `drms/demo_attitude_control_port_corrupt_persistent.drm.yaml`'s
/// own header comment for the full derivation):** exactly ONE `decode_error_start`/`decode_error_
/// end` pair, `decode_error_start.tai_ns == FAULT_START_TAI_NS` (t=5s, identical to the bounded
/// fixture's own first attempt), `decode_error_end.tai_ns == CONTROL_START_TAI_NS + 300s` (the
/// run's own declared `end_tai_ns` -- nothing ever closes the episode naturally), `resumed ==
/// false`, `values["frames_affected"] == 2950.0` ((300 - 5) / 0.1), and `detail` states plainly
/// that decoding never resumed.
#[test]
fn corrupt_startracker_persistent_fault_leaves_the_episode_open_at_run_end_with_the_real_count() {
    let _engine = gmat_sys::engine_lock();
    let (drm, sos, systems) = load_control_bundle("demo_attitude_control_port_corrupt_persistent.drm.yaml");
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");

    let products = execute(run_config(&gmat, &drm, &sos, &systems, "test-decode-errors-corrupt-persistent", None)).expect("a persistently corrupted star-tracker port must never abort the run (question 188), even with no natural resumption (question 193)");

    let decode_error_events: Vec<_> = products.events.iter().filter(|e| e.kind == EventKind::Fault as i32 && (e.name == "decode_error_start" || e.name == "decode_error_end")).collect();
    assert_eq!(decode_error_events.len(), 2, "exactly one episode (start + end), even though it never closed naturally: {:#?}", decode_error_events.iter().map(|e| (e.name.as_str(), e.tai_ns)).collect::<Vec<_>>());

    let start = decode_error_events.iter().find(|e| e.name == "decode_error_start").expect("exactly one decode_error_start, asserted above");
    let end = decode_error_events.iter().find(|e| e.name == "decode_error_end").expect("exactly one decode_error_end, asserted above");
    let run_end_tai_ns = CONTROL_START_TAI_NS + 300_000_000_000;
    assert_eq!(start.tai_ns, FAULT_START_TAI_NS, "decode_error_start must still land at the first real decode attempt inside the window, t=5s");
    assert_eq!(end.tai_ns, run_end_tai_ns, "decode_error_end must land at the run's own declared end_tai_ns -- nothing else ever closes this episode");
    assert_eq!(end.values.get("frames_affected"), Some(&2950.0), "(300 - 5) / 0.1 = 2950 controller decode attempts, every one of them corrupted from t=5s to run end");
    assert!(end.detail.contains("never resumed"), "detail must say plainly that decoding never resumed (question 193's own instruction: no extra values key for this distinction): {:?}", end.detail);
    assert!(!end.detail.contains("decoding resumed"), "must not ALSO claim decoding resumed: {:?}", end.detail);

    // The run genuinely completed to the end regardless -- an unbounded internal accumulator
    // (~2950 decode-error records, folded into one episode) must never itself abort or hang the
    // run.
    let traj = &products.trajectories["attitude"];
    assert!(traj.samples.iter().any(|s| s.tai_ns == run_end_tai_ns), "the attitude plant's own trajectory must reach the run's declared end epoch");
}

// =================================================================================================
// The headline divergence test: true pointing error, faulted vs. the unfaulted baseline.
// =================================================================================================

/// See this file's own module doc comment for the full derivation, stated before this test's own
/// numbers were measured. Mirrors `tests/sensor_faults.rs::dropout_fixture_true_pointing_error_
/// diverges_below_the_unfaulted_baseline_during_the_window_and_reconverges_by_run_end`'s own
/// structure exactly: both DRMs executed in this one test, a difference asserted at matched
/// epochs (never an absolute bound the unfaulted baseline could also satisfy -- this round's own
/// standing rule after that exact mistake was found and fixed once already).
#[test]
fn corrupt_fixture_true_pointing_error_diverges_below_the_unfaulted_baseline_during_the_window_and_reconverges_by_run_end() {
    let _engine = gmat_sys::engine_lock();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");

    let (base_drm, base_sos, base_systems) = load_control_bundle("demo_attitude_control.drm.yaml");
    let base_products = execute(run_config(&gmat, &base_drm, &base_sos, &base_systems, "test-decode-errors-baseline", None)).expect("the unfaulted baseline DRM executes end to end");

    let (drm, sos, systems) = load_control_bundle("demo_attitude_control_port_corrupt.drm.yaml");
    let products = execute(run_config(&gmat, &drm, &sos, &systems, "test-decode-errors-corrupt-divergence", None)).expect("the corrupted DRM executes end to end, never aborting");

    let epochs = [
        ("t=5s (fault epoch)", FAULT_START_TAI_NS),
        ("t=20s (mid-window)", CONTROL_START_TAI_NS + 20_000_000_000),
        ("t=35s (window end)", FAULT_END_TAI_NS),
        ("t=300s (run end)", CONTROL_START_TAI_NS + 300_000_000_000),
    ];
    let mut faulted = [0.0; 4];
    let mut baseline = [0.0; 4];
    for (i, (label, tai_ns)) in epochs.iter().enumerate() {
        faulted[i] = true_pointing_error_at(&products, *tai_ns);
        baseline[i] = true_pointing_error_at(&base_products, *tai_ns);
        eprintln!("[corrupt fixture] {label}: faulted={:.6e} rad, baseline={:.6e} rad, baseline-faulted={:.6e} rad", faulted[i], baseline[i], baseline[i] - faulted[i]);
    }
    let [err_at_fault_epoch, err_mid_window, err_at_window_end, err_at_run_end] = faulted;
    let [base_at_fault_epoch, base_mid_window, base_at_window_end, base_at_run_end] = baseline;

    // Sanity: nothing has diverged yet at the fault epoch itself.
    assert!((err_at_fault_epoch - base_at_fault_epoch).abs() < 1e-9, "t=5s: faulted ({err_at_fault_epoch}) and baseline ({base_at_fault_epoch}) must be numerically identical -- the fault has not taken effect yet");

    // The evidence a no-op fault could not produce (see the module doc comment's derivation):
    // baseline exceeds faulted at both in-window epochs, by a margin well above the RNG-restart
    // noise floor (~1e-5 rad) a no-op would leave.
    assert!(base_at_window_end - err_at_window_end > 0.005, "t=35s (window end): baseline ({base_at_window_end} rad) must exceed faulted ({err_at_window_end} rad) by more than 0.005 rad -- the frozen, never-shrinking restoring torque must overdrive the faulted loop's own true error below the baseline's; a no-op corrupt fault would leave this gap at the RNG-restart noise floor, not this");
    assert!(base_mid_window > err_mid_window, "t=20s (mid-window): baseline ({base_mid_window} rad) must already exceed faulted ({err_mid_window} rad)");

    // Recovery: by run end, both must have re-settled to the same order of magnitude.
    assert!((err_at_run_end - base_at_run_end).abs() < 1e-3, "t=300s: faulted ({err_at_run_end} rad) must have recovered to within an order of magnitude of the baseline's own final value ({base_at_run_end} rad) -- 265s (>13 time constants) of real feedback remained after the window closed");
}

// =================================================================================================
// Determinism.
// =================================================================================================

/// Two identically-seeded executions of the identical corrupt-faulted DRM produce byte-identical
/// `RunProducts` and `PortTrafficLog` -- mirrors `tests/port_faults.rs`'s/`tests/sensor_faults.
/// rs`'s own identical determinism tests. Both executions write into the SAME `products_dir`
/// (sidesteps the sidecar-URI-embeds-the-path issue those files' own module doc comments record).
#[test]
fn the_same_corrupt_faulted_drm_executed_twice_produces_byte_identical_run_products_and_events() {
    let _engine = gmat_sys::engine_lock();
    let (drm, sos, systems) = load_control_bundle("demo_attitude_control_port_corrupt.drm.yaml");
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let dir = scratch_dir("determinism");

    let run_a = execute(run_config(&gmat, &drm, &sos, &systems, "test-decode-errors-determinism", Some(dir.clone()))).expect("first run executes");
    let log_a = read_port_traffic_log(&dir.join("port_traffic.pb"));
    let run_b = execute(run_config(&gmat, &drm, &sos, &systems, "test-decode-errors-determinism", Some(dir.clone()))).expect("second run executes");
    let log_b = read_port_traffic_log(&dir.join("port_traffic.pb"));
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(run_a.to_proto().encode_to_vec(), run_b.to_proto().encode_to_vec(), "two runs of the identical, identically-seeded corrupt-faulted DRM must produce byte-identical RunProducts");
    assert_eq!(log_a.encode_to_vec(), log_b.encode_to_vec(), "and byte-identical port_traffic.pb");
}
