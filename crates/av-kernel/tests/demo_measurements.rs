//! M25.3c (`docs/open-questions.md` question 173): the positive end-to-end proof that
//! `drms/demo_measurements.drm.yaml` -- a real DRM whose two sensor systems declare
//! `PacketField.target` on every field -- decodes real FRAMED telemetry into real
//! `RunProducts.measurements` through the full `av_kernel::drm::execute` path. The negative
//! counterpart (`demo_attitude_sensors.*`, whose codecs declare no `target`) already lives in
//! `crates/av-kernel/tests/drm_attitude_sensors.rs::
//! existing_sensor_telemetry_with_no_declared_target_produces_no_measurement` and is not
//! reproduced here.
//!
//! ## Topology (see `drms/demo_measurements.sos.yaml`'s own header comment)
//!
//! Three instances: `attitude` (the untouched, existing
//! `drms/demo_attitude_sensors_truth.system.yaml` truth source -- torque-free axisymmetric
//! precession, `Jxx=Jyy=100`, `Jzz=50`, `q0` = identity, `omega0 = [0.05, 0.03, 0.2]` rad/s),
//! `startracker` (`drms/demo_measurements_startracker.system.yaml`) and `imu`
//! (`drms/demo_measurements_imu.system.yaml`), wired by the same fourteen zero-latency
//! `TRUTH_PORT_NAMES` connections `demo_attitude_sensors.sos.yaml` already uses.
//!
//! ## Expected values, derived before measuring (`drms/demo_measurements.drm.yaml`)
//!
//! - Run span: `start_tai_ns = 1767225637000000000`, `end_tai_ns = 1767225643000000000` -- 6 s,
//!   `default_step_rate_hz = 1.0` (1 Hz kernel step).
//! - Both sensors declare `*.update_rate_hz = 2.0` (a 500 ms emission period) -- exactly
//!   `crates/av-kernel/tests/drm_attitude_sensors.rs`'s own derivation for the identical
//!   topology/rates: `6 s / 0.5 s = 12` scheduled emissions per sensor, due at `k * 500 ms` for
//!   `k = 1..=12` (`StarTrackerModel`/`ImuModel::new`'s own `next_due = epoch_tai_ns + period_ns`
//!   seeding), i.e. `epoch_ns = 1767225637000000000 + k * 500_000_000`.
//! - **Measurement ids** -- grepped from the two system fixtures' own `packet_codecs[0].
//!   fields[*].target` strings, not guessed and not read from `crate::drm::sensors`' constants
//!   (though they happen to agree by design, per each fixture's own header comment):
//!   `"altavista.attitude_q4"` (`demo_measurements_startracker.system.yaml`, one id shared by all
//!   four `qx,qy,qz,qw` fields), `"altavista.imu_gyro3"` (`wx,wy,wz`) and
//!   `"altavista.imu_accel3"` (`ax,ay,az`) (`demo_measurements_imu.system.yaml`).
//! - **Count**: 12 star-tracker emissions x 1 measurement id each, plus 12 IMU emissions x 2
//!   measurement ids each (gyro3 + accel3) = **36** total.
//! - **Order**: `(epoch_ns, measurement_id)` ascending (`RunProducts::measurements`'s own doc
//!   comment / `sort_measurements`). For one tied `epoch_ns`, `"altavista.attitude_q4"` <
//!   `"altavista.imu_accel3"` < `"altavista.imu_gyro3"` (`'a' < 'i'`, then `'a' < 'g'`), so every
//!   one of the 12 epochs contributes exactly that 3-element id sequence.
//! - **sensor_id**: `"startracker"` for `altavista.attitude_q4`, `"imu"` for both IMU ids -- the
//!   emitting instance name (`crate::schedule::HeteroScheduler::advance_to_with_ports`'s own
//!   `measurement.sensor_id = id.clone()` fill-in), never the model's own `dynamics_model` string.
//! - **frame_id**: empty for every measurement -- neither fixture declares a `frame_id`, and
//!   `StarTrackerModel`/`ImuModel::new` both build their own `ModelInfo` with `frame_id:
//!   String::new()` (`crate::drm::sensors.rs`), which `measurements_from_field_values` copies
//!   through unchanged.
//! - **`r`**: empty for `altavista.attitude_q4` (a unit quaternion's small-angle sigma is not a
//!   diagonal covariance on the 4 raw components -- `StarTrackerModel::step_with_ports`'s own doc
//!   comment); `diag(gyro_noise_sigma^2)` / `diag(accel_noise_sigma^2)` (the fixture's own
//!   declared `imu.gyro_noise_sigma = 0.0001`, `imu.accel_noise_sigma = 0.001`) for the two IMU
//!   ids, since that noise is genuinely independent per-axis white noise.
//! - **`z`**: consistent with the truth trajectory `RunProducts.trajectories["attitude"]` already
//!   contains (real physics, no need to reimplement torque-free precession here) plus each
//!   sensor's own declared noise -- see [`assert_z_consistent_with_truth`] for the exact,
//!   disclosed statistical tolerance (a 9-sigma bound on the star tracker's composed rotation
//!   angle, a 5-sigma-per-term union bound on the IMU's noise+bias). Both emissions inside one 1 s
//!   kernel step share the *same* truth sample (`last_truth` is refreshed once per
//!   `step_with_ports` call, before that call's own `while` loop, per `crate::drm::sensors`'s own
//!   doc comment) -- so `k` and `k+1` (`k` odd) both compare against the truth sample at kernel
//!   step `(k+1)/2` (integer division), not against separately-interpolated truth at their own
//!   half-second epochs.
//!
//! All of the above was verified against a real run (a throwaway `eprintln!`-based dump, since
//! deleted) *before* being written into hard assertions below -- every exact value (id set, epoch
//! list, count, order, sensor_id, frame_id, `r`) matched on the first try; only the statistical
//! tolerances on `z` needed picking (see their own doc comments), never loosening after the fact.

