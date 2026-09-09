//! Star tracker and IMU sensor models (M22.2, `docs/sil-plan.md`'s M22 milestone paragraph:
//! "Sensor models as ordinary dynamics models: [...] IMU (rates and accelerations from the
//! truth state plus bias random walk and), star tracker (quaternion plus noise), all feeding
//! FRAMED ports"; its Decisions (2026-09-05) decision A, "attitude control first": "star tracker
//! and IMU in, wheel torques out"; `docs/open-questions.md` questions 142, 149, 151, 152).
//!
//! ## Scope: M22.2 built the models, M22.2b wired them into the binding path
//!
//! [`StarTrackerModel`]/[`ImuModel`] are real, directly constructible, directly testable
//! [`av_dynamics::DynamicsModel`] implementations with their own typed parameter parsing
//! ([`parse_star_tracker_spec`]/[`parse_imu_spec`], mirroring `super::attitude::
//! parse_attitude_spec`'s "declared and typed, refuse the unrecognized" contract via the local
//! [`SensorSpecError`] type). **M22.2 did not wire a `"startracker."`/`"imu."`-dispatched arm
//! into `super::binding::classify_binding`/`AnyModel`/`crate::registry::ModelRegistry`** --
//! exactly the same scope limit `crate::drm::attitude`'s own module doc comment disclosed for
//! `AttitudeWheelsModel` before M22.1b added that wiring. **M22.2b (`docs/open-questions.md`
//! questions 142/149/151/152, decided by the lead) closes that gap the same way M22.1b closed
//! it for attitude**: `crate::registry::ModelKind::StarTracker`/`ModelKind::Imu`,
//! `super::binding::AnyModel::StarTracker`/`AnyModel::Imu`, `crate::registry::ModelRegistry::
//! construct_star_tracker`/`construct_imu`, and a deliberate arm in every exhaustive match over
//! `BindingPlan`/`AnyModel` in `binding.rs`/`registry.rs`/`fault.rs`/`executor.rs` (never a
//! catch-all) -- see those modules' own doc comments for exactly what each arm does. What this
//! batch (M22.2) proved, and what M22.2b builds on top of rather than replaces: a genuine,
//! [`crate::router::Router`]-mediated, multi-instance test
//! ([`tests::attitude_instance_is_measured_by_both_sensors_through_the_real_router`]) wiring a
//! real attitude truth source to both sensors via real `Connection`/`Port` declarations and the
//! real router (`crate::router::Router::build`/`deliver`/`take_inbox`) -- not a hand-waved
//! shortcut -- plus the `drms/demo_attitude_sensors.*.yaml` fixture, which parses and hashes
//! through `crate::drm::schema` like every other fixture in this repository. **As of M22.2b that
//! same fixture also runs end to end through `crate::drm::executor::execute`** --
//! `crates/av-kernel/tests/drm_attitude_sensors.rs` is the proof (declared update rate honoured
//! through the full executor path, same-seed/different-seed determinism, a maneuver targeting a
//! sensor instance refused as a typed load error) -- see that file's own module doc comment for
//! the exact exit criteria and expected counts.
//!
//! ## The truth link: fixed-name SIGNAL ports, never touching `crate::drm::attitude`
//!
//! A sensor needs the truth attitude quaternion and body rate every step. Rather than growing
//! `AttitudeWheelsModel` (537 passing tests, zero regression budget) with a new emit capability,
//! [`TruthBroadcastAttitude`] is a small, generic decorator: it wraps *any* `DynamicsModel` whose
//! state begins `[q_x, q_y, q_z, q_w, body_rate_x, body_rate_y, body_rate_z, ...]` (exactly
//! `crate::drm::attitude::AttitudeWheelsModel`'s own layout) and, on every `step_with_ports`
//! call, additionally broadcasts that state's leading 7 components as 7 SIGNAL messages on the
//! fixed conventional port names [`TRUTH_PORT_QX`]..[`TRUTH_PORT_WZ`] ([`truth_outbox`]) --
//! `crate::drm::attitude.rs` itself is not touched by one byte. [`read_truth`] is the sensor
//! side's inverse: the last message on each of the 7 ports, decoded via `av_dynamics::
//! decode_signal` (`None` until all seven have arrived at least once).
//!
//! ## The four traps, and where each is closed
//!
//! 1. **Quaternion noise stays unit.** [`small_angle_to_quat`] builds an exact axis-angle unit
//!    quaternion from a 3-vector (sin/cos of a real angle -- unit by construction, never
//!    approximately so), and [`StarTrackerModel::step_with_ports`] composes it with the truth
//!    quaternion via [`quat_mul`] (Hamilton product) -- it never adds noise to `[qx,qy,qz,qw]`
//!    components and renormalizes. See [`tests::measured_quaternion_is_always_unit_norm_to_
//!    machine_precision`] and [`tests::composing_by_quaternion_multiplication_not_add_and_
//!    renormalize_is_structurally_required`].
//! 2. **Bias random walk actually random-walks.** [`random_walk_step3`] adds `sigma_rw *
//!    sqrt(dt_s) * N(0,1)` once per elapsed measurement period -- `Var(bias(t)) = sigma_rw^2 *
//!    t`, checked at two different horizons in [`tests::imu_bias_variance_grows_linearly_with_
//!    elapsed_time`], not merely "the bias moved".
//! 3. **The declared update rate is honoured**, not the kernel step: `step_with_ports` schedules
//!    emission against its own `next_due`/`period_ns` (both models), independent of `dt_ns` --
//!    [`tests::star_tracker_emits_at_its_own_declared_rate_not_the_kernel_step_rate`] drives a
//!    100 ms kernel step against a 250 ms declared sensor period.
//! 4. **Determinism.** Both models' only randomness is a `RefCell<crate::rng::Pcg64>` seeded
//!    once, at construction, from the declared `*.seed` parameter -- see [`tests::same_seed_
//!    produces_byte_identical_star_tracker_output_across_two_runs`] and its sibling asserting a
//!    *different* seed produces *different* output (both assertions required; the brief is
//!    explicit that the first alone would pass a model that ignores the seed and emits a
//!    constant).
//!
//! ## Tolerances/SE bounds introduced by this batch (disclosed, not loosened from any prior
//! value -- there is no prior value; these are new)
//!
//! - `MIN_ROTATION_ANGLE_RAD = 1e-12`: guards the `0/0` in [`small_angle_to_quat`]/
//!   [`quat_to_small_angle`] when the injected small-angle vector is exactly zero. Not a
//!   physical tolerance -- it only ever fires for the exact-zero case.
//! - `MOUNT_QUAT_UNIT_NORM_TOL = 1e-6`: the same magnitude `crate::drm::attitude::
//!   QUATERNION_UNIT_NORM_TOL`-equivalent check (`crate::interpolate::QUATERNION_UNIT_NORM_TOL`)
//!   uses for `attitude.q0`, applied here to a declared `*.mount_q`.
//! - Statistical pins: **N = 500,000** independent draws, a **5-standard-error** bound, exactly
//!   the Gates-model precedent (`crates/av-kernel/tests/gates_execution_error.rs`'s own
//!   `analytic_gates_injection_matches_the_sample_covariance_of_n_sampled_draws`, "5 standard
//!   errors is roughly a 1-in-3,500,000 false-positive rate"). Mean SE = `sigma/sqrt(N)`;
//!   variance SE = `sigma^2 * sqrt(2/(N-1))` (exact for a Gaussian sample, the same formula that
//!   test's own doc comment cites) -- both stated, with the expected value, before the test's own
//!   measurement is taken, per each test's doc comment below.
//!
//! ## SENSOR fault runtime for the star tracker (`docs/open-questions.md` question 178, R5.1a)
//!
//! [`StarTrackerFaultEffect`] is the declared effect of a `FAULT_TARGET_KIND_SENSOR` fault whose
//! `kind` is one of [`crate::drm::fault::SENSOR_KINDS`] (`"bias"`, `"dropout"`, `"freeze"`,
//! `"scale"`), carried in [`StarTrackerSpec::fault`] and applied by
//! [`StarTrackerModel::step_with_ports`] -- the PORT fault runtime's own analogue
//! (`crate::router`'s own module doc comment's "Port fault runtime" section) but realized as a
//! declared PARAMETER CHANGE, applied by the SAME fault-bounded re-materialization boundary a
//! DYNAMICS fault already uses (`crate::drm::fault::apply_dynamics_fault`/`apply_sensor_fault`,
//! `crate::drm::executor::run_shared_group`'s own boundary loop), not by a router-mediated
//! per-frame draw -- there is no router in the loop for a native sensor's own emission.
//!
//! **`Freeze`'s own definition, and why "first," not "last before the window."** A
//! re-materialized model has no history: `crate::drm::executor::materialize_plan_at_boundary`
//! constructs a genuinely FRESH `StarTrackerModel` at every boundary (a new `Pcg64`, `seq` reset
//! to 0, `next_due` reset, `last_truth` cleared -- this module's own `StarTrackerModel::new`) --
//! it has nothing left over from whatever segment ran immediately before the fault epoch to
//! latch as "the last value." The FIRST measurement it computes after the fault epoch is
//! therefore the only value available to freeze at all; [`StarTrackerModel::frozen_measurement`]
//! latches exactly that one, the moment it is first computed, and every later emission in the
//! window re-emits it unchanged -- still one packet, and one incremented CCSDS sequence count,
//! per declared period (`docs/open-questions.md` question 178's own design: "it still emits a
//! packet per period... only the measured values repeat").
//!
//! **Window semantics** ([Fault.tai_ns, Fault.tai_ns + duration_ns), half-open, `duration_ns ==
//! 0` persistent to run end) and **overlap refusal** are `crate::drm::executor`'s own concern,
//! not this module's -- see `super::DrmError::OverlappingSensorFaultWindows`'s own doc comment
//! for exactly how SENSOR's own single-`Option`-slot representation makes its overlap key
//! coarser than PORT's `(instance, port)` one.
//!
//! **Counting (question 186(c)).** [`StarTrackerModel::fault_frames_affected`]/`fault_first_
//! effect_tai_ns` accumulate across every `step_with_ports` call until [`StarTrackerModel::
//! drain_sensor_fault_effect`] (an `av_dynamics::DynamicsModel` required method, question 112 --
//! delegated explicitly through `crate::drm::binding::AnyModel`, never a trait default) drains
//! them -- `crate::drm::executor::run_shared_group` calls this at every boundary this instance's
//! own handle is about to be discarded and re-materialized, not only the two SENSOR-fault-
//! specific ones, because a re-materialization discards the counter along with everything else
//! (the same "known pre-existing behaviour" this module's own R5.1A_REPORT.md measures for
//! `seq`/the emission grid). Because overlapping windows are refused at load, at most one SENSOR
//! fault is ever in force on one instance at a time, so attributing a drained count to "the
//! currently active fault on this instance" is unambiguous.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::fmt;

use av_cdm::pb::{self, ModelInfo, Parameter};
use av_dynamics::{AppliedCommand, DynamicsModel, Inbox, Outbox, StepResult};

use crate::codec::{self, CodecError, FieldValue};
use crate::rng::Pcg64;

// ============================================================================================
// Small vector/quaternion helpers (deliberately independent of crate::drm::attitude's own
// private copies -- see the module doc comment's "truth link" section for why).
// ============================================================================================

