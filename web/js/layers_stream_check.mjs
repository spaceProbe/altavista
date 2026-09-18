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
import { selectTiles, geodeticToEcef } from './globe_lod.js';

// ------------------------------------------------------------------------ arguments
const [origin, manifestSha256, frameBudgetMsRaw, memoryBudgetBytesRaw, maxLevelRaw, tileBytesRaw, maxConcurrentLoadsRaw, dwellRoundTripsRaw] = process.argv.slice(2);

function fail(message) {
  process.stdout.write(JSON.stringify({ error: message }));
  process.exit(1);
}

if (!origin || !manifestSha256 || !frameBudgetMsRaw || !memoryBudgetBytesRaw) {
  fail(
    'usage: node web/js/layers_stream_check.mjs <origin> <manifestSha256> <frameBudgetMs> <memoryBudgetBytes> [maxLevel] [tileBytes] [maxConcurrentLoads] [dwellRoundTrips]',
  );
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

const layer = new GatewayImageryLayerAdapter({ manifestSha256, origin, tileBytes: declaredTileBytes });
const realLoad = layer.load.bind(layer);
layer.load = async (request, signal) => {
  try {
    const payload = await realLoad(request, signal);
    tilesFetched += 1;
    bytesFetched += payload.bytes.byteLength;
    byteCostCheckedCount += 1;
    const err = Math.abs(payload.bytes.byteLength - request.byteCost);
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
  const recomputed = createHash('sha256').update(Buffer.from(payload.bytes)).digest('hex');
  if (recomputed !== payload.sha256) {
    // payload.sha256 was already verified against the gateway's own ETag inside
    // load() (crypto.subtle, off-frame); this is a SECOND, synchronous check over
    // the SAME bytes this harness already has in hand -- see module docstring.
    // Disagreement here would mean the bytes changed after load() returned, which
    // never legitimately happens; fail loudly rather than silently commit garbage.
    throw new Error(`layers_stream_check: commit-time SHA-256 ${recomputed} disagrees with load-time ${payload.sha256} for ${globalKey}`);
  }
  pngDimensions(payload.bytes); // real, synchronous per-tile work -- see module docstring
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
};

process.stdout.write(JSON.stringify(result));
