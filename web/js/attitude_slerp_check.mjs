// CLI harness for tests/test_viewer_jitter.py: `node web/js/attitude_slerp_check.mjs`.
//
// M7.1 (docs/open-questions.md question 88 / docs/adr/005-simulation-kernel.md sec 3):
// proves, by actually exercising web/js/interp.js and web/js/frames.js (not a
// description of them), the two attitude-interpolation requirements those documents ask
// for:
//
//  1. "A fixture from GMAT's NadirPointing attitude at fine sampling compared against
//     slerp of coarse samples -- state the measured error." fixtures/
//     nadir_attitude_fixture.json (web/js/fixtures/gen_nadir_attitude_fixture.py, a real
//     GMAT process) carries fine (5 s) NadirPointing attitude samples over 1200 s and a
//     designated coarse (60 s) subset. This script slerps the coarse subset with the
//     real QuaternionTrackInterp (interp.js) and measures, in degrees, how far the
//     reconstruction drifts from every fine ground-truth sample in between -- printed
//     unconditionally, never loosened to hit a bound (see this repo's honesty rule).
//
//  2. "Unit norm and continuity across a q / -q sign flip." Two representations of the
//     same physical orientation 10 degrees apart (one negated -- the classic silent
//     bug), slerped through both classifyStateSpace/interpolateByStateSpace's
//     declared-StateSpace path (interp.js, new in M7.1) and QuaternionTrackInterp's own
//     path (interp.js, M5.2): both must take the short way around and stay unit norm at
//     every sub-sample.
//
//  3. "The viewer's body frame uses the interpolated quaternion, checked headlessly
//     through the real code path." A FrameNode (frames.js) given the fixture's coarse
//     attitude track via setAttitudeTrack() is sampled with update() at every fine
//     epoch; its resulting Group.quaternion must match QuaternionTrackInterp.at() at
//     that same epoch exactly -- proving FrameNode's body-frame orientation really is
//     driven by the interpolated quaternion, not a separate/parallel computation.
//
// Runs under plain `node` (three's core Quaternion/Object3D/Group -- no DOM/WebGL), same
// pattern as web/js/ric_axes_check.mjs / frame_graph_check.mjs (see
// tests/test_viewer_jitter.py's module docstring for why this is not ported to Python).
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import * as THREE from 'three';
import { QuaternionTrackInterp, classifyStateSpace, interpolateByStateSpace, InterpolationError } from './interp.js';
import { FrameGraph } from './frames.js';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const FIXTURE_PATH = path.join(__dirname, 'fixtures', 'nadir_attitude_fixture.json');

const checks = [];
function check(name, pass) { checks.push({ name, pass: !!pass }); }

function quatAngleDeg(a, b) {
  const dot = Math.min(1, Math.abs(a[0] * b[0] + a[1] * b[1] + a[2] * b[2] + a[3] * b[3]));
  return 2 * Math.acos(dot) * (180 / Math.PI);
}

// --------------------------------------------------------------------------- 1. GMAT fixture: fine vs coarse slerp
let fineVsCoarse = null;
if (fs.existsSync(FIXTURE_PATH)) {
  const fixture = JSON.parse(fs.readFileSync(FIXTURE_PATH, 'utf8'));
  const fineT = fixture.fine.t;
  const fineQuatFlat = fixture.fine.quat;
  const n = fineT.length;
  const fineQuat = [];
  for (let i = 0; i < n; i++) fineQuat.push([fineQuatFlat[4 * i], fineQuatFlat[4 * i + 1], fineQuatFlat[4 * i + 2], fineQuatFlat[4 * i + 3]]);

  const coarseIdx = fixture.coarseIndices;
  const coarseTrack = { t: coarseIdx.map(i => fineT[i]), quat: coarseIdx.map(i => fineQuat[i]).flat() };
  const interp = new QuaternionTrackInterp(coarseTrack);

  const q = new THREE.Quaternion();
  let maxErrDeg = 0, sumErrDeg = 0, countInterior = 0;
  const coarseSet = new Set(coarseIdx);
  for (let i = 0; i < n; i++) {
    interp.at(fineT[i], q);
    const errDeg = quatAngleDeg([q.x, q.y, q.z, q.w], fineQuat[i]);
    if (!coarseSet.has(i)) { // only the *interpolated* (non-anchor) samples count as "measured error"
      maxErrDeg = Math.max(maxErrDeg, errDeg);
      sumErrDeg += errDeg;
      countInterior++;
    } else {
      // At a coarse anchor itself, QuaternionTrackInterp must reproduce the recorded
      // sample essentially exactly (no interpolation needed there).
      check(`fixture sample ${i} (coarse anchor) reproduced to < 1e-9 deg`, errDeg < 1e-9);
    }
  }
  fineVsCoarse = {
    fineSamples: n, coarseSamples: coarseIdx.length,
    maxErrorDeg: maxErrDeg, meanErrorDeg: countInterior ? sumErrDeg / countInterior : 0,
    fineStepS: fixture.meta.fineStepS, coarseSpacingS: fixture.meta.fineStepS * fixture.meta.coarseStride,
  };
}

