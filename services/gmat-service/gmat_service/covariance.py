"""Covariance hygiene: a Cholesky-based SPD check, applied *before* gmat-service ever emits a
propagated covariance as ``TrajectorySample.cov`` (``docs/open-questions.md`` question 80).

This is the Python-side mirror of ``av_cdm::covariance`` (``crates/av-cdm/src/covariance.rs``
-- see that module's own doc comment for the full rationale, which applies here unchanged):
the same finite -> symmetric -> Cholesky bar ``spoore_cdm::GaussianState``'s Rust constructor
enforces, applied at the point a covariance is *produced* rather than wherever it might later
cross into a spoore type. ``spoore-cdm`` is a Rust crate with no Python binding, so this module
cannot call into it directly; instead it implements the identical algorithm (same
``SYMMETRY_RTOL`` value and formula, same check order) and is tested against the same fixed
fixtures the Rust side's own equivalence tests use
(``crates/av-cdm/src/covariance.rs::tests::equivalence_*``), so "this module agrees with
spoore's bar" rests on the Rust side's direct proof plus this module reproducing the same
algorithm on the same inputs, not on an independent claim.

A failure is a typed exception and increments :func:`spd_check_failures` -- never a silent
repair. :func:`nearest_spd` is the opt-in projection (off by default); see its own docstring.
"""
from __future__ import annotations

import threading
from dataclasses import dataclass
from typing import List, Sequence

import numpy as np

# Identical value and formula to spoore_cdm::state::SYMMETRY_RTOL (Rust, private to that
# crate) and av_cdm::covariance::SYMMETRY_RTOL (crates/av-cdm/src/covariance.rs) -- three
# independent implementations of the same tolerance across three language/binding pairs.
SYMMETRY_RTOL = 1e-9

# Matches av_cdm::covariance::DEFAULT_NEAREST_SPD_FLOOR_RATIO.
DEFAULT_NEAREST_SPD_FLOOR_RATIO = 1e-9


class CovarianceHygieneError(Exception):
    """Base class for a covariance hygiene check failure. Callers never catch this to repair
    it silently -- see :func:`nearest_spd` for the one declared, opt-in, logged exception."""

    context: str


class DimensionMismatchError(CovarianceHygieneError):
    def __init__(self, context: str, expected: int, actual: int):
        self.context = context
        self.expected = expected
        self.actual = actual
        super().__init__(f"covariance {context}: {actual} elements is not {expected} (expected=n*n for the declared n)")


class NotFiniteError(CovarianceHygieneError):
    def __init__(self, context: str, index: int):
        self.context = context
        self.index = index
        super().__init__(f"covariance {context}[{index}] is not finite")


class NotSymmetricError(CovarianceHygieneError):
    def __init__(self, context: str, i: int, j: int, asymmetry: float):
        self.context = context
        self.i = i
        self.j = j
        self.asymmetry = asymmetry
        super().__init__(f"covariance {context}: |c[{i}][{j}] - c[{j}][{i}]| = {asymmetry:e}, exceeding the relative tolerance")


class NotPositiveDefiniteError(CovarianceHygieneError):
    def __init__(self, context: str):
        self.context = context
        super().__init__(f"covariance {context}: failed a Cholesky attempt (not positive definite)")


@dataclass(frozen=True)
class CholeskyDiagnostics:
    """What :func:`check_spd` read off a successful Cholesky factorization ``P = L L^T``.

    ``min_cholesky_diag_sq`` is ``min_i L[i][i]**2``. See
    ``av_cdm::covariance::CholeskyDiagnostics`` (``crates/av-cdm/src/covariance.rs``) for the
    precise meaning: each Cholesky pivot is a Rayleigh quotient of ``P``, so
    ``lambda_min(P) <= L[i][i]**2 <= lambda_max(P)`` for every ``i`` -- this value is an
    **upper bound** on the true smallest eigenvalue (not a lower one), forced toward zero
    exactly when ``lambda_min(P)`` is. Cheap and always available once the Cholesky factor
    exists; not a substitute for an eigenvalue computation where the exact value matters.
    """

    n: int
    min_cholesky_diag_sq: float


_lock = threading.Lock()
_spd_check_failures = 0
_nearest_spd_projections_applied = 0


