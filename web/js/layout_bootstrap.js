// M26.2 windowing core bootstrap: wires web/js/layout/layout_manager.js into
// index.html. Deliberately separate from web/js/app.js (which owns the sidebar/HUD
// *content* and is otherwise unmodified by this task -- see
// web/js/layout/REPORT.md's "what moved out of app.js" section: nothing did, app.js
// still only ever calls document.getElementById() for the controls it has always
// owned; only their DOM *ancestor* changes, from a static CSS-grid #app to a pane
// this module renders).
//
// Runs before app.js in index.html's script order so the real #sidebar/#viewport
// elements (declared, unchanged, inside index.html's hidden #pane-content-pool) are
// already moved into their panes by the time app.js's own getElementById() calls run
// -- though app.js does not actually depend on that order, since it never touches
// #sidebar/#viewport themselves, only their descendants by id.
import { LayoutManager } from './layout/layout_manager.js';
import {
  ICRF_PANEL_ID, RIC_PANEL_ID, GLOBE_PANEL_ID,
  RUN_PRODUCTS_PANEL_ID, MAP_PANEL_ID, CONSOLE_PANEL_ID,
} from './layout/default_layouts.js';

const layoutManager = new LayoutManager({
  root: document.getElementById('layout-root'),
  contentProviders: {
    sidebar: document.getElementById('sidebar'),
    viewport: document.getElementById('viewport'),
    // M26.3: the three extra 3D-viewport panes (index.html's #viewport-icrf/-ric/-globe)
    // for the RPO default layout -- panelId keys imported from default_layouts.js itself
    // (not re-typed literals) so the two can never silently drift apart.
    [ICRF_PANEL_ID]: document.getElementById('viewport-icrf'),
    [RIC_PANEL_ID]: document.getElementById('viewport-ric'),
    [GLOBE_PANEL_ID]: document.getElementById('viewport-globe'),
    // M26.4: the three panels (index.html's #panel-run-products/-map/-console).
    [RUN_PRODUCTS_PANEL_ID]: document.getElementById('panel-run-products'),
    [MAP_PANEL_ID]: document.getElementById('panel-map'),
    [CONSOLE_PANEL_ID]: document.getElementById('panel-console'),
  },
  errorBanner: document.getElementById('layout-error'),
  onResize: () => window.dispatchEvent(new Event('resize')), // scene.js's Viewer.resize() is wired to window 'resize' by app.js
});

// Exposed for app.js (M19.5's per-profile default, once a scenario's real `imagery`
// is known) and for manual/browser-check use -- not a new server API, just a client-
// side handle.
window.altavistaLayoutManager = layoutManager;

document.getElementById('layout-export').addEventListener('click', () => {
  const json = layoutManager.exportJson();
  const blob = new Blob([json], { type: 'application/json' });
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = 'altavista-layout.json';
  document.body.appendChild(a);
  a.click();
  a.remove();
  URL.revokeObjectURL(url);
});

const importInput = document.getElementById('layout-import-file');
document.getElementById('layout-import').addEventListener('click', () => importInput.click());
importInput.addEventListener('change', async () => {
  const file = importInput.files && importInput.files[0];
  importInput.value = '';
  if (!file) return;
  const text = await file.text();
  try {
    layoutManager.importJson(text);
  } catch (e) {
    // Visible, not silent (this task's brief): an invalid imported layout is
    // rejected and reported, never quietly replaced by the current/default layout.
    window.alert(`Could not import layout: ${e.message}`);
  }
});

document.getElementById('layout-reset').addEventListener('click', () => {
  // M26.3: resetToDefault() now takes the whole scenario (an RPO-shaped one resets to
  // the ICRF/RIC/globe triple-viewport default, not just the sidebar+viewport pair).
  layoutManager.resetToDefault(window.altavistaCurrentScenario || null);
});
