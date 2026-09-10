// M25.3d/M25.3e (docs/sil-plan.md's M25 milestone: "telemetry into the viewer"): pure,
// DOM-free helpers that give the timeline's contact-window and command-transition
// events real structure instead of the opaque `Event.type` label
// `web/js/app.js`'s `buildTicks`/`buildLists` and `web/js/panels/run_products_panel.js`
// otherwise treat every event kind as (`web/js/cdm_run.js`'s `eventKinds()` doc
// comment). No DOM, no import of app.js/scene.js -- testable headlessly under plain
// `node` (`web/js/timeline_check.mjs`), the same convention this codebase's other
// pure-logic modules (`web/js/cdm_run.js`, `web/js/panels/*.js`) already use.
//
// ## Where the data this module reads actually comes from
//
// `altavista/cdm.py`'s `cdm_event_to_viewer_event` (~line 808) is the one place a real
// `altavista.v1.Event` (what `crates/av-kernel/src/drm/events.rs::contact_event` and
// `crates/av-kernel/src/drm/command.rs::transition_event` actually emit) becomes the
// wire-shape `{name, t, type, spacecraft, detail, referenceId, attributes}` this
// module's functions consume (`altavista/model.py`'s `Event.to_dict()`).
//
// As of M25.3e (docs/open-questions.md question 177) `referenceId` (the real
// `ev.reference_id` -- for a command transition, the Command id) and `attributes` (the
// real `ev.provenance.attributes`, where `ack_level` lives on a command's ACKED
// transition) reach the viewer directly -- before this task BOTH were dropped, forcing
// [`groupCommandTransitions`] to recover a command's id from `detail`'s free text and
// leaving `ackLevel` always `null` (see `docs/open-questions.md` question 177 and
// `drms/M25_3E_REPORT.md` for the full before/after account). [`parseCommandId`] stays,
// but only as a FALLBACK for an event with no `referenceId` (e.g. a scenario published
// before this task, or by a producer that never sets it) -- `groupCommandTransitions`
// below prefers the real field and only falls back to parsing `detail` when it is
// absent.
//
// `ev.provenance` still has no dedicated slot for the contact counterpart (a contact
// event's own `reference_id` is not the counterpart -- `events.rs::contact_event` never
// sets one), so [`parseContactCounterpart`] is NOT a fallback -- it is still the only
// way to recover it, from `detail`'s free text, exactly as before.
//
// ## Contact windows: recovering the (station, counterpart) pairing key
//
// `crates/av-kernel/src/drm/events.rs::contact_event` builds `detail` as:
//   format!("ground instance {:?}: {} (target {:?})", cmd.instance, name, sender)
// e.g. `ground instance "ground": contact_start (target "flight")`. `cmd.instance`
// (the receiving/ground side) already reaches the viewer as `spacecraft`
// (`entity_id` -> `spacecraft`, `cdm_event_to_viewer_event`'s own mapping); `sender`
// (the OTHER side of the pass -- the tracked spacecraft) does not have a field of its
// own on the viewer `Event` at all, so [`parseContactCounterpart`] recovers it from
// `detail`'s own `(target "...")` suffix, Rust `{:?}`-escaped exactly like a Rust
// string literal (`\"`/`\\` -- see that function's own escape handling).
//
// ## Command transitions: which command a transition belongs to
//
// `crates/av-kernel/src/drm/command.rs::transition_event` sets `reference_id = cmd.id`
// on the real `Event` (now carried through to the wire as `referenceId`) and builds
// `detail` as:
//   format!("command {:?} ({}): {} -> {}", cmd.id, cmd.command_class, state.as_str_name(), reason)
// e.g. `command "cmd1" (drag_sail): COMMAND_STATE_PROPOSED -> declared in Scenario.events`.
// [`groupCommandTransitions`] groups by `ev.referenceId` when present (the normal case
// as of M25.3e), falling back to [`parseCommandId`] (which recovers `cmd.id` from
// `detail` the same way) only when `referenceId` is absent. The real `CommandState`
// name (`state.as_str_name()`, e.g. `"COMMAND_STATE_ACKED"`) is ALSO `ev.name` verbatim
// (`transition_event`'s own `name: state.as_str_name().to_string()`, carried through
// unchanged by `cdm_event_to_viewer_event`'s `name=ev.name or ev.id`) -- so this module
// never needs to parse the state out of `detail` at all.

