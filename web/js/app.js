// altavista viewer: glue between the network link, the scene and the controls.
import { Viewer } from './scene.js';
import { Net } from './net.js';
import { formatScenarioInfo, frameOptionLabel, hudText, viewportPaneTitle } from './cdm_run.js';
// M25.3d (docs/sil-plan.md's M25 milestone: "telemetry into the viewer"): contact
// windows and command-state transitions get their own timeline treatment instead of
// the generic opaque-label tick every event kind used to get -- see
// web/js/timeline_events.js's own module doc comment for exactly what real,
// Rust-emitted CDM data this recovers from `detail` and why (`reference_id`/
// `provenance` never reach the viewer's own `Event` wire shape).
import { timelineTickPlan } from './timeline_events.js';
// M26.3 (docs/ui-rework-plan.md): multiple 3D viewports. hasRicFrame() is the same,
// real detection web/js/layout/default_layouts.js's defaultLayoutForScenario() already
// uses to pick the ICRF/RIC/globe triple-viewport default layout -- reused here (not
// re-derived) to decide whether to actually populate those three panes with live
// Viewport instances once a scenario loads into them.
import { hasRicFrame, ICRF_PANEL_ID, RIC_PANEL_ID, GLOBE_PANEL_ID } from './layout/default_layouts.js';
// M26.4 (docs/ui-rework-plan.md): the three panels. Each module's render() is pure DOM-
// building from data app.js already has, including `sc.scores` -- as of M26.4b
// (docs/open-questions.md question 165, web/js/REPORT_M26_4b.md) `altavista/server.py`'s
// POST /api/cdm/run threads `RunProducts.scores` into every published scenario, so
// `sc.scores` is a real (possibly empty) object for every scenario the server publishes.
// M25.3e (question 174): `sc.measurements` is the same story -- a real (possibly
// empty) list of {id, epoch, sensorId, frameId, z, r} for every scenario the server
// publishes (drms/M25_3E_REPORT.md).
import { render as renderRunProducts, timelineMeasurementTicks } from './panels/run_products_panel.js';
import { render as renderMap } from './panels/map_panel.js';
import { render as renderConsole } from './panels/console_panel.js';
// F3b (docs/feasibility-plan.md's F3 milestone): the feasibility-study panel. `sc.sweep`
// is real, wire-published data only for a scenario worker A's `POST /api/cdm/sweep`
// route published (`meta.sweepSource === 'SweepResults'`) -- `undefined` for every other
// scenario, which feasibility_panel.js's own render() turns into an honest "no study in
// this scenario" notice, never invented data (see that module's own top comment).
import { render as renderFeasibility } from './panels/feasibility_panel.js';

const SEC_PER_DAY = 86400;
const SPEEDS = [
  [1, '1x (real time)'], [10, '10x'], [60, '1 min/s'], [600, '10 min/s'], [3600, '1 h/s'],
  [21600, '6 h/s'], [86400, '1 day/s'], [604800, '1 week/s'], [2592000, '30 days/s'],
];

const $ = (id) => document.getElementById(id);
const els = {
  canvas: $('canvas'), labels: $('labels'), empty: $('empty'), hud: $('hud'), conn: $('conn'),
  scenarioSelect: $('scenario-select'), scenarioInfo: $('scenario-info'),
  scList: $('sc-list'), bodyList: $('body-list'), eventList: $('event-list'),
  frameSelect: $('frame-select'), frameOriginList: $('frame-origin-list'),
  focusSelect: $('focus-select'), trail: $('opt-trail'), labelsOpt: $('opt-labels'), axes: $('opt-axes'),
  grid: $('opt-grid'), stars: $('opt-stars'), origin: $('opt-origin'), sync: $('opt-sync'), reset: $('btn-reset'),
  play: $('btn-play'), start: $('btn-start'), speed: $('speed'), slider: $('slider'), ticks: $('event-ticks'), epoch: $('epoch'),
  globe: $('opt-globe'), loadTiles: $('btn-load-3dtiles'), globeInfo: $('globe-info'),
  // M26.4: the three panels' pane content (index.html's #panel-run-products/-map/-console).
  runProductsPanel: $('panel-run-products'), mapPanel: $('panel-map'), consolePanel: $('panel-console'),
  // F3b: the feasibility-study panel's pane content (index.html's #panel-feasibility).
  feasibilityPanel: $('panel-feasibility'),
};

// M26.4: the console/log panel accumulates a message log across the whole page lifetime
// (unlike run-products/map, which re-render fresh per scenario load) -- built once,
// here, before `net.connect()` below so no early status/message callback is missed.
const consolePanelHandles = renderConsole(els.consolePanel);

