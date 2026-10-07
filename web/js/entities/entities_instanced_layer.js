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
// How the live viewer uses these (heavy cleanup round 1, questions 235/237): `web/js/
// scene.js`'s `Viewer` registers both adapters on its ONE shared `LayerManager` and, every
// render tick, makes exactly one `LayerManager.update(view)` call whose `view` merges the
// globe's, the 3D-Tiles overlay's and this file's `ResidentEntityScene` fragments
// (`../layers/layer.js`'s `updateComposed`) -- so markers and trails are re-planned
// every frame like imagery is, an entity payload the manager evicted is re-admitted
// when it is wanted and fits again, and the scene graph follows what is resident.
// `ResidentEntityScene`, below, is that per-tick participant. The same classes are
// proven headlessly against a real `LayerManager` in `web/js/entities_layer_check.mjs`,
// `web/js/entities_scene_check.mjs` and `web/js/merged_view_check.mjs`, and in a real
// browser by `tests/test_viewer_entities_browser.py` and
// `tests/test_viewer_globe_layer_manager.py`.
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
import { globalKeyFor } from '../layers/layer.js';

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

const NO_ENTITY_REQUESTS = Object.freeze([]);

/** Default entity-marker radius on screen, in CSS pixels (a 10 px disc). The viewer's
 * per-spacecraft `s.marker` sphere is 7 px across (`MARKER_PX` in `web/js/scene.js`), so
 * the two stay distinguishable where they coincide: the entity marker reads as a ring
 * round the sphere. */
export const ENTITY_MARKER_RADIUS_PX = 5;

/**
 * The material of the entity markers: a `MeshBasicMaterial` (per-instance colour, the
 * log-depth and fog chunks) whose vertex stage is replaced so that every instance is a
 * flat disc of a FIXED SIZE IN SCREEN PIXELS, at any camera distance.
 *
 * Why it is done in the vertex shader of the instanced mesh: the mesh is ONE object that
 * every viewport's camera renders (`web/js/scene.js`: the primary renderer and one per
 * extra viewport, each with its own camera, distance and canvas size). Scaling the
 * instances from the CPU once per tick can only use one camera's distance, so the other
 * viewports would see a marker that is sub-pixel or huge (the defect
 * `Viewer._markerReferenceDistance` documents for the per-spacecraft markers). Here the
 * size is resolved per draw call, from the camera and the drawing canvas that draw it:
 *
 *   - the instance CENTRE is projected as usual (`projectionMatrix * modelViewMatrix *
 *     instanceMatrix * (0,0,0,1)`), so the floating-origin positions the viewer writes
 *     into the instance matrices are used exactly as before and the instance's scale is
 *     ignored;
 *   - the geometry's x/y are then added in clip space as `xy * radiusPx * 2 / viewportPx *
 *     w`, i.e. `radiusPx` pixels after the perspective divide, whatever the depth or FOV.
 *     z and w are the centre's, so the disc sits at the centre's depth (and the
 *     logarithmic depth buffer, which reads `gl_Position.w`, is unaffected).
 *
 * `uViewportPx` is one uniform object shared by every renderer's compiled program, set in
 * `mesh.onBeforeRender` (`bindMarkerViewport`) from the renderer about to draw the mesh;
 * renders are synchronous, so the value is the drawing renderer's at upload time.
 *
 * Why not `THREE.Points` with `sizeAttenuation: false`: the markers are an
 * `InstancedMesh` of a shared geometry on purpose (the residency model above, the
 * per-instance colour and the `count` the proofs read back are that class's), and a
 * point's size is capped by the GL implementation's point-size range.
 *
 * @param {object} [opts]
 * @param {number} [opts.radiusPx] marker radius in CSS pixels (default `ENTITY_MARKER_RADIUS_PX`)
 * @returns {THREE.MeshBasicMaterial} with `userData.markerUniforms` = `{uMarkerRadiusPx, uViewportPx}`
 */
