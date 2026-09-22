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
import * as THREE from 'three';
import { ImageryLayerAdapter } from './imagery_layer.js';
import { decodeTileSetManifest, manifestTileKey } from './tileset_manifest.js';

// Round 6 (docs/open-questions.md question 231's ruling, "replace or composite" --
// docs/heavy-plan.md's round-5 status, "the one thing round 5 does NOT deliver").
// The seam this file cuts, stated once, in full (the two "real problems" the task
// brief itself named, not papered over):
//
// Problem 1 -- `load()` used to resolve with `{kind, tile, url, sha256, bytes}`, raw
// verified bytes, never something `THREE.Material.map` would accept; `web/js/
// globe.js`'s `applyTileTexture` needs a `THREE.Texture`-shaped payload, exactly what
// `ImageryLayerAdapter.load()` already produces (via the injected
// `THREE.TextureLoader`). The conversion happens RIGHT HERE, in `load()`, AFTER the
// SHA-256/ETag verification below has already run and thrown on any mismatch --
// never before it, and never weakened by it: this class still recomputes the SHA-256
// over the exact bytes it received and rejects with the same typed
// `TileEtagMismatchError` on any disagreement, byte for byte unchanged from before
// this task. Only once those bytes are genuinely trustworthy are they handed to
// `decodeTileBytesToTexture` (below) -- "the conversion happens where the bytes are
// owned": this adapter is the one and only place in this codebase that ever has
// these bytes in hand at all (`LayerManager` treats `payload` as opaque,
// `web/js/layers/layer.js`'s own module docstring), so there is no more honest place
// to put it.
//
// The verified bytes are NOT thrown away once decoded -- they are kept, alongside
// the digest and the source tile/url, as `texture.userData.bytes`/`.sha256`/`.tile`/
// `.url` (THREE's own sanctioned extension point on every `Object3D`/`Texture`
// instance, see `THREE.Texture`'s own constructor: `this.userData = {}`). This is
// what makes `web/js/layers_stream_check.mjs` -- which independently re-verifies the
// SHA-256 a SECOND time and parses the real PNG header, both genuinely synchronous
// per-tile work this task's proof depends on (see that file's own module docstring,
// "What a frame is") -- able to keep doing exactly that against a real texture
// payload, unchanged in substance, only in shape (`payload.bytes` ->
// `payload.userData.bytes`).
//
// `texture.userData.sourceLayerId = this.id` (set below, in `load()`) is the same
// provenance tag `./imagery_layer.js`'s own `ImageryLayerAdapter.load()` now applies
// at ITS source -- a real, per-texture property `GlobeLayer`/a test can trace back to
// the layer that produced it, never a global counter.
//
// Problem 2 (tile-key agreement) -- verified, not assumed: `ImageryLayerAdapter.plan()`
// (this class's own superclass, called by `super.plan(view)` below, never
// reimplemented) sets `key: tileKey(tile)` (`web/js/globe_lod.js`'s own function).
// `web/js/globe.js`'s `GlobeLayer.update()` builds every mesh's own `_meshes` Map key
// via the SAME `tileKey` import (`import { ... tileKey ... } from './globe_lod.js'`,
// globe.js's own top-of-file imports) and reads `getResidentPayload(globalKeyFor(
// layer.id, k))` for that SAME `k`. Both call sites resolve to the one function
// (`globe_lod.js`'s `tileKey`, a pure `${level}/${x}/${y}` join over non-negative
// integers -- see that file for the no-legal-collision reasoning `layer.js`'s
// `globalKeyFor` doc comment already cites for the SAME function). There is no second,
// independently-derived key format anywhere in this path to disagree with it. A
// selected tile set whose manifest simply does not cover a requested `(level,x,y)`
// never reaches this file's `load()` at all in a way that would matter here: it still
// gets a `Request` (from `ImageryLayerAdapter.plan()`, unconditionally, for every tile
// `view.tiles` contains) and `LayerManager` still tries to admit/load it against this
// adapter's real gateway URL, which 404s (a real `TileHttpError`, counted in
// `_onFailed`/`failureNames()`, never resident) -- `getResidentPayload` for that key
// then simply stays `undefined` forever for this layer, which is exactly the
// "fallback handles it" case the task brief names.

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

// A single, opaque, always-valid 1x1 opaque-white RGBA texel -- the documented
// fallback payload for EITHER of `decodeTileBytesToTexture`'s two fallback cases
// (below): the platform has no `createImageBitmap` at all (every version of node
// this codebase's own binding rules target, see this file's module docstring's own
// measurement: `typeof createImageBitmap === 'function'` is `false` under plain
// node), or it does but decoding these particular ETag-verified bytes as an image
// still failed. A real `THREE.WebGLRenderer` accepts this exactly as it would any
// other `THREE.DataTexture` (`web/js/gateway_imagery_layer_check.mjs` proves
// `isTexture`/`.dispose` on it directly) -- never a fake object a real renderer
// would reject, just genuinely, disclosedly not the tile's own real pixels.
// `web/js/layers_stream_check.mjs`'s own `texture.userData.bytes`
// (`decodeTileBytesToTexture`, below) is what still lets that harness verify the
// REAL bytes this adapter received, independent of whatever this placeholder's own
// (deliberately uninformative) pixel content is.
const FALLBACK_TEXEL_RGBA = new Uint8Array([255, 255, 255, 255]);

