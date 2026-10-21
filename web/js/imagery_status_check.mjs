// CLI harness for tests/test_viewer_imagery_status.py: `node web/js/imagery_status_check.mjs`.
// Question 264: the server's verdict on the Blue Marble basemap (`scenario.imagery.installed`,
// `.notice`) as the viewer reads it. Drives the REAL, shipped modules (imagery_status.js and
// panels/map_panel.js's render()) with a minimal DOM stand-in -- there is no `document` under plain
// node -- and prints one JSON array of {name, pass, detail}. Exit 1 if any check fails.
import { basemapNotInstalled, basemapNotice, globeOnInfo, globeNotInstalledInfo } from './imagery_status.js';
import { render } from './panels/map_panel.js';

const checks = [];
function check(name, pass, detail) { checks.push({ name, pass: !!pass, detail: detail === undefined ? null : detail }); }

// ---- a just-enough DOM: elements record their children, attributes and style
class El {
  constructor(tag) { this.tagName = tag; this.children = []; this.style = {}; this.attrs = {}; this.className = ''; this.textContent = ''; }
  appendChild(c) { this.children.push(c); return c; }
  setAttribute(k, v) { this.attrs[k] = v; if (k === 'class') this.className = v; }
  set innerHTML(_v) { this.children = []; }
  all() { return this.children.flatMap((c) => [c, ...c.all()]); }
}
globalThis.document = {
  createElement: (t) => new El(t),
  createElementNS: (_ns, t) => new El(t),
};

const NOTICE = 'Blue Marble basemap not installed: manifest.json is missing. Expected in /d. Build it once with: .venv/bin/python scripts/imagery/build_blue_marble_tiles.py --original <the original> --out /d (then restart the server).';
const installed = { urlTemplate: './imagery/bluemarble/{z}/{x}/{y}.jpg', attribution: 'Blue Marble: Next Generation (July 2004), NASA Earth Observatory', maxLevel: 5, installed: true };
const missing = { ...installed, installed: false, notice: NOTICE };
const fixture = { urlTemplate: './fixtures/tiles/{z}/{x}/{y}.png', attribution: 'fixture', maxLevel: 2 }; // no `installed` key at all

// ---- imagery_status.js
check('only an explicit installed:false is "not installed"',
  basemapNotInstalled(missing) === true && basemapNotInstalled(installed) === false && basemapNotInstalled(fixture) === false
  && basemapNotInstalled(null) === false && basemapNotInstalled(undefined) === false && basemapNotInstalled({}) === false);
check('the notice is the server\'s own sentence, and null when usable',
  basemapNotice(missing) === NOTICE && basemapNotice(installed) === null && basemapNotice(fixture) === null && basemapNotice(null) === null);
check('a not-installed verdict with no notice still yields a sentence (never empty)',
  basemapNotice({ installed: false }).startsWith('Blue Marble basemap not installed'));
check('globe-info when on carries the attribution', globeOnInfo(installed).includes('imagery: Blue Marble: Next Generation (July 2004), NASA Earth Observatory'));
check('globe-info when not installed says plain body kept, not installed, and how to build',
  globeNotInstalledInfo(missing).includes('plain body kept') && globeNotInstalledInfo(missing).includes('not installed')
  && globeNotInstalledInfo(missing).includes('build_blue_marble_tiles.py') && !globeNotInstalledInfo(missing).includes('imagery: '));

// ---- map_panel.render()
const earth = { name: 'Earth', radius: 6378.137, flattening: 0, t: [0], pos: [[0, 0, 0]], quat: [[0, 0, 0, 1]] };
function renderMap(imagery, level = 1) {
  const container = new El('div');
  render(container, { sc: { imagery, bodies: [earth], spacecraft: [] }, level, t: 0 });
  const all = container.all();
  return {
    imgs: all.filter((e) => e.tagName === 'img'),
    notices: all.filter((e) => e.className.includes('av-panel-notice')),
    overlays: all.filter((e) => e.tagName === 'svg'),
  };
}
{
  const m = renderMap(missing);
  check('map: not installed -> no tile images at all (no mosaic of 404s)', m.imgs.length === 0, { imgs: m.imgs.length });
  check('map: not installed -> the server\'s notice is shown', m.notices.length === 1 && m.notices[0].textContent === NOTICE, m.notices.map((n) => n.textContent));
  check('map: not installed -> the overlay (ground tracks) is still drawn', m.overlays.length === 1);
}
{
  const m = renderMap(installed);
  check('map: installed -> the level-1 mosaic of 8 tiles from /imagery/bluemarble/', m.imgs.length === 8 && m.imgs.every((i) => i.src.startsWith('./imagery/bluemarble/1/') && i.src.endsWith('.jpg')), { n: m.imgs.length, first: m.imgs[0] && m.imgs[0].src });
  check('map: installed -> no notice', m.notices.length === 0);
}
{
  const m = renderMap(fixture, 2);
  check('map: a source with no verdict (the test fixture) still draws its mosaic at its level', m.imgs.length === 32 && m.imgs.every((i) => i.src.startsWith('./fixtures/tiles/2/')) && m.notices.length === 0, { n: m.imgs.length });
}

console.log(JSON.stringify(checks));
process.exit(checks.every((c) => c.pass) ? 0 : 1);
