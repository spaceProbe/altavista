// CLI harness for tests/test_marker_pixel_size.py: `node web/js/marker_pixel_size_check.mjs`; its
// measurement kernel is also imported by the real-browser proof,
// tests/test_viewer_marker_pixel_size_browser.py.
//
// Question 254 item 3b (heavy open-items round), the marker half of question 239: the spacecraft
// marker spheres and the event octahedra are a FIXED SIZE ON SCREEN (7 px and 9 px across, in the
// view that draws them), at any distance, in every view.
//
// The defect: each is ONE object drawn by every viewport, and `update(t)` scaled it once per tick
// from the CLOSEST camera of all views, measured from the marker's primary-placed position, BEFORE
// `_updateViewport` moved the extra viewports' cameras. With the clock running a close-up
// viewport (cameras follow focus) therefore drew the marker sized for where its camera was one
// tick earlier (thousands of pixels), and a viewport farther than the closest camera drew it
// sub-pixel. `Viewer._sizeMarkers(vp)` now sizes them for the view about to draw, from that view's
// own camera (after it moved), canvas height and fov, at the render-time placement.
//
// What is measured, for each view that draws (the primary and two extra viewports with canvases of
// different heights: A portrait 300 x 500, one ~10 m from the Chaser; B 400 x 640 framed on the
// Target, farther), on EVERY tick of a run with the clock running, right after that view's renderer
// drew: each marker's and event mesh's on-screen radius in pixels from the DRAWN state, i.e.
// the object's world matrix and the camera's, as the GPU receives them (the model-view matrix
// rounded to float32): `r_px = (scale / distance) * (H / 2) * projection[1][1]`, `scale` the
// length of the matrix's first column, `distance` the length of its translation, `H` the view's
// canvas height in CSS pixels. Nothing the sizing computed is used: the declared size (3.5 px and
// 4.5 px radius) is written here from the issue, not read from scene.js.
//
// Tolerance: the drawn radius must be within 0.1% of the declared one. The sizing uses the f64
// camera-to-entity distance (`abs` against the camera's world position) and the draw uses the
// float32-rounded placed position (`fround(abs - O_v)`), so the placement's rounding against a
// camera ~10 m away is a legitimate, bounded difference: measured below as `placementRel`, at most
// 1e-4 of the distance (a few mm at 10 m), which is also the largest deviation seen (0.005%).
// 0.1% is 20x that, so it does not flicker, and ~1e5 times tighter than the defect it replaces
// (100x to 1000x). The observed maxima are printed with the placement's share next to them.
//
// Also checked: with no extra viewport the primary's scales are bit-identical to the code before
// this change (a digest of every scale of every tick, recorded on the previous code), and equal the
// documented formula `max(d * 2 tan(fov/2) / H * PX / 2, 1e-10)` evaluated independently; with
// extra viewports the primary still holds that formula's value between ticks (sized for its own
// camera); a camera that moves or a canvas that resizes within one epoch (after the view was
// placed, before the next tick) is sized again by the next call (the (view, epoch) memo does not
// skip it); and the cost of sizing one view for a tick.
//
// Two ways to use it: `node web/js/marker_pixel_size_check.mjs` (the REAL scene.js `Viewer.prototype`
// on the stand-in of entities_viewport_precision_check.mjs, with canvas stand-ins of the sizes
// above; prints one JSON object, FAIL lines and a `marker_pixel_size_check: N passed, M failed`
// summary on stderr, exits 1 on any failure) and imported by the browser test.
import * as THREE from 'three';
import { SCALE } from './scene.js';
import {
  nodeSetup, buildViewer, arrange, runTicks, instrumentRenderer, disableCulling, DISTANCES, TICKS, T0_DAYS,
} from './entities_viewport_precision_check.mjs';

/** Declared on-screen RADIUS in CSS pixels: the marker is 7 px across, the event octahedron 9 px. */
export const DECLARED_RADIUS_PX = { marker: 3.5, event: 4.5 };
/** Relative tolerance on the drawn radius, see the module docstring. */
export const TOLERANCE = 0.001;
/** The canvas stand-ins of the node run (the browser uses its real canvases): CSS pixels. */
export const NODE_CANVASES = { primary: [1900, 800], A: [300, 500], B: [400, 640] };

const fr = Math.fround;

// ----------------------------------------------------------------------- the measurement
const _inv = new THREE.Matrix4();
const _mv = new THREE.Matrix4();
const _truth = new THREE.Vector3();

/** One object's on-screen radius in `camera`'s view from the drawn state (see the module docstring),
 * plus how far the drawn float32 position is from the f64 truth `abs` (relative to the distance). */