// Rust `{:?}` (Debug) on a `&str`/`String` wraps it in `"..."` and escapes only `\`
// and `"` (plus control characters, never seen in these DRM-declared ids/instance
// names) -- so `(?:[^"\\]|\\.)*` inside a quoted pair round-trips every id this
// codebase's own DRMs declare (`drms/demo_ground_command.drm.yaml`'s `id: cmd1`,
// `drms/demo_ground_segment.sos.yaml`'s `ground`/`flight` instances, etc).
const QUOTED = '((?:[^"\\\\]|\\\\.)*)';
const CONTACT_TARGET_RE = new RegExp(`\\(target "${QUOTED}"\\)\\s*$`);
const COMMAND_ID_RE = new RegExp(`^command "${QUOTED}"\\s*\\(`);

function unescapeRustDebugString(s) {
  return s.replace(/\\(.)/g, '$1');
}

/**
 * The tracked spacecraft (`sender`) named in a `contact_start`/`contact_end` event's
 * `detail` -- see this module's own doc comment's "Contact windows" section for the
 * exact `format!` this reverses. Returns `null` (never a guess) when `detail` is
 * missing or does not match that exact shape.
 * @param {string|null|undefined} detail
 * @returns {string|null}
 */
export function parseContactCounterpart(detail) {
  if (!detail) return null;
  const m = CONTACT_TARGET_RE.exec(detail);
  return m ? unescapeRustDebugString(m[1]) : null;
}

/**
 * The `Command.id` (`cmd.id`) named in a `command_transition` event's `detail` -- see
 * this module's own doc comment's "Command transitions" section for the exact
 * `format!` this reverses. As of M25.3e (question 177) this is only a FALLBACK: an
 * event's own `referenceId` (when present) carries the same id directly and
 * [`groupCommandTransitions`] prefers it. Returns `null` (never a guess) when `detail`
 * is missing or does not match that exact shape.
 * @param {string|null|undefined} detail
 * @returns {string|null}
 */
export function parseCommandId(detail) {
  if (!detail) return null;
  const m = COMMAND_ID_RE.exec(detail);
  return m ? unescapeRustDebugString(m[1]) : null;
}

/**
 * Generic start/end pairing into spanned windows -- the engine [`pairContactWindows`]
 * and [`pairDecodeErrorWindows`] both build on, so the matching rules below are
 * written and tested exactly once rather than twice. `isStart`/`isEnd` select which
 * events belong to this pairing at all (every other event is ignored, passed through
 * untouched by whichever caller also wants it); `keyFor` computes the pairing key a
 * start and its matching end must share (e.g. station+counterpart for a contact
 * window, spacecraft+port for a decode-error window) -- two different keys never
 * cross-pair, even when they share a `type`/`name`.
 *
 * Matching is strictly chronological per pairing key: a start for a key that already
 * has a pending (unclosed) start makes that EARLIER start unmatched (never silently
 * overwritten, never silently paired with the wrong end) before the new one becomes
 * pending; an end for a key with no pending start is unmatched too; any start still
 * pending once every event has been consumed is unmatched. Nothing is ever dropped --
 * every matched `isStart`/`isEnd` event in `events` ends up in exactly one of
 * `windows` (as `start` or `end`) or `unmatched`.
 *
 * @param {Array<object>|null|undefined} events
 * @param {{isStart: (ev:object)=>boolean, isEnd: (ev:object)=>boolean, keyFor: (ev:object)=>string}} opts
 * @returns {{
 *   windows: Array<{start:object, end:object, startT:number, endT:number, durationT:number}>,
 *   unmatched: Array<{event:object, reason:string}>,
 * }}
 */
