/*
 * test_adcs_control.c -- host-side unit tests for adcs_control.c (the ported M22.4 control
 * law). Plain C harness (see adcs_test_framework.h for why: no cFS UT-assert available yet).
 *
 * Reference values for the "reproduces the same torque as the Rust controller" tests were
 * computed by running a standalone Rust program that copies
 * crates/av-kernel/src/drm/controller.rs's `signed_error_vector` and
 * crates/av-kernel/src/drm/sensors.rs's `quat_mul`/`quat_conj`/`cross3`/`dot3` verbatim (same
 * operation order) -- i.e. computed BY HAND from the module doc comment's formula, using
 * rustc directly (not `cargo test -p av-kernel`, and nothing under crates/ was modified or
 * executed) so the reference numbers are exact f64 arithmetic, not a manually-rounded
 * approximation. That throwaway program is not part of this deliverable; the numbers below are
 * copied from its output at 17-significant-digit precision. Case B independently
 * cross-checks by hand: sin(0.1) = 0.0998334166468282 (theta=0.2, half-angle 0.1),
 * tau_z = kp*sin(0.1) + kd*0.03 = 0.25*0.0998334166468282 + 5.0*0.03 = 0.174958354161707,
 * matching case_b below exactly.
 */
#include "adcs_test_framework.h"
#include "adcs_control.h"
#include <math.h>

/* Tolerance for the cross-language pinned-torque comparisons: both adcs_control.c and the Rust
 * reference implement the identical formula in the identical operation order (double/f64 are
 * both IEEE-754 binary64), so bit-exact agreement is expected in principle. This tolerance is
 * disclosed defensively against floating-point contraction (FMA) differences between rustc's
 * codegen and whatever C compiler builds this test -- this test suite's own build explicitly
 * passes -ffp-contract=off (see run_tests.sh) specifically to make this tolerance unnecessary
 * in practice; it is kept as a small, stated safety margin rather than a silently loosened
 * pass, not because a real discrepancy was observed. 1e-13 is ~3 orders of magnitude looser
 * than double epsilon (~2.2e-16) relative to these O(1e-1) magnitude values -- tight enough
 * that a genuine algorithmic error (wrong sign, wrong axis, missing term) still fails loudly.
 */
#define ADCS_CROSS_LANG_TOL 1e-13

static ADCS_Quat_t quat_about_axis(double axis_x, double axis_y, double axis_z, double theta)
{
    double n = sqrt(axis_x * axis_x + axis_y * axis_y + axis_z * axis_z);
    double s = sin(theta / 2.0);
    ADCS_Quat_t q;
    q.x = (axis_x / n) * s;
    q.y = (axis_y / n) * s;
    q.z = (axis_z / n) * s;
    q.w = cos(theta / 2.0);
    return q;
}

/* ---------------------------------------------------------------------------------------
 * "The control law reproduces the same torque as the Rust controller for a set of given
 * inputs." Two cases: identity target (case B) and a full quat_mul composition through a
 * non-trivial target (case A) -- the latter exercises ADCS_QuatMul/ADCS_QuatConj beyond the
 * "target=identity" shortcut the sign tests below use.
 * ------------------------------------------------------------------------------------- */

ADCS_TEST(case_b_identity_target_reproduces_rust_reference_torque)
{
    double theta = 0.2;
    ADCS_Quat_t target = ADCS_QuatIdentity();
    ADCS_Quat_t measured = {0.0, 0.0, sin(theta / 2.0), cos(theta / 2.0)};
    ADCS_Vec3_t omega = {0.01, -0.02, 0.03};

    ADCS_Vec3_t tau = ADCS_ComputeWheelTorque(0.25, 5.0, target, measured, omega);

    /* Rust reference (ref_controller.rs, case_b): [5.00000000000000028e-2,
     * -1.00000000000000006e-1, 1.74958354161707019e-1]. */
    ADCS_CHECK_NEAR(tau.x, 5.00000000000000028e-2, ADCS_CROSS_LANG_TOL);
    ADCS_CHECK_NEAR(tau.y, -1.00000000000000006e-1, ADCS_CROSS_LANG_TOL);
    ADCS_CHECK_NEAR(tau.z, 1.74958354161707019e-1, ADCS_CROSS_LANG_TOL);
}

