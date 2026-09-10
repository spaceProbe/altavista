// CLI harness for tests/test_viewer_panels.py: `node web/js/panels_check.mjs
// <path-to-json>`, following the exact pattern every other viewer milestone's harness
// uses (web/js/viewport_check.mjs, web/js/globe_lod_check.mjs, web/js/layout/
// layout_tree_check.mjs): drives the REAL, shipped ES modules and prints one JSON
// object of named checks -- never a reimplementation of their logic.
//
// M26.4 (docs/ui-rework-plan.md): "the three panels." See web/js/REPORT_M26_4.md for
// the full account, in particular the FINDING that `RunProducts.scores` never reaches
// the client on any existing publish path today -- the input JSON file this harness
// reads therefore carries a `scenario.scores` key the Python test side adds ON TOP OF
// a real, live-server-published scenario (`tests/fixtures/demo_two_instance.
// runproducts.bin`, decoded and re-attached), so `run_products_panel.js`'s binding
// logic is proven against the real fixture's own scores even though no current server
// code path actually sends them.
import { objectiveRows, timelineEvents, eventTimelinePercent, measurementRows, measurementCountsBySensor, timelineMeasurementTicks } from './panels/run_products_panel.js';
import { provenanceLines } from './panels/console_panel.js';
import { groundTrack, lonLatFromScenarioFramePosition, bodyFixedPositionKm } from './ground_track.js';
import { ecefToGeodeticDeg, geodeticToEcef, tileCountX, tileCountY, WGS84_A_M, WGS84_B_M } from './globe_lod.js';
import { mosaicRows, tileUrlsForLevel, lonLatToPercent } from './panels/map_panel.js';
import {
  attachM264Panels, defaultLayoutForScenario, buildBaseSidebarViewportLayout, buildRpoTripleViewportLayout,
  RUN_PRODUCTS_PANEL_ID, MAP_PANEL_ID, CONSOLE_PANEL_ID,
} from './layout/default_layouts.js';
import { listLeaves } from './layout/split_tree.js';
// F3b (docs/feasibility-plan.md's F3 milestone): the feasibility-study panel's own pure,
// DOM-free functions -- `render()` itself is deliberately NOT imported/driven here,
// exactly like run_products_panel.js's own `render()` above: this harness runs under
// plain `node` (no `document`), so every DOM-touching function in this codebase is
// proven only by the manual browser check (see web/js/REPORT_F3b.md), never here.
import {
  scoreNames, gridAxes, axisLevels, gridRows, heatBucket, drawRows, isSampleOpenable, defaultScoreName,
} from './panels/feasibility_panel.js';
import { readFileSync } from 'fs';

const inputPath = process.argv[2];
if (!inputPath) { console.error('usage: node panels_check.mjs <path-to-json>'); process.exit(2); }
const input = JSON.parse(readFileSync(inputPath, 'utf8'));
const { scenario } = input;

const checks = [];
function check(name, pass) { checks.push({ name, pass: !!pass }); }
function approxEqual(a, b, tol) { return Math.abs(a - b) <= tol; }

// ================================================================ 1. ecefToGeodeticDeg
// Hand-derivable special cases (pure geometry, no formula needed): a point on the
// equatorial plane at height 0 with x=0, y>0 must be at lon=90, lat=0 by the ellipsoid's
// own symmetry (z=0 <=> lat=0 for a spheroid flattened about Z; lon is atan2(y,x)
// independent of the ellipsoid entirely, per geodeticToEcef's own module comment) --
// this is NOT a round-trip through geodeticToEcef, it is checked directly against a
// hand-picked ECEF point. Fails against an implementation that transposes lon/lat, or
// one that gets the equatorial-radius/polar-radius roles backwards.
{
  const eq = ecefToGeodeticDeg(0, WGS84_A_M, 0);
  check('ecefToGeodeticDeg: equatorial point on +Y axis is lon=90, lat=0 (hand-derived, not round-tripped)',
    approxEqual(eq.lonDeg, 90, 1e-9) && approxEqual(eq.latDeg, 0, 1e-9) && approxEqual(eq.heightM, 0, 1e-6));
  const negX = ecefToGeodeticDeg(-WGS84_A_M, 0, 0);
  check('ecefToGeodeticDeg: equatorial point on -X axis is lon=180 or -180, lat=0',
    (approxEqual(Math.abs(negX.lonDeg), 180, 1e-9)) && approxEqual(negX.latDeg, 0, 1e-9));
  const northPole = ecefToGeodeticDeg(0, 0, WGS84_B_M);
  check('ecefToGeodeticDeg: north pole (0,0,polar radius) is lat=90 (hand-derived from ellipsoid symmetry)',
    approxEqual(northPole.latDeg, 90, 1e-9) && approxEqual(northPole.heightM, 0, 1e-6));
  const southPole = ecefToGeodeticDeg(0, 0, -WGS84_B_M);
  check('ecefToGeodeticDeg: south pole is lat=-90',
    approxEqual(southPole.latDeg, -90, 1e-9));
  // Sanity round-trip (secondary -- the special cases above are the load-bearing proof;
  // this only guards against a gross regression across both functions together).
  const rt = ecefToGeodeticDeg(...Object.values(geodeticToEcef(37.5, -12.25, 1000)));
  check('ecefToGeodeticDeg: round-trips geodeticToEcef to within 1e-6 deg / 1e-3 m',
    approxEqual(rt.lonDeg, 37.5, 1e-6) && approxEqual(rt.latDeg, -12.25, 1e-6) && approxEqual(rt.heightM, 1000, 1e-3));
}

// ============================================================== 2. bodyFixedPositionKm
// Trivial wiring case: identity quaternion + zero body position means the body-fixed
// frame IS the scenario frame -- bodyFixedPositionKm must be the identity transform.
// Fails against an implementation that (for instance) forgets to invert the quaternion,
// or applies the quaternion in the wrong order, or forgets to subtract the body's own
// position first (any of these would still show as "identity" bugs against a NON-
// identity case below, but this one isolates "does the plumbing even pass the numbers
// through correctly" first).
{
  const bodyTrack = { t: [0, 1], pos: [0, 0, 0, 0, 0, 0], quat: [0, 0, 0, 1, 0, 0, 0, 1], spinRate: 0 };
  const bf = bodyFixedPositionKm({ x: 100, y: -50, z: 25 }, bodyTrack, 0.5);
  check('bodyFixedPositionKm: identity body quaternion + zero body position is the identity transform',
    approxEqual(bf.x, 100, 1e-9) && approxEqual(bf.y, -50, 1e-9) && approxEqual(bf.z, 25, 1e-9));
}

