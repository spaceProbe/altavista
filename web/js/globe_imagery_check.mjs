// CLI harness for tests/test_viewer_globe.py: `node web/js/globe_imagery_check.mjs`.
//
// M19.5 (docs/open-questions.md question 132, decided by the lead: "the imagery source
// is a profile setting ... an XYZ/WMTS URL template and an attribution string; no
// network in tests"). Proves the real, shipped `web/js/globe.js`'s `GlobeLayer` actually
// requests tiles from whatever `imageryUrl` template it is constructed with --
// substituting `{z}`/`{x}`/`{y}` verbatim, never re-encoded/prefixed/normalized, and
// never silently falling back to `DEFAULT_IMAGERY_URL` -- and that it does this without
// ever touching the network, which is this task's own binding rule as much as it is a
// property of a good test.
//
// How "no network call" is made structural, not merely observed: `GlobeLayer` (M19.5,
// this task) accepts an injectable `opts.textureLoader` -- any object with a `.load(url,
// onLoad, onProgress, onError)` method, the same shape `THREE.TextureLoader` exposes.
// This harness passes a stub whose `.load()` does nothing but record the exact URL
// string it was given: no `fetch`, no `Image` element, no XHR, so there is no code path
// left by which a real request could happen, independent of whatever the *real*
// `THREE.TextureLoader.load()` would have done with the same URL (which does hit the
// network in a browser -- that is exactly the call this harness must never make under
// `node`, and does not).
//
// What each check would fail against (this task's own "name the wrong implementation"
// requirement):
//   - `verbatimTemplateUsed`: an implementation that silently falls back to
//     `DEFAULT_IMAGERY_URL` instead of the configured template (the exact regression
//     this task exists to fix -- a profile's imagery source configured but never
//     actually wired to GlobeLayer), or one that re-encodes the substituted URL
//     (e.g. `encodeURIComponent`-ing `?`/`&`/`=` in a query string) or re-prefixes it
//     (e.g. always joining onto a hardcoded base rather than using the template as
//     given) -- the configured template below deliberately carries a query string and a
//     path prefix unrelated to the fixture's own `./fixtures/tiles/` shape, so either
//     bug changes the recorded URL away from this harness's independently-computed
//     `expectedUrl()`.
//   - `defaultProfileUsesTheOfflineFixture`: an implementation whose *default*
//     (`imageryUrl` omitted entirely, exactly what every pre-M19.5 call site did) no
//     longer matches `DEFAULT_IMAGERY_URL` -- i.e. a change to globe.js's own default
//     that `tests/test_profiles.py` (which pins every profile's declared default
//     `url_template` against this same constant, read from this file's source text) has
//     not been told about.
//   - `noRealNetworkIoAttempted`: the fake loader itself is the proof -- it is
//     structurally incapable of network I/O, so if `recordedUrls` is non-empty at all,
//     every one of those "requests" was intercepted, not sent.
import { GlobeLayer } from './globe.js';
import { SCENE_UNITS_PER_METRE } from './globe_lod.js';

const SCREEN = { screenHeightPx: 900, fovYRad: (50 * Math.PI) / 180 };

// A LEO-altitude camera (same class of position globe_lod_check.mjs's own "LEO close-up"
// step and scene_jitter_harness.mjs's RPO camera use) -- close enough that selectTiles()
// refines past the two level-0 root tiles, so this harness exercises a real, non-empty,
// non-trivial tile selection, not just the two roots.
const CAMERA_ECEF_M = { x: 6900000, y: 500000, z: 800000 };
const CAMERA_LOCAL_SCENE_UNITS = {
  x: CAMERA_ECEF_M.x * SCENE_UNITS_PER_METRE,
  y: CAMERA_ECEF_M.y * SCENE_UNITS_PER_METRE,
  z: CAMERA_ECEF_M.z * SCENE_UNITS_PER_METRE,
};

/** A stub matching THREE.TextureLoader's `.load(url, onLoad, onProgress, onError)`
 * shape, but performing no I/O of any kind -- see this file's own module docstring for
 * why this is what makes "no network call" a structural fact, not merely an observation. */
