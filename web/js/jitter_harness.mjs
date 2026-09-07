// CLI harness for tests/test_viewer_jitter.py: `node web/js/jitter_harness.mjs`.
//
// Prints one JSON object to stdout measuring the same quantity docs/open-questions.md
// Q46 asks CI to measure: positional jitter, in metres, with and without the floating
// origin, at LEO, Moon distance, Mars distance and a 10 m RPO scene. All arithmetic is
// imported from web/js/origin.js -- nothing here reimplements Math.fround or the
// origin subtraction; this file only supplies scene data (realistic distances and
// speeds) and glues origin.js's functions together the way a caller (the frame graph /
// scene.js, once wired in) would.
//
// Why "jitter" is modelled as "error vs. the exact f64 answer, one frame after an
// origin rebase": the origin is not re-based every render frame (only on a focus
// change, or when drift grows large -- see FloatingOrigin.setOrigin in origin.js), so
// the worst case *between* rebases is what matters. Evaluating one frame (1/60 s) of
// real motion after a rebase is the common case, not a cherry-picked best case: it is
// what every frame between two rebases looks like. The reported number is the
// deviation between what the render pipeline reconstructs and the true double-
// precision position -- exactly the shake a viewer would show, because consecutive
// frames sample different points inside this same error band as the scene animates.
import { FloatingOrigin, toRenderSpaceNoOrigin, trueRelative, length } from './origin.js';

const SCALE = 1e-3; // km -> scene units; must match scene.js's exported SCALE
const SCENE_UNIT_M = 1e6; // 1 scene unit = 1000 km = 1e6 m
const FRAME_DT_S = 1 / 60; // one render frame

function km(x) { return x * SCALE; }

// Scene data. Position and velocity *directions* are deliberately not axis-aligned
// and not (anti)parallel to each other -- reusing the exact unit directions of
// altavista/FRAMES.md's own worked example state, scaled to each scene's distance and
// orbital speed. This matters for correctness of the measurement, not just realism:
// an axis-aligned scene (e.g. position purely along +x, velocity purely along +y)
// would put the moving component on an axis where the position component is exactly
// zero, so `fround(0 + small) - fround(0)` comes back essentially exact and the
// float32 cancellation error the floating origin exists to fix never shows up --
// silently understating (in fact nearly zeroing) the no-origin error. Every axis
// below carries both a large absolute component and the small frame-to-frame change,
// which is what a real 3D orbit looks like and what actually exercises the bug.
const POS_DIR = { x: 0.9968627986978583, y: 0.07426320500053758, z: -0.02737767256948 };
const VEL_DIR = { x: -0.028802960407004915, y: 0.9646124473319705, z: 0.2620939066899859 };
function scaleVec(dir, mag) { return { x: dir.x * mag, y: dir.y * mag, z: dir.z * mag }; }

const SCENES = {
  LEO: {
    // altavista/FRAMES.md's own worked example state: |pos| ~= 6899.8 km, |vel| ~= 7.673 km/s.
    posKm: scaleVec(POS_DIR, 6899.783008237939),
    velKmS: scaleVec(VEL_DIR, 7.672822407041623),
  },
  Moon: {
    // Mean Earth-Moon distance, ~384400 km; mean lunar orbital speed ~1.022 km/s.
    posKm: scaleVec(POS_DIR, 384400),
    velKmS: scaleVec(VEL_DIR, 1.022),
  },
  Mars: {
    // Mars mean distance from the Sun (semi-major axis), ~227.9426e6 km; Mars mean
    // heliocentric orbital speed ~24.07 km/s.
    posKm: scaleVec(POS_DIR, 227942600),
    velKmS: scaleVec(VEL_DIR, 24.07),
  },
};

function vecKmToScene(v) {
  return { x: km(v.x), y: km(v.y), z: km(v.z) };
}

