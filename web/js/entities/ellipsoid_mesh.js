// web/js/entities/ellipsoid_mesh.js -- turns an ellipsoid/keep-out volume (the plain
// data `covariance_ellipsoid.js`/`keepout_volume.js` return) into a real THREE.Mesh, so
// a headless check can assert "from the scene graph, never a counter alone" (this
// round's rule): `mesh.scale`/`mesh.quaternion` are read back and checked against the
// independently-known ellipsoid, not merely "a mesh was built" (see
// `web/js/entities_ellipsoid_check.mjs`). Deliberately a separate file from
// `covariance_ellipsoid.js`: the math there stays THREE-free and independently
// testable (its own doc comment), this file is the thin THREE-aware adapter over it.
import * as THREE from 'three';

// A unit sphere (radius 1) scaled per-axis by the ellipsoid's own semi-axes and
// rotated by its own quaternion reproduces the ellipsoid exactly (an affine image of
// the unit sphere), the standard way to draw one without a bespoke ellipsoid geometry
// -- `web/js/globe_lod.js`/`globe.js` already draw the (oblate-spheroid) Earth the same
// way (a sphere non-uniformly scaled by flattening), so this is not a new convention
// for this codebase, only the same one applied to a general triaxial ellipsoid.
const WIDTH_SEGMENTS = 24;
const HEIGHT_SEGMENTS = 16;
const _unitSphereGeometry = new THREE.SphereGeometry(1, WIDTH_SEGMENTS, HEIGHT_SEGMENTS);

/**
 * @param {{semiAxesKm:number[]|number[], quaternion:[number,number,number,number]}} ellipsoid
 *   Accepts either a `covarianceEllipsoid()` result (`semiAxesKm`) or a
 *   `keepOutVolumeFromCovariance()` result (`keepOutSemiAxesKm` preferred when present,
 *   so a caller can pass the SAME object either function returned without picking a
 *   field name itself).
 * @param {{color?:number, opacity?:number, wireframe?:boolean}} [opts]
 * @returns {THREE.Mesh} `userData.ellipsoid` holds the source data this mesh was built
 *   from, so a check can cross-reference the drawn mesh back to its own input without
 *   re-deriving it.
 */
export function buildEllipsoidMesh(ellipsoid, opts = {}) {
  const semiAxesKm = ellipsoid.keepOutSemiAxesKm || ellipsoid.semiAxesKm;
  const { color = 0xff9f43, opacity = 0.25, wireframe = false } = opts;
  const material = new THREE.MeshBasicMaterial({
    color, transparent: true, opacity, wireframe, depthWrite: false,
  });
  const mesh = new THREE.Mesh(_unitSphereGeometry, material);
  mesh.scale.set(semiAxesKm[0], semiAxesKm[1], semiAxesKm[2]);
  mesh.quaternion.set(...ellipsoid.quaternion);
  mesh.userData.ellipsoid = ellipsoid;
  mesh.userData.kind = ellipsoid.keepOutSemiAxesKm ? 'keepout-volume' : 'covariance-ellipsoid';
  return mesh;
}

/** Reads the WORLD-SPACE semi-axis lengths a built mesh actually carries, by
 * decomposing its world matrix -- the scene-graph-truth counterpart to trusting
 * `mesh.scale` directly (a parent transform, if any, could otherwise change the
 * apparent size without `mesh.scale` itself changing). `mesh.updateWorldMatrix(true,
 * false)` is called first so this is correct even if a caller just reparented/moved an
 * ancestor and has not rendered a frame yet.
 * @param {THREE.Mesh} mesh
 * @returns {[number,number,number]}
 */
export function worldSemiAxesKm(mesh) {
  mesh.updateWorldMatrix(true, false);
  const scale = new THREE.Vector3();
  const pos = new THREE.Vector3();
  const quat = new THREE.Quaternion();
  mesh.matrixWorld.decompose(pos, quat, scale);
  return [scale.x, scale.y, scale.z];
}
