// CLI harness for tests/test_entities_viewport_precision.py:
// `node web/js/entities_viewport_precision_check.mjs`; its measurement kernel is also imported
// by the real-browser proof, tests/test_viewer_viewport_precision_browser.py.
//
// Question 240 (docs/heavy-plan.md, carry-over round, defect 3; question 252, heavy precision
// round): the entity objects (spacecraft marker spheres, instanced entity markers, trail
// lines, covariance / keep-out meshes, glTF model groups, event meshes) are ONE set of
// Object3Ds drawn by every viewport's renderer. They used to be written once per tick
// relative to the PRIMARY camera's floating origin (`Math.fround` in `_toLocal`, Float32
// instance matrices and trail buffers), so a second viewport framed ~10 m from the Chaser
// drew them 0.04-0.25 m off at LEO, metres at GEO and tens of metres at lunar distance,
// hopping between ticks, against question 46's centimetre bound. `Viewer._placeSharedEntities`
// now writes them for the view that is about to draw them.
//
// What is measured, for every view that draws (the primary and each extra viewport), right
// after that view's renderer has drawn: the FINAL vertex position in that view's own eye frame,
// the way the GPU computes it -- the object's `modelViewMatrix` (three sets it per object per
// draw, `camera.matrixWorldInverse * object.matrixWorld` in f64) rounded to float32, applied
// to the object's float32 vertex data with every multiply and add rounded to float32 -- minus
// the float64 truth: the spacecraft's absolute position from its f64 interpolator (x SCALE),
// plus, for a model vertex, the model-local vertex through its node chain, the entity's
// attitude and the scale, transformed into the same camera's eye frame in f64 with the
// camera's own f64 `matrixWorldInverse`. Errors are metres (1 scene unit = 1e6 m). Per
// (view, class) the maximum error and the maximum tick-to-tick change of the error vector
// ("hop") are kept.
//
// Two ways to use it:
//   * `node web/js/entities_viewport_precision_check.mjs`: a stand-in object made with
//     `Object.create(Viewer.prototype)` (WebGL is unavailable under node) runs the REAL
//     `setScenario`, `addViewport`, `setViewportFocus`, `setFocus`, `update(t)` ... of scene.js
//     with real `Viewport`s, a real `InstancedMesh`, a real `ResidentEntityScene` / `LayerManager`
//     and the real parsed glTF fixture; only the renderers (`standInRenderer`: three's
//     per-render prologue and the per-object modelViewMatrix line, nothing else), the DOM
//     (`_label`), OrbitControls and the canvas are stand-ins. Prints one JSON object of named
//     checks and exits nonzero when any fails (question 233).
//   * imported by the browser test, which wraps the real renderers with `instrumentRenderer`
//     and drives the real page.
import * as THREE from 'three';
import { Viewer, SCALE } from './scene.js';

export const SCENE_UNIT_M = 1e6;
export const CENTIMETRE_M = 0.01;
const fr = Math.fround;

// ----------------------------------------------------------------------- the kernel
/** The 16 elements of a THREE.Matrix4 rounded to float32 (the uniform the GPU receives). */
export function matrixF32(m) { return Array.from(m.elements, fr); }

/** `m * vec4(p, 1)` in float32 arithmetic, `m` column-major with float32-valued elements,
 * `p` float32-valued: every multiply and every add is rounded (xyz only; the matrices are affine). */
export function applyF32(m, p) {
  const out = [0, 0, 0];
  for (let r = 0; r < 3; r++) {
    let acc = fr(m[r] * p[0]);
    acc = fr(acc + fr(m[4 + r] * p[1]));
    acc = fr(acc + fr(m[8 + r] * p[2]));
    out[r] = fr(acc + m[12 + r]);
  }
  return out;
}

const _mv = new THREE.Matrix4();
/** True when `obj.modelViewMatrix` was set by the draw that just happened (an object that was
 * culled or invisible keeps a stale one). */
function drawnNow(obj, camera) {
  _mv.multiplyMatrices(camera.matrixWorldInverse, obj.matrixWorld);
  const a = _mv.elements, b = obj.modelViewMatrix.elements;
  for (let i = 0; i < 16; i++) if (a[i] !== b[i]) return false;
  return true;
}

/** Keeps, per (view, class), the largest error and the largest change of the error vector
 * between consecutive measurements of the same vertex. */