export function pairEventWindows(events, { isStart, isEnd, keyFor }) {
  const relevant = (events || [])
    .filter((e) => e && (isStart(e) || isEnd(e)))
    .slice()
    .sort((a, b) => a.t - b.t);

  const pending = new Map(); // key -> pending start event
  const windows = [];
  const unmatched = [];

  for (const ev of relevant) {
    const key = keyFor(ev);
    if (isStart(ev)) {
      const prior = pending.get(key);
      if (prior) {
        unmatched.push({ event: prior, reason: 'no matching end before the next start for this pairing key' });
      }
      pending.set(key, ev);
    } else {
      const start = pending.get(key);
      if (start) {
        pending.delete(key);
        windows.push({ start, end: ev, startT: start.t, endT: ev.t, durationT: ev.t - start.t });
      } else {
        unmatched.push({ event: ev, reason: 'no matching start for this pairing key' });
      }
    }
  }
  for (const start of pending.values()) {
    unmatched.push({ event: start, reason: 'no matching end for this pairing key' });
  }
  // Chronological output (Map iteration order for leftover `pending` entries is
  // insertion order, not necessarily time order once multiple keys are involved).
  unmatched.sort((a, b) => a.event.t - b.event.t);
  return { windows, unmatched };
}

/**
 * Pairs `contact_start`/`contact_end` events into spanned windows -- a start and its
 * matching end for the SAME (station, counterpart) pair, never a different pair's
 * event (the whole point of keying on `spacecraft` (the ground/receiving instance,
 * already a real field) PLUS the counterpart parsed out of `detail` -- two concurrent
 * passes at two different stations, or two different spacecraft passing the same
 * station back-to-back, never cross-pair). Built on [`pairEventWindows`] -- see that
 * function's own doc comment for the shared matching rules (chronological, per key,
 * nothing ever dropped).
 *
 * @param {Array<{name:string,t:number,type:string,spacecraft?:string,detail?:string}>|null|undefined} events
 * @returns {{
 *   windows: Array<{kind:'contact', spacecraft:string|null, counterpart:string|null, start:object, end:object, startT:number, endT:number, durationT:number}>,
 *   unmatched: Array<{event:object, reason:string}>,
 * }}
 */
export function pairContactWindows(events) {
  const { windows, unmatched } = pairEventWindows(events, {
    isStart: (e) => e.type === 'contact_start',
    isEnd: (e) => e.type === 'contact_end',
    keyFor: (ev) => `${ev.spacecraft || ''} ${parseContactCounterpart(ev.detail) || ''}`,
  });
  return {
    windows: windows.map((w) => ({
      kind: 'contact',
      spacecraft: w.start.spacecraft || null,
      counterpart: parseContactCounterpart(w.start.detail),
      start: w.start, end: w.end, startT: w.startT, endT: w.endT, durationT: w.durationT,
    })),
    // Restores this function's own pre-generalisation reason wording (`pairEventWindows`'s own
    // reason text is deliberately generic, shared with `pairDecodeErrorWindows`) -- kept
    // specific here since callers (and this repo's own existing tests) already match on the
    // literal "contact_start"/"contact_end" substrings.
    unmatched: unmatched.map((u) => ({
      event: u.event,
      reason: u.event.type === 'contact_start'
        ? 'no matching contact_end before the next contact_start for this station/counterpart pair'
        : 'no matching contact_start for this station/counterpart pair',
    })),
  };
}

