# R5.1a: SENSOR fault runtime for the star tracker

Task: `docs/open-questions.md` question 178 (the SENSOR half; PORT was R4.1a/R4.1b). Build a real
SENSOR fault runtime for the star tracker only (`"bias"`, `"dropout"`, `"freeze"`, `"scale"`) --
the IMU stays a typed refusal, R5.1b's own scope, exactly the way `"corrupt"`/`"duplicate"` stayed
refused through R4.1a until R4.1b built them.

Written incrementally: hypotheses and expected values are stated in the source (module/test/
fixture doc comments) before the corresponding measurement, per the task's own instruction; this
report cross-references those in-source predictions rather than duplicating them verbatim.

## 1. What was built

- `crates/av-dynamics/src/lib.rs` -- `DynamicsModel::drain_sensor_fault_effect`, a new REQUIRED
  trait method (question 112: no default returning a valid empty result), mirroring
  `last_measurements`'s own precedent exactly; `SensorFaultEffectDrain { first_effect_tai_ns,
  frames_affected }`. Every `impl DynamicsModel` in the workspace was updated with an explicit
  arm (almost all trivial `None`; `ErasedModel`/`StmAugmented` delegate to their own wrapped
  model) -- `crates/av-dynamics/src/erase.rs`, `crates/av-dynamics/src/stm.rs`,
  `crates/gmat-sys/src/model.rs`, `crates/av-kernel/tests/ports_router.rs` (4 sites),
  `crates/av-kernel/src/kernel.rs` (6 test-only sites), `crates/av-kernel/src/schedule.rs`
  (4 test-only sites), `crates/av-kernel/src/drm/{attitude,controller,ground,replay,
  gmat_command}.rs`.
- `crates/av-kernel/src/drm/sensors.rs`:
  - `StarTrackerFaultEffect` enum (`Bias{axis,value_rad}`, `Dropout`, `Freeze`,
    `Scale{value}`); `StarTrackerSpec::fault: Option<StarTrackerFaultEffect>` (never a declared
    parameter -- set only by `fault::apply_sensor_fault` at a re-materialization boundary).
  - `StarTrackerModel::step_with_ports` applies all four: `Dropout` skips the whole emission
    (no packet, no `Measurement`, `seq` left untouched); `Bias`/`Scale` perturb the small-angle
    error vector BEFORE `small_angle_to_quat` (bias adds a fixed per-axis term; scale multiplies
    the combined noise+bias vector -- `value == 1.0` is a bit-exact no-op, asserted); `Freeze`
    latches the FIRST measurement computed after the fault epoch (`frozen_measurement:
    RefCell<Option<[f64;4]>>`) and re-emits it unchanged at every later instant in the window,
    still incrementing `seq` every period.
  - `fault_frames_affected`/`fault_first_effect_tai_ns` (`Cell`s) count every affected emission
    (dropout's suppressions included, question 186(c)); `drain_sensor_fault_effect` (real impl)
    drains and clears them, `None` when nothing has been affected since the last drain.
  - `fault` included in the settings hash (and therefore `dynamics_hash`), so a fault-bounded
    re-materialization always produces a genuinely different configuration hash and
    `merge_adjacent_segments` never wrongly merges the pre-fault/faulted/post-fault segments.
  - Module doc comment's new "SENSOR fault runtime" section: the full contract, including the
    `Freeze` "first, not last" definition and why (a re-materialized model has no history).
  - 6 new unit tests (one per effect, the no-op-at-1.0 proof, and the drain/clear proof).
- `crates/av-kernel/src/drm/fault.rs`:
  - `SENSOR_KINDS: [&str; 4] = ["bias", "dropout", "freeze", "scale"]` (`pub(crate)`, mirrors
    `PORT_KINDS`) -- the star tracker's own REAL vocabulary, deliberately DIFFERENT from ADR-005
    section 5's generic sensor vocabulary (`"bias"`, `"noise"`, `"dropout"`, `"misalign"`), which
    already existed here under the same name and is renamed to `SENSOR_KINDS_ADR005_GENERIC`
    (private, unchanged behaviour -- still `kinds_for`/`realize_unapplied_fault`'s own generic,
    never-called-by-`execute()`-for-SENSOR-any-more path). See that constant's own doc comment
    for the full "why a different vocabulary, why a different name" reasoning.
  - `apply_sensor_fault`/`clear_sensor_fault`: the SENSOR counterparts of `apply_dynamics_fault`,
    called from the identical `Boundary::Fault` arm (branched by `target_kind`).
  - `validate_no_overlapping_sensor_fault_windows`: mirrors `Router::install_port_faults`'s own
    overlap check, but keyed on `instance` ALONE, not `(instance, target)` -- see item 3 below and
    `DrmError::OverlappingSensorFaultWindows`'s own doc comment for why.
  - 9 new unit tests.
- `crates/av-kernel/src/drm/mod.rs` (`DrmError`): `PortOrSensorFaultNotYetSupported`'s doc
  comment narrowed to IMU-only (message text updated to match; variant NOT deleted, per the
  task's own explicit instruction -- R5.1b deletes it once the IMU gets a real runtime, exactly
  as R4.1b deleted `PortFaultKindNotYetSupported`). New variants: `UnknownSensorFaultKind`,
  `SensorFaultTargetNotASensor`, `OverlappingSensorFaultWindows`.
- `crates/av-kernel/src/drm/executor.rs`:
  - Load-time fault-validation loop: a SENSOR fault now resolves its own instance's binding
    (`crate::registry::kind_for`) and dispatches to the IMU refusal, the star-tracker kind/epoch
    checks (BOTH start and end epoch against the sample grid), or `SensorFaultTargetNotASensor`.
  - `fault::validate_no_overlapping_sensor_fault_windows(&scenario.faults)?` called once, after
    the loop, alongside `router.install_port_faults`.
  - `Boundary` gained `SensorFaultEnd(&'a Fault)` (the synthesized second boundary at a windowed
    SENSOR fault's own end epoch, `duration_ns > 0` only) alongside the existing `Fault`/
    `Maneuver`; `run_shared_group`'s boundary-collection pass pushes it.
  - The per-instance boundary loop: a `Boundary::Fault(f)` with `target_kind == Sensor` installs
    the effect (`apply_sensor_fault`) and records `active_sensor_fault[instance] = fault.id`
    (deferring the event, unlike DYNAMICS, since "first real effect" is data-dependent);
    `Boundary::SensorFaultEnd` restores the spec (`clear_sensor_fault`); after the per-instance
    loop, `SensorFaultEnd` clears `active_sensor_fault` and emits exactly one event from the
    accumulated totals (only if `frames_affected > 0`).
  - **A real bug found and fixed, not merely a design choice (see Defects, item 1):** the
    obvious place to drain a SENSOR fault's effect -- `ModelSpanState::handle`, right before it
    is overwritten with a fresh re-materialization -- is ALWAYS `None` at that point:
    `run_one_span` (`span.handle.take()`) erases every handle into the `HeteroKernel` for the
    WHOLE span and never puts it back; the live model (and its accumulated counter) is dropped
    with the kernel at the end of `run_one_span`. Fixed at the actual source: `crates/av-kernel/
    src/schedule.rs`'s `HeteroSystemEntry` gained a `sensor_fault_effect` running total,
    accumulated (and folded, per step) the identical way `measurements`/`applied_commands`
    already are, right after each `step_with_ports` call, exposed via `HeteroScheduler::
    sensor_fault_effect(id)`/`HeteroKernel::sensor_fault_effect(id)`; `run_one_span` gained an
    output parameter (`sensor_fault_drains: &mut BTreeMap<String, SensorFaultEffectDrain>`)
    populated from `kernel.sensor_fault_effect(name)` while `kernel` is still alive, which
    `run_shared_group` folds into its own running totals right after each `run_one_span` call.
  - Persistent (`duration_ns == 0`) SENSOR faults have no `SensorFaultEnd` boundary; the run-end
    tail folds the final span's own drain and emits the event for every fault still active in
    `active_sensor_fault`.
- `crates/av-kernel/src/drm/events.rs`: `sensor_fault_event` (mirrors `port_fault_event` --
  applied epoch not declared window start, `values["frames_affected"]`); `declared_events`
  extended to include an in-range SENSOR fault whose `kind` is in `fault::SENSOR_KINDS`. 3 new
  unit tests.
- `crates/av-kernel/src/registry.rs`: `ModelHandle::drain_sensor_fault_effect` (delegates to
  `AnyModel`) -- added for API completeness/symmetry with `describe`/`stm_capable`, though the
  actual drain path (above) does not use it (see Defects, item 1).
- `crates/av-kernel/src/drm/binding.rs`: `AnyModel::drain_sensor_fault_effect` (explicit arm per
  variant, no catch-all); `any_model_arm_count_for_replay_covers_every_dynamics_model_match_site`
  updated 11 -> 12; `any_model_arm_count_for_ground_station_matches_star_tracker`'s own doc
  comment updated (nine-method -> ten-method shape) -- the test itself needed no numeric change
  (StarTracker and GroundStation each gained exactly one matching arm, parity preserved).
- `drms/demo_attitude_control_startracker_dropout.drm.yaml` -- new fixture, `demo_attitude_
  control.{sos,*.system}.yaml` reused UNCHANGED. One `dropout` fault on `startracker.output`,
  window `[start+5s, start+35s)` (30 s, 1.5 closed-loop time constants), during the slew while
  the error is still large and actively decaying -- the header comment states the expected order
  of magnitude (true pointing error stays comparable to its value at the fault epoch, ~0.1-0.2
  rad, rather than collapsing toward the ~1e-4 rad noise floor an undisturbed loop would reach on
  this timescale) and the expected recovery by run end, BEFORE the measurement in `tests/
  sensor_faults.rs`.
- `crates/av-kernel/tests/sensor_faults.rs` -- new file, 10 tests (6 load-time refusals, the
  headline true-pointing-error acceptance test, the sidecar-cross-checked `frames_affected` test,
  a determinism test, and the CCSDS-sequence/epoch-coalescing measurement test).

## 2. Verification

`export PATH="/opt/homebrew/opt/rustup/bin:$PATH"` first, per the environment rules. Contention
checked before every run.

### Targeted (development)

- `cargo test -p av-kernel --lib drm::sensors::` -- 41 passed, 0 failed.
- `cargo test -p av-kernel --lib drm::fault::` -- 35 passed, 0 failed.
- `cargo test -p av-kernel --lib drm::events::` -- 18 passed, 0 failed.
- `cargo test -p av-kernel --lib drm::binding::` -- 138 passed, 0 failed (both arm-count tests
  confirmed passing directly: `cargo test -p av-kernel --lib any_model_arm_count`).
- `cargo test -p av-kernel --lib schedule::` -- 17 passed, 0 failed.
- `cargo test -p av-kernel --test sensor_faults` -- 10 passed, 0 failed.
- Regression sweep against everything this task's own changes touch (the shared boundary loop,
  `run_one_span`, `HeteroScheduler`, `AnyModel`, `DynamicsModel`): `cargo test -p av-kernel --test
  drm_attitude_control --test drm_attitude_sensors --test drm_executor --test port_faults --test
  replay` -- 53 passed, 0 failed across all 5 files (9 + 6 + 16 + 14 + 8).