// M26.3: the three extra 3D-viewport panes (index.html's #viewport-icrf/-ric/-globe,
// web/js/layout/default_layouts.js's ICRF/RIC/GLOBE_PANEL_ID). Kept as plain
// {id, canvas, labels, hud} descriptors -- `setupRpoViewports()` below registers each
// one as a real web/js/viewport.js `Viewport` (via `viewer.addViewport`) the first time
// a scenario that declares a RIC frame loads.
const EXTRA_VIEWPORTS = [
  { id: 'icrf', role: 'icrf', panelId: ICRF_PANEL_ID, canvas: $('canvas-icrf'), labels: $('labels-icrf'), hud: $('hud-icrf') },
  { id: 'ric', role: 'ric', panelId: RIC_PANEL_ID, canvas: $('canvas-ric'), labels: $('labels-ric'), hud: $('hud-ric') },
  { id: 'globe', role: 'globe', panelId: GLOBE_PANEL_ID, canvas: $('canvas-globe'), labels: $('labels-globe'), hud: $('hud-globe') },
];

// M26.5 (question 167): viewports minted on demand from an empty pane's chooser or a
// pane header's swap menu ("3D Viewport" -- web/js/layout/layout_manager.js's
// REGISTERED_PANEL_TYPES) -- unlike EXTRA_VIEWPORTS above (a fixed pool of three,
// pre-declared in index.html for the RPO default layout), each of these gets a
// brand-new canvas/labels/hud DOM triple built here, on the fly, and its own
// independent `Viewport` (own camera, frame, focus and floating origin --
// web/js/viewport.js's whole module docstring on why that isolation matters for the
// RPO precision figure). Tracked the same shape as EXTRA_VIEWPORTS entries so the
// per-frame HUD/title update loop below treats both uniformly.
const dynamicViewports = [];

function mintViewport(panelId) {
  const wrapper = document.createElement('div');
  wrapper.className = 'av-viewport3d';
  const canvas = document.createElement('canvas');
  const labels = document.createElement('div');
  labels.className = 'labels';
  const hud = document.createElement('div');
  hud.className = 'av-viewport3d-hud mono small';
  wrapper.append(canvas, labels, hud);

  viewer.addViewport(panelId, canvas, labels);
  new ResizeObserver(() => viewer.resize()).observe(canvas);
  // Picking resolves against THIS viewport's own camera, exactly like the pre-declared
  // extra viewports below (registerExtraViewportsOnce()'s own comment on why the
  // camera argument matters).
  canvas.addEventListener('dblclick', (ev) => {
    const vp = viewer.viewports.get(panelId);
    if (!vp) return;
    const rect = canvas.getBoundingClientRect();
    const ndcX = ((ev.clientX - rect.left) / rect.width) * 2 - 1;
    const ndcY = -(((ev.clientY - rect.top) / rect.height) * 2 - 1);
    const hit = viewer.pick(vp.camera, ndcX, ndcY);
    if (hit) viewer.setViewportFocus(panelId, hit.name);
  });

  dynamicViewports.push({ id: panelId, panelId, hud });
  return wrapper;
}
// Registered here (not in web/js/layout_bootstrap.js, which constructs the
// LayoutManager before `viewer` exists) -- layout_bootstrap.js passes an empty
// `panelFactories` object and this assigns into that SAME object by reference, so
// LayoutManager's already-live registry picks it up with no further wiring.
if (window.altavistaLayoutManager) window.altavistaLayoutManager.panelFactories.viewport = mintViewport;

const viewer = new Viewer(els.canvas, els.labels);
const clock = { t: 0, t0: 0, t1: 1, playing: false, speed: 60, t0Iso: null };
let scenario = null;

// F3b: the feasibility panel's own selection state -- owned by app.js (the caller), not
// feasibility_panel.js itself, exactly the same split run_products_panel.js's
// `onJumpToEvent` pattern already uses (that module holds no mutable state of its own
// either). Reset per scenario load (renderFeasibilityPanel() below is called with a
// fresh `sc` every time loadScenario() runs) so a stale selectedPoint from a PREVIOUS
// study is never carried into a different one.
const feasibilityState = { selectedScore: null, selectedPoint: null };

function renderFeasibilityPanel(sc) {
  renderFeasibility(els.feasibilityPanel, {
    sweep: sc.sweep,
    selectedScore: feasibilityState.selectedScore,
    selectedPoint: feasibilityState.selectedPoint,
    onSelectScore: (name) => { feasibilityState.selectedScore = name; renderFeasibilityPanel(sc); },
    onSelectPoint: (pointIndex) => { feasibilityState.selectedPoint = pointIndex; renderFeasibilityPanel(sc); },
    onOpenSample: (drawRow) => openFeasibilitySample(sc, drawRow),
  });
}

