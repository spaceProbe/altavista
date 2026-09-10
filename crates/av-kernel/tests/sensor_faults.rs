//! `docs/open-questions.md` question 178 (R5.1a/R5.1b): the SENSOR fault runtime for the star
//! tracker (R5.1a) and, as of R5.1b, the IMU too, exercised end to end through
//! [`av_kernel::drm::execute`]. The IMU's own load-time refusal coverage (unrecognized `kind`,
//! off-grid epoch) lives alongside the star tracker's identical tests below; the IMU's own unit-
//! level fault-effect coverage (one test per declared kind) lives in `crate::drm::sensors`'s own
//! `#[cfg(test)]` module, mirroring the star tracker's identical split.
//!
//! ## Load-time refusals
//!
//! Against `drms/demo_attitude_sensors.*.yaml` (three instances -- `attitude` (truth, not a
//! sensor), `startracker`, `imu`; a short 6 s / 1 Hz run, `start_tai_ns = 1767225637000000000`,
//! `output_period_ns = 1_000_000_000`), a synthetic `Fault` built directly in Rust and pushed onto
//! the loaded `Scenario` -- mirrors `tests/port_faults.rs`'s own identical method.
//!
//! ## The headline acceptance test
//!
//! `drms/demo_attitude_control_startracker_dropout.drm.yaml`'s own header comment derives, before
//! running, the expected TRUE pointing error trajectory during the fault's own `[5s, 35s)` window
//! **relative to the unfaulted baseline** (`demo_attitude_control.drm.yaml`, identical topology,
//! executed a second time in the SAME test) -- an R5.1a-fix correction: the first draft of this
//! test asserted only absolute bounds (`err > 1e-3` during the window, `err < 1e-2` at run end)
//! that the unfaulted baseline ALSO satisfies, so they were never evidence that `Dropout` does
//! anything at all. The corrected derivation (`crate::drm::controller`'s own linearized control
//! law, `qv_z` frozen at its own t=5s value while `omega_z` keeps updating from the live,
//! unfaulted IMU) predicts the TRUE error diverges BELOW the baseline's own trajectory during the
//! window (the frozen, never-shrinking restoring torque overdrives the decay relative to what the
//! same loop reaches with a working sensor) and reconverges to the same order of magnitude by run
//! end (265 s, over 13 closed-loop time constants, of real feedback after the window closes) --
//! see [`dropout_fixture_true_pointing_error_diverges_below_the_unfaulted_baseline_during_the_
//! window_and_reconverges_by_run_end`]'s own doc comment for the full derivation and measured
//! numbers. This file measures the TRUE pointing error directly from `RunProducts.
//! trajectories["attitude"]` (the plant's own propagated quaternion, leading 4 components of
//! `TrajectorySample.mean` -- `crate::drm::attitude::AttitudeWheelsModel`'s own state layout,
//! `[q_x,q_y,q_z,q_w,...]`), NEVER `output.controller.pointing_error_rad` (which is derived from
//! the SAME frozen star-tracker reading the fault holds stale, and would therefore stay flat and
//! uninformative for the whole window by construction, not because the plant stopped moving).
//!
//! `frames_affected` (question 186(c)) is cross-checked against a count measured from a REAL
//! run's own `PortTrafficLog` sidecar, not hardcoded: the unfaulted baseline
//! (`demo_attitude_control.drm.yaml`, identical topology) is run once with `products_dir` set,
//! and the number of `startracker.st_meas` OUT records landing inside the fault's own declared
//! `[5s, 35s)` window is the independent ground truth the faulted run's own `EVENT_KIND_FAULT`
//! event must match exactly (every emission the baseline's own star tracker really produced in
//! that window is exactly the emission the dropout fault suppressed).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use av_cdm::pb::{DesignReferenceMission, EventKind, Fault, FaultTargetKind, PortDirection, PortTrafficLog, SosConfiguration, SystemDefinition};
use av_kernel::drm::{execute, hash, schema, DrmError, RunConfig};
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
fn rehash_drm(mut drm: DesignReferenceMission) -> DesignReferenceMission {
    drm.hash = hash::canonical_drm_hash(&drm);
    drm
}
fn run_config<'a>(gmat: &'a Gmat, drm: &'a DesignReferenceMission, sos: &'a SosConfiguration, systems: &'a BTreeMap<String, SystemDefinition>, run_id: &str, products_dir: Option<PathBuf>) -> RunConfig<'a> {
    RunConfig { gmat, drm, sos, systems, run_id: run_id.to_string(), error_mode: Default::default(), products_dir, replay: None }
}
fn scratch_dir(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("sensor-faults-test-{}-{label}-{n}", std::process::id()));
    assert!(!dir.exists(), "scratch dir {dir:?} must not already exist");
    dir
}
fn read_port_traffic_log(path: &std::path::Path) -> PortTrafficLog {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("reading {path:?}: {e}"));
    PortTrafficLog::decode(bytes.as_slice()).unwrap_or_else(|e| panic!("{path:?} did not decode as a PortTrafficLog: {e}"))
}

// =================================================================================================
// Load-time refusals -- against drms/demo_attitude_sensors.*.yaml (attitude/startracker/imu).
// =================================================================================================

const SENSORS_START_TAI_NS: i64 = 1_767_225_637_000_000_000;
const SENSORS_OUTPUT_PERIOD_NS: i64 = 1_000_000_000;

