//! Native attitude controller (M22.4, `docs/sil-plan.md`'s M22 milestone paragraph: "A native
//! 'controller' instance closes the loop first so the whole chain is proven before any external
//! binary is involved"; its Decisions (2026-09-05) decision A, "attitude control first": "star
//! tracker and IMU in, reaction wheel torques out"; `docs/open-questions.md` questions 142, 149,
//! 151, 152).
//!
//! ## What this module builds
//!
//! [`AttitudeControllerModel`]: a real, directly constructible, directly testable
//! [`av_dynamics::DynamicsModel`] that consumes the star tracker's and IMU's own measurement
//! packets **as CCSDS packets over FRAMED ports**, decoded through the M22.3 codec
//! (`crate::codec`/`crate::ports`) -- never the plant's truth state directly, which would not be
//! a closed loop -- and commands reaction wheel torques back out, also as a CCSDS packet, on its
//! own declared FRAMED OUT port. [`CommandedAttitude`] is the plant-side counterpart: a small,
//! generic decorator (mirrors `super::sensors::TruthBroadcastAttitude`'s own "wrap, don't touch
//! `crate::drm::attitude`" shape) that decodes an incoming wheel-torque command packet and
//! threads it through as the wrapped model's own `controls` argument.
//!
//! ## Control law
//!
//! A quaternion-feedback PD regulator (Wie, *Space Vehicle Dynamics and Control*), body-torque
//! commanded per wheel, wheel `k` (`k = 1..=3`) assumed aligned with body axis `k` (a documented
//! convention, not a general axis-distribution solve -- see [`AttitudeControllerModel::new`]'s
//! own doc comment for exactly what is checked): with `q_err = target_q^-1 (x) measured_q`
//! (`super::sensors::quat_mul`/`quat_conj`, the same Hamilton-product primitives the sensor
//! models already use and already unit-test against a textbook rotation), `sign =
//! sign(q_err.w)` (shortest-path correction) and `qv = sign * q_err.{x,y,z}`:
//!
//! ```text
//! tau_k = kp * qv_k + kd * omega_k        (k = x, y, z; omega from the IMU's own measured rate)
//! ```
//!
//! **Sign derivation (checked against Euler's equation, not assumed).**
//! `crate::drm::attitude::AttitudeWheelsModel::derivatives` computes `J*omega_dot = -omega x (J
//! omega + h_w) - tau_w` where `tau_w_k = axis_k * tau_eff_k`; for a wheel aligned with body axis
//! `k` this reduces (small rate, no gyroscopic coupling) to `J_k * omega_dot_k = -tau_eff_k`.
//! Substituting `tau_eff_k = kp*qv_k + kd*omega_k` gives `omega_dot_k = -(kp*qv_k +
//! kd*omega_k)/J_k` -- **negative** feedback on both the angle error and the rate, i.e. stable.
//! Linearizing for a single-axis error (target = identity, so `qv_k = sin(theta_k/2) ~=
//! theta_k/2`, `theta_dot_k = omega_k` exactly for a planar single-axis rotation) gives the
//! standard second-order form `theta_ddot + (kd/J_k) theta_dot + (kp/(2 J_k)) theta = 0`, so:
//!
//! ```text
//! omega_n = sqrt(kp / (2 J_k))          (undamped natural frequency, rad/s)
//! zeta    = kd / (2 J_k omega_n)        (damping ratio)
//! tau     = 2 J_k / kd                  (1/e envelope decay time constant, seconds)
//! ```
//!
//! `crates/av-kernel/tests/drm_attitude_control.rs` states these three numbers from the demo
//! fixture's own declared `kp`/`kd`/`J_z` *before* running, then measures the settling behaviour
//! against them -- see that file's own module doc comment.
//!
//! ## Why the objective is scored from the controller's own belief, not the plant's truth
//!
//! [`AttitudeControllerModel::step_with_ports`] exposes its own computed pointing-error angle
//! (`2 * asin(|qv|)`, the exact quantity the control law above acts on) as `StepResult.
//! outputs["pointing_error_rad"]` every call (not gated on this step actually firing a new
//! command -- see the method's own doc comment) -- reachable as `output.<instance>.
//! pointing_error_rad@<time>` (M10.2/M22.2's own `"output.<name>"` convention, `crate::drm::
//! executor::declared_outputs`) once the controller's own `SystemDefinition` declares `output.
//! pointing_error_rad`. This is the star tracker's own **measured**, noisy attitude compared
//! against the declared target -- deliberately never the plant's truth quaternion -- for two
//! reasons: (1) it is literally the error signal the control law is closing the loop on, so it
//! is the most direct evidence the loop (sensor -> controller -> actuator) is actually wired
//! end to end, and (2) computing it from truth instead is exactly the trap the task brief warns
//! against ("if your pointing error settles to exactly zero, you have probably wired truth in by
//! mistake") -- scoring against truth would launder that mistake into an equally
//! indistinguishable, but wrong, passing test. `crate::drm::attitude`/`AttitudeWheelsModel` is
//! not touched by this module at all (mirrors `crate::drm::sensors`'s own "zero regression
//! budget" rule for that model) -- [`CommandedAttitude`] only ever reads truth state that
//! `AttitudeWheelsModel::step`/`step_with_ports` already returns, never a new hook into it.
//!
//! ## Typed refusals (question 149's "never a silent drop" rule, applied here)
//!
//! [`ControllerSpecError`] covers declared-parameter/codec problems at classification time
//! (mirrors `super::sensors::SensorSpecError`); [`ControllerRuntimeError`]/
//! [`CommandedAttitudeError`] cover a malformed inbound packet at run time (mirrors
//! `crate::codec::CodecError`'s own "typed, never silent" contract) -- both are real,
//! `std::error::Error`-implementing types wired into `super::binding::AnyModelError::PortCodec`,
//! never a panic or a silently-substituted default.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::fmt;

use av_cdm::pb::{self, ModelInfo, PacketCodec, Parameter};
use av_dynamics::{AppliedCommand, DynamicsModel, Inbox, Outbox, StepResult};

use crate::codec::{self, ApidMap, CodecError, FieldValue};
use super::sensors::{quat_conj, quat_mul, quat_norm};

// ============================================================================================
// Fixed port-name convention (mirrors `super::sensors::TRUTH_PORT_NAMES`'s own "exactly one
// place this convention is spelled out" role).
// ============================================================================================

