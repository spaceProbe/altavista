// web/js/entities/covariance_ellipsoid.js -- H6 scope item 1 (docs/heavy-plan.md):
// covariance ellipsoids and keep-out volumes "from the covariance the kernel already
// carries".
//
// What was actually established before writing this module (this task's own honesty
// requirement -- "establish what shape actually reaches the browser before designing
// anything"), by reading web/js/panels/run_products_panel.js, web/js/interp.js,
// altavista/server.py, altavista/cdm.py and altavista/model.py end to end:
//
//   - The wire protocol DOES carry a spacecraft-state covariance: `TrajectorySample.cov`
//     (proto/altavista/v1/trajectory.proto) is documented "Row-major n x n covariance,
//     SPD; empty when the DRM did not request covariance", present only at NATIVE
//     samples (web/js/interp.js's `classifyStateSpace` explicitly refuses to
//     interpolate a covariance/STM component -- ADR-005 sec 3 -- so a consumer must
//     treat it as a per-native-sample-only quantity, never Hermite/slerp-blended).
//   - `altavista/cdm.py`'s own module docstring, "The covariance placeholder" section,
//     states plainly: "`trajectory_to_cdm` never fills `TrajectorySample.cov`... this
//     module has no DRM to consult". `altavista/model.py`'s `Trajectory` dataclass --
//     the ONE dataclass whose `.to_dict()` actually feeds the browser's `ScenarioData`
//     JSON (`web/js/scene.js`'s `_buildFrameGraph`/`setScenario` path) -- has no `cov`
//     field at all: only `t`/`pos`/`vel`/`attitude`. `web/js/panels/run_products_panel.js`
//     only ever sees `measurements[].r`, the per-OBSERVATION sensor noise covariance
//     (docs/open-questions.md question 174), which is a different quantity from a
//     spacecraft's own state-estimate covariance and is not drawn as a 3D volume at
//     all in that panel.
//
//   Conclusion, stated plainly rather than glossed over: **no producer in this
//   codebase currently plumbs a spacecraft position covariance into the browser's
//   scenario JSON.** The wire shape exists (row-major n x n, SPD, native-samples-only)
//   and this module consumes EXACTLY that shape -- so the moment a producer starts
//   setting `TrajectorySample.cov` (or a future `Trajectory.cov` on the viewer-facing
//   dataclass, symmetric with `Trajectory.attitude`'s own "P2, schema groundwork only"
//   precedent), this module needs no change. Wiring that production end-to-end is
//   backend/model.py/scenario.py work outside `web/js/entities/`'s own scope and this
//   round's file-ownership boundaries; see this task's report for exactly what is
//   proven here instead (synthetic covariance matrices with closed-form or
//   independently-constructed ground truth, per this round's rules on what a proof
//   must do).
//
// ----------------------------------------------------------------- the sigma convention
// A covariance matrix alone does not define an ellipsoid: the semi-axis lengths are
// `sigma * sqrt(eigenvalue)` for a CHOSEN sigma (68.3% of probability mass inside the
// ellipsoid at 1-sigma in 1-D per axis; the enclosed-probability-mass fraction for a
// 3-D Gaussian is a different, chi-squared-derived number entirely, e.g. ~19.9% at
// 1-sigma-per-axis, ~97.1% at 3-sigma-per-axis -- MOMENTS, not probability mass, are
// what this module scales). Drawing a 1-sigma covariance as a 3-sigma ellipsoid (or
// vice versa) silently changes what the picture claims by roughly 9x in linear size --
// for a keep-out/collision-safety picture that is not a cosmetic bug, it is a
// mission-safety lie (this task's brief, verbatim). The fix this module makes
// structural, not documentary: `sigma` is a REQUIRED, explicit argument to every
// function in this file that turns a covariance into a size -- there is no default,
// so a call site that forgets to state its sigma throws (`CovarianceShapeError`)
// instead of silently drawing something with an implicit, unstated scale. The chosen
// sigma is echoed back on every returned object (`.sigma`) so a renderer/label/legend
// downstream can always state, in the picture itself, what confidence bound is drawn.

/** Thrown for any covariance input this module cannot honestly turn into an ellipsoid
 * (wrong shape, non-finite entries, not symmetric within tolerance, a missing/invalid
 * `sigma`). Never silently coerced -- an ellipsoid drawn from a coerced/guessed input
 * is exactly the kind of silent mission-safety lie the sigma convention above refuses
 * to allow one level up; this is the same discipline applied to shape validation. */
export class CovarianceShapeError extends Error {
  constructor(message) {
    super(message);
    this.name = 'CovarianceShapeError';
  }
}