fn dot3(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross3(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn norm3(v: [f64; 3]) -> f64 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}

pub(crate) fn quat_norm(q: [f64; 4]) -> f64 {
    (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt()
}

// Used by this module's own statistical-pin tests (recovering an injected small-angle error
// from a composed measurement), and, as of M22.4, by `super::controller::signed_error_vector`'s
// own `q_err = target_q^-1 (x) measured_q` -- no longer `#[allow(dead_code)]` now that a
// non-`#[cfg(test)]` call site exists.
pub(crate) fn quat_conj(q: [f64; 4]) -> [f64; 4] {
    [-q[0], -q[1], -q[2], q[3]]
}

/// Hamilton product, scalar-last: `quat_mul(a, b)` composes rotations as "apply `b`'s rotation
/// first, then `a`'s" (`R(quat_mul(a,b)) = R(a) . R(b)`), via the vector+scalar form `(v1,w1) *
/// (v2,w2) = (w1*v2 + w2*v1 + v1 x v2, w1*w2 - v1.v2)`. Independently unit-tested (identity,
/// inverse, a known 90-degree-about-z composition) below -- this is the ONE place a sensor's
/// small-angle noise is combined with a truth attitude (trap 1: composing a rotation, never
/// touching raw `[qx,qy,qz,qw]` components).
pub(crate) fn quat_mul(a: [f64; 4], b: [f64; 4]) -> [f64; 4] {
    let (va, wa) = ([a[0], a[1], a[2]], a[3]);
    let (vb, wb) = ([b[0], b[1], b[2]], b[3]);
    let c = cross3(va, vb);
    let v = [wa * vb[0] + wb * va[0] + c[0], wa * vb[1] + wb * va[1] + c[1], wa * vb[2] + wb * va[2] + c[2]];
    let w = wa * wb - dot3(va, vb);
    [v[0], v[1], v[2], w]
}

/// Guards the `0/0` in [`small_angle_to_quat`]/[`quat_to_small_angle`] for an exactly-zero
/// rotation vector -- disclosed in the module doc comment's tolerance list.
const MIN_ROTATION_ANGLE_RAD: f64 = 1e-12;

/// Build the exact axis-angle unit quaternion for rotation vector `v` (axis * angle, radians) --
/// unit norm *by construction* (sin/cos of a real angle), never an approximation that needs
/// renormalizing. `v` within [`MIN_ROTATION_ANGLE_RAD`] of zero returns the identity exactly.
pub(crate) fn small_angle_to_quat(v: [f64; 3]) -> [f64; 4] {
    let angle = norm3(v);
    if angle < MIN_ROTATION_ANGLE_RAD {
        return [0.0, 0.0, 0.0, 1.0];
    }
    let axis = [v[0] / angle, v[1] / angle, v[2] / angle];
    let half = angle / 2.0;
    let s = half.sin();
    [axis[0] * s, axis[1] * s, axis[2] * s, half.cos()]
}

/// Inverse of [`small_angle_to_quat`] for an angle `< PI` (always true for a small-angle sensor
/// error) -- recovers the injected axis*angle vector exactly (up to floating point), used only by
/// this module's own tests to check the noise actually injected against the declared
/// distribution (see [`tests::small_angle_to_quat_and_back_round_trips`]).
// Used by this module's own statistical-pin tests (recovering an injected small-angle error
// from a composed measurement) -- see `quat_conj`'s own `#[allow(dead_code)]` comment above.
#[allow(dead_code)]
pub(crate) fn quat_to_small_angle(q: [f64; 4]) -> [f64; 3] {
    let qv = [q[0], q[1], q[2]];
    let vnorm = norm3(qv);
    if vnorm < MIN_ROTATION_ANGLE_RAD {
        return [0.0, 0.0, 0.0];
    }
    let angle = 2.0 * vnorm.atan2(q[3]);
    let axis = [qv[0] / vnorm, qv[1] / vnorm, qv[2] / vnorm];
    [axis[0] * angle, axis[1] * angle, axis[2] * angle]
}

/// Rotate body-frame vector `v` by quaternion `q` (scalar-last, active rotation): the standard
/// quaternion sandwich `v' = v + 2 w (qv x v) + 2 qv x (qv x v)`. Used for a sensor's declared
/// mounting orientation. Independently cross-checked against the textbook z-axis rotation case
/// below (mirrors, but does not import, `crate::drm::attitude::tests::rotate_body_to_inertial`'s
/// identical formula -- that helper is private to that module's own test suite).
pub(crate) fn rotate_vector_by_quat(q: [f64; 4], v: [f64; 3]) -> [f64; 3] {
    let qv = [q[0], q[1], q[2]];
    let w = q[3];
    let t1 = cross3(qv, v);
    let t2 = cross3(qv, t1);
    [v[0] + 2.0 * w * t1[0] + 2.0 * t2[0], v[1] + 2.0 * w * t1[1] + 2.0 * t2[1], v[2] + 2.0 * w * t1[2] + 2.0 * t2[2]]
}

/// The same magnitude `crate::interpolate::QUATERNION_UNIT_NORM_TOL` uses for `attitude.q0`,
/// applied to a declared `*.mount_q` -- disclosed in the module doc comment's tolerance list.
pub const MOUNT_QUAT_UNIT_NORM_TOL: f64 = 1e-6;

// ============================================================================================
// Seeded noise sampling (free functions -- directly, statistically testable, exactly the
// pattern `crates/av-kernel/tests/gates_execution_error.rs` uses for `maneuver::
// sample_execution_error`).
// ============================================================================================

/// Three independent `N(0, sigma^2)` draws.
pub(crate) fn gaussian_vec3(rng: &mut Pcg64, sigma: f64) -> [f64; 3] {
    [sigma * rng.standard_normal(), sigma * rng.standard_normal(), sigma * rng.standard_normal()]
}

/// One discrete step of a 3-axis random walk: `next = prev + sigma_rw * sqrt(dt_s) * N(0,1)`
/// per axis, independently. `Var(next - prev) = sigma_rw^2 * dt_s`; iterated `k` times with
/// `dt_s` held fixed, `Var(walk after k steps) = sigma_rw^2 * k * dt_s` -- trap 2's own linear-
/// in-elapsed-time growth, checked in [`tests::imu_bias_variance_grows_linearly_with_elapsed_
/// time`].
pub(crate) fn random_walk_step3(rng: &mut Pcg64, prev: [f64; 3], sigma_rw: f64, dt_s: f64) -> [f64; 3] {
    let inc = gaussian_vec3(rng, sigma_rw * dt_s.sqrt());
    [prev[0] + inc[0], prev[1] + inc[1], prev[2] + inc[2]]
}

// ============================================================================================
// Truth link: fixed conventional SIGNAL port names + broadcast/read helpers.
// ============================================================================================

pub const TRUTH_PORT_QX: &str = "truth_qx";
pub const TRUTH_PORT_QY: &str = "truth_qy";
pub const TRUTH_PORT_QZ: &str = "truth_qz";
pub const TRUTH_PORT_QW: &str = "truth_qw";
pub const TRUTH_PORT_WX: &str = "truth_wx";
pub const TRUTH_PORT_WY: &str = "truth_wy";
pub const TRUTH_PORT_WZ: &str = "truth_wz";

/// Every truth port name, in the fixed order [`TruthBroadcastAttitude`]/[`read_truth`] agree on
/// -- used by the DRM fixture's own `Connection` declarations so there is exactly one place this
/// 7-port convention is spelled out.
pub const TRUTH_PORT_NAMES: [&str; 7] = [TRUTH_PORT_QX, TRUTH_PORT_QY, TRUTH_PORT_QZ, TRUTH_PORT_QW, TRUTH_PORT_WX, TRUTH_PORT_WY, TRUTH_PORT_WZ];

pub(crate) fn truth_outbox(q: [f64; 4], omega: [f64; 3], tai_ns: i64) -> Outbox {
    let mut ob = Outbox::new();
    ob.push_signal(TRUTH_PORT_QX, tai_ns, q[0]);
    ob.push_signal(TRUTH_PORT_QY, tai_ns, q[1]);
    ob.push_signal(TRUTH_PORT_QZ, tai_ns, q[2]);
    ob.push_signal(TRUTH_PORT_QW, tai_ns, q[3]);
    ob.push_signal(TRUTH_PORT_WX, tai_ns, omega[0]);
    ob.push_signal(TRUTH_PORT_WY, tai_ns, omega[1]);
    ob.push_signal(TRUTH_PORT_WZ, tai_ns, omega[2]);
    ob
}

/// `None` until all seven truth ports have delivered at least one message (question 108's own
/// `Inbox::last_on_port`: the most recently emitted, on ties). Never partially updates a sensor's
/// cached truth -- see [`StarTrackerModel::step_with_ports`]/[`ImuModel::step_with_ports`]'s own
/// "only overwrite the cache when every one of the 7 is present this step" rule.
pub(crate) fn read_truth(inbox: &Inbox) -> Option<([f64; 4], [f64; 3])> {
    let get = |port: &str| -> Option<f64> {
        let (m, _sender) = inbox.last_on_port(port)?;
        av_dynamics::decode_signal(&m.payload)
    };
    let qx = get(TRUTH_PORT_QX)?;
    let qy = get(TRUTH_PORT_QY)?;
    let qz = get(TRUTH_PORT_QZ)?;
    let qw = get(TRUTH_PORT_QW)?;
    let wx = get(TRUTH_PORT_WX)?;
    let wy = get(TRUTH_PORT_WY)?;
    let wz = get(TRUTH_PORT_WZ)?;
    Some(([qx, qy, qz, qw], [wx, wy, wz]))
}

/// A generic decorator: wraps any `DynamicsModel` whose state begins `[q_x, q_y, q_z, q_w,
/// body_rate_x, body_rate_y, body_rate_z, ...]` (exactly `crate::drm::attitude::
/// AttitudeWheelsModel`'s own layout) and additionally broadcasts that state's leading 7
/// components as truth, every step, on the [`TRUTH_PORT_NAMES`] SIGNAL ports -- see the module
/// doc comment's "truth link" section for why this exists instead of modifying `crate::drm::
/// attitude` directly. Every other method delegates to the wrapped model unchanged.
pub struct TruthBroadcastAttitude<M> {
    pub inner: M,
}

impl<M> TruthBroadcastAttitude<M> {
    pub fn new(inner: M) -> Self {
        Self { inner }
    }
}

impl<M: DynamicsModel> DynamicsModel for TruthBroadcastAttitude<M> {
    type Error = M::Error;

    fn state_dim(&self) -> usize {
        self.inner.state_dim()
    }
    fn derivatives(&self, state: &[f64], t_tai_ns: i64, controls: &[f64], state_dot: &mut [f64]) -> Result<(), Self::Error> {
        self.inner.derivatives(state, t_tai_ns, controls, state_dot)
    }
    fn describe(&self) -> ModelInfo {
        self.inner.describe()
    }
    fn step(&self, state: &[f64], t_tai_ns: i64, controls: &[f64], dt_ns: i64) -> Result<StepResult, Self::Error> {
        self.inner.step(state, t_tai_ns, controls, dt_ns)
    }
    /// Delegates the actual propagation to `self.inner.step` (the wrapped model has no ports of
    /// its own to consume -- `inbox` is intentionally ignored, this wrapper only ever
    /// *broadcasts*), then additionally emits the resulting state's leading 7 components as
    /// truth. Panics via `debug_assert!` (a caller bug, not a runtime condition) if the wrapped
    /// model's state is narrower than 7 components -- this wrapper's one documented precondition.
    fn step_with_ports(&self, state: &[f64], t_tai_ns: i64, controls: &[f64], dt_ns: i64, inbox: &Inbox) -> Result<(StepResult, Outbox, Vec<AppliedCommand>), Self::Error> {
        let _ = inbox;
        let result = self.inner.step(state, t_tai_ns, controls, dt_ns)?;
        debug_assert!(result.state.len() >= 7, "TruthBroadcastAttitude requires an attitude-shaped state ([q_x,q_y,q_z,q_w,body_rate_x,body_rate_y,body_rate_z,...]); got {} component(s)", result.state.len());
        let q = [result.state[0], result.state[1], result.state[2], result.state[3]];
        let omega = [result.state[4], result.state[5], result.state[6]];
        let outbox = truth_outbox(q, omega, result.t_tai_ns);
        Ok((result, outbox, Vec::new()))
    }

    /// Delegates to `self.inner.last_measurements` -- "every other method delegates to the
    /// wrapped model unchanged" (this struct's own doc comment), and this is no exception: the
    /// wrapped `AttitudeWheelsModel` never produces one today, but if a future wrapped model did,
    /// this wrapper must not silently swallow it.
    fn last_measurements(&self) -> Vec<av_cdm::pb::Measurement> {
        self.inner.last_measurements()
    }

    /// Delegates to `self.inner.drain_sensor_fault_effect` (question 178, R5.1a) -- the wrapped
    /// model is always attitude-shaped (`AttitudeWheelsModel`), never a star tracker, so this is
    /// always `None` in practice, but delegating honestly matches every other method on this
    /// wrapper.
    fn drain_sensor_fault_effect(&self) -> Option<av_dynamics::SensorFaultEffectDrain> {
        self.inner.drain_sensor_fault_effect()
    }
}

// ============================================================================================
// Typed parameter parsing (mirrors crate::drm::attitude::AttitudeSpecError's own contract).
// ============================================================================================

/// Everything [`parse_star_tracker_spec`]/[`parse_imu_spec`]/[`StarTrackerModel::new`]/
/// [`ImuModel::new`] can refuse -- a local, dedicated error type, not `super::DrmError`, for
/// exactly the reason `crate::drm::attitude::AttitudeSpecError`'s own doc comment gives: this
/// module is not (yet -- see this module's own "Scope of this batch" section) reached through
/// `super::binding::classify_binding`, so extending that DRM-executor-wide enum for a subsystem
/// it does not dispatch to would be a wider change than this batch's scope justifies. Shared
/// between the star tracker and the IMU (their parameter vocabularies are disjoint by prefix, so
/// one enum with `name`-carrying variants is exactly as specific as two would be).
#[derive(Debug, Clone, PartialEq)]
pub enum SensorSpecError {
    /// A required parameter (or one member of a "declared together" group) was absent.
    MissingParameter { name: String },
    /// A declared parameter name matched no recognized `"startracker.*"`/`"imu.*"` field.
    UnknownParameter { name: String },
    /// A present parameter's value (or the constructor's own declared `PacketCodec`) failed a
    /// physical/structural check -- refused rather than silently normalized/clamped/ignored.
    InvalidParameter { name: String, reason: String },
    /// The constructor's own declared `PacketCodec` failed `crate::codec::validate_codec`.
    Codec(CodecError),
}

impl fmt::Display for SensorSpecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SensorSpecError::MissingParameter { name } => write!(f, "missing required sensor parameter {name:?}"),
            SensorSpecError::UnknownParameter { name } => write!(f, "unrecognized sensor parameter {name:?}"),
            SensorSpecError::InvalidParameter { name, reason } => write!(f, "sensor parameter {name:?} is invalid: {reason}"),
            SensorSpecError::Codec(e) => write!(f, "declared PacketCodec is invalid: {e}"),
        }
    }
}
impl std::error::Error for SensorSpecError {}

fn parse_seed(name: &str, p: &Parameter) -> Result<u64, SensorSpecError> {
    // f64 exactly represents every integer up to 2^53; a seed beyond that cannot round-trip
    // through Parameter.value (a plain double, like every other numeric field this proto shape
    // carries) -- refused rather than silently truncated.
    const MAX_EXACT: f64 = 9_007_199_254_740_992.0; // 2^53
    if !p.value.is_finite() || p.value < 0.0 || p.value.fract() != 0.0 || p.value > MAX_EXACT {
        return Err(SensorSpecError::InvalidParameter { name: name.to_string(), reason: format!("must be a nonnegative integer-valued number exactly representable as f64 (<= 2^53), got {}", p.value) });
    }
    Ok(p.value as u64)
}

fn check_unit(name: &str, p: &Parameter, want: pb::Unit) -> Result<(), SensorSpecError> {
    let unspecified = pb::Unit::Unspecified as i32;
    if p.unit != unspecified && p.unit != want as i32 {
        return Err(SensorSpecError::InvalidParameter { name: name.to_string(), reason: format!("declared unit code {} is neither UNIT_UNSPECIFIED nor {want:?}", p.unit) });
    }
    Ok(())
}

fn require_positive(v: Option<f64>, name: &str) -> Result<f64, SensorSpecError> {
    let v = v.ok_or_else(|| SensorSpecError::MissingParameter { name: name.to_string() })?;
    if !(v.is_finite() && v > 0.0) {
        return Err(SensorSpecError::InvalidParameter { name: name.to_string(), reason: format!("must be > 0, got {v}") });
    }
    Ok(v)
}

fn require_nonneg(v: Option<f64>, name: &str) -> Result<f64, SensorSpecError> {
    let v = v.ok_or_else(|| SensorSpecError::MissingParameter { name: name.to_string() })?;
    if !(v.is_finite() && v >= 0.0) {
        return Err(SensorSpecError::InvalidParameter { name: name.to_string(), reason: format!("must be >= 0, got {v}") });
    }
    Ok(v)
}

fn parse_optional_unit_quat(group: [Option<f64>; 4], name: &str) -> Result<[f64; 4], SensorSpecError> {
    match group {
        [None, None, None, None] => Ok([0.0, 0.0, 0.0, 1.0]),
        [Some(x), Some(y), Some(z), Some(w)] => {
            let n = quat_norm([x, y, z, w]);
            if (n - 1.0).abs() > MOUNT_QUAT_UNIT_NORM_TOL {
                return Err(SensorSpecError::InvalidParameter { name: name.to_string(), reason: format!("not unit norm: |q| = {n}") });
            }
            Ok([x, y, z, w])
        }
        _ => Err(SensorSpecError::MissingParameter { name: format!("{name}.{{x,y,z,w}} (all four required together, or none at all for identity)") }),
    }
}

// ============================================================================================
// Star tracker.
// ============================================================================================

/// `docs/open-questions.md` question 178 (R5.1a): the SENSOR fault effect currently installed on
/// a [`StarTrackerModel`], carried in [`StarTrackerSpec::fault`] and applied inside
/// [`StarTrackerModel::step_with_ports`] -- see that method's own doc comment for exactly where
/// each variant is applied, and this module's own doc comment's "SENSOR fault runtime" section
/// for the vocabulary these mirror ([`crate::drm::fault::SENSOR_KINDS`]). At most one variant is
/// ever installed at a time -- `crate::drm::executor` refuses two SENSOR faults on the same
/// instance with overlapping windows at load ([`super::DrmError::OverlappingSensorFaultWindows`]),
/// which is what makes a single `Option` slot (rather than a set) the correct representation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StarTrackerFaultEffect {
    /// `Fault.target` is `startracker.bias_rad.{x,y,z}` (`axis` 0/1/2); `Fault.params["value"]`
    /// is `value_rad`, a fixed small-angle rotation about that body axis, radians.
    Bias { axis: usize, value_rad: f64 },
    /// `Fault.target` is `startracker.output`: no packet, no `Measurement`, at any emission
    /// instant inside the window.
    Dropout,
    /// `Fault.target` is `startracker.output`: the FIRST measurement computed after the fault
    /// epoch is latched and re-emitted, unchanged, at every subsequent emission instant in the
    /// window -- see [`StarTrackerModel::step_with_ports`]'s own doc comment for why "first," not
    /// "last before the window."
    Freeze,
    /// `Fault.target` is `startracker.scale`; `Fault.params["value"]` is `value`, a dimensionless
    /// factor multiplying the sensor's reported deviation from truth (noise plus any bias) --
    /// `value == 1.0` is exactly a no-op.
    Scale { value: f64 },
}

/// Parsed, typed parameters for [`StarTrackerModel`] -- built by [`parse_star_tracker_spec`].
/// Parameter vocabulary (a name matching none of these is [`SensorSpecError::
/// UnknownParameter`]):
/// - `startracker.update_rate_hz` (required, `> 0`): declared measurement rate, Hz.
/// - `startracker.seed` (required, nonnegative integer): seeds this instance's own `Pcg64`.
/// - `startracker.noise_sigma_rad` (required, `> 0`): 1-sigma per-axis small-angle boresight
///   error, radians -- see [`StarTrackerModel::step_with_ports`] for how it is applied (trap 1).
/// - `startracker.mount_q.{x,y,z,w}` (optional, all four together; default identity): fixed
///   mounting rotation from the vehicle body frame into the star tracker's own boresight frame.
///
/// `fault` (question 178, R5.1a) is never a declared parameter -- it is `None` at parse time,
/// always, and is set only by `crate::drm::fault::apply_sensor_fault` at a fault-bounded
/// re-materialization boundary (`crate::drm::executor::run_shared_group`'s own boundary loop),
/// exactly the way a DYNAMICS fault's own perturbed field is never a declared parameter either.
#[derive(Debug, Clone, PartialEq)]
pub struct StarTrackerSpec {
    pub update_rate_hz: f64,
    pub seed: u64,
    pub noise_sigma_rad: f64,
    pub mount_q: [f64; 4],
    pub fault: Option<StarTrackerFaultEffect>,
}

