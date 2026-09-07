#!/usr/bin/env node
// M16.3 headless harness (question 5's first demo bridge): reads a scenario JSON exactly as
// altavista's server publishes it (the shape GET /api/scenario/{name} returns, and what a
// browser's `loadScenario()` -- web/js/app.js -- receives over the WebSocket), runs it
// through the *real*, shipped `web/js/cdm_run.js` (no reimplementation -- the same module
// app.js imports), and prints the results as JSON. tests/test_cdm_run.py drives this exactly
// like tests/test_viewer_jitter.py's own harnesses (`_run_node_json`): run the real ES
// module under node, assert on the JSON it prints, never re-derive the arithmetic in Python.
//
// Usage: node verify_cdm_run.mjs <path-to-scenario.json> <duration-text>
//
// `<duration-text>` is the pre-formatted duration string app.js's own `fmtDuration` would
// have produced for this scenario's span (fmtDuration is a trivial, DOM-free pure function
// already, but it is not exported from app.js -- app.js is not a library other modules
// import from, only the browser's own entry point -- so the caller supplies the same text it
// independently computed from the scenario's own t0/t1, keeping this harness honest about
// testing only formatScenarioInfo's own logic, not silently re-deriving fmtDuration a second
// time).
//
// Optional 3rd/4th args `<focus-spacecraft-name> <epoch-a1mjd>` (M17.1, question 122): when
// given, also interpolates that spacecraft's position/velocity at that A1MJD epoch through
// the real, shipped `web/js/interp.js` `TrajectoryInterp` (the exact cubic-Hermite
// interpolation the viewer uses to place a spacecraft in whatever frame it is currently
// parented under -- frames.js's own module docstring: a frame node's motion "is exactly
// interp.js's TrajectoryInterp, reused here rather than re-implemented"), so a caller can
// prove *where* a CDM-ingested trajectory is placed, not just that the JSON contains numbers
// that look plausible.
import { readFileSync } from 'node:fs';
import * as THREE from 'three';
import { formatScenarioInfo, eventKinds, frameOptionLabel, hudText, viewportPaneTitle } from './cdm_run.js';
import { FrameGraph, orderFrameDefsByParent } from './frames.js';
import { TrajectoryInterp } from './interp.js';
import { SCALE, FIT_DISTANCE_FACTOR, computeFitRadius, defaultFrameViewRadius, resetTargetFrameId, eventHasRenderedInstance } from './scene.js';

const [, , scenarioPath, durationText, focusName, epochArg] = process.argv;
if (!scenarioPath) {
  console.error('usage: node verify_cdm_run.mjs <scenario.json> <duration-text> [focus-spacecraft-name] [epoch-a1mjd]');
  process.exit(2);
}
const sc = JSON.parse(readFileSync(scenarioPath, 'utf8'));

// M17.1 (question 122): the real frame graph a browser would build from this scenario's
// own additive `frames` list -- `web/js/scene.js`'s `_buildFrameGraph` is not reimplemented
// here (not this task's file to reproduce), only its documented wire-shape normalization
// (`frameDefinition.parentFrameId` -> `FrameNode`'s `parentId`, altavista/model.py's
// ScenarioData docstring) is repeated, exactly as the non-owned `web/js/frame_graph_check.mjs`
// already does against a hand-built wire-shaped list in its own "M4.1: multi-frame
// consumption" section. `FrameGraph`/`orderFrameDefsByParent` themselves are the real,
// unmodified, shipped code every browser session runs. This never synthesizes a fallback
// root the way scene.js's own (documented, `console.warn`-logged) fallback does for a
// scenario frame missing from `frames` -- a missing entities frame shows up here as
// `entitiesFrameInGraph: false`, not silently patched over, so a test asserting "the frame
// is actually there" cannot be fooled by a synthesized substitute.
function frameGraphFacts(scenario) {
  const defs = (scenario.frames || []).map((fd) => ({
    id: fd.id,
    parentId: fd.parentFrameId || null,
    originTrack: fd.originTrack || null,
    axes: fd.axes || null,
  }));
  const graph = new FrameGraph();
  for (const d of orderFrameDefsByParent(defs)) {
    const node = graph.addFrame(d);
    if (d.originTrack && d.originTrack.t && d.originTrack.t.length) node.setOriginTrack(d.originTrack);
  }
  return {
    frameIds: defs.map((d) => d.id),
    frameAxesById: Object.fromEntries(defs.map((d) => [d.id, d.axes])),
    entitiesFrameInGraph: graph.has(scenario.frame.name),
  };
}

