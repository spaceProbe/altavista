#!/usr/bin/env node
// H5b-2 deliverable 3 (docs/heavy-plan.md H5, round 3): the headless frame-time/
// memory-budget harness H5's own exit criterion names -- "a headless harness measures
// frame time while a tile set streams from a real av-tiles gateway and asserts no
// frame exceeds the budget and the memory budget is never crossed."
//
// Usage: node web/js/layers_stream_check.mjs <origin> <manifestSha256> <frameBudgetMs> <memoryBudgetBytes>
//
//   <origin>            e.g. http://127.0.0.1:54321 -- the viewer server's own real,
//                       already-running origin (tests/test_viewer_layers_stream.py
//                       starts a real `python -m altavista serve` subprocess and
//                       passes its own bound loopback address here). This is the
//                       ONE place this harness is given an absolute host at all --
//                       GatewayImageryLayerAdapter's own default (`origin: ''`) is
//                       what a real browser page uses; this harness passes one
//                       explicitly only because `node`'s own `fetch` has no implicit
//                       page origin to resolve a relative URL against (see
//                       web/js/layers/gateway_imagery_layer.js's own module
//                       docstring, question 51).
//   <manifestSha256>    the real tile set's manifest hash (`av-tile-fixture`'s own
//                       printed `manifest_sha256`).
//   <frameBudgetMs>     the frame-time budget this run asserts against
//                       (`everyFrameWithinBudget`) -- chosen and justified by the
//                       CALLER (tests/test_viewer_layers_stream.py), not by this
//                       file; see that test's own module docstring for the number
//                       and the measured margin.
//   <memoryBudgetBytes> the resident byte budget `LayerManager` is built with.
//
// Prints ONE JSON object to stdout and exits 0 regardless of whether the budgets
// held (the CALLER asserts on the JSON -- an exit code is not evidence, question
// 148) -- unless setup itself fails (a malformed argument, or the manifest fetch
// itself throwing), in which case this prints one JSON `{ error: "..." }` object and
// exits 1, so a caller can tell "the run happened and the numbers are real" apart
// from "this harness itself never got to run at all".
//
// # What a "frame" is, and why loads are NOT measured inside it (read this before
// trusting `maxFrameMs`/`p95FrameMs` at all)
//
// A frame here is EXACTLY: one `manager.update(view)` call (synchronous: priority
// sort, cancellation, starting new loads, eviction -- see web/js/layers/layer.js)
// PLUS committing at most `MAX_COMMITS_PER_FRAME` already-completed loads
// (`commitOne`, below), where "committing" a tile does real, non-trivial synchronous
// per-tile work: recomputing its SHA-256 over the bytes already in hand (Node's own
// `node:crypto` `createHash('sha256')`, which -- unlike the browser's
// `crypto.subtle.digest` `GatewayImageryLayerAdapter.load()` already used, off-frame,
// to verify the gateway's own ETag during the fetch itself -- is genuinely
// synchronous, exactly modelling a second, synchronous integrity check a real
// viewer's main thread might run just before handing bytes to the GPU) and parsing
// the tile's own PNG header (`pngDimensions`, below: the 8-byte PNG signature plus
// the fixed-offset `IHDR` chunk's width/height fields -- every valid PNG's first
// chunk, by the format's own spec) to get its pixel dimensions, exactly the kind of
// cheap-but-real decode-adjacent work a texture upload path actually does before
// calling into the GPU. Both of those together are what stop `maxFrameMs` from ever
// being trivially zero (a frame that did nothing would report ~0ms and prove
// nothing).
//
// What is explicitly, deliberately NOT inside a frame's timed body: the real
// `fetch()` call `GatewayImageryLayerAdapter.load()` makes, and everything that
// happens while it is in flight (the httpx-shaped proxy hop through
// `altavista/server.py`, the real network round trip to the real `av-tiles`
// gateway, the real MinIO read behind it). `LayerManager.update()` only ever
// *starts* a load and returns synchronously (see layer.js's own module docstring:
// "Never awaits a `load()` promise") -- exactly why this is a fair model of a real
// browser's main thread, where `fetch()` genuinely does hand control back to the
// event loop immediately and the response arrives on a LATER task, not inside the
// call that started it. This harness's own frame loop (below) never `await`s a
// `load()` promise directly either; it only ever awaits a bare event-loop-tick yield
// between frames (`setImmediate`, not a timer *duration* -- the same "no sleep,
// ever" discipline every other harness in this codebase already follows, see
// web/js/layers_check.mjs's own module docstring on `flushMicrotasks`) so that
// whatever real I/O has already completed by wall-clock time gets to run its own
// `.then()` callback (`LayerManager._onLoaded`) between two frames, exactly like a
// real browser's event loop runs a resolved fetch's callback between two animation
// frames. A reader who takes `maxFrameMs` as "how long a tile took to load over the
// real network" has misread this file -- it measures how long the SYNCHRONOUS main-
// thread work per frame took, which is the number that actually determines whether a
// real browser drops a frame; the network time is real, it is just not what this
// number is measuring, on purpose, because it is not what a dropped frame is caused
// by in a `fetch`-based loader.
//
// # No sleeping, anywhere (this task's own binding rule)
//
// The frame loop below runs until the camera path is exhausted AND every started
// load has either settled (committed or failed/cancelled) or been given up on at a
// hard `MAX_FRAMES` safety cap -- a real, polled condition (`manager.pending.size`,
// the commit queue's own length), never a fixed sleep-then-assume. `settledBeforeMaxFrames`
// in the printed JSON says which one actually happened; the caller (tests/
// test_viewer_layers_stream.py) asserts it is `true`, so a run that silently hit the
// safety cap instead of genuinely draining is itself a visible, named failure, not a
// quietly-truncated report.
import { createHash } from 'node:crypto';
import { performance } from 'node:perf_hooks';
import { LayerManager } from './layers/layer.js';
import { GatewayImageryLayerAdapter, TileHttpError, TileEtagMismatchError } from './layers/gateway_imagery_layer.js';
import { selectTiles, geodeticToEcef, SCENE_UNITS_PER_METRE } from './globe_lod.js';
// Round 6 (docs/open-questions.md question 231's ruling, "replace or composite" --
// docs/heavy-plan.md's round-5 status, "the one thing round 5 does NOT deliver: a
// selected tile set is streamed, not drawn"). This task's own precedent for building
// a real GlobeLayer under node is web/js/globe_imagery_check.mjs (see that file's own
// module docstring) -- this harness follows the identical shape, just against the
// real gateway this file already stands up rather than a network-free stub for every
// layer.
import { GlobeLayer } from './globe.js';

// Round 6: "console-clean: zero errors/warnings from the harness" (this task's own
// required proof 5) -- tracked for the WHOLE run, not just the new GlobeLayer section
// below, since a regression anywhere in this file should be caught. `console.warn`/
// `console.error` are wrapped (nothing in this file calls either today -- a
// call appearing at all is itself the signal); `unhandledRejection` catches a
// promise this file (or anything it drives) failed to attach a `.catch` to, exactly
// the class of bug a "zero page exceptions" browser gate (tests/
// test_viewer_globe_layer_manager.py) would catch for a page -- this is that same
// discipline applied to a node CLI harness instead of a page.
const consoleWarnings = [];
const originalConsoleWarn = console.warn.bind(console);
const originalConsoleError = console.error.bind(console);
console.warn = (...args) => { consoleWarnings.push(`warn: ${args.map(String).join(' ')}`); originalConsoleWarn(...args); };
console.error = (...args) => { consoleWarnings.push(`error: ${args.map(String).join(' ')}`); originalConsoleError(...args); };
const unhandledRejections = [];
process.on('unhandledRejection', (reason) => {
  unhandledRejections.push(String((reason && reason.stack) || reason));
});

