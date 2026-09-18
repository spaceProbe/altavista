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
//     `load()` with a payload carrying that same digest.
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
const matchingEtagResolves = okPayload.sha256 === REAL_SHA256 && okStub.calls.length === 1
  && okStub.calls[0].url === expectedRelativeUrl;

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

const result = {
  planNeverFetches,
  urlIsSameOriginRelativeByDefault,
  originIsHonoured,
  matchingEtagResolves,
  mismatchedEtagRejectsWithTypedError,
  missingEtagRejectsWithTypedError,
  nonOkStatusRejectsWithTypedHttpError,
  abortRejectsAndSignalWasPassedThrough,
  expectedRelativeUrl,
  realSha256: REAL_SHA256,
};

process.stdout.write(JSON.stringify(result));
