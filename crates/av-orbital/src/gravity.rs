//! Point-mass and spherical-harmonic gravity acceleration in the body-fixed frame, with
//! their exact 3x3 partial-derivative (gravity-gradient) matrices.
//!
//! **No frame rotation happens in this module.** Every position in, and every acceleration
//! out, is body-fixed Cartesian. The next N1 worker rotates through the frame registry's
//! GMAT-validated convert shim on the way to and from the inertial frame `DynamicsModel`
//! needs.
//!
//! # Formulation
//!
//! The spherical-harmonic term uses the **Cunningham/Gottlieb** recursion (Montenbruck &
//! Gill, *Satellite Orbits*, sec. 3.2.4-3.2.5's `V_nm`/`W_nm` form), evaluated directly in
//! body-fixed Cartesian coordinates `(x, y, z)`. It is non-singular at the poles *by
//! construction*: nothing in this recursion ever forms a latitude, a longitude, or a
//! `1/cos(latitude)` term. Latitude and longitude are coordinate charts with a singularity at
//! the poles (longitude is undefined there); `V_nm`/`W_nm` are smooth polynomials in `x/r`,
//! `y/r`, `z/r`, well-defined and finite everywhere on the sphere including both poles, which
//! is what "non-singular" means for this formulation.
//!
//! `V_nm`, `W_nm` (reference radius `Re`, `rho2 = Re^2/r^2`, `rx = x*Re/r^2` etc.):
//!
//! ```text
//! V_00 = Re/r,  W_00 = 0
//! V_mm = (2m-1) (rx V_{m-1,m-1} - ry W_{m-1,m-1}),  W_mm analogous       (m >= 1, sectorial)
//! V_nm = [(2n-1) rz V_{n-1,m} - (n+m-1) rho2 V_{n-2,m}] / (n-m)          (n > m, column;
//! W_nm analogous                                                         the second term is
//!                                                                        dropped when n=m+1)
//! ```
//!
//! and the acceleration from **unnormalised** `C_nm`, `S_nm` (converted once from the
//! fully-normalised coefficients [`crate::cof::GravityModel`] loads, via `C_nm = C̄_nm *
//! N_nm`, `N_nm = sqrt((2n+1)(2-delta_m0)(n-m)!/(n+m)!)` -- the same normalisation factor
//! `cof.rs`'s J2 cross-check and `legendre.rs`'s equator reference both use, computed here by
//! the identical iterative-ratio method so no intermediate factorial is ever formed):
//!
//! ```text
//! m = 0:  ax += -C_n0 V_{n+1,1},  ay += -C_n0 W_{n+1,1}
//! m > 0:  ax += 0.5(-C_nm V_{n+1,m+1} - S_nm W_{n+1,m+1})
//!              + 0.5(n-m+2)(n-m+1)(C_nm V_{n+1,m-1} + S_nm W_{n+1,m-1})
//!         ay += 0.5(-C_nm W_{n+1,m+1} + S_nm V_{n+1,m+1})
//!              + 0.5(n-m+2)(n-m+1)(-C_nm W_{n+1,m-1} + S_nm V_{n+1,m-1})
//! all m:  az += (n-m+1)(-C_nm V_{n+1,m} - S_nm W_{n+1,m})
//! (ax, ay, az) *= mu / Re^2
//! ```
//!
//! Sanity-checked by hand before any test was run: the `n=0, m=0` term alone (`C_00=1`)
//! collapses exactly to `V_{1,1} = x Re^2/r^3`, `W_{1,1} = y Re^2/r^3`, `V_{1,0} = z Re^2/r^3`,
//! giving `(ax,ay,az) = -mu(x,y,z)/r^3` -- the textbook point-mass formula -- *before* the
//! degree-0 test in this module ever ran it; see `degree_zero_matches_point_mass` for the
//! measured (bit-for-bit) confirmation, and `matches_classical_potential_gradient_at_degree_2`
//! for an independent check of the `m >= 1` terms this hand check does not exercise (a
//! central finite difference of the classical latitude/longitude potential formula, built
//! from `legendre.rs`'s independently-tested `P̄_nm`, at a real degree-2 JGM2-like field).
//!
//! # Partials
//!
//! The point-mass partial is the closed-form `d/dr[-mu r/|r|^3] = -mu/|r|^3 I + 3 mu r r^T /
//! |r|^5` (standard, e.g. Vallado). The spherical-harmonic partial is **not** a second,
//! hand-derived recursion; it is the exact forward-mode automatic derivative of the
//! acceleration formula above (see `crate::dual`'s module doc for why: the classical
//! latitude/longitude second-derivative formula has its own, different pole-singularity
//! problem in the cross terms, and re-deriving Cunningham/Gottlieb's own published
//! second-derivative recursion from memory is exactly the kind of subtle-constant risk this
//! crate's rules ask to avoid in favour of a measured, tested tolerance). The same
//! `sh_acceleration` function runs once with `T = Dual3`, position seeded with unit tangents,
//! and the output tangents are the Jacobian -- verified against a central finite difference
//! of the plain `f64` path in this module's tests, with the measured agreement recorded in
//! this crate's N1 report.