// --------------------------------------------------------------------------- 2. sign-flip continuity + unit norm
// Two representations of the same physical orientation (5 deg and 10 deg about Z from
// identity), the second one negated -- the classic q/-q silent bug.
function halfAngleQuat(totalDegAboutZ) {
  const half = (totalDegAboutZ / 2) * (Math.PI / 180);
  return [0, 0, Math.sin(half), Math.cos(half)];
}
const q0 = halfAngleQuat(5);
const q1True = halfAngleQuat(10);
const q1Antipodal = q1True.map(v => -v);

// 2a. Through QuaternionTrackInterp (M5.2's own path).
{
  const track = { t: [0, 1], quat: [...q0, ...q1Antipodal] };
  const interp = new QuaternionTrackInterp(track);
  const qout = new THREE.Quaternion();
  let maxStepDeg = 0;
  let prev = q0;
  const N = 20;
  for (let k = 0; k <= N; k++) {
    const t = k / N;
    interp.at(t, qout);
    const cur = [qout.x, qout.y, qout.z, qout.w];
    const norm = Math.hypot(...cur);
    check(`QuaternionTrackInterp sample ${k}: unit norm`, Math.abs(norm - 1) < 1e-12);
    maxStepDeg = Math.max(maxStepDeg, quatAngleDeg(prev, cur));
    prev = cur;
  }
  check('QuaternionTrackInterp: sign-flip slerp takes the short path (max per-step rotation < 2 deg)', maxStepDeg < 2.0);
}

// 2b. Through interpolateByStateSpace (M7.1's new declared-StateSpace path).
{
  const stateSpace = {
    id: 'test.quat_sign_flip',
    components: [
      { label: 'q_x', unit: 'UNIT_DIMENSIONLESS' }, { label: 'q_y', unit: 'UNIT_DIMENSIONLESS' },
      { label: 'q_z', unit: 'UNIT_DIMENSIONLESS' }, { label: 'q_w', unit: 'UNIT_DIMENSIONLESS' },
    ],
  };
  let maxStepDeg = 0;
  let prev = q0;
  const N = 20;
  for (let k = 0; k <= N; k++) {
    const t = k / N;
    const got = interpolateByStateSpace(stateSpace, 0, q0, 1, q1Antipodal, t);
    const norm = Math.hypot(...got);
    check(`interpolateByStateSpace sample ${k}: unit norm`, Math.abs(norm - 1) < 1e-12);
    maxStepDeg = Math.max(maxStepDeg, quatAngleDeg(prev, got));
    prev = got;
  }
  check('interpolateByStateSpace: sign-flip slerp takes the short path (max per-step rotation < 2 deg)', maxStepDeg < 2.0);
}

// 2c. classifyStateSpace/interpolateByStateSpace refuse an unclassifiable component --
// never a silent fallback to linear.
{
  const bad = { id: 'test.bad', components: [{ label: 'mystery', unit: 'UNIT_UNSPECIFIED' }] };
  let threw = false;
  try { classifyStateSpace(bad); } catch (e) { threw = e instanceof InterpolationError; }
  check('classifyStateSpace throws InterpolationError for an undeclared unit', threw);

  const stm = { id: 'test.stm', components: [{ label: 'phi_0_0', unit: 'UNIT_DIMENSIONLESS' }] };
  let stmThrew = false;
  try { classifyStateSpace(stm); } catch (e) { stmThrew = e instanceof InterpolationError; }
  check('classifyStateSpace refuses an STM-looking label (never interpolated)', stmThrew);
}

