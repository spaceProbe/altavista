// CLI harness for tests/test_viewer_jitter.py: `node web/js/scene_jitter_harness.mjs`.
//
// web/js/jitter_harness.mjs measures origin.js's arithmetic in isolation, against
// synthetic position vectors it constructs itself. This harness drives one level
// higher up the real call stack: it builds an actual {t, pos, vel} track, runs it
// through interp.js's real `TrajectoryInterp.polyline()` (the real Hermite
// densification code), and feeds the result into scene.js's real, exported
// `trajectoryRenderPositions()` -- the *exact* function `Viewer.setScenario()` and
// `Viewer._refreshOriginRelativeGeometry()` call to build a trajectory's
// `LineGeometry` vertex buffer. None of the three functions this imports
// (`TrajectoryInterp`, `trajectoryRenderPositions`, `FloatingOrigin`'s arithmetic) are
// reimplemented here; this file only supplies scene data and glues them together the
// way `scene.js` itself does.
//
// What is NOT exercised here, and why that's fine: `Viewer`'s constructor builds a
// `THREE.WebGLRenderer`, which needs a real GPU context/canvas unavailable under plain
// `node`. Nothing in `TrajectoryInterp.polyline()` or `trajectoryRenderPositions()`
// touches WebGL -- see scene.js's docstring on `trajectoryRenderPositions`, which is
// exported as a plain function specifically so this harness can call it without
// constructing a `Viewer`. The gap between this and a real browser is therefore
// exactly "does WebGL accept a Float32Array and render it", which
// docs/architecture.md's "WebGL2 only, vendored Three.js only" requirement and the
// manual browser verification in this task's report cover; it is not a gap in the
// origin-subtraction arithmetic, which is what jitter measures.
import { TrajectoryInterp } from './interp.js';
import { FloatingOrigin, length, trueRelative } from './origin.js';
import { SCALE, trajectoryRenderPositions } from './scene.js';
import { selectTiles, tileVertexPositions } from './globe_lod.js';

const SCENE_UNIT_M = 1e6; // 1 scene unit = 1000 km = 1e6 m
const FRAME_DT_S = 1 / 60; // one render frame

// Same non-axis-aligned unit directions as jitter_harness.mjs, and for the same
// reason: an axis-aligned scene would put the moving component on an axis where the
// position component is exactly zero, hiding the float32 cancellation error this
// whole pipeline exists to fix.
const POS_DIR = { x: 0.9968627986978583, y: 0.07426320500053758, z: -0.02737767256948 };
const VEL_DIR = { x: -0.028802960407004915, y: 0.9646124473319705, z: 0.2620939066899859 };
function scaleVec(dir, mag) { return [dir.x * mag, dir.y * mag, dir.z * mag]; }

// A mutable Vec3-like scratch object satisfying interp.js's `at()`/`segment()`
// contract (some branches call `.set(x,y,z)`, others assign `.x/.y/.z` directly).
function scratch() {
  return { x: 0, y: 0, z: 0, set(x, y, z) { this.x = x; this.y = y; this.z = z; return this; } };
}

export const SCENES = {
  // altavista/FRAMES.md's own worked example state.
  LEO: { distKm: 6899.783008237939, speedKmS: 7.672822407041623 },
  Moon: { distKm: 384400, speedKmS: 1.022 },
  Mars: { distKm: 227942600, speedKmS: 24.07 },
};

/** A real {t, pos, vel} track (A1MJD days, km, km/s) covering one render frame of
 * straight-line motion at (distKm, speedKmS) -- exactly the shape `setScenario()`
 * passes to `new TrajectoryInterp(s)` for a real spacecraft, just with 2 samples.
 * `pos`/`vel` are flat [x0,y0,z0,x1,y1,z1,...] arrays -- TrajectoryInterp indexes them
 * via `3*i` (see interp.js), the same flattened shape altavista/model.py's
 * Trajectory.to_dict() sends over the wire (`[c for p in self.pos for c in p]`). */