export function drawnRadiusPx(obj, camera, H, abs) {
  _inv.copy(camera.matrixWorld).invert();
  _mv.multiplyMatrices(_inv, obj.matrixWorld);
  const m = Array.from(_mv.elements, fr);
  const scale = Math.hypot(m[0], m[1], m[2]);
  const distance = Math.hypot(m[12], m[13], m[14]);
  const radiusPx = (scale / distance) * (H / 2) * camera.projectionMatrix.elements[5];
  _truth.copy(abs).applyMatrix4(_inv);
  const trueDistance = _truth.length();
  const placementRel = Math.hypot(m[12] - _truth.x, m[13] - _truth.y, m[14] - _truth.z) / trueDistance;
  return { radiusPx, distance, trueDistance, placementRel };
}

/** The canvas height of a view (`null` = the primary). */
export function viewHeight(viewer, view) {
  return (view ? view.canvas : viewer.canvas).clientHeight;
}

/** Per (view, kind, name): the largest relative deviation of the drawn radius from the declared one
 * over the run, where it happened, and the placement's share. */
export class SizeRecorder {
  constructor() { this.stat = new Map(); this.tick = 0; }
  add(view, kind, name, m) {
    const id = `${view}|${kind}|${name}`;
    const declared = DECLARED_RADIUS_PX[kind];
    const dev = Math.abs(m.radiusPx / declared - 1);
    let s = this.stat.get(id);
    if (!s) { s = { view, kind, name, n: 0, maxDev: 0, atTick: -1, radiusAtMax: declared, minPx: Infinity, maxPx: 0, maxPlacementRel: 0, distanceM: m.trueDistance * 1e6 }; this.stat.set(id, s); }
    s.n += 1;
    if (dev > s.maxDev) { s.maxDev = dev; s.atTick = this.tick; s.radiusAtMax = m.radiusPx; }
    s.minPx = Math.min(s.minPx, m.radiusPx); s.maxPx = Math.max(s.maxPx, m.radiusPx);
    s.maxPlacementRel = Math.max(s.maxPlacementRel, m.placementRel);
    s.distanceM = Math.min(s.distanceM, m.trueDistance * 1e6);
  }
  rows() { return [...this.stat.values()]; }
}

/** Wrap the primary's and every viewport's `renderer.render` so each draw measures every spacecraft
 * marker and every event mesh into `rec`. Returns the function that removes the wrappers. */
export function instrumentSizes(viewer, rec) {
  const viewOf = (camera) => {
    if (camera === viewer.camera) return { name: 'primary', view: null };
    for (const [id, vp] of viewer.viewports) if (vp.camera === camera) return { name: id, view: vp };
    return null;
  };
  const onDrawn = (camera) => {
    const v = viewOf(camera);
    if (!v) return;
    const H = viewHeight(viewer, v.view);
    for (const [name, s] of viewer.spacecraft) if (s.marker.visible) rec.add(v.name, 'marker', name, drawnRadiusPx(s.marker, camera, H, s.abs));
    viewer.events.forEach((e, i) => { if (e.mesh.visible && e.interp) rec.add(v.name, 'event', e.data.name || String(i), drawnRadiusPx(e.mesh, camera, H, e.abs)); });
  };
  const undo = [instrumentRenderer(viewer.renderer, onDrawn)];
  for (const vp of viewer.viewports.values()) if (vp.renderer) undo.push(instrumentRenderer(vp.renderer, onDrawn));
  return () => undo.forEach((u) => u());
}

/** `ticks` ticks of the running clock (`dtS` simulated seconds each), measured at every draw. */
export function sizeRun(viewer, { ticks = TICKS, dtS = 1.0, t0 = T0_DAYS } = {}) {
  const rec = new SizeRecorder();
  const undo = instrumentSizes(viewer, rec);
  for (let k = 0; k < ticks; k++) { rec.tick = k; viewer.update(t0 + (k * dtS) / 86400); }
  undo();
  return rec;
}

/** Camera-to-entity distances in metres, as arranged (f64): { view: { Chaser, Target } }. */
export function viewDistancesM(viewer, t) {
  const out = {};
  const views = [['primary', viewer.camera], ...[...viewer.viewports].map(([id, vp]) => [id, vp.camera])];
  for (const [name, camera] of views) {
    const cw = camera.getWorldPosition(new THREE.Vector3());
    out[name] = {};
    for (const [sn, s] of viewer.spacecraft) {
      out[name][sn] = s.interp.at(t, new THREE.Vector3()).multiplyScalar(SCALE).distanceTo(cw) * 1e6;
    }
  }
  return out;
}

