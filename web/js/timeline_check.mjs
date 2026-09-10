// CLI harness for tests/test_viewer_timeline.py: `node web/js/timeline_check.mjs
// <path-to-json>`. Same pattern as web/js/panels_check.mjs/web/js/viewport_check.mjs:
// drives the REAL, shipped web/js/timeline_events.js against a real, protocol-honest
// published scenario (see this file's own input's `scenario` -- a real, live-server-
// published RunProducts, POSTed through the real POST /api/cdm/run route, whose events
// mirror crates/av-kernel/src/drm/events.rs::contact_event /
// src/drm/command.rs::transition_event's own `format!` calls verbatim -- see
// tests/test_viewer_timeline.py's own module docstring) and prints one JSON object of
// named checks.
//
// M25.3d/M25.3e (docs/sil-plan.md's M25 milestone: "telemetry into the viewer"). See
// docs/open-questions.md questions 174/177 and drms/M25_3E_REPORT.md for the full
// account (there is no earlier REPORT_M25_3d.md -- M25.3d's own worker was cut off
// before writing one; this task's report is the record of both).
import { pairContactWindows, pairDecodeErrorWindows, groupCommandTransitions, timelineTickPlan, parseContactCounterpart, parseCommandId } from './timeline_events.js';
import { readFileSync } from 'fs';

const inputPath = process.argv[2];
if (!inputPath) { console.error('usage: node timeline_check.mjs <path-to-json>'); process.exit(2); }
const input = JSON.parse(readFileSync(inputPath, 'utf8'));
const { scenario } = input;

const checks = [];
function check(name, pass) { checks.push({ name, pass: !!pass }); }
function approxEqual(a, b, tol) { return Math.abs(a - b) <= tol; }

// ============================================================ 1. real published events
// Confirms (by inspection of the real published scenario, not by assumption -- this
// task's own brief) that contact_start/contact_end/command_transition all reach the
// viewer's own `sc.events`, each under the honest fallback label
// `altavista/cdm.py`'s `cdm_event_to_viewer_event` mints for a kind it has no special
// case for (EVENT_KIND_CONTACT_START -> "contact_start", etc).
{
  const types = new Set((scenario.events || []).map((e) => e.type));
  check('real published scenario: contact_start reaches the viewer', types.has('contact_start'));
  check('real published scenario: contact_end reaches the viewer', types.has('contact_end'));
  check('real published scenario: command_transition reaches the viewer', types.has('command_transition'));
  check('real published scenario: an unrelated kind (fault) is still present, unaffected', types.has('fault'));

  // Question 193 (R6.2): decode_error_start/_end share EVENT_KIND_FAULT with every other
  // fault-shaped event (events.rs's own "Naming" doc section), so `type` is "fault" for these
  // too -- checked by `name`, not `type`, here and everywhere else in this file.
  const names = new Set((scenario.events || []).map((e) => e.name));
  check('real published scenario: decode_error_start reaches the viewer (type=fault, name=decode_error_start)',
    names.has('decode_error_start'));
  check('real published scenario: decode_error_end reaches the viewer (type=fault, name=decode_error_end)',
    names.has('decode_error_end'));
}