// F3b/F3c: "opens any sample's run in the existing viewer through its products_uri"
// (docs/feasibility-plan.md's F3 milestone, verbatim) -- the client-side half. F3b's
// first attempt at this tried to `fetch(drawRow.productsUri)` straight from the browser
// then POST the bytes to `POST /api/cdm/run`; that could never work, for two independent
// reasons verified against the source (F3c, this task's own defect fix): (1)
// `SweepSample.products_uri` is the sample's DIRECTORY, not a file
// (crates/av-sweep/src/bin/av-sweep/study.rs's `finalize()` sets it to
// `std::fs::canonicalize(&r.sample_dir)`) -- the real `RunProducts` bytes are at
// `<products_uri>/run_products.pb`; (2) it is an absolute LOCAL FILESYSTEM PATH on
// whichever machine ran the study, e.g. `/private/var/folders/.../sample_p0_d0` -- a
// browser `fetch()` of that string resolves it against the page's own origin and asks
// *this* HTTP server for that path, which serves nothing.
//
// The fix: the server now does the reading. `POST /api/cdm/sweep/sample`
// (altavista/server.py) takes only an identity -- `{sweepId, pointIndex, drawIndex}`,
// never a path -- resolves it against its own already-published sweep scenario (the one
// `POST /api/cdm/sweep` put in the hub), reads the `productsUri` IT ITSELF RECORDED for
// that sample, decodes `<productsUri>/run_products.pb` and publishes the resulting run
// scenario exactly like `POST /api/cdm/run` does (see that route's own docstring for the
// full "why an identity, never a path" reasoning). This function supplies `sweepId` from
// `sc.sweep.sweepId` and `pointIndex` from `feasibilityState.selectedPoint` (both already
// known to app.js, the caller -- `feasibility_panel.js`'s own `onOpenSample(drawRow)`
// callback shape is unchanged, so `drawRow` only needs to carry its own `drawIndex`,
// which `drawRows()` already returns); `sc` is threaded in from `renderFeasibilityPanel`
// above rather than read off module-level state, so this never accidentally opens a
// sample against a STALE previously-loaded scenario's sweep.
//
// Selects the resulting published scenario by name exactly like choosing it from the
// scenario dropdown does (`els.scenarioSelect`'s own 'change' handler below, same
// `net.send` call). Never silently swallows a failure -- a fetch/publish error surfaces
// visibly (mirrors LayoutManager._showError's "never silently lost" posture elsewhere in
// this codebase), since this is a rare, user-initiated action, not a hot path worth a
// quieter failure mode. Keeps the existing rule that a failed sample is not openable:
// `drawRow.openable` is `feasibility_panel.js`'s own `isSampleOpenable()` result (the one
// place that rule is decided -- see that module's own doc comment), so this never
// re-derives "openable" from `productsUri`/`error` a second time.
async function openFeasibilitySample(sc, drawRow) {
  if (!drawRow || !drawRow.openable) return; // render() already hides this control when non-openable; defensive no-op otherwise.
  const sweepId = sc && sc.sweep && sc.sweep.sweepId;
  const pointIndex = feasibilityState.selectedPoint;
  // Defensive: onOpenSample is only reachable from a rendered draw row, which only ever
  // exists once a study is loaded (sc.sweep) and a grid point is selected -- but this
  // never assumes that silently.
  if (!sweepId || pointIndex === null || pointIndex === undefined) return;
  try {
    const publishResp = await fetch('/api/cdm/sweep/sample', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ sweepId, pointIndex, drawIndex: drawRow.drawIndex }),
    });
    if (!publishResp.ok) {
      const detail = await publishResp.json().catch(() => null);
      const reason = detail && typeof detail.detail === 'string' ? detail.detail : `HTTP ${publishResp.status}`;
      throw new Error(`POST /api/cdm/sweep/sample: ${reason}`);
    }
    const { name } = await publishResp.json();
    net.send({ type: 'select', name });
  } catch (e) {
    window.alert(`Could not open this sample's run (${drawRow.runId}): ${e.message}`);
  }
}
let lastFrame = performance.now();
let lastClockSend = 0;
let suppressSync = false;
let lastMapRenderWall = 0; // M26.4: throttles the 2D map panel's current-position redraw

// ---------------------------------------------------------------- network
const net = new Net({
  onStatus: (ok) => {
    els.conn.textContent = ok ? 'connected' : 'offline';
    els.conn.className = 'badge ' + (ok ? 'on' : 'off');
    consolePanelHandles.setConnection(ok); // M26.4: console/log panel's own connection badge
  },
  onMessage: (msg) => {
    consolePanelHandles.logMessage(msg); // M26.4: every raw server message reaches the log, first
    if (msg.type === 'scenario') loadScenario(msg.scenario);
    else if (msg.type === 'list') fillScenarioList(msg.names);
    else if (msg.type === 'clock') applyRemoteClock(msg);
    else if (msg.type === 'removed' && scenario && msg.name === scenario.name) { /* keep showing until replaced */ }
  },
});
net.connect();

function fillScenarioList(names) {
  const cur = scenario ? scenario.name : null;
  els.scenarioSelect.innerHTML = '';
  for (const n of names) {
    const o = document.createElement('option');
    o.value = n; o.textContent = n; if (n === cur) o.selected = true;
    els.scenarioSelect.appendChild(o);
  }
}
els.scenarioSelect.addEventListener('change', () => net.send({ type: 'select', name: els.scenarioSelect.value }));

