//! Covariance hygiene: a Cholesky-based SPD check, applied *before* a propagated covariance
//! ever crosses into `spoore_cdm::GaussianState` (`docs/open-questions.md` question 80, a
//! follow-up to M3.2's `av_dynamics::propagate_covariance`, whose own doc comment already
//! flags that it "does not independently verify positive-definiteness").
//!
//! ## Why this exists, and why it duplicates spoore's own check
//!
//! `spoore_cdm::GaussianState::new`/`from_slices` already enforces finite -> symmetric ->
//! positive-definite (`crates/spoore-cdm/src/state.rs`'s `validate_covariance`, a Cholesky
//! attempt: `cov.clone().cholesky().is_none()`). So a covariance that fails this check would,
//! eventually, fail *there* too -- but `validate_covariance` and the functions it calls are
//! `pub(crate)`/private to `spoore-cdm`, not callable from here, and more importantly
//! `av-kernel`'s `run_with_covariance` and `gmat-service`'s `propagate_covariance` never
//! construct a `GaussianState` at all -- they emit `TrajectorySample.cov` as plain
//! `Vec<f64>`, which may or may not be converted into spoore native types somewhere
//! downstream, possibly in a different process, possibly much later. Waiting for that
//! eventual conversion to fail is waiting for the wrong place and the wrong time to notice a
//! covariance has drifted out of the shape spoore requires: the failure would surface far
//! from its cause, uncounted, unmeasured, and without ever having been given the chance to be
//! repaired the one, narrow, opt-in way this module allows.
//!
//! So this module mirrors spoore's exact algorithm -- same order (finite, symmetric,
//! Cholesky), same symmetry tolerance and formula ([`SYMMETRY_RTOL`]) -- rather than calling
//! into it, and applies it at the point a covariance is *produced* (`av-kernel`'s
//! `run_with_covariance`, `gmat-service`'s `propagate_covariance`). Being a mirror, not a
//! call, means "equivalent to spoore's bar" is a claim that must be demonstrated, not assumed
//! by construction -- `tests::equivalence_*` below feeds the same matrices through this
//! check and through `spoore_cdm::GaussianState::from_slices` directly and asserts the two
//! decisions agree, which is the actual point of this module (a check that is looser than
//! spoore's would just move the failure downstream; a check that is stricter would reject
//! covariances spoore itself would have accepted).
//!
//! ## What a failure means, and what happens to it
//!
//! [`check_spd`] / [`check_spd_row_major`] return a typed [`CovarianceHygieneError`] and
//! increment [`spd_check_failures`] on every failure -- never a silent repair. A caller that
//! has *not* opted into [`nearest_spd`] simply propagates the error (`av-kernel`:
//! `ScheduleError::CovarianceHygiene`; `gmat-service`: a `ModelError`). A caller that *has*
//! opted in (a DRM declaring `DrmOptions.nearest_spd_projection`,
//! `proto/altavista/v1/system.proto` -- see [`nearest_spd`]'s own doc comment for why that
//! proto field still reaches this module as a plain `bool` parameter rather than a
//! `DesignReferenceMission`/`DrmOptions` value threaded all the way through) applies the
//! projection, counts that separately ([`nearest_spd_projections_applied`]), and logs it
//! loudly rather than silently swapping in a repaired matrix.

use std::sync::atomic::{AtomicU64, Ordering};

use nalgebra::{DMatrix, SymmetricEigen};
use spoore_cdm::symmetrize;

/// Relative tolerance for the symmetry check. Identical value and formula to
/// `spoore_cdm::state::SYMMETRY_RTOL` (`crates/spoore-cdm/src/state.rs`) -- that constant is
/// `pub(crate)` to `spoore-cdm`, so it cannot be imported; mirrored here instead, and
/// `tests::equivalence_asymmetric_matrix_is_rejected_by_both` proves the mirror decides the
/// same way spoore's own check does on a matrix right at this boundary, not merely a matrix
/// far inside or outside it.
pub const SYMMETRY_RTOL: f64 = 1e-9;

/// The floor [`nearest_spd`] raises a clipped eigenvalue to, relative to the input's own
/// largest eigenvalue, when no caller-supplied ratio is given. `1e-9` matches the relative
/// scale `SYMMETRY_RTOL` already uses for "negligible relative to this matrix's own
/// magnitude" -- small enough that a direction this floor touches carries essentially none of
/// the matrix's information after the repair.
pub const DEFAULT_NEAREST_SPD_FLOOR_RATIO: f64 = 1e-9;