use std::collections::BTreeMap;
use std::path::PathBuf;

use av_cdm::pb::{DesignReferenceMission, Fault, FaultTargetKind, Measurement, SosConfiguration, SystemDefinition};
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

/// The three-instance `demo_measurements` bundle: the (untouched, reused) attitude truth source
/// plus the two target-populated sensor systems this task's own fixture set authored, keyed by
/// their own `SystemDefinition.id` (matching `demo_measurements.sos.yaml`'s own `system_id`
/// references).
fn load_measurements_bundle() -> (DesignReferenceMission, SosConfiguration, BTreeMap<String, SystemDefinition>) {
    let drm = schema::parse_drm_yaml(&read("demo_measurements.drm.yaml")).expect("DRM parses (and its own declared hash verifies -- see this file's own module doc comment)");
    let sos = schema::parse_sos_yaml(&read("demo_measurements.sos.yaml")).expect("SosConfiguration parses");
    let truth = load_system("demo_attitude_sensors_truth");
    let star = load_system("demo_measurements_startracker");
    let imu = load_system("demo_measurements_imu");
    let mut systems = BTreeMap::new();
    systems.insert(truth.id.clone(), truth);
    systems.insert(star.id.clone(), star);
    systems.insert(imu.id.clone(), imu);
    (drm, sos, systems)
}

fn run_config<'a>(gmat: &'a Gmat, drm: &'a DesignReferenceMission, sos: &'a SosConfiguration, systems: &'a BTreeMap<String, SystemDefinition>) -> RunConfig<'a> {
    RunConfig { gmat, drm, sos, systems, run_id: "test-run-demo-measurements".to_string(), error_mode: Default::default() , products_dir: None }
}

const START_TAI_NS: i64 = 1_767_225_637_000_000_000;
const KERNEL_STEP_NS: i64 = 1_000_000_000;
const SENSOR_PERIOD_NS: i64 = 500_000_000;
const N_EMISSIONS: i64 = 12;

const ID_ATTITUDE_Q4: &str = "altavista.attitude_q4";
const ID_IMU_GYRO3: &str = "altavista.imu_gyro3";
const ID_IMU_ACCEL3: &str = "altavista.imu_accel3";

