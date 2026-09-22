// CLI harness for tests/test_entities_model.py: `node web/js/entities_model_check.mjs`.
//
// Proves web/js/entities/model_entity.js end to end:
//   - realGLTFParse: the vendored `GLTFLoader` (web/vendor/three/addons/loaders/
//     GLTFLoader.js) really parses web/js/fixtures/entity_model_fixture.gltf (a
//     hand-authored, embedded-buffer, texture-free asset -- 4 vertices, 3 triangles)
//     under plain `node`, with no DOM -- checked from the SCENE GRAPH (walking
//     `gltf.scene`'s children to find the real `THREE.Mesh`, reading its geometry's
//     `position` attribute back out and comparing every one of the 4 vertices against
//     the exact values this task wrote into the fixture's own base64 buffer), never
//     from a "did it throw" counter alone.
//   - modelEntityAttachModel: `ModelEntity.attachModel()` reparents the glTF's own
//     scene children into the entity's STABLE `group` (never swaps which object IS
//     `group`) -- checked by object identity of `entity.group` across the call, plus
//     the same real-mesh-in-the-scene-graph check as above via `entity.group`.
//   - attitudeWiredThroughBodyInterp: a REAL `BodyInterp` (web/js/interp.js, unmodified,
//     imported not reimplemented) WITH a `quat` track drives `entity.update(t)`; the
//     resulting `entity.group.quaternion` is compared against calling
//     `BodyInterp.orientation(t, out)` directly (the same function, proving the WIRING,
//     which is this file's own job -- BodyInterp's own slerp correctness is already
//     proven independently by web/js/attitude_slerp_check.mjs, not re-proven here).
//   - missingQuatDegradesToIdentity: a REAL `BodyInterp` constructed from a body with NO
//     `quat` key at all (interp.js's own round-4 guard, "Question 229 / round-4 defect
//     4") drives `entity.update(t)` -- `entity.group.quaternion` must be the identity
//     quaternion, and the model's mesh must STILL be present in the scene graph
//     (`entity.group.children.length > 0`) -- "must degrade to the identity
//     orientation, not throw and not disappear", this task's own brief, verbatim.
//   - noAttitudeSourceDegradesToIdentity: a `ModelEntity` with NO attitude source
//     configured at all (one level up from BodyInterp's own guard) also lands on the
//     identity quaternion, never a stale/uninitialized value.
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import * as THREE from 'three';
import { GLTFLoader } from 'three/addons/loaders/GLTFLoader.js';
import { BodyInterp } from './interp.js';
import { ModelEntity, createModelEntityFromGLTF, parseGLTFAsset } from './entities/model_entity.js';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const FIXTURE_PATH = path.join(__dirname, 'fixtures', 'entity_model_fixture.gltf');

// Node-only shim, torn down immediately after use -- same pattern
// web/js/command_panel_check.mjs/web/js/layers_panel_check.mjs already use for
// `globalThis.document`, `web/js/gateway_imagery_layer_check.mjs` for
// `globalThis.createImageBitmap`: every real browser has `ProgressEvent` natively (it
// is a standard DOM event type), but plain `node` does not, and the vendored
// `THREE.FileLoader` (web/vendor/three/three.core.js) constructs one internally even
// when resolving an EMBEDDED base64 data-URI buffer (no network fetch happens for
// this fixture -- see this file's own module docstring -- but the loader's generic
// XHR/fetch-shaped code path still dispatches a synthetic progress event either way).
// This shim exists ONLY so `node` can run the real, unmodified `GLTFLoader`/
// `FileLoader` for this check; it changes nothing about what is parsed or measured,
// and `web/js/entities/model_entity.js` itself never references `ProgressEvent`.
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

const checks = [];
function check(name, pass, detail) { checks.push({ name, pass: !!pass, detail }); }
function approxEqual(a, b, tol) { return Math.abs(a - b) <= tol; }
function quatApproxEqual(a, b, tol) {
  return approxEqual(a.x, b.x, tol) && approxEqual(a.y, b.y, tol)
    && approxEqual(a.z, b.z, tol) && approxEqual(a.w, b.w, tol);
}

// Ground truth: the exact vertex positions this task encoded into the fixture's own
// base64 buffer when it was authored (web/js/fixtures/entity_model_fixture.gltf) --
// written down independently of the loader, not read back from a prior parse.
const EXPECTED_VERTS = [
  [0, 0, 0],
  [1, 0, 0],
  [0, 1, 0],
  [0, 0, 1.5],
];

function findFirstMesh(object3D) {
  if (object3D.isMesh) return object3D;
  for (const child of object3D.children) {
    const found = findFirstMesh(child);
    if (found) return found;
  }
  return null;
}

async function main() {
  await withNodeProgressEventShim(runChecks);
  const allPass = checks.every((c) => c.pass);
  process.stdout.write(JSON.stringify({ allPass, checks }));
  // Manager review, round 6: match web/js/layout/layout_tree_check.mjs -- a failing
  // check must not exit 0. The pytest side parses the JSON for the failing names.
  if (!allPass) process.exitCode = 1;
}

