//! N4 (`docs/native-dynamics-plan.md`): the pure math the state transition matrix needs, kept
//! independent of any one force model so it can be unit-tested on its own --
//! [`crate::model::EarthGravityModel`]'s own `stm_derivatives` override (ADR-002 second
//! amendment) is the only caller, and does nothing more than gather each force's own position/
//! velocity partial (gravity's own `spherical_harmonic_gravity`, `crate::third_body`'s and
//! `crate::srp`'s analytic partials, `crate::drag`'s analytic velocity block and
//! finite-differenced position block) and hand them to [`stm_rate`] below.
//!
//! # The A-matrix, and why `[[0, I], [da_dr, da_dv]]`
//!
//! The physical state is `x = [r; v]` (position, velocity), `x' = f(x,t) = [v; a(r,v,t)]`. The
//! state transition matrix `Phi(t0,t)` satisfies `Phi' = A(t) Phi`, `A = d f / d x`:
//!
//! ```text
//! A = [ d(v)/d(r)   d(v)/d(v) ]   = [   0      I   ]
//!     [ d(a)/d(r)   d(a)/d(v) ]     [ da_dr   da_dv ]
//! ```
//!
//! (`d(v)/d(r) = 0` because velocity does not depend on position; `d(v)/d(v) = I` because
//! `x'`'s first three components are literally a copy of `x`'s last three -- the same fact
//! `crate::model`'s own module doc states for `derivatives` itself, "`d(pos)/dt == vel`
//! EXACTLY"). This is standard (e.g. Vallado, *Fundamentals of Astrodynamics*, the variational
//! equations for two-body-plus-perturbations motion) -- stated here, not merely assumed,
//! because [`stm_rate`]'s own correctness rests on it.
//!
//! `da_dr`/`da_dv` are the SUM of every configured force's own contribution -- ADR-002's third
//! amendment's own finding, applied here: each force fills whichever block(s) it actually has
//! a (possibly zero) dependence on, and the blocks simply add (partial derivatives of a sum are
//! the sum of the partial derivatives). See `crate::model::EarthGravityModel`'s own
//! `acceleration_partials` for exactly which force contributes to which block, analytically or
//! by finite difference, and the evidence for each.

use crate::frame::Rotation;

