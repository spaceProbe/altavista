//! The C ABI export of the native derivative function (`docs/native-dynamics-plan.md`
//! milestone N6, deliverable 1: "question 9's hedge, ADR-002's 'portable form'"). Hand-mirrored
//! by `include/av_orbital.h` -- **read that header's own module doc comment first**, it states
//! the panic strategy, the thread-safety contract and exactly which model configuration this
//! handle wraps; this module's own doc comments below restate only what is Rust-specific.
//!
//! # Why `Fk5BodyFixedRotation`, never `GmatBodyFixedRotation`
//!
//! This is deliberately the GMAT-free half of this crate: [`crate::fk5::Fk5BodyFixedRotation`]
//! carries no cargo feature gate (unlike [`crate::frame_gmat::GmatBodyFixedRotation`], behind
//! `gmat-frames`), so this whole module -- and the `--no-default-features` build's own gate --
//! never touches `gmat-sys`. N6's own charter line (question 222(e)) is explicit that this
//! export "is N6, question 9's hedge, not a driver": the hedge is exactly a derivative function
//! a GMAT-free build can still expose to a foreign C caller, so wiring this handle to the
//! GMAT-backed rotation instead would defeat the one property this module exists to prove.
//!
//! # No `#[allow]`
//!
//! Every `pub unsafe extern "C" fn` below carries a `# Safety` doc section (clippy's
//! `missing_safety_doc`, on by default, would otherwise fail this crate's `-D warnings` gate)
//! rather than suppressing the lint.
use std::ffi::CStr;
use std::os::raw::c_char;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;

use av_dynamics::DynamicsModel;

use crate::fk5::Fk5BodyFixedRotation;
use crate::model::{EarthGravityModel, EarthGravityModelInfo};

/// `include/av_orbital.h`'s `AV_ORBITAL_STATE_DIM` -- this model's `state_dim()` is always 6
/// ([`crate::model::EarthGravityModel::state_dim`]), restated here as a plain Rust constant so
/// this module never hard-codes the literal `6` a second time.
pub const AV_ORBITAL_STATE_DIM: usize = 6;

/// The concrete model this opaque handle wraps -- see this module's own doc comment, "Why
/// `Fk5BodyFixedRotation`". Opaque to C on purpose: no `#[repr(C)]`, no field is ever read from
/// the C side, only a `*mut`/`*const AvOrbitalModel` obtained from [`av_orbital_model_new`] and
/// passed back to [`av_orbital_model_derivatives`]/[`av_orbital_model_free`].
pub struct AvOrbitalModel(EarthGravityModel<Fk5BodyFixedRotation>);

/// Mirrors `include/av_orbital.h`'s `av_orbital_status_t` exactly, member for member and value
/// for value -- `#[repr(i32)]` so the two really do agree on the wire (a plain C `enum`'s
/// underlying type is implementation-defined but `int`, i.e. 32-bit, on every platform and
/// compiler this workspace targets; see `include/av_orbital.h`'s own doc comment for what each
/// variant means and when it is returned).
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AvOrbitalStatus {
    Ok = 0,
    ErrNullArgument = 1,
    ErrInvalidUtf8 = 2,
    ErrInvalidLength = 3,
    ErrRotationConstructionFailed = 4,
    ErrModelConstructionFailed = 5,
    ErrDerivativesFailed = 6,
    ErrPanic = 7,
}

/// `include/av_orbital.h`'s `av_orbital_state_dim()`. Cannot fail, cannot panic (a plain
/// constant), takes no lock and touches no model -- no `catch_unwind` needed, unlike every
/// other function below.
#[no_mangle]
pub extern "C" fn av_orbital_state_dim() -> usize {
    AV_ORBITAL_STATE_DIM
}