export class Recorder {
  constructor() { this.reset(); }
  reset() { this.last = new Map(); this.stat = new Map(); this.skipped = new Map(); }
  add(view, cls, key, errM) {
    const id = `${view}|${cls}`;
    let s = this.stat.get(id);
    if (!s) { s = { view, cls, maxErr: 0, maxHop: 0, n: 0 }; this.stat.set(id, s); }
    const e = Math.hypot(errM[0], errM[1], errM[2]);
    s.maxErr = Math.max(s.maxErr, e);
    s.n += 1;
    const lk = `${id}|${key}`;
    const prev = this.last.get(lk);
    if (prev) s.maxHop = Math.max(s.maxHop, Math.hypot(errM[0] - prev[0], errM[1] - prev[1], errM[2] - prev[2]));
    this.last.set(lk, errM);
  }
  skip(view, cls) { const id = `${view}|${cls}`; this.skipped.set(id, (this.skipped.get(id) || 0) + 1); }
  /** { view: { cls: {maxErr, maxHop, n} } } */
  summary() {
    const out = {};
    for (const s of this.stat.values()) {
      (out[s.view] ||= {})[s.cls] = { maxErr: s.maxErr, maxHop: s.maxHop, n: s.n, skipped: this.skipped.get(`${s.view}|${s.cls}`) || 0 };
    }
    return out;
  }
}

export function nearestIndex(times, t) {
  let best = 0, bd = Infinity;
  for (let i = 0; i < times.length; i++) { const d = Math.abs(times[i] - t); if (d < bd) { bd = d; best = i; } }
  return best;
}

/**
 * Measure every entity object that `camera` has just drawn. `viewName` labels the view. Call
 * it right after that view's `renderer.render(scene, camera)`, before anything else renders.
 */
export function measureDraw(viewer, camera, viewName, rec) {
  const t = viewer._lastT;
  const camInv = camera.matrixWorldInverse;
  const truthEye = (world) => world.clone().applyMatrix4(camInv);
  const errOf = (eye32, truth) => [
    (eye32[0] - truth.x) * SCENE_UNIT_M, (eye32[1] - truth.y) * SCENE_UNIT_M, (eye32[2] - truth.z) * SCENE_UNIT_M,
  ];
  // an object's origin (a mesh centre) through its own float32 model-view matrix
  const centre = (obj, cls, key, absWorld) => {
    if (!obj.visible || !drawnNow(obj, camera)) { rec.skip(viewName, cls); return; }
    const mv = matrixF32(obj.modelViewMatrix);
    rec.add(viewName, cls, key, errOf(applyF32(mv, [0, 0, 0]), truthEye(absWorld)));
  };
  for (const [name, s] of viewer.spacecraft) {
    const abs = s.interp.at(t, new THREE.Vector3()).multiplyScalar(SCALE);
    centre(s.marker, 'sphere', name, abs);

    // instanced entity marker: the shader's `modelViewMatrix * (instanceMatrix * (0,0,0,1))`
    const mm = viewer._entityMarkerMesh;
    const mi = viewer._entityMarkerResidentNames.indexOf(name);
    if (mm && mm.visible && mi >= 0 && drawnNow(mm, camera)) {
      const im = mm.instanceMatrix.array; // Float32Array
      const p = [im[16 * mi + 12], im[16 * mi + 13], im[16 * mi + 14]];
      rec.add(viewName, 'marker', name, errOf(applyF32(matrixF32(mm.modelViewMatrix), p), truthEye(abs)));
    } else if (mi >= 0) rec.skip(viewName, 'marker');

    // trail: the newest vertex of the line's position buffer
    const tr = viewer._entityTrailLines.get(name);
    if (tr && tr.line.visible && drawnNow(tr.line, camera)) {
      const count = tr.line.geometry.drawRange.count;
      const idx = nearestIndex(s.poly.times, t);
      const truth = new THREE.Vector3(s.poly.points[3 * idx], s.poly.points[3 * idx + 1], s.poly.points[3 * idx + 2]).multiplyScalar(SCALE);
      const p = [tr.positions[3 * (count - 1)], tr.positions[3 * (count - 1) + 1], tr.positions[3 * (count - 1) + 2]];
      rec.add(viewName, 'trail', name, errOf(applyF32(matrixF32(tr.line.modelViewMatrix), p), truthEye(truth)));
    } else if (tr) rec.skip(viewName, 'trail');

    const cov = viewer._entityCovarianceMeshes.get(name);
    if (cov) centre(cov.mesh, 'ellipsoid', name, abs);
    const ko = viewer._entityKeepoutMeshes.get(name);
    if (ko) centre(ko.mesh, 'keepout', name, abs);

    // glTF model: every vertex of every mesh, through the mesh's own modelViewMatrix
    const me = viewer._entityModelEntities.get(name);
    if (me && me.modelLoaded && me.group.visible) {
      me.group.updateMatrix();
      me.group.traverse((o) => {
        if (!o.isMesh) return;
        if (!drawnNow(o, camera)) { rec.skip(viewName, 'model'); return; }
        // the node chain below the entity group (exact: small local matrices), then the
        // entity's attitude, then its absolute position -- all f64
        const chain = new THREE.Matrix4();
        for (let n = o; n !== me.group; n = n.parent) { n.updateMatrix(); chain.premultiply(n.matrix); }
        const mv = matrixF32(o.modelViewMatrix);
        const pos = o.geometry.attributes.position;
        for (let v = 0; v < pos.count; v++) {
          const p = [pos.getX(v), pos.getY(v), pos.getZ(v)];
          const truthWorld = new THREE.Vector3(p[0], p[1], p[2]).applyMatrix4(chain).applyQuaternion(me.group.quaternion).add(abs);
          rec.add(viewName, 'model', `${o.id}:${v}`, errOf(applyF32(mv, p), truthEye(truthWorld)));
        }
      });
    }
  }
  for (let i = 0; i < viewer.events.length; i++) {
    const e = viewer.events[i];
    if (e.interp) centre(e.mesh, 'event', String(i), e.interp.at(e.data.t, new THREE.Vector3()).multiplyScalar(SCALE));
  }
}

