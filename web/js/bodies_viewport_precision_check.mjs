// CLI harness for tests/test_bodies_viewport_precision.py:
// `node web/js/bodies_viewport_precision_check.mjs`; its measurement kernel is also imported by the
// real-browser proof, tests/test_viewer_viewport_precision_browser.py (`test_every_viewport_draws_the_bodies_...`).
//
// Question 254 item 4 (docs/heavy-plan.md, heavy open-items round), the bodies half of question 240:
// the celestial bodies (their sphere meshes, the sub-pixel dots), the tiled globe's group and the
// body-fixed frame the 3D Tiles overlay hangs from are ONE set of objects drawn by every viewport.
// They used to be written once per tick against the PRIMARY camera's floating origin (`_toLocal`,
// `Math.fround(abs - O_primary)`), so an extra viewport framed near a body, or near a spacecraft
// close to one, drew them where the primary's rounding put them. `Viewer._placeSharedEntities`
// (the render-time, per-view write the entities already use) now places them for the drawing view.
//
// What is measured, for every view that draws (the primary and the two extra viewports), right
// after that view's renderer drew, for each class: the object's FINAL vertex position in that
// view's eye frame, the way the GPU computes it (the `modelViewMatrix` three set for the draw,
// rounded to float32, applied to the float32 vertex data with every multiply and add rounded to
// float32), against the float64 truth: the body's absolute position from its f64 interpolator
// (x SCALE) and orientation from its f64 slerp, the vertex through the object's node chain, into
// the same camera's f64 `matrixWorldInverse`. Three numbers per vertex, all in metres (1 scene
// unit = 1e6 m):
//   total  = the f32 pipeline on the placement the viewer wrote, minus the truth (what is on screen);
//   place  = the same placement applied in f64 (no GPU rounding), minus the truth: ONLY what the
//            placement got wrong;
//   floor  = the f32 pipeline on the EXACT placement, minus the truth: what the GPU's float32
//            model-view multiply cannot do whatever the placement is (about ulp(distance to the
//            vertex's body centre); a sphere's vertices sit one body radius from its origin, so
//            they are at it).
// Per (view, class) the maximum and the maximum tick-to-tick change of the error vector ("hop").
//
// Classes: bodyCentre / bodyVertex (the body nearest the framed spacecraft: its mesh origin, and
// its 64 mesh vertices nearest the camera), rockCentre (a 10 km body 100 km from the framed
// spacecraft), dot:Mars (a body that is a sub-pixel dot in every view: centre, and its error in
// pixels), globe (the 64 tile vertices nearest the camera, `enableGlobe` on the nearest body),
// bodyFixedOrigin / bodyFixedSurface (probe meshes hung in the body-fixed frame the way the 3D
// Tiles overlay is: `placeOverlayInBodyFixedFrame`; the first has its vertices at the frame's
// origin, the second at the body's surface under the spacecraft, as ECEF metres in a 1e-6 scaled
// group, the tile's translation on the mesh's f64 position).
//
// Per-view dot decisions (visibility and size) are also checked: a dot is drawn when the body's
// disc is under 1.5 px radius IN THAT VIEW (its own camera distance, canvas height and fov), and
// then is 1.5 px in radius in that view.
//
// Two ways to use it:
//   * `node web/js/bodies_viewport_precision_check.mjs`: the REAL `Viewer.prototype` of scene.js on
//     the stand-in of entities_viewport_precision_check.mjs (only WebGL, the DOM and OrbitControls
//     are stand-ins); prints one JSON object and exits 1 when any check fails, with `FAIL` lines and
//     a `bodies_viewport_precision_check: N passed, M failed` summary on stderr.
//   * imported by the browser test, which wraps the real renderers.
import * as THREE from 'three';
import { Viewer, SCALE } from './scene.js';
import { placeOverlayInBodyFixedFrame } from './tiles_layer.js';
import { SCENE_UNITS_PER_METRE } from './globe_lod.js';
import {
  SCENE_UNIT_M, CENTIMETRE_M, matrixF32, applyF32, Recorder, nodeSetup, buildViewer, arrange, runTicks,
  nearestIndex, EPOCH_A1MJD, STEP_S, N_SAMPLES, INCLINATION_DEG, CHASER_LEAD_M, MODEL_URL, COV_DIAG_KM2,
  TICKS, T0_DAYS, instrumentRenderer, MU_EARTH_KM3_S2,
} from './entities_viewport_precision_check.mjs';
import { sceneState, firstDifference } from './viewport_picking_check.mjs';

const MU_MOON_KM3_S2 = 4902.8;
const DOT_RADIUS_PX = 1.5;
const NEAREST_VERTICES = 64;

export const SCENES = [
  { id: 'LEO', label: 'LEO 7,000 km', radiusKm: 7000, centre: 'Earth' },
  { id: 'GEO', label: 'GEO 42,164 km', radiusKm: 42164, centre: 'Earth' },
  { id: 'lunar', label: 'low lunar orbit, 1,800 km from the Moon\'s centre', radiusKm: 1800, centre: 'Moon' },
];
export const MOON_KM = [384400 * Math.cos(0.35), 384400 * Math.sin(0.35) * Math.cos(0.09), 384400 * Math.sin(0.35) * Math.sin(0.09)];
const MARS_KM = [2.0e8, 1.1e8, 0.4e8];
export const NEAR_BODY = { LEO: 'Earth', GEO: 'Earth', lunar: 'Moon' };

// ----------------------------------------------------------------------- the scenes (closed form)
/** The scene JSON: Target and Chaser (the Chaser 30 m ahead, carrying the fixture glTF) on one
 * circular orbit of `radiusKm` around Earth (LEO, GEO) or around the Moon (`lunar`, which sits
 * 384,400 km from Earth), and the bodies Earth (central, spinning), Moon, Mars (static, 2.3e8 km
 * away: a sub-pixel dot in every view) and Debris (10 km radius, on the orbit 100 km ahead of the
 * Target, so it is a big disc from the framed viewports and a dot from a far primary). */
