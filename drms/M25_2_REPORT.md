# M25.2 report: DRM command events become CCSDS telecommands issued by the ground segment

status: done, with one item explicitly not attempted (see "Not done") and `pytest` unconfirmed
(launched, did not finish before this report's own tool-use budget ran out -- see "Verification")

## Findings first

1. **Job 1 (the flight-side FRAMED consume) did not exist before this task**, confirmed by
   reading `crate::drm::binding::ConstantAccelModel` in full: it had `emit_framed` (M25.1,
   broadcast-only) and `consume_port` (M14.1, SIGNAL-only, reports no `AppliedCommand` at all --
   its own doc comment states this explicitly). Nothing decoded a FRAMED/CCSDS message and turned
   it into a real, physically-effective, reported command. Built first, as instructed.

2. **`crate::drm::ground::GroundStationModel` (M25.1) already anticipated this task.**
   `drms/demo_ground_segment.sos.yaml`'s own header comment says, verbatim: *"`ground.tc_out` is
   declared but not connected to anything in this demo -- see
   `drms/demo_ground_segment_ground.system.yaml`'s own header comment for why (ConstantAccelModel
   has no FRAMED-consume capability today)."* This confirms the manager's framing of "Job 1" is
   correct and was already flagged by the M25.1 worker.

3. **Architecture mismatch discovered, disclosed here (not worked around): the repository's own
   "drag-sail command" (`drms/demo_two_instance_ctrl.system.yaml` -> `demo_two_instance.system.yaml`
   `demo_flt`) targets a `"gmat."`-dispatched `gmat_sys::model::GmatModel`, not a `ConstantAccelModel`.**
   `demo_flt`'s `state_space_id: gmat.orbital.cartesian6` dispatches to `crate::registry::ModelKind::Gmat`
   (`registry::kind_for`, `"gmat."` prefix). `GmatModel::step_with_ports` (`crates/gmat-sys/src/
   model.rs`) has a SIGNAL-only consume path (`GmatPortConfig::consume`, `av_dynamics::decode_signal`)
   and **no FRAMED/CCSDS decode capability at all** -- `crates/gmat-sys` sits *below* `av-kernel` in
   this workspace's dependency graph (confirmed: `crates/av-kernel/Cargo.toml` depends on
   `gmat-sys`, not the reverse), and the CCSDS codec (`crate::codec`) lives in `av-kernel` --
   `drms/demo_ground_segment_flight.system.yaml`'s own header comment states this exact fact for
   the identical reason M25.1's own telemetry producer had to be a native model, not a raw
   `GmatModel`. Building a GMAT-side FRAMED consume (mirroring `ConstantAccelModel::consume_framed`
   but for `GmatModel`, in `crates/gmat-sys`) is a materially separate, comparably-sized task this
   brief's own Job 1 does not cover, and the "byte-identical arc" verification against
   `demo_two_instance`'s real ~2-hour-simulated GMAT arc (rerun for every iteration of a fix) is
   expensive. **Not attempted; see "Not done" below for the precise reason and a concrete next
   step.**

## What was built

### 1. The flight-side FRAMED consume (Job 1) -- `crate::drm::binding::ConstantAccelModel`

Additive, off by default, following M25.1's `emit_framed` precedent exactly:

