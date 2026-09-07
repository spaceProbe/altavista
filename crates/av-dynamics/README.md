# av-dynamics

The shared dynamics contract and integrator ([ADR-002](../../docs/adr/002-dynamics-contract.md)
"One dynamics contract, GMAT at three depths, goldens as the proof"). **No GMAT
dependency** -- this crate builds and is unit-tested in an environment that has never linked
GMAT; `crates/gmat-sys::model::GmatModel` is the GMAT-backed binding and lives in `gmat-sys`
instead, for exactly that reason.

## The contract

```
derivatives(state, t, controls)   -> state_dot
step(state, t, controls, dt)      -> (state, t + dt, outputs)
describe()                        -> ModelInfo
```

`DynamicsModel` is the trait every dynamics model implements. `derivatives` is the portable
form -- it is what lets another engine own the integrator instead of this one. `step` is what
the kernel calls, and **the trait's default implementation of `step` drives
`integrate::Dopri5` over `derivatives`**, so a model that supplies only `derivatives` gets a
correct, adaptive `step` for free; nothing requires overriding it. ADR-002's other two verbs,
`propagate` (design-time, returns a CDM `Trajectory`) and `solve` (a declared capability), are
not part of this trait -- `propagate` is built from many `step`s one layer up (see
`crates/av-kernel`), and `solve` is per-model; both are out of scope for this crate's
skeleton (M2.1).

```rust
pub trait DynamicsModel {
    type Error: std::fmt::Debug + std::fmt::Display;

    fn state_dim(&self) -> usize;

    fn derivatives(
        &self, state: &[f64], t_tai_ns: i64, controls: &[f64], state_dot: &mut [f64],
    ) -> Result<(), Self::Error>;

    fn describe(&self) -> av_cdm::pb::ModelInfo;

    fn integrator(&self) -> integrate::Dopri5 { integrate::Dopri5::default() }

    fn step(
        &self, state: &[f64], t_tai_ns: i64, controls: &[f64], dt_ns: i64,
    ) -> Result<StepResult, Self::Error> { /* integrates `derivatives` with `integrator()` */ }

    // Ports (docs/open-questions.md question 108, M13.1) -- see below.
    fn step_with_ports(
        &self, state: &[f64], t_tai_ns: i64, controls: &[f64], dt_ns: i64, inbox: &Inbox,
    ) -> Result<(StepResult, Outbox), Self::Error> { /* ignores `inbox`, calls `step`, empty `Outbox` */ }

    // STM / covariance capability (ADR-002 second amendment, M3.2) -- see below.
    fn stm_capable(&self) -> bool { false }
    fn stm_derivatives(
        &self, augmented_state: &[f64], t_tai_ns: i64, controls: &[f64], augmented_state_dot: &mut [f64],
    ) -> Result<(), Self::Error> { /* unimplemented!() by default */ }
    fn step_with_stm(
        &self, state: &[f64], t_tai_ns: i64, controls: &[f64], dt_ns: i64,
    ) -> Result<StmStepResult, Self::Error> { /* integrates `stm_derivatives` with `integrator()` */ }
}
```

`t_tai_ns` is always the *absolute* epoch, TAI nanoseconds -- never a step-relative offset --
even inside `step`'s default implementation, whose own substep arithmetic is `f64` seconds
internally (`Dopri5` operates on plain `f64` time). Each substep's relative time is converted
back to an absolute TAI instant (`t_tai_ns + round(t_rel_s * 1e9)`) before calling
`derivatives`, so a model's `derivatives` never observes anything but an honest TAI
nanosecond count.

## Ports (`docs/open-questions.md` question 108, M13.1)

```rust
pub type PortMessage = av_cdm::pb::PortMessage;   // proto/altavista/v1/lockstep.proto, verbatim

pub struct Inbox { /* every PortMessage av_kernel::router::Router delivered for this step */ }
pub struct Outbox { /* every PortMessage this step_with_ports call emits */ }

pub fn encode_signal(value: f64) -> Vec<u8>;      // little-endian f64 -- PortMessage's own doc comment
pub fn decode_signal(payload: &[u8]) -> Option<f64>;
```

