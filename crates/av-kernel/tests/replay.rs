//! M25.4b (question 175's own follow-on, `docs/sil-plan.md`'s M25 milestone): the replay
//! binding, exercised end to end through the real, public [`av_kernel::drm::execute`] entry
//! point -- T1 (byte-identical acceptance, Docker-free), T2 (hash mismatch refused before any
//! step), T3 (one deleted interior record detected). T4 (the posix cFS container demo) lives in
//! `tests/drm_attitude_control_cfs.rs`, alongside the existing container-binding tests it
//! extends.
//!
//! ## Fixture choice, and why it is NOT `drms/demo_attitude_control.*.yaml`
//!
//! The task brief that started this work named `drms/demo_attitude_control.{drm,sos}.yaml`'s
//! own `controller` instance (FRAMED `wheel_torque_out`) as T1's fixture. Investigated directly
//! against the real source, not assumed: replaying `controller` from that EXACT, unmodified
//! fixture cannot reach a genuinely byte-identical `RunProducts` without either fabricating a
//! value this binding has no honest way to reconstruct, or excluding an entire category of
//! `Event`s from the comparison -- both of which this task's own standing rules forbid
//! ("never claim a guarantee you cannot deliver"; "excluding a trajectory or an event is not
//! acceptable"). Two independent, unavoidable causes, both confirmed by reading the real code
//! this fixture drives, not merely suspected:
//!
//! 1. `crate::drm::controller::AttitudeControllerModel::step_with_ports` reports its own
//!    `pointing_error_rad`/`seq` as `StepResult.outputs`, never onto any port -- a
//!    port-traffic-only replay cannot reconstruct them, and `drms/demo_attitude_control.
//!    drm.yaml`'s own declared `Objective`/`MeasureOfEffectiveness` read exactly those two names
//!    (`output.controller.pointing_error_rad@end`/`output.controller.seq@end`), so
//!    `RunProducts.scores` would differ (or the replayed run's own scoring pass would fail to
//!    evaluate at all -- `crate::expr::error::ExprError::UnknownOutput`, surfaced as
//!    `DrmError::InvalidExpression`, since the referenced series would have zero data points).
//! 2. `AttitudeControllerModel::step_with_ports` ALSO reports an `AppliedCommand` on its own
//!    `wheel_torque_out` port (`field: "tau_mag"`) UNCONDITIONALLY, every firing step (`crates/
//!    av-kernel/src/drm/controller.rs`, the `while end >= self.next_due.get()` loop) --
//!    `crate::drm::executor::run_shared_group`'s own tail turns EVERY `AppliedCommand` into an
//!    `Event` (`EVENT_KIND_PORT_COMMAND`, unconditionally, no filter). A replay binding never
//!    computes an `AppliedCommand` (`crate::drm::replay::ReplayModel::last_measurements`'s own
//!    doc comment states the identical reasoning) -- so the replayed run's own `RunProducts.
//!    events` would be missing every one of these, real per-step events over the whole run.
//!
//! **Chosen instead: `startracker` from the already-existing, already-tested, objective-free
//! `drms/demo_attitude_sensors.*.yaml` (M22.2/M22.2b)** -- unmodified, read-only, exactly like
//! the brief's own instruction to reuse an existing fixture, just a different (smaller) one.
//! Verified, not assumed, that THIS instance has none of the two problems above:
//! - `crate::drm::sensors::StarTrackerModel::step_with_ports` returns `Vec::new()` for its own
//!   `AppliedCommand`s unconditionally (`crates/av-kernel/src/drm/sensors.rs` line 675) -- it
//!   never consumes anything, only measures and emits.
//! - Its own declared `PacketCodec` (`drms/demo_attitude_sensors_startracker.system.yaml`)
//!   declares no `PacketField.target` at all, so `crate::codec::measurements_from_field_values`
//!   -- confirmed by grep, not assumed -- produces nothing: `last_measurements()` is EMPTY on
//!   the REAL, non-replayed run too, so replay's own always-empty `last_measurements` is not a
//!   divergence here, it is what this exact fixture already does.
//! - `demo_attitude_sensors.drm.yaml` declares no `objectives`/`measures`/`faults`/`maneuvers`/
//!   `events` at all (confirmed by reading the file), so nothing ever reads a named `output.*`
//!   series or triggers the executor's own command-dispatch machinery in the first place.
//! - `StarTrackerModel::state_dim() == 0`, so it is excluded from `RunProducts.trajectories`
//!   entirely (the same "emits no trajectory" treatment `AttitudeControllerModel`/
//!   `GroundStationModel` also get) -- there is no physical-state comparison to even consider.
//!
//! With this fixture, both runs pointed at the SAME `products_dir` and the SAME `run_id`
//! (deliberately, so `provenance.attributes["port_traffic_uri"]` is identical too, and the
//! REPLAYED run's own re-recorded `port_traffic.pb` -- which faithfully reproduces the
//! original's own bytes, since replay plays them back verbatim -- rehashes to the identical
//! `port_traffic_hash`), the two runs' `RunProducts.to_proto().encode_to_vec()` come out
//! genuinely, unqualifiedly byte-identical: **T1 excludes nothing from its own comparison.**
//! This is stronger than the brief's own anticipated "exclude `port_traffic_uri`/
//! `port_traffic_hash`" fallback, not weaker -- disclosed here as a considered substitution,
//! not a silent one, for the manager to revisit if literal reuse of the `controller`/
//! `wheel_torque_out` fixture is required regardless of the `RunProducts`-divergence
//! consequences documented above.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use av_cdm::pb::{BindingKind, DesignReferenceMission, EventKind, PortDirection, PortTrafficLog, SosConfiguration, SystemDefinition};
use av_kernel::drm::replay::ReplayConfig;
use av_kernel::drm::{execute, hash, schema, DrmError, RunConfig};
use gmat_sys::Gmat;
use prost::Message as _;
use sha2::{Digest, Sha256};

