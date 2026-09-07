// M26.4 (docs/ui-rework-plan.md): the 2D companion map -- ground tracks, footprints and
// current positions on an equirectangular map, using the SAME imagery profile as the
// globe (M19.5, `sc.imagery`: `{urlTemplate, attribution, maxLevel}`) and the offline
// fixture (`web/fixtures/tiles/`). No network: every tile URL is a relative path served
// by this same dev server from the local fixture directory (exactly how
// `web/js/globe.js`'s 3D globe already serves imagery -- this panel reuses its own
// `urlForTile` rather than a second URL-building implementation, see that export).
//
// The globe's own tiling scheme (`web/js/globe_lod.js`) is already a geographic
// "plate carree" (equirectangular) quadtree -- level L has `2**(L+1)` columns x `2**L`
// rows spanning the whole 360x180 degree globe (that module's own doc comment) -- so
// "an equirectangular map" is not a second projection to invent: it is literally one
// whole level of the SAME tile pyramid the globe already renders, laid out flat as an
// image mosaic instead of wrapped on a sphere. Reusing it (rather than a second static
// world-map image) is what makes "same imagery profile as the globe" true by
// construction, not just by convention.
import { tileCountX, tileCountY, tileBoundsDeg } from '../globe_lod.js';
import { urlForTile } from '../globe.js';
import { groundTrack, currentGroundPosition } from '../ground_track.js';

/**
 * Every tile at `level`, in mosaic row order: row 0 (rendered at the TOP of the map) is
 * the NORTHERNMOST row. `globe_lod.js`'s own tile addressing has y=0 at the SOUTH edge
 * (`tileBoundsDeg`: `south = -90 + tile.y * dLat`) -- the same convention the tile
 * fixture generator (`web/fixtures/gen_globe_tiles.py`) and the 3D globe's own UV
 * mapping (`web/js/globe.js`'s `buildTileMesh`, `uvs[ui+1] = 1 - j/segments`) both
 * already flip for exactly this reason: a raster image reads top-to-bottom as
 * north-to-south, but this tiling scheme addresses bottom-to-top. Flipping the ROW
 * order here (once, explicitly) rather than leaving it to CSS is what makes a visual
 * inspection of the mosaic (the browser check) match a real map, not a mirrored one.
 * @returns {{level:number, x:number, y:number, url:string, boundsDeg:object}[][]} rows[0] = north
 */
export function mosaicRows(urlTemplate, level) {
  const nx = tileCountX(level), ny = tileCountY(level);
  const rows = [];
  for (let rowFromTop = 0; rowFromTop < ny; rowFromTop++) {
    const y = ny - 1 - rowFromTop; // rowFromTop=0 -> northernmost tile row
    const row = [];
    for (let x = 0; x < nx; x++) {
      const tile = { level, x, y };
      row.push({ level, x, y, url: urlForTile(urlTemplate, tile), boundsDeg: tileBoundsDeg(tile) });
    }
    rows.push(row);
  }
  return rows;
}

/** Flat list of every tile URL `mosaicRows` would request at `level`, for a headless
 * "no network, only the offline fixture" check (web/js/panels_check.mjs) -- pure data,
 * never issues a fetch/XHR/Image itself. */
export function tileUrlsForLevel(urlTemplate, level) {
  return mosaicRows(urlTemplate, level).flat().map((t) => t.url);
}

/** Geodetic lon/lat (degrees) -> percentage position within the equirectangular mosaic
 * (`{xPct, yPct}`, both in `[0, 100]`, origin top-left -- CSS `left`/`top` percentages).
 * `xPct` is linear in longitude (plate carree has no longitude distortion); `yPct` is
 * linear in latitude with north (lat=+90) at `yPct=0` (the top), matching
 * `mosaicRows`'s own top-is-north convention above -- both must agree, or a marker
 * would sit on the wrong tile row than the tile mosaic it is drawn over. */
export function lonLatToPercent(lonDeg, latDeg) {
  return { xPct: ((lonDeg + 180) / 360) * 100, yPct: ((90 - latDeg) / 180) * 100 };
}

function findBody(sc, name) {
  return (sc.bodies || []).find((b) => b.name === name) || null;
}

// -------------------------------------------------------------------------------- DOM
/**
 * Render the map into `container`. `opts.t` (A1MJD, optional) positions each
 * spacecraft's "current position" dot at that epoch (via `ground_track.js`'s
 * `currentGroundPosition`, the same `TrajectoryInterp` interpolation the 3D view's own
 * marker uses) -- omitted, only the static ground tracks are drawn.
 * @param {HTMLElement} container
 * @param {{sc: object, level?: number, t?: number}} opts
 */
export function render(container, { sc, level = 1, t } = {}) {
  container.innerHTML = '';
  if (!sc || !sc.imagery || !sc.imagery.urlTemplate) {
    const notice = document.createElement('p');
    notice.className = 'av-panel-notice';
    notice.textContent = 'No imagery profile in this scenario.';
    container.appendChild(notice);
    return;
  }
  const earth = findBody(sc, 'Earth');
  const rows = mosaicRows(sc.imagery.urlTemplate, Math.min(level, sc.imagery.maxLevel ?? level));

  const wrap = document.createElement('div');
  wrap.className = 'av-map-wrap';
  const mosaic = document.createElement('div');
  mosaic.className = 'av-map-mosaic';
  mosaic.style.gridTemplateColumns = `repeat(${rows[0].length}, 1fr)`;
  mosaic.style.gridTemplateRows = `repeat(${rows.length}, 1fr)`;
  for (const row of rows) {
    for (const tile of row) {
      const img = document.createElement('img');
      img.className = 'av-map-tile';
      img.src = tile.url;
      img.alt = `tile ${tile.level}/${tile.x}/${tile.y}`;
      img.draggable = false;
      mosaic.appendChild(img);
    }
  }
  wrap.appendChild(mosaic);

  const overlay = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  overlay.setAttribute('class', 'av-map-overlay');
  overlay.setAttribute('viewBox', '0 0 100 100');
  overlay.setAttribute('preserveAspectRatio', 'none');

  if (earth) {
    for (const s of sc.spacecraft || []) {
      const track = groundTrack(s, earth);
      if (track.length) {
        const path = document.createElementNS('http://www.w3.org/2000/svg', 'polyline');
        const pts = track.map((p) => {
          const { xPct, yPct } = lonLatToPercent(p.lonDeg, p.latDeg);
          return `${xPct},${yPct}`;
        }).join(' ');
        path.setAttribute('points', pts);
        path.setAttribute('class', 'av-map-track');
        path.style.stroke = s.color || '#fff';
        overlay.appendChild(path);
      }
      if (typeof t === 'number') {
        const cur = currentGroundPosition(s, earth, t);
        const { xPct, yPct } = lonLatToPercent(cur.lonDeg, cur.latDeg);
        const dot = document.createElementNS('http://www.w3.org/2000/svg', 'circle');
        dot.setAttribute('cx', String(xPct));
        dot.setAttribute('cy', String(yPct));
        dot.setAttribute('r', '1.2');
        dot.setAttribute('class', 'av-map-current');
        dot.style.fill = s.color || '#fff';
        overlay.appendChild(dot);
      }
    }
  }
  wrap.appendChild(overlay);

  if (!earth) {
    const notice = document.createElement('p');
    notice.className = 'av-panel-notice';
    notice.textContent = 'No Earth body in this scenario -- ground tracks need a body-fixed frame to project against.';
    container.appendChild(notice);
  }
  container.appendChild(wrap);
}
