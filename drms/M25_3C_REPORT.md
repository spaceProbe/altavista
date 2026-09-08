# M25.3c report (kernel only)

Incremental report, written as work happens, not at the end. Sections are appended in the order
work was done; each states a hypothesis/expected value before the corresponding measurement where
one applies (standing rule).

## Starting state (discovery pass, before any edits)

- `git status`: branch `develop`, up to date with `origin/develop`. Only untracked file:
  `crates/av-kernel/tests/demo_measurements.rs`. No other local modifications.

### Piece 1 (positive test) -- discrepancy from the brief

The brief describes `crates/av-kernel/tests/demo_measurements.rs` as "currently ... a single
`#[test] fn debug_dump` that only `eprintln!`s." That is **not** what is on disk. The file on disk
(251 lines) is already a complete, real test module:
- Module doc comment states the exact hypothesis (measurement ids, count=36, epochs, sensor_id,
  frame_id, `r` shape, order) *before* any run, and says that hypothesis was checked against a
  throwaway `eprintln!` dump "since deleted" -- i.e. the debug-dump step the brief describes
  already happened and was already replaced.
- a single test fn `demo_measurements_drm_decodes_real_measurements_through_execute` (line 204)
  that runs the DRM through `av_kernel::drm::execute` and asserts: exact count (36 =
  12 star-tracker + 12*2 IMU), per-epoch id order `[ID_ATTITUDE_Q4, ID_IMU_ACCEL3, ID_IMU_GYRO3]`
  (proving `(epoch_ns, measurement_id)` sort), exact `epoch_ns` per `k`, `sensor_id`
  (`"startracker"`/`"imu"`), `frame_id` empty, `z.len()` per id, `r` empty for the star tracker and
  bit-exact `diag(sigma^2)` for both IMU ids, plus a statistically-bounded `z`-vs-truth check with
  disclosed, derived (not tuned) tolerances.
- No `debug_dump`, no bare `eprintln!` test (`grep -n "debug_dump\|eprintln\|#\[test\]"` confirms:
  only one hit for `eprintln` and it is inside a doc comment recounting history, and the only
  `#[test]` is the real one).

**Conclusion:** piece 1's deliverable already exists on disk and appears to satisfy every stated
requirement. I have not yet run it (see Gates section below -- it will run as part of the `-p
av-kernel` gate, and I will also try a targeted single-test run first). I am treating this file as
essentially complete pending that run, and will only touch it if the run surfaces a problem. This
is a factual correction to the brief's description of the file's starting state, disclosed per the
standing rule, not a reason to stop -- the rest of the task is unaffected and proceeds.

### Piece 4 (Q176) -- discrepancy from the brief

The brief says: "this team already has port-fault / router-drop machinery (look at
`crates/av-kernel/src/drm/fault.rs` and the existing port-fault DRM tests for the declared way to
drop a port's traffic); use the declared mechanism, do not hand-roll one."

Checked this directly. `crates/av-kernel/src/drm/fault.rs` defines `PORT_KINDS = ["drop", "delay",
"corrupt", "duplicate"]` (the vocabulary `Fault.kind` accepts for `FAULT_TARGET_KIND_PORT`), but
`realize_unapplied_fault` -- the only thing that runs for a PORT-targeted fault today -- always
returns `DrmError::FaultTargetKindNotSupported` (`fault.rs:221`). This is confirmed, not
speculative:
- `fault.rs`'s own module doc comment: "ADR-005's PORT/SENSOR bindings are still Planned" / "this
  crate has no router/sensor-model runtime to apply it to yet."
- `fault.rs::port_fault_targets_are_seeded_but_not_yet_applied` and
  `crates/av-kernel/tests/faults_seeded.rs::every_documented_port_kind_is_accepted_but_not_yet_applied`
  (names paraphrased from their assertions) both assert exactly
  `DrmError::FaultTargetKindNotSupported { target_kind: "FAULT_TARGET_KIND_PORT", .. }` for every
  one of the four documented kinds, `"drop"` included.

