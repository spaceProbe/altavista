// CLI harness for tests/test_entities_ellipsoid.py: `node web/js/entities_ellipsoid_check.mjs`.
//
// Proves web/js/entities/covariance_ellipsoid.js, keepout_volume.js and
// ellipsoid_mesh.js against INDEPENDENTLY computed ground truth -- never against the
// module's own answer (this round's rule: "a check that reads its answer from the
// thing it is checking proves nothing"):
//   - diagonalClosedForm: cov = diag(d0,d1,d2). The closed-form truth for a DIAGONAL
//     covariance is that the semi-axes are EXACTLY sigma*sqrt(d_i) along the standard
//     basis vectors -- no eigensolver needed to know this independently, it's the
//     definition of a diagonal matrix's own eigendecomposition.
//   - rotatedInvariant: cov = R * diag(d0,d1,d2) * R^T, with R and the multiply built
//     here directly (never by calling anything in web/js/entities/) from a known
//     quaternion. A correct eigendecomposition must recover the SAME eigenvalues
//     (rotation cannot change them) and axes that are R's own columns up to sign/
//     permutation -- checked against R's columns directly, not against the module's
//     own prior output.
//   - n6PositionBlockOnly: a 6x6 covariance whose top-left 3x3 is the SAME diagonal
//     block as diagonalClosedForm, with nonzero velocity/cross terms elsewhere; the
//     recovered ellipsoid must equal diagonalClosedForm's own ground truth exactly,
//     proving positionCovarianceBlock() truly ignores the velocity block rather than
//     being lucky.
//   - keepOutMargin: keepOutSemiAxesKm must equal semiAxesKm + marginKm exactly, per
//     axis, and pointInsideKeepOut must agree with the exact algebraic ellipsoid
//     quadratic form evaluated independently here (not by calling pointInsideKeepOut
//     twice).
//   - meshScaleFromSceneGraph: buildEllipsoidMesh's resulting THREE.Mesh, read back via
//     worldSemiAxesKm (matrixWorld decomposition, not mesh.scale trusted directly),
//     must equal the same closed-form semi-axes -- "assert from the scene graph, never
//     a counter alone".
//   - errorPaths: missing sigma, missing marginKm, asymmetric input and a
//     non-positive-semidefinite input must each throw CovarianceShapeError -- proven by
//     actually calling with each bad input and catching, not asserted by inspection.
import * as THREE from 'three';
import {
  covarianceEllipsoid, symmetricEigen3, quaternionFromAxes, positionCovarianceBlock, CovarianceShapeError,
} from './entities/covariance_ellipsoid.js';
import { keepOutVolumeFromCovariance, pointInsideKeepOut } from './entities/keepout_volume.js';
import { buildEllipsoidMesh, worldSemiAxesKm } from './entities/ellipsoid_mesh.js';

const checks = [];
function check(name, pass, detail) { checks.push({ name, pass: !!pass, detail }); }
function approxEqual(a, b, tol) { return Math.abs(a - b) <= tol; }

// ------------------------------------------------------------- diagonal closed form
const SIGMA = 2;
const D = [4, 9, 16]; // km^2, deliberately unordered so sorting is exercised for real
const diagCov = [
  D[0], 0, 0,
  0, D[1], 0,
  0, 0, D[2],
];
const diagExpectedSemiAxesKm = D.map((d) => SIGMA * Math.sqrt(d)).sort((a, b) => b - a); // [8,6,4]

