//! `Kernel`: ties the clock authority ([`crate::clock::Clock`]), the multi-rate scheduler
//! ([`crate::schedule::Scheduler`]) and CDM `Trajectory` assembly ([`crate::trajectory`])
//! together into the smallest thing that can run several `DynamicsModel` systems from one
//! clock and emit a CDM v1 `Trajectory` per system, sampled at a declared output rate
//! (ADR-002 default: 10 Hz).

use std::collections::BTreeMap;

use av_cdm::covariance::{check_spd_row_major, nearest_spd_row_major, CovarianceHygieneError, DEFAULT_NEAREST_SPD_FLOOR_RATIO};
use av_cdm::pb::{Trajectory, TrajectorySample};
use av_dynamics::{propagate_covariance, BoxedModel, DynamicsModel, StmAugmented};

use crate::clock::BasePeriodError;
use crate::interpolate::hermite_velocity;
use crate::schedule::{HeteroScheduleError, HeteroScheduler, SampleKind, ScheduleError, Scheduler};
use crate::trajectory::build_trajectory;

// =================================================================================================
// Covariance availability: Option<Vec<f64>> in the Rust product, never a NaN sentinel (M14.3,
// question 111)
// =================================================================================================
//
// ADR-005 sec 3 lifts the requirement that every covariance-requesting system be registered at
// exactly the kernel's own `output_period_ns`: covariance now propagates at each system's own
// step period and is sampled -- never interpolated -- at output epochs (the table's "state
// transition matrix, covariance | never interpolated: available only at the instance's own
// samples"). That leaves two reasons a sample can carry no covariance, beyond "a real, propagated
// one is present": covariance was never requested for this system at all (question 11's original
// `cov`-empty convention), or it *was* requested but this particular output epoch does not land
// on the system's own native step grid (new with ADR-005 sec 3).
//
// **M13.3 represented the second case as an all-`f64::NAN` `cov` of the correct `n*n` length**
// (`covariance_unavailable`/`CovarianceAvailability`, both deleted by this task), so a consumer
// could still tell it apart from "not requested" (empty) by inspecting `cov` alone. That NaN
// sentinel is exactly `docs/open-questions.md` question 111's own defect: it is unambiguous *by
// construction*, but nothing in the type system forces a caller to classify `cov` before doing
// arithmetic with it -- `cov[0] * cov[1]` on a NaN-filled slice type-checks fine and silently
// produces `NaN`, not a compile error or a panic, so a consumer that forgets to check first gets
// a wrong, `NaN`-poisoned answer with no error to notice it by.
//
// **The fix (question 111, decided by the lead): the Rust-side representation this module
// produces and reads is `Option<Vec<f64>>`/`Option<&[f64]>`, never `f64::NAN`.** [`covariance`]
// is the one reader ([`Option::None`] for "no covariance, whichever reason"), and every writer in
// this module ([`Kernel::run_with_covariance`], [`HeteroKernel::run_with_covariance`]) computes an
// `Option<Vec<f64>>` and only converts it to the wire's `cov: repeated double` shape at the last
// possible moment ([`sample_with_cov`]). **The CDM wire form is unchanged, as the decision
// requires**: both "not requested" and "unavailable at this sample" now serialize as the *same*
// empty `cov` -- the distinction between the two `None` reasons is no longer recoverable from
// `cov` alone (a real behavior change from M13.3, disclosed here and in this task's own report),
// but a caller that needs it already has the information from context (whether the system was
// named in the `p0` map passed to `run_with_covariance` at all) without ever inspecting `cov`
// post hoc. This trades away a wire-level distinction this crate itself introduced one task ago,
// in exchange for making the *only* footgun question 111 exists to close -- a NaN a naive caller
// could silently compute with -- structurally impossible: there is no `f64::NAN` anywhere in
// this representation for a forgetful read to consume.

/// The Rust-side view of one `TrajectorySample.cov` slice (question 111): `None` when this
/// sample carries no covariance -- covariance was never requested for this system, or (ADR-005
/// sec 3) this output epoch does not land on the system's own native covariance step grid --
/// `Some` when a real, hygiene-checked covariance (`av_cdm::covariance::check_spd_row_major`,
/// question 80/83) is present. See the module doc comment section above for why this crate makes
/// no attempt to tell the two `None` reasons apart from `cov` alone any more, and for why that is
/// the deliberate fix, not an oversight. Replaces `covariance_state`/`CovarianceAvailability`
/// (deleted): those classified a NaN sentinel this crate no longer ever writes, so there is
/// nothing left to misclassify.
pub fn covariance(sample: &TrajectorySample) -> Option<&[f64]> {
    if sample.cov.is_empty() {
        None
    } else {
        Some(sample.cov.as_slice())
    }
}

/// Build a `TrajectorySample` from a covariance that may or may not be available (question 111):
/// `None` crosses into the wire's existing `cov: repeated double` shape as an empty slice -- the
/// same "no covariance" convention `cov` already carried before M13.3's NaN sentinel ever existed,
/// and the one [`covariance`] (above) already expects on the way back out. **The single
/// `TrajectorySample` write site in this crate** (question 116: every one of `Kernel::run`,
/// `Kernel::run_with_covariance`, `HeteroKernel::run`, `HeteroKernel::run_with_covariance` and
/// `HeteroKernel::run_with_ports` below constructs its samples through this function, never a bare
/// `TrajectorySample { .. }` literal), so there is exactly one place that ever constructs the
/// wire's empty-`cov` convention from an `Option`, and exactly one place that ever writes
/// `TrajectorySample.kind` -- `kind` is the caller's own classification of how `mean` was produced
/// (`av_cdm::pb::SampleKind::Native` on the producing instance's own grid, `Interpolated` between
/// native steps per the declared component class (ADR-005 sec 3), `Held` by zero-order hold),
/// already decided by the caller from `crate::schedule::SampleKind`/`HoldKind` before this
/// function is ever reached -- this function only ever transcribes it onto the wire's `i32`.
fn sample_with_cov(tai_ns: i64, mean: Vec<f64>, cov: Option<Vec<f64>>, kind: av_cdm::pb::SampleKind) -> TrajectorySample {
    TrajectorySample { tai_ns, mean, cov: cov.unwrap_or_default(), kind: kind as i32 }
}

pub struct Kernel<M: DynamicsModel> {
    scheduler: Scheduler<M>,
    output_period_ns: i64,
}

impl<M: DynamicsModel> Kernel<M> {
    /// `output_period_ns` is the rate `run` samples every registered system at, independent
    /// of any system's own step period (ADR-002 default: 10 Hz kernel dynamics, i.e.
    /// `100_000_000` ns).
    pub fn new(output_period_ns: i64) -> Self {
        assert!(output_period_ns > 0, "output period must be positive, got {output_period_ns} ns");
        Self { scheduler: Scheduler::new(), output_period_ns }
    }

    /// Register one system (ADR-002: step rate is declared per system). `t0_tai_ns` must
    /// equal every other registered system's `t0_tai_ns` and the `start_tai_ns` later passed
    /// to [`Kernel::run`] -- the kernel drives every system from the same starting instant;
    /// this isn't checked until `run`.
    pub fn register_system(&mut self, id: impl Into<String>, period_ns: i64, model: M, t0_tai_ns: i64, initial_state: Vec<f64>) {
        self.scheduler.register(id, period_ns, model, t0_tai_ns, initial_state);
    }

    /// Run every registered system from `start_tai_ns` to `end_tai_ns` (inclusive of both
    /// endpoints), sampling each at this kernel's `output_period_ns`, and return one CDM v1
    /// `Trajectory` per system (id-sorted, `BTreeMap`, ADR-002/ADR-004 determinism).
    ///
    /// Question 116: every `TrajectorySample.kind` is `av_cdm::pb::SampleKind::Native` when the
    /// output tick lands exactly on this system's own native step grid, `Interpolated` when it
    /// falls strictly between two native steps (the same Hermite-with-velocity blend `sample`
    /// itself already computes -- see [`sample_with_cov`]'s own doc comment for why every
    /// `TrajectorySample` this crate emits is written through that one function).
    ///
    /// # Panics
    ///
    /// If `end_tai_ns <= start_tai_ns`, or if the horizon is not an exact multiple of
    /// `output_period_ns` (so the last sample lands exactly on `end_tai_ns` rather than
    /// silently stopping short of it or overshooting).
    pub fn run(&mut self, start_tai_ns: i64, end_tai_ns: i64) -> Result<BTreeMap<String, Trajectory>, ScheduleError<M::Error>> {
        assert!(end_tai_ns > start_tai_ns, "end_tai_ns ({end_tai_ns}) must be after start_tai_ns ({start_tai_ns})");
        let horizon_ns = end_tai_ns - start_tai_ns;
        assert!(
            horizon_ns % self.output_period_ns == 0,
            "run horizon ({horizon_ns} ns) is not an exact multiple of the output period ({} ns)",
            self.output_period_ns
        );

        let ids: Vec<String> = self.scheduler.system_ids().map(str::to_string).collect();
        let mut samples: BTreeMap<String, Vec<TrajectorySample>> = ids.iter().map(|id| (id.clone(), Vec::new())).collect();

        let mut t = start_tai_ns;
        loop {
            self.scheduler.advance_to(t)?;
            for id in &ids {
                // Question 116: classify via `sample_kind` (rather than the blended `sample`)
                // so this run can also record `TrajectorySample.kind` -- otherwise numerically
                // identical to what `sample` itself already computed internally.
                let (mean, kind) = match self.scheduler.sample_kind(id, t)? {
                    SampleKind::Native(s) => (s.to_vec(), av_cdm::pb::SampleKind::Native),
                    SampleKind::Between { prev_t, prev_s, curr_t, curr_s } => {
                        (hermite_velocity(prev_t, prev_s, curr_t, curr_s, t), av_cdm::pb::SampleKind::Interpolated)
                    }
                };
                samples.get_mut(id).expect("id came from system_ids()").push(sample_with_cov(t, mean, None, kind));
            }
            if t == end_tai_ns {
                break;
            }
            t += self.output_period_ns;
        }

        let mut out = BTreeMap::new();
        for id in &ids {
            let info = self.scheduler.describe(id).expect("id came from system_ids()");
            let samples_for_id = samples.remove(id).expect("populated above");
            out.insert(id.clone(), build_trajectory(id, &info, samples_for_id, start_tai_ns, end_tai_ns));
        }
        Ok(out)
    }

    /// Every named output series system `id`'s model produced via `StepResult.outputs` during
    /// every `run`/`run_with_covariance` call so far on this `Kernel` (question 95's second
    /// half) -- see `crate::schedule::Scheduler::outputs` for exactly what epochs this covers.
    /// `None` if `id` was never registered.
    pub fn outputs(&self, id: &str) -> Option<crate::schedule::OutputSeries<'_>> {
        self.scheduler.outputs(id)
    }
}