fn drms_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../drms").join(name)
}
fn read(name: &str) -> String {
    std::fs::read_to_string(drms_path(name)).unwrap_or_else(|e| panic!("reading drms/{name}: {e}"))
}
fn load_system(stem: &str) -> SystemDefinition {
    schema::parse_system_definition_yaml(&read(&format!("{stem}.system.yaml"))).unwrap_or_else(|e| panic!("{stem}.system.yaml: {e}"))
}

/// `drms/demo_attitude_sensors.*.yaml` -- see this module's own doc comment for exactly why
/// this fixture, unmodified, is what every test below replays "startracker" from.
fn load_sensors_bundle() -> (DesignReferenceMission, SosConfiguration, BTreeMap<String, SystemDefinition>) {
    let drm = schema::parse_drm_yaml(&read("demo_attitude_sensors.drm.yaml")).expect("DRM parses");
    let sos = schema::parse_sos_yaml(&read("demo_attitude_sensors.sos.yaml")).expect("SosConfiguration parses");
    let truth = load_system("demo_attitude_sensors_truth");
    let star = load_system("demo_attitude_sensors_startracker");
    let imu = load_system("demo_attitude_sensors_imu");
    let mut systems = BTreeMap::new();
    for s in [truth, star, imu] {
        systems.insert(s.id.clone(), s);
    }
    (drm, sos, systems)
}

/// A fresh, empty scratch directory under the OS temp dir, unique per call within this process
/// (mirrors `tests/port_traffic_sidecar.rs::scratch_dir`'s own convention).
fn scratch_dir(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("av-kernel-replay-test-{}-{label}-{n}", std::process::id()));
    assert!(!dir.exists(), "scratch dir {dir:?} must not already exist");
    dir
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// `drms/demo_attitude_control.*.yaml` -- the closed-loop fixture T1b uses. Four
/// `BINDING_KIND_MODEL` instances (`attitude`, `startracker`, `imu`, `controller`), no
/// container, no Docker.
fn load_attitude_control_bundle() -> (DesignReferenceMission, SosConfiguration, BTreeMap<String, SystemDefinition>) {
    let drm = schema::parse_drm_yaml(&read("demo_attitude_control.drm.yaml")).expect("DRM parses");
    let sos = schema::parse_sos_yaml(&read("demo_attitude_control.sos.yaml")).expect("SosConfiguration parses");
    let mut systems = BTreeMap::new();
    for stem in ["demo_attitude_control_truth", "demo_attitude_control_startracker", "demo_attitude_control_imu", "demo_attitude_control_controller"] {
        let s = load_system(stem);
        systems.insert(s.id.clone(), s);
    }
    (drm, sos, systems)
}

