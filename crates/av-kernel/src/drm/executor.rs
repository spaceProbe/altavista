//! [`execute`]: the end-to-end DRM run. Verifies every declared hash, binds every
//! `BINDING_KIND_MODEL`/`BINDING_KIND_CONTAINER` instance, honours `DrmOptions`, injects
//! `FAULT_TARGET_KIND_DYNAMICS` faults, validates every declared `Objective`/
//! `MeasureOfEffectiveness` expression **at load, before any propagation**, runs every instance
//! through [`crate::kernel::HeteroKernel`], and returns [`RunProducts`] (`docs/open-questions.md`
//! question 93): trajectories, events, evaluated `scores`, and the run's own overall provenance.
//!
//! ## One shared kernel run per `SosConfiguration` (M14.1, question 109)
//!
//! **Decided by the lead:** every non-covariance instance of one `SosConfiguration` -- every
//! `BINDING_KIND_MODEL` and `BINDING_KIND_CONTAINER` instance -- is driven through **one**
//! shared [`HeteroKernel::run_with_ports`] call per boundary-bounded span
//! ([`run_shared_group`]), with `crate::router::Router` actually delivering
//! `SosConfiguration.connections` between them. Before this task, `execute()` ran every
//! instance on its own isolated per-instance loop, so a `Router` built at load
//! (`crate::router::Router::build`) validated wiring but nothing a `step_with_ports` call ever
//! emitted reached another instance's own `Inbox` -- see [`run_shared_group`]'s own doc comment
//! for the full design, including how `FAULT_TARGET_KIND_DYNAMICS` faults and maneuvers now
//! split the *whole* shared run rather than one instance's own loop. **The covariance path is
//! unchanged**: a `DrmOptions.covariance` run still drives every model instance through its own
//! isolated [`run_covariance_instance`] loop (a `BINDING_KIND_CONTAINER` instance is still
//! refused, `DrmError::ModelNotStmCapable`, exactly as before this task) -- this is a disclosed
//! limitation, recorded in [`RunProducts::provenance`]'s own `"kernel_run_mode"` attribute
//! (`build_run_provenance`), not silently different behaviour depending on `covariance`.
//!
//! ## Segment merge across an unaffected boundary (M15.1, `docs/open-questions.md` question 115)
//!
//! M14.4 found that the "re-materialize every currently active instance at every boundary"
//! mechanism above leaves an honest but misleading mark on an instance a boundary never targets:
//! every re-materialization emits a fresh `TrajectorySegment`, so a bystander beside a
//! two-boundary (fault, then maneuver) target ends up with three segments -- all sharing one
//! identical `dynamics_hash` -- instead of the single segment its own, genuinely
//! never-reconfigured dynamics would suggest (`tests/restart_invariance.rs`'s own module doc
//! comment has the full account). **Decided (question 115): adjacent segments of one instance
//! merge when their `dynamics_hash` is equal AND no maneuver applied to that instance at the
//! boundary between them.** A maneuver on the instance itself always keeps its own boundary -- a
//! delta-v is a real state discontinuity even though the dynamics configuration did not change
//! (a maneuver boundary never touches `cur_plan`, so its own `dynamics_hash` can come out *equal*
//! to the segment before it -- the merge rule's maneuver check is therefore load-bearing on its
//! own, not merely a belt-and-braces restatement of the hash check; see
//! `tests/segment_merge.rs::a_maneuver_never_merges_its_own_boundary_even_when_the_dynamics_
//! hash_is_unchanged` for the case this actually happens).
//!
//! [`merge_adjacent_segments`] implements this as one small pass over each instance's own
//! `all_segments`, run once per instance at the end of [`run_shared_group`] (never inside the
//! covariance path, which has no analogous "re-materialize every active instance" mechanism to
//! begin with -- see this module's own "One shared kernel run" section above for why the
//! covariance path stays untouched). The "was this boundary a maneuver for this instance" flag it
//! needs is *not* a new, independently-maintained bookkeeping field: it is read straight off
//! [`append_span_samples`]'s own `keep_previous_last_and_drop_incoming_first` sample-deduplication
//! flag (`segment_preceded_by_own_maneuver`, threaded alongside `all_segments` in
//! [`ModelSpanState`]/[`ContainerSpanState`]) -- a maneuver boundary is precisely the case where
//! that flag is `false` (the pre-burn sample is popped rather than the post-burn one dropped), so
//! reusing it here ties both decisions to one source of truth instead of two that could silently
//! disagree.
//!
//! **Verifying the fault half of the rule, rather than assuming it (question 115's own
//! instruction).** A `FAULT_TARGET_KIND_DYNAMICS` fault's own boundary carries no special "this is
//! a fault" flag in the merge check at all -- it relies entirely on `dynamics_hash` actually
//! differing. This holds because [`fault::apply_dynamics_fault`] only ever changes a value that
//! feeds `av_dynamics::settings_hash` on the *next* materialization: a native binding's
//! `"accel.{x,y,z}"` fault writes directly into the `ConstantAccelSpec.a` component
//! `binding::materialize_constant_accel`'s own `settings_map` hashes, and a GMAT binding's
//! `"force_model.*"`/`"spacecraft.*"` fault writes into the `GmatSystemSpec` fields
//! `binding::gmat_settings` hashes the same way -- both proven directly by `tests/drm_executor.rs
//! ::a_dynamics_fault_splits_the_run_into_two_segments_with_continuous_state`'s own
//! `assert_ne!(traj.segments[0].dynamics_hash, traj.segments[1].dynamics_hash, ...)`. **A
//! degenerate fault that sets a parameter to the value it already had would leave the hash
//! unchanged** (nothing in `apply_dynamics_fault` refuses that), and the merge rule would then
//! merge across it -- correctly, since a fault that changes nothing really did leave the dynamics
//! configuration identical either side of its own boundary, the same "the recorded structure
//! should describe what actually happened" principle this whole task exists to restore. No test
//! in this crate exercises that degenerate case: no `Fault` in this crate's own fixtures or
//! goldens ever declares a no-op value.
//!
//! **A second case, disclosed through M18.3 and closed by M18.4 (`docs/open-questions.md`
//! question 127's second half).** Through M18.3, a GMAT-bound instance's own `dynamics_hash` baked
//! in its instantaneous Cartesian state: `fault::rebind_gmat_spec_at_state` always sets
//! `spacecraft.X/Y/Z/VX/VY/VZ` from the segment's own final physical state before every
//! re-materialization, fault or not, and `binding::gmat_settings` hashed every `spacecraft.*`
//! field including those six -- so a GMAT-bound bystander's own segments practically never shared
//! a hash across a boundary (its position/velocity differ at each re-materialization epoch) and
//! this merge pass left them unmerged. Measured directly, not merely predicted, by
//! `tests/demo_two_instance.rs::demo_two_instance_bystander_invariance_against_real_single_
//! instance_gmat_runs` (the first real-GMAT measurement of it). **M18.4 fixes the root cause**:
//! `binding::gmat_settings` now excludes exactly those six fields (`fault::CARTESIAN_FIELDS`) from
//! the `BTreeMap` it hashes -- see that function's own doc comment for the full account -- so a
//! GMAT-bound bystander's `dynamics_hash` is genuinely unchanged when nothing about its own
//! dynamics configuration changed, and this merge pass now collapses it exactly like a
//! native-bound bystander already did. `tests/demo_two_instance.rs`'s own test above was upgraded
//! from "measure and report either way" to asserting the merge as an equality, the same bar
//! `tests/restart_invariance.rs` already held native bindings to.
//!
//! ## `HeteroKernel` is now the only kernel this executor drives (M9.1)
//!
//! Every instance -- GMAT-bound or the native `ConstantAccelModel` placeholder, fault-split or
//! not, covariance-requested or not -- is erased to `av_dynamics::BoxedModel` and driven
//! through `crate::kernel::HeteroKernel`, never `crate::kernel::Kernel<binding::AnyModel>`.
//! `Kernel<AnyModel>`/`Kernel<StmAugmented<AnyModel>>` -- the two instantiations this module
//! used before M9.1 -- are retired: nothing in this crate registers either any more
//! (`crate::kernel::Kernel<M>` itself is unchanged and stays alive for its own unit tests and
//! `tests/golden_acceptance.rs`, which drive it directly against a single concrete model type;
//! only the *DRM executor's* use of the generic `Kernel<M>` over the `AnyModel` enum
//! specifically is gone).
//!
//! ## `crate::registry::ModelRegistry` is the sole constructor (M10.3, question 98)
//!
//! Through M10.2, this module called `binding::materialize_gmat`/`materialize_constant_accel`
//! directly, bypassing `crate::registry::ModelRegistry` entirely: fault/maneuver re-binding and
//! covariance seeding needed the unerased `binding::AnyModel`/`Materialized` shape a moment
//! longer than the registry's own constructors exposed at the time. As of M10.3,
//! `crate::registry::ModelHandle` is that "a moment longer" shape, made opaque -- this module
//! now calls only `crate::registry::ModelRegistry::construct_gmat`/`construct_native` (never
//! `binding::materialize_gmat`/`materialize_constant_accel` directly) and carries
//! `ModelHandle`s between construction and erasure ([`run_one_span`]/[`run_covariance_span`],
//! via `ModelHandle::into_boxed`/`into_boxed_stm`). **This module never names `binding::AnyModel`
//! at all** -- see `crate::registry`'s own module doc comment for `ModelHandle`'s full shape
//! and why re-binding at a fault/maneuver boundary needs no separate "mutate" API (it is just
//! another call to the same two constructors, with a freshly rebuilt spec).
//!
//! ## `DrmOptions` -> kernel knobs, field by field
//!
//! - **`sample_interval_s`** -> `HeteroKernel::new`'s `output_period_ns` (the trajectory's own
//!   sampling grid; ADR-002 "Trajectory sampling interval for stored products").
//! - **`default_step_rate_hz`** / a `SystemInstance`'s own **`step_rate_hz`** (which wins when
//!   positive) -> the instance's native integration period,
//!   `crate::schedule::HeteroScheduler::register`'s `period_ns`.
//! - **`covariance`** -> selects `HeteroKernel::run_with_covariance` instead of plain
//!   `HeteroKernel::run` (see "Covariance", below).
//! - **`nearest_spd_projection`** -> passed straight through to `run_with_covariance`'s own
//!   parameter of the same name (question 83).
//! - **`accept_missing_stm_terms`** -> passed to `GmatModel::new` for every GMAT-bound
//!   instance (question 82), *and* checked a second time, before any GMAT call, by
//!   `binding::classify_binding` (refuses a covariance request against a declared
//!   `RelativisticCorrection` force model that has not accepted it).
//! - **`real_time`** -> [`super::DrmError::RealTimeNotSupported`] when `true`: ADR-005's
//!   real-time runtime is still Planned and this crate only ever runs lockstep, so this
//!   option is refused rather than silently honoured as lockstep anyway (question 87's rule:
//!   say so, never drop it quietly).
//!
//! ## Covariance
//!
//! `options.covariance == true` for an instance requires: (a) the bound model to be
//! STM-capable ([`super::DrmError::ModelNotStmCapable`] otherwise -- the native
//! `ConstantAccelModel` placeholder never is), and (b) a declared initial covariance, read
//! from `SystemInstance.initial_covariance` (`proto/altavista/v1/system.proto` field 8,
//! question 89: row-major `n x n`, SI, packed doubles) -- [`super::DrmError::
//! MissingInitialCovariance`] if empty. This P0 is run through
//! `av_cdm::covariance::check_spd_row_major` **at load, before any propagation** (question 83's
//! SPD bar applied to the seed itself, not only to what `HeteroKernel::run_with_covariance`
//! later propagates from it): a failure is [`super::DrmError::CovarianceHygiene`] unless
//! `options.nearest_spd_projection` is set, in which case the same opt-in nearest-SPD projection
//! `HeteroKernel::run_with_covariance` applies to propagated samples is applied here too (see
//! [`load_initial_covariance`]). **Not yet supported together with `FAULT_TARGET_KIND_DYNAMICS`
//! faults on the same instance** ([`super::DrmError::CovarianceWithFaultsNotSupported`]):
//! combining the two would mean re-augmenting `StmAugmented` at every fault boundary the same
//! way the plain path re-materializes its model, which this task's required tests do not
//! exercise and this executor does not implement -- refused explicitly rather than silently
//! running the covariance path and ignoring the faults.
//!
//! **M13.3: the instance's own effective step rate no longer has to equal `sample_interval_s`.**
//! Before M13.3 this function refused any instance whose `step_rate_hz`/`default_step_rate_hz`
//! did not equal `sample_interval_s` exactly ([`HeteroKernel::run_with_covariance`]'s own prior
//! hard requirement). That kernel-level restriction is lifted (see that method's own doc
//! comment): covariance now propagates at the instance's own declared step, sampled -- never
//! interpolated -- at the trajectory's output epochs (ADR-005 sec 3); an output epoch off the
//! instance's own native grid gets a physical `mean` but no covariance
//! (`av_kernel::kernel::covariance` reads `None` there -- question 111, M14.3: no longer a
//! NaN-filled sentinel, and no longer distinguishable on the wire from "not requested," a
//! disclosed, deliberate trade documented on that function's own module doc comment). One
//! consequence threaded through [`run_covariance_span`]/
//! [`run_covariance_instance`]: a **maneuver** boundary's own covariance is carried into the
//! next span's `p0` unmodified (or Gates-injected -- see [`run_covariance_instance`]'s own
//! "Covariance across a burn" section), which only means anything if the boundary actually
//! lands on the instance's own covariance-native grid -- checked explicitly
//! ([`super::DrmError::ManeuverEpochNotOnCovarianceGrid`]) before ever propagating that span,
//! rather than letting a NaN-poisoned `p0` reach the next span's own SPD hygiene check under a
//! confusing "hygiene failure" report. The scenario's own end is not checked this way: the
//! final span's own `cov` is never carried anywhere further, so an off-grid run end simply means
//! the trajectory's very last sample has no real covariance -- exactly the documented, honest
//! "off-grid produces no covariance sample" behaviour, not an error.
//!
//! ## Fault injection (`FAULT_TARGET_KIND_DYNAMICS`, and `_HARDWARE` for a container power
//! cycle -- M16.2, question 120)
//!
//! See [`super::fault`]'s module doc comment for exactly what a fault can change and how a
//! GMAT-bound instance is re-bound at the fault epoch. Every DYNAMICS or HARDWARE fault epoch
//! must land exactly on the `sample_interval_s` output grid (checked up front, before any GMAT
//! call -- [`super::DrmError::FaultEpochNotOnSampleGrid`]): `HeteroKernel::run` panics if a
//! sub-run's own horizon is not an exact multiple of its output period, and turning that into a
//! typed error before it can happen is safer than letting a badly-timed fault reach the panic --
//! a container power-cycle boundary is driven through the identical `run_one_span`/
//! `HeteroKernel::run_with_ports` call a DYNAMICS boundary is, so it needs the identical
//! protection.
//!
//! ## Impulsive maneuvers (`docs/open-questions.md` question 97)
//!
//! `Scenario.events` of kind `"maneuver"` (`super::maneuver`) are the same shape of problem as
//! a DYNAMICS fault -- split the run at an exact epoch, re-bind from the previous segment's own
//! final physical state -- except a maneuver changes the *state* (an instantaneous velocity
//! jump, computed by [`apply_dv_to_state`]/`maneuver::dv_to_inertial` from the instance's own
//! physical `(r, v)` at the burn epoch), never the dynamics configuration, so no `BindingPlan`
//! change accompanies it. [`run_shared_group`] merges every active model instance's own faults
//! and maneuvers into one `(tai_ns, id)`-sorted [`Boundary`] list rather than treating them as
//! mutually exclusive: both are applied, in order, against the same continuously-carried
//! physical state. A maneuver's epoch must land exactly on the `sample_interval_s` output grid
//! too ([`super::DrmError::ManeuverEpochNotOnSampleGrid`], modeled directly on
//! `FaultEpochNotOnSampleGrid`, checked up front for the same reason). Unlike a fault boundary
//! (state fully continuous, so either of the two coincident samples may be kept), a maneuver
//! boundary is a real velocity discontinuity -- see [`append_span_samples`]'s own doc comment
//! for exactly how the sample kept at that epoch is chosen (the post-burn one, never both, so
//! `Interpolation::HermiteVelocity`'s distinct-epoch contract is never violated). Covariance
//! *is* supported together with maneuvers (unlike DYNAMICS faults) -- see
//! [`run_covariance_instance`]'s own "Covariance across a burn" doc comment section. Which dv a
//! declared `execution_error` block actually applies (commanded exactly, or a Gates-sampled
//! realization) is [`RunConfig::error_mode`] (`docs/open-questions.md` question 103), selected
//! identically by [`maneuver::dv_to_apply`] on both [`run_shared_group`] and
//! `run_covariance_instance` -- see that field's own doc comment.
//!
//! ## Scoring (`docs/open-questions.md` question 93)
//!
//! Every `DesignReferenceMission.objectives`/`.measures` expression is parsed and unit-checked
//! **twice**, deliberately: once at load (against a declared-shape-only [`crate::expr::
//! ExprRunProducts`] built from every instance's `SystemDefinition.state_space_id` with empty
//! samples -- [`super::DrmError::InvalidExpression`], before any instance has run) and again,
//! implicitly, as part of [`crate::expr::evaluate_objective`]/`evaluate_moe`'s own
//! parse-check-evaluate pipeline against the *real* post-run trajectories. The second pass is
//! not redundant: `crate::expr::typecheck::check` only validates units and reference/event
//! *existence*, never a `@time` offset's numeric range (`crate::expr::runproducts::
//! ExprRunProducts::resolve_time`'s bounds check runs only during evaluation) -- so a
//! `number 's'` time offset past the scenario's own duration passes load-time validation but
//! can still fail when actually evaluated against the real run, surfacing as the same
//! [`super::DrmError::InvalidExpression`] rather than a second, undocumented error shape.
//! `Objective` results carry `passed: Some(bool)` (ADR-005 sec 6's `|value - target| <=
//! tolerance` rule); `MeasureOfEffectiveness` results carry `passed: None` (no pass/fail
//! concept).
//!
//! ## Events (question 95, M9.3) and outputs
//!
//! This executor emits real [`Event`]s -- see [`super::events`]'s own module doc comment for
//! exactly which `EventKind`s (`EVENT_KIND_LIFECYCLE` run start/end, `EVENT_KIND_FAULT` for
//! every DYNAMICS fault actually applied, `EVENT_KIND_MANEUVER` for every maneuver actually
//! applied -- question 97) and, honestly, which one of this task's named kinds it still does
//! not (`EVENT_KIND_MODE_CHANGE`) and why. Every event from every
//! instance is collected into one `Vec<Event>` and sorted `(epoch, id)`
//! ([`super::events::epoch_id_order`], the same tie-break `super::fault::epoch_id_order` already
//! applies to `Fault` application order) before scoring and before being returned as
//! [`RunProducts::events`]. The load-time expression-validation pass (above) sees the *declared*
//! shape of these same events ([`super::events::declared_events`], built from `scenario`/
//! `cfg.sos.instances` alone, before any instance has run) so an `event.*`-referencing
//! `Objective`/`MeasureOfEffectiveness` validates at load exactly like any other reference does.
//!
//! `output.<instance>.<name>@time` similarly resolves against one derived producer this executor
//! attaches for every instance whose trajectory has velocity components:
//! [`crate::expr::speed_output`] (`|velocity|`, m/s) -- see that function's own doc comment for
//! why a derived quantity of the real trajectory, not `av_dynamics::StepResult.outputs` or a
//! GMAT calculated field, is what this executor can honestly produce today.
//!
//! ## Provenance (question 87's "What to build" item 6, extended by question 93)
//!
//! Each individual `Trajectory.config_hash` is the **DRM's own hash**; its `Provenance.
//! config_hash` carries the **`SosConfiguration`'s** hash, `Provenance.data_pack_hash` is
//! `Scenario.data_pack_hash`, `Provenance.run_id` is the caller-supplied `run_id`, and each
//! `SystemDefinition`'s own hash/id are recorded in `Provenance.attributes` -- all unchanged
//! from before this task. [`RunProducts.provenance`] (new, question 93) is the *run's own*
//! overall provenance built the same way (`config_hash` = the DRM's own hash,
//! `Provenance.attributes["sos_configuration_hash"]` = the `SosConfiguration`'s), so a caller
//! that only wants "what produced this whole run" does not have to read it off an arbitrary
//! per-instance `Trajectory`. `Provenance.created_tai_ns` is left `0` everywhere, deliberately:
//! this crate never reads the wall clock (ADR-002/ADR-004 determinism).
//!
//! ## GMAT object naming
//!
//! Every GMAT object this module constructs is named from the instance name and a segment
//! index (`format!("Drm{instance}_{segment}...")`, `binding::materialize_gmat`'s
//! `name_suffix`), unique within one `execute` call. GMAT's configuration manager is
//! process-global (`gmat_sys`'s own module docs), so two *separate* `execute` calls in the
//! same test binary reusing the same instance name would collide -- exactly the reason
//! `crates/gmat-sys/tests/*.rs` already gives every test's spacecraft a distinct name.
//!
//! ## Port traffic sidecar (`docs/open-questions.md` question 175, M25.4a)
//!
//! `crate::router::Router` is the single choke point every FRAMED/BYTE_STREAM frame this run
//! ever carries passes through (`crate::router`'s own module doc comment), so it is also the
//! sole recorder of `altavista.v1.PortTrafficLog` -- see `Router::deliver`/`begin_step`/
//! `take_port_traffic`'s own doc comments for exactly what is recorded and when. This module's
//! own part is only: call `Router::take_port_traffic` once, at the same place [`Router::
//! pending_count`] is already read (after every span of the run has finished); sort the result
//! `(sequence, instance, port)`, stably; and, only when [`RunConfig::products_dir`] is `Some`,
//! serialize it as `altavista.v1.PortTrafficLog` (`prost::Message::encode_to_vec`, the same
//! encoding path `RunProducts::to_proto` already uses), write it to `<products_dir>/
//! port_traffic.pb` (creating the directory if needed), and hash the exact bytes written
//! (`hash::sha256_hex`, this crate's one SHA-256 primitive -- no new crate, no `ring`) into
//! `RunProducts.port_traffic_hash`. `RunProducts.provenance.attributes["port_traffic_uri"]`
//! names the file. `products_dir: None` writes no file at all: `port_traffic_hash` stays
//! empty and `provenance.attributes["port_traffic"] = "not recorded"` records that explicitly
//! -- the two attributes are mutually exclusive, never both present, so a consumer can tell "no
//! sidecar was requested" from "a sidecar was requested and genuinely carried nothing" (the
//! empty-but-`Some`-`products_dir` case still writes a valid, empty-`records` `PortTrafficLog`
//! and a real hash of it, never conflated with the `None` case). Any I/O failure along the way
//! (creating the directory, writing the file) is [`super::DrmError::PortTrafficSidecarIo`], a
//! typed refusal -- never a panic, never a silently-empty hash.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use av_cdm::pb;
use av_cdm::pb::{
    AuthorKind, DesignReferenceMission, Event, Fault, FaultTargetKind, Provenance, Scenario, SosConfiguration, SystemDefinition, SystemInstance, Trajectory, TrajectorySample, TrajectorySegment, Unit,
};
use av_cdm::time::Tai;
use av_cdm::units;
use av_dynamics::{BoxedModel, ModelError};
use gmat_sys::Gmat;
use prost::Message as _;

use super::binding::{self, BindingPlan, ContainerError, SharedContainerModel};
use super::command;
use crate::registry::{ModelHandle, ModelRegistry};

/// Every named `StepResult.outputs` series one instance's run produced (question 95's second
/// half, task M10.2): name -> (native step epochs TAI ns, values), parallel per name. Factored
/// into a named alias (clippy::type_complexity) rather than written out at every call site --
/// epochs are the model's own native step times (`crate::schedule::HeteroScheduler::outputs`'s
/// own doc comment), not resampled onto the trajectory's own output grid.
type NamedOutputSeries = BTreeMap<String, (Vec<i64>, Vec<f64>)>;
use super::events;
use super::fault;
use super::hash;
use super::maneuver::{self, ExecutionErrorMode, ParsedManeuver};
use super::replay;
use super::DrmError;
use crate::kernel::{HeteroKernel, HeteroKernelError};

/// Everything [`execute`] needs: a live GMAT handle (only touched for `"gmat."`-dispatched
/// instances -- see the module doc comment), the three loaded-and-parsed artifacts, and a
/// caller-supplied run id. The caller must hold `gmat_sys::engine_lock()` for the duration of
/// this call whenever any instance actually needs GMAT.
pub struct RunConfig<'a> {
    pub gmat: &'a Gmat,
    pub drm: &'a DesignReferenceMission,
    pub sos: &'a SosConfiguration,
    /// `SystemDefinition`s, keyed by `SystemDefinition.id`, covering every
    /// `SystemInstance.system_id` this `SosConfiguration` names.
    pub systems: &'a BTreeMap<String, SystemDefinition>,
    pub run_id: String,
    /// `docs/open-questions.md` question 103 (lead review of M11.4): which of the Gates burn
    /// execution error model's two paths (`maneuver`'s own module doc comment's "Burn execution
    /// error" section) this run realizes a declared `execution_error` block through --
    /// [`ExecutionErrorMode::Nominal`] (the `Default`) applies the commanded dv and injects `P+
    /// = P- + G Q G^T` when covariance is on; [`ExecutionErrorMode::Sampled`] applies a drawn
    /// realization and injects nothing, on *either* the plain or the covariance path.
    /// **Never inferred from `DrmOptions.covariance` or any other shape of the run** -- see
    /// [`ExecutionErrorMode`]'s own doc comment for the M11.4 bug this field closes (the run's
    /// own shape used to pick the path implicitly, so a nominal single run carrying a Gates
    /// block silently got a dispersed burn). Threaded unchanged into both
    /// [`run_shared_group`] and [`run_covariance_instance`] -- see [`maneuver::dv_to_apply`],
    /// the one function either path calls to select the applied dv, so the two paths can never
    /// disagree about what a given mode means.
    pub error_mode: ExecutionErrorMode,
    /// `docs/open-questions.md` question 175 (M25.4a): where this run's own out-of-band
    /// products go. `Some(dir)` -- the `PortTrafficLog` sidecar (`RunProducts.
    /// port_traffic_hash`'s own wire doc comment) is written to `dir.join("port_traffic.pb")`
    /// (`dir` created if needed), `RunProducts.port_traffic_hash` is set to its real SHA-256,
    /// and `RunProducts.provenance.attributes["port_traffic_uri"]` names the file.  `None` --
    /// no file is written, `port_traffic_hash` stays empty, and `provenance.attributes[
    /// "port_traffic"] = "not recorded"` instead (absence is explicit, never silent -- see
    /// [`execute`]'s own "Port traffic sidecar" doc section). `crates/av-run/src/main.rs`
    /// derives this from `--out`'s own parent directory; every test in this crate that does not
    /// care about the sidecar passes `None`.
    pub products_dir: Option<PathBuf>,
    /// M25.4b (question 175's own follow-on): `Some` replays one or more instances' own
    /// recorded port traffic instead of running their bound process -- see
    /// [`super::replay::ReplayConfig`]'s own doc comment for the exact fields and
    /// `crate::drm::replay`'s own module doc comment for the full contract (what is and is not
    /// replayed, the missing-frame rule). `None` -- every test in this crate that does not care
    /// about replay, and `crates/av-run/src/main.rs` (no `--replay` CLI flag is added by this
    /// task; `av-run` always passes `None` here) -- runs exactly as before this field existed.
    pub replay: Option<super::replay::ReplayConfig>,
}

/// One `Objective`'s or `MeasureOfEffectiveness`'s evaluated result (`docs/open-questions.md`
/// question 93). `passed` is `Some(bool)` for an `Objective` (ADR-005 sec 6's `|value - target|
/// <= tolerance` rule, already applied by `crate::expr::evaluate_objective`) and `None` for a
/// `MeasureOfEffectiveness` ("a value with a unit," no pass/fail concept).
#[derive(Debug, Clone, PartialEq)]
pub struct Score {
    pub value: f64,
    pub unit: Unit,
    pub passed: Option<bool>,
}

/// [`execute`]'s return value (`docs/open-questions.md` question 93): everything one DRM run
/// produced. See `crate::expr`'s own module doc comment ("`ExprRunProducts` vs. `crate::drm::
/// executor::RunProducts`") for why this is a deliberately different, differently-named type
/// from [`crate::expr::ExprRunProducts`] (the expression evaluator's own borrowed, read-only
/// view built FROM this type's `trajectories`/`events` fields, used only while scoring is in
/// progress).
///
/// `events` (`docs/open-questions.md` question 95, M9.3) is every real `Event` this run
/// produced -- run start/end and every DYNAMICS fault actually applied, see the module doc
/// comment's "Events (question 95, M9.3) and outputs" section and [`super::events`]'s own doc
/// comment for exactly what that is, and honestly is not. Sorted `(epoch, id)`
/// ([`super::events::epoch_id_order`]). `provenance` is the run's own overall provenance (see
/// the module doc comment's "Provenance" section); every individual `Trajectory` in
/// `trajectories` additionally keeps its own per-instance `Provenance`, unchanged from before
/// this struct existed.
#[derive(Debug, Clone)]
pub struct RunProducts {
    pub trajectories: BTreeMap<String, Trajectory>,
    pub events: Vec<Event>,
    pub scores: BTreeMap<String, Score>,
    pub provenance: Provenance,
    /// Count of in-flight port messages dropped at `end_tai_ns` (question 117, M14.4) --
    /// exactly the value `build_run_provenance` already records as `provenance.attributes[
    /// "dropped_in_flight_messages"]`, kept there too for older consumers (`ScoreResult`'s own
    /// proto doc comment on `pb::RunProducts.dropped_in_flight_messages`). Added as a
    /// first-class field (question 121, M17.2) so [`RunProducts::to_proto`] can set the wire
    /// message's own `dropped_in_flight_messages` field directly, rather than re-parsing it
    /// back out of a string-valued provenance attribute.
    pub dropped_in_flight_messages: u64,
    /// `FrameDefinition`s every `Trajectory.frame_id` this run's `trajectories` reference
    /// resolves against (question 121/122, M17.2): `Scenario.frames` (the DRM's own explicit
    /// declarations -- today always empty, since `schema::RawScenario` still refuses a
    /// non-empty `frames` block, see that module's own doc comment) plus the registry defaults
    /// this run actually used (see [`registry_default_frame`]). Sorted by id. See
    /// [`collect_frames`] for exactly how this is built, and this field's own "Known
    /// limitations" README note for what is -- and honestly is not -- covered.
    pub frames: Vec<pb::FrameDefinition>,
    /// CDM `Measurement`s this run produced (question 173, M25.3): telemetry decoded by the
    /// declared `PacketCodec`s (`PacketField.target`) and sensor-model outputs -- see
    /// [`crate::codec::measurements_from_field_values`]'s own doc comment for exactly how a
    /// packet's fields become one of these. Sorted `(epoch_ns, measurement_id)` (`RunProducts.
    /// measurements`'s own proto doc comment states this ordering as part of the contract) --
    /// [`sort_measurements`] is the one place this crate imposes it. Empty, never synthesized,
    /// for a packet whose declared codec maps no field.
    ///
    /// **Drop semantics (M25.3c, pinned by `crates/av-kernel/tests/demo_measurements.rs`'s own
    /// "Work item 3" section):** a `Measurement` is decoded at the emitting instance's own
    /// `step_with_ports` call, before the packet ever reaches `crate::router::Router`, so a
    /// telemetry packet the router later never delivers (no declared `Connection` for
    /// its port, or a `"latency"` connection whose delivery never lands before the run ends) still
    /// contributes its `Measurement` here. **A declared SENSOR-targeted fault
    /// (`av_cdm::pb::FaultTargetKind::Sensor`) can still never reach this field at all** --
    /// `execute()` refuses such a DRM at load (`DrmError::PortOrSensorFaultNotYetSupported`,
    /// raised in the same up-front fault-validation loop as `DrmError::UnknownFaultInstance`),
    /// before any binding or GMAT call, unchanged since M25.4a (`docs/open-questions.md` question
    /// 178). **A PORT-targeted fault is different as of R4.1a**: `kind == "drop"` now genuinely
    /// CAN reach this field, honestly -- a dropped FRAMED packet never leaves `crate::router::
    /// Router::deliver`'s own OUT recording, which happens entirely independently of, and after,
    /// `Measurement` decoding at the emitter's own `step_with_ports` call (this doc comment's own
    /// first sentence, above), so a dropped packet's own `Measurement` still appears here exactly
    /// like an unconnected port's own packet already did before this task. **R4.1b: `kind ==
    /// "corrupt"`/`"duplicate"` are real now too, and neither retroactively changes this field
    /// either** -- decoding always happens at the emitter, from its own pre-fault bytes, before
    /// `crate::router::Router::deliver` is ever consulted, so a corrupted or duplicated packet's
    /// `Measurement` is exactly what an unfaulted run would have produced (`crate::router`'s own
    /// module doc comment, "Corrupt"/"Duplicate": the mutation/second delivery is something the
    /// RECEIVER experiences, never something that reaches back and changes what the emitter
    /// already decoded from its own original values) -- pinned by `tests/demo_measurements.rs`.
    /// Every `FAULT_TARGET_KIND_SENSOR` fault is still refused at load
    /// (`DrmError::PortOrSensorFaultNotYetSupported`), so that shape still never reaches a real
    /// run at all.
    pub measurements: Vec<pb::Measurement>,
    /// `docs/open-questions.md` question 175 (M25.4a): SHA-256 (lowercase hex) of the exact
    /// bytes written to this run's `PortTrafficLog` sidecar -- empty when [`RunConfig::
    /// products_dir`] was `None` (no sidecar was requested; `RunProducts::to_proto`'s own wire
    /// field doc comment). See this module's own "Port traffic sidecar" doc section for exactly
    /// how this is computed and written.
    pub port_traffic_hash: String,
}

/// [`RunProducts::measurements`]'s own required order (question 173): `(epoch_ns,
/// measurement_id)`, ascending, a stable sort so two measurements that tie on both keys keep
/// whatever order they were produced in (native-step order, per instance) -- the same
/// "ascending, stable" convention [`super::events::epoch_id_order`] already established for
/// `RunProducts.events`.
fn sort_measurements(measurements: &mut [pb::Measurement]) {
    measurements.sort_by(|a, b| a.epoch_ns.cmp(&b.epoch_ns).then_with(|| a.measurement_id.cmp(&b.measurement_id)));
}

