//! The multi-rate scheduler: several systems, each with its own step period, all driven off
//! one [`crate::clock::Clock`] (ADR-002 "Rates and pacing" -- "step rates are declared per
//! system in the system definition, default 10 Hz for kernel dynamics"). A system stepping
//! at 50 Hz and one at 10 Hz are both driven correctly: each is advanced by exactly its own
//! period, as many times as needed, independent of what any other system's period is or what
//! rate the caller happens to be sampling output at.
//!
//! Iteration over registered systems is always by system id (`BTreeMap`), so two runs given
//! the same systems and inputs schedule the same operations in the same order no matter what
//! order the systems were registered in (ADR-002 / ADR-004 determinism).
//!
//! **Scope note (M2.1 skeleton):** `Scheduler<M>` (below) is homogeneous over a single
//! `DynamicsModel` type `M` -- every system it drives must be the same Rust type. It is kept
//! exactly as it was: every existing test in this crate (and `tests/golden_acceptance.rs`)
//! drives a `Scheduler<M>`/`Kernel<M>` directly, and ADR-005's hard constraint is that they
//! keep passing unchanged. Real multi-domain scheduling (a space model and a 6-DoF air model
//! in the same run) is [`HeteroScheduler`] (ADR-005 sec 1): the trait objects
//! (`Box<dyn DynamicsModel<Error = ModelError>>`, `av_dynamics::BoxedModel`) and shared error
//! type this module's old scope note flagged as undecided are exactly what ADR-005 sec 1
//! decided (`av_dynamics::ModelError`/`av_dynamics::ErasedModel`) -- see `HeteroScheduler`'s
//! own doc comment below.
//!
//! **Controls are not yet wired.** Every step call passes an empty control slice (`&[]`);
//! routing a per-system control schedule into the scheduler is out of scope for this
//! skeleton (bindings/the port router, ADR-005 sec 4, are still Planned/P2).

use std::collections::BTreeMap;

use av_dynamics::{BoxedModel, DynamicsModel, ModelError};

use crate::ports::AppliedPortCommand;

/// Named output series for one system, as [`Scheduler::outputs`]/[`HeteroScheduler::outputs`]
/// return them: `.0` is the native step epochs (TAI ns) every entry in `.1` is parallel to,
/// `.1` is every output name's own value series -- see [`Scheduler::outputs`]'s own doc comment
/// for exactly what epochs this covers. Factored into a named alias (clippy::type_complexity)
/// rather than written out at every call site.
pub type OutputSeries<'a> = (&'a [i64], &'a BTreeMap<String, Vec<f64>>);

/// What answering `sample(id, t_tai_ns)` would require -- [`Scheduler::sample_kind`]/
/// [`HeteroScheduler::sample_kind`] report this directly (borrowed, un-blended) rather than
/// making a covariance-propagating caller re-derive it from `sample`'s own already-interpolated
/// `Vec<f64>` (M13.3, ADR-005 sec 3: "state transition matrix, covariance | never interpolated
/// -- available only at the instance's own samples"). A caller carrying an STM/covariance tail
/// in its own state (`av_dynamics::StmAugmented`) must never let [`SampleKind::Between`]'s
/// blended vector reach that tail -- only the leading, ordinarily-interpolable physical
/// components -- which is exactly why this exists as its own type instead of `sample` growing a
/// "did I interpolate?" out-parameter: the *caller* (`crate::kernel`), not this module, knows
/// which components of its own state are STM-bearing.
#[derive(Debug, Clone, Copy)]
pub enum SampleKind<'a> {
    /// `t_tai_ns` is exactly one of this system's own two most recently recorded native step
    /// times -- no interpolation of any kind; the borrowed slice is that native step's own
    /// state, in full.
    Native(&'a [f64]),
    /// `t_tai_ns` falls strictly between this system's two most recent native step times --
    /// `sample` itself resolves this by calling `crate::interpolate::hermite_velocity` on the
    /// whole vector; a caller with STM/covariance components must instead interpolate only its
    /// own physical prefix (or refuse to, per ADR-005 sec 3) and treat the rest as unavailable.
    /// Reachable for any system whose own period exceeds the caller's sampling rate (M13.3 lifts
    /// exactly that restriction for a covariance-requesting system driven through `advance_to`;
    /// M16.1, question 119, lifts the same restriction for a plain physical system driven through
    /// [`HeteroScheduler::advance_to_with_ports`]) -- including the interval before that system's
    /// very first *reported* native step, because [`Scheduler::advance_to`]/
    /// [`HeteroScheduler::advance_to`]/[`HeteroScheduler::advance_to_with_ports`] all advance a
    /// physical system at least one full period past whatever target it is given (see each
    /// method's own doc comment), so a bracket already exists (`prev`, the seed, and `curr`, the
    /// first real step) by the time any output tick in that window is ever queried -- there is no
    /// separate "no bracket yet" case to handle.
    Between { prev_t: i64, prev_s: &'a [f64], curr_t: i64, curr_s: &'a [f64] },
}

/// What [`HeteroScheduler::sample_held`] found at a query time for a system with **no physical
/// state to interpolate at all** (`state_dim() == 0`, e.g. a `BINDING_KIND_CONTAINER` instance
/// -- ADR-005 sec 3's "discrete modes, counters | zero-order hold" rule, applied to a system that
/// carries nothing *but* discrete/counter-shaped outputs). Unlike [`SampleKind`], there is no
/// `Between` bracket: a zero-order hold never blends two points, it only ever repeats the most
/// recently delivered one, so this only distinguishes *whether* the query landed exactly on that
/// delivery or is still waiting for the next one -- M14.4 (`docs/open-questions.md`, "lift
/// `ContainerPeriodExceedsSampleInterval`"): a container's own trajectory sample must say which,
/// not silently look identical either way. M15.2 (question 116) carries this straight onto the
/// wire: `HeteroKernel::run_with_ports` maps `Fresh`/`Held` onto `TrajectorySample.kind`
/// (`av_cdm::pb::SampleKind::Native`/`Held`) directly, since `TrajectorySample.mean` itself is
/// always empty for a zero-dimensional system and so cannot carry the distinction on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldKind {
    /// `t_tai_ns` is exactly this system's own most recently recorded native step time -- a
    /// real, freshly produced value, not a hold.
    Fresh,
    /// `t_tai_ns` is strictly after this system's own most recently recorded native step time
    /// (that next native step has not happened yet) -- the returned value is the last one
    /// actually delivered, repeated flat.
    Held,
}

/// The last one or two native step results for one system -- enough to interpolate (or
/// return exactly) the state at any time within `[prev.0, curr.0]`.
#[derive(Debug, Clone)]
struct History {
    prev: Option<(i64, Vec<f64>)>,
    curr: (i64, Vec<f64>),
}

struct SystemEntry<M: DynamicsModel> {
    period_ns: i64,
    model: M,
    history: History,
    /// Every native step's `StepResult.outputs` this system has produced so far, accumulated
    /// across every `advance_to` call (question 95's second half: `av_dynamics::StepResult
    /// .outputs`, populated today only by `gmat_sys::model::GmatModel::step`). `epochs_tai_ns`
    /// is the native step time of each entry, parallel across every name in `values` -- see
    /// [`Scheduler::outputs`].
    output_epochs_tai_ns: Vec<i64>,
    outputs: BTreeMap<String, Vec<f64>>,
}

/// Error advancing or sampling the scheduler.
#[derive(Debug)]
pub enum ScheduleError<E> {
    /// The underlying model's `step` failed.
    Model(E),
    /// No system is registered under that id.
    UnknownSystem(String),
    /// The requested sample time is outside every history window this system currently has
    /// (before its first recorded sample, or past the last time `advance_to` reached for
    /// it) -- this scheduler only interpolates, it never extrapolates.
    OutOfRange { system: String, t_tai_ns: i64, earliest_ns: i64, latest_ns: i64 },
    /// A covariance `crate::kernel::Kernel::run_with_covariance` propagated failed the
    /// Cholesky-based SPD hygiene check (`av_cdm::covariance`, `docs/open-questions.md`
    /// question 80) and the caller did not opt into the nearest-SPD projection -- see that
    /// method's doc comment. Never raised by plain `run` (which never computes a covariance
    /// at all).
    CovarianceHygiene(av_cdm::covariance::CovarianceHygieneError),
}

impl<E: std::fmt::Display> std::fmt::Display for ScheduleError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScheduleError::Model(e) => write!(f, "dynamics model step failed: {e}"),
            ScheduleError::UnknownSystem(id) => write!(f, "no system registered with id {id:?}"),
            ScheduleError::OutOfRange { system, t_tai_ns, earliest_ns, latest_ns } => {
                write!(f, "system {system:?}: t_tai_ns {t_tai_ns} outside history window [{earliest_ns}, {latest_ns}]")
            }
            ScheduleError::CovarianceHygiene(e) => write!(f, "covariance hygiene check failed: {e}"),
        }
    }
}
impl<E: std::fmt::Debug + std::fmt::Display> std::error::Error for ScheduleError<E> {}

/// Several [`DynamicsModel`] systems of the same type `M`, each stepped at its own declared
/// period, all driven off one simulated-time authority.
pub struct Scheduler<M: DynamicsModel> {
    systems: BTreeMap<String, SystemEntry<M>>,
}

impl<M: DynamicsModel> Default for Scheduler<M> {
    fn default() -> Self {
        Self { systems: BTreeMap::new() }
    }
}

