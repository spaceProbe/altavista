# gmat-sys

FFI to GMAT's C++ core: drive `ODEModel::GetDerivatives` from Rust so the kernel's own
integrator steps the state while GMAT supplies the validated physics. This is
[ADR-002](../../docs/adr/002-dynamics-contract.md)'s depth 2, and the P0 spike that proved it.

## What it does

- `Gmat::setup(startup_file)` loads GMAT once per process (the engine is a singleton).
- `gmat.construct(type, name)` and `Object::set_*` mirror the Python API's `Construct` / `SetField`.
- `gmat.derivative_model(&force_model, &spacecraft)` binds an `ODEModel` to a spacecraft
  through a `PropagationStateManager` and returns `f(state, dt) -> state_dot`.
- `gmat.derivative_model_with_stm(&force_model, &spacecraft)` does the same but also requests
  the spacecraft's orbit State Transition Matrix (`PropagationStateManager::SetProperty("STM",
  spacecraft)`, per GMAT_API_Cookbook's "STM and Covariance Propagation" chapter), returning a
  42-state model: index 0..6 Cartesian, index `6 + row*6 + col` is STM element `(row, col)`
  (row-major — verified element-for-element against GMAT's own `Spacecraft::GetRmatrixParameter
  ("STM")`). `GetDerivatives` fills `d(Phi)/dt = A(t) Phi` in that block; the 6-state block is
  bit-identical to the plain 6-state model (requesting the STM changes nothing else).
- `integrate::Dopri5` is a Dormand-Prince 5(4) adaptive integrator over any such function
  (state-dimension-agnostic: it integrates the 42-state exactly as it does the 6-state).
- `model::GmatModel` implements `av_dynamics::DynamicsModel` over the plain 6-state
  `DerivativeModel` (M2.1): the km<->m and A1MJD<->TAI boundary, so `av-kernel` and anything
  else above it works in SI metres and TAI nanoseconds without ever touching a kilometre or
  an A1MJD itself. See "The dynamics contract binding" below.
- `DerivativeModel::sync_spacecraft_cartesian_km`/`real_parameter` (M11.1, question 99): write
  a propagated state back into the bound spacecraft, then read one of GMAT's own named real
  parameters off it (`GmatBase::GetRealParameter`, through the shim's `gmatffi_get_real_parameter`)
  -- see "Named `step` outputs" below.

Units at this boundary are GMAT's (kilometres, km/s, A.1 MJD); the CDM adapter converts to
SI metres and TAI.

## `av-dynamics` (M2.1): where the integrator lives now

`integrate::Dopri5` (and its `Stats`) moved to `crates/av-dynamics/src/integrate.rs`
unchanged -- not one line of the algorithm differs -- because ADR-002 calls it "one
[integrator] family ... shared by every domain and binding," not a GMAT-specific detail.
`gmat-sys` depends on `av-dynamics` and re-exports the module (`pub use
av_dynamics::integrate;` in `src/lib.rs`), so `gmat_sys::integrate::Dopri5` still resolves
and every existing caller (this crate's own tests included) needed no changes.

**Why a dependency, not the reverse, and not just a re-export of gmat-sys's types from
av-dynamics:** `av-dynamics` must build with no GMAT dependency at all (a native or
non-space model needs the same integrator and the same `DynamicsModel` trait without ever
linking GMAT); `gmat-sys` is the one that needs `av-dynamics`'s pieces (`Dopri5` to keep
driving `DerivativeModel`, and the `DynamicsModel` trait for `model::GmatModel`). A
dependency edge from `av-dynamics` toward `gmat-sys` would contradict the "no GMAT
dependency" requirement, so the edge only makes sense the way it's built: `gmat-sys ->
av-dynamics`.

**Verified bit-identical.** `cargo test -p gmat-sys --test leo_golden -- --nocapture` after
the move reports the exact same numbers as before it: `8947 steps, 139 rejected, 63602
GetDerivatives calls`, `|dr| = 0.0057 m`, `|dv| = 6.283e-6 m/s` -- matching ADR-002's
amendment table (`5.7 mm`, `6.3e-6 m/s`) to the last displayed digit.

## M14.3 (question 111/112): no change needed here

`docs/open-questions.md` questions 111 (the covariance-unavailable NaN sentinel) and 112
(`ErasedModel`/`AnyModel` delegation) were both closed in `av-dynamics`/`av-kernel` -- see those
crates' own READMEs. Neither required a change to this crate: `GmatModel` never produced or read
a covariance-unavailable sentinel (that representation lives entirely in `av-kernel::kernel`,
above this crate), and `GmatModel` already implements `state_dim`/`derivatives`/`describe`/
`stm_capable`/`stm_derivatives`/`step`/`step_with_stm` explicitly (the "Named `step` outputs" and
"Outputs on the STM path" sections below), relying on the trait's own default only for
`integrator` and `step_with_ports` -- both re-audited by `av-dynamics`'s question 112 write-up
as "genuinely, functionally complete" defaults, not empty/no-op ones, so nothing here was ever
the recurring defect question 112 names. Disclosed here rather than left silent, per this task's
"update the three READMEs" instruction.

## The dynamics contract binding (`model::GmatModel`, M2.1; STM capability, M3.2)

`GmatModel` wraps a `DerivativeModel` -- either the plain 6-state one (`Gmat::derivative_model`)
or the 42-state Cartesian+STM one (`Gmat::derivative_model_with_stm`) -- and implements
`av_dynamics::DynamicsModel`, so `av-kernel` (or any other caller of the shared contract)
can drive it through `derivatives`/`step` without ever seeing a kilometre or an A1MJD.
`DynamicsModel::state_dim()` is always 6 either way -- the STM, when present, is a *declared
capability* (`stm_capable()`) reached through `stm_derivatives`, not a change to the model's
own physical state space:

- `derivatives` converts the incoming SI state to km, calls `DerivativeModel::derivatives`
  with `dt_seconds` measured from the bound model's own epoch (`epoch_a1mjd`, converted to
  TAI nanoseconds via `av_cdm::time::Tai::from_a1_mjd`), and converts the km/km-s result back
  to SI (m, m/s) with `av_cdm::units`. When the bound model is the 42-state one, this pads the
  caller's 6-state input with an identity STM block before calling through and keeps only the
  physical 6-state block of the result -- exact, since GMAT's `GetDerivatives` was measured
  bit-identical in that block whether or not the STM was requested.
- `describe()` returns an `altavista.v1.ModelInfo` with `depth = "gmat-ffi"` and
  `capabilities = [DERIVATIVES, STEP, DETERMINISTIC]`, plus `STM` when the bound model came
  from `derivative_model_with_stm`.
- `GmatModel::new` accepts either a 6- or 42-dimension `DerivativeModel` (panics on any other
  dimension) and now stops panicking on the 42-state case (M2.1's skeleton refused it).
- `stm_derivatives` (M3.2, `av_dynamics::DynamicsModel::stm_derivatives`): fills `state_dot`
  for the STM-augmented 42-state (index 0..6 physical, index `6 + row*6 + col` is `d(Phi)/dt`
  element `(row, col)`, row-major). **No unit conversion on the STM block at all** -- position
  and velocity both scale km<->m by the identical factor 1000, so for a state perturbation
  scaled uniformly across all 6 components, `Phi` (and `A(t) = d(state_dot)/d(state)`,
  and therefore `d(Phi)/dt`) is numerically identical in the km and SI bases (`Phi_SI = (1000
  I) Phi_km (1000 I)^-1 = Phi_km` since the scaling is a scalar multiple of the identity). This
  is a mathematical fact about this specific, uniform rescaling, not an assumption. Panics if
  called on a model built from `derivative_model` (no STM) -- callers must check
  `stm_capable()` first, per `av_dynamics::DynamicsModel`'s declared-capability contract.
- `GmatModel` is `!Send`/`!Sync` automatically (it holds a `DerivativeModel`, which holds a
  raw pointer), and the caller is still responsible for `crate::engine_lock()` around any
  sequence of calls that must not interleave with another thread's GMAT access -- documented
  in the module's doc comment, not enforced further.

Exercised end-to-end (not merely unit-tested) by `crates/av-kernel/tests/golden_acceptance.rs`
and, at this crate's own level, `crates/gmat-sys/tests/model_stm.rs` -- see that test and
`crates/av-kernel/README.md` for the measured results (kernel STM/covariance vs the golden,
`Phi(t0,t0)=I`, `det(Phi)`).

**Design constraint (M3.2, explicit in the task brief): the kernel does not read GMAT's own
`Spacecraft` STM after propagation as its mechanism.** `stm_derivatives` always drives
`GetDerivatives` and lets the caller's own integrator (`av-dynamics`'s `Dopri5`, via
`DynamicsModel::step_with_stm`) accumulate `Phi` -- exactly like the plain 6-state
`derivatives` path already does for the state itself. Reading GMAT's STM back
(`Spacecraft::GetRmatrixParameter("STM")`, or the raw propagator's own 42-state after
stepping) is the *reference* the kernel's own integration is measured against
(`goldens/gen_leo_1day.py`'s `"stm"` block, `tests/fixtures/stm_golden.json`), not a path this
crate's `DynamicsModel` binding takes.

## Named `step` outputs (`model::OUTPUT_RMAG`/`OUTPUT_CD`, M11.1, question 99)

`GmatModel::step` overrides `DynamicsModel::step`'s default (empty-`outputs`) implementation:
same `Dopri5` integration as the default, but the returned `StepResult.outputs` carries a
**declared, finite set** -- `model::OUTPUT_RMAG` ("rmag", SI metres) and `model::OUTPUT_CD`
("cd", dimensionless) -- both read from **GMAT's own parameter subsystem**, not computed in
Rust.

**How (question 99 closes the M10.2 shim gap).** Through M10.2, `rmag` was computed in Rust as
`sqrt(x^2 + y^2 + z^2)` on the propagated state, because the shim exposed no
`GmatBase::GetRealParameter`-style getter and `DerivativeModel` retained no handle to the bound
`Spacecraft` at all -- escalated then, since the shim was out of that task's ownership. Both
gaps are closed now:

- The shim's internal `Model` struct (`shim/gmatffi.cpp`) gained a `GmatBase *spacecraft`
  member, set from `gmatffi_model_new`/`gmatffi_model_new_stm`'s own `spacecraft` argument, and
  a new accessor `gmatffi_model_spacecraft(model)` returns it; `Gmat::derivative_model`/
  `derivative_model_with_stm` store it in `DerivativeModel` (never freed from Rust, exactly
  like `Object::ptr` -- GMAT's configuration manager owns it).
- A new shim function, `gmatffi_get_real_parameter(gmatffi_object obj, const char *name, double
  *out)`, wraps `GmatBase::GetRealParameter(const std::string&)` in the same `guarded()` template
  every other shim function uses: a C++ exception never crosses the FFI boundary, the return is
  the usual status int, and the message is `gmatffi_last_error()`. An **unknown parameter name is
  a typed error, not a garbage or silent-zero value** -- `GmatBase::GetParameterID` (which
  `GetRealParameter(label)` calls internally) throws `GmatBaseException("... has no parameter
  defined with ...")` for a label it does not recognize
  (`third_party/gmat-src/src/base/foundation/GmatBase.cpp`), and `guarded()` catches
  `BaseException` (GMAT's own exception base) before `std::exception`/`...` -- the same order
  every existing shim function already uses -- so this is `Err(GmatError)` in Rust, not a `0.0`.
- `DerivativeModel::sync_spacecraft_cartesian_km([x,y,z,vx,vy,vz] as km/km-s)` writes the
  propagated state into the bound spacecraft's `X`/`Y`/`Z`/`VX`/`VY`/`VZ` fields through the
  *existing* `gmatffi_set_field_real` (no new shim function needed for this half) -- the same
  `GmatBase::SetField`/`Spacecraft::SetElement` path `Object::set_real` already uses to seed a
  spacecraft's initial state (e.g. `av-kernel`'s `binding::materialize_gmat`). This step is
  necessary because this crate's own `av_dynamics::integrate::Dopri5` drives `GetDerivatives`
  directly and never calls GMAT's `PropagationStateManager::MapVectorToObjects` (the step GMAT's
  own `Propagator` would take) -- without it, the bound `Spacecraft`'s fields would stay at their
  construction-time values for the whole run, and `GetRealParameter("RMAG")` would report the
  *initial* position forever, not the propagated one.
