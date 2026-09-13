#!/usr/bin/env node
// R3.5b (docs/aiplane-plan.md milestone A5's browser half): headless CLI harness for the
// command console panel (web/js/panels/command_panel.js) AND its execution-profile-only
// default-layout wiring (web/js/layout/default_layouts.js). `node command_panel_check.mjs
// <path-to-json>`, following the exact convention web/js/panels_check.mjs and
// web/js/layout/layout_tree_check.mjs already use: drives the REAL, shipped ES modules
// and prints one JSON object of named checks -- never a reimplementation of either
// module's logic. `tests/test_viewer_command_panel.py` builds the input JSON from a REAL,
// running `av-command` service (the same fixture shape `tests/test_command_console_
// routes.py` already uses) and a REAL `create_app(profile="execution", ...)` app.
//
// Section map (named here so tests/test_viewer_command_panel.py's own docstrings can
// point back at it, matching this codebase's own standing review-documentation rule):
//   1. proposalRows        -- real proposal (rationale/evidenceIds), empty list, null
//   2. decisionView        -- real decision id/policy hash/allow, null
//   3. trailRows           -- real transition sequence, WIRE ORDER preserved (never
//                              re-sorted -- the one field in this panel where order IS
//                              the data)
//   4. counterRows/Meta    -- real refusals, sorted; run id carried through
//   5. errorLine           -- formatting only
//   6. degraded cases      -- REAL captured server refusal text (not configured, not yet
//                              Checked, wrong-role authorize refusal) rendered honestly,
//                              per section, never a generic message
//   7. render() assembly   -- a fake DOM (this file's own, no jsdom -- same posture as
//                              web/js/layout/layout_tree_check.mjs's LayoutManager
//                              section) proves real rendered TEXT, not just that
//                              functions returned truthy values
//   8. the token rule      -- click the real button built by render(), assert the
//                              caller's onAuthorize gets the exact token, the input is
//                              cleared SYNCHRONOUSLY, and the token never reappears in
//                              any later render's text -- plus a grep of this module's
//                              OWN on-disk source for browser storage/cookie APIs
//   9. layout               -- COMMAND_PANEL_ID registered/chooser-reachable in every
//                              profile; isExecutionProfile; defaultLayoutTreeForScenario
//                              adds the panel exactly once, at the documented share, for
//                              an execution-profile scenario in every shape (ordinary/
//                              RPO/sweep), and leaves every other profile's shape
//                              byte-identical to before this task.

import { readFileSync } from 'fs';
import { fileURLToPath } from 'url';
import path from 'path';
import {
  proposalRows, decisionView, trailRows, counterRows, counterMeta, errorLine, render,
} from './panels/command_panel.js';
import {
  COMMAND_PANEL_ID, REGISTERED_PANEL_TYPES, availablePanelChoices, isExecutionProfile,
  defaultLayoutTreeForScenario,
} from './layout/default_layouts.js';
import { listLeaves, createLeaf, findNode } from './layout/split_tree.js';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const inputPath = process.argv[2];
if (!inputPath) { console.error('usage: node command_panel_check.mjs <path-to-json>'); process.exit(2); }
const input = JSON.parse(readFileSync(inputPath, 'utf8'));

const checks = [];
function check(name, pass, detail) { checks.push({ name, pass: !!pass, detail: detail ?? null }); }

// ============================================================================== 1. proposalRows
{
  const rows = proposalRows(input.proposals);
  const exp = input.expectedProposal;
  const row = rows.find((r) => r.commandId === exp.commandId);
  check('proposalRows: real proposal\'s rationale reaches the panel verbatim', !!row && row.rationale === exp.rationale, { row });
  check('proposalRows: real proposal\'s evidence ids reach the panel verbatim', !!row && JSON.stringify(row.evidenceIds) === JSON.stringify(exp.evidenceIds));
  check('proposalRows: real proposal\'s entityId/commandClass reach the panel verbatim', !!row && row.entityId === exp.entityId && row.commandClass === exp.commandClass);
  check('proposalRows: empty proposal list (real server payload for an entity with none) yields [], not a throw',
    Array.isArray(proposalRows(input.emptyProposals)) && proposalRows(input.emptyProposals).length === 0);
  check('proposalRows: null/malformed payload yields [], never a throw',
    proposalRows(null).length === 0 && proposalRows(undefined).length === 0 && proposalRows({}).length === 0);
}

