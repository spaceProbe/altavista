// altavista viewer: CDM run bundle presentation helpers (M16.3, question 5's first demo
// bridge: "a DRM authored in Python, propagated with GMAT dynamics, shown on the custom
// globe and in ICRF, reproducible from its config hash"). Pure functions only -- no DOM, no
// import of scene.js/net.js/app.js's own event listeners -- so the exact code app.js calls
// can also run headless, under plain `node` (tests/test_cdm_run.py's headless harness,
// web/js/verify_cdm_run.mjs), per this project's established "run the real, shipped code
// under node rather than reimplementing it in Python" testing convention
// (tests/test_viewer_jitter.py's module docstring).

// A scenario published through altavista's POST /api/cdm/run (altavista/server.py) carries the
// run's own config hash additively, at `sc.meta.configHash` (altavista/cdm.py's
// `RunBundle.provenance.config_hash`, itself the DRM's own canonical hash --
// crates/av-kernel/src/drm/hash.rs verified and refused-to-run-if-tampered before the run
// ever executed). Every other publishing path (POST /api/scenario, POST /api/cdm/trajectory)
// never sets `meta.configHash`, so `formatScenarioInfo` for those is byte-identical to the
// pre-M16.3 info line -- this is purely additive, never a shape change for an existing
// scenario.
export function formatScenarioInfo(sc, durationText) {
  const base = `${sc.frame.name} · ${durationText} · ${sc.spacecraft.length} spacecraft`;
  const hash = sc.meta && sc.meta.configHash;
  return hash ? `${base} · config ${hash}` : base;
}

// The `type` of every event on `sc.events`, in list order -- exactly what
// web/js/app.js's `buildTicks`/`buildLists` iterate over to place events on the timeline and
// the event list (both treat `Event.type` as an opaque label already, so no further viewer
// change is needed for a new type string to appear there). A thin, explicit accessor -- so
// a test asserting "the three event kinds reached the viewer's own data model" reads this
// function's real return value rather than reaching into `sc.events` ad hoc and hand-rolling
// the same `.map` at every call site.
export function eventKinds(sc) {
  return (sc.events || []).map((e) => e.type);
}

// M20.2 (question 136, viewer half): the frame picker/"Frames" list option text +
// tooltip for one `viewer.frameList()` entry -- the frame **id** as the visible text,
// never producer prose (the pre-M20.2 bug: `fd.description || fd.id`, showing text
// like "registry default for GMAT CoordinateSystem \"EarthBodyFixed\" ..." or
// "Declared explicitly (question 124): ..."). `description` (when set; the frame's
// own id otherwise, the same fallback `frameList()` already applies) becomes the
// tooltip instead, never the visible label. Deliberately does not depend on any
// particular description *text* -- a concurrent producer-side task is rewriting
// `description` to be a human frame description rather than a process note, and this
// function's own contract (id visible, description in the tooltip) holds either way.
export function frameOptionLabel(fd) {
  return { text: fd.id, title: fd.description || fd.id };
}

// M20.2 (question 135): the HUD line -- the scenario name, the CURRENT view frame id
// (`viewer.viewFrameId`, scene.js -- reparented by `setViewFrame()`/`fit()` on every
// frame switch) and the current focus (or "origin" when unfocused). Replaces the
// pre-M20.2 `${scenario.name} · ${scenario.frame.name} · ...`, which printed the
// scenario's *base* declared frame and so never changed after a frame switch.
export function hudText(scenarioName, viewFrameId, focus) {
  return `${scenarioName} · ${viewFrameId} · ${focus ? 'focus ' + focus : 'origin'}`;
}

// Question 169 (docs/open-questions.md): a viewport pane's title must derive from the
// frame it ACTUALLY shows, never from a layout's static intent. Before this fix, the RPO
// default layout's first pane was hardcoded "3D View -- ICRF" (web/js/layout/
// layout_manager.js's old PANEL_TITLES entry for ICRF_PANEL_ID) even when the Python
// scenario declares no ICRF frame at all and the viewport's camera falls back to
// whatever frame it actually got parented in (e.g. EarthMJ2000Eq) -- a real, observed
// mislabelling (the pane said ICRF, its own HUD line said EarthMJ2000Eq, right below it).
// `baseLabel` is the pane's role-neutral label ("3D View"); `frameId` is that VIEWPORT's
// own current `cameraFrameId` (web/js/viewport.js's `Viewport.cameraFrameId` for an extra
// viewport, `Viewer.viewFrameId` for the legacy primary one) -- `null` until a scenario
// has actually parented this viewport's camera in a real frame, in which case the pane
// shows the base label alone rather than naming any frame (never a claim this fix cannot
// back up). Pure and DOM-free like this module's other label helpers, so it is testable
// headlessly (tests/test_cdm_run.py) independent of web/js/layout/layout_manager.js's own
// DOM-only rendering (that module's `setPaneTitle` calls this function; see its own
// docstring on why the two are split).
export function viewportPaneTitle(baseLabel, frameId) {
  return frameId ? `${baseLabel} -- ${frameId}` : baseLabel;
}