- `DerivativeModel::real_parameter(name)` then calls `gmatffi_get_real_parameter` on the stored
  spacecraft handle.

`GmatModel::step` chains these three calls after each integration: convert `x1` to km
(`av_cdm::units::state_m_to_km`), `sync_spacecraft_cartesian_km`, then `real_parameter("RMAG")`
(converted back to SI metres, `units::km_to_m`) and `real_parameter("Cd")` (dimensionless,
crosses unconverted).

**Why `Cd` too, not only `rmag`.** `rmag` alone would not prove the call reaches GMAT rather than
reproducing Rust arithmetic that happens to agree -- `RMAG` is, after all, defined as the exact
formula `rmag` used to compute directly. `Cd` is a spacecraft property this crate never computes
or stores in Rust anywhere; it is set once, in C++, by whatever built the bound `Spacecraft`
(from a `SystemDefinition`'s `spacecraft.Cd` parameter), and does not depend on the propagated
state at all. `crates/av-kernel/tests/drm_executor.rs::
drm_rmag_output_matches_a_genuine_gmat_reportfile` reads `output.leo_rmag.cd@end` back and checks
it against the golden's own declared `2.2` -- a value only a genuine `GetRealParameter("Cd")`
call against the real bound `Spacecraft` object could reproduce.

**Measured agreement (re-measured at M11.1, real GMAT read, not asserted unchanged).**
`goldens/gen_leo_1day_rmag.py`'s `ReportFile` value is unchanged (it was already GMAT's own
answer, from an independent script + `ReportFile` code path). `av-kernel`'s DRM executor, driving
a `GmatModel` through the real `execute()` path over the identical golden arc, now resolves
`output.leo_rmag.rmag@end` = `6870294.675577` m against that `ReportFile`'s `6870294.675573` m --
**|err| = 0.000004 m (4 µm), the same measured agreement as the M10.2 Rust-derived value,
to the last displayed digit.** This is expected, not a coincidence to be suspicious of: the
propagated position `GmatModel::step` writes into the spacecraft is the identical
double-precision three-vector the old Rust computation used, `Spacecraft::SetElement`'s
Cartesian-label write-back is an exact assignment (no representation round-trip for `X`/`Y`/`Z`
themselves), and `Spacecraft::GetElement("RMAG")` computes the identical `sqrt(x^2+y^2+z^2)`
GMAT's own `RMAG` is defined as -- so the read-through-GMAT and computed-in-Rust values are
bit-identical for this arc, and the 4 µm residual is unchanged because it was never coming from
the (now-deleted) Rust `sqrt` call in the first place; it is the genuine disagreement between
this crate's own `GetDerivatives`-driven integration and the script engine's own `Propagate`
statement over the same golden arc. See `crates/av-kernel/README.md`'s "Outputs" section and
`crates/av-kernel/tests/drm_executor.rs::drm_rmag_output_matches_a_genuine_gmat_reportfile` for
the full comparison.