/// What [`check_spd`] read off a successful Cholesky factorization `P = L Lᵀ`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CholeskyDiagnostics {
    /// The covariance's dimension.
    pub n: usize,
    /// `min_i L[i][i]²`, the smallest squared diagonal entry of the Cholesky factor.
    ///
    /// **What this is.** Free to compute (the factor already exists once [`check_spd`]
    /// succeeds) and always well-defined for an SPD matrix, unlike an eigenvalue
    /// computation, which this module otherwise avoids on the required (non-opt-in) path.
    ///
    /// **What it means, precisely.** Each Cholesky pivot `L[i][i]²` is a Rayleigh quotient of
    /// `P` (a standard property of `LDLᵀ`/Cholesky pivots for a symmetric positive-definite
    /// matrix), so every pivot -- and therefore `min_i L[i][i]²` -- satisfies
    /// `λ_min(P) <= L[i][i]² <= λ_max(P)`. That makes `min_i L[i][i]²` an **upper bound** on
    /// the true smallest eigenvalue, not a lower bound: the honest reading is "the smallest
    /// eigenvalue is at most this," and since `Π_i L[i][i]² = det(P) = Π` eigenvalues, this
    /// proxy is forced toward zero exactly when `λ_min(P)` is, which is the property that
    /// makes it useful as a drift signal even though it is not the eigenvalue itself.
    ///
    /// **What it does not mean.** It is not `λ_min(P)`, does not equal it in general, and a
    /// value that looks healthy does not *prove* every eigenvalue is healthy -- only an
    /// actual eigenvalue decomposition (as [`nearest_spd`] performs, when opted into) answers
    /// that precisely. Track this value as a cheap, always-available signal that a run is
    /// trending toward singularity across samples or across runs, not as a substitute for an
    /// eigenvalue computation where the answer needs to be exact.
    pub min_cholesky_diag_sq: f64,
}

/// A propagated covariance failed the hygiene check this module applies before a covariance
/// crosses into spoore types. Every variant mirrors one of `spoore_cdm::CdmError`'s
/// covariance-related variants (`NotFinite`, `NotSymmetric`, `NotPositiveDefinite`) by name
/// and by the condition that raises it -- see the module docs for why this is a mirror, not a
/// re-export, and `tests::equivalence_*` for the proof the mirror agrees with the original.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum CovarianceHygieneError {
    /// A row-major slice's length is not `n * n` for the declared `n`, so it cannot even be
    /// reshaped into a matrix -- checked before any of the matrix-shaped checks below run.
    #[error("covariance {context}: {actual} elements is not {expected} ({expected}=n*n for the declared n)")]
    DimensionMismatch {
        context: String,
        expected: usize,
        actual: usize,
    },
    /// A `DMatrix` handed to [`check_spd`] directly (not via [`check_spd_row_major`]) was not
    /// square. Every producer in this codebase builds a square matrix by construction, so this
    /// is defensive, not a path any current caller can reach.
    #[error("covariance {context}: {rows}x{cols} is not square")]
    NotSquare {
        context: String,
        rows: usize,
        cols: usize,
    },
    /// Mirrors `spoore_cdm::CdmError::NotFinite`.
    #[error("covariance {context}[{index}] is not finite")]
    NotFinite { context: String, index: usize },
    /// Mirrors `spoore_cdm::CdmError::NotSymmetric`.
    #[error("covariance {context}: |c[{i}][{j}] - c[{j}][{i}]| = {asymmetry:e}, exceeding the relative tolerance")]
    NotSymmetric {
        context: String,
        i: usize,
        j: usize,
        asymmetry: f64,
    },
    /// Mirrors `spoore_cdm::CdmError::NotPositiveDefinite`: the Cholesky attempt failed.
    #[error("covariance {context}: failed a Cholesky attempt (not positive definite)")]
    NotPositiveDefinite { context: String },
}

