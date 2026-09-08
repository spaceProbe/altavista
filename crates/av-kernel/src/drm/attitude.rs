//! Native rigid-body attitude dynamics with reaction wheels (M22.1, `docs/sil-plan.md`'s M22
//! milestone paragraph and its Decisions (2026-09-05) decision A: "attitude control first";
//! `docs/open-questions.md` questions 88 and 142: "the attitude state space becomes part of the
//! propagated dynamics with wheel and torque models").
//!
//! ## Scope of this batch, disclosed
//!
//! This module builds the propagated-state model itself -- [`AttitudeWheelsModel`] (a real
//! [`av_dynamics::DynamicsModel`], "alongside `ConstantAccelModel`" per the task brief), its
//! declared [`av_cdm::pb::StateSpace`] (`crate::trajectory::attitude_wheels_state_space`), and
//! typed parameter parsing ([`parse_attitude_spec`], mirroring `super::binding::
//! parse_constant_accel_spec`'s own "declared and typed, refuse the unrecognized" contract) --
//! plus the five closed-form goldens the brief names. It does **not** wire a new
//! `"attitude."`-dispatched arm into `super::binding::classify_binding`/`AnyModel`/
//! `crate::registry::ModelRegistry`: that plumbing is a materially larger, higher-risk change
//! spanning `binding.rs`, `registry.rs` and every exhaustive match over `BindingPlan`/`AnyModel`
//! in `executor.rs`/`fault.rs` (fault/maneuver re-binding, covariance, ports), and M22's own
//! milestone paragraph groups that end-to-end DRM wiring with the sensor/actuator/CCSDS work
//! that comes after this state-space sub-task, not before it. What this batch delivers is a
//! directly constructible, directly testable `DynamicsModel` whose physical dimension already
//! comes from a declared `StateSpace` (see [`AttitudeWheelsModel::new`]'s own doc comment) --
//! the same "declared width is authoritative" contract M21.3 established for
//! `super::binding::ConstantAccelModel` -- ready for that follow-on wiring without redoing the
//! physics.
//!
//! ## Dynamics
//!
//! State (scalar-last quaternion, matching `crate::trajectory::CARTESIAN_POS_VEL_6_ATTITUDE_
//! QUAT_4_ID`'s own `q_x, q_y, q_z, q_w` convention): `[q_x, q_y, q_z, q_w, omega_x, omega_y,
//! omega_z, h_w_1, .., h_w_n]` -- see [`crate::trajectory::attitude_wheels_state_space`] for the
//! declared labels/units.
//!
//! - **Quaternion kinematics**, body-to-inertial `q`, body-frame rate `omega` (Markley &
//!   Crassidis's `q_dot = 1/2 Xi(q) omega`, vector-first/scalar-last convention):
//!   `dq_v/dt = 1/2 (q_w * omega + q_v x omega)`, `dq_w/dt = -1/2 (q_v . omega)`.
//! - **Euler's equation with wheels**: `J omega_dot = -omega x (J omega + h_w) - tau_w`, where
//!   `h_w = sum_i axis_i * h_w_i` (each wheel's scalar momentum resolved onto its own body-frame
//!   spin axis) and `tau_w = sum_i axis_i * tau_i_eff` (the *effective*, post-saturation torque
//!   -- see below).
//! - **Wheel momentum**: `dh_w_i/dt = tau_i_eff`, `tau_i_eff` the `i`-th entry of `controls`
//!   (the commanded wheel torque, N*m -- `av_dynamics::DynamicsModel::derivatives`'s own
//!   `controls` slot; `controls.len()` must equal the wheel count) clamped by **declared
//!   saturation**: a wheel already at its declared limit accepts no *further* push outward
//!   (`commanded > 0` while `h_w_i >= limit_i`, or `commanded < 0` while `h_w_i <= -limit_i`)
//!   -- see [`AttitudeWheelsModel::wheel_is_saturated`], the explicit, named, tested predicate
//!   this clamp is observable through (never a silent no-op: the clamped derivative is the
//!   *only* place the limit takes effect, so a wheel's own momentum trajectory is the direct,
//!   measurable evidence of it -- [`tests::a_wheel_driven_past_its_limit_stops_accumulating_
//!   momentum_and_the_body_responds_accordingly`] pins exactly this).
//!
//! **No renormalization.** `derivatives` never projects `q` back onto the unit sphere -- the
//! brief's own instruction ("say what the integrator's drift actually is rather than
//! renormalizing silently") is honoured by *not* renormalizing at all; see
//! [`tests::quaternion_norm_drift_over_a_long_run_is_bounded_and_disclosed`] for the measured
//! drift.

use std::collections::BTreeMap;
use std::fmt;

use av_cdm::pb::{ModelInfo, Parameter, StateSpace};
use av_dynamics::DynamicsModel;

// ------------------------------------------------------------------------------------------
// Small 3-vector / 3x3-matrix helpers (no new linear-algebra dependency for three numbers).
// ------------------------------------------------------------------------------------------

fn cross3(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn mat3_vec3_mul(m: &[[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

/// Inverse of a **symmetric** 3x3 matrix via the cofactor/adjugate formula -- for a symmetric
/// input the cofactor matrix is itself symmetric, so it equals its own transpose (the adjugate)
/// and no separate transpose step is needed. `None` if singular (determinant magnitude below
/// `1e-30`, chosen only to guard an exact `/0.0`; every inertia tensor this module actually
/// constructs from parsed parameters is checked for physical positive-definiteness first --
/// see [`parse_attitude_spec`] -- so this guard is a last-resort typed refusal, not the primary
/// validation).
fn mat3_inverse_symmetric(m: &[[f64; 3]; 3]) -> Option<[[f64; 3]; 3]> {
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0]) + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    if det.abs() < 1e-30 {
        return None;
    }
    let inv_det = 1.0 / det;
    let cof = [
        [m[1][1] * m[2][2] - m[1][2] * m[2][1], -(m[1][0] * m[2][2] - m[1][2] * m[2][0]), m[1][0] * m[2][1] - m[1][1] * m[2][0]],
        [-(m[0][1] * m[2][2] - m[0][2] * m[2][1]), m[0][0] * m[2][2] - m[0][2] * m[2][0], -(m[0][0] * m[2][1] - m[0][1] * m[2][0])],
        [m[0][1] * m[1][2] - m[0][2] * m[1][1], -(m[0][0] * m[1][2] - m[0][2] * m[1][0]), m[0][0] * m[1][1] - m[0][1] * m[1][0]],
    ];
    let mut inv = [[0.0; 3]; 3];
    for r in 0..3 {
        for c in 0..3 {
            inv[r][c] = cof[r][c] * inv_det;
        }
    }
    Some(inv)
}

fn norm3(v: [f64; 3]) -> f64 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}

// ------------------------------------------------------------------------------------------
// Typed parameter parsing (mirrors super::binding::parse_constant_accel_spec's own contract).
// ------------------------------------------------------------------------------------------

/// Everything [`parse_attitude_spec`]/[`AttitudeWheelsModel::new`] can refuse -- a local,
/// dedicated error type rather than reusing `super::DrmError`: this module is not (yet, see the
/// module doc comment's "Scope of this batch") reached through `super::binding::
/// classify_binding`, so extending that heavily-matched, DRM-executor-wide enum for a subsystem
/// it does not yet dispatch to would be a wider, less-contained change than this batch's own
/// scope justifies. Every variant still follows the same "typed refusal, never a silent guess"
/// rule `DrmError::MissingParameter`/`UnknownParameter` follow.
#[derive(Debug, Clone, PartialEq)]
pub enum AttitudeSpecError {
    /// A required parameter (or one member of a "declared together" group) was absent.
    MissingParameter { name: String },
    /// A declared parameter name matched neither `"attitude.*"` fixed field nor the
    /// `"attitude.wheel.<k>.*"` pattern.
    UnknownParameter { name: String },
    /// A present parameter's *value* failed a physical check (quaternion not unit norm, a wheel
    /// axis not a unit vector, a non-positive momentum limit, a non-positive-definite inertia
    /// tensor, ...) -- refused rather than silently normalized/clamped/ignored.
    InvalidParameter { name: String, reason: String },
    /// [`AttitudeWheelsModel::new`]'s own load-time check (mirrors `DrmError::
    /// StateSpaceDimensionMismatch`): the declared `StateSpace`'s component count did not equal
    /// `crate::trajectory::ATTITUDE_WHEELS_BASE_COMPONENTS + wheel_axes.len()` -- the model's
    /// dimension is never hard-coded, only ever read off the declared shape, so a mismatch here
    /// is refused rather than the model silently adopting one width or the other.
    StateSpaceDimensionMismatch { declared_dim: usize, expected_dim: usize },
    /// M22.1b (`docs/open-questions.md` question 151, decided by the lead): a declared
    /// `wheel_h_<n>` state-space component's own `Unit` was not [`av_cdm::pb::Unit::
    /// NewtonMeterSecond`] (`kg*m^2/s`, angular momentum) -- in particular, [`av_cdm::pb::Unit::
    /// NewtonMeter`] (torque), M22.1's own documented approximation of the component's
    /// dimension before this unit existed in `core.proto`, is no longer accepted. Checked by
    /// [`AttitudeWheelsModel::new`] against exactly the declared `StateSpace` it is handed --
    /// see that function's own doc comment.
    WheelMomentumUnitIsTorque { label: String, got_unit_code: i32 },
}

impl fmt::Display for AttitudeSpecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AttitudeSpecError::MissingParameter { name } => write!(f, "missing required attitude parameter {name:?}"),
            AttitudeSpecError::UnknownParameter { name } => write!(f, "unrecognized attitude parameter {name:?}"),
            AttitudeSpecError::InvalidParameter { name, reason } => write!(f, "attitude parameter {name:?} is invalid: {reason}"),
            AttitudeSpecError::StateSpaceDimensionMismatch { declared_dim, expected_dim } => {
                write!(f, "declared state space has {declared_dim} component(s) but the parsed attitude spec needs {expected_dim} (7 + wheel count)")
            }
            AttitudeSpecError::WheelMomentumUnitIsTorque { label, got_unit_code } => {
                let torque_code = av_cdm::pb::Unit::NewtonMeter as i32;
                if *got_unit_code == torque_code {
                    write!(f, "wheel-momentum component {label:?} is declared UNIT_NEWTON_METER (torque) instead of UNIT_NEWTON_METER_SECOND (kg*m^2/s, angular momentum) -- M22.1's documented approximation of this component's dimension is no longer accepted (question 151)")
                } else {
                    write!(f, "wheel-momentum component {label:?} is declared with unit code {got_unit_code}, not UNIT_NEWTON_METER_SECOND (kg*m^2/s, angular momentum, question 151)")
                }
            }
        }
    }
}
impl std::error::Error for AttitudeSpecError {}