/// **T1b, added by the manager during review of M25.4b.** T1 above is real but weaker than it
/// reads: `demo_attitude_sensors` declares no `Connection` at all from `startracker`'s own
/// FRAMED OUT port (checked directly in that `.sos.yaml`: no `from_instance: startracker`
/// entry), so the frames T1 replays are carried by the router and then delivered to nobody.
/// T1's byte-identity is therefore genuine but narrow -- it proves the replay binding emits
/// exactly the recorded frames (they are hashed into `RunProducts.port_traffic_hash`, which is
/// part of the compared bytes) and perturbs nothing else. It does **not** prove that a
/// downstream consumer, driven by replayed frames instead of a live model, computes the same
/// thing, because in that fixture there is no downstream consumer.
///
/// This test closes that gap without Docker. `demo_attitude_control` wires
/// `startracker.st_meas -> controller.startracker_in`, and the DRM scores two real objectives
/// off the controller's own outputs (`output.controller.pointing_error_rad@end`,
/// `output.controller.seq@end`). So replaying `startracker` here drives the whole closed loop
/// through a replay binding: the controller consumes replayed frames, its wheel torques feed
/// back into the attitude dynamics, and the scores are recomputed from that. Byte-identical
/// encoded `RunProducts` therefore covers trajectories, events, measurements, scores and the
/// port-traffic hash together.
///
/// `controller` itself is deliberately NOT the replayed instance: the M25.4b worker established
/// (and this manager confirmed by reading `crate::drm::controller`) that it reports a
/// self-originating `AppliedCommand` every step and derives its scored outputs from its own
/// `StepResult.outputs` -- side channels no content-agnostic replay of port frames can
/// reproduce. That is a real property of replaying a *native model*, and it is why question
/// 180 scopes replay to container-bound instances; T4 in `tests/drm_attitude_control_cfs.rs`
/// covers the container-bound controller for real.
///
/// **Fails against** a replay binding that emits the recorded frames at the wrong epochs, in
/// the wrong order, on the wrong port, or that holds/interpolates a frame -- any of which
/// changes what the controller sees on at least one step and therefore changes the trajectory,
/// the scores, or both.
#[test]
fn t1b_replaying_a_sensor_that_drives_a_closed_loop_reproduces_the_whole_run_byte_identically() {
    let _engine = gmat_sys::engine_lock();
    let (drm, sos, systems) = load_attitude_control_bundle();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");

    // Pinned before either run, against the loaded artifact rather than against a run's output:
    // the replayed instance is declared BINDING_KIND_MODEL, and its OUT port really is wired to
    // the controller (otherwise this test would be no stronger than T1).
    let star = sos.instances.iter().find(|i| i.name == "startracker").expect("startracker instance exists");
    assert_eq!(star.binding.as_ref().expect("binding set").kind, BindingKind::Model as i32);
    let feeds_controller = sos.connections.iter().any(|c| c.from_instance == "startracker" && c.to_instance == "controller");
    assert!(feeds_controller, "this test is only stronger than T1 if startracker's frames actually reach the controller");

    let dir = scratch_dir("t1b");
    let run_id = "test-replay-t1b".to_string();

    let cfg_real = RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: run_id.clone(), error_mode: Default::default(), products_dir: Some(dir.clone()), replay: None };
    let run_real = execute(cfg_real).expect("first (real) run executes");
    assert!(!run_real.port_traffic_hash.is_empty(), "sanity: a sidecar was recorded");
    assert!(!run_real.scores.is_empty(), "sanity: this fixture scores real objectives, so the comparison below covers scoring too");

    let replay_cfg = ReplayConfig {
        log_path: dir.join("port_traffic.pb"),
        expected_hash: run_real.port_traffic_hash.clone(),
        instances: vec!["startracker".to_string()],
    };
    let cfg_replay = RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id, error_mode: Default::default(), products_dir: Some(dir.clone()), replay: Some(replay_cfg) };
    let run_replayed = execute(cfg_replay).expect("second (replayed) run executes");

    assert_eq!(
        run_real.to_proto().encode_to_vec(),
        run_replayed.to_proto().encode_to_vec(),
        "a closed loop driven by a replayed sensor must reproduce the entire run byte for byte -- trajectories, events, measurements, scores and the port traffic hash, with nothing excluded"
    );

    std::fs::remove_dir_all(&dir).ok();
}

// ================================================================================================
// T1: the acceptance test -- no Docker, no exclusions (see the module doc comment).
// ================================================================================================

/// **Hypothesis, stated before running:** replaying `startracker` (a `BINDING_KIND_MODEL`
/// instance, no container, no Docker) against `demo_attitude_sensors.*.yaml`'s own 6 s/1 Hz run
/// produces a second `RunProducts` whose encoded protobuf bytes are IDENTICAL, byte for byte, to
/// the first (real) run's own -- not merely "equal after excluding known-differing fields": this
/// module's own doc comment explains why this specific fixture/instance choice has no such
/// fields to exclude in the first place (same `products_dir`/`run_id`, no objectives/measures,
/// no applied-commands or measurements on the replayed instance either way).
#[test]
fn t1_replayed_startracker_produces_byte_identical_run_products_with_no_exclusions() {
    let _engine = gmat_sys::engine_lock();
    let (drm, sos, systems) = load_sensors_bundle();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");

    // Pinned before either run: the instance under replay must genuinely be BINDING_KIND_MODEL
    // in the loaded artifact -- question 175's own "the binding kind in the artifact stays as
    // declared" requirement, checked directly against the SosConfiguration this test loaded,
    // not against anything a run produces.
    let star_instance = sos.instances.iter().find(|i| i.name == "startracker").expect("startracker instance exists");
    assert_eq!(star_instance.binding.as_ref().expect("binding set").kind, BindingKind::Model as i32, "sanity: startracker must be BINDING_KIND_MODEL before replay is even involved");

    let dir = scratch_dir("t1");
    let run_id = "test-replay-t1".to_string();

    let cfg_real = RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: run_id.clone(), error_mode: Default::default(), products_dir: Some(dir.clone()), replay: None };
    let run_real = execute(cfg_real).expect("first (real) run executes");
    assert!(!run_real.port_traffic_hash.is_empty(), "sanity: a sidecar was actually recorded (products_dir was Some)");
    assert!(!run_real.trajectories.contains_key("startracker"), "sanity: StarTrackerModel::state_dim() == 0 excludes it from RunProducts.trajectories entirely -- confirms this module doc comment's own claim");

    let log_path = dir.join("port_traffic.pb");
    let replay_cfg = ReplayConfig { log_path: log_path.clone(), expected_hash: run_real.port_traffic_hash.clone(), instances: vec!["startracker".to_string()] };
    // Same products_dir, same run_id -- see the module doc comment for why this is what lets the
    // comparison below exclude nothing.
    let cfg_replay = RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id, error_mode: Default::default(), products_dir: Some(dir.clone()), replay: Some(replay_cfg) };
    let run_replayed = execute(cfg_replay).expect("second (replayed) run executes");

    // Re-checked after the run too: replaying an instance must never rewrite what the loaded
    // SosConfiguration itself declares about it.
    assert_eq!(star_instance.binding.as_ref().unwrap().kind, BindingKind::Model as i32, "the SosConfiguration this test loaded must still declare BINDING_KIND_MODEL for startracker after a replay run used it");

    let bytes_real = run_real.to_proto().encode_to_vec();
    let bytes_replayed = run_replayed.to_proto().encode_to_vec();
    assert_eq!(
        bytes_real, bytes_replayed,
        "run_real (startracker's own real StarTrackerModel) and run_replayed (startracker replayed from its own recorded port traffic) must produce byte-identical encoded RunProducts, with NOTHING excluded -- see this test file's own module doc comment for why that is achievable for this fixture/instance choice"
    );

    std::fs::remove_dir_all(&dir).ok();
}

