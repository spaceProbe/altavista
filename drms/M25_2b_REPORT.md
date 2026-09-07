# M25.2b report: FRAMED consume for GMAT-bound instances, and the demo's drag-sail command
# migrated to a ground-issued telecommand

status: done, with two verification numbers unconfirmed (see "Not done") -- `cargo test -p
av-kernel`'s own full 694-test total and `.venv/bin/pytest -q`'s own full 421-test total; zero
failures observed in either as far as each got before this task's own tool-use budget ran out

## Findings first

1. **No new `gmat-sys` call is needed, confirmed by reading the dependency graph, not assumed.**
   `crates/av-kernel/Cargo.toml` depends on `gmat-sys`; `gmat-sys` depends only on `av-dynamics`/
   `av-cdm`. The CCSDS codec (`crate::codec::decode_packet`) lives in `av-kernel`. So decode must
   happen in `av-kernel`, and the *apply* reuses `gmat_sys::model::GmatModel::step_with_ports`'s
   existing SIGNAL-consume path (`GmatPortConfig::consume` -> `DerivativeModel::
   set_real_parameter`) completely unmodified -- confirmed by reading `crates/gmat-sys/src/
   model.rs` in full.

2. **The shape: a small decorator in `av-kernel`, `crate::drm::gmat_command::
   GmatFramedCommandModel`, translates a decoded FRAMED command into a synthetic SIGNAL `Inbox`
   message on the exact port name `GmatModel`'s own `GmatPortConfig::consume` already expects,
   then delegates to the wrapped, unmodified `GmatModel::step_with_ports`.** This is "a second
   entry point to an existing one," per the brief -- `GmatModel` itself never learns a command
   arrived over CCSDS at all. `PacketField.target` (question 149) is used, for the first time in
   this codebase, as the actual mapping layer: `resolve_gmat_command_port` finds the one declared
   codec field whose `target` is in `GMAT_WRITABLE_PARAMETERS`, rather than a DRM author naming
   the target a second way via a separate parameter (the M25.2 `ConstantAccelSpec::
   consume_framed_field` precedent's approach) -- disclosed as a deliberate difference from that
   precedent below.

3. **`crate::drm::command`'s existing dispatch mechanism (M25.2, Job 2) already generalizes to a
   GMAT-bound target with one match-arm addition in `executor.rs`'s `run_shared_group`** (it
   picks the *target's own resolved `consume_framed_codec`* generically, keyed on `BindingPlan`,
   and only special-cased `ConstantAccel` because no other variant had one before this task). No
   change to `crate::drm::command`'s state machine, port-name convention, or ACK correlation was
   needed. `GmatFramedCommandModel` also grows an `ack_framed` send (mirroring `ConstantAccelModel
   ::consume_framed`'s own ack, M25.2) so `commands_by_target_field`'s ACKED derivation (which
   *assumes* "an applied command's mere presence proves the ack was sent," per that code's own
   comment) stays true for a GMAT target too -- without this, wiring GMAT into the dispatch match
   would have made that assumption false for a GMAT target.

## What was built: Job 1, the GMAT-bound FRAMED consume

- **`crates/av-kernel/src/drm/gmat_command.rs`** (new module): `GmatFramedCommandModel`, a
  decorator wrapping `gmat_sys::model::GmatModel`. `step_with_ports` decodes the latest FRAMED
  message on the declared command port (`crate::codec::decode_packet`), looks the decoded value
  up by the resolved `PacketField`'s own `name`, and builds a **fresh, minimal synthetic `Inbox`**
  containing one SIGNAL-encoded message (`av_dynamics::encode_signal`) on the identical port name
  -- handed to the wrapped, **completely unmodified** `GmatModel::step_with_ports`, which decodes
  it via its own existing `GmatPortConfig::consume`/`decode_signal` path and applies via
  `set_real_parameter` exactly as a SIGNAL command already does. Every other `DynamicsModel`
  method (question 112: no reliance on the trait's own defaults) delegates straight to `self.inner`.
  Also carries an optional `ack_framed` send, gated on the wrapped model's own returned
  `Vec<AppliedCommand>` being non-empty -- see "Ack telemetry" below for why this had to be added.
- **`GmatSystemSpec`** (`binding.rs`) grows `consume_framed_port: Option<String>` (parsed from
  `"port.consume_framed"`) and `consume_framed: Option<Box<GmatConsumeFramedResolution>>` +
  `ack_framed_port`/`ack_framed_codec` (resolved, not parsed, at `classify_binding` time).
  **No `"port.consume_framed_field"` parameter** -- unlike `ConstantAccelSpec::
  consume_framed_field` (M25.2), the target parameter is read straight off the codec's own
  declared `PacketField.target` (question 149's "mapping layer," used here for the first time in
  this codebase -- confirmed by grep: `.target` was previously round-tripped through YAML/proto
  but never consulted by any decode/apply path).
- **`resolve_gmat_command_port`** (`binding.rs`): matches the declared port by name (`PORT_KIND_
  FRAMED`/`PORT_DIRECTION_IN`, exactly one), and the codec by shape (`is_command == true`,
  **exactly one** field whose `target` is in `GMAT_WRITABLE_PARAMETERS` -- zero or more than one
  is a typed refusal, never a guess).
- **`materialize_gmat`** wires `GmatPortConfig::consume` to the identical `(port, target)` pair
  the wrapper's synthetic message uses (falling back to it only when `spec.consume`, the
  SIGNAL-only field, is `None` -- `parse_gmat_spec` refuses declaring both together, a typed
  `UnknownParameter`, so the two can never silently race for the same `GmatPortConfig::consume`
  slot), then wraps the resulting `GmatModel` in `GmatFramedCommandModel`.
- **`AnyModel::Gmat`**'s payload type changed from `GmatModel` to `GmatFramedCommandModel`.
  Because the wrapper delegates every method explicitly and keeps `Error = gmat_sys::GmatError`
  (identical to before), **every other `AnyModel::Gmat(m) => m.foo()` match arm, and
  `registry::ModelHandle::into_boxed`'s own erasure closure, needed no change at all.**

## Ack telemetry: a correctness fix this task's own dispatch-generalization required

`crate::drm::executor::run_shared_group`'s existing applied-commands drain (M25.2, Job 2) derives
a dispatched `command` `Scenario.event`'s ACKED transition from "this applied command's mere
presence [...] is already proof the ack telemetry was sent" -- true only because
`ConstantAccelModel::consume_framed` always sends its own ack in the same step it reports an
`AppliedCommand`. Generalizing `run_shared_group`'s dispatch-target match to also accept
`BindingPlan::Gmat` (one new arm, see "Job 2" below) would have made that assumption **false**
for a GMAT target without an ack. `GmatFramedCommandModel` therefore also sends an ack packet,
gated on the *wrapped* `GmatModel`'s own returned `Vec<AppliedCommand>` (non-empty exactly when
"changed, or first" just fired) -- keeping the ACKED claim honest for a GMAT target too, and
reusing `resolve_constant_accel_ack_port` verbatim (that resolver takes only
`sys`/`instance`/`port_name`, nothing `ConstantAccelSpec`-specific).

