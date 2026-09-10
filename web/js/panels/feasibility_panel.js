// F3b (docs/feasibility-plan.md's F3 milestone): "a feasibility panel in the tiling
// layout shows the grid with a chosen score as colour, the per-point distribution
// across draws, and opens any sample's run in the existing viewer through its
// products_uri." Same convention as web/js/panels/run_products_panel.js (that file's
// own module comment, verbatim): pure, DOM-free data-binding functions in the top half
// of this file, independently testable under plain `node`
// (web/js/panels_check.mjs/tests/test_viewer_feasibility_panel.py), exactly one
// DOM-touching `render()` at the bottom, used by web/js/app.js and the browser check,
// never by the headless harness.
//
// The wire contract this panel binds to (FIXED by the feasibility manager, worker A
// implements the `POST /api/cdm/sweep` publish path that produces it -- see
// docs/feasibility-plan.md's F3 milestone and this task's own brief): a scenario
// published by that route carries a `sweep` key shaped as
//   sweep.sweepId/sweepHash/drmHash: str
//   sweep.axisKeys:   [str]                 -- sorted union of every sample's axis keys
//   sweep.scoreNames: [str]                 -- sorted union of every aggregate's name
//   sweep.points:     [{pointIndex, axisValues, samples: [...]}]   -- ascending pointIndex
//   sweep.aggregates: [{name, pointIndex, draws, mean, stdDev, min, max, passFraction}]
// sorted by (name, pointIndex). A scenario published by any other route has NO `sweep`
// key at all -- every function below treats `sweep` (and any nested piece of it) as
// possibly null/undefined/malformed and degrades to an empty/honest result rather than
// throwing or inventing data; `render()`'s own "no study in this scenario" notice is
// the DOM-facing half of that same rule.
//
// This study's own scores (docs/feasibility-plan.md's fixture study, question 192(c)'s
// second axis) are every one a Measure of Effectiveness -- no Objective is declared, so
// every `ScoreResult.passed`/`ScoreAggregate.passFraction` on the real wire is `null`.
// That is the NORMAL case for this panel (unlike run_products_panel.js's fixture, which
// has a mix), not an edge case -- nothing below ever coerces a `null` passFraction to
// `0`/`false` (run_products_panel.js's `objectiveRows` doc comment already explains
// exactly why that coercion is a real misrepresentation, ADR-005 sec 6; the same rule
// applies here).

// ------------------------------------------------------------------------ score names

/**
 * The score names a user may colour the grid by, sorted. `sweep.scoreNames` is already
 * the sorted union per the wire contract above, but this function does not trust that
 * silently -- it re-sorts a copy explicitly, mirroring run_products_panel.js's own
 * `measurementRows` doc comment ("does not trust that silently... re-sorts explicitly,
 * so a caller of this module never depends on an ordering guarantee it cannot see for
 * itself"). Falls back to deriving the union from `sweep.aggregates` when `scoreNames`
 * itself is missing/malformed (a defensive path only -- the real wire always sends it),
 * so a caller still gets a correct, sorted answer rather than an empty one for an
 * otherwise-well-formed sweep.
 * @param {{scoreNames?: string[], aggregates?: Array<{name:string}>}|null|undefined} sweep
 * @returns {string[]}
 */
export function scoreNames(sweep) {
  if (!sweep) return [];
  if (Array.isArray(sweep.scoreNames)) return [...sweep.scoreNames].sort();
  const fromAggregates = new Set((sweep.aggregates || []).map((a) => a && a.name).filter((n) => typeof n === 'string'));
  return [...fromAggregates].sort();
}

// ------------------------------------------------------------------------------ axes

/**
 * `sweep.axisKeys`, defensively copied and re-sorted (same "never trust the wire's own
 * claimed order silently" posture as `scoreNames` above) -- `[]` for a null/absent
 * sweep or a malformed `axisKeys`.
 * @param {{axisKeys?: string[]}|null|undefined} sweep
 * @returns {string[]}
 */
