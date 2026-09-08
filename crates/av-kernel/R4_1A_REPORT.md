# R4.1a: PORT fault runtime (`drop`/`delay`) in the kernel's router

Task: `docs/open-questions.md` question 178; ADR-005 section 5; question 28's scope. Build the
PORT fault machinery in `crate::router::Router` to take all four ADR-005 section 5 PORT kinds
(`drop`, `delay`, `corrupt`, `duplicate`) but implement only `drop` and `delay`. R4.1b builds
`corrupt`/`duplicate` on this same machinery.

This report is written incrementally, as instructed: measurements and hypotheses are stated
before each corresponding run, not retrofitted afterward.

## 1. What was built

- `crates/av-kernel/src/router.rs` -- the PORT fault runtime itself:
  - `Router::install_port_faults` -- resolves and validates every `FAULT_TARGET_KIND_PORT` fault
    (matching, port existence/kind, seed, `delay_s`, `rate`, `clear` refusal).
  - `Router::deliver` -- extended to consult installed PORT faults per candidate frame: drop
    suppresses the IN record and delivery but keeps the OUT record; delay adds to the connection's
    own declared latency.
  - `Router::take_applied_port_faults` -- drains, once, the first-applied epoch of every PORT
    fault genuinely applied at least once.
  - New types: `PortFaultKind`, `InstalledPortFault`, `AppliedPortFault` (public).
  - New `RouterError` variants: `UndeclaredPortFaultTarget`, `PortFaultTargetNotFramed`,
    `UnsupportedPortFaultKind`, `MissingPortFaultDelay`, `InvalidPortFaultRate`,
    `PortFaultClearNotSupported`, `MissingFaultSeed`.
  - Module doc comment: new "Port fault runtime" section stating matching, window, rate/seed,
    what the log records (drop keeps OUT/gates IN; delay adds to declared latency), events (one
    per fault, at first real effect), and the epoch-grid decision.
  - 12 new unit tests in `router.rs`'s own `mod tests`.
- `crates/av-kernel/src/drm/mod.rs` -- `DrmError`: narrowed `PortOrSensorFaultNotYetSupported` to
  SENSOR-only (wording updated so it no longer claims "the port and sensor fault runtimes do not
  exist"); added `PortFaultKindNotYetSupported` (corrupt/duplicate, naming R4.1b) and
  `UnknownPortFaultKind` (outside the documented vocabulary entirely).
- `crates/av-kernel/src/drm/executor.rs`:
  - Load-time fault-validation loop: SENSOR still refused; PORT sorted into
    accept-drop/delay / refuse-corrupt-duplicate / refuse-unknown-kind, and the DYNAMICS/HARDWARE
    sample-grid check no longer applies to PORT faults (rule 9).
  - `router.install_port_faults(&scenario.faults, &scenario.seeds)` called once, right after the
    fault-validation loop, before Pass 1 (binding classification) -- `RouterError::
    MissingFaultSeed` mapped to the crate-wide `DrmError::MissingFaultSeed`; everything else wraps
    through `DrmError::Router`.
  - New block draining `router.take_applied_port_faults()` into `EVENT_KIND_FAULT` events
    (`events::port_fault_event`), alongside the existing `dropped_in_flight_messages` handling.
  - `RunProducts::measurements`'s own doc comment updated (a dropped PORT packet's `Measurement`
    still appears, honestly, since decoding happens before the packet ever reaches the router).
