// Quadtree ellipsoid globe: tile addressing, WGS84 geometry, screen-space-error LOD
// selection, and a load scheduler with a memory budget and cancellation.
//
// Framework-free (no Three.js import) on purpose, same reasoning as origin.js: this is
// the one place the tile-selection arithmetic lives, so `web/js/globe_lod_check.mjs`
// (run headlessly under `node`, see tests/test_viewer_globe.py) and the browser-facing
// renderer (`web/js/globe.js`, which turns a selected tile into a THREE.BufferGeometry
// + texture) exercise the exact same selection logic, never a second copy of it.
// `web/js/globe.js` is the only file in this pair that imports Three.js.
//
// docs/architecture.md's Presentation-plane "Globe" bullet: "WGS84 ellipsoid quadtree
// with imagery and terrain tiles from our tile gateway, screen-space-error LOD,
// decoding in worker threads, budgeted loading." This module builds the quadtree
// addressing, the WGS84 geodetic<->ECEF geometry, the screen-space-error LOD selection
// and the budgeted/cancellable load scheduler; `globe.js` builds geometry/texture from
// what this module selects. Worker-thread decoding is not built (P1 scope is one tile
// gateway of small PNG tiles, not terrain meshes) -- see web/VIEWER.md's escalations.

/** @typedef {{level: number, x: number, y: number}} Tile */

// ------------------------------------------------------------------------ WGS84
export const WGS84_A_M = 6378137.0;           // equatorial (semi-major) radius, metres
export const WGS84_B_M = 6356752.314245;      // polar (semi-minor) radius, metres
export const WGS84_E2 = 1 - (WGS84_B_M * WGS84_B_M) / (WGS84_A_M * WGS84_A_M); // eccentricity^2
const DEG2RAD = Math.PI / 180;

// Scene units per metre: web/js/scene.js's SCALE is 1e-3 scene units per km, so
// 1e-3 / 1000 m-per-km = 1e-6 scene units per metre. Kept here (not re-derived in
// globe.js or scene_jitter_harness.mjs) so there is exactly one number every tile
// vertex, everywhere, is built against -- see `tileVertexPositions` below and
// globe.js's module docstring for why tile vertices are safe in plain float32 at this
// scale without any floating-origin subtraction.
export const SCENE_UNITS_PER_METRE = 1e-6;

/**
 * WGS84 geodetic (lon/lat degrees, height metres above the ellipsoid) -> ECEF metres.
 * Standard closed-form conversion (no iteration needed since height is exact, not
 * derived from a Cartesian point) -- e.g. Vermeille/NGA "ECEF to LLA" companion
 * formula, run in the forward direction. `out` (if given) is mutated and returned,
 * matching the `{x,y,z,set(...)}` scratch convention `interp.js`/`scene_jitter_
 * harness.mjs` already use, so this composes with the rest of the codebase's f64
 * scratch-object style without allocating on a hot path.
 * @returns {{x:number,y:number,z:number}}
 */
export function geodeticToEcef(lonDeg, latDeg, heightM = 0, out = { x: 0, y: 0, z: 0 }) {
  const lon = lonDeg * DEG2RAD, lat = latDeg * DEG2RAD;
  const sinLat = Math.sin(lat), cosLat = Math.cos(lat);
  const N = WGS84_A_M / Math.sqrt(1 - WGS84_E2 * sinLat * sinLat);
  out.x = (N + heightM) * cosLat * Math.cos(lon);
  out.y = (N + heightM) * cosLat * Math.sin(lon);
  out.z = (N * (1 - WGS84_E2) + heightM) * sinLat;
  return out;
}

const RAD2DEG = 180 / Math.PI;
const WGS84_EP2 = (WGS84_A_M * WGS84_A_M - WGS84_B_M * WGS84_B_M) / (WGS84_B_M * WGS84_B_M); // second eccentricity^2

