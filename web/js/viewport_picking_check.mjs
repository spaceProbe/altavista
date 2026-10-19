// CLI harness for tests/test_viewport_picking.py: `node web/js/viewport_picking_check.mjs`;
// its measurement kernel is also imported by the real-browser proof,
// tests/test_viewer_viewport_picking_browser.py.
//
// Question 254 item 3 (docs/heavy-plan.md, heavy open-items round): picking in an extra viewport
// uses THAT viewport's own placement of the shared entity objects.
//
// The defect: since question 252 the shared entity objects (spacecraft marker spheres, event
// meshes, glTF models, ...) are written per drawing view at render time
// (`Viewer._placeSharedEntities`), but between ticks the scene graph holds the PRIMARY's
// placement (`update(t)` restores it), and `Viewer.pick(camera, ...)` raycast that state whatever
// camera it was given. An extra viewport framed ~10 m from the 1.5 m Chaser model draws the
// model where the viewport's own float32 placement puts it (cm-exact); the primary's placement
// is 0.26-0.74 m away from that at LEO, 1.9-5.1 m at GEO, 15-54 m at lunar distance (the
// precision round's BEFORE table), so a click on the model as drawn could miss it, or hit the
// wrong thing. And the model's meshes were not a pick target at all: only the marker, a 7 px
// dot at the model's origin.
//
// What is measured, per distance (LEO 7,000 km, GEO 42,164 km, lunar 384,400 km) and per view
// (viewport A, Focus on the Chaser; viewport B, Focus on the Target; the primary after a Focus
// on the Chaser), through the real `Viewer.pick(view.camera, ndcX, ndcY)`:
//   * the TRUTH of where the Chaser's model is on screen: every model vertex through its node
//     chain, the entity's attitude and the spacecraft's float64 interpolator position (x SCALE),
//     into the view's own camera (`Vector3.project`, float64). Nothing the placement wrote is
//     used for the truth.
//   * picks at the model's true projected origin and at 12 points across its true silhouette
//     (4 barycentric points in each of its 3 triangles) -> each must resolve to the Chaser;
//   * picks 3 model-widths off the true silhouette in 8 directions (those inside the NDC
//     square) -> none may resolve to the Chaser;
//   * for the Target (no model): the true projected position and four points inside its
//     marker disc as drawn -> the Target;
//   * pick() leaves the scene graph exactly as it found it (every object's position, quaternion,
//     scale and world matrix, the instance-matrix and trail buffers: bit for bit, `Object.is`);
//   * the primary's picks are what they were: wherever the old target list (bodies, markers,
//     events) hit something, the new pick agrees;
//   * the cost of one pick (median over many).
// The node mode runs the REAL scene.js code on a stand-in Viewer (see
// entities_viewport_precision_check.mjs: only the renderers, DOM and OrbitControls are stand-ins);
// it prints one JSON object and a summary line and exits non-zero when any check fails.
import * as THREE from 'three';
import { SCALE } from './scene.js';
import { pickAlongCamera } from './viewport.js';
import {
  nodeSetup, buildViewer, arrange, runTicks, instrumentViewer, Recorder, DISTANCES,
} from './entities_viewport_precision_check.mjs';

const IN_SAMPLES = [[1 / 3, 1 / 3, 1 / 3], [0.6, 0.2, 0.2], [0.2, 0.6, 0.2], [0.2, 0.2, 0.6]];
const OFF_DIRECTIONS = [[1, 0], [-1, 0], [0, 1], [0, -1], [1, 1], [-1, 1], [1, -1], [-1, -1]];
const OFF_MODEL_WIDTHS = [3, 2, 1.5, 1];

// ----------------------------------------------------------------------- scene-graph snapshot
/** Every number a pick could disturb, as one flat array: for each object under the scene its
 * position, quaternion, scale, visibility and world matrix, plus the instance matrices and the
 * trail position buffers. Two snapshots are equal bit for bit (`Object.is`) or the pick left a
 * trace in the scene graph. */