// ==================================================================== 2. pairContactWindows
// Fixture (tests/test_viewer_timeline.py): THREE contact events forming one matched
// window (ground_alpha <-> demo_flt) plus two deliberately unmatched ones -- an
// unclosed start at a DIFFERENT station (ground_beta <-> demo_flt, concurrent with the
// matched window) and an end with no start at the SAME station as the matched window
// but a DIFFERENT counterpart (ground_alpha <-> demo_other). This exact shape is the
// load-bearing trap for "never silently paired with the wrong partner": an
// implementation that pairs by station alone (ignoring the counterpart) would wrongly
// close the ground_alpha/demo_flt start with the ground_alpha/demo_other end instead of
// its real, later end -- checks 2f/2g below fail against exactly that bug.
{
  const { windows, unmatched } = pairContactWindows(scenario.events);
  check('pairContactWindows: exactly one matched window', windows.length === 1);
  check('pairContactWindows: exactly two unmatched contact events', unmatched.length === 2);

  const w = windows[0];
  if (w) {
    check('pairContactWindows: matched window is ground_alpha (station, spacecraft field)', w.spacecraft === 'ground_alpha');
    check('pairContactWindows: matched window counterpart is demo_flt (parsed from detail, not swapped with station)', w.counterpart === 'demo_flt');
    check('pairContactWindows: matched window startT matches the real AOS epoch (hand-computed in Python)',
      approxEqual(w.startT, input.expectedWindowStartT, 1e-9));
    check('pairContactWindows: matched window endT matches the real LOS epoch (hand-computed in Python)',
      approxEqual(w.endT, input.expectedWindowEndT, 1e-9));
    check('pairContactWindows: durationT is endT - startT exactly',
      approxEqual(w.durationT, w.endT - w.startT, 1e-12));
    // The trap: the window's own END event must be the real "target demo_flt" one, NOT
    // the concurrent ground_alpha/demo_other end (detail would say target demo_other).
    check('pairContactWindows: TRAP -- window end is the demo_flt end, not cross-paired with the demo_other end at the same station',
      !!w.end.detail && w.end.detail.includes('"demo_flt"') && !w.end.detail.includes('"demo_other"'));
  } else {
    check('pairContactWindows: matched window is ground_alpha (station, spacecraft field)', false);
    check('pairContactWindows: matched window counterpart is demo_flt (parsed from detail, not swapped with station)', false);
    check('pairContactWindows: matched window startT matches the real AOS epoch (hand-computed in Python)', false);
    check('pairContactWindows: matched window endT matches the real LOS epoch (hand-computed in Python)', false);
    check('pairContactWindows: durationT is endT - startT exactly', false);
    check('pairContactWindows: TRAP -- window end is the demo_flt end, not cross-paired with the demo_other end at the same station', false);
  }

  const unmatchedStart = unmatched.find((u) => u.event.spacecraft === 'ground_beta');
  check('pairContactWindows: the never-closed ground_beta start is reported unmatched, not dropped',
    !!unmatchedStart && unmatchedStart.event.type === 'contact_start' && /contact_end/.test(unmatchedStart.reason));

  const unmatchedEnd = unmatched.find((u) => u.event.type === 'contact_end');
  check('pairContactWindows: the orphan ground_alpha/demo_other end is reported unmatched, not dropped, and not paired',
    !!unmatchedEnd && unmatchedEnd.event.spacecraft === 'ground_alpha' && /contact_start/.test(unmatchedEnd.reason));
}