**`speed` is not one of these.** `av-kernel`'s own `output.<instance>.speed@time` is a
*trajectory-derived* quantity (`crate::expr::speed_output`, computed from `Trajectory.samples`'
own recorded velocity, never from `StepResult.outputs`) -- deliberately kept a separate
mechanism so a consumer can always tell a genuine, declared model output (this section) from a
value re-derived after the fact from the propagated state.

**Reaching this override needs a `step`-delegating erasure, and it now has one.**
`av_dynamics::erase::ErasedModel` delegates `step` and `step_with_stm` to the wrapped model
(M10.3), and `av_dynamics::StmAugmented::step` delegates to the wrapped model's
`step_with_stm` and composes `Phi(t0, t+dt) = Phi(t, t+dt) . Phi(t0, t)` (M11.2). Before those
two fixes neither override was reachable through erasure, and `av-kernel`'s DRM executor
carried a local `StepDelegating` workaround; **that workaround is deleted** and the executor
constructs every model through `av_kernel::registry::ModelRegistry`.

## Outputs on the STM path (M11.2, question 101)

`GmatModel::step_with_stm` reports the **same** named outputs as `GmatModel::step`, read the
same way from GMAT after the 42-state integration completes. The write-back happens strictly
*after* `integrate()` returns, and nothing in the STM integration path reads the spacecraft's
Cartesian fields, so it cannot perturb a finished `Phi`. A covariance run therefore produces
the same product set as a plain run -- `av-kernel`'s
`covariance_and_plain_paths_produce_the_same_named_output_set` pins that.