async function runChecks() {
  const fixtureText = fs.readFileSync(FIXTURE_PATH, 'utf8');
  const loader = new GLTFLoader();

  // --------------------------------------------------------- real glTF parse
  const gltf = await parseGLTFAsset(loader, fixtureText, __dirname);
  const mesh = findFirstMesh(gltf.scene);
  check('realGLTFParse_meshFoundInSceneGraph', mesh !== null && mesh.isMesh === true, { found: mesh !== null });
  if (mesh) {
    const posAttr = mesh.geometry.getAttribute('position');
    check('realGLTFParse_vertexCount', posAttr.count === EXPECTED_VERTS.length, { got: posAttr.count, expected: EXPECTED_VERTS.length });
    let allVertsOk = true;
    const gotVerts = [];
    for (let i = 0; i < EXPECTED_VERTS.length; i++) {
      const v = [posAttr.getX(i), posAttr.getY(i), posAttr.getZ(i)];
      gotVerts.push(v);
      const exp = EXPECTED_VERTS[i];
      if (!v.every((c, k) => approxEqual(c, exp[k], 1e-6))) allVertsOk = false;
    }
    check('realGLTFParse_vertexPositionsMatchFixtureGroundTruth', allVertsOk, { gotVerts, expectedVerts: EXPECTED_VERTS });
    check('realGLTFParse_triangleCount', mesh.geometry.index.count / 3 === 3, { indexCount: mesh.geometry.index.count });
  }

  // --------------------------------------------------------- ModelEntity.attachModel
  const entity = new ModelEntity({ id: 'fixture-entity' });
  const groupBeforeAttach = entity.group;
  const gltf2 = await parseGLTFAsset(loader, fixtureText, __dirname);
  entity.attachModel(gltf2);
  check('modelEntityAttachModel_groupIdentityPreserved', entity.group === groupBeforeAttach, {});
  check('modelEntityAttachModel_modelLoadedFlagSet', entity.modelLoaded === true, {});
  const attachedMesh = findFirstMesh(entity.group);
  check('modelEntityAttachModel_meshPresentInEntityGroup', attachedMesh !== null, {});

  // --------------------------------------------------------- convenience factory
  const entity2 = await createModelEntityFromGLTF({ id: 'fixture-entity-2', data: fixtureText, path: __dirname, loader: new GLTFLoader() });
  check('createModelEntityFromGLTF_meshPresent', findFirstMesh(entity2.group) !== null, {});

  // --------------------------------------------------------- attitude: real BodyInterp WITH quat
  const bodyWithQuat = {
    t: [0, 1, 2],
    pos: [[0, 0, 0], [1, 0, 0], [2, 0, 0]],
    // A real, non-trivial quaternion track [x,y,z,w] per sample (not all-identity,
    // so slerp between samples is actually exercised).
    quat: [
      0, 0, 0, 1,
      0, 0, 0.3826834323650898, 0.9238795325112867, // 45deg about Z
      0, 0, 0.7071067811865476, 0.7071067811865476, // 90deg about Z
    ],
  };
  const bi = new BodyInterp(bodyWithQuat);
  const entity3 = new ModelEntity({ id: 'attitude-entity', attitudeSource: bi });
  entity3.attachModel(await parseGLTFAsset(new GLTFLoader(), fixtureText, __dirname));
  const tSample = 0.5;
  entity3.update(tSample);
  const expectedQuat = new THREE.Quaternion();
  bi.orientation(tSample, expectedQuat);
  check('attitudeWiredThroughBodyInterp_matchesDirectCall', quatApproxEqual(entity3.group.quaternion, expectedQuat, 1e-12), {
    got: entity3.group.quaternion.toArray(), expected: expectedQuat.toArray(),
  });
  // Sanity: this should NOT be the identity (a wiring bug that silently fell back to
  // identity even though a real, non-identity attitude source was given would pass a
  // weaker "quaternion is defined" check but must fail this one).
  const isIdentity = quatApproxEqual(entity3.group.quaternion, new THREE.Quaternion(), 1e-9);
  check('attitudeWiredThroughBodyInterp_isNotIdentity', !isIdentity, { quat: entity3.group.quaternion.toArray() });

  // --------------------------------------------------------- attitude: real BodyInterp, body has NO quat
  const bodyNoQuat = { t: [0, 1], pos: [[0, 0, 0], [1, 1, 1]] }; // no `quat` key at all
  const biNoQuat = new BodyInterp(bodyNoQuat);
  const entity4 = new ModelEntity({ id: 'no-attitude-body-entity', attitudeSource: biNoQuat });
  entity4.attachModel(await parseGLTFAsset(new GLTFLoader(), fixtureText, __dirname));
  entity4.update(0.5); // must not throw
  const identityQ = new THREE.Quaternion();
  check('missingQuatDegradesToIdentity_quaternionIsIdentity', quatApproxEqual(entity4.group.quaternion, identityQ, 1e-12), {
    got: entity4.group.quaternion.toArray(),
  });
  check('missingQuatDegradesToIdentity_modelStillInSceneGraph', findFirstMesh(entity4.group) !== null && entity4.group.children.length > 0, {
    childCount: entity4.group.children.length,
  });

  // --------------------------------------------------------- attitude: no source at all
  const entity5 = new ModelEntity({ id: 'no-source-entity' });
  entity5.attachModel(await parseGLTFAsset(new GLTFLoader(), fixtureText, __dirname));
  entity5.update(123.456); // must not throw despite no attitude source ever set
  check('noAttitudeSourceDegradesToIdentity_quaternionIsIdentity', quatApproxEqual(entity5.group.quaternion, identityQ, 1e-12), {
    got: entity5.group.quaternion.toArray(),
  });
}

main().catch((err) => {
  process.stdout.write(JSON.stringify({ allPass: false, checks: [{ name: 'uncaughtException', pass: false, detail: { message: String(err && err.stack || err) } }] }));
  // Manager review, round 6: an uncaught exception is a failure and exits non-zero,
  // like every other check in this tree; the JSON above still carries the reason so
  // the pytest side reports the stack rather than a bare exit code.
  process.exitCode = 1;
});