export function gridAxes(sweep) {
  if (!sweep || !Array.isArray(sweep.axisKeys)) return [];
  return [...sweep.axisKeys].sort();
}

/**
 * The sorted, de-duplicated set of values `gridRows`' `axisValues[axisKey]` takes on --
 * `render()`'s own building block for the 2D table's row/column headers (one level per
 * distinct value actually present, not an assumed evenly-spaced range: this project's
 * `ParameterSweep.SweepAxis` allows explicit value lists, not only `min..max` steps, so
 * "the levels" must be read off the data, never synthesized from a count). Numeric
 * ascending order (`localeCompare` would sort "10" before "2" as strings) -- every axis
 * value on the real wire is a JSON number (`axis_values` is `map<string, double>`), so a
 * numeric comparator is always the correct one here, never a string one.
 * @param {Array<{axisValues: Object<string, number>}>} rows gridRows()'s own output (or
 *   any array shaped like it)
 * @param {string} axisKey
 * @returns {number[]}
 */
export function axisLevels(rows, axisKey) {
  const seen = new Set();
  for (const row of rows || []) {
    if (row && row.axisValues && Object.prototype.hasOwnProperty.call(row.axisValues, axisKey)) {
      seen.add(row.axisValues[axisKey]);
    }
  }
  return [...seen].sort((a, b) => a - b);
}

// ------------------------------------------------------------------------- grid rows

/**
 * Bucket a normalized colour position `t` (see `gridRows` below) into one of
 * `buckets` discrete steps, or the string `'none'` for a `t` of `null` (no aggregate for
 * this score at this point -- see `gridRows`' own doc comment on why that case is never
 * silently folded into bucket 0/`t=0`). `render()` uses the returned value to pick a
 * CSS class (`av-heat-<n>` / `av-heat-none`) rather than computing/interpolating an
 * actual colour string in JS -- see this module's own `render()` doc comment ("colour
 * lives in CSS") for why that split follows this codebase's existing convention
 * (`.av-pass`/`.av-fail`/`.av-measure`, `.av-timeline-event-fault` etc. in web/style.css
 * are all discrete CSS classes keyed off a small enum this code computes, never an
 * inline computed style string) rather than inventing a second style for this one
 * panel. `buckets` defaults to 7: enough steps to show a real gradient without
 * approaching the point where two adjacent buckets become indistinguishable by eye --
 * an arbitrary but stated choice, not derived from anything.
 * @param {number|null} t in `[0, 1]`, or `null`
 * @param {number} [buckets=7]
 * @returns {number|'none'}
 */
export function heatBucket(t, buckets = 7) {
  if (t === null || t === undefined || Number.isNaN(t)) return 'none';
  const clamped = Math.min(1, Math.max(0, t));
  return Math.min(buckets - 1, Math.floor(clamped * buckets));
}

