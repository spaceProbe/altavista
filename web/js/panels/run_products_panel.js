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
//
// M25.3e (docs/open-questions.md question 174, "Mirrors question 165"):
// `POST /api/cdm/run` now also threads `RunProducts.measurements` into the published
// scenario as an additive `measurements` key -- a list of
// `{id, epoch, sensorId, frameId, z, r}` (`altavista/server.py`'s
// `_measurement_to_dict`), plus `meta.measurementsSource = "RunProducts.measurements"`.
// `measurementRows`/`measurementCountsBySensor` below are this panel's own binding
// logic for that data (mirrors `objectiveRows`'s "pure function, tested headlessly"
// convention); `web/js/app.js`'s `buildTicks` places one small tick per measurement
// epoch on the timeline itself, using `timelineMeasurementTicks` below.

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

// M25.3d/M25.3e (docs/sil-plan.md's M25 milestone: "telemetry into the viewer"):
// command acknowledgements -- `groupCommandTransitions` (web/js/timeline_events.js)
// groups by the command's real `referenceId` as of M25.3e (docs/open-questions.md
// question 177), falling back to parsing `detail`'s free text only when an event
// carries no `referenceId`. `ackLevel` is now the real `AckLevel` name
// (`proto/altavista/v1/command.proto`'s own `ACK_LEVEL_EDGE`/`_ASSET_RECEIVED`/
// `_ASSET_EXECUTED`) read off the real CDM `Event.provenance.attributes["ack_level"]`
// (carried through to the wire as `attributes` by `altavista/cdm.py`'s
// `cdm_event_to_viewer_event`), or `null` for a command that never reached ACKED --
// see `web/js/timeline_events.js`'s own doc comment on `groupCommandTransitions` for
// the full contract, and `drms/M25_3E_REPORT.md` for the before/after account.
export { groupCommandTransitions } from '../timeline_events.js';
import { groupCommandTransitions } from '../timeline_events.js';

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

// -------------------------------------------------------------- measurements (M25.3e)
// `sc.measurements` (already-published wire shape: `{id, epoch, sensorId, frameId, z,
// r}`, `altavista/model.py`'s `ScenarioData.to_dict()`, question 174) -- these three
// functions are the pure binding logic this panel and `web/js/app.js`'s `buildTicks`
// both use; neither reshapes a value beyond sorting/counting, so a caller reading
// `.id`/`.epoch` off a returned row is reading the exact wire value, never a derived
// or re-computed one.

/**
 * `measurements` sorted by epoch (ascending) -- the server already sorts
 * `RunProducts.measurements` by `(epoch, id)` (question 173), but this function does
 * not trust that silently: it re-sorts explicitly, so a caller of this module never
 * depends on an ordering guarantee it cannot see for itself.
 * @param {Array<{id:string,epoch:number,sensorId?:string,frameId?:string,z?:number[],r?:number[]}>|null|undefined} measurements
 * @returns {Array<{id:string,epoch:number,sensorId:string|null,frameId:string|null,zLen:number,rLen:number}>}
 */
export function measurementRows(measurements) {
  return (measurements || [])
    .slice()
    .sort((a, b) => a.epoch - b.epoch)
    .map((m) => ({
      id: m.id,
      epoch: m.epoch,
      // `sensorId`/`frameId` are carried through EXACTLY as the wire sent them --
      // deliberately `??`, not `||`: an empty string is a legitimate, common wire value
      // (question 174's own "frame_id empty for every measurement" is the NORMAL case
      // for demo_measurements, not an absent-field edge case), so it must not be
      // coerced to `null` alongside a genuinely missing field.
      sensorId: m.sensorId ?? null,
      frameId: m.frameId ?? null,
      zLen: Array.isArray(m.z) ? m.z.length : 0,
      // Deliberately NOT coerced to a fabricated length -- an empty `r` (e.g. a star
      // tracker's own unit-quaternion measurement, which declares no covariance) stays
      // exactly 0 here, matching the wire's own "nothing is ever synthesized" rule
      // (question 174) rather than being mistaken for "field missing".
      rLen: Array.isArray(m.r) ? m.r.length : 0,
    }));
}

/**
 * Measurement counts grouped by `sensorId`, sorted by sensor id -- this panel's own
 * "how much telemetry, from which sensor" summary, the measurement analogue of
 * `objectiveRows`' per-name grouping.
 * @param {Array<{sensorId?:string}>|null|undefined} measurements
 * @returns {Array<{sensorId:string, count:number}>}
 */
export function measurementCountsBySensor(measurements) {
  const counts = new Map();
  for (const m of measurements || []) {
    const key = (m && m.sensorId) || '(no sensorId)';
    counts.set(key, (counts.get(key) || 0) + 1);
  }
  return [...counts.entries()].sort((a, b) => a[0].localeCompare(b[0]))
    .map(([sensorId, count]) => ({ sensorId, count }));
}

/**
 * `measurements` as timeline tick descriptors -- `web/js/app.js`'s `buildTicks()` own
 * consumer, so a measurement's epoch is placed on the SAME timeline strip every other
 * tick uses, never a second, independently-computed position. One tick per
 * measurement (never merged/deduplicated by epoch -- e.g. this file's own module
 * comment's 12-epoch/3-id-per-epoch demo shape stays 3 distinct ticks per epoch, not 1).
 * @param {Array<{id:string,epoch:number,sensorId?:string}>|null|undefined} measurements
 * @returns {Array<{id:string, t:number, sensorId:string|null}>}
 */
