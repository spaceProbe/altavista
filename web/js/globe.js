// The tiled globe's Three.js renderer: turns web/js/globe_lod.js's tile selection into
// actual BufferGeometry + imagery textures. This is the only file that imports Three.js
// for the globe (globe_lod.js is framework-free -- see its module docstring); keeping
// the split means web/js/globe_lod_check.mjs (the headless determinism/budget/
// cancellation harness, tests/test_viewer_globe.py) never needs a WebGL context.
//
// Floating-origin compatibility (docs/open-questions.md Q46, this task's hard
// constraint): tile vertices are built directly in **body-local** units -- WGS84 ECEF
// metres converted to scene units (globe_lod.js's `SCENE_UNITS_PER_METRE`, via
// `tileVertexPositions`), *not* the body's absolute (origin-frame) position. A body's
// radius in scene units is always small (Earth: ~6.378, SCALE=1e-3 km/unit -- see
// web/js/scene.js's SCALE) regardless of how far the body itself is from the current
// floating-origin origin, so these vertices never need the f64-subtract-then-f32-cast
// pipeline origin.js provides for absolute (Mars-distance-scale) coordinates -- the
// same reason web/js/scene.js's existing `makeBodyMesh()` sphere geometry is built in
// unit-sphere-then-`mesh.scale()` space rather than absolute coordinates.
// `GlobeLayer.group` is positioned/oriented every tick to exactly track the body's own
// already-origin-relative `mesh.position`/`mesh.quaternion` (see `Viewer.
// _syncGlobeLayer` in scene.js) -- it is parented as a sibling of the body mesh, not a
// child of it (so tile vertices are never additionally scaled by the body mesh's own
// `(r,r,r*(1-flattening))` scale factor; ellipsoid flattening is instead baked
// directly into each tile vertex via WGS84_A_M/WGS84_B_M in globe_lod.js's
// geodeticToEcef()). This is what makes "the globe must be compatible
// with the per-frame floating origin: a target-centred RPO view must keep centimetre
// precision while the globe is visible" true by construction: the globe never writes
// into the *entities* frame's shared, origin-relative vertex buffers (trajectory
// lines) that the RPO precision bound is about -- see
// web/js/scene_jitter_harness.mjs's `measureRpoWithGlobePresent()` for the proof this
// task requires, which builds a real GlobeLayer alongside the RPO measurement and
// confirms the trajectory numbers are unaffected.
import * as THREE from 'three';
import {
  selectTiles, TileLoadScheduler, tileKey, tileVertexPositions,
  WGS84_A_M, WGS84_B_M, SCENE_UNITS_PER_METRE,
} from './globe_lod.js';
// Round 4 (docs/open-questions.md question 228 finding 2, "wire the globe through the
// LayerManager"): imported from the INDIVIDUAL files, not the barrel
// (`./layers/index.js`) -- deliberately, not a style choice. `./layers/imagery_layer.js`
// already imports `urlForTile` from THIS file (`../globe.js`); if this file imported the
// barrel instead, that barrel's own re-export of `./layers/gateway_imagery_layer.js`
// (`class GatewayImageryLayerAdapter extends ImageryLayerAdapter`) would get pulled into
// the SAME synchronous circular-evaluation chain this import creates, and Node evaluates
// that `extends` clause before `imagery_layer.js`'s own `class ImageryLayerAdapter`
// declaration has run in that specific cycle ordering -- a real
// `ReferenceError: Cannot access 'ImageryLayerAdapter' before initialization`, caught
// live while building this task (`node web/js/layers_check.mjs` failing) and root-caused
// to exactly this, not papered over by reordering or a dynamic import. Importing the
// two leaf files this module actually needs sidesteps the barrel's own wider cycle
// entirely: `./layers/imagery_layer.js`/`./layers/terrain_layer.js` are only ever
// dereferenced inside `GlobeLayer`'s constructor/methods (never at this module's own
// top level or in a class `extends` clause), so the remaining globe.js<->imagery_layer.js
// cycle itself is safe (see globe_lod.js's own note on the module-load-order
// discipline this codebase already applies for a comparable case).
import { ImageryLayerAdapter } from './layers/imagery_layer.js';
import { TerrainLayerAdapter } from './layers/terrain_layer.js';
import { globalKeyFor } from './layers/layer.js';

const DEFAULT_IMAGERY_URL = './fixtures/tiles/{z}/{x}/{y}.png';
const DEFAULT_SEGMENTS = 8; // quads per tile edge