/**
 * WGS84 ECEF metres -> geodetic (lon/lat degrees, height metres above the ellipsoid) --
 * the inverse of `geodeticToEcef` above, needed by `web/js/ground_track.js` (M26.4's 2D
 * companion map: a ground track is exactly "spacecraft position in the body-fixed frame,
 * converted to lon/lat"). Bowring's 1976 closed-form approximation (one reduced-latitude
 * correction step, not an iterative solver) -- standard, non-iterative, and accurate to
 * sub-millimetre at LEO/GEO altitudes for WGS84's actual flattening (~1/298.257), which
 * this project's own tolerance classes never need to distinguish from an iterative
 * solution. `lon = atan2(y, x)` needs no ellipsoid correction (the ellipsoid is a body of
 * revolution about the Z axis, so longitude is exactly the same as for a sphere) -- only
 * latitude does, which is what Bowring's step corrects for.
 * On the Z axis (x=y=0, the pole itself) longitude is conventionally 0 -- `Math.atan2(0,
 * 0)` already returns 0, so no special case is needed.
 * @returns {{lonDeg:number, latDeg:number, heightM:number}}
 */
export function ecefToGeodeticDeg(xM, yM, zM) {
  const p = Math.hypot(xM, yM);
  const lon = Math.atan2(yM, xM);
  if (p < 1e-9) {
    // On the polar axis: longitude is conventionally 0 (atan2(0,0) already gives that);
    // latitude is exactly +-90 by the symmetry of an ellipsoid of revolution (this is
    // the one geometric fact this function's own test can check without invoking the
    // Bowring formula at all -- see web/js/ground_track_check.mjs).
    return { lonDeg: 0, latDeg: zM >= 0 ? 90 : -90, heightM: Math.abs(zM) - WGS84_B_M };
  }
  // Bowring's reduced-latitude (parametric latitude) initial angle, then one correction.
  const theta = Math.atan2(zM * WGS84_A_M, p * WGS84_B_M);
  const sinTheta = Math.sin(theta), cosTheta = Math.cos(theta);
  const lat = Math.atan2(
    zM + WGS84_EP2 * WGS84_B_M * sinTheta * sinTheta * sinTheta,
    p - WGS84_E2 * WGS84_A_M * cosTheta * cosTheta * cosTheta,
  );
  const sinLat = Math.sin(lat), cosLat = Math.cos(lat);
  const N = WGS84_A_M / Math.sqrt(1 - WGS84_E2 * sinLat * sinLat);
  const heightM = p / cosLat - N;
  return { lonDeg: lon * RAD2DEG, latDeg: lat * RAD2DEG, heightM };
}

// Exported (M16.4) so web/js/tiles_layer.js's 3D-Tiles-overlay tile selection can
// reuse this exact ECEF-distance primitive rather than a second copy of it -- both
// consumers measure camera-to-tile-bounding-sphere distance the same way.
export function dist(a, b) {
  const dx = a.x - b.x, dy = a.y - b.y, dz = a.z - b.z;
  return Math.sqrt(dx * dx + dy * dy + dz * dz);
}

// ------------------------------------------------------------------- tile addressing
// Geographic (EPSG:4326-style, "plate carree") tiling scheme, not Web Mercator: level 0
// is 2 tiles side by side (a whole 360x180 degree globe, no polar singularity to work
// around), and each level doubles both axes. This is the tiling scheme our tile
// gateway serves (docs/architecture.md's "self-hosted public data" imagery, question
// 45) -- URLs are still requested with an XYZ-shaped {z}/{x}/{y} template
// (`globe.js`'s `imageryUrl`), just addressed against this grid rather than Mercator's.
export function tileCountX(level) { return 2 ** (level + 1); }
export function tileCountY(level) { return 2 ** level; }

/** Tile {level,x,y} -> geodetic bounds in degrees: {west, south, east, north}. */
export function tileBoundsDeg(tile) {
  const nx = tileCountX(tile.level), ny = tileCountY(tile.level);
  const dLon = 360 / nx, dLat = 180 / ny;
  const west = -180 + tile.x * dLon;
  const south = -90 + tile.y * dLat;
  return { west, south, east: west + dLon, north: south + dLat };
}

/** The 4 children of `tile`, in a fixed order (NW, NE, SW, SE by y-then-x) -- this
 * fixed order is what makes `selectTiles`'s traversal (and hence its output) not
 * depend on Map/Set insertion accidents (binding rule in this task's brief). */
