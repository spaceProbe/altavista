// web/js/entities/entities_instanced_layer.js -- H6 scope item 3: "Instanced markers
// and trails, budgeted like the layers... not a simile: they go through the same
// LayerManager discipline in web/js/layers/layer.js -- a declared byte budget that is a
// HARD admission limit, a plan()/load()/release() adapter shape, deferral counted,
// cancellation honoured."
//
// `MarkerLayerAdapter` and `TrailLayerAdapter` below implement EXACTLY `./layer.js`'s
// (`web/js/layers/layer.js`) `Layer` interface, the same interface
// `ImageryLayerAdapter`/`TerrainLayerAdapter`/`Tiles3DLayerAdapter` already implement --
// registered on the SAME `LayerManager` instance those adapters share (`addLayer`), so
// markers/trails compete for the identical byte budget, priority queue and
// cancellation policy as imagery/terrain/3D-Tiles content, never a second, parallel
// budget mechanism (this file owns no eviction/admission logic of its own -- see
// `web/js/layers/layer.js`'s own module docstring, "no second, independently-maintained
// copy of a budget/eviction rule").
//
// What this file deliberately does NOT do, and why (this task's own report covers
// this in full): it does not wire these adapters into `web/js/scene.js`'s live
// `Viewer`/`LayerManager` instance, because this round's own file-ownership rule
// forbids editing `web/js/scene.js`, `web/js/app.js` and `web/js/panels/layers_panel.js`
// (owned by other concurrent workers this round). Registration and budget discipline
// are proven instead against a STANDALONE `LayerManager` in
// `web/js/entities_layer_check.mjs` -- the identical class real wiring would use, just
// not yet driven by the live `Viewer`'s own per-frame `update(view)` call.
//
// "Loading" a marker or trail is not a network fetch -- there is nothing to download,
// the instance data is built from already-in-memory entity state (a position, a
// color, a polyline of already-known points). What `load()` still genuinely owns,
// though, is respecting the `AbortSignal` `LayerManager` hands it (the interface's own
// contract, `./layer.js`'s module docstring: "Must respect signal... reject once it
// aborts"): both adapters below defer their (synchronous, cheap) instance-data
// construction across a microtask specifically so a request that gets cancelled
// between admission and settlement (a real race `web/js/layers/tiles3d_layer.js`'s own
// `cancellationReal` proof exercises for network loads) is genuinely observable here
// too, not merely assumed impossible because "it's all synchronous anyway".
import * as THREE from 'three';

/** One marker instance's declared resident byte cost: a `THREE.InstancedMesh`
 * per-instance entry is a 4x4 matrix (16 floats, `instanceMatrix`) plus an RGB color
 * (`instanceColor`, `THREE.InstancedBufferAttribute` with `itemSize` 3) -- 16*4 + 3*4 =
 * 76 bytes. Declared, documented, not measured off a live GPU buffer (`./layer.js`'s
 * own "declared, documented estimate" convention for `IMAGERY_TILE_BYTES` applied
 * here), because a THREE.InstancedMesh's real GPU-resident layout is implementation
 * detail this codebase does not control per-instance any more than it controls a
 * texture's mipmap chain.
 */
export const MARKER_INSTANCE_BYTES = 16 * 4 + 3 * 4;

/** One trail point's declared resident byte cost: xyz, float32 -- matches
 * `web/js/scene.js`'s own trajectory `LineGeometry` vertex convention (3 floats per
 * point), not a new layout invented for this file.
 */
export const TRAIL_POINT_BYTES = 3 * 4;

/** Fixed per-trail overhead (declared, not measured): one `THREE.BufferGeometry` plus
 * one draw call's worth of bookkeeping. Small and constant, added once per trail
 * regardless of point count, the same "declared, documented" discipline as the two
 * byte constants above -- kept separate from `TRAIL_POINT_BYTES` so a caller/report can
 * see the per-point and fixed components of a trail's cost separately. */