- `crates/av-kernel/src/drm/events.rs`:
  - New `events::port_fault_event` (applied epoch, not the fault's own declared window start).
  - `declared_events` extended to include an in-range PORT fault of `kind == "drop"`/`"delay"`
    (still excludes `corrupt`/`duplicate`, which a real run can never reach).
  - 2 new unit tests; 1 existing test's own fixture/comment revisited per the task's explicit
    instruction.
- `crates/av-kernel/src/drm/fault.rs` -- `PORT_KINDS` made `pub(crate)` (reused by
  `executor.rs`'s own vocabulary check); module doc comment updated to describe the new PORT
  runtime split (drop/delay real, corrupt/duplicate and SENSOR still refused) without touching
  `realize_unapplied_fault`'s own logic (still SENSOR/PORT-generic, still exercised only by its
  own direct tests, never by a real `execute()` run for PORT any more).
- `crates/av-kernel/tests/port_faults.rs` -- new acceptance-test file, 9 tests.
- `drms/demo_command_port_drop.drm.yaml`, `drms/demo_command_port_delay.drm.yaml` -- new DRM
  fixtures, reusing `drms/demo_command.sos.yaml`/`_flight.system.yaml`/`_ground.system.yaml`
  unchanged.
- `crates/av-kernel/tests/demo_measurements.rs` -- one existing test's fixture/name/assertion
  updated (`kind: "drop"` -> `"corrupt"`, since `"drop"` is no longer refused).

## 2. Verification

All commands run with `export PATH="/opt/homebrew/opt/rustup/bin:$PATH"` first, per the
environment rules. Contention (`ps -Ao pid,etime,command | grep -E "cargo test|pytest|docker
build"`) was checked before every targeted run and before the one full run; a Docker build was in
flight during early development (targeted runs only, per the standing instruction), and had
cleared by the time the full run below was launched.

### Targeted (development)

- `cargo test -p av-kernel --lib router::` -- 32 passed, 0 failed (all 12 new PORT-fault unit
  tests plus the 20 pre-existing `router.rs` tests, unaffected).
- `cargo test -p av-kernel --test port_faults --test drm_command --test port_traffic_sidecar
  --test demo_measurements --test faults_seeded --test faults_determinism` -- 31 passed, 0 failed
  across the six directly-affected integration test files (9 + 4 + 4 + 3 + 9 + 2). Full output
  captured in this session's own tool transcript (not re-saved to a separate file, since the
  targeted runs are development-time evidence, superseded by the full run below for the gate).

### Full run (the gate)