// ---------------------------------------------------------------- scenario
function loadScenario(sc) {
  const sameName = scenario && scenario.name === sc.name;
  const prevRel = sameName && clock.t1 > clock.t0 ? (clock.t - clock.t0) / (clock.t1 - clock.t0) : 0;
  scenario = sc;
  // M26.2: the windowing core's "default layout per profile" (question 161) is keyed
  // by the active profile's imagery config, which only becomes known once a scenario
  // is actually loaded (see web/js/layout/default_layouts.js's module docstring).
  // window.altavistaLayoutManager is set up by web/js/layout_bootstrap.js, loaded
  // before this module; it is a no-op once the user has customized or a real saved
  // layout was loaded (LayoutManager.applyDefaultForScenario's own guard, M26.3).
  window.altavistaCurrentScenario = sc;
  // M26.3: the default layout is a function of the whole scenario now (an RPO-shaped
  // scenario -- one that declares a RIC frame -- gets the ICRF/RIC/globe triple-viewport
  // default; web/js/layout/default_layouts.js's defaultLayoutForScenario()), not only
  // imagery, so the whole `sc` is passed rather than just `sc.imagery`.
  if (window.altavistaLayoutManager) window.altavistaLayoutManager.applyDefaultForScenario(sc);
  els.empty.style.display = 'none';
  viewer.setScenario(sc); // clear()s and drops any previous scenario's globe/3D-Tiles-overlay state (see scene.js)
  els.globe.checked = false;
  els.globeInfo.textContent = '';
  setupRpoViewports(sc);
  clock.t0 = sc.t0 ?? 0;
  clock.t1 = sc.t1 ?? clock.t0 + 1;
  clock.t0Iso = sc.t0Iso;
  clock.t = clock.t0 + (clock.t1 - clock.t0) * Math.min(Math.max(prevRel, 0), 1);
  reanchor();
  if (!sameName) pickDefaultSpeed();
  fillScenarioList([...els.scenarioSelect.options].map(o => o.value).includes(sc.name)
    ? [...els.scenarioSelect.options].map(o => o.value) : [...[...els.scenarioSelect.options].map(o => o.value), sc.name]);
  els.scenarioSelect.value = sc.name;
  const days = (clock.t1 - clock.t0);
  // M16.3: formatScenarioInfo (web/js/cdm_run.js) additively appends the run's own config
  // hash when the scenario carries one (sc.meta.configHash, set by POST /api/cdm/run) --
  // every other publishing path is unaffected (see that function's own doc comment).
  els.scenarioInfo.textContent = formatScenarioInfo(sc, fmtDuration(days * SEC_PER_DAY));
  buildLists(sc);
  buildTicks(sc);
  viewer.setOptions(currentOptions());
  viewer.setFocus(null);
  updateTimeUI();
  // M26.4b (question 165): `sc.scores` is the real, server-threaded RunProducts.scores
  // for a POST /api/cdm/run scenario (`{}` for every other publish path or a run that
  // declared no scores) -- run_products_panel.js's own render() shows an honest "no
  // objectives or measures" notice only for that empty case, never invented data.
  renderRunProducts(els.runProductsPanel, {
    scores: sc.scores, events: sc.events, measurements: sc.measurements, t0: clock.t0, t1: clock.t1,
    onJumpToEvent: (ev) => setTime(ev.t, true),
  });
  renderMap(els.mapPanel, { sc, level: 1, t: clock.t });
  lastMapRenderWall = performance.now();
  consolePanelHandles.setProvenance(sc);
  // F3b: a fresh scenario load resets the feasibility panel's own selection state (never
  // carries a selectedPoint from a DIFFERENT study's grid into this one -- see
  // feasibilityState's own comment above) and re-renders against this scenario's own
  // `sc.sweep` (absent/undefined for every non-sweep scenario -- renderFeasibilityPanel
  // -> feasibility_panel.js's render() turns that into an honest notice, not a throw).
  feasibilityState.selectedScore = null;
  feasibilityState.selectedPoint = null;
  renderFeasibilityPanel(sc);
}

function pickDefaultSpeed() {
  const span = (clock.t1 - clock.t0) * SEC_PER_DAY;
  const want = span / 45;   // play the whole scenario in ~45 s
  let best = SPEEDS[0][0];
  for (const [v] of SPEEDS) if (v <= want) best = v;
  clock.speed = best;
  els.speed.value = String(best);
}