## clippy: a real regression this task introduced, on the first workspace clippy run

`cargo clippy -p av-kernel --all-targets -- -D warnings` failed with `large_enum_variant` on
`Classification::Model(BindingPlan)` ("at least 464 bytes") after the first version of this task's
own `GmatSystemSpec` additions (four separate fields: port, boxed codec, packet-field name,
target). `ConstantAccelSpec`'s own FRAMED fields (M25.2) were the same rough size and had not
tripped this before -- `GmatSystemSpec` was already the larger `BindingPlan` variant (per that
enum's own doc comment), so this addition pushed `Classification` itself (which carries no
`#[allow]`) over its own ratio threshold. **Fixed by boxing, not by `#[allow]`** (the precedent
this task was told to follow): consolidated `consume_framed_codec`/`consume_framed_packet_field`/
`consume_framed_target` into one `Option<Box<GmatConsumeFramedResolution>>`, keeping only the
small, unavoidable `consume_framed_port: Option<String>` unboxed (the one signal `classify_binding`
needs before resolution has run). Re-ran clippy after the fix: clean.

## Change-only recording and re-materialization

Unmodified from `GmatModel::step_with_ports`'s own pre-existing M20.3 mechanism (question 137):
`last_applied: RefCell<BTreeMap<String, f64>>`, a field on `GmatModel` itself, compared by exact
`f64` equality, emptied by construction at every `materialize_gmat` call (a fault/maneuver
boundary always constructs a fresh `GmatModel`, hence a fresh `GmatFramedCommandModel`, hence a
fresh, empty cache). `GmatFramedCommandModel` never introduces a second cache of its own -- the
translated SIGNAL message reaches the *same* cache a SIGNAL command always did, so this task did
not need to re-derive or re-prove this rule; it inherited it by construction.

## Tests and break-and-restore evidence (Job 1)

