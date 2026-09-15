#!/usr/bin/env node
// CLI harness for tests/test_viewer_jitter.py: `node web/js/resolve_frame_graph_input_check.mjs`.
//
// Question 209(c): `_buildFrameGraph`'s pure half, `resolveFrameGraphInput` (web/js/
// scene.js), extracted specifically so it can be exercised under plain `node` without
// a `THREE.WebGLRenderer` -- see that function's own doc comment and
// scene_jitter_harness.mjs's own module doc for why scene.js keeps a WebGL-free half
// at all (importing scene.js under plain `node` already works today, proven by
// scene_jitter_harness.mjs's own `trajectoryRenderPositions` import -- this harness
// follows the identical pattern for a different exported function).
//
// Three cases, matching `resolveFrameGraphInput`'s own doc comment exactly:
//   1. A normal scenario (a declared frame with a matching `frames[]` entry) -- no
//      synthesis, no warning.
//   2. A declared frame with NO matching `frames[]` entry (e.g. GMAT's Topocentric --
//      the pre-209(c) synthesized-root case, unchanged by this question) -- a root
//      node prefixed to `defs`, `warning` names the frame id.
//   3. Question 209(c)'s own trigger: NO declared frame at all (an empty `{"name",
//      "spacecraft": []}` publish) -- `originFrameId` degrades to `'root'`, `defs` is
//      just that one synthesized node, `warning` says so.
import { resolveFrameGraphInput } from './scene.js';

const checks = [];
function check(name, pass, detail) { checks.push({ name, pass: !!pass, detail: detail ?? null }); }

// ---- 1. a normal, matched scenario: no synthesis, no warning
{
  const sc = {
    frame: { name: 'EarthMJ2000Eq' },
    frames: [{ id: 'EarthMJ2000Eq', axes: 'AXES_KIND_UNSPECIFIED' }, { id: 'sat1_ric', parentFrameId: 'EarthMJ2000Eq', axes: 'AXES_KIND_RIC' }],
  };
  const { originFrameId, defs, warning } = resolveFrameGraphInput(sc);
  check('resolveFrameGraphInput: a declared, matched frame resolves to itself, defs pass through, no warning',
    originFrameId === 'EarthMJ2000Eq' && defs.length === 2 && defs.some((d) => d.id === 'sat1_ric') && warning === null,
    { originFrameId, defsIds: defs.map((d) => d.id), warning });
}

// ---- 2. a declared frame with no matching frames[] entry -- unchanged pre-209(c) case
{
  const sc = { frame: { name: 'BodyInertial' }, frames: [{ id: 'EarthMJ2000Eq', axes: 'AXES_KIND_UNSPECIFIED' }] };
  const { originFrameId, defs, warning } = resolveFrameGraphInput(sc);
  check('resolveFrameGraphInput: a declared frame absent from frames[] still resolves to its own id, with a synthesized root node prefixed',
    originFrameId === 'BodyInertial' && defs.length === 2 && defs[0].id === 'BodyInertial' && defs[0].parentId === null);
  check('resolveFrameGraphInput: names the missing frame id in its warning, never silent',
    typeof warning === 'string' && warning.includes('BodyInertial') && warning.includes('frames'), { warning });
}

// ---- 3. question 209(c)'s own trigger: no `frame` at all
{
  const sc = { name: 'empty-scenario', spacecraft: [] };
  const { originFrameId, defs, warning } = resolveFrameGraphInput(sc);
  check('resolveFrameGraphInput: a scenario with no frame at all synthesizes a bare \'root\' id, never throws',
    originFrameId === 'root' && Array.isArray(defs) && defs.length === 1 && defs[0].id === 'root' && defs[0].parentId === null);
  check('resolveFrameGraphInput: names the absent frame in its warning, never silent',
    typeof warning === 'string' && warning.includes("'frame'") && warning.includes('absent'), { warning });

  // Same synthesis for null/undefined `sc` and for a `sc.frames` list present but no
  // top-level `frame` -- both degrade the same honest way, never a throw.
  const forNull = resolveFrameGraphInput(null);
  const forUndefined = resolveFrameGraphInput(undefined);
  check('resolveFrameGraphInput: null/undefined sc synthesizes the same bare root, never throws',
    forNull.originFrameId === 'root' && forUndefined.originFrameId === 'root');
  const forFramesNoFrame = resolveFrameGraphInput({ frames: [{ id: 'EarthMJ2000Eq' }] });
  check('resolveFrameGraphInput: frames[] present but no declared frame still synthesizes root (there is no id to relate frames[] to)',
    forFramesNoFrame.originFrameId === 'root' && forFramesNoFrame.defs.length === 1);
}

const allPass = checks.every((c) => c.pass);
process.stdout.write(JSON.stringify({ allPass, checks }));
process.exit(allPass ? 0 : 1);