export const TRAIL_FIXED_OVERHEAD_BYTES = 256;

function microtaskLoad(build, signal) {
  return new Promise((resolve, reject) => {
    if (signal.aborted) { reject(signal.reason); return; }
    const onAbort = () => reject(signal.reason);
    signal.addEventListener('abort', onAbort, { once: true });
    Promise.resolve().then(() => {
      signal.removeEventListener('abort', onAbort);
      if (signal.aborted) { reject(signal.reason); return; }
      resolve(build());
    });
  });
}

/**
 * Layer adapter for instanced point markers (e.g. ground stations, debris, waypoints).
 * `view.markers`: `[{id, positionKm:[x,y,z], color?, sizePx?, viewDistanceM, sseError}]`
 * -- position/visibility arithmetic (screen-space size, view distance) is the CALLER's
 * job to compute and hand in via each marker's own `sseError`/`viewDistanceM`, exactly
 * how `ImageryLayerAdapter.plan()` (`../layers/imagery_layer.js`) takes an
 * already-selected `view.tiles` rather than re-deriving tile selection itself -- this
 * adapter does not reimplement `web/js/globe_lod.js`'s screen-space-error math for an
 * arbitrary point (there is no existing "SSE of a point marker" function in this tree
 * to reuse, and inventing one is out of this item's own scope; a caller with a real
 * camera already has the distance and can derive a reasonable proxy sseError itself,
 * e.g. `sizePx` directly, same shape as `screenSpaceErrorPx`'s own return value).
 */
export class MarkerLayerAdapter {
  constructor({ id = 'markers', markerBytes = MARKER_INSTANCE_BYTES, kind = 'entity-marker' } = {}) {
    this.id = id;
    this.markerBytes = markerBytes;
    // Heavy round 7 (H6 wiring, question 233): an explicit, non-'imagery' `kind` --
    // `LayerManager.imageryLayers()` (web/js/layers/layer.js) filters on
    // `layer.kind === 'imagery'`, and this adapter never set `kind` at all before this
    // round, which happened to be a safe default (`undefined !== 'imagery'`) but left
    // the separation implicit rather than stated. Overridable (never hardcoded) only so
    // a headless check can PROVE the separation matters, by constructing one with
    // `kind: 'imagery'` and showing it wrongly appears in `imageryLayers()` -- never
    // overridden by any real caller.
    this.kind = kind;
  }

  plan(view) {
    const { markers = [] } = view;
    return markers.map((m) => ({
      key: m.id,
      sseError: m.sseError,
      viewDistanceM: m.viewDistanceM,
      byteCost: this.markerBytes,
      marker: m,
    }));
  }

  load(request, signal) {
    return microtaskLoad(() => {
      const m = request.marker;
      const matrix = new THREE.Matrix4().makeTranslation(m.positionKm[0], m.positionKm[1], m.positionKm[2]);
      const color = new THREE.Color(m.color || '#ffffff');
      return { key: request.key, matrix, color };
    }, signal);
  }

  release(_key) {}
}

/**
 * Layer adapter for entity trails (a recent-history polyline). `view.trails`:
 * `[{id, pointsKm:[[x,y,z],...], color?, viewDistanceM, sseError}]` -- same division of
 * responsibility as `MarkerLayerAdapter` above (the caller supplies already-computed
 * visibility metrics per trail).
 */
export class TrailLayerAdapter {
  constructor({
    id = 'trails', pointBytes = TRAIL_POINT_BYTES, fixedOverheadBytes = TRAIL_FIXED_OVERHEAD_BYTES, kind = 'entity-trail',
  } = {}) {
    this.id = id;
    this.pointBytes = pointBytes;
    this.fixedOverheadBytes = fixedOverheadBytes;
    // See MarkerLayerAdapter's own comment on `kind`, above -- identical reasoning.
    this.kind = kind;
  }