**Predicted new total, stated before the full run (27 net-new tests, by name):**
`drm::sensors::` +6, `drm::fault::` +9, `drm::events::` +3, `tests/sensor_faults.rs` +10 (new
file) -- wait, the 6th `sensors::` test (`dropout_emits_no_packet...`) already counted --
6 + 9 + 3 + 10 = 27... actually the exact enumerated list:
- `sensors.rs`: `bias_adds_a_fixed_rotation_about_the_declared_axis_when_noise_is_zero`,
  `dropout_emits_no_packet_and_no_measurement_at_any_emission_instant`,
  `freeze_latches_the_first_measurement_and_repeats_it_while_truth_keeps_changing`,
  `scale_multiplies_the_deviation_from_truth_and_one_is_exactly_a_no_op`,
  `drain_sensor_fault_effect_is_none_with_no_fault_and_clears_once_drained` (5).
- `fault.rs`: `apply_sensor_fault_bias_writes_the_declared_axis_and_value`,
  `apply_sensor_fault_dropout_and_freeze_write_the_declared_effect`,
  `apply_sensor_fault_scale_writes_the_declared_value`,
  `apply_sensor_fault_refuses_a_target_that_does_not_match_its_own_kind`,
  `clear_sensor_fault_restores_fault_to_none_and_touches_nothing_else`,
  `overlapping_sensor_fault_windows_on_the_same_instance_are_refused_even_with_different_targets`,
  `disjoint_sensor_fault_windows_on_the_same_instance_are_legal`,
  `overlapping_windows_on_different_instances_are_legal`,
  `a_persistent_sensor_fault_conflicts_with_any_later_fault_on_the_same_instance` (9).
- `events.rs`: `sensor_fault_event_uses_the_applied_epoch_not_the_faults_own_declared_window_start`,
  `sensor_fault_event_carries_frames_affected_in_values_distinguishing_one_frame_from_many`,
  `an_in_range_sensor_fault_of_a_documented_kind_is_included_but_an_unrecognized_kind_is_not` (3).
- `tests/sensor_faults.rs` (whole new file): 10.
- **5 + 9 + 3 + 10 = 27. Baseline (R4.1b, tree started from): 807 passed, 0 failed, 1 ignored.
  Predicted: 834 passed, 0 failed, 1 ignored.**

### Full run (the gate)