impl RunProducts {
    /// This run's wire form (question 121, M17.2): `altavista.v1.RunProducts`, replacing the
    /// ad hoc `AVRUN1` length-prefixed concatenation `crates/av-run` used to hand-roll before
    /// the lead added a real CDM message for a whole run (`docs/open-questions.md` question
    /// 121's own decision). Every field converts directly -- `trajectories`/`events`/
    /// `provenance`/`frames` are already the real, unmodified `av_cdm::pb` types this struct
    /// carries, so they clone straight across; `scores` becomes one `ScoreResult` per entry,
    /// under the identical map key (`execute()` always inserts a score under its own
    /// `Objective`/`MeasureOfEffectiveness.name`, so the map key and `ScoreResult.name` can
    /// never disagree).
    ///
    /// **`passed: Option<bool>` -> proto3 `optional bool`, distinguishably.** `ScoreResult.
    /// passed` is declared `optional bool` in `run.proto` specifically so an evaluated
    /// `Objective`'s `Some(false)` (failed) and a `MeasureOfEffectiveness`'s `None` (no
    /// pass/fail concept at all -- ADR-005 sec 6) never collapse onto the same wire value the
    /// way a plain, non-optional `bool` field would force them to (both would have to encode as
    /// `false`, indistinguishable on the wire). `prost`'s codegen for a proto3 `optional` field
    /// is `Option<bool>` here too (`av_cdm::pb::ScoreResult::passed`), so this assignment is a
    /// straight copy, not a lossy `unwrap_or` -- see `tests/run_products_proto.rs` (or this
    /// module's own `#[cfg(test)]`, wherever the round-trip test lives) for the byte-level proof
    /// that `None` really does survive `encode_to_vec`/`decode` as `None`, not `Some(false)`.
    pub fn to_proto(&self) -> pb::RunProducts {
        pb::RunProducts {
            run_id: self.provenance.run_id.clone(),
            trajectories: self.trajectories.clone(),
            events: self.events.clone(),
            scores: self
                .scores
                .iter()
                .map(|(name, score)| (name.clone(), pb::ScoreResult { name: name.clone(), value: score.value, unit: score.unit as i32, passed: score.passed }))
                .collect(),
            provenance: Some(self.provenance.clone()),
            dropped_in_flight_messages: self.dropped_in_flight_messages,
            frames: self.frames.clone(),
            // Question 173: real as of M25.3 -- telemetry decoded by the declared codecs and
            // sensor-model outputs, already sorted (`sort_measurements`, applied once at
            // `RunProducts` construction so every reader of this struct, not just `to_proto`,
            // sees the required order).
            measurements: self.measurements.clone(),
            // Question 175 (M25.4a): the real SHA-256 of the PortTrafficLog sidecar this run
            // wrote (empty when RunConfig::products_dir was None) -- computed once, by
            // execute(), and carried on this struct rather than recomputed here.
            port_traffic_hash: self.port_traffic_hash.clone(),
        }
    }
}

/// [`RunProducts::frames`]'s "registry defaults the run actually used" half (question 122):
/// the one case this crate can derive a `FrameDefinition` for a `Trajectory.frame_id` without a
/// live frame registry -- this crate has none yet (Planned/partial; `schema.rs`'s own doc
/// comment: "the frame registry is Planned/partial and this task does not need it"). A
/// GMAT-bound instance's `Trajectory.frame_id` is always its own `spacecraft.CoordinateSystem`
/// name (`binding::ModelInfo.frame_id`, `binding.rs`), and every body-axes `CoordinateSystem`
/// either this crate or the Python side's `altavista.frames.FrameRegistry._register_body_axes`
/// constructs is named `f"{body}{axes_gmat_name}"` (e.g. `"EarthMJ2000Eq"`) for exactly the
/// four body-axes kinds this crate's own `AxesKind` declares GMAT realizations for
/// (ICRF/MJ2000Eq/MJ2000Ec/BodyFixed) -- this function is that naming convention's reverse:
/// strip a known axes suffix off `frame_id` and, if a non-empty body prefix remains, return the
/// matching `FrameDefinition { body, axes }`.
///
/// Any `frame_id` that does not end in one of those four suffixes -- a native/`ConstantAccel`
/// instance's own opaque `SystemDefinition.parameters["frame_id"]` string (e.g. this crate's
/// own test fixtures' `"test.frame"`), or an `ObjectReferenced` RIC/VNB/VVLH `CoordinateSystem`
/// GMAT names some other way -- is left unrepresented here, never guessed: the same "never
/// silently approximate an axes kind" rule `altavista/cdm.py::frame_definition_for` already
/// follows on the Python side of this same boundary.
/// The plain "does `frame_id` decompose as `{body}{axes}`" half of [`registry_default_frame`],
/// factored out at M19.1 (question 128) so `binding::parse_gmat_spec`'s own Step 4
/// realizability check and [`convert_gmat_trajectory_to_declared_frame`]'s own GMAT AxisSystem
/// type lookup share the identical vocabulary this crate recognizes -- never a second, drifting
/// copy of the same four suffixes. Returns `(body, axes)` where `axes` is both this crate's own
/// registry axes string AND, not by coincidence, the exact GMAT AxisSystem factory type string
/// (`Construct(axes, name)` builds a real AxisSystem of that kind -- confirmed for `"BodyFixed"`
/// by `crates/gmat-sys/tests/epoch_writeback.rs`'s own "EpochBackAxes"; `"ICRF"`/`"MJ2000Eq"`/
/// `"MJ2000Ec"` are GMAT's own well-known AxisSystem names). `pub(crate)`: also called from
/// `binding::parse_gmat_spec` (a sibling module under `drm`).
pub(crate) fn body_axes_suffix(frame_id: &str) -> Option<(&str, &'static str)> {
    const SUFFIXES: [&str; 4] = ["MJ2000Eq", "MJ2000Ec", "BodyFixed", "ICRF"];
    for suffix in SUFFIXES {
        if let Some(body) = frame_id.strip_suffix(suffix) {
            if !body.is_empty() {
                return Some((body, suffix));
            }
        }
    }
    None
}

/// A human description of `axes` -- what the frame physically is and how it is oriented --
/// with `{body}` left as a placeholder for the origin body's name. Question 136 (`docs/
/// open-questions.md`, decided by the lead): a producer-supplied `FrameDefinition.description`
/// must read as a mission analyst's tooltip for the frame itself (what it is, its origin, its
/// axes), never a process note ("registry default for...") or a question-number citation.
fn human_axes_description(axes: pb::AxesKind, body: &str) -> String {
    match axes {
        pb::AxesKind::Icrf => format!(
            "International Celestial Reference Frame (ICRF): an inertial frame whose axes are fixed to distant quasar positions, origin at {body}'s centre of mass."
        ),
        pb::AxesKind::Mj2000Eq => format!(
            "Mean equatorial inertial frame at the J2000 epoch: axes fixed to {body}'s mean equator and equinox at that epoch (X toward the mean equinox, Z along the mean rotation axis), origin at {body}'s centre of mass."
        ),
        pb::AxesKind::Mj2000Ec => format!(
            "Mean ecliptic inertial frame at the J2000 epoch: axes fixed to the mean ecliptic plane and equinox at that epoch (X toward the mean equinox, Z normal to the ecliptic), origin at {body}'s centre of mass."
        ),
        pb::AxesKind::BodyFixed => format!(
            "{body}-fixed frame: axes rotate with {body}'s own rotation (X through the prime meridian at the equator, Z along the spin axis), origin at {body}'s centre of mass."
        ),
        // `registry_default_frame` (this function's only caller) only ever constructs one of
        // the four arms above -- see that function's own `unreachable!` on the identical
        // exhaustive set. This crate's own "never silently guess" rule means a fifth axes kind
        // gets a typed panic here too, not an invented description.
        other => unreachable!("human_axes_description: {other:?} is not one of the four axes kinds registry_default_frame constructs"),
    }
}

fn registry_default_frame(frame_id: &str) -> Option<pb::FrameDefinition> {
    let (body, suffix) = body_axes_suffix(frame_id)?;
    let axes = match suffix {
        "MJ2000Eq" => pb::AxesKind::Mj2000Eq,
        "MJ2000Ec" => pb::AxesKind::Mj2000Ec,
        "BodyFixed" => pb::AxesKind::BodyFixed,
        "ICRF" => pb::AxesKind::Icrf,
        _ => unreachable!("body_axes_suffix only ever returns one of the four suffixes matched above"),
    };
    Some(pb::FrameDefinition {
        id: frame_id.to_string(),
        origin: Some(pb::frame_definition::Origin::Body(body.to_string())),
        axes: axes as i32,
        description: human_axes_description(axes, body),
        ..Default::default()
    })
}

/// [`RunProducts::frames`] itself: `scenario.frames` (the DRM's own explicit declarations, by
/// id) plus [`registry_default_frame`] for every distinct `Trajectory.frame_id` this run's
/// `trajectories` reference that `scenario.frames` did not already declare -- a declared
/// `Scenario.frames` entry always wins over a derived registry default for the same id, never
/// the other way around (an author's explicit declaration is authoritative). Sorted by id
/// (this crate's own "no `HashMap` iteration order" rule -- a proto `repeated` field has no
/// map to lean on, so an explicit `BTreeMap`/sort stands in for one, exactly like
/// `CdmBundle.state_spaces` does on the Python side). A `frame_id` neither `scenario.frames`
/// declares nor [`registry_default_frame`] can honestly derive is left out entirely -- see that
/// function's own doc comment for why silence, not a guess, is the right failure mode here.
fn collect_frames(scenario: &Scenario, trajectories: &BTreeMap<String, Trajectory>) -> Vec<pb::FrameDefinition> {
    let mut by_id: BTreeMap<String, pb::FrameDefinition> = BTreeMap::new();
    for f in &scenario.frames {
        by_id.insert(f.id.clone(), f.clone());
    }
    let referenced: std::collections::BTreeSet<&str> = trajectories.values().map(|t| t.frame_id.as_str()).filter(|id| !id.is_empty()).collect();
    for frame_id in referenced {
        if !by_id.contains_key(frame_id) {
            if let Some(def) = registry_default_frame(frame_id) {
                by_id.insert(frame_id.to_string(), def);
            }
        }
    }
    by_id.into_values().collect()
}

/// Question 10's day-one mandatory frames -- ICRF and MJ2000 equatorial for a run's own central
/// body, plus that body's body-fixed frame -- as [`registry_default_frame`]'s own body-axes
/// suffixes.
const MANDATORY_BODY_AXES_SUFFIXES: [&str; 3] = ["ICRF", "MJ2000Eq", "BodyFixed"];

/// Question 10/124 (M18.1): [`collect_frames`]'s own result, augmented with the mandatory
/// ICRF/MJ2000Eq/BodyFixed frames for every central body this run's own `BINDING_KIND_MODEL`
/// GMAT-bound instances declare (`binding::GmatSystemSpec.central_body`, read straight from
/// `plans` -- the same source `binding::gmat_settings`/`materialize_gmat` use to set GMAT's own
/// `ForceModel.CentralBody`/`GravityField.Earth.BodyName`, not a re-derivation from any
/// `Trajectory.frame_id` string) -- **always present regardless of which coordinate system the
/// instance actually propagated in** (question 124's own rationale: "a consumer may view any
/// trajectory in any registry frame the producer can realize"). A native (`ConstantAccel`)
/// instance declares no central body and contributes nothing here -- it was never propagated
/// against a body-centred force model, so "the central body" has no meaning for it; its own
/// (possibly opaque) `frame_id` is untouched, exactly as [`collect_frames`] already left it.
///
/// A declared `Scenario.frames` entry, or a frame already present because some instance's own
/// trajectory referenced it, always wins over this function's own construction for the same id
/// -- this only ever fills a gap, never overwrites (mirrors [`collect_frames`]'s own "declared
/// wins over derived" rule, question 122). See [`DrmError::MandatoryFrameNotRealizable`]'s own
/// doc comment for why the `None` arm below is structurally unreachable today, and is still a
/// typed refusal rather than a silent omission.
fn add_mandatory_body_frames(frames: Vec<pb::FrameDefinition>, plans: &BTreeMap<String, (BindingPlan, i64)>) -> Result<Vec<pb::FrameDefinition>, DrmError> {
    let mut by_id: BTreeMap<String, pb::FrameDefinition> = frames.into_iter().map(|f| (f.id.clone(), f)).collect();
    let central_bodies: std::collections::BTreeSet<&str> = plans
        .values()
        .filter_map(|(plan, _)| match plan {
            BindingPlan::Gmat(spec) if !spec.central_body.is_empty() => Some(spec.central_body.as_str()),
            _ => None,
        })
        .collect();
    for body in central_bodies {
        for suffix in MANDATORY_BODY_AXES_SUFFIXES {
            let frame_id = format!("{body}{suffix}");
            if let std::collections::btree_map::Entry::Vacant(e) = by_id.entry(frame_id) {
                let def = registry_default_frame(e.key()).ok_or_else(|| DrmError::MandatoryFrameNotRealizable { body: body.to_string(), suffix })?;
                e.insert(def);
            }
        }
    }
    Ok(by_id.into_values().collect())
}

/// Question 129, M19.2 (ADR-002's fourth amendment "frame conversion is part of the contract"):
/// the tolerance a `FrameDefinition.fixed_rotation_q` this crate computes must satisfy to be
/// treated as a genuine unit quaternion (`core.proto` field 13's own contract) -- 1e-9 is many
/// orders of magnitude looser than the floating-point noise a correct DCM-to-quaternion
/// conversion of a genuine rotation matrix produces (~1e-15), so this only ever catches a real
/// arithmetic bug, never ordinary rounding.
const FIXED_ROTATION_UNIT_NORM_TOLERANCE: f64 = 1e-9;

/// Question 129: the wire's own contract for `FrameDefinition.fixed_rotation_q` --
/// **exactly 0 or 4 entries, and if 4, a unit quaternion** (`core.proto` field 13's own doc
/// comment: "Exactly 0 or 4 entries"). A malformed value is `DrmError::
/// InvalidFixedRotationQuaternion`, typed and naming the offending frame, never silently
/// truncated, padded or re-normalized away -- this task's own "no silent fallbacks" rule applied
/// to a field this crate computes itself. Called on every quaternion [`fill_fixed_rotations`]
/// computes (belt-and-suspenders against this module's own arithmetic), and `pub(crate)` so a
/// future caller elsewhere in this crate can reuse the identical rule rather than a second,
/// drifting copy of "what counts as a valid fixed_rotation_q".
pub(crate) fn validate_fixed_rotation_q(frame_id: &str, q: &[f64]) -> Result<(), DrmError> {
    match q.len() {
        0 => Ok(()),
        4 => {
            let norm = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
            if (norm - 1.0).abs() > FIXED_ROTATION_UNIT_NORM_TOLERANCE {
                Err(DrmError::InvalidFixedRotationQuaternion {
                    frame_id: frame_id.to_string(),
                    reason: format!("norm {norm} is not within {FIXED_ROTATION_UNIT_NORM_TOLERANCE:e} of 1.0 (a unit quaternion is required)"),
                })
            } else {
                Ok(())
            }
        }
        n => Err(DrmError::InvalidFixedRotationQuaternion { frame_id: frame_id.to_string(), reason: format!("has {n} entries; must be exactly 0 or 4") }),
    }
}

/// [`fill_fixed_rotations`]'s own measurement interval -- **measured** at 10 seconds, not the
/// "a day apart" example question 129's own brief opens with, for a concrete, root-caused
/// reason recorded here rather than silently deviated from.
///
/// **What was found.** Measuring `EarthICRF` against `EarthMJ2000Eq` at a full day's separation
/// gave a max element-wise disagreement of 1.32e-9 -- three orders of magnitude past
/// [`FIXED_ROTATION_CONSTANCY_TOLERANCE`], which would have left the wire's own headline case
/// (question 129: "ICRF against MJ2000 equatorial, the frame bias") empty. Root-caused, not
/// papered over, by reading GMAT's own C++ source (`third_party/gmat-src/src/base/coordsystem/
/// AxisSystem.cpp::RotationMatrixFromICRFToFK5`, `ICRFFile.cpp::GetICRFRotationVector`): GMAT does
/// **not** treat the ICRF<->FK5(MJ2000Eq) rotation as a hard-coded constant. It 9th-order-Lagrange-
/// interpolates an Euler rotation vector out of a bundled, unequally-spaced data table
/// (`ICRF_Table.txt`) at the queried epoch -- a numerically-realized model of the (real,
/// documented-in-the-literature) tiny secular drift between the ICRS pole/equinox and the
/// dynamical-ephemeris-defined FK5/J2000 equinox, not a bug in this crate's own shim, extraction
/// arithmetic or epoch conversion (confirmed: repeating the identical epoch through two
/// independent `Gmat::convert` calls returns bit-identical matrices -- see
/// `fixed_rotation_measurement_interval_tests::repeating_the_identical_epoch_gives_a_bit_identical_rotation_matrix`
/// below, and this task's own report for the full measured table). The residual is
/// *linear* in the separation, not noise: 1.78e-14 at 1 s, 1.78e-13 at 10 s, 1.07e-12 at 60 s,
/// 1.32e-9 at 1 day, 2.33e-8 at 100 days -- an apparent rate of roughly 1.78e-14 per second
/// (~0.27 mas/day) baked into the bundled `ICRF_Table.txt`, present regardless of which two
/// epochs are chosen, only *smaller* the closer together they are.
///
/// **Why 10 s is still an honest "measure, don't assume" check, not tolerance-shopping.** The
/// requirement this constant serves is "distinguish a frame whose rotation is fixed to the
/// precision this field promises from one that is genuinely time-varying" -- `AXES_KIND_BODY_
/// FIXED` (Earth's own rotation, ~7.29e-5 rad/s) changes by ~7.3e-4 rad in 10 s, twelve orders of
/// magnitude past [`FIXED_ROTATION_CONSTANCY_TOLERANCE`] -- unmistakably time-varying at ANY
/// separation from 1 ms up. 10 s sits comfortably below the 56 s point at which the ICRF table's
/// own measured linear residual would cross 1e-12 (5.6x margin), while still being two genuinely
/// distinct, independently-computed epochs rather than adjacent floating-point representable
/// values. A full day (or any separation approaching the ICRF table's own knot spacing) would
/// incorrectly report the wire's own headline fixed-frame case as time-varying -- exactly the
/// failure this constant's value was chosen to avoid, root-caused rather than worked around by
/// loosening [`FIXED_ROTATION_CONSTANCY_TOLERANCE`] itself (which stays at the brief's own 1e-12).
const FIXED_ROTATION_MEASURE_INTERVAL_NS: i64 = 10 * 1_000_000_000;

/// [`fill_fixed_rotations`]'s own constancy bound: the two measured rotation matrices
/// ([`FIXED_ROTATION_MEASURE_INTERVAL_NS`] apart) must agree element-wise to this tolerance for a
/// frame to be treated as fixed (this task's own brief: "assert they agree to 1e-12"). Never
/// loosened to make a frame pass -- see [`FIXED_ROTATION_MEASURE_INTERVAL_NS`]'s own doc comment
/// for the real, measured, non-noise reason a *day-long* separation would have failed this bound
/// even for the genuinely-fixed `EarthICRF`/`EarthMJ2000Eq` pair, and why the fix is the
/// separation, not this tolerance. See that same doc comment for why a body-fixed frame is
/// *expected* to fail this at any separation, not a bug to work around.
const FIXED_ROTATION_CONSTANCY_TOLERANCE: f64 = 1e-12;

/// The 3x3 direction-cosine (change-of-basis) matrix `C` such that, for any physical vector,
/// `v_to = C * v_from` -- i.e. `C`'s column `i` is `from_cs`'s own `i`-th basis vector, expressed
/// in `to_cs`'s coordinates. Built from three [`Gmat::convert`] calls, one per basis vector
/// (`[1,0,0,0,0,0]`/`[0,1,0,0,0,0]`/`[0,0,1,0,0,0]` -- zero velocity: a pure inertial-to-inertial
/// rotation has no time-varying relative angular rate between `from_cs`/`to_cs` for this
/// function's own two candidate axes kinds, ICRF and MJ2000Ec, both against the same body's
/// MJ2000Eq, so the converted velocity component is never read). This is exactly the matrix
/// [`matrix_to_quaternion`] turns into `fixed_rotation_q`'s own "parent -> this" unit quaternion
/// when `from_cs` is the body's MJ2000Eq and `to_cs` is the candidate frame.
fn rotation_matrix(gmat: &Gmat, epoch_a1mjd: f64, from_cs: &str, to_cs: &str) -> Result<[[f64; 3]; 3], DrmError> {
    let mut c = [[0.0_f64; 3]; 3];
    for col in 0..3 {
        let mut basis = [0.0_f64; 6];
        basis[col] = 1.0;
        let out = gmat.convert(epoch_a1mjd, &basis, from_cs, to_cs).map_err(DrmError::Gmat)?;
        for row in 0..3 {
            c[row][col] = out[row];
        }
    }
    Ok(c)
}

/// A proper 3x3 rotation matrix -> unit quaternion `[w, x, y, z]` (scalar-first, matching
/// `fixed_rotation_q`'s own documented order). The standard trace-based method (e.g. Shepperd
/// 1978), branching on the largest diagonal term to avoid dividing by a near-zero `sqrt` --
/// numerically safe for any proper rotation, which `rotation_matrix`'s output always is (GMAT's
/// own `CoordinateConverter::Convert` on two inertial axes systems is orthogonal by construction).
fn matrix_to_quaternion(m: &[[f64; 3]; 3]) -> [f64; 4] {
    let trace = m[0][0] + m[1][1] + m[2][2];
    if trace > 0.0 {
        let s = (trace + 1.0).sqrt() * 2.0; // s = 4w
        [s / 4.0, (m[2][1] - m[1][2]) / s, (m[0][2] - m[2][0]) / s, (m[1][0] - m[0][1]) / s]
    } else if m[0][0] > m[1][1] && m[0][0] > m[2][2] {
        let s = (1.0 + m[0][0] - m[1][1] - m[2][2]).sqrt() * 2.0; // s = 4x
        [(m[2][1] - m[1][2]) / s, s / 4.0, (m[0][1] + m[1][0]) / s, (m[0][2] + m[2][0]) / s]
    } else if m[1][1] > m[2][2] {
        let s = (1.0 + m[1][1] - m[0][0] - m[2][2]).sqrt() * 2.0; // s = 4y
        [(m[0][2] - m[2][0]) / s, (m[0][1] + m[1][0]) / s, s / 4.0, (m[1][2] + m[2][1]) / s]
    } else {
        let s = (1.0 + m[2][2] - m[0][0] - m[1][1]).sqrt() * 2.0; // s = 4z
        [(m[1][0] - m[0][1]) / s, (m[0][2] + m[2][0]) / s, (m[1][2] + m[2][1]) / s, s / 4.0]
    }
}

/// Question 129, M19.2 (ADR-002's fourth amendment): fills `FrameDefinition.fixed_rotation_q`
/// for every body-centred inertial frame in `frames` whose rotation relative to that SAME body's
/// own MJ2000Eq frame is constant in time -- measured, not assumed, by comparing the rotation
/// GMAT's own `CoordinateConverter::Convert` (`Gmat::convert`, via [`rotation_matrix`]) produces
/// at two epochs [`FIXED_ROTATION_MEASURE_INTERVAL_NS`] apart, to
/// [`FIXED_ROTATION_CONSTANCY_TOLERANCE`].
///
/// **Candidates**: a frame definition whose `origin` is `Body(b)` and whose `axes` is
/// `AXES_KIND_ICRF` or `AXES_KIND_MJ2000_EC`, for a body `b` that also has a registered
/// `AXES_KIND_MJ2000_EQ` frame somewhere in this same `frames` list -- the reference this
/// function measures every other body-axes frame's bias against, matching how RIC/VNB/VVLH's own
/// `parent_frame_id` already treats a body's MJ2000Eq as its canonical inertial reference
/// (`docs/open-questions.md` question 76). `AXES_KIND_MJ2000_EQ` itself is never a candidate: it
/// IS the reference, so there is nothing to measure it against, and its own `fixed_rotation_q`
/// stays empty -- consistent with `parent_frame_id`'s own "" (registry root) convention for it:
/// no rotation is applied to it in the viewer's frame graph either (`web/js/frames.js`).
///
/// **Never a candidate at all** (structurally excluded from the axes-kind check above, not
/// merely expected to fail the 1e-12 measurement): `AXES_KIND_BODY_FIXED` (a body's own rotation
/// is time-varying by construction -- see this task's own brief: a constancy check that ever
/// marked BodyFixed fixed would itself be a bug), and any frame whose `origin` is
/// `platform_id`/`entity_id` (ENU/NED, RIC/VNB/VVLH, PLATFORM_BODY, LOCAL_CARTESIAN -- these have
/// a moving origin, so their own `CoordinateConverter::Convert` mixes a real, time-varying
/// translation into any attempt to measure a "pure rotation", and several have no real GMAT
/// `CoordinateSystem` behind them at all, e.g. PLATFORM_BODY -- see `altavista/frames.py`'s own
/// AxesKind table). All left with an empty `fixed_rotation_q`, per this task's own scope.
///
/// Every GMAT object this constructs is named under `gmat_ns` (unique per [`execute`]
/// invocation) and the candidate frame's own `id`, so two different frames' own `CoordinateSystem`
/// pairs can never collide with each other or with another `execute()` call's objects -- the same
/// convention [`convert_gmat_trajectory_to_declared_frame`] already follows.
fn fill_fixed_rotations(gmat: &Gmat, gmat_ns: &str, frames: Vec<pb::FrameDefinition>, base_tai_ns: i64) -> Result<Vec<pb::FrameDefinition>, DrmError> {
    use std::collections::BTreeSet;

    let bodies_with_mj2000eq: BTreeSet<String> = frames
        .iter()
        .filter(|f| f.axes == pb::AxesKind::Mj2000Eq as i32)
        .filter_map(|f| match &f.origin {
            Some(pb::frame_definition::Origin::Body(b)) => Some(b.clone()),
            _ => None,
        })
        .collect();

    let epoch1_a1mjd = Tai::from_nanos(base_tai_ns).to_a1_mjd();
    let epoch2_a1mjd = Tai::from_nanos(base_tai_ns + FIXED_ROTATION_MEASURE_INTERVAL_NS).to_a1_mjd();

    frames
        .into_iter()
        .map(|mut f| {
            let axes_gmat_name = if f.axes == pb::AxesKind::Icrf as i32 {
                Some("ICRF")
            } else if f.axes == pb::AxesKind::Mj2000Ec as i32 {
                Some("MJ2000Ec")
            } else {
                None
            };
            let body = match (&f.origin, axes_gmat_name) {
                (Some(pb::frame_definition::Origin::Body(b)), Some(_)) if bodies_with_mj2000eq.contains(b) => Some(b.clone()),
                _ => None,
            };
            let (Some(body), Some(axes_gmat_name)) = (body, axes_gmat_name) else { return Ok(f) };

            let from_name = format!("FixRot{gmat_ns}_{}_Parent", f.id);
            let to_name = format!("FixRot{gmat_ns}_{}_This", f.id);
            gmat.coordinate_system(&from_name, &body, "MJ2000Eq").map_err(DrmError::Gmat)?;
            gmat.coordinate_system(&to_name, &body, axes_gmat_name).map_err(DrmError::Gmat)?;
            gmat.initialize().map_err(DrmError::Gmat)?;

            let c1 = rotation_matrix(gmat, epoch1_a1mjd, &from_name, &to_name)?;
            let c2 = rotation_matrix(gmat, epoch2_a1mjd, &from_name, &to_name)?;
            let max_delta = c1.iter().flatten().zip(c2.iter().flatten()).map(|(a, b)| (a - b).abs()).fold(0.0_f64, f64::max);
            if max_delta <= FIXED_ROTATION_CONSTANCY_TOLERANCE {
                let q = matrix_to_quaternion(&c1);
                validate_fixed_rotation_q(&f.id, &q)?;
                f.fixed_rotation_q = q.to_vec();
            }
            // else: genuinely time-varying over one day (or this crate's "this body's MJ2000Eq
            // is the fixed reference" assumption does not hold for this body/axes pair) --
            // fixed_rotation_q stays empty, never a guess.
            Ok(f)
        })
        .collect()
}

/// `SystemDefinition.parameters` entries named `"output.<name>"` declare that this instance
/// exposes `output.<instance>.<name>@time` (question 95's second half, task M10.2), pointing at
/// one of `gmat_sys::model::GmatModel::step`'s fixed, finite `StepResult.outputs` names (today:
/// `gmat_sys::model::OUTPUT_RMAG`) -- **the exact "use `SystemDefinition.parameters`, the
/// established extension point, so the declaration hashes with the definition" pattern the
/// covariance-P0 seeding used before it earned a real field** (this module's own doc comment
/// references that precedent). The value/min/max/string_value fields are unused; `unit` is the
/// declared unit `crate::expr::runproducts::Series`/`ExprRunProducts::with_output` attach, and
/// `description` is free text for a human reader.
///
/// **M10.3: no filtering pass needed any more.** Through M10.2, `binding::classify_binding`'s
/// `parse_gmat_spec` refused any parameter name outside its own `"force_model."`/`"spacecraft."`
/// vocabulary, including a declared `"output.<name>"` entry -- so this module had to hand it a
/// filtered copy of `sys` with every `"output.*"` parameter stripped first
/// (`strip_output_parameters`, this crate's own escalated-but-worked-around fix, since
/// `binding.rs` was not owned by that task). `binding.rs` is owned by this task, so the actual
/// fix landed there instead: `parse_gmat_spec` now recognizes and skips `"output.<name>"`
/// directly (see `binding`'s module doc comment's "Parameter vocabulary" section). This module
/// hands `binding::classify_binding` the real, unmodified `sys` unconditionally now -- the same
/// `SystemDefinition` `hash::verify_system_hash` checks and [`declared_outputs`] reads.
const OUTPUT_PARAMETER_PREFIX: &str = "output.";

/// See [`OUTPUT_PARAMETER_PREFIX`]'s doc comment: every `"output.<name>"` parameter this
/// `SystemDefinition` declares, as `(name, declared unit)` pairs -- `Unit::Unspecified` if the
/// parameter's own `unit` field was left unset/unrecognized (never refused here; an expression
/// referencing it would still typecheck, just against an unhelpful unit, same as any other
/// `Parameter` this crate reads).
fn declared_outputs(sys: &SystemDefinition) -> Vec<(String, Unit)> {
    sys.parameters.iter().filter_map(|p| p.name.strip_prefix(OUTPUT_PARAMETER_PREFIX).map(|name| (name.to_string(), Unit::try_from(p.unit).unwrap_or(Unit::Unspecified)))).collect()
}

fn effective_step_rate_hz(instance: &SystemInstance, options: &av_cdm::pb::DrmOptions) -> f64 {
    if instance.step_rate_hz > 0.0 {
        instance.step_rate_hz
    } else {
        options.default_step_rate_hz
    }
}

/// Process-wide, monotonically increasing: [`gmat_execution_namespace`]'s own tie-breaker for two
/// `execute()` calls that reuse the identical `run_id` (M18.4, `docs/open-questions.md` question
/// 127). `Relaxed` because nothing but uniqueness is required of it -- no other memory access
/// needs to be ordered against reading or incrementing this counter. Never itself surfaced in any
/// output; folded only into a purely-internal GMAT object name string (see
/// `binding::materialize_gmat`'s own doc comment).
static GMAT_EXECUTE_SEQ: AtomicU64 = AtomicU64::new(0);

/// A caller-supplied `run_id` (`RunConfig.run_id`) is not, by itself, enough to namespace this
/// `execute()` call's own GMAT object names (M18.4, `docs/open-questions.md` question 127):
/// `Provenance.run_id` places `run_id` directly into `RunProducts`, so this task's own required
/// "run the same DRM twice, diff the products" test necessarily calls `execute()` twice with the
/// *identical* `run_id` (see `tests/drm_executor.rs::running_the_identical_drm_twice_in_one_
/// process_produces_byte_identical_products`'s own doc comment for why that is the one fair
/// comparison) -- namespacing by `run_id` alone would then hand GMAT's process-global
/// configuration manager the identical object name twice, exactly the collision this task exists
/// to fix. [`GMAT_EXECUTE_SEQ`] is the tie-breaker: unique per `execute()` **invocation**,
/// regardless of what `run_id` the caller passed. Sanitized to ASCII alphanumeric/`_` before use
/// (GMAT's own `Construct` enforces no character restriction at all -- see `binding::
/// materialize_gmat`'s own doc comment -- but a `run_id` containing e.g. `-` (a UUID) should never
/// produce a name some *later* GMAT operation, such as a script re-parse, would refuse under
/// GMAT's own `GmatStringUtil::IsValidName`).
fn gmat_execution_namespace(run_id: &str) -> String {
    let sanitized: String = run_id.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    let seq = GMAT_EXECUTE_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("{sanitized}_{seq}")
}

