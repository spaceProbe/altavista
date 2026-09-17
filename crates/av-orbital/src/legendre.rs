//! Fully-normalised associated Legendre functions `P̄_nm(sin φ)`, stable to degree 70 and
//! well beyond.
//!
//! # Formulation
//!
//! The **standard normalised forward-column recursion** (the base recursion common to
//! Lundberg & Schutz 1988 and to Holmes & Featherstone 2002 -- *not* Holmes & Featherstone's
//! extended-range rescaling, which exists to push stability past degree ~1800 and is more
//! machinery than a degree-70 field needs). Working directly in `u = sin(latitude)`:
//!
//! ```text
//! P̄_00(u) = 1
//! P̄_11(u) = sqrt(3) * cos(φ) * P̄_00(u)                                                  (first sectorial step;
//!                                                                                        see NormalizedLegendre::new's
//!                                                                                        comment for why m=1 is special)
//! P̄_mm(u) = sqrt((2m+1)/(2m)) * cos(φ) * P̄_{m-1,m-1}(u),                       m >= 2   (sectorial)
//! P̄_{m+1,m}(u) = sqrt(2m+3) * u * P̄_mm(u)                                               (sub-diagonal)
//! P̄_nm(u) = a_nm * u * P̄_{n-1,m}(u) - b_nm * P̄_{n-2,m}(u),                     n >= m+2  (column)
//!   a_nm = sqrt( (2n-1)(2n+1) / ((n-m)(n+m)) )
//!   b_nm = sqrt( (2n+1)(n+m-1)(n-m-1) / ((2n-3)(n-m)(n+m)) )
//! ```
//!
//! This is chosen over the equivalent Cunningham/Gottlieb complex-coefficient form because
//! it is the more direct way to get a standalone, independently-testable `P̄_nm(sin φ)`
//! function pinned against the closed forms this module's tests derive; `gravity.rs` uses an
//! unrelated, self-contained Cartesian recursion for the force model itself (see its module
//! doc for why that -- not this -- is where the pole-singularity property actually has to
//! hold for an *acceleration*).
//!
//! Crucially, **every quantity computed here is already normalised** -- unlike computing the
//! classical unnormalised `P_nm` first and dividing by a normalisation factor afterward, which
//! overflows for `m` around 150 in `f64` (the sectorial term grows like `(2m-1)!!`, and
//! `(2*150-1)!! ~ 1e283`, close enough to `f64::MAX ~ 1.8e308` that a few more degrees would
//! overflow, and by degree 70 it has already lost most of its usable precision to catastrophic
//! cancellation against the equally huge normalisation factor). The sectorial recursion above
//! never forms that unnormalised value at all: `sqrt((2m+1)/(2m))` is `O(1)` at every step, so
//! the normalised `P̄_mm` stays `O(1)` for any `m`, bounded by `cos(φ)^m <= 1`.
//!
//! # Sign convention
//!
//! No Condon-Shortley phase (the geodesy/Ferrers convention GMAT's `.cof` coefficients are
//! normalised against, matching `cof.rs`'s `POTFIELD` reading of "fully normalised").
//!
//! # `NormalizedLegendre`
//!
//! [`NormalizedLegendre::new`] computes the full triangular table for a given `u = sin
//! (latitude)` up to a requested maximum degree in one pass; [`NormalizedLegendre::get`]
//! reads back `P̄_nm(u)`.

use thiserror::Error;

/// `u` was outside the domain of `sin(latitude)`, `[-1, 1]` (with a small tolerance for
/// floating-point roundoff at exactly the poles).
#[derive(Debug, Error, PartialEq)]
#[error("u = {0} is outside [-1, 1] (u must be sin(latitude))")]
pub struct DomainError(pub f64);

/// The full triangular table of fully-normalised associated Legendre functions `P̄_nm(u)` for
/// `0 <= m <= n <= max_degree`, evaluated at one `u = sin(latitude)`.
#[derive(Debug, Clone)]
pub struct NormalizedLegendre {
    max_degree: usize,
    // Packed triangular: index(n, m) = n*(n+1)/2 + m.
    values: Vec<f64>,
}

fn tri_index(n: usize, m: usize) -> usize {
    n * (n + 1) / 2 + m
}