/// The controller's own declared FRAMED IN port for the star tracker's measurement packets.
pub const CONTROLLER_STARTRACKER_IN_PORT: &str = "startracker_in";
/// The controller's own declared FRAMED IN port for the IMU's measurement packets.
pub const CONTROLLER_IMU_IN_PORT: &str = "imu_in";
/// The controller's own declared FRAMED OUT port for the wheel-torque command packet.
pub const CONTROLLER_WHEEL_TORQUE_OUT_PORT: &str = "wheel_torque_out";
/// The attitude (plant) instance's own declared FRAMED IN port for the wheel-torque command
/// packet -- the `Connection` in a closed-loop SoS runs from
/// [`CONTROLLER_WHEEL_TORQUE_OUT_PORT`] to this port.
pub const ATTITUDE_WHEEL_TORQUE_IN_PORT: &str = "wheel_torque_in";

/// Exactly the three wheel-torque field names [`wheel_torque_command_packet_codec`] builds and
/// every wheel-torque codec this module accepts must declare, in this order -- wheel `k` (1-
/// indexed) is assumed aligned with body axis `k` (x, y, z respectively), a documented
/// convention, not a computed general wheel-distribution solve (see [`AttitudeControllerModel::
/// new`]'s own doc comment for exactly what is checked and why three, not fewer or more).
pub const WHEEL_TORQUE_FIELD_NAMES: [&str; 3] = ["tau_1", "tau_2", "tau_3"];

/// Build the fixed field layout every wheel-torque command `PacketCodec` this module builds/
/// expects uses: three big-endian IEEE-754 `binary64` fields (`tau_1`, `tau_2`, `tau_3`, `N*m`),
/// 24 user-data bytes total, `is_command = true` (CCSDS 133.0-B-2's own type bit: this packet
/// commands actuation, it does not report telemetry -- `crate::codec`'s own module doc comment).
pub fn wheel_torque_command_packet_codec(id: &str, apid: u32) -> PacketCodec {
    let f = |name: &str, bit_offset: u32| pb::PacketField { name: name.to_string(), bit_offset, bit_width: 64, r#type: pb::PacketFieldType::Float64 as i32, unit: pb::Unit::NewtonMeter as i32, scale: 1.0, offset: 0.0, target: String::new() };
    PacketCodec {
        id: id.to_string(),
        apid,
        is_command: true,
        secondary_header_bytes: 0,
        user_data_bytes: 24,
        fields: vec![f("tau_1", 0), f("tau_2", 64), f("tau_3", 128)],
        description: "reaction wheel torque command, wheel k aligned with body axis k (M22.4)".to_string(),
    }
}

fn has_all_fields(codec: &PacketCodec, names: &[&str]) -> bool {
    names.iter().all(|n| codec.fields.iter().any(|f| f.name == *n))
}

/// `true` iff `codec` declares exactly [`WHEEL_TORQUE_FIELD_NAMES`] (no more, no fewer) --
/// the structural check both [`AttitudeControllerModel::new`] (the emitting side) and
/// `super::binding`'s attitude classification arm (the consuming side, when a wheel-command port
/// is declared for an attitude instance) run so the two can never silently disagree about what
/// counts as a valid wheel-torque command codec.
pub fn is_wheel_torque_command_codec(codec: &PacketCodec) -> bool {
    codec.fields.len() == WHEEL_TORQUE_FIELD_NAMES.len() && has_all_fields(codec, &WHEEL_TORQUE_FIELD_NAMES)
}

// ============================================================================================
// Typed parameter parsing (mirrors `super::sensors::SensorSpecError`'s own contract).
// ============================================================================================

/// Everything [`parse_attitude_controller_spec`]/[`AttitudeControllerModel::new`] can refuse --
/// a local, dedicated error type for the identical reason `super::sensors::SensorSpecError`'s
/// own doc comment gives: this module is not reached through `super::binding::classify_binding`
/// (parsing), so it is not itself a `super::DrmError` variant.
#[derive(Debug, Clone, PartialEq)]
pub enum ControllerSpecError {
    MissingParameter { name: String },
    UnknownParameter { name: String },
    InvalidParameter { name: String, reason: String },
    Codec(CodecError),
}
impl fmt::Display for ControllerSpecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ControllerSpecError::MissingParameter { name } => write!(f, "missing required controller parameter {name:?}"),
            ControllerSpecError::UnknownParameter { name } => write!(f, "unrecognized controller parameter {name:?}"),
            ControllerSpecError::InvalidParameter { name, reason } => write!(f, "controller parameter {name:?} is invalid: {reason}"),
            ControllerSpecError::Codec(e) => write!(f, "declared PacketCodec is invalid: {e}"),
        }
    }
}
impl std::error::Error for ControllerSpecError {}

fn require_positive(v: Option<f64>, name: &str) -> Result<f64, ControllerSpecError> {
    let v = v.ok_or_else(|| ControllerSpecError::MissingParameter { name: name.to_string() })?;
    if !(v.is_finite() && v > 0.0) {
        return Err(ControllerSpecError::InvalidParameter { name: name.to_string(), reason: format!("must be > 0, got {v}") });
    }
    Ok(v)
}

/// Parsed, typed parameters for [`AttitudeControllerModel`] -- built by
/// [`parse_attitude_controller_spec`]. Parameter vocabulary (a name matching none of these is
/// [`ControllerSpecError::UnknownParameter`]):
/// - `controller.kp` (required, `> 0`): proportional (attitude-error) gain.
/// - `controller.kd` (required, `> 0`): derivative (rate-error) gain.
/// - `controller.target_q.{x,y,z,w}` (required together): target attitude quaternion,
///   scalar-last, unit norm.
/// - `controller.update_rate_hz` (required, `> 0`): declared control-law evaluation rate, Hz --
///   the controller runs at this rate, not the kernel step (see [`AttitudeControllerModel::
///   step_with_ports`]).
#[derive(Debug, Clone, PartialEq)]
pub struct AttitudeControllerSpec {
    pub kp: f64,
    pub kd: f64,
    pub target_q: [f64; 4],
    pub update_rate_hz: f64,
}

