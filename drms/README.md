# `drms/`

Design Reference Mission fixtures for `av-kernel`'s DRM executor (`crates/av-kernel/src/drm`,
M6.1, `docs/open-questions.md` question 87). Each DRM is three files:

- `<name>.system.yaml` -- one `SystemDefinition` (`proto/altavista/v1/system.proto`).
- `<name>.sos.yaml` -- one `SosConfiguration`, binding instances to system definitions.
- `<name>.drm.yaml` -- one `DesignReferenceMission`: the `SosConfiguration` it names, a
  `Scenario` (time window, faults, seeds), and `DrmOptions`.

## Authoring format

YAML, field-for-field mirroring the proto message it represents -- see
`crates/av-kernel/src/drm/schema.rs`'s module doc comment for the full rationale (in short:
`av_cdm::pb` types have no `serde` derive, so this crate hand-writes a 1:1 mirror struct per
message rather than inventing a different, "friendlier" schema). Enum fields are written as
the proto enum's own name (`BINDING_KIND_MODEL`, `FAULT_TARGET_KIND_DYNAMICS`, ...) -- the
same string form protobuf's own canonical JSON mapping uses.

A `SystemDefinition`'s `parameters` (generic `name`/`value`/`string_value` triples -- the CDM
has no dedicated force-model or orbital-elements message yet) carry everything a
`"gmat."`-dispatched binding needs to construct a real GMAT `Spacecraft`/`ForceModel`: see
`crates/av-kernel/src/drm/binding.rs`'s module doc comment for the exact `force_model.*` /
`spacecraft.*` vocabulary. `spacecraft.*` parameters are forwarded **verbatim** to GMAT's own
`Object::set_real`/`set_str` -- so `spacecraft.SMA`, `spacecraft.DryMass`, `spacecraft.Cd`,
etc. are literally GMAT field names, not a re-invented vocabulary. This is also how question
81 ("a seed is a vehicle, not a state vector") is satisfied here: the golden fixture below
declares all five ballistic fields explicitly rather than leaving them at GMAT's defaults.

**`Scenario.events` (M10.1, question 97).** Typed as of M10.1 -- an event's `kind` must be
`"maneuver"` (any other value is refused, not silently accepted); a `"maneuver"` event needs a
non-empty `instance`, `values` with exactly `dv_x`/`dv_y`/`dv_z` (SI m/s, in the declared
frame's own basis order), and `attributes` with exactly `frame_id` (one of `AXES_KIND_RIC`/
`AXES_KIND_VNB`/`AXES_KIND_VVLH`/`AXES_KIND_ICRF`/`AXES_KIND_MJ2000_EQ` -- `AXES_KIND_LVLH`
was renamed to `AXES_KIND_VVLH` by M12.4, question 106, and is now a typed load error, not a
recognized value). See
`crates/av-kernel/src/drm/maneuver.rs`'s module doc comment for the full contract and the
`leo_1day_maneuver_vnb` section below for a worked example:

```yaml
scenario:
  events:
    - id: burn1
      tai_ns: 1767229237000000000   # must land exactly on the sample_interval_s output grid
      kind: maneuver
      instance: leo_mvr
      values:
        dv_x: 20.0   # V (VNB) -- SI m/s
        dv_y: 0.0    # N
        dv_z: 0.0    # B
      attributes:
        frame_id: AXES_KIND_VNB
```

**`ScenarioEvent.execution_error` (M11.4, question 100): the Gates maneuver execution error
model.** A `"maneuver"` event may additionally declare `execution_error` -- the Gates model (S.
Gates, *"A Simplified Model of Midcourse Maneuver Execution Errors,"* JPL Technical Report
32-1234, 1963): a fixed and a proportional-to-`|dv|` sigma for magnitude error (along the
commanded `dv`'s own direction) and for pointing error (in the plane transverse to it), plus a
`seed` key into `Scenario.seeds`. **Absent means a perfect burn and is never defaulted** -- all
four sigmas are required and checked finite when the block *is* present (a declared `0.0` is an
explicit zero, not an omission), and `seed` must name a real `Scenario.seeds` key or the DRM is
refused at load. See `crates/av-kernel/src/drm/maneuver.rs`'s module doc comment ("Burn
execution error") for the model's exact formulas and the two paths it drives (a Gates-sampled
realization on a plain/non-covariance run; an analytic `P+ = P- + G Q G^T` injection on a
covariance run, `Phi` untouched either way):

```yaml
scenario:
  seeds:
    burn1_error: 20260903   # any u64; the DRM author picks the key name
  events:
    - id: burn1
      tai_ns: 1767229237000000000
      kind: maneuver
      instance: leo_mvr
      values:
        dv_x: 20.0
        dv_y: 0.0
        dv_z: 0.0
      attributes:
        frame_id: AXES_KIND_VNB
      execution_error:
        sigma_magnitude_fixed_mps: 0.02        # m/s, 1-sigma
        sigma_magnitude_proportional: 0.001    # dimensionless fraction of |dv|
        sigma_pointing_fixed_mps: 0.01         # m/s, 1-sigma, per transverse axis
        sigma_pointing_proportional_rad: 0.0006 # radians, 1-sigma, per transverse axis
        seed: burn1_error                      # must name a Scenario.seeds key
```