function assertSigma(sigma) {
  if (!(Number.isFinite(sigma) && sigma > 0)) {
    throw new CovarianceShapeError(
      `sigma must be a positive finite number (the confidence-bound multiplier this `
      + `ellipsoid is drawn at) -- got ${sigma}. There is no default: a caller must `
      + `state its sigma convention explicitly, never rely on an implicit one.`,
    );
  }
}

/** Extract the 3x3 POSITION block from a row-major, flattened n x n covariance array
 * -- exactly `TrajectorySample.cov`'s own documented wire shape (proto/altavista/v1/
 * trajectory.proto: "Row-major n x n covariance, SPD"). `n === 3`: the whole matrix is
 * the position block. `n === 6` (the codebase's own `STATE_SPACE_ID_CARTESIAN_POS_VEL_6`
 * convention, altavista/model.py): the top-left 3x3 (rows/cols 0-2, position; 3-5 would
 * be velocity, never used for a spatial ellipsoid). Any other `n`, or an array whose
 * length is not exactly `n*n`, is refused rather than guessed at.
 * @param {number[]} covFlat row-major n x n, length n*n
 * @param {number} n
 * @returns {number[][]} 3x3 nested array
 */
export function positionCovarianceBlock(covFlat, n) {
  if (!Array.isArray(covFlat) && !(covFlat instanceof Float64Array) && !(covFlat instanceof Float32Array)) {
    throw new CovarianceShapeError('covFlat must be an array-like of numbers');
  }
  if (n !== 3 && n !== 6) {
    throw new CovarianceShapeError(`unsupported covariance dimension n=${n}; this module only knows how to find a position block inside n=3 (pure position) or n=6 (position+velocity, altavista's STATE_SPACE_ID_CARTESIAN_POS_VEL_6 convention) -- got n=${n}`);
  }
  if (covFlat.length !== n * n) {
    throw new CovarianceShapeError(`covFlat.length must be n*n=${n * n} (row-major n x n) -- got length ${covFlat.length} for n=${n}`);
  }
  const block = [[0, 0, 0], [0, 0, 0], [0, 0, 0]];
  for (let i = 0; i < 3; i++) {
    for (let j = 0; j < 3; j++) {
      const v = covFlat[i * n + j];
      if (!Number.isFinite(v)) {
        throw new CovarianceShapeError(`covFlat[${i * n + j}] is not finite (${v}) -- a covariance with a NaN/Infinity entry cannot be turned into an ellipsoid`);
      }
      block[i][j] = v;
    }
  }
  return block;
}

const SYMMETRY_TOL = 1e-9; // relative tolerance against the matrix's own largest entry

/** Cyclic Jacobi eigenvalue algorithm for a real SYMMETRIC 3x3 matrix. Chosen over a
 * general (non-symmetric) eigensolver because a covariance matrix is symmetric by
 * construction (SPD, per the wire shape's own doc comment) -- Jacobi is the standard,
 * numerically stable, non-iterative-tolerance-fragile choice for a small symmetric
 * matrix (converges quadratically, needs no shifting/deflation machinery a general QR
 * algorithm would). No external dependency: this is the one piece of eigendecomposition
 * arithmetic `covariance_ellipsoid.js` owns, deliberately NOT duplicated anywhere else
 * in `web/js/entities/` (`keepout_volume.js` calls this file's own
 * `covarianceEllipsoid`, never a second copy).
 * @param {number[][]} a 3x3 nested array, symmetric (this function reads `a[i][j]` for
 *   `i<=j` only and mirrors it -- an asymmetric input is silently symmetrized by that
 *   read pattern, but `covarianceEllipsoid` below rejects real asymmetry first, per its
 *   own doc comment, so this function is never actually handed a genuinely asymmetric
 *   matrix in this module's own call path)
 * @returns {{values:number[], vectors:number[][]}} `values[k]` is the k-th eigenvalue;
 *   `vectors[k]` is its unit-norm eigenvector `[x,y,z]`. Not sorted -- the caller sorts.
 */