  plan(view) {
    const { trails = [] } = view;
    return trails.map((tr) => ({
      key: tr.id,
      sseError: tr.sseError,
      viewDistanceM: tr.viewDistanceM,
      byteCost: this.fixedOverheadBytes + tr.pointsKm.length * this.pointBytes,
      trail: tr,
    }));
  }

  load(request, signal) {
    return microtaskLoad(() => {
      const tr = request.trail;
      const positions = new Float32Array(tr.pointsKm.length * 3);
      for (let i = 0; i < tr.pointsKm.length; i++) {
        positions[3 * i] = tr.pointsKm[i][0];
        positions[3 * i + 1] = tr.pointsKm[i][1];
        positions[3 * i + 2] = tr.pointsKm[i][2];
      }
      const geometry = new THREE.BufferGeometry();
      geometry.setAttribute('position', new THREE.BufferAttribute(positions, 3));
      const material = new THREE.LineBasicMaterial({ color: tr.color || '#ffffff' });
      const line = new THREE.Line(geometry, material);
      return { key: request.key, line, pointCount: tr.pointsKm.length };
    }, signal);
  }

  release(key) {
    // Real geometry disposal (matching `ImageryLayerAdapter.release`'s own doc comment
    // on why a texture disposal has nowhere to live inside THIS adapter): the payload
    // is opaque to `LayerManager` (`./layer.js`'s own docstring), so a caller that
    // wants GPU disposal wires its own eviction hook where it actually removes the
    // `THREE.Line` from the scene -- there is nothing this adapter itself retains for
    // `key` beyond what `LayerManager.resident` already owns and releases via this
    // very call.
    void key;
  }
}

/** Build a real `THREE.InstancedMesh` from a `MarkerLayerAdapter`'s CURRENT resident
 * set on `manager` -- the scene-graph counterpart of "assert from the scene graph,
 * never a counter alone" (this round's rule) for markers: a caller/check reads
 * `mesh.count` and `mesh.getMatrixAt(i)`/`mesh.getColorAt(i)` back, not merely
 * `manager.resident.size`. Rebuilds fully each call (this function is a snapshot, not
 * a maintained live binding) -- cheap for the marker counts this module's own check
 * exercises, and it keeps this file from having to track incremental add/remove
 * itself, which `LayerManager` already does via `resident`.
 * @param {THREE.BufferGeometry} geometry per-instance base geometry (e.g. a small
 *   sphere/quad) -- supplied by the caller, this module does not opinion a marker's
 *   visual shape.
 * @param {import('../layers/layer.js').LayerManager} manager
 * @param {string} layerId
 * @returns {THREE.InstancedMesh}
 */
export function buildMarkerInstancedMesh(geometry, manager, layerId) {
  const entries = [...manager.resident.values()].filter((e) => e.layerId === layerId);
  const material = new THREE.MeshBasicMaterial();
  const mesh = new THREE.InstancedMesh(geometry, material, Math.max(1, entries.length));
  mesh.count = entries.length;
  entries.forEach((entry, i) => {
    mesh.setMatrixAt(i, entry.payload.matrix);
    mesh.setColorAt(i, entry.payload.color);
  });
  mesh.instanceMatrix.needsUpdate = true;
  if (mesh.instanceColor) mesh.instanceColor.needsUpdate = true;
  mesh.userData.residentKeys = entries.map((e) => e.localKey);
  return mesh;
}

/** Same scene-graph-truth idea as `buildMarkerInstancedMesh`, for trails: returns a
 * `THREE.Group` containing every currently-resident trail's own real `THREE.Line`
 * (built by `TrailLayerAdapter.load`, retrieved via `payload.line` -- never rebuilt
 * here, so this is a direct scene-graph attachment of the SAME line object `load()`
 * produced, not a copy). */
export function buildTrailGroup(manager, layerId) {
  const group = new THREE.Group();
  for (const entry of manager.resident.values()) {
    if (entry.layerId === layerId) group.add(entry.payload.line);
  }
  return group;
}