/// Covariance propagation (ADR-002 second amendment): a `Kernel` whose systems were
/// registered as `StmAugmented<M>` (`av_dynamics::StmAugmented::new`, seeded with
/// `StmAugmented::<M>::seed(x0)`) already carries `Phi(t0, t)` in the tail of every native
/// sample -- the *same* single integration that produces the physical trajectory also
/// produces the STM, per `av_dynamics`'s "the kernel integrates it" (never a read-back of
/// GMAT's own `Spacecraft` STM after the fact). This impl block turns that raw augmented
/// output into ordinary `TrajectorySample`s (`mean` truncated back to the physical state,
/// `cov` filled in) -- covariance never leaks into a plain `Kernel<M>::run` caller, and a
/// `Kernel<StmAugmented<M>>` that is only ever run with `run` (not `run_with_covariance`)
/// still reports empty `cov` on every sample (question 11: covariance is always optional and
/// explicitly requested, never a silent default -- `run_with_covariance` is the only place
/// `cov` is ever filled in this crate).
impl<M: DynamicsModel> Kernel<StmAugmented<M>> {
    /// Like [`Kernel::run`], but also fills `TrajectorySample.cov` for every system named in
    /// `p0` (its declared initial covariance, row-major `n x n`, `n` = the wrapped model's
    /// physical `state_dim()`) by computing `P(t) = Phi(t0, t) P0 Phi(t0, t)^T` from the
    /// STM this kernel already integrated (`av_dynamics::propagate_covariance`, which
    /// symmetrizes explicitly and this method logs the pre-symmetrization asymmetry for --
    /// see that function's doc comment). A system not named in `p0` gets an empty `cov`, same
    /// as [`Kernel::run`].
    ///
    /// **Covariance hygiene (`docs/open-questions.md` question 80).** Every propagated `P(t)`
    /// is run through `av_cdm::covariance::check_spd_row_major` -- the same Cholesky-based SPD
    /// bar `spoore_cdm::GaussianState`'s constructor enforces -- **before** it is ever written
    /// into `TrajectorySample.cov`, i.e. before it has any chance to cross into a spoore type
    /// downstream. A failure is a typed `ScheduleError::CovarianceHygiene`, never a silent
    /// repair, *unless* `nearest_spd_projection` is `true`, in which case the failing sample is
    /// replaced by `av_cdm::covariance::nearest_spd_row_major`'s projection (logged loudly,
    /// still counted as a hygiene failure) rather than the run being aborted. Off by default;
    /// see that function's doc comment for the algorithm, its limits, and for why this plain
    /// `bool` -- rather than a caller passing a `DesignReferenceMission` in directly -- is how
    /// `DrmOptions.nearest_spd_projection` (`proto/altavista/v1/system.proto`, a real field as
    /// of the lead's question 82/83 decision) reaches this method: no crate in this repo yet
    /// owns constructing a `DesignReferenceMission` and driving a `Kernel` from it end to end,
    /// so a caller that has read `drm.options.nearest_spd_projection` passes the value straight
    /// through rather than this method reading the proto message itself.
    ///
    /// **M13.3: every registered system's own step period is honoured, whatever it is** --
    /// this no longer requires (nor checks) that it equal this kernel's own `output_period_ns`.
    /// At every output tick, a covariance-requesting system's own [`crate::schedule::
    /// SampleKind`] decides what happens: [`SampleKind::Native`] (the tick lands exactly on
    /// this system's own native step) truncates to `(mean, Phi)` and propagates `P(t)` exactly
    /// as before; [`SampleKind::Between`] (the tick falls strictly between two native steps --
    /// only possible now that a system's period need not equal `output_period_ns`) Hermite-
    /// interpolates *only* the leading `n` physical components (`crate::interpolate::
    /// hermite_velocity` on `prev_s[..n]`/`curr_s[..n]`, never the `Phi` tail) for `mean`, and
    /// never pass-through- or Hermite-interpolates `Phi` at all -- `mean` is the only thing an
    /// off-native-step tick ever gets, `cov` is always [`None`] there (see the module doc comment
    /// above [`covariance`] for the full `Option`-based convention this produces, question 111).
    /// This is exactly ADR-005 sec 3's rule ("state transition matrix, covariance | never
    /// interpolated: available only at the instance's own samples") made real rather than
    /// sidestepped by a panic.
    ///
    /// Because this can no longer simply call [`Kernel::run`] and post-process its output (that
    /// would run every registered system's *physical* state through `Scheduler::sample`, which
    /// interpolates the whole augmented vector -- `Phi` included -- whenever a tick is not a
    /// native step; exactly the bug this task exists to close), this method drives its own
    /// `advance_to`/`sample_kind` loop, structurally identical to `run`'s own
    /// (`advance_to(t)` then visit every system in id-sorted order at every tick from
    /// `start_tai_ns` to `end_tai_ns`) but sampling through `sample_kind` instead of `sample`.
    ///
    /// **Question 116:** the same `crate::schedule::SampleKind::Native`/`Between` classification
    /// this method already computes to decide *whether* to propagate covariance also becomes
    /// `TrajectorySample.kind` on the wire (`av_cdm::pb::SampleKind::Native`/`Interpolated`) --
    /// one classification, two uses, both written through [`sample_with_cov`].
    ///
    /// # Errors
    ///
    /// [`ScheduleError::CovarianceHygiene`] if a propagated covariance fails the SPD hygiene
    /// check above and `nearest_spd_projection` is `false`; otherwise as [`Kernel::run`].
    pub fn run_with_covariance(&mut self, start_tai_ns: i64, end_tai_ns: i64, n: usize, p0: &BTreeMap<String, Vec<f64>>, nearest_spd_projection: bool) -> Result<BTreeMap<String, Trajectory>, ScheduleError<M::Error>> {
        assert!(end_tai_ns > start_tai_ns, "end_tai_ns ({end_tai_ns}) must be after start_tai_ns ({start_tai_ns})");
        let horizon_ns = end_tai_ns - start_tai_ns;
        assert!(
            horizon_ns % self.output_period_ns == 0,
            "run horizon ({horizon_ns} ns) is not an exact multiple of the output period ({} ns)",
            self.output_period_ns
        );

        let ids: Vec<String> = self.scheduler.system_ids().map(str::to_string).collect();
        let mut samples: BTreeMap<String, Vec<TrajectorySample>> = ids.iter().map(|id| (id.clone(), Vec::new())).collect();
        // Tracked per system, across every NATIVE sample this run actually propagates (an
        // off-grid output epoch contributes nothing here -- see the module doc comment above
        // `covariance`), and reported once at the end -- same reasoning as before M13.3 ("the
        // maximum asymmetry actually encountered is the honest summary"), just scoped to samples
        // that actually went through `propagate_covariance`.
        let mut max_asym_over_run = vec![0.0f64; ids.len()];
        let mut min_diag_sq_over_run = vec![f64::INFINITY; ids.len()];
        let mut native_count = vec![0usize; ids.len()];

        let mut t = start_tai_ns;
        loop {
            self.scheduler.advance_to(t)?;
            for (idx, id) in ids.iter().enumerate() {
                let p0_flat = p0.get(id);
                let sample = match self.scheduler.sample_kind(id, t)? {
                    SampleKind::Native(aug) => {
                        assert_eq!(aug.len(), n + n * n, "system {id:?}: expected an StmAugmented n + n^2 sample, got {} components (n = {n})", aug.len());
                        let (mean, phi) = aug.split_at(n);
                        let cov: Option<Vec<f64>> = match p0_flat {
                            Some(p0v) => {
                                let (mut cov, max_asym) = propagate_covariance(phi, p0v, n);
                                max_asym_over_run[idx] = max_asym_over_run[idx].max(max_asym);
                                native_count[idx] += 1;

                                let context = format!("run_with_covariance: system {id:?} at tai_ns {t}");
                                let diag = match check_spd_row_major(&cov, n, &context) {
                                    Ok(diag) => diag,
                                    Err(e) if nearest_spd_projection => {
                                        eprintln!(
                                            "[av-kernel] run_with_covariance: {context} failed the SPD hygiene \
                                             check ({e}); applying the opt-in nearest-SPD projection (declared, \
                                             not a silent repair -- av_cdm::covariance::nearest_spd_projections_applied() now {})",
                                            av_cdm::covariance::nearest_spd_projections_applied() + 1
                                        );
                                        cov = nearest_spd_row_major(&cov, n, DEFAULT_NEAREST_SPD_FLOOR_RATIO);
                                        check_spd_row_major(&cov, n, &context).unwrap_or_else(|e| {
                                            panic!("{context}: nearest_spd_row_major's own output failed the hygiene check it was built to pass: {e}")
                                        })
                                    }
                                    Err(e) => return Err(ScheduleError::CovarianceHygiene(e)),
                                };
                                min_diag_sq_over_run[idx] = min_diag_sq_over_run[idx].min(diag.min_cholesky_diag_sq);
                                Some(cov)
                            }
                            None => None,
                        };
                        sample_with_cov(t, mean.to_vec(), cov, av_cdm::pb::SampleKind::Native)
                    }
                    SampleKind::Between { prev_t, prev_s, curr_t, curr_s } => {
                        assert_eq!(prev_s.len(), n + n * n, "system {id:?}: expected an StmAugmented n + n^2 sample, got {} components (n = {n})", prev_s.len());
                        assert_eq!(curr_s.len(), n + n * n, "system {id:?}: expected an StmAugmented n + n^2 sample, got {} components (n = {n})", curr_s.len());
                        let mean = hermite_velocity(prev_t, &prev_s[..n], curr_t, &curr_s[..n], t);
                        // Never a real covariance here, whether or not this system was even
                        // requested -- see the module doc comment above `covariance` (question
                        // 111): both reasons for "no covariance" now produce the identical
                        // `None`/empty-`cov` result.
                        sample_with_cov(t, mean, None, av_cdm::pb::SampleKind::Interpolated)
                    }
                };
                samples.get_mut(id).expect("id came from system_ids()").push(sample);
            }
            if t == end_tai_ns {
                break;
            }
            t += self.output_period_ns;
        }

        let mut out = BTreeMap::new();
        for (idx, id) in ids.iter().enumerate() {
            if p0.contains_key(id) {
                eprintln!(
                    "[av-kernel] run_with_covariance: system {id:?}, {} native (propagated) covariance sample(s): \
                     max Phi P0 Phi^T asymmetry {:.6e} before symmetrizing (corrected on every sample); \
                     smallest Cholesky-diagonal^2 proxy seen this run: {:.6e} \
                     (an upper bound on the true smallest eigenvalue -- see CholeskyDiagnostics::min_cholesky_diag_sq)",
                    native_count[idx], max_asym_over_run[idx], min_diag_sq_over_run[idx]
                );
            }
            let info = self.scheduler.describe(id).expect("id came from system_ids()");
            let samples_for_id = samples.remove(id).expect("populated above");
            out.insert(id.clone(), build_trajectory(id, &info, samples_for_id, start_tai_ns, end_tai_ns));
        }
        Ok(out)
    }
}

// =================================================================================================
// HeteroKernel: the trait-object, multi-model-kind kernel (ADR-005 sec 1-2)
// =================================================================================================

/// Everything [`HeteroKernel::run`] or [`HeteroKernel::run_with_covariance`] can be refused
/// with. Parallels [`ScheduleError<E>`] the way [`HeteroScheduleError`] parallels
/// [`ScheduleError`] itself: no type parameter, because every trait-object model already
/// speaks the one [`av_dynamics::ModelError`] ([`HeteroScheduleError::Model`] carries it).
#[derive(Debug)]
pub enum HeteroKernelError {
    /// ADR-005 sec 2's base-period gate refused the run before any system was stepped --
    /// [`HeteroKernel::run`] computes `HeteroScheduler::base_period_ns` from this kernel's own
    /// `output_period_ns` and every registered instance's period, and runs
    /// `crate::clock::check_integer_multiples` against it, up front. `Kernel<M>` has no
    /// equivalent gate: ADR-005 sec 2 is new with this type.
    BasePeriod(BasePeriodError),
    /// Advancing or sampling [`HeteroScheduler`] itself failed (a model step error, an unknown
    /// system, an out-of-range sample).
    Schedule(HeteroScheduleError),
    /// [`HeteroKernel::run_with_covariance`] propagated a covariance that failed the
    /// Cholesky-based SPD hygiene check (`av_cdm::covariance`, `docs/open-questions.md`
    /// question 80) and the caller did not opt into the nearest-SPD projection -- mirrors
    /// [`ScheduleError::CovarianceHygiene`] exactly. Never raised by plain [`HeteroKernel::run`].
    CovarianceHygiene(CovarianceHygieneError),
}

impl std::fmt::Display for HeteroKernelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HeteroKernelError::BasePeriod(e) => write!(f, "base-period check failed: {e}"),
            HeteroKernelError::Schedule(e) => write!(f, "{e}"),
            HeteroKernelError::CovarianceHygiene(e) => write!(f, "covariance hygiene check failed: {e}"),
        }
    }
}
impl std::error::Error for HeteroKernelError {}

impl From<HeteroScheduleError> for HeteroKernelError {
    fn from(e: HeteroScheduleError) -> Self {
        HeteroKernelError::Schedule(e)
    }
}