fn load_sensors_bundle() -> (DesignReferenceMission, SosConfiguration, BTreeMap<String, SystemDefinition>) {
    let drm = load_drm("demo_attitude_sensors.drm.yaml");
    let sos = schema::parse_sos_yaml(&read("demo_attitude_sensors.sos.yaml")).expect("SosConfiguration parses");
    let truth = load_system("demo_attitude_sensors_truth");
    let star = load_system("demo_attitude_sensors_startracker");
    let imu = load_system("demo_attitude_sensors_imu");
    let mut systems = BTreeMap::new();
    systems.insert(truth.id.clone(), truth);
    systems.insert(star.id.clone(), star);
    systems.insert(imu.id.clone(), imu);
    (drm, sos, systems)
}

fn sensor_fault(id: &str, instance: &str, target: &str, kind: &str, tai_ns: i64, duration_ns: i64) -> Fault {
    Fault { id: id.to_string(), tai_ns, duration_ns, target_kind: FaultTargetKind::Sensor as i32, instance: instance.to_string(), target: target.to_string(), kind: kind.to_string(), ..Default::default() }
}

#[test]
fn a_sensor_fault_naming_an_unrecognized_kind_on_a_star_tracker_is_a_typed_load_error() {
    let _engine = gmat_sys::engine_lock();
    let (mut drm, sos, systems) = load_sensors_bundle();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    {
        let scenario = drm.scenario.as_mut().expect("scenario");
        scenario.seeds.insert("f_bad_kind".to_string(), 1);
        scenario.faults.push(sensor_fault("f_bad_kind", "startracker", "startracker.output", "not_a_real_kind", SENSORS_START_TAI_NS + SENSORS_OUTPUT_PERIOD_NS, 0));
    }
    let drm = rehash_drm(drm);
    let err = execute(run_config(&gmat, &drm, &sos, &systems, "test-sensor-fault-unknown-kind", None)).expect_err("an unrecognized SENSOR kind must be a typed load refusal");
    assert!(matches!(&err, DrmError::UnknownSensorFaultKind { fault_id, instance, kind } if fault_id == "f_bad_kind" && instance == "startracker" && kind == "not_a_real_kind"), "{err:?}");
}

#[test]
fn a_sensor_fault_naming_a_non_sensor_instance_is_a_typed_load_error() {
    let _engine = gmat_sys::engine_lock();
    let (mut drm, sos, systems) = load_sensors_bundle();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    {
        let scenario = drm.scenario.as_mut().expect("scenario");
        scenario.seeds.insert("f_not_sensor".to_string(), 1);
        scenario.faults.push(sensor_fault("f_not_sensor", "attitude", "startracker.output", "dropout", SENSORS_START_TAI_NS + SENSORS_OUTPUT_PERIOD_NS, 0));
    }
    let drm = rehash_drm(drm);
    let err = execute(run_config(&gmat, &drm, &sos, &systems, "test-sensor-fault-not-a-sensor", None)).expect_err("a SENSOR fault naming a non-sensor instance must be a typed load refusal");
    assert!(matches!(&err, DrmError::SensorFaultTargetNotASensor { fault_id, instance, .. } if fault_id == "f_not_sensor" && instance == "attitude"), "{err:?}");
}

/// Question 178 (R5.1b): the IMU now has a real SENSOR fault runtime too -- an unrecognized
/// `kind` naming an IMU instance is a typed load refusal, on the identical `DrmError::
/// UnknownSensorFaultKind` shape the star tracker's own identical test above proves (mirrors
/// `a_sensor_fault_naming_an_unrecognized_kind_on_a_star_tracker_is_a_typed_load_error`).
#[test]
fn a_sensor_fault_naming_an_unrecognized_kind_on_the_imu_is_a_typed_load_error() {
    let _engine = gmat_sys::engine_lock();
    let (mut drm, sos, systems) = load_sensors_bundle();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    {
        let scenario = drm.scenario.as_mut().expect("scenario");
        scenario.seeds.insert("f_imu_bad_kind".to_string(), 1);
        scenario.faults.push(sensor_fault("f_imu_bad_kind", "imu", "imu.output", "not_a_real_kind", SENSORS_START_TAI_NS + SENSORS_OUTPUT_PERIOD_NS, 0));
    }
    let drm = rehash_drm(drm);
    let err = execute(run_config(&gmat, &drm, &sos, &systems, "test-sensor-fault-imu-unknown-kind", None)).expect_err("an unrecognized SENSOR kind on the IMU must be a typed load refusal");
    assert!(matches!(&err, DrmError::UnknownSensorFaultKind { fault_id, instance, kind } if fault_id == "f_imu_bad_kind" && instance == "imu" && kind == "not_a_real_kind"), "{err:?}");
}

