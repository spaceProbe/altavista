# R6.2: the decode-error event shape (question 193)

Task: `docs/open-questions.md` question 193 (team 2, R5.2's own escalation, decided by the
lead R6.2): decode errors follow the fault-event shape questions 137/186(c) already established
for PORT/SENSOR faults -- one `EVENT_KIND_FAULT` event of kind `decode_error` at the first
undecodable frame per (instance, port), carrying the codec error text and the first sequence, a
matching end event when decoding resumes, and `values["frames_affected"]` on the end event.

Written incrementally, as instructed: hypotheses and expected values are stated in the source
(`crates/av-kernel/tests/decode_errors.rs`'s own module doc comment, the new DRM fixtures' own
header comments) before the corresponding measurement, not retrofitted.

## 1. What was built

### 1. The event shape (`crates/av-kernel/src/drm/events.rs`)

- `decode_error_event` (R5.2's per-occurrence builder) **deleted**, replaced by:
  - `DecodeErrorEpisode { port, first_tai_ns, first_sequence_count, first_error, frames_affected }`
    -- a small pub struct carrying everything an episode's own start/end events need.
  - `decode_error_start_event(episode, instance, provenance) -> Event` -- `name =
    "decode_error_start"`, `tai_ns = episode.first_tai_ns`, `values["sequence_count"]` when
    recoverable, `provenance.attributes["codec_error"]` the first frame's own error text.
  - `decode_error_end_event(episode, end_tai_ns, resumed, instance, provenance) -> Event` --
    `name = "decode_error_end"`, `values["frames_affected"]`, `detail` states "decoding resumed"
    when `resumed` or "decoding never resumed" otherwise (no extra `values` key for the
    distinction, per the task's own instruction). `id` is keyed off `episode.first_tai_ns` on
    BOTH the start and its own end (`decode_error_start:<instance>:<port>:<first_tai_ns>` /
    `decode_error_end:<instance>:<port>:<first_tai_ns>`), so a start and its own end share the
    pairing key derivable from either one, while never colliding (different `name` prefix).
  - **Naming**: `"decode_error_start"`/`"decode_error_end"`, matching `contact_event`'s own
    `"contact_start"`/`"contact_end"` convention exactly -- no better-justified alternative was
    found. `Event.kind` stays `EVENT_KIND_FAULT` for both (unlike `contact_event`, which gets its
    own dedicated `EventKind::ContactStart`/`ContactEnd` values) -- a decode error still has no
    single declared `Fault` behind it, so there is nothing new to add to `EventKind` for it, and
    reusing `EVENT_KIND_FAULT` matches every other fault-shaped event this module builds. This
    makes `Event.name` -- not `Event.kind` -- the field that discriminates a decode-error event
    from a plain DYNAMICS/PORT/SENSOR fault event, which is exactly why `web/js/timeline_events.
    js`'s new pairing function keys off `name`, not `type` (see item 5).
  - 7 new/updated unit tests (start carries first epoch/port/sequence/error; start omits sequence
    when unrecoverable; end-resumed carries the resumption epoch and count; end-not-resumed states
    "never resumed" in `detail` and invents no extra `values` key; two episodes on the same port
    get distinct ids).

### 2. The episode tracking (`crates/av-kernel/src/ports.rs`, `schedule.rs`, `kernel.rs`,
`drm/executor.rs`)

**What was considered for the "successful decode" signal, and why the chosen path (no new
`DynamicsModel` trait method)**: closing an episode needs to know when a consumer's NEXT decode
attempt on the same port succeeds -- `drain_decode_errors()` only ever reports failures. Three
designs were weighed:

1. **A new required `DynamicsModel::drain_decode_successes()` method**, mirroring
   `drain_decode_errors`'s own R5.2 precedent exactly (question 112's "no trait default that
   returns a valid empty result" rule). Rejected: `av_dynamics::erase::ErasedModel` (a single
   generic wrapper, `impl<M: DynamicsModel> DynamicsModel for ErasedModel<M>`, in
   `crates/av-dynamics/src/erase.rs`) explicitly hand-forwards every method to `self.inner` --
   confirmed by reading it (`fn drain_decode_errors(&self) -> ... { self.inner.
   drain_decode_errors() }`, one line per method, no macro/blanket delegation). Every model this
   crate constructs is wrapped in `ErasedModel` before it ever reaches a `BoxedModel`
   (`crate::registry::ModelHandle::into_boxed`), so a NEW trait method needs an explicit forward
   in `erase.rs` or it silently returns the trait's own default, never the wrapped model's real
   answer (question 112's exact historical bug, reproduced by construction if skipped). `erase.rs`
   is **not** in this round's file allowlist (only `crates/av-dynamics/src/lib.rs` is), and
   several OTHER files that implement `DynamicsModel` and would also need a trivial arm
   (`crates/av-kernel/src/drm/attitude.rs`, `sensors.rs`, `replay.rs`; `crates/av-dynamics/src/
   erase.rs`, `stm.rs`, `error.rs`; `crates/gmat-sys/src/model.rs`) are likewise outside it. A
   *defaulted* (non-required) new method would not fix this either: an `impl` block that does not
   mention a defaulted method inherits the TRAIT's own default, never delegates to the wrapped
   type -- Rust has no automatic forwarding for trait methods. Building this would have required
   editing files outside the allowlist; per the task's own explicit instruction ("stop and report
   rather than editing it"), this path was not taken.
2. **Independently re-decoding each delivered frame at the executor/schedule layer**, using the
   already-`pub` `codec::decode_packet`/`ApidMap`/`PacketCodec` types (no `codec.rs` edit needed,
   only calling its existing public API). Rejected as MORE invasive, not less: it would require
   duplicating each of the four real consumers' own "which port, which codec, decode the LAST
   message per port" logic outside the model (`controller.rs` alone has two different
   ApidMaps for two ports), a second, independently-maintained copy of exactly the decode
   selection logic `Inbox::last_on_port` already centralizes once -- a real risk of silently
   diverging from what the model itself actually does, for no benefit over option 3.