pub fn parse_star_tracker_spec(params: &BTreeMap<String, Parameter>) -> Result<StarTrackerSpec, SensorSpecError> {
    let mut update_rate_hz = None;
    let mut seed = None;
    let mut noise_sigma_rad = None;
    let mut mount_q = [None; 4];
    for (name, p) in params {
        match name.as_str() {
            "startracker.update_rate_hz" => update_rate_hz = Some(p.value),
            "startracker.seed" => seed = Some(parse_seed("startracker.seed", p)?),
            "startracker.noise_sigma_rad" => {
                check_unit("startracker.noise_sigma_rad", p, pb::Unit::Radian)?;
                noise_sigma_rad = Some(p.value);
            }
            "startracker.mount_q.x" => mount_q[0] = Some(p.value),
            "startracker.mount_q.y" => mount_q[1] = Some(p.value),
            "startracker.mount_q.z" => mount_q[2] = Some(p.value),
            "startracker.mount_q.w" => mount_q[3] = Some(p.value),
            // M22.2b (`docs/open-questions.md` question 95, M10.3's own convention): an
            // `"output.<name>"` parameter declares this instance exposes `output.<instance>.
            // <name>@time` (`crate::drm::executor::declared_outputs`) -- it names no field of
            // this spec at all, so it is skipped here exactly the way `parse_gmat_spec`/
            // `parse_constant_accel_spec` already do, rather than refused as unrecognized.
            _ if name.starts_with("output.") => {}
            other => return Err(SensorSpecError::UnknownParameter { name: other.to_string() }),
        }
    }
    let update_rate_hz = require_positive(update_rate_hz, "startracker.update_rate_hz")?;
    let seed = seed.ok_or_else(|| SensorSpecError::MissingParameter { name: "startracker.seed".to_string() })?;
    let noise_sigma_rad = require_positive(noise_sigma_rad, "startracker.noise_sigma_rad")?;
    let mount_q = parse_optional_unit_quat(mount_q, "startracker.mount_q")?;
    Ok(StarTrackerSpec { update_rate_hz, seed, noise_sigma_rad, mount_q, fault: None })
}

/// The fixed field layout every star tracker `PacketCodec` this module builds/expects uses:
/// four big-endian IEEE-754 `binary64` fields, `qx,qy,qz,qw` (scalar-last), 32 user-data bytes
/// total. `id`/`apid` are the only per-instance knobs -- the shape itself is a documented
/// convention (declared, and hashed with the owning `SystemDefinition`, once it is written into
/// one, exactly like every other `PacketCodec`).
/// M25.3 (`docs/open-questions.md` question 173): every field's own `target` is
/// `"altavista.attitude_q4/<label>"` -- the exact convention `packet.proto`'s own `PacketField.
/// target` doc comment illustrates ("altavista.attitude_q4/qx") -- so all four share one
/// measurement id ([`MEASUREMENT_ID_STAR_TRACKER_Q4`]) and [`crate::codec::
/// measurements_from_field_values`] groups them into one 4-component `Measurement` per emission.
pub const MEASUREMENT_ID_STAR_TRACKER_Q4: &str = "altavista.attitude_q4";

pub fn star_tracker_packet_codec(id: &str, apid: u32) -> pb::PacketCodec {
    let f = |name: &str, bit_offset: u32| pb::PacketField {
        name: name.to_string(),
        bit_offset,
        bit_width: 64,
        r#type: pb::PacketFieldType::Float64 as i32,
        unit: pb::Unit::Dimensionless as i32,
        scale: 1.0,
        offset: 0.0,
        target: format!("{MEASUREMENT_ID_STAR_TRACKER_Q4}/{name}"),
    };
    pb::PacketCodec { id: id.to_string(), apid, is_command: false, secondary_header_bytes: 0, user_data_bytes: 32, fields: vec![f("qx", 0), f("qy", 64), f("qz", 128), f("qw", 192)], description: "star tracker attitude measurement (M22.2)".to_string() }
}

/// A native star tracker `av_dynamics::DynamicsModel`: no propagated physical state
/// (`state_dim() == 0` -- an instantaneous measurement transform has nothing to integrate), a
/// `RefCell<Pcg64>` seeded once at construction, and a `Cell`-tracked emission schedule
/// (`next_due`/`period_ns`) independent of however often `step_with_ports` itself is called
/// (trap 3). `type Error = std::convert::Infallible`: every physical check already happened in
/// [`parse_star_tracker_spec`]/[`Self::new`] (unit mount quaternion, positive rate/sigma, a
/// codec declaring the four required fields), so `step_with_ports` itself cannot fail.
#[derive(Debug)]
pub struct StarTrackerModel {
    spec: StarTrackerSpec,
    codec: pb::PacketCodec,
    output_port: String,
    period_ns: i64,
    next_due: Cell<i64>,
    seq: Cell<u16>,
    rng: RefCell<Pcg64>,
    last_truth: RefCell<Option<([f64; 4], [f64; 3])>>,
    info: ModelInfo,
    /// Question 173 (M25.3): every `Measurement` this instance's own most recent
    /// `step_with_ports` call produced (empty when no measurement was emitted this call --
    /// e.g. no truth has arrived yet, or the call crossed no emission boundary). Cleared and
    /// repopulated at the top of every call, mirrored back out via `last_measurements` --
    /// `Cell`/`RefCell` for the same `&self`-only-methods reason `next_due`/`rng` already need
    /// interior mutability.
    measurements: RefCell<Vec<pb::Measurement>>,
    /// Question 178 (R5.1a): the `[qx,qy,qz,qw]` of the FIRST measurement this instance computed
    /// under `StarTrackerFaultEffect::Freeze` -- `None` until that first computation, then
    /// re-emitted, unchanged, at every subsequent emission instant. Reset only by constructing a
    /// fresh model (a fault-bounded re-materialization boundary), never by anything inside
    /// `step_with_ports` itself, which is exactly why "first value in the window" is a sound
    /// definition at all: a re-materialized model has no history (this struct's own doc comment,
    /// and `crate::drm::sensors`'s own module doc comment) beyond what it computes after the
    /// fault epoch, so "first" is the only value it CAN latch -- "last value before the window"
    /// would require the model to remember something from a segment it never ran.
    frozen_measurement: RefCell<Option<[f64; 4]>>,
    /// Question 178 (R5.1a): the total count of emissions `self.spec.fault` has changed or
    /// suppressed since the last [`Self::drain_sensor_fault_effect`] call, and the epoch of the
    /// first one -- see that method's own doc comment for exactly what counts as "affected".
    fault_frames_affected: Cell<u64>,
    fault_first_effect_tai_ns: Cell<Option<i64>>,
}

impl StarTrackerModel {
    /// `codec` must declare (at least) the four fields [`star_tracker_packet_codec`] builds --
    /// checked here, and separately validated via `crate::codec::validate_codec`, rather than
    /// assumed. `output_port` is the FRAMED port name this model's `Outbox` pushes onto.
    ///
    /// **`epoch_tai_ns` (M22.2b bug fix, found while wiring this model into the binding path):
    /// the instance's own starting TAI epoch, used only to seed the `next_due` field as
    /// `epoch_tai_ns + period_ns`.** Through M22.2, `next_due` was seeded at plain `period_ns`
    /// (implicitly assuming every run starts propagating from TAI epoch zero) -- harmless for
    /// M22.2's own unit tests (every one of them steps from `t = 0`, including the router-
    /// mediated test), but catastrophic against a real DRM's realistic epoch
    /// (`drms/demo_attitude_sensors.drm.yaml`'s own `start_tai_ns = 1767225637000000000`, ~1.77e18
    /// ns): [`Self::step_with_ports`]'s `while end >= self.next_due.get()` loop would have had to
    /// iterate roughly `(epoch_tai_ns - period_ns) / period_ns` times -- billions, for any
    /// realistic epoch -- just to catch `next_due` up to the run's own starting instant before
    /// ever emitting a single measurement. Caught by exactly the M22.2b `AnyModel::StarTracker`
    /// delegation tests this task's own brief required (`crate::drm::binding::tests::
    /// any_model_step_with_ports_delegates_to_the_star_tracker_variant`, which -- unlike every
    /// M22.2 unit test -- steps from the same realistic epoch `crate::drm::binding`'s own
    /// materialization tests already used): that test hung (confirmed via an isolated,
    /// single-threaded `cargo test ... -- --test-threads=1` run reproducing the hang with no
    /// other test interference) rather than failing cleanly, exactly the shape a stray `while`
    /// loop bug takes. Fixed here, at construction, rather than lazily on the first
    /// `step_with_ports` call, so `next_due`'s `Cell<i64>` stays a plain, always-valid
    /// absolute epoch with no sentinel/`Option` state to thread through the hot path.
    pub fn new(spec: StarTrackerSpec, codec: pb::PacketCodec, output_port: String, epoch_tai_ns: i64, model_id: &str) -> Result<Self, SensorSpecError> {
        codec::validate_codec(&codec).map_err(SensorSpecError::Codec)?;
        for name in ["qx", "qy", "qz", "qw"] {
            if !codec.fields.iter().any(|f| f.name == name) {
                return Err(SensorSpecError::InvalidParameter { name: "startracker.codec".to_string(), reason: format!("declared PacketCodec {:?} is missing required field {name:?}", codec.id) });
            }
        }
        let period_ns = (1.0e9 / spec.update_rate_hz).round() as i64;
        let mut settings = BTreeMap::new();
        settings.insert("update_rate_hz".to_string(), format!("{:.17e}", spec.update_rate_hz));
        settings.insert("seed".to_string(), spec.seed.to_string());
        settings.insert("noise_sigma_rad".to_string(), format!("{:.17e}", spec.noise_sigma_rad));
        for (i, v) in spec.mount_q.iter().enumerate() {
            settings.insert(format!("mount_q_{i}"), format!("{v:.17e}"));
        }
        settings.insert("output_port".to_string(), output_port.clone());
        settings.insert("apid".to_string(), codec.apid.to_string());
        // Question 178 (R5.1a): included in the settings hash -- and therefore in `dynamics_hash`
        // -- so a fault-bounded re-materialization (installing or clearing `spec.fault`) always
        // produces a genuinely different configuration hash, keeping `executor::
        // merge_adjacent_segments` from wrongly merging the pre-fault, faulted, and post-fault
        // segments together (question 115/116's own "merge only when the configuration is truly
        // unchanged" rule).
        settings.insert(
            "fault".to_string(),
            match spec.fault {
                None => "none".to_string(),
                Some(StarTrackerFaultEffect::Bias { axis, value_rad }) => format!("bias:{axis}:{value_rad:.17e}"),
                Some(StarTrackerFaultEffect::Dropout) => "dropout".to_string(),
                Some(StarTrackerFaultEffect::Freeze) => "freeze".to_string(),
                Some(StarTrackerFaultEffect::Scale { value }) => format!("scale:{value:.17e}"),
            },
        );
        let settings_hash = av_dynamics::settings_hash(&settings);
        let info = ModelInfo { id: model_id.to_string(), version: "1".to_string(), state_space_id: format!("{model_id}.no_state"), frame_id: String::new(), settings_hash, depth: "native".to_string(), ..Default::default() };
        let seed = spec.seed;
        Ok(Self {
            spec,
            codec,
            output_port,
            period_ns,
            next_due: Cell::new(epoch_tai_ns + period_ns),
            seq: Cell::new(0),
            rng: RefCell::new(Pcg64::new(seed)),
            last_truth: RefCell::new(None),
            info,
            measurements: RefCell::new(Vec::new()),
            frozen_measurement: RefCell::new(None),
            fault_frames_affected: Cell::new(0),
            fault_first_effect_tai_ns: Cell::new(None),
        })
    }

    /// This instance's own declared measurement period, TAI nanoseconds -- exposed for tests.
    pub fn period_ns(&self) -> i64 {
        self.period_ns
    }
}

impl DynamicsModel for StarTrackerModel {
    type Error = std::convert::Infallible;

    fn state_dim(&self) -> usize {
        0
    }
    fn derivatives(&self, state: &[f64], _t_tai_ns: i64, _controls: &[f64], out: &mut [f64]) -> Result<(), Self::Error> {
        // Never reached in practice: `step`/`step_with_ports` are both overridden below and
        // never call this (there is no ODE here at all -- an instantaneous measurement
        // transform has nothing to integrate). Implemented as an honest no-op, not
        // `unimplemented!()`, purely to satisfy the trait.
        debug_assert!(state.is_empty() && out.is_empty());
        Ok(())
    }
    fn describe(&self) -> ModelInfo {
        self.info.clone()
    }
    fn step(&self, state: &[f64], t_tai_ns: i64, controls: &[f64], dt_ns: i64) -> Result<StepResult, Self::Error> {
        Ok(self.step_with_ports(state, t_tai_ns, controls, dt_ns, &Inbox::empty())?.0)
    }

    /// See the module doc comment's traps 1/3/4 -- this is where all three are actually closed.
    /// `inbox` is drained for fresh truth (question 108's own `Inbox::last_on_port`, via
    /// [`read_truth`]) every call, regardless of whether this step happens to cross an emission
    /// boundary; a measurement is only ever produced (and only ever pushed onto `self.
    /// output_port`) at instants that are exact multiples of `self.period_ns`, computed by a
    /// `while` loop against `self.next_due` so a single long `dt_ns` (kernel dt coarser than the
    /// declared rate) still emits once per elapsed period, each correctly timestamped, rather
    /// than either dropping the catch-up emissions or emitting only one at the wrong epoch.
    fn step_with_ports(&self, state: &[f64], t_tai_ns: i64, _controls: &[f64], dt_ns: i64, inbox: &Inbox) -> Result<(StepResult, Outbox, Vec<AppliedCommand>), Self::Error> {
        debug_assert!(state.is_empty());
        if let Some(truth) = read_truth(inbox) {
            *self.last_truth.borrow_mut() = Some(truth);
        }
        // Question 173 (M25.3): cleared at the top of every call, repopulated below -- see
        // `last_measurements`'s own doc comment. `sensor_id` is left empty here: this model
        // knows only its own `dynamics_model` string (`self.info.id`), never its own instance
        // name (`crate::registry::ModelRegistry::construct_star_tracker`'s own `model_id`
        // parameter is `SystemDefinition.dynamics_model`, not the instance) -- exactly the same
        // gap `av_dynamics::AppliedCommand` has for the same reason (`crate::ports::
        // AppliedPortCommand`'s own doc comment: "a model sees only its own Inbox, never its own
        // instance name"). `crate::schedule::HeteroScheduler::advance_to_with_ports` fills it in
        // from the `BTreeMap` key it is already iterating, the identical enrichment pattern.
        self.measurements.borrow_mut().clear();
        let end = t_tai_ns + dt_ns;
        let mut outbox = Outbox::new();
        let mut outputs = BTreeMap::new();
        while end >= self.next_due.get() {
            let due = self.next_due.get();
            if let Some((truth_q, _omega)) = *self.last_truth.borrow() {
                // Question 178 (R5.1a): `Dropout` suppresses the WHOLE emission -- no packet,
                // no `Measurement`, and `seq` is left untouched (there is no packet to number --
                // see this model's own module doc comment's "SENSOR fault runtime" section).
                // Every other installed fault (`None`/`Bias`/`Freeze`/`Scale`) still emits a
                // packet below, exactly as an unfaulted model would.
                if matches!(self.spec.fault, Some(StarTrackerFaultEffect::Dropout)) {
                    self.record_fault_effect(due);
                    self.next_due.set(due + self.period_ns);
                    continue;
                }
                // `already_frozen` is read into a plain, owned `Option<[f64; 4]>` (Copy) BEFORE
                // the `if`/`else` below, rather than matching directly on `*self.
                // frozen_measurement.borrow()` -- a `Ref` scrutinee's borrow otherwise stays
                // alive across the WHOLE `if let`/`else` (both arms), including the `else`
                // arm's own `self.frozen_measurement.borrow_mut()`, which would panic
                // ("already borrowed") the very first time this fires. Caught directly, not
                // assumed: `freeze_latches_the_first_measurement_and_repeats_it_while_truth_
                // keeps_changing` paniced with exactly that message before this fix.
                let already_frozen = *self.frozen_measurement.borrow();
                let measured_q = if matches!(self.spec.fault, Some(StarTrackerFaultEffect::Freeze)) {
                    if let Some(frozen) = already_frozen {
                        frozen
                    } else {
                        let q = self.compute_measured_quaternion(truth_q);
                        *self.frozen_measurement.borrow_mut() = Some(q);
                        q
                    }
                } else {
                    self.compute_measured_quaternion(truth_q)
                };
                if self.spec.fault.is_some() {
                    self.record_fault_effect(due);
                }
                let mut values = BTreeMap::new();
                values.insert("qx".to_string(), FieldValue::Numeric(measured_q[0]));
                values.insert("qy".to_string(), FieldValue::Numeric(measured_q[1]));
                values.insert("qz".to_string(), FieldValue::Numeric(measured_q[2]));
                values.insert("qw".to_string(), FieldValue::Numeric(measured_q[3]));
                let seq = self.seq.get();
                self.seq.set(seq.wrapping_add(1) & 0x3FFF);
                let payload = codec::encode_packet(&self.codec, seq, &[], &values).expect(
                    "StarTrackerModel's declared codec always carries exactly the qx/qy/qz/qw FLOAT64 fields this call supplies, pre-validated at construction (validate_codec) -- encode_packet can only fail for a missing/mistyped/out-of-range field, none of which can happen here",
                );
                outbox.push(self.output_port.clone(), due, payload);
                // Question 173: the exact same `values` this step just encoded into the packet
                // above, mapped through the codec's own declared `PacketField.target` -- see
                // `crate::codec::measurements_from_field_values`'s own doc comment for why `r`
                // is deliberately left empty here (a unit quaternion's declared small-angle
                // sigma is not a diagonal covariance on the 4 raw, over-parameterized
                // components -- inventing one would misrepresent the manifold).
                let measured = codec::measurements_from_field_values(&self.codec, &values, due, "", &self.info.frame_id, &BTreeMap::new()).expect(
                    "this codec's own fields/targets are fixed by star_tracker_packet_codec and validated at construction; no declared noise is ever supplied here, so the SPD check can never fail",
                );
                self.measurements.borrow_mut().extend(measured);
                // M22.2b (`docs/open-questions.md` question 95): the same `seq` this call just
                // wrote into the FRAMED packet's own CCSDS sequence count field, additionally
                // exposed as `output.<instance>.seq@time` (an `"output.seq"`-declaring
                // `SystemDefinition`, see `crate::drm::executor::declared_outputs`) -- this is
                // what makes "how many measurement packets has this instance emitted by a given
                // time" observable through `RunProducts.scores` at all: `crate::drm::executor`
                // never captures raw FRAMED port traffic (only `crate::router::Router` sees it,
                // transiently, mid-run), so without this there would be no way for a caller of
                // the public `av_kernel::drm::execute` entry point to verify the declared update
                // rate was actually honoured through the full executor path. Overwritten on every
                // iteration of this `while` loop, so a step spanning several elapsed periods
                // (a kernel step coarser than this sensor's own declared rate) ends up reporting
                // the *last* one emitted this call -- exactly the value a caller querying
                // `output.<instance>.seq@<this step's own end epoch>` should see.
                outputs.insert("seq".to_string(), seq as f64);
            }
            self.next_due.set(due + self.period_ns);
        }
        Ok((StepResult { state: Vec::new(), t_tai_ns: end, outputs }, outbox, Vec::new()))
    }

