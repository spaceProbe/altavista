//! A minimal forward-mode dual number carrying a value and its gradient with respect to three
//! independent variables, and the [`GravScalar`] trait that lets `gravity.rs`'s
//! spherical-harmonic recursion run unchanged over either plain `f64` (the fast, value-only
//! path) or [`Dual3`] (the gradient path).
//!
//! `gravity.rs`'s acceleration and its 3x3 partial-derivative matrix are computed by the
//! *same* body-fixed Cartesian recursion, run twice: once with `T = f64` for the value, and
//! once with `T = Dual3` seeded so that `x`, `y`, `z` each carry a unit tangent in their own
//! direction. The output's tangent vectors are then exactly `d(acceleration)/d(x,y,z)` -- the
//! gravity-gradient matrix -- to floating-point precision, with **no finite-difference step
//! size and no truncation error**, and (this is the point) with no separate, hand-derived
//! second-order recursion to get subtly wrong: the partials are mechanically the derivative
//! of whatever the acceleration function actually computes, so the two can never silently
//! disagree with each other, only (measurably, in the test suite) with the truth.
//!
//! This is forward-mode automatic differentiation, a standard, exact (not approximate)
//! technique -- not a new numerical scheme invented for this crate. It is validated the same
//! way any AD implementation is: against a central finite difference of the value-only path,
//! in `gravity.rs`'s own tests.

use std::ops::{Add, Div, Mul, Neg, Sub};

/// The arithmetic `gravity.rs`'s recursion needs, implemented for both `f64` (the value-only
/// path) and [`Dual3`] (the gradient path).
pub trait GravScalar:
    Copy + Add<Output = Self> + Sub<Output = Self> + Mul<Output = Self> + Div<Output = Self> + Neg<Output = Self>
{
    /// Lifts a plain `f64` constant (a coefficient, `mu`, a reference radius -- never a
    /// quantity being differentiated) into `Self`.
    fn constant(v: f64) -> Self;
    /// The plain `f64` value, discarding any derivative information.
    fn value(self) -> f64;
    /// Square root, differentiated by the chain rule when `Self` carries derivatives.
    fn sqrt(self) -> Self;
}

impl GravScalar for f64 {
    fn constant(v: f64) -> Self {
        v
    }
    fn value(self) -> f64 {
        self
    }
    fn sqrt(self) -> Self {
        f64::sqrt(self)
    }
}

/// A value paired with its partial derivatives with respect to three independent variables
/// (conventionally body-fixed `x`, `y`, `z`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Dual3 {
    /// The function value.
    pub v: f64,
    /// `[d/dx, d/dy, d/dz]`.
    pub d: [f64; 3],
}

impl Dual3 {
    /// An independent variable: value `v`, with a unit tangent along `axis` (`0..3`).
    pub fn variable(v: f64, axis: usize) -> Self {
        let mut d = [0.0; 3];
        d[axis] = 1.0;
        Self { v, d }
    }
}

impl GravScalar for Dual3 {
    fn constant(v: f64) -> Self {
        Self { v, d: [0.0; 3] }
    }
    fn value(self) -> f64 {
        self.v
    }
    fn sqrt(self) -> Self {
        let v = self.v.sqrt();
        let dv_dself = 0.5 / v;
        Self {
            v,
            d: [self.d[0] * dv_dself, self.d[1] * dv_dself, self.d[2] * dv_dself],
        }
    }
}

impl Add for Dual3 {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        Self {
            v: self.v + rhs.v,
            d: [self.d[0] + rhs.d[0], self.d[1] + rhs.d[1], self.d[2] + rhs.d[2]],
        }
    }
}

impl Sub for Dual3 {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        Self {
            v: self.v - rhs.v,
            d: [self.d[0] - rhs.d[0], self.d[1] - rhs.d[1], self.d[2] - rhs.d[2]],
        }
    }
}

impl Mul for Dual3 {
    type Output = Self;
    fn mul(self, rhs: Self) -> Self {
        Self {
            v: self.v * rhs.v,
            d: [
                self.d[0] * rhs.v + self.v * rhs.d[0],
                self.d[1] * rhs.v + self.v * rhs.d[1],
                self.d[2] * rhs.v + self.v * rhs.d[2],
            ],
        }
    }
}

impl Div for Dual3 {
    type Output = Self;
    fn div(self, rhs: Self) -> Self {
        let inv_rhs_v = 1.0 / rhs.v;
        let v = self.v * inv_rhs_v;
        // (a/b)' = (a'b - ab') / b^2 = (a' - v*b') / b
        Self {
            v,
            d: [
                (self.d[0] - v * rhs.d[0]) * inv_rhs_v,
                (self.d[1] - v * rhs.d[1]) * inv_rhs_v,
                (self.d[2] - v * rhs.d[2]) * inv_rhs_v,
            ],
        }
    }
}

impl Neg for Dual3 {
    type Output = Self;
    fn neg(self) -> Self {
        Self {
            v: -self.v,
            d: [-self.d[0], -self.d[1], -self.d[2]],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn product_rule_matches_hand_derivative() {
        // f(x,y,z) = x*y, at (2,3,_): df/dx=y=3, df/dy=x=2, df/dz=0.
        let x = Dual3::variable(2.0, 0);
        let y = Dual3::variable(3.0, 1);
        let f = x * y;
        assert_eq!(f.v, 6.0);
        assert_eq!(f.d, [3.0, 2.0, 0.0]);
    }

    #[test]
    fn quotient_rule_matches_hand_derivative() {
        // f(x,y) = x/y at (6,3): v=2, df/dx=1/y=1/3, df/dy=-x/y^2=-6/9=-2/3.
        let x = Dual3::variable(6.0, 0);
        let y = Dual3::variable(3.0, 1);
        let f = x / y;
        assert!((f.v - 2.0).abs() < 1e-14);
        assert!((f.d[0] - 1.0 / 3.0).abs() < 1e-14);
        assert!((f.d[1] - (-2.0 / 3.0)).abs() < 1e-14);
    }

    #[test]
    fn sqrt_matches_hand_derivative() {
        // f(x) = sqrt(x) at x=4: v=2, df/dx = 1/(2*sqrt(x)) = 0.25.
        let x = Dual3::variable(4.0, 0);
        let f = x.sqrt();
        assert!((f.v - 2.0).abs() < 1e-14);
        assert!((f.d[0] - 0.25).abs() < 1e-14);
    }
}