export function buildTrack(distKm, speedKmS) {
  const p0 = scaleVec(POS_DIR, distKm);
  const v = scaleVec(VEL_DIR, speedKmS);
  const p1 = [p0[0] + v[0] * FRAME_DT_S, p0[1] + v[1] * FRAME_DT_S, p0[2] + v[2] * FRAME_DT_S];
  const dtDay = FRAME_DT_S / 86400;
  return { t: [0, dtDay], pos: [...p0, ...p1], vel: [...v, ...v] };
}

/** LEO/Moon/Mars: rebase to the track's first sample (the "focus changed here"
 * moment, exactly as Viewer._rebaseOriginTo does), build the real densified polyline,
 * run it through the real trajectoryRenderPositions(), and for every vertex compare
 * the reconstructed absolute position against the ground truth -- interp.at(t) at
 * that same vertex's own sample time, which reproduces bit-for-bit the value
 * polyline() used to build it, isolating origin/quantization error from
 * interpolation-choice error. */
function measureSingleObject(distKm, speedKmS) {
  const track = buildTrack(distKm, speedKmS);
  const interp = new TrajectoryInterp(track);
  const poly = interp.polyline();
  const originAbs = { x: track.pos[0] * SCALE, y: track.pos[1] * SCALE, z: track.pos[2] * SCALE };

  const fo = new FloatingOrigin();
  fo.setOrigin('scene', originAbs.x, originAbs.y, originAbs.z);
  const renderWith = trajectoryRenderPositions(poly, fo, 'scene');

  // "without": FloatingOrigin globally disabled -- origin.js's documented
  // equivalence to pre-M3.1 scene.js (absolute coordinate, Math.fround, no origin).
  const foOff = new FloatingOrigin({ globalEnabled: false });
  const renderWithout = trajectoryRenderPositions(poly, foOff, 'scene');

  let errWithM = 0, errWithoutM = 0;
  const tmp = scratch();
  for (let i = 0, vi = 0; i < poly.times.length; i++, vi += 3) {
    interp.at(poly.times[i], tmp);
    const trueAbs = { x: tmp.x * SCALE, y: tmp.y * SCALE, z: tmp.z * SCALE };

    const reconWith = { x: originAbs.x + renderWith[vi], y: originAbs.y + renderWith[vi + 1], z: originAbs.z + renderWith[vi + 2] };
    errWithM = Math.max(errWithM, length(trueRelative(reconWith, trueAbs)) * SCENE_UNIT_M);

    const reconWithout = { x: renderWithout[vi], y: renderWithout[vi + 1], z: renderWithout[vi + 2] };
    errWithoutM = Math.max(errWithoutM, length(trueRelative(reconWithout, trueAbs)) * SCENE_UNIT_M);
  }
  return { errWithM, errWithoutM };
}

/** RPO: two independent tracks (chief, deputy) 10 m apart at LEO altitude, each with
 * its own real TrajectoryInterp/polyline/trajectoryRenderPositions call -- exactly
 * how two spacecraft's trajectory lines are built independently in setScenario(),
 * both against the *same* rebased frame origin. What matters is their reconstructed
 * *separation*, the RIC/VVLH quantity of interest.
 *
 * `fo` (a FloatingOrigin instance) is injectable -- default a fresh one, but
 * `measureRpoWithGlobePresent()` below passes one that has *also* been used to render
 * globe tile vertices under a different frame id ('earth-body'), so this same
 * function's numbers can be compared bit-for-bit against a pristine baseline run
 * (see that function for what a mismatch would mean). */