impl<M: DynamicsModel> Scheduler<M> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a system: `period_ns` is how often it steps (ADR-002: declared per system,
    /// not a property of where the kernel runs); `t0_tai_ns`/`initial_state` seed its history
    /// so it can be sampled starting at `t0_tai_ns` even before its first step.
    pub fn register(&mut self, id: impl Into<String>, period_ns: i64, model: M, t0_tai_ns: i64, initial_state: Vec<f64>) {
        assert!(period_ns > 0, "system period must be positive, got {period_ns} ns");
        self.systems.insert(
            id.into(),
            SystemEntry {
                period_ns,
                model,
                history: History { prev: None, curr: (t0_tai_ns, initial_state) },
                output_epochs_tai_ns: Vec::new(),
                outputs: BTreeMap::new(),
            },
        );
    }

    /// Advance every registered system, in id order, until each has stepped to (or past)
    /// `target_tai_ns` -- i.e. until each system's most recent native step time is `>=
    /// target_tai_ns`. A system whose period is larger than `target_tai_ns - <its current
    /// time>` steps once, past the target; a system whose period is much smaller steps
    /// repeatedly, each step exactly `period_ns` after the last, so its own step boundaries
    /// never depend on `target_tai_ns`'s value -- only on where it last was.
    ///
    /// **M13.3: the loop condition is `history.curr`'s own recorded time, not `next_due_ns`.**
    /// Before M13.3 these were interchangeable -- every existing caller only ever registered
    /// systems whose period divides evenly into the caller's own sampling rate, so a system was
    /// always already due (`next_due_ns <= target_tai_ns`) whenever it would otherwise have been
    /// left short of `target_tai_ns`. Once a covariance-requesting system's own period can
    /// exceed the sampling rate (`crate::kernel::Kernel::run_with_covariance`/`HeteroKernel::
    /// run_with_covariance`, the whole point of M13.3), the two diverge: `next_due_ns <=
    /// target_tai_ns` stops *before* catching up (this system's own most recent recorded step
    /// stays behind `target_tai_ns`, contradicting this very doc comment's "steps once, past the
    /// target" -- a latent bug this task's own tests exposed, never triggered by any prior
    /// caller). Looping on `history.curr.0 < target_tai_ns` instead restores the documented
    /// contract exactly: identical step count and timing whenever a period already divides the
    /// sampling rate evenly (every existing test), and, additively, one extra catch-up step for
    /// a system that would otherwise be left short -- which is exactly what lets
    /// `Scheduler::sample_kind`/`sample` resolve a query anywhere inside that system's own most
    /// recent native period as [`SampleKind::Between`] (a genuine two-point Hermite bracket)
    /// rather than needing any separate "no bracket yet" handling.
    pub fn advance_to(&mut self, target_tai_ns: i64) -> Result<(), ScheduleError<M::Error>> {
        for sys in self.systems.values_mut() {
            while sys.history.curr.0 < target_tai_ns {
                let (t_ns, state) = sys.history.curr.clone();
                let result = sys.model.step(&state, t_ns, &[], sys.period_ns).map_err(ScheduleError::Model)?;
                debug_assert_eq!(result.t_tai_ns, t_ns + sys.period_ns);
                if !result.outputs.is_empty() {
                    sys.output_epochs_tai_ns.push(result.t_tai_ns);
                    for (name, value) in &result.outputs {
                        sys.outputs.entry(name.clone()).or_default().push(*value);
                    }
                }
                sys.history.prev = Some(sys.history.curr.clone());
                sys.history.curr = (result.t_tai_ns, result.state);
            }
        }
        Ok(())
    }

    /// Every named output series system `id`'s model has produced so far via `StepResult
    /// .outputs` (question 95's second half) -- `None` if `id` was never registered. Epochs are
    /// exactly the native step times the model actually produced an output at (every name's
    /// series parallel to `.0`), **not** resampled onto any output-tick grid -- a caller wanting
    /// a value at an arbitrary time still needs its own interpolation (`crate::expr::runproducts
    /// ::ExprRunProducts::output_at` already does exactly that, linearly, over whatever epochs a
    /// series carries). Empty for a model that never populates `StepResult.outputs` -- every
    /// model in this crate today except a GMAT-bound `gmat_sys::model::GmatModel`.
    pub fn outputs(&self, id: &str) -> Option<OutputSeries<'_>> {
        self.systems.get(id).map(|s| (s.output_epochs_tai_ns.as_slice(), &s.outputs))
    }

    /// What answering [`Scheduler::sample`] at `t_tai_ns` would require -- see
    /// [`SampleKind`]'s own doc comment for why this exists as its own method rather than
    /// `sample` alone. Same within-range rule as `sample` ([`ScheduleError::OutOfRange`] under
    /// the identical condition), just returning the un-blended, borrowed pieces instead of an
    /// owned, already-interpolated `Vec<f64>`.
    pub fn sample_kind(&self, id: &str, t_tai_ns: i64) -> Result<SampleKind<'_>, ScheduleError<M::Error>> {
        let sys = self.systems.get(id).ok_or_else(|| ScheduleError::UnknownSystem(id.to_string()))?;
        let (curr_t, curr_s) = &sys.history.curr;
        if t_tai_ns == *curr_t {
            return Ok(SampleKind::Native(curr_s.as_slice()));
        }
        match &sys.history.prev {
            Some((prev_t, prev_s)) if *prev_t <= t_tai_ns && t_tai_ns <= *curr_t => {
                if t_tai_ns == *prev_t {
                    Ok(SampleKind::Native(prev_s.as_slice()))
                } else {
                    Ok(SampleKind::Between { prev_t: *prev_t, prev_s: prev_s.as_slice(), curr_t: *curr_t, curr_s: curr_s.as_slice() })
                }
            }
            _ => {
                let earliest_ns = sys.history.prev.as_ref().map(|(t, _)| *t).unwrap_or(*curr_t);
                Err(ScheduleError::OutOfRange { system: id.to_string(), t_tai_ns, earliest_ns, latest_ns: *curr_t })
            }
        }
    }

    /// The state of system `id` at `t_tai_ns`: exact if `t_tai_ns` is one of that system's
    /// two most recent native step times, Hermite-with-velocity interpolated
    /// ([`crate::interpolate::hermite_velocity`]) otherwise. Requires `t_tai_ns` to fall
    /// within the system's current history window (i.e. `advance_to` must already have been
    /// called with a `target_tai_ns >= t_tai_ns`) -- this never extrapolates. Built on
    /// [`Scheduler::sample_kind`] (unchanged behaviour, just factored so a covariance-carrying
    /// caller can ask the same question without receiving an already-blended vector).
    pub fn sample(&self, id: &str, t_tai_ns: i64) -> Result<Vec<f64>, ScheduleError<M::Error>> {
        match self.sample_kind(id, t_tai_ns)? {
            SampleKind::Native(s) => Ok(s.to_vec()),
            SampleKind::Between { prev_t, prev_s, curr_t, curr_s } => Ok(crate::interpolate::hermite_velocity(prev_t, prev_s, curr_t, curr_s, t_tai_ns)),
        }
    }

    /// The [`av_cdm::pb::ModelInfo`] of the system registered under `id`, if any.
    pub fn describe(&self, id: &str) -> Option<av_cdm::pb::ModelInfo> {
        self.systems.get(id).map(|sys| sys.model.describe())
    }

    /// The step period a system was registered with, if any. Used by
    /// `crate::kernel::Kernel::run_with_covariance` to refuse a system whose period does not
    /// match the kernel's output rate -- see that method's doc comment for why.
    pub fn period_ns(&self, id: &str) -> Option<i64> {
        self.systems.get(id).map(|sys| sys.period_ns)
    }

    /// Registered system ids, in the deterministic (sorted) order every other method visits
    /// them in.
    pub fn system_ids(&self) -> impl Iterator<Item = &str> {
        self.systems.keys().map(String::as_str)
    }
}

// =============================================================================================
// HeteroScheduler: the trait-object, multi-model-kind scheduler (ADR-005 sec 1)
// =============================================================================================

/// The last one or two native step results for one [`HeteroScheduler`] system -- identical in
/// shape and purpose to [`History`] above, duplicated rather than shared because [`Scheduler`]
/// stores a concrete `M` inline while this stores a `Box<dyn DynamicsModel<...>>`.
#[derive(Debug, Clone)]
struct HeteroHistory {
    prev: Option<(i64, Vec<f64>)>,
    curr: (i64, Vec<f64>),
}