// ============================================================================== 2. decisionView
{
  const view = decisionView(input.decision);
  check('decisionView: real decision id matches the raw gRPC Check() response (independently captured by the pytest driver)',
    !!view && view.decisionId === input.expectedDecisionId);
  check('decisionView: real policy hash is 64 hex chars (SHA-256) and allow is true (mode is unconditionally allowed)',
    !!view && view.policyHash.length === 64 && view.allow === true);
  check('decisionView: reasons is a real array (never undefined/null), matchedRulePath carried through',
    !!view && Array.isArray(view.reasons));
  check('decisionView: null payload (the route\'s own 404 case, handled by the caller as decisionError -- see section 6) yields null',
    decisionView(null) === null);
}

// ================================================================================= 3. trailRows
{
  const rows = trailRows(input.trail);
  check('trailRows: real transition sequence is exactly PROPOSED -> CHECKED -> AUTHORIZED',
    JSON.stringify(rows.map((r) => r.state)) === JSON.stringify(['COMMAND_STATE_PROPOSED', 'COMMAND_STATE_CHECKED', 'COMMAND_STATE_AUTHORIZED']),
    { states: rows.map((r) => r.state) });
  check('trailRows: the final (authorize) transition carries the real operator principal and ackLevel UNSPECIFIED',
    rows.length > 0 && rows[rows.length - 1].principal === input.expectedAuthorizedPrincipal && rows[rows.length - 1].ackLevel === 'ACK_LEVEL_UNSPECIFIED');
  check('trailRows: null/malformed payload yields [], never a throw', trailRows(null).length === 0 && trailRows({}).length === 0);

  // The manager's R3.5b review: a TAI nanosecond epoch is about 1.77e18, two orders of
  // magnitude past Number.MAX_SAFE_INTEGER, so a payload that sent it as a JSON *number*
  // would arrive already rounded, with nothing anywhere saying so. The server sends it as a
  // decimal string (altavista/command_client.py::_int64) and this function keeps it as one.
  // Asserted on the REAL payload, digit for digit, against the value the server actually
  // wrote -- and with the round-trip through Number() shown to be lossy, so this check
  // cannot pass against an implementation that quietly parses it.
  check('trailRows: every real taiNs survives as an exact decimal string, never a rounded JS number',
    rows.length > 0 && rows.every((r) => typeof r.taiNs === 'string' && /^-?[0-9]+$/.test(r.taiNs)),
    { taiNs: rows.map((r) => r.taiNs) });
  // Lossiness is demonstrated by comparing the double's EXACT value (`BigInt(Number(x))`)
  // with the original, not by comparing `String(Number(x))` with it: at this magnitude the
  // double's shortest round-trip decimal usually still prints the original digits (the
  // trailing zeros of a microsecond-granularity epoch hide the 256 ns spacing), so a
  // String comparison would report "no loss" while the value had in fact moved. That near
  // miss is itself the point -- the rounding is invisible in exactly the way that makes it
  // dangerous.
  check('trailRows: those epochs are genuinely past Number.MAX_SAFE_INTEGER and a JS number really does move them, so the string is load-bearing and not decoration',
    rows.length > 0 && rows.every((r) => BigInt(r.taiNs) > BigInt(Number.MAX_SAFE_INTEGER))
      && rows.some((r) => BigInt(Number(r.taiNs)) !== BigInt(r.taiNs)),
    { maxSafe: String(Number.MAX_SAFE_INTEGER), first: rows[0] && rows[0].taiNs, exactValueOfTheDouble: rows[0] && BigInt(Number(rows[0].taiNs)).toString() });

  // The order IS the data -- never re-sorted. A synthetic, deliberately-reversed input
  // (real transition objects, just handed in reverse) must come back in that SAME
  // (reversed) order, proving this function does not alphabetize/re-sort `state` the
  // way every OTHER "never trust the wire order" function in this codebase's panels
  // does for ITS OWN field.
  const reversedInput = { transitions: [...(input.trail.transitions || [])].reverse() };
  const reversedRows = trailRows(reversedInput);
  check('trailRows: NEVER re-sorts -- a deliberately reversed input comes back reversed, unlike every other "re-sort defensively" function in this codebase',
    JSON.stringify(reversedRows.map((r) => r.state)) === JSON.stringify([...rows.map((r) => r.state)].reverse()));
}