So a `Fault { target_kind: Port, kind: "drop" }` does not drop a packet at runtime -- it is a typed
refusal at fault realization, i.e. it would make a DRM using it fail to execute at all, not
exercise the "packet dropped, measurement still present" path Q176 wants tested.

What genuinely IS a real, already-implemented, already-tested router-level drop in this codebase:
`crates/av-kernel/src/router.rs`'s own documented behavior, "a message on a port with no matching
connection is dropped, not an error" (module doc comment; concrete test
`a_message_on_a_port_with_no_matching_connection_is_dropped_not_an_error`). This is the actual
"declared mechanism" available for dropping a port's traffic in this codebase today.

**Decision:** rather than stop all four pieces of work over this one sub-claim, I am disclosing
the discrepancy here (per the standing rule) and, for the router-drop test specifically, using the
real router "no matching connection => silently dropped" path instead of the non-functional
`FAULT_TARGET_KIND_PORT` fault path the brief pointed at. This reuses existing, tested router
code, not a hand-rolled mechanism. If the lead considers this substitution unacceptable, that is
easy to strike and redo once real PORT fault support exists -- flagging clearly rather than
silently swapping the mechanism.

**Update:** while implementing, `crates/av-kernel/tests/demo_measurements.rs` was found to
*already* contain a second test, `a_declared_port_drop_fault_against_the_star_tracker_instance_has_no_effect_on_measurements_today`,
independently reaching the identical conclusion (declaring a real `Fault{target_kind: Port, kind:
"drop"}` and proving, by actually running it, that it changes nothing about `RunProducts.
measurements` -- pinning the current no-op as current behavior, for the lead to escalate). This
corroborates the finding above from a second angle and is complementary to, not a duplicate of,
the `decoded_at` test I added (see below) -- I left it in place unmodified.

## Note on workspace state during this task