`crates/av-kernel/tests/gates_execution_error.rs` is the acceptance suite: byte-identical
determinism across two sampled runs sharing a seed, a present-but-all-zero `execution_error`
block reproducing the perfect-burn golden (`leo_1day_maneuver_vnb.json`) bit-for-bit, the
analytic injection matching the sample covariance of 500,000 independently-seeded sampled draws
to a 5-standard-error tolerance derived from sampling theory (measured agreement: well under 1.5
SE on every one of the nine 3x3 matrix entries -- see that test's own `eprintln!` output), a
proportional-only block's variance scaling exactly as `|dv|^2`, the "fresh substream per event
id" property proven end to end (adding a second maneuver event that shares the same
`Scenario.seeds` key never shifts the first event's own sampled draw), and the covariance path's
own wiring injecting exactly `G Q G^T` (measured relative error ~1e-16, floating-point noise).

## Declared state spaces (M9.2, `docs/open-questions.md` question 94)

`SystemDefinition.state_space_id` (a bare id string) has always had to resolve against
`crates/av-kernel/src/trajectory.rs`'s built-in registry of three ids
(`altavista.cartesian_pos_vel_6`, `altavista.cartesian_pos_vel_6_attitude_quat_4`,
`gmat.orbital.cartesian6`). Question 94 makes the declaration itself part of the artifact,
additively: a `SystemDefinition` may carry its own `state_space` block --

```yaml
state_space_id: gmat.orbital.cartesian6
state_space:
  id: gmat.orbital.cartesian6        # must equal state_space_id -- checked, not assumed
  components:
    - label: pos_x
      unit: UNIT_METER
    - label: pos_y
      unit: UNIT_METER
    - label: pos_z
      unit: UNIT_METER
    - label: vel_x
      unit: UNIT_METER_PER_SECOND
    - label: vel_y
      unit: UNIT_METER_PER_SECOND
    - label: vel_z
      unit: UNIT_METER_PER_SECOND
  # frame_id: ""                     # optional; a FrameDefinition.id
```

**Resolution** (`crates/av-kernel/src/trajectory.rs::resolve_state_space`, the function the
DRM executor calls once per instance in place of a bare registry lookup):

- `state_space` present -> authoritative. Its `id` must equal `state_space_id` (refused
  otherwise, naming both strings) and every component must classify under ADR-005 sec 3
  (position/velocity, a `q_x..q_w` quaternion group, a linear scalar, or a zero-order-hold
  discrete label -- never a silent fallback to linear for a component that does not fit).
  A declared `state_space` is **not** cross-checked against the registry's own idea of the
  same id -- it may redefine what that id means for this one artifact.
- `state_space` absent -> `state_space_id` must name one of the three built-in ids above.

`leo_1day_golden.system.yaml` declares its `state_space` explicitly (field-for-field the
same shape the built-in registry already returns for `gmat.orbital.cartesian6`) -- the
declaration did not change what that id means, only made it an artifact property rather
than something that existed only by the registry's own convention (ADR-001). Declaring it
changed the file's own canonical hash; the recomputed value was pasted in the same way any
other content change is (see "Hashing" below).

A DRM authored from an altavista scenario declares the identical `StateSpace` message
`altavista.cdm.state_space_for`/`CdmBundle.state_spaces` already emit on the CDM path (same
id, labels, units, order) -- `crates/av-kernel/tests/state_space_declaration.rs` and
`tests/test_cdm_adapter.py::test_a_altavista_emitted_state_space_round_trips_through_the_rust_
drm_loader` prove that shape round-trips through this loader and through
`resolve_state_space`.

## Hashing

Every `hash` field is SHA-256 (hex) of the message's own canonical protobuf encoding
(`prost::Message::encode_to_vec`) with `hash` itself cleared first -- the same convention
`tests/test_cdm_v1.py::test_drm_with_ric_frame_and_mixed_bindings_round_trips` already uses on
the Python side. Compute or check one with:

```bash
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cargo run -p av-kernel --example drm_hash -- drm    drms/leo_1day_golden.drm.yaml
cargo run -p av-kernel --example drm_hash -- sos    drms/leo_1day_golden.sos.yaml
cargo run -p av-kernel --example drm_hash -- system drms/leo_1day_golden.system.yaml
```

The executor recomputes and checks each one before running anything -- a DRM/SosConfiguration/
SystemDefinition whose declared `hash` does not match its own content is refused
(`DrmError::HashMismatch`), never silently accepted or merely warned about.

## `leo_1day_golden`

Reproduces `goldens/leo_1day_jgm2_8x8_sunmoon.json` (LEO, JGM2 8x8 Earth gravity, Sun + Moon
point masses, one day) as a DRM: one instance (`leo`), bound to a real
`gmat_sys::model::GmatModel`, at the same 10 Hz native step rate and 10 Hz output sampling
rate `tests/golden_acceptance.rs` uses (so the DRM path reproduces that test's own measured
result, not a differently-configured run). `crates/av-kernel/tests/drm_executor.rs`'s
`drm_matches_the_golden_arc` runs it end to end (with `options.covariance: false`) and checks
the final state against the golden's own recorded tolerance.

The `leo` instance's `initial_covariance` (question 89, `SystemInstance.initial_covariance`)
is the golden's own declared P0 (`goldens/leo_1day_jgm2_8x8_sunmoon.json`'s `stm.p0_si`:
row-major 6x6 SI, diag(10000, 10000, 10000, 0.01, 0.01, 0.01), i.e. 100 m position / 0.1 m/s
velocity 1-sigma) -- unused by `drm_matches_the_golden_arc` (whose DRM keeps `covariance:
false`), but read by `crates/av-kernel/tests/drm_executor.rs::drm_covariance_matches_the_
golden_stm_and_propagated_cov`, which builds its own covariance-enabled DRM/SosConfiguration
around this same `leo_sys` and P0 and checks the result against the golden's `stm.cov_t1_si`.

`scenario.start_tai_ns`/`end_tai_ns` are the golden's own epoch (`"01 Jan 2026
00:00:00.000"` UTC) and one-day duration, expressed in TAI nanoseconds -- computed once,
offline (see the comment at the top of `leo_1day_golden.drm.yaml` for the arithmetic), not by
any calendar-parsing code in this crate. The DRM executor converts this TAI instant to GMAT's
A1MJD numerically (`av_cdm::time::Tai::to_a1_mjd`) when it binds the GMAT spacecraft, and
cross-checks GMAT's own read-back epoch against the declared value.

## `leo_1day_maneuver_vnb` (M10.1, question 97)