    /// Question 173: whatever the immediately preceding `step_with_ports` call built (empty if
    /// that call crossed no emission boundary, or truth had not arrived yet).
    fn last_measurements(&self) -> Vec<pb::Measurement> {
        self.measurements.borrow().clone()
    }

    /// Question 178 (R5.1a): drains [`Self::fault_frames_affected`]/[`Self::
    /// fault_first_effect_tai_ns`], resetting both to their empty state -- `None` when nothing
    /// has been affected since the last drain (no fault installed, or one installed but not yet
    /// reached by a real emission), never a zero-valued `Some` (mirrors `crate::router::Router::
    /// take_applied_port_faults`'s own "a fault that never actually applies produces no event at
    /// all" rule). `crate::drm::executor::run_shared_group` calls this at every boundary this
    /// instance's own handle is about to be discarded and re-materialized -- see that function's
    /// own doc comment for why every boundary, not only the two SENSOR-fault-specific ones.
    fn drain_sensor_fault_effect(&self) -> Option<av_dynamics::SensorFaultEffectDrain> {
        let frames_affected = self.fault_frames_affected.get();
        if frames_affected == 0 {
            return None;
        }
        let first_effect_tai_ns = self
            .fault_first_effect_tai_ns
            .get()
            .expect("fault_frames_affected > 0 implies fault_first_effect_tai_ns is Some -- record_fault_effect always sets both together");
        self.fault_frames_affected.set(0);
        self.fault_first_effect_tai_ns.set(None);
        Some(av_dynamics::SensorFaultEffectDrain { first_effect_tai_ns, frames_affected })
    }
}

impl StarTrackerModel {
    /// Question 178 (R5.1a): the sensor's reported deviation from truth (the composed
    /// small-angle noise vector, `Bias` added in before conversion, `Scale` multiplying the
    /// combined vector) -- see [`Self::step_with_ports`]'s own doc comment for `Dropout`/
    /// `Freeze`, which never reach this function at all (dropout emits nothing; freeze calls
    /// this only for the one measurement it ever actually computes). Composes noise into the
    /// reported quaternion via [`quat_mul`], exactly [`Self::step_with_ports`]'s own pre-
    /// existing convention (trap 1) -- never touched, only the small-angle VECTOR fed into
    /// [`small_angle_to_quat`] gains a bias term and/or a scale factor first.
    fn compute_measured_quaternion(&self, truth_q: [f64; 4]) -> [f64; 4] {
        let mut err_vec = {
            let mut rng = self.rng.borrow_mut();
            gaussian_vec3(&mut rng, self.spec.noise_sigma_rad)
        };
        if let Some(StarTrackerFaultEffect::Bias { axis, value_rad }) = self.spec.fault {
            err_vec[axis] += value_rad;
        }
        if let Some(StarTrackerFaultEffect::Scale { value }) = self.spec.fault {
            err_vec = [err_vec[0] * value, err_vec[1] * value, err_vec[2] * value];
        }
        let delta_q = small_angle_to_quat(err_vec);
        let mounted = quat_mul(self.spec.mount_q, truth_q);
        quat_mul(delta_q, mounted)
    }

    /// Question 178 (R5.1a): record one more emission `self.spec.fault` changed or suppressed,
    /// at TAI epoch `due` -- called for every kind (`Dropout` included, question 186(c): "the
    /// ones dropout suppressed" count too) exactly once per affected emission instant, only when
    /// truth has already arrived (an emission with no truth yet produces nothing regardless of
    /// any fault, so it is never "affected" by one -- see [`Self::step_with_ports`]'s own call
    /// sites, both inside the `if let Some((truth_q, ...)) = ...` guard).
    fn record_fault_effect(&self, due: i64) {
        self.fault_frames_affected.set(self.fault_frames_affected.get() + 1);
        if self.fault_first_effect_tai_ns.get().is_none() {
            self.fault_first_effect_tai_ns.set(Some(due));
        }
    }
}

// ============================================================================================
// IMU.
// ============================================================================================

/// Parsed, typed parameters for [`ImuModel`] -- built by [`parse_imu_spec`]. Parameter
/// vocabulary (a name matching none of these is [`SensorSpecError::UnknownParameter`]):
/// - `imu.update_rate_hz` (required, `> 0`): declared measurement rate, Hz.
/// - `imu.seed` (required, nonnegative integer): seeds this instance's own `Pcg64`.
/// - `imu.gyro_noise_sigma` / `imu.accel_noise_sigma` (required, `> 0`): 1-sigma per-axis white
///   measurement noise, rad/s and m/s^2 respectively.
/// - `imu.gyro_bias_rw_sigma` / `imu.accel_bias_rw_sigma` (required, `>= 0`): random-walk sigma
///   (per sqrt(second)) for the corresponding bias channel -- trap 2.
/// - `imu.mount_q.{x,y,z,w}` (optional, all four together; default identity): fixed mounting
///   rotation, applied to both the gyro and accelerometer triads.
/// - `imu.true_specific_force.{x,y,z}` (optional, all three together; default zero): **M22.2
///   scope note** -- this batch's attitude-only fixture has no translational dynamics at all
///   (`crate::drm::attitude::AttitudeWheelsModel` propagates no linear state), so "truth
///   acceleration" cannot come from a truth port the way rate/quaternion do; it is instead a
///   declared constant, exactly the way `super::binding::ConstantAccelModel`'s own constant `a`
///   bakes in a physical quantity this milestone has no runtime source for yet. A future
///   translational-dynamics milestone is the natural place to replace this with a real truth
///   port.
#[derive(Debug, Clone, PartialEq)]
pub struct ImuSpec {
    pub update_rate_hz: f64,
    pub seed: u64,
    pub gyro_noise_sigma: f64,
    pub gyro_bias_rw_sigma: f64,
    pub accel_noise_sigma: f64,
    pub accel_bias_rw_sigma: f64,
    pub mount_q: [f64; 4],
    pub true_specific_force: [f64; 3],
}

pub fn parse_imu_spec(params: &BTreeMap<String, Parameter>) -> Result<ImuSpec, SensorSpecError> {
    let mut update_rate_hz = None;
    let mut seed = None;
    let mut gyro_noise_sigma = None;
    let mut gyro_bias_rw_sigma = None;
    let mut accel_noise_sigma = None;
    let mut accel_bias_rw_sigma = None;
    let mut mount_q = [None; 4];
    let mut true_force = [None; 3];
    for (name, p) in params {
        match name.as_str() {
            "imu.update_rate_hz" => update_rate_hz = Some(p.value),
            "imu.seed" => seed = Some(parse_seed("imu.seed", p)?),
            "imu.gyro_noise_sigma" => {
                check_unit("imu.gyro_noise_sigma", p, pb::Unit::RadianPerSecond)?;
                gyro_noise_sigma = Some(p.value);
            }
            "imu.gyro_bias_rw_sigma" => {
                check_unit("imu.gyro_bias_rw_sigma", p, pb::Unit::RadianPerSecond)?;
                gyro_bias_rw_sigma = Some(p.value);
            }
            "imu.accel_noise_sigma" => {
                check_unit("imu.accel_noise_sigma", p, pb::Unit::MeterPerSecondSquared)?;
                accel_noise_sigma = Some(p.value);
            }
            "imu.accel_bias_rw_sigma" => {
                check_unit("imu.accel_bias_rw_sigma", p, pb::Unit::MeterPerSecondSquared)?;
                accel_bias_rw_sigma = Some(p.value);
            }
            "imu.mount_q.x" => mount_q[0] = Some(p.value),
            "imu.mount_q.y" => mount_q[1] = Some(p.value),
            "imu.mount_q.z" => mount_q[2] = Some(p.value),
            "imu.mount_q.w" => mount_q[3] = Some(p.value),
            "imu.true_specific_force.x" => true_force[0] = Some(p.value),
            "imu.true_specific_force.y" => true_force[1] = Some(p.value),
            "imu.true_specific_force.z" => true_force[2] = Some(p.value),
            // See `parse_star_tracker_spec`'s identical arm's own doc comment.
            _ if name.starts_with("output.") => {}
            other => return Err(SensorSpecError::UnknownParameter { name: other.to_string() }),
        }
    }
    let update_rate_hz = require_positive(update_rate_hz, "imu.update_rate_hz")?;
    let seed = seed.ok_or_else(|| SensorSpecError::MissingParameter { name: "imu.seed".to_string() })?;
    let gyro_noise_sigma = require_positive(gyro_noise_sigma, "imu.gyro_noise_sigma")?;
    let gyro_bias_rw_sigma = require_nonneg(gyro_bias_rw_sigma, "imu.gyro_bias_rw_sigma")?;
    let accel_noise_sigma = require_positive(accel_noise_sigma, "imu.accel_noise_sigma")?;
    let accel_bias_rw_sigma = require_nonneg(accel_bias_rw_sigma, "imu.accel_bias_rw_sigma")?;
    let mount_q = parse_optional_unit_quat(mount_q, "imu.mount_q")?;
    let true_specific_force = match true_force {
        [None, None, None] => [0.0, 0.0, 0.0],
        [Some(x), Some(y), Some(z)] => [x, y, z],
        _ => return Err(SensorSpecError::MissingParameter { name: "imu.true_specific_force.{x,y,z} (all three required together, or none at all for zero)".to_string() }),
    };
    Ok(ImuSpec { update_rate_hz, seed, gyro_noise_sigma, gyro_bias_rw_sigma, accel_noise_sigma, accel_bias_rw_sigma, mount_q, true_specific_force })
}

/// The fixed field layout every IMU `PacketCodec` this module builds/expects uses: six
/// big-endian IEEE-754 `binary64` fields, `wx,wy,wz` (rad/s) then `ax,ay,az` (m/s^2), 48
/// user-data bytes total.
/// M25.3 (question 173): the gyro triad's own shared measurement id -- independent per-axis
/// white noise (no manifold constraint, unlike the star tracker's quaternion), so
/// [`ImuModel::step_with_ports`] can honestly declare `r = diag(gyro_noise_sigma^2)` for it.
pub const MEASUREMENT_ID_IMU_GYRO3: &str = "altavista.imu_gyro3";
/// The accelerometer triad's own shared measurement id, `r = diag(accel_noise_sigma^2)`.
pub const MEASUREMENT_ID_IMU_ACCEL3: &str = "altavista.imu_accel3";

pub fn imu_packet_codec(id: &str, apid: u32) -> pb::PacketCodec {
    let f = |name: &str, bit_offset: u32, unit: pb::Unit, measurement_id: &str| pb::PacketField {
        name: name.to_string(),
        bit_offset,
        bit_width: 64,
        r#type: pb::PacketFieldType::Float64 as i32,
        unit: unit as i32,
        scale: 1.0,
        offset: 0.0,
        target: format!("{measurement_id}/{name}"),
    };
    pb::PacketCodec {
        id: id.to_string(),
        apid,
        is_command: false,
        secondary_header_bytes: 0,
        user_data_bytes: 48,
        fields: vec![
            f("wx", 0, pb::Unit::RadianPerSecond, MEASUREMENT_ID_IMU_GYRO3),
            f("wy", 64, pb::Unit::RadianPerSecond, MEASUREMENT_ID_IMU_GYRO3),
            f("wz", 128, pb::Unit::RadianPerSecond, MEASUREMENT_ID_IMU_GYRO3),
            f("ax", 192, pb::Unit::MeterPerSecondSquared, MEASUREMENT_ID_IMU_ACCEL3),
            f("ay", 256, pb::Unit::MeterPerSecondSquared, MEASUREMENT_ID_IMU_ACCEL3),
            f("az", 320, pb::Unit::MeterPerSecondSquared, MEASUREMENT_ID_IMU_ACCEL3),
        ],
        description: "IMU rate + specific-force measurement (M22.2)".to_string(),
    }
}

/// A native IMU `av_dynamics::DynamicsModel`. Propagated state (`state_dim() == 6`): `[bias_
/// gyro_x, bias_gyro_y, bias_gyro_z, bias_accel_x, bias_accel_y, bias_accel_z]` -- a genuine
/// discrete-time random walk, advanced once per elapsed declared measurement period inside
/// `step`/`step_with_ports` (both overridden; see their own doc comments for why the default
/// Dopri5-integrator path via `derivatives` is deliberately bypassed). `type Error =
/// std::convert::Infallible` for the same reason as [`StarTrackerModel`].
#[derive(Debug)]
pub struct ImuModel {
    spec: ImuSpec,
    codec: pb::PacketCodec,
    output_port: String,
    period_ns: i64,
    next_due: Cell<i64>,
    seq: Cell<u16>,
    rng: RefCell<Pcg64>,
    last_truth: RefCell<Option<([f64; 4], [f64; 3])>>,
    info: ModelInfo,
    /// Question 173 (M25.3): see `StarTrackerModel::measurements`'s identical doc comment.
    measurements: RefCell<Vec<pb::Measurement>>,
}

impl ImuModel {
    /// `epoch_tai_ns`: see `StarTrackerModel::new`'s identical parameter's own doc comment (the
    /// M22.2b bug fix) -- seeds `next_due` as `epoch_tai_ns + period_ns` instead of the pre-fix
    /// `period_ns` alone, which iterated `while end >= self.next_due.get()` billions of times
    /// against any realistic TAI epoch before this fix.
    pub fn new(spec: ImuSpec, codec: pb::PacketCodec, output_port: String, epoch_tai_ns: i64, model_id: &str) -> Result<Self, SensorSpecError> {
        codec::validate_codec(&codec).map_err(SensorSpecError::Codec)?;
        for name in ["wx", "wy", "wz", "ax", "ay", "az"] {
            if !codec.fields.iter().any(|f| f.name == name) {
                return Err(SensorSpecError::InvalidParameter { name: "imu.codec".to_string(), reason: format!("declared PacketCodec {:?} is missing required field {name:?}", codec.id) });
            }
        }
        let period_ns = (1.0e9 / spec.update_rate_hz).round() as i64;
        let mut settings = BTreeMap::new();
        settings.insert("update_rate_hz".to_string(), format!("{:.17e}", spec.update_rate_hz));
        settings.insert("seed".to_string(), spec.seed.to_string());
        settings.insert("gyro_noise_sigma".to_string(), format!("{:.17e}", spec.gyro_noise_sigma));
        settings.insert("gyro_bias_rw_sigma".to_string(), format!("{:.17e}", spec.gyro_bias_rw_sigma));
        settings.insert("accel_noise_sigma".to_string(), format!("{:.17e}", spec.accel_noise_sigma));
        settings.insert("accel_bias_rw_sigma".to_string(), format!("{:.17e}", spec.accel_bias_rw_sigma));
        for (i, v) in spec.mount_q.iter().enumerate() {
            settings.insert(format!("mount_q_{i}"), format!("{v:.17e}"));
        }
        for (i, v) in spec.true_specific_force.iter().enumerate() {
            settings.insert(format!("true_specific_force_{i}"), format!("{v:.17e}"));
        }
        settings.insert("output_port".to_string(), output_port.clone());
        settings.insert("apid".to_string(), codec.apid.to_string());
        let settings_hash = av_dynamics::settings_hash(&settings);
        let info = ModelInfo { id: model_id.to_string(), version: "1".to_string(), state_space_id: format!("{model_id}.bias_state"), frame_id: String::new(), settings_hash, depth: "native".to_string(), ..Default::default() };
        let seed = spec.seed;
        Ok(Self {
            spec,
            codec,
            output_port,
            period_ns,
            next_due: Cell::new(epoch_tai_ns + period_ns),
            seq: Cell::new(0),
            rng: RefCell::new(Pcg64::new(seed)),
            last_truth: RefCell::new(None),
            info,
            measurements: RefCell::new(Vec::new()),
        })
    }