function buildLists(sc) {
  els.scList.innerHTML = '';
  els.bodyList.innerHTML = '';
  els.eventList.innerHTML = '';
  els.focusSelect.innerHTML = '';
  els.frameSelect.innerHTML = '';
  els.frameOriginList.innerHTML = '';
  const addFocus = (value, text) => {
    const o = document.createElement('option'); o.value = value; o.textContent = text; els.focusSelect.appendChild(o);
  };
  addFocus('', `${sc.frame.origin} (frame origin)`);
  // Frame graph (M4.1, extended M5.2 with per-entity body frames): the single source
  // of truth is viewer.frameList() (web/js/scene.js), built by the viewer.
  // setScenario(sc) call above -- not re-derived from sc.frames here. This used to
  // duplicate scene.js's "entities frame may be missing from sc.frames, synthesize a
  // root option" logic by hand; viewer.frameList() already reflects exactly what
  // _buildFrameGraph() built (including the synthetic root when needed, and M5.2's
  // body-frame nodes, which have no entry in sc.frames at all), so there is one place
  // that knows the frame graph's real contents. `selected` below (not index/position)
  // is how the scenario's own base frame is marked at load -- M20.2 (question 134/
  // E-27) stopped assuming index 0 is always the entities frame (a CDM-ingested run's
  // frames arrive id-sorted, M18.1's mandatory EarthBodyFixed/EarthICRF/EarthMJ2000Eq
  // set, not entities-frame-first) and, separately, stopped having "Reset view" force
  // the picker back to any particular option at all -- see that handler below.
  for (const fd of viewer.frameList()) {
    const label = frameOptionLabel(fd);
    const o = document.createElement('option');
    o.value = fd.id; o.textContent = label.text; o.title = label.title;
    if (fd.id === sc.frame.name) o.selected = true;
    els.frameSelect.appendChild(o);

    const li = document.createElement('li');
    const cb = document.createElement('input'); cb.type = 'checkbox'; cb.checked = true;
    cb.title = `Floating origin for frame '${fd.id}' (docs/open-questions.md Q46: per-frame, not only global)`;
    cb.addEventListener('change', () => viewer.setFrameOriginEnabled(fd.id, cb.checked));
    const nm = document.createElement('span'); nm.className = 'name'; nm.textContent = label.text; nm.title = label.title;
    // M5.2: a body frame that fell back to nadir-pointing VVLH (no attitude stream)
    // is already labelled in `description`/the tooltip above -- this class just makes
    // it visually distinct too, never the only place the fallback is surfaced.
    if (fd.fallback) nm.classList.add('frame-fallback');
    li.append(cb, nm);
    els.frameOriginList.appendChild(li);
  }
  const item = (kind, obj, color, label) => {
    const li = document.createElement('li');
    const cb = document.createElement('input'); cb.type = 'checkbox'; cb.checked = true;
    cb.addEventListener('change', () => viewer.setVisible(kind, obj.name, cb.checked));
    const sw = document.createElement('span'); sw.className = 'swatch'; sw.style.background = color;
    const nm = document.createElement('span'); nm.className = 'name'; nm.textContent = label;
    const go = document.createElement('span'); go.className = 'go'; go.textContent = 'focus';
    go.addEventListener('click', () => { els.focusSelect.value = obj.name; applyView(); });
    li.append(cb, sw, nm, go);
    return li;
  };
  for (const s of sc.spacecraft) {
    els.scList.appendChild(item('sc', s, s.color || '#fff', `${s.label || s.name} (${s.t.length} pts)`));
    addFocus(s.name, s.label || s.name);
  }
  for (const b of sc.bodies) {
    els.bodyList.appendChild(item('body', b, b.color || '#888', b.name));
    addFocus(b.name, b.name);
  }
  if (!sc.events || sc.events.length === 0) {
    const li = document.createElement('li'); li.className = 'empty'; li.textContent = 'none'; els.eventList.appendChild(li);
  }
  for (const ev of sc.events || []) {
    const li = document.createElement('li');
    const nm = document.createElement('span'); nm.className = 'name';
    nm.textContent = `${ev.name}${ev.spacecraft ? ' · ' + ev.spacecraft : ''}`;
    nm.title = ev.detail || '';
    const go = document.createElement('span'); go.className = 'go'; go.textContent = fmtEpochShort(ev.t);
    go.title = 'jump to event';
    go.addEventListener('click', () => { setTime(ev.t, true); });
    li.append(nm, go);
    els.eventList.appendChild(li);
  }
}

// M25.3d: a contact_start/contact_end pair for the same station/counterpart reads as
// ONE spanned window (not two unrelated point ticks), an unmatched contact event is
// shown distinctly rather than silently dropped or mispaired, and a command_transition
// tick is visually AND textually distinct (real CommandState name) from a contact
// window and from every other event kind. Every other kind's rendering (a single
// 'tick' point positioned at ev.t, titled ev.name) is unchanged -- all of the new
// grouping/labelling logic lives in the pure, headlessly-tested web/js/timeline_events.js
// (`timelineTickPlan`); this function is only the DOM loop over its output.
function buildTicks(sc) {
  els.ticks.innerHTML = '';
  const span = clock.t1 - clock.t0;
  if (span <= 0) return;
  const plan = timelineTickPlan(sc.events || []);

  for (const w of plan.windows) {
    const d = document.createElement('div'); d.className = 'tick tick-contact-window';
    d.style.left = ((w.startT - clock.t0) / span * 100) + '%';
    d.style.width = Math.max((w.endT - w.startT) / span * 100, 0.15) + '%';
    const durationText = fmtDuration((w.endT - w.startT) * SEC_PER_DAY);
    d.title = `contact: ${w.spacecraft || '?'}${w.counterpart ? ' ↔ ' + w.counterpart : ''} · ${durationText}`;
    els.ticks.appendChild(d);
  }
  for (const u of plan.unmatched) {
    const d = document.createElement('div'); d.className = 'tick tick-contact-unmatched';
    d.style.left = ((u.event.t - clock.t0) / span * 100) + '%';
    d.title = `${u.event.name} — unmatched (${u.reason})`;
    els.ticks.appendChild(d);
  }
  for (const p of plan.points) {
    const d = document.createElement('div'); d.className = p.className ? `tick ${p.className}` : 'tick';
    d.style.left = ((p.event.t - clock.t0) / span * 100) + '%';
    d.title = p.label;
    els.ticks.appendChild(d);
  }
  // M25.3e (question 174): one small tick per real measurement epoch (sc.measurements),
  // via the same pure timelineMeasurementTicks() the Run Products panel's own
  // "Telemetry" section is built from -- never a second, independently-derived
  // placement for the same data.
  for (const m of timelineMeasurementTicks(sc.measurements)) {
    const d = document.createElement('div'); d.className = 'tick tick-measurement';
    d.style.left = ((m.t - clock.t0) / span * 100) + '%';
    d.title = `${m.id}${m.sensorId ? ' · ' + m.sensorId : ''}`;
    els.ticks.appendChild(d);
  }
}