def spd_check_failures() -> int:
    """Process-wide count of :func:`check_spd` failures, any exception type -- incremented
    once per failed call. Not reset between calls, mirroring
    ``av_cdm::covariance::spd_check_failures`` (Rust) and ``spoore-math``'s own
    ``regularization_count()`` convention: a caller wanting a windowed rate samples this at
    both ends of the window."""
    with _lock:
        return _spd_check_failures


def nearest_spd_projections_applied() -> int:
    """Process-wide count of :func:`nearest_spd` applications, mirroring
    ``av_cdm::covariance::nearest_spd_projections_applied`` (Rust)."""
    with _lock:
        return _nearest_spd_projections_applied


def _count_failure() -> None:
    global _spd_check_failures
    with _lock:
        _spd_check_failures += 1


def _count_projection() -> None:
    global _nearest_spd_projections_applied
    with _lock:
        _nearest_spd_projections_applied += 1


def check_spd(cov_row_major: Sequence[float], n: int, context: str) -> CholeskyDiagnostics:
    """The Cholesky-based SPD check: finite, then symmetric (within :data:`SYMMETRY_RTOL`,
    relative to magnitude), then a Cholesky attempt -- the exact order and bar
    ``spoore_cdm::GaussianState``'s Rust constructor applies (see module docstring).

    Raises :class:`DimensionMismatchError`, :class:`NotFiniteError`,
    :class:`NotSymmetricError` or :class:`NotPositiveDefiniteError`, in that order of
    preference (the first violation found is the one raised).
    """
    if len(cov_row_major) != n * n:
        _count_failure()
        raise DimensionMismatchError(context, n * n, len(cov_row_major))

    arr = np.asarray(cov_row_major, dtype=np.float64).reshape(n, n)

    flat = arr.reshape(-1)
    for index, v in enumerate(flat):
        if not np.isfinite(v):
            _count_failure()
            raise NotFiniteError(context, index)

    for i in range(n):
        for j in range(i + 1, n):
            a, b = float(arr[i, j]), float(arr[j, i])
            asymmetry = abs(a - b)
            scale = max(abs(a), abs(b), 1.0)
            if asymmetry > SYMMETRY_RTOL * scale:
                _count_failure()
                raise NotSymmetricError(context, i, j, asymmetry)

    try:
        l = np.linalg.cholesky(arr)
    except np.linalg.LinAlgError:
        _count_failure()
        raise NotPositiveDefiniteError(context) from None

    min_diag_sq = float(min(l[i, i] ** 2 for i in range(n)))
    return CholeskyDiagnostics(n=n, min_cholesky_diag_sq=min_diag_sq)


def nearest_spd(cov_row_major: Sequence[float], n: int, floor_ratio: float = DEFAULT_NEAREST_SPD_FLOOR_RATIO) -> List[float]:
    """Opt-in nearest symmetric positive-definite projection -- **off by default, used only if
    a caller explicitly opts in.** Mirrors ``av_cdm::covariance::nearest_spd``
    (``crates/av-cdm/src/covariance.rs``) exactly: see that function's docstring for the named
    algorithm (eigenvalue clipping, specialized from Higham (1988)'s nearest positive
    semidefinite matrix in Frobenius norm to an already-symmetric input, eigenvalues floored
    to ``floor_ratio * (largest eigenvalue)`` rather than exactly zero because Cholesky needs
    strict positive-definiteness) and its limits (not the iterative/non-symmetric form of
    Higham's algorithm; not guaranteed to preserve trace or any other physical quantity; not
    re-validated against any golden). This is the same construction, not a second independent
    design -- both sides were written from the same derivation.

    Counts the application via :func:`nearest_spd_projections_applied`; callers are expected
    to also log the application loudly (see ``model.GmatModel.propagate_covariance``) rather
    than swap in the repaired matrix silently.
    """
    arr = np.asarray(cov_row_major, dtype=np.float64).reshape(n, n)
    sym = 0.5 * (arr + arr.T)
    eigvals, eigvecs = np.linalg.eigh(sym)  # ascending order; real, since sym is symmetric.
    max_eig = float(eigvals[-1])
    scale = max_eig if np.isfinite(max_eig) and max_eig > 0.0 else 1.0
    floor = floor_ratio * scale
    floored = np.maximum(eigvals, floor)
    out = eigvecs @ np.diag(floored) @ eigvecs.T
    out = 0.5 * (out + out.T)
    _count_projection()
    return out.reshape(-1).tolist()