// ------------------------------------------------------------------------ arguments
const [
  origin, manifestSha256, frameBudgetMsRaw, memoryBudgetBytesRaw, maxLevelRaw, tileBytesRaw,
  maxConcurrentLoadsRaw, dwellRoundTripsRaw,
  // Round 6 (question 231's ruling): OPTIONAL, a SECOND real tile set's manifest
  // hash -- a shallower one (fewer levels) than `manifestSha256`'s own, deliberately,
  // so the GlobeLayer probe (below) can construct "a tile the later set does not
  // cover" for real, against the real gateway, rather than assume it. Omitted, the
  // two-set/composition half of that probe is skipped and says so explicitly
  // (`twoSetProbe.skipped`) -- this file stays runnable standalone with just one
  // manifest, exactly as it always has.
  manifestSha256BRaw,
] = process.argv.slice(2);

function fail(message) {
  process.stdout.write(JSON.stringify({ error: message }));
  process.exit(1);
}

if (!origin || !manifestSha256 || !frameBudgetMsRaw || !memoryBudgetBytesRaw) {
  fail(
    'usage: node web/js/layers_stream_check.mjs <origin> <manifestSha256> <frameBudgetMs> <memoryBudgetBytes> '
    + '[maxLevel] [tileBytes] [maxConcurrentLoads] [dwellRoundTrips] [manifestSha256B]',
  );
}
const manifestSha256B = manifestSha256BRaw === undefined ? null : manifestSha256BRaw;
if (manifestSha256B !== null && !/^[0-9a-f]{64}$/.test(manifestSha256B)) {
  fail(`manifestSha256B must be 64 lowercase hex characters, got ${JSON.stringify(manifestSha256BRaw)}`);
}
const frameBudgetMs = Number(frameBudgetMsRaw);
const memoryBudgetBytes = Number(memoryBudgetBytesRaw);
// `maxLevel` and `tileBytes` (round 3, the scale proof) are optional and default to
// exactly what this harness used before they existed, so `tests/
// test_viewer_layers_stream.py` is unaffected byte for byte. They exist because the
// tile set a caller points this harness at is not obliged to be the small fixture:
// the ten-gigabyte proof's own set runs to level 6 with 1024-pixel RGB8 tiles of
// 3147060 bytes each, twelve times `IMAGERY_TILE_BYTES`. A memory budget accounted in
// the wrong units is not a memory budget, and a `maxLevel` that stops at 2 would
// stream the coarse corner of a set and call it the set -- so both are declared by
// the caller that knows the tile set, and `tileBytes` is then CHECKED against the
// real length of every tile the gateway returns (`byteCostMismatchCount` below).
const maxLevel = maxLevelRaw === undefined ? 2 : Number(maxLevelRaw);
const declaredTileBytes = tileBytesRaw === undefined ? undefined : Number(tileBytesRaw);
if (!(Number.isInteger(maxLevel) && maxLevel >= 0)) fail(`maxLevel must be a non-negative integer, got ${JSON.stringify(maxLevelRaw)}`);
if (declaredTileBytes !== undefined && !(Number.isFinite(declaredTileBytes) && declaredTileBytes > 0)) fail(`tileBytes must be a positive number, got ${JSON.stringify(tileBytesRaw)}`);
// Also optional, also defaulting to exactly what this file used before they existed
// (see `STREAM_MAX_CONCURRENT_LOADS` and `DWELL_ROUND_TRIPS_PER_POSITION` below for
// why those two numbers are what they are for the SMALL fixture). A caller pointing
// this harness at a large tile set needs both: a 3147060-byte tile takes far longer
// to cross the wire than the fixture's 1 KB one, so at two concurrent slots and one
// round trip of dwell only a handful of tiles ever complete, and "streamed at scale"
// would be five tiles wearing a large tile set's name.
const cliMaxConcurrentLoads = maxConcurrentLoadsRaw === undefined ? undefined : Number(maxConcurrentLoadsRaw);
const cliDwellRoundTrips = dwellRoundTripsRaw === undefined ? undefined : Number(dwellRoundTripsRaw);
if (cliMaxConcurrentLoads !== undefined && !(Number.isInteger(cliMaxConcurrentLoads) && cliMaxConcurrentLoads > 0)) fail(`maxConcurrentLoads must be a positive integer, got ${JSON.stringify(maxConcurrentLoadsRaw)}`);
if (cliDwellRoundTrips !== undefined && !(Number.isFinite(cliDwellRoundTrips) && cliDwellRoundTrips > 0)) fail(`dwellRoundTrips must be a positive number, got ${JSON.stringify(dwellRoundTripsRaw)}`);
if (!(Number.isFinite(frameBudgetMs) && frameBudgetMs > 0)) fail(`frameBudgetMs must be a positive number, got ${JSON.stringify(frameBudgetMsRaw)}`);
if (!(Number.isFinite(memoryBudgetBytes) && memoryBudgetBytes > 0)) fail(`memoryBudgetBytes must be a positive number, got ${memoryBudgetBytesRaw}`);
if (!/^[0-9a-f]{64}$/.test(manifestSha256)) fail(`manifestSha256 must be 64 lowercase hex characters, got ${JSON.stringify(manifestSha256)}`);

// -------------------------------------------------------------------- fixed camera
// The real tile set (tests/heavy_stack.py's `tile_set` fixture) is built with
// `--min-level 0 --max-level 2` over a whole-globe synthetic source (confirmed
// against crates/av-jobs/src/scheme.rs: a synthetic source's bounds are always the
// full [-180,180]x[-90,90], so every (level,x,y) in that range at levels 0-2
// genuinely exists in the store -- see this task's own investigation into
// crates/av-jobs/src/scheme.rs's `tiles_covering`). `maxLevel: 2` below is what
// keeps `selectTiles()` from ever choosing a level-3+ address this fixture never
// generated (which would 404, a setup bug this harness would rather fail loudly on
// than silently tolerate as a "real" HTTP status).
const SCREEN = { screenHeightPx: 900, fovYRad: (50 * Math.PI) / 180, maxLevel, maxTiles: 64 };

function cameraEcef(lonDeg, latDeg, heightM) {
  return geodeticToEcef(lonDeg, latDeg, heightM);
}

