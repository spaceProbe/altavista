// CLI harness for tests/test_viewer_jitter.py: `node web/js/frame_graph_check.mjs`.
//
// Proves, by actually exercising web/js/frames.js (not a description of it), that
// "switching frames is re-parenting, not re-loading" (docs/architecture.md §4) is
// structurally true: a spacecraft's trajectory geometry and a camera are built once,
// the camera is switched between frames several times, and every check below shows
// object identity of the retained geometry/material/buffer -- not just equal values,
// the exact same object reference -- survives every switch. If a future change to
// frames.js ever made a frame switch dispose or recreate geometry, this would fail
// (the `===` identity checks; a value-equality check would not catch that class of
// regression, which is the whole reason this compares references).
//
// Runs under plain `node` (no browser/DOM/WebGL): frames.js only touches Three's core
// Object3D graph (Group, Object3D.attach, Matrix4), which is DOM-free and works
// headlessly, as web/node_modules/three (a resolution shim, see its package.json)
// makes importable from Node the same vendored file web/vendor/three/three.module.js
// the browser uses.
import * as THREE from 'three';
import { FrameGraph, orderFrameDefsByParent } from './frames.js';

const checks = [];
function check(name, pass) { checks.push({ name, pass: !!pass }); }

const graph = new FrameGraph();
// A small, realistic frame tree: an inertial Earth frame, a Moon-centred frame
// hanging off it, and an entity-relative RIC frame (RPO) hanging off the inertial
// frame -- mirrors the FrameDefinition oneof origin (body / platform / entity) and
// parent-implying-axes relationships described in altavista/FRAMES.md.
graph.addFrame({ id: 'earth_mj2000eq' });
graph.addFrame({ id: 'moon_mj2000eq', parentId: 'earth_mj2000eq' });
graph.addFrame({ id: 'sat1_ric', parentId: 'earth_mj2000eq' });

// Build the geometry/material/camera exactly once, as the existing scene.js does for
// a spacecraft's trajectory line (LineGeometry + LineMaterial + Line2) -- see
// web/js/scene.js's setScenario(). We use plain BufferGeometry here since only
// object-identity through re-parenting is under test, not line rendering.
const geometry = new THREE.BufferGeometry();
geometry.setAttribute('position', new THREE.BufferAttribute(new Float32Array([0, 0, 0, 1, 1, 1]), 3));
const material = new THREE.LineBasicMaterial();
const buffer = geometry.getAttribute('position'); // the actual typed-array-backed buffer
const trajectory = new THREE.Line(geometry, material);
trajectory.name = 'sat1-trajectory';

const camera = new THREE.PerspectiveCamera(45, 1, 1e-3, 1e9);

// Initial placement: trajectory lives in its spacecraft's RIC frame, camera starts
// Earth-fixed (a typical "overview" start).
graph.reparent(trajectory, 'sat1_ric');
graph.reparent(camera, 'earth_mj2000eq');
check('camera starts in earth_mj2000eq', graph.frameOf(camera) === 'earth_mj2000eq');
check('trajectory starts in sat1_ric', graph.frameOf(trajectory) === 'sat1_ric');

// Switch the camera through every frame in the graph, spacecraft-relative included --
// this is the exact scenario the module list promises ("the camera parented to any
// frame (Earth-fixed, inertial, body-centred, spacecraft-relative)").
for (const frameId of ['moon_mj2000eq', 'sat1_ric', 'earth_mj2000eq', 'sat1_ric']) {
  graph.reparent(camera, frameId);
  check(`camera reparented to ${frameId}`, graph.frameOf(camera) === frameId);
  // No geometry rebuild, ever: the trajectory's geometry/material/buffer are the
  // exact objects created once above, not equal-but-new instances.
  check(`geometry identity intact after camera -> ${frameId}`, trajectory.geometry === geometry);
  check(`material identity intact after camera -> ${frameId}`, trajectory.material === material);
  check(`vertex buffer identity intact after camera -> ${frameId}`, geometry.getAttribute('position') === buffer);
  check(`vertex buffer array identity intact after camera -> ${frameId}`, geometry.getAttribute('position').array === buffer.array);
}

// The trajectory itself can also switch frames (e.g. viewing it in RIC vs. inertial)
// without ever touching its geometry.
graph.reparent(trajectory, 'moon_mj2000eq');
check('trajectory reparented to moon_mj2000eq', graph.frameOf(trajectory) === 'moon_mj2000eq');
check('trajectory geometry identity intact after its own reparent', trajectory.geometry === geometry);
check('trajectory buffer array identity intact after its own reparent', geometry.getAttribute('position').array === buffer.array);

// No frame's node was disposed or recreated by any of the above: same three node
// count as after construction, camera/trajectory are still descendants of the graph
// root through their (possibly new) parent frame's Group -- not detached.
check('graph still has exactly 3 frame nodes', graph.nodes.size === 3);
check('camera still attached under the graph root', isDescendantOf(camera, graph.root));
check('trajectory still attached under the graph root', isDescendantOf(trajectory, graph.root));

function isDescendantOf(obj, root) {
  let p = obj.parent;
  while (p) { if (p === root) return true; p = p.parent; }
  return false;
}

