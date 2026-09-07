# Draft amendment to ADR-002: the STM spike (M1.4)

**Status:** draft notes for the manager/lead to fold into `docs/adr/002-dynamics-contract.md`
if they agree. Not itself an amendment — `docs/adr/002-dynamics-contract.md` is untouched by
this file.

**Question (from the 2026-09-02 amendment):** "The covariance path (`PropagateSTM`, 42-state)
is the next spike." Can GMAT's 42-state STM propagation path be driven through
`ODEModel::GetDerivatives` from Rust, so that covariance propagates alongside the state?

## Answer

**Yes.** `PropagationStateManager::SetProperty("STM", spacecraft)`, called before
`BuildState()`, grows the propagation state from 6 to 42 (6 Cartesian + 36 STM elements);
`ODEModel::GetDerivatives` fills `d(Phi)/dt = A(t)·Phi` in the STM block on every call; the
kernel's existing state-dimension-agnostic Dopri5 integrates the 42-state exactly as it does
the 6-state; and the result agrees with GMAT's own 42-state propagation (Python API, same
golden arc) to 2.7 mm / 3.0e-6 m/s in the Cartesian block and 6.3e-5 max absolute error in the
STM block. Deliverable A is landed in `crates/gmat-sys`.

## Evidence: the exact GMAT calls

