// R3.5b (docs/aiplane-plan.md milestone A5's browser half): the command console panel.
// Same convention as web/js/panels/feasibility_panel.js (that file's own module comment,
// verbatim): pure, DOM-free data-binding functions in the top half of this file,
// independently testable under plain `node` (web/js/command_panel_check.mjs,
// tests/test_viewer_command_panel.py), exactly one DOM-touching `render()` at the bottom.
//
// The wire contract this panel binds to -- every route `altavista/server.py` declares
// under `/api/command/*` (R3.5a, already shipped at commit ac335ce; this file makes NO
// change to any of them, per this task's own scope fence):
//   GET  /api/command/proposals                     -> {proposals: [{commandId, entityId,
//        commandClass, hazardous, idempotencyKey, state, rationale, evidenceIds}]} --
//        despite the route's own name, `state` (question 209(a)/(b)) means these rows are
//        commands AWAITING A HUMAN, both COMMAND_STATE_PROPOSED and COMMAND_STATE_CHECKED,
//        not proposals alone.
//   GET  /api/command/commands/{id}/decision         -> {decisionId, allow, policyHash,
//        reasons, matchedRulePath, evaluatedTaiNs, input} (404 when not yet Checked)
//   GET  /api/command/commands/{id}/trail            -> {commandId, transitions:
//        [{state, taiNs, principal, reason, ackLevel, delegationId}, ...]} (wire order
//        IS the transition order -- never re-sorted, unlike this codebase's other
//        "never trust the wire's own claimed order silently" fields)
//   GET  /api/command/counters                       -> {fips, partitions, refusals,
//        run_id, version} (`altavista/command_client.py`'s own docstring: a direct proxy
//        of av-command's `/admin/api/evidence`)
//   POST /api/command/commands/{id}/authorize        -> {commandId, state, transitions}
//        on success; a real, typed refusal (never a generic 500) otherwise.
//
// This module never issues an HTTP request itself -- exactly like feasibility_panel.js/
// run_products_panel.js never do (see web/js/app.js's own
// `openFeasibilitySample`, the ONE place in this codebase that turns a panel's callback
// into a real network call). The caller (web/js/app.js in the real browser; a hand-built
// mock in the headless check) already has each route's raw JSON (or a caught
// `{status, message}` failure) and hands it to `render()` as plain data. This keeps the
// panel testable under plain `node` for every pure function below, and lets the ONE
// genuinely stateful action here -- authorize -- be exercised headlessly too (see this
// file's own render() doc comment on why the click handler still needs a document).
//
// ---------------------------------------------------------------- the token rule (201(b))
// The operator's token is read from a plain <input> once, forwarded to the caller's
// `onAuthorize(commandId, token)` callback, and the input's own value is cleared
// SYNCHRONOUSLY in the same click handler, before `onAuthorize`'s promise ever settles.
// This module does not persist the token in any browser storage, does not place it in any
// URL/query string, and does not write it to any log -- there is no such code anywhere
// below (grep this file for the literal API names a browser would use for any of those;
// web/js/command_panel_check.mjs's own check re-asserts that against this file's real,
// on-disk source text). `render()` is a full teardown/rebuild on every call (like
// feasibility_panel.js's own render(), never console_panel.js's accumulate-in-place
// exception) -- the caller re-renders with the *result* of authorize (never the token
// itself) once the promise settles, so the token cannot resurface in a later render even
// by accident: there is no code path that could hand it back in.

// ------------------------------------------------------------------------- proposals

/**
 * `{proposals: [...]}` (or a malformed/absent payload) -> an array of rows, each
 * carrying its rationale and evidence ids verbatim, sorted by `commandId` (this
 * module's own explicit, re-derived order -- never trusting the wire's own array order
 * silently, mirroring feasibility_panel.js's `scoreNames`/`gridAxes` doc comments).
 *
 * Question 209(b): despite the route's own name (`/api/command/proposals`, unchanged
 * for the browser panel's/tests' sake), the server now lists commands AWAITING A
 * HUMAN -- both `COMMAND_STATE_PROPOSED` and `COMMAND_STATE_CHECKED` (question 209(a):
 * `Check` runs automatically inside `Propose`, so CHECKED is the normal case a human
 * actually needs to act on) -- and each row carries the REAL `CommandState` enum name
 * verbatim under `"state"` (`altavista/command_client.py::list_pending_commands`,
 * `command_pb2.CommandState.Name(...)`). `row.state` is carried through EXACTLY as
 * received, never re-mapped/guessed here (this module's own standing rule -- see
 * `trailRows`'s identical rule for `.state`); a row with no `state` key at all (a
 * payload from before this round, or a malformed one) degrades to `''`, never an
 * invented `'COMMAND_STATE_PROPOSED'` guess.
 * @param {{proposals?: Array<object>}|null|undefined} payload
 * @returns {Array<{commandId:string, entityId:string, commandClass:string,
 *   hazardous:boolean, idempotencyKey:string, state:string, rationale:string,
 *   evidenceIds:string[]}>}
 */