export function symmetricEigen3(a) {
  // Working copy, symmetrized defensively (see doc comment).
  const m = [
    [a[0][0], 0.5 * (a[0][1] + a[1][0]), 0.5 * (a[0][2] + a[2][0])],
    [0.5 * (a[1][0] + a[0][1]), a[1][1], 0.5 * (a[1][2] + a[2][1])],
    [0.5 * (a[2][0] + a[0][2]), 0.5 * (a[2][1] + a[1][2]), a[2][2]],
  ];
  // v accumulates the rotation product -- its columns converge to the eigenvectors.
  const v = [[1, 0, 0], [0, 1, 0], [0, 0, 1]];

  function offDiagSumSq() {
    return m[0][1] * m[0][1] + m[0][2] * m[0][2] + m[1][2] * m[1][2];
  }

  const MAX_SWEEPS = 100;
  for (let sweep = 0; sweep < MAX_SWEEPS; sweep++) {
    if (offDiagSumSq() < 1e-30) break;
    for (const [p, q] of [[0, 1], [0, 2], [1, 2]]) {
      if (Math.abs(m[p][q]) < 1e-300) continue;
      const theta = (m[q][q] - m[p][p]) / (2 * m[p][q]);
      const t = Math.sign(theta || 1) / (Math.abs(theta) + Math.sqrt(theta * theta + 1));
      const c = 1 / Math.sqrt(t * t + 1);
      const s = t * c;
      const mpp = m[p][p], mqq = m[q][q], mpq = m[p][q];
      m[p][p] = mpp - t * mpq;
      m[q][q] = mqq + t * mpq;
      m[p][q] = 0; m[q][p] = 0;
      for (let i = 0; i < 3; i++) {
        if (i !== p && i !== q) {
          const mip = m[i][p], miq = m[i][q];
          m[i][p] = c * mip - s * miq; m[p][i] = m[i][p];
          m[i][q] = s * mip + c * miq; m[q][i] = m[i][q];
        }
        const vip = v[i][p], viq = v[i][q];
        v[i][p] = c * vip - s * viq;
        v[i][q] = s * vip + c * viq;
      }
    }
  }
  const values = [m[0][0], m[1][1], m[2][2]];
  const vectors = [
    [v[0][0], v[1][0], v[2][0]],
    [v[0][1], v[1][1], v[2][1]],
    [v[0][2], v[1][2], v[2][2]],
  ];
  return { values, vectors };
}

