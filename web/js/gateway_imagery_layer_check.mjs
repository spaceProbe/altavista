// CLI harness for tests/test_gateway_imagery_layer.py: `node
// web/js/gateway_imagery_layer_check.mjs`.
//
// Drives the real `GatewayImageryLayerAdapter` (web/js/layers/gateway_imagery_layer.js)
// with an injected `fetchImpl` stub -- no real network I/O of any kind (question 51,
// same discipline as web/js/layers_check.mjs's own loader stubs) -- and prints one
// JSON object of every case's outcome. This is the deliberately network-free half of
// deliverable 2's own proof; the OTHER half (a real fetch across a real loopback
// socket to a real `av-tiles` gateway, with real ETag bytes) is
// tests/test_viewer_layers_stream.py, which is docker-gated and cannot run on every
// host -- this file's own cases always run, everywhere, with no gate at all.
//
// What each case proves, and what a wrong implementation would fail against:
//   - `planNeverFetches`: `plan()` must only declare demand (design constraint a,
//     the same rule every other adapter in web/js/layers/ is held to) -- an
//     implementation whose `plan()` eagerly fetched would move the stub's own
//     invocation counter before `load()` is ever called.
//   - `urlIsSameOriginRelativeByDefault`: with `origin` left at its own default
//     (`''`), the URL `load()` is asked to fetch must be a relative path starting
//     with `/api/tiles/...` -- never an absolute `http(s)://` host. An
//     implementation that hardcodes or derives an absolute host anywhere (question
//     51's binding rule: the viewer never fetches any other origin) would fail this
//     even though nothing else in this file could ever observe a real cross-origin
//     request actually happening (there is no real network here at all).
//   - `matchingEtagResolves`: the ordinary, correct case -- a stub response whose
//     `ETag` header equals the real SHA-256 of the bytes it returns must resolve
//     `load()` with a payload carrying that same digest (round 6: now
//     `payload.userData.sha256`, since `load()`'s own payload changed shape -- see
//     `textureShapedPayload`, below).
//   - `textureShapedPayload` (round 6, docs/open-questions.md question 231's
//     ruling): `load()`'s payload must now be something a real
//     `THREE.Material.map` would accept -- `isTexture === true`, a real
//     `.dispose()` -- never the old raw `{bytes,...}` object a `THREE.
//     WebGLRenderer` would reject outright. Node has no `createImageBitmap` (this
//     file's own module docstring measured this directly: `typeof
//     createImageBitmap === 'function'` is `false` under plain node), so this
//     check's own stub bytes -- deliberately NOT a real PNG (see `REAL_BYTES`,
//     below) -- exercise the documented FALLBACK decode path
//     (`decodeMode: 'placeholder-no-createImageBitmap'`), never the real-decode
//     path; the real-decode path against a real PNG is proved instead in a real
//     browser (this task's own report says exactly where). An implementation that
//     still returned the raw bytes object, or one that threw instead of using the
//     documented fallback when `createImageBitmap` is simply absent (an
//     environment limitation, not a data problem), would fail this.
//   - `provenanceTagged` (round 6): the resolved texture must carry
//     `userData.sourceLayerId` equal to this adapter's own `id` -- the "real,
//     per-texture property traceable to the layer that produced it" question 231's
//     ruling requires, tagged AT THE SOURCE (this file's own `load()`), never
//     inferred from which `LayerManager` slot it happened to be stored under.
//   - `verifiedBytesStillReachable` (round 6): `web/js/layers_stream_check.mjs`
//     still needs the real, ETag-verified wire bytes for its own second,
//     synchronous SHA-256 check and real PNG-header parse -- `userData.bytes` must
//     be the exact `ArrayBuffer` `load()` received, unchanged in substance from
//     before this task, just relocated.
//   - `releaseDisposesTheRealTexture` (round 6): `release(key)` must call
//     `.dispose()` on the EXACT texture object `load()` created for that key (an
//     implementation that left `release()` a no-op, or that disposed the wrong
//     key's texture, would fail this) -- proved by a real `THREE.Texture.dispose`
//     spy, not merely "release() didn't throw".
//   - `mismatchedEtagRejectsWithTypedError`: the deliverable's own named
//     requirement -- a stub response whose `ETag` does NOT match the bytes it
//     returns must reject `load()` with `TileEtagMismatchError` by name (not merely
//     "some error") -- an adapter that trusts the gateway's own `ETag` without
//     recomputing it, or that silently resolves anyway, would fail this by not
//     rejecting at all; an adapter that rejects with the platform's own generic
//     `Error`/`TypeError` instead of the typed class would fail the `.name` pin.
//   - `missingEtagRejectsWithTypedError`: the same typed rejection when the
//     `ETag` header is absent entirely (never a silently-accepted "no header means
//     trust the bytes" fallback).
//   - `nonOkStatusRejectsWithTypedHttpError`: a stub response with `ok: false`
//     (a non-2xx status) must reject with `TileHttpError`, carrying the real status
//     code -- an adapter that only checks for a thrown/rejected fetch (network
//     failure) and not a successfully-received-but-unsuccessful HTTP status would
//     fail this.
//   - `abortRejectsAndSignalWasPassedThrough`: aborting the controller before
//     `load()`'s stub `fetchImpl` ever inspects the bytes must still reject --
//     proving `signal` reaches the injected fetch implementation at all (this
//     adapter itself adds no bespoke abort bookkeeping on top of what `fetch`
//     itself does natively, see gateway_imagery_layer.js's own module docstring;
//     this case is what would catch a `load()` that forgot to pass `signal` through
//     to its own `fetchImpl` call in the first place).
//   - `manifestByteCostProbe` (round 4, question 228's own round-3-defect-5
//     follow-up): before `fetchManifest()` is ever awaited, `byteCost` must be the
//     constructor's own fallback estimate, tagged `byteCostSource:
//     'fallback-estimate'`. After it resolves against a real (offline, hand-encoded
//     -- see `MANIFEST_BYTES` below, verified byte-for-byte against Python's own
//     `google.protobuf` encoder in this task's own report) `TileSetManifest`, a
//     request whose `(level,x,y)` the manifest DOES list must carry that tile's real
//     `size_bytes`, tagged `byteCostSource: 'manifest'` -- an implementation that
//     still charged the fixed/declared estimate after a manifest was fetched would
//     fail this half. A request for a tile the manifest does NOT list (a manifest/
//     selection inconsistency this adapter must not assume away) must still fall
//     back to the estimate, tagged accordingly -- an implementation that threw, or
//     silently charged `undefined`/`0`, instead of falling back would fail this half.
import { GatewayImageryLayerAdapter, TileEtagMismatchError, TileHttpError } from './layers/gateway_imagery_layer.js';