export function bodiesScenario(sceneId) {
  const S = SCENES.find((s) => s.id === sceneId);
  const lunar = sceneId === 'lunar';
  const radiusKm = S.radiusKm;
  const mu = lunar ? MU_MOON_KM3_S2 : MU_EARTH_KM3_S2;
  const c = lunar ? MOON_KM : [0, 0, 0];
  const n = Math.sqrt(mu / radiusKm ** 3);
  const inc = (INCLINATION_DEG * Math.PI) / 180;
  const speed = n * radiusKm;
  const cov = [COV_DIAG_KM2[0], 0, 0, 0, COV_DIAG_KM2[1], 0, 0, 0, COV_DIAG_KM2[2]];
  const orbit = (phase, k) => {
    const a = n * k * STEP_S + phase;
    return {
      pos: [c[0] + radiusKm * Math.cos(a), c[1] + radiusKm * Math.sin(a) * Math.cos(inc), c[2] + radiusKm * Math.sin(a) * Math.sin(inc)],
      vel: [-speed * Math.sin(a), speed * Math.cos(a) * Math.cos(inc), speed * Math.cos(a) * Math.sin(inc)],
    };
  };
  const times = [];
  for (let k = 0; k < N_SAMPLES; k++) times.push(EPOCH_A1MJD + (k * STEP_S) / 86400);
  const track = (name, phase, color, model) => {
    const pos = [], vel = [], covFlat = [];
    for (let k = 0; k < N_SAMPLES; k++) { const o = orbit(phase, k); pos.push(...o.pos); vel.push(...o.vel); covFlat.push(...cov); }
    const d = { name, label: name, color, t: times, pos, vel, attitude: [], cov: covFlat, covDim: 3 };
    if (model) d.model = model;
    return d;
  };
  const target = track('Target', 0, '#54a0ff');
  const chaser = track('Chaser', (CHASER_LEAD_M / 1000) / radiusKm, '#ff6b6b', MODEL_URL);
  const span = [times[0], times[N_SAMPLES - 1]];
  const still = (name, radius, flattening, color, p, extra = {}) => ({
    name, radius, flattening, texture: null, color, central: false,
    t: span, pos: [...p, ...p], quat: [0, 0, 0, 1, 0, 0, 0, 1], spinRate: 0, ...extra,
  });
  const debrisPos = [];
  for (let k = 0; k < N_SAMPLES; k++) debrisPos.push(...orbit(100 / radiusKm, k).pos);
  const bodies = [
    still('Earth', 6378.137, 1 / 298.257223563, '#2e6fbd', [0, 0, 0], { central: true, spinRate: 360.9856 }),
    still('Moon', 1737.4, 0.0012, '#b0b0b0', MOON_KM, { spinRate: 13.1764 }),
    still('Mars', 3389.5, 0.0059, '#c1440e', MARS_KM),
    { name: 'Debris', radius: 10, flattening: 0, texture: null, color: '#ffd166', central: false, t: times, pos: debrisPos,
      quat: times.flatMap(() => [0, 0, 0, 1]), spinRate: 0 },
  ];
  return {
    name: `Bodies viewport precision ${sceneId}`,
    frame: { name: 'EarthMJ2000Eq', origin: 'Earth', axes: 'MJ2000Eq' },
    frames: [{ id: 'EarthMJ2000Eq', axes: 'AXES_KIND_MJ2000_EQ', body: 'Earth' }],
    spacecraft: [target, chaser], bodies,
    events: [{ name: 'Burn', t: EPOCH_A1MJD + 612 / 86400, type: 'maneuver', spacecraft: 'Chaser', detail: null, referenceId: null, attributes: {} }],
    footprints: [],
  };
}

// ----------------------------------------------------------------------- helpers
const _chainM = new THREE.Matrix4();
/** The product of the LOCAL matrices from `stop`'s child down to `obj` (f64, exact for what is
 * placed below a body): the part of `obj.matrixWorld` that does not depend on where the body is. */
function chainBelow(obj, stop) {
  const m = new THREE.Matrix4();
  for (let n = obj; n && n !== stop; n = n.parent) { n.updateMatrix(); m.premultiply(n.matrix); }
  return m;
}

function visibleChain(obj) {
  for (let n = obj; n; n = n.parent) if (!n.visible) return false;
  return true;
}

/** True when `obj.modelViewMatrix` was set by the draw that just happened. */
function drawnNow(obj, camera) {
  _chainM.multiplyMatrices(camera.matrixWorldInverse, obj.matrixWorld);
  const a = _chainM.elements, b = obj.modelViewMatrix.elements;
  for (let i = 0; i < 16; i++) if (a[i] !== b[i]) return false;
  return true;
}

/** A body's float64 truth at the viewer's clock: absolute scaled position and orientation, from
 * the interpolator -- nothing the placement wrote. */
export function bodyTruth(viewer, b) {
  const t = viewer._lastT;
  const abs = b.interp.position(t, new THREE.Vector3()).multiplyScalar(SCALE);
  const q = b.interp.orientation(t, new THREE.Quaternion());
  return { abs, q, M: new THREE.Matrix4().compose(abs, q, new THREE.Vector3(1, 1, 1)) };
}

/** Radius, in pixels, of a body's disc in the view of `camera` (canvas height `H`, float64 distance). */
export function discRadiusPx(b, distance, camera, H) {
  return (b.data.radius * SCALE * H) / (2 * distance * Math.tan(THREE.MathUtils.degToRad(camera.fov / 2)));
}

