# M25.3 report: telemetry into the viewer as CDM Measurements

status: in progress

## Not done (running list, updated as work proceeds)

- Everything -- just started. This list will be trimmed as items complete.

## Findings first

(to be filled in as exploration proceeds)

---

## M25.3c worker session (this section) -- work items 1-3

**Concurrency note, disclosed up front.** A second, independently-dispatched session ("team 2",
per `docs/teamlog/2026-09-02-team-1.md`'s own tail: "new manager after the pause ... First task:
check M25.3's partial state, continue it") was actively editing this exact working tree for this
same task at the same time as this session, and wrote its own parallel account to
`drms/M25_3C_REPORT.md` (note the different filename -- capital C). The two sessions' edits
interleaved file-by-file without conflict (different `impl DynamicsModel for ...` sites were
picked up by whichever session reached them first; each session's own `git diff`/`cargo check`
showed the other's prior edits as "already done" and verified rather than re-did them). Where the
two sessions' findings agree, that agreement is independent corroboration, not double-counting;
where they disagreed at first (see Drop semantics below), the discrepancy and its resolution are
recorded plainly. This section describes only what this session verified and produced; credit for
anything authored by team 2 belongs to `M25_3C_REPORT.md`.

### Work item 1: `crates/av-kernel/tests/demo_measurements.rs`

**Hypothesis, stated before running anything** (derived from `drms/demo_measurements.drm.yaml`,
`.sos.yaml`, and the two target-populated system fixtures -- grepped, not guessed):
- Run span 6 s (`1767225637000000000` to `1767225643000000000`), 1 Hz kernel step.
- Both sensors declare `*.update_rate_hz = 2.0` (500 ms period) -> 12 emissions each over the run,
  due at `start + k*500_000_000 ns` for `k = 1..=12`.
- Measurement ids, from the fixtures' own `packet_codecs[0].fields[*].target` strings:
  `altavista.attitude_q4` (star tracker, all 4 fields), `altavista.imu_gyro3` (wx,wy,wz),
  `altavista.imu_accel3` (ax,ay,az).
- Count: 12 (star tracker) + 12*2 (IMU, two ids per emission) = **36**.
- Order: `(epoch_ns, measurement_id)` ascending; per tied epoch, alphabetical id order puts
  `attitude_q4` before `imu_accel3` before `imu_gyro3`.
- `sensor_id`: the emitting instance name (`"startracker"`/`"imu"`), filled in by
  `HeteroScheduler::advance_to_with_ports`, never the model's own `dynamics_model` string.
- `frame_id`: empty for all 36 (neither fixture declares one; `StarTrackerModel`/`ImuModel`'s own
  `ModelInfo.frame_id` is always `String::new()`).
- `r`: empty for `attitude_q4` (a unit quaternion's small-angle sigma is not a diagonal covariance
  on 4 raw components); `diag(gyro_noise_sigma^2)` / `diag(accel_noise_sigma^2)` for the two IMU
  ids (declared `0.0001`/`0.001`).

**Measured (via a throwaway `eprintln!` dump, run once, then deleted -- not asserted from reading
the code):** every one of the above matched exactly on the first try -- 36 measurements, the exact
epoch set, the exact 3-id-per-epoch order, `sensor_id`/`frame_id`/`r` all as predicted. The only
thing that needed picking, not predicting, was the statistical tolerance on `z` (see the test
file's own doc comments): a 9-sigma bound on the star tracker's composed rotation angle (derived
from a 5-sigma-per-axis union bound over the 3-component small-angle error vector, `5*sigma*sqrt(3)
~= 8.66*sigma`, this repo's own existing 5-sigma convention from `crate::drm::sensors`'s module doc
comment) and a `5*noise_sigma + 5*bias_rw_sigma*sqrt(elapsed_s)` union bound for the IMU. Both
bounds are far looser than the observed residuals (checked against the same dump) -- tight enough
to catch a real bug, loose enough not to flake.

**Hash verification.** `drms/demo_measurements.drm.yaml`'s (and both system fixtures') declared
hash verified through the loader on every run -- `execute()` calls `hash::verify_drm_hash` at its
very top and returns `Err` on mismatch via `?`; every test in this file calls `execute(...)
.expect(...)`, and none ever panicked on that `expect`. No regeneration was needed.

**Break-and-restore evidence.** Temporarily removed `measurement.sensor_id = id.clone();` from
`crates/av-kernel/src/schedule.rs::HeteroScheduler::advance_to_with_ports` (leaving `sensor_id`
empty). Re-ran `cargo test -p av-kernel --test demo_measurements`:
```
thread '...' panicked at crates/av-kernel/tests/demo_measurements.rs:230:21:
assertion `left == right` failed: k=1
  left: ""
 right: "startracker"
```
Restored the line; re-ran -- back to green. This is the specific wrong implementation named in the
task brief ("drop the `sensor_id` fill in `schedule.rs`"), and the test catches it.

**Final state of the file:** 3 tests (`demo_measurements_drm_decodes_real_measurements_through_execute`,
`a_declared_port_drop_fault_against_the_star_tracker_instance_has_no_effect_on_measurements_today`
(this session), and `dropped_measurement_packets_still_decode_at_the_emitter_with_decoded_at_naming_it`
(team 2, addressing question 176 -- see Drop semantics below)), all passing together.

### Work item 2: `last_measurements` made a required trait method

`av_dynamics::DynamicsModel::last_measurements`'s `Vec::new()` default body was removed
(`crates/av-dynamics/src/lib.rs`); it is now `fn last_measurements(&self) -> Vec<av_cdm::pb::
Measurement>;` with no body, per the standing "no trait default that returns a valid empty result"
rule.

**Impl count.** The task's own suggested command (`grep -rn "impl DynamicsModel for" --include=
"*.rs" crates`) finds 35 literal matches, 2 of which are doc-comment mentions, not real `impl`
blocks (33 real). That literal grep misses **generic** wrapper impls (Rust renders
`impl<M: DynamicsModel> DynamicsModel for Wrapper<M>`, which does not contain the substring "impl
DynamicsModel for"): `CommandedAttitude<M>`, `TruthBroadcastAttitude<M>`, `ErasedModel<M>`,
`StmAugmented<M>`. A precise regex
(`^\s*(pub\s*\(?crate\)?\s*)?impl(<[^>]*>)?\s+DynamicsModel\s+for\s`) across the workspace finds
**38** real `impl DynamicsModel for ...` blocks total, cross-checked against `grep -rl "fn
last_measurements"` (13 files) and a raw `fn last_measurements` occurrence count of 39 (38 impls +
1 trait declaration) -- both counts agree. All 38 now implement `last_measurements`; `cargo build
--workspace --all-targets` is clean.

**Wrapper delegation**, verified for every wrapper named in the brief:
- `CommandedAttitude<M>`, `TruthBroadcastAttitude<M>`, `SharedContainerModel`, `ErasedModel<M>`,
  `StmAugmented<M>`, `GmatFramedCommandModel` (wraps `GmatModel`) -- all delegate
  (`self.inner.last_measurements()` / `self.0.last_measurements()`).
- `AnyModel` -- explicit 7-arm match (`Gmat`/`ConstantAccel`/`Attitude`/`Controller`/
  `StarTracker`/`Imu`/`GroundStation`), no catch-all, each delegating to the wrapped model.
- `ContainerModel` -- genuinely empty with a one-line reason (`lockstep.proto`'s
  `LockstepStepResponse` has no `Measurement` concept), not a wrapper in the delegation sense.

**Delegation test (required by the brief, "add a test for at least one wrapper").** Added
`erased_model_reaches_inner_last_measurements` in `crates/av-dynamics/src/erase.rs`, extending the
existing `AllOverridden` full-per-method-delegation test model with a distinguishable marker
measurement (`measurement_id: "all_overridden_measurement"`, `z: [123.0]`) -- exactly this file's
own established "one test per trait method, each checking a value no default/inherited path could
produce by coincidence" pattern. Passes; `cargo test -p av-dynamics` is 35/35. (Team 2
independently added the `AnyModel`-level counterpart,
`any_model_last_measurements_delegates_to_the_star_tracker_variant`, in `binding.rs` -- see
`M25_3C_REPORT.md`; both are complementary, not duplicates.)

