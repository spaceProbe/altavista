# Native orbital dynamics: plan

The edge team's charter after `p5-plan.md` closed (question 221), taken by the lead on
2026-09-16 as its standing recommendation the user had not redirected (the board half of P3
needs a ZCU102/104 on the bench, which this host does not have). Decisions in
`open-questions.md` question 222. This is P3's "native Rust orbital dynamics pinned against
GMAT goldens" in `architecture.md`, under ADR-002: one dynamics contract, GMAT at three
depths, goldens as the proof, the integrator ours.

## Goal

A native Rust orbital force model behind ADR-002's `DynamicsModel` contract, selectable per
DRM beside the GMAT-backed model, reading GMAT's own data files (gravity coefficients,
planetary ephemerides, space weather), with every force pinned against a GMAT golden at a
recorded tolerance, its state transition matrix through the same variational path the
GMAT-backed model exposes, and no GMAT library linked when the DRM selects it, so a kernel
build without `gmat-sys` can propagate. GMAT stays the validated reference; the native model
is the portable one.

## What exists

- ADR-002 and its five amendments; `crates/av-dynamics` (`DynamicsModel`, `ErasedModel`,
  `StmAugmented`, `Dopri5`, `StepResult`, the outputs plumbing) and `crates/gmat-sys` (the
  shim: `GetDerivatives`, STM, drag and SRP A-matrix terms, `convert`, parameter access);
  the kernel's executor selecting models through the registry by `ModelKind`.
- Goldens under `goldens/` generated against GMAT with their `gen_*.py` scripts and
  reasons: LEO one day JGM2 8×8 with Sun and Moon (the P0 spike, 5.7 mm/day through GMAT's
  derivatives), the ICRF and body-fixed two-hour runs, covariance runs, maneuver runs in
  VNB, RIC and GMAT's LVLH, planetodetic longitude, `rmag`, the two-instance demo.
- GMAT's data in the install: `data/gravity/earth/{JGM2,JGM3,EGM96}.cof`, the DE
  ephemerides (`leDE1941.405` and later), `data/atmosphere` for Earth and Mars, space
  weather and EOP files, the IAU pole data the frame service already validates.