// ----------------------------------------------------------------------- primary-only identity
/** FNV-1a over the 8 bytes of each double, in order: a digest two runs share only if every value is
 * bit for bit the same. */
export function digestDoubles(values) {
  const buf = new DataView(new ArrayBuffer(8));
  let h = 0x811c9dc5;
  for (const v of values) {
    buf.setFloat64(0, v);
    for (let i = 0; i < 8; i++) { h ^= buf.getUint8(i); h = Math.imul(h, 0x01000193) >>> 0; }
  }
  return h.toString(16).padStart(8, '0');
}

/** The primary's marker and event scales at the end of each of `ticks` ticks (the state between
 * ticks), in tick, spacecraft, event order. */
export function primaryScales(viewer, { ticks = TICKS, dtS = 1.0, t0 = T0_DAYS } = {}) {
  const out = [];
  for (let k = 0; k < ticks; k++) {
    viewer.update(t0 + (k * dtS) / 86400);
    for (const s of viewer.spacecraft.values()) out.push(s.marker.scale.x);
    for (const e of viewer.events) out.push(e.mesh.scale.x);
  }
  return out;
}

/** The documented primary sizing, stated independently: `max(d * (2 tan(fov/2) / H) * PX * 0.5, 1e-10)`
 * with `d` the distance from the object's world position to the primary camera's, PX 7 (marker) or 9
 * (event). Evaluated on the scene graph a tick leaves, it must reproduce `scale.x` bit for bit. */
export function formulaMismatches(viewer, h = viewer.canvas.clientHeight) {
  const bad = [];
  const cam = viewer.camera;
  const cw = cam.getWorldPosition(new THREE.Vector3());
  const pxScale = 2 * Math.tan(THREE.MathUtils.degToRad(cam.fov / 2)) / h;
  const want = (obj, px) => Math.max(obj.getWorldPosition(new THREE.Vector3()).distanceTo(cw) * pxScale * px * 0.5, 1e-10);
  for (const [name, s] of viewer.spacecraft) { const w = want(s.marker, 7); if (!Object.is(w, s.marker.scale.x)) bad.push({ name, got: s.marker.scale.x, want: w }); }
  viewer.events.forEach((e, i) => { if (!e.interp) return; const w = want(e.mesh, 9); if (!Object.is(w, e.mesh.scale.x)) bad.push({ event: i, got: e.mesh.scale.x, want: w }); });
  return bad;
}

/** Digests of `primaryScales` recorded on the code BEFORE this change (commit a09d5b7, primary only,
 * 24 ticks of 1 s, closed-form scenario: deterministic f64 arithmetic). */
export const BEFORE_PRIMARY_ONLY = {
  'LEO/fit': 'eabe0b01', 'LEO/framed': '8dc5603c', 'GEO/fit': 'c8be1cdc',
  'GEO/framed': '07560189', 'lunar/fit': '29a67d57', 'lunar/framed': '20bf5105',
};

// ----------------------------------------------------------------------- within one epoch
/**
 * The (view, epoch) memo of `_placeSharedEntities` must not hide a camera that moved or a canvas
 * that resized after the view was placed in the current epoch. On a viewer ticked and left with
 * the primary placed: place `view` (null = the primary), move its camera 3x farther from the
 * Chaser along the line to it and change its canvas height, call `_placeSharedEntities(view)` again
 * (same epoch: a memo hit) and measure the Chaser marker as drawn. Restores camera, canvas and the
 * primary's placement. Returns the radius before and after in pixels.
 */