// ------------------------------------------------------------------ M26.3: RPO viewports
// True once every extra viewport (web/js/viewport.js instances, one per EXTRA_VIEWPORTS
// entry) has been registered against the shared `viewer` -- registration itself only
// needs to happen once per page load (viewer.setScenario()'s own per-viewport rebuild
// loop, scene.js, keeps them in sync with every later scenario reload); only each
// viewport's ROLE (which frame/focus it shows) needs re-applying on every load that
// declares a RIC frame, since setScenario() resets every registered viewport back to
// the plain entities-frame default (see that method's own comment on why).
let rpoViewportsRegistered = false;

function registerExtraViewportsOnce() {
  if (rpoViewportsRegistered) return;
  for (const v of EXTRA_VIEWPORTS) {
    if (!v.canvas) continue; // defensive: index.html always declares these, but never assume
    viewer.addViewport(v.id, v.canvas, v.labels);
    new ResizeObserver(() => viewer.resize()).observe(v.canvas);
    // "Picking resolves against the correct viewport" (M26.3's own brief) -- a
    // double-click in THIS pane casts its ray through THIS viewport's own camera
    // (viewer.pick()'s whole contract, see scene.js's own docstring on it), never the
    // primary's or another pane's. Single click is left to OrbitControls' own
        // rotate-drag gesture, same as the primary canvas already does implicitly.
    v.canvas.addEventListener('dblclick', (ev) => {
      const vp = viewer.viewports.get(v.id);
      if (!vp) return;
      const rect = v.canvas.getBoundingClientRect();
      const ndcX = ((ev.clientX - rect.left) / rect.width) * 2 - 1;
      const ndcY = -(((ev.clientY - rect.top) / rect.height) * 2 - 1);
      const hit = viewer.pick(vp.camera, ndcX, ndcY);
      if (hit) viewer.setViewportFocus(v.id, hit.name);
    });
  }
  rpoViewportsRegistered = true;
}

// "Default layout for the RPO profile: ICRF beside RIC beside globe" (M26.3's own
// brief) -- gives the three extra panes real roles once a scenario that declares a RIC
// frame loads (hasRicFrame(), the exact same detection default_layouts.js's
// defaultLayoutForScenario() uses to pick that layout in the first place, so "the
// layout is the triple-viewport one" and "these three panes show something real" can
// never disagree). A scenario with no RIC frame leaves the three panes registered but
// unconfigured (harmless -- nothing routes them into view unless the layout itself
// places them, which only the RPO default/an imported layout naming them would do).
function setupRpoViewports(sc) {
  registerExtraViewportsOnce();
  if (!hasRicFrame(sc)) return;
  const ricFrame = (sc.frames || []).find((fd) => fd && fd.axes === 'AXES_KIND_RIC');
  viewer.setViewportFrame('icrf', sc.frame.name, null);
  if (ricFrame) viewer.setViewportFrame('ric', ricFrame.id, null);
  viewer.setViewportFrame('globe', sc.frame.name, null);
  if (viewer.bodies.has('Earth')) {
    const imagery = sc.imagery || {};
    viewer.enableGlobe('Earth', { imageryUrl: imagery.urlTemplate, maxLevel: imagery.maxLevel });
    els.globe.checked = true;
    els.globeInfo.textContent = 'globe: Earth (quadtree WGS84, LOD by camera distance)' + (imagery.attribution ? ` · imagery: ${imagery.attribution}` : '');
  }
}