// ========================================================================= 4. counterRows/Meta
{
  const rows = counterRows(input.counters);
  const expectedSortedNames = Object.keys(input.counters.refusals).sort();
  check('counterRows: real refusals map -> sorted array, exact match against Object.keys(...).sort() computed independently here',
    JSON.stringify(rows.map((r) => r.name)) === JSON.stringify(expectedSortedNames), { rows, expectedSortedNames });
  check('counterRows: at least one real authz_role_not_granted refusal was actually counted (the wrong-role authorize attempt the pytest driver made)',
    rows.some((r) => r.name === 'authz_role_not_granted' && r.count >= 1));
  check('counterRows: null/malformed payload yields [], never a throw', counterRows(null).length === 0 && counterRows({}).length === 0);

  const meta = counterMeta(input.counters);
  check('counterMeta: real run_id carried through as runId', !!meta && meta.runId === input.counters.run_id);
  check('counterMeta: null payload yields null', counterMeta(null) === null);
}

// =============================================================================== 5. errorLine
{
  check('errorLine: {status, message} formats as "(status) message"', errorLine({ status: 503, message: 'x' }) === '(503) x');
  check('errorLine: a message with no status still renders (never dropped for missing status)', errorLine({ message: 'y' }) === 'y');
  check('errorLine: null/absent/no-message yields null (nothing to say)', errorLine(null) === null && errorLine({}) === null);
}

// ======================================================================= 6/7/8. render() + DOM
// A minimal, hand-rolled fake DOM -- no jsdom, no new dependency (this repo's own stated
// "no dependency" posture, exactly like web/js/layout/layout_tree_check.mjs's own
// LayoutManager section, which takes the identical approach for the identical reason:
// proving REAL rendered behaviour, not just that a pure function returned something
// truthy, needs a real `document.createElement` call site to exist, and this task's own
// brief requires proving DOM TEXT (the token must never appear in it), which a
// data-only check cannot do.
function makeEl(tag) {
  const node = {
    tagName: String(tag).toUpperCase(),
    _text: '',
    _children: [],
    _listeners: {},
    _attrs: {},
    className: '',
    type: '',
    placeholder: '',
    value: '',
    title: '',
    disabled: false,
    classList: {
      _set: new Set(),
      add(c) { this._set.add(c); },
      remove(c) { this._set.delete(c); },
      contains(c) { return this._set.has(c); },
    },
    get textContent() { return this._children.length ? this._children.map((c) => c.textContent).join('') : this._text; },
    set textContent(v) { this._text = String(v); this._children = []; },
    set innerHTML(_v) { this._children = []; this._text = ''; },
    appendChild(child) { this._children.push(child); return child; },
    append(...items) { for (const it of items) this.appendChild(it); },
    addEventListener(evt, fn) { (this._listeners[evt] = this._listeners[evt] || []).push(fn); },
    removeEventListener() {},
    setAttribute(k, v) { this._attrs[k] = v; },
    getAttribute(k) { return this._attrs[k]; },
    click() { (this._listeners.click || []).forEach((fn) => fn()); },
    querySelector() { return null; },
  };
  return node;
}
function withFakeDocument(fn) {
  const previous = globalThis.document;
  globalThis.document = { createElement: (tag) => makeEl(tag) };
  try { return fn(); } finally {
    if (previous === undefined) delete globalThis.document; else globalThis.document = previous;
  }
}
function findAll(node, predicate, out = []) {
  if (predicate(node)) out.push(node);
  for (const c of node._children || []) findAll(c, predicate, out);
  return out;
}
function hasClass(node, cls) { return typeof node.className === 'string' && node.className.split(/\s+/).includes(cls); }