Partway through, `git diff`/`cargo check` revealed that several files this task also needed to
touch (`crates/av-dynamics/src/lib.rs`, `crates/av-dynamics/src/stm.rs`,
`crates/av-kernel/tests/ports_router.rs`, and part of
`crates/av-kernel/tests/demo_measurements.rs` itself) already carried real, correct, uncommitted
work addressing the same requirements before I edited them -- e.g. `DynamicsModel::
last_measurements` was already a required (no-body) trait method with an updated doc comment, and
`av_dynamics::erase::ErasedModel`'s own wrapper-delegation test (`erased_model_reaches_inner_
last_measurements`) already existed, matching this task's own requirement almost verbatim. I did
not author these; I verified them (see the break-and-restore evidence for
`erased_model_reaches_inner_last_measurements` below), left them as-is where correct, and filled
in every gap `cargo check --workspace --all-targets` still reported. Several unrelated, out-of-scope
files also appeared as modified/untracked over the course of this session (`web/js/*`,
`tests/test_viewer_timeline.py`, `drms/M25_3_REPORT.md` [a *different* report file than this one --
M25.3, not M25.3c], `docs/open-questions.md`) -- frontend/viewer work, not kernel, evidently another
concurrent session's activity in this same working tree. Not touched, per this task's "kernel only"
scope. One exception worth noting: `crates/av-kernel/src/drm/executor.rs` picked up a 10-line
doc-comment-only addition to `RunProducts.measurements` describing exactly this task's own drop
semantics finding (referencing `demo_measurements.rs`'s "Work item 3" section, which contains both
my `decoded_at` test and the concurrently-added PORT-drop-fault-is-a-no-op test) -- additive
documentation only, no code, no conflict with anything here.

## Piece 2: Defect D1 (silent drop in `measurements_from_field_values`)

**Hypothesis, before editing:** `crates/av-kernel/src/codec.rs`'s
`let Some(FieldValue::Numeric(v)) = values.get(&field.name) else { continue; };` (around line 663)
silently skips a targeted field (non-empty `PacketField.target`) whose value is either absent from
`values` or is `FieldValue::Bytes` -- both should become a typed `CodecError`, while an
empty-`target` field must keep skipping silently (that is the declared, correct "recorded but not
mapped" case, not a bug).

**Fix:** added two new `CodecError` variants next to `MeasurementNoiseNotSpd`:
`MeasurementFieldValueMissing { codec_id, field }` and `MeasurementFieldValueNotNumeric { codec_id,
field }`, with `Display` impls following the existing style. Replaced the silent-`continue` with
`values.get(...).ok_or_else(...)?` plus an explicit match on `FieldValue::Numeric`/`::Bytes` that
returns the new typed error instead of dropping. Updated `measurements_from_field_values`'s own doc
comment (previously documented the silent skip as intended for *all three* cases; now states the
empty-target case is still silent by design, and the other two are typed errors).

**Tests added** (`crates/av-kernel/src/codec.rs`, `mod tests`):
- `a_targeted_field_with_no_value_at_all_is_a_typed_error_not_a_silent_drop`
- `a_targeted_field_with_a_bytes_value_is_a_typed_error_not_a_silent_drop`
- `an_empty_target_field_is_still_skipped_silently_alongside_targeted_fields` (regression: proves
  the empty-target case is unaffected, even sitting next to targeted fields and a `Bytes` value)

**Break-and-restore evidence:** reverted the fix to the original `let Some(FieldValue::Numeric(v))
... else { continue }` line and re-ran the two new failure-path tests:
```
thread '...a_targeted_field_with_no_value_at_all_is_a_typed_error_not_a_silent_drop' panicked:
  called `Result::unwrap_err()` on an `Ok` value: [Measurement { measurement_id: "imu.gyro3",
  z: [1.0, 3.0], ... }]
thread '...a_targeted_field_with_a_bytes_value_is_a_typed_error_not_a_silent_drop' panicked:
  called `Result::unwrap_err()` on an `Ok` value: [Measurement { measurement_id: "imu.gyro3",
  z: [1.0, 3.0], ... }]
test result: FAILED. 0 passed; 2 failed
```
Both fail exactly as predicted (the old code silently drops `wy` and returns `Ok` with a
2-component `z`). Restored the fix; re-ran -- `2 passed; 0 failed` (plus the regression test,
separately: `1 passed; 0 failed`).

## Piece 3: Defect D2 (forbidden trait default on `last_measurements`)

**Hypothesis, before editing:** `av_dynamics::DynamicsModel::last_measurements` has a `Vec::new()`
default body, violating this team's "no trait default that returns a valid empty result" rule.
Removing the body (making it required) will break compilation at every `impl DynamicsModel for X`
site that does not already override it; `cargo check --workspace --all-targets`'s own error list is
the authoritative, exhaustive inventory (more reliable than a manual `grep`, which -- confirmed --
misses generic impls like `impl<M: DynamicsModel> DynamicsModel for StmAugmented<M>`).

**What I found already done** (see "Note on workspace state" above): the trait method itself was
already required, with an updated doc comment, in the working tree before I touched it, along with
several impls (`av-dynamics::stm::StmAugmented`/its two test models, `av-dynamics::lib::
ConstantAccel` test model, `av-kernel::tests::ports_router` mocks) and one full wrapper-delegation
test (`av_dynamics::erase::tests::erased_model_reaches_inner_last_measurements`, extending the
existing `AllOverridden` test model with a distinctive marker measurement).

**What I added**, driven entirely by `cargo check --workspace --all-targets`'s own error list,
iterated to zero errors:
- `crates/gmat-sys/src/model.rs::GmatModel` -- `Vec::new()` (no measurement path wired to any
  GMAT-backed model).
- `crates/av-kernel/src/drm/attitude.rs::AttitudeWheelsModel` -- `Vec::new()` (no declared codec).
- `crates/av-kernel/src/drm/sensors.rs::TruthBroadcastAttitude<M>` -- delegates
  (`self.inner.last_measurements()`), matching its own "every other method delegates unchanged"
  doc comment.
- `crates/av-kernel/src/drm/controller.rs::AttitudeControllerModel` -- `Vec::new()` (decodes
  telemetry for its own control law only; emits a command packet, never through
  `measurements_from_field_values`).
- `crates/av-kernel/src/drm/controller.rs::CommandedAttitude<M>` -- delegates (same "every other
  method delegates unchanged" pattern as `TruthBroadcastAttitude`).
- `crates/av-kernel/src/drm/controller.rs::RecordingModel` (test-only) -- `Vec::new()`.
- `crates/av-kernel/src/drm/gmat_command.rs::GmatFramedCommandModel` -- delegates to
  `self.inner.last_measurements()` (wraps a `GmatModel`).
- `crates/av-kernel/src/drm/ground.rs::GroundStationModel` -- `Vec::new()` (neither declared codec
  carries a `target`).
- `crates/av-kernel/src/drm/binding.rs::ConstantAccelModel` -- `Vec::new()` (its `emit_framed`
  codec declares no `target`).
- `crates/av-kernel/src/drm/binding.rs::ContainerModel` -- `Vec::new()` (`lockstep.proto`'s
  `LockstepStepResponse` has no `Measurement` concept).
- `crates/av-kernel/src/drm/binding.rs::SharedContainerModel` -- delegates (`self.0.
  last_measurements()`), matching its own "plain passthrough wrapper... delegate every method"
  doc comment.
- Nine test-only mock models across `crates/av-kernel/src/kernel.rs` (`ConstantAccel`,
  `ConstantAccelStm`, `OutputtingAccel`, `Rotator`, `HeteroRotator`, `ZeroDimSystem`) and
  `crates/av-kernel/src/schedule.rs` (`ConstantAccel`, `Rotator`, `ZeroDim`, `AlwaysFails`) --
  all `Vec::new()` with a one-line "test-only, never emits telemetry" comment.

**`AnyModel` (`crates/av-kernel/src/drm/binding.rs`):** the 7-arm `last_measurements` match and its
doc comment were already present/updated in the working tree (delegating every variant explicitly,
`AnyModel::Gmat`/`ConstantAccel`/`Attitude`/`Controller`/`StarTracker`/`Imu`/`GroundStation`, no
catch-all). The executable arm-count symmetry check
(`any_model_arm_count_for_ground_station_matches_star_tracker`, comparing literal
`AnyModel::StarTracker(`/`AnyModel::GroundStation(` occurrence counts via `include_str!`) still
passes -- ran it directly: `1 passed; 0 failed`. No new arm was added (the arm already existed), so
this count is unaffected by my changes.

**What was still missing and I added:** a real delegation *test* exercising `AnyModel::
last_measurements` itself (only the arm existed; nothing called it). Added
`any_model_last_measurements_delegates_to_the_star_tracker_variant` (`binding.rs`, in the same test
block as the existing `any_model_step_with_ports_delegates_to_the_star_tracker_variant`): feeds the
identical real truth inbox that test does, calls `step_with_ports` to populate a genuine
(non-fixture) measurement, then reads it back through `AnyModel::last_measurements()` and asserts
`len() == 2` (2 Hz over a 1 s step) with `measurement_id == "altavista.attitude_q4"` on both.

**Break-and-restore for the new `AnyModel` test:** changed the `StarTracker` arm to
`AnyModel::StarTracker(_m) => Vec::new()`:
```
assertion `left == right` failed: 2 Hz declared rate over a 1s kernel step = 2 emissions...
  left: 0
 right: 2
test result: FAILED. 0 passed; 1 failed
```
Restored; re-ran -- `1 passed; 0 failed`.

**Break-and-restore for the pre-existing `ErasedModel` test** (verifying, since I did not author
it): changed `ErasedModel::last_measurements` to `Vec::new()` instead of
`self.inner.last_measurements()`:
```
assertion `left == right` failed: must reach AllOverridden::last_measurements's own override, not
the wrong empty result
  left: 0
 right: 1
test result: FAILED. 0 passed; 1 failed
```
Restored; re-ran -- `1 passed; 0 failed`.

**No `#[allow]`, no catch-all match arms anywhere added.** `cargo check --workspace --all-targets`
is clean (0 errors, 0 warnings) after every site above was filled in.

## Piece 4: Question 176 (`decoded_at`)

`av_cdm::pb::Measurement` does declare `map<string, string> meta = 8;`
(`proto/altavista/v1/core.proto:266`) -- confirmed before writing any code, per the brief's own
escalate-if-absent instruction. No proto change needed; no escalation required.

**Fix:** in `crates/av-kernel/src/schedule.rs`'s `HeteroScheduler::advance_to_with_ports`, in the
same loop that already stamps `measurement.sensor_id = id.clone()` (right after `sys.model.
last_measurements()`, before `router.deliver`), added
`measurement.meta.insert("decoded_at".to_string(), id.clone());`. Both fields get the *same* value
today (the emitting instance) because no receiver-side decode exists yet (question 176 explicitly
scopes that to a later task) -- `sensor_id` and `decoded_at` are kept as two distinct fields anyway
because they will diverge once a receiver-side `Measurement` exists (its `sensor_id` names the
model the measurement is *about*; its `decoded_at` would name the instance that decoded *it*, a
different one).

**Test added** (`crates/av-kernel/tests/demo_measurements.rs`):
`dropped_measurement_packets_still_decode_at_the_emitter_with_decoded_at_naming_it`. Reuses the
existing `demo_measurements` fixture as-is (no new DRM/system files): `drms/demo_measurements.sos.
yaml`'s 14 declared `Connection`s route only the 7 truth SIGNAL ports into `startracker`/`imu`;
neither sensor's own declared FRAMED OUT measurement port (`st_meas`/`imu_meas`) is named by any
`Connection` at all, so all 36 measurement packets are dropped **immediately** by `crate::router::
Router::deliver`'s own documented, already-tested rule ("a message on a port with no matching
connection is dropped, not an error") -- confirmed via `products.dropped_in_flight_messages == 0`
(these are not merely late/in-flight, they are never routed). The test asserts all 36 measurements
still appear and every one carries `meta["decoded_at"] == sensor_id` (the emitting instance).

**Break-and-restore:** removed the `meta.insert(...)` line and re-ran:
```
assertion `left == right` failed: measurement "altavista.attitude_q4" at
epoch_ns=1767225637500000000: decoded_at must name the emitting instance ("startracker")...
  left: None
 right: Some("startracker")
test result: FAILED. 0 passed; 1 failed
```
Restored; re-ran the whole file -- `3 passed; 0 failed` (the new test, the pre-existing positive
test, and the pre-existing `a_declared_port_drop_fault_against_the_star_tracker_instance_has_no_
effect_on_measurements_today`, all green).

**Did not implement the receiver side** (explicitly out of scope per the lead's decision, quoted
in the brief).

## Gates

### `cargo build --workspace --all-targets`
Clean. `Finished dev profile [unoptimized + debuginfo] target(s) in 1m 07s`, 0 errors, 0 warnings.

### `cargo test --workspace --exclude av-kernel` -- baseline 178 passed
First run (piped through `tail -100`, a mistake -- truncated the earlier crates' output) showed a
failure: `av-lockstep`'s `docker_lifecycle::docker_image_lifecycle_pull_by_digest_run_bind_reset_
shutdown_stop_remove` failed with `docker rm ...: removal of container ... is already in progress`.
This crate/test is entirely unrelated to every change in this task (D1/D2/Q176 touch `av-kernel`,
`av-dynamics`, `gmat-sys`; nothing in `av-lockstep`). Per the standing rule ("a contended gate
result is not a result"), re-ran the full suite twice more, both times capturing complete,
untruncated output with `--no-fail-fast`: **179 passed, 0 failed** both times, `docker_lifecycle`
included (its two tests passed cleanly in 96.87 s the second time). The first failure is a
contended/flaky Docker daemon state issue (a container removal racing something else on the host),
not a regression -- confirmed by two clean re-runs, reported as a flake, not a defect.
`179 = 178 (baseline) + 1` -- the one new `av_dynamics::erase::tests::erased_model_reaches_inner_
last_measurements` test (see Piece 3 above; already present, uncommitted, before I started editing
that file).

### `cargo test -p av-kernel` ALONE -- baseline 719 passed, 0 failed, 1 ignored
**Announced before starting, per this task's own host-quirk rule (this is the heavy run; several
minutes). Confirmed no other cargo process running first (`ps aux` -- one other session's `cargo
test --workspace --exclude av-kernel` was still finishing; waited for it to exit before starting
this one, so this result is not contended). Ran alone, nothing else concurrently for its whole
duration.**

**Result: 734 passed, 0 failed, 1 ignored** (32 test binaries, `lib.rs` unittests plus every file
under `crates/av-kernel/tests/`). The 1 ignored is
`byte_identical_port_traffic_between_posix_container_and_renode` with reason string "question 171:
Renode port traffic beyond STEP 1 does not deliver; verified posix-container-only until resolved"
-- exactly the required question-171 Renode reproducer, still ignored, untouched.

**Reconciling the count against the stated baseline (719):** I can concretely attribute **+8** new
tests to this task's own uncommitted work: `erased_model_reaches_inner_last_measurements` (1,
`av-dynamics::erase`, already present before I touched the file), three D1 tests (`codec.rs`), one
`AnyModel` delegation test (`binding.rs`), and all three tests in the wholly-new
`crates/av-kernel/tests/demo_measurements.rs` (which does not exist in git HEAD at all, so all
three count as new). Verified by diffing `#[test]` counts between `git show HEAD:<file>` and the
working tree for every one of the 15 files this task's own `git status` shows touched -- only
`erase.rs` (15->16), `codec.rs` (25->28), and `binding.rs` (136->137) changed count; every other
file's `#[test]` count is unchanged (only `last_measurements` impl bodies were added, no new test
functions). `719 + 8 = 727`, not `734` -- a further **+7** I cannot attribute to any file this task
touched. Given this branch is shared with at least one other concurrent session (see "Note on
workspace state" above) and I did not verify the 719 baseline against this exact `develop` HEAD
commit myself before starting, I am reporting the discrepancy rather than papering over it: the
measured, current, real result is **734 passed, 0 failed, 1 ignored**, and it is possible the
stated 719 baseline was measured at a slightly different point than this task's actual starting
HEAD. Either way, the number that matters for this gate -- **0 failed** -- holds.

### `cargo clippy --workspace --all-targets -- -D warnings`
First attempt (after waiting for a concurrent session's own identical `cargo clippy` invocation to
finish, per "never run concurrently with anything else you start") failed:
```
error: the loop variable `i` is used to index `truth_omega`
   --> crates/av-kernel/tests/demo_measurements.rs:184:22
```
inside `assert_z_consistent_with_truth`'s `ID_IMU_GYRO3` arm -- pre-existing code I did not author.
Before I could fix it, the concurrent session (independently, apparently reacting to the identical
clippy failure on their own run) rewrote it to a `zip`-based loop. Re-read the file, confirmed the
fix was already correct and complete, then re-ran clippy fresh (again after waiting for that
session's own `cargo test -p av-kernel --test demo_measurements` to clear first): **clean, 0
warnings, 0 errors.** No `#[allow]` added anywhere by me; no `clippy::large_enum_variant` was hit
(the two new `CodecError` variants are plain `{ codec_id: String, field: String }`, no boxing
question arises).

### `cargo deny check`
Clean: `advisories ok, bans ok, licenses ok, sources ok`, exit code 0, 0 errors.

### Final re-confirmation
Re-ran `cargo build --workspace --all-targets` once more at the end (given the amount of
concurrent activity in this shared working tree over the course of the task) -- still clean, 0
errors, 0 warnings, nothing to rebuild. Re-ran `crates/av-kernel/tests/demo_measurements.rs` one
more time after the concurrent session's own `zip`-based clippy fix landed: `3 passed; 0 failed`.

---

## Summary

All four pieces of work are done. Piece 1 (positive test) was already complete on disk when this
task started (a discrepancy from the brief, disclosed above) and is verified passing. Pieces 2
(D1) and 3 (D2) are implemented, tested, and break-and-restore-verified. Piece 4 (Q176) is
implemented, tested, and break-and-restore-verified, using the router's real "no matching
connection" drop mechanism rather than the brief's named `FAULT_TARGET_KIND_PORT` mechanism, which
was found to be a confirmed no-op today (disclosed above, not worked around silently). No proto
changes, no `docs/adr/**` changes, no golden regeneration. All five required gates are clean.

**New/changed test functions, by file:**
- `crates/av-kernel/src/codec.rs`: `a_targeted_field_with_no_value_at_all_is_a_typed_error_not_a_silent_drop`,
  `a_targeted_field_with_a_bytes_value_is_a_typed_error_not_a_silent_drop`,
  `an_empty_target_field_is_still_skipped_silently_alongside_targeted_fields` (all new, mine).
- `crates/av-kernel/src/drm/binding.rs`: `any_model_last_measurements_delegates_to_the_star_tracker_variant`
  (new, mine).
- `crates/av-kernel/tests/demo_measurements.rs`: `demo_measurements_drm_decodes_real_measurements_through_execute`
  (pre-existing, verified by me, not authored by me),
  `dropped_measurement_packets_still_decode_at_the_emitter_with_decoded_at_naming_it` (new, mine),
  `a_declared_port_drop_fault_against_the_star_tracker_instance_has_no_effect_on_measurements_today`
  (pre-existing/concurrent, verified by me, not authored by me).
- `crates/av-dynamics/src/erase.rs`: `erased_model_reaches_inner_last_measurements` (pre-existing,
  verified by me with break-and-restore, not authored by me).

**Exact gate counts:**
- `cargo build --workspace --all-targets`: clean, 0 errors, 0 warnings.
- `cargo test --workspace --exclude av-kernel`: 179 passed, 0 failed (baseline 178; one Docker
  test flaked once under contention, confirmed clean on two isolated re-runs).
- `cargo test -p av-kernel` (heavy, run alone): 734 passed, 0 failed, 1 ignored (baseline 719;
  discrepancy discussed above -- 8 of the +15 concretely attributed, remainder likely baseline
  drift, not a regression).
- `cargo clippy --workspace --all-targets -- -D warnings`: clean, 0 warnings, 0 errors.
- `cargo deny check`: clean, `advisories ok, bans ok, licenses ok, sources ok`.

**What I could not fully resolve:** the exact +7 gap between the attributable new-test count (+8)
and the observed baseline delta (+15) for the `-p av-kernel` gate -- see that gate's own section
above for the full reconciliation attempt. It does not affect the pass/fail outcome (0 failed
either way) and I did not spend a second ~5-minute heavy run chasing it further given it isn't a
defect signal.

**Escalation for the lead (not a blocker on this report, but flagged as instructed):** the brief's
Q176 instructions describe `FAULT_TARGET_KIND_PORT`'s `"drop"` kind as an already-working,
"declared" router-drop mechanism to reuse. It is not -- `crate::drm::fault::realize_unapplied_fault`
returns `DrmError::FaultTargetKindNotSupported` for every PORT/SENSOR fault kind today (confirmed,
tested, disclosed above). I used the router's own real "no matching connection" drop instead, which
proves the identical semantics the brief actually wants tested. A second test already present in
the tree (`a_declared_port_drop_fault_against_the_star_tracker_instance_has_no_effect_on_measurements_today`)
independently pins this exact gap as current, disclosed, unimplemented behavior for the lead's own
review.