/** WGS84 outward surface normal at ECEF point (x,y,z) -- `grad(x^2/a^2+y^2/a^2+z^2/b^2)`,
 * normalized. Not the same as `normalize(position)` once flattening is nonzero
 * (b < a), which is exactly why this is computed explicitly rather than reusing the
 * position vector as its own normal (a common shortcut that is subtly wrong for an
 * oblate ellipsoid, most visible near the poles). */
function ellipsoidNormal(x, y, z, out) {
  const a2 = WGS84_A_M * WGS84_A_M, b2 = WGS84_B_M * WGS84_B_M;
  out.set(x / a2, y / a2, z / b2).normalize();
  return out;
}

// Exported (M26.4) so web/js/panels/map_panel.js's 2D companion map can build the exact
// same tile URLs the 3D globe requests for a given imagery profile, rather than a second
// copy of this one-line template substitution -- "the same imagery profile as the globe"
// (docs/ui-rework-plan.md's M26.4 wording) means literally the same URL-building
// function, not merely the same urlTemplate string passed to two independent
// implementations that could silently drift.
export function urlForTile(template, tile) {
  return template.replace('{z}', String(tile.level)).replace('{x}', String(tile.x)).replace('{y}', String(tile.y));
}

/** One tile's renderable mesh: BufferGeometry (position, normal, uv) built directly in
 * body-local scene units (see module docstring) + a texture loaded from `imageryUrl`.
 * A tile that fails to load (fixture doesn't cover it, or a real tile gateway 404s)
 * falls back to a flat colour material -- mirrors web/js/scene.js's `makeBodyMesh()`
 * texture-load-failure fallback exactly, never a hard error (this task's honesty
 * requirement: no unrecorded approximation -- the fallback is visible, not silent, via
 * `mesh.userData.imageryLoaded`).
 */
/** Apply a loaded texture payload to a tile mesh's material -- exactly the steps
 * `buildTileMesh`'s own direct-load callback (below) already performed inline;
 * factored out (round 4, question 228's wiring task) so the SAME application logic
 * runs whether the texture arrived via `buildTileMesh`'s own direct,
 * `textureLoader.load()` call (`layerManager` absent) or via
 * `LayerManager.getResidentPayload()` once a manager-driven load has genuinely
 * completed (`layerManager` present, see `GlobeLayer.update()`) -- never two
 * independently-written copies of "how a loaded texture becomes visible".
 */
function applyTileTexture(mesh, tex) {
  tex.colorSpace = THREE.SRGBColorSpace;
  mesh.material.map = tex;
  mesh.material.color.set(0xffffff);
  mesh.material.needsUpdate = true;
  mesh.userData.imageryLoaded = true;
}