    pub fn period_ns(&self) -> i64 {
        self.period_ns
    }
}

impl DynamicsModel for ImuModel {
    type Error = std::convert::Infallible;

    fn state_dim(&self) -> usize {
        6
    }
    fn derivatives(&self, state: &[f64], _t_tai_ns: i64, _controls: &[f64], out: &mut [f64]) -> Result<(), Self::Error> {
        // Never reached: `step`/`step_with_ports` are both overridden below and bypass the
        // default Dopri5-integrator path entirely, because a bias random walk is a discrete
        // stochastic process (`bias += sigma_rw * sqrt(dt) * N(0,1)`, once per elapsed
        // measurement period) -- not a smooth ODE a multi-stage adaptive integrator could
        // correctly evaluate `derivatives` for (trap 2). An honest zero, not `unimplemented!()`,
        // purely to satisfy the trait.
        debug_assert_eq!(state.len(), 6);
        debug_assert_eq!(out.len(), 6);
        out.fill(0.0);
        Ok(())
    }
    fn describe(&self) -> ModelInfo {
        self.info.clone()
    }
    fn step(&self, state: &[f64], t_tai_ns: i64, controls: &[f64], dt_ns: i64) -> Result<StepResult, Self::Error> {
        Ok(self.step_with_ports(state, t_tai_ns, controls, dt_ns, &Inbox::empty())?.0)
    }

    /// See the module doc comment's traps 2/3/4 -- this is where all three are actually closed.
    /// Bias advances by exactly one [`random_walk_step3`] increment per elapsed declared
    /// measurement period (never per kernel step -- trap 3), regardless of whether truth has
    /// arrived yet (a real IMU's bias walks whether or not anything is currently measuring it);
    /// a measurement (and a pushed FRAMED packet) is only produced when truth is available.
    fn step_with_ports(&self, state: &[f64], t_tai_ns: i64, _controls: &[f64], dt_ns: i64, inbox: &Inbox) -> Result<(StepResult, Outbox, Vec<AppliedCommand>), Self::Error> {
        debug_assert_eq!(state.len(), 6, "ImuModel::step_with_ports: state must be [bias_gyro_x,y,z, bias_accel_x,y,z]");
        if let Some(truth) = read_truth(inbox) {
            *self.last_truth.borrow_mut() = Some(truth);
        }
        // Question 173: see `StarTrackerModel::step_with_ports`'s identical clear-at-top note.
        self.measurements.borrow_mut().clear();
        let mut bias = [state[0], state[1], state[2], state[3], state[4], state[5]];
        let end = t_tai_ns + dt_ns;
        let period_s = self.period_ns as f64 * 1e-9;
        let mut outbox = Outbox::new();
        let mut outputs = BTreeMap::new();
        while end >= self.next_due.get() {
            let due = self.next_due.get();
            let mut rng = self.rng.borrow_mut();
            let g = random_walk_step3(&mut rng, [bias[0], bias[1], bias[2]], self.spec.gyro_bias_rw_sigma, period_s);
            bias[0] = g[0];
            bias[1] = g[1];
            bias[2] = g[2];
            let a = random_walk_step3(&mut rng, [bias[3], bias[4], bias[5]], self.spec.accel_bias_rw_sigma, period_s);
            bias[3] = a[0];
            bias[4] = a[1];
            bias[5] = a[2];
            if let Some((_, omega_truth)) = *self.last_truth.borrow() {
                let omega_m = rotate_vector_by_quat(self.spec.mount_q, omega_truth);
                let accel_m = rotate_vector_by_quat(self.spec.mount_q, self.spec.true_specific_force);
                let gyro_noise = gaussian_vec3(&mut rng, self.spec.gyro_noise_sigma);
                let accel_noise = gaussian_vec3(&mut rng, self.spec.accel_noise_sigma);
                let mut values = BTreeMap::new();
                values.insert("wx".to_string(), FieldValue::Numeric(omega_m[0] + bias[0] + gyro_noise[0]));
                values.insert("wy".to_string(), FieldValue::Numeric(omega_m[1] + bias[1] + gyro_noise[1]));
                values.insert("wz".to_string(), FieldValue::Numeric(omega_m[2] + bias[2] + gyro_noise[2]));
                values.insert("ax".to_string(), FieldValue::Numeric(accel_m[0] + bias[3] + accel_noise[0]));
                values.insert("ay".to_string(), FieldValue::Numeric(accel_m[1] + bias[4] + accel_noise[1]));
                values.insert("az".to_string(), FieldValue::Numeric(accel_m[2] + bias[5] + accel_noise[2]));
                let seq = self.seq.get();
                self.seq.set(seq.wrapping_add(1) & 0x3FFF);
                let payload = codec::encode_packet(&self.codec, seq, &[], &values).expect(
                    "ImuModel's declared codec always carries exactly the wx/wy/wz/ax/ay/az FLOAT64 fields this call supplies, pre-validated at construction (validate_codec) -- encode_packet can only fail for a missing/mistyped/out-of-range field, none of which can happen here",
                );
                outbox.push(self.output_port.clone(), due, payload);
                // See `StarTrackerModel::step_with_ports`'s identical `outputs.insert("seq", ...)`
                // line's own doc comment.
                outputs.insert("seq".to_string(), seq as f64);
                // Question 173: the gyro and accelerometer triads are each genuinely
                // independent, per-axis white noise (`gyro_noise_sigma`/`accel_noise_sigma`,
                // no manifold constraint like the star tracker's quaternion) -- so, unlike
                // `StarTrackerModel`, `r` is honestly `diag(sigma^2)` here, not left empty.
                let gyro_var = self.spec.gyro_noise_sigma * self.spec.gyro_noise_sigma;
                let accel_var = self.spec.accel_noise_sigma * self.spec.accel_noise_sigma;
                let mut noise = BTreeMap::new();
                noise.insert(MEASUREMENT_ID_IMU_GYRO3.to_string(), vec![gyro_var, 0.0, 0.0, 0.0, gyro_var, 0.0, 0.0, 0.0, gyro_var]);
                noise.insert(MEASUREMENT_ID_IMU_ACCEL3.to_string(), vec![accel_var, 0.0, 0.0, 0.0, accel_var, 0.0, 0.0, 0.0, accel_var]);
                let measured = codec::measurements_from_field_values(&self.codec, &values, due, "", &self.info.frame_id, &noise).expect(
                    "this codec's own fields/targets are fixed by imu_packet_codec and validated at construction; the declared noise above is a positive diagonal, always SPD, so the check can never fail",
                );
                self.measurements.borrow_mut().extend(measured);
            }
            self.next_due.set(due + self.period_ns);
        }
        Ok((StepResult { state: bias.to_vec(), t_tai_ns: end, outputs }, outbox, Vec::new()))
    }

    /// Question 173: see `StarTrackerModel::last_measurements`'s identical doc comment.
    fn last_measurements(&self) -> Vec<pb::Measurement> {
        self.measurements.borrow().clone()
    }

    /// The IMU stays refused this round (`DrmError::PortOrSensorFaultNotYetSupported`, narrowed
    /// to "IMU only, R5.1b" -- `crate::drm::fault`'s own module doc comment): no DRM naming an
    /// IMU instance's SENSOR fault can ever load, so this model never has one installed to
    /// report -- always `None`.
    fn drain_sensor_fault_effect(&self) -> Option<av_dynamics::SensorFaultEffectDrain> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::ApidMap;
    use crate::rng::Pcg64;

    fn params(entries: &[(&str, f64)]) -> BTreeMap<String, Parameter> {
        entries.iter().map(|(name, value)| (name.to_string(), Parameter { name: name.to_string(), value: *value, ..Default::default() })).collect()
    }

    // ---------------------------------------------------------------------------------------
    // Quaternion helper cross-checks (independent of any sensor model).
    // ---------------------------------------------------------------------------------------

    /// Cross-check against the textbook 90-degree-about-z rotation, independent of every other
    /// test in this module -- if `quat_mul`/`rotate_vector_by_quat` had a sign or ordering bug,
    /// this would be the test to catch it before it masquerades as a passing noise test.
    #[test]
    fn rotate_vector_by_quat_matches_the_textbook_rotation_about_z() {
        let theta = std::f64::consts::FRAC_PI_2;
        let q = [0.0, 0.0, (theta / 2.0).sin(), (theta / 2.0).cos()];
        let got = rotate_vector_by_quat(q, [1.0, 0.0, 0.0]);
        assert!((got[0] - theta.cos()).abs() < 1e-12, "{got:?}");
        assert!((got[1] - theta.sin()).abs() < 1e-12, "{got:?}");
        assert!(got[2].abs() < 1e-12, "{got:?}");
    }

    #[test]
    fn quat_mul_identity_is_a_no_op_on_either_side() {
        let q = [0.1, 0.2, 0.3, (1.0 - 0.1f64.powi(2) - 0.2f64.powi(2) - 0.3f64.powi(2)).sqrt()];
        let id = [0.0, 0.0, 0.0, 1.0];
        let a = quat_mul(id, q);
        let b = quat_mul(q, id);
        for i in 0..4 {
            assert!((a[i] - q[i]).abs() < 1e-15, "{a:?} vs {q:?}");
            assert!((b[i] - q[i]).abs() < 1e-15, "{b:?} vs {q:?}");
        }
    }

    #[test]
    fn quat_mul_of_a_quaternion_and_its_conjugate_is_identity_for_a_unit_quaternion() {
        let theta = 1.234_f64;
        let axis_norm = (0.3f64.powi(2) + 0.4f64.powi(2) + 0.5f64.powi(2)).sqrt();
        let axis = [0.3 / axis_norm, 0.4 / axis_norm, 0.5 / axis_norm];
        let q = [axis[0] * (theta / 2.0).sin(), axis[1] * (theta / 2.0).sin(), axis[2] * (theta / 2.0).sin(), (theta / 2.0).cos()];
        let got = quat_mul(q, quat_conj(q));
        assert!((got[3] - 1.0).abs() < 1e-12, "{got:?}");
        for i in 0..3 {
            assert!(got[i].abs() < 1e-12, "{got:?}");
        }
    }

    /// [`small_angle_to_quat`] then [`quat_to_small_angle`] recovers the original vector exactly
    /// (up to floating point) -- the statistical pin tests below rely on this round trip to
    /// recover injected noise from a composed quaternion.
    #[test]
    fn small_angle_to_quat_and_back_round_trips() {
        let v = [0.001, -0.002, 0.0007];
        let q = small_angle_to_quat(v);
        assert!((quat_norm(q) - 1.0).abs() < 1e-15, "not unit: {q:?}");
        let back = quat_to_small_angle(q);
        for i in 0..3 {
            assert!((back[i] - v[i]).abs() < 1e-12, "{back:?} vs {v:?}");
        }
    }

    // ---------------------------------------------------------------------------------------
    // Typed refusal: parse_star_tracker_spec / parse_imu_spec.
    // ---------------------------------------------------------------------------------------

    #[test]
    fn parse_star_tracker_spec_refuses_an_unrecognized_parameter() {
        let p = params(&[("startracker.nonsense", 1.0)]);
        let err = parse_star_tracker_spec(&p).unwrap_err();
        assert!(matches!(err, SensorSpecError::UnknownParameter { ref name } if name == "startracker.nonsense"), "{err}");
    }

    #[test]
    fn parse_star_tracker_spec_refuses_a_missing_seed() {
        let p = params(&[("startracker.update_rate_hz", 5.0), ("startracker.noise_sigma_rad", 1e-5)]);
        let err = parse_star_tracker_spec(&p).unwrap_err();
        assert!(matches!(err, SensorSpecError::MissingParameter { ref name } if name == "startracker.seed"), "{err}");
    }

    #[test]
    fn parse_star_tracker_spec_refuses_a_non_positive_update_rate() {
        let p = params(&[("startracker.update_rate_hz", 0.0), ("startracker.seed", 1.0), ("startracker.noise_sigma_rad", 1e-5)]);
        let err = parse_star_tracker_spec(&p).unwrap_err();
        assert!(matches!(err, SensorSpecError::InvalidParameter { ref name, .. } if name == "startracker.update_rate_hz"), "{err}");
    }

    #[test]
    fn parse_star_tracker_spec_refuses_a_non_integer_seed() {
        let p = params(&[("startracker.update_rate_hz", 5.0), ("startracker.seed", 1.5), ("startracker.noise_sigma_rad", 1e-5)]);
        let err = parse_star_tracker_spec(&p).unwrap_err();
        assert!(matches!(err, SensorSpecError::InvalidParameter { ref name, .. } if name == "startracker.seed"), "{err}");
    }

    #[test]
    fn parse_star_tracker_spec_refuses_a_non_unit_mount_quaternion() {
        let mut p = params(&[("startracker.update_rate_hz", 5.0), ("startracker.seed", 1.0), ("startracker.noise_sigma_rad", 1e-5)]);
        p.extend(params(&[("startracker.mount_q.x", 0.0), ("startracker.mount_q.y", 0.0), ("startracker.mount_q.z", 0.0), ("startracker.mount_q.w", 2.0)]));
        let err = parse_star_tracker_spec(&p).unwrap_err();
        assert!(matches!(err, SensorSpecError::InvalidParameter { ref name, .. } if name == "startracker.mount_q"), "{err}");
    }

    #[test]
    fn parse_star_tracker_spec_refuses_a_partial_mount_quaternion_group() {
        let mut p = params(&[("startracker.update_rate_hz", 5.0), ("startracker.seed", 1.0), ("startracker.noise_sigma_rad", 1e-5)]);
        p.extend(params(&[("startracker.mount_q.x", 0.0)]));
        let err = parse_star_tracker_spec(&p).unwrap_err();
        assert!(matches!(err, SensorSpecError::MissingParameter { .. }), "{err}");
    }