/** The canvas height the viewer's own dot logic uses for a view (see `Viewer._placeBodies`). */
export function viewCanvasHeight(viewer, viewName) {
  const vp = viewName === 'primary' ? null : viewer.viewports.get(viewName);
  return (vp && vp.canvas && vp.canvas.clientHeight) || viewer._referenceCanvasHeight();
}

// ----------------------------------------------------------------------- the kernel
/** Recorders for the three numbers: total / place / floor (+ aux, in pixels and other units). */
export function makeRecorders() { return { total: new Recorder(), place: new Recorder(), floor: new Recorder(), aux: new Recorder(), decisions: [] }; }
export function resetRecorders(R) { R.total.reset(); R.place.reset(); R.floor.reset(); R.aux.reset(); R.decisions.length = 0; }

/**
 * Measure the vertices `idx` (indices into `positions`, a flat f32 array) of `obj`, which `camera`
 * has just drawn, against `Mtrue`, the f64 matrix that puts `obj`'s local frame where it truly is.
 */
function measureVertices(R, view, cls, obj, camera, Mtrue, positions, idx, keyOf) {
  const camInv = camera.matrixWorldInverse;
  const Meye = new THREE.Matrix4().multiplyMatrices(camInv, Mtrue);
  const mvTrue = matrixF32(Meye);
  const mvPlaced = matrixF32(obj.modelViewMatrix);
  const mvPlacedF64 = obj.modelViewMatrix;
  const out = [];
  for (const i of idx) {
    const p = [positions[3 * i], positions[3 * i + 1], positions[3 * i + 2]];
    const truth = new THREE.Vector3(p[0], p[1], p[2]).applyMatrix4(Meye);
    const placed = new THREE.Vector3(p[0], p[1], p[2]).applyMatrix4(mvPlacedF64);
    const err = (e) => [(e[0] - truth.x) * SCENE_UNIT_M, (e[1] - truth.y) * SCENE_UNIT_M, (e[2] - truth.z) * SCENE_UNIT_M];
    const key = keyOf(i);
    R.total.add(view, cls, key, err(applyF32(mvPlaced, p)));
    R.place.add(view, cls, key, err([placed.x, placed.y, placed.z]));
    R.floor.add(view, cls, key, err(applyF32(mvTrue, p)));
    out.push(truth);
  }
  return out;
}

/** The indices of the `k` vertices of `positions` (flat f32) nearest the camera under `Meye`. */
function nearestVertices(positions, Meye, k) {
  const n = positions.length / 3;
  const d = new Float64Array(n);
  const v = new THREE.Vector3();
  for (let i = 0; i < n; i++) { v.set(positions[3 * i], positions[3 * i + 1], positions[3 * i + 2]).applyMatrix4(Meye); d[i] = v.lengthSq(); }
  return Array.from({ length: n }, (_, i) => i).sort((a, b) => d[a] - d[b]).slice(0, k);
}

/**
 * Measure every body-related object `camera` has just drawn. `ctx`: `{ near, globeBody, overlay:
 * {origin, surface} | null, dotBodies: [names], rock }`. Call it right after that view's
 * `renderer.render(scene, camera)`.
 */
