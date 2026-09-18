// GatewayImageryLayerAdapter -- H5b-2 deliverable 2: an imagery Layer (./layer.js)
// backed by a REAL `av-tiles` gateway, proxied same-origin through this viewer
// server's own `/api/tiles/*` routes (`altavista/server.py`, `altavista/
// tiles_client.py`), rather than `ImageryLayerAdapter`'s injected
// `THREE.TextureLoader`-shaped stub loader.
//
// `plan(view)` does NOT reimplement tile selection or screen-space-error
// arithmetic a second time: it extends `ImageryLayerAdapter` and calls its real
// `plan()` (which itself only ever reuses `web/js/globe_lod.js`'s own `selectTiles`/
// `screenSpaceErrorPx`/`dist`/`tileBoundingSphere`/`tileKey` -- see that file's own
// module docstring) and replaces the `url` field with this gateway's own tile
// address. Every `sseError`/`viewDistanceM`/`key` value a caller sees from this
// adapter is `ImageryLayerAdapter`'s own, byte for byte; this file adds nothing to
// that arithmetic at all.
//
// `byteCost` is the one field this adapter DOES override, and only when it can do so
// honestly (round 4, question 228's own round-3-defect-5 follow-up): round 3 already
// replaced a fixed 262,144-byte-whatever-the-tile-set estimate with a caller-DECLARED
// `tileBytes` (still one uniform number per tile set, only checked post-hoc against
// what the gateway actually returns -- see `web/js/layers_stream_check.mjs`'s own
// `byteCostMismatchCount`). This closes that remaining gap: once `fetchManifest()`
// (below) has been awaited, `plan()` charges each tile's REAL, per-tile
// `TileEntry.size_bytes` from the tile set's own manifest (`./tileset_manifest.js`'s
// hand-rolled protobuf decoder -- ADR-004's "no bundled dependency" crypto rule,
// extended the same way to protobuf: see that file's own module docstring) --
// `TileHttpError`'s `502`-shaped intent has always applied to manifest fetch failures
// too, TileEtagMismatchError to the individual tile bytes -- **never** to a fixed
// estimate again. A request's manifest-sourced byte cost is tagged
// `byteCostSource: 'manifest'`; BEFORE `fetchManifest()` resolves (or if the manifest
// simply does not list a requested tile -- an inconsistency between the manifest and
// what `selectTiles()` asked for, which should never happen against a real tile set
// but is not this adapter's job to assume away), `plan()` falls back to the
// constructor's own declared `tileBytes` estimate, tagged `byteCostSource:
// 'fallback-estimate'` -- so a caller/harness can always tell which unit a given
// request's `byteCost` is actually in, and a run can never silently be accounted in
// estimated units without that being visible on every single request it happened for.
//
// Question 51 (the viewer never fetches any other origin, structurally, not merely
// "in practice"): `origin` is an injected string, defaulting to `''` -- in the
// browser, a same-origin *relative* URL (`fetch('/api/tiles/...')`) always resolves
// against whatever origin served the page, because that is what the Fetch spec says
// a relative URL resolves against; there is no code path in this file that reads
// `location.origin`, `document.baseURI`, or any other absolute-host source, and no
// string concatenation here can ever produce an absolute `http(s)://` URL unless a
// caller explicitly passes one as `origin` (this headless harness's own
// `web/js/layers_stream_check.mjs` does exactly that, since `node`'s own `fetch` has
// no implicit page origin to resolve a relative URL against at all).
//
// The gateway proxy route (`altavista/server.py`'s `tiles_tile`) sets `ETag` to the
// tile's own SHA-256 hex digest (`crates/av-tiles`' own route -- see
// `tests/test_viewer_tiles_route.py::test_a_tiles_bytes_come_back_and_their_sha256_
// equals_the_gateways_own_etag` for the byte-for-byte proof against the real
// gateway). This adapter does not trust that header blindly: `load()` recomputes the
// SHA-256 of the bytes it actually received, with the platform `crypto.subtle.
// digest('SHA-256', ...)` (ADR-004's crypto rule: system/platform crypto only, no
// bundled dependency, and SHA-256 is the only algorithm this codebase's crypto rule
// permits) -- never a bundled hashing library -- and rejects with a typed, named
// `TileEtagMismatchError` on any disagreement, including a missing `ETag` entirely
// (a missing header can never equal a real hex digest, so it is a mismatch, not a
// separate silently-accepted case).
import { ImageryLayerAdapter } from './imagery_layer.js';
import { decodeTileSetManifest, manifestTileKey } from './tileset_manifest.js';