The impulsive-maneuver golden: the *same* vehicle and force model as `leo_1day_golden`
(`leo_1day_maneuver_vnb.sos.yaml`'s `leo_mvr` instance binds to `leo_1day_golden.system.yaml`'s
`leo_sys` directly -- no separate `SystemDefinition` file, so the two goldens can never drift
apart on the vehicle), with a single declared `"maneuver"` `Scenario.events` entry: a 20 m/s
prograde burn in VNB, one hour after `start_tai_ns`, over a two-hour scenario window (one hour
before the burn, one hour after). `sample_interval_s: 60.0` (coarser than `leo_1day_golden`'s
10 Hz -- this golden only needs samples at the burn/final epochs, not a full trajectory) while
`default_step_rate_hz: 10.0` keeps the same native integration accuracy as the plain golden
(the DRM executor's native step rate and output sample rate are independent outside the
covariance path -- see `crates/av-kernel/src/drm/executor.rs`'s module doc comment).

`goldens/gen_leo_1day_maneuver_vnb.py` generates `goldens/leo_1day_maneuver_vnb.json` by
running the identical scenario through **altavista's own `Scenario.maneuver` call**
(`frame="VNB"`) -- the reference implementation `crates/av-kernel/src/drm/maneuver.rs::
dv_to_inertial`'s VNB branch is pinned against, not a hand re-derivation of its V/N/B basis
formula. `tests/drm_maneuver.rs::drm_matches_the_maneuver_golden_vnb_burn` runs this DRM
through the executor and checks the sample at the burn's own applied epoch against the
golden's `state_post_burn`, and the final sample against `final_state`, at the same tolerance
class as `leo_1day_jgm2_8x8_sunmoon.json` (0.05 m / 5e-5 m/s) -- measured agreement is far
tighter (|dr| = 0.0000 m, |dv| ~ 1-2e-9 m/s; see `crates/av-kernel/README.md`'s "Impulsive
maneuvers" section for the exact numbers).

## `leo_1day_maneuver_ric` / `leo_1day_maneuver_vvlh` (M11.3, question 102; renamed by M12.4, question 106)

Same vehicle/force model/timing as `leo_1day_maneuver_vnb` (both `.sos.yaml`s bind directly
to `leo_1day_golden.system.yaml`'s `leo_sys`; same 3600 s pre-burn / burn / 3600 s post-burn
window, same `01 Jan 2026 00:00:00Z` start epoch), a single 20 m/s burn, but through
`Scenario.maneuver`'s `frame="RIC"`/`frame="VVLH"`/`frame="LVLH"` paths (`altavista/scenario.py::
Scenario._fire_impulsive_burn`) instead of the hand-rolled VNB basis -- all three fire a
**real GMAT `ImpulsiveBurn`** rather than a Python-computed rotation:

* **RIC** (`goldens/gen_leo_1day_maneuver_ric.py` / `leo_1day_maneuver_ric.json`,
  `leo_1day_maneuver_ric.{sos,drm}.yaml`, instance `leo_mvr_ric`): the burn's
  `CoordinateSystem` is an ObjectReferenced RIC system (`XAxis = R`, `ZAxis = N`, question
  73) built through `altavista.frames.FrameRegistry` -- the same registry `_build_frames`
  uses, not a second ObjectReferenced builder. `dv = (0, 20, 0)` m/s (in-track).
  `tests/drm_maneuver.rs::drm_matches_the_maneuver_golden_ric_burn` matches this golden at
  the same tolerance class as VNB (0.05 m / 5e-5 m/s) -- measured |dr| = 0.0000 m,
  |dv| ~ 1-3e-9 m/s, because `crates/av-kernel`'s ratified `AxesKind::Ric` is exactly this
  same X=R,Z=N convention.
* **GMAT-native LVLH** (`goldens/gen_leo_1day_maneuver_gmat_lvlh.py` /
  `leo_1day_maneuver_gmat_lvlh.json` -- renamed by M12.4 from `..._lvlh.py`/`.json` so the
  filename could never be mistaken for this platform's own convention): the burn is fired
  with `CoordinateSystem = Local`, `Origin = Earth`, `Axes = LVLH` -- **GMAT's own literal,
  native local burn axes**, per question 102's decision. Same `dv = (0, 20, 0)` m/s as the
  RIC golden, deliberately (see below).
* **VVLH** (`leo_1day_maneuver_vvlh.{sos,drm}.yaml`, instance `leo_mvr_vvlh`, added by
  M12.4): declares this platform's own ratified `AXES_KIND_VVLH` (`frame_id:
  AXES_KIND_VVLH`) against the same `dv`, the numbers `leo_1day_maneuver_lvlh.{sos,drm}.yaml`
  used before the rename -- kept only as the fixture proving the retired name is now a typed
  load error (see below).

**The finding this task exists to surface** (see `altavista/FRAMES.md`'s "GMAT `ImpulsiveBurn`
`Axes = LVLH` vs `AXES_KIND_VVLH`" section for the full write-up): GMAT's `ImpulsiveBurn
Axes = LVLH` is measured, empirically, to realize `X = R, Y = N×R (in-track), Z = N` --
**numerically identical to `AXES_KIND_RIC`**, not to this platform's own ratified
`AXES_KIND_VVLH` (`Z = -R, Y = -N, X = N×R`, question 73's VVLH convention -- renamed from
`AXES_KIND_LVLH` by question 106 for exactly this reason). Direct proof:
`leo_1day_maneuver_gmat_lvlh.json`'s `state_pre_burn`/`state_post_burn`/`final_state` are
bit-for-bit identical to `leo_1day_maneuver_ric.json`'s, despite being generated through a
completely different GMAT mechanism (`Axes=LVLH` vs. an ObjectReferenced
`CoordinateSystem`).

Consequently, `leo_1day_maneuver_vvlh.drm.yaml` declares its `ScenarioEvent`
`frame_id: AXES_KIND_VVLH` (the honest tag for "a caller who wants this platform's ratified
VVLH convention"), and running it through `crates/av-kernel`'s executor (whose
`dv_to_inertial::AxesKind::Vvlh` arm, read-only/ratified, implements that different VVLH
convention) does **not** reproduce this golden --
`tests/drm_maneuver.rs::drm_maneuver_axes_kind_vvlh_does_not_reproduce_gmats_native_lvlh_burn`
measures and asserts the real mismatch rather than skipping the comparison or loosening the
tolerance: |dv| = 28.284271 m/s at the burn epoch itself (the two conventions send the same
`dv_y = 20` m/s component in orthogonal directions -- in-track vs. anti-normal) and
|dr| = 276275.3 m / |dv| = 271.3 m/s an hour later, after coasting on the resulting wrong
post-burn velocity. The sibling test
`drm_maneuver_axes_kind_ric_reproduces_gmats_native_lvlh_burn` retags the identical `dv`
`AXES_KIND_RIC` instead and confirms *that* reproduces `leo_1day_maneuver_gmat_lvlh.json` at
the tight golden tolerance -- independent, Rust-side confirmation of the same mapping
altavista measured on the Python side, i.e. the record that GMAT's `Axes=LVLH` equals
`AXES_KIND_RIC`. Neither the goldens nor `dv_to_inertial`'s ratified `AxesKind::Vvlh` arm are
altered anywhere in this task to force a match.

**The retired name.** `leo_1day_maneuver_lvlh.{sos,drm}.yaml` are kept on disk, deliberately
unchanged (still literally declaring `frame_id: AXES_KIND_LVLH`), no longer as a mismatch
fixture but solely to prove that name is refused at load: `core.proto` now carries
`reserved "AXES_KIND_LVLH";` on the enum, and
`tests/drm_maneuver.rs::a_drm_naming_axes_kind_lvlh_is_a_typed_load_error` pins
`DrmError::InvalidEnumValue` for it. See that fixture's own header comment for the full
story.

## `demo_two_instance` (M17.3, `docs/open-questions.md` question 123)

The two-instance demo fixture: **the first fixture in this repo to declare more than one
`BINDING_KIND_MODEL` instance on real GMAT dynamics.** "Decided by the lead: a two-instance DRM
fixture with a real fault on one instance and a maneuver on the other becomes a golden and is
the demo bundle."

- `drms/demo_two_instance.system.yaml` -- `leo_demo_sys`: field-for-field the same vehicle and
  JGM2 8x8 Earth gravity + Sun/Moon force model as `leo_1day_golden.system.yaml`'s own
  `leo_sys` (same orbit, same ballistic properties), plus one additive `output.rmag`
  declaration. A **separate** file from `leo_1day_golden.system.yaml` rather than adding
  `output.rmag` there directly -- that file is an existing golden this task must not change
  (see its own header comment for the reasoning).
- `drms/demo_two_instance.sos.yaml` -- two instances bound to `leo_demo_sys`:
  - `demo_flt` -- no `initial_covariance` (mutually exclusive with its own DYNAMICS fault,
    `DrmError::CovarianceWithFaultsNotSupported`).
  - `demo_mvr` -- declares `initial_covariance`, the identical P0
    `leo_1day_golden.sos.yaml`'s own `"leo"` instance uses (diag(10000, 10000, 10000, 0.01,
    0.01, 0.01)).
- `drms/demo_two_instance.drm.yaml` -- one scenario, `01 Jan 2026 00:00:00Z` start
  (`leo_1day_golden`'s own epoch), `sample_interval_s: 60`:
  - **`demo_flt`'s real DYNAMICS fault** at `start + 1800 s`: `force_model.gravity_order`,
    8 -> 0 (JGM2 8x8 drops to zonal-only, degree stays 8). A **real** fault on **real** GMAT
    dynamics -- the first in this crate; every other `FAULT_TARGET_KIND_DYNAMICS` fixture in
    this crate's own test suite (`tests/faults_determinism.rs`, `tests/faults_seeded.rs`,
    `tests/expr_goldens.rs`'s `fault_split_accel` golden, `tests/drm_executor.rs`'s
    `a_dynamics_fault_splits_the_run_into_two_segments_with_continuous_state`) uses the
    synthetic `native.constant_accel` `"accel.x"` target instead. `spacecraft.Cd`/`DragArea`
    were considered and rejected: `leo_sys`'s own force model (`materialize_gmat`) never
    constructs a `DragForce` regardless of `Cd`/`DragArea`, so faulting either would be
    physically inert -- a vacuous fault, exactly the kind of "still passes against the known-
    bad implementation" test this task's own review standard forbids. `force_model.gravity_order`
    is real: measured effect (faulted vs. an identical unfaulted counterfactual, same
    two-hour window) is 721.3 m at the end -- see
    `crates/av-kernel/tests/demo_two_instance.rs`'s own closed-form check.
  - **`demo_mvr`'s VNB maneuver** at `start + 5400 s`: the identical 20 m/s prograde burn
    `leo_1day_maneuver_vnb.drm.yaml` declares.
  - **One `Objective`**, `demo_flt_rmag_at_end`, scoring `output.demo_flt.rmag@end` against a
    target/tolerance taken from the golden's own recorded `rmag_at_end_flt_m`
    (6870541.971488949 m, tolerance 0.05 m -- see below).
  - `options.covariance: false` -- required for the fault + shared kernel run + real-GMAT
    two-instance execution to work at all (`DrmOptions.covariance` is DRM-wide, not
    per-instance: `true` sends **every** instance through the isolated covariance path, which
    refuses a DYNAMICS-faulted instance outright and requires every instance to declare its
    own `initial_covariance`). `demo_mvr`'s own declared covariance is a load-bearing
    **declaration**, exercised by a dedicated single-instance covariance-enabled DRM built in
    the test (see "Covariance" below) -- never by turning `options.covariance` on for the
    committed two-instance DRM itself.

`goldens/gen_demo_two_instance.py` generates `goldens/demo_two_instance.json` through
**altavista's own force-model/propagator construction and its own `Scenario.maneuver` VNB path**
(never a hand re-derivation): both spacecraft propagate together under the unfaulted force
model for the first 1800 s (bit-identical initial conditions, checked), then `DemoFlt`'s own
`ForceModel`/`Propagator` is rebuilt with `Order=0` for the remaining two hours (mirroring
`crates/av-kernel/src/drm/fault.rs::apply_gmat_target`'s `"force_model.gravity_order"` branch +
`binding.rs::materialize_gmat`'s re-binding, both pinned against this golden), while `DemoMvr`
propagates unchanged to the burn epoch, fires `altavista.scenario.Scenario.maneuver(frame="VNB")`
(the same reference path `leo_1day_maneuver_vnb` pins), and propagates unchanged to the end.

**Tolerance (0.05 m / 5e-5 m/s), derived, not fitted.** The same tolerance class every VNB/RIC/
VVLH maneuver golden in this file already uses: `leo_1day_jgm2_8x8_sunmoon.json`'s own
0.0001 m / 7.837e-8 m/s (that golden's own tolerance, derived from the JGM2 8x8 + Sun/Moon
force model's PrinceDormand78 `Accuracy = 1e-13` integration bound), loosened by the maneuver
goldens for the additional burn-frame rotation step -- see `leo_1day_maneuver_vnb`'s own
section above. `demo_two_instance` reuses this existing, already-derived class verbatim
(measured agreement in the Rust test is far tighter: |dr| = 0.0000 m, |dv| ~ 3-4e-9 m/s for
`demo_flt`/`demo_mvr`, |err| = 2.0e-7 m for the Objective).

### Test coverage (`crates/av-kernel/tests/demo_two_instance.rs`)

Three tests, all against the real committed fixture (loaded from disk, never rebuilt in Rust):

1. **`demo_two_instance_matches_the_golden_and_scores_its_objective`** -- the two-instance run
   matches `goldens/demo_two_instance.json` (`demo_flt`'s faulted final state, `demo_mvr`'s
   post-burn and final states), and the declared `Objective` scores `value` within tolerance of
   the golden's own `rmag_at_end_flt_m` with `passed == Some(true)`. Fails against: a mis-bound
   instance, a fault/maneuver applied at the wrong epoch or to the wrong instance, or an
   `Objective` evaluator reading the wrong instance's `output.rmag` or computing `passed` wrong.
2. **`demo_two_instance_bystander_invariance_against_real_single_instance_gmat_runs`** -- see
   "Bystander invariance" below.
3. **`covariance_is_declared_on_demo_mvr_only_and_the_asymmetry_is_load_bearing`** -- see
   "Covariance" below.

### Bystander invariance -- the first real-GMAT measurement, two findings

`tests/restart_invariance.rs` already proved bystander invariance for a **native**
(`native.constant_accel`) bystander; its own module doc comment says plainly this crate never
checked it against a real GMAT run. `demo_two_instance` is that check, and finds **two** real,
previously-invisible effects:

1. **Physical samples (`TrajectorySample.mean`) are NOT bit-identical, but agree to ~1 ULP.**
   `demo_flt`'s own samples, run alone vs. together with `demo_mvr`, first diverge at the
   sample immediately after `demo_mvr`'s own maneuver boundary (t = 5400 s) -- worst observed
   `|delta|` = 2.27e-13 on a ~1252 m/s velocity component (relative error ~1.8e-16, one ULP of
   an `f64`). Root cause: unlike a native model's re-materialization (an exact copy of the same
   `f64` state), a GMAT-bound re-materialization (`fault::rebind_gmat_spec_at_state` +
   `binding::materialize_gmat`) rebuilds a real GMAT `Spacecraft` through a genuine SI metre ->
   km -> GMAT-internal -> back round trip plus a fresh `Initialize()` -- not perfectly
   round-trip-neutral at the last bit. This is **not a physics bug**: the test asserts a
   1e-6 m/(m/s) tolerance (measured max across both instances: 1.86e-9 and 1.34e-7, both ~1e4x
   below the bound, itself ~1e6x above the observed noise floor) rather than exact equality,
   and reports the max delta either way.
2. **Segments now DO merge back for a real GMAT bystander (`merge_adjacent_segments`) --
   closed by M18.4 (question 127).** As first measured in M17.3, they did not: `demo_flt`
   alone had 2 segments (its own fault boundary) but 3 together (an extra, un-merged segment
   from being re-materialized at `demo_mvr`'s maneuver boundary), and the same for `demo_mvr`.
   The cause was that a GMAT-bound instance's `dynamics_hash` baked in its own instantaneous
   state: `rebind_gmat_spec_at_state` sets `spacecraft.X/Y/Z/VX/VY/VZ` before every
   re-materialization (and also flips `DisplayStateType` to `Cartesian`, dropping the Keplerian
   element keys), so the hash differed at every re-materialization epoch regardless of whether
   the dynamics configuration itself changed. M18.4 makes `binding::gmat_settings` hash the
   dynamics **configuration** only, excluding `fault::CARTESIAN_FIELDS`, `KEPLERIAN_FIELDS` and
   `DISPLAY_STATE_TYPE_FIELD`, so an untouched bystander's hash is genuinely unchanged across a
   boundary and the segments collapse back to the single-instance shape. The bystander test now
   asserts that as an **equality** on `(start_tai_ns, end_tai_ns, dynamics_hash, dynamics_depth)`.
   The exclusion is narrow by design: `force_model.*` and the ballistic `spacecraft.*` parameters
   (Cd, Cr, DragArea, ...) are still hashed, so a DYNAMICS fault -- the demo's own
   `force_model.gravity_order` 8 -> 0 -- still changes the hash and still splits the segment.

### Covariance -- present on `demo_mvr`, absent (and refused) on `demo_flt`

`demo_two_instance.sos.yaml`'s `demo_mvr` instance declares the golden P0; `demo_flt` does
not. The test checks both the **declaration** (36 values on `demo_mvr`, matching
`leo_1day_golden`'s own P0 exactly; zero on `demo_flt`) and that the asymmetry is
**load-bearing**: attempting `options.covariance = true` against a DRM shaped exactly like the
committed one (both instances, no fault declared, so the fault-exclusion rule is not what is
being isolated) is refused by name, `DrmError::MissingInitialCovariance{instance: "demo_flt"}`
-- proving `demo_flt`'s absence is a real, checked precondition, not merely an unread field --
and a **positive** check that `demo_mvr`'s own declared covariance, run alone with covariance
enabled and its own maneuver applied, actually propagates a real, non-empty `cov` end to end.

(The probe and positive-check DRMs above rename their instances to `demo_flt_covprobe`/
`demo_mvr_covprobe`/`demo_mvr_covcheck` rather than reusing the literal `demo_flt`/`demo_mvr`
-- see "A previously-undiscovered GMAT hazard" below for why.)

### The SIGNAL connection between `demo_flt` and `demo_mvr` (M18.3, question 126 -- closed)

M17.3's "What to build" item 3 asked for "a connection between them carrying one SIGNAL, so the
router is exercised." Through M17.3 this was escalated rather than forced, because it could not
be expressed honestly without a source change outside that task's ownership:

- `gmat_sys::model::GmatModel` never overrode `DynamicsModel::step_with_ports` -- it used
  `av_dynamics`'s own default implementation, which ignores its `Inbox` and always returns an
  empty `Outbox`.
- `crate::drm::binding::parse_gmat_spec`'s own parameter allowlist refused
  `"port.emit"`/`"port.emit_value"`/`"port.consume"` on a `"gmat.*"`-dynamics `SystemDefinition`
  as `DrmError::UnknownParameter`.

**M18.3 closes both.** `parse_gmat_spec` now accepts `"port.emit"`/`"port.emit_output"` and
`"port.consume"`/`"port.consume_parameter"` (see that function's own doc comment for the exact
pairing rules, and `crate::drm::binding::GMAT_WRITABLE_PARAMETERS` for the declared-writable
allowlist -- `Cd` today), and `gmat_sys::model::GmatModel::step_with_ports` genuinely applies a
consumed SIGNAL value (through the new `gmat_sys::DerivativeModel::set_real_parameter`) before
its own next `step`, and emits one of its own named outputs (`OUTPUT_RMAG`/`OUTPUT_CD`) after
it. `demo_two_instance.system.yaml` now declares two `Port`s (`cd_cmd_out`/`cd_cmd_in`), and
`demo_two_instance.sos.yaml` declares the `Connection` plus each instance's own `port.*`
parameter overrides: `demo_mvr` emits its own live `output.rmag`, `demo_flt` consumes it into
its own `Cd`.

**Deliberately still physically inert on this fixture, through M18.3 -- superseded by M19.4
below (`docs/open-questions.md` question 131), kept here as the historical record.**
`leo_demo_sys`'s force model (gravity + Sun/Moon point masses) had no drag, so the commanded `Cd`
had zero effect on either instance's own propagated trajectory or on the `demo_flt_rmag_at_end`
Objective -- M18.3's own existing golden (`goldens/demo_two_instance.json`) needed no change.
`crates/av-kernel/tests/demo_two_instance.rs`'s own SIGNAL-port test (since replaced -- see
M19.4 below) proved the connection was genuinely exercised anyway: it checked the *delivered
value* itself (`demo_flt`'s own `output.cd@end` against `demo_mvr`'s own live `output.rmag@end`,
chosen specifically so a delivered-vs-never-delivered signal is unambiguous from the numbers
alone, never merely "the DRM loaded and ran" -- the same "a router-exercised test that would
still pass if no message were ever delivered is not a test" anti-pattern this crate's own review
standard names). A physically-meaningful, drag-inclusive "a commanded Cd change genuinely alters
the arc" case was proven separately, in `crates/gmat-sys/tests/gmat_port_cd_command.rs`, pinned
against a genuine GMAT script + `ReportFile` reference (`goldens/gmat_port_cd_command.json`) --
that fixture is unaffected by M19.4 and still stands on its own.

### M19.4 (`docs/open-questions.md` question 131): the drag-sail command replaces the physically-inert wiring

Question 131 named the M18.3 wiring above exactly what it was: "range magnitude fed into Cd on
an instance with no drag." **Decided by the lead:** a native controller instance commands a
drag-sail `Cd` change on `demo_flt` when a declared range condition holds, with drag in
`demo_flt`'s own force model, so the command visibly changes the arc.

**What changed, concretely:**

- A third instance, `demo_ctrl` (`drms/demo_two_instance_ctrl.system.yaml`), a native
  `ConstantAccelModel` (no physical trajectory of its own -- `a = [0,0,0]`) extended with a
  declared range condition (`crate::drm::binding::ConstantAccelSpec::condition`,
  `"condition.threshold_m"`/`"condition.mode"`). The wiring is now a chain, not a direct link:
  `demo_mvr --(cd_cmd_out, rmag)--> demo_ctrl --(cd_sail_cmd_out, Cd)--> demo_flt`
  (`drms/demo_two_instance.sos.yaml`'s own `connections`, two entries now).
- `demo_ctrl` consumes `demo_mvr`'s own live `rmag` every native step (cheap), but only ever
  *emits* the declared drag-sail `Cd` (`220.0`, ~100x `demo_flt`'s own baseline `2.2`) the first
  step the consumed value rises to or past the declared threshold -- edge-triggered and latched
  (`ConstantAccelModel::step_with_ports`'s own doc comment), never re-emitted afterward within
  one materialization. This is the fix for a real, measured size regression M19.3's own
  per-command event recording exposed on this exact fixture: the pre-M19.4 "consume every step,
  forever" wiring produced 71,999 `EVENT_KIND_PORT_COMMAND` events over the demo's 7200 s window
  (`tests/fixtures/demo_two_instance.runproducts.bin` ballooned from ~20 KB to ~42 MB); the
  edge-triggered design produces exactly 1.
- `demo_flt`'s own force model (`drms/demo_two_instance.sos.yaml`'s own `parameter_overrides` on
  `demo_flt` only -- `leo_demo_sys` itself, shared with `demo_mvr`, stays drag-free) now
  genuinely includes atmospheric drag: `force_model.drag_model = "JacchiaRoberts"`, plus the
  packaged CSSI space-weather file this repository's own GMAT install ships
  (`GMAT R2026a/data/atmosphere/earth/SpaceWeather-All-v1.2.txt`, never downloaded) via three
  companion `force_model.drag_*` fields, all required together
  (`crate::drm::binding::GmatSystemSpec::drag_model`'s own doc comment). Threaded into a real
  `DragForce` + atmosphere-model object pair by `materialize_gmat`, the identical pattern
  `crates/gmat-sys/tests/drag_srp_stm.rs::build_fm_drag_srp` already proves end to end
  (`Object::set_reference`, closing the M4.3-found "Atmosphere model not defined" gap).
- The drag-sail command now has a real, measured effect: ~20 m of position divergence at the end
  of the run between the commanded arc and a structurally identical run whose Cd is never
  commanded (`crates/av-kernel/tests/demo_two_instance.rs::demo_two_instance_signal_port_
  delivers_a_drag_sail_command_that_measurably_changes_the_arc`) -- reasoned about, in writing,
  *before* being measured (`goldens/gen_demo_two_instance.py`'s own module docstring: scaled from
  M18.3's own 891.8 m / Cd-factor-2 / 250 km-altitude calibration to this fixture's own
  factor-100 Cd change on a much-thinner ~500 km-altitude atmosphere).
- `goldens/demo_two_instance.json` was regenerated (`goldens/gen_demo_two_instance.py`, rewritten
  to build `demo_flt`'s own three-stage drag-inclusive arc and to independently re-derive the
  command epoch by propagating `demo_mvr`'s own drag-free arc and searching for the declared
  threshold crossing -- not copied from the Rust executor's own output). `demo_flt`'s own
  tolerance widened from 0.05 m to 1.0 m (disclosed; `demo_mvr`'s own tolerance is unchanged) --
  see that script's own `--tolerance-m` help text for the sub-second timing-sensitivity bound
  this is sized against.
- The bystander-invariance test's own comparison (below) needed a genuine, disclosed narrowing,
  not a loosened tolerance: `demo_flt`'s alone-run (no `SosConfiguration.connections` at all)
  never receives the drag-sail command, so from the command's own epoch onward the alone and
  together arcs are *supposed* to diverge -- `diff_or_panic` now takes an optional
  `stop_before_tai_ns` and stays bit-identical (unchanged `RESTART_ULP_TOLERANCE = 1e-6`) up to
  that point; `assert_command_epoch_diverges` checks the other half explicitly (growing,
  non-vacuous divergence), so the split narrows the comparison's scope honestly instead of
  silently ignoring the back half.

### A previously-undiscovered GMAT object-namespace hazard

While building the bystander-invariance test above, reusing an identical `SystemInstance.name`
bound to a real `"gmat.*"` system for **two independent `execute()` calls anywhere in one test
binary's process** (same `#[test]` function or a different one) reliably fails:

```
GMAT error -1: ODEModel Exception Thrown: Attempted to add a GravityField force to the force
model for the body Earth, but there is already a GravityField force in place for that body.
```

Confirmed with a minimal reproduction: two single-instance, no-fault-no-maneuver `execute()`
calls, both naming the instance `"demo_flt"` and nothing else different -- the second fails;
renaming only the second instance to `"demo_flt_2"` makes both succeed.
`crate::drm::binding::materialize_gmat`'s own `name_suffix` doc comment already says GMAT
object names are only guaranteed "unique within one `execute` call" -- true, but no test
before this task's own bystander-invariance methodology (inherently needing an alone run AND a
together run sharing the SAME instance name for the comparison to mean anything) ever
constructed the identical `"gmat.*"`-bound instance name more than once in one process; every
other GMAT-bound test in this crate calls `execute()` at most once per instance name in the
whole binary. The exact boundary inside `gmat_sys`'s FFI layer (why a fresh `Construct`/
`AddForce` under a previously-used name collides with leftover state) is not further diagnosed
here -- `crates/gmat-sys/**`/`crates/av-kernel/src/**` are not owned by this task.