export function measureBodies(viewer, camera, viewName, R, ctx) {
  const camInv = camera.matrixWorldInverse;
  const H = viewCanvasHeight(viewer, viewName);
  const pxPerRad = (H / 2) / Math.tan(THREE.MathUtils.degToRad(camera.fov / 2));
  const camWorld = new THREE.Vector3().setFromMatrixPosition(camera.matrixWorld);
  const drawn = (obj) => visibleChain(obj) && drawnNow(obj, camera);

  const near = viewer.bodies.get(ctx.near);
  if (near) {
    const tr = bodyTruth(viewer, near);
    const Mtrue = new THREE.Matrix4().compose(tr.abs, tr.q, near.mesh.scale);
    const pos = near.mesh.geometry.attributes.position.array;
    if (drawn(near.mesh)) {
      measureVertices(R, viewName, 'bodyCentre', near.mesh, camera, Mtrue, [0, 0, 0], [0], () => 'c');
      const idx = nearestVertices(pos, new THREE.Matrix4().multiplyMatrices(camInv, Mtrue), NEAREST_VERTICES);
      measureVertices(R, viewName, 'bodyVertex', near.mesh, camera, Mtrue, pos, idx, (i) => String(i));
    } else { R.total.skip(viewName, 'bodyCentre'); R.total.skip(viewName, 'bodyVertex'); }
  }
  const rock = viewer.bodies.get(ctx.rock);
  if (rock) {
    const tr = bodyTruth(viewer, rock);
    const Mtrue = new THREE.Matrix4().compose(tr.abs, tr.q, rock.mesh.scale);
    if (drawn(rock.mesh)) measureVertices(R, viewName, 'rockCentre', rock.mesh, camera, Mtrue, [0, 0, 0], [0], () => 'c');
    else R.total.skip(viewName, 'rockCentre');
  }

  // dots: position error, the per-view visibility decision and the apparent size
  for (const name of ctx.dotBodies) {
    const b = viewer.bodies.get(name);
    if (!b) continue;
    const tr = bodyTruth(viewer, b);
    const dist = tr.abs.distanceTo(camWorld);
    const discPx = discRadiusPx(b, dist, camera, H);
    const expectDot = b.visible && discPx < DOT_RADIUS_PX;
    R.decisions.push({ view: viewName, body: name, discPx, expectDot, drawnDot: !!(b.dot.visible && visibleChain(b.dot)), borderline: Math.abs(discPx - DOT_RADIUS_PX) < 1e-3 * DOT_RADIUS_PX });
    if (b.dot.visible && drawn(b.dot)) {
      const cls = `dot:${name}`;
      const Mtrue = new THREE.Matrix4().compose(tr.abs, new THREE.Quaternion(), b.dot.scale);
      const [truthEye] = measureVertices(R, viewName, cls, b.dot, camera, Mtrue, [0, 0, 0], [0], () => 'c');
      // the lateral (on-screen) error of the f32 pipeline, in pixels, and the dot's apparent radius
      const e32 = applyF32(matrixF32(b.dot.modelViewMatrix), [0, 0, 0]);
      const dx = e32[0] - truthEye.x, dy = e32[1] - truthEye.y, dz = e32[2] - truthEye.z;
      const along = (dx * truthEye.x + dy * truthEye.y + dz * truthEye.z) / truthEye.length();
      const lateral = Math.sqrt(Math.max(0, dx * dx + dy * dy + dz * dz - along * along));
      R.aux.add(viewName, `${cls} lateral px`, 'c', [(lateral / truthEye.length()) * pxPerRad, 0, 0]);
      R.aux.add(viewName, `${cls} radius-1.5 px`, 'c', [(b.dot.scale.x / truthEye.length()) * pxPerRad - DOT_RADIUS_PX, 0, 0]);
    } else R.total.skip(viewName, `dot:${name}`);
  }

  // the tiled globe: the tile vertices nearest the camera
  const gb = viewer.bodies.get(ctx.globeBody || '');
  if (viewer.globeLayer && gb) {
    const tr = bodyTruth(viewer, gb);
    const meshes = [...viewer.globeLayer._meshes.values()].filter((m) => drawn(m));
    if (meshes.length) {
      const cand = [];
      const Meyes = new Map();
      for (const m of meshes) {
        const Mt = tr.M.clone().multiply(chainBelow(m, viewer.globeLayer.group));
        const Me = new THREE.Matrix4().multiplyMatrices(camInv, Mt);
        Meyes.set(m, { Mt, Me });
        const pos = m.geometry.attributes.position.array;
        const v = new THREE.Vector3();
        for (let i = 0; i < pos.length / 3; i++) { v.set(pos[3 * i], pos[3 * i + 1], pos[3 * i + 2]).applyMatrix4(Me); cand.push([v.lengthSq(), m, i]); }
      }
      cand.sort((a, b) => a[0] - b[0]);
      const byMesh = new Map();
      for (const [, m, i] of cand.slice(0, NEAREST_VERTICES)) { if (!byMesh.has(m)) byMesh.set(m, []); byMesh.get(m).push(i); }
      for (const [m, idx] of byMesh) {
        measureVertices(R, viewName, 'globe', m, camera, Meyes.get(m).Mt, m.geometry.attributes.position.array, idx, (i) => `${m.userData.tile ? `${m.userData.tile.level}/${m.userData.tile.x}/${m.userData.tile.y}` : m.id}:${i}`);
      }
    } else R.total.skip(viewName, 'globe');
  }

  // the body-fixed frame: probe meshes hung in it the way the overlay's tiles are
  if (ctx.overlay) {
    const ob = viewer.bodies.get(ctx.overlay.bodyName);
    const tr = bodyTruth(viewer, ob);
    const node = viewer.frameGraph.frame(`body-fixed:${ctx.overlay.bodyName}`).object3D;
    for (const [cls, probe] of [['bodyFixedOrigin', ctx.overlay.origin], ['bodyFixedSurface', ctx.overlay.surface]]) {
      if (!drawn(probe)) { R.total.skip(viewName, cls); continue; }
      const Mtrue = tr.M.clone().multiply(chainBelow(probe, node));
      const pos = probe.geometry.attributes.position.array;
      measureVertices(R, viewName, cls, probe, camera, Mtrue, pos, [0, 1, 2], (i) => String(i));
    }
  }
}

/** Wrap the primary's and every viewport's renderer so each draw is measured into `R`. */
export function instrumentBodies(viewer, R, ctx) {
  const undo = [];
  const nameOf = (camera) => {
    if (camera === viewer.camera) return 'primary';
    for (const [id, vp] of viewer.viewports) if (vp.camera === camera) return id;
    return 'other';
  };
  const hook = (camera) => measureBodies(viewer, camera, nameOf(camera), R, ctx);
  undo.push(instrumentRenderer(viewer.renderer, hook));
  for (const vp of viewer.viewports.values()) if (vp.renderer) undo.push(instrumentRenderer(vp.renderer, hook));
  return () => undo.forEach((u) => u());
}

// ----------------------------------------------------------------------- scene objects the measurement needs
/** Three probe meshes stand in for the 3D Tiles overlay's tiles, hung in `bodyName`'s body-fixed
 * frame by `placeOverlayInBodyFixedFrame` (a group scaled by SCENE_UNITS_PER_METRE, ECEF metres
 * below it): `origin` has its three vertices (0, 500 m, 500 m) around the frame's origin, `surface` has its
 * mesh at the body's surface under `underName` (the tile's translation on the mesh's f64
 * position, 3 vertices within 500 m of it). Marks the body as the overlay's, so `update(t)`
 * keeps the frame in step like it does with a real overlay. */
export function attachOverlayProbes(viewer, bodyName, underName) {
  const b = viewer.bodies.get(bodyName);
  const node = viewer._bodyFixedFrame(bodyName);
  viewer._tilesOverlayBodyName = bodyName;
  const tr = bodyTruth(viewer, b);
  const sc = viewer.spacecraft.get(underName);
  const dir = sc.interp.at(viewer._lastT, new THREE.Vector3()).multiplyScalar(SCALE).sub(tr.abs)
    .applyQuaternion(tr.q.clone().invert()).normalize();
  const P = dir.multiplyScalar(b.data.radius * 1000); // ECEF-like metres, a sphere's surface under the spacecraft
  const tri = (verts) => {
    const g = new THREE.BufferGeometry();
    g.setAttribute('position', new THREE.BufferAttribute(new Float32Array(verts), 3));
    g.setIndex([0, 1, 2]);
    const m = new THREE.Mesh(g, new THREE.MeshBasicMaterial({ side: THREE.DoubleSide }));
    m.frustumCulled = false;
    return m;
  };
  const group = new THREE.Group();
  const origin = tri([0, 0, 0, 500, 0, 0, 0, 500, 0]);
  const surface = tri([0, 0, 0, 500, 0, 0, 0, 500, 0]);
  surface.position.copy(P);
  group.add(origin, surface);
  placeOverlayInBodyFixedFrame(group, viewer.frameGraph, node.id);
  viewer._syncBodyFixedFrame(bodyName);
  return { bodyName, group, origin, surface };
}

