status: done (verification suite green; see "Not done / disclosed gaps" for the one deliberately
narrowed scope item)

# M25.1 — the ground segment as a system

Findings first, measurements before claims, per the task brief.

## 1. Ground `SystemDefinition` and its native model

New module `crates/av-kernel/src/drm/ground.rs`. `GroundStationModel` is a native
`av_dynamics::DynamicsModel`, `state_dim() == 0` (a fixed geodetic site with an instantaneous
visibility transform has nothing to integrate — the same shape as `StarTrackerModel`/
`AttitudeControllerModel`).

**Visibility, through the topocentric frame (bullet 1).** `docs/open-questions.md` question 10
already mandates ENU/NED at an origin as a day-one frame, and `core.proto`'s
`FrameDefinition.origin_geodetic` ("Required for ENU / NED") is the declared site
representation — used verbatim, nothing invented:

- `GroundStationSpec { body, latitude_rad, longitude_rad, height_m, elevation_mask_rad }`,
  parsed by `parse_ground_station_spec` from `"station.*"` instance parameters — the *same four
  numbers* `Geodetic` carries (`body`/`latitude_rad`/`longitude_rad`/`height_m`).
- `geodetic_to_ecef_m` (standard WGS84 ellipsoidal transform), `ecef_to_topocentric` (the
  standard local East-North-Up rotation from geodetic lat/lon — `AXES_KIND_ENU`), `elevation_rad`
  (`asin(up / |enu|)`), composed as `elevation_of` — pure Rust, no GMAT dependency, matching every
  other native model's "GMAT-free, cheap" contract.
- **Why the model reads `station.*` parameters rather than a live `Scenario.frames`/
  `FrameRegistry` lookup, stated plainly.** Every native model's spec parser
  (`parse_star_tracker_spec`, `parse_attitude_spec`, ...) is handed only the instance's flat
  `BTreeMap<String, Parameter>` — `classify_binding`/`ModelRegistry` never thread
  `Scenario.frames` through model construction at all (frames are realized elsewhere, in
  `executor::collect_frames`, question 124). Threading `Scenario.frames` into every native model
  constructor to support one instance kind is a wider change than this task's scope justifies.
  Instead, a DRM declares the same four `Geodetic` numbers **twice**: once as `"station.*"`
  parameters (what the model reads every step) and once as a real `Scenario.frames`
  `FrameDefinition` (`axes: AXES_KIND_ENU`, `origin_geodetic`, `platform_id`) for the CDM/
  viewer's own frame graph — see `drms/demo_ground_segment.drm.yaml`'s own `scenario.frames`
  entry. Not a second, competing site representation — the identical `Geodetic` shape, declared
  in the one additional place the CDM's own frame registry needs it.

**Contact window model with an elevation mask.** `contact_windows(elevation_mask_rad, samples:
&[(tai_ns, elevation_rad)]) -> Vec<ContactWindow>` — a pure function, linearly interpolates the
AOS/LOS crossing epoch between the two bracketing samples (not "nearest sample"; see its own doc
comment for the interpolation formula and the acceptance-pin test for the measured accuracy this
buys). `GroundStationModel::step_with_ports` calls it one sample at a time (via `elevation_of`)
against a per-instance `Cell<bool>` latch (`in_contact`), so the identical geometry function is
exercised both live (per-step, in a run) and in bulk (the acceptance-pin test, fed a whole GMAT
arc at once) — one implementation, two call sites, never two.