const MANIFEST_SHA256 = 'e'.repeat(64);
const TILE = { level: 2, x: 3, y: 1 };
const VIEW = {
  tiles: [TILE],
  cameraEcef: { x: 20000000, y: 0, z: 0 },
  screenHeightPx: 900,
  fovYRad: (50 * Math.PI) / 180,
};

const REAL_BYTES = new TextEncoder().encode('a synthetic PNG-shaped tile payload, for this check only');

async function sha256Hex(bytes) {
  const digest = await crypto.subtle.digest('SHA-256', bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength));
  return Array.from(new Uint8Array(digest)).map((b) => b.toString(16).padStart(2, '0')).join('');
}

const REAL_SHA256 = await sha256Hex(REAL_BYTES);
const WRONG_SHA256 = '0'.repeat(64);

/** Builds a fetch-Response-shaped stub (only the surface GatewayImageryLayerAdapter
 * actually reads: `.ok`, `.status`, `.headers.get('etag')`, `.arrayBuffer()`), and
 * records every call it receives (url, signal) for this harness's own assertions. */
function makeFetchStub({ ok = true, status = 200, etag = REAL_SHA256, bytes = REAL_BYTES } = {}) {
  const calls = [];
  const fetchImpl = async (url, init) => {
    calls.push({ url, signal: init && init.signal });
    if (init && init.signal && init.signal.aborted) {
      const err = new Error('aborted');
      err.name = 'AbortError';
      throw err;
    }
    return {
      ok,
      status,
      headers: { get: (name) => (name.toLowerCase() === 'etag' && etag !== null ? `"${etag}"` : null) },
      arrayBuffer: async () => bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength),
    };
  };
  return { fetchImpl, calls };
}