/**
 * Pairs `decode_error_start`/`decode_error_end` events (`docs/open-questions.md`
 * question 193, R6.2 -- `crates/av-kernel/src/drm/events.rs::decode_error_start_event`/
 * `decode_error_end_event`) into spanned windows -- one decode-error EPISODE per
 * (instance, port), never a different instance's or port's episode. Unlike
 * [`pairContactWindows`], both pairing-key fields already reach the viewer as real
 * fields (`spacecraft` off `entity_id`, `referenceId` off `reference_id` -- the
 * receiving port), so no `detail`-parsing fallback is needed here at all.
 *
 * **Matched on `ev.name`, not `ev.type`.** Every fault-shaped event this crate emits
 * (`fault_event`/`port_fault_event`/`sensor_fault_event`/`decode_error_start_event`/
 * `decode_error_end_event`) carries `Event.kind == EVENT_KIND_FAULT`, which
 * `altavista/cdm.py`'s own `cdm_event_to_viewer_event` maps to the SAME `type ==
 * "fault"` for every one of them -- `type` cannot tell a decode-error event apart
 * from a plain DYNAMICS/PORT/SENSOR fault event at all. `Event.name` is what
 * distinguishes them (`"decode_error_start"`/`"decode_error_end"`, set verbatim by
 * the Rust builders above), so this function keys off `ev.name`, mirroring
 * [`groupCommandTransitions`]'s own "prefer the real field" discipline rather than
 * [`pairContactWindows`]'s `ev.type` (contact events get their OWN distinct
 * `EventKind`, so `type` already discriminates them -- a decode-error event does not
 * have that luxury, sharing `EVENT_KIND_FAULT` with every other fault event).
 *
 * @param {Array<{name:string,t:number,type:string,spacecraft?:string,referenceId?:string|null,detail?:string}>|null|undefined} events
 * @returns {{
 *   windows: Array<{kind:'decode_error', spacecraft:string|null, port:string|null, start:object, end:object, startT:number, endT:number, durationT:number}>,
 *   unmatched: Array<{event:object, reason:string}>,
 * }}
 */
export function pairDecodeErrorWindows(events) {
  const { windows, unmatched } = pairEventWindows(events, {
    isStart: (e) => e.name === 'decode_error_start',
    isEnd: (e) => e.name === 'decode_error_end',
    keyFor: (ev) => `${ev.spacecraft || ''} ${ev.referenceId || ''}`,
  });
  return {
    windows: windows.map((w) => ({
      kind: 'decode_error',
      spacecraft: w.start.spacecraft || null,
      port: w.start.referenceId || null,
      start: w.start, end: w.end, startT: w.startT, endT: w.endT, durationT: w.durationT,
    })),
    unmatched,
  };
}

/**
 * Groups `command_transition` events by the command they belong to -- `ev.referenceId`
 * when present (M25.3e, question 177: the real `Event.reference_id`, `cmd.id` on the
 * Rust side), falling back to [`parseCommandId`] (recovered from `detail`'s free text)
 * only for an event with no `referenceId` at all. Each command's own `transitions` are
 * sorted by epoch and carry the REAL `CommandState` name verbatim off `ev.name`
 * (`COMMAND_STATE_PROPOSED`, ..., `COMMAND_STATE_ACKED`/`_REJECTED`/`_EXPIRED`/
 * `_FAILED` -- `proto/altavista/v1/command.proto`'s own enum, never paraphrased).
 *
 * `ackLevel` is the real `AckLevel` name (e.g. `"ACK_LEVEL_ASSET_EXECUTED"`) read off
 * whichever transition in the group carries a truthy `ev.attributes.ack_level` (the
 * real `command.rs::transition_event` only ever sets it on the ACKED transition), or
 * `null` if none does (a command that never reached ACKED, or -- before M25.3e, or for
 * an event whose `attributes` this module was never given -- a scenario where the level
 * genuinely never reached the viewer at all). Never fabricated: this function only ever
 * copies a value that was actually present on some transition in the group, never
 * invents one for a command that has none.
 *
 * A `command_transition` event with neither a `referenceId` nor a `detail` matching the
 * expected `command "<id>" (...)` shape (so no command id can be recovered at all) is
 * returned separately, in `unparsed`, rather than silently dropped or grouped under a
 * guessed id.
 *
 * @param {Array<{name:string,t:number,type:string,spacecraft?:string,detail?:string,referenceId?:string|null,attributes?:Object<string,string>}>|null|undefined} events
 * @returns {{
 *   commands: Array<{commandId:string, spacecraft:string|null, transitions:Array<{state:string,t:number,detail:string|null}>, ackLevel:string|null}>,
 *   unparsed: Array<object>,
 * }}
 */