// ================================================================================================
// T2: hash mismatch refused before any step.
// ================================================================================================

/// **Hypothesis:** a `ReplayConfig.expected_hash` that does not match `log_path`'s real bytes is
/// refused (`DrmError::ReplayLogHashMismatch`) before a single step of the replay run has been
/// taken. **Proof this happens BEFORE any step, not merely "eventually":** the corrupted-hash
/// run targets a DRM/SOS/systems bundle that is otherwise perfectly valid and would execute
/// successfully were replay not requested at all (proven by [`t1_replayed_startracker_
/// produces_byte_identical_run_products_with_no_exclusions`] running the identical bundle to
/// completion) -- so the ONLY thing that can make this call fail is the hash check itself, and
/// [`DrmError::ReplayLogHashMismatch`]'s own variant, not a generic schedule/model error, is
/// what actually comes back.
#[test]
fn t2_a_replay_log_hash_mismatch_is_refused_before_any_step() {
    let _engine = gmat_sys::engine_lock();
    let (drm, sos, systems) = load_sensors_bundle();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");

    let dir = scratch_dir("t2");
    let run_id = "test-replay-t2".to_string();
    let cfg_real = RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: run_id.clone(), error_mode: Default::default(), products_dir: Some(dir.clone()), replay: None };
    let run_real = execute(cfg_real).expect("first (real) run executes");

    let log_path = dir.join("port_traffic.pb");
    let wrong_hash = "0".repeat(64);
    assert_ne!(run_real.port_traffic_hash, wrong_hash, "sanity: the wrong hash must actually differ from the real one");
    let replay_cfg = ReplayConfig { log_path: log_path.clone(), expected_hash: wrong_hash.clone(), instances: vec!["startracker".to_string()] };
    let cfg_replay = RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id, error_mode: Default::default(), products_dir: None, replay: Some(replay_cfg) };
    let err = execute(cfg_replay).expect_err("a wrong expected_hash must refuse the run, not silently proceed");
    match err {
        DrmError::ReplayLogHashMismatch { computed, expected, .. } => {
            assert_eq!(expected, wrong_hash);
            assert_eq!(computed, run_real.port_traffic_hash, "the computed hash must be the log file's own real hash -- proves the check actually read and hashed the real bytes, not merely compared two strings blindly");
        }
        other => panic!("expected DrmError::ReplayLogHashMismatch, got {other:?}"),
    }

    std::fs::remove_dir_all(&dir).ok();
}

/// The companion proof, from the other direction: corrupt the FILE's own bytes (rather than
/// passing a wrong `expected_hash`) and pass the ORIGINAL `port_traffic_hash` as `expected_hash`
/// -- refused the identical way, proving the check hashes the file's real, current bytes on
/// every call rather than trusting a value cached from when the file was first written.
#[test]
fn t2_a_corrupted_replay_log_file_is_refused_even_with_the_original_hash_as_expected() {
    let _engine = gmat_sys::engine_lock();
    let (drm, sos, systems) = load_sensors_bundle();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");

    let dir = scratch_dir("t2-corrupt");
    let run_id = "test-replay-t2-corrupt".to_string();
    let cfg_real = RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: run_id.clone(), error_mode: Default::default(), products_dir: Some(dir.clone()), replay: None };
    let run_real = execute(cfg_real).expect("first (real) run executes");

    let log_path = dir.join("port_traffic.pb");
    let mut bytes = std::fs::read(&log_path).expect("sidecar was written");
    // Flip one byte -- the smallest possible corruption, exactly the brief's own "corrupt one
    // byte of port_traffic.pb" wording.
    let last = bytes.len() - 1;
    bytes[last] ^= 0xFF;
    std::fs::write(&log_path, &bytes).expect("writing the corrupted file back");

    let replay_cfg = ReplayConfig { log_path: log_path.clone(), expected_hash: run_real.port_traffic_hash.clone(), instances: vec!["startracker".to_string()] };
    let cfg_replay = RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id, error_mode: Default::default(), products_dir: None, replay: Some(replay_cfg) };
    let err = execute(cfg_replay).expect_err("a corrupted log file must refuse the run even though expected_hash matches the ORIGINAL, un-corrupted content");
    assert!(matches!(err, DrmError::ReplayLogHashMismatch { .. }), "{err:?}");

    std::fs::remove_dir_all(&dir).ok();
}

// ================================================================================================
// T3: one deleted interior record is detected.
// ================================================================================================