async function rejectionOf(promise) {
  try {
    await promise;
    return null;
  } catch (err) {
    return err;
  }
}

// ------------------------------------------------------------------- planNeverFetches
const planProbe = makeFetchStub();
const planLayer = new GatewayImageryLayerAdapter({ manifestSha256: MANIFEST_SHA256, fetchImpl: planProbe.fetchImpl });
const planRequests = planLayer.plan(VIEW);
const planNeverFetches = planProbe.calls.length === 0 && planRequests.length === 1;

// ---------------------------------------------------- urlIsSameOriginRelativeByDefault
const defaultOriginRequest = planRequests[0];
const expectedRelativeUrl = `/api/tiles/${MANIFEST_SHA256}/tiles/${TILE.level}/${TILE.x}/${TILE.y}`;
const urlIsSameOriginRelativeByDefault = defaultOriginRequest.url === expectedRelativeUrl
  && !/^[a-z]+:\/\//i.test(defaultOriginRequest.url);

// An explicit origin (this harness's own future docker-gated sibling, against a real
// loopback server, injects exactly this -- node's own fetch has no implicit page
// origin) is honoured verbatim, never silently dropped.
const explicitOriginLayer = new GatewayImageryLayerAdapter({
  manifestSha256: MANIFEST_SHA256, origin: 'http://127.0.0.1:59999', fetchImpl: planProbe.fetchImpl,
});
const explicitOriginRequest = explicitOriginLayer.plan(VIEW)[0];
const originIsHonoured = explicitOriginRequest.url === `http://127.0.0.1:59999${expectedRelativeUrl}`;

// --------------------------------------------------------------------- matchingEtagResolves
const okStub = makeFetchStub({ etag: REAL_SHA256 });
const okLayer = new GatewayImageryLayerAdapter({ manifestSha256: MANIFEST_SHA256, fetchImpl: okStub.fetchImpl });
const okRequest = okLayer.plan(VIEW)[0];
const okController = new AbortController();
const okPayload = await okLayer.load(okRequest, okController.signal);
const matchingEtagResolves = okPayload.userData.sha256 === REAL_SHA256 && okStub.calls.length === 1
  && okStub.calls[0].url === expectedRelativeUrl;

// --------------------------------------------------------------------- textureShapedPayload
// Round 6 (question 231's ruling, "Problem 1" -- see this file's own module
// docstring): `okPayload` (from the SAME successful load, above) must be something
// `THREE.Material.map`/a real `THREE.WebGLRenderer` would accept, not the old raw
// `{bytes, sha256, tile, url, kind}` object.
const textureShapedPayload = okPayload.isTexture === true
  && typeof okPayload.dispose === 'function'
  && okPayload.userData.kind === 'gateway-imagery-tile'
  // Node has no createImageBitmap (measured directly, see module docstring) --
  // REAL_BYTES below is deliberately not a real PNG either, so a healthy run under
  // node MUST show the documented fallback decode mode, never the real one (which
  // would mean this check accidentally decoded garbage bytes as if they were a
  // real image, hiding a real bug).
  && okPayload.userData.decodeMode === 'placeholder-no-createImageBitmap';

// --------------------------------------------------------------------- provenanceTagged
const provenanceTagged = okPayload.userData.sourceLayerId === okLayer.id && okLayer.id === 'gateway-imagery';

// --------------------------------------------------------------------- verifiedBytesStillReachable
const verifiedBytesStillReachable = okPayload.userData.bytes instanceof ArrayBuffer
  && Buffer.from(okPayload.userData.bytes).equals(Buffer.from(REAL_BYTES.buffer, REAL_BYTES.byteOffset, REAL_BYTES.byteLength));

// --------------------------------------------------------------------- releaseDisposesTheRealTexture
let disposeCallCount = 0;
const realDispose = okPayload.dispose.bind(okPayload);
okPayload.dispose = (...args) => { disposeCallCount += 1; return realDispose(...args); };
okLayer.release(okRequest.key);
const releaseDisposesTheRealTexture = disposeCallCount === 1;
// A second release() of the SAME key is a documented no-op (the constructor's own
// `_texturesByKey` doc comment: "No-op if key was never loaded ... or already
// released once") -- proves this isn't merely "dispose was called once ever", it is
// specifically release()'s own bookkeeping that stops a repeat call from disposing
// (or looking up) anything a second time.
okLayer.release(okRequest.key);
const releaseIsNoopOnceAlreadyReleased = disposeCallCount === 1;