function buildTileMesh(tile, imageryUrl, textureLoader, segments = DEFAULT_SEGMENTS, skipDirectLoad = false) {
  const n = segments + 1;
  // The exact vertex-position math scene_jitter_harness.mjs's
  // measureRpoWithGlobePresent() also calls (see globe_lod.js's docstring) -- f64
  // body-local scene units, converted to f32 only here at the very end (GPU upload),
  // never reimplemented.
  const f64positions = tileVertexPositions(tile, segments);
  const positions = new Float32Array(f64positions);
  const normals = new Float32Array(n * n * 3);
  const uvs = new Float32Array(n * n * 2);
  const nrm = new THREE.Vector3();
  let vi = 0, ui = 0;
  for (let j = 0; j < n; j++) {
    for (let i = 0; i < n; i++) {
      // ellipsoidNormal's gradient direction is invariant to uniformly scaling
      // (x,y,z) together (see globe.js's own reasoning in this comment's history / the
      // task report) -- WGS84_A_M/WGS84_B_M stay in metres regardless, so this is
      // correct even though `positions` is in scene units, not ECEF metres.
      ellipsoidNormal(positions[vi], positions[vi + 1], positions[vi + 2], nrm);
      normals[vi] = nrm.x; normals[vi + 1] = nrm.y; normals[vi + 2] = nrm.z;
      vi += 3;
      uvs[ui] = i / segments; uvs[ui + 1] = 1 - j / segments;
      ui += 2;
    }
  }
  const indices = new Uint32Array(segments * segments * 6);
  let idx = 0;
  for (let j = 0; j < segments; j++) {
    for (let i = 0; i < segments; i++) {
      const a = j * n + i, b = a + 1, c = a + n, d = c + 1;
      indices[idx++] = a; indices[idx++] = c; indices[idx++] = b;
      indices[idx++] = b; indices[idx++] = c; indices[idx++] = d;
    }
  }
  const geometry = new THREE.BufferGeometry();
  geometry.setAttribute('position', new THREE.BufferAttribute(positions, 3));
  geometry.setAttribute('normal', new THREE.BufferAttribute(normals, 3));
  geometry.setAttribute('uv', new THREE.BufferAttribute(uvs, 2));
  geometry.setIndex(new THREE.BufferAttribute(indices, 1));

  const material = new THREE.MeshPhongMaterial({ color: 0x3a5a78, shininess: 2 });
  const mesh = new THREE.Mesh(geometry, material);
  mesh.userData.tile = tile;
  mesh.userData.imageryLoaded = false;
  // `skipDirectLoad` (round 4, question 228's wiring task): true only when this
  // mesh's owning `GlobeLayer` was constructed with a `layerManager` -- the texture
  // then arrives through that manager's own admitted, genuinely cancellable load
  // instead (`GlobeLayer.update()`, below, applies it via `applyTileTexture` once
  // `LayerManager.getResidentPayload()` says it is actually resident). A load fired
  // directly here could never be genuinely cancelled on camera motion -- a real
  // `THREE.TextureLoader`'s underlying XHR/fetch has no public abort hook (see
  // `web/js/layers/imagery_layer.js`'s own docstring) -- so routing it through the
  // manager instead is the whole point of this task's wiring.
  // Every EXISTING call site (every headless check in this repo --
  // `web/js/globe_imagery_check.mjs`, `web/js/globe_lod_check.mjs`,
  // `web/js/scene_jitter_harness.mjs` -- and every call from `GlobeLayer` itself
  // when no `layerManager` was given) omits this argument, so their behaviour -- an
  // immediate, synchronous `textureLoader.load()` call for every newly-built mesh --
  // is unchanged, byte for byte.
  if (!skipDirectLoad) {
    textureLoader.load(
      urlForTile(imageryUrl, tile),
      (tex) => applyTileTexture(mesh, tex),
      undefined,
      () => { /* graceful fallback: keep the flat colour material, never throw */ },
    );
  }
  return mesh;
}

/**
 * A quadtree-tiled WGS84 globe layer: `group` is a plain `THREE.Group` the caller
 * parents/positions however it likes (see scene.js's `_syncGlobeLayer`, which tracks a
 * body's own already-origin-relative transform). `update(cameraLocalPos, ...)` expects
 * the camera's position **already expressed in this globe's own body-local, body-fixed
 * frame**, in scene units (small magnitude, see module docstring) -- the caller is
 * responsible for that `worldToLocal` transform (scene.js does it once per tick, using
 * the same body `Group` this layer's `group` is synced to), so this module never has
 * to know about the frame graph or the floating origin at all.
 */