/** Switch frustum culling off on what the measurement looks at (an object three culls keeps a
 * stale modelViewMatrix, and the measurement wants every one of them drawn in every view). */
export function disableBodyCulling(viewer) {
  for (const b of viewer.bodies.values()) { b.mesh.frustumCulled = false; b.dot.frustumCulled = false; }
  if (viewer.globeLayer) {
    const g = viewer.globeLayer.group;
    g.traverse((o) => { o.frustumCulled = false; });
    if (!g.userData.cullingHooked) {
      const add = g.add.bind(g);
      g.add = (...objs) => { objs.forEach((o) => { o.frustumCulled = false; }); return add(...objs); };
      g.userData.cullingHooked = true;
    }
  }
}

const stubTextureLoader = { load() {} };

/** Enable the globe on `bodyName` with a texture loader that never answers (the tiles keep their
 * flat colour; the vertices are what is measured). */
export function enableGlobeForMeasurement(viewer, bodyName) {
  // `segments: 8` pins the tile geometry the numbers in fixtures/bodies_viewport_precision_before_primary.json
  // were measured on. The globe's own default (question 264) is finer at the shallow levels
  // (`segmentsForLevel` in globe.js: about 2.8-degree facets instead of 22.5), which changes WHICH
  // vertices lie nearest the camera, so the pinned "globe" class would no longer compare like with
  // like; what this check measures is float32 placement of a vertex, which does not depend on it.
  // `minLevel: 0` likewise pins the level floor (question 264, default 2) off: with it the tile set
  // nearest the camera, and so the nearest vertices the pinned numbers were measured on, would change.
  const ok = viewer.enableGlobe(bodyName, { textureLoader: stubTextureLoader, segments: 8, minLevel: 0 });
  disableBodyCulling(viewer);
  return ok;
}

// ----------------------------------------------------------------------- the run protocol
export const CONFIGS = ['plain', 'globe'];
export const MODES = ['fit', 'framed'];

/**
 * One measured run: arrange the three views (`mode` for the primary), switch the globe on or off,
 * tick `ticks` times with every draw measured. Returns `{ total, place, floor, aux, decisions,
 * geometry }` summaries.
 */
export function measureRun(viewer, A, B, sceneId, mode, config, probes, ticks = TICKS) {
  const near = NEAR_BODY[sceneId];
  arrange(viewer, A, B, mode);
  viewer.disableGlobe();
  const ctx = { near, rock: 'Debris', dotBodies: ['Mars', 'Debris'], globeBody: null, overlay: probes };
  if (config === 'globe') { enableGlobeForMeasurement(viewer, near); ctx.globeBody = near; }
  disableBodyCulling(viewer);
  runTicks(viewer, 2); // settle the tile selection before the measured ticks
  const R = makeRecorders();
  const undo = instrumentBodies(viewer, R, ctx);
  resetRecorders(R);
  runTicks(viewer, ticks);
  undo();
  const chaser = viewer.spacecraft.get('Chaser').interp.at(viewer._lastT, new THREE.Vector3()).multiplyScalar(SCALE);
  const oA = viewer.viewports.get('A').floatingOrigin.getOrigin(viewer._originFrameId);
  const oP = viewer.floatingOrigin.getOrigin(viewer._originFrameId);
  return {
    total: R.total.summary(), place: R.place.summary(), floor: R.floor.summary(), aux: R.aux.summary(), decisions: R.decisions.slice(),
    geometry: {
      chaserDistanceFromViewportA_m: viewer.viewports.get('A').camera.getWorldPosition(new THREE.Vector3()).distanceTo(chaser) * SCENE_UNIT_M,
      originPrimary: [oP.x, oP.y, oP.z], originA: [oA.x, oA.y, oA.z],
      originDifferenceAvsPrimary: Math.hypot(oA.x - oP.x, oA.y - oP.y, oA.z - oP.z),
      tiles: viewer.globeLayer ? viewer.globeLayer._meshes.size : 0,
    },
  };
}

/** Every class the table reports, in order. */
export const BODY_CLASSES = ['bodyCentre', 'bodyVertex', 'rockCentre', 'dot:Mars', 'dot:Debris', 'globe', 'bodyFixedOrigin', 'bodyFixedSurface'];

// ----------------------------------------------------------------------- primary identity
const sameBits = (a, b) => Object.is(a, b);

/** What the primary camera's placement of the bodies must be, bit for bit: the body mesh, the dot
 * and the globe group at exactly `_toLocal`'s value (`fround(abs - O_primary)`), the body-fixed
 * frame node's LOCAL position the same value, and its WORLD position the body mesh's world
 * position (the node is a child of the entities render group, like the mesh). Returns the
 * differences (empty = identical). */