export function proposalRows(payload) {
  if (!payload || !Array.isArray(payload.proposals)) return [];
  return payload.proposals
    .map((p) => ({
      commandId: (p && p.commandId) || '',
      entityId: (p && p.entityId) || '',
      commandClass: (p && p.commandClass) || '',
      hazardous: !!(p && p.hazardous),
      idempotencyKey: (p && p.idempotencyKey) || '',
      state: (p && p.state) || '',
      rationale: (p && p.rationale) || '',
      evidenceIds: Array.isArray(p && p.evidenceIds) ? [...p.evidenceIds] : [],
    }))
    .sort((a, b) => a.commandId.localeCompare(b.commandId));
}

/**
 * `row.state`'s real `CommandState` enum name, shortened for display by stripping the
 * `COMMAND_STATE_` prefix common to every value (`command_pb2.CommandState.Name(...)`,
 * e.g. `"COMMAND_STATE_CHECKED"` -> `"CHECKED"`) -- readability only, never a re-guess:
 * a value that doesn't carry that prefix (or is empty/unrecognized) is returned
 * unchanged, so this never invents a label the server didn't send. The RAW value stays
 * reachable regardless (`buildProposalsSection` below puts it verbatim in the cell's
 * `title`), so a caller that needs the exact wire string never loses it to this
 * shortening.
 * @param {string} state
 * @returns {string}
 */
export function shortCommandState(state) {
  const prefix = 'COMMAND_STATE_';
  return typeof state === 'string' && state.startsWith(prefix) ? state.slice(prefix.length) : (state || '');
}

// -------------------------------------------------------------------------- decision

/**
 * The decision payload, defensively copied -- `null` for "no decision yet" (the route's
 * own 404 case, surfaced by the caller as `data.decisionError`, never confused with this
 * function's `null`, which only ever means "nothing to show", not "something failed").
 * @param {object|null|undefined} payload
 * @returns {{decisionId:string, allow:boolean, policyHash:string, reasons:string[],
 *   matchedRulePath:string, evaluatedTaiNs:number|null, input:object|null}|null}
 */
export function decisionView(payload) {
  if (!payload) return null;
  return {
    decisionId: payload.decisionId || '',
    allow: payload.allow === true,
    policyHash: payload.policyHash || '',
    reasons: Array.isArray(payload.reasons) ? [...payload.reasons] : [],
    matchedRulePath: payload.matchedRulePath || '',
    evaluatedTaiNs: typeof payload.evaluatedTaiNs === 'number' ? payload.evaluatedTaiNs : null,
    input: payload.input || null,
  };
}

// ------------------------------------------------------------------------------ trail

/**
 * `{commandId, transitions: [...]}` -> the transitions, UNCHANGED IN ORDER (the wire
 * order IS the transition sequence -- re-sorting it would be actively wrong, unlike
 * every other "re-sort defensively" function in this codebase's panels). Each row's
 * `state`/`ackLevel` are carried through as the real `CommandState`/`AckLevel` enum
 * name strings the server already sends (`command_pb2.CommandState.Name(...)` etc,
 * `altavista/command_client.py`'s own `_transition_to_dict`) -- never re-mapped to a
 * shorter label here, so a caller reading `.state` is reading the exact wire value.
 * @param {{transitions?: Array<object>}|null|undefined} payload
 * @returns {Array<{state:string, taiNs:string|null, principal:string, reason:string,
 *   ackLevel:string, delegationId:string}>}
 */
export function trailRows(payload) {
  if (!payload || !Array.isArray(payload.transitions)) return [];
  return payload.transitions.map((t) => ({
    state: (t && t.state) || '',
    // A decimal STRING, never a number: TAI nanosecond epochs exceed Number.MAX_SAFE_INTEGER
    // by two orders of magnitude, so `JSON.parse` would round one silently (see
    // altavista/command_client.py::_int64). Kept as the string the server sent and rendered
    // as that string; a consumer that genuinely needs arithmetic on it must use BigInt.
    taiNs: typeof (t && t.taiNs) === 'string' && /^-?[0-9]+$/.test(t.taiNs) ? t.taiNs : null,
    principal: (t && t.principal) || '',
    reason: (t && t.reason) || '',
    ackLevel: (t && t.ackLevel) || '',
    delegationId: (t && t.delegationId) || '',
  }));
}

