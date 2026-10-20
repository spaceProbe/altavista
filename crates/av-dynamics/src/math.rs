//! The one place a transcendental function on the native run path is evaluated
//! (`docs/open-questions.md` question 258).
//!
//! Question 248 measured Apple's libm and glibc disagreeing by one ulp on a few percent of inputs
//! to `sin`, `cos`, `asin`, `acos`, `atan2`, `hypot`, `exp` and `powf`, and traced those ulps
//! through `av_orbital::fk5`'s nutation series into the integrated state, so a native run's bytes
//! differ between macOS and Linux. Every such call on the native path (`av-orbital`,
//! `av-dynamics`, `av-kernel`, outside `#[cfg(test)]`; GMAT-backed code keeps GMAT's own libm) goes
//! through the functions below, so there is one place to audit and one place to change.
//!
//! **Backend: the platform libm, unchanged.** Routing the calls here is bit-neutral (question 258's
//! Phase 1 measured the 12-sample study's 14 hashes and every native golden residual identical
//! before and after the routing). Replacing these bodies with the pure-Rust `libm` crate, which
//! makes native runs bit-identical across platforms, waits on the lead's ruling on the Phase 1
//! measurements.
//!
//! What stays on `std` at the call sites, and why that is not a platform dependence:
//!
//! - `sqrt`: IEEE 754 requires it correctly rounded, and every target this repository builds for
//!   (aarch64, x86_64) evaluates it with one hardware instruction. Question 248 measured it equal.
//! - `powi`: not a libm function. It is LLVM's `powi` intrinsic, lowered to multiplications or to
//!   `__powidf2` from Rust's own compiler-builtins, the same algorithm on every target. Question
//!   248 measured it equal.
//! - `+ - * /`, `abs`, `floor`, `ceil`, `round`, `trunc`, `min`, `max`, `clamp`, `rem_euclid`,
//!   `to_radians`, `to_degrees`: IEEE 754 basic or exact operations, identical everywhere. Rust
//!   never contracts `a * b + c` into a fused multiply-add on its own.

/// `sin(x)`.
#[inline]
pub fn sin(x: f64) -> f64 {
    x.sin()
}

/// `cos(x)`.
#[inline]
pub fn cos(x: f64) -> f64 {
    x.cos()
}

/// `(sin(x), cos(x))`, as `f64::sin_cos` returns them.
#[inline]
pub fn sin_cos(x: f64) -> (f64, f64) {
    x.sin_cos()
}

/// `asin(x)`.
#[inline]
pub fn asin(x: f64) -> f64 {
    x.asin()
}

/// `acos(x)`.
#[inline]
pub fn acos(x: f64) -> f64 {
    x.acos()
}

/// `atan(x)`.
#[inline]
pub fn atan(x: f64) -> f64 {
    x.atan()
}

/// `atan2(y, x)`, argument order as `y.atan2(x)`.
#[inline]
pub fn atan2(y: f64, x: f64) -> f64 {
    y.atan2(x)
}

/// `e^x`.
#[inline]
pub fn exp(x: f64) -> f64 {
    x.exp()
}

/// Natural logarithm, as `f64::ln`.
#[inline]
pub fn ln(x: f64) -> f64 {
    x.ln()
}

/// Base-10 logarithm.
#[inline]
pub fn log10(x: f64) -> f64 {
    x.log10()
}

/// `x^y` for a real exponent, as `f64::powf`.
#[inline]
pub fn powf(x: f64, y: f64) -> f64 {
    x.powf(y)
}

/// `sqrt(x^2 + y^2)` without undue overflow, as `x.hypot(y)`.
#[inline]
pub fn hypot(x: f64, y: f64) -> f64 {
    x.hypot(y)
}
