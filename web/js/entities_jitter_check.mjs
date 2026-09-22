// CLI harness for tests/test_entities_jitter.py: `node web/js/entities_jitter_check.mjs`.
//
// H6 scope item 4 (docs/heavy-plan.md): "the RIC jitter test extended to a model at a
// ten-metre RPO range (question 46)." Question 46's answer: "floating origin per frame
// with centimetre stability in RIC and sub-metre elsewhere... CI jitter tests at LEO,
// Moon, Mars and a 10 m RPO scene." The existing RPO proof
// (web/js/scene_jitter_harness.mjs's `measureRpo`, driven by tests/test_viewer_jitter.py)
// measures centimetre stability of the chief/deputy TRAJECTORY POLYLINE vertices at
// 10 m separation. This file extends that to a real glTF MODEL's own mesh vertices,
// attached to the deputy spacecraft, with a real (non-identity) attitude applied --
// the concrete thing a `web/js/entities/model_entity.js` `ModelEntity` actually draws,
// as opposed to a zero-extent point.
//
// This file reuses, never reimplements:
//   - `buildTrack`, `SCENES` from `./scene_jitter_harness.mjs` (the exact RPO
//     chief/deputy construction `measureRpo` itself uses -- imported, not copied, per
//     that file's own module docstring, "M26.3: guarded so web/js/viewport_check.mjs
//     can import... reusing the exact RPO-measurement arithmetic, not a second copy").
//   - `TrajectoryInterp` from `./interp.js`, `FloatingOrigin`/`trueRelative`/`length`
//     from `./origin.js`, `SCALE` from `./scene.js` -- the identical real modules
//     `measureRpo` itself is built from.
//   - the real vendored `GLTFLoader` + `web/js/entities/model_entity.js`'s
//     `parseGLTFAsset`, against the SAME fixture `web/js/entities_model_check.mjs`
//     already proves parses correctly (`web/js/fixtures/entity_model_fixture.gltf`).
//
// What is measured: for every one of the fixture model's 4 real mesh vertices
// (0 to 1.5 m from the model's own local origin), attached to the DEPUTY spacecraft
// (10 m from the chief, at LEO altitude) with a real, non-identity attitude quaternion
// applied via `THREE.Vector3.applyQuaternion` (real Three.js, not reimplemented),
// compare the render-space-reconstructed absolute position against the true absolute
// position (computed directly, never through `toRenderSpace`) -- WITH and WITHOUT the
// floating origin, exactly `measureRpo`'s own two-sided proof shape. Bound: centimetre,
// WITH the origin; the WITHOUT path must EXCEED the centimetre bound (proving the
// origin is load-bearing for a real model at this range, not merely present).
import * as THREE from 'three';
import path from 'node:path';
import fs from 'node:fs';
import { fileURLToPath } from 'node:url';
import { GLTFLoader } from 'three/addons/loaders/GLTFLoader.js';
import { TrajectoryInterp } from './interp.js';
import { FloatingOrigin, length, trueRelative } from './origin.js';
import { SCALE } from './scene.js';
import { buildTrack, SCENES } from './scene_jitter_harness.mjs';
import { parseGLTFAsset } from './entities/model_entity.js';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const FIXTURE_PATH = path.join(__dirname, 'fixtures', 'entity_model_fixture.gltf');

const SCENE_UNIT_M = 1e6; // 1 scene unit = 1000 km = 1e6 m -- matches scene_jitter_harness.mjs's own constant
const CENTIMETRE_BOUND_M = 0.01;
const SEP_M = 10; // the RPO range this item extends the jitter test to (question 46)

// A real, non-identity, non-axis-aligned attitude for the deputy's model -- proves the
// extension covers a rotated model, not merely a translated point (a rotation this
// small in magnitude does not itself affect the floating-origin ARITHMETIC, which is
// pure translation -- see origin.js's own module docstring -- but it does change which
// absolute world point each local vertex maps to, which is what must still be
// reconstructed correctly).
const DEPUTY_ATTITUDE_QUAT = (() => {
  const q = new THREE.Quaternion();
  q.setFromAxisAngle(new THREE.Vector3(0.267, 0.535, 0.802).normalize(), THREE.MathUtils.degToRad(37));
  return q;
})();

function findFirstMesh(object3D) {
  if (object3D.isMesh) return object3D;
  for (const child of object3D.children) {
    const found = findFirstMesh(child);
    if (found) return found;
  }
  return null;
}

// Node-only shim, torn down immediately after use -- see web/js/entities_model_check.mjs's
// own identical shim for the full explanation (a real browser has ProgressEvent
// natively; plain node does not, and the vendored FileLoader constructs one internally
// even for an embedded base64 buffer with no network fetch).
async function withNodeProgressEventShim(fn) {
  const hadOwn = Object.prototype.hasOwnProperty.call(globalThis, 'ProgressEvent');
  const previous = globalThis.ProgressEvent;
  if (typeof globalThis.ProgressEvent === 'undefined') {
    globalThis.ProgressEvent = class ProgressEvent {
      constructor(type, init = {}) {
        this.type = type;
        this.lengthComputable = !!init.lengthComputable;
        this.loaded = init.loaded || 0;
        this.total = init.total || 0;
      }
    };
  }
  try {
    return await fn();
  } finally {
    if (hadOwn) globalThis.ProgressEvent = previous; else delete globalThis.ProgressEvent;
  }
}