/// Parsed, typed parameters for [`AttitudeWheelsModel`] -- built by [`parse_attitude_spec`] from
/// a `SystemDefinition`'s effective parameters (`super::binding::effective_parameters`'s own
/// merge of `SystemDefinition.parameters` and `SystemInstance.parameter_overrides`), the same
/// hashed-declaration surface every other native binding kind's spec is parsed from.
///
/// Parameter vocabulary (a name matching none of these is [`AttitudeSpecError::
/// UnknownParameter`]):
/// - `attitude.inertia.jxx` / `.jyy` / `.jzz` (required): principal-diagonal inertia, `kg*m^2`.
/// - `attitude.inertia.jxy` / `.jxz` / `.jyz` (optional, default `0.0`): off-diagonal terms --
///   the tensor is always assembled symmetric.
/// - `attitude.wheel.<k>.axis_x` / `.axis_y` / `.axis_z` / `.momentum_limit` (`k = 1..=n`,
///   contiguous from 1, all four required together per `k`): the `k`-th reaction wheel's
///   body-frame spin axis (must be within [`UNIT_VECTOR_TOL`] of unit length) and its declared
///   saturation momentum limit (must be `> 0`).
/// - `attitude.q0.x` / `.y` / `.z` / `.w` (required together): initial attitude quaternion,
///   scalar-last, must be within [`QUATERNION_UNIT_NORM_TOL`] of unit norm.
/// - `attitude.omega0.x` / `.y` / `.z` (required together): initial body rate, rad/s.
#[derive(Debug, Clone, PartialEq)]
pub struct AttitudeWheelsSpec {
    /// Symmetric body-frame inertia tensor, `kg*m^2`.
    pub inertia: [[f64; 3]; 3],
    /// Each wheel's unit spin axis, body frame, index-aligned with `wheel_momentum_limits`.
    pub wheel_axes: Vec<[f64; 3]>,
    /// Each wheel's declared saturation momentum limit (`> 0`), index-aligned with `wheel_axes`.
    pub wheel_momentum_limits: Vec<f64>,
    /// Initial attitude quaternion, scalar-last `[x, y, z, w]`, unit norm.
    pub q0: [f64; 4],
    /// Initial body rate, rad/s.
    pub omega0: [f64; 3],
    /// M22.1b (question 152, decided by the lead): whether each wheel is available to exert
    /// torque at all -- index-aligned with `wheel_axes`, defaulting to `true` for every wheel a
    /// DRM does not explicitly declare `attitude.wheel.<k>.available` for (backward compatible
    /// with every M22.1 fixture/spec, none of which declares this parameter). A `false` entry is
    /// the "a wheel's own availability" arm a `FAULT_TARGET_KIND_DYNAMICS` fault can flip (see
    /// `super::fault::apply_dynamics_fault`'s own `BindingPlan::Attitude` arm) -- distinct from
    /// (and orthogonal to) `wheel_momentum_limits`: a wheel can be available but saturated, or
    /// unavailable while still holding whatever momentum it had at the moment it went offline.
    pub wheel_available: Vec<bool>,
    /// M22.1b: each wheel's own declared, constant commanded torque (`N*m`), index-aligned with
    /// `wheel_axes`, defaulting to `0.0`. `av_dynamics::DynamicsModel::derivatives`'s own
    /// `controls` slot (M22.1's own mechanism) is never actually populated by a real DRM run --
    /// `crate::schedule`'s own module doc comment discloses "Controls are not yet wired. Every
    /// step call passes an empty control slice (`&[]`)" -- so a wheel torque command has to be
    /// *declared*, hashed configuration to have any effect at all through `crate::drm::executor::
    /// execute`, exactly the same "no runtime control input yet, so bake the actuation into the
    /// model" choice `super::binding::ConstantAccelModel`'s own constant `a: [f64; 3]` already
    /// makes for translational acceleration. [`AttitudeWheelsModel::derivatives`] still honours
    /// an explicit, real `controls` entry when a *direct* (non-DRM) caller supplies one -- see
    /// that method's own doc comment -- so every one of M22.1's own five closed-form goldens
    /// (which all call `step`/`step_with_ports` directly with explicit `controls` slices) keeps
    /// its exact prior behaviour unchanged.
    pub wheel_commanded_torque: Vec<f64>,
}

/// Tolerance [`parse_attitude_spec`] checks a declared wheel axis and `q0` against for unit
/// length -- loose enough for an author's own rounded decimal literal (e.g. `0.7071` for
/// `1/sqrt(2)`), tight enough to catch a genuinely wrong (unnormalized, or garbage) vector.
pub const UNIT_VECTOR_TOL: f64 = 1e-6;

pub(crate) fn parse_index_and_field(rest: &str) -> Option<(usize, &str)> {
    let (idx_str, field) = rest.split_once('.')?;
    let idx: usize = idx_str.parse().ok()?;
    if idx == 0 {
        return None;
    }
    Some((idx, field))
}