export function sceneState(viewer) {
  const out = [];
  const push = (arr) => { for (let i = 0; i < arr.length; i++) out.push(arr[i]); };
  viewer.scene.traverse((o) => {
    out.push(o.visible ? 1 : 0);
    push(o.position.toArray()); push(o.quaternion.toArray()); push(o.scale.toArray());
    push(o.matrixWorld.elements);
    if (o.isInstancedMesh) push(o.instanceMatrix.array);
    if (o.geometry && o.geometry.attributes && o.geometry.attributes.position && !o.isInstancedMesh) {
      // the trail lines' preallocated buffers (the only per-view-written vertex data)
      for (const rec of viewer._entityTrailLines.values()) if (rec.line === o) push(rec.positions);
    }
  });
  return out;
}

/** Index and values of the first difference between two snapshots, or null. */
export function firstDifference(a, b) {
  if (a.length !== b.length) return { length: [a.length, b.length] };
  for (let i = 0; i < a.length; i++) if (!Object.is(a[i], b[i])) return { index: i, before: a[i], after: b[i] };
  return null;
}

// ----------------------------------------------------------------------- the truth
/** The Chaser's model triangles projected into `camera` (float64 truth): [{ndc:[Vector3 x3]}],
 * plus the model origin's projection. Returns null when the model is not loaded/visible. */
export function modelTruth(viewer, camera, name = 'Chaser') {
  const s = viewer.spacecraft.get(name);
  const me = viewer._entityModelEntities.get(name);
  if (!s || !me || !me.modelLoaded || !me.group.visible) return null;
  const abs = s.interp.at(viewer._lastT, new THREE.Vector3()).multiplyScalar(SCALE);
  me.group.updateMatrix();
  const tris = [];
  me.group.traverse((o) => {
    if (!o.isMesh) return;
    const chain = new THREE.Matrix4();
    for (let n = o; n !== me.group; n = n.parent) { n.updateMatrix(); chain.premultiply(n.matrix); }
    const pos = o.geometry.attributes.position;
    const idx = o.geometry.index;
    const vertex = (i) => new THREE.Vector3(pos.getX(i), pos.getY(i), pos.getZ(i))
      .applyMatrix4(chain).applyQuaternion(me.group.quaternion).add(abs).project(camera);
    const count = idx ? idx.count : pos.count;
    for (let k = 0; k + 2 < count; k += 3) {
      const ids = idx ? [idx.getX(k), idx.getX(k + 1), idx.getX(k + 2)] : [k, k + 1, k + 2];
      tris.push(ids.map(vertex));
    }
  });
  return { tris, origin: abs.clone().project(camera), abs };
}

/** Per-pick evidence that a pick leaves the scene graph as it found it. `tracedPick` snapshots
 * (`sceneState`) and records `_placedView` immediately before and after EACH single pick, with
 * nothing in between (no update(), no other pick), and logs any difference in `trace.changed`.
 * `pickingRun` resets `trace` and reports it. */
export const trace = { picks: 0, changed: [] };
export function resetTrace() { trace.picks = 0; trace.changed = []; }

export function tracedPick(viewer, camera, x, y) {
  const stateBefore = sceneState(viewer), placedBefore = viewer._placedView;
  const hit = viewer.pick(camera, x, y);
  const diff = firstDifference(stateBefore, sceneState(viewer));
  trace.picks += 1;
  if (diff || viewer._placedView !== placedBefore) {
    trace.changed.push({ ndc: [x, y], diff, placedBefore: String(placedBefore && placedBefore.id), placedAfter: String(viewer._placedView && viewer._placedView.id) });
  }
  return hit;
}

const isSpacecraft = (hit, name) => !!hit && hit.kind === 'spacecraft' && hit.name === name;
const inSquare = (p) => Math.abs(p.x) <= 1 && Math.abs(p.y) <= 1 && p.z > -1 && p.z < 1;

/**
 * Pick through `view.camera` (`view` null = the primary) at the Chaser model's true projected
 * origin and across its true silhouette, and well off it. Returns counts and the misses.
 */