// A deliberate large jump (like web/js/layers_check.mjs's own CAMERA_PATH) so a
// real cancellation is exercised against real in-flight `fetch()` calls, not a
// stub -- 'near-0-0' -> 'near-0-0-close' builds up real pending loads over a few
// tight frames, then 'jump-antipodal' moves the camera to the opposite side of the
// globe before those loads can all realistically have completed, which drops those
// requests out of the new plan and must cancel them (`LayerManager.update()`'s own
// cancellation, see layer.js).
const CAMERA_PATH = [
  { label: 'far', lonDeg: 0, latDeg: 0, heightM: 20_000_000 },
  { label: 'near-0-0', lonDeg: 0, latDeg: 0, heightM: 2_000_000 },
  { label: 'near-0-0-close', lonDeg: 2, latDeg: 2, heightM: 400_000 },
  { label: 'jump-antipodal', lonDeg: 179, latDeg: -10, heightM: 400_000 },
  { label: 'jump-antipodal-2', lonDeg: 175, latDeg: -8, heightM: 400_000 },
  { label: 'far-2', lonDeg: 0, latDeg: 0, heightM: 20_000_000 },
];
// How long this harness dwells at each camera position before jumping to the next
// one is NOT a hardcoded tick count -- a fixed number of `setImmediate` yields is
// not a fixed amount of real wall-clock time (Node's own event-loop tick cost, and
// therefore how much real network I/O gets a chance to complete between two frames,
// varies with host load, which this task's own binding rules say to expect:
// "this host is over-subscribed and shared with another track"). Instead, this
// harness measures ONE real round trip against the real gateway before the camera
// path even starts (`calibrationRoundTripMs`, below) and dwells at each position for
// a real, polled multiple of THAT measurement -- see the camera-path loop, below.
// This is still "no sleeping, ever" in the sense this task's binding rule means it:
// there is no fixed timer duration anywhere in this file: the wait is bounded by a
// REAL measured condition (elapsed `performance.now()` since the position started,
// checked between real `setImmediate` yields), not an arbitrarily guessed constant.
// Deliberately SMALL (not generous): dwelling long enough to fully drain every
// request a busy camera position makes (this fixture's close-up positions select up
// to 20 tiles, far more than `STREAM_MAX_CONCURRENT_LOADS`'s own 2 concurrent slots,
// below, so fully draining one takes several concurrency "waves") would mean
// nothing is ever left pending by the time the deliberate camera jump happens,
// which would make `cancelledCount` structurally unable to exercise real
// cancellation against real in-flight `fetch()` calls -- the exact thing this
// harness's jump exists to prove (confirmed empirically against a local smoke-test
// gateway: a more generous multiple, e.g. 8, let every position fully settle before
// its own dwell window ran out, and `cancelledCount` was 0 on every run -- only
// once both this constant AND `STREAM_MAX_CONCURRENT_LOADS` were tightened did a
// real backlog reliably survive into the jump). 1 round trip's worth of dwell is
// enough for a LOW-demand position (e.g. the 'far' root-tile view, 2 tiles) to
// settle well inside its own window, while leaving a genuine backlog at a
// HIGH-demand position for the next jump to cancel.
const DWELL_ROUND_TRIPS_PER_POSITION = cliDwellRoundTrips === undefined ? 1 : cliDwellRoundTrips;
const MIN_DWELL_MS = 25; // floor, in case the calibration round trip was implausibly fast (e.g. warm keep-alive)
const MAX_DWELL_MS = 3000; // ceiling, in case the calibration round trip was implausibly slow
// Hard safety cap on total frames (module docstring: "a real, polled condition,
// never a fixed sleep-then-assume") -- large enough that a healthy real gateway on
// this host never comes close to it, small enough that a genuinely hung gateway does
// not make this harness hang forever either.
// The global safety cap. It must be large enough that the per-position wall-clock
// dwell, not this number, is what ends each position -- otherwise the first position
// eats the whole budget and the camera never jumps, which is exactly what a run at
// `dwellRoundTrips` 40 did once `PER_POSITION_MAX_FRAMES` stopped being the binding
// bound (measured: 60000 frames, all of them at position one, cancelledCount 0).
// This loop yields with a bare `setImmediate`, which costs about 0.01 ms here, so it
// runs on the order of 60000 frames a second: budget accordingly. Scaled from the
// dwell rather than fixed, with the old 60000 as a floor so a default run is exactly
// what it was.
const MAX_FRAMES = Math.max(60_000, Math.ceil(DWELL_ROUND_TRIPS_PER_POSITION * CAMERA_PATH.length * 4000));

// Per-frame commit cap (module docstring, "What a frame is") -- deliberately small
// (well below this fixture's own total distinct-tile count, 42) so that a burst of
// loads completing around the same wall-clock moment cannot all be committed in one
// single frame, which is the realistic behaviour this cap exists to model (a real
// texture-upload budget per rendered frame, not "commit everything the instant it
// arrives").
const MAX_COMMITS_PER_FRAME = 4;

// ------------------------------------------------------------------ PNG header read
/** Reads a PNG's 8-byte signature and its first chunk (always `IHDR`, by the PNG
 * spec) to recover `{width, height}` -- real, synchronous, per-tile "frame" work
 * (see this file's own module docstring). Throws a typed, named error on anything
 * that is not a well-formed PNG signature+IHDR, so a fixture that ever served
 * something else would fail loudly here rather than silently report a zero-cost
 * commit. */
class NotAPngError extends Error {
  constructor(detail) {
    super(`layers_stream_check: not a well-formed PNG (${detail})`);
    this.name = 'NotAPngError';
  }
}
const PNG_SIGNATURE = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
function pngDimensions(bytes) {
  const buf = Buffer.from(bytes);
  if (buf.length < 8 + 8 + 8 || !buf.subarray(0, 8).equals(PNG_SIGNATURE)) {
    throw new NotAPngError(`got ${buf.length} bytes, signature ${buf.subarray(0, 8).toString('hex')}`);
  }
  const chunkType = buf.toString('ascii', 12, 16);
  if (chunkType !== 'IHDR') throw new NotAPngError(`first chunk was ${JSON.stringify(chunkType)}, not IHDR`);
  return { width: buf.readUInt32BE(16), height: buf.readUInt32BE(20) };
}

// ---------------------------------------------------------------------- the layer
const httpStatusCounts = {};
let tilesFetched = 0;
let bytesFetched = 0;
let etagVerifiedCount = 0;
let etagMismatchCount = 0;
// The memory budget is accounted against each request's DECLARED `byteCost`, which is
// necessarily an estimate: `plan()` runs before the fetch, so nothing knows a tile's
// real length yet. That makes the declaration worth checking rather than trusting --
// every tile this harness actually receives has its real `byteLength` compared to the
// `byteCost` the budget was charged, and a mismatch is counted. A budget assertion
// that holds only because the estimate was small is not a budget assertion.
let byteCostCheckedCount = 0;
let byteCostMismatchCount = 0;
let maxByteCostErrorBytes = 0;
// Round 4 (question 228's own round-3-defect-5 follow-up): once `fetchManifest()`
// (below) resolves, `GatewayImageryLayerAdapter.plan()` charges each request the
// manifest's own real per-tile `size_bytes` instead of this harness's single
// DECLARED `tileBytes` estimate -- tagged `byteCostSource` on every request (see
// gateway_imagery_layer.js's own `plan()`). Tallied here, per distinct load attempt,
// so a run can never silently be accounted in estimated units without that being
// visible in the printed JSON: a real run against a real, fully-populated manifest is
// expected to show every entry under `'manifest'`, never `'fallback-estimate'`.
const byteCostSourceCounts = {};
// Round 6 (docs/open-questions.md question 231's ruling; manager review of this
// task's own report): tallied on EVERY successful real load, per
// `payload.userData.decodeMode` (`web/js/layers/gateway_imagery_layer.js`'s own
// `decodeTileBytesToTexture`) -- so a reader never has to take "the bound texture's
// provenance is the selected set's own payload" as proof that the set's own REAL
// PIXELS reached the screen. Stated plainly, not left implicit: this harness runs
// under NODE, which has no `createImageBitmap` at all (measured directly, see
// gateway_imagery_layer.js's own module docstring) -- so EVERY entry here is
// structurally expected to be `'placeholder-no-createImageBitmap'`, regardless of
// how real the bytes crossing the wire are (and they ARE real: `av-tile-fixture`'s
// own tiler, `crates/av-jobs/src/tiler.rs`, encodes genuine PNGs). The one place a
// real `createImageBitmap` decode of a real PNG is actually proved is a real
// browser: `tests/test_viewer_globe_layer_manager.py`'s own
// `test_decode_tile_bytes_to_texture_really_decodes_a_real_png_in_a_real_browser`.
const decodeModeCounts = {};