async function loadFixtureVertices() {
  const text = fs.readFileSync(FIXTURE_PATH, 'utf8');
  const gltf = await withNodeProgressEventShim(() => parseGLTFAsset(new GLTFLoader(), text, __dirname));
  const mesh = findFirstMesh(gltf.scene);
  const posAttr = mesh.geometry.getAttribute('position');
  const verts = [];
  for (let i = 0; i < posAttr.count; i++) {
    verts.push(new THREE.Vector3(posAttr.getX(i), posAttr.getY(i), posAttr.getZ(i)));
  }
  return verts; // metres, local model space
}

/** Extends measureRpo's own scene (chief/deputy, SEP_M apart at LEO altitude) with a
 * real glTF model's own mesh vertices attached to the DEPUTY, at real sample times
 * across one render frame -- same "ordinary case between two rebases" discipline
 * `measureSingleObject`/`measureRpo` (scene_jitter_harness.mjs) already use, never a
 * cherry-picked single instant. */
async function measureModelAtRpoRange() {
  const modelVertsM = await loadFixtureVertices();

  const chiefTrack = buildTrack(SCENES.LEO.distKm, SCENES.LEO.speedKmS);
  const deputyPos = chiefTrack.pos.slice();
  for (let i = 1; i < deputyPos.length; i += 3) deputyPos[i] += SEP_M / 1000; // +y, km -- identical to measureRpo's own offset
  const deputyTrack = { t: chiefTrack.t, pos: deputyPos, vel: chiefTrack.vel };
  const deputyInterp = new TrajectoryInterp(deputyTrack);
  const deputyPoly = deputyInterp.polyline();

  const originAbs = {
    x: chiefTrack.pos[0] * SCALE, y: chiefTrack.pos[1] * SCALE, z: chiefTrack.pos[2] * SCALE,
  };

  const fo = new FloatingOrigin();
  fo.setOrigin('ric-model', originAbs.x, originAbs.y, originAbs.z);
  const foOff = new FloatingOrigin({ globalEnabled: false });

  let errWithM = 0;
  let errWithoutM = 0;
  const tmp = { x: 0, y: 0, z: 0, set(x, y, z) { this.x = x; this.y = y; this.z = z; return this; } };
  const perVertexMaxErrWithM = new Array(modelVertsM.length).fill(0);

  for (let i = 0; i < deputyPoly.times.length; i++) {
    const t = deputyPoly.times[i];
    deputyInterp.at(t, tmp);
    // The deputy's TRUE absolute position at this sample, in scene units -- exactly
    // the same ground-truth source measureRpo/measureSingleObject use (interp.at at
    // the vertex's own sample time), never re-derived from the densified polyline.
    const deputyAbsSceneUnits = { x: tmp.x * SCALE, y: tmp.y * SCALE, z: tmp.z * SCALE };

    for (let v = 0; v < modelVertsM.length; v++) {
      const localM = modelVertsM[v].clone().applyQuaternion(DEPUTY_ATTITUDE_QUAT); // real THREE rotation
      const offsetSceneUnits = { x: localM.x / SCENE_UNIT_M, y: localM.y / SCENE_UNIT_M, z: localM.z / SCENE_UNIT_M };
      const trueAbs = {
        x: deputyAbsSceneUnits.x + offsetSceneUnits.x,
        y: deputyAbsSceneUnits.y + offsetSceneUnits.y,
        z: deputyAbsSceneUnits.z + offsetSceneUnits.z,
      };

      const renderedWith = fo.toRenderSpace('ric-model', trueAbs);
      const reconWith = {
        x: originAbs.x + renderedWith.x, y: originAbs.y + renderedWith.y, z: originAbs.z + renderedWith.z,
      };
      const errWith = length(trueRelative(reconWith, trueAbs)) * SCENE_UNIT_M;
      errWithM = Math.max(errWithM, errWith);
      perVertexMaxErrWithM[v] = Math.max(perVertexMaxErrWithM[v], errWith);

      const renderedWithout = foOff.toRenderSpace('ric-model', trueAbs);
      const errWithout = length(trueRelative(renderedWithout, trueAbs)) * SCENE_UNIT_M;
      errWithoutM = Math.max(errWithoutM, errWithout);
    }
  }

  return {
    errWithM, errWithoutM, perVertexMaxErrWithM, vertexCount: modelVertsM.length,
    farthestVertexOffsetM: Math.max(...modelVertsM.map((v) => v.length())),
  };
}

const result = await measureModelAtRpoRange();
process.stdout.write(JSON.stringify({
  sepM: SEP_M,
  centimetreBoundM: CENTIMETRE_BOUND_M,
  ...result,
}));