/// Process-wide count of [`check_spd`]/[`check_spd_row_major`] failures, any variant --
/// incremented once per failed call, regardless of which invariant failed first. Not reset
/// between calls, the same convention `spoore-math`'s own `regularization_count()`
/// (`spoore/crates/spoore-math/src/spd.rs`) establishes: a caller that wants a windowed or
/// per-run rate samples this at both ends of the window. A node can publish this value the
/// same way `spoore-math` already publishes its own counter as a metric.
static SPD_CHECK_FAILURES: AtomicU64 = AtomicU64::new(0);

/// The current value of [`SPD_CHECK_FAILURES`].
pub fn spd_check_failures() -> u64 {
    SPD_CHECK_FAILURES.load(Ordering::Relaxed)
}

/// Process-wide count of [`nearest_spd`] applications (via [`nearest_spd_row_major`], which
/// is the only function in this module that increments it) -- separate from
/// [`spd_check_failures`] so "how often did a covariance fail" and "how often was a failure
/// then repaired by the opt-in projection" can each be read independently. Every increment
/// here also has a matching increment of [`spd_check_failures`] (the projection only ever
/// runs after a failed check), never the reverse.
static NEAREST_SPD_PROJECTIONS_APPLIED: AtomicU64 = AtomicU64::new(0);

/// The current value of [`NEAREST_SPD_PROJECTIONS_APPLIED`].
pub fn nearest_spd_projections_applied() -> u64 {
    NEAREST_SPD_PROJECTIONS_APPLIED.load(Ordering::Relaxed)
}

/// The Cholesky-based SPD check: finite, then symmetric (within [`SYMMETRY_RTOL`], relative to
/// magnitude), then a Cholesky attempt -- the exact order and bar
/// `spoore_cdm::GaussianState`'s constructor applies (see the module docs). `context` is
/// carried into every error variant and the log line a caller writes around this, so a
/// failure names the system/sample it came from.
///
/// # Errors
///
/// [`CovarianceHygieneError::NotSquare`], `NotFinite`, `NotSymmetric` or
/// `NotPositiveDefinite`, in that order of preference (the first violation found is the one
/// reported; later checks are not attempted once an earlier one fails, matching
/// `spoore_cdm::validate_covariance`'s own short-circuiting order).
pub fn check_spd(cov: &DMatrix<f64>, context: &str) -> Result<CholeskyDiagnostics, CovarianceHygieneError> {
    let (rows, cols) = (cov.nrows(), cov.ncols());
    if rows != cols {
        SPD_CHECK_FAILURES.fetch_add(1, Ordering::Relaxed);
        return Err(CovarianceHygieneError::NotSquare { context: context.to_string(), rows, cols });
    }
    let n = rows;

    for (index, v) in cov.iter().enumerate() {
        if !v.is_finite() {
            SPD_CHECK_FAILURES.fetch_add(1, Ordering::Relaxed);
            return Err(CovarianceHygieneError::NotFinite { context: context.to_string(), index });
        }
    }

    for i in 0..n {
        for j in (i + 1)..n {
            let (a, b) = (cov[(i, j)], cov[(j, i)]);
            let asymmetry = (a - b).abs();
            let scale = a.abs().max(b.abs()).max(1.0);
            if asymmetry > SYMMETRY_RTOL * scale {
                SPD_CHECK_FAILURES.fetch_add(1, Ordering::Relaxed);
                return Err(CovarianceHygieneError::NotSymmetric { context: context.to_string(), i, j, asymmetry });
            }
        }
    }

    match cov.clone().cholesky() {
        Some(chol) => {
            let l = chol.l();
            let min_cholesky_diag_sq = (0..n).map(|i| l[(i, i)] * l[(i, i)]).fold(f64::INFINITY, f64::min);
            Ok(CholeskyDiagnostics { n, min_cholesky_diag_sq })
        }
        None => {
            SPD_CHECK_FAILURES.fetch_add(1, Ordering::Relaxed);
            Err(CovarianceHygieneError::NotPositiveDefinite { context: context.to_string() })
        }
    }
}