/** LEO/Moon/Mars: single moving object, origin rebased at t0, error measured at t0+1frame. */
function measureSingleObject(posKm, velKmS) {
  const pos0 = vecKmToScene(posKm);
  const disp = vecKmToScene({ x: velKmS.x * FRAME_DT_S, y: velKmS.y * FRAME_DT_S, z: velKmS.z * FRAME_DT_S });
  const pos1 = { x: pos0.x + disp.x, y: pos0.y + disp.y, z: pos0.z + disp.z };

  // WITH floating origin: rebase to pos0 (the "focus changed here" moment), then
  // render pos1 (one frame of real motion later) relative to that origin.
  const fo = new FloatingOrigin();
  fo.setOrigin('scene', pos0.x, pos0.y, pos0.z);
  const renderWith = fo.toRenderSpace('scene', pos1);
  const reconWith = { x: pos0.x + renderWith.x, y: pos0.y + renderWith.y, z: pos0.z + renderWith.z };
  const errWithM = length(trueRelative(reconWith, pos1)) * SCENE_UNIT_M;

  // WITHOUT floating origin: today's scene.js behaviour -- pos1 and the eye (here,
  // the same pos0 the camera is orbiting, i.e. focused near the object) both stored
  // as absolute float32 values, subtracted on the GPU in float32. eye == pos0 models
  // a camera that is, as scene.js's setFocus/_focusPosition do, centred on the
  // object being viewed -- the *best case* for the no-origin path, not a worst case,
  // so this does not overstate the failure.
  const renderWithout = toRenderSpaceNoOrigin(pos1, pos0);
  const trueRel = trueRelative(pos1, pos0);
  const errWithoutM = length({
    x: renderWithout.x - trueRel.x,
    y: renderWithout.y - trueRel.y,
    z: renderWithout.z - trueRel.z,
  }) * SCENE_UNIT_M;

  return { errWithM, errWithoutM, trueDisplacementM: length(disp) * SCENE_UNIT_M };
}

/** RPO: two objects 10 m apart at LEO altitude; what matters is their *separation*,
 * not either one's absolute position -- exactly the RIC/VVLH use case Q46 names. */
function measureRpo() {
  const chiefKm = SCENES.LEO.posKm;
  const sepM = 10;
  const deputyKm = { x: chiefKm.x, y: chiefKm.y + sepM / 1000, z: chiefKm.z };
  const chiefPos = vecKmToScene(chiefKm);
  const deputyPos = vecKmToScene(deputyKm);
  const trueSepM = length(trueRelative(deputyPos, chiefPos)) * SCENE_UNIT_M;

  // WITH floating origin: this is the point of a *per-frame* configurable origin --
  // the RIC frame's own origin is the chief, so the deputy's render-space position
  // IS its RIC-relative position, no large common term ever appears.
  const fo = new FloatingOrigin();
  fo.setOrigin('ric', chiefPos.x, chiefPos.y, chiefPos.z);
  const chiefRender = fo.toRenderSpace('ric', chiefPos);
  const deputyRender = fo.toRenderSpace('ric', deputyPos);
  const chiefWorld = { x: chiefPos.x + chiefRender.x, y: chiefPos.y + chiefRender.y, z: chiefPos.z + chiefRender.z };
  const deputyWorld = { x: chiefPos.x + deputyRender.x, y: chiefPos.y + deputyRender.y, z: chiefPos.z + deputyRender.z };
  const reconSepM = length(trueRelative(deputyWorld, chiefWorld)) * SCENE_UNIT_M;
  const errWithM = Math.abs(reconSepM - trueSepM);

  // WITHOUT floating origin: chief and deputy each independently stored as an
  // absolute float32 scene-unit coordinate (today's behaviour) -- their 10 m
  // separation is the *difference* of two ~6800 km-scale float32 numbers.
  const chiefNoFo = toRenderSpaceNoOrigin(chiefPos, { x: 0, y: 0, z: 0 });
  const deputyNoFo = toRenderSpaceNoOrigin(deputyPos, { x: 0, y: 0, z: 0 });
  const noFoSepM = length({
    x: deputyNoFo.x - chiefNoFo.x, y: deputyNoFo.y - chiefNoFo.y, z: deputyNoFo.z - chiefNoFo.z,
  }) * SCENE_UNIT_M;
  const errWithoutM = Math.abs(noFoSepM - trueSepM);

  return { errWithM, errWithoutM, trueSeparationM: trueSepM };
}

const result = {
  LEO: measureSingleObject(SCENES.LEO.posKm, SCENES.LEO.velKmS),
  Moon: measureSingleObject(SCENES.Moon.posKm, SCENES.Moon.velKmS),
  Mars: measureSingleObject(SCENES.Mars.posKm, SCENES.Mars.velKmS),
  RPO: measureRpo(),
};

process.stdout.write(JSON.stringify(result));
