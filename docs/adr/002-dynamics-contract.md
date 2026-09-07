# ADR-002: One dynamics contract, GMAT at three depths, goldens as the proof

- **Status:** Accepted (drafted 2026-09-02; accepted by the user 2026-09-02)
- **Date:** 2026-09-02
- **Plan reference:** [architecture.md](../architecture.md) sections 2–3; questions 15–22
- **Implemented by:** `proto/altavista/v1/dynamics_service.proto`, `crates/av-dynamics` (`gmat-sys`, native models), `services/gmat-service`, `goldens/` (P0 spike, P1, P3)
- **Depends on:** [ADR-001](001-cdm-v1.md); spoore ADR-001 (the comparison currency), spoore ADR-005 (out-of-process outputs are logged as evidence)

## Context

The user's instruction is precise: GMAT's real value is *validated model performance*, and
the platform may need to integrate GMAT dynamics directly into a different simulation engine.
gmatviz established three facts. GMAT R2026a is Apache 2.0, so its C++ core may be linked.
`ODEModel.GetDerivatives(state, dt, order)` exposes the force model as a derivative function
that an external integrator can drive (`api/Ex_R2020a_BasicForceModel.py`). And stepping the
GMAT propagator with state write-back reproduces a continuous run to 1e-11 km, so design-time
propagation and a real-time step are the same computation at different pacing.

The other side of the requirement is that the same dynamics must serve batch design,
feasibility sweeps, lockstep software-in-the-loop, and real-time hardware-in-the-loop, at
step rates that are a configuration option (question 19), in domains GMAT does not cover
(relative motion for RPO, 6-DoF air first; question 21).

## Decision

### One contract

Every dynamics model implements, over CDM v1 types:

```
derivatives(state, t, controls)        -> state_dot
step(state, t, controls, dt)           -> (state, t + dt, outputs)
propagate(seed, control_schedule,      -> Trajectory
          horizon, sampling, covariance?)
solve(problem)                         -> Solution         (optional capability)
describe()                             -> ModelInfo        (state space, controls, capabilities)
```

`derivatives` is the portable form: it is what lets another engine own the integrator.
`step` is what the kernel calls. `propagate` is the design-time verb and returns the CDM
`Trajectory`. `solve` is a declared capability that GMAT-backed models expose for targeting
(differential corrector as a platform call; Yukon and CSALT through GMAT scripts,
question 17). spoore's estimation contract (`Predictor` / `ModelService`) is unchanged and
sits beside this one; a node in the model tree may bind either.

The contract is exported three ways so other engines can use it: a C ABI from
`av-dynamics`, gRPC (`DynamicsService`), and an FMU (FMI 3.0 co-simulation, P2; question 23).

### GMAT at three depths, in this order

1. **Python API in-process, now** (question 15). `services/gmat-service` hosts GMAT through
   `gmatpy` (the gmatviz lineage): `step` by propagator stepping with write-back, `propagate`
   by the same loop or by a script run with an injected report (`SolverIterations = Current`),
   `solve` by GMAT's differential corrector. Outputs are logged as evidence (spoore
   ADR-005) so replays read the answer and never re-run GMAT.
2. **FFI to the C++ core, spiked in P0.** `gmat-sys` links `libGmatBase` and the force-model
   plugins through a C shim and calls `GetDerivatives` from Rust, so the kernel's own
   integrator drives GMAT's gravity, drag, SRP, point-mass, relativity and tide models.
   The spike answers one question: can `GetDerivatives` be driven re-entrantly from Rust in a
   process that holds no GMAT script state? Its exit criterion is a one-day LEO arc
   integrated by our integrator over GMAT's derivatives matching GMAT's own propagator to a
   declared tolerance.
3. **Native Rust models, P3, with GMAT as the oracle.** Spherical-harmonic gravity for Earth,
   Moon and Mars, third bodies from DE ephemerides, spherical and high-fidelity SRP,
   MSISE-00 and Jacchia-Roberts drag, relativity, solid and ocean tides, and SPICE kernel
   reading and ephemeris propagation, in that order (question 16). Each is pinned in CI
   against GMAT reference arcs (position, velocity and state transition matrix to declared
   tolerances over declared arcs, at declared step sizes and settings). This is how the
   validated performance survives an engine that cannot link GMAT at all.

### The integrator is ours

Whichever depth supplies `derivatives`, **the kernel's integrator steps the state**
(question 20): one family (Runge-Kutta, Prince-Dormand 7(8), adaptive step) shared by every
domain and binding, validated against GMAT's integrators on the same goldens. GMAT's own
integrators remain the reference and the design-time path through depth 1.