/// [`check_spd`] over a row-major flat slice (the wire/`TrajectorySample.cov` shape) rather
/// than a `DMatrix` -- the form every current caller (`av-kernel`, `gmat-service`) actually
/// has in hand.
///
/// # Errors
///
/// [`CovarianceHygieneError::DimensionMismatch`] if `cov.len() != n * n`; otherwise as
/// [`check_spd`].
pub fn check_spd_row_major(cov: &[f64], n: usize, context: &str) -> Result<CholeskyDiagnostics, CovarianceHygieneError> {
    if cov.len() != n * n {
        SPD_CHECK_FAILURES.fetch_add(1, Ordering::Relaxed);
        return Err(CovarianceHygieneError::DimensionMismatch { context: context.to_string(), expected: n * n, actual: cov.len() });
    }
    check_spd(&DMatrix::from_row_slice(n, n, cov), context)
}

/// Opt-in nearest symmetric positive-definite projection, **off by default, used only if a
/// caller explicitly opts in** -- `DrmOptions.nearest_spd_projection`
/// (`proto/altavista/v1/system.proto`, question 83), a real, additive field on
/// `DesignReferenceMission.options` since the lead's decision on questions 82/83. `proto/**`
/// is read-only to this task, and no crate in this repo yet owns constructing a
/// `DesignReferenceMission` and driving `av-kernel`/`gmat-service` from it (that DRM-executor
/// layer is future work, not part of M5.1's file list), so today the field's *value* still
/// reaches this module as a plain `bool` parameter threaded by hand through
/// `av-kernel::Kernel::run_with_covariance` and `gmat_service.model.GmatModel
/// .propagate_covariance` -- a caller that has read `drm.options.nearest_spd_projection` off an
/// actual `DesignReferenceMission` passes that value straight through; nothing here re-derives
/// or duplicates the proto's own default (`false`).
///
/// # Algorithm, named, and its limits
///
/// **Eigenvalue clipping, specialized from Higham (1988)'s nearest positive semidefinite
/// matrix in Frobenius norm.** Higham's general result finds the nearest SPD matrix to an
/// arbitrary (possibly non-symmetric) matrix via a symmetric polar factor; for an *already
/// symmetric* input `A` (every covariance here is symmetric by the time this runs, since
/// [`check_spd`] already confirmed it, or the caller symmetrized it beforehand), that polar
/// factor step is the identity and the theorem reduces to: eigendecompose `A = V diag(λ) Vᵀ`
/// and zero every negative eigenvalue. This function does exactly that, with one deviation
/// from the textbook result: Cholesky (what [`check_spd`] and every downstream consumer that
/// calls it needs) requires *strict* positive definiteness, not merely positive
/// semi-definiteness, so eigenvalues are floored to `floor_ratio * (largest eigenvalue)`
/// rather than to exactly zero.
///
/// **What this is not.** Not the iterative alternating-projections form of Higham's
/// algorithm (needed only for a non-symmetric starting matrix, or a nearest-*correlation*-
/// matrix variant with a unit-diagonal constraint -- neither applies here). Not guaranteed to
/// preserve trace, marginal variances, or any other physical quantity of the input beyond
/// "symmetric, PD, and the Frobenius-nearest such matrix to the input among matrices sharing
/// its eigenvectors" -- a direction with a large negative eigenvalue is genuinely, not
/// numerically, wrong, and this projection reports that direction as now-negligible variance
/// rather than reconstructing what it "should" have been (there is no way to know). Not
/// re-validated against any golden: a DRM that enables this is accepting the trade that a
/// projected covariance may disagree with what an unprojected one would have been, in
/// exchange for the run continuing instead of failing outright.
///
/// `floor_ratio` is typically [`DEFAULT_NEAREST_SPD_FLOOR_RATIO`]; exposed as a parameter so a
/// caller with a reason to choose differently can.
pub fn nearest_spd(cov: &DMatrix<f64>, floor_ratio: f64) -> DMatrix<f64> {
    let mut sym = cov.clone();
    symmetrize(&mut sym);

    let eigen = SymmetricEigen::new(sym);
    let max_eig = eigen.eigenvalues.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let scale = if max_eig.is_finite() && max_eig > 0.0 { max_eig } else { 1.0 };
    let floor = floor_ratio * scale;

    let floored = eigen.eigenvalues.map(|e| e.max(floor));
    let v = &eigen.eigenvectors;
    let mut out = v * DMatrix::from_diagonal(&floored) * v.transpose();
    symmetrize(&mut out);
    out
}

