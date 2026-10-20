//! The one place a transcendental function on the native run path is evaluated
//! (`docs/open-questions.md` question 258).
//!
//! Question 248 measured Apple's libm and glibc disagreeing by one ulp on a few percent of inputs
//! to `sin`, `cos`, `asin`, `acos`, `atan2`, `hypot`, `exp` and `powf`, and traced those ulps
//! through `av_orbital::fk5`'s nutation series into the integrated state, so a native run's bytes
//! differed between macOS and Linux. Every such call on the native path (`av-orbital`,
//! `av-dynamics`, `av-kernel`, outside `#[cfg(test)]`; GMAT-backed code keeps GMAT's own libm and
//! is outside this) goes through the functions below, which evaluate the pure-Rust `libm` crate:
//! the same Rust source on every target, so the same bits on every platform.
//!
//! **The guard.** `clippy.toml` in `crates/av-dynamics`, `crates/av-orbital` and `crates/av-kernel`
//! disallows the platform's `f64` and `f32` transcendental methods. The lint is allowed
//! package-wide (each `Cargo.toml`'s `[lints.clippy]`), so test code, integration tests and
//! examples keep `std` as their reference. It is denied outside `cfg(test)` on the native path:
//! - in `av-dynamics` and `av-orbital`, by the crate-root attribute;
//! - in `av-kernel`, by the attributes on `drm`, `rng` and `interpolate`.
//!
//! A platform call there fails `cargo clippy --workspace --all-targets -- -D warnings`. This module
//! needs no exemption, because it calls `libm`, never an `f64` method.
//!
//! What stays on `std`, and why that is not a platform dependence:
//!
//! - `sqrt`: IEEE 754 requires it correctly rounded, and every target this repository builds for
//!   (aarch64, x86_64) evaluates it with one hardware instruction. Question 248 measured it equal.
//! - `powi`: not a libm function. It is LLVM's `powi` intrinsic, lowered to multiplications or to
//!   `__powidf2` from Rust's own compiler-builtins, the same algorithm on every target. Question
//!   248 measured it equal.
//! - `+ - * /`, `abs`, `floor`, `ceil`, `round`, `trunc`, `min`, `max`, `clamp`, `rem_euclid`,
//!   `to_radians`, `to_degrees`: IEEE 754 basic operations or exact operations, identical
//!   everywhere. Rust never contracts `a * b + c` into a fused multiply-add on its own.
//!
//! The `libm` crate's only target-specific code is `sqrt`, `fma` and `rint` through a hardware
//! instruction where one exists; all three are exact or correctly rounded, so they cannot differ.

/// `sin(x)`.
#[inline]
pub fn sin(x: f64) -> f64 {
    libm::sin(x)
}

/// `cos(x)`.
#[inline]
pub fn cos(x: f64) -> f64 {
    libm::cos(x)
}

/// `(sin(x), cos(x))`, as `f64::sin_cos` returns them.
#[inline]
pub fn sin_cos(x: f64) -> (f64, f64) {
    libm::sincos(x)
}

/// `asin(x)`.
#[inline]
pub fn asin(x: f64) -> f64 {
    libm::asin(x)
}

/// `acos(x)`.
#[inline]
pub fn acos(x: f64) -> f64 {
    libm::acos(x)
}

/// `atan(x)`.
#[inline]
pub fn atan(x: f64) -> f64 {
    libm::atan(x)
}

/// `atan2(y, x)`, argument order as `y.atan2(x)`.
#[inline]
pub fn atan2(y: f64, x: f64) -> f64 {
    libm::atan2(y, x)
}

/// `e^x`.
#[inline]
pub fn exp(x: f64) -> f64 {
    libm::exp(x)
}

/// Natural logarithm, as `f64::ln`.
#[inline]
pub fn ln(x: f64) -> f64 {
    libm::log(x)
}

/// Base-10 logarithm.
#[inline]
pub fn log10(x: f64) -> f64 {
    libm::log10(x)
}

/// `x^y` for a real exponent, as `f64::powf`.
#[inline]
pub fn powf(x: f64, y: f64) -> f64 {
    libm::pow(x, y)
}

/// `sqrt(x^2 + y^2)` without undue overflow, as `x.hypot(y)`.
#[inline]
pub fn hypot(x: f64, y: f64) -> f64 {
    libm::hypot(x, y)
}