impl NormalizedLegendre {
    /// Computes `P̄_nm(u)` for every `0 <= m <= n <= max_degree`. Returns
    /// [`DomainError`] if `u` is not in `[-1, 1]` (beyond a `1e-12` tolerance for roundoff at
    /// exactly the poles, since callers typically compute `u = z / r` and may land at
    /// `u = 1.0000000000000002` by floating-point error).
    pub fn new(u: f64, max_degree: usize) -> Result<Self, DomainError> {
        if !u.is_finite() || !(-1.0 - 1e-12..=1.0 + 1e-12).contains(&u) {
            return Err(DomainError(u));
        }
        let u = u.clamp(-1.0, 1.0);
        let cos_phi = (1.0 - u * u).sqrt();

        let len = tri_index(max_degree, max_degree) + 1;
        let mut values = vec![0.0_f64; len];
        values[tri_index(0, 0)] = 1.0;

        for m in 1..=max_degree {
            let prev = values[tri_index(m - 1, m - 1)];
            let mut sectorial = ((2 * m + 1) as f64 / (2 * m) as f64).sqrt() * cos_phi * prev;
            // The general ratio above assumes both P̄_mm and P̄_{m-1,m-1} carry the same
            // (2 - delta_{m,0}) normalisation factor, which holds for m >= 2 (both orders
            // non-zero) but not for the very first step m=0 -> m=1: P̄_00's normalisation
            // uses delta_{0,0}=1 (factor 1) while P̄_11's uses delta_{1,0}=0 (factor 2), an
            // extra factor of sqrt(2) the general ratio does not account for. Concretely,
            // P̄_11(x) = sqrt(3) cos(phi) (N_11 = sqrt(3*2*0!/2!) = sqrt(3)), not
            // sqrt(3/2) cos(phi); caught by `equator_values_match_independent_closed_form`
            // (measured n=1,m=1 error 5.07e-1 before this fix -- see the N1 report).
            if m == 1 {
                sectorial *= 2.0_f64.sqrt();
            }
            values[tri_index(m, m)] = sectorial;
        }
        for m in 0..max_degree {
            let sub = (2 * m + 3) as f64;
            let pmm = values[tri_index(m, m)];
            values[tri_index(m + 1, m)] = sub.sqrt() * u * pmm;
        }
        for m in 0..=max_degree {
            let mut n = m + 2;
            while n <= max_degree {
                let a = (((2 * n - 1) * (2 * n + 1)) as f64 / ((n - m) * (n + m)) as f64).sqrt();
                let b = (((2 * n + 1) * (n + m - 1) * (n - m - 1)) as f64
                    / ((2 * n - 3) * (n - m) * (n + m)) as f64)
                    .sqrt();
                let p1 = values[tri_index(n - 1, m)];
                let p2 = values[tri_index(n - 2, m)];
                values[tri_index(n, m)] = a * u * p1 - b * p2;
                n += 1;
            }
        }

        Ok(Self { max_degree, values })
    }

    /// `P̄_nm(u)`. Returns `0.0` for `m > n` (mathematically zero) rather than panicking;
    /// panics only if `n` exceeds the degree this table was built for (a caller bug, not
    /// malformed input -- there is no file or external data behind this type).
    pub fn get(&self, n: usize, m: usize) -> f64 {
        if m > n {
            return 0.0;
        }
        assert!(
            n <= self.max_degree,
            "P̄_{n}{m} requested but table was built to degree {}",
            self.max_degree
        );
        self.values[tri_index(n, m)]
    }