## Epoch write-back (M12.2, question 105)

`DerivativeModel::sync_spacecraft_epoch_a1mjd(a1mjd)` forces `DateFormat = "A1ModJulian"` and
sets `Epoch` as a string (GMAT refuses a numeric `SetField` on `Epoch`); Rust's `f64` formatting
round-trips losslessly, so the only precision cost is the one already inherent in
`Tai::to_a1_mjd` (up to 252 ns, question 81). `GmatModel::step`/`step_with_stm` call it
**before** `sync_spacecraft_cartesian_km`.

`Object::real_parameter` exposes `GetRealParameter` on any `Object`, not only a
`DerivativeModel`'s bound spacecraft.

**The `RecomputeStateAtEpoch` gotcha.** Setting `Epoch` on a spacecraft whose
`CoordinateSystem` differs from GMAT's internal one *rewrites* its Cartesian state to hold the
display-frame value constant across the epoch change. It is a no-op when the two match, which
is the case for every spacecraft this crate binds -- but it is why epoch is written first.

Reading a genuinely epoch-dependent parameter through this shim needs a **mirror spacecraft**
(`CoordinateSystem = EarthFixed`, state written through the hidden `CartesianX..CartesianVZ`
fields), because the `Parameter` subsystem (`Sat.Earth.Longitude`) needs
`Moderator::CreateParameter` wiring the shim does not expose. `tests/epoch_writeback.rs` and
`goldens/gen_leo_1day_planetodetic_lon.py` do exactly that.