/// ADR-005 sec 1-2's heterogeneous kernel: ties [`HeteroScheduler`] (several systems, **each
/// its own `DynamicsModel` kind**, `BTreeMap<InstanceName, av_dynamics::BoxedModel>`) and CDM
/// `Trajectory` assembly ([`crate::trajectory`]) together exactly the way [`Kernel<M>`] ties
/// [`Scheduler<M>`] to the same assembly step -- exactly the same run loop
/// (`advance_to`/`sample`/`describe`, id-sorted output, Hermite-with-velocity interpolation at
/// this kernel's own `output_period_ns`), minus [`Kernel`]'s single-Rust-type limitation. Every
/// model registered here must already be erased to `av_dynamics::BoxedModel`
/// (`av_dynamics::erase_with_id`, or `crate::registry::ModelRegistry`'s constructors) --
/// `HeteroKernel` itself never converts an error or constructs a model, exactly like
/// [`HeteroScheduler`] never does.
///
/// **M9.1: this is now the only kernel `crate::drm::executor::execute` drives.** Its
/// per-instance run loop (fault re-binding, the STM-augmented covariance path) erases every
/// `crate::drm::binding::AnyModel` it materializes (via `av_dynamics::erase_with_id`, wrapping
/// in `StmAugmented` first for the covariance path -- `crate::drm::executor::erase_any_model`/
/// `erase_any_model_stm`) and registers the result here, never on `Kernel<AnyModel>` -- see
/// `crate::drm::executor`'s own module doc comment ("`HeteroKernel` is now the only kernel..."
/// section) for exactly why and what changed. `Kernel<AnyModel>`/`Kernel<StmAugmented<
/// AnyModel>>`, the two instantiations the executor used before M9.1, are retired; `Kernel<M>`
/// itself is unchanged and still used directly by this module's own unit tests and
/// `tests/golden_acceptance.rs`.
pub struct HeteroKernel {
    scheduler: HeteroScheduler,
    output_period_ns: i64,
}

impl HeteroKernel {
    /// See [`Kernel::new`] -- identical contract, just over [`HeteroScheduler`] instead of
    /// [`Scheduler<M>`].
    pub fn new(output_period_ns: i64) -> Self {
        assert!(output_period_ns > 0, "output period must be positive, got {output_period_ns} ns");
        Self { scheduler: HeteroScheduler::new(), output_period_ns }
    }

    /// Register one system. See [`Kernel::register_system`] -- identical contract, except
    /// `model` is already boxed and erased ([`av_dynamics::BoxedModel`]), so systems of
    /// different concrete `DynamicsModel` types can be registered on the same kernel.
    pub fn register_system(&mut self, id: impl Into<String>, period_ns: i64, model: BoxedModel, t0_tai_ns: i64, initial_state: Vec<f64>) {
        self.scheduler.register(id, period_ns, model, t0_tai_ns, initial_state);
    }

    /// Run every registered system from `start_tai_ns` to `end_tai_ns`, sampling each at this
    /// kernel's `output_period_ns`, and return one CDM v1 `Trajectory` per system (id-sorted).
    /// See [`Kernel::run`] for the exact run-loop contract this mirrors -- **including question
    /// 116's `TrajectorySample.kind`** (`Native`/`Interpolated`, identically derived); the one
    /// addition here is the ADR-005 sec 2 base-period gate, run once before any system is
    /// stepped -- see [`HeteroKernelError::BasePeriod`].
    ///
    /// # Panics
    ///
    /// Identical to [`Kernel::run`]: if `end_tai_ns <= start_tai_ns`, or if the horizon is not
    /// an exact multiple of `output_period_ns`.
    ///
    /// # Errors
    ///
    /// [`HeteroKernelError::BasePeriod`] if any registered instance's period is not an integer
    /// multiple of the GCD-derived base period (named, per ADR-005 sec 2); otherwise
    /// [`HeteroKernelError::Schedule`] from advancing or sampling the scheduler.
    pub fn run(&mut self, start_tai_ns: i64, end_tai_ns: i64) -> Result<BTreeMap<String, Trajectory>, HeteroKernelError> {
        assert!(end_tai_ns > start_tai_ns, "end_tai_ns ({end_tai_ns}) must be after start_tai_ns ({start_tai_ns})");
        let horizon_ns = end_tai_ns - start_tai_ns;
        assert!(
            horizon_ns % self.output_period_ns == 0,
            "run horizon ({horizon_ns} ns) is not an exact multiple of the output period ({} ns)",
            self.output_period_ns
        );

        self.scheduler.base_period_ns(self.output_period_ns).map_err(HeteroKernelError::BasePeriod)?;

        let ids: Vec<String> = self.scheduler.system_ids().map(str::to_string).collect();
        let mut samples: BTreeMap<String, Vec<TrajectorySample>> = ids.iter().map(|id| (id.clone(), Vec::new())).collect();

        let mut t = start_tai_ns;
        loop {
            self.scheduler.advance_to(t)?;
            for id in &ids {
                // Question 116: see `Kernel::run`'s identical `sample_kind` classification.
                let (mean, kind) = match self.scheduler.sample_kind(id, t)? {
                    SampleKind::Native(s) => (s.to_vec(), av_cdm::pb::SampleKind::Native),
                    SampleKind::Between { prev_t, prev_s, curr_t, curr_s } => {
                        (hermite_velocity(prev_t, prev_s, curr_t, curr_s, t), av_cdm::pb::SampleKind::Interpolated)
                    }
                };
                samples.get_mut(id).expect("id came from system_ids()").push(sample_with_cov(t, mean, None, kind));
            }
            if t == end_tai_ns {
                break;
            }
            t += self.output_period_ns;
        }

        let mut out = BTreeMap::new();
        for id in &ids {
            let info = self.scheduler.describe(id).expect("id came from system_ids()");
            let samples_for_id = samples.remove(id).expect("populated above");
            out.insert(id.clone(), build_trajectory(id, &info, samples_for_id, start_tai_ns, end_tai_ns));
        }
        Ok(out)
    }

    /// Covariance propagation over a heterogeneous run -- the [`HeteroKernel`] counterpart of
    /// [`Kernel::run_with_covariance`]. The difference that method's `n: usize` parameter does
    /// not have to face: [`Kernel<StmAugmented<M>>`] is monomorphized over one `M`, so every
    /// registered system shares one physical `state_dim()` the type system already knows.
    /// `HeteroKernel` has no such single type to ask, so **`physical_dims` must name every
    /// registered system's own physical `state_dim()` explicitly** (the dimension *before*
    /// STM augmentation) -- a system missing from `physical_dims` is a caller bug (asserted,
    /// not a typed error, matching how [`Kernel::run_with_covariance`] asserts its own
    /// `period_ns` precondition). Exactly like that method: every system driven through this
    /// one must already be registered as an STM-augmented model (erase a
    /// `av_dynamics::StmAugmented::new(model)` with `av_dynamics::erase_with_id`, seeded with
    /// `StmAugmented::<M>::seed`) -- its own native sample is `[state; vec(Phi)]`, length `n +
    /// n^2`, checked per sample below; a system named in `physical_dims` but not in `p0` still
    /// gets its mean truncated back to the physical state and an empty `cov`, same as
    /// [`Kernel::run_with_covariance`]'s "unrequested" systems.
    ///
    /// See [`Kernel::run_with_covariance`]'s doc comment for the SPD hygiene check, the opt-in
    /// nearest-SPD projection, and the asymmetry/Cholesky-diagnostic logging this mirrors
    /// verbatim (same `av_cdm::covariance` functions, same reporting, just looped per system
    /// with that system's own `n` rather than one shared `n`).
    ///
    /// **M13.3: every registered system's own step period is honoured, whatever it is** -- see
    /// [`Kernel::run_with_covariance`]'s own doc comment (this mirrors it exactly, over
    /// [`HeteroScheduler`] and per-system `n` rather than one shared `n`): no requirement that
    /// it equal this kernel's own `output_period_ns` any more, `crate::schedule::SampleKind`
    /// decides `Native` (exact native step: truncate and propagate `P(t)`) vs. `Between`
    /// (Hermite-interpolate only the leading `n` physical components for `mean`, `cov` is always
    /// [`None`] there, never touching `Phi` -- question 111, see the module doc comment above
    /// [`covariance`]) at every output tick, and this drives its own `advance_to`/`sample_kind`
    /// loop rather than
    /// post-processing [`HeteroKernel::run`]'s own output (which interpolates whole augmented
    /// vectors through `Scheduler::sample` -- exactly the bug this task exists to close).
    ///
    /// # Panics
    ///
    /// If any registered system has no entry in `physical_dims`; if a system's native sample
    /// length does not equal `physical_dims[id] + physical_dims[id]^2`.
    ///
    /// # Errors
    ///
    /// [`HeteroKernelError::CovarianceHygiene`] if a propagated covariance fails the SPD
    /// hygiene check and `nearest_spd_projection` is `false`; otherwise as [`HeteroKernel::run`].
    pub fn run_with_covariance(
        &mut self,
        start_tai_ns: i64,
        end_tai_ns: i64,
        physical_dims: &BTreeMap<String, usize>,
        p0: &BTreeMap<String, Vec<f64>>,
        nearest_spd_projection: bool,
    ) -> Result<BTreeMap<String, Trajectory>, HeteroKernelError> {
        assert!(end_tai_ns > start_tai_ns, "end_tai_ns ({end_tai_ns}) must be after start_tai_ns ({start_tai_ns})");
        let horizon_ns = end_tai_ns - start_tai_ns;
        assert!(
            horizon_ns % self.output_period_ns == 0,
            "run horizon ({horizon_ns} ns) is not an exact multiple of the output period ({} ns)",
            self.output_period_ns
        );

        self.scheduler.base_period_ns(self.output_period_ns).map_err(HeteroKernelError::BasePeriod)?;

        let ids: Vec<String> = self.scheduler.system_ids().map(str::to_string).collect();
        for id in &ids {
            assert!(
                physical_dims.contains_key(id),
                "run_with_covariance: system {id:?} has no entry in physical_dims -- HeteroKernel \
                 has no single M::state_dim() to infer it from, so every system driven through \
                 this method must declare its own physical (pre-augmentation) state_dim here"
            );
        }

        let mut samples: BTreeMap<String, Vec<TrajectorySample>> = ids.iter().map(|id| (id.clone(), Vec::new())).collect();
        // See Kernel::run_with_covariance's identical fields for why these are tracked across
        // the whole run (scoped to NATIVE, actually-propagated samples) and reported once.
        let mut max_asym_over_run = vec![0.0f64; ids.len()];
        let mut min_diag_sq_over_run = vec![f64::INFINITY; ids.len()];
        let mut native_count = vec![0usize; ids.len()];

        let mut t = start_tai_ns;
        loop {
            self.scheduler.advance_to(t)?;
            for (idx, id) in ids.iter().enumerate() {
                let n = *physical_dims.get(id).expect("checked above");
                let p0_flat = p0.get(id);
                let sample = match self.scheduler.sample_kind(id, t)? {
                    SampleKind::Native(aug) => {
                        assert_eq!(aug.len(), n + n * n, "system {id:?}: expected an StmAugmented n + n^2 sample, got {} components (n = {n})", aug.len());
                        let (mean, phi) = aug.split_at(n);
                        let cov: Option<Vec<f64>> = match p0_flat {
                            Some(p0v) => {
                                let (mut cov, max_asym) = propagate_covariance(phi, p0v, n);
                                max_asym_over_run[idx] = max_asym_over_run[idx].max(max_asym);
                                native_count[idx] += 1;

                                let context = format!("HeteroKernel::run_with_covariance: system {id:?} at tai_ns {t}");
                                let diag = match check_spd_row_major(&cov, n, &context) {
                                    Ok(diag) => diag,
                                    Err(e) if nearest_spd_projection => {
                                        eprintln!(
                                            "[av-kernel] run_with_covariance: {context} failed the SPD hygiene \
                                             check ({e}); applying the opt-in nearest-SPD projection (declared, \
                                             not a silent repair -- av_cdm::covariance::nearest_spd_projections_applied() now {})",
                                            av_cdm::covariance::nearest_spd_projections_applied() + 1
                                        );
                                        cov = nearest_spd_row_major(&cov, n, DEFAULT_NEAREST_SPD_FLOOR_RATIO);
                                        check_spd_row_major(&cov, n, &context).unwrap_or_else(|e| {
                                            panic!("{context}: nearest_spd_row_major's own output failed the hygiene check it was built to pass: {e}")
                                        })
                                    }
                                    Err(e) => return Err(HeteroKernelError::CovarianceHygiene(e)),
                                };
                                min_diag_sq_over_run[idx] = min_diag_sq_over_run[idx].min(diag.min_cholesky_diag_sq);
                                Some(cov)
                            }
                            None => None,
                        };
                        sample_with_cov(t, mean.to_vec(), cov, av_cdm::pb::SampleKind::Native)
                    }
                    SampleKind::Between { prev_t, prev_s, curr_t, curr_s } => {
                        assert_eq!(prev_s.len(), n + n * n, "system {id:?}: expected an StmAugmented n + n^2 sample, got {} components (n = {n})", prev_s.len());
                        assert_eq!(curr_s.len(), n + n * n, "system {id:?}: expected an StmAugmented n + n^2 sample, got {} components (n = {n})", curr_s.len());
                        let mean = hermite_velocity(prev_t, &prev_s[..n], curr_t, &curr_s[..n], t);
                        // Never a real covariance here, whether or not this system was even
                        // requested -- see the module doc comment above `covariance` (question
                        // 111).
                        sample_with_cov(t, mean, None, av_cdm::pb::SampleKind::Interpolated)
                    }
                };
                samples.get_mut(id).expect("id came from system_ids()").push(sample);
            }
            if t == end_tai_ns {
                break;
            }
            t += self.output_period_ns;
        }

        let mut out = BTreeMap::new();
        for (idx, id) in ids.iter().enumerate() {
            if p0.contains_key(id) {
                eprintln!(
                    "[av-kernel] run_with_covariance: system {id:?}, {} native (propagated) covariance sample(s): \
                     max Phi P0 Phi^T asymmetry {:.6e} before symmetrizing (corrected on every sample); \
                     smallest Cholesky-diagonal^2 proxy seen this run: {:.6e} \
                     (an upper bound on the true smallest eigenvalue -- see CholeskyDiagnostics::min_cholesky_diag_sq)",
                    native_count[idx], max_asym_over_run[idx], min_diag_sq_over_run[idx]
                );
            }
            let info = self.scheduler.describe(id).expect("id came from system_ids()");
            let samples_for_id = samples.remove(id).expect("populated above");
            out.insert(id.clone(), build_trajectory(id, &info, samples_for_id, start_tai_ns, end_tai_ns));
        }
        Ok(out)
    }