struct HeteroSystemEntry {
    period_ns: i64,
    model: BoxedModel,
    next_due_ns: i64,
    history: HeteroHistory,
    /// See [`SystemEntry::output_epochs_tai_ns`]/`.outputs` -- identical purpose, duplicated for
    /// the same reason [`HeteroHistory`] duplicates [`History`].
    output_epochs_tai_ns: Vec<i64>,
    outputs: BTreeMap<String, Vec<f64>>,
    /// Every command this system's own `step_with_ports` calls have actually applied so far
    /// (`docs/open-questions.md` question 130) -- only ever populated by
    /// [`HeteroScheduler::advance_to_with_ports`] (plain [`HeteroScheduler::advance_to`] never
    /// calls `step_with_ports` at all, so this stays empty for a system driven only that way).
    applied_commands: Vec<AppliedPortCommand>,
    /// Every CDM `Measurement` this system's own `step_with_ports` calls have actually produced
    /// so far (`docs/open-questions.md` question 173, M25.3) -- collected from `model.
    /// last_measurements()` right after each call, the same "read once, right after the step
    /// that produced it" pattern `applied_commands` above already uses. `sensor_id` is filled in
    /// here from `id` (this `BTreeMap`'s own key), since the wrapped model itself never knows
    /// its own instance name -- see `crate::drm::sensors::StarTrackerModel::step_with_ports`'s
    /// own doc comment for why, and `AppliedPortCommand`'s doc comment for the identical
    /// enrichment already established for applied commands. Only ever populated by
    /// [`HeteroScheduler::advance_to_with_ports`], same as `applied_commands`.
    measurements: Vec<av_cdm::pb::Measurement>,
    /// Question 178 (R5.1a): this system's own SENSOR fault effect, accumulated across every
    /// `step_with_ports` call this ENTIRE `HeteroScheduler` has driven so far -- collected from
    /// `model.drain_sensor_fault_effect()` right after each call, the identical "read once,
    /// right after the step that produced it" pattern `measurements`/`applied_commands` above
    /// already use. **Why accumulation must happen here, per step, rather than reading the
    /// model once after the whole run:** `crate::drm::executor::run_one_span` erases every
    /// `ModelHandle` into a `BoxedModel` and hands it to `HeteroKernel::register_system` --
    /// `ModelSpanState::handle` is `None` for the ENTIRE duration of the run this scheduler
    /// drives (`run_one_span`'s own `span.handle.take()`), so there is no live handle left to
    /// drain from once `run_with_ports` returns; the underlying model itself is dropped along
    /// with this scheduler at the end of `run_one_span`. Only ever populated by
    /// [`HeteroScheduler::advance_to_with_ports`], same as `applied_commands`/`measurements`.
    sensor_fault_effect: (Option<i64>, u64),
    /// Every undecodable FRAMED frame this system's own `step_with_ports` calls have recorded so
    /// far (`docs/open-questions.md` question 188, R5.2) -- collected from `model.
    /// drain_decode_errors()` right after each call, the identical "read once, right after the
    /// step that produced it, ever-growing list" pattern `measurements` above already uses (not
    /// `sensor_fault_effect`'s own running-total shape: a decode error is not tied to any
    /// declared `Fault`'s own window/boundary, so there is no cross-span total to fold -- every
    /// occurrence is already final the moment `step_with_ports` returns it). `instance` is filled
    /// in here from `id` (this `BTreeMap`'s own key), mirroring `measurements`'s own `sensor_id`
    /// enrichment for the identical reason: the wrapped model itself never knows its own instance
    /// name. Only ever populated by [`HeteroScheduler::advance_to_with_ports`], same as
    /// `applied_commands`/`measurements`.
    decode_errors: Vec<crate::ports::DecodeErrorRecord>,
    /// Question 193 (R6.2): every successful decode this system's own `step_with_ports` calls
    /// have recorded so far, derived (not reported by the model) right next to `decode_errors`
    /// above -- see [`crate::ports::DecodeSuccessRecord`]'s own doc comment for exactly how and
    /// why. Only ever populated by [`HeteroScheduler::advance_to_with_ports`], same as
    /// `decode_errors`.
    decode_successes: Vec<crate::ports::DecodeSuccessRecord>,
}

/// Error advancing or sampling a [`HeteroScheduler`] -- the non-generic twin of
/// [`ScheduleError`], carrying [`av_dynamics::ModelError`] directly rather than a
/// model-specific `E` (ADR-005 sec 1: every trait-object model speaks the one error type, so
/// this error type needs no type parameter either).
#[derive(Debug)]
pub enum HeteroScheduleError {
    /// The underlying model's `step` failed.
    Model(ModelError),
    /// No system is registered under that id.
    UnknownSystem(String),
    /// The requested sample time is outside every history window this system currently has --
    /// see [`ScheduleError::OutOfRange`], which this mirrors exactly.
    OutOfRange { system: String, t_tai_ns: i64, earliest_ns: i64, latest_ns: i64 },
}

impl std::fmt::Display for HeteroScheduleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HeteroScheduleError::Model(e) => write!(f, "dynamics model step failed: {e}"),
            HeteroScheduleError::UnknownSystem(id) => write!(f, "no system registered with id {id:?}"),
            HeteroScheduleError::OutOfRange { system, t_tai_ns, earliest_ns, latest_ns } => {
                write!(f, "system {system:?}: t_tai_ns {t_tai_ns} outside history window [{earliest_ns}, {latest_ns}]")
            }
        }
    }
}
impl std::error::Error for HeteroScheduleError {}

/// ADR-005 sec 1's heterogeneous scheduler: several systems, **each its own `DynamicsModel`
/// kind** (a spacecraft on GMAT dynamics, an aircraft on a 6-DoF model, a power subsystem on a
/// 0-D model, ...), each at its own declared step period, all driven off one simulated-time
/// authority -- exactly [`Scheduler`]'s job, minus that type's single-`M` limitation. Holds no
/// type parameter at all: `BTreeMap<InstanceName, Box<dyn DynamicsModel<Error = ModelError>>>`
/// (`av_dynamics::BoxedModel`), so instance order -- and therefore output order -- is always
/// the sorted instance name, never insertion order (ADR-005 sec 1's determinism rule, ADR-004)
/// -- `system_ids`'s own test below pins this the same way [`Scheduler::system_ids`]'s does.
///
/// A model that does not itself implement `DynamicsModel<Error = ModelError>` needs
/// `av_dynamics::erase_with_id`/`ErasedModel` first (`crate::registry`'s constructors do this
/// for the native/GMAT cases) -- this type never converts an error itself, it only ever calls
/// `model.step`, whose `Result`'s error type is already pinned to `ModelError` by the `BoxedModel`
/// alias.
///
/// **Lockstep** (ADR-005 sec 2's default): `advance_to` only ever steps a system up to
/// `target_tai_ns`, never past it speculatively, and every system's native step times are
/// exact multiples of its own declared period from its own `t0_tai_ns` -- the same lockstep
/// contract [`Scheduler::advance_to`] documents, reused verbatim here. [`Self::base_period_ns`]
/// wires `crate::clock::base_period_ns`/`check_integer_multiples` (ADR-005 sec 2's base-period
/// clock) into this scheduler directly, as a load-time gate a caller runs once before driving
/// `advance_to`.
#[derive(Default)]
pub struct HeteroScheduler {
    systems: BTreeMap<String, HeteroSystemEntry>,
}