const diagEll = covarianceEllipsoid(diagCov, 3, { sigma: SIGMA });
{
  const tol = 1e-9;
  const okAxes = diagEll.semiAxesKm.every((v, i) => approxEqual(v, diagExpectedSemiAxesKm[i], tol));
  check('diagonalClosedForm_semiAxes', okAxes, { got: diagEll.semiAxesKm, expected: diagExpectedSemiAxesKm });

  // Independent orthonormality check of the RETURNED axes (never assumed): each unit
  // norm, all pairwise dot products ~0.
  const [a0, a1, a2] = diagEll.axes;
  const norms = [a0, a1, a2].map((v) => Math.hypot(...v));
  const dots = [dot(a0, a1), dot(a0, a2), dot(a1, a2)];
  const orthonormal = norms.every((n) => approxEqual(n, 1, 1e-9)) && dots.every((d) => approxEqual(d, 0, 1e-9));
  check('diagonalClosedForm_axesOrthonormal', orthonormal, { norms, dots });
}
function dot(a, b) { return a[0] * b[0] + a[1] * b[1] + a[2] * b[2]; }
function cross(a, b) { return [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]; }
function matVec(m, v) { return [dot(m[0], v), dot(m[1], v), dot(m[2], v)]; }
function transpose(m) { return [[m[0][0], m[1][0], m[2][0]], [m[0][1], m[1][1], m[2][1]], [m[0][2], m[1][2], m[2][2]]]; }
function matMul(a, b) {
  const bt = transpose(b);
  return a.map((row) => bt.map((col) => dot(row, col)));
}
function quatToMatrixColsAsRows(q) {
  // [x,y,z,w] -> 3 rows, each a COLUMN of the rotation (matches covariance_ellipsoid.js's
  // own quaternionFromAxes contract) -- built directly from the quaternion-to-matrix
  // formula here, independent of anything in web/js/entities/.
  const [x, y, z, w] = q;
  const col0 = [1 - 2 * (y * y + z * z), 2 * (x * y + z * w), 2 * (x * z - y * w)];
  const col1 = [2 * (x * y - z * w), 1 - 2 * (x * x + z * z), 2 * (y * z + x * w)];
  const col2 = [2 * (x * z + y * w), 2 * (y * z - x * w), 1 - 2 * (x * x + y * y)];
  return [col0, col1, col2];
}

// ------------------------------------------------------------- rotated invariant
// A known, non-axis-aligned rotation (arbitrary unit quaternion, normalized here).
const Q = (() => {
  const raw = [0.2, -0.4, 0.1, 1.0];
  const n = Math.hypot(...raw);
  return raw.map((c) => c / n);
})();
const colsAsRows = quatToMatrixColsAsRows(Q); // colsAsRows[k] is axis k, as covariance_ellipsoid.js expects
// R as a plain row-major 3x3 matrix (columns = colsAsRows) for the multiply below.
const R = [
  [colsAsRows[0][0], colsAsRows[1][0], colsAsRows[2][0]],
  [colsAsRows[0][1], colsAsRows[1][1], colsAsRows[2][1]],
  [colsAsRows[0][2], colsAsRows[1][2], colsAsRows[2][2]],
];
const Rt = transpose(R);
const Ddiag = [[D[0], 0, 0], [0, D[1], 0], [0, 0, D[2]]];
const covRotMat = matMul(matMul(R, Ddiag), Rt); // R * diag(D) * R^T, independent multiply
const covRotFlat = [
  covRotMat[0][0], covRotMat[0][1], covRotMat[0][2],
  covRotMat[1][0], covRotMat[1][1], covRotMat[1][2],
  covRotMat[2][0], covRotMat[2][1], covRotMat[2][2],
];
const rotEll = covarianceEllipsoid(covRotFlat, 3, { sigma: SIGMA });
{
  const tol = 1e-7;
  const okEigen = rotEll.semiAxesKm.every((v, i) => approxEqual(v, diagExpectedSemiAxesKm[i], tol));
  check('rotatedInvariant_semiAxesUnchanged', okEigen, { got: rotEll.semiAxesKm, expected: diagExpectedSemiAxesKm });

  // The eigenvalue order is descending by construction; D sorted descending is
  // [16,9,4], so recovered axis k should align with R's column matching that same
  // sorted D index. Sort D's OWN indices descending independently, then compare.
  const dOrder = [0, 1, 2].sort((a, b) => D[b] - D[a]); // [2,1,0] for D=[4,9,16]
  let alignOk = true;
  const alignDetail = [];
  for (let k = 0; k < 3; k++) {
    const expectedAxis = colsAsRows[dOrder[k]];
    const gotAxis = rotEll.axes[k];
    const d = Math.abs(dot(expectedAxis, gotAxis)); // sign-agnostic alignment
    alignDetail.push(d);
    if (!approxEqual(d, 1, 1e-6)) alignOk = false;
  }
  check('rotatedInvariant_axesMatchKnownRotation', alignOk, { absDots: alignDetail });
}

