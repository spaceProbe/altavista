# av-kernel

The deterministic kernel skeleton (M2.1): a TAI-nanosecond clock authority, a multi-rate
scheduler over `av_dynamics::DynamicsModel` systems with Hermite-with-velocity interpolated
output, and CDM v1 `Trajectory` assembly. Lockstep only -- ADR-005 (the runtime this
eventually plugs into: bindings, port router, container/Renode) is still **Planned** and not
yet drafted anywhere in `docs/adr/`, and nothing of it beyond "lockstep" is invented here. As
of M6.1, this crate also hosts the **DRM executor** ([`drm`](#the-drm-executor-drm-m61)):
load a `DesignReferenceMission` + `SosConfiguration` + `SystemDefinition`s, verify their
canonical hashes, bind every `BINDING_KIND_MODEL` instance (and, as of M13.2, every
`BINDING_KIND_CONTAINER` instance -- see ["Container (lockstep)
binding"](#container-lockstep-binding-m132-question-107) below), honour `DrmOptions`, inject
`Scenario` faults and (M10.1, question 97) impulsive maneuvers, and emit `Trajectory`s and
(M9.3/M10.1) real `Event`s, all with `Provenance`.

## Module layout

- **`clock`** -- `Clock`: an integer TAI-nanosecond counter that only advances by an
  explicit `tick()`. Never reads the wall clock.
- **`interpolate`** -- `hermite_velocity`: cubic Hermite interpolation using each sample's
  velocity components, matching `INTERPOLATION_HERMITE_VELOCITY`
  (`proto/altavista/v1/trajectory.proto`) by formula, not merely by name.
- **`schedule`** -- `Scheduler<M>`: several systems of one `DynamicsModel` type `M`, each
  stepped at its own declared period, all driven off one clock (ADR-002 "Rates and
  pacing").
- **`trajectory`** -- `build_trajectory`: assembles one CDM v1 `Trajectory` from a system's
  output samples.
- **`kernel`** -- `Kernel<M>`: ties the above together -- register systems, `run` from a
  start to an end instant, get back one `Trajectory` per system. `HeteroKernel` (ADR-005 sec
  1-2) is the trait-object counterpart: several systems, each its **own** `DynamicsModel`
  Rust type (`av_dynamics::BoxedModel`), one clock -- the type `drm::executor::execute` now
  drives exclusively (M9.1, see below).
- **`registry`**/**`schedule`** -- `ModelRegistry` (`SystemDefinition.dynamics_model` id ->
  constructor -- **the sole constructor as of M10.3**, question 98: see ["The DRM
  executor"](#the-drm-executor-drm-m61) below) and `HeteroScheduler` (the base-period clock
  gate, ADR-005 sec 2) that `HeteroKernel` is built on.
- **`expr`** (M8.2, corrected M9.1) -- the ADR-005 sec 6 expression language `Objective`/
  `MeasureOfEffectiveness.expression` are written in: hand-written lexer/parser (no
  third-party parser crate), unit-checked before evaluation, evaluated against
  `ExprRunProducts` (trajectories, events, outputs). See ["Scoring"](#scoring-the-expression-
  language-and-runproducts-expr-adr-005-sec-6-m91) below.
- **`drm`** (M6.1, driven by `HeteroKernel` as of M9.1) -- the DRM executor: `schema` (YAML
  authoring format), `hash` (canonical SHA-256), `binding` (bind a `SystemInstance` to a real
  `DynamicsModel`), `fault` (DYNAMICS fault injection), `maneuver` (M10.1, question 97 --
  typed `Scenario.events` of kind `"maneuver"` and the RIC/VNB/VVLH/inertial delta-v frame
  transform), `events` (M9.3/M10.1 -- the CDM `Event`s `execute` emits), `executor`
  (`execute`, the end-to-end entry point, returning `RunProducts` -- trajectories, real
  events, evaluated `scores`, provenance). See ["The DRM executor"](#the-drm-executor-drm-m61)
  below.
- **`ports`**/**`router`** (`docs/open-questions.md` question 108, M13.1; question 110, M14.2)
  -- `router::Router`: `SosConfiguration.connections` validated against every named instance's
  declared `SystemDefinition.ports` and realized as a routing table, plus the run-time
  pending-delivery queues that now actually **hold** a message until its own availability
  epoch (emission plus latency) is reached, delivering it at the first receiver step whose own
  epoch is at or after that -- never earlier, never lost, see "Latency actually defers
  delivery" below; `ports::{QueuedMessage, sorted_inbox}` the delivery-order machinery it is
  built on (`Inbox`/`Outbox`/`PortMessage` themselves are re-exported from `av_dynamics`, which
  owns them -- see that crate's README). GMAT-free, like `clock`/`interpolate`/`schedule`/
  `kernel`/`trajectory`. See ["Ports and the router"](#ports-and-the-router-docs-open-
  questionsmd-question-108-m131-question-110-m142) below.

## GMAT dependency: everywhere except `clock`/`interpolate`/`schedule`/`kernel`/`trajectory`

`clock`, `interpolate`, `schedule`, `kernel` and `trajectory` depend only on `av-cdm` and
`av-dynamics`, neither of which touches GMAT. Every unit test in those five modules runs
against a small synthetic `DynamicsModel` (constant acceleration, closed-form solution known)
and needs no GMAT install:

```bash
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cargo build -p av-kernel --lib   # builds with no GMAT_ROOT/GMAT_LIB set
cargo test -p av-kernel --lib    # 243 tests total (as of M14.1); every one of them GMAT-free (see below)
```

`tests/golden_acceptance.rs` drives a real `gmat_sys::model::GmatModel` through this crate's
`Kernel` and checks the result against `goldens/leo_1day_jgm2_8x8_sunmoon.json`. That test
takes `gmat_sys::engine_lock()` first, per this repository's existing convention, and a small
`build.rs` in this crate re-emits the `-rpath` linker argument `gmat-sys`'s own build script
sets for its own targets -- Cargo does not propagate `rustc-link-arg` (unlike
`rustc-link-lib`/`rustc-link-search`, which do propagate via the `links` key) across a
package boundary, so without this the acceptance test's binary would link successfully but
abort at run time unable to find `libGmatBase.R2026a.dylib`. This `build.rs` is a **soft**
dependency: if no GMAT install is found (`GMAT_ROOT`/`GMAT_LIB` unset and the default path
missing) it emits nothing, and the library and every non-GMAT test still build.

**`gmat-sys` is no longer a dev-dependency (M6.1).** `drm::binding` binds a
`"gmat."`-dispatched `BINDING_KIND_MODEL` instance to a real `GmatModel` -- production code,
not a test -- so `gmat-sys` moved to `[dependencies]`. This does **not** change the paragraph
above: `clock`/`interpolate`/`schedule`/`kernel`/`trajectory` still never call into GMAT, and
most of `drm` doesn't either -- only `drm::binding::materialize_gmat` (and, transitively,
`drm::executor::execute` when a DRM actually binds a `"gmat."` instance) touches the engine.
Every one of `drm`'s 55 lib-level unit tests above (18 of them in `drm::maneuver`, M10.1; 2 new
in `drm::binding` for M10.3's `output.*` allowlist and `AnyModel::step` delegation) runs
with no GMAT install; only `tests/drm_executor.rs`'s and `tests/drm_maneuver.rs`'s
golden-reproduction tests need one (see below).

## The multi-rate scheduler

`Scheduler::register(id, period_ns, model, t0_tai_ns, initial_state)` declares one system's
step rate; `advance_to(target_tai_ns)` steps every registered system, in id order
(`BTreeMap`), until each has a native step at or past `target_tai_ns` -- a system at 50 Hz
steps five times for every one step a 10 Hz system takes to cover the same span, and neither
system's own step boundaries depend on what the other system's rate is or what the caller
happens to be sampling at. `sample(id, t_tai_ns)` returns the exact state if `t_tai_ns` is one
of that system's own last two native step times, or the Hermite-with-velocity interpolation
between them otherwise -- and a typed `OutOfRange` error if `t_tai_ns` falls outside the
system's current history window (this scheduler **interpolates, it never extrapolates**).

Verified in `schedule`'s unit tests: two systems at 50 Hz and 10 Hz, driven by one `Clock` to
the same target instant, both land on the closed-form answer for their own (different)
constant accelerations; sampling exactly on a native step boundary needs no interpolation;
sampling between two native steps does interpolate and still matches the closed form to
`< 1e-6` (limited by the toy model's own step size, not by the interpolation).

**Scope note.** `Scheduler<M>` is homogeneous over one `DynamicsModel` type per instance --
every system it drives must be the same Rust type. True multi-domain scheduling (mixing,
say, a space model and a 6-DoF air model in one run) would need trait objects
(`Box<dyn DynamicsModel<Error = ...>>`, which in turn needs a shared or erased error type)
or an enum over the platform's model types -- a real design decision ADR-002 doesn't make
and this task doesn't ask for. Flagged here, not papered over.

**Controls are not wired.** Every `step` call the scheduler makes passes an empty control
slice (`&[]`). Routing a per-system control schedule into the scheduler is ADR-005
territory (still Planned) and out of scope for this skeleton.

## Interpolation: `hermite_velocity`

Two-point cubic Hermite, `p(s) = h00(s) p0 + h10(s) dt v0 + h01(s) p1 + h11(s) dt v1`, applied
independently to each of the three position components using that component's own velocity
(index `3+i`) as the endpoint derivative; the interpolated velocity is the *analytic
derivative of that same cubic* (`dp/dt = dp/ds / dt`), not an independently-interpolated
value that could disagree with `dp/dt`. State components past index 6 (were this ever
applied to something like a 42-state STM vector, which nothing in this crate does) are not
covered by the Hermite contract; the nearer endpoint's value passes through unchanged rather
than being silently zeroed or blended.

Unit-tested to: reproduce both endpoints exactly; reproduce constant-velocity (straight-line)
motion exactly at points strictly between the endpoints (a real correctness check, not a
smoke test -- a cubic Hermite matching a line's two endpoint values/derivatives must
reproduce the line everywhere, including its interior); reproduce a genuine cubic
(`p(t) = t^3`) exactly at five points between two exact samples of it, to `< 1e-6`
(floating-point roundoff on the polynomial evaluation, not a modeling error); pass the
nearer endpoint's value through unchanged for a component past index 6.

## `Kernel<M>`

```rust
let mut kernel: Kernel<GmatModel> = Kernel::new(output_period_ns); // e.g. 100_000_000 (10 Hz)
kernel.register_system("leo", period_ns, model, t0_tai_ns, initial_state_si);
let trajectories: BTreeMap<String, av_cdm::pb::Trajectory> = kernel.run(t0_tai_ns, end_tai_ns)?;
```

`run` samples every registered system at `output_period_ns` from `start_tai_ns` to
`end_tai_ns` inclusive (panics if the horizon isn't an exact multiple of the output period,
so the last sample always lands exactly on `end_tai_ns` rather than silently stopping short
or overshooting), and returns one `Trajectory` per system, `BTreeMap`-ordered. Every
`Trajectory`'s `interpolation` field is set to `INTERPOLATION_HERMITE_VELOCITY`, matching
what `Scheduler::sample` actually used to produce interpolated samples.

`entity_id` currently equals the registered system id -- the kernel skeleton has no entity
catalog yet (DRM/system-definition wiring, out of scope for M2.1), so this is a placeholder,
not a claim that the two concepts are the same thing. `Trajectory.provenance` and
`config_hash` are left at their proto defaults; populating them (spoore ADR-005 "evidence")
is future work.

## The acceptance test: the golden arc through the kernel at 10 Hz

`tests/golden_acceptance.rs` builds the same GMAT force model / spacecraft as
`crates/gmat-sys/tests/leo_golden.rs` (LEO, JGM2 8x8, Sun + Moon point masses, one day),
wraps the resulting `DerivativeModel` in `gmat_sys::model::GmatModel`, and drives it through
`Kernel::run` at 10 Hz output sampling (`output_period_ns = 100_000_000`, matching ADR-002's
default kernel dynamics rate) for the golden's full 86,400 s duration.

**Measured** (`cargo test -p av-kernel --test golden_acceptance -- --nocapture`, debug
build, Apple Silicon):

| | Result |
|---|---|
| Output samples | 864,001 (10 Hz x 86,400 s + 1) |
| Wall time | 24.1 s (debug build) |
| Position error vs `goldens/leo_1day_jgm2_8x8_sunmoon.json`'s `final_state` | **0.0000 m** (sub-millimetre; tolerance 0.05 m) |
| Velocity error | **2.430e-8 m/s** (tolerance 5e-5 m/s) |

Both well inside the golden's declared tolerance -- no tolerance was loosened to make this
pass; these are the numbers the run actually produced.

**Why the error is smaller here than `gmat-sys`'s own golden test's 5.7 mm / 6.3e-6 m/s,
not larger.** `leo_golden.rs` runs one continuous `Dopri5` integration across the full
86,400 s with adaptive step sizes up to 600 s. This test instead calls `GmatModel::step`
once per 100 ms output tick, and each call runs its *own* fresh `Dopri5` integration over
just that 100 ms span (`initial_step.min(max_step)` clamps to the 100 ms span itself, so
each call takes exactly one accepted RK step). A 100 ms step's local truncation error at
`rtol = atol = 1e-12` is far smaller than a step sized for a multi-hundred-second span, so
the accumulated error over the whole day is smaller too -- at the cost of roughly 100x more
`GetDerivatives` calls (7 per 100 ms tick x 864,000 ticks vs 63,602 for the one continuous
run) and a correspondingly longer wall time (24 s vs 0.2 s in the plain `leo_golden` test).
This is a real, measured consequence of stepping at the kernel's declared rate rather than
letting the integrator pick its own step sizes freely across the whole arc -- not a
discrepancy to be explained away.

Also verified by the acceptance test, not just asserted: the model's own bound initial state
(via `GmatModel::initial_state_si`, converted SI) matches the golden's recorded
`initial_state` (km) to `< 1e-6` m; the trajectory's sample count, first/last sample epochs,
and declared interpolation mode.

## Covariance: `Kernel<StmAugmented<M>>::run_with_covariance` (ADR-002 second amendment, M3.2)

`av_dynamics::StmAugmented<M>` wraps a model whose `stm_capable()` is `true` and presents the
augmented `[state; vec(Phi)]` vector as its own state, so the *existing* `Scheduler`/`Kernel`
machinery drives it exactly like a plain model -- no new stepping code. `Kernel<StmAugmented
<M>>::run_with_covariance(start, end, n, p0)` calls the ordinary `run` internally (one
integration pass, not two) and then, for every system named in `p0` (its declared initial
covariance, row-major `n x n`), splits each raw sample into `(mean, Phi)`, computes `P(t) =
Phi(t0, t) P0 Phi(t0, t)^T` (`av_dynamics::propagate_covariance`, symmetrized), and fills
`TrajectorySample.cov`; a system not named in `p0` gets an empty `cov`, identical to plain
`run` (question 11: covariance is always optional and explicitly requested, never a silent
default -- `run_with_covariance` is the only place in this crate `cov` is ever filled in).

**Design constraint, honoured exactly:** this is the kernel *integrating* the STM (one
augmented `GetDerivatives`-driven integration alongside the physical state), never reading
GMAT's own `Spacecraft` STM back after propagation -- that read-back is only ever the
*reference* value pinned in the golden, compared against, not the mechanism.

**M13.3: the equal-rate requirement is lifted.** Through M13.2, every covariance-requesting
system had to be registered at exactly this kernel's own `output_period_ns`, checked up front
with a loud panic -- `interpolate::hermite_velocity` has no interpolation contract for the STM
tail, so a system stepping at a different rate than the kernel samples at would otherwise have
had its `Phi` silently pass-through interpolated at off-boundary samples, which is physically
meaningless. As of M13.3, covariance propagates at **each system's own declared step period**,
whatever it is, and is **sampled -- never interpolated -- at output epochs** (ADR-005 sec 3:
"state transition matrix, covariance | never interpolated: available only at the instance's own
samples"). At every output tick, `crate::schedule::SampleKind` tells `run_with_covariance`
which case it is: `Native` (the tick lands exactly on this system's own native step) truncates
to `(mean, Phi)` and propagates `P(t)` exactly as before; `Between` (the tick falls strictly
between two native steps -- only reachable now that a system's period need not equal
`output_period_ns`) Hermite-interpolates *only* the leading `n` physical components for `mean`
(never the `Phi` tail) and marks `cov` unavailable at that sample (see below). This drives its
own `advance_to`/`sample_kind` loop rather than post-processing plain `run`'s own output, which
would run every system's physical state through `Scheduler::sample` -- interpolating the whole
augmented vector, `Phi` included, whenever a tick is not a native step.

**A latent `advance_to` bug this lift exposed, fixed as part of it.** `Scheduler::advance_to`'s
own doc comment always claimed "until each system has stepped to (or past) `target_tai_ns`," but
the code looped on `next_due_ns <= target_tai_ns`, which leaves a system *short* of the target
(not past it) whenever its own period exceeds what's left to cover -- unreachable before M13.3
because every existing caller only ever registered systems whose period divides evenly into the
caller's own sampling rate. `run_with_covariance`'s own tests exposed it (a coarser-than-output
covariance instance's very first period would otherwise hit `ScheduleError::OutOfRange` for
every off-grid tick before its own first native step). Fixed by looping on `history.curr`'s own
recorded time instead, restoring the documented contract exactly: identical step count/timing
whenever a period already divides the sampling rate evenly (every pre-M13.3 test, still
passing), and, additively, one extra catch-up step for a system that would otherwise be left
short -- which is exactly what makes `SampleKind::Between` a genuine two-point Hermite bracket
in that first-period window too, no separate "no bracket yet" case needed.

**Covariance availability: `Option<Vec<f64>>` in the Rust product, never a NaN sentinel (M14.3,
question 111).** `TrajectorySample.cov`'s wire shape is unchanged (`proto/**` is read-only to
this task) -- a sample carries a real covariance or it does not:

| Case | `cov` on the wire | Rust product (`kernel::covariance(sample)`) |
|---|---|---|
| A real, hygiene-checked propagated covariance | `n*n` entries, every one finite | `Some(&[f64])` |
| No covariance (not requested for this system, **or** requested but this output epoch is off the instance's own native grid) | empty (`len() == 0`) | `None` |

Through M13.3, the second row was actually two distinguishable cases: "not requested" (empty
`cov`) and "requested, unavailable at this sample" (an all-`f64::NAN` `cov` of the correct `n*n`
length, `kernel::covariance_unavailable`, classified by `kernel::covariance_state(cov: &[f64]) ->
CovarianceAvailability`'s three variants). That NaN sentinel is exactly question 111's own
defect: unambiguous by construction, but nothing forces a caller to classify `cov` before doing
arithmetic with it, so a caller that forgets to check first gets a silent, `NaN`-poisoned answer
with no error to notice it by. M14.3 deletes `covariance_unavailable`/`CovarianceAvailability`/
`covariance_state` and replaces the read side with `kernel::covariance(sample:
&TrajectorySample) -> Option<&[f64]>` -- `None` for either "no covariance" reason, `Some` for a
real one -- and the write side (`Kernel::run_with_covariance`/`HeteroKernel::run_with_covariance`)
computes an `Option<Vec<f64>>` internally and only ever converts it to the wire's empty-`cov`
convention at the very last step (`kernel::sample_with_cov`, private). **This collapses the wire
distinction between "not requested" and "unavailable at this sample"** -- both now serialize
identically as an empty `cov`, a real, disclosed behavior change from M13.3 -- in exchange for
making the actual footgun (a `NaN` a naive caller could silently compute with) structurally
impossible: there is no `f64::NAN` anywhere in this representation left for a forgetful read to
consume. A caller that needs the distinction already has it from context (whether the system was
named in the `p0` map passed to `run_with_covariance`), never by inspecting `cov` post hoc.

**Measured** (`cargo test -p av-kernel --test golden_acceptance -- --nocapture`,
`kernel_covariance_matches_the_golden_stm_and_propagated_cov`, debug build, Apple Silicon).
Registered at `period_ns = 600 s` (this golden's own `MaxStep` -- a system's own declared
rate, not a shortcut chosen to make the test fast, though it also happens to be one), over
the same golden's full 86,400 s arc, against `goldens/leo_1day_jgm2_8x8_sunmoon.json`'s
`"stm"` block (a declared diagonal SI P0: (100 m)^2 position variance, (0.1 m/s)^2 velocity
variance):

| | Result |
|---|---|
| Samples | 145 (600 s x 144 + 1) |
| Wall time | 1.3 s (was 0.66-0.68 s before M11.2 -- `StmAugmented::step` now delegates to `step_with_stm`, which reads back GMAT's own `RMAG`/`Cd` real parameters every native period, unlike the previous scheme's plain `derivatives`-only integration) |
| `Phi(t0,t0)` (zero-duration `step_with_stm` on the unwrapped model) | exactly the identity, `0` max abs error vs the golden's own `max_abs_identity_error_t0` |
| Position / velocity error vs golden at t1 | **0.0001 m / 1.6e-7 m/s** (M11.2: improved from 2.7 mm / 3.0e-6 m/s -- `StmAugmented::step`'s new per-native-period-reseeded scheme, question 101, happens to track this golden slightly more closely than the old continuous-accumulation one; both are well inside tolerance) |
| Covariance error vs golden's `cov_t1_si` at t1 | Frobenius **4.5e-2** absolute, **3.1e-11 relative** to the golden's own covariance norm (M11.2: improved from 8.27e-1 / 5.6e-10; **unchanged by M13.3** -- this test's own `period_ns == output_period_ns`, so its numbers exercise the same `Native`-only path as before, re-measured after M13.3's refactor to confirm) |
| `Phi P0 Phi^T` pre-symmetrization asymmetry (max over the run) | 1.2e-7 (M11.2: was 2.38e-7; corrected on every sample regardless -- `av_dynamics::propagate_covariance`'s job) |
| A system never named in `p0` | `cov` stays empty at every sample (checked by the GMAT-free synthetic unit test, not repeated here at GMAT's cost) |

No tolerance was loosened to make this pass -- these are the measured numbers, re-measured
after M11.2's `StmAugmented::step` change (question 101: the covariance path now delegates to
`step_with_stm` per native period and composes local STMs, rather than one continuous
augmented-state integration -- see `crates/av-dynamics/src/stm.rs`'s own doc comment). A
separate,
GMAT-free unit test (`kernel::tests::run_with_covariance_matches_the_closed_form_rotation_
and_leaves_unrequested_systems_empty`) checks the same mechanism against a planar rotation
with a closed-form `Phi`, including that a system absent from `p0` never gets a `cov`.
`kernel::tests::run_with_covariance_at_a_coarser_instance_period_propagates_only_on_the_
native_grid`/`hetero_kernel_run_with_covariance_at_a_coarser_instance_period_propagates_only_
on_the_native_grid` (M13.3, GMAT-free) check the lifted restriction itself: a 6-state
constant-acceleration model with a closed-form `Phi = I + dt A`, registered at a 500 ms
covariance step under a 100 ms output rate -- every output tick's `mean` matches the closed
form exactly (Hermite reproduces the true quadratic position exactly, on or off the native
grid), `cov` is `Available` and matches the closed form only at the four 500 ms-aligned ticks,
and `UnavailableAtThisSample` everywhere else.

**Golden: coarser instance period vs. the fine one, at shared epochs**
(`tests/golden_acceptance.rs::kernel_covariance_at_a_coarser_instance_period_matches_the_fine_
one_at_shared_epochs`, M13.3 "Golden" requirement). Two independent
`Kernel<StmAugmented<GmatModel>>` runs over the *same* golden arc and the *same* `P0`: "fine"
at `period_ns = 600 s` (matches its kernel's own `output_period_ns`, same as the test above),
"coarse" at `period_ns = 1200 s` while its kernel still samples every 600 s. At every shared
(1200 s-aligned) epoch the coarse run's `cov` must be `Available` and must match the fine run's
own `cov` at that identical epoch; at every other 600 s tick the coarse run's `cov` must be
`UnavailableAtThisSample` while the fine run's is `Available` (600 s is on *its* own grid).
**Tolerance, derived before the test was run, not fitted to it:** `Phi(0, 1200s)` is
mathematically identical however computed (STM composition is exact), but the fine run gets
there as the product of two independently-reseeded 600 s local STMs
(`StmAugmented::step`'s per-native-period reseed scheme) while the coarse run integrates one
continuous 1200 s span reseeded once -- both at the model's declared `rtol = atol = 1e-12`, but
Dopri5's adaptive step-size choices differ slightly between the two schemes (the same effect
`StmAugmented::step`'s own doc comment already documents for the plain-vs-covariance
comparison, applied here to fine-vs-coarse). Given this codebase's own harder comparison above
(kernel vs. an *independently generated* GMAT reference) already measures 3.1e-11 relative at
the same `rtol/atol`, and this comparison shares the same kernel, Rust integration code and
GMAT process, the same established `1e-6` relative bound every other covariance-vs-reference
comparison in this codebase uses was set as the gate -- **measured: 1.308e-11 relative**, 73
shared (`Available`) epochs and 72 coarse-off-grid (`UnavailableAtThisSample`) epochs, both
counted exactly.

### Covariance hygiene (`docs/open-questions.md` question 80)

Every `P(t)` `run_with_covariance` computes is run through
`av_cdm::covariance::check_spd_row_major` -- the same Cholesky-based SPD bar
`spoore_cdm::GaussianState`'s own constructor enforces -- **before** it is written into
`TrajectorySample.cov`, i.e. before it has any chance to cross into a spoore type downstream
(see `crates/av-cdm/README.md`'s "Covariance hygiene" section for the check itself, the
counter, the eigenvalue-proxy measurement, and the opt-in nearest-SPD projection).

`run_with_covariance` now takes a `nearest_spd_projection: bool` parameter (the opt-in
projection's hook). `DrmOptions.nearest_spd_projection` (`proto/altavista/v1/system.proto`,
question 83) is a real, additive field as of the lead's decision; what is not yet wired is a
path from an actual `DesignReferenceMission` to this method -- no crate in this repo owns
constructing one and driving a `Kernel` from it end to end, so a caller that has read
`drm.options.nearest_spd_projection` passes the value straight through as this plain `bool`
rather than this method reading the proto message itself (see `av-cdm`'s README for the
matching note on `av_cdm::covariance::nearest_spd`). `false`: a failing sample returns
`ScheduleError::CovarianceHygiene(..)` immediately, aborting the run -- the golden test above
passes `false` and never needed to opt in, because the golden's own propagated covariance is
genuinely SPD (measured below). `true`: a failing sample is replaced by
`av_cdm::covariance::nearest_spd_row_major`'s projection, logged loudly on `stderr`, and the
run continues -- pinned by `kernel::tests::run_with_covariance_repairs_an_indefinite_
covariance_when_projection_is_opted_into` (a synthetic, deliberately negative-definite `P0`
through a zero-rotation `Phi = I`, so the indefinite matrix reaches the check unchanged; no
GMAT dependency). The un-opted-in failure path is pinned by
`kernel::tests::run_with_covariance_returns_a_typed_hygiene_error_for_an_indefinite_
covariance_and_counts_it`, which also checks `av_cdm::covariance::spd_check_failures()`
increased.

**Measured** (`kernel_covariance_matches_the_golden_stm_and_propagated_cov`, same run as
above): the kernel's own final propagated sample passes `check_spd_row_major` (asserted
explicitly in the test, not merely implied by `run_with_covariance` not erroring), with a
smallest Cholesky-diagonal² proxy printed via `eprintln!` on that run -- see that function's
own doc comment (`av_cdm::covariance::CholeskyDiagnostics::min_cholesky_diag_sq`) for exactly
what this proxy is (an upper bound on the true smallest eigenvalue, not the eigenvalue
itself) and is not. `av-cdm`'s own test suite additionally checks the golden's declared `P0`
and its recorded `cov_t1_si` directly against the same check, independent of a full kernel/GMAT
run -- see `crates/av-cdm/src/covariance.rs`'s
`the_golden_arcs_own_p0_and_propagated_cov_t1_both_pass` test for the exact numbers.

### Covariance frame conversion (`docs/open-questions.md` question 138, M21.4)

M19.1 (ADR-002's fourth amendment) made `executor::convert_gmat_trajectory_to_declared_frame`
rotate every `TrajectorySample.mean` through `gmat_sys::Gmat::convert` when a `"gmat."`-bound
instance's declared `spacecraft.CoordinateSystem` differs from its own integration frame, but
left `cov` untouched and refused the combination outright (`DrmError::
CovarianceFrameConversionNotSupported`) -- rotating a propagated covariance needs the same
`CoordinateConverter` rotation applied to the 6x6 `cov` block, and, for a time-varying rotation
like a body-fixed frame, its time derivative too. M21.4 closes this.

**Shim shape.** `gmat-sys`'s `gmatffi_convert_state_and_rotation` (a sibling of `gmatffi_
convert_state`) runs the identical `CoordinateConverter::Convert` call and additionally returns
`GetLastRotationMatrix()`/`GetLastRotationDotMatrix()` -- one `Convert` per sample, never two,
so a caller rotating a covariance never has to trust that two separate calls agree on which
conversion they mean. `Gmat::convert_with_rotation` (`crates/gmat-sys/src/lib.rs`) is the safe
Rust wrapper, returning `ConvertedStateWithRotation { state_km, rotation, rotation_dot }`
(`rotation`/`rotation_dot` row-major 3x3, unitless -- never scaled by the km-vs-m choice of
`state_km`).

**The 6x6 Jacobian is `[[R,0],[Rdot,R]]`, not block-diagonal.** The rotation between two frames
is generally time-varying (a body-fixed frame rotates with its body), so the velocity block
picks up an `Rdot` coupling term the position block does not need:
`out_pos = R·in_pos` (+ a state-independent origin translation, already in `state_km`),
`out_vel = Rdot·in_pos + R·in_vel`. `executor::rotate_covariance` builds this matrix and applies
it as `P' = M P Mᵀ` via `av_dynamics::propagate_covariance` (reused, not reimplemented -- the
same `M P Mᵀ`-with-symmetrization primitive the STM covariance path above already uses,
generalized to any invertible `M`, not just an STM), then re-runs `av_cdm::covariance::
check_spd_row_major` on the rotated result (`DrmError::CovarianceHygiene` on failure) -- a
congruence transform preserves positive-definiteness only in exact arithmetic, and this
repository's own hygiene rule (question 80) is to check, not assume. `executor::convert_gmat_
trajectory_to_declared_frame` calls `Gmat::convert_with_rotation` only for a sample whose `cov`
is non-empty; a sample with no covariance still uses plain `Gmat::convert`, unchanged.

**GMAT's own `OrbitErrorCovariance` `ReportFile` is not a usable reference for this.**
`OrbitData::GetCovarianceRmat66` (`third_party/gmat-src/src/base/parameter/OrbitData.cpp`)
converts a spacecraft's covariance to a different `CoordinateSystem` using only `GetLastRotation
Matrix()` -- block-diagonal `[[R,0],[0,R]]`, never calling `GetLastRotationDotMatrix()` at all.
That is exact for two frames with no relative angular velocity (e.g. `EarthMJ2000Eq` ->
`EarthICRF`, a fixed frame bias) but wrong for a rotating target frame like `EarthBodyFixed`.
`crates/gmat-sys/tests/convert_rotation.rs` measures this directly against a real GMAT
`ReportFile` (`goldens/covariance_bodyfixed_leo_2h.json`, generated by `goldens/gen_covariance_
bodyfixed_leo_2h.py`): the position block agrees with GMAT's own report to ~1e-17 relative (the
one block `Rdot` cannot affect, so this pins `rotation` itself), while the velocity/cross blocks
disagree by a real, measured, non-noise amount (~1e-10 / ~1e-6, matching the `Rdot`-quadratic
and `Rdot`-linear terms this repository's own implementation correctly includes and GMAT's
`OrbitErrorCovariance` Parameter does not). The REQUIRED acceptance pin therefore uses the
other option the task brief allowed: `crates/av-kernel/tests/covariance_frame_conversion.rs`
runs a covariance DRM twice -- once declaring the integration frame itself (a no-op conversion,
so its `cov` is the raw, un-rotated `Phi P0 Phi^T`) and once declaring `EarthBodyFixed` -- and
checks that the second run's `cov` equals `R` (the first run's own `cov`) `Rᵀ`, `R`/`Rdot` built
from an independent `Gmat::convert_with_rotation` call, to 1e-12 relative. (A real GMAT
mechanism -- `Earth.NutationUpdateInterval`'s default 60 s BodyFixedAxes caching, the same
effect `goldens/gen_bodyfixed_leo_2h.py` already documents for the mean-state conversion path --
has to be disabled, once, for this specific test to reach that bound; see that test file's own
module doc comment for the measured ~2.4e-10-relative effect it has if left at the default.)

### `AnyModel` delegates every `DynamicsModel` method explicitly (M14.3, question 112)

`docs/open-questions.md` question 112 names `ErasedModel`/`AnyModel` delegation as a recurring
defect class -- see `crates/av-dynamics/README.md`'s own "No trait default returns a valid empty
result" section for the full three-occurrence history and the trait-level audit. `AnyModel`
(`src/drm/binding.rs`) is this crate's own half of that fix: through M13.2 it overrode `state_dim`/
`derivatives`/`describe`/`stm_capable`/`stm_derivatives`/`step`/`step_with_stm`/`step_with_ports`
but not `integrator` -- harmless only because neither `GmatModel` nor `ConstantAccelModel`
overrides `integrator` either, so the missing arm silently reached the *trait's* default
(`Dopri5::default()`) and happened to agree. M14.3 adds the `integrator` arm so this stops being
a coincidence, and re-asserts the other eight explicitly rather than trusting that they still
compile-error if a ninth method is ever added without a matching arm (a `match self { AnyModel::
Gmat(m) => ..., AnyModel::ConstantAccel(m) => ... }` over a two-variant enum, unlike a generic
`impl<M> DynamicsModel for ErasedModel<M>`, would not warn on a missing method at all -- only a
missing *match arm* on an *existing* method is caught by the compiler, which is exactly why the
per-method test suite below exists).

**`AnyModelError::CapabilityMissing`** (new, M14.3) mirrors `av_dynamics::ModelError::
CapabilityMissing` at this enum's own erasure boundary: `AnyModel::ConstantAccel`'s
`stm_derivatives`/`step_with_stm` arms used to reach the trait's own `unimplemented!()` default
and panic the whole process whenever called on the (always non-STM-capable) native placeholder;
they now return this typed, catchable error instead -- the same panic-to-`Result` upgrade
`ErasedModel::stm_derivatives` already made, applied at the one other place in this crate where a
`DynamicsModel::Error` is concretely fixed before the trait's own generic default would apply.

Per-method delegation proof (`src/drm/binding.rs`'s own `#[cfg(test)]` module -- `AnyModel` is
`pub(crate)`, so this cannot live in `tests/`), one test per method, all against the
`ConstantAccel` variant: `any_model_state_dim_delegates_to_the_constant_accel_variant`,
`any_model_derivatives_delegates_to_the_constant_accel_variant`,
`any_model_describe_delegates_to_the_constant_accel_variant`,
`any_model_integrator_delegates_to_the_constant_accel_variant`,
`any_model_stm_capable_delegates_to_the_constant_accel_variant` (all reaching
`ConstantAccelModel`'s own values), `any_model_stm_derivatives_returns_a_typed_capability_
missing_error_for_the_constant_accel_variant` and `any_model_step_with_stm_returns_a_typed_
capability_missing_error_for_the_constant_accel_variant` (the panic-to-`Result` upgrade),
`any_model_step_delegates_to_the_constant_accel_variant` (M10.3, pre-existing) and
`any_model_step_with_ports_delegates_to_the_constant_accel_variant` (M14.1/M13.2, pre-existing)
round out all nine methods. Each fails if `AnyModel`'s own match arm for that method silently
reached the trait's default instead of the named variant's own implementation -- GMAT-free by
construction (only the `ConstantAccel` variant is exercised here); the `Gmat` variant's own
`step`/`step_with_ports` overrides are proven end to end elsewhere against a real GMAT install
(`tests/drm_executor.rs::drm_rmag_output_matches_a_genuine_gmat_reportfile`,
`tests/golden_acceptance.rs`).

## Ports and the router (`docs/open-questions.md` question 108, M13.1; question 110, M14.2)

The loader used to refuse any DRM that declared `SystemDefinition.ports` at all
(`schema::refuse_if_nonempty`). The lead's decision: type the field, build a router from
`SosConfiguration.connections`, deliver at the first receiver step whose own epoch is at or
after a message's availability (question 110 -- see "Latency actually defers delivery" below;
M13.1 originally read this as "the receiver's next step", full stop, which M14.2 corrects),
implement latency (and only latency) as the phase-one link model, and refuse a bad connection
at load rather than at run time. `Variant` (`SystemDefinition`) stays refused -- it has no
consumer yet, and this task does not add one.

### The loader: `Port`/`PortTiming` (`schema::RawPort`/`RawPortTiming`)

Field-for-field mirrors of `Port`/`PortTiming` (`proto/altavista/v1/system.proto`), matching
`PortKind`/`PortDirection` by their proto enum names exactly like every other enum field in
this loader. Loading a `Port` does not validate it against anything else by itself -- an empty
`ports: []` (every existing DRM, including the golden) parses exactly as it did when the field
was opaquely refused, and the golden's own canonical hash is unchanged (an empty repeated field
encodes to zero bytes either way -- see `tests/drm_executor.rs::the_golden_bundles_canonical_
hashes_are_byte_identical_to_before_the_typed_port_loader`, which hardcodes the three digests
independently of the loader, not merely re-checking `execute`'s own internal comparison).

### `router::Router`

```rust
Router::build(sos: &SosConfiguration, systems: &BTreeMap<String, SystemDefinition>) -> Result<Router, RouterError>
```

Validates every `Connection` against the two named ports (looked up by instance ->
`SystemInstance.system_id` -> that `SystemDefinition`'s own `ports`, by name) before returning:

- **undeclared port** -- `from_port`/`to_port` names no `Port.name` the instance declares
  (`RouterError::UndeclaredPort`).
- **direction mismatch** -- `from_port` must be OUT or INOUT, `to_port` must be IN or INOUT
  (`RouterError::DirectionMismatch`).
- **kind mismatch** -- `from_port.kind != to_port.kind` (`RouterError::KindMismatch`).
- **unsupported link model** -- `Connection.link_model` (a free-form "dynamics model id" for a
  future richer link) is `""` (no link model: zero added latency) or `"latency"` (phase one's
  own link model: added latency is the **sum of both named ports' own declared
  `PortTiming.latency_ns`** -- "rates and latencies are part of the system, not of where it
  runs," `Port`'s own doc comment, so the router applies numbers a system already declares
  about its own ports rather than inventing a second place to put them); anything else is
  `RouterError::UnsupportedLinkModel` (question 108: "a link_model naming anything else is a
  typed refusal, not a silent ignore").

An empty `connections` list always builds trivially (every existing DRM, the golden included).
`crate::drm::executor::execute` calls `Router::build` as one of its own load-time checks
(alongside hash verification, expression validation), mapped to `DrmError::Router` -- refused
before any instance is even classified or bound, exactly like every other pre-propagation
refusal in that module. **The built `Router` is not driven any further by the DRM executor
yet**: neither `BindingPlan::Gmat` nor `BindingPlan::ConstantAccel` (the only two binding kinds
this executor materializes) overrides `step_with_ports`, so there is nothing yet for it to
actually deliver during a DRM run -- see `tests/ports_router.rs` for where the router *is*
driven end to end, directly against `HeteroKernel`.

`Router::deliver(from_instance, emission_tai_ns, outbox)` queues every message in `outbox`
(a `Connection`'s `to_port`, `emission_tai_ns + latency_ns` as its *availability* epoch, the
payload unchanged) onto every connected receiver's pending queue; a message on a port with no
matching connection is dropped, silently -- a run-time condition (nothing was declared to
receive it), not the load-time refusal `Router::build` already applies to every *declared*
connection.

`Router::take_inbox(to_instance, as_of_tai_ns)` drains and sorts everything currently queued for
one receiver whose availability is `<= as_of_tai_ns` into `crate::ports::sorted_inbox`'s order
(below) and returns it as an `Inbox`; anything not yet available stays in the pending queue,
untouched, and is reconsidered the next time this is called for the same receiver (see "Latency
actually defers delivery" below -- M14.2, question 110).

### Delivery order (question 108's decision, `ports::sorted_inbox`)

**Receiving instance, then port name, then sender emission epoch, then sender instance id.**
The "receiving instance" field is enforced structurally -- `Router` keeps one pending queue per
receiver, so `sorted_inbox` only ever sorts one receiver's own queue, over the remaining three
fields. `tests/ports_router.rs::delivery_order_is_deterministic_and_matches_port_then_sender_
and_repeats_identically` wires two senders on two differently-named ports into one receiver
(port name "alpha" < "beta" while sender instance name "sender_a" < "sender_b" -- deliberately
the *opposite* pairing, so the test cannot pass by coincidentally sorting on the wrong field)
and compares the **exact delivered sequence** -- port names paired with epochs, in order, not a
count or a hash of one -- bit-identical across two independent runs of the same setup.

### `HeteroScheduler::advance_to_with_ports`/`HeteroKernel::run_with_ports`

Additive methods, alongside (never replacing) `advance_to`/`run`. `advance_to`'s existing loop
advances one system all the way to `target_tai_ns` before touching the next -- correct when
systems never talk to each other, but wrong for ports: question 110's rule ("delivered at the
first receiver step whose own epoch is `>=` availability") only makes sense if a message a
sender emits at time `t` is even *queued* before a receiver whose own next native step is also
at (or after) `t` asks for its inbox. `advance_to_with_ports` instead finds the smallest
`next_due_ns` still eligible for `target_tai_ns` across every registered system, steps every
system tied for that minimum together (in `BTreeMap` -- sorted instance-name -- order, same
determinism rule as `advance_to`), delivers each of their outboxes to `router`, and repeats.
Two systems that never step at the same native time interleave exactly as `advance_to` would
order them, so this is a strict refinement of that method's own determinism guarantee.

**M16.1 (`docs/open-questions.md` question 119): a coarse *physical* system now catches up past
`target_tai_ns`, atomically, exactly like `advance_to`.** Through M15.2, "eligible" meant
`next_due_ns <= target_tai_ns` for every system alike -- correct only as long as every system's
own period divides the query interval evenly, and wrong the instant a physical (`state_dim() !=
0`) system's own period exceeds `target_tai_ns` itself: its `next_due_ns` was never `<=
target_tai_ns`, so it was never stepped at all, and the very next off-grid `sample_kind` query
for it failed `OutOfRange` -- no `prev`/`curr` bracket to interpolate ever existed. The lead's
ruling on the question 109/110 "fork" the manager raised: **there is no fork.** A coarse
system's step from its current time to `+ period_ns` is one atomic advance; its port outputs are
still stamped with that step's own end epoch and still held by `Router::take_inbox` until a
receiver's own step reaches it (question 110, unchanged, not duplicated) -- so stepping a sender
ahead of the output clock changes *when it is asked to step*, never *when its output becomes
available*. Eligibility is now `history.curr.0 < target_tai_ns` for a physical system (the same
test `advance_to` already uses), so it is guaranteed to end up caught up *past*
`target_tai_ns`, never left short of it. A **zero-dimensional** system (`state_dim() == 0`, a
`BINDING_KIND_CONTAINER` instance) deliberately keeps the original `next_due_ns <=
target_tai_ns` test instead: `HeteroScheduler::sample_held`'s zero-order hold already answers
any query at or after `history.curr` correctly with no bracket at all, and is forward-hold-only
(a query strictly behind `history.curr` is a typed refusal, pinned by
`hetero_scheduler_sample_held_is_fresh_at_the_seed_and_held_strictly_after_with_no_bracket_
needed`) -- catching a container up ahead of the query the same way as a physical system would
turn every intermediate `sample_held` query into exactly that refused, backward case, trading
one bug for another in the one place that never had it.

Required tests: `crate::kernel`'s `hetero_kernel_run_with_ports_populates_native_interpolated_
and_held_kind_across_a_multi_rate_run` (M15.2's `#[ignore]`d reproducer, now passing unchanged --
a 300 ms model and a 300 ms container both sampled at a 100 ms output rate: NATIVE at 0/300/600/
900 ms, INTERPOLATED for the model off-grid, HELD for the container off-grid) and, in
`tests/ports_router.rs`, `a_coarse_sender_stepped_ahead_atomically_does_not_deliver_to_a_fine_
receiver_before_its_own_step_end` (a 300 ms sender's atomic first step, forced to happen three
whole receiver periods ahead of the output clock, still does not reach a 100 ms receiver before
the receiver's own step reaches 300 ms -- counted by step, via `SignalReceiverStepLog`, not by
comparing final `tai_ns` values) and `delivery_order_is_deterministic_when_multiple_senders_
catch_up_by_different_amounts` (two senders catching up by different numbers of periods on the
very first output tick still deliver a bit-identical sequence across two runs).

At each such step, every system calls `av_dynamics::DynamicsModel::step_with_ports` (never
`step`) with `router.take_inbox(id, t)` -- `t` is that step's own **result** epoch (the epoch
`id`'s state will have advanced to once the step completes, not the epoch it steps *from*) --
and hands the returned `Outbox` to `router.deliver`. A system that never overrides
`step_with_ports` -- every `MODEL` binding this workspace has today -- behaves identically
whether it is run through `run_with_ports` or plain `run`: always an empty `Inbox`, never
anything in its `Outbox`. This is what makes `run_with_ports` additive rather than a change to
`run`: no existing caller needs to change, or even know it exists.

`tests/ports_router.rs::two_native_models_exchange_a_signal_without_latency`/`..._with_latency`
wire a sender (emits a SIGNAL carrying its own emission epoch) to a receiver (logs every
delivered message) with `link_model = ""` and `link_model = "latency"` respectively, and check
the delivered `tai_ns` against the closed-form prediction in both cases (exactly the emission
epoch with no link model; the emission epoch plus both ports' summed `latency_ns` with one) --
see "Latency actually defers delivery" (M14.2) for what the latter test now also proves about
*when*, not only what, is delivered.

**M14.1 (question 109): `execute()` now actually calls `run_with_ports`.** Through M13.1,
`run_with_ports` was built and tested at the kernel level (above) but the DRM executor's own
`execute()` never called it -- every instance ran on its own isolated per-instance loop, so a
`Router` built at load validated `SosConfiguration.connections` but delivered nothing at run
time. `av_kernel::drm::executor::run_shared_group` is the caller that closes that gap -- see
"One shared kernel run" below.

### Latency actually defers delivery (`docs/open-questions.md` question 110, M14.2)

M13.1's own delivery model, as originally built, only ever *stamped* latency onto
`PortMessage.tai_ns` -- delivery itself still happened at the receiver's very next
`step_with_ports` call regardless of that timestamp. Under a 50 ms latency and a 100 ms
receiver step, a message could arrive in the *same* step it would have with no latency at all:
"a latency that never delays data is an annotation, not a link model" (question 110). **The
lead's decision, ADR-005 sec 4:** a message is *available* at `emission_tai_ns + latency_ns`,
and is delivered at the first receiver step whose own epoch is `>=` availability -- never
earlier. A message not yet available is **held**, not dropped: `Router::take_inbox` partitions
its receiver's pending queue into what clears now (`tai_ns <= as_of_tai_ns`) and what stays
queued, reconsidering the latter every later call for that same receiver until one finally
clears it (`src/router.rs`'s own module doc comment and `#[cfg(test)] mod tests` -- the
`Router`-level unit tests for holding across multiple calls, the inclusive boundary, and never
losing a message across many small `as_of` steps).

**A message still pending when the run ends is lost, not delivered late.** Nothing calls
`take_inbox` again once the run's very last `advance_to_with_ports`/`run_with_ports` call
returns (`run_shared_group` threads one `Router` across every span of a DRM run, but the run
itself still has a last span), so a message whose availability never coincides with a receiver
step actually taken before `end_tai_ns` simply stays in `Router`'s own `pending` map and is
dropped along with it. `Router::has_pending` after a run tells a caller whether this happened.

`tests/ports_router.rs` carries the required kernel-level tests, driven through the real
`HeteroKernel::run_with_ports` entry point M14.1 wired the router into:

- `fifty_ms_latency_under_a_hundred_ms_step_delivers_one_step_later_than_zero_latency_when_
  emission_lands_mid_step` -- counts native steps directly: a 50 ms latency delivers one
  receiver step later than zero latency, off the identical emission, when that emission lands
  strictly inside a receiver step rather than on one of its boundaries.
- `zero_latency_delivers_in_the_same_step_when_emission_lands_exactly_on_a_step_boundary` --
  the inclusive `<=` boundary: availability exactly equal to a step's own epoch delivers at
  that step, not one later.
- `delivery_order_is_deterministic_under_deferral_and_repeats_identically` -- question 108's own
  delivery-order rule survives deferral: two independent runs of a setup where one connection's
  messages are genuinely held across a step boundary and the other's never are still produce a
  bit-identical delivered sequence.
- `every_deliverable_message_is_delivered_none_lost_over_a_run_long_enough_for_all_of_them_to_
  land` -- total delivered equals total emitted (zero residue) once a run gives its receiver
  enough trailing, emission-free steps to drain everything a finite latency ever held.
- `two_native_models_exchange_a_signal_with_latency` (`src/router.rs`'s own unit tests
  updated alongside it) -- **rewritten for M14.2**: the original 50 ms-under-100 ms-period
  setup could never distinguish "latency merely stamped" from "latency actually gates delivery"
  (both land at the same step either way, since 50 ms is under one period) -- see that test's
  own doc comment for the full derivation. It now uses 120 ms of latency, above one period, so
  the fix and the bug it replaces disagree on which step delivers.

## The DRM executor (`drm`, M6.1)

`docs/open-questions.md` question 87: "a DRM executor in `av-kernel` (load a
`DesignReferenceMission` + `SosConfiguration` + `SystemDefinition`s, hash them, bind
model-kind instances, run, emit trajectories with provenance carrying the hashes)." That
executor is `drm::execute` (`src/drm/executor.rs`); its inputs are YAML files under `drms/`
(see `drms/README.md`), one `DesignReferenceMission` + `SosConfiguration` + N
`SystemDefinition`s per run. As of M9.1 (question 93), `execute` returns `drm::RunProducts`
(trajectories, events, evaluated `scores`, provenance), not a bare `BTreeMap<String,
Trajectory>` -- see ["Scoring"](#scoring-the-expression-language-and-runproducts-expr-adr-005-
sec-6-m91) below.

### One shared kernel run per `SosConfiguration` (M14.1, question 109)

**Decided by the lead:** every non-covariance instance of one `SosConfiguration` -- every
`BINDING_KIND_MODEL` instance (GMAT or native) *and* every `BINDING_KIND_CONTAINER` instance --
now runs through **one** shared `HeteroKernel::run_with_ports` call per boundary-bounded span
(`drm::executor::run_shared_group`), with the `Router` built at load actually delivering
`SosConfiguration.connections` between them. Through M13.3, `execute()` ran every instance on
its own isolated loop (`run_plain_instance`/`run_covariance_instance`/`run_container_instance`):
the router validated wiring but nothing a `step_with_ports` call ever emitted reached another
instance's own `Inbox` -- `docs/open-questions.md` question 109 names this "no message crosses
between instances through `execute()`" and calls it "the first item of M14... everything else
in P2 waits on this." `run_plain_instance`/`run_container_instance` are retired outright (their
logic is now `run_shared_group`/`run_one_span`, generalized from one instance to N); `run_span`
is retired too, replaced by the shared `append_span_samples` both the plain and container paths
now go through.

**Faults and maneuvers now split the whole shared run, not one instance's own loop.** Before
this task, a `FAULT_TARGET_KIND_DYNAMICS` fault or a maneuver split *one* instance's own
isolated loop into boundary-bounded segments. Now the split applies to the union of every fault
and maneuver naming *any* `BINDING_KIND_MODEL` instance in the run (a container instance can
never be a fault/maneuver's own target -- refused up front, unchanged): at every such boundary,
**every currently active model instance** is re-materialized from its own last physical state
(`materialize_plan_at_boundary`) -- the one instance this particular boundary targets gets its
plan changed (a fault) or its state dv-jumped (a maneuver); every other active instance is
re-materialized from its own *unchanged* plan and its own continuously-sampled state, exactly
the same call shape a maneuver boundary already used for its own one target instance before this
task. Every container instance's own live connection simply moves into the next span's kernel
unrebuilt (never re-`Bind`-ed) -- see `binding::SharedContainerModel`'s own doc comment.

Every DRM this crate's required tests and every existing golden exercise declares exactly **one**
`BINDING_KIND_MODEL` instance, so for all of them the boundary list, by construction, only ever
targets that one instance -- "re-materialize every other active instance too" is vacuously a
no-op, and the whole mechanism reduces to *exactly* the sequence of `HeteroKernel`/model
constructions the pre-M14.1 per-instance loop already performed. That is what makes "every
existing golden still passes, byte-identical, unmodified" (below) a real proof rather than a
claim. **Through M14.3, no required or existing test exercised a DRM with two or more
`BINDING_KIND_MODEL` instances where only some of them carry a fault/maneuver** -- the
"re-materialize an unaffected instance too" path was implemented and relied, by symmetry with the
single-instance case, on the same restart-invariance assumption that case already made, but was
not separately measured against a golden. **M14.4 measured it directly** -- see that task's own
section below for what it found.

**The covariance path is untouched.** `DrmOptions.covariance == true` still drives every model
instance through its own isolated `run_covariance_instance` loop; a container-bound instance is
still refused (`DrmError::ModelNotStmCapable`), exactly as before this task. This is a disclosed
limitation, not silently different behaviour depending on `covariance` -- `RunProducts.
provenance.attributes["kernel_run_mode"]` records which of the two run shapes -- `"shared"` or
`"covariance_per_instance"` -- actually produced a given run's own trajectories (`build_run_
provenance`'s own doc comment), so a caller never has to re-derive it from `DrmOptions` itself.
Question 111's own follow-on is where a shared covariance run, if ever built, would land.

**A container instance's own step period had to evenly divide `DrmOptions.sample_interval_s`
through M14.3** (`DrmError::ContainerPeriodExceedsSampleInterval` otherwise) -- a genuinely new
restriction the pre-M14.1 dedicated container loop never had (it sampled at the container's own
native period only, entirely decoupled from the trajectory's own output grid). The shared kernel
samples *every* registered system, container included, at `output_period_ns` and, through M14.3,
unconditionally Hermite-interpolated a system whose own period is coarser -- and
`crate::interpolate::hermite_velocity` requires at least 6 state components, while a container's
own `state_dim() == 0`, so that path panicked the moment an output tick fell strictly between two
of a coarser container's own native steps. **M14.4 lifts this restriction** -- see below.

### Lifting `ContainerPeriodExceedsSampleInterval`; multi-instance restart invariance measured; dropped in-flight messages surfaced (M14.4)

Three decided follow-ons from M14.1/M14.2.

**1. A container's own period no longer has to divide the sample interval.** A container has no
state to interpolate (`state_dim() == 0`), so ADR-005 sec 3's "discrete modes, counters |
zero-order hold" rule applies instead of Hermite interpolation. `crate::schedule::HeteroScheduler`
gains `state_dim(id)` and `sample_held(id, t)` (returning `(HoldKind, &[f64])`, `HoldKind` being
`Fresh`/`Held`); `HeteroKernel::run_with_ports` routes any system with `state_dim() == 0` through
`sample_held` instead of `sample`, and records every `Held` output tick in a new per-kernel
accumulator, `HeteroKernel::held_epochs(id)`. `DrmError::ContainerPeriodExceedsSampleInterval` and
the `output_period_ns % period_ns == 0` check that raised it are both gone from `drm::executor`;
the only restriction left on a container's own period is the one every instance already has (an
integer multiple of the GCD-derived base period, ADR-005 sec 2, enforced generically by
`HeteroKernel::run_with_ports`'s own `base_period_ns` gate). **A held sample is never silently
indistinguishable from a fresh one**: at M14.4 this was recorded out of band, via a per-kernel
`HeteroKernel::held_epochs(id)` accumulator feeding `run_shared_group`'s own comma-joined
`Trajectory.provenance.attributes["held_sample_tai_ns"]` -- **M15.2 (question 116) replaces that
side channel with a real field, `TrajectorySample.kind`; see that section below.** Required test
(`tests/drm_container.rs::a_container_period_coarser_than_the_sample_interval_is_zero_order_
held_not_interpolated`, updated for M15.2 to read `kind` instead of the now-deleted attribute): a
300 ms container under a 100 ms sample interval over 900 ms produces `SampleKind::Held` at exactly
t = 100, 200, 400, 500, 700, 800 ms -- held, held, fresh, repeating -- and never touches
`hermite_velocity`.

**2. Multi-instance restart invariance, measured (`tests/restart_invariance.rs`), not merely
argued by symmetry.** Two required tests: two `BINDING_KIND_MODEL` instances, one carrying a
`FAULT_TARGET_KIND_DYNAMICS` fault and later a maneuver, the other untouched
(`two_model_instances_only_one_faulted_and_maneuvered_matches_each_single_instance_run`); the same
with a `BINDING_KIND_CONTAINER` instance standing in for the untouched participant
(`a_container_instance_alongside_a_faulted_model_instance_matches_each_single_instance_run`). Both
compare the untouched (or faulted) instance's own trajectory, run alone, against its own
trajectory inside the combined multi-instance run.
**Finding (at M14.4): restart invariance did NOT fully hold.** `Trajectory.samples` (the actual
physical state at every output tick) *are* byte-identical either way, for both the untouched model
instance and the untouched container instance -- the physical claim M14.1 relied on by symmetry
is real. But `Trajectory.segments` was **not** invariant: `run_shared_group` re-materializes
*every* currently active instance at *every* boundary, including one a boundary does not target,
which always produces a new `TrajectorySegment` entry for it -- an untouched instance alongside a
fault-then-maneuver (two-boundary) run used to end up with **three** segments instead of the single
segment its own (unchanged) dynamics configuration would suggest. Both tests pinned this exactly,
including the proof that all three segments shared the identical `dynamics_hash` (nothing was
actually reconfigured, only re-segmented). **Fixed at M15.1 -- see "Segment merge across an
unaffected boundary" below.**

**3. In-flight port messages still queued at run end are no longer silently dropped.**
`crate::router::Router` gains `pending_count()` (the counted form of the existing `has_pending()`
bool). `drm::executor::execute` reads it once every span of the shared run has finished and
records it unconditionally on `RunProducts.provenance.attributes["dropped_in_flight_messages"]`
(`"0"` for a clean run, never omitted); when it is non-zero, `execute` also adds exactly one real
`EVENT_KIND_LIFECYCLE` event naming it (`drm::events::dropped_messages_event` --
`name = "dropped_in_flight_messages"`, `values["dropped_count"]`, `entity_id` empty: a run-level
fact, not tied to any one instance). Required tests (`tests/dropped_messages.rs`): two SIGNAL
messages made permanently undeliverable (20 s combined link latency over a 2 s scenario) yield
`dropped_in_flight_messages = "2"` and exactly one LIFECYCLE event; a run with no connections at
all yields `"0"` and no such event.

No proto change was needed for any of the three items, and none was made (`proto/**` stayed
untouched, as required).

`tests/drm_shared_run.rs` (new, M14.1) carries this task's own required tests: a native SIGNAL
producer, a `BINDING_KIND_CONTAINER` instance (`services/lockstep-ref`), and a native consumer,
wired together and run end to end in one DRM, scored by an `output.*` `MeasureOfEffectiveness`
on the consumer (`a_native_producer_a_container_and_a_native_consumer_run_end_to_end_in_one_
drm`); byte-identical `RunProducts` across two runs of that same DRM against two independently
spawned `lockstep-ref` processes (`byte_identical_run_products_across_two_runs`); and two native
instances with different closed-form accelerations and *no* declared connection between them,
run together in one shared kernel run, producing -- per instance -- exactly the samples/segments
that instance alone (as its own single-instance `SosConfiguration`) would have produced
(`two_unconnected_instances_match_what_each_produces_run_alone`, the direct multi-instance
generalization of every single-instance golden already passing unchanged). Every existing
golden/fault-split/maneuver acceptance test in `tests/drm_executor.rs`/`tests/drm_maneuver.rs`/
`tests/expr_goldens.rs`/`tests/gates_execution_error.rs` now runs through `run_shared_group`
(covariance is off in all of them) and still passes with **no change to any recorded tolerance**.

### `TrajectorySample.kind` on the wire (M15.2, `docs/open-questions.md` question 116)

**Decided by the lead: `TrajectorySample` gets a fourth field, `SampleKind kind = 4`**
(`SAMPLE_KIND_UNSPECIFIED`/`_NATIVE`/`_INTERPOLATED`/`_HELD`) -- the one proto change this task
was authorized to make, `proto/altavista/v1/trajectory.proto`, field 4 confirmed free (only
fields 1-3 existed on `TrajectorySample`, nothing reserved). `NATIVE` ties to the producing
instance's own native step grid, `INTERPOLATED` to the declared component class ADR-005 sec 3
already specifies (Hermite-with-velocity for position/velocity, this crate's only interpolated
shape today), `HELD` to zero-order hold (a `state_dim() == 0` system, e.g. a container).

**Reconciling with the Rust-side types that already existed (M13.3/M14.4).**
`crate::schedule::SampleKind<'a>` (`Native(&[f64])` / `Between { .. }`) and `crate::schedule::
HoldKind` (`Fresh`/`Held`) are unchanged -- they classify what a *scheduler* query needs (a bare
value vs. a two-point bracket to interpolate, or a bare value vs. "keep repeating the last one"),
which the wire enum has no use for. The *kernel* (this module) is what maps one onto the other,
at the one place every `TrajectorySample` this crate emits is now constructed,
[`sample_with_cov`]: `schedule::SampleKind::Native` -> `av_cdm::pb::SampleKind::Native`,
`schedule::SampleKind::Between` -> `av_cdm::pb::SampleKind::Interpolated`, `HoldKind::Fresh` ->
`av_cdm::pb::SampleKind::Native` (a zero-dimensional system's own native step is still "on its
own grid", the same as a physical system's), `HoldKind::Held` -> `av_cdm::pb::SampleKind::Held`.
`Kernel::run`, `Kernel<StmAugmented<M>>::run_with_covariance`, `HeteroKernel::run`, `HeteroKernel::
run_with_covariance` and `HeteroKernel::run_with_ports` all switched from calling `Scheduler::
sample`/`HeteroScheduler::sample` (which already did this same `Native`/`Between` match
internally, just to produce a blended `Vec<f64>`) to calling `sample_kind`/`sample_held`
themselves, so the classification is visible to the caller and can be written onto `kind` --
numerically identical `mean` either way, since it is the same Hermite blend either path takes.

**`held_sample_tai_ns` is deleted, not merely superseded.** The M14.4 side channel
(`HeteroKernel::held_epochs`, `ContainerSpanState::held_epochs`, `run_shared_group`'s own
`BTreeMap<String, Vec<i64>>` return member, and the provenance-attribute write in `execute()`)
is removed outright: a consumer now reads `TrajectorySample.kind` straight off the sample it is
already looking at, so the out-of-band, comma-joined-string side channel keyed by TAI ns serves
no purpose left to preserve.

**Required tests, and where each one actually lives.** `kernel.rs`'s own `#[cfg(test)] mod
tests` already had `run_with_covariance_populates_native_at_the_instance_grid_and_interpolated_
between` (synthetic closed-form model, `NATIVE` at the four ticks on the 500 ms grid,
`INTERPOLATED` at the other twelve) -- unchanged, still passing. `tests/golden_acceptance.rs`'s
`kernel_covariance_at_a_coarser_instance_period_matches_the_fine_one_at_shared_epochs` (a real,
GMAT-propagated day-long arc, previously only checking covariance availability) now also asserts
`kind` directly: `NATIVE` at every fine-run tick (its own period equals the output rate) and at
the coarse run's shared epochs, `INTERPOLATED` at the coarse run's off-grid ticks. Container
`HELD` coverage is `tests/drm_container.rs::a_container_period_coarser_than_the_sample_interval_
is_zero_order_held_not_interpolated` (updated, not added, for M15.2, to assert `kind` instead of
the deleted attribute -- see that test's own doc comment for what it would fail against).

**`hetero_kernel_run_with_ports_populates_native_interpolated_and_held_kind_across_a_multi_rate_
run` (`kernel.rs`'s own tests) was `#[ignore]`d through M15.2; M16.1 (question 119) un-ignores it
and it now passes, unchanged.** It reads all three `SampleKind` variants off one
`HeteroKernel::run_with_ports` run (one physical model and one zero-dimensional system sharing a
300 ms period under a 100 ms output rate). When this section was first written it was believed to
work and did not: running it surfaced a real, previously-undiscovered bug, unrelated to M15.2's
own `SampleKind` mapping (which was always correct wherever the test reached it) --
`crate::schedule::HeteroScheduler::advance_to_with_ports` never caught a system up past a query
time the way `advance_to` does, so a **physical** system whose period exceeds `output_period_ns`
never had the two-point bracket `sample_kind` needs and `run_with_ports` errored (`OutOfRange`)
at every off-grid tick for that system. Zero-dimensional systems were unaffected (`sample_held`
only ever needs the last recorded value, never a forward bracket), which is why the
container-only scenario above worked but this combined one did not. See "M16.1" under
`HeteroScheduler::advance_to_with_ports`/`HeteroKernel::run_with_ports` above for the fix.

**The Python-side half of M15.2** is `tests/test_cdm_adapter.py`'s
`test_sample_kind_round_trips_through_the_regenerated_bindings_for_every_declared_value` (every
`SampleKind` value round-trips through a real `SerializeToString`/`FromString`, not just an
in-memory attribute) and `test_trajectory_to_cdm_emits_sample_kind_native_not_the_unspecified_
default` (`altavista.cdm.trajectory_to_cdm` stamps `SAMPLE_KIND_NATIVE`, not the zero-value
default a forgotten `kind=` would leave it at).

Every `TrajectorySample` literal elsewhere in this workspace that listed only `tai_ns`/`mean`/
`cov` (test fixtures in `expr::{typecheck,objective,eval}`/`trajectory.rs`/`runproducts.rs`, and
`av-dynamics-service`'s own streaming propagator) needed a fourth field or a `..Default::
default()` to keep compiling; `av-dynamics-service`'s is set to `Native` deliberately, not as a
placeholder -- its `propagate::run_plain`/`run_with_covariance` step GMAT directly to each
requested epoch (`model.step(&state, t0, &[], t1 - t0)`), so there is no coarser native grid any
of its samples ever falls between.

`altavista/cdm.py`'s `trajectory_to_cdm` sets `kind=trajectory_pb2.SAMPLE_KIND_NATIVE` on every
sample it emits (every sample altavista itself produces already comes from GMAT's own native
report grid, never interpolated). `altavista/pb/generate.py` was rerun to regenerate the committed
Python bindings; `tests/test_cdm_v1.py`'s spoore.v0 byte-compatibility pin still passes unchanged
(the new field is CDM-v1-only, spoore.v0 has no equivalent and never reads `TrajectorySample`
directly).

### Segment merge across an unaffected boundary (M15.1, `docs/open-questions.md` question 115)

**Decided: adjacent segments of one instance merge when their `dynamics_hash` is equal AND no
maneuver applied to that instance at the boundary between them.** A maneuver on the instance
itself always keeps its own boundary -- a delta-v is a real state discontinuity even though the
dynamics configuration did not change. Implemented as `drm::executor::merge_adjacent_segments`, a
small pass run once per instance at the end of `run_shared_group` (the covariance path is
untouched -- it has no "re-materialize every active instance" mechanism to begin with, so it never
had this fragmentation). The rule's "was this a maneuver on this instance" input is not a new,
independently-tracked flag: `append_span_samples` already computes the exact boolean the merge
needs (`keep_previous_last_and_drop_incoming_first`; a maneuver boundary is precisely the case
where it is `false`), so `merge_adjacent_segments` reads it straight off that existing
sample-deduplication decision (`ModelSpanState`/`ContainerSpanState::segment_preceded_by_own_
maneuver`) rather than maintaining a second flag that could silently disagree.

**Verifying, not assuming, that a DYNAMICS fault changes `dynamics_hash` (question 115's own
instruction).** No special "this is a fault boundary" case exists in the merge check -- it relies
entirely on the hash actually differing. Traced through `drm::fault::apply_dynamics_fault` and
`drm::binding`: a native binding's `"accel.{x,y,z}"` fault writes into `ConstantAccelSpec.a`,
which `binding::materialize_constant_accel`'s own `settings_map` (hashed by `av_dynamics::
settings_hash`) includes directly; a GMAT binding's `"force_model.*"`/`"spacecraft.*"` fault writes
into `GmatSystemSpec` fields `binding::gmat_settings` hashes the same way. Both are proven directly
by the pre-existing `tests/drm_executor.rs::a_dynamics_fault_splits_the_run_into_two_segments_
with_continuous_state`'s `assert_ne!(traj.segments[0].dynamics_hash, traj.segments[1].
dynamics_hash, ...)`, which continues to pass unchanged under the new merge logic (the differing
hash is exactly why those two segments are not merge-eligible). A degenerate fault that sets a
parameter to the value it already had would leave the hash unchanged and the merge rule would then
merge across it -- correctly, since nothing was actually reconfigured; no fixture in this crate
declares such a fault, so this is disclosed rather than tested.

**A maneuver's own boundary can carry an *identical* `dynamics_hash`, and that is exactly the case
the maneuver check exists for.** A maneuver never touches `cur_plan` (only the physical state), so
a maneuver that immediately follows a fault re-materializes with the same settings the fault-
changed segment already had -- `dynamics_hash` genuinely comes out equal either side of that
boundary. `tests/segment_merge.rs::a_maneuver_never_merges_its_own_boundary_even_when_the_
dynamics_hash_is_unchanged` (new, required) builds exactly this fixture (a fault at 1 s, a maneuver
at 1.5 s, on the same instance) and asserts three segments survive, with segment 1 and segment 2's
`dynamics_hash` asserted *equal* (proving the fixture actually exercises the maneuver check, not
merely the hash-differs case) yet not merged. `src/drm/executor.rs::merge_adjacent_segments_tests`
carries the same claims as fast, GMAT-free unit tests directly against `merge_adjacent_segments`
(`identical_hash_across_a_maneuver_boundary_still_does_not_merge`, `a_fault_then_a_maneuver_back_
to_the_same_hash_still_keeps_three_segments`, plus the straightforward merge/no-merge/empty-list
cases).

**A disclosed case this merge still leaves un-collapsed: a GMAT-bound bystander.**
`fault::rebind_gmat_spec_at_state` sets `spacecraft.X/Y/Z/VX/VY/VZ` from the segment's own final
physical state at *every* re-materialization, fault or not, and `binding::gmat_settings` hashes
every `spacecraft.*` field -- so a GMAT-bound bystander's own `dynamics_hash` differs at every
boundary purely because its position/velocity differ, regardless of whether anything was actually
reconfigured. This merge pass therefore leaves a GMAT-bound bystander's segments un-merged:
conservative (never a *wrong* merge), not a bug, and not exercised by this task's required tests,
which use a native `"accel.*"` binding for every bystander -- exactly the shape this fix helps,
since a native binding's own settings never include state.

`tests/restart_invariance.rs`'s two required tests were upgraded from asserting only `samples`
(`physical_shape`) plus the old three-segments-one-hash finding to asserting `segments` themselves
byte-identical between the alone and together runs, for both the untouched participant and the
faulted+maneuvered target -- restart invariance now holds at both levels that file checks. No
proto change was needed or made; no `DrmError` variant was added (merging is a pure post-processing
improvement over an already-successful run, not a new failure mode); no golden's `segments`
changed (every golden declares at most one `BINDING_KIND_MODEL` instance with at most one
boundary of its own, so `merge_adjacent_segments` is a no-op for all of them -- verified by running
`drm_matches_the_golden_arc`, `drm_covariance_matches_the_golden_stm_and_propagated_cov`, the VNB
and RIC maneuver goldens, and `drm_rmag_output_matches_a_genuine_gmat_reportfile` unchanged).

### `HeteroKernel` is the only kernel this executor drives (M9.1)

Every instance -- GMAT-bound or the native `ConstantAccelModel` placeholder, fault-split or
not, covariance-requested or not -- is erased to `av_dynamics::BoxedModel` (wrapping in
`StmAugmented` first for the covariance path) and driven through `kernel::HeteroKernel`.
`Kernel<binding::AnyModel>`/`Kernel<StmAugmented<AnyModel>>` -- the two instantiations this
executor used through M8.2 -- are **retired**: nothing in this crate registers either any
more. `Kernel<M>` itself is unchanged and stays alive for its own unit tests and
`tests/golden_acceptance.rs`, which still drive it directly against one concrete model type
(`GmatModel`) -- only the *DRM executor's* use of the generic type over the `AnyModel` enum
specifically is gone.

### `ModelRegistry` is the sole constructor (M10.3, question 98)

Through M10.2, `registry::ModelRegistry` was *not* used by this path: the executor called
`binding::materialize_gmat`/`materialize_constant_accel` directly, because fault/maneuver
re-binding and covariance seeding needed the unerased `binding::AnyModel`/`Materialized` shape
a moment longer than the registry's own constructors exposed at the time. As of M10.3,
`registry::ModelHandle` is that shape, made opaque: it wraps `binding::AnyModel` privately and
exposes exactly what a caller needs (`t0_tai_ns`/`x0_si`/`settings` as plain fields,
`state_dim()`/`stm_capable()`/`describe()` as read-only methods, `into_boxed`/`into_boxed_stm`
to erase, consuming `self`) without ever exposing the enum. The executor now calls only
`registry::ModelRegistry::construct_gmat`/`construct_native` -- for an instance's first
segment *and* for every fault/maneuver re-binding, which needs no separate "mutate this
handle" API: `fault::rebind_gmat_spec_at_state` already produces a *fresh* `GmatSystemSpec`
from the previous segment's own final state, and re-binding is just another call to the same
two constructors with that fresh spec. **This executor never names `binding::AnyModel` at
all.**

`binding::AnyModel`/`AnyModelError`/`ConstantAccelModel`/`Materialized` and
`binding::materialize_gmat`/`materialize_constant_accel` are `pub(crate)` as of M10.3, not
`pub` -- `crate::registry` is their only caller. This is the finest visibility Rust allows for
a type defined in `binding.rs` rather than in `registry.rs` itself: `pub(in crate::registry)`
only restricts to an *ancestor* of the defining module, and `binding`/`registry` are siblings
under this crate's root, not each other's ancestor, so that attribute cannot name "just
`registry`" for a type declared in `binding.rs`. `pub(crate)` still technically lets another
module *within* `av-kernel` reach in, but nothing does, and no external crate can any more
(`AnyModel` is no longer part of this crate's public API -- `av_kernel::drm::binding::AnyModel`
does not resolve). What actually enforces "the executor never touches `AnyModel`" is the code
no longer referencing it, stated here rather than left implicit.

`tests/registry.rs` (new, M10.3) exercises `ModelRegistry::construct_gmat` directly, GMAT-gated,
against the golden bundle's own classified `GmatSystemSpec` -- proving the registry works
standalone, not merely as a component `execute()` happens to route through, and directly
asserting `ModelHandle::t0_tai_ns` equals the declared epoch exactly (question 96, below).

### Authoring format: YAML, field-for-field

`av_cdm::pb` types have no `serde` derive (`crates/av-cdm/build.rs` is shared by every crate
depending on `av-cdm` and this task does not touch it), so `src/drm/schema.rs` hand-writes a
`Raw*` struct per proto message with exactly the same fields, names and nesting as its
`.proto` counterpart, and converts field-by-field into the real `pb` type -- an honest
transcription, not a bespoke schema. Enum fields are written as the proto enum's own name
(`"BINDING_KIND_MODEL"`, `"FAULT_TARGET_KIND_DYNAMICS"`, ...) and parsed with the generated
`from_str_name` -- the same string form protobuf's own canonical JSON mapping uses.
`Port`/`Variant`/`FrameDefinition` are not yet fully modeled (ADR-005's port routing and the
frame registry are Planned/partial and this task does not need them): a non-empty one of these
fields is a typed `DrmError::UnsupportedField`, never silently dropped. **`Scenario.events` is
fully typed as of M10.1** (question 97): `RawScenarioEvent` mirrors `ScenarioEvent`
field-for-field, and every event is additionally run through `drm::maneuver::parse` at load
time -- a `kind` other than `"maneuver"`, or a malformed `"maneuver"` event, is a typed load
error, not silently accepted opaque data. See ["Impulsive
maneuvers"](#impulsive-maneuvers-scenarioevents-of-kind-maneuver-m101-question-97) below.

### Hashing: SHA-256 of the canonical protobuf encoding

`src/drm/hash.rs`: clear the message's own `hash` field, encode with
`prost::Message::encode_to_vec` (deterministic -- field-number order, and every map in
`av_cdm::pb` is a `BTreeMap`, so map entries serialize key-sorted too), SHA-256 the bytes, hex.
Exactly `tests/test_cdm_v1.py`'s own Python-side convention
(`hashlib.sha256(msg.SerializeToString(deterministic=True))` with `hash` cleared first).
**`sha2`**, not `openssl`: the task allows `sha2` when it is already a workspace dependency,
and it is (`crates/av-dynamics::settings_hash`) -- reused here rather than adding a second,
unrelated hashing path through `openssl`. `verify_drm_hash`/`verify_sos_hash`/
`verify_system_hash` compare a declared `hash` against the freshly-computed one and return a
typed `DrmError::HashMismatch` on any disagreement; `execute` calls all three before doing
anything else, so a tampered artifact is refused before it can influence a run at all. A
DRM-authoring workflow tool, `cargo run -p av-kernel --example drm_hash -- <drm|sos|system>
<path>`, exposes the same computation standalone.

### Binding: `BINDING_KIND_MODEL` -> a real `DynamicsModel`

`src/drm/binding.rs`. `classify_binding` refuses `BINDING_KIND_RENODE`/`_BOARD` (and an unset
binding) with a typed `DrmError::UnsupportedBinding` -- ADR-005's runtime for either is still
Planned and this crate has no machinery for them. **`BINDING_KIND_CONTAINER` is classified,
not refused, as of M13.2** -- see ["Container (lockstep)
binding"](#container-lockstep-binding-m132-question-107) below. A
`BINDING_KIND_MODEL` instance dispatches on `SystemDefinition.dynamics_model`'s prefix (no
model registry exists yet in this repo, so the id itself is the dispatch key, flagged as
future work): `"gmat."` binds a real `gmat_sys::model::GmatModel`; anything else binds
`ConstantAccelModel`, the same closed-form model this crate's own unit tests already use, per
the task's explicit allowance ("the existing `ConstantAccel`-style test model for anything
else for now"). `classify_binding` itself never touches GMAT (parameter parsing and the
questions-82/83 refusal below are pure data), so it -- and therefore the "non-model bindings
are refused" and "RelativisticCorrection refused" required tests -- run with no GMAT install.

Both binding kinds pull their configuration from `SystemDefinition.parameters` (`repeated
Parameter name/value/string_value/...` -- the CDM has no dedicated force-model/orbital-element
message yet) plus the binding `SystemInstance`'s own `parameter_overrides` applied on top by
name (both are fields of a *hashed* message, never a profile -- squarely inside question 11's
rule that only a DRM or system definition may change model behaviour). The `"gmat."`
vocabulary: `force_model.{central_body,gravity_file,gravity_degree,gravity_order,point_masses,
relativistic_correction,golden_ref}` and `spacecraft.<GmatFieldName>` (forwarded **verbatim**
to `Object::set_real`/`set_str` -- e.g. `spacecraft.SMA`, `spacecraft.CoordinateSystem`, and
question 81's five ballistic fields `spacecraft.{DryMass,Cd,Cr,DragArea,SRPArea}`, one uniform
mechanism for orbital elements and ballistics alike). The native vocabulary:
`accel.{x,y,z}`, `frame_id`, `state.{px,py,pz,vx,vy,vz}`. A parameter matching neither is a
typed `DrmError::UnknownParameter` -- never a silent drop.

**Epoch (question 96, M10.3).** `Scenario.start_tai_ns` (TAI ns, the CDM-native, hashed field)
is the one source of truth for time, **end to end** -- A1MJD is used only to make the one GMAT
call that needs it, never converted back. `materialize_gmat` converts it to GMAT's A1MJD
numerically (`av_cdm::time::Tai::to_a1_mjd`) and sets it via `DateFormat = "A1ModJulian"` -- **as
a string** (`Object::set_str`, confirmed empirically: GMAT's `Epoch` field refuses a real number
under this `DateFormat` too, with "Epoch expects a String value..."), formatted with Rust's own
round-trip-exact `f64` `Display`. Confirmed bit-identical to this repository's existing
`UTCGregorian`-string convention for the golden's own epoch.

Through M9.3, the bound model's own `epoch_tai_ns()` (`gmat_sys::model::GmatModel::
epoch_tai_ns`, which converts GMAT's internal A1MJD *back* to TAI ns) was read back and used as
`Materialized::t0_tai_ns` -- cross-checked against the declared value to within one microsecond
(`EPOCH_CROSSCHECK_TOLERANCE_NS`, sized for the documented `Tai` A1MJD round-trip residual, a
larger disagreement being a typed `DrmError::EpochMismatch`), so every sample epoch downstream
was self-consistent with GMAT's own internal time reference, but never quite the exact integer
`Scenario.start_tai_ns` declared -- and forced the M9.3-era `output.*` GMAT-bound test to assert
`@end`, not `@start`, to dodge the residual at the very first sample.

**As of M10.3, `materialize_gmat` never calls `epoch_tai_ns()` at all.**
`ModelHandle::t0_tai_ns` (`registry::ModelRegistry::construct_gmat`) is simply the caller's own
`epoch_tai_ns`, an exact integer, always -- `EPOCH_CROSSCHECK_TOLERANCE_NS` and
`DrmError::EpochMismatch` are deleted with it (`DrmError::Model(av_dynamics::ModelError)`
reports a genuine GMAT construction failure instead). This does not eliminate A1MJD's own `f64`
imprecision at these epoch magnitudes (question 81) -- it moves where it shows up: GMAT's own
internal time reference for a bound model is fixed at construction from the same `a1mjd` string
this module sends it, and the kernel's own clock (exact-integer TAI ns from `t0_tai_ns` onward)
can now drift from that internal reference by a bounded, one-time residual no larger than the
old cross-check's own tolerance (up to ~252 ns, question 81's own measurement) -- never
compounded by a second round trip. At LEO orbital speeds this is a position residual on the
order of a millimetre. **Measured, not merely estimated:** `drm_matches_the_golden_arc` (the
golden's own `tolerance_m = 0.05`, `tolerance_mps = 5e-5`, not tightened by this task) recorded
`|dr| = 0.0001 m`, `|dv| = 8.155e-8 m/s` before this task (`docs/teamlog/2026-09-02-team-1.md`);
after this change, `|dr| = 0.0001 m` (unchanged to the precision printed), `|dv| = 7.837e-8 m/s`
-- a real, small, disclosed change comfortably inside the pinned tolerance, exactly the size
question 96's own bounded residual predicts (see `binding`'s own module doc comment's "Epoch"
section for the derivation). `tests/drm_executor.rs::output_speed_resolves_against_a_real_gmat_bound_instance`
(M9.3's `output.*` GMAT-bound test) is restored to `@start`, and directly asserts the run's own
first sample lands exactly on `Scenario.start_tai_ns` -- see `binding`'s own module doc
comment's "Epoch" section for the full derivation.

### Container (lockstep) binding (M13.2, `docs/open-questions.md` question 107)

A `BINDING_KIND_CONTAINER` instance is bound either to an **already-running** process at a
declared address (M13.2, `container.<field>` parameters), or -- **M15.3, question 118** -- to
one this crate pulls and runs itself from a declared Docker image, over
`altavista.v1.LockstepService` (`proto/altavista/v1/lockstep.proto`) either way: `Bind` once,
`Step` at the instance's own effective rate, `Shutdown` once at the end. See "Docker image
lifecycle (M15.3, question 118)" below for the second path; the two are mutually exclusive
(`DrmError::InvalidBinding` if a spec declares both).

**The client: `crates/av-lockstep`, not folded into `av-grpc`.** `av-grpc`'s own scope is
`altavista.v1.DynamicsService` fronted by nginx mTLS; lockstep is a different shape of problem
(a stateful Bind-then-many-Steps session with its own protocol-error contract), so it gets its
own crate. `av-lockstep` adds **no new `protoc`/`tonic-build` pass**: `av-grpc`'s own
`build.rs` already compiles every `.proto` under `proto/altavista/v1/` (it globs the
directory), so `av_grpc::pb::lockstep_service_client::LockstepServiceClient` and the
`LockstepBindRequest`/`LockstepStepRequest`/... types already existed; `av-lockstep` depends
on `av-grpc` as an ordinary library and reuses them. Two new `av-grpc/build.rs`
`.extern_path` entries (`.altavista.v1.Port` -> `av_cdm::pb::Port`, `.altavista.v1.PortMessage`
-> `av_cdm::pb::PortMessage`) make `LockstepBindRequest.ports`/`LockstepStepRequest.inputs`/
`LockstepStepResponse.outputs` reference `av_cdm`'s own already-generated types directly
(the same types `av_dynamics::Inbox`/`Outbox` already speak), so a message crosses this RPC
boundary with no field-by-field conversion. `av_lockstep::LockstepClient` is a bare async RPC
transport (`bind`/`step`/`reset`/`shutdown`, nothing more -- sequence tracking and
protocol-error checking live in `av-kernel`, not here); `av_lockstep::BlockingLockstepClient`
wraps it with a single-threaded `tokio::runtime::Runtime` so `ContainerModel` (an ordinary
synchronous `av_dynamics::DynamicsModel`) can call it, the same way a synchronous FFI call
blocks. `LockstepClient::connect_mtls` reuses `av_grpc::tls::connect` (the existing
OpenSSL-backed connector, never `ring`); `LockstepClient::connect_plaintext` needs no TLS
stack at all (`tonic`'s `channel` feature alone speaks plain HTTP/2/h2c over an `http://` URI)
-- **plaintext loopback is for local-subprocess tests only** (the task brief's own rule); a
real deployed container binding sets `container.tls = true`. `cargo tree -p av-lockstep | grep
-i ring` is empty, same as `av-grpc`.

**Parameter vocabulary** (`binding::parse_container_spec`, the same "`SystemDefinition.
parameters` is the established extension point" pattern `force_model.*`/`spacecraft.*`/
`output.*` already use; this module never trusts a pre-declared `ContainerBinding.
lockstep_capable` -- every Bind asks the live process and only believes *that* response):
`container.address` (`"host:port"`, required **unless** `ContainerBinding.image` is set --
see below), `container.tls` (default `false`; refused alongside `image`, question 118: "Bind
over loopback"), `container.ca_file`/`client_cert`/`client_key` (required together iff `tls`),
`container.seed_key` (required -- names an entry in `Scenario.seeds` resolved by
`crate::drm::executor`, ADR-004 "seeds are inputs," the same pattern a maneuver's
`execution_error.seed` already uses; `DrmError::UnknownContainerSeed` if absent),
`container.control_port` (default `50070`, matching `services/lockstep-ref`'s own `Dockerfile
EXPOSE` -- only meaningful alongside `image`).

**`BindingPlan` itself is unchanged** (still exactly `Gmat`/`ConstantAccel` -- `crate::drm::
fault::apply_dynamics_fault` and `crate::registry::ModelRegistry`/`ModelHandle`/`AnyModel`
(neither owned by this task) match on it exhaustively, so a third variant would have been a
breaking change to two files this task may not edit). `classify_binding`'s actual return type
is `binding::Classification` (`Model(BindingPlan)` or `Container(ContainerSpec)`);
`crate::drm::executor::execute`'s Pass 1 keys a container-bound instance's spec into its own
sibling map (`container_plans`, alongside `plans`), and Pass 2 dispatches on which map an
instance's name is in.

**`ContainerModel` carries no physical ODE state: `state_dim() == 0`.** This binding kind's
process integrates a SIGNAL input into a SIGNAL output and exposes named outputs
(`services/lockstep-ref`'s own reference behaviour) -- there is no position/velocity for this
crate to track, so every `TrajectorySample.mean` a container-bound instance produces is an
empty `Vec` (a legitimate, if degenerate, zero-component `StateSpace` -- `crate::interpolate::
classify` accepts one without error). `ContainerModel::step_with_ports` sends one `Step`,
checks **both halves of the protocol contract** -- `response.sequence == the sequence just
sent` and `response.reached_tai_ns == until_tai_ns` -- and stops the run with a typed
`DrmError::ContainerProtocol` (wrapping `binding::ContainerError::SequenceMismatch`/
`ReachedTaiMismatch`) on either mismatch, exactly `lockstep.proto`'s own doc comment ("a
response whose sequence does not match is a protocol error and the run stops," and likewise
for `reached_tai_ns`). `LockstepBindResponse.lockstep_capable == false` (a bare refusal, or a
declared port-set mismatch -- the process's own responsibility per `lockstep.proto`'s "the
bound process must accept exactly this set (names, kinds, directions) or refuse") is
`DrmError::ContainerRefused`, checked once at `Bind`, before any `Step`.

**M14.1 (question 109): a container instance now goes through the same shared `HeteroKernel`
every other instance does.** Through M13.2, `crate::drm::executor::run_container_instance` ran
on its own dedicated loop -- outside `HeteroKernel` entirely -- because `ContainerModel::
state_dim() == 0` gives the multi-rate scheduler's Hermite-velocity sampler nothing useful to
interpolate. That dedicated loop is retired: `crate::drm::executor::run_shared_group` registers
a container instance's `ContainerModel` on the shared kernel exactly like a model instance's
`GmatModel`/`ConstantAccelModel`, via a thin `av_dynamics::DynamicsModel`-implementing wrapper
(`binding::SharedContainerModel`, an `Rc<ContainerModel>` newtype -- see its own doc comment for
why `Rc` rather than moving the model itself: the *same* live connection/`next_sequence` counter
has to be re-erased into a fresh `BoxedModel` at every boundary-bounded span, never re-`Bind`-ed).
0-length Hermite interpolation is never actually reached: a container instance's own step period
must now evenly divide `DrmOptions.sample_interval_s`'s own output period
(`DrmError::ContainerPeriodExceedsSampleInterval` otherwise, a genuinely new restriction this
task's own "Known limitations" section below states plainly) -- every output tick then lands
exactly on one of the container's own native steps, never strictly between two.

Reuses `crate::trajectory::build_trajectory` the same way the old dedicated loop did (via
`HeteroKernel`'s own internal call, not a direct one any more) to assemble the result in the
identical shape every other binding kind's `Trajectory` uses -- `Interpolation::HermiteVelocity`
is still declared unconditionally, harmlessly vacuous here since nothing downstream ever
interpolates between two 0-length samples. `LockstepStepResponse.named_outputs` (already
`BTreeMap<String, f64>`, per `av-cdm`/`av-grpc`'s shared `.btree_map(["."])` determinism
convention) flows straight into `StepResult::outputs` and then the same `NamedOutputSeries`
accumulator every other binding kind already populates -- `output.<instance>.<name>@time`
expressions work identically regardless of binding kind (proven end to end by `tests/drm_
container.rs::container_binding_runs_end_to_end_and_records_binding_hash_and_named_outputs`'s
`MeasureOfEffectiveness`, and by `tests/drm_shared_run.rs`'s new three-instance end-to-end test,
below).

**`binding_hash` into provenance** (question 107's "What to build" item 2):
`LockstepBindResponse.binding_hash` is recorded as `Trajectory.provenance.attributes
["container_binding_hash"]` on this instance's own trajectory -- the same place
`finish_trajectory` already records `system_definition_hash`/`system_definition_id`, rather
than a new `Trajectory` field (`proto/**` is read-only to this task).

**Cross-instance port delivery is now real** (question 109 closes the gap M13.2 disclosed):
`ContainerModel::step_with_ports` accepts and forwards a real `Inbox`, and `run_shared_group`'s
own shared `crate::router::Router` actually delivers a connected sender's `Outbox` into it --
`tests/drm_shared_run.rs::a_native_producer_a_container_and_a_native_consumer_run_end_to_end_
in_one_drm` wires a native producer's SIGNAL through a real `lockstep-ref` process into a native
consumer and checks the value that arrives, not merely that each instance runs.
`services/lockstep-ref`'s own "integrates a SIGNAL input into a SIGNAL output" claim is *also*
still proven standalone against the process directly (`tests/test_lockstep_ref.py`).

**Still not wired, disclosed rather than silently absent:**
- **Maneuvers, and any DYNAMICS fault at all,** naming a container-bound instance are refused
  (`DrmError::ContainerFaultsOrManeuversNotSupported`), never silently ignored -- a container
  instance can never be a maneuver's own target (see `run_shared_group`'s own doc comment); this
  is unchanged since M14.1, only the mechanism enforcing it moved. Through M15.3 a DYNAMICS
  fault of `kind == "power_cycle"` was this refusal's one carved-out exception; **M16.2 (below)
  removes that exception** -- a power cycle is `FAULT_TARGET_KIND_HARDWARE` now, so *every*
  DYNAMICS fault naming a container is refused here, unconditionally.
- **Covariance** on a container-bound instance is refused the same way the native
  `ConstantAccelModel`'s already is (`DrmError::ModelNotStmCapable`) -- a container binding
  is never STM-capable, since it has no ODE state at all; unchanged by M14.1 (the covariance
  path is untouched -- see "One shared kernel run" below).

### `Reset` wired to power-cycle faults, on `FAULT_TARGET_KIND_HARDWARE` (M15.3/M16.2,
`docs/open-questions.md` questions 118/120)

Through M15.2, `Reset` was implemented on both ends (`av_lockstep::LockstepClient::reset`,
`services/lockstep-ref`'s own handler) but never called. **M15.3 wired it** to a
`FAULT_TARGET_KIND_DYNAMICS` fault of `kind == "power_cycle"` naming a container-bound instance,
on the mistaken premise that DYNAMICS, PORT and SENSOR were the only `FaultTargetKind` values
available to this crate -- that premise was the M15.3 brief's own error.
`FAULT_TARGET_KIND_HARDWARE` already exists in `proto/altavista/v1/system.proto` (`= 3`), and
its own doc comment reads "Hardware: Renode peripheral fault, board reset, power cycle" -- it
names a power cycle directly. **The lead decided (question 120): HARDWARE, for containers now
and Renode and boards later.** M16.2 moves the wiring there: a `FAULT_TARGET_KIND_HARDWARE`
fault of `kind == "power_cycle"` naming a container-bound instance calls
`binding::ContainerModel::reset` at the fault's own epoch, with `reason = "fault:<fault id>"`
(`lockstep.proto`'s own `LockstepResetRequest.reason` doc comment names exactly this form),
emits an `EVENT_KIND_FAULT` event the same way a model instance's DYNAMICS fault already does,
and the run continues on the *same* live connection (no re-`Bind`, no plan to change, no state
to re-materialize -- a container instance has neither). See `crate::drm::fault`'s own module doc
comment's "Container power-cycle (HARDWARE)" section for the full account, including why PORT
does not fit and why DYNAMICS never really described a power cycle in the first place.

**The interim DYNAMICS/`"power_cycle"` shape is now a typed load error.** No DRM can keep the
M15.3 shape working: `fault::is_legacy_dynamics_power_cycle` recognizes it, and
`crate::drm::executor::execute` refuses it at load with
`DrmError::PowerCycleFaultMustTargetHardware`, checked before the generic container-fault
refusal so the message names the actual mistake (retarget to HARDWARE) rather than a generic
"not supported."
`tests/drm_container.rs::a_dynamics_fault_of_kind_power_cycle_is_refused_as_a_typed_load_error`
is the required test -- against the pre-M16.2 code this exact fixture is the *positive* case
(`Ok(RunProducts)`), so this test would fail with a panicked `.unwrap_err()` against it.

**HARDWARE naming anything other than a container's own power cycle is refused too, not
silently dropped.** `run_shared_group`'s own boundary-collection loop has exactly two arms
(DYNAMICS naming a model instance, or `fault::is_container_power_cycle` naming a container) --
a HARDWARE fault naming a `BINDING_KIND_MODEL` instance, or naming a container with a `kind`
other than `"power_cycle"`, matches neither and would otherwise vanish silently. Both are
refused explicitly and at load: `DrmError::HardwareFaultNotSupportedOnInstance` (HARDWARE has no
runtime yet for a model instance -- Renode/board bindings are still Planned) and
`DrmError::HardwareFaultKindNotSupported` (a container has no Renode peripheral or board to act
on, only its own process to power-cycle) respectively. The former is required
(`tests/drm_container.rs::a_hardware_fault_on_a_model_instance_is_refused_as_unsupported`); the
latter closes the identical gap for the container side and is included for the same reason,
though not itself individually required
(`tests/drm_container.rs::a_hardware_fault_with_an_unsupported_kind_on_a_container_instance_is_
refused`).

**A HARDWARE/DYNAMICS fault's epoch is checked against the output sampling grid too, now.**
Through M15.3 a power cycle got `DrmError::FaultEpochNotOnSampleGrid`'s protection "for free" by
being DYNAMICS; moving it to HARDWARE without extending that check would have silently reopened
the `HeteroKernel::run_with_ports` horizon-must-be-an-exact-multiple-of-the-output-period
`assert!` this check exists to keep unreachable, for an off-grid power-cycle epoch specifically
-- a container power-cycle boundary is driven through the identical `run_one_span`/
`run_with_ports` call a DYNAMICS boundary is. `crate::drm::executor::execute`'s epoch-grid check
now gates both target kinds.

`crate::drm::executor::execute`'s own load-time validation still refuses every *other* DYNAMICS
fault naming a container instance (unconditionally, as above) --
`tests/drm_container.rs::a_dynamics_fault_on_a_container_instance_is_refused` (`kind ==
"parameter"`) still passes unchanged.

**Required test, end to end (moved from DYNAMICS to HARDWARE; assertions unchanged):**
`tests/drm_container.rs::a_power_cycle_fault_on_a_container_instance_resets_the_integrator_and_
the_run_continues` wires a native SIGNAL emitter into a container instance, lets a real,
nonzero integral accumulate for three steps, fires a power-cycle fault, and asserts the
*post-reset* value the router's already-queued messages produce afterward (`10.0`, not the
`20.0` an executor that never called `Reset` -- or called it but the reference process's own
handler did not really zero anything, or one that never moved `is_container_power_cycle` off
DYNAMICS -- would have produced), plus that exactly one FAULT event was recorded and the run's
samples cover the whole scenario. See that test's own doc comment for the worked timeline and
exactly what a wrong implementation would produce instead.

### Docker image lifecycle (M15.3, `docs/open-questions.md` question 118)

`ContainerBinding.image`/`image_digest` (until now parsed by nobody -- `binding.rs`'s own
pre-M15.3 module doc comment called this out explicitly) are now a second way to bind a
`BINDING_KIND_CONTAINER` instance, mutually exclusive with `container.address`:
`binding::materialize_container` pulls the declared image **by digest**
(`av_lockstep::docker::ManagedContainer::pull_and_run`, `docker pull <image>@<image_digest>`),
runs it with the control port (`container.control_port`, default `50070`) and every declared
`ContainerBinding.port_endpoints` entry published on **loopback** (`127.0.0.1`, ADR-003, question
118: "Bind over loopback" -- `container.tls` is refused alongside `image`), connects
`BlockingLockstepClient::connect_plaintext` to the host port Docker actually picked, `Bind`s
(retried within a bounded readiness window: a freshly started container's TCP listen socket can
accept a connection slightly before its own request-handling thread pool is actually servicing
one -- observed directly while building this), and stores the resulting
`av_lockstep::docker::ManagedContainer` on `ContainerModel` so `ContainerModel::shutdown` can
stop and remove it, in that order, once the run's own `Shutdown` RPC succeeds (question 118:
"Shutdown then stop and remove on run end"). `ManagedContainer`'s own `Drop` best-effort tears
it down too, so a panic or an early `?` between `pull_and_run` and a successful `Bind` can never
leak a running container.

**`binding_hash` includes the digest.** `materialize_container` sets `IMAGE_DIGEST` (an
environment variable on the running container, never an RPC field) to `ContainerBinding.
image_digest`; `services/lockstep-ref/lockstep_ref/server.py`'s own `Bind` handler folds it into
`LockstepBindResponse.binding_hash`'s SHA-256 -- the same `Trajectory.provenance.attributes
["container_binding_hash"]` path every container binding already used. Proven directly (not
merely "a hash is produced") by `crates/av-lockstep/tests/docker_lifecycle.rs`, which runs the
identical pulled image twice with two different `IMAGE_DIGEST` values and asserts the two
resulting hashes differ.

**No image is pushed anywhere** by this crate or its tests. Every test that needs to prove a
*genuine* "pull by digest" (not a locally-cached tag silently reused) builds this repository's
own `services/lockstep-ref/Dockerfile`, starts a throwaway `registry:2` container on loopback,
pushes to *that*, and hands the resulting `<host>:<port>/lockstep-ref@sha256:...` reference to
the code under test -- see `crates/av-lockstep/tests/docker_lifecycle.rs`'s own module doc
comment for the full account. Three tests exercise this, at three levels: `av-lockstep`'s own
(`ManagedContainer` directly, plus the `binding_hash`-varies-with-digest proof above),
`av-kernel`'s (`tests/drm_container.rs
::docker_image_lifecycle_through_execute_pulls_by_digest_runs_binds_and_removes_on_shutdown`,
the same lifecycle through the real `execute()` entry point), and this repository's own
`tests/test_lockstep_ref.py::test_docker_image_lifecycle_pull_by_digest_run_bind_and_remove`
(Rust-independent: proves the *image* itself behaves correctly under a plain `docker` CLI).

**Gated on `docker info`, visibly.** All three tests check `docker info` first and skip with a
recorded reason otherwise (question 118: "tests run only when docker info succeeds ... never
silently, never buried"). Verified directly: a bare `pytest.skip(reason)` is invisible under a
plain `pytest -q` (only a bare "s" and a count) unless pytest is told to report skip reasons --
this repository's `pyproject.toml` now sets `addopts = "-rs"` for exactly that. **Rust has no
equivalent for a *passing* test's own runtime-conditional `println!`**: `cargo test`'s default
runner does not print captured stdout for a test that did not fail, verified directly, so the
two Rust-side tests' own skip-reason prints are best-effort (visible under `cargo test --
--nocapture`, or in CI logs that do not capture) -- the pytest-side test is this repository's
verified-visible location for the requirement. Docker is installed and running in every
environment this task was built and verified against, so in practice all three tests run for
real rather than skip; see each test's own doc comment for this disclosed limitation.

### `services/lockstep-ref`: the reference `LockstepService` process (test fixture only)

`services/lockstep-ref` (Python, `grpcio`) is what `tests/drm_container.rs` and
`tests/test_lockstep_ref.py` spawn as a local subprocess (`python -m lockstep_ref`, no
Docker) -- **a test fixture, not a deployed service**, the same distinction
`services/gmat-service`'s own README draws for itself (design-time only, but a real GMAT
host) versus the deployed Rust `av-dynamics-service`. One SIGNAL input port, one SIGNAL
output port, one named output: integrates the input (explicit Euler, held constant over the
step) into a running total, reported both ways every `Step`. `Bind` validates the caller's
declared `ports` against exactly this expectation, refusing (`lockstep_capable = false`) on
any name/kind/direction mismatch. Test-only misbehaviour knobs, read only from environment
variables (`LOCKSTEP_REF_REFUSE`, `LOCKSTEP_REF_LIE_REACHED_AT_STEP`, `LOCKSTEP_REF_LIE_
SEQUENCE_AT_STEP`) are what `tests/drm_container.rs` uses to prove this crate's own protocol
checks actually stop a run -- see `services/lockstep-ref/README.md` for the full contract,
the misbehaviour knobs, its own `Dockerfile` (M15.3, question 118), and an honest "not
exercised end to end" accounting (mTLS is still the main one; `Reset` no longer is).

### The container binding tests (`tests/drm_container.rs`, M13.2)

Byte-identical `RunProducts` across two runs against two independently started (never
concurrent) `lockstep-ref` processes on the same port; a fixture that lies about
`reached_tai_ns` on exactly one `Step` call is caught (`DrmError::ContainerProtocol`
wrapping `ContainerError::ReachedTaiMismatch`) and the run stops; the `lockstep_capable =
false` refusal and, separately, the port-set-mismatch refusal (both surface as
`DrmError::ContainerRefused`, exercised via two different fixture configurations -- a
`LOCKSTEP_REF_REFUSE=1` process and a normally-configured one given a deliberately
mismatched `SystemDefinition.ports`); a `sequence` mismatch is caught the same way as the
`reached_tai_ns` case. **M15.3 (question 118) added two more:** a power-cycle fault resets a
real, nonzero integral and the run continues (see "`Reset` wired to power-cycle faults" above
for the worked timeline), and the Docker image-lifecycle path runs a real pulled-by-digest
image end to end through `execute()` and removes it on `Shutdown` (see "Docker image lifecycle"
above). **M16.2 (question 120) moved the power-cycle fixture to `FAULT_TARGET_KIND_HARDWARE`**
(assertions unchanged) and added three more: the retired DYNAMICS/`"power_cycle"` shape is a
typed load error, a HARDWARE fault naming a model instance is a typed refusal (required), and
(not individually required) a HARDWARE fault naming a container with an unsupported `kind` is
refused too. Also (not individually required): the happy path proving `binding_hash` reaches
`Trajectory.provenance.attributes` and a named output reaches a `MeasureOfEffectiveness`, and
any DYNAMICS fault naming a container instance refused rather than silently ignored (no
power-cycle exception any more -- see "`Reset` wired to power-cycle faults" above). Every
subprocess is killed in a `Drop` guard, so a failing assertion can never leak a
listening server (this crate's own environment note: "a previous worker left a server
running") -- the two Docker-lifecycle tests apply the identical discipline to the containers
and throwaway registry they start (`Drop` guards around every `docker` resource, on top of
`ManagedContainer`'s own).

### `DrmOptions` -> kernel knobs, field by field (question 87's central point)

| `DrmOptions` field | Drives |
|---|---|
| `sample_interval_s` | `HeteroKernel::new`'s `output_period_ns` -- the trajectory's own sampling grid. |
| `default_step_rate_hz` / a `SystemInstance`'s own `step_rate_hz` (wins if positive) | `HeteroScheduler::register`'s `period_ns` -- the instance's native integration step. |
| `covariance` | Selects `HeteroKernel::run_with_covariance` instead of plain `HeteroKernel::run` (M9.1 -- see "`HeteroKernel` is the only kernel" below; before M9.1 this selected `Kernel<StmAugmented<AnyModel>>::run_with_covariance`/`Kernel<AnyModel>::run`). |
| `nearest_spd_projection` | Passed straight through to `run_with_covariance`'s parameter of the same name (question 83, `crate::kernel`'s existing mechanism -- unchanged by this task). |
| `accept_missing_stm_terms` | Passed to `GmatModel::new` (question 82) **and** checked a second time by `classify_binding`, before any GMAT call, so a covariance request against a declared `RelativisticCorrection` force model is refused early. |
| `real_time` | `true` -> `DrmError::RealTimeNotSupported`. ADR-005's real-time runtime is still Planned and this crate only ever runs lockstep -- refused rather than silently honoured as lockstep anyway. |

Every one of these six fields is read and actually changes behaviour; none is parsed and
dropped.

**Covariance**, concretely: requires the bound model to be STM-capable
(`DrmError::ModelNotStmCapable` otherwise -- the native `ConstantAccelModel` never is), and
reads the initial covariance `P0` from **`SystemInstance.initial_covariance`**
(`proto/altavista/v1/system.proto` field 8,
question 89: row-major `n x n`, SI, packed doubles) -- `DrmError::MissingInitialCovariance` if
empty. **M7.2: the former `covariance.p0_row_major` parameter-string convention is gone --
deleted, not deprecated** (the lead's question 89 decision: an additive, purpose-built proto
field superseded it). `executor::load_initial_covariance` runs this P0 through
`av_cdm::covariance::check_spd_row_major` **at load, before any propagation** -- question 83's
Cholesky-based SPD bar applied to the seed itself, not only to what `Kernel::
run_with_covariance` later propagates from it (see "Covariance hygiene" above): a failure is
`DrmError::CovarianceHygiene` unless `nearest_spd_projection` is set, in which case the same
opt-in nearest-SPD projection is applied to P0 too, exactly the `Kernel::run_with_covariance`
pattern reused rather than reinvented. **Not yet supported together with a DYNAMICS fault on
the same instance** (`DrmError::CovarianceWithFaultsNotSupported`): re-augmenting
`StmAugmented` at every fault boundary the way the plain path re-materializes its model is not
implemented, and this is refused explicitly rather than silently running the covariance path
while ignoring the faults. Stated plainly as a scope limitation, not hidden.

`drms/leo_1day_golden.sos.yaml`'s `leo` instance now carries the golden's own declared P0
(`goldens/leo_1day_jgm2_8x8_sunmoon.json`'s `stm.p0_si`, diag(10000, 10000, 10000, 0.01, 0.01,
0.01) SI -- 100 m position / 0.1 m/s velocity 1-sigma) as `initial_covariance`, the same seed
`tests/golden_acceptance.rs`'s own `kernel_covariance_matches_the_golden_stm_and_propagated_cov`
feeds the kernel directly. `tests/drm_executor.rs::drm_covariance_matches_the_golden_stm_and_
propagated_cov` builds its own `SosConfiguration`/`DesignReferenceMission` around that same P0
and `leo_sys` (a distinct `leo_cov` instance name -- see "GMAT object naming" below for why),
runs the golden's full one-day arc through the *full* DRM executor with `covariance: true`,
and checks the result against `goldens/leo_1day_jgm2_8x8_sunmoon.json`'s `stm.cov_t1_si` with
the same Frobenius relative-error bound `golden_acceptance.rs` uses (< 1e-6) -- **measured
1.697e-11** (near-exact agreement, expected: both paths ultimately drive the same `gmat-sys`
STM integration). `covariance_requested_without_initial_covariance_is_refused` and
`a_non_spd_initial_covariance_is_refused_with_the_typed_hygiene_error_and_counted` cover the
two load-time refusals over a short (0.1 s, one output period) fixture against the same
`leo_sys` system, so they stay fast.

**M13.3: an instance's own effective step rate no longer has to equal `sample_interval_s`.**
`tests/drm_executor.rs::covariance_at_a_coarser_step_rate_than_the_sample_interval_succeeds_
end_to_end` runs a covariance instance at `step_rate_hz = 2.0` (500 ms) under
`sample_interval_s = 0.1` (100 ms, previously the only value that would have been accepted) --
the run now succeeds, with a real covariance (`av_kernel::kernel::covariance` returns `Some`) at
the four 500 ms-aligned ticks (0, 500, 1000, 1500 ms) and `None` at the other twelve -- the full
DRM-executor proof of `crate::kernel`'s own
GMAT-free unit tests, driven through real GMAT propagation. One consequence threaded through
`run_covariance_span`/`run_covariance_instance`: a **maneuver** boundary's own covariance is
carried into the next span's `p0` unmodified (or Gates-injected -- see "Impulsive maneuvers"
below), which only means anything if the boundary actually lands on the instance's own
covariance-native grid, not merely the (coarser-or-equal) `sample_interval_s` grid a maneuver
must already land on. Checked explicitly and refused before ever propagating that span
(`DrmError::ManeuverEpochNotOnCovarianceGrid`, a new variant this task added -- genuinely
needed: without it, a misaligned maneuver would silently carry the NaN "unavailable" sentinel
forward as this instance's next `p0`, poisoning every covariance sample after the burn) rather
than letting a NaN-poisoned `p0` reach the next span's own SPD hygiene check under a confusing
generic "hygiene failure" report. `tests/drm_maneuver.rs::a_maneuver_on_the_sample_grid_but_
off_a_coarser_covariance_grid_is_refused` pins this: a burn at 300 ms is on the 100 ms sample
grid (so `ManeuverEpochNotOnSampleGrid` does not fire) but not on a 500 ms covariance grid.
The scenario's own end is never checked this way -- the final span's `cov` is never carried
anywhere further, so an off-grid run end simply means the trajectory's last sample has no real
covariance, the documented "off-grid produces no covariance sample" behaviour, not an error.

### Fault injection: `FAULT_TARGET_KIND_DYNAMICS`

`src/drm/fault.rs`. Only `kind == "parameter"` is supported (`Fault.kind`'s other named kinds
-- `"drop"`, `"bias"`, ... -- apply to ports/sensors/hardware, not dynamics; any other kind on
a DYNAMICS fault is a typed error). `Fault.target` names one parameter from the same
vocabulary above (`force_model.gravity_degree`, `spacecraft.Cd`, `accel.x`, ...);
`Fault.params["value"]` is the new value. `executor::execute` splits a plain (non-covariance)
run into one `TrajectorySegment` per fault-bounded span -- exactly the "contiguous span
produced by one propagation with one dynamics configuration" `TrajectorySegment`'s own doc
comment describes -- re-binding at each boundary from the previous segment's own final
physical state (continuity: position/velocity carry over exactly; only the named parameter
changes). A GMAT-bound instance is rebuilt with `DisplayStateType = "Cartesian"` and
X/Y/Z/VX/VY/VZ set from that boundary state (never by re-deriving Keplerian elements, which
would need an osculating-element computation this crate does not have or need). Every
DYNAMICS (and, as of M16.2, HARDWARE) fault's `tai_ns` must land exactly on the
`sample_interval_s` output grid, checked up front before any GMAT call
(`DrmError::FaultEpochNotOnSampleGrid`) -- `Kernel::run` panics on a sub-run horizon that is not
an exact multiple of its output period, and this turns that potential panic into a typed
refusal before it can happen. See "`Reset` wired to power-cycle faults" below for
`FAULT_TARGET_KIND_HARDWARE`, the one other fault target kind this crate applies today.

Verified end to end (not just "it ran without panicking") by
`tests/drm_executor.rs::a_dynamics_fault_splits_the_run_into_two_segments_with_continuous_
state`: a native `accel.x` binding, one fault changing `accel.x` from 1.0 to 5.0 m/s^2 at the
1 s midpoint of a 2 s run, checked against the closed-form constant-acceleration solution
independently on each half (`x(1s)=0.5, vx(1s)=1.0`, then `x(2s)=4.0, vx(2s)=6.0`), with
`segments.len() == 2` and the two segments' `dynamics_hash` differing. Entirely GMAT-free.

### Impulsive maneuvers: `Scenario.events` of kind `"maneuver"` (M10.1, question 97)

`src/drm/maneuver.rs`. Question 97: "the scenario carries no impulsive maneuvers today...
next batch: accept scenario events of kind `maneuver` (frame, delta-v vector, epoch), apply
them between steps at the exact epoch, emit `EVENT_KIND_MANEUVER`, and pin against the
altavista maneuver path (VNB burn) on a golden." All four are done.

**Schema** (also enforced by `schema::RawScenarioEvent`, see above): a `maneuver`
`ScenarioEvent` must have a non-empty `instance`; `values` containing *exactly* `dv_x`/`dv_y`/
`dv_z` (SI m/s, in the declared frame's own basis order); `attributes` containing *exactly*
`frame_id`, one of `AXES_KIND_RIC`/`AXES_KIND_VNB`/`AXES_KIND_VVLH`/`AXES_KIND_ICRF`/
`AXES_KIND_MJ2000_EQ` (the proto enum's own name, matching every other enum field this loader
parses). Anything else is a typed load error (`DrmError::UnsupportedScenarioEventKind`,
`MissingParameter`, `UnknownParameter`, `InvalidEnumValue`, or `ManeuverFrameNotSupported`) --
`maneuver::parse` is the one place this contract lives, called from both `schema` (load time)
and `executor` (run time).

**Frame realization**, pinned against `altavista.scenario.Scenario.maneuver`'s own VNB path
(`maneuver::dv_to_inertial`): `V = v/|v|`, `N = (r x v)/|r x v|`, `B = V x N` for VNB (field-
for-field the same `V`/`h`/`N`/`B` construction `altavista/scenario.py::Scenario.maneuver`'s
`frame.upper() == "VNB"` branch uses); RIC (`X=R, Z=N, Y=N x R`) and VVLH (`Z=-R, Y=-N,
X=N x R`, ratified question 73) reuse the same `R`/`N` building blocks from
`proto/altavista/v1/core.proto`'s `AxesKind` doc comment. ICRF/MJ2000Eq apply `dv` unrotated
(every binding in this crate already propagates in an inertial frame). Only VNB is pinned
against altavista's own reference implementation -- altavista's `Scenario.maneuver` itself only
ever realizes `"VNB"` or `"inertial"`; RIC/VVLH are implemented from the same ratified
convention and covered by orthonormal-basis unit tests, not an external golden (a stated scope
limit, not a hidden one).

**Applying a burn: the same "split the run at this epoch, re-bind from the segment's own final
state" shape a DYNAMICS fault already gets**, except a maneuver changes the *state* (an
instantaneous velocity jump), never the dynamics configuration -- `executor::
apply_maneuver_to_state` adds `dv_to_inertial`'s result to the segment's own final velocity;
`materialize_plan_at_boundary` re-binds the *same*, unmodified plan at the new state. Faults
and maneuvers on one instance are merged into a single `(tai_ns, id)`-sorted boundary list
(`executor::Boundary`) and applied together, in order. A maneuver's epoch must land exactly on
the `sample_interval_s` output grid too (`DrmError::ManeuverEpochNotOnSampleGrid`, modeled
directly on `FaultEpochNotOnSampleGrid`).

**The kept sample at a maneuver boundary is the post-burn one, not both.** Unlike a fault
boundary (state fully continuous -- either of the two coincident samples is identical, so
either may be kept), a maneuver boundary is a real velocity discontinuity.
`executor::append_span_samples` (M14.1; the pre-M14.1 `run_span` did this inline) pops the
previous segment's already-appended last (pre-burn) sample instead of dropping the new
segment's first (post-burn) one when the boundary just crossed was a maneuver -- so exactly one
sample is ever recorded per epoch, and `Interpolation::HermiteVelocity`'s distinct-epoch
contract is never handed a zero-width interval. This is a deliberate design choice, not merely
mirroring altavista (whose own `Trajectory` format has no interpolation-continuity contract and
does record both points at the same epoch): see `append_span_samples`'s own doc comment.

**Covariance across a burn is supported** (unlike DYNAMICS faults, still refused together with
covariance): for an impulsive burn with **no execution error** (question 97's own scope --
burn dispersion is explicitly out of this task), Phi is unchanged and P is unchanged across the
boundary. Implemented as: every maneuver boundary starts a **new**
`HeteroKernel::run_with_covariance` call (`executor::run_covariance_span`, `Phi(seg_start,
seg_start) = I` by `StmAugmented::seed`'s own construction) fed the *previous* span's own final
`cov` as its `p0`, copied through unmodified, never recomputed -- so the only way a burn could
change `P` would be a bug in that carry-through. `tests/drm_maneuver.rs::covariance_is_
unchanged_across_a_no_execution_error_burn` proves this through the public `execute()` API
alone: a covariance run split by a **zero-dv** maneuver must match, to a `< 1e-6` relative
Frobenius tolerance, a run over the identical span with no maneuver event at all (measured
**2.348e-16** relative covariance error, **0.0 m**/**0.0 m/s** mean error -- see
`run_covariance_instance`'s own "Covariance across a burn" doc comment for the full argument).
**Resolved in M11.4** (question 100): burn execution error is now modeled -- see the "Burn
execution error: the Gates model" section immediately below.

**The golden** (`goldens/leo_1day_maneuver_vnb.json`, `goldens/gen_leo_1day_maneuver_vnb.py`):
the golden LEO orbit (`drms/leo_1day_golden.system.yaml`'s own `leo_sys` -- same vehicle, same
JGM2 8x8 + Sun/Moon force model), 1 hour of propagation, a 20 m/s prograde VNB burn applied
through **altavista's own `Scenario.maneuver` call** (not a hand re-derivation of its formula),
then another hour of propagation. `tests/drm_maneuver.rs::drm_matches_the_maneuver_golden_
vnb_burn` runs the equivalent DRM (`drms/leo_1day_maneuver_vnb.*.yaml`) through the executor
and checks the sample at the burn's own applied epoch against the golden's `state_post_burn`,
and the final sample against `final_state`, at the same tolerance class as
`leo_1day_jgm2_8x8_sunmoon.json` (0.05 m / 5e-5 m/s). **Measured: |dr| = 0.0000 m, |dv| =
1.222e-9 m/s at the burn; |dr| = 0.0000 m, |dv| = 2.048e-9 m/s at the end** -- both far inside
tolerance, no tolerance loosened.

A second, entirely GMAT-free test (`tests/drm_maneuver.rs::a_maneuver_splits_the_run_and_the_
kept_boundary_sample_is_the_post_burn_one`) checks the boundary mechanics exactly, in closed
form, against a native `accel.x` binding.

### Burn execution error: the Gates model (M11.4, `docs/open-questions.md` question 100)

`src/drm/maneuver.rs`'s "Burn execution error" doc comment section. Question 100: impulsive
burns were perfect (Phi = I, P unchanged across the boundary); the user chose the **Gates
model** (S. Gates, *"A Simplified Model of Midcourse Maneuver Execution Errors,"* JPL Technical
Report 32-1234, 1963) -- fixed and proportional 1-sigmas for magnitude error (along the
commanded `dv`) and pointing error (in the plane transverse to it). `ScenarioEvent.
execution_error` (`ManeuverExecutionError`, added by the lead, proto field 7) carries the four
sigmas plus a `seed` key into `Scenario.seeds`; **absent means a perfect burn and is never
defaulted** -- a present block with all four sigmas `0.0` is an explicit zero, not an omission,
and both leave the applied `dv` numerically identical to the commanded one.

**The model, exactly** (`maneuver::gates_sigmas`): for commanded `dv` of magnitude `v = |dv|`
and unit direction `u = dv/v`, completed into an orthonormal triad `(u, p1, p2)`
(`maneuver::burn_triad`, a deterministic Gram-Schmidt completion -- falls back to the fixed
canonical basis for the degenerate `dv = 0` case, an explicit, stated approximation not expected
in a real DRM): `sigma_m^2 = sigma_magnitude_fixed_mps^2 + (sigma_magnitude_proportional * v)^2`;
`sigma_p^2 = sigma_pointing_fixed_mps^2 + (sigma_pointing_proportional_rad * v)^2` (the same
`sigma_p` for both transverse axes -- the model has no separate "up"/"down" pointing sigma).

**Two paths, selected by which executor function applies the burn -- never a separate declared
"mode":**

- **(a) Sampled** (`maneuver::sample_execution_error`, called from `executor::
  run_shared_group`, the non-covariance path -- one realized trajectory, exactly what a Monte
  Carlo sweep draw needs): draw three standard normals from `crate::rng::event_rng(base_seed,
  event_id)` (`base_seed` = `Scenario.seeds[execution_error.seed]`, `event_id` =
  `ScenarioEvent.id`) -- **a fresh substream per event id**, so adding or removing an unrelated
  event, even one sharing the same `Scenario.seeds` key, never shifts this event's own draws
  (`crate::rng::event_rng` XORs a hand-rolled FNV-1a hash of the event id onto the shared seed,
  so one event's stream is a pure function of its own `(base_seed, event_id)` pair alone).
  Applied `dv = dv + z0*sigma_m*u + z1*sigma_p*p1 + z2*sigma_p*p2`, run through the same
  `dv_to_inertial` every commanded burn already uses. The MANEUVER event's `values` record both
  the commanded and applied vectors (`dv_x`/`_y`/`_z` and `applied_dv_x`/`_y`/`_z`/`_mps`) and
  the three raw draws (`draw_magnitude`/`draw_pointing_1`/`draw_pointing_2`).
- **(b) Analytic injection** (`maneuver::inject_gates_covariance`, called from `executor::
  run_covariance_instance`): the commanded `dv` is applied **exactly** (never perturbed --
  `run_covariance_instance` tracks a mean and a covariance, not a realized dispersion), and
  `P+ = P- + G Q G^T` is injected at the burn epoch, where `G` maps `(u, p1, p2)` -- rotated
  into the trajectory's own inertial frame by `maneuver::inertial_triad`, the same rotation
  `dv_to_inertial` itself applies -- into the velocity block of the state (indices 3..6), and
  `Q = diag(sigma_m^2, sigma_p^2, sigma_p^2)`. Since `(u, p1, p2)` is orthonormal, this reduces
  to adding `sigma_m^2 (u u^T) + sigma_p^2 (p1 p1^T + p2 p2^T)` into the velocity-velocity 3x3
  block; every other entry of `P` (position-position, position-velocity, anything beyond index
  6) is untouched. **Phi is not modified** -- only `P` changes at the boundary, exactly as the
  no-execution-error case already established.

**Loader** (`schema::RawManeuverExecutionError`): all four sigmas required and checked finite
when the block is present (`DrmError::InvalidManeuverExecutionError` otherwise);
`execution_error.seed` must name a real `Scenario.seeds` key, checked both at load
(`schema::RawScenario::into_pb`, which has `Scenario.seeds` in scope) and at run time
(`executor::execute`, for a `Scenario` built directly, bypassing the loader) --
`DrmError::UnknownManeuverSeed` otherwise. The four-sigma finiteness check and the seed-key
check are two separate concerns living at two different scopes (`maneuver::parse` only ever
sees one bare `ScenarioEvent`; `maneuver::validate_execution_error_seed` needs the surrounding
`Scenario`), mirroring how `maneuver::parse` itself is already called from both `schema` and
`executor`.

**The acceptance tests** (`tests/gates_execution_error.rs`, six tests, all against the real
`execute()` API or the real production functions directly -- never a re-derived shadow
implementation):

- **Byte-identical determinism**: two sampled (non-covariance) runs with the same
  `Scenario.seeds` value produce bit-identical trajectories and identical MANEUVER event
  `values` (applied dv and raw draws).
- **Zero-sigma reproduces the perfect-burn golden bit-for-bit**: a present, all-zero
  `execution_error` block and a wholly absent one produce bit-identical trajectories against
  `leo_sys`/`leo_1day_maneuver_vnb.json`'s own epoch/dv -- measured **|dr| = 0.0000 m, |dv| =
  1.982e-9 m/s** against the read-only golden itself (same tolerance class as the M10.1
  maneuver golden; the golden was not touched).
- **The analytic injection matches the sample covariance of `N = 500,000` independently-seeded
  sampled draws**, to a tolerance of 5 standard errors per matrix element derived from sampling
  theory (`sigma^2 * sqrt(2/(N-1))` for a diagonal/variance entry, `sigma_a*sigma_b/sqrt(N)` for
  an off-diagonal entry whose true covariance is `0`) -- fixed before the sampling loop ran, not
  tuned to the result. **Measured: max deviation 1.40 standard errors** across all nine 3x3
  entries (full breakdown in the test's own `eprintln!`), comfortably inside the 5-SE tolerance
  and consistent with correct Gaussian sampling (no finding to report).
- **A proportional-only block scales with `|dv|` as expected**: deterministic (no sampling
  needed, since the analytic path is a pure function) -- a 4x `|dv|` burn injects exactly 16x
  the covariance (sigma scales linearly, variance quadratically), checked to `< 1e-9` relative
  error.
- **Fresh substream per event id, end to end**: adding a second maneuver event (on a second
  instance, sharing the *same* `Scenario.seeds` key) never changes the first event's own
  MANEUVER event `values` -- proven through the full DRM executor, not only at
  `crate::rng::tests::event_substream_is_independent_of_other_events`'s RNG-module level.
- **The covariance path's own wiring**: `P+ - P-` (two otherwise-identical covariance runs,
  with and without `execution_error`) matches the analytically-computed `G Q G^T` (computed
  independently through the same `inertial_triad`/`inject_gates_covariance` functions the
  executor calls) to a **measured 8.674e-17 relative error** -- floating-point noise, not an
  approximation.

**Stated approximations** (see `maneuver.rs`'s own doc comments for each): (1) `burn_triad`'s
degenerate `dv = 0` fallback to a fixed canonical basis, not expected in a real DRM; (2)
`crate::rng::Pcg64::standard_normal`'s Box-Muller transform discards the second normal the
transform naturally produces per call (documented as a deliberate simplification: caching it
across calls would make one "logical" `standard_normal()` call sometimes consume the underlying
stream differently than other calls, which is a subtler determinism hazard than the wasted
draw); (3) the per-event substream key folds the event id through a hand-rolled, non-
cryptographic FNV-1a hash (`crate::rng::event_rng`) rather than a cryptographic one -- adequate
for well-distributing arbitrary id strings into independent PCG64 seeds, and deliberately kept
disjoint from `drm::hash`'s SHA-256 canonical-artifact hashing (`deny.toml`'s crypto rules are
unaffected).

### Provenance (question 87's "What to build" item 6)

`Trajectory.config_hash` is the **DRM's own hash** (its field's own doc comment: "SHA-256 of
the DRM configuration this trajectory was produced from"). `Trajectory.provenance
.config_hash` carries the **`SosConfiguration`'s** hash (the configuration the DRM's
`sos_configuration_id` names); `.data_pack_hash` is `Scenario.data_pack_hash`; `.run_id` is
the caller-supplied `RunConfig.run_id`; each instance's own `SystemDefinition`'s hash and id
are recorded in `.attributes` (there is no dedicated proto field for a *third* hash on
`Provenance`, and `attributes` is exactly the "free-form" escape hatch its doc comment
describes). `.created_tai_ns` is left `0`, deliberately: this crate never reads the wall clock
(ADR-002/ADR-004 determinism); a caller wanting a real creation timestamp must supply one
itself (not yet threaded through `RunConfig`, since no required test needs it) rather than
this module reaching for `SystemTime::now()`. `RunConfig.run_id` is the caller's own field
precisely so a caller can derive one from a clock *outside* this crate and keep it out of
anything hashed -- it never touches `hash::canonical_*`.

**M9.1 (question 93): `RunProducts.provenance` is the run's own overall provenance**, built
the same way (`config_hash` = the DRM's own hash, `attributes["sos_configuration_hash"]` =
the `SosConfiguration`'s), so a caller wanting "what produced this whole run" does not have
to read it off an arbitrary per-instance `Trajectory`. Every individual `Trajectory` keeps its
own per-instance `Provenance` exactly as described above, unchanged.

### The required tests (`tests/drm_executor.rs`)

| Test | What it checks | Needs GMAT? |
|---|---|---|
| `drm_matches_the_golden_arc` | `drms/leo_1day_golden.*.yaml` runs end to end and matches `goldens/leo_1day_jgm2_8x8_sunmoon.json` within its own tolerance; `Trajectory.config_hash`/`.provenance` populated correctly. | Yes (~24 s, same cost as `golden_acceptance.rs`'s own 10 Hz run -- see that section above for why). |
| `a_tampered_drm_hash_is_refused`, `a_drm_whose_content_no_longer_matches_its_declared_hash_is_refused` | A tampered/edited-after-hashing DRM is refused with `DrmError::HashMismatch`. | Constructs a `Gmat` handle (cheap after the first call, `Once`) but never reaches a GMAT call -- refused during hash verification. |
| `covariance_with_relativistic_correction_is_refused_unless_accepted` | Questions 82/83: `covariance=true` against a declared `RelativisticCorrection` force model, without `accept_missing_stm_terms`, is refused. | Same as above -- refused during `classify_binding`, before any GMAT call. |
| `a_renode_binding_is_refused_through_the_full_executor` | `BINDING_KIND_RENODE` (still Planned/unsupported) is refused with `DrmError::UnsupportedBinding` -- `BINDING_KIND_CONTAINER` is real, classified behaviour as of M13.2 (see `tests/drm_container.rs`), so this no longer exercises the container binding kind for this purpose. | Same as above. |
| `a_dynamics_fault_splits_the_run_into_two_segments_with_continuous_state` | Not individually required; DYNAMICS fault injection end to end (see above). | No -- native binding only. |
| `drm_covariance_matches_the_golden_stm_and_propagated_cov` | M7.2/question 89: not individually required by M6.1, but the DRM-level proof that `SystemInstance.initial_covariance` reaches the kernel and reproduces `stm.cov_t1_si`. | Yes (~90 s together with `drm_matches_the_golden_arc` in the same `cargo test` invocation -- covariance's `StmAugmented` state is 7x wider per sample than the plain path). |
| `covariance_requested_without_initial_covariance_is_refused`, `a_non_spd_initial_covariance_is_refused_with_the_typed_hygiene_error_and_counted` | M7.2/question 89/83: the two load-time refusals (`DrmError::MissingInitialCovariance`, `DrmError::CovarianceHygiene`). | Yes, but a 0.1 s fixture (one output period) against the golden's own `leo_sys`, not the full arc -- fast. |
| `covariance_and_plain_paths_produce_the_same_named_output_set` | Question 101/M11.2: not individually required by M6.1, but this task's own "same product name set" proof -- two DRMs over the same `leo_sys` definition, one plain, one covariance, both declaring `output.rmag`/`output.cd`; both `execute()` calls must score every declared measure (would fail with `DrmError::InvalidExpression` if either path failed to carry outputs), `cd` must agree to floating-point noise (a static spacecraft property), `rmag` to a tight bound. | Yes, but a 0.1 s fixture (one output period) -- fast. |

Lower-level GMAT-free unit tests (`src/drm/{schema,hash,binding,fault,maneuver,events}.rs`'s
own `#[cfg(test)]` modules) additionally cover: every `BindingKind` variant's refusal
(`RENODE`/`BOARD`/unset, not just `CONTAINER`), unknown parameter/enum names, hash stability
and tamper-sensitivity in isolation, the fault-application logic in isolation from the kernel
run around it, and (M10.1) `maneuver::parse`'s full schema contract plus `dv_to_inertial`'s
VNB/RIC/VVLH/inertial basis math against a hand-checkable orbit state.

**M14.1's own required tests are `tests/drm_shared_run.rs`** -- see "One shared kernel run"
above for the three tests it carries (the producer/container/consumer end-to-end run, the
byte-identical-across-two-runs proof, and the two-unconnected-instances proof) -- not this
file's own table, which predates that task.

### The maneuver tests (`tests/drm_maneuver.rs`, M10.1)

| Test | What it checks | Needs GMAT? |
|---|---|---|
| `drm_matches_the_maneuver_golden_vnb_burn` | `drms/leo_1day_maneuver_vnb.*.yaml` matches `goldens/leo_1day_maneuver_vnb.json` (altavista's own VNB burn path) at the post-burn and final epochs; the `EVENT_KIND_MANEUVER` event's `values`/`frame_id`. | Yes (~1 s, a 2-hour arc at 60 s output sampling). |
| `a_maneuver_splits_the_run_and_the_kept_boundary_sample_is_the_post_burn_one` | Closed-form boundary mechanics: position continuity, the velocity jump, exactly one (post-burn) sample kept at the epoch, two segments, the maneuver event. | No -- native binding only. |
| `a_maneuver_epoch_off_the_sample_grid_is_refused_before_any_binding`, `a_maneuver_naming_an_unknown_instance_is_refused`, `a_maneuver_with_an_unsupported_frame_is_refused_through_the_full_executor` | The three typed load-time refusals. | Constructs a `Gmat` handle but never reaches a GMAT call. |
| `covariance_is_unchanged_across_a_no_execution_error_burn` | Question 97 item 5: a covariance run split by a zero-dv maneuver matches a run with no maneuver at all (< 1e-6 relative covariance error). | Yes (~0.4 s arc, fast). |

### The Gates execution error tests (`tests/gates_execution_error.rs`, M11.4, question 100)

| Test | What it checks | Needs GMAT? |
|---|---|---|
| `sampled_runs_are_byte_identical_across_two_runs_with_the_same_seed` | Two sampled (plain-path) runs sharing a `Scenario.seeds` value produce bit-identical trajectories and MANEUVER event `values`. | Constructs a `Gmat` handle, native binding only. |
| `a_zero_sigma_execution_error_block_reproduces_the_perfect_burn_golden_bit_for_bit` | A present, all-zero `execution_error` block matches a wholly absent one bit-for-bit, and both match `leo_1day_maneuver_vnb.json` at its own tolerance. | Yes (~1 s, the M10.1 golden's own 2-hour arc, twice). |
| `analytic_gates_injection_matches_the_sample_covariance_of_n_sampled_draws` | `N = 500,000` sampled draws' covariance vs. the analytic `G Q G^T`, to a 5-standard-error tolerance derived from sampling theory. Measured: 1.40 SE max. | No -- calls `maneuver::sample_execution_error`/`inject_gates_covariance` directly. |
| `a_proportional_only_execution_error_block_scales_the_analytic_injection_with_dv_squared` | A 4x `|dv|` burn injects exactly 16x the covariance (deterministic, `< 1e-9` relative error). | No. |
| `adding_a_second_maneuver_event_does_not_shift_the_first_ones_sampled_draw` | The "fresh substream per event id" property, end to end: a second maneuver sharing the same `Scenario.seeds` key never changes the first's own draw. | Constructs a `Gmat` handle, native binding only. |
| `the_covariance_path_injects_exactly_the_analytic_gates_term_at_the_burn_epoch` | `run_covariance_instance`'s own wiring: `P+ - P-` matches the independently-computed analytic injection (measured 8.674e-17 relative error). | Yes (~0.2 s arc, fast). |

### The registry tests (`tests/registry.rs`, M10.3)

`registry::ModelRegistry`'s own module doc comment promises these -- proving the registry works
standalone (not merely as a component `execute()` happens to route through) and, concretely,
that question 96's fix is real: `ModelHandle::t0_tai_ns` is exactly the declared epoch.

| Test | What it checks | Needs GMAT? |
|---|---|---|
| `construct_gmat_builds_a_steppable_model_with_the_exact_declared_epoch` | `ModelRegistry::construct_gmat` against the golden bundle's own classified `GmatSystemSpec`; `ModelHandle::t0_tai_ns == Scenario.start_tai_ns` exactly; the erased `ModelHandle` steps. | Yes, but a single 10 s step against the golden's own spec -- fast. |
| `construct_gmat_reports_a_gmat_ffi_failure_as_a_typed_model_error` | An empty `gravity_file` (a genuine GMAT FFI refusal, not fabricated) surfaces as `ModelError::Gmat` naming the constructor's own `model_id`. | Yes, but no propagation -- fails at construction. |

## Scoring: the expression language and `RunProducts` (`expr`, ADR-005 sec 6, M9.1)

`docs/adr/005-simulation-kernel.md` section 6 defines a small, total, unit-checked expression
language `Objective`/`MeasureOfEffectiveness.expression` are written in. M8.2 implemented it;
this task (M9.1) found the lead's amendment note was right to be skeptical of its own claim
("the evaluator... implements this corrected grammar") -- **it did not**, at the point this
task started: comparison operators (`<`, `<=`, `>`, `>=`, `==`, `!=`) were not lexed at all,
and `call` (so `range(a, b)@time`) had no `@time` suffix, exactly the two gaps the amendment
itself names as defects in the *original* EBNF. `crate::expr::parser`'s own module doc comment
now states this history explicitly. This task implements the amendment's corrected grammar:

```text
expr       := comparison
comparison := sum [('<' | '<=' | '>' | '>=' | '==' | '!=') sum]   (non-associative)
sum        := term (('+' | '-') term)*
term       := unary (('*' | '/') unary)*
unary      := '-' unary | postfix
postfix    := primary ['@' time]                                  (applies to any primary, calls included)
primary    := number [unit] | call | ref | '(' expr ')'
call       := name '(' [expr (',' expr)*] ')'
ref        := ident ('.' ident)*
time       := 'start' | 'end' | number 's' | ident
```

**`range`/`duration`, disclosed** (the ADR names them but not their numerics, the same gap
its prose already leaves for `min`/`max`/`mean`/`final`/`integral`, disclosed in `crate::
expr::eval`'s own module doc comment): `range(<a>, <b>)` is the Euclidean distance between two
entities' `pos_x`/`pos_y`/`pos_z`, in metres; `duration(<condition>)` requires its argument to
literally be a `comparison` whose bare side is a series (an entity/output reference or a bare
`range(...)`), evaluated pointwise at the series' own native samples with **zero-order hold
from the left sample of each interval** -- the same "hold until the next sample" rule ADR-005
section 3 already uses for discrete `StateSpace` components, applied here to a derived boolean
condition. `goldens/expr_range_duration.json` pins the amendment's own worked example,
`duration(range(a, b) < 100 m)`, against a real two-entity DRM.

**`ExprRunProducts` vs. `drm::RunProducts`** -- deliberately two different, differently-named
types, not one type doing both jobs (`crate::expr`'s own module doc comment has the full
reasoning). `drm::RunProducts` (question 93) is `execute()`'s owned return value: `trajectories:
BTreeMap<String, Trajectory>`, `events: Vec<Event>` (M9.3, question 95: every real `Event` this
run produced -- see ["Events"](#events-runproductsevents-drmevents-question-95-m93) below),
`scores: BTreeMap<String, Score>` (`Score { value, unit, passed: Option<bool> }` -- `Some(bool)`
for an `Objective`, `None` for a `MeasureOfEffectiveness`), and `provenance`.
`expr::ExprRunProducts` is the evaluator's own borrowed, read-only view built *from*
`RunProducts.trajectories`/`.events`, used only while scoring is in progress.

**`DrmError::InvalidExpression { name, reason }`, validated at load, before any propagation**
(question 93): `execute` parses and unit-checks every declared `Objective`/
`MeasureOfEffectiveness.expression` against a *declared-shape-only* `ExprRunProducts` (every
instance's `entity_id`/`state_space_id`, empty `samples`) immediately after classifying every
instance's binding and *before* a single GMAT call -- `crate::expr::typecheck::check` never
touches sample data, which is exactly what makes this possible. A malformed expression (a typo
in a component name, a unit mismatch) fails fast instead of surfacing only after a run that can
take tens of seconds. After every instance has actually run, `execute` evaluates the same
expressions again (this time for real, against the real `Trajectory`s) into `RunProducts.
scores` -- the second pass is not redundant: a `number 's'` time offset past the scenario's own
duration passes load-time validation (typecheck never range-checks a fixed offset) but can
still fail at evaluation time, surfacing as the same `DrmError::InvalidExpression` rather than
an undocumented second error shape.

## Events (`RunProducts.events`, `drm::events`, question 95, M9.3)

`execute()` now emits real CDM `Event`s, collected across every instance and sorted `(epoch,
id)` (`events::epoch_id_order`, the same tie-break `fault::epoch_id_order` already applies to
fault application order) before scoring and before being returned as `RunProducts.events`:

- **`EVENT_KIND_LIFECYCLE`** -- a `run_start`/`run_end` pair per `SystemInstance`, at that
  instance's own *actual* propagation start/end (a GMAT-bound instance's own epoch
  reconciliation can differ from the declared `Scenario.start_tai_ns` by the documented A1MJD
  round-trip tolerance -- `entity_id` is the instance name, since `EventKind::
  EVENT_KIND_LIFECYCLE`'s own doc comment ties it to one entity).
- **`EVENT_KIND_FAULT`** -- one event per `FAULT_TARGET_KIND_DYNAMICS` fault actually applied,
  and (M15.3/M16.2) one per `FAULT_TARGET_KIND_HARDWARE`/`"power_cycle"` fault naming a
  container instance too (a fault outside the executed span gets none, exactly like it gets no
  segment split). `values` is `Fault.params` verbatim: a DYNAMICS/`"parameter"` fault draws no
  random element (`fault`'s own module doc comment -- "there is nothing to draw"), so there is
  no distinction between "declared nominal" and "realized" for this fault kind; a power-cycle
  fault carries no declared `params` at all, so `values` is simply empty for it. `name`/
  `reference_id` are `Fault.id` (a `Fault` has no separate `name` field).

- **`EVENT_KIND_MANEUVER`** (M10.1, question 97) -- one event per `"maneuver"` `ScenarioEvent`
  actually applied (a maneuver outside the executed span gets none, exactly like it gets no
  segment split). `values` carries the declared `dv_x`/`dv_y`/`dv_z` (SI m/s, in the declared
  frame's own basis order) plus `dv_mps` (the magnitude); `frame_id` is the declared `AxesKind`'s
  own proto name. `name`/`reference_id` are the `ScenarioEvent.id`. See ["Impulsive
  maneuvers"](#impulsive-maneuvers-scenarioevents-of-kind-maneuver-m101-question-97) above.

- **`EVENT_KIND_PORT_COMMAND`** (M19.3, `docs/open-questions.md` question 130) -- one event per
  SIGNAL port command a bound instance's own `step_with_ports` call actually *applied* to its
  bound configuration (a message that arrives but fails to decode, or no message on the port at
  all, applies nothing and emits nothing -- "applied" is the bar, not "wired for it"). `entity_id`
  is the applying instance; `name`/`reference_id` are the commanded field/port; `values["value"]`
  and `tai_ns` carry the applied value and epoch. The instance, parameter, value, epoch and
  sender are also written as strings on `provenance.attributes` (`Event` has no generic
  string-attribute map of its own) under exactly those five keys. **`dynamics_hash` stays the
  configuration hash and no segment opens per command** -- the lead's explicit decision: folding
  a commanded value into the hash would make two runs with an identical configuration but
  different command streams disagree on it, which is backwards. Instead: "equal `dynamics_hash`
  means equal configuration; equal configuration plus equal command events means equal
  dynamics." See `drm::events::port_command_event` and `gmat_sys::model::GmatModel::
  step_with_ports`'s own doc comment for the mechanism (`av_dynamics::AppliedCommand` surfaced
  as a third, typed return value out of `step_with_ports`, threaded through
  `schedule::HeteroScheduler::applied_commands` and `crate::ports::AppliedPortCommand` -- never a
  side channel).

**Not emitted, honestly.** `EVENT_KIND_MODE_CHANGE` ("instance step-rate changes"): an
instance's effective step rate is computed once, at load, and used unchanged for every
boundary-split segment of its run; no `Fault.target` vocabulary names a step-rate field, and
nothing else in this crate changes an instance's step rate mid-run -- this never happens at
runtime today. See `drm::events`'s own module doc comment for the full reasoning.

**Load-time validation sees the same events.** `events::declared_events` builds the *declared*
shape of every event the real run will emit (same `id`/`name`/`kind`/`entity_id`, `tai_ns`
approximated from the declared `Scenario.start_tai_ns`/`end_tai_ns` -- immaterial, since
`crate::expr::typecheck::check` only ever checks an event's `name`/`kind` exist, never its
`tai_ns`) purely from `scenario`/`cfg.sos.instances`, before any instance has run -- so an
`event.*`-referencing `Objective`/`MeasureOfEffectiveness` validates at load exactly like any
other reference does, and (M9.3) can now be declared on `DesignReferenceMission.objectives`/
`.measures` directly: `tests/expr_common/mod.rs`'s `fault_split_accel_case` moved its three
`event.*`-referencing expressions onto the DRM itself once this landed (`goldens/
expr_fault_split_accel.json` was regenerated accordingly -- its own `"reason"` field records
why); they are no longer evaluated only via a test-built `ExprRunProducts` carrying a synthetic,
caller-supplied `Event`.

## Outputs (`output.<instance>.<name>@time`, question 95, M9.3 + M10.2)

Two kinds of `output.*`, kept deliberately distinguishable (never merged into one mechanism):

- **`speed` -- always derived, never a model output.** `execute()` attaches one derived output
  producer for every instance whose trajectory has velocity components:
  `output.<instance>.speed@time` -- `|velocity|` (m/s), computed directly from the real,
  already-propagated `Trajectory` (`crate::expr::speed_output`), GMAT-bound or native, whichever
  the run actually used. This never reads `av_dynamics::StepResult.outputs` at all -- it is
  purely a function of the trajectory's own recorded samples. A consumer can tell it apart from
  a real model output by name (it is always `"speed"`) and by construction: it is the one output
  name every instance with a 3-component velocity gets automatically, declared nowhere in
  `SystemDefinition.parameters`.
- **Named `GmatModel` outputs -- real, declared, and finite (M10.2; read from GMAT itself as of
  M11.1, question 99).** `gmat_sys::model::GmatModel::step` (`crates/gmat-sys/src/model.rs`) is
  overridden to populate `av_dynamics::StepResult.outputs` with a small, fixed set of named
  values: `gmat_sys::model::OUTPUT_RMAG` ("rmag", SI metres) -- position magnitude from the
  model's central body -- and, added at M11.1, `gmat_sys::model::OUTPUT_CD` ("cd",
  dimensionless) -- the spacecraft's own drag coefficient. **Both are read through
  `GmatBase::GetRealParameter`, not computed in Rust.** Through M10.2, `rmag` was Rust's own
  `sqrt(x^2+y^2+z^2)` on the propagated state, because `gmat-sys`'s FFI shim
  (`crates/gmat-sys/shim/gmatffi.{h,cpp}`, not owned by that task) exposed no
  `GetRealParameter`-style getter and `DerivativeModel` retained no handle to the bound
  `Spacecraft` at all -- escalated rather than worked around. Question 99/M11.1 closed that gap
  in `gmat-sys` (owned by that task): a new shim function `gmatffi_get_real_parameter` wraps
  `GmatBase::GetRealParameter` in the same never-throws-across-the-boundary `guarded()` pattern
  every other shim function uses (an unknown parameter name is a typed `GmatError`, not a
  garbage or silent-zero value), `DerivativeModel` now keeps the bound spacecraft's handle, and
  `GmatModel::step` writes the propagated state back into that spacecraft
  (`sync_spacecraft_cartesian_km`, through the *existing* `gmatffi_set_field_real` -- necessary
  because this crate's own integrator never calls GMAT's own
  `PropagationStateManager::MapVectorToObjects`) before reading `"RMAG"`/`"Cd"` back. `Cd` was
  added specifically because it does not depend on the propagated state at all and this crate
  never stores it anywhere in Rust -- reading it back correctly proves the call reaches GMAT's
  own object, not merely that `rmag`'s arithmetic still agrees with itself. See
  `crates/gmat-sys/README.md`'s "Named `step` outputs" section for the full mechanism and the
  re-measured agreement (unchanged at 4 µm -- see below).
  - **Carried through `HeteroKernel`, on both the plain and covariance paths (covariance as of
    M11.2, question 101).** `crate::schedule::Scheduler`/`HeteroScheduler` accumulate every
    native step's non-empty `StepResult.outputs` per system (`Scheduler::outputs`/
    `HeteroScheduler::outputs`, epochs = native step times, not resampled onto the output-tick
    grid), and `crate::kernel::Kernel::outputs`/`HeteroKernel::outputs` expose the accumulated
    series after a `run`/`run_with_covariance` call. Through M11.1 the covariance path never
    carried outputs at all: `av_dynamics::StmStepResult` had no `outputs` field, and
    `av_dynamics::StmAugmented::step` (what the covariance path actually steps) inherited the
    trait's default -- a continuous augmented-state integration over `derivatives` that never
    calls a wrapped model's own `step_with_stm` in the first place, so `outputs` was dead code
    there regardless of `StmStepResult`'s shape. Question 101 ("product sets must not depend on
    the run mode") closed both: `StmStepResult` gained an `outputs` field
    (`crates/av-dynamics/src/lib.rs`), `gmat_sys::model::GmatModel::step_with_stm` was overridden
    to populate it exactly like `GmatModel::step` does (`crates/gmat-sys/src/model.rs`), and
    `StmAugmented::step` (`crates/av-dynamics/src/stm.rs`) now overrides the trait default to
    delegate to the wrapped model's own `step_with_stm` per native period, composing the returned
    local `Phi(t, t+dt)` with the accumulated `Phi(t0, t)` (`Phi(t0, t+dt) = Phi(t, t+dt) .
    Phi(t0, t)`, exact STM composition) -- see that module's own doc comment for the numerical
    discussion (a different but measured-equivalent integration scheme from the old continuous
    accumulation). `run_covariance_span`/`run_covariance_instance` in this crate now call
    `kernel.outputs` and thread a `NamedOutputSeries` through exactly like `run_one_span` already
    does for the plain path -- see `run_covariance_instance`'s own doc comment.
  - **Declared in `SystemDefinition.parameters` so it hashes.** `"output.<name>"` parameter
    entries (e.g. `output.rmag`, `unit: UNIT_METER`) declare which of a `GmatModel`'s fixed
    outputs an instance actually exposes -- the same `SystemDefinition.parameters` extension
    point the covariance-P0 seeding used before it earned a real field (no proto change is
    authorized). `declared_outputs` reads the real, unmodified `SystemDefinition` (so the
    declaration is part of what `hash::verify_system_hash` checks) -- see
    `OUTPUT_PARAMETER_PREFIX`'s own doc comment in `crate::drm::executor`. **M10.3:**
    `binding::classify_binding`'s `parse_gmat_spec` now recognizes and skips `"output.<name>"`
    directly (it names no `GmatSystemSpec` field), so `crate::drm::executor` hands it the real
    `sys` unconditionally -- through M10.2 this allowlist refused any name outside its own
    `"force_model."`/`"spacecraft."` vocabulary (`binding.rs` was not owned by that task), so
    `crate::drm::executor::strip_output_parameters` had to hand it a filtered copy with
    `"output.*"` removed first; `binding.rs` is owned by this task, so the fix landed at the
    actual source instead, and `strip_output_parameters` is gone.
  - **The erasure hook M10.2 needed and escalated instead of editing -- fixed at the source in
    M10.3.** `av_dynamics::erase::ErasedModel` (`crates/av-dynamics/src/erase.rs`) did not
    delegate `step`/`step_with_stm` to the wrapped model through M10.2 (reasoned at the time as
    "erasure changes nothing about the physics, only the error type"), so a `GmatModel` boxed
    through `av_dynamics::erase_with_id` would silently and permanently lose its `step`
    override -- `outputs` would just always be empty, with no error to notice it by. M10.2
    could not fix this (only `src/lib.rs` was owned in that crate) or `binding::AnyModel`'s own
    parallel gap (`AnyModel::step` also inherited the trait default instead of delegating to
    whichever variant it wrapped), so it built `crate::drm::executor::StepDelegating`, a local
    `BoxedModel` wrapper used only by the plain (non-covariance) path, as an in-ownership
    workaround. **M10.3 owns both `av-dynamics/src/erase.rs` and `av-kernel/src/drm/binding.rs`**,
    so both gaps are fixed at their actual source: `ErasedModel::step`/`step_with_stm` now
    delegate to the wrapped model's own methods (`crates/av-dynamics/src/erase.rs`'s own module
    doc comment), and `AnyModel::step`/`step_with_stm` now delegate per-variant
    (`binding.rs`'s own `impl DynamicsModel for AnyModel`). `StepDelegating` is deleted --
    `crate::registry::ModelHandle::into_boxed`/`into_boxed_stm` (`av_dynamics::erase_with_id`
    directly) now carry the fix for every path, not just the plain one.

Verified end to end (not just at the model level) by `tests/drm_executor.rs
::drm_rmag_output_matches_a_genuine_gmat_reportfile`: a fresh `SystemDefinition` (the golden
bundle's own `leo_sys`, plus declared `output.rmag` and, as of M11.1, `output.cd` parameters),
run through `execute()` over the golden's full one-day arc, `output.leo_rmag.rmag@end`/
`output.leo_rmag.cd@end` read via two `MeasureOfEffectiveness` entries. `rmag` is checked against
GMAT's own `ReportFile` value for the identical arc -- **measured agreement: 4 micrometres**
(`6870294.675577` m from this run vs. `6870294.675573` m from GMAT's `ReportFile`), asserted at a
0.1 m bound (documented in the test as "same order as the base golden's own 0.05 m position
tolerance, loosened for headroom") -- **unchanged from M10.2's Rust-derived value to the last
displayed digit**, because the write-back-then-read round trip through GMAT's own Cartesian
fields is exact for this arc (see `crates/gmat-sys/README.md` for why that is expected, not
coincidental). `cd` is checked against the golden bundle's own declared `spacecraft.Cd = 2.2`
(`drms/leo_1day_golden.system.yaml`) to floating-point noise (`< 1e-9`) -- a value this crate
never computes or stores in Rust, so only a genuine `GetRealParameter("Cd")` call against the
real bound `Spacecraft` could reproduce it. `output_speed_resolves_against_a_real_gmat_bound
_instance` (above) remains the `speed` counterpart proof, unchanged by M10.2/M11.1.

## `RunProducts` on the wire: `to_proto`, scores, dropped count, frames (question 121/122, M17.2)

Through M16.3, the CDM had no message for a whole run, so `crates/av-run` framed
`drm::executor::RunProducts` as an ad hoc, explicitly length-prefixed concatenation of its
`Trajectory`/`Event`/`Provenance` fields' own binary protobuf encodings (`b"AVRUN1"`). **The
lead has since added a real message** (`docs/open-questions.md` question 121):
`proto/altavista/v1/run.proto` declares `RunProducts { run_id, trajectories (map, keyed by
`SystemInstance.id`), events, scores (map of `ScoreResult`), provenance,
dropped_in_flight_messages, frames }` and `ScoreResult { name, value, unit, optional bool
passed }`. The `AVRUN1` framing is deleted on both sides (`crates/av-run/src/main.rs`'s old
`encode_run_bundle`, `altavista/cdm.py`'s old `parse_run_wire`/`RunBundle`), not kept as a
fallback.

`drm::executor::RunProducts` gained two new fields to carry this: `dropped_in_flight_messages:
u64` (the same count `build_run_provenance` already recorded as a string in
`provenance.attributes` -- see "Ports and the router" above -- now also a first-class field)
and `frames: Vec<av_cdm::pb::FrameDefinition>` (below). `RunProducts::to_proto(&self) ->
av_cdm::pb::RunProducts` converts: `trajectories`/`events`/`provenance`/`frames` clone straight
across (already the real, unmodified `av_cdm::pb` types); `scores` becomes one `ScoreResult`
per entry, under the identical map key.

**`passed: Option<bool>` stays distinguishable through a real byte round trip.** `ScoreResult.
passed` is `optional bool` in `run.proto`, not a plain `bool` -- `prost`'s codegen for that is
`Option<bool>`, so an `Objective`'s `Some(false)` (failed, not absent) and a
`MeasureOfEffectiveness`'s `None` (no pass/fail concept, ADR-005 sec 6) never collapse onto the
same wire value the way a plain `bool` would force them to. `drm::executor`'s own
`to_proto_tests::passed_none_and_some_false_stay_distinguishable_through_a_real_byte_round_trip`
proves this past `encode_to_vec`/`decode`, not just the in-memory `pb::RunProducts` value --
see that test's own doc comment for the specific wrong implementation
(`Some(score.passed.unwrap_or(false))`) it would fail against, which an in-memory-only check
would not catch.

### `RunProducts.frames`: the DRM's `Scenario.frames` plus the registry defaults actually used

`Scenario.frames` ("frames this scenario declares beyond the registry defaults",
`proto/altavista/v1/system.proto`) is still refused if non-empty by the YAML loader (`schema::
RawScenario::refuse_if_nonempty` -- see "Known limitations / approximations in the DRM
executor" below: the frame registry is Planned/partial and this task does not model it). So in
practice today `Scenario.frames` is always empty, and `RunProducts.frames` is built entirely
from `registry_default_frame` (`src/drm/executor.rs`): the one case this crate can honestly
derive a `FrameDefinition` for a `Trajectory.frame_id` without a live frame registry. A
GMAT-bound instance's `Trajectory.frame_id` is always its own `spacecraft.CoordinateSystem`
name (`binding::ModelInfo.frame_id`), and every body-axes `CoordinateSystem` either this crate
or the Python side's `altavista.frames.FrameRegistry._register_body_axes` constructs is named
`f"{body}{axes_gmat_name}"` (e.g. `"EarthMJ2000Eq"`) for exactly the four body-axes kinds this
crate's `AxesKind` declares GMAT realizations for (ICRF/MJ2000Eq/MJ2000Ec/BodyFixed) --
`registry_default_frame` is that naming convention's reverse: strip a known axes suffix and, if
a non-empty body prefix remains, return the matching `FrameDefinition { body, axes }`.

`collect_frames` combines `scenario.frames` (an explicit declaration always wins over a derived
default for the same id) with `registry_default_frame` applied to every distinct
`Trajectory.frame_id` the run's final `trajectories` actually reference, sorted by id (no
`HashMap`/insertion-order dependence -- a proto `repeated` field has no map to lean on, so this
stands in for the `BTreeMap` iteration rule the rest of this crate follows). A `frame_id`
neither source can honestly cover (a native/`ConstantAccel` instance's own opaque `frame_id`
parameter, e.g. this crate's own test fixtures' `"test.frame"`; an `ObjectReferenced`
RIC/VNB/VVLH `CoordinateSystem` GMAT names some other way) is left out entirely -- never
guessed, the same "never silently approximate an axes kind" rule `altavista/cdm.py::
frame_definition_for` already follows on the Python side of this same boundary.

**Genuinely populated, not an empty list satisfying a weak test.** `tests/drm_executor.rs::
drm_matches_the_golden_arc` and `tests/drm_maneuver.rs::drm_matches_the_maneuver_golden_vnb_burn`
both assert `products.frames == [FrameDefinition { id: "EarthMJ2000Eq", body: "Earth", axes:
AXES_KIND_MJ2000_EQ, .. }]` against a **real** GMAT-propagated run (the golden `leo_sys`
declares `spacecraft.CoordinateSystem = "EarthMJ2000Eq"`) -- not a synthetic fixture. The
maneuver golden additionally proves this survives a fault/maneuver-split run (two dynamics
segments) unchanged, and that the burn's own `Event.frame_id` (`"AXES_KIND_VNB"`, a different
concept -- the frame a delta-v is declared in, not a trajectory's own frame) is never confused
with the trajectory's `frame_id`. `crates/av-kernel/src/drm/executor.rs`'s own
`frame_registry_tests` module additionally unit-tests `registry_default_frame`/`collect_frames`
in isolation (all four recognized suffixes, an opaque native id returning `None`, a bare suffix
with no body prefix returning `None`, a declared `Scenario.frames` entry winning over the
derived default, deduplication and sorting).

**Where this is going: M17.1 (question 122).** `altavista/server.py`'s `POST /api/cdm/run`
parses and preserves `RunProducts.frames` on the wire (M17.2, this task) but does not yet build
anything from it -- threading it into the viewer's own scene through
`altavista.frames.FrameRegistry` so the viewer can offer ICRF (question 5's demo requirement) is
a separate, later task. Nothing here silently drops the field; it simply is not this task's job
to surface in the viewer yet.

## Determinism (ADR-002 / ADR-004)

- Simulated time is TAI nanoseconds, `i64` (`clock::Clock`) -- never a float.
- Every iteration over systems is by system id, `BTreeMap` (`schedule::Scheduler`,
  `kernel::Kernel`) -- never `HashMap`.
- No RNG anywhere in this crate.
- No wall-clock read that affects a result: the clock only advances by an explicit `tick()`.

## Known limitations / approximations, stated plainly

- One `Scheduler<M>` cannot mix `DynamicsModel` types (see "Scope note" above).
- Controls are not routed into the scheduler at all (empty slice on every step).
- `Trajectory.provenance`/`config_hash` are left unpopulated.
- `entity_id` is currently just the system id, not a resolved catalog entity.
- `Kernel::run` requires the run horizon to be an exact multiple of the output period (a
  design choice, not an oversight: it keeps "the last sample lands exactly on `end_tai_ns`"
  an invariant rather than a best-effort).
- The acceptance test takes ~24 s (debug build) because it steps GMAT roughly 864,000 times
  at 10 Hz over a full day; this is the literal 10 Hz output-sampling requirement, not a
  reduced-scope stand-in for it.
- `run_with_covariance` requires a covariance-requesting system's `period_ns` to equal the
  kernel's `output_period_ns` exactly (see "Covariance" above) -- a system that legitimately
  needs both a faster physical step rate and covariance output would need its own
  covariance-only companion system at the output rate; nothing in this crate builds that.
- `run_with_covariance`'s SPD hygiene check (`av_cdm::covariance::check_spd_row_major`) is a
  Cholesky attempt, the same bar spoore's own `GaussianState` constructor uses -- not a
  diagonal-positivity shortcut (that was the earlier M3.2 state; question 80 replaced it, see
  "Covariance hygiene" above). It is, however, a necessary condition on the covariance the
  kernel actually produced, not a guarantee that the *physics* behind it is well-conditioned:
  a genuinely singular or near-singular P0/Phi combination that still happens to round-trip
  through symmetrization into something Cholesky accepts will pass, by design (this check
  cannot and does not second-guess a caller's declared P0).
- The opt-in nearest-SPD projection is a plain `bool` parameter on `run_with_covariance`.
  **As of M6.1, `drm::executor::execute` is exactly the crate this note used to say did not
  exist yet**: it constructs a `DesignReferenceMission`, reads `options.nearest_spd_projection`
  off it, and passes that value straight through to `run_with_covariance` -- see "The DRM
  executor" above. This method's own signature is otherwise unchanged.
- Question 82 (`RelativisticCorrection` withholds the STM capability unless
  `DrmOptions.accept_missing_stm_terms` is acknowledged) is enforced one layer below this
  crate, at `gmat-sys::model::GmatModel::new`/`stm_capable()` -- a model whose capability was
  withheld there never declares `stm_capable() == true`, so `av_dynamics::StmAugmented::new`
  (which panics on a non-STM-capable model) already refuses it before it ever reaches
  `Kernel::register_system`/`run_with_covariance`. This crate does not re-check the condition;
  see `crates/gmat-sys/README.md`'s "Question 82" section for the mechanism and its own test.
  `drm::binding::classify_binding` adds a *second*, earlier check at the DRM layer (before any
  GMAT call), refusing the same combination from declared metadata alone -- see "The DRM
  executor" above.

## Known limitations / approximations in the DRM executor (`drm`, M6.1), stated plainly

- `SystemDefinition.dynamics_model`'s prefix (`"gmat."`/`"remote."` vs. anything else,
  `registry::kind_for`) is still the binding dispatch key -- `registry::ModelRegistry` is the
  sole *constructor* as of M10.3 (question 98), but it still classifies by that same three-way
  prefix convention, not a real catalog keyed by more than "is this gmat/remote/anything else."
  A richer catalog (ids -> declared capabilities/metadata, not just a dispatch prefix) is future
  work; this is a stated stand-in, not a hidden one.
- `Variant` (`SystemDefinition`) and `FrameDefinition` (`Scenario`) are not modeled by the YAML
  loader -- a non-empty one is refused (`DrmError::UnsupportedField`), not silently dropped, but
  this does mean a DRM that actually needs named parameter-override variants or custom
  *declared* frames cannot be expressed yet. **`Port`/`PortTiming` are modeled as of M13.1**
  (question 108, see "Ports and the router" above) -- `Variant` alone remains opaque-refused.
  `Scenario.events` is modeled as of M10.1 (question 97) -- but only `kind = "maneuver"`; any
  other kind (`"mode"`, `"contact"`, `"custom"`) is still refused
  (`DrmError::UnsupportedScenarioEventKind`), not silently accepted. **M17.2 (question 121/122)
  partially works around the `Scenario.frames` gap, not the loader change itself**:
  `RunProducts.frames` is still real and genuinely populated for every DRM this crate's own
  tests exercise, but entirely from `registry_default_frame`'s derived body-axes defaults (see
  "RunProducts on the wire" above), since `scenario.frames` itself is always empty coming out
  of the loader -- a DRM that needs a *non*-body-axes frame (RIC/VNB/VVLH/ENU/NED/etc.) declared
  on `RunProducts.frames` still cannot get one until the loader actually models
  `Scenario.frames`.
- **Resolved in M14.1** (previously listed here as "M13.1's router is validated by `execute()`
  but not yet driven by it"): `execute()` now calls `HeteroKernel::run_with_ports` (via
  `run_shared_group`), and the native `ConstantAccelModel` placeholder now optionally overrides
  `step_with_ports` (`port.emit`/`port.consume`, `binding::ConstantAccelModel`'s own doc
  comment) -- a real DRM run does exchange port messages now, proven end to end by `tests/drm_
  shared_run.rs`. See "One shared kernel run" above.
- **Resolved in M14.1** (previously listed here as "a latent gap in `av_dynamics::erase::
  ErasedModel`, not owned by this task"): that gap turned out to already be closed --
  `ErasedModel::step_with_ports` (`crates/av-dynamics/src/erase.rs`) already delegates to the
  wrapped model's own override, not the trait's default (confirmed by `crates/av-dynamics/tests`
  `erased_model_delegates_a_wrapped_models_own_step_with_ports_override`, and now genuinely
  exercised rather than inert: `AnyModel::step_with_ports` needed its *own* explicit delegation
  to each variant, mirroring `AnyModel::step`/`step_with_stm` -- added this task, `binding.rs`
  -- since `AnyModel` sits one level further out than `ErasedModel`, and its own inherited
  trait default would otherwise have silently discarded `ConstantAccelModel`'s/`GmatModel`'s
  `step_with_ports` behaviour the moment either is wrapped in `AnyModel` and erased via
  `crate::registry::ModelRegistry::construct_native`/`construct_gmat` -- exactly the shared
  kernel run's own construction path.
- **Resolved in M9.1** (previously listed here as a gap, and already stale before that --
  M8.2 had added the `expr` evaluator but nothing wired it to the executor): `Objective`/
  `MeasureOfEffectiveness` are now evaluated by `execute()` itself, over the real run, into
  `RunProducts.scores` -- see ["Scoring"](#scoring-the-expression-language-and-runproducts-
  expr-adr-005-sec-6-m91) above.
- **Resolved in M9.3** (question 95, previously listed here as "`RunProducts.events` is always
  empty"): `execute()` now emits real `EVENT_KIND_LIFECYCLE`/`EVENT_KIND_FAULT` events, and
  `event.*`-referencing `Objective`/`MeasureOfEffectiveness` expressions can be declared on the
  DRM itself -- see ["Events"](#events-runproductsevents-drmevents-question-95-m93) and
  ["Outputs"](#outputs-outputinstancenametime-question-95-m93--m102) above.
  **Resolved in M10.1** (previously listed here as a gap): `EVENT_KIND_MANEUVER` is now emitted
  for every applied maneuver -- see ["Impulsive
  maneuvers"](#impulsive-maneuvers-scenarioevents-of-kind-maneuver-m101-question-97) above.
  `EVENT_KIND_MODE_CHANGE` is still never emitted -- a stated fact about what this crate has no
  honest way to produce today (no mechanism changes an instance's step rate mid-run), not a gap
  left half-done.
- Covariance and DYNAMICS faults are not supported together on the same instance
  (`DrmError::CovarianceWithFaultsNotSupported`) -- see "DrmOptions -> kernel knobs" above.
  **Covariance and maneuvers ARE supported together (M10.1)** -- see "Impulsive maneuvers"
  above for the "no execution error -> Phi/P unchanged" argument and its test.
- **Resolved in M11.4** (question 100, previously listed here as "burn execution error is out
  of scope"): the Gates model (`ScenarioEvent.execution_error`) is now implemented -- see the
  "Burn execution error: the Gates model" section above. One scope limit remains, stated rather
  than hidden: burn *duration* (a finite-duration, non-impulsive burn) is still not modeled --
  every maneuver in this crate is instantaneous, and question 100 did not ask for that to
  change.
- **Resolved in M7.2** (previously listed here as a gap): the initial covariance `P0` now has
  a dedicated proto field, `SystemInstance.initial_covariance` (question 89), and the former
  `covariance.p0_row_major` parameter-string convention is gone.
- **Partially resolved in M10.2, erasure fixed at the source in M10.3, covariance path fixed in
  M11.2** (question 95's second half, previously "`GmatModel` never populates
  `StepResult.outputs` and `HeteroKernel` never reads them"): see the "Outputs" section above.
  M10.2/M10.3 fixed erasure delegation; through M11.1 one scope limit remained, structural rather
  than an erasure gap: the covariance path (`run_covariance_instance`) never carried outputs --
  `StmStepResult` (what a `step_with_stm` call produces) had no `outputs` field to carry, and
  `av_dynamics::StmAugmented::step` (what the covariance path actually steps) never called
  `step_with_stm` at all, regardless of how faithfully erasure delegated `step`/`step_with_stm`
  itself. **Resolved in M11.2** (question 101, "product sets must not depend on the run mode"):
  see the "Outputs" section's "Carried through `HeteroKernel`" bullet above for the fix
  (`StmStepResult.outputs`, `GmatModel::step_with_stm`'s override, `StmAugmented::step`'s
  delegate-and-compose scheme) and `tests/drm_executor.rs::
  covariance_and_plain_paths_produce_the_same_named_output_set` for the test pinning it. One
  further limit is unchanged: a named output series accumulated across a fault/maneuver-split
  run (`run_one_span`'s `all_outputs` parameter, and `run_covariance_span`'s too) is a simple
  concatenation, not deduplicated at segment boundaries the way `Trajectory.samples` is -- a
  boundary epoch can appear twice in a series (harmless: linear interpolation over the series
  picks the first exact match, so this changes nothing observable, but it is a real, disclosed
  difference from how samples themselves are stitched).
- `Scenario.data_pack_hash` is carried through to `Provenance.data_pack_hash` verbatim but is
  never itself verified against anything (there is no data-pack registry or content-hash
  authority in this repository yet to check it against) -- declared and threaded through
  honestly, not independently validated.
- A GMAT-bound instance's object names (`Drm<instance>_<segment>...`) are unique within one
  `execute` call but not across separate `execute` calls in the same process, since GMAT's own
  configuration manager is process-global -- matching the existing convention every
  `crates/gmat-sys/tests/*.rs` test already follows (each test's spacecraft has a distinct
  name). Several tests in `tests/drm_executor.rs` now call into GMAT through the DRM executor
  over the same golden `leo_sys` `SystemDefinition` (`drm_matches_the_golden_arc`,
  `drm_covariance_matches_the_golden_stm_and_propagated_cov`,
  `output_speed_resolves_against_a_real_gmat_bound_instance`, and the short-fixture load-time
  refusal tests) -- each uses its own distinct instance/DRM naming (`"leo"`, `"leo_cov"`,
  `"leo_out"`, `"leo_mvr"`, `"leo_cov_zero_burn"`, `"leo_cov_no_burn"`, ...) to stay
  collision-free, per this note.
- `RunConfig.run_id` is caller-supplied, not derived from anything inside this crate -- this
  crate never reads the wall clock (ADR-002/ADR-004). `Provenance.created_tai_ns` is left `0`
  for the same reason: no test here needs a real creation timestamp yet, and this module would
  rather leave the field honestly empty than reach for `SystemTime::now()` and violate the
  "no wall-clock read that affects a result" rule for a field nothing currently checks.
- **M15.3 (question 118), unaffected by M16.2's move to `FAULT_TARGET_KIND_HARDWARE`:** a
  power-cycle boundary on a container instance does not mark that
  instance's own next `TrajectorySegment` as "preceded by a state discontinuity" the way a
  maneuver boundary does for the model instance it targets -- `merge_adjacent_segments` may
  therefore merge the segment before a reset with the one after it (both share the identical
  `dynamics_hash`: `ContainerModel::describe()` never changes, reset or not). This is a stated,
  deliberate simplification: a container instance's `TrajectorySample.mean` is always empty
  (`state_dim() == 0`), so nothing about its own segment list changes shape either way, and the
  fault is fully recorded regardless (an `EVENT_KIND_FAULT` event, plus the reference process's
  own `named_outputs` visibly dropping back) -- unlike a maneuver's dv jump, a reset leaves no
  physical-state discontinuity for `Trajectory.segments` to exist to describe.
- **M15.3:** the Docker image-lifecycle path retries its own connect-then-`Bind` attempt for up
  to 30 s when `ContainerBinding.image` is set (`binding::materialize_container`) -- a freshly
  `docker run -d`-started container's TCP listen socket can accept a connection slightly before
  its own request-handling thread pool is actually servicing one, observed directly while
  building this task (a `Bind` sent in that window fails with a transport error even though a
  moment later it succeeds). The `container.address` path is unchanged and never retries: its
  own M13.2 contract is "already running," so a transport failure there is a real error, not a
  startup race.
