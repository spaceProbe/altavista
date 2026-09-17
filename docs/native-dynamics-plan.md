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

## Status (native-dynamics manager, 2026-09-16) — round 1

N1 is delivered whole and N2 is delivered for the Sun and Moon; question 223's mTLS defect
is fixed and proved. Ten commits on `edge`, each its own task, none pushed.

| Commit | What |
|---|---|
| `f740acd` | question 223: the mTLS test's batch is stamped by the client at submission, not by Python before it spawns |
| `e88e91e` | `crates/av-orbital`: the GMAT-free gravity core — `.cof` reader, normalised Legendre to degree 70, Cunningham/Gottlieb acceleration and partials |
| `e537f7a` | N1's `DynamicsModel`, the GMAT-validated body-fixed rotation behind a cargo feature, the two-body analytic golden |
| `3707d93` | the two new GMAT goldens (`leo_1day_jgm2_8x8`, `leo_6h_egm96_70x70`) and the native model pinned against both |
| `2f61908` | the goldens record the MEASURED native tolerance (they had inherited a 0.05 m default that pinned nothing) |
| `765f9b1` | N2: the DE405 binary reader, TAI→TDB, the Battin third-body perturbation |
| `4dc415b` | the TDB phase fit root-caused and replaced by a derived constant; the crate uses the correct series |
| `4e5056e` | the six Rust SBOMs regenerated for the new workspace member (question 220) |
| `3476dba` | `docs/compliance/BUNDLE.md`'s recorded bundle hash regenerated after that epoch move |
| `7a66cc9` | `test_verify_prebuild_base_image_digest_...` skips visibly when the pinned base image is absent |

### The contract, and what the native model is

`crates/av-orbital` is new. `EarthGravityModel<R: BodyFixedRotation>` implements
`av_dynamics::DynamicsModel` over SI metres and absolute TAI nanoseconds — the same units
`gmat_sys::model::GmatModel` exposes to the kernel, so a DRM can select either (charter
decision 222(a)); `state_dim() == 6`; `describe()` declares the STM capability absent (that is
N4) and its `settings_hash` covers the gravity file's name and SHA-256, the degree and order,
`mu`, the reference radius, the frame names and the integrator settings.

**Integrator settings, as N1 asks to be stated:** `Dopri5::default()` unchanged —
`rtol = atol = 1e-12`, `initial_step = 30 s`, `max_step = 600 s`. No measured reason to deviate.

**The body-fixed rotation, as N1 asks to be stated (charter decision 222(c)):** exactly one
GMAT call per evaluation,

```
gmat.coordinate_system("<ns>_Earth_Inertial", "Earth", "MJ2000Eq");
gmat.coordinate_system("<ns>_Earth_Fixed",    "Earth", "BodyFixed");
gmat.initialize();                                  // once, in ::new
gmat.convert_with_rotation(Tai::from_nanos(t_tai_ns).to_a1_mjd(), &[0.0; 6],
                           "<ns>_Earth_Inertial", "<ns>_Earth_Fixed")
```

taking `rotation` and `rotation_dot`; the inverse is the transpose, so body-fixed→inertial
costs no second call. The coordinate systems are built fresh under a namespaced name, never
GMAT's own `EarthMJ2000Eq` default, exactly as `av-kernel`'s executor does. Measured cost
1.18 µs on a miss, 0.026 µs on a hit of the single-entry cache, which is keyed on the identical
`i64` epoch and never interpolates. `av-orbital` builds and unit-tests with the whole GMAT
path compiled out (`cargo test -p av-orbital --no-default-features`), which is the N5/N6
portability hedge made real now rather than promised.

### Goldens

| Golden | Generator | Reason (short) | Recorded tolerance | Measured residual |
|---|---|---|---|---|
| `twobody_analytic` | `goldens/gen_twobody_analytic.py` | a pure two-body arc has an exact closed-form solution, so it is proved against Kepler's equation, not against GMAT — stated plainly in the file, and explicitly NOT ADR-002's fifth amendment, which is about covariance in a rotating frame | 0.01 m / 1e-5 m/s | 6.827223e-3 m / 7.363563e-6 m/s (circular); 8.355303e-4 m / 3.194748e-7 m/s (e = 0.6) |
| `leo_1day_jgm2_8x8` | `goldens/gen_leo_1day_jgm2_8x8.py` | the P0 arc with the third bodies removed, so N1's gravity-only model has a golden with no force outside its own scope | 6e-3 m / 6e-6 m/s | 4.557868e-3 m / 5.050580e-6 m/s over 86,400 s |
| `leo_6h_egm96_70x70` | `goldens/gen_leo_6h_egm96_70x70.py` | pins the recursion at the 70×70 the crate claims stability to | 5e-4 m / 5e-7 m/s | 2.742649e-4 m / 3.043023e-7 m/s over 21,600 s |
| `tdb_check` | `goldens/gen_tdb_check.py` | 522 samples of GMAT's own `TDB − TT`, so our time scale is pinned against GMAT's and their disagreement is a measurement, not an opinion | see the TDB section | GMAT-phase path: 153.5 ns RMS, 322.8 ns max |
| `leo_1day_jgm2_8x8_sunmoon` (existing, untouched) | `goldens/gen_leo_1day.py` | the P0 spike's own arc | its own 0.05 m stands, unmodified | native: 4.547920e-3 m / 5.039387e-6 m/s |