const layer = new GatewayImageryLayerAdapter({ manifestSha256, origin, tileBytes: declaredTileBytes });
const realLoad = layer.load.bind(layer);
layer.load = async (request, signal) => {
  byteCostSourceCounts[request.byteCostSource] = (byteCostSourceCounts[request.byteCostSource] || 0) + 1;
  try {
    // Round 6 (question 231's ruling): `realLoad`'s own payload is now a real
    // `THREE.Texture`-shaped object, not the pre-round-6 `{bytes, sha256, ...}` --
    // the verified wire bytes/digest this section has always measured are still
    // there, just relocated to `payload.userData.bytes`/`.sha256` (see
    // web/js/layers/gateway_imagery_layer.js's own module docstring, "Problem 1").
    const payload = await realLoad(request, signal);
    decodeModeCounts[payload.userData.decodeMode] = (decodeModeCounts[payload.userData.decodeMode] || 0) + 1;
    tilesFetched += 1;
    bytesFetched += payload.userData.bytes.byteLength;
    byteCostCheckedCount += 1;
    const err = Math.abs(payload.userData.bytes.byteLength - request.byteCost);
    if (err > maxByteCostErrorBytes) maxByteCostErrorBytes = err;
    if (err !== 0) byteCostMismatchCount += 1;
    etagVerifiedCount += 1;
    httpStatusCounts['200'] = (httpStatusCounts['200'] || 0) + 1;
    return payload;
  } catch (err) {
    if (err instanceof TileEtagMismatchError) {
      tilesFetched += 1;
      etagMismatchCount += 1;
      httpStatusCounts['200'] = (httpStatusCounts['200'] || 0) + 1; // the HTTP call itself succeeded; only verification failed
    } else if (err instanceof TileHttpError) {
      httpStatusCounts[String(err.status)] = (httpStatusCounts[String(err.status)] || 0) + 1;
    }
    // AbortError (cancellation) and any other rejection are re-thrown unchanged --
    // LayerManager's own _onFailed handles bookkeeping for those; this wrapper only
    // ever adds counters, never changes control flow.
    throw err;
  }
};

// maxConcurrentLoads: a deliberately small STREAM_MAX_CONCURRENT_LOADS (below
// LayerManager's own realistic default of 6 -- see layer.js's constructor doc),
// chosen for this harness specifically (not a statement about what a real viewer
// should use) so real cancellation AND real eviction are both exercised robustly
// regardless of how fast this host's real gateway round trip happens to be on any
// given run: throttling concurrency this far below this fixture's own per-position
// tile count (up to 20) guarantees a real backlog is still in flight when the
// deliberate camera jump happens, without this harness having to precisely
// time-match its own dwell window against an unknown, host-load-dependent real
// request latency.
//
// Corrective round 3 re-tried the class's own real default (6) here directly, now
// that Finding 1's failure-memory fix is in layer.js (the starvation defect this
// cap's small value was never modelling in the first place -- unlike the terrain
// zombie livelock web/js/layers_check.mjs's own MAIN_MAX_CONCURRENT_LOADS
// documents, which really was Finding 1's defect and really did go away). Measured
// directly against THIS real stack: four consecutive `tests/
// test_viewer_layers_stream.py` runs with maxConcurrentLoads=6 -- `cancelledCount`
// stayed reliable every time (12, 12, 12, 12), but `evictedCount` did NOT (5, 0, 2,
// 0 -- two of the four runs never evicted a single resident tile at all, failing
// `test_eviction_and_cancellation_both_actually_happened` outright). Root cause: at
// higher concurrency, real fetches against the real gateway settle fast enough,
// relative to this harness's own calibration-based dwell window (see
// DWELL_ROUND_TRIPS_PER_POSITION's own comment), that how many DISTINCT tiles ever
// become resident before the run ends varies with host-load-dependent luck -- some
// runs fetch enough to cross MEMORY_BUDGET_BYTES and evict, some don't. 2 is kept
// (not 6) because it restores the same throttling this constant's value already
// existed to provide: concurrency itself becomes the bottleneck instead of real
// network timing, so a real backlog reliably survives long enough for both eviction
// and cancellation to be exercised on every run, not merely most of them (empirically
// re-confirmed with STREAM_MAX_CONCURRENT_LOADS reverted to 2: five consecutive runs,
// see this task's own report for the five evictedCount/cancelledCount/maxFrameMs
// values).
const STREAM_MAX_CONCURRENT_LOADS = cliMaxConcurrentLoads === undefined ? 2 : cliMaxConcurrentLoads;
const manager = new LayerManager({ memoryBudgetBytes, maxConcurrentLoads: STREAM_MAX_CONCURRENT_LOADS }); // now() defaults to performance.now() -- see layer.js
manager.addLayer(layer);

// A tile becomes "committable" the frame AFTER it is observed resident but has not
// yet been committed by THIS harness -- see the frame loop, below, for exactly how
// this queue is filled and drained.
const commitQueue = [];
const committedKeys = new Set();
let commitCount = 0;

function commitOne(globalKey) {
  const payload = manager.getResidentPayload(globalKey);
  if (payload === undefined) return; // evicted before its own turn to commit -- nothing to do
  // Round 6: `payload.bytes`/`.sha256` -> `payload.userData.bytes`/`.sha256` -- see
  // the `layer.load` wrapper's own comment, above, for why.
  const recomputed = createHash('sha256').update(Buffer.from(payload.userData.bytes)).digest('hex');
  if (recomputed !== payload.userData.sha256) {
    // payload.userData.sha256 was already verified against the gateway's own ETag
    // inside load() (crypto.subtle, off-frame); this is a SECOND, synchronous check
    // over the SAME bytes this harness already has in hand -- see module docstring.
    // Disagreement here would mean the bytes changed after load() returned, which
    // never legitimately happens; fail loudly rather than silently commit garbage.
    throw new Error(`layers_stream_check: commit-time SHA-256 ${recomputed} disagrees with load-time ${payload.userData.sha256} for ${globalKey}`);
  }
  pngDimensions(payload.userData.bytes); // real, synchronous per-tile work -- see module docstring
  commitCount += 1;
}

// ------------------------------------------------------------------- the frame loop
const frameMsList = [];
let maxResidentBytesObserved = 0;
let softViolationTaken = false;
const knownResidentKeys = new Set();

function runOneFrame(view) {
  const t0 = performance.now();

  manager.update(view); // synchronous: priority sort, cancellation, new loads started, eviction

  // Detect newly-resident items since the last frame and enqueue them for commit
  // (oldest-observed-first) -- LayerManager's own bookkeeping already made them
  // resident (via _onLoaded, whenever its promise settled, off-frame); this queue
  // is this harness's OWN additional per-frame throttling on top of that, see the
  // module docstring's "What a frame is".
  for (const globalKey of manager.resident.keys()) {
    if (!knownResidentKeys.has(globalKey)) {
      knownResidentKeys.add(globalKey);
      commitQueue.push(globalKey);
    }
  }
  let committedThisFrame = 0;
  while (committedThisFrame < MAX_COMMITS_PER_FRAME && commitQueue.length > 0) {
    const globalKey = commitQueue.shift();
    if (committedKeys.has(globalKey)) continue; // already committed by an earlier frame
    commitOne(globalKey);
    committedKeys.add(globalKey);
    committedThisFrame += 1;
  }

  if (manager.softViolationCount > 0) softViolationTaken = true;
  maxResidentBytesObserved = Math.max(maxResidentBytesObserved, manager.residentBytes);

  const frameMs = performance.now() - t0;
  frameMsList.push(frameMs);
}