// --------------------------------------------------------------------------------
// M4.1: multi-frame consumption from a *wire-shaped* frames list, the real path.
//
// The checks above build a FrameGraph by hand-calling addFrame() in parent-first
// order -- exactly what web/js/scene.js no longer gets to assume once the frames
// come from the scene JSON's additive `frames` list (altavista/scenario.py's
// _build_frames, docs/open-questions.md question 78): that list can name a child
// before its parent (register() order in altavista/frames.py's FrameRegistry is not
// guaranteed to match the JSON array order once auto-registered parents and
// explicitly-declared entity-relative frames interleave). This section drives
// exactly the function scene.js's _buildFrameGraph() calls (orderFrameDefsByParent,
// exported from frames.js, not reimplemented here) against a deliberately
// out-of-order, protobuf-JSON-camelCase-shaped payload -- an EarthMJ2000Eq root, an
// EarthBodyFixed root, and a RIC frame parented under EarthMJ2000Eq, in an order that
// lists the RIC frame *before* its parent -- proving the real ordering/consumption
// path, not a parallel implementation, handles it.
const wireFrames = [
  // RIC frame listed FIRST, before its own parent -- the out-of-order case.
  { id: 'target_ric', parentFrameId: 'EarthMJ2000Eq', axes: 'AXES_KIND_RIC',
    entityId: 'target', referenceEntityId: 'target', referenceBody: 'Earth',
    originTrack: { t: [0, 1], pos: [7000, 0, 0, 7000, 1, 0], vel: [0, 7.6, 0, 0, 7.6, 0] } },
  { id: 'EarthBodyFixed', axes: 'AXES_KIND_BODY_FIXED', body: 'Earth' },
  { id: 'EarthMJ2000Eq', axes: 'AXES_KIND_MJ2000_EQ', body: 'Earth' },
];
const normalized = wireFrames.map(fd => ({
  id: fd.id, parentId: fd.parentFrameId || null, originTrack: fd.originTrack || null,
}));
const ordered = orderFrameDefsByParent(normalized);
check('out-of-order wire list: parent-first order recovered',
  ordered.findIndex(d => d.id === 'EarthMJ2000Eq') < ordered.findIndex(d => d.id === 'target_ric'));

const wireGraph = new FrameGraph();
for (const d of ordered) {
  const node = wireGraph.addFrame(d);
  if (d.originTrack) node.setOriginTrack(d.originTrack);
}
check('wire graph has all 3 frame nodes', wireGraph.nodes.size === 3);
check('RIC frame correctly parented under EarthMJ2000Eq (not silently under root)',
  wireGraph.frame('target_ric').object3D.parent === wireGraph.frame('EarthMJ2000Eq').object3D);
check('EarthBodyFixed is a root frame (parented directly under graph root)',
  wireGraph.frame('EarthBodyFixed').object3D.parent === wireGraph.root);

// The RIC frame's own motion (originTrack, an entity's real trajectory shape --
// {t, pos, vel} A1MJD/km/km-s, exactly what altavista/scenario.py's _origin_track_for
// emits) drives its Group.position via FrameNode.update(), the same interp.js Hermite
// path a spacecraft's own trajectory uses -- not re-derived here.
const SCALE = 1e-3;
wireGraph.update(0, SCALE);
const ricPos0 = wireGraph.frame('target_ric').object3D.position.clone();
check('RIC frame origin tracks its declared originTrack at t=0',
  Math.abs(ricPos0.x - 7000 * SCALE) < 1e-9 && ricPos0.y === 0 && ricPos0.z === 0);
wireGraph.update(1, SCALE);
const ricPos1 = wireGraph.frame('target_ric').object3D.position.clone();
check('RIC frame origin moves between samples (not frozen at t=0)',
  Math.abs(ricPos1.y - 1 * SCALE) < 1e-9 && ricPos1.y !== ricPos0.y);

// A camera parented into the RIC frame node correctly composes through the parent
// chain (EarthMJ2000Eq -> target_ric), i.e. its world position reflects BOTH the
// RIC frame's own translation and its local offset -- the mechanism setViewFrame()
// in scene.js relies on (see that method's docstring for the full precision
// argument). EarthMJ2000Eq itself is never moved in this check (it has no
// originTrack), matching how scene.js keeps a true FrameDefinition node an
// always-zero anchor.
const wireCam = new THREE.PerspectiveCamera(45, 1, 1e-3, 1e9);
wireGraph.reparent(wireCam, 'target_ric');
wireCam.position.set(0.001, 0, 0); // a small, RIC-local offset
wireCam.updateMatrixWorld(true);
const camWorld = wireCam.getWorldPosition(new THREE.Vector3());
check('camera parented in RIC composes world position through EarthMJ2000Eq -> target_ric',
  Math.abs(camWorld.x - (ricPos1.x + 0.001)) < 1e-9 && Math.abs(camWorld.y - ricPos1.y) < 1e-9);
check('EarthMJ2000Eq (RIC\'s parent) stayed at true zero -- not perturbed by RIC\'s motion',
  wireGraph.frame('EarthMJ2000Eq').object3D.position.length() === 0);

// orderFrameDefsByParent() fails loudly (never silently drops/reparents to root) on
// a dangling parent reference or a parent cycle -- altavista/frames.py's own
// FrameParentMissingError/FrameCycleError counterparts, exercised for real here.
let dangling = false;
try { orderFrameDefsByParent([{ id: 'a', parentId: 'does_not_exist' }]); } catch { dangling = true; }
check('orderFrameDefsByParent throws on a dangling parentId', dangling);
let cyclic = false;
try { orderFrameDefsByParent([{ id: 'a', parentId: 'b' }, { id: 'b', parentId: 'a' }]); } catch { cyclic = true; }
check('orderFrameDefsByParent throws on a parent cycle', cyclic);

const allPass = checks.every(c => c.pass);
process.stdout.write(JSON.stringify({ allPass, checks }));
process.exit(allPass ? 0 : 1);
