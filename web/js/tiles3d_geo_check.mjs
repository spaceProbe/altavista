// CLI harness for tests/test_viewer_globe.py: `node web/js/tiles3d_geo_check.mjs`.
//
// M16.4: closes M15.4's first disclosed shortcut ("3D Tiles overlay placement is a
// fixed demo transform, not real geo-referencing against the frame graph"). Two
// things are checked here, both against the *real* modules, never a description of
// them or a Python port:
//
//   1. The fixture's `root.transform` (web/fixtures/3dtiles/tileset.json, generated
//      by web/fixtures/gen_3dtiles_fixture.py) is genuinely derived from the geodetic
//      point it claims (`tileset.extras.geoReference`), not an arbitrary or absent
//      matrix -- checked by independently recomputing `geodeticToEcef(lonDeg, latDeg,
//      heightM)` from those declared fields (web/js/globe_lod.js's real, canonical
//      implementation -- the exact function the viewer itself uses, not reimplemented
//      here) and comparing it to the transform's own translation column
//      (`ecefFromRootTransform`, web/js/tiles_layer.js), plus checking the rotation
//      columns are a genuine orthonormal East-North-Up basis whose "up" points along
//      the true WGS84 ellipsoid normal at that point (not merely *a* rotation).
//      A wrong implementation this would fail against: M15.4's original fixture
//      (no `root.transform` at all -- `ecefFromRootTransform` returns `null`), or any
//      fixture/implementation that keeps a transform physically disconnected from its
//      declared geodetic metadata (e.g. an identity matrix, or numbers copied from an
//      unrelated location).
//   2. Placing a stand-in overlay group via `web/js/tiles_layer.js`'s
//      `placeOverlayInBodyFixedFrame` and reparenting it into a *rotating* body-fixed
//      frame node (web/js/frames.js's real `FrameGraph`, exactly how
//      web/js/scene.js's `_bodyFixedFrame`/`_syncBodyFixedFrame` drive it) makes the
//      overlay's *world* position respond to that frame's rotation exactly as
//      predicted by composing the frame's rotation with the fixture's own
//      metadata-derived ECEF anchor point. A wrong implementation this would fail
//      against: M15.4's original code, which parented the overlay under the
//      *entities* (inertial, non-rotating) frame at a small fixed offset unrelated to
//      geography -- under that code, rotating a body-fixed frame node has *zero*
//      effect on the overlay's world position (they are not related in the scene
//      graph at all), so this check's "moves exactly as predicted" assertion would
//      fail outright, not merely "by some amount".
//
// Runs under plain `node` (no browser/DOM/WebGL): frames.js and this check only touch
// Three's core Object3D graph (Group, Quaternion, Object3D.attach), which is DOM-free
// -- same reasoning as frame_graph_check.mjs.
import * as THREE from 'three';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { FrameGraph } from './frames.js';
import { geodeticToEcef, SCENE_UNITS_PER_METRE } from './globe_lod.js';
import {
  parseTileset3D, ecefFromRootTransform, enuBasisFromRootTransform, placeOverlayInBodyFixedFrame,
} from './tiles_layer.js';

const checks = [];
function check(name, pass) { checks.push({ name, pass: !!pass }); }
function approxEqual(a, b, tol) { return Math.abs(a - b) <= tol; }

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const TILESET_PATH = path.join(__dirname, '..', 'fixtures', '3dtiles', 'tileset.json');
const tilesetJson = JSON.parse(fs.readFileSync(TILESET_PATH, 'utf8'));
const tree = parseTileset3D(tilesetJson);

// ---------------------------------------------------------------- 1. real metadata
const geo = tree.extras && tree.extras.geoReference;
check('fixture declares extras.geoReference', !!geo);
check('fixture has a root.transform at all (M15.4 had none)', !!tree.rootTransform);

const ecefFromTransform = ecefFromRootTransform(tree.rootTransform);
const ecefFromMetadata = geo ? geodeticToEcef(geo.lonDeg, geo.latDeg, geo.heightM) : null;
const translationErrM = ecefFromMetadata
  ? Math.hypot(
    ecefFromTransform.x - ecefFromMetadata.x,
    ecefFromTransform.y - ecefFromMetadata.y,
    ecefFromTransform.z - ecefFromMetadata.z,
  )
  : Infinity;
check(
  'root.transform translation column == geodeticToEcef(declared lon/lat/height) to sub-millimetre',
  translationErrM < 1e-6,
);
// Real ECEF magnitude (~Earth radius), not e.g. a tiny M15.4-style demo offset.
check(
  'root.transform translation magnitude is Earth-surface scale (not a small demo offset)',
  Math.hypot(ecefFromTransform.x, ecefFromTransform.y, ecefFromTransform.z) > 6e6,
);