export function measureRpo(fo = new FloatingOrigin()) {
  const sepM = 10;
  const chiefTrack = buildTrack(SCENES.LEO.distKm, SCENES.LEO.speedKmS);
  const deputyPos = chiefTrack.pos.slice();
  for (let i = 1; i < deputyPos.length; i += 3) deputyPos[i] += sepM / 1000; // offset +y, km
  const deputyTrack = { t: chiefTrack.t, pos: deputyPos, vel: chiefTrack.vel };
  const chiefInterp = new TrajectoryInterp(chiefTrack);
  const deputyInterp = new TrajectoryInterp(deputyTrack);
  const chiefPoly = chiefInterp.polyline();
  const deputyPoly = deputyInterp.polyline();
  const originAbs = { x: chiefTrack.pos[0] * SCALE, y: chiefTrack.pos[1] * SCALE, z: chiefTrack.pos[2] * SCALE };

  fo.setOrigin('ric', originAbs.x, originAbs.y, originAbs.z);
  const chiefWith = trajectoryRenderPositions(chiefPoly, fo, 'ric');
  const deputyWith = trajectoryRenderPositions(deputyPoly, fo, 'ric');

  const foOff = new FloatingOrigin({ globalEnabled: false });
  const chiefWithout = trajectoryRenderPositions(chiefPoly, foOff, 'ric');
  const deputyWithout = trajectoryRenderPositions(deputyPoly, foOff, 'ric');

  let errWithM = 0, errWithoutM = 0;
  const ctmp = scratch(), dtmp = scratch();
  const n = Math.min(chiefWith.length, deputyWith.length, chiefPoly.times.length * 3);
  for (let vi = 0, i = 0; vi < n; vi += 3, i++) {
    const t = chiefPoly.times[i];
    chiefInterp.at(t, ctmp);
    deputyInterp.at(t, dtmp);
    const trueSepM = length({
      x: (dtmp.x - ctmp.x) * SCALE, y: (dtmp.y - ctmp.y) * SCALE, z: (dtmp.z - ctmp.z) * SCALE,
    }) * SCENE_UNIT_M;

    const cWith = { x: originAbs.x + chiefWith[vi], y: originAbs.y + chiefWith[vi + 1], z: originAbs.z + chiefWith[vi + 2] };
    const dWith = { x: originAbs.x + deputyWith[vi], y: originAbs.y + deputyWith[vi + 1], z: originAbs.z + deputyWith[vi + 2] };
    errWithM = Math.max(errWithM, Math.abs(length(trueRelative(dWith, cWith)) * SCENE_UNIT_M - trueSepM));

    const cWithout = { x: chiefWithout[vi], y: chiefWithout[vi + 1], z: chiefWithout[vi + 2] };
    const dWithout = { x: deputyWithout[vi], y: deputyWithout[vi + 1], z: deputyWithout[vi + 2] };
    errWithoutM = Math.max(errWithoutM, Math.abs(length(trueRelative(dWithout, cWithout)) * SCENE_UNIT_M - trueSepM));
  }
  return { errWithM, errWithoutM };
}

/**
 * M15.4's required precision proof: "a target-centred RPO view must keep centimetre
 * precision while the globe is visible." Builds the *exact* tile-vertex-position
 * pipeline web/js/globe.js's GlobeLayer uses (web/js/globe_lod.js's real
 * `selectTiles`/`tileVertexPositions`, not a description of them) for a camera at the
 * RPO scene's own LEO altitude, running every tile's vertices through the *same*
 * `FloatingOrigin` instance the RPO measurement below uses -- but under a different
 * frame id ('earth-body', vs. the RPO measurement's 'ric') -- then reruns exactly
 * `measureRpo()` against that same, now-globe-touched instance and compares the
 * result to a pristine baseline instance that never saw the globe at all.
 *
 * What a wrong implementation would fail here: origin.js's per-frame-id Map means a
 * correct implementation *must* produce `withGlobe.errWithM === baseline.errWithM`
 * exactly (not merely "both under the bound") -- any bug that let globe tile
 * construction leak into the 'ric' frame's stored origin (e.g. a frame-id typo, a
 * shared/aliased scratch object, or `toRenderSpaceArray` accidentally reading global
 * mutable state instead of only the passed frameId's own Map entry) would change
 * `withGlobe.errWithM` away from `baseline.errWithM`, and this assertion would catch
 * it even if the resulting number still happened to be under the centimetre bound.
 * `globeVertexMaxMagnitudeSceneUnits` separately proves the compatibility *mechanism*
 * itself (globe.js's module docstring): tile vertices stay at Earth-radius scale
 * (~6.4 scene units) regardless of the RPO camera's LEO-altitude distance from Earth's
 * centre, which is *why* they never need floating-origin treatment in the first place
 * -- a wrong implementation that (incorrectly) built tile vertices from the body's
 * *absolute* frame-origin-relative position (Mars-distance-scale magnitudes for a body
 * far from the current origin) would blow this small-magnitude expectation.
 */