export function modelPickProof(viewer, view, name = 'Chaser') {
  const camera = view ? view.camera : viewer.camera;
  const truth = modelTruth(viewer, camera, name);
  if (!truth) return { error: 'model not loaded' };
  const record = (list, label, p, expectHit) => {
    const hit = tracedPick(viewer, camera, p.x, p.y);
    list.push({ label, ndc: [p.x, p.y], hit, ok: isSpacecraft(hit, name) === expectHit });
  };
  // inside: the origin and 4 barycentric points in each triangle
  const inside = [];
  if (inSquare(truth.origin)) record(inside, 'origin', truth.origin, true);
  truth.tris.forEach((tri, ti) => {
    IN_SAMPLES.forEach((w, wi) => {
      const p = new THREE.Vector3().addScaledVector(tri[0], w[0]).addScaledVector(tri[1], w[1]).addScaledVector(tri[2], w[2]);
      if (inSquare(p)) record(inside, `tri${ti}/${wi}`, p, true);
    });
  });
  // the silhouette's centre and extent, in half-height units (x scaled by the aspect)
  const aspect = camera.aspect;
  let x0 = Infinity, x1 = -Infinity, y0 = Infinity, y1 = -Infinity;
  for (const tri of truth.tris) for (const p of tri) {
    x0 = Math.min(x0, p.x * aspect); x1 = Math.max(x1, p.x * aspect); y0 = Math.min(y0, p.y); y1 = Math.max(y1, p.y);
  }
  const cx = (x0 + x1) / 2, cy = (y0 + y1) / 2, w = x1 - x0, h = y1 - y0, width = Math.max(w, h);
  // Points `m` model-widths beyond the silhouette's bounding box along each of 8 directions: the
  // largest m in OFF_MODEL_WIDTHS (3, then 2 ...) for which at least 4 of them are inside the
  // viewport (a portrait pane that the model fills has no room for 3 widths).
  const off = [];
  let offWidths = 0;
  for (const m of OFF_MODEL_WIDTHS) {
    const pts = [];
    for (const [dx, dy] of OFF_DIRECTIONS) {
      const px = cx + dx * (w / 2 + m * width), py = cy + dy * (h / 2 + m * width);
      const p = new THREE.Vector3(px / aspect, py, 0);
      if (Math.abs(p.x) <= 1 && Math.abs(p.y) <= 1) pts.push([`off${m}(${dx},${dy})`, p]);
    }
    if (pts.length >= 4) { offWidths = m; for (const [label, p] of pts) record(off, label, p, false); break; }
  }
  return {
    view: view ? view.id : 'primary',
    silhouetteWidthNdcHalfHeights: width,
    inside: { total: inside.length, ok: inside.filter((r) => r.ok).length, failed: inside.filter((r) => !r.ok), points: inside.map((r) => r.ndc) },
    off: { widths: offWidths, total: off.length, ok: off.filter((r) => r.ok).length, failed: off.filter((r) => !r.ok), points: off.map((r) => r.ndc) },
  };
}

/** Radius, in pixels, at which the spacecraft's marker is drawn in `view` (float64 distance). The
 * marker is ONE object sized for the view it is placed for (`Viewer._sizeMarkers`, written by
 * `_placeSharedEntities(view)`, as a pick or a draw does), so it is read with the shared objects
 * placed for `view`, and the placement current on entry is written back. */
export function markerRadiusPx(viewer, view, name) {
  const camera = view ? view.camera : viewer.camera;
  const s = viewer.spacecraft.get(name);
  const before = viewer._placedView;
  viewer._placeSharedEntities(view);
  const scale = s.marker.scale.x;
  if (before !== view) viewer._placeSharedEntities(before === undefined ? null : before);
  const abs = s.interp.at(viewer._lastT, new THREE.Vector3()).multiplyScalar(SCALE);
  const dist = abs.distanceTo(camera.getWorldPosition(new THREE.Vector3()));
  const H = (view && view.canvas && view.canvas.clientHeight) || (viewer.canvas && viewer.canvas.clientHeight) || 600;
  return (scale / dist) * (H / 2) / Math.tan(THREE.MathUtils.degToRad(camera.fov / 2));
}