withFakeDocument(() => {
  const exp = input.expectedProposal;

  // ---- 6/7a. no selection yet: honest "select a command" notices, disabled authorize
  {
    const container = makeEl('div');
    render(container, { proposals: input.proposals, selectedCommandId: null });
    const text = container.textContent;
    check('render: with no command selected, decision/trail sections say so honestly (never blank)',
      text.includes('Select a command above'));
    const authBtn = findAll(container, (n) => hasClass(n, 'av-command-authorize-btn'))[0];
    check('render: authorize control is DISABLED (present, never hidden) when no command is selected', !!authBtn && authBtn.disabled === true);
  }

  // ---- 6/7a (cont'd). an empty proposal list is an honest notice, never a blank panel
  {
    const container = makeEl('div');
    render(container, { proposals: input.emptyProposals, selectedCommandId: null });
    check('render: an empty proposal list shows an honest "no proposed commands" notice, never a blank section',
      container.textContent.includes('No proposed commands'));
  }

  // ---- 6/7b. degraded cases: REAL captured server refusal text, never a generic message
  {
    const container = makeEl('div');
    render(container, { proposals: null, proposalsError: input.notConfiguredError });
    check('render: "no command service configured" shows the REAL server message text, not a generic one',
      container.textContent.includes(input.notConfiguredError.message) && input.notConfiguredError.message.length > 0);
  }
  {
    const container = makeEl('div');
    render(container, { proposals: input.proposals, selectedCommandId: input.notYetCheckedCommandId, decision: null, decisionError: input.decisionNotYetCheckedError });
    check('render: "not yet Checked" 404 shows the REAL server message text (names the command id) in the decision section',
      container.textContent.includes(input.decisionNotYetCheckedError.message));
  }

  // ---- 7c. a fully-populated render shows the real rationale/decision/trail/counters.
  // Deliberately TWO different real command ids here, matching the real lifecycle: a
  // command visible in the PROPOSED-only `/api/command/proposals` list (`exp`, never
  // Checked in this fixture) is never simultaneously the one this test has a real
  // decision/trail for (`input.selectedCommandId`, Checked-then-Authorized) -- the real
  // server would drop a Checked command out of the PROPOSED-filtered proposals list
  // (`altavista.command_client.list_proposed_commands`'s own `state_filter=PROPOSED`).
  // This section proves render() binds each REAL data source correctly; it does not (and
  // does not need to) claim these two ids are the same command's simultaneous state.
  {
    const container = makeEl('div');
    render(container, {
      proposals: input.proposals, selectedCommandId: input.selectedCommandId,
      decision: input.decision, trail: input.trail, counters: input.counters,
    });
    const text = container.textContent;
    check('render: the real rationale text appears in the rendered proposals table', text.includes(exp.rationale));
    check('render: every real evidence id appears in the rendered proposals table', exp.evidenceIds.every((id) => text.includes(id)));
    check('render: the real decision id and policy hash appear in the rendered decision section', text.includes(input.expectedDecisionId) && text.includes(input.decision.policyHash));
    const trailRowsBuilt = findAll(container, (n) => n.tagName === 'TABLE' && hasClass(n, 'av-command-trail'));
    const trailTrs = trailRowsBuilt.length ? trailRowsBuilt[0]._children : [];
    check('render: the trail table has exactly one row per real transition, in the real wire order',
      trailTrs.length === input.trail.transitions.length &&
      trailTrs.every((tr, i) => tr.textContent.includes(input.trail.transitions[i].state)));
    check('render: the real refusal counter (authz_role_not_granted) appears in the counters section', text.includes('authz_role_not_granted'));
    const authBtn = findAll(container, (n) => hasClass(n, 'av-command-authorize-btn'))[0];
    check('render: authorize control is ENABLED once a command is selected', !!authBtn && authBtn.disabled === false);
  }

  // ---- 8. the token rule: sent once, cleared synchronously, never resurfaces
  {
    const SECRET_TOKEN = 'super-secret-operator-token-do-not-log-me';
    const container = makeEl('div');
    let capturedToken = null;
    let capturedCommandId = null;
    const onAuthorize = (commandId, token) => {
      capturedCommandId = commandId; capturedToken = token;
      return new Promise(() => {}); // deliberately never settles here -- see the synchronous assertions below
    };
    render(container, { proposals: input.proposals, selectedCommandId: input.selectedCommandId, decision: input.decision, trail: input.trail, onAuthorize });
    const tokenInput = findAll(container, (n) => hasClass(n, 'av-command-token-input'))[0];
    const authBtn = findAll(container, (n) => hasClass(n, 'av-command-authorize-btn'))[0];
    tokenInput.value = SECRET_TOKEN;
    authBtn.click();
    check('token rule: onAuthorize receives the exact commandId and the exact token the operator typed (sent once)',
      capturedCommandId === input.selectedCommandId && capturedToken === SECRET_TOKEN);
    check('token rule: the input\'s own value is cleared SYNCHRONOUSLY in the click handler, before onAuthorize\'s promise ever settles',
      tokenInput.value === '');
    check('token rule: the token does not appear anywhere in the rendered DOM text immediately after submission',
      !container.textContent.includes(SECRET_TOKEN));

    // The caller's NEXT render (after the real promise would have settled) carries only
    // the RESULT, never the token -- proven against the REAL captured success/refusal
    // text from the pytest driver's own live server calls.
    const successContainer = makeEl('div');
    render(successContainer, {
      proposals: input.proposals, selectedCommandId: input.selectedCommandId, decision: input.decision, trail: input.trail,
      authorizeResult: { ok: true, state: input.authorizeSuccessState },
    });
    check('token rule: a successful authorize\'s re-render shows the REAL new state and never the token',
      successContainer.textContent.includes(input.authorizeSuccessState) && !successContainer.textContent.includes(SECRET_TOKEN));

    const refusalContainer = makeEl('div');
    render(refusalContainer, {
      proposals: input.proposals, selectedCommandId: input.selectedCommandId, decision: input.decision, trail: input.trail,
      authorizeResult: { ok: false, status: input.authorizeRefusalError.status, message: input.authorizeRefusalError.message },
    });
    check('render: an authorize refusal shows the REAL server refusal reason (never a generic "failed" message), and never the token',
      refusalContainer.textContent.includes(input.authorizeRefusalError.message) && !refusalContainer.textContent.includes(SECRET_TOKEN));
  }

  // ---- 8 (cont'd). this module's own on-disk source never references browser storage/
  // cookies, and offers no dispatch/ack/expire/fail control (question 53: propose-only).
  {
    const src = readFileSync(path.join(__dirname, 'panels', 'command_panel.js'), 'utf8');
    const forbidden = ['localStorage', 'sessionStorage', 'document.cookie'];
    const hits = forbidden.filter((f) => src.includes(f));
    check('command_panel.js source: zero references to browser storage or cookies (grep of the real on-disk file)', hits.length === 0, { hits });

    const buttonCreations = (src.match(/createElement\('button'\)/g) || []).length;
    check('command_panel.js source: exactly 2 buttons are ever created (select + authorize) -- no dispatch/ack/expire/fail control exists', buttonCreations === 2, { buttonCreations });

    // Never a network call of its own -- this module's own top comment's explicit
    // design rule (every route call lives in web/js/app.js instead, see
    // `openFeasibilitySample`/`commandFetch` there). A literal grep for route path
    // SUBSTRINGS like '/dispatch' would false-positive against this very file's own
    // prose (its doc comments say "no dispatch/ack/expire/fail control", which
    // contains those substrings) -- checking for an absent `fetch(` call is the real,
    // structural way to confirm this module drives no request to ANY route, dispatch/
    // ack/expire/fail included.
    check('command_panel.js source: contains no `fetch(` call anywhere -- every network request lives in web/js/app.js, never here',
      !src.includes('fetch('));
  }
});

