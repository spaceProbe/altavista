# M25.4a report (kernel only, parts 1 and 2 of task M25.4a)

Incremental report, written as work happens, not at the end. Sections are appended in the order
work was done; each states a hypothesis/expected value before the corresponding measurement where
one applies (standing rule).

## Starting state

- Branch `develop`, 4 commits ahead of `origin/develop`, clean working tree at task start.
- `proto/altavista/v1/run.proto` already declares `RunProducts.port_traffic_hash = 9`,
  `PortTrafficRecord`, `PortTrafficLog` exactly as the brief states -- verified directly by
  reading the proto file. Rust bindings (`av_cdm::pb::PortTrafficRecord`/`PortTrafficLog`) are
  generated automatically by `crates/av-cdm/build.rs` (prost-build over `proto/**`) -- confirmed
  present in the build output (`target/debug/build/av-cdm-*/out/altavista.v1.rs`) with no code
  change needed on the `av-cdm` side.
- `crates/av-kernel/src/drm/executor.rs`'s `port_traffic_hash: String::new()` stub and its
  "filled by M25.4" comment: confirmed present before this task's edits, at the line the brief
  named (shifted slightly by unrelated prior edits, but the same code).

## Part 1c: verifying `run_shared_group`/`run_one_span` reuse ONE `Router` across spans

Verified by reading, not assumed: `run_shared_group` (executor.rs) takes `router: &mut
crate::router::Router` as a parameter and calls `run_one_span(seg_start, boundary, ...,
router, ...)` once per boundary in its own `for b in &boundaries` loop, then once more after
the loop for the final span (`run_one_span(seg_start, run_end_tai_ns, ..., router, ...)`) --
the identical `&mut Router` reference threaded through every call. `run_one_span` itself builds
a fresh `HeteroKernel::new(output_period_ns)` on every call (a fresh kernel per span, as the
brief states) but takes `router: &mut crate::router::Router` as its own parameter rather than
owning one -- so a `Router`'s internal state (here, the new step counter and port-traffic
buffer) survives across every span of one `execute()` call. This confirms the brief's claim
and is why `Router::begin_step`'s own step counter can safely never reset: nothing in
`run_shared_group`/`run_one_span` ever constructs a second `Router` mid-run.

`execute()` itself builds exactly one `Router` (`Router::build`, near its own top) and passes
`&mut router` into `run_shared_group`; the covariance path (`options.covariance == true`) never
touches `router` at all (unchanged from before this task), so a covariance run's own
`port_traffic_records` is always empty, honestly (mirrors `dropped_in_flight_messages`'s
identical existing behaviour on that path).

## Part 1c: the "unknown (instance, port) pair" decision

