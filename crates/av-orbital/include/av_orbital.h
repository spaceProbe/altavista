/* av_orbital.h -- the C ABI export of av-orbital's native derivative function.
 *
 * docs/native-dynamics-plan.md milestone N6, deliverable 1: "the C ABI export of the native
 * derivative function (question 9's hedge, ADR-002's 'portable form') with a test that a C
 * caller obtains the same derivative bytes". Question 9's hedge (docs/open-questions.md,
 * answered 2026-09-02): "our kernel first, C ABI export second" -- this crate is not the
 * engine, this header is the hedge for an engine that is not ours, so it exports the smallest
 * useful surface: build a model, evaluate its derivative at a state and epoch into a
 * caller-provided buffer, tear it down.
 *
 * Hand-written to match crates/av-orbital/src/ffi.rs exactly -- there is no cbindgen step in
 * this workspace (question 154: no network at build or test time, and cbindgen is not a
 * dependency anywhere in this repository), so the two are kept in sync BY HAND. If you change
 * one, change the other: crates/av-orbital/tests/c/av_orbital_ffi_test.c (compiled against
 * this exact header at test time) and crates/av-orbital/tests/ffi_c_caller.rs (which compares
 * this C path's own derivative bytes, bit for bit, against the same call made through the
 * ordinary Rust av_dynamics::DynamicsModel trait) are what catch the two drifting apart in
 * practice: a narrowed/widened integer or a reordered argument changes what the C test actually
 * computes (a link succeeds on name alone; it is `tests/c/av_orbital_ffi_test.c`'s own use of
 * every declared field, and the byte-exact comparison against the Rust path, that would turn a
 * silent signature drift into a loud, failing test).
 *
 * The model this handle wraps is `av_orbital::model::EarthGravityModel<av_orbital::fk5::
 * Fk5BodyFixedRotation>` -- Earth point-mass/spherical-harmonic gravity with N5's native
 * (GMAT-free, no cargo feature gate) IAU-76/FK5 body-fixed rotation, never
 * `frame_gmat::GmatBodyFixedRotation`. That is deliberate, not a placeholder: the whole point
 * of this hedge (N6's own charter, question 222(e): "not a driver") is a derivative function
 * that needs no GMAT library linked, so it is exactly the configuration this crate's own
 * `--no-default-features` gate already proves builds and runs GMAT-free. Third bodies, SRP and
 * drag are not exposed here -- an additive, larger surface is a future task's to add if a real
 * caller needs it; this hedge proves the mechanism, not the full model.
 *
 * # Panic strategy (read before calling anything below)
 *
 * Every function in this header wraps its entire Rust body in `std::panic::catch_unwind`,
 * converting any unexpected Rust panic into `AV_ORBITAL_ERR_PANIC` rather than unwinding across
 * the FFI boundary (undefined behaviour in the C ABI -- a Rust panic must never cross into a
 * foreign frame). This is defense in depth, not a concession that a panic is expected in normal
 * operation: `av-orbital`'s own contract (crates/av-orbital/src/model.rs's `OrbitalModelError`
 * doc comment) is "typed errors throughout -- no panic on any input a DRM could supply", so
 * every genuine failure mode already has a typed `AvOrbitalStatus` of its own below; the
 * `catch_unwind` wrapper exists only to convert whatever this crate's own contract has not yet
 * anticipated into a safe, checkable status code instead of a crash.
 *
 * # Thread safety
 *
 * A single `av_orbital_model_t*` is NOT safe to call concurrently from more than one thread
 * (no interior synchronisation is used, matching `av_orbital::model::EarthGravityModel`'s own
 * plain `&self` -- unsynchronised -- `derivatives`). Distinct handles are independent and may be
 * used from distinct threads concurrently.
 */
#ifndef AV_ORBITAL_H
#define AV_ORBITAL_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* The number of `double`s in this model's state vector and its derivative -- position (x, y, z)
 * then velocity (x, y, z), SI metres and metres per second, in the inertial frame named by the
 * model's construction (`"EarthMJ2000Eq"`). Always 6 for `av_orbital_model_t`; also obtainable
 * at runtime from `av_orbital_state_dim()` below, so a caller need not hard-code it twice. */
#define AV_ORBITAL_STATE_DIM ((size_t)6)

/* Opaque handle to one constructed native orbital model. Never inspect its fields (there is no
 * C-visible layout) -- only ever hold a pointer obtained from `av_orbital_model_new` and pass it
 * to `av_orbital_model_derivatives`/`av_orbital_model_free`. */
typedef struct av_orbital_model av_orbital_model_t;

/* Every status this header's functions can return. `AV_ORBITAL_OK` is always zero (a caller may
 * test `status != AV_ORBITAL_OK` without naming every failure variant); every other value is a
 * distinct, stable, named failure reason -- never renumbered across a release, so a caller may
 * safely switch on the exact value. */