// ------------------------------------------------------------- calibration round trip
// ONE real fetch of a real, known-to-exist tile (level 0, x 0, y 0 -- always present:
// every level's own full grid exists in this fixture, see module docstring), made
// directly (bypassing `layer.load`'s own counting wrapper -- this probe's own outcome
// must not pollute `tilesFetched`/`etagVerifiedCount`/httpStatusCounts, it is
// infrastructure for THIS harness, not part of what it is measuring) so this run's
// own per-position dwell time (below) is sized against a REAL measurement of this
// host's actual gateway round-trip latency right now, not a guess.
const calibrationController = new AbortController();
const calibrationT0 = performance.now();
let calibrationRoundTripMs;
try {
  await realLoad({ tile: { level: 0, x: 0, y: 0 }, url: `${origin}/api/tiles/${manifestSha256}/tiles/0/0/0` }, calibrationController.signal);
  calibrationRoundTripMs = performance.now() - calibrationT0;
} catch (err) {
  fail(`calibration fetch of tile 0/0/0 failed -- cannot size this run's per-position dwell time without a real round-trip measurement: ${err && err.message}`);
}
const dwellMsPerPosition = Math.min(
  MAX_DWELL_MS,
  Math.max(MIN_DWELL_MS, calibrationRoundTripMs * DWELL_ROUND_TRIPS_PER_POSITION),
);

// ------------------------------------------------------------------- manifest fetch
// Round 4 (question 228): ONE real fetch of this tile set's own manifest, over the
// SAME same-origin proxy route the tile fetches themselves use (question 51 -- never
// a second origin), before the camera path starts -- exactly the same "fetch once,
// up front" shape as the calibration round trip above. Deliberately NOT fatal to this
// run if it fails (unlike the calibration fetch, which this harness cannot proceed
// without at all): a manifest fetch failure only means every subsequent request falls
// back to the declared `tileBytes` estimate (see gateway_imagery_layer.js's own
// `plan()`), which this harness can still measure and report honestly --
// `manifestFetchError` (below) makes that failure visible rather than silently
// swallowed, and `byteCostSourceCounts` (see the `layer.load` wrapper above) makes
// the CONSEQUENCE of it (every request accounted in estimated, not manifest, units)
// impossible to miss in the printed JSON either way.
let manifestFetchError = null;
try {
  await layer.fetchManifest();
} catch (err) {
  manifestFetchError = (err && err.message) || String(err);
}

// A real, idle host can run many hundreds of bare `setImmediate` round trips inside
// even a modest `dwellMsPerPosition` window -- `PER_POSITION_MAX_FRAMES` is a second,
// independent stopping condition (a tick-count ceiling) purely so a position with
// nothing left to do cannot burn an unbounded slice of the global `MAX_FRAMES`
// budget spinning through its own idle dwell window; it is intentionally large
// enough to never itself be the reason a HIGH-demand position stops early (the real
// wall-clock `dwellMsPerPosition` bound below is what does that job, deliberately,
// so a real backlog survives into the next position's camera jump -- see
// `DWELL_ROUND_TRIPS_PER_POSITION`'s own comment for why that dwell is kept short
// rather than generous).
// Measured, round 3: 2000 was NOT large enough to leave the wall-clock bound in
// charge, and the comment above was wrong about it. A bare `setImmediate` round trip
// on this host costs about 0.01 ms, so 2000 ticks is roughly 20 ms of wall clock --
// shorter than a single real tile round trip (measured through the viewer-server
// proxy against a real gateway: about 40 ms for a 3147060-byte tile). Every position
// therefore ended on the tick ceiling before `dwellMsPerPosition` had any effect at
// all, and a run against a large tile set streamed three to five tiles no matter what
// dwell it was given. 200000 ticks is about two seconds of pure spinning, so the
// wall-clock bound below is now genuinely the binding one and this constant is the
// safety cap it was always documented to be.
const PER_POSITION_MAX_FRAMES = 200_000;

let frame = 0;
for (const cam of CAMERA_PATH) {
  const view = {
    cameraEcef: cameraEcef(cam.lonDeg, cam.latDeg, cam.heightM),
    screenHeightPx: SCREEN.screenHeightPx,
    fovYRad: SCREEN.fovYRad,
  };
  view.tiles = selectTiles(view.cameraEcef, { ...SCREEN });

  const positionStart = performance.now();
  let positionFrames = 0;
  while (
    frame < MAX_FRAMES
    && positionFrames < PER_POSITION_MAX_FRAMES
    && (performance.now() - positionStart) < dwellMsPerPosition
  ) {
    runOneFrame(view);
    frame += 1;
    positionFrames += 1;
    // Yield one real event-loop tick -- NOT a sleep (no timer duration at all): see
    // this file's own module docstring, "No sleeping, anywhere". This is what lets
    // whatever real `fetch()` I/O has already completed by wall-clock time run its
    // own `.then()` callback before the next frame reads `manager.resident`. The
    // WHILE condition above bounds how many of these happen at each position by two
    // real, measured quantities -- elapsed wall time (`performance.now()`) and a
    // tick-count safety ceiling -- never a fixed sleep.
    await new Promise((resolve) => setImmediate(resolve));
  }
}

// Drain: keep running frames (same real event-loop-tick yield, no sleep) until every
// started load has settled (pending is empty) and every resident item this harness
// has ever seen has been committed, or MAX_FRAMES is reached -- a real, polled
// condition (module docstring).
let settledBeforeMaxFrames = false;
while (frame < MAX_FRAMES) {
  const stillSettling = manager.pending.size > 0 || commitQueue.length > 0;
  if (!stillSettling) { settledBeforeMaxFrames = true; break; }
  runOneFrame(CAMERA_PATH_LAST_VIEW());
  frame += 1;
  await new Promise((resolve) => setImmediate(resolve));
}
if (frame >= MAX_FRAMES && !settledBeforeMaxFrames) {
  settledBeforeMaxFrames = manager.pending.size === 0 && commitQueue.length === 0;
}

function CAMERA_PATH_LAST_VIEW() {
  const cam = CAMERA_PATH[CAMERA_PATH.length - 1];
  const camEcef = cameraEcef(cam.lonDeg, cam.latDeg, cam.heightM);
  return {
    cameraEcef: camEcef,
    screenHeightPx: SCREEN.screenHeightPx,
    fovYRad: SCREEN.fovYRad,
    tiles: selectTiles(camEcef, { ...SCREEN }),
  };
}