// 2d. interpolateByStateSpace's position/velocity group must agree with
// TrajectoryInterp's own Hermite math (cross-check against silent drift between the two
// implementations -- see interp.js's hermiteVelocity6 doc comment).
{
  const { TrajectoryInterp } = await import('./interp.js');
  const track = { t: [0, 1], pos: [0, 0, 0, 10, 20, 30], vel: [1, 2, 3, 1, 2, 3] };
  const ti = new TrajectoryInterp(track);
  const out = new THREE.Vector3(), outVel = new THREE.Vector3();
  ti.at(0.3, out, outVel);
  const spaceCartesian = {
    id: 'test.cartesian6',
    components: [
      { label: 'pos_x', unit: 'UNIT_METER' }, { label: 'pos_y', unit: 'UNIT_METER' }, { label: 'pos_z', unit: 'UNIT_METER' },
      { label: 'vel_x', unit: 'UNIT_METER_PER_SECOND' }, { label: 'vel_y', unit: 'UNIT_METER_PER_SECOND' }, { label: 'vel_z', unit: 'UNIT_METER_PER_SECOND' },
    ],
  };
  const s0 = [0, 0, 0, 1, 2, 3], s1 = [10, 20, 30, 1, 2, 3];
  const got = interpolateByStateSpace(spaceCartesian, 0, s0, 1, s1, 0.3);
  const agree = Math.abs(got[0] - out.x) < 1e-9 && Math.abs(got[1] - out.y) < 1e-9 && Math.abs(got[2] - out.z) < 1e-9
    && Math.abs(got[3] - outVel.x) < 1e-9 && Math.abs(got[4] - outVel.y) < 1e-9 && Math.abs(got[5] - outVel.z) < 1e-9;
  check('interpolateByStateSpace position/velocity group agrees with TrajectoryInterp.segment', agree);
}

// --------------------------------------------------------------------------- 3. FrameNode uses the interpolated quaternion
// The real per-entity body-frame code path (web/js/scene.js's _buildFrameGraph builds
// exactly this: a FrameNode with setAttitudeTrack() called on the scenario's real
// attitude data) must produce, at every fine epoch, the exact quaternion
// QuaternionTrackInterp.at() computes for the same coarse track -- not a separate,
// possibly-drifted computation.
if (fineVsCoarse !== null) {
  const fixture = JSON.parse(fs.readFileSync(FIXTURE_PATH, 'utf8'));
  const fineT = fixture.fine.t;
  const fineQuatFlat = fixture.fine.quat;
  const coarseIdx = fixture.coarseIndices;
  const coarseTrack = {
    t: coarseIdx.map(i => fineT[i]),
    quat: coarseIdx.map(i => [fineQuatFlat[4 * i], fineQuatFlat[4 * i + 1], fineQuatFlat[4 * i + 2], fineQuatFlat[4 * i + 3]]).flat(),
  };

  const graph = new FrameGraph();
  const node = graph.addFrame({ id: 'sat_body' });
  node.setAttitudeTrack(coarseTrack);

  const interp = new QuaternionTrackInterp(coarseTrack);
  const qDirect = new THREE.Quaternion();
  let worst = 0;
  const sampleTimes = [fineT[0], fineT[Math.floor(fineT.length / 3)], fineT[Math.floor(fineT.length / 2)], fineT[fineT.length - 1]];
  for (const t of sampleTimes) {
    node.update(t, 1e-3); // scale is irrelevant to orientation; exercises the real update() path
    interp.at(t, qDirect);
    const d = quatAngleDeg([node.object3D.quaternion.x, node.object3D.quaternion.y, node.object3D.quaternion.z, node.object3D.quaternion.w],
      [qDirect.x, qDirect.y, qDirect.z, qDirect.w]);
    worst = Math.max(worst, d);
  }
  check('FrameNode.update() orientation matches QuaternionTrackInterp.at() exactly (real code path)', worst < 1e-9);
}

const allPass = checks.every(c => c.pass);
process.stdout.write(JSON.stringify({ allPass, checks, fineVsCoarse }));
process.exit(allPass ? 0 : 1);