/// Rotates a 3x3 partial-derivative (gradient) matrix computed in the BODY-FIXED frame into
/// the INERTIAL frame, given the same [`Rotation`] (`r`: inertial -> body-fixed) the
/// acceleration itself was rotated through.
///
/// # Derivation (stated, not assumed -- this is exactly the kind of transpose/inverse mixup
/// this crate's own rules call out as the single most likely defect here)
///
/// `crate::model::EarthGravityModel::derivatives` computes
/// `accel_inertial(pos_inertial) = R^T * accel_fixed(R * pos_inertial)`, `R = rotation.r`
/// (inertial -> body-fixed; [`Rotation`]'s own doc comment states this direction). By the
/// chain rule,
///
/// ```text
/// d(accel_inertial)/d(pos_inertial) = R^T * [d(accel_fixed)/d(pos_fixed)] * d(pos_fixed)/d(pos_inertial)
///                                    = R^T * G_fixed * R
/// ```
///
/// (`pos_fixed = R * pos_inertial`, so `d(pos_fixed)/d(pos_inertial) = R` exactly; `G_fixed`
/// is `spherical_harmonic_gravity`'s own returned partial matrix, `G_fixed[i][j] =
/// d(accel_fixed_i)/d(pos_fixed_j)`). So the inertial gradient is `R^T G_fixed R` -- NOT
/// `R G_fixed R^T`, the natural-looking but WRONG mirror of how the acceleration itself
/// rotates (`R^T` on the outside, matching `accel_inertial = R^T accel_fixed(...)`, `R` on the
/// inside, matching `pos_fixed = R pos_inertial`) -- see
/// `tests::rotates_r_transpose_g_r_not_the_reversed_form` for the direction proved against a
/// central finite difference on a deliberately asymmetric field, mirroring
/// `tests/frame_gmat.rs::asymmetric_field_detects_a_transposed_rotation`'s own method.
pub fn rotate_gradient_body_to_inertial(rotation: &Rotation, g_fixed: [[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let r = rotation.r;
    // tmp = G_fixed * R  (tmp[k][j] = sum_l G_fixed[k][l] * R[l][j])
    let mut tmp = [[0.0_f64; 3]; 3];
    for k in 0..3 {
        for j in 0..3 {
            let mut s = 0.0;
            for l in 0..3 {
                s += g_fixed[k][l] * r[l][j];
            }
            tmp[k][j] = s;
        }
    }
    // out = R^T * tmp  (out[i][j] = sum_k R[k][i] * tmp[k][j])
    let mut out = [[0.0_f64; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            let mut s = 0.0;
            for k in 0..3 {
                s += r[k][i] * tmp[k][j];
            }
            out[i][j] = s;
        }
    }
    out
}

/// The `(row, k)` element of the 6x6 `A = [[0, I], [da_dr, da_dv]]` matrix (see this module's
/// own doc comment), `row`/`k` both `0..6`.
fn a_element(row: usize, k: usize, da_dr: &[[f64; 3]; 3], da_dv: &[[f64; 3]; 3]) -> f64 {
    if row < 3 {
        if k == row + 3 {
            1.0
        } else {
            0.0
        }
    } else {
        let r = row - 3;
        if k < 3 {
            da_dr[r][k]
        } else {
            da_dv[r][k - 3]
        }
    }
}

/// `d(Phi)/dt = A Phi`, row-major 36-flat (`out[row*6+col]`), matching ADR-002's second
/// amendment's own layout exactly -- the caller (`crate::model::EarthGravityModel::
/// stm_derivatives`) writes this directly into `augmented_state_dot[6..42]` (index
/// `6 + row*6 + col`, identical convention). `da_dr[i][j] = d(accel_i)/d(pos_j)`,
/// `da_dv[i][j] = d(accel_i)/d(vel_j)` (both row-major 3x3, the SUM of every configured
/// force's own contribution -- see this module's own doc comment). `phi` must be row-major
/// 36-flat.
///
/// # Panics
///
/// If `phi.len() != 36`.
pub fn stm_rate(da_dr: [[f64; 3]; 3], da_dv: [[f64; 3]; 3], phi: &[f64]) -> [f64; 36] {
    assert_eq!(phi.len(), 36, "stm_rate: phi is not a 36-element (6x6) row-major matrix");
    let mut out = [0.0_f64; 36];
    for row in 0..6 {
        for col in 0..6 {
            let mut s = 0.0;
            for k in 0..6 {
                let a_rk = a_element(row, k, &da_dr, &da_dv);
                if a_rk != 0.0 {
                    s += a_rk * phi[k * 6 + col];
                }
            }
            out[row * 6 + col] = s;
        }
    }
    out
}

/// The determinant of a 6x6 row-major matrix (Liouville/symplecticity checks: a conservative
/// system's `det(Phi)` must stay near 1). Gaussian elimination with partial pivoting (not a
/// hand-expanded 6x6 cofactor formula -- error-prone to transcribe correctly at this size, and
/// this crate's own rule against re-deriving a published closed form from memory when a
/// mechanical, independently-checkable method is available applies here too).
///
/// # Panics
///
/// If `m.len() != 36`.
pub fn det6(m: &[f64]) -> f64 {
    assert_eq!(m.len(), 36, "det6: not a 36-element (6x6) row-major matrix");
    let mut a = [[0.0_f64; 6]; 6];
    for row in 0..6 {
        for col in 0..6 {
            a[row][col] = m[row * 6 + col];
        }
    }
    let mut det = 1.0_f64;
    for col in 0..6 {
        // Partial pivot: the largest-magnitude entry in this column, at or below the diagonal.
        let mut pivot_row = col;
        let mut pivot_val = a[col][col].abs();
        for (row, candidate) in a.iter().enumerate().skip(col + 1) {
            let v = candidate[col].abs();
            if v > pivot_val {
                pivot_row = row;
                pivot_val = v;
            }
        }
        if pivot_val == 0.0 {
            return 0.0;
        }
        if pivot_row != col {
            a.swap(pivot_row, col);
            det = -det;
        }
        det *= a[col][col];
        for row in (col + 1)..6 {
            let factor = a[row][col] / a[col][col];
            if factor != 0.0 {
                // `row > col` always here, so `a[col]` (the pivot row) and `a[row]` (the row
                // being reduced) never alias -- `split_at_mut` makes that non-aliasing
                // explicit to the borrow checker instead of indexing `a` twice in one
                // expression.
                let (upper, lower) = a.split_at_mut(row);
                let pivot = &upper[col];
                let target = &mut lower[0];
                for (t, p) in target.iter_mut().zip(pivot.iter()).skip(col) {
                    *t -= factor * p;
                }
            }
        }
    }
    det
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cof;
    use crate::gravity;

    // -- stm_rate: pure 6x6 matrix math, no force model or GMAT needed ----------------------

    /// `A * I = A` -- feeding the identity `Phi` back must return `A` itself, flattened in the
    /// documented `row*6+col` layout, directly checkable by inspection.
    #[test]
    fn stm_rate_of_identity_phi_returns_a_itself() {
        let da_dr = [[1.0, 2.0, 3.0], [4.0, 5.0, 6.0], [7.0, 8.0, 9.0]];
        let da_dv = [[0.1, 0.2, 0.3], [0.4, 0.5, 0.6], [0.7, 0.8, 0.9]];
        let identity: Vec<f64> = (0..36).map(|k| if k / 6 == k % 6 { 1.0 } else { 0.0 }).collect();
        let out = stm_rate(da_dr, da_dv, &identity);

        for row in 0..6 {
            for col in 0..6 {
                let want = a_element(row, col, &da_dr, &da_dv);
                assert_eq!(out[row * 6 + col], want, "row={row} col={col}");
            }
        }
    }

    /// `A * Phi` for a non-trivial `Phi`, checked against a direct, independent hand-written
    /// 6x6 matrix multiply -- catches a row/column transposition in [`stm_rate`]'s own loop
    /// that feeding the identity above cannot (multiplying by the identity is too forgiving:
    /// swapping `row`/`col` in the assembly would still pass that test).
    #[test]
    fn stm_rate_matches_a_direct_6x6_matmul_for_a_non_trivial_phi() {
        let da_dr = [[1.0, 0.0, -2.0], [0.0, 3.0, 0.0], [5.0, 0.0, -1.0]];
        let da_dv = [[0.0, 0.1, 0.0], [-0.2, 0.0, 0.0], [0.0, 0.0, 0.3]];
        // An arbitrary, non-symmetric 6x6 Phi (values chosen so a transposed assembly gives a
        // visibly different answer, not merely a coincidentally-close one).
        let phi: Vec<f64> = (0..36).map(|k| (k as f64) * 0.1 - 1.0 + if k % 7 == 0 { 2.3 } else { 0.0 }).collect();

        let mut a = [[0.0_f64; 6]; 6];
        for (row, a_row) in a.iter_mut().enumerate() {
            for (k, v) in a_row.iter_mut().enumerate() {
                *v = a_element(row, k, &da_dr, &da_dv);
            }
        }
        let mut want = [0.0_f64; 36];
        for row in 0..6 {
            for col in 0..6 {
                let mut s = 0.0;
                for k in 0..6 {
                    s += a[row][k] * phi[k * 6 + col];
                }
                want[row * 6 + col] = s;
            }
        }

        let got = stm_rate(da_dr, da_dv, &phi);
        for i in 0..36 {
            assert!((got[i] - want[i]).abs() < 1e-12, "index {i}: got {} want {}", got[i], want[i]);
        }
    }

    /// The top-left 3x3 block (`row < 3`) must be exactly zero and the top-right 3x3 block
    /// exactly the identity, for ANY `da_dr`/`da_dv` -- pins `A`'s own `[[0, I], ...]` shape
    /// (this module's own doc comment) independent of any force model's numbers.
    #[test]
    fn top_block_of_a_is_exactly_zero_identity_regardless_of_the_force_partials() {
        let da_dr = [[123.0, -45.0, 6.0], [0.0, 0.0, 0.0], [1.0, 1.0, 1.0]];
        let da_dv = [[9.0, 9.0, 9.0], [0.0, 0.0, 0.0], [-1.0, -1.0, -1.0]];
        for row in 0..3 {
            for k in 0..6 {
                let want = if k == row + 3 { 1.0 } else { 0.0 };
                assert_eq!(a_element(row, k, &da_dr, &da_dv), want, "row={row} k={k}");
            }
        }
    }

    // -- det6 ---------------------------------------------------------------------------------

    #[test]
    fn det6_of_identity_is_one() {
        let identity: Vec<f64> = (0..36).map(|k| if k / 6 == k % 6 { 1.0 } else { 0.0 }).collect();
        assert!((det6(&identity) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn det6_of_a_singular_matrix_is_zero() {
        // Row 1 = 2 * row 0 -- singular by construction.
        let mut m = [0.0_f64; 36];
        for j in 0..6 {
            m[j] = (j + 1) as f64;
            m[6 + j] = 2.0 * (j + 1) as f64;
        }
        for j in 0..6 {
            m[2 * 6 + j] = if j == 2 { 1.0 } else { 0.0 };
        }
        for j in 0..6 {
            m[3 * 6 + j] = if j == 3 { 1.0 } else { 0.0 };
        }
        for j in 0..6 {
            m[4 * 6 + j] = if j == 4 { 1.0 } else { 0.0 };
        }
        for j in 0..6 {
            m[5 * 6 + j] = if j == 5 { 1.0 } else { 0.0 };
        }
        assert_eq!(det6(&m), 0.0);
    }

    /// A diagonal matrix's determinant is the product of its diagonal -- a simple,
    /// independently-obvious cross-check of the elimination's own bookkeeping (pivoting,
    /// sign).
    #[test]
    fn det6_of_a_diagonal_matrix_is_the_product_of_the_diagonal() {
        let diag = [2.0, -3.0, 0.5, 4.0, -1.0, 7.0];
        let mut m = [0.0_f64; 36];
        for i in 0..6 {
            m[i * 6 + i] = diag[i];
        }
        let want: f64 = diag.iter().product();
        let got = det6(&m);
        assert!((got - want).abs() < 1e-9 * want.abs(), "got {got} want {want}");
    }

    // -- rotate_gradient_body_to_inertial: direction proved, not assumed --------------------

    fn potfield_line(n: usize, m: usize, mu: f64, radius: f64) -> String {
        format!("POTFIELD{n:>3}{m:>3}  1 {mu:e} {radius:e} 1.0")
    }
    fn recoef_line(n: usize, m: usize, c: f64, s: f64) -> String {
        format!("RECOEF{n:>5}{m:>3}   {:>21}{:>21}", format!("{c:e}"), format!("{s:e}"))
    }

    /// A deliberately asymmetric degree/order-2 field (large synthetic `C22`/`S22`) -- mirrors
    /// `tests/frame_gmat.rs::write_synthetic_asymmetric_cof` exactly (a spherically symmetric
    /// field cannot catch a rotation transpose bug at all; see that test's own doc comment),
    /// duplicated here (rather than shared) because that helper lives in a separate,
    /// `gmat-frames`-gated integration test binary and this module's own rotation-direction
    /// test is deliberately GMAT-free (a synthetic, hand-built [`Rotation`] below plays the
    /// role GMAT's real `Gmat::convert` plays there).
    fn write_synthetic_asymmetric_cof() -> std::path::PathBuf {
        let mu = 3.986004415e14;
        let radius = 6_378_136.3;
        let mut content = String::new();
        content.push_str("CCCCC synthetic asymmetric field (av-orbital src/stm.rs tests) CCCCC\n");
        content.push_str(&potfield_line(2, 2, mu, radius));
        content.push('\n');
        content.push_str(&recoef_line(2, 0, -4.84165371736e-4, 0.0));
        content.push('\n');
        content.push_str(&recoef_line(2, 1, 0.0, 0.0));
        content.push('\n');
        content.push_str(&recoef_line(2, 2, 0.02, 0.015));
        content.push('\n');
        let path = std::env::temp_dir().join(format!("av_orbital_stm_synthetic_asym_{}_{:?}.cof", std::process::id(), std::thread::current().id()));
        std::fs::write(&path, content).expect("write synthetic .cof");
        path
    }

    /// A 90-degree rotation about z (inertial x -> body-fixed -y), the SAME `rz90` shape
    /// `crate::frame`'s own tests use -- picked because it is NOT its own inverse, so a
    /// transposed rotation gives a visibly, not merely numerically, different answer.
    fn rz90() -> Rotation {
        Rotation {
            r: [[0.0, 1.0, 0.0], [-1.0, 0.0, 0.0], [0.0, 0.0, 1.0]],
            r_dot: [[0.0; 3]; 3],
        }
    }

    /// **The rotation direction, proved against a central finite difference, not assumed.**
    /// `accel_inertial(pos) = R^T accel_fixed(R pos)`; a central finite difference of THIS
    /// function (never touching `rotate_gradient_body_to_inertial` at all) is the independent
    /// ground truth. `R^T G_fixed R` (this module's own [`rotate_gradient_body_to_inertial`])
    /// must match it; the reversed form `R G_fixed R^T` -- what a transpose/inverse mixup would
    /// produce -- must NOT, and must differ by a visible fraction of the signal on this
    /// deliberately asymmetric field, mirroring `tests/frame_gmat.rs::
    /// asymmetric_field_detects_a_transposed_rotation`'s own method exactly (see that test's
    /// own doc comment).
    #[test]
    fn rotates_r_transpose_g_r_not_the_reversed_form() {
        let path = write_synthetic_asymmetric_cof();
        let model = cof::read_earth_gravity(&path, 2, 2).expect("read synthetic field");
        std::fs::remove_file(&path).ok();

        let rotation = rz90();
        let pos_inertial = [6_800_000.0, 1_200_000.0, 2_300_000.0];

        let accel_inertial = |p: [f64; 3]| -> [f64; 3] {
            let p_fixed = rotation.apply(p);
            let (a_fixed, _) = gravity::spherical_harmonic_gravity(p_fixed, &model);
            rotation.apply_transpose(a_fixed)
        };

        let pos_fixed = rotation.apply(pos_inertial);
        let (_, g_fixed) = gravity::spherical_harmonic_gravity(pos_fixed, &model);
        let g_correct = rotate_gradient_body_to_inertial(&rotation, g_fixed);

        // The deliberately WRONG reversed form: R G_fixed R^T (swap which side gets the
        // transpose relative to the correct R^T G_fixed R).
        let r = rotation.r;
        let mut tmp = [[0.0_f64; 3]; 3];
        for k in 0..3 {
            for j in 0..3 {
                let mut s = 0.0;
                for l in 0..3 {
                    s += g_fixed[k][l] * r[j][l]; // G_fixed * R^T
                }
                tmp[k][j] = s;
            }
        }
        let mut g_wrong = [[0.0_f64; 3]; 3];
        for i in 0..3 {
            for j in 0..3 {
                let mut s = 0.0;
                for k in 0..3 {
                    s += r[i][k] * tmp[k][j]; // R * (G_fixed * R^T)
                }
                g_wrong[i][j] = s;
            }
        }

        let h = 1.0; // metres; |pos| ~ 7.2e6 m, relative step ~1.4e-7
        let mut fd = [[0.0_f64; 3]; 3];
        for j in 0..3 {
            let mut plus = pos_inertial;
            let mut minus = pos_inertial;
            plus[j] += h;
            minus[j] -= h;
            let a_plus = accel_inertial(plus);
            let a_minus = accel_inertial(minus);
            for i in 0..3 {
                fd[i][j] = (a_plus[i] - a_minus[i]) / (2.0 * h);
            }
        }

        let frob = |m: [[f64; 3]; 3]| -> f64 { m.iter().flatten().map(|v| v * v).sum::<f64>().sqrt() };
        let diff = |a: [[f64; 3]; 3], b: [[f64; 3]; 3]| -> f64 {
            let mut d = [[0.0_f64; 3]; 3];
            for i in 0..3 {
                for j in 0..3 {
                    d[i][j] = a[i][j] - b[i][j];
                }
            }
            frob(d)
        };

        let scale = frob(fd);
        let err_correct = diff(g_correct, fd) / scale;
        let err_wrong = diff(g_wrong, fd) / scale;
        println!("n4-gradient-rotation-direction: correct_rel_err={err_correct:e} wrong_rel_err={err_wrong:e} (of signal {scale:e})");
        assert!(err_correct < 1e-6, "R^T G_fixed R disagrees with a central finite difference by {err_correct:e} relative -- suspect the rotation direction");
        assert!(
            err_wrong > 0.01,
            "the reversed form R G_fixed R^T must be VISIBLY wrong on this asymmetric field (got only {err_wrong:e} relative difference from the correct answer) -- suspect the field is not asymmetric enough to catch a transpose bug"
        );
    }
}