/// Question 178 (R5.1b): the IMU's own SENSOR fault epoch must land on the output sample grid
/// too, on the identical `DrmError::FaultEpochNotOnSampleGrid` shape the star tracker's own
/// identical test above proves (mirrors `a_sensor_fault_start_epoch_off_the_sample_grid_is_a_
/// typed_load_error`).
#[test]
fn a_sensor_fault_start_epoch_off_the_sample_grid_on_the_imu_is_a_typed_load_error() {
    let _engine = gmat_sys::engine_lock();
    let (mut drm, sos, systems) = load_sensors_bundle();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    {
        let scenario = drm.scenario.as_mut().expect("scenario");
        scenario.seeds.insert("f_imu_off_grid".to_string(), 1);
        scenario.faults.push(sensor_fault("f_imu_off_grid", "imu", "imu.output", "dropout", SENSORS_START_TAI_NS + SENSORS_OUTPUT_PERIOD_NS + 500_000_000, 0));
    }
    let drm = rehash_drm(drm);
    let err = execute(run_config(&gmat, &drm, &sos, &systems, "test-sensor-fault-imu-off-grid-start", None)).expect_err("an off-grid start epoch on the IMU must be a typed load refusal");
    assert!(matches!(&err, DrmError::FaultEpochNotOnSampleGrid { fault_id, .. } if fault_id == "f_imu_off_grid"), "{err:?}");
}

#[test]
fn a_sensor_fault_start_epoch_off_the_sample_grid_is_a_typed_load_error() {
    let _engine = gmat_sys::engine_lock();
    let (mut drm, sos, systems) = load_sensors_bundle();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    {
        let scenario = drm.scenario.as_mut().expect("scenario");
        scenario.seeds.insert("f_off_grid".to_string(), 1);
        scenario.faults.push(sensor_fault("f_off_grid", "startracker", "startracker.output", "dropout", SENSORS_START_TAI_NS + SENSORS_OUTPUT_PERIOD_NS + 500_000_000, 0));
    }
    let drm = rehash_drm(drm);
    let err = execute(run_config(&gmat, &drm, &sos, &systems, "test-sensor-fault-off-grid-start", None)).expect_err("an off-grid start epoch must be a typed load refusal");
    assert!(matches!(&err, DrmError::FaultEpochNotOnSampleGrid { fault_id, .. } if fault_id == "f_off_grid"), "{err:?}");
}

/// The END epoch (`tai_ns + duration_ns`) must ALSO land on the sample grid -- the design's own
/// item 3, distinct from the DYNAMICS/HARDWARE check (which only ever has one epoch to check).
#[test]
fn a_sensor_fault_end_epoch_off_the_sample_grid_is_a_typed_load_error() {
    let _engine = gmat_sys::engine_lock();
    let (mut drm, sos, systems) = load_sensors_bundle();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    {
        let scenario = drm.scenario.as_mut().expect("scenario");
        scenario.seeds.insert("f_off_grid_end".to_string(), 1);
        // A well-formed start (one whole period after the run start) with a duration that lands
        // the END epoch mid-period.
        scenario.faults.push(sensor_fault("f_off_grid_end", "startracker", "startracker.output", "dropout", SENSORS_START_TAI_NS + SENSORS_OUTPUT_PERIOD_NS, 1_500_000_000));
    }
    let drm = rehash_drm(drm);
    let err = execute(run_config(&gmat, &drm, &sos, &systems, "test-sensor-fault-off-grid-end", None)).expect_err("an off-grid end epoch must be a typed load refusal");
    assert!(matches!(&err, DrmError::FaultEpochNotOnSampleGrid { fault_id, tai_ns, .. } if fault_id == "f_off_grid_end" && *tai_ns == SENSORS_START_TAI_NS + SENSORS_OUTPUT_PERIOD_NS + 1_500_000_000), "{err:?}");
}

/// Two SENSOR faults on the SAME instance with overlapping windows are refused, even when their
/// own `target` strings differ -- `DrmError::OverlappingSensorFaultWindows`'s own doc comment
/// explains why this is keyed coarser than PORT's `(instance, port)`.
#[test]
fn two_sensor_faults_on_the_same_instance_with_overlapping_windows_are_a_typed_load_refusal_naming_both_ids() {
    let _engine = gmat_sys::engine_lock();
    let (mut drm, sos, systems) = load_sensors_bundle();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    {
        let scenario = drm.scenario.as_mut().expect("scenario");
        scenario.seeds.insert("f_a".to_string(), 1);
        scenario.seeds.insert("f_b".to_string(), 2);
        scenario.faults.push(sensor_fault("f_a", "startracker", "startracker.bias_rad.x", "bias", SENSORS_START_TAI_NS + SENSORS_OUTPUT_PERIOD_NS, 3_000_000_000));
        scenario.faults[0].params.insert("value".to_string(), 0.001);
        scenario.faults.push(sensor_fault("f_b", "startracker", "startracker.output", "dropout", SENSORS_START_TAI_NS + 2 * SENSORS_OUTPUT_PERIOD_NS, 3_000_000_000));
    }
    let drm = rehash_drm(drm);
    let err = execute(run_config(&gmat, &drm, &sos, &systems, "test-sensor-fault-overlap", None)).expect_err("overlapping SENSOR fault windows on one instance must be a typed load refusal");
    assert!(matches!(&err, DrmError::OverlappingSensorFaultWindows { fault_a, fault_b, instance, .. } if fault_a == "f_a" && fault_b == "f_b" && instance == "startracker"), "{err:?}");
}

// =================================================================================================
// Question 189 (R6.2): the unreachable `frames_affected == 0` end-event guard, exercised for
// real, not merely reasoned about (R5.1's own record).
// =================================================================================================