// ---------------------------------------------------------------- options
function currentOptions() {
  return { labels: els.labelsOpt.checked, axes: els.axes.checked, grid: els.grid.checked, stars: els.stars.checked, trail: els.trail.value };
}
for (const el of [els.labelsOpt, els.axes, els.grid, els.stars, els.trail]) {
  el.addEventListener('change', () => viewer.setOptions(currentOptions()));
}
// Frame graph (M4.1): one control path for "which frame is the camera in" and "which
// object is it looking at" -- setViewFrame() itself reduces to the pre-M4.1
// setFocus() behaviour when frameSelect names the entities frame (the common case),
// and refits (M20.2, question 134/E-27 -- previously only re-aimed, leaving the scene
// out of view on an actual frame switch) when it names a different one, e.g. an
// entity-relative RIC frame declared via Scenario.frame_ric() (examples/05_rpo_ric.py):
// picking the target's RIC frame here, then the chaser as focus, is "focus the
// chaser in the target's RIC frame".
function applyView() {
  viewer.setViewFrame(els.frameSelect.value || els.frameSelect.options[0]?.value, els.focusSelect.value || null);
}
els.frameSelect.addEventListener('change', applyView);
els.focusSelect.addEventListener('change', applyView);
els.reset.addEventListener('click', () => {
  // M20.2 (question 134/E-27): "Reset view" keeps the currently selected frame -- it
  // used to force the picker back to its first option (`selectedIndex = 0`) on every
  // click, so there was no way to land in e.g. ICRF with the scene framed. `viewer.
  // fit()` itself now refits *within* `viewer._cameraFrameId` (whatever frame is
  // currently active, matching what the picker already shows) instead of always
  // reparenting back to the entities frame -- see scene.js's fit()/setViewFrame() for
  // the shared framing math (body-scale-aware for a body-axes frame, question 134's
  // own fix).
  els.focusSelect.value = '';
  viewer.focus = null;
  viewer.fit();
});
// Floating origin: on by default, globally switchable (docs/open-questions.md Q46's
// "switchable globally" half); the per-frame half is the "Frames" section's
// checkbox list (viewer.setFrameOriginEnabled(frameId, enabled), wired in
// buildLists() above) -- both exercise the same viewer.floatingOrigin API.
els.origin.addEventListener('change', () => viewer.setFloatingOriginEnabled(els.origin.checked));

// ------------------------------------------------------------------ M15.4: globe / 3D Tiles
// Both are opt-in (default off) so the existing five examples' default rendering is
// unaffected -- see web/js/scene.js's enableGlobe()/loadTilesOverlay() docstrings.
//
// M19.5 (question 132): the globe's imagery source is a profile setting, not a viewer
// default -- altavista/server.py's Hub.put() stamps the active profile's imagery config
// ({urlTemplate, attribution, maxLevel}, altavista/profile.py) onto every published
// scenario's own `imagery` field, so it rides along with `scenario` exactly like
// `meta`/`frames`/`bodies` already do. This module owns the globe panel (this task's
// brief), so it is the one place both halves land: `imagery.urlTemplate`/`maxLevel`
// configure the actual GlobeLayer (web/js/globe.js's existing opts seam), and
// `imagery.attribution` is surfaced in `globeInfo` -- an imagery source with an
// attribution string nothing displays would not be done.
els.globe.addEventListener('change', () => {
  if (els.globe.checked) {
    const imagery = (scenario && scenario.imagery) || {};
    const ok = viewer.enableGlobe('Earth', { imageryUrl: imagery.urlTemplate, maxLevel: imagery.maxLevel });
    els.globeInfo.textContent = ok
      ? 'globe: Earth (quadtree WGS84, LOD by camera distance)' + (imagery.attribution ? ` · imagery: ${imagery.attribution}` : '')
      : "globe: no 'Earth' body in this scenario";
    if (!ok) els.globe.checked = false;
  } else {
    viewer.disableGlobe();
    els.globeInfo.textContent = '';
  }
});
els.loadTiles.addEventListener('click', () => {
  viewer.loadTilesOverlay('./fixtures/3dtiles/tileset.json');
  els.globeInfo.textContent = (els.globeInfo.textContent ? els.globeInfo.textContent + ' · ' : '') + '3D Tiles fixture loading (overlay)';
});

// ---------------------------------------------------------------- time
for (const [v, label] of SPEEDS) {
  const o = document.createElement('option'); o.value = String(v); o.textContent = label; els.speed.appendChild(o);
}
els.speed.value = String(clock.speed);
els.speed.addEventListener('change', () => { clock.speed = Number(els.speed.value); reanchor(); sendClock(true); });
els.play.addEventListener('click', () => togglePlay());
els.start.addEventListener('click', () => setTime(clock.t0, true));
els.slider.addEventListener('input', () => {
  const rel = Number(els.slider.value);
  setTime(clock.t0 + (clock.t1 - clock.t0) * rel, true);
});
window.addEventListener('keydown', (e) => {
  if (e.target.tagName === 'INPUT' || e.target.tagName === 'SELECT') return;
  if (e.code === 'Space') { e.preventDefault(); togglePlay(); }
  if (e.code === 'Home') setTime(clock.t0, true);
  if (e.code === 'End') setTime(clock.t1, true);
  if (e.code === 'ArrowRight') setTime(clock.t + (clock.t1 - clock.t0) * 0.01, true);
  if (e.code === 'ArrowLeft') setTime(clock.t - (clock.t1 - clock.t0) * 0.01, true);
});

// Playback is anchored to wall-clock time so throttled/hidden tabs stay accurate.
const anchor = { wall: performance.now(), t: 0 };
function reanchor() { anchor.wall = performance.now(); anchor.t = clock.t; }

function togglePlay() {
  clock.playing = !clock.playing;
  if (clock.playing && clock.t >= clock.t1) clock.t = clock.t0;
  reanchor();
  els.play.innerHTML = clock.playing ? '&#10074;&#10074;' : '&#9654;';
  sendClock(true);
}

function setTime(t, fromUser = false) {
  clock.t = Math.min(Math.max(t, clock.t0), clock.t1);
  reanchor();
  updateTimeUI();
  if (fromUser) sendClock(false);
}