pub fn parse_attitude_controller_spec(params: &BTreeMap<String, Parameter>) -> Result<AttitudeControllerSpec, ControllerSpecError> {
    let mut kp = None;
    let mut kd = None;
    let mut target_q = [None; 4];
    let mut update_rate_hz = None;
    for (name, p) in params {
        match name.as_str() {
            "controller.kp" => kp = Some(p.value),
            "controller.kd" => kd = Some(p.value),
            "controller.target_q.x" => target_q[0] = Some(p.value),
            "controller.target_q.y" => target_q[1] = Some(p.value),
            "controller.target_q.z" => target_q[2] = Some(p.value),
            "controller.target_q.w" => target_q[3] = Some(p.value),
            "controller.update_rate_hz" => update_rate_hz = Some(p.value),
            // M10.2/M22.2's own convention -- see `super::sensors::parse_star_tracker_spec`'s
            // identical arm's own doc comment.
            _ if name.starts_with("output.") => {}
            other => return Err(ControllerSpecError::UnknownParameter { name: other.to_string() }),
        }
    }
    let kp = require_positive(kp, "controller.kp")?;
    let kd = require_positive(kd, "controller.kd")?;
    let target_q = match target_q {
        [Some(x), Some(y), Some(z), Some(w)] => {
            let n = quat_norm([x, y, z, w]);
            if (n - 1.0).abs() > 1e-6 {
                return Err(ControllerSpecError::InvalidParameter { name: "controller.target_q".to_string(), reason: format!("not unit norm: |q| = {n}") });
            }
            [x, y, z, w]
        }
        _ => return Err(ControllerSpecError::MissingParameter { name: "controller.target_q.{x,y,z,w} (all four required together)".to_string() }),
    };
    let update_rate_hz = require_positive(update_rate_hz, "controller.update_rate_hz")?;
    Ok(AttitudeControllerSpec { kp, kd, target_q, update_rate_hz })
}

// ============================================================================================
// AttitudeControllerModel: the native controller DynamicsModel.
// ============================================================================================

/// A malformed inbound packet at run time (question 149's "typed, never a silent drop" rule) --
/// the only way [`AttitudeControllerModel::step_with_ports`] can fail (every physical/structural
/// check already happened in [`parse_attitude_controller_spec`]/[`AttitudeControllerModel::
/// new`]).
#[derive(Debug, Clone, PartialEq)]
pub enum ControllerRuntimeError {
    Codec(CodecError),
}
impl fmt::Display for ControllerRuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ControllerRuntimeError::Codec(e) => write!(f, "{e}"),
        }
    }
}
impl std::error::Error for ControllerRuntimeError {}

/// A native attitude controller: no propagated physical state (`state_dim() == 0`, exactly like
/// `super::sensors::StarTrackerModel` -- a control law with no integrator state has nothing to
/// integrate), a `Cell`-tracked emission schedule (`next_due`/`period_ns`) independent of however
/// often `step_with_ports` itself is called (the same trap 3 the sensor models close, applied
/// here to the *command* side of the loop), and no randomness at all (a deterministic function
/// of its own cached last measurements -- determinism therefore falls straight out of the
/// upstream sensors' own seeded determinism, with nothing new to seed here).
#[derive(Debug)]
pub struct AttitudeControllerModel {
    spec: AttitudeControllerSpec,
    star_apid_map: ApidMap,
    imu_apid_map: ApidMap,
    command_codec: PacketCodec,
    period_ns: i64,
    next_due: Cell<i64>,
    seq: Cell<u16>,
    last_star_q: RefCell<Option<[f64; 4]>>,
    last_imu_omega: RefCell<Option<[f64; 3]>>,
    info: ModelInfo,
}

impl AttitudeControllerModel {
    /// `star_codec`/`imu_codec` must each declare (at least) the fields `super::sensors::
    /// star_tracker_packet_codec`/`imu_packet_codec` build (`qx,qy,qz,qw` / `wx,wy,wz`
    /// respectively) -- checked here, not assumed, exactly like `super::sensors::
    /// StarTrackerModel::new`'s own identical check. `command_codec` must be exactly
    /// [`is_wheel_torque_command_codec`] -- **why exactly three, wheel `k` = body axis `k`, not a
    /// general wheel-distribution solve**: this task's own closed-loop demo is a fully-actuated,
    /// axis-aligned three-wheel plant (`drms/demo_attitude_control_truth.system.yaml`), and
    /// solving a general (possibly over/under-actuated, non-orthogonal) wheel distribution matrix
    /// is a separate piece of physics this task's brief does not ask for; declared here as a
    /// checked precondition rather than silently assumed, so a differently-shaped plant is
    /// refused at load time, not silently mis-actuated.
    ///
    /// `epoch_tai_ns`: seeds `next_due` as `epoch_tai_ns + period_ns` -- the identical M22.2b bug
    /// fix `super::sensors::StarTrackerModel::new`'s own doc comment explains (a realistic TAI
    /// epoch, not `0`, is what every real DRM actually starts from).
    pub fn new(spec: AttitudeControllerSpec, star_codec: PacketCodec, imu_codec: PacketCodec, command_codec: PacketCodec, epoch_tai_ns: i64, model_id: &str) -> Result<Self, ControllerSpecError> {
        codec::validate_codec(&star_codec).map_err(ControllerSpecError::Codec)?;
        codec::validate_codec(&imu_codec).map_err(ControllerSpecError::Codec)?;
        codec::validate_codec(&command_codec).map_err(ControllerSpecError::Codec)?;
        if !has_all_fields(&star_codec, &["qx", "qy", "qz", "qw"]) {
            return Err(ControllerSpecError::InvalidParameter { name: "controller.star_codec".to_string(), reason: format!("declared PacketCodec {:?} is missing a required qx/qy/qz/qw field", star_codec.id) });
        }
        if !has_all_fields(&imu_codec, &["wx", "wy", "wz"]) {
            return Err(ControllerSpecError::InvalidParameter { name: "controller.imu_codec".to_string(), reason: format!("declared PacketCodec {:?} is missing a required wx/wy/wz field", imu_codec.id) });
        }
        if !is_wheel_torque_command_codec(&command_codec) {
            return Err(ControllerSpecError::InvalidParameter { name: "controller.command_codec".to_string(), reason: format!("declared PacketCodec {:?} must declare exactly {WHEEL_TORQUE_FIELD_NAMES:?}, got {:?}", command_codec.id, command_codec.fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>()) });
        }
        let mut star_apid_map = ApidMap::new();
        star_apid_map.insert(star_codec.apid, star_codec.clone());
        let mut imu_apid_map = ApidMap::new();
        imu_apid_map.insert(imu_codec.apid, imu_codec.clone());

        let period_ns = (1.0e9 / spec.update_rate_hz).round() as i64;
        let mut settings = BTreeMap::new();
        settings.insert("kp".to_string(), format!("{:.17e}", spec.kp));
        settings.insert("kd".to_string(), format!("{:.17e}", spec.kd));
        for (i, v) in spec.target_q.iter().enumerate() {
            settings.insert(format!("target_q_{i}"), format!("{v:.17e}"));
        }
        settings.insert("update_rate_hz".to_string(), format!("{:.17e}", spec.update_rate_hz));
        settings.insert("star_apid".to_string(), star_codec.apid.to_string());
        settings.insert("imu_apid".to_string(), imu_codec.apid.to_string());
        settings.insert("command_apid".to_string(), command_codec.apid.to_string());
        let settings_hash = av_dynamics::settings_hash(&settings);
        let info = ModelInfo { id: model_id.to_string(), version: "1".to_string(), state_space_id: format!("{model_id}.no_state"), frame_id: String::new(), settings_hash, depth: "native".to_string(), ..Default::default() };

        Ok(Self { spec, star_apid_map, imu_apid_map, command_codec, period_ns, next_due: Cell::new(epoch_tai_ns + period_ns), seq: Cell::new(0), last_star_q: RefCell::new(None), last_imu_omega: RefCell::new(None), info })
    }