/// `executor::run_shared_group`'s own `Boundary::SensorFaultEnd` arm only emits a SENSOR
/// `EVENT_KIND_FAULT` event when `sensor_fault_totals.get(&f.id)` actually has a `Some((Some(_),
/// _))` entry -- a fault that never truly applied produces no event at all. R5.1 recorded this
/// `None` half of the guard as exercised only indirectly, because every existing SENSOR-fault
/// fixture's own fault genuinely applies at least once. `drms/demo_sensor_fault_no_truth.*.yaml`
/// (see that DRM's own header comment for the full derivation) declares a star tracker with NO
/// truth connection at all, so `StarTrackerModel::step_with_ports` never even reaches the code
/// path that would record a fault effect, regardless of the declared fault's own window.
///
/// **Asserted, before running: this is the SAME hypothesis question 189 itself names ("a sensor
/// produces no measurement when its truth input never arrives") -- measured, not assumed.**
/// (a) the run completes normally (`execute()` returns `Ok`): an unreachable sensor is a
/// legitimate, if useless, configuration, never a load-time refusal (nothing about the
/// DECLARATION is malformed, only its real-world effect is nil). (b) NOT ONE `EVENT_KIND_FAULT`
/// event appears anywhere in `RunProducts.events` -- this fixture declares no OTHER fault, so
/// this is a direct, unambiguous proof the guard's `None` branch ran.
#[test]
fn a_sensor_fault_whose_star_tracker_has_no_truth_connection_never_emits_an_event() {
    let _engine = gmat_sys::engine_lock();
    let drm = load_drm("demo_sensor_fault_no_truth.drm.yaml");
    let sos = schema::parse_sos_yaml(&read("demo_sensor_fault_no_truth.sos.yaml")).expect("SosConfiguration parses");
    let truth = load_system("demo_attitude_sensors_truth");
    let star = load_system("demo_attitude_sensors_startracker");
    let mut systems = BTreeMap::new();
    systems.insert(truth.id.clone(), truth);
    systems.insert(star.id.clone(), star);
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");

    let products = execute(run_config(&gmat, &drm, &sos, &systems, "test-sensor-fault-unreachable-guard", None))
        .expect("an unreachable star tracker (no truth connection at all) is a legitimate, if useless, configuration -- never a load-time refusal");

    let fault_events: Vec<_> = products.events.iter().filter(|e| e.kind == EventKind::Fault as i32).collect();
    assert!(fault_events.is_empty(), "a SENSOR fault that never actually applies (no truth ever arrives) must emit no EVENT_KIND_FAULT event at all -- question 189's own guard's None branch: {fault_events:#?}");
    // Belt and suspenders: specifically no event references this fault's own id either.
    assert!(!products.events.iter().any(|e| e.reference_id == "never_applies" || e.name == "never_applies"), "no event of any kind may reference the never-applied fault's own id: {:#?}", products.events);
}

// =================================================================================================
// The headline acceptance test: a star-tracker dropout in the closed attitude control loop.
// =================================================================================================

const CONTROL_START_TAI_NS: i64 = 1_767_225_637_000_000_000;
const FAULT_START_TAI_NS: i64 = CONTROL_START_TAI_NS + 5_000_000_000;
const FAULT_END_TAI_NS: i64 = CONTROL_START_TAI_NS + 35_000_000_000;

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

/// The TRUE physical pointing error at one output sample: `2*atan2(|qv|, qw)` against
/// `target_q = identity` (this fixture's own declared `controller.target_q`, `drms/
/// demo_attitude_control_controller.system.yaml`) -- the plant's own propagated quaternion IS the
/// error quaternion when the target is identity, so no separate composition is needed. Leading 4
/// components of `TrajectorySample.mean` (`crate::drm::attitude::AttitudeWheelsModel`'s own state
/// layout, `[q_x,q_y,q_z,q_w,...]`).
fn true_pointing_error_rad(mean: &[f64]) -> f64 {
    let (qx, qy, qz, qw) = (mean[0], mean[1], mean[2], mean[3]);
    2.0 * (qx * qx + qy * qy + qz * qz).sqrt().atan2(qw)
}

fn true_pointing_error_at(products: &av_kernel::drm::RunProducts, tai_ns: i64) -> f64 {
    let traj = &products.trajectories["attitude"];
    let sample = traj.samples.iter().find(|s| s.tai_ns == tai_ns).unwrap_or_else(|| panic!("no attitude sample at tai_ns={tai_ns}; available epochs: {:?}", traj.samples.iter().map(|s| s.tai_ns).collect::<Vec<_>>()));
    true_pointing_error_rad(&sample.mean)
}