export function primaryBodiesIdentity(viewer, ctx) {
  const fo = viewer.floatingOrigin, frame = viewer._originFrameId;
  const bad = [];
  const eq = (what, got, want) => {
    for (let i = 0; i < 3; i++) if (!sameBits(got[i], want[i])) { bad.push(`${what}[${i}] ${got[i]} != ${want[i]}`); return; }
  };
  for (const [name, b] of viewer.bodies) {
    const tr = bodyTruth(viewer, b);
    const r = fo.toRenderSpace(frame, tr.abs);
    eq(`${name} mesh`, b.mesh.position.toArray(), [r.x, r.y, r.z]);
    if (b.dot.visible) eq(`${name} dot`, b.dot.position.toArray(), [r.x, r.y, r.z]);
    if (viewer.globeLayer && viewer.globeBodyName === name) eq(`${name} globe group`, viewer.globeLayer.group.position.toArray(), [r.x, r.y, r.z]);
    if (viewer._tilesOverlayBodyName === name) {
      const node = viewer.frameGraph.frame(`body-fixed:${name}`).object3D;
      eq(`${name} body-fixed node (local)`, node.position.toArray(), [r.x, r.y, r.z]);
      node.updateMatrixWorld(true); b.mesh.updateWorldMatrix(true, false);
      eq(`${name} body-fixed node (world) vs mesh (world)`, node.matrixWorld.elements.slice(12, 15), b.mesh.matrixWorld.elements.slice(12, 15));
    }
  }
  return bad;
}

/** The primary's measured numbers for one run (`run` = a `measureRun` result) against the ones the
 * code gave before item 4 (`want`: class -> [on-screen max error, max hop, placement max error],
 * fixtures/bodies_viewport_precision_before_primary.json), at 1e-9 relative. Returns the classes
 * that differ. */
export function primaryMatchesBefore(run, want) {
  const close = (a, b) => Math.abs(a - b) <= 1e-9 * Math.max(Math.abs(b), 1e-3);
  const bad = [];
  for (const [cls, w] of Object.entries(want)) {
    const t = (run.total.primary || {})[cls], p = (run.place.primary || {})[cls];
    if (!t || !p || !close(t.maxErr, w[0]) || !close(t.maxHop, w[1]) || !close(p.maxErr, w[2])) {
      bad.push({ cls, got: t && p ? [t.maxErr, t.maxHop, p.maxErr] : null, want: w });
    }
  }
  return bad;
}

// ----------------------------------------------------------------------- picking a body
/** `viewer.pick(camera, x, y)` through `n` points inside the facets of the body's mesh nearest the
 * view (the facet corners are the float64 truth: the mesh vertices through the body's true
 * position and orientation), with the view's camera aimed at the nearest facet: each must resolve
 * to the body; and the ray's hit distance on the placed mesh (a THREE.Raycaster, after the view's
 * placement, exactly what `pick` casts) is compared with the float64 ray/triangle truth. Returns
 * { picks, ok, maxHitErrM, changed } (changed: picks that disturbed the scene graph). */
export function bodyPickProof(viewer, view, bodyName, n = 12) {
  const camera = view ? view.camera : viewer.camera;
  const b = viewer.bodies.get(bodyName);
  const tr = bodyTruth(viewer, b);
  const Mtrue = new THREE.Matrix4().compose(tr.abs, tr.q, b.mesh.scale);
  const geo = b.mesh.geometry;
  const pos = geo.attributes.position, idx = geo.index;
  const camPos = new THREE.Vector3().setFromMatrixPosition(camera.matrixWorld);
  const tri = (k) => [0, 1, 2].map((j) => {
    const i = idx ? idx.getX(3 * k + j) : 3 * k + j;
    return new THREE.Vector3(pos.getX(i), pos.getY(i), pos.getZ(i)).applyMatrix4(Mtrue);
  });
  const nTri = idx ? idx.count / 3 : pos.count / 3;
  const near = [];
  for (let k = 0; k < nTri; k++) {
    const t3 = tri(k);
    const c = new THREE.Vector3().add(t3[0]).add(t3[1]).add(t3[2]).multiplyScalar(1 / 3);
    near.push([c.distanceTo(camPos), k, t3, c]);
  }
  near.sort((a, c) => a[0] - c[0]);
  // the viewport looks at its focus, not at the body: aim it at the nearest facet for the proof
  // (the camera's position, which the placement reads, is untouched) and put the aim back after
  const savedQ = camera.quaternion.clone();
  const reaim = () => { camera.updateMatrixWorld(true); camera.matrixWorldInverse.copy(camera.matrixWorld).invert(); };
  camera.lookAt(near[0][3]);
  reaim();
  const rc = new THREE.Raycaster();
  let ok = 0, picks = 0, maxHitErrM = 0;
  const changed = [];
  const camInv = camera.matrixWorldInverse;
  // interior points of the nearest facets (a barycentric grid, 55 per facet) that are in the view,
  // `n` of them in all: the nearest facet first, the camera is aimed at its centroid
  const samples = [];
  const GRID = 12;
  for (const [, , t3] of near.slice(0, 30)) for (let i = 1; i < GRID; i++) for (let j = 1; i + j < GRID; j++) {
    const w = [i / GRID, j / GRID, (GRID - i - j) / GRID];
    samples.push([t3, new THREE.Vector3().addScaledVector(t3[0], w[0]).addScaledVector(t3[1], w[1]).addScaledVector(t3[2], w[2])]);
  }
  for (const [t3, c] of samples) {
    if (picks >= n) break;
    const ndc = c.clone().applyMatrix4(camInv).applyMatrix4(camera.projectionMatrix);
    if (Math.abs(ndc.x) > 0.95 || Math.abs(ndc.y) > 0.95 || ndc.z < -1 || ndc.z > 1) continue;
    picks += 1;
    const before = sceneState(viewer), placedBefore = viewer._placedView;
    const hit = viewer.pick(camera, ndc.x, ndc.y);
    const diff = firstDifference(before, sceneState(viewer));
    if (diff || viewer._placedView !== placedBefore) changed.push({ ndc: [ndc.x, ndc.y], diff });
    if (hit && hit.kind === 'body' && hit.name === bodyName) ok += 1;
    // the placed mesh's hit point along that ray, against the true triangle's, in the view's placement
    const placedFor = view || null;
    const prev = viewer._placedView;
    viewer._placeSharedEntities(placedFor);
    rc.setFromCamera(new THREE.Vector2(ndc.x, ndc.y), camera);
    const hits = rc.intersectObject(b.mesh, false);
    if (prev !== placedFor) viewer._placeSharedEntities(prev === undefined ? null : prev);
    // truth: the ray against the float64 triangle
    const o = rc.ray.origin, d = rc.ray.direction;
    const e1 = t3[1].clone().sub(t3[0]), e2 = t3[2].clone().sub(t3[0]);
    const pv = d.clone().cross(e2), det = e1.dot(pv);
    let tTrue = NaN;
    if (Math.abs(det) > 1e-30) {
      const tv = o.clone().sub(t3[0]);
      const u = tv.dot(pv) / det;
      const qv = tv.clone().cross(e1);
      const v = d.dot(qv) / det;
      if (u >= -1e-9 && v >= -1e-9 && u + v <= 1 + 1e-9) tTrue = e2.dot(qv) / det;
    }
    if (hits.length && Number.isFinite(tTrue)) maxHitErrM = Math.max(maxHitErrM, Math.abs(hits[0].distance - tTrue) * SCENE_UNIT_M);
    else if (Number.isFinite(tTrue)) maxHitErrM = Math.max(maxHitErrM, Infinity);
  }
  camera.quaternion.copy(savedQ);
  reaim();
  return { picks, ok, maxHitErrM, changed };
}