/** Thrown by `load()` when the gateway answers with a non-2xx status. Typed and
 * named (this codebase's binding rule for every refusal: never a bare `Error`, see
 * `web/js/layers/terrain_layer.js`'s `TerrainLoaderNotImplementedError` for the
 * identical discipline applied to a different kind of failure). */
export class TileHttpError extends Error {
  constructor(url, status) {
    super(`GatewayImageryLayerAdapter: GET ${url} answered HTTP ${status}`);
    this.name = 'TileHttpError';
    this.url = url;
    this.status = status;
  }
}

/** Thrown by `load()` when the SHA-256 this adapter recomputes over the response
 * bytes it actually received does not equal the gateway's own `ETag` (or the
 * `ETag` header is absent altogether -- see this file's module docstring). Typed
 * and named for the same reason as `TileHttpError`. */
export class TileEtagMismatchError extends Error {
  constructor(url, expectedEtag, actualSha256) {
    super(
      `GatewayImageryLayerAdapter: GET ${url} -- recomputed SHA-256 ${actualSha256} `
      + `does not equal the gateway's own ETag ${JSON.stringify(expectedEtag)}`,
    );
    this.name = 'TileEtagMismatchError';
    this.url = url;
    this.expectedEtag = expectedEtag;
    this.actualSha256 = actualSha256;
  }
}

/** `ArrayBuffer` -> lowercase hex string, matching `hashlib.sha256(...).hexdigest()`
 * on the Python side (`tests/test_viewer_tiles_route.py` compares against exactly
 * this form) and `openssl dgst -sha256`'s own default output shape. No dependency:
 * this is the one and only place this file turns digest bytes into text. */
function hex(buffer) {
  const bytes = new Uint8Array(buffer);
  let out = '';
  for (let i = 0; i < bytes.length; i += 1) {
    out += bytes[i].toString(16).padStart(2, '0');
  }
  return out;
}

/** Strips the surrounding double quotes an HTTP `ETag` header conventionally
 * carries (`"<sha256>"`, a "strong" entity tag) -- mirrors `tests/
 * test_viewer_tiles_route.py`'s own `.strip('"')` on the same header, so this
 * adapter's comparison and that test's comparison agree on the same real gateway
 * response shape. A `null` header (absent entirely) passes through as `null`. */
function unquoteEtag(headerValue) {
  if (headerValue == null) return null;
  return headerValue.replace(/^"|"$/g, '');
}

export class GatewayImageryLayerAdapter extends ImageryLayerAdapter {
  /**
   * @param {{id?: string, manifestSha256: string, origin?: string, fetchImpl?: Function}} opts
   *   `manifestSha256` addresses which tile set's `/api/tiles/<manifestSha256>/...`
   *   route this adapter fetches from (`crates/av-tiles/src/route.rs`'s own route
   *   shape). `origin` -- see this file's module docstring -- defaults to `''`
   *   (same-origin relative URL); a caller never has any reason to pass an absolute
   *   `http(s)://` host in the browser, only this task's own headless harness does
   *   (`node` has no implicit page origin). `fetchImpl` (default the platform
   *   `fetch`) is the one seam `web/js/layers_stream_check.mjs`'s own tests use to
   *   inject a stub response for the ETag-mismatch proof
   *   (`gateway_imagery_layer_check.mjs`) without a real network call.
   */
  constructor({ id = 'gateway-imagery', manifestSha256, origin = '', fetchImpl = defaultFetch, tileBytes } = {}) {
    if (!manifestSha256) {
      throw new TypeError('GatewayImageryLayerAdapter: manifestSha256 is required');
    }
    // imageryUrl is passed as '' and loader as a never-called placeholder: this
    // class never uses ImageryLayerAdapter's own load() (it overrides load()
    // below), and plan() below overwrites every url this template would have
    // produced with this adapter's own gateway address -- see this file's module
    // docstring, "plan(view) does NOT reimplement...".
    // `tileBytes` is forwarded, not defaulted here, so `ImageryLayerAdapter`'s own
    // default (`IMAGERY_TILE_BYTES`) stays the single place that number lives: a
    // caller streaming a tile set whose `tile_size` is not 256 declares the real
    // per-tile cost (the manifest carries it), and this harness then checks that
    // declaration against the length of every tile the gateway actually returns.
    super({ id, imageryUrl: '', loader: { load: NEVER_CALLED_LOADER }, ...(tileBytes === undefined ? {} : { tileBytes }) });
    this._manifestSha256 = manifestSha256;
    this._origin = origin;
    this._fetch = fetchImpl;
    // Round 4 (question 228): `null` until `fetchManifest()` (below) resolves, then a
    // `Map` from `tileKey({level,x,y})`'s own string format (`manifestTileKey`,
    // `./tileset_manifest.js` -- IDENTICAL format to `web/js/globe_lod.js`'s own
    // `tileKey`, which is exactly what `r.key` already is on every request
    // `ImageryLayerAdapter.plan()` produces, see `plan()` below) to that tile's real
    // `TileEntry.size_bytes`. Never fetched automatically by the constructor -- a
    // caller (a real viewer, or this task's own headless harness) awaits
    // `fetchManifest()` explicitly, exactly like `web/js/layers_stream_check.mjs`'s
    // own separate calibration fetch before its real camera-path loop starts.
    this._manifestSizeByTile = null;
    this.manifestLoaded = false;
    this.manifestTileCount = 0;
  }