// --------------------------------------------------------------------------- counters

/**
 * `{fips, partitions, refusals, run_id, version}` -> the `refusals` map as a SORTED
 * array of `{name, count}` rows (the brief's own required "sorted" -- re-sorted
 * explicitly here, never trusting whatever key order the real JSON object happened to
 * arrive in, same posture as every other function in this module). `[]` for a
 * null/malformed payload, never a throw.
 * @param {{refusals?: Object<string, number>}|null|undefined} payload
 * @returns {Array<{name:string, count:number}>}
 */
export function counterRows(payload) {
  if (!payload || !payload.refusals || typeof payload.refusals !== 'object') return [];
  return Object.keys(payload.refusals)
    .sort()
    .map((name) => ({ name, count: payload.refusals[name] }));
}

/**
 * The non-`refusals` fields of the counters payload, carried through unchanged, or
 * `null` for a null/absent payload -- `render()`'s own "run id / version / fips /
 * partitions" info line uses this rather than re-reading `payload` itself, so every
 * field this panel shows is named once, here.
 * @param {object|null|undefined} payload
 * @returns {{runId:string, version:string, fips:boolean|null, partitions:*}|null}
 */
export function counterMeta(payload) {
  if (!payload) return null;
  return {
    runId: payload.run_id || '',
    version: payload.version || '',
    fips: typeof payload.fips === 'boolean' ? payload.fips : null,
    partitions: 'partitions' in payload ? payload.partitions : null,
  };
}

// ----------------------------------------------------------------------- honest errors

/**
 * A caller-supplied `{status, message}` (or `null`) -> a single human-readable line, or
 * `null` when there is nothing to say. NEVER a generic "something went wrong" -- the
 * brief's own explicit rule ("shown to the operator with the server's own reason, never
 * swallowed and never replaced by a generic message"); this function's only job is to
 * format the two fields it is given, never to invent a message when they are absent.
 * @param {{status?:number, message?:string}|null|undefined} err
 * @returns {string|null}
 */
export function errorLine(err) {
  if (!err || !err.message) return null;
  return typeof err.status === 'number' ? `(${err.status}) ${err.message}` : String(err.message);
}

// -------------------------------------------------------------------------------- DOM
// Colour/heat conventions do not apply here (no scalar to bucket) -- this panel is
// tables and lists, same structural convention as run_products_panel.js's own score
// tables.

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

function noticeEl(text) {
  return el('p', 'av-panel-notice', text);
}

function buildProposalsSection(rows, error, selectedCommandId, onSelectCommand) {
  const section = document.createElement('div');
  section.className = 'av-panel-section';
  // Question 209(b): this list is no longer PROPOSED-only (question 209(a): `Check`
  // runs automatically inside `Propose`, so a CHECKED command awaiting authorization
  // is now the normal case) -- the heading says what these rows actually are, never
  // "Proposals" alone, which would be dishonest about the CHECKED rows mixed in.
  section.appendChild(el('h4', null, 'Commands awaiting a human'));

  const line = errorLine(error);
  if (line) {
    const notice = noticeEl(`Cannot load commands awaiting a human: ${line}`);
    notice.className += ' av-command-error';
    section.appendChild(notice);
    return section;
  }
  if (rows.length === 0) {
    section.appendChild(noticeEl('No commands awaiting a human.'));
    return section;
  }

  const table = document.createElement('table');
  table.className = 'av-command-proposals';
  for (const row of rows) {
    const tr = document.createElement('tr');
    tr.className = row.commandId === selectedCommandId ? 'av-command-selected' : '';

    const idTd = el('td', null, row.commandId);
    const classTd = el('td', null, row.commandClass + (row.hazardous ? ' (hazardous)' : ''));
    // State column (question 209(b)): the shortened label is readable
    // ("CHECKED" rather than "COMMAND_STATE_CHECKED"), but the cell's `title` carries
    // the REAL, raw `CommandState` enum name verbatim -- never lost, only shortened
    // for display -- so a check (or an operator hovering) can verify the exact wire
    // value this row actually carries. A row with no state (`''`, `shortCommandState`
    // above) shows an honest placeholder rather than an empty cell.
    const stateTd = el('td', null, shortCommandState(row.state) || '(unknown)');
    stateTd.title = row.state;
    const rationaleTd = el('td', null, row.rationale);
    const evidenceTd = el('td', null, row.evidenceIds.join(', '));

    const openTd = document.createElement('td');
    const btn = document.createElement('button');
    btn.type = 'button';
    btn.className = 'av-pane-btn av-command-select';
    btn.textContent = 'View';
    btn.addEventListener('click', () => onSelectCommand && onSelectCommand(row.commandId));
    openTd.appendChild(btn);

    tr.append(idTd, classTd, stateTd, rationaleTd, evidenceTd, openTd);
    table.appendChild(tr);
  }
  section.appendChild(table);
  return section;
}