/** The measurement wants every entity object drawn in every view, wherever it lands in that
 * view's frustum (an object three culls keeps a stale modelViewMatrix): switch culling off on
 * the entity objects. */
export function disableCulling(viewer) {
  for (const s of viewer.spacecraft.values()) s.marker.frustumCulled = false;
  for (const e of viewer.events) e.mesh.frustumCulled = false;
  if (viewer._entityGroups) for (const g of Object.values(viewer._entityGroups)) g.traverse((o) => { o.frustumCulled = false; });
}

/** Wrap `renderer.render` so that `onDrawn(camera)` runs right after each draw. */
export function instrumentRenderer(renderer, onDrawn) {
  const orig = renderer.render.bind(renderer);
  renderer.render = (scene, camera) => { const r = orig(scene, camera); onDrawn(camera); return r; };
  return () => { renderer.render = orig; };
}

/** Wrap the primary renderer and every viewport's renderer of `viewer` so each draw is measured
 * into `rec`. Returns a function that removes the wrappers. */
export function instrumentViewer(viewer, rec) {
  const undo = [];
  const nameOf = (camera) => {
    if (camera === viewer.camera) return 'primary';
    for (const [id, vp] of viewer.viewports) if (vp.camera === camera) return id;
    return 'other';
  };
  const hook = (camera) => measureDraw(viewer, camera, nameOf(camera), rec);
  undo.push(instrumentRenderer(viewer.renderer, hook));
  for (const vp of viewer.viewports.values()) if (vp.renderer) undo.push(instrumentRenderer(vp.renderer, hook));
  return () => undo.forEach((u) => u());
}

// -------------------------------------------------------------- the scenario (closed form)
export const MU_EARTH_KM3_S2 = 398600.4418;
export const EPOCH_A1MJD = 34000.0;
export const STEP_S = 5.0;
export const N_SAMPLES = 400;
export const INCLINATION_DEG = 51.6;
export const CHASER_LEAD_M = 30.0;
export const MODEL_URL = '/js/fixtures/entity_model_fixture.gltf';
export const COV_DIAG_KM2 = [1.0, 0.25, 0.0625];
export const DISTANCES = [
  { id: 'LEO', label: 'LEO 7,000 km', radiusKm: 7000 },
  { id: 'GEO', label: 'GEO 42,164 km', radiusKm: 42164 },
  { id: 'lunar', label: 'lunar 384,400 km', radiusKm: 384400 },
];

/** Target and Chaser on one circular orbit of radius `radiusKm` (the Chaser 30 m ahead in
 * track, carrying the fixture glTF as its model), both carrying a closed-form diagonal
 * covariance, plus one event on the Chaser: the scene JSON the viewer is handed. */
export function closedFormScenario(radiusKm, { withEarth = false } = {}) {
  const n = Math.sqrt(MU_EARTH_KM3_S2 / radiusKm ** 3);
  const inc = (INCLINATION_DEG * Math.PI) / 180;
  const speed = n * radiusKm;
  const lead = (CHASER_LEAD_M / 1000) / radiusKm;
  const cov = [COV_DIAG_KM2[0], 0, 0, 0, COV_DIAG_KM2[1], 0, 0, 0, COV_DIAG_KM2[2]];
  const track = (name, phase, color, model) => {
    const t = [], pos = [], vel = [], covFlat = [];
    for (let k = 0; k < N_SAMPLES; k++) {
      const s = k * STEP_S, a = n * s + phase;
      t.push(EPOCH_A1MJD + s / 86400);
      pos.push(radiusKm * Math.cos(a), radiusKm * Math.sin(a) * Math.cos(inc), radiusKm * Math.sin(a) * Math.sin(inc));
      vel.push(-speed * Math.sin(a), speed * Math.cos(a) * Math.cos(inc), speed * Math.cos(a) * Math.sin(inc));
      covFlat.push(...cov);
    }
    const d = { name, label: name, color, t, pos, vel, attitude: [], cov: covFlat, covDim: 3 };
    if (model) d.model = model;
    return d;
  };
  const target = track('Target', 0, '#54a0ff');
  const chaser = track('Chaser', lead, '#ff6b6b', MODEL_URL);
  const sc = {
    name: `Viewport precision ${radiusKm}`,
    frame: { name: 'EarthMJ2000Eq', origin: 'Earth', axes: 'MJ2000Eq' },
    frames: [{ id: 'EarthMJ2000Eq', axes: 'AXES_KIND_MJ2000_EQ', body: 'Earth' }],
    spacecraft: [target, chaser],
    bodies: [],
    // mid-window: the event is then within ~100 km of every viewport's focus for the whole run
    events: [{ name: 'Burn', t: EPOCH_A1MJD + 612 / 86400, type: 'maneuver', spacecraft: 'Chaser', detail: null, referenceId: null, attributes: {} }],
    footprints: [],
  };
  if (withEarth) {
    sc.bodies = [{
      name: 'Earth', radius: 6378.137, flattening: 1 / 298.257223563, texture: null, color: '#2e6fbd', central: true,
      t: [target.t[0], target.t[N_SAMPLES - 1]], pos: [0, 0, 0, 0, 0, 0], quat: [0, 0, 0, 1, 0, 0, 0, 1], spinRate: 0,
    }];
  }
  return sc;
}