export class GlobeLayer {
  constructor(opts = {}) {
    this.imageryUrl = opts.imageryUrl || DEFAULT_IMAGERY_URL;
    this.sseThreshold = opts.sseThreshold ?? 24;
    this.maxLevel = opts.maxLevel ?? 2;      // kept low by default -- matches the small offline fixture pyramid (web/fixtures/gen_globe_tiles.py); a real tile gateway would raise this.
    this.maxTiles = opts.maxTiles ?? 128;
    this.segments = opts.segments ?? DEFAULT_SEGMENTS;
    this.group = new THREE.Group();
    this.group.name = 'globe-layer';
    this.scheduler = new TileLoadScheduler({ residentBudget: opts.residentBudget ?? 64 });
    // Injectable (M19.5, question 132's headline test): a real THREE.TextureLoader's
    // constructor is DOM-free (only .load() touches an <img>/network -- see
    // web/vendor/three/three.core.js's TextureLoader), so the default is always safe to
    // construct, but web/js/globe_imagery_check.mjs needs to intercept every requested
    // URL without touching the network at all (this task's binding rule) -- opts.
    // textureLoader (any object with a `.load(url, onLoad, onProgress, onError)`
    // method) lets it substitute a recording stub for the real loader. Every existing
    // call site (scene.js's enableGlobe) omits this option, so production behaviour is
    // byte-for-byte unchanged: a real THREE.TextureLoader, exactly as before.
    this.textureLoader = opts.textureLoader || new THREE.TextureLoader();
    /** @type {Map<string, THREE.Mesh>} */
    this._meshes = new Map();
    this._lastSelectedKeys = new Set();

    // Round 4 (docs/open-questions.md question 228 finding 2, "wire the globe
    // through the LayerManager"): OPTIONAL -- `opts.layerManager`, a
    // `web/js/layers/` `LayerManager` (`./layers/index.js`) the CALLER owns (one per
    // viewer -- see `web/js/scene.js`'s `Viewer` constructor -- "the whole point of
    // a byte budget is that three layers ... share it", `./layers/layer.js`'s own
    // module docstring).
    //
    // Absent (every headless check in this repo -- `web/js/globe_lod_check.mjs`,
    // `web/js/globe_imagery_check.mjs`, `web/js/scene_jitter_harness.mjs` -- and
    // every pre-round-4 call site): behaviour is UNCHANGED, BY CONSTRUCTION --
    // nothing below this `if` ever runs, `this.scheduler` (unchanged,
    // `TileLoadScheduler`) is still what drives admission/eviction bookkeeping
    // exactly as before, and `buildTileMesh` still fires its own direct, immediate,
    // synchronous `textureLoader.load()` call for every newly-selected tile. Proof:
    // `web/js/globe_lod_check.mjs`/`web/js/tiles3d_check.mjs` (untouched,
    // `globe_lod.js` itself is not part of this task) and every pytest in
    // `tests/test_viewer_globe.py` keep passing byte-for-byte identical output (see
    // this task's own report).
    //
    // Present (`web/js/scene.js`'s own `enableGlobe()`, the real, live viewer path
    // -- round 4's own fix for "nothing in app.js or the scene imports
    // web/js/layers/ ... a user cannot see a gateway tile set at all", question
    // 228's finding 2): this layer registers a REAL `ImageryLayerAdapter` -- the
    // SAME `imageryUrl`/`textureLoader` this instance already owns, never a second
    // copy of either -- on the shared manager under a stable id (`imageryLayerId`,
    // default `'imagery'`), and a `TerrainLayerAdapter` under `terrainLayerId`
    // (default `'terrain'`) purely so terrain's own typed, named refusal
    // (`TerrainLoaderNotImplementedError`, `./layers/terrain_layer.js`) genuinely
    // participates in the manager's failure memory (asked once, not once per frame
    // forever -- see that class's own module docstring) instead of never being
    // asked at all -- there is still no terrain LOADER anywhere in this codebase
    // (`web/VIEWER.md`'s "Explicitly not built"), and this wiring does not add one.
    // `update()` (below) then takes its ADMISSION/EVICTION/CANCELLATION decision --
    // which tile's texture is actually allowed to appear, and aborting an in-flight
    // load that drops out of the plan -- from the manager instead of
    // `this.scheduler`; the LOD SELECTION itself (`selectTiles()`) and the
    // mesh-building/disposal SHAPE are unchanged either way (see `update()`'s own
    // comment: "the same tiles are selected as before the change" is a design
    // invariant, not an accident).
    //
    // Stable, not per-instance, ids: `web/js/scene.js`'s `enableGlobe()`/
    // `disableGlobe()` construct and dispose a FRESH `GlobeLayer` on every call
    // against the SAME long-lived manager -- `dispose()` (below) unregisters this
    // instance's own adapters (`LayerManager.removeLayer`, new this task) so a
    // later `enableGlobe()` call can register fresh ones under the same ids without
    // colliding with `addLayer`'s own already-registered guard.
    this.layerManager = opts.layerManager || null;
    this.imageryLayerId = opts.imageryLayerId || 'imagery';
    this.terrainLayerId = opts.terrainLayerId || 'terrain';
    if (this.layerManager) {
      this._imageryAdapter = new ImageryLayerAdapter({
        id: this.imageryLayerId, imageryUrl: this.imageryUrl, loader: this.textureLoader,
      });
      this.layerManager.addLayer(this._imageryAdapter);
      this._terrainAdapter = new TerrainLayerAdapter({ id: this.terrainLayerId });
      this.layerManager.addLayer(this._terrainAdapter);
    }
  }