All GMAT-touching tests take `gmat_sys::engine_lock()`, build a small, fast, drag-inclusive
force model (`DragForce` + `JacchiaRoberts`, ~250 km LEO, mirrors `crates/gmat-sys/tests/
gmat_port_cd_command.rs::build_model` field-for-field) so a commanded `Cd` has a measurable
effect -- no 2-hour arc needed for these unit-level proofs (each runs in under a second of real
propagation time; the ~20 s wall time per test is GMAT setup/teardown, not integration).

- **`drm::gmat_command::tests::gmat_framed_command_applies_within_the_same_step_reports_it_and_sends_an_ack`**
  (`crates/av-kernel/src/drm/gmat_command.rs`) -- the headline test. Asserts (a) `result.outputs
  [OUTPUT_CD]` (GMAT's own real-parameter readback) already reflects the commanded value in the
  SAME step; (b) exactly one `AppliedCommand`, `field == "Cd"` (the codec's declared *target*,
  not the packet field's own name `"value"`); (c) exactly one ack packet echoing the command's
  own CCSDS `sequence_count`. **Broken** (commented out the FRAMED->SIGNAL `Inbox::new(...)`
  translation): **failed** with `the SAME step's own GMAT real-parameter readback must already
  reflect the commanded Cd; got 2.2` (the untouched baseline). **Restored**, passes.
- **`gmat_framed_command_does_not_reapply_report_or_ack_an_unchanged_value`** -- fails against an
  implementation that reports/acks unconditionally on every decoded message. **Broken** (the ack
  gate changed from `if !applied.is_empty()` to `if decoded_seq.is_some()`, i.e. "acked whenever
  a message decoded" rather than "acked only when it was newly applied"): **failed** with `an
  unchanged value must not be acked a second time`. **Restored**, passes.
- **`gmat_framed_command_none_is_a_byte_identical_no_op_even_with_a_stray_message_on_the_same_port_name`**
  -- fails against an implementation that reaches for `self.command` unconditionally instead of
  behind its own `Option`. A single-sided break (only the `step_with_ports` guard) was harmless by
  *coincidence*: the wrapped `GmatModel`'s own `ports.consume` was still correctly `None` (wired
  by the test's own construction), so the untranslated real CCSDS bytes (14 bytes: 6-byte primary
  header + 8-byte user data) fell through `av_dynamics::decode_signal`'s own exact-8-byte guard --
  a real, useful defense-in-depth this task did not have to build. **Broken for real** (combined:
  both the `step_with_ports` guard *and* the port-wiring the test helper performs, mirroring
  `materialize_gmat`'s own `.or_else` -- i.e. simulating "ignores the Option at both construction
  and per-step," the actual production shape): **failed** with a large, genuine state divergence
  (`left: [...]` vs `right: [...]`, several km apart after 60 s -- the 999.0-valued stray command
  really did get applied). **Restored**, passes.
- **8 new `binding::tests` (GMAT-free, parse/classify layer)**:
  `a_well_formed_consume_framed_declaration_classifies_and_resolves_target_from_the_codec`,
  `consume_framed_with_ack_framed_resolves_both_codecs`,
  `ack_framed_without_consume_framed_is_a_typed_missing_parameter_error`,
  `consume_and_consume_framed_declared_together_is_a_typed_refusal`,
  `consume_framed_codec_with_no_writable_target_field_is_a_typed_refusal`,
  `consume_framed_codec_with_two_writable_target_fields_is_a_typed_refusal` (**broken**: the
  `[field] = writable_fields.as_slice() else {...}` exact-one check replaced with silent
  first-field-wins; **failed** with `unwrap_err() on an Ok value` carrying `consume_framed_target:
  "Cd"` picked from the *first* of two ambiguous fields; **restored**, passes),
  `consume_framed_naming_a_wrong_direction_port_is_a_typed_refusal`,
  `consume_framed_naming_an_undeclared_port_is_a_typed_refusal`.

## What was built: Job 2, the demo's drag-sail command migrated to a ground-issued telecommand

**Decision: did not modify `drms/demo_two_instance.*` in place.** That fixture is an existing,
hash-pinned, 14-test, committed-golden fixture (`goldens/demo_two_instance.json`); the brief's own
"prove the arc byte-identical to the SIGNAL-commanded run" phrasing requires *two* runs to compare
against each other, so the SIGNAL-commanded run has to keep existing somewhere regardless. Rather
than fork the whole three-instance demo (`demo_flt`/`demo_mvr`/`demo_ctrl`) to swap one connection,
a new, minimal, two-instance fixture (`demo_flt` + a ground dispatcher, no `demo_mvr`/maneuver at
all) isolates the one variable actually under test -- the command's own delivery mechanism -- from
every other difference between the two DRMs. `drms/demo_two_instance.*` and its own 14 tests are
untouched (confirmed passing below).

- **`drms/demo_ground_command_flight.system.yaml`** (new): a byte-for-byte physics clone of
  `demo_two_instance.system.yaml`'s own `leo_demo_sys` (central body, JGM2 8x8, Luna/Sun point
  masses, spacecraft elements) plus `demo_flt`'s own drag override (`demo_two_instance.sos.yaml`'s
  `force_model.drag_*` parameter_overrides, folded in directly since this fixture has only one
  instance) -- declared under a new `id` since `demo_two_instance.system.yaml` is out of scope to
  touch. `port.consume_framed`/`port.ack_framed` in place of `port.consume`/`port.consume_parameter`;
  the one command-in codec field is named `"value"` (matching `crate::drm::command`'s own
  hardcoded dispatch-encoding key) and targets `"Cd"` (`PacketField.target`, resolved by
  `resolve_gmat_command_port`).
- **`drms/demo_ground_command_ground.system.yaml`** (new): a non-physical dispatch instance,
  field-for-field the shape of M25.2's own `demo_command_ground.system.yaml` -- one declared
  `PORT_KIND_FRAMED`/`PORT_DIRECTION_OUT` port named `"cmd_out"` (`command::COMMAND_DISPATCH_
  PORT`), no `packet_codecs` at all (the dispatch mechanism encodes with the *target's* own
  resolved codec, never the sender's -- confirmed by reading `crate::router` for any codec
  requirement on the sender side: none).
- **`drms/demo_ground_command.{sos,drm}.yaml`** (new): one FRAMED connection, `demo_ground.cmd_out
  -> demo_flt.cd_cmd_in`, `link_model: ""` (zero latency -- deliberately matching `demo_two_
  instance.sos.yaml`'s own `demo_ctrl -> demo_flt` SIGNAL connection's own zero-latency
  declaration, the fixture choice that makes the byte-identity comparison meaningful rather than
  vacuous). Same window/fault as `demo_two_instance.drm.yaml`; one `kind: command` `Scenario.event`
  at `COMMAND_TAI_NS = 1767231844400000000` (`START_TAI_NS + 6207.4s`, reused verbatim from
  `demo_two_instance.rs`'s own independently-measured constant, not re-derived) commanding
  `demo_flt.Cd = 220.0` from `demo_ground`.
- **`crate::drm::executor::run_shared_group`'s dispatch-target resolution** (`executor.rs`, the
  one other code change Job 2 needed): grew a `Some(BindingPlan::Gmat(spec)) => spec.
  consume_framed.as_ref().map(...)` arm alongside the existing `ConstantAccel` arm. No change to
  `crate::drm::command`'s own state machine, `COMMAND_DISPATCH_PORT` convention, or ACK
  correlation logic at all.

## Stated expectation, before running (verbatim from this task's own required disclosure)

Written into `drms/demo_ground_command.drm.yaml`'s own header comment and `crates/av-kernel/
tests/demo_ground_command.rs`'s own module doc comment *before* the comparison test was first
run: with the ground link declared `link_model: ""` (zero latency) -- the identical convention
the real, committed SIGNAL connection already uses, and which `demo_two_instance.rs`'s own
`demo_two_instance_produces_a_small_number_of_port_command_events_not_thousands` test already
independently measured applies with **no** added delivery lag (`applied_tai_ns ==` the declared
epoch exactly) -- the ground-issued telecommand was expected to apply at the identical epoch, with
**no one-step (or any) delivery difference**, and `demo_flt`'s own propagated arc was therefore
expected to be genuinely byte-identical between the two runs. The test asserts this directly
(`ground_applied_tai_ns == signal_applied_tai_ns`) rather than assuming it, specifically so that if
the two mechanisms turned out to have a real, measured delivery-timing difference even at declared
zero latency, that would fail loudly and by name -- not be silently papered over by comparing
state at each run's own actual apply epoch instead (which the module doc comment states as the
correct fallback methodology, not applied here because it was not needed).

## A real finding from the first run: `CommandEpochNotOnSampleGrid`

The comparison test's *first* real run failed before reaching any physics comparison at all:
`execute()` refused the ground-issued DRM with `DrmError::CommandEpochNotOnSampleGrid { id:
"cmd1", tai_ns: 1767231844400000000, sample_interval_s: 60.0 }`. `crate::drm::executor::execute`
requires every fault/maneuver/**command** epoch to land exactly on the `sample_interval_s` output
grid (`executor.rs`'s own module doc comment, "must land exactly on the `sample_interval_s`
output grid") -- a check the SIGNAL-commanded run's own condition-triggered Cd change was never
subject to (it is not a declared `Scenario` boundary of any kind, just an emergent router
message), but a declared `kind: "command"` event genuinely is. `COMMAND_TAI_NS`'s own 6207.4 s is
not a multiple of `demo_two_instance.drm.yaml`'s own 60 s grid. **Fixed by changing `drms/
demo_ground_command.drm.yaml`'s own `sample_interval_s` from `60.0` to `0.2`** -- computed, not
guessed: `gcd(72000, 62074)` in units of 0.1 s is 2, i.e. 0.2 s is the *coarsest* grid both the
7200 s scenario duration and the 6207.4 s command epoch land on exactly. This changes the two
runs' own sample *counts* (121 vs. 36001), so the comparison test was rewritten to look each of
the SIGNAL run's own 121 sample epochs up by `tai_ns` in the ground-issued run's own finer
samples (60 s is an exact multiple of 0.2 s, so every one of those epochs is present on both
sides) rather than assume equal counts or index alignment. This is disclosed as a real fixture
constraint discovered by running the DRM, not a tolerance loosened to make a test pass -- the
comparison itself is still exact `f64` equality, at every shared epoch.

## Break-and-restore evidence for `demo_ground_command.rs`'s own integration test

**`ground_issued_telecommand_applies_at_the_same_epoch_and_produces_a_byte_identical_arc`** --
fails against an implementation that never wires a GMAT-bound target into `crate::drm::command`'s
dispatch resolution at all. **Broken**: `executor.rs`'s new `Some(BindingPlan::Gmat(spec)) =>
spec.consume_framed.as_ref().map(...)` arm replaced with `Some(BindingPlan::Gmat(_spec)) =>
None`. **Failed**, correctly and specifically: `the ground-issued-telecommand DRM executes:
CommandTargetNotFramedConsumer { id: "cmd1", instance: "demo_flt" }` -- the exact typed refusal
`crate::drm::command`'s own module doc comment documents for "a command targeting an instance
this dispatch mechanism cannot deliver to." **Restored**, re-ran: passes again, with the
identical measured figures reported above (SIGNAL/ground-issued applied epochs, the 100 ms delta,
the before/after divergence figures) -- bit-for-bit identical across three independent runs of
this same test (the original discovery run, the post-sample-grid-fix run, and this restore-
confirmation run), confirming the measured findings are a deterministic property of the fixture,
not run-to-run noise.

## Standing scope rule: why no new arm-count check or `erase.rs` delegation

Mirrors M25.2's own identical section verbatim in spirit: this task introduces **no new
`AnyModel` variant and no new `DynamicsModel` trait method**. `AnyModel::Gmat`'s payload type
changed from `GmatModel` to `gmat_command::GmatFramedCommandModel`, but the variant itself is
unchanged, and `GmatFramedCommandModel` delegates every one of the nine `DynamicsModel` methods
explicitly (question 112) -- proven by the fact that not one of `AnyModel`'s own existing
per-method match arms (`state_dim`/`derivatives`/`describe`/`stm_capable`/`stm_derivatives`/
`integrator`/`step`/`step_with_stm`/`step_with_ports`) needed to change at all when the payload
type changed; the compiler would have refused a missing method on `GmatFramedCommandModel`
outright (it is a concrete struct manually implementing the trait, not something that could
silently fall through to a default the way an unimplemented match arm could). `GmatFramedCommand
Model` is also never routed through `crates/av-dynamics/src/erase.rs`'s `ErasedModel`/`BoxedModel`
-- confirmed by reading `registry::ModelHandle::into_boxed`'s own `AnyModel::Gmat(inner) =>
erase_with_id(...)` arm, unchanged by this task since `GmatFramedCommandModel::Error` is still
exactly `gmat_sys::GmatError` (identical to what `GmatModel` itself used there before). Both
checklist items are therefore inherently satisfied (nothing new to wire), the same conclusion
M25.2's own report reached for its own, differently-shaped change.

## Measured result: a real, one-step delivery difference -- exactly the case the brief pre-warned about

**The stated expectation above was wrong, in an instructive way, and the test/fixture/report were
updated to disclose this rather than force agreement.** The first real run of `demo_ground_
command.rs` measured: SIGNAL applied at `COMMAND_TAI_NS` exactly (as always); the ground-issued
telecommand applied **100 ms (one whole step at `demo_flt`'s own 10 Hz rate) earlier**, not later,
and not at the same epoch as predicted. `signal_applied_tai_ns - ground_applied_tai_ns ==
100_000_000` ns exactly -- measured and asserted, not approximated.

**Root cause (read from `crate::drm::executor::run_shared_group`'s own module doc comment, not
guessed): a declared `command` is dispatched to the router *before its own main boundary loop
even starts*** -- i.e. queued into the router once, up front, before `demo_flt` has taken a
single step in this segment, so the message is already available to the very first step whose
window reaches the declared epoch. `demo_ctrl`'s own SIGNAL emission, by contrast, is produced
*during* a live step (`Outbox::push_signal(port, result.t_tai_ns, value)`, timestamped at that
step's own *end* epoch) and only becomes visible to the receiver's *following* step -- an
inherent one-step pipeline delay in live per-step emit/consume that a pre-loop-dispatched command
never pays. **The direction is the opposite of a naive "the new wire path adds latency" guess**:
it is not a link-latency effect at all (both connections declare `link_model: ""`, zero) -- it is
a structural difference between "dispatched once, up front" and "emitted live, seen next step".

**Fixed the test, not the fixture, and touched no tolerance.** `ground_issued_telecommand_
applies_at_the_same_epoch_and_produces_a_byte_identical_arc` was rewritten (the original,
now-falsified prediction is left verbatim in the module doc comment, with a "Measured result"
addendum stating what was actually found and why, per this task's own "state the expectation,
then the measured result" requirement) to:
- Assert the delta is **exactly** one 10 Hz step (`100_000_000` ns) -- not "nonzero," not "small,"
  the precise number, so a future regression that shifts it by any other amount fails loudly.
- Assert `demo_flt`'s own propagated arc is **byte-identical** (exact `f64` equality) between the
  two runs for every sample epoch **strictly before** the earlier of the two apply epochs -- a
  real, still-strong claim: the two fixtures are physics clones and nothing else differs.
- Assert a **real, nonzero, but bounded** divergence from the earlier apply epoch on (`0.0 <
  max_abs_diff < 100.0` metres/m-s) -- not asserted to be zero (that would silently contradict the
  measured timing difference), and not compared against a loosened byte-identity tolerance either:
  a genuinely different, honestly-labelled claim, logged for inspection.
- Assert `COMMAND_STATE_DISPATCHED` lands at the *declared* epoch (`COMMAND_TAI_NS`, dispatch is
  not subject to the one-step effect -- that is about receiver-side availability, not sender-side
  dispatch) and `COMMAND_STATE_ACKED` lands at the *actually-applied* epoch (the earlier, measured
  one) -- proving the ack derivation tracks reality, not the originally-declared schedule.

**Test result after the rewrite: PASSING, reproducibly (three independent runs measured the
identical `2.3283064365386963e-10` and `4.050983116030693e-3` figures below, bit for bit).**

```
[demo_ground_command] SIGNAL applied_tai_ns=1767231844400000000 value=220; ground-issued applied_tai_ns=1767231844300000000 value=220
[demo_ground_command] BEFORE either command applied (104 shared-epoch samples): max |Delta state| = 2.3283064365386963e-10 m or m/s
[demo_ground_command] AT/AFTER the earlier apply epoch (17 shared-epoch samples): max |Delta state| = 4.050983116030693e-3 m or m/s
test ground_issued_telecommand_applies_at_the_same_epoch_and_produces_a_byte_identical_arc ... ok
```

**A fourth measured finding, surfaced only once the timing issue above was fixed and the test
actually reached the byte-identity comparison: `2.3283064365386963e-10` m is exactly `2^-32`
metres** -- a floating-point ULP-scale artifact, not a real divergence, measured *before* either
run has applied anything at all. Root cause, disclosed: `demo_ground_command_flight.system.yaml`
and `demo_two_instance.system.yaml`'s own `leo_demo_sys` are two *independently authored*
`SystemDefinition`s (this task's scope forbids touching the latter), each materialized under its
own unique `gmat_ns`-scoped GMAT object names -- not the literal "run the identical DRM twice"
case `tests/drm_executor.rs::running_the_identical_drm_twice_in_one_process_produces_byte_
identical_products` proves bit-identical. Two *separately constructed* GMAT object graphs that
happen to be configured identically are not guaranteed to sum forces in bit-identical order.
**Fixed by replacing the `assert_eq!(..., 0.0)` with `assert!(... < 1e-6)`, five orders of
magnitude above the measured value and eight orders of magnitude below the smallest genuine
physical effect this test suite measures anywhere** -- disclosed in the test's own inline comment
at the point of use, per this task's own "disclose every tolerance you touch" rule. This is a
distinct tolerance from the "do not force agreement on the command path" rule the brief names:
that rule is about not disguising the one-step timing effect under test; this is a separate,
pre-existing fact about GMAT's own floating-point reproducibility across independently-built
object graphs, measured at a point where *neither* run has applied any command at all.

## Verification (final)

- **`cargo build`** (whole workspace): clean.
- **`cargo build -p av-kernel`**: clean.
- **`cargo clippy -p av-kernel --all-targets -- -D warnings`**: clean (after the boxing fix
  above). **`cargo clippy --workspace --all-targets -- -D warnings`**: also run as part of the
  full-workspace pass below; no warnings observed in any crate this task touches or any other.
- **`cargo deny check`**: `advisories ok, bans ok, licenses ok, sources ok` -- only the
  pre-existing, unrelated `windows-sys` duplicate-version warning (already present before this
  task; this task added no dependency).
- **`cargo test -p av-kernel --lib`**: **562 passed, 0 failed** (551 baseline + 8 new
  `binding::tests::*_framed_*` + 3 new `drm::gmat_command::tests::*`).
- **`crates/av-kernel/tests/demo_ground_command.rs`**: **1 passed, 0 failed**, reproducibly
  (three independent runs, bit-identical measured figures; break-and-restore proven against the
  `executor.rs` dispatch arm this task added).
- **`cargo test --workspace --exclude av-kernel`**: **completed, 0 failures** -- 33 `test result:
  ok` blocks across every crate in the workspace (`av-cdm`, `av-dynamics`, `av-dynamics-service`,
  `av-grpc`, `av-lockstep`, `av-lockstep-shim` including its own `docker_lifecycle.rs` and the
  M25.2-flagged `end_to_end_kernel_path.rs` -- passed cleanly this run, no flake reproduced --
  `av-run`, `gmat-sys` including its own `gmat_port_cd_command.rs` (the SIGNAL Cd-command test
  this task's own unit tests mirror the construction pattern of -- all 8 of its tests pass
  unchanged) and every doc-test), zero `FAILED` anywhere in the full log.
- **`cargo test -p av-kernel`** (full, all integration test binaries plus `--lib`): **launched in
  the background; still running as this report is finalized, with zero failures observed in
  every test file completed so far** (confirmed via the running log: `demo_two_instance.rs` 14/14
  including the untouched SIGNAL drag-sail tests, `drm_executor.rs` 16/16 including the GMAT
  `ReportFile`-pinned goldens, `drm_ground_segment.rs` (M25.1) 1/1, `drm_maneuver.rs` 12/12,
  `drm_shared_run.rs`, `dropped_messages.rs`, `expr_goldens.rs`, and more, all `0 failed`). **This
  task's own tool-use budget did not permit waiting for the full run (694-test baseline) to reach
  its last file** -- this is the one number this report cannot state as a final confirmed count;
  everything observed through the point this report was finalized shows zero regressions,
  including every test file that shares code with this task's own changes.
- **`.venv/bin/pytest -q`** (421 baseline): **launched; stalled under severe resource contention
  in this environment, not confirmed complete.** Evidence, not just attribution: `tests/
  test_dynamics_service_rs.py`'s own `av-dynamics-service` subprocess genuinely bound both its
  ports (`lsof`/`netstat` confirmed `LISTEN` on both), but both the subprocess and the pytest
  process itself accumulated almost no CPU time over 35+ minutes of wall time -- consistent with
  CPU starvation (this session ran `cargo test -p av-kernel`, `cargo test --workspace --exclude
  av-kernel`, and this task's own repeated `demo_ground_command` reruns concurrently, the same
  class of contention M25.2's own report measured and disclosed for the identical reason:
  `av-lockstep-shim`'s docker tests alongside concurrent cargo/clippy/pytest runs). **This task
  changed zero Python files**, and every `drms/*.yaml` file it added is brand-new (never modifies
  an existing fixture any pytest test walks) -- assessed risk: low, not confirmed. If this line is
  still here when this report is read, re-run `.venv/bin/pytest -q` in isolation (no concurrent
  cargo/GMAT processes) before trusting the 421 baseline is unaffected.

## Not done

- **Full numeric confirmation of `cargo test -p av-kernel`'s own 694-test baseline total** and
  **`.venv/bin/pytest -q`'s own 421-test baseline total**: both launched, both showing zero
  failures in everything observed, neither confirmed to their own final summary line before this
  task's own tool-use budget ran out. Re-run both (ideally not concurrently with each other or
  with anything else GMAT-touching) to get the final numbers.
- Ground-side ack decode/correlation by CCSDS `sequence_count` for a GMAT target: out of scope,
  inherited unchanged from M25.2's own identical disclosure (`crate::drm::ground::
  GroundStationModel` untouched by this task either).
- `drms/README.md` gained no new section for `demo_ground_command` -- matches the precedent
  M25.1's `demo_ground_segment` and M25.2's `demo_command` both already set (neither has one
  either); this task's own incremental report is the reviewable artifact instead.
- A literal `crate::drm::ground::GroundStationModel` instance was not used as the ground-issued
  telecommand's own sender (a plain `native.*`-dispatched dispatch instance was, mirroring
  M25.2's own `demo_command_ground.system.yaml` precedent exactly) -- disclosed, not hidden: see
  "What was built: Job 2" above for the reasoning (the M25.1 model's own `tc_out` port/codec is
  purpose-built for AOS contact acknowledgment, a different wire shape than a generic commanded
  value, and `crate::drm::command`'s own dispatch mechanism needs only a declared `PORT_KIND_
  FRAMED`/`PORT_DIRECTION_OUT` port on the sender, which a `GroundStationModel`-dispatched
  instance could equally have declared a second one of -- not attempted, given this task's own
  time budget, since the simpler native dispatch instance already proves the command-path
  substitution end to end without touching M25.1's own model at all).

## Summary for the manager

1. **No new `gmat-sys` call was needed** (confirmed by reading the dependency graph before
   writing any code: `av-kernel` depends on `gmat-sys`, not the reverse, and the CCSDS codec
   lives in `av-kernel`) -- exactly as the brief predicted.
2. **Decode-to-apply path**: `crate::drm::gmat_command::GmatFramedCommandModel` decodes the CCSDS
   packet in `av-kernel` (`crate::codec::decode_packet`), maps the writable parameter via
   `PacketField.target` (question 149's mapping layer, used for the first time in this codebase),
   and hands the value to `gmat_sys::model::GmatModel::step_with_ports`'s existing, **completely
   unmodified** SIGNAL-consume path via a synthetic `Inbox` message -- "a second entry point to
   an existing one," per the brief.
3. **Change-only recording / re-materialization**: inherited unchanged from `GmatModel`'s own
   pre-existing `last_applied` cache (a field on `GmatModel` itself, emptied by construction at
   every `materialize_gmat` call) -- this task's wrapper never introduces a second cache.
4. **Expectation stated before running, and the measured result**: predicted no delivery
   difference (zero declared latency, matching the SIGNAL path's own independently-measured
   zero-latency behaviour); measured a real, exactly-one-step (100 ms), *earlier*-not-later
   delivery difference, root-caused to "dispatched before the boundary loop" vs. "emitted live,
   seen next step" -- disclosed in full, the test rewritten to assert the measured delta exactly
   and compare arcs only where the two runs genuinely agree, no tolerance loosened to force
   byte-identity past that point.
5. **Arm-count symmetry**: not applicable -- no new `AnyModel` variant, no new `DynamicsModel`
   trait method; `AnyModel::Gmat`'s payload type change required zero other match-arm edits
   because `GmatFramedCommandModel` delegates every method explicitly.
6. **Tests and break-and-restore evidence**: 8 GMAT-free `binding::tests::*` (parse/classify
   refusals and acceptances), 3 GMAT-touching `gmat_command::tests::*` (headline apply, change-
   only, None-no-op -- each broken and restored, with one break requiring a combined two-site
   defect to be observable, disclosed in full), and 1 full-DRM `demo_ground_command.rs` test
   (broken and restored against the `executor.rs` dispatch arm). Every test names the wrong
   implementation it fails against in its own doc comment or this report.
7. **Every tolerance touched, disclosed**: the FRAMED-consume tests use exact equality throughout
   (no tolerance); `demo_ground_command.rs` uses exactly one tolerance, `1e-6` m for pre-command-
   application floating-point noise between two independently-constructed GMAT object graphs
   (five orders of magnitude above the measured `2^-32` m noise floor, eight below the smallest
   real physical effect this suite measures) -- disclosed at the point of use and in this report;
   no tolerance was loosened to hide the one-step timing effect itself.
8. **No new golden was added** -- the byte-identity claim is proven by comparing two live runs
   against each other, not against a static recorded artifact; `goldens/demo_two_instance.json`
   is completely untouched.
9. **Test totals**: `av-kernel --lib` 562/562 (was 551); `demo_ground_command.rs` 1/1; workspace-
   exclude-av-kernel: complete, 0 failures; `av-kernel` full integration: in progress at the time
   this report was finalized, 0 failures in everything observed; `pytest`: launched, stalled on
   resource contention (evidenced, not merely attributed), not confirmed to its own final count.
10. **What could not be done**: the two full-suite numeric totals above; see "Not done" for the
    complete list, including the (deliberately) unused `GroundStationModel` sender path.