**Worked around, not fixed, within this file's own ownership
(`crates/av-kernel/tests/demo_two_instance.rs`):** every real `"gmat.*"`-bound instance name
used anywhere in the file is globally unique, except the two literal names the committed
fixture itself uses (`"demo_flt"`/`"demo_mvr"`), which are constructed through GMAT **exactly
once** in the whole test binary via a `std::sync::OnceLock`-memoized `together_products()`
helper shared by every test that needs the real committed DRM's own result (documented in that
function's own doc comment, including why it must NOT take `gmat_sys::engine_lock()` itself --
`std::sync::Mutex` is not reentrant, and every caller already holds it).

### The exact swap for `tests/test_cdm_run.py` (worker BE owns that file this round)

M17.3's own remit: "this bundle replaces the hand-built FAULT fixture in the demo tests."
`tests/test_cdm_run.py` currently builds its FAULT-event demo bundle by hand (a synthetic
`RunProducts`/CDM bundle constructed directly in Python, not run through the executor). The
swap: replace that hand-built bundle with the real output of running
`drms/demo_two_instance.{drm,sos,system}.yaml` through `av_kernel::drm::execute` (via
whichever binding this repo's Python test layer already uses to invoke the Rust executor for
other DRM-backed tests -- see `tests/test_cdm_v1.py`/`tests/test_cdm_adapter.py` for the
existing pattern) and feeding `RunProducts.to_proto()`'s bytes into the same ingest path the
FAULT demo test already exercises. `demo_flt`'s own `EVENT_KIND_LIFECYCLE`/fault-application
events and `demo_mvr`'s `EVENT_KIND_MANEUVER` event replace whatever synthetic events the
hand-built bundle fabricated. Not applied here -- `tests/test_*.py` is owned by worker BE this
round.