/** The measurement protocol both the node check and the browser test run: three views
 * (primary, A, B), `ticks` ticks of `dtS` simulated seconds from `t0Days`. */
export const TICKS = 24;
export const DT_S = 1.0;
export const T0_DAYS = EPOCH_A1MJD + 600 / 86400;
export function runTicks(viewer, ticks = TICKS, dtS = DT_S, t0 = T0_DAYS) {
  for (let k = 0; k < ticks; k++) viewer.update(t0 + (k * dtS) / 86400);
}

// ====================================================================== node main
const IS_NODE_MAIN = typeof process !== 'undefined' && process.argv && process.argv[1]
  && process.argv[1].endsWith('entities_viewport_precision_check.mjs');

/** three's render() prologue and its per-object modelViewMatrix line, nothing else. */
export function standInRenderer() {
  return {
    render(scene, camera) {
      if (scene.matrixWorldAutoUpdate === true) scene.updateMatrixWorld();
      if (camera.parent === null && camera.matrixWorldAutoUpdate === true) camera.updateMatrixWorld();
      if (scene.isScene === true) scene.onBeforeRender(this, scene, camera, null);
      scene.traverseVisible((o) => {
        if ((o.isMesh || o.isLine || o.isPoints) && o.layers.test(camera.layers)) {
          o.modelViewMatrix.multiplyMatrices(camera.matrixWorldInverse, o.matrixWorld);
        }
      });
    },
  };
}

const fakeEl = () => ({ style: {}, className: '', textContent: '', remove() {}, classList: { add() {}, remove() {} } });
export const stubControls = () => ({ target: new THREE.Vector3(), minDistance: 1e-3, maxDistance: 1e8, update() {} });

export async function nodeSetup() {
  const fs = await import('node:fs');
  const path = await import('node:path');
  const { fileURLToPath } = await import('node:url');
  const here = path.dirname(fileURLToPath(import.meta.url));
  const fixture = fs.readFileSync(path.join(here, 'fixtures', 'entity_model_fixture.gltf'));
  // `_loadEntityModel` fetches the model relative to the page; serve the fixture from disk,
  // everything else (the glTF's embedded data: URIs) through node's own fetch.
  globalThis.window = { location: { href: 'http://127.0.0.1/' }, devicePixelRatio: 1 };
  const realFetch = globalThis.fetch;
  globalThis.fetch = async (url, ...rest) => {
    if (String(url).endsWith('entity_model_fixture.gltf')) {
      return { ok: true, status: 200, arrayBuffer: async () => fixture.buffer.slice(fixture.byteOffset, fixture.byteOffset + fixture.byteLength) };
    }
    return realFetch(url, ...rest);
  };
  if (typeof globalThis.ProgressEvent === 'undefined') {
    globalThis.ProgressEvent = class ProgressEvent { constructor(type, init = {}) { this.type = type; Object.assign(this, init); } };
  }
  const { LayerManager } = await import('./layers/index.js');
  const { FrameGraph } = await import('./frames.js');
  const { FloatingOrigin } = await import('./origin.js');
  const { PRIMARY_LAYER, resetViewportLayerCounterForTests } = await import('./viewport.js');
  return { LayerManager, FrameGraph, FloatingOrigin, PRIMARY_LAYER, resetViewportLayerCounterForTests };
}

/** A `Viewer` made without its constructor (WebGL), with only the fields the code under test
 * reads; every method on it is the real `Viewer.prototype`'s. */