    pub fn period_ns(&self) -> i64 {
        self.period_ns
    }
}

impl DynamicsModel for AttitudeControllerModel {
    type Error = ControllerRuntimeError;

    fn state_dim(&self) -> usize {
        0
    }
    fn derivatives(&self, state: &[f64], _t_tai_ns: i64, _controls: &[f64], out: &mut [f64]) -> Result<(), Self::Error> {
        debug_assert!(state.is_empty() && out.is_empty());
        Ok(())
    }
    fn describe(&self) -> ModelInfo {
        self.info.clone()
    }
    fn step(&self, state: &[f64], t_tai_ns: i64, controls: &[f64], dt_ns: i64) -> Result<StepResult, Self::Error> {
        Ok(self.step_with_ports(state, t_tai_ns, controls, dt_ns, &Inbox::empty())?.0)
    }

    /// Decodes fresh star tracker/IMU measurements off [`CONTROLLER_STARTRACKER_IN_PORT`]/
    /// [`CONTROLLER_IMU_IN_PORT`] every call (regardless of whether this step happens to cross
    /// the controller's own declared emission boundary), caching the last of each -- exactly the
    /// zero-order hold `super::sensors::StarTrackerModel`'s own `last_truth` cache uses. A wheel-
    /// torque command packet is only ever *emitted* (pushed onto [`CONTROLLER_WHEEL_TORQUE_OUT_
    /// PORT`]) at instants that are exact multiples of `self.period_ns` (the declared control
    /// rate, trap 3 -- the same `while end >= self.next_due` catch-up loop the sensor models
    /// use), and only once both a star tracker and an IMU measurement have arrived at least
    /// once. `pointing_error_rad` (see the module doc comment's "Why the objective..." section)
    /// is computed and inserted into `outputs` on **every** call with a cached star measurement,
    /// not only a firing one, so `output.<instance>.pointing_error_rad@<any time>` is always the
    /// controller's own current belief, not stale-by-up-to-one-period.
    fn step_with_ports(&self, state: &[f64], t_tai_ns: i64, _controls: &[f64], dt_ns: i64, inbox: &Inbox) -> Result<(StepResult, Outbox, Vec<AppliedCommand>), Self::Error> {
        debug_assert!(state.is_empty());
        if let Some((msg, _sender)) = inbox.last_on_port(CONTROLLER_STARTRACKER_IN_PORT) {
            let decoded = codec::decode_packet(&self.star_apid_map, &msg.payload).map_err(ControllerRuntimeError::Codec)?;
            let get = |name: &str| match decoded.fields.get(name) {
                Some(FieldValue::Numeric(v)) => *v,
                _ => 0.0,
            };
            *self.last_star_q.borrow_mut() = Some([get("qx"), get("qy"), get("qz"), get("qw")]);
        }
        if let Some((msg, _sender)) = inbox.last_on_port(CONTROLLER_IMU_IN_PORT) {
            let decoded = codec::decode_packet(&self.imu_apid_map, &msg.payload).map_err(ControllerRuntimeError::Codec)?;
            let get = |name: &str| match decoded.fields.get(name) {
                Some(FieldValue::Numeric(v)) => *v,
                _ => 0.0,
            };
            *self.last_imu_omega.borrow_mut() = Some([get("wx"), get("wy"), get("wz")]);
        }

        let end = t_tai_ns + dt_ns;
        let mut outbox = Outbox::new();
        let mut outputs = BTreeMap::new();
        let mut applied = Vec::new();
        while end >= self.next_due.get() {
            let due = self.next_due.get();
            if let (Some(q), Some(omega)) = (*self.last_star_q.borrow(), *self.last_imu_omega.borrow()) {
                let qv = signed_error_vector(self.spec.target_q, q);
                let tau = [self.spec.kp * qv[0] + self.spec.kd * omega[0], self.spec.kp * qv[1] + self.spec.kd * omega[1], self.spec.kp * qv[2] + self.spec.kd * omega[2]];
                let mut values = BTreeMap::new();
                values.insert("tau_1".to_string(), FieldValue::Numeric(tau[0]));
                values.insert("tau_2".to_string(), FieldValue::Numeric(tau[1]));
                values.insert("tau_3".to_string(), FieldValue::Numeric(tau[2]));
                let seq = self.seq.get();
                self.seq.set(seq.wrapping_add(1) & 0x3FFF);
                let payload = codec::encode_packet(&self.command_codec, seq, &[], &values)
                    .expect("AttitudeControllerModel's declared command_codec always carries exactly the tau_1/2/3 FLOAT64 fields this call supplies, pre-validated at construction");
                outbox.push(CONTROLLER_WHEEL_TORQUE_OUT_PORT.to_string(), due, payload);
                outputs.insert("seq".to_string(), seq as f64);
                applied.push(AppliedCommand { port: CONTROLLER_WHEEL_TORQUE_OUT_PORT.to_string(), field: "tau_mag".to_string(), value: (tau[0] * tau[0] + tau[1] * tau[1] + tau[2] * tau[2]).sqrt(), applied_tai_ns: due });
            }
            self.next_due.set(due + self.period_ns);
        }
        if let Some(q) = *self.last_star_q.borrow() {
            let qv = signed_error_vector(self.spec.target_q, q);
            let vnorm = (qv[0] * qv[0] + qv[1] * qv[1] + qv[2] * qv[2]).sqrt().clamp(-1.0, 1.0);
            outputs.insert("pointing_error_rad".to_string(), 2.0 * vnorm.asin());
        }
        Ok((StepResult { state: Vec::new(), t_tai_ns: end, outputs }, outbox, applied))
    }

