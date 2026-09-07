// M26.4 (docs/ui-rework-plan.md): "Run products and scores" panel -- objectives with
// pass state, measures, and port command/fault events linked to the timeline.
//
// Pure data-binding functions only (no DOM) in this half of the file, exactly the
// convention web/js/cdm_run.js already established (pure functions app.js's DOM code
// calls into, independently testable under plain `node` --
// web/js/panels_check.mjs/tests/test_viewer_panels.py). `render()` at the bottom is the
// one DOM-touching function, used by web/js/app.js and the browser check, never by the
// headless harness.
//
// M26.4b (docs/open-questions.md question 165): `altavista/server.py`'s
// `POST /api/cdm/run` now threads `RunProducts.scores` into the published scenario as
// an additive `scores` key (`{name: {value, unit, passed}}`, `altavista/server.py`'s
// `_score_result_to_dict`) plus `meta.scoresSource = "RunProducts.scores"` -- see
// `web/js/REPORT_M26_4b.md`. `objectiveRows()` below is unchanged from M26.4 (it was
// already written against, and proven correct against, this exact wire shape -- only
// the wire itself was missing); `render()`'s old "server doesn't send them yet" notice
// is gone, since every scenario a live server publishes now carries a real (possibly
// empty) `scores` object.

/**
 * `scores`-shaped object (`{[name]: {value, unit, passed}}`) -> a sorted array of rows,
 * `passed` distinguishing an Objective (has a pass criterion, `proto/altavista/v1/
 * run.proto`'s `ScoreResult.passed` is an `optional bool`) from a Measure of
 * Effectiveness (`passed` absent/`null` -- ADR-005 sec 6, no pass criterion at all,
 * never coerced to `false`). Sorted by name for deterministic rendering/testing
 * (mirrors this codebase's own "sort explicitly" convention, e.g.
 * `altavista/server.py`'s `RunProducts.trajectories` sorted-key iteration).
 * @param {Object<string,{value:number,unit?:string,passed?:boolean|null}>|null|undefined} scores
 * @returns {{name:string, value:number, unit:string|null, passed:boolean|null, kind:'objective'|'measure'}[]}
 */
export function objectiveRows(scores) {
  if (!scores) return [];
  return Object.keys(scores).sort().map((name) => {
    const s = scores[name] || {};
    const hasPassed = s.passed === true || s.passed === false;
    return {
      name,
      value: typeof s.value === 'number' ? s.value : null,
      unit: s.unit || null,
      passed: hasPassed ? s.passed : null,
      kind: hasPassed ? 'objective' : 'measure',
    };
  });
}

// The two event kinds M26.4's brief calls out by name ("port command and fault events
// linked to the timeline") -- `altavista/cdm.py`'s `_VIEWER_EVENT_TYPE_BY_KIND` is the
// one place these label strings are minted server-side; mirrored here as the two
// literal strings this panel filters on (an event's `type` is already an opaque label
// as far as the viewer is concerned -- web/js/cdm_run.js's `eventKinds()` doc comment --
// so there is no enum to import from the client side).
const TIMELINE_LINKED_KINDS = new Set(['fault', 'port_command']);

/**
 * `sc.events` (already-published wire shape: `{name, t, type, spacecraft, detail}`,
 * `altavista/model.py`'s `Event.to_dict()`) filtered to the two kinds this panel links
 * to the timeline, sorted by epoch. A thin accessor, like `cdm_run.js`'s `eventKinds()`
 * -- no re-shaping beyond filtering/sorting, so a caller reading `.t`/`.detail` off a
 * returned row is reading the exact same value `web/js/app.js`'s own event list/ticks
 * already use for that event (never a second, independently-derived copy of it).
 * @param {Array<{name:string,t:number,type:string,spacecraft?:string,detail?:string}>|null|undefined} events
 */
export function timelineEvents(events) {
  return (events || [])
    .filter((e) => e && TIMELINE_LINKED_KINDS.has(e.type))
    .slice()
    .sort((a, b) => a.t - b.t);
}

/**
 * Where a timeline-linked event sits along `[t0, t1]`, as a 0..100 percentage -- the
 * EXACT same arithmetic `web/js/app.js`'s `buildTicks()` already computes inline for
 * every event tick (`(ev.t - clock.t0) / span * 100`), extracted here so this panel's
 * own "linked to the timeline" ticks/labels can never silently drift from where
 * app.js's own timeline actually places that epoch. Returns `null` for a degenerate
 * `t1 <= t0` span (never divides by zero or a negative span).
 */