function makeRecordingLoader() {
  const recordedUrls = [];
  return {
    recordedUrls,
    load(url) { recordedUrls.push(url); },
  };
}

/** Independent re-implementation of globe.js's `urlForTile` substitution (not imported
 * from it -- see this file's module docstring: the point is to catch a bug in the real
 * `urlForTile`/`buildTileMesh` wiring, which importing the same function under test
 * could not do). Uses split/join rather than a single `.replace()` chain as a further
 * independent detail, and only ever touches the three declared placeholders -- nothing
 * else in the template is transformed, which is exactly what "verbatim" means. */
function expectedUrl(template, tile) {
  return template
    .split('{z}').join(String(tile.level))
    .split('{x}').join(String(tile.x))
    .split('{y}').join(String(tile.y));
}

/** Builds a real GlobeLayer with `imageryUrl` (or the class default, when omitted) and
 * the recording stub loader, runs one `update()` over the fixed LEO camera above, and
 * returns the tiles selected plus every URL the (never-networked) loader recorded. */
function driveGlobeLayer(imageryUrlOpt) {
  const opts = { textureLoader: makeRecordingLoader() };
  if (imageryUrlOpt !== undefined) opts.imageryUrl = imageryUrlOpt;
  const layer = new GlobeLayer(opts);
  const tiles = layer.update(CAMERA_LOCAL_SCENE_UNITS, SCREEN.screenHeightPx, SCREEN.fovYRad);
  return { tiles, recordedUrls: layer.textureLoader.recordedUrls, imageryUrlUsed: layer.imageryUrl };
}

// ------------------------------------------------------------- headline: verbatim template
// Deliberately unlike the offline fixture's own `./fixtures/tiles/{z}/{x}/{y}.png` shape
// in every way a re-encoding/re-prefixing/normalization bug could catch: an absolute
// URL, a path segment before the placeholders, and a query string with characters
// (`?`, `&`, `=`) a naive "URL-safe" re-encode would mangle.
const CONFIGURED_TEMPLATE = 'https://tiles.example.test/v3/{z}/{x}/{y}.png?session=abc123&fmt=webp';
const configured = driveGlobeLayer(CONFIGURED_TEMPLATE);
const configuredExpected = configured.tiles.map((t) => expectedUrl(CONFIGURED_TEMPLATE, t));
const verbatimTemplateUsed =
  configured.tiles.length > 0 &&
  configured.recordedUrls.length === configured.tiles.length &&
  configured.recordedUrls.every((u, i) => u === configuredExpected[i]) &&
  // Never the default fixture template -- proves this isn't a silent fallback.
  configured.recordedUrls.every((u) => !u.includes('./fixtures/tiles/'));

// --------------------------------------------------------- default profile == fixture
// No `imageryUrl` opt at all -- exactly what every call site that never configures the
// globe (and, per the default profile's own declared `url_template`, matching
// tests/test_profiles.py) would do.
const byDefault = driveGlobeLayer(undefined);
const defaultExpected = byDefault.tiles.map((t) => expectedUrl('./fixtures/tiles/{z}/{x}/{y}.png', t));
const defaultProfileUsesTheOfflineFixture =
  byDefault.tiles.length > 0 &&
  byDefault.imageryUrlUsed === './fixtures/tiles/{z}/{x}/{y}.png' &&
  byDefault.recordedUrls.length === byDefault.tiles.length &&
  byDefault.recordedUrls.every((u, i) => u === defaultExpected[i]);

const result = {
  configuredTemplate: CONFIGURED_TEMPLATE,
  configuredTileCount: configured.tiles.length,
  configuredRecordedUrls: configured.recordedUrls,
  configuredExpectedUrls: configuredExpected,
  verbatimTemplateUsed,
  defaultTileCount: byDefault.tiles.length,
  defaultImageryUrlUsed: byDefault.imageryUrlUsed,
  defaultRecordedUrls: byDefault.recordedUrls,
  defaultProfileUsesTheOfflineFixture,
  // True by construction (the recording stub never performs I/O) -- reported anyway so
  // the printed JSON states the fact explicitly rather than leaving it implicit.
  noRealNetworkIoAttempted: true,
};

process.stdout.write(JSON.stringify(result));