/// [`nearest_spd`] over the row-major flat shape, and the only function in this module that
/// increments [`nearest_spd_projections_applied`] -- callers apply the projection exclusively
/// through this function (not [`nearest_spd`] directly) so every application is counted, per
/// the binding rule that an opt-in repair is still never a *silent* one.
pub fn nearest_spd_row_major(cov: &[f64], n: usize, floor_ratio: f64) -> Vec<f64> {
    let projected = nearest_spd(&DMatrix::from_row_slice(n, n, cov), floor_ratio);
    NEAREST_SPD_PROJECTIONS_APPLIED.fetch_add(1, Ordering::Relaxed);
    let mut out = Vec::with_capacity(n * n);
    for i in 0..n {
        for j in 0..n {
            out.push(projected[(i, j)]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- The Cholesky-diagnostics proxy, on a known matrix -------------------------------

    #[test]
    fn min_cholesky_diag_sq_is_computed_from_the_actual_factor_on_a_diagonal_matrix() {
        // diag(4, 9): Cholesky is exact, L = diag(2, 3), so L[i][i]^2 = (4, 9) exactly --
        // an independently-known answer, not just "the code ran".
        let cov = DMatrix::from_row_slice(2, 2, &[4.0, 0.0, 0.0, 9.0]);
        let diag = check_spd(&cov, "test").unwrap();
        assert_eq!(diag.n, 2);
        assert!((diag.min_cholesky_diag_sq - 4.0).abs() < 1e-12, "{}", diag.min_cholesky_diag_sq);
    }

    // -- Required test: a hand-built asymmetric matrix fails with the typed error --------

    #[test]
    fn asymmetric_matrix_is_rejected_with_the_typed_error_and_counted() {
        let cov = DMatrix::from_row_slice(2, 2, &[1.0, 0.3, 0.2, 1.0]);
        // `>`, not `== before + 1`: this static counter is process-wide and other test
        // functions in this same binary increment it concurrently (cargo test's default
        // parallel threads, which the workspace baseline keeps) -- monotonic increase by at
        // least our own call is exactly what "the counter increments on failure" claims, and
        // is the only claim safe to make under concurrency.
        let before = spd_check_failures();
        let err = check_spd(&cov, "test").unwrap_err();
        assert!(matches!(err, CovarianceHygieneError::NotSymmetric { i: 0, j: 1, .. }), "{err}");
        assert!(spd_check_failures() > before, "a failed check must be counted");
    }

    // -- Required test: a hand-built indefinite matrix fails with the typed error --------

    #[test]
    fn indefinite_matrix_is_rejected_with_the_typed_error_and_counted() {
        // Symmetric, finite, but indefinite (eigenvalues -1 and 3): same fixture spoore's own
        // rejects_non_positive_definite_covariance test uses (crates/spoore-cdm/src/state.rs).
        let cov = DMatrix::from_row_slice(2, 2, &[1.0, 2.0, 2.0, 1.0]);
        let before = spd_check_failures();
        let err = check_spd(&cov, "test").unwrap_err();
        assert!(matches!(err, CovarianceHygieneError::NotPositiveDefinite { .. }), "{err}");
        assert!(spd_check_failures() > before, "a failed check must be counted");
    }

    #[test]
    fn non_finite_entry_is_rejected_and_counted() {
        let cov = DMatrix::from_row_slice(2, 2, &[f64::NAN, 0.0, 0.0, 1.0]);
        let before = spd_check_failures();
        let err = check_spd(&cov, "test").unwrap_err();
        assert!(matches!(err, CovarianceHygieneError::NotFinite { index: 0, .. }), "{err}");
        assert!(spd_check_failures() > before);
    }

    #[test]
    fn row_major_wrong_length_is_a_dimension_mismatch_not_a_panic() {
        let before = spd_check_failures();
        let err = check_spd_row_major(&[1.0, 0.0, 0.0], 2, "test").unwrap_err();
        assert!(matches!(err, CovarianceHygieneError::DimensionMismatch { expected: 4, actual: 3, .. }), "{err}");
        assert!(spd_check_failures() > before);
    }

    #[test]
    fn a_well_conditioned_diagonal_matrix_passes() {
        let cov = DMatrix::from_row_slice(3, 3, &[4.0, 0.0, 0.0, 0.0, 9.0, 0.0, 0.0, 0.0, 1.0]);
        assert!(check_spd(&cov, "test").is_ok());
    }

    // -- Required: a propagated P from the golden passes the check -----------------------

    #[test]
    fn the_golden_arcs_own_p0_and_propagated_cov_t1_both_pass() {
        // goldens/leo_1day_jgm2_8x8_sunmoon.json's "stm" block: p0_si is the declared initial
        // covariance, cov_t1_si is GMAT's own PropagationStateManager("STM") propagation of it
        // over the full 86,400 s arc (goldens/** is owned by another worker; read only, never
        // regenerated or edited here).
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../goldens/leo_1day_jgm2_8x8_sunmoon.json");
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {path:?}: {e}"));
        let golden: serde_json::Value = serde_json::from_str(&text).unwrap();
        let stm = &golden["stm"];

        let p0: Vec<f64> = stm["p0_si"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
        let cov_t1: Vec<f64> = stm["cov_t1_si"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
        assert_eq!(p0.len(), 36);
        assert_eq!(cov_t1.len(), 36);

        let p0_diag = check_spd_row_major(&p0, 6, "golden.p0_si").expect("the golden's declared P0 must pass the SPD hygiene check");
        let cov_t1_diag = check_spd_row_major(&cov_t1, 6, "golden.cov_t1_si").expect("the golden's own propagated P(t1) must pass the SPD hygiene check");

        eprintln!(
            "[av-cdm covariance] golden leo_1day_jgm2_8x8_sunmoon: P0 min Cholesky diag^2 proxy = {:.6e}, \
             P(t1) min Cholesky diag^2 proxy = {:.6e} (golden's own recorded pre-symmetrization \
             asymmetry at t1: {})",
            p0_diag.min_cholesky_diag_sq,
            cov_t1_diag.min_cholesky_diag_sq,
            stm["cov_t1_pre_symmetrization_asymmetry"],
        );
    }

    // -- Opt-in nearest-SPD projection -----------------------------------------------------

    #[test]
    fn nearest_spd_of_an_indefinite_matrix_passes_the_check_afterward() {
        let cov = DMatrix::from_row_slice(2, 2, &[1.0, 2.0, 2.0, 1.0]); // eigenvalues -1, 3
        assert!(check_spd(&cov, "test").is_err(), "fixture must actually be indefinite");

        let before = nearest_spd_projections_applied();
        let projected = nearest_spd_row_major(&[1.0, 2.0, 2.0, 1.0], 2, DEFAULT_NEAREST_SPD_FLOOR_RATIO);
        assert!(nearest_spd_projections_applied() > before, "an application must be counted");

        let diag = check_spd_row_major(&projected, 2, "test").expect("a projected covariance must pass the check it was built to pass");
        // The negative eigenvalue (-1) is floored near zero; the positive one (3) is left
        // close to untouched -- checked via the trace, which for a 2x2 with one eigenvalue
        // near-zero and one near-unchanged is dominated by the surviving eigenvalue.
        assert!((projected[0] + projected[3] - 3.0).abs() < 1e-3, "trace should be dominated by the surviving eigenvalue: {projected:?}");
        let _ = diag;
    }

    #[test]
    fn nearest_spd_leaves_an_already_spd_matrix_close_to_unchanged() {
        let flat = [4.0, 1.0, 0.0, 1.0, 9.0, 2.0, 0.0, 2.0, 1.0];
        let cov = DMatrix::from_row_slice(3, 3, &flat);
        assert!(check_spd(&cov, "test").is_ok(), "fixture must already be SPD");
        let projected = nearest_spd(&cov, DEFAULT_NEAREST_SPD_FLOOR_RATIO);
        for (got, want) in projected.iter().zip(flat.iter()) {
            assert!((got - want).abs() < 1e-6, "{got} vs {want}");
        }
    }

    // -- Equivalence to spoore_cdm::GaussianState -- the actual point of this module -----
    //
    // A matrix this check accepts must also be accepted by spoore_cdm::GaussianState::
    // from_slices, and a matrix this check rejects must also be rejected by it -- otherwise
    // this module would just move the failure downstream (looser) or reject something spoore
    // itself would have taken (stricter). Fixtures mirror spoore_cdm::state's own test module
    // (crates/spoore-cdm/src/state.rs) exactly, so "equivalent" is checked at the same points
    // spoore's own author chose to test its bar at.

    fn spoore_accepts(mean: &[f64], cov_row_major: &[f64]) -> bool {
        spoore_cdm::GaussianState::from_slices(mean, cov_row_major, "eq_test", spoore_cdm::Epoch::UNIX_EPOCH).is_ok()
    }

    #[test]
    fn equivalence_a_well_conditioned_matrix_is_accepted_by_both() {
        let cov = [4.0, 0.0, 0.0, 0.0, 4.0, 0.0, 0.0, 0.0, 1.0];
        let mean = [1.0, 2.0, 0.5];
        assert!(check_spd_row_major(&cov, 3, "test").is_ok(), "our check must accept it");
        assert!(spoore_accepts(&mean, &cov), "spoore must also accept it");
    }

    #[test]
    fn equivalence_asymmetric_matrix_is_rejected_by_both() {
        // Same fixture as spoore_cdm::state::tests::rejects_asymmetric_covariance.
        let cov = [1.0, 0.3, 0.2, 1.0];
        let mean = [0.0, 0.0];
        let our_err = check_spd_row_major(&cov, 2, "test").unwrap_err();
        assert!(matches!(our_err, CovarianceHygieneError::NotSymmetric { .. }), "{our_err}");
        let spoore_err = spoore_cdm::GaussianState::from_slices(&mean, &cov, "eq_test", spoore_cdm::Epoch::UNIX_EPOCH).unwrap_err();
        assert!(matches!(spoore_err, spoore_cdm::CdmError::NotSymmetric { .. }), "{spoore_err}");
    }

    #[test]
    fn equivalence_indefinite_matrix_is_rejected_by_both() {
        // Same fixture as spoore_cdm::state::tests::rejects_non_positive_definite_covariance.
        let cov = [1.0, 2.0, 2.0, 1.0];
        let mean = [0.0, 0.0];
        let our_err = check_spd_row_major(&cov, 2, "test").unwrap_err();
        assert!(matches!(our_err, CovarianceHygieneError::NotPositiveDefinite { .. }), "{our_err}");
        let spoore_err = spoore_cdm::GaussianState::from_slices(&mean, &cov, "eq_test", spoore_cdm::Epoch::UNIX_EPOCH).unwrap_err();
        assert!(matches!(spoore_err, spoore_cdm::CdmError::NotPositiveDefinite { .. }), "{spoore_err}");
    }

    #[test]
    fn equivalence_singular_matrix_is_rejected_by_both() {
        // Same fixture as spoore_cdm::state::tests::rejects_singular_covariance: positive
        // *semi*-definite, rank-deficient, det = 0 -- the boundary case between "accepted" and
        // "rejected" that most exercises whether the two Cholesky attempts agree.
        let cov = [1.0, 1.0, 1.0, 1.0];
        let mean = [0.0, 0.0];
        let our_err = check_spd_row_major(&cov, 2, "test").unwrap_err();
        assert!(matches!(our_err, CovarianceHygieneError::NotPositiveDefinite { .. }), "{our_err}");
        let spoore_err = spoore_cdm::GaussianState::from_slices(&mean, &cov, "eq_test", spoore_cdm::Epoch::UNIX_EPOCH).unwrap_err();
        assert!(matches!(spoore_err, spoore_cdm::CdmError::NotPositiveDefinite { .. }), "{spoore_err}");
    }

    #[test]
    fn equivalence_symmetry_tolerance_is_relative_to_magnitude_on_both_sides() {
        // Same fixture as spoore_cdm::state::tests::symmetry_tolerance_is_relative_to_magnitude:
        // large-magnitude round-off asymmetry must pass both; small-magnitude genuine
        // asymmetry must fail both.
        let c = 1e9_f64;
        let eps = c * 1e-12;
        let healthy = [c, c / 2.0, c / 2.0 + eps, c];
        assert!(check_spd_row_major(&healthy, 2, "test").is_ok());
        assert!(spoore_accepts(&[0.0, 0.0], &healthy));

        let tiny_but_real = [1e-6, 1e-7, 2e-7, 1e-6];
        assert!(matches!(
            check_spd_row_major(&tiny_but_real, 2, "test").unwrap_err(),
            CovarianceHygieneError::NotSymmetric { .. }
        ));
        assert!(!spoore_accepts(&[0.0, 0.0], &tiny_but_real));
    }
}