/// **Stated before running** (mirrors `drms/demo_attitude_control_startracker_dropout.drm.yaml`'s
/// own header comment, "Expected effect DURING the window, corrected"; the manager's own review
/// of the first draft of this test found that its bounds -- `err > 1e-3` during the window,
/// `err < 1e-2` at run end -- are ALSO satisfied by the UNFAULTED baseline (`demo_attitude_control
/// .drm.yaml`'s own measured trajectory), so they are not evidence that `Dropout` does anything at
/// all. This version instead runs BOTH DRMs in this same test and asserts a DIFFERENCE between the
/// two trajectories at matched epochs -- a bound a no-op dropout could not satisfy, because a
/// no-op faulted run would be numerically indistinguishable from the baseline before the fault
/// epoch and would diverge from it ONLY through the RNG-restart-at-rematerialization side effect
/// (a few 1e-5-rad-scale noise-draw differences), never through the ~1e-2-rad-scale gap derived
/// below.
///
/// **Derivation (linearizing around the fault epoch, freezing `qv_z` at its own t=5s value --
/// see `crate::drm::controller`'s own module doc comment for the undamped/damped-form derivation
/// this specializes).** During the window, `AttitudeControllerModel::step_with_ports` never
/// overwrites `last_star_q` (no packet ever arrives), so `qv_z` is pinned at its own t=5s value,
/// `qv_z0 = sin(theta(5)/2) ~= theta(5)/2` (`theta(5) = 0.2*1.25*exp(-0.25) = 0.194701` rad from
/// `demo_attitude_control.drm.yaml`'s own closed form, so `qv_z0 ~= 0.097307` for this small
/// angle). `last_imu_omega` keeps updating (the IMU is unfaulted), so `omega_z` stays live. The
/// control law `tau_z = kp*qv_z + kd*omega_z` (`kp=0.25`, `kd=5.0`) then drives `omega_z` by a
/// FIRST-ORDER linear ODE with a CONSTANT forcing term (`qv_z0` no longer shrinks with the real,
/// still-decaying error the way the unfaulted loop's own `qv_z` does):
/// `Jz*omega_dot_z = -(kp*qv_z0 + kd*omega_z)`, i.e. `omega_dot_z + (kd/Jz)*omega_z =
/// -kp*qv_z0/Jz`, a relaxation toward `omega_ss = -kp*qv_z0/kd ~= -4.87e-3` rad/s with its OWN
/// time constant `tau2 = Jz/kd = 10` s (half the closed loop's own `tau = 20` s). Using the
/// undisturbed closed form's own `theta_dot(5) = -theta(0)*5/tau^2*exp(-5/tau) = -1.947e-3` rad/s
/// as the initial rate (true at t=5s in BOTH runs, since nothing has diverged yet) and integrating
/// `omega_z(t)` once more for `theta(t) = theta(5) + omega_ss*(t-5) + (omega(5)-omega_ss)*tau2*
/// (1-exp(-(t-5)/tau2))` gives, against the UNFAULTED closed form
/// `theta_base(t) = 0.2*(1+t/20)*exp(-t/20)`:
///
/// | t (s) | faulted theta (linearized) | baseline theta (closed form) | baseline - faulted |
/// |-------|-----------------------------|-------------------------------|---------------------|
/// | 20    | 0.14437                     | 0.147152                      | ~0.0028 (~1.9%)      |
/// | 35    | 0.07643                     | 0.095576                      | ~0.0191 (~20%)       |
///
/// **Measured** (both runs executed below, real noise and discretization included, not the
/// idealized linearization): t=5s baseline-faulted = 0.0 exactly (bit-identical, confirming
/// nothing has diverged before the fault epoch); t=20s baseline-faulted = 2.6896e-3 rad (predicted
/// ~0.0028); t=35s baseline-faulted = 1.8950e-2 rad (predicted ~0.0191); t=300s baseline-faulted =
/// -2.3807e-5 rad (both re-settled to the ~1e-4 rad order of magnitude, sign flipped -- the
/// desynced post-window noise streams landing on slightly different sides of zero, exactly the
/// "same order of magnitude, not bit-identical" recovery this derivation predicts). The
/// linearization's own idealized numbers land within a few percent of the real, noisy measurement
/// at both t=20s and t=35s, confirming the frozen-forcing "overdrive" mechanism -- not RNG noise
/// -- is what the divergence assertions below are actually measuring.
///
/// **The predicted DIRECTION is the opposite of "the frozen error overdrives a bigger swing than
/// baseline" read naively:** the frozen `kp*qv_z0` term does not shrink as the real angle shrinks
/// the way the unfaulted loop's own live `qv_z` does, so the faulted loop's restoring torque stays
/// relatively LARGER than the (now-smaller) real error would justify for LONGER -- it "overdrives"
/// the decay, so the TRUE angle in the faulted run ends up SMALLER (closer to zero, decaying
/// FASTER) than the unfaulted baseline's own trajectory at the same epoch, with the gap GROWING
/// over the window (small at t=20s, an order of magnitude larger by t=35s) as the linear,
/// never-shrinking overdrive keeps compounding while the baseline's own restoring term keeps
/// shrinking. (Left unchecked past this window the frozen-forcing linearization above predicts
/// `theta` would eventually cross zero, around t~=51s -- well past this fixture's own 30 s window
/// -- and diverge in the OTHER direction; not observed here, noted only to explain why "the error
/// stays large" is the wrong frame for THIS window: the absolute magnitude does stay large
/// relative to the ~1e-4 rad noise floor, which is real and worth keeping as context, but it is
/// SMALLER than, not larger than, what the same closed loop would have reached with a working
/// sensor -- the opposite of what "large" suggests on its own. Renamed below to say so.)
///
/// **Recovery, unchanged from the original derivation:** the window ends at t=35s; real feedback
/// resumes immediately, and 265s (>13 of the SAME tau=20s time constant) remain -- expected to
/// re-settle to the same ORDER OF MAGNITUDE as the baseline's own final value (not bit-identical:
/// `Dropout` desyncs which Pcg64 draws the post-window star-tracker RNG stream consumes relative
/// to the baseline's own continuous stream, since `apply_sensor_fault`/`clear_sensor_fault` each
/// re-materialize the model, which reseeds `Pcg64::new(seed)` FROM SCRATCH -- see `crate::drm::
/// sensors`'s own module doc comment).
#[test]
fn dropout_fixture_true_pointing_error_diverges_below_the_unfaulted_baseline_during_the_window_and_reconverges_by_run_end() {
    let _engine = gmat_sys::engine_lock();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");

    let (base_drm, base_sos, base_systems) = load_control_bundle("demo_attitude_control.drm.yaml");
    let base_products = execute(run_config(&gmat, &base_drm, &base_sos, &base_systems, "test-sensor-fault-dropout-baseline", None)).expect("the unfaulted baseline DRM executes end to end");

    let (drm, sos, systems) = load_control_bundle("demo_attitude_control_startracker_dropout.drm.yaml");
    let products = execute(run_config(&gmat, &drm, &sos, &systems, "test-sensor-fault-dropout-control", None)).expect("the star-tracker-dropout DRM executes end to end");

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
        eprintln!("[dropout fixture] {label}: faulted={:.6e} rad, baseline={:.6e} rad, baseline-faulted={:.6e} rad", faulted[i], baseline[i], baseline[i] - faulted[i]);
    }
    let [err_at_fault_epoch, err_mid_window, err_at_window_end, err_at_run_end] = faulted;
    let [base_at_fault_epoch, base_mid_window, base_at_window_end, base_at_run_end] = baseline;

    // Sanity: nothing has diverged yet at the fault epoch itself -- both runs share the identical
    // pre-fault control history (same seeds, no fault applied before this instant), so the plant's
    // own propagated attitude at t=5s must match to floating-point precision, not merely "close".
    assert!((err_at_fault_epoch - base_at_fault_epoch).abs() < 1e-9, "t=5s: faulted ({err_at_fault_epoch}) and baseline ({base_at_fault_epoch}) must be numerically identical -- the fault has not taken effect yet, so nothing in either run's own history differs before this instant");

    // The evidence a no-op dropout could not produce: the TRUE pointing error genuinely diverges
    // BELOW the baseline's own trajectory, growing through the window (derivation above) -- 0.005
    // rad is roughly 2.6x the derived t=20s gap's own noise floor and well under half the derived
    // t=35s gap, so it separates "diverged as derived" from "no-op" (which would differ from the
    // baseline only by ~1e-5-rad-scale RNG-restart noise) without being so tight it chases the
    // exact linearized prediction.
    assert!(base_at_window_end - err_at_window_end > 0.005, "t=35s (window end): baseline ({base_at_window_end} rad) must exceed faulted ({err_at_window_end} rad) by more than 0.005 rad -- the frozen, never-shrinking restoring torque must overdrive the faulted loop's own true error below what the same closed loop reaches with a working sensor; a no-op dropout would leave this gap at the RNG-restart noise floor (~1e-5 rad), not this");
    assert!(base_mid_window > err_mid_window, "t=20s (mid-window): baseline ({base_mid_window} rad) must already exceed faulted ({err_mid_window} rad) -- the derivation predicts a small (~0.003 rad) but nonzero gap this early in the window, growing by t=35s");

    // Recovery: by run end, both trajectories must have re-settled to the SAME order of magnitude
    // (not bit-identical -- the post-window RNG streams are desynced, see the derivation above).
    // 1e-3 rad is an order of magnitude above either run's own expected ~1e-4 rad noise-floor
    // residual, so it catches a real failure-to-recover (e.g. the uncleared-dropout break in Job
    // 1 item 4/break-and-restore item 12) while tolerating the genuinely different stochastic
    // steady state two desynced noise streams settle into.
    assert!((err_at_run_end - base_at_run_end).abs() < 1e-3, "t=300s: faulted ({err_at_run_end} rad) must have recovered to within an order of magnitude of the baseline's own final value ({base_at_run_end} rad) -- the loop had 265s (>13 time constants) of real feedback to re-settle");
}