export function eventTimelinePercent(ev, t0, t1) {
  const span = t1 - t0;
  if (!(span > 0)) return null;
  return ((ev.t - t0) / span) * 100;
}

// -------------------------------------------------------------------------------- DOM
function fmtValue(v) {
  if (v === null || v === undefined) return '--';
  const av = Math.abs(v);
  if (av !== 0 && (av >= 1e6 || av < 1e-3)) return v.toExponential(4);
  return v.toFixed(3);
}

/**
 * Render this panel's content into `container` (an existing, empty DOM element --
 * matches this codebase's "re-parent existing content, do not build new state on top
 * of the live DOM until told" style, though here `container` is a panel body owned
 * outright by this module, not a shared/re-parented node like #sidebar/#viewport).
 * `onJumpToEvent(ev)` mirrors web/js/app.js's own event-list "go" click handler
 * (`setTime(ev.t, true)`) -- passed in rather than imported, so this module has no
 * dependency on app.js's clock state.
 * @param {HTMLElement} container
 * @param {{scores?: object, events?: object[], t0?: number, t1?: number, onJumpToEvent?: (ev:object)=>void}} data
 */
export function render(container, data) {
  container.innerHTML = '';
  const { scores, events, t0, t1, onJumpToEvent } = data || {};

  const scoresSection = document.createElement('div');
  scoresSection.className = 'av-panel-section';
  const scoresTitle = document.createElement('h4');
  scoresTitle.textContent = 'Objectives & measures';
  scoresSection.appendChild(scoresTitle);

  const rows = objectiveRows(scores);
  if (rows.length === 0) {
    const notice = document.createElement('p');
    notice.className = 'av-panel-notice';
    notice.textContent = 'This run declared no objectives or measures.';
    scoresSection.appendChild(notice);
  } else {
    const table = document.createElement('table');
    table.className = 'av-scores-table';
    for (const row of rows) {
      const tr = document.createElement('tr');
      tr.className = `av-score-row av-score-${row.kind}`;
      const nameTd = document.createElement('td');
      nameTd.textContent = row.name;
      const valueTd = document.createElement('td');
      valueTd.textContent = fmtValue(row.value);
      const passTd = document.createElement('td');
      if (row.kind === 'objective') {
        passTd.textContent = row.passed ? 'PASS' : 'FAIL';
        passTd.className = row.passed ? 'av-pass' : 'av-fail';
      } else {
        passTd.textContent = 'measure';
        passTd.className = 'av-measure';
      }
      tr.append(nameTd, valueTd, passTd);
      table.appendChild(tr);
    }
    scoresSection.appendChild(table);
  }
  container.appendChild(scoresSection);

  const eventsSection = document.createElement('div');
  eventsSection.className = 'av-panel-section';
  const eventsTitle = document.createElement('h4');
  eventsTitle.textContent = 'Port commands & faults';
  eventsSection.appendChild(eventsTitle);
  const linked = timelineEvents(events);
  if (linked.length === 0) {
    const notice = document.createElement('p');
    notice.className = 'av-panel-notice';
    notice.textContent = 'No port-command or fault events in this scenario.';
    eventsSection.appendChild(notice);
  } else {
    const list = document.createElement('ul');
    list.className = 'av-timeline-events';
    for (const ev of linked) {
      const li = document.createElement('li');
      li.className = `av-timeline-event av-timeline-event-${ev.type}`;
      const label = document.createElement('span');
      label.className = 'name';
      label.textContent = `${ev.type === 'fault' ? 'FAULT' : 'PORT CMD'} · ${ev.name}${ev.spacecraft ? ' · ' + ev.spacecraft : ''}`;
      label.title = ev.detail || '';
      const go = document.createElement('span');
      go.className = 'go';
      const pct = eventTimelinePercent(ev, t0, t1);
      go.textContent = pct === null ? '' : `${pct.toFixed(1)}%`;
      go.title = 'jump to event on the timeline';
      if (onJumpToEvent) go.addEventListener('click', () => onJumpToEvent(ev));
      li.append(label, go);
      list.appendChild(li);
    }
    eventsSection.appendChild(list);
  }
  container.appendChild(eventsSection);
}