**Arm-count symmetry check.** `crates/av-kernel/src/drm/binding.rs::tests::
any_model_arm_count_for_ground_station_matches_star_tracker` counts literal `AnyModel::StarTracker(`
vs `AnyModel::GroundStation(` occurrences in the file via `include_str!`, method-agnostic -- it
already covered `last_measurements`'s own arm the moment that arm existed (no change needed to the
check itself). Confirmed still passing: `cargo test -p av-kernel --lib drm::binding::` includes
`any_model_arm_count_for_ground_station_matches_star_tracker ... ok` among 137 passed, 0 failed.

**Every `Vec::new()` override carries its own one-line "why no measurement" comment** -- confirmed
by direct inspection of all 38 sites (test-only models: "test-only, never emits telemetry";
production models: cite the specific reason -- no declared codec, a command-only codec, no
`PacketField.target`, or no CDM concept in the wire protocol).

### Work item 3: drop semantics

**Design (unchanged, as instructed):** a `Measurement` is decoded at the emitting sensor's own
`step_with_ports` call (`crate::codec::measurements_from_field_values`, called from
`StarTrackerModel`/`ImuModel`), before the resulting packet is ever handed to `crate::router::
Router`. This is entirely independent of whatever the router later does with that packet.

**Experiment 1 (already real, no fixture change needed).** `drms/demo_measurements.sos.yaml`
declares zero `Connection`s for either sensor's own FRAMED OUT port (`st_meas`/`imu_meas`) -- only
the 14 truth SIGNAL connections. Per `crate::router`'s own module doc comment: "a message on a port
with no matching connection is silently dropped (not wired anywhere)." So every one of the 36
measurement-producing packets `demo_measurements_drm_decodes_real_measurements_through_execute`
proves real was, by the router's own definition, dropped -- yet all 36 measurements appeared.