// ==================================================================================== 9. layout
{
  check('layout: COMMAND_PANEL_ID is a registered panel type (chooser-reachable in every profile)',
    REGISTERED_PANEL_TYPES.some((t) => t.panelId === COMMAND_PANEL_ID));
  const solo = createLeaf('empty-solo', { id: 'solo' });
  check('layout: availablePanelChoices offers the command console to an empty pane regardless of profile (chooser availability is not profile-gated)',
    availablePanelChoices(solo, 'solo').some((c) => c.panelId === COMMAND_PANEL_ID));

  check('isExecutionProfile: true only for profileId === "execution"', isExecutionProfile({ profileId: 'execution' }) === true);
  check('isExecutionProfile: false for a different real profile id (design)', isExecutionProfile({ profileId: 'design' }) === false);
  check('isExecutionProfile: false for a scenario with NO profileId key at all (every scenario published before this task, degrade never guess)',
    isExecutionProfile({ imagery: null }) === false);
  check('isExecutionProfile: false for null', isExecutionProfile(null) === false);

  function leafIds(tree) { return listLeaves(tree).map((l) => l.panelId); }
  function countCommandLeaves(tree) { return leafIds(tree).filter((id) => id === COMMAND_PANEL_ID).length; }

  // ---- non-execution: BYTE-IDENTICAL to before this task (the brief's own explicit
  // "the default layout for every other profile must be byte-identical to today's").
  const ordinaryNoProfile = defaultLayoutTreeForScenario({ imagery: null });
  check('layout: ordinary scenario, NO profileId -- still exactly 5 leaves, no command panel (unregressed)',
    leafIds(ordinaryNoProfile).length === 5 && countCommandLeaves(ordinaryNoProfile) === 0);
  const ordinaryDesign = defaultLayoutTreeForScenario({ imagery: null, profileId: 'design' });
  check('layout: ordinary scenario, profileId "design" -- still exactly 5 leaves, no command panel',
    leafIds(ordinaryDesign).length === 5 && countCommandLeaves(ordinaryDesign) === 0);
  const rpoNoProfile = defaultLayoutTreeForScenario({ frames: [{ axes: 'AXES_KIND_RIC' }] });
  check('layout: RPO scenario, no profileId -- still exactly 7 leaves, no command panel',
    leafIds(rpoNoProfile).length === 7 && countCommandLeaves(rpoNoProfile) === 0);
  const sweepNoProfile = defaultLayoutTreeForScenario({ sweep: {} });
  check('layout: sweep scenario, no profileId -- still exactly 4 leaves, no command panel',
    leafIds(sweepNoProfile).length === 4 && countCommandLeaves(sweepNoProfile) === 0);

  // ---- execution profile: the command panel joins the default layout EXACTLY ONCE, in
  // every underlying shape, at the documented share.
  const ordinaryExec = defaultLayoutTreeForScenario({ imagery: null, profileId: 'execution' });
  check('layout: ordinary scenario, profileId "execution" -- exactly 6 leaves (5 + the command panel), added exactly once',
    leafIds(ordinaryExec).length === 6 && countCommandLeaves(ordinaryExec) === 1, { leaves: leafIds(ordinaryExec) });
  const rpoExec = defaultLayoutTreeForScenario({ frames: [{ axes: 'AXES_KIND_RIC' }], profileId: 'execution' });
  check('layout: RPO scenario, profileId "execution" -- exactly 8 leaves (7 + the command panel), added exactly once',
    leafIds(rpoExec).length === 8 && countCommandLeaves(rpoExec) === 1, { leaves: leafIds(rpoExec) });
  const sweepExec = defaultLayoutTreeForScenario({ sweep: {}, profileId: 'execution' });
  check('layout: sweep scenario, profileId "execution" -- exactly 5 leaves (4 + the command panel), added exactly once',
    leafIds(sweepExec).length === 5 && countCommandLeaves(sweepExec) === 1, { leaves: leafIds(sweepExec) });

  // "a sensible share": the command console is the OUTERMOST, narrower child (documented
  // 0.78/0.22 split, default_layouts.js's own attachCommandPanel doc comment) -- checked
  // structurally here rather than merely trusting that comment.
  const outerSplit = ordinaryExec;
  check('layout: the command panel sits behind an outer split at the documented 0.78 share, as the SECOND (narrower) child',
    outerSplit.type === 'split' && Math.abs(outerSplit.ratio - 0.78) < 1e-9 && outerSplit.children[1].panelId === COMMAND_PANEL_ID);
  check('layout: every leaf the underlying (non-execution) layout already had is still present, untouched, alongside the command panel',
    ['sidebar', 'viewport', 'run-products', 'map-2d', 'console-log'].every((id) => leafIds(ordinaryExec).includes(id)));
}

const allPass = checks.every((c) => c.pass);
process.stdout.write(JSON.stringify({ allPass, checks }));
process.exit(allPass ? 0 : 1);