export function timelineMeasurementTicks(measurements) {
  return (measurements || []).map((m) => ({ id: m.id, t: m.epoch, sensorId: m.sensorId || null }));
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
 * @param {{scores?: object, events?: object[], measurements?: object[], t0?: number, t1?: number, onJumpToEvent?: (ev:object)=>void}} data
 */
export function render(container, data) {
  container.innerHTML = '';
  const { scores, events, measurements, t0, t1, onJumpToEvent } = data || {};

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

  // M25.3d/M25.3e: command acknowledgements -- the states each command passed through,
  // using the real COMMAND_STATE_* names, from `command_transition` events (a SEPARATE
  // kind from the port-command/fault list above, filtered by TIMELINE_LINKED_KINDS --
  // never added to that set here, so M26.4's own "exactly fault+port_command" test
  // stays green). ackLevel is the real AckLevel name as of M25.3e (question 177) -- see
  // this file's own module-level comment on `groupCommandTransitions` for the full
  // contract.
  const ackSection = document.createElement('div');
  ackSection.className = 'av-panel-section';
  const ackTitle = document.createElement('h4');
  ackTitle.textContent = 'Command acknowledgements';
  ackSection.appendChild(ackTitle);
  const { commands, unparsed } = groupCommandTransitions(events);
  if (commands.length === 0) {
    const notice = document.createElement('p');
    notice.className = 'av-panel-notice';
    notice.textContent = 'No command transitions in this scenario.';
    ackSection.appendChild(notice);
  } else {
    const list = document.createElement('ul');
    list.className = 'av-command-list';
    for (const cmd of commands) {
      const li = document.createElement('li');
      li.className = 'av-command';
      const head = document.createElement('div');
      head.className = 'name';
      head.textContent = `${cmd.commandId}${cmd.spacecraft ? ' · ' + cmd.spacecraft : ''}`;
      const states = document.createElement('ol');
      states.className = 'av-command-states';
      for (const tr of cmd.transitions) {
        const item = document.createElement('li');
        item.textContent = tr.state;
        item.title = tr.detail || '';
        if (onJumpToEvent) {
          item.classList.add('go');
          item.addEventListener('click', () => onJumpToEvent({ t: tr.t }));
        }
        states.appendChild(item);
      }
      const ack = document.createElement('div');
      ack.className = 'av-panel-notice av-ack-level';
      // M25.3e (question 177): the real AckLevel, when this command has one (only ever
      // set on its ACKED transition) -- never fabricated for a command that has none
      // (still-pending, REJECTED, EXPIRED, FAILED all legitimately have no ack level).
      ack.textContent = cmd.ackLevel ? `ack level: ${cmd.ackLevel}` : 'ack level: not reported for this command';
      li.append(head, states, ack);
      list.appendChild(li);
    }
    ackSection.appendChild(list);
  }
  if (unparsed.length > 0) {
    const notice = document.createElement('p');
    notice.className = 'av-panel-notice';
    notice.textContent = `${unparsed.length} command_transition event(s) could not be linked to a command id and are not shown above.`;
    ackSection.appendChild(notice);
  }
  container.appendChild(ackSection);

  // M25.3e (question 174): telemetry (`sc.measurements`) -- per-sensor counts plus a
  // list of individual measurements, each linked to its own epoch on the timeline
  // (mirrors the "Port commands & faults" section above; `web/js/app.js`'s buildTicks
  // places the same epochs as ticks on the timeline strip itself, via
  // timelineMeasurementTicks).
  const measurementsSection = document.createElement('div');
  measurementsSection.className = 'av-panel-section';
  const measurementsTitle = document.createElement('h4');
  measurementsTitle.textContent = 'Telemetry';
  measurementsSection.appendChild(measurementsTitle);
  const mRows = measurementRows(measurements);
  if (mRows.length === 0) {
    const notice = document.createElement('p');
    notice.className = 'av-panel-notice';
    notice.textContent = 'This run declared no measurements.';
    measurementsSection.appendChild(notice);
  } else {
    const summary = document.createElement('p');
    summary.className = 'av-panel-notice';
    const bySensor = measurementCountsBySensor(measurements)
      .map((s) => `${s.sensorId}: ${s.count}`).join(', ');
    summary.textContent = `${mRows.length} total (${bySensor})`;
    measurementsSection.appendChild(summary);

    const list = document.createElement('ul');
    list.className = 'av-timeline-events';
    for (const m of mRows) {
      const li = document.createElement('li');
      li.className = 'av-timeline-event av-timeline-event-measurement';
      const label = document.createElement('span');
      label.className = 'name';
      label.textContent = `${m.id}${m.sensorId ? ' · ' + m.sensorId : ''}`;
      const go = document.createElement('span');
      go.className = 'go';
      const pct = eventTimelinePercent({ t: m.epoch }, t0, t1);
      go.textContent = pct === null ? '' : `${pct.toFixed(1)}%`;
      go.title = 'jump to measurement on the timeline';
      if (onJumpToEvent) go.addEventListener('click', () => onJumpToEvent({ t: m.epoch }));
      li.append(label, go);
      list.appendChild(li);
    }
    measurementsSection.appendChild(list);
  }
  container.appendChild(measurementsSection);
}