/** Turns ETag-verified tile bytes into a `THREE.Texture`-shaped payload --
 * `applyTileTexture` (`web/js/globe.js`) needs exactly this shape to bind
 * `mesh.material.map`, and `ImageryLayerAdapter.load()`'s own payload (a real
 * `THREE.TextureLoader`'s own texture, in production) already has it; this function
 * is what gives `GatewayImageryLayerAdapter.load()` (below) the same shape.
 *
 * Real decode path: when the platform exposes `createImageBitmap` (every real
 * browser this codebase targets, never plain node -- see `FALLBACK_TEXEL_RGBA`'s own
 * doc comment), the bytes are wrapped in a `Blob` (tagged `image/png` -- every tile
 * this codebase's own tiler/fixture generator produces is a PNG, `crates/av-jobs`'
 * own tiler and `web/fixtures/gen_globe_tiles.py` agree) and decoded for real, then
 * wrapped in a genuine `new THREE.Texture(bitmap)` -- REAL pixels, a REAL GPU-upload-
 * ready texture, not a stand-in.
 *
 * TWO documented, disclosed fallback cases -- neither one ever rejects `load()`
 * (found the hard way, this task's own report: an EARLIER version of this function
 * rejected with a typed `TileDecodeError` on a real decode failure, which made
 * `tests/test_viewer_layers_panel.py`'s own real-browser proof regress -- that
 * fixture's own tile bytes, like several others across this codebase's test suite,
 * are a repeating ASCII pattern tagged `media_type: "image/png"`, never a REAL PNG,
 * exactly like a real `THREE.TextureLoader`'s own `onError` path -- `web/js/globe.js`'s
 * `buildTileMesh`'s direct-load callback -- already treats a failed image load as
 * "keep the flat colour material, never throw", this codebase's standing "graceful
 * fallback, never a hard error" rule applied consistently across BOTH of this
 * adapter's own texture-producing paths, not a new, stricter one invented for the
 * gateway path alone):
 *   - no `createImageBitmap` at all (plain node, this task's own headless proof
 *     surface) resolves with `FALLBACK_TEXEL_RGBA` wrapped in a real
 *     `THREE.DataTexture`, tagged `decodeMode: 'placeholder-no-createImageBitmap'`;
 *   - `createImageBitmap` exists but decoding THESE bytes failed (a real decode
 *     error, kept -- never swallowed -- as `texture.userData.decodeError`) resolves
 *     with the SAME placeholder texture, tagged `decodeMode:
 *     'createImageBitmap-failed'` instead, so the two cases are always
 *     distinguishable in the printed/inspected payload, never conflated.
 * Both fallback cases still carry this file's own provenance tag
 * (`load()`, below, sets `texture.userData.sourceLayerId` on every payload
 * regardless of which of the three `decodeMode`s produced it) -- "a headless harness
 * with no image decoder must degrade to something that still proves provenance",
 * not to a failure a selected tile set would then never actually show up in.
 * `texture.userData.decodeMode` says which of the three cases ran, on every payload,
 * unconditionally, this codebase's "a gap is recorded, never hidden" rule applied to
 * a decode mode exactly like it already is to a budget deferral/failure/byteCost
 * revision elsewhere in `web/js/layers/`. The ONE piece this genuinely cannot prove
 * under node -- that a REAL PNG this codebase's own gateway serves decodes into
 * REAL, correctly-sized pixels -- is proved instead in a real headless-Chrome
 * browser check (`tests/test_viewer_globe_layer_manager.py`'s own CDP pattern; see
 * this task's own report for exactly where).
 * @param {ArrayBuffer} bytes
 * @returns {Promise<THREE.Texture>}
 */