/// `frames_affected` (question 186(c)) cross-checked against a count measured from a REAL run's
/// own `PortTrafficLog` sidecar -- the module doc comment's own full account.
#[test]
fn dropout_fault_event_frames_affected_matches_the_unfaulted_baselines_own_sidecar_count() {
    let _engine = gmat_sys::engine_lock();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");

    // The unfaulted baseline, with the sidecar enabled, as the independent ground truth.
    let (baseline_drm, sos, systems) = load_control_bundle("demo_attitude_control.drm.yaml");
    let baseline_dir = scratch_dir("baseline");
    let _baseline_products = execute(run_config(&gmat, &baseline_drm, &sos, &systems, "test-sensor-fault-baseline-sidecar", Some(baseline_dir.clone()))).expect("the unfaulted baseline executes");
    let log = read_port_traffic_log(&baseline_dir.join("port_traffic.pb"));
    let expected_frames_affected = log
        .records
        .iter()
        .filter(|r| r.instance == "startracker" && r.port == "st_meas" && r.direction == PortDirection::Out as i32 && r.tai_ns >= FAULT_START_TAI_NS && r.tai_ns < FAULT_END_TAI_NS)
        .count() as f64;
    assert!(expected_frames_affected > 0.0, "sanity: the unfaulted baseline must have emitted at least one st_meas OUT record inside the fault's own declared window");
    std::fs::remove_dir_all(&baseline_dir).ok();

    // The faulted run.
    let (faulted_drm, sos, systems) = load_control_bundle("demo_attitude_control_startracker_dropout.drm.yaml");
    let faulted_products = execute(run_config(&gmat, &faulted_drm, &sos, &systems, "test-sensor-fault-dropout-frames", None)).expect("the faulted run executes");
    let fault_events: Vec<_> = faulted_products.events.iter().filter(|e| e.kind == EventKind::Fault as i32 && e.reference_id == "dropout_startracker").collect();
    assert_eq!(fault_events.len(), 1, "exactly one EVENT_KIND_FAULT event for the dropout fault: {fault_events:#?}");
    let got = fault_events[0].values.get("frames_affected").copied().unwrap_or_else(|| panic!("no frames_affected in {:?}", fault_events[0].values));
    assert_eq!(got, expected_frames_affected, "frames_affected ({got}) must match the count of st_meas OUT records the unfaulted baseline's own sidecar recorded inside the identical window ({expected_frames_affected}) -- every emission the fault suppressed is exactly one the baseline really produced");
}

