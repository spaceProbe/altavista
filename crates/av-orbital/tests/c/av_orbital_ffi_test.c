/* av_orbital_ffi_test.c -- the committed C caller for N6 deliverable 1's own acceptance test
 * (`docs/native-dynamics-plan.md`: "a test that a C caller obtains the same derivative bytes").
 *
 * Compiled and linked against the real `libav_orbital.a` (the `staticlib` crate-type this
 * task's own Cargo.toml change adds) by `crates/av-orbital/tests/ffi_c_caller.rs`, a Rust
 * integration test that shells out to the system `cc` -- see that file's own doc comment for
 * exactly how (the compile command, the link command, and where the native-static-libs list
 * linking the archive needs comes from). This file is never compiled by `cargo build`/`cargo
 * test` on its own; it has no meaning to Cargo at all, only to the test that invokes `cc` on it.
 *
 * Usage: `av_orbital_ffi_test <gravity_file_path> <gmat_root>`. On success, prints exactly six
 * lines to stdout, each one `state_dot[i]`'s raw IEEE-754 bit pattern as 16 lowercase hex
 * digits (via a `memcpy` into a `uint64_t`, never a decimal/`%f` round-trip, which is not
 * guaranteed bit-exact) and exits 0. On any failure, prints a one-line reason to stderr and
 * exits nonzero -- never partial stdout output on failure, so the Rust test can tell "no output
 * at all" apart from "some lines, then a crash".
 */
#include <inttypes.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "av_orbital.h"

/* The same LEO-shaped state and epoch every call in this repository's own GMAT-free tests uses
 * for a plain derivative check (see e.g. crates/av-orbital/tests/twobody_golden.rs's own
 * derivatives_first_three_components_equal_velocity_exactly) -- not a golden arc, just a fixed,
 * realistic six-state input both sides of this comparison evaluate the derivative at. */
static const double STATE[6] = {6878000.0, 0.0, 0.0, 0.0, 7500.0, 1000.0};
/* 2026-09-02T.., crate::fk5's own module test constant (TAI_NS), nanoseconds since the CDM
 * epoch (1970-01-01T00:00:00, av_cdm::time::Tai). This model is built with the real
 * Fk5BodyFixedRotation (never a no-op identity), so the epoch must fall inside
 * eopc04_08.62-now's covered date range or av_orbital_model_derivatives legitimately returns
 * AV_ORBITAL_ERR_DERIVATIVES_FAILED -- an arbitrary far-future value is not safe here the way
 * it would be for a point-mass-only, rotation-free comparison. Shared with the Rust side
 * (crates/av-orbital/tests/ffi_c_caller.rs), never independently chosen on each side. */
static const int64_t T_TAI_NS = 1788307237000000000LL;

static void print_status_and_exit(const char *what, av_orbital_status_t status) {
    fprintf(stderr, "%s failed: av_orbital_status_t = %d\n", what, (int)status);
    exit(1);
}

int main(int argc, char **argv) {
    if (argc != 3) {
        fprintf(stderr, "usage: %s <gravity_file_path> <gmat_root>\n", argv[0]);
        return 2;
    }
    const char *gravity_file_path = argv[1];
    const char *gmat_root = argv[2];

    if (av_orbital_state_dim() != AV_ORBITAL_STATE_DIM) {
        fprintf(stderr, "av_orbital_state_dim() = %zu, expected %zu\n", av_orbital_state_dim(), AV_ORBITAL_STATE_DIM);
        return 1;
    }

    av_orbital_model_t *model = NULL;
    av_orbital_status_t status = av_orbital_model_new(gravity_file_path, 8, 8, gmat_root, &model);
    if (status != AV_ORBITAL_OK) {
        print_status_and_exit("av_orbital_model_new", status);
    }
    if (model == NULL) {
        fprintf(stderr, "av_orbital_model_new returned AV_ORBITAL_OK but *out_model is NULL\n");
        return 1;
    }

    double state_dot[6];
    memset(state_dot, 0, sizeof(state_dot));
    status = av_orbital_model_derivatives(model, STATE, AV_ORBITAL_STATE_DIM, T_TAI_NS, state_dot, AV_ORBITAL_STATE_DIM);
    if (status != AV_ORBITAL_OK) {
        av_orbital_model_free(model);
        print_status_and_exit("av_orbital_model_derivatives", status);
    }

    av_orbital_model_free(model);

    /* d(pos)/dt must equal velocity exactly -- pinned here too, not just on the Rust side
     * (crates/av-orbital/tests/twobody_golden.rs's own
     * derivatives_first_three_components_equal_velocity_exactly), since this is the C caller's
     * own independent check that it received a real, sane derivative and not e.g. all zeros. */
    for (int i = 0; i < 3; i++) {
        if (state_dot[i] != STATE[3 + i]) {
            fprintf(stderr, "state_dot[%d] = %.17g, expected exactly STATE[%d] = %.17g (d(pos)/dt must equal velocity exactly)\n", i, state_dot[i], 3 + i, STATE[3 + i]);
            return 1;
        }
    }

    for (int i = 0; i < 6; i++) {
        uint64_t bits;
        memcpy(&bits, &state_dot[i], sizeof(bits));
        printf("%016" PRIx64 "\n", bits);
    }
    return 0;
}