// ======================================================== 3. real ground-track sample
// The load-bearing check: `groundTrack()`'s output for the REAL, ingested demo run's
// first sample (index 0, demo_flt) against a value hand-computed *in Python*
// (tests/test_viewer_panels.py), independently -- a DIFFERENT ECEF->geodetic algorithm
// (iterative Newton-style fixed point on latitude) than this module's own Bowring
// closed form, and a from-scratch quaternion-vector rotation formula, not a call into
// any of this project's own JS or Python code. Fails against a `bodyFixedPositionKm`/
// `ecefToGeodeticDeg` that get the rotation direction, subtraction order, or km<->m
// conversion wrong -- any of those would move this real sample's lon/lat well outside
// the tolerance below (these are physically ~6871 km positions; a wrong rotation
// direction or missing km->m conversion is not a small error).
{
  const flt = scenario.spacecraft.find((s) => s.name === 'demo_flt');
  const earth = scenario.bodies.find((b) => b.name === 'Earth');
  const track = groundTrack(flt, earth);
  const sample0 = track[0];
  const exp = input.expectedGroundTrack;
  check('groundTrack: real demo_flt sample 0 lonDeg matches independent Python computation',
    approxEqual(sample0.lonDeg, exp.lonDeg, 1e-6));
  check('groundTrack: real demo_flt sample 0 latDeg matches independent Python computation',
    approxEqual(sample0.latDeg, exp.latDeg, 1e-6));
  check('groundTrack: same sample via lonLatFromScenarioFramePosition (single-point API) agrees with the batch API',
    (() => {
      const p = lonLatFromScenarioFramePosition({ x: flt.pos[0], y: flt.pos[1], z: flt.pos[2] }, earth, flt.t[0]);
      return approxEqual(p.lonDeg, sample0.lonDeg, 1e-9) && approxEqual(p.latDeg, sample0.latDeg, 1e-9);
    })());
  check('groundTrack: returns one entry per recorded trajectory sample', track.length === flt.t.length);
}

// =========================================================== 4. objectiveRows / scores
// The real, decoded fixture's own scores (Python attached `scenario.scores`, MessageToDict
// shape -- see this file's own module docstring). Fails against an implementation that
// treats every score as a measure (never sets kind:'objective'), or one that coerces an
// absent `passed` to `false` instead of `null` (a measure of effectiveness would then be
// wrongly shown as a FAILED objective, which ADR-005 sec 6 explicitly distinguishes).
{
  const rows = objectiveRows(scenario.scores);
  check('objectiveRows: exactly 3 rows for the real fixture (demo_flt_rmag_at_end, demo_mvr_rmag_at_end, demo_flt_cd_at_end)',
    rows.length === 3);
  const objective = rows.find((r) => r.name === 'demo_flt_rmag_at_end');
  check('objectiveRows: demo_flt_rmag_at_end is a PASSED objective (real fixture value)',
    !!objective && objective.kind === 'objective' && objective.passed === true);
  const measures = rows.filter((r) => r.kind === 'measure');
  check('objectiveRows: the other two real scores are measures with passed === null (never coerced to false)',
    measures.length === 2 && measures.every((m) => m.passed === null));
  check('objectiveRows: absent scores (e.g. a pre-M26.4b cached scenario) yields an empty array, not a throw',
    Array.isArray(objectiveRows(undefined)) && objectiveRows(undefined).length === 0);
}

// =============================== 4b. measurementRows / measurementCountsBySensor / timelineMeasurementTicks (M25.3e, question 174)
// Synthetic, hand-built input (no real demo_measurements bundle is available in this
// environment -- see tests/test_cdm_run.py's own acceptance-test section for why, and
// tests/test_cdm_run.py's real, server-threaded plumbing tests for the actual
// end-to-end wire proof); this section only proves the PURE binding logic these three
// functions apply on top of whatever `sc.measurements` the server sends, using values
// shaped like -- but not claiming to be -- the demo_measurements DRM's own three ids.
{
  // Ordering is deliberate, not incidental: the FIRST sensor encountered by insertion
  // is 'startracker', the second is 'imu' -- alphabetical order ('imu' < 'startracker')
  // is the OPPOSITE of insertion order, so the measurementCountsBySensor sort-by-name
  // check below only passes for an implementation that actually sorts (an
  // insertion-order-only implementation would report ['startracker', 'imu'] and fail
  // the check's exact-array assertion).
  const measurements = [
    { id: 'altavista.attitude_q4', epoch: 1.5, sensorId: 'startracker', frameId: '', z: [0, 0, 0, 1], r: [] },
    { id: 'altavista.imu_gyro3', epoch: 2.5, sensorId: 'imu', frameId: '', z: [0.05, 0.03, 0.2], r: [1e-8, 0, 0, 0, 1e-8, 0, 0, 0, 1e-8] },
    { id: 'altavista.imu_accel3', epoch: 1.5, sensorId: 'imu', frameId: '', z: [0, 0, 0], r: [1e-6, 0, 0, 0, 1e-6, 0, 0, 0, 1e-6] },
  ];
  const rows = measurementRows(measurements);
  check('measurementRows: sorted by epoch ascending (input was NOT already sorted)',
    rows.map((r) => r.id).join(',') === 'altavista.attitude_q4,altavista.imu_accel3,altavista.imu_gyro3');
  check('measurementRows: id/epoch/sensorId/frameId carried through unchanged',
    rows[0].id === 'altavista.attitude_q4' && rows[0].epoch === 1.5 && rows[0].sensorId === 'startracker' && rows[0].frameId === '');
  check('measurementRows: zLen/rLen are real array lengths, not a fabricated non-zero value for an empty r',
    rows[0].zLen === 4 && rows[0].rLen === 0);
  check('measurementRows: a measurement with a real (non-empty) r reports the real rLen',
    rows.find((r) => r.id === 'altavista.imu_gyro3').rLen === 9);
  check('measurementRows: absent measurements yields an empty array, not a throw',
    Array.isArray(measurementRows(undefined)) && measurementRows(undefined).length === 0);

  const counts = measurementCountsBySensor(measurements);
  check('measurementCountsBySensor: two sensors, correct counts, sorted by sensorId',
    JSON.stringify(counts) === JSON.stringify([{ sensorId: 'imu', count: 2 }, { sensorId: 'startracker', count: 1 }]));
  check('measurementCountsBySensor: a measurement with no sensorId is grouped under an honest placeholder, never silently dropped',
    measurementCountsBySensor([{ sensorId: '' }, {}]).some((c) => c.sensorId === '(no sensorId)' && c.count === 2));

  const ticks = timelineMeasurementTicks(measurements);
  check('timelineMeasurementTicks: one tick per measurement (never merged/deduplicated by epoch -- two ids share epoch 1.5 here)',
    ticks.length === 3 && ticks.filter((t) => t.t === 1.5).length === 2);
  check('timelineMeasurementTicks: t is the measurement\'s own epoch (same field app.js\'s buildTicks places every other tick by)',
    ticks.every((t, i) => t.t === measurements[i].epoch && t.id === measurements[i].id));
}