/**
 * The grid as data: one entry per point in `sweep.points`, carrying its `pointIndex`,
 * its `axisValues`, the chosen score's `aggregate` at that point (or `null`), and a
 * normalized colour position `t` in `[0, 1]` derived from the MIN and MAX of that
 * score's `mean` ACROSS POINTS (never across draws within one point -- that spread is
 * `drawRows`' own job below).
 *
 * Two documented, decided edge cases (the brief's own required decisions):
 * - **Zero-width range** (every point's mean for this score is equal, so max === min):
 *   `t = 0.5` for every point that HAS an aggregate, never a `0/0` division. Rationale:
 *   `0.5` is the scale's own neutral midpoint (see `render()`'s colour comment) -- it
 *   says "no informative gradient exists here", which is honest, versus `t = 0` (would
 *   render every point as if it were the coldest/lowest extreme of a range that does
 *   not actually exist) or `NaN` (would either crash a naive consumer or render as
 *   `heatBucket`'s `'none'`, which is reserved for "no aggregate at all" below and must
 *   stay visually distinguishable from "flat, but real, data").
 * - **No aggregate for this score at this point** (a point whose samples all failed for
 *   this score, or a point that never had this score name at all): `t = null`, and
 *   `aggregate` is `null`. This is NEVER coalesced to `t = 0` -- doing so would render a
 *   "we have no data here" point identically to a real, measured minimum, which is
 *   exactly the kind of invented-looking-real data this whole track's brief forbids.
 *   `heatBucket(null)` returns the sentinel `'none'` bucket precisely so `render()` can
 *   give this case its own honest visual treatment (see that function's doc comment).
 *
 * Points are returned in ascending `pointIndex` order, matching `sweep.points`' own wire
 * order (the wire contract's own "ascending by pointIndex" -- re-asserted here, not
 * merely assumed, by sorting explicitly rather than trusting the array's incoming
 * order, same "never trust silently" posture as `scoreNames`/`gridAxes` above).
 * @param {{points?: Array<{pointIndex:number, axisValues:Object<string,number>}>, aggregates?: Array<{name:string,pointIndex:number,mean:number,stdDev:number,min:number,max:number,draws:number,passFraction:number|null}>}|null|undefined} sweep
 * @param {string|null|undefined} scoreName
 * @returns {Array<{pointIndex:number, axisValues:Object<string,number>, aggregate:object|null, t:number|null}>}
 */
export function gridRows(sweep, scoreName) {
  if (!sweep || !Array.isArray(sweep.points)) return [];
  const aggByPoint = new Map();
  if (scoreName) {
    for (const a of sweep.aggregates || []) {
      if (a && a.name === scoreName) aggByPoint.set(a.pointIndex, a);
    }
  }
  const points = [...sweep.points].sort((a, b) => a.pointIndex - b.pointIndex);
  const means = points
    .map((p) => aggByPoint.get(p.pointIndex))
    .filter((a) => !!a)
    .map((a) => a.mean);
  const lo = means.length ? Math.min(...means) : null;
  const hi = means.length ? Math.max(...means) : null;
  const zeroWidth = lo !== null && hi !== null && hi === lo;

  return points.map((p) => {
    const aggregate = aggByPoint.get(p.pointIndex) || null;
    let t = null;
    if (aggregate) {
      t = zeroWidth ? 0.5 : (aggregate.mean - lo) / (hi - lo);
    }
    return { pointIndex: p.pointIndex, axisValues: p.axisValues || {}, aggregate, t };
  });
}

// ------------------------------------------------------------------------- draw rows

/**
 * Whether a sample can be opened in the viewer -- true iff `productsUri` is a non-empty
 * string. A failed sample's `productsUri` is always empty on the real wire (it never
 * ran to completion, so there is nothing at a URI to open); this is the one place that
 * rule is decided, so `render()` never has to re-derive "openable" from `error` instead
 * (which would be a second, potentially-divergent definition of the same thing).
 * @param {{productsUri?: string}|null|undefined} sample
 * @returns {boolean}
 */
export function isSampleOpenable(sample) {
  return !!(sample && typeof sample.productsUri === 'string' && sample.productsUri.length > 0);
}