function updateTimeUI() {
  const span = clock.t1 - clock.t0;
  els.slider.value = span > 0 ? String((clock.t - clock.t0) / span) : '0';
  els.epoch.textContent = fmtEpoch(clock.t);
  els.play.innerHTML = clock.playing ? '&#10074;&#10074;' : '&#9654;';
}

function sendClock(force) {
  if (!els.sync.checked || suppressSync) return;
  const now = performance.now();
  if (!force && now - lastClockSend < 120) return;
  lastClockSend = now;
  net.send({ type: 'clock', t: clock.t, playing: clock.playing, speed: clock.speed, scenario: scenario ? scenario.name : null });
}

function applyRemoteClock(msg) {
  if (!els.sync.checked || !scenario) return;
  if (msg.scenario && msg.scenario !== scenario.name) return;   // clock belongs to another scenario
  suppressSync = true;
  if (typeof msg.speed === 'number') { clock.speed = msg.speed; els.speed.value = String(msg.speed); }
  if (typeof msg.playing === 'boolean') clock.playing = msg.playing;
  if (typeof msg.t === 'number') setTime(msg.t); else reanchor();
  updateTimeUI();
  suppressSync = false;
}

// ---------------------------------------------------------------- formatting
function fmtEpoch(t) {
  if (!clock.t0Iso) return `A1MJD ${t.toFixed(6)}`;
  const base = Date.parse(clock.t0Iso);
  const d = new Date(base + (t - clock.t0) * SEC_PER_DAY * 1000);
  return d.toISOString().replace('T', ' ').replace('Z', ' UTC');
}
function fmtEpochShort(t) {
  if (!clock.t0Iso) return t.toFixed(3);
  const base = Date.parse(clock.t0Iso);
  const d = new Date(base + (t - clock.t0) * SEC_PER_DAY * 1000);
  return d.toISOString().slice(5, 16).replace('T', ' ');
}
function fmtDuration(sec) {
  if (sec < 3600) return `${(sec / 60).toFixed(1)} min`;
  if (sec < 2 * SEC_PER_DAY) return `${(sec / 3600).toFixed(1)} h`;
  return `${(sec / SEC_PER_DAY).toFixed(1)} days`;
}

// ---------------------------------------------------------------- loop
function frame(now) {
  lastFrame = now;
  if (scenario && clock.playing) {
    clock.t = anchor.t + ((now - anchor.wall) / 1000) * clock.speed / SEC_PER_DAY;
    if (clock.t >= clock.t1) { clock.t = clock.t1; clock.playing = false; reanchor(); sendClock(true); }
    updateTimeUI();
  }
  if (scenario) {
    viewer.update(clock.t);
    // M26.4: redraw the 2D map's current-position markers at ~2 Hz, not every render
    // frame -- renderMap() rebuilds the whole tile mosaic + overlay each call, and
    // there is no functional need to do that 60x/sec for a marker that (at any
    // sensible playback speed) moves a visually tiny amount between two ticks.
    if (now - lastMapRenderWall > 500) {
      lastMapRenderWall = now;
      renderMap(els.mapPanel, { sc: scenario, level: 1, t: clock.t });
    }
    els.hud.textContent = hudText(scenario.name, viewer.viewFrameId, viewer.focus);
    // M26.3: each extra viewport gets its own HUD text, from ITS OWN cameraFrameId/focus
    // -- never the primary's -- exactly like its own labels/picking above.
    for (const v of EXTRA_VIEWPORTS) {
      const vp = viewer.viewports.get(v.id);
      if (vp && v.hud) v.hud.textContent = hudText(scenario.name, vp.cameraFrameId, vp.focus);
    }
    for (const v of dynamicViewports) {
      const vp = viewer.viewports.get(v.id);
      if (vp && v.hud) v.hud.textContent = hudText(scenario.name, vp.cameraFrameId, vp.focus);
    }
  } else {
    viewer.update(0);
  }
  // Question 169: every viewport pane's title derives from the frame it is ACTUALLY
  // showing right now, never a static label baked into the layout -- cheap to call
  // every frame (setPaneTitle() itself no-ops when the text has not changed). `null`
  // (no scenario yet, or a viewport not yet given a role) shows the base "3D View"
  // label alone, never a frame name the viewport cannot back up.
  if (window.altavistaLayoutManager) {
    const lm = window.altavistaLayoutManager;
    lm.setPaneTitle('viewport', viewportPaneTitle('3D View', scenario ? viewer.viewFrameId : null));
    for (const v of EXTRA_VIEWPORTS) {
      const vp = viewer.viewports.get(v.id);
      lm.setPaneTitle(v.panelId, viewportPaneTitle('3D View', vp ? vp.cameraFrameId : null));
    }
    for (const v of dynamicViewports) {
      const vp = viewer.viewports.get(v.id);
      lm.setPaneTitle(v.panelId, viewportPaneTitle('3D View', vp ? vp.cameraFrameId : null));
    }
  }
  requestAnimationFrame(frame);
}
window.addEventListener('resize', () => viewer.resize());
new ResizeObserver(() => viewer.resize()).observe(els.canvas);
requestAnimationFrame(frame);