    /// The maximum degree this table covers.
    pub fn max_degree(&self) -> usize {
        self.max_degree
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Independent reference for the fully-normalised `P̄_nm(0)` (equator, `u = sin(latitude)
    /// = 0`), derived and coded here from first principles -- **not** taken from this
    /// module's own recursion, and not a literal hardcoded from memory.
    ///
    /// Source of each step (standard, citable identities for the *unnormalised*, Ferrers-
    /// convention associated Legendre functions `P_n^m`; e.g. Abramowitz & Stegun 8.5.3 for
    /// the three-term recursion, and the sectorial closed form is the standard Rodrigues-
    /// formula corollary `P_m^m(x) = (2m-1)!! (1-x^2)^{m/2}` in this no-Condon-Shortley
    /// convention):
    ///
    /// 1. Sectorial seed at `x=0`: `P_m^m(0) = (2m-1)!!` (since `(1-0^2)^{m/2} = 1`).
    /// 2. Sub-diagonal seed: `P_{m+1}^m(x) = (2m+1) x P_m^m(x)`, so `P_{m+1}^m(0) = 0`.
    /// 3. The standard fixed-`m` three-term recursion in `n` (Abramowitz & Stegun 8.5.3 /
    ///    NIST DLMF 14.10.3, Ferrers convention):
    ///    `(n-m+1) P_{n+1}^m(x) = (2n+1) x P_n^m(x) - (n+m) P_{n-1}^m(x)`.
    ///    At `x=0` the first term on the right vanishes, giving
    ///    `P_{n+1}^m(0) = -(n+m)/(n-m+1) * P_{n-1}^m(0)`, which (given `P_{m+1}^m(0)=0` from
    ///    step 2) makes every `P_n^m(0)` with `n-m` odd exactly zero, and builds every `n-m`
    ///    even value from the sectorial seed alone -- a completely different code path from
    ///    this module's own `u`-parameterised recursion.
    /// 4. Converts to fully-normalised via `P̄_nm = N_nm * P_nm`,
    ///    `N_nm = sqrt((2n+1)(2 - δ_m0)(n-m)!/(n+m)!)` (the same normalisation `cof.rs`'s J2
    ///    cross-check uses for `m=0`), computed as an iterative product/ratio so no
    ///    intermediate factorial is ever formed.
    fn reference_p_bar_at_equator(n: usize, m: usize) -> f64 {
        if m > n {
            return 0.0;
        }
        if !(n - m).is_multiple_of(2) {
            return 0.0;
        }
        // Unnormalised P_n^m(0) via the fixed-m recursion above, starting from the sectorial
        // seed P_m^m(0) = (2m-1)!! and P_{m+1}^m(0) = 0.
        let sectorial_double_fact = {
            // (2m-1)!! computed iteratively (never forming (2m)! or (2m-1)! directly).
            let mut r = 1.0_f64;
            let mut k = 1usize;
            while k < m {
                r *= (2 * k + 1) as f64;
                k += 1;
            }
            r
        };
        let mut p_prev2 = sectorial_double_fact; // P_m^m(0)
        let mut p_prev1 = 0.0_f64; // P_{m+1}^m(0)
        if n == m {
            return normalize_at(m, m, p_prev2);
        }
        if n == m + 1 {
            return normalize_at(m + 1, m, p_prev1);
        }
        let mut k = m + 1; // p_prev1 = P_k^m(0), p_prev2 = P_{k-1}^m(0)
        while k < n {
            let next = -((k + m) as f64) / ((k - m + 1) as f64) * p_prev2;
            p_prev2 = p_prev1;
            p_prev1 = next;
            k += 1;
        }
        normalize_at(n, m, p_prev1)
    }

    fn normalization_factor(n: usize, m: usize) -> f64 {
        // sqrt((2n+1)(2-delta_m0)(n-m)!/(n+m)!) via an iterative ratio.
        let mut ratio = 1.0_f64;
        let mut k = n - m + 1;
        while k <= n + m {
            ratio /= k as f64;
            k += 1;
        }
        let delta_factor = if m == 0 { 1.0 } else { 2.0 };
        ((2 * n + 1) as f64 * delta_factor * ratio).sqrt()
    }

    fn normalize_at(n: usize, m: usize, unnormalized: f64) -> f64 {
        normalization_factor(n, m) * unnormalized
    }

    #[test]
    fn pole_values_match_closed_form() {
        // Closed form (Heiskanen & Moritz, Physical Geodesy, and the standard identity
        // P_n(1) = 1 for the ordinary Legendre polynomial together with N_n0 = sqrt(2n+1)):
        // P̄_n0(pole) = sqrt(2n+1); P̄_nm(pole) = 0 for m >= 1, since the unnormalised
        // associated P_nm(x) has a factor (1-x^2)^{m/2} which vanishes at x = +/-1 for any
        // m >= 1.
        let degree = 20;
        let mut max_m0_err = 0.0_f64;
        let mut max_mgt0_abs = 0.0_f64;
        for &u in &[1.0, -1.0] {
            let table = NormalizedLegendre::new(u, degree).unwrap();
            for n in 0..=degree {
                let expected0 = (2 * n + 1) as f64;
                let got0 = table.get(n, 0);
                let err0 = (got0 * got0 - expected0).abs();
                max_m0_err = max_m0_err.max(err0);
                for m in 1..=n {
                    let got = table.get(n, m);
                    max_mgt0_abs = max_mgt0_abs.max(got.abs());
                }
            }
        }
        println!(
            "n1a-legendre-pole: max_abs_err_m0_squared={max_m0_err:e} max_abs_value_m_ge_1={max_mgt0_abs:e}"
        );
        assert!(max_m0_err < 1e-10, "{max_m0_err:e}");
        assert!(max_mgt0_abs < 1e-10, "{max_mgt0_abs:e}");
    }

    #[test]
    fn equator_values_match_independent_closed_form() {
        let degree = 12;
        let table = NormalizedLegendre::new(0.0, degree).unwrap();
        let mut max_abs_err = 0.0_f64;
        for n in 0..=degree {
            for m in 0..=n {
                let want = reference_p_bar_at_equator(n, m);
                let got = table.get(n, m);
                let err = (got - want).abs();
                max_abs_err = max_abs_err.max(err);
                assert!(
                    err < 1e-11,
                    "n={n} m={m}: recursion={got} reference={want} err={err:e}"
                );
            }
        }
        println!("n1a-legendre-equator: max_abs_err={max_abs_err:e}");
    }

    /// Stability at degree 70: every value is finite, and the sum-of-squares-over-`m`
    /// identity holds. Derivation (the unnormalised spherical-harmonic addition theorem,
    /// e.g. Heiskanen & Moritz eq. 1-71, evaluated at zero angular separation so
    /// `P_n(cos 0) = P_n(1) = 1`):
    /// `1 = sum_{m=0}^{n} (2 - delta_m0) [(n-m)!/(n+m)!] [P_nm(cos theta)]^2`
    /// (unnormalised `P_nm`). Substituting the normalisation `P̄_nm = N_nm P_nm`,
    /// `N_nm^2 = (2n+1)(2-delta_m0)(n-m)!/(n+m)!` -- i.e. `(n-m)!/(n+m)! = N_nm^2 /
    /// ((2n+1)(2-delta_m0))` -- the `(2-delta_m0)` factors cancel exactly, leaving
    /// `sum_{m=0}^{n} P̄_nm(u)^2 = 2n + 1` for every `u`: **no** extra `(2-delta_m0)` weight
    /// on the *normalised* sum (an earlier version of this test carried that weight over
    /// from the unnormalised identity by mistake, and measured almost exactly double the
    /// expected value at every degree -- e.g. degree 70: sum 281.988 against an asserted
    /// 141, and `281.988 / 2 = 140.99`, confirming the fix rather than the recursion was
    /// wrong; full evidence in this crate's N1 report). Independent of latitude, and
    /// consistent with (in fact forced by, at `u=1`) the pole closed form above, since at the
    /// pole only the `m=0` term survives and it alone must equal `2n+1`.
    #[test]
    fn degree_70_is_stable_and_satisfies_addition_theorem() {
        let degree = 70;
        // A generic, non-special latitude (not pole, not equator) so the check is not
        // accidentally trivial.
        let u = 0.371_f64;
        let table = NormalizedLegendre::new(u, degree).unwrap();
        let mut max_identity_err = 0.0_f64;
        for n in 0..=degree {
            let mut sum = 0.0_f64;
            for m in 0..=n {
                let p = table.get(n, m);
                assert!(p.is_finite(), "P̄_{n}{m}({u}) is not finite: {p}");
                sum += p * p;
            }
            let expected = (2 * n + 1) as f64;
            let err = (sum - expected).abs() / expected;
            max_identity_err = max_identity_err.max(err);
        }
        println!("n1a-legendre-degree70: max_relative_addition_theorem_err={max_identity_err:e}");
        assert!(
            max_identity_err < 1e-12,
            "addition-theorem identity violated at degree 70: {max_identity_err:e}"
        );
    }

    /// A second, independent consistency check at a different latitude, specifically to
    /// catch a wrong normalisation constant (e.g. an off-by-`sqrt(2)` on the `m=0` handling,
    /// or a wrong `a_nm`/`b_nm` coefficient) that the pole/equator closed forms above might
    /// not stress: repeats the same addition-theorem identity at three more latitudes,
    /// including one very close to (but not exactly at) a pole, where a wrong normalisation
    /// tends to show up as a growing, latitude-dependent error rather than a constant one.
    #[test]
    fn addition_theorem_holds_across_latitudes() {
        let degree = 70;
        let mut max_identity_err = 0.0_f64;
        for &u in &[-0.9999, -0.5, 0.1234, 0.987] {
            let table = NormalizedLegendre::new(u, degree).unwrap();
            for n in 0..=degree {
                let mut sum = 0.0_f64;
                for m in 0..=n {
                    let p = table.get(n, m);
                    sum += p * p; // see degree_70_is_stable_and_satisfies_addition_theorem's
                                  // doc for the derivation showing no (2-delta_m0) weight
                                  // belongs on the *normalised* sum
                }
                let expected = (2 * n + 1) as f64;
                let err = (sum - expected).abs() / expected;
                max_identity_err = max_identity_err.max(err);
            }
        }
        println!("n1a-legendre-multilat: max_relative_addition_theorem_err={max_identity_err:e}");
        assert!(max_identity_err < 1e-10, "{max_identity_err:e}");
    }

    #[test]
    fn domain_error_on_out_of_range_u() {
        assert!(NormalizedLegendre::new(1.5, 5).is_err());
        assert!(NormalizedLegendre::new(-1.5, 5).is_err());
        assert!(NormalizedLegendre::new(f64::NAN, 5).is_err());
    }

    #[test]
    fn p00_is_one() {
        let table = NormalizedLegendre::new(0.42, 3).unwrap();
        assert_eq!(table.get(0, 0), 1.0);
    }
}