/**
 * The per-point distribution across draws -- what makes the study's stochastic spread
 * visible (this panel's own required job, per the brief). One entry per sample at
 * `pointIndex`, ascending by `drawIndex` (re-sorted explicitly, not trusted from the
 * wire's own order -- same posture as everywhere else in this module), each carrying:
 * - `value`/`unit`: the chosen score's value for that draw, or `null`/`null` when the
 *   sample failed OR never carried that score name (both are real, distinct
 *   possibilities on the wire -- a failed sample's `scores` map is always empty per
 *   `crates/av-sweep/src/aggregate.rs`'s own "a failed sample's scores map is always
 *   empty" comment, but a SUCCEEDED sample could in principle omit a score name the DRM
 *   did not declare; either way `value: null` here, never a fabricated `0`).
 * - `failed`: `sample.error !== ''` -- the exact same rule
 *   `crates/av-sweep/src/aggregate.rs`'s `aggregate()` uses to decide a sample
 *   "contributes" or not (`!sample.error.is_empty()` there); mirrored here so this
 *   panel's own "failed" never silently drifts from what the aggregate the grid already
 *   shows was actually computed from.
 * - `error`: the sample's own error string, verbatim (`''` for a succeeded sample) --
 *   **a failed sample is a row here, never dropped** (the brief's own required rule;
 *   this is the literal reason `drawRows` returns EVERY sample at the point, not only
 *   the ones that contributed a value).
 * - `runId`, `configHash`, `seeds`, `productsUri`: carried through unchanged from the
 *   wire (deliberately not reshaped -- a caller reading `.seeds` off a returned row is
 *   reading the exact wire value).
 * - `openable`: `isSampleOpenable(sample)` above, so `render()` never has to re-derive it.
 *
 * `pointIndex` naming an absent point, or a `sweep` with no `points` array, yields `[]`
 * (never a throw) -- the same "malformed/absent input degrades to empty, not an
 * exception" posture as every other function in this module.
 * @param {{points?: Array<{pointIndex:number, samples: Array<object>}>}|null|undefined} sweep
 * @param {number|null|undefined} pointIndex
 * @param {string|null|undefined} scoreName
 * @returns {Array<{drawIndex:number, value:number|null, unit:string|null, failed:boolean, error:string, runId:string, configHash:string, seeds:Object<string,string>, productsUri:string, openable:boolean}>}
 */
export function drawRows(sweep, pointIndex, scoreName) {
  if (!sweep || !Array.isArray(sweep.points) || pointIndex === null || pointIndex === undefined) return [];
  const point = sweep.points.find((p) => p && p.pointIndex === pointIndex);
  if (!point || !Array.isArray(point.samples)) return [];

  return [...point.samples]
    .sort((a, b) => a.drawIndex - b.drawIndex)
    .map((s) => {
      const score = scoreName && s.scores ? s.scores[scoreName] : null;
      const failed = !!(s.error && s.error.length > 0);
      return {
        drawIndex: s.drawIndex,
        value: score && typeof score.value === 'number' ? score.value : null,
        unit: (score && score.unit) || null,
        failed,
        error: s.error || '',
        runId: s.runId || '',
        configHash: s.configHash || '',
        seeds: s.seeds || {},
        productsUri: s.productsUri || '',
        openable: isSampleOpenable(s),
      };
    });
}

// -------------------------------------------------------------------------------- DOM
// Colour lives in CSS (web/style.css), not as computed inline style strings in this
// file -- checked against the existing convention before writing this: run_products_panel.js's
// pass/fail/measure states and its fault/port-command event kinds are BOTH rendered as
// CSS classes keyed off a small enum (`.av-pass`/`.av-fail`/`.av-measure`,
// `.av-timeline-event-fault`/`-port_command`), and map_panel.js's one inline-style use
// (`path.style.stroke = s.color`) is a per-ENTITY colour the wire already hands it
// (spacecraft's own declared colour), not a computed function of a value -- there is no
// precedent in this codebase for computing/interpolating a colour string in JS. This
// panel follows the first, dominant pattern: `heatBucket()` above maps a continuous `t`
// to one of a small number of discrete `av-heat-<n>` classes (or `av-heat-none`), and
// web/style.css owns the actual colours for each class.
//
// The scale itself (documented at its CSS rule, web/style.css's own "F3b feasibility
// panel" section): a diverging blue -> neutral -> orange ramp that also varies
// LIGHTNESS across the range (not a pure hue sweep) -- the brief's own requirement
// ("does not encode magnitude as hue alone"). Two more channels carry the same
// information redundantly, so no one channel is load-bearing on its own: every cell's
// exact mean is also printed as text inside it, and the "no aggregate" case gets its own
// hatched/muted treatment (never a colour bucket at all) rather than relying on the
// viewer to distinguish "very low value" from "no value" by hue/lightness alone.