// ============================================================================
// Round 6 (docs/open-questions.md question 231's ruling, "replace or composite" --
// docs/heavy-plan.md's round-5 status, "the one thing round 5 does NOT deliver: a
// selected tile set is streamed, not drawn"). A SEPARATE `LayerManager`/`GlobeLayer`
// pair from the one above (`manager`/`layer`) -- this probe's own admission/eviction
// bookkeeping must never perturb the frame-time/memory numbers `result` (below)
// already reports, which this task's own brief says to leave exactly as they were.
//
// Measures, against the SAME real gateway/manifest this whole file already proves
// real bytes cross a real loopback socket for:
//   1. a catalogued set toggled ON binds a globe mesh's texture to THAT set's own
//      payload (provenance tagged at its source, gateway_imagery_layer.js's own
//      `load()` -- see that file's module docstring), not the default's;
//   2. toggling it OFF restores the default's texture on those same meshes;
//   3. (only when `manifestSha256B` was given) with TWO real sets on, the LATER one
//      wins PER TILE, and a tile the later one does not cover falls back to the
//      earlier one -- constructed for real by giving the second manifest a
//      SHALLOWER real tile pyramid than the first (fewer levels), never assumed;
//   4. the globe's own meshes never show `material.map === null` once the default
//      has first loaded -- checked on EVERY tick this whole probe ever runs, not
//      just before/after.
// ============================================================================

// A close-in camera -- the SAME position this file's own CAMERA_PATH already uses
// for 'near-0-0-close' (never a second, independently-chosen position) -- deliberately
// picked there for a reason that also serves this probe: `selectTiles()` at this
// position returns a REAL MIX of levels (finer near the camera, coarser at the
// edges of the current view), so at least one selected tile sits at the tile set's
// own deepest level (2, covered by `manifestSha256` but NOT by the deliberately
// shallower `manifestSha256B`) and at least one sits at a shallower level (0 or 1,
// covered by BOTH) -- exactly the two cases requirement 3 needs, in ONE camera
// position, never a contrived selection.
const GLOBE_PROBE_CAMERA_ECEF_M = cameraEcef(2, 2, 400_000);
const GLOBE_PROBE_CAMERA_LOCAL = {
  x: GLOBE_PROBE_CAMERA_ECEF_M.x * SCENE_UNITS_PER_METRE,
  y: GLOBE_PROBE_CAMERA_ECEF_M.y * SCENE_UNITS_PER_METRE,
  z: GLOBE_PROBE_CAMERA_ECEF_M.z * SCENE_UNITS_PER_METRE,
};

/** THREE.TextureLoader-shaped stub for the globe's own DEFAULT imagery -- this
 * probe is about the GATEWAY-backed sets (proof 1/2/3 above), not about proving a
 * real XYZ tile server exists (there is none under node -- see web/js/
 * globe_imagery_check.mjs's own identical reasoning for GlobeLayer's default
 * imagery loader). Resolves SYNCHRONOUSLY, immediately, with a plain, distinctly-
 * tagged payload -- `ImageryLayerAdapter.load()` (unmodified by this task except
 * for its own provenance tagging, see that file's module docstring) wraps this in a
 * real Promise and tags `.userData.sourceLayerId = 'imagery'` itself; nothing here
 * needs to do that tagging a second time. */
function makeDefaultImageryLoaderStub() {
  let invocationCount = 0;
  return {
    invocationCount: () => invocationCount,
    load(url, onLoad) {
      invocationCount += 1;
      onLoad({ kind: 'default-imagery-probe-stub', url });
    },
  };
}

/** Runs real `globe.update()` ticks (yielding one real event-loop tick between each,
 * same "no sleeping, ever" discipline as this whole file) until `probeManager.
 * pending.size === 0` (every admitted load has settled -- resolved, rejected, or
 * cancelled) or `maxTicks` is hit. `onTick(globe)` is called after EVERY tick
 * (including the very first, before anything may have settled at all) -- this is
 * what lets the caller check requirement 4 ("never material.map === null once the
 * default has loaded") across the WHOLE run, not just at the end. */
async function driveGlobeUntilSettled(globe, probeManager, onTick, maxTicks = 20_000) {
  let ticks = 0;
  for (;;) {
    globe.update(GLOBE_PROBE_CAMERA_LOCAL, SCREEN.screenHeightPx, SCREEN.fovYRad);
    if (onTick) onTick(globe);
    ticks += 1;
    if (probeManager.pending.size === 0) return { settled: true, ticks };
    if (ticks >= maxTicks) return { settled: false, ticks };
    await new Promise((resolve) => setImmediate(resolve));
  }
}

/** `{ "<level>/<x>/<y>": { sourceLayerId, hasTexture } }` for every mesh `globe.group`
 * currently holds -- read off the REAL scene graph (`group.traverse`), the same
 * technique `tests/test_viewer_globe_layer_manager.py`'s own browser probe uses
 * (`group.traverse((o) => { if (!o.isMesh) return; ... })`), not a private reach
 * into `GlobeLayer`'s own `_meshes` Map. `sourceLayerId` comes from
 * `mesh.material.map.userData.sourceLayerId` -- the REAL, per-texture provenance tag
 * `ImageryLayerAdapter`/`GatewayImageryLayerAdapter`'s own `load()` applies AT ITS
 * SOURCE (see those files' own module docstrings), asserted PER MESH here, never
 * from a global counter. */
function meshProvenance(globe) {
  const out = {};
  globe.group.traverse((obj) => {
    if (!obj.isMesh) return;
    const tile = obj.userData.tile;
    const key = tile ? `${tile.level}/${tile.x}/${tile.y}` : '(no tile)';
    const map = obj.material && obj.material.map;
    out[key] = {
      hasTexture: !!map,
      sourceLayerId: map && map.userData ? (map.userData.sourceLayerId ?? null) : null,
      level: tile ? tile.level : null,
    };
  });
  return out;
}

// Required proof 4, exactly as written: "never a frame with material.map === null
// once the default has loaded" -- a PER-MESH monotonic invariant, not a global one.
// A brand-new mesh (or one whose own first load simply has not resolved yet --
// `probeManager`'s own `maxConcurrentLoads: 6` means, with `meshCountProbe` tiles
// selected at once, only 6 default-imagery requests are admitted per tick, so the
// OTHER meshes legitimately still show their initial `material.map === null` for a
// few ticks even after the FIRST few meshes' own default texture has already
// resolved) is not a regression -- `buildTileMesh`'s own documented contract is a
// flat placeholder colour, `material.map === null`, until THAT mesh's own first
// load completes (web/js/globe.js's module docstring). What this invariant actually
// guards against is a mesh that ALREADY had a texture LOSING it (regressing back to
// null) on a later tick -- toggling a set on/off must never do that. `everLoadedKeys`
// is the real, per-tile-key memory this needs: once a key's own mesh has EVER shown
// a texture, this checks that it still shows ONE (not necessarily the SAME one) on
// every subsequent tick.
const everLoadedKeys = new Set();
let neverTexturelessOnceLoadedPerMesh = true;
let texturelessRegressionCount = 0;
function checkTextureInvariant(globe) {
  const prov = meshProvenance(globe);
  for (const [key, v] of Object.entries(prov)) {
    if (v.hasTexture) {
      everLoadedKeys.add(key);
    } else if (everLoadedKeys.has(key)) {
      neverTexturelessOnceLoadedPerMesh = false;
      texturelessRegressionCount += 1;
    }
  }
}

const probeManager = new LayerManager({ memoryBudgetBytes: 50_000_000, maxConcurrentLoads: 6 });
const globe = new GlobeLayer({
  layerManager: probeManager,
  textureLoader: makeDefaultImageryLoaderStub(),
  maxLevel,
  maxTiles: 64,
});