function buildDecisionSection(commandId, decision, error) {
  const section = document.createElement('div');
  section.className = 'av-panel-section';
  section.appendChild(el('h4', null, 'Policy decision'));

  if (!commandId) {
    section.appendChild(noticeEl('Select a command above to see its policy decision.'));
    return section;
  }
  const line = errorLine(error);
  if (line) {
    const notice = noticeEl(line);
    notice.className += ' av-command-error';
    section.appendChild(notice);
    return section;
  }
  if (!decision) {
    section.appendChild(noticeEl('No policy decision recorded for this command yet.'));
    return section;
  }

  const dl = document.createElement('dl');
  dl.className = 'av-command-decision';
  const addRow = (label, value) => {
    dl.appendChild(el('dt', null, label));
    dl.appendChild(el('dd', null, value));
  };
  addRow('decision id', decision.decisionId);
  addRow('policy hash', decision.policyHash);
  addRow('allow', decision.allow ? 'ALLOW' : 'DENY');
  addRow('matched rule', decision.matchedRulePath || '(none)');
  section.appendChild(dl);

  if (decision.reasons.length > 0) {
    const reasons = document.createElement('ul');
    reasons.className = 'av-command-reasons';
    for (const r of decision.reasons) reasons.appendChild(el('li', null, r));
    section.appendChild(reasons);
  }
  return section;
}

function buildTrailSection(commandId, rows, error) {
  const section = document.createElement('div');
  section.className = 'av-panel-section';
  section.appendChild(el('h4', null, 'Trail'));

  if (!commandId) {
    section.appendChild(noticeEl('Select a command above to see its trail.'));
    return section;
  }
  const line = errorLine(error);
  if (line) {
    const notice = noticeEl(line);
    notice.className += ' av-command-error';
    section.appendChild(notice);
    return section;
  }
  if (rows.length === 0) {
    section.appendChild(noticeEl('No transitions recorded for this command.'));
    return section;
  }

  const table = document.createElement('table');
  table.className = 'av-command-trail';
  for (const t of rows) {
    const tr = document.createElement('tr');
    tr.append(
      el('td', null, t.state),
      el('td', null, String(t.taiNs)),
      el('td', null, t.principal),
      el('td', null, t.reason),
      el('td', null, t.ackLevel),
      el('td', null, t.delegationId || '(none)'),
    );
    table.appendChild(tr);
  }
  section.appendChild(table);
  return section;
}

function buildCountersSection(rows, meta, error) {
  const section = document.createElement('div');
  section.className = 'av-panel-section';
  section.appendChild(el('h4', null, 'Counters'));

  const line = errorLine(error);
  if (line) {
    const notice = noticeEl(`Cannot load counters: ${line}`);
    notice.className += ' av-command-error';
    section.appendChild(notice);
    return section;
  }
  if (meta) {
    section.appendChild(noticeEl(`run ${meta.runId || '(unknown)'} -- ${meta.version || '(unknown version)'}`));
  }
  if (rows.length === 0) {
    section.appendChild(noticeEl('No refusals recorded.'));
    return section;
  }
  const list = document.createElement('ul');
  list.className = 'av-command-counters';
  for (const r of rows) list.appendChild(el('li', null, `${r.name}: ${r.count}`));
  section.appendChild(list);
  return section;
}

/**
 * The authorize control -- the ONE state-changing action this panel offers (question
 * 53: propose-only stands; there is no dispatch/ack/expire/fail control anywhere in
 * this file). `commandId` is the currently selected command (from the proposals
 * section above); when it is falsy the control is present but disabled, never hidden
 * (an operator should see the control exists, and why it cannot be used yet).
 *
 * The token: read from `tokenInput.value` inside the click handler, and
 * `tokenInput.value` is set back to `''` in the SAME synchronous handler, before
 * `onAuthorize`'s returned promise is even awaited -- see this module's own top-of-file
 * doc comment ("the token rule") for why this ordering is the whole point. Nothing here
 * writes the token anywhere else: not to a data attribute, not to `section`'s own
 * state, not to a variable that outlives this function call.
 * @param {string|null} commandId
 * @param {{ok:boolean, status?:number, message?:string}|null} lastResult the caller's
 *   own record of the most recent authorize attempt's outcome (never the token that
 *   produced it) -- rendered honestly below, success or refusal alike.
 * @param {(commandId:string, token:string)=>Promise<object>} [onAuthorize]
 */