use crate::cof::GravityModel;
use crate::dual::{Dual3, GravScalar};

fn tri_idx(n: usize, m: usize) -> usize {
    n * (n + 1) / 2 + m
}

/// `N_nm = sqrt((2n+1)(2-delta_m0)(n-m)!/(n+m)!)`, the standard fully-normalised <->
/// unnormalised conversion factor, computed as an iterative ratio so no intermediate
/// factorial (which would overflow for `n` anywhere near 70) is ever formed.
fn normalization_factor(n: usize, m: usize) -> f64 {
    let mut ratio = 1.0_f64;
    let mut k = n - m + 1;
    while k <= n + m {
        ratio /= k as f64;
        k += 1;
    }
    let delta_factor = if m == 0 { 1.0 } else { 2.0 };
    ((2 * n + 1) as f64 * delta_factor * ratio).sqrt()
}

/// Unnormalised `(C_nm, S_nm)` for every `0 <= m <= n <= model.max_degree()`, converted once
/// from `model`'s fully-normalised coefficients via `C_nm = C̄_nm * N_nm`.
struct UnnormalizedCoefficients {
    max_degree: usize,
    c: Vec<f64>,
    s: Vec<f64>,
}

impl UnnormalizedCoefficients {
    fn from_model(model: &GravityModel) -> Self {
        let max_degree = model.max_degree();
        let len = tri_idx(max_degree, max_degree) + 1;
        let mut c = vec![0.0_f64; len];
        let mut s = vec![0.0_f64; len];
        for n in 0..=max_degree {
            for m in 0..=n.min(model.max_order()) {
                let nrm = normalization_factor(n, m);
                c[tri_idx(n, m)] = model.c(n, m) * nrm;
                s[tri_idx(n, m)] = model.s(n, m) * nrm;
            }
        }
        Self { max_degree, c, s }
    }

    fn c(&self, n: usize, m: usize) -> f64 {
        if n == 0 && m == 0 {
            return 1.0;
        }
        if m > n || n > self.max_degree {
            return 0.0;
        }
        self.c[tri_idx(n, m)]
    }

    fn s(&self, n: usize, m: usize) -> f64 {
        if m > n || n > self.max_degree {
            return 0.0;
        }
        self.s[tri_idx(n, m)]
    }
}