Ground truth first (GMAT's own 42-state propagation), then the Rust-driven reproduction.

### 1. Ground truth — GMAT's own 42-state run, Python API

Source: `GMAT_API_Cookbook`'s "STM and Covariance Propagation" chapter
(`GMAT R2026a/docs/GMAT_API_Cookbook/_sources/source/stmpropagation.rst.txt`), which gives the
exact call sequence:

```python
psm = pdprop.GetPropStateManager()
psm.SetProperty("Covariance", sat)   # not used here — STM only
psm.SetProperty("STM", sat)
...
gmat.Initialize()
pdprop.PrepareInternals()
propagator = pdprop.GetPropagator()
propagator.UpdateSpaceObject()
...
stm = sat.GetRmatrixParameter("STM")
```

Reproduced (script: was written to the scratchpad, not committed — see "Files" below) against
the same spacecraft/force model as `goldens/leo_1day_jgm2_8x8_sunmoon.json` (LEO, SMA 6878 km,
JGM2 8x8, Luna + Sun point masses, epoch 01 Jan 2026 00:00:00 UTC, `PrinceDormand78` at
accuracy 1e-13, `MaxStep` 600 s, duration 86,400 s). Key finding: the original attempt stepped
the wrong object — `gator.Step(dt)` was called on the `Construct()`-time integrator handle,
not the one `PrepareInternals()` builds — and silently produced a frozen state (state1 ==
state0 to the last bit). `goldens/gen_leo_1day.py` avoids this by reassigning
`gator = prop.GetPropagator()` after `PrepareInternals()`; the ground-truth script had to do
the same. This is a real trap for anyone driving GMAT's propagation loop directly, STM or not.

Results:

| Call | Return / value |
|---|---|
| `psm.SetProperty("STM", sat)` | `True` |
| `len(propagator.GetState())` after | 42 (was 6) |
| Flat state indices 6..42, row-major (`state[6 + i*6 + j] == stm.GetElement(i,j)`) at t0 | matches `sat.GetRmatrixParameter("STM")` element-for-element |
| Flat state indices 6..42, **column**-major (`state[6 + j*6 + i] == stm.GetElement(i,j)`) at t0 | **also** matches at t0 (identity is ambiguous — see below) |
| `Phi(t0,t0)` | exact 6x6 identity, `max\|Phi - I\| = 0.0` |
| 6-state part of `state1` (after 86,400 s) | bit-identical (16 significant digits) to `goldens/leo_1day_jgm2_8x8_sunmoon.json`'s `final_state` |
| Flat state indices 6..42 at t1, row-major, vs `sat.GetRmatrixParameter("STM")` at t1 | matches element-for-element (`match_rowmajor_state1_vs_spacecraft = True`) |
| `det(Phi)` at t1 | `1.0000000000906126` |

The t0 identity check cannot distinguish row-major from column-major (a diagonal matrix looks
the same flattened either way — this is exactly the trap the task brief called out). The t1
check, after 86,400 s of a non-symmetric A-matrix, settles it unambiguously: **row-major**
(`element = row*6 + col`), matching the C++ source directly (see below).

### 2. C++ source confirms both the ordering and the fill mechanism

`third_party/gmat-src/src/base/propagator/PropagationStateManager.cpp`:

- `SetProperty(std::string propName, GmatBase *forObject)` (line 393) validates via
  `forObject->SetPropItem(propName)`; `Spacecraft::SetPropItem("STM")` (Spacecraft.cpp:8746)
  returns `Gmat::ORBIT_STATE_TRANSITION_MATRIX`.
- `Spacecraft::GetPropItemSize(Gmat::ORBIT_STATE_TRANSITION_MATRIX)` (Spacecraft.cpp:8828-8830)
  returns `fullSTMRowCount * fullSTMRowCount`; `fullSTMRowCount` defaults to 6
  (Spacecraft.cpp:542), giving 36.
- `PropagationStateManager::RequiresCompletion()` (PropagationStateManager.cpp:886-889) returns
  `hasPostSuperpositionMember`, set when the STM (or A-Matrix) element is present.

`third_party/gmat-src/src/base/forcemodel/ODEModel.cpp`:

- `GetDerivatives(state, dt, order, id)` (line 3107): `order == 2 && fillSTM` throws
  (`"Second order integrators cannot be used when propagating the ... STM"`, line 3169-3172) —
  irrelevant here since a first-order integrator (`order == 1`) is used throughout, as GMAT's
  own `PrinceDormand78` and our `Dopri5` both are.
- After the force-superposition loop, `if (psm->RequiresCompletion())
  CompleteDerivativeCalculations(state);` (line 3393-3394) — this is the gate that fills the
  STM derivative block; without a requested STM this branch never runs and nothing writes past
  index 6.
- `CompleteDerivativeCalculations` (line 3665-3733) builds `aTilde` (the A-matrix) from
  `deriv[i6..i6+36]` (the per-force contributions already summed there), forces the upper-right
  3x3 block to identity (`aTilde[3 + n*(stmRows+1)] = 1.0`, i.e. `d(vel)/d(vel)` coupling —
  `dPos/dt = Vel`), then computes `deriv[i6+j*stmRows+k] = sum_l aTilde[j*stmRows+l] *
  state[i6+l*stmRows+k]` — **row-major** (`element = j*stmRows+k`), confirming the ground
  truth above.

`third_party/gmat-src/src/base/forcemodel/GravityField.cpp` (line 622-676) and
`third_party/gmat-src/src/base/forcemodel/PointMassForce.cpp` (line 335, 567-596) both fill
`deriv[i6..]` with their own gravity-gradient / point-mass contribution to the A-matrix when
`fillSTM` is set — **both forces in this golden's force model contribute to the A-matrix**, not
just the point-mass term. `GravityField`'s A-matrix contribution is gated by `StmLimit`
(`HarmonicField.hpp`/`.cpp`, default `100`); the golden uses degree/order 8, so the A-matrix
gravity-gradient term is the *full* field for this arc, not truncated — `StmLimit` only matters
when it is set below the field's own degree/order, which does not happen here and was not
otherwise tested.

### 3. The Rust-driven reproduction

`crates/gmat-sys/shim/gmatffi.cpp` gained `gmatffi_model_new_stm`, sharing the existing
`gmatffi_model_new`'s setup via a `build_model(..., with_stm)` helper that inserts exactly one
extra call — `m->psm->SetProperty("STM", static_cast<GmatBase *>(spacecraft))` — between
`SetObject` and `BuildState()`. Everything downstream (`SetPropStateManager`, `SetState`,
`Initialize("")`, `BuildModelFromMap()`, `UpdateInitialData()`, the reported dimension) is
unchanged code, now operating on a 42-element `GmatState` because `BuildState()` sized it that
way. `crates/gmat-sys/src/lib.rs` exposes this as `Gmat::derivative_model_with_stm`, returning
the same `DerivativeModel` type used for the 6-state model (its `dimension` field is already
generic, so `state()` / `derivatives()` / `derivatives_into()` needed no changes).

`crates/gmat-sys/tests/stm_spike.rs` (new; takes `engine_lock()` first, per the existing
convention) does, in order:

1. Builds a 42-state model (`derivative_model_with_stm`) and a separate plain 6-state model
   (`derivative_model`) from independently-constructed but identically-configured
   spacecraft/force-model objects.
2. Asserts `model.dimension() == 42`.
3. Asserts the initial 42-state matches the ground-truth fixture to `< 1e-9` per element, and
   that `Phi(t0,t0)` is the exact identity (`assert_eq!`, not a tolerance — GMAT sets it
   exactly).
4. Calls `derivatives()` on both the 42-state and 6-state models at `dt = 0` and asserts the
   first 6 elements agree to `< 1e-15` (bit-level) — **requesting the STM does not change the
   6-state derivatives**.
5. Asserts the STM derivative block at t0 is not all-zero (i.e. `GetDerivatives` is actually
   filling `d(Phi)/dt`, not leaving `PrepareDerivativeArray()`'s zero-init in place).
6. Integrates the 42-state with the kernel's existing `Dopri5::default()` (rtol = atol =
   1e-12, same as the 6-state golden test) over the golden's 86,400 s.
7. Compares the resulting 6-state part against GMAT's own 42-state run and against the
   original 6-state golden's tolerance (0.05 m / 5e-5 m/s).
8. Compares the resulting 36 STM elements against GMAT's own final STM.
9. Recomputes `det(Phi)` at t1 from our own integrated STM via Gaussian elimination (a small
   local helper in the test, not GMAT code) and checks it is near 1 and near GMAT's own
   `det(Phi)` at t1.

## Measurements

Settings: golden arc `goldens/leo_1day_jgm2_8x8_sunmoon.json` (LEO, JGM2 8x8 + Luna + Sun,
86,400 s), GMAT R2026a `PrinceDormand78` at accuracy 1e-13 / MaxStep 600 s as the 42-state
reference (regenerated for this spike, not the shipped 6-state golden file, which is untouched
— see "Files" below), our `Dopri5` at rtol = atol = 1e-12, release build, macOS/Apple Silicon.

| Measurement | Result |
|---|---|
| State dimension after `SetProperty("STM", sc)` | 42 |
| `Phi(t0,t0)` | exact 6x6 identity |
| 6-state derivatives, STM requested vs not, at t0 | bit-identical (`< 1e-15`) |
| STM derivative block at t0 | non-zero (confirms `d(Phi)/dt = A·Phi` is filled) |
| Our DP5(4) 42-state integration | 10,587 accepted steps, 350 rejected, 76,559 `GetDerivatives` calls, 0.26 s (release) |
| Position / velocity error vs GMAT's own 42-state run, after 86,400 s | 2.7 mm / 3.0e-6 m/s |
| STM (36 elements) vs GMAT's own final STM | max abs error 6.3e-5, max relative error (scale ≥ 1) 5.5e-7 |
| `det(Phi)` at t1 | ours 1.000000000010, GMAT's 1.000000000091 |
| `GetDerivatives` per call, 42-state, release | 3.40 µs/call (measured 3× consecutively: 3.43, 3.43, 3.40 µs) |
| `GetDerivatives` per call, 6-state, release (ADR-002 amendment baseline) | 2.57 µs/call |
| 42-state overhead vs 6-state | ≈ 1.33x |

`cargo test --release -p gmat-sys --test stm_spike -- --nocapture` (repeated 3x for stability)
and `cargo test --workspace` (default parallel threads) both pass; see "Commands run" below.

## Traps named in the task brief — resolved

- **Element ordering (row- vs column-major).** Row-major (`6 + row*6 + col`), confirmed two
  independent ways: (1) the C++ source's `element = j*stmRows+k` in
  `ODEModel::CompleteDerivativeCalculations`, and (2) empirically, by comparing the flat state
  vector against `Spacecraft::GetRmatrixParameter("STM")` read element-by-element *after*
  propagation (the t0 identity is ambiguous between row- and column-major; t1 is not, and both
  readings only agree row-major).
- **Does `GetDerivatives` fill the STM derivative block, or leave it zero?** It fills it, but
  only when `PropagationStateManager::RequiresCompletion()` is true, which is only true once
  `SetProperty("STM", ...)` (or `"AMatrix"`) has been requested — confirmed both by reading
  `ODEModel::GetDerivatives`'s `if (psm->RequiresCompletion()) CompleteDerivativeCalculations
  (state);` gate and by the Rust test's direct check that the STM derivative block is non-zero
  once requested.
- **Does the A-matrix include all forces, or only the point-mass term?** For this golden's
  force model (`GravityField` + `PointMassForce`, no drag, no SRP), both contribute to the
  A-matrix (`GravityField.cpp:622-676`, `PointMassForce.cpp:335,567-596`). Not verified: whether
  `DragForce`, `SolarRadiationPressure`, or `RelativisticCorrection` also fill an A-matrix block
  — the golden's force model doesn't exercise them, and `DragForce.hpp`/`.cpp` and
  `SolarRadiationPressure.hpp`/`.cpp` both mention `STM`/`fillSTM` in their source (per an
  earlier grep) but this was not read in depth. Flagged as unverified below.
- **Does requesting the STM change the 6-state derivatives?** No — confirmed bit-identical
  (`< 1e-15`) between a 42-state and an independently-built 6-state model at the same state and
  epoch, both in the Rust test and (implicitly) in the ground-truth Python run, whose 6-state
  final values match the pre-existing 6-state golden file to 16 significant digits.

## Files created/changed (absolute paths)

- `/Users/probe/code/AltaVista/crates/gmat-sys/shim/gmatffi.h` — declared
  `gmatffi_model_new_stm`.
- `/Users/probe/code/AltaVista/crates/gmat-sys/shim/gmatffi.cpp` — refactored
  `gmatffi_model_new`'s body into a shared `build_model(..., with_stm)` helper; added
  `gmatffi_model_new_stm`.
- `/Users/probe/code/AltaVista/crates/gmat-sys/src/lib.rs` — added the
  `gmatffi_model_new_stm` FFI binding and `Gmat::derivative_model_with_stm`.
- `/Users/probe/code/AltaVista/crates/gmat-sys/tests/stm_spike.rs` — new test (above).
- `/Users/probe/code/AltaVista/crates/gmat-sys/tests/fixtures/stm_golden.json` — new fixture:
  GMAT's own 42-state at t0 and t1 for the golden arc, generated by the Python ground-truth
  script described above (script itself lives only in the scratchpad, per this task's file
  ownership — it never touches `goldens/`, which is out of scope for this crate).
- `/Users/probe/code/AltaVista/crates/gmat-sys/README.md` — documented
  `derivative_model_with_stm` and the measurements above.
- `/Users/probe/code/AltaVista/docs/teamlog/adr-002-amendment-draft-stm.md` — this file.
- `docs/adr/002-dynamics-contract.md` — **not touched**, per the task's file-ownership rule.

## Commands run (representative)

```
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd /Users/probe/code/AltaVista
cargo test --workspace
# ... gmat_sys: 0 unit tests; leo_golden.rs: 2 passed; stm_spike.rs: 1 passed; av_cdm: 24 passed

cargo test --release -p gmat-sys --test stm_spike -- --nocapture
# [gmat-sys STM spike] 42-state: 10587 steps, 350 rejected, 76559 GetDerivatives calls in 0.260 s (3.40 us/call)
# [gmat-sys STM spike] position error vs GMAT: 0.0027 m, velocity error: 3.034e-6 m/s
# [gmat-sys STM spike] STM max abs error vs GMAT: 6.328904e-5, max rel error (scale>=1): 5.525347e-7
# [gmat-sys STM spike] det(Phi) at t1: ours = 1.000000000010, GMAT's = 1.000000000091
# test rust_driven_42state_stm_matches_gmat_and_is_internally_consistent ... ok

.venv/bin/python -m pytest -q
# 19 passed
```

## Approximations and caveats (stated plainly)

- **Build verification used a temporary standalone copy of the crate.** While diagnosing and
  fixing the shim/Rust changes, `crates/av-cdm` (owned by another worker, mid-edit on this
  session's shared checkout) was between commits and had no `src/lib.rs`, so `cargo build -p
  gmat-sys` failed at the workspace-manifest stage (unrelated to this change).
  `crates/gmat-sys` was copied verbatim to a scratch directory with a self-contained
  `Cargo.toml` (concrete `version`/`edition`/etc. instead of `.workspace = true`, matching the
  real workspace's `[workspace.package]` values) and built/tested there against the real GMAT
  install via `GMAT_ROOT`/`GMAT_SRC`/`CSPICE_INCLUDE` env overrides, to get a green signal
  without touching files outside this task's ownership. Once `av-cdm` was finished by the other
  worker, `cargo test --workspace` was re-run from the real checkout and passes (see "Commands
  run"); the standalone copy was not otherwise used and is not part of the deliverable.
- **The STM reference (ground truth) comes from a script run once in this session, not a
  checked-in, reproducible golden.** Unlike `goldens/gen_leo_1day.py` (owned by another area of
  the repo, out of this task's file list), the STM ground-truth script lives only in the
  scratchpad and was not committed anywhere in the repository; `crates/gmat-sys/tests/fixtures/
  stm_golden.json` is the frozen output of that one run. If GMAT's STM behavior needs to be
  re-derived later (e.g. after a GMAT version bump), that script would need to be rewritten from
  the API Cookbook chapter cited above, not merely re-run.
- **`StmLimit` and non-gravity/point-mass forces are not exercised.** The golden's force model
  has no drag, SRP, or relativistic correction, so whether those forces' A-matrix contributions
  (if any) work correctly through this same path is unverified — see "Traps... resolved" above.
- **The det(Phi) check uses a hand-written 6x6 Gaussian-elimination determinant** in the test
  file (not a GMAT or third-party routine) — a small, auditable piece of arithmetic, but worth
  naming since it's the one piece of "verification code" not sourced from GMAT itself.
- **No tolerance was loosened to make anything pass.** The STM comparison assertion in
  `stm_spike.rs` (`max_abs_err < 1.0`) is deliberately loose relative to the measured 6.3e-5,
  because STM entries in this golden reach ~1.8e5 in magnitude (position sensitivity compounded
  over a full day of many-body dynamics) and a tighter fixed bound would be arbitrary; the
  actual measured error is recorded here and in the README rather than encoded as a tight
  pass/fail line.

## What could not be determined / next steps

- Whether `DragForce`, `SolarRadiationPressure`, and `RelativisticCorrection` correctly fill
  their STM/A-matrix contributions through this same `GetDerivatives` path (mentioned in their
  source but not exercised by this golden's force model).
- Whether `PropagationStateManager::SetProperty("Covariance", sc)` (the sibling call shown in
  the same Cookbook chapter, for propagating the covariance matrix itself rather than just the
  STM) composes cleanly with this same shim path — not attempted; the task scope was the STM
  only ("the covariance path (`PropagateSTM`, 42-state)").
- Behavior with more than one spacecraft in the same `ODEModel`, both requesting STM
  (`stmCount > 1` in the C++ source) — not attempted; the existing `two_models_in_one_process_
  do_not_interfere` pattern uses two separate `ODEModel`s, not one model with two STM-bearing
  spacecraft.
- A cross-check against the Python API's per-call cost for the 42-state path (the ADR-002
  amendment records 10.4 µs/call for the 6-state case) was not measured for the 42-state case.