3. **Chosen: derive the signal in `crate::schedule::HeteroScheduler::advance_to_with_ports`,
   where `inbox` (the same `Inbox` the model was just handed) and this step's own freshly-drained
   `drain_decode_errors()` occurrences are ALREADY both in scope.** A port present in `inbox`
   whose name does NOT appear among this call's own failure occurrences is, by construction, a
   successful decode (every real consumer here either updates its cache or records exactly one
   failure per port per call -- the two are mutually exclusive and exhaustive for a real decode
   attempt). This needs zero new `DynamicsModel` methods and touches only files already in the
   allowlist (`ports.rs`, `schedule.rs`, `kernel.rs`, `executor.rs`) -- see `crate::ports::
   DecodeSuccessRecord`'s own doc comment for the full account. Cost, disclosed: a port this
   instance never actually decodes (a declared SIGNAL port, say) also passes this test, but
   harmlessly -- such a port can never have accumulated a `DecodeErrorRecord` either, so it can
   never have an open episode for this signal to close.

Built:
- `ports.rs`: `DecodeSuccessRecord { instance, port, tai_ns }`.
- `schedule.rs`: `HeteroSystemEntry.decode_successes`, derived right next to the existing
  `drain_decode_errors()` drain in `advance_to_with_ports` (dedup ports already checked this
  call, skip any port that just failed, otherwise record `inbox.last_on_port(port)`'s own
  `tai_ns`); `HeteroScheduler::decode_successes(id)` accessor mirroring `decode_errors`.
- `kernel.rs`: `HeteroKernel::decode_successes(id)` delegate.
- `drm/executor.rs`: `ModelSpanState.decode_successes`, drained in `run_one_span` exactly like
  `decode_errors`; `decode_error_episode_events(errors, successes, instance, run_end_tai_ns,
  provenance) -> Vec<Event>` -- groups both lists by port, merges into one chronologically-sorted
  `DecodeAttempt` sequence per port, walks it opening/closing episodes per question 193's own
  rule, and (item 3) closes any port still open once the sequence is exhausted at
  `run_end_tai_ns` with `resumed = false`. `run_shared_group`'s own per-instance tail replaces
  the old per-occurrence loop with one call to this function.