// ======================================================== 5. timeline-linked events
// Real fixture events (via the live server's own POST /api/cdm/run -- `sc.events`
// already carries `type`, confirmed to include exactly one 'fault' and one
// 'port_command' among the fixture's 10 events, see web/js/REPORT_M26_4.md). Fails
// against an implementation that also includes 'lifecycle'/'maneuver' events (not
// "linked to the timeline" in this panel's own sense -- the brief names fault/port
// command specifically), or one that drops the real `t` value needed to place the
// event correctly on the timeline.
{
  const linked = timelineEvents(scenario.events);
  check('timelineEvents: exactly the fault + port_command events from the real fixture (2 total)',
    linked.length === 2 && linked.some((e) => e.type === 'fault') && linked.some((e) => e.type === 'port_command'));
  check('timelineEvents: excludes lifecycle/maneuver (not this panel\'s "linked to timeline" kinds)',
    !linked.some((e) => e.type === 'lifecycle' || e.type === 'maneuver'));
  const fault = linked.find((e) => e.type === 'fault');
  const rawFault = scenario.events.find((e) => e.type === 'fault');
  check('timelineEvents: the fault event\'s epoch (t) is passed through UNCHANGED from sc.events (linked to the correct epoch)',
    fault.t === rawFault.t);

  const pct = eventTimelinePercent(fault, scenario.t0, scenario.t1);
  const expectedPct = input.expectedFaultTimelinePercent;
  check('eventTimelinePercent: matches hand arithmetic (ev.t - t0) / (t1 - t0) * 100 computed directly from the real t0/t1/ev.t',
    approxEqual(pct, expectedPct, 1e-9));
  check('eventTimelinePercent: degenerate zero-length span returns null, never divides by zero',
    eventTimelinePercent({ t: 5 }, 5, 5) === null);
}

// ==================================================================== 6. console panel
// provenanceLines must surface the real config hash the fixture's own Provenance
// carries (never a placeholder/hardcoded string) -- fails against an implementation
// that drops meta.configHash or renders it under the wrong label.
{
  const lines = provenanceLines(scenario);
  const hashLine = lines.find((l) => l.label === 'config hash');
  check('provenanceLines: real config hash from the fixture\'s Provenance reaches the console panel',
    !!hashLine && hashLine.value === scenario.meta.configHash && hashLine.value.length > 0);
  const runIdLine = lines.find((l) => l.label === 'run id');
  check('provenanceLines: run id present', !!runIdLine && runIdLine.value === scenario.meta.runId);
  check('provenanceLines: absent scenario yields no lines, not a throw', provenanceLines(null).length === 0);
}

// ============================================================ 7. map tiles, no network
// `tileUrlsForLevel`/`mosaicRows` are pure data -- proving "no network request is made"
// at the level this harness CAN prove under plain `node` (no fetch/Image is ever
// constructed by these functions; the browser check separately confirms zero real
// network requests via the Browser pane's network log). Fails against an
// implementation that requests a real (non-fixture) tile host, or gets the tile grid
// size wrong for the declared imagery profile's maxLevel.
{
  const level = 1;
  const urls = tileUrlsForLevel(scenario.imagery.urlTemplate, level);
  check(`map tiles: exactly ${tileCountX(level) * tileCountY(level)} tiles at level ${level} (the globe's own tile-count formula)`,
    urls.length === tileCountX(level) * tileCountY(level));
  check('map tiles: every URL is the offline fixture path (relative, no host -- "no network")',
    urls.every((u) => u.startsWith('./fixtures/tiles/') && !u.includes('://')));
  const rows = mosaicRows(scenario.imagery.urlTemplate, level);
  check('mosaicRows: row 0 (rendered at the top) is the NORTHERNMOST tile row',
    rows[0][0].boundsDeg.north === 90);
  check('mosaicRows: the last row is the SOUTHERNMOST tile row',
    rows[rows.length - 1][0].boundsDeg.south === -90);

  // Hand-derivable projection: the antimeridian/equator corner (lon=-180,lat=0) must
  // land at xPct=0, yPct=50 -- by definition of the linear plate-carree mapping, no
  // call into mosaicRows/tileBoundsDeg needed to know this.
  const p = lonLatToPercent(-180, 0);
  check('lonLatToPercent: (lon=-180, lat=0) is (xPct=0, yPct=50) (hand-derived from the linear projection\'s own definition)',
    approxEqual(p.xPct, 0, 1e-9) && approxEqual(p.yPct, 50, 1e-9));
  const p2 = lonLatToPercent(180, 90);
  check('lonLatToPercent: (lon=180, lat=90) is (xPct=100, yPct=0)',
    approxEqual(p2.xPct, 100, 1e-9) && approxEqual(p2.yPct, 0, 1e-9));
}

