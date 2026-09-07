/*
 * adcs_control.h -- the M22.4 quaternion-feedback PD attitude control law, ported to C
 * (M23.3, docs/open-questions.md questions 142/146/147/149/153, docs/sil-plan.md's M23
 * milestone).
 *
 * Reproduces crates/av-kernel/src/drm/controller.rs's `signed_error_vector` and
 * `AttitudeControllerModel::step_with_ports`'s `tau_k = kp*qv_k + kd*omega_k` control law
 * exactly -- same formula, same operation order (so the two implementations are bit-for-bit
 * reproducible against each other, modulo compiler floating-point contraction choices; see
 * services/cfs/apps/adcs/unit-test/test_adcs_control.c for the disclosed tolerance this is
 * checked against) -- plus `crates/av-kernel/src/drm/sensors.rs`'s `quat_mul`/`quat_conj`/
 * `cross3`/`dot3` primitives the control law is built from, also copied with the identical
 * operation order.
 *
 * ## Control law
 *
 * Quaternion convention: **scalar-last**, `[x, y, z, w]` (matches `ADCS_Quat_t` below and the
 * wire packets in adcs_packets.h).
 *
 *   q_err = target_q^-1 (x) measured_q          (quat_mul(quat_conj(target_q), measured_q))
 *   sign  = q_err.w < 0 ? -1 : 1                 (shortest rotational path)
 *   qv    = sign * q_err.{x,y,z}
 *   tau_k = kp*qv_k + kd*omega_k                 (k = x, y, z; omega from the IMU's measured rate)
 *
 * Wheel k (k=1,2,3) is aligned with body axis k (x,y,z respectively) -- a documented
 * convention, not a general wheel-distribution solve (matches
 * `crate::drm::controller::AttitudeControllerModel::new`'s own doc comment: this task's closed
 * -loop demo is a fully-actuated, axis-aligned three-wheel plant).
 *
 * ## Gains (the M22.4 fixture's own values, drms/demo_attitude_control_controller.system.yaml)
 *
 * `kp = 0.25`, `kd = 5.0`, sized for exact critical damping against the truth plant's
 * `Jz = 50 kg*m^2` (drms/demo_attitude_control_truth.system.yaml): critical damping
 * (`zeta = 1`) requires `kp = kd^2 / (2*Jz)`; with `kd = 5.0`, `Jz = 50`, `kp = 25/100 = 0.25`
 * exactly. Closed-form envelope time constant `tau = 2*Jz/kd = 20 s`.
 *
 * ## Sign convention -- the assertion that matters most
 *
 * A positive attitude error about an axis must produce a **stabilizing** (negative-feedback,
 * restoring) torque about that axis: `omega_dot_k = -(kp*qv_k + kd*omega_k)/J_k` (Euler's
 * equation for a wheel aligned with body axis k, small rate, no gyroscopic coupling --
 * `crate::drm::controller`'s own module doc comment has the full derivation). Flipping this
 * sign makes the loop diverge instead of settle (M22.4's own break test); see
 * services/cfs/apps/adcs/unit-test/test_adcs_control.c's stabilizing-sign test, which breaks
 * and restores exactly this sign to prove the assertion has teeth.
 *
 * No cFE/OSAL dependency here either (same reasoning as adcs_packets.h) -- this is the part of
 * the app that must be provably identical to the Rust controller regardless of whether it can
 * yet run inside a cFS scheduler cycle.
 */
#ifndef ADCS_CONTROL_H
#define ADCS_CONTROL_H

#ifdef __cplusplus
extern "C" {
#endif

/** The M22.4 fixture's own declared gains (drms/demo_attitude_control_controller.system.yaml
 *  `controller.kp`/`controller.kd`) -- sized for exact critical damping against Jz=50 with
 *  kd=5.0 (see this header's own top comment). Not a general-purpose default: a different
 *  plant inertia needs different gains re-derived from the same critical-damping formula. */
#define ADCS_CONTROLLER_KP_DEFAULT 0.25
#define ADCS_CONTROLLER_KD_DEFAULT 5.0

/** A quaternion, **scalar-last**: `[x, y, z, w]`. */
typedef struct {
    double x;
    double y;
    double z;
    double w;
} ADCS_Quat_t;

/** A body-frame 3-vector -- used for both measured rate (rad/s) and commanded torque (N*m). */
typedef struct {
    double x;
    double y;
    double z;
} ADCS_Vec3_t;

/** The identity quaternion (no rotation), scalar-last: `[0,0,0,1]`. */
static inline ADCS_Quat_t ADCS_QuatIdentity(void)
{
    ADCS_Quat_t q = {0.0, 0.0, 0.0, 1.0};
    return q;
}

/** `quat_conj(q) = [-x, -y, -z, w]` -- mirrors crate::drm::sensors::quat_conj exactly. */
ADCS_Quat_t ADCS_QuatConj(ADCS_Quat_t q);

/** Hamilton product, scalar-last: `ADCS_QuatMul(a, b)` composes rotations as "apply b's
 *  rotation first, then a's" -- mirrors crate::drm::sensors::quat_mul exactly, same operation
 *  order (`v = wa*vb + wb*va + cross(va,vb)`, `w = wa*wb - dot(va,vb)`). */
ADCS_Quat_t ADCS_QuatMul(ADCS_Quat_t a, ADCS_Quat_t b);

/** `q_err = target_q^-1 (x) measured_q`, then sign-flipped for the shortest rotational path
 *  (`sign(q_err.w)`) -- mirrors crate::drm::controller::signed_error_vector exactly. This is
 *  the exact quantity `ADCS_ComputeWheelTorque`'s `kp` term acts on, and (as `2*asin(|qv|)`,
 *  not implemented here -- see the Rust module's own `pointing_error_rad` output) the
 *  controller's own pointing-error belief. */
ADCS_Vec3_t ADCS_SignedErrorVector(ADCS_Quat_t target_q, ADCS_Quat_t measured_q);

/** The full control law: `tau_k = kp*qv_k + kd*omega_k` for k = x,y,z, with
 *  `qv = ADCS_SignedErrorVector(target_q, measured_q)`. `omega` is the IMU's own measured
 *  body rate (never the plant's truth rate -- measured attitude and rate only, per
 *  docs/open-questions.md question 142's "star tracker and IMU in" and the M22.4 module doc
 *  comment's "why the objective is scored from the controller's own belief, not the plant's
 *  truth"). */
ADCS_Vec3_t ADCS_ComputeWheelTorque(double kp, double kd, ADCS_Quat_t target_q, ADCS_Quat_t measured_q, ADCS_Vec3_t omega);

#ifdef __cplusplus
}
#endif

#endif /* ADCS_CONTROL_H */