/// `include/av_orbital.h`'s `av_orbital_model_new` -- see that header's own doc comment for the
/// full contract (argument meanings, what `*out_model` is left as on failure, the panic
/// strategy). This function is total over every input, including a malformed one: every branch
/// below returns a typed [`AvOrbitalStatus`] rather than dereferencing a pointer it has not
/// first checked non-NULL, and the whole body is wrapped in [`catch_unwind`] as defense in
/// depth (see `include/av_orbital.h`'s own "Panic strategy" section).
///
/// # Safety
///
/// `gravity_file_path` and `gmat_root` must each be either NULL or a valid pointer to a
/// NUL-terminated C string that remains valid for the duration of this call (this function does
/// not retain either pointer past its own return). `out_model` must be either NULL or a valid,
/// properly aligned pointer to a `*mut AvOrbitalModel` the caller owns and may write through.
#[no_mangle]
pub unsafe extern "C" fn av_orbital_model_new(
    gravity_file_path: *const c_char,
    max_degree: usize,
    max_order: usize,
    gmat_root: *const c_char,
    out_model: *mut *mut AvOrbitalModel,
) -> AvOrbitalStatus {
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        if gravity_file_path.is_null() || gmat_root.is_null() || out_model.is_null() {
            return AvOrbitalStatus::ErrNullArgument;
        }
        // SAFETY: both pointers were just checked non-NULL; the caller's contract (this
        // function's own `# Safety` section) is that each points to a valid, live
        // NUL-terminated C string for the duration of this call.
        let gravity_path = match unsafe { CStr::from_ptr(gravity_file_path) }.to_str() {
            Ok(s) => s,
            Err(_) => return AvOrbitalStatus::ErrInvalidUtf8,
        };
        // SAFETY: same as above, for `gmat_root`.
        let gmat_root_str = match unsafe { CStr::from_ptr(gmat_root) }.to_str() {
            Ok(s) => s,
            Err(_) => return AvOrbitalStatus::ErrInvalidUtf8,
        };

        let rotation = match Fk5BodyFixedRotation::new(Path::new(gmat_root_str)) {
            Ok(r) => r,
            Err(_) => return AvOrbitalStatus::ErrRotationConstructionFailed,
        };
        let info = EarthGravityModelInfo {
            id: "native.orbital.c_abi_export".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            // This handle is a hedge-mechanism demonstration, not a configuration flown against
            // any one golden -- see include/av_orbital.h's own doc comment for why third
            // bodies/SRP/drag are deliberately absent here.
            goldens: Vec::new(),
        };
        let model = match EarthGravityModel::new(Path::new(gravity_path), max_degree, max_order, "Earth", rotation, info) {
            Ok(m) => m,
            Err(_) => return AvOrbitalStatus::ErrModelConstructionFailed,
        };

        let boxed = Box::new(AvOrbitalModel(model));
        // SAFETY: `out_model` was checked non-NULL above, and the caller's contract requires it
        // to be a valid, properly aligned, writable `*mut AvOrbitalModel` slot.
        unsafe {
            *out_model = Box::into_raw(boxed);
        }
        AvOrbitalStatus::Ok
    }));
    outcome.unwrap_or(AvOrbitalStatus::ErrPanic)
}

/// `include/av_orbital.h`'s `av_orbital_model_free`.
///
/// # Safety
///
/// `model` must be either NULL (a no-op) or a pointer previously returned by
/// [`av_orbital_model_new`] via its `out_model` slot, not already passed to this function
/// before, and not used again (by any call, on any thread) after this call returns.
#[no_mangle]
pub unsafe extern "C" fn av_orbital_model_free(model: *mut AvOrbitalModel) {
    if model.is_null() {
        return;
    }
    // A `Drop` of `EarthGravityModel`/`Fk5BodyFixedRotation` is not expected to panic (neither
    // type owns anything whose `Drop` impl can fail), but this crosses back into C either way,
    // so the same defense-in-depth applies: catch, never unwind across the boundary.
    let _ = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: `model` was checked non-NULL above; the caller's contract (this function's
        // own `# Safety` section) is that it is a live handle from `av_orbital_model_new`,
        // freed at most once.
        drop(unsafe { Box::from_raw(model) });
    }));
}