/** Pick through `view.camera` at the true projected position of `name`'s marker and at four
 * points inside its disc as drawn (half its radius out). */
export function markerPickProof(viewer, view, name = 'Target') {
  const camera = view ? view.camera : viewer.camera;
  const s = viewer.spacecraft.get(name);
  const abs = s.interp.at(viewer._lastT, new THREE.Vector3()).multiplyScalar(SCALE);
  const c = abs.clone().project(camera);
  const rPx = markerRadiusPx(viewer, view, name);
  const H = (view && view.canvas && view.canvas.clientHeight) || (viewer.canvas && viewer.canvas.clientHeight) || 600;
  const dNdc = (0.5 * rPx) / (H / 2); // half-height units
  const pts = [['centre', c.x, c.y], ['+x', c.x + dNdc / camera.aspect, c.y], ['-x', c.x - dNdc / camera.aspect, c.y],
    ['+y', c.x, c.y + dNdc], ['-y', c.x, c.y - dNdc]];
  const results = pts.filter(([, x, y]) => inSquare({ x, y, z: c.z }))
    .map(([label, x, y]) => { const hit = tracedPick(viewer, camera, x, y); return { label, ndc: [x, y], hit, ok: isSpacecraft(hit, name) }; });
  return { view: view ? view.id : 'primary', radiusPx: rPx, total: results.length, ok: results.filter((r) => r.ok).length, failed: results.filter((r) => !r.ok) };
}

/** The pick of the code before question 254 item 3 (bodies, markers, events; no model, no
 * placement), on whatever the scene graph holds now. */
export function legacyPick(viewer, camera, ndcX, ndcY) {
  const targets = [];
  for (const b of viewer.bodies.values()) if (b.visible && b.mesh.visible) targets.push(b.mesh);
  for (const s of viewer.spacecraft.values()) if (s.visible && s.marker.visible) targets.push(s.marker);
  for (const e of viewer.events) if (e.mesh.visible) targets.push(e.mesh);
  const hit = pickAlongCamera(camera, ndcX, ndcY, targets);
  if (!hit) return null;
  for (const [name, b] of viewer.bodies) if (b.mesh === hit) return { kind: 'body', name };
  for (const [name, s] of viewer.spacecraft) if (s.marker === hit) return { kind: 'spacecraft', name };
  for (const e of viewer.events) if (e.mesh === hit) return { kind: 'event', name: e.data.name };
  return null;
}

/** On the primary camera's own placement: wherever the old target list hits something, the new
 * pick agrees (a 41 x 41 grid of rays, plus the rays through each marker's centre) -- except
 * that a glTF model nearer than what the old list hit (the Earth behind a framed Chaser) now
 * wins, resolving to its spacecraft: `modelInFront` counts those, and they count as agreeing
 * (the new target list is the old one plus the model meshes, so the nearest hit can only differ
 * when a model mesh is nearer). */
export function primaryAgreesWithLegacy(viewer) {
  const camera = viewer.camera;
  const rays = [];
  for (let i = 0; i <= 40; i++) for (let j = 0; j <= 40; j++) rays.push([i / 20 - 1, j / 20 - 1]);
  for (const s of viewer.spacecraft.values()) {
    // through the marker where the scene graph holds it (the primary's placement), which is
    // where the old pick's ray lands on it
    const p = s.marker.getWorldPosition(new THREE.Vector3()).project(camera);
    if (inSquare(p)) rays.push([p.x, p.y]);
  }
  let legacyHits = 0, agree = 0, modelInFront = 0;
  const disagree = [];
  for (const [x, y] of rays) {
    const old = legacyPick(viewer, camera, x, y);
    if (!old) continue;
    legacyHits += 1;
    const now = viewer.pick(camera, x, y);
    if (now && now.kind === old.kind && now.name === old.name) agree += 1;
    else if (now && now.kind === 'spacecraft' && (viewer._entityModelEntities.get(now.name) || {}).modelLoaded) { agree += 1; modelInFront += 1; }
    else disagree.push({ ndc: [x, y], old, now });
  }
  return { rays: rays.length, legacyHits, agree, modelInFront, disagree: disagree.slice(0, 3) };
}