## ICRF on the wire (M18.1, `docs/open-questions.md` questions 10/124)

Question 124: "the demo fixture cannot show ICRF -- the loader refuses `Scenario.frames`, and
the frame list on the wire only carries the coordinate system the instance propagated in plus
body fallbacks." Decided by the lead, no proto change, no ADR change:

1. **`Scenario.frames` is now fully typed**, mirroring `FrameDefinition` (`core.proto`)
   field-for-field, the same way `Scenario.events` already was (question 97) --
   `crates/av-kernel/src/drm/schema.rs`'s `RawFrameDefinition`/`RawGeodetic`/
   `RawAttitudeSource`. The placeholder refusal (`DrmError::UnsupportedField {
   field: "Scenario.frames" }` on any non-empty list) is gone.
2. A declared frame is realized through the same path the Python scenario uses:
   `crate::drm::executor::collect_frames` already preferred a declared `Scenario.frames` entry
   over a derived registry default for the same id (`registry_default_frame`) -- that branch
   was unit-tested but unreachable end to end before this task, since the loader refused any
   declared frame at all. It is reachable now; see the new end-to-end test below.
3. **Question 10's mandatory frames (ICRF, MJ2000 equatorial, body-fixed for the run's own
   central body) are always added to `RunProducts.frames`**, regardless of which coordinate
   system an instance actually propagated in --
   `crate::drm::executor::add_mandatory_body_frames`, reading `binding::GmatSystemSpec.
   central_body` off every `BINDING_KIND_MODEL` GMAT-bound instance's own plan (never guessed
   from a `Trajectory.frame_id` string). A declared or already-referenced frame for the same id
   always wins (never overwritten); a native (`ConstantAccel`) instance declares no central
   body and contributes nothing.