/// `include/av_orbital.h`'s `av_orbital_model_derivatives` -- see that header's own doc comment
/// for the full contract (buffer lengths, why there is no `controls` parameter, aliasing rules,
/// what `state_dot` is left as on failure).
///
/// Deliberately seven parameters, not the `av_dynamics::DynamicsModel::derivatives` trait's own
/// eight (`state, t, controls, state_dot`, `controls` split into pointer and length) -- this
/// model has no control input (Earth point-mass/spherical-harmonic gravity; see this function's
/// own header doc comment), so a `controls`/`controls_len` pair that could only ever legally be
/// `(NULL, 0)` would document nothing and cost real interface complexity (clippy's default
/// `too_many_arguments` lint, among other things) for no caller benefit; [`EarthGravityModel::
/// derivatives`] is called with an empty slice literal below instead.
///
/// # Safety
///
/// `model` must be a valid pointer obtained from [`av_orbital_model_new`] (not yet freed).
/// `state` must be either NULL (only when `state_len == 0`, which is itself then a checked
/// [`AvOrbitalStatus::ErrInvalidLength`]) or valid for reading `state_len` `f64`s. `state_dot`
/// must be valid for reading AND writing `state_dot_len` `f64`s (this function reads nothing
/// from it, but Rust's `&mut` aliasing rule still requires the memory be valid for writes and
/// not simultaneously borrowed elsewhere for the duration of this call).
#[no_mangle]
pub unsafe extern "C" fn av_orbital_model_derivatives(
    model: *const AvOrbitalModel,
    state: *const f64,
    state_len: usize,
    t_tai_ns: i64,
    state_dot: *mut f64,
    state_dot_len: usize,
) -> AvOrbitalStatus {
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        if model.is_null() || state.is_null() || state_dot.is_null() {
            return AvOrbitalStatus::ErrNullArgument;
        }
        if state_len != AV_ORBITAL_STATE_DIM || state_dot_len != AV_ORBITAL_STATE_DIM {
            return AvOrbitalStatus::ErrInvalidLength;
        }

        // SAFETY: `model` was checked non-NULL above; the caller's contract (this function's
        // own `# Safety` section) is that it is a live handle from `av_orbital_model_new`.
        let model_ref = unsafe { &(*model).0 };
        // SAFETY: `state` was checked non-NULL above and `state_len == AV_ORBITAL_STATE_DIM`;
        // the caller's contract requires it valid for that many reads.
        let state_slice = unsafe { std::slice::from_raw_parts(state, state_len) };
        // SAFETY: `state_dot` was checked non-NULL above and `state_dot_len ==
        // AV_ORBITAL_STATE_DIM`; the caller's contract requires it valid for that many writes,
        // not aliased by any other live reference for the duration of this call.
        let state_dot_slice = unsafe { std::slice::from_raw_parts_mut(state_dot, state_dot_len) };

        // This model takes no control input -- see this function's own doc comment for why
        // there is no `controls` parameter to thread through here.
        match model_ref.derivatives(state_slice, t_tai_ns, &[], state_dot_slice) {
            Ok(()) => AvOrbitalStatus::Ok,
            Err(_) => AvOrbitalStatus::ErrDerivativesFailed,
        }
    }));
    outcome.unwrap_or(AvOrbitalStatus::ErrPanic)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    fn gravity_path() -> CString {
        let root = crate::cof::locate_gmat_root().expect("GMAT_ROOT set (this task's own environment rule)");
        CString::new(root.join("data/gravity/earth/JGM2.cof").to_str().unwrap()).unwrap()
    }

    fn gmat_root_cstring() -> CString {
        let root = crate::cof::locate_gmat_root().expect("GMAT_ROOT set (this task's own environment rule)");
        CString::new(root.to_str().unwrap()).unwrap()
    }

    #[test]
    fn state_dim_is_six() {
        assert_eq!(av_orbital_state_dim(), 6);
    }

    #[test]
    fn new_then_free_round_trips_with_null_checks_honoured() {
        let gravity = gravity_path();
        let gmat_root = gmat_root_cstring();
        let mut handle: *mut AvOrbitalModel = std::ptr::null_mut();

        // Every null-pointer branch is checked, never dereferenced.
        unsafe {
            assert_eq!(av_orbital_model_new(std::ptr::null(), 8, 8, gmat_root.as_ptr(), &mut handle), AvOrbitalStatus::ErrNullArgument);
            assert_eq!(av_orbital_model_new(gravity.as_ptr(), 8, 8, std::ptr::null(), &mut handle), AvOrbitalStatus::ErrNullArgument);
            assert_eq!(av_orbital_model_new(gravity.as_ptr(), 8, 8, gmat_root.as_ptr(), std::ptr::null_mut()), AvOrbitalStatus::ErrNullArgument);
            assert!(handle.is_null(), "a failed construction must never touch *out_model");
        }

        let status = unsafe { av_orbital_model_new(gravity.as_ptr(), 8, 8, gmat_root.as_ptr(), &mut handle) };
        assert_eq!(status, AvOrbitalStatus::Ok);
        assert!(!handle.is_null());

        unsafe { av_orbital_model_free(handle) };
        // A NULL free is a documented no-op, not a crash.
        unsafe { av_orbital_model_free(std::ptr::null_mut()) };
    }

    #[test]
    fn derivatives_rejects_a_wrong_length_state_and_a_null_buffer() {
        let gravity = gravity_path();
        let gmat_root = gmat_root_cstring();
        let mut handle: *mut AvOrbitalModel = std::ptr::null_mut();
        let status = unsafe { av_orbital_model_new(gravity.as_ptr(), 0, 0, gmat_root.as_ptr(), &mut handle) };
        assert_eq!(status, AvOrbitalStatus::Ok);

        let state = [6_878_000.0, 0.0, 0.0, 0.0, 5_000.0, 5_500.0];
        let mut dot5 = [0.0; 5];
        let mut dot6 = [0.0; 6];
        // 2026-09-02, `crate::fk5`'s own module test constant (`TAI_NS`) -- unlike
        // `tests/twobody_golden.rs`'s `IdentityRotation`-based fixture epoch (which never
        // touches a real EOP table so any i64 works), this model is built with the real
        // `Fk5BodyFixedRotation` above, so the epoch must actually fall inside
        // `eopc04_08.62-now`'s covered date range or `inertial_to_fixed` legitimately returns a
        // typed error -- see that file's own doc comment for why this exact value is known-good.
        const T_TAI_NS: i64 = 1_788_307_237_000_000_000;

        unsafe {
            assert_eq!(
                av_orbital_model_derivatives(handle, state.as_ptr(), 5, T_TAI_NS, dot5.as_mut_ptr(), 5),
                AvOrbitalStatus::ErrInvalidLength
            );
            assert_eq!(
                av_orbital_model_derivatives(handle, state.as_ptr(), 6, T_TAI_NS, dot5.as_mut_ptr(), 5),
                AvOrbitalStatus::ErrInvalidLength
            );
            assert_eq!(
                av_orbital_model_derivatives(handle, std::ptr::null(), 6, T_TAI_NS, dot6.as_mut_ptr(), 6),
                AvOrbitalStatus::ErrNullArgument
            );
            let status = av_orbital_model_derivatives(handle, state.as_ptr(), 6, T_TAI_NS, dot6.as_mut_ptr(), 6);
            assert_eq!(status, AvOrbitalStatus::Ok, "derivatives failed at a known-in-range epoch -- see this test's own T_TAI_NS doc comment");
            assert_eq!(dot6[0..3], state[3..6], "derivative of position must equal velocity exactly, through the C ABI too");
            av_orbital_model_free(handle);
        }
    }

    #[test]
    fn model_new_reports_a_typed_status_for_a_nonexistent_gravity_file_rather_than_panicking() {
        let bad_path = CString::new("/nonexistent/path/does/not/exist.cof").unwrap();
        let gmat_root = gmat_root_cstring();
        let mut handle: *mut AvOrbitalModel = std::ptr::null_mut();
        let status = unsafe { av_orbital_model_new(bad_path.as_ptr(), 0, 0, gmat_root.as_ptr(), &mut handle) };
        assert_eq!(status, AvOrbitalStatus::ErrModelConstructionFailed);
        assert!(handle.is_null());
    }
}
