# R4.1b: `corrupt`/`duplicate` PORT faults, two rulings, and the replay test

Task: `docs/open-questions.md` questions 178, 184, 186 (a/b/c); ADR-005 section 5. Finishes the
PORT fault runtime R4.1a built (`drop`/`delay` real; `corrupt`/`duplicate` a typed refusal naming
R4.1b) by implementing the remaining two kinds on the identical machinery, applying the manager's
two rulings from the R4.1a review (overlapping windows refused at load; the fault event carries an
affected-frame count), and writing the replay test R4.1a's own scope explicitly deferred.

Written incrementally, as instructed: every hypothesis/expected value below was stated (in the
relevant module doc comment, test doc comment, or DRM fixture header comment) BEFORE the
corresponding measurement was taken, not retrofitted afterward -- this report largely restates and
cross-references those in-source predictions rather than duplicating them.

## 1. What was built

- `crates/av-kernel/src/router.rs`:
  - `PortFaultKind` gained `Corrupt`/`Duplicate` variants (was `Drop`/`Delay` only).
  - `InstalledPortFault` gained `corrupt_mask: Option<u8>` (declared `params["corrupt_mask"]`,
    validated `[0, 255]` integer at install) and `duplicate_offset_ns: i64` (always the run's own
    `output_period_ns`, a new parameter `Router::install_port_faults` now takes) and
    `frames_affected: u64` (question 186(c), total count over the whole run, not just the first
    epoch).
  - New free function `corrupt_payload` -- XORs a declared mask into every byte, or (no mask
    declared) draws one uniformly-random bit position from the fault's own seeded stream and
    flips it. Documented, both halves, in the module doc comment's new "Corrupt" section.
  - `Router::deliver`'s own fault-matching loop rewritten around a new local `PortFaultEffect`
    enum (`Drop`/`Delay(i64)`/`Corrupt(Vec<u8>)`/`Duplicate(i64)`) resolved once per candidate
    frame, replacing R4.1a's `dropped: bool` + `extra_delay_ns: i64` accumulator pair -- the
    accumulator's own "sum every applying fault's delay" behavior is DELETED (question 184/
    186(b) make it unreachable; see "Defects," item 1, and "Measurements," below). Delivery
    dispatch generalized to a single "record IN(s), deliver message(s)" block parameterized by
    `(delivery_payload, extra_delay_ns, duplicate_offset_ns: Option<i64>)`, covering all four
    kinds without duplicating the OUT/IN/pending-queue bookkeeping four times.
  - `Router::install_port_faults` signature gained `output_period_ns: i64`; gained the
    question-184/186(b) overlap check (pairwise, against every already-installed fault on the
    identical `(instance, port)`, inside the existing all-or-nothing validation loop) and the
    `corrupt_mask` validation.
  - New `RouterError` variants: `InvalidPortFaultCorruptMask`, `OverlappingPortFaultWindows`
    (names both fault ids and the overlapping interval, per the task's own instruction).
  - `AppliedPortFault` gained `pub frames_affected: u64`; `Router::take_applied_port_faults` drains
    it alongside `first_applied_tai_ns`.
  - Module doc comment's "Port fault runtime" section: new "Corrupt"/"Duplicate"/"Overlapping
    windows are refused at load"/"Replay re-applies every installed PORT fault" subsections;
    "Multiple applicable faults on one port" retired (superseded by the overlap rule); "Events"
    section updated for `frames_affected`.
  - 16 new/converted unit tests in `router.rs`'s own `mod tests` (9 net new; see section 2).
- `crates/av-kernel/src/drm/executor.rs`:
  - Load-time fault-validation loop: the `corrupt`/`duplicate` -> `PortFaultKindNotYetSupported`
    branch removed; every PORT kind in `fault::PORT_KINDS` now falls through to
    `Router::install_port_faults` for the rest of its own validation.
  - `router.install_port_faults(...)` call site passes the new `output_period_ns` argument.
  - `events::port_fault_event(...)` call site passes `applied.frames_affected`.
  - `RunProducts::measurements`'s own doc comment extended: `corrupt`/`duplicate` never
    retroactively change an already-decoded `Measurement` either (decoding happens at the emitter,
    before the fault runtime is ever consulted).
- `crates/av-kernel/src/drm/mod.rs`: `DrmError::PortFaultKindNotYetSupported` REMOVED (see
  "Escalations," item 1, for the "keep or remove" decision this task was asked to make).
  `DrmError::UnknownPortFaultKind`'s and `PortOrSensorFaultNotYetSupported`'s own doc comments
  updated to say so.
- `crates/av-kernel/src/drm/events.rs`:
  - `events::port_fault_event` gained a `frames_affected: u64` parameter, inserted into the
    returned `Event.values["frames_affected"]` (question 186(c)) alongside `fault.params`
    verbatim (unchanged).
  - `declared_events`'s own PORT-kind filter widened from `"drop" | "delay"` to all four documented
    kinds.
  - 1 new unit test, 1 existing test converted (see section 2).
- `crates/av-kernel/src/drm/fault.rs`: module doc comment updated throughout (PORT now fully real,
  not "two of four kinds"); no logic changes (`realize_unapplied_fault`/`PORT_KINDS` untouched, as
  R4.1a's own report already anticipated).
- `crates/av-kernel/tests/port_faults.rs`: 5 new tests, 1 existing test converted (see section 2);
  module doc comment gained "Test 1b (corrupt)"/"Test 2b (duplicate)" hypothesis sections.
- `crates/av-kernel/tests/demo_measurements.rs`: 1 existing test converted (`kind: "corrupt"` is no
  longer refused, so its own prior "typed load refusal" test now pins the opposite: the run
  succeeds and never retroactively changes `RunProducts.measurements`).
- `crates/av-kernel/tests/replay.rs`: 1 new test, **T5** -- this task's own headline deliverable
  (see section 3).
- New DRM fixtures (`drms/`), all `demo_command.*`/`demo_attitude_control.*`-derived, matching R4.1a's
  own two additions:
  - `demo_command_port_corrupt.drm.yaml` -- a declared `corrupt_mask` fault on `ground.cmd_out`.
  - `demo_command_port_duplicate.drm.yaml` -- a `duplicate` fault on `ground.cmd_out`.
  - `demo_attitude_control_port_duplicate.drm.yaml` -- a `duplicate` fault on `startracker.st_meas`,
    for T5 (why `duplicate`, not `corrupt`, for THIS fixture: see section 3 and the fixture's own
    header comment).

## 2. Verification

All commands run with `export PATH="/opt/homebrew/opt/rustup/bin:$PATH"` first. Contention
(`ps -Ao pid,etime,command | grep -E "cargo test|pytest|docker build"`) was checked and clear
before the full run and before the full-workspace clippy run.

### Targeted (development)

- `cargo test -p av-kernel --lib router::` -- 39 passed, 0 failed (was 32 after R4.1a; +7 net new,
  2 renamed in place -- see below).
- `cargo test -p av-kernel --lib drm::events::` -- 15 passed, 0 failed (+1 net new, 1 renamed in
  place).
- `cargo test -p av-kernel --test port_faults` -- 14 passed, 0 failed (was 9 after R4.1a; +5 net
  new, 1 renamed in place).
- `cargo test -p av-kernel --test replay` -- 8 passed, 0 failed (was 7 before this task; +1 net
  new, T5).
- `cargo test -p av-kernel --test demo_measurements --test drm_command --test port_traffic_sidecar
  --test faults_seeded --test faults_determinism` -- 22 passed, 0 failed (3 + 4 + 4 + 9 + 2;
  `demo_measurements` count UNCHANGED at 3, one test renamed/re-targeted).
- Broader regression sweep (files that touch `Router`/PORT faults/events only indirectly, or
  reuse fixtures this task did not modify): `cargo test -p av-kernel --test drm_attitude_control
  --test drm_attitude_sensors --test ports_router --test dropped_messages --test drm_executor
  --test golden_acceptance --test drm_shared_run --test drm_container` -- 61 passed, 0 failed
  across all 8 files.
- `cargo clippy -p av-kernel --all-targets -- -D warnings` -- clean (one `clippy::doc_lazy_
  continuation` lint fixed during development -- see "Defects," item 3 -- before this clean run;
  `Finished` in 1m 19s, 0 lines matching `^warning|^error`).

**Test count reconciliation (net new vs. renamed-in-place):**

| File | New tests | Renamed/converted (no count delta) |
|---|---|---|
| `router.rs` (unit) | `a_declared_corrupt_mask_is_xored_into_every_byte_on_the_in_side_only`, `a_corrupt_fault_with_no_declared_mask_flips_exactly_one_bit_from_its_own_seeded_stream`, `a_duplicate_fault_delivers_a_second_copy_one_output_period_later_with_its_own_in_record_but_no_second_out_record`, `frames_affected_counts_every_applied_frame_not_just_the_first`, `overlapping_windows_are_refused_regardless_of_the_two_faults_own_kinds`, `install_port_faults_refuses_a_non_byte_corrupt_mask`, `two_delay_faults_on_the_same_port_with_disjoint_windows_are_both_legal_and_independent` (7) | `install_port_faults_refuses_corrupt_and_duplicate_the_same_generic_way_at_the_router_level` -> `install_port_faults_accepts_corrupt_and_duplicate_as_real_kinds_r4_1b`; `two_delay_faults_on_the_same_port_sum_their_own_delays` -> `two_delay_faults_on_the_same_port_with_overlapping_persistent_windows_are_refused_at_load` |
| `drm/events.rs` (unit) | `port_fault_event_carries_frames_affected_in_values_distinguishing_one_frame_from_many` (1) | `an_in_range_applicable_port_fault_is_included_but_an_unimplemented_kind_is_not` -> `an_in_range_applicable_port_fault_is_included_for_all_four_kinds_but_an_unknown_kind_is_not` |
| `tests/port_faults.rs` | `demo_command_port_corrupt_mutates_the_bytes_flight_receives_and_keeps_the_out_record_original`, `demo_command_port_duplicate_delivers_the_command_twice_but_is_invisible_to_command_semantics`, `the_same_corrupt_faulted_drm_executed_twice_produces_byte_identical_run_products_and_port_traffic`, `the_same_duplicate_faulted_drm_executed_twice_produces_byte_identical_run_products_and_port_traffic`, `two_port_faults_on_the_same_port_with_overlapping_windows_are_a_typed_load_refusal_naming_both_ids` (5) | `a_port_fault_naming_corrupt_or_duplicate_is_a_distinct_typed_load_error_naming_r4_1b` -> `a_port_fault_naming_corrupt_or_duplicate_now_loads_and_applies_r4_1b` |
| `tests/replay.rs` | `t5_replaying_the_emitting_instance_of_a_duplicate_port_fault_reproduces_the_faulted_run_byte_identically` (1) | -- |
| `tests/demo_measurements.rs` | -- (0) | `a_declared_port_corrupt_fault_against_the_star_tracker_instance_is_a_typed_load_refusal_pending_r4_1b` -> `a_declared_port_corrupt_fault_against_the_star_tracker_instance_never_retroactively_changes_measurements` |

**Total net new: 7 + 1 + 5 + 1 + 0 = 14.**

### Full run (the gate)

- `cargo test -p av-kernel` -- saved to
  `/private/tmp/claude-501/-Users-probe-code-AltaVista/ed238b37-d349-42f1-956b-22807101027f/scratchpad/full_test_run.log`.
  **Baseline (R4.1a, commit `1bef35e`): 793 passed, 0 failed, 1 ignored. Predicted, stated before
  running: 793 + 14 = 807 passed, 0 failed, 1 ignored** (the 14 net-new tests counted above; every
  renamed test keeps the total unchanged by construction).
  **Measured: 807 passed, 0 failed, 1 ignored** -- exactly the prediction
  (`grep -oE "[0-9]+ passed; [0-9]+ failed; [0-9]+ ignored" full_test_run.log | awk '{p+=$1;
  f+=$3; i+=$5} END {print p, f, i}'`, summed across every test binary in the run). The one
  ignored test is still `drm_attitude_control_renode.rs::byte_identical_port_traffic_between_
  posix_container_and_renode` ("question 171: Renode port traffic beyond STEP 1 does not deliver;
  verified posix-container-only until resolved") -- unchanged, as required. `+14` is exactly
  accounted for, by name, in the table above.

### Clippy

- `cargo clippy --workspace --all-targets -- -D warnings` -- saved to
  `/private/tmp/claude-501/-Users-probe-code-AltaVista/ed238b37-d349-42f1-956b-22807101027f/scratchpad/clippy_full.log`.
  **Result: clean.** `Finished \`dev\` profile [unoptimized + debuginfo] target(s) in 1m 58s`, 0
  lines matching `^warning|^error` in the whole log, exit code 0. No new `#[allow]` anywhere in
  this task's own diff (`git diff -- crates/av-kernel/src crates/av-kernel/tests | grep -c
  "#\[allow("` -- checked, zero hits).

## 3. Measurements worth keeping

**Test 1b (`demo_command_port_corrupt.drm.yaml`).** Predicted (stated in `port_faults.rs`'s own
module doc comment before running): OUT keeps the original bytes; IN = OUT XOR 0xFF (the declared
`corrupt_mask`); the whole-payload XOR corrupts the CCSDS APID away from 700, so `flight`'s own
`decode_packet` fails and the command is never applied -- same COMMAND_TRANSITION shape as R4.1a's
own drop test (never ACKED), but 2 `PortTrafficLog` records (1 OUT + 1 IN), not 1, since the frame
really is delivered. Measured: all of the above, exactly, including `in_rec.payload` computed and
compared against `out_rec.payload.iter().map(|b| b ^ 0xFF)` (never hand-copied hex).

**Test 2b (`demo_command_port_duplicate.drm.yaml`).** Predicted: `ConstantAccelModel`'s own
"changed, or first" rule means the duplicate's second, identical-value
delivery never re-applies -- same single ACKED transition, at the SAME epoch, as the unfaulted
baseline; only the sidecar differs (1 cmd_out OUT stays 1; 1 cmd_in IN becomes 2, both sharing the
one real `tai_ns`). Measured: exactly that -- `faulted.events.len() == baseline.events.len() + 1`
(the one extra FAULT event), `cmd_in` IN record count 2, both at `COMMAND_TAI_NS`.

**T5 (`demo_attitude_control_port_duplicate.drm.yaml`, this task's own headline deliverable).**
Stated before running (see `tests/replay.rs`'s own extensive doc comment on this test, and the
fixture's own header comment): a `duplicate` fault on `startracker.st_meas` is invisible to the
closed loop's own physics (no idempotency guard needed -- the second delivery carries the
identical, unmutated value); replaying `startracker` reproduces the ENTIRE faulted run's own
`RunProducts` byte for byte, nothing excluded, matching `t1_`/`t1b_`'s own bar in the same file.
Measured: `run_real.to_proto().encode_to_vec() == run_replayed.to_proto().encode_to_vec()`,
exactly; `in_count == 2 * out_count` on `controller.startracker_in` in the real run's own sidecar
(measured live, not merely predicted qualitatively).

**Why `"duplicate"`, not `"corrupt"`, for T5's own closed-loop fixture (a considered substitution,
disclosed, not a shortcut).** Investigated directly against the real consuming code, not assumed:
`crate::drm::controller::AttitudeControllerModel::step_with_ports` decodes `startracker_in` with
`codec::decode_packet(...).map_err(...)?` -- the `?` propagates ANY `CodecError` as a hard model
error, aborting the whole run, unlike `crate::drm::binding::ConstantAccelModel`'s own graceful
`if let Ok(decoded) = ...` (the `demo_command` family). A byte-mask/bit-flip corruption applies to
the WHOLE payload (header included, `crate::router::Router::install_port_faults`'s own contract),
so a fault declared persistent over a 300 s/~20 Hz run (thousands of independent candidate frames,
each drawing its own bit position when no mask is declared) cannot realistically be guaranteed to
never land in the 6-byte CCSDS primary header and trip that `?`. Measured directly, not merely
argued: a seed search (`Pcg64::new(seed)`, `bernoulli(1.0)`, `next_u64() % (38*8)`) over seeds
1..5000 found the first seed whose FIRST bit-flip lands in the 32-byte user-data region only at
seed 2 (bit 270, i.e. still only ONE draw verified safe -- the run's OWN persistent window draws
many more, each independently risking the header). `"Corrupt"` itself IS pinned byte-for-byte,
end to end, by Test 1b above -- just not through a replay of that specific run (`demo_command`'s
own `ground`/`flight` are each unsuited to replay for unrelated reasons, stated in T5's own doc
comment and in `drms/demo_attitude_control_port_duplicate.drm.yaml`'s own header comment).

**The "think hard" conclusion (this task's own explicit instruction): faults DO re-apply during
replay, deliberately, and this is REQUIRED for replay to reproduce a faulted outcome at all.**
`crate::drm::executor::execute` calls `Router::install_port_faults` unconditionally, whether or not
`RunConfig.replay` is set (read directly, not assumed); `Router::deliver` has no notion of "this
Outbox came from a replayed instance." This is safe -- not merely convenient -- because of the
"OUT is always the emitter's own pre-fault bytes" invariant (R4.1a's own rule, extended to
`corrupt`/`duplicate` this task): `crate::drm::replay::ReplayModel` only ever plays back OUT
frames, so a replay run's own re-emission is bit-for-bit what the fault would see on a genuinely
fresh, independent run of the identical DRM -- exactly `port_faults.rs`'s own "two independent runs
of one faulted DRM are byte-identical" determinism property, with a replayed instance standing in
for one of the two runs. The alternative (bypassing fault re-application for a replayed instance)
was considered and rejected: it would be new, unrequested machinery, AND it would be actively WRONG
for `"corrupt"` specifically (bypassing would replay the pre-fault, uncorrupted bytes and never
reproduce the corruption at all). See `router.rs`'s own new "Replay re-applies every installed PORT
fault" doc subsection for the same conclusion at the source.

**Break-and-restore evidence (question 178's own "every new test must fail against a nameable
wrong implementation" rule, matching R4.1a's report section 3 as the standard).** Nine wrong
implementations were built, run against the relevant real test(s), confirmed to fail with the
panic text below, then reverted; `git diff` on every touched file was empty after every restore
(checked, not assumed -- `git diff --stat` showed only this task's own intended changes both
mid-development and at the end):

1. **`router.rs`: a declared `corrupt_mask` is never applied** (`Some(_m) => {}` instead of the
   XOR loop). `router::tests::a_declared_corrupt_mask_is_xored_into_every_byte_on_the_in_side_only`
   -- `assertion left == right failed: declared corrupt_mask=0xFF XORed into every byte: left:
   [1, 2, 3] right: [254, 253, 252]`.
2. **`router.rs`: the no-mask bit-flip is never drawn** (the RNG draw kept, for stream-position
   parity, but the byte array left untouched). `router::tests::a_corrupt_fault_with_no_declared_
   mask_flips_exactly_one_bit_from_its_own_seeded_stream` -- `assertion left == right failed: must
   match the fault's own reconstructed seeded stream exactly: left: [0, 0, 0, 0] right: [64, 0, 0,
   0]`.
3. **`router.rs`: a duplicate fault's own `offset_ns` is dropped**, so both copies land at the
   identical availability epoch. `router::tests::a_duplicate_fault_delivers_a_second_copy_one_
   output_period_later_with_its_own_in_record_but_no_second_out_record` -- `assertion left ==
   right failed: first copy delivered at normal availability: left: 2 right: 1` (both copies
   arrived together at the first `take_inbox` call instead of one apiece).
4. **`router.rs`: the OUT record is (also) recorded twice** -- the opposite-direction break from
   #3, targeting the "never a second OUT record" half of the same rule.
   `router::tests::a_duplicate_fault_delivers_a_second_copy_one_output_period_later_with_its_own_
   in_record_but_no_second_out_record` -- `assertion left == right failed: 1 OUT + 2 IN, stated
   before running: [...4 records shown...]: left: 4 right: 3`.
5. **`router.rs`: the overlap check never fires** (`if false && overlap_start < overlap_end`).
   `router::tests::two_delay_faults_on_the_same_port_with_overlapping_persistent_windows_are_
   refused_at_load` -- `called Result::unwrap_err() on an Ok value: ()`.
6. **`router.rs`: the overlap check over-fires on merely-touching (disjoint) windows** (`<=`
   instead of `<`) -- the opposite-direction break from #5.
   `router::tests::two_delay_faults_on_the_same_port_with_disjoint_windows_are_both_legal_and_
   independent` -- `disjoint windows must stay legal: OverlappingPortFaultWindows { fault_a: "f1",
   fault_b: "f2", ..., overlap_start_tai_ns: 1000, overlap_end_tai_ns: Some(1000) }`.
7. **`router.rs`: `frames_affected` is never incremented** in `Router::deliver`.
   `router::tests::frames_affected_counts_every_applied_frame_not_just_the_first` -- `assertion
   left == right failed: must count every one of the 5 applied frames, not just the first: [...
   frames_affected: 0 ...]: left: 0 right: 5`.
8. **`events.rs`: `frames_affected` is computed but never inserted into the event's own `values`**.
   `drm::events::tests::port_fault_event_carries_frames_affected_in_values_distinguishing_one_
   frame_from_many` -- `assertion left == right failed: left: None right: Some(1.0)`.
9. **`executor.rs`: `Router::install_port_faults` is skipped whenever `RunConfig.replay` is
   `Some`** -- simulating the "bypass fault re-application on replay" alternative T5's own doc
   comment explicitly considered and rejected. `replay::t5_replaying_the_emitting_instance_of_a_
   duplicate_port_fault_reproduces_the_faulted_run_byte_identically` -- `assertion left == right
   failed: replaying the duplicate-faulted instance must reproduce the ENTIRE faulted run byte for
   byte ...` (the replayed run's own re-recorded sidecar carried only ONE `startracker_in` IN
   record per candidate frame instead of the real run's own TWO, and its own `events` list was
   missing the FAULT event entirely -- both real, hash/list-visible differences, not merely a
   record-count check).

## 4. Defects found, including my own

1. **My own design correction, caught before it shipped, not after:** the first draft of
   `Router::deliver`'s own dispatch kept R4.1a's `dropped: bool` / `extra_delay_ns: i64`
   accumulator pattern and simply added `corrupt`/`duplicate` branches alongside it. Reviewing
   against this task's own explicit instruction ("delete the interim delay-summing path... it is
   unreachable and must not be left as dead code") caught that the accumulator pattern itself is
   what needed deleting, not merely extending -- rewritten around a single `PortFaultEffect`
   value per candidate frame (section 1) before any test was written against it, so no
   break-and-restore evidence exists for the accumulator's own removal (there is nothing left to
   accidentally regress it back into).
2. **A real design question resolved by reading the actual consuming code, not assumed:** the
   original intention for T5 (this task's own headline deliverable) was to reuse `"corrupt"` for
   the closed-loop replay test, mirroring the drop/delay demo's own `demo_command` shape. Reading
   `crate::drm::controller::AttitudeControllerModel::step_with_ports`'s own decode call (`?`,
   not `if let Ok`) surfaced a real risk that a corrupt fault persistent over a long, high-rate run
   could eventually corrupt a packet's own CCSDS header and abort the whole run -- not a defect in
   this task's own new code, but a property of the EXISTING closed-loop fixture family this task
   had to discover before committing to a specific fixture/kind pairing. Resolved by switching to
   `"duplicate"` for T5 specifically (see "Measurements," above, and section 5, below, for whether
   this gap should be closed).
3. **A real `clippy::doc_lazy_continuation` lint**, caught by `cargo clippy -p av-kernel
   --all-targets -- -D warnings` during development: a doc-comment line beginning `* 8\`` (meant as
   plain prose, "eight bit positions per byte") was parsed by rustdoc as an unindented Markdown
   list-item continuation. Fixed by rewording, not suppressing (no `#[allow]` added anywhere in
   this task's own diff -- checked).
4. No defects were found in the delivered implementation itself beyond item 1 above (caught before
   any test existed to catch it) -- every break-and-restore case in section 3 confirms the real
   code behaves as documented once the deliberate bug is reverted.

## 5. Escalations for the manager

1. **`DrmError::PortFaultKindNotYetSupported` removed, not kept.** This task's own brief asked
   "decide whether the variant still has a legitimate use... or should be removed, and say which."
   The variant existed solely to name a PORT `kind` that is real, documented (in `fault::
   PORT_KINDS`), but not yet implemented by this crate's own runtime -- exactly `"corrupt"`/
   `"duplicate"` through R4.1a. Now that all four of ADR-005 section 5's own documented PORT kinds
   have a real runtime, no PORT fault can ever reach that state again (`DrmError::
   UnknownPortFaultKind` is the only refusal a PORT fault's own `kind` can still produce, for
   anything outside the documented vocabulary entirely) -- the variant was therefore genuinely
   dead code, not merely unused for now, and removing it follows this task's own explicit
   "unreachable code must not be left as dead code" instruction (applied identically to R4.1a's own
   delay-summing path). If ADR-005 section 5 ever grows a FIFTH PORT kind this crate declines to
   implement immediately, the identical variant shape (or a freshly-added one) is trivial to bring
   back -- nothing about removing it now forecloses that.
2. **T5 (the replay test) uses `"duplicate"`, not `"corrupt"`, for its own closed-loop fixture** --
   see "Measurements," above, for the full investigation. `"Corrupt"` itself is pinned byte-for-
   byte end to end by Test 1b (`demo_command`-based, not replayed). If the manager wants a replay
   test SPECIFICALLY exercising `"corrupt"` through a real downstream consumer, closing this gap
   would need either (a) a new closed-loop fixture whose consuming model degrades gracefully on a
   decode failure (the `ConstantAccelModel` pattern) rather than propagating one via `?` (the
   `AttitudeControllerModel`/`CommandedAttitude` pattern), or (b) narrowing `"corrupt"`'s own
   candidate-frame count enough (a short window covering few candidate frames) that a seed search
   can guarantee every draw over the WHOLE fault lifetime stays inside the user-data region, not
   merely the first draw. Flagging per this task's own standing instruction to escalate rather than
   guess when a decision is the manager's.
3. **`corrupt_mask`'s own declared shape (a single byte, XORed into every byte of the payload)
   was my own design choice**, among several the task's "a declared byte mask" wording could have
   meant (a per-byte-index mask; a byte range; a repeating multi-byte pattern). A single byte
   applied uniformly is the smallest shape that is still genuinely useful (breaks a checksum/CRC,
   corrupts identifying header fields, or perturbs a numeric field, depending where it lands) and
   needs no second declared parameter (a byte index) whose own out-of-range behavior would need a
   separate ruling. If a future DRM needs finer control (corrupt only byte N, or only the user-data
   region), that is a straightforward, backward-compatible extension (an optional
   `corrupt_byte_index`/`corrupt_byte_count` param) -- not attempted here, since nothing in this
   task's own scope needed it and inventing unused parameter surface is exactly what this crate's
   own "cheapest honest vehicle" convention warns against.
4. **`"duplicate"`'s own offset is always exactly the run's `output_period_ns`, not a declared
   parameter.** The task's own brief said "a second delivery one step later" without naming a
   configurable offset; `output_period_ns` is the one value this run already has that means "one
   step," so no new param was invented. If a future DRM wants a duplicate delivered N steps later
   (N != 1), that would need a declared `duplicate_offset_steps` (or similar) param -- not built,
   since nothing in this task's own scope asked for it.

## 6. What remains

- Item 2 above (a `"corrupt"`-through-replay closed-loop test) is the one deliberately deferred
  piece of this task's own scope, with the investigation already done and disclosed.
- SENSOR faults remain a typed refusal (`DrmError::PortOrSensorFaultNotYetSupported`) -- R4.2's own
  scope, untouched by this task, as instructed.
- `Fault.clear == true` on a PORT fault remains refused (`RouterError::PortFaultClearNotSupported`)
  -- R4.1a's own escalation 1, still open; this task did not touch `clear` semantics for any of the
  four kinds.
