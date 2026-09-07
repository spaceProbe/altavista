# Draft amendment: does `GetDerivatives` fill drag and SRP's A-matrix contributions?

- **Status:** draft evidence for the lead to fold into `docs/adr/002-dynamics-contract.md` (or not). Not itself an ADR amendment.
- **Task:** M4.3, worker M.
- **Question (ADR-002's second amendment, left open):** *"whether `DragForce`, `SolarRadiationPressure` and `RelativisticCorrection` fill their A-matrix contributions -- this golden's force model does not exercise them."*

## Answer, in one sentence

**Yes** -- `GetDerivatives` fills a real, physically correct, nonzero A-matrix contribution for both `DragForce` (position *and* velocity blocks) and `SolarRadiationPressure` (position block only, correctly zero in the velocity block) -- established decisively by finite-difference (evidence 3) and confirmed by reading the source (evidence 2); a full-arc kernel-vs-GMAT STM comparison (evidence 1) was attempted but is reported with an unresolved anomaly, not as a clean third confirmation. `RelativisticCorrection`, not this task's focus but touched in passing, is a different story: **its A-matrix contribution is unconditionally zero** -- a stub, not a physically-absent term (see "Bonus finding" below).

## What this does and does not cover

- In scope, per the brief: `DragForce` and `SolarRadiationPressure`.
- `RelativisticCorrection` is mentioned because the source-reading pass (evidence 2) covered it too, cheaply, and the finding is directly relevant to the same open question -- reported below as a bonus, not exhaustively measured the way drag/SRP were.

## Evidence 1: measurement (kernel-integrated STM vs. GMAT's own STM)

**What this line of evidence proves, and what it does not:** if a kernel-driven integration over `GetDerivatives` reproduces GMAT's own propagated STM, that is consistent with (but does not, by itself, *prove*) a correct A-matrix fill -- GMAT's own STM is the *reference* here, computed by the same mechanism ADR-002's second amendment already validated for gravity/point-mass. Critically, per the brief: if `GetDerivatives` silently omitted a drag/SRP term, GMAT's own propagated STM would omit it too, and this comparison would show clean agreement while both sides were wrong. That is exactly why evidence 3 (finite difference) is decisive and this one is not.

### The shim gap (escalated, not worked around)

`crates/gmat-sys` cannot drive a working `DragForce` today. `gmatviz.scenario.Scenario.force_model()` attaches an atmosphere object with `DragForce.SetReference(atmos)` (the Python API's name for `GmatBase::SetRefObject`). `shim/gmatffi.h` exposes no equivalent -- there is no `gmatffi_set_reference` -- and `DragForce::Initialize()` (`third_party/gmat-src/src/base/forcemodel/DragForce.cpp`, around line 1048) throws when `internalAtmos` was never set that way:

```cpp
if (!atmos)
   throw ODEModelException("Atmosphere model not defined");
```

Confirmed empirically, not just by reading the source -- `crates/gmat-sys/tests/drag_srp_stm.rs`'s `dragforce_without_atmosphere_reference_confirms_the_shim_gap` test builds a `DragForce` with `set_str("AtmosphereModel", "JacchiaRoberts")` (the field alone -- `DragForce::SetStringParameter` for that field only records a string, per `third_party/gmat-src/.../DragForce.cpp` lines ~2454-2467, it never constructs or attaches an object) and gets, verbatim:

```
GMAT error -1: ODEModel Exception Thrown: Atmosphere model not defined
```

Per this task's brief ("do not change the shim -- if the investigation genuinely requires a shim change, stop and escalate"), **this is escalated, not fixed**. `SolarRadiationPressure` has no such gap (`fm.AddForce(Construct("SolarRadiationPressure"))` needs no referenced sub-object), so it is exercised fully through `crates/gmat-sys` (see evidence 3). For drag, and for the full-arc STM measurement below, this task instead drove GMAT's Python API (`gmatpy`) directly -- the same shared library `gmat-sys` links, same `ODEModel::GetDerivatives`/`DragForce::GetDerivatives`/`SolarRadiationPressure::GetDerivatives` C++ code, different binding.

### What was measured, and a genuine, unresolved anomaly

An external fixed-step RK4 (Python, driving `ODEModel::GetDerivatives`/`GetDerivativeArray()` directly -- the same operation `av-dynamics::Dopri5` performs in Rust, just not the same code, because of the shim gap above) was run over the 42-state (STM-requested) drag+SRP model, seeded from a live, in-process, bit-identical copy of the golden's own `x0`/epoch (never round-tripped through the JSON file's string form -- see "a methodological trap" below), at four step sizes:

```
  h(s)   steps  wall(s)        dr(m)        dv(m/s)  STM max abs err       det(Phi)
  30.0    2880     0.25  467399.1898   5.497533e+02       1.8225e+01   0.9955296157
  10.0    8640     0.77  467029.6943   5.493185e+02       2.1213e-01   0.9955314959
   5.0   17280     1.48  467026.0252   5.493142e+02       1.2962e-02   0.9955315132
   2.0   43200     3.77  467025.7977   5.493140e+02       2.2610e-04   0.9955315144
GMAT's own det(Phi) at t1: 0.9955315141
```

Two things are true at once, and both are reported plainly:

1. **The propagated STM (Phi) converges beautifully to GMAT's own recorded value** as step size shrinks -- max abs error 2.3e-4 at h=2s (elements reach ~2x10^5 in magnitude), `det(Phi)` matching GMAT's own to 9 significant figures. Taken alone this would read as a clean confirmation.
2. **The propagated 6-state position does not converge to GMAT's own recorded `final_state`** -- it *stabilizes* (467399 -> 467026 m, essentially flat from h=10s down to h=2s) at a **467 km** error, with velocity error stuck at 0.55 m/s. This is not RK4 truncation error (truncation error shrinks with h; this doesn't) and it is not a seed mismatch -- the seed `x0`/epoch were verified bit-identical to GMAT's own live values before propagating, character-for-character, and a **plain GMAT `Propagator.Step()` run from that same live seed, in the same process, reproduces the same 467 km-away final state** -- i.e. GMAT's own official stepping mechanism, run a second time from a state that prints identically to the golden's own, does not reproduce the golden either.