// ================================================================ 2b. pairDecodeErrorWindows
// Fixture (tests/test_viewer_timeline.py): THREE decode-error events forming one matched
// window (controller/startracker_in) plus two deliberately unmatched ones -- an unclosed start
// on a DIFFERENT PORT of the SAME instance (controller/imu_in) and an end with no start on a
// THIRD port of the SAME instance (controller/cmd_in), landing chronologically BETWEEN the real
// window's own start and end. This is the load-bearing trap for "never silently paired with the
// wrong partner": an implementation that pairs by spacecraft (entity_id) alone, ignoring the
// port (referenceId), would wrongly close the real controller/startracker_in start with the
// controller/cmd_in end instead of its own real, later end -- checks 2b-f/2b-g below fail
// against exactly that bug (mirrors section 2's own identical contact-pairing trap).
{
  const { windows, unmatched } = pairDecodeErrorWindows(scenario.events);
  check('pairDecodeErrorWindows: exactly one matched window', windows.length === 1);
  check('pairDecodeErrorWindows: exactly two unmatched decode-error events', unmatched.length === 2);

  const w = windows[0];
  if (w) {
    check('pairDecodeErrorWindows: matched window kind is decode_error', w.kind === 'decode_error');
    check('pairDecodeErrorWindows: matched window spacecraft is controller (entity_id, a real field)', w.spacecraft === 'controller');
    check('pairDecodeErrorWindows: matched window port is startracker_in (referenceId, a real field -- no detail parsing needed)', w.port === 'startracker_in');
    check('pairDecodeErrorWindows: matched window startT matches the real episode-open epoch (hand-computed in Python)',
      approxEqual(w.startT, input.expectedDecodeErrorWindowStartT, 1e-9));
    check('pairDecodeErrorWindows: matched window endT matches the real episode-close epoch (hand-computed in Python)',
      approxEqual(w.endT, input.expectedDecodeErrorWindowEndT, 1e-9));
    check('pairDecodeErrorWindows: durationT is endT - startT exactly',
      approxEqual(w.durationT, w.endT - w.startT, 1e-12));
    // The trap: the window's own END event must be the real startracker_in one, NOT the
    // concurrent controller/cmd_in orphan end (referenceId would say cmd_in).
    check('pairDecodeErrorWindows: TRAP -- window end is the startracker_in end, not cross-paired with the cmd_in end on the same instance',
      w.end.referenceId === 'startracker_in');
  } else {
    check('pairDecodeErrorWindows: matched window kind is decode_error', false);
    check('pairDecodeErrorWindows: matched window spacecraft is controller (entity_id, a real field)', false);
    check('pairDecodeErrorWindows: matched window port is startracker_in (referenceId, a real field -- no detail parsing needed)', false);
    check('pairDecodeErrorWindows: matched window startT matches the real episode-open epoch (hand-computed in Python)', false);
    check('pairDecodeErrorWindows: matched window endT matches the real episode-close epoch (hand-computed in Python)', false);
    check('pairDecodeErrorWindows: durationT is endT - startT exactly', false);
    check('pairDecodeErrorWindows: TRAP -- window end is the startracker_in end, not cross-paired with the cmd_in end on the same instance', false);
  }

  const unmatchedStart = unmatched.find((u) => u.event.referenceId === 'imu_in');
  check('pairDecodeErrorWindows: the never-closed controller/imu_in start is reported unmatched, not dropped',
    !!unmatchedStart && unmatchedStart.event.name === 'decode_error_start' && /no matching end/.test(unmatchedStart.reason));

  const unmatchedEnd2 = unmatched.find((u) => u.event.name === 'decode_error_end');
  check('pairDecodeErrorWindows: the orphan controller/cmd_in end is reported unmatched, not dropped, and not paired',
    !!unmatchedEnd2 && unmatchedEnd2.event.referenceId === 'cmd_in' && /no matching start/.test(unmatchedEnd2.reason));
}

// ============================================================== 3. groupCommandTransitions
// Fixture: three commands. cmd1 (instance demo_flt) walks the full real state machine
// PROPOSED -> CHECKED -> AUTHORIZED -> DISPATCHED -> ACKED. cmd2 uses the SAME
// instance (demo_flt) as cmd1 but is a DIFFERENT command (only ever reaches PROPOSED)
// -- the load-bearing trap for "grouped by command, not by spacecraft": an
// implementation that groups by `spacecraft`/entity_id instead of the command id
// parsed from `detail` would wrongly merge cmd2's one transition into cmd1's five.
// cmd3 (a different instance) is REJECTED instead of ACKED, proving a different real
// terminal CommandState name is never paraphrased.
{
  const { commands, unparsed } = groupCommandTransitions(scenario.events);
  check('groupCommandTransitions: exactly three distinct commands', commands.length === 3);
  check('groupCommandTransitions: no command_transition event failed to parse a command id', unparsed.length === 0);

  const cmd1 = commands.find((c) => c.commandId === 'cmd1');
  check('groupCommandTransitions: cmd1 found', !!cmd1);
  check('groupCommandTransitions: cmd1 has exactly 5 transitions (never merged with cmd2 despite sharing instance demo_flt)',
    !!cmd1 && cmd1.transitions.length === 5);
  check('groupCommandTransitions: cmd1 transitions are the real CommandState names, in order',
    !!cmd1 && JSON.stringify(cmd1.transitions.map((t) => t.state)) === JSON.stringify([
      'COMMAND_STATE_PROPOSED', 'COMMAND_STATE_CHECKED', 'COMMAND_STATE_AUTHORIZED',
      'COMMAND_STATE_DISPATCHED', 'COMMAND_STATE_ACKED',
    ]));

  const cmd2 = commands.find((c) => c.commandId === 'cmd2');
  check('groupCommandTransitions: TRAP -- cmd2 (same instance as cmd1) is its own group with exactly 1 transition, not merged into cmd1',
    !!cmd2 && cmd2.transitions.length === 1 && cmd2.transitions[0].state === 'COMMAND_STATE_PROPOSED');

  const cmd3 = commands.find((c) => c.commandId === 'cmd3');
  check('groupCommandTransitions: cmd3\'s last transition is the real COMMAND_STATE_REJECTED (a different terminal state, not paraphrased)',
    !!cmd3 && cmd3.transitions[cmd3.transitions.length - 1].state === 'COMMAND_STATE_REJECTED');

  // M25.3e (question 177): ackLevel is now the REAL AckLevel for cmd1 (the only one of
  // the three that reaches ACKED, with a real ack_level attribute on that transition --
  // tests/test_viewer_timeline.py's own fixture) -- never null, never fabricated for
  // cmd2/cmd3 (neither ever reaches ACKED, so both stay null, exactly as before).
  check('groupCommandTransitions: cmd1 (reaches ACKED) has the real AckLevel from attributes, not null',
    !!cmd1 && cmd1.ackLevel === 'ACK_LEVEL_ASSET_EXECUTED');
  check('groupCommandTransitions: cmd2 (never reaches ACKED) still has ackLevel null -- not fabricated',
    !!cmd2 && cmd2.ackLevel === null);
  check('groupCommandTransitions: cmd3 (REJECTED, never ACKED) still has ackLevel null -- not fabricated',
    !!cmd3 && cmd3.ackLevel === null);
}