export function tileChildren(tile) {
  const cl = tile.level + 1, x0 = tile.x * 2, y0 = tile.y * 2;
  return [
    { level: cl, x: x0, y: y0 }, { level: cl, x: x0 + 1, y: y0 },
    { level: cl, x: x0, y: y0 + 1 }, { level: cl, x: x0 + 1, y: y0 + 1 },
  ];
}

export function tileKey(tile) { return `${tile.level}/${tile.x}/${tile.y}`; }

/** Canonical order for a tile list -- (level, x, y) ascending. `selectTiles` always
 * returns its result sorted by this before returning, specifically so two independent
 * `node` process runs over the same camera path produce byte-identical output (the
 * "deterministic... same tile set, same order" requirement) regardless of what
 * traversal order internally discovered them in. */
export function compareTiles(a, b) {
  return a.level - b.level || a.x - b.x || a.y - b.y;
}

/** Tile centre point in ECEF metres (surface, height 0) and an approximate bounding
 * radius (max corner-to-centre distance) -- cheap linear approximations adequate for
 * an LOD heuristic, not a terrain-accurate footprint. */
export function tileBoundingSphere(tile) {
  const b = tileBoundsDeg(tile);
  const midLon = (b.west + b.east) / 2, midLat = (b.south + b.north) / 2;
  const center = geodeticToEcef(midLon, midLat);
  let r = 0;
  for (const [lon, lat] of [[b.west, b.south], [b.east, b.south], [b.west, b.north], [b.east, b.north]]) {
    r = Math.max(r, dist(center, geodeticToEcef(lon, lat)));
  }
  return { center, radius: r };
}

/**
 * Body-local (WGS84-ellipsoid-relative), origin-independent scene-unit positions for a
 * `(segments+1) x (segments+1)` grid spanning `tile`'s geodetic bounds -- flat
 * `[x0,y0,z0,x1,y1,z1,...]`, row-major (`j` outer/latitude, `i` inner/longitude), f64.
 * This is the exact vertex-position math `web/js/globe.js`'s `buildTileMesh()` uses to
 * fill its `THREE.BufferGeometry` (never a second copy of it -- same "one
 * implementation" principle as `scene.js`'s `trajectoryRenderPositions`), factored
 * here so it stays framework-free and callable from `web/js/scene_jitter_harness.mjs`
 * (see `measureRpoWithGlobePresent` there) without importing Three.js.
 * @param {Tile} tile
 * @param {number} segments quads per tile edge
 * @returns {Float64Array} length `3 * (segments+1)^2`
 */
export function tileVertexPositions(tile, segments) {
  const b = tileBoundsDeg(tile);
  const n = segments + 1;
  const out = new Float64Array(n * n * 3);
  const ecef = { x: 0, y: 0, z: 0 };
  let vi = 0;
  for (let j = 0; j < n; j++) {
    const lat = b.south + (b.north - b.south) * (j / segments);
    for (let i = 0; i < n; i++) {
      const lon = b.west + (b.east - b.west) * (i / segments);
      geodeticToEcef(lon, lat, 0, ecef);
      out[vi] = ecef.x * SCENE_UNITS_PER_METRE;
      out[vi + 1] = ecef.y * SCENE_UNITS_PER_METRE;
      out[vi + 2] = ecef.z * SCENE_UNITS_PER_METRE;
      vi += 3;
    }
  }
  return out;
}

// --------------------------------------------------------------- screen-space error
const TEXELS_PER_TILE = 256;               // assumed imagery tile resolution
const EQUATOR_CIRCUMFERENCE_M = 2 * Math.PI * WGS84_A_M;

/** Geometric error (metres of ground resolution one texel of this tile's imagery
 * represents) at `level` -- halves each level, same idea 3D Tiles' own
 * `geometricError` field encodes, just derived from the tiling scheme instead of
 * read from a tileset.json (this quadtree has no such file; it is generated, not
 * authored). */
export function geometricErrorAtLevel(level) {
  return (EQUATOR_CIRCUMFERENCE_M / tileCountX(level)) / TEXELS_PER_TILE;
}