function makeStandIn(dep, rec, viewSpecs) {
  const v = Object.create(Viewer.prototype);
  v.canvas = { clientWidth: 1900, clientHeight: 800 };
  v.labelLayer = fakeEl();
  v.renderer = standInRenderer();
  v.scene = new THREE.Scene();
  v._originFrameId = 'root'; v._cameraFrameId = 'root';
  v.frameGraph = new dep.FrameGraph();
  v.scene.add(v.frameGraph.root);
  v.frameGraph.addFrame({ id: v._originFrameId });
  v._entitiesGroup = new THREE.Group();
  v.frameGraph.frame(v._originFrameId).object3D.add(v._entitiesGroup);
  v.floatingOrigin = new dep.FloatingOrigin({ globalEnabled: true });
  v.floatingOrigin.setOrigin(v._originFrameId, 0, 0, 0);
  v.camera = new THREE.PerspectiveCamera(45, 1900 / 800, 1e-3, 1e9);
  v.camera.up.set(0, 0, 1);
  v.camera.layers.enable(dep.PRIMARY_LAYER);
  v._entitiesGroup.attach(v.camera);
  v.controls = stubControls();
  v.viewports = new Map();
  v.sun = new THREE.DirectionalLight(); v.ambient = new THREE.AmbientLight();
  v.axes = new THREE.Object3D(); v.grid = new THREE.Object3D();
  v.scenario = null; v.bodies = new Map(); v.spacecraft = new Map(); v.events = []; v.footprints = new Map();
  v.globeLayer = null; v.globeBodyName = null; v.tilesOverlay = null;
  v.layerManager = new dep.LayerManager({ memoryBudgetBytes: 64 * 1024 * 1024 });
  v.entityOptions = { markers: true, trails: true, covarianceEllipsoids: false, keepOutVolumes: false, models: false };
  v._entityMarkerGeometry = new THREE.CircleGeometry(1, 24);
  v._entityGroups = null; v._entityResidency = null;
  v._entityCovarianceMeshes = new Map(); v._entityKeepoutMeshes = new Map();
  v._entityModelEntities = new Map(); v._entityModelLoadTokens = new Map();
  v.options = { labels: false, trail: 'full', stars: true };
  v.focus = null;
  v._focusPrev = new THREE.Vector3(); v._tmp = new THREE.Vector3(); v._tmpAbs = new THREE.Vector3();
  v._tmpAim = new THREE.Vector3(); v._tmpQ = new THREE.Quaternion(); v._sunDir = new THREE.Vector3(0, 0, 0);
  v.fitRadius = 30; v._entityFramed = false;
  // (absent before question 240's fix, which is how this same file measures the BEFORE state)
  if (v._initPlacementState) { v._initPlacementState(); v._installPlacementHook(); }
  v._label = () => fakeEl();
  void rec; void viewSpecs;
  return v;
}

export const settle = async (n = 6) => { for (let i = 0; i < n; i++) await new Promise((r) => setImmediate(r)); };

/** `scenario` (optional): the scene JSON to load instead of `closedFormScenario(radiusKm)` (the
 * bodies check, bodies_viewport_precision_check.mjs, passes scenes with bodies). */
export async function buildViewer(dep, rec, radiusKm, scenario = closedFormScenario(radiusKm)) {
  dep.resetViewportLayerCounterForTests();
  const v = makeStandIn(dep, rec);
  v.setScenario(scenario);
  // two extra viewports (real Viewports; stand-in renderer and controls)
  const mk = (id, aspect) => {
    const vp = v.addViewport(id);
    vp.camera.aspect = aspect; vp.camera.updateProjectionMatrix();
    vp.renderer = standInRenderer();
    vp.controls = stubControls();
    return vp;
  };
  const A = mk('A', 0.6);   // portrait, Focus on the Chaser
  const B = mk('B', 1.6);   // landscape, Focus on the Target
  // wait for the Chaser's model and let the layer manager admit the markers / trails
  v.entityOptions.models = true;
  for (let i = 0; i < 40 && !(v._entityModelEntities.get('Chaser') || {}).modelLoaded; i++) await settle(2);
  for (let i = 0; i < 6; i++) { v.update(T0_DAYS); await settle(2); }
  return { v, A, B };
}

/** Frame the three views (models only, so the extent is the 1.5 m model), then switch every
 * class on. `primaryMode`: 'fit' (origin at the frame origin) or 'framed' (Focus on the Chaser). */
export function arrange(v, A, B, primaryMode) {
  v.entityOptions.markers = true;
  v.entityOptions.covarianceEllipsoids = false; v.entityOptions.keepOutVolumes = false; v.entityOptions.models = true;
  v.update(T0_DAYS); // extents are read from the live scene objects
  v.setFocus(null);  // (fit() keeps the focus; the primary must really be unfocused for 'fit')
  v.fit();
  v.setViewportFocus('A', 'Chaser');
  v.setViewportFocus('B', 'Target');
  if (primaryMode === 'framed') v.setFocus('Chaser');
  for (const cls of Object.keys(v.entityOptions)) v.setEntityClassEnabled(cls, true);
}