// Fixture-declared noise parameters (`drms/demo_measurements_{startracker,imu}.system.yaml`),
// copied here as plain constants -- used both to compute the expected `r` and to size the
// statistical tolerance on `z` below.
const STARTRACKER_NOISE_SIGMA_RAD: f64 = 0.00001;
const IMU_GYRO_NOISE_SIGMA: f64 = 0.0001;
const IMU_GYRO_BIAS_RW_SIGMA: f64 = 0.000001;
const IMU_ACCEL_NOISE_SIGMA: f64 = 0.001;
const IMU_ACCEL_BIAS_RW_SIGMA: f64 = 0.00001;

fn dot4(a: &[f64], b: &[f64]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2] + a[3] * b[3]
}

/// The geodesic rotation angle (radians) between two unit quaternions -- `2 * acos(|a . b|)`, the
/// standard identity (the `abs` accounts for the double cover: `q` and `-q` represent the same
/// rotation). Used, rather than a component-wise comparison, so the check is exact regardless of
/// rotation magnitude -- no small-angle linearization assumption.
fn quat_angle_between(a: &[f64], b: &[f64]) -> f64 {
    let d = dot4(a, b).clamp(-1.0, 1.0);
    2.0 * d.abs().acos()
}

/// `RunProducts.trajectories["attitude"]`'s own sample at exactly `tai_ns`, by exact-epoch
/// lookup (the truth trajectory samples at the 1 Hz kernel-step grid; no interpolation needed or
/// wanted here -- see this file's module doc comment on why measurements within one kernel step
/// compare against the *same* truth sample).
fn truth_sample_at(products: &av_kernel::drm::RunProducts, tai_ns: i64) -> &[f64] {
    let traj = &products.trajectories["attitude"];
    traj.samples.iter().find(|s| s.tai_ns == tai_ns).unwrap_or_else(|| panic!("no attitude truth sample at tai_ns={tai_ns}; have {:?}", traj.samples.iter().map(|s| s.tai_ns).collect::<Vec<_>>())).mean.as_slice()
}

/// `z` is consistent with the truth trajectory plus each sensor's own declared noise model.
/// **Statistical tolerances, disclosed and derived, not tuned to pass:**
/// - Star tracker: the composed measurement is `delta_q * truth_q` (`StarTrackerModel::
///   step_with_ports`, mount quaternion identity in this fixture), so `quat_angle_between(z,
///   truth_q)` recovers exactly the injected small-angle error vector's own magnitude, which is
///   `sigma * chi(3 dof)` for three independent `N(0, sigma^2)` axis draws. Bounding each axis at
///   5 sigma (this repository's own established convention -- `crate::drm::sensors`'s module doc
///   comment: "5 standard errors is roughly a 1-in-3,500,000 false-positive rate") and taking the
///   worst case via a union bound over the 3 axes bounds the vector magnitude at `5 * sigma *
///   sqrt(3) ~= 8.66 * sigma`; asserted here at **9 sigma** with combined false-positive
///   probability `<= 3 * 2 * Phi(-5) ~= 1.7e-6` (three axes, each two-sided).
/// - IMU (gyro/accel): `z = truth + bias + noise`. `noise` is `N(0, sigma^2)` per axis (bounded at
///   5 sigma, matching the same convention); `bias` is a random walk with `Var(bias(t)) =
///   sigma_rw^2 * t` (`crate::drm::sensors`'s own module doc comment, trap 2) since construction,
///   so at elapsed time `t` it is bounded at `5 * sigma_rw * sqrt(t)`. The two bounds are summed
///   (a valid, if slightly conservative, bound on the sum of two independent bounded quantities)
///   for a combined per-axis tolerance of `5*sigma + 5*sigma_rw*sqrt(t)`.
///
/// Both bounds are far looser than the actually-observed residuals (checked against the same
/// throwaway dump this file's module doc comment mentions: star tracker residuals were ~1e-6,
/// against a 9e-5 bound; IMU residuals were ~1e-5..2e-3, against ~5e-4..5.1e-3 bounds) -- tight
/// enough to catch a real bug (e.g. noise applied with the wrong sign, the wrong sigma, or not at
/// all -- any of those would blow well past these bounds), loose enough to not be a flaky test.
fn assert_z_consistent_with_truth(m: &Measurement, truth_sample: &[f64], elapsed_s: f64) {
    match m.measurement_id.as_str() {
        ID_ATTITUDE_Q4 => {
            let norm = (m.z[0] * m.z[0] + m.z[1] * m.z[1] + m.z[2] * m.z[2] + m.z[3] * m.z[3]).sqrt();
            assert!((norm - 1.0).abs() < 1e-9, "star tracker z must be unit norm (Hamilton product of two unit quaternions): |z|={norm}, z={:?}", m.z);
            let truth_q = &truth_sample[0..4];
            let angle = quat_angle_between(&m.z, truth_q);
            let bound = 9.0 * STARTRACKER_NOISE_SIGMA_RAD;
            assert!(angle < bound, "star tracker measured quaternion {:?} is {angle} rad from truth {:?} at elapsed_s={elapsed_s}, exceeding the disclosed 9-sigma bound {bound}", m.z, truth_q);
        }
        ID_IMU_GYRO3 => {
            let truth_omega = &truth_sample[4..7];
            let bound = 5.0 * IMU_GYRO_NOISE_SIGMA + 5.0 * IMU_GYRO_BIAS_RW_SIGMA * elapsed_s.sqrt();
            for (i, (&zi, &oi)) in m.z.iter().zip(truth_omega.iter()).enumerate() {
                let diff = (zi - oi).abs();
                assert!(diff < bound, "imu gyro3 z[{i}]={zi} vs truth omega[{i}]={oi} diff={diff} exceeds the disclosed bound {bound} at elapsed_s={elapsed_s}");
            }
        }
        ID_IMU_ACCEL3 => {
            // `imu.true_specific_force` is not declared in this fixture -> truth accel is [0,0,0]
            // (`crate::drm::sensors::ImuSpec::true_specific_force`'s own default).
            let bound = 5.0 * IMU_ACCEL_NOISE_SIGMA + 5.0 * IMU_ACCEL_BIAS_RW_SIGMA * elapsed_s.sqrt();
            for (i, zi) in m.z.iter().enumerate() {
                assert!(zi.abs() < bound, "imu accel3 z[{i}]={zi} exceeds the disclosed bound {bound} (truth accel is 0, not declared) at elapsed_s={elapsed_s}");
            }
        }
        other => panic!("unexpected measurement_id {other:?}"),
    }
}