export function groupCommandTransitions(events) {
  const transitions = (events || [])
    .filter((e) => e && e.type === 'command_transition')
    .slice()
    .sort((a, b) => a.t - b.t);

  const byCommand = new Map();
  const order = [];
  const unparsed = [];
  for (const ev of transitions) {
    const commandId = (ev.referenceId != null && ev.referenceId !== '') ? ev.referenceId : parseCommandId(ev.detail);
    if (commandId == null) { unparsed.push(ev); continue; }
    let entry = byCommand.get(commandId);
    if (!entry) {
      entry = { commandId, spacecraft: ev.spacecraft || null, transitions: [], ackLevel: null };
      byCommand.set(commandId, entry);
      order.push(commandId);
    }
    entry.transitions.push({ state: ev.name, t: ev.t, detail: ev.detail || null });
    const ackLevel = ev.attributes && ev.attributes.ack_level;
    if (ackLevel) entry.ackLevel = ackLevel;
  }
  return { commands: order.map((id) => byCommand.get(id)), unparsed };
}

/**
 * The full render plan for `web/js/app.js`'s `buildTicks()` -- ALL of the "which kind
 * gets which visual treatment" logic lives here (pure, testable), so `buildTicks`
 * itself stays a thin DOM loop over this function's output, exactly like
 * `web/js/panels/run_products_panel.js`'s own "pure data-binding, one DOM function at
 * the bottom" split.
 *
 * - `windows`: spanned contact intervals ([`pairContactWindows`]'s own `windows`) PLUS
 *   spanned decode-error episodes ([`pairDecodeErrorWindows`]'s own `windows`, R6.2,
 *   question 193) -- both rendered as a bar from `startT` to `endT`, never a single
 *   point tick; each entry's own `kind` (`'contact'`/`'decode_error'`) says which.
 * - `unmatched`: contact events OR decode-error events with no partner (both
 *   functions' own `unmatched`) -- rendered distinctly from a matched window (never
 *   silently dropped, never merged into a fabricated window).
 * - `points`: every OTHER event (maneuver, fault, lifecycle, command_transition,
 *   marker, ...) as a single point tick, UNCHANGED from the pre-M25.3d rendering
 *   except that a `command_transition` carries its own `className`/`label` so it reads
 *   as visually and textually distinct from every other kind -- `label` is the real
 *   `CommandState` name already on `ev.name` (`COMMAND_STATE_PROPOSED`, ...,
 *   `COMMAND_STATE_ACKED`/`_REJECTED`/`_EXPIRED`/`_FAILED`), plus `ev.spacecraft` for
 *   context, never a paraphrase. `contact_start`/`contact_end`/`decode_error_start`/
 *   `decode_error_end` events never appear here -- they are exhaustively accounted for
 *   in `windows`/`unmatched` above, so neither is ALSO rendered as a generic point tick.
 *
 * @param {Array<{name:string,t:number,type:string,spacecraft?:string,detail?:string,referenceId?:string|null}>|null|undefined} events
 * @returns {{
 *   windows: Array<ReturnType<typeof pairContactWindows>['windows'][number] | ReturnType<typeof pairDecodeErrorWindows>['windows'][number]>,
 *   unmatched: Array<ReturnType<typeof pairContactWindows>['unmatched'][number]>,
 *   points: Array<{event:object, className:string|null, label:string}>,
 * }}
 */
export function timelineTickPlan(events) {
  const contact = pairContactWindows(events);
  const decodeError = pairDecodeErrorWindows(events);
  const windows = [...contact.windows, ...decodeError.windows];
  const unmatched = [...contact.unmatched, ...decodeError.unmatched];

  const pairedEvents = new Set();
  for (const w of windows) { pairedEvents.add(w.start); pairedEvents.add(w.end); }
  for (const u of unmatched) pairedEvents.add(u.event);

  const points = (events || [])
    .filter((e) => e && !pairedEvents.has(e))
    .map((e) => {
      if (e.type === 'command_transition') {
        return { event: e, className: 'tick-command-transition', label: `${e.name}${e.spacecraft ? ' · ' + e.spacecraft : ''}` };
      }
      return { event: e, className: null, label: e.name };
    });
  return { windows, unmatched, points };
}