4. `drms/demo_two_instance.drm.yaml` now declares `EarthICRF` explicitly under
   `scenario.frames` (id/body/axes/description), on top of the mandatory augmentation --
   demonstrating the typed path end to end on the fixture the lead drives, not only the
   mandatory-frame fallback. Its own hash was recomputed (`cargo run -p av-kernel --example
   drm_hash -- drm drms/demo_two_instance.drm.yaml`); `demo_two_instance.sos.yaml`/
   `.system.yaml` are unchanged (the instances still propagate in `EarthMJ2000Eq`, per the
   task's own instruction not to drop or rename an instance's own propagation frame).

### Test coverage added (`crates/av-kernel/tests/demo_two_instance.rs`,
`crates/av-kernel/src/drm/schema.rs`/`executor.rs`'s own inline `#[cfg(test)]` modules)

- `schema::tests::parses_a_declared_scenario_frame_with_a_body_origin`,
  `a_frame_declaring_two_origin_fields_at_once_is_a_typed_load_error`,
  `a_frame_with_an_unrecognized_axes_name_is_a_typed_load_error`,
  `an_empty_frames_list_still_parses_exactly_like_before_this_field_was_typed` -- the typed
  `RawFrameDefinition` conversion itself.
- `executor::frame_registry_tests::mandatory_frames_appear_for_the_central_body_even_when_no_
  trajectory_referenced_them`, `mandatory_frames_never_overwrite_an_already_present_frame_for_
  the_same_id`, `a_native_instance_with_no_central_body_contributes_no_mandatory_frames`,
  `mandatory_frames_are_added_per_distinct_central_body_and_sorted` -- `add_mandatory_body_
  frames` in isolation.