function fmtValue(v) {
  if (v === null || v === undefined) return '--';
  const av = Math.abs(v);
  if (av !== 0 && (av >= 1e6 || av < 1e-3)) return v.toExponential(4);
  return v.toFixed(3);
}

function fmtAxisValue(v) {
  return typeof v === 'number' ? (Number.isInteger(v) ? String(v) : v.toFixed(3)) : String(v);
}

function shortHash(h) {
  if (!h) return '';
  return h.length > 12 ? `${h.slice(0, 10)}…` : h;
}

/**
 * Build the grid section's DOM, choosing a layout by `axisKeys.length` (the brief's own
 * required, documented decision):
 * - **0 axes**: a "no axes declared" notice (a degenerate sweep -- never rendered as an
 *   empty table).
 * - **1 axis**: a single-column list, one row per level of that one axis (still a real
 *   table for consistent styling/hover, just width 1) -- there is no second axis to
 *   project onto columns, so this is not a fallback so much as the ONLY honest shape for
 *   one axis.
 * - **2 axes**: a real 2D table -- rows by `axisKeys[0]`'s levels, columns by
 *   `axisKeys[1]`'s levels (the case the brief calls out as "the case that matters",
 *   and the fixture study's own real shape). A `(row, col)` pair with no matching point
 *   renders as a blank/absent cell (`av-heat-none`), never a wrong point's data guessed
 *   into an unrelated cell.
 * - **3+ axes**: a flat list, one row per point, printing every one of its axis values
 *   inline as text (`key=value, key2=value2, ...`) alongside its colour swatch --
 *   deliberately NOT a silently-wrong 2D projection of a 3+ dimensional grid onto two
 *   axes (which axes would even be picked? any answer throws away real information
 *   without saying so). This is the brief's own explicitly-sanctioned "a documented,
 *   honest fallback... is fine" case.
 */
function buildGridSection(rows, axisKeys, selectedPoint, onSelectPoint) {
  const section = document.createElement('div');
  section.className = 'av-panel-section';
  const title = document.createElement('h4');
  title.textContent = 'Grid';
  section.appendChild(title);

  if (axisKeys.length === 0) {
    const notice = document.createElement('p');
    notice.className = 'av-panel-notice';
    notice.textContent = 'This study declares no axes.';
    section.appendChild(notice);
    return section;
  }

  const cell = (row) => {
    const td = document.createElement('td');
    if (!row) {
      td.className = 'av-heat-cell av-heat-none';
      td.textContent = '';
      return td;
    }
    const bucket = heatBucket(row.t);
    td.className = `av-heat-cell av-heat-${bucket}` + (row.pointIndex === selectedPoint ? ' av-heat-selected' : '');
    td.textContent = row.aggregate ? fmtValue(row.aggregate.mean) : 'no data';
    td.title = row.aggregate
      ? `point ${row.pointIndex}: mean=${row.aggregate.mean}, stdDev=${row.aggregate.stdDev}, draws=${row.aggregate.draws}`
      : `point ${row.pointIndex}: no aggregate for this score`;
    td.addEventListener('click', () => onSelectPoint && onSelectPoint(row.pointIndex));
    return td;
  };

  if (axisKeys.length <= 2) {
    const rowAxis = axisKeys[0];
    const colAxis = axisKeys.length === 2 ? axisKeys[1] : null;
    const rowLevels = axisLevels(rows, rowAxis);
    const colLevels = colAxis ? axisLevels(rows, colAxis) : [null];

    const table = document.createElement('table');
    table.className = 'av-feasibility-grid';
    if (colAxis) {
      const headTr = document.createElement('tr');
      headTr.appendChild(document.createElement('th'));
      for (const cv of colLevels) {
        const th = document.createElement('th');
        th.textContent = `${colAxis}=${fmtAxisValue(cv)}`;
        headTr.appendChild(th);
      }
      table.appendChild(headTr);
    }
    for (const rv of rowLevels) {
      const tr = document.createElement('tr');
      const rowHeadTh = document.createElement('th');
      rowHeadTh.textContent = `${rowAxis}=${fmtAxisValue(rv)}`;
      tr.appendChild(rowHeadTh);
      for (const cv of colLevels) {
        const row = rows.find((r) => r.axisValues[rowAxis] === rv && (colAxis === null || r.axisValues[colAxis] === cv));
        tr.appendChild(cell(row));
      }
      table.appendChild(tr);
    }
    section.appendChild(table);
    return section;
  }

  // 3+ axes: flat list fallback (documented above).
  const list = document.createElement('ul');
  list.className = 'av-feasibility-flat-grid';
  for (const row of rows) {
    const li = document.createElement('li');
    const swatch = document.createElement('span');
    const bucket = heatBucket(row.t);
    swatch.className = `av-heat-swatch av-heat-${bucket}`;
    const label = document.createElement('span');
    label.className = 'label';
    const axisText = axisKeys.map((k) => `${k}=${fmtAxisValue(row.axisValues[k])}`).join(', ');
    label.textContent = `point ${row.pointIndex}: ${axisText} -- ${row.aggregate ? fmtValue(row.aggregate.mean) : 'no data'}`;
    li.className = row.pointIndex === selectedPoint ? 'av-heat-selected' : '';
    li.append(swatch, label);
    li.addEventListener('click', () => onSelectPoint && onSelectPoint(row.pointIndex));
    list.appendChild(li);
  }
  section.appendChild(list);
  return section;
}