/// Body-fixed `V_nm`, `W_nm` (Cunningham/Gottlieb), for `0 <= m <= n <= table_degree`.
fn compute_vw<T: GravScalar>(pos: [T; 3], reference_radius: f64, table_degree: usize) -> (Vec<T>, Vec<T>) {
    let [x, y, z] = pos;
    let re = T::constant(reference_radius);
    let r2 = x * x + y * y + z * z;
    let inv_r2 = T::constant(1.0) / r2;
    let rx = x * re * inv_r2;
    let ry = y * re * inv_r2;
    let rz = z * re * inv_r2;
    let rho2 = re * re * inv_r2;
    let r = r2.sqrt();

    let len = tri_idx(table_degree, table_degree) + 1;
    let mut v = vec![T::constant(0.0); len];
    let mut w = vec![T::constant(0.0); len];
    v[tri_idx(0, 0)] = re / r;

    for m in 1..=table_degree {
        let vprev = v[tri_idx(m - 1, m - 1)];
        let wprev = w[tri_idx(m - 1, m - 1)];
        let coeff = T::constant((2 * m - 1) as f64);
        v[tri_idx(m, m)] = coeff * (rx * vprev - ry * wprev);
        w[tri_idx(m, m)] = coeff * (rx * wprev + ry * vprev);
    }
    for m in 0..=table_degree {
        let mut n = m + 1;
        while n <= table_degree {
            let inv_nm = T::constant(1.0 / (n - m) as f64);
            let c1 = T::constant((2 * n - 1) as f64) * inv_nm;
            let vprev = v[tri_idx(n - 1, m)];
            let wprev = w[tri_idx(n - 1, m)];
            if n >= m + 2 {
                let c2 = T::constant((n + m - 1) as f64) * inv_nm;
                let vprev2 = v[tri_idx(n - 2, m)];
                let wprev2 = w[tri_idx(n - 2, m)];
                v[tri_idx(n, m)] = c1 * rz * vprev - c2 * rho2 * vprev2;
                w[tri_idx(n, m)] = c1 * rz * wprev - c2 * rho2 * wprev2;
            } else {
                v[tri_idx(n, m)] = c1 * rz * vprev;
                w[tri_idx(n, m)] = c1 * rz * wprev;
            }
            n += 1;
        }
    }
    (v, w)
}

/// The spherical-harmonic acceleration only (no point-mass term folded in beyond whatever
/// `(0,0)` the model itself carries -- `GravityModel::c(0,0)` is always `1.0`, so calling this
/// with the model as loaded already includes the point-mass term; see
/// `degree_zero_matches_point_mass`).
fn sh_acceleration<T: GravScalar>(pos: [T; 3], model: &GravityModel, coeffs: &UnnormalizedCoefficients) -> [T; 3] {
    let table_degree = model.max_degree() + 1;
    let (v, w) = compute_vw(pos, model.reference_radius(), table_degree);

    let mut ax = T::constant(0.0);
    let mut ay = T::constant(0.0);
    let mut az = T::constant(0.0);
    let half = T::constant(0.5);

    for n in 0..=model.max_degree() {
        for m in 0..=n.min(model.max_order()) {
            let cnm = coeffs.c(n, m);
            let snm = coeffs.s(n, m);
            if cnm == 0.0 && snm == 0.0 {
                continue;
            }
            let cnm_t = T::constant(cnm);
            let snm_t = T::constant(snm);

            if m == 0 {
                ax = ax + (-cnm_t) * v[tri_idx(n + 1, 1)];
                ay = ay + (-cnm_t) * w[tri_idx(n + 1, 1)];
            } else {
                let vp1 = v[tri_idx(n + 1, m + 1)];
                let wp1 = w[tri_idx(n + 1, m + 1)];
                let vm1 = v[tri_idx(n + 1, m - 1)];
                let wm1 = w[tri_idx(n + 1, m - 1)];
                let factor = T::constant(0.5 * ((n - m + 2) * (n - m + 1)) as f64);
                ax = ax + half * (-cnm_t * vp1 - snm_t * wp1) + factor * (cnm_t * vm1 + snm_t * wm1);
                ay = ay + half * (-cnm_t * wp1 + snm_t * vp1) + factor * (-cnm_t * wm1 + snm_t * vm1);
            }
            let nm1 = T::constant((n - m + 1) as f64);
            az = az + nm1 * (-cnm_t * v[tri_idx(n + 1, m)] - snm_t * w[tri_idx(n + 1, m)]);
        }
    }

    let scale = T::constant(model.mu() / (model.reference_radius() * model.reference_radius()));
    [ax * scale, ay * scale, az * scale]
}