typedef enum av_orbital_status {
    /* The call succeeded; any output parameters are valid. */
    AV_ORBITAL_OK = 0,
    /* A required pointer argument was NULL (a gravity/GMAT-root path, an output-model slot, or
     * a state/state-dot buffer). */
    AV_ORBITAL_ERR_NULL_ARGUMENT = 1,
    /* A `const char *` path argument was not valid UTF-8 (this crate's own `Path`/`str` boundary
     * requires it, exactly as every other `av-orbital` entry point does). */
    AV_ORBITAL_ERR_INVALID_UTF8 = 2,
    /* `state_len`/`state_dot_len` was not exactly `AV_ORBITAL_STATE_DIM` (6). */
    AV_ORBITAL_ERR_INVALID_LENGTH = 3,
    /* `av_orbital::fk5::Fk5BodyFixedRotation::new` failed -- the GMAT-root path did not contain
     * a readable `NUTATION.DAT`/EOP file pair (see that function's own doc comment for exactly
     * which files and why; this is plain filesystem access, never a link against GMAT's own
     * library -- `fk5.rs` carries no cargo feature gate). */
    AV_ORBITAL_ERR_ROTATION_CONSTRUCTION_FAILED = 4,
    /* `av_orbital::model::EarthGravityModel::new` failed -- the gravity file could not be read
     * or parsed (a missing file, or a degree/order the file does not contain). */
    AV_ORBITAL_ERR_MODEL_CONSTRUCTION_FAILED = 5,
    /* The bound model's own `derivatives` call returned a typed `OrbitalModelError` (a rotation
     * failure at the requested epoch; every other variant is unreachable through this reduced,
     * gravity-only C surface -- no third bodies, SRP or drag are ever configured here, so their
     * own error variants can never be produced through this header). */
    AV_ORBITAL_ERR_DERIVATIVES_FAILED = 6,
    /* An unexpected Rust panic was caught at the FFI boundary -- see this header's own "Panic
     * strategy" section above. Never expected in normal operation. */
    AV_ORBITAL_ERR_PANIC = 7,
} av_orbital_status_t;

/* Returns `AV_ORBITAL_STATE_DIM` (always 6) -- cannot fail, cannot panic, takes no lock and
 * touches no model. Lets a caller size its own buffers without hard-coding the constant twice. */
size_t av_orbital_state_dim(void);

/* Constructs a new model: Earth point-mass/spherical-harmonic gravity read from the `.cof` file
 * at `gravity_file_path` (a GMAT gravity-coefficient file, e.g. `.../data/gravity/earth/
 * JGM2.cof`) truncated to `max_degree`/`max_order`, with the native (GMAT-free) IAU-76/FK5
 * body-fixed rotation for Earth, whose own EOP/nutation data files are read from under
 * `gmat_root` (a GMAT install root, e.g. the value of the `GMAT_ROOT` environment variable this
 * repository's own tooling already uses -- see `av_orbital::fk5::Fk5BodyFixedRotation::new`'s
 * own doc comment for exactly which files under it).
 *
 * On `AV_ORBITAL_OK`, `*out_model` is set to a freshly heap-allocated handle that the caller
 * must eventually pass to `av_orbital_model_free` exactly once. On any other status,
 * `*out_model` is left UNCHANGED (never set to NULL, never partially initialised) -- a caller
 * that only ever reads `*out_model` after checking the status will never see a stale value.
 *
 * `gravity_file_path` and `gmat_root` must be non-NULL, NUL-terminated, valid UTF-8 strings;
 * `out_model` must be a non-NULL pointer to a valid `av_orbital_model_t*` slot the caller owns.
 * This function performs plain filesystem reads only -- it never links against or starts a GMAT
 * engine (see this header's own module doc comment). */
av_orbital_status_t av_orbital_model_new(
    const char *gravity_file_path,
    size_t max_degree,
    size_t max_order,
    const char *gmat_root,
    av_orbital_model_t **out_model);

/* Destroys a model previously returned by `av_orbital_model_new`. `model` may be NULL (a no-op,
 * matching `free(3)`'s own convention); otherwise it must be a still-valid handle this call has
 * not already freed -- freeing the same handle twice, or using it afterwards, is undefined
 * behaviour, exactly as for any other heap-allocated C handle. */
void av_orbital_model_free(av_orbital_model_t *model);

/* Evaluates the model's derivative -- `[d(pos)/dt; d(vel)/dt] = [vel; accel]`, SI metres,
 * metres/second and metres/second^2, inertial frame -- for `state` (length `state_len`, must be
 * `AV_ORBITAL_STATE_DIM`) at the absolute epoch `t_tai_ns` (TAI nanoseconds since the CDM epoch,
 * matching `av_dynamics::DynamicsModel::derivatives`'s own `t_tai_ns` convention exactly),
 * writing the result into the caller-owned buffer `state_dot` (length `state_dot_len`, must
 * also be `AV_ORBITAL_STATE_DIM`).
 *
 * No `controls` parameter -- unlike the Rust `av_dynamics::DynamicsModel::derivatives` trait
 * method this wraps, which takes one: this model (Earth point-mass/spherical-harmonic gravity)
 * has no control input at all, so this function calls it internally with an empty slice rather
 * than exposing a pointer/length pair through this header that could only ever legally be
 * `(NULL, 0)`.
 *
 * `model` and `state` must be non-NULL and valid for `state_len` reads; `state_dot` must be
 * non-NULL and valid for `state_dot_len` writes; `state` and `state_dot` may safely alias the
 * SAME buffer only if `state_len == state_dot_len` (Rust's own slice aliasing rules would
 * otherwise apply, but this function never holds a shared and a mutable borrow of the same
 * bytes at once internally, so same-buffer in-place use is sound) -- ordinary callers should
 * simply use two distinct buffers. On any status other than `AV_ORBITAL_OK`, `state_dot`'s
 * contents are UNCHANGED (this function never partially writes it). */
av_orbital_status_t av_orbital_model_derivatives(
    const av_orbital_model_t *model,
    const double *state,
    size_t state_len,
    int64_t t_tai_ns,
    double *state_dot,
    size_t state_dot_len);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* AV_ORBITAL_H */