export function createScreenSpaceMarkerMaterial({ radiusPx = ENTITY_MARKER_RADIUS_PX } = {}) {
  if (!(Number.isFinite(radiusPx) && radiusPx > 0)) {
    throw new TypeError(`createScreenSpaceMarkerMaterial: radiusPx must be a finite number > 0, got ${radiusPx}`);
  }
  const uniforms = {
    uMarkerRadiusPx: { value: radiusPx },
    uViewportPx: { value: new THREE.Vector2(1, 1) },
  };
  const material = new THREE.MeshBasicMaterial({ vertexColors: true });
  material.userData.markerUniforms = uniforms;
  material.onBeforeCompile = (shader) => {
    shader.uniforms.uMarkerRadiusPx = uniforms.uMarkerRadiusPx;
    shader.uniforms.uViewportPx = uniforms.uViewportPx;
    const marker = [
      'vec4 mvPosition = vec4( 0.0, 0.0, 0.0, 1.0 );',
      '#ifdef USE_INSTANCING',
      '\tmvPosition = instanceMatrix * mvPosition;',
      '#endif',
      'mvPosition = modelViewMatrix * mvPosition;',
      'gl_Position = projectionMatrix * mvPosition;',
      'gl_Position.xy += transformed.xy * ( uMarkerRadiusPx * 2.0 / uViewportPx ) * gl_Position.w;',
    ].join('\n');
    if (!shader.vertexShader.includes('#include <project_vertex>')) {
      throw new Error('createScreenSpaceMarkerMaterial: the vertex shader has no project_vertex chunk to replace');
    }
    shader.vertexShader = 'uniform float uMarkerRadiusPx;\nuniform vec2 uViewportPx;\n'
      + shader.vertexShader.replace('#include <project_vertex>', marker);
  };
  // Distinct program-cache key: never share a compiled program with an ordinary MeshBasicMaterial.
  material.customProgramCacheKey = () => 'entity-marker-screen-space';
  return material;
}

/** Point `material`'s `uViewportPx` at the drawing area of `renderer` (CSS pixels): the
 * size the screen-space marker is measured against. Called from `mesh.onBeforeRender`. */
export function bindMarkerViewport(material, renderer, target = new THREE.Vector4()) {
  renderer.getViewport(target);
  material.userData.markerUniforms.uViewportPx.value.set(Math.max(target.z, 1), Math.max(target.w, 1));
}