/// Point-mass (two-body) acceleration, `a = -mu r / |r|^3`, body-fixed or inertial -- this
/// formula does not care which, since it uses no frame-specific quantity beyond `mu`.
pub fn point_mass_acceleration(pos: [f64; 3], mu: f64) -> [f64; 3] {
    let r2 = pos[0] * pos[0] + pos[1] * pos[1] + pos[2] * pos[2];
    let r = r2.sqrt();
    let scale = -mu / (r2 * r);
    [pos[0] * scale, pos[1] * scale, pos[2] * scale]
}

/// The closed-form point-mass partial, `d a / d r = -mu/|r|^3 I + 3 mu r r^T / |r|^5`.
/// Returned row-major: `partials[i][j] = d a_i / d r_j`.
pub fn point_mass_partials(pos: [f64; 3], mu: f64) -> [[f64; 3]; 3] {
    let r2 = pos[0] * pos[0] + pos[1] * pos[1] + pos[2] * pos[2];
    let r = r2.sqrt();
    let r3 = r2 * r;
    let r5 = r3 * r2;
    let mut out = [[0.0_f64; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            let identity = if i == j { 1.0 } else { 0.0 };
            out[i][j] = -mu / r3 * identity + 3.0 * mu * pos[i] * pos[j] / r5;
        }
    }
    out
}