// ============================================== 3b. referenceId/attributes on the wire (question 177)
// Confirms, on the REAL published scenario (not by assumption), that `referenceId` and
// `attributes` now reach every command_transition event -- `cmd.id` on `referenceId`,
// and (only on the ACKED transition) `ack_level` inside `attributes`.
{
  const transitions = (scenario.events || []).filter((e) => e && e.type === 'command_transition');
  check('referenceId/attributes: every command_transition event carries a non-empty referenceId',
    transitions.length > 0 && transitions.every((e) => typeof e.referenceId === 'string' && e.referenceId.length > 0));
  check('referenceId/attributes: every command_transition event carries an attributes object (possibly empty)',
    transitions.every((e) => e.attributes && typeof e.attributes === 'object'));
  const acked = transitions.find((e) => e.name === 'COMMAND_STATE_ACKED');
  check('referenceId/attributes: the ACKED transition\'s attributes carries the real ack_level',
    !!acked && acked.attributes.ack_level === 'ACK_LEVEL_ASSET_EXECUTED');
  const proposed = transitions.find((e) => e.name === 'COMMAND_STATE_PROPOSED' && e.referenceId === 'cmd1');
  check('referenceId/attributes: a non-ACKED transition\'s attributes carries no ack_level (never fabricated)',
    !!proposed && !('ack_level' in proposed.attributes));
}

// ========================================== 3c. referenceId fallback and priority (pure, synthetic)
// Judgement call recorded here (see tests/test_viewer_timeline.py / drms/M25_3E_REPORT.md
// for the full account): parseCommandId stays as a FALLBACK for an event with no
// referenceId at all (never removed as dead code -- this is what still exercises it via
// groupCommandTransitions, on top of its own direct tests in section 5 below), but
// groupCommandTransitions prefers a real referenceId over detail-parsing whenever one is
// present, even if detail would not itself parse. Both fixtures below are synthetic, tiny,
// pure JS objects -- no server round trip needed to prove pure grouping logic.
{
  const noReferenceId = [
    { name: 'COMMAND_STATE_PROPOSED', t: 0, type: 'command_transition', spacecraft: 'sc1',
      detail: 'command "legacy1" (generic): COMMAND_STATE_PROPOSED -> x' },
  ];
  const { commands: legacyCommands, unparsed: legacyUnparsed } = groupCommandTransitions(noReferenceId);
  check('groupCommandTransitions: FALLBACK -- an event with no referenceId still groups via parseCommandId(detail)',
    legacyUnparsed.length === 0 && legacyCommands.length === 1 && legacyCommands[0].commandId === 'legacy1');

  const referenceIdWinsOverGarbledDetail = [
    { name: 'COMMAND_STATE_PROPOSED', t: 0, type: 'command_transition', spacecraft: 'sc1',
      referenceId: 'cmdZ', detail: 'this text does not match the command "..." (...) shape at all', attributes: {} },
    { name: 'COMMAND_STATE_ACKED', t: 1, type: 'command_transition', spacecraft: 'sc1',
      referenceId: 'cmdZ', detail: 'also garbled', attributes: { ack_level: 'ACK_LEVEL_ASSET_RECEIVED' } },
  ];
  const { commands: zCommands, unparsed: zUnparsed } = groupCommandTransitions(referenceIdWinsOverGarbledDetail);
  check('groupCommandTransitions: PRIORITY -- a present referenceId groups correctly even when detail would fail to parse',
    zUnparsed.length === 0 && zCommands.length === 1 && zCommands[0].commandId === 'cmdZ' && zCommands[0].transitions.length === 2);
  check('groupCommandTransitions: PRIORITY case also reads the real ackLevel from attributes',
    zCommands.length === 1 && zCommands[0].ackLevel === 'ACK_LEVEL_ASSET_RECEIVED');
}