Every generator records the gravity file's name and SHA-256, the ballistic set actually flown
(question 81: a seed is a vehicle), and the propagator settings read back off
`prop.GetPropagator()` after `PrepareInternals()`.

### What the numbers say

**The P0 spike's 5.7 mm/day is the reference for what `Dopri5` alone costs** (ADR-002, first
amendment: our integrator over GMAT's own derivatives). The native model's one-day residual on
the same class of arc is **4.557868e-3 m — 0.8× that reference.** The reason it is not larger is
measured directly rather than argued: the native acceleration agrees with GMAT's own
`GetDerivatives`, for the identical state and force model, at four epochs across each arc, to

| Force model | max abs | max relative |
|---|---|---|
| JGM2 8×8 | 3.972066e-15 m/s² | 4.698088e-16 |
| EGM96 70×70 | 3.316145e-14 m/s² | 3.922327e-15 |
| JGM2 8×8 + Luna + Sun | 3.972128e-15 m/s² | 4.698162e-16 |

i.e. machine-precision noise. The trajectory residual is therefore integrator-family
disagreement (our `Dopri5` at 1e-12 against GMAT's `PrinceDormand78` at 1e-13), not a
force-model difference, and no root-cause hunt was needed on any of the three arcs.

Supporting measurements, all from test output: the Legendre recursion satisfies the addition
theorem to 1.84e-15 max relative at degree 70 and 1.45e-13 across latitudes; degree 2 against
the closed-form J2 acceleration, 4.176926e-16 max relative; the gravity partials against a
central finite difference, 3.79e-9 max relative at h = 1 m; degree/order (0,0) reproduces the
point-mass acceleration to the last two ulp; the acceleration is continuous over the pole
(pole step / median step = 1 − 1.3e-9 over 2000 steps); the rotation reproduces `Gmat::convert`'s
own converted state bit-exactly at a 7.3e6 m position scale, and a deliberately transposed
rotation changes the acceleration by 154% of signal on an asymmetric synthetic field, so the
direction is proved rather than assumed; `r_dot` against a central finite difference of `r`,
4.478e-12 max abs; two-body energy drift 2.160e-12 relative and |h| drift 7.905e-13 relative
over one day.

### N2: the DE ephemeris, and which file GMAT actually reads

Read off a live GMAT instance, not assumed: `SolarSystem.EphemerisSource = "DE405"`,
`SolarSystem.DEFilename = <install>/data/planetary_ephem/de/leDE1941.405`, and `Earth`, `Luna`
and `Sun` each report `PosVelSource = "DE405"` independently. `SPKFilename` is populated but is
not the active source — a configured path is not evidence of use. The native reader therefore
reads the same file GMAT does.

The DE405 layout was determined from the bytes, and every cross-check is printed by the test
that reads it: `TTL[0] = "JPL Planetary Ephemeris DE405/DE405"`; `AU = 149597870.691` km;
`EMRAT = 81.30056`; `SS = [2430000.5, 2525008.5, 32.0]`, which brackets the golden's epoch;
`KSIZE = 1018` f64 words = 8144 bytes, derived from the pointer table, with
`24195824 % 8144 == 0` and the quotient 2971 = 2 header records + 2969 32-day blocks, matching
`(2525008.5 − 2430000.5) / 32` exactly. The file is pinned by SHA-256
(`f8695149bc54be449788f4d6007d1f6a7053f5be16eae60229dc89c661f6fe4d`) in the test that reads it,
through `openssl::sha::sha256` (ADR-004), as the five `.cof` files already are.

### A GMAT finding: the TDB periodic term's phase

> **Retracted in round 2, 2026-09-17. This subsection is wrong and is kept only as the record
> of what was believed and why.** There is no TDB defect in GMAT. The 288.6879178644° phase is
> produced by `goldens/gen_tdb_check.py` calling `TimeSystemConverter::Convert` with
> `refJd = 0.0`, which no call site inside GMAT does. See `docs/reports/gmat-tdb-phase/REPORT.md`
> and round 2's status section below; the golden, the crate and the tests were corrected in
> commit `7a19455`. The production path was never affected.

The N2 worker could not reproduce GMAT's own `TimeSystemConverter::Convert(..., TDBMJD, ...)`
with GMAT's own exposed `M_E_OFFSET = 357.5277233` and fitted a constant (68.8398465155°)
instead. A fitted constant is a symptom, so the manager root-caused it, and the cause is exact:

> GMAT's TDB periodic-term argument is computed from its internal **Modified** Julian Date
> (2,430,000.0-based) while subtracting the J2000 **Julian** Date constant 2451545.0, so the
> mean-anomaly argument is short by exactly 2,430,000 days of the `M_E_COEFF1` rate — a pure
> constant phase error, which is why a constant absorbed it perfectly.

`2,430,000 × 35999.05034 / 36525 = 2,395,008.687918°`, `mod 360 = 288.6879178644°`, and
`357.5277233 − 288.6879178644 = 68.8398054354°` — 4.108e-5° (about 1.2 ns of `TDB − TT`) from
the fit, i.e. inside the fit's own 153.5 ns RMS. Definitive.

The crate now uses the **correct** series with GMAT's own published offset, and keeps GMAT's
phase available as a derived constant so the disagreement is asserted rather than described —
ADR-002's fifth amendment's pattern, which is the precedent for the platform being deliberately
more complete than GMAT with a test that makes the difference impossible to miss. Measured:
the GMAT-phase path reproduces `goldens/tdb_check.json` to 153.5 ns RMS / 322.8 ns max over 522
samples with the DERIVED constant, and the correct series disagrees with GMAT's report by at
most 1.959386844059527e-3 s (asserted under 2.1e-3 s, with a 1e-4 s floor so the two series
cannot silently collapse together).

**And the change was validated downstream, which is the interesting part.** Switching to the
correct series *reduced* the third-body acceleration disagreement with GMAT's own
`GetDerivatives` from 1.206205e-14 to 3.972128e-15 m/s² — onto the same floor as the
gravity-only arc. A 1.6 ms TDB shift moves the Moon about 1.6 m, which costs about 7e-15 m/s²
of third-body acceleration on this arc (the direct and indirect terms largely cancel), and that
is the size of the improvement observed. So GMAT's *ephemeris* path appears to use a TDB that
its own exposed `TimeSystemConverter` does not: the phase error is in the converter, and our
correct series agrees with what GMAT's forces actually used. The one-day residual against the
P0 golden moved from 4.542078e-3 m to 4.547920e-3 m, well inside the test's own 6e-3 m.

`goldens/leo_1day_jgm2_8x8_sunmoon.json` and its recorded 0.05 m were not touched: that
tolerance pins the `gmat-sys` path, not this one.

### Defects found in review, and their root causes

1. **Question 223's own wording did not describe the code.** `batch_tai_ns` was already stamped
   after every fixture, inside the `with` block. `git log -p -S batch_tai_ns` shows it was never
   stamped earlier in any revision. The real gap — client process spawn, the mTLS handshake
   through nginx, and the `Announce` round trip — was measured at 0.32–0.40 s on an idle host
   against a 5 s window, and grows under this host's routine contention. Fixed at the only place
   "just before submission" exists: the client now accepts `--batch-tai-ns now` and reads the
   clock immediately before it builds and signs the batch, through the same
   `av_cdm::time::Tai::from_utc_nanos` boundary `av-ingest-server --real-clock` uses. Proved by
   running the test with a deliberate `time.sleep(60)` between server/nginx startup and the
   client call: `1 passed, 5 deselected in 100.69s`, and `1 passed ... in 3.06s` without it.
2. **The two new goldens recorded a tolerance that pinned nothing.** They inherited
   `gen_leo_1day.py`'s 0.05 m default while the tolerance actually in force lived in two `const`s
   in the test. Both were regenerated through their own scripts with the reason recorded in the
   file, at the measured tolerance, and the tests now read it from the golden. The GMAT side is
   byte-identical across the regeneration — `initial_state` and `final_state` unchanged — which
   is itself a useful result about GMAT's reproducibility on this host.
3. **The TDB phase fit** — root cause above.
4. **`openssl` was a normal dependency of `av-orbital` while only `#[cfg(test)]` used it.** Moved
   to `dev-dependencies` in review; it came back as a real dependency in `e537f7a` for a stated
   reason (the settings hash covers the gravity file's own bytes at construction time).
5. **`test_verify_prebuild_base_image_digest_...` failed hard on an absent image.** Its skip guard
   checked `docker info` only, but its positive half needs the pinned base image cached, and the
   suite may not pull (question 154). Colima's kubelet image garbage collector had evicted it
   (questions 196(d)/205) — the same collector `test_edge_plugin_hardening_alpine.py` already
   guards against. Now a visible skip naming the image and the `docker pull` that restores it.
6. **Not a defect, corrected in a comment.** N2 reported that the "widely-quoted"
   `q(3+3q+q²)/(1+(1+q)^1.5)` disagrees with `1−(1+q)^−1.5` by ~1%. With `u = (1+q)^{3/2}` the
   first equals `u−1` exactly and the second `(u−1)/u`; they differ by the factor `u` and are
   different quantities, not a right and a wrong one. The implementation was already validated
   against the naive difference form and against GMAT; only the comment was wrong.

### Decisions taken

1. `av-orbital` depends on `gmat-sys` only through an optional, default-on `gmat-frames` cargo
   feature, with the rotation behind a `BodyFixedRotation` trait — so N5 is a drop-in and the
   `--no-default-features` build is a gate from today, not from N5.
2. The two-body golden is proved against the analytic Kepler solution, which is what N1 asks for;
   the file says so in its own `reason` and says explicitly that this is not the fifth amendment's
   exception. Every other force still gets its golden against GMAT.
3. The tolerance committed with a golden is the tolerance in force, and the test reads it from the
   file rather than carrying a copy.
4. The native model uses the correct TDB series, not GMAT's phase, with the disagreement pinned by
   a test — ADR-002's fifth amendment's precedent, and the downstream measurement supports it.
5. New time-scale code lives in `av-orbital`, not in `av-cdm`: `av-cdm` is shared with other
   tracks and is not this track's to extend mid-round.
6. The SBOMs and `BUNDLE.md` are regenerated on this branch so the track's own gate is green;
   question 220's rule still puts the merge-time regeneration with the lead.

### Gates (manager, no worker active)

| Gate | Result |
|---|---|
| `cargo test -p av-orbital -p av-dynamics` | 99 passed, 0 failed (36 av-dynamics, 46 av-orbital lib, 4 `frame_gmat`, 4 `gravity_goldens`, 4 `tdb_check`, 2 `thirdbody_goldens`, 3 `twobody_golden`) |
| `cargo test --workspace --exclude av-kernel --no-fail-fast` | 1174 passed, 0 failed, 3 ignored, across 127 result lines |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean, zero warnings, no `#[allow]` added anywhere in `av-orbital` |
| `cargo deny check` | advisories ok, bans ok, licenses ok, sources ok; exactly the six spoore wildcard warnings (question 207) |
| `.venv/bin/python -m pytest -q -rs` | 767 passed, 1 failed, 16 skipped — the one failure is question 207's cross-track Docker collision, re-run alone and green (below) |

Full outputs are under the manager's scratchpad as `GATE1`–`GATE5`.

The first run of the python gate had three failures, all three understood and all three now
fixed or accounted for: the two `BUNDLE.md` hash tests (the SBOM epoch move, commit `3476dba`)
and `test_verify_prebuild_base_image_digest_...` (the skip-guard defect, commit `7a66cc9`). The
final run's single remaining failure is
`services/cfs/tests/test_image_digest.py::test_image_digest_matches_recorded_value`
("No such image: altavista-cfs-lockstep:local"). That test is correctly guarded by a
module-level `_SKIP_REASON` computed at import, so the image existed when the run started and
was removed while it ran: `ps` during the gate showed the heavy track's own
`/Users/probe/code/AltaVista-aiplane` worktree running `pytest -q -rs` and `cargo build`
concurrently, and question 207 records exactly this — `prune_stale_test_resources` is
daemon-wide while its lock is process-local, so two worktrees' docker tests tear out each
other's labelled resources, and "a contended docker failure is re-run alone before it is
believed". Re-run alone: `services/cfs/tests/test_image_digest.py` → **3 passed, 2 skipped**,
exit 0. Nothing in this round touches `services/cfs`.

### Open items

- Whether GMAT's `TimeSystemConverter` TDB phase is worth reporting upstream — a sibling of
  question 150. The evidence here is arithmetic-exact and would make a short, complete report.
- N2's Mars-and-Jupiter golden is not built; the reader supports the bodies generically but they
  are not exercised.
- N3 (SRP and drag), N4 (the STM) and N5 (native frame reduction) are untouched.
- `goldens/README.md` still does not exist; N6 is where every golden's generator, reason,
  tolerance and residual gets tabulated in one place.

## Status (native-dynamics manager, 2026-09-17) — round 2 (PAUSED)

**The user stopped work mid-gate on 2026-09-17. Every chartered task is committed; nothing was
in flight and no worker was running when the round was paused; the working tree is clean apart
from this file. What remains is the gate run, itemised below.**

N2 is closed, N3 and N4 are delivered, question 224 is done, and round 1's headline GMAT
finding is retracted: it was ours. Nine commits on `edge`, each its own task, none pushed.

| Commit | What |
|---|---|
| `1c2b4df` | `docs/reports/gmat-tdb-phase/`: the commissioned upstream report, written, run, and returning a negative result |
| `7a19455` | the TDB golden regenerated through its own script with the correct `refJd`; the phase story removed from the crate |
| `929d601` | N2's remaining golden: Mars and Jupiter as third bodies, with its generator and four tests |
| `561d005` | the DE lookup gets a two-part TDB epoch; the ten-epoch ephemeris disagreement drops eighty-fold |
| `3065f5e` | N3a: cannonball SRP with GMAT's own conical shadow, its golden and three tests |
| `886e9df` | N3b: the CSSI space-weather reader, drag with GMAT's rotating atmosphere, Jacchia-Roberts, and the 400 km golden |
| `e081dcf` | N3c: the M5 drag+SRP arc flown natively, and MSISE90 with its own 400 km golden |
| `101b81e` | N4: the native STM, the variational equations, and the covariance comparison |
| `6fe41d7` | question 224: the Python SBOM's input becomes a committed lock file, the venv only cross-checks |

### The retraction, first, because it changes what question 225 records

Question 225 commissioned an upstream report on a TDB periodic-term phase error in GMAT's
`TimeSystemConverter`, a sibling of question 150. The report was written and the reproduction
was run. **It does not reproduce, and the defect is ours.**

`Sat.TDBModJulian − Sat.TTModJulian`, read from a plain GMAT script at five epochs from 2020 to
2033, matches the correct undisplaced series to 6.023e-7 s — the `ReportFile` text-precision
floor — and never comes within 1.629e-3 s of the phase-shifted series round 1 attributed to
GMAT. The 288.6879178644° phase is real and exactly reproducible, but only by calling
`TimeSystemConverter::Convert(value, from, to, refJd)` with `refJd = 0.0`. That is not
`Convert()`'s own default (`GmatTimeConstants::JD_JAN_5_1941`, 2,430,000.0) and not what any
call site in GMAT's R2026a source passes. The source, present on this host at
`third_party/gmat-src`, shows why exactly:

```cpp
Real tttOffset = T_TT_OFFSET - refJd;
Real t_TT = (origValue - tttOffset) / T_TT_COEFF1;
```

so a caller handing `Convert` a GMAT modified Julian date while passing `refJd = 0` leaves the
mean-anomaly argument short by 2,430,000 days of the `M_E_COEFF1` rate. `goldens/gen_tdb_check.py`
passed that `0.0`; it is the only call site found anywhere that does. `Sat.TDBModJulian`
(`TimeData.cpp`) and GMAT's own DE ephemeris reader (`DeFile::GetPosVel`) both pass the default.

That also dissolves question 225's further observation. GMAT's ephemeris path does not use a TDB
its converter withholds — it uses the same correct series the converter returns, which is exactly
why round 1's switch to the correct series improved the native third-body agreement.

The report directory is kept as a negative result and says so at its top; nothing in it is
proposed for submission. The repository's own artifacts were corrected in `7a19455`: the
generator passes `2430000.0` explicitly, the golden was regenerated through its own script with
the reason recorded, and `tai_ns_to_tdb_minus_tt_seconds_gmat_phase` and
`GMAT_M_E_PHASE_SHIFT_DEG` are gone — they existed only to reproduce a GMAT behaviour that does
not exist. **Round 1's decision 4 is superseded**, and round 1's "A GMAT finding" subsection now
carries a retraction pointer rather than being rewritten.

What is left of that comparison is measured and root-caused rather than described: over all 526
fixture points the native series and GMAT's agree to 146.1 ns RMS and 321.8 ns max, and **every
single residual is at most 0.531 of its own epoch's f64 ULP** — the golden records
`tdb_minus_tt_s` as the difference of two MJD-magnitude doubles, whose ULP is 314.3 ns below the
32768-day binade and twice that above. The test asserts that ratio stays under 1.0, which pins
far more than the 5e-7 s absolute bound. The one genuine difference is 10.80 ns: GMAT forms the
series argument from the TAI modified Julian date, this module from the TT one, which is the
argument the series is defined on.

### Goldens

| Golden | Generator | Reason (short) | Recorded tolerance | Measured residual |
|---|---|---|---|---|
| `tdb_check` (regenerated) | `goldens/gen_tdb_check.py` | the previous revision called `Convert` with `refJd = 0.0`; regenerated with GMAT's own default so the fixture records what GMAT actually computes | `tolerance_s` 5e-7 s, in the file | 146.1 ns RMS, 321.8 ns max over 526 points; every residual ≤ 0.531 ULP of its own epoch |
| `leo_1day_jgm2_8x8_mars_jupiter` | `goldens/gen_leo_1day_jgm2_8x8_mars_jupiter.py` | the planets' DE records are barycentric while the Moon's is geocentric, so this arc exercises a path the Sun/Moon golden cannot | 6e-3 m / 6e-6 m/s; ephemeris 0.03 m and 1e-13 relative, all in the file | 4.559312e-3 m / 5.052095e-6 m/s; ephemeris Mars 1.757690e-2 m (4.874442e-14), Jupiter 5.438822e-3 m (8.572570e-15) |
| `leo_1day_jgm2_8x8_sunmoon_srp` | `goldens/gen_leo_1day_jgm2_8x8_sunmoon_srp.py` | SRP is the only force added relative to a golden that already exists, and the arc enters the shadow (37.01 % umbra, 0.28 % penumbra) | 4e-3 m / 4e-6 m/s, in the file | 3.168026e-3 m / 3.549427e-6 m/s |
| `leo_400km_jacchia_roberts` | `goldens/gen_leo_400km_jacchia_roberts.py` | drag alone, at GMAT's own constant-weather defaults, on a gravity arc class already pinned | 10 m / 1e-2 m/s, in the file | 3.490744 m / 3.951549e-3 m/s |
| `leo_400km_msise90` | `goldens/gen_leo_400km_msise90.py` | the second atmosphere on identical geometry, so the two are comparable | 10 m / 1e-2 m/s, in the file | 3.6947 m / 4.1826e-3 m/s |
| `leo_1day_jgm2_8x8_sunmoon_drag_srp` (existing, untouched) | `goldens/gen_leo_1day.py --drag-srp` | the M5 arc; its 0.05 m pins the `gmat-sys` path, not this one | unchanged, never asserted against | native 68.32 m / 0.0803 m/s — see below |
| `leo_1day_jgm2_8x8_sunmoon` (existing, untouched) | `goldens/gen_leo_1day.py` | the P0 arc, and its `stm` block is GMAT's own 42-state run | unchanged | native STM 6.392e-5 max abs / 5.580e-7 max relative; covariance 5.749e-10 relative Frobenius; `det Φ` 1.000000000103 vs GMAT's 1.000000000091 |

Every generator records the gravity file's name and SHA-256, the DE file's, and — for the drag
goldens — the space-weather file's, plus the ballistic set read back off the spacecraft
(question 81) and the propagator settings read back off `prop.GetPropagator()` after
`PrepareInternals()`.

### What the numbers say

**The forces agree with GMAT at the level the force model allows, and the disagreements are
attributed.** Native acceleration against GMAT's own `GetDerivatives`, identical state and force
model, debug build on this host:

| Force model | max abs | max relative |
|---|---|---|
| JGM2 8×8 + Mars + Jupiter | 4.528849e-15 m/s² | 5.356641e-16 |
| JGM2 8×8 + Sun + Moon + SRP | 6.404746e-15 m/s² | 7.033944e-16 |
| the same, at a penumbra epoch (ν = 0.005254) | 4.788138e-15 m/s² | 5.264756e-16 |
| JGM2 8×8 + Jacchia-Roberts drag | 3.855085e-10 m/s² | 4.436967e-11 |
| JGM2 8×8 + MSISE90 drag | 3.785e-10 m/s² | 4.356e-11 |
| JGM2 8×8 + Sun + Moon + drag + SRP (the M5 arc) | 7.659e-9 m/s² | 8.412e-10 |

Gravity, third bodies and SRP sit on the machine-precision floor round 1 established. Drag is six
orders worse, and that is the atmosphere, not the plumbing.

**The M5 arc is the honest number of this round.** The native model flies it to **68.32 m /
0.0803 m/s** against the `gmat-sys` shim path's 7 mm / 3 mm/s. The golden's own 0.05 m was not
touched and is not asserted against; the test asserts a measured-plus-margin 110 m bound and says
so in its own doc comment. The cause is arithmetic, not mystery: Jacchia-Roberts' density
disagreement at this arc's 250 km is about 2.0e-4 relative, which on a 6.45e-5 m/s² drag
acceleration predicts 1.29e-8 m/s², the order measured, and integrated as a constant bias over
the day predicts about 48 m — roughly 70 % of what was seen. A second check: `dv/dr` =
1.176e-3 rad/s reproduces the orbit's own mean motion 1.170e-3 rad/s to one percent, the signature
of a coherent secular phase error rather than integrator noise. The remaining 30 % is open.

**And the two atmospheres disagree about how well they agree, which is the round's most useful
open finding.** By the same harness, on the same altitude ladder, MSISE90's density matches
GMAT's to **1.538e-6** max relative while Jacchia-Roberts matches only to **1.407e-3**. Three
orders apart on identical machinery is evidence that the Jacchia-Roberts port carries a real
defect rather than that the model class is imprecise, and the M5 residual is what that defect
costs. It is recorded with a definite next step, not closed.

**N4 lands on the depth-2 reference points while computing everything independently.** The STM
against a finite-difference STM: 1.290e-6 max abs, 4.048e-9 max relative. The two-body STM's
symplectic property: max |ΦᵀJΦ − J| = 5.873e-9. Against GMAT's own 42-state propagation of the P0
arc: 6.392e-5 max abs and 5.580e-7 max relative on the 36 elements, essentially ADR-002's second
amendment's own 6.3e-5 / 5.5e-7 for the `gmat-sys` path — with an A-matrix that never touches
GMAT. The propagated covariance agrees with the recorded `cov_t1_si` to 5.749e-10 relative
Frobenius through `av_dynamics::propagate_covariance`, whose pre-symmetrization asymmetry was
1.19e-7.

Supporting measurements, all from test output: the gravity gradient's rotation direction is
proved, not assumed — `RᵀGR` gives 6.29e-10 relative against a central difference on a
deliberately asymmetric field while the reversed `RGRᵀ` is off by 96 % of signal; the third-body
partials 6.29e-9 max relative; SRP's velocity block is **exactly** zero, pinned bit-for-bit;
drag's velocity block 2.39e-10 relative; the conical shadow is continuous through both boundaries
and ν is exactly 1 in full sun and exactly 0 in umbra; the penumbra crossing this arc makes is
9.000 s wide.

### Both finite-difference steps were swept, not chosen

Drag's position-block step: `h` from 0.5 m to 1e4 m against a Richardson-extrapolated reference,
a flat near-optimal region at 0.5–1 m (8.72e-7 and 8.85e-7 relative), growing monotonically past
about 10 m to 3.3e-3 at 1e4 m; **1 m recorded**. The finite-difference STM's own step: a classic V
with its minimum at a relative fraction of 1e-5.

### Two things GMAT does that had to be measured rather than assumed

**Drag's weather source.** `DragForce`'s own C++ constructor defaults `HistoricWeatherSource` and
`PredictedWeatherSource` to `ConstantFluxAndGeoMag` with `F107 = 150`, `F107A = 150`,
`MagneticIndex = 3`, and a live readback after `Initialize()` reproduced exactly those. GMAT never
reads the CSSI file's *values* unless a DRM sets those fields, though `Initialize()` does validate
the file exists. The drag goldens therefore fly GMAT's own constants on both sides and record
them. GMAT *can* be driven to read the file headlessly — both fields set to
`CSSISpaceWeatherFile` initialises clean and moves the density by 2.6 % at an epoch inside the
file's daily-predicted section.

**Drag's relative velocity.** From `DragForce::Accelerate` and `AtmosphereModel.cpp`:
`v_rel = v − ω × r` with ω a **scalar z-axis rate** 7.29211585530e-5 rad/s applied to the
**inertial** state, while the density is evaluated at the true body-fixed position through GMAT's
own `CoordinateConverter`. Two fidelities for two terms of one force; both are documented in the
modules rather than smoothed over.

### `av-dynamics` was not touched

Round 1 reserved the right to extend `crates/av-dynamics` additively where the contract lacked a
hook. **It never needed one.** `StmAugmented`, `propagate_covariance` and the trait's own
`stm_derivatives`/`step_with_stm` defaults were exactly sufficient: `EarthGravityModel` overrides
`stm_capable`, `stm_derivatives` and `describe()` inside its own impl block in `av-orbital`, and
`StmAugmented` wraps it and propagates with `Φ(t0,t0)` the exact identity — the path
`Kernel::run_with_covariance` relies on, exercised by a test in `av-orbital`. The kernel registry
is untouched; question 225's additive `ModelKind` is N6's, not this round's.

### Defects found in review, and their root causes

1. **The TDB phase was ours, not GMAT's** — root cause above, definitive, arithmetic-exact.
2. **The ten-epoch ephemeris check disagreed with GMAT by 1.4 m** (Mars) and 0.44 m (Jupiter) from
   the same DE file — four orders above the f64 floor at those magnitudes. The first explanation
   offered was cancellation in the barycentric subtraction, which is not credible when the
   differenced quantities are all the order of the result. Root cause, definitive:
   `tai_ns_to_tdb_jd` returned a full Julian Date as one f64, whose ULP at ~2.46e6 is 40.2 µs
   measured directly, and that value was the DE lookup's epoch. Two facts prove it: the
   disagreement vector is parallel to each body's geocentric velocity (cos θ = ±1.000000), and the
   implied time offset is **the same for Mars and Jupiter at the same epoch** to a few nanoseconds
   — an epoch error is a property of the epoch, not of the body. That equality also falsifies the
   EMRAT-split and light-time candidates without a separate run, since those scale with each
   body's own distance. Fixed with a two-part SOFA/ERFA-style epoch; eighty-fold improvement.
3. **The SRP golden was committed with a 100 m placeholder tolerance** left from the first pass of
   the measure-then-record loop. A 100 m tolerance on a 500 km LEO arc pins nothing — round 1's own
   review lesson, recurring. Regenerated through its own script with the measured number.
4. **`penumbra_epoch_acceleration_agreement` reported no penumbra on an arc that demonstrably
   enters one.** Not the shadow function: a 0.25 s sweep across the real bracket printed ν rising
   smoothly 0.0046, 0.0172, 0.101 … 0.969, 1.0. The search was at fault — the bisection abandoned
   the bracket once it fell under one second without ever testing a midpoint inside it, and the
   crossing is 9.0 s wide. Floor lowered to 10 ms.
5. **`densu` implemented only its `alt ≥ za` branch**, on the reasoning that the spline branch was
   dead given the module's floor. It returned NaN at every test altitude. `GTS6`'s turbopause
   sub-calls pass `densu` a species-specific reference altitude near 100–110 km, below `za`
   whatever the spacecraft's altitude, so the spline branch runs on nearly every call.
6. **The MSISE90 density test copied `drag_goldens.rs`'s `IdentityRotation` mock**, valid for
   Jacchia-Roberts, which has no longitude dependence, and invalid for MSISE90, whose density
   depends on body-fixed longitude through local solar time. It measured 4.5–73 % disagreement
   growing with altitude until the real rotation was used, after which it fell to 1.5e-6.
7. **Question 224's `setuptools` half was a real dependency gap**, not a venv quirk: `setuptools`
   is required by `grpcio-tools`' own `Requires-Dist` through the `dev` extra. The `altavista`
   stale-dist-info half was never a real disagreement — both records always reported `Apache-2.0`
   — so the whole defect was the one structural thing, that the package list was whatever the venv
   had.
8. **Not a defect, a host finding, and it cost this round real time.** Every freshly linked test
   binary blocks in `_dyld_start` at 0 % CPU for one to three minutes while macOS `syspolicyd`
   evaluates it; `sample` shows the whole stack in dyld and `syspolicyd` at 100–134 % CPU
   throughout. It is not contention between the tracks and it is not any track's code. Killing and
   rebuilding makes it worse, since that produces a new binary to scan.

### Decisions taken

1. **The TDB report is published as a negative result and nothing is submitted upstream.** The
   report directory is kept, with a header saying so, because the claim, the reproduction that
   tested it, and the reason it failed all belong on the record. Round 1's decision 4 is
   superseded.
2. **The correct series stays, and the crate keeps TT as the series argument** rather than GMAT's
   TAI, a deliberate 10.80 ns difference, because TT is the argument the series is defined on.
3. **A two-part TDB epoch for the ephemeris lookup**, `tai_ns_to_tdb_jd` kept beside it. The
   remaining floor is `av_cdm::time::Tai::to_a1_mjd`'s own ~600 ns f64 ULP, about 3 cm at Mars'
   geocentric speed. `av-cdm` is shared and was not opened this round (round 1's decision 5 still
   stands); it is the lead's item.
4. **MSISE90, not MSISE-00.** GMAT R2026a does not ship NRLMSISE-00 — confirmed twice, by symbols
   in the shipped `libGmatBase.dylib` and by `msise90_sub.for` exposing `GTD6`/`GTS6`/`GLOBE6`,
   the 1990 naming that NRLMSISE-00 renamed `GTD7`/`GTS7`/`GLOB7S`. Charter 222(d) requires every
   force to have a golden against GMAT, and a native NRLMSISE-00 would have no reference on this
   host. The plan's N3 wording is not followed, and the module says so.
5. **The drag goldens fly GMAT's own constant weather** because that is what GMAT actually uses by
   default, so the comparison is like for like; the file reader is built and pinned by hash
   regardless, since N3 asks for it and N5/N6 will need it.
6. **No second, file-weather golden was built.** GMAT's `SolarFluxReader` source is not in this
   host's mirrored tree, so the record-selection convention (which calendar day, which 3-hourly
   slot, any processing lag) could not be verified from source. Building a golden on an unverified
   convention would pin the approximation rather than the model. The reader documents the gap and
   its same-day, first-slot approximation agrees with GMAT's own file mode at the same 0.1–0.3 %
   level as constant mode.
7. **The M5 arc's tolerance is a measured bound in the test, not a change to the golden.** The
   golden's 0.05 m pins the `gmat-sys` path and was left alone, exactly as round 1 left the plain
   arc's.
8. **Drag's SRP-style analytic position block was not attempted.** Its velocity block is analytic
   and its position block is a central finite difference, which is what ADR-002's third amendment
   records GMAT itself doing for the whole drag block; matching GMAT's own shape is worth more
   than being more analytic than the reference.
9. **Question 224 takes the committed-lock-file route**, not pyproject-resolved-against-wheels:
   the wheels route is network-gated at kit-build time and lands in the gitignored `out/`, so it
   is available neither at generation time nor at test time, and it would not give every package a
   committed licence source. The lock file joins the SBOM's epoch paths so question 220's
   merge-time regeneration stays deterministic.
10. **`pip` is the one named cross-check exception**, encoded as `PYTHON_CROSS_CHECK_IGNORED`, and
    justified by checking that nothing in the graph requires it. Dev-extra packages are part of
    the declared set, not an exception, because the committed SBOM is by standing instruction
    generated from a `pip install -e ".[dev]"` venv.

### Gates (manager, no worker active) — PARTIAL, the round was paused mid-gate

The round was paused by the user before the gate run finished. Every commit was verified
individually before it was made (the per-task runs are quoted in each commit message); the
round-wide gate reached gate 3 before it was stopped.

| Gate | Result |
|---|---|
| `cargo test -p av-orbital -p av-dynamics` | **189 passed, 0 failed** (36 av-dynamics; av-orbital: 119 lib, 4 `drag_goldens`, 2 `drag_srp_m5`, 4 `frame_gmat`, 4 `gravity_goldens`, 4 `msise90_goldens`, 3 `srp_goldens`, 1 `stm_goldens`, 3 `tdb_check`, 2 `thirdbody_goldens`, 4 `thirdbody_mars_jupiter`, 3 `twobody_golden`) |
| `cargo test -p av-orbital --no-default-features` | **125 passed, 0 failed** — the whole native force model, STM included, with no GMAT linked |
| `cargo test --workspace --exclude av-kernel --no-fail-fast` | **incomplete: 1262 passed, 0 failed, 3 ignored when the run was stopped.** It was still running `av-store`'s docker-gated `minio_store` suite, which had been alive over ten minutes — question 207's cross-track Docker contention, another track's crate, nothing this round touches |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean at `101b81e`, the last commit that changed Rust; the question-224 commit after it changes no Rust. Zero warnings, no `#[allow]` anywhere in `av-orbital` |
| `cargo deny check` | **not run this round.** No dependency was added by any commit — `av-orbital`'s `Cargo.toml` is unchanged since round 1 and no new crate entered `Cargo.lock` |
| `.venv/bin/python -m pytest -q -rs` | **not run round-wide.** `tests/test_sbom.py` alone: 61 passed, 6 skipped (the skips are the opt-in `AV_SBOM_REBUILD` Rust rebuild, visible and named). The question-224 worker measured the full suite at 770 passed / 0 failed / 18 skipped before its change and 774 passed / 0 failed / 16 skipped after |

Full outputs are under the manager's scratchpad as `GATE1`–`GATE3`, `CLIPPY6` and `PYSBOM`.

**The gates the lead must run before accepting** are therefore: the workspace test gate to
completion, `cargo deny check`, and the full `pytest -q -rs`.

### A host finding the lead should have, because it cost this round hours

Every freshly linked test binary on this host blocks in `_dyld_start` at **0 % CPU** for one to
three minutes while macOS `syspolicyd` evaluates it; `sample` shows the entire stack in dyld
before any of our code runs, and `syspolicyd` sits at 27–134 % CPU throughout. It is **not**
contention between the two tracks — it happened at load average 2.5 — and it is not any track's
code. Killing and rebuilding makes it worse, since that produces a new binary to scan. Every
worker this round hit it and two of them mis-attributed it to cross-track load before it was
root-caused. Changing the host's security settings is not this track's to do; the lead should
decide whether to raise it with the user.

### Open items

- **The Jacchia-Roberts port almost certainly carries a defect.** On the same altitude ladder
  through the same harness, MSISE90 matches GMAT's density to 1.538e-6 while Jacchia-Roberts
  matches only to 1.407e-3. Three orders apart on identical machinery is a defect signal, not a
  property of the model class, and it is what the M5 arc's 68 m residual is made of. Next step is
  concrete: compare the two implementations' intermediate quantities term by term against GMAT's,
  starting with the exospheric temperature.
- **The M5 residual is 70 % explained.** The remaining 30 %, and the exact velocity residual, are
  unattributed.
- **`av_cdm::time::Tai::to_a1_mjd`'s ~600 ns f64 ULP is now the ephemeris floor** — about 3 cm at
  Mars' geocentric speed, after the two-part TDB epoch removed the 40 µs one. `av-cdm` is shared
  and was not opened this round; it is the lead's.
- **Question 225 needs correcting**, and the lead has said it is doing so: there is no GMAT TDB
  defect, and the "GMAT's ephemeris path appears not to use the TDB its converter exposes"
  observation dissolves with it.
- **No file-weather drag golden**, for the stated reason: GMAT's `SolarFluxReader` source is not
  in this host's mirrored tree, so its record-selection convention could not be verified.
- **MSISE90's floor is `za`, about 122.8 km**, not `GTD6`'s own 72.5 km dispatch boundary, and
  neither atmosphere ports `GTD6`'s lower-mesosphere blend below that.
- **N5 (native frame reduction) and N6 (selection, the goldens table, the C ABI hedge) are
  untouched.** `goldens/README.md` still does not exist.
- The STM comparison's tolerances live in `tests/stm_goldens.rs`'s own constants rather than in a
  golden file, because it reuses round 1's `leo_1day_jgm2_8x8_sunmoon.json`, whose recorded
  tolerance pins the `gmat-sys` path and was deliberately left alone.