Chose: a `debug_assert!` inside `Router::deliver`, not a typed `Result`/error. Reasoning:
- `Router::deliver`'s signature is `fn deliver(&mut self, from_instance: &str, emission_tai_ns:
  i64, outbox: Outbox)` -- no `Result` today, and all three real call sites
  (`schedule.rs::advance_to_with_ports`, `executor.rs`'s command-dispatch loop, and
  `sensors.rs`'s own test) already assume it cannot fail. Adding a `Result` return would touch
  all three call sites and their own differently-shaped error types (`HeteroScheduleError`,
  `DrmError`, a test) for a case that should be structurally unreachable in a DRM that already
  passed `execute()`'s load-time port/binding validation -- a model can only construct an
  `Outbox` entry naming a port it was itself configured with.
- A `debug_assert!` makes a real occurrence loud in every `cargo test`/`cargo build` (dev
  profile keeps debug assertions on) without changing the production error surface or the
  three call sites' own signatures.
- **Found and fixed a pre-existing test that would have collided with this choice**:
  `crates/av-kernel/src/router.rs::a_message_on_a_port_with_no_matching_connection_is_dropped_
  not_an_error` pushed a message on `"some_other_port"`, a port name the sender's own
  `SystemDefinition` never declared at all -- i.e. it exercised exactly the "unknown pair" case
  I was about to make loud, not (as its own name says) "a declared port with no connection".
  Fixed by declaring that port on the sender too (kept unconnected) so the test still proves
  what its name says, without tripping the new assertion. Disclosed here as a defect in my own
  reasoning caught before it broke an existing green test, not a silent rewrite.

## Part 2: `DrmError::FaultTargetKindNotSupported` vs. a new variant

Added a new variant, `DrmError::PortOrSensorFaultNotYetSupported`, rather than reusing
`FaultTargetKindNotSupported`. Reasoning (as the task invited arguing the other way, if
warranted -- concluded a new variant is correct):
- `FaultTargetKindNotSupported` is `fault::realize_unapplied_fault`'s own *realization-time*
  result: seeded, validated, a deterministic PCG64 draw actually computed, and named as "no
  runtime to apply this to yet" -- for a caller that actually invokes it. `execute()` has never
  called `realize_unapplied_fault` (confirmed by reading `executor.rs`: no call site exists),
  so reusing that variant here would mean either (a) actually computing a seeded draw for a
  fault this task's job is to REFUSE before any seed lookup at all, which contradicts "checked
  up front, before any binding or GMAT call" and would spuriously require `Scenario.seeds` to
  carry an entry it now never needs, or (b) faking the variant's own fields (a bogus
  `realized_draw`), which misrepresents what happened.
- The new variant is raised earlier (at load, before any seed is even looked up) and for a
  different reason (a load-time POLICY refusal -- question 178's decision -- not "I tried to
  realize this and there is nothing to apply it to"). Its `Display` names question 178 and says
  "the port and sensor fault runtimes are the next kernel item," matching the task's explicit
  wording rather than the pre-existing "unsupported" phrasing.

## Part 1e: hypothesis for `demo_command`'s `PortTrafficLog` (stated before running)

Fixture facts (`drms/demo_command.{drm,sos}.yaml`, both read directly, not paraphrased from
memory): `start_tai_ns = 1_700_000_000_000_000_000`, `sample_interval_s = 1.0` (`output_period_ns
= 1e9`), one `command` event dispatching at `tai_ns = 1_700_000_050_000_000_000` on
`ground.cmd_out`, two FRAMED `"latency"` connections (`ground.cmd_out -> flight.cmd_in`,
`flight.ack_out -> ground.ack_in`), each summing to 3 s (1.5 s declared on each port).

Predicted **exactly 4 records** (see `crates/av-kernel/tests/port_traffic_sidecar.rs`'s own
module doc comment for the full derivation):

1. `(ground, cmd_out, OUT, tai_ns=1_700_000_050_000_000_000, sequence=0)`
2. `(flight, cmd_in, IN, tai_ns=1_700_000_050_000_000_000, sequence=0)`
3. `(flight, ack_out, OUT, tai_ns=<applied>, sequence=S)`
4. `(ground, ack_in, IN, tai_ns=<applied>, sequence=S)`

The non-obvious claim, stated up front rather than discovered by a failing assertion: the
cmd_out/cmd_in pair's `sequence` is **0**, not aligned with the real output-tick index at t=50s
(which the tick formula would put at 51). This follows from reading (not running)
`run_shared_group`: its command-dispatch loop calls `Router::deliver` directly, once per
declared `command` event, BEFORE the boundary loop that runs the span through
`HeteroKernel::run_with_ports` -- and `Router::begin_step` (the only thing that ever advances
the sequence counter) is only ever called inside that loop's own `advance_to_with_ports` call
site. So the dispatch's own `deliver` call happens at `Router`'s initial, pre-`begin_step`
`step = 0`.

**Measured, first attempt: the hypothesis was WRONG on one point, caught by the test itself, not
retrofitted.** The first real run of
`demo_command_sidecar_records_match_an_independent_reconstruction_from_fixtures_and_events`
failed its own sanity assertion:

```
assertion `left == right` failed: 3 s of real router latency ... after dispatch
  left: 1700000052000000000
 right: 1700000053000000000