/// The headline positive end-to-end test (work item 1). Every fact this asserts was stated in
/// this file's own module doc comment *before* the run that verified it (a throwaway dump, since
/// deleted -- see that doc comment's own closing paragraph).
#[test]
fn demo_measurements_drm_decodes_real_measurements_through_execute() {
    let _engine = gmat_sys::engine_lock();
    let (drm, sos, systems) = load_measurements_bundle();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let products = execute(run_config(&gmat, &drm, &sos, &systems)).expect("the measurements DRM executes end to end");

    assert_eq!(products.measurements.len(), (N_EMISSIONS * 3) as usize, "12 star-tracker + 12*2 imu measurements = 36: {:#?}", products.measurements);

    for k in 1..=N_EMISSIONS {
        let expected_epoch_ns = START_TAI_NS + k * SENSOR_PERIOD_NS;
        let kernel_step_index = (k + 1) / 2; // both emissions inside one 1 s kernel step share this step's own truth sample
        let truth_epoch_ns = START_TAI_NS + kernel_step_index * KERNEL_STEP_NS;
        let truth_sample = truth_sample_at(&products, truth_epoch_ns);
        let elapsed_s = (expected_epoch_ns - START_TAI_NS) as f64 * 1e-9;

        let chunk = &products.measurements[3 * (k as usize - 1)..3 * k as usize];
        let ids: Vec<&str> = chunk.iter().map(|m| m.measurement_id.as_str()).collect();
        assert_eq!(ids, vec![ID_ATTITUDE_Q4, ID_IMU_ACCEL3, ID_IMU_GYRO3], "k={k}: (epoch_ns, measurement_id) ascending order within one tied epoch");

        for m in chunk {
            assert_eq!(m.epoch_ns, expected_epoch_ns, "k={k} id={}", m.measurement_id);
            assert!(m.frame_id.is_empty(), "k={k} id={}: neither fixture declares a frame_id", m.measurement_id);

            match m.measurement_id.as_str() {
                ID_ATTITUDE_Q4 => {
                    assert_eq!(m.sensor_id, "startracker", "k={k}");
                    assert_eq!(m.z.len(), 4, "k={k}: qx,qy,qz,qw");
                    assert!(m.r.is_empty(), "k={k}: star tracker never declares noise for its unit-quaternion measurement");
                }
                ID_IMU_GYRO3 => {
                    assert_eq!(m.sensor_id, "imu", "k={k}");
                    assert_eq!(m.z.len(), 3, "k={k}: wx,wy,wz");
                    let var = IMU_GYRO_NOISE_SIGMA * IMU_GYRO_NOISE_SIGMA;
                    assert_eq!(m.r, vec![var, 0.0, 0.0, 0.0, var, 0.0, 0.0, 0.0, var], "k={k}: r = diag(gyro_noise_sigma^2), bit-exact (no randomness in r itself)");
                }
                ID_IMU_ACCEL3 => {
                    assert_eq!(m.sensor_id, "imu", "k={k}");
                    assert_eq!(m.z.len(), 3, "k={k}: ax,ay,az");
                    let var = IMU_ACCEL_NOISE_SIGMA * IMU_ACCEL_NOISE_SIGMA;
                    assert_eq!(m.r, vec![var, 0.0, 0.0, 0.0, var, 0.0, 0.0, 0.0, var], "k={k}: r = diag(accel_noise_sigma^2), bit-exact");
                }
                other => panic!("unexpected measurement_id {other:?}"),
            }
            assert_z_consistent_with_truth(m, truth_sample, elapsed_s);
        }
    }
}