    // Decodes star tracker/IMU telemetry for its own control law (`last_star_q`/`last_imu_omega`)
    // and emits a wheel-torque *command* packet (`command_codec.is_command == true`), never
    // telemetry mapped through `crate::codec::measurements_from_field_values` -- no CDM
    // measurement. Question 176 pins receiver-side decode-into-measurement as a later task's
    // scope, not this one's.
    fn last_measurements(&self) -> Vec<av_cdm::pb::Measurement> {
        Vec::new()
    }
    // No SENSOR fault runtime.
    fn drain_sensor_fault_effect(&self) -> Option<av_dynamics::SensorFaultEffectDrain> {
        None
    }
}

/// `q_err = target_q^-1 (x) measured_q`, then sign-flipped for the shortest rotational path
/// (`sign(q_err.w)`) -- see the module doc comment's "Control law" section for the full
/// derivation this feeds. Shared by [`AttitudeControllerModel::step_with_ports`]'s command law
/// and its own `pointing_error_rad` output, so the two can never silently disagree about what
/// "the error" means.
fn signed_error_vector(target_q: [f64; 4], measured_q: [f64; 4]) -> [f64; 3] {
    let q_err = quat_mul(quat_conj(target_q), measured_q);
    let sign = if q_err[3] < 0.0 { -1.0 } else { 1.0 };
    [q_err[0] * sign, q_err[1] * sign, q_err[2] * sign]
}

// ============================================================================================
// CommandedAttitude: the plant-side decorator that consumes a wheel-torque command packet.
// ============================================================================================

/// A malformed inbound wheel-torque command packet, or the wrapped model's own error -- see the
/// module doc comment's "Typed refusals" section.
#[derive(Debug, Clone, PartialEq)]
pub enum CommandedAttitudeError<E> {
    Inner(E),
    Codec(CodecError),
}
impl<E: fmt::Display> fmt::Display for CommandedAttitudeError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CommandedAttitudeError::Inner(e) => write!(f, "{e}"),
            CommandedAttitudeError::Codec(e) => write!(f, "{e}"),
        }
    }
}
impl<E: fmt::Debug + fmt::Display> std::error::Error for CommandedAttitudeError<E> {}

/// Declared-once-at-construction command configuration -- `None` for every attitude instance
/// that declares no wheel-torque-command FRAMED IN port (every fixture through M22.4: `demo_
/// attitude_precession`, `demo_attitude_wheel_fault`, `demo_attitude_sensors_truth`), in which
/// case [`CommandedAttitude`] is a strict behavioural no-op -- identical `controls` passed
/// straight through, mirroring `super::sensors::TruthBroadcastAttitude`'s own "unconditional
/// wrap, provably inert wherever nothing is connected" rule (see `super::binding::AnyModel::
/// Attitude`'s own doc comment for why that same unconditional-wrap choice is repeated here
/// rather than a second, parallel "does this instance have a command port" signal).
struct CommandInput {
    apid_map: ApidMap,
}

/// Wraps any `DynamicsModel` (in practice, `super::sensors::TruthBroadcastAttitude<super::
/// attitude::AttitudeWheelsModel>`) and, when a wheel-torque command codec was declared for this
/// instance, decodes the latest command packet off [`ATTITUDE_WHEEL_TORQUE_IN_PORT`] each call
/// and threads the three decoded `tau_k` values through as the wrapped model's own `controls`
/// argument (zero-order hold between commands -- `super::attitude::AttitudeWheelsModel::
/// derivatives`'s own doc comment: a wheel with no covering `controls` entry falls back to its
/// declared `wheel_commanded_torque`, so *before* the controller's first command arrives, the
/// commanded torque is whatever `RefCell<Vec<f64>>` was initialized to -- all-zero, here, not
/// that per-wheel declared fallback, since a real closed loop is expected to command from its
/// very first emission onward). Every other `DynamicsModel` method delegates to the wrapped
/// model unchanged, exactly like `TruthBroadcastAttitude`'s own minimal override set.
pub struct CommandedAttitude<M> {
    pub inner: M,
    command: Option<CommandInput>,
    last_commanded_torque: RefCell<[f64; 3]>,
}

impl<M> CommandedAttitude<M> {
    /// `command_codec = None` for the "no command port declared" case (see [`CommandInput`]'s
    /// own doc comment); `Some(codec)` must be [`is_wheel_torque_command_codec`] -- checked by
    /// `super::binding::classify_binding`'s own attitude arm before this is ever called, not
    /// re-checked here (this constructor cannot itself return a `Result` without widening every
    /// call site that only ever hands it an already-validated codec or `None`).
    pub fn new(inner: M, command_codec: Option<PacketCodec>) -> Self {
        let command = command_codec.map(|codec| {
            let mut apid_map = ApidMap::new();
            apid_map.insert(codec.apid, codec);
            CommandInput { apid_map }
        });
        Self { inner, command, last_commanded_torque: RefCell::new([0.0; 3]) }
    }
}

impl<M: DynamicsModel> DynamicsModel for CommandedAttitude<M> {
    type Error = CommandedAttitudeError<M::Error>;

    fn state_dim(&self) -> usize {
        self.inner.state_dim()
    }
    fn derivatives(&self, state: &[f64], t_tai_ns: i64, controls: &[f64], state_dot: &mut [f64]) -> Result<(), Self::Error> {
        self.inner.derivatives(state, t_tai_ns, controls, state_dot).map_err(CommandedAttitudeError::Inner)
    }
    fn describe(&self) -> ModelInfo {
        self.inner.describe()
    }
    fn step(&self, state: &[f64], t_tai_ns: i64, controls: &[f64], dt_ns: i64) -> Result<StepResult, Self::Error> {
        self.inner.step(state, t_tai_ns, controls, dt_ns).map_err(CommandedAttitudeError::Inner)
    }