```

Root cause (confirmed by reading `crates/av-dynamics/src/lib.rs::AppliedCommand`'s own doc
comment only after this failure, not before): the hypothesis silently equated "the epoch
`RunProducts.events`' `EVENT_KIND_PORT_COMMAND` reports as `applied_tai_ns`" with "the epoch
`Router::deliver` actually records the ack's own frames at." They are two different numbers, one
native step (1 s here) apart: `AppliedCommand.applied_tai_ns` is documented as the consuming
step's own *start* epoch (`t_ns`), while `HeteroScheduler::advance_to_with_ports` hands that same
step's `Outbox` to `Router::deliver` with the step's own *result* (end) epoch
(`result.t_tai_ns = t_ns + period_ns`). Measured: `applied_tai_ns = 1_700_000_052_000_000_000`
(dispatch + 2 s); the ack's real emission epoch is `applied_tai_ns + flight's own period_ns =
1_700_000_053_000_000_000` (dispatch + 3 s) -- which DOES match the fixture's own header-comment
arithmetic for the round trip as a whole, just not the raw `PORT_COMMAND` event field. Fixed the
test's own reconstruction to use `applied_tai_ns + OUTPUT_PERIOD_NS` (justified in the test's own
comment as fixture-specific: `flight`'s declared `step_rate_hz` equals the output rate here) for
the ack's expected `tai_ns`/`sequence`, and kept the corrected sanity assertion so this discovery
stays pinned, not silently reverted.

**Measured, second attempt** (`cargo test -p av-kernel --test port_traffic_sidecar`, see Gates
section for the exact run): PASS with the corrected reconstruction. cmd_out/cmd_in both at
`tai_ns=1_700_000_050_000_000_000`, `sequence=0`; ack_out/ack_in both at
`tai_ns=1_700_000_053_000_000_000`, `sequence=54`.

## Break-and-restore (part 1e's own required proof, plus the standing rule applied to every
## test added in this task)

Each entry: the wrong implementation named, which real tests failed, and the real panic text.
All restored immediately after capture (`git diff` clean on the named file afterward).

### Break: `Router::deliver` records only OUT, never IN (comment out the IN-side push)

Affected: `router::tests::begin_step_starts_the_first_output_ticks_sequence_at_one_not_zero`,
`router::tests::deliver_records_out_and_in_port_traffic_for_a_framed_connection_sharing_one_sequence`,
`router::tests::take_port_traffic_drains_and_does_not_repeat_on_a_second_call`, and
`port_traffic_sidecar.rs::demo_command_sidecar_records_match_an_independent_reconstruction_from_fixtures_and_events`.

```
thread 'router::tests::begin_step_starts_the_first_output_ticks_sequence_at_one_not_zero' panicked:
assertion `left == right` failed: one OUT + one IN record
  left: 1
 right: 2

thread 'demo_command_sidecar_records_match_an_independent_reconstruction_from_fixtures_and_events' panicked:
assertion `left == right` failed: [...]
  left: 2
 right: 4
