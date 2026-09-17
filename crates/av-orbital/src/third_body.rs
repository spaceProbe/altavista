//! Third-body point-mass perturbation acceleration (`docs/native-dynamics-plan.md`
//! milestone N2, step 4), in the numerically robust "Battin" form rather than a direct
//! difference of two nearly-equal large vectors.
//!
//! # Why not the naive difference
//!
//! The textbook third-body perturbation on a spacecraft at position `r` (relative to the
//! central body), from a third body at `d` (also relative to the central body), is
//!
//! ```text
//! a = -mu * [ (r - d) / |r - d|^3 + d / |d|^3 ]
//! ```
//!
//! (the direct attraction toward the third body, minus the central body's own acceleration
//! toward it, since the whole computation is done in the central body's own, non-inertial,
//! translating frame). For the Moon (`|d| ~ 384,400` km) or the Sun (`|d| ~ 1.5e8` km) acting
//! on a LEO spacecraft (`|r| ~ 6,900` km), `|d|` and `|r - d|` differ by a few parts in
//! `1e4`-`1e2` of `|d|` itself -- computing `r - d` directly then does not lose catastrophic
//! precision, but it loses enough (`f64` carries ~15-16 significant digits; a mm-level
//! tolerance at Sun distance needs ~14-15 of them) to matter at exactly the tolerance this
//! crate's goldens are pinned to. Measured in this crate's N2 report: the naive and Battin
//! forms agree to ~1e-12 to 1e-14 relative at these distances (verified with a Python/NumPy
//! script before this Rust code was written), which is the entire point -- the Battin form is
//! not a different physical model, only a numerically stable way to evaluate the same one.
//!
//! # The Battin form, derived and verified (not quoted from memory)
//!
//! Let `q = (r.r - 2 r.d) / d.d` (so `|r - d|^2 = |d|^2 (1 + q)`). Then
//!
//! ```text
//! F(q) = 1 - (1+q)^(-3/2)              -- exact, but loses precision for small q if evaluated directly
//!      = q (2 + q + sqrt(1+q)) / [ (1+q)^1.5 (1 + sqrt(1+q)) ]     -- algebraically identical, stable
//!
//! a = -mu / |d|^3 * [ r / (1+q)^1.5 + F(q) d ]
//! ```
//!
//! **The `F(q)` identity was re-derived here, not copied from a reference.** A widely
//! circulated closed form, `q(3+3q+q^2)/(1+(1+q)^1.5)`, disagrees numerically with
//! `1 - (1+q)^-1.5` by ~1% at `q ~ 0.01` (this crate's N2 report has the numeric check) --
//! but the two are not meant to be equal in the first place, so this is not evidence either
//! one is wrong. Let `u = (1+q)^1.5`. Expanding `(1+q)^3 - 1 = q(3+3q+q^2)` and factoring the
//! difference of squares `u^2 - 1 = (u-1)(u+1)` gives, exactly:
//!
//! ```text
//! q(3+3q+q^2) / (1 + u) = (u-1)(u+1) / (1+u) = u - 1 = (1+q)^1.5 - 1
//! 1 - (1+q)^-1.5                              = (u-1) / u
//! ```
//!
//! The widely circulated form computes `u - 1`; `F(q)` here is `(u-1)/u`. They differ by
//! exactly the factor `u = (1+q)^1.5` -- a genuinely different quantity, not a wrong one:
//! which of the two belongs in a given third-body formula depends entirely on how that
//! formula distributes the `(1+q)^1.5` factor between `F(q)` and the rest of the bracket (a
//! formula that already divides its `r` term by a bare `1` instead of `(1+q)^1.5` would want
//! `u - 1` there instead of this module's `F(q)`). This module's own bracket, `r / (1+q)^1.5 +
//! F(q) d`, needs `F(q) = 1 - (1+q)^-1.5` exactly, so the widely circulated form is not usable
//! here unmodified -- hence the re-derivation below, not a correction of "wrong" math
//! elsewhere. The form below was derived by factoring `x^3 - 1 = (x-1)(x^2+x+1)` with
//! `x = sqrt(1+q)`, and verified to agree with the direct `1-(1+q)^-1.5` to machine precision
//! from `q=1e-4` up (and to the *expected*, precision-limited direct value below that -- see
//! this crate's N2 report for the printed sweep) before this Rust code was written.
//!
//! `d3()`, below, is `|d|^3`, and the whole bracket is scaled by `-mu/|d|^3` as shown.