- First `cargo test -p av-kernel` (before the clippy-driven `OverlappingSensorFaultWindows`
  shrink -- see "Clippy," below): saved to this session's own scratchpad,
  `full_test_run.log`. **Measured: 834 passed, 0 failed, 1 ignored** -- exactly the prediction
  above (`grep -oE "[0-9]+ passed; [0-9]+ failed; [0-9]+ ignored" full_test_run.log | awk
  '{p+=$1; f+=$3; i+=$5} END {print p, f, i}'`, summed across every test binary in the run). The
  one ignored test is still `drm_attitude_control_renode.rs::byte_identical_port_traffic_
  between_posix_container_and_renode` ("question 171: Renode port traffic beyond STEP 1 does not
  deliver; verified posix-container-only until resolved") -- unchanged, as required.
- Second `cargo test -p av-kernel` (after the `OverlappingSensorFaultWindows` field removal --
  a struct-shape change, not a test-count change; re-run for completeness against the exact
  source clippy required): saved to `full_test_run2.log`. **Measured: 834 passed, 0 failed, 1
  ignored** -- identical to the first run, confirming the field removal changed no test's own
  outcome (every `matches!` pattern on this variant already used `..`).
- `cargo test --workspace --exclude av-kernel` (`av-dynamics`, `gmat-sys`, and every other
  workspace crate this task touched or could have affected): saved to `workspace_exclude_
  kernel.log`. **Measured: 182 passed, 0 failed, 0 ignored** -- exactly the stated baseline, no
  delta (this task added no new `#[test]` function to `av-dynamics`/`gmat-sys`, only explicit
  `drain_sensor_fault_effect` arms on EXISTING test-only `DynamicsModel` impls).

### Clippy / cargo deny

- `cargo deny check` -- clean: `advisories ok, bans ok, licenses ok, sources ok`. No new
  dependency added by this task.
- `cargo clippy --workspace --all-targets -- -D warnings` -- **first run: 81 errors**, all
  `clippy::result_large_err` on `DrmError`-returning functions across `crates/av-kernel/src/drm/
  schema.rs` (and others) -- caused by this task's own `DrmError::OverlappingSensorFaultWindows`
  variant reaching 144 bytes (`fault_a`/`fault_b`/`instance`/`target_a`/`target_b`, all `String`)
  once the `target_a`/`target_b` diagnostic fields were added, pushing `DrmError`'s own largest
  variant past clippy's threshold and cascading into every OTHER function in the crate returning
  `Result<_, DrmError>` (not merely the one new variant's own call sites) -- a whole-enum-size
  lint, not a per-variant one. Fixed at the root cause, not suppressed: `target_a`/`target_b`
  removed (the actual join key is `instance` alone -- see the variant's own doc comment for why
  those two fields were never load-bearing information in the first place, only diagnostic
  convenience); `instance` + 2 fault ids + the interval is enough, and brings the variant back
  under 128 bytes. **Second run: clean**, 0 lines matching `^warning:|^error:` in the whole log,
  saved to `clippy_full2.log`. No `#[allow]` added anywhere in this task's own diff -- checked
  directly: `git diff -- crates/av-dynamics crates/av-kernel crates/gmat-sys | grep -c
  "^\+.*#\[allow("` = 0.

## 3. Measurements worth keeping

**The headline acceptance test (`demo_attitude_control_startracker_dropout.drm.yaml`) --
R5.1a-fix correction.** The manager's review of R5.1a found this test's own original bounds
(`err > 1e-3` during the window, `err < 1e-2` at run end) were satisfied by the UNFAULTED baseline
too, so they were never evidence `Dropout` does anything -- see the "R5.1a-fix (manager's review)"
section, below, for the full account. The test now runs BOTH `demo_attitude_control_startracker_
dropout.drm.yaml` and the unfaulted `demo_attitude_control.drm.yaml` in the same test and asserts
a DIFFERENCE at matched epochs (`tests/sensor_faults.rs::dropout_fixture_true_pointing_error_
diverges_below_the_unfaulted_baseline_during_the_window_and_reconverges_by_run_end`). **Measured**
(both runs, real noise/discretization included): t=5s baseline-faulted = 0.0 exactly (bit-
identical, confirming nothing has diverged before the fault epoch: faulted=baseline=1.950298e-1
rad, matching the undisturbed closed-form prediction 0.2*1.25*exp(-0.25) = 0.1947 rad); t=20s
faulted=1.448198e-1 rad, baseline=1.475094e-1 rad, gap=2.6896e-3 rad; t=35s
faulted=7.677167e-2 rad, baseline=9.572128e-2 rad, gap=1.8950e-2 rad; t=300s
faulted=1.451282e-4 rad, baseline=1.213215e-4 rad, gap=-2.3807e-5 rad (sign flipped, both
re-settled to the ~1e-4 rad order of magnitude). **The corrected direction, derived before
measuring** (frozen, never-shrinking `qv_z0` "overdrives" the decay relative to the baseline's own
live, shrinking `qv_z`): the true error ends up SMALLER than the baseline's own trajectory during
the window, growing from ~0.003 rad at t=20s to ~0.019 rad by t=35s -- the OPPOSITE of "the frozen
error causes a bigger swing than baseline," which has the sign backwards. This is why the original
"stays large" framing was misleading even though the absolute-magnitude claim inside it was true
(the error genuinely never collapses to the ~1e-4 rad noise floor during the window) -- "large
relative to the noise floor" and "larger than the baseline" are different claims, and only the
first one holds. Full derivation and the linearized-vs-measured comparison table are in `tests/
sensor_faults.rs`'s own doc comment on the renamed test, and in `drms/demo_attitude_control_
startracker_dropout.drm.yaml`'s own header comment.

**`frames_affected` cross-check.** The dropout fault's own `EVENT_KIND_FAULT.values
["frames_affected"]` matches EXACTLY (not merely approximately) the count of `startracker.
st_meas` OUT records the UNFAULTED baseline's own real `PortTrafficLog` sidecar recorded inside
the identical `[5s, 35s)` window -- measured, not hardcoded (`tests/sensor_faults.rs::
dropout_fault_event_frames_affected_matches_the_unfaulted_baselines_own_sidecar_count`).

**Known pre-existing behaviour (design item 5), measured, not redesigned.**
1. The CCSDS sequence count DOES restart at 0 at EACH of the two re-materialization boundaries
   (fault start and fault end) -- confirmed directly (`tests/sensor_faults.rs::measured_ccsds_
   sequence_restarts_at_zero_...`).
2. Investigating "does the emission grid shift" surfaced a SECOND, genuinely PRE-EXISTING finding
   unrelated to this task: `crate::router::Router::deliver(&mut self, from_instance, emission_
   tai_ns, outbox)` stamps EVERY message in one `Outbox` with the single caller-supplied
   `emission_tai_ns` (the KERNEL STEP's own end epoch), never each message's own individually-
   `push`ed due epoch (`PortMessage.tai_ns`, set correctly inside `StarTrackerModel::
   step_with_ports`). The star tracker's declared 20 Hz rate exceeds this fixture's 10 Hz kernel
   step, so two real, 0.05s-apart emissions land inside one kernel step and are logged with the
   IDENTICAL, coarser `PortTrafficRecord.tai_ns`, distinguished only by `sequence_count` (measured
   directly: both records at the fault-end boundary's first kernel step share one `tai_ns`, with
   `sequence_count` 0 and 1). This means the `PortTrafficLog` sidecar cannot answer "does the true,
   sub-kernel-step due-epoch grid shift" for this instance at all; only the model's own `next_due`
   arithmetic can (exact, and already pinned by this crate's own pre-existing unit tests). This
   confound applies to the UNFAULTED baseline too, at every ordinary kernel step where two
   emissions land inside one -- it predates and is unrelated to this task's own SENSOR fault work.
   **Not fixed here** (out of this task's own charter) -- see Escalations, below.

**Break-and-restore evidence.** [See section 5 for the itemized list -- N wrong implementations
built, run against the relevant real test(s), confirmed to fail, then reverted; `git diff` on
every touched file was empty after every restore.]

## 4. Defects found, including my own

1. **My own design defect, caught by a failing integration test, not shipped silently:** the
   first draft of the executor wiring drained a SENSOR fault's accumulated effect from
   `ModelSpanState::handle` right before it was overwritten with a fresh re-materialized handle
   -- the same shape `materialize_plan_at_boundary`'s own surrounding code already uses for
   everything else. `handle` is ALWAYS `None` at that point: `run_one_span` erases every handle
   into the `HeteroKernel` for the whole span (`span.handle.take()`) and never restores it; the
   live model (and its internal counter) is dropped along with the kernel when `run_one_span`
   returns. `tests/sensor_faults.rs::dropout_fault_event_frames_affected_matches_the_unfaulted_
   baselines_own_sidecar_count` failed with `left: 0, right: 1` (no FAULT event found at all --
   the accumulated count was silently always zero). Root-caused by adding temporary `eprintln!`
   tracing (removed before this report), which showed `handle_some=false` on every single call.
   Fixed at the actual source (`crates/av-kernel/src/schedule.rs`'s `HeteroSystemEntry`), not
   papered over in the executor -- see "What was built," above.
2. **A second, genuinely pre-existing defect/quirk surfaced (not introduced by this task, not
   fixed by this task):** `Router::deliver`'s own per-Outbox (not per-message) epoch stamping --
   see "Measurements," item 2, and "Escalations," below.
3. **My own bug in the `Freeze` implementation, caught by its own new unit test before this
   report was written:** `if let Some(frozen) = *self.frozen_measurement.borrow() { ... } else {
   ...self.frozen_measurement.borrow_mut()... }` panicked (`RefCell already borrowed`) the first
   time it ever fired -- a `Ref` scrutinee's borrow stays alive across the WHOLE `if let`/`else`
   in Rust, including the `else` arm's own `borrow_mut()`. Fixed by reading the borrow into a
   plain, owned `Option<[f64; 4]>` (`Copy`) in a separate statement first.

## 5. Break-and-restore evidence

Every new unit/integration test was confirmed to fail against a nameable wrong implementation,
then the implementation was restored; `git diff` on every touched file was empty after every
restore (checked directly, not assumed).

**R5.1a-fix update: every item below (1-12, 14) has now been mechanically executed** -- a real
snapshot of the touched file taken first, the exact break described applied with the `Edit` tool,
the named test re-run, the real panic/assertion text captured verbatim to a scratch file, the file
restored from its snapshot, and `cmp` run to prove byte-identity before moving to the next item
(not merely `git diff --stat`). Item 13 remains disclosed, not independently pinned by a dedicated
fixture -- see its own entry below for why. The original report's own honest disclosure (quoted
below the item list, struck through in spirit, kept for the record) covered only item 12 plus two
bugs found live during development; this pass closes the rest.

<details>
<summary>Original "honest accounting" disclosure (R5.1a, superseded by the executed evidence below)</summary>

> Item 12 (the recovery-assertion one) was deliberately, freshly executed for this report... Two
> further items were genuinely, independently discovered as real bugs during development (Defects
> items 1 and 3)... The remaining items below (2-11, 13-14) are stated as the failure mode each
> test's own assertions are DESIGNED to catch, not independently re-verified by literally
> reverting and re-running each one.

</details>

1. **`sensors.rs`: `Dropout` never suppresses the emission** (`if matches!(...Dropout)` changed to
   `if false && matches!(...Dropout)`, falling through to the normal packet-emitting path).
   **Executed** against BOTH tests this affects:
   `drm::sensors::tests::dropout_emits_no_packet_and_no_measurement_at_any_emission_instant` --
   `panicked at crates/av-kernel/src/drm/sensors.rs:1904:13: dropout must emit no packet`; and the
   rewritten headline integration test (job 1, item 4 below) -- `sensor_faults.rs::
   dropout_fixture_true_pointing_error_diverges_below_the_unfaulted_baseline_during_the_window_
   and_reconverges_by_run_end` -- `panicked at crates/av-kernel/tests/sensor_faults.rs:356:5: t=35s
   (window end): baseline (0.09572127597101422 rad) must exceed faulted (0.09572816620889142 rad)
   by more than 0.005 rad ... a no-op dropout would leave this gap at the RNG-restart noise floor
   (~1e-5 rad), not this` -- the real gap left by a no-op dropout was measured at 6.89e-6 to
   7.09e-6 rad (t=35s/t=20s), exactly the RNG-restart-noise-floor order of magnitude predicted, not
   the ~1.9e-2/2.7e-3 rad the real fault produces. Restored; `cmp` confirmed byte-identical; both
   re-runs green.
2. **`sensors.rs`: `Bias` writes into the wrong axis** (`err_vec[axis] += value_rad` changed to
   `err_vec[0] += value_rad`, `axis` unused). **Executed:**
   `bias_adds_a_fixed_rotation_about_the_declared_axis_when_noise_is_zero` -- `panicked at
   crates/av-kernel/src/drm/sensors.rs:1883:9: x axis must be untouched:
   [0.0009999999999999998, 0.0, 0.0]` (the y-axis-declared bias landed on x instead). Restored;
   `cmp` confirmed byte-identical; re-run green.
3. **`sensors.rs`: `Scale` multiplies AFTER `small_angle_to_quat`** (moved the `value` multiply
   from the small-angle vector to the resulting quaternion's raw `[qx,qy,qz,qw]` components).
   **Executed:** `scale_multiplies_the_deviation_from_truth_and_one_is_exactly_a_no_op` --
   `panicked at crates/av-kernel/src/drm/sensors.rs:1981:13: axis 0: scaled=2.686e-5 expected
   2x baseline=5.372e-5` -- confirms the report's own prediction exactly: the `value == 1.0` no-op
   case still passed by coincidence, but the `value == 2.0` linearity check failed (a quaternion is
   not a vector space). Restored; `cmp` confirmed byte-identical; re-run green.
4. **`sensors.rs`: `Freeze` re-computes a fresh measurement every time** (the `if let Some(frozen)
   = already_frozen { frozen } else { ... }` branch replaced with an unconditional fresh
   computation, `already_frozen` discarded). **Executed:**
   `freeze_latches_the_first_measurement_and_repeats_it_while_truth_keeps_changing` -- `panicked at
   crates/av-kernel/src/drm/sensors.rs:1943:13: assertion left == right failed: emission 1 must
   repeat the FIRST measurement exactly, byte for byte, not the true (changing) attitude
   left: [-5.0492647544870186e-5, 8.059320694686155e-5, 0.008717184125194432, 0.9999620001060816]
   right: [-3.004386693759247e-6, -1.2175525164929009e-6, -3.2897840367511846e-5,
   0.9999999994536116]`. Restored; `cmp` confirmed byte-identical; re-run green.
5. **`sensors.rs`: `record_fault_effect` never sets `fault_first_effect_tai_ns`** (only the count
   incremented, `due` discarded). **Executed:**
   `drain_sensor_fault_effect_is_none_with_no_fault_and_clears_once_drained` -- `panicked at
   crates/av-kernel/src/drm/sensors.rs:843:14: fault_frames_affected > 0 implies fault_first_
   effect_tai_ns is Some -- record_fault_effect always sets both together` -- matches the original
   report's own predicted panic text exactly. Restored; `cmp` confirmed byte-identical; re-run
   green.
6. **`sensors.rs`: `drain_sensor_fault_effect` never clears its own accumulator** (the `self.
   fault_frames_affected.set(0)`/`self.fault_first_effect_tai_ns.set(None)` reset lines removed).
   **Executed:** `drain_sensor_fault_effect_is_none_with_no_fault_and_clears_once_drained` --
   `panicked at crates/av-kernel/src/drm/sensors.rs:2005:9: a second, immediate drain with nothing
   newly affected must be empty, not a repeat`. Restored; `cmp` confirmed byte-identical; re-run
   green.
7. **`fault.rs`: `apply_sensor_fault`'s `"bias"` arm accepts any target** (the `match fault.
   target.as_str() { ... }` replaced with an unconditional `axis = 0`). **Executed:**
   `apply_sensor_fault_refuses_a_target_that_does_not_match_its_own_kind` -- `panicked at
   crates/av-kernel/src/drm/fault.rs:896:96: called Result::unwrap_err() on an Ok value:
   StarTracker(StarTrackerSpec { ..., fault: Some(Bias { axis: 0, value_rad: 1.0 }) })` (a `"bias"`
   fault naming `startracker.scale` was silently accepted instead of refused). Restored; `cmp`
   confirmed byte-identical; re-run green.
8. **`fault.rs`: `validate_no_overlapping_sensor_fault_windows` keys on `(instance, target)`
   instead of `instance` alone** (`if a.instance != b.instance` changed to `if a.instance !=
   b.instance || a.target != b.target`). **Executed:** `overlapping_sensor_fault_windows_on_the_
   same_instance_are_refused_even_with_different_targets` -- `panicked at crates/av-kernel/src/
   drm/fault.rs:929:73: called Result::unwrap_err() on an Ok value: ()` -- the two different-target
   faults loaded successfully instead of being refused. Restored; `cmp` confirmed byte-identical;
   re-run green.
9. **`fault.rs`: the overlap check over-fires on disjoint windows** (`if overlap_start <
   overlap_end` changed to `if overlap_start <= overlap_end`). **Executed:**
   `disjoint_sensor_fault_windows_on_the_same_instance_are_legal` -- `panicked at crates/av-kernel/
   src/drm/fault.rs:938:9: assertion failed: validate_no_overlapping_sensor_fault_windows
   (&faults).is_ok()`. Restored; `cmp` confirmed byte-identical; re-run green.
10. **`executor.rs`: the load-time loop never distinguishes IMU from star tracker** (the
    `ModelKind::Imu => {...refuse...}, ModelKind::StarTracker => {...validate...}` arms merged into
    one `ModelKind::Imu | ModelKind::StarTracker => {...refuse...}` arm, leaving the original
    `StarTracker` arm dead code below it -- compiles with an `unreachable_patterns` warning, not an
    error, under plain `cargo test`, so no `#[allow]` was needed for the experiment). **Executed:**
    `a_sensor_fault_naming_an_unrecognized_kind_on_a_star_tracker_is_a_typed_load_error` --
    `panicked at crates/av-kernel/tests/sensor_faults.rs:118:5: PortOrSensorFaultNotYetSupported
    { fault_id: "f_bad_kind", instance: "startracker", target_kind: "FAULT_TARGET_KIND_SENSOR" }`
    -- the pre-task refusal variant, not the new typed `UnknownSensorFaultKind` the test expects.
    Restored; `cmp` confirmed byte-identical; re-run green.
11. **`executor.rs`: the END epoch is never checked against the sample grid** (`if f.duration_ns >
    0` changed to `if false && f.duration_ns > 0`). **Executed:**
    `a_sensor_fault_end_epoch_off_the_sample_grid_is_a_typed_load_error` -- **not** the typed
    refusal the test expects: `panicked at crates/av-kernel/src/kernel.rs:718:9: run horizon
    (1500000000 ns) is not an exact multiple of the output period (1000000000 ns)` -- an
    UNTYPED, lower-level `HeteroKernel::run` panic, deep inside execution rather than a clean
    load-time refusal, confirming this check is load-bearing for turning an internal invariant
    violation into a typed error the caller can match on. Restored; `cmp` confirmed byte-identical;
    re-run green.
12. **`executor.rs`: `Boundary::SensorFaultEnd` never restores the spec** (the `fault::
    clear_sensor_fault` line removed from the match arm). Already executed in R5.1a against the
    ORIGINAL (now-superseded) headline test (see original disclosure above); **re-executed here**
    against the NEW, renamed headline test as well, since the old test it was originally proven
    against no longer exists (`span.cur_plan = fault::clear_sensor_fault(&span.cur_plan)` changed
    to `let _ = fault::clear_sensor_fault(&span.cur_plan)`): `panicked at crates/av-kernel/tests/
    sensor_faults.rs:327:112: the star-tracker-dropout DRM executes end to end: InvalidExpression
    { name: "controller_pointing_error_at_end", reason: "position 0: time 300 s is outside the
    run's window [0.2, 35] s" }` -- identical failure mode to the original R5.1a execution (the
    fix did not touch this line, only the assertions after `execute()` returns, so the same
    upstream failure reproduces). Restored; `cmp` confirmed byte-identical; re-run green.
13. **`executor.rs`: the SensorFaultEnd event uses `sensor_fault_totals.get(&f.id)` unconditionally
    (no `frames_affected > 0` guard)** -- would emit a spurious event even for a fault that never
    applied. **Still disclosed, not independently pinned, by this pass's own explicit decision:**
    the `if let Some((Some(first_effect_tai_ns), frames_affected))` destructuring this guard sits
    behind already only matches when `fold_sensor_fault_span_drain` set the first element to
    `Some` (line ~1616, gated on `drain.frames_affected > 0`), so the reachable-in-practice half of
    "a fault that never fires" is structurally prevented already; building a fixture that reaches
    the remaining, purely-defensive half (a fault installed but truth never once arrives before its
    window closes -- `StarTrackerModel::step_with_ports`'s own `if let Some((truth_q, ...)) =
    *self.last_truth.borrow()` guard, which `record_fault_effect` sits inside) would need a new,
    carefully-timed load-refusal-adjacent bundle (truth wired but delayed past the fault window)
    disproportionate to what it would prove, given every other item in this pass was executed for
    real. Left disclosed, per the task's own explicit permission to make this call.
14. **`schedule.rs`: `sensor_fault_effect` never accumulates, only overwrites** (`sys.
    sensor_fault_effect.1 += drain.frames_affected` changed to `sys.sensor_fault_effect.1 =
    drain.frames_affected`). **Executed:**
    `dropout_fault_event_frames_affected_matches_the_unfaulted_baselines_own_sidecar_count` --
    `panicked at crates/av-kernel/tests/sensor_faults.rs:394:5: assertion left == right failed:
    frames_affected (2) must match the count of st_meas OUT records the unfaulted baseline's own
    sidecar recorded inside the identical window (600) ... left: 2.0 right: 600.0` -- confirms the
    report's own prediction exactly: only the LAST step's own contribution (2, the final kernel
    step's worth) survived, not the sum (600).

All twelve breaks were applied one file at a time (`crates/av-kernel/src/drm/sensors.rs`,
`fault.rs`, `executor.rs`, `crates/av-kernel/src/schedule.rs`), restored immediately from a
pre-task snapshot taken into this session's own scratchpad before any edit, and `cmp`-verified
byte-identical before moving to the next item -- never more than one file mutated at a time. After
all fourteen items (12 newly executed here, plus the 2 genuine defects and item 12 already real
from R5.1a), `cargo test -p av-kernel --lib drm::sensors::`/`drm::fault::`/`schedule::` and
`cargo test -p av-kernel --test sensor_faults` were re-run and confirmed green at their original
counts (41/35/17/10 passed respectively) -- the restore left no trace.

## 6. Escalations for the manager

1. **`Router::deliver`'s per-Outbox (not per-message) epoch stamping** (Measurements/Defects,
   item 2) -- a genuinely pre-existing property, discovered while investigating this task's own
   "does the emission grid shift" question, unrelated to and not introduced by SENSOR faults: any
   native model emitting more than once per kernel step has every one of those emissions logged
   in `PortTrafficLog` with the SAME, coarser `tai_ns` (the kernel step's own end epoch), never
   each message's own individually-set due epoch. This means `PortTrafficLog` cannot answer
   "when, exactly, did this specific emission happen" for a sensor running faster than its own
   kernel step -- only `sequence_count` disambiguates within one coalesced epoch, and the true
   sub-step timestamp is unrecoverable from the sidecar at all. Flagging per the standing
   instruction to report a defect found, even one outside this task's own charter, rather than
   silently working around it. Not fixed here (would touch `Router::deliver`'s own signature/
   contract, `crate::router`'s module doc comment's "the arrival epoch is derivable, not stored
   twice" reasoning, and every existing PORT-fault test that reads `PortTrafficRecord.tai_ns`) --
   a decision for the manager/lead on whether and how to fix it, and whether it is in fact already
   known/accepted (it may be; `crate::router`'s own doc comment never explicitly claims per-
   message epoch fidelity, only "the emission epoch," which this finding shows is ambiguous the
   moment more than one message shares a step).
2. **The overlap-refusal key for SENSOR faults is `instance` alone, not `(instance, target)`** --
   a DELIBERATE departure from the literal PORT precedent the task's own item 3 named, disclosed
   in `DrmError::OverlappingSensorFaultWindows`'s own doc comment and `crates/av-kernel/src/drm/
   fault.rs::validate_no_overlapping_sensor_fault_windows`'s own doc comment: PORT's `(instance,
   port)` key is correct there because two different ports are genuinely independent channels a
   `Router` can act on simultaneously, but a star tracker's own SENSOR fault runtime carries its
   currently-installed effect in ONE `Option<StarTrackerFaultEffect>` slot regardless of which of
   the four kinds it is -- two faults naming DIFFERENT targets on the SAME instance (e.g. a
   `"bias"` fault on `startracker.bias_rad.x` and a `"scale"` fault on `startracker.scale`) could
   not both be installed at once either, so keying the refusal on `target` alone would let such a
   pair load and then silently let the later one clobber the earlier one's own effect for the
   overlap -- exactly the ambiguous-join failure mode question 184's own reasoning exists to
   prevent. Recommend the manager/lead ratify this reading explicitly (or direct a different
   resolution) before a real DRM ever declares two different-kind SENSOR faults on one instance.
3. **`corrupt_mask`-style parameter naming for `Bias`/`Scale`.** `Fault.params["value"]` (reused,
   not a new key) carries the bias radians or the scale factor -- the same "declared byte mask" /
   "the ONE value this run already has" minimalism R4.1a/R4.1b's own escalations already
   established as this crate's convention, not independently re-litigated here.
4. **A wrong-implementation test for "a SensorFaultEnd event with `frames_affected == 0` never
   fires" (break-and-restore item 13) is disclosed but not independently pinned** by a dedicated
   fixture -- every real fixture's own SENSOR fault genuinely applies at least once, so this
   guard's own reachable-in-practice half is exercised only indirectly (via `dropout_fault_event_
   frames_affected_...`'s own positive case). If the manager wants this closed explicitly, it
   needs a fixture whose fault window contains no real emission (e.g. truth never arrives before
   the window closes) -- not built here, out of this task's own time budget.
5. **`av_dynamics::DynamicsModel` gained a new required method** (`drain_sensor_fault_effect`),
   touching ~13 files across `av-dynamics`/`gmat-sys`/`av-kernel` purely to add a trivial `None`
   arm each -- the identical, accepted cost `last_measurements` (question 173) already paid, per
   question 112's own standing rule ("a required method with no default needs none of [the
   caller] call sites to change -- only the [smaller] set of `impl DynamicsModel for ...`
   blocks"). Flagged only for visibility, not because it is in doubt.

## 7. What remains, in priority order

1. **`Router::deliver`'s per-Outbox epoch-stamping confound** (escalation 1) -- a decision for
   the manager/lead: fix, formally accept and document, or defer. Affects every high-rate native
   sensor under a coarser kernel step, not only a faulted one.
2. **A dedicated "a SENSOR fault whose window never actually fires produces no event" fixture**
   (escalation 4) -- closes break-and-restore item 13's own gap explicitly.
3. **A second closed-loop SENSOR fault fixture for a kind other than `dropout`** (e.g. `freeze`
   or `bias`) through the full attitude-control loop, for parity with how R4.1b built both a
   `demo_command`-family PORT test (drop/delay/corrupt/duplicate, byte-level) AND a closed-loop
   replay fixture -- this task's own headline test covers `dropout` end to end; `bias`/`freeze`/
   `scale` are proven at the unit level (`sensors.rs`) and the load-refusal/apply level
   (`fault.rs`) but not through a real closed loop's own physical response.
4. **A `crate::drm::replay` test replaying the star-tracker instance of a SENSOR-faulted run**
   (mirroring R4.1b's own T5) -- not attempted here; `Router::install_port_faults` is called
   unconditionally regardless of `RunConfig.replay` (R4.1b's own "faults DO re-apply during
   replay" finding), but a SENSOR fault's own re-application during replay goes through
   `apply_sensor_fault`/`materialize_plan_at_boundary`, not `Router`, and has not been checked
   against a replayed instance directly.
5. The IMU's own SENSOR fault runtime -- R5.1b's own charter, `DrmError::
   PortOrSensorFaultNotYetSupported` (now IMU-only) is the one remaining typed refusal.

## 8. R5.1a-fix (manager's review)

A follow-up pass (worker R5.1a-fix), reporting to the round-5 manager, with two jobs: (1) fix the
headline acceptance test, which the manager found asserted bounds the UNFAULTED baseline also
satisfies -- not evidence `Dropout` does anything; (2) mechanically execute the break-and-restore
cycles section 5 above had only reasoned about (items 1-11, 14; item 12 already real; item 13
stays disclosed).

### 8.1 What changed

- `crates/av-kernel/tests/sensor_faults.rs` -- the headline test renamed
  `dropout_fixture_true_pointing_error_stays_large_during_the_window_and_recovers_by_end_of_run`
  -> `dropout_fixture_true_pointing_error_diverges_below_the_unfaulted_baseline_during_the_window_
  and_reconverges_by_run_end`; it now executes BOTH `demo_attitude_control_startracker_dropout.
  drm.yaml` and the unfaulted `demo_attitude_control.drm.yaml` in the same test and asserts a
  DIFFERENCE between the two trajectories at matched epochs, with the expected direction and
  magnitude derived in the doc comment BEFORE the numbers it then reports. The old absolute-bound
  assertions (`err > 1e-3` during the window, `err < 1e-2`/`> 1e-8` at run end -- all satisfied by
  the unfaulted baseline, per the manager's own review) are removed, not kept alongside the new
  ones -- non-evidence, not a looser tolerance to preserve.
- `drms/demo_attitude_control_startracker_dropout.drm.yaml` -- header comment corrected in place
  (not deleted) to state the right DIRECTION: the frozen restoring torque "overdrives" the decay,
  so the true error ends up SMALLER than the baseline's own trajectory during the window, not
  larger -- the naive "frozen larger error causes a bigger swing" reading has the sign backwards.
  No scenario/objective data changed (comments only), so `hash` was not recomputed -- verified by
  re-running `cargo run -p av-kernel --example drm_hash -- drm drms/demo_attitude_control_
  startracker_dropout.drm.yaml` and confirming the output still matches the file's own declared
  `ee3066aa8f7f436a1d08cfbcb8367d5a68e3450bb6f972ed8547ad6a8a0b6f9e`.
- `crates/av-kernel/R5_1A_REPORT.md` -- this section appended; section 3's headline-test paragraph
  and section 5 (break-and-restore evidence) updated in place with the real captured evidence.
- No changes to `proto/**`, `docs/**`, `services/**`, or `crates/av-dynamics` (out of scope, per
  the task's own explicit instruction) -- confirmed by `git status --short` showing no files
  touched outside `crates/av-kernel/{tests/sensor_faults.rs,R5_1A_REPORT.md}` and `drms/
  demo_attitude_control_startracker_dropout.drm.yaml` for this pass (the `src/drm/{sensors,fault,
  executor}.rs`/`schedule.rs` changes were the ORIGINAL R5.1a worker's, already on disk before
  this pass started, and every one of this pass's own break-and-restore edits to them was
  `cmp`-verified byte-identical to that starting state after each restore).

### 8.2 Job 2: break-and-restore, mechanically executed

Every touched file was snapshotted (`cp`) into this session's own scratchpad before any edit. Each
of the 12 items below was applied one at a time (one file, one targeted change), the named test
re-run, the real failure text captured verbatim, the file restored from its snapshot, and `cmp`
run to prove byte-identity BEFORE moving to the next item -- never more than one file mutated at
once. Full detail (the exact edit, the exact captured panic text, per item) is in section 5 above,
which was edited in place rather than duplicated here. Item 12 was re-executed against the NEW
headline test as well (the old test it was originally proven against no longer exists after the
rename). Item 13 stays disclosed -- see its own entry in section 5 for the reasoning (the
reachable-in-practice half is already structurally prevented by the destructuring the guard sits
behind; the remaining, purely-defensive half would need a new, carefully-timed fixture
disproportionate to what it would prove, given every other item was executed for real this pass).

After all cycles: `cmp` confirmed every one of `crates/av-kernel/src/drm/{sensors,fault,
executor}.rs` and `crates/av-kernel/src/schedule.rs` byte-identical to its pre-pass snapshot (not
merely `git diff --stat`); `git status --short` on those four paths shows only the ORIGINAL R5.1a
`M` markers, no residual edits from this pass. `cargo test -p av-kernel --lib drm::sensors::`/
`drm::fault::`/`schedule::` and `cargo test -p av-kernel --test sensor_faults` were re-run
afterward and confirmed green at their original counts (41/35/17/10 passed).

### 8.3 Job 1: measured faulted-vs-baseline divergence

**Expectation, stated before measuring** (full derivation in `tests/sensor_faults.rs`'s own doc
comment on the renamed test, and in the fixture's own header comment): linearizing the control law
around the fault epoch with `qv_z` frozen at its own t=5s value (`0.097307` rad) while `omega_z`
keeps updating from the live, unfaulted IMU gives a first-order relaxation in the rate with a
CONSTANT forcing term -- unlike the baseline's own `qv_z`, which shrinks as the real error shrinks.
Predicted, before running: `theta_base(20) - theta(20) ~= 0.0028` rad (~1.9%), growing to
`theta_base(35) - theta(35) ~= 0.0191` rad (~20%) by window end -- the faulted true error ends up
SMALLER than the baseline's, not larger (the frozen, never-shrinking restoring torque "overdrives"
the decay), with the gap growing through the window and reconverging to the same order of
magnitude by run end.

**Measured** (both DRMs executed in the same test, real noise and discretization included): t=5s
baseline-faulted = 0.0 exactly (bit-identical, confirming nothing diverges before the fault
epoch); t=20s gap = 2.6896e-3 rad (predicted ~0.0028); t=35s gap = 1.8950e-2 rad (predicted
~0.0191); t=300s gap = -2.3807e-5 rad (sign flipped, both re-settled to the ~1e-4 rad order of
magnitude). The idealized linearization lands within a few percent of the real, noisy measurement
at both in-window epochs, confirming the frozen-forcing "overdrive" mechanism -- not RNG noise --
is what the divergence assertions measure. **Proof the new test cannot pass against a no-op
dropout** (mandatory, mechanically executed, not reasoned): `StarTrackerModel::step_with_ports`'s
own `Dropout` branch broken to never fire (falls through to the normal emitting path); the real
gap a no-op dropout leaves is 6.89e-6 to 7.09e-6 rad (t=35s/t=20s) -- the RNG-restart-at-
rematerialization noise floor, not the ~1.9e-2/2.7e-3 rad the real fault produces -- so the
`> 0.005` rad divergence assertion at t=35s genuinely fails against the no-op (`panicked at
crates/av-kernel/tests/sensor_faults.rs:356:5`, captured verbatim in section 5, item 1).

**The window was NOT changed.** Per the task's own instruction to consider a different window if
the chosen one produces only a small departure: it does not -- the measured t=35s gap (1.895e-2
rad) is ~2,700x the no-op-dropout noise floor (~7e-6 rad), a large, honestly-explainable,
mechanistically-derived divergence. No rehash was needed as a result.

**Neighbouring assertions, sanity-checked per the task's own instruction:** `err_at_run_end >
1e-8` (the old test) was deleted outright -- a generically-true "not exactly zero" claim any
noisy closed loop satisfies, faulted or not; not evidence of anything. The old `err_mid_window >
1e-3`/`err_at_window_end > 1e-3` bounds were likewise deleted, not kept alongside the new
baseline-relative assertions -- both are satisfied by the unfaulted baseline (`1.475e-1`/`9.572e-2`
rad at t=20s/t=35s, both `> 1e-3`), so per the task's own rule ("a bound the unfaulted run also
satisfies is not a tolerance, it is a non-assertion") they were removed, not loosened or retained.

### 8.4 Defects found by this pass

1. **The headline test's own bounds were non-evidence** -- the defect this whole pass exists to
   fix; see job 1 above and the manager's own original finding. Not a code defect, a TEST defect
   (M25.4's own "a test that reads stronger than it is" failure mode, recurring).
2. **`executor.rs`'s missing END-epoch check (break-and-restore item 11) fails UNTYPED, not
   typed, without the guard** -- worth flagging beyond the item's own entry in section 5: without
   the check, the run does not merely load and execute (as items 1-10/14's own break descriptions
   predicted for their own guards); it panics deep inside `HeteroKernel::run` (`crates/av-kernel/
   src/kernel.rs:718`, "run horizon ... is not an exact multiple of the output period") -- an
   internal invariant violation surfacing as an untyped panic rather than a typed `DrmError`. This
   confirms the check is load-bearing for error-type hygiene, not merely for refusing a
   theoretically-malformed DRM early; a caller matching on `DrmError` variants would not catch
   this at all without the guard. No fix needed (the guard already exists and is correct) --
   flagged as a useful data point on WHY the check matters, found only by actually executing the
   break rather than reasoning about it.
3. **`executor.rs`'s merged-arm break (item 10) compiles with a warning, not an error, under plain
   `cargo test`** -- worth noting for future break-and-restore work: an `unreachable_patterns`
   warning from a duplicate match arm does not block `cargo test` (only `-D warnings`, i.e.
   `cargo clippy`, would), so no `#[allow]` was needed for the experiment, and the break was still
   fully mechanical (no source restructuring beyond the literal arm merge the report's own item 10
   describes).

### 8.5 Escalations for the manager

1. Item 13 (break-and-restore, the `frames_affected > 0` guard on `SensorFaultEnd`'s own event
   emission) remains disclosed, not independently pinned by a dedicated fixture -- this pass's own
   explicit call, per the task's own permission to make it. See section 5, item 13, for the full
   reasoning (the reachable-in-practice half is already structurally prevented; the remaining half
   needs a new fixture disproportionate to what it would prove).
2. Every escalation from the original R5.1a report (section 6) is unaffected by this pass and
   still stands -- not re-litigated here.

### 8.6 Verification

`cargo test -p av-kernel --test sensor_faults` -- 10 passed, 0 failed, 0 ignored (re-run green
after every restore in this pass, most recently after the item-12 re-verification cycle).
`cargo test -p av-kernel --lib drm::sensors::`/`drm::fault::`/`schedule::` -- 41/35/17 passed, 0
failed (unchanged from R5.1a's own baseline).

**Full suite** (`cargo test -p av-kernel`, saved to this session's own scratchpad,
`full_test_run_fix.log`) -- **predicted before running: 834 passed, 0 failed, 1 ignored**
(unchanged from the R5.1a baseline -- this pass renamed/rewrote one existing test and added no new
`#[test]` function anywhere). **Measured: 834 passed, 0 failed, 1 ignored** -- exactly the
prediction (`grep -oE "[0-9]+ passed; [0-9]+ failed; [0-9]+ ignored" full_test_run_fix.log | awk
'{p+=$1; f+=$3; i+=$5} END {print p, f, i}'`, summed across every test binary in the run,
`sensor_faults.rs` itself showing 10/10 passed within it). The one ignored test is still
`drm_attitude_control_renode.rs::byte_identical_port_traffic_between_posix_container_and_renode`
(question 171, unchanged).

**Clippy** (`cargo clippy --workspace --all-targets -- -D warnings`, saved to
`clippy_fix2.log`) -- first run caught a real `clippy::doc_lazy_continuation` error this pass's own
new doc comment introduced (`tests/sensor_faults.rs`'s derivation table: a formula split across two
`///` lines happened to start the second line with `- exp(...)`, which rustdoc's markdown parser
reads as an unindented list-item continuation). Fixed by reflowing the formula so no line starts
with `- ` (`clippy_fix.log` has the first run's own captured error text) -- not suppressed with
`#[allow]`. **Second run: clean, exit 0, 0 lines matching `^warning:|^error:`.** No `#[allow]`
added anywhere in this pass's own diff.

### 8.7 What remains

Nothing new from this pass beyond what section 7 (above) already lists -- this pass's own two jobs
are both complete. Item 13's own fixture (section 7, item 2 of the original list) is the one item
this pass's own scope touched without closing; left as-is, per the escalation above.