/// Construct `plan` through `crate::registry::ModelRegistry` (M10.3: never `binding::
/// materialize_gmat`/`materialize_constant_accel` directly -- see the module doc comment).
/// `model_id` for the registry's own construction-time labelling (distinct from `name_suffix`,
/// which becomes `ModelInfo.id`/output) is `sys.dynamics_model`, matching `ModelRegistry::
/// construct_native`/`construct_gmat`'s own documented convention. `gmat_ns` (M18.4, question
/// 127) is ignored for a `ConstantAccel` plan (the native path constructs no GMAT objects at all)
/// and threaded straight through to `construct_gmat`/`binding::materialize_gmat` for a `Gmat`
/// plan -- see that function's own doc comment for exactly what it namespaces.
#[allow(clippy::too_many_arguments)]
fn materialize_plan(gmat: &Gmat, plan: &BindingPlan, sys: &SystemDefinition, epoch_tai_ns: i64, with_stm: bool, accept_missing_stm_terms: bool, gmat_ns: &str, name_suffix: &str) -> Result<ModelHandle, DrmError> {
    match plan {
        BindingPlan::Gmat(spec) => {
            ModelRegistry::construct_gmat(gmat, spec, epoch_tai_ns, &sys.dynamics_model, gmat_ns, name_suffix, &sys.state_space_id, with_stm, accept_missing_stm_terms).map_err(DrmError::Model)
        }
        BindingPlan::ConstantAccel(spec) => Ok(ModelRegistry::construct_native(spec, epoch_tai_ns, &sys.dynamics_model, &sys.state_space_id)),
        // M22.1b: `AttitudeWheelsModel::new` needs the full resolved `StateSpace` (dimension and
        // per-component units), not just `sys.state_space_id` -- re-resolved here rather than
        // threaded through as an extra parameter (cheap, pure Rust, no I/O; `classify_binding`
        // already proved this resolves and validates cleanly for this instance before accepting
        // it, so this is not expected to fail in practice, but stays a real, typed `Result`
        // rather than an `.expect()`).
        BindingPlan::Attitude(spec) => {
            let resolved = crate::trajectory::resolve_state_space(sys).map_err(|e| DrmError::InvalidStateSpace { instance: sys.id.clone(), reason: e.to_string() })?;
            // M22.4: re-resolved the same "cheap, pure Rust, already proved at classify time"
            // way `resolve_state_space` immediately above is -- `None` for every fixture that
            // declares no wheel-torque-command port (see `binding::resolve_attitude_wheel_
            // command_input`'s own doc comment for the "provably inert" contract this preserves).
            let wheel_command = binding::resolve_attitude_wheel_command_input(sys, &sys.id)?;
            ModelRegistry::construct_attitude(spec, &resolved, wheel_command, epoch_tai_ns, &sys.dynamics_model).map_err(DrmError::Model)
        }
        // M22.2b: `resolve_sensor_output` re-resolves the declared codec/port pairing from `sys`
        // (cheap, pure Rust, no I/O) -- `classify_binding` already proved this resolves cleanly
        // for this instance before accepting it, mirroring the Attitude arm's own re-resolution
        // of `resolve_state_space` immediately above.
        BindingPlan::StarTracker(spec) => {
            let (codec, output_port) = binding::resolve_sensor_output(sys, &sys.id)?;
            ModelRegistry::construct_star_tracker(spec, codec, output_port, epoch_tai_ns, &sys.dynamics_model).map_err(DrmError::Model)
        }
        BindingPlan::Imu(spec) => {
            let (codec, output_port) = binding::resolve_sensor_output(sys, &sys.id)?;
            ModelRegistry::construct_imu(spec, codec, output_port, epoch_tai_ns, &sys.dynamics_model).map_err(DrmError::Model)
        }
        // M22.4: `resolve_controller_ports` re-resolves the declared three-codec/three-port
        // convention from `sys`, the same "already proved at classify time" pattern as every
        // other native binding kind above.
        BindingPlan::Controller(spec) => {
            let ports = binding::resolve_controller_ports(sys, &sys.id)?;
            ModelRegistry::construct_attitude_controller(spec, ports.star_codec, ports.imu_codec, ports.command_codec, epoch_tai_ns, &sys.dynamics_model).map_err(DrmError::Model)
        }
        // M25.1: `resolve_ground_ports` re-resolves the declared telemetry-in/telecommand-out
        // codec/port pair from `sys`, the same "already proved at classify time" pattern as
        // every other native binding kind above.
        BindingPlan::GroundStation(spec) => {
            let ports = binding::resolve_ground_ports(sys, &sys.id)?;
            ModelRegistry::construct_ground_station(spec, ports.tm_codec, ports.tm_port, ports.tc_codec, ports.tc_port, epoch_tai_ns, &sys.dynamics_model).map_err(DrmError::Model)
        }
    }
}

/// Re-bind `plan` at a fault or maneuver boundary, from the previous segment's own final
/// physical state (continuity of position -- see `fault`'s module doc comment; a maneuver
/// boundary's `state_si` additionally carries the post-burn velocity jump, computed by the
/// caller before this is called). Unlike [`materialize_plan`], a `ConstantAccel` plan here still
/// uses its own declared `x0_si` internally, but the caller (this module's boundary-splitting
/// loop) always uses the carried-over physical state, not `ModelHandle::x0_si`, as the next
/// segment's initial state -- see the loop below. `with_stm` is threaded through for the
/// covariance-plus-maneuver path (question 97's "Covariance across a burn" -- see the module
/// doc comment); the plain fault-only path always passes `false`, unchanged from before this
/// task. Re-binding is just another call to the same two `ModelRegistry` constructors with a
/// freshly rebuilt spec (`fault::rebind_gmat_spec_at_state`) -- there is no separate "mutate an
/// existing handle" API; see `crate::registry`'s own module doc comment for why.
#[allow(clippy::too_many_arguments)]
fn materialize_plan_at_boundary(
    gmat: &Gmat,
    plan: &BindingPlan,
    sys: &SystemDefinition,
    epoch_tai_ns: i64,
    state_si: &[f64],
    with_stm: bool,
    accept_missing_stm_terms: bool,
    gmat_ns: &str,
    name_suffix: &str,
) -> Result<ModelHandle, DrmError> {
    match plan {
        BindingPlan::Gmat(spec) => {
            // A `Gmat` plan's own carried-over state is always exactly 6-dimensional
            // (`gmat_sys::model::GmatModel` has no other physical shape) -- M21.3 (question
            // 141) only makes a `"native."`-dispatched instance's own width variable, so this
            // conversion's precondition is unchanged and unaffected by that task.
            let state_si_6: [f64; 6] = state_si.try_into().expect("a Gmat plan's own carried-over state is always 6-dimensional");
            let rebound = fault::rebind_gmat_spec_at_state(spec, state_si_6);
            ModelRegistry::construct_gmat(gmat, &rebound, epoch_tai_ns, &sys.dynamics_model, gmat_ns, name_suffix, &sys.state_space_id, with_stm, accept_missing_stm_terms).map_err(DrmError::Model)
        }
        // `state_si` (the previous segment's own carried-over physical state) is deliberately
        // unused here -- see this function's own doc comment: a `ConstantAccel` plan's fresh
        // handle re-reads its own declared `spec.x0_si` instead (the boundary-splitting loop
        // above, not this function, is what actually threads state continuity forward for a
        // native instance, via `ModelSpanState::x0`).
        BindingPlan::ConstantAccel(spec) => Ok(ModelRegistry::construct_native(spec, epoch_tai_ns, &sys.dynamics_model, &sys.state_space_id)),
        // M22.1b: same "state_si deliberately unused, the boundary loop's own span.x0 already
        // carries continuity forward" shape as the ConstantAccel arm immediately above -- a
        // freshly re-bound AttitudeWheelsModel's own `initial_state` (all-zero wheels, spec's
        // own q0/omega0) is likewise never what actually seeds the next span; only its
        // *behaviour* (inertia, wheel limits/availability/commanded torque, all possibly
        // changed by the fault that triggered this re-bind) matters here.
        BindingPlan::Attitude(spec) => {
            let resolved = crate::trajectory::resolve_state_space(sys).map_err(|e| DrmError::InvalidStateSpace { instance: sys.id.clone(), reason: e.to_string() })?;
            let wheel_command = binding::resolve_attitude_wheel_command_input(sys, &sys.id)?;
            ModelRegistry::construct_attitude(spec, &resolved, wheel_command, epoch_tai_ns, &sys.dynamics_model).map_err(DrmError::Model)
        }
        // M22.2b: same "state_si deliberately unused, the boundary loop's own span.x0 already
        // carries continuity forward" shape as the ConstantAccel/Attitude arms immediately
        // above -- a freshly re-bound StarTrackerModel/ImuModel's own declared initial state
        // (empty, or all-zero bias) is likewise never what actually seeds the next span; only
        // its *behaviour* (a fault-perturbed noise/bias-random-walk sigma) matters here.
        BindingPlan::StarTracker(spec) => {
            let (codec, output_port) = binding::resolve_sensor_output(sys, &sys.id)?;
            ModelRegistry::construct_star_tracker(spec, codec, output_port, epoch_tai_ns, &sys.dynamics_model).map_err(DrmError::Model)
        }
        BindingPlan::Imu(spec) => {
            let (codec, output_port) = binding::resolve_sensor_output(sys, &sys.id)?;
            ModelRegistry::construct_imu(spec, codec, output_port, epoch_tai_ns, &sys.dynamics_model).map_err(DrmError::Model)
        }
        // M22.4: same "state_si deliberately unused" shape -- a freshly re-bound
        // AttitudeControllerModel's own declared initial state (empty, `state_dim() == 0`) is
        // likewise never what seeds the next span; only its *behaviour* (a fault-perturbed
        // kp/kd gain) matters here.
        BindingPlan::Controller(spec) => {
            let ports = binding::resolve_controller_ports(sys, &sys.id)?;
            ModelRegistry::construct_attitude_controller(spec, ports.star_codec, ports.imu_codec, ports.command_codec, epoch_tai_ns, &sys.dynamics_model).map_err(DrmError::Model)
        }
        // M25.1: same "state_si deliberately unused" shape -- a freshly re-bound
        // GroundStationModel's own declared initial state (empty, `state_dim() == 0`) is
        // likewise never what seeds the next span; only its *behaviour* (a fault-perturbed site
        // position/elevation mask) matters here.
        BindingPlan::GroundStation(spec) => {
            let ports = binding::resolve_ground_ports(sys, &sys.id)?;
            ModelRegistry::construct_ground_station(spec, ports.tm_codec, ports.tm_port, ports.tc_codec, ports.tc_port, epoch_tai_ns, &sys.dynamics_model).map_err(DrmError::Model)
        }
    }
}

/// [`HeteroKernelError`] -> [`DrmError`], stringifying the same way `crate::schedule::
/// ScheduleError` already was mapped into [`DrmError::Schedule`]/[`DrmError::CovarianceHygiene`]
/// before this task (the underlying error types are not `Clone`/`'static`-simple enough to nest
/// here without another layer of boilerplate for no behavioural gain -- see `DrmError::
/// Schedule`'s own doc comment). [`HeteroKernelError::BasePeriod`] is folded into
/// [`DrmError::Schedule`] too: this executor always registers exactly one system per
/// `HeteroKernel` (one instance, one fault segment), so the GCD-derived base-period gate can
/// never actually disagree with that one system's own period -- see `HeteroKernel::run`'s own
/// doc comment for why the gate's failure arm is not reachable through a single-system
/// registration.
fn hetero_err_to_drm(e: HeteroKernelError) -> DrmError {
    match e {
        HeteroKernelError::BasePeriod(be) => DrmError::Schedule(be.to_string()),
        HeteroKernelError::Schedule(se) => DrmError::Schedule(se.to_string()),
        HeteroKernelError::CovarianceHygiene(ce) => DrmError::CovarianceHygiene(ce.to_string()),
    }
}