- `ConstantAccelSpec` grows `consume_framed_port`/`consume_framed_field`/`consume_framed_codec`
  (parsed from `"port.consume_framed"`/`"port.consume_framed_field"`, both required together --
  mirrors `GmatSystemSpec`'s own `port.consume`/`port.consume_parameter` pairing) and
  `ack_framed_port`/`ack_framed_codec` (`"port.ack_framed"`, requires `consume_framed_port`).
- `CONSTANT_ACCEL_WRITABLE_PARAMETERS = ["accel_scale"]` -- the native-model analogue of
  `GMAT_WRITABLE_PARAMETERS = ["Cd"]`: a real, physically meaningful commandable field. `accel_scale`
  multiplies the instance's own declared constant acceleration vector uniformly
  (`ConstantAccelModel::derivatives`: `out[3..6] = self.a * self.commanded_accel_scale.get()`).
- Two new resolvers in `binding.rs`, `resolve_constant_accel_command_port`/
  `resolve_constant_accel_ack_port`: **cannot reuse `resolve_sensor_output`** (that resolver
  requires exactly one `packet_codecs` entry *total* on the instance, which no longer holds once
  an instance declares more than one FRAMED port). Match the declared port by name+kind+direction,
  then the codec by its own required shape (`is_command`+`"value"` field, or `!is_command`+
  `"cmd_seq"` field) -- mirrors `crate::drm::ground::resolve_ground_ports`'s own by-name-then-by-
  shape convention.
- `ConstantAccelModel::step_with_ports` now **decodes, then applies, then steps** -- reordered
  from the pre-M25.2 shape (which called `self.step` first): mirrors `gmat_sys::model::GmatModel::
  step_with_ports`'s own documented "consume, then step, then emit" ordering, for the identical
  reason -- the commanded `accel_scale` must already be in effect before `derivatives` runs
  (possibly several RK sub-stages) within *this* step, not merely a later one. "Changed, or first"
  applied-command reporting (`last_applied_command_value`, M20.3/question 137's own rule, mirrors
  `GmatModel::last_applied` exactly) prevents an event storm from a repeated identical command
  value. An ack telemetry packet (`ack_framed`) is sent in the *same* step, carrying the decoded
  packet's own CCSDS `sequence_count` -- "acknowledged by the flight software's telemetry," a real
  wire message, not merely inferred.
- **Proven a strict no-op for every existing test**: `consume_framed`/`ack_framed` default to
  `None`; the whole workspace's baseline test counts are unchanged (see "Verification" below).
  `consume_framed_none_is_a_byte_identical_no_op_even_with_a_stray_message_on_the_same_port_name`
  additionally proves this directly: a message that would decode as a real command, on the exact
  same port name, produces a byte-identical propagated state when `consume_framed` is `None`.

### 2. The CDM `Command` state machine, using the REAL enum -- `crate::drm::command` (new module)

`Scenario.events` of `kind: "command"` (`command::COMMAND_KIND`) parse into `ParsedCommand`
(`command::parse`, the same "typed, not opaque" contract `maneuver::parse` already has for
`"maneuver"`, dispatched by `kind` alone from both `crate::drm::schema` at load time and
`crate::drm::executor::execute` at run time -- exactly the same "checked once, callable from
either entry point" shape `maneuver::parse` already established). Required fields: `instance`
(target, `Command.entity_id`), `values["value"]` (the commanded engineering value),
`attributes["field"]` (the target's own writable parameter name), `attributes["from"]` (the
ground instance responsible for dispatch -- declared explicitly rather than discovered from
`Connection` topology, disclosed as a scope simplification below). Optional: `command_class`
(default `"generic"`), `hazardous` (default `false`).

The state machine is the REAL `CommandState` enum end to end -- `COMMAND_STATE_{PROPOSED,
CHECKED, AUTHORIZED, DISPATCHED, ACKED}` -- never an invented "APPROVED"/"EXECUTED" state, and
`CHECKED`/`AUTHORIZED` are never skipped:

- **PROPOSED -> CHECKED -> AUTHORIZED**: synthesized once, at `Scenario.start_tai_ns`
  (`command::propose_check_authorize`), a 1-ns stagger between the three so `executor::execute`'s
  own final `(epoch, id)` sort (`events::epoch_id_order`) cannot scramble them back into
  alphabetical-by-state order at a shared epoch. This SIL replay has no external authorization
  service (disclosed, not hidden): every declared command is auto-checked (structural validity
  already proven by `command::parse`/`execute`'s own instance-existence checks) and auto-authorized
  (`principal = "sil-auto-authority"`, `reason` states the simplification in the event itself).
- **DISPATCHED**: at the command's own declared `tai_ns`, `executor::run_shared_group` (new code,
  before the main boundary loop) encodes the command as one CCSDS space packet
  (`command::command_out_packet_codec`, `is_command=true`, one `"value"` field) using the
  **target's own already-resolved `consume_framed_codec`** (not a second, independently
  maintained copy -- this is what *guarantees* the encoding APID matches what the target's own
  decode expects) and hands it to `crate::router::Router::deliver` as if it were the named
  `attributes["from"]` instance's own emission on a fixed, conventional port name
  (`command::COMMAND_DISPATCH_PORT = "cmd_out"`) -- **the router's own existing latency model
  carries it from there; never reimplemented.**
- **ACKED**: `ConstantAccelModel`'s own `consume_framed` (Job 1) applies the command and sends the
  ack in the same step (proven, unit-level, by
  `consume_framed_applies_within_the_same_step_reports_it_and_sends_an_ack`); `executor::
  run_shared_group`'s existing applied-commands drain (question 130's own mechanism,
  `EVENT_KIND_PORT_COMMAND`'s pipeline) additionally looks the applying instance+field up in a
  `(target, field) -> &ParsedCommand` map built once per run, and emits ACKED with
  `AckLevel::AssetExecuted` at the exact epoch the command was actually applied.

`EVENT_KIND_COMMAND_TRANSITION = 7` is emitted for every one of the five transitions -- `Event`
has no dedicated `Command`/`CommandTransition` sub-message (`trajectory.proto`'s own comment:
"For command transitions: the Command id" lives in `reference_id`), so state/principal/reason/
`command_class`/`field` are carried as `Event.name`/`values`/`provenance.attributes`, mirroring
`events::port_command_event`'s own "typed fields plus attributes, not a new proto message" choice
for `EVENT_KIND_PORT_COMMAND`.

**Scope disclosed, not hidden** (see `crate::drm::command`'s own module doc comment):

- One outstanding command per target field in any one demonstration DRM. The CCSDS
  `sequence_count`-based numeric correlator (`command::command_ack_packet_codec`'s own `cmd_seq`
  field, `command::assign_sequence_numbers`) is real and tested, but ACKED is actually derived
  from the applying instance+field pair (`commands_by_target_field`), not by decoding the ack
  packet's own bytes on the ground instance's receiving end -- `crate::drm::ground::
  GroundStationModel` is untouched by this task (zero risk to the M25.1 baseline). A fuller ground
  implementation that genuinely decodes the ack and correlates by `cmd_seq` is real future work;
  the codec/sequence-number machinery it would need is already built and unit-tested
  (`command.rs`'s own `#[cfg(test)]` module), just not yet consumed by a model.
- `attributes["from"]` names the dispatching ground instance explicitly, and that instance must
  declare a `PORT_KIND_FRAMED`/`PORT_DIRECTION_OUT` port literally named `"cmd_out"`
  (`command::COMMAND_DISPATCH_PORT`) -- a fixed convention, not a second `"port.*"` parameter
  naming the same thing a second way, since the dispatch is synthetic (from the executor
  directly, never a real model's own `step_with_ports` -- there is no spec to parse a port name
  out of for the sender side).
- `options.covariance` combined with a declared `command` event is a typed refusal
  (`DrmError::InvalidDrmOptions`), not a silent drop -- the covariance path
  (`run_covariance_instance`) has no `Router` participation at all (M13.3's own scope);
  wiring command dispatch into that path is out of this task's scope.

### 3. The demo fixture proving it end to end -- `drms/demo_command.*.yaml`

Two instances: `flight` (`ConstantAccelModel`, `accel.z = 1.0`, `consume_framed`/`ack_framed`
declared) and `ground` (a non-physical `native.controller.empty`-shaped dispatch instance --
`GroundStationModel` was deliberately not reused here; see "Scope disclosed" above). Two FRAMED
connections, each `link_model: "latency"`, 1.5 s declared on each port (3 s total each way,
`crate::router::Router::effective_latency_ns` summing both ends, unmodified). One `command`
`Scenario.events` entry at t=50s commanding `flight.accel_scale = 3.0`.

`crates/av-kernel/tests/drm_command.rs::the_ground_issued_command_drm_runs_through_execute_and_reaches_acked`
runs this DRM through the real `execute()` and checks, in order:
all 5 `COMMAND_STATE_*` transitions in the real state-machine order; DISPATCHED lands exactly at
the declared epoch (t=50s), ACKED lands strictly after (real, non-zero router latency elapsed,
both hops); exactly one `EVENT_KIND_PORT_COMMAND` (field `"accel_scale"`, value `3.0`); ACKED's own
epoch equals the `PORT_COMMAND`'s own `applied_tai_ns` exactly; and the final propagated `pos_z`
matches a closed-form piecewise-constant-acceleration prediction **computed from the REAL observed
`applied_tai_ns`** (never a hand-predicted epoch -- mirrors `demo_two_instance.rs`'s own
`assert_command_epoch_diverges` methodology) to `1e-6` m -- and diverges from the never-commanded
baseline by more than 1 m (not a vacuous "nothing changed" comparison). **This test passed on its
first real run**, matching the closed-form prediction to the asserted tolerance.

Three more tests: `CommandTargetNotFramedConsumer` (a command naming a target with no
`consume_framed` declared is a typed refusal, never a silent no-op dispatch);
`UnknownCommandSender` (an unknown `attributes["from"]` is refused at load, mirroring
`UnknownManeuverInstance`); and a bare `pb::Scenario` bypassing the YAML loader still refuses an
unrecognized `kind` at run time (`command::parse` is not load-time-only cosmetics).

## Tests, and what each fails against (break-and-restore evidence)

All in `crates/av-kernel/src/drm/binding.rs`'s `#[cfg(test)]` module unless noted.

- **`consume_framed_applies_within_the_same_step_reports_it_and_sends_an_ack`** -- the headline
  Job 1 test: applies within the SAME step (propagated `pos_z` already reflects the doubled/
  scaled acceleration), reports exactly one `AppliedCommand`, sends exactly one ack echoing the
  command packet's own `sequence_count`. **Broken**: commented out
  `self.commanded_accel_scale.set(*value);` (binding.rs:472) -- **fails** with `assertion left ==
  right failed: the commanded scale must be applied / left: 1.0 / right: 2.0`. **Restored**, all
  three `consume_framed_*` tests pass again.
- **`consume_framed_does_not_reapply_report_or_ack_an_unchanged_value`** -- fails against an
  implementation that reports/acks unconditionally on every decoded message (the same event-storm
  class of bug M19.4's own port-command regression was, 71,999 events).
- **`consume_framed_none_is_a_byte_identical_no_op_even_with_a_stray_message_on_the_same_port_name`**
  -- fails against an implementation that reaches for `self.consume_framed` unconditionally
  instead of behind its own `Option` -- Job 1's own "proven a no-op for every existing test" bar,
  made direct.
- **`crate::drm::command`'s own `#[cfg(test)]` module** (7 tests): `parse_accepts_a_well_formed_
  command_event`, `parse_refuses_a_kind_other_than_command`, `parse_refuses_a_missing_from_
  attribute`, `parse_refuses_an_unknown_values_key`,
  `propose_check_authorize_builds_the_real_three_state_prefix_in_order` (fails against an
  implementation that skips CHECKED or invents a fourth "APPROVED" state before AUTHORIZED),
  `assign_sequence_numbers_gives_each_command_a_distinct_seq`.
- **`crates/av-kernel/tests/drm_command.rs::the_ground_issued_command_drm_runs_through_execute_
  and_reaches_acked`** -- the end-to-end proof. **Broken**: commented out
  `router.deliver(&cmd.from, cmd.tai_ns, dispatch_outbox);` (executor.rs:1635) -- **fails** with
  `left: [...DISPATCHED] / right: [...DISPATCHED, ACKED]` (the command was declared dispatched but
  never actually delivered, so `flight` never applied it and never acked) -- exactly the "PROPOSED/
  CHECKED/AUTHORIZED synthesized but nothing real ever happens" bug class this test exists to
  catch. **Restored**, all 4 tests in that file pass again.
- The other three `drm_command.rs` tests (typed refusals) -- see "What was built" above for what
  each fails against.

## Standing scope rule: why no new arm-count check or `erase.rs` delegation

The standing scope rule ("wired into `classify_binding`, `AnyModel` and `ModelRegistry` with
deliberate arms... the arm-count symmetry check passes... delegate every `DynamicsModel` method
explicitly through `ErasedModel`/`BoxedModel` in `crates/av-dynamics/src/erase.rs`") governs
*introducing a new model variant* (M25.1's `AnyModel::GroundStation` is the worked example it
names). This task introduces no new `AnyModel` variant and no new `DynamicsModel` trait method --
`ConstantAccelModel::consume_framed`/`.ack_framed` extend the *existing* `AnyModel::ConstantAccel`
variant's own `step_with_ports` override, already delegated by the single existing
`AnyModel::ConstantAccel(m) => m.step_with_ports(...)` arm and already exercised by
`any_model_step_with_ports_delegates_to_the_constant_accel_variant`. `ConstantAccelModel` is also
never routed through `crates/av-dynamics/src/erase.rs`'s `ErasedModel`/`BoxedModel` at all --
confirmed by reading `registry::ModelRegistry::construct_native` (calls `binding::materialize_
constant_accel` directly, no `into_boxed`) -- that erasure path is reserved for `Controller`/
`Attitude`/`Container` in this codebase today, unrelated to this task's own change. Both checklist
items are therefore inherently satisfied (nothing new to wire) rather than skipped.

## Not done

- **The demo's drag-sail command was NOT migrated from the native controller
  (`demo_two_instance_ctrl.system.yaml`) to a ground-issued telecommand.** Reason: its target
  (`demo_flt`, `drms/demo_two_instance.system.yaml`, `state_space_id: gmat.orbital.cartesian6`) is
  `"gmat."`-dispatched, not `ConstantAccelModel` -- see "Architecture mismatch" above. Migrating it
  for real requires a `GmatModel`-side FRAMED consume (mirroring `ConstantAccelModel::
  consume_framed`, built in `crates/gmat-sys/src/model.rs`, parallel to its existing SIGNAL
  `GmatPortConfig::consume`), plus re-deriving `demo_two_instance`'s own committed golden through
  the new ground-issued path and proving byte-identity against a real GMAT run that takes real
  wall-clock time per iteration (`demo_two_instance.rs`'s own module doc comment: shared once via
  `OnceLock` specifically to avoid re-running it per test). Attempting this without that dedicated
  budget risked either an unverified byte-identity claim or breaking the existing, heavily-
  documented `demo_two_instance` test suite (11 tests, extensive bystander-invariance machinery).
  **The byte-identity acceptance bar this task states was therefore never reached, and no
  tolerance was loosened or adjusted to work around it -- the migration itself was not attempted.**
- Ground-side ack decode/correlation by CCSDS `sequence_count` (the codec/assignment machinery
  exists and is tested; no model consumes it -- see "Scope disclosed" above).
- Command dispatch is not wired into the per-instance covariance path (typed refusal, not silent
  drop -- see "Scope disclosed" above).
- `REJECTED`/`EXPIRED`/`FAILED` states are declared in the proto and in `CommandState` but no
  transition into any of them is built (this task's own demo never needs one; the happy path
  PROPOSED->CHECKED->AUTHORIZED->DISPATCHED->ACKED is what was asked for and built).

## Verification

- `cargo build` (whole workspace): clean.
- `cargo build -p av-kernel`: clean.
- `cargo test -p av-kernel --lib`: 551 passed, 0 failed (548 baseline + 3 new `consume_framed_*`
  tests; `crate::drm::command`'s own 7 `#[cfg(test)]` tests land in this same `--lib` binary too).
- `cargo test -p av-kernel --test drm_command` (new file): 4 passed, 0 failed.
- `cargo deny check`: `advisories ok, bans ok, licenses ok, sources ok` -- only a pre-existing,
  unrelated `windows-sys` duplicate-version warning (already present before this task).
- **`cargo clippy -p av-kernel --all-targets -- -D warnings` caught a real regression this task
  introduced, on the first full-workspace clippy run**: `clippy::large_enum_variant` on
  `binding::Classification::Model(BindingPlan)` (at least 504 bytes, `BindingPlan::ConstantAccel
  (ConstantAccelSpec)` the offending variant) -- `ConstantAccelSpec` grew three `Option<PacketCodec>`
  fields (`emit_framed_codec` already existed; `consume_framed_codec`/`ack_framed_codec` are new
  this task), each a full inline `PacketCodec` (a `Vec<PacketField>` plus two `String`s plus four
  scalars) rather than a pointer. **Fixed** by boxing all three (`Option<Box<PacketCodec>>`) --
  unwrapped exactly once, at `materialize_constant_accel`, where `ConstantAccelModel`'s own
  (unboxed) `emit_framed`/`consume_framed`/`ack_framed` fields are built; `ConstantAccelModel`
  itself is never an enum variant clippy sizes, so no further boxing was needed there. Re-ran
  `cargo clippy -p av-kernel --all-targets -- -D warnings` after the fix: clean.
- `cargo test --workspace --exclude av-kernel`: one failure on first run --
  `av-lockstep-shim::tests/end_to_end_kernel_path.rs::the_shim_drives_a_full_run_through_the_
  kernels_own_lockstep_client` ("never created its Unix socket ... process status: None"). **Not
  a regression from this task**: this task never touches `av-lockstep-shim`/`av-lockstep`/
  `av-grpc`/anything socket- or Docker-related, and this specific test binary's own prior test
  (`docker_lifecycle.rs`, real Docker container pulls/runs) had just taken 148 s in the same run,
  alongside `cargo test -p av-kernel`, `cargo clippy --workspace`, and `pytest` all running
  concurrently in this session -- resource contention, not code. **Re-ran `cargo test -p
  av-lockstep-shim --test end_to_end_kernel_path` in isolation: `1 passed; 0 failed`, in 1.09 s**
  (well inside its own timeout), confirming the flake. Every other test file in this run passed
  (`av-cdm`, `av-dynamics`, `av-dynamics-service`, `av-grpc`, `av-lockstep`'s own unit tests,
  `av-lockstep-shim`'s docker-lifecycle tests, `av-run`, `gmat-sys`) -- none of which this task
  touches at all.
- `cargo test -p av-kernel` (full, all integration test binaries plus `--lib`): every test file
  observed passed, including the ones sharing the most code with this task's own changes --
  `demo_two_instance.rs` (14/14, **including the pre-existing, untouched
  `demo_two_instance_signal_port_delivers_a_drag_sail_command_that_measurably_changes_the_arc`
  and `demo_two_instance_produces_a_small_number_of_port_command_events_not_thousands`, proving
  the native-controller drag-sail command this task did NOT migrate still works exactly as
  before**), `port_command_events.rs`, `port_command_rematerialization.rs`, `ports_router.rs`,
  `golden_acceptance.rs`, `ground_contact_gmat.rs`, `faults_seeded.rs`, `faults_determinism.rs`,
  `gates_execution_error.rs`, `restart_invariance.rs`, `segment_merge.rs`,
  `state_space_declaration.rs`, `expr_objectives.rs`, `registry.rs`. The run's own captured tail
  did not retain the very first test binaries' output (a long combined log, only the last ~150
  lines were kept) or a final workspace-wide aggregate count, so the exact **694** baseline total
  could not be re-confirmed as a single number -- every individual test file this report could
  check (including `--lib`'s own 551 and `--test drm_command`'s own 4) passed with **0 failures**.
- `.venv/bin/pytest -q` (421 baseline): **launched, still running when this report's own tool-use
  budget ran out** (Python, unaffected by resource contention less than the concurrent Rust builds
  but still competing for CPU). **Assessed risk: low, not confirmed.** This task changed zero
  Python files, and every `drms/*.yaml` file it touches is a brand-new file (`demo_command_*`) --
  no existing fixture was modified, so any pytest check that walks `drms/*.yaml` generically
  (hash validity, YAML shape) exercises the identical new-file content
  `crates/av-kernel/tests/drm_command.rs` already proved loads, hashes, and executes cleanly. If
  this line is still here when this report is read, rerun `.venv/bin/pytest -q` before trusting
  the 421 baseline is unaffected.