### 3. The run-end rule (item 3) -- **escalated for the lead to ratify, see section 5**

Implemented exactly as specified: an episode still open when a port's own attempt sequence is
exhausted gets its closing `decode_error_end` at `run_end_tai_ns`, `resumed = false`, `detail`
stating decoding never resumed, no extra `values` key. Mirrors `run_shared_group`'s own existing
precedent for a persistent SENSOR fault with no `Boundary::SensorFaultEnd` (that function's
"active_sensor_fault" tail loop, read before mirroring it, per the task's own instruction).

### 4. Tests re-derived (`crates/av-kernel/tests/decode_errors.rs`)

- Module doc comment gained an "R6.2" section stating the prediction (2 events, `frames_affected
  == 300`, start at t=5s, end at t=35s) BEFORE the corresponding measurement -- see section 3
  below for the measured numbers.
- Headline test rewritten to assert the new shape exactly (not loosened): 2 events, both epochs,
  `frames_affected`, `resumed` wording, unique ids.
- New fixture `drms/demo_attitude_control_port_corrupt_persistent.drm.yaml` (identical to the
  existing corrupt fixture, `duration_ns: 0` instead of a 30s window) plus new test
  `corrupt_startracker_persistent_fault_leaves_the_episode_open_at_run_end_with_the_real_count`
  -- the ONLY existing fixture never exercises the run-end/`resumed == false` path (its own
  window always closes before run end), so this is a genuinely new code path, not merely a
  restated assertion on already-covered behaviour.

### 5. The viewer (`web/js/timeline_events.js`)

- `pairEventWindows(events, {isStart, isEnd, keyFor})` -- the shared engine factored out of the
  pre-existing `pairContactWindows`, preserving its exact guarantees (chronological per key,
  nothing dropped, no cross-pairing) as a reusable primitive.
- `pairContactWindows` rewritten on top of it (byte-identical external behaviour restored after
  one deliberate regression, see section 4's break-and-restore log) -- its own `unmatched` reason
  text is post-processed back to the original contact-specific wording, since existing tests and
  callers match on the literal "contact_start"/"contact_end" substrings.
- `pairDecodeErrorWindows(events)` -- new, built on the same engine, keyed on `(spacecraft,
  referenceId)` (both real fields already on the wire -- no `detail`-parsing fallback needed,
  unlike `pairContactWindows`'s counterpart). **Matches on `ev.name`, not `ev.type`** -- decode-
  error events share `EVENT_KIND_FAULT`/`type: "fault"` with every other fault event, so `type`
  cannot discriminate them; `name` (`"decode_error_start"`/`"decode_error_end"`) is the only field
  that can.
- `timelineTickPlan` merges both pairing functions' `windows`/`unmatched` and excludes both from
  `points`.

### 6. The unreachable `frames_affected == 0` guard fixture (item 6)

New fixtures `drms/demo_sensor_fault_no_truth.{sos,drm}.yaml` (reusing the existing
`demo_attitude_sensors_truth`/`_startracker` systems unchanged) plus
`crates/av-kernel/tests/sensor_faults.rs::
a_sensor_fault_whose_star_tracker_has_no_truth_connection_never_emits_an_event`. **No change to
`executor.rs`'s guard was needed or made** -- the existing `Boundary::SensorFaultEnd` guard
already correctly emits nothing when `sensor_fault_totals` has no entry; this item only needed a
fixture and a test proving the guard's `None` branch is REACHABLE, per R5.1's own "recorded as
exercised only indirectly" finding.

## 2. Verification

`export PATH="/opt/homebrew/opt/rustup/bin:$PATH"` first. Contention checked
(`ps -Ao pid,etime,command | grep -E "cargo test|cargo build"`) before every cargo run; two waits
were needed for another worker's own `cargo test` invocations (`cargo test -p av-lockstep --lib
-- docker::`, then later `cargo test --workspace --exclude av-kernel`) -- waited via the standing
blocking-loop pattern both times, until clear. Full output of every run below is saved under
`/private/tmp/claude-501/-Users-probe-code-AltaVista/ed238b37-d349-42f1-956b-22807101027f/scratchpad/r6_2/`.

### Rust (`crates/av-kernel`)

- `cargo check -p av-kernel --lib` -- clean, 1m56s (`check1.txt`).
- `cargo check -p av-kernel --lib --tests` -- clean.
- `cargo test -p av-kernel --lib` -- **667 passed, 0 failed**, run twice: once mid-task
  (`test_lib_full1.txt`) and once more as the final run after every break-and-restore below was
  reverted (`test_lib_full_final_667.txt`) -- identical result both times.
- `cargo test -p av-kernel --lib drm::events::` -- **23 passed, 0 failed** (`test_events1.txt`) --
  includes the 7 new/rewritten `decode_error_start_event`/`decode_error_end_event` unit tests.
- `cargo test -p av-kernel --lib schedule::` -- **17 passed, 0 failed** (`test_schedule1.txt`).
- `cargo test -p av-kernel --lib kernel::` -- **19 passed, 0 failed**.
- `cargo test -p av-kernel --test decode_errors` -- run three times: first with 4 tests right
  after the shape landed (`decode_errors_test1.txt`, the pre-existing headline/router-frames-
  affected/divergence/determinism tests re-derived for the new shape), then **5 passed, 0
  failed** after adding the persistent-fault fixture and test (`decode_errors_test2.txt`), then
  the same 5/5 as the final post-restore run (`decode_errors_final.txt`).
- `cargo test -p av-kernel --test sensor_faults` -- **12 passed, 0 failed**, run twice
  (`sensor_faults1.txt`, then the final post-restore run `sensor_faults_restored.txt`) --
  includes the new `a_sensor_fault_whose_star_tracker_has_no_truth_connection_never_emits_an_
  event`.
- `cargo test -p av-kernel --test drm_attitude_control` -- **9 passed, 0 failed**
  (`drm_attitude_control1.txt`, unaffected by this round's changes, run as a sanity check on the
  shared `executor.rs` edits).
- `cargo clippy -p av-kernel --all-targets -- -D warnings` -- **clean** (one real finding fixed
  along the way, see "Defects," item 1; `clippy1.txt` has the pre-fix finding, `clippy2_clean.txt`
  the clean rerun after the fix -- `Finished` with no error/warning lines).

### Viewer (`web/js`, `tests/test_viewer_timeline.py`)

- `node --check web/js/timeline_events.js` / `timeline_check.mjs` -- clean.
- `.venv/bin/python -m pytest -q tests/test_viewer_timeline.py -s` -- **10 passed** (final run),
  `timeline_check.mjs` itself reporting **55/55 named checks passing** -- 2 new event-reaches-
  the-viewer checks (section 1), a new "2b. pairDecodeErrorWindows" block (11 checks, mirroring
  section 2's own contact-pairing checks including its own TRAP case), and 3 new
  `timelineTickPlan` checks (section 4); see the printed table in `test_timeline_check_report`'s
  own `-s` output for the full list.

### Break-and-restore evidence (this crate's own "every new test must fail against a nameable
wrong implementation, mechanically executed" rule)

4 wrong implementations were built, run against the relevant real test(s), confirmed to fail with
the panic/failure text below, then reverted; `git diff` confirmed every touched file byte-
identical to its pre-break state after every restore (`git diff <file> | grep DELIBERATE` empty
in every case).

1. **`executor.rs::decode_error_episode_events`: never folds consecutive failures, one trivial
   episode per occurrence** (reverts to the R5.2 per-occurrence shape in substance).
   `tests/decode_errors.rs::corrupt_startracker_run_completes_with_exactly_the_predicted_decode_
   error_event_count` -- `assertion left == right failed ... left: 600 right: 2` (`break1.txt`).
2. **`executor.rs::decode_error_episode_events`: the success signal is never fed in** (`let _ =
   successes;`) -- every episode stays open forever, closing only via the run-end fallback.
   Same test -- `assertion left == right failed: decode_error_end must land at the fault
   window's own declared (half-open) end, t=35s ... left: 1767225937000000000 (run end) right:
   1767225672000000000 (t=35s)` (`break2.txt`).
3. **`web/js/timeline_events.js::pairDecodeErrorWindows`: keys pairing on spacecraft alone,
   ignoring the port** -- `tests/test_viewer_timeline.py::
   test_decode_error_windows_paired_correctly_never_cross_paired` fails: 6 of
   `pairDecodeErrorWindows`'s own checks fail, including the TRAP check itself (window end
   cross-paired with the wrong port's orphan end) (`break3_js_trap.txt`).
4. **`executor.rs`'s `Boundary::SensorFaultEnd` guard bypassed entirely** (unconditionally
   emits, defaulting to `frames_affected: 0` when nothing was ever drained) --
   `tests/sensor_faults.rs::a_sensor_fault_whose_star_tracker_has_no_truth_connection_never_
   emits_an_event` fails: a fabricated `Event` with `values.frames_affected == 0.0` and `detail`
   claiming "affecting 0 emission(s)" is asserted absent, and is not (`break3.txt`/
   `break3_raw.txt`, the guard-bypass break -- unrelated to the decode-error item 3 above despite
   the shared file-naming coincidence).

Not independently mechanically executed (disclosed): the `schedule.rs`/`kernel.rs`/`ports.rs`
plumbing that carries `DecodeSuccessRecord` from `Inbox` to `HeteroKernel::decode_successes` --
a break there (e.g. never populating `sys.decode_successes`) is the IDENTICAL observable shape to
break 2 above (the episode never closes naturally), already executed and restored; re-breaking
each intermediate hop separately would reproduce the same panic text for no new information. The
Python-side `_decode_error_event`/fixture wiring in `test_viewer_timeline.py` is exercised
directly by every JS check passing/failing correctly (breaks 3's own failure list already proves
the fixture data reaches the checker correctly shaped), so it was not separately broken.

## 3. Measurements worth keeping

**The decode-error episode count and `frames_affected`, predicted before running (this file's
citation: `tests/decode_errors.rs`'s own module doc comment, "R6.2" section).** Against
`drms/demo_attitude_control_port_corrupt.drm.yaml` (unchanged from R5.2): predicted exactly 2
events (1 start + 1 end), `decode_error_start.tai_ns == FAULT_START_TAI_NS` (t=5s),
`decode_error_end.tai_ns == FAULT_END_TAI_NS` (t=35s), `frames_affected == 300.0`. **Measured:
matches every one of those four predictions exactly, on the first real run** -- no adjustment
needed. This is the direct proof the R6.2 shape actually bounds the event count: R5.2's own
identical fixture produced 300 events; this shape produces 2, for the identical underlying 300
failed decode attempts.

**The persistent (run-end) case, predicted before running (`drms/
demo_attitude_control_port_corrupt_persistent.drm.yaml`'s own header comment).** A `duration_ns:
0` corrupt fault, otherwise identical: predicted 2 events, `decode_error_start.tai_ns == 5s`
(unchanged from the bounded fixture -- the first real attempt is identical either way),
`decode_error_end.tai_ns == 300s` (the run's own end, since nothing ever closes the episode
naturally), `frames_affected == 2950.0` ((300 - 5) / 0.1). **Measured: matches exactly.** This is
the one code path (item 3, the run-end rule) no existing R5.2 fixture ever exercised -- confirmed
now genuinely reachable and correct, not merely reasoned about.

**Internal accumulator size is not the bottleneck the original R5.2 escalation worried about.**
The persistent fixture's own `ModelSpanState::decode_errors` grows to ~2950 entries during the
run (bounded by run length, same order of magnitude as the trajectory's own sample count) before
being folded into exactly 2 events -- the run completed in the same ~34s as the bounded fixture,
confirming the unboundedness R5.2 flagged was specifically about `RunProducts.events` (a
persisted, transmitted artifact), never about an internal, transient working list.

## 4. Defects found, including my own

1. **My own bug, caught by `cargo clippy`, not shipped:** the new
   `decode_error_start_event_carries_the_first_epoch_port_sequence_and_error_text` unit test used
   `e.values.get("frames_affected").is_none()` where `!e.values.contains_key("frames_affected")`
   is what clippy's `unnecessary_get_then_check` lint wants -- a style-only finding (identical
   truth value either way), fixed immediately, re-verified clean by a clippy rerun.
2. No defects were found in the delivered implementation itself beyond item 1 above -- every
   break-and-restore case in section 2 confirms the real code behaves as documented once the
   deliberate bug is reverted, and the persistent-fault and no-truth-connection fixtures both
   matched their own stated predictions exactly on the first real run.
3. **A genuine, disclosed gap found while designing the episode-tracking signal (not a bug in
   what shipped, a documented limitation of the CHOSEN design):** the derived "successful decode"
   signal (section 1, item 2) treats every port present in a step's own `Inbox` as a decode
   attempt, without distinguishing a real FRAMED-codec port from a plain SIGNAL port. This is
   provably harmless today (a SIGNAL port can never have accumulated a `DecodeErrorRecord`, so it
   can never have an open episode to spuriously close) but is a structural assumption, not an
   enforced invariant -- a future FRAMED port that is declared but genuinely never decoded by any
   model logic (unlikely, but not impossible) would silently generate unused, harmless
   `DecodeSuccessRecord` entries rather than a typed refusal. Recorded here per this crate's own
   "disclose, don't hide" culture; not fixed, because fixing it would require exactly the kind of
   model-side codec knowledge at the schedule.rs layer that was the whole reason option 2
   (independent re-decode) was rejected as MORE invasive in section 1.

## 5. Escalations for the manager

1. **The run-end rule (item 3) is the manager's own decision to ratify, not question 193's own
   wording.** Implemented exactly as specified in the brief (mirrors `run_shared_group`'s own
   persistent-SENSOR-fault precedent): an episode still open at run end gets its own
   `decode_error_end` at `run_end_tai_ns`, `resumed = false`, `detail` stating decoding never
   resumed, no extra `values` key. Flagging per the brief's own explicit instruction to do so.
2. **The "no new `DynamicsModel` trait method" design choice (section 1, item 2) is a judgment
   call made under this round's own file-allowlist constraint, not a claim that it is the only
   correct design in general.** If a future round's allowlist includes `crates/av-dynamics/src/
   erase.rs` and the other non-allowlisted `DynamicsModel` implementors, the "new required trait
   method, delegated explicitly through `ErasedModel`/`AnyModel`" design (the one the brief itself
   anticipated) becomes buildable and would be a more locally-obvious signal path than the
   derived-from-`Inbox` approach shipped here -- flagging so this is a deliberate, disclosed
   trade-off under this round's constraints, not an oversight.
3. **Defects, item 3** (the SIGNAL-vs-FRAMED-port ambiguity in the derived success signal) is
   disclosed there in full; not escalated as a decision needed now, since it is provably inert
   today, but worth the lead's awareness if a future port kind changes that invariant.

## 6. What remains

- Everything this task's brief asked for (items 1-6) is built, tested, and verified against its
  own stated predictions.
- Escalation 1 above (run-end rule ratification) is the one open decision for the lead.
- Section 4's own disclosed, not-independently-broken items (the `schedule.rs`/`kernel.rs`
  plumbing hops, the Python fixture wiring) are the only "not mechanically executed" gaps, both
  with a stated reason.
- Not touched, out of this round's scope: `web/js/app.js`'s own DOM rendering loop
  (`buildTicks`) is NOT in this round's file allowlist, so a decode-error window currently rides
  through `plan.windows` alongside a contact window but would render with the SAME
  `tick-contact-window` CSS class and a tooltip built from `w.counterpart` (which a decode-error
  window entry does not have, `w.port` instead) -- the pure data layer (`timelineTickPlan`) is
  correct and fully tested, but a small `app.js` change (reading `w.kind` and branching the
  class/tooltip) is needed before a decode-error episode reads visually distinct from a contact
  window in the actual browser UI. Recorded here rather than worked around by writing into a file
  outside this round's ownership.
