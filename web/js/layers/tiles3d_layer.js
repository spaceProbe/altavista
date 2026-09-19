// Tiles3DLayerAdapter -- wraps the vendored 3DTilesRendererJS overlay behind the
// Layer interface (./layer.js). It does not reimplement 3D Tiles tile-tree
// screen-space-error selection: it imports `selectTiles3D`/`regionBoundingSphereEcef`
// from `web/js/tiles_layer.js`, the one place that arithmetic lives (itself already
// built on `web/js/globe_lod.js`'s `dist`/`sseFromGeometricError`, per that file's own
// module docstring) -- question 218's "no second copy of a budget/eviction rule"
// reasoning applied one level up, to selection, not just eviction.
//
// `web/js/tiles_layer.js`'s `TilesOverlayLayer` already drives the real vendored
// `TilesRenderer`'s own fetch/parse/LOD pipeline directly (see its module docstring:
// that pipeline cannot run headlessly under `node` -- no `window`/
// `requestAnimationFrame` -- and is not reimplemented, Q44's ratified decision). This
// adapter does not duplicate that live wiring; it is the `web/js/layers/`-side
// counterpart -- a `Layer` whose `load()` delegates to an *injected* per-tile loader
// function (the same "loader as an injected stub" shape every adapter in this module
// uses, design constraint f), so `LayerManager`'s priority queue/byte budget/
// cancellation can be exercised headlessly (`web/js/layers_check.mjs`) against this
// project's own 3D Tiles fixture (`web/fixtures/3dtiles/tileset.json`) exactly as
// `web/js/tiles3d_check.mjs` already does for `TileLoadScheduler` alone. Wiring a real
// `TilesRenderer`'s per-tile content fetch to this adapter's `load()` (so
// `TilesOverlayLayer` itself is driven through `web/js/layers/` end to end, live, in
// a browser) is disclosed as not done in this task in `web/VIEWER.md` -- see that
// file's new section for exactly what is and is not covered.
import { selectTiles3D, regionBoundingSphereEcef } from '../tiles_layer.js';
import { dist, sseFromGeometricError } from '../globe_lod.js';
import { IMAGERY_TILE_BYTES } from './imagery_layer.js';

/** A glTF-bearing 3D tile's estimated resident byte cost, used when a tile's own
 * `tileset.json` entry carries no `content.byteLength` (which this project's fixture
 * generator, `web/fixtures/gen_3dtiles_fixture.py`, does not emit -- the 3D Tiles
 * spec does not require it, and `web/js/tiles_layer.js`'s `parseTileset3D` does not
 * currently read one). A declared, documented estimate (design constraint b), not a
 * measurement -- deliberately an order of magnitude above `imagery_layer.js`'s
 * `IMAGERY_TILE_BYTES` (derived from it directly, so the relationship stays true if
 * that constant ever changes), matching this design constraint's own framing of "a
 * glTF-bearing 3D tile" as the heaviest of the three per-item costs this module's
 * byte budget has to reconcile.
 */
export const DEFAULT_TILE3D_BYTES = IMAGERY_TILE_BYTES * 10;

export class Tiles3DLayerAdapter {
  /**
   * @param {{id?: string, tree: {nodes: Map, rootId: string}, loader: Function, defaultByteCost?: number}} opts
   *   `tree` is `web/js/tiles_layer.js`'s `parseTileset3D(tilesetJson)` output.
   *   `loader(request, signal): Promise<any>` is the injected per-tile content
   *   loader (see this file's module docstring for why this, not a direct
   *   `TilesRenderer` wire-up, is this task's scope).
   */
  constructor({ id = 'tiles3d', tree, loader, defaultByteCost = DEFAULT_TILE3D_BYTES } = {}) {
    this.id = id;
    this._tree = tree;
    this._loader = loader;
    this._defaultByteCost = defaultByteCost;
  }

  /**
   * `view.cameraEcef`/`screenHeightPx`/`fovYRad` are shared with the other adapters
   * (see `ImageryLayerAdapter.plan`); `view.sseThreshold`/`maxLevel`/`maxTiles` are
   * this layer's own `selectTiles3D` traversal knobs, passed through unchanged
   * (`undefined` falls back to `selectTiles3D`'s own defaults, exactly as calling it
   * directly would).
   */
  plan(view) {
    const {
      cameraEcef, screenHeightPx, fovYRad, sseThreshold, maxLevel, maxTiles,
    } = view;
    const ids = selectTiles3D(this._tree, cameraEcef, {
      screenHeightPx, fovYRad, sseThreshold, maxLevel, maxTiles,
    });
    return ids.map((id) => {
      const node = this._tree.nodes.get(id);
      const { center, radius } = regionBoundingSphereEcef(node.region);
      const d = Math.max(dist(cameraEcef, center) - radius, radius * 0.01, 1);
      return {
        key: id,
        sseError: sseFromGeometricError(node.geometricError, d, screenHeightPx, fovYRad),
        viewDistanceM: d,
        byteCost: node.byteLength || this._defaultByteCost,
        // level (round 4, question 228): this node's own tree DEPTH, not a byte-cost
        // or screen-space notion -- `web/js/tiles_layer.js`'s own node ids are
        // dot-joined path strings ("0", "0.1", "0.2.0", ...), and `id.split('.').
        // length - 1` is exactly `selectTiles3D`'s own `level` local (see that
        // function's own traversal in tiles_layer.js) recovered from the id alone, so
        // this is not a second, independently-derived notion of depth -- the root
        // ("0") is level 0, the coarsest, matching layer.js's own `level` convention.
        level: id.split('.').length - 1,
        contentUri: node.contentUri,
      };
    });
  }

  /** Delegates to the injected loader; respecting `signal` is the loader's own
   * responsibility (exactly like `ImageryLayerAdapter`'s injected `.load()`, and
   * documented the same way for `web/js/layers_check.mjs`'s stub). */
  load(request, signal) {
    return this._loader(request, signal);
  }

  /** The vendored `TilesRenderer`'s own `lruCache` owns real glTF disposal for
   * whatever it actually fetches (see this file's module docstring); this adapter's
   * `load()` never returns a payload tied to that cache in this task's headless
   * scope (a stub loader in the harness, see `web/js/layers_check.mjs`), so there is
   * nothing of its own to free here yet. Explicit no-op, not omitted, so the Layer
   * interface is satisfied on its own terms -- same reasoning as
   * `ImageryLayerAdapter.release`.
   */
  release(_key) {}
}