Control experiments (not exhaustive, but load-bearing for what's reported here):

- The identical RK4-over-`GetDerivatives` method, on the **plain** golden (`leo_1day_jgm2_8x8_sunmoon.json`, no drag/SRP, gravity+point-mass only, higher/slower orbit) converges cleanly on *both* the 6-state and the STM (STM max abs error 1.65 -> 9.0e-6 as h: 30 -> 2s; GMAT's own `det(Phi)` matched to 10 places). So this is not a generic flaw in driving `GetDerivatives` directly instead of through `Propagator::Step()`.
- The same method, on a throwaway **grav+point-mass-only** model built at the *same low, 250 km-altitude orbit* as the drag+SRP golden (no saved golden file for this -- a diagnostic only), also converges cleanly (STM max abs error 2.08 -> 1.04e-5 as h: 30 -> 2s). So it is not simply "this orbit is stiffer."
- That isolates the effect to **drag and/or SRP specifically**, at this orbit, when propagated over the full day.

**What was not resolved:** why the 6-state trajectory is ~467 km off while the STM still agrees closely. The best-supported explanation, though not confirmed: gravity dominates the *magnitude* of Phi's growth over a full day (16 orbits) far more than drag/SRP's comparatively tiny forces (~5.6e-8 km/s^2 for drag vs. ~1e-3 to 1e-6 km/s^2 for gravity at this altitude) do, so two trajectories on the same orbit that differ by a modest along-track phase (467 km is a few percent of the ~41,000 km/day path) can still produce nearly the same gravity-dominated Phi, even while the 6-state itself has visibly diverged. This would mean the STM agreement above is a weaker signal than it looks -- consistent with (not contradicting) the brief's framing that a propagated-STM comparison is not decisive and evidence 3 is. What causes the 467 km divergence itself was not root-caused within this task's scope; candidates considered and not confirmed: epoch precision loss (tested directly -- ruled out, see below), and non-monotonic/repeated-call effects on some delta-time-dependent internal drag/atmosphere bookkeeping that `Propagator::Step()` might maintain differently than repeated direct `GetDerivatives()` calls (not confirmed either way).

**A methodological trap, found and fixed along the way, reported because it nearly produced a false "A-matrix isn't filled" conclusion:** an earlier version of this measurement seeded the 42-state model by *re-specifying the spacecraft's Keplerian elements* (SMA/ECC/INC/...) rather than its Cartesian state. That reproduces a Cartesian `x0` that looks bit-identical when printed but apparently is not exactly so at the level that matters -- under drag+SRP's much steeper A-matrix, that invisible-at-print difference amplified into a **21,164**-magnitude STM disagreement and a `det(Phi)` off by 0.0021, while remaining essentially invisible in the 6-state comparison (a few mm to cm). This was diagnosed by (a) confirming `gen_leo_1day.py`'s own two-phase sequence (build+propagate a plain Keplerian spacecraft, *then* build the STM spacecraft from that run's live Cartesian output) reproduces its own golden to **0.0 m/0.0 STM error** when repeated in a fresh process, and (b) showing the discrepancy disappears the moment the STM spacecraft is seeded from live Cartesian state instead of re-derived Keplerian elements. Reported in full because "do not compensate silently" applies here as much as anywhere: a naive, plausible-looking measurement setup would have produced a large, wrong "STM disagrees" number that had nothing to do with whether `GetDerivatives` fills the A-matrix, and everything to do with how the test itself was seeded.

**Bottom line for evidence 1:** attempted, partially informative (rules out several explanations, demonstrates the shim gap concretely, and shows the STM roughly tracks GMAT's own), but **not** a clean third confirmation of the A-matrix fill -- the unresolved 6-state divergence means it cannot be reported as a tight, GMAT-matching kernel-vs-GMAT STM number the way the plain golden's STM spike (`crates/gmat-sys/tests/stm_spike.rs`) could. Evidence 3 carries the weight of the answer.

## Evidence 2: source

Read `third_party/gmat-src/src/base/forcemodel/DragForce.cpp`, `SolarRadiationPressure.cpp`, `RelativisticCorrection.cpp`, and `ODEModel.cpp` directly; grepped for `fillAMatrix`, `fillSTM`, `stmRowCount`, `GetDerivatives`, `SetStart`/`SetPropertyDerivative`. Quotes below are line-numbered against those files as read during this task.

**`DragForce::GetDerivatives`** (`DragForce.cpp` ~1656-1899): guarded by `if (fillSTM || fillAMatrix)`, it finite-differences its own `Accelerate()` call to build the position submatrix (default: forward difference, `pert = 1.0e-2` km, `useCentralDifferences` defaults `false`) and, when `finiteDifferenceDv` (defaults `true`), the velocity submatrix (`pert = 1.0e-6` km/s):

```cpp
if (finiteDifferenceDv) {
   ... // fills aTilde's velocity columns
} else {
   throw ODEModelException("Analytic differencing for drag model A-matrix "
         "d(accel)/dv terms in not yet implemented");
}
```

Both defaults (`useCentralDifferences=false`, `finiteDifferenceDv=true`, `DragForce.cpp` lines 172-173) mean the shipped default *does* fill both blocks internally, via its own finite differencing -- not analytic, but not zero and not a stub either.

**`SolarRadiationPressure::GetDerivatives`**, spherical model (`SolarRadiationPressure.cpp` ~1280-1325): fills the position submatrix **analytically**:

```cpp
mag = percentSun * cr[i] * fluxPressure * area[i] * distancefactor / (mass[i] * sunDistance);
...
ix = stmRowCount * 3;
aTilde[ix]     = mag * (1.0 - 3.0 * sunSat[0] * sunSat[0] / sSquared);
aTilde[ix + 1] = mag * (    - 3.0 * sunSat[0] * sunSat[1] / sSquared);
aTilde[ix + 2] = mag * (    - 3.0 * sunSat[0] * sunSat[2] / sSquared);
```

No velocity submatrix is written for the spherical model -- correct, not missing: spherical SRP acceleration has no velocity dependence, so `d(accel)/d(vel) = 0` is the physically right answer, and evidence 3 confirms the finite difference agrees.

**A caveat found in the same block**, worth carrying forward: `SolarRadiationPressure.cpp` line ~1196 warns explicitly that shadow (eclipse) partial derivatives are *not* included:

```cpp
MessageInterface::ShowMessage("Warning: The orbit state transition "
      "matrix does not currently contain SRP contributions from shadow "
      "partial derivatives when using " + srpShapeModel + " SRP.\n");
```

i.e. `if (percentSun > 0.0)` gates the whole fill -- inside a penumbra (`0 < percentSun < 1`) the A-matrix is filled as if `percentSun` were constant (its own derivative w.r.t. position is not accounted for), and in full umbra (`percentSun == 0`) the block is filled as exactly zero. The zero-in-umbra case is itself correct (SRP acceleration and its local derivative both are exactly zero deep in shadow), but the *within-penumbra* omission is a real, documented (by GMAT's own authors) incompleteness. This task's golden and finite-difference check both happened to land in full sun or full shadow, never inside a penumbra, so it does not affect the numbers reported here, but it is a limitation the lead should know about if SRP-in-covariance work leans on penumbra-crossing arcs.

**`ODEModel::CompleteDerivativeCalculations`** (`ODEModel.cpp` ~3665-3733) confirms how these per-force contributions combine: each `PhysicalModel` subclass (`GravityField`, `PointMassForce`, `DragForce`, `SolarRadiationPressure`, `RelativisticCorrection`) computes its own A-matrix contribution into its *own* private `deriv` buffer (zeroed, then filled only where that force contributes), and `ODEModel::GetDerivatives`'s per-force loop (`ODEModel.cpp` line 3314, `deriv[j] += ddt[j];`) sums every force's full buffer -- including the STM/A-matrix block -- into the model's aggregate derivative array. This is superposition working correctly: it is why gravity, point-mass, drag and SRP contributions all land in the same final A-matrix without one force's fill overwriting another's. Then, still in `CompleteDerivativeCalculations`, the summed A-matrix (`aTilde`) is multiplied into `d(Phi)/dt = A~ * Phi`:

```cpp
for (Integer j = 0; j < stmRows; ++j)
   for (Integer k = 0; k < stmRows; ++k) {
      Integer element = j * stmRows + k;
      deriv[i6+element] = 0.0;
      for (Integer l = 0; l < stmRows; ++l)
         deriv[i6+element] += aTilde[j*stmRows+l] * state[i6+l*stmRows+k];
   }
```

This is also the mechanism this task's evidence-3 "identity trick" exploits: seeding `Phi = I` in the input state makes this multiplication return `A~` itself, with no propagation needed.

**Bonus finding, not this task's central question but directly relevant to the same open sentence in the ADR:** `RelativisticCorrection::GetDerivatives` (`RelativisticCorrection.cpp` ~429-509) allocates and zero-initializes its `aTilde` buffer for both the `fillSTM` and `fillAMatrix` branches, and then writes that all-zero buffer straight into `deriv` -- **no physics is ever computed into it**:

```cpp
if (fillSTM) {
   ...
   for (Integer j = 0; j < stmRowCount; ++j) { ix = j * stmRowCount; for (k...) aTilde[ix+k] = 0.0; }
   for (Integer j = 0; j < stmRowCount; j++)
      for (Integer k = 0; k < stmRowCount; k++) {
         element = j * stmRowCount + k;
         deriv[i6+element] = aTilde[element];   // aTilde is exactly zero here, unconditionally
      }
}
```

Unlike SRP's umbra case (correctly zero) or drag's finite-difference fill (correctly nonzero), this is unconditional -- `RelativisticCorrection`'s A-matrix contribution is zero regardless of state, always, a stub rather than a physically-justified absence. Not independently measured by finite difference in this task (out of the stated scope, and this task's golden does not include a relativistic-correction force), so this is source-reading evidence only, not confirmed by evidence 3's method -- but the code is unambiguous, and the lead may want a follow-up task scoped to it specifically.

## Evidence 3: finite difference (the decisive check)

**What this proves, and what a propagated-STM comparison cannot:** this compares `GetDerivatives`'s own acceleration output, differentiated numerically, against the A-matrix block `GetDerivatives` itself reports for the same state -- both sides come from the same function, so if a term were silently zero, *both* the analytic/internal A-matrix and the finite difference of the acceleration it's supposed to match would show it (a real, physically nonzero acceleration whose position/velocity derivative the A-matrix fails to capture) -- there is no way for "both sides omit the same term and agree" to happen here, unlike comparing against GMAT's own STM.

**Method (the "identity-Phi trick"):** `ODEModel::CompleteDerivativeCalculations` computes `d(Phi)/dt = A~ * Phi` using whatever `Phi` is in the *input* state (see evidence 2's quote). Seeding a fresh 42-state as `[position, velocity, identity 6x6]` and calling `GetDerivatives` at any state/epoch therefore returns `d(Phi)/dt = A~ * I = A~` directly -- the A-matrix itself, at that exact state and epoch, no propagation or matrix inversion required. This works at *any* state, not only `t=0`.

### SRP (`crates/gmat-sys/tests/drag_srp_stm.rs`, `finite_difference_a_matrix_isolates_srp_contribution`, real assertions, real output)

Built an SRP-only force model (no gravity, no point masses -- isolates SRP's own contribution without needing to subtract two models) through `gmat-sys`, at two epochs (t0 and t1 of the drag+SRP golden's arc). The golden's own orbital positions at those exact epochs happen to be in Earth's shadow (`percentSun == 0`, confirmed -- both GMAT's A-matrix and the finite difference agree it's exactly zero there, which is correct per evidence 2's umbra case, but uninformative for checking a *nonzero* fill), so the test scans six candidate positions (+/-X/Y/Z at the same orbital radius) and finite-differences at the first sunlit one it finds. Central differences, h_pos = 1 m, h_vel = 1 mm/s.

```
[gmat-sys drag/SRP] t0 candidate 0 (dir [1.0, 0.0, 0.0]): |SRP accel| = 8.488513e-11
[gmat-sys drag/SRP] SRP-only A-matrix vs finite difference at t0 (dt=0):
    d(accel)/d(pos) max abs error = 2.362854e-23 (GMAT's block nonzero: true)
    d(accel)/d(vel) max abs error = 0.000000e0
[gmat-sys drag/SRP] t0: GMAT's d(accel)/d(pos) block          = [5.22685932190112e-19, 2.770096424831848e-19, 1.2007816256664278e-19, 0, 0, 0, 2.770096424831848e-19, -8.344974143435821e-19, -6.118767672457352e-19]
[gmat-sys drag/SRP] t0: finite-difference d(accel)/d(pos) block = [5.226844430797477e-19, 2.770085699772307e-19, 1.2008336048797156e-19, 0, 0, 0, 2.770085699772307e-19, -8.344895287467579e-19, -6.119003957875667e-19]

[gmat-sys drag/SRP] t1 candidate 0 (dir [1.0, 0.0, 0.0]): |SRP accel| = 8.488780e-11
[gmat-sys drag/SRP] SRP-only A-matrix vs finite difference at t1 (dt=86400):
    d(accel)/d(pos) max abs error = 2.394623e-23 (GMAT's block nonzero: true)
    d(accel)/d(vel) max abs error = 0.000000e0
```

Position block: nonzero, and the analytic A-matrix agrees with the finite difference to **~1e-23 absolute** (~2e-4 relative, at the finite-difference truncation floor for this step size on ~1e-19-scale numbers). Velocity block: exactly zero on both sides, matching evidence 2's reading (spherical SRP has no velocity dependence -- correctly absent, not missing).

### Drag (Python/`gmatpy`, due to the shim gap; same C++ code as `gmat-sys` links)

Isolated drag's own A-matrix contribution by subtraction (grav+pm+SRP+drag minus grav+pm+SRP, both evaluated at the golden's actual `x0`, real orbital velocity ~7.6 km/s so drag's velocity dependence is fully exercised, `dt=0`), using the same identity-Phi trick, and independently central-differenced (h_pos = 1 m, h_vel = 1 mm/s) the same subtraction applied to plain acceleration output:

```
GMAT's drag-only A-matrix block (rows=accel x,y,z; cols=x,y,z,vx,vy,vz):
   [-3.321135e-10, -1.928671e-10, -1.937224e-13, -8.097797e-09,  1.090497e-09,  1.765330e-09]
   [ 5.757819e-10,  3.331125e-10,  3.341853e-13,  1.090497e-09, -9.356996e-09, -3.057641e-09]
   [ 9.312118e-10,  5.392541e-10,  5.432259e-13,  1.765330e-09, -3.057642e-09, -1.241800e-08]
Finite-difference drag-only Jacobian:
   [-3.321536e-10, -1.928804e-10, -1.934217e-13, -8.097689e-09,  1.091141e-09,  1.765948e-09]
   [ 5.758519e-10,  3.331359e-10,  3.335006e-13,  1.090707e-09, -9.357098e-09, -3.057450e-09]
   [ 9.313251e-10,  5.392914e-10,  5.424076e-13,  1.765330e-09, -3.057641e-09, -1.241800e-08]

drag-only d(accel)/d(pos) block: GMAT nonzero = True, max abs error vs FD = 1.133064e-13
drag-only d(accel)/d(vel) block: GMAT nonzero = True, max abs error vs FD = 6.436689e-13
drag-only acceleration at x0 (km/s^2): [1.6202023164074708e-08, -2.8062727305995516e-08, -4.542879354489155e-08]
|drag accel| (km/s^2): 5.580141e-08
```

Both blocks nonzero and matching finite difference to **~1e-13 absolute** against a ~1e-8 to 1e-9-scale signal (relative error ~1e-4 to 1e-5, consistent with the finite-difference truncation floor at these step sizes, not a real discrepancy). **The velocity block is the specific thing the brief called out as diagnostic** ("Drag depends strongly on velocity, so a missing drag term shows up most clearly in the velocity block") -- it is filled, nonzero, and correct.

### Conclusion from evidence 3

Both drag's and SRP's A-matrix contributions are filled, nonzero where physics says they should be, zero where physics says they should be (SRP's velocity block), and match an independent finite difference of `GetDerivatives`'s own acceleration output to within finite-difference truncation error. This is the decisive evidence per the brief's own framing, and it says **yes**.

## The new golden

- **File:** `goldens/leo_1day_jgm2_8x8_sunmoon_drag_srp.json` (new; the existing `goldens/leo_1day_jgm2_8x8_sunmoon.json` was not modified -- verified byte-identical, modulo `generated`/`reason`/`sha256`, before and after this task's changes to the shared generator script).
- **Orbit:** SMA 6628 km (~250 km circular altitude, ECC 0.001, INC 51.6 deg, RAAN 30 deg, AOP 0, TA 0) vs. the plain golden's SMA 6878 km (~500 km altitude) -- chosen because atmospheric density falls off exponentially with altitude, so 250 km is low enough that drag measurably perturbs the arc (~9 km of altitude decay over one day, confirmed in the golden's own `final_state`) while staying well clear of the propagator's `MaxStep`/`Step()`-return-value concerns (question 77) -- same inclination and spacecraft physical parameters (mass, Cd, Cr, drag/SRP area) as the plain golden, so only the force model and altitude differ.
- **Force model:** JGM2 8x8 gravity + Luna/Sun point masses (same as the plain golden) + `DragForce` (`JacchiaRoberts` atmosphere) + spherical `SolarRadiationPressure`.
- **Atmosphere model chosen: `JacchiaRoberts`, not `MSISE90`.** Both `gmat.Construct()` headlessly with the packaged `data/atmosphere/earth/SpaceWeather-All-v1.2.txt` data pack (verified: both constructed without error). `JacchiaRoberts` was picked because it is the more commonly used LEO drag model in GMAT's own tutorials/examples -- no other reason; either would likely have worked equally well for this task's purpose (measuring whether the A-matrix is filled, not comparing drag models).
- **`--reason` recorded:** *"M4.3: ADR-002's second amendment left open whether DragForce, SolarRadiationPressure and RelativisticCorrection fill their A-matrix contributions -- the leo_1day_jgm2_8x8_sunmoon golden's force model exercises none of them. This second golden (250 km-altitude circular LEO, same inclination/spacecraft as the plain golden) adds DragForce (JacchiaRoberts atmosphere) and spherical SolarRadiationPressure so drag and SRP actually perturb the arc, and records GMAT's own 6-state end state plus 42-state STM (with drag+SRP in the A-matrix) for crates/gmat-sys/tests/drag_srp_stm.rs to pin against."*
- **Recorded numbers:** `det(Phi)` at t1 = **0.9955315140717097** (vs. the plain golden's 1.000000000091) -- itself a useful, independent confirmation that drag's dissipative (non-conservative) contribution is present in the propagated STM: Liouville's theorem (`det(Phi) = 1`) holds for conservative (gravity-only) dynamics and should *not* hold once a velocity-damping force like drag is genuinely included, exactly what's observed.
- Regenerated once to check reproducibility of the generator itself (separate from the RK4-seeding anomaly above): identical `det_phi_t1`/`final_stm`/`final_state` to the original generation, confirming `gen_leo_1day.py --drag-srp` itself is deterministic across process invocations.

## Files created/changed

- `/Users/probe/code/AltaVista/goldens/gen_leo_1day.py` -- added `--drag-srp` and a shared `_add_forces()` helper (gravity/point-mass/drag/SRP); the plain-golden code path is unchanged in behavior (verified: regenerating it reproduces every field except `generated`/`reason`/`sha256` byte-for-byte; the file on disk was restored to its pre-task content afterward and confirmed byte-identical).
- `/Users/probe/code/AltaVista/goldens/leo_1day_jgm2_8x8_sunmoon_drag_srp.json` -- new golden.
- `/Users/probe/code/AltaVista/crates/gmat-sys/tests/drag_srp_stm.rs` -- new test file, two tests, both passing:
  - `dragforce_without_atmosphere_reference_confirms_the_shim_gap`
  - `finite_difference_a_matrix_isolates_srp_contribution`
- `/Users/probe/code/AltaVista/docs/teamlog/adr-002-amendment-draft-drag-srp.md` -- this file.
- `docs/adr/002-dynamics-contract.md` -- **not modified** (read-only per the brief).

## Definition-of-done commands, real output

```
$ cd /Users/probe/code/AltaVista
$ export PATH="/opt/homebrew/opt/rustup/bin:$PATH"; cargo test --workspace
   ... (11 test binaries; totals below)
   test result: ok. 47 passed; 0 failed ...
   test result: ok. 2 passed; 0 failed ...
   test result: ok. 12 passed; 0 failed ...
   test result: ok. 11 passed; 0 failed ...
   test result: ok. 20 passed; 0 failed ...
   test result: ok. 2 passed; 0 failed ... (24.02s -- the STM spike's own integration)
   test result: ok. 1 passed; 0 failed ...
   test result: ok. 2 passed; 0 failed ...
   test result: ok. 2 passed; 0 failed ...
   test result: ok. 1 passed; 0 failed ...
   test result: ok. 1 passed; 0 failed ...
   (+ 4 doc-test crates, 0 tests each)
   TOTAL: 101 passed, 0 failed (baseline in the brief: 83; sibling workers' concurrent additions plus this task's 2 new tests account for the difference)

$ export PATH="/opt/homebrew/opt/rustup/bin:$PATH"; cargo clippy --workspace --all-targets -- -D warnings
    Checking av-cdm v0.1.0 ...
    Checking av-dynamics v0.1.0 ...
    Checking gmat-sys v0.1.0 ...
    Checking av-kernel v0.1.0 ...
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.34s
   (clean, no warnings)

$ .venv/bin/python -m pytest -q
   ........................................................................ [ 51%]
   ............................................................. [100%]
   141 passed, 3 warnings in 2.16s
   (warnings are pre-existing/unrelated: an httpx2 deprecation notice and two unregistered
   pytest.mark.slow markers in tests/test_gmat_service.py, not owned by this task)
```

One transient note for the record: a single `cargo test --workspace` run mid-task failed to *compile* `av-kernel` (a `run_with_covariance` argument-count mismatch between `crates/av-kernel/src/kernel.rs` and its own test) -- that crate is explicitly not mine and the failure was gone on the very next run, consistent with a concurrent sibling worker mid-edit, not anything in this task's changes (this task touches only `goldens/**` and one new file under `crates/gmat-sys/tests/`).

## What could not be determined, and what to try next

1. **The 467 km / 0.55 m/s 6-state divergence in evidence 1's full-arc measurement was not root-caused.** Ruled out: RK4 truncation error (doesn't shrink with step size), seed-state mismatch (verified bit-identical), general GetDerivatives-vs-Step() incompatibility (the same method works cleanly on the plain golden and on a grav+pm-only model at the same low orbit). Not confirmed: whether it's specific to repeated direct `GetDerivatives()` calls under drag's own time-dependence (space weather/atmosphere rotation bookkeeping that `Propagator::Step()` might maintain differently), or something else entirely. **Next step:** instrument `DragForce`/atmosphere-model internals (or add targeted `MessageInterface::ShowMessage` debug output, GMAT already has `#ifdef DEBUG_*` hooks for exactly this throughout `DragForce.cpp`) to compare the density/LST computed at matching epochs between a `Step()`-driven run and a direct-`GetDerivatives()`-driven run at the same instants.
2. **`RelativisticCorrection`'s zero A-matrix fill was read from source but not independently confirmed by finite difference** (out of this task's stated scope, and the goldens here carry no relativistic-correction force). If the lead wants it nailed down the way drag/SRP were, that is a small, well-scoped follow-up: add `RelativisticCorrection` to a force model, evaluate `GetDerivatives` at a state where the correction is non-negligible, and finite-difference it the same way.
3. **The shim gap (`gmatffi_set_reference`/`SetRefObject`) blocks any drag work through `crates/gmat-sys`,** not only this task's. Escalating: this needs a lead/owner decision on whether it's worth a shim change (it is currently the single largest blocker to exercising `DragForce` from Rust at all, including outside this task's scope -- e.g. any future kernel-side drag model validation).

## Approximations and shortcuts, stated plainly

- Evidence 1's "kernel-style" integrator is a hand-written fixed-step Python RK4, not `av-dynamics::Dopri5` (Rust) -- necessitated by the shim gap for drag. It is not adaptive, and is not the actual product code path; it stands in only as an external-integrator-over-GetDerivatives demonstration.
- The finite-difference checks use central differences with step sizes chosen once (h_pos = 1 m, h_vel = 1 mm/s) and not swept for optimal truncation-vs-cancellation balance; the reported agreement (~1e-13 to 1e-23 absolute, depending on the check) is well within what these step sizes would be expected to achieve for smooth functions, and is reported as measured, not tuned to pass.
- The SRP finite-difference check had to scan for a sunlit test position rather than using the golden's own t0/t1 states directly, because those specific points are in Earth's shadow for this orbit/epoch (itself confirmed, not assumed). The scan is a legitimate methodological adjustment (SRP being genuinely, physically zero in full shadow is not informative for checking whether a *nonzero* fill is correct), documented in the test file and here, not a cherry-pick of a favorable result.
- No golden was generated for a grav+pm-only model at the drag+SRP golden's lower orbit; the comparisons against it (evidence 1's control experiments) used a throwaway, unsaved reference computed in the same script, appropriate for a diagnostic but not a pinned artifact.
- `docs/teamlog/adr-002-amendment-draft-drag-srp.md` (this file) is a draft for the lead's review, per the brief -- it does not itself amend `docs/adr/002-dynamics-contract.md`, which was not touched.