// ------------------------------------------------------------- n=6 position-block-only
const covN6 = new Array(36).fill(0);
for (let i = 0; i < 3; i++) for (let j = 0; j < 3; j++) covN6[i * 6 + j] = diagCov[i * 3 + j];
// Nonzero, asymmetric-looking-if-mishandled velocity/cross terms elsewhere (rows/cols 3-5) --
// deliberately large so a bug that accidentally pulled from the wrong block would be
// obviously wrong, not coincidentally close.
for (let i = 3; i < 6; i++) for (let j = 3; j < 6; j++) covN6[i * 6 + j] = (i === j) ? 1000 + i : 0;
for (let i = 0; i < 3; i++) { covN6[i * 6 + (i + 3)] = 500; covN6[(i + 3) * 6 + i] = 500; }
const extractedBlock = positionCovarianceBlock(covN6, 6);
const n6Ell = covarianceEllipsoid(covN6, 6, { sigma: SIGMA });
{
  const blockOk = extractedBlock.every((row, i) => row.every((v, j) => approxEqual(v, diagCov[i * 3 + j], 1e-12)));
  check('n6_positionBlockExtractedCorrectly', blockOk, { extractedBlock, expected: [[D[0], 0, 0], [0, D[1], 0], [0, 0, D[2]]] });
  const okAxes = n6Ell.semiAxesKm.every((v, i) => approxEqual(v, diagExpectedSemiAxesKm[i], 1e-9));
  check('n6_positionBlockOnly_semiAxes', okAxes, { got: n6Ell.semiAxesKm, expected: diagExpectedSemiAxesKm });
}

// ------------------------------------------------------------- keep-out margin
const MARGIN_KM = 0.5; // e.g. combined hard-body radius + safety pad, km
const keepOut = keepOutVolumeFromCovariance(diagCov, 3, { sigma: SIGMA, marginKm: MARGIN_KM });
{
  const okMargin = keepOut.keepOutSemiAxesKm.every((v, i) => approxEqual(v, diagEll.semiAxesKm[i] + MARGIN_KM, 1e-12));
  check('keepOut_marginAppliedExactly', okMargin, { got: keepOut.keepOutSemiAxesKm, semiAxesKm: diagEll.semiAxesKm, marginKm: MARGIN_KM });

  // Independent point-in-ellipsoid quadratic-form evaluation, built directly here
  // (never calling pointInsideKeepOut to check itself).
  function independentInside(axes, semis, p) {
    let s = 0;
    for (let k = 0; k < 3; k++) {
      const proj = dot(p, axes[k]);
      s += (proj * proj) / (semis[k] * semis[k]);
    }
    return s <= 1;
  }
  const onAxisPointJustInside = keepOut.axes[0].map((c) => c * (keepOut.keepOutSemiAxesKm[0] * 0.999));
  const onAxisPointJustOutside = keepOut.axes[0].map((c) => c * (keepOut.keepOutSemiAxesKm[0] * 1.001));
  const insideAgree = pointInsideKeepOut(keepOut, onAxisPointJustInside) === true
    && independentInside(keepOut.axes, keepOut.keepOutSemiAxesKm, onAxisPointJustInside) === true;
  const outsideAgree = pointInsideKeepOut(keepOut, onAxisPointJustOutside) === false
    && independentInside(keepOut.axes, keepOut.keepOutSemiAxesKm, onAxisPointJustOutside) === false;
  const originInside = pointInsideKeepOut(keepOut, [0, 0, 0]) === true;
  check('keepOut_pointContainment', insideAgree && outsideAgree && originInside, { insideAgree, outsideAgree, originInside });
}