pub fn parse_attitude_spec(params: &BTreeMap<String, Parameter>) -> Result<AttitudeWheelsSpec, AttitudeSpecError> {
    let mut jxx: Option<f64> = None;
    let mut jyy: Option<f64> = None;
    let mut jzz: Option<f64> = None;
    let mut jxy = 0.0_f64;
    let mut jxz = 0.0_f64;
    let mut jyz = 0.0_f64;
    let mut q0 = [None; 4];
    let mut omega0 = [None; 3];
    let mut wheel_axis_x: BTreeMap<usize, f64> = BTreeMap::new();
    let mut wheel_axis_y: BTreeMap<usize, f64> = BTreeMap::new();
    let mut wheel_axis_z: BTreeMap<usize, f64> = BTreeMap::new();
    let mut wheel_limit: BTreeMap<usize, f64> = BTreeMap::new();
    // M22.1b (question 152): optional per-wheel overrides, defaulted below once n_wheels is
    // known -- see AttitudeWheelsSpec::wheel_available/wheel_commanded_torque's own doc
    // comments for why these exist and why the default (available, zero torque) is safe for
    // every spec that never mentions either parameter.
    let mut wheel_available: BTreeMap<usize, f64> = BTreeMap::new();
    let mut wheel_commanded_torque: BTreeMap<usize, f64> = BTreeMap::new();

    // M22.1b (question 151, decided by the lead): an inertia parameter's declared `Parameter.unit`
    // must be UNIT_KILOGRAM_METER_SQUARED when set at all -- `UNIT_UNSPECIFIED` (the proto
    // default, `Parameter::default().unit`) is tolerated so every pre-question-151 fixture that
    // never set `unit` on these six parameters keeps loading unchanged, but a *wrong* declared
    // unit (e.g. a copy-paste of UNIT_NEWTON_METER from a wheel-momentum parameter) is refused
    // rather than silently accepted -- checked once, right here, rather than duplicated at each
    // of the six match arms below.
    let check_inertia_unit = |name: &str, p: &av_cdm::pb::Parameter| -> Result<(), AttitudeSpecError> {
        let unspecified = av_cdm::pb::Unit::Unspecified as i32;
        let kg_m2 = av_cdm::pb::Unit::KilogramMeterSquared as i32;
        if p.unit != unspecified && p.unit != kg_m2 {
            return Err(AttitudeSpecError::InvalidParameter { name: name.to_string(), reason: format!("declared unit code {} is neither UNIT_UNSPECIFIED nor UNIT_KILOGRAM_METER_SQUARED (question 151)", p.unit) });
        }
        Ok(())
    };

    for (name, p) in params {
        match name.as_str() {
            "attitude.inertia.jxx" => {
                check_inertia_unit(name, p)?;
                jxx = Some(p.value);
            }
            "attitude.inertia.jyy" => {
                check_inertia_unit(name, p)?;
                jyy = Some(p.value);
            }
            "attitude.inertia.jzz" => {
                check_inertia_unit(name, p)?;
                jzz = Some(p.value);
            }
            "attitude.inertia.jxy" => {
                check_inertia_unit(name, p)?;
                jxy = p.value;
            }
            "attitude.inertia.jxz" => {
                check_inertia_unit(name, p)?;
                jxz = p.value;
            }
            "attitude.inertia.jyz" => {
                check_inertia_unit(name, p)?;
                jyz = p.value;
            }
            "attitude.q0.x" => q0[0] = Some(p.value),
            "attitude.q0.y" => q0[1] = Some(p.value),
            "attitude.q0.z" => q0[2] = Some(p.value),
            "attitude.q0.w" => q0[3] = Some(p.value),
            "attitude.omega0.x" => omega0[0] = Some(p.value),
            "attitude.omega0.y" => omega0[1] = Some(p.value),
            "attitude.omega0.z" => omega0[2] = Some(p.value),
            other => {
                let rest = other.strip_prefix("attitude.wheel.").ok_or_else(|| AttitudeSpecError::UnknownParameter { name: name.clone() })?;
                let (idx, field) = parse_index_and_field(rest).ok_or_else(|| AttitudeSpecError::UnknownParameter { name: name.clone() })?;
                match field {
                    "axis_x" => {
                        wheel_axis_x.insert(idx, p.value);
                    }
                    "axis_y" => {
                        wheel_axis_y.insert(idx, p.value);
                    }
                    "axis_z" => {
                        wheel_axis_z.insert(idx, p.value);
                    }
                    "momentum_limit" => {
                        wheel_limit.insert(idx, p.value);
                    }
                    // M22.1b (question 152): optional, per-wheel -- see
                    // AttitudeWheelsSpec::wheel_available's own doc comment.
                    "available" => {
                        wheel_available.insert(idx, p.value);
                    }
                    // M22.1b: optional, per-wheel -- see
                    // AttitudeWheelsSpec::wheel_commanded_torque's own doc comment.
                    "commanded_torque" => {
                        wheel_commanded_torque.insert(idx, p.value);
                    }
                    _ => return Err(AttitudeSpecError::UnknownParameter { name: name.clone() }),
                }
            }
        }
    }

    let jxx = jxx.ok_or_else(|| AttitudeSpecError::MissingParameter { name: "attitude.inertia.jxx".to_string() })?;
    let jyy = jyy.ok_or_else(|| AttitudeSpecError::MissingParameter { name: "attitude.inertia.jyy".to_string() })?;
    let jzz = jzz.ok_or_else(|| AttitudeSpecError::MissingParameter { name: "attitude.inertia.jzz".to_string() })?;
    let inertia = [[jxx, jxy, jxz], [jxy, jyy, jyz], [jxz, jyz, jzz]];
    // Physical sanity: a real inertia tensor is positive definite. Checked here via Sylvester's
    // criterion (leading principal minors all positive) rather than deferred to
    // AttitudeWheelsModel::new's own singularity guard, so a non-physical (but non-singular,
    // e.g. indefinite) tensor is refused with a specific reason instead of silently accepted and
    // only failing much later, opaquely, inside the integrator.
    let minor1 = inertia[0][0];
    let minor2 = inertia[0][0] * inertia[1][1] - inertia[0][1] * inertia[1][0];
    let minor3 = inertia[0][0] * (inertia[1][1] * inertia[2][2] - inertia[1][2] * inertia[2][1]) - inertia[0][1] * (inertia[1][0] * inertia[2][2] - inertia[1][2] * inertia[2][0]) + inertia[0][2] * (inertia[1][0] * inertia[2][1] - inertia[1][1] * inertia[2][0]);
    if !(minor1 > 0.0 && minor2 > 0.0 && minor3 > 0.0) {
        return Err(AttitudeSpecError::InvalidParameter { name: "attitude.inertia".to_string(), reason: format!("tensor {inertia:?} is not positive definite (Sylvester minors {minor1}, {minor2}, {minor3})") });
    }

    let q0 = match (q0[0], q0[1], q0[2], q0[3]) {
        (Some(x), Some(y), Some(z), Some(w)) => [x, y, z, w],
        _ => return Err(AttitudeSpecError::MissingParameter { name: "attitude.q0.{x,y,z,w} (all four required together)".to_string() }),
    };
    let q0_norm = (q0[0] * q0[0] + q0[1] * q0[1] + q0[2] * q0[2] + q0[3] * q0[3]).sqrt();
    if (q0_norm - 1.0).abs() > crate::interpolate::QUATERNION_UNIT_NORM_TOL {
        return Err(AttitudeSpecError::InvalidParameter { name: "attitude.q0".to_string(), reason: format!("not unit norm: |q0| = {q0_norm}") });
    }

    let omega0 = match (omega0[0], omega0[1], omega0[2]) {
        (Some(x), Some(y), Some(z)) => [x, y, z],
        _ => return Err(AttitudeSpecError::MissingParameter { name: "attitude.omega0.{x,y,z} (all three required together)".to_string() }),
    };

    let indices: std::collections::BTreeSet<usize> =
        wheel_axis_x.keys().chain(wheel_axis_y.keys()).chain(wheel_axis_z.keys()).chain(wheel_limit.keys()).chain(wheel_available.keys()).chain(wheel_commanded_torque.keys()).copied().collect();
    let n_wheels = indices.len();
    for (rank, idx) in indices.iter().enumerate() {
        if *idx != rank + 1 {
            return Err(AttitudeSpecError::InvalidParameter { name: format!("attitude.wheel.{idx}"), reason: format!("wheel indices must be contiguous starting at 1; got index {idx} at rank {rank} among {n_wheels} wheel(s)") });
        }
    }
    let mut wheel_axes = Vec::with_capacity(n_wheels);
    let mut wheel_momentum_limits = Vec::with_capacity(n_wheels);
    let mut wheel_available_vec = Vec::with_capacity(n_wheels);
    let mut wheel_commanded_torque_vec = Vec::with_capacity(n_wheels);
    for idx in 1..=n_wheels {
        let ax = wheel_axis_x.get(&idx).copied().ok_or_else(|| AttitudeSpecError::MissingParameter { name: format!("attitude.wheel.{idx}.axis_x") })?;
        let ay = wheel_axis_y.get(&idx).copied().ok_or_else(|| AttitudeSpecError::MissingParameter { name: format!("attitude.wheel.{idx}.axis_y") })?;
        let az = wheel_axis_z.get(&idx).copied().ok_or_else(|| AttitudeSpecError::MissingParameter { name: format!("attitude.wheel.{idx}.axis_z") })?;
        let limit = wheel_limit.get(&idx).copied().ok_or_else(|| AttitudeSpecError::MissingParameter { name: format!("attitude.wheel.{idx}.momentum_limit") })?;
        let axis = [ax, ay, az];
        let n = norm3(axis);
        if (n - 1.0).abs() > UNIT_VECTOR_TOL {
            return Err(AttitudeSpecError::InvalidParameter { name: format!("attitude.wheel.{idx}.axis"), reason: format!("not a unit vector: |axis| = {n}") });
        }
        // Explicit `is_nan` alongside `<=` (rather than `!(limit > 0.0)`, which clippy flags as
        // hard to read on a partially-ordered type) so a NaN momentum limit is refused for
        // exactly the reason it would have been under the negated form: it is not > 0.0 either.
        if limit.is_nan() || limit <= 0.0 {
            return Err(AttitudeSpecError::InvalidParameter { name: format!("attitude.wheel.{idx}.momentum_limit"), reason: format!("must be > 0, got {limit}") });
        }
        wheel_axes.push(axis);
        wheel_momentum_limits.push(limit);
        // M22.1b (question 152): default available=true (`value != 0.0`, the same
        // "!= 0.0" convention this crate's own boolean-flag parameters already use, e.g.
        // super::binding::ConstantAccelSpec's "container.tls") when a wheel never declares
        // `attitude.wheel.<k>.available`; default commanded_torque=0.0 when it never declares
        // `attitude.wheel.<k>.commanded_torque` -- backward compatible with every spec that
        // predates question 152.
        wheel_available_vec.push(wheel_available.get(&idx).map(|v| *v != 0.0).unwrap_or(true));
        wheel_commanded_torque_vec.push(wheel_commanded_torque.get(&idx).copied().unwrap_or(0.0));
    }

    Ok(AttitudeWheelsSpec { inertia, wheel_axes, wheel_momentum_limits, q0, omega0, wheel_available: wheel_available_vec, wheel_commanded_torque: wheel_commanded_torque_vec })
}

// ------------------------------------------------------------------------------------------
// AttitudeWheelsModel: the DynamicsModel itself.
// ------------------------------------------------------------------------------------------

/// A native rigid-body attitude dynamics model with reaction wheels -- see the module doc
/// comment for the exact state layout and equations of motion. `type Error =
/// std::convert::Infallible`, the same choice `super::binding::ConstantAccelModel` makes: every
/// physical check (inertia positive-definiteness, unit-norm quaternion, unit wheel axes,
/// positive momentum limits, declared-vs-parsed dimension agreement) already happened in
/// [`parse_attitude_spec`]/[`AttitudeWheelsModel::new`], so `derivatives` itself cannot fail --
/// wheel saturation is a clamp inside the arithmetic, never a runtime error.
#[derive(Debug)]
pub struct AttitudeWheelsModel {
    inertia: [[f64; 3]; 3],
    inertia_inv: [[f64; 3]; 3],
    wheel_axes: Vec<[f64; 3]>,
    wheel_momentum_limits: Vec<f64>,
    /// M22.1b (question 152): see `AttitudeWheelsSpec::wheel_available`'s own doc comment.
    wheel_available: Vec<bool>,
    /// M22.1b: see `AttitudeWheelsSpec::wheel_commanded_torque`'s own doc comment.
    wheel_commanded_torque: Vec<f64>,
    dim: usize,
    info: ModelInfo,
}

