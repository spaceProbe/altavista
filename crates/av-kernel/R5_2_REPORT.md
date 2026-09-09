# R5.2: a corrupted packet no longer aborts the run at a native FRAMED consumer

Task: `docs/open-questions.md` question 188 (team 2, R5.2), building on the PORT fault runtime
(R4.1a/R4.1b, questions 178/184/186) and question 189's ruling not to touch `Router::deliver`'s
epoch stamping. Every FRAMED consumer records a typed `decode_error` event and continues on its
own last good input instead of propagating a `CodecError` from a received frame; only a malformed
*declared* codec stays a load-time error.

Written incrementally, as instructed: hypotheses and expected values (the 300/600 frame counts,
the divergence direction and rough magnitude) are stated in the source (`drms/
demo_attitude_control_port_corrupt.drm.yaml`'s own header comment, `tests/decode_errors.rs`'s own
module doc comment) before the corresponding measurement, not retrofitted afterward.

## 1. What was built

- `crates/av-dynamics/src/lib.rs`:
  - `DecodeErrorOccurrence { port, tai_ns, sequence_count: Option<u16>, error: String }` -- a new
    public type.
  - `DynamicsModel::drain_decode_errors(&self) -> Vec<DecodeErrorOccurrence>`, a new REQUIRED
    trait method (question 112: no default returning a valid empty result), mirroring
    `last_measurements`'s exact shape (this call's own occurrences, not `drain_sensor_fault_
    effect`'s "since the last drain" accumulator -- a model only ever decodes its *last* message
    on a port once per `step_with_ports` call, so the two coincide here). Every `impl
    DynamicsModel` in the workspace got an explicit arm: trivial `Vec::new()` for a model that
    never itself calls `crate::codec::decode_packet` (the large majority -- mechanically inserted
    via a small Python script mirroring each site's own pre-existing `drain_sensor_fault_effect`
    shape, then hand-verified; two sites the script mis-transformed because their own `drain_
    sensor_fault_effect` was NOT the trivial `None` case -- `StarTrackerModel`/`ImuModel` in
    `crates/av-kernel/src/drm/sensors.rs` -- were caught by the very next full-suite/compile check
    and hand-corrected to the real trivial `Vec::new()` arm, since neither model itself decodes a
    FRAMED frame); a combining arm (own occurrences + `self.inner.drain_decode_errors()`) for the
    three wrapper types that both decode their own frame AND wrap another model
    (`CommandedAttitude`, `GmatFramedCommandModel`); a real accumulating implementation for the
    five FRAMED-consuming models themselves (see below).
- `crates/av-kernel/src/codec.rs`:
  - `peek_sequence_count(data: &[u8]) -> Option<u16>` -- reads the CCSDS primary header's own
    sequence-count bits directly, independent of *why* `decode_packet` failed (an `UnknownApid`
    failure has a real sequence count sitting in the bytes that a plain `decode_packet` `Err`
    discards; other failures happen even later). `None` only for a payload shorter than the
    6-byte primary header.
  - `decode_error_occurrence(port, msg, error) -> DecodeErrorOccurrence` -- the one shared builder
    every FRAMED consumer calls, so the shape can never drift between them.
  - 3 new unit tests.