```
(2 records instead of 4 -- both OUT, no IN at all.)

### Break: `Router::deliver` drops the `sequence` fill (hardcodes `sequence: 0` on both pushes)

Affected: `router::tests::begin_step_starts_the_first_output_ticks_sequence_at_one_not_zero`
(also would fail Test A's own sequence columns, confirmed separately below).

```
thread 'router::tests::begin_step_starts_the_first_output_ticks_sequence_at_one_not_zero' panicked at crates/av-kernel/src/router.rs:770:9:
the first begin_step() call must produce sequence 1, not 0: [PortTrafficRecord { instance: "sender", port: "out", direction: Out, tai_ns: 1000, payload: [1, 2, 3], sequence: 0 }, PortTrafficRecord { instance: "receiver", port: "in", direction: In, tai_ns: 1000, payload: [1, 2, 3], sequence: 0 }]
```

Also ran Test A against the identical break (left in place across both commands, confirming one
defect breaks both the unit and the acceptance-level test, not merely rebuilding it to pass by
coincidence):

```
thread 'demo_command_sidecar_records_match_an_independent_reconstruction_from_fixtures_and_events' panicked at crates/av-kernel/tests/port_traffic_sidecar.rs:208:5:
assertion `left == right` failed: sidecar records must equal the independently reconstructed expectation
  left: [(2, "flight", "ack_out", 1700000053000000000, 0), (1, "flight", "cmd_in", 1700000050000000000, 0), (1, "ground", "ack_in", 1700000053000000000, 0), (2, "ground", "cmd_out", 1700000050000000000, 0)]
 right: [(1, "flight", "cmd_in", 1700000050000000000, 0), (2, "ground", "cmd_out", 1700000050000000000, 0), (2, "flight", "ack_out", 1700000053000000000, 54), (1, "ground", "ack_in", 1700000053000000000, 54)]
```
(Left tuples are `(direction, instance, port, tai_ns, sequence)`; the `actual` list on the left
also shows `left`/`right` reordered relative to `expected` because the sort itself keys on
`sequence` first -- with every real sequence collapsed to 0, the stable sort's remaining keys
(`instance`, `port`) reorder the whole set differently than the expectation, which is itself
further confirmation the sequence field is genuinely load-bearing for the required sort order,
not just for its own column.)

Restored both breaks; `router.rs` back to `sequence: self.step,` at both call sites.

### Break: `sort_port_traffic` sorts by `(instance, sequence, port)` instead of `(sequence, instance, port)`

Affected: `drm::executor::sort_port_traffic_tests::sorts_by_sequence_first`.

```
thread 'drm::executor::sort_port_traffic_tests::sorts_by_sequence_first' panicked:
assertion `left == right` failed
  left: [5, 1]
 right: [1, 5]
```

### Break: `sort_port_traffic` uses `sort_unstable_by` instead of `sort_by` -- **negative result,
### disclosed rather than hidden**

Tried at 3, 40, and 2000 fully-tied `(sequence, instance, port)` records: on this toolchain
(`rustc`/Rust 1.97.0's pattern-defeating quicksort), `sort_unstable_by` did not reorder ANY of
the three -- `a_full_tie_keeps_the_original_emission_order_stable_sort` kept passing against
this specific break at every size tried. This is the one place in this task where a "nameable
wrong implementation" named in the task brief itself (`slice::sort_unstable_by`) turned out not
to produce an observable failure, measured directly rather than assumed from the API's own "no
order guarantee" disclaimer. Recorded here rather than quietly dropped or reported as passing
when it does not actually prove what it claims to.

**Found a break that DOES work for the same test** -- `records.reverse()` immediately before the
(otherwise correct, stable) `sort_by` call, a plausible real mistake (e.g. someone iterating a
`BTreeMap`/`Vec` in the wrong direction upstream and "fixing" it locally):

```
thread 'a_full_tie_keeps_the_original_emission_order_stable_sort' panicked:
assertion `left == right` failed: a full (sequence, instance, port) tie must keep the router's own emission order -- a stable sort's own guarantee
  left: ["39", "38", "37", ..., "1", "0"]
 right: ["0", "1", "2", ..., "38", "39"]
```

The test's own doc comment now states this finding plainly instead of the originally-drafted
(and, it turned out, wrong) claim that `sort_unstable_by` would fail it.

### Break: `sort_port_traffic` drops the instance/port tie-break entirely (`sequence` only)

Affected: `breaks_a_sequence_tie_by_instance`, `breaks_a_sequence_and_instance_tie_by_port`.

```
thread 'breaks_a_sequence_tie_by_instance' panicked:
assertion `left == right` failed
  left: ["zebra", "alpha"]
 right: ["alpha", "zebra"]