impl AttitudeWheelsModel {
    /// Builds the model from a parsed [`AttitudeWheelsSpec`] and the instance's own declared
    /// [`StateSpace`] (`crate::trajectory::attitude_wheels_state_space`, or an equivalent
    /// inline declaration -- see that function's own doc comment for why this shape is never a
    /// fixed built-in registry id). **This is where the model's dimension comes from the
    /// declared state space, not a hard-coded width**: `expected_dim` is computed from the
    /// spec's own wheel count (`ATTITUDE_WHEELS_BASE_COMPONENTS + wheel_axes.len()`), compared
    /// against `declared_state_space.components.len()`, and only on agreement does `self.dim`
    /// (what [`DynamicsModel::state_dim`] reports) get set to it -- mirroring `super::binding::
    /// ConstantAccelModel`'s own M21.3 "declared width is authoritative" contract
    /// (`super::binding::CONSTANT_ACCEL_STATE_DIM`'s doc comment) rather than assuming either
    /// side is right.
    pub fn new(spec: &AttitudeWheelsSpec, declared_state_space: &StateSpace, model_id: &str) -> Result<Self, AttitudeSpecError> {
        let expected_dim = crate::trajectory::ATTITUDE_WHEELS_BASE_COMPONENTS + spec.wheel_axes.len();
        let declared_dim = declared_state_space.components.len();
        if declared_dim != expected_dim {
            return Err(AttitudeSpecError::StateSpaceDimensionMismatch { declared_dim, expected_dim });
        }
        // M22.1b (question 151, decided by the lead): every declared wheel-momentum component
        // must carry UNIT_NEWTON_METER_SECOND, never UNIT_NEWTON_METER (M22.1's own documented
        // approximation, no longer accepted) or any other unit -- checked against exactly the
        // declared StateSpace this constructor was handed, the same "declared shape is
        // authoritative" source this function already reads `expected_dim`'s own agreement
        // from above.
        let base = crate::trajectory::ATTITUDE_WHEELS_BASE_COMPONENTS;
        for comp in &declared_state_space.components[base..] {
            if comp.unit != av_cdm::pb::Unit::NewtonMeterSecond as i32 {
                return Err(AttitudeSpecError::WheelMomentumUnitIsTorque { label: comp.label.clone(), got_unit_code: comp.unit });
            }
        }
        let inertia_inv = mat3_inverse_symmetric(&spec.inertia).ok_or_else(|| AttitudeSpecError::InvalidParameter { name: "attitude.inertia".to_string(), reason: format!("tensor {:?} is numerically singular", spec.inertia) })?;

        let mut settings = BTreeMap::new();
        for (label, v) in [("jxx", spec.inertia[0][0]), ("jyy", spec.inertia[1][1]), ("jzz", spec.inertia[2][2]), ("jxy", spec.inertia[0][1]), ("jxz", spec.inertia[0][2]), ("jyz", spec.inertia[1][2])] {
            settings.insert(label.to_string(), format!("{v:.17e}"));
        }
        for (i, (axis, limit)) in spec.wheel_axes.iter().zip(&spec.wheel_momentum_limits).enumerate() {
            settings.insert(format!("wheel_{i}_axis_x"), format!("{:.17e}", axis[0]));
            settings.insert(format!("wheel_{i}_axis_y"), format!("{:.17e}", axis[1]));
            settings.insert(format!("wheel_{i}_axis_z"), format!("{:.17e}", axis[2]));
            settings.insert(format!("wheel_{i}_momentum_limit"), format!("{limit:.17e}"));
        }
        // M22.1b (question 152): declared, hashed configuration exactly like every other wheel
        // field above -- a DRM naming a different availability/commanded-torque hashes
        // differently, never silently (question 11's rule).
        for (i, available) in spec.wheel_available.iter().enumerate() {
            settings.insert(format!("wheel_{i}_available"), available.to_string());
        }
        for (i, torque) in spec.wheel_commanded_torque.iter().enumerate() {
            settings.insert(format!("wheel_{i}_commanded_torque"), format!("{torque:.17e}"));
        }
        let settings_hash = av_dynamics::settings_hash(&settings);

        let info = ModelInfo {
            id: model_id.to_string(),
            version: "1".to_string(),
            state_space_id: declared_state_space.id.clone(),
            frame_id: declared_state_space.frame_id.clone(),
            settings_hash,
            depth: "native".to_string(),
            ..Default::default()
        };

        Ok(Self {
            inertia: spec.inertia,
            inertia_inv,
            wheel_axes: spec.wheel_axes.clone(),
            wheel_momentum_limits: spec.wheel_momentum_limits.clone(),
            wheel_available: spec.wheel_available.clone(),
            wheel_commanded_torque: spec.wheel_commanded_torque.clone(),
            dim: expected_dim,
            info,
        })
    }

    /// The initial state vector `[q0; omega0; 0, .., 0]` -- every wheel starts at zero stored
    /// momentum (this spec has no `"attitude.wheel.<k>.h0"` parameter; a nonzero initial wheel
    /// momentum is not a case this batch's parameter vocabulary declares).
    pub fn initial_state(&self, spec: &AttitudeWheelsSpec) -> Vec<f64> {
        let mut x0 = vec![0.0; self.dim];
        x0[0..4].copy_from_slice(&spec.q0);
        x0[4..7].copy_from_slice(&spec.omega0);
        x0
    }

    pub fn n_wheels(&self) -> usize {
        self.wheel_axes.len()
    }

    /// **Explicit, named, directly testable saturation predicate** (the brief's "that clamp
    /// must be explicit and observable, never a silent no-op"): `true` iff wheel `wheel_idx`'s
    /// stored momentum in `state` is at (or past, which [`Self::derivatives`]'s own clamp never
    /// lets it go beyond by more than the integrator's own local step error) its declared
    /// [`AttitudeWheelsSpec::wheel_momentum_limits`] entry, in either direction.
    pub fn wheel_is_saturated(&self, state: &[f64], wheel_idx: usize) -> bool {
        let h = state[crate::trajectory::ATTITUDE_WHEELS_BASE_COMPONENTS + wheel_idx];
        // Tolerance sized to the adaptive integrator's own local error near the derivative
        // kink at h == limit (see tests::a_wheel_driven_past_its_limit_... for the measured
        // scale), not to floating-point round-off alone.
        h.abs() >= self.wheel_momentum_limits[wheel_idx] - 1e-6
    }
}

impl DynamicsModel for AttitudeWheelsModel {
    type Error = std::convert::Infallible;

    fn state_dim(&self) -> usize {
        self.dim
    }

    fn describe(&self) -> ModelInfo {
        self.info.clone()
    }

    /// See the module doc comment's "Dynamics" section for the equations this implements.
    /// `controls[i]` is the commanded torque (N*m) for wheel `i` when a caller supplies a real
    /// `controls` slice (every one of M22.1's own five closed-form goldens calls `step`/
    /// `step_with_ports` directly this way, so their exact prior behaviour is unchanged); when
    /// `controls` does not cover wheel `i` at all (in particular, `controls == &[]`, exactly
    /// what `crate::schedule`'s own module doc comment discloses every DRM-driven step call
    /// passes today -- "Controls are not yet wired") this falls back to that wheel's own
    /// declared, hashed [`AttitudeWheelsSpec::wheel_commanded_torque`] (M22.1b, question 152) --
    /// the same "bake the actuation into the model, there is no runtime control input yet"
    /// choice `super::binding::ConstantAccelModel`'s own constant `a` already makes. A wheel
    /// whose [`AttitudeWheelsSpec::wheel_available`] entry is `false` exerts zero torque
    /// regardless of either source -- see `super::fault::apply_dynamics_fault`'s own
    /// `BindingPlan::Attitude` arm for how a DYNAMICS fault flips this.
    fn derivatives(&self, state: &[f64], _t_tai_ns: i64, controls: &[f64], out: &mut [f64]) -> Result<(), Self::Error> {
        let n_wheels = self.wheel_axes.len();
        debug_assert_eq!(state.len(), self.dim, "AttitudeWheelsModel::derivatives: state length must equal the declared dimension");
        debug_assert_eq!(out.len(), self.dim, "AttitudeWheelsModel::derivatives: out length must equal the declared dimension");
        debug_assert!(controls.is_empty() || controls.len() == n_wheels, "AttitudeWheelsModel::derivatives: a non-empty controls must cover every wheel (got {} for {n_wheels} wheel(s))", controls.len());

        let q = [state[0], state[1], state[2], state[3]];
        let omega = [state[4], state[5], state[6]];

        let mut h_w_vec = [0.0; 3];
        let mut tau_w_vec = [0.0; 3];
        let mut dh = vec![0.0; n_wheels];
        for i in 0..n_wheels {
            let axis = self.wheel_axes[i];
            let h_i = state[crate::trajectory::ATTITUDE_WHEELS_BASE_COMPONENTS + i];
            let limit = self.wheel_momentum_limits[i];
            let commanded = if !self.wheel_available[i] {
                // M22.1b (question 152): an unavailable wheel exerts no torque at all,
                // regardless of any explicit `controls` entry or its own declared
                // `wheel_commanded_torque` -- it does not lose whatever momentum it already
                // holds (still contributes to h_w_vec below), only its own actuation authority.
                0.0
            } else {
                controls.get(i).copied().unwrap_or(self.wheel_commanded_torque[i])
            };
            // Declared saturation: a wheel already at its limit accepts no further push
            // outward, in either direction -- the clamp acts on the *derivative*, so the
            // ODE's own solution naturally never exceeds the declared limit (up to the
            // integrator's own local truncation error), rather than the state being clamped
            // after the fact.
            let at_positive_limit = h_i >= limit && commanded > 0.0;
            let at_negative_limit = h_i <= -limit && commanded < 0.0;
            let tau_eff = if at_positive_limit || at_negative_limit { 0.0 } else { commanded };
            dh[i] = tau_eff;
            for k in 0..3 {
                h_w_vec[k] += axis[k] * h_i;
                tau_w_vec[k] += axis[k] * tau_eff;
            }
        }

        let j_omega = mat3_vec3_mul(&self.inertia, omega);
        let total = [j_omega[0] + h_w_vec[0], j_omega[1] + h_w_vec[1], j_omega[2] + h_w_vec[2]];
        let gyro = cross3(omega, total);
        let rhs = [-gyro[0] - tau_w_vec[0], -gyro[1] - tau_w_vec[1], -gyro[2] - tau_w_vec[2]];
        let omega_dot = mat3_vec3_mul(&self.inertia_inv, rhs);

        let qv = [q[0], q[1], q[2]];
        let qw = q[3];
        let qv_cross_omega = cross3(qv, omega);
        out[0] = 0.5 * (qw * omega[0] + qv_cross_omega[0]);
        out[1] = 0.5 * (qw * omega[1] + qv_cross_omega[1]);
        out[2] = 0.5 * (qw * omega[2] + qv_cross_omega[2]);
        out[3] = -0.5 * (qv[0] * omega[0] + qv[1] * omega[1] + qv[2] * omega[2]);

        out[4] = omega_dot[0];
        out[5] = omega_dot[1];
        out[6] = omega_dot[2];

        let base = crate::trajectory::ATTITUDE_WHEELS_BASE_COMPONENTS;
        out[base..base + n_wheels].copy_from_slice(&dh);
        Ok(())
    }

