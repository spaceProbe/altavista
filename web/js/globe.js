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
function buildTileMesh(tile, imageryUrl, textureLoader, segments = DEFAULT_SEGMENTS) {
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
  textureLoader.load(
    urlForTile(imageryUrl, tile),
    (tex) => {
      tex.colorSpace = THREE.SRGBColorSpace;
      material.map = tex;
      material.color.set(0xffffff);
      material.needsUpdate = true;
      mesh.userData.imageryLoaded = true;
    },
    undefined,
    () => { /* graceful fallback: keep the flat colour material, never throw */ },
  );
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
  }

  /**
   * Recompute the tile selection for `cameraLocalPos` (scene units, body-local -- see
   * class docstring) and reconcile `group`'s children to match: build+add a mesh for
   * every newly selected tile, dispose+remove one for every tile that fell out of
   * selection. `screenHeightPx`/`fovYRad` are passed straight through to
   * `selectTiles()` (web/js/globe_lod.js) -- the real screen-space-error LOD
   * computation, not reimplemented here.
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
    const selectedKeys = this.scheduler.update(tiles);
    for (const tile of tiles) {
      const k = tileKey(tile);
      if (!this._meshes.has(k)) {
        const mesh = buildTileMesh(tile, this.imageryUrl, this.textureLoader, this.segments);
        this._meshes.set(k, mesh);
        this.group.add(mesh);
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
    // Keep the scheduler's resident bookkeeping in step with what is actually built
    // (unlike globe_lod_check.mjs's offline harness, the browser render path doesn't
    // need to simulate a slow multi-frame load queue -- a mesh with a fallback-colour
    // material renders immediately, and the texture swaps in asynchronously).
    this.scheduler.completeLoads(this._meshes.size, selectedKeys);
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
  }
}