- `demo_two_instance_run_products_frames_include_icrf_and_the_mandatory_central_body_frames`
  -- the real committed fixture's own `RunProducts.frames` carries the declared `EarthICRF`
  (with its author-supplied description, proving it is the declared entry, not a re-derived
  one), the instances' own `EarthMJ2000Eq` propagation frame (unchanged), and the mandatory
  `EarthBodyFixed`.
- `declared_scenario_frame_wins_over_the_registry_default_through_the_real_loader` -- an ad hoc
  DRM/SosConfiguration, real YAML text through `schema::parse_drm_yaml`/`parse_sos_yaml` (never
  a `pb` value built directly in Rust), declaring `EarthMJ2000Eq` with a distinguishing
  description; `execute()`'s own `RunProducts.frames` carries that description, not the
  registry-derived one -- the declared-wins precedence exercised end to end through the loader,
  not only as `executor::frame_registry_tests::collect_frames_prefers_a_declared_scenario_
  frame_over_the_registry_default`'s in-memory unit test.
- `mandatory_frames_appear_even_when_the_instance_propagated_in_icrf_not_mj2000eq` -- an ad hoc
  instance (`leo_demo_sys` cloned with `spacecraft.CoordinateSystem` overridden to
  `"EarthICRF"`) still gets `EarthMJ2000Eq`/`EarthBodyFixed` in `RunProducts.frames` even though
  neither is referenced by any trajectory in the run.
- `crates/av-kernel/tests/drm_executor.rs::drm_matches_the_golden_arc` and
  `crates/av-kernel/tests/drm_maneuver.rs::drm_matches_the_maneuver_golden_vnb_burn`'s own
  `RunProducts.frames` assertions were updated from "exactly one entry" to "the mandatory three,
  sorted by id" -- both pre-existing goldens propagate in `EarthMJ2000Eq` alone, so this is the
  mandatory-frame augmentation actually landing on fixtures this task did not otherwise touch.

### Escalated, not forced: the required GMAT-ICRF-report comparison test