/**
 * The entity markers' and trails' participant in the merged per-tick view, and the
 * scene-graph side of their residency (heavy cleanup round 1, questions 235/237). One
 * instance per loaded scenario; `web/js/scene.js` builds it in `_buildEntities()` and
 * disposes it in `clear()`, and `../layers/layer.js`'s `updateComposed` drives it:
 *
 *   - `planView()` is this scene's fragment of the composed view, `{markers, trails}`.
 *     The two lists are built ONCE, in the constructor, from `entities`, and handed back
 *     unchanged every tick: same local keys (the entity names), same declared byte
 *     costs (each adapter's own `plan()` derives them from the unchanged data), so for
 *     an already-resident payload the manager's re-plan is a no-op (`LayerManager.
 *     update()` refreshes `lastUsedStep` and finds the stored `byteCost` equal -- no
 *     load, no revision), never a reload per tick. A class that `isEnabled('markers')`/
 *     `isEnabled('trails')` reports off (the viewer passes its `entityOptions`)
 *     contributes an empty list: its requests are then not wanted, so it stops
 *     spending the shared budget -- its resident payloads stay until the manager
 *     actually needs the room (then it evicts them, `LayerManager._evictEntry`), and
 *     switching the class back on re-plans them (still resident: free; evicted:
 *     re-admitted like any other request once it fits).
 *   - `commit()` (= `sync()`) runs after the composed update and reads the manager's
 *     own `resident` map into the scene graph: a marker is an instance of the
 *     `InstancedMesh` -- and a trail a `THREE.Line` in `trailGroup` -- exactly while its
 *     payload is resident. No second residency record is kept: the instance order
 *     (`markerNames`), the mesh's `count` and the set of trail lines are recomputed
 *     from `manager.resident` every tick and only rewritten when that read differs.
 *
 * Why instances are compacted rather than hidden in place: the mesh is allocated once
 * with capacity for every entity (the number of entities is fixed for a scenario), and
 * `mesh.count` is set to the number of resident markers with their colours rewritten in
 * resident order. That keeps `markerNames[i]` <-> instance `i` a plain parallel pair,
 * which is what `Viewer._updateEntityMarkers` and the browser proofs index by, and a
 * hidden-in-place instance (scale 0) would still be a drawn instance. A trail's
 * `THREE.Line` is created when its payload becomes resident and disposed when it stops
 * being, so GPU memory follows residency too. Positions are NOT taken from the payloads
 * (they bake raw absolute km, see `web/js/scene.js`'s `_updateEntityMarkers`); the
 * viewer overwrites each resident instance/line every tick through the floating-origin
 * pipeline, reading `markerNames`/`trailLines` back from here.
 *
 * @param {object} opts
 * @param {import('../layers/layer.js').LayerManager} opts.manager the shared manager; the
 *   adapters must already be registered on it under `markerLayerId`/`trailLayerId`
 * @param {string} opts.markerLayerId
 * @param {string} opts.trailLayerId
 * @param {THREE.Object3D} opts.markerGroup receives the marker `InstancedMesh`
 * @param {THREE.Object3D} opts.trailGroup receives one `THREE.Line` per resident trail
 * @param {THREE.BufferGeometry} opts.markerGeometry SHARED per-instance geometry; never
 *   disposed here
 * @param {Array<{name:string, positionKm:number[], color?:string, trailPointsKm:number[][]}>} opts.entities
 * @param {number} opts.trailMaxPoints capacity of each trail line's position buffer
 * @param {(cls: 'markers'|'trails') => boolean} [opts.isEnabled] read fresh every tick (default: both on)
 * @param {number} [opts.markerRadiusPx] on-screen marker radius in CSS pixels, constant at
 *   any camera distance in every viewport (`createScreenSpaceMarkerMaterial`); the markers
 *   have no size in scene units, and `markerGeometry` only needs its x/y in [-1, 1]
 */
export class ResidentEntityScene {
  constructor({
    manager, markerLayerId, trailLayerId, markerGroup, trailGroup, markerGeometry, entities, trailMaxPoints,
    isEnabled = () => true, markerRadiusPx = ENTITY_MARKER_RADIUS_PX,
  }) {
    this.manager = manager;
    this.markerLayerId = markerLayerId;
    this.trailLayerId = trailLayerId;
    this.markerGroup = markerGroup;
    this.trailGroup = trailGroup;
    this.trailMaxPoints = trailMaxPoints;
    this._entities = entities;
    this._isEnabled = isEnabled;
    this._markerView = entities.map((e) => ({
      id: e.name, positionKm: e.positionKm, color: e.color || '#ffffff', sseError: 1, viewDistanceM: 1,
    }));
    this._trailView = entities.map((e) => ({
      id: e.name, pointsKm: e.trailPointsKm, color: e.color || '#ffffff', sseError: 1, viewDistanceM: 1,
    }));
    this._colorByName = new Map(entities.map((e) => [e.name, new THREE.Color(e.color || '#ffffff')]));

    /** Names whose marker payload is resident, in `entities` order; instance `i` of
     * `markerMesh` is `markerNames[i]`. */
    this.markerNames = [];
    /** @type {THREE.InstancedMesh|null} null only when the scenario has no entities */
    this.markerMesh = null;
    if (entities.length) {
      const material = createScreenSpaceMarkerMaterial({ radiusPx: markerRadiusPx });
      const mesh = new THREE.InstancedMesh(markerGeometry, material, entities.length);
      mesh.count = 0;
      // A fixed on-screen size, resolved per draw call from the drawing renderer (see
      // `createScreenSpaceMarkerMaterial`): the mesh is rendered by several cameras/canvases.
      const viewport = new THREE.Vector4();
      mesh.onBeforeRender = (renderer) => bindMarkerViewport(material, renderer, viewport);
      // Never frustum-culled: `InstancedMesh` caches ONE bounding sphere the first time it is
      // frustum-tested and never recomputes it, while the instances move every tick (a
      // floating-origin rebase shifts every render-space coordinate, by up to ~7 scene
      // units at LEO), so a sphere cached for an earlier position culls the mesh while its
      // instances are in view (reproduced in web/js/entities_marker_material_check.mjs). The
      // draw is a handful of discs, so the cull saved nothing anyway.
      mesh.frustumCulled = false;
      entities.forEach((e, i) => mesh.setColorAt(i, this._colorByName.get(e.name)));
      mesh.userData.sourceLayerId = markerLayerId;
      mesh.userData.residentKeys = this.markerNames;
      markerGroup.add(mesh);
      this.markerMesh = mesh;
    }
    /** @type {Map<string, {line: THREE.Line, positions: Float32Array, maxPoints: number}>} resident trails only */
    this.trailLines = new Map();
  }