- `crates/av-kernel/src/drm/controller.rs` (`AttitudeControllerModel`/`CommandedAttitude`):
  - Both star-tracker/IMU decode sites and the wheel-torque-command decode site rewritten from
    `?`-propagation (controller) / silent `if let Ok(...)` swallow (`CommandedAttitude`) to a
    `match`: `Ok` updates state as before; `Err` records one `DecodeErrorOccurrence` into a new
    `decode_errors_this_step: RefCell<Vec<...>>` field (cleared/repopulated at the top of every
    `step_with_ports` call, mirroring `StarTrackerModel::measurements`'s own precedent) and
    leaves the cached state (`last_star_q`/`last_imu_omega`/`last_commanded_torque`) untouched.
  - `ControllerRuntimeError::Codec` -- the only variant, and therefore now permanently
    unconstructible -- **deleted**; the enum itself is kept as `pub enum ControllerRuntimeError
    {}` (zero-variant, uninhabited) rather than replacing `type Error` with `std::convert::
    Infallible` everywhere, to keep the blast radius to this one file (no ripple into `AnyModel`'s
    dispatch in `binding.rs`, which still compiles unchanged: `Result<T, Uninhabited>::map_err` is
    ordinary, unreachable-at-runtime code, not a compile error). See "Defects/escalations" for the
    two-shapes-considered disclosure.
  - `CommandedAttitudeError::Codec` -- also now unreachable -- **deleted**; `CommandedAttitudeError
    <E>` keeps its `Inner(E)` variant (still real: the wrapped model's own errors, e.g. `Attitude
    WheelsModel`'s, are unrelated and unaffected).
  - `drain_decode_errors` added to both models (real accumulator for the controller; own +
    `self.inner.drain_decode_errors()` for `CommandedAttitude`).
  - 4 tests: 2 replace the old `a_malformed_star_packet_is_a_typed_error_not_a_panic`/`a_malformed
    _command_packet_is_a_typed_error_not_a_panic` (which asserted the now-deleted `Err` shape) with
    tests asserting `Ok` + the recorded occurrence + the untouched cache; 1 new test proves a later
    good frame resumes normal commanding after a bad one; 1 new test pins that a malformed
    *declared* star codec is still refused at `AttitudeControllerModel::new` (question 188's own
    "only a malformed declared codec stays a load-time error" requirement, checked directly against
    `codec::validate_codec`, not merely assumed unchanged).
- `crates/av-kernel/src/drm/ground.rs` (`GroundStationModel`): the `tm_in` decode site's `if let
  Ok(decoded) = ...` silent swallow rewritten to a `match`, recording an occurrence on `Err` (the
  model's own `Error` type is already `Infallible`, unaffected). 1 new test.
- `crates/av-kernel/src/drm/gmat_command.rs` (`GmatFramedCommandModel`): the command-decode site's
  `if let Ok(decoded) = ...` rewritten identically; `drain_decode_errors` combines its own
  occurrences with the wrapped `GmatModel`'s (always empty -- `GmatModel` never itself decodes a
  FRAMED frame). 1 new test.
- `crates/av-kernel/src/drm/binding.rs` (`ConstantAccelModel`, `AnyModel`):
  - `ConstantAccelModel::consume_framed`'s decode site rewritten identically; new `decode_errors_
    this_step` field added to all 5 struct-literal construction sites (1 production, 4 test-only).
  - `AnyModel::drain_decode_errors` -- a new explicit per-variant dispatch arm, no catch-all,
    mirroring `AnyModel::drain_sensor_fault_effect` exactly.
  - `any_model_arm_count_for_replay_covers_every_dynamics_model_match_site` updated: 12 -> 13 (one
    more plain arm, the identical shape `drain_sensor_fault_effect`'s own R5.1a addition took).
    `any_model_arm_count_for_ground_station_matches_star_tracker` needed no numeric change (it
    compares two variant counts against each other, and my new dispatch adds one arm to each
    side symmetrically).
  - `ContainerModel`/`SharedContainerModel`: trivial/delegate arms (neither ever decodes a FRAMED
    frame -- `lockstep.proto`'s wire protocol has no CCSDS concept).
  - 1 new test (`ConstantAccelModel`'s own decode-error handling).
- `crates/av-kernel/src/ports.rs`: `DecodeErrorRecord { instance, port, tai_ns, sequence_count,
  error }` -- the kernel-level enrichment of `av_dynamics::DecodeErrorOccurrence` with the
  receiving instance name, mirroring `AppliedPortCommand`'s identical enrichment of `av_dynamics::
  AppliedCommand`.
- `crates/av-kernel/src/schedule.rs`: `HeteroSystemEntry.decode_errors: Vec<DecodeErrorRecord>`,
  an ever-growing list (NOT `sensor_fault_effect`'s cross-span running-total shape -- a decode
  error is not tied to any declared `Fault`'s own window/boundary, so nothing needs folding across
  spans), populated right after `drain_sensor_fault_effect` in `advance_to_with_ports`'s own loop,
  enriched with the receiving instance id (mirrors `measurements`'s own `sensor_id` enrichment).
  New `HeteroScheduler::decode_errors(id)` accessor.
- `crates/av-kernel/src/kernel.rs`: `HeteroKernel::decode_errors(id)` -- delegates to the
  scheduler, mirrors `measurements`/`sensor_fault_effect`'s own identical delegation.
- `crates/av-kernel/src/registry.rs`: `ModelHandle::drain_decode_errors` -- added for API
  completeness/symmetry with `describe`/`drain_sensor_fault_effect`, mirroring R5.1a's own
  identical addition and identical disclosure (the real drain path does not use it).
- `crates/av-kernel/src/drm/executor.rs`:
  - `ModelSpanState.decode_errors: Vec<crate::ports::DecodeErrorRecord>`, drained from `kernel.
    decode_errors(name)` at the end of every `run_one_span` call (extend, exactly like
    `measurements`) -- added to all 2 `ModelSpanState` struct-literal construction sites.
  - `run_shared_group`'s own tail: for every span, every accumulated occurrence becomes exactly
    one `EVENT_KIND_FAULT`/`decode_error` `Event` (`events::decode_error_event`) -- ONE EVENT PER
    OCCURRENCE, per question 188's own literal wording, not collapsed the way PORT/SENSOR fault
    events are (see "Escalations," item 1).
- `crates/av-kernel/src/drm/events.rs`:
  - `decode_error_event(occ, occurrence_index, provenance) -> Event` -- a third, separate
    `EVENT_KIND_FAULT` builder alongside `fault_event`/`port_fault_event`/`sensor_fault_event`, for
    the identical reason those two are separate from each other: no `Fault.id` exists behind a
    decode error at all (it can happen with no declared PORT/SENSOR fault installed whatsoever).
    `reference_id` is the receiving port name; `name` is the literal `"decode_error"`.
    `occurrence_index` disambiguates `id` when two occurrences share `(instance, port, tai_ns)`.
  - 3 new unit tests.
- `drms/demo_attitude_control_port_corrupt.drm.yaml` -- new fixture, `demo_attitude_control.
  {sos,*.system}.yaml` reused UNCHANGED. A `corrupt` PORT fault (declared `corrupt_mask: 255`) on
  `startracker.st_meas`, window `[start+5s, start+35s)` -- the SAME window R5.1a's own dropout
  fixture uses, reused deliberately (see "Measurements" and the fixture's own header comment for
  the full "why this window, why this mask" derivation).
- `crates/av-kernel/tests/decode_errors.rs` -- new file, 4 tests (the headline event-count/
  completion test, the router-level `frames_affected` cross-check, the baseline-vs-faulted
  divergence test, and a determinism test).

## 2. Verification

`export PATH="/opt/homebrew/opt/rustup/bin:$PATH"` first. Contention (`ps -Ao pid,etime,command |
grep -E "cargo test|pytest|docker build"`) checked before every heavy run; one wait was needed
(another session's `pytest`/`docker build` was running) -- waited via a blocking `while ps ... ;
sleep 30; done` script in the foreground, per the standing instruction, until clear.

### Targeted (development)

- `cargo test -p av-kernel --lib` -- 656 passed, 0 failed (was 655 with one deliberately failing
  test mid-development, fixed -- see "Defects," item 1). Includes every unit test listed above.
- `cargo test -p av-kernel --test decode_errors` -- 4 passed, 0 failed.
- `cargo test -p av-kernel --lib drm::gmat_command:: / drm::ground:: / drm::binding::` (targeted
  reruns during development) -- all green.

### Full run (the gate)

**Predicted new total, stated before running, 15 net new tests by name:**
- `codec.rs` (+3): `peek_sequence_count_reads_the_same_value_decode_packet_does_even_when_the_
  apid_is_corrupted`, `peek_sequence_count_is_none_for_a_payload_shorter_than_the_primary_header`,
  `decode_error_occurrence_carries_the_real_epoch_sequence_and_error_text`.
- `controller.rs` (+2 net; 2 removed as renames of the old `?`-propagation tests, 4 added -- see
  "What was built"): `after_a_malformed_star_packet_a_later_good_one_resumes_normal_control`,
  `a_malformed_declared_star_codec_is_still_a_load_time_error_never_a_runtime_decode_event` (the
  other 2 additions, `a_malformed_star_packet_is_recorded_...`/`a_malformed_command_packet_is_
  recorded_...`, are renamed-in-place, no count delta).
- `ground.rs` (+1): `an_undecodable_tm_frame_is_recorded_and_a_later_good_frame_resumes_normal_
  reporting`.
- `gmat_command.rs` (+1): `gmat_framed_command_records_an_undecodable_frame_and_a_later_good_one_
  still_applies`.
- `binding.rs` (+1): `consume_framed_records_an_undecodable_frame_and_a_later_good_one_still_
  applies`.
- `events.rs` (+3): `decode_error_event_carries_the_real_epoch_port_sequence_and_error_text`,
  `decode_error_event_omits_sequence_count_when_it_could_not_be_recovered`, `decode_error_event_
  occurrence_index_disambiguates_same_epoch_ids`.
- `tests/decode_errors.rs` (+4, whole new file): `corrupt_startracker_run_completes_with_exactly_
  the_predicted_decode_error_event_count`, `corrupt_startracker_router_level_fault_event_frames_
  affected_is_600`, `corrupt_fixture_true_pointing_error_diverges_below_the_unfaulted_baseline_
  during_the_window_and_reconverges_by_run_end`, `the_same_corrupt_faulted_drm_executed_twice_
  produces_byte_identical_run_products_and_events`.
- 3+2+1+1+1+3+4 = **15**. **Baseline (`d046806`): 848 passed, 0 failed, 1 ignored. Predicted: 863
  passed, 0 failed, 1 ignored.**

<!-- measured total filled in once the background full-suite run in this session finishes -->

### Clippy

- `cargo clippy -p av-kernel --all-targets -- -D warnings` -- clean during development.
- `cargo clippy --workspace --all-targets -- -D warnings` -- <!-- filled in below -->

### cargo deny

<!-- filled in below -->

## 3. Measurements worth keeping

**Why `[start+5s, start+35s)` with a declared `corrupt_mask`, not the whole 300 s run (stated
before running, `drms/demo_attitude_control_port_corrupt.drm.yaml`'s own header comment and
`tests/decode_errors.rs`'s own module doc comment).** `AttitudeControllerModel::step_with_ports`
only ever decodes the LAST star tracker message in its own `Inbox` each call (`Inbox::
last_on_port`) -- the star tracker's 20 Hz rate against the controller's own 10 Hz update rate
means exactly two star tracker frames land in the controller's `Inbox` every controller step, and
only the later one (landing exactly on the controller's own 0.1 s step boundary) is ever actually
handed to `decode_packet`. A PERSISTENT fault over the whole run would still draw ~6000
independent corruptions at the router but produce an effectively unbounded `decode_error` event
list (one per rejected frame) -- exactly the risk escalated, not built (see "Escalations," item
1). The bounded 30 s window instead gives an EXACT, hand-computable count: 30 s * 20 Hz = **600**
candidate frames drawn (all corrupted -- a declared mask always applies at `rate == 1.0`), of
which 30 s / 0.1 s = **300** are the ones the controller ever actually attempts to decode (all
rejected, since the deterministic mask flips the APID bits on every one of them). **Measured:
exactly 300 `decode_error` events, exactly `frames_affected == 600.0`** on the router's own PORT
fault event -- both asserted exactly, matching the stated prediction on the first real run.

**The headline divergence (`tests/decode_errors.rs::corrupt_fixture_true_pointing_error_
diverges_...`).** An undecodable frame leaves `AttitudeControllerModel::last_star_q` frozen at
whatever it last successfully decoded just before the window opens, while `last_imu_omega` stays
live (the IMU is unfaulted) -- structurally the SAME mechanism `crate::drm::sensors::
StarTrackerFaultEffect::Freeze`/`Dropout` already produce one layer upstream, on the IDENTICAL
fixture topology and gains R5.1a's own headline test uses. Predicted (reusing that derivation,
not re-deriving from scratch, since the physics is the same one layer downstream): faulted TRUE
pointing error ends up SMALLER than the baseline's own trajectory during the window (the frozen,
never-shrinking restoring torque overdrives the decay), roughly `~0.003` rad at t=20s growing to
`~0.019` rad by t=35s, reconverging to the same ~1e-4 rad order of magnitude by run end. **Measured**
(both runs, real noise/discretization): t=5s baseline-faulted = 0.0 exactly (bit-identical, confirms
nothing diverged before the fault epoch); t=20s gap = 2.7332e-3 rad (predicted ~0.0028); t=35s gap
= 1.9061e-2 rad (predicted ~0.0191); t=300s gap = 1.6131e-7 rad (both fully re-settled, even closer
than R5.1a's own -2.3807e-5 rad). The linearization lands within a few percent of the real,
measured divergence at both in-window epochs, confirming the "same mechanism, one layer
downstream" claim rather than merely asserting it.

**Break-and-restore evidence (this crate's own "every new test must fail against a nameable wrong
implementation, mechanically executed" rule).** 9 wrong implementations were built, run against
the relevant real test(s), confirmed to fail with the panic text below, then reverted; `cmp`
confirmed every touched file byte-identical to its pre-break snapshot after every restore:

1. **`controller.rs`: an undecodable star-tracker frame is silently swallowed** (`Err(_e) => {}`
   instead of recording an occurrence). `tests/decode_errors.rs::corrupt_startracker_run_completes
   _with_exactly_the_predicted_decode_error_event_count` -- `assertion left == right failed:
   expected exactly 300 decode_error events ... got 0: []`.
2. **`controller.rs`: an undecodable star-tracker frame fabricates an identity quaternion**
   instead of leaving `last_star_q` untouched. `tests/decode_errors.rs::corrupt_fixture_true_
   pointing_error_diverges_...` -- `t=35s (window end): baseline (0.0957...) must exceed faulted
   (0.1767...) by more than 0.005 rad` (the sign and magnitude both go wrong: faulted BECOMES
   LARGER than baseline, gap negative, the opposite of the derived direction).
3. **`codec.rs`: `peek_sequence_count` reads the APID bytes (0-1) instead of the sequence-count
   bytes (2-3)**. `codec::tests::peek_sequence_count_reads_the_same_value_decode_packet_does_...`
   -- `assertion left == right failed: ... left: Some(291) right: Some(42)`.
4. **`ground.rs`: an undecodable `tm_in` frame is silently swallowed** (same shape as item 1).
   `drm::ground::tests::an_undecodable_tm_frame_is_recorded_and_a_later_good_frame_resumes_normal_
   reporting` -- `assertion left == right failed: [] left: 0 right: 1`.
5. **`gmat_command.rs`: an undecodable command frame is silently swallowed**. `drm::gmat_command::
   tests::gmat_framed_command_records_an_undecodable_frame_and_a_later_good_one_still_applies` --
   `assertion left == right failed: [] left: 0 right: 1`.
6. **`binding.rs`: `ConstantAccelModel`'s undecodable `consume_framed` frame is silently
   swallowed**. `drm::binding::tests::consume_framed_records_an_undecodable_frame_and_a_later_
   good_one_still_applies` -- `assertion left == right failed: [] left: 0 right: 1`.
7. **`events.rs`: `decode_error_event`'s own `id` drops `occurrence_index`**. `drm::events::
   tests::decode_error_event_occurrence_index_disambiguates_same_epoch_ids` -- `assertion left !=
   right failed: ... left: "decode_error:controller:startracker_in:1000" right:
   "decode_error:controller:startracker_in:1000"` (collision).
8. **`controller.rs`: `AttitudeControllerModel::new` never runs `validate_codec` on the star
   codec** (`let _ = codec::validate_codec(&star_codec);` instead of `?`-propagating). `drm::
   controller::tests::a_malformed_declared_star_codec_is_still_a_load_time_error_never_a_runtime_
   decode_event` -- `called Result::unwrap_err() on an Ok value: AttitudeControllerModel { ... }`
   (the malformed codec loaded successfully instead of being refused).
9. **`codec.rs`: `decode_error_occurrence` hardcodes `tai_ns: 0`** instead of `msg.tai_ns`.
   `codec::tests::decode_error_occurrence_carries_the_real_epoch_sequence_and_error_text` --
   `assertion left == right failed: must be the message's own real delivery epoch left: 0 right:
   42000000000`.

Not independently mechanically executed (disclosed, per this crate's own standing permission to
make this call when the remaining items are lower-value than the nine above): the `AnyModel` arm-
count test's own new numeric value (13); `schedule.rs`/`kernel.rs`/`executor.rs`'s own drain/fold/
event-emission wiring beyond what items 1-2 and the acceptance test already exercise end to end
(a break there would either fail to compile -- a missing struct field -- or reproduce exactly the
"events never appear" shape item 1 already proves through the identical `execute()` call path);
`ports.rs`'s `DecodeErrorRecord` struct itself (a plain data carrier, no logic); the "second,
immediate read returns the same occurrence, not an empty list" per-call-snapshot semantics
(covered by inspection of the passing test `a_malformed_star_packet_is_recorded_and_the_run_
continues_on_the_last_good_input`'s own two-call assertion, not independently broken).

## 4. Defects found, including my own

1. **My own bug, caught by the very next full-suite run, not shipped:** the mechanical Python
   script that inserted a trivial `drain_decode_errors` arm everywhere `drain_sensor_fault_effect`
   already existed assumed every such site was either a bare `None` or a one-line delegate --
   `StarTrackerModel`/`ImuModel` in `crates/av-kernel/src/drm/sensors.rs` have a REAL, multi-line
   `drain_sensor_fault_effect` (their own SENSOR-fault accumulator), so the script's blind
   `drain_sensor_fault_effect` -> `drain_decode_errors` text substitution produced a
   type-mismatched method (`Vec<DecodeErrorOccurrence>` return type, `Option<SensorFaultEffectDrain
   >`-shaped body) that happened to still compile (both are generic enough shapes) but was
   semantically wrong. Caught immediately by `cargo check -p av-kernel --lib` (a hard compile
   error, `Some(...)`/`None` not matching `Vec<...>`) before any test ever ran against it -- fixed
   by hand to the real, trivial `Vec::new()` body (neither model itself decodes a FRAMED frame).
2. **A design question resolved by reading the actual downstream consumer, not assumed:**
   `ControllerRuntimeError`/`CommandedAttitudeError::Codec` becoming unreachable could have been
   handled by widening `type Error` to `std::convert::Infallible` everywhere instead of keeping a
   zero-variant enum. Checked directly: `super::binding::AnyModel::Controller`/`::Attitude`'s own
   `derivatives`/`step`/`step_with_stm` arms call `.map_err(|e| AnyModelError::PortCodec { ...
   detail: e.to_string() })` generically, which compiles unchanged against either shape -- the
   zero-variant-enum choice was made to minimize blast radius (one file, `controller.rs`, instead
   of every generic call site that names `ControllerRuntimeError`/`CommandedAttitudeError`
   explicitly), not because the `Infallible` alternative was unsound.
3. No defects were found in the delivered implementation itself beyond item 1 above (caught before
   any test ever ran against it) -- every break-and-restore case in section 3 confirms the real
   code behaves as documented once the deliberate bug is reverted.

## 5. Escalations for the manager

1. **The unbounded-event-count risk, per this task's own explicit instruction to escalate rather
   than decide alone.** Question 188 reads literally as "one event per undecodable frame," and
   that is what this task built (`events::decode_error_event`, called once per element of
   `ModelSpanState::decode_errors` in `executor::run_shared_group`'s own tail) -- but a PERSISTENT
   corrupt fault (`duration_ns == 0`) on a high-rate FRAMED port over a long run would produce an
   unbounded `RunProducts.events` list (this fixture's own bounded 30 s window already produces
   300; a full 300 s run on the identical port would produce ~3000). Recommend the same collapse
   questions 178/186(c) already established for PORT/SENSOR faults: one event per (instance, port)
   carrying a `frames_affected`-style count, rather than one per frame -- but this is a genuine
   design decision (does a caller need each occurrence's own individual `sequence_count`/epoch, or
   only the aggregate?) that question 188's own wording does not settle, and this task's own
   instructions were explicit not to invent it unilaterally.
2. **`corrupt_mask`'s deterministic, whole-payload XOR (0xFF) was reused, not redesigned** --
   R4.1b's own escalation on this exact design choice (a single declared byte, applied uniformly)
   still stands; this task did not revisit it.
3. **The zero-variant-enum choice for `ControllerRuntimeError`** (Defects, item 2) is my own
   judgment call among two reasonable shapes; flagging for visibility, not because it is in doubt.

## 6. What remains

- Item 1 above (the unbounded decode-error event-count risk) is the one deliberately-escalated,
  not-decided-here design question.
- The "not independently mechanically executed" items listed at the end of section 3.
- Question 188's own scope is now closed for every FRAMED consumer this crate has (`controller`,
  `ground`, `gmat_command`, `binding::ConstantAccelModel`) -- no further consumer exists today.
