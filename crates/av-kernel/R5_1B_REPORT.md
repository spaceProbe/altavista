# R5.1b: the IMU's SENSOR fault runtime, a SENSOR-faulted replay test, and question 187's helper

Task: `docs/open-questions.md` question 178 (the IMU half; the star tracker was R5.1a), plus
question 187 (the two-epoch trap in command bookkeeping) and a `crate::drm::replay` test for a
SENSOR-faulted run (R5.1a's own "what remains" items 3/4). Written incrementally: hypotheses and
expected values are stated in the source (module/test/fixture doc comments) before the
corresponding measurement, per the task's own instruction; this report cross-references those
in-source predictions rather than duplicating them verbatim.

## 1. What was built

### Job 1: the IMU's SENSOR fault runtime

- `crates/av-kernel/src/drm/sensors.rs`:
  - `ImuBiasChannel` (`Gyro`/`Accel`) and `ImuFaultEffect` (`Bias{channel,axis,value}`,
    `Dropout`, `Freeze`, `Scale{value}`) -- the IMU's own counterpart of `StarTrackerFaultEffect`,
    on the identical single-`Option`-slot machinery. `ImuSpec` gains `fault: Option<ImuFaultEffect>`
    (never a declared parameter, `None` at parse time, set only by `fault::apply_sensor_fault`).
  - `ImuFaultEffect::Bias`'s own doc comment states explicitly how a declared bias fault composes
    with the IMU's own propagated bias random walk (`state[0..6]`): the declared value is an
    ADDITIVE constant summed in only at the point the reported measurement is computed
    (`ImuModel::compute_measured_values`), never written into `state[0..6]` itself and never
    resetting/scaling the random walk -- the walk keeps evolving normally, fault installed or not
    (`step_with_ports`'s own per-period `random_walk_step3` calls stay unconditional, unchanged).
    Clearing the fault removes exactly the additive term; the random walk's own accumulated value
    is untouched.
  - `ImuModel` gains `frozen_measurement: RefCell<Option<[f64;6]>>` (the `Freeze` latch, the full
    `[wx,wy,wz,ax,ay,az]` sextet -- both triads share one physical packet, unlike the star
    tracker's single quaternion) and `fault_frames_affected`/`fault_first_effect_tai_ns` (`Cell`s,
    identical shape to `StarTrackerModel`'s own).
  - `ImuModel::compute_measured_values` (new): the raw six-component reported value -- `Bias` adds
    atop the declared channel/axis's own deviation term (bias random walk + white noise); `Scale`
    multiplies BOTH triads' combined deviation uniformly (`value == 1.0` a bit-exact no-op,
    asserted). Draws noise from the CALLER's already-mutably-borrowed `rng` (the same-iteration
    bias-random-walk borrow `step_with_ports` already holds) rather than re-borrowing --
    `StarTrackerModel::step_with_ports`'s own `Freeze`-borrow bug (R5.1a, Defects item 3) is the
    exact class this avoids by construction.
  - `ImuModel::step_with_ports`: `Dropout` suppresses the whole emission (no packet, no
    `Measurement`, `seq` untouched) exactly like the star tracker's; `Freeze` latches the FIRST
    full sextet computed after the fault epoch and re-emits it unchanged, `seq` still incrementing
    (the propagated bias random walk keeps advancing underneath the frozen output -- only the
    REPORTED values freeze).
  - `ImuModel::drain_sensor_fault_effect` (real implementation, replacing the `None` stub) --
    identical drain/clear contract to `StarTrackerModel`'s own.
  - `fault` included in `ImuModel::new`'s settings hash (and therefore `dynamics_hash`), for the
    identical "a fault-bounded re-materialization must always hash differently" reason
    `StarTrackerModel::new` already has.
  - Module doc comment's "SENSOR fault runtime" section rewritten to cover both models together
    (R5.1a's star-tracker-only framing retired) -- states the shared window/overlap-refusal
    contract, the `Freeze` "first, not last" reasoning extended to the IMU's own random-walk-keeps-
    advancing wrinkle, and cross-links `ImuFaultEffect::Bias`'s own composition doc comment.
  - 5 new unit tests: one per effect (`imu_bias_adds_a_fixed_offset_to_the_declared_channel_and_
    axis_when_noise_is_close_to_zero`, `imu_dropout_emits_no_packet_and_no_measurement_at_any_
    emission_instant`, `imu_freeze_latches_the_first_measurement_and_repeats_it_while_truth_keeps_
    changing`, `imu_scale_multiplies_the_deviation_from_truth_on_both_triads_and_one_is_exactly_a_
    no_op`) plus the drain/clear proof (`imu_drain_sensor_fault_effect_is_none_with_no_fault_and_
    clears_once_drained`). **The bias test could NOT reuse the star tracker's own `noise_sigma_rad
    == 0.0` exact-zero technique** -- see that test's own doc comment: an IMU `Measurement`'s own
    noise diagonal must be genuinely positive DEFINITE (`codec::measurements_from_field_values`'s
    Cholesky check), unlike the star tracker (which never builds one at all), so an exact-zero
    sigma panics (`MeasurementNoiseNotSpd`, measured directly before this fix, not assumed) --
    fixed by using a tiny (`1e-9`) nonzero sigma instead, 8-9 orders of magnitude below the
    declared bias, with a `1e-6` tolerance.
- `crates/av-kernel/src/drm/fault.rs`:
  - `apply_sensor_fault`/`clear_sensor_fault` gain `BindingPlan::Imu` arms (the same functions
    R5.1a built for the star tracker, now matching on `plan` instead of destructuring a single
    variant) -- `"bias"` maps `imu.gyro_bias.{x,y,z}`/`imu.accel_bias.{x,y,z}` to
    `(ImuBiasChannel, axis)`, `"dropout"`/`"freeze"` require `imu.output`, `"scale"` requires
    `imu.scale`. `validate_no_overlapping_sensor_fault_windows` needed NO change (already generic
    over any SENSOR fault regardless of which model it targets).
  - Module doc comment's "SENSOR" section and `SENSOR_KINDS`'s own doc comment rewritten to state
    the vocabulary is shared by both models, not star-tracker-only.
  - 5 new unit tests, mirroring the star tracker's own five: `apply_sensor_fault_bias_writes_the_
    declared_channel_axis_and_value_for_imu`, `apply_sensor_fault_dropout_and_freeze_write_the_
    declared_effect_for_imu`, `apply_sensor_fault_scale_writes_the_declared_value_for_imu`,
    `apply_sensor_fault_refuses_a_target_that_does_not_match_its_own_kind_for_imu`,
    `clear_sensor_fault_restores_fault_to_none_and_touches_nothing_else_for_imu`.
- `crates/av-kernel/src/drm/executor.rs`: the load-time SENSOR fault-validation loop's
  `ModelKind::Imu => {...refuse...}` arm merged into `ModelKind::StarTracker | ModelKind::Imu =>
  {...validate SENSOR_KINDS, both epochs against the sample grid...}` -- the IMU now gets the
  IDENTICAL validation the star tracker already had, not a separate copy. `RunProducts.
  measurements`'s own doc comment (`Vec<pb::Measurement>` field) rewritten -- its old claim ("a
  SENSOR-targeted fault can still never reach this field at all") is now false for both models and
  is corrected to state what genuinely happens (a `dropout` suppresses the `Measurement` along
  with the packet; `bias`/`freeze`/`scale` still produce a genuine, possibly-perturbed one).
- `crates/av-kernel/src/drm/mod.rs` (`DrmError`): `PortOrSensorFaultNotYetSupported` **deleted**
  (both the variant and its `Display` arm) -- once every SENSOR shape had a real runtime (both
  models, all four kinds), it became unreachable dead code, exactly as R4.1b deleted
  `PortFaultKindNotYetSupported` once every PORT kind had a real runtime (this is the task's own
  named precedent, followed exactly). `UnknownSensorFaultKind`'s own doc comment updated to say
  "both sensor models," not "the star tracker."
- `crates/av-kernel/tests/sensor_faults.rs`: the obsolete `a_sensor_fault_naming_the_imu_
  instance_is_still_refused_naming_r5_1b` test (asserted the now-deleted `DrmError::
  PortOrSensorFaultNotYetSupported`) replaced by two new load-time refusal tests mirroring the
  star tracker's own identical pair: `a_sensor_fault_naming_an_unrecognized_kind_on_the_imu_is_a_
  typed_load_error` (`DrmError::UnknownSensorFaultKind`) and `a_sensor_fault_start_epoch_off_the_
  sample_grid_on_the_imu_is_a_typed_load_error` (`DrmError::FaultEpochNotOnSampleGrid`).
- `ImuSpec { ... }` literal construction sites updated with `fault: None` at every call site the
  new field touched (found by grep, not guessed): `crates/av-kernel/src/drm/sensors.rs` (3 test
  sites + `parse_imu_spec`), `crates/av-kernel/src/drm/fault.rs` (`imu_spec()` test helper),
  `crates/av-kernel/src/drm/binding.rs` (`simple_imu_spec()` test helper), `crates/av-kernel/src/
  registry.rs` (one test).

### Job 2: replay of a SENSOR-faulted run, byte for byte

- `drms/demo_attitude_control_imu_bias.drm.yaml` -- new fixture, `demo_attitude_control.
  {sos,*.system}.yaml` reused UNCHANGED. A `"bias"` SENSOR fault on `imu.gyro_bias.z` (`value =
  0.001` rad/s, ~100x the declared `imu.gyro_noise_sigma`), window `[start+5s, start+35s)` --
  identical window to R5.1a's own star-tracker-dropout fixture, reused for the same "during the
  slew, error large and changing" reasoning.
- **The instance faulted is `imu`; the instance replayed is `startracker` -- a deliberate, fully
  disclosed substitution, not "replay the faulted instance" as a first reading of the task might
  suggest.** Investigated directly, not assumed: `crate::drm::replay::ReplayModel::drain_sensor_
  fault_effect` always returns `None` (that method's own doc comment, "No SENSOR fault runtime"),
  and `AnyModel::Replay` delegates straight to it -- so `crate::drm::executor::run_shared_group`'s
  own `sensor_fault_totals` accumulator, which gates whether a SENSOR fault's own `EVENT_KIND_
  FAULT` event is ever emitted, can NEVER receive a contribution from a replayed instance, for ANY
  SENSOR fault kind or window shape. This is a real, structural difference from a PORT fault
  (applied by the router, at delivery, to already-recorded pre-fault frames -- replay re-triggers
  it naturally) -- confirmed by an actual mechanical test (`t6b_`, below), not merely reasoned
  about. Faulting `imu` while replaying `startracker` (a genuinely different sensor the identical
  topology also wires to the controller) sidesteps this: `imu` is never wrapped in a `ReplayModel`
  in either run, so it re-executes for real, deterministically, in both, and its own FAULT event
  and physical effect reproduce correctly.
- `crates/av-kernel/tests/replay.rs`:
  - `t6_replaying_a_different_sensor_from_the_one_a_sensor_fault_targets_reproduces_the_whole_
    faulted_run_byte_identically` -- the headline claim, mirroring `t5_`'s own shape exactly:
    real run vs. replayed run (`startracker` named in `RunConfig.replay.instances`), asserts the
    ENTIRE encoded `RunProducts` matches byte for byte, nothing excluded. Its own doc comment
    states the full reasoning above BEFORE the test body, per the task's own instruction.
  - `t6b_replaying_the_same_instance_a_sensor_fault_targets_measurably_drops_that_faults_own_
    event` -- the measured evidence backing `t6_`'s own claim: replays `imu` itself (the SAME
    instance the fault targets) against the identical fixture and confirms, directly, that the
    real run's own single `EVENT_KIND_FAULT` for `"bias_imu"` has no counterpart in the replayed
    run's own event list (`assert!(replayed_fault_events.is_empty())`), and that the two runs are
    consequently NOT byte-identical (`assert_ne!`). This turns what would otherwise be an
    unverified claim in `t6_`'s own doc comment into a pinned, mechanically-confirmed test.
  - Module doc comment's opening enumeration extended to name T5/T6/T6b.

### Job 3: question 187, the two-epoch trap

- `crates/av-kernel/src/drm/command.rs`: `pub fn ack_emission_epoch(applied: i64, period: i64) ->
  i64` (`applied + period`) -- a small pure helper, doc comment states both epochs (`AppliedCommand
  .applied_tai_ns`, the consuming step's own START epoch, where ACKED lands; the ack packet's own
  REAL wire emission epoch, that step's own RESULT/end epoch) and their relation explicitly,
  cross-referencing the real call sites on both sides (`crate::drm::command::acked_event`'s own
  call in `executor.rs`, which uses `applied_tai_ns` alone; `ConstantAccelModel::step_with_ports`/
  `GmatFramedCommandModel::step_with_ports`'s own `outbox.push(port, result.t_tai_ns, payload)`,
  which is where the ack is actually emitted). One new unit test, `ack_emission_epoch_is_applied_
  plus_period`.
- **`executor.rs` itself has no call site computing this relation** -- searched directly (grepped
  the whole crate for `applied_tai_ns +`/`period`/`OUTPUT_PERIOD_NS` combinations, not guessed):
  `command::acked_event`'s own call (`executor.rs`) passes `cmd.applied_tai_ns` alone, correctly,
  never `+ period` (the ACKED transition genuinely lands at the START epoch, per the ratified
  rule) -- so there was nothing to change there. **The one genuine inline-arithmetic call site
  found and changed**: `crates/av-kernel/tests/port_traffic_sidecar.rs:192`, `let ack_tai_ns =
  applied_tai_ns + OUTPUT_PERIOD_NS;` (that test's own module doc comment already documents this
  as the exact spot an EARLIER draft of the same test was written against the wrong epoch and had
  to be root-caused -- the literal "two rounds running" question 187 itself names) -- replaced
  with `av_kernel::drm::command::ack_emission_epoch(applied_tai_ns, OUTPUT_PERIOD_NS)`.
  `crates/av-kernel/tests/port_faults.rs`'s own `baseline_applied.tai_ns + n_steps *
  OUTPUT_PERIOD_NS`/`baseline_ack_emission + n_steps * OUTPUT_PERIOD_NS` lines were inspected and
  left alone: they express a DIFFERENT relation (an N-step shift between two already-measured
  epochs from a delay fault, not the single-period applied-to-ack relation this helper models).
- `crates/av-dynamics/src/lib.rs`: `AppliedCommand.applied_tai_ns`'s own doc comment gains a new
  paragraph stating both epochs and their relation explicitly (mirrors `drms/README.md`'s new
  section, below, since `av-dynamics` cannot depend on `av-kernel` to link the helper directly).
- `drms/README.md`: new "Command bookkeeping: the two-epoch trap" section, stating both epochs,
  their relation, the real call sites on each side, and `ack_emission_epoch`'s own role as the one
  place the relation is written.

## 2. Verification

`export PATH="/opt/homebrew/opt/rustup/bin:$PATH"` first, per the environment rules. Contention
checked before every heavy run.

### Targeted (development)

- `cargo test -p av-kernel --lib drm::sensors::` -- 46 passed, 0 failed (was 41 at the R5.1a
  baseline; +5, exactly the predicted new IMU fault tests).
- `cargo test -p av-kernel --lib drm::fault::` -- 40 passed, 0 failed (was 35; +5).
- `cargo test -p av-kernel --lib drm::command::` -- 7 passed, 0 failed (was 6; +1).
- `cargo test -p av-kernel --test sensor_faults` -- 11 passed, 0 failed (was 10; net +1: -1
  obsolete IMU-refusal test, +2 new IMU load-refusal tests).
- `cargo test -p av-kernel --test replay` -- 10 passed, 0 failed (was 8; +2, T6/T6b).
- `cargo test -p av-kernel --test port_traffic_sidecar` -- 4 passed, 0 failed (unchanged count --
  only an existing test's own internal arithmetic changed, no test added/removed).
- `cargo test -p av-kernel --test drm_command` -- unchanged (no test added; ACKED's own existing
  assertion already correctly used `applied_tai_ns` alone).
- Regression sweep against everything this task's own changes touch (`fault.rs`/`sensors.rs`/
  `executor.rs`/`mod.rs`/`command.rs`, the shared boundary loop, `AnyModel`, replay):
  `cargo test -p av-kernel --test drm_attitude_control --test drm_attitude_sensors --test
  drm_executor --test port_faults --test replay --test sensor_faults --test port_traffic_sidecar
  --test drm_command` -- **74 passed, 0 failed** across all 8 files (9+6+4+16+14+4+10+11), saved
  to this session's own scratchpad, `sweep_test_run.log`.

**Predicted new total, stated before the full run:** R5.1a's own recorded baseline (this task's
own starting commit `e95485d`) is **834 passed, 0 failed, 1 ignored**. Net new tests this task
adds: `drm::sensors::` +5, `drm::fault::` +5, `drm::command::` +1 (all three `--lib`, 11 net),
`tests/sensor_faults.rs` net +1 (removed 1 obsolete, added 2), `tests/replay.rs` net +2 (T6, T6b).
**11 + 1 + 2 = 14. Predicted: 834 + 14 = 848 passed, 0 failed, 1 ignored.**

### Full run (the gate)

[Filled in once the full `cargo test -p av-kernel` run and `cargo test --workspace --exclude
av-kernel` complete -- see the final report message for the measured counts and saved log paths.]

### Clippy / cargo deny

[Filled in once run.]

## 3. Measurements worth keeping

**The IMU bias `Measurement`-noise-SPD constraint (a genuine, pre-existing, IMU-only property,
discovered while writing the bias unit test).** Unlike the star tracker (which never builds a
noise covariance for its own `Measurement` at all -- `MEASUREMENT_ID_STAR_TRACKER_Q4`'s own doc
comment, `r` deliberately left empty), `ImuModel::step_with_ports` always builds a genuine
`diag(sigma^2)` noise matrix for `codec::measurements_from_field_values`, and an EXACTLY-zero
sigma is positive SEMI-definite, not positive DEFINITE -- the Cholesky check inside that call
panics (`MeasurementNoiseNotSpd`), measured directly (not assumed) when the bias test's first
draft tried the star tracker's own `noise_sigma_rad == 0.0` technique verbatim. Not a defect this
task introduces or fixes -- `parse_imu_spec`'s own `require_positive` already refuses a declared
`gyro_noise_sigma`/`accel_noise_sigma` of `0.0` or less for exactly this class of reason; only a
direct `ImuSpec` struct literal (bypassing `parse_imu_spec`, as every unit test in this module
does) can reach it at all. Fixed in the test, not the model: `1e-9` sigma sidesteps the SPD check
while staying 8-9 orders of magnitude below the declared `0.5` bias.

**Job 2's own central finding, measured twice (once as `t6b_`'s own pinned assertion, once as
disclosed investigative evidence during authoring): a SENSOR-faulted instance cannot replay its
own fault event.** `crate::drm::replay::ReplayModel::drain_sensor_fault_effect` always returns
`None`; `crate::drm::executor::run_shared_group`'s `sensor_fault_totals` accumulator -- which
gates `EVENT_KIND_FAULT` emission for a SENSOR fault entirely -- therefore never fires for a
replayed instance's own SENSOR fault, regardless of kind or window shape. This is architecturally
different from a PORT fault (applied by the router, at delivery, to already-recorded pre-fault
frames -- replay re-triggers it naturally, `t5_`'s own proof). `t6b_` measures this directly:
replaying `imu` (the SAME instance `demo_attitude_control_imu_bias.drm.yaml`'s own `"bias"` fault
targets) drops that fault's own `EVENT_KIND_FAULT` from the replayed run's `RunProducts.events`
entirely -- confirmed with `run_replayed.events` carrying zero `EVENT_KIND_FAULT` entries for
`"bias_imu"` where `run_real.events` carries exactly one, and the two runs' encoded `RunProducts`
genuinely NOT byte-identical as a direct consequence. **Not the only cause of that mismatch, and
`t6b_`'s own doc comment says so explicitly** (an earlier draft of this report and that test's own
doc comment overstated it as the sole difference, caught and corrected before this report was
finalized): `ImuModel::state_dim() == 6` (the propagated bias random walk -- a REAL, populated
trajectory, confirmed by `crates/av-kernel/tests/drm_attitude_control.rs`'s own `trajectories
["imu"]` usage, not merely declared), unlike the star tracker's `0`, so replaying `imu` ALSO
replaces that real propagation with `ReplayModel`'s own zero-order hold and drops its own
`Measurement`s -- independent, compounding reasons `imu` is a poor replay subject at all, on top
of the fault-event gap specifically. This is why `t6_`'s own fixture faults `imu` (which cannot
honestly replay itself for at least three reasons) while replaying `startracker` (`state_dim() ==
0`, already proven a sound replay subject by T1).

**Break-and-restore evidence.** [See section 5 for the itemized list.]

## 4. Defects found, including my own

1. **My own test-authoring mistake, caught immediately, not shipped:** the first draft of the IMU
   bias unit test tried to reuse the star tracker's own `noise_sigma_rad == 0.0` exact-zero
   technique for a clean, deterministic proof. `ImuModel::step_with_ports` unconditionally builds
   a genuine `diag(sigma^2)` noise matrix for `codec::measurements_from_field_values` (unlike the
   star tracker, which never builds one), so an exactly-zero sigma is positive SEMI-definite, not
   DEFINITE -- `panicked at ...: MeasurementNoiseNotSpd`, measured directly. This is a real,
   pre-existing, IMU-only property (see "Measurements," above), not a defect in this task's own
   new code -- `parse_imu_spec`'s own `require_positive` already refuses a declared sigma of `0.0`
   for exactly this class of reason; only a direct `ImuSpec` struct literal (every unit test in
   this module) can reach it. Fixed in the test (a tiny `1e-9` sigma instead), not the model.
2. **No genuine implementation defect was found live during this task's own new code**, unlike
   R5.1a's own two (the `Freeze` `RefCell` double-borrow panic, the drain-accumulator overwrite
   bug) -- `ImuModel::compute_measured_values` was deliberately written to take the caller's
   already-borrowed `rng` as a parameter (rather than re-borrowing `self.rng` internally, the
   exact shape of R5.1a's own `Freeze` bug) specifically to avoid reproducing that class of
   mistake, and every one of the four IMU fault effects, the drain/clear machinery, and both
   replay tests passed on their first real run once written -- confirmed, not assumed, by the
   break-and-restore cycles below (which prove the tests would have caught a mistake, had one been
   made, rather than merely arguing they would).
3. **Job 2's own central finding is a genuine, pre-existing architectural property, not a defect
   introduced by this task** -- see Escalation 1: a SENSOR-faulted instance cannot replay its own
   fault event, because `ReplayModel::drain_sensor_fault_effect` (built in R5.1a, unchanged here)
   always returns `None`. Listed here for visibility since it IS a real gap, even though it
   predates this round and this round did not introduce it.

## 5. Break-and-restore evidence

Every new test was confirmed to fail against a nameable wrong implementation, then the
implementation was restored; a pre-edit snapshot of every touched file was taken first
(`cp` into this session's own scratchpad) and `cmp`-verified byte-identical after every restore.

1. **`sensors.rs`: IMU `Bias` writes into the wrong channel/axis** (`gyro_dev[0] += value`
   unconditionally, `channel`/`axis` ignored). **Executed:** `imu_bias_adds_a_fixed_offset_to_the_
   declared_channel_and_axis_when_noise_is_close_to_zero` -- `panicked at crates/av-kernel/src/
   drm/sensors.rs:2253:13: component 0: got 5.000e-1, expected 0.000e0 ...` (the declared
   accel-z bias landed on gyro-x instead). Restored; `cmp` confirmed byte-identical; re-run green.
2. **`sensors.rs`: IMU `Dropout` never suppresses the emission** (the `if matches!(...Dropout)`
   guard disabled via `if false && ...`). **Executed:** `imu_dropout_emits_no_packet_and_no_
   measurement_at_any_emission_instant` -- `panicked at crates/av-kernel/src/drm/sensors.rs:2275:
   13: dropout must emit no packet`. Restored; `cmp` confirmed byte-identical; re-run green.
3. **`sensors.rs`: IMU `Freeze` re-computes a fresh measurement every time** (the frozen-value
   branch removed, `compute_measured_values` always called unconditionally). **Executed:** `imu_
   freeze_latches_the_first_measurement_and_repeats_it_while_truth_keeps_changing` -- `panicked at
   crates/av-kernel/src/drm/sensors.rs:2303:13: assertion left == right failed: emission 1 must
   repeat the FIRST measurement exactly ...` (each emission carried the true, changing rate
   instead of the latched first value). Restored; `cmp` confirmed byte-identical; re-run green.
4. **`sensors.rs`: IMU `Scale` never multiplies** (the `gyro_dev`/`accel_dev` scaling lines
   removed, the `if let Some(Scale)` match becomes a no-op). **Executed:** `imu_scale_multiplies_
   the_deviation_from_truth_on_both_triads_and_one_is_exactly_a_no_op` -- `panicked at crates/
   av-kernel/src/drm/sensors.rs:2344:13: component 0: scaled deviation=9.373e-5 expected 2x
   baseline deviation=1.875e-4` (the 2x-scale run came back numerically identical to the
   unfaulted baseline). Restored; `cmp` confirmed byte-identical; re-run green.
5. **`sensors.rs`: `ImuModel::drain_sensor_fault_effect` never clears its own accumulator** (the
   two `.set(0)`/`.set(None)` reset lines removed). **Executed:** `imu_drain_sensor_fault_effect_
   is_none_with_no_fault_and_clears_once_drained` -- `panicked at crates/av-kernel/src/drm/
   sensors.rs:2366:9: a second, immediate drain with nothing newly affected must be empty, not a
   repeat`. Restored; `cmp` confirmed byte-identical; re-run green.
6. **`fault.rs`: `apply_sensor_fault`'s IMU `"bias"` arm accepts any target** (the `match fault.
   target.as_str()` replaced with an unconditional `(Gyro, 0)`). **Executed:** `apply_sensor_
   fault_refuses_a_target_that_does_not_match_its_own_kind_for_imu` -- `panicked at crates/
   av-kernel/src/drm/fault.rs:1020:95: called Result::unwrap_err() on an Ok value: Imu(ImuSpec {
   ..., fault: Some(Bias { channel: Gyro, axis: 0, value: 1.0 }) })` (a `"bias"` fault naming
   `imu.scale` was silently accepted instead of refused). Restored; `cmp` confirmed
   byte-identical; re-run green.
7. **`executor.rs`: the load-time loop never distinguishes IMU from star tracker any more, in the
   OTHER direction** (the merged `ModelKind::StarTracker | ModelKind::Imu` arm split back into a
   no-op `ModelKind::Imu => {}` and a `ModelKind::StarTracker => {...full checks...}` -- i.e. the
   IMU's own validation silently skipped again). **Executed against BOTH new load-refusal tests:**
   `a_sensor_fault_naming_an_unrecognized_kind_on_the_imu_is_a_typed_load_error` -- `panicked at
   crates/av-kernel/src/drm/fault.rs:507:26: apply_sensor_fault called with kind "not_a_real_
   kind"; executor::execute's own load-time validation against fault::SENSOR_KINDS guarantees only
   bias/dropout/freeze/scale ever reach here` (an untyped panic, not the typed `DrmError::
   UnknownSensorFaultKind` the test expects); `a_sensor_fault_start_epoch_off_the_sample_grid_on_
   the_imu_is_a_typed_load_error` -- `panicked at crates/av-kernel/src/kernel.rs:718:9: run
   horizon (1500000000 ns) is not an exact multiple of the output period (1000000000 ns)` -- the
   IDENTICAL untyped, lower-level panic R5.1a's own break-and-restore item 11 found for the star
   tracker's own end-epoch check, now confirmed for the IMU's own start-epoch check too. Restored;
   `cmp` confirmed byte-identical; both re-runs green (`cargo test -p av-kernel --test
   sensor_faults` -- 11 passed).
8. **`command.rs`: `ack_emission_epoch` returns `applied` alone, dropping `period`** (`applied +
   period` -> `applied`, mirroring the ACTUAL historical mistake question 187 names). **Executed
   against BOTH the helper's own unit test and the real integration test that depends on it:**
   `ack_emission_epoch_is_applied_plus_period` -- `panicked at crates/av-kernel/src/drm/
   command.rs:366:9: assertion left == right failed left: 1700000052000000000 right:
   1700000053000000000`; `crates/av-kernel/tests/port_traffic_sidecar.rs::demo_command_sidecar_
   records_match_an_independent_reconstruction_from_fixtures_and_events` -- `panicked at crates/
   av-kernel/tests/port_traffic_sidecar.rs:199:5: assertion left == right failed: the ack's own
   real emission epoch (the step's RESULT epoch) is exactly 3 s of real router latency after
   dispatch ... left: 1700000052000000000 right: 1700000053000000000` -- exactly "a test which
   used the wrong epoch now fails," the task's own named acceptance bar for this job. Restored
   (this file predates the Job 3 additions, so the snapshot could not be used directly -- the break
   was manually reverted to the exact pre-break text, then re-verified green and re-snapshotted);
   both re-runs green.
9. **`replay.rs`: `ReplayModel::step_with_ports`'s own emission epoch drops `dt_ns`** (`t_tai_ns +
   dt_ns` -> `t_tai_ns`, an off-by-one-kernel-step shift in every replayed frame's own epoch).
   **Executed:** `t6_replaying_a_different_sensor_from_the_one_a_sensor_fault_targets_reproduces_
   the_whole_faulted_run_byte_identically` -- `panicked at crates/av-kernel/src/schedule.rs:554:
   17: assertion left == right failed left: 1767225637000000000 right: 1767225637100000000` (an
   internal invariant assertion inside the shared scheduler caught the epoch mismatch before the
   test's own final `RunProducts` comparison was even reached). Restored; `cmp` confirmed
   byte-identical; both `t6_`/`t6b_` re-runs green.

## 6. Escalations for the manager

1. **A SENSOR-faulted instance cannot replay its own fault event (Job 2's own central finding,
   measured, not merely reasoned about).** `crate::drm::replay::ReplayModel::drain_sensor_fault_
   effect` always returns `None`; `crate::drm::executor::run_shared_group`'s `sensor_fault_totals`
   accumulator -- which gates SENSOR-fault `EVENT_KIND_FAULT` emission entirely -- therefore never
   fires for a replayed instance's own SENSOR fault, for ANY kind or window shape (unlike a PORT
   fault, which the router re-applies naturally at delivery to already-recorded pre-fault frames).
   Measured directly: `crates/av-kernel/tests/replay.rs::t6b_replaying_the_same_instance_a_sensor_
   fault_targets_measurably_drops_that_faults_own_event`. Not fixed here -- fixing it would need
   either (a) a sensor-model-specific fault-effect reconstruction inside `ReplayModel` itself
   (which this module's own doc comment already disclaims -- "a generic, content-agnostic
   binding," deliberately no per-sensor knowledge), or (b) deriving `frames_affected`/`first_
   effect_tai_ns` for a replayed SENSOR-faulted instance directly from the replay log's own
   recorded frames within `run_shared_group` (nontrivial: needs the sensor's own declared period,
   only available from the `rebound` model this path already discards after reading `describe()`/
   `state_dim()`) -- both real, invasive design decisions beyond this task's own charter
   (Job 1: IMU faults; Job 2: one replay test; Job 3: the epoch helper). `drms/README.md`'s own
   new section documents this plainly for the next worker who reaches for it.
2. **Job 2's own fixture faults `imu` while replaying `startracker` -- a deliberate substitution
   from a literal "replay the faulted instance" reading, disclosed exactly like T1b's own `demo_
   attitude_sensors`-vs-`demo_attitude_control` substitution.** Recommend the manager/lead
   explicitly ratify this reading (mirrors T1b's own escalation precedent) or direct that
   escalation 1's own gap be closed first, if a literal same-instance replay is required for this
   round to be considered complete.
3. Every escalation from R5.1a's own report (`R5_1A_REPORT.md`, section 6) is unaffected by this
   round and still stands -- not re-litigated here. Item 4 of that report's own "what remains"
   ("A `crate::drm::replay` test replaying the star-tracker instance of a SENSOR-faulted run...
   has not been checked against a replayed instance directly") is now resolved into this round's
   own escalation 1 above, a concrete, measured finding rather than an open question.

## 7. What remains, in priority order

1. **Escalation 1** (SENSOR fault + replay of the SAME instance) -- a decision for the manager/
   lead: fix (and how), formally accept and document as a permanent limitation, or defer.
2. **`Router::deliver`'s per-Outbox epoch-stamping confound** (R5.1a's own escalation 1, question
   189, explicitly assigned to a LATER round per question 189's own ratification -- not this
   round's to touch, listed here only for continuity).
3. A dedicated "a SENSOR fault whose window never actually fires produces no event" fixture for
   the IMU, mirroring R5.1a's own disclosed, not independently pinned, item 13 (the star tracker's
   identical gap) -- neither model has this fixture; still disclosed for both.