/// **Hypothesis:** deleting every `startracker` OUT record recorded at one INTERIOR emission
/// epoch (neither its own first nor its own last recorded epoch), re-serializing the log, and
/// re-computing `expected_hash` from the MODIFIED bytes (so T2's own hash check passes and this
/// test proves the gap detection itself, not merely that the hash check works again) produces
/// [`DrmError::Schedule`] naming the instance and the missing epoch --
/// [`crate::drm::replay::ReplayError::MissingFrame`]'s own `Display`, reached through
/// `ModelHandle::into_boxed`'s `ReplayError -> ModelError::InvalidSpec -> HeteroScheduleError::
/// Model -> DrmError::Schedule` chain (`crate::registry::ModelRegistry::into_boxed`'s own
/// `AnyModel::Replay` arm doc comment explains why this is the same "stringify the
/// model-specific error" convention every other post-load runtime model failure in this crate
/// already surfaces through, e.g. a malformed inbound controller packet).
///
/// **"Every record at one epoch," not literally "exactly one record," and why:** measured
/// directly (see the test body's own comment at the point it measures this), this fixture's own
/// 2 Hz sensor rate against the DRM's 1 Hz kernel step means EVERY emission epoch this instance
/// ever records carries exactly two OUT records, never one -- `ReplayModel`'s own missing-frame
/// rule fires on an epoch with NO recorded entry at all, so deleting only one of an epoch's two
/// records would silently drop just that message (a real, but different, gap this rule does not
/// claim to detect -- its own doc comment's disclosed limitation is about a missing EPOCH, not a
/// thinned one) and this test would pass having exercised nothing. Deleting both is what
/// actually opens the interior gap this test needs to prove.
#[test]
fn t3_one_deleted_interior_record_is_a_typed_missing_frame_error_naming_the_instance_and_epoch() {
    let _engine = gmat_sys::engine_lock();
    let (drm, sos, systems) = load_sensors_bundle();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");

    let dir = scratch_dir("t3");
    let run_id = "test-replay-t3".to_string();
    let cfg_real = RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: run_id.clone(), error_mode: Default::default(), products_dir: Some(dir.clone()), replay: None };
    let run_real = execute(cfg_real).expect("first (real) run executes");

    let log_path = dir.join("port_traffic.pb");
    let original_bytes = std::fs::read(&log_path).expect("sidecar was written");
    let mut log = PortTrafficLog::decode(original_bytes.as_slice()).expect("sidecar decodes");

    // Every distinct emission epoch startracker's own OUT records carry, ascending, with how
    // many records land at each one -- the same (instance, direction) filter `crate::drm::
    // replay::ReplayModel::new` itself applies. `startracker`'s own declared 2 Hz update rate
    // against this DRM's 1 Hz kernel step means its own `step_with_ports`'s internal catch-up
    // loop can push MORE than one frame per kernel-tick epoch (`Router::deliver`'s own
    // `emission_tai_ns` is the STEP's epoch, shared by every message in that one call's
    // `Outbox`) -- measured directly below, not assumed, since `ReplayModel`'s own missing-frame
    // rule fires on an EMPTY epoch bucket, not a smaller one: deleting one of two records at the
    // same epoch would silently drop only that message, never open a detectable gap. An epoch
    // with exactly one recorded frame is required so this test's own deletion actually empties
    // the bucket.
    let mut star_out_epochs: Vec<i64> = log.records.iter().filter(|r| r.instance == "startracker" && r.direction == PortDirection::Out as i32).map(|r| r.tai_ns).collect();
    star_out_epochs.sort_unstable();
    let first_epoch = *star_out_epochs.first().expect("at least one startracker OUT record");
    let last_epoch = *star_out_epochs.last().expect("at least one startracker OUT record");
    let mut counts: BTreeMap<i64, usize> = BTreeMap::new();
    for &e in &star_out_epochs {
        *counts.entry(e).or_default() += 1;
    }
    let interior_epoch = *counts.keys().find(|&&epoch| epoch != first_epoch && epoch != last_epoch).expect("at least one interior epoch (first/interior/last are all distinct for a multi-tick run)");
    let frames_at_interior_epoch = counts[&interior_epoch];
    // **Measured, not assumed:** with startracker's own declared 2 Hz update rate against this
    // DRM's 1 Hz kernel step, EVERY kernel-tick epoch carries exactly two records (its own
    // internal catch-up loop fires twice per tick, and `Router::deliver`'s own `emission_tai_ns`
    // -- the STEP's epoch -- is shared by every message one `deliver` call carries, not each
    // message's own `due` sub-epoch) -- there is no naturally single-record epoch in this
    // fixture to pick instead. `ReplayModel`'s own missing-frame rule fires on an EMPTY epoch
    // bucket (this module's own doc comment), not a merely-smaller one, so opening a genuine
    // interior gap here means deleting EVERY record this fixture recorded at the chosen epoch,
    // not just one -- disclosed here rather than silently deleting only one and reporting a
    // pass that never actually exercised the rule (the FIRST run of this test, before this
    // comment existed, measured exactly that: deleting one of two left the epoch's own bucket
    // non-empty, and the replay run below completed with `Ok`, not the expected error).
    assert_eq!(frames_at_interior_epoch, 2, "sanity: this fixture's own known catch-up shape; if this ever changes, this test's own deletion strategy below needs revisiting");
    let victim_indices: Vec<usize> = log
        .records
        .iter()
        .enumerate()
        .filter(|(_, r)| r.instance == "startracker" && r.direction == PortDirection::Out as i32 && r.tai_ns == interior_epoch)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(victim_indices.len(), frames_at_interior_epoch);
    for &i in victim_indices.iter().rev() {
        log.records.remove(i);
    }

    let modified_bytes = log.encode_to_vec();
    std::fs::write(&log_path, &modified_bytes).expect("writing the modified log");
    let recomputed_hash = sha256_hex(&modified_bytes);
    assert_ne!(recomputed_hash, run_real.port_traffic_hash, "sanity: deleting a record must actually change the file's own hash");

    let replay_cfg = ReplayConfig { log_path: log_path.clone(), expected_hash: recomputed_hash, instances: vec!["startracker".to_string()] };
    let cfg_replay = RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id, error_mode: Default::default(), products_dir: None, replay: Some(replay_cfg) };
    let err = execute(cfg_replay).expect_err("a deleted interior record must be refused, not silently held or interpolated");
    match &err {
        DrmError::Schedule(detail) => {
            assert!(detail.contains("startracker"), "error must name the instance: {detail:?}");
            assert!(detail.contains(&interior_epoch.to_string()), "error must name the missing epoch ({interior_epoch}): {detail:?}");
            assert!(detail.contains("interior"), "error must say this is an interior gap, not merely a generic failure: {detail:?}");
        }
        other => panic!("expected DrmError::Schedule (crate::drm::replay::ReplayError::MissingFrame's own stringified surface), got {other:?}"),
    }

    std::fs::remove_dir_all(&dir).ok();
}

