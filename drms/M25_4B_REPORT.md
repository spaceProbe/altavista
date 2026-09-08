# M25.4b report: the replay binding

Incremental report, written as work happened, not at the end (standing rule). Sections appear
in the order the work was actually done; each states a hypothesis/expected value before the
corresponding measurement where one applies.

## Starting state

Branch `develop`, working tree clean at task start except an unrelated pre-existing
modification to `docs/teamlog/2026-09-02-team-1.md` (not touched by this task). M25.4a
(commit `685af17`) already in the tree: `PortTrafficRecord`/`PortTrafficLog`/
`RunProducts.port_traffic_hash` in `proto/altavista/v1/run.proto`, `crates/av-kernel/src/
router.rs` recording port traffic, `crates/av-kernel/src/drm/executor.rs`'s
`write_port_traffic_sidecar`, and `crates/av-kernel/tests/port_traffic_sidecar.rs`'s four
tests.

## Design decision 1: `RunConfig.replay` hash check happens before ANYTHING else in `execute()`

Implemented as literally the first two statements of `execute()` (`crates/av-kernel/src/drm/
executor.rs`): the `RunConfig.replay` + `DrmOptions.covariance` refusal (cheap, no I/O), then
`crate::drm::replay::verify_and_load` (reads and hashes `log_path`'s exact bytes, refuses on a
mismatch, only then decodes as `PortTrafficLog`) -- both before the canonical DRM/SOS/system
hash verification, before `Router::build`, before any `ModelRegistry::construct_*` call, before
any GMAT call, and before any step. T2's own two tests are the proof this ordering holds (see
below).

## Design decision 2: `ReplayModel` is a wrapper around a REAL, freshly-constructed model's own `ModelInfo`

Hypothesis, stated before writing any code: for a replayed run's `Trajectory` to come out
byte-identical to the original, `TrajectorySegment.dynamics_model`/`.dynamics_hash`/
`.dynamics_depth` (stamped straight from `ModelInfo.id`/`.settings_hash`/`.depth`,
`crate::trajectory::build_trajectory`) must match exactly what the REAL, non-replayed model
would have reported. For a `BINDING_KIND_MODEL` instance this is always achievable without
running the real model's own step logic: `ModelInfo` is a pure function of the instance's own
DECLARED configuration (no GMAT call, no network) -- `crate::registry::ModelRegistry::
wrap_replay` constructs the real model exactly as a non-replayed run would (through the SAME
`classify_binding`/`materialize_plan`/`materialize_plan_at_boundary` dispatch, unchanged),
reads its `describe()`/`state_dim()` ONCE, and only THEN discards its real behaviour in favour
of a `ReplayModel` built from those captured values.

**Measured, not merely designed around:** `crates/av-kernel/tests/replay.rs::t1_replayed_
startracker_produces_byte_identical_run_products_with_no_exclusions` compares
`RunProducts.to_proto().encode_to_vec()` between a real run and a replayed run byte for byte,
with **zero fields excluded** -- this only holds because `wrap_replay` reads the real model's
own `ModelInfo` first.

For a `BINDING_KIND_CONTAINER` instance there is no such option: its own `ModelInfo`/
`binding_hash` come only from a live `Bind` response, and the whole point of replaying a
container instance is running Docker-free. `crate::registry::ModelRegistry::
construct_replay_container` builds a SYNTHETIC `ModelInfo` instead (from the instance's own
declared `dynamics_model`/`state_space_id` alone) -- a disclosed, unavoidable difference from
the original run's own container segment, excluded explicitly (never silently) by
`crates/av-kernel/tests/drm_attitude_control_cfs.rs`'s own replay test.

## Design decision 3: a `BINDING_KIND_CONTAINER` instance being replayed never reaches `binding::materialize_container` at all

`crate::drm::executor::run_shared_group`'s own `container_plans` construction loop now branches,
per instance, on `replay_targets.contains(name)` BEFORE calling `binding::materialize_container`
-- a replayed container instance is registered as a `ModelSpanState` (the identical shared
`HeteroKernel`/`Router` machinery every `BINDING_KIND_MODEL` instance already uses), never a
`ContainerSpanState`, and the real `materialize_container` call (which would dial a live
address) is never made for it. Verified directly: `crates/av-kernel/tests/
drm_attitude_control_cfs.rs::run_byte_identical_products_when_the_container_bound_controller_
is_replayed_docker_free`'s second `execute()` call never touches the `_registry_guards`/image
plumbing the first call set up, and passes against the real `docker` binary present on this
host -- see the Docker gate below.