// Round 6 (manager review): the SAME `decodeModeCounts` disclosure as the frame-time
// section above, for every REAL gateway-backed layer this probe registers
// (`gateway-a`/`gateway-b`, never the default -- the default's own payload is this
// probe's own synchronous stub, not `decodeTileBytesToTexture`'s output, so it has
// no `decodeMode` at all and is intentionally excluded here). Tallies whatever is
// CURRENTLY resident under a `gateway-*` layer id at the moment it's called --
// called after every settle point below, so a tile that later gets evicted/disposed
// is still counted for the settle point(s) where it WAS resident (a running tally,
// not a live recount from probeManager.resident at report time, which would miss
// anything already evicted/removed by then).
const probeDecodeModeCounts = {};
function tallyGatewayDecodeModes() {
  for (const entry of probeManager.resident.values()) {
    if (!entry.layerId.startsWith('gateway-')) continue;
    const mode = entry.payload && entry.payload.userData && entry.payload.userData.decodeMode;
    if (!mode) continue;
    probeDecodeModeCounts[mode] = (probeDecodeModeCounts[mode] || 0) + 1;
  }
}

// -------------------------------------------------------------- step A: default only
const stepA = await driveGlobeUntilSettled(globe, probeManager, checkTextureInvariant);
const provenanceDefaultOnly = meshProvenance(globe);
const meshCountProbe = Object.keys(provenanceDefaultOnly).length;
const allDefaultBeforeAnyGatewaySet = meshCountProbe > 0
  && Object.values(provenanceDefaultOnly).every((v) => v.sourceLayerId === 'imagery' && v.hasTexture);

// ---------------------------------------------------------- step B: toggle set A ON
const layerA = new GatewayImageryLayerAdapter({ id: 'gateway-a', manifestSha256, origin });
await layerA.fetchManifest();
probeManager.addLayer(layerA);
const stepB = await driveGlobeUntilSettled(globe, probeManager, checkTextureInvariant);
tallyGatewayDecodeModes();
const provenanceWithA = meshProvenance(globe);
// Requirement 1: EVERY mesh this camera selected (manifest A covers levels 0-2 in
// full, the same maxLevel this whole file already runs at) must now be bound to
// gateway-a's own payload, not the default's.
const everyMeshBoundToSetAWhileOn = Object.values(provenanceWithA).length > 0
  && Object.values(provenanceWithA).every((v) => v.sourceLayerId === 'gateway-a' && v.hasTexture);
const someMeshChangedProvenanceFromDefaultToA = Object.keys(provenanceWithA).some(
  (k) => provenanceDefaultOnly[k] && provenanceDefaultOnly[k].sourceLayerId === 'imagery' && provenanceWithA[k].sourceLayerId === 'gateway-a',
);

// --------------------------------------------------------- step C: toggle set A OFF
probeManager.removeLayer('gateway-a');
globe.update(GLOBE_PROBE_CAMERA_LOCAL, SCREEN.screenHeightPx, SCREEN.fovYRad); // "on the next update() tick, without a page reload"
checkTextureInvariant(globe);
const provenanceAfterRemoveA = meshProvenance(globe);
// Requirement 2: restored to the default on the VERY NEXT tick, no further settling
// needed (the default's own resident payload never went anywhere -- only the
// now-unregistered gateway-a entries drop out of layerManager.imageryLayers()).
const restoredToDefaultAfterToggleOff = Object.values(provenanceAfterRemoveA).length > 0
  && Object.values(provenanceAfterRemoveA).every((v) => v.sourceLayerId === 'imagery' && v.hasTexture);

// ------------------------------------------------ step D/E: two real sets, per tile
// Only when the caller gave this harness a second, real manifest -- see this file's
// own module docstring on `manifestSha256B` for why this is optional and what
// running without it means.
let twoSetProbe = { skipped: 'no manifestSha256B given on the command line' };
if (manifestSha256B) {
  // Re-add set A fresh (the instance `removeLayer`'d above already released/disposed
  // its own textures -- a fresh instance is exactly what web/js/app.js's own
  // toggleGatewayLayer does on every toggle-on, never a reused, half-torn-down one).
  const layerA2 = new GatewayImageryLayerAdapter({ id: 'gateway-a', manifestSha256, origin });
  await layerA2.fetchManifest();
  probeManager.addLayer(layerA2);
  await driveGlobeUntilSettled(globe, probeManager, checkTextureInvariant);
  tallyGatewayDecodeModes();
  const provenanceWithA2 = meshProvenance(globe);

  // Set B: deliberately shallower (maxLevel below the tile set the CALLER built it
  // as -- see tests/test_viewer_layers_stream.py's own `tile_set_b` fixture) than
  // set A, so it genuinely, provably does NOT cover this camera's own level-2 tiles
  // -- never assumed, checked below from B's own real, fetched manifest.
  const layerB = new GatewayImageryLayerAdapter({ id: 'gateway-b', manifestSha256: manifestSha256B, origin });
  const manifestTileCountB = await layerB.fetchManifest();
  probeManager.addLayer(layerB); // registered AFTER layerA2 -- "later in list order", per question 231's ruling
  const stepD = await driveGlobeUntilSettled(globe, probeManager, checkTextureInvariant);
  tallyGatewayDecodeModes();
  const provenanceWithB = meshProvenance(globe);

  const meshesByLevel = {};
  for (const [k, v] of Object.entries(provenanceWithB)) {
    (meshesByLevel[v.level] ??= []).push({ key: k, sourceLayerId: v.sourceLayerId, hasTexture: v.hasTexture });
  }
  const levelsPresent = Object.keys(meshesByLevel).map(Number).sort((a, b) => a - b);
  const deepestLevel = Math.max(...levelsPresent);
  const shallowLevels = levelsPresent.filter((lv) => lv < deepestLevel);

  // Requirement 3, half 1: every mesh at a level SET B's own manifest covers (< the
  // deepest/finest level this camera selected, which is exactly the level B's own
  // shallower pyramid stops short of -- confirmed structurally below, not assumed)
  // must be bound to gateway-b -- the LATER-registered set wins, per tile.
  const laterSetWinsWhereCovered = shallowLevels.length > 0 && shallowLevels.every(
    (lv) => meshesByLevel[lv].every((m) => m.sourceLayerId === 'gateway-b' && m.hasTexture),
  );
  // Requirement 3, half 2: every mesh at the deepest level (NOT in set B's own
  // manifest -- set B's manifest only goes to `maxLevelB`, confirmed against
  // `manifestTileCountB` and the real HTTP failures this produced, below) falls back
  // to set A, which DOES cover it -- never left textureless, never wrongly shown as
  // set B's.
  const earlierSetWinsWhereLaterDoesNotCover = meshesByLevel[deepestLevel]
    && meshesByLevel[deepestLevel].length > 0
    && meshesByLevel[deepestLevel].every((m) => m.sourceLayerId === 'gateway-a' && m.hasTexture);
  // The real, structural reason half 2 holds: set B's real gateway genuinely 404'd
  // real requests for the deepest level's real tile addresses (never assumed from
  // the manifest tile count alone) -- `TileHttpError`, this manager's own real
  // failure-memory policy (web/js/layers/layer.js's own module docstring).
  const realHttpErrorsRecordedForUncoveredTiles = probeManager.failureNames().includes('TileHttpError')
    && probeManager.failedCount > 0;

  // ------------------------------------------------------- toggle B off, back to A
  probeManager.removeLayer('gateway-b');
  globe.update(GLOBE_PROBE_CAMERA_LOCAL, SCREEN.screenHeightPx, SCREEN.fovYRad);
  checkTextureInvariant(globe);
  const provenanceAfterRemoveB = meshProvenance(globe);
  const restoredToSetAAfterRemovingB = Object.values(provenanceAfterRemoveB).length > 0
    && Object.values(provenanceAfterRemoveB).every((v) => v.sourceLayerId === 'gateway-a' && v.hasTexture);

  twoSetProbe = {
    manifestSha256B,
    manifestTileCountB,
    deepestLevel,
    shallowLevels,
    meshCountAtDeepestLevel: (meshesByLevel[deepestLevel] || []).length,
    meshCountAtShallowLevels: shallowLevels.reduce((n, lv) => n + meshesByLevel[lv].length, 0),
    laterSetWinsWhereCovered,
    earlierSetWinsWhereLaterDoesNotCover,
    realHttpErrorsRecordedForUncoveredTiles,
    restoredToSetAAfterRemovingB,
    settledD: stepD,
    // Sanity: this probe is only meaningful if it genuinely exercised both a
    // covered-by-both level AND an uncovered-by-B level in the SAME run -- the "only
    // a meaningful test if it had to run" reasoning this codebase applies elsewhere
    // (tests/test_viewer_layers_stream.py's own module docstring).
    exercisedBothCases: shallowLevels.length > 0 && (meshesByLevel[deepestLevel] || []).length > 0,
  };
  twoSetProbe.ok = laterSetWinsWhereCovered && earlierSetWinsWhereLaterDoesNotCover
    && realHttpErrorsRecordedForUncoveredTiles && restoredToSetAAfterRemovingB && twoSetProbe.exercisedBothCases;
}