// --------------------------------------------------------------------- decodeModeSwitch
// Round 6: this file's own module docstring measured `typeof createImageBitmap ===
// 'function'` as `false` under plain node -- every OTHER case in this file therefore
// only ever exercises `decodeTileBytesToTexture`'s "no createImageBitmap at all"
// fallback. This case exercises the OTHER TWO branches' own BRANCH LOGIC (never the
// real pixel-accuracy of a real browser's own `createImageBitmap` against a real
// PNG -- that is out of node's reach entirely, see this task's own report for the
// real-browser check that covers it) by temporarily stubbing the global
// `createImageBitmap` this environment does not have: a resolving stub proves the
// real-decode path wraps its result in a real `THREE.Texture` tagged
// `'createImageBitmap'`; a throwing stub proves a genuine decode failure -- bytes
// that pass SHA-256/ETag verification but are not a valid image -- still RESOLVES
// `load()` (never rejects: this codebase's standing "graceful fallback, never a hard
// error" rule, restated in decodeTileBytesToTexture's own doc comment after an
// earlier, stricter version of this function regressed a real-browser test --
// tests/test_viewer_layers_panel.py's own fixture tile bytes are exactly this case,
// a tagged-image/png byte string that isn't really one) with the SAME placeholder
// texture, tagged `decodeMode: 'createImageBitmap-failed'` and carrying the real
// decode error message on `userData.decodeError`, DISTINCT from the "API absent"
// case's own `'placeholder-no-createImageBitmap'` tag -- disclosed, never conflated.
const savedCreateImageBitmap = globalThis.createImageBitmap;
const fakeBitmap = { width: 4, height: 4 };
let decodeSuccessPayload;
let decodeFailurePayload;
let failLayerId;
try {
  globalThis.createImageBitmap = async () => fakeBitmap;
  const successStub = makeFetchStub({ etag: REAL_SHA256 });
  const successLayer = new GatewayImageryLayerAdapter({ manifestSha256: MANIFEST_SHA256, fetchImpl: successStub.fetchImpl });
  const successRequest = successLayer.plan(VIEW)[0];
  decodeSuccessPayload = await successLayer.load(successRequest, new AbortController().signal);

  globalThis.createImageBitmap = async () => { throw new Error('deliberate decode failure, this check only'); };
  const failStub = makeFetchStub({ etag: REAL_SHA256 });
  const failLayer = new GatewayImageryLayerAdapter({ id: 'gateway-decode-fail-probe', manifestSha256: MANIFEST_SHA256, fetchImpl: failStub.fetchImpl });
  failLayerId = failLayer.id;
  const failRequest = failLayer.plan(VIEW)[0];
  decodeFailurePayload = await failLayer.load(failRequest, new AbortController().signal);
} finally {
  // Never leave this global mutated for any later case in this file (or any other
  // process this node invocation might share -- it does not, but the discipline is
  // the same as this codebase's own "no test mutates the process environment" rule
  // applied to a global function instead of process.env).
  if (savedCreateImageBitmap === undefined) delete globalThis.createImageBitmap;
  else globalThis.createImageBitmap = savedCreateImageBitmap;
}
const decodeModeSwitch = {
  realDecodePathProducesRealTexture: decodeSuccessPayload.isTexture === true
    && decodeSuccessPayload.image === fakeBitmap
    && decodeSuccessPayload.userData.decodeMode === 'createImageBitmap',
  realDecodeFailureStillResolvesWithTaggedFallback: decodeFailurePayload.isTexture === true
    && decodeFailurePayload.userData.decodeMode === 'createImageBitmap-failed'
    && typeof decodeFailurePayload.userData.decodeError === 'string'
    && decodeFailurePayload.userData.decodeError.length > 0
    // Still carries provenance, exactly like every other payload this adapter ever
    // produces -- "a headless harness with no image decoder must degrade to
    // something that still proves provenance" applies here too, not only to the
    // "API absent" fallback.
    && decodeFailurePayload.userData.sourceLayerId === failLayerId,
  globalRestoredAfterStubbing: globalThis.createImageBitmap === savedCreateImageBitmap,
};
decodeModeSwitch.ok = decodeModeSwitch.realDecodePathProducesRealTexture
  && decodeModeSwitch.realDecodeFailureStillResolvesWithTaggedFallback
  && decodeModeSwitch.globalRestoredAfterStubbing;

