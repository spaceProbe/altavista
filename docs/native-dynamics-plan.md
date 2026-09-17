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