ADCS_TEST(case_a_non_identity_target_reproduces_rust_reference_torque)
{
    /* target_q: 30 degrees about +x. measured_q = quat_mul(target_q, delta_q) where delta_q is
     * an 8 degree rotation about a mixed [0.3,-0.4,0.5] axis -- so q_err = target_q^-1 *
     * measured_q recovers (approximately, modulo float rounding through the composition)
     * delta_q, independently exercising the full Hamilton product, not just its
     * identity-target special case. */
    double theta_t = 30.0 * M_PI / 180.0;
    ADCS_Quat_t target = quat_about_axis(1.0, 0.0, 0.0, theta_t);

    double theta_d = 8.0 * M_PI / 180.0;
    ADCS_Quat_t delta = quat_about_axis(0.3, -0.4, 0.5, theta_d);
    ADCS_Quat_t measured = ADCS_QuatMul(target, delta);

    ADCS_Vec3_t omega = {0.01, -0.02, 0.03};
    ADCS_Vec3_t tau = ADCS_ComputeWheelTorque(0.25, 5.0, target, measured, omega);

    /* Rust reference (ref_controller.rs, case_a): [5.73987913424198679e-2,
     * -1.09865055123226474e-1, 1.62331318904033073e-1]. */
    ADCS_CHECK_NEAR(tau.x, 5.73987913424198679e-2, ADCS_CROSS_LANG_TOL);
    ADCS_CHECK_NEAR(tau.y, -1.09865055123226474e-1, ADCS_CROSS_LANG_TOL);
    ADCS_CHECK_NEAR(tau.z, 1.62331318904033073e-1, ADCS_CROSS_LANG_TOL);
}

/* ---------------------------------------------------------------------------------------
 * Sign convention -- "the assertion that matters most" per this app's own brief: a positive
 * attitude error about an axis must produce the *stabilizing* commanded torque. Per
 * adcs_control.h's own derivation, Euler's equation for a wheel-aligned-with-body-axis gives
 * `omega_dot_k = -(kp*qv_k + kd*omega_k)/J_k`: a positive qv_z (positive attitude error) must
 * command a *positive* tau_z so that the body's reaction (-tau_z/J_z, per
 * crate::drm::controller's own doc comment: "J*omega_dot = ... - tau_w") drives omega_z
 * negative and rotates the body back toward zero error -- i.e. the commanded sign that is
 * physically restoring. Fails against a sign-flipped control law, which would instead command
 * a torque that drives the error further from zero (an unstable, diverging loop -- M22.4's own
 * break test). Mirrors crates/av-kernel/src/drm/controller.rs's own
 * `commanded_z_torque_has_the_stabilizing_sign_for_a_positive_z_rotation_error` test exactly
 * (same theta, same reasoning).
 * ------------------------------------------------------------------------------------- */

ADCS_TEST(positive_z_rotation_error_commands_the_stabilizing_positive_z_torque)
{
    double theta = 0.2;
    ADCS_Quat_t target = ADCS_QuatIdentity();
    ADCS_Quat_t measured = {0.0, 0.0, sin(theta / 2.0), cos(theta / 2.0)};
    ADCS_Vec3_t qv = ADCS_SignedErrorVector(target, measured);
    ADCS_CHECK(qv.z > 0.0);

    ADCS_Vec3_t omega_zero = {0.0, 0.0, 0.0};
    ADCS_Vec3_t tau = ADCS_ComputeWheelTorque(0.5, 1.0, target, measured, omega_zero);
    ADCS_CHECK(tau.z > 0.0);
}

ADCS_TEST(negative_z_rotation_error_commands_the_opposite_sign_torque)
{
    double theta = -0.2;
    ADCS_Quat_t target = ADCS_QuatIdentity();
    ADCS_Quat_t measured = {0.0, 0.0, sin(theta / 2.0), cos(theta / 2.0)};
    ADCS_Vec3_t qv = ADCS_SignedErrorVector(target, measured);
    ADCS_CHECK(qv.z < 0.0);

    ADCS_Vec3_t omega_zero = {0.0, 0.0, 0.0};
    ADCS_Vec3_t tau = ADCS_ComputeWheelTorque(0.5, 1.0, target, measured, omega_zero);
    ADCS_CHECK(tau.z < 0.0);
}

/* ---------------------------------------------------------------------------------------
 * Shortest-path correction: sign(q_err.w). Only a q_err with a NEGATIVE w distinguishes this
 * from "no sign flip at all" -- a 200 degree rotation about +z has half-angle 100 degrees,
 * cos(100 deg) < 0.
 * ------------------------------------------------------------------------------------- */