// ====================================================================== node main
const IS_NODE_MAIN = typeof process !== 'undefined' && process.argv && process.argv[1]
  && process.argv[1].endsWith('bodies_viewport_precision_check.mjs');

/** Build the viewer for `sceneId` (three views, the Chaser's model loaded) with the overlay probes. */
export async function buildBodiesViewer(dep, sceneId) {
  const rec = new Recorder();
  const built = await buildViewer(dep, rec, SCENES.find((s) => s.id === sceneId).radiusKm, bodiesScenario(sceneId));
  built.probes = attachOverlayProbes(built.v, NEAR_BODY[sceneId], 'Chaser');
  return built;
}

async function main() {
  const dep = await nodeSetup();
  const checks = [];
  const check = (name, pass, detail) => checks.push({ name, pass: !!pass, detail });
  const table = {};
  const bound = process.env.AV_PRECISION_BOUND_M ? Number(process.env.AV_PRECISION_BOUND_M) : null;
  const PLACEMENT_BOUND_M = bound ?? 1e-3;
  const hasPlacement = typeof Viewer.prototype._placeBodies === 'function';
  check('Viewer places the bodies per drawing view (Viewer._placeBodies exists)', hasPlacement);
  const picking = {};
  const fs = await import('node:fs');
  const path = await import('node:path');
  const { fileURLToPath } = await import('node:url');
  const beforePrimary = JSON.parse(fs.readFileSync(
    path.join(path.dirname(fileURLToPath(import.meta.url)), 'fixtures', 'bodies_viewport_precision_before_primary.json'), 'utf8'));

  for (const S of SCENES) {
    const { v, A, B, probes } = await buildBodiesViewer(dep, S.id);
    const near = NEAR_BODY[S.id];
    for (const mode of MODES) {
      for (const config of CONFIGS) {
        const key = `${S.id}/${mode}/${config}`;
        const r = measureRun(v, A, B, S.id, mode, config, probes);
        table[key] = r;
        const views = ['primary', 'A', 'B'];
        const classes = BODY_CLASSES.filter((c) => (config === 'globe' ? c !== 'bodyCentre' && c !== 'bodyVertex' : c !== 'globe'));
        // ---- every class measured on every tick, in every view
        for (const view of views) {
          for (const cls of classes) {
            if (cls === 'dot:Debris' && !(r.total[view] || {})[cls]) continue; // a dot only where its disc is under 1.5 px (a view-dependent fact, below)
            const s = (r.total[view] || {})[cls];
            check(`${key}: ${view}/${cls} was measured on every tick`, !!s && s.n >= TICKS && s.skipped === 0, s);
          }
        }
        check(`${key}: viewport A is framed on the 1.5 m model, ${r.geometry.chaserDistanceFromViewportA_m.toFixed(1)} m from the Chaser`,
          r.geometry.chaserDistanceFromViewportA_m > 2 && r.geometry.chaserDistanceFromViewportA_m < 40, r.geometry);
        // ---- what the PLACEMENT got wrong, in the extra viewports: nothing (f64 exact)
        for (const view of ['A', 'B']) {
          for (const cls of classes) {
            const s = (r.place[view] || {})[cls];
            if (!s) continue;
            check(`${key}: ${view}/${cls} placement error ${s.maxErr.toExponential(2)} m <= ${PLACEMENT_BOUND_M} m`, s.maxErr <= PLACEMENT_BOUND_M, s);
            check(`${key}: ${view}/${cls} placement hop ${s.maxHop.toExponential(2)} m <= ${PLACEMENT_BOUND_M} m`, s.maxHop <= PLACEMENT_BOUND_M, s);
          }
        }
        // ---- the end-to-end number is the GPU's float32 floor, not the placement: total <= floor + the placement + one float32 step of the floor's own size
        for (const view of ['A', 'B']) {
          for (const cls of classes) {
            const t = (r.total[view] || {})[cls], f = (r.floor[view] || {})[cls];
            if (!t || !f || cls.startsWith('dot:')) continue;
            const limit = 2 * f.maxErr + CENTIMETRE_M;
            check(`${key}: ${view}/${cls} on-screen error ${t.maxErr.toExponential(2)} m is within 2x the float32 floor ${f.maxErr.toExponential(2)} m (+ 1 cm)`, t.maxErr <= limit, { total: t, floor: f });
          }
        }
        // ---- the primary: bit-identical placement, and the body-fixed frame is where the body is
        if (hasPlacement) {
          const bad = primaryBodiesIdentity(v, { near });
          check(`${key}: after the tick the primary's bodies, dots, globe group and body-fixed frame hold exactly _toLocal's values, bit for bit`, bad.length === 0, bad.slice(0, 5));
          check(`${key}: the tick leaves the primary placed (the restore after the viewport loop)`, v._placedView === null, { placed: v._placedView && v._placedView.id });
        }
        // ---- the primary's numbers are the ones the code gave before this change (the body-fixed frame excepted)
        const wantBefore = beforePrimary.runs[key];
        const moved = primaryMatchesBefore(r, wantBefore);
        check(`${key}: the primary's on-screen error, hop and placement error for the bodies, dots and globe equal the numbers measured before the change (${Object.keys(wantBefore).length} classes)`,
          moved.length === 0 && Object.keys(wantBefore).length >= 3, moved);
        // ---- question 46's centimetre bound, where the object is near the view's origin: the 10 km body 100 km from the Chaser
        for (const view of ['A', 'B']) {
          const s = (r.total[view] || {}).rockCentre;
          check(`${key}: ${view}/rockCentre (near the view's origin) on-screen error ${s ? s.maxErr.toExponential(2) : 'n/a'} m and hop ${s ? s.maxHop.toExponential(2) : 'n/a'} m are within question 46's 1 cm`,
            !!s && s.maxErr <= CENTIMETRE_M && s.maxHop <= CENTIMETRE_M, s);
        }
        // ---- per-view dot decisions and sizes
        if (hasPlacement) {
          const wrong = r.decisions.filter((d) => !d.borderline && d.expectDot !== d.drawnDot);
          check(`${key}: every view draws a body's dot exactly when its disc is under 1.5 px in THAT view (${r.decisions.length} decisions)`, wrong.length === 0, wrong.slice(0, 4));
          for (const view of views) for (const cls of ['dot:Mars', 'dot:Debris']) {
            const s = (r.aux[view] || {})[`${cls} radius-1.5 px`];
            if (!s) continue;
            check(`${key}: ${view}/${cls} is ${DOT_RADIUS_PX} px in radius in that view (max deviation ${s.maxErr.toExponential(2)} px)`, s.maxErr <= 0.01, s);
          }
          if (mode === 'fit' && config === 'plain') {
            const dd = (view) => r.decisions.filter((d) => d.view === view && d.body === 'Debris');
            check(`${key}: Debris is a dot from the far primary and a disc from viewport A (the case a single shared dot gets wrong)`,
              dd('primary').every((d) => d.drawnDot) && dd('A').every((d) => !d.drawnDot), { primary: dd('primary')[0], A: dd('A')[0] });
          }
        }
      }
    }

    // ---- picking a body in an extra viewport resolves at the body's true position
    for (const mode of MODES) {
      arrange(v, A, B, mode);
      v.disableGlobe();
      disableBodyCulling(v);
      runTicks(v, 3);
      for (const [label, view] of [['A', A], ['B', B]]) {
        const p = bodyPickProof(v, view, near);
        picking[`${S.id}/${mode}/${label}`] = p;
        check(`${S.id}/${mode}: pick in viewport ${label} through ${p.picks} points of the ${near}'s true facets resolves to the ${near} (${p.ok}/${p.picks}); its hit is within ${PLACEMENT_BOUND_M} m of the f64 truth (max ${p.maxHitErrM.toExponential(2)} m); each pick leaves the scene graph as found`,
          p.picks >= 8 && p.ok === p.picks && p.maxHitErrM <= PLACEMENT_BOUND_M && p.changed.length === 0, p);
      }
    }

    // ---- teeth: the old behaviour (every draw placed for the primary's origin) is measured as the
    // defect it was, by this same measurement, in this process; the primary is unaffected
    if (hasPlacement && S.id === 'lunar') {
      v._onSceneBeforeRender = () => v._placeSharedEntities(null);
      const rb = measureRun(v, A, B, S.id, 'fit', 'plain', probes);
      delete v._onSceneBeforeRender;
      table['lunar/fit/plain/BREAKS (every draw placed for the primary origin)'] = rb;
      check('BREAKS: placing every draw for the primary\'s origin puts viewport A\'s Moon, Debris and body-fixed frame off by more than 1 m at the Earth-Moon distance',
        ['bodyCentre', 'rockCentre', 'bodyFixedOrigin'].every((c) => rb.place.A[c].maxErr > 1), ['bodyCentre', 'rockCentre', 'bodyFixedOrigin'].map((c) => rb.place.A[c].maxErr));
      check('BREAKS: ... and the primary\'s numbers are the same either way', primaryMatchesBefore(rb, beforePrimary.runs['lunar/fit/plain']).length === 0);
      check('BREAKS: ... and the dot is wrong in viewport A (Debris, a disc there, drawn as a dot sized for the far primary)',
        rb.decisions.some((d) => d.view === 'A' && d.body === 'Debris' && d.drawnDot && !d.expectDot) && ((rb.aux.A || {})['dot:Debris radius-1.5 px'] || { maxErr: 0 }).maxErr > 100, rb.aux.A);
    }
  }

  const failed = checks.filter((c) => !c.pass);
  const allPass = failed.length === 0;
  for (const c of failed) process.stderr.write(`FAIL ${c.name}: ${(JSON.stringify(c.detail) ?? '').slice(0, 400)}\n`);
  process.stderr.write(`bodies_viewport_precision_check: ${checks.length - failed.length} passed, ${failed.length} failed (placement bound ${PLACEMENT_BOUND_M} m)\n`);
  process.stdout.write(JSON.stringify({ allPass, checks, table, picking }), () => process.exit(allPass ? 0 : 1));
}

if (IS_NODE_MAIN) main();