    // Pure attitude/wheel dynamics -- no declared `PacketCodec`, so never emits telemetry mapped
    // to a CDM measurement. `sensors::TruthBroadcastAttitude<AttitudeWheelsModel>` (the wrapper
    // every "attitude." instance actually binds through) broadcasts truth via SIGNAL ports, not
    // through this measurement path.
    fn last_measurements(&self) -> Vec<av_cdm::pb::Measurement> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trajectory::attitude_wheels_state_space;

    fn params(entries: &[(&str, f64)]) -> BTreeMap<String, Parameter> {
        entries.iter().map(|(name, value)| (name.to_string(), Parameter { name: name.to_string(), value: *value, ..Default::default() })).collect()
    }

    fn build(spec: &AttitudeWheelsSpec) -> AttitudeWheelsModel {
        let space = attitude_wheels_state_space("test.attitude", spec.wheel_axes.len());
        AttitudeWheelsModel::new(spec, &space, "test.attitude_wheels").expect("valid spec must construct")
    }

    /// Rotate body-frame vector `v` into the frame `q` (scalar-last, body-to-inertial) maps
    /// into, via the standard quaternion Rodrigues formula: `v' = v + 2 q_w (q_v x v) + 2 q_v x
    /// (q_v x v)`. Used only by the momentum-conservation golden below to express the
    /// body-frame conserved quantity in the inertial frame; independently verified (in this
    /// same function's own unit test below) against the textbook case of a pure rotation about
    /// z, so a sign error here could not masquerade as a passing physics golden.
    fn rotate_body_to_inertial(q: [f64; 4], v: [f64; 3]) -> [f64; 3] {
        let qv = [q[0], q[1], q[2]];
        let qw = q[3];
        let t1 = cross3(qv, v);
        let t2 = cross3(qv, t1);
        [v[0] + 2.0 * qw * t1[0] + 2.0 * t2[0], v[1] + 2.0 * qw * t1[1] + 2.0 * t2[1], v[2] + 2.0 * qw * t1[2] + 2.0 * t2[2]]
    }

    #[test]
    fn rotate_body_to_inertial_matches_the_textbook_rotation_about_z() {
        let theta = 0.7_f64;
        let q = [0.0, 0.0, (theta / 2.0).sin(), (theta / 2.0).cos()];
        let got = rotate_body_to_inertial(q, [1.0, 0.0, 0.0]);
        assert!((got[0] - theta.cos()).abs() < 1e-12, "{got:?}");
        assert!((got[1] - theta.sin()).abs() < 1e-12, "{got:?}");
        assert!(got[2].abs() < 1e-12, "{got:?}");
    }

    // ---------------------------------------------------------------------------------------
    // parse_attitude_spec: typed refusal.
    // ---------------------------------------------------------------------------------------

    #[test]
    fn parse_attitude_spec_refuses_an_unrecognized_parameter_name() {
        let p = params(&[("attitude.nonsense", 1.0)]);
        let err = parse_attitude_spec(&p).unwrap_err();
        assert!(matches!(err, AttitudeSpecError::UnknownParameter { ref name } if name == "attitude.nonsense"), "{err}");
    }

    #[test]
    fn parse_attitude_spec_refuses_a_missing_required_inertia_component() {
        let p = params(&[("attitude.inertia.jyy", 1.0), ("attitude.inertia.jzz", 1.0)]);
        let err = parse_attitude_spec(&p).unwrap_err();
        assert!(matches!(err, AttitudeSpecError::MissingParameter { ref name } if name == "attitude.inertia.jxx"), "{err}");
    }

    #[test]
    fn parse_attitude_spec_refuses_a_non_positive_definite_inertia_tensor() {
        // jxy = 100 is far larger than sqrt(jxx*jyy) = 10 -- Sylvester's second minor is
        // negative, a physically impossible inertia tensor.
        let mut p = params(&[("attitude.inertia.jxx", 10.0), ("attitude.inertia.jyy", 10.0), ("attitude.inertia.jzz", 10.0), ("attitude.inertia.jxy", 100.0)]);
        p.extend(params(&[("attitude.q0.x", 0.0), ("attitude.q0.y", 0.0), ("attitude.q0.z", 0.0), ("attitude.q0.w", 1.0), ("attitude.omega0.x", 0.0), ("attitude.omega0.y", 0.0), ("attitude.omega0.z", 0.0)]));
        let err = parse_attitude_spec(&p).unwrap_err();
        assert!(matches!(err, AttitudeSpecError::InvalidParameter { ref name, .. } if name == "attitude.inertia"), "{err}");
    }

    #[test]
    fn parse_attitude_spec_refuses_a_non_unit_initial_quaternion() {
        let mut p = params(&[("attitude.inertia.jxx", 1.0), ("attitude.inertia.jyy", 1.0), ("attitude.inertia.jzz", 1.0)]);
        p.extend(params(&[("attitude.q0.x", 0.0), ("attitude.q0.y", 0.0), ("attitude.q0.z", 0.0), ("attitude.q0.w", 2.0), ("attitude.omega0.x", 0.0), ("attitude.omega0.y", 0.0), ("attitude.omega0.z", 0.0)]));
        let err = parse_attitude_spec(&p).unwrap_err();
        assert!(matches!(err, AttitudeSpecError::InvalidParameter { ref name, .. } if name == "attitude.q0"), "{err}");
    }

    #[test]
    fn parse_attitude_spec_refuses_a_non_unit_wheel_axis() {
        let mut p = params(&[("attitude.inertia.jxx", 1.0), ("attitude.inertia.jyy", 1.0), ("attitude.inertia.jzz", 1.0)]);
        p.extend(params(&[("attitude.q0.x", 0.0), ("attitude.q0.y", 0.0), ("attitude.q0.z", 0.0), ("attitude.q0.w", 1.0), ("attitude.omega0.x", 0.0), ("attitude.omega0.y", 0.0), ("attitude.omega0.z", 0.0)]));
        p.extend(params(&[("attitude.wheel.1.axis_x", 2.0), ("attitude.wheel.1.axis_y", 0.0), ("attitude.wheel.1.axis_z", 0.0), ("attitude.wheel.1.momentum_limit", 1.0)]));
        let err = parse_attitude_spec(&p).unwrap_err();
        assert!(matches!(err, AttitudeSpecError::InvalidParameter { ref name, .. } if name == "attitude.wheel.1.axis"), "{err}");
    }

    #[test]
    fn parse_attitude_spec_refuses_noncontiguous_wheel_indices() {
        let mut p = params(&[("attitude.inertia.jxx", 1.0), ("attitude.inertia.jyy", 1.0), ("attitude.inertia.jzz", 1.0)]);
        p.extend(params(&[("attitude.q0.x", 0.0), ("attitude.q0.y", 0.0), ("attitude.q0.z", 0.0), ("attitude.q0.w", 1.0), ("attitude.omega0.x", 0.0), ("attitude.omega0.y", 0.0), ("attitude.omega0.z", 0.0)]));
        // Wheel 2 declared, wheel 1 never declared -- not contiguous from 1.
        p.extend(params(&[("attitude.wheel.2.axis_x", 1.0), ("attitude.wheel.2.axis_y", 0.0), ("attitude.wheel.2.axis_z", 0.0), ("attitude.wheel.2.momentum_limit", 1.0)]));
        let err = parse_attitude_spec(&p).unwrap_err();
        assert!(matches!(err, AttitudeSpecError::InvalidParameter { .. }), "{err}");
    }

