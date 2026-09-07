// CLI harness for tests/test_viewer_jitter.py: `node web/js/ric_axes_check.mjs`.
//
// M5.2: checks the viewer's client-side RIC/VNB/VVLH axes computation
// (web/js/frames.js's axesRIC/axesVNB/axesVVLH, driven every render tick by
// FrameNode.update() for a frame whose axesKind is set) against GMAT's own
// ObjectReferenced axes computation, via the fixture
// web/js/fixtures/ric_axes_fixture.json (web/js/fixtures/gen_ric_fixture.py -- real
// GMAT, read-only altavista/frames.py's FrameRegistry.rotation_matrix()). See that
// generator's module docstring for why every tested epoch is an exact
// trajectory-sample knot (isolating axes-formula agreement from Hermite
// interpolation fidelity, which web/js/scene_jitter_harness.mjs already covers
// separately).
//
// This is the real client code path, not a reimplementation of it: the origin track
// is interpolated with interp.js's actual TrajectoryInterp (the same class
// FrameNode.setOriginTrack uses), and the axes/quaternion math is frames.js's actual
// exported functions (the same ones FrameNode.update() calls every tick). There is
// exactly one implementation of this arithmetic; this check and the browser both run
// it.
//
// Two things are checked per (epoch, axes kind):
//   1. `axesResidual` -- the client's raw x/y/z axis vectors (axesForKind) against
//      GMAT's rotation-matrix rows (the actual GMAT-vs-JS comparison this fixture is
//      for).
//   2. `quatResidual` -- a self-consistency check that quaternionFromAxes (what
//      FrameNode.update() actually assigns to object3D.quaternion) reproduces the
//      exact same x/y/z when applied to the unit axes -- proving the quaternion
//      construction step doesn't introduce error of its own, independent of #1.
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import * as THREE from 'three';
import { TrajectoryInterp } from './interp.js';
import { axesForKind, quaternionFromAxes } from './frames.js';

const here = path.dirname(fileURLToPath(import.meta.url));
const fixture = JSON.parse(readFileSync(path.join(here, 'fixtures', 'ric_axes_fixture.json'), 'utf8'));

const interp = new TrajectoryInterp(fixture.originTrack);
const pos = new THREE.Vector3(), vel = new THREE.Vector3();
const x = new THREE.Vector3(), y = new THREE.Vector3(), z = new THREE.Vector3();
const q = new THREE.Quaternion();
const xr = new THREE.Vector3(), yr = new THREE.Vector3(), zr = new THREE.Vector3();

const BOUND = 1e-9;
const results = [];
let worstResidual = 0;
let worstDetail = null;

for (const entry of fixture.epochs) {
  const t = entry.t;
  interp.at(t, pos, vel); // real Hermite interpolation, at an exact knot (see docstring above)
  for (const kind of Object.keys(entry.rotation)) {
    axesForKind(kind, pos, vel, x, y, z);
    const gmatRows = entry.rotation[kind]; // [row0=X axis, row1=Y axis, row2=Z axis], each in EarthMJ2000Eq coords
    const axesArr = [x, y, z];
    let axesResidual = 0;
    for (let i = 0; i < 3; i++) {
      const g = gmatRows[i], c = axesArr[i];
      axesResidual = Math.max(axesResidual, Math.abs(g[0] - c.x), Math.abs(g[1] - c.y), Math.abs(g[2] - c.z));
    }

    quaternionFromAxes(x, y, z, q);
    xr.set(1, 0, 0).applyQuaternion(q);
    yr.set(0, 1, 0).applyQuaternion(q);
    zr.set(0, 0, 1).applyQuaternion(q);
    const quatResidual = Math.max(xr.distanceTo(x), yr.distanceTo(y), zr.distanceTo(z));

    const maxAbsResidual = Math.max(axesResidual, quatResidual);
    results.push({ t, kind, axesResidual, quatResidual, maxAbsResidual });
    if (maxAbsResidual > worstResidual) { worstResidual = maxAbsResidual; worstDetail = { t, kind }; }
  }
}

const pass = worstResidual < BOUND;
process.stdout.write(JSON.stringify({ pass, bound: BOUND, worstResidual, worstDetail, results }));
process.exit(pass ? 0 : 1);