const basis = enuBasisFromRootTransform(tree.rootTransform);
const dot = (u, v) => u.x * v.x + u.y * v.y + u.z * v.z;
const len = (u) => Math.hypot(u.x, u.y, u.z);
check('East/North/Up basis is unit length', [basis.east, basis.north, basis.up].every((v) => approxEqual(len(v), 1, 1e-12)));
check('East/North/Up basis is orthogonal', (
  approxEqual(dot(basis.east, basis.north), 0, 1e-12)
  && approxEqual(dot(basis.east, basis.up), 0, 1e-12)
  && approxEqual(dot(basis.north, basis.up), 0, 1e-12)
));
// "Up" must be the true WGS84 ellipsoid outward normal at the anchor point, not just
// "pointing away from Earth's centre" (which a spherical approximation would also
// satisfy) -- checked by comparing it to geodeticToEcef's own well-known ellipsoid-
// normal construction: the gradient of x^2/a^2+y^2/a^2+z^2/b^2=1 at the surface point,
// i.e. (x/a^2, y/a^2, z/b^2) normalized, which for an ellipsoid is NOT parallel to the
// position vector itself (that's the sphere-normal error this project's own
// globe_lod.js module docstring warns `ellipsoidNormal` (globe.js) exists to avoid).
const WGS84_A_M = 6378137.0, WGS84_B_M = 6356752.314245;
const gx = ecefFromTransform.x / (WGS84_A_M * WGS84_A_M);
const gy = ecefFromTransform.y / (WGS84_A_M * WGS84_A_M);
const gz = ecefFromTransform.z / (WGS84_B_M * WGS84_B_M);
const gLen = Math.hypot(gx, gy, gz);
const ellipsoidNormal = { x: gx / gLen, y: gy / gLen, z: gz / gLen };
check(
  'Up basis vector matches the true WGS84 ellipsoid normal (not merely the sphere/position direction)',
  approxEqual(dot(basis.up, ellipsoidNormal), 1, 1e-9),
);

// ------------------------------------------------------ 2. body-fixed frame placement
const graph = new FrameGraph();
graph.addFrame({ id: 'root' });
graph.addFrame({ id: 'body-fixed:Earth', parentId: 'root' });

const group = new THREE.Group(); // stand-in for TilesOverlayLayer.group
placeOverlayInBodyFixedFrame(group, graph, 'body-fixed:Earth');
check('group reparented under the body-fixed frame node', graph.frameOf(group) === 'body-fixed:Earth');
check('group has identity local position (no re-applied demo offset)', group.position.length() === 0);
check('group scale is SCENE_UNITS_PER_METRE (metres -> scene units, same constant globe.js uses)',
  group.scale.x === SCENE_UNITS_PER_METRE && group.scale.y === SCENE_UNITS_PER_METRE && group.scale.z === SCENE_UNITS_PER_METRE);

// A probe point standing in for a loaded tile-content vertex at its own local
// origin: per tiles_layer.js's module docstring, the vendored TilesRenderer
// premultiplies every loaded tile's local matrix by the cumulative root.transform
// *before* adding it as a child of `group` -- so, in `group`-local metre-space
// (`group` itself carries no additional offset, by design), such a vertex already
// sits at `ecefFromTransform` exactly. In *scene units*, under an unrotated
// body-fixed frame, its world position must therefore be
// `ecefFromTransform * SCENE_UNITS_PER_METRE`.
const probeLocal = new THREE.Vector3(ecefFromTransform.x, ecefFromTransform.y, ecefFromTransform.z);
function worldPositionOfProbe() {
  graph.frame('body-fixed:Earth').object3D.updateMatrixWorld(true);
  return probeLocal.clone().applyMatrix4(group.matrixWorld);
}

const bodyNode = graph.frame('body-fixed:Earth').object3D;
bodyNode.position.set(0, 0, 0);
bodyNode.quaternion.identity();
let world = worldPositionOfProbe();
const expectedUnrotated = {
  x: ecefFromTransform.x * SCENE_UNITS_PER_METRE,
  y: ecefFromTransform.y * SCENE_UNITS_PER_METRE,
  z: ecefFromTransform.z * SCENE_UNITS_PER_METRE,
};
check('unrotated body-fixed frame: overlay world position == geo-referenced ECEF point (scene units)', (
  approxEqual(world.x, expectedUnrotated.x, 1e-9)
  && approxEqual(world.y, expectedUnrotated.y, 1e-9)
  && approxEqual(world.z, expectedUnrotated.z, 1e-9)
));

// Now rotate the body-fixed frame (exactly what scene.js's _syncBodyFixedFrame does
// every tick, copying a body's real BodyInterp-derived orientation onto this node) --
// the overlay must rotate WITH it, since ECEF content is meaningless without that.
// This is the check M15.4's fixed-demo-transform code (parented in the *entities*,
// non-rotating frame) would fail outright: rotating a body-fixed node it was never
// attached to changes nothing about its world position.
const spin = new THREE.Quaternion().setFromAxisAngle(new THREE.Vector3(0, 0, 1), Math.PI / 2); // 90 degrees about +Z
bodyNode.quaternion.copy(spin);
world = worldPositionOfProbe();
const expectedRotated = new THREE.Vector3(expectedUnrotated.x, expectedUnrotated.y, expectedUnrotated.z)
  .applyQuaternion(spin);
check('rotating the body-fixed frame rotates the overlay world position exactly as predicted', (
  approxEqual(world.x, expectedRotated.x, 1e-9)
  && approxEqual(world.y, expectedRotated.y, 1e-9)
  && approxEqual(world.z, expectedRotated.z, 1e-9)
));
check('rotation actually changed the overlay world position (not a no-op)', (
  Math.abs(world.x - expectedUnrotated.x) > 1e-6 || Math.abs(world.y - expectedUnrotated.y) > 1e-6
));

// The frame's own parent ('root') is untouched by any of this -- mirrors
// frame_graph_check.mjs's "parent stayed at true zero" style check.
check('root frame itself was never perturbed', graph.frame('root').object3D.position.length() === 0);

const allPass = checks.every((c) => c.pass);
process.stdout.write(JSON.stringify({
  allPass,
  checks,
  ecefFromTransform,
  ecefFromMetadata,
  translationErrM,
  geoReference: geo,
}));
process.exit(allPass ? 0 : 1);