    #[test]
    fn new_refuses_a_state_space_whose_width_disagrees_with_the_parsed_wheel_count() {
        let mut p = params(&[("attitude.inertia.jxx", 1.0), ("attitude.inertia.jyy", 1.0), ("attitude.inertia.jzz", 1.0)]);
        p.extend(params(&[("attitude.q0.x", 0.0), ("attitude.q0.y", 0.0), ("attitude.q0.z", 0.0), ("attitude.q0.w", 1.0), ("attitude.omega0.x", 0.0), ("attitude.omega0.y", 0.0), ("attitude.omega0.z", 0.0)]));
        p.extend(params(&[("attitude.wheel.1.axis_x", 1.0), ("attitude.wheel.1.axis_y", 0.0), ("attitude.wheel.1.axis_z", 0.0), ("attitude.wheel.1.momentum_limit", 1.0)]));
        let spec = parse_attitude_spec(&p).unwrap();
        assert_eq!(spec.wheel_axes.len(), 1);
        // Declares a 0-wheel (7-component) shape for a 1-wheel spec -- must be refused, not
        // silently truncated or padded.
        let wrong_space = attitude_wheels_state_space("test.attitude", 0);
        let err = AttitudeWheelsModel::new(&spec, &wrong_space, "test.model").unwrap_err();
        assert_eq!(err, AttitudeSpecError::StateSpaceDimensionMismatch { declared_dim: 7, expected_dim: 8 });
    }

    // ---------------------------------------------------------------------------------------
    // Golden 1: torque-free axisymmetric precession.
    //
    // J = diag(Jt, Jt, Jz), no wheels, no external torque. Closed form (Euler's equations for
    // an axisymmetric body): omega_z(t) = omega_z(0) exactly for all t, and the transverse rate
    // vector (omega_x, omega_y) rotates rigidly at the analytic rate
    // lambda = omega_z(0) * (Jz - Jt) / Jt:
    //   omega_x(t) = omega_x(0) cos(lambda t) - omega_y(0) sin(lambda t)
    //   omega_y(t) = omega_x(0) sin(lambda t) + omega_y(0) cos(lambda t)
    // Fails an implementation that drops the -omega x (J omega) gyroscopic term entirely (that
    // implementation would hold omega_x/omega_y constant forever, not precessing at all -- a
    // deviation many orders larger than the tolerance below) or that gets its sign/magnitude
    // wrong (a wrong lambda desynchronizes the rotation within a fraction of one cycle).
    // ---------------------------------------------------------------------------------------

    #[test]
    fn torque_free_axisymmetric_precession_matches_the_closed_form() {
        let (jt, jz) = (100.0_f64, 50.0_f64);
        let spec = AttitudeWheelsSpec { inertia: [[jt, 0.0, 0.0], [0.0, jt, 0.0], [0.0, 0.0, jz]], wheel_axes: vec![], wheel_momentum_limits: vec![], q0: [0.0, 0.0, 0.0, 1.0], omega0: [0.05, 0.03, 0.2], wheel_available: vec![], wheel_commanded_torque: vec![] };
        let model = build(&spec);
        let lambda = spec.omega0[2] * (jz - jt) / jt;
        assert!((lambda - (-0.1)).abs() < 1e-12, "lambda sanity check: {lambda}");

        let mut state = model.initial_state(&spec);
        let dt_ns = 1_000_000_000i64; // 1 s native steps
        let mut max_wz_err = 0.0_f64;
        let mut max_transverse_err = 0.0_f64;
        for k in 0..=30 {
            let t = k as f64;
            let want_wz = spec.omega0[2];
            let want_wx = spec.omega0[0] * (lambda * t).cos() - spec.omega0[1] * (lambda * t).sin();
            let want_wy = spec.omega0[0] * (lambda * t).sin() + spec.omega0[1] * (lambda * t).cos();
            max_wz_err = max_wz_err.max((state[6] - want_wz).abs());
            max_transverse_err = max_transverse_err.max(((state[4] - want_wx).powi(2) + (state[5] - want_wy).powi(2)).sqrt());
            if k < 30 {
                let r = model.step(&state, k as i64 * dt_ns, &[], dt_ns).unwrap();
                state = r.state;
            }
        }
        // Dopri5 here runs at rtol = atol = 1e-12 (av_dynamics::integrate::Dopri5::default,
        // av_dynamics::DynamicsModel::integrator's own default). Measured (this environment)
        // over 30 accepted 1-second steps of this smooth, non-stiff RHS: max_wz_err = 0.0
        // exactly and max_transverse_err = 2.55e-14. 1e-12/1e-9 below are disclosed safety
        // margins above that measurement (for sub-ULP floating-point-rounding-order variation
        // across platforms/compilers), not tolerances tuned to this one run.
        assert!(max_wz_err < 1e-12, "omega_z drifted by {max_wz_err} over the run -- should be exactly constant");
        assert!(max_transverse_err < 1e-9, "transverse rate deviated from the closed-form precession by {max_transverse_err}");
    }

    // ---------------------------------------------------------------------------------------
    // Golden 2: angular momentum conservation with a non-diagonal inertia tensor and wheels
    // that start with nonzero momentum (no commanded torque -- purely internal exchange).
    //
    // Closed form: with no external torque, L_inertial = R(q) (J omega + h_w) is exactly
    // constant. Fails an implementation that drops or mis-signs the -omega x (J omega + h_w)
    // term (L_inertial would drift materially away from its t=0 value, since that term is
    // exactly what keeps the body-frame vector's *inertial* image constant while q rotates).
    // ---------------------------------------------------------------------------------------

    #[test]
    fn angular_momentum_is_conserved_in_the_inertial_frame_over_a_long_run() {
        let inertia = [[120.0, 5.0, -3.0], [5.0, 100.0, 2.0], [-3.0, 2.0, 80.0]];
        let axis1 = { let a = [1.0, 1.0, 0.0]; let n = norm3(a); [a[0] / n, a[1] / n, a[2] / n] };
        let axis2 = { let a = [0.0, 1.0, 1.0]; let n = norm3(a); [a[0] / n, a[1] / n, a[2] / n] };
        let spec = AttitudeWheelsSpec { inertia, wheel_axes: vec![axis1, axis2], wheel_momentum_limits: vec![1000.0, 1000.0], q0: [0.0, 0.0, 0.0, 1.0], omega0: [0.02, -0.01, 0.03], wheel_available: vec![true, true], wheel_commanded_torque: vec![0.0, 0.0] };
        let model = build(&spec);
        let mut state = model.initial_state(&spec);
        state[7] = 0.5;
        state[8] = -0.3;

        let l_body0 = {
            let jw = mat3_vec3_mul(&spec.inertia, [state[4], state[5], state[6]]);
            let hw = [axis1[0] * state[7] + axis2[0] * state[8], axis1[1] * state[7] + axis2[1] * state[8], axis1[2] * state[7] + axis2[2] * state[8]];
            [jw[0] + hw[0], jw[1] + hw[1], jw[2] + hw[2]]
        };
        let l_inertial0 = rotate_body_to_inertial([state[0], state[1], state[2], state[3]], l_body0);

        let dt_ns = 1_000_000_000i64;
        let mut max_drift = 0.0_f64;
        for k in 0..100 {
            let r = model.step(&state, k * dt_ns, &[0.0, 0.0], dt_ns).unwrap();
            state = r.state;
            let jw = mat3_vec3_mul(&spec.inertia, [state[4], state[5], state[6]]);
            let hw = [axis1[0] * state[7] + axis2[0] * state[8], axis1[1] * state[7] + axis2[1] * state[8], axis1[2] * state[7] + axis2[2] * state[8]];
            let l_body = [jw[0] + hw[0], jw[1] + hw[1], jw[2] + hw[2]];
            let l_inertial = rotate_body_to_inertial([state[0], state[1], state[2], state[3]], l_body);
            let drift = norm3([l_inertial[0] - l_inertial0[0], l_inertial[1] - l_inertial0[1], l_inertial[2] - l_inertial0[2]]);
            max_drift = max_drift.max(drift);
        }
        // Measured (this environment): |L0| = 3.4295, max_drift = 2.54e-12 over 100 accepted
        // 1-second steps. 1e-8 is a disclosed, ~4000x safety margin above that measurement
        // (~3e-9 relative to |L0|), not a value tuned to this one run.
        assert!(max_drift < 1e-8, "inertial angular momentum drifted by {max_drift} over 100 s (|L0| = {})", norm3(l_inertial0));
    }

    // ---------------------------------------------------------------------------------------
    // Golden 3: quaternion norm drift, disclosed, never silently renormalized.
    // ---------------------------------------------------------------------------------------