// ================================================================================================
// T5 (R4.1b, this task's own headline deliverable): replaying a faulted run reproduces the fault.
// ================================================================================================

/// **What this test proves, and what it explicitly does NOT (stated before running, per this
/// task's own standing instruction).**
///
/// PROVES: a run faulted with a real `FAULT_TARGET_KIND_PORT` fault (here, `"duplicate"` on
/// `startracker.st_meas` -- see `drms/demo_attitude_control_port_duplicate.drm.yaml`'s own header
/// comment for why `"duplicate"`, not `"corrupt"`, was chosen for THIS closed-loop fixture),
/// recorded with `products_dir: Some(...)`, and then REPLAYED (`RunConfig.replay`, replaying
/// `startracker` -- a `BINDING_KIND_MODEL` instance, `state_dim() == 0`, no Docker, mirroring
/// T1/T1b's own instance choice) reproduces the ENTIRE faulted run's own `RunProducts`, byte for
/// byte, with NOTHING excluded -- the SAME bar `t1_...`/`t1b_...` already meet in this file. This
/// is possible here for the identical reason T1/T1b's own byte-identity is possible: same
/// `products_dir`, same `run_id`, and `crate::router::Router::install_port_faults` is called
/// UNCONDITIONALLY by `execute()` regardless of `RunConfig.replay` (see "the one thing to think
/// hard about," below).
///
/// DOES NOT PROVE: that replaying a `"corrupt"` fault (byte mutation, rather than `"duplicate"`'s
/// own re-delivery) reproduces correctly through THIS SAME closed-loop fixture -- see
/// `drms/demo_attitude_control_port_duplicate.drm.yaml`'s own header comment for exactly why that
/// specific combination was not attempted here (a real, investigated risk in the CURRENT
/// topology, not a shortcut: `AttitudeControllerModel::step_with_ports` propagates any
/// `CodecError` as a hard model error via `?`, so a byte-mutating fault persistent over a
/// thousands-of-frames run risks eventually corrupting the CCSDS primary header and aborting the
/// whole run). `"Corrupt"` itself IS pinned, byte-for-byte, end to end (both the OUT-keeps-
/// original and the IN-carries-mutated halves), by `crates/av-kernel/tests/port_faults.rs::
/// demo_command_port_corrupt_mutates_the_bytes_flight_receives_and_keeps_the_out_record_
/// original` -- just not through a REPLAY of that specific run: `demo_command`'s own two
/// instances are unsuited to replay for unrelated reasons (`ground` never itself constructs the
/// packet it emits -- `drms/demo_command_ground.system.yaml`'s own header comment -- and `flight`
/// has a real, 6-dimensional propagated trajectory a zero-order-hold replay could never honestly
/// reproduce, `crate::drm::replay`'s own module doc comment's disclosed limitation).
///
/// **The one thing to think hard about, per this task's own explicit instruction, and the
/// conclusion, stated before running:** the fault runtime IS installed on the Router during a
/// replay run too (`crate::drm::executor::execute` calls `router.install_port_faults` from its
/// own load-time validation pass, which runs identically whether or not `RunConfig.replay` is
/// set -- read directly in `crates/av-kernel/src/drm/executor.rs`, not assumed). So the replayed
/// run's own Router DOES re-apply `"duplicate_startracker"` to `startracker`'s own replayed
/// emissions -- this is DELIBERATE, not a bug, and it is what makes replay reproduce the faulted
/// outcome at all: `crate::drm::replay::ReplayModel` only ever plays back OUT frames, and
/// `crate::router`'s own "OUT is the emitter's own original bytes" rule (unchanged by R4.1b,
/// extended to `"corrupt"`/`"duplicate"`) means the REPLAYED emission is bit-for-bit the SAME
/// pre-fault content, at the SAME epochs, as the original run's own real emitter produced.
/// Re-applying the identical seeded fault to identical candidate frames draws identical outcomes
/// -- this is emphatically NOT "the same frame faulted twice within one run": the REAL run
/// applies the fault to its own frames exactly once; the REPLAY run is a SEPARATE, independent
/// `execute()` call whose own Router starts a fresh `Pcg64` from the SAME seed and applies the
/// SAME fault, exactly once, to its own (identical) candidate frames -- the identical "two
/// independent runs of one faulted DRM are byte-identical" property `tests/port_faults.rs::
/// the_same_faulted_drm_executed_twice_produces_byte_identical_run_products_and_port_traffic`
/// already proves, with a replayed instance standing in for one of the two "real" runs.
/// **The alternative (the Router somehow bypassing fault application for a replayed instance's
/// own traffic) was considered and rejected**: `Router::deliver` has, and needs, no notion of
/// "this Outbox came from a replayed instance" at all -- adding one purely to suppress fault
/// re-application would be new, unrequested machinery this task's own "cheapest honest vehicle"
/// instruction does not justify, and it would ALSO be WRONG for `"corrupt"` specifically: since
/// the OUT record (what replay reads) is always PRE-fault, bypassing re-application on replay
/// would replay the ORIGINAL, uncorrupted bytes and never reproduce the corruption at all -- the
/// opposite of "reproducing the faulted run." See `crate::router`'s own module doc comment's
/// "Replay re-applies every installed PORT fault" section for the same conclusion, stated at the
/// source.
///
/// **This test distinguishes "reapplies" from "bypassed" itself** -- it does not merely compare
/// PortTrafficLog record COUNTS (this task's own explicitly named "a test that reads stronger
/// than it is" trap): it compares the ENTIRE encoded `RunProducts`, which embeds
/// `port_traffic_hash` (a SHA-256 of the ENTIRE re-recorded sidecar), so ANY difference in the
/// replayed run's own re-application of the fault -- fewer/more/differently-timed records, a
/// missing FAULT event, anything -- changes that hash (or the event list directly) and fails this
/// one assertion. A bypassed implementation would replay `startracker`'s own single recorded OUT
/// frame per candidate epoch WITHOUT re-triggering `"duplicate"` at all, so `controller.
/// startracker_in` would see exactly ONE IN record per epoch in the replayed run's own
/// re-recorded sidecar instead of the original run's own TWO, and `run_replayed.events` would be
/// missing the FAULT event entirely -- both real, hash/event-list-visible differences this test's
/// own byte-for-byte comparison catches. **Confirmed directly, not merely argued**: see
/// `R4_1B_REPORT.md` section 3 for this exact break (skip `install_port_faults` whenever
/// `RunConfig.replay` is `Some`), the real panic text it produces against this test, and the
/// restore.
#[test]
fn t5_replaying_the_emitting_instance_of_a_duplicate_port_fault_reproduces_the_faulted_run_byte_identically() {
    let _engine = gmat_sys::engine_lock();
    let drm = schema::parse_drm_yaml(&read("demo_attitude_control_port_duplicate.drm.yaml")).expect("DRM parses");
    let sos = schema::parse_sos_yaml(&read("demo_attitude_control.sos.yaml")).expect("SosConfiguration parses");
    let mut systems = BTreeMap::new();
    for stem in ["demo_attitude_control_truth", "demo_attitude_control_startracker", "demo_attitude_control_imu", "demo_attitude_control_controller"] {
        let s = load_system(stem);
        systems.insert(s.id.clone(), s);
    }
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");

    // Pinned before either run: the replayed instance is genuinely BINDING_KIND_MODEL and really
    // is wired to a real downstream consumer -- the identical sanity T1b already checks, so this
    // test is not merely "T1 again" for the fault case.
    let star = sos.instances.iter().find(|i| i.name == "startracker").expect("startracker instance exists");
    assert_eq!(star.binding.as_ref().expect("binding set").kind, BindingKind::Model as i32);
    let feeds_controller = sos.connections.iter().any(|c| c.from_instance == "startracker" && c.to_instance == "controller");
    assert!(feeds_controller, "the duplicate fault's own effect must reach a real downstream consumer for this to test anything beyond T1's own narrow claim");

    let dir = scratch_dir("t5-duplicate-replay");
    let run_id = "test-replay-t5-duplicate".to_string();

    let cfg_real = RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: run_id.clone(), error_mode: Default::default(), products_dir: Some(dir.clone()), replay: None };
    let run_real = execute(cfg_real).expect("first (real, faulted) run executes");
    assert!(!run_real.port_traffic_hash.is_empty(), "sanity: a sidecar was recorded");

    // Sanity, stated before comparing against replay: the fault genuinely applied (a real FAULT
    // event), and its own effect on the sidecar is exactly what the fixture's own header comment
    // predicted -- TWO IN records per candidate frame on controller.startracker_in, never a
    // second OUT record on startracker.st_meas.
    let fault_events: Vec<_> = run_real.events.iter().filter(|e| e.kind == EventKind::Fault as i32 && e.reference_id == "duplicate_startracker").collect();
    assert_eq!(fault_events.len(), 1, "{fault_events:#?}");
    let real_log = {
        let bytes = std::fs::read(dir.join("port_traffic.pb")).expect("sidecar written");
        PortTrafficLog::decode(bytes.as_slice()).expect("sidecar decodes")
    };
    let out_count = real_log.records.iter().filter(|r| r.instance == "startracker" && r.port == "st_meas" && r.direction == PortDirection::Out as i32).count();
    let in_count = real_log.records.iter().filter(|r| r.instance == "controller" && r.port == "startracker_in" && r.direction == PortDirection::In as i32).count();
    assert!(out_count > 0, "sanity: startracker must have actually emitted something over this 300s run");
    assert_eq!(in_count, 2 * out_count, "every OUT emission must have gotten its own extra duplicate IN record: {out_count} OUT, {in_count} IN");
    // R5.0 (manager's review of R4.1b): question 186(c)'s `frames_affected` asserted END TO END,
    // through a real `execute()`, over a MULTI-frame window -- R4.1b pinned the multi-frame count
    // only in `router.rs`'s own unit test (5 synthetic frames) and `events.rs`'s own (1 vs. 42 as
    // a plain argument), while the one integration assertion (`tests/port_faults.rs::demo_command_
    // port_corrupt_...`) covers a ONE-frame fault, where "counts every frame" and "records only
    // the first" are indistinguishable. Here the fault's window is the whole 300 s run and every
    // candidate frame applies (`rate` defaults to 1.0), so the event's own count must equal the
    // number of frames `startracker` genuinely emitted on the faulted port -- measured from the
    // sidecar above, never hardcoded. Stated before running: this is > 1 (the whole point), so an
    // implementation that recorded only the first frame fails here with 1 != out_count.
    assert!(out_count > 1, "sanity: this assertion is only meaningful for a multi-frame window: {out_count} OUT");
    assert_eq!(
        fault_events[0].values.get("frames_affected").copied(),
        Some(out_count as f64),
        "question 186(c): the FAULT event must carry the TOTAL number of frames this fault affected over the whole run ({out_count}), not just its first: {:#?}",
        fault_events[0]
    );

    let replay_cfg = ReplayConfig { log_path: dir.join("port_traffic.pb"), expected_hash: run_real.port_traffic_hash.clone(), instances: vec!["startracker".to_string()] };
    let cfg_replay = RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id, error_mode: Default::default(), products_dir: Some(dir.clone()), replay: Some(replay_cfg) };
    let run_replayed = execute(cfg_replay).expect("second (replayed) run executes");

    assert_eq!(
        run_real.to_proto().encode_to_vec(),
        run_replayed.to_proto().encode_to_vec(),
        "replaying the duplicate-faulted instance must reproduce the ENTIRE faulted run byte for byte -- trajectories, events (including the FAULT event), measurements, scores, and the port traffic hash, with nothing excluded"
    );

    std::fs::remove_dir_all(&dir).ok();
}