export function moveWithinEpoch(viewer, view, name = 'Chaser') {
  const camera = view ? view.camera : viewer.camera;
  const owner = view ? view : viewer;
  const s = viewer.spacecraft.get(name);
  const canvas = owner.canvas;
  const H0 = canvas.clientHeight;
  // a DOM canvas is resized through its style (clientHeight is read-only), a stand-in's property is set
  const styleHeight = canvas.style ? canvas.style.height : null;
  const setHeight = (h) => { if (canvas.style) canvas.style.height = `${h}px`; else canvas.clientHeight = h; };
  const posBefore = camera.position.clone();
  viewer._placeSharedEntities(view);
  const epoch = viewer._placeEpoch;
  camera.updateMatrixWorld(true);
  const r0 = drawnRadiusPx(s.marker, camera, H0, s.abs).radiusPx;
  // 3x as far from the marker along the line from it, in the camera's parent space
  const marker = s.marker.getWorldPosition(new THREE.Vector3());
  const cw = camera.getWorldPosition(new THREE.Vector3());
  const farther = cw.clone().sub(marker).multiplyScalar(3).add(marker);
  camera.position.copy(camera.parent ? camera.parent.worldToLocal(farther) : farther);
  camera.updateMatrixWorld(true);
  setHeight(Math.round(H0 / 2));
  const memoHit = viewer._placedView === view && viewer._placedEpoch === viewer._placeEpoch;
  viewer._placeSharedEntities(view);
  const r1 = drawnRadiusPx(s.marker, camera, canvas.clientHeight, s.abs).radiusPx;
  const sameEpoch = viewer._placeEpoch === epoch;
  camera.position.copy(posBefore);
  camera.updateMatrixWorld(true);
  if (canvas.style) canvas.style.height = styleHeight; else canvas.clientHeight = H0;
  viewer._placeSharedEntities(null);
  return { r0, r1, memoHit, sameEpoch };
}

/** Median / p95 / max, in milliseconds, of one `_sizeMarkers(view)` (the memo-hit path every draw
 * takes; nothing changed, so nothing is rewritten) over `n` batches of `batch` calls. */
export function timeSizing(viewer, view, n = 40, batch = 500) {
  const ts = [];
  for (let i = 0; i < 20; i++) viewer._sizeMarkers(view);
  for (let i = 0; i < n; i++) {
    const a = performance.now();
    for (let k = 0; k < batch; k++) viewer._sizeMarkers(view);
    ts.push((performance.now() - a) / batch);
  }
  ts.sort((x, y) => x - y);
  return { calls: n * batch, median: ts[n >> 1], p95: ts[Math.floor(n * 0.95)], max: ts[n - 1] };
}

/** The assertions on `moveWithinEpoch` results (`mine`: for the view named `view`). */
export function judgeWithinEpoch(key, view, m, check) {
  check(`${key}: ${view}: a camera moved 3x farther and a canvas halved within one epoch (a memo hit: ${m.memoHit}, same epoch: ${m.sameEpoch}) is sized again: ${m.r0.toFixed(4)} px before, ${m.r1.toFixed(4)} px after, declared 3.5`,
    m.memoHit && m.sameEpoch && Math.abs(m.r0 / 3.5 - 1) < TOLERANCE && Math.abs(m.r1 / 3.5 - 1) < TOLERANCE, m);
}

// ----------------------------------------------------------------------- the judging
export function giveCanvases(viewer, A, B) {
  viewer.canvas = { clientWidth: NODE_CANVASES.primary[0], clientHeight: NODE_CANVASES.primary[1] };
  A.canvas = { clientWidth: NODE_CANVASES.A[0], clientHeight: NODE_CANVASES.A[1] };
  B.canvas = { clientWidth: NODE_CANVASES.B[0], clientHeight: NODE_CANVASES.B[1] };
}

/** Assertions on one `SizeRecorder`: every row within tolerance; the expected rows exist. */
export function judgeSizes(key, rec, check, { views = ['primary', 'A', 'B'] } = {}) {
  const rows = rec.rows();
  for (const view of views) {
    for (const kind of ['marker', 'event']) {
      const mine = rows.filter((r) => r.view === view && r.kind === kind);
      const worst = mine.reduce((a, r) => (r.maxDev > a.maxDev ? r : a), { maxDev: -1 });
      check(`${key}: ${view}: ${kind} drawn within ${(TOLERANCE * 100).toFixed(1)}% of ${DECLARED_RADIUS_PX[kind]} px radius on all ticks (${mine.length} objects, ${mine.reduce((n, r) => n + r.n, 0)} draws; worst ${(worst.maxDev * 100).toFixed(3)}% = ${worst.radiusAtMax === undefined ? '' : worst.radiusAtMax.toPrecision(5)} px at tick ${worst.atTick})`,
        mine.length >= (kind === 'marker' ? 2 : 1) && mine.every((r) => r.n >= TICKS && r.maxDev <= TOLERANCE), mine.filter((r) => r.maxDev > TOLERANCE).slice(0, 3));
    }
  }
}

// ====================================================================== node main
const IS_NODE_MAIN = typeof process !== 'undefined' && process.argv && process.argv[1]
  && process.argv[1].endsWith('marker_pixel_size_check.mjs');