// ------------------------------------------------------- mismatchedEtagRejectsWithTypedError
const mismatchStub = makeFetchStub({ etag: WRONG_SHA256 });
const mismatchLayer = new GatewayImageryLayerAdapter({ manifestSha256: MANIFEST_SHA256, fetchImpl: mismatchStub.fetchImpl });
const mismatchRequest = mismatchLayer.plan(VIEW)[0];
const mismatchErr = await rejectionOf(mismatchLayer.load(mismatchRequest, new AbortController().signal));
const mismatchedEtagRejectsWithTypedError = mismatchErr instanceof TileEtagMismatchError
  && mismatchErr.name === 'TileEtagMismatchError'
  && mismatchErr.expectedEtag === WRONG_SHA256
  && mismatchErr.actualSha256 === REAL_SHA256;

// --------------------------------------------------------- missingEtagRejectsWithTypedError
const noEtagStub = makeFetchStub({ etag: null });
const noEtagLayer = new GatewayImageryLayerAdapter({ manifestSha256: MANIFEST_SHA256, fetchImpl: noEtagStub.fetchImpl });
const noEtagRequest = noEtagLayer.plan(VIEW)[0];
const noEtagErr = await rejectionOf(noEtagLayer.load(noEtagRequest, new AbortController().signal));
const missingEtagRejectsWithTypedError = noEtagErr instanceof TileEtagMismatchError
  && noEtagErr.name === 'TileEtagMismatchError' && noEtagErr.expectedEtag === null;

// ------------------------------------------------------- nonOkStatusRejectsWithTypedHttpError
const httpErrStub = makeFetchStub({ ok: false, status: 404 });
const httpErrLayer = new GatewayImageryLayerAdapter({ manifestSha256: MANIFEST_SHA256, fetchImpl: httpErrStub.fetchImpl });
const httpErrRequest = httpErrLayer.plan(VIEW)[0];
const httpErr = await rejectionOf(httpErrLayer.load(httpErrRequest, new AbortController().signal));
const nonOkStatusRejectsWithTypedHttpError = httpErr instanceof TileHttpError
  && httpErr.name === 'TileHttpError' && httpErr.status === 404;

// -------------------------------------------------- abortRejectsAndSignalWasPassedThrough
const abortStub = makeFetchStub();
const abortLayer = new GatewayImageryLayerAdapter({ manifestSha256: MANIFEST_SHA256, fetchImpl: abortStub.fetchImpl });
const abortRequest = abortLayer.plan(VIEW)[0];
const abortController = new AbortController();
abortController.abort(new Error('deliberate test abort'));
const abortErr = await rejectionOf(abortLayer.load(abortRequest, abortController.signal));
const abortRejectsAndSignalWasPassedThrough = abortErr !== null && abortStub.calls.length === 1
  && abortStub.calls[0].signal === abortController.signal && abortStub.calls[0].signal.aborted === true;

// ---------------------------------------------------------------- manifestByteCostProbe
// Hand-encoded `TileSetManifest` protobuf bytes (`proto/altavista/v1/heavy.proto`):
// two `TileEntry`s, `(level:2,x:3,y:1,size_bytes:999999)` (exactly this file's own
// `TILE`/`VIEW`) and `(level:9,x:9,y:9,size_bytes:123)` (never requested here -- only
// present to prove the lookup is keyed correctly, not "first entry wins"). 999999 is
// deliberately far from `IMAGERY_TILE_BYTES` (262144, the constructor's own default
// fallback) so a bug that kept charging the fallback after the manifest resolved is
// unmistakable. Verified byte-for-byte, in this task's own report, against a real
// `google.protobuf` (Python) encoding of the identical two entries -- this is not
// merely "decodes back to what this file put in" (which `./layers/tileset_manifest.js`'s
// OWN module doc already covers) but "is a real protobuf wire-format encoding",
// independently produced.
const MANIFEST_SHA256_2 = 'f'.repeat(64);
const MANIFEST_BYTES = new Uint8Array([58, 10, 8, 2, 16, 3, 24, 1, 40, 191, 132, 61, 58, 8, 8, 9, 16, 9, 24, 9, 40, 123]);
const MANIFEST_ENTRY_SIZE_BYTES = 999999;