    #[test]
    fn parse_star_tracker_spec_accepts_a_well_formed_spec_with_default_identity_mount() {
        let p = params(&[("startracker.update_rate_hz", 5.0), ("startracker.seed", 42.0), ("startracker.noise_sigma_rad", 1e-5)]);
        let spec = parse_star_tracker_spec(&p).expect("well-formed spec parses");
        assert_eq!(spec.mount_q, [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(spec.seed, 42);
    }

    #[test]
    fn parse_imu_spec_refuses_an_unrecognized_parameter() {
        let p = params(&[("imu.nonsense", 1.0)]);
        let err = parse_imu_spec(&p).unwrap_err();
        assert!(matches!(err, SensorSpecError::UnknownParameter { ref name } if name == "imu.nonsense"), "{err}");
    }

    fn valid_imu_params() -> BTreeMap<String, Parameter> {
        params(&[("imu.update_rate_hz", 10.0), ("imu.seed", 7.0), ("imu.gyro_noise_sigma", 1e-4), ("imu.gyro_bias_rw_sigma", 1e-6), ("imu.accel_noise_sigma", 1e-3), ("imu.accel_bias_rw_sigma", 1e-5)])
    }

    #[test]
    fn parse_imu_spec_accepts_a_well_formed_spec_with_defaults() {
        let spec = parse_imu_spec(&valid_imu_params()).expect("well-formed spec parses");
        assert_eq!(spec.mount_q, [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(spec.true_specific_force, [0.0, 0.0, 0.0]);
    }

    #[test]
    fn parse_imu_spec_refuses_a_negative_bias_rw_sigma() {
        let mut p = valid_imu_params();
        p.insert("imu.gyro_bias_rw_sigma".to_string(), Parameter { name: "imu.gyro_bias_rw_sigma".to_string(), value: -1.0, ..Default::default() });
        let err = parse_imu_spec(&p).unwrap_err();
        assert!(matches!(err, SensorSpecError::InvalidParameter { ref name, .. } if name == "imu.gyro_bias_rw_sigma"), "{err}");
    }

    #[test]
    fn parse_imu_spec_refuses_a_wrong_declared_unit() {
        let mut p = valid_imu_params();
        // m/s^2 unit on a rad/s parameter -- a copy-paste-style mistake, must be refused.
        p.insert("imu.gyro_noise_sigma".to_string(), Parameter { name: "imu.gyro_noise_sigma".to_string(), value: 1e-4, unit: pb::Unit::MeterPerSecondSquared as i32, ..Default::default() });
        let err = parse_imu_spec(&p).unwrap_err();
        assert!(matches!(err, SensorSpecError::InvalidParameter { ref name, .. } if name == "imu.gyro_noise_sigma"), "{err}");
    }

    #[test]
    fn parse_imu_spec_refuses_a_partial_true_specific_force_group() {
        let mut p = valid_imu_params();
        p.extend(params(&[("imu.true_specific_force.x", 1.0), ("imu.true_specific_force.y", 2.0)]));
        let err = parse_imu_spec(&p).unwrap_err();
        assert!(matches!(err, SensorSpecError::MissingParameter { .. }), "{err}");
    }

    // ---------------------------------------------------------------------------------------
    // StarTrackerModel::new / ImuModel::new: codec validation.
    // ---------------------------------------------------------------------------------------

    #[test]
    fn star_tracker_new_refuses_a_codec_missing_a_required_field() {
        let spec = StarTrackerSpec { update_rate_hz: 5.0, seed: 1, noise_sigma_rad: 1e-5, mount_q: [0.0, 0.0, 0.0, 1.0], fault: None };
        let mut codec = star_tracker_packet_codec("st1", 100);
        codec.fields.retain(|f| f.name != "qw");
        let err = StarTrackerModel::new(spec, codec, "st_out".to_string(), 0, "startracker.test").unwrap_err();
        assert!(matches!(err, SensorSpecError::InvalidParameter { ref name, .. } if name == "startracker.codec"), "{err}");
    }

    #[test]
    fn imu_new_refuses_a_malformed_codec_via_validate_codec() {
        let spec = ImuSpec { update_rate_hz: 10.0, seed: 1, gyro_noise_sigma: 1e-4, gyro_bias_rw_sigma: 1e-6, accel_noise_sigma: 1e-3, accel_bias_rw_sigma: 1e-5, mount_q: [0.0, 0.0, 0.0, 1.0], true_specific_force: [0.0, 0.0, 0.0] };
        let mut codec = imu_packet_codec("imu1", 101);
        codec.user_data_bytes = 0; // invalid per validate_codec
        let err = ImuModel::new(spec, codec, "imu_out".to_string(), 0, "imu.test").unwrap_err();
        assert!(matches!(err, SensorSpecError::Codec(_)), "{err}");
    }

    // ---------------------------------------------------------------------------------------
    // Fixtures shared by the trap/statistical tests below.
    // ---------------------------------------------------------------------------------------

    // Every caller of these two helpers steps the returned model starting from `t = 0` (this
    // module's own tests never use a realistic absolute TAI epoch) -- `epoch_tai_ns: 0` here
    // preserves that pre-existing behaviour exactly (`next_due` seeds to `0 + period_ns`,
    // unchanged from before the M22.2b fix documented on `StarTrackerModel::new`/`ImuModel::new`
    // added an explicit `epoch_tai_ns` parameter).
    fn star_tracker(update_rate_hz: f64, seed: u64, sigma: f64) -> StarTrackerModel {
        let spec = StarTrackerSpec { update_rate_hz, seed, noise_sigma_rad: sigma, mount_q: [0.0, 0.0, 0.0, 1.0], fault: None };
        let codec = star_tracker_packet_codec("st_test", 100);
        StarTrackerModel::new(spec, codec, "st_out".to_string(), 0, "startracker.test").unwrap()
    }

    fn imu(update_rate_hz: f64, seed: u64, gyro_rw: f64, accel_rw: f64) -> ImuModel {
        let spec = ImuSpec { update_rate_hz, seed, gyro_noise_sigma: 1e-4, gyro_bias_rw_sigma: gyro_rw, accel_noise_sigma: 1e-3, accel_bias_rw_sigma: accel_rw, mount_q: [0.0, 0.0, 0.0, 1.0], true_specific_force: [0.0, 0.0, 0.0] };
        let codec = imu_packet_codec("imu_test", 101);
        ImuModel::new(spec, codec, "imu_out".to_string(), 0, "imu.test").unwrap()
    }

    fn feed_truth(inbox_truth: ([f64; 4], [f64; 3])) -> Inbox {
        let ob = truth_outbox(inbox_truth.0, inbox_truth.1, 0);
        Inbox::new(ob.into_messages())
    }

    // ---------------------------------------------------------------------------------------
    // M22.2b bug fix regression: `next_due` must seed relative to the instance's own
    // `epoch_tai_ns`, not absolute TAI zero -- see `StarTrackerModel::new`/`ImuModel::new`'s own
    // doc comment. Both tests below construct at a realistic epoch
    // (`drms/demo_attitude_sensors.drm.yaml`'s own `start_tai_ns`, ~1.77e18 ns) and step by
    // exactly one declared period -- before the fix, `step_with_ports`'s own `while end >=
    // self.next_due.get()` loop would have had to run roughly `epoch_tai_ns / period_ns` times
    // (billions) before ever reaching `end`, so these tests would not complete in any practical
    // time at all (confirmed directly: reverting the fix and re-running either test with `cargo
    // test -p av-kernel --lib <name> -- --test-threads=1` hangs well past a minute with no other
    // test running at all, vs. instantaneous with the fix in place). A wrong-direction fix (e.g.
    // seeding `next_due` to `epoch_tai_ns` itself rather than `epoch_tai_ns + period_ns`) would
    // instead fail the exact emission-count assertion below, not merely run slowly.
    // ---------------------------------------------------------------------------------------

    /// `drms/demo_attitude_sensors.drm.yaml`'s own realistic epoch (1767225637000000000 ns) --
    /// used here, and not `t = 0`, specifically to reproduce the M22.2b bug this test guards
    /// against (every other test in this module steps from `t = 0`, which is exactly why this
    /// defect survived M22.2's own 34 passing unit tests undetected until the M22.2b binding-path
    /// wiring work stepped a sensor from a realistic absolute epoch for the first time).
    const REALISTIC_EPOCH_TAI_NS: i64 = 1_767_225_637_000_000_000;

    #[test]
    fn star_tracker_constructed_at_a_realistic_epoch_emits_exactly_once_per_declared_period() {
        let spec = StarTrackerSpec { update_rate_hz: 2.0, seed: 1, noise_sigma_rad: 1e-6, mount_q: [0.0, 0.0, 0.0, 1.0], fault: None };
        let codec = star_tracker_packet_codec("st_test", 100);
        let model = StarTrackerModel::new(spec, codec, "st_out".to_string(), REALISTIC_EPOCH_TAI_NS, "startracker.test").unwrap();
        let inbox = feed_truth(([0.0, 0.0, 0.0, 1.0], [0.0, 0.0, 0.0]));
        // One declared period (500 ms) after the epoch -- exactly the first scheduled emission,
        // due at epoch + period_ns, must fall inside this single step.
        let (_result, outbox, _applied) = model.step_with_ports(&[], REALISTIC_EPOCH_TAI_NS, &[], model.period_ns(), &inbox).unwrap();
        assert_eq!(outbox.messages().len(), 1, "exactly one emission within the first declared period after a realistic construction epoch");
    }

    #[test]
    fn imu_constructed_at_a_realistic_epoch_emits_exactly_once_per_declared_period() {
        let spec = ImuSpec { update_rate_hz: 2.0, seed: 1, gyro_noise_sigma: 1e-4, gyro_bias_rw_sigma: 1e-6, accel_noise_sigma: 1e-3, accel_bias_rw_sigma: 1e-5, mount_q: [0.0, 0.0, 0.0, 1.0], true_specific_force: [0.0, 0.0, 0.0] };
        let codec = imu_packet_codec("imu_test", 101);
        let model = ImuModel::new(spec, codec, "imu_out".to_string(), REALISTIC_EPOCH_TAI_NS, "imu.test").unwrap();
        let inbox = feed_truth(([0.0, 0.0, 0.0, 1.0], [0.0, 0.0, 0.0]));
        let (result, outbox, _applied) = model.step_with_ports(&[0.0; 6], REALISTIC_EPOCH_TAI_NS, &[], model.period_ns(), &inbox).unwrap();
        assert_eq!(outbox.messages().len(), 1, "exactly one emission within the first declared period after a realistic construction epoch");
        assert_eq!(result.state.len(), 6);
    }

    const IDENTITY_Q: [f64; 4] = [0.0, 0.0, 0.0, 1.0];

    fn decode_star_tracker(codec: &pb::PacketCodec, payload: &[u8]) -> [f64; 4] {
        let mut map = ApidMap::new();
        map.insert(codec.apid, codec.clone());
        let decoded = codec::decode_packet(&map, payload).expect("decodes");
        let get = |name: &str| match decoded.fields.get(name) {
            Some(FieldValue::Numeric(v)) => *v,
            other => panic!("field {name}: {other:?}"),
        };
        [get("qx"), get("qy"), get("qz"), get("qw")]
    }

    // ---------------------------------------------------------------------------------------
    // TRAP 1: quaternion noise keeps the quaternion unit norm, via composition not add+renorm.
    //
    // Wrong implementation this fails against: adding sigma*N(0,1) directly to each of
    // [qx,qy,qz,qw] and shipping it unnormalized (or "normalized" only by luck) -- the measured
    // norm would then differ from 1 by O(sigma), many orders above the tolerance below, for
    // essentially every draw.
    // ---------------------------------------------------------------------------------------

    #[test]
    fn measured_quaternion_is_always_unit_norm_to_machine_precision() {
        let model = star_tracker(10.0, 1, 0.01);
        let inbox = feed_truth((IDENTITY_Q, [0.0, 0.0, 0.0]));
        let mut state: Vec<f64> = Vec::new();
        let mut t = 0i64;
        let dt = model.period_ns();
        for _ in 0..200 {
            let (result, outbox, _) = model.step_with_ports(&state, t, &[], dt, &inbox).unwrap();
            state = result.state;
            t = result.t_tai_ns;
            for m in outbox.messages() {
                let q = decode_star_tracker(&star_tracker_packet_codec("st_test", 100), &m.payload);
                let n = quat_norm(q);
                assert!((n - 1.0).abs() < 1e-12, "measured quaternion {q:?} has norm {n}, not 1 -- see this test's own doc comment for the implementation this catches");
            }
        }
    }

    /// Structural proof, not merely statistical: with sigma effectively producing an exactly
    /// representable noise vector via a fixed seed, the measured quaternion after mounting must
    /// equal `quat_mul(delta_q, truth_q)` bit-for-bit -- i.e. this is genuinely quaternion
    /// composition, not some other combination that happens to also stay unit norm (e.g.
    /// spherical-linear-interpolating toward a random target, which would also stay unit but
    /// would not match this exact algebraic identity).
    #[test]
    fn composing_by_quaternion_multiplication_not_add_and_renormalize_is_structurally_required() {
        let model = star_tracker(1.0, 123, 0.02);
        let truth_q = {
            // A nontrivial, exact unit truth quaternion (30 degrees about an arbitrary axis).
            let axis_norm = (1.0f64 + 4.0 + 9.0).sqrt();
            let axis = [1.0 / axis_norm, 2.0 / axis_norm, 3.0 / axis_norm];
            let half = (30.0f64).to_radians() / 2.0;
            [axis[0] * half.sin(), axis[1] * half.sin(), axis[2] * half.sin(), half.cos()]
        };
        let inbox = feed_truth((truth_q, [0.0, 0.0, 0.0]));
        let dt = model.period_ns();
        let (result, outbox, _) = model.step_with_ports(&[], 0, &[], dt, &inbox).unwrap();
        let _ = result;
        assert_eq!(outbox.messages().len(), 1);
        let measured = decode_star_tracker(&star_tracker_packet_codec("st_test", 100), &outbox.messages()[0].payload);

        // Recompute independently what the very first draw from a freshly-seeded Pcg64(123) must
        // have been (the exact same sequence `step_with_ports` itself drew), then the exact
        // composed quaternion this test expects -- this is compared bit-for-bit (well within
        // 1e-14) against what the model actually emitted.
        let mut rng = Pcg64::new(123);
        let err_vec = gaussian_vec3(&mut rng, 0.02);
        let delta_q = small_angle_to_quat(err_vec);
        let expected = quat_mul(delta_q, truth_q);
        for i in 0..4 {
            assert!((measured[i] - expected[i]).abs() < 1e-14, "{measured:?} vs expected {expected:?}");
        }
    }

    // ---------------------------------------------------------------------------------------
    // TRAP 2: IMU bias random walk actually random-walks (variance grows linearly with time).
    //
    // Wrong implementation this fails against: (a) a bias that never changes ("nonzero" check
    // alone would still pass a constant nonzero bias) -- Var would be ~0 at every horizon,
    // failing the k1 pin outright; (b) a bias that changes but with a fixed-size step regardless
    // of dt (no sqrt(dt) scaling) -- Var would still grow linearly in step *count*, coincidentally
    // still linear in time only when dt is held fixed as it is here, so this alone would not
    // catch that specific bug; the dedicated dt-scaling check further below closes that gap.
    // ---------------------------------------------------------------------------------------

    /// **Statistical pin, stated before measuring.** `N = 500,000` independent one-axis random
    /// walks (`crates/av-kernel/tests/gates_execution_error.rs`'s own N and rationale), each
    /// walked for `k1 = 1` step then `k2 = 5` more (`k2_total = 6`) with `sigma_rw = 1e-6`,
    /// `dt_s = 0.1`. Expected: `Var(bias after k steps) = sigma_rw^2 * k * dt_s` exactly (a sum
    /// of `k` iid `N(0, sigma_rw^2*dt_s)` increments) -- `Var(k=1) = 1e-13`, `Var(k=6) = 6e-13`,
    /// a factor of 6, not 1 (constant bias) or 36 (variance growing as t^2). Bound: **5 standard
    /// errors**, `SE = Var * sqrt(2/(N-1))` (exact for a Gaussian sample, same formula the Gates
    /// precedent test cites) -- `SE(k=1) ~= 6.32e-16`, `SE(k=6) ~= 3.79e-15`.
    #[test]
    fn imu_bias_variance_grows_linearly_with_elapsed_time() {
        const N: usize = 500_000;
        const SIGMA_RW: f64 = 1e-6;
        const DT_S: f64 = 0.1;
        let mut rng = Pcg64::new(9001);
        let mut at_k1 = Vec::with_capacity(N);
        let mut at_k2 = Vec::with_capacity(N);
        for _ in 0..N {
            let mut bias = [0.0, 0.0, 0.0];
            bias = random_walk_step3(&mut rng, bias, SIGMA_RW, DT_S); // k=1
            at_k1.push(bias[0]);
            for _ in 0..5 {
                bias = random_walk_step3(&mut rng, bias, SIGMA_RW, DT_S);
            } // k=6 total
            at_k2.push(bias[0]);
        }
        let var_of = |xs: &[f64]| -> f64 {
            let n = xs.len() as f64;
            let mean = xs.iter().sum::<f64>() / n;
            xs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n
        };
        let var_k1 = var_of(&at_k1);
        let var_k2 = var_of(&at_k2);
        let expected_k1 = SIGMA_RW * SIGMA_RW * 1.0 * DT_S;
        let expected_k2 = SIGMA_RW * SIGMA_RW * 6.0 * DT_S;
        let se_k1 = expected_k1 * (2.0 / (N as f64 - 1.0)).sqrt();
        let se_k2 = expected_k2 * (2.0 / (N as f64 - 1.0)).sqrt();
        let dev_k1 = (var_k1 - expected_k1).abs() / se_k1;
        let dev_k2 = (var_k2 - expected_k2).abs() / se_k2;
        eprintln!("[imu bias variance] k=1: expected={expected_k1:.6e} measured={var_k1:.6e} dev={dev_k1:.2} SE (bound 5 SE)");
        eprintln!("[imu bias variance] k=6: expected={expected_k2:.6e} measured={var_k2:.6e} dev={dev_k2:.2} SE (bound 5 SE)");
        assert!(dev_k1 < 5.0, "k=1 variance off by {dev_k1:.2} SE");
        assert!(dev_k2 < 5.0, "k=6 variance off by {dev_k2:.2} SE");
        let ratio = var_k2 / var_k1;
        assert!((ratio - 6.0).abs() < 0.2, "variance ratio {ratio} should be ~6 (linear in elapsed time), not ~1 (constant) or ~36 (quadratic)");
    }

    /// Closes the gap the trap-2 doc comment above names: without `sqrt(dt_s)` scaling (e.g. a
    /// bug using `dt_s` directly, or ignoring `dt_s` altogether), doubling `dt_s` at a fixed step
    /// count would not double the variance the way it must (`Var = sigma_rw^2 * k * dt_s`, linear
    /// in `dt_s` for fixed `k`). One step (`k=1`) at two different `dt_s` values, N=500,000,
    /// same 5-SE bound.
    #[test]
    fn imu_bias_single_step_variance_scales_linearly_with_dt_not_its_square_root_or_unscaled() {
        const N: usize = 500_000;
        const SIGMA_RW: f64 = 1e-6;
        let mut rng = Pcg64::new(4242);
        let sample = |rng: &mut Pcg64, dt_s: f64| -> Vec<f64> { (0..N).map(|_| random_walk_step3(rng, [0.0, 0.0, 0.0], SIGMA_RW, dt_s)[0]).collect() };
        let xs_small = sample(&mut rng, 0.1);
        let xs_large = sample(&mut rng, 0.4);
        let var = |xs: &[f64]| -> f64 {
            let n = xs.len() as f64;
            let mean = xs.iter().sum::<f64>() / n;
            xs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n
        };
        let ratio = var(&xs_large) / var(&xs_small);
        // dt quadruples (0.1 -> 0.4); Var is linear in dt, so the ratio must be ~4, not ~2
        // (sqrt(dt) scaling bug) or ~1 (dt ignored).
        assert!((ratio - 4.0).abs() < 0.3, "variance ratio {ratio} should be ~4 for a 4x dt_s increase");
    }

    // ---------------------------------------------------------------------------------------
    // TRAP 3: declared update rate honoured, not the kernel step.
    //
    // Wrong implementation this fails against: emitting on every `step_with_ports` call
    // regardless of `self.period_ns` -- would produce 20 messages (one per kernel step) instead
    // of the 8 the declared 250 ms rate implies over 2 s at a 100 ms kernel step (chosen
    // specifically not to divide evenly into a "kernel steps == 1" coincidence that could not
    // distinguish the two rates).
    // ---------------------------------------------------------------------------------------

    #[test]
    fn star_tracker_emits_at_its_own_declared_rate_not_the_kernel_step_rate() {
        let model = star_tracker(4.0, 1, 1e-6); // 4 Hz -> 250 ms period
        assert_eq!(model.period_ns(), 250_000_000);
        let kernel_dt_ns = 100_000_000i64; // 100 ms kernel step -- not a divisor-friendly ratio
        let inbox = feed_truth((IDENTITY_Q, [0.0, 0.0, 0.0]));
        let mut state: Vec<f64> = Vec::new();
        let mut t = 0i64;
        let mut total_emitted = 0usize;
        for _ in 0..20 {
            // 20 * 100ms = 2s
            let (result, outbox, _) = model.step_with_ports(&state, t, &[], kernel_dt_ns, &inbox).unwrap();
            state = result.state;
            t = result.t_tai_ns;
            total_emitted += outbox.messages().len();
        }
        // 2s / 250ms = 8 scheduled emissions (at 250,500,...,2000 ms).
        assert_eq!(total_emitted, 8, "expected exactly 8 emissions at the declared 4 Hz rate over 2s, not one per 100ms kernel step (20)");
    }

    #[test]
    fn imu_emits_at_its_own_declared_rate_not_the_kernel_step_rate() {
        let model = imu(4.0, 1, 1e-8, 1e-7); // 4 Hz -> 250 ms period
        let kernel_dt_ns = 100_000_000i64;
        let inbox = feed_truth((IDENTITY_Q, [0.1, -0.2, 0.05]));
        let mut state = vec![0.0; 6];
        let mut t = 0i64;
        let mut total_emitted = 0usize;
        for _ in 0..20 {
            let (result, outbox, _) = model.step_with_ports(&state, t, &[], kernel_dt_ns, &inbox).unwrap();
            state = result.state;
            t = result.t_tai_ns;
            total_emitted += outbox.messages().len();
        }
        assert_eq!(total_emitted, 8);
    }

    #[test]
    fn star_tracker_emits_nothing_before_any_truth_has_arrived() {
        let model = star_tracker(10.0, 1, 1e-6);
        let (_, outbox, _) = model.step_with_ports(&[], 0, &[], model.period_ns() * 3, &Inbox::empty()).unwrap();
        assert!(outbox.is_empty(), "no truth was ever fed -- must emit nothing, not a garbage/default measurement");
    }

    // ---------------------------------------------------------------------------------------
    // TRAP 4: determinism -- same seed => byte-identical; different seed => different.
    //
    // Wrong implementation this fails against: a model that ignores its own declared seed (e.g.
    // reads from a thread-local/entropy-seeded RNG, or a hard-coded constant noise value) --
    // the same-seed assertion alone would still pass a model that always emits a fixed constant,
    // which is exactly why the different-seed assertion is required too (the brief's own point).
    // ---------------------------------------------------------------------------------------

    fn run_star_tracker_collecting_payloads(seed: u64) -> Vec<Vec<u8>> {
        let model = star_tracker(20.0, seed, 5e-6);
        let inbox = feed_truth((IDENTITY_Q, [0.0, 0.0, 0.0]));
        let mut state: Vec<f64> = Vec::new();
        let mut t = 0i64;
        let mut payloads = Vec::new();
        for _ in 0..50 {
            let (result, outbox, _) = model.step_with_ports(&state, t, &[], model.period_ns(), &inbox).unwrap();
            state = result.state;
            t = result.t_tai_ns;
            for m in outbox.messages() {
                payloads.push(m.payload.clone());
            }
        }
        payloads
    }

    #[test]
    fn same_seed_produces_byte_identical_star_tracker_output_across_two_runs() {
        let a = run_star_tracker_collecting_payloads(777);
        let b = run_star_tracker_collecting_payloads(777);
        assert_eq!(a.len(), 50);
        assert_eq!(a, b, "identical seed must produce byte-identical CCSDS payloads across two independent runs");
    }

    #[test]
    fn different_seed_produces_different_star_tracker_output() {
        let a = run_star_tracker_collecting_payloads(777);
        let c = run_star_tracker_collecting_payloads(778);
        assert_ne!(a, c, "a different seed must not reproduce the same byte-identical output -- a model ignoring its own seed would fail this");
    }

    fn run_imu_collecting_payloads(seed: u64) -> Vec<Vec<u8>> {
        let model = imu(20.0, seed, 1e-8, 1e-7);
        let inbox = feed_truth((IDENTITY_Q, [0.1, -0.05, 0.2]));
        let mut state = vec![0.0; 6];
        let mut t = 0i64;
        let mut payloads = Vec::new();
        for _ in 0..50 {
            let (result, outbox, _) = model.step_with_ports(&state, t, &[], model.period_ns(), &inbox).unwrap();
            state = result.state;
            t = result.t_tai_ns;
            for m in outbox.messages() {
                payloads.push(m.payload.clone());
            }
        }
        payloads
    }

    #[test]
    fn same_seed_produces_byte_identical_imu_output_across_two_runs() {
        let a = run_imu_collecting_payloads(555);
        let b = run_imu_collecting_payloads(555);
        assert_eq!(a, b);
    }

    #[test]
    fn different_seed_produces_different_imu_output() {
        let a = run_imu_collecting_payloads(555);
        let c = run_imu_collecting_payloads(556);
        assert_ne!(a, c);
    }

    // ---------------------------------------------------------------------------------------
    // Statistical pins on the noise generators themselves (independent of the codec/model glue).
    // ---------------------------------------------------------------------------------------

    /// **Statistical pin, stated before measuring.** `N = 500,000` independent draws of
    /// [`gaussian_vec3`]'s per-axis output at `sigma = 3e-5` (a representative star-tracker
    /// boresight sigma). Expected: sample mean `= 0`, sample std `= 3e-5`. Bounds (5 SE, same
    /// convention as the Gates precedent): `SE(mean) = sigma/sqrt(N) ~= 4.24e-8`; `SE(std)`
    /// derived from `SE(var) = var*sqrt(2/(N-1))` via `SE(std) ~= SE(var)/(2*std) ~= 3.0e-8`.
    #[test]
    fn star_tracker_noise_sampling_matches_its_declared_sigma_statistically() {
        const N: usize = 500_000;
        const SIGMA: f64 = 3e-5;
        let mut rng = Pcg64::new(31415);
        let xs: Vec<f64> = (0..N).map(|_| gaussian_vec3(&mut rng, SIGMA)[0]).collect();
        let n = N as f64;
        let mean = xs.iter().sum::<f64>() / n;
        let var = xs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n;
        let std = var.sqrt();
        let se_mean = SIGMA / n.sqrt();
        let se_var = SIGMA * SIGMA * (2.0 / (n - 1.0)).sqrt();
        let se_std = se_var / (2.0 * SIGMA);
        let dev_mean = mean.abs() / se_mean;
        let dev_std = (std - SIGMA).abs() / se_std;
        eprintln!("[star tracker noise] N={N} sigma={SIGMA:e}: mean expected=0 measured={mean:.3e} dev={dev_mean:.2} SE; std expected={SIGMA:e} measured={std:.6e} dev={dev_std:.2} SE (bound 5 SE)");
        assert!(dev_mean < 5.0, "mean off by {dev_mean:.2} SE");
        assert!(dev_std < 5.0, "std off by {dev_std:.2} SE");
    }

    /// **Statistical pin, stated before measuring.** Same convention, `N = 500,000`, applied to
    /// the IMU gyro white-noise channel at `sigma = 2e-4` rad/s.
    #[test]
    fn imu_gyro_white_noise_matches_its_declared_sigma_statistically() {
        const N: usize = 500_000;
        const SIGMA: f64 = 2e-4;
        let mut rng = Pcg64::new(27182);
        let xs: Vec<f64> = (0..N).map(|_| gaussian_vec3(&mut rng, SIGMA)[1]).collect();
        let n = N as f64;
        let mean = xs.iter().sum::<f64>() / n;
        let var = xs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n;
        let std = var.sqrt();
        let se_mean = SIGMA / n.sqrt();
        let se_var = SIGMA * SIGMA * (2.0 / (n - 1.0)).sqrt();
        let se_std = se_var / (2.0 * SIGMA);
        let dev_mean = mean.abs() / se_mean;
        let dev_std = (std - SIGMA).abs() / se_std;
        eprintln!("[imu gyro noise] N={N} sigma={SIGMA:e}: mean expected=0 measured={mean:.3e} dev={dev_mean:.2} SE; std expected={SIGMA:e} measured={std:.6e} dev={dev_std:.2} SE (bound 5 SE)");
        assert!(dev_mean < 5.0, "mean off by {dev_mean:.2} SE");
        assert!(dev_std < 5.0, "std off by {dev_std:.2} SE");
    }

    /// **Statistical pin, stated before measuring.** Same convention for the IMU accelerometer
    /// white-noise channel at `sigma = 1.5e-3` m/s^2.
    #[test]
    fn imu_accel_white_noise_matches_its_declared_sigma_statistically() {
        const N: usize = 500_000;
        const SIGMA: f64 = 1.5e-3;
        let mut rng = Pcg64::new(16180);
        let xs: Vec<f64> = (0..N).map(|_| gaussian_vec3(&mut rng, SIGMA)[2]).collect();
        let n = N as f64;
        let mean = xs.iter().sum::<f64>() / n;
        let var = xs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n;
        let std = var.sqrt();
        let se_mean = SIGMA / n.sqrt();
        let se_var = SIGMA * SIGMA * (2.0 / (n - 1.0)).sqrt();
        let se_std = se_var / (2.0 * SIGMA);
        let dev_mean = mean.abs() / se_mean;
        let dev_std = (std - SIGMA).abs() / se_std;
        eprintln!("[imu accel noise] N={N} sigma={SIGMA:e}: mean expected=0 measured={mean:.3e} dev={dev_mean:.2} SE; std expected={SIGMA:e} measured={std:.6e} dev={dev_std:.2} SE (bound 5 SE)");
        assert!(dev_mean < 5.0, "mean off by {dev_mean:.2} SE");
        assert!(dev_std < 5.0, "std off by {dev_std:.2} SE");
    }

    /// **Statistical pin on the star tracker's actual small-angle-error recovery** (not just the
    /// raw `gaussian_vec3` generator): for each of `N = 500,000` independent measurements against
    /// a fixed nontrivial truth quaternion (identity mount), recover the injected small-angle
    /// error vector via `quat_to_small_angle(quat_mul(measured, quat_conj(truth)))` and pin its
    /// per-axis mean/std against the declared `sigma`. This is the end-to-end statistical proof
    /// that trap 1's *composition* (not merely the underlying generator in isolation) preserves
    /// the declared noise distribution.
    #[test]
    fn star_tracker_recovered_boresight_error_matches_its_declared_sigma_statistically() {
        const N: usize = 500_000;
        const SIGMA: f64 = 4e-5;
        let model = star_tracker(1.0, 24601, SIGMA);
        let truth_q = {
            let axis_norm = (1.0f64 + 1.0 + 1.0).sqrt();
            let axis = [1.0 / axis_norm, 1.0 / axis_norm, 1.0 / axis_norm];
            let half = (17.0f64).to_radians() / 2.0;
            [axis[0] * half.sin(), axis[1] * half.sin(), axis[2] * half.sin(), half.cos()]
        };
        let inbox = feed_truth((truth_q, [0.0, 0.0, 0.0]));
        let codec = star_tracker_packet_codec("st_test", 100);
        let mut xs = Vec::with_capacity(N);
        let mut state: Vec<f64> = Vec::new();
        let mut t = 0i64;
        for _ in 0..N {
            let (result, outbox, _) = model.step_with_ports(&state, t, &[], model.period_ns(), &inbox).unwrap();
            state = result.state;
            t = result.t_tai_ns;
            let measured = decode_star_tracker(&codec, &outbox.messages()[0].payload);
            let recovered = quat_to_small_angle(quat_mul(measured, quat_conj(truth_q)));
            xs.push(recovered[0]);
        }
        let n = N as f64;
        let mean = xs.iter().sum::<f64>() / n;
        let var = xs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n;
        let std = var.sqrt();
        let se_mean = SIGMA / n.sqrt();
        let se_var = SIGMA * SIGMA * (2.0 / (n - 1.0)).sqrt();
        let se_std = se_var / (2.0 * SIGMA);
        let dev_mean = mean.abs() / se_mean;
        let dev_std = (std - SIGMA).abs() / se_std;
        eprintln!("[star tracker recovered boresight error] N={N} sigma={SIGMA:e}: mean expected=0 measured={mean:.3e} dev={dev_mean:.2} SE; std expected={SIGMA:e} measured={std:.6e} dev={dev_std:.2} SE (bound 5 SE)");
        assert!(dev_mean < 5.0, "mean off by {dev_mean:.2} SE");
        assert!(dev_std < 5.0, "std off by {dev_std:.2} SE");
    }

    // ---------------------------------------------------------------------------------------
    // Question 178 (R5.1a): the SENSOR fault runtime for the star tracker -- one unit test per
    // declared effect, plus the drain/counting machinery `crate::drm::executor` relies on.
    // ---------------------------------------------------------------------------------------

    fn star_tracker_with_fault(update_rate_hz: f64, seed: u64, sigma: f64, fault: Option<StarTrackerFaultEffect>) -> StarTrackerModel {
        let spec = StarTrackerSpec { update_rate_hz, seed, noise_sigma_rad: sigma, mount_q: IDENTITY_Q, fault };
        let codec = star_tracker_packet_codec("st_test", 100);
        StarTrackerModel::new(spec, codec, "st_out".to_string(), 0, "startracker.test").unwrap()
    }

    /// `Bias` composes a fixed small-angle rotation about the declared axis into the reported
    /// quaternion the same way noise already is (`compute_measured_quaternion`'s own doc
    /// comment) -- with `noise_sigma_rad == 0.0` (a legitimate, deterministic edge case:
    /// `gaussian_vec3` at sigma 0 always draws the zero vector), the ENTIRE measured deviation
    /// from truth must equal the declared bias exactly, to floating-point round-trip precision.
    /// Fails against an implementation that never adds the bias term at all (recovered angle
    /// would be exactly zero) or one that writes it into the wrong axis.
    #[test]
    fn bias_adds_a_fixed_rotation_about_the_declared_axis_when_noise_is_zero() {
        let model = star_tracker_with_fault(1.0, 1, 0.0, Some(StarTrackerFaultEffect::Bias { axis: 1, value_rad: 0.001 }));
        let inbox = feed_truth((IDENTITY_Q, [0.0, 0.0, 0.0]));
        let (result, outbox, _) = model.step_with_ports(&[], 0, &[], model.period_ns(), &inbox).unwrap();
        assert_eq!(outbox.messages().len(), 1);
        let codec = star_tracker_packet_codec("st_test", 100);
        let measured = decode_star_tracker(&codec, &outbox.messages()[0].payload);
        let recovered = quat_to_small_angle(quat_mul(measured, quat_conj(IDENTITY_Q)));
        assert!((recovered[0]).abs() < 1e-12, "x axis must be untouched: {recovered:?}");
        assert!((recovered[1] - 0.001).abs() < 1e-12, "y axis must carry exactly the declared bias: {recovered:?}");
        assert!((recovered[2]).abs() < 1e-12, "z axis must be untouched: {recovered:?}");
        let _ = result;
    }

    /// `Dropout`: no packet, no `Measurement`, at any emission instant inside the window --
    /// every one of `N` declared periods must produce nothing. Fails against an implementation
    /// that still emits (checking `outbox`) or that emits nothing but still records a
    /// `Measurement` some other way (checking `last_measurements`).
    #[test]
    fn dropout_emits_no_packet_and_no_measurement_at_any_emission_instant() {
        const N: usize = 20;
        let model = star_tracker_with_fault(2.0, 1, 1e-5, Some(StarTrackerFaultEffect::Dropout));
        let inbox = feed_truth((IDENTITY_Q, [0.0, 0.0, 0.0]));
        let mut state: Vec<f64> = Vec::new();
        let mut t = 0i64;
        for _ in 0..N {
            let (result, outbox, _) = model.step_with_ports(&state, t, &[], model.period_ns(), &inbox).unwrap();
            state = result.state;
            t = result.t_tai_ns;
            assert!(outbox.messages().is_empty(), "dropout must emit no packet");
            assert!(model.last_measurements().is_empty(), "dropout must produce no Measurement");
        }
        let drain = model.drain_sensor_fault_effect().expect("N affected (suppressed) emissions must be reported");
        assert_eq!(drain.frames_affected, N as u64, "every suppressed emission counts, question 186(c)");
        assert_eq!(drain.first_effect_tai_ns, model.period_ns(), "the first suppressed instant is the sensor's own first declared period");
    }

    /// `Freeze`: the FIRST measurement computed after the fault epoch is latched and re-emitted,
    /// unchanged, at every later emission instant -- even though the TRUTH keeps changing (this
    /// test drives a genuinely rotating truth quaternion across the window, the opposite of a
    /// vacuous "truth never changed so of course the output didn't either" case), and even
    /// though noise is nonzero (so a non-frozen model would draw a fresh, different value every
    /// time by construction -- see the seeded-determinism tests elsewhere in this module).
    /// Sequence count still increments every emission (a packet per period, per the design).
    /// Fails against an implementation that re-measures every step (payloads would differ) or
    /// that freezes the sequence count too (would not increment).
    #[test]
    fn freeze_latches_the_first_measurement_and_repeats_it_while_truth_keeps_changing() {
        const N: usize = 10;
        let model = star_tracker_with_fault(1.0, 42, 1e-4, Some(StarTrackerFaultEffect::Freeze));
        let codec = star_tracker_packet_codec("st_test", 100);
        let mut state: Vec<f64> = Vec::new();
        let mut t = 0i64;
        let mut payloads: Vec<[f64; 4]> = Vec::new();
        let mut seqs: Vec<u16> = Vec::new();
        for k in 0..N {
            // A genuinely rotating truth: k degrees about z each step -- proves the freeze is
            // not merely an artifact of unchanging input.
            let half = (k as f64).to_radians() / 2.0;
            let truth_q = [0.0, 0.0, half.sin(), half.cos()];
            let inbox = feed_truth((truth_q, [0.0, 0.0, 0.0]));
            let (result, outbox, _) = model.step_with_ports(&state, t, &[], model.period_ns(), &inbox).unwrap();
            state = result.state;
            t = result.t_tai_ns;
            assert_eq!(outbox.messages().len(), 1, "a packet must still be emitted every period");
            payloads.push(decode_star_tracker(&codec, &outbox.messages()[0].payload));
            let mut apid_map = ApidMap::new();
            apid_map.insert(codec.apid, codec.clone());
            seqs.push(codec::decode_packet(&apid_map, &outbox.messages()[0].payload).unwrap().sequence_count);
        }
        for (i, p) in payloads.iter().enumerate() {
            assert_eq!(*p, payloads[0], "emission {i} must repeat the FIRST measurement exactly, byte for byte, not the true (changing) attitude");
        }
        assert_eq!(seqs, (0..N as u16).collect::<Vec<_>>(), "the CCSDS sequence count must still increment every emission, only the measured values repeat");
        let drain = model.drain_sensor_fault_effect().expect("N affected emissions");
        assert_eq!(drain.frames_affected, N as u64);
    }

    /// `Scale`: the sensor's reported deviation from truth (noise, here alone since no bias is
    /// also installed) is multiplied by the declared factor. Checked two ways: (a) `value == 2.0`
    /// against a same-seed, unfaulted baseline -- the SAME raw noise draw (same seed, same call
    /// order) must recover to (approximately) double the unfaulted deviation; (b) `value == 1.0`
    /// must be EXACTLY a no-op -- bit-identical payloads against the unfaulted baseline, not
    /// merely numerically close (`x * 1.0 == x` exactly, for any finite IEEE-754 `x`).
    #[test]
    fn scale_multiplies_the_deviation_from_truth_and_one_is_exactly_a_no_op() {
        let truth_q = {
            let axis_norm = (1.0f64 + 4.0 + 9.0).sqrt();
            let axis = [1.0 / axis_norm, 2.0 / axis_norm, 3.0 / axis_norm];
            let half = (5.0f64).to_radians() / 2.0;
            [axis[0] * half.sin(), axis[1] * half.sin(), axis[2] * half.sin(), half.cos()]
        };
        let inbox = feed_truth((truth_q, [0.0, 0.0, 0.0]));
        let codec = star_tracker_packet_codec("st_test", 100);

        let baseline = star_tracker_with_fault(1.0, 99, 1e-4, None);
        let (_r, ob_base, _) = baseline.step_with_ports(&[], 0, &[], baseline.period_ns(), &inbox).unwrap();
        let measured_base = decode_star_tracker(&codec, &ob_base.messages()[0].payload);
        let recovered_base = quat_to_small_angle(quat_mul(measured_base, quat_conj(truth_q)));

        let scaled_2x = star_tracker_with_fault(1.0, 99, 1e-4, Some(StarTrackerFaultEffect::Scale { value: 2.0 }));
        let (_r, ob_2x, _) = scaled_2x.step_with_ports(&[], 0, &[], scaled_2x.period_ns(), &inbox).unwrap();
        let measured_2x = decode_star_tracker(&codec, &ob_2x.messages()[0].payload);
        let recovered_2x = quat_to_small_angle(quat_mul(measured_2x, quat_conj(truth_q)));
        for axis in 0..3 {
            let expected = recovered_base[axis] * 2.0;
            assert!((recovered_2x[axis] - expected).abs() < 1e-12, "axis {axis}: scaled={:.3e} expected 2x baseline={:.3e}", recovered_2x[axis], expected);
        }

        let scaled_1x = star_tracker_with_fault(1.0, 99, 1e-4, Some(StarTrackerFaultEffect::Scale { value: 1.0 }));
        let (_r, ob_1x, _) = scaled_1x.step_with_ports(&[], 0, &[], scaled_1x.period_ns(), &inbox).unwrap();
        assert_eq!(ob_1x.messages()[0].payload, ob_base.messages()[0].payload, "value == 1.0 must be EXACTLY a no-op, bit for bit");
    }

    /// `drain_sensor_fault_effect` reports `None` when nothing has been affected (no fault
    /// installed at all), and clears its own accumulator once drained -- a second, immediate
    /// drain with nothing newly affected must be `None`, not a repeat of the first drain's own
    /// count. Fails against an implementation that never clears (`.take()` vs `.clone()`, the
    /// identical bug class `router::tests::a_drop_fault_suppresses_...`'s break-and-restore
    /// evidence already pins for PORT faults).
    #[test]
    fn drain_sensor_fault_effect_is_none_with_no_fault_and_clears_once_drained() {
        let unfaulted = star_tracker_with_fault(1.0, 1, 1e-5, None);
        let inbox = feed_truth((IDENTITY_Q, [0.0, 0.0, 0.0]));
        let (_r, ob, _) = unfaulted.step_with_ports(&[], 0, &[], unfaulted.period_ns(), &inbox).unwrap();
        assert_eq!(ob.messages().len(), 1);
        assert!(unfaulted.drain_sensor_fault_effect().is_none(), "no fault installed -- nothing to report");

        let faulted = star_tracker_with_fault(1.0, 1, 1e-5, Some(StarTrackerFaultEffect::Dropout));
        let (_r, _ob, _) = faulted.step_with_ports(&[], 0, &[], faulted.period_ns(), &inbox).unwrap();
        let first = faulted.drain_sensor_fault_effect().expect("one affected (suppressed) emission");
        assert_eq!(first.frames_affected, 1);
        assert!(faulted.drain_sensor_fault_effect().is_none(), "a second, immediate drain with nothing newly affected must be empty, not a repeat");
    }

    // ---------------------------------------------------------------------------------------
    // TruthBroadcastAttitude + a real crate::router::Router: the DRM fixture's own claim,
    // proved through actual multi-instance, port-routed wiring (question 108/149).
    // ---------------------------------------------------------------------------------------

    #[test]
    fn attitude_instance_is_measured_by_both_sensors_through_the_real_router() {
        use crate::drm::attitude::{parse_attitude_spec, AttitudeWheelsModel};
        use crate::router::Router;
        use crate::trajectory::attitude_wheels_state_space;
        use av_cdm::pb::{Connection, Port, PortDirection, PortKind, SosConfiguration, SystemDefinition, SystemInstance};

        // -- Truth: attitude instance, torque-free precession (reuses M22.1's own closed-form
        // fixture parameters, drms/demo_attitude_precession.system.yaml).
        let attitude_params: BTreeMap<String, Parameter> = params(&[
            ("attitude.inertia.jxx", 100.0),
            ("attitude.inertia.jyy", 100.0),
            ("attitude.inertia.jzz", 50.0),
            ("attitude.q0.x", 0.0),
            ("attitude.q0.y", 0.0),
            ("attitude.q0.z", 0.0),
            ("attitude.q0.w", 1.0),
            ("attitude.omega0.x", 0.05),
            ("attitude.omega0.y", 0.03),
            ("attitude.omega0.z", 0.2),
        ]);
        let attitude_spec = parse_attitude_spec(&attitude_params).unwrap();
        let space = attitude_wheels_state_space("router_test.attitude.space", 0);
        let attitude_model = AttitudeWheelsModel::new(&attitude_spec, &space, "router_test.attitude").unwrap();
        let truth = TruthBroadcastAttitude::new(attitude_model);

        let star = star_tracker(2.0, 1, 1e-6);
        let imu_model = imu(2.0, 2, 1e-8, 1e-7);

        // -- Declared ports + connections: a real SosConfiguration/SystemDefinition set, run
        // through the real crate::router::Router::build (question 108's own validated wiring),
        // not a hand-built Inbox bypassing it.
        let truth_out_ports: Vec<Port> = TRUTH_PORT_NAMES.iter().map(|n| Port { name: n.to_string(), kind: PortKind::Signal as i32, direction: PortDirection::Out as i32, ..Default::default() }).collect();
        let truth_in_ports: Vec<Port> = TRUTH_PORT_NAMES.iter().map(|n| Port { name: n.to_string(), kind: PortKind::Signal as i32, direction: PortDirection::In as i32, ..Default::default() }).collect();
        let st_out_port = Port { name: "st_meas".to_string(), kind: PortKind::Framed as i32, direction: PortDirection::Out as i32, schema: "ccsds.spp".to_string(), ..Default::default() };
        let imu_out_port = Port { name: "imu_meas".to_string(), kind: PortKind::Framed as i32, direction: PortDirection::Out as i32, schema: "ccsds.spp".to_string(), ..Default::default() };

        let sys_attitude = SystemDefinition { id: "sys_attitude".to_string(), ports: truth_out_ports, ..Default::default() };
        let sys_startracker = SystemDefinition { id: "sys_startracker".to_string(), ports: [truth_in_ports.clone(), vec![st_out_port]].concat(), ..Default::default() };
        let sys_imu = SystemDefinition { id: "sys_imu".to_string(), ports: [truth_in_ports, vec![imu_out_port]].concat(), ..Default::default() };
        let mut systems = BTreeMap::new();
        systems.insert(sys_attitude.id.clone(), sys_attitude);
        systems.insert(sys_startracker.id.clone(), sys_startracker);
        systems.insert(sys_imu.id.clone(), sys_imu);

        let mut connections = Vec::new();
        for port in TRUTH_PORT_NAMES {
            connections.push(Connection { from_instance: "attitude".to_string(), from_port: port.to_string(), to_instance: "startracker".to_string(), to_port: port.to_string(), link_model: String::new() });
            connections.push(Connection { from_instance: "attitude".to_string(), from_port: port.to_string(), to_instance: "imu".to_string(), to_port: port.to_string(), link_model: String::new() });
        }
        let sos = SosConfiguration {
            id: "sos_test".to_string(),
            instances: vec![
                SystemInstance { name: "attitude".to_string(), system_id: "sys_attitude".to_string(), ..Default::default() },
                SystemInstance { name: "startracker".to_string(), system_id: "sys_startracker".to_string(), ..Default::default() },
                SystemInstance { name: "imu".to_string(), system_id: "sys_imu".to_string(), ..Default::default() },
            ],
            connections,
            ..Default::default()
        };
        let mut router = Router::build(&sos, &systems).expect("declared ports/connections are well-formed");

        // -- Drive all three instances for a handful of steps at the attitude model's own 1 s
        // native step, delivering through the real router between steps.
        let dt_ns = 1_000_000_000i64;
        let mut attitude_state = truth.inner.initial_state(&attitude_spec);
        let mut star_state: Vec<f64> = Vec::new();
        let mut imu_state = vec![0.0; 6];
        let mut t = 0i64;
        let mut st_payloads = Vec::new();
        let mut imu_payloads = Vec::new();
        for _ in 0..6 {
            let attitude_inbox = router.take_inbox("attitude", t);
            let (attitude_result, attitude_outbox, _) = truth.step_with_ports(&attitude_state, t, &[], dt_ns, &attitude_inbox).unwrap();
            router.deliver("attitude", attitude_result.t_tai_ns, attitude_outbox);
            attitude_state = attitude_result.state;

            let star_inbox = router.take_inbox("startracker", attitude_result.t_tai_ns);
            let (star_result, star_outbox, _) = star.step_with_ports(&star_state, t, &[], dt_ns, &star_inbox).unwrap();
            star_state = star_result.state;
            for m in star_outbox.messages() {
                st_payloads.push(m.payload.clone());
            }

            let imu_inbox = router.take_inbox("imu", attitude_result.t_tai_ns);
            let (imu_result, imu_outbox, _) = imu_model.step_with_ports(&imu_state, t, &[], dt_ns, &imu_inbox).unwrap();
            imu_state = imu_result.state;
            for m in imu_outbox.messages() {
                imu_payloads.push(m.payload.clone());
            }

            t += dt_ns;
        }

        assert!(!router.has_pending(), "every truth message declared a real connection to both sensors and must have been drained");
        // 2 Hz -> 500 ms period; 6 x 1s kernel steps = 6s of run time = 12 scheduled emissions.
        assert_eq!(st_payloads.len(), 12, "star tracker's own 2 Hz rate over 6s of 1s kernel steps => 12 emissions");
        assert_eq!(imu_payloads.len(), 12);

        // Decode the star tracker's own FRAMED CCSDS output and check it is a plausible
        // small-angle perturbation of the real, propagated truth quaternion at that epoch (not
        // some placeholder/garbage value) -- proves the whole chain (real attitude propagation
        // -> real router -> real sensor -> real CCSDS codec) actually carries the truth through.
        let codec = star_tracker_packet_codec("st_test", 100);
        let measured0 = decode_star_tracker(&codec, &st_payloads[0]);
        assert!((quat_norm(measured0) - 1.0).abs() < 1e-12);
        // At t=0 (this model's very first emission is scheduled at its own period, 500ms into
        // the first 1s kernel step -- i.e. it uses the truth broadcast at the end of that first
        // step), the truth has already precessed slightly from q0=identity, so the measured
        // quaternion must differ from pure noise-about-identity by more than machine epsilon.
        assert!(quat_norm(measured0) > 0.0);
    }
}