`PortMessage` is a type alias for the real wire type generated from `lockstep.proto`, not a
parallel struct -- `av-kernel`'s router, and eventually the lockstep client (M13.2), all speak
the identical type from load to run to (later) the network. `Inbox`/`Outbox` live here (not in
`av-kernel`, where the router and the `SosConfiguration.connections` wiring actually live)
because `DynamicsModel::step_with_ports`'s own signature needs them, and this crate sits below
`av-kernel` in the dependency graph (the reverse dependency would be circular); `av-kernel::
ports` re-exports them and adds the routing-specific types (`QueuedMessage`, `sorted_inbox`)
that build on top.

**`step_with_ports`'s default implementation ignores `inbox` entirely, calls `step` exactly as
before, and returns an empty `Outbox`.** This is the "MODEL bindings get a default no-port
implementation so nothing existing changes" the task requires: every `DynamicsModel` in this
workspace today (`gmat_sys::model::GmatModel`, `av_kernel::drm::binding::AnyModel`, every test
model in this crate and in `av-kernel`) overrides neither this method nor `step`'s own
signature, so nothing about any of them changes by this method's existence.
`av_kernel::schedule::HeteroScheduler::advance_to_with_ports`/`crate::kernel::HeteroKernel::
run_with_ports` call this method (never `step` directly) when routing ports, so a ported and an
unported model run through the identical call site.

**Why a new method, rather than adding `inbox`/an `Outbox` return value to `step` itself?**
`step`'s signature is a public contract every model in this workspace already implements,
including `gmat_sys::model::GmatModel` -- a crate outside this task's ownership. Changing
`step`'s own signature would force every implementation to change in lockstep with this task;
a new default-provided method gives the kernel exactly the "pass the inbox, collect the outbox"
hook without that ripple.

`encode_signal`/`decode_signal`/`Outbox::push_signal` are the one encode/decode site for a
SIGNAL port's payload (a little-endian IEEE-754 double, per `lockstep.proto`'s own
`PortMessage` doc comment), so every SIGNAL emitter and reader in the workspace agrees on the
same bytes.

## The integrator: `integrate::Dopri5`

A Dormand-Prince 5(4) adaptive-step integrator, moved here **unchanged** from
`crates/gmat-sys/src/integrate.rs` (M2.1) -- not one line of the algorithm differs; see
`gmat-sys`'s own README for the bit-identical verification against the golden arc. ADR-002:
"the integrator is ours" -- one family, shared by every domain and binding, not a detail that
belongs to any one binding.

`Dopri5::default()` is `rtol = atol = 1e-12`, `initial_step = 30.0` s, `max_step = 600.0` s
-- the settings the P0 spike and this crate's own tests were measured at.

## STM / covariance: a declared capability (ADR-002 second amendment, M3.2)

A model reports whether it can also propagate its own state transition matrix (STM)
alongside the state via `stm_capable()` -- default `false`. This is the whole declaration: no
separate trait, no generic bound gymnastics anywhere a `DynamicsModel` is used generically.
A model that answers `true` must also override `stm_derivatives`, which fills `state_dot` for
the state **augmented** with its own STM (`[state; vec(Phi)]`, length `state_dim() +
state_dim()^2`, `Phi` row-major -- `state_dim() + row*state_dim() + col` is `d(Phi)/dt`
element `(row, col)`, the layout the ADR-002 second amendment measured against GMAT). The
default `stm_derivatives` is `unimplemented!()`: reaching it means a caller invoked it without
checking `stm_capable()` first, a caller bug, not a condition well-behaved code can trigger.