function normalize3(v) {
  const n = Math.hypot(v[0], v[1], v[2]) || 1;
  return [v[0] / n, v[1] / n, v[2] / n];
}
function cross3(a, b) {
  return [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
}
function dot3(a, b) { return a[0] * b[0] + a[1] * b[1] + a[2] * b[2]; }

/** Row-major 3x3 rotation matrix (columns are the axes, matching THREE.Matrix4's own
 * column convention when embedded) -> a scalar-last quaternion [x,y,z,w] (Shepperd's
 * method, numerically stable for every rotation including 180-degree ones, the same
 * concern `web/js/frames.js` already documents for its own axes-to-quaternion
 * conversions). Kept local and dependency-free -- `web/js/entities/` doesn't reuse
 * THREE.Matrix4.decompose here so `symmetricEigen3`'s own output (plain arrays) stays
 * directly, independently testable without constructing a THREE object first.
 * @param {number[][]} colsAsRows 3 rows, each an orthonormal COLUMN of the rotation
 *   (i.e. colsAsRows[k] is the k-th axis) -- this module's own `covarianceEllipsoid`
 *   builds its rotation this way, see that function's own comment.
 */
export function quaternionFromAxes(colsAsRows) {
  const [ex, ey, ez] = colsAsRows;
  // Row-major 3x3, m[row][col]; column k is axis k.
  const m = [
    [ex[0], ey[0], ez[0]],
    [ex[1], ey[1], ez[1]],
    [ex[2], ey[2], ez[2]],
  ];
  const trace = m[0][0] + m[1][1] + m[2][2];
  let x, y, z, w;
  if (trace > 0) {
    const s = 0.5 / Math.sqrt(trace + 1);
    w = 0.25 / s;
    x = (m[2][1] - m[1][2]) * s;
    y = (m[0][2] - m[2][0]) * s;
    z = (m[1][0] - m[0][1]) * s;
  } else if (m[0][0] > m[1][1] && m[0][0] > m[2][2]) {
    const s = 2 * Math.sqrt(1 + m[0][0] - m[1][1] - m[2][2]);
    w = (m[2][1] - m[1][2]) / s;
    x = 0.25 * s;
    y = (m[0][1] + m[1][0]) / s;
    z = (m[0][2] + m[2][0]) / s;
  } else if (m[1][1] > m[2][2]) {
    const s = 2 * Math.sqrt(1 + m[1][1] - m[0][0] - m[2][2]);
    w = (m[0][2] - m[2][0]) / s;
    x = (m[0][1] + m[1][0]) / s;
    y = 0.25 * s;
    z = (m[1][2] + m[2][1]) / s;
  } else {
    const s = 2 * Math.sqrt(1 + m[2][2] - m[0][0] - m[1][1]);
    w = (m[1][0] - m[0][1]) / s;
    x = (m[0][2] + m[2][0]) / s;
    y = (m[1][2] + m[2][1]) / s;
    z = 0.25 * s;
  }
  return normalizeQuat([x, y, z, w]);
}
function normalizeQuat(q) {
  const n = Math.hypot(q[0], q[1], q[2], q[3]) || 1;
  return [q[0] / n, q[1] / n, q[2] / n, q[3] / n];
}

/**
 * Turn a covariance (row-major n x n, `positionCovarianceBlock`'s own accepted shapes)
 * into a 1-sigma-scaled ellipsoid: semi-axis lengths are `sigma * sqrt(eigenvalue)`,
 * oriented by the eigenvectors, in the SAME length unit as the covariance's own
 * position entries (this codebase's convention throughout -- `altavista/model.py`'s
 * `Trajectory.pos` -- is km; this module never assumes a unit, it only preserves
 * whatever unit the caller's covariance was already in, hence `semiAxesKm` names the
 * unit explicitly rather than leaving it to be assumed).
 *
 * @param {number[]} covFlat row-major n x n covariance
 * @param {number} n 3 or 6 (see `positionCovarianceBlock`)
 * @param {{sigma:number}} opts REQUIRED `sigma` -- see this module's own doc comment,
 *   "the sigma convention", for why there is no default.
 * @returns {{
 *   sigma:number, n:number,
 *   semiAxesKm:[number,number,number],      // descending order
 *   eigenvaluesKm2:[number,number,number],  // same order as semiAxesKm
 *   axes:[number[],number[],number[]],      // unit eigenvectors, same order, right-handed
 *   quaternion:[number,number,number,number], // [x,y,z,w], rotation taking the unit
 *                                               // sphere's local axes onto `axes`
 * }}
 */
export function covarianceEllipsoid(covFlat, n, { sigma } = {}) {
  assertSigma(sigma);
  const block = positionCovarianceBlock(covFlat, n);

  // Reject genuine asymmetry (beyond float round-trip noise) rather than silently
  // symmetrizing a covariance that was never actually symmetric -- a caller passing a
  // non-covariance array by mistake should learn that here, not get a plausible-looking
  // ellipsoid back.
  const maxAbs = Math.max(1e-300, ...block.flat().map(Math.abs));
  for (let i = 0; i < 3; i++) {
    for (let j = i + 1; j < 3; j++) {
      if (Math.abs(block[i][j] - block[j][i]) > SYMMETRY_TOL * maxAbs) {
        throw new CovarianceShapeError(
          `position covariance block is not symmetric at (${i},${j}): ${block[i][j]} vs (${j},${i}): ${block[j][i]} -- this is not a valid covariance matrix`,
        );
      }
    }
  }

  const { values, vectors } = symmetricEigen3(block);
  // Sort descending by eigenvalue -- deterministic order, matches this project's own
  // "total, deterministic order" discipline (web/js/layers/layer.js's comparators).
  const order = [0, 1, 2].sort((a, b) => values[b] - values[a]);
  const sortedValues = order.map((k) => values[k]);
  let sortedVectors = order.map((k) => normalize3(vectors[k]));

  for (const lam of sortedValues) {
    if (lam < -SYMMETRY_TOL * maxAbs) {
      throw new CovarianceShapeError(
        `covariance is not positive-semidefinite: eigenvalue ${lam} < 0 -- refusing to draw an ellipsoid with an imaginary axis`,
      );
    }
  }

  // Force a right-handed axis triad (det > 0): symmetricEigen3's accumulated rotation
  // can legitimately come out improper (a reflection) depending on the sweep's own
  // arbitrary sign choices -- an ellipsoid's SHAPE is unaffected by flipping one axis's
  // sign (axis k and -axis k describe the identical ellipsoid), but a later consumer
  // that turns `axes` into a rotation matrix/quaternion (this function's own
  // `quaternion` field) needs a proper rotation, not a reflection, or composing it with
  // any other transform silently mirrors everything downstream.
  const det = dot3(sortedVectors[0], cross3(sortedVectors[1], sortedVectors[2]));
  if (det < 0) sortedVectors = [sortedVectors[0], sortedVectors[1], sortedVectors[2].map((c) => -c)];

  const semiAxesKm = sortedValues.map((lam) => sigma * Math.sqrt(Math.max(0, lam)));
  const quaternion = quaternionFromAxes(sortedVectors);

  return {
    sigma,
    n,
    semiAxesKm,
    eigenvaluesKm2: sortedValues,
    axes: sortedVectors,
    quaternion,
  };
}