// M18.2 (question 125, docs/open-questions.md): whether `Viewer.enableGlobe(bodyName)`
// (web/js/scene.js) would find a body to attach the globe to, for scenario JSON `sc` --
// the shape GET /api/scenario/{name} returns. enableGlobe()'s entire guard is
// `this.bodies.get(bodyName)` truthy; `this.bodies` is populated 1:1, keyed by `b.name`,
// straight from `sc.bodies` in setScenario() (`for (const b of sc.bodies) { ...
// this.bodies.set(b.name, ...) }` -- no filtering, no renaming, no dedup beyond what a
// Map key naturally does), so "would enableGlobe(bodyName) succeed for this scenario"
// and "does sc.bodies contain an entry named bodyName" are the exact same fact, not an
// approximation of it. Repeated here rather than constructed via a real `Viewer` for
// the same reason `frameGraphFacts()` above does not build one either: `Viewer`'s
// constructor needs a real `THREE.WebGLRenderer`/canvas, unavailable under plain node
// (see web/js/scene_jitter_harness.mjs's own module docstring for the identical
// constraint on `trajectoryRenderPositions()`).
function globeFacts(scenario, bodyName = 'Earth') {
  const bodyNames = (scenario.bodies || []).map((b) => b.name);
  // M19.5 (question 132): the scenario's own `imagery` field (altavista/server.py's
  // Hub.put(), stamped from the active profile's profiles/*.yaml imagery: section) is
  // what web/js/app.js reads to configure GlobeLayer's imageryUrl/maxLevel and to
  // display the attribution string -- surfaced here, verbatim, straight off the wire
  // scenario object (no re-derivation), so a Python test can prove the attribution
  // string this endpoint published is the exact same string the *viewer's own JS*
  // would read, not merely that the raw JSON happens to contain a key of that name.
  const imagery = scenario.imagery && typeof scenario.imagery === 'object' ? scenario.imagery : null;
  return { bodyNames, wouldEnableGlobe: bodyNames.includes(bodyName), imagery };
}

// M20.2 (question 134/E-27): camera-framing facts, using the real, shipped
// `computeFitRadius`/`defaultFrameViewRadius`/`FIT_DISTANCE_FACTOR` (web/js/scene.js
// -- the exact functions/constant `Viewer.setScenario()`/`fit()`/`setViewFrame()`
// use) directly against this scenario's own `bodies`/`spacecraft`/`frames` -- no
// `Viewer`/WebGL needed, see this file's own module docstring for why. `fitRadius` is
// what "Reset view" frames the whole scenario from while parented in the entities
// frame (camera at `fitRadius * FIT_DISTANCE_FACTOR` scene units, `_fitOrigin()`);
// `nonOriginFrameDefaultRadii` maps every OTHER frame's id to what `setViewFrame(id,
// null)` -- i.e. "Reset view" while that frame is the one selected, M20.2's "keeps
// the currently selected frame" fix -- uses as its own camera distance from that
// frame's origin. A body-axes frame's entry here must reflect that body's own scale
// (not the flat RPO constant) -- the concrete regression question 134/E-27 fixed.
function fitFacts(scenario) {
  // A well-formed published scenario's spacecraft always carry a real `t` array (real
  // or empty -- altavista/model.py's Trajectory.t defaults to `[]`, never omitted); this
  // harness is also driven against test_cdm_run.py's own deliberately minimal
  // formatScenarioInfo-only fixture (`{"name": "a"}`, no `t` at all), which
  // `computeFitRadius()` -- verbatim from `setScenario()`, which never receives such a
  // malformed entry in production -- is not required to tolerate. Filtered here, in
  // this harness only, not in scene.js's own function.
  const tracks = (scenario.spacecraft || []).filter((s) => Array.isArray(s.t));
  const fitRadius = computeFitRadius(scenario.bodies, tracks);
  const central = (scenario.bodies || []).find((b) => b.central) || null;
  const bodyByName = new Map((scenario.bodies || []).map((b) => [b.name, b]));
  const nonOriginFrameDefaultRadii = {};
  for (const fd of scenario.frames || []) {
    const body = fd.body ? bodyByName.get(fd.body) : null;
    nonOriginFrameDefaultRadii[fd.id] = defaultFrameViewRadius(body ? body.radius : undefined);
  }
  // Reset-target fact (question 134/E-27's "keeps the currently selected frame"):
  // simulates the camera currently sitting in the *other* declared frame (not the
  // scenario's own base frame) and asks resetTargetFrameId() -- the exact function
  // fit() calls -- what "Reset view" should target. Only meaningful when the
  // scenario declares more than one frame (a CDM-ingested run always does, M18.1);
  // null otherwise.
  const otherFrameId = (scenario.frames || []).map((fd) => fd.id).find((id) => id !== scenario.frame.name) || null;
  return {
    fitRadius,
    centralBodyRadiusSceneUnits: central ? central.radius * SCALE : null,
    originResetDistance: fitRadius * FIT_DISTANCE_FACTOR,
    nonOriginFrameDefaultRadii,
    resetTargetWhenCameraInOtherFrame: otherFrameId ? resetTargetFrameId(otherFrameId, scenario.frame.name) : null,
    otherFrameId,
  };
}

// M20.2 (question 136, viewer half): the frame picker option text `frameOptionLabel`
// (web/js/cdm_run.js, the exact function `app.js`'s `buildLists` calls) would produce
// for every frame this scenario declares -- straight off the wire `frames` list (id +
// description), not through a full `Viewer.frameList()` (which needs a real frame
// graph node but adds nothing frameOptionLabel itself reads).
function frameLabels(scenario) {
  return (scenario.frames || []).map((fd) => ({ id: fd.id, ...frameOptionLabel({ id: fd.id, description: fd.description }) }));
}