  /** Fetches and decodes this tile set's own manifest (`GET
   * /api/tiles/<manifestSha256>/manifest`, `altavista/server.py`'s `tiles_manifest`
   * route -- the same real, same-origin proxy `_tileUrl` below uses for tile bytes,
   * question 51: never a second origin) and populates the per-tile byte-cost map
   * `plan()` (below) reads from thereafter. Idempotent-by-caller-discipline (calling
   * it twice simply re-fetches and replaces the map; this class does not itself
   * cache across calls or de-duplicate concurrent calls -- a caller that only ever
   * awaits it once, before its first `plan()`/`update()` call, exactly like every
   * other one-time setup fetch in this codebase, gets the single-fetch behaviour for
   * free without this method adding sequencing logic on top of a single `await`).
   *
   * **REQUIREMENT ON CALLERS, not a suggestion (manager review of round 4's own
   * admission fix):** await this method to completion BEFORE handing this layer to a
   * `LayerManager` (i.e. before that manager's first `update()` that could `plan()`
   * this layer at all) -- never let a real viewer's first frame(s) run with the
   * manifest still in flight. `LayerManager.update()` DOES now reconcile an
   * already-resident entry's `byteCost` if a later `plan()` call reports a different
   * one for the same key (see `web/js/layers/layer.js`'s own `update()`, the
   * reconciliation pass) -- so a tile admitted at this class's fallback estimate
   * before the manifest resolves is no longer PERMANENTLY stuck at that stale
   * number, and the hard admission invariant is defended even if a caller gets the
   * ordering wrong. But that reconciliation is DEFENCE IN DEPTH, not a licence to
   * open this window on purpose: every tile admitted before the manifest resolves is
   * a real HTTP round trip and a real GPU-texture-shaped resident cost accounted in
   * the wrong units for however long that window stays open, and a revision that
   * later turns out to be irreconcilable (the true costs of everything still wanted
   * simply do not fit, with nothing unwanted left to evict) surfaces as a genuine,
   * correctly-reported `softViolationCount` increment rather than being avoided in
   * the first place. `await layer.fetchManifest()` once, up front, and this window
   * never opens at all -- see `web/js/layers_stream_check.mjs`'s own real-gateway
   * harness for the concrete "fetch the manifest before the camera path starts"
   * shape a real caller should copy.
   *
   * Rejects with the same typed `TileHttpError` a non-2xx tile fetch would (a
   * manifest fetch is not special-cased: `altavista/server.py`'s proxy passes the
   * real gateway status through unchanged for the manifest route exactly as it does
   * for the tile route), or with `./tileset_manifest.js`'s own typed
   * `ManifestDecodeError` if the response body does not decode as a well-formed
   * `TileSetManifest`. Never resolves with a partially-populated map: on ANY
   * rejection here, `this._manifestSizeByTile` is left exactly as it was before this
   * call (`null` on a first call, or the previous manifest's map on a re-fetch) --
   * `plan()` then keeps using the fallback estimate (or the previous manifest) for
   * every tile, never a half-decoded manifest silently mixed with fallback values for
   * a single run.
   * @param {AbortSignal} [signal]
   * @returns {Promise<number>} the number of distinct tiles this manifest lists.
   */
  async fetchManifest(signal) {
    const url = `${this._origin}/api/tiles/${this._manifestSha256}/manifest`;
    const resp = await this._fetch(url, signal ? { signal } : undefined);
    if (!resp.ok) {
      throw new TileHttpError(url, resp.status);
    }
    const bytes = await resp.arrayBuffer();
    const decoded = decodeTileSetManifest(bytes); // throws ManifestDecodeError on malformed bytes -- see that function's own doc comment
    const sizeByTile = new Map();
    for (const entry of decoded.tiles) {
      sizeByTile.set(manifestTileKey(entry), entry.sizeBytes);
    }
    this._manifestSizeByTile = sizeByTile;
    this.manifestLoaded = true;
    this.manifestTileCount = decoded.tiles.length;
    return this.manifestTileCount;
  }