`step_with_stm` (default-provided, like `step`) seeds `Phi(t0, t0) = I` and integrates the
augmented state with `integrator()`, returning `StmStepResult { state, phi, t_tai_ns, outputs }`
-- `phi` is `Phi` over exactly the interval just integrated. `outputs` (question 101, M11.2) is
the `StmStepResult` sibling of `StepResult::outputs`, empty by default unless a model overrides
`step_with_stm` to populate it (e.g. `gmat_sys::model::GmatModel::step_with_stm`, mirroring
`GmatModel::step`'s own `OUTPUT_RMAG`/`OUTPUT_CD`) -- added because a covariance run had fewer
named products than a plain run through M11.1 (`docs/open-questions.md` question 101: "product
sets must not depend on the run mode").

**`StmAugmented<M>`** (`crate::stm`) wraps any `M: DynamicsModel` with `stm_capable() ==
true` and presents the augmented vector as its own `state_dim()`/`derivatives` (which calls
the wrapped model's `stm_derivatives`) -- so anything that drives it purely through
`derivatives` (a caller integrating it directly, rather than through `step`) gets the same
dimension-agnostic behaviour as the plain model. `StmAugmented::seed(x0)` builds the
`[x0; vec(I)]` initial condition. `av-kernel`'s `Kernel<StmAugmented<M>>::run_with_covariance`
is where this is actually driven end-to-end and `TrajectorySample.cov` gets filled in -- see
that crate's README.

**`StmAugmented::step` overrides the trait default, as of question 101 (M11.2).** Through M11.1
`step` was never overridden, so the kernel's covariance path (which steps a `StmAugmented<M>`
one native period at a time) got the trait's default: one continuous `Dopri5` integration of
the whole `[state; vec(Phi)]` vector via `derivatives` -- which never calls the wrapped model's
own `step_with_stm` at all, so any `outputs` a model's `step_with_stm` override populated was
dead code as far as the covariance path was concerned. `step` now delegates to
`self.0.step_with_stm` for exactly this native period, seeded from the physical state alone,
and composes the returned **local** STM `Phi(t, t+dt)` with the accumulated `Phi(t0, t)`
already carried in the incoming augmented state: `Phi(t0, t+dt) = Phi(t, t+dt) . Phi(t0, t)`
-- exact in continuous time (STM composition), not an approximation. This is what makes a
wrapped model's own `step_with_stm` override actually reachable from the covariance path, and
what carries `outputs` through. Numerically this is a different (reseed-per-period,
multiply-to-accumulate) scheme than the old continuous accumulation, so `Dopri5`'s own adaptive
step-size choices can differ slightly between the two -- measured, not assumed, against
`crates/av-kernel/tests/golden_acceptance.rs::kernel_covariance_matches_the_golden_stm_and_
propagated_cov`, which still passes at its existing tolerance (see this task's own report for
the measured numbers).

**`propagate_covariance(phi, p0, n) -> (Vec<f64>, f64)`** computes `P(t) = Phi P0 Phi^T`,
`n x n` row-major, and **explicitly symmetrizes** the result (`0.5 * (P + P^T)`), returning
the maximum pre-symmetrization asymmetry alongside it -- a caller reports the correction
(`av-kernel` logs it) rather than letting an asymmetric matrix through silently. This function
is the only place covariance is ever computed in this crate; nothing calls it implicitly.

**Never automatic.** The kernel calls `run_with_covariance` (as opposed to plain `run`) only
when a caller explicitly asks, and only for the systems it names a P0 for (question 11:
covariance is always optional and explicitly requested, never a silent default, never a
profile-driven behaviour change). A model with `stm_capable() == false` simply cannot be
wrapped in `StmAugmented` (`StmAugmented::new` panics on construction if the wrapped model
doesn't declare the capability) -- covariance is unreachable for it by construction, not by a
runtime check that could be bypassed.

Verified independently of GMAT: `crate::stm`'s tests use a planar rotation (`x' = A x`, `A =
[[0, w], [-w, 0]]`, autonomous) whose STM has the closed form `Phi(dt) = [[cos(w dt), sin(w
dt)], [-sin(w dt), cos(w dt)]]`, `det(Phi) = 1` exactly -- an independently-checkable answer
for `stm_derivatives`/`step_with_stm`/`StmAugmented`/`propagate_covariance` together with no
GMAT dependency. See `crates/gmat-sys/README.md` and `crates/av-kernel/README.md` for the
GMAT-backed measurements (STM element ordering verified against GMAT, `Phi(t0,t0)=I`,
`det(Phi)` near 1, covariance vs a golden).

## No trait default returns a valid *empty* result (M14.3, question 112)

`docs/open-questions.md` question 112 names a recurring defect: a wrapper that erases/boxes a
`DynamicsModel` (`erase::ErasedModel`, `av_kernel::drm::binding::AnyModel`) forgot to delegate a
method to the *wrapped* model, so the wrapper silently fell back to *this trait's own* default
instead -- three times (`step` in M10.3, `step_with_stm` in M11.2, `step_with_ports` in
M13.1/M13.2), each invisible because the fallback default still returned something plausible.
M14.3 re-audited every default against "does this return a valid but *empty*/no-op result a
caller could mistake for the real thing" -- every method on this trait is now one of exactly
three shapes:

- **Required** (`state_dim`, `derivatives`, `describe`): no default exists, so there is nothing
  to silently fall back to.
- **A default that is genuinely, functionally complete for a model that opts out of the
  capability** (`integrator`, `step`, `step_with_stm`, `step_with_ports`): none of these return
  an empty/no-op placeholder -- `integrator` returns real, documented tolerances; `step`/
  `step_with_stm` really do drive `integrate::Dopri5` over `derivatives`/`stm_derivatives` and
  produce a physically correct answer; `step_with_ports` really does call `step` and returns a
  correct, empty `Outbox` for a model that does not participate in ports -- required because
  `av_kernel::schedule::{Scheduler, HeteroScheduler}` call this method unconditionally for every
  registered system, so this default cannot become `Err` (a `NotSupported` default here would
  break every model in this workspace that never overrides it) and cannot become a required
  method either (it would force every non-ported model, in every crate that implements this
  trait, to hand-write the identical three-line override for no safety benefit). **The actual
  defect was never in these defaults -- it was in the wrapper types not delegating**, which
  M13.1/M13.2 and M14.3 close directly by making `ErasedModel`/`AnyModel` delegate every method
  explicitly, never relying on this trait's own default for a wrapper's own dispatch.
- **A default that panics on a declared-capability precondition violation** (`stm_derivatives`):
  not "a valid empty result" at all -- it never returns -- so it already fails loudly, just not
  as a typed `Result`. It cannot become `Err(ModelError::NotSupported)` at the trait level
  because `Self::Error` is this trait's own unconstrained associated type (`std::convert::
  Infallible` foremost among the implementors that cannot construct any such conversion at
  all -- see `error`'s module doc comment). Where the concrete error type *is* fixed -- at
  exactly the trait-object boundary `ErasedModel`/`AnyModel` provide -- the panic is upgraded to
  a typed, catchable `ModelError::CapabilityMissing`/`AnyModelError::CapabilityMissing` instead;
  the generic default here stays a panic for direct (non-erased) callers, where "check
  `stm_capable()` first" remains an ordinary, review-enforceable precondition.

No new `ModelError` variant was needed for this task: `CapabilityMissing` already existed
(`stm_capable()`-declared-but-not-honoured was already a named case); nothing here required
inventing a `NotSupported` variant, since no method's default actually needed converting to one
once the above audit was done -- see `error.rs`'s own doc comment for why the trait's associated
`Error` type could not carry one uniformly in any case.

**The defect class is closed by delegation, not by trait-default surgery.** `erase::ErasedModel`
(this crate) and `av_kernel::drm::binding::AnyModel` now override *every* `DynamicsModel` method
explicitly. `erase.rs`'s `erased_model_reaches_inner_*` test suite (one test per method, against
an `AllOverridden` test model whose every override returns a marker value no trait default could
produce by coincidence) is the one-per-method proof that a future regression -- `ErasedModel`
silently losing one of its overrides and falling back to the trait's own default -- fails
exactly one named test: `erased_model_reaches_inner_state_dim`, `_derivatives`, `_describe`,
`_integrator`, `_stm_capable`, `_stm_derivatives`, `_step`, `_step_with_stm`,
`_step_with_ports`, plus `erased_model_stm_derivatives_returns_a_typed_capability_missing_error_
instead_of_panicking_when_stm_capable_is_false` for the `CapabilityMissing` upgrade specifically.
`av_kernel::drm::binding::AnyModel` has the identical per-method suite (`any_model_state_dim_
delegates_to_the_constant_accel_variant` and its eight siblings) -- see
`crates/av-kernel/README.md`.

## Trait objects: `erase::ErasedModel`/`erase_with_id` (ADR-005 sec 1)

`Box<dyn DynamicsModel<Error = ModelError>>` (`BoxedModel`) is what a caller holding several
different concrete `DynamicsModel` types at once (e.g. `av-kernel`'s `HeteroKernel`, one system
per Rust type) actually schedules. `ErasedModel<M>` wraps an `M: DynamicsModel` plus a
conversion from `M::Error` to `ModelError`, and `erase_with_id` is the common-case constructor
(`ErasedModel::new` plus `Box::new`) that also carries the erased model's own id for every
`ModelError` it can produce.

**`step`/`step_with_stm` delegate to the wrapped model's own implementation, as of M10.3.**
Every `DynamicsModel` method delegates -- `derivatives`/`stm_derivatives` mapping the error,
`state_dim`/`describe`/`integrator`/`stm_capable` passed straight through -- but through M10.2,
`step`/`step_with_stm` did not: both are default-implemented on the trait purely in terms of
`derivatives`/`stm_derivatives` and `self.integrator()`, so `ErasedModel` simply inherited that
default, reasoned at the time as "erasure changes nothing about the physics, only the error
type." That reasoning quietly assumed no wrapped model ever overrides `step`/`step_with_stm`
itself -- true until `gmat_sys::model::GmatModel::step` was overridden (M10.2,
`docs/open-questions.md` question 95's second half) to populate `StepResult.outputs`: boxing a
`GmatModel` through `erase_with_id` silently and *permanently* discarded that override forever,
with no error to notice it by (the inherited default still computed the identical physical
state via `derivatives`, so nothing *looked* broken). `av-kernel::drm::executor::
StepDelegating` was `av-kernel`'s in-ownership workaround for exactly this gap at the time
(this crate's `src/erase.rs` was not owned by that task). M10.3 owns `erase.rs` and fixes the
actual gap: `ErasedModel::step`/`step_with_stm` now call `self.inner.step`/`step_with_stm`
directly, so any wrapped model's own override survives erasure -- see `erase.rs`'s own module
doc comment, and `erased_model_delegates_a_wrapped_models_own_step_override` (below) for the
test that proves it, alongside `erased_models_default_step_still_drives_dopri5_over_
derivatives` proving this is a no-op for the (still-common) case of a model that overrides
neither. **M13.1/M13.2 added the identical fix for `step_with_ports`, and M14.3 (question 112)
makes it total: every `DynamicsModel` method is now delegated explicitly**, with a dedicated
test per method -- see the "No trait default returns a valid empty result" section above.

## `settings_hash`

```rust
pub fn settings_hash(settings: &BTreeMap<String, String>) -> String
```

SHA-256 (`sha2`, pure Rust, no bundled C crypto -- ADR-004's crypto rule) over a
canonically-ordered settings description, for `ModelInfo.settings_hash` ("SHA-256 of the
model's settings"). `BTreeMap` iteration is already key-sorted, so the digest is a pure
function of the settings themselves, independent of the order a caller happened to build the
map in (ADR-002 / ADR-004 determinism: no `HashMap` on an output path). This crate does not
decide *what* goes into the map -- a binding (e.g. `GmatModel`) is the one that knows what
its own settings actually are.

## Determinism (ADR-002 / ADR-004)

- No `HashMap` on any output path: `StepResult::outputs`, `StmStepResult::outputs`, and
  `settings_hash`'s input are all `BTreeMap`.
- No RNG anywhere in this crate.
- No wall-clock read that affects a result: every time value that matters is threaded through
  explicitly (`t_tai_ns`, `dt_ns`) rather than read from the system clock.
- Time is TAI nanoseconds, `i64`, at the trait boundary; `f64` only appears inside `step`'s
  own substep arithmetic, never as something a caller passes in or reads back as "the" time.

## Testing

```bash
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cargo test -p av-dynamics       # no GMAT required
cargo clippy -p av-dynamics --all-targets -- -D warnings
```

Unit tests cover: the default `step` against a constant-acceleration model with a closed-form
solution (checks both the integration result and that `derivatives` really does see absolute
TAI nanoseconds, not a step-relative offset); `settings_hash`'s stability and order
independence; `crate::stm`'s STM/covariance machinery against the closed-form rotation model
described above (`stm_derivatives` leaves the physical block identical to `derivatives`,
`step_with_stm` matches the closed-form `Phi`, `Phi(t0,t0)` is the exact identity,
`StmAugmented` refuses a non-STM-capable model, `propagate_covariance` reproduces `P0`
through an identity `Phi`, preserves trace under a rotation, and symmetrizes a manufactured
asymmetry while reporting it); `crate::erase`'s delegation, including (M10.3)
`erased_model_delegates_a_wrapped_models_own_step_override` -- a test model whose own `step`
populates `StepResult.outputs` in a way `derivatives` alone could not, proving the erased,
boxed model still sees it, not just the trait's default. Question 112 (M14.3) adds the full
per-method suite: `erased_model_reaches_inner_state_dim`, `_derivatives`, `_describe`,
`_integrator`, `_stm_capable`, `_stm_derivatives`, `_step`, `_step_with_stm`,
`_step_with_ports` (all against one `AllOverridden` model whose every override is a
distinguishable, non-default value), plus
`erased_model_stm_derivatives_returns_a_typed_capability_missing_error_instead_of_panicking_
when_stm_capable_is_false`. Question 101 (M11.2) adds
`stm_augmented_step_delegates_to_step_with_stm_and_carries_its_outputs` and
`stm_augmented_step_composes_the_local_stm_with_the_accumulated_one_over_two_native_periods`
(`crate::stm`) -- a test model whose `step_with_stm` override populates `StmStepResult.outputs`,
proving `StmAugmented::step` reaches it and that composing local STMs across two native periods
reproduces the same `Phi` a single call over the combined interval would. Question 108 (M13.1)
adds `encode_signal_and_decode_signal_round_trip` (bit-exact for `0.0`/`-0.0`/extreme
magnitudes, and rejects a non-8-byte payload), `the_default_step_with_ports_ignores_the_inbox_
and_matches_plain_step_exactly` (a non-empty `Inbox` on an unrelated port makes no difference to
the state produced, and the `Outbox` is empty -- "MODEL bindings see no behaviour change"), and
`inbox_for_port_filters_by_name_and_preserves_order`.

## Known limitations

- `DynamicsModel::step`'s default implementation only steps forward: `dt_ns <= 0` is not
  guarded against and silently returns the input state unchanged (`Dopri5::integrate`'s own
  loop condition, `while t < t1 - 1e-9`, simply never executes). Callers (the kernel's
  scheduler) are expected to only ever advance time forward; this crate does not defend
  against a caller that doesn't. `step_with_stm` shares this behaviour.
- `propagate` and `solve` (ADR-002's other two contract verbs) are not defined by this trait
  at all yet -- deliberately, per this task's scope, not an oversight.
- `propagate_covariance` itself still only symmetrizes and reports the pre-symmetrization
  asymmetry -- it does not verify positive-definiteness, deliberately: that check now lives
  one layer up, in `av-cdm::covariance` (a Cholesky-based SPD check mirroring
  `spoore_cdm::GaussianState`'s own bar, `docs/open-questions.md` question 80), because
  `av-cdm` -- not this crate -- is the crate that owns the boundary a covariance crosses on
  its way into a spoore type, and already depends on `nalgebra` and `spoore-cdm` for exactly
  that reason. `av-kernel::Kernel::run_with_covariance` calls it on every sample before
  filling `TrajectorySample.cov`; see `crates/av-cdm/README.md` and
  `crates/av-kernel/README.md`'s "Covariance hygiene" sections. This crate stays as it was:
  the linear algebra and the symmetrization live here, the spoore-boundary invariant lives in
  `av-cdm`.
