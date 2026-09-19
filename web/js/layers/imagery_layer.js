// ImageryLayerAdapter -- wraps the globe's imagery loader behind the Layer interface
// (./layer.js). "The globe's imagery loader" means exactly the loader shape
// `web/js/globe.js`'s `GlobeLayer` already accepts as `opts.textureLoader`: any
// object with a `.load(url, onLoad, onProgress, onError)` method (the same shape
// `THREE.TextureLoader` exposes, and the same shape `web/js/globe_imagery_check.mjs`
// already injects a network-free stub for -- see that file's module docstring for why
// this shape is what makes "no network call" in a headless harness structural, not
// merely observed). This adapter does not reimplement URL templating: it imports
// `urlForTile` from `web/js/globe.js` (the exact function `GlobeLayer` itself uses to
// build the same URLs), and it does not reimplement screen-space-error/distance
// arithmetic: it imports `screenSpaceErrorPx`/`dist`/`tileBoundingSphere` from
// `web/js/globe_lod.js`, the one place that arithmetic lives.
import { urlForTile } from '../globe.js';
import { screenSpaceErrorPx, dist, tileBoundingSphere, tileKey } from '../globe_lod.js';

/** One 256x256 RGBA8 imagery tile's estimated resident byte cost -- 256 is
 * `globe_lod.js`'s own `TEXELS_PER_TILE` assumption (its screen-space-error
 * derivation already assumes this resolution), 4 bytes/texel for RGBA8. This is a
 * declared, documented estimate (design constraint b: "the manager holds
 * memoryBudgetBytes, every resident item's byte cost is accounted"), not a byte
 * count measured from a real decoded texture -- a real `THREE.Texture`'s GPU-resident
 * size depends on mipmaps/compression this codebase does not control per-tile, so an
 * honest fixed estimate (matching the one resolution this codebase's fixtures and
 * `globe_lod.js`'s own SSE model already assume) is what is accounted, disclosed
 * here rather than silently treated as exact.
 */
export const IMAGERY_TILE_BYTES = 256 * 256 * 4;

export class ImageryLayerAdapter {
  /**
   * @param {{id?: string, imageryUrl: string, loader: {load: Function}}} opts
   *   `loader` is the injected `.load(url, onLoad, onProgress, onError)`-shaped
   *   object (a real `THREE.TextureLoader` in production -- see `web/js/globe.js`'s
   *   `GlobeLayer` constructor for the identical injection point and reasoning --
   *   or `web/js/layers_check.mjs`'s network-free stub in this task's headless
   *   harness, per design constraint f).
   */
  /**
   * `tileBytes` (round 3, the ten-gigabyte proof) is the per-tile byte cost this
   * adapter declares to `LayerManager`. It defaults to [`IMAGERY_TILE_BYTES`], the
   * 256x256 RGBA estimate every existing caller and fixture assumes, so no existing
   * call site moves. It is a constructor option because a tile set is not obliged to
   * be 256x256: the tile sets `crates/av-jobs`' tiler produces carry their own
   * `tile_size` in their manifest, and a 1024-pixel RGB8 PNG tile is 3147060 bytes,
   * twelve times this default. A memory budget accounted in the wrong units is not a
   * memory budget, so a caller that knows its tile set's real size declares it --
   * and `web/js/layers_stream_check.mjs` goes further and checks the declared number
   * against the length of every tile it actually receives.
   */
  constructor({ id = 'imagery', imageryUrl, loader, tileBytes = IMAGERY_TILE_BYTES } = {}) {
    this.id = id;
    this.imageryUrl = imageryUrl;
    this._loader = loader;
    this.tileBytes = tileBytes;
  }

  /**
   * `view.tiles` is `web/js/globe_lod.js`'s `selectTiles(cameraEcef, ...)` output
   * (a caller-supplied selection, not recomputed here -- selection stays the
   * quadtree module's job, this adapter only turns an already-selected tile into a
   * `Request`). `view.cameraEcef`/`screenHeightPx`/`fovYRad` are the same inputs
   * `selectTiles()` itself was called with, reused here only to compute each
   * selected tile's screen-space error/distance for priority ordering.
   */
  plan(view) {
    const { tiles = [], cameraEcef, screenHeightPx, fovYRad } = view;
    return tiles.map((tile) => {
      const { center } = tileBoundingSphere(tile);
      return {
        key: tileKey(tile),
        sseError: screenSpaceErrorPx(tile, cameraEcef, screenHeightPx, fovYRad),
        viewDistanceM: dist(cameraEcef, center),
        byteCost: this.tileBytes,
        // level (round 4, question 228): the tile's own quadtree level -- 0 is the
        // whole-globe root tile (coarsest), ascending levels are finer, exactly
        // `LayerManager`'s own `level` convention (see layer.js's module docstring).
        // Used only for admission order (`compareAdmission`), never for selection or
        // priority (`comparePriority`) -- this adapter reuses `globe_lod.js`'s own
        // `tile.level`, never a second copy of that notion.
        level: tile.level,
        url: urlForTile(this.imageryUrl, tile),
        tile,
      };
    });
  }

  /** Promise-ifies the injected `.load(url, onLoad, onProgress, onError)` loader and
   * makes it genuinely cancellable: a real `THREE.TextureLoader`'s underlying
   * XHR/fetch has no public abort hook, so what `signal` cancels here is *this
   * module's* bookkeeping of the result -- exactly the "cancellation of in-flight
   * requests" this module (not the browser's network stack) is responsible for
   * owning (see ./layer.js's module docstring, "no caller outside web/js/layers/").
   * A request whose signal fires after `onLoad`/`onError` has already settled the
   * promise is a no-op (the abort listener is removed on settle), matching a real
   * `AbortSignal`'s "abort after already-settled is harmless" semantics.
   */
  load(request, signal) {
    return new Promise((resolve, reject) => {
      if (signal.aborted) { reject(signal.reason); return; }
      const onAbort = () => reject(signal.reason);
      signal.addEventListener('abort', onAbort, { once: true });
      this._loader.load(
        request.url,
        (payload) => { signal.removeEventListener('abort', onAbort); resolve(payload); },
        undefined,
        (err) => { signal.removeEventListener('abort', onAbort); reject(err); },
      );
    });
  }

  /** A real GPU texture's disposal happens where the mesh/material that consumes it
   * lives (`web/js/globe.js`'s `GlobeLayer.dispose()`/per-mesh cleanup already does
   * this for its own resident set) -- there is nothing this adapter itself owns to
   * free for an opaque loaded payload it never inspects (see ./layer.js's module
   * docstring: `LayerManager` treats `payload` as opaque). Kept as an explicit no-op
   * (not omitted) so the Layer interface is satisfied on its own terms; a caller
   * that wants real disposal wires its own loader/payload type to do that inside
   * `onLoad`/here, which is exactly the seam `loader` injection provides.
   */
  release(_key) {}
}