function buildDistributionSection(draws, pointIndex, onOpenSample) {
  const section = document.createElement('div');
  section.className = 'av-panel-section';
  const title = document.createElement('h4');
  title.textContent = pointIndex === null || pointIndex === undefined
    ? 'Distribution across draws'
    : `Distribution across draws -- point ${pointIndex}`;
  section.appendChild(title);

  if (pointIndex === null || pointIndex === undefined) {
    const notice = document.createElement('p');
    notice.className = 'av-panel-notice';
    notice.textContent = 'Select a grid point to see its draws.';
    section.appendChild(notice);
    return section;
  }
  if (draws.length === 0) {
    const notice = document.createElement('p');
    notice.className = 'av-panel-notice';
    notice.textContent = 'No draws recorded for this point.';
    section.appendChild(notice);
    return section;
  }

  const table = document.createElement('table');
  table.className = 'av-scores-table av-feasibility-draws';
  for (const d of draws) {
    const tr = document.createElement('tr');
    tr.className = d.failed ? 'av-fail' : '';

    const drawTd = document.createElement('td');
    drawTd.textContent = `draw ${d.drawIndex}`;

    const valueTd = document.createElement('td');
    valueTd.textContent = d.failed ? 'FAILED' : fmtValue(d.value);
    if (d.failed) valueTd.className = 'av-fail';

    const runTd = document.createElement('td');
    runTd.textContent = `${d.runId} (${shortHash(d.configHash)})`;
    runTd.title = d.error || '';

    const openTd = document.createElement('td');
    if (d.openable) {
      const btn = document.createElement('button');
      btn.type = 'button';
      btn.className = 'av-pane-btn av-feasibility-open';
      btn.textContent = 'Open';
      btn.title = `Open this sample's run (${d.productsUri})`;
      btn.addEventListener('click', () => onOpenSample && onOpenSample(d));
      openTd.appendChild(btn);
    } else {
      // Non-openable (typically a failed sample, `productsUri` empty) -- a visibly
      // disabled control, NEVER a dead/inert-looking link (the brief's own explicit
      // requirement): both styled muted AND textually says why.
      const span = document.createElement('span');
      span.className = 'av-panel-notice av-feasibility-unopenable';
      span.textContent = d.failed ? 'unavailable (failed)' : 'unavailable';
      span.title = d.error || 'no products_uri recorded for this sample';
      openTd.appendChild(span);
    }
    tr.append(drawTd, valueTd, runTd, openTd);
    table.appendChild(tr);
  }
  section.appendChild(table);

  if (draws.some((d) => d.failed)) {
    const notice = document.createElement('p');
    notice.className = 'av-panel-notice';
    const failedRows = draws.filter((d) => d.failed);
    notice.textContent = `${failedRows.length} of ${draws.length} draw(s) failed: ` +
      failedRows.map((d) => `draw ${d.drawIndex} (${d.error})`).join('; ');
    section.appendChild(notice);
  }
  return section;
}