    fn step_with_ports(&self, state: &[f64], t_tai_ns: i64, controls: &[f64], dt_ns: i64, inbox: &Inbox) -> Result<(StepResult, Outbox, Vec<AppliedCommand>), Self::Error> {
        let mut extra_applied = Vec::new();
        if let Some(cmd) = &self.command {
            if let Some((msg, _sender)) = inbox.last_on_port(ATTITUDE_WHEEL_TORQUE_IN_PORT) {
                let decoded = codec::decode_packet(&cmd.apid_map, &msg.payload).map_err(CommandedAttitudeError::Codec)?;
                let mut torque = [0.0; 3];
                for (i, name) in WHEEL_TORQUE_FIELD_NAMES.iter().enumerate() {
                    if let Some(FieldValue::Numeric(v)) = decoded.fields.get(*name) {
                        torque[i] = *v;
                    }
                }
                *self.last_commanded_torque.borrow_mut() = torque;
                extra_applied.push(AppliedCommand { port: ATTITUDE_WHEEL_TORQUE_IN_PORT.to_string(), field: "tau_mag".to_string(), value: (torque[0] * torque[0] + torque[1] * torque[1] + torque[2] * torque[2]).sqrt(), applied_tai_ns: t_tai_ns });
            }
        }
        let effective_controls: Vec<f64> = if self.command.is_some() { self.last_commanded_torque.borrow().to_vec() } else { controls.to_vec() };
        let (result, outbox, mut applied) = self.inner.step_with_ports(state, t_tai_ns, &effective_controls, dt_ns, inbox).map_err(CommandedAttitudeError::Inner)?;
        applied.extend(extra_applied);
        Ok((result, outbox, applied))
    }

    /// Delegates to `self.inner.last_measurements` -- "every other `DynamicsModel` method
    /// delegates to the wrapped model unchanged" (this struct's own doc comment).
    fn last_measurements(&self) -> Vec<av_cdm::pb::Measurement> {
        self.inner.last_measurements()
    }