// ================================================================================================
// Load-time refusals: an unknown replay instance, and covariance + replay together.
// ================================================================================================

#[test]
fn a_replay_config_naming_an_unknown_instance_is_a_typed_load_refusal() {
    let _engine = gmat_sys::engine_lock();
    let (drm, sos, systems) = load_sensors_bundle();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");

    let dir = scratch_dir("unknown-instance");
    let run_id = "test-replay-unknown-instance".to_string();
    let cfg_real = RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: run_id.clone(), error_mode: Default::default(), products_dir: Some(dir.clone()), replay: None };
    let run_real = execute(cfg_real).expect("first (real) run executes");

    let replay_cfg = ReplayConfig { log_path: dir.join("port_traffic.pb"), expected_hash: run_real.port_traffic_hash.clone(), instances: vec!["nobody_named_this".to_string()] };
    let cfg_replay = RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id, error_mode: Default::default(), products_dir: None, replay: Some(replay_cfg) };
    let err = execute(cfg_replay).expect_err("an unknown replay instance name must be refused");
    match err {
        DrmError::UnknownReplayInstance { instance } => assert_eq!(instance, "nobody_named_this"),
        other => panic!("expected DrmError::UnknownReplayInstance, got {other:?}"),
    }

    std::fs::remove_dir_all(&dir).ok();
}