**Experiment 2 (a declared PORT fault, run for real, not inferred).** Added
`Fault { target_kind: FAULT_TARGET_KIND_PORT, kind: "drop", instance: "startracker", target:
"st_meas" }` to a rehashed, mutated copy of the DRM's `scenario.faults` and ran `execute()`.
Result, actually observed: `execute()` succeeds, and `RunProducts.measurements` is byte-for-byte
identical to the unfaulted baseline (36 measurements, same ids/epochs/z/r/sensor_id/frame_id) --
`a_declared_port_drop_fault_against_the_star_tracker_instance_has_no_effect_on_measurements_today`,
committed in `demo_measurements.rs`.

**A discrepancy worth recording plainly.** Team 2's own report (`M25_3C_REPORT.md`) initially
concluded, from reading `crate::drm::fault::realize_unapplied_fault`'s own doc comment and its unit
tests, that declaring this exact `Fault` "would make a DRM using it fail to execute at all" (citing
`realize_unapplied_fault` always returning `DrmError::FaultTargetKindNotSupported` for a PORT
target). **That is true of `realize_unapplied_fault` called directly, but that function has no call
site anywhere in `crate::drm::executor` at all** -- confirmed by `grep -n
"realize_unapplied_fault" crates/av-kernel/src/drm/*.rs`, which shows it used only inside
`fault.rs`'s own unit tests (`fault.rs`'s own "Integration note (still open, out of scope for
M16.2)" doc comment says as much). `execute()`'s own fault dispatch only ever branches on
`FaultTargetKind::Dynamics`/`::Hardware` (grepped: no `if f.target_kind == ...Port...` or `...
Sensor...` guard exists in `executor.rs`), so a `FaultTargetKind::Port` entry in `scenario.faults`
is never even inspected by `execute()` -- it is a true no-op end to end, not a refusal. This
session's test above is the empirical proof (it actually ran and passed); team 2's own report
already found and folded in this test, crediting it as independent corroboration from "a second
angle," so the two reports are reconciled, not in conflict, as of this writing.

**Pinned in a doc comment**, per the brief: `RunProducts::measurements`'s own field doc comment
(`crates/av-kernel/src/drm/executor.rs`) now states, in one paragraph: a `Measurement` is decoded
at the emitting instance before the router ever sees the packet, so a packet the router never
delivers (no connection, or a `"latency"` connection that never lands before the run ends) still
contributes its `Measurement`; a declared PORT `"drop"` fault currently has no effect on this field
at all, since `execute()` does not realize PORT/SENSOR faults yet.