// ==================================================================== 8. attachM264Panels
// "Every panel is collapsible and re-tileable" / "both demos ... load into the default
// layout" (docs/ui-rework-plan.md's Exit criteria) needs the 3 new panels actually
// present in the tree the live app renders -- but NOT by changing
// `defaultLayoutForScenario`'s own output shape, which web/js/viewport_check.mjs's
// `test_ordinary_scenario_keeps_pre_m26_3_default` (exactly 2 leaves) and
// `test_default_layout_is_icrf_beside_ric_beside_globe` (exactly 4 leaves) assert by
// name -- regressing either is this task's own explicit "do not regress
// tests/test_viewer_viewport.py" instruction. `attachM264Panels` is therefore a
// SEPARATE wrapper (web/js/layout/layout_manager.js calls it on top of
// `defaultLayoutForScenario`'s result, never inside default_layouts.js's own
// selection functions) -- these checks prove (a) it adds exactly the 3 new leaves on
// top of whatever it wraps, without touching what was already there, and (b) that the
// two pre-existing default-layout functions remain byte-shape-unchanged when called
// directly (the exact regression this task must not introduce).
{
  const base = buildBaseSidebarViewportLayout();
  const baseLeaves = listLeaves(base).map((l) => l.panelId);
  check('attachM264Panels: buildBaseSidebarViewportLayout() itself is UNCHANGED (still exactly sidebar+viewport) -- not regressed by this task',
    baseLeaves.length === 2 && baseLeaves.includes('sidebar') && baseLeaves.includes('viewport'));

  const rpo = buildRpoTripleViewportLayout();
  const rpoLeaves = listLeaves(rpo).map((l) => l.panelId);
  check('attachM264Panels: buildRpoTripleViewportLayout() itself is UNCHANGED (still exactly 4 leaves) -- not regressed by this task',
    rpoLeaves.length === 4);

  const wrapped = attachM264Panels(base);
  const wrappedLeaves = listLeaves(wrapped).map((l) => l.panelId);
  check('attachM264Panels: adds exactly the 3 new panel leaves on top of an unrelated tree, keeping the original leaves intact',
    wrappedLeaves.length === 5 && ['sidebar', 'viewport', RUN_PRODUCTS_PANEL_ID, MAP_PANEL_ID, CONSOLE_PANEL_ID]
      .every((id) => wrappedLeaves.includes(id)));

  const wrappedRpo = attachM264Panels(rpo);
  const wrappedRpoLeaves = listLeaves(wrappedRpo).map((l) => l.panelId);
  check('attachM264Panels: works generically over the RPO 4-leaf tree too (7 leaves total, RPO leaves untouched)',
    wrappedRpoLeaves.length === 7 && rpoLeaves.every((id) => wrappedRpoLeaves.includes(id)));

  check('attachM264Panels: defaultLayoutForScenario() itself (not wrapped) is still exactly the pre-M26.4 2-leaf default for an ordinary scenario',
    (() => {
      const leaves = listLeaves(defaultLayoutForScenario({ imagery: null })).map((l) => l.panelId);
      return leaves.length === 2 && leaves.includes('sidebar') && leaves.includes('viewport');
    })());
}

// =========================================================== 9. server-threaded scores (M26.4b)
// docs/open-questions.md question 165: POST /api/cdm/run now threads RunProducts.scores
// into the published scenario itself (additive `scores` key, `meta.scoresSource`) --
// no Python-side patching here, unlike section 4 above (M26.4's binding-only proof
// against a fixture the server did not yet surface). `input.attitudeControlScenario`
// is the REAL, live-server-published scenario for drms/demo_attitude_control.*
// (frozen tests/fixtures/demo_attitude_control.runproducts.bin), whose real
// controller_pointing_error_at_end objective PASSES against this exact seeded run
// (crates/av-kernel/tests/drm_attitude_control.rs's own pinned assertion) and whose
// controller_seq_at_end measure of effectiveness has no pass criterion at all. Fails
// against a server handler that never threads RunProducts.scores through (scores would
// be {} -- `pointing`/`seq` below would be undefined), that coerces the measure's
// unset `passed` to `false` on the wire (question 165's own literal requirement:
// "passed is null for a measure of effectiveness"), or that never stamps
// meta.scoresSource.
if (input.attitudeControlScenario) {
  const sc2 = input.attitudeControlScenario;
  check('server-threaded scores: meta.scoresSource says RunProducts.scores',
    sc2.meta.scoresSource === 'RunProducts.scores');

  const pointing = sc2.scores && sc2.scores.controller_pointing_error_at_end;
  check('server-threaded scores: wire-level passed is true for the real pointing objective',
    !!pointing && pointing.passed === true);

  const seq = sc2.scores && sc2.scores.controller_seq_at_end;
  check('server-threaded scores: wire-level passed is exactly null (not false/absent) for the measure of effectiveness',
    !!seq && seq.passed === null && 'passed' in seq);

  const rows2 = objectiveRows(sc2.scores);
  const pointingRow = rows2.find((r) => r.name === 'controller_pointing_error_at_end');
  check('server-threaded scores: objectiveRows shows the real pointing objective as a PASSED objective',
    !!pointingRow && pointingRow.kind === 'objective' && pointingRow.passed === true);
  const seqRow = rows2.find((r) => r.name === 'controller_seq_at_end');
  check('server-threaded scores: objectiveRows shows the seq measure with passed === null, never coerced to false',
    !!seqRow && seqRow.kind === 'measure' && seqRow.passed === null);
}