/** A primary-only viewer in the same arrangement as `arrange(v, A, B, mode)` gives the primary. */
export async function primaryOnlyViewer(dep, rec, radiusKm, mode) {
  const { v, A, B } = await buildViewer(dep, rec, radiusKm);
  for (const vp of [A, B]) { vp.renderer.dispose = () => {}; vp.controls.dispose = () => {}; }
  v.removeViewport('A'); v.removeViewport('B');
  v.canvas = { clientWidth: NODE_CANVASES.primary[0], clientHeight: NODE_CANVASES.primary[1] };
  v.entityOptions.markers = true; v.entityOptions.models = true;
  v.update(T0_DAYS);
  v.setFocus(null);
  v.fit();
  if (mode === 'framed') v.setFocus('Chaser');
  void A; void B;
  return v;
}

async function main() {
  const dep = await nodeSetup();
  const checks = [];
  const check = (name, pass, detail) => checks.push({ name, pass: !!pass, detail });
  const table = {};
  const digests = {};
  const wantDigests = process.argv.includes('--print-digests');
  for (const D of DISTANCES) {
    for (const mode of ['fit', 'framed']) {
      const key = `${D.id}/${mode}`;
      const { v, A, B } = await buildViewer(dep, null, D.radiusKm);
      giveCanvases(v, A, B);
      arrange(v, A, B, mode);
      disableCulling(v);
      const dist = viewDistancesM(v, T0_DAYS);
      const rec = sizeRun(v);
      table[key] = { distancesM: dist, rows: rec.rows() };
      judgeSizes(key, rec, check);
      check(`${key}: viewport A is ~10 m from the Chaser (${dist.A.Chaser.toFixed(1)} m) and B is farther from the Target (${dist.B.Target.toExponential(2)} m)`,
        dist.A.Chaser > 2 && dist.A.Chaser < 40 && dist.B.Target > 5 * dist.A.Chaser, dist);
      // what the tick leaves in the scene graph is the primary's sizing, for its own camera
      const leftBad = formulaMismatches(v);
      check(`${key}: with two extra viewports the tick leaves the primary's own sizing in the scene graph (${leftBad.length} differ from the formula for the primary's camera)`, leftBad.length === 0, leftBad.slice(0, 3));
      // a camera moved / a canvas resized within one epoch is sized again by the next call
      for (const [name, view] of [['primary', null], ['A', A], ['B', B]]) judgeWithinEpoch(key, name, moveWithinEpoch(v, view), check);
      const cost = { primary: timeSizing(v, null), A: timeSizing(v, A), B: timeSizing(v, B) };
      table[key].cost = cost;
      check(`${key}: sizing one view for a tick costs ${Math.max(cost.primary.median, cost.A.median, cost.B.median).toFixed(5)} ms at most (median; budget 0.05 ms)`,
        Math.max(cost.primary.median, cost.A.median, cost.B.median) < 0.05, cost);
      // the primary alone: bit-identical to the code before this change
      const solo = await primaryOnlyViewer(dep, null, D.radiusKm, mode);
      const scales = primaryScales(solo);
      digests[key] = digestDoubles(scales);
      const bad = formulaMismatches(solo);
      check(`${key}: primary only: every scale of ${scales.length} (24 ticks) is bit for bit the documented formula's (${bad.length} differ)`, bad.length === 0 && scales.length > 0, bad.slice(0, 3));
      if (!wantDigests) {
        check(`${key}: primary only: the digest of all ${scales.length} scales equals the one recorded before the change (${BEFORE_PRIMARY_ONLY[key]})`, digests[key] === BEFORE_PRIMARY_ONLY[key], digests[key]);
      }
    }
  }
  const failed = checks.filter((c) => !c.pass);
  const allPass = failed.length === 0;
  if (wantDigests) process.stderr.write(`digests: ${JSON.stringify(digests)}\n`);
  for (const [key, t] of Object.entries(table)) {
    for (const r of t.rows) process.stderr.write(`  ${key} ${r.view} ${r.kind} ${r.name}: dist ${r.distanceM.toExponential(2)} m, radius ${r.minPx.toPrecision(5)}..${r.maxPx.toPrecision(5)} px, max dev ${(r.maxDev * 100).toFixed(3)}%, placement ${r.maxPlacementRel.toExponential(1)}\n`);
  }
  for (const c of failed) process.stderr.write(`FAIL ${c.name}: ${(JSON.stringify(c.detail) ?? '').slice(0, 400)}\n`);
  process.stderr.write(`marker_pixel_size_check: ${checks.length - failed.length} passed, ${failed.length} failed (tolerance ${TOLERANCE * 100}%)\n`);
  process.stdout.write(JSON.stringify({ allPass, checks, table, digests }), () => process.exit(allPass ? 0 : 1));
}

if (IS_NODE_MAIN) main();