/// The third-body perturbation acceleration on a spacecraft at `r` (m, relative to the
/// central body) from a point mass of standard gravitational parameter `mu_third` (m^3/s^2)
/// at `d` (m, relative to the same central body) -- see this module's doc for the formula and
/// why it is written this way. All vectors in the same (inertial) frame; no rotation happens
/// here.
pub fn third_body_acceleration(r: [f64; 3], d: [f64; 3], mu_third: f64) -> [f64; 3] {
    let dot = |a: [f64; 3], b: [f64; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    let d2 = dot(d, d);
    let q = (dot(r, r) - 2.0 * dot(r, d)) / d2;
    let x = (1.0 + q).sqrt();
    let f_q = q * (2.0 + q + x) / ((1.0 + q).powf(1.5) * (1.0 + x));
    let d3 = d2 * d2.sqrt();
    let one_plus_q_15 = (1.0 + q).powf(1.5);
    let scale = -mu_third / d3;
    [
        scale * (r[0] / one_plus_q_15 + f_q * d[0]),
        scale * (r[1] / one_plus_q_15 + f_q * d[1]),
        scale * (r[2] / one_plus_q_15 + f_q * d[2]),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn naive(r: [f64; 3], d: [f64; 3], mu_third: f64) -> [f64; 3] {
        let diff = [r[0] - d[0], r[1] - d[1], r[2] - d[2]];
        let diff_norm3 = {
            let n2 = diff[0] * diff[0] + diff[1] * diff[1] + diff[2] * diff[2];
            n2 * n2.sqrt()
        };
        let d_norm3 = {
            let n2 = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
            n2 * n2.sqrt()
        };
        [
            -mu_third * (diff[0] / diff_norm3 + d[0] / d_norm3),
            -mu_third * (diff[1] / diff_norm3 + d[1] / d_norm3),
            -mu_third * (diff[2] / diff_norm3 + d[2] / d_norm3),
        ]
    }

    /// The Battin form must agree with the naive direct-difference form (both are the same
    /// physical formula; see this module's doc) at LEO-vs-Moon distance scales, to several
    /// orders of magnitude tighter than either form's own floating-point noise floor.
    #[test]
    fn battin_form_agrees_with_naive_form_at_moon_distance() {
        let r = [6_878_000.0, 0.0, 0.0]; // m, ~LEO
        let d = [200_000_000.0, 300_000_000.0, 50_000_000.0]; // m, ~Moon-scale
        let mu = 4.9028e12; // m^3/s^2, Moon-scale
        let a_battin = third_body_acceleration(r, d, mu);
        let a_naive = naive(r, d, mu);
        let diff = (0..3).map(|i| (a_battin[i] - a_naive[i]).powi(2)).sum::<f64>().sqrt();
        let scale = (0..3).map(|i| a_naive[i].powi(2)).sum::<f64>().sqrt();
        println!("n2-battin-vs-naive-moon: rel diff = {:e}", diff / scale);
        assert!(diff / scale < 1e-9, "Battin and naive third-body forms disagree by {:e} relative", diff / scale);
    }

    /// Same check at Sun distance scale (~1.5e11 m) -- the regime the naive form loses the
    /// most precision in, and where the Battin form is most needed.
    #[test]
    fn battin_form_agrees_with_naive_form_at_sun_distance() {
        let r = [6_878_000.0, 1_200_000.0, -300_000.0]; // m, ~LEO
        let d = [1.4e11, 3.0e10, 1.0e10]; // m, ~Sun-scale
        let mu = 1.32712440018e20; // m^3/s^2, Sun-scale
        let a_battin = third_body_acceleration(r, d, mu);
        let a_naive = naive(r, d, mu);
        let diff = (0..3).map(|i| (a_battin[i] - a_naive[i]).powi(2)).sum::<f64>().sqrt();
        let scale = (0..3).map(|i| a_naive[i].powi(2)).sum::<f64>().sqrt();
        println!("n2-battin-vs-naive-sun: rel diff = {:e}", diff / scale);
        assert!(diff / scale < 1e-9, "Battin and naive third-body forms disagree by {:e} relative", diff / scale);
    }

    /// A zero-mass third body contributes nothing (a trivial but real check on the formula's
    /// own scaling).
    #[test]
    fn zero_mu_gives_zero_acceleration() {
        let a = third_body_acceleration([7e6, 0.0, 0.0], [1e11, 0.0, 0.0], 0.0);
        assert_eq!(a, [0.0, 0.0, 0.0]);
    }
}