function buildAuthorizeSection(commandId, lastResult, onAuthorize) {
  const section = document.createElement('div');
  section.className = 'av-panel-section';
  section.appendChild(el('h4', null, 'Authorize'));

  const label = el('label', 'av-command-token-label', 'Operator token: ');
  const tokenInput = document.createElement('input');
  tokenInput.type = 'password';
  tokenInput.className = 'av-command-token-input';
  tokenInput.placeholder = 'paste one-time operator token';
  label.appendChild(tokenInput);
  section.appendChild(label);

  const btn = document.createElement('button');
  btn.type = 'button';
  btn.className = 'av-pane-btn av-command-authorize-btn';
  btn.textContent = 'Authorize';
  btn.disabled = !commandId;
  if (!commandId) {
    btn.title = 'Select a command above first.';
  }
  btn.addEventListener('click', () => {
    if (!commandId) return; // disabled, but a defensive no-op if triggered anyway
    const token = tokenInput.value;
    tokenInput.value = ''; // cleared SYNCHRONOUSLY -- see this function's own doc comment
    if (!token) return;
    if (onAuthorize) onAuthorize(commandId, token);
  });
  section.appendChild(btn);

  // The most recent outcome -- success or refusal -- shown with the SERVER's own text,
  // never a generic "authorize failed" (the brief's own explicit rule). `lastResult`
  // never carries the token (see this function's own doc comment); there is nothing
  // here that could leak it even if a caller made a mistake upstream, because this
  // function never reads anything off `lastResult` except `ok`/`status`/`message`/
  // `state`.
  if (lastResult) {
    if (lastResult.ok) {
      section.appendChild(el('p', 'av-panel-notice av-command-authorize-ok', `Authorized -- new state ${lastResult.state || '(unknown)'}.`));
    } else {
      const notice = noticeEl(`Authorize refused: ${errorLine(lastResult) || '(no reason given)'}`);
      notice.className += ' av-command-error';
      section.appendChild(notice);
    }
  }
  return section;
}

/**
 * Render this panel's content into `container` (an existing, empty DOM element -- same
 * contract as feasibility_panel.js's own `render()`). Full teardown/rebuild on every
 * call; the caller (web/js/app.js) owns every piece of mutable state
 * (`selectedCommandId`, the last authorize outcome) and re-renders after each fetch or
 * authorize attempt settles, exactly like `feasibilityState` in web/js/app.js already
 * does for the feasibility panel.
 *
 * Degraded cases (this task's own required "no blank panel, no spinner that never
 * resolves" rule): a `proposalsError`/`decisionError`/`trailError`/`countersError` of
 * `{status, message}` renders that SECTION's own honest message (never a generic one,
 * never silently dropped) while every other section still renders normally from
 * whatever data it does have -- one section's failure never blanks the whole panel.
 * @param {HTMLElement} container
 * @param {{proposals?: object|null, proposalsError?: {status:number,message:string}|null,
 *   selectedCommandId?: string|null, onSelectCommand?: (commandId:string)=>void,
 *   decision?: object|null, decisionError?: {status:number,message:string}|null,
 *   trail?: object|null, trailError?: {status:number,message:string}|null,
 *   counters?: object|null, countersError?: {status:number,message:string}|null,
 *   authorizeResult?: {ok:boolean,status?:number,message?:string,state?:string}|null,
 *   onAuthorize?: (commandId:string, token:string)=>Promise<object>}} data
 */
export function render(container, data) {
  container.innerHTML = '';
  const {
    proposals, proposalsError, selectedCommandId, onSelectCommand,
    decision, decisionError, trail, trailError,
    counters, countersError, authorizeResult, onAuthorize,
  } = data || {};

  container.appendChild(buildProposalsSection(proposalRows(proposals), proposalsError, selectedCommandId, onSelectCommand));
  container.appendChild(buildDecisionSection(selectedCommandId, decisionView(decision), decisionError));
  container.appendChild(buildTrailSection(selectedCommandId, trailRows(trail), trailError));
  container.appendChild(buildAuthorizeSection(selectedCommandId, authorizeResult, onAuthorize));
  container.appendChild(buildCountersSection(counterRows(counters), counterMeta(counters), countersError));
}