// M20.2 (question 135): `hudText` (web/js/cdm_run.js, the exact function app.js's
// render loop calls every frame) applied to this scenario's own base frame and, when
// the scenario declares a second frame, to that other frame too -- so a Python test
// can prove the HUD string genuinely differs when the *view* frame differs from the
// scenario's base `frame.name`, which a pre-M20.2 implementation (hardcoded to
// `scenario.frame.name`) could never produce.
function hudFacts(scenario) {
  const baseFrame = scenario.frame.name;
  const otherFrame = (scenario.frames || []).map((f) => f.id).find((id) => id !== baseFrame) || null;
  return {
    baseFrame,
    otherFrame,
    hudAtBaseFrame: hudText(scenario.name, baseFrame, null),
    hudAtOtherFrame: otherFrame ? hudText(scenario.name, otherFrame, null) : null,
    hudAtBaseFrameWithFocus: (scenario.spacecraft || []).length
      ? hudText(scenario.name, baseFrame, scenario.spacecraft[0].name) : null,
  };
}

// M21.2 (question 140): for every event on scenario `sc`, whether it would get a 3D
// label/marker in a live Viewer -- the real, shipped `eventHasRenderedInstance()`
// (web/js/scene.js), the exact function `setScenario()`'s events loop calls, run here
// against this scenario's own wire `spacecraft` list (already filtered server-side by
// `altavista.cdm.has_position_class`, question 133/M20.1 -- see that function's own
// docstring) rather than a live `this.spacecraft` Map, for the same "no WebGLRenderer
// under plain node" reason `globeFacts()`/`fitFacts()` above avoid building one. Keyed
// by event `name` (not index) so a caller can look up a specific event's fact without
// depending on `sc.events`'s own array order.
function event3dLabelFacts(scenario) {
  const spacecraftNames = (scenario.spacecraft || []).map((s) => s.name);
  return (scenario.events || []).map((ev) => ({
    name: ev.name,
    spacecraft: ev.spacecraft ?? null,
    wouldGetA3dLabel: eventHasRenderedInstance(ev, spacecraftNames),
  }));
}

// Interpolated {pos: [x,y,z] km, vel: [vx,vy,vz] km/s} for spacecraft `name` at A1MJD `t`,
// via the real TrajectoryInterp -- null if `name` is not on this scenario.
function interpolatedState(scenario, name, t) {
  const s = (scenario.spacecraft || []).find((x) => x.name === name);
  if (!s) return null;
  const interp = new TrajectoryInterp({ t: s.t, pos: s.pos, vel: s.vel });
  const pos = new THREE.Vector3();
  const vel = new THREE.Vector3();
  interp.at(t, pos, vel);
  return { pos: [pos.x, pos.y, pos.z], vel: [vel.x, vel.y, vel.z] };
}

// Question 169: `viewportPaneTitle` (web/js/cdm_run.js) applied to the exact real-world
// case the lead found live -- an "ICRF"-role pane (docs/open-questions.md's RPO default
// layout, web/js/layout/default_layouts.js's ICRF_PANEL_ID) whose viewport actually ended
// up parented in `EarthMJ2000Eq` (the RPO scenario's own base frame; the Python scenario
// declares no ICRF frame at all, so the viewport falls back). The wrong implementation
// this must fail against is the pre-M26.5 layout: a title hardcoded to the pane's
// INTENDED role ("3D View -- ICRF") regardless of what frame the viewport actually shows
// -- `whenActuallyShowingEarthMJ2000Eq` below must name that real frame, and must never
// contain the substring "ICRF" it cannot back up. `beforeAnyFrameIsKnown` is the
// pre-scenario state (page just loaded, no viewport parented in anything yet): the base
// label alone, never a frame name at all.
function paneTitleFacts() {
  return {
    beforeAnyFrameIsKnown: viewportPaneTitle('3D View', null),
    whenActuallyShowingEarthMJ2000Eq: viewportPaneTitle('3D View', 'EarthMJ2000Eq'),
    whenActuallyShowingEarthICRF: viewportPaneTitle('3D View', 'EarthICRF'),
  };
}

const result = {
  info: formatScenarioInfo(sc, durationText ?? ''),
  eventKinds: eventKinds(sc),
  eventEpochs: (sc.events || []).map((e) => e.t),
  configHash: (sc.meta && sc.meta.configHash) || null,
  frameGraph: frameGraphFacts(sc),
  globe: globeFacts(sc),
  fit: fitFacts(sc),
  frameLabels: frameLabels(sc),
  hud: hudFacts(sc),
  event3dLabels: event3dLabelFacts(sc),
  paneTitle: paneTitleFacts(),
};
if (focusName && epochArg !== undefined) {
  result.interpolated = interpolatedState(sc, focusName, parseFloat(epochArg));
}
console.log(JSON.stringify(result));