- `cargo test -p av-kernel` -- saved to
  `/private/tmp/claude-501/-Users-probe-code-AltaVista/ed238b37-d349-42f1-956b-22807101027f/scratchpad/full_test_run.log`
  (this is this session's own scratchpad directory, not under the repo).
  **Result: 793 passed, 0 failed, 1 ignored** (`grep -oE "[0-9]+ passed; [0-9]+ failed; [0-9]+
  ignored" full_test_run.log | awk '{p+=$1; f+=$3; i+=$5} END {print p, f, i}'`, summed across
  every test binary in the run -- library unit tests, every `tests/*.rs` integration file, and the
  0-test doc-test pass). The one ignored test is still
  `drm_attitude_control_renode.rs::byte_identical_port_traffic_between_posix_container_and_renode`
  ("question 171: Renode port traffic beyond STEP 1 does not deliver; verified posix-container-
  only until resolved") -- unchanged, as required.

  Baseline (stated before this run, per the task's own instructions): 770 passed, 0 failed, 1
  ignored. **Delta: +23 passed, exactly accounted for by name:**
  - `crates/av-kernel/src/router.rs`'s own `mod tests` -- 12 new tests:
    `install_port_faults_refuses_an_undeclared_port_target`,
    `install_port_faults_refuses_a_non_framed_port_target`,
    `install_port_faults_refuses_an_unrecognized_kind`,
    `install_port_faults_refuses_corrupt_and_duplicate_the_same_generic_way_at_the_router_level`,
    `install_port_faults_refuses_a_missing_seed`,
    `install_port_faults_refuses_a_delay_fault_missing_delay_s`,
    `install_port_faults_refuses_a_rate_outside_zero_one`,
    `install_port_faults_refuses_clear_true`,
    `a_drop_fault_suppresses_the_in_record_and_delivery_but_keeps_the_out_record`,
    `a_delay_fault_adds_its_own_delay_to_the_connections_declared_latency`,
    `two_delay_faults_on_the_same_port_sum_their_own_delays`,
    `two_port_faults_on_different_ports_draw_from_independent_seeded_substreams`.
  - `crates/av-kernel/src/drm/events.rs`'s own `mod tests` -- 2 new tests:
    `port_fault_event_uses_the_applied_epoch_not_the_faults_own_declared_window_start`,
    `an_in_range_applicable_port_fault_is_included_but_an_unimplemented_kind_is_not`.
  - `crates/av-kernel/tests/port_faults.rs` -- 9 new tests (the whole file):
    `demo_command_port_drop_suppresses_delivery_keeps_the_out_record_and_emits_one_fault_event`,
    `demo_command_port_delay_moves_delivery_exactly_one_step_later_than_the_unfaulted_run`,
    `the_same_faulted_drm_executed_twice_produces_byte_identical_run_products_and_port_traffic`,
    `a_port_fault_naming_an_undeclared_port_is_a_typed_load_error`,
    `a_port_fault_naming_an_unknown_kind_is_a_typed_load_error`,
    `a_port_fault_naming_corrupt_or_duplicate_is_a_distinct_typed_load_error_naming_r4_1b`,
    `a_port_fault_missing_its_scenario_seed_is_a_typed_load_error`,
    `two_port_faults_wired_through_execute_do_not_interfere`,
    `a_port_fault_off_the_sample_grid_still_loads_and_applies`.
  - 12 + 2 + 9 = 23, matching the measured delta exactly.
  - `crates/av-kernel/tests/demo_measurements.rs`'s own count is UNCHANGED (one existing test
    renamed/re-targeted, not added -- `a_declared_port_drop_fault_...` became
    `a_declared_port_corrupt_fault_..._pending_r4_1b`).

### Clippy

- `cargo clippy --workspace --all-targets -- -D warnings` -- saved to
  `/private/tmp/claude-501/-Users-probe-code-AltaVista/ed238b37-d349-42f1-956b-22807101027f/scratchpad/clippy_run.log`.
  **Result: clean.** `Finished \`dev\` profile [unoptimized + debuginfo] target(s) in 1m 57s`, 0
  lines containing `warning`/`error` in the whole log (`grep -c "^warning\|^error"` = 0). No new
  `#[allow]` was added anywhere in this task's own diff (checked: `git diff` for every touched
  file contains no `#[allow(`).

## 3. Measurements worth keeping

**`demo_command_port_drop.drm.yaml` (drop test).** Predicted (stated in `port_faults.rs`'s own
module doc comment, before running): the OUT record on `ground.cmd_out` survives, no IN record on
`flight.cmd_in`, no ACKED transition, no `PORT_COMMAND` event, exactly one `EVENT_KIND_FAULT`
event, total `RunProducts.events.len() == 9` (4 COMMAND_TRANSITION + 4 LIFECYCLE + 1 FAULT), total
`PortTrafficLog.records.len() == 1`. Measured: all of the above, exactly.

**`demo_command_port_delay.drm.yaml` (delay test).** Predicted: `params["delay_s"] = 1.0` (one
whole native step, since `output_period_ns` is also 1 s in this fixture) adds exactly one step to
delivery -- the faulted run's own `PORT_COMMAND.applied_tai_ns` and the ack's own real emission
epoch (read off the sidecar) must each be exactly `output_period_ns` later than the SAME run's own
live-measured unfaulted baseline. Measured: both hold exactly, `N = 1` step confirmed both ways.

**A real bug found and fixed in the test itself, not the implementation (see "Defects," below,
item 1):** the first version of the delay test asserted the ACKED `CommandTransition` event's own
`tai_ns` shifts by one step. It did not -- `tai_ns` stayed IDENTICAL to the unfaulted baseline's
own value in both runs. Root-caused (not assumed): `tests/drm_command.rs::
the_ground_issued_command_drm_runs_through_execute_and_reaches_acked`'s own existing assertion
(`assert_eq!(transitions[4].tai_ns, applied_tai_ns, ...)`) already establishes that ACKED's own
`tai_ns` equals `PORT_COMMAND.applied_tai_ns` (the consuming step's own START epoch), never the
ack packet's own later, real emission epoch -- a pre-existing fact about this codebase's own
command-event bookkeeping, unrelated to this task's own PORT fault work. The Router-level delay
mechanism itself was correct throughout; only the test's own comparison target was wrong. Fixed by
reading the ack's own real emission epoch off `PortTrafficLog` (`ack_out`'s OUT record) instead of
off any `Event`.

**A second, unrelated test-methodology bug found and fixed (see "Defects," item 2):** the
byte-identical-determinism test originally ran the two executions into two DIFFERENT
`products_dir`s. `RunProducts.provenance.attributes["port_traffic_uri"]` embeds the literal
directory path, so the two runs' encoded `RunProducts` genuinely differed -- correctly, for a
reason that has nothing to do with this task's own determinism claim. Fixed by running both
executions into the SAME `products_dir` (the second overwriting the first's `port_traffic.pb` on
disk, read back into memory before the second run starts) -- this is exactly the methodology
`tests/faults_determinism.rs`'s own pre-existing test already uses (`products_dir: None` for
both, sidestepping the issue entirely; PORT faults need `products_dir: Some` to inspect the
sidecar, so this task's own test could not simply omit it).

**Break-and-restore evidence (question 178's own "every new test must fail against a nameable
wrong implementation" rule).** Nine wrong implementations were built, run against the relevant
real test(s), confirmed to fail with the panic text below, then reverted; `git diff` on the
touched file was empty after every restore (checked, not assumed):

1. **`router.rs`: drop never suppresses IN/delivery** (`if false && dropped` instead of
   `if dropped`). `router::tests::a_drop_fault_suppresses_the_in_record_and_delivery_but_keeps_the_
   out_record` -- `assertion left == right failed: OUT only, no IN: [...] left: 2 right: 1`.
   `port_faults::demo_command_port_drop_suppresses_delivery_keeps_the_out_record_and_emits_one_
   fault_event` -- `assertion left == right failed: no ACKED: the command never reached flight`.
2. **`router.rs`: delay fault has no effect** (`extra_delay_ns` computed but never added).
   `router::tests::a_delay_fault_adds_its_own_delay_to_the_connections_declared_latency` --
   `panicked ... not yet available: held` (delivered early). `router::tests::
   two_delay_faults_on_the_same_port_sum_their_own_delays` -- `assertion failed: ... is_empty()`.
   `port_faults::demo_command_port_delay_moves_delivery_exactly_one_step_later_than_the_unfaulted_
   run` -- `left: 1700000052000000000 right: 1700000053000000000` (0 steps, not 1).
3. **`router.rs`: missing seed silently defaults to 0** instead of refusing.
   `router::tests::install_port_faults_refuses_a_missing_seed` -- `called Result::unwrap_err() on
   an Ok value: ()`. `port_faults::a_port_fault_missing_its_scenario_seed_is_a_typed_load_error`
   -- the run SUCCEEDED and printed a full `RunProducts` instead of refusing.
4. **`router.rs`: every fault shares seed 0**, ignoring its own `Scenario.seeds` entry.
   `router::tests::two_port_faults_on_different_ports_draw_from_independent_seeded_substreams` --
   `assertion left == right failed: fault A's own outcome sequence must match its own seeded
   stream exactly` (25-element boolean sequences disagreed).
5. **`router.rs`: `take_applied_port_faults` never drains** (`.clone()` instead of `.take()`).
   `router::tests::a_drop_fault_suppresses_the_in_record_and_delivery_but_keeps_the_out_record` --
   `panicked ... a second drain with nothing newly applied must be empty, not a repeat`.
6. **`events.rs`: `declared_events` never includes a PORT fault** (`is_applicable_port_fault =
   false`, the pre-R4.1a behaviour). `drm::events::tests::
   an_in_range_applicable_port_fault_is_included_but_an_unimplemented_kind_is_not` -- the events
   list printed showed only the two lifecycle events, no FAULT event.
7. **`executor.rs`: a PORT fault's own epoch is subjected to the DYNAMICS/HARDWARE sample-grid
   check** (rule 9 removed). `port_faults::a_port_fault_off_the_sample_grid_still_loads_and_
   applies` -- `panicked ... FaultEpochNotOnSampleGrid { fault_id: "f_off_grid", tai_ns:
   1700000049500000000, sample_interval_s: 1.0 }` (the run that must load instead refused).
8. **`executor.rs`: corrupt/duplicate no longer get their own R4.1b-naming refusal** (falls
   through to `Router`'s generic `UnsupportedPortFaultKind` instead).
   `port_faults::a_port_fault_naming_corrupt_or_duplicate_is_a_distinct_typed_load_error_naming_
   r4_1b` -- got `Router(UnsupportedPortFaultKind { fault_id: "f_unimplemented", kind: "corrupt"
   })`, not `PortFaultKindNotYetSupported`.
   `demo_measurements::a_declared_port_corrupt_fault_against_the_star_tracker_instance_is_a_typed_
   load_refusal_pending_r4_1b` -- identical wrong-variant failure.
9. **`router.rs`: a drop fault also suppresses the OUT record** (rule 4's "keep the OUT, gate the
   IN" violated the other direction from #1). `router::tests::
   a_drop_fault_suppresses_the_in_record_and_delivery_but_keeps_the_out_record` -- `assertion left
   == right failed: OUT only, no IN: [] left: 0 right: 1`.

## 4. Defects found, including my own

1. **My own test bug (not an implementation bug):** the delay test's first draft compared the
   ACKED `CommandTransition` event's own `tai_ns` against the ack's real emission epoch. Those are
   two different, already-documented-elsewhere epochs (`tests/drm_command.rs`'s own existing
   assertion already states ACKED lands at `applied_tai_ns`, not the ack's own later emission) --
   my test conflated them. Root-caused by isolating the Router's own delay+base-latency math in a
   temporary unit test (confirmed correct in isolation) before concluding the bug was in the test,
   not the implementation, per the standing "test all bugs until a definitive root cause is found"
   rule. Fixed; see "Measurements," above.
2. **My own test bug:** the byte-identical-determinism test used two different `products_dir`s,
   which legitimately makes the encoded `RunProducts` differ (the sidecar URI is embedded in
   provenance) for a reason unrelated to fault-runtime determinism. Fixed; see "Measurements,"
   above.
3. No defects were found in the delivered implementation itself beyond the two test bugs above --
   every break-and-restore case (section 3) confirms the real code behaves as documented once the
   deliberate bug is reverted.

## 5. Escalations for the manager

1. **`Fault.clear == true` on a PORT fault is refused, not honoured** (`RouterError::
   PortFaultClearNotSupported`). Rule 3 explicitly permitted this if `clear` "cannot be honoured
   coherently in this design" -- my own conclusion: honouring it needs a rule for WHICH field ties
   a later "clear" `Fault` to the earlier one it ends. `Fault.id` equality at a different `tai_ns`
   is the most natural reading, but neither the proto nor ADR-005 section 5 states this, and nor
   does this crate anywhere else enforce `Fault.id` uniqueness across `Scenario.faults` (a
   precondition that rule would silently need). Guessing this now would become an unreviewed part
   of the wire contract the moment R4.1b or any real DRM relied on it. Recommend the manager
   decide the exact matching rule (id equality? shared `instance`+`target`? something else?)
   before R4.1b, since `corrupt`/`duplicate` faults may want `clear` too.
2. **Multiple PORT faults with overlapping windows on the identical `(instance, port)`** are
   handled (every applicable fault always draws, in `(tai_ns, id)`-sorted order; a drop from any
   one drops the frame; delays from every applying fault sum) but not exercised by an `execute()`-
   level or even a fully adversarial `router.rs` unit test beyond the two-delay-fault sum case
   (`two_delay_faults_on_the_same_port_sum_their_own_delays`). Rule 7 itself names this exact
   shape as the case where the `(tai_ns, instance, port)`+window join from a `PortTrafficRecord`
   back to "which fault caused this" becomes genuinely ambiguous for a REPLAY consumer trying to
   reconstruct which fault produced a given record after the fact (as opposed to this Router's own
   internal bookkeeping, which is unambiguous). No DRM in this crate declares this shape today.
   Flagging per rule 7's own instruction ("if you conclude that join is genuinely ambiguous ...
   STOP and report it") -- I did not stop mid-task since the ambiguity only matters for a REPLAY
   consumer, out of this task's own scope (replay is explicitly R4.1b's), but the manager should
   decide whether a future replay design needs `PortTrafficRecord` to carry a fault-attribution
   field after all, before two such faults are ever declared in a real DRM.
3. **One event per fault, at first real effect, was my own design choice among the several rule 7
   named as legitimate** (one per applied frame; one per fault regardless of whether it ever
   fired). I believe "first real effect" is the closest analogue to question 137's "record changes
   only" rule and to how `EVENT_KIND_CONTACT_START`/`_END` already treat a continuously-evaluated
   condition as a transition, not a level -- but this is a genuine design choice, not something
   ADR-005 section 5 states explicitly, and the manager may want a different one (e.g., one event
   per fault occurrence-window boundary, which would need a second event kind for "window ended").

## 6. What remains for R4.1b

- `corrupt`/`duplicate` kinds: `Router::install_port_faults`/`deliver` need two new
  `PortFaultKind` arms. The seeded-substream/window/rate machinery is already generic; only the
  per-kind *effect* (mutating bytes for corrupt, re-queuing a second delivery for duplicate) is
  new work.
- Rule 4's "where a kind mutates bytes (corrupt), the IN records carry post-fault bytes and the
  OUT record carries the emitter's own original bytes" is already anticipated in this task's own
  module doc comment but not implemented -- R4.1b's own job.
- The replay test explicitly deferred by this task's own scope ("Do NOT write the replay test --
  that is R4.1b's, after corrupt/duplicate land").