The brief also asked for "the trajectory expressed in ICRF matches GMAT's own ICRF report for
the same arc" -- a real `av-kernel`-propagated trajectory compared against a genuine GMAT
script + `ReportFile` reference (`goldens/gen_icrf_leo_2h.py`/`goldens/icrf_leo_2h.json`, built
and verified exactly the way `goldens/gen_leo_1day_rmag.py` already does it in this repo).
**This one is not built, for a real, independently-verified reason, not lack of effort:**
`spacecraft.CoordinateSystem` only ever labels `Trajectory.frame_id` in this crate's current
GMAT binding -- it does not change the propagated Cartesian numbers `gmat_sys::DerivativeModel`/
`GmatModel` reads (`crates/gmat-sys/shim/gmatffi.cpp::gmatffi_model_state` returns
`PropagationStateManager`'s own raw internal state buffer, fixed to GMAT's own internal
representation regardless of what `Spacecraft.CoordinateSystem` names). Measured three
independent ways (a GMAT script comparing `Golden.EarthICRF.X` vs. `Golden.EarthMJ2000Eq.X` for
one spacecraft: ~1.4 m apart at LEO; the bare object API showing `CoordinateSystem` has zero
effect on read-back Cartesian state even for the unmistakable ~23.4 degree `EarthMJ2000Ec`
case; and `av-kernel`'s own `"EarthICRF"`-labeled run disagreeing with the real ICRF golden by
1.330 m, matching the first measurement's frame-bias magnitude). An attempted fix
(`Object::set_reference` wiring the named `CoordinateSystem` onto the production spacecraft,
`crates/av-kernel/src/drm/binding.rs`) was tried and reverted: it produced no numeric change
for `EarthICRF` and corrupted GMAT's own hyperbolic-orbit consistency check for the
already-working `EarthMJ2000Eq` fixtures. See
`crates/av-kernel/tests/demo_two_instance.rs`'s own doc comment (right above
`icrf_system_definition`) for the full write-up. Realizing this needs GMAT's
`CoordinateConverter::Convert` through a *new* `gmat-sys` shim function -- real, bounded work,
but a capability addition to `crates/gmat-sys`, not a frame-metadata change, and outside "no
proto, no ADR change" M18.1. The golden is committed, ready for whoever picks this up.

### A companion Python-side gap, not touched (owned by the concurrent altavista/web worker)

`tests/test_cdm_run.py::test_run_frames_populate_the_scene_frame_list_through_frame_registry`
(line ~469) asserts `len(frames) == 1` against the golden DRM's own `RunProducts.frames` --
this will need updating to 3 (`EarthBodyFixed`, `EarthICRF`, `EarthMJ2000Eq`) to match the
mandatory-frame augmentation above, exactly like the two Rust golden tests were. Not edited
here: this task's own environment rules exclude `altavista/`/`web/` and running `pytest`, and
`tests/test_*.py` is explicitly owned by the concurrent Python-side worker this round (see the
section just above).

## The port traffic sidecar, and replaying a DRM (M25.4a/M25.4b, `docs/open-questions.md`
## question 175)

Every FRAMED/BYTE_STREAM frame `crate::router::Router` ever carries during a run -- who sent
it, on which port, in which direction relative to that instance, and the raw bytes -- can be
recorded as a sidecar beside a run's `RunProducts`: pass `RunConfig.products_dir: Some(dir)` and
`execute()` writes `dir/port_traffic.pb` (an `altavista.v1.PortTrafficLog`, sorted `(tai_ns,
sequence, instance, port)` -- epoch first, `docs/open-questions.md` question 181, because a
declared command dispatch is carried before the run's first output tick and so has `sequence =
0` with a mid-run epoch), sets `RunProducts.port_traffic_hash` to its SHA-256, and records the file's
own location in `RunProducts.provenance.attributes["port_traffic_uri"]`. `products_dir: None`
(every fixture/test in this crate that does not care) writes nothing and leaves
`port_traffic_hash` empty, with `provenance.attributes["port_traffic"] = "not recorded"`
instead -- absence is always explicit, never silently inferred from an empty hash.

**Replaying a DRM** means playing one or more instances' own recorded OUT frames back through a
`crate::drm::replay::ReplayModel`, instead of running whatever process (native or a real bound
container) actually produced them -- set `RunConfig.replay: Some(ReplayConfig { log_path,
expected_hash, instances })`:

- `log_path` -- the recorded `port_traffic.pb` from the ORIGINAL run.
- `expected_hash` -- that original run's own `RunProducts.port_traffic_hash`. Verified against
  `log_path`'s exact bytes before anything else in `execute()` happens (before binding, before
  any GMAT call, before any step) -- a byte mismatch or an unparseable file is a typed refusal
  (`DrmError::ReplayLogHashMismatch`/`ReplayLogIo`), never a warning.
- `instances` -- which instances to replay. Empty means every `BINDING_KIND_CONTAINER`
  instance; naming one or more instances explicitly replays exactly those, of ANY binding kind
  -- naming a `BINDING_KIND_MODEL` instance is what makes a Docker-free (or GMAT-free,
  Renode-free, board-free) replay test possible at all. An instance name absent from the loaded
  `SosConfiguration` is a typed load refusal (`DrmError::UnknownReplayInstance`).

**What is, and is not, replayed.** Only OUT frames (a replayed instance's own emissions) are
ever played back -- an IN record is the receiver's own view of the identical frame some OTHER
instance sent, and replaying it too would double every frame the instance ever received. A
model's own physical state (`StepResult.state`, for any instance that has one), any named
`StepResult.outputs`, and any CDM `Measurement` are never reconstructed -- these are
model-internal computations that were never serialized onto any port, so a port-traffic-only
replay has no honest way to reproduce them (`crate::drm::replay`'s own module doc comment has
the full account, including why `drms/demo_attitude_control.*.yaml`'s own `controller` instance
is a real, worked example of exactly this limitation: its own `pointing_error_rad`/`seq`
outputs and its own self-reported `AppliedCommand` on `wheel_torque_out` cannot survive a
generic replay, so `crates/av-kernel/tests/replay.rs`'s own acceptance test replays
`drms/demo_attitude_sensors.*.yaml`'s `startracker` instead, where no such side channel exists).
A `BINDING_KIND_CONTAINER` instance's own `dynamics_hash`/`binding_hash` likewise cannot survive
a Docker-free replay (they come only from a live `Bind` response) -- `crates/av-kernel/tests/
drm_attitude_control_cfs.rs`'s own replay test discloses and excludes exactly those two fields
when it compares a real cFS-container run against its own Docker-free replay.

**The missing-frame rule.** A step whose own emission epoch has no recorded frame is a typed
error (`crate::drm::replay::ReplayError::MissingFrame`, surfaced as `DrmError::Schedule`)
exactly when that epoch falls strictly between the replayed instance's own first and last
recorded epoch; before the first or after the last is legitimate silence. This detects a
deleted or corrupted INTERIOR record and cannot detect one deleted from the leading or trailing
edge -- disclosed, not claimed away, in the module's own doc comment.

Reading the sidecar directly (e.g. to build a `ReplayConfig` or to inspect what a run actually
carried): `av_cdm::pb::PortTrafficLog::decode(&bytes)` (`prost::Message`), the same wire type
`execute()` itself writes.