/** Median / p95 / max, in milliseconds per pick, of `n` batches of `batch` picks through `camera`
 * at `ndc` (a batch mean, because `performance.now` is coarsened to 0.1 ms in a page). */
export function timePicks(viewer, camera, ndc, n = 60, batch = 50) {
  for (let i = 0; i < 20; i++) viewer.pick(camera, ndc[0], ndc[1]);
  const ts = [];
  for (let i = 0; i < n; i++) {
    const a = performance.now();
    for (let k = 0; k < batch; k++) viewer.pick(camera, ndc[0], ndc[1]);
    ts.push((performance.now() - a) / batch);
  }
  ts.sort((a, b) => a - b);
  return { n: n * batch, median: ts[n >> 1], p95: ts[Math.floor(n * 0.95)], max: ts[n - 1] };
}

/**
 * The whole proof on a viewer that has been `arrange`d and ticked: per-view model / marker pick
 * results, the scene-graph-unchanged check, the primary check and the pick cost. `mode` is the
 * primary's mode ('fit' or 'framed'). Returns plain data.
 */
export function pickingRun(viewer, A, B, mode) {
  const out = {
    mode,
    // marker radius as the view draws it right after the moving ticks, the clock running (before
    // question 254 item 3b the marker was sized once per tick from the cameras' positions of the
    // previous frame, far larger than 3.5 px in a close-up viewport; asserted in `judge`)
    markerRadiusPxWhileRunning: { A_Chaser: markerRadiusPx(viewer, A, 'Chaser'), B_Target: markerRadiusPx(viewer, B, 'Target') },
  };
  // the steady state: one more tick at the same clock value (a paused clock, or any slow playback),
  // after which the cameras and the marker sizes agree
  viewer.update(viewer._lastT);
  const before = sceneState(viewer);
  const placedBefore = viewer._placedView;
  out.placedBefore = placedBefore === null ? 'primary' : String(placedBefore && placedBefore.id);
  resetTrace();
  out.A = modelPickProof(viewer, A);
  out.B = markerPickProof(viewer, B);
  out.markerRadiusPx = { A_Chaser: markerRadiusPx(viewer, A, 'Chaser'), B_Target: markerRadiusPx(viewer, B, 'Target') };
  out.primary = mode === 'framed' ? modelPickProof(viewer, null) : null;
  out.perPick = { picks: trace.picks, changed: trace.changed.slice(0, 3), changedCount: trace.changed.length };
  // a camera that is none of the viewports' picks against the primary
  const unknown = new THREE.PerspectiveCamera(45, 1, 1e-3, 1e9);
  viewer.pick(unknown, 0, 0);
  out.legacy = primaryAgreesWithLegacy(viewer);
  out.unchanged = firstDifference(before, sceneState(viewer));
  out.placedAfter = viewer._placedView === null ? 'primary' : String(viewer._placedView && viewer._placedView.id);
  const centreA = A.camera ? modelTruth(viewer, A.camera).origin : null;
  out.cost = {
    A: timePicks(viewer, A.camera, [centreA.x, centreA.y]),
    primary: timePicks(viewer, viewer.camera, [0, 0]),
  };
  return out;
}