    #[test]
    fn quaternion_norm_drift_over_a_long_run_is_bounded_and_disclosed() {
        let spec = AttitudeWheelsSpec { inertia: [[100.0, 0.0, 0.0], [0.0, 100.0, 0.0], [0.0, 0.0, 50.0]], wheel_axes: vec![], wheel_momentum_limits: vec![], q0: [0.0, 0.0, 0.0, 1.0], omega0: [0.05, 0.03, 0.2], wheel_available: vec![], wheel_commanded_torque: vec![] };
        let model = build(&spec);
        let mut state = model.initial_state(&spec);
        let dt_ns = 1_000_000_000i64;
        let mut max_drift = 0.0_f64;
        for k in 0..200 {
            let r = model.step(&state, k * dt_ns, &[], dt_ns).unwrap();
            state = r.state;
            let n = norm3([state[0], state[1], state[2]]).hypot(state[3]);
            max_drift = max_drift.max((n - 1.0).abs());
        }
        // Measured (this environment) over 200 accepted 1-second steps: |q| drifts from unity
        // by 4.30e-12 at Dopri5's default rtol=atol=1e-12 -- no renormalization step exists
        // anywhere in `derivatives`/`step`; this is the integrator's own uncorrected
        // local-truncation-error accumulation, disclosed rather than hidden by a silent
        // renormalize. 1e-10 is a disclosed, ~20x safety margin above that measurement; a run
        // printing materially more drift than this is a real regression, not a tolerance to
        // loosen.
        assert!(max_drift < 1e-10, "quaternion norm drifted by {max_drift} over 200 s with no renormalization");
    }

    // ---------------------------------------------------------------------------------------
    // Golden 4: wheel momentum exchange, Delta(J omega) = -Delta(h_w).
    //
    // Starting from omega(0) = 0 and h_w(0) = 0 (total body-frame momentum exactly zero), no
    // external torque means L_inertial(t) = R(q(t)) * 0 = 0 for all t, hence L_body(t) = J
    // omega(t) + h_w(t) = 0 for all t exactly (not merely to leading order) -- so
    // Delta(J omega) = J omega(t) = -h_w(t) = -Delta(h_w) exactly, limited only by numerical
    // integration. Fails an implementation that "ignores the wheels" (omits the -tau_w reaction
    // term from Euler's equation while still integrating h_w_dot = tau_w for wheel bookkeeping
    // alone): omega would stay at 0 (nothing drives J omega_dot at all), so Delta(J omega) = 0
    // while -Delta(h_w) = -tau*t != 0.
    // ---------------------------------------------------------------------------------------

    #[test]
    fn wheel_torque_exchange_matches_delta_j_omega_equals_minus_delta_h_w() {
        let inertia = [[120.0, 5.0, -3.0], [5.0, 100.0, 2.0], [-3.0, 2.0, 80.0]];
        let axis1 = { let a = [1.0, 1.0, 0.0]; let n = norm3(a); [a[0] / n, a[1] / n, a[2] / n] };
        let axis2 = [0.0, 0.0, 1.0];
        let spec = AttitudeWheelsSpec { inertia, wheel_axes: vec![axis1, axis2], wheel_momentum_limits: vec![1000.0, 1000.0], q0: [0.0, 0.0, 0.0, 1.0], omega0: [0.0, 0.0, 0.0], wheel_available: vec![true, true], wheel_commanded_torque: vec![0.0, 0.0] };
        let model = build(&spec);
        let mut state = model.initial_state(&spec);
        let controls = [0.02_f64, -0.01_f64];
        let dt_ns = 1_000_000_000i64;
        let mut max_residual = 0.0_f64;
        for k in 0..10 {
            let r = model.step(&state, k * dt_ns, &controls, dt_ns).unwrap();
            state = r.state;
            let jw = mat3_vec3_mul(&spec.inertia, [state[4], state[5], state[6]]);
            let hw = [axis1[0] * state[7] + axis2[0] * state[8], axis1[1] * state[7] + axis2[1] * state[8], axis1[2] * state[7] + axis2[2] * state[8]];
            let residual = norm3([jw[0] + hw[0], jw[1] + hw[1], jw[2] + hw[2]]);
            max_residual = max_residual.max(residual);
        }
        // Measured (this environment): max_residual = 9.31e-17 over 10 accepted 1-second steps
        // (|h_w| reaches ~0.2 over the run). 1e-10 is a disclosed, ~1e6x safety margin above
        // that measurement, not a value tuned to this one run.
        assert!(max_residual < 1e-10, "J*omega + h_w departed from exact zero by {max_residual} -- Delta(J omega) != -Delta(h_w)");
    }

    // ---------------------------------------------------------------------------------------
    // Golden 5: saturation. A single wheel, axis aligned with a principal (isotropic) inertia
    // axis, driven by a constant torque past its declared momentum limit.
    //
    // Closed form (isotropic J = j*I and a single wheel on a principal axis keep omega
    // collinear with that axis for all time, so the transverse components never activate the
    // gyroscopic term -- it is identically zero along this trajectory):
    //   h_w(t)    = min(tau * t, limit)
    //   omega_x(t) = -h_w(t) / j            (from Delta(J omega) = -Delta(h_w), golden 4's
    //                                         identity, specialized to this 1-D case)
    //   omega_y(t) = omega_z(t) = 0
    // t* = limit / tau is the exact saturation instant. Fails an implementation that never
    // clamps h_w_dot at the limit (h_w(t) would keep growing past `limit` linearly forever,
    // and omega_x would keep growing past -limit/j too).
    // ---------------------------------------------------------------------------------------

    #[test]
    fn a_wheel_driven_past_its_limit_stops_accumulating_momentum_and_the_body_responds_accordingly() {
        let j = 10.0_f64;
        let (tau, limit) = (0.4_f64, 1.0_f64);
        let spec = AttitudeWheelsSpec { inertia: [[j, 0.0, 0.0], [0.0, j, 0.0], [0.0, 0.0, j]], wheel_axes: vec![[1.0, 0.0, 0.0]], wheel_momentum_limits: vec![limit], q0: [0.0, 0.0, 0.0, 1.0], omega0: [0.0, 0.0, 0.0], wheel_available: vec![true], wheel_commanded_torque: vec![0.0] };
        let model = build(&spec);
        let mut state = model.initial_state(&spec);
        let t_star = limit / tau; // 2.5 s

        let dt_ns = 100_000_000i64; // 0.1 s native steps, run to 5 s (well past t*)
        let mut max_h_err = 0.0_f64;
        let mut max_w_err = 0.0_f64;
        let mut saw_saturated = false;
        let mut saw_unsaturated = false;
        for k in 0..=50 {
            let t = k as f64 * 0.1;
            let want_h = (tau * t).min(limit);
            let want_wx = -want_h / j;
            max_h_err = max_h_err.max((state[7] - want_h).abs());
            max_w_err = max_w_err.max((state[4] - want_wx).abs());
            assert!(state[7] <= limit + 1e-6, "wheel momentum {} exceeded its declared limit {limit} at t={t}", state[7]);
            assert!((state[5]).abs() < 1e-12 && (state[6]).abs() < 1e-12, "transverse rates activated unexpectedly at t={t}: {:?}", &state[5..7]);
            if t > t_star + 0.2 {
                assert!(model.wheel_is_saturated(&state, 0), "wheel_is_saturated must report true once past t* = {t_star}");
                saw_saturated = true;
            } else if t < t_star - 0.2 {
                assert!(!model.wheel_is_saturated(&state, 0), "wheel_is_saturated must report false before t* = {t_star}");
                saw_unsaturated = true;
            }
            if k < 50 {
                let r = model.step(&state, k as i64 * dt_ns, &[tau], dt_ns).unwrap();
                state = r.state;
            }
        }
        assert!(saw_saturated && saw_unsaturated, "test must actually cross the saturation boundary to be meaningful");
        // Measured (this environment): max_h_err = 2.18e-10, max_w_err = 2.18e-11 over 50
        // accepted 0.1 s steps (including the derivative kink at t* = 2.5 s). 1e-7 is a
        // disclosed, ~450x safety margin above the larger of those two measurements, not a
        // value tuned to this one run.
        assert!(max_h_err < 1e-7, "wheel momentum deviated from min(tau*t, limit) by {max_h_err}");
        assert!(max_w_err < 1e-7, "omega_x deviated from -h_w/J by {max_w_err}");
    }

    // ---------------------------------------------------------------------------------------
    // M22.1b (question 151): the wheel-momentum unit must be UNIT_NEWTON_METER_SECOND, never
    // UNIT_NEWTON_METER (M22.1's own documented approximation).
    // ---------------------------------------------------------------------------------------

    /// Fails against an implementation that never added `AttitudeWheelsModel::new`'s own unit
    /// check (M22.1's original behaviour: any declared unit on the wheel-momentum component was
    /// silently accepted) -- that implementation would construct successfully here instead of
    /// refusing.
    #[test]
    fn new_refuses_a_wheel_momentum_component_labelled_with_the_torque_unit() {
        let spec = AttitudeWheelsSpec {
            inertia: [[10.0, 0.0, 0.0], [0.0, 10.0, 0.0], [0.0, 0.0, 10.0]],
            wheel_axes: vec![[1.0, 0.0, 0.0]],
            wheel_momentum_limits: vec![1.0],
            q0: [0.0, 0.0, 0.0, 1.0],
            omega0: [0.0, 0.0, 0.0],
            wheel_available: vec![true],
            wheel_commanded_torque: vec![0.0],
        };
        // Built by hand, not via crate::trajectory::attitude_wheels_state_space, precisely to
        // reproduce the pre-question-151 shape that function itself used to emit: the wheel
        // momentum component labelled with the torque unit.
        let mut space = attitude_wheels_state_space("test.attitude_bad_unit", 1);
        let base = crate::trajectory::ATTITUDE_WHEELS_BASE_COMPONENTS;
        assert_eq!(space.components[base].unit, av_cdm::pb::Unit::NewtonMeterSecond as i32, "sanity: the fixed builder must already declare the correct unit before this test corrupts it");
        space.components[base].unit = av_cdm::pb::Unit::NewtonMeter as i32;

        let err = AttitudeWheelsModel::new(&spec, &space, "test.model").unwrap_err();
        assert!(
            matches!(err, AttitudeSpecError::WheelMomentumUnitIsTorque { ref label, got_unit_code } if label == "wheel_h_1" && got_unit_code == av_cdm::pb::Unit::NewtonMeter as i32),
            "{err}"
        );
    }