/**
 * Render this panel's content into `container` (an existing, empty DOM element -- same
 * contract as run_products_panel.js's own `render()`). `data.sweep` is `scenario.sweep`
 * exactly as published by worker A's `POST /api/cdm/sweep` route, or `undefined`/`null`
 * for any scenario published by another route -- that absence gets an honest "no study
 * in this scenario" notice here, never invented data (this module's own top comment).
 * `selectedScore`/`selectedPoint` are owned by the caller (web/js/app.js), not this
 * module -- mirrors run_products_panel.js's `onJumpToEvent` pattern of passing behaviour
 * in rather than this module holding its own mutable state.
 * @param {HTMLElement} container
 * @param {{sweep?: object|null, selectedScore?: string|null, selectedPoint?: number|null,
 *   onSelectPoint?: (pointIndex:number)=>void, onSelectScore?: (name:string)=>void,
 *   onOpenSample?: (drawRow:object)=>void}} data
 */
export function render(container, data) {
  container.innerHTML = '';
  const { sweep, selectedScore, selectedPoint, onSelectPoint, onSelectScore, onOpenSample } = data || {};

  if (!sweep) {
    const notice = document.createElement('p');
    notice.className = 'av-panel-notice';
    notice.textContent = 'No study in this scenario.';
    container.appendChild(notice);
    return;
  }

  const names = scoreNames(sweep);
  // The score to colour/select by, defaulting to the first sorted name when the caller
  // has not chosen one yet (or chose a name this sweep no longer has) -- never silently
  // renders with no score selected when at least one is available.
  const effectiveScore = names.includes(selectedScore) ? selectedScore : (names[0] || null);

  const header = document.createElement('div');
  header.className = 'av-panel-section av-feasibility-header';
  const idLine = document.createElement('p');
  idLine.className = 'av-panel-notice';
  idLine.textContent = `sweep ${sweep.sweepId || '(unknown)'} -- sweepHash ${shortHash(sweep.sweepHash)}, drmHash ${shortHash(sweep.drmHash)}`;
  header.appendChild(idLine);

  if (names.length === 0) {
    const notice = document.createElement('p');
    notice.className = 'av-panel-notice';
    notice.textContent = 'This study declares no scores.';
    header.appendChild(notice);
  } else {
    const label = document.createElement('label');
    label.className = 'av-feasibility-score-label';
    label.textContent = 'Colour by: ';
    const select = document.createElement('select');
    select.className = 'av-feasibility-score-select';
    for (const name of names) {
      const opt = document.createElement('option');
      opt.value = name;
      opt.textContent = name;
      if (name === effectiveScore) opt.selected = true;
      select.appendChild(opt);
    }
    select.addEventListener('change', () => onSelectScore && onSelectScore(select.value));
    label.appendChild(select);
    header.appendChild(label);
  }
  container.appendChild(header);

  const axisKeys = gridAxes(sweep);
  const rows = gridRows(sweep, effectiveScore);
  container.appendChild(buildGridSection(rows, axisKeys, selectedPoint, onSelectPoint));

  const draws = drawRows(sweep, selectedPoint, effectiveScore);
  container.appendChild(buildDistributionSection(draws, selectedPoint, onOpenSample));
}