### Rates and pacing

Step rates are declared per system in the system definition, default 10 Hz for kernel
dynamics with faster loops inside the bound flight software (question 19). Depth 1 is a
supported configuration at the default rate; faster settings use depths 2 and 3. Nothing in
a profile changes a rate (ADR-000, rule 1).

### Goldens

`goldens/` holds GMAT reference arcs generated by an explicit command with a recorded reason,
never regenerated by a test. Every golden records the GMAT version, data-pack hash, script
hash, settings and tolerance. Native models, our integrator, the km-to-metre conversion and
the frame conversions are all pinned against them.

## Alternatives considered

**GMAT as a sidecar process only** (the first draft of the architecture). Simplest
integration; rejected as the only depth because it makes the platform depend on a GMAT
process for every step, cannot run in another engine, and pays an RPC per step at real-time
rates. Kept as depth 1's hosting form.

**FFI first.** Highest fidelity in the hot path soonest. Rejected as the first step because
it blocks the first demonstration on C++ build and re-entrancy work with an unknown answer.
Spiked in P0 instead, so the answer arrives before P1 depends on it.

**Native models only, GMAT for goldens.** Cleanest deployment. Rejected as the first step
because it re-implements the validated performance before reusing it; kept as the P3 end
state for the hot path.

**GMAT's integrators, always.** Simplest parity. Rejected because it leaves a second
integrator in the kernel for every other domain and no portable derivative interface, which
is the property the user asked for.

**Different rates per profile.** Rejected by ADR-000 rule 1; the rate is a property of the
system, not of where it runs.

## Consequences

**A `gmat-sys` crate exists and is hard.** GMAT's core was not built as an embeddable
library: the API bundles it through SWIG and holds global state (one configuration per
process). If the spike fails, depths 1 and 3 carry the plan and this ADR is amended with the
measurement.

**GMAT data packs are platform data.** Gravity files, DE ephemerides, SPICE kernels,
space-weather and EOP files ship as versioned, hashed data packs (question 22); every run
records the pack hash beside its configuration hash.

**Every domain model, not only GMAT's, has goldens.** The relative-motion models
(Clohessy-Wiltshire, full nonlinear relative propagation) are pinned against GMAT's
`ObjectReferenced` relative states; 6-DoF air models are pinned against a declared
reference (to be chosen with the model, likely JSBSim), because a model without a golden is
a model nobody can tell has drifted.

**Evidence, not re-execution.** Any out-of-process host (depth 1, an FMU, a third-party
engine) has its outputs logged as evidence per spoore ADR-005, so replay is exact even when
the host is not deterministic.

## What would falsify this

The premise is that GMAT's force models can be driven by an external integrator and that our
integrator, validated against GMAT's, reproduces GMAT's results to a tolerance that the
mission designers accept.