## `AnyModel::Replay` and the arm-count check

Added as an 8th `AnyModel` variant (`crates/av-kernel/src/drm/binding.rs`), delegating all nine
`DynamicsModel` methods explicitly (no trait defaults relied on) -- `state_dim`, `derivatives`,
`describe`, `stm_capable` (always `false`), `stm_derivatives`/`step_with_stm` (each split into a
guard arm + a dead-but-typechecking delegate arm, mirroring `Attitude`/`Controller`),
`integrator`, `step`, `step_with_ports`, `last_measurements`. Eleven real match arms measured
directly (`grep -c` on the 12-space-indented needle), matching `AnyModel::GroundStation`'s own
eleven exactly.

**The arm-count check needed a genuine redesign, not a copy-paste of the existing
`any_model_arm_count_for_ground_station_matches_star_tracker` test.** `AnyModel::Replay` is the
first variant whose own construction call site lives OUTSIDE `binding.rs` (`crate::registry::
ModelRegistry::wrap_replay`/`construct_replay_container`, in `registry.rs` -- every other
variant's own `materialize_*` function constructs it inline, in this same file). A direct
`include_str!("binding.rs")`-only text-count comparison against `GroundStation` was tried FIRST
and measured wrong (19 vs. 14 raw occurrences -- the arithmetic did not naturally cancel the way
the existing `GroundStation`/`StarTracker` test's own two self-referential occurrences do,
because `GroundStation` also has its own construction call site AND a stray prose mention this
file's own `any_model_arm_count_for_ground_station_matches_star_tracker` test predates). Fixed
by anchoring on the 12-space match-arm indentation (excludes every comment/doc/string
occurrence, real or in either test's own source) for the in-file count, plus a separate
`registry.rs`-`include_str!` check that `wrap_replay`'s own construction call exists --
`any_model_arm_count_for_replay_covers_every_dynamics_model_match_site`
(`crates/av-kernel/src/drm/binding.rs`).

## T1 fixture: NOT `drms/demo_attitude_control.*.yaml`, and why (measured, not assumed)

**Hypothesis, before investigating the exact fixture:** the task brief's own named T1 fixture
(`drms/demo_attitude_control.{drm,sos}.yaml`'s `controller` instance, FRAMED
`wheel_torque_out`) would work directly.

**Measured wrong**, by reading the real source `execute()` and `AttitudeControllerModel::
step_with_ports` actually run, not by assumption -- two independent, unavoidable obstacles to a
genuinely byte-identical replayed `RunProducts` for that exact fixture:

1. `crate::drm::controller::AttitudeControllerModel::step_with_ports` reports
   `pointing_error_rad`/`seq` as `StepResult.outputs`, never onto any port. `drms/
   demo_attitude_control.drm.yaml`'s own declared `Objective`/`MeasureOfEffectiveness` read
   exactly those two names (`output.controller.pointing_error_rad@end`/`output.controller.
   seq@end`) -- a replayed run's own scoring pass would either fail to evaluate at all
   (`ExprError::UnknownOutput`, surfaced as `DrmError::InvalidExpression`, since the referenced
   series would have zero data points under replay) or, if objectives were simply cleared,
   would still leave `RunProducts.scores` structurally different from a run that DID declare
   them.
2. `AttitudeControllerModel::step_with_ports` ALSO reports an `AppliedCommand` on its own
   `wheel_torque_out` port (`field: "tau_mag"`) UNCONDITIONALLY, every firing step (`crates/
   av-kernel/src/drm/controller.rs` line 399, inside the `while end >= self.next_due.get()`
   loop -- confirmed by reading the source directly, not the doc comments alone).
   `run_shared_group`'s own tail turns EVERY `AppliedCommand` into an `Event`
   (`EVENT_KIND_PORT_COMMAND`), unconditionally, no filter (line ~1947-1972). A replay binding
   never computes an `AppliedCommand` at all (there is no real controller logic left behind it
   to have applied one) -- so the replayed run's own `RunProducts.events` would be missing
   every one of these, real per-step events over the whole run. This directly conflicts with
   this task's own standing rule: "excluding a trajectory or an event is not acceptable."

**Chosen instead:** `startracker` from the already-existing, already-tested, objective-free
`drms/demo_attitude_sensors.*.yaml` (M22.2/M22.2b) -- unmodified, read-only. Verified, not
assumed, that this instance has neither problem:
- `StarTrackerModel::step_with_ports` returns `Vec::new()` for `AppliedCommand`s
  unconditionally (`crates/av-kernel/src/drm/sensors.rs` line 675) -- it never consumes
  anything, only measures and emits.
- Its own declared `PacketCodec` (`drms/demo_attitude_sensors_startracker.system.yaml`)
  declares no `PacketField.target` at all (confirmed by `grep`), so `crate::codec::
  measurements_from_field_values` produces nothing -- `last_measurements()` is EMPTY on the
  REAL, non-replayed run too, so `ReplayModel`'s own always-empty `last_measurements` is not a
  divergence for this fixture.
- `demo_attitude_sensors.drm.yaml` declares no `objectives`/`measures`/`faults`/`maneuvers`/
  `events` at all.
- `StarTrackerModel::state_dim() == 0`, excluded from `RunProducts.trajectories` entirely (the
  same treatment `AttitudeControllerModel`/`GroundStationModel` get) -- confirmed directly in
  T1's own test body (`assert!(!run_real.trajectories.contains_key("startracker"))`).

With both runs pointed at the SAME `products_dir`/`run_id`, T1's own comparison excludes
**nothing** -- stronger than the anticipated "exclude `port_traffic_uri`/`port_traffic_hash`"
fallback, not weaker. `crates/av-kernel/tests/replay.rs`'s own module doc comment carries the
full account (it is deliberately not repeated verbatim here); this is disclosed as a considered
substitution, not a silent one, for the manager to revisit if literal reuse of the `controller`/
`wheel_torque_out` fixture is required regardless of the divergence consequences documented
above.

## T3: the fixture's own catch-up shape forced a design change mid-test (measured, not assumed)

**Hypothesis:** delete exactly one interior `startracker` OUT record, re-serialize, re-hash,
and the replay run refuses with `ReplayError::MissingFrame`.

**Measured wrong on the first attempt:** `demo_attitude_sensors_startracker.system.yaml`'s own
declared 2 Hz update rate against the DRM's 1 Hz kernel step means `StarTrackerModel::
step_with_ports`'s internal catch-up loop fires exactly twice per kernel-tick epoch, and
`Router::deliver`'s own `emission_tai_ns` (the STEP's epoch) is shared by every message one
`deliver` call carries -- so EVERY emission epoch this instance ever records carries exactly
TWO OUT records, never one. Deleting only one left the epoch's own bucket non-empty (one frame
remained), and the first real run of the test completed with `Ok(RunProducts)`, not the
expected error -- `ReplayModel`'s own missing-frame rule fires on an EMPTY epoch bucket, never
a merely-thinned one (this is documented, deliberate behaviour, not itself a bug: see
`crate::drm::replay`'s own module doc comment). Fixed by deleting BOTH records recorded at the
chosen interior epoch, with the measured shape and the reasoning disclosed directly in the
test's own comments (`crates/av-kernel/tests/replay.rs`) rather than silently deleting only one
and reporting a pass that never actually exercised the rule.

## Break-and-restore (this task's own standing rule, applied to every test/claim below)

Each entry: the wrong implementation actually broken, which real test(s) failed, and the real
failure text. All restored immediately after capture; `git diff`/`cargo build` clean afterward.

### Break: skip the hash check (`if false && computed != cfg.expected_hash` in `crate::drm::replay::verify_and_load`)

Affected: `t2_a_replay_log_hash_mismatch_is_refused_before_any_step`,
`t2_a_corrupted_replay_log_file_is_refused_even_with_the_original_hash_as_expected`.

Real failures (both printed the full, successfully-produced `RunProducts` instead of the
expected `DrmError`):
```
thread 't2_a_corrupted_replay_log_file_is_refused_even_with_the_original_hash_as_expected' panicked at crates/av-kernel/tests/replay.rs:240:5:
ReplayLogIo { path: "...", detail: "decoding 2331 verified byte(s) as PortTrafficLog: failed to decode Protobuf message: Provenance.attributes: PortTrafficLog.provenance: invalid string value: data is not UTF-8 encoded" }

thread 't2_a_replay_log_hash_mismatch_is_refused_before_any_step' panicked at crates/av-kernel/tests/replay.rs:202:35:
a wrong expected_hash must refuse the run, not silently proceed: RunProducts { trajectories: {...}, events: [...], scores: {}, ... }
```
(The corrupted-file variant happened to also trip a decode error once the hash gate was
removed, since the corrupting byte-flip landed inside a UTF-8 string field -- still proof the
gate, not the decoder, is what T2 exists to test: the SECOND test, a wrong `expected_hash`
against an otherwise-valid file, shows the real, uncorrupted `RunProducts` sailing through.)

### Break: never detect an interior gap (`let interior_gap = false;` in `ReplayModel::step_with_ports`)

Affected: `t3_one_deleted_interior_record_is_a_typed_missing_frame_error_naming_the_instance_and_epoch`.

Real failure:
```
thread 't3_one_deleted_interior_record_is_a_typed_missing_frame_error_naming_the_instance_and_epoch' panicked at crates/av-kernel/tests/replay.rs:338:35:
a deleted interior record must be refused, not silently held or interpolated: RunProducts { ... scores: {}, ... }
```

### Break: replay IN records too (drop the `record.direction != PortDirection::Out as i32` filter in `ReplayModel::new`)

Affected: `crate::drm::replay::tests::in_records_for_this_instance_are_never_played_back`
(unit test; the equivalent integration-level failure was not separately re-captured once the
unit-level proof was in hand, since it exercises the identical filter).

Real failure:
```
thread 'drm::replay::tests::in_records_for_this_instance_are_never_played_back' panicked at crates/av-kernel/src/drm/replay.rs:294:9:
assertion `left == right` failed: the IN record must not also be replayed as an emission: [PortMessage { port: "wheel_torque_out", ... }, PortMessage { port: "star_in", ... }]
  left: 2
 right: 1
```

## Gates

Run each in isolation (checked with the host-quiet command before each `cargo test`), exact
counts below.

1. `cargo build --workspace --all-targets` -- clean. `Finished \`dev\` profile [unoptimized +
   debuginfo] target(s) in 1m 42s`, no warnings, no errors.
2. `cargo test --workspace --exclude av-kernel` -- **182 passed, 0 failed** -- exactly the
   baseline, unchanged (summed directly from every `test result:` line the run printed: 47 + 2
   + 12 + 35 + 16 + 6 + 2 + 1 + 4 + 6 + 10 + 11 + 1 + 5 + 6 + 4 + 1 + 8 + 2 + 2 + 1 = 182, plus
   several `0`-test binaries).
3. `cargo clippy --workspace --all-targets -- -D warnings` -- clean after one fix (`clippy::
   bool_assert_comparison` in `crates/av-kernel/tests/replay.rs`, `assert_eq!(x, false)` ->
   `assert!(!x)` -- no `#[allow]`).
4. `cargo deny check` -- `advisories ok, bans ok, licenses ok, sources ok`, exit code 0. The
   printed `duplicate` warnings (`tower`, `windows-sys`, `getrandom`, ...) are pre-existing,
   unrelated to this task's own dependency graph (no crate this task added), not investigated
   further.
5. `cargo test -p av-kernel` (isolated, host confirmed quiet first; started 04:28:40, finished
   05:40:33 -- about 72 minutes, longer than the 55-minute baseline note, no other explanation
   sought since the count itself is what matters here) -- **767 passed, 0 failed, 1 ignored**
   (summed programmatically from every `test result:` line in the captured log, cross-checked
   against the "no FAILED anywhere" grep). Baseline was **750 passed, 0 failed, 1 ignored**.
   Delta: **+17 passed, test by test**:
   - `crates/av-kernel/src/drm/replay.rs`'s own 9 unit tests (`describe_and_state_dim_report_
     exactly_the_caller_supplied_values`, `an_instance_with_no_recorded_frames_at_all_never_
     errors`, `a_step_after_the_last_recorded_epoch_emits_nothing_and_is_not_an_error`,
     `a_step_at_a_recorded_epoch_replays_exactly_that_epochs_own_frame`, `in_records_for_this_
     instance_are_never_played_back`, `a_step_strictly_between_the_first_and_last_recorded_
     epoch_with_no_frame_is_a_typed_error`, `a_step_before_the_first_recorded_epoch_emits_
     nothing_and_is_not_an_error`, `verify_and_load_refuses_a_hash_mismatch`, `verify_and_load_
     accepts_a_matching_hash_and_decodes_the_log`).
   - `crates/av-kernel/src/drm/binding.rs`'s own 1 new test (`any_model_arm_count_for_replay_
     covers_every_dynamics_model_match_site`).
   - `crates/av-kernel/tests/replay.rs`'s own 6 tests (`t1_replayed_startracker_produces_byte_
     identical_run_products_with_no_exclusions`, `t2_a_replay_log_hash_mismatch_is_refused_
     before_any_step`, `t2_a_corrupted_replay_log_file_is_refused_even_with_the_original_hash_
     as_expected`, `t3_one_deleted_interior_record_is_a_typed_missing_frame_error_naming_the_
     instance_and_epoch`, `a_replay_config_naming_an_unknown_instance_is_a_typed_load_refusal`,
     `replay_combined_with_covariance_is_a_typed_refusal_not_a_silent_ignore`).
   - `crates/av-kernel/tests/drm_attitude_control_cfs.rs`'s own 1 new test
     (`byte_identical_products_when_the_container_bound_controller_is_replayed_docker_free`).

   9 + 1 + 6 + 1 = 17, exactly the measured delta. The one ignored test is still the identical
   `byte_identical_port_traffic_between_posix_container_and_renode` (question 171), confirmed
   unchanged in name and reason text.

   **One real defect found and fixed by this gate run, not before:** the FIRST attempt at this
   gate (started 04:22:43, killed partway through inspection once the failure below was seen)
   failed one PRE-EXISTING test:
   ```
   thread 'drm::binding::tests::any_model_arm_count_for_ground_station_matches_star_tracker' panicked at crates/av-kernel/src/drm/binding.rs:5623:9:
   assertion `left == right` failed: AnyModel::GroundStation( must appear exactly as many times in this file as AnyModel::StarTracker( -- one deliberate arm per DynamicsModel method (plus this test's own two occurrences of the needle string, which cancel identically on both sides)
     left: 17
    right: 16
   ```
   Root cause: my own new test's doc comment (`any_model_arm_count_for_replay_covers_every_
   dynamics_model_match_site`) mentioned the literal text `` `AnyModel::GroundStation(` `` once
   in prose, with no balancing `` `AnyModel::StarTracker(` `` mention -- exactly the kind of
   self-referential text-count fragility the pre-existing test's own design already worked
   around for its OWN two occurrences, and my new test's prose accidentally broke. Fixed by
   rephrasing to "the `GroundStation` variant's own" (no literal `(`-suffixed needle). Re-run
   confirmed both arm-count tests pass in isolation; the full gate 3 was then re-run from a
   clean, host-quiet start (the 04:28:40-05:40:33 run reported above) rather than trusting the
   partial first attempt.

## T4 (the posix cFS container demo)

Docker WAS available on this host (`docker info` succeeds; `altavista-cfs-lockstep:local` was
already built from a prior task) -- so T4 runs for real, not `#[ignore]`d.
`crates/av-kernel/tests/drm_attitude_control_cfs.rs::byte_identical_products_when_the_
container_bound_controller_is_replayed_docker_free`: records one real run against the real cFS
container (the same `push_cfs_image_to_local_registry`/`container_sos`/`container_drm` helpers
`byte_identical_run_products_across_two_separately_spawned_cfs_containers` already established),
then replays `"controller"` (via `RunConfig.replay.instances: vec![]` -- the "every
`BINDING_KIND_CONTAINER` instance" default, exercised deliberately rather than naming
`"controller"` explicitly, so this test also proves that default resolution) from nothing but
the recorded log, Docker-free for the second `execute()` call. **Measured: PASS**
(`cargo test -p av-kernel --test drm_attitude_control_cfs byte_identical_products_when_the_
container_bound_controller_is_replayed_docker_free`, 44.76s). Excludes exactly the fields the
container's own live `Bind` response supplies and a Docker-free replay cannot
(`dynamics_hash`/`dynamics_model`/`dynamics_depth` for the `"controller"` segment,
`Trajectory.provenance.attributes["container_binding_hash"]`), each justified in the test's own
doc comment; every other field -- `"attitude"`/`"imu"`'s own trajectories, every `Event`, every
`Score` (both runs' `container_drm` clears objectives/measures the same way the sibling
determinism test's own `container_drm` call does) -- compared unmodified.

No dedicated break-and-restore was run against T4's own container-redirect branch specifically
(`crate::drm::executor::run_shared_group`'s `container_plans` loop) -- disclosed, not hidden:
the shared machinery it depends on (`ReplayModel`/`verify_and_load`) is already
break-and-restore-proven above, and T4 passing independently against a real container is strong
but not exhaustive evidence the redirect itself is correct.

---

## Manager review (2026-09-08)

Independent re-runs at the tree the worker left, before any manager edit: `cargo test -p
av-kernel --test replay` **6 passed**, `--lib drm::replay` **9 passed**, `--test
drm_attitude_control_cfs` **4 passed in 179 s** (Docker present, so T4 genuinely ran rather
than printing its skip). Only `drms/M25_4B_REPORT.md` had been modified after the worker's own
04:28 kernel run started, so that run's 767/0/1 applies to this exact source.

### T1 is narrower than it reads, and T1b closes the gap

The worker's substitution of `demo_attitude_sensors`/`startracker` for the brief's
`demo_attitude_control`/`controller` is **correct and I accept it**: `AttitudeControllerModel::
step_with_ports` really does report an `AppliedCommand` unconditionally every firing step, and
`run_shared_group` really does turn every one into an `EVENT_KIND_PORT_COMMAND` event, so
replaying that instance would silently lose real events -- exactly what the standing rules
forbid excluding. Checked at the source, not taken on the report's word.

But one thing the report does not say, and I found by reading the fixture:
**`drms/demo_attitude_sensors.sos.yaml` declares no `Connection` at all from `startracker`**
(no `from_instance: startracker` entry anywhere in the file). So the frames T1 replays are
carried by the router and delivered to nobody. T1's byte-identity is real but narrow: it proves
the replay binding emits exactly the recorded frames (they are hashed into
`port_traffic_hash`, which is part of the compared bytes) and perturbs nothing else. It does
**not** prove a downstream consumer driven by replayed frames computes the same thing, because
in that fixture there is no downstream consumer. A brief that asks for a closed-loop
demonstration is not satisfied by an open one, however clean the comparison.

**Added by the manager: `t1b_replaying_a_sensor_that_drives_a_closed_loop_reproduces_the_whole_
run_byte_identically`** (`crates/av-kernel/tests/replay.rs`). It uses `demo_attitude_control`
(Docker-free, four `BINDING_KIND_MODEL` instances) and replays `startracker`, which that
fixture DOES wire to the controller (`startracker.st_meas -> controller.startracker_in`,
asserted in the test itself so the test cannot quietly become an open loop). The controller
stays live -- so its `AppliedCommand`s still happen and the worker's objection does not apply --
and the DRM scores two real objectives off the controller's outputs. The whole encoded
`RunProducts` matches byte for byte with nothing excluded: trajectories, events, measurements,
scores and the port-traffic hash, with a replayed sensor closing the loop.

**Break-and-restore (manager's own).** Wrong implementation: replay the frames one step late
(`frames_by_epoch.get(&(emission_tai_ns - dt_ns))` instead of `&emission_tai_ns`) -- a
plausible off-by-one between a step's start and result epoch, and exactly the confusion that
already bit M25.4a's own hypothesis.

```
thread 't1b_replaying_a_sensor_that_drives_a_closed_loop_reproduces_the_whole_run_byte_identically'
panicked at crates/av-kernel/tests/replay.rs:195:5:
assertion `left == right` failed: a closed loop driven by a replayed sensor must reproduce the
entire run byte for byte -- trajectories, events, measurements, scores and the port traffic
hash, with nothing excluded
test result: FAILED. 0 passed; 1 failed
```

Restored from a byte-exact backup; `--test replay` back to 7 passed. `grep -rn "BREAK-TEST"
crates/` returns nothing.

### One documentation overclaim, corrected

`docs/sil-plan.md` said the cFS replay produced "byte-identical port traffic for the replayed
one". T4 passes `products_dir: None` on its replay run, so that run records no port traffic at
all and the claim is not checked anywhere in T4. The sentence now says what T4 actually asserts
(trajectory samples, segment epochs, `event_ids`, events, `dropped_in_flight_messages`, with
each excluded field named), notes that T4's `scores` comparison is real but vacuous because its
DRM declares no objectives, and points the scored half of the claim at T1b, where it is
genuinely checked. A description of the artifact is not the artifact -- including in our own
plan documents.

### Accepted with those two additions

The replay design is sound: keyed on `tai_ns` rather than `sequence` (correct, given M25.4a's
own `sequence = 0` dispatch record), OUT records only, hash verified before anything runs, the
declared binding kind preserved and asserted against the loaded artifact rather than against a
run's output. The missing-frame rule's limits are stated honestly in the code, the report, and
`docs/sil-plan.md`, including the empty-bucket-not-thinned-bucket subtlety the worker found by
having T3 fail first.