// ============================================== 10. feasibility panel (F3b)
// `input.feasibilitySweep` is the REAL, measured `demo_two_instance_sweep` study fixture
// (web/js/fixtures/feasibility_sweep_fixture.json, generated by
// web/js/fixtures/gen_feasibility_sweep_fixture.py -- see that script's own module
// docstring for exactly which numbers are measured vs. structural placeholders). Every
// check below that needs an EDGE CASE this real 2-axis, no-failure study does not
// exhibit (a 1-axis or 3+-axis grid, a zero-width colour range, a point with no
// aggregate, a failed-but-not-dropped sample) uses a small, hand-built synthetic sweep
// object instead, declared inline right where it is used -- the same "Synthetic,
// hand-built input" convention section 4b above already follows for measurementRows,
// clearly distinguished by comment from the real-fixture checks around it.
if (input.feasibilitySweep) {
  const sweep = input.feasibilitySweep;

  // ---- scoreNames -------------------------------------------------------------------
  check('feasibility scoreNames: real fixture yields exactly the two measured score names, sorted',
    JSON.stringify(scoreNames(sweep)) === JSON.stringify(['demo_flt_rmag_at_end', 'demo_mvr_rmag_at_end']));
  check('feasibility scoreNames: null/absent sweep yields [], never a throw',
    Array.isArray(scoreNames(null)) && scoreNames(null).length === 0 &&
    Array.isArray(scoreNames(undefined)) && scoreNames(undefined).length === 0);

  // ---- gridAxes / axisLevels (grid arity) --------------------------------------------
  check('feasibility gridAxes: real fixture yields exactly the two measured axis keys, sorted',
    JSON.stringify(gridAxes(sweep)) === JSON.stringify(['demo_flt.spacecraft.DragArea', 'event:burn1.dv_x']));
  check('feasibility gridAxes: null sweep yields []', Array.isArray(gridAxes(null)) && gridAxes(null).length === 0);

  const realRows = gridRows(sweep, 'demo_mvr_rmag_at_end');
  check('feasibility axisLevels: real fixture DragArea axis has exactly the 2 measured levels (5.0, 25.0)',
    JSON.stringify(axisLevels(realRows, 'demo_flt.spacecraft.DragArea')) === JSON.stringify([5, 25]));
  check('feasibility axisLevels: real fixture dv_x axis has exactly the 2 measured levels (10.0, 30.0)',
    JSON.stringify(axisLevels(realRows, 'event:burn1.dv_x')) === JSON.stringify([10, 30]));

  // 1-axis synthetic grid: axisLevels/gridRows must not assume a 2nd axis exists.
  {
    const oneAxisSweep = {
      axisKeys: ['a.x'], scoreNames: ['s'],
      points: [
        { pointIndex: 0, axisValues: { 'a.x': 1 }, samples: [] },
        { pointIndex: 1, axisValues: { 'a.x': 2 }, samples: [] },
      ],
      aggregates: [
        { name: 's', pointIndex: 0, draws: 1, mean: 10, stdDev: 0, min: 10, max: 10, passFraction: null },
        { name: 's', pointIndex: 1, draws: 1, mean: 20, stdDev: 0, min: 20, max: 20, passFraction: null },
      ],
    };
    const rows = gridRows(oneAxisSweep, 's');
    check('feasibility 1-axis grid: axisLevels over the single axis has exactly its 2 levels',
      JSON.stringify(axisLevels(rows, 'a.x')) === JSON.stringify([1, 2]));
    check('feasibility 1-axis grid: gridAxes reports exactly 1 axis (the arity render() branches on)',
      gridAxes(oneAxisSweep).length === 1);
  }

  // 3-axis synthetic grid: the documented flat-list fallback -- this check does not
  // (and cannot, headlessly) prove render()'s actual DOM output, but it DOES prove the
  // data this harness can reach (gridAxes' own reported arity) correctly signals "3
  // axes", which is the exact fact render() branches its layout decision on.
  {
    const threeAxisSweep = { axisKeys: ['a.x', 'b.y', 'c.z'], scoreNames: ['s'], points: [], aggregates: [] };
    check('feasibility 3-axis grid: gridAxes reports exactly 3 axes (render()\'s own documented flat-list-fallback trigger)',
      gridAxes(threeAxisSweep).length === 3);
  }

  // ---- gridRows: real fixture aggregates match the manager's own measured/hand-computed values
  {
    const mvrByPoint = new Map(realRows.map((r) => [r.pointIndex, r]));
    // point 0's demo_mvr_rmag_at_end: draws 6895836.508, 6895853.128 -> mean 6895844.818,
    // population stdDev 8.31 (hand arithmetic identical to
    // web/js/fixtures/gen_feasibility_sweep_fixture.py's own computation -- see this
    // harness file's own top-of-section comment for where the draw values themselves
    // came from).
    const p0 = mvrByPoint.get(0);
    check('feasibility gridRows: real fixture point 0 demo_mvr_rmag_at_end mean matches hand arithmetic (6895844.818)',
      !!p0 && !!p0.aggregate && approxEqual(p0.aggregate.mean, 6895844.818, 1e-6));
    check('feasibility gridRows: real fixture point 0 demo_mvr_rmag_at_end stdDev matches hand arithmetic (population, /n -- 8.31)',
      !!p0 && !!p0.aggregate && approxEqual(p0.aggregate.stdDev, 8.31, 1e-6));
    check('feasibility gridRows: real fixture point 0 aggregate.passFraction is null (this study has no Objective, never coerced to 0/false)',
      !!p0 && p0.aggregate.passFraction === null);
    check('feasibility gridRows: real fixture rows are in ascending pointIndex order',
      realRows.every((r, i) => r.pointIndex === i));
    // Across the 4 real points' own measured demo_mvr_rmag_at_end means (point 0:
    // 6895844.818, point 1: 6944507.056, point 2: 6895782.132, point 3: 6946252.7335 --
    // web/js/fixtures/gen_feasibility_sweep_fixture.py's own printed output, computed
    // from the manager's measured per-draw table), point 2 has the SMALLEST mean and
    // point 3 has the LARGEST -> those must get t=0 and t=1 respectively. Hand-derivable
    // directly from the fixture's own measured values, not by calling gridRows a second
    // time on itself.
    check('feasibility gridRows: real fixture point with the smallest mean (point 2) gets t=0',
      approxEqual(mvrByPoint.get(2).t, 0, 1e-9));
    check('feasibility gridRows: real fixture point with the largest mean (point 3) gets t=1',
      approxEqual(mvrByPoint.get(3).t, 1, 1e-9));
  }

  // ---- gridRows: zero-width colour range (BREAK-AND-RESTORE CANDIDATE #1, see
  // web/js/REPORT_F3b.md for the exact captured failure text) -- every point's mean for
  // this score is IDENTICAL, so max === min. Fails against a naive `(mean - lo) / (hi -
  // lo)` implementation with a `0/0 -> NaN` (or, for a t that then flows into
  // heatBucket(), an implementation that special-cases NaN as bucket 0, silently
  // painting a flat-data study as if it had a real minimum).
  {
    const flatSweep = {
      axisKeys: ['a.x'], scoreNames: ['s'],
      points: [
        { pointIndex: 0, axisValues: { 'a.x': 1 }, samples: [] },
        { pointIndex: 1, axisValues: { 'a.x': 2 }, samples: [] },
        { pointIndex: 2, axisValues: { 'a.x': 3 }, samples: [] },
      ],
      aggregates: [
        { name: 's', pointIndex: 0, draws: 1, mean: 42, stdDev: 0, min: 42, max: 42, passFraction: null },
        { name: 's', pointIndex: 1, draws: 1, mean: 42, stdDev: 0, min: 42, max: 42, passFraction: null },
        { name: 's', pointIndex: 2, draws: 1, mean: 42, stdDev: 0, min: 42, max: 42, passFraction: null },
      ],
    };
    const flatRows = gridRows(flatSweep, 's');
    check('feasibility gridRows ZERO-WIDTH RANGE: every point with an identical mean gets t=0.5, never NaN/undefined',
      flatRows.every((r) => r.t === 0.5));
    check('feasibility gridRows ZERO-WIDTH RANGE: t=0.5 buckets to the MIDDLE heat bucket (3 of 0..6), not bucket 0',
      flatRows.every((r) => heatBucket(r.t) === 3));
  }

  // ---- gridRows: no aggregate for a (point, score) pair -- must be t=null, distinguishable
  // from t=0 (never silently coloured as the minimum).
  {
    const gapSweep = {
      axisKeys: ['a.x'], scoreNames: ['s'],
      points: [
        { pointIndex: 0, axisValues: { 'a.x': 1 }, samples: [] },
        { pointIndex: 1, axisValues: { 'a.x': 2 }, samples: [] },
      ],
      // Only point 1 has an aggregate for 's' -- point 0's samples all failed (the
      // real-world case this models, crates/av-sweep/src/aggregate.rs's own "zero
      // contributing draws -> no row at all" rule).
      aggregates: [
        { name: 's', pointIndex: 1, draws: 1, mean: 5, stdDev: 0, min: 5, max: 5, passFraction: null },
      ],
    };
    const gapRows = gridRows(gapSweep, 's');
    const missing = gapRows.find((r) => r.pointIndex === 0);
    const present = gapRows.find((r) => r.pointIndex === 1);
    check('feasibility gridRows NO-AGGREGATE POINT: a point with no aggregate for this score gets aggregate=null, t=null (not 0)',
      !!missing && missing.aggregate === null && missing.t === null);
    check('feasibility gridRows NO-AGGREGATE POINT: t=null is NOT the same bucket as t=0 -- heatBucket distinguishes them',
      heatBucket(missing.t) === 'none' && heatBucket(present.t) !== 'none');
  }

  // ---- heatBucket: boundary cases, hand-derivable from its own documented formula
  // (floor(clamp(t,0,1) * buckets), clamped to buckets-1) -- not round-tripped through gridRows.
  check('feasibility heatBucket: t=0 is bucket 0 (the lowest)', heatBucket(0) === 0);
  check('feasibility heatBucket: t=1 is bucket 6 (the highest of the default 7 -- exactly buckets-1, no off-by-one)', heatBucket(1) === 6);
  check('feasibility heatBucket: t=0.999 is still bucket 6 (floor(0.999*7)=6), not clamped down early',
    heatBucket(0.999) === 6);
  check('feasibility heatBucket: null/undefined/NaN all report the "none" sentinel, never a numeric bucket',
    heatBucket(null) === 'none' && heatBucket(undefined) === 'none' && heatBucket(NaN) === 'none');

  // ---- drawRows: real fixture, ascending drawIndex, values/identifiers carried through unchanged
  {
    const draws0 = drawRows(sweep, 0, 'demo_mvr_rmag_at_end');
    check('feasibility drawRows: real fixture point 0 has exactly 2 draws, ascending drawIndex',
      draws0.length === 2 && draws0[0].drawIndex === 0 && draws0[1].drawIndex === 1);
    check('feasibility drawRows: real fixture point 0 draw 0 value is the measured 6895836.508 (not the aggregate mean)',
      approxEqual(draws0[0].value, 6895836.508, 1e-6));
    check('feasibility drawRows: real fixture point 0 draw 1 value is the measured 6895853.128',
      approxEqual(draws0[1].value, 6895853.128, 1e-6));
    check('feasibility drawRows: real fixture draws carry a non-empty runId/configHash/productsUri through unchanged',
      draws0.every((d) => d.runId.length > 0 && d.configHash.length > 0 && d.productsUri.length > 0));
    check('feasibility drawRows: real fixture draws are openable (non-empty productsUri, none failed)',
      draws0.every((d) => d.openable === true && d.failed === false));
    check('feasibility drawRows: absent pointIndex yields [], never a throw', drawRows(sweep, 999, 'demo_mvr_rmag_at_end').length === 0);
    check('feasibility drawRows: null sweep yields []', drawRows(null, 0, 's').length === 0);
  }

  // ---- drawRows: failed sample is a row, never dropped (BREAK-AND-RESTORE CANDIDATE #2,
  // see web/js/REPORT_F3b.md for the exact captured failure text). Synthetic point: one
  // succeeded draw, one failed draw (error non-empty, empty scores map, empty
  // productsUri -- exactly crates/av-sweep/src/aggregate.rs's own documented shape for a
  // failed sample: "a failed sample's scores map is always empty").
  {
    const mixedSweep = {
      axisKeys: ['a.x'], scoreNames: ['s'],
      points: [{
        pointIndex: 0, axisValues: { 'a.x': 1 },
        samples: [
          { drawIndex: 0, runId: 'r0', configHash: 'c0', seeds: { k: '1' }, scores: { s: { value: 7, unit: 'm', passed: null } }, productsUri: 'runs/r0/products.bin', error: '' },
          { drawIndex: 1, runId: 'r1', configHash: 'c1', seeds: { k: '2' }, scores: {}, productsUri: '', error: 'GMAT exited with status 1: deliberately failed for this check' },
        ],
      }],
      aggregates: [{ name: 's', pointIndex: 0, draws: 1, mean: 7, stdDev: 0, min: 7, max: 7, passFraction: null }],
    };
    const mixedDraws = drawRows(mixedSweep, 0, 's');
    check('feasibility drawRows FAILED-SAMPLE-NOT-DROPPED: both draws are present (2), the failed one is NOT silently removed',
      mixedDraws.length === 2);
    const failedRow = mixedDraws.find((d) => d.drawIndex === 1);
    check('feasibility drawRows FAILED-SAMPLE-NOT-DROPPED: the failed draw reports failed=true with its real error text carried through',
      !!failedRow && failedRow.failed === true && failedRow.error === 'GMAT exited with status 1: deliberately failed for this check');
    check('feasibility drawRows FAILED-SAMPLE-NOT-DROPPED: the failed draw has value=null (never a fabricated 0) and is not openable',
      !!failedRow && failedRow.value === null && failedRow.openable === false);
    check('feasibility drawRows FAILED-SAMPLE-NOT-DROPPED: isSampleOpenable independently agrees the failed sample (empty productsUri) is not openable',
      isSampleOpenable({ productsUri: '' }) === false && isSampleOpenable({ productsUri: 'x' }) === true &&
      isSampleOpenable(null) === false);
    const okRow = mixedDraws.find((d) => d.drawIndex === 0);
    check('feasibility drawRows FAILED-SAMPLE-NOT-DROPPED: the succeeded draw in the SAME point is unaffected (value/openable correct)',
      !!okRow && okRow.failed === false && okRow.value === 7 && okRow.openable === true);
  }

  // ---- defaultScoreName (F5.1, question 197) ----------------------------------------
  // The real fixture's own two scores (demo_flt_rmag_at_end, demo_mvr_rmag_at_end) BOTH
  // vary across the grid (means 6870530.237/6870507.9725/6870483.4115/6870369.12 and
  // 6895844.818/6944507.056/6895782.132/6946252.7335 respectively -- verified directly
  // against the fixture, not assumed) -- the round's own brief claimed the fixture's
  // first score is constant, which is NOT true of the committed fixture; the check right
  // below only PINS what the real fixture actually does (both the old names[0] rule and
  // the new "first varying" rule agree here, so this one check alone cannot distinguish
  // them -- see the VARYING-OVER-ALPHABETICAL check further down for the check that can).
  check('feasibility defaultScoreName: real fixture (both scores vary) returns the first sorted, first varying name (demo_flt_rmag_at_end) -- pinned, not assumed',
    defaultScoreName(sweep) === 'demo_flt_rmag_at_end');

  // The actual discriminating case: a hand-built sweep where the FIRST sorted score is
  // constant across the grid and a LATER one varies. Fails against the old
  // `names.includes(selectedScore) ? selectedScore : (names[0] || null)` rule (would
  // return 'a_constant'), passes only for an implementation that actually inspects each
  // score's own values.
  {
    const constantThenVaryingSweep = {
      axisKeys: ['a.x'], scoreNames: ['a_constant', 'b_varies'],
      points: [
        { pointIndex: 0, axisValues: { 'a.x': 1 }, samples: [] },
        { pointIndex: 1, axisValues: { 'a.x': 2 }, samples: [] },
      ],
      aggregates: [
        { name: 'a_constant', pointIndex: 0, draws: 1, mean: 10, stdDev: 0, min: 10, max: 10, passFraction: null },
        { name: 'a_constant', pointIndex: 1, draws: 1, mean: 10, stdDev: 0, min: 10, max: 10, passFraction: null },
        { name: 'b_varies', pointIndex: 0, draws: 1, mean: 5, stdDev: 0, min: 5, max: 5, passFraction: null },
        { name: 'b_varies', pointIndex: 1, draws: 1, mean: 7, stdDev: 0, min: 7, max: 7, passFraction: null },
      ],
    };
    check('feasibility defaultScoreName VARYING-OVER-ALPHABETICAL: first sorted score (a_constant) is constant across the grid, second (b_varies) varies -- returns the varying one, not names[0]',
      defaultScoreName(constantThenVaryingSweep) === 'b_varies');
  }

  // Every score constant (including the degenerate single-point-grid case, which is
  // trivially "constant") -- must fall back to the first sorted name, never null.
  {
    const allConstantSweep = {
      axisKeys: ['a.x'], scoreNames: ['a_constant', 'b_constant'],
      points: [
        { pointIndex: 0, axisValues: { 'a.x': 1 }, samples: [] },
        { pointIndex: 1, axisValues: { 'a.x': 2 }, samples: [] },
      ],
      aggregates: [
        { name: 'a_constant', pointIndex: 0, draws: 1, mean: 10, stdDev: 0, min: 10, max: 10, passFraction: null },
        { name: 'a_constant', pointIndex: 1, draws: 1, mean: 10, stdDev: 0, min: 10, max: 10, passFraction: null },
        { name: 'b_constant', pointIndex: 0, draws: 1, mean: 20, stdDev: 0, min: 20, max: 20, passFraction: null },
        { name: 'b_constant', pointIndex: 1, draws: 1, mean: 20, stdDev: 0, min: 20, max: 20, passFraction: null },
      ],
    };
    check('feasibility defaultScoreName ALL-CONSTANT: every score constant across the grid falls back to the first sorted name, never null',
      defaultScoreName(allConstantSweep) === 'a_constant');

    const singlePointSweep = {
      axisKeys: ['a.x'], scoreNames: ['s'],
      points: [{ pointIndex: 0, axisValues: { 'a.x': 1 }, samples: [] }],
      aggregates: [{ name: 's', pointIndex: 0, draws: 1, mean: 3, stdDev: 0, min: 3, max: 3, passFraction: null }],
    };
    check('feasibility defaultScoreName SINGLE-POINT GRID: only one point overall -- cannot show variation, falls back to the first sorted name',
      defaultScoreName(singlePointSweep) === 's');
  }

  // A score with fewer than two points carrying an aggregate cannot be shown to vary --
  // treated like a constant score for selection purposes and skipped in favor of a
  // later score that does demonstrably vary.
  {
    const sparseThenVaryingSweep = {
      axisKeys: ['a.x'], scoreNames: ['a_sparse', 'b_varies'],
      points: [
        { pointIndex: 0, axisValues: { 'a.x': 1 }, samples: [] },
        { pointIndex: 1, axisValues: { 'a.x': 2 }, samples: [] },
      ],
      aggregates: [
        // a_sparse has an aggregate at only ONE of the two points (e.g. every draw at
        // the other point failed for this score) -- a single value cannot vary against
        // anything.
        { name: 'a_sparse', pointIndex: 0, draws: 1, mean: 99, stdDev: 0, min: 99, max: 99, passFraction: null },
        { name: 'b_varies', pointIndex: 0, draws: 1, mean: 1, stdDev: 0, min: 1, max: 1, passFraction: null },
        { name: 'b_varies', pointIndex: 1, draws: 1, mean: 2, stdDev: 0, min: 2, max: 2, passFraction: null },
      ],
    };
    check('feasibility defaultScoreName FEWER-THAN-TWO-AGGREGATE-POINTS: a_sparse has an aggregate at only one point (cannot be shown to vary) -- skipped in favor of b_varies',
      defaultScoreName(sparseThenVaryingSweep) === 'b_varies');
  }

  check('feasibility defaultScoreName: null/undefined/malformed sweep returns null, never throws',
    defaultScoreName(null) === null && defaultScoreName(undefined) === null && defaultScoreName({}) === null);
}