// [maxErr_m, maxHop_m] of the PRIMARY view, measured on the code before question 240's fix
// (commit c6c463a) by this same file: the primary's numbers must not change. Deterministic
// (closed-form scenario, f64 arithmetic and Math.fround only), compared at 1e-9 relative.
const BEFORE_PRIMARY = {
  'LEO/fit': {
    sphere: [0.6482591210716747, 0.799979557478262],
    marker: [1.3542339447239091, 1.1299452836018757],
    trail: [1.317182930240791, 0.8341469512392023],
    ellipsoid: [0.6482591210716747, 0.799979557478262],
    keepout: [0.6482591210716747, 0.799979557478262],
    model: [1.0973302644876812, 0.8004928895149562],
    event: [0.5780867170877207, 0],
  },
  'LEO/framed': {
    sphere: [0.000606086078846374, 0.0006984333130463716],
    marker: [0.0016968295042123864, 0.0016968413121003798],
    trail: [0.00203689990887277, 0.0024618194372638767],
    ellipsoid: [0.000606086078846374, 0.0006984333130463716],
    keepout: [0.000606086078846374, 0.0006984333130463716],
    model: [0.000606086078846374, 0.000609300544348873],
    event: [0.003124769514635379, 0.004918300448282446],
  },
  'GEO/fit': {
    sphere: [4.804025473023889, 8.159619524155127],
    marker: [10.989886723003016, 10.963656645358686],
    trail: [9.091110762773113, 9.16049432804023],
    ellipsoid: [4.804025473023889, 8.159619524155127],
    keepout: [4.804025473023889, 8.159619524155127],
    model: [5.0601079797312485, 6.661660509670761],
    event: [1.4425695543912336, 0],
  },
  'GEO/framed': {
    sphere: [0.0006336003499207542, 0.0007359255446610171],
    marker: [0.001708192061152222, 0.002144769418821046],
    trail: [0.0037249639684301212, 0.0036094875266013994],
    ellipsoid: [0.0006336003499207542, 0.0007359255446610171],
    keepout: [0.0006336003499207542, 0.0007359255446610171],
    model: [0.0004518378044997557, 0.0007008008708034442],
    event: [0.0013993155839245519, 0.0017291151646643813],
  },
  'lunar/fit': {
    sphere: [36.459285621183135, 61.82588722963301],
    marker: [41.654132463030635, 61.83047014524352],
    trail: [38.85794791775944, 48.675158614880715],
    ellipsoid: [36.459285621183135, 61.82588722963301],
    keepout: [36.459285621183135, 61.82588722963301],
    model: [37.83947882518136, 61.82588950246257],
    event: [19.311130428585717, 0],
  },
  'lunar/framed': {
    sphere: [0.0005803278017679065, 0.0005469770837569841],
    marker: [0.0023696297573969328, 0.001993930136018202],
    trail: [0.0019581697361021736, 0.0026633784366221934],
    ellipsoid: [0.0005803278017679065, 0.0005469770837569841],
    keepout: [0.0005803278017679065, 0.0005469770837569841],
    model: [0.0005803454957997062, 0.000539953907319633],
    event: [0.000601462567488628, 0.00034623715147990116],
  },
};

export const CLASSES = ['sphere', 'marker', 'trail', 'ellipsoid', 'keepout', 'model', 'event'];
const sameBits = (a, b) => Object.is(a, b);

/** What the primary camera draws must be exactly what `_toLocal` (the pre-fix code) wrote:
 * checked on the scene graph the tick leaves behind (the viewports have drawn since, so
 * this includes the restore after the viewport loop). */
export function primaryIdentity(v) {
  const frame = v._originFrameId;
  const fo = v.floatingOrigin;
  const abs = new THREE.Vector3();
  const bad = [];
  const eq = (what, got, want) => {
    for (let i = 0; i < 3; i++) if (!sameBits(got[i], want[i])) { bad.push(`${what}[${i}] ${got[i]} != ${want[i]}`); return; }
  };
  const wantLocal = (a) => { const r = fo.toRenderSpace(frame, a); return [r.x, r.y, r.z]; };
  const mm = v._entityMarkerMesh;
  eq('markers mesh position', mm.position.toArray(), [0, 0, 0]);
  for (const [name, s] of v.spacecraft) {
    s.interp.at(v._lastT, abs).multiplyScalar(SCALE);
    const w = wantLocal(abs);
    eq(`${name} sphere`, s.marker.position.toArray(), w);
    const i = v._entityMarkerResidentNames.indexOf(name);
    if (i >= 0) eq(`${name} instance`, [mm.instanceMatrix.array[16 * i + 12], mm.instanceMatrix.array[16 * i + 13], mm.instanceMatrix.array[16 * i + 14]], w);
    const cov = v._entityCovarianceMeshes.get(name); if (cov) eq(`${name} ellipsoid`, cov.mesh.position.toArray(), w);
    const ko = v._entityKeepoutMeshes.get(name); if (ko) eq(`${name} keepout`, ko.mesh.position.toArray(), w);
    const me = v._entityModelEntities.get(name); if (me) eq(`${name} model group`, me.group.position.toArray(), w);
    const tr = v._entityTrailLines.get(name);
    if (tr) {
      eq(`${name} trail line position`, tr.line.position.toArray(), [0, 0, 0]);
      const count = tr.line.geometry.drawRange.count;
      const idx = nearestIndex(s.poly.times, v._lastT);
      const p = new THREE.Vector3(s.poly.points[3 * idx], s.poly.points[3 * idx + 1], s.poly.points[3 * idx + 2]).multiplyScalar(SCALE);
      eq(`${name} trail newest vertex`, [tr.positions[3 * (count - 1)], tr.positions[3 * (count - 1) + 1], tr.positions[3 * (count - 1) + 2]], wantLocal(p));
    }
  }
  for (const e of v.events) if (e.interp) eq('event', e.mesh.position.toArray(), wantLocal(e.interp.at(e.data.t, abs).multiplyScalar(SCALE)));
  return bad;
}