function measureRpoWithGlobePresent() {
  const fo = new FloatingOrigin();
  // Camera at the RPO scene's own LEO altitude, arbitrary direction (this harness's
  // POS_DIR, same non-axis-aligned reasoning as the rest of this file) -- realistic:
  // in web/05_rpo_ric.py's live scene, the camera *is* at LEO altitude even though the
  // two spacecraft it is looking at are only 10 m apart.
  const camEcefM = scaleVec(POS_DIR, SCENES.LEO.distKm * 1000);
  const tiles = selectTiles({ x: camEcefM[0], y: camEcefM[1], z: camEcefM[2] }, {
    screenHeightPx: 900, fovYRad: (50 * Math.PI) / 180, sseThreshold: 24, maxLevel: 4, maxTiles: 200,
  });
  let globeVertexMaxMagnitudeSceneUnits = 0;
  for (const tile of tiles) {
    const verts = tileVertexPositions(tile, 4); // f64 body-local scene units -- exactly globe.js's buildTileMesh() input
    // Exercise the identical origin-subtract/fround pipeline scene.js's trajectory
    // path uses (FloatingOrigin.toRenderSpaceArray), under a different frame id --
    // globe.js's own design never needs this subtraction (see its module docstring),
    // but this harness deliberately routes through it anyway to prove the shared
    // machinery tolerates a second, independent frame id without cross-contaminating
    // 'ric' (the RPO measurement's frame), rather than merely asserting it by
    // inspection.
    const rendered = fo.toRenderSpaceArray('earth-body', verts);
    for (let i = 0; i < rendered.length; i += 3) {
      globeVertexMaxMagnitudeSceneUnits = Math.max(
        globeVertexMaxMagnitudeSceneUnits, Math.hypot(rendered[i], rendered[i + 1], rendered[i + 2]),
      );
    }
  }
  const withGlobe = measureRpo(fo);
  const baseline = measureRpo(new FloatingOrigin());
  return {
    errWithM: withGlobe.errWithM,
    errWithoutM: withGlobe.errWithoutM,
    matchesBaselineExactly: withGlobe.errWithM === baseline.errWithM && withGlobe.errWithoutM === baseline.errWithoutM,
    tileCount: tiles.length,
    globeVertexMaxMagnitudeSceneUnits,
  };
}

// M26.3: guarded so `web/js/viewport_check.mjs` can `import { measureRpo, buildTrack,
// SCENES }` from this module (reusing the exact RPO-measurement arithmetic, not a second
// copy of it -- see that file's own module docstring) WITHOUT triggering this script's
// own stdout/exit side effects merely by being imported. `node web/js/
// scene_jitter_harness.mjs` (tests/test_viewer_jitter.py's own invocation, a real
// subprocess run, never an import) is completely unaffected: `process.argv[1]` is this
// file's own path in exactly that case, so the guard is true and this still runs and
// prints, unchanged.
if (import.meta.url === `file://${process.argv[1]}`) {
  const result = {
    LEO: measureSingleObject(SCENES.LEO.distKm, SCENES.LEO.speedKmS),
    Moon: measureSingleObject(SCENES.Moon.distKm, SCENES.Moon.speedKmS),
    Mars: measureSingleObject(SCENES.Mars.distKm, SCENES.Mars.speedKmS),
    RPO: measureRpo(),
    RPO_with_globe: measureRpoWithGlobePresent(),
  };
  process.stdout.write(JSON.stringify(result));
}