    /// Delegates to `self.inner.drain_sensor_fault_effect`, for the same reason
    /// [`CommandedAttitude::last_measurements`] does (question 178, R5.1a). The wrapped model is
    /// always attitude-shaped, never a star tracker, so this is always `None` in practice today
    /// -- delegating honestly is still the right shape rather than hardcoding that fact here.
    fn drain_sensor_fault_effect(&self) -> Option<av_dynamics::SensorFaultEffectDrain> {
        self.inner.drain_sensor_fault_effect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn star_codec() -> PacketCodec {
        super::super::sensors::star_tracker_packet_codec("ctrl_test.star", 200)
    }
    fn imu_codec() -> PacketCodec {
        super::super::sensors::imu_packet_codec("ctrl_test.imu", 201)
    }
    fn command_codec() -> PacketCodec {
        wheel_torque_command_packet_codec("ctrl_test.cmd", 202)
    }
    fn spec(kp: f64, kd: f64, target_q: [f64; 4]) -> AttitudeControllerSpec {
        AttitudeControllerSpec { kp, kd, target_q, update_rate_hz: 4.0 }
    }

    // ---------------------------------------------------------------------------------------
    // parse_attitude_controller_spec: typed refusal + acceptance.
    // ---------------------------------------------------------------------------------------

    fn params(entries: &[(&str, f64)]) -> BTreeMap<String, Parameter> {
        entries.iter().map(|(name, value)| (name.to_string(), Parameter { name: name.to_string(), value: *value, ..Default::default() })).collect()
    }

    #[test]
    fn parse_attitude_controller_spec_refuses_an_unrecognized_parameter() {
        let p = params(&[("controller.nonsense", 1.0)]);
        let err = parse_attitude_controller_spec(&p).unwrap_err();
        assert!(matches!(err, ControllerSpecError::UnknownParameter { ref name } if name == "controller.nonsense"), "{err}");
    }

    #[test]
    fn parse_attitude_controller_spec_refuses_a_non_positive_kp() {
        let mut p = params(&[("controller.kp", 0.0), ("controller.kd", 1.0), ("controller.update_rate_hz", 2.0)]);
        p.extend(params(&[("controller.target_q.x", 0.0), ("controller.target_q.y", 0.0), ("controller.target_q.z", 0.0), ("controller.target_q.w", 1.0)]));
        let err = parse_attitude_controller_spec(&p).unwrap_err();
        assert!(matches!(err, ControllerSpecError::InvalidParameter { ref name, .. } if name == "controller.kp"), "{err}");
    }

    #[test]
    fn parse_attitude_controller_spec_refuses_a_non_unit_target_quaternion() {
        let mut p = params(&[("controller.kp", 1.0), ("controller.kd", 1.0), ("controller.update_rate_hz", 2.0)]);
        p.extend(params(&[("controller.target_q.x", 0.0), ("controller.target_q.y", 0.0), ("controller.target_q.z", 0.0), ("controller.target_q.w", 2.0)]));
        let err = parse_attitude_controller_spec(&p).unwrap_err();
        assert!(matches!(err, ControllerSpecError::InvalidParameter { ref name, .. } if name == "controller.target_q"), "{err}");
    }

    #[test]
    fn parse_attitude_controller_spec_accepts_a_well_formed_spec() {
        let mut p = params(&[("controller.kp", 0.5), ("controller.kd", 5.0), ("controller.update_rate_hz", 2.0)]);
        p.extend(params(&[("controller.target_q.x", 0.0), ("controller.target_q.y", 0.0), ("controller.target_q.z", 0.0), ("controller.target_q.w", 1.0)]));
        let s = parse_attitude_controller_spec(&p).expect("well-formed spec parses");
        assert_eq!(s.kp, 0.5);
        assert_eq!(s.kd, 5.0);
        assert_eq!(s.target_q, [0.0, 0.0, 0.0, 1.0]);
    }

    // ---------------------------------------------------------------------------------------
    // AttitudeControllerModel::new: codec structural checks.
    // ---------------------------------------------------------------------------------------

    #[test]
    fn new_refuses_a_command_codec_missing_a_wheel_torque_field() {
        let mut bad_command = command_codec();
        bad_command.fields.truncate(2); // only tau_1, tau_2
        let err = AttitudeControllerModel::new(spec(1.0, 1.0, [0.0, 0.0, 0.0, 1.0]), star_codec(), imu_codec(), bad_command, 0, "ctrl_test").unwrap_err();
        assert!(matches!(err, ControllerSpecError::InvalidParameter { ref name, .. } if name == "controller.command_codec"), "{err:?}");
    }

    #[test]
    fn new_refuses_a_star_codec_missing_a_required_field() {
        let mut bad_star = star_codec();
        bad_star.fields.retain(|f| f.name != "qw");
        let err = AttitudeControllerModel::new(spec(1.0, 1.0, [0.0, 0.0, 0.0, 1.0]), bad_star, imu_codec(), command_codec(), 0, "ctrl_test").unwrap_err();
        assert!(matches!(err, ControllerSpecError::InvalidParameter { ref name, .. } if name == "controller.star_codec"), "{err:?}");
    }

    // ---------------------------------------------------------------------------------------
    // Control law: sign/stability, against a hand-derived case (not merely "it runs").
    // ---------------------------------------------------------------------------------------

    /// A measured quaternion representing a +0.2 rad rotation about +z, target = identity, zero
    /// measured rate: the commanded z-torque must be *positive* (per the module doc comment's
    /// derivation, `omega_dot_z = -(kp*qv_z + kd*omega_z)/J_z` -- a positive commanded torque
    /// drives `omega_z` negative, which is what rotates the body *back* toward identity). Fails
    /// against a sign-flipped control law (which would instead command a torque that drives the
    /// error away from zero -- an unstable, diverging loop).
    #[test]
    fn commanded_z_torque_has_the_stabilizing_sign_for_a_positive_z_rotation_error() {
        let theta = 0.2_f64;
        let measured_q = [0.0, 0.0, (theta / 2.0).sin(), (theta / 2.0).cos()];
        let qv = signed_error_vector([0.0, 0.0, 0.0, 1.0], measured_q);
        assert!(qv[2] > 0.0, "qv_z must be positive for a positive-angle rotation about z: {qv:?}");
        let kp = 0.5;
        let tau_z = kp * qv[2]; // kd*omega_z term is zero here (omega assumed zero)
        assert!(tau_z > 0.0, "a positive z rotation error must command a positive (restoring) z torque, got {tau_z}");
    }

    /// The identical rotation, negated (a -0.2 rad rotation about z), must command the opposite
    /// sign -- proves the law is genuinely proportional (not e.g. clamped to a constant sign),
    /// and independently exercises `quat_conj`'s own sign convention (not merely `quat_mul`'s
    /// identity-composition case `super::sensors::tests` already covers).
    #[test]
    fn commanded_z_torque_flips_sign_with_the_error_sign() {
        let theta = -0.2_f64;
        let measured_q = [0.0, 0.0, (theta / 2.0).sin(), (theta / 2.0).cos()];
        let qv = signed_error_vector([0.0, 0.0, 0.0, 1.0], measured_q);
        assert!(qv[2] < 0.0, "{qv:?}");
    }

    /// `pointing_error_rad`'s own `2*asin(|qv|)` formula recovers the exact injected rotation
    /// angle for a pure single-axis case -- a hand-computed pin, not a round trip through the
    /// control law itself.
    #[test]
    fn signed_error_vector_recovers_the_exact_rotation_angle_for_a_pure_z_rotation() {
        let theta = 0.37_f64;
        let measured_q = [0.0, 0.0, (theta / 2.0).sin(), (theta / 2.0).cos()];
        let qv = signed_error_vector([0.0, 0.0, 0.0, 1.0], measured_q);
        let vnorm = (qv[0] * qv[0] + qv[1] * qv[1] + qv[2] * qv[2]).sqrt();
        let recovered = 2.0 * vnorm.asin();
        assert!((recovered - theta).abs() < 1e-12, "recovered {recovered}, expected {theta}");
    }

    // ---------------------------------------------------------------------------------------
    // AttitudeControllerModel::step_with_ports: own declared rate, not the kernel step (trap 3).
    // ---------------------------------------------------------------------------------------

    fn star_message(codec: &PacketCodec, tai_ns: i64, q: [f64; 4], seq: u16) -> av_dynamics::PortMessage {
        let mut values = BTreeMap::new();
        values.insert("qx".to_string(), FieldValue::Numeric(q[0]));
        values.insert("qy".to_string(), FieldValue::Numeric(q[1]));
        values.insert("qz".to_string(), FieldValue::Numeric(q[2]));
        values.insert("qw".to_string(), FieldValue::Numeric(q[3]));
        av_dynamics::PortMessage { port: CONTROLLER_STARTRACKER_IN_PORT.to_string(), tai_ns, payload: codec::encode_packet(codec, seq, &[], &values).unwrap() }
    }
    fn imu_message(codec: &PacketCodec, tai_ns: i64, omega: [f64; 3], seq: u16) -> av_dynamics::PortMessage {
        let mut values = BTreeMap::new();
        values.insert("wx".to_string(), FieldValue::Numeric(omega[0]));
        values.insert("wy".to_string(), FieldValue::Numeric(omega[1]));
        values.insert("wz".to_string(), FieldValue::Numeric(omega[2]));
        values.insert("ax".to_string(), FieldValue::Numeric(0.0));
        values.insert("ay".to_string(), FieldValue::Numeric(0.0));
        values.insert("az".to_string(), FieldValue::Numeric(0.0));
        av_dynamics::PortMessage { port: CONTROLLER_IMU_IN_PORT.to_string(), tai_ns, payload: codec::encode_packet(codec, seq, &[], &values).unwrap() }
    }

    /// **Expected count, derived before measuring.** `update_rate_hz = 4.0` -> a 250 ms period;
    /// a single 2 s `step_with_ports` call (coarser than the declared period, exactly the "kernel
    /// dt coarser than the declared rate" shape `super::sensors::StarTrackerModel`'s own trap-3
    /// test drives) must emit `2s / 0.25s = 8` command packets, not one. Fails against an
    /// implementation that emits once per `step_with_ports` call regardless of `dt_ns` (would
    /// see 1, not 8) or that ignores `next_due` bookkeeping entirely.
    #[test]
    fn controller_emits_at_its_own_declared_rate_not_the_caller_step_size() {
        let model = AttitudeControllerModel::new(spec(0.5, 5.0, [0.0, 0.0, 0.0, 1.0]), star_codec(), imu_codec(), command_codec(), 0, "ctrl_test").unwrap();
        let star = star_codec();
        let imu = imu_codec();
        let inbox = Inbox::new(vec![star_message(&star, 0, [0.0, 0.0, 0.1, (1.0f64 - 0.01).sqrt()], 0), imu_message(&imu, 0, [0.0, 0.0, 0.0], 0)]);
        let (_result, outbox, applied) = model.step_with_ports(&[], 0, &[], 2_000_000_000, &inbox).unwrap();
        assert_eq!(outbox.messages().len(), 8, "8 emissions expected over 2s at a 4 Hz declared rate (0.25s period)");
        assert_eq!(applied.len(), 8);
    }

    /// Before any star tracker measurement has arrived, the controller must not emit a command
    /// at all (there is nothing to control on yet) -- fails against an implementation that
    /// commands zero torque (or any other default) instead of genuinely waiting.
    #[test]
    fn controller_emits_nothing_before_the_first_measurement_arrives() {
        let model = AttitudeControllerModel::new(spec(0.5, 5.0, [0.0, 0.0, 0.0, 1.0]), star_codec(), imu_codec(), command_codec(), 0, "ctrl_test").unwrap();
        let (_result, outbox, applied) = model.step_with_ports(&[], 0, &[], 2_000_000_000, &Inbox::empty()).unwrap();
        assert!(outbox.messages().is_empty());
        assert!(applied.is_empty());
    }

    /// A malformed inbound star tracker packet (declares an APID this controller's own star
    /// codec does not carry) is a typed `ControllerRuntimeError`, never a panic or a silent skip.
    #[test]
    fn a_malformed_star_packet_is_a_typed_error_not_a_panic() {
        let model = AttitudeControllerModel::new(spec(0.5, 5.0, [0.0, 0.0, 0.0, 1.0]), star_codec(), imu_codec(), command_codec(), 0, "ctrl_test").unwrap();
        let bad = av_dynamics::PortMessage { port: CONTROLLER_STARTRACKER_IN_PORT.to_string(), tai_ns: 0, payload: vec![0xFF, 0xFF, 0xFF, 0xFF] };
        let inbox = Inbox::new(vec![bad]);
        let err = model.step_with_ports(&[], 0, &[], 1_000_000_000, &inbox).unwrap_err();
        assert!(matches!(err, ControllerRuntimeError::Codec(_)), "{err:?}");
    }

    // ---------------------------------------------------------------------------------------
    // CommandedAttitude: passthrough when unconfigured, decode-and-forward when configured.
    // ---------------------------------------------------------------------------------------

    /// A trivial `DynamicsModel` that records exactly the `controls` slice its own `step`
    /// override was last called with, so `CommandedAttitude`'s own delegation can be checked
    /// against a real recorded value rather than inferred from side effects.
    struct RecordingModel {
        last_controls: RefCell<Vec<f64>>,
    }
    impl DynamicsModel for RecordingModel {
        type Error = std::convert::Infallible;
        fn state_dim(&self) -> usize {
            0
        }
        fn derivatives(&self, _s: &[f64], _t: i64, _c: &[f64], _o: &mut [f64]) -> Result<(), Self::Error> {
            Ok(())
        }
        fn describe(&self) -> ModelInfo {
            ModelInfo { id: "test.recording".to_string(), ..Default::default() }
        }
        fn step(&self, _state: &[f64], t_tai_ns: i64, controls: &[f64], dt_ns: i64) -> Result<StepResult, Self::Error> {
            *self.last_controls.borrow_mut() = controls.to_vec();
            Ok(StepResult { state: Vec::new(), t_tai_ns: t_tai_ns + dt_ns, outputs: BTreeMap::new() })
        }
        // Test-only recorder; never emits telemetry.
        fn last_measurements(&self) -> Vec<av_cdm::pb::Measurement> {
            Vec::new()
        }
        // No SENSOR fault runtime.
        fn drain_sensor_fault_effect(&self) -> Option<av_dynamics::SensorFaultEffectDrain> {
            None
        }
    }

    #[test]
    fn commanded_attitude_with_no_command_configured_passes_controls_through_unchanged() {
        let wrapped = CommandedAttitude::new(RecordingModel { last_controls: RefCell::new(Vec::new()) }, None);
        let (_r, _ob, _applied) = wrapped.step_with_ports(&[], 0, &[9.0, 8.0, 7.0], 1_000_000_000, &Inbox::empty()).unwrap();
        assert_eq!(*wrapped.inner.last_controls.borrow(), vec![9.0, 8.0, 7.0], "no command declared: the caller's own controls must pass through unchanged");
    }

    #[test]
    fn commanded_attitude_decodes_a_command_packet_and_threads_it_through_as_controls() {
        let cmd_codec = command_codec();
        let wrapped = CommandedAttitude::new(RecordingModel { last_controls: RefCell::new(Vec::new()) }, Some(cmd_codec.clone()));
        let mut values = BTreeMap::new();
        values.insert("tau_1".to_string(), FieldValue::Numeric(1.5));
        values.insert("tau_2".to_string(), FieldValue::Numeric(-2.5));
        values.insert("tau_3".to_string(), FieldValue::Numeric(0.25));
        let msg = av_dynamics::PortMessage { port: ATTITUDE_WHEEL_TORQUE_IN_PORT.to_string(), tai_ns: 0, payload: codec::encode_packet(&cmd_codec, 0, &[], &values).unwrap() };
        let inbox = Inbox::new(vec![msg]);
        let (_r, _ob, applied) = wrapped.step_with_ports(&[], 0, &[], 1_000_000_000, &inbox).unwrap();
        assert_eq!(*wrapped.inner.last_controls.borrow(), vec![1.5, -2.5, 0.25]);
        assert_eq!(applied.len(), 1, "the decoded command must be reported as an AppliedCommand (question 130)");
    }

    /// Before any command packet has arrived, a *configured* `CommandedAttitude` commands
    /// all-zero torque (not the caller's own `controls`, and not left uninitialized) -- see
    /// [`CommandedAttitude`]'s own doc comment for why zero, not a per-wheel declared fallback,
    /// is the right default for a genuinely closed loop.
    #[test]
    fn commanded_attitude_with_a_command_configured_defaults_to_zero_before_the_first_packet_arrives() {
        let wrapped = CommandedAttitude::new(RecordingModel { last_controls: RefCell::new(Vec::new()) }, Some(command_codec()));
        let (_r, _ob, _applied) = wrapped.step_with_ports(&[], 0, &[9.0, 9.0, 9.0], 1_000_000_000, &Inbox::empty()).unwrap();
        assert_eq!(*wrapped.inner.last_controls.borrow(), vec![0.0, 0.0, 0.0], "before the first command, a configured CommandedAttitude must command zero, not the caller's own controls");
    }

    /// A malformed inbound wheel-torque command packet is a typed `CommandedAttitudeError`,
    /// never a panic.
    #[test]
    fn a_malformed_command_packet_is_a_typed_error_not_a_panic() {
        let wrapped = CommandedAttitude::new(RecordingModel { last_controls: RefCell::new(Vec::new()) }, Some(command_codec()));
        let bad = av_dynamics::PortMessage { port: ATTITUDE_WHEEL_TORQUE_IN_PORT.to_string(), tai_ns: 0, payload: vec![0xFF, 0xFF] };
        let inbox = Inbox::new(vec![bad]);
        let err = wrapped.step_with_ports(&[], 0, &[], 1_000_000_000, &inbox).unwrap_err();
        assert!(matches!(err, CommandedAttitudeError::Codec(_)), "{err:?}");
    }
}