- The frame registry and the convert shim (ADR-002's fourth amendment), `altavista/FRAMES.md`
  with measured tolerances, the jitter harnesses.
- Not yet: any native force, ephemeris reader, atmosphere, SRP, native STM, native frame
  reduction, or a kernel build without GMAT.

## Isolation

The team keeps the worktree `/Users/probe/code/AltaVista-edge` on branch `edge` (the name is
history; the plan is this file). New code lives in a new crate `crates/av-orbital` (forces,
ephemerides, atmospheres, the native model and its STM), new goldens under `goldens/` with
their generator scripts and a recorded reason each, and additive changes to
`crates/av-dynamics` only where the contract needs a hook it lacks (each such change named in
the status section). It does not edit `gmat-sys`, the kernel's executor, router or fault
modules, or any other track's crates. The heavy team works in its own worktree at the same
time; the tracks share nothing but `develop`, which the lead merges into both between
rounds and each back after acceptance.

## Rules that bind this track

Every standing rule in `teamlog/2026-09-02-team-1.md` and `open-questions.md` applies:
questions 148, 154 (GMAT's data files are read from the install or a hashed data pack, never
fetched at test time), 156 and 207, 157, 194, 199, 218 (`CARGO_BUILD_JOBS=4`), 220, ADR-002's
rules verbatim (goldens against GMAT with the generator script and the reason committed;
a golden is regenerated only through its script with a stated reason; the integrator is
ours; the fifth amendment's one measured exception), ADR-004's crypto rule (no new crypto
crates; hashing through the platform's OpenSSL path), and the contention rule. A tolerance
is measured and recorded, never asserted from a paper; a force that cannot meet its golden
records the measured residual and the reason, not a loosened tolerance. A worker's last act
is its report.

## Milestones

**N1 Point mass and spherical-harmonic gravity.** `crates/av-orbital`: the Earth field
from GMAT's `.cof` files (JGM2, JGM3, EGM96 to any degree and order the DRM declares),
normalised Legendre recursion stable to degree 70, the body-fixed to inertial rotation
through the frame registry's existing GMAT-validated conversion for now (N5 makes it
native), the acceleration and its partials; a `DynamicsModel` implementing `derivatives`,
`step` with `Dopri5` and `describe` for a single spacecraft. Goldens: a two-body case
against the analytic solution; JGM2 8×8 one day against the existing P0 golden's GMAT
derivative samples and trajectory, tolerance recorded (the P0 spike's 5.7 mm/day is the
reference for what the integrator alone costs); EGM96 70×70 six hours as a new golden with
its generator and reason. Tests: the recursion against published normalised coefficients
at the poles and the equator; degree 2 against the closed-form J2 acceleration.

**N2 Third bodies.** A reader for the JPL DE binary ephemeris files GMAT ships (the
Chebyshev record layout of DE405 and later), positions of the Sun, Moon and planets in
the ICRF at TDB, with the TAI to TDB conversion from ADR-001's time scales; point-mass
perturbations; a golden against GMAT's Sun and Moon one-day run (the P0 golden) and a new
one for Mars and Jupiter as third bodies. Tests: ephemeris positions against GMAT's
reported values at ten epochs to the file's own precision; the record boundary handled
without a discontinuity.

**N3 Solar radiation pressure and drag.** Cannonball SRP with the Sun's position from N2,
a conical shadow model, the spacecraft's `SRPArea` and `Cr` from the DRM; drag with the
Jacchia-Roberts and MSISE-00 densities from GMAT's own space-weather files and the DRM's
`DragArea` and `Cd`, the rotating atmosphere assumption GMAT uses, with the same file
formats; goldens against GMAT's drag and SRP runs (the M5 arc: 7 mm and 3 mm through the
shim) and new ones at 400 km with Jacchia-Roberts and MSISE-00 separately, each generator
recording the space-weather file hash. Tests: density at tabulated altitudes against the
models' published values; the shadow function's penumbra handled continuously.

**N4 The state transition matrix.** The variational equations for every force above (the
gravity gradient, third-body and SRP partials analytically, drag's partials as ADR-002's
third amendment measured them), `StmAugmented` through the same path the GMAT-backed model
uses, so `Kernel::run_with_covariance` accepts either model; goldens against the existing
covariance runs with the determinant and the propagated covariance compared at the
recorded tolerance. Tests: the STM against a finite-difference STM at tight tolerance; the
symplectic property of the two-body STM.

**N5 Native frame reduction.** The inertial to body-fixed rotation natively: IAU-76/FK5
precession and nutation with EOP from GMAT's files, the IAU pole models the frame service
already validates for the Moon and Mars, so the native model no longer calls the convert
shim; pinned against the convert shim's rotation at a thousand epochs with the tolerance
recorded (ADR-002's fourth amendment is the reference). After N5 a kernel built without
`gmat-sys` (a cargo feature) propagates the demo DRM with the native model and the
executor's tests for the native path run in that build.

**N6 Selection, goldens and the C ABI hedge.** The DRM selects the native model by name
beside the GMAT model with identical parameters; a run of the demo DRM with each and the
difference reported in the run products as a score; every golden's generator script,
reason, tolerance and residual tabulated in `goldens/README.md`; the C ABI export of the
native derivative function (question 9's hedge, ADR-002's "portable form") with a test that
a C caller obtains the same derivative bytes; a control-matrix row for the model's
provenance (which data files, which hashes).

## Exit

The demo DRM propagates one day with the native model alone in a kernel built without
GMAT; every force has a golden against GMAT with a recorded tolerance and residual; the
native STM propagates the covariance runs within the recorded tolerance; the native frame
reduction matches the convert shim within the recorded tolerance; the derivative function
is callable over a C ABI. Every number in the status section is traceable to a golden's
generator script and a commit.