/// Spherical-harmonic gravity acceleration in the body-fixed frame, to `model`'s degree and
/// order, plus its exact 3x3 partial-derivative (gravity-gradient) matrix in the same frame.
/// `pos` is body-fixed Cartesian, metres. `partials[i][j] = d(accel_i) / d(pos_j)`.
///
/// No frame rotation: `pos` in, body-fixed `accel`/`partials` out (see this module's doc).
pub fn spherical_harmonic_gravity(pos: [f64; 3], model: &GravityModel) -> ([f64; 3], [[f64; 3]; 3]) {
    let coeffs = UnnormalizedCoefficients::from_model(model);

    let accel_f64 = sh_acceleration([pos[0], pos[1], pos[2]], model, &coeffs);
    let accel = [accel_f64[0].value(), accel_f64[1].value(), accel_f64[2].value()];

    let dual_pos = [
        Dual3::variable(pos[0], 0),
        Dual3::variable(pos[1], 1),
        Dual3::variable(pos[2], 2),
    ];
    let accel_dual = sh_acceleration(dual_pos, model, &coeffs);
    let mut partials = [[0.0_f64; 3]; 3];
    for i in 0..3 {
        partials[i] = accel_dual[i].d;
    }

    (accel, partials)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cof::read_earth_gravity;
    use crate::legendre::NormalizedLegendre;
    use std::path::PathBuf;

    fn jgm2_dir() -> PathBuf {
        crate::cof::locate_gmat_root()
            .expect("GMAT_ROOT must be set to run this test")
            .join("data")
            .join("gravity")
            .join("earth")
    }

    /// A representative LEO-ish body-fixed position, deliberately not on an axis.
    const LEO_POS: [f64; 3] = [4_517_590.0, 2_662_240.0, 4_524_130.0];

    #[test]
    fn point_mass_matches_textbook_formula() {
        let mu = 3.986_004_415e14;
        let pos = LEO_POS;
        let a = point_mass_acceleration(pos, mu);
        let r = (pos[0] * pos[0] + pos[1] * pos[1] + pos[2] * pos[2]).sqrt();
        for i in 0..3 {
            let expected = -mu * pos[i] / r.powi(3);
            assert!((a[i] - expected).abs() < 1e-9 * expected.abs());
        }
    }

    #[test]
    fn point_mass_partials_match_finite_difference() {
        let mu = 3.986_004_415e14;
        let pos = LEO_POS;
        let analytic = point_mass_partials(pos, mu);
        let h = 1.0; // metres; position magnitude ~6.5e6 m, so this is a relative step ~1.5e-7
        let mut max_rel_err = 0.0_f64;
        for j in 0..3 {
            let mut plus = pos;
            let mut minus = pos;
            plus[j] += h;
            minus[j] -= h;
            let a_plus = point_mass_acceleration(plus, mu);
            let a_minus = point_mass_acceleration(minus, mu);
            for i in 0..3 {
                let fd = (a_plus[i] - a_minus[i]) / (2.0 * h);
                let err = (fd - analytic[i][j]).abs();
                let rel = err / analytic[i][j].abs().max(1e-30);
                max_rel_err = max_rel_err.max(rel);
            }
        }
        println!("n1a-point-mass-partials: max_relative_err={max_rel_err:e} h={h}");
        assert!(max_rel_err < 1e-7, "{max_rel_err:e}");
    }

    #[test]
    fn degree_zero_matches_point_mass() {
        let dir = jgm2_dir();
        let model = read_earth_gravity(&dir.join("JGM2.cof"), 0, 0).expect("parse JGM2.cof");
        let (a_sh, _) = spherical_harmonic_gravity(LEO_POS, &model);
        let a_pm = point_mass_acceleration(LEO_POS, model.mu());
        println!("n1a-degree0: sh={a_sh:?} point_mass={a_pm:?}");
        for i in 0..3 {
            let err = (a_sh[i] - a_pm[i]).abs();
            let rel = err / a_pm[i].abs();
            assert!(rel < 1e-13, "component {i}: sh={} pm={} rel_err={rel:e}", a_sh[i], a_pm[i]);
        }
    }

    /// Degree-2, `C_20` only (`C_21=S_21=C_22=S_22=0`, i.e. an artificially truncated
    /// JGM2), against the closed-form J2 acceleration written out explicitly here (e.g.
    /// Vallado, *Fundamentals of Astrodynamics*, the standard J2 perturbation formula):
    /// `a = -1.5 J2 (mu/r^2) (Re/r)^2 [ (x/r)(1 - 5(z/r)^2), (y/r)(1 - 5(z/r)^2),
    /// (z/r)(3 - 5(z/r)^2) ]`, `J2 = -C_20` (unnormalised).
    #[test]
    fn j2_only_matches_closed_form_at_several_positions() {
        let dir = jgm2_dir();
        let full = read_earth_gravity(&dir.join("JGM2.cof"), 2, 2).expect("parse JGM2.cof");

        // A synthetic .cof with every coefficient zero except C_20 (JGM2's real value), so
        // sh_acceleration exercises exactly the J2 term plus the always-present point-mass
        // term, which the closed form below adds back explicitly. Built by round-tripping
        // through the real fixed-width RECOEF format (not a second, ad hoc constructor on
        // GravityModel) so this test exercises the same parser as every other test here.
        let c20 = full.c(2, 0);
        let potfield = format!(
            "POTFIELD{:>3}{:>3}  1 {:e} {:e} 1.0",
            2,
            2,
            full.mu(),
            full.reference_radius()
        );
        let c_field = format!("{:>21}", format!("{c20:.14e}"));
        let recoef = format!("RECOEF{:>5}{:>3}   {c_field}", 2, 0);
        let synthetic = format!("{potfield}\n{recoef}\nEND\n");
        let scratch = std::env::temp_dir().join("av_orbital_j2_only_test.cof");
        std::fs::write(&scratch, synthetic).expect("write scratch .cof");
        let zonal_only = read_earth_gravity(&scratch, 2, 2).expect("parse synthetic zonal-only .cof");
        let _ = std::fs::remove_file(&scratch);

        let mu = full.mu();
        let re = full.reference_radius();
        let j2 = -c20 * 5.0_f64.sqrt();

        let positions: [[f64; 3]; 5] = [
            LEO_POS,
            [7_000_000.0, 0.0, 0.0],           // equatorial
            [0.0, 0.0, 7_000_000.0 - 1.0],     // near-polar (avoid exact pole: x=y=0 is fine
            // here since J2 has no longitude dependence, but keep a tiny offset for realism)
            [10.0, 10.0, 7_000_000.0],
            [-3_000_000.0, 6_000_000.0, -2_000_000.0],
        ];

        let mut max_rel_err = 0.0_f64;
        for pos in positions {
            let (a, _) = spherical_harmonic_gravity(pos, &zonal_only);
            let a_point_mass = point_mass_acceleration(pos, mu);
            let r = (pos[0] * pos[0] + pos[1] * pos[1] + pos[2] * pos[2]).sqrt();
            let z_over_r = pos[2] / r;
            let common = -1.5 * j2 * (mu / (r * r)) * (re / r) * (re / r);
            let expected_j2 = [
                common * (pos[0] / r) * (1.0 - 5.0 * z_over_r * z_over_r),
                common * (pos[1] / r) * (1.0 - 5.0 * z_over_r * z_over_r),
                common * (pos[2] / r) * (3.0 - 5.0 * z_over_r * z_over_r),
            ];
            for i in 0..3 {
                let expected_total = a_point_mass[i] + expected_j2[i];
                let err = (a[i] - expected_total).abs();
                let rel = err / expected_total.abs().max(1e-20);
                max_rel_err = max_rel_err.max(rel);
            }
        }
        println!("n1a-j2-closed-form: max_relative_err={max_rel_err:e}");
        assert!(max_rel_err < 1e-12, "{max_rel_err:e}");
    }

    /// Independent cross-check of the `m >= 1` (non-zonal) terms the J2-only test above does
    /// not exercise: the classical latitude/longitude potential formula (built from
    /// `legendre.rs`'s separately-tested `P̄_nm`, a different code path from this module's
    /// Cunningham/Gottlieb recursion), central-differenced to a gradient, against this
    /// module's acceleration, at a real degree-2 JGM2 field (`C_20`, `C_21`, `S_21`, `C_22`,
    /// `S_22` all populated) and a position away from any pole (so the classical formula's
    /// own coordinate singularity is not in play for the reference side of this check).
    #[test]
    fn matches_classical_potential_gradient_at_degree_2() {
        let dir = jgm2_dir();
        let model = read_earth_gravity(&dir.join("JGM2.cof"), 2, 2).expect("parse JGM2.cof");

        let potential = |pos: [f64; 3]| -> f64 {
            let r = (pos[0] * pos[0] + pos[1] * pos[1] + pos[2] * pos[2]).sqrt();
            let lat_sin = pos[2] / r;
            let lambda = pos[1].atan2(pos[0]);
            let legendre = NormalizedLegendre::new(lat_sin, 2).expect("u in range");
            let re_over_r = model.reference_radius() / r;
            let mut u = 1.0; // n=0 (point mass) term: P00=1, C00=1
            for n in 1..=2usize {
                let mut inner = model.c(n, 0) * legendre.get(n, 0);
                for m in 1..=n {
                    inner += (model.c(n, m) * (m as f64 * lambda).cos()
                        + model.s(n, m) * (m as f64 * lambda).sin())
                        * legendre.get(n, m);
                }
                u += re_over_r.powi(n as i32) * inner;
            }
            model.mu() / r * u
        };

        let (a_cg, _) = spherical_harmonic_gravity(LEO_POS, &model);

        let h = 0.5; // metres
        let mut a_fd = [0.0_f64; 3];
        for j in 0..3 {
            let mut plus = LEO_POS;
            let mut minus = LEO_POS;
            plus[j] += h;
            minus[j] -= h;
            // U = mu/r * (...) is the attractive-convention *positive* potential this
            // crate's own point-mass formula matches (a = -mu r/|r|^3 = +grad(mu/r), not
            // -grad(mu/r)): d(mu/r)/dx = -mu x/r^3 = a_x. So the gradient, not its negation,
            // is the reference acceleration here.
            a_fd[j] = (potential(plus) - potential(minus)) / (2.0 * h);
        }

        println!("n1a-classical-crosscheck: cunningham_gottlieb={a_cg:?} classical_fd={a_fd:?}");
        let mut max_rel_err = 0.0_f64;
        for i in 0..3 {
            let err = (a_cg[i] - a_fd[i]).abs();
            let rel = err / a_fd[i].abs();
            max_rel_err = max_rel_err.max(rel);
        }
        println!("n1a-classical-crosscheck: max_relative_err={max_rel_err:e}");
        assert!(max_rel_err < 1e-7, "{max_rel_err:e}");
    }

    #[test]
    fn partials_match_central_finite_difference() {
        let dir = jgm2_dir();
        let model = read_earth_gravity(&dir.join("JGM2.cof"), 8, 8).expect("parse JGM2.cof");
        let (_, analytic) = spherical_harmonic_gravity(LEO_POS, &model);

        let h = 1.0; // metres, relative step ~1.6e-7 against |r|~6.5e6 m
        let mut fd = [[0.0_f64; 3]; 3];
        for j in 0..3 {
            let mut plus = LEO_POS;
            let mut minus = LEO_POS;
            plus[j] += h;
            minus[j] -= h;
            let (a_plus, _) = spherical_harmonic_gravity(plus, &model);
            let (a_minus, _) = spherical_harmonic_gravity(minus, &model);
            for i in 0..3 {
                fd[i][j] = (a_plus[i] - a_minus[i]) / (2.0 * h);
            }
        }

        let mut max_rel_err = 0.0_f64;
        for i in 0..3 {
            for j in 0..3 {
                let err = (analytic[i][j] - fd[i][j]).abs();
                let rel = err / fd[i][j].abs().max(1e-12);
                max_rel_err = max_rel_err.max(rel);
            }
        }
        println!("n1a-sh-partials-fd: max_relative_err={max_rel_err:e} h={h}");
        assert!(max_rel_err < 1e-7, "{max_rel_err:e}");
    }

    /// Regression: the acceleration must not jump across the pole. Evaluates on a fine path
    /// that passes directly over the body-fixed pole (x=y=0 at the midpoint) and checks that
    /// every sample is finite, then compares the step-to-step change in acceleration *right
    /// at* the pole crossing against the typical step-to-step change everywhere else on the
    /// same path. A real, smooth gravity field changes continuously along this path even far
    /// from the pole (that is expected, not a bug); what a coordinate-singularity artifact
    /// would look like is the pole step being *anomalously larger* than that typical
    /// variation, which this test measures directly rather than asserting an arbitrary
    /// absolute bound.
    #[test]
    fn continuous_across_the_pole() {
        let dir = jgm2_dir();
        let model = read_earth_gravity(&dir.join("JGM2.cof"), 8, 8).expect("parse JGM2.cof");
        let r = 7_000_000.0_f64;
        let n_steps = 2000;
        let mid = n_steps / 2; // theta=0 exactly here (x=y=0, the body-fixed pole)
        let mut samples = Vec::with_capacity(n_steps + 1);
        for i in 0..=n_steps {
            // theta sweeps the position from just below the pole, through it, to just above,
            // staying in the x-z plane (y=0) so the path crosses x=0 (the pole) at the
            // midpoint.
            let theta = -0.01 + 0.02 * (i as f64) / (n_steps as f64);
            let x = r * theta.sin();
            let z = r * theta.cos();
            let pos = [x, 0.0, z];
            let (a, _) = spherical_harmonic_gravity(pos, &model);
            for c in a {
                assert!(c.is_finite(), "non-finite acceleration at theta={theta}: {a:?}");
            }
            samples.push(a);
        }
        let step_norm = |a: [f64; 3], b: [f64; 3]| {
            ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
        };
        let mut steps: Vec<f64> = (1..samples.len()).map(|i| step_norm(samples[i], samples[i - 1])).collect();
        let pole_step = step_norm(samples[mid], samples[mid - 1]);
        steps.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let median_step = steps[steps.len() / 2];
        let ratio = pole_step / median_step;
        println!(
            "n1a-pole-continuity: pole_step={pole_step:e} median_step={median_step:e} \
             ratio={ratio:e} m/s^2 over {n_steps} steps"
        );
        // The pole-crossing step should be an ordinary member of the same distribution as
        // every other step on this path, not an outlier -- measured ratio recorded above and
        // in the N1 report.
        assert!(ratio < 3.0, "pole step is anomalously larger than typical: ratio={ratio:e}");
    }
}