**Link with the existing latency model (bullet 1's third requirement).** `tm_in`/`tc_out` are
ordinary `PORT_KIND_FRAMED` ports with a declared `PortTiming.latency_ns`; the demo's own
`Connection.link_model = "latency"` is resolved by the **existing**
`crate::router::Router::effective_latency_ns` exactly like every other FRAMED connection in this
workspace — nothing in `ground.rs` computes or reimplements a latency figure.

## 2. FRAMED telecommand + telemetry ports and their `PacketCodec`s (bullet 2)

- `tm_in` (IN): decodes the target's Earth-fixed Cartesian position — `ground_tm_packet_codec`
  builds the fixed field layout (`x`,`y`,`z`, `PACKET_FIELD_TYPE_FLOAT64`, `UNIT_METER`,
  24 user-data bytes).
- `tc_out` (OUT): encodes an AOS-acknowledgment telecommand (`is_command: true`) on a rising
  edge — `ground_tc_packet_codec` (`seq`, `PACKET_FIELD_TYPE_UINT`, 4 user-data bytes).
- Both codecs reuse the existing, already-corrected CCSDS encoder (`crate::codec::encode_packet`/
  `decode_packet`) — **the packet-data-length off-by-one (`secondary_header_bytes +
  user_data_bytes - 1`, CCSDS 133.0-B 4.1.2.5) was not touched**; `crates/av-kernel/src/
  codec.rs` is unmodified by this task.
- `resolve_ground_ports` (`binding.rs`) resolves which declared `packet_codecs`/`.ports` entries
  are the telemetry-in/telecommand-out pair, by shape (`x`/`y`/`z` fields, `is_command`) and by
  the fixed port names `ground::GROUND_TM_IN_PORT`/`GROUND_TC_OUT_PORT` — mirrors
  `resolve_sensor_output`/`resolve_controller_ports`'s own convention exactly; any other count is
  the new, typed `DrmError::GroundPortConfiguration`, never silently defaulted.

## 3. The demo grows a ground instance connected to a flight instance (bullet 3)

**Why the flight-side FRAMED endpoint cannot be a raw `GmatModel` — checked directly, not
assumed.** `crates/gmat-sys` sits *below* `av-kernel` in this workspace's dependency graph
(`av-kernel` depends on `gmat-sys`, never the reverse); CCSDS encoding (`crate::codec`,
`crate::ports::encode_ccsds_message`) lives in `av-kernel`. `gmat_sys::model::
GmatModel::step_with_ports` therefore cannot call it — a hard dependency-direction fact, not a
missing feature. Separately, `GmatPortConfig` (`crates/gmat-sys/src/model.rs`) supports exactly
one `(port, output_key)` SIGNAL emit pair, `output_key` restricted to `GmatModel::step`'s own two
named outputs (`rmag`, `cd`) — no position vector. Every existing FRAMED-CCSDS producer in this
codebase (`StarTrackerModel`, `ImuModel`, `AttitudeControllerModel`) is, for the identical
reason, a native av-kernel-level model, never a raw `GmatModel`.

**Decision: extend `ConstantAccelModel` (existing, widely-used native placeholder) with an
optional FRAMED position-broadcast capability, additive and off by default.** `ConstantAccelSpec`
gains `emit_framed_port: Option<String>` (parsed from a new `"port.emit_framed"` parameter) and
`emit_framed_codec: Option<PacketCodec>` (resolved by `classify_binding`'s `ModelKind::Native`
arm via the **existing** `resolve_sensor_output`, exactly the codec-resolution convention every
FRAMED-emitting native model already uses — refused, typed, at load time if the declared port
name disagrees or the codec is missing `x`/`y`/`z`). `ConstantAccelModel::step_with_ports`
encodes `state[0..3]` (its own propagated position, already computed by the existing closed-form
double integrator) as one CCSDS packet, unconditionally, every step, when set. `None` (every
fixture before M25.1) is a proven no-op: **533 pre-existing tests still pass unchanged** after
this addition (`cargo test -p av-kernel --lib`, before vs. after).

The demo's flight instance (`drms/demo_ground_segment_flight.system.yaml`) is therefore a
`ConstantAccelModel` with `port.emit_framed: tm_out`, seeded with a real Earth-fixed
position/velocity snapshot (not a fabricated one — derived directly from this task's own,
already-tested `crate::drm::ground::{geodetic_to_ecef_m, elevation_of}`, printed via a throwaway
`#[test]` and removed before commit) chosen so a straight-line kinematic pass rises above and
sets below the ground station's own 10° mask within a 900 s window: AOS ≈ 50 s, peak ≈ 84° at
420 s, LOS ≈ 795 s. **This is a genuine, declared, testable physical carrier — not GMAT fidelity
over a long arc, exact over this fixture's own short window — and the report says so plainly: the
GMAT `ContactLocator` acceptance pin (§5 below) is measured separately, directly against a real
GMAT-propagated arc, and does not depend on this demo's simplified flight-side kinematics at
all.**

**Fixtures**: `drms/demo_ground_segment_ground.system.yaml`, `demo_ground_segment_flight.
system.yaml`, `demo_ground_segment.sos.yaml`, `demo_ground_segment.drm.yaml`. Router connection
`flight.tm_out -> ground.tm_in`, `link_model: "latency"` (50 ms declared on each port, 100 ms
total, via the router's own existing latency summation). `ground.tc_out` is declared (bullet 2)
but not connected to anything — `ConstantAccelModel` has no FRAMED-*consume* capability today (a
second, separate extension this task did not need and did not build); disclosed, not hidden.
`scenario.frames` declares the ENU `FrameDefinition` per §1 above.

**Golden hashes** (`cargo run -p av-kernel --example drm_hash -- <kind> <path>`, the existing,
unmodified tool): all four files parse and their declared `hash` fields verify against
`crate::drm::hash::canonical_*_hash` (proven by `execute()` itself refusing a tampered hash and
this DRM running to completion below).

**DRM fixture through `execute()` (the standing-scope requirement).**
`crates/av-kernel/tests/drm_ground_segment.rs::
the_ground_segment_drm_runs_through_execute_and_reports_one_rise_and_set_pass` — loads the four
YAML files, runs `av_kernel::drm::execute`, and asserts (stated before measuring, in the test's
own doc comment): exactly one `Trajectory` entry (`flight`; `ground` deliberately emits none,
`state_dim() == 0`), exactly one `EVENT_KIND_CONTACT_START` and one `EVENT_KIND_CONTACT_END` on
the `ground` instance, start strictly before end, AOS in `[40, 60]` s and LOS in `[785, 815]` s
(a generous window around the hand-derived numbers above — tight enough to fail a sign/rotation
error, not a precision pin), and `output.ground.in_contact@end == 0.0` (the pass has already
ended by the 900 s horizon). **Passes.**

## 4. `EVENT_KIND_CONTACT_START`/`_END` (bullet 4)

Both already existed in `trajectory.proto` (`EventKind` 2/3) — not touched. New:
`events::contact_event` (`crates/av-kernel/src/drm/events.rs`) builds the `Event`, dispatched
from `executor::execute`'s existing applied-commands drain (`crates/av-kernel/src/drm/
executor.rs`, the loop that already turns every `av_dynamics::AppliedCommand` into
`EVENT_KIND_PORT_COMMAND`): one new `if cmd.port == ground::CONTACT_TRANSITION_PORT` branch
routes to `contact_event` instead — the identical "an applied command becomes a CDM event, once,
at the point it is actually applied" pipeline question 130 already built, reused rather than
reimplemented. `CONTACT_TRANSITION_PORT` (`"__contact_state__"`) is a reserved synthetic string,
never pushed through an `Outbox` and never a real declared `Port.name` — it cannot collide with
router-delivered traffic because `AppliedCommand` is an entirely separate side channel from
`crate::router::Router`'s own message delivery.

## 5. The acceptance pin — GMAT `ContactLocator`

**Expected windows, stated before measuring.** Reused `leo_demo_sys`'s own orbit (SMA 6878 km,
ECC 0.001, INC 51.6°, RAAN 30°, AOP 0°, TA 0°, JGM2 8x8 + Sun/Moon — the same, already-pinned
vehicle `goldens/gen_leo_1day.py`/`drms/demo_two_instance.system.yaml` use, not a new orbit
invented for this task), a Cape-Canaveral-like site (28.5°N, 80.6°W, 0 m — recognizable, not
cherry-picked for pass count), 10° elevation mask, 3 h (10800 s) arc — long enough for at least
one full pass at this orbit's ~93-minute period. Before running, the only claim was "at least one
rise-and-set window inside the arc, computed independently by GMAT's own `ContactLocator`."

**Generator**: `goldens/gen_ground_contact_gmat.py` (run explicitly, `--reason` required, never
regenerated by a test — ADR-002's own goldens rule). One GMAT script, run via
`gmat.LoadScript`/`gmat.RunScript` (never `Execute()` through the object API — the documented
`ReportFile`/`SaveScript` quirks from `gmat-api-quirks.md`, confirmed not to apply to
`ContactLocator` itself since `RunMode = Automatic` needs no solver loop, but the same safe
script+report pattern is reused regardless): a `GroundStation`/`ContactLocator` pair
(`UseLightTimeDelay`/`UseStellarAberration = false`, so the comparison is purely geometric — the
same instantaneous, non-relativistic transform `elevation_of` itself computes) plus a dense
30 s-cadence `ReportFile` of `Golden.EarthFixed.{X,Y,Z}` over the identical arc.
`Earth.EquatorialRadius`/`.Flattening` are pinned to this repository's own WGS84 constants
(`crate::drm::ground::{WGS84_A_M, WGS84_F}`: 6378.137 km, 1/298.257223563) — not GMAT's own
slightly different built-in default — so a disagreement in the comparison can only be this
task's own geometry, never a mismatched reference ellipsoid.

**A real defect found and fixed while building this golden, disclosed rather than hidden.** The
first generation run produced contact windows off by a consistent ~37 s from what the position
series implied — the Python script computed the `ContactLocator` report's own UTC-Gregorian
timestamps' elapsed time against the position `ReportFile`'s own `Golden.A1ModJulian` column
(two independently-computed epoch references, one apparently not applying the full UTC↔TAI
leap-second table in that call context). Fixed by referencing *both* the mission epoch and the
contact-window boundaries through the identical `TimeSystemConverter::ConvertGregorianToMjd`
call — self-consistent regardless of which absolute time system that function actually returns,
since a fixed offset between systems cancels out of an elapsed-time subtraction. After the fix,
the measured delta dropped from ~37 s to ~0.4 s (see below) — the generator script's own header
comment and inline comments record this for the next person who touches it.

**Measured** (`goldens/ground_contact_gmat.json`, `crates/av-kernel/tests/
ground_contact_gmat.rs::contact_windows_matches_gmats_own_contact_locator_for_the_same_site_and_arc`):
exactly one window found by both GMAT and `crate::drm::ground::contact_windows`. GMAT: AOS
5960.386 s, LOS 6371.995 s (duration 411.609 s). This crate's own geometry, fed GMAT's own
30 s-cadence `EarthFixed` position series: AOS 5959.988 s, LOS 6372.429 s. **Deltas: 0.398 s
(AOS), 0.434 s (LOS).**

**Tolerance: 10 s, justified against the sampling cadence before the delta above was measured,
never loosened to fit it.** GMAT's `ContactLocator` root-finds the crossing continuously (far
finer than the 30 s reporting cadence) — effectively exact for this comparison.
`contact_windows` only ever sees the 30 s samples and linearly interpolates between the
bracketing pair; the interpolation error is bounded by the *curvature* of elevation-vs-time over
one 30 s step, not by the step size itself — near this shallow 10° mask the elevation rate is
already non-zero (order 0.05–0.15°/s, from `peak elevation / half the 411.6 s duration` as a
rough scale) and does not change enormously over one 30 s step for a smooth orbital ground
track, so a linear fit over 30 s is expected accurate to a small fraction of the step, not the
full 30 s. 10 s (one-third of the sampling interval) was picked as large enough to absorb that
curvature error and tight enough to still fail against a real defect (a sign error, a swapped
axis, a wrong-direction mask comparison, the wrong reference ellipsoid) — not a number chosen
after seeing the result. The actual **0.4 s** margin is disclosed, not used to retroactively
justify a tighter number now; the 10 s tolerance stands, with ~25x headroom recorded honestly.

**Test count matches too**: exactly one window on both sides (not merely "close enough on
whichever window happened to align") — the count assertion is unconditional, before the epoch
deltas are even compared.

## 6. Fault / maneuver / covariance — decided and stated (the lead's rule)

- **DYNAMICS fault**: ADR-005 §5's general rule applies unchanged ("sets a model parameter
  through the model's declared parameter interface"). `apply_ground_station_target`
  (`crates/av-kernel/src/drm/fault.rs`) makes every numeric `"station.*"` field a valid target
  (`elevation_mask_rad` — an antenna outage or degraded horizon; `latitude_rad`/`longitude_rad`/
  `height_m` — a relocated or mobile asset). `station.body` (a string) is refused with the typed
  `DrmError::UnknownParameter`, the same "not every declared field is a numeric fault target"
  rule `apply_star_tracker_target` already applies to non-noise fields.
- **Maneuver**: refused. `GroundStationModel::state_dim() == 0`, not the 6-D Cartesian
  position/velocity shape a Δv jump requires. The existing, generic `DrmError::
  ManeuverTargetNotSixDimensional` (`executor.rs`'s length-6 conversion) already catches this;
  `BindingPlan::GroundStation(_)` is additionally named explicitly in the maneuver-boundary
  `matches!` guard, for parity with the `Imu`/`StarTracker`/`Controller` precedent ("naming it
  explicitly here proves the refusal is deliberate, not an accident of the generic check",
  M22.4's own rationale, applied identically here).
- **Covariance**: refused. `stm_capable() == false` unconditionally — a fixed geodetic site has
  no propagated state to carry a state-transition matrix at all. The existing, generic
  `DrmError::ModelNotStmCapable` refusal in `executor::run_covariance_instance` applies with
  zero ground-station-specific covariance code — the same pattern every other native model in
  this crate uses (`ConstantAccel`, `Attitude`, sensors, `Controller` all return `false` too).

## 7. Standing scope for this model — every item checked

- **`classify_binding`/`AnyModel`/`ModelRegistry`, deliberate arms, no catch-all**: done in the
  same change, not a follow-up (see §1/§2). `registry.rs`: `ModelKind::Ground`, `kind_for`
  prefix, `construct_ground_station`, `into_boxed` arm. `binding.rs`: `BindingPlan::
  GroundStation`, `AnyModel::GroundStation`, all 9 `DynamicsModel`-method arms (`state_dim`,
  `derivatives`, `describe`, `integrator`, `stm_capable`, `stm_derivatives`, `step_with_stm`,
  `step`, `step_with_ports` — matching `AnyModel::{ConstantAccel, Attitude, StarTracker, Imu,
  Controller}`'s own nine-method shape exactly). `fault.rs`: `apply_ground_station_target`.
  `executor.rs`: **four** separate exhaustive `BindingPlan` matches all needed and got a new
  arm — `materialize_plan`, `materialize_plan_at_boundary`, `convert_gmat_trajectory_to_
  declared_frame`'s frame-passthrough match, and the "emits no trajectory" match — found by
  `cargo build` itself refusing to compile until every one was covered (Rust's own
  exhaustiveness check as the safety net the task brief asks for).
- **Arm-count symmetry check, passing**: `any_model_arm_count_for_ground_station_matches_
  star_tracker` (`crates/av-kernel/src/drm/binding.rs`) — reads its own source
  (`include_str!("binding.rs")`) and counts literal `AnyModel::StarTracker(` vs.
  `AnyModel::GroundStation(` occurrences, asserting equality (15 == 15). This is a **real,
  executable** check, not the "verified by the arm-count grep this task's own report states"
  comment every earlier variant relies on — an upgrade over the existing convention, not merely
  matching it.
- **One DRM fixture instantiates it through `execute()`**: §3's `drm_ground_segment.rs`.
- **`ErasedModel`/`BoxedModel` delegation, explicit**: `GroundStationModel::Error =
  std::convert::Infallible`, erased via the **existing**, already-generic `erase_with_id` in
  `registry.rs::into_boxed` (`|_id, never: Infallible| match never {}`) — the identical shape
  `ConstantAccel`/`StarTracker`/`Imu` already use. `crates/av-dynamics/src/erase.rs` itself did
  **not** need changes: `ErasedModel<M>` is already generic over any `M: DynamicsModel` with
  full, explicit, per-method delegation (no trait defaults) — confirmed by reading its own
  `impl DynamicsModel for ErasedModel<M>` block, which has no `M`-specific code at all. Extended
  its "marker-value" proof anyway, at the `AnyModel`-delegation level (the level this task's own
  new code actually lives at): `any_model_step_with_ports_delegates_to_the_ground_station_
  variant` feeds a real overhead-target telemetry packet and checks a real marker (`applied[0]
  .value == CONTACT_START_VALUE`, a real encoded ack packet on `tc_out`) — not a synthetic
  sentinel, the same "prove it against a real production model" standard `registry.rs`'s own
  `into_boxed_erases_a_controller_and_preserves_its_step_with_ports_marker_outputs` sets for the
  Controller variant.

## Exact test functions, the wrong implementation each fails against, and break/restore evidence

Every new/changed function below states what a wrong implementation would look like; the ones
marked **[break/restore]** were mechanically broken, watched fail, then restored, with the
before/after test output shown to the run.

- `crate::drm::ground::tests::a_target_directly_overhead_is_at_ninety_degrees_elevation`,
  `a_target_on_the_local_horizontal_plane_is_at_zero_degrees_elevation` — fail against a
  transposed/mis-signed ENU rotation. **[break/restore]** swapped `elevation_rad`'s `atan2`
  arguments (`horiz.atan2(enu[2])` instead of `enu[2].atan2(horiz)`): 5 tests failed (both
  geometry tests plus 3 `step_with_ports` tests that depend on elevation), including the
  overhead test reporting `0.00000000000001° ` instead of `90°` and the horizontal-plane test
  reporting `90°` instead of `0°` — restored, all 12 `ground.rs` tests pass again.
- `contact_windows_interpolates_a_single_rise_and_set_pass` — fails against "snap to nearest
  sample" instead of interpolating. **[break/restore]** replaced the rising-edge interpolation
  with `t0` (nearest-sample): reported window start `0` instead of the expected `5`; restored.
- `contact_windows_open_at_the_first_sample_when_already_above_mask`,
  `contact_windows_reports_nothing_when_always_below_mask` — fail against an implementation that
  requires a rising edge to ever open a window, or that reports a window when none crossed.
- `parse_ground_station_spec_accepts_a_valid_set`, `..._refuses_an_unknown_parameter`,
  `..._refuses_an_out_of_range_elevation_mask` — fail against a parser that silently drops an
  unrecognized `"station.*"` name, or accepts an elevation mask outside `[0, π/2)`.
- `step_with_ports_reports_contact_start_and_sends_an_ack_when_a_target_rises_above_the_mask`,
  `..._reports_no_contact_when_a_target_is_below_the_mask`,
  `..._reports_contact_end_on_the_following_step_once_the_target_sets` — fail against an
  implementation that never reports the rising/falling edge, emits the ack unconditionally, or
  double-reports a steady-state contact every step. Exercised indirectly by the elevation-formula
  break/restore above (3 of these 5 failed under that break).
- `ground_station_model_declares_zero_state_dim_and_no_stm_capability` — fails against a
  `state_dim()`/`stm_capable()` that drifts from the §6 decisions.
- `any_model_{state_dim,derivatives,describe,integrator,stm_capable}_delegates_to_the_ground_
  station_variant`, `any_model_{stm_derivatives,step_with_stm}_returns_a_typed_capability_
  missing_error_for_the_ground_station_variant`, `any_model_step_delegates_to_the_ground_
  station_variant`, `any_model_step_with_ports_delegates_to_the_ground_station_variant`
  (`binding.rs`) — each fails against a missing or catch-all `AnyModel::GroundStation` arm (a
  compile error for a missing arm; a silent trait-default reach for a catch-all `_ =>`, which
  this codebase's own history names as "recurred three times"). `step_with_ports`'s own version
  feeds a real overhead telemetry packet and checks the real applied-command/outbox marker, not
  a synthetic sentinel.
- `any_model_arm_count_for_ground_station_matches_star_tracker` — fails against a future edit
  that adds a `GroundStation` call site (or removes a `StarTracker` one) without its matching
  counterpart, drifting the two out of lockstep. **[break/restore]** added one dangling comment
  occurrence of the needle string: count jumped from 15==15 to 17!=15, test failed with the
  exact numbers in its own message; restored.
- `construct_ground_station_builds_a_usable_zero_dimensional_handle`,
  `..._returns_a_typed_error_for_a_tm_codec_missing_a_required_field` (`registry.rs`) — fail
  against a missing constructor (compile error) or one that skips the `x`/`y`/`z` field check.
- `a_dynamics_fault_on_a_ground_station_instance_changes_each_declared_field_independently`,
  `..._naming_a_non_numeric_field_is_a_typed_error` (`fault.rs`) — fail against a target mapped
  to the wrong field, or `station.body` silently accepted as a numeric fault target.
- `constant_accel_model_with_emit_framed_broadcasts_its_propagated_position_every_step`
  (`binding.rs`) — fails against broadcasting the pre-step state instead of the propagated one,
  or gating the broadcast on `condition` the way `emit` is gated. **[break/restore]** encoded
  `state[0]` (pre-step) instead of `result.state[0]` (propagated): decoded `x` was `1000.0`
  (input) instead of the expected `1010.0` (after 1 s at 10 m/s); restored.
- `the_ground_segment_drm_runs_through_execute_and_reports_one_rise_and_set_pass`
  (`tests/drm_ground_segment.rs`) — fails against `"ground."` never reaching `kind_for` at all
  (DRM refused before propagation), a silent-trait-default `step_with_ports`, or
  `ConstantAccelModel::emit_framed` never actually broadcasting (an always-empty `tm_in` inbox,
  no elevation ever computed). Live measurement above (§3): AOS/LOS both land inside the
  generous stated windows.
- `contact_windows_matches_gmats_own_contact_locator_for_the_same_site_and_arc`
  (`tests/ground_contact_gmat.rs`) — the acceptance pin itself (§5); fails against a swapped
  ENU component, a sign error, the wrong reference ellipsoid, or nearest-sample instead of
  interpolated crossings (any of which would move the measured delta from ~0.4 s to several
  seconds or more, past the justified 10 s tolerance).

**Delegation tests not independently broken one by one, disclosed rather than silently claimed
complete**: the nine `any_model_*_delegates_to_the_ground_station_variant` tests share one
underlying defect class (a missing/misrouted match arm) that Rust's own exhaustiveness check
already turns into a compile error, and the arm-count symmetry test (itself break/restored)
covers the "quietly wrong wiring that still compiles" case generically. Given the ~300-tool-use
budget, live break/restore was spent on the geometry, interpolation, symmetry-check, and
`emit_framed` logic — the actual novel code this task wrote — rather than re-proving the same
"missing match arm" defect nine more times by hand.

## Every golden, with its `--reason`

- `goldens/ground_contact_gmat.json` — `goldens/gen_ground_contact_gmat.py --reason "M25.1
  acceptance pin: ground contact windows vs GMAT ContactLocator (fix: self-consistent epoch
  reference for contact windows)"`. GMAT `ContactLocator` AOS/LOS windows plus a dense
  `EarthFixed` position series for `leo_demo_sys`'s own orbit against a Cape-Canaveral-like site
  — the acceptance pin for `crate::drm::ground::{elevation_of, contact_windows}` (§5).

No other golden file was regenerated or touched by this task.

## Test totals (measured, this run)

- `cargo build` (workspace): clean.
- `cargo test --workspace --exclude av-kernel`: **all passed** (every crate's own suite, incl.
  `gmat-sys`'s STM/drag-SRP/convert-rotation tests, `av-run`, `av-lockstep*`).
- `cargo test -p av-kernel --lib`: **542 passed, 0 failed** (unit tests; includes this task's own
  12 `ground.rs` + 11 `binding.rs` delegation/symmetry + 2 `registry.rs` + 2 `fault.rs` + 1
  `emit_framed` test).
- `cargo test -p av-kernel` (full, integration tests included): **all passed except the one
  pre-existing, documented Renode failure** (`drm_attitude_control_renode.rs::
  byte_identical_port_traffic_between_posix_container_and_renode`, question 171's known STEP-2
  gap — unrelated to this task, reproduced identically with the exact same panic message before
  any M25.1 change was made). Includes the two new integration test files
  (`drm_ground_segment.rs`, `ground_contact_gmat.rs`), both passing.
- `cargo clippy -p av-kernel --all-targets -- -D warnings`: clean (one lint fixed along the way —
  a doc-comment lazy-continuation warning in `ground.rs`, unrelated to logic).
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo deny check`: `advisories ok, bans ok, licenses ok, sources ok` (one pre-existing,
  unrelated `windows-sys` duplicate-version warning from the existing `tonic`/`tokio` dependency
  tree, not introduced by this task).
- `.venv/bin/pytest -q`: **421 passed** (matches the stated baseline exactly — no Python code was
  touched by this task beyond the new, non-test golden generator script).

## Not done / disclosed gaps

- **`ground.tc_out` is declared but not consumed by the flight side in the live demo.**
  `ConstantAccelModel` has no FRAMED-*consume* capability (only its existing SIGNAL
  `port.consume`) — building one was judged out of this task's core scope (bullet 2 only
  requires the FRAMED command port to exist and be declared with a codec, which it does; the
  encode-and-send half is proven by `step_with_ports_reports_contact_start_and_sends_an_ack_
  when_a_target_rises_above_the_mask`). Stated here rather than silently left unmentioned.
- **Nine `AnyModel::GroundStation` delegation tests were not each individually broken and
  restored** (see the note at the end of the test-function list above) — the arm-count symmetry
  test plus Rust's own exhaustiveness check were judged to cover the same defect class with a
  better cost/benefit than nine near-identical break/restore cycles, given the ~300-tool-use
  budget; disclosed rather than silently claimed as nine separate proofs.
- **No tolerance was loosened anywhere in this task.** The one tolerance this task introduces
  (`ground_contact_gmat.rs`'s 10 s) is disclosed above with its derivation, fixed before the
  comparison was run, and the actual measured margin (~25x) is reported honestly rather than
  used to justify a tighter number after the fact.
- Scope respected: no edits to `proto/`, `web/`, `altavista/`, `services/`, `third_party/`, or
  `docs/adr/`. `EVENT_KIND_CONTACT_START`/`_END`, `PORT_KIND_FRAMED`, `AXES_KIND_ENU`/`NED`, and
  `Geodetic`/`FrameDefinition.origin_geodetic` were all already present in the proto and used
  as-is; no proto field was added.