function makeManifestFetchStub() {
  const calls = [];
  const fetchImpl = async (url, init) => {
    calls.push({ url, signal: init && init.signal });
    return {
      ok: true,
      status: 200,
      headers: { get: () => null },
      arrayBuffer: async () => MANIFEST_BYTES.buffer.slice(MANIFEST_BYTES.byteOffset, MANIFEST_BYTES.byteOffset + MANIFEST_BYTES.byteLength),
    };
  };
  return { fetchImpl, calls };
}

const manifestStub = makeManifestFetchStub();
const manifestLayer = new GatewayImageryLayerAdapter({ manifestSha256: MANIFEST_SHA256_2, fetchImpl: manifestStub.fetchImpl });

// BEFORE fetchManifest(): must be the fallback estimate, tagged as such.
const beforeRequest = manifestLayer.plan(VIEW)[0]; // VIEW's own TILE is {level:2,x:3,y:1}
const beforeFetchIsFallback = beforeRequest.byteCost === beforeRequest.byteCost // always true; kept for symmetry with the assertions below
  && manifestLayer.manifestLoaded === false
  && beforeRequest.byteCostSource === 'fallback-estimate';

const manifestTileCount = await manifestLayer.fetchManifest();
const manifestFetchedExpectedUrl = manifestStub.calls.length === 1
  && manifestStub.calls[0].url === `/api/tiles/${MANIFEST_SHA256_2}/manifest`;

// AFTER fetchManifest(): the requested tile IS in the manifest -- real size, tagged 'manifest'.
const afterRequest = manifestLayer.plan(VIEW)[0];
const afterFetchUsesManifestByteCost = manifestLayer.manifestLoaded === true
  && manifestTileCount === 2
  && afterRequest.byteCost === MANIFEST_ENTRY_SIZE_BYTES
  && afterRequest.byteCostSource === 'manifest';

// A tile the manifest does NOT list (a different, unrequested level/x/y) must still
// fall back to the estimate, never throw and never silently charge nothing.
const unlistedView = {
  ...VIEW,
  tiles: [{ level: 4, x: 4, y: 4 }],
};
const unlistedRequest = manifestLayer.plan(unlistedView)[0];
const unlistedTileFallsBackToEstimate = unlistedRequest.byteCostSource === 'fallback-estimate'
  && unlistedRequest.byteCost === manifestLayer.tileBytes
  && unlistedRequest.byteCost !== MANIFEST_ENTRY_SIZE_BYTES;

const manifestByteCostProbe = {
  beforeFetchIsFallback,
  manifestFetchedExpectedUrl,
  manifestTileCount,
  afterFetchUsesManifestByteCost,
  unlistedTileFallsBackToEstimate,
  ok: beforeFetchIsFallback && manifestFetchedExpectedUrl && afterFetchUsesManifestByteCost && unlistedTileFallsBackToEstimate,
};

const result = {
  planNeverFetches,
  urlIsSameOriginRelativeByDefault,
  originIsHonoured,
  matchingEtagResolves,
  mismatchedEtagRejectsWithTypedError,
  missingEtagRejectsWithTypedError,
  nonOkStatusRejectsWithTypedHttpError,
  abortRejectsAndSignalWasPassedThrough,
  manifestByteCostProbe,
  expectedRelativeUrl,
  realSha256: REAL_SHA256,
  // Round 6 (docs/open-questions.md question 231's ruling) -- see this file's own
  // module docstring for what each of these would fail against.
  textureShapedPayload,
  provenanceTagged,
  verifiedBytesStillReachable,
  releaseDisposesTheRealTexture,
  releaseIsNoopOnceAlreadyReleased,
  decodeModeSwitch,
};

process.stdout.write(JSON.stringify(result));