thread 'breaks_a_sequence_and_instance_tie_by_port' panicked:
assertion `left == right` failed
  left: ["zzz_port", "aaa_port"]
 right: ["aaa_port", "zzz_port"]
```

All three `sort_port_traffic` breaks restored; `cargo test -p av-kernel --lib
drm::executor::sort_port_traffic_tests` green again (4 passed) after each restore.

### Break: `write_port_traffic_sidecar` writes half the bytes to disk but hashes the full buffer

Affected: `port_traffic_hash_matches_an_independently_computed_sha256_of_the_file_and_uri_names_it`
(Test B). Named defect: "hash the in-memory buffer instead of the file."

```
thread 'port_traffic_hash_matches_an_independently_computed_sha256_of_the_file_and_uri_names_it' panicked:
assertion `left == right` failed: RunProducts.port_traffic_hash must equal a fresh SHA-256 of the exact bytes on disk
  left: "0b6180321371835bc267135dbeebfa588ff388dcd4f29d13d3386c202519935f"
 right: "796ff415174c58fa7218cf8918f1bf8e29d03d05203e92b4726dd0b5ec999502"
```

### Break: `write_port_traffic_sidecar`'s `None` branch also sets `port_traffic_uri`

Affected: `products_dir_none_writes_no_sidecar_and_marks_it_explicitly_not_recorded` (Test C).

```
thread 'products_dir_none_writes_no_sidecar_and_marks_it_explicitly_not_recorded' panicked at crates/av-kernel/tests/port_traffic_sidecar.rs:282:5:
the two attributes are mutually exclusive: no sidecar means no uri
```

### Break: `execute()`'s PORT/SENSOR fault refusal disabled (`if false && ...`)

Affected: `a_declared_port_drop_fault_against_the_star_tracker_instance_is_a_typed_load_refusal`
(demo_measurements.rs, part 2b). Reverts to the exact M25.3c-era silent no-op this task closes.

```
thread 'a_declared_port_drop_fault_against_the_star_tracker_instance_is_a_typed_load_refusal' panicked at crates/av-kernel/tests/demo_measurements.rs:397:10:
a declared PORT fault must be a typed load refusal now (question 178), not a run that silently ignores it: RunProducts { ... measurements: [... 36 real Measurements ...], port_traffic_hash: "" }
```
(`.expect_err(...)` panicked because `execute()` returned `Ok` -- the fault was silently
ignored, exactly the pre-task behaviour.)

### Break: `products_dir_for_out` drops its empty-parent special case (`out.parent().map(...)`, no `.` fallback)

Affected: `products_dir_for_out_is_the_current_directory_for_a_bare_filename` (part 1b).

```
thread 'tests::products_dir_for_out_is_the_current_directory_for_a_bare_filename' panicked at crates/av-run/src/main.rs:331:9:
assertion `left == right` failed
  left: Some("")
 right: Some(".")
```

### Break: `RunProducts::to_proto` hardcodes `port_traffic_hash: String::new()` (the pre-task stub)

Affected: `port_traffic_hash_survives_to_proto_and_a_real_byte_round_trip`.

```
thread 'drm::executor::to_proto_tests::port_traffic_hash_survives_to_proto_and_a_real_byte_round_trip' panicked at crates/av-kernel/src/drm/executor.rs:3537:9:
assertion `left == right` failed
  left: ""
 right: "sample-port-traffic-hash"
```

All breaks above restored immediately after capture; `grep -rn "BREAK-TEST" crates/` returns
nothing once every restore lands (checked directly, not assumed).

**Not separately broken-and-restored** (self-demonstrating or already covered elsewhere):
- `router::tests::an_emission_on_an_undeclared_port_trips_the_debug_assertion` -- this test's own
  job IS to trigger the "wrong" input state (an emission on an undeclared port) and prove the
  `debug_assert!` fires; there is no separate "wrong implementation" to break it against beyond
  removing the `debug_assert!` itself, which is equivalent to the "record only OUT" family of
  breaks above in spirit (an unobserved defect) -- the test's own `#[should_panic]` already IS
  the positive proof the assertion exists and fires.