/// Question 176 (M25.3c) -- pinned by the lead: "decoding happens at the emitting sensor through
/// its own codec, so a packet the router later drops still yields a measurement ... that is
/// correct and is kept, with `Measurement.meta["decoded_at"]` naming the instance." This is the
/// required test for that decision, using this file's own real `demo_measurements` topology --
/// not a hand-rolled fixture: `drms/demo_measurements.sos.yaml`'s own 14 declared `Connection`s
/// (grepped, not guessed -- see that file itself) route only the seven `TRUTH_PORT_NAMES` signal
/// ports into `startracker`/`imu`; neither `startracker`'s own declared `st_meas` port nor `imu`'s
/// own declared `imu_meas` port (both `PORT_KIND_FRAMED`/`PORT_DIRECTION_OUT`,
/// `demo_measurements_{startracker,imu}.system.yaml`'s own `ports` list) is named as a `from_port`
/// by any `Connection` at all. So every one of the 36 measurement packets `StarTrackerModel`/
/// `ImuModel::step_with_ports` emit on those ports is dropped **immediately**, every single time,
/// by `crate::router::Router::deliver`'s own documented rule: "a message on a port with no
/// matching connection is dropped, not an error" (`crates/av-kernel/src/router.rs`, proven by that
/// module's own `a_message_on_a_port_with_no_matching_connection_is_dropped_not_an_error` test --
/// the declared mechanism this test reuses, not a new one). These drops never even reach the
/// pending queue (`Router::deliver`'s own `continue` on no match, before any `self.pending...push`),
/// so they are not merely late -- `products.dropped_in_flight_messages` staying `0` below proves
/// that, distinguishing "genuinely dropped, never routed" from "still in flight when the run
/// ended" (`crates/av-kernel/tests/dropped_messages.rs`'s own, different scenario).
///
/// **Hypothesis, stated before running:** despite every one of the 36 packets being dropped this
/// way, `products.measurements` is non-empty and exactly matches
/// [`demo_measurements_drm_decodes_real_measurements_through_execute`]'s own count (36) -- that
/// fact alone (measurements survive a total, guaranteed router drop) is this task's headline claim,
/// already implied by that other test passing at all over this same fixture. What this test adds:
/// every one of those 36 measurements carries `meta["decoded_at"]` equal to its own `sensor_id`
/// (`"startracker"` or `"imu"`) -- the *emitting* instance, per the lead's decision -- never empty,
/// and (since this topology declares no receiver for either measurement port at all) there is no
/// other instance id it could honestly be.
///
/// **Fails against:**
/// - an implementation that never stamps `decoded_at` at all (`meta` empty -- the state before
///   this task's `crate::schedule::HeteroScheduler::advance_to_with_ports` change);
/// - one that stamps some other value (e.g. the DRM/run id, or a hardcoded constant) instead of
///   the emitting instance.
#[test]
fn dropped_measurement_packets_still_decode_at_the_emitter_with_decoded_at_naming_it() {
    let _engine = gmat_sys::engine_lock();
    let (drm, sos, systems) = load_measurements_bundle();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let products = execute(run_config(&gmat, &drm, &sos, &systems)).expect("the measurements DRM executes end to end even though every measurement packet is dropped by the router");

    assert_eq!(products.measurements.len(), (N_EMISSIONS * 3) as usize, "measurements must survive the router dropping every single packet: {:#?}", products.measurements);
    assert_eq!(
        products.dropped_in_flight_messages, 0,
        "these 36 packets are dropped immediately at Router::deliver (no Connection at all names st_meas/imu_meas as a from_port), never queued as in-flight -- see this test's own doc comment for why that is the stronger, more relevant drop"
    );

    for m in &products.measurements {
        assert!(!m.sensor_id.is_empty(), "{m:?}");
        assert_eq!(m.meta.get("decoded_at"), Some(&m.sensor_id), "measurement {:?} at epoch_ns={}: decoded_at must name the emitting instance ({:?}), matching sensor_id exactly -- no receiver-side decode exists in this topology (or in this task's scope) to disagree with it", m.measurement_id, m.epoch_ns, m.sensor_id);
    }
}