    /// A wheel-momentum component correctly declared `UNIT_NEWTON_METER_SECOND` (the normal,
    /// fixed-builder shape) must still construct -- this is the same spec/state-space pair as
    /// the refusal test above, with only the unit corruption removed, so any failure here would
    /// mean the new check itself is too strict (refusing the correct unit too), not that it is
    /// missing.
    #[test]
    fn new_accepts_a_wheel_momentum_component_correctly_labelled_newton_meter_second() {
        let spec = AttitudeWheelsSpec {
            inertia: [[10.0, 0.0, 0.0], [0.0, 10.0, 0.0], [0.0, 0.0, 10.0]],
            wheel_axes: vec![[1.0, 0.0, 0.0]],
            wheel_momentum_limits: vec![1.0],
            q0: [0.0, 0.0, 0.0, 1.0],
            omega0: [0.0, 0.0, 0.0],
            wheel_available: vec![true],
            wheel_commanded_torque: vec![0.0],
        };
        let space = attitude_wheels_state_space("test.attitude_good_unit", 1);
        AttitudeWheelsModel::new(&spec, &space, "test.model").expect("UNIT_NEWTON_METER_SECOND must be accepted");
    }

    // ---------------------------------------------------------------------------------------
    // M22.1b (question 151): inertia parameters must declare UNIT_KILOGRAM_METER_SQUARED (or
    // leave `unit` unset), never a mismatched unit such as the wheel-momentum torque unit.
    // ---------------------------------------------------------------------------------------

    /// Fails against an implementation that never checks `Parameter.unit` at all for the six
    /// inertia fields (parses `p.value` and ignores `p.unit` entirely, M22.1's original
    /// behaviour) -- that implementation would return `Ok` here instead of refusing.
    #[test]
    fn parse_attitude_spec_refuses_an_inertia_parameter_declared_with_the_wrong_unit() {
        let mut p = params(&[("attitude.inertia.jyy", 1.0), ("attitude.inertia.jzz", 1.0)]);
        p.insert("attitude.inertia.jxx".to_string(), Parameter { name: "attitude.inertia.jxx".to_string(), value: 1.0, unit: av_cdm::pb::Unit::NewtonMeter as i32, ..Default::default() });
        p.extend(params(&[("attitude.q0.x", 0.0), ("attitude.q0.y", 0.0), ("attitude.q0.z", 0.0), ("attitude.q0.w", 1.0), ("attitude.omega0.x", 0.0), ("attitude.omega0.y", 0.0), ("attitude.omega0.z", 0.0)]));
        let err = parse_attitude_spec(&p).unwrap_err();
        assert!(matches!(err, AttitudeSpecError::InvalidParameter { ref name, .. } if name == "attitude.inertia.jxx"), "{err}");
    }

    /// An inertia parameter declaring the *correct* unit, or leaving it unset (`UNIT_UNSPECIFIED`,
    /// the proto default -- every M22.1 fixture predating question 151), must still parse --
    /// proves the new check above is not over-strict.
    #[test]
    fn parse_attitude_spec_accepts_inertia_parameters_with_the_correct_unit_or_no_declared_unit() {
        let mut p: BTreeMap<String, Parameter> = BTreeMap::new();
        p.insert("attitude.inertia.jxx".to_string(), Parameter { name: "attitude.inertia.jxx".to_string(), value: 1.0, unit: av_cdm::pb::Unit::KilogramMeterSquared as i32, ..Default::default() });
        p.insert("attitude.inertia.jyy".to_string(), Parameter { name: "attitude.inertia.jyy".to_string(), value: 1.0, ..Default::default() }); // unit left UNIT_UNSPECIFIED
        p.insert("attitude.inertia.jzz".to_string(), Parameter { name: "attitude.inertia.jzz".to_string(), value: 1.0, ..Default::default() });
        p.extend(params(&[("attitude.q0.x", 0.0), ("attitude.q0.y", 0.0), ("attitude.q0.z", 0.0), ("attitude.q0.w", 1.0), ("attitude.omega0.x", 0.0), ("attitude.omega0.y", 0.0), ("attitude.omega0.z", 0.0)]));
        let spec = parse_attitude_spec(&p).expect("correct unit and unset unit must both be accepted");
        assert_eq!(spec.inertia[0][0], 1.0);
    }

    // ---------------------------------------------------------------------------------------
    // M22.1b (question 152): wheel_commanded_torque is the declared fallback actuation
    // `crate::schedule`'s always-empty `controls` needs to have any effect at all through a real
    // DRM run; wheel_available is the "a wheel's own availability" DYNAMICS-fault arm.
    // ---------------------------------------------------------------------------------------

    /// Fails against an implementation that keeps M22.1's original `controls.get(i).copied().
    /// unwrap_or(0.0)` (ignoring `wheel_commanded_torque` when `controls` does not cover a
    /// wheel): that implementation would leave the wheel's momentum at exactly 0.0 the whole
    /// run, deviating from `min(tau*t, limit)` by the full 0.4 rad (at t=1s) this test checks
    /// against, not by the disclosed numerical-integration tolerance.
    #[test]
    fn derivatives_falls_back_to_the_declared_commanded_torque_when_controls_is_empty() {
        let j = 10.0_f64;
        let (tau, limit) = (0.4_f64, 1.0_f64);
        let spec = AttitudeWheelsSpec {
            inertia: [[j, 0.0, 0.0], [0.0, j, 0.0], [0.0, 0.0, j]],
            wheel_axes: vec![[1.0, 0.0, 0.0]],
            wheel_momentum_limits: vec![limit],
            q0: [0.0, 0.0, 0.0, 1.0],
            omega0: [0.0, 0.0, 0.0],
            wheel_available: vec![true],
            wheel_commanded_torque: vec![tau],
        };
        let model = build(&spec);
        let mut state = model.initial_state(&spec);
        let dt_ns = 1_000_000_000i64; // 1 s
        // Empty controls -- exactly what crate::schedule's own module doc comment says every
        // real DRM step call passes today ("Controls are not yet wired").
        let r = model.step(&state, 0, &[], dt_ns).unwrap();
        state = r.state;
        // Golden 5's own closed form, specialized to t=1s (well before t* = limit/tau = 2.5s):
        // h_w(1s) = tau*1 = 0.4, omega_x(1s) = -h_w/J = -0.04.
        assert!((state[7] - 0.4).abs() < 1e-9, "wheel momentum with empty controls = {}; expected ~0.4 from the declared commanded_torque", state[7]);
        assert!((state[4] - (-0.04)).abs() < 1e-9, "omega_x with empty controls = {}; expected ~-0.04", state[4]);
    }

    /// Fails against an implementation that ignores `wheel_available` entirely (ANDs it into
    /// nothing, or never reads the field): that implementation would let the wheel accumulate
    /// momentum from its own declared `wheel_commanded_torque` exactly like the test above,
    /// rather than staying at exactly 0.0.
    #[test]
    fn an_unavailable_wheel_exerts_no_torque_even_with_a_nonzero_declared_commanded_torque() {
        let j = 10.0_f64;
        let (tau, limit) = (0.4_f64, 1.0_f64);
        let spec = AttitudeWheelsSpec {
            inertia: [[j, 0.0, 0.0], [0.0, j, 0.0], [0.0, 0.0, j]],
            wheel_axes: vec![[1.0, 0.0, 0.0]],
            wheel_momentum_limits: vec![limit],
            q0: [0.0, 0.0, 0.0, 1.0],
            omega0: [0.0, 0.0, 0.0],
            wheel_available: vec![false],
            wheel_commanded_torque: vec![tau],
        };
        let model = build(&spec);
        let mut state = model.initial_state(&spec);
        let dt_ns = 1_000_000_000i64;
        for k in 0..5 {
            // Explicit, nonzero `controls` here (not `&[]`) -- deliberately so this test cannot
            // pass merely because both `wheel_available` and the `controls`-empty fallback to
            // `wheel_commanded_torque` happen to agree on "no torque": a `commanded = controls.
            // get(i).copied().unwrap_or(0.0)` implementation with the `wheel_available` check
            // simply deleted (M22.1's own pre-question-152 behaviour) would let this explicit
            // `tau` command through and accumulate momentum, which this test would then catch.
            let r = model.step(&state, k * dt_ns, &[tau], dt_ns).unwrap();
            state = r.state;
        }
        assert_eq!(state[7], 0.0, "an unavailable wheel must never accumulate momentum, even from an explicit nonzero commanded torque");
        assert_eq!(state[4], 0.0, "with zero net wheel torque and zero initial rate, omega_x must stay exactly zero too");
    }
}