  /** This scene's fragment of the composed view; `null` (not participating) when the
   * scenario has no entities at all. */
  planView() {
    if (!this._entities.length) return null;
    return {
      markers: this._isEnabled('markers') ? this._markerView : NO_ENTITY_REQUESTS,
      trails: this._isEnabled('trails') ? this._trailView : NO_ENTITY_REQUESTS,
    };
  }

  commit() { this.sync(); }

  /** Make the scene graph match `manager.resident` -- see the class docstring. */
  sync() {
    const resident = this.manager.resident;
    const names = [];
    this._entities.forEach((e) => {
      if (resident.has(globalKeyFor(this.markerLayerId, e.name))) names.push(e.name);
    });
    const mesh = this.markerMesh;
    if (mesh && (names.length !== this.markerNames.length || names.some((n, i) => n !== this.markerNames[i]))) {
      this.markerNames = names;
      mesh.userData.residentKeys = names;
      mesh.count = names.length;
      names.forEach((n, i) => mesh.setColorAt(i, this._colorByName.get(n)));
      if (mesh.instanceColor) mesh.instanceColor.needsUpdate = true;
    }

    for (const e of this._entities) {
      const isResident = resident.has(globalKeyFor(this.trailLayerId, e.name));
      const rec = this.trailLines.get(e.name);
      if (isResident && !rec) {
        const positions = new Float32Array(this.trailMaxPoints * 3);
        const geometry = new THREE.BufferGeometry();
        geometry.setAttribute('position', new THREE.BufferAttribute(positions, 3));
        geometry.setDrawRange(0, 0);
        const line = new THREE.Line(geometry, new THREE.LineBasicMaterial({ color: new THREE.Color(e.color || '#ffffff') }));
        line.userData.sourceLayerId = this.trailLayerId;
        line.userData.spacecraft = e.name;
        line.frustumCulled = false;
        this.trailGroup.add(line);
        this.trailLines.set(e.name, { line, positions, maxPoints: this.trailMaxPoints });
      } else if (!isResident && rec) {
        this._disposeTrail(e.name, rec);
      }
    }
  }

  _disposeTrail(name, rec) {
    this.trailGroup.remove(rec.line);
    rec.line.geometry.dispose();
    rec.line.material.dispose();
    this.trailLines.delete(name);
  }

  /** Remove everything this scene put in the graph (the shared marker geometry is left
   * alone). Safe to call more than once. */
  dispose() {
    if (this.markerMesh) {
      this.markerGroup.remove(this.markerMesh);
      this.markerMesh.material.dispose();
      this.markerMesh.dispose();
      this.markerMesh = null;
    }
    this.markerNames = [];
    for (const [name, rec] of [...this.trailLines]) this._disposeTrail(name, rec);
  }
}