  /**
   * Recompute the tile selection for `cameraLocalPos` (scene units, body-local -- see
   * class docstring) and reconcile `group`'s children to match: build+add a mesh for
   * every newly selected tile, dispose+remove one for every tile that fell out of
   * selection. `screenHeightPx`/`fovYRad` are passed straight through to
   * `selectTiles()` (web/js/globe_lod.js) -- the real screen-space-error LOD
   * computation, not reimplemented here.
   *
   * Round 5 (question 228/decision 9) note, not a signature change: when a
   * `web/js/tiles_layer.js` `TilesOverlayLayer` is ALSO registered on this SAME
   * `layerManager` (`web/js/scene.js`'s `loadTilesOverlay()`), the `layerManager.
   * update()` call below already carries everything `Tiles3DLayerAdapter.plan()`
   * needs (`cameraEcef`/`screenHeightPx`/`fovYRad`, the same field names and the same
   * body-centred-ECEF-metres convention that adapter uses -- see tiles_layer.js's own
   * module docstring, "both ECEF-metre consumers in this codebase agree on scale") --
   * so `scene.js` deliberately does NOT also drive a second, independent
   * `layerManager.update()` call for the overlay that tick (`TilesOverlayLayer`'s own
   * `selfDriveManager: false` mode, used exactly when a globe is active). Two
   * independent per-tick calls on ONE shared manager would each see the OTHER
   * layer's content as "not wanted" (`LayerManager.update(view)` treats every
   * registered layer's `plan(view)` output as THIS tick's entire wanted set, layer.js's
   * own module docstring) -- cancelling in-flight loads and evicting resident entries
   * that are still genuinely wanted, every single frame. See `web/js/tiles_layer.js`'s
   * own "Round 5" module docstring and `tests/test_viewer_tiles3d_manager.py` for the
   * live proof this does not happen. This method itself needs no change for any of
   * that: it already builds and sends exactly the view a co-registered `Tiles3D
   * LayerAdapter` needs, it simply did not have one registered before this task.
   */
  update(cameraLocalPos, screenHeightPx, fovYRad) {
    const cameraEcef = {
      x: cameraLocalPos.x / SCENE_UNITS_PER_METRE,
      y: cameraLocalPos.y / SCENE_UNITS_PER_METRE,
      z: cameraLocalPos.z / SCENE_UNITS_PER_METRE,
    };
    const tiles = selectTiles(cameraEcef, {
      screenHeightPx, fovYRad, sseThreshold: this.sseThreshold, maxLevel: this.maxLevel, maxTiles: this.maxTiles,
    });

    // Round 4: which tiles keep a mesh this tick is ALWAYS exactly this tick's own
    // LOD selection (`tiles`) -- true before this task too (`TileLoadScheduler.
    // update()` never filtered its own return value by `residentBudget` either, see
    // globe_lod.js) and kept true here deliberately, in BOTH branches below: this
    // task's own binding rule is "the same tiles are selected as before the
    // change", and what a `layerManager` actually decides (below) is the narrower
    // question of whether a given mesh's TEXTURE is allowed to appear yet -- never
    // whether the mesh itself exists.
    let selectedKeys;
    if (this.layerManager) {
      // The real admission/eviction/cancellation decision: `view.tiles` is this
      // tick's own LOD selection, `plan()`'d by both the registered imagery and
      // terrain adapters against the SAME `cameraEcef`/`screenHeightPx`/`fovYRad`
      // this function already computes screen-space error from -- no second
      // camera-state shape invented for this wiring. A tile no longer in `tiles`
      // this tick is no longer "wanted" the instant this call returns, so
      // `LayerManager.update()`'s own cancellation loop aborts its real, in-flight
      // `ImageryLayerAdapter.load()` (a real `textureLoader.load()` tied to a real
      // `AbortSignal`) synchronously, at `abort()` time -- genuinely real
      // cancellation on camera motion, a property `this.scheduler`'s own
      // (nothing-ever-listens-to-it) `AbortController` never had.
      this.layerManager.update({
        tiles, cameraEcef, screenHeightPx, fovYRad,
      });
      selectedKeys = new Set(tiles.map(tileKey));
    } else {
      selectedKeys = this.scheduler.update(tiles);
    }

    for (const tile of tiles) {
      const k = tileKey(tile);
      if (!this._meshes.has(k)) {
        // `skipDirectLoad` (last arg) -- see `buildTileMesh`'s own comment -- true
        // only in `layerManager` mode: the texture arrives through the manager's
        // own admitted load instead (applied just below, once genuinely resident).
        const mesh = buildTileMesh(tile, this.imageryUrl, this.textureLoader, this.segments, !!this.layerManager);
        this._meshes.set(k, mesh);
        this.group.add(mesh);
      }
    }

    if (this.layerManager) {
      // Round 6 (docs/open-questions.md question 231's ruling, "replace or
      // composite" -- docs/heavy-plan.md's round-5 status, "the one thing round 5
      // does NOT deliver: a selected tile set is streamed, not drawn"): a mesh's
      // texture is bound from whichever REGISTERED imagery layer is topmost FOR
      // THIS TILE, not from a fixed id -- walk `layerManager.imageryLayers()`
      // (./layers/layer.js's own ordered, read-only accessor: every registered
      // layer whose `kind === 'imagery'`, in REGISTRATION/list order) and take the
      // LAST one that actually has a resident payload for this tile's globalKey.
      // This class's own default `'imagery'` adapter is itself just one entry in
      // that same list -- registered FIRST, in this class's own constructor above
      // -- so it is exactly what a mesh falls back to when nothing toggled on above
      // it has anything resident yet, and exactly what is restored the instant a
      // higher layer is unregistered (`web/js/app.js`'s `toggleGatewayLayer`'s
      // `removeLayer` call, on the very next `update()` tick) -- never a second,
      // hand-rolled notion of "topmost" or "default" here.
      //
      // Re-evaluated EVERY tick for EVERY mesh (unlike the pre-round-6 version of
      // this loop, which bound a mesh's texture once and never revisited it): a
      // toggle-off/toggle-on, or a still-loading topmost layer's tile resolving on a
      // LATER tick than the layer(s) under it, must change what a mesh shows without
      // a page reload, and only a per-tick recompute can do that. `mesh.material.map
      // !== chosenTex` is a cheap guard against reapplying the identical texture
      // object every single tick when nothing actually changed -- `applyTileTexture`
      // itself is idempotent (setting the same map again would be harmless, just
      // wasteful).
      //
      // `chosenTex === undefined` (no registered layer has ANYTHING resident yet for
      // this tile -- e.g. the first few ticks, before even the default has loaded)
      // deliberately leaves `mesh.material.map` exactly as it was: the flat
      // placeholder colour `buildTileMesh` starts every mesh with, or whatever was
      // bound on an earlier tick -- this class's own "graceful fallback, never
      // throw" contract (module docstring) restated as "never clear a bound texture
      // back to nothing", which is also this task's own required proof 4 ("the
      // globe's own two meshes keep a texture at all times ... once the default has
      // loaded").
      for (const [k, mesh] of this._meshes) {
        let chosenTex;
        for (const layer of this.layerManager.imageryLayers()) {
          const tex = this.layerManager.getResidentPayload(globalKeyFor(layer.id, k));
          if (tex !== undefined) chosenTex = tex; // later (topmost) registered layer wins, per tile
        }
        if (chosenTex !== undefined && mesh.material.map !== chosenTex) {
          applyTileTexture(mesh, chosenTex);
        }
      }
    }

    for (const [k, mesh] of this._meshes) {
      if (!selectedKeys.has(k)) {
        this.group.remove(mesh);
        mesh.geometry.dispose();
        mesh.material.map?.dispose();
        mesh.material.dispose();
        this._meshes.delete(k);
      }
    }

    if (!this.layerManager) {
      // Unchanged: keep the scheduler's resident bookkeeping in step with what is
      // actually built (unlike globe_lod_check.mjs's offline harness, the browser
      // render path doesn't need to simulate a slow multi-frame load queue -- a
      // mesh with a fallback-colour material renders immediately, and the texture
      // swaps in asynchronously). Only reached when no `layerManager` was given --
      // in `layerManager` mode the manager's own `update()` (above) already did the
      // equivalent real bookkeeping.
      this.scheduler.completeLoads(this._meshes.size, selectedKeys);
    }
    this._lastSelectedKeys = selectedKeys;
    return tiles;
  }

  dispose() {
    for (const mesh of this._meshes.values()) {
      mesh.geometry.dispose();
      mesh.material.map?.dispose();
      mesh.material.dispose();
    }
    this._meshes.clear();
    // Round 4: unregister this instance's own adapters from the shared manager (if
    // any) -- see the constructor's own comment: `web/js/scene.js`'s `enableGlobe()`
    // disposes the OLD `GlobeLayer` before constructing a fresh one against the SAME
    // long-lived manager, under the SAME stable ids -- without this, a second
    // `enableGlobe()` call would throw on `LayerManager.addLayer()`'s own
    // already-registered guard, and the old, disposed instance's stale
    // `textureLoader` would stay registered forever, silently consuming shared
    // budget for content nothing renders any more.
    if (this.layerManager) {
      this.layerManager.removeLayer(this.imageryLayerId);
      this.layerManager.removeLayer(this.terrainLayerId);
    }
  }
}