// ==================================================================== 4. timelineTickPlan
{
  const plan = timelineTickPlan(scenario.events);
  check('timelineTickPlan: no contact_start/contact_end event leaks into points (would double-render a windowed contact)',
    !plan.points.some((p) => p.event.type === 'contact_start' || p.event.type === 'contact_end'));
  check('timelineTickPlan: no decode_error_start/_end event leaks into points (question 193, R6.2 -- would double-render a windowed decode-error episode)',
    !plan.points.some((p) => p.event.name === 'decode_error_start' || p.event.name === 'decode_error_end'));

  const ctPoints = plan.points.filter((p) => p.event.type === 'command_transition');
  check('timelineTickPlan: every command_transition point gets the distinct tick-command-transition class',
    ctPoints.length > 0 && ctPoints.every((p) => p.className === 'tick-command-transition'));
  check('timelineTickPlan: a command_transition point\'s label carries the real CommandState name (ev.name), not a paraphrase',
    ctPoints.every((p) => p.label.startsWith(p.event.name)));

  const faultPoints = plan.points.filter((p) => p.event.type === 'fault');
  check('timelineTickPlan: an unrelated kind (fault) is unaffected -- null className, label is exactly ev.name (pre-M25.3d rendering)',
    faultPoints.length === 1 && faultPoints[0].className === null && faultPoints[0].label === faultPoints[0].event.name);

  // Question 193 (R6.2): the decode-error window is present in plan.windows alongside the
  // contact window, distinguished by its own `kind` field, and the unmatched decode-error
  // events are present in plan.unmatched too -- both merged from pairDecodeErrorWindows, not
  // dropped by timelineTickPlan's own generalisation.
  const decodeErrorWindows = plan.windows.filter((w) => w.kind === 'decode_error');
  check('timelineTickPlan: plan.windows carries the decode-error window alongside the contact window',
    decodeErrorWindows.length === 1 && plan.windows.some((w) => w.kind === 'contact'));
  const decodeErrorUnmatched = plan.unmatched.filter((u) => u.event.name === 'decode_error_start' || u.event.name === 'decode_error_end');
  check('timelineTickPlan: plan.unmatched carries both decode-error unmatched events alongside the contact ones',
    decodeErrorUnmatched.length === 2 && plan.unmatched.length === 4);
}

// ============================================================ 5. detail parsing edge cases
{
  check('parseContactCounterpart: null for a detail with no "(target ...)" suffix',
    parseContactCounterpart('some unrelated free text') === null);
  check('parseContactCounterpart: null for missing detail', parseContactCounterpart(undefined) === null);
  // Runtime detail text: command "cmd\"1" (generic): ... -- i.e. a command id that
  // itself contains a literal `"`, Rust `{:?}`-escaped as `\"` (source has `\\"` to
  // produce that one runtime backslash-quote pair). Expected recovered id: cmd"1
  // (the backslash removed, the quote kept) -- proves unescaping, not just stripping.
  check('parseCommandId: recovers an escaped-quote command id (Rust {:?} debug-escaping)',
    parseCommandId('command "cmd\\"1" (generic): COMMAND_STATE_PROPOSED -> x') === 'cmd"1');
  check('parseCommandId: null for a detail that is not a command_transition-shaped string',
    parseCommandId('ground instance "ground_alpha": contact_start (target "demo_flt")') === null);
}

const allPass = checks.every((c) => c.pass);
process.stdout.write(JSON.stringify({ allPass, checks }));
process.exit(allPass ? 0 : 1);
