/*
 * adcs_control.c -- see adcs_control.h for the full derivation and the Rust source this
 * mirrors line for line (crates/av-kernel/src/drm/controller.rs,
 * crates/av-kernel/src/drm/sensors.rs). No cFE/OSAL dependency.
 */
#include "adcs_control.h"

/* dot3/cross3, copied from crate::drm::sensors with the identical operation order (floating
 * point addition/subtraction is not associative, so preserving the exact order is what makes
 * this implementation reproducible against the Rust one, not merely "the same formula"). */
static double ADCS_Dot3(ADCS_Vec3_t a, ADCS_Vec3_t b)
{
    return a.x * b.x + a.y * b.y + a.z * b.z;
}

static ADCS_Vec3_t ADCS_Cross3(ADCS_Vec3_t a, ADCS_Vec3_t b)
{
    ADCS_Vec3_t c;
    c.x = a.y * b.z - a.z * b.y;
    c.y = a.z * b.x - a.x * b.z;
    c.z = a.x * b.y - a.y * b.x;
    return c;
}

ADCS_Quat_t ADCS_QuatConj(ADCS_Quat_t q)
{
    ADCS_Quat_t r;
    r.x = -q.x;
    r.y = -q.y;
    r.z = -q.z;
    r.w = q.w;
    return r;
}

ADCS_Quat_t ADCS_QuatMul(ADCS_Quat_t a, ADCS_Quat_t b)
{
    ADCS_Vec3_t va = {a.x, a.y, a.z};
    double wa = a.w;
    ADCS_Vec3_t vb = {b.x, b.y, b.z};
    double wb = b.w;
    ADCS_Vec3_t c = ADCS_Cross3(va, vb);

    ADCS_Quat_t r;
    r.x = wa * vb.x + wb * va.x + c.x;
    r.y = wa * vb.y + wb * va.y + c.y;
    r.z = wa * vb.z + wb * va.z + c.z;
    r.w = wa * wb - ADCS_Dot3(va, vb);
    return r;
}

ADCS_Vec3_t ADCS_SignedErrorVector(ADCS_Quat_t target_q, ADCS_Quat_t measured_q)
{
    ADCS_Quat_t q_err = ADCS_QuatMul(ADCS_QuatConj(target_q), measured_q);
    /* Shortest rotational path: sign(q_err.w). This is the single line that, if flipped or
     * dropped, makes the loop diverge instead of settle (M22.4's own break test; see
     * unit-test/test_adcs_control.c's own break-and-restore evidence for this exact line). */
    double sign = (q_err.w < 0.0) ? -1.0 : 1.0;
    ADCS_Vec3_t qv;
    qv.x = q_err.x * sign;
    qv.y = q_err.y * sign;
    qv.z = q_err.z * sign;
    return qv;
}

ADCS_Vec3_t ADCS_ComputeWheelTorque(double kp, double kd, ADCS_Quat_t target_q, ADCS_Quat_t measured_q, ADCS_Vec3_t omega)
{
    ADCS_Vec3_t qv = ADCS_SignedErrorVector(target_q, measured_q);
    ADCS_Vec3_t tau;
    tau.x = kp * qv.x + kd * omega.x;
    tau.y = kp * qv.y + kd * omega.y;
    tau.z = kp * qv.z + kd * omega.z;
    return tau;
}