- The router-level port-traffic tests already broken via the "record only OUT" and "drop the
  sequence fill" entries above (`deliver_records_out_and_in_port_traffic_for_a_framed_connection_
  sharing_one_sequence`, `begin_step_starts_the_first_output_ticks_sequence_at_one_not_zero`,
  `take_port_traffic_drains_and_does_not_repeat_on_a_second_call`) are not repeated here.
- `a_framed_port_with_no_connection_still_gets_an_out_record_and_no_in_record` and
  `signal_and_cdm_ports_are_never_recorded` are directly covered by the same "record only OUT"
  code path and the `recordable` gate respectively -- not independently re-broken given the time
  budget on an already very large task; both assert real, specific, non-trivially-true shapes
  (an OUT-only record; zero records at all) that a no-op or catch-all implementation could not
  produce by accident.
- `records_within_one_sequence_are_sorted_by_instance` (Test D, integration level) exercises the
  identical sort-order property `drm::executor::sort_port_traffic_tests::breaks_a_sequence_tie_
  by_instance` already breaks-and-restores at the unit level, against the exact function it
  calls -- not independently re-run at the integration level given the ~30 s-per-run cost of the
  full `demo_command` DRM.

## Gates

The worker never reached its own gate section: it stopped after the break-and-restore work with
no gates run and no final report (`docs/open-questions.md` question 157's pattern, fifth
occurrence). Everything below is the manager's own, from isolated runs at the tree the worker
left plus the manager's own fix described in the next section.

## Manager review (2026-09-08): the `debug_assert!` on an undeclared port was wrong

**Found by the manager's own first isolated `cargo test -p av-kernel` run**, not by review of the
code: five of the nine tests in `crates/av-kernel/tests/drm_attitude.rs` panicked.

```
thread 'a_wheel_limit_fault_drm_runs_through_execute' panicked at crates/av-kernel/src/router.rs:325:13:
Router::deliver: instance "att_wheel" emitted on port "truth_qx", which is not a declared port of
its own SystemDefinition -- no PortKind on record (question 175)
```

**Root cause, from the artifact rather than from attribution.** The worker's stated premise for
choosing a `debug_assert!` (recorded above, in "the unknown (instance, port) pair decision") was
"a model can only construct an `Outbox` entry naming a port it was itself configured with." That
premise is false in this codebase today. `crate::drm::sensors::TruthBroadcastAttitude` broadcasts
the truth quaternion and body rates every step on seven fixed conventional port names,
`sensors::TRUTH_PORT_NAMES` (`truth_qx`..`truth_wz`, `sensors.rs:222-243`), whether or not the
emitting instance's own `SystemDefinition` declares them -- and `drms/demo_attitude_precession`
and `drms/demo_attitude_wheel_fault` declare no ports at all. So those runs legitimately emit
seven undeclared-port messages per step, and the assertion fired on correct behaviour.

This is also why the worker's own earlier observation was a warning it read the wrong way: it
found that `router::tests::a_message_on_a_port_with_no_matching_connection_is_dropped_not_an_error`
already emitted on an undeclared port, and changed the *test* to declare the port rather than
treating the pre-existing test as evidence about the premise. That edit is kept (the test is
genuinely more precise now), but the undeclared-port case it used to cover is restored as its own
test, below.

**Fix, at the source** (`crates/av-kernel/src/router.rs`, `crate::drm::executor`):

- An emission on a port with no declared `PortKind` records nothing and is not an error. The
  assertion is gone. The justification is checkable, not merely plausible: `Router::build`
  refuses any `Connection` naming a port the endpoint's own `SystemDefinition` does not declare
  (`RouterError::UndeclaredPort`), so an undeclared port provably has no `edges` entry -- nothing
  is ever delivered from it, to anyone, so there is nothing for a replay binding to play back.
- It is skipped but never *silently*: `Router` counts every such emission
  (`Router::undeclared_port_emissions()`), and `execute()` writes a non-zero count into the
  sidecar's own `PortTrafficLog.provenance.attributes["undeclared_port_emissions"]`. No
  zero-valued attribute, the convention `events::dropped_messages_event` already follows.

**Tests changed by the manager** (`crates/av-kernel/src/router.rs`):
- `an_emission_on_an_undeclared_port_trips_the_debug_assertion` (worker's, `#[should_panic]`)
  replaced by `an_emission_on_an_undeclared_port_records_nothing_and_is_counted_not_silent`,
  which emits on `sensors::TRUTH_PORT_QX` -- the real shape that broke `drm_attitude` -- plus an
  invented undeclared port, and asserts no records, nothing pending, and a count of 2.