// =================================================================================================
// Determinism.
// =================================================================================================

/// Mirrors `tests/port_faults.rs::the_same_faulted_drm_executed_twice_produces_byte_identical_
/// run_products_and_port_traffic` -- both executions write into the SAME `products_dir` (the
/// second overwriting the first's `port_traffic.pb`, read back into memory before the second run
/// starts), sidestepping the sidecar-URI-embeds-the-path issue that same file's own module doc
/// comment records.
#[test]
fn the_same_dropout_faulted_drm_executed_twice_produces_byte_identical_run_products_and_port_traffic() {
    let _engine = gmat_sys::engine_lock();
    let (drm, sos, systems) = load_control_bundle("demo_attitude_control_startracker_dropout.drm.yaml");
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let dir = scratch_dir("determinism");

    let run_a = execute(run_config(&gmat, &drm, &sos, &systems, "test-sensor-fault-determinism", Some(dir.clone()))).expect("first run executes");
    let log_a = read_port_traffic_log(&dir.join("port_traffic.pb"));
    let run_b = execute(run_config(&gmat, &drm, &sos, &systems, "test-sensor-fault-determinism", Some(dir.clone()))).expect("second run executes");
    let log_b = read_port_traffic_log(&dir.join("port_traffic.pb"));
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(run_a.to_proto().encode_to_vec(), run_b.to_proto().encode_to_vec(), "two runs of the identical, identically-seeded dropout-faulted DRM must produce byte-identical RunProducts");
    assert_eq!(log_a.encode_to_vec(), log_b.encode_to_vec(), "and byte-identical port_traffic.pb");
}

// =================================================================================================
// Known pre-existing behaviour (design item 5, R5_1A_REPORT.md's own "Measurements" section):
// `materialize_plan_at_boundary` constructs a FRESH `StarTrackerModel` at every re-materialization
// -- a new `Pcg64::new(seed)`, `seq` reset to 0, `next_due` reset to `boundary + period_ns`,
// `last_truth` cleared -- and a windowed SENSOR fault means this happens TWICE (fault start, fault
// end). Measured here, not redesigned: does the CCSDS sequence count restart at 0 at each
// boundary, and does the emission grid shift? (R6.1, question 189: the second question now has a
// real, sidecar-answerable "yes" -- see the test's own doc comment for the inversion.)
// =================================================================================================

/// Decodes every `st_meas` OUT record's own CCSDS `sequence_count` field from a run's own
/// `PortTrafficLog`, in `tai_ns` order.
fn decode_star_tracker_sequence_counts(log: &PortTrafficLog) -> Vec<(i64, u16)> {
    let codec = av_kernel::drm::sensors::star_tracker_packet_codec("st_meas_codec", 200);
    let mut apid_map = av_kernel::codec::ApidMap::new();
    apid_map.insert(codec.apid, codec);
    let mut out: Vec<(i64, u16)> = log
        .records
        .iter()
        .filter(|r| r.instance == "startracker" && r.port == "st_meas" && r.direction == PortDirection::Out as i32)
        .map(|r| (r.tai_ns, av_kernel::codec::decode_packet(&apid_map, &r.payload).unwrap_or_else(|e| panic!("decoding st_meas OUT record at tai_ns={}: {e}", r.tai_ns)).sequence_count))
        .collect();
    out.sort_by_key(|(t, _)| *t);
    out
}