The observables: the P0 spike failing on re-entrancy (falsifies depth 2, not the ADR); a
golden arc where our integrator over GMAT's derivatives disagrees with GMAT's propagator
beyond tolerance at every reasonable step control (falsifies "the integrator is ours" and
would force GMAT's integrators back into the kernel for the space domain); or a native model
that cannot reach parity on a golden despite matching the published model (which would mean
the golden captures a GMAT implementation detail, and the tolerance, not the model, needs
revisiting).

## Amendment 2026-09-02: the P0 spike, measured

The spike in `crates/gmat-sys` answered depth 2's question the same day the ADR was
accepted. Setup: GMAT R2026a's shipped `libGmatBase` / `libGmatUtil` linked through a C shim
compiled against the R2026a source headers (SourceForge branch `GMAT-R2026a`) with
`-D__USE_SPICE__` to match the binaries' class layouts, plus CSPICE headers; an
`ODEModel` bound to a spacecraft through a `PropagationStateManager`; `GetDerivatives`
called from Rust by our Dormand-Prince 5(4) at rtol = atol = 1e-12.

| Measurement | Result |
|---|---|
| Golden `leo_1day_jgm2_8x8_sunmoon` (JGM2 8x8, Sun, Moon; 86,400 s) vs GMAT PrinceDormand78 at 1e-13 | position 5.7 mm, velocity 6.3e-6 m/s |
| Integration cost | 8,947 accepted steps, 139 rejected, 63,602 derivative calls, 0.20 s (debug build) |
| `GetDerivatives` per call, release build, 8x8 gravity + Sun + Moon | 2.57 µs (390 kHz) |
| Same call through the Python API | 10.4 µs |
| Model build (construct, initialize, bind) | 29.6 ms |
| Two `ODEModel`s in one process, calls interleaved | derivatives unchanged; third-body `dt` honoured |
| Layout | derivative of position equals velocity; state is central-body MJ2000Eq, km and km/s |

**Consequences for the plan.** Depth 2 is real: our integrator over GMAT's derivatives
reproduces GMAT's propagator to the millimetre with one process-wide GMAT configuration, so
the kernel can own the integrator for the space domain from P1 rather than P3. The per-call
cost leaves a 10 Hz kernel step with adaptive substeps far inside budget (a whole day of LEO
at 1e-12 costs 0.2 s). Two limits stand: GMAT holds one configuration per process and is not
thread-safe (all handles are `!Send`; parallelism is by process, as ADR-003's job runner
assumes), and the shim is macOS-only until `build.rs` learns Linux library names. The
covariance path (`PropagateSTM`, 42-state) is the next spike; the Python-API host remains
the design-time path for solvers.

## Amendment 2026-09-02 (second): the state transition matrix path, measured

The covariance spike named above ran the same day (team 1, M1.4; full evidence in
`docs/teamlog/adr-002-amendment-draft-stm.md`). `PropagationStateManager::SetProperty("STM",
spacecraft)` before `BuildState()` grows the propagation state from 6 to 42, and
`ODEModel::GetDerivatives` fills `dΦ/dt = A(t)Φ` in the extra block on every call, so the
same dimension-agnostic integrator propagates the STM alongside the state.

| Measurement | Result |
|---|---|
| 42-state DP5(4) at 1e-12 vs GMAT's own 42-state run, golden arc | position 2.7 mm, velocity 3.0e-6 m/s |
| STM (36 elements) vs GMAT's final STM | max abs 6.3e-5, max relative 5.5e-7 |
| `det Φ` at t1 (Liouville check) | ours 1.000000000010, GMAT 1.000000000091 |
| `Φ(t0,t0)` | exact identity; requesting the STM leaves the 6-state derivatives bit-identical |
| `GetDerivatives` per call, 42-state, release | 3.40 µs (1.33× the 6-state 2.57 µs) |
| Element order | row-major, `6 + row·6 + col`, confirmed in `ODEModel::CompleteDerivativeCalculations` and against `GetRmatrixParameter("STM")` after one day |

**Consequences.** Covariance propagation through the STM is available at depth 2 from now,
which is what makes `DrmOptions.covariance` cheap to honour in the kernel. Not yet
determined, and left open deliberately: whether drag, SRP and relativity fill their A-matrix
contributions (the golden's force model has none of them); GMAT's own `Covariance`
property; and more than one STM-bearing spacecraft in one `ODEModel`. The kernel's
`GmatModel` still wraps the 6-state model only; carrying the 42-state into the
`DynamicsModel` trait is the next kernel task.

## Amendment 2026-09-02 (third): which forces fill the A-matrix, measured

Team 1's M4.3 answered the second amendment's open item on a second golden arc
(`leo_1day_jgm2_8x8_sunmoon_drag_srp.json`: 250 km LEO, JGM2 8x8, Sun and Moon,
Jacchia-Roberts drag, spherical SRP), by finite-differencing `GetDerivatives` around the
identity STM and by reading GMAT's source. Full evidence in
`docs/teamlog/adr-002-amendment-draft-drag-srp.md`.

| Force | A-matrix contribution | Evidence |
|---|---|---|
| `DragForce` | filled, position and velocity blocks (GMAT finite-differences its own acceleration internally) | finite difference agrees to ~1e-13; `det Φ` = 0.99553 on the dissipative arc, exactly as Liouville requires and impossible with a zero drag block |
| `SolarRadiationPressure` (spherical) | filled analytically, position block; velocity block correctly zero | finite difference agrees to 2e-23 against a 5e-19 signal |
| SRP inside a penumbra | shadow partials omitted (GMAT warns at initialization) | source; not exercised by the arc |
| `RelativisticCorrection` | **zero: a stub, not an absent term** | source (`RelativisticCorrection.cpp`, both `fillSTM` and `fillAMatrix` branches write a zeroed buffer) |

**Decisions (questions 82, 83).** The platform declares the STM capability absent for any
force model that includes `RelativisticCorrection`, and treats penumbra arcs the same way;
a covariance request on such a model is a typed error unless the DRM sets
`accept_missing_stm_terms`. A covariance that fails the Cholesky check is a typed, counted
error unless the DRM sets `nearest_spd_projection`. Both fields are additive on `DrmOptions`.

**The divergence that was not a physics finding (question 81).** The team reported a 467 km
one-day disagreement between a `GetDerivatives`-driven integration and the drag golden's
end state while the STM agreed. The lead root-caused it: the golden generator seeded the
42-state spacecraft from a Cartesian state without copying `DryMass`, `Cd`, `Cr`, `DragArea`
and `SRPArea`, so it flew with GMAT's defaults (850 kg, 15 m², 1 m²) while the 6-state run
used 500 kg, 5 m², 5 m². Reproduced to the metre (467,025.793 m with defaults, 0.000 m
with the golden's properties). The generator now copies the ballistic set, asserts both
vehicles agree, records them in the golden, and both goldens were regenerated; the plain
arc's end state is bit-identical to before. The lesson generalizes: a seed is a vehicle,
not a state vector, and every adapter that constructs a GMAT spacecraft from CDM state must
carry the entity's physical properties with it.

## References

- `crates/gmat-sys` (shim, wrapper, integrator, golden test, STM spike test, drag/SRP STM test, bench); `goldens/gen_leo_1day.py`; `docs/teamlog/adr-002-amendment-draft-stm.md`; `docs/teamlog/adr-002-amendment-draft-drag-srp.md`.
- GMAT R2026a: `api/Ex_R2020a_BasicForceModel.py` (`GetDerivatives`, `GetDerivativesForSpacecraft`), `api/Ex_R2020a_PropagationLoop.py`, `docs/GMAT_API_Cookbook` (propagation, STM propagation), `docs/GMATMathSpec.pdf`, `License.txt`.
- gmatviz measurements (this repo, 2026-09-02): chained stepping with write-back reproduces a continuous run to 3.5e-11 km over 20 steps; `SolverIterations = Current` captures the converged pass of a targeting loop (Hohmann 152 rows, Mars B-plane 1090 rows over 308 days).
- spoore ADR-001 (comparison currency), ADR-005 (logged evidence), `crates/spoore-models/src/predictor.rs`.
- Modelica Association, *FMI 3.0 co-simulation*.
- Vallado, D. *Fundamentals of Astrodynamics and Applications*, 4th ed. (relative motion, Clohessy-Wiltshire); Alfriend et al., *Spacecraft Formation Flying* (RIC dynamics).

## Amendment 2026-09-04 (fourth): frame conversion is part of the contract

Team 1's M18.1 found that `spacecraft.CoordinateSystem` in a system definition was copied
into the trajectory's `frame_id` while the state read back through the shim is always the
integration frame, so a DRM declaring any other coordinate system produced a silently
mislabelled trajectory (question 128). The contract therefore gains a fourth capability next
to derivatives, step and STM: **convert**, realized by a `gmat-sys` shim over GMAT's
`CoordinateConverter::Convert` for a state at an epoch between two registered coordinate
systems. A trajectory is emitted in its declared frame by converting every sample through
that call, the conversion is pinned against GMAT's own report in the target frame, and the
same call fills `FrameDefinition.fixed_rotation_q` for frames whose rotation relative to
their parent is constant (ICRF against MJ2000 equatorial, the frame bias), so a consumer
such as the viewer can realize inertial-to-inertial rotations from the wire alone
(question 129). Until the shim lands the loader refuses a coordinate system other than the
integration frame with a typed error rather than mislabel.

## Amendment 2026-09-05 (fifth): one measured exception to "goldens against GMAT"

Team 1's M21.4 rotated covariance through the same `CoordinateConverter` call as the mean
(question 138) and pinned it against GMAT's own `OrbitErrorCovariance` report in
`EarthFixed`. The position block agrees to 8.7e-17 relative; the cross and velocity blocks
disagree by 1.26e-6 and 9.17e-11, linear and quadratic in Earth's rotation rate, because
GMAT's `OrbitData::GetCovarianceRmat66` builds the block-diagonal `[[R, 0], [0, R]]` and
never applies the rotation's time derivative. Our rotation uses `[[R, 0], [Ṙ, R]]` and is
checked against that form to 9.6e-18 relative, with the SPD check re-run per sample. This is
the first place the platform is deliberately more complete than GMAT's report, so the rule
"goldens against GMAT are the proof for every dynamics model" gains its one recorded
exception: a covariance golden in a rotating frame is proved against the closed-form
rotation built from GMAT's reported `R` and `Ṙ`, and a test asserts the disagreement with
GMAT's report so the exception cannot go unnoticed. Whether to report it upstream to the
GMAT project is the user's call (question 150).