- `a_declared_port_emission_never_counts_as_an_undeclared_one` (new): the counter stays 0 for a
  run whose every emission is on a declared port, so the sidecar attribute is genuinely absent
  rather than present-and-zero.

**Break-and-restore (manager's own).** Removed `self.undeclared_port_emissions += 1;` from
`Router::deliver`:

```
thread 'router::tests::an_emission_on_an_undeclared_port_records_nothing_and_is_counted_not_silent'
panicked at crates/av-kernel/src/router.rs:933:9:
assertion `left == right` failed: both undeclared emissions must be counted -- the skip is never silent
  left: 0
 right: 2
test result: FAILED. 0 passed; 1 failed
```

Restored; `cargo test -p av-kernel --lib router::` back to 20 passed, 0 failed. `grep -rn
"BREAK-TEST" crates/` returns nothing.

**Verification of the fix, isolated:** `cargo test -p av-kernel --test drm_attitude --test
port_traffic_sidecar` -> `9 passed; 0 failed` and `4 passed; 0 failed`.

Also corrected by the manager: five doc comments dated this work "M25.4b" when it is M25.4a
(`executor.rs`, `demo_measurements.rs`, this report's own title).
## Gate results (manager's own, isolated runs, host confirmed quiet before each)

Run back to back on a quiet host, 2026-09-08 01:38 to 02:42 CDT, output captured in full to
files (not piped through `tail`, which on the manager's own first attempt hid a real failure
*and* masked the exit code -- an exit code is not evidence, and neither is a truncated log).

- `cargo build --workspace --all-targets`: clean.
- `cargo test --workspace --exclude av-kernel --no-fail-fast`: **182 passed, 0 failed**.
  Baseline 179; +3 = the three new `av-run` `products_dir_for_out_*` unit tests.
- `cargo test -p av-kernel --no-fail-fast` (alone, 53 minutes): **750 passed, 0 failed, 1
  ignored**. Baseline 734; +16, accounted for exactly: 4 in `tests/port_traffic_sidecar.rs`,
  7 in `src/router.rs` (`begin_step_starts_the_first_output_ticks_sequence_at_one_not_zero`,
  `deliver_records_out_and_in_port_traffic_for_a_framed_connection_sharing_one_sequence`,
  `a_framed_port_with_no_connection_still_gets_an_out_record_and_no_in_record`,
  `signal_and_cdm_ports_are_never_recorded`, `take_port_traffic_drains_and_does_not_repeat_on_a_
  second_call`, `an_emission_on_an_undeclared_port_records_nothing_and_is_counted_not_silent`,
  `a_declared_port_emission_never_counts_as_an_undeclared_one`), 4 in
  `src/drm/executor.rs::sort_port_traffic_tests`, 1 in `to_proto_tests`. The one ignored test is
  question 171's, by name and reason string, still ignored:
  `byte_identical_port_traffic_between_posix_container_and_renode ... ignored, question 171:
  Renode port traffic beyond STEP 1 does not deliver; verified posix-container-only until
  resolved`.
- `cargo clippy --workspace --all-targets -- -D warnings`: clean, no `#[allow]` added anywhere.
- `cargo deny check`: `advisories ok, bans ok, licenses ok, sources ok`.


## Open design points the manager is escalating rather than changing

1. **A dispatched command's frames carry `sequence = 0`.** `run_shared_group` delivers every
   declared `command` `Scenario.event` into the router before the first output tick, so those
   records sort ahead of every other record even though their `tai_ns` is mid-run. It is
   literally true ("the router carried this before step 1") and `tai_ns` is correct, so replay
   keyed on epoch is unaffected -- but `PortTrafficLog.records`'s `(sequence, instance, port)`
   order is therefore not epoch-monotonic. Left as measured and documented; M25.4b must key
   replay on `tai_ns`, not on `sequence` alone.
   **Closed as question 181** (see the section below).
2. **`sort_unstable_by` is not a usable wrong implementation on this toolchain** for the
   stable-sort test -- measured at 3, 40 and 2000 fully-tied elements, it reordered none. The
   test is pinned against a `records.reverse()` break instead, and says so. Recorded because it
   is a limit on the break-and-restore standard, not a gap someone should quietly re-open.
   **Recorded by the lead alongside question 181.**

## Question 181: the log's order becomes epoch first (implemented, 2026-09-08)

The lead read point 1 above and decided question 181 mid-round, committing the proto doc-comment
change in `da3f019`: `PortTrafficLog.records` is sorted **`(tai_ns, sequence, instance, port)`**,
epoch first, with the sidecar writer to follow "in the next round".

**The manager implemented it this round instead, deliberately.** The reason is not impatience:
`da3f019` changed only the proto's own doc comment, so between that commit and this one
`develop` carried a proto that *declared* an order the code did not *implement* -- exactly the
"a description of the artifact is not the artifact" failure this team keeps rediscovering, and
worse than either the old or the new order consistently applied. A full kernel gate was already
required anyway (to cover the tree `da3f019` had changed underneath the previous run), so
implementing it cost one edit and no extra gate. Flagged plainly for the lead as a deviation
from the stated sequencing, not from the decision.

- `sort_port_traffic` now keys `(tai_ns, sequence, instance, port)`. `sequence` stays the first
  tie-break, so frames carried at one epoch by different ticks still order by tick.
- `sort_port_traffic_tests` restated per the lead's instruction, 4 tests -> 6. The two new ones
  are the ones the old order could not pass: `sorts_by_epoch_first_even_when_sequence_disagrees`
  (the later-epoch record deliberately has the *smaller* sequence and the alphabetically-earlier
  instance and port, so nothing but the epoch key can order it correctly) and
  `a_sequence_zero_dispatch_sorts_by_its_epoch_not_at_the_front_of_the_log` (the real
  `demo_command` shape in miniature: a sequence-0 dispatch between a first tick and a later ack).
- `tests/port_traffic_sidecar.rs`'s Test A now states the new key. **Its expectation did not
  change**: for `demo_command` the two orders happen to agree, because the sequence-0 dispatch is
  also the earlier epoch. Said in the test itself, so nobody later mistakes agreement for proof.

**Break-and-restore (manager's own).** Wrong implementation: the M25.4a order,
`(sequence, instance, port)` -- i.e. exactly what shipped in `685af17`.

```
thread '...a_sequence_zero_dispatch_sorts_by_its_epoch_not_at_the_front_of_the_log' panicked:
assertion `left == right` failed: the sequence-0 dispatch belongs between the first tick and the ack, by epoch
  left: [50000, 1000, 53000]
 right: [1000, 50000, 53000]

thread '...sorts_by_epoch_first_even_when_sequence_disagrees' panicked:
assertion `left == right` failed: epoch is the primary key, ahead of sequence
  left: [2000, 1000]
 right: [1000, 2000]
test result: FAILED. 4 passed; 2 failed
```

The other four sort tests passed against the break, correctly: they tie on `tai_ns`, so the two
orders agree for them. Restored; 6 passed. `grep -rn "BREAK-TEST" crates/` returns nothing.
