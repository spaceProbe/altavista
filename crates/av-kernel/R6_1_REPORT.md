# R6.1: per-message emission epochs in the router (question 189)

Sections per the task brief: 1. What was built, 2. Verification, 3. Measurements, 4. Defects
found, 5. Escalations, 6. What remains.

## 1. What was built

### `crates/av-kernel/src/router.rs`

- New module doc section **"Per-message emission epochs (question 189, R6.1)"**, inserted right
  after the "Link model" section and before "Port traffic recording" -- states the decision, lists
  all six call sites that changed, justifies the `NO_MESSAGE_EPOCH` sentinel, and notes the
  consequences for question 181's sidecar order and for replay.
- New `pub const NO_MESSAGE_EPOCH: i64 = 0;`, with its own doc comment, next to
  `LATENCY_LINK_MODEL`.
- `Router::deliver`'s parameter renamed `emission_tai_ns` -> `fallback_emission_tai_ns` (a pure
  rename; every call site is positional, so no caller needed a signature-visible change). Inside
  the per-message loop, one new line:
  ```rust
  let msg_epoch = if message.tai_ns == NO_MESSAGE_EPOCH { fallback_emission_tai_ns } else { message.tai_ns };
  ```
  and all six existing uses of the old `emission_tai_ns` (OUT `PortTrafficRecord.tai_ns`, the
  PORT-fault window comparison, `InstalledPortFault::first_applied_tai_ns`, the IN
  `PortTrafficRecord.tai_ns` -- ordinary and duplicate-fault copy, the delivered
  `PortMessage.tai_ns` -- `msg_epoch + edge.latency_ns + extra_delay_ns`/`+ offset_ns`, and
  `QueuedMessage.sender_emission_tai_ns` -- ordinary and duplicate copy) now read `msg_epoch`.
  `QueuedMessage.sender_emission_tai_ns` was not named explicitly in the manager's six-item list
  but is included deliberately: it feeds `crate::ports::sorted_inbox`'s own question-108 sort key,
  and leaving it at the coalesced fallback while every other field switched to `msg_epoch` would
  have silently reintroduced the coarsening one field later -- pinned by
  `queued_message_sender_emission_epoch_reflects_each_messages_own_due_epoch_for_sort_order`.
- `Router::deliver`'s own doc comment and the module doc's "Duplicate"/`PortFaultEffect` doc
  comments updated to say `msg_epoch` instead of the old `emission_tai_ns` wherever they described
  what gets recorded/compared.
- Five new unit tests in `router::tests` (all pass; break-and-restore evidence in section 2):
  - `a_message_carrying_the_sentinel_falls_back_to_the_callers_own_epoch`
  - `a_message_carrying_its_own_epoch_uses_it_not_the_callers_fallback_epoch`
  - `two_messages_in_one_deliver_call_carry_distinct_sub_step_epochs_not_coalesced`
  - `a_port_faults_window_matches_by_each_messages_own_epoch_not_the_callers_coarse_one`
  - `queued_message_sender_emission_epoch_reflects_each_messages_own_due_epoch_for_sort_order`

### Empirical check of the `NO_MESSAGE_EPOCH = 0` sentinel