// Exported (round 6, manager review) so a real browser can exercise the REAL
// `createImageBitmap` branch directly, against a REAL PNG, without needing a real
// `av-tiles` gateway/manifest/ETag round trip to reach it -- see
// `tests/test_viewer_globe_layer_manager.py`'s own
// `test_decode_tile_bytes_to_texture_really_decodes_a_real_png_in_a_real_browser`
// for exactly that proof (this task's own report explains why the OTHER real-gateway
// proofs in this round, `web/js/layers_stream_check.mjs` and its GlobeLayer probe,
// are structurally placeholder-mode under node -- no `createImageBitmap` there at
// all -- even though the real gateway they drive serves genuinely real PNG bytes,
// `crates/av-jobs/src/tiler.rs`'s own `image::codecs::png` encoder).
export async function decodeTileBytesToTexture(bytes) {
  if (typeof createImageBitmap === 'function') {
    try {
      const bitmap = await createImageBitmap(new Blob([bytes], { type: 'image/png' }));
      const texture = new THREE.Texture(bitmap);
      texture.needsUpdate = true;
      texture.userData.decodeMode = 'createImageBitmap';
      return texture;
    } catch (cause) {
      const texture = new THREE.DataTexture(FALLBACK_TEXEL_RGBA, 1, 1, THREE.RGBAFormat);
      texture.needsUpdate = true;
      texture.userData.decodeMode = 'createImageBitmap-failed';
      texture.userData.decodeError = String((cause && cause.message) || cause);
      return texture;
    }
  }
  const texture = new THREE.DataTexture(FALLBACK_TEXEL_RGBA, 1, 1, THREE.RGBAFormat);
  texture.needsUpdate = true;
  texture.userData.decodeMode = 'placeholder-no-createImageBitmap';
  return texture;
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
    // Round 6 (question 231's ruling): `load()` (below) now hands `LayerManager` a
    // real `THREE.Texture`-shaped payload it created -- a real GPU resource this
    // adapter, not `LayerManager` (which treats `payload` as opaque) or `GlobeLayer`
    // (which never owned this class's own payload), is responsible for freeing.
    // Keyed by this adapter's own LOCAL key (`request.key`, `ImageryLayerAdapter.
    // plan()`'s `tileKey(tile)` -- the exact string `release(key)` below receives,
    // `LayerManager._evictEntry`'s own `entry.localKey`), so `release()` disposes
    // EXACTLY the texture `load()` created for that key, never guesses from a
    // payload it is not even handed (the Layer interface's `release(key): void`
    // signature carries no payload at all -- see ./layer.js's own module
    // docstring).
    this._texturesByKey = new Map();
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
    // Round 6 (question 231's ruling) -- see this file's own module docstring,
    // "Problem 1", for the full reasoning: the SHA-256/ETag verification above is
    // unweakened and unchanged; only once it has already thrown on any mismatch are
    // these now-trustworthy bytes handed to `decodeTileBytesToTexture` to become a
    // real `THREE.Texture`-shaped payload -- `applyTileTexture` (web/js/globe.js)
    // needs exactly that shape, `ImageryLayerAdapter.load()`'s own payload already
    // has it, and this is what gives this class's own payload the same shape.
    const texture = await decodeTileBytesToTexture(bytes);
    // Provenance (question 231's ruling: "a real, per-texture property traceable to
    // the layer that produced it") -- tagged AT THE SOURCE, the same discipline
    // `./imagery_layer.js`'s own `ImageryLayerAdapter.load()` applies to its own
    // payload, just inline here rather than via that superclass's own `load()`
    // (never called -- see `NEVER_CALLED_LOADER`, below).
    texture.userData.kind = 'gateway-imagery-tile';
    texture.userData.sourceLayerId = this.id;
    texture.userData.tile = request.tile;
    texture.userData.url = request.url;
    texture.userData.sha256 = actualSha256;
    // The verified bytes themselves are NOT discarded -- kept here so a caller that
    // genuinely needs the real wire payload (web/js/layers_stream_check.mjs's own
    // second, synchronous SHA-256 check and real PNG-header parse -- see this file's
    // module docstring) still can, unchanged in substance from before this task.
    texture.userData.bytes = bytes;
    // See the constructor's own doc comment on `_texturesByKey`: `release(key)`
    // below has no other way to find the exact GPU resource `load()` created for
    // this request's local key, because the Layer interface's `release(key): void`
    // is never handed the payload itself.
    this._texturesByKey.set(request.key, texture);
    return texture;
  }

  /** Disposes the REAL `THREE.Texture`/`THREE.DataTexture` GPU resource `load()`
   * created for this local `key` (see the constructor's own doc comment on
   * `_texturesByKey`) -- unlike `ImageryLayerAdapter.release()`'s own no-op (that
   * class's payload is disposed where the consuming mesh/material lives,
   * `web/js/globe.js`'s own per-mesh cleanup -- see that class's doc comment), THIS
   * class's `load()` now creates a real GPU resource of its own (round 6, question
   * 231's ruling), so `release()` is the one and only place this codebase disposes
   * it when `LayerManager` evicts/unregisters it (LRU eviction, `removeLayer` on
   * toggle-off, `web/js/app.js`'s own `toggleGatewayLayer`) -- never leaked, and
   * safe even if `web/js/globe.js`'s own per-mesh cleanup ALSO happens to call
   * `.dispose()` on the very same texture object first (a real, disclosed
   * interaction this task's own report explains in full: a mesh whose tile falls
   * out of this tick's own LOD selection is torn down, disposing whatever
   * `material.map` currently points at, at the SAME moment every registered
   * imagery layer's `plan()` stops declaring that tile wanted at all -- so a
   * resident entry this adapter still owns for that exact key can, on a LATER LRU
   * eviction, be disposed a second time here; `THREE.Texture.prototype.dispose()`
   * is itself idempotent -- it only ever re-dispatches an internal `'dispose'`
   * event a real `WebGLRenderer`'s texture cache already handles as a no-op for an
   * already-removed entry -- so this is inert, not a double-free in any sense the
   * GPU/driver would ever see). No-op if `key` was never loaded (already
   * evicted/never resident) or already released once. */
  release(key) {
    const texture = this._texturesByKey.get(key);
    if (!texture) return;
    texture.dispose();
    this._texturesByKey.delete(key);
  }
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