async function main() {
  const dep = await nodeSetup();
  const checks = [];
  const check = (name, pass, detail) => checks.push({ name, pass: !!pass, detail });
  const table = {};
  // question 46's bound, 1 cm; AV_PRECISION_BOUND_M overrides it so a test can make the check fail
  const bound = process.env.AV_PRECISION_BOUND_M ? Number(process.env.AV_PRECISION_BOUND_M) : CENTIMETRE_M;
  const hasPlacement = typeof Viewer.prototype._placeSharedEntities === 'function';
  check('Viewer places the shared entity objects per drawing view (Viewer._placeSharedEntities exists)', hasPlacement);

  for (const D of DISTANCES) {
    const rec = new Recorder();
    const { v, A, B } = await buildViewer(dep, rec, D.radiusKm);
    const undo = instrumentViewer(v, rec);
    for (const mode of ['fit', 'framed']) {
      const key = `${D.id}/${mode}`;
      arrange(v, A, B, mode);
      rec.reset();
      runTicks(v);
      const sum = rec.summary();
      const oA = A.floatingOrigin.getOrigin(v._originFrameId), oP = v.floatingOrigin.getOrigin(v._originFrameId);
      const dA = A.camera.getWorldPosition(new THREE.Vector3())
        .distanceTo(v.spacecraft.get('Chaser').interp.at(v._lastT, new THREE.Vector3()).multiplyScalar(SCALE)) * SCENE_UNIT_M;
      const geometry = {
        chaserDistanceFromViewportA_m: dA,
        originDifferenceAvsPrimary: Math.hypot(oA.x - oP.x, oA.y - oP.y, oA.z - oP.z),
        viewportARenderShiftLength: A.renderGroup.position.length(),
      };
      table[key] = { ...sum, geometry };

      // question 46's centimetre bound: every viewport that is framed near the entity, for
      // every class. The primary is held to it when it is framed on the Chaser; with its
      // origin at the frame origin (after fit()) it keeps the pre-fix numbers (below).
      for (const view of mode === 'fit' ? ['A', 'B'] : ['primary', 'A', 'B']) {
        for (const cls of CLASSES) {
          const s = (sum[view] || {})[cls];
          check(`${key}: ${view}/${cls} measured on every tick`, !!s && s.n >= TICKS && s.skipped === 0, s);
          if (!s) continue;
          check(`${key}: ${view}/${cls} max error ${s.maxErr.toExponential(2)} m <= ${bound} m`, s.maxErr <= bound, s);
          check(`${key}: ${view}/${cls} max tick-to-tick hop ${s.maxHop.toExponential(2)} m <= ${bound} m`, s.maxHop <= bound, s);
        }
      }
      // the primary's numbers are the pre-fix numbers
      for (const cls of CLASSES) {
        const s = (sum.primary || {})[cls], want = BEFORE_PRIMARY[key][cls];
        const close = (a, b) => Math.abs(a - b) <= 1e-9 * Math.max(Math.abs(b), 1e-3);
        check(`${key}: primary/${cls} error and hop equal the pre-fix numbers (${want[0].toExponential(3)} / ${want[1].toExponential(3)} m)`,
          !!s && close(s.maxErr, want[0]) && close(s.maxHop, want[1]), { got: s, want });
      }
      check(`${key}: viewport A is framed on the 1.5 m model, ${dA.toFixed(1)} m from the Chaser`, dA > 2 && dA < 40, geometry);
      if (mode === 'fit') {
        check(`${key}: the extra viewports' origins are far from the primary's (${geometry.originDifferenceAvsPrimary.toFixed(1)} scene units): the case the defect needs`,
          geometry.originDifferenceAvsPrimary > 1, geometry);
      }
      if (hasPlacement) {
        const bad = primaryIdentity(v);
        check(`${key}: after the tick the scene graph holds exactly the primary's pre-fix values, bit for bit`, bad.length === 0, bad.slice(0, 5));
        check(`${key}: the tick leaves the primary placed (the restore after the viewport loop)`, v._placedView === null, { placed: v._placedView && v._placedView.id });
      }
    }

    if (hasPlacement && D.id === 'LEO') {
      arrange(v, A, B, 'fit');
      runTicks(v, 2);
      const frame = v._originFrameId;
      // (a) placed for viewport A, the world position of every object is S_A + fround(abs - O_A)
      v._placeEpoch++;
      v._placeSharedEntities(A);
      const abs = v.spacecraft.get('Chaser').interp.at(v._lastT, new THREE.Vector3()).multiplyScalar(SCALE);
      const r = A.floatingOrigin.toRenderSpace(frame, abs);
      const S = A.renderGroup.position;
      const want = [S.x + r.x, S.y + r.y, S.z + r.z];
      const worldOf = (o) => [o.matrixWorld.elements[12], o.matrixWorld.elements[13], o.matrixWorld.elements[14]];
      const near = (g) => Math.hypot(g[0] - want[0], g[1] - want[1], g[2] - want[2]) <= 1e-12;
      const chaser = v.spacecraft.get('Chaser');
      check('placed for viewport A: the Chaser sphere\'s world position is S_A + fround(abs - O_A) to 1e-12 scene units', near(worldOf(chaser.marker)));
      check('placed for viewport A: the model group, ellipsoid and keep-out are there too',
        near(worldOf(v._entityModelEntities.get('Chaser').group)) && near(worldOf(v._entityCovarianceMeshes.get('Chaser').mesh)) && near(worldOf(v._entityKeepoutMeshes.get('Chaser').mesh)));
      const mm = v._entityMarkerMesh, i = v._entityMarkerResidentNames.indexOf('Chaser');
      const inst = new THREE.Vector3(mm.instanceMatrix.array[16 * i + 12], mm.instanceMatrix.array[16 * i + 13], mm.instanceMatrix.array[16 * i + 14]);
      const instWorld = inst.applyMatrix4(mm.matrixWorld).toArray();
      check('placed for viewport A: the instanced marker (Float32 matrix + the mesh\'s f64 shift) is there too', near(instWorld));
      check('placed for viewport A: the Float32 instance matrix holds only the small origin-relative remainder',
        Math.hypot(mm.instanceMatrix.array[16 * i + 12], mm.instanceMatrix.array[16 * i + 13], mm.instanceMatrix.array[16 * i + 14]) < 0.05);
      const tr = v._entityTrailLines.get('Chaser');
      check('placed for viewport A: the trail line carries the f64 shift difference as its own position',
        tr.line.position.distanceTo(A.renderGroup.position.clone().sub(v._entitiesGroup.position)) === 0 && tr.line.position.length() > 1,
        { linePosition: tr.line.position.toArray(), aShift: A.renderGroup.position.toArray(), entitiesShift: v._entitiesGroup.position.toArray(), placed: v._placedView && v._placedView.id });
      // (b) a view that is already placed in this epoch is not written again
      const iv = mm.instanceMatrix.version, tv = tr.line.geometry.attributes.position.version;
      v._placeSharedEntities(A);
      check('already placed for this view in this epoch: no buffer is rewritten (version unchanged)',
        mm.instanceMatrix.version === iv && tr.line.geometry.attributes.position.version === tv);
      v._placeEpoch++;
      v._placeSharedEntities(A);
      check('a new epoch (a tick, a rebase) writes the buffers again (version bumped)',
        mm.instanceMatrix.version === iv + 1 && tr.line.geometry.attributes.position.version === tv + 1);
      // (c) the hook is keyed by the drawing camera
      const seen = [];
      const orig = v._placeSharedEntities;
      v._placeSharedEntities = (vp) => seen.push(vp ? vp.id : null);
      v.scene.onBeforeRender(null, v.scene, v.camera);
      v.scene.onBeforeRender(null, v.scene, A.camera);
      v.scene.onBeforeRender(null, v.scene, B.camera);
      v.scene.onBeforeRender(null, v.scene, new THREE.PerspectiveCamera());
      v._placeSharedEntities = orig;
      check('scene.onBeforeRender places for the drawing camera: primary -> primary, A -> A, B -> B, any other camera -> primary',
        JSON.stringify(seen) === JSON.stringify([null, 'A', 'B', null]), seen);

      // (d) teeth: the old behaviour (every draw placed for the primary's origin) is measured
      // as the defect it was, by this same measurement
      const bk = new Recorder();
      const undoBk = instrumentViewer(v, bk);
      v._onSceneBeforeRender = () => v._placeSharedEntities(null);
      arrange(v, A, B, 'fit');
      bk.reset();
      runTicks(v);
      delete v._onSceneBeforeRender;
      undoBk();
      const sb = bk.summary();
      table['LEO/fit/BREAKS (every draw placed for the primary origin)'] = sb;
      check('BREAKS: placing every draw for the primary\'s origin puts viewport A\'s sphere, marker, trail and model off by more than 10 cm at LEO',
        ['sphere', 'marker', 'trail', 'model'].every((c) => sb.A[c].maxErr > 0.1), sb.A);
      check('BREAKS: ... and the primary\'s numbers are the same either way',
        Math.abs(sb.primary.sphere.maxErr - BEFORE_PRIMARY['LEO/fit'].sphere[0]) < 1e-9);
    }
    undo();
  }

  const allPass = checks.every((c) => c.pass);
  // exit from the write callback: a pipe is flushed asynchronously and the JSON is large
  process.stdout.write(JSON.stringify({ allPass, checks, table }), () => process.exit(allPass ? 0 : 1));
}

if (IS_NODE_MAIN) main();