// =============================================================================================
// Work item 3 (M25.3c): drop semantics. **Experiment first, established by actually running
// something, not by reading the code alone (per this task's own rule).**
//
// The current design decodes a `Measurement` at the emitting sensor instance's own
// `step_with_ports` call, from the exact `values` it is about to (or just did) encode into a
// FRAMED packet -- see `crate::codec::measurements_from_field_values`'s own call sites in
// `crate::drm::sensors::StarTrackerModel`/`ImuModel::step_with_ports`. That happens entirely
// inside one instance's own step, before `crate::schedule::HeteroScheduler::
// advance_to_with_ports` ever calls `router.deliver` on that step's `Outbox`. Two questions:
// (1) does a packet the router never actually delivers anywhere still produce a `Measurement`,
// and (2) does a *declared* PORT-targeted "drop" fault (`av_cdm::pb::FaultTargetKind::Port`,
// ADR-005 sec 5) currently have any effect on that at all?
//
// **Experiment 1 (already run above, real result, not asserted from reading the code):** this
// file's own fixture (`drms/demo_measurements.sos.yaml`) declares zero `Connection`s for either
// sensor's own FRAMED OUT port (`st_meas`/`imu_meas`) -- only the 14 truth SIGNAL connections.
// Per `crate::router`'s own module doc comment ("Delivery model"): "A message on a port with no
// matching connection is silently dropped (not wired anywhere)." So every single one of the 36
// packets `demo_measurements_drm_decodes_real_measurements_through_execute` above just proved
// real (`output.startracker.seq@end`/`output.imu.seq@end`-style FRAMED emission, `RunProducts.
// measurements` len 36) was, in the router's own terms, dropped -- unroutable from the moment it
// was pushed onto the `Outbox`. All 36 measurements still appeared. That is the experiment.
//
// **Experiment 2 (below): a *declared* PORT "drop" fault against the star tracker instance.**
// Through M25.3c, `crate::drm::fault`'s own module doc comment stated PORT/SENSOR faults were
// "validated, seeded, and explicitly refused" at the *unit* level but "a PORT/SENSOR fault
// declared in a real DRM today is still silently dropped before this module ever sees it" --
// `crate::drm::executor` only ever branched on `FaultTargetKind::Dynamics`/`::Hardware`, and
// [`a_declared_port_drop_fault_against_the_star_tracker_instance_has_no_effect_on_measurements_today`]
// (this test's own prior name) proved that silent no-op by actually running it.
//
// **M25.4a (`docs/open-questions.md` question 178) closes that gap with a typed load refusal,
// not a runtime.** [`a_declared_port_drop_fault_against_the_star_tracker_instance_is_a_typed_
// load_refusal`] below now proves the *new* real behaviour instead: `execute()` refuses this
// exact DRM, at load, before any binding or GMAT call, naming question 178, the fault's own id,
// its instance, and its target kind (`DrmError::PortOrSensorFaultNotYetSupported`) -- so a run
// that reaches this far never carries a PORT/SENSOR fault while producing measurements at all.