/** The standard "geometric error projected to screen" formula (the same one
 * Cesium/3D Tiles use): `sse = geometricError * screenHeight / (2 * distance *
 * tan(fovY/2))`. Larger sse == this content looks coarser on screen than acceptable
 * == refine into children. Factored out (M16.4) from `screenSpaceErrorPx` below so
 * web/js/tiles_layer.js's 3D-Tiles-overlay selection -- which reads a real, authored
 * `geometricError` straight from tileset.json instead of deriving one from a
 * synthetic quadtree level -- uses the exact same arithmetic, not a second copy of
 * it, for the one number this module's Screen-space-error section is about.
 */
export function sseFromGeometricError(geometricError, distanceM, screenHeightPx, fovYRad) {
  return (geometricError * screenHeightPx) / (2 * distanceM * Math.tan(fovYRad / 2));
}

/** Projected screen-space error, in pixels, of rendering `tile` from `cameraEcef` with
 * a `screenHeightPx`-tall viewport and vertical field of view `fovYRad`. */
export function screenSpaceErrorPx(tile, cameraEcef, screenHeightPx, fovYRad) {
  const { center, radius } = tileBoundingSphere(tile);
  const d = Math.max(dist(cameraEcef, center) - radius, radius * 0.01, 1);
  return sseFromGeometricError(geometricErrorAtLevel(tile.level), d, screenHeightPx, fovYRad);
}

// ------------------------------------------------------------------- tile selection
const ROOT_TILES = [{ level: 0, x: 0, y: 0 }, { level: 0, x: 1, y: 0 }];

/**
 * Select the set of tiles to render for `cameraEcef`, by camera distance and
 * screen-space error -- docs/architecture.md's "Globe" bullet, verbatim. Traversal is
 * breadth-first from the two level-0 root tiles, in a fixed order (`tileChildren`'s
 * fixed NW/NE/SW/SE order, a plain array-backed queue, never a `Map`/`Set` whose
 * iteration order could depend on insertion history) -- refine into a tile's children
 * when its projected screen-space error exceeds `sseThreshold` pixels, `tile.level <
 * maxLevel`, and refining would not push the running (selected + still-queued) tile
 * count past `maxTiles`; otherwise the tile itself is selected (a leaf of this
 * traversal, whatever its level). The budget check is a deterministic function of the
 * current counts only (no randomness, no wall-clock, no object hashing), so it does not
 * introduce nondeterminism of its own.
 *
 * The returned array is always sorted by `compareTiles` before it is returned --
 * *this* is what actually guarantees "same tile set, same order" across two
 * independent `node` process invocations over the same camera path, not merely that
 * the traversal above happens to be deterministic (see `tileKey`'s docstring and this
 * task's binding rule: "Determinism on the tile-selection path: sorted iteration, no
 * dependence on Map/Set insertion accidents").
 *
 * @param {{x:number,y:number,z:number}} cameraEcef
 * @param {{screenHeightPx?: number, fovYRad?: number, sseThreshold?: number, maxLevel?: number, maxTiles?: number}} [opts]
 * @returns {Tile[]}
 */
export function selectTiles(cameraEcef, opts = {}) {
  const {
    screenHeightPx = 900,
    fovYRad = (50 * Math.PI) / 180,
    sseThreshold = 16,
    maxLevel = 12,
    maxTiles = 512,
  } = opts;
  const selected = [];
  const queue = [...ROOT_TILES];
  while (queue.length) {
    const tile = queue.shift();
    const sse = screenSpaceErrorPx(tile, cameraEcef, screenHeightPx, fovYRad);
    const canRefine = tile.level < maxLevel && (selected.length + queue.length + 4) <= maxTiles;
    if (sse > sseThreshold && canRefine) queue.push(...tileChildren(tile));
    else selected.push(tile);
  }
  selected.sort(compareTiles);
  return selected;
}

// ------------------------------------------------------------------ load scheduler
/**
 * Budgeted, cancellable tile-load scheduler. Framework-free (uses the platform
 * `AbortController`, available in both `node` and every target browser -- question 48 --
 * not a custom cancellation token type). Owns three pieces of state, keyed by
 * `tileKey()`:
 *   - `resident`: tiles whose imagery has finished loading and is considered "in the
 *     memory budget" (an LRU cache, evicted in `_evictIfNeeded`).
 *   - `pending`: tiles a load has been *started* for but not yet completed, each with
 *     its own `AbortController` -- calling `update()` with a selection that no longer
 *     names a pending tile aborts it (`cancelledCount` counts this).
 *   - `residentBudget`: the memory budget (in tile count, a proxy for byte budget --
 *     `globe.js`'s imagery textures are a fixed size, so tile count times a per-tile
 *     byte estimate is exactly the byte budget; kept as a tile count here so this
 *     module never has to know a texture's actual byte size).
 */