    /// See [`Kernel::outputs`] -- identical contract, over [`HeteroScheduler`].
    pub fn outputs(&self, id: &str) -> Option<crate::schedule::OutputSeries<'_>> {
        self.scheduler.outputs(id)
    }

    /// See [`HeteroScheduler::applied_commands`] -- identical contract (`docs/open-questions.md`
    /// question 130). Only ever non-empty for a system driven through
    /// [`HeteroKernel::run_with_ports`] (plain [`HeteroKernel::run`] never calls
    /// `step_with_ports` at all).
    pub fn applied_commands(&self, id: &str) -> Option<&[crate::ports::AppliedPortCommand]> {
        self.scheduler.applied_commands(id)
    }

    /// See [`HeteroScheduler::measurements`] -- identical contract (`docs/open-questions.md`
    /// question 173, M25.3). Only ever non-empty for a system driven through
    /// [`HeteroKernel::run_with_ports`] whose own model overrides `last_measurements`.
    pub fn measurements(&self, id: &str) -> Option<&[av_cdm::pb::Measurement]> {
        self.scheduler.measurements(id)
    }

    /// Like [`HeteroKernel::run`], but drives [`HeteroScheduler::advance_to_with_ports`]
    /// instead of `advance_to` at every output tick, so any registered system that overrides
    /// `av_dynamics::DynamicsModel::step_with_ports` exchanges port messages with the rest of
    /// this kernel's own systems through `router` (`docs/open-questions.md` question 108: "the
    /// router implements `SosConfiguration.connections` with deterministic delivery order ...
    /// at the receiver's next step"). A system that never overrides `step_with_ports` (every
    /// `MODEL` binding this workspace has today) behaves identically whether it is run through
    /// this method or plain [`HeteroKernel::run`] -- seeing an always-empty `Inbox` and
    /// contributing nothing to `router` either way -- which is exactly what makes this an
    /// additive method rather than a change to `run` itself: no existing caller of `run` needs
    /// to change, or even know this method exists.
    ///
    /// **M14.4: a zero-dimensional system (`state_dim() == 0`, a `BINDING_KIND_CONTAINER`
    /// instance) is sampled by zero-order hold, never Hermite interpolation.** Lifts `crate::
    /// drm::executor`'s old `DrmError::ContainerPeriodExceedsSampleInterval` restriction, which
    /// existed only because this method used to call plain [`HeteroScheduler::sample`]
    /// unconditionally -- correct for a physical (>= 6 component) system, but `crate::
    /// interpolate::hermite_velocity` panics on a 0-length pair, and a container instance whose
    /// own period exceeds `output_period_ns` reaches exactly that pair the moment an output tick
    /// falls strictly between two of its own native steps (`crate::schedule::SampleKind::
    /// Between`). Every system this method samples is now routed by `HeteroScheduler::
    /// state_dim`: `Some(0)` goes through [`HeteroScheduler::sample_held`] instead (ADR-005 sec
    /// 3's "discrete modes, counters | zero-order hold" rule -- ["outputs and named outputs are
    /// the last delivered values"], `crate::drm::executor`'s own module doc comment); every
    /// other system is unaffected, still classified via `sample_kind` exactly like
    /// [`HeteroKernel::run`].
    ///
    /// **M15.2 (question 116): the held vs. fresh distinction rides the wire directly, on
    /// `TrajectorySample.kind` itself.** `TrajectorySample.mean` carries no information either
    /// way for a 0-length state (empty is empty, correctly, regardless of which produced it), so
    /// M14.4 originally recorded every held epoch out of band, in `HeteroKernel::held_epochs`
    /// (deleted by this task) and from there into `Trajectory.provenance.
    /// attributes["held_sample_tai_ns"]` (`crate::drm::executor`, also deleted). The lead's
    /// question 116 decision replaces that indirection with a real field: `av_cdm::pb::
    /// SampleKind::Held` on the sample itself (`crate::schedule::HoldKind::Fresh` maps to
    /// `SampleKind::Native` -- exactly on this system's own grid -- and `HoldKind::Held` maps to
    /// `SampleKind::Held`), so a consumer reads it straight off the sample it is already looking
    /// at instead of cross-referencing a side channel keyed by TAI ns.
    ///
    /// # Panics
    ///
    /// Identical to [`HeteroKernel::run`].
    ///
    /// # Errors
    ///
    /// Identical to [`HeteroKernel::run`].
    pub fn run_with_ports(&mut self, start_tai_ns: i64, end_tai_ns: i64, router: &mut crate::router::Router) -> Result<BTreeMap<String, Trajectory>, HeteroKernelError> {
        assert!(end_tai_ns > start_tai_ns, "end_tai_ns ({end_tai_ns}) must be after start_tai_ns ({start_tai_ns})");
        let horizon_ns = end_tai_ns - start_tai_ns;
        assert!(
            horizon_ns % self.output_period_ns == 0,
            "run horizon ({horizon_ns} ns) is not an exact multiple of the output period ({} ns)",
            self.output_period_ns
        );

        self.scheduler.base_period_ns(self.output_period_ns).map_err(HeteroKernelError::BasePeriod)?;

        let ids: Vec<String> = self.scheduler.system_ids().map(str::to_string).collect();
        let mut samples: BTreeMap<String, Vec<TrajectorySample>> = ids.iter().map(|id| (id.clone(), Vec::new())).collect();

        let mut t = start_tai_ns;
        loop {
            self.scheduler.advance_to_with_ports(t, router)?;
            for id in &ids {
                let (mean, kind) = if self.scheduler.state_dim(id) == Some(0) {
                    let (hold_kind, s) = self.scheduler.sample_held(id, t)?;
                    let kind = match hold_kind {
                        crate::schedule::HoldKind::Fresh => av_cdm::pb::SampleKind::Native,
                        crate::schedule::HoldKind::Held => av_cdm::pb::SampleKind::Held,
                    };
                    (s.to_vec(), kind)
                } else {
                    match self.scheduler.sample_kind(id, t)? {
                        SampleKind::Native(s) => (s.to_vec(), av_cdm::pb::SampleKind::Native),
                        SampleKind::Between { prev_t, prev_s, curr_t, curr_s } => {
                            (hermite_velocity(prev_t, prev_s, curr_t, curr_s, t), av_cdm::pb::SampleKind::Interpolated)
                        }
                    }
                };
                samples.get_mut(id).expect("id came from system_ids()").push(sample_with_cov(t, mean, None, kind));
            }
            if t == end_tai_ns {
                break;
            }
            t += self.output_period_ns;
        }

        let mut out = BTreeMap::new();
        for id in &ids {
            let info = self.scheduler.describe(id).expect("id came from system_ids()");
            let samples_for_id = samples.remove(id).expect("populated above");
            out.insert(id.clone(), build_trajectory(id, &info, samples_for_id, start_tai_ns, end_tai_ns));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use av_cdm::pb::{Interpolation, ModelInfo};

    // -- Option<Vec<f64>> covariance (question 111) ------------------------------------------

    /// Direct, minimal proof of the question 111 contract, independent of any full
    /// `run_with_covariance` call: an unavailable sample is `None` in the Rust product and an
    /// empty `cov` on the wire; an available one round-trips through [`covariance`]/
    /// [`sample_with_cov`] unchanged, never touching `f64::NAN`.
    #[test]
    fn covariance_reads_none_for_an_empty_wire_cov_and_round_trips_a_populated_one() {
        let unavailable = sample_with_cov(0, vec![1.0, 2.0], None, av_cdm::pb::SampleKind::Native);
        assert!(unavailable.cov.is_empty(), "None must cross into the wire as an empty cov, never a NaN sentinel");
        assert_eq!(covariance(&unavailable), None, "an empty wire cov must read back as None");

        let available = sample_with_cov(0, vec![1.0, 2.0], Some(vec![9.0, 0.0, 0.0, 4.0]), av_cdm::pb::SampleKind::Native);
        assert_eq!(available.cov, vec![9.0, 0.0, 0.0, 4.0], "Some(cov) must cross into the wire unchanged");
        assert_eq!(covariance(&available), Some(&[9.0, 0.0, 0.0, 4.0][..]), "a populated wire cov must round-trip back out unchanged");

        // A directly-constructed TrajectorySample (as if just deserialized off the wire) with an
        // empty cov must also read back as None -- `covariance` never depends on how the sample
        // was built, only on `cov`'s own shape.
        let from_wire = TrajectorySample { tai_ns: 0, mean: vec![], cov: vec![], kind: av_cdm::pb::SampleKind::Native as i32 };
        assert_eq!(covariance(&from_wire), None);
    }

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
            ModelInfo { id: "test.constant_accel".to_string(), state_space_id: "test.6d".to_string(), frame_id: "test.frame".to_string(), ..Default::default() }
        }
        // Test-only closed-form model; never emits telemetry.
        fn last_measurements(&self) -> Vec<av_cdm::pb::Measurement> {
            Vec::new()
        }
    }

    /// An STM-capable, 6-state constant-acceleration model (M13.3): `x' = v`, `v' = a`
    /// (`a` constant, so the dynamics Jacobian `A = d(x')/dx` is `[[0, I3], [0, 0]]`,
    /// independent of the state and of `a` itself). `A` is nilpotent (`A^2 = 0`), so
    /// `Phi(dt) = exp(A dt) = I + A dt` **exactly** -- `Phi[i][i] = 1` everywhere, `Phi[i][i+3]
    /// = dt` for `i` in `0..3`, zero elsewhere -- an independently-checkable closed form with a
    /// state_dim of 6 (unlike the 2-state `Rotator` above, which is too short for
    /// `crate::interpolate::hermite_velocity`'s own `>= 6` requirement), needed here because
    /// M13.3's "off the native grid" case exercises the Hermite interpolation path on the
    /// physical prefix directly. Also gives an exact closed-form *position* (quadratic in `t`,
    /// degree <= 3), so a Hermite-interpolated `mean` at an off-grid tick can be checked exactly
    /// too, not just the on-grid `Native` samples.
    #[derive(Clone)]
    struct ConstantAccelStm {
        a: [f64; 3],
    }
    impl DynamicsModel for ConstantAccelStm {
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
            ModelInfo { id: "test.constant_accel_stm".to_string(), state_space_id: "test.6d".to_string(), frame_id: "test.frame".to_string(), ..Default::default() }
        }
        fn stm_capable(&self) -> bool {
            true
        }
        fn stm_derivatives(&self, augmented_state: &[f64], t_tai_ns: i64, controls: &[f64], out: &mut [f64]) -> Result<(), Self::Error> {
            let n = 6;
            self.derivatives(&augmented_state[0..n], t_tai_ns, controls, &mut out[0..n])?;
            // d(Phi)/dt = A Phi, A[row][row+3] = 1 for row in 0..3, else 0 -- so (A Phi)[row][col]
            // is Phi[row+3][col] for row in 0..3, and 0 for row in 3..6.
            let phi = &augmented_state[n..n + n * n];
            for row in 0..n {
                for col in 0..n {
                    out[n + row * n + col] = if row < 3 { phi[(row + 3) * n + col] } else { 0.0 };
                }
            }
            Ok(())
        }
        // Test-only closed-form STM model; never emits telemetry.
        fn last_measurements(&self) -> Vec<av_cdm::pb::Measurement> {
            Vec::new()
        }
    }

    /// Closed-form position/velocity for [`ConstantAccelStm`] from `x0` after `t_s` seconds.
    fn constant_accel_stm_closed_form(x0: &[f64; 6], a: [f64; 3], t_s: f64) -> [f64; 6] {
        [
            x0[0] + x0[3] * t_s + 0.5 * a[0] * t_s * t_s,
            x0[1] + x0[4] * t_s + 0.5 * a[1] * t_s * t_s,
            x0[2] + x0[5] * t_s + 0.5 * a[2] * t_s * t_s,
            x0[3] + a[0] * t_s,
            x0[4] + a[1] * t_s,
            x0[5] + a[2] * t_s,
        ]
    }

    /// Closed-form `Phi(dt)` for [`ConstantAccelStm`] (row-major 6x6): `I + dt * A`, exact
    /// (`A` nilpotent) -- see that struct's own doc comment.
    fn constant_accel_stm_closed_form_phi(dt_s: f64) -> [f64; 36] {
        let mut phi = [0.0; 36];
        for i in 0..6 {
            phi[i * 6 + i] = 1.0;
        }
        for i in 0..3 {
            phi[i * 6 + (i + 3)] = dt_s;
        }
        phi
    }

    /// `P(t) = Phi P0 Phi^T` computed directly from [`constant_accel_stm_closed_form_phi`],
    /// independent of `av_dynamics::propagate_covariance` (which the code under test also
    /// calls) -- an honest cross-check, not a tautology.
    fn constant_accel_stm_closed_form_cov(p0: &[f64; 36], dt_s: f64) -> [f64; 36] {
        let phi = constant_accel_stm_closed_form_phi(dt_s);
        let mut tmp = [0.0; 36];
        for i in 0..6 {
            for j in 0..6 {
                let mut s = 0.0;
                for k in 0..6 {
                    s += phi[i * 6 + k] * p0[k * 6 + j];
                }
                tmp[i * 6 + j] = s;
            }
        }
        let mut out = [0.0; 36];
        for i in 0..6 {
            for j in 0..6 {
                let mut s = 0.0;
                for k in 0..6 {
                    s += tmp[i * 6 + k] * phi[j * 6 + k];
                }
                out[i * 6 + j] = s;
            }
        }
        out
    }

    #[test]
    fn run_produces_one_trajectory_per_system_sampled_at_the_output_rate() {
        let t0: i64 = 0;
        let x0 = vec![0.0, 0.0, 0.0, 1.0, 0.0, 0.0];
        let mut kernel: Kernel<ConstantAccel> = Kernel::new(100_000_000); // 10 Hz output
        // A system at 50 Hz (faster than output) and one at 10 Hz (matching output).
        kernel.register_system("fast", 20_000_000, ConstantAccel { a: [0.0, 0.0, -1.0] }, t0, x0.clone());
        kernel.register_system("slow", 100_000_000, ConstantAccel { a: [1.0, 0.0, 0.0] }, t0, x0.clone());

        let end = t0 + 1_000_000_000; // 1 s
        let trajectories = kernel.run(t0, end).unwrap();

        assert_eq!(trajectories.len(), 2);
        let ids: Vec<&String> = trajectories.keys().collect();
        assert_eq!(ids, vec!["fast", "slow"], "BTreeMap output must be id-sorted");

        for (id, traj) in &trajectories {
            assert_eq!(traj.interpolation, Interpolation::HermiteVelocity as i32);
            // 1 s at 10 Hz output = 11 samples (t = 0, 100ms, ..., 1000ms).
            assert_eq!(traj.samples.len(), 11, "system {id}");
            assert_eq!(traj.samples.first().unwrap().tai_ns, t0);
            assert_eq!(traj.samples.last().unwrap().tai_ns, end);
        }

        // Closed-form check on the "slow" system's last sample (a = [1,0,0], v0 = [1,0,0]).
        let last = trajectories["slow"].samples.last().unwrap();
        let want_x = x0[0] + x0[3] * 1.0 + 0.5 * 1.0 * 1.0_f64.powi(2);
        assert!((last.mean[0] - want_x).abs() < 1e-6, "{} vs {want_x}", last.mean[0]);
    }

    /// A model that overrides `step` to populate `StepResult.outputs` directly (question 95's
    /// second half, task M10.2) -- proves `Kernel::outputs` actually carries `StepResult
    /// .outputs` through `advance_to`/`run`, independent of whether `gmat_sys::model
    /// ::GmatModel::step`'s own override (the real producer) is reachable through any
    /// particular erasure path (that boundary is `crate::drm::executor`'s concern, not this
    /// crate's kernel/scheduler plumbing).
    #[derive(Clone)]
    struct OutputtingAccel {
        a: [f64; 3],
    }
    impl DynamicsModel for OutputtingAccel {
        type Error = av_dynamics::ModelError;
        fn state_dim(&self) -> usize {
            6
        }
        fn derivatives(&self, state: &[f64], _t_tai_ns: i64, _controls: &[f64], out: &mut [f64]) -> Result<(), Self::Error> {
            out[0..3].copy_from_slice(&state[3..6]);
            out[3..6].copy_from_slice(&self.a);
            Ok(())
        }
        fn describe(&self) -> ModelInfo {
            ModelInfo { id: "test.outputting_accel".to_string(), state_space_id: "test.6d".to_string(), frame_id: "test.frame".to_string(), ..Default::default() }
        }
        fn step(&self, state: &[f64], t_tai_ns: i64, _controls: &[f64], dt_ns: i64) -> Result<av_dynamics::StepResult, Self::Error> {
            let dt_s = dt_ns as f64 * 1e-9;
            let x0: [f64; 6] = state.try_into().expect("6-state");
            let out = [
                x0[0] + x0[3] * dt_s + 0.5 * self.a[0] * dt_s * dt_s,
                x0[1] + x0[4] * dt_s + 0.5 * self.a[1] * dt_s * dt_s,
                x0[2] + x0[5] * dt_s + 0.5 * self.a[2] * dt_s * dt_s,
                x0[3] + self.a[0] * dt_s,
                x0[4] + self.a[1] * dt_s,
                x0[5] + self.a[2] * dt_s,
            ];
            let rmag = (out[0].powi(2) + out[1].powi(2) + out[2].powi(2)).sqrt();
            let mut outputs = BTreeMap::new();
            outputs.insert("rmag".to_string(), rmag);
            Ok(av_dynamics::StepResult { state: out.to_vec(), t_tai_ns: t_tai_ns + dt_ns, outputs })
        }
        // Test-only closed-form model; never emits telemetry.
        fn last_measurements(&self) -> Vec<av_cdm::pb::Measurement> {
            Vec::new()
        }
    }

    #[test]
    fn kernel_outputs_carries_step_result_outputs_through_run() {
        let t0: i64 = 0;
        let x0 = vec![10.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let mut kernel: Kernel<OutputtingAccel> = Kernel::new(100_000_000); // 10 Hz output
        kernel.register_system("s", 100_000_000, OutputtingAccel { a: [0.0; 3] }, t0, x0.clone());

        assert!(kernel.outputs("s").expect("registered").0.is_empty(), "no output before any run");

        let end = t0 + 300_000_000; // 3 native steps
        let _ = kernel.run(t0, end).unwrap();

        let (epochs, outputs) = kernel.outputs("s").expect("registered");
        assert_eq!(epochs, &[100_000_000, 200_000_000, 300_000_000]);
        let rmag = outputs.get("rmag").expect("rmag populated");
        assert_eq!(rmag.len(), 3);
        // Stationary at x = 10 with zero acceleration and zero velocity: rmag stays 10.0 at
        // every native step.
        for v in rmag {
            assert!((v - 10.0).abs() < 1e-9, "{v}");
        }
        assert!(kernel.outputs("nope").is_none());
    }

    #[test]
    #[should_panic(expected = "not an exact multiple")]
    fn run_rejects_a_horizon_that_does_not_land_on_the_output_rate() {
        let mut kernel: Kernel<ConstantAccel> = Kernel::new(100_000_000);
        kernel.register_system("s", 100_000_000, ConstantAccel { a: [0.0; 3] }, 0, vec![0.0; 6]);
        let _ = kernel.run(0, 150_000_000);
    }

    /// A planar rotation at constant rate `w`: `x' = A x`, `A = [[0, w], [-w, 0]]`. Autonomous
    /// and linear, so its STM has the closed form `Phi(dt) = [[cos(w dt), sin(w dt)],
    /// [-sin(w dt), cos(w dt)]]` -- an independently-checkable exact answer for
    /// `run_with_covariance`, with no GMAT dependency (`av-kernel`'s own library code stays
    /// GMAT-free; this test model lives entirely in this test module).
    #[derive(Clone)]
    struct Rotator {
        w: f64,
    }
    impl DynamicsModel for Rotator {
        type Error = std::convert::Infallible;
        fn state_dim(&self) -> usize {
            2
        }
        fn derivatives(&self, state: &[f64], _t_tai_ns: i64, _controls: &[f64], out: &mut [f64]) -> Result<(), Self::Error> {
            out[0] = self.w * state[1];
            out[1] = -self.w * state[0];
            Ok(())
        }
        fn describe(&self) -> ModelInfo {
            ModelInfo { id: "test.rotator".to_string(), state_space_id: "test.2d".to_string(), frame_id: "test.frame".to_string(), ..Default::default() }
        }
        fn stm_capable(&self) -> bool {
            true
        }
        fn stm_derivatives(&self, augmented_state: &[f64], t_tai_ns: i64, controls: &[f64], out: &mut [f64]) -> Result<(), Self::Error> {
            let n = 2;
            self.derivatives(&augmented_state[0..n], t_tai_ns, controls, &mut out[0..n])?;
            let phi = &augmented_state[n..n + n * n];
            let a = [[0.0, self.w], [-self.w, 0.0]];
            for row in 0..n {
                for col in 0..n {
                    out[n + row * n + col] = (0..n).map(|k| a[row][k] * phi[k * n + col]).sum();
                }
            }
            Ok(())
        }
        // Test-only closed-form rotation model; never emits telemetry.
        fn last_measurements(&self) -> Vec<av_cdm::pb::Measurement> {
            Vec::new()
        }
    }

    #[test]
    fn run_with_covariance_matches_the_closed_form_rotation_and_leaves_unrequested_systems_empty() {
        let t0: i64 = 0;
        let output_period_ns: i64 = 500_000_000; // 0.5 s
        let w = 0.4;
        let x0 = vec![1.0, 0.0];

        let mut kernel: Kernel<StmAugmented<Rotator>> = Kernel::new(output_period_ns);
        // "spun": covariance requested. "unrequested": registered but absent from `p0`.
        kernel.register_system("spun", output_period_ns, StmAugmented::new(Rotator { w }), t0, StmAugmented::<Rotator>::seed(&x0));
        kernel.register_system("unrequested", output_period_ns, StmAugmented::new(Rotator { w }), t0, StmAugmented::<Rotator>::seed(&x0));

        let end = t0 + 2_000_000_000; // 2 s = 4 output periods
        let mut p0_map = BTreeMap::new();
        p0_map.insert("spun".to_string(), vec![9.0, 0.0, 0.0, 4.0]); // diagonal, unequal variances

        let trajectories = kernel.run_with_covariance(t0, end, 2, &p0_map, false).unwrap();

        let spun = &trajectories["spun"];
        assert_eq!(spun.samples.first().unwrap().mean, x0, "mean must be truncated back to the physical state");
        assert_eq!(spun.samples.first().unwrap().cov, vec![9.0, 0.0, 0.0, 4.0], "Phi(t0,t0)=I: P(t0) must equal P0 exactly");

        let last = spun.samples.last().unwrap();
        let dt_s = (end - t0) as f64 * 1e-9;
        let (c, s) = ((w * dt_s).cos(), (w * dt_s).sin());
        let want_mean = [c * x0[0] + s * x0[1], -s * x0[0] + c * x0[1]];
        for (got, want) in last.mean.iter().zip(want_mean.iter()) {
            assert!((got - want).abs() < 1e-6, "{got} vs {want}");
        }
        // A rotation conjugates covariance without changing its eigenvalues: trace (sum of
        // variances) is conserved -- an independent physical check, not just "code ran".
        let trace0 = 9.0 + 4.0;
        let trace1 = last.cov[0] + last.cov[3];
        assert!((trace0 - trace1).abs() < 1e-6, "{trace0} vs {trace1}");
        assert_eq!(last.cov[1], last.cov[2], "propagated covariance must be exactly symmetric");

        // A system never named in p0 gets an empty cov at every sample, same as plain `run`.
        let unrequested = &trajectories["unrequested"];
        assert!(unrequested.samples.iter().all(|s| s.cov.is_empty()), "unrequested system must not get covariance (question 11)");
        assert_eq!(unrequested.samples.len(), spun.samples.len());
    }

    /// M13.3: the equal-rate requirement is lifted. A system's own covariance step (500 ms,
    /// `ConstantAccelStm`) is now *coarser* than the kernel's own output sampling rate (100 ms)
    /// -- exactly the case that used to panic (see this test's own former name/body, replaced
    /// by this one) -- and the run succeeds: every output tick gets a physical `mean` (Hermite-
    /// interpolated off the native grid, exact here since position is quadratic in `t`, degree
    /// <= 3 -- see `constant_accel_stm_closed_form`'s doc comment), but `cov` is only ever
    /// really propagated ([`covariance`] returns `Some`, matching the closed-form
    /// `Phi = I + dt A` exactly) at the four ticks that land on the system's own 500 ms native
    /// grid (0, 500, 1000, 1500 ms); every other tick gets [`None`] (question 111: no longer
    /// distinguishable on the wire from "not requested" -- this test only has one requested
    /// system, "cov", so that collapse introduces no ambiguity here).
    #[test]
    fn run_with_covariance_at_a_coarser_instance_period_propagates_only_on_the_native_grid() {
        let t0: i64 = 0;
        let output_period_ns: i64 = 100_000_000; // 100 ms: finer than the instance's own period
        let period_ns: i64 = 500_000_000; // 500 ms: the instance's own covariance step
        // Nonzero acceleration is fine here (unlike a cruder one-point prediction would need):
        // every off-grid tick below resolves through a genuine two-point Hermite bracket between
        // two REAL, numerically-integrated native samples (M13.3's `advance_to` fix guarantees
        // `advance_to` always catches a coarser system up past its target rather than leaving it
        // short -- see that method's own doc comment), and a cubic Hermite built from exact
        // (position, velocity) endpoints reproduces any degree-<=3 polynomial exactly --
        // `constant_accel_stm_closed_form`'s own quadratic position included (same reasoning
        // `interpolate::tests::exact_for_a_true_cubic_position_function` already establishes for
        // `hermite_velocity` directly).
        let a = [0.0, 0.0, -2.0];
        let x0 = [0.0, 0.0, 0.0, 1.0, 0.5, 0.0];
        let p0 = [
            100.0, 0.0, 0.0, 0.0, 0.0, 0.0, //
            0.0, 100.0, 0.0, 0.0, 0.0, 0.0, //
            0.0, 0.0, 100.0, 0.0, 0.0, 0.0, //
            0.0, 0.0, 0.0, 1.0, 0.0, 0.0, //
            0.0, 0.0, 0.0, 0.0, 1.0, 0.0, //
            0.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        ];

        let mut kernel: Kernel<StmAugmented<ConstantAccelStm>> = Kernel::new(output_period_ns);
        kernel.register_system("cov", period_ns, StmAugmented::new(ConstantAccelStm { a }), t0, StmAugmented::<ConstantAccelStm>::seed(&x0));
        // A system never named in p0 must stay untouched by any of this -- same "not requested"
        // contract as before M13.3.
        kernel.register_system("unrequested", period_ns, StmAugmented::new(ConstantAccelStm { a }), t0, StmAugmented::<ConstantAccelStm>::seed(&x0));

        let end = t0 + 1_500_000_000; // 1.5 s = 3 native periods, 15 output ticks
        let mut p0_map = BTreeMap::new();
        p0_map.insert("cov".to_string(), p0.to_vec());

        let trajectories = kernel.run_with_covariance(t0, end, 6, &p0_map, false).unwrap();
        let traj = &trajectories["cov"];
        assert_eq!(traj.samples.len(), 16, "1.5 s at 100 ms output = 16 samples");

        for s in &traj.samples {
            let t_s = (s.tai_ns - t0) as f64 * 1e-9;
            let want_mean = constant_accel_stm_closed_form(&x0, a, t_s);
            for (got, want) in s.mean.iter().zip(want_mean.iter()) {
                assert!((got - want).abs() < 1e-9, "tai_ns {}: mean {} vs closed-form {}", s.tai_ns, got, want);
            }

            let on_native_grid = (s.tai_ns - t0) % period_ns == 0;
            match covariance(s) {
                Some(cov) => {
                    assert!(on_native_grid, "tai_ns {}: got a real covariance off the 500 ms native grid", s.tai_ns);
                    let want_cov = constant_accel_stm_closed_form_cov(&p0, t_s);
                    for (got, want) in cov.iter().zip(want_cov.iter()) {
                        let scale = want.abs().max(1.0);
                        assert!((got - want).abs() / scale < 1e-6, "tai_ns {}: cov {} vs closed-form {}", s.tai_ns, got, want);
                    }
                }
                None => {
                    // Question 111: an unavailable sample now serializes as an empty `cov`, never
                    // the M13.3 all-NaN sentinel -- assert the wire shape directly rather than
                    // through a NaN check, since there is no NaN left to check for.
                    assert!(!on_native_grid, "tai_ns {}: no covariance but is on the native grid", s.tai_ns);
                    assert!(s.cov.is_empty(), "an unavailable sample's cov must be empty on the wire (question 111), got {} entries", s.cov.len());
                }
            }
        }
        // Every output tick lands on the native grid exactly at t = 0, 500, 1000, 1500 ms.
        let native_ticks = traj.samples.iter().filter(|s| covariance(s).is_some()).count();
        assert_eq!(native_ticks, 4);

        let unrequested = &trajectories["unrequested"];
        assert!(unrequested.samples.iter().all(|s| covariance(s).is_none()), "unrequested system must not get covariance (question 11)");
    }

    /// **M15.2 (question 116), required test:** the identical coarser-covariance-period scenario
    /// as `run_with_covariance_at_a_coarser_instance_period_propagates_only_on_the_native_grid`
    /// above, checked from `TrajectorySample.kind`'s own side rather than `covariance`'s: NATIVE
    /// at exactly the four ticks landing on the system's own 500 ms grid, INTERPOLATED at every
    /// other output tick (question 116's mapping of `crate::schedule::SampleKind::Native`/
    /// `Between` onto `av_cdm::pb::SampleKind`, applied inside `sample_with_cov`'s callers).
    ///
    /// **What this test would fail against:** an implementation that keeps computing the right
    /// `mean`/`cov` (M13.3's own contract, unchanged and separately covered above) but never
    /// threads a classification onto the wire at all -- `kind` would read back
    /// `SampleKind::Unspecified` (0) at every tick, failing every assertion below; or one that
    /// inverts the mapping (a `Between` bracket written as `Native`, a real native step written
    /// as `Interpolated`) -- the four native-grid assertions and the six off-grid assertions
    /// would each fail, landing on exactly the opposite kind.
    #[test]
    fn run_with_covariance_populates_native_at_the_instance_grid_and_interpolated_between() {
        let t0: i64 = 0;
        let output_period_ns: i64 = 100_000_000; // 100 ms
        let period_ns: i64 = 500_000_000; // 500 ms: coarser than the output rate
        let a = [0.0, 0.0, -2.0];
        let x0 = [0.0, 0.0, 0.0, 1.0, 0.5, 0.0];
        let p0 = [
            100.0, 0.0, 0.0, 0.0, 0.0, 0.0, //
            0.0, 100.0, 0.0, 0.0, 0.0, 0.0, //
            0.0, 0.0, 100.0, 0.0, 0.0, 0.0, //
            0.0, 0.0, 0.0, 1.0, 0.0, 0.0, //
            0.0, 0.0, 0.0, 0.0, 1.0, 0.0, //
            0.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        ];

        let mut kernel: Kernel<StmAugmented<ConstantAccelStm>> = Kernel::new(output_period_ns);
        kernel.register_system("cov", period_ns, StmAugmented::new(ConstantAccelStm { a }), t0, StmAugmented::<ConstantAccelStm>::seed(&x0));

        let end = t0 + 1_500_000_000; // 1.5 s = 3 native periods, 16 output ticks
        let mut p0_map = BTreeMap::new();
        p0_map.insert("cov".to_string(), p0.to_vec());

        let trajectories = kernel.run_with_covariance(t0, end, 6, &p0_map, false).unwrap();
        let traj = &trajectories["cov"];
        assert_eq!(traj.samples.len(), 16);

        let mut native_ticks = 0;
        for s in &traj.samples {
            let on_native_grid = (s.tai_ns - t0) % period_ns == 0;
            if on_native_grid {
                native_ticks += 1;
                assert_eq!(s.kind, av_cdm::pb::SampleKind::Native as i32, "tai_ns {} is on the 500 ms native grid, must be NATIVE", s.tai_ns);
            } else {
                assert_eq!(s.kind, av_cdm::pb::SampleKind::Interpolated as i32, "tai_ns {} is strictly between two native steps, must be INTERPOLATED", s.tai_ns);
            }
        }
        assert_eq!(native_ticks, 4, "0, 500, 1000, 1500 ms are the only ticks on the 500 ms native grid over a 1.5 s run");
    }

    // -- Covariance hygiene wiring (docs/open-questions.md question 80) --------------------
    //
    // `w = 0.0` makes Rotator's own closed-form Phi the exact identity at every dt (no
    // rotation), so `P(t) = Phi P0 Phi^T = P0` exactly -- a deliberately-indefinite P0 (here,
    // negative definite) reaches `check_spd_row_major` unchanged, giving a controlled,
    // GMAT-free way to exercise the hygiene-check wiring itself, not just the isolated
    // function `av_cdm::covariance` already tests.

    #[test]
    fn run_with_covariance_returns_a_typed_hygiene_error_for_an_indefinite_covariance_and_counts_it() {
        let t0: i64 = 0;
        let period_ns: i64 = 500_000_000;
        let mut kernel: Kernel<StmAugmented<Rotator>> = Kernel::new(period_ns);
        kernel.register_system("s", period_ns, StmAugmented::new(Rotator { w: 0.0 }), t0, StmAugmented::<Rotator>::seed(&[1.0, 0.0]));
        let mut p0_map = BTreeMap::new();
        p0_map.insert("s".to_string(), vec![-1.0, 0.0, 0.0, -1.0]); // negative definite: eigenvalues -1, -1

        let before = av_cdm::covariance::spd_check_failures();
        let err = kernel.run_with_covariance(t0, t0 + period_ns, 2, &p0_map, false).unwrap_err();
        assert!(
            matches!(err, ScheduleError::CovarianceHygiene(av_cdm::covariance::CovarianceHygieneError::NotPositiveDefinite { .. })),
            "{err}"
        );
        assert!(av_cdm::covariance::spd_check_failures() > before, "the failure must be counted");
    }

    #[test]
    fn run_with_covariance_repairs_an_indefinite_covariance_when_projection_is_opted_into() {
        let t0: i64 = 0;
        let period_ns: i64 = 500_000_000;
        let mut kernel: Kernel<StmAugmented<Rotator>> = Kernel::new(period_ns);
        kernel.register_system("s", period_ns, StmAugmented::new(Rotator { w: 0.0 }), t0, StmAugmented::<Rotator>::seed(&[1.0, 0.0]));
        let mut p0_map = BTreeMap::new();
        p0_map.insert("s".to_string(), vec![-1.0, 0.0, 0.0, -1.0]);

        let trajectories = kernel.run_with_covariance(t0, t0 + period_ns, 2, &p0_map, true).expect("projection is opted into, so the run must succeed rather than error");
        let cov = &trajectories["s"].samples.last().unwrap().cov;
        assert_eq!(cov.len(), 4);
        av_cdm::covariance::check_spd_row_major(cov, 2, "test").expect("the projected covariance must pass the hygiene check it was built to pass");
    }

    // =========================================================================================
    // HeteroKernel (ADR-005 sec 1-2)
    // =========================================================================================

    /// A *second*, unrelated `DynamicsModel` kind, distinct from `ConstantAccel` above --
    /// registering one of these next to a `ConstantAccel` on the same `HeteroKernel` is exactly
    /// the "one kernel, several model kinds" case `Kernel<M>` cannot express. Reused from
    /// `crate::schedule`'s own test module's `Rotator`, minus the STM machinery, plus it here
    /// (this module needs `stm_derivatives` for the covariance tests below, `schedule`'s copy
    /// does not).
    struct HeteroRotator {
        w: f64,
    }
    impl DynamicsModel for HeteroRotator {
        type Error = std::convert::Infallible;
        fn state_dim(&self) -> usize {
            2
        }
        fn derivatives(&self, state: &[f64], _t_tai_ns: i64, _controls: &[f64], out: &mut [f64]) -> Result<(), Self::Error> {
            out[0] = self.w * state[1];
            out[1] = -self.w * state[0];
            Ok(())
        }
        fn describe(&self) -> ModelInfo {
            ModelInfo { id: "test.hetero_rotator".to_string(), state_space_id: "test.2d".to_string(), frame_id: "test.frame".to_string(), ..Default::default() }
        }
        fn stm_capable(&self) -> bool {
            true
        }
        fn stm_derivatives(&self, augmented_state: &[f64], t_tai_ns: i64, controls: &[f64], out: &mut [f64]) -> Result<(), Self::Error> {
            let n = 2;
            self.derivatives(&augmented_state[0..n], t_tai_ns, controls, &mut out[0..n])?;
            let phi = &augmented_state[n..n + n * n];
            let a = [[0.0, self.w], [-self.w, 0.0]];
            for row in 0..n {
                for col in 0..n {
                    out[n + row * n + col] = (0..n).map(|k| a[row][k] * phi[k * n + col]).sum();
                }
            }
            Ok(())
        }
        // Test-only closed-form rotation model; never emits telemetry.
        fn last_measurements(&self) -> Vec<av_cdm::pb::Measurement> {
            Vec::new()
        }
    }

    fn erased(model_id: &str, a: [f64; 3]) -> BoxedModel {
        av_dynamics::erase_with_id(model_id, ConstantAccel { a }, |_model_id, never| match never {})
    }
    fn erased_rotator(model_id: &str, w: f64) -> BoxedModel {
        av_dynamics::erase_with_id(model_id, HeteroRotator { w }, |_model_id, never| match never {})
    }
    fn erased_stm_rotator(model_id: &str, w: f64) -> BoxedModel {
        av_dynamics::erase_with_id(model_id, StmAugmented::new(HeteroRotator { w }), |_model_id, never| match never {})
    }
    fn erased_stm_accel(model_id: &str, a: [f64; 3]) -> BoxedModel {
        av_dynamics::erase_with_id(model_id, StmAugmented::new(ConstantAccelStm { a }), |_model_id, never| match never {})
    }

    #[test]
    fn hetero_kernel_run_produces_one_trajectory_per_system_sampled_at_the_output_rate() {
        let t0: i64 = 0;
        let x0 = vec![0.0, 0.0, 0.0, 1.0, 0.0, 0.0];
        let mut kernel = HeteroKernel::new(100_000_000); // 10 Hz output
        // Two genuinely different DynamicsModel Rust types, registered on the same kernel --
        // exactly what Kernel<M> cannot do.
        kernel.register_system("fast", 20_000_000, erased("test.accel", [0.0, 0.0, -1.0]), t0, x0.clone());
        kernel.register_system("rotor", 100_000_000, erased_rotator("test.rotor", 0.5), t0, vec![1.0, 0.0]);

        let end = t0 + 1_000_000_000; // 1 s
        let trajectories = kernel.run(t0, end).unwrap();

        assert_eq!(trajectories.len(), 2);
        let ids: Vec<&String> = trajectories.keys().collect();
        assert_eq!(ids, vec!["fast", "rotor"], "BTreeMap output must be id-sorted");

        for (id, traj) in &trajectories {
            assert_eq!(traj.interpolation, Interpolation::HermiteVelocity as i32);
            assert_eq!(traj.samples.len(), 11, "system {id}"); // 1 s at 10 Hz = 11 samples
            assert_eq!(traj.samples.first().unwrap().tai_ns, t0);
            assert_eq!(traj.samples.last().unwrap().tai_ns, end);
        }

        let last_rotor = trajectories["rotor"].samples.last().unwrap();
        let (c, s) = (0.5_f64.cos(), 0.5_f64.sin());
        assert!((last_rotor.mean[0] - c).abs() < 1e-6, "{}", last_rotor.mean[0]);
        assert!((last_rotor.mean[1] - (-s)).abs() < 1e-6, "{}", last_rotor.mean[1]);
    }

    /// `HeteroKernel::outputs`'s counterpart to `kernel_outputs_carries_step_result_outputs
    /// _through_run`: boxed directly as `av_dynamics::BoxedModel` (its `Error` is already
    /// `av_dynamics::ModelError`, so no `av_dynamics::erase_with_id`/`ErasedModel` wrapping is
    /// needed) so this test proves `HeteroScheduler`/`HeteroKernel`'s own outputs-carrying
    /// plumbing is correct on its own terms -- independent of `crate::drm::executor`'s own
    /// erasure choice for a real GMAT-bound instance (see that module's doc comment).
    fn boxed_outputting(a: [f64; 3]) -> BoxedModel {
        Box::new(OutputtingAccel { a })
    }

    #[test]
    fn hetero_kernel_outputs_carries_step_result_outputs_through_run() {
        let t0: i64 = 0;
        let x0 = vec![10.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let mut kernel = HeteroKernel::new(100_000_000); // 10 Hz output
        kernel.register_system("s", 100_000_000, boxed_outputting([0.0; 3]), t0, x0.clone());
        // A second, plain system with no output producer -- proves the two coexist cleanly and
        // an outputless system's own entry stays empty rather than erroring.
        kernel.register_system("plain", 100_000_000, erased("test.accel", [0.0; 3]), t0, x0.clone());

        let end = t0 + 200_000_000; // 2 native steps
        let _ = kernel.run(t0, end).unwrap();

        let (epochs, outputs) = kernel.outputs("s").expect("registered");
        assert_eq!(epochs, &[100_000_000, 200_000_000]);
        let rmag = outputs.get("rmag").expect("rmag populated");
        assert_eq!(rmag.len(), 2);
        for v in rmag {
            assert!((v - 10.0).abs() < 1e-9, "{v}");
        }

        let (plain_epochs, plain_outputs) = kernel.outputs("plain").expect("registered");
        assert!(plain_epochs.is_empty(), "erased ConstantAccel never populates StepResult.outputs");
        assert!(plain_outputs.is_empty());

        assert!(kernel.outputs("nope").is_none());
    }

    #[test]
    #[should_panic(expected = "not an exact multiple")]
    fn hetero_kernel_run_rejects_a_horizon_that_does_not_land_on_the_output_rate() {
        let mut kernel = HeteroKernel::new(100_000_000);
        kernel.register_system("s", 100_000_000, erased("test.accel", [0.0; 3]), 0, vec![0.0; 6]);
        let _ = kernel.run(0, 150_000_000);
    }

    /// Non-harmonic periods (neither a multiple of the other, `gcd(60_000_000, 20_000_000,
    /// 30_000_000) = 10_000_000`) -- the same shape of case
    /// `crate::schedule`'s own `hetero_scheduler_base_period_ns_computes_the_gcd_and_passes_its_own_check`
    /// exercises directly on `HeteroScheduler`; here it's exercised through
    /// `HeteroKernel::run`'s own load-time gate (ADR-005 sec 2), proving the gate is actually
    /// wired into the kernel's run path, not just present on the scheduler underneath. Both
    /// instance periods are chosen to divide the output period evenly (as
    /// `Kernel::run`'s own `"slow"`-at-the-output-rate test also arranges) -- a system whose
    /// period is *slower* than the kernel's output rate has no native sample at the first
    /// output tick and is a documented `Scheduler`/`HeteroScheduler` limitation
    /// (`ScheduleError`/`HeteroScheduleError::OutOfRange`) shared by both kernel types alike,
    /// not something this gate changes.
    /// (`HeteroKernelError::BasePeriod`'s failure arm is not reachable through this crate's
    /// public API: `HeteroScheduler::register` already asserts `period_ns > 0` and
    /// `HeteroKernel::new` already asserts `output_period_ns > 0`, and a GCD of strictly
    /// positive periods can never fail its own integer-multiple check -- see
    /// `crate::clock::base_period_ns`'s doc comment. The success path below is what a caller
    /// can actually observe.)
    #[test]
    fn hetero_kernel_run_succeeds_through_the_base_period_gate_with_non_harmonic_rates() {
        let t0: i64 = 0;
        let mut kernel = HeteroKernel::new(60_000_000); // output every 60 ms
        kernel.register_system("fast", 20_000_000, erased("test.accel", [0.0; 3]), t0, vec![0.0; 6]); // 3 native steps per output tick
        kernel.register_system("medium", 30_000_000, erased_rotator("test.rotor", 0.1), t0, vec![1.0, 0.0]); // 2 native steps per output tick
        let trajectories = kernel.run(t0, t0 + 180_000_000).unwrap(); // 3 output ticks
        assert_eq!(trajectories.len(), 2);
        for traj in trajectories.values() {
            assert_eq!(traj.samples.len(), 4); // t = 0, 60, 120, 180 ms
        }
    }

    #[test]
    fn hetero_kernel_run_with_covariance_matches_the_closed_form_rotation_and_leaves_unrequested_systems_empty() {
        let t0: i64 = 0;
        let output_period_ns: i64 = 500_000_000; // 0.5 s
        let w = 0.4;

        let mut kernel = HeteroKernel::new(output_period_ns);
        kernel.register_system("spun", output_period_ns, erased_stm_rotator("test.rotor", w), t0, StmAugmented::<HeteroRotator>::seed(&[1.0, 0.0]));
        kernel.register_system("unrequested", output_period_ns, erased_stm_rotator("test.rotor", w), t0, StmAugmented::<HeteroRotator>::seed(&[1.0, 0.0]));

        let end = t0 + 2_000_000_000; // 2 s = 4 output periods
        let mut dims = BTreeMap::new();
        dims.insert("spun".to_string(), 2);
        dims.insert("unrequested".to_string(), 2);
        let mut p0_map = BTreeMap::new();
        p0_map.insert("spun".to_string(), vec![9.0, 0.0, 0.0, 4.0]); // diagonal, unequal variances

        let trajectories = kernel.run_with_covariance(t0, end, &dims, &p0_map, false).unwrap();

        let spun = &trajectories["spun"];
        assert_eq!(spun.samples.first().unwrap().mean, vec![1.0, 0.0], "mean must be truncated back to the physical state");
        assert_eq!(spun.samples.first().unwrap().cov, vec![9.0, 0.0, 0.0, 4.0], "Phi(t0,t0)=I: P(t0) must equal P0 exactly");

        let last = spun.samples.last().unwrap();
        let dt_s = (end - t0) as f64 * 1e-9;
        let (c, s) = ((w * dt_s).cos(), (w * dt_s).sin());
        let want_mean = [c * 1.0 + s * 0.0, -s * 1.0 + c * 0.0];
        for (got, want) in last.mean.iter().zip(want_mean.iter()) {
            assert!((got - want).abs() < 1e-6, "{got} vs {want}");
        }
        let trace0 = 9.0 + 4.0;
        let trace1 = last.cov[0] + last.cov[3];
        assert!((trace0 - trace1).abs() < 1e-6, "{trace0} vs {trace1}");
        assert_eq!(last.cov[1], last.cov[2], "propagated covariance must be exactly symmetric");

        let unrequested = &trajectories["unrequested"];
        assert!(unrequested.samples.iter().all(|s| s.cov.is_empty()), "unrequested system must not get covariance (question 11)");
        assert_eq!(unrequested.samples.len(), spun.samples.len());
    }

    /// `HeteroKernel` counterpart of `run_with_covariance_at_a_coarser_instance_period_
    /// propagates_only_on_the_native_grid` -- M13.3's lifted equal-rate requirement, over the
    /// trait-object kernel. Same 500 ms native / 100 ms output split, same closed-form checks.
    #[test]
    fn hetero_kernel_run_with_covariance_at_a_coarser_instance_period_propagates_only_on_the_native_grid() {
        let t0: i64 = 0;
        let output_period_ns: i64 = 100_000_000; // 100 ms: finer than the instance's own period
        let period_ns: i64 = 500_000_000; // 500 ms: the instance's own covariance step
        // Nonzero acceleration is fine here (unlike a cruder one-point prediction would need):
        // every off-grid tick below resolves through a genuine two-point Hermite bracket between
        // two REAL, numerically-integrated native samples (M13.3's `advance_to` fix guarantees
        // `advance_to` always catches a coarser system up past its target rather than leaving it
        // short -- see that method's own doc comment), and a cubic Hermite built from exact
        // (position, velocity) endpoints reproduces any degree-<=3 polynomial exactly --
        // `constant_accel_stm_closed_form`'s own quadratic position included (same reasoning
        // `interpolate::tests::exact_for_a_true_cubic_position_function` already establishes for
        // `hermite_velocity` directly).
        let a = [0.0, 0.0, -2.0];
        let x0 = [0.0, 0.0, 0.0, 1.0, 0.5, 0.0];
        let p0 = [
            100.0, 0.0, 0.0, 0.0, 0.0, 0.0, //
            0.0, 100.0, 0.0, 0.0, 0.0, 0.0, //
            0.0, 0.0, 100.0, 0.0, 0.0, 0.0, //
            0.0, 0.0, 0.0, 1.0, 0.0, 0.0, //
            0.0, 0.0, 0.0, 0.0, 1.0, 0.0, //
            0.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        ];

        let mut kernel = HeteroKernel::new(output_period_ns);
        kernel.register_system("cov", period_ns, erased_stm_accel("test.accel_stm", a), t0, StmAugmented::<ConstantAccelStm>::seed(&x0));
        kernel.register_system("unrequested", period_ns, erased_stm_accel("test.accel_stm", a), t0, StmAugmented::<ConstantAccelStm>::seed(&x0));

        let end = t0 + 1_500_000_000; // 1.5 s = 3 native periods, 15 output ticks
        let mut dims = BTreeMap::new();
        dims.insert("cov".to_string(), 6);
        dims.insert("unrequested".to_string(), 6);
        let mut p0_map = BTreeMap::new();
        p0_map.insert("cov".to_string(), p0.to_vec());

        let trajectories = kernel.run_with_covariance(t0, end, &dims, &p0_map, false).unwrap();
        let traj = &trajectories["cov"];
        assert_eq!(traj.samples.len(), 16);

        for s in &traj.samples {
            let t_s = (s.tai_ns - t0) as f64 * 1e-9;
            let want_mean = constant_accel_stm_closed_form(&x0, a, t_s);
            for (got, want) in s.mean.iter().zip(want_mean.iter()) {
                assert!((got - want).abs() < 1e-9, "tai_ns {}: mean {} vs closed-form {}", s.tai_ns, got, want);
            }
            let on_native_grid = (s.tai_ns - t0) % period_ns == 0;
            match covariance(s) {
                Some(cov) => {
                    assert!(on_native_grid, "tai_ns {}: got a real covariance off the 500 ms native grid", s.tai_ns);
                    let want_cov = constant_accel_stm_closed_form_cov(&p0, t_s);
                    for (got, want) in cov.iter().zip(want_cov.iter()) {
                        let scale = want.abs().max(1.0);
                        assert!((got - want).abs() / scale < 1e-6, "tai_ns {}: cov {} vs closed-form {}", s.tai_ns, got, want);
                    }
                }
                None => {
                    // Question 111: no NaN sentinel any more -- an unavailable sample's cov is
                    // just empty on the wire.
                    assert!(!on_native_grid, "tai_ns {}: no covariance but is on the native grid", s.tai_ns);
                    assert!(s.cov.is_empty(), "an unavailable sample's cov must be empty on the wire (question 111), got {} entries", s.cov.len());
                }
            }
        }

        let unrequested = &trajectories["unrequested"];
        assert!(unrequested.samples.iter().all(|s| covariance(s).is_none()), "unrequested system must not get covariance (question 11)");
    }

    #[test]
    #[should_panic(expected = "physical_dims")]
    fn hetero_kernel_run_with_covariance_panics_on_a_system_missing_from_physical_dims() {
        let t0: i64 = 0;
        let period_ns: i64 = 500_000_000;
        let mut kernel = HeteroKernel::new(period_ns);
        kernel.register_system("s", period_ns, erased_stm_rotator("test.rotor", 1.0), t0, StmAugmented::<HeteroRotator>::seed(&[1.0, 0.0]));
        let p0_map = BTreeMap::new();
        // `dims` deliberately left empty -- "s" has no entry.
        let _ = kernel.run_with_covariance(t0, t0 + period_ns, &BTreeMap::new(), &p0_map, false);
    }

    #[test]
    fn hetero_kernel_run_with_covariance_returns_a_typed_hygiene_error_for_an_indefinite_covariance_and_counts_it() {
        let t0: i64 = 0;
        let period_ns: i64 = 500_000_000;
        let mut kernel = HeteroKernel::new(period_ns);
        kernel.register_system("s", period_ns, erased_stm_rotator("test.rotor", 0.0), t0, StmAugmented::<HeteroRotator>::seed(&[1.0, 0.0]));
        let mut dims = BTreeMap::new();
        dims.insert("s".to_string(), 2);
        let mut p0_map = BTreeMap::new();
        p0_map.insert("s".to_string(), vec![-1.0, 0.0, 0.0, -1.0]); // negative definite

        let before = av_cdm::covariance::spd_check_failures();
        let err = kernel.run_with_covariance(t0, t0 + period_ns, &dims, &p0_map, false).unwrap_err();
        assert!(
            matches!(err, HeteroKernelError::CovarianceHygiene(av_cdm::covariance::CovarianceHygieneError::NotPositiveDefinite { .. })),
            "{err}"
        );
        assert!(av_cdm::covariance::spd_check_failures() > before, "the failure must be counted");
    }

    #[test]
    fn hetero_kernel_run_with_covariance_repairs_an_indefinite_covariance_when_projection_is_opted_into() {
        let t0: i64 = 0;
        let period_ns: i64 = 500_000_000;
        let mut kernel = HeteroKernel::new(period_ns);
        kernel.register_system("s", period_ns, erased_stm_rotator("test.rotor", 0.0), t0, StmAugmented::<HeteroRotator>::seed(&[1.0, 0.0]));
        let mut dims = BTreeMap::new();
        dims.insert("s".to_string(), 2);
        let mut p0_map = BTreeMap::new();
        p0_map.insert("s".to_string(), vec![-1.0, 0.0, 0.0, -1.0]);

        let trajectories = kernel.run_with_covariance(t0, t0 + period_ns, &dims, &p0_map, true).expect("projection is opted into, so the run must succeed rather than error");
        let cov = &trajectories["s"].samples.last().unwrap().cov;
        assert_eq!(cov.len(), 4);
        av_cdm::covariance::check_spd_row_major(cov, 2, "test").expect("the projected covariance must pass the hygiene check it was built to pass");
    }

    // -- TrajectorySample.kind across a multi-rate run (question 116, M15.2) -----------------

    /// A system with **no physical state at all** (`state_dim() == 0`), the same shape
    /// `BINDING_KIND_CONTAINER` always has -- duplicated from `crate::schedule`'s own private
    /// `ZeroDim` test model (that one is not visible outside `schedule`'s own test module).
    struct ZeroDimSystem;
    impl DynamicsModel for ZeroDimSystem {
        type Error = std::convert::Infallible;
        fn state_dim(&self) -> usize {
            0
        }
        fn derivatives(&self, _s: &[f64], _t: i64, _c: &[f64], _o: &mut [f64]) -> Result<(), Self::Error> {
            Ok(())
        }
        fn describe(&self) -> ModelInfo {
            ModelInfo { id: "test.zero_dim".to_string(), ..Default::default() }
        }
        // Test-only zero-dimensional model; never emits telemetry.
        fn last_measurements(&self) -> Vec<av_cdm::pb::Measurement> {
            Vec::new()
        }
    }

    /// **M15.2 (question 116), the required "kinds on the multi-rate run" test.** One physical
    /// (`state_dim() == 6`) model and one zero-dimensional, container-shaped system, both
    /// registered at a 300 ms period under a 100 ms output rate, run together through
    /// `HeteroKernel::run_with_ports` -- exactly the shared-kernel path `crate::drm::executor`
    /// drives a real `SosConfiguration` through. All three `av_cdm::pb::SampleKind` variants are
    /// read directly off `TrajectorySample.kind`, in one run: NATIVE for both systems at their
    /// shared native-grid ticks (0, 300, 600, 900 ms), INTERPOLATED for the model strictly
    /// between them (Hermite, ADR-005 sec 3), HELD for the container strictly between them
    /// (zero-order hold, ADR-005 sec 3 -- it has nothing to interpolate).
    ///
    /// **What this test would fail against:**
    /// 1. An implementation that never threads a `SampleKind` onto the wire at all -- every
    ///    sample's `kind` reads back `SampleKind::Unspecified` (0), failing every assertion below.
    /// 2. An implementation that classifies the *model* correctly but reuses `HoldKind::Fresh ->
    ///    SampleKind::Held` / `HoldKind::Held -> SampleKind::Native` (the inverted mapping for a
    ///    zero-dimensional system) -- the container's real native-grid ticks would read HELD and
    ///    its off-grid ticks would read NATIVE, exactly backwards from the assertions below,
    ///    while the model's own assertions would still pass (catching a container-specific bug
    ///    a model-only test, like the one just above, cannot).
    /// 3. An implementation that reverts to `HeteroScheduler::sample` unconditionally for the
    ///    container (pre-M14.4 behaviour) -- this test would panic inside
    ///    `crate::interpolate::hermite_velocity` (0-length pair) at the first off-grid tick,
    ///    rather than completing.
    #[test]
    fn hetero_kernel_run_with_ports_populates_native_interpolated_and_held_kind_across_a_multi_rate_run() {
        let t0: i64 = 0;
        let output_period_ns: i64 = 100_000_000; // 100 ms
        let period_ns: i64 = 300_000_000; // 300 ms: coarser than the output rate, for both systems

        let mut router = crate::router::Router::build(&av_cdm::pb::SosConfiguration::default(), &BTreeMap::new()).expect("no connections declared, trivially valid");
        let mut kernel = HeteroKernel::new(output_period_ns);
        kernel.register_system("model", period_ns, erased("test.accel", [0.0, 0.0, 0.0]), t0, vec![0.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
        kernel.register_system(
            "container",
            period_ns,
            av_dynamics::erase_with_id("test.zero_dim", ZeroDimSystem, |_id, never: std::convert::Infallible| match never {}),
            t0,
            vec![],
        );

        let end = t0 + 900_000_000; // 900 ms = 3 native periods, 10 output ticks (0, 100, .., 900)
        let trajectories = kernel.run_with_ports(t0, end, &mut router).unwrap();

        let model_traj = &trajectories["model"];
        let container_traj = &trajectories["container"];
        assert_eq!(model_traj.samples.len(), 10);
        assert_eq!(container_traj.samples.len(), 10);

        let mut model_native = 0;
        for s in &model_traj.samples {
            let on_native_grid = (s.tai_ns - t0) % period_ns == 0;
            let want = if on_native_grid {
                model_native += 1;
                av_cdm::pb::SampleKind::Native
            } else {
                av_cdm::pb::SampleKind::Interpolated
            };
            assert_eq!(s.kind, want as i32, "model tai_ns {}: on_native_grid={on_native_grid}", s.tai_ns);
        }
        assert_eq!(model_native, 4, "0, 300, 600, 900 ms are the only ticks on the 300 ms native grid over a 900 ms run");

        let mut container_held = 0;
        for s in &container_traj.samples {
            assert!(s.mean.is_empty(), "a zero-dimensional system's mean is always empty, held or fresh");
            let on_native_grid = (s.tai_ns - t0) % period_ns == 0;
            let want = if on_native_grid {
                av_cdm::pb::SampleKind::Native
            } else {
                container_held += 1;
                av_cdm::pb::SampleKind::Held
            };
            assert_eq!(s.kind, want as i32, "container tai_ns {}: on_native_grid={on_native_grid}", s.tai_ns);
        }
        assert_eq!(container_held, 6, "the remaining 6 of 10 output ticks fall strictly between two of the container's own native steps");
    }
}