/// **Measured** (stated as a fact discovered by running, not predicted beforehand -- the design's
/// own item 5 asks only for measurement and disclosure, not a fix): the CCSDS sequence count DOES
/// restart at 0 at each of the two re-materialization boundaries (fault start AND fault end),
/// exactly mirroring the pre-existing DYNAMICS-fault-on-a-sensor behaviour `crate::drm::sensors`'s
/// own module doc comment already discloses for a single re-materialization. A downstream
/// consumer relying on strict sequence-count monotonicity to detect gaps across a SENSOR fault
/// boundary would see two spurious resets, not a genuine restart of the whole link. This test
/// pins the CURRENT, measured behaviour as a regression guard -- if a future change alters it,
/// this test's own failure is the signal to update this disclosure, not silently absorb the
/// change.
///
/// **R6.1 (`docs/open-questions.md` question 189) inverts this test's own second half, and this
/// is exactly the inversion the task brief asked for -- documented here, not silently dropped.**
/// R5.1a's own investigation of "does the emission grid shift" surfaced a PRE-EXISTING confound
/// (root-caused, not merely observed, and disclosed in `R5_1A_REPORT.md` as an escalation): the
/// first two OUT records after the fault-end boundary shared the IDENTICAL `tai_ns` (both
/// stamped at the boundary's own `t + one KERNEL step`, not their own individual due epochs
/// `t + 0.05s`/`t + 0.10s`), with `sequence_count` 0 and 1 distinguishing them -- `crate::router::
/// Router::deliver` stamped EVERY message in one `Outbox` with the SAME caller-supplied step-end
/// epoch, never each message's own individually-`push`ed due epoch, so the sidecar could not
/// answer "does the true, sub-kernel-step due-epoch grid shift" for this instance at all. R6.1
/// fixed exactly that: `Router::deliver` now records/delivers each message at its OWN due epoch
/// (`crate::router`'s own module doc comment, "Per-message emission epochs"), falling back to the
/// caller's coarser epoch only for a message that carries none (never the case here). **Measured
/// again, after the fix, not merely asserted from the fix's own description:** the first two OUT
/// records after the fault-end boundary now carry two DIFFERENT `tai_ns` values, 0.05 s (one
/// star-tracker period) apart -- `FAULT_END_TAI_NS + 50_000_000` (`sequence_count = 0`) then
/// `FAULT_END_TAI_NS + 100_000_000` (`sequence_count = 1`), matching `StarTrackerModel::new`'s own
/// documented `next_due = boundary + period_ns` seeding exactly. The sidecar can now answer "does
/// the true, sub-kernel-step due-epoch grid shift" directly from `PortTrafficRecord.tai_ns`
/// itself, with no need to fall back to the model's own internal `next_due` arithmetic.
#[test]
fn measured_ccsds_sequence_restarts_at_zero_at_each_rematerialization_boundary_and_the_sidecars_own_epoch_now_resolves_each_sub_step_emission_distinctly() {
    let _engine = gmat_sys::engine_lock();
    let (drm, sos, systems) = load_control_bundle("demo_attitude_control_startracker_dropout.drm.yaml");
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let dir = scratch_dir("seq-measurement");
    let _products = execute(run_config(&gmat, &drm, &sos, &systems, "test-sensor-fault-seq-measurement", Some(dir.clone()))).expect("the dropout DRM executes");
    let log = read_port_traffic_log(&dir.join("port_traffic.pb"));
    std::fs::remove_dir_all(&dir).ok();

    let seqs = decode_star_tracker_sequence_counts(&log);
    for (t, s) in seqs.iter().filter(|(t, _)| *t >= FAULT_END_TAI_NS - 200_000_000 && *t <= FAULT_END_TAI_NS + 300_000_000) {
        eprintln!("[seq measurement] window record: tai_ns={t} (offset from fault end = {}) seq={s}", t - FAULT_END_TAI_NS);
    }

    // Before the fault (last emission strictly before t=5s): seq is whatever the run-start
    // model's own continuous count reached.
    let before_fault: Vec<&(i64, u16)> = seqs.iter().filter(|(t, _)| *t < FAULT_START_TAI_NS).collect();
    let last_before = before_fault.last().expect("at least one emission before the fault epoch");
    eprintln!("[seq measurement] last emission before fault: tai_ns={} seq={}", last_before.0, last_before.1);
    assert!(last_before.1 > 0, "sanity: the run-start model must have emitted more than one packet by t=5s (continuous count, never reset yet)");

    // First emission strictly at or after the fault END (t=35s, the second re-materialization) --
    // MEASURED: seq is back at 0, not continuing from wherever it was mid-window.
    let after_end: Vec<&(i64, u16)> = seqs.iter().filter(|(t, _)| *t >= FAULT_END_TAI_NS).collect();
    let first_after = after_end.first().expect("at least one emission at or after the fault's own end epoch");
    eprintln!("[seq measurement] first emission at/after fault end: tai_ns={} seq={}", first_after.0, first_after.1);
    assert_eq!(first_after.1, 0, "MEASURED: the CCSDS sequence count restarts at 0 at the fault-end re-materialization boundary, not continuing the pre-fault count");

    // R6.1 (question 189), the inversion: the first TWO records after the boundary now carry
    // their own DISTINCT due epochs, 0.05 s (one star-tracker period) apart -- `StarTrackerModel::
    // new`'s own documented `next_due = boundary + period_ns` seeding, directly readable from the
    // sidecar now instead of only from the model's own internal arithmetic -- still distinguished
    // by sequence_count 0 then 1.
    const STAR_TRACKER_PERIOD_NS: i64 = 50_000_000; // 20 Hz (drms/demo_attitude_control_startracker.system.yaml)
    assert!(after_end.len() >= 2, "expected at least two records at/after the fault end to check the sub-step epoch pattern");
    assert_eq!(
        after_end[0].0,
        FAULT_END_TAI_NS + STAR_TRACKER_PERIOD_NS,
        "MEASURED: the first post-fault emission's own recorded epoch is the re-materialization boundary's own next_due (boundary + one period), not the coarser kernel step end"
    );
    assert_eq!(
        after_end[1].0,
        FAULT_END_TAI_NS + 2 * STAR_TRACKER_PERIOD_NS,
        "MEASURED: the second post-fault emission's own recorded epoch is one further star-tracker period along, not coalesced onto the first"
    );
    assert_ne!(after_end[0].0, after_end[1].0, "the two post-fault emissions must no longer share one coalesced sidecar tai_ns");
    assert_eq!((after_end[0].1, after_end[1].1), (0, 1), "the two distinctly-timestamped records are still distinguished by sequence_count, 0 then 1");
}