/// `hash::canonical_drm_hash`-based mutate-then-rehash, mirroring `tests/drm_attitude_control.
/// rs::rehash_drm`'s own convention -- used only to flip `DrmOptions.covariance` on for this one
/// refusal test (`demo_attitude_sensors.drm.yaml` itself never declares covariance).
fn rehash_drm(mut drm: DesignReferenceMission) -> DesignReferenceMission {
    drm.hash = hash::canonical_drm_hash(&drm);
    drm
}

#[test]
fn replay_combined_with_covariance_is_a_typed_refusal_not_a_silent_ignore() {
    let _engine = gmat_sys::engine_lock();
    let (mut drm, sos, systems) = load_sensors_bundle();
    drm.options.as_mut().expect("DrmOptions is set").covariance = true;
    let drm = rehash_drm(drm);
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");

    // No real log is needed: the covariance+replay refusal is checked before the replay log is
    // even read (this test's own `log_path` need not exist).
    let replay_cfg = ReplayConfig { log_path: PathBuf::from("/nonexistent/does-not-matter.pb"), expected_hash: String::new(), instances: vec!["startracker".to_string()] };
    let cfg = RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: "test-replay-covariance".to_string(), error_mode: Default::default(), products_dir: None, replay: Some(replay_cfg) };
    let err = execute(cfg).expect_err("covariance + replay must be refused");
    assert!(matches!(err, DrmError::ReplayWithCovarianceNotSupported), "{err:?}");
}