const globeLayerProbe = {
  meshCountProbe,
  allDefaultBeforeAnyGatewaySet,
  stepASettled: stepA.settled,
  stepBSettled: stepB.settled,
  everyMeshBoundToSetAWhileOn,
  someMeshChangedProvenanceFromDefaultToA,
  restoredToDefaultAfterToggleOff,
  twoSetProbe,
  neverTexturelessOnceLoadedPerMesh,
  defaultHasEverLoaded: everLoadedKeys.size > 0,
  texturelessRegressionCount,
  probeManagerFailedCount: probeManager.failedCount,
  probeManagerFailureNames: probeManager.failureNames(),
  // Round 6 (manager review) -- see `tallyGatewayDecodeModes`'s own doc comment:
  // structurally expected to be 100% `'placeholder-no-createImageBitmap'` under
  // node, regardless of how real the underlying PNG bytes are.
  decodeModeCounts: probeDecodeModeCounts,
};
globeLayerProbe.ok = allDefaultBeforeAnyGatewaySet && everyMeshBoundToSetAWhileOn
  && someMeshChangedProvenanceFromDefaultToA && restoredToDefaultAfterToggleOff
  && neverTexturelessOnceLoadedPerMesh && globeLayerProbe.defaultHasEverLoaded
  && (manifestSha256B ? twoSetProbe.ok === true : true);

// ------------------------------------------------------------------------- report
frameMsList.sort((a, b) => a - b);
function percentile(sorted, p) {
  if (sorted.length === 0) return 0;
  const idx = Math.min(sorted.length - 1, Math.floor(p * sorted.length));
  return sorted[idx];
}
const maxFrameMs = frameMsList.length ? frameMsList[frameMsList.length - 1] : 0;
const p50FrameMs = percentile(frameMsList, 0.5);
const p95FrameMs = percentile(frameMsList, 0.95);

const result = {
  origin,
  manifestSha256,
  frameCount: frameMsList.length,
  maxFrameMs,
  p50FrameMs,
  p95FrameMs,
  frameBudgetMs,
  everyFrameWithinBudget: frameMsList.every((ms) => ms <= frameBudgetMs),
  memoryBudgetBytes,
  maxResidentBytesObserved,
  budgetRespected: maxResidentBytesObserved <= memoryBudgetBytes,
  softViolationTaken,
  tilesFetched,
  bytesFetched,
  etagVerifiedCount,
  etagMismatchCount,
  // See the `byteCostCheckedCount` declaration above: the budget is charged the
  // DECLARED per-tile cost, so these three say whether that declaration matched the
  // bytes that actually arrived. `byteCostMismatchCount == 0` is what makes
  // `budgetRespected` a statement about real memory rather than about an estimate.
  maxLevel,
  declaredTileBytes: layer.tileBytes,
  byteCostCheckedCount,
  byteCostMismatchCount,
  maxByteCostErrorBytes,
  // Round 4 (question 228's own round-3-defect-5 follow-up) -- see the manifest-fetch
  // section and the `layer.load` wrapper, above, for what these mean: a healthy run
  // against this fixture's real, fully-populated manifest is expected to show
  // `manifestLoaded: true` and every entry of `byteCostSourceCounts` under
  // `'manifest'`, never `'fallback-estimate'`.
  manifestLoaded: layer.manifestLoaded,
  manifestTileCount: layer.manifestTileCount,
  manifestFetchError,
  byteCostSourceCounts,
  decodeModeCounts,
  // Round 4 (question 228): reported unconditionally, alongside softViolationTaken,
  // for the identical reason -- see layer.js's constructor doc comment for the exact
  // distinction between a budget deferral (counted here) and a request merely held
  // back by maxConcurrentLoads or the failure-memory blacklist (neither counted).
  deferredCount: manager.deferredCount,
  lastStepDeferred: manager.lastStepDeferred,
  // Round 4 follow-up (manager review): reported unconditionally too -- see layer.js's
  // constructor doc comment. THIS harness is exactly where a nonzero value here would
  // be most meaningful: `layer.fetchManifest()` (above) resolves mid-run relative to
  // this file's own calibration timing, so any tile admitted between this run's first
  // `update()` and that resolution -- if the ordering requirement documented on
  // `fetchManifest()` itself were ever violated -- would show up here as a nonzero
  // revision, not silently.
  byteCostRevisionCount: manager.byteCostRevisionCount,
  byteCostRevisionBytes: manager.byteCostRevisionBytes,
  cancelledCount: manager.cancelledCount,
  evictedCount: manager.evictedCount,
  // Failure-memory policy (Finding 1, corrective round 3 -- see layer.js's
  // constructor doc comment): reported unconditionally here too, not just by
  // web/js/layers_check.mjs, so a failure against the REAL gateway is never
  // silently uncounted either. A `TileHttpError`/`TileEtagMismatchError` (or an
  // AbortError from a genuine cancellation, which never reaches `_onFailed` at all
  // -- see that method's own doc comment) are the only rejection shapes this real
  // adapter can produce; on a healthy run against a fully-populated fixture (every
  // real, existing tile address, see this file's own module docstring) this is
  // expected to be 0/empty.
  failedCount: manager.failedCount,
  failureNames: manager.failureNames(),
  commitCount,
  httpStatusCounts,
  settledBeforeMaxFrames,
  maxFrames: MAX_FRAMES,
  maxCommitsPerFrame: MAX_COMMITS_PER_FRAME,
  maxConcurrentLoads: manager.maxConcurrentLoads,
  calibrationRoundTripMs,
  dwellMsPerPosition,
  // Round 6 (docs/open-questions.md question 231's ruling) -- see this file's own
  // module docstring section above the GlobeLayer probe for what each field means.
  globeLayerProbe,
  // Required proof 5 ("console-clean") -- see this file's own module docstring at
  // the top, "console.warn/console.error are wrapped".
  consoleWarnings,
  unhandledRejections,
};

process.stdout.write(JSON.stringify(result));