- Every `drms/*.drm.yaml`'s own `Scenario.start_tai_ns` was read directly (`grep -rn
  "start_tai_ns" drms/*.drm.yaml`): every one is a large positive number (e.g.
  `1_700_000_000_000_000_000`, `1_767_225_637_000_000_000`), decades after TAI epoch 0. None is 0.
- Every `av_dynamics::Outbox::push`/`push_signal` call site in the workspace was located
  (`crates/av-kernel/src/drm/{controller,sensors,ground,gmat_command,binding,replay}.rs`,
  `executor.rs`'s command dispatch) and read: every one computes a real epoch (`due`,
  `result.t_tai_ns`, `cmd.tai_ns`, `m.tai_ns` forwarded from a container response, `mat.t0_tai_ns`)
  -- none passes a literal `0`.
- Grepped the whole workspace (`crates/av-kernel/{src,tests}`) for a literal `0` `tai_ns` argument
  to `Outbox::push`/`push_signal` -- zero matches.
- `crates/av-kernel/src/drm/controller.rs`'s own unit tests DO construct an `av_dynamics::
  PortMessage { tai_ns: 0, .. }` directly, but only as an INBOUND `Inbox` fixture fed straight to
  `step_with_ports` in a unit test -- never through `Outbox`/`Router::deliver` -- so it does not
  touch this sentinel at all.
- Conclusion: `0` is safe as the "no epoch of its own" sentinel for every real producer today; the
  fallback path is exercised only by a test that deliberately constructs a sentinel message.

### `crates/av-kernel/src/drm/replay.rs`

- Module doc's "The missing-frame rule" section rewritten: `ReplayModel::step_with_ports` now
  gathers every recorded epoch in `(t_tai_ns, t_tai_ns + dt_ns]` (a window), not a single exact
  match at `t_tai_ns + dt_ns`. Explains why `t_tai_ns` alone (no separate watermark) is the correct
  lower bound: `HeteroScheduler::advance_to_with_ports` steps one system contiguously at its own
  registered `period_ns` (verified by reading `t_ns == sys.next_due_ns` / `sys.next_due_ns +=
  sys.period_ns`, not assumed), and a replayed instance is driven by the identical registered
  period the original model was, so this call's own `t_tai_ns` is always the previous call's own
  end.
- New, explicitly disclosed limitation (item 2 under "Honest limits of this rule"): when one
  call's window legitimately covers more than one recorded due epoch (the sub-step case),
  deleting only SOME of them leaves the window non-empty, so `MissingFrame` does not fire -- this
  rule still only detects a call's window turning up completely EMPTY, the same granularity it
  always had; R6.1 just makes a new, previously-impossible partial-loss shape possible (a call's
  frames are no longer atomic). Pinned by a new unit test (below), not merely stated in prose.
- `ReplayModel::step_with_ports`: replaced `self.frames_by_epoch.get(&emission_tai_ns)` with
  `self.frames_by_epoch.range((Bound::Excluded(t_tai_ns), Bound::Included(end)))`, collecting every
  due epoch in ascending order and pushing each at its own real epoch (not `end`). The
  interior-gap check generalizes from `emission_tai_ns > first && emission_tai_ns < last` (a
  point) to `t_tai_ns < last && end > first` (a window), with the reported `tai_ns` now the
  window's own `end`.
- `ReplayModel::frames_by_epoch`'s own field doc comment updated to note a `tai_ns` key is now
  each message's own due epoch, and one call's window can cover more than one key.
- Two new unit tests in `drm::replay::tests` (both pass; break-and-restore evidence in section 2):
  - `a_calls_window_covering_two_recorded_epochs_replays_both_at_their_own_due_epochs`
  - `deleting_only_one_of_two_recorded_sub_step_epochs_in_one_calls_window_is_not_detected` (pins
    the disclosed limitation directly, not just in prose)

### `crates/av-kernel/tests/replay.rs` (T3 re-derived, not loosened)

`t3_one_deleted_interior_record_is_a_typed_missing_frame_error_naming_the_instance_and_epoch`'s
own doc comment and body rewritten for the new one-record-per-due-epoch reality:

- **Old shape (pre-R6.1):** startracker's 2 Hz rate under this DRM's 1 Hz kernel step meant every
  kernel tick's TWO emissions shared ONE coalesced `tai_ns`, so the test deleted both records at
  that one shared epoch.
- **New shape, measured (not assumed):** every due epoch now carries exactly ONE record (pinned by
  a new sanity `assert!`). Deleting a single due epoch's record does NOT open a detectable gap
  (its sibling due epoch, 0.5 s away in the SAME kernel tick, still survives inside that tick's own
  `(previous end, this end]` window) -- this was actually measured, not merely reasoned: the
  first rewritten version of this test picked an arbitrary "0.5 s apart" pair, which turned out to
  straddle a TICK BOUNDARY (a cross-tick pair, not a within-tick one) and the replay run completed
  with `Ok`, not the expected error. Fixed by disambiguating with the DRM's own declared
  `start_tai_ns` and 1 Hz kernel period directly: a within-tick pair's LATER member always lands
  exactly on the kernel's own 1 s grid; a cross-tick pair's does not. The test now finds a genuine
  within-tick interior pair this way, deletes both of ITS due epochs, and asserts the resulting
  error names that tick's own end epoch.

### `crates/av-kernel/tests/sensor_faults.rs` (the "real test" for item 5, inverted)

`measured_ccsds_sequence_restarts_at_zero_at_each_rematerialization_boundary_and_the_sidecars_own_epoch_coalesces_multiple_emissions_per_kernel_step`
renamed to
`..._and_the_sidecars_own_epoch_now_resolves_each_sub_step_emission_distinctly` and its own
"epoch-coalescing" doc-comment + final assertions inverted: the first two post-fault-end
star-tracker OUT records, measured, now carry `FAULT_END_TAI_NS + 50_000_000` (`sequence_count =
0`) and `FAULT_END_TAI_NS + 100_000_000` (`sequence_count = 1`) -- two DISTINCT epochs, 0.05 s (one
star-tracker period) apart, matching `StarTrackerModel::new`'s own documented `next_due = boundary
+ period_ns` seeding exactly, and an explicit `assert_ne!` that they no longer share one epoch.
This is exactly the R5.1a measurement ("both share one `tai_ns`, distinguished only by
`sequence_count` 0 and 1") inverted, as the task instructed.

## 2. Verification (exact counts)

All logs under
`/private/tmp/claude-501/-Users-probe-code-AltaVista/ed238b37-d349-42f1-956b-22807101027f/scratchpad/r6_1/`.

| Command | Result | Log |
|---|---|---|
| `cargo build -p av-kernel --lib` | clean, 0 warnings | `build_1.log` |
| `cargo test -p av-kernel --lib router::` | **44 passed, 0 failed** (39 pre-existing + 5 new) | `router_lib_test_1.log` |
| `cargo test -p av-kernel --lib drm::replay::` | **11 passed, 0 failed** (9 pre-existing + 2 new) | `replay_lib_test_2.log` |
| `cargo test -p av-kernel --test replay` (T1-T6b) | **10 passed, 0 failed** | `replay_integration_2.log`, `replay_integration_restored_summary.log` |
| `cargo test -p av-kernel --test sensor_faults` | **11 passed, 0 failed** | `sensor_faults_full_1.log` |
| `cargo test -p av-kernel --test port_traffic_sidecar` | **4 passed, 0 failed**, unmodified | `port_traffic_sidecar_1.log` |
| `cargo test -p av-kernel --test port_faults` | **14 passed, 0 failed**, unmodified | `port_faults_1.log` |
| `cargo test -p av-kernel --test ports_router` | **9 passed, 0 failed**, unmodified | `ports_router_1.log` |
| `cargo test -p av-kernel --test drm_attitude_control` | **9 passed, 0 failed**, unmodified | `drm_attitude_control_1.log` |
| `cargo test -p av-kernel --test decode_errors` (forbidden to edit; read-only risk check) | **4 passed, 0 failed**, unmodified | `decode_errors_1.log` |
| `cargo test -p av-kernel --lib drm::executor::sort_port_traffic` | **6 passed, 0 failed**, unmodified (question 181's order is untouched code) | `executor_sort_test_1.log` |
| `cargo test -p av-kernel --test expr_goldens` | **4 passed, 0 failed** -- no golden drifted | `expr_goldens_1.log` |
| `cargo test -p av-kernel --test golden_acceptance` | **3 passed, 0 failed** -- no golden drifted | `golden_acceptance_1.log` |
| `cargo clippy -p av-kernel --all-targets -- -D warnings` | clean, 0 warnings | `clippy_1.log` |

**Every pre-existing test in every one of these files passed completely unmodified.** For the
files I did not need to touch (`ports.rs`, `executor.rs`, `port_traffic_sidecar.rs`,
`port_faults.rs`, `ports_router.rs`, `drm_attitude_control.rs`, `decode_errors.rs`,
`expr_goldens.rs`, `golden_acceptance.rs`) this is exactly what was predicted going in: none of
them exercise a native model that emits more than once per kernel step while also writing a
sidecar/asserting an exact epoch, so `msg_epoch` resolves identically to the old coalesced
`emission_tai_ns` for every message they push (see section 3 for why).

### Break-and-restore, executed (not reasoned about)

**1. `router.rs`'s core resolution line.** Temporarily replaced
`let msg_epoch = if message.tai_ns == NO_MESSAGE_EPOCH { fallback_emission_tai_ns } else { message.tai_ns };`
with `let msg_epoch = fallback_emission_tai_ns;` (the exact pre-R6.1 "coalesce everything onto the
caller's one epoch" shape).

- `cargo test -p av-kernel --lib router::`: **4 of the 5 new tests failed** (log:
  `break_restore_1.log`); the 5th (`a_message_carrying_the_sentinel_falls_back_to_the_callers_own_epoch`)
  still passed -- disclosed, not hidden: it only ever pushes a sentinel message, so it cannot by
  itself distinguish "fall back only for the sentinel" from "always fall back"; the other four
  close that gap. Real panic text:
  ```
  a_message_carrying_its_own_epoch_uses_it_not_the_callers_fallback_epoch:
    must use the message's own epoch, not the caller's fallback 9_000: [...tai_ns: 9000...]
  a_port_faults_window_matches_by_each_messages_own_epoch_not_the_callers_coarse_one:
    assertion `left == right` failed: []  left: 0  right: 1
  queued_message_sender_emission_epoch_reflects_each_messages_own_due_epoch_for_sort_order:
    sorted_inbox must order by each message's own true due epoch (1_000 before 2_000), not by
    push order: [[178], [161]]  left: [[178], [161]]  right: [[161], [178]]
  two_messages_in_one_deliver_call_carry_distinct_sub_step_epochs_not_coalesced:
    the two OUT records must carry their own distinct due epochs, not both coalesced to 2_000
    left: [2000, 2000]  right: [1950, 2000]
  ```
- Same break also run against `cargo test -p av-kernel --test sensor_faults measured_ccsds` (the
  inverted DRM-level test): **failed** with
  `left: 1767225672100000000  right: 1767225672050000000` (log: `sensor_faults_break_restore.log`)
  -- the two post-fault records collapsed back to one shared, coarser epoch, exactly the pre-R6.1
  measurement.
- Restored; re-ran both: **44/44** and **11/11** pass again (logs: `router_lib_test_restored.log`,
  `sensor_faults_full_1.log`). The break touched only that one line in each run, in place; `git
  status --short` after every restore showed the same file set as before the break (no stray diff
  left behind by the break/restore cycle itself -- the files' overall diffs are this task's own
  legitimate, still-uncommitted feature changes, not break-and-restore residue).

**2. `replay.rs`'s window query.** Temporarily replaced
`self.frames_by_epoch.range((Bound::Excluded(t_tai_ns), Bound::Included(end))).map(|(epoch, _)| *epoch).collect()`
with `self.frames_by_epoch.get(&end).map(|_| vec![end]).unwrap_or_default()` (the exact pre-R6.1
"exact match at `end` only" shape).

- `cargo test -p av-kernel --lib drm::replay::`: **1 of 2 new tests failed** (log:
  `replay_break_restore_1.log`) --
  `a_calls_window_covering_two_recorded_epochs_replays_both_at_their_own_due_epochs` panicked with
  `left: 1  right: 2` (only the exact-match frame replayed, the sub-step one silently dropped);
  `deleting_only_one_of_two_...` still passed, correctly -- it asserts a NON-detection, which the
  old exact-match code also produces (for a different reason: it only ever looked at `end`
  anyway), so it cannot discriminate the two implementations by itself either.
- `cargo test -p av-kernel --test replay`: **T1 failed** (huge byte-level `RunProducts` diff,
  log preview captured in
  `/Users/probe/.claude/.../tool-results/bo4n2is6p.txt`, first ~2 KB retained) -- proves the
  sub-step-dropping bug really does propagate to a full DRM run's own `RunProducts`, not just the
  unit-level `Outbox`.
- Restored; re-ran: **11/11** (lib) and **10/10** (integration) pass again (logs:
  `replay_lib_test_restored.log`, `replay_integration_restored_summary.log`).
- **T3 was NOT independently break-and-restore-able against this one line alone** -- disclosed,
  not hidden: T3 deletes BOTH members of a within-tick pair, including the tick's own boundary
  epoch (`end`), which the OLD exact-match code also checks (and finds missing) regardless of the
  window mechanism. Re-ran `t3_...` against the broken line and it still passed (log:
  `replay_t3_break_restore.log`) -- T3 is real, useful integration coverage of "the missing-frame
  rule still functions at the new granularity," but the two `drm::replay::tests` unit tests above
  are what actually discriminate the windowing mechanism itself; T3 does not duplicate that
  specific claim and is not represented as doing so.

## 3. Measurements worth keeping

**Hypothesis, stated before running (per the task brief):** making the recorded/available epoch
finer can only ever make it EARLIER within the emitting instance's own kernel step, never later.
For a receiver whose own step cadence is NO FINER than that kernel step (every receiver in every
fixture this task's scope reaches -- the attitude controller in `demo_attitude_control*` steps at
10 Hz, exactly matching the kernel; nothing in `demo_attitude_sensors*`/`demo_command*` has any
receiver stepping faster than its own kernel step at all), a message that becomes available
EARLIER than before still cannot be seen any earlier than the receiver's own NEXT scheduled step,
which is unchanged -- so **no DRM's scores, trajectories, or golden hashes should change**, only
`port_traffic_hash` (the sidecar records the finer epochs directly) should. Measured, not assumed:

- **`port_traffic_hash` changes** for any DRM/run that (a) writes a sidecar (`products_dir:
  Some`) and (b) has an instance emitting more than once per kernel step on a FRAMED/BYTE_STREAM
  port -- concretely, `drms/demo_attitude_control*.drm.yaml` (star tracker/IMU at 20 Hz under a
  10 Hz kernel) and `drms/demo_attitude_sensors.drm.yaml` (2 Hz under 1 Hz). No test in this repo
  pins a literal `port_traffic_hash` STRING as a golden value (checked directly: `grep -rn
  'port_traffic_hash, "' crates/av-kernel/tests/*.rs` finds only the trivial `products.
  port_traffic_hash, ""` no-sidecar case) -- so there is no golden `port_traffic_hash` to
  regenerate or flag. `drms/demo_command*.drm.yaml`'s own hash is UNCHANGED (confirmed:
  `port_traffic_sidecar.rs`'s Test A -- the question-181 acceptance test -- passed byte-for-byte
  unmodified), because `demo_command`'s only two emitters (`ground`'s dispatched command,
  `flight`'s ack) each push exactly one message per `deliver` call, at exactly the epoch already
  passed to `deliver` -- there is no divergence between `message.tai_ns` and the caller's fallback
  for this fixture, so `msg_epoch` resolves identically before and after this change.
- **No golden hash changed.** `golden_acceptance.rs` (`kernel_at_10hz_matches_the_golden_arc`,
  `kernel_covariance_matches_the_golden_stm_and_propagated_cov`,
  `kernel_covariance_at_a_coarser_instance_period_matches_the_fine_one_at_shared_epochs`) and
  `expr_goldens.rs` (`straight_accel`/`fault_split_accel`/`range_duration`) all passed unmodified.
  Root cause, confirmed by reading the code, not merely by the tests passing: `golden_acceptance.
  rs` drives `av_kernel::Kernel` directly (pure GMAT propagation, no `HeteroScheduler`/`Router`/
  ports at all), and `expr_goldens.rs`'s own DRM cases declare zero `Connection`s (`grep -n
  "Connection" crates/av-kernel/tests/expr_goldens.rs` -> no matches) -- neither test path ever
  calls `Router::deliver`, so this change cannot touch either.
- **No score or trajectory changed for any DRM this worker's scope reaches.** Root-caused, with
  the specific delivery named, per the task's own instruction (not "it changed because the epochs
  are finer"): `demo_attitude_control*`'s ONLY consumer of the star tracker's/IMU's FRAMED output
  is `controller` (`startracker.st_meas -> controller.startracker_in`,
  `imu.imu_meas -> controller.imu_in`, both `link_model: ""`, zero declared latency), and
  `controller` is registered at `update_rate_hz = 10.0` -- IDENTICAL to this DRM's own
  `default_step_rate_hz = 10.0` kernel step. `Router::take_inbox` is only ever called by
  `controller`'s own next scheduled step, which lands only on 10 Hz-grid epochs (100 ms
  multiples); a star-tracker/IMU sub-step message's new, finer availability (e.g.
  `emission + 50_000_000` instead of the old, coarser `emission + 100_000_000`) is still `<=` that
  same next 10 Hz-grid epoch either way (both `<=` the step boundary that already bounded the OLD
  coalesced epoch), so it becomes visible to `controller` at the identical step as before -- and
  `crate::ports::sorted_inbox`'s own "last message on this port wins" rule
  (`av_dynamics::Inbox::last_on_port`) means `controller` consumes the identical (LATEST,
  chronologically-last) measurement either way, whether the two sub-step siblings shared one
  epoch or carry two distinct ones. Measured, not merely reasoned: `drm_attitude_control.rs`'s own
  score/trajectory-checking tests (`the_final_pointing_error_is_meaningfully_smaller_...`,
  `the_measured_pointing_error_tracks_the_closed_form_decay_...`,
  `two_runs_of_the_control_drm_with_the_same_seed_are_byte_identical_...`, `the_wheel_never_
  approaches_its_declared_saturation_limit`, `the_error_never_overshoots_...`) all passed
  unmodified. `sensor_faults.rs`'s `dropout_fault_event_frames_affected_matches_the_unfaulted_
  baselines_own_sidecar_count` and `decode_errors.rs`'s `corrupt_startracker_router_level_fault_
  event_frames_affected_is_600` passing unmodified additionally confirms the SAME reasoning holds
  for a PORT-fault window whose boundaries land exactly on the kernel's own 100 ms grid (see next
  bullet).
- **A PORT-fault window's own frame count can shift at the boundary tick, but for these two
  specific fixtures nets to the identical total -- worked by hand, then confirmed by the
  unmodified tests.** `demo_attitude_control_port_corrupt.drm.yaml`'s fault window is `[5s, 35s)`,
  and `demo_attitude_control_port_duplicate.drm.yaml`'s is persistent from `start_tai_ns` --
  both boundaries land exactly on the 100 ms KERNEL grid (not merely the 50 ms sensor sub-step
  grid). At the FIRST affected kernel tick (spanning `(4.9s, 5.0s]`, due epochs `4.95s`/`5.00s`):
  pre-R6.1, BOTH sub-step messages were classified using the coarse `emission_tai_ns = 5.0s`
  (`>= window start 5.0s`), so BOTH were treated as inside the window -- a false positive for the
  `4.95s` one. Post-R6.1, `4.95s` is correctly excluded (before the window) and `5.00s` is
  correctly included. Symmetrically, at the LAST affected tick (`(34.9s, 35.0s]`, due epochs
  `34.95s`/`35.00s`): pre-R6.1, BOTH were excluded (coarse `emission_tai_ns = 35.0s >=` window end
  35.0s), a false NEGATIVE for the truly-in-window `34.95s`. These two boundary errors are
  opposite in sign and each off by exactly one candidate frame, so the TOTAL count (600 = 30 s *
  20 Hz) is identical either way -- but WHICH specific due epoch is the one that changed status
  did move. This never becomes observable in `decode_errors.rs`/`sensor_faults.rs` because the
  ONLY thing any receiver (`controller`, at 10 Hz) ever reads is `last_on_port`'s pick -- the
  chronologically LATEST message on that port at each of ITS OWN 10 Hz steps -- and the boundary
  message whose fault status flipped (`4.95s`/`34.95s`) is NEVER the latest one in its own
  containing tick (`5.00s`/`35.00s` always is), so it is never actually consumed either way,
  fault-affected or not. Disclosed here as a real, measured behavior change in the RECORDED
  sidecar (which specific frame is corrupted/dropped did change) that happens to be invisible to
  every consumer this task's scope reaches, not claimed to be invisible in general (a hypothetical
  receiver stepping faster than 10 Hz, or a future test reading the sidecar's own per-frame
  corrupt/drop status directly rather than through `controller`, could observe it).
- **`AppliedPortFault.applied_tai_ns`/`frames_affected` for the corrupt/duplicate DRMs are
  unaffected in total** (per the cancellation above), confirmed by `decode_errors.rs`'s
  `corrupt_startracker_router_level_fault_event_frames_affected_is_600` passing unmodified with
  the identical pinned value `600`.
- **Replay determinism/byte-identity is unaffected in the tests that check it twice**:
  `sensor_faults.rs`'s `the_same_dropout_faulted_drm_executed_twice_produces_byte_identical_
  run_products_and_port_traffic`, `port_faults.rs`'s three `the_same_..._faulted_drm_executed_
  twice_...` tests, and `decode_errors.rs`'s `the_same_corrupt_faulted_drm_executed_twice_...` all
  passed unmodified -- a run's own internal determinism (same seed, same result) was never in
  question for this change, only the absolute epoch VALUES recorded, and these tests only compare
  a run against itself.

## 4. Defects found, including my own

1. **My own mistake, caught by running, not shipped silently:** the first draft of `tests/
   replay.rs`'s rewritten T3 picked an "interior pair of due epochs exactly 0.5 s apart" without
   checking WHICH pair -- since every consecutive due epoch in this fixture is 0.5 s apart (both
   within one kernel tick AND across the boundary between two), that selection non-deterministically
   picked a CROSS-tick pair the first time it ran, and the replay run completed with `Ok` instead
   of the expected `MissingFrame` (log: `replay_t3_1.log`). Root-caused by reading exactly what
   `ReplayModel::step_with_ports`'s new window checks (a whole call's `(t_tai_ns, end]`, not
   individual due epochs), then fixed by disambiguating a within-tick pair from a cross-tick one
   using the DRM's own declared `start_tai_ns` and 1 Hz kernel period directly (the later member of
   a within-tick pair always lands exactly on the kernel's own 1 s grid; a cross-tick pair's does
   not) -- see "What was built," above, and `replay_t3_2.log` for the fixed, passing run.
2. **A new, disclosed (not silently accepted) detection-granularity gap in `ReplayModel`'s own
   missing-frame rule**, a direct, structural consequence of message epochs no longer being
   atomic per `deliver` call -- see "What was built" (`replay.rs`) and the pinned unit test
   `deleting_only_one_of_two_recorded_sub_step_epochs_in_one_calls_window_is_not_detected`. Not a
   regression in what the OLD rule could detect (it also only ever caught a whole-call miss), but
   a new state (partial loss within a still-non-empty window) that is now possible and
   undetectable, where before R6.1 it could not even occur.
3. No other defects found. Every pre-existing test in every file this task's scope reaches passed
   unmodified (section 2), and the boundary-cancellation arithmetic worked out by hand for the
   corrupt/duplicate PORT-fault DRMs (section 3) was confirmed, not merely predicted, by those
   tests' own unchanged, exact pinned counts.

## 5. Escalations for the manager

None. Every risk this task's brief flagged in advance (golden drift, score drift,
port_traffic_hash drift) was measured directly rather than assumed, and none produced a result
requiring the manager's own judgment call: no golden changed, no score/trajectory changed, and
`port_traffic_hash` changing is the expected, task-anticipated consequence with no golden pinned
against it anywhere in this repo.

One item flagged for awareness, not action: `crates/av-kernel/tests/drm_attitude_control_cfs.rs`
and `drm_attitude_control_renode.rs` (both forbidden to me, both container/Docker-based) also
replay a container-bound instance alongside a native star tracker/IMU in the same
`demo_attitude_control` family, and were NOT run by this worker (Docker contention with the other
worker's own concurrent `docker build`/`pytest`, and both are explicitly forbidden to edit). Read
directly (not run): their own container-controller rate matches the kernel step exactly (like the
native controller), and their replay targets the CONTAINER instance, not the sub-stepping star
tracker/IMU, so the same "shadowed by `last_on_port`" reasoning in section 3 should apply
unchanged -- but this is reasoning, not measurement, for these two files specifically, and the
manager's own full gate run is what will actually exercise them.

## 6. What remains, in priority order

1. Nothing left in this worker's own scope. All five allowed-to-edit files that needed a change
   (`router.rs`, `drm/replay.rs`, `tests/replay.rs`, `tests/sensor_faults.rs`, this report) are
   done, verified, and clippy-clean; `ports.rs` and `executor.rs` needed no code change (their own
   existing tests, run directly, confirm this).
2. The manager's own full gate (`cargo test -p av-kernel`, `cargo test --workspace`) is the
   authority this worker's targeted runs are evidence for, not a substitute for.
3. `drm_attitude_control_cfs.rs`/`_renode.rs` (forbidden to this worker, Docker-gated) are the one
   place this worker's own reasoning (section 5) was not backed by an actual run -- worth the
   manager's own attention if time/Docker availability allows, though nothing in this worker's
   analysis suggests a problem there.