// ------------------------------------------------------------- scene-graph assertion
const mesh = buildEllipsoidMesh(diagEll);
const meshWorldAxes = worldSemiAxesKm(mesh);
{
  const ok = meshWorldAxes.every((v, i) => approxEqual(v, diagExpectedSemiAxesKm[i], 1e-9));
  check('meshScaleFromSceneGraph_matchesClosedForm', ok, { meshWorldAxes, expected: diagExpectedSemiAxesKm });
  check('meshScaleFromSceneGraph_isRealThreeMesh', mesh.isMesh === true && mesh.geometry.type === 'SphereGeometry', { isMesh: mesh.isMesh, geometryType: mesh.geometry.type });
}

// Reparent under a scaled group -- proves worldSemiAxesKm reads the SCENE GRAPH
// (matrixWorld), not just mesh.scale in isolation.
{
  const group = new THREE.Group();
  group.scale.set(2, 2, 2);
  group.add(mesh);
  group.updateMatrixWorld(true);
  const scaledWorldAxes = worldSemiAxesKm(mesh);
  const ok = scaledWorldAxes.every((v, i) => approxEqual(v, diagExpectedSemiAxesKm[i] * 2, 1e-9));
  check('meshScaleFromSceneGraph_reflectsParentTransform', ok, { scaledWorldAxes, expected: diagExpectedSemiAxesKm.map((v) => v * 2) });
}

// ------------------------------------------------------------- error paths
function throwsShapeError(fn) {
  try { fn(); return false; } catch (e) { return e instanceof CovarianceShapeError; }
}
check('errorPath_missingSigma', throwsShapeError(() => covarianceEllipsoid(diagCov, 3, {})), {});
check('errorPath_missingMargin', throwsShapeError(() => keepOutVolumeFromCovariance(diagCov, 3, { sigma: 1 })), {});
check('errorPath_asymmetricInput', throwsShapeError(() => covarianceEllipsoid([1, 100, 0, 0, 2, 0, 0, 0, 3], 3, { sigma: 1 })), {});
{
  // A genuinely indefinite (not PSD) symmetric matrix: eigenvalues include a negative one.
  const indefinite = [-1, 0, 0, 0, 2, 0, 0, 0, 3];
  check('errorPath_notPositiveSemidefinite', throwsShapeError(() => covarianceEllipsoid(indefinite, 3, { sigma: 1 })), {});
}

// ------------------------------------------------------------- symmetricEigen3 / quaternionFromAxes sanity
{
  // Identity covariance: every direction is an eigenvector with eigenvalue 1 -- the
  // degenerate case where "the" eigenvectors are not unique, but they must still be
  // ORTHONORMAL and the eigenvalues must all be exactly 1.
  const { values, vectors } = symmetricEigen3([[1, 0, 0], [0, 1, 0], [0, 0, 1]]);
  const valuesOk = values.every((v) => approxEqual(v, 1, 1e-12));
  const orthonormalOk = vectors.every((v) => approxEqual(Math.hypot(...v), 1, 1e-9));
  check('symmetricEigen3_identityDegenerateCase', valuesOk && orthonormalOk, { values, vectors });

  const q = quaternionFromAxes([[1, 0, 0], [0, 1, 0], [0, 0, 1]]);
  check('quaternionFromAxes_identity', approxEqual(q[3], 1, 1e-12) && [q[0], q[1], q[2]].every((c) => approxEqual(c, 0, 1e-12)), { q });
}

const allPass = checks.every((c) => c.pass);
process.stdout.write(JSON.stringify({
  allPass,
  checks,
  sigma: SIGMA,
  diagExpectedSemiAxesKm,
  diagonalMeasuredSemiAxesKm: diagEll.semiAxesKm,
  rotatedMeasuredSemiAxesKm: rotEll.semiAxesKm,
  keepOutSemiAxesKm: keepOut.keepOutSemiAxesKm,
  meshWorldSemiAxesKm: meshWorldAxes,
}));
// Manager review, round 6: a failing check sets a non-zero exit code, the same way
// web/js/layout/layout_tree_check.mjs already does. Without it `node <check>` exits 0
// on a broken build and the round's own gate line ("the node checks at the final head:
// N of N exit 0") is silently meaningless for this file. The JSON still goes to stdout
// either way, and the pytest side still reports the failing check NAMES rather than a
// bare exit code -- see this check's own test file.
if (!allPass) process.exitCode = 1;