  /** `super.plan(view)` -- `ImageryLayerAdapter.plan()` -- computes every field
   * (`key`, `sseError`, `viewDistanceM`, `byteCost`, `tile`) from `web/js/
   * globe_lod.js`'s own real tile selection and screen-space-error arithmetic; this
   * override replaces the *local-fixture* `url` that call would have built with this
   * adapter's own real gateway tile address (`_tileUrl`, below), and -- round 4,
   * question 228 -- replaces `byteCost` with this tile's own real manifest-declared
   * size whenever `fetchManifest()` has resolved AND the manifest actually lists this
   * exact `(level,x,y)` (see this file's module docstring for the fallback and the
   * `byteCostSource` tag every request now carries, both cases). `r.key` is already
   * `tileKey(tile)` (`ImageryLayerAdapter.plan()`'s own computation, reused, never
   * duplicated) -- exactly `manifestTileKey`'s own format, so the lookup below is a
   * single `Map.get`, no second key derivation. */
  plan(view) {
    return super.plan(view).map((r) => {
      const url = this._tileUrl(r.tile);
      const manifestByteCost = this._manifestSizeByTile ? this._manifestSizeByTile.get(r.key) : undefined;
      if (manifestByteCost !== undefined) {
        return {
          ...r, url, byteCost: manifestByteCost, byteCostSource: 'manifest',
        };
      }
      // Fallback -- see this file's module docstring: the constructor's own declared
      // `tileBytes` (`r.byteCost`, unchanged from `ImageryLayerAdapter.plan()`'s own
      // computation), explicitly tagged so a caller/harness can never mistake this
      // for a manifest-verified number.
      return { ...r, url, byteCostSource: 'fallback-estimate' };
    });
  }

  /** `<origin>/api/tiles/<manifestSha256>/tiles/<level>/<x>/<y>` -- exactly
   * `altavista/server.py`'s `tiles_tile` route shape, which itself proxies
   * `crates/av-tiles/src/route.rs`'s `Route::Tile`. Never builds an absolute host
   * unless `origin` itself is one (see this file's module docstring, question 51). */
  _tileUrl(tile) {
    return `${this._origin}/api/tiles/${this._manifestSha256}/tiles/${tile.level}/${tile.x}/${tile.y}`;
  }

  /** Real `fetch`, honouring `signal` (the platform `AbortSignal` `LayerManager`
   * hands every `load()`, see layer.js's module docstring) natively -- unlike
   * `ImageryLayerAdapter.load()`'s injected `THREE.TextureLoader`-shaped stub
   * (whose underlying XHR/fetch has no public abort hook, see that file's own
   * docstring), `fetch(url, {signal})` itself rejects with a platform `AbortError`
   * the instant `signal` fires, so no bespoke cancellation bookkeeping is needed
   * here at all. Rejects with a typed `TileHttpError` on any non-2xx status
   * (`resp.ok` is false for anything outside [200,300) -- `altavista/server.py`'s
   * proxy passes the gateway's real status through unchanged, 206/304/403/404/416
   * included, per `tiles_client.py`'s own rule 4), and with a typed
   * `TileEtagMismatchError` if the recomputed SHA-256 of the received bytes does
   * not equal the gateway's own `ETag` (see this file's module docstring).
   */
  async load(request, signal) {
    const resp = await this._fetch(request.url, { signal });
    if (!resp.ok) {
      throw new TileHttpError(request.url, resp.status);
    }
    const bytes = await resp.arrayBuffer();
    const digest = await crypto.subtle.digest('SHA-256', bytes);
    const actualSha256 = hex(digest);
    const expectedEtag = unquoteEtag(resp.headers.get('etag'));
    if (expectedEtag !== actualSha256) {
      throw new TileEtagMismatchError(request.url, expectedEtag, actualSha256);
    }
    return {
      kind: 'gateway-imagery-tile', tile: request.tile, url: request.url, sha256: actualSha256, bytes,
    };
  }

  /** No GPU/decoded resource of this adapter's own to free -- see
   * `ImageryLayerAdapter.release`'s identical reasoning; this override exists only
   * because `plan()`/`load()` are overridden and `release()` is inherited
   * unchanged, so this comment, not `ImageryLayerAdapter`'s, is the one that
   * actually applies to what THIS class's `load()` returns (a plain `{bytes,
   * sha256, ...}` object, never a `THREE.Texture`). */
  release(_key) {}
}

function defaultFetch(...args) {
  return fetch(...args);
}

function NEVER_CALLED_LOADER() {
  throw new Error(
    'GatewayImageryLayerAdapter: the injected THREE.TextureLoader-shaped loader seam '
    + 'is unused -- this class overrides load() itself (a real fetch) and never calls '
    + "ImageryLayerAdapter's own load(); reaching this function means something called "
    + 'the inherited load() by mistake.',
  );
}