// =============================== 11. feasibility panel against the SERVER'S OWN output (F3c join)
// `input.serverFeasibilitySweep` is NOT the hand-authored `feasibility_sweep_fixture.json`
// section 10 above drives -- it is `scenario["sweep"]` read back from a REAL, live
// `POST /api/cdm/sweep` publish of the real, frozen
// `tests/fixtures/demo_two_instance_sweep.sweepresults.bin` (tests/test_feasibility_join.py,
// the point of that file: "the panel has so far only been proven against a hand-authored
// fixture, never against the server's actual payload"). This section cannot reuse section
// 10's own hardcoded expectations verbatim -- the real study declares a THIRD score
// (`demo_flt_cd_at_end`) the hand-authored fixture never included, so e.g. "exactly two
// score names" does not hold here -- every expectation below is instead handed in
// independently by tests/test_feasibility_join.py, computed directly off the decoded
// `run_pb2.SweepResults` protobuf (never by calling `altavista.server._sweep_results_to_dict`,
// the very function under test), the same "hand-compute independently, pass in as
// input.expectedXxx" convention tests/test_viewer_panels.py already uses for ground-track/
// timeline-percent above. Only runs when the Python side actually supplies it (this key is
// absent for every OTHER caller of this harness, section 10's own test file included).
if (input.serverFeasibilitySweep) {
  const sweep = input.serverFeasibilitySweep;

  check('server-sweep scoreNames: real server-published sweep matches the independently-computed score-name set (3 names, including the real demo_flt_cd_at_end the hand-authored fixture never had)',
    JSON.stringify(scoreNames(sweep)) === JSON.stringify(input.expectedServerScoreNames));

  check('server-sweep gridAxes: real server-published sweep matches the independently-computed axis-key set',
    JSON.stringify(gridAxes(sweep)) === JSON.stringify(input.expectedServerAxisKeys));

  const serverRows = gridRows(sweep, 'demo_mvr_rmag_at_end');
  for (const axisKey of input.expectedServerAxisKeys) {
    check(`server-sweep axisLevels: real server-published '${axisKey}' levels match the independently-computed set`,
      JSON.stringify(axisLevels(serverRows, axisKey)) === JSON.stringify(input.expectedServerAxisLevels[axisKey]));
  }

  const p0 = serverRows.find((r) => r.pointIndex === 0);
  check('server-sweep gridRows: real server-published point 0 demo_mvr_rmag_at_end mean matches the independently-computed population mean',
    !!p0 && !!p0.aggregate && approxEqual(p0.aggregate.mean, input.expectedServerPoint0Mvr.mean, 1e-6));
  check('server-sweep gridRows: real server-published point 0 demo_mvr_rmag_at_end stdDev matches the independently-computed population stdDev',
    !!p0 && !!p0.aggregate && approxEqual(p0.aggregate.stdDev, input.expectedServerPoint0Mvr.stdDev, 1e-6));
  check('server-sweep gridRows: real server-published point 0 aggregate.passFraction is null (this study has no Objective)',
    !!p0 && p0.aggregate.passFraction === null);
  check('server-sweep gridRows: real server-published rows are in ascending pointIndex order',
    serverRows.every((r, i) => r.pointIndex === i));

  const draws0 = drawRows(sweep, 0, 'demo_mvr_rmag_at_end');
  check('server-sweep drawRows: real server-published point 0 has exactly the independently-decoded draw count, ascending drawIndex',
    draws0.length === input.expectedServerPoint0MvrDraws.length &&
    draws0.every((d, i) => d.drawIndex === i));
  check('server-sweep drawRows: real server-published point 0 per-draw values match the independently-decoded RunProducts.samples values exactly (not the aggregate mean)',
    draws0.every((d, i) => approxEqual(d.value, input.expectedServerPoint0MvrDraws[i], 1e-6)));
  check('server-sweep drawRows: every real server-published draw across every point is openable (this study fixture has no failed sample) and carries a non-empty productsUri/runId',
    (sweep.points || []).every((p) => (p.samples || []).every((s) => {
      const row = drawRows(sweep, p.pointIndex, 'demo_mvr_rmag_at_end').find((d) => d.drawIndex === s.drawIndex);
      return row && row.openable === true && row.failed === false && row.productsUri.length > 0 && row.runId.length > 0;
    })));
  check('server-sweep isSampleOpenable: independently agrees on a real sample object taken straight out of the server-published sweep',
    isSampleOpenable(sweep.points[0].samples[0]) === true);
}

const allPass = checks.every((c) => c.pass);
process.stdout.write(JSON.stringify({ allPass, checks }));
process.exit(allPass ? 0 : 1);
