// TerrainLayerAdapter -- the globe's terrain loader, wrapped behind the Layer
// interface (./layer.js). There is no terrain loader in this codebase yet
// (web/VIEWER.md's "Explicitly not built": "Globe terrain: imagery only -- no
// terrain/elevation tiles"), and docs/heavy-plan.md's H5 brief is explicit that this
// is allowed: "The terrain adapter may be a thin one whose loader is not yet
// implemented -- if so it must be a typed, named refusal with a test pinning it,
// never a silent stub (this workspace's rule: an exit code is not evidence, and a gap
// is recorded, never hidden)."
//
// So: `plan()` is real -- it declares exactly what terrain content the current view
// would want, using the same screen-space-error/distance shape every other adapter
// uses (via `web/js/globe_lod.js`'s own `screenSpaceErrorPx`/`dist`/
// `tileBoundingSphere`, never a second copy of that arithmetic), so this layer
// genuinely participates in `LayerManager`'s one priority queue and its requests are
// genuinely visible in the harness's ordered output -- honest demand accounting, even
// though nothing can be loaded yet. `load()`, in contrast, always rejects with
// `TerrainLoaderNotImplementedError`, a named, typed refusal (not a generic `Error`,
// not a silently-resolved fake payload) -- `tests/test_viewer_layers.py` pins both
// that this rejects (never resolves) and the exact error name, and
// `web/js/layers_check.mjs` counts every refusal it observes
// (`terrainRefusalCount`) so it is reported, not swallowed.
import { screenSpaceErrorPx, dist, tileBoundingSphere, tileKey } from '../globe_lod.js';

/** A terrain mesh's estimated resident byte cost: a (segments+1)^2 vertex grid with
 * position + normal (3 f32 each), no separate UV/texture (terrain meshes are shaded
 * by their own vertex normals here, not textured -- consistent with there being no
 * terrain imagery in this codebase, see this file's module docstring). Documented
 * assumption, same spirit as `imagery_layer.js`'s `IMAGERY_TILE_BYTES`: an honest,
 * disclosed estimate, not a measurement from a real terrain payload (there is none to
 * measure). `DEFAULT_SEGMENTS` mirrors `web/js/globe.js`'s own tile mesh segment
 * count so the estimate is at least consistent with this codebase's one existing
 * mesh-building convention.
 */
const DEFAULT_SEGMENTS = 8;
const FLOATS_PER_VERTEX = 6; // position(3) + normal(3), f32
const BYTES_PER_FLOAT32 = 4;
export const TERRAIN_MESH_BYTES = (DEFAULT_SEGMENTS + 1) ** 2 * FLOATS_PER_VERTEX * BYTES_PER_FLOAT32;

/** Named, typed refusal (design constraint a, binding): thrown/rejected, never a
 * silently-resolved fake payload. `.name` is what `tests/test_viewer_layers.py` pins
 * -- a plain `instanceof Error` check alone would also pass for any other error type,
 * which is exactly the silent-stub failure mode this class exists to make
 * impossible to fake by accident. */
export class TerrainLoaderNotImplementedError extends Error {
  constructor(key) {
    super(
      `TerrainLayerAdapter: no terrain loader is implemented yet (requested key '${key}') `
      + '-- see web/VIEWER.md\'s "Explicitly not built" and web/js/layers/terrain_layer.js\'s '
      + 'module docstring: this is a disclosed gap, not a silent stub.',
    );
    this.name = 'TerrainLoaderNotImplementedError';
    this.key = key;
  }
}

export class TerrainLayerAdapter {
  constructor({ id = 'terrain' } = {}) {
    this.id = id;
    // Task 5b (panel-failure-attribution, round 7): this layer declares, about
    // ITSELF, that it has no loader -- the same "a layer states a fact about itself,
    // the manager only reads it" pattern `./imagery_layer.js`'s `this.kind =
    // 'imagery'` already establishes (see `LayerManager.imageryLayers()`). It lives
    // here, on the adapter, rather than as a name/error-string check in the Layers
    // panel, for the same reason `kind` does: the panel would otherwise have to know
    // -- and keep re-guessing correctly -- which layer ids or error names currently
    // mean "no loader", which breaks the instant a second no-loader layer exists or
    // this one's id changes; the adapter that actually made the decision to always
    // reject (`load()`, below) is the one honest place to declare it. A future real
    // terrain loader drops this flag (and updates `load()` to actually load), and
    // every caller reading it via `LayerManager.noLoaderLayers()` -- today only the
    // Layers panel's "not a fault of the selected set" attribution -- needs no change
    // at all.
    this.notImplemented = true;
  }

  /**
   * Declares the terrain this layer *would* request for `view.tiles` (the same
   * `{level,x,y}` tiles `web/js/globe_lod.js`'s `selectTiles()` returns, reused
   * as-is by `view` -- `LayerManager` passes one shared `view` object to every
   * layer's `plan()`, and each adapter reads only what it needs off it). Every
   * request this returns will, if `LayerManager` ever tries to load it, reject with
   * `TerrainLoaderNotImplementedError` -- see this file's module docstring.
   */
  plan(view) {
    const { tiles = [], cameraEcef, screenHeightPx, fovYRad } = view;
    return tiles.map((tile) => {
      const { center } = tileBoundingSphere(tile);
      return {
        key: tileKey(tile),
        sseError: screenSpaceErrorPx(tile, cameraEcef, screenHeightPx, fovYRad),
        viewDistanceM: dist(cameraEcef, center),
        byteCost: TERRAIN_MESH_BYTES,
        // level (round 4, question 228): same quadtree level as imagery_layer.js's own
        // `tile.level` -- see layer.js's module docstring and that file's identical
        // comment for why this is populated, not a second copy of the notion.
        level: tile.level,
        tile,
      };
    });
  }

  /** Always rejects -- see this file's module docstring and
   * `TerrainLoaderNotImplementedError`. Never resolves, under any input. */
  load(request, _signal) {
    return Promise.reject(new TerrainLoaderNotImplementedError(request.key));
  }

  /** Nothing is ever resident for this layer (`load()` never resolves), so
   * `release()` is never called by `LayerManager` in practice; kept as a real no-op
   * implementation (not omitted) so this class still satisfies the full Layer
   * interface on its own terms. */
  release(_key) {}
}