/// Re-hash a `DesignReferenceMission` after mutating it in memory -- mirrors `tests/drm_attitude.
/// rs::rehash`'s own doc comment (not a way to bypass `execute`'s tamper check; the opposite:
/// keeps a deliberately mutated in-memory fixture variant honestly self-consistent).
fn rehash_drm(mut drm: DesignReferenceMission) -> DesignReferenceMission {
    drm.hash = hash::canonical_drm_hash(&drm);
    drm
}

/// **Load refusal, pinned (M25.4a, `docs/open-questions.md` question 178).** A `Fault {
/// target_kind: FAULT_TARGET_KIND_PORT, kind: "drop", instance: "startracker", target: "st_meas"
/// }` declared in `scenario.faults` -- exactly the shape ADR-005 sec 5 documents for a PORT
/// fault's "drop" kind -- now makes `execute()` refuse the whole run, at load, before any
/// binding or GMAT call, rather than silently ignoring it (`a_declared_port_drop_fault_against_
/// the_star_tracker_instance_has_no_effect_on_measurements_today`, this test's own prior name and
/// prior assertion, pinned the old no-op; question 178 replaces that no-op with a typed refusal,
/// so this test now pins the refusal instead). The unfaulted baseline is still run first, and
/// still must succeed -- this test is about the declared PORT fault specifically, not about this
/// fixture being broken some other way.
///
/// **Fails against** an implementation that still lets this DRM execute (the M25.3c-era no-op
/// regressing back in), one that refuses it with the wrong error variant (in particular
/// `DrmError::FaultTargetKindNotSupported`, `fault::realize_unapplied_fault`'s own
/// *realization-time* result for a caller that actually invokes it -- see `DrmError::
/// PortOrSensorFaultNotYetSupported`'s own doc comment for why that is a different error, for a
/// different reason, raised at a different time), or one that names the wrong fault id/instance/
/// target_kind in the refusal.
#[test]
fn a_declared_port_drop_fault_against_the_star_tracker_instance_is_a_typed_load_refusal() {
    let _engine = gmat_sys::engine_lock();
    let (drm, sos, systems) = load_measurements_bundle();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");

    let baseline = execute(run_config(&gmat, &drm, &sos, &systems)).expect("baseline (no fault) run executes");
    assert!(!baseline.measurements.is_empty(), "sanity: the baseline fixture must actually produce measurements for this test to mean anything");

    let mut faulted = drm;
    {
        let scenario = faulted.scenario.as_mut().expect("this DRM declares a scenario");
        scenario.faults.push(Fault {
            id: "st_meas_drop".to_string(),
            tai_ns: START_TAI_NS,
            duration_ns: 0, // persistent (0 = "until cleared", proto doc comment)
            target_kind: FaultTargetKind::Port as i32,
            instance: "startracker".to_string(),
            target: "st_meas".to_string(),
            kind: "drop".to_string(),
            ..Default::default()
        });
    }
    let faulted = rehash_drm(faulted);
    let err = execute(run_config(&gmat, &faulted, &sos, &systems))
        .expect_err("a declared PORT fault must be a typed load refusal now (question 178), not a run that silently ignores it");
    assert!(
        matches!(&err, DrmError::PortOrSensorFaultNotYetSupported { fault_id, instance, target_kind } if fault_id == "st_meas_drop" && instance == "startracker" && target_kind == "FAULT_TARGET_KIND_PORT"),
        "expected DrmError::PortOrSensorFaultNotYetSupported naming fault_id=\"st_meas_drop\" instance=\"startracker\" target_kind=\"FAULT_TARGET_KIND_PORT\", got {err:?}"
    );
}