/// One boundary event this loop can split a run at -- a `FAULT_TARGET_KIND_DYNAMICS`/`_HARDWARE`/
/// `_SENSOR` fault (reconfigures the dynamics, state continuous), a `"maneuver"` `ScenarioEvent`
/// (jumps the velocity, dynamics configuration unchanged), or [`Boundary::SensorFaultEnd`] --
/// question 178 (R5.1a)'s own SECOND, executor-synthesized boundary at a windowed SENSOR fault's
/// own end epoch (`Fault.tai_ns + Fault.duration_ns`, only when `duration_ns > 0`), which restores
/// the star-tracker instance's spec to its exact pre-fault value (`fault::clear_sensor_fault`).
/// See [`run_shared_group`]'s own doc comment for how all of these are merged into one sorted
/// boundary list (M14.1: across every active `BINDING_KIND_MODEL` instance, not just one).
enum Boundary<'a> {
    Fault(&'a Fault),
    Maneuver(&'a ParsedManeuver),
    SensorFaultEnd(&'a Fault),
}
impl Boundary<'_> {
    fn tai_ns(&self) -> i64 {
        match self {
            Boundary::Fault(f) => f.tai_ns,
            Boundary::Maneuver(m) => m.tai_ns,
            // `Fault.duration_ns > 0` is `run_shared_group`'s own precondition for ever
            // constructing this variant at all -- see its own boundary-collection pass.
            Boundary::SensorFaultEnd(f) => f.tai_ns + f.duration_ns,
        }
    }
    fn id(&self) -> &str {
        match self {
            Boundary::Fault(f) => &f.id,
            Boundary::Maneuver(m) => &m.id,
            Boundary::SensorFaultEnd(f) => &f.id,
        }
    }
    /// M14.1 (question 109): which instance this boundary targets -- the shared multi-instance
    /// run ([`run_shared_group`]) needs this to know which currently-active model instance gets
    /// its plan changed (a fault) or its state dv-jumped (a maneuver) at this boundary, versus
    /// every other active instance, which simply continues (see [`run_shared_group`]'s own doc
    /// comment).
    fn instance(&self) -> &str {
        match self {
            Boundary::Fault(f) => &f.instance,
            Boundary::Maneuver(m) => &m.instance,
            Boundary::SensorFaultEnd(f) => &f.instance,
        }
    }
}

/// Add `dv` (declared in `axes`'s own basis order) to `state_si`'s velocity components,
/// building the V/N/B, R/I/C, or VVLH basis from `state_si`'s own position/velocity at the
/// burn epoch (`maneuver::dv_to_inertial`) -- the shared step both [`run_shared_group`] and
/// [`run_covariance_instance`] apply at a maneuver boundary. `dv` is a parameter (not always
/// `m.dv` directly) since a [`ExecutionErrorMode::Sampled`] run applies a Gates-perturbed dv,
/// not the bare commanded one -- see [`dv_to_apply`].
fn apply_dv_to_state(axes: av_cdm::pb::AxesKind, dv: [f64; 3], state_si: [f64; 6]) -> [f64; 6] {
    let r = [state_si[0], state_si[1], state_si[2]];
    let v = [state_si[3], state_si[4], state_si[5]];
    let dv = maneuver::dv_to_inertial(axes, dv, r, v);
    let mut new_state = state_si;
    new_state[3] += dv[0];
    new_state[4] += dv[1];
    new_state[5] += dv[2];
    new_state
}

/// Question 103: the dv to actually apply for `m`'s burn, selected by `mode` -- a thin wrapper
/// over [`maneuver::dv_to_apply`] (this crate's one place that branches on
/// [`ExecutionErrorMode`], shared by both [`run_shared_group`] and [`run_covariance_instance`]
/// so the two paths can never disagree about what a given mode means, closing the M11.4 bug
/// question 103 exists to fix -- see [`RunConfig::error_mode`]'s own doc comment). This wrapper
/// only converts `maneuver::SampledDv` into [`events::SampledManeuver`] (the two are
/// field-for-field identical; `maneuver` does not depend on `events`, so the conversion happens
/// here rather than there).
fn dv_to_apply(mode: ExecutionErrorMode, m: &ParsedManeuver, seeds: &BTreeMap<String, u64>) -> ([f64; 3], Option<events::SampledManeuver>) {
    let (applied_dv, sampled) = maneuver::dv_to_apply(mode, m, seeds);
    (applied_dv, sampled.map(|s| events::SampledManeuver { applied_dv: s.applied_dv, draws: s.draws }))
}

// ============================================================================================
// The shared kernel run (M14.1, `docs/open-questions.md` question 109)
// ============================================================================================

/// Append one span's own `Trajectory` (as [`HeteroKernel::run_with_ports`] returned it for one
/// registered system) onto that system's own running accumulators -- the pre-M14.1 per-instance
/// loop had this logic inline (once for the plain path, once for the covariance path); this is
/// the general form [`run_one_span`] applies uniformly to every model *and* container instance
/// in one shared span, and [`run_covariance_span`] still keeps its own copy unchanged (the
/// covariance path is untouched by this task -- see the module doc comment's "One shared kernel
/// run" section).
///
/// **Sample deduplication at the boundary.** Every span after the first starts at exactly the
/// epoch the previous one ended at, so naively concatenating would emit that instant twice.
/// Across a **fault** boundary (or an unaffected instance simply continuing through someone
/// else's boundary) the two samples are numerically identical (only the dynamics
/// *configuration* changed, if anything; position and velocity are continuous), so which of the
/// two is kept makes no difference -- `keep_previous_last_and_drop_incoming_first = true` (this
/// span's own first sample is dropped). Across a **maneuver** boundary the two samples are *not*
/// identical for the maneuver's own target instance -- the previous span's last sample is the
/// pre-burn state, this span's first is the post-burn one (a real velocity discontinuity,
/// question 97) -- so the previous span's already-appended last sample is popped instead,
/// keeping the post-burn state as the one sample recorded at that epoch
/// (`keep_previous_last_and_drop_incoming_first = false`). Either way exactly one sample per
/// epoch reaches the returned `Trajectory`, so `Interpolation::HermiteVelocity`'s
/// distinct-strictly-increasing-epoch contract (`crate::interpolate`, not owned by this task) is
/// never handed a zero-width interval.
///
/// **M15.1 (question 115): `segment_preceded_by_own_maneuver` records, per appended segment,
/// whether the boundary immediately before it was a maneuver applied to *this* instance** -- the
/// exact condition [`merge_adjacent_segments`] needs and nothing more. It is derived from
/// `keep_previous_last_and_drop_incoming_first` itself (`!keep_previous_last_and_drop_incoming_
/// first`) rather than threaded in as a second, independent flag: that parameter is already
/// `false` exactly when this boundary is a maneuver on this instance (see the paragraph above),
/// so re-deriving it here means the sample-dedup rule and the segment-merge eligibility rule can
/// never silently disagree about what boundary they are looking at. `sub.segments.len()` is
/// always `0` or `1` in practice (`crate::trajectory::build_trajectory` builds exactly one segment
/// per [`HeteroKernel`] sub-run), but this loops over however many there are rather than assuming
/// exactly one: only the *first* new segment was preceded by this span's own boundary, any
/// further one (were `sub.segments` ever to carry more than one) was not.
fn append_span_samples(sub: Trajectory, all_samples: &mut Vec<TrajectorySample>, all_segments: &mut Vec<TrajectorySegment>, segment_preceded_by_own_maneuver: &mut Vec<bool>, shell: &mut Option<Trajectory>, keep_previous_last_and_drop_incoming_first: bool) {
    if shell.is_none() {
        *shell = Some(Trajectory { samples: vec![], segments: vec![], ..sub.clone() });
    }
    let mut sub_samples = sub.samples;
    if !all_samples.is_empty() {
        if keep_previous_last_and_drop_incoming_first {
            sub_samples.remove(0);
        } else {
            all_samples.pop();
        }
    }
    all_samples.extend(sub_samples);
    let boundary_is_own_maneuver = !keep_previous_last_and_drop_incoming_first;
    for i in 0..sub.segments.len() {
        segment_preceded_by_own_maneuver.push(i == 0 && boundary_is_own_maneuver);
    }
    all_segments.extend(sub.segments);
}

/// M15.1 (`docs/open-questions.md` question 115): merge adjacent entries of one instance's own
/// `segments` list when they describe nothing that actually happened -- the decided rule is
/// "`dynamics_hash` equal AND no maneuver applied to that instance at the boundary between them".
/// `boundary_is_own_maneuver[i]` (aligned index-for-index with `segments`, `i == 0` meaningless
/// since there is no segment before the first one to merge with) is exactly [`append_span_samples`]'s
/// own `segment_preceded_by_own_maneuver` accumulator for this instance -- see this module's own
/// doc comment's "Segment merge across an unaffected boundary" section for why that reuse, rather
/// than a second flag, is the point. A merge keeps every field of the earlier segment except
/// `end_tai_ns` (extended to the later segment's own `end_tai_ns`) -- `name`/`dynamics_model`/
/// `dynamics_hash`/`dynamics_depth` are already identical between two segments this function
/// merges (that is what "mergeable" means), so there is nothing to reconcile between them.
fn merge_adjacent_segments(segments: Vec<TrajectorySegment>, boundary_is_own_maneuver: &[bool]) -> Vec<TrajectorySegment> {
    let mut merged: Vec<TrajectorySegment> = Vec::with_capacity(segments.len());
    for (i, seg) in segments.into_iter().enumerate() {
        let mergeable = i > 0 && !boundary_is_own_maneuver[i] && merged.last().is_some_and(|prev: &TrajectorySegment| prev.dynamics_hash == seg.dynamics_hash);
        if mergeable {
            merged.last_mut().expect("checked above via i > 0 implies a previous merged segment exists").end_tai_ns = seg.end_tai_ns;
        } else {
            merged.push(seg);
        }
    }
    merged
}

/// One `BINDING_KIND_MODEL` instance's mutable state, threaded across every boundary-bounded
/// span of the shared run [`run_shared_group`] drives. `handle` is `Some` only between
/// materialization and the next [`run_one_span`] call -- taken (moved into that span's own
/// [`HeteroKernel`]) the moment it is registered, and always refilled (by
/// [`materialize_plan_at_boundary`]) before the *next* span, so it is `None` only transiently
/// inside [`run_one_span`] itself.
struct ModelSpanState {
    period_ns: i64,
    cur_plan: BindingPlan,
    /// The instance's own current physical state, carried across span boundaries. Always
    /// length 6 for a `Gmat` plan; for a `ConstantAccel` plan, this instance's own honoured
    /// width -- 0 or 6, never a fixed constant (M21.3, `docs/open-questions.md` question 141).
    x0: Vec<f64>,
    handle: Option<ModelHandle>,
    all_samples: Vec<TrajectorySample>,
    all_segments: Vec<TrajectorySegment>,
    /// M15.1 (question 115): parallel to `all_segments` (same length, same index-for-index
    /// meaning) -- whether the boundary immediately before that segment was a maneuver applied to
    /// *this* instance, fed straight to [`merge_adjacent_segments`] at the end of
    /// [`run_shared_group`]. Appended by [`append_span_samples`] itself, never set directly here.
    segment_preceded_by_own_maneuver: Vec<bool>,
    shell: Option<Trajectory>,
    all_outputs: NamedOutputSeries,
    /// Every command this instance's own `step_with_ports` calls actually applied, across every
    /// span of this shared run (`docs/open-questions.md` question 130) -- drained from
    /// `HeteroKernel::applied_commands` at the end of each [`run_one_span`] call, the same way
    /// `all_outputs` is drained from `HeteroKernel::outputs`. Converted into
    /// `EVENT_KIND_PORT_COMMAND` `Event`s once the whole run finishes (`run_shared_group`'s own
    /// tail, mirroring how `all_outputs` itself is only ever turned into `RunProducts` scoring
    /// data once the run is done).
    applied_commands: Vec<crate::ports::AppliedPortCommand>,
    /// Every CDM `Measurement` this instance's own `step_with_ports` calls actually produced,
    /// across every span of this shared run (`docs/open-questions.md` question 173, M25.3) --
    /// drained from `HeteroKernel::measurements` at the end of each [`run_one_span`] call, the
    /// same way `applied_commands` above is drained from `HeteroKernel::applied_commands`.
    /// Folded straight into [`RunProducts::measurements`] once the whole run finishes (sorted by
    /// `(epoch_ns, measurement_id)` there, not here) -- unlike `applied_commands`, a measurement
    /// never becomes an `Event`: question 173's whole point is that it is its own CDM type.
    measurements: Vec<av_cdm::pb::Measurement>,
    /// Whether this instance's own `x0`/`handle` were produced by a maneuver's dv jump at the
    /// boundary that ends the *previous* span (i.e. whether the sample recorded at this span's
    /// own start is velocity-discontinuous with the previous span's last sample) -- see
    /// [`append_span_samples`]'s own doc comment. Never set for a container instance ([`ContainerSpanState`]
    /// has no analogous field): a `BINDING_KIND_CONTAINER` instance is never a maneuver's own
    /// target (`execute()` refuses that before this function is ever called), so its own
    /// boundary samples are always numerically identical across a split, the same as an
    /// unaffected model instance's.
    seg_start_is_post_maneuver: bool,
}

/// One `BINDING_KIND_CONTAINER` instance's mutable state, threaded across every span the same
/// way [`ModelSpanState`] is for a model instance -- except there is no physical state to carry
/// (`state_dim() == 0`) and no plan to ever rebind: the *same* live [`binding::ContainerModel`]
/// (one connection, one `next_sequence` counter) is simply re-erased into a fresh
/// [`BoxedModel`] for each new span's [`HeteroKernel`] (see [`erase_container`] and
/// [`binding::SharedContainerModel`]'s own doc comment). `error_slot` is how a `ContainerError`
/// raised deep inside a `step_with_ports` call (erased to `av_dynamics::ModelError` by the time
/// [`HeteroKernel::run_with_ports`] reports it) makes it back out as the *typed* error every
/// container test in this crate matches on -- see [`hetero_err_to_drm_shared`].
struct ContainerSpanState {
    period_ns: i64,
    model: Rc<binding::ContainerModel>,
    error_slot: Rc<RefCell<Option<ContainerError>>>,
    all_samples: Vec<TrajectorySample>,
    all_segments: Vec<TrajectorySegment>,
    /// M15.1 (question 115): see [`ModelSpanState::segment_preceded_by_own_maneuver`]'s own doc
    /// comment. Always ends up all-`false` for a container (`execute()` already refuses a
    /// maneuver naming a `BINDING_KIND_CONTAINER` instance before `run_shared_group` is ever
    /// called), but is threaded through [`append_span_samples`] the same generic way for both
    /// span kinds rather than special-cased away here.
    segment_preceded_by_own_maneuver: Vec<bool>,
    shell: Option<Trajectory>,
    all_outputs: NamedOutputSeries,
}

/// Erase `model` (a shared handle to one live [`binding::ContainerModel`]) into a
/// [`BoxedModel`] for [`HeteroKernel::register_system`], stashing any [`ContainerError`] this
/// particular box's `step_with_ports` call ever raises into `error_slot` *before* converting it
/// to the one [`ModelError`] shape a container protocol failure has always mapped to
/// (`ModelError::BindingTransport`, per that variant's own doc comment: "a container's lockstep
/// protocol failed to deliver a step") -- see [`hetero_err_to_drm_shared`] for how the typed
/// error is recovered from `error_slot` after [`HeteroKernel::run_with_ports`] returns `Err`.
fn erase_container(name: &str, model: Rc<binding::ContainerModel>, error_slot: Rc<RefCell<Option<ContainerError>>>) -> BoxedModel {
    av_dynamics::erase_with_id(name.to_string(), SharedContainerModel(model), move |model_id, err: ContainerError| {
        *error_slot.borrow_mut() = Some(err.clone());
        ModelError::BindingTransport { model_id, detail: err.to_string() }
    })
}

/// [`hetero_err_to_drm`]'s counterpart for the shared multi-instance run: identical for every
/// error shape except one -- a container instance's own `step_with_ports` failure, which
/// [`erase_container`]'s wrap closure already stashed (as the original, typed
/// [`ContainerError`]) into that instance's own `error_slot` before erasing it to
/// `ModelError::BindingTransport`. Recovering it here, rather than teaching the pre-M14.1
/// dedicated container loop's old direct-`ContainerError`-return shape some other way, is what
/// lets every `tests/drm_container.rs` test that matches on a specific `ContainerError` variant
/// (`SequenceMismatch`/`ReachedTaiMismatch`) keep passing unchanged even though the container
/// now steps through the *generic* `HeteroScheduler`/`ModelError` machinery instead of a
/// dedicated loop calling `ContainerModel::step_with_ports` directly.
fn hetero_err_to_drm_shared(e: HeteroKernelError, container_error_slots: &BTreeMap<String, Rc<RefCell<Option<ContainerError>>>>) -> DrmError {
    if let HeteroKernelError::Schedule(crate::schedule::HeteroScheduleError::Model(ModelError::BindingTransport { ref model_id, .. })) = e {
        if let Some(slot) = container_error_slots.get(model_id) {
            if let Some(err) = slot.borrow_mut().take() {
                return DrmError::ContainerProtocol { instance: model_id.clone(), source: err };
            }
        }
    }
    hetero_err_to_drm(e)
}

/// Run one boundary-bounded span `[seg_start, seg_end]` of the shared multi-instance kernel run:
/// register every currently active model instance's own materialized [`ModelHandle`] (taking it
/// out of `model_spans`) and every container instance's own live [`binding::ContainerModel`]
/// (re-erased, never re-`Bind`-ed) on one fresh [`HeteroKernel`], drive it with `router` so
/// `SosConfiguration.connections` actually deliver between them (`docs/open-questions.md`
/// question 108/109), and append every instance's own span result onto its own accumulators. A
/// fresh [`HeteroKernel`] every span (rather than one long-lived kernel across the whole run) is
/// required the moment *any* instance's own plan or state changes at a boundary --
/// [`HeteroKernel::register_system`] has no "replace this system's model" API -- and is harmless
/// for every other, unaffected instance for the same reason the pre-M14.1 single-instance
/// fault-rebuild already relied on: a fresh GMAT/native model, re-materialized from the exact
/// continuous physical state the previous span's own kernel produced, propagates forward
/// identically to a kernel that was never split at all (see [`run_shared_group`]'s own doc
/// comment for the "byte-identical, not merely assumed" scope this claim is limited to).
#[allow(clippy::too_many_arguments)]
fn run_one_span(
    seg_start: i64,
    seg_end: i64,
    model_spans: &mut BTreeMap<String, ModelSpanState>,
    container_spans: &mut BTreeMap<String, ContainerSpanState>,
    router: &mut crate::router::Router,
    output_period_ns: i64,
    sensor_fault_drains: &mut BTreeMap<String, av_dynamics::SensorFaultEffectDrain>,
) -> Result<(), DrmError> {
    sensor_fault_drains.clear();
    let mut kernel = HeteroKernel::new(output_period_ns);
    for (name, span) in model_spans.iter_mut() {
        let handle = span.handle.take().expect("materialized before this span (either the initial materialization or the previous boundary's rebuild)");
        kernel.register_system(name.clone(), span.period_ns, handle.into_boxed(name), seg_start, span.x0.to_vec());
    }
    let mut error_slots: BTreeMap<String, Rc<RefCell<Option<ContainerError>>>> = BTreeMap::new();
    for (name, span) in container_spans.iter() {
        error_slots.insert(name.clone(), span.error_slot.clone());
        kernel.register_system(name.clone(), span.period_ns, erase_container(name, span.model.clone(), span.error_slot.clone()), seg_start, vec![]);
    }

    let mut result = kernel.run_with_ports(seg_start, seg_end, router).map_err(|e| hetero_err_to_drm_shared(e, &error_slots))?;

    for (name, span) in model_spans.iter_mut() {
        let sub = result.remove(name).expect("registered above");
        if let Some((epochs, values)) = kernel.outputs(name) {
            for (oname, vals) in values {
                let entry = span.all_outputs.entry(oname.clone()).or_default();
                entry.0.extend_from_slice(epochs);
                entry.1.extend_from_slice(vals);
            }
        }
        // Question 130: drain whatever this span's own kernel run recorded this instance
        // applying -- see `ModelSpanState::applied_commands`'s own doc comment.
        if let Some(applied) = kernel.applied_commands(name) {
            span.applied_commands.extend_from_slice(applied);
        }
        // Question 173: same drain shape as `applied_commands` immediately above.
        if let Some(measured) = kernel.measurements(name) {
            span.measurements.extend_from_slice(measured);
        }
        // Question 178 (R5.1a): this instance's own SENSOR fault effect over the WHOLE span
        // this call just ran -- read here, while `kernel` (and the boxed model it owns) is
        // still alive, and handed back to the caller (`run_shared_group`) via `sensor_fault_
        // drains`, since `span.handle` itself is `None` for the entire duration of this call
        // (`span.handle.take()`, above) and stays that way until the caller re-materializes a
        // fresh handle at the next boundary -- there is no live handle left in `span` to drain
        // from once this function returns.
        if let Some(drain) = kernel.sensor_fault_effect(name) {
            sensor_fault_drains.insert(name.clone(), drain);
        }
        append_span_samples(sub, &mut span.all_samples, &mut span.all_segments, &mut span.segment_preceded_by_own_maneuver, &mut span.shell, !span.seg_start_is_post_maneuver);
    }
    for (name, span) in container_spans.iter_mut() {
        let sub = result.remove(name).expect("registered above");
        if let Some((epochs, values)) = kernel.outputs(name) {
            for (oname, vals) in values {
                let entry = span.all_outputs.entry(oname.clone()).or_default();
                entry.0.extend_from_slice(epochs);
                entry.1.extend_from_slice(vals);
            }
        }
        // A container instance is never a maneuver's own target (refused before this function
        // is ever reached), so its two samples at any boundary are always numerically
        // identical -- always drop this span's own incoming first sample, never the previous
        // span's last (see ModelSpanState::seg_start_is_post_maneuver's own doc comment).
        append_span_samples(sub, &mut span.all_samples, &mut span.all_segments, &mut span.segment_preceded_by_own_maneuver, &mut span.shell, true);
    }
    Ok(())
}

/// **M14.1, `docs/open-questions.md` question 109's decision:** every non-covariance instance
/// of one `SosConfiguration` -- every `BINDING_KIND_MODEL` instance (GMAT or native) and every
/// `BINDING_KIND_CONTAINER` instance -- driven through **one** shared [`HeteroKernel::
/// run_with_ports`] call per boundary-bounded span, with `router` actually delivering
/// `SosConfiguration.connections` between them. Before this task, `execute()` ran every
/// instance on its own isolated per-instance loop (a dedicated function per binding kind), so
/// the router validated wiring at load (`crate::router::Router::build`) but nothing a
/// `step_with_ports` call ever emitted reached
/// another instance's own `Inbox` -- this function is the collapse that closes that gap. **The
/// covariance path is untouched**: [`run_covariance_instance`] still runs on its own
/// per-instance loop (see `execute`'s own module doc comment for why, and
/// [`build_run_provenance`] for how that limitation is recorded).
///
/// ## Faults and maneuvers: the split now applies to the whole kernel run
///
/// Before this task, a `FAULT_TARGET_KIND_DYNAMICS` fault or a maneuver split *one* instance's
/// own isolated loop into boundary-bounded segments (see [`append_span_samples`]'s own doc
/// comment for the sample-deduplication rule that mechanism relied on, unchanged here). Now the
/// split applies to the **whole shared run**: `boundaries` below is the union of every fault
/// and maneuver naming *any* `BINDING_KIND_MODEL` instance, plus (M15.3 question 118, moved to
/// HARDWARE by M16.2 question 120) every `FAULT_TARGET_KIND_HARDWARE`/`"power_cycle"` fault
/// naming a `BINDING_KIND_CONTAINER` instance (a container instance can never be a *maneuver's*
/// own target, and no other fault shape naming a container -- `execute()` already refuses every
/// other DYNAMICS or HARDWARE fault naming one, before this function is ever called), and at
/// every such boundary **every currently active model
/// instance** is re-materialized from its own last physical state (`materialize_plan_at_boundary`)
/// -- the one this boundary actually targets gets its plan changed (a fault,
/// `fault::apply_dynamics_fault`) or its state dv-jumped (a maneuver, [`apply_dv_to_state`]);
/// every other active model instance is re-materialized from its own *unchanged* plan and its own
/// continuously-sampled state, exactly the same call shape a maneuver boundary already uses for
/// its own one target instance (an unchanged plan, a new state) -- this function only generalizes
/// that same, already-accepted mechanism from one instance to every active one. Every container
/// instance's own live [`binding::ContainerModel`] simply moves into the next span's kernel
/// unrebuilt (see [`ContainerSpanState`]'s own doc comment) -- a container instance never has a
/// plan to change or physical state to re-materialize from, even at a boundary that targets *it*:
/// a power-cycle boundary calls `ContainerModel::reset` (below, in the per-boundary loop) instead
/// of anything `materialize_plan_at_boundary`-shaped, and the same live connection continues into
/// the next span either way.
///
/// **Scope of the "byte-identical" claim.** Every DRM this task's required tests and every
/// existing golden exercise (`drm_matches_the_golden_arc`, the fault-split golden, both maneuver
/// goldens) declares exactly **one** `BINDING_KIND_MODEL` instance, so for all of them this
/// function's boundary list, by construction, only ever contains boundaries targeting that one
/// instance -- "re-materialize every other active instance too" is vacuously a no-op (there is
/// no other active instance), so this function's own span-splitting reduces to *exactly* the
/// sequence of `HeteroKernel`/model constructions the pre-M14.1 per-instance loop already
/// performed for that one instance, which is what makes the byte-identical assertions in
/// `tests/drm_executor.rs`/`tests/drm_maneuver.rs` a real proof, not merely a claim.
///
/// **M14.4 measured the N > 1 case directly (`tests/restart_invariance.rs`) rather than leaving
/// it asserted only by symmetry.** The physical claim holds: an unaffected instance's own
/// `Trajectory.samples` (position/velocity at every output tick) are byte-identical whether it
/// runs alone or alongside a target instance's own fault/maneuver boundaries, for both a second
/// `BINDING_KIND_MODEL` instance and a `BINDING_KIND_CONTAINER` instance. `Trajectory.segments`
/// is a different matter: every currently active instance -- including one this boundary does not
/// target -- is re-materialized here (see the "Faults and maneuvers" section above), which always
/// produces a *new* `TrajectorySegment` entry for it, so an unaffected instance's own `all_segments`
/// (before the merge below) ends up with one entry per boundary in the run instead of the single
/// entry its own dynamics configuration (unchanged throughout) would suggest. **M15.1 (question
/// 115) merges this back down**: [`merge_adjacent_segments`], run once per instance at the end of
/// this function, collapses adjacent entries sharing an identical `dynamics_hash` where the
/// boundary between them was not a maneuver on that instance -- restoring full segment
/// byte-identity for exactly the case M14.4 found (a native-bound bystander, or a container,
/// beside a faulted-then-maneuvered target: `tests/restart_invariance.rs`'s two required tests now
/// assert `segments` equal, not merely `samples`) while never merging across a real
/// reconfiguration (a fault: `dynamics_hash` differs, verified directly by `tests/drm_executor.rs
/// ::a_dynamics_fault_splits_the_run_into_two_segments_with_continuous_state`) or a real state
/// discontinuity (a maneuver on the instance itself: kept regardless of `dynamics_hash`, proven by
/// `tests/segment_merge.rs`, since a maneuver never changes `cur_plan` and so *can* leave the hash
/// unchanged either side of its own boundary). See this module's own doc comment's "Segment merge
/// across an unaffected boundary" section for the full account, including the GMAT-bound bystander
/// case M18.4 (question 127) closed -- `binding::gmat_settings` no longer hashes a GMAT-bound
/// instance's own instantaneous Cartesian state, so its `dynamics_hash` is genuinely unchanged
/// across a boundary that never touched its own dynamics, and this merge now collapses it exactly
/// like a native-bound bystander already did.
///
/// ## Container period vs. the trajectory's own output grid (M14.4: restriction lifted)
///
/// **Through M14.3, a container instance's own `period_ns` had to evenly divide
/// `output_period_ns`** (`DrmError::ContainerPeriodExceedsSampleInterval` otherwise) -- a
/// genuinely new restriction the pre-M14.1 dedicated container loop never had (it sampled at the
/// container's own native period only, entirely decoupled from `DrmOptions.sample_interval_s`).
/// It existed because the shared [`HeteroKernel`] samples **every** registered system at
/// `output_period_ns` and, through M14.3, unconditionally Hermite-interpolated a system whose own
/// period is coarser (`crate::schedule::SampleKind::Between`) -- `crate::interpolate::
/// hermite_velocity` requires at least 6 state components, while a container's own `state_dim()
/// == 0`, so that path would panic the moment an output tick fell strictly between two of a
/// coarser container's own native steps.
///
/// **M14.4 lifts it: a container has no state to interpolate, so ADR-005 sec 3's "discrete modes,
/// counters | zero-order hold" rule applies instead of Hermite interpolation, and there is no
/// restriction on the container period left beyond the one every instance already has** --
/// `output_period_ns` and every instance's own `period_ns` must be integer multiples of the
/// GCD-derived base period (`crate::clock::base_period_ns`/`check_integer_multiples`, ADR-005
/// sec 2, enforced generically by `HeteroKernel::run_with_ports`'s own `base_period_ns` gate,
/// [`HeteroKernelError::BasePeriod`]) -- nothing container-specific left to check here.
/// `HeteroKernel::run_with_ports` (see that method's own doc comment) now routes a
/// zero-dimensional system through `crate::schedule::HeteroScheduler::sample_held` instead of
/// `sample`: at an output tick landing exactly on one of its own native steps, `HoldKind::Fresh`;
/// strictly between two (or before its first), `HoldKind::Held` -- the last delivered value,
/// repeated flat, never blended. **A held sample is never indistinguishable from a fresh one**:
/// **M15.2 (question 116)** carries this straight onto the wire as `TrajectorySample.kind`
/// (`av_cdm::pb::SampleKind::Native`/`Held`, set by `HeteroKernel::run_with_ports` itself) --
/// this executor no longer needs to record held epochs out of band at all, so the M14.4-era
/// `Trajectory.provenance.attributes["held_sample_tai_ns"]` side channel (and
/// `ContainerSpanState::held_epochs`, which fed it) are deleted; a consumer reads `kind` straight
/// off the sample instead of cross-referencing a comma-joined attribute keyed by TAI ns.
/// `DrmError::ContainerPeriodNotOnGrid` (the scenario's own duration must still be an exact
/// multiple of the container's period) is unrelated to this and is unchanged.
///
/// [`run_shared_group`]'s own return shape (clippy::type_complexity): every instance's finished
/// `Trajectory`, every real `Event` the run produced, every instance's own `NamedOutputSeries`,
/// and every container instance's own `LockstepBindResponse.binding_hash` (for `execute()`'s
/// caller to attach to `Trajectory.provenance.attributes`, question 107's "binding_hash into
/// provenance"). M15.2 drops this tuple's old fifth member (every container instance's own held
/// epochs, M14.4) -- see this function's own doc comment's "Container period vs. the trajectory's
/// own output grid" section for why that side channel is gone.
type SharedGroupResult = (BTreeMap<String, Trajectory>, Vec<Event>, BTreeMap<String, NamedOutputSeries>, BTreeMap<String, String>, Vec<av_cdm::pb::Measurement>);

/// Question 178 (R5.1a): drain `handle`'s own accumulated SENSOR fault effect (if any) and fold
/// it into `sensor_fault_totals`, attributed to whichever fault id `active_sensor_fault` says is
/// currently installed on `name` -- a no-op when `name` has no active SENSOR fault, `handle` is
/// `None`, or the drain itself is `None` (nothing affected since the last drain). Called by
/// [`run_shared_group`] at every point one instance's own handle is about to be discarded and
/// replaced with a freshly re-materialized one -- see that function's own doc comment for why
/// this must happen at every boundary, not only the two SENSOR-fault-specific ones.
/// Fold `name`'s own per-span SENSOR fault drain (if any -- see [`run_one_span`]'s own doc
/// comment for why this is collected from `kernel.sensor_fault_effect` DURING the span rather
/// than from `ModelSpanState::handle` afterward, which is always `None` by the time a span
/// finishes) into `sensor_fault_totals`, attributed to whichever fault id [`active_sensor_fault`]
/// says is currently installed on `name`.
fn fold_sensor_fault_span_drain(name: &str, drain: Option<av_dynamics::SensorFaultEffectDrain>, active_sensor_fault: &BTreeMap<String, String>, sensor_fault_totals: &mut BTreeMap<String, (Option<i64>, u64)>) {
    let Some(fault_id) = active_sensor_fault.get(name) else { return };
    let Some(drain) = drain else { return };
    let entry = sensor_fault_totals.entry(fault_id.clone()).or_insert((None, 0));
    entry.1 += drain.frames_affected;
    if drain.frames_affected > 0 {
        entry.0 = Some(entry.0.map_or(drain.first_effect_tai_ns, |e| e.min(drain.first_effect_tai_ns)));
    }
}

#[allow(clippy::too_many_arguments)]
fn run_shared_group(
    gmat: &Gmat,
    plans: &BTreeMap<String, (BindingPlan, i64)>,
    container_plans: &BTreeMap<String, (binding::ContainerSpec, i64)>,
    instances_by_name: &BTreeMap<String, &SystemInstance>,
    systems: &BTreeMap<String, SystemDefinition>,
    system_hashes: &BTreeMap<String, String>,
    scenario: &Scenario,
    options: &av_cdm::pb::DrmOptions,
    output_period_ns: i64,
    maneuvers: &[ParsedManeuver],
    commands: &[command::ParsedCommand],
    error_mode: ExecutionErrorMode,
    sos_hash: &str,
    run_id: &str,
    gmat_ns: &str,
    router: &mut crate::router::Router,
    replay_targets: &std::collections::BTreeSet<String>,
    replay_log: Option<&pb::PortTrafficLog>,
) -> Result<SharedGroupResult, DrmError> {
    // M25.4b: `replay_targets` non-empty implies `replay_log` is `Some` -- `execute()`'s own
    // resolution of `replay_targets` (Pass 1's own tail) only ever produces a non-empty set when
    // `cfg.replay` was `Some`, in which case `replay_log` was already read by `replay::
    // verify_and_load` before this function was ever called. An empty `replay_targets` with
    // `replay_log` still `None` is exactly the "no replay requested" case every existing test in
    // this crate exercises.
    debug_assert!(replay_targets.is_empty() || replay_log.is_some(), "run_shared_group: replay_targets is non-empty but replay_log is None -- execute() should never construct this combination");

    let mut model_spans: BTreeMap<String, ModelSpanState> = BTreeMap::new();
    for (name, (plan, period_ns)) in plans {
        let instance = instances_by_name[name];
        let sys = systems.get(&instance.system_id).expect("validated in pass 1");
        let initial = materialize_plan(gmat, plan, sys, scenario.start_tai_ns, /* with_stm */ false, options.accept_missing_stm_terms, gmat_ns, &format!("{name}_0"))?;
        // M25.4b: this instance's construction is replaced, wholesale, with a replay binding
        // that plays its own recorded OUT frames back -- see `crate::drm::replay`'s own module
        // doc comment. `initial.describe()`/`.state_dim()` (read inside `wrap_replay`, BEFORE
        // this substitution) are exactly what the real, non-replayed `initial` above would have
        // reported, so `TrajectorySegment.dynamics_model`/`.dynamics_hash`/`.dynamics_depth` for
        // this instance still come out identical to a non-replayed run's -- only `step`/
        // `step_with_ports`'s own behaviour changes.
        let initial = if replay_targets.contains(name) {
            ModelRegistry::wrap_replay(initial, name, replay_log.expect("checked by the debug_assert! above"))
        } else {
            initial
        };
        model_spans.insert(
            name.clone(),
            ModelSpanState {
                period_ns: *period_ns,
                cur_plan: plan.clone(),
                // `.clone()`, not a move: `x0_si` is now `Vec<f64>` (M21.3), so moving it out
                // here would leave `initial` only partially valid for the `Some(initial)` move
                // just below -- the array it replaced was `Copy`, so this needed no clone before.
                x0: initial.x0_si.clone(),
                handle: Some(initial),
                all_samples: Vec::new(),
                all_segments: Vec::new(),
                segment_preceded_by_own_maneuver: Vec::new(),
                shell: None,
                all_outputs: BTreeMap::new(),
                applied_commands: Vec::new(),
                measurements: Vec::new(),
                seg_start_is_post_maneuver: false,
            },
        );
    }

    let mut container_spans: BTreeMap<String, ContainerSpanState> = BTreeMap::new();
    let mut container_binding_hashes: BTreeMap<String, String> = BTreeMap::new();
    for (name, (spec, period_ns)) in container_plans {
        let instance = instances_by_name[name];
        let sys = systems.get(&instance.system_id).expect("validated in pass 1");
        // M14.4: the old `output_period_ns % period_ns == 0` check (`DrmError::
        // ContainerPeriodExceedsSampleInterval`) is gone -- see this function's own doc
        // comment's "Container period vs. the trajectory's own output grid" section. The only
        // restriction left on a container's own period is the one every instance already has,
        // enforced generically by `run_one_span`'s own `HeteroKernel::run_with_ports` call
        // (`base_period_ns`'s GCD-integer-multiple gate).
        let duration_ns = scenario.end_tai_ns - scenario.start_tai_ns;
        if duration_ns % period_ns != 0 {
            return Err(DrmError::ContainerPeriodNotOnGrid { instance: name.clone(), period_ns: *period_ns, duration_ns });
        }

        if replay_targets.contains(name) {
            // M25.4b: replaying a BINDING_KIND_CONTAINER instance means never dialing it at all
            // -- the whole point is running Docker-free (`crate::drm::replay`'s own module doc
            // comment). Registered as a `ModelSpanState` (not a `ContainerSpanState`), through
            // the identical shared `HeteroKernel`/`Router` machinery every `BINDING_KIND_MODEL`
            // instance already uses -- see `crate::registry::ModelRegistry::
            // construct_replay_container`'s own doc comment for the synthetic `ModelInfo` this
            // builds (a disclosed difference from the original run's own real container
            // `ModelInfo`, which only a live `Bind` response could ever supply) and this
            // function's own caller (`execute()`'s `ContainerFaultsOrManeuversNotSupported`
            // check, before this function is ever reached) for why `cur_plan` below is never
            // actually read: a container instance -- replayed or not -- can never be a
            // fault/maneuver boundary's own target.
            let handle = ModelRegistry::construct_replay_container(name, &sys.dynamics_model, &sys.state_space_id, scenario.start_tai_ns, replay_log.expect("checked by the debug_assert! above"));
            model_spans.insert(
                name.clone(),
                ModelSpanState {
                    period_ns: *period_ns,
                    cur_plan: BindingPlan::ConstantAccel(binding::ConstantAccelSpec::default()),
                    x0: Vec::new(),
                    handle: Some(handle),
                    all_samples: Vec::new(),
                    all_segments: Vec::new(),
                    segment_preceded_by_own_maneuver: Vec::new(),
                    shell: None,
                    all_outputs: BTreeMap::new(),
                    applied_commands: Vec::new(),
                    measurements: Vec::new(),
                    seg_start_is_post_maneuver: false,
                },
            );
            continue;
        }

        let seed = *scenario.seeds.get(&spec.seed_key).ok_or_else(|| DrmError::UnknownContainerSeed { instance: name.clone(), seed_key: spec.seed_key.clone() })?;
        let bind_parameters: BTreeMap<String, String> = binding::effective_parameters(sys, instance)
            .into_iter()
            .filter(|(pname, _)| !pname.starts_with("container.") && !pname.starts_with("output."))
            .map(|(pname, p)| (pname, if p.string_value.is_empty() { p.value.to_string() } else { p.string_value.clone() }))
            .collect();
        let materialized = binding::materialize_container(spec, sys, name, run_id, scenario.start_tai_ns, *period_ns, seed, &bind_parameters)?;
        // A container's own epoch is simply the caller's own `epoch_tai_ns` argument, exactly
        // like a model instance's (`binding`'s module doc comment's "Epoch" section, question
        // 96) -- every span below registers containers at `seg_start`, never this recorded
        // value again, so this is only a sanity check that `materialize_container` never
        // silently returns a different epoch than the one it was asked for.
        debug_assert_eq!(materialized.t0_tai_ns, scenario.start_tai_ns);
        container_binding_hashes.insert(name.clone(), materialized.model.binding_hash.clone());
        container_spans.insert(
            name.clone(),
            ContainerSpanState {
                period_ns: *period_ns,
                model: Rc::new(materialized.model),
                error_slot: Rc::new(RefCell::new(None)),
                all_samples: Vec::new(),
                all_segments: Vec::new(),
                segment_preceded_by_own_maneuver: Vec::new(),
                shell: None,
                all_outputs: BTreeMap::new(),
            },
        );
    }

    let mut boundaries: Vec<Boundary> = Vec::new();
    for f in &scenario.faults {
        if f.target_kind == FaultTargetKind::Dynamics as i32 && plans.contains_key(&f.instance) {
            boundaries.push(Boundary::Fault(f));
        } else if fault::is_container_power_cycle(f) && container_plans.contains_key(&f.instance) {
            // M15.3 (question 118), moved off DYNAMICS onto HARDWARE by M16.2 (question 120):
            // the one boundary shape a container instance *does* participate in -- see this
            // function's own doc comment's "Faults and maneuvers" section (above) and `fault`'s
            // own module doc comment's "Container power-cycle (HARDWARE)" section for why
            // HARDWARE/"power_cycle" is what a container-targeting fault looks like now.
            // `execute()`'s own load-time validation (this function's caller) already refuses
            // every *other* DYNAMICS or HARDWARE fault naming a container instance before
            // `run_shared_group` is ever called, using the identical `fault::
            // is_container_power_cycle` predicate, so the two can never disagree about which
            // faults reach here.
            boundaries.push(Boundary::Fault(f));
        } else if f.target_kind == FaultTargetKind::Sensor as i32 && plans.contains_key(&f.instance) {
            // Question 178 (R5.1a): a SENSOR fault naming a star-tracker instance is the third
            // boundary shape -- `execute()`'s own load-time validation already refused any other
            // SENSOR shape (an IMU instance, a non-sensor instance, an unknown kind, an
            // off-grid epoch, an overlapping window) before this function is ever called, so
            // every SENSOR fault reaching here is known-good. `duration_ns > 0` additionally
            // gets a SECOND, synthesized boundary at its own window end (`Boundary::
            // SensorFaultEnd`) -- `duration_ns == 0` (persistent to end of run) gets only the
            // one, exactly like a persistent PORT fault needs no second Router-side bookkeeping
            // either.
            boundaries.push(Boundary::Fault(f));
            if f.duration_ns > 0 {
                boundaries.push(Boundary::SensorFaultEnd(f));
            }
        }
    }
    for m in maneuvers {
        if plans.contains_key(&m.instance) {
            boundaries.push(Boundary::Maneuver(m));
        }
    }
    boundaries.sort_by_key(|b| (b.tai_ns(), b.id().to_string()));

    let t0 = scenario.start_tai_ns;
    let run_end_tai_ns = scenario.end_tai_ns;
    let mut seg_start = t0;
    let mut all_events: Vec<Event> = Vec::new();
    // Question 173 (M25.3): every CDM `Measurement` any instance in this shared group produced,
    // across every span -- folded in from each `ModelSpanState::measurements` in the same loop
    // that already walks `model_spans` for events/trajectories, below.
    let mut all_measurements: Vec<av_cdm::pb::Measurement> = Vec::new();
    // Question 178 (R5.1a): the currently-installed SENSOR fault id on each star-tracker
    // instance (at most one at a time -- `fault::validate_no_overlapping_sensor_fault_windows`),
    // and the running (first-effect epoch, frames-affected) total drained from that instance's
    // own model at every boundary it survives -- summed here, not read once at the end like
    // PORT's `Router::take_applied_port_faults`, because a fresh `StarTrackerModel` is
    // constructed at every boundary in this shared run, targeted at this instance or not
    // (question 115/116's own "re-segmented at every boundary" behaviour discards the model's
    // own internal counter each time -- see `crate::drm::sensors`'s own module doc comment).
    let mut active_sensor_fault: BTreeMap<String, String> = BTreeMap::new();
    let mut sensor_fault_totals: BTreeMap<String, (Option<i64>, u64)> = BTreeMap::new();
    // Question 178 (R5.1a): scratch space `run_one_span` fills in, per call, with whatever it
    // drained from `kernel.sensor_fault_effect` for that one span -- see `run_one_span`'s own
    // doc comment for why this cannot be read from `ModelSpanState::handle` afterward.
    let mut sensor_fault_span_drains: BTreeMap<String, av_dynamics::SensorFaultEffectDrain> = BTreeMap::new();

    // M25.2 (`docs/sil-plan.md`'s M25 milestone: "DRM command events become CDM `Command`s,
    // framed as CCSDS telecommands, delivered through the router with the link model"). See
    // `crate::drm::command`'s own module doc comment for the full account of what happens where;
    // this is where PROPOSED/CHECKED/AUTHORIZED are synthesized (`command::propose_check_
    // authorize`) and DISPATCHED actually happens, once per command, before the main boundary
    // loop below (a command dispatch is not itself a re-materialization boundary -- it changes no
    // dynamics configuration, only hands a message to the router, exactly like a SIGNAL emission
    // already does every step for `ConstantAccelModel::emit`).
    //
    // `seq_map`/`id_to_seq`: the numeric CCSDS `sequence_count` this task's own ack-correlation
    // convention uses (`crate::drm::command`'s own doc comment, "Scope disclosed, not hidden") --
    // built once, here, from the same deterministic `(tai_ns, id)` order `commands` was sorted
    // into by `execute()`'s own caller.
    let seq_map = command::assign_sequence_numbers(commands)?;
    let id_to_seq: BTreeMap<String, u16> = seq_map.iter().map(|(seq, c)| (c.id.clone(), *seq)).collect();
    // `(target instance, target field) -> &ParsedCommand`, consulted by the applied-commands
    // drain below (mirrors `contact_event`'s own reserved-port dispatch) to build the ACKED
    // event once the target instance's own `consume_framed` path reports it actually applied the
    // value -- see this function's own doc comment further down, where it is read.
    let commands_by_target_field: BTreeMap<(String, String), &command::ParsedCommand> = commands.iter().map(|c| ((c.instance.clone(), c.field.clone()), c)).collect();

    for cmd in commands {
        let target_instance = instances_by_name.get(cmd.instance.as_str()).expect("validated by execute()'s own caller: every ParsedCommand.instance names a real SosConfiguration instance");
        let target_sys = systems.get(&target_instance.system_id).expect("validated in pass 1");
        let target_hash = system_hashes.get(&target_instance.system_id).expect("verified above");
        let (_proposed_command, transition_events) = command::propose_check_authorize(cmd, t0, events::event_provenance(sos_hash, &scenario.data_pack_hash, run_id, target_hash, &target_sys.id));
        all_events.extend(transition_events);

        if cmd.tai_ns <= t0 || cmd.tai_ns >= run_end_tai_ns {
            // Outside the executed span -- declared (PROPOSED/CHECKED/AUTHORIZED above) but
            // never dispatched, the same "the DRM's own scenario window is the authority on what
            // 'outside the run' means" rule the boundary loop below already applies to a fault/
            // maneuver landing outside `(seg_start, run_end_tai_ns)`.
            continue;
        }
        // The target's own already-resolved `consume_framed_codec` (`classify_binding`'s own
        // `ModelKind::Native` arm) -- reusing it, rather than re-deriving a codec here, is what
        // *guarantees* the APID this dispatch encodes with matches what the target's own decode
        // expects; there is no second, independently-maintained copy of that pairing to drift.
        // M25.2b: a `BindingPlan::Gmat` target (`GmatSystemSpec::consume_framed`,
        // `crate::drm::gmat_command::GmatFramedCommandModel`) is now an equally valid dispatch
        // target -- resolved the identical way (`PacketField.target`, question 149), so this
        // dispatch mechanism needed no other change to support it (only this match arm).
        let codec = match model_spans.get(cmd.instance.as_str()).map(|s| &s.cur_plan) {
            Some(BindingPlan::ConstantAccel(spec)) => spec.consume_framed_codec.clone(),
            Some(BindingPlan::Gmat(spec)) => spec.consume_framed.as_ref().map(|r| Box::new(r.codec.clone())),
            _ => None,
        };
        let Some(codec) = codec else {
            return Err(DrmError::CommandTargetNotFramedConsumer { id: cmd.id.clone(), instance: cmd.instance.clone() });
        };
        let seq = *id_to_seq.get(&cmd.id).expect("id_to_seq was built from the same `commands` slice this loop iterates");
        let mut values = BTreeMap::new();
        values.insert("value".to_string(), crate::codec::FieldValue::Numeric(cmd.value));
        let payload = crate::codec::encode_packet(&codec, seq, &[], &values).expect(
            "consume_framed_codec was resolved by classify_binding's own resolve_constant_accel_command_port, which already requires a \"value\" field wide enough for a Numeric value -- encode_packet can only fail for a missing/mistyped/out-of-range field, none of which can happen here",
        );
        let mut dispatch_outbox = av_dynamics::Outbox::new();
        dispatch_outbox.push(command::COMMAND_DISPATCH_PORT.to_string(), cmd.tai_ns, payload);
        // Reuse `crate::router::Router::deliver`/its own latency model -- never reimplemented
        // here (`docs/sil-plan.md`'s own "reuse ports.rs's Router and its latency" instruction).
        // `router` already validated (`Router::build`, `execute()`'s own Pass 0) that `cmd.from`
        // declares a `PORT_KIND_FRAMED`/`PORT_DIRECTION_OUT` port literally named `"cmd_out"`
        // (`command::COMMAND_DISPATCH_PORT`) connected to the target's own `consume_framed_port`
        // -- a DRM that does not declare that connection simply drops this message, same as any
        // other unconnected port (`crate::router`'s own module doc comment).
        router.deliver(&cmd.from, cmd.tai_ns, dispatch_outbox);
        let from_instance = instances_by_name.get(cmd.from.as_str()).expect("validated by execute()'s own caller");
        let from_sys = systems.get(&from_instance.system_id).expect("validated in pass 1");
        let from_hash = system_hashes.get(&from_instance.system_id).expect("verified above");
        all_events.push(command::dispatched_event(cmd, cmd.tai_ns, events::event_provenance(sos_hash, &scenario.data_pack_hash, run_id, from_hash, &from_sys.id)));
    }

    for b in &boundaries {
        let boundary = b.tai_ns();
        if boundary <= seg_start || boundary >= run_end_tai_ns {
            // Outside the executed span -- declared but has no effect, the same "the DRM's own
            // scenario window is the authority on what 'outside the run' means" rule the
            // pre-M14.1 per-instance loop already applied.
            continue;
        }
        run_one_span(seg_start, boundary, &mut model_spans, &mut container_spans, router, output_period_ns, &mut sensor_fault_span_drains)?;
        // Question 178 (R5.1a): fold whatever this just-finished span contributed, attributed
        // by `active_sensor_fault` as it stood BEFORE this boundary's own updates below (i.e.
        // "who was under fault during [seg_start, boundary)", the span that just ran).
        for (name, drain) in std::mem::take(&mut sensor_fault_span_drains) {
            fold_sensor_fault_span_drain(&name, Some(drain), &active_sensor_fault, &mut sensor_fault_totals);
        }

        let target = b.instance().to_string();
        for (name, span) in model_spans.iter_mut() {
            let instance = instances_by_name[name];
            let sys = systems.get(&instance.system_id).expect("validated in pass 1");
            let sys_hash = system_hashes.get(&instance.system_id).expect("verified above");
            // M21.3 (question 141): the carried-over state is a `Vec<f64>` now, not a fixed
            // `[f64; 6]` -- a `"native."`-dispatched instance's own width is no longer
            // guaranteed to be 6 (an empty declared state space is a legitimate `dim == 0`
            // shape). No truncation/conversion here: only `Boundary::Maneuver`'s own dv-jump
            // arm below actually needs exactly 6 components, and it checks that explicitly.
            let last_state: Vec<f64> = span.all_samples.last().expect("run_one_span always appends >= 1 sample").mean.clone();

            if *name == target {
                match b {
                    // Question 178 (R5.1a): a SENSOR fault installs its declared effect exactly
                    // the way a DYNAMICS fault installs a parameter change -- `apply_sensor_
                    // fault` instead of `apply_dynamics_fault` -- but its own `EVENT_KIND_FAULT`
                    // event is NOT emitted here, unlike DYNAMICS: "at the epoch of its FIRST
                    // real effect" (the design's own rule, mirroring PORT's identical rule) is
                    // data-dependent (truth may not have arrived yet) and is only known once
                    // this span -- and every later span this fault stays active across -- has
                    // actually run and been drained. `active_sensor_fault` records which fault
                    // is now installed on this instance; the event is built once the fault's own
                    // window ends (`Boundary::SensorFaultEnd`, below) or, for a persistent
                    // fault, once the whole run ends (this function's own tail).
                    Boundary::Fault(f) if f.target_kind == FaultTargetKind::Sensor as i32 => {
                        let new_plan = fault::apply_sensor_fault(&span.cur_plan, f)?;
                        span.cur_plan = new_plan;
                        span.x0 = last_state;
                        span.seg_start_is_post_maneuver = false;
                        active_sensor_fault.insert(name.clone(), f.id.clone());
                    }
                    Boundary::Fault(f) => {
                        let new_plan = fault::apply_dynamics_fault(&span.cur_plan, f)?;
                        span.cur_plan = new_plan;
                        span.x0 = last_state;
                        span.seg_start_is_post_maneuver = false;
                        all_events.push(events::fault_event(f, name, events::event_provenance(sos_hash, &scenario.data_pack_hash, run_id, sys_hash, &sys.id)));
                    }
                    // Question 178 (R5.1a): the second, synthesized boundary at a windowed
                    // SENSOR fault's own end epoch -- restores the spec to its exact pre-fault
                    // value. `active_sensor_fault` is cleared, and the fault's own event built
                    // from the accumulated totals, after this per-instance loop (below), once
                    // this instance's own about-to-be-discarded handle has also been drained
                    // (the drain call right before `span.handle` is overwritten, further down,
                    // is what captures this span's own final contribution).
                    Boundary::SensorFaultEnd(_f) => {
                        span.cur_plan = fault::clear_sensor_fault(&span.cur_plan);
                        span.x0 = last_state;
                        span.seg_start_is_post_maneuver = false;
                    }
                    Boundary::Maneuver(m) => {
                        // M22.2b: `BindingPlan::Imu`'s own declared state -- [bias_gyro_x,y,z,
                        // bias_accel_x,y,z] -- is *also* exactly 6 components (`ImuModel::
                        // state_dim() == 6`), so the generic "is this length 6" check just below
                        // is no longer, by itself, a reliable test for "is this a translational
                        // Cartesian state a burn can sensibly be applied to." Refused explicitly
                        // here, by plan variant, regardless of dimension, before that generic
                        // check runs (which still correctly catches every other non-6-dim shape:
                        // StarTracker's own state_dim() == 0, Attitude's 7+).
                        // M22.4: `BindingPlan::Controller` added to this explicit guard for
                        // parity with `StarTracker` (`AttitudeControllerModel::state_dim() == 0`
                        // too, so the generic six-component check below would already catch it,
                        // but naming it explicitly here proves the refusal is deliberate, not an
                        // accident of the generic check).
                        // M25.1: `BindingPlan::GroundStation` added to this explicit guard for
                        // the same parity reason M22.4 added `Controller`
                        // (`GroundStationModel::state_dim() == 0` too, so the generic six-
                        // component check below would already catch it, but naming it explicitly
                        // here proves the refusal is deliberate, not an accident of the generic
                        // check).
                        if matches!(span.cur_plan, BindingPlan::Imu(_) | BindingPlan::StarTracker(_) | BindingPlan::Controller(_) | BindingPlan::GroundStation(_)) {
                            return Err(DrmError::ManeuverTargetNotSixDimensional { instance: name.clone(), maneuver_id: m.id.clone(), state_dim: last_state.len() });
                        }
                        // A dv jump only makes sense against a 6-component Cartesian
                        // position/velocity state -- refused, typed, rather than panicking on
                        // a length-6 array conversion that (post-M21.3) can no longer be
                        // assumed to succeed for every `plans`-classified instance.
                        let last_state_6: [f64; 6] = last_state.as_slice().try_into().map_err(|_| DrmError::ManeuverTargetNotSixDimensional {
                            instance: name.clone(),
                            maneuver_id: m.id.clone(),
                            state_dim: last_state.len(),
                        })?;
                        let (applied_dv, sampled) = dv_to_apply(error_mode, m, &scenario.seeds);
                        span.x0 = apply_dv_to_state(m.axes, applied_dv, last_state_6).to_vec();
                        span.seg_start_is_post_maneuver = true;
                        all_events.push(events::maneuver_event(m, name, boundary, sampled.as_ref(), error_mode, events::event_provenance(sos_hash, &scenario.data_pack_hash, run_id, sys_hash, &sys.id)));
                    }
                }
            } else {
                // Unaffected by this particular boundary: unchanged plan, continuous state --
                // see this function's own doc comment's "Faults and maneuvers" section.
                span.x0 = last_state;
                span.seg_start_is_post_maneuver = false;
            }
            let name_suffix = format!("{name}_{}", span.all_segments.len());
            let rebound = materialize_plan_at_boundary(gmat, &span.cur_plan, sys, boundary, &span.x0, /* with_stm */ false, options.accept_missing_stm_terms, gmat_ns, &name_suffix)?;
            // Question 178 (R5.1a): this span's own SENSOR fault contribution (if any) was
            // already folded into `sensor_fault_totals` right after `run_one_span` returned,
            // above -- `span.handle` itself was `None` throughout that span (`run_one_span`'s
            // own `span.handle.take()`), so there is nothing left to drain from it here.
            // M25.4b: every span re-materializes every registered instance's own handle, even one
            // this particular boundary did not target (`span.cur_plan`/`span.x0` are simply
            // unchanged in that case, above) -- so a replayed instance's handle must be
            // re-wrapped here too, on every boundary, the same way its FIRST span's handle was
            // wrapped above this loop.
            span.handle = Some(if replay_targets.contains(name) { ModelRegistry::wrap_replay(rebound, name, replay_log.expect("checked by the debug_assert! above")) } else { rebound });
        }

        // Question 178 (R5.1a): a windowed SENSOR fault's own end boundary -- the drain above
        // (inside the per-instance loop that just ran, for `target`'s own about-to-be-discarded
        // handle) has now captured this span's own final contribution, so the accumulated total
        // is complete. Clear `active_sensor_fault` and emit exactly one `EVENT_KIND_FAULT` event
        // -- at the epoch of the fault's own first real effect, carrying the total count -- but
        // only if it ever actually applied (mirrors PORT's own "a fault that never actually
        // applies produces no event at all" rule; `sensor_fault_totals` has no entry, or a
        // `None` first epoch, when nothing was ever drained).
        if let Boundary::SensorFaultEnd(f) = b {
            active_sensor_fault.remove(&target);
            if let Some((Some(first_effect_tai_ns), frames_affected)) = sensor_fault_totals.get(&f.id).copied() {
                let instance = instances_by_name[&target];
                let sys = systems.get(&instance.system_id).expect("validated in pass 1");
                let sys_hash = system_hashes.get(&instance.system_id).expect("verified above");
                all_events.push(events::sensor_fault_event(f, first_effect_tai_ns, frames_affected, events::event_provenance(sos_hash, &scenario.data_pack_hash, run_id, sys_hash, &sys.id)));
            }
        }

        // M15.3 (question 118), a FAULT_TARGET_KIND_HARDWARE fault as of M16.2 (question 120):
        // the one boundary effect a container instance can have on itself -- a power-cycle
        // fault calls `Reset` at the fault's own epoch, with
        // `reason = "fault:<fault id>"` (`lockstep.proto`'s own `LockstepResetRequest.reason`
        // doc comment: `"power_cycle", "watchdog", "fault:<fault id>"` -- this is the third
        // form). Unlike a model instance at a fault boundary, there is no plan to change and no
        // state to re-materialize from (`ContainerSpanState` carries neither): the *same* live
        // connection simply continues into the next span's kernel (`run_one_span`'s own
        // `container_spans` registration, unchanged by this boundary) with its integrator reset
        // server-side. A container instance this boundary does not target is untouched, exactly
        // like an unaffected model instance.
        if let Boundary::Fault(f) = b {
            if fault::is_container_power_cycle(f) {
                if let Some(span) = container_spans.get(&target) {
                    let instance = instances_by_name[&target];
                    let sys = systems.get(&instance.system_id).expect("validated in pass 1");
                    let sys_hash = system_hashes.get(&instance.system_id).expect("verified above");
                    span.model.reset(boundary, format!("fault:{}", f.id)).map_err(|e| DrmError::ContainerProtocol { instance: target.clone(), source: e })?;
                    all_events.push(events::fault_event(f, &target, events::event_provenance(sos_hash, &scenario.data_pack_hash, run_id, sys_hash, &sys.id)));
                }
            }
        }
        seg_start = boundary;
    }
    run_one_span(seg_start, run_end_tai_ns, &mut model_spans, &mut container_spans, router, output_period_ns, &mut sensor_fault_span_drains)?;
    for (name, drain) in std::mem::take(&mut sensor_fault_span_drains) {
        fold_sensor_fault_span_drain(&name, Some(drain), &active_sensor_fault, &mut sensor_fault_totals);
    }

    // Question 178 (R5.1a): a PERSISTENT SENSOR fault (`duration_ns == 0`) has no `Boundary::
    // SensorFaultEnd` -- it stays active through run end, so its own final span (the
    // `run_one_span` call immediately above) never reaches the per-boundary handling the
    // boundary loop above gives every OTHER span (that final span's own contribution was
    // already folded into `sensor_fault_totals` immediately above). Emit each such fault's own
    // event here, once, mirroring the `SensorFaultEnd` handling above exactly, just triggered by
    // "the run ended" rather than "the window ended".
    for (name, fault_id) in &active_sensor_fault {
        if let Some((Some(first_effect_tai_ns), frames_affected)) = sensor_fault_totals.get(fault_id).copied() {
            let fault = scenario.faults.iter().find(|f| &f.id == fault_id).expect("active_sensor_fault only ever names a fault id from scenario.faults");
            let instance = instances_by_name[name.as_str()];
            let sys = systems.get(&instance.system_id).expect("validated in pass 1");
            let sys_hash = system_hashes.get(&instance.system_id).expect("verified above");
            all_events.push(events::sensor_fault_event(fault, first_effect_tai_ns, frames_affected, events::event_provenance(sos_hash, &scenario.data_pack_hash, run_id, sys_hash, &sys.id)));
        }
    }

    // Shutdown every container, once, after its own last Step -- mirrors run_container_
    // instance's old identical call, now made once per instance after the shared run finishes
    // rather than at the end of that instance's own dedicated loop.
    for (name, span) in &container_spans {
        span.model
            .shutdown(av_lockstep::LockstepShutdownRequest { run_id: run_id.to_string() })
            .map_err(|e| DrmError::ContainerProtocol { instance: name.clone(), source: e })?;
    }

    let mut trajectories = BTreeMap::new();
    let mut outputs_by_instance = BTreeMap::new();
    for (name, span) in model_spans {
        let instance = instances_by_name[&name];
        let sys = systems.get(&instance.system_id).expect("validated in pass 1");
        let sys_hash = system_hashes.get(&instance.system_id).expect("verified above");
        let mut traj = span.shell.expect("at least the final run_one_span call always runs");
        traj.samples = span.all_samples;
        // M15.1 (question 115): merge adjacent segments this instance's own boundary
        // re-materializations produced but that describe no real reconfiguration -- see this
        // module's own doc comment's "Segment merge across an unaffected boundary" section.
        traj.segments = merge_adjacent_segments(span.all_segments, &span.segment_preceded_by_own_maneuver);
        // Question 130: every command this instance actually applied, across every span of this
        // shared run, becomes exactly one EVENT_KIND_PORT_COMMAND event -- never a segment split
        // (dynamics_hash stays the configuration hash, unaffected by the command stream).
        for cmd in &span.applied_commands {
            // M25.1: a ground-station contact transition rides the same applied-commands channel
            // (`crate::drm::ground::CONTACT_TRANSITION_PORT`, never a real declared `Port`) but
            // becomes an `EVENT_KIND_CONTACT_START`/`_END` event, not a generic
            // `EVENT_KIND_PORT_COMMAND` -- see `events::contact_event`'s own doc comment.
            if cmd.port == super::ground::CONTACT_TRANSITION_PORT {
                all_events.push(events::contact_event(cmd, events::event_provenance(sos_hash, &scenario.data_pack_hash, run_id, sys_hash, &sys.id)));
            } else {
                all_events.push(events::port_command_event(cmd, events::event_provenance(sos_hash, &scenario.data_pack_hash, run_id, sys_hash, &sys.id)));
                // M25.2: this same applied command, if it is what a declared `command`
                // Scenario.event actually dispatched (`commands_by_target_field`, built above),
                // ALSO closes that command's own state machine at ACKED -- "acknowledged by the
                // flight software's telemetry" (`docs/sil-plan.md`'s M25 milestone): `Constant
                // AccelModel::step_with_ports` only ever reports this `AppliedCommand` in the
                // same step it also pushes `ack_framed`'s own packet onto its `Outbox` (proven
                // by `consume_framed_applies_within_the_same_step_reports_it_and_sends_an_ack`,
                // `crate::drm::binding`'s own test module) -- so this applied command's mere
                // presence here is already proof the ack telemetry was sent, without this
                // executor needing to separately decode the ack packet's own bytes on the
                // ground instance's receiving end (`crate::drm::command`'s own module doc
                // comment, "Scope disclosed, not hidden", discloses this plainly).
                if let Some(parsed) = commands_by_target_field.get(&(cmd.instance.clone(), cmd.field.clone())) {
                    all_events.push(command::acked_event(parsed, cmd.applied_tai_ns, events::event_provenance(sos_hash, &scenario.data_pack_hash, run_id, sys_hash, &sys.id)));
                }
            }
        }
        all_events.extend(events::lifecycle_pair(&name, t0, run_end_tai_ns, events::event_provenance(sos_hash, &scenario.data_pack_hash, run_id, sys_hash, &sys.id)));
        // Question 173: fold this instance's own measurements straight in -- global sort by
        // (epoch_ns, measurement_id) happens once, in `execute()`, after every instance (and
        // every span) has contributed (`RunProducts.measurements`'s own proto doc comment).
        all_measurements.extend(span.measurements);
        trajectories.insert(name.clone(), traj);
        outputs_by_instance.insert(name, span.all_outputs);
    }
    for (name, span) in container_spans {
        let instance = instances_by_name[&name];
        let sys = systems.get(&instance.system_id).expect("validated in pass 1");
        let sys_hash = system_hashes.get(&instance.system_id).expect("verified above");
        let mut traj = span.shell.expect("at least the final run_one_span call always runs");
        traj.samples = span.all_samples;
        traj.segments = merge_adjacent_segments(span.all_segments, &span.segment_preceded_by_own_maneuver);
        all_events.extend(events::lifecycle_pair(&name, t0, run_end_tai_ns, events::event_provenance(sos_hash, &scenario.data_pack_hash, run_id, sys_hash, &sys.id)));
        trajectories.insert(name.clone(), traj);
        outputs_by_instance.insert(name.clone(), span.all_outputs);
    }

    Ok((trajectories, all_events, outputs_by_instance, container_binding_hashes, all_measurements))
}

/// Read `instance.initial_covariance` (question 89) and SPD-check it at load, before any
/// propagation -- question 83's Cholesky-based hygiene bar applied to the seed covariance
/// itself, at the one place this executor owns it, rather than relying on
/// `HeteroKernel::run_with_covariance`'s own check of the *propagated* `P(t)` (whose first
/// sample would catch an invalid P0 too, since `Phi(t0,t0) = I`, but with a kernel-internal
/// error context that does not name this as a load-time refusal of the declared seed). Empty is
/// [`super::DrmError::MissingInitialCovariance`]; any other hygiene failure
/// ([`av_cdm::covariance::CovarianceHygieneError::DimensionMismatch`/`NotFinite`/
/// `NotSymmetric`/`NotPositiveDefinite`]) is [`super::DrmError::CovarianceHygiene`], unless
/// `options.nearest_spd_projection` is set, in which case the failing P0 is replaced by
/// `av_cdm::covariance::nearest_spd_row_major`'s projection -- logged loudly and still counted
/// as a hygiene failure, exactly the pattern `HeteroKernel::run_with_covariance` already applies
/// to propagated samples, reused here rather than reinvented.
fn load_initial_covariance(instance: &SystemInstance, n: usize, options: &av_cdm::pb::DrmOptions) -> Result<Vec<f64>, DrmError> {
    if instance.initial_covariance.is_empty() {
        return Err(DrmError::MissingInitialCovariance { instance: instance.name.clone() });
    }
    let context = format!("instance {:?}: SystemInstance.initial_covariance at load", instance.name);
    match av_cdm::covariance::check_spd_row_major(&instance.initial_covariance, n, &context) {
        Ok(_) => Ok(instance.initial_covariance.clone()),
        Err(e) if options.nearest_spd_projection => {
            eprintln!(
                "[av-kernel drm executor] {context} failed the SPD hygiene check ({e}); applying \
                 the opt-in nearest-SPD projection (declared, not a silent repair -- \
                 av_cdm::covariance::nearest_spd_projections_applied() now {})",
                av_cdm::covariance::nearest_spd_projections_applied() + 1
            );
            let projected = av_cdm::covariance::nearest_spd_row_major(&instance.initial_covariance, n, av_cdm::covariance::DEFAULT_NEAREST_SPD_FLOOR_RATIO);
            av_cdm::covariance::check_spd_row_major(&projected, n, &context)
                .unwrap_or_else(|e| panic!("{context}: nearest_spd_row_major's own output failed the hygiene check it was built to pass: {e}"));
            Ok(projected)
        }
        Err(e) => Err(DrmError::CovarianceHygiene(e.to_string())),
    }
}

/// Covariance-path analogue of [`run_one_span`]/[`append_span_samples`]: run one maneuver-bounded
/// span `[seg_start, seg_end]` through `HeteroKernel::run_with_covariance` (a fresh kernel per
/// span, for the same reason -- a boundary always needs a freshly re-materialized model), append
/// its samples/segment onto the accumulators (same deduplication rule as
/// [`append_span_samples`]'s own doc comment -- a maneuver boundary keeps the post-burn
/// sample, not the pre-burn one), and return its final physical state and covariance -- the next
/// span's own seed. `p0` seeds this span's own `StmAugmented` at `seg_start` (`Phi(seg_start,
/// seg_start) = I` by `StmAugmented::seed`'s own construction); see [`run_covariance_instance`]'s
/// doc comment's "Covariance across a burn" section for why the *caller* passing the previous
/// span's own final `cov` back in as this span's `p0`, unmodified, is exactly "P unchanged
/// across the burn".
///
/// **M13.3: `period_ns` (the instance's own covariance step) is threaded separately from
/// `output_period_ns` (the trajectory's own sampling grid) now** -- `HeteroKernel::
/// run_with_covariance` no longer requires them to be equal (see that method's own doc
/// comment), so this registers the instance at its own declared `period_ns` while the kernel
/// still samples every output tick at `output_period_ns`. An output tick that does not land on
/// `period_ns`'s own native grid gets a physical `mean` (Hermite-interpolated) but no real
/// `cov` (`av_kernel::kernel::covariance` reads `None` there, an empty wire `cov` -- question
/// 111, M14.3: no longer the old NaN sentinel) -- **the caller of this function must ensure
/// `seg_end` itself lands on that native grid** (checked in [`run_covariance_instance`], before
/// this is ever called): the `last.cov` this function reads below becomes the *next* span's own
/// `p0` (see this function's own doc comment above), and an off-grid `seg_end` would carry an
/// empty `p0` forward -- caught immediately by the next span's own SPD hygiene check
/// (`CovarianceHygieneError::DimensionMismatch`, `0 != n*n`), not silently accepted as a real
/// covariance.
#[allow(clippy::too_many_arguments)]
fn run_covariance_span(
    instance_name: &str,
    output_period_ns: i64,
    period_ns: i64,
    model: ModelHandle,
    seg_start: i64,
    seg_end: i64,
    x0: [f64; 6],
    p0: Vec<f64>,
    n: usize,
    nearest_spd_projection: bool,
    all_samples: &mut Vec<av_cdm::pb::TrajectorySample>,
    all_segments: &mut Vec<av_cdm::pb::TrajectorySegment>,
    shell: &mut Option<Trajectory>,
    keep_previous_last_and_drop_incoming_first: bool,
    all_outputs: &mut NamedOutputSeries,
) -> Result<([f64; 6], Vec<f64>), DrmError> {
    let boxed = model.into_boxed_stm(instance_name);
    let mut kernel = HeteroKernel::new(output_period_ns);
    kernel.register_system(instance_name.to_string(), period_ns, boxed, seg_start, ModelRegistry::stm_seed(&x0));

    let mut dims = BTreeMap::new();
    dims.insert(instance_name.to_string(), n);
    let mut p0_map = BTreeMap::new();
    p0_map.insert(instance_name.to_string(), p0);

    let mut result = kernel.run_with_covariance(seg_start, seg_end, &dims, &p0_map, nearest_spd_projection).map_err(hetero_err_to_drm)?;
    // Question 101 (M11.2): `av_dynamics::StmAugmented::step` now delegates to the wrapped
    // model's own `step_with_stm` (e.g. `gmat_sys::model::GmatModel::step_with_stm`'s
    // `OUTPUT_RMAG`/`OUTPUT_CD`), so `kernel.outputs` carries real named outputs here exactly
    // the way `run_one_span`'s identical block does for the plain path -- see this module's own
    // doc comment's "Events (question 95, M9.3) and outputs" section.
    if let Some((epochs, values)) = kernel.outputs(instance_name) {
        for (name, vals) in values {
            let entry = all_outputs.entry(name.clone()).or_default();
            entry.0.extend_from_slice(epochs);
            entry.1.extend_from_slice(vals);
        }
    }
    let sub = result.remove(instance_name).expect("registered exactly one system");
    if shell.is_none() {
        *shell = Some(Trajectory { samples: vec![], segments: vec![], ..sub.clone() });
    }
    let mut sub_samples = sub.samples;
    let last = sub_samples.last().expect("run_with_covariance always emits >= 1 sample");
    let last_state: [f64; 6] = last.mean.as_slice().try_into().expect("run_with_covariance truncates mean back to the physical n-state");
    let last_cov = last.cov.clone();
    if !all_samples.is_empty() {
        if keep_previous_last_and_drop_incoming_first {
            sub_samples.remove(0);
        } else {
            all_samples.pop();
        }
    }
    all_samples.extend(sub_samples);
    all_segments.extend(sub.segments);
    Ok((last_state, last_cov))
}

/// Like [`run_shared_group`], but for one instance's own isolated covariance-path loop -- also
/// returns the instance's
/// `run_start`/`run_end` `EVENT_KIND_LIFECYCLE` pair and one `EVENT_KIND_MANEUVER` event per
/// maneuver actually applied (question 95/97, M9.3). No `EVENT_KIND_FAULT` events here:
/// `execute()` refuses covariance combined with DYNAMICS faults for the same instance before
/// ever calling this function ([`DrmError::CovarianceWithFaultsNotSupported`]), so this path
/// never applies one.
///
/// ## Covariance across a burn (question 97's item 5)
///
/// For an impulsive burn with **no execution error** (question 97's own scope -- burn
/// dispersion is explicitly out of this task, see `maneuver`'s module doc comment and this
/// crate's own task report), the sensitivity of the post-burn state to the pre-burn state is
/// exactly the identity: `[r; v] -> [r; v + dv(r,v)]` is a deterministic function of the
/// nominal `(r, v)` alone (no execution error to linearize), so **Phi is unchanged** across the
/// boundary in the sense that no extra Jacobian beyond identity is ever applied there, and
/// **P is unchanged** across the boundary -- the covariance seeded into the next span
/// ([`run_covariance_span`]'s own `p0` argument) is *exactly* the previous span's own final
/// `cov`, copied through unmodified, never recomputed. This is implemented, not merely asserted:
/// every maneuver boundary below starts a **new** `HeteroKernel::run_with_covariance` call (a
/// fresh `StmAugmented` seed, `Phi(seg_start, seg_start) = I` by construction) fed the previous
/// span's own last `cov` as its `p0` -- so the only way a burn could change `P` at all would be
/// a bug in this carry-through, which `tests/drm_maneuver.rs::covariance_is_unchanged_across_a_
/// no_execution_error_burn` pins directly (the sample immediately before the burn and the one
/// immediately after must carry bit-identical `cov`).
///
/// **With execution error, mode-dependent (question 103).** Under [`ExecutionErrorMode::
/// Nominal`], the commanded `dv` is applied exactly (via [`dv_to_apply`], same as the
/// no-execution-error case above) and, when `execution_error` is declared,
/// `P+ = P- + G Q G^T` is injected in place at this same boundary -- `Phi` is not touched either
/// way. Under [`ExecutionErrorMode::Sampled`], one Gates-model realization is drawn and the
/// *drawn* `dv` is applied instead, and **nothing is injected**: the dispersion this draw
/// represents is already realized in the mean, so injecting the analytic term on top would
/// double-count the same uncertainty as both a mean perturbation and a covariance term. See
/// [`ExecutionErrorMode`]'s own doc comment and `tests/gates_execution_error.rs`'s
/// covariance-wiring tests for both cases proven end to end.
///
/// **Outputs (question 95's second half, task M10.2; carried as of question 101, M11.2).**
/// Through M11.1 this did not accumulate `StepResult.outputs` at all: the covariance path erases
/// through `ModelHandle::into_boxed_stm` (`StmAugmented<AnyModel>`, `crate::registry`), and
/// `av_dynamics::StmStepResult` (what a `step_with_stm` call produces) had no `outputs` field to
/// carry in the first place, and `av_dynamics::StmAugmented::step` never called `step_with_stm`
/// to begin with (it inherited the trait's default, continuous-augmented-integration `step`).
/// Question 101 ("product sets must not depend on the run mode") closed both gaps:
/// `StmStepResult` now has an `outputs` field (`av-dynamics`, not owned by this task) and
/// `StmAugmented::step` now delegates to the wrapped model's own `step_with_stm` per native
/// period (also `av-dynamics`), so `run_covariance_span`'s `kernel.outputs` call below now
/// reports real values exactly like `run_one_span`'s does for the plain path -- see this
/// module's own doc comment's "Events (question 95, M9.3) and outputs" section, and
/// `crates/av-kernel/tests/drm_executor.rs`'s equality test for the plain-vs-covariance product
/// set comparison this makes possible.
#[allow(clippy::too_many_arguments)]
fn run_covariance_instance(
    gmat: &Gmat,
    plan: &BindingPlan,
    sys: &SystemDefinition,
    instance: &SystemInstance,
    scenario: &Scenario,
    options: &av_cdm::pb::DrmOptions,
    output_period_ns: i64,
    period_ns: i64,
    maneuvers: &[&ParsedManeuver],
    error_mode: ExecutionErrorMode,
    sos_hash: &str,
    sys_hash: &str,
    run_id: &str,
    gmat_ns: &str,
) -> Result<(Trajectory, Vec<Event>, NamedOutputSeries), DrmError> {
    let mat = materialize_plan(gmat, plan, sys, scenario.start_tai_ns, /* with_stm */ true, options.accept_missing_stm_terms, gmat_ns, &format!("{}_0", instance.name))?;
    if !mat.stm_capable() {
        return Err(DrmError::ModelNotStmCapable { instance: instance.name.clone() });
    }
    let n = mat.state_dim();
    let p0 = load_initial_covariance(instance, n, options)?;

    let t0_tai_ns = mat.t0_tai_ns;
    // `.as_slice().try_into()`, not a move: `x0_si` is `Vec<f64>` now (M21.3), and `mat` (the
    // whole `ModelHandle`) is moved into `model` just below -- always safe to convert to a
    // fixed `[f64; 6]` here since only a `Gmat` plan ever reaches this covariance-only path
    // (`ModelNotStmCapable` already refused a `"native."`-dispatched instance above,
    // regardless of its own declared dimension).
    let mut x0: [f64; 6] = mat.x0_si.as_slice().try_into().expect("a Gmat plan's own initial state is always 6-dimensional");
    let run_end_tai_ns = t0_tai_ns + (scenario.end_tai_ns - scenario.start_tai_ns);

    let mut model = mat;
    let mut p_cur = p0;
    let mut seg_start = t0_tai_ns;
    let mut seg_start_is_post_maneuver = false;

    let mut all_samples = Vec::new();
    let mut all_segments = Vec::new();
    let mut shell: Option<Trajectory> = None;
    let mut emitted = Vec::new();
    let mut all_outputs: NamedOutputSeries = BTreeMap::new();

    let mut sorted_maneuvers: Vec<&ParsedManeuver> = maneuvers.to_vec();
    sorted_maneuvers.sort_by_key(|m| maneuver::epoch_id_order(m));

    for m in &sorted_maneuvers {
        let boundary = t0_tai_ns + (m.tai_ns - scenario.start_tai_ns);
        if boundary <= seg_start || boundary >= run_end_tai_ns {
            continue;
        }
        // M13.3: a maneuver boundary's own covariance is carried, unmodified (or Gates-injected,
        // "Covariance across a burn" above), into the *next* span's `p0` -- that only means
        // anything if `boundary` actually landed on this instance's own covariance native grid
        // (`av_kernel::kernel::covariance` returning `Some`, not `None` -- question 111, M14.3:
        // no longer a NaN sentinel to distinguish from). Checked explicitly here, before ever
        // calling `run_covariance_span`, rather than letting an empty `p0` reach the next span's
        // own SPD hygiene check (which would still catch it -- `check_spd_row_major`'s
        // `DimensionMismatch` case, `0 != n*n` -- but with a confusing "hygiene failure" report
        // rather than naming the real cause).
        if (boundary - seg_start) % period_ns != 0 {
            return Err(DrmError::ManeuverEpochNotOnCovarianceGrid { id: m.id.clone(), instance: instance.name.clone(), tai_ns: m.tai_ns, period_ns });
        }
        let (last_state, last_cov) = run_covariance_span(
            &instance.name,
            output_period_ns,
            period_ns,
            model,
            seg_start,
            boundary,
            x0,
            p_cur,
            n,
            options.nearest_spd_projection,
            &mut all_samples,
            &mut all_segments,
            &mut shell,
            !seg_start_is_post_maneuver,
            &mut all_outputs,
        )?;

        // Question 103: `error_mode` selects the applied dv identically on this path and on
        // `run_shared_group`'s own maneuver loop -- see `dv_to_apply`'s own doc comment.
        // `Nominal` always applies `m.dv` exactly; `Sampled` draws one Gates-model realization
        // when `m.execution_error` is declared (never on this path before this task -- question
        // 103 is exactly what makes that possible here).
        let (applied_dv, sampled) = dv_to_apply(error_mode, m, &scenario.seeds);
        let new_state = apply_dv_to_state(m.axes, applied_dv, last_state);
        let rebound =
            materialize_plan_at_boundary(gmat, plan, sys, boundary, &new_state, /* with_stm */ true, options.accept_missing_stm_terms, gmat_ns, &format!("{}_{}", instance.name, all_segments.len()))?;
        model = rebound;
        x0 = new_state;
        // Injection only ever happens under `Nominal` -- see this function's own "Covariance
        // across a burn" doc comment section. "No execution error": P carries over unchanged,
        // never recomputed. "With execution error, Nominal": P+ = P- + G Q G^T is injected in
        // place, analytically, at this same boundary -- Phi is not touched either way. "Sampled"
        // (with or without execution_error): never injects -- the dispersion, if any, is already
        // realized in `applied_dv`/the mean above.
        let mut new_cov = last_cov;
        if error_mode == ExecutionErrorMode::Nominal {
            if let Some(ee) = &m.execution_error {
                let r = [last_state[0], last_state[1], last_state[2]];
                let v = [last_state[3], last_state[4], last_state[5]];
                let (sigma_m, sigma_p) = maneuver::gates_sigmas(ee, (m.dv[0] * m.dv[0] + m.dv[1] * m.dv[1] + m.dv[2] * m.dv[2]).sqrt());
                let triad = maneuver::inertial_triad(m.axes, m.dv, r, v);
                maneuver::inject_gates_covariance(&mut new_cov, n, sigma_m, sigma_p, triad);
            }
        }
        p_cur = new_cov;
        seg_start = boundary;
        seg_start_is_post_maneuver = true;

        emitted.push(events::maneuver_event(m, &instance.name, boundary, sampled.as_ref(), error_mode, events::event_provenance(sos_hash, &scenario.data_pack_hash, run_id, sys_hash, &sys.id)));
    }
    let _ = run_covariance_span(
        &instance.name,
        output_period_ns,
        period_ns,
        model,
        seg_start,
        run_end_tai_ns,
        x0,
        p_cur,
        n,
        options.nearest_spd_projection,
        &mut all_samples,
        &mut all_segments,
        &mut shell,
        !seg_start_is_post_maneuver,
        &mut all_outputs,
    )?;

    let mut traj = shell.expect("at least the final run_covariance_span call always runs");
    traj.samples = all_samples;
    traj.segments = all_segments;
    emitted.extend(events::lifecycle_pair(&instance.name, t0_tai_ns, run_end_tai_ns, events::event_provenance(sos_hash, &scenario.data_pack_hash, run_id, sys_hash, &sys.id)));
    Ok((traj, emitted, all_outputs))
}

fn finish_trajectory(mut traj: Trajectory, drm_hash: &str, sos_hash: &str, scenario: &Scenario, run_id: &str, sys: &SystemDefinition, sys_hash: &str) -> Trajectory {
    traj.config_hash = drm_hash.to_string();
    traj.provenance = Some(Provenance {
        author_kind: AuthorKind::Service as i32,
        principal: String::new(),
        tool: "av-kernel::drm::executor".to_string(),
        config_hash: sos_hash.to_string(),
        data_pack_hash: scenario.data_pack_hash.clone(),
        dataset_hash: String::new(),
        // Deliberately not a wall-clock read -- see the module doc comment's "Provenance"
        // section.
        created_tai_ns: 0,
        run_id: run_id.to_string(),
        attributes: BTreeMap::from([("system_definition_hash".to_string(), sys_hash.to_string()), ("system_definition_id".to_string(), sys.id.clone())]),
    });
    traj
}

/// Question 128, M19.1 (ADR-002's fourth amendment): a `"gmat."`-bound trajectory is emitted in
/// its *declared* frame by converting every sample through GMAT's own `CoordinateConverter::
/// Convert` (`gmat_sys::Gmat::convert`), rather than left in the integration frame under a
/// mismatched label. A no-op (returns `traj` unchanged) for:
/// - a native (`BindingPlan::ConstantAccel`) instance -- no GMAT central body/integration frame
///   concept applies to it at all;
/// - a `"gmat."`-bound instance whose declared frame already IS its own integration frame
///   (`{central_body}MJ2000Eq`) -- every trajectory before M19.1, unaffected.
///
/// For a `"gmat."`-bound instance whose declared frame differs, `binding::parse_gmat_spec`
/// (Step 1/4, question 128) already guarantees `traj.frame_id` decomposes under
/// [`body_axes_suffix`] -- the `ok_or_else` below is a defensive re-check, not a path any
/// caller of [`execute`] can actually reach (classification refuses first).
///
/// **M21.4 (question 138, closing the M19.1 refusal): a sample carrying a propagated
/// covariance is rotated too, not just skipped.** For a sample whose `cov` is non-empty, this
/// function calls [`Gmat::convert_with_rotation`] instead of [`Gmat::convert`] -- the identical
/// `CoordinateConverter::Convert` call, additionally returning the 3x3 rotation matrix `R` and
/// its time derivative `Rdot` that call computed (`shim/gmatffi.h`'s own doc comment on
/// `gmatffi_convert_state_and_rotation` has the full account) -- and applies the resulting 6x6
/// Jacobian `M = [[R,0],[Rdot,R]]` as `P' = M P M^T` ([`rotate_covariance`]). The rotation
/// between two frames is generally time-varying (a body-fixed frame rotates with its body), so
/// `M` is NOT block-diagonal `[[R,0],[0,R]]` -- the velocity block gains the `Rdot` coupling
/// term; getting this wrong shows up as a velocity-block error while the position block looks
/// perfect (see `crates/gmat-sys/tests/convert_rotation.rs`, which measures exactly that
/// signature against GMAT's own `OrbitErrorCovariance` `ReportFile` -- GMAT's own report turns
/// out to omit this same term, which is why this function does not pin against it; see that
/// test file's own doc comment and this task's own report for the full account). A sample whose
/// `cov` is empty keeps calling plain [`Gmat::convert`] (unchanged from before this task) --
/// there is nothing to rotate, and no reason to pay for the extra rotation-matrix extraction.
///
/// [`av_cdm::covariance::check_spd_row_major`] is run again on every rotated `cov` before it is
/// written back (`DrmError::CovarianceHygiene` on failure) -- the same Cholesky-based SPD bar
/// `Kernel::run_with_covariance` already ran on the *pre-rotation* covariance (`kernel.rs`), but
/// a congruence transform `M P M^T` is only guaranteed symmetric positive-*semi*-definite in
/// exact arithmetic (an invertible `M`, which `[[R,0],[Rdot,R]]` always is since `R` is a
/// rotation, preserves strict positive-definiteness too, but this function checks rather than
/// assumes: floating-point round-off is exactly the kind of "assumed but never verified" gap
/// this repository's own covariance hygiene rule (`docs/open-questions.md` question 80) exists
/// to catch). [`av_dynamics::propagate_covariance`] (reused rather than reimplemented -- it
/// already is exactly `M P M^T` with explicit symmetrization and asymmetry reporting, `n = 6`
/// generic) does the matrix arithmetic and its own symmetrization; this function additionally
/// re-runs the SPD check the symmetrization step does not itself guarantee.
///
/// The two `CoordinateSystem` objects this needs (the integration frame and the declared one)
/// are constructed fresh, under names namespaced by `gmat_ns` (unique per [`execute`] call,
/// exactly `materialize_gmat`'s own convention) and `instance_name` (unique within one run) --
/// never touching any of GMAT's own pre-existing named defaults (`"EarthMJ2000Eq"`,
/// `"EarthICRF"`, ...), so this can never reconfigure an object some other instance, or GMAT
/// itself, is already relying on (GMAT is a process-wide singleton -- see the module doc
/// comment's threading section).
///
/// Units/epoch: `TrajectorySample.mean`/`tai_ns` are SI metres / metres-per-second and TAI
/// nanoseconds (ADR-001); [`Gmat::convert`]/[`Gmat::convert_with_rotation`] -- like every other
/// `gmat-sys` shim call -- are km/km-s and A.1 Modified Julian, so every sample crosses through
/// `av_cdm::units::state_m_to_km`/`state_km_to_m` and `av_cdm::time::Tai::to_a1_mjd` here, the
/// one place this function touches units or epochs at all (mirrors `gmat_sys::model::GmatModel`'s
/// own "this module is the km<->m and A1MJD<->TAI boundary" rule). `cov` needs no such crossing:
/// `rotation`/`rotation_dot` are unitless (a pure rotation and its rate), so the SI `cov` this
/// function reads and writes back never passes through a km<->m conversion at all -- see
/// [`Gmat::convert_with_rotation`]'s own doc comment.
fn convert_gmat_trajectory_to_declared_frame(gmat: &Gmat, plan: &BindingPlan, gmat_ns: &str, instance_name: &str, mut traj: Trajectory) -> Result<Trajectory, DrmError> {
    let spec = match plan {
        BindingPlan::Gmat(spec) => spec,
        BindingPlan::ConstantAccel(_) => return Ok(traj),
        // M22.1b: an attitude instance has no GMAT central body/integration frame concept
        // either -- same no-op passthrough as the ConstantAccel arm above, for the same reason.
        BindingPlan::Attitude(_) => return Ok(traj),
        // M22.2b: neither native sensor model has a GMAT central body/integration frame concept
        // either -- same no-op passthrough, for the same reason.
        BindingPlan::StarTracker(_) => return Ok(traj),
        BindingPlan::Imu(_) => return Ok(traj),
        // M22.4: the controller has no GMAT central body/integration frame concept either --
        // same no-op passthrough, for the same reason.
        BindingPlan::Controller(_) => return Ok(traj),
        // M25.1: a ground station has no GMAT central body/integration frame concept either --
        // same no-op passthrough, for the same reason.
        BindingPlan::GroundStation(_) => return Ok(traj),
    };
    let integration_frame = format!("{}MJ2000Eq", spec.central_body);
    if traj.frame_id.is_empty() || traj.frame_id == integration_frame {
        return Ok(traj);
    }
    let (body, axes) = body_axes_suffix(&traj.frame_id).ok_or_else(|| DrmError::UnsupportedCoordinateSystem {
        context: format!("instance {instance_name:?}"),
        declared: traj.frame_id.clone(),
        integration_frame: integration_frame.clone(),
    })?;

    let from_name = format!("Cvt{gmat_ns}_{instance_name}_From");
    let to_name = format!("Cvt{gmat_ns}_{instance_name}_To");
    gmat.coordinate_system(&from_name, &spec.central_body, "MJ2000Eq").map_err(DrmError::Gmat)?;
    gmat.coordinate_system(&to_name, body, axes).map_err(DrmError::Gmat)?;
    gmat.initialize().map_err(DrmError::Gmat)?;

    for sample in &mut traj.samples {
        assert_eq!(sample.mean.len(), 6, "a GMAT-bound instance's TrajectorySample.mean is always the 6-state Cartesian shape (gmat.orbital.cartesian6)");
        let epoch_a1mjd = Tai::from_nanos(sample.tai_ns).to_a1_mjd();
        let state_m: [f64; 6] = sample.mean[0..6].try_into().expect("checked by the assert above");
        let state_km = units::state_m_to_km(state_m);
        if sample.cov.is_empty() {
            let out_km = gmat.convert(epoch_a1mjd, &state_km, &from_name, &to_name).map_err(DrmError::Gmat)?;
            sample.mean.copy_from_slice(&units::state_km_to_m(out_km));
        } else {
            let converted = gmat.convert_with_rotation(epoch_a1mjd, &state_km, &from_name, &to_name).map_err(DrmError::Gmat)?;
            sample.mean.copy_from_slice(&units::state_km_to_m(converted.state_km));
            let context = format!("instance {instance_name:?} tai_ns {} frame {:?}", sample.tai_ns, traj.frame_id);
            sample.cov = rotate_covariance(&converted.rotation, &converted.rotation_dot, &sample.cov, &context)?;
        }
    }
    Ok(traj)
}

/// `P' = M P M^T` for `M = [[R,0],[Rdot,R]]` (row-major 6x6, built from the row-major 3x3
/// `rotation`/`rotation_dot` [`Gmat::convert_with_rotation`] returns), re-checked for SPD
/// hygiene afterward -- see [`convert_gmat_trajectory_to_declared_frame`]'s own doc comment for
/// why both the block-matrix shape and the re-check exist.
///
/// # Errors
///
/// [`DrmError::CovarianceHygiene`] if the rotated covariance fails
/// [`av_cdm::covariance::check_spd_row_major`].
fn rotate_covariance(rotation: &[f64; 9], rotation_dot: &[f64; 9], cov: &[f64], context: &str) -> Result<Vec<f64>, DrmError> {
    assert_eq!(cov.len(), 36, "TrajectorySample.cov is always a 6x6 row-major matrix (36 elements) when non-empty");
    let mut m = [0.0_f64; 36];
    for i in 0..3 {
        for j in 0..3 {
            m[i * 6 + j] = rotation[i * 3 + j]; // top-left: R
            // top-right stays 0.0 (translation is state-independent, not part of this Jacobian)
            m[(i + 3) * 6 + j] = rotation_dot[i * 3 + j]; // bottom-left: Rdot
            m[(i + 3) * 6 + (j + 3)] = rotation[i * 3 + j]; // bottom-right: R
        }
    }
    let (rotated, _pre_symmetrization_asymmetry) = av_dynamics::propagate_covariance(&m, cov, 6);
    av_cdm::covariance::check_spd_row_major(&rotated, 6, context).map_err(|e| DrmError::CovarianceHygiene(e.to_string()))?;
    Ok(rotated)
}

/// [`RunProducts.provenance`]: the run's own overall provenance, built the same way each
/// individual `Trajectory`'s is (`config_hash` = the DRM's own hash) -- see the module doc
/// comment's "Provenance" section. `error_mode` (question 103: "provenance records the mode")
/// is recorded here too, at the run level, in addition to each individual MANEUVER event's own
/// `Provenance.attributes` (`events::maneuver_event`) -- a caller inspecting only
/// `RunProducts.provenance` can still tell which mode the whole run used, even for a DRM with no
/// maneuvers at all.
///
/// **M14.1 (question 109): `"kernel_run_mode"`.** `"shared"` when this run's non-covariance
/// instances went through the one shared [`run_shared_group`] kernel call, `"covariance_per_
/// instance"` when `options.covariance` sent every model instance through its own isolated
/// [`run_covariance_instance`] loop instead -- the covariance path's own disclosed limitation
/// (see the module doc comment's "One shared kernel run" section): a caller reading only
/// `RunProducts.provenance` can tell, without inspecting `DrmOptions` itself, which of the two
/// run shapes actually produced this run's own trajectories.
///
/// **M14.4: `"dropped_in_flight_messages"`.** The exact count `execute()` read from `Router::
/// pending_count` once the run's own last span finished (`crate::router::Router`'s own module
/// doc comment's "still pending when the run ends" note) -- recorded here unconditionally
/// (`"0"` for a clean run, same as any other run-level fact this crate reports honestly rather
/// than omitting when it happens to be uninteresting), so a caller reading only `RunProducts.
/// provenance` can tell whether anything was lost without separately reconstructing the router's
/// own wiring. See `events::dropped_messages_event`'s own doc comment for the companion
/// `EVENT_KIND_LIFECYCLE` event `execute()` adds to `RunProducts.events` when this is non-zero.
fn build_run_provenance(drm_hash: &str, sos_hash: &str, scenario: &Scenario, run_id: &str, error_mode: ExecutionErrorMode, covariance: bool, dropped_in_flight_messages: usize) -> Provenance {
    let kernel_run_mode = if covariance { "covariance_per_instance" } else { "shared" };
    Provenance {
        author_kind: AuthorKind::Service as i32,
        principal: String::new(),
        tool: "av-kernel::drm::executor".to_string(),
        config_hash: drm_hash.to_string(),
        data_pack_hash: scenario.data_pack_hash.clone(),
        dataset_hash: String::new(),
        created_tai_ns: 0,
        run_id: run_id.to_string(),
        attributes: BTreeMap::from([
            ("sos_configuration_hash".to_string(), sos_hash.to_string()),
            ("execution_error_mode".to_string(), error_mode.as_str().to_string()),
            ("kernel_run_mode".to_string(), kernel_run_mode.to_string()),
            ("dropped_in_flight_messages".to_string(), dropped_in_flight_messages.to_string()),
        ]),
    }
}

/// A `Trajectory` shell carrying only what `crate::expr::typecheck::check` ever reads
/// (`entity_id`, `state_space_id`) and empty `samples` -- built from `cfg.systems`/
/// `cfg.sos.instances` alone, before any instance has run, for the load-time expression
/// validation pass. See the module doc comment's "Scoring" section.
fn declared_shape_trajectory(instance_name: &str, state_space_id: &str) -> Trajectory {
    Trajectory { entity_id: instance_name.to_string(), state_space_id: state_space_id.to_string(), samples: vec![], ..Default::default() }
}

/// [`av_cdm::pb::PortTrafficLog::records`]'s own required order: **`(tai_ns, sequence,
/// instance, port)`, epoch first** (`docs/open-questions.md` question 181, decided by the lead
/// after M25.4a measured the problem). M25.4a originally sorted `(sequence, instance, port)`,
/// matching the proto's own doc comment at the time; that order is not epoch-monotonic, because
/// `run_shared_group` hands every declared `command` `Scenario.event` to `crate::router::
/// Router::deliver` *before* the run's first output tick, so those records carry `sequence = 0`
/// while their `tai_ns` is mid-run (`drms/demo_command`'s telecommand, dispatched at t = 50 s of
/// a 100 s run, is the concrete case, pinned by `tests/port_traffic_sidecar.rs`). Epoch first
/// means a reader keyed on epoch -- which is what a replay is (`crate::drm::replay`) -- sees a
/// monotonic log.
///
/// `sequence` remains the first tie-break, so two frames carried at the same epoch by different
/// ticks still order by tick. The sort is STABLE, so records tying on all four (two frames on
/// the very same port at the very same epoch in the very same step) keep
/// [`crate::router::Router::take_port_traffic`]'s own emission order rather than being
/// reordered here.
fn sort_port_traffic(records: &mut [pb::PortTrafficRecord]) {
    records.sort_by(|a, b| (a.tai_ns, a.sequence, a.instance.as_str(), a.port.as_str()).cmp(&(b.tai_ns, b.sequence, b.instance.as_str(), b.port.as_str())));
}

/// Question 175 (M25.4a): write this run's `PortTrafficLog` sidecar when `products_dir` is
/// `Some`, and record the outcome on a fresh copy of `base_provenance` either way -- see
/// `execute`'s own module doc comment's "Port traffic sidecar" section for the full contract.
/// Returns `(port_traffic_hash, updated_provenance)`; `port_traffic_hash` is empty exactly when
/// `products_dir` is `None`. `base_provenance` is what `PortTrafficLog.provenance` itself
/// carries (a snapshot from *before* either `"port_traffic_uri"` or `"port_traffic"` is added --
/// the sidecar's own provenance describes the run, not itself), never mutated in place; the
/// returned `Provenance` is what `RunProducts.provenance` becomes.
fn write_port_traffic_sidecar(products_dir: Option<&std::path::Path>, run_id: &str, mut records: Vec<pb::PortTrafficRecord>, undeclared_port_emissions: u64, base_provenance: &Provenance) -> Result<(String, Provenance), DrmError> {
    let mut provenance = base_provenance.clone();
    let Some(dir) = products_dir else {
        // Absence is explicit, never both attributes at once (see this function's own doc
        // comment and the module doc comment's "Port traffic sidecar" section).
        provenance.attributes.insert("port_traffic".to_string(), "not recorded".to_string());
        return Ok((String::new(), provenance));
    };
    sort_port_traffic(&mut records);
    // The sidecar's own provenance is the run's, plus one fact that is about the recording
    // rather than about the run: how many emissions this run's router saw on a port with no
    // declared `PortKind` and therefore could not classify (`crate::router::Router::deliver`'s
    // own doc comment). Written only when non-zero -- there is no zero-valued attribute, the
    // same convention `events::dropped_messages_event` follows for in-flight messages -- so its
    // presence always means something, and its absence means nothing was skipped.
    let mut log_provenance = base_provenance.clone();
    if undeclared_port_emissions > 0 {
        log_provenance.attributes.insert("undeclared_port_emissions".to_string(), undeclared_port_emissions.to_string());
    }
    let log = pb::PortTrafficLog { run_id: run_id.to_string(), records, provenance: Some(log_provenance) };
    // Same encoding path `RunProducts::to_proto`'s own callers already use for the main
    // RunProducts bundle (`prost::Message::encode_to_vec`) -- reused, not hand-rolled.
    let bytes = log.encode_to_vec();
    std::fs::create_dir_all(dir).map_err(|e| DrmError::PortTrafficSidecarIo { path: dir.to_path_buf(), detail: format!("creating directory: {e}") })?;
    let path = dir.join("port_traffic.pb");
    std::fs::write(&path, &bytes).map_err(|e| DrmError::PortTrafficSidecarIo { path: path.clone(), detail: format!("writing {} byte(s): {e}", bytes.len()) })?;
    // This crate's one SHA-256 primitive (`hash::sha256_hex`) -- no new crate, no `ring` -- over
    // the EXACT bytes just written, not the in-memory `log` value re-serialized a second time
    // (which could theoretically disagree with what actually landed on disk).
    let port_traffic_hash = hash::sha256_hex(&bytes);
    provenance.attributes.insert("port_traffic_uri".to_string(), path.display().to_string());
    Ok((port_traffic_hash, provenance))
}

/// Question 175 (M25.4a): [`sort_port_traffic`] alone, with no GMAT/kernel/router machinery
/// involved -- `tests/port_traffic_sidecar.rs`'s own module doc comment explains why
/// `demo_command` itself cannot exercise the `(sequence, instance, port)` tie-break (its own
/// two connections never put two records on the same `(sequence, instance)` pair), so this is
/// the one place that tie-break is actually proven, directly against the sort function real
/// runs also use.
#[cfg(test)]
mod sort_port_traffic_tests {
    use super::*;

    /// Every record with the same `tai_ns` unless a test says otherwise, so the
    /// `sequence`/`instance`/`port` tie-breaks below are exercised in isolation from the
    /// epoch key (question 181 made `tai_ns` the primary key; see [`sort_port_traffic`]).
    fn rec(sequence: u64, instance: &str, port: &str, payload: &[u8]) -> pb::PortTrafficRecord {
        rec_at(0, sequence, instance, port, payload)
    }

    fn rec_at(tai_ns: i64, sequence: u64, instance: &str, port: &str, payload: &[u8]) -> pb::PortTrafficRecord {
        pb::PortTrafficRecord { instance: instance.to_string(), port: port.to_string(), direction: 0, tai_ns, payload: payload.to_vec(), sequence }
    }

    /// **The primary key is `tai_ns`, ascending** (question 181), even when the lower-priority
    /// keys all point the other way: the record here with the LATER epoch has the smaller
    /// `sequence` and the alphabetically-earlier `instance`/`port`, so an implementation still
    /// sorting `(sequence, instance, port)` -- exactly what M25.4a shipped -- puts it first and
    /// fails this test.
    #[test]
    fn sorts_by_epoch_first_even_when_sequence_disagrees() {
        let mut records = vec![rec_at(2_000, 0, "a", "a", b"late-epoch-seq-0"), rec_at(1_000, 9, "z", "z", b"early-epoch-seq-9")];
        sort_port_traffic(&mut records);
        assert_eq!(records.iter().map(|r| r.tai_ns).collect::<Vec<_>>(), vec![1_000, 2_000], "epoch is the primary key, ahead of sequence");
        assert_eq!(records.iter().map(|r| r.sequence).collect::<Vec<_>>(), vec![9, 0], "and sequence really did disagree, so this could not have passed by coincidence");
    }

    /// This is the real shape question 181 exists for, in miniature: a declared command dispatch
    /// is handed to the router before the first output tick, so it carries `sequence = 0` with a
    /// mid-run epoch, while every tick-driven record around it has a real sequence. Epoch-first
    /// puts the dispatch where its epoch says it belongs rather than at the very front of the
    /// whole log.
    #[test]
    fn a_sequence_zero_dispatch_sorts_by_its_epoch_not_at_the_front_of_the_log() {
        let mut records = vec![
            rec_at(53_000, 54, "flight", "ack_out", b"ack"),
            rec_at(50_000, 0, "ground", "cmd_out", b"cmd"),
            rec_at(1_000, 1, "ground", "tick", b"first-tick"),
        ];
        sort_port_traffic(&mut records);
        assert_eq!(records.iter().map(|r| r.tai_ns).collect::<Vec<_>>(), vec![1_000, 50_000, 53_000], "the sequence-0 dispatch belongs between the first tick and the ack, by epoch");
    }

    /// Records sharing one `tai_ns` break the tie by `sequence`, ascending -- two frames the
    /// router carried at the same epoch in different ticks still order by tick.
    #[test]
    fn breaks_an_epoch_tie_by_sequence() {
        let mut records = vec![rec_at(7_000, 5, "a", "p", b""), rec_at(7_000, 1, "a", "p", b"")];
        sort_port_traffic(&mut records);
        assert_eq!(records.iter().map(|r| r.sequence).collect::<Vec<_>>(), vec![1, 5]);
    }

    /// Two records sharing `(tai_ns, sequence)` break the tie by `instance`, ascending.
    #[test]
    fn breaks_a_sequence_tie_by_instance() {
        let mut records = vec![rec(1, "zebra", "p", b""), rec(1, "alpha", "p", b"")];
        sort_port_traffic(&mut records);
        assert_eq!(records.iter().map(|r| r.instance.as_str()).collect::<Vec<_>>(), vec!["alpha", "zebra"]);
    }

    /// Two records sharing `(tai_ns, sequence, instance)` -- the case `tests/
    /// port_traffic_sidecar.rs`'s own module doc comment says `demo_command` cannot produce --
    /// break the tie by `port`, ascending. Proven here, directly against the sort function
    /// itself, since no fixture in this crate happens to exercise it end to end.
    #[test]
    fn breaks_a_sequence_and_instance_tie_by_port() {
        let mut records = vec![rec(7, "ground", "zzz_port", b""), rec(7, "ground", "aaa_port", b"")];
        sort_port_traffic(&mut records);
        assert_eq!(records.iter().map(|r| r.port.as_str()).collect::<Vec<_>>(), vec!["aaa_port", "zzz_port"]);
    }

    /// A full tie on `(tai_ns, sequence, instance, port)` -- only possible for two frames on the
    /// exact same port at the exact same epoch in the exact same step -- keeps the router's own
    /// emission order (a stable sort), never reordered by payload or anything else.
    ///
    /// **Measured, not assumed: `slice::sort_unstable_by` does NOT actually fail this test on
    /// this toolchain (Rust 1.97.0).** The obvious "wrong implementation" to break this against
    /// is `sort_unstable_by` (`slice::sort_by`'s own rustdoc: unstable makes no order guarantee
    /// for equal elements) -- tried directly, at 3, 40, and 2000 fully-tied elements, and none
    /// of the three reordered anything (this toolchain's pattern-defeating quicksort evidently
    /// leaves an all-equal-key partition untouched in practice, even though nothing in its own
    /// contract promises that). Reported honestly rather than kept as a test that looks like it
    /// proves something it does not: this test instead fails against a wrong implementation that
    /// actually IS observable -- one that reverses `records` before its own (otherwise correctly
    /// stable) sort, which order-preserving-among-ties definitionally cannot undo. This still
    /// pins the real contract (`PortTrafficLog.records`'s own proto doc comment: keep the
    /// router's own emission order); it just cannot be pinned against `sort_unstable_by`
    /// specifically on this toolchain, a fact worth recording rather than silently working
    /// around. Recorded by the lead alongside question 181.
    #[test]
    fn a_full_tie_keeps_the_original_emission_order_stable_sort() {
        let mut records: Vec<pb::PortTrafficRecord> = (0..40).map(|i| rec(3, "a", "p", format!("{i}").as_bytes())).collect();
        sort_port_traffic(&mut records);
        let order: Vec<String> = records.iter().map(|r| String::from_utf8(r.payload.clone()).unwrap()).collect();
        let expected: Vec<String> = (0..40).map(|i| i.to_string()).collect();
        assert_eq!(order, expected, "a full (tai_ns, sequence, instance, port) tie must keep the router's own emission order -- a stable sort's own guarantee");
    }
}

/// Parse and unit-typecheck one `Objective`/`MeasureOfEffectiveness.expression` against `run`
/// -- [`super::DrmError::InvalidExpression`] on either failure. Never touches
/// `Trajectory.samples` (`crate::expr::typecheck::check`'s own guarantee), so this is safe to
/// call before any instance has been propagated.
fn validate_expression_at_load(name: &str, expression: &str, run: &crate::expr::ExprRunProducts) -> Result<(), DrmError> {
    let expr = crate::expr::parse(expression).map_err(|e| DrmError::InvalidExpression { name: name.to_string(), reason: e.to_string() })?;
    crate::expr::check(&expr, run).map_err(|e| DrmError::InvalidExpression { name: name.to_string(), reason: e.to_string() })?;
    Ok(())
}

/// Run a `DesignReferenceMission` end to end: verify every declared hash, validate every
/// declared `Objective`/`MeasureOfEffectiveness` expression against the run's declared shape
/// (before any propagation), bind and run every `SosConfiguration.instances` entry through
/// [`HeteroKernel`] (honouring `DrmOptions`, injecting DYNAMICS faults), evaluate every
/// objective/measure against the real result, and return one [`RunProducts`]. See the module
/// doc comment for exactly how `DrmOptions` fields map to kernel behaviour and how scoring
/// works.
pub fn execute(cfg: RunConfig<'_>) -> Result<RunProducts, DrmError> {
    // M25.4b: `RunConfig.replay` combined with `DrmOptions.covariance` is refused first, before
    // even opening `RunConfig.replay.log_path` -- cheaper than the hash check just below (no
    // I/O), and there is no reason to make a caller supply, or this executor read, a real replay
    // log at all for a combination that will be refused regardless of what that file contains.
    // Question 107/M14.1's own "covariance path is unchanged" limitation, extended here:
    // `run_covariance_instance` never consults `RunConfig.replay` at all, so honouring it only on
    // the plain path while silently ignoring it under `DrmOptions.covariance` would be exactly
    // the "say so, never drop it quietly" failure mode this executor refuses everywhere else.
    // `cfg.drm.options` is read directly (not the `options` local a few lines below, parsed only
    // once Pass 0's other checks have run) since this is the very first thing this function does.
    if cfg.replay.is_some() && cfg.drm.options.as_ref().is_some_and(|o| o.covariance) {
        return Err(DrmError::ReplayWithCovarianceNotSupported);
    }

    // M25.4b: the replay log's hash is verified next -- before the canonical DRM/SOS/system hash
    // checks below, before any binding, before any GMAT call, and before any step
    // (`crate::drm::replay::verify_and_load`'s own doc comment). `replay_log` is `None` for
    // every non-replay run (unchanged behaviour); `Some(log)` is threaded down into
    // `run_shared_group` once `replay_targets` (below, after Pass 1) resolves which instances it
    // actually applies to.
    let replay_log: Option<pb::PortTrafficLog> = match &cfg.replay {
        Some(rc) => Some(replay::verify_and_load(rc)?),
        None => None,
    };

    // M18.4 (`docs/open-questions.md` question 127): namespace every GMAT object this call's own
    // model materializations construct, so this `execute()` invocation can never collide with a
    // GMAT object another invocation (in this same process, GMAT's configuration being
    // process-global) already registered under the identical name -- see `gmat_ns`'s own use in
    // `materialize_plan`/`materialize_plan_at_boundary` and `binding::materialize_gmat`'s own doc
    // comment for exactly what this namespaces and, just as importantly, what it deliberately does
    // NOT touch (`name_suffix`/`ModelInfo.id`/`dynamics_hash`/any golden -- gmat_ns never reaches
    // any of them). Computed once per `execute()` call, not per instance/segment, and threaded
    // down through both the shared-group and covariance paths.
    let gmat_ns = gmat_execution_namespace(&cfg.run_id);

    // -- Canonical hash verification (question 87's central requirement: refuse a tampered
    // artifact, never merely warn). --
    let computed_drm_hash = hash::verify_drm_hash(cfg.drm)?;
    let computed_sos_hash = hash::verify_sos_hash(cfg.sos)?;
    let mut system_hashes = BTreeMap::new();
    for (id, sys) in cfg.systems {
        system_hashes.insert(id.clone(), hash::verify_system_hash(id, sys)?);
    }
    if cfg.drm.sos_configuration_id != cfg.sos.id {
        return Err(DrmError::UnknownSosConfiguration { declared: cfg.drm.sos_configuration_id.clone(), provided: cfg.sos.id.clone() });
    }

    // Question 108/109: `SosConfiguration.connections` validated against every named instance's
    // declared ports before any instance runs -- an undeclared port, a direction mismatch, a
    // kind mismatch, or an unsupported link_model is a typed load error, never discovered only
    // once two instances actually try to exchange a message (see `crate::router`'s own module
    // doc comment). **M14.1: the built `Router` is now threaded into Pass 2's shared kernel run**
    // (`run_shared_group`, below) -- before this task it was built only to validate and then
    // discarded, since no instance ran on a kernel any other instance shared. An empty
    // `connections` list still builds trivially and delivers nothing, so every DRM declaring no
    // ports/connections (every existing golden) is unaffected.
    let mut router = crate::router::Router::build(cfg.sos, cfg.systems).map_err(DrmError::Router)?;

    let options = cfg.drm.options.ok_or_else(|| DrmError::InvalidDrmOptions { reason: "DesignReferenceMission.options is unset".to_string() })?;
    if options.real_time {
        return Err(DrmError::RealTimeNotSupported);
    }
    if options.sample_interval_s <= 0.0 {
        return Err(DrmError::InvalidDrmOptions { reason: format!("sample_interval_s must be positive, got {}", options.sample_interval_s) });
    }
    let output_period_ns = (options.sample_interval_s * 1e9).round() as i64;

    let scenario = cfg.drm.scenario.clone().ok_or_else(|| DrmError::InvalidDrmOptions { reason: "DesignReferenceMission.scenario is unset".to_string() })?;
    if scenario.end_tai_ns <= scenario.start_tai_ns {
        return Err(DrmError::InvalidDrmOptions { reason: format!("Scenario.end_tai_ns ({}) must be after start_tai_ns ({})", scenario.end_tai_ns, scenario.start_tai_ns) });
    }
    if (scenario.end_tai_ns - scenario.start_tai_ns) % output_period_ns != 0 {
        return Err(DrmError::InvalidDrmOptions {
            reason: format!("Scenario duration ({} ns) is not an exact multiple of sample_interval_s's period ({output_period_ns} ns)", scenario.end_tai_ns - scenario.start_tai_ns),
        });
    }

    // Every fault names a known instance, and every DYNAMICS fault lands exactly on the
    // output sampling grid -- checked up front, before any binding or GMAT call, per the
    // module doc comment's "Fault injection" section. M16.2 (question 120): a HARDWARE fault
    // gets the identical grid check now too -- a container power-cycle boundary is driven
    // through the exact same `run_one_span`/`HeteroKernel::run_with_ports` machinery a DYNAMICS
    // boundary is (`run_shared_group`'s own boundary loop, below), whose own horizon-must-be-
    // an-exact-multiple-of-the-output-period `assert!` this check exists to keep unreachable;
    // through M15.3 a power cycle got this protection "for free" by being DYNAMICS -- moving it
    // to HARDWARE without extending this check here would have silently reopened exactly the
    // panic this check exists to prevent, for an off-grid power-cycle epoch.
    for f in &scenario.faults {
        if !cfg.sos.instances.iter().any(|i| i.name == f.instance) {
            return Err(DrmError::UnknownFaultInstance { fault_id: f.id.clone(), instance: f.instance.clone() });
        }
        // `docs/open-questions.md` question 178 (R5.1a): a SENSOR fault naming a star-tracker
        // instance now has a real runtime; one naming an IMU instance is still a typed load
        // refusal (R5.1b's own scope); one naming anything else is refused as not a sensor
        // instance at all. Checked here, before any binding or GMAT call, the same "checked up
        // front" pattern `DrmError::UnknownFaultInstance`/`FaultEpochNotOnSampleGrid`
        // (immediately above/below) already follow.
        if f.target_kind == FaultTargetKind::Sensor as i32 {
            let instance = cfg
                .sos
                .instances
                .iter()
                .find(|i| i.name == f.instance)
                .expect("DrmError::UnknownFaultInstance was already checked, and returned, for this same fault immediately above");
            let sys = cfg.systems.get(&instance.system_id).ok_or_else(|| DrmError::UnknownSystemDefinition { instance: instance.name.clone(), system_id: instance.system_id.clone() })?;
            match crate::registry::kind_for(&sys.dynamics_model) {
                crate::registry::ModelKind::Imu => {
                    return Err(DrmError::PortOrSensorFaultNotYetSupported { fault_id: f.id.clone(), instance: f.instance.clone(), target_kind: FaultTargetKind::Sensor.as_str_name().to_string() });
                }
                crate::registry::ModelKind::StarTracker => {
                    if !fault::SENSOR_KINDS.contains(&f.kind.as_str()) {
                        return Err(DrmError::UnknownSensorFaultKind { fault_id: f.id.clone(), instance: f.instance.clone(), kind: f.kind.clone() });
                    }
                    // Question 178, item 3: both the start AND (when declared, `duration_ns >
                    // 0`) the end epoch must land on the output sample grid -- the DYNAMICS/
                    // HARDWARE check below only ever checks the one epoch a DYNAMICS/HARDWARE
                    // fault has; a windowed SENSOR fault has two, since the executor synthesizes
                    // a second re-materialization boundary at the window's own end (`Boundary::
                    // SensorFaultEnd`, `run_shared_group`'s own boundary loop, below).
                    if (f.tai_ns - scenario.start_tai_ns) % output_period_ns != 0 {
                        return Err(DrmError::FaultEpochNotOnSampleGrid { fault_id: f.id.clone(), tai_ns: f.tai_ns, sample_interval_s: options.sample_interval_s });
                    }
                    if f.duration_ns > 0 {
                        let end_tai_ns = f.tai_ns + f.duration_ns;
                        if (end_tai_ns - scenario.start_tai_ns) % output_period_ns != 0 {
                            return Err(DrmError::FaultEpochNotOnSampleGrid { fault_id: f.id.clone(), tai_ns: end_tai_ns, sample_interval_s: options.sample_interval_s });
                        }
                    }
                }
                _ => {
                    return Err(DrmError::SensorFaultTargetNotASensor { fault_id: f.id.clone(), instance: f.instance.clone(), binding: sys.dynamics_model.clone() });
                }
            }
            continue;
        }
        // R4.1a/R4.1b (question 178): PORT now has a real runtime for all four of its documented
        // kinds -- `crate::router::Router::install_port_faults` (below, once `router` and
        // `scenario` both exist) does the REST of a PORT fault's own load-time validation
        // (declared port, FRAMED/BYTE_STREAM, seed, delay_s/corrupt_mask, rate, clear, and R4.1b's
        // own overlapping-window refusal); this loop's own job is only to refuse a `kind` outside
        // ADR-005 section 5's whole vocabulary entirely ([`DrmError::UnknownPortFaultKind`])
        // before that call ever runs -- every kind still in the vocabulary (`"drop"`, `"delay"`,
        // `"corrupt"`, `"duplicate"`) falls through to `install_port_faults` for the rest of its
        // own validation. This loop does NOT apply the DYNAMICS/HARDWARE sample-grid check below
        // to a PORT fault either way: a PORT fault splits no segment, so it need not land on that
        // grid (`crate::router`'s own module doc comment, "Port fault runtime," "Epoch grid").
        if f.target_kind == FaultTargetKind::Port as i32 {
            if !fault::PORT_KINDS.contains(&f.kind.as_str()) {
                return Err(DrmError::UnknownPortFaultKind { fault_id: f.id.clone(), instance: f.instance.clone(), kind: f.kind.clone() });
            }
            continue;
        }
        if (f.target_kind == FaultTargetKind::Dynamics as i32 || f.target_kind == FaultTargetKind::Hardware as i32) && (f.tai_ns - scenario.start_tai_ns) % output_period_ns != 0 {
            return Err(DrmError::FaultEpochNotOnSampleGrid { fault_id: f.id.clone(), tai_ns: f.tai_ns, sample_interval_s: options.sample_interval_s });
        }
    }
    // Question 178/184/186(b) (R5.1a): refuse two SENSOR faults on the same instance with
    // overlapping windows -- see `DrmError::OverlappingSensorFaultWindows`'s own doc comment for
    // why this is keyed on `instance` alone, not `(instance, target)` (PORT's own key, at
    // `Router::install_port_faults`). Every SENSOR fault in `scenario.faults` reaching this call
    // has already been validated (instance is a real sensor, kind is real) by the loop just
    // above, which returns before this point on any earlier failure.
    fault::validate_no_overlapping_sensor_fault_windows(&scenario.faults)?;
    // R4.1a/R4.1b (question 178): resolve and install every FAULT_TARGET_KIND_PORT fault (every
    // kind in `fault::PORT_KINDS` -- the loop just above already refused any other PORT shape)
    // onto `router` -- see `crate::router`'s own module doc comment's "Port fault runtime"
    // section for the full validation this performs (declared port, FRAMED/BYTE_STREAM, seed,
    // delay_s/corrupt_mask, rate, clear, overlapping windows on the same (instance, port)).
    // Before any binding or GMAT call, and before Pass 1's own classification -- `router` (built
    // above, right after the canonical hash checks) already has every declared port's own
    // PortKind, from every `SosConfiguration.instances` entry's own `SystemDefinition`, so
    // nothing further needs to happen first. `output_period_ns` (computed above, from
    // `options.sample_interval_s`) is R4.1b's own addition -- `"duplicate"` needs the run's own
    // output period and the Router has no other way to learn it (`Router::install_port_faults`'s
    // own doc comment). `RouterError::MissingFaultSeed` is mapped to the crate-wide
    // `DrmError::MissingFaultSeed` (reusing the existing variant, per this crate's own convention
    // for a seed check -- ADR-004 "seeds are logged inputs"); every other `RouterError` wraps
    // generically through `DrmError::Router`, the same way `Router::build`'s own connection
    // validation already does, immediately above.
    router.install_port_faults(&scenario.faults, &scenario.seeds, output_period_ns).map_err(|e| match e {
        crate::router::RouterError::MissingFaultSeed { fault_id } => DrmError::MissingFaultSeed { fault_id },
        other => DrmError::Router(other),
    })?;

    // Question 97: every `Scenario.events` entry is parsed as a typed maneuver, EXCEPT a
    // `kind == "command"` entry (M25.2, `docs/sil-plan.md`'s M25 milestone), which gets `command::
    // parse`'s own typed contract instead -- `crate::drm::schema` already validated this same
    // split at YAML load time (`RawScenarioEvent::into_pb`'s own identical dispatch), but a
    // `pb::ScenarioEvent` built directly, bypassing the loader, is validated here too, so the
    // typed contract is not bypassable either way. Every maneuver/command names a known instance
    // and lands exactly on the output sampling grid -- checked up front, before any binding or
    // GMAT call, exactly like DYNAMICS faults are above.
    let mut maneuvers: Vec<ParsedManeuver> = Vec::new();
    let mut commands: Vec<command::ParsedCommand> = Vec::new();
    for event in &scenario.events {
        if event.kind == command::COMMAND_KIND {
            let c = command::parse(event)?;
            if !cfg.sos.instances.iter().any(|i| i.name == c.instance) {
                return Err(DrmError::UnknownCommandInstance { id: c.id, instance: c.instance });
            }
            if !cfg.sos.instances.iter().any(|i| i.name == c.from) {
                return Err(DrmError::UnknownCommandSender { id: c.id, sender: c.from, reason: "not in this SosConfiguration".to_string() });
            }
            if (c.tai_ns - scenario.start_tai_ns) % output_period_ns != 0 {
                return Err(DrmError::CommandEpochNotOnSampleGrid { id: c.id, tai_ns: c.tai_ns, sample_interval_s: options.sample_interval_s });
            }
            commands.push(c);
            continue;
        }
        let m = maneuver::parse(event)?;
        if !cfg.sos.instances.iter().any(|i| i.name == m.instance) {
            return Err(DrmError::UnknownManeuverInstance { id: m.id, instance: m.instance });
        }
        if (m.tai_ns - scenario.start_tai_ns) % output_period_ns != 0 {
            return Err(DrmError::ManeuverEpochNotOnSampleGrid { id: m.id, tai_ns: m.tai_ns, sample_interval_s: options.sample_interval_s });
        }
        // Question 100: a declared execution_error.seed must name a real Scenario.seeds key --
        // checked here too (schema::RawScenario::into_pb already checks it for anything that
        // went through the YAML loader), so a Scenario built directly in Rust, bypassing the
        // loader (as this crate's own tests do), gets the same refusal before any instance runs.
        maneuver::validate_execution_error_seed(&m, &scenario.seeds)?;
        maneuvers.push(m);
    }
    // M25.2: `command::assign_sequence_numbers` needs a stable order -- `(tai_ns, id)`, the same
    // tie-break `events::epoch_id_order`/`maneuver::epoch_id_order` already use everywhere else
    // in this module.
    commands.sort_by(|a, b| (a.tai_ns, a.id.as_str()).cmp(&(b.tai_ns, b.id.as_str())));

    // -- Pass 1: classify every instance (binding::classify_binding touches no GMAT state --
    // see that module's own doc comment), validate its declared state space, and compute its
    // effective period -- no propagation happens in this pass. --
    let mut plans: BTreeMap<String, (BindingPlan, i64)> = BTreeMap::new();
    // Question 107: a BINDING_KIND_CONTAINER instance classifies to a `binding::ContainerSpec`
    // (`binding::Classification::Container`), never a `BindingPlan` -- `BindingPlan` itself is
    // unchanged by M13.2 (still exactly the two variants `crate::drm::fault`/`crate::registry`,
    // neither owned by this task, already match exhaustively; see `BindingPlan`'s own doc
    // comment) -- so a container-bound instance's plan lives in this sibling map instead,
    // keyed the same way.
    let mut container_plans: BTreeMap<String, (binding::ContainerSpec, i64)> = BTreeMap::new();
    let mut declared_trajectories: BTreeMap<String, Trajectory> = BTreeMap::new();
    let mut declared_outputs_by_instance: BTreeMap<String, Vec<(String, Unit)>> = BTreeMap::new();
    for instance in &cfg.sos.instances {
        let sys = cfg.systems.get(&instance.system_id).ok_or_else(|| DrmError::UnknownSystemDefinition { instance: instance.name.clone(), system_id: instance.system_id.clone() })?;
        // M10.3: `classify_binding` recognizes "output.*" directly now (see
        // OUTPUT_PARAMETER_PREFIX's doc comment) -- the real, unmodified `sys` goes straight in,
        // no filtered copy needed.
        let classification = binding::classify_binding(instance, sys, &options)?;
        declared_outputs_by_instance.insert(instance.name.clone(), declared_outputs(sys));
        // ADR-001 / questions 88 and 94: the state space must be declared, not a bare id
        // string. A declared `SystemDefinition.state_space` is authoritative (its id must equal
        // `state_space_id`, and every component must classify under ADR-005 section 3); absent
        // one, the id resolves against the built-in registry. Refused at load, before any
        // propagation, so a bad shape fails fast rather than after a 24-second GMAT run (M9.2).
        // M20.1 (question 133): a `"native."`-dispatched instance's own declared-dimension-vs-
        // model-state_dim check (`DrmError::StateSpaceDimensionMismatch`) already happened
        // inside `classify_binding` above, GMAT-free, alongside its other binding-kind-specific
        // checks (`binding::CONSTANT_ACCEL_STATE_DIM`'s own doc comment) -- this call only
        // re-runs the binding-kind-agnostic ADR-005 sec 3 check every instance needs.
        crate::trajectory::resolve_state_space(sys).map_err(|e| DrmError::InvalidStateSpace {
            instance: instance.name.clone(),
            reason: e.to_string(),
        })?;

        let step_rate_hz = effective_step_rate_hz(instance, &options);
        if step_rate_hz <= 0.0 {
            return Err(DrmError::InvalidDrmOptions {
                reason: format!("instance {:?}: neither its own step_rate_hz nor DrmOptions.default_step_rate_hz is positive", instance.name),
            });
        }
        let period_ns = (1e9 / step_rate_hz).round() as i64;

        declared_trajectories.insert(instance.name.clone(), declared_shape_trajectory(&instance.name, &sys.state_space_id));
        match classification {
            binding::Classification::Model(plan) => {
                plans.insert(instance.name.clone(), (plan, period_ns));
            }
            binding::Classification::Container(spec) => {
                container_plans.insert(instance.name.clone(), (spec, period_ns));
            }
        }
    }

    // M25.4b: which instances this run replays -- resolved here, once, now that `container_plans`
    // (needed for the "empty means every BINDING_KIND_CONTAINER instance" default) is fully
    // populated. `RunConfig.replay.instances` naming an instance absent from `SosConfiguration.
    // instances` is refused BEFORE this (checked against `cfg.sos.instances` directly, not
    // `plans`/`container_plans`, so it also catches a name that classified into neither map --
    // structurally impossible today since every instance classifies into exactly one, but this
    // keeps the check meaningful even if that ever changes) -- see `crate::drm::replay`'s own
    // module doc comment for the full "instances" contract.
    let replay_targets: std::collections::BTreeSet<String> = match &cfg.replay {
        None => std::collections::BTreeSet::new(),
        Some(rc) if rc.instances.is_empty() => container_plans.keys().cloned().collect(),
        Some(rc) => {
            for name in &rc.instances {
                if !cfg.sos.instances.iter().any(|i| &i.name == name) {
                    return Err(DrmError::UnknownReplayInstance { instance: name.clone() });
                }
            }
            rc.instances.iter().cloned().collect()
        }
    };

    // Question 107: a BINDING_KIND_CONTAINER instance does not yet support DYNAMICS faults or
    // maneuvers (a container instance is never itself a fault/maneuver boundary's own target --
    // see `run_shared_group`'s own doc comment) -- refused here, before any binding or network
    // call, the
    // same "checked up front" pattern every other fault/maneuver validation in this function
    // already follows. Checked only now (not inside the fault/maneuver-parsing loops above)
    // because it needs `container_plans`/`plans`, populated by the classification pass just
    // above.
    //
    // M16.2 (question 120): a container power cycle is FAULT_TARGET_KIND_HARDWARE now, not
    // DYNAMICS (`fault::is_container_power_cycle`) -- so unlike through M15.3, *every* DYNAMICS
    // fault naming a container instance is refused below, with no exception. Three checks, all
    // load-time, all before any binding or network call:
    //
    // 1. The retired M15.3 shape (`fault::is_legacy_dynamics_power_cycle`) is refused by its own
    //    specific name (`DrmError::PowerCycleFaultMustTargetHardware`) rather than falling
    //    through to the generic container-fault refusal below or to `apply_dynamics_fault`'s own
    //    generic "kind != parameter" refusal, both of which would be technically true but would
    //    not say what the caller actually needs to do (retarget to HARDWARE) -- checked first so
    //    it always wins over the more generic checks that would otherwise also match.
    // 2. Every other DYNAMICS fault naming a container instance -- unchanged in effect from
    //    M15.3, just without the power-cycle carve-out (case 1 above already claimed that shape).
    // 3. A HARDWARE fault is refused unless it is exactly a container power cycle
    //    (`fault::is_container_power_cycle`): a HARDWARE fault whose `kind` is not
    //    `"power_cycle"` naming a container is refused by kind
    //    (`DrmError::HardwareFaultKindNotSupported` -- a container has no Renode peripheral or
    //    board to act on); a HARDWARE fault naming anything other than a container (a
    //    BINDING_KIND_MODEL instance) is refused by target
    //    (`DrmError::HardwareFaultNotSupportedOnInstance` -- HARDWARE has no meaning yet for a
    //    model instance). Without this, either shape would otherwise be silently dropped later
    //    (`run_shared_group`'s own boundary-collection pass only ever pushes a HARDWARE fault
    //    that *is* `fault::is_container_power_cycle` naming a container) -- exactly the "say so,
    //    never drop it quietly" rule every other refusal in this executor follows, extended here
    //    to the one gap the DYNAMICS-era carve-out never had to consider (DYNAMICS always names
    //    the fault-collecting loop's own two known dispositions; HARDWARE now needs its own).
    for f in &scenario.faults {
        if fault::is_legacy_dynamics_power_cycle(f) {
            return Err(DrmError::PowerCycleFaultMustTargetHardware { fault_id: f.id.clone(), instance: f.instance.clone() });
        }
        if f.target_kind == FaultTargetKind::Dynamics as i32 && container_plans.contains_key(&f.instance) {
            return Err(DrmError::ContainerFaultsOrManeuversNotSupported { instance: f.instance.clone() });
        }
        if f.target_kind == FaultTargetKind::Hardware as i32 {
            if container_plans.contains_key(&f.instance) {
                if f.kind != fault::POWER_CYCLE_KIND {
                    return Err(DrmError::HardwareFaultKindNotSupported { fault_id: f.id.clone(), instance: f.instance.clone(), kind: f.kind.clone() });
                }
            } else if plans.contains_key(&f.instance) {
                return Err(DrmError::HardwareFaultNotSupportedOnInstance { fault_id: f.id.clone(), instance: f.instance.clone() });
            }
            // Neither map contains `f.instance`: already refused above, `DrmError::
            // UnknownFaultInstance`, before this loop is ever reached -- every instance
            // classifies into exactly one of `plans`/`container_plans` (Pass 1, just above), so
            // an instance passing that earlier check is guaranteed to be in one of the two here.
        }
    }
    for m in &maneuvers {
        if container_plans.contains_key(&m.instance) {
            return Err(DrmError::ContainerFaultsOrManeuversNotSupported { instance: m.instance.clone() });
        }
    }

    // -- Load-time expression validation (question 93 / DrmError::InvalidExpression), strictly
    // before any instance is propagated -- see the module doc comment's "Scoring" section, and
    // its "Events (question 95, M9.3) and outputs" section for why `declared_events`/
    // `crate::expr::speed_output` give this pass the same event names/output names (by name and
    // unit, not by real value) the real run below will produce. --
    let instance_names: Vec<String> = cfg.sos.instances.iter().map(|i| i.name.clone()).collect();
    let declared_events_vec = events::declared_events(&scenario, &instance_names, &maneuvers, cfg.error_mode);
    let mut declared_run = crate::expr::ExprRunProducts::new(scenario.start_tai_ns, scenario.end_tai_ns, &declared_trajectories, &declared_events_vec);
    for (name, traj) in &declared_trajectories {
        if let Some(series) = crate::expr::speed_output(traj) {
            declared_run = declared_run.with_output(name, crate::expr::SPEED_OUTPUT_NAME, series);
        }
    }
    // Question 95's second half (task M10.2): every declared "output.<name>" (see
    // OUTPUT_PARAMETER_PREFIX's doc comment) gets an empty-but-present series at load time too,
    // by name and unit only -- the same "declared shape, no real samples yet" contract
    // `declared_shape_trajectory`/`speed_output` above already give load-time validation; the
    // real values are attached to `run_view` below, after propagation.
    for (name, decls) in &declared_outputs_by_instance {
        for (output_name, unit) in decls {
            declared_run = declared_run.with_output(name, output_name, crate::expr::Series { epochs_tai_ns: vec![], values: vec![], unit: *unit });
        }
    }
    for obj in &cfg.drm.objectives {
        validate_expression_at_load(&obj.name, &obj.expression, &declared_run)?;
    }
    for moe in &cfg.drm.measures {
        validate_expression_at_load(&moe.name, &moe.expression, &declared_run)?;
    }

    // -- Pass 2: actually run every instance (GMAT propagation happens here), collecting every
    // real Event each instance's run produced (question 95, M9.3) along the way. --
    //
    // **M14.1 (question 109): one shared kernel run, not one loop per instance.** When
    // `options.covariance` is false, every `BINDING_KIND_MODEL` and `BINDING_KIND_CONTAINER`
    // instance is driven through exactly one [`run_shared_group`] call -- see that function's
    // own doc comment for the boundary-splitting/container-period rules this entails. The
    // covariance path is unchanged: every model instance still runs on its own isolated loop
    // ([`run_covariance_instance`]), a container instance is still refused
    // (`DrmError::ModelNotStmCapable`, exactly as before this task), and this limitation is
    // recorded in the run's own provenance (`build_run_provenance`'s `"kernel_run_mode"`
    // attribute, below).
    let mut trajectories = BTreeMap::new();
    let mut all_events: Vec<Event> = Vec::new();
    // Question 173 (M25.3): every CDM `Measurement` this whole run produced -- only ever filled
    // from the shared-group (non-covariance) path below; the covariance path produces none
    // (every model it can even reach must be `stm_capable()`, which neither sensor model in this
    // workspace declares -- `DrmError::ModelNotStmCapable` already refuses a covariance run
    // naming one before any measurement code could run). Sorted `(epoch_ns, measurement_id)`
    // just before `RunProducts` is built, below -- `RunProducts.measurements`'s own proto doc
    // comment states this ordering as part of the contract.
    let mut all_measurements: Vec<av_cdm::pb::Measurement> = Vec::new();
    // Question 95's second half (task M10.2), carried on the covariance path too as of question
    // 101 (M11.2): every real `StepResult.outputs` series each instance's run actually produced,
    // name -> (epochs, values) -- raw, not yet filtered against `declared_outputs_by_instance`
    // (done below, once `run_view` exists). Empty only for an instance whose model never
    // populates `StepResult.outputs` at all (every model except a GMAT-bound `gmat_sys::model
    // ::GmatModel`) -- no longer unconditionally empty for every covariance-path instance the
    // way it was through M11.1 (see `run_covariance_instance`'s own doc comment).
    let mut outputs_by_instance: BTreeMap<String, NamedOutputSeries> = BTreeMap::new();

    // M25.2: command dispatch (`run_shared_group`'s own new pre-seeding, below) is wired into
    // the shared-kernel path only -- the per-instance covariance path (`run_covariance_instance`,
    // immediately below) has no `Router` participation at all today (M13.3's own scope). Refusing
    // here, typed, is what keeps a declared `command` event from being silently dropped rather
    // than dispatched, if a DRM ever combines `options.covariance` with a `command` event; wiring
    // the covariance path itself is out of this task's own scope (see `drms/M25_2_REPORT.md`).
    if options.covariance && !commands.is_empty() {
        return Err(DrmError::InvalidDrmOptions { reason: "DrmOptions.covariance and a declared \"command\" Scenario.events entry are not supported together yet (M25.2's own scope: command dispatch is wired into the shared-kernel/Router path only)".to_string() });
    }

    if options.covariance {
        for instance in &cfg.sos.instances {
            let sys = cfg.systems.get(&instance.system_id).expect("validated in pass 1");
            let sys_hash = system_hashes.get(&instance.system_id).expect("verified above").clone();

            // A container-bound instance is never STM-capable (it carries no ODE state at
            // all) -- refused the same way the native ConstantAccelModel's own covariance
            // request already is, DrmError::ModelNotStmCapable, rather than a
            // container-specific variant for what is really the identical situation.
            if container_plans.contains_key(&instance.name) {
                return Err(DrmError::ModelNotStmCapable { instance: instance.name.clone() });
            }
            let instance_faults: Vec<&Fault> = scenario.faults.iter().filter(|f| f.instance == instance.name && f.target_kind == FaultTargetKind::Dynamics as i32).collect();
            if !instance_faults.is_empty() {
                return Err(DrmError::CovarianceWithFaultsNotSupported { instance: instance.name.clone() });
            }
            let instance_maneuvers: Vec<&ParsedManeuver> = maneuvers.iter().filter(|m| m.instance == instance.name).collect();
            let (plan, period_ns) = plans.get(&instance.name).expect("populated in pass 1 (every instance is in exactly one of plans/container_plans)");
            let (traj, events, outputs) =
                run_covariance_instance(
                    cfg.gmat,
                    plan,
                    sys,
                    instance,
                    &scenario,
                    &options,
                    output_period_ns,
                    *period_ns,
                    &instance_maneuvers,
                    cfg.error_mode,
                    &computed_sos_hash,
                    &sys_hash,
                    &cfg.run_id,
                    &gmat_ns,
                )?;
            all_events.extend(events);
            outputs_by_instance.insert(instance.name.clone(), outputs);
            let traj = convert_gmat_trajectory_to_declared_frame(cfg.gmat, plan, &gmat_ns, &instance.name, traj)?;
            let finished = finish_trajectory(traj, &computed_drm_hash, &computed_sos_hash, &scenario, &cfg.run_id, sys, &sys_hash);
            trajectories.insert(instance.name.clone(), finished);
        }
    } else {
        let instances_by_name: BTreeMap<String, &SystemInstance> = cfg.sos.instances.iter().map(|i| (i.name.clone(), i)).collect();
        let (shared_trajectories, shared_events, shared_outputs, container_binding_hashes, shared_measurements) = run_shared_group(
            cfg.gmat,
            &plans,
            &container_plans,
            &instances_by_name,
            cfg.systems,
            &system_hashes,
            &scenario,
            &options,
            output_period_ns,
            &maneuvers,
            &commands,
            cfg.error_mode,
            &computed_sos_hash,
            &cfg.run_id,
            &gmat_ns,
            &mut router,
            &replay_targets,
            replay_log.as_ref(),
        )?;
        all_events.extend(shared_events);
        all_measurements.extend(shared_measurements);
        for instance in &cfg.sos.instances {
            let sys = cfg.systems.get(&instance.system_id).expect("validated in pass 1");
            let sys_hash = system_hashes.get(&instance.system_id).expect("verified above").clone();
            let traj = shared_trajectories.get(&instance.name).expect("run_shared_group returns every plans/container_plans instance").clone();
            // Question 128, M19.1: a container-bound instance has no `plans` entry at all (it is
            // in `container_plans` instead) -- no GMAT central body/integration frame concept
            // applies to it, so `None` here passes `traj` through unchanged, exactly like
            // `convert_gmat_trajectory_to_declared_frame`'s own `BindingPlan::ConstantAccel` arm
            // does for a native (non-container) instance.
            let traj = match plans.get(&instance.name) {
                Some((plan, _period_ns)) => convert_gmat_trajectory_to_declared_frame(cfg.gmat, plan, &gmat_ns, &instance.name, traj)?,
                None => traj,
            };
            // M21.3 (`docs/open-questions.md` question 141, decided by the lead): an instance
            // whose materialized native model has zero physical state (an empty declared state
            // space -- `ConstantAccelSpec::x0_si` empty, `ConstantAccelModel::state_dim() ==
            // 0`) emits no trajectory entry at all, not a zero-width or empty-sample one --
            // nothing in `RunProducts.trajectories` for it. Its own events are unaffected: they
            // reach `all_events` from `shared_events` above unconditionally, entity-tagged
            // independently of `trajectories` (same split question 133 already established for
            // rendering, taken one step further here for the producer itself).
            // M22.2b: converted from a two-way `matches!` to an exhaustive match over every
            // `BindingPlan` variant -- the brief's own "no catch-all `_ =>`" rule applied here,
            // not just to `binding.rs`/`fault.rs`. `StarTrackerModel::state_dim() == 0` always
            // (an instantaneous measurement transform has no propagated physical state, exactly
            // like an empty-declared-state-space `ConstantAccel` plan), so a star tracker
            // instance emits no trajectory either -- deliberately, not merely because it happens
            // to fall through some default. `ImuModel::state_dim() == 6` (the bias random walk
            // IS real, meaningful propagated state), so an IMU instance emits its trajectory
            // exactly like `Gmat`/`Attitude` always do.
            let emits_no_trajectory = match plans.get(&instance.name) {
                Some((BindingPlan::ConstantAccel(spec), _)) => spec.x0_si.is_empty(),
                Some((BindingPlan::StarTracker(_), _)) => true,
                Some((BindingPlan::Imu(_), _)) => false,
                Some((BindingPlan::Gmat(_), _)) => false,
                Some((BindingPlan::Attitude(_), _)) => false,
                // M22.4: `AttitudeControllerModel::state_dim() == 0` always (a control law with
                // no integrator state has nothing to propagate) -- same "emits no trajectory"
                // rule as `StarTracker` above.
                Some((BindingPlan::Controller(_), _)) => true,
                // M25.1: `GroundStationModel::state_dim() == 0` always (a fixed geodetic site
                // with an instantaneous visibility transform has nothing to propagate) -- same
                // "emits no trajectory" rule as `StarTracker`/`Controller` above.
                Some((BindingPlan::GroundStation(_), _)) => true,
                None => false,
            };
            if !emits_no_trajectory {
                let mut finished = finish_trajectory(traj, &computed_drm_hash, &computed_sos_hash, &scenario, &cfg.run_id, sys, &sys_hash);
                // Question 107's "binding_hash into provenance": LockstepBindResponse.binding_hash,
                // recorded on this instance's own Trajectory.provenance.attributes -- the same place
                // finish_trajectory already records system_definition_hash/id -- rather than a new
                // Trajectory field (proto/** is read-only to this task).
                if let Some(hash) = container_binding_hashes.get(&instance.name) {
                    if let Some(prov) = finished.provenance.as_mut() {
                        prov.attributes.insert("container_binding_hash".to_string(), hash.clone());
                    }
                }
                // M15.2 (question 116): the M14.4-era "held_sample_tai_ns" provenance attribute is
                // gone -- `TrajectorySample.kind` (`av_cdm::pb::SampleKind::Held`) now carries this
                // distinction directly on the sample, so there is nothing left to attach here. See
                // `run_shared_group`'s own doc comment's "Container period vs. the trajectory's own
                // output grid" section.
                trajectories.insert(instance.name.clone(), finished);
            }
            outputs_by_instance.insert(instance.name.clone(), shared_outputs.get(&instance.name).cloned().unwrap_or_default());
        }
    }
    // ADR-005 sec 5's `(epoch, id)` ordering, applied to every Event this run produced (see
    // `super::events::epoch_id_order`'s own doc comment).
    //
    // M14.4: in-flight port messages still queued (delivered or not yet available) when the run
    // ended are not silently dropped any more (`crate::router::Router`'s own module doc comment's
    // "still pending when the run ends" note) -- `Router::pending_count` is read once, here,
    // after every span has run (both branches above: the covariance path never drives `router` at
    // all, so this is always 0 there, honestly). The count reaches `RunProducts.provenance`
    // unconditionally (`build_run_provenance`, below); a real `EVENT_KIND_LIFECYCLE` event naming
    // it is added to `all_events` only when it is non-zero (`events::dropped_messages_event`'s
    // own doc comment: "there is no 'zero dropped' event, by design").
    let dropped_in_flight_messages = router.pending_count();
    if dropped_in_flight_messages > 0 {
        all_events.push(events::dropped_messages_event(scenario.end_tai_ns, dropped_in_flight_messages, &computed_sos_hash, &scenario.data_pack_hash, &cfg.run_id));
    }

    // R4.1a/R4.1b (question 178): every PORT fault `router` genuinely applied at least once (a
    // frame actually affected -- dropped, delayed, corrupted, or duplicated) becomes exactly one
    // EVENT_KIND_FAULT event, at the epoch of its own first real effect, carrying the TOTAL count
    // of frames it affected over the whole run (`values["frames_affected"]`, question 186(c),
    // R4.1b) -- see `crate::router::Router::take_applied_port_faults`'s own doc comment and
    // `events::port_fault_event`'s own doc comment for why PORT gets its own builder instead of
    // reusing `events::fault_event`. Read once, here, alongside `pending_count` (both only make
    // sense once every span of the run has finished; the covariance path never drives `router` at
    // all, so this is always empty there, honestly, exactly like `dropped_in_flight_messages`
    // above).
    for applied in router.take_applied_port_faults() {
        let fault = scenario.faults.iter().find(|f| f.id == applied.fault_id).expect("Router only ever reports a fault id it was itself installed with, from scenario.faults");
        let instance = cfg.sos.instances.iter().find(|i| i.name == applied.instance).expect("Router only ever reports an instance its own port_kinds table knows, built from cfg.sos.instances");
        let sys_hash = system_hashes.get(&instance.system_id).expect("verified above");
        let sys = cfg.systems.get(&instance.system_id).expect("validated in pass 1");
        all_events.push(events::port_fault_event(fault, applied.applied_tai_ns, applied.frames_affected, events::event_provenance(&computed_sos_hash, &scenario.data_pack_hash, &cfg.run_id, sys_hash, &sys.id)));
    }

    all_events.sort_by_key(events::epoch_id_order);

    // Question 175 (M25.4a): every FRAMED/BYTE_STREAM frame this run's `router` carried, taken
    // once, here, right alongside `pending_count` -- both are read only after every span of the
    // run has finished (the covariance path never drives `router` at all, so this is always
    // empty there, honestly, exactly like `dropped_in_flight_messages` above). Written to the
    // `PortTrafficLog` sidecar (or not) below, once `provenance` exists to snapshot into it.
    let port_traffic_records = router.take_port_traffic();
    // Question 175 (M25.4a): emissions this router could not classify because the port carries
    // no declared `PortKind` -- `crate::router::Router::deliver`'s own doc comment for the real,
    // legitimate case (`sensors::TRUTH_PORT_NAMES`, broadcast every step by
    // `sensors::TruthBroadcastAttitude` whether or not the instance declares them). Recorded on
    // the sidecar so the skip is never silent; no zero-valued attribute, by design.
    let undeclared_port_emissions = router.undeclared_port_emissions();

    // Question 130 ("the event is referenced from the trajectory's event_ids, the same way
    // existing events are"): every event this run produced that is tied to a single instance
    // (`entity_id` non-empty -- lifecycle/fault/maneuver/port-command events all set it; only
    // `events::dropped_messages_event`'s run-level event does not, and it is accordingly
    // referenced from no trajectory) is listed on that instance's own `Trajectory.event_ids`, in
    // the same `(epoch, id)` order `all_events` itself is already sorted into.
    for (name, traj) in trajectories.iter_mut() {
        traj.event_ids = all_events.iter().filter(|e| e.entity_id == *name).map(|e| e.id.clone()).collect();
    }

    // -- Evaluate every Objective/MeasureOfEffectiveness against the real run (question 93's
    // "scores"), against the real Events and derived outputs this run actually produced
    // (question 95, M9.3 -- see the module doc comment's "Events (question 95, M9.3) and
    // outputs" section). --
    let mut run_view = crate::expr::ExprRunProducts::new(scenario.start_tai_ns, scenario.end_tai_ns, &trajectories, &all_events);
    for (name, traj) in &trajectories {
        if let Some(series) = crate::expr::speed_output(traj) {
            run_view = run_view.with_output(name, crate::expr::SPEED_OUTPUT_NAME, series);
        }
    }
    // Question 95's second half (task M10.2): attach exactly the *declared* ("output.<name>"
    // parameters, see OUTPUT_PARAMETER_PREFIX's doc comment) subset of what each instance's run
    // actually produced -- a name this instance's model computed but did not declare is simply
    // never attached (harmless: `output.<instance>.<undeclared name>@time` stays
    // ExprError::UnknownOutput, same as any other name nothing ever attached); a name declared
    // but never actually produced (e.g. declared on a native, non-GMAT instance) is likewise
    // never attached here, so it fails only at real evaluation, not at load time -- the same
    // "second pass is not redundant" property the module doc comment's "Scoring" section already
    // documents for `@time` range checks.
    for (name, decls) in &declared_outputs_by_instance {
        let Some(produced) = outputs_by_instance.get(name) else { continue };
        for (output_name, unit) in decls {
            if let Some((epochs, values)) = produced.get(output_name) {
                run_view = run_view.with_output(name, output_name, crate::expr::Series { epochs_tai_ns: epochs.clone(), values: values.clone(), unit: *unit });
            }
        }
    }
    let mut scores = BTreeMap::new();
    for obj in &cfg.drm.objectives {
        let result = crate::expr::evaluate_objective(obj, &run_view).map_err(|e| DrmError::InvalidExpression { name: obj.name.clone(), reason: e.to_string() })?;
        scores.insert(obj.name.clone(), Score { value: result.value, unit: result.unit, passed: Some(result.pass) });
    }
    for moe in &cfg.drm.measures {
        let result = crate::expr::evaluate_moe(moe, &run_view).map_err(|e| DrmError::InvalidExpression { name: moe.name.clone(), reason: e.to_string() })?;
        scores.insert(moe.name.clone(), Score { value: result.value, unit: result.unit, passed: None });
    }

    let provenance = build_run_provenance(&computed_drm_hash, &computed_sos_hash, &scenario, &cfg.run_id, cfg.error_mode, options.covariance, dropped_in_flight_messages);
    // Question 175 (M25.4a): writes the sidecar (or not) and folds "port_traffic_uri"/
    // "port_traffic" into a fresh copy of `provenance` -- see `write_port_traffic_sidecar`'s
    // own doc comment and this module's own "Port traffic sidecar" doc section.
    let (port_traffic_hash, provenance) = write_port_traffic_sidecar(cfg.products_dir.as_deref(), &cfg.run_id, port_traffic_records, undeclared_port_emissions, &provenance)?;
    // Question 121/122 (M17.2): frames every Trajectory.frame_id in this run resolves against
    // -- see collect_frames's own doc comment. Computed from the real, final `trajectories` map
    // (after every instance has run), not the load-time `declared_trajectories` shell, so a
    // frame_id this run's own re-binding/materialization actually produced is never missed.
    // Question 10/124 (M18.1): the mandatory ICRF/MJ2000Eq/BodyFixed frames for every central
    // body this run's own GMAT-bound instances declare are added on top of collect_frames's own
    // declared-plus-referenced set, always -- see add_mandatory_body_frames's own doc comment.
    let frames = collect_frames(&scenario, &trajectories);
    let frames = add_mandatory_body_frames(frames, &plans)?;
    // Question 129, M19.2 (ADR-002's fourth amendment): measure and fill `fixed_rotation_q` for
    // every body-centred inertial frame whose rotation relative to its own body's MJ2000Eq is
    // constant -- see fill_fixed_rotations's own doc comment. Runs after every frame this run
    // could possibly reference (declared, referenced-by-trajectory, and mandatory) is already
    // known, so a frame added by either earlier pass is covered too.
    let frames = fill_fixed_rotations(cfg.gmat, &gmat_ns, frames, scenario.start_tai_ns)?;
    sort_measurements(&mut all_measurements);
    Ok(RunProducts { trajectories, events: all_events, scores, provenance, dropped_in_flight_messages: dropped_in_flight_messages as u64, frames, measurements: all_measurements, port_traffic_hash })
}

/// M15.1 (`docs/open-questions.md` question 115): unit tests for [`merge_adjacent_segments`]
/// alone, with no GMAT/kernel machinery involved -- see this module's own doc comment's "Segment
/// merge across an unaffected boundary" section, and `tests/segment_merge.rs` for the same claims
/// proven end to end through the real `execute()` path.
#[cfg(test)]
mod merge_adjacent_segments_tests {
    use super::*;

    fn seg(start: i64, end: i64, hash: &str) -> TrajectorySegment {
        TrajectorySegment { name: "veh".to_string(), start_tai_ns: start, end_tai_ns: end, dynamics_model: "m".to_string(), dynamics_hash: hash.to_string(), dynamics_depth: "native".to_string() }
    }

    /// The exact bystander shape M14.4 found and this task fixes: three segments, one identical
    /// `dynamics_hash`, no maneuver on this instance at either boundary -> one merged segment
    /// spanning the whole run. Fails against a wrong implementation that never merges at all (the
    /// pre-M15.1 code): would see `merged.len() == 3`, not `1`.
    #[test]
    fn three_segments_sharing_one_hash_with_no_own_maneuver_merge_into_one() {
        let segments = vec![seg(0, 10, "h"), seg(10, 15, "h"), seg(15, 20, "h")];
        let merged = merge_adjacent_segments(segments, &[false, false, false]);
        assert_eq!(merged, vec![seg(0, 20, "h")]);
    }

    /// A DYNAMICS fault changes the hash -> the two segments it splits never merge. Fails against
    /// a wrong implementation that merges on any adjacent pair regardless of hash (e.g. one that
    /// only checks the maneuver flag): would see `merged.len() == 1`, collapsing a real
    /// reconfiguration.
    #[test]
    fn differing_hashes_never_merge_even_with_no_own_maneuver() {
        let segments = vec![seg(0, 10, "before"), seg(10, 20, "after")];
        let merged = merge_adjacent_segments(segments, &[false, false]);
        assert_eq!(merged, segments_unchanged());
        fn segments_unchanged() -> Vec<TrajectorySegment> {
            vec![seg(0, 10, "before"), seg(10, 20, "after")]
        }
    }

    /// Rule 2's own load-bearing case: a maneuver never touches `cur_plan`, so its own boundary
    /// can carry an *identical* `dynamics_hash` either side, exactly like the fault-then-maneuver
    /// fixture in `tests/segment_merge.rs`. Fails against a wrong implementation that merges on
    /// hash equality alone, ignoring the maneuver flag: would see `merged.len() == 1`, silently
    /// erasing the recorded velocity discontinuity.
    #[test]
    fn identical_hash_across_a_maneuver_boundary_still_does_not_merge() {
        let segments = vec![seg(0, 10, "h"), seg(10, 20, "h")];
        let merged = merge_adjacent_segments(segments.clone(), &[false, true]);
        assert_eq!(merged, segments);
    }

    /// A mixed run: fault (hash changes, no merge), then a maneuver back to the *same* hash the
    /// fault produced (own-maneuver flag blocks the merge even though hash now matches), matching
    /// this task's own restart-invariance fixture's target instance shape (three segments stay
    /// three). Fails against an implementation that merges the last two segments because their
    /// hashes happen to agree.
    #[test]
    fn a_fault_then_a_maneuver_back_to_the_same_hash_still_keeps_three_segments() {
        let segments = vec![seg(0, 10, "base"), seg(10, 15, "faulted"), seg(15, 20, "faulted")];
        let merged = merge_adjacent_segments(segments.clone(), &[false, false, true]);
        assert_eq!(merged, segments);
    }

    /// An empty segment list (never produced by this crate's own `run_shared_group`, but a
    /// defensive boundary case for the function's own logic) must not panic.
    #[test]
    fn an_empty_segment_list_merges_to_empty() {
        assert_eq!(merge_adjacent_segments(vec![], &[]), Vec::<TrajectorySegment>::new());
    }
}

/// `registry_default_frame`/`collect_frames` (question 121/122, M17.2): GMAT-free unit tests
/// for the "registry defaults the run actually used" half of `RunProducts.frames`, isolated
/// from any real DRM run -- `tests/drm_executor.rs`/`tests/drm_maneuver.rs` separately prove
/// this is actually wired into a real GMAT-bound run (the golden's own `EarthMJ2000Eq`).
#[cfg(test)]
mod frame_registry_tests {
    use super::*;

    /// The one shape this crate can honestly derive: a GMAT-bound instance's own
    /// `spacecraft.CoordinateSystem = "EarthMJ2000Eq"` (the golden fixture's literal value)
    /// decomposes into body "Earth" + `AxesKind::Mj2000Eq`. Fails against a wrong
    /// implementation that returns `None` for a recognizable id, swaps body/axes, or leaves
    /// `origin` unset.
    #[test]
    fn registry_default_frame_recognizes_earth_mj2000eq() {
        let def = registry_default_frame("EarthMJ2000Eq").expect("EarthMJ2000Eq is a recognized body-axes name");
        assert_eq!(def.id, "EarthMJ2000Eq");
        assert_eq!(def.origin, Some(pb::frame_definition::Origin::Body("Earth".to_string())));
        assert_eq!(def.axes, pb::AxesKind::Mj2000Eq as i32);
    }

    /// Question 136 (`docs/open-questions.md`, decided by the lead): a producer-supplied
    /// `description` must be a human description of the frame itself (what it is, its origin,
    /// its axes) -- never a process note or an internal question-number citation. Fails against
    /// the pre-M20.1 implementation (`format!("registry default for GMAT CoordinateSystem
    /// {frame_id:?} (body {body:?}, axes {axes:?})")`), which both fails the "not a process
    /// note" check (contains the literal substring "registry default") and never actually says
    /// what a J2000 equatorial frame physically is.
    #[test]
    fn registry_default_frame_description_is_a_human_description_of_the_frame_not_a_process_note() {
        let def = registry_default_frame("EarthMJ2000Eq").expect("EarthMJ2000Eq is a recognized body-axes name");
        let lower = def.description.to_ascii_lowercase();
        assert!(!lower.contains("registry default"), "must be a human description, not a process note; got {:?}", def.description);
        assert!(!lower.contains("question"), "must never cite an internal question number; got {:?}", def.description);
        assert!(!lower.contains("coordinatesystem"), "must never name the internal GMAT type this was derived from; got {:?}", def.description);
        assert!(def.description.contains("Earth"), "must name the frame's own origin body; got {:?}", def.description);
        assert!(lower.contains("j2000") && lower.contains("equator"), "must describe what a J2000 equatorial frame physically is; got {:?}", def.description);
    }

    /// Every one of the four recognized suffixes, with a distinct body each time -- proves the
    /// table is not just accidentally right for one entry. Fails against an implementation that
    /// only handles MJ2000Eq (the most common case in this crate's own fixtures) and silently
    /// mishandles the other three.
    #[test]
    fn registry_default_frame_recognizes_every_body_axes_suffix() {
        let cases = [("MarsMJ2000Ec", "Mars", pb::AxesKind::Mj2000Ec), ("LunaBodyFixed", "Luna", pb::AxesKind::BodyFixed), ("SunICRF", "Sun", pb::AxesKind::Icrf)];
        for (frame_id, body, axes) in cases {
            let def = registry_default_frame(frame_id).unwrap_or_else(|| panic!("{frame_id} should be recognized"));
            assert_eq!(def.origin, Some(pb::frame_definition::Origin::Body(body.to_string())), "{frame_id}");
            assert_eq!(def.axes, axes as i32, "{frame_id}");
        }
    }

    /// An opaque native `frame_id` (this crate's own `"test.frame"` fixture convention) never
    /// ends in a recognized suffix, so it must come back `None`, not a guessed/fabricated
    /// definition. Fails against a wrong implementation that always returns `Some` (e.g.
    /// defaulting to `AXES_KIND_UNSPECIFIED`) for any unrecognized string.
    #[test]
    fn registry_default_frame_returns_none_for_an_opaque_native_frame_id() {
        assert_eq!(registry_default_frame("test.frame"), None);
    }

    /// A bare axes suffix with no body prefix at all (`"MJ2000Eq"`) must not be accepted as a
    /// frame with an empty body -- `FrameDefinition.origin`'s own oneof requires a real,
    /// non-empty body string. Fails against an implementation that omits the `!body.is_empty()`
    /// guard and returns `Some(FrameDefinition { origin: Body(""), .. })`.
    #[test]
    fn registry_default_frame_requires_a_nonempty_body_prefix() {
        assert_eq!(registry_default_frame("MJ2000Eq"), None);
    }

    fn traj_with_frame(frame_id: &str) -> Trajectory {
        Trajectory { frame_id: frame_id.to_string(), ..Default::default() }
    }

    /// An explicitly declared `Scenario.frames` entry always wins over the derived registry
    /// default for the same id -- proven by giving the declared one a distinguishing
    /// `description` the derived default would never produce. Fails against an implementation
    /// that always overwrites (or always keeps only) whichever of the two it inserts last,
    /// rather than preferring the declared one specifically.
    #[test]
    fn collect_frames_prefers_a_declared_scenario_frame_over_the_registry_default() {
        let declared = pb::FrameDefinition { id: "EarthMJ2000Eq".to_string(), description: "author-declared, not derived".to_string(), ..Default::default() };
        let scenario = Scenario { frames: vec![declared.clone()], ..Default::default() };
        let mut trajectories = BTreeMap::new();
        trajectories.insert("veh".to_string(), traj_with_frame("EarthMJ2000Eq"));
        let frames = collect_frames(&scenario, &trajectories);
        assert_eq!(frames, vec![declared]);
    }

    /// Two trajectories sharing one `frame_id` deduplicate to a single entry; several distinct
    /// `frame_id`s come back sorted by id, not in trajectory-map iteration order (which is
    /// itself sorted by instance name, deliberately not frame id, so this is a real check).
    /// Fails against an implementation that appends one `FrameDefinition` per trajectory
    /// (duplicates) or that does not sort explicitly (a `HashMap`-backed or insertion-order
    /// implementation would produce "MarsMJ2000Eq" before "EarthMJ2000Eq" here since "mars" is
    /// inserted by a trajectory keyed before "earth" alphabetically... this test inserts them in
    /// exactly that adversarial order to make the point).
    #[test]
    fn collect_frames_deduplicates_and_sorts_by_id() {
        let scenario = Scenario::default();
        let mut trajectories = BTreeMap::new();
        trajectories.insert("a_mars_instance".to_string(), traj_with_frame("MarsMJ2000Eq"));
        trajectories.insert("b_earth_instance_1".to_string(), traj_with_frame("EarthMJ2000Eq"));
        trajectories.insert("c_earth_instance_2".to_string(), traj_with_frame("EarthMJ2000Eq"));
        let frames = collect_frames(&scenario, &trajectories);
        assert_eq!(frames.iter().map(|f| f.id.as_str()).collect::<Vec<_>>(), vec!["EarthMJ2000Eq", "MarsMJ2000Eq"]);
    }

    /// An empty `frame_id` (never populated, e.g. a declared-shape-only trajectory) is never
    /// looked up -- `registry_default_frame("")` would spuriously match every suffix (every
    /// non-empty suffix `strip_suffix`s a body of `""`... no, `strip_suffix` on `""` only
    /// matches if the whole string equals the suffix, which it does for none of the four here,
    /// but this test pins the intended behaviour explicitly rather than relying on that
    /// incidental fact) and must never produce a spurious frame.
    #[test]
    fn collect_frames_ignores_trajectories_with_no_frame_id() {
        let scenario = Scenario::default();
        let mut trajectories = BTreeMap::new();
        trajectories.insert("veh".to_string(), traj_with_frame(""));
        assert_eq!(collect_frames(&scenario, &trajectories), Vec::<pb::FrameDefinition>::new());
    }

    // -----------------------------------------------------------------------------------
    // add_mandatory_body_frames (question 10/124, M18.1).
    // -----------------------------------------------------------------------------------

    fn gmat_plan(central_body: &str) -> (BindingPlan, i64) {
        (BindingPlan::Gmat(binding::GmatSystemSpec { central_body: central_body.to_string(), ..Default::default() }), 100_000_000)
    }
    fn native_plan() -> (BindingPlan, i64) {
        (BindingPlan::ConstantAccel(binding::ConstantAccelSpec::default()), 100_000_000)
    }

    /// The mandatory ICRF/MJ2000Eq/BodyFixed frames for a run's own central body must appear
    /// even when NO trajectory in the run ever referenced them (the instance propagated in
    /// EarthMJ2000Eq only, say) -- question 124's own rationale: "a consumer may view any
    /// trajectory in any registry frame the producer can realize." Fails against the pre-M18.1
    /// behaviour (only `collect_frames`'s declared-plus-referenced set, so ICRF/BodyFixed are
    /// silently absent whenever nothing happened to propagate in them) and against an
    /// implementation that adds only one of the three suffixes.
    #[test]
    fn mandatory_frames_appear_for_the_central_body_even_when_no_trajectory_referenced_them() {
        let mut plans = BTreeMap::new();
        plans.insert("veh".to_string(), gmat_plan("Earth"));
        let frames = add_mandatory_body_frames(vec![], &plans).expect("Earth's mandatory frames are always realizable");
        let ids: Vec<&str> = frames.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(ids, vec!["EarthBodyFixed", "EarthICRF", "EarthMJ2000Eq"], "sorted by id, all three mandatory suffixes present");
        let icrf = frames.iter().find(|f| f.id == "EarthICRF").unwrap();
        assert_eq!(icrf.origin, Some(pb::frame_definition::Origin::Body("Earth".to_string())));
        assert_eq!(icrf.axes, pb::AxesKind::Icrf as i32);
    }

    /// A frame already present (declared, or already derived from a referenced trajectory)
    /// under a mandatory id is never overwritten -- mirrors `collect_frames`'s own "declared
    /// wins over derived" rule (question 122), applied here to the mandatory-frame pass too.
    /// Fails against an implementation that unconditionally re-inserts the registry-default
    /// definition for every mandatory id regardless of what is already there.
    #[test]
    fn mandatory_frames_never_overwrite_an_already_present_frame_for_the_same_id() {
        let declared = pb::FrameDefinition { id: "EarthICRF".to_string(), description: "author-declared, not derived".to_string(), ..Default::default() };
        let mut plans = BTreeMap::new();
        plans.insert("veh".to_string(), gmat_plan("Earth"));
        let frames = add_mandatory_body_frames(vec![declared.clone()], &plans).expect("realizable");
        let icrf = frames.iter().find(|f| f.id == "EarthICRF").unwrap();
        assert_eq!(icrf, &declared, "the already-present EarthICRF must be kept exactly, not replaced by the registry default");
    }

    /// A native (`ConstantAccel`) instance declares no central body, so it contributes no
    /// mandatory frames at all -- "central body" has no meaning for a system that was never
    /// propagated against a body-centred force model. Fails against an implementation that
    /// tries to derive a body from the native instance's own opaque frame_id (fabricating a
    /// bogus mandatory frame) or panics on the `ConstantAccel` arm.
    #[test]
    fn a_native_instance_with_no_central_body_contributes_no_mandatory_frames() {
        let mut plans = BTreeMap::new();
        plans.insert("veh".to_string(), native_plan());
        let frames = add_mandatory_body_frames(vec![], &plans).expect("no central body to realize");
        assert_eq!(frames, Vec::<pb::FrameDefinition>::new());
    }

    /// Two GMAT-bound instances with distinct central bodies (Earth, Mars) each get their own
    /// three mandatory frames, sorted by id -- not a `HashMap`-order-dependent list (ADR-004).
    /// Fails against an implementation that only looks at one instance's own plan (e.g. always
    /// `plans.values().next()`) or that returns them in map-iteration order rather than sorted.
    #[test]
    fn mandatory_frames_are_added_per_distinct_central_body_and_sorted() {
        let mut plans = BTreeMap::new();
        plans.insert("mars_veh".to_string(), gmat_plan("Mars"));
        plans.insert("earth_veh".to_string(), gmat_plan("Earth"));
        let frames = add_mandatory_body_frames(vec![], &plans).expect("both bodies are realizable");
        let ids: Vec<&str> = frames.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(ids, vec!["EarthBodyFixed", "EarthICRF", "EarthMJ2000Eq", "MarsBodyFixed", "MarsICRF", "MarsMJ2000Eq"]);
    }
}

/// `RunProducts::to_proto` (question 121, M17.2): GMAT-free unit tests for the executor's
/// `RunProducts` -> `altavista.v1.RunProducts` conversion, isolated from any real DRM run (this
/// crate's own `RunProducts` fields are all `pub`, so a fixture is built directly rather than
/// through a full `execute()` call -- `tests/drm_executor.rs`/`tests/drm_maneuver.rs` separately
/// prove `to_proto` is reachable and correct against a real, GMAT-propagated run).
#[cfg(test)]
mod to_proto_tests {
    use super::*;
    use prost::Message;

    fn sample() -> RunProducts {
        let mut trajectories = BTreeMap::new();
        trajectories.insert("veh".to_string(), Trajectory { id: "veh-traj".to_string(), entity_id: "veh".to_string(), frame_id: "EarthMJ2000Eq".to_string(), config_hash: "traj-hash".to_string(), ..Default::default() });
        let mut scores = BTreeMap::new();
        scores.insert("obj_pass".to_string(), Score { value: 1.0, unit: Unit::Meter, passed: Some(true) });
        scores.insert("obj_fail".to_string(), Score { value: 2.0, unit: Unit::MeterPerSecond, passed: Some(false) });
        scores.insert("moe".to_string(), Score { value: 3.0, unit: Unit::Second, passed: None });
        RunProducts {
            trajectories,
            events: vec![Event { id: "ev1".to_string(), name: "run_start".to_string(), tai_ns: 0, ..Default::default() }],
            scores,
            provenance: Provenance { config_hash: "run-hash".to_string(), run_id: "test-run-id".to_string(), ..Default::default() },
            dropped_in_flight_messages: 7,
            frames: vec![pb::FrameDefinition { id: "EarthMJ2000Eq".to_string(), origin: Some(pb::frame_definition::Origin::Body("Earth".to_string())), axes: pb::AxesKind::Mj2000Eq as i32, ..Default::default() }],
            // Question 173: already in `sort_measurements`'s own required order (epoch_ns then
            // measurement_id) -- `sample()` is hand-built, never run through `execute()`'s own
            // sort call, so this list is pre-sorted deliberately, not by accident.
            measurements: vec![
                pb::Measurement { measurement_id: "m1".to_string(), z: vec![1.0, 2.0], r: vec![0.1, 0.0, 0.0, 0.1], epoch_ns: 100, sensor_id: "veh".to_string(), ..Default::default() },
                pb::Measurement { measurement_id: "m2".to_string(), z: vec![3.0], epoch_ns: 200, sensor_id: "veh".to_string(), ..Default::default() },
            ],
            port_traffic_hash: "sample-port-traffic-hash".to_string(),
        }
    }

    /// **The load-bearing assertion this task's honesty requirements call out by name:** an
    /// Objective's `Some(false)` (failed, not absent) and a MeasureOfEffectiveness's `None` (no
    /// pass/fail concept) must stay distinguishable all the way through a real byte-level
    /// `encode_to_vec`/`decode` round trip, not just in the in-memory `pb::RunProducts` value
    /// `to_proto()` returns directly. Fails against a wrong implementation that flattens
    /// `optional bool` to a plain `bool` (would not compile against `Option<bool>` at all -- a
    /// compile-time catch) or, more insidiously, one whose `to_proto` does
    /// `Some(score.passed.unwrap_or(false))`: that still type-checks and even makes the
    /// in-memory assertion below pass by coincidence for `obj_fail`, but `moe`'s `None` would
    /// come back as `Some(false)` after a real round trip -- exactly what this test's decode
    /// step catches and an in-memory-only check would not.
    #[test]
    fn passed_none_and_some_false_stay_distinguishable_through_a_real_byte_round_trip() {
        let products = sample();
        let proto = products.to_proto();
        assert_eq!(proto.scores["obj_pass"].passed, Some(true));
        assert_eq!(proto.scores["obj_fail"].passed, Some(false));
        assert_eq!(proto.scores["moe"].passed, None);

        let bytes = proto.encode_to_vec();
        let decoded = pb::RunProducts::decode(bytes.as_slice()).expect("valid altavista.v1.RunProducts bytes");
        assert_eq!(decoded.scores["obj_pass"].passed, Some(true));
        assert_eq!(decoded.scores["obj_fail"].passed, Some(false), "an Objective that failed must decode as Some(false), not None");
        assert_eq!(decoded.scores["moe"].passed, None, "a MeasureOfEffectiveness has no pass/fail concept and must decode as None, not Some(false)");
    }

    /// Every score's map key and its own `ScoreResult.name` must agree (`execute()`'s own
    /// invariant -- see `to_proto`'s doc comment). Fails against an implementation that leaves
    /// `name` empty, copies a different entry's name, or only sets it for one arbitrary entry.
    #[test]
    fn every_score_result_name_matches_its_own_map_key() {
        let proto = sample().to_proto();
        for (key, result) in &proto.scores {
            assert_eq!(&result.name, key, "ScoreResult.name must equal the map key it is stored under");
        }
    }

    /// The dropped count and frames survive a real byte round trip, not just field assignment
    /// in memory. Fails against an implementation that leaves `pb::RunProducts.
    /// dropped_in_flight_messages`/`frames` at their proto defaults (0 / empty) regardless of
    /// what `RunProducts` actually carries, or one that only sets them in the in-memory value
    /// without them surviving `encode_to_vec`/`decode` (e.g. a field number collision with
    /// another field).
    #[test]
    fn dropped_count_and_frames_survive_a_real_byte_round_trip() {
        let products = sample();
        let bytes = products.to_proto().encode_to_vec();
        let decoded = pb::RunProducts::decode(bytes.as_slice()).expect("valid altavista.v1.RunProducts bytes");
        assert_eq!(decoded.dropped_in_flight_messages, 7);
        assert_eq!(decoded.frames, products.frames);
    }

    /// Question 173: `RunProducts.measurements` -- its own CDM type, on the wire as `field 8`,
    /// never smuggled into an `Event`'s attributes (`docs/open-questions.md` question 173's own
    /// wording) -- survives a real byte round trip, `z`/`r` included. Fails against an
    /// implementation that leaves `pb::RunProducts.measurements` empty regardless of what
    /// `RunProducts` carries (the pre-M25.3 stub this test would have caught), or one that drops
    /// `r` (a plausible bug: `z` is required for every measurement in `sample()`, `r` is only
    /// non-empty for one of the two).
    #[test]
    fn measurements_survive_a_real_byte_round_trip() {
        let products = sample();
        let bytes = products.to_proto().encode_to_vec();
        let decoded = pb::RunProducts::decode(bytes.as_slice()).expect("valid altavista.v1.RunProducts bytes");
        assert_eq!(decoded.measurements, products.measurements);
        assert_eq!(decoded.measurements[0].r, vec![0.1, 0.0, 0.0, 0.1]);
        assert!(decoded.measurements[1].r.is_empty());
    }

    /// [`sort_measurements`]'s own required order (question 173: "sorted by epoch and id").
    /// Fails against an implementation that sorts by `measurement_id` alone (would put "a"
    /// before "b" regardless of epoch) or leaves input order untouched.
    #[test]
    fn sort_measurements_orders_by_epoch_then_id() {
        let mut m = vec![
            pb::Measurement { measurement_id: "b".to_string(), epoch_ns: 100, ..Default::default() },
            pb::Measurement { measurement_id: "a".to_string(), epoch_ns: 200, ..Default::default() },
            pb::Measurement { measurement_id: "a".to_string(), epoch_ns: 100, ..Default::default() },
        ];
        sort_measurements(&mut m);
        let order: Vec<(i64, &str)> = m.iter().map(|x| (x.epoch_ns, x.measurement_id.as_str())).collect();
        assert_eq!(order, vec![(100, "a"), (100, "b"), (200, "a")]);
    }

    /// `run_id` comes from `provenance.run_id`, not a constant or a different field -- two
    /// otherwise-identical `RunProducts` differing only in `provenance.run_id` must produce
    /// different `pb::RunProducts.run_id` values. Fails against an implementation that
    /// hardcodes `run_id` (e.g. `String::new()`) or reads the wrong source field.
    #[test]
    fn run_id_comes_from_provenance_run_id_not_a_constant() {
        let mut a = sample();
        a.provenance.run_id = "run-a".to_string();
        let mut b = sample();
        b.provenance.run_id = "run-b".to_string();
        assert_eq!(a.to_proto().run_id, "run-a");
        assert_eq!(b.to_proto().run_id, "run-b");
        assert_ne!(a.to_proto().run_id, b.to_proto().run_id);
    }

    /// `trajectories`/`events`/`provenance` pass through unchanged (not dropped, not mutated).
    /// Fails against an implementation that forgets a field (e.g. always emits `Provenance::
    /// default()` instead of the real one) or silently drops the one trajectory/event this
    /// fixture carries.
    #[test]
    fn trajectories_events_and_provenance_pass_through_unchanged() {
        let products = sample();
        let proto = products.to_proto();
        assert_eq!(proto.trajectories, products.trajectories);
        assert_eq!(proto.events, products.events);
        assert_eq!(proto.provenance, Some(products.provenance));
    }

    /// Question 175 (M25.4a): `RunProducts.port_traffic_hash` survives `to_proto` and a real
    /// byte round trip unchanged -- fails against an implementation that still hardcodes
    /// `String::new()` (the pre-M25.4a stub this test would have caught) or copies some other
    /// field into this one by mistake.
    #[test]
    fn port_traffic_hash_survives_to_proto_and_a_real_byte_round_trip() {
        let products = sample();
        assert_eq!(products.to_proto().port_traffic_hash, "sample-port-traffic-hash");
        let bytes = products.to_proto().encode_to_vec();
        let decoded = pb::RunProducts::decode(bytes.as_slice()).expect("valid altavista.v1.RunProducts bytes");
        assert_eq!(decoded.port_traffic_hash, "sample-port-traffic-hash");
    }
}

/// Question 129 (M19.2): `validate_fixed_rotation_q`/`matrix_to_quaternion`, no GMAT/`gmat_sys::
/// engine_lock()` needed -- pure arithmetic, unlike `fixed_rotation_measurement_interval_tests`
/// below.
#[cfg(test)]
mod fixed_rotation_math_tests {
    use super::*;

    /// core.proto field 13's own contract: "Empty" is a valid, meaningful value (no fixed
    /// rotation declared -- e.g. the reference frame itself). Fails against an implementation
    /// that treats an empty slice as malformed.
    #[test]
    fn empty_fixed_rotation_q_is_valid() {
        assert!(validate_fixed_rotation_q("f", &[]).is_ok());
    }

    /// A genuine unit quaternion (90 degrees about Z: w=x=y=0's... concretely
    /// `[cos(45deg), 0, 0, sin(45deg)]`) must validate. Fails against an implementation that
    /// rejects every non-empty value (e.g. a stray `n == 0` check written backwards).
    #[test]
    fn a_genuine_unit_quaternion_is_valid() {
        let h = std::f64::consts::FRAC_PI_4;
        assert!(validate_fixed_rotation_q("f", &[h.cos(), 0.0, 0.0, h.sin()]).is_ok());
    }

    /// core.proto field 13: "Exactly 0 or 4 entries" -- any other length is a typed refusal, not
    /// a silent truncation/pad. Fails against an implementation that only checks `len() > 4`
    /// (would accept 1, 2 or 3) or that pads/truncates to 4 instead of erroring.
    #[test]
    fn wrong_length_is_a_typed_error_never_truncated_or_padded() {
        for bad in [vec![1.0], vec![1.0, 0.0], vec![1.0, 0.0, 0.0], vec![1.0, 0.0, 0.0, 0.0, 0.0]] {
            match validate_fixed_rotation_q("badlen", &bad) {
                Err(DrmError::InvalidFixedRotationQuaternion { frame_id, .. }) => assert_eq!(frame_id, "badlen"),
                other => panic!("expected InvalidFixedRotationQuaternion for length {}, got {other:?}", bad.len()),
            }
        }
    }

    /// A length-4 value whose norm is far from 1 (here, 2.0 -- `[2,0,0,0]`) is a typed refusal,
    /// never silently renormalized to `[1,0,0,0]`. Fails against an implementation that
    /// normalizes instead of erroring -- exactly the "no silent... normalized away" rule this
    /// task's own brief states for this field.
    #[test]
    fn non_unit_norm_is_a_typed_error_never_silently_renormalized() {
        match validate_fixed_rotation_q("badnorm", &[2.0, 0.0, 0.0, 0.0]) {
            Err(DrmError::InvalidFixedRotationQuaternion { frame_id, reason }) => {
                assert_eq!(frame_id, "badnorm");
                assert!(reason.contains("norm"), "reason should mention the norm violation: {reason}");
            }
            other => panic!("expected InvalidFixedRotationQuaternion, got {other:?}"),
        }
    }

    /// A norm just barely outside [`FIXED_ROTATION_UNIT_NORM_TOLERANCE`] is still refused --
    /// proves the check is a real bound, not accidentally vacuous (e.g. comparing against the
    /// wrong constant, or a `<` where a `<=` was intended, that would let everything through).
    #[test]
    fn a_norm_just_outside_the_tolerance_is_refused() {
        let norm = 1.0 + FIXED_ROTATION_UNIT_NORM_TOLERANCE * 10.0;
        assert!(validate_fixed_rotation_q("f", &[norm, 0.0, 0.0, 0.0]).is_err());
    }

    /// [`matrix_to_quaternion`] on a known rotation -- 90 degrees about the Z axis
    /// (`[[0,-1,0],[1,0,0],[0,0,1]]`) -- must reproduce the textbook quaternion
    /// `[cos(45deg), 0, 0, sin(45deg)]` (scalar-first). Fails against a row/column-swapped
    /// extraction (would give the *inverse*, i.e. -90 degrees, `z` flipped in sign) or a wrong
    /// branch in the trace-based Shepperd method (this matrix's trace is 1.0, exercising the
    /// `trace > 0.0` branch).
    #[test]
    fn ninety_degrees_about_z_matches_the_textbook_quaternion() {
        let m = [[0.0, -1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]];
        let q = matrix_to_quaternion(&m);
        let h = std::f64::consts::FRAC_PI_4;
        let expected = [h.cos(), 0.0, 0.0, h.sin()];
        for i in 0..4 {
            assert!((q[i] - expected[i]).abs() < 1e-12, "component {i}: got {}, expected {}", q[i], expected[i]);
        }
    }

    /// [`matrix_to_quaternion`] must also handle a matrix with negative trace (exercises the
    /// `m[0][0] > m[1][1] && m[0][0] > m[2][2]` branch): 180 degrees about X,
    /// `[[1,0,0],[0,-1,0],[0,0,-1]]`, trace = -1.0. Expected quaternion `[0, 1, 0, 0]` up to the
    /// universal `q`/`-q` sign ambiguity (both represent the identical rotation) -- fails against
    /// a branch selection bug that would divide by a near-zero `sqrt` (NaN) or pick the wrong
    /// off-diagonal pairing.
    #[test]
    fn one_hundred_eighty_degrees_about_x_matches_the_textbook_quaternion_up_to_sign() {
        let m = [[1.0, 0.0, 0.0], [0.0, -1.0, 0.0], [0.0, 0.0, -1.0]];
        let q = matrix_to_quaternion(&m);
        let norm = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
        assert!((norm - 1.0).abs() < 1e-12, "must be a unit quaternion, got norm {norm}");
        let expected = [0.0_f64, 1.0, 0.0, 0.0];
        let same_sign_err: f64 = q.iter().zip(expected.iter()).map(|(a, b)| (a - b).powi(2)).sum();
        let flipped_sign_err: f64 = q.iter().zip(expected.iter()).map(|(a, b)| (a + b).powi(2)).sum();
        assert!(same_sign_err.min(flipped_sign_err) < 1e-20, "expected [0,1,0,0] up to sign, got {q:?}");
    }
}

/// Question 129 (M19.2): direct, GMAT-backed measurements of
/// [`FIXED_ROTATION_MEASURE_INTERVAL_NS`]'s own doc comment -- proves the discovery that doc
/// comment reports (a day-long separation would have failed `EarthICRF`'s own constancy check)
/// rather than merely asserting it in prose, and pins the chosen 10 s separation as genuinely
/// safe with margin. No `#[ignore]`: cheap (a handful of `Gmat::convert` calls, ~5.6 us each per
/// ADR-002's fourth amendment) and this is exactly the kind of GMAT-version-sensitive fact that
/// should re-fail loudly, not silently bit-rot, if a future GMAT/data-file update ever changes
/// `ICRF_Table.txt`'s own tabulated values.
#[cfg(test)]
mod fixed_rotation_measurement_interval_tests {
    use super::*;

    fn earth_icrf_vs_mj2000eq_max_delta(gmat: &Gmat, base_a1mjd: f64, gap_days: f64) -> f64 {
        let c0 = rotation_matrix(gmat, base_a1mjd, "IntervalDbgParent", "IntervalDbgThis").expect("rotation_matrix at base epoch");
        let c1 = rotation_matrix(gmat, base_a1mjd + gap_days, "IntervalDbgParent", "IntervalDbgThis").expect("rotation_matrix at base+gap epoch");
        c0.iter().flatten().zip(c1.iter().flatten()).map(|(a, b)| (a - b).abs()).fold(0.0_f64, f64::max)
    }

    /// The necessity proof: [`FIXED_ROTATION_MEASURE_INTERVAL_NS`]'s own doc comment reports that
    /// a full day's separation measures 1.32e-9 for the genuinely-fixed `EarthICRF`/`EarthMJ2000Eq`
    /// pair -- three orders of magnitude past [`FIXED_ROTATION_CONSTANCY_TOLERANCE`]. Fails
    /// against a reviewer's doubt that this was ever really measured (or was noise that has since
    /// gone away): if GMAT's own `ICRF_Table.txt`-driven residual ever shrank under 1e-12 at one
    /// day, *this* assertion (not the production code) is what would need revisiting -- the
    /// production code already uses the shorter, safe interval regardless.
    #[test]
    fn a_day_long_separation_would_have_failed_the_constancy_check_for_earthicrf() {
        let _engine = gmat_sys::engine_lock();
        let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
        gmat.coordinate_system("IntervalDbgParent", "Earth", "MJ2000Eq").expect("construct MJ2000Eq");
        gmat.coordinate_system("IntervalDbgThis", "Earth", "ICRF").expect("construct ICRF");
        gmat.initialize().expect("initialize");
        let base_a1mjd = Tai::from_nanos(1_767_225_637_000_000_000_i64).to_a1_mjd();

        let one_day_delta = earth_icrf_vs_mj2000eq_max_delta(&gmat, base_a1mjd, 1.0);
        eprintln!("[fixed_rotation] EarthICRF vs EarthMJ2000Eq, 1-day separation: max_delta = {one_day_delta:e} (tolerance {FIXED_ROTATION_CONSTANCY_TOLERANCE:e})");
        assert!(
            one_day_delta > FIXED_ROTATION_CONSTANCY_TOLERANCE,
            "expected a day-long separation to exceed the tolerance (GMAT's own ICRF_Table.txt-driven residual, per FIXED_ROTATION_MEASURE_INTERVAL_NS's own doc comment); measured {one_day_delta:e}"
        );

        let chosen_gap_days = FIXED_ROTATION_MEASURE_INTERVAL_NS as f64 / 86_400.0e9;
        let chosen_delta = earth_icrf_vs_mj2000eq_max_delta(&gmat, base_a1mjd, chosen_gap_days);
        eprintln!(
            "[fixed_rotation] EarthICRF vs EarthMJ2000Eq, {}s separation (the interval this crate actually uses): max_delta = {chosen_delta:e} (tolerance {FIXED_ROTATION_CONSTANCY_TOLERANCE:e})",
            FIXED_ROTATION_MEASURE_INTERVAL_NS / 1_000_000_000
        );
        assert!(
            chosen_delta <= FIXED_ROTATION_CONSTANCY_TOLERANCE,
            "the interval this crate actually uses must stay under the tolerance with margin; measured {chosen_delta:e}"
        );
    }

    /// Sanity: two independent `Gmat::convert`-driven [`rotation_matrix`] calls at the IDENTICAL
    /// epoch must be bit-for-bit identical -- rules out cross-call state contamination in this
    /// crate's own extraction (as opposed to a genuine, epoch-dependent GMAT computation) as an
    /// alternative explanation for [`FIXED_ROTATION_MEASURE_INTERVAL_NS`]'s own measured residual.
    #[test]
    fn repeating_the_identical_epoch_gives_a_bit_identical_rotation_matrix() {
        let _engine = gmat_sys::engine_lock();
        let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
        gmat.coordinate_system("RepeatDbgParent", "Earth", "MJ2000Eq").expect("construct MJ2000Eq");
        gmat.coordinate_system("RepeatDbgThis", "Earth", "ICRF").expect("construct ICRF");
        gmat.initialize().expect("initialize");
        let a1mjd = Tai::from_nanos(1_767_225_637_000_000_000_i64).to_a1_mjd();
        let c0 = rotation_matrix(&gmat, a1mjd, "RepeatDbgParent", "RepeatDbgThis").unwrap();
        let c1 = rotation_matrix(&gmat, a1mjd, "RepeatDbgParent", "RepeatDbgThis").unwrap();
        assert_eq!(c0, c1, "identical epoch, two independent calls, must be bit-identical");
    }
}