export class TileLoadScheduler {
  constructor({ residentBudget = 64 } = {}) {
    this.residentBudget = residentBudget;
    /** @type {Map<string, {lastUsedStep: number}>} */
    this.resident = new Map();
    /** @type {Map<string, {controller: AbortController, startedStep: number}>} */
    this.pending = new Map();
    this.cancelledCount = 0;
    this.evictedCount = 0;
    this._step = 0;
  }

  /**
   * Apply one selection (the output of `selectTiles`, this "frame"'s desired tile
   * set): cancel any pending load for a tile no longer selected, and start a load
   * (register a pending entry with a fresh `AbortController`) for any selected tile
   * that is neither resident nor already pending. Does not itself complete any load --
   * see `completeLoads` -- so a tile whose load takes several `update()` calls to
   * finish stays genuinely cancellable across camera moves, the same as a real
   * network/decode-bound load would.
   *
   * `keyFn` (M16.4, default `tileKey`) is what makes this class reusable verbatim
   * for web/js/tiles_layer.js's 3D-Tiles overlay (this task's brief: "the globe's
   * streaming budget and cancellation reused for the overlay ... the same
   * TileLoadScheduler machinery"), whose tiles are addressed by a tree-path id
   * string, not this module's `{level,x,y}` shape -- every other method below
   * (`completeLoads`/`_evictIfNeeded`) already only ever handles plain string keys
   * (from `selectedKeys`/`protectedKeys`), so no other change was needed to
   * generalize this class. Every existing call site (globe.js, globe_lod_check.mjs)
   * omits the second argument, so its behaviour is byte-for-byte unchanged.
   * @param {Tile[]|Array<{id: string}>} selectedTiles
   * @param {(tile: any) => string} [keyFn]
   */
  update(selectedTiles, keyFn = tileKey) {
    this._step += 1;
    const selectedKeys = new Set(selectedTiles.map(keyFn));
    for (const [k, p] of this.pending) {
      if (!selectedKeys.has(k)) {
        p.controller.abort();
        this.pending.delete(k);
        this.cancelledCount += 1;
      }
    }
    for (const k of selectedKeys) {
      if (this.resident.has(k)) { this.resident.get(k).lastUsedStep = this._step; continue; }
      if (this.pending.has(k)) continue;
      this.pending.set(k, { controller: new AbortController(), startedStep: this._step });
    }
    return selectedKeys;
  }

  /**
   * Simulate up to `n` of the oldest pending loads finishing this "frame" (oldest
   * `startedStep` first, tied-broken by `tileKey` for determinism): move each to
   * `resident`, then evict least-recently-used resident tiles (never one in
   * `protectedKeys`, i.e. this step's own selection) until `resident.size <=
   * residentBudget` or nothing left is safe to evict.
   * @param {number} n
   * @param {Set<string>} protectedKeys tiles currently selected -- never evicted
   */
  completeLoads(n, protectedKeys) {
    const items = [...this.pending.entries()]
      .sort((a, b) => a[1].startedStep - b[1].startedStep || (a[0] < b[0] ? -1 : 1));
    for (let i = 0; i < n && i < items.length; i++) {
      const [k] = items[i];
      this.pending.delete(k);
      this.resident.set(k, { lastUsedStep: this._step });
    }
    this._evictIfNeeded(protectedKeys);
  }

  _evictIfNeeded(protectedKeys) {
    while (this.resident.size > this.residentBudget) {
      let victim = null, victimLru = Infinity;
      for (const [k, v] of this.resident) {
        if (protectedKeys && protectedKeys.has(k)) continue;
        if (v.lastUsedStep < victimLru) { victimLru = v.lastUsedStep; victim = k; }
      }
      if (victim === null) break; // everything resident is protected; budget soft-violated this step
      this.resident.delete(victim);
      this.evictedCount += 1;
    }
  }
}
