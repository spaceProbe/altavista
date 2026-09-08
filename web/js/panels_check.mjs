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

const allPass = checks.every((c) => c.pass);
process.stdout.write(JSON.stringify({ allPass, checks }));
process.exit(allPass ? 0 : 1);