**Escalation, plainly, for the lead.** This is a semantics question already decided once (question
176, `docs/open-questions.md` line 317, added mid-session by the lead: emitter-side decode is kept,
`Measurement.meta["decoded_at"]` names the instance, receiver-side decode is future scope) --
this session's own finding is a narrower, additional fact the lead should have on record alongside
that decision: **`FaultTargetKind::Port`/`::Sensor` faults are currently silent no-ops through the
real `execute()` entry point, not typed refusals** (they are typed refusals only if a caller
invokes `crate::drm::fault::realize_unapplied_fault` directly, which nothing in the executor does).
If a DRM author declares a PORT/SENSOR fault today expecting it to do something, they get neither an
effect nor an error -- silently wrong in a different way than either team initially assumed. Not
redesigned here, per the task's own instruction; reported for the lead to decide whether that gap
should be closed (e.g. `execute()` refusing to load a DRM that declares an unrealized PORT/SENSOR
fault, rather than silently ignoring it) in a future task.

### Gates (this session, run in isolation per the standing "a contended gate result is not a
result" rule -- this host had team 2 running its own gates concurrently for long stretches;
contended attempts are disclosed, not hidden)

- `cargo build --workspace --all-targets`: clean, 0 errors.
- `cargo test --workspace --exclude av-kernel`: two contended attempts truncated/failed on
  unrelated `av-lockstep`/Docker-registry tests while team 2's own gate run (and, separately, a
  stuck build-lock from this session's own earlier retry) were active on the same host; killing the
  stuck process and re-running alone once the host was quiet gave a clean, reproducible **179
  passed, 0 failed** (baseline 178 + this session's own new `erased_model_reaches_inner_
  last_measurements` test). Matches team 2's own independently-measured 179/0 exactly.
- `cargo clippy --workspace --all-targets -- -D warnings`: **first run found 2 real errors**, both
  in this session's own `crates/av-kernel/tests/demo_measurements.rs` (not pre-existing, not from
  team 2's edits): `needless_lifetimes` on `truth_sample_at<'a>` (elided: `fn truth_sample_at(
  products: &av_kernel::drm::RunProducts, tai_ns: i64) -> &[f64]`) and `needless_range_loop` on the
  IMU gyro3 bound check's `for i in 0..3 { ... m.z[i] ... truth_omega[i] ... }` (rewritten as `for
  (i, (&zi, &oi)) in m.z.iter().zip(truth_omega.iter()).enumerate()`). Fixed at the source, no
  `#[allow(...)]` anywhere -- re-ran `cargo test -p av-kernel --test demo_measurements` after the
  fix to confirm all 3 tests still pass (`3 passed; 0 failed`), then re-ran clippy: **clean, 0
  errors, 0 warnings**, workspace-wide.
- `cargo deny check`: clean (`advisories ok, bans ok, licenses ok, sources ok`; one pre-existing,
  unrelated `getrandom` duplicate-version warning, not introduced by this task).
- `cargo test -p av-kernel` (the heavy one, run alone, timestamped): **announced at 17:50 CDT
  2026-09-07.** Team 2's own identical gate run was already in progress at that point (its process
  showed zero accumulated CPU time across a 20+ minute observation window, with no Docker activity
  and no OS-level lock explaining the stall -- judged genuinely hung, not merely slow, since
  `gmat_sys::engine_lock()` is an in-process `std::sync::Mutex`, not a cross-process lock, so it
  cannot itself explain a stuck *other* process). Rather than wait indefinitely on an apparently
  stalled process consuming negligible resources, this session ran its own copy alongside it.
  Result, actually measured end to end (every "Running tests/..." binary reached `test result: ok`,
  zero `FAILED` lines in the full output): **734 passed, 0 failed, 1 ignored** in
  `target/debug/deps/*` across 32 test binaries (unit + every integration test file + doc-tests).
  The one ignored test is confirmed, by name, to be the expected one:
  `byte_identical_port_traffic_between_posix_container_and_renode ... ignored, question 171: Renode
  port traffic beyond STEP 1 does not deliver; verified posix-container-only until resolved` --
  still `#[ignore]`d, per the gate instructions. `734 = 719 (baseline) + 15` -- accounted for by
  this session's own 3 new tests in `demo_measurements.rs` (the file did not exist as a real test
  before this task) plus team 2's own additions (the D1 defect tests in `codec.rs`, the `AnyModel`
  delegation test in `binding.rs`, and others per `M25_3C_REPORT.md`); not reconciled test-by-test
  here since both sessions' additions are visible in `git diff` and neither session removed or
  weakened any pre-existing test.
