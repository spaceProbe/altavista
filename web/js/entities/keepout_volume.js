// web/js/entities/keepout_volume.js -- H6 scope item 1's second half: "A keep-out
// volume is the same primitive with a declared radius/margin; check whether anything
// in the tree already declares keep-out semantics before inventing them."
//
// That check was done first, honestly, before writing this file: `grep -rn
// "keep.?out" -i` across the whole tree (excluding node_modules) turns up nothing but
// this task's own brief and docs/heavy-plan.md's H6 entry -- no proto message, no
// Python dataclass, no existing JS module declares any keep-out semantics today. This
// module therefore INVENTS the minimal contract stated below, deliberately as thin as
// possible so it does not foreclose whatever a real conjunction-safety consumer
// eventually needs:
//
//   A keep-out volume is a covariance ellipsoid (`./covariance_ellipsoid.js`'s
//   `covarianceEllipsoid`, never a second, independent ellipsoid computation) whose
//   semi-axes are inflated by a single, EXPLICIT, caller-declared margin -- e.g. a
//   combined hard-body radius (chief + deputy) plus a safety pad. Nothing here invents
//   a numeric default margin: like `sigma` in `covariance_ellipsoid.js`, `marginKm` is
//   a required argument, for the identical reason (an implicit margin is exactly as
//   dangerous a silent guess as an implicit sigma for a safety volume).
import { covarianceEllipsoid, CovarianceShapeError } from './covariance_ellipsoid.js';

function assertMargin(marginKm) {
  if (!(Number.isFinite(marginKm) && marginKm >= 0)) {
    throw new CovarianceShapeError(
      `marginKm must be a finite number >= 0 (the declared hard-body-radius + safety-pad `
      + `this keep-out volume is inflated by) -- got ${marginKm}. There is no default, `
      + `for the same reason covarianceEllipsoid's sigma has none: an unstated margin on `
      + `a safety volume is a silent guess, not a declared one.`,
    );
  }
}

/**
 * A keep-out volume: the `sigma`-scaled covariance ellipsoid (`covarianceEllipsoid`),
 * with `marginKm` added to EACH semi-axis (an isotropic inflation -- the simplest
 * honest choice for a scalar hard-body/safety margin that itself carries no
 * orientation; a caller with an anisotropic margin can inflate `axes` itself using the
 * returned orientation, this function does not foreclose that, it just doesn't invent
 * it unasked).
 * @param {number[]} covFlat row-major n x n covariance (see covariance_ellipsoid.js)
 * @param {number} n 3 or 6
 * @param {{sigma:number, marginKm:number}} opts both REQUIRED, no defaults (see this
 *   module's and covariance_ellipsoid.js's own doc comments for why)
 * @returns the same shape `covarianceEllipsoid` returns, PLUS `marginKm` and
 *   `keepOutSemiAxesKm` (== `semiAxesKm[k] + marginKm` for each k, same order/axes)
 */
export function keepOutVolumeFromCovariance(covFlat, n, { sigma, marginKm } = {}) {
  assertMargin(marginKm);
  const ellipsoid = covarianceEllipsoid(covFlat, n, { sigma });
  const keepOutSemiAxesKm = ellipsoid.semiAxesKm.map((a) => a + marginKm);
  return { ...ellipsoid, marginKm, keepOutSemiAxesKm };
}

/**
 * Point-containment test against a keep-out volume, in the SAME frame the covariance's
 * own axes are expressed in (this module never converts frames -- the caller is
 * responsible for expressing `relPointKm` relative to the ellipsoid's own center, in
 * the frame `axes` was computed in, exactly the discipline `web/js/frames.js` already
 * requires of every other frame-relative quantity in this codebase). Exact algebraic
 * test (no sampling, no mesh needed): a point is inside iff the sum of its squared
 * projections onto each axis, each divided by that axis's own squared inflated length,
 * is <= 1 -- the standard ellipsoid quadratic form, evaluated directly against
 * `keepOutSemiAxesKm`/`axes` rather than by testing against a rendered mesh (which
 * would only ever be an approximation of the true algebraic surface).
 * @param {{axes:number[][], keepOutSemiAxesKm:number[]}} keepOut
 * @param {[number,number,number]} relPointKm
 * @returns {boolean}
 */
export function pointInsideKeepOut(keepOut, relPointKm) {
  let sum = 0;
  for (let k = 0; k < 3; k++) {
    const axis = keepOut.axes[k];
    const proj = relPointKm[0] * axis[0] + relPointKm[1] * axis[1] + relPointKm[2] * axis[2];
    const a = keepOut.keepOutSemiAxesKm[k];
    sum += (proj * proj) / (a * a);
  }
  return sum <= 1;
}