ADCS_TEST(shortest_path_sign_flip_engages_for_a_negative_w_error_quaternion)
{
    double theta = 200.0 * M_PI / 180.0;
    ADCS_Quat_t target = ADCS_QuatIdentity();
    ADCS_Quat_t measured = {0.0, 0.0, sin(theta / 2.0), cos(theta / 2.0)};

    ADCS_Quat_t q_err = ADCS_QuatMul(ADCS_QuatConj(target), measured);
    ADCS_CHECK(q_err.w < 0.0); /* the case that actually exercises the sign flip */

    ADCS_Vec3_t qv = ADCS_SignedErrorVector(target, measured);
    /* Without the sign flip, qv.z would equal q_err.z (positive, sin(100deg) > 0). With the
     * flip applied (sign = -1 because q_err.w < 0), qv.z must be negative. */
    ADCS_CHECK(q_err.z > 0.0);
    ADCS_CHECK(qv.z < 0.0);
    ADCS_CHECK_NEAR(qv.z, -q_err.z, 1e-15);

    /* Rust reference (ref_controller.rs, case_c): qv = [-0.0, -0.0, -9.84807753012208020e-1],
     * tau (kp=0.25, kd=5.0, omega=0) = [0, 0, -2.46201938253052005e-1]. */
    ADCS_CHECK_NEAR(qv.z, -9.84807753012208020e-1, ADCS_CROSS_LANG_TOL);
    ADCS_Vec3_t omega_zero = {0.0, 0.0, 0.0};
    ADCS_Vec3_t tau = ADCS_ComputeWheelTorque(0.25, 5.0, target, measured, omega_zero);
    ADCS_CHECK_NEAR(tau.z, -2.46201938253052005e-1, ADCS_CROSS_LANG_TOL);
}

/* ---------------------------------------------------------------------------------------
 * Basic quaternion algebra sanity (not a Rust cross-check -- these are properties any correct
 * quat_mul/quat_conj must have, independent of any specific implementation).
 * ------------------------------------------------------------------------------------- */

ADCS_TEST(quat_mul_with_identity_is_a_no_op_on_either_side)
{
    ADCS_Quat_t q = {0.1, 0.2, 0.3, sqrt(1.0 - 0.01 - 0.04 - 0.09)};
    ADCS_Quat_t id = ADCS_QuatIdentity();
    ADCS_Quat_t a = ADCS_QuatMul(id, q);
    ADCS_Quat_t b = ADCS_QuatMul(q, id);
    ADCS_CHECK_NEAR(a.x, q.x, 1e-15);
    ADCS_CHECK_NEAR(a.y, q.y, 1e-15);
    ADCS_CHECK_NEAR(a.z, q.z, 1e-15);
    ADCS_CHECK_NEAR(a.w, q.w, 1e-15);
    ADCS_CHECK_NEAR(b.x, q.x, 1e-15);
    ADCS_CHECK_NEAR(b.w, q.w, 1e-15);
}

ADCS_TEST(zero_error_and_zero_rate_commands_exactly_zero_torque)
{
    ADCS_Quat_t target = ADCS_QuatIdentity();
    ADCS_Quat_t measured = ADCS_QuatIdentity();
    ADCS_Vec3_t omega = {0.0, 0.0, 0.0};
    ADCS_Vec3_t tau = ADCS_ComputeWheelTorque(ADCS_CONTROLLER_KP_DEFAULT, ADCS_CONTROLLER_KD_DEFAULT, target, measured, omega);
    ADCS_CHECK_EQ_INT(tau.x == 0.0, 1);
    ADCS_CHECK_EQ_INT(tau.y == 0.0, 1);
    ADCS_CHECK_EQ_INT(tau.z == 0.0, 1);
}

int main(void)
{
    printf("test_adcs_control:\n");
    ADCS_RUN_TEST(case_b_identity_target_reproduces_rust_reference_torque);
    ADCS_RUN_TEST(case_a_non_identity_target_reproduces_rust_reference_torque);
    ADCS_RUN_TEST(positive_z_rotation_error_commands_the_stabilizing_positive_z_torque);
    ADCS_RUN_TEST(negative_z_rotation_error_commands_the_opposite_sign_torque);
    ADCS_RUN_TEST(shortest_path_sign_flip_engages_for_a_negative_w_error_quaternion);
    ADCS_RUN_TEST(quat_mul_with_identity_is_a_no_op_on_either_side);
    ADCS_RUN_TEST(zero_error_and_zero_rate_commands_exactly_zero_torque);
    ADCS_TEST_SUMMARY_AND_EXIT();
}