/** The assertions both the node check and the browser test make on one `pickingRun`. */
export function judge(key, r, check) {
  for (const [view, p] of [['A', r.A], ['primary', r.primary]]) {
    if (!p) continue;
    check(`${key}: ${view}: the model's true projected origin and ${p.inside.total - 1} points across its true silhouette all resolve to the Chaser (${p.inside.ok}/${p.inside.total})`,
      p.inside.total >= 13 && p.inside.ok === p.inside.total, p.inside.failed.slice(0, 3));
    check(`${key}: ${view}: ${p.off.total} points ${p.off.widths} model-widths off the true silhouette do not resolve to the Chaser (${p.off.ok}/${p.off.total})`,
      p.off.total >= 4 && p.off.ok === p.off.total, p.off.failed.slice(0, 3));
  }
  check(`${key}: while the clock runs the markers are drawn at their declared 3.5 px radius in the view that draws them (A's Chaser ${r.markerRadiusPxWhileRunning.A_Chaser.toFixed(4)}, B's Target ${r.markerRadiusPxWhileRunning.B_Target.toFixed(4)})`,
    [r.markerRadiusPxWhileRunning.A_Chaser, r.markerRadiusPxWhileRunning.B_Target].every((px) => Math.abs(px / 3.5 - 1) < 0.01), r.markerRadiusPxWhileRunning);
  check(`${key}: B: the Target's marker, picked at its true projected position and inside its disc as drawn (${r.B.radiusPx.toExponential(2)} px radius), resolves to the Target (${r.B.ok}/${r.B.total})`,
    r.B.total >= 3 && r.B.ok === r.B.total, r.B.failed.slice(0, 3));
  check(`${key}: EACH of ${r.perPick.picks} single picks (viewport A, B, primary), snapshot taken immediately before and after it, leaves every position, quaternion, scale, world matrix, instance matrix and trail buffer bit for bit as it was and _placedView unchanged (${r.perPick.changedCount} changed)`,
    r.perPick.picks >= 20 && r.perPick.changedCount === 0, r.perPick.changed);
  check(`${key}: pick() leaves the scene graph exactly as it found it, bit for bit (whole run)`, r.unchanged === null, r.unchanged);
  check(`${key}: the primary is the placed view after the picks (it was before)`, r.placedBefore === 'primary' && r.placedAfter === 'primary', [r.placedBefore, r.placedAfter]);
  check(`${key}: the primary camera's picks agree with the old target list wherever it hit something (${r.legacy.agree}/${r.legacy.legacyHits})`,
    r.legacy.legacyHits > 0 && r.legacy.agree === r.legacy.legacyHits, r.legacy.disagree);
}

// ====================================================================== node main
const IS_NODE_MAIN = typeof process !== 'undefined' && process.argv && process.argv[1]
  && process.argv[1].endsWith('viewport_picking_check.mjs');

async function main() {
  const dep = await nodeSetup();
  const checks = [];
  const check = (name, pass, detail) => checks.push({ name, pass: !!pass, detail });
  const table = {};
  for (const D of DISTANCES) {
    const rec = new Recorder();
    const { v, A, B } = await buildViewer(dep, rec, D.radiusKm);
    const undo = instrumentViewer(v, rec); // (the stand-in renderers' draws are what place the views)
    for (const mode of ['fit', 'framed']) {
      const key = `${D.id}/${mode}`;
      arrange(v, A, B, mode);
      runTicks(v);
      const r = pickingRun(v, A, B, mode);
      table[key] = r;
      const dA = A.camera.getWorldPosition(new THREE.Vector3())
        .distanceTo(v.spacecraft.get('Chaser').interp.at(v._lastT, new THREE.Vector3()).multiplyScalar(SCALE)) * 1e6;
      check(`${key}: viewport A is framed on the 1.5 m model, ${dA.toFixed(1)} m from the Chaser`, dA > 2 && dA < 40, dA);
      judge(key, r, check);
    }
    undo();
  }
  const failed = checks.filter((c) => !c.pass);
  const summary = DISTANCES.map((D) => {
    const f = table[`${D.id}/fit`];
    return `${D.id}: A inside ${f.A.inside.ok}/${f.A.inside.total}, off ${f.A.off.ok}/${f.A.off.total}, B marker ${f.B.ok}/${f.B.total}`;
  }).join('; ');
  const allPass = failed.length === 0;
  const line = `viewport picking: ${checks.length - failed.length}/${checks.length} checks passed (${summary}); `
    + `pick cost A median ${table['LEO/fit'].cost.A.median.toFixed(3)} ms, primary median ${table['LEO/fit'].cost.primary.median.toFixed(3)} ms`;
  process.stdout.write(JSON.stringify({ allPass, checks, table }) + '\n' + line + '\n', () => process.exit(allPass ? 0 : 1));
}

if (IS_NODE_MAIN) main();