## `Gmat::convert_with_rotation` (M21.4, question 138): rotating a covariance into a declared frame

`Gmat::convert` (M19.1, ADR-002's fourth amendment) rotates a 6-state Cartesian sample between
two registered `CoordinateSystem`s through `CoordinateConverter::Convert`. `Gmat::convert_with_
rotation` is its sibling: the identical `Convert` call, additionally returning the row-major 3x3
`rotation`/`rotation_dot` (`CoordinateConverter::GetLastRotationMatrix`/`GetLastRotationDot
Matrix`, read back from the *same* call -- `shim/gmatffi.h`'s own doc comment on `gmatffi_
convert_state_and_rotation` has the full account). A caller rotating a covariance needs the 6x6
Jacobian `[[R,0],[Rdot,R]]`, not just `R`: the rotation between two frames is generally
time-varying (a body-fixed frame rotates with its body), so the velocity block picks up an
`Rdot` coupling term the position block does not need. `crates/av-kernel`'s `drm::executor::
rotate_covariance` is the one caller in this repository (see that crate's own README for the
full account, including why GMAT's own `OrbitErrorCovariance` `ReportFile` -- which uses only
the block-diagonal `[[R,0],[0,R]]` form -- is not a usable reference for this).

**Measured** (`cargo test -p gmat-sys --test convert_rotation -- --nocapture
measure_the_per_call_cost_of_convert_with_rotation_vs_plain_convert`, debug build, Apple
Silicon, warmed-up loop of 2000 calls each -- mirrors M19.1's own `convert` measurement style):

| | `convert` | `convert_with_rotation` | added cost |
|---|---|---|---|
| To `EarthBodyFixed` | 0.670 µs/call | 0.854 µs/call | +0.184 µs (+27.5%) |
| To `EarthICRF` | 5.533 µs/call | 5.764 µs/call | +0.231 µs (+4.2%) |

Per-trajectory cost this adds to emitting a covariance trajectory: `executor::rotate_covariance`
is called once per sample whose `cov` is non-empty (never for a sample with no covariance,
which still uses plain `convert`), so the added cost scales linearly with the number of
covariance-bearing samples -- e.g. +0.18 ms total over 1,000 such samples to `EarthBodyFixed`,
+0.23 ms to `EarthICRF` (same measured per-call deltas above, multiplied out).

## Build requirements

| Input | Default | Override |
|---|---|---|
| GMAT install (binaries, data) | `<repo>/GMAT R2026a` | `GMAT_ROOT` |
| GMAT headers (`src/base`, `src/gmatutil`) | `<repo>/third_party/gmat-src` via `third_party/fetch-gmat-src.sh` | `GMAT_SRC` |
| CSPICE headers (`SpiceUsr.h`) | `<repo>/third_party/cspice/include` via `third_party/fetch-cspice.sh` | `CSPICE_INCLUDE` |
| `libGmatBase.R2026a.dylib`, `libGmatUtil.R2026a.dylib` | the app bundle's `Contents/Frameworks` | `GMAT_LIB` |

The shim is compiled with `-D__USE_SPICE__` because the shipped binaries were built with it
and the define changes class layouts. macOS only for now (the dylib names and rpath handling
are Darwin's); Linux needs the `.so` names and `-rpath` equivalents in `build.rs`.

```bash
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cargo test -p gmat-sys -- --nocapture
cargo run --release -p gmat-sys --example bench
```

## Measured (2026-09-02, Apple Silicon, GMAT R2026a)

Golden arc `goldens/leo_1day_jgm2_8x8_sunmoon.json` (LEO, JGM2 8x8, Sun and Moon point
masses, one day, GMAT PrinceDormand78 at 1e-13 as the reference):

| | Result |
|---|---|
| Our DP5(4) at rtol = atol = 1e-12 over GMAT derivatives, from Rust | 8,947 steps, 139 rejected, 63,602 calls |
| Position / velocity error vs GMAT's propagator after 86,400 s | 5.7 mm / 6.3e-6 m/s |
| `GetDerivatives` cost through the shim | 3.2 µs per call (debug), 2.57 µs per call = 390 kHz (release) |
| Model build (construct, initialize, bind) | 29.6 ms |
| Same arc through the Python API (`gmatpy`) | 10.4 µs per call, identical error |
| Two `ODEModel`s in one process, alternating calls | derivatives unchanged; third-body `dt` honoured |

ADR-002's amendment records what these numbers change in the plan.

## STM / covariance path (2026-09-02, M1.4 spike)

`derivative_model_with_stm` drives GMAT's 42-state (Cartesian + STM) path from Rust; see
[`docs/teamlog/adr-002-amendment-draft-stm.md`](../../docs/teamlog/adr-002-amendment-draft-stm.md)
for the full evidence. Same golden arc, GMAT's own 42-state Python-API run as the reference:

| | Result |
|---|---|
| State dimension after `PropagationStateManager::SetProperty("STM", sc)` | 42 (was 6) |
| `Phi(t0,t0)` | exactly the 6x6 identity |
| Requesting the STM changes the 6-state derivatives | no — bit-identical (< 1e-15) to the plain model |
| Our DP5(4) 42-state vs GMAT's own 42-state after 86,400 s (position / velocity) | 2.7 mm / 3.0e-6 m/s |
| STM (36 elements) vs GMAT's own final STM | max abs error 6.3e-5, max relative error 5.5e-7 |
| `det(Phi)` at t1 (Liouville / symplecticity check) | ours 1.000000000010, GMAT's 1.000000000091 |
| `GetDerivatives` cost, 42-state, release build | 3.4 µs/call (vs 2.57 µs/call for the 6-state model) |

A-matrix coverage for this force model: both `PointMassForce` and `GravityField` fill their
STM/A-matrix block in `ODEModel::CompleteDerivativeCalculations`; `GravityField`'s `StmLimit`
defaults to 100 (above the golden's degree/order 8), so the gravity-gradient contribution to
the A-matrix is the full field, not a truncated one, for this arc.

## `GmatModel`/`StmAugmented` through the full `DynamicsModel` contract (M3.2)

`crates/gmat-sys/tests/model_stm.rs` repeats the same golden-arc comparison as the M1.4 spike
above, but through `GmatModel`/`av_dynamics::StmAugmented`/`step_with_stm` (SI units, TAI
nanoseconds) instead of the raw `DerivativeModel` -- proving the higher-level wiring, not just
the FFI, reproduces the same numbers:

| | Result |
|---|---|
| Position / velocity error vs golden after 86,400 s | 2.8 mm / 3.05e-6 m/s |
| STM (36 elements) vs golden | max abs error 6.37e-5 |
| `Phi(t0,t0)` (via a zero-duration `step_with_stm`) | exactly the 6x6 identity |
| `det(Phi)` at t1 | ours 1.000000000163, golden's 1.000000000091 |
| `propagate_covariance` on a diagonal SI P0 ((100 m)^2 / (0.1 m/s)^2) | symmetric after correction (asymmetry ~1.9e-9), positive diagonal |

## Limits found

- One GMAT configuration per process, and no thread safety: every handle is `!Send`.
- Objects created with `Construct` are owned by GMAT's configuration and never freed here.
- The state dimension follows the propagation state manager (6 for one spacecraft; 42 with
  the STM via `derivative_model_with_stm`, above).

## `Object::set_reference` and drag/SRP end to end (M5.1)

**The shim gap is closed.** `Object::set_reference` (`gmatffi_set_reference`,
`shim/gmatffi.{h,cpp}`) is a thin wrapper over `GmatBase::SetReference(ref, -1)` -- the same
method the Python API's `obj.SetReference(ref)` calls (`GmatBase.cpp`: `SetRefObject(obj,
obj->GetType(), obj->GetName())`, i.e. the type/name GMAT routes the reference by comes from
the reference object's own `GetType()`/`GetName()`, not a type this shim chooses). This is what
`tests/drag_srp_stm.rs`'s M4.3-era `dragforce_without_atmosphere_reference_confirms_the_shim_gap`
test used to document as missing; that test still exists (renamed
`dragforce_without_a_referenced_atmosphere_still_fails_to_initialize`) but now proves a
different, still-true fact -- `DragForce::Initialize()` genuinely requires the reference, not
that this crate has no way to provide it.

`Object::set_ballistics(&SpacecraftBallistics)` sets `DryMass`/`Cd`/`Cr`/`DragArea`/`SRPArea`
in one call -- `docs/open-questions.md` question 81's rule ("a seed is a vehicle, not a state
vector") made mechanical: a `Spacecraft` seeded without these five fields flies with GMAT's
defaults (`DEFAULT_DRY_MASS_KG` 850 kg, `DEFAULT_DRAG_AREA_M2` 15 m², `DEFAULT_SRP_AREA_M2` 1
m²) instead of the vehicle's real ones, which `goldens/leo_1day_jgm2_8x8_sunmoon_drag_srp.json`'s
own `"reason"` field measured at 467,025.793 m of one-day position error.

`tests/drag_srp_stm.rs` drives that golden end to end through this crate's own Dopri5 over
`GetDerivatives`, both at 6-state and (STM-requested) 42-state, comparing state, STM and a
covariance propagated through the STM this crate's own integration produced (never GMAT's) --
not merely re-running the M4.3 finite-difference check. Measured:

| | 6-state | 42-state |
|---|---|---|
| Position error vs golden after 86,400 s | 0.0070 m | 0.0030 m |
| Velocity error vs golden | 8.22e-6 m/s | 3.57e-6 m/s |
| STM (36 elements) vs golden's `stm.final_stm` | -- | max abs error 9.91e-5, max relative error (scale >= 1) 8.15e-9 |
| `det(Phi)` at t1 | -- | ours 0.997654, golden's 0.997654 (dissipative arc -- not a symplecticity check) |
| `propagate_covariance(phi, p0_si, 6)` vs golden's `stm.cov_t1_si` | -- | max abs error 0.857, relative to golden's own norm 6.16e-10 |

Both position/velocity comparisons stay inside the golden's own declared `tolerance_m` /
`tolerance_mps` (0.05 m, 5e-5 m/s), unchanged from the drag-free arc. The declared STM bound
(`STM_MAX_ABS_ERR_BOUND = 1.0` in the test) reuses the plain arc's own absolute bound rather
than a drag-specific one, for a reason worth stating plainly: `DragForce`'s A-matrix is filled
by a first-order **forward** finite difference internal to GMAT itself
(`third_party/gmat-src/src/base/forcemodel/DragForce.cpp`: `pert = 1.0e-2` km position,
`1.0e-6` km/s velocity, `useCentralDifferences = false`), and that exact code runs inside
*both* GMAT's own PrinceDormand78 propagation (this golden's reference) and this crate's own
Dopri5 run -- fed the same state, both sides compute the same (equally approximate) A-matrix, so
the truncation error is not, by itself, a source of disagreement between the two. The
measurement confirms that reasoning (same order of agreement as the drag-free arc, not
measurably worse), rather than a hypothesis this README states without having checked it.

## Question 82: `RelativisticCorrection` withholds the STM capability (M5.1)

`GmatModelInfo.has_relativistic_correction` (caller-supplied, like `goldens` -- this crate
cannot introspect what forces a `ForceModel` was built with) and `GmatModel::new`'s new
`accept_missing_stm_terms: bool` parameter implement the ADR-002 third amendment's decision:
`RelativisticCorrection::GetDerivatives` fills its A-matrix/STM contribution with an
unconditional zero (`third_party/gmat-src/src/base/forcemodel/RelativisticCorrection.cpp`, both
the `fillSTM` and `fillAMatrix` branches -- a stub, not a physically-absent term), so a 42-state
model built from a force model that includes it reports `stm_capable() == false` (and
`describe()` omits `MODEL_CAPABILITY_STM`) even though `model.dimension() == 42`, **unless**
`accept_missing_stm_terms` is `true` -- the DRM's `DrmOptions.accept_missing_stm_terms`
acknowledgement (`proto/altavista/v1/system.proto`), read by whichever caller constructs the
`GmatModel` and passed straight through as a `bool` (no crate in this repo yet owns
constructing a `DesignReferenceMission` and reading that field off it directly).
`tests/model_stm.rs::relativistic_correction_withholds_the_stm_capability_unless_accepted`
builds a real force model with `RelativisticCorrection` added (confirming it constructs and
that GMAT genuinely fills its STM-derivative block with a real zero, not merely "small"), and
checks both the withheld and accepted cases against `stm_capable()`/`describe()` directly.

## Question 83: `nearest_spd_projection` (M5.1, at this crate's depth)

Not applicable to `gmat-sys` directly -- this crate never checks or repairs a covariance itself
(that happens one layer up, in `av-kernel::Kernel::run_with_covariance`, over the `Phi` this
crate's `DynamicsModel::stm_derivatives` produces). See `crates/av-kernel/README.md`.