impl HeteroScheduler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a system under `id`, exactly like [`Scheduler::register`] except `model` is
    /// already boxed and erased to `ModelError` (`av_dynamics::BoxedModel`) -- see that
    /// method's doc comment for `period_ns`/`t0_tai_ns`/`initial_state`.
    pub fn register(&mut self, id: impl Into<String>, period_ns: i64, model: BoxedModel, t0_tai_ns: i64, initial_state: Vec<f64>) {
        assert!(period_ns > 0, "system period must be positive, got {period_ns} ns");
        self.systems.insert(
            id.into(),
            HeteroSystemEntry {
                period_ns,
                model,
                next_due_ns: t0_tai_ns + period_ns,
                history: HeteroHistory { prev: None, curr: (t0_tai_ns, initial_state) },
                output_epochs_tai_ns: Vec::new(),
                outputs: BTreeMap::new(),
                applied_commands: Vec::new(),
                measurements: Vec::new(),
                sensor_fault_effect: (None, 0),
                decode_errors: Vec::new(),
                decode_successes: Vec::new(),
            },
        );
    }

    /// Advance every registered system, **in sorted instance-name order** (`BTreeMap` iteration
    /// -- ADR-005 sec 1's determinism rule made real, not merely documented: two runs given the
    /// same systems and inputs step them in the same order no matter what order they were
    /// registered in, exactly like [`Scheduler::advance_to`]), until each has stepped to (or
    /// past) `target_tai_ns`. See [`Scheduler::advance_to`]'s doc comment for the exact
    /// per-system stepping rule this mirrors, **including its M13.3 fix** (looping on
    /// `history.curr`'s own recorded time rather than `next_due_ns`, so a system whose period
    /// exceeds `target_tai_ns - <its current time>` is guaranteed to end up caught up past
    /// `target_tai_ns`, not left short of it) -- `next_due_ns` itself is unchanged and still
    /// exact (needed by [`HeteroScheduler::advance_to_with_ports`]'s own, different stepping
    /// order).
    pub fn advance_to(&mut self, target_tai_ns: i64) -> Result<(), HeteroScheduleError> {
        for sys in self.systems.values_mut() {
            while sys.history.curr.0 < target_tai_ns {
                let (t_ns, state) = sys.history.curr.clone();
                let result = sys.model.step(&state, t_ns, &[], sys.period_ns).map_err(HeteroScheduleError::Model)?;
                debug_assert_eq!(result.t_tai_ns, t_ns + sys.period_ns);
                if !result.outputs.is_empty() {
                    sys.output_epochs_tai_ns.push(result.t_tai_ns);
                    for (name, value) in &result.outputs {
                        sys.outputs.entry(name.clone()).or_default().push(*value);
                    }
                }
                sys.history.prev = Some(sys.history.curr.clone());
                sys.history.curr = (result.t_tai_ns, result.state);
                sys.next_due_ns += sys.period_ns;
            }
        }
        Ok(())
    }

    /// Like [`HeteroScheduler::advance_to`], but drives every native step through
    /// `av_dynamics::DynamicsModel::step_with_ports` instead of `step`, asking `router` for each
    /// instance's currently-*available* [`crate::ports::Inbox`] (question 110: everything queued
    /// for that instance whose availability epoch is `<=` this very step's own epoch -- see
    /// [`crate::router::Router::take_inbox`]) immediately before its own step, and handing the
    /// returned `Outbox` back to `router` for delivery to any connected receiver
    /// (`docs/open-questions.md` question 108: "`HeteroKernel` passes the inbox to step and
    /// collects the outbox").
    ///
    /// **Why this cannot reuse [`HeteroScheduler::advance_to`]'s own per-system loop.** That
    /// loop advances one system all the way to `target_tai_ns` (every one of its own native
    /// steps) before touching the next system at all -- correct when systems never talk to each
    /// other, since [`Scheduler::sample`]/[`HeteroScheduler::sample`] only ever read a finished
    /// system's own history. Ports break that independence: question 110's rule ("delivered at
    /// the first receiver step whose own epoch is `>=` availability") only makes sense if a
    /// message a sender emits at time `t` is even queued before a receiver whose own next native
    /// step is also at (or after) `t` asks for its inbox, which requires stepping every system
    /// that is due at the *same* instant together, in lock with each other, rather than running
    /// one system to completion first. This method therefore steps in native-time order across
    /// **every** registered system: at each iteration it finds the smallest `next_due_ns` among
    /// every system still [`Self::catch_up_eligible`] for `target_tai_ns`, steps every system
    /// tied for that minimum (in `BTreeMap` -- i.e. sorted instance-name -- order, same
    /// determinism rule as `advance_to`), delivers each of their outboxes to `router`, and
    /// repeats until no system is eligible any more. Two systems that never step at the same
    /// native time never interleave differently than `advance_to` would order them either (each
    /// is still visited id-sorted whenever it is its turn), so this is a strict refinement of
    /// `advance_to`'s own determinism guarantee, not a different one -- and it is what keeps
    /// question 108's own delivery-order rule (receiving instance, then port name, then sender
    /// emission epoch, then sender instance id) intact under question 110's deferral: a held
    /// message re-enters the *same* per-receiver queue [`crate::ports::sorted_inbox`] sorts, so a
    /// later `take_inbox` call sorts it alongside whatever else is available then exactly as if
    /// it had just arrived, never in some separate, differently-ordered holding area.
    ///
    /// **M16.1 (`docs/open-questions.md` question 119): a coarse *physical* system now catches up
    /// past `target_tai_ns`, atomically, exactly like [`HeteroScheduler::advance_to`].** Before
    /// this task, the eligibility test was `next_due_ns <= target_tai_ns` for every system alike
    /// -- correct as long as every registered system's own period divides the query interval
    /// evenly, but wrong the moment a physical (`state_dim() != 0`) system's period exceeds
    /// `target_tai_ns` itself: its `next_due_ns` is never `<= target_tai_ns` at all, so it is
    /// never stepped, `history.curr` never advances past whatever it started at, and the very
    /// next off-grid [`HeteroScheduler::sample_kind`] query for it fails `OutOfRange` -- no
    /// `prev`/`curr` bracket to interpolate ever gets built (M15.2's `#[ignore]`d reproducer in
    /// `crate::kernel`). The fix, per the lead's ruling (no fork between question 109's shared run
    /// and question 110's ordering): a coarse system's step from its own current time to
    /// `+ period_ns` is one atomic advance, so [`Self::catch_up_eligible`] uses `history.curr.0 <
    /// target_tai_ns` for a physical system -- the exact same test [`HeteroScheduler::advance_to`]
    /// already uses -- rather than gating on `next_due_ns`. Its outputs are still stamped with the
    /// step's own end epoch (`result.t_tai_ns`, unchanged below) and its inputs are still whatever
    /// `router.take_inbox` returns as of that same epoch (unchanged below) -- question 110's
    /// existing availability gate ([`crate::router::Router::take_inbox`]) is what keeps a stepped-
    /// ahead sender's message from reaching a receiver before the receiver's own step reaches
    /// that epoch; stepping a sender ahead changes *when it is asked to step*, never *when its
    /// output becomes available*, so nothing new needs gating here. A query strictly between two
    /// of the caught-up system's own native steps is answered by [`SampleKind::Between`]
    /// (Hermite interpolation, `SampleKind::INTERPOLATED` on the wire) exactly as `advance_to`
    /// already provides for `Kernel::run`/`run_with_covariance`.
    ///
    /// **A zero-dimensional system (`state_dim() == 0`, `BINDING_KIND_CONTAINER`) is deliberately
    /// exempted from this catch-up and keeps the original `next_due_ns <= target_tai_ns` test.**
    /// [`HeteroScheduler::sample_held`]'s zero-order hold already answers any query at or after
    /// `history.curr` correctly with no bracket at all (M14.4) -- it was never the system that
    /// failed here -- and, unlike `sample_kind`, it is deliberately forward-hold-only: a query
    /// strictly behind `history.curr` is `OutOfRange`, "never extrapolates backward, only holds
    /// forward" (`HeteroScheduler::sample_held`'s own doc comment, pinned by
    /// `hetero_scheduler_sample_held_is_fresh_at_the_seed_and_held_strictly_after_with_no_bracket_
    /// needed` below). Catching a container up ahead of `target_tai_ns` the same way as a physical
    /// system would move `history.curr` past `target_tai_ns` and turn every intermediate
    /// `sample_held` query for it into exactly that backward, refused case -- trading M15.2's bug
    /// for a new one in the one place that never had it. So [`Self::catch_up_eligible`] branches
    /// on `state_dim()`, and only a genuinely physical system gets the new, broader test.
    pub fn advance_to_with_ports(&mut self, target_tai_ns: i64, router: &mut crate::router::Router) -> Result<(), HeteroScheduleError> {
        loop {
            let next_time = self.systems.values().filter(|s| Self::catch_up_eligible(s, target_tai_ns)).map(|s| s.next_due_ns).min();
            let Some(t) = next_time else {
                break;
            };
            for (id, sys) in self.systems.iter_mut() {
                if sys.next_due_ns != t || !Self::catch_up_eligible(sys, target_tai_ns) {
                    continue;
                }
                let (t_ns, state) = sys.history.curr.clone();
                // Question 110: a message is delivered at the first receiver step whose own
                // epoch is `>=` its availability, never earlier. `t` (this iteration's tied
                // `next_due_ns`) *is* that step's own epoch -- `debug_assert_eq!` above/below
                // pins `t == t_ns + sys.period_ns`, i.e. the epoch `id`'s state will have
                // advanced to once this very step completes -- so `router.take_inbox` is asked
                // for exactly that epoch, not `t_ns` (the epoch it is stepping *from*). This
                // holds unchanged for a coarse system's own atomic catch-up step too: `t` is
                // still that one step's own end epoch, however many periods ahead of
                // `target_tai_ns` it lands.
                let inbox = router.take_inbox(id, t);
                let (result, outbox, applied) = sys.model.step_with_ports(&state, t_ns, &[], sys.period_ns, &inbox).map_err(HeteroScheduleError::Model)?;
                debug_assert_eq!(result.t_tai_ns, t_ns + sys.period_ns);
                if !result.outputs.is_empty() {
                    sys.output_epochs_tai_ns.push(result.t_tai_ns);
                    for (name, value) in &result.outputs {
                        sys.outputs.entry(name.clone()).or_default().push(*value);
                    }
                }
                // Question 130: enrich each applied command with this receiving instance's own
                // id and (when known) its sender -- resolved from the SAME `inbox` the model was
                // just handed, via the identical "last message on this port" selection the model
                // itself used (`Inbox::last_on_port`), so the two can never disagree about which
                // message was actually applied.
                for cmd in applied {
                    let sender = inbox.last_on_port(&cmd.port).and_then(|(_msg, sender)| sender).map(str::to_string);
                    sys.applied_commands.push(AppliedPortCommand { instance: id.clone(), port: cmd.port, field: cmd.field, value: cmd.value, applied_tai_ns: cmd.applied_tai_ns, sender });
                }
                // Question 173 (M25.3): read right after the step that produced them, exactly
                // like `applied` above -- `sensor_id` filled in from `id` (this model itself
                // does not know its own instance name; see `HeteroSystemEntry::measurements`'s
                // own doc comment).
                //
                // Question 176 (M25.3c), pinned by the lead: "decoding happens at the emitting
                // sensor through its own codec, so a packet the router later drops still yields a
                // measurement... `Measurement.meta["decoded_at"]` naming the instance." Stamped
                // here, in the same place and for the same reason `sensor_id` is: this model
                // computed `z`/`r` from the values it *itself* just encoded into the outbound
                // packet a few lines above (`StarTrackerModel`/`ImuModel::step_with_ports`), before
                // that packet is handed to `router.deliver` below -- so this measurement exists
                // regardless of whether the router goes on to actually deliver it anywhere.
                // `decoded_at` is the emitting instance `id`, identical to `sensor_id` today (no
                // receiver-side decode exists yet -- that is a later task's scope, not this one's),
                // but a distinct field because a future receiver-side `Measurement` (question 176's
                // own "receiver... decodes... into its own measurements with its own decoded_at")
                // would carry a *different* `sensor_id` (the model the measurement is *about*) from
                // `decoded_at` (the instance that *decoded* it).
                for mut measurement in sys.model.last_measurements() {
                    measurement.sensor_id = id.clone();
                    measurement.meta.insert("decoded_at".to_string(), id.clone());
                    sys.measurements.push(measurement);
                }
                // Question 178 (R5.1a): drain and fold this step's own SENSOR fault effect (if
                // any) into `sys.sensor_fault_effect` -- see that field's own doc comment for
                // why this must happen here, per step, rather than once after the whole run.
                if let Some(drain) = sys.model.drain_sensor_fault_effect() {
                    sys.sensor_fault_effect.1 += drain.frames_affected;
                    if drain.frames_affected > 0 {
                        sys.sensor_fault_effect.0 = Some(sys.sensor_fault_effect.0.map_or(drain.first_effect_tai_ns, |e| e.min(drain.first_effect_tai_ns)));
                    }
                }
                // Question 188 (R5.2): enrich and collect this call's own undecodable-frame
                // occurrences, exactly like `measurements` above (`instance` filled in from `id`,
                // for the identical reason).
                let mut failed_ports_this_call: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
                for occ in sys.model.drain_decode_errors() {
                    failed_ports_this_call.insert(occ.port.clone());
                    sys.decode_errors.push(crate::ports::DecodeErrorRecord { instance: id.clone(), port: occ.port, tai_ns: occ.tai_ns, sequence_count: occ.sequence_count, error: occ.error });
                }
                // Question 193 (R6.2): derive this call's own successful decodes -- see
                // `crate::ports::DecodeSuccessRecord`'s own doc comment for the full "why derived
                // here, not reported by the model" account. A port counts as successfully decoded
                // this call when `inbox` carried a message on it (`Inbox::last_on_port`, the same
                // selection every real FRAMED consumer uses) and that same port did NOT just
                // appear in `failed_ports_this_call` above (built from the identical `drain_
                // decode_errors()` call this loop already made this step). Every distinct port
                // name in `inbox` is checked once, regardless of how many messages that port
                // carries this step (only the LAST one, `last_on_port`, is ever actually
                // attempted).
                let mut checked_ports_this_call: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
                for msg in inbox.messages() {
                    if !checked_ports_this_call.insert(msg.port.as_str()) {
                        continue;
                    }
                    if failed_ports_this_call.contains(&msg.port) {
                        continue;
                    }
                    if let Some((last_msg, _sender)) = inbox.last_on_port(&msg.port) {
                        sys.decode_successes.push(crate::ports::DecodeSuccessRecord { instance: id.clone(), port: msg.port.clone(), tai_ns: last_msg.tai_ns });
                    }
                }
                sys.history.prev = Some(sys.history.curr.clone());
                sys.history.curr = (result.t_tai_ns, result.state);
                router.deliver(id, result.t_tai_ns, outbox);
                sys.next_due_ns += sys.period_ns;
            }
        }
        Ok(())
    }

    /// Whether `sys` still needs another native step before [`HeteroScheduler::
    /// advance_to_with_ports`] can stop advancing it towards `target_tai_ns` -- see that method's
    /// own doc comment (M16.1, question 119) for why a physical system and a zero-dimensional one
    /// use different tests. A physical system (`state_dim() != 0`) mirrors [`HeteroScheduler::
    /// advance_to`]'s own catch-up test (`history.curr.0 < target_tai_ns`), guaranteeing it ends
    /// up caught up *past* `target_tai_ns` -- never left short of it -- exactly like `advance_to`.
    /// A zero-dimensional system keeps the original, narrower `next_due_ns <= target_tai_ns` test,
    /// since [`HeteroScheduler::sample_held`] needs no catch-up (it already answers any query at
    /// or after `history.curr` with no bracket) and is forward-hold-only (a query behind
    /// `history.curr` is a typed refusal, not something to engineer around by moving `history.curr`
    /// ahead of the query itself).
    fn catch_up_eligible(sys: &HeteroSystemEntry, target_tai_ns: i64) -> bool {
        if sys.model.state_dim() == 0 {
            sys.next_due_ns <= target_tai_ns
        } else {
            sys.history.curr.0 < target_tai_ns
        }
    }

    /// See [`Scheduler::outputs`] -- identical contract.
    pub fn outputs(&self, id: &str) -> Option<OutputSeries<'_>> {
        self.systems.get(id).map(|s| (s.output_epochs_tai_ns.as_slice(), &s.outputs))
    }

    /// Every command [`HeteroScheduler::advance_to_with_ports`] has recorded `id` actually
    /// applying so far (`docs/open-questions.md` question 130) -- in the order the underlying
    /// `step_with_ports` calls returned them (native-step order, i.e. epoch order for one
    /// instance). Always empty for `id` if it was only ever driven through
    /// [`HeteroScheduler::advance_to`] (no ports in play at all) or never applied anything.
    pub fn applied_commands(&self, id: &str) -> Option<&[AppliedPortCommand]> {
        self.systems.get(id).map(|s| s.applied_commands.as_slice())
    }

    /// Every CDM `Measurement` [`HeteroScheduler::advance_to_with_ports`] has recorded `id`
    /// actually producing so far (`docs/open-questions.md` question 173) -- in the order the
    /// underlying `step_with_ports` calls returned them (native-step order). Always empty for
    /// `id` if it was only ever driven through [`HeteroScheduler::advance_to`], or if its own
    /// model never overrides `last_measurements` (every model except `StarTrackerModel`/
    /// `ImuModel` today).
    pub fn measurements(&self, id: &str) -> Option<&[av_cdm::pb::Measurement]> {
        self.systems.get(id).map(|s| s.measurements.as_slice())
    }

    /// The total SENSOR fault effect [`HeteroScheduler::advance_to_with_ports`] has accumulated
    /// for `id` over the WHOLE lifetime of this scheduler (question 178, R5.1a) -- `None` if `id`
    /// is not registered, or if nothing has been affected (no fault installed on `id`'s own
    /// model, or one installed but not yet reached by a real emission). Unlike `measurements`/
    /// `applied_commands` (an ever-growing list), this is a single running total, since that is
    /// exactly what `av_dynamics::SensorFaultEffectDrain` itself already is (see that type's own
    /// doc comment).
    pub fn sensor_fault_effect(&self, id: &str) -> Option<av_dynamics::SensorFaultEffectDrain> {
        let sys = self.systems.get(id)?;
        let frames_affected = sys.sensor_fault_effect.1;
        if frames_affected == 0 {
            return None;
        }
        let first_effect_tai_ns = sys.sensor_fault_effect.0.expect("frames_affected > 0 implies the first-effect epoch was recorded alongside it");
        Some(av_dynamics::SensorFaultEffectDrain { first_effect_tai_ns, frames_affected })
    }

    /// Every undecodable FRAMED frame [`HeteroScheduler::advance_to_with_ports`] has recorded
    /// `id` receiving so far (`docs/open-questions.md` question 188, R5.2) -- in the order the
    /// underlying `step_with_ports` calls returned them (native-step order), mirroring
    /// [`HeteroScheduler::measurements`]'s own identical contract. Always empty for `id` if it
    /// was only ever driven through [`HeteroScheduler::advance_to`], or if its own model never
    /// hit a decode error.
    pub fn decode_errors(&self, id: &str) -> Option<&[crate::ports::DecodeErrorRecord]> {
        self.systems.get(id).map(|s| s.decode_errors.as_slice())
    }

    /// Every successful decode [`HeteroScheduler::advance_to_with_ports`] has recorded `id`
    /// making so far (`docs/open-questions.md` question 193, R6.2) -- mirrors
    /// [`HeteroScheduler::decode_errors`]'s own identical contract exactly, one entry per
    /// (port, step) this instance decoded without failure. See [`crate::ports::
    /// DecodeSuccessRecord`]'s own doc comment for how this is derived.
    pub fn decode_successes(&self, id: &str) -> Option<&[crate::ports::DecodeSuccessRecord]> {
        self.systems.get(id).map(|s| s.decode_successes.as_slice())
    }

    /// See [`Scheduler::sample_kind`] -- identical contract, over [`HeteroScheduler`].
    pub fn sample_kind(&self, id: &str, t_tai_ns: i64) -> Result<SampleKind<'_>, HeteroScheduleError> {
        let sys = self.systems.get(id).ok_or_else(|| HeteroScheduleError::UnknownSystem(id.to_string()))?;
        let (curr_t, curr_s) = &sys.history.curr;
        if t_tai_ns == *curr_t {
            return Ok(SampleKind::Native(curr_s.as_slice()));
        }
        match &sys.history.prev {
            Some((prev_t, prev_s)) if *prev_t <= t_tai_ns && t_tai_ns <= *curr_t => {
                if t_tai_ns == *prev_t {
                    Ok(SampleKind::Native(prev_s.as_slice()))
                } else {
                    Ok(SampleKind::Between { prev_t: *prev_t, prev_s: prev_s.as_slice(), curr_t: *curr_t, curr_s: curr_s.as_slice() })
                }
            }
            _ => {
                let earliest_ns = sys.history.prev.as_ref().map(|(t, _)| *t).unwrap_or(*curr_t);
                Err(HeteroScheduleError::OutOfRange { system: id.to_string(), t_tai_ns, earliest_ns, latest_ns: *curr_t })
            }
        }
    }

    /// The state of system `id` at `t_tai_ns` -- identical contract to [`Scheduler::sample`]
    /// (exact at a native step time, Hermite-with-velocity interpolated between the two most
    /// recent ones, never extrapolated). Built on [`HeteroScheduler::sample_kind`].
    pub fn sample(&self, id: &str, t_tai_ns: i64) -> Result<Vec<f64>, HeteroScheduleError> {
        match self.sample_kind(id, t_tai_ns)? {
            SampleKind::Native(s) => Ok(s.to_vec()),
            SampleKind::Between { prev_t, prev_s, curr_t, curr_s } => Ok(crate::interpolate::hermite_velocity(prev_t, prev_s, curr_t, curr_s, t_tai_ns)),
        }
    }

    /// `state_dim()` of the system registered under `id`, if any -- the one signal
    /// [`HeteroKernel::run_with_ports`] uses to tell a physical system (`>= 6` components,
    /// Hermite-interpolable) from a `BINDING_KIND_CONTAINER` instance (`0`, nothing to
    /// interpolate at all -- [`HeteroScheduler::sample_held`] is the right way to sample one of
    /// those instead of [`HeteroScheduler::sample`]).
    pub fn state_dim(&self, id: &str) -> Option<usize> {
        self.systems.get(id).map(|sys| sys.model.state_dim())
    }

    /// Zero-order-hold sample for a system with no physical state to interpolate
    /// (`state_dim() == 0`) -- see [`HoldKind`]'s own doc comment for exactly what this means
    /// and why it cannot reuse [`HeteroScheduler::sample`]/[`SampleKind::Between`] (calling
    /// [`crate::interpolate::hermite_velocity`] on a 0-length pair panics: it requires at least
    /// six components). Unlike `sample`/`sample_kind`, this never interpolates and never
    /// requires a `prev` to exist: `t_tai_ns` need only be at or after this system's own most
    /// recently recorded native step (`history.curr`, which starts at its own registration seed)
    /// -- exactly the situation [`crate::schedule::HeteroScheduler::advance_to_with_ports`]
    /// leaves a coarser-period system in in between its own native steps, since that method
    /// (unlike plain [`HeteroScheduler::advance_to`]) deliberately never steps a system ahead of
    /// where it is actually due (question 110's port-delivery ordering requires exactly this
    /// lockstep honesty -- see that method's own doc comment). Still refuses a query strictly
    /// *before* `history.curr` (this can only happen if a caller queries time out of the
    /// monotonically increasing order [`HeteroKernel::run_with_ports`] always uses) -- never
    /// extrapolates backward, only holds forward.
    pub fn sample_held(&self, id: &str, t_tai_ns: i64) -> Result<(HoldKind, &[f64]), HeteroScheduleError> {
        let sys = self.systems.get(id).ok_or_else(|| HeteroScheduleError::UnknownSystem(id.to_string()))?;
        let (curr_t, curr_s) = &sys.history.curr;
        if t_tai_ns < *curr_t {
            return Err(HeteroScheduleError::OutOfRange { system: id.to_string(), t_tai_ns, earliest_ns: *curr_t, latest_ns: *curr_t });
        }
        if t_tai_ns == *curr_t {
            Ok((HoldKind::Fresh, curr_s.as_slice()))
        } else {
            Ok((HoldKind::Held, curr_s.as_slice()))
        }
    }

    /// The [`av_cdm::pb::ModelInfo`] of the system registered under `id`, if any.
    pub fn describe(&self, id: &str) -> Option<av_cdm::pb::ModelInfo> {
        self.systems.get(id).map(|sys| sys.model.describe())
    }

    /// The step period a system was registered with, if any.
    pub fn period_ns(&self, id: &str) -> Option<i64> {
        self.systems.get(id).map(|sys| sys.period_ns)
    }

    /// Registered system ids, in the deterministic (sorted) order every other method visits
    /// them in.
    pub fn system_ids(&self) -> impl Iterator<Item = &str> {
        self.systems.keys().map(String::as_str)
    }

    /// ADR-005 sec 2's base-period clock, computed from every currently registered instance's
    /// period and `output_period_ns`, with the integer-multiple check run against it
    /// immediately (`crate::clock::base_period_ns` then `crate::clock::check_integer_multiples`
    /// -- see both functions' own doc comments). A load-time gate a caller runs once before
    /// driving `advance_to`; always succeeds when it succeeds at all (a GCD-derived base period
    /// can never itself fail the check), so a failure here is always
    /// [`crate::clock::BasePeriodError::NonPositivePeriod`] from a bad registered period, never
    /// `NotAnIntegerMultiple` -- see `crate::clock`'s own tests for a case that exercises
    /// `NotAnIntegerMultiple` directly against a base period chosen some other way.
    pub fn base_period_ns(&self, output_period_ns: i64) -> Result<i64, crate::clock::BasePeriodError> {
        let periods: BTreeMap<String, i64> = self.systems.iter().map(|(id, sys)| (id.clone(), sys.period_ns)).collect();
        let base = crate::clock::base_period_ns(output_period_ns, &periods)?;
        crate::clock::check_integer_multiples(base, output_period_ns, &periods)?;
        Ok(base)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use av_cdm::pb::ModelInfo;

    /// Constant-acceleration toy model (closed-form solution), for exercising the scheduler
    /// without any GMAT dependency.
    #[derive(Clone)]
    struct ConstantAccel {
        a: [f64; 3],
    }
    impl DynamicsModel for ConstantAccel {
        type Error = std::convert::Infallible;
        fn state_dim(&self) -> usize {
            6
        }
        fn derivatives(&self, state: &[f64], _t_tai_ns: i64, _controls: &[f64], out: &mut [f64]) -> Result<(), Self::Error> {
            out[0..3].copy_from_slice(&state[3..6]);
            out[3..6].copy_from_slice(&self.a);
            Ok(())
        }
        fn describe(&self) -> ModelInfo {
            ModelInfo::default()
        }
        // Test-only closed-form model; never emits telemetry.
        fn last_measurements(&self) -> Vec<av_cdm::pb::Measurement> {
            Vec::new()
        }
        // No SENSOR fault runtime.
        fn drain_sensor_fault_effect(&self) -> Option<av_dynamics::SensorFaultEffectDrain> {
            None
        }
        // See `drain_sensor_fault_effect`'s identical reasoning immediately above (question 188, R5.2).
        fn drain_decode_errors(&self) -> Vec<av_dynamics::DecodeErrorOccurrence> {
            Vec::new()
        }
    }

    fn closed_form(x0: &[f64; 6], a: [f64; 3], t_s: f64) -> [f64; 6] {
        [
            x0[0] + x0[3] * t_s + 0.5 * a[0] * t_s * t_s,
            x0[1] + x0[4] * t_s + 0.5 * a[1] * t_s * t_s,
            x0[2] + x0[5] * t_s + 0.5 * a[2] * t_s * t_s,
            x0[3] + a[0] * t_s,
            x0[4] + a[1] * t_s,
            x0[5] + a[2] * t_s,
        ]
    }

    #[test]
    fn two_systems_at_different_rates_are_both_driven_correctly_from_one_clock() {
        let t0: i64 = 1_700_000_000_000_000_000;
        let x0 = [0.0, 0.0, 0.0, 1.0, 2.0, 3.0];

        let mut sched: Scheduler<ConstantAccel> = Scheduler::new();
        // 50 Hz.
        sched.register("fast", 20_000_000, ConstantAccel { a: [0.0, 0.0, -9.8] }, t0, x0.to_vec());
        // 10 Hz.
        sched.register("slow", 100_000_000, ConstantAccel { a: [1.0, 0.0, 0.0] }, t0, x0.to_vec());

        let one_second = t0 + 1_000_000_000;
        sched.advance_to(one_second).unwrap();

        let fast = sched.sample("fast", one_second).unwrap();
        let slow = sched.sample("slow", one_second).unwrap();

        let want_fast = closed_form(&x0, [0.0, 0.0, -9.8], 1.0);
        let want_slow = closed_form(&x0, [1.0, 0.0, 0.0], 1.0);
        for i in 0..6 {
            assert!((fast[i] - want_fast[i]).abs() < 1e-6, "fast[{i}]: {} vs {}", fast[i], want_fast[i]);
            assert!((slow[i] - want_slow[i]).abs() < 1e-6, "slow[{i}]: {} vs {}", slow[i], want_slow[i]);
        }
    }

    #[test]
    fn a_system_stepped_faster_than_the_sample_rate_needs_no_interpolation_at_its_own_boundaries() {
        let t0: i64 = 0;
        let x0 = [0.0, 0.0, 0.0, 1.0, 0.0, 0.0];
        let mut sched: Scheduler<ConstantAccel> = Scheduler::new();
        sched.register("fast", 10_000_000, ConstantAccel { a: [0.0, 0.0, 0.0] }, t0, x0.to_vec());
        sched.advance_to(50_000_000).unwrap();
        // 50_000_000 ns is exactly the 5th native step boundary (5 * 10_000_000).
        let got = sched.sample("fast", 50_000_000).unwrap();
        let want = closed_form(&x0, [0.0, 0.0, 0.0], 0.05);
        for i in 0..6 {
            assert!((got[i] - want[i]).abs() < 1e-9);
        }
    }

    #[test]
    fn sampling_between_native_steps_interpolates() {
        let t0: i64 = 0;
        let x0 = [0.0, 0.0, 0.0, 1.0, 0.0, 0.0];
        let mut sched: Scheduler<ConstantAccel> = Scheduler::new();
        // 10 Hz: native steps land at 0, 100_000_000, 200_000_000, ...
        sched.register("s", 100_000_000, ConstantAccel { a: [0.0, 0.0, 0.0] }, t0, x0.to_vec());
        sched.advance_to(200_000_000).unwrap();
        // 150_000_000 ns is between the native steps at 100_000_000 and 200_000_000.
        let got = sched.sample("s", 150_000_000).unwrap();
        let want = closed_form(&x0, [0.0, 0.0, 0.0], 0.15);
        for i in 0..6 {
            assert!((got[i] - want[i]).abs() < 1e-6, "component {i}: {} vs {}", got[i], want[i]);
        }
    }

    #[test]
    fn sampling_before_the_first_step_or_after_the_last_is_an_error_not_extrapolation() {
        let t0: i64 = 0;
        let x0 = [0.0; 6];
        let mut sched: Scheduler<ConstantAccel> = Scheduler::new();
        sched.register("s", 100_000_000, ConstantAccel { a: [0.0, 0.0, 0.0] }, t0, x0.to_vec());
        sched.advance_to(100_000_000).unwrap();
        assert!(matches!(sched.sample("s", -1), Err(ScheduleError::OutOfRange { .. })));
        assert!(matches!(sched.sample("s", 200_000_000), Err(ScheduleError::OutOfRange { .. })));
    }

    #[test]
    fn unknown_system_is_a_typed_error() {
        let sched: Scheduler<ConstantAccel> = Scheduler::new();
        assert!(matches!(sched.sample("nope", 0), Err(ScheduleError::UnknownSystem(_))));
    }

    /// `sample_kind` (M13.3) reports `Native` exactly at a system's own recorded step times
    /// (both `curr` and, once it exists, `prev`) and `Between` strictly in between -- the same
    /// partition `sample` itself already relied on internally before being refactored onto
    /// this method, now checked directly rather than only inferred from `sample`'s blended
    /// output.
    #[test]
    fn sample_kind_is_native_at_recorded_step_times_and_between_strictly_inside() {
        let t0: i64 = 0;
        let x0 = [0.0, 0.0, 0.0, 1.0, 0.0, 0.0];
        let mut sched: Scheduler<ConstantAccel> = Scheduler::new();
        sched.register("s", 100_000_000, ConstantAccel { a: [0.0; 3] }, t0, x0.to_vec());
        // One native step only: history is now (prev = t0's seed, curr = the 100 ms step).
        sched.advance_to(100_000_000).unwrap();
        assert!(matches!(sched.sample_kind("s", 0).unwrap(), SampleKind::Native(_)), "the recorded prev (t0's own seed) must be Native");
        assert!(matches!(sched.sample_kind("s", 100_000_000).unwrap(), SampleKind::Native(_)), "the recorded curr step must be Native");

        sched.advance_to(200_000_000).unwrap();
        assert!(matches!(sched.sample_kind("s", 200_000_000).unwrap(), SampleKind::Native(_)), "the recorded curr step must be Native");
        match sched.sample_kind("s", 150_000_000).unwrap() {
            SampleKind::Between { prev_t, curr_t, .. } => {
                assert_eq!(prev_t, 100_000_000);
                assert_eq!(curr_t, 200_000_000);
            }
            other => panic!("expected Between strictly inside the window, got {other:?}"),
        }
        // t0 = 0 has since been evicted (only the last two native samples are kept).
        assert!(matches!(sched.sample_kind("s", 0), Err(ScheduleError::OutOfRange { .. })));
        assert!(matches!(sched.sample_kind("s", -1), Err(ScheduleError::OutOfRange { .. })));
    }

    /// M13.3: `advance_to` catches a system up *past* `target_tai_ns` when its own period would
    /// otherwise leave it short (its own doc comment's "steps once, past the target," restored
    /// to match the code -- see that method's own doc comment for the bug this closes). Only
    /// reachable once a system's own period can exceed the caller's sampling rate -- previously
    /// impossible in `run_with_covariance`, which required period == output rate exactly, so no
    /// prior caller of `advance_to` could ever have hit the old, short-stepping behaviour. This
    /// is exactly what makes `SampleKind::Between` (a genuine two-point Hermite bracket)
    /// reachable for an output tick that falls inside a coarser system's own first native
    /// period, with no separate "no bracket yet" case needed.
    #[test]
    fn advance_to_catches_a_coarser_system_up_past_the_target_rather_than_leaving_it_short() {
        let t0: i64 = 0;
        let x0 = [1.0, 2.0, 3.0, 4.0, -1.0, 0.5];
        let mut sched: Scheduler<ConstantAccel> = Scheduler::new();
        // 500 ms period; queried at 200 ms, well short of the first native step at 500 ms.
        sched.register("s", 500_000_000, ConstantAccel { a: [10.0, 0.0, 0.0] }, t0, x0.to_vec());
        sched.advance_to(200_000_000).unwrap();

        // The system was caught up all the way to 500 ms (one full period), not left at t0.
        match sched.sample_kind("s", 500_000_000).unwrap() {
            SampleKind::Native(_) => {}
            other => panic!("expected the system to have already reached its own 500 ms native step, got {other:?}"),
        }
        // 200 ms now resolves as a genuine Hermite bracket between the seed (t0) and that first
        // real native step, not an error and not a cruder one-point prediction.
        match sched.sample_kind("s", 200_000_000).unwrap() {
            SampleKind::Between { prev_t, curr_t, .. } => {
                assert_eq!(prev_t, t0);
                assert_eq!(curr_t, 500_000_000);
            }
            other => panic!("expected Between, got {other:?}"),
        }
        // Still refuses a query strictly before t0 -- `advance_to`'s catch-up never manufactures
        // history earlier than the system's own seed.
        assert!(matches!(sched.sample_kind("s", -1), Err(ScheduleError::OutOfRange { .. })));
    }

    #[test]
    fn system_ids_are_visited_in_sorted_order() {
        let mut sched: Scheduler<ConstantAccel> = Scheduler::new();
        for id in ["zeta", "alpha", "mu"] {
            sched.register(id, 100_000_000, ConstantAccel { a: [0.0; 3] }, 0, vec![0.0; 6]);
        }
        let ids: Vec<&str> = sched.system_ids().collect();
        assert_eq!(ids, vec!["alpha", "mu", "zeta"]);
    }

    // =========================================================================================
    // HeteroScheduler (ADR-005 sec 1)
    // =========================================================================================

    /// A *second*, unrelated `DynamicsModel` kind (own `Error` type, own physics) --
    /// registering one of these next to a `ConstantAccel` in the same `HeteroScheduler` is
    /// exactly the "one scheduler, several model kinds" case `Scheduler<M>` cannot express.
    struct Rotator {
        w: f64,
    }
    impl DynamicsModel for Rotator {
        type Error = std::convert::Infallible;
        fn state_dim(&self) -> usize {
            6
        }
        fn derivatives(&self, state: &[f64], _t: i64, _c: &[f64], out: &mut [f64]) -> Result<(), Self::Error> {
            // A planar rotation in the position/velocity-shaped first two axes, everything
            // else held at zero -- deliberately different dynamics from ConstantAccel, so a
            // test mixing the two can tell them apart by their closed-form solutions.
            out[0] = self.w * state[1];
            out[1] = -self.w * state[0];
            out[2..6].copy_from_slice(&[0.0; 4]);
            Ok(())
        }
        fn describe(&self) -> av_cdm::pb::ModelInfo {
            av_cdm::pb::ModelInfo { id: "test.rotator".to_string(), ..Default::default() }
        }
        // Test-only closed-form rotation model; never emits telemetry.
        fn last_measurements(&self) -> Vec<av_cdm::pb::Measurement> {
            Vec::new()
        }
        // No SENSOR fault runtime.
        fn drain_sensor_fault_effect(&self) -> Option<av_dynamics::SensorFaultEffectDrain> {
            None
        }
        // See `drain_sensor_fault_effect`'s identical reasoning immediately above (question 188, R5.2).
        fn drain_decode_errors(&self) -> Vec<av_dynamics::DecodeErrorOccurrence> {
            Vec::new()
        }
    }

    fn erased_constant_accel(model_id: &str, a: [f64; 3]) -> BoxedModel {
        av_dynamics::erase_with_id(model_id, ConstantAccel { a }, |_model_id, never| match never {})
    }
    fn erased_rotator(model_id: &str, w: f64) -> BoxedModel {
        av_dynamics::erase_with_id(model_id, Rotator { w }, |_model_id, never| match never {})
    }

    #[test]
    fn hetero_scheduler_drives_two_different_model_kinds_from_one_clock() {
        let t0: i64 = 1_700_000_000_000_000_000;
        let x0 = vec![0.0, 0.0, 0.0, 1.0, 2.0, 3.0];

        let mut sched = HeteroScheduler::new();
        sched.register("accel", 20_000_000, erased_constant_accel("test.accel", [0.0, 0.0, -9.8]), t0, x0.clone());
        sched.register("rotor", 100_000_000, erased_rotator("test.rotator", 0.5), t0, vec![1.0, 0.0, 0.0, 0.0, 0.0, 0.0]);

        let one_second = t0 + 1_000_000_000;
        sched.advance_to(one_second).unwrap();

        let accel = sched.sample("accel", one_second).unwrap();
        let want_accel = closed_form(&[0.0, 0.0, 0.0, 1.0, 2.0, 3.0], [0.0, 0.0, -9.8], 1.0);
        for i in 0..6 {
            assert!((accel[i] - want_accel[i]).abs() < 1e-6, "accel[{i}]: {} vs {}", accel[i], want_accel[i]);
        }

        let rotor = sched.sample("rotor", one_second).unwrap();
        let (c, s) = (0.5_f64.cos(), 0.5_f64.sin());
        assert!((rotor[0] - c).abs() < 1e-6, "rotor x: {}", rotor[0]);
        assert!((rotor[1] - (-s)).abs() < 1e-6, "rotor y: {}", rotor[1]);
    }

    #[test]
    fn hetero_scheduler_system_ids_are_visited_in_sorted_order_regardless_of_registration_order() {
        let mut sched = HeteroScheduler::new();
        sched.register("zeta", 100_000_000, erased_rotator("test.rotator", 0.1), 0, vec![0.0; 6]);
        sched.register("alpha", 100_000_000, erased_constant_accel("test.accel", [0.0; 3]), 0, vec![0.0; 6]);
        sched.register("mu", 100_000_000, erased_constant_accel("test.accel", [0.0; 3]), 0, vec![0.0; 6]);
        let ids: Vec<&str> = sched.system_ids().collect();
        assert_eq!(ids, vec!["alpha", "mu", "zeta"], "BTreeMap iteration must be sorted by instance name, never insertion order");
    }

    #[test]
    fn hetero_scheduler_errors_carry_model_error_naming_the_model_id() {
        let sched = HeteroScheduler::new();
        assert!(matches!(sched.sample("nope", 0), Err(HeteroScheduleError::UnknownSystem(id)) if id == "nope"));
    }

    #[test]
    fn hetero_scheduler_sample_kind_is_native_at_recorded_step_times_and_between_strictly_inside() {
        let mut sched = HeteroScheduler::new();
        sched.register("s", 100_000_000, erased_constant_accel("test.accel", [0.0; 3]), 0, vec![0.0; 6]);
        sched.advance_to(100_000_000).unwrap();
        assert!(matches!(sched.sample_kind("s", 0).unwrap(), SampleKind::Native(_)));
        assert!(matches!(sched.sample_kind("s", 100_000_000).unwrap(), SampleKind::Native(_)));

        sched.advance_to(200_000_000).unwrap();
        assert!(matches!(sched.sample_kind("s", 200_000_000).unwrap(), SampleKind::Native(_)));
        assert!(matches!(sched.sample_kind("s", 150_000_000).unwrap(), SampleKind::Between { .. }));
    }

    /// `HeteroScheduler` counterpart of `advance_to_catches_a_coarser_system_up_past_the_
    /// target_rather_than_leaving_it_short` -- M13.3's `advance_to` fix, over the trait-object
    /// scheduler.
    #[test]
    fn hetero_scheduler_advance_to_catches_a_coarser_system_up_past_the_target() {
        let x0 = vec![1.0, 2.0, 3.0, 4.0, -1.0, 0.5];
        let mut sched = HeteroScheduler::new();
        sched.register("s", 500_000_000, erased_constant_accel("test.accel", [10.0, 0.0, 0.0]), 0, x0.clone());
        sched.advance_to(200_000_000).unwrap();

        assert!(matches!(sched.sample_kind("s", 500_000_000).unwrap(), SampleKind::Native(_)), "must already have reached the 500 ms native step");
        match sched.sample_kind("s", 200_000_000).unwrap() {
            SampleKind::Between { prev_t, curr_t, .. } => {
                assert_eq!(prev_t, 0);
                assert_eq!(curr_t, 500_000_000);
            }
            other => panic!("expected Between, got {other:?}"),
        }
    }

    #[test]
    fn hetero_scheduler_out_of_range_sample_is_a_typed_error_not_extrapolation() {
        let mut sched = HeteroScheduler::new();
        sched.register("s", 100_000_000, erased_constant_accel("test.accel", [0.0; 3]), 0, vec![0.0; 6]);
        sched.advance_to(100_000_000).unwrap();
        assert!(matches!(sched.sample("s", -1), Err(HeteroScheduleError::OutOfRange { .. })));
        assert!(matches!(sched.sample("s", 200_000_000), Err(HeteroScheduleError::OutOfRange { .. })));
    }

    /// A zero-dimensional system (`state_dim() == 0`, the shape `binding::ContainerModel`
    /// always has -- nothing to interpolate at all). `advance_to`, not `advance_to_with_ports`,
    /// only because this test drives the system directly through `sample_held`, which does not
    /// depend on which advance method fed `history` -- both leave the same `curr`/`prev` shape.
    struct ZeroDim;
    impl DynamicsModel for ZeroDim {
        type Error = std::convert::Infallible;
        fn state_dim(&self) -> usize {
            0
        }
        fn derivatives(&self, _s: &[f64], _t: i64, _c: &[f64], _o: &mut [f64]) -> Result<(), Self::Error> {
            Ok(())
        }
        fn describe(&self) -> av_cdm::pb::ModelInfo {
            av_cdm::pb::ModelInfo::default()
        }
        // Test-only zero-dimensional model; never emits telemetry.
        fn last_measurements(&self) -> Vec<av_cdm::pb::Measurement> {
            Vec::new()
        }
        // No SENSOR fault runtime.
        fn drain_sensor_fault_effect(&self) -> Option<av_dynamics::SensorFaultEffectDrain> {
            None
        }
        // See `drain_sensor_fault_effect`'s identical reasoning immediately above (question 188, R5.2).
        fn drain_decode_errors(&self) -> Vec<av_dynamics::DecodeErrorOccurrence> {
            Vec::new()
        }
    }

    /// M14.4 (lifting `DrmError::ContainerPeriodExceedsSampleInterval`): [`HeteroScheduler::
    /// sample_held`] is [`HoldKind::Fresh`] exactly at a zero-dim system's own recorded native
    /// step (including its own registration seed, before it has stepped at all) and
    /// [`HoldKind::Held`] at every query strictly past it, still returning that same last value
    /// -- never erroring the way plain `sample`/`sample_kind` would for a query beyond the
    /// latest recorded step, and never touching `crate::interpolate::hermite_velocity` (which
    /// would panic on a 0-length pair). A caller that instead called `sample`/`sample_kind`
    /// here would get `OutOfRange` for every one of the "should be Held" queries below, since
    /// `advance_to` was never even called -- this test would catch a `HeteroKernel::
    /// run_with_ports` that forgot to special-case `state_dim() == 0` and fell through to
    /// `sample` (an immediate panic once a real container's period exceeds the output period, or
    /// an `OutOfRange` error here) just as readily as it would catch a `sample_held` that
    /// mislabels Held as Fresh or vice versa.
    #[test]
    fn hetero_scheduler_sample_held_is_fresh_at_the_seed_and_held_strictly_after_with_no_bracket_needed() {
        let boxed: BoxedModel = av_dynamics::erase_with_id("test.zero_dim", ZeroDim, |_id, never: std::convert::Infallible| match never {});
        let mut sched = HeteroScheduler::new();
        sched.register("c", 300_000_000, boxed, 0, vec![]);

        assert_eq!(sched.state_dim("c"), Some(0));
        // Before any step at all: the seed itself is Fresh at t=0, Held for every later query up
        // to (not including) the first real native step at 300 ms -- no `advance_to` call
        // needed, unlike `sample`/`sample_kind`, which would refuse these as OutOfRange.
        assert_eq!(sched.sample_held("c", 0).unwrap().0, HoldKind::Fresh);
        assert_eq!(sched.sample_held("c", 100_000_000).unwrap().0, HoldKind::Held);
        assert_eq!(sched.sample_held("c", 200_000_000).unwrap().0, HoldKind::Held);
        assert!(sched.sample_held("c", 100_000_000).unwrap().1.is_empty(), "a 0-length state holds an empty slice, not a fabricated one");

        sched.advance_to(300_000_000).unwrap();
        assert_eq!(sched.sample_held("c", 300_000_000).unwrap().0, HoldKind::Fresh, "the real native step landed exactly here");
        assert_eq!(sched.sample_held("c", 400_000_000).unwrap().0, HoldKind::Held, "waiting for the next native step at 600 ms");

        // Never extrapolates backward, only holds forward.
        assert!(matches!(sched.sample_held("c", 200_000_000), Err(HeteroScheduleError::OutOfRange { .. })), "200 ms is now behind the system's own last recorded step (300 ms)");
    }

    #[test]
    fn hetero_scheduler_base_period_ns_computes_the_gcd_and_passes_its_own_check() {
        let mut sched = HeteroScheduler::new();
        sched.register("fast", 20_000_000, erased_constant_accel("test.accel", [0.0; 3]), 0, vec![0.0; 6]); // 50 Hz
        sched.register("slow", 500_000_000, erased_rotator("test.rotator", 0.1), 0, vec![0.0; 6]); // 2 Hz
        let base = sched.base_period_ns(100_000_000).expect("gcd(100_000_000, 20_000_000, 500_000_000) = 20_000_000");
        assert_eq!(base, 20_000_000);
    }

    #[test]
    fn hetero_scheduler_model_error_from_a_failing_step_names_the_offending_model() {
        struct AlwaysFails;
        impl DynamicsModel for AlwaysFails {
            type Error = String;
            fn state_dim(&self) -> usize {
                6
            }
            fn derivatives(&self, _s: &[f64], _t: i64, _c: &[f64], _o: &mut [f64]) -> Result<(), Self::Error> {
                Err("synthetic failure".to_string())
            }
            fn describe(&self) -> av_cdm::pb::ModelInfo {
                av_cdm::pb::ModelInfo::default()
            }
            // Test-only failing model; never emits telemetry.
            fn last_measurements(&self) -> Vec<av_cdm::pb::Measurement> {
                Vec::new()
            }
            // No SENSOR fault runtime.
            fn drain_sensor_fault_effect(&self) -> Option<av_dynamics::SensorFaultEffectDrain> {
                None
            }
            // See `drain_sensor_fault_effect`'s identical reasoning immediately above (question 188, R5.2).
            fn drain_decode_errors(&self) -> Vec<av_dynamics::DecodeErrorOccurrence> {
                Vec::new()
            }
        }
        let boxed: BoxedModel = av_dynamics::erase_with_id("test.always_fails", AlwaysFails, |model_id, detail| ModelError::Numerical { model_id, detail });
        let mut sched = HeteroScheduler::new();
        sched.register("doomed", 100_000_000, boxed, 0, vec![0.0; 6]);
        let err = sched.advance_to(100_000_000).unwrap_err();
        match err {
            HeteroScheduleError::Model(ModelError::Numerical { model_id, .. }) => assert_eq!(model_id, "test.always_fails"),
            other => panic!("expected HeteroScheduleError::Model(ModelError::Numerical), got {other:?}"),
        }
    }
}
