//! Every way an `av-sweep` operation can be refused (F1a, `docs/feasibility-plan.md`),
//! mirroring `av_kernel::drm::DrmError`'s *shape*: one variant per named refusal, named
//! fields, every message naming the offending artifact and value, no silent fallback.
//!
//! **Style note, disclosed per this task's own rules.** The brief that commissioned this
//! crate asked for "a thiserror enum in the style of `av_kernel::drm::DrmError`". Read
//! literally, that is not quite what `DrmError` (`crates/av-kernel/src/drm/mod.rs`) actually
//! is: it is a hand-rolled `#[derive(Debug)]` enum with a manual `impl std::fmt::Display` and
//! a bare `impl std::error::Error for DrmError {}` -- not a `#[derive(thiserror::Error)]`, and
//! `av-kernel`'s own `Cargo.toml` does not depend on `thiserror` at all. This module keeps
//! `DrmError`'s shape (one variant per refusal, named fields, a `write!`-quality message naming
//! the offending artifact/value) but actually derives `thiserror::Error` rather than
//! hand-writing a second `Display` impl of the same kind -- `thiserror` is on this crate's
//! short allowed-dependency list and the task brief names it explicitly for `error.rs`.
//!
//! **The one variant that wraps `DrmError`** ([`SweepError::Drm`]) is genuinely exercised, not
//! merely declared for the brief's sake: [`crate::sample::sample_config`] verifies the caller's
//! supplied base `DesignReferenceMission`/`SosConfiguration`/each `SystemDefinition` against
//! their own canonical hashes (`av_kernel::drm::hash::verify_*_hash`) before ever copying or
//! mutating them -- the same "never build on top of a tampered or inconsistent artifact" rule
//! this whole platform applies everywhere else. This check is not spelled out verbatim in this
//! task's brief (which only says `sample_config` "produc[es] a per-sample copy... plus the
//! config hash"); it is disclosed here, and in `REPORT.md`, as a deliberate addition rather
//! than a silent one.

use av_kernel::drm::DrmError;

#[derive(Debug, thiserror::Error)]
pub enum SweepError {
    /// The sweep YAML did not parse as the expected shape at all (`serde_yaml`'s own message).
    #[error("sweep YAML did not parse: {0}")]
    Yaml(String),

    /// A string field that names a proto enum value (today, only `Provenance.author_kind`)
    /// did not match any of that enum's `from_str_name` variants.
    #[error("{field}: {value:?} is not a recognized enum value")]
    InvalidEnumValue { field: &'static str, value: String },

    /// A `ParameterSweep.hash` did not match its own canonical hash
    /// ([`crate::hash::canonical_sweep_hash`]) -- refused, never a warning.
    #[error("ParameterSweep {id:?}: declared hash {declared:?} does not match its canonical hash {computed:?}; refusing to trust a tampered sweep")]
    HashMismatch { id: String, declared: String, computed: String },

    /// A [`SweepAxis`](av_cdm::pb::SweepAxis) declared explicit `values` *and* a non-zero
    /// `min`/`max`/`steps` range at the same time -- an ambiguous declaration is never silently
    /// resolved by preferring one over the other.
    #[error("axis on {instance}.{parameter}: both explicit `values` and a min/max/steps range were declared; an ambiguous axis is never silently resolved")]
    AmbiguousAxisDeclaration { instance: String, parameter: String },

    /// A range-declared axis (`values` empty) had `steps < 2` -- too few points to expand a
    /// range at all (this is also what an axis declaring neither `values` nor a usable range
    /// looks like: `steps` defaults to `0`, which is `< 2`).
    #[error("axis on {instance}.{parameter}: steps={steps} is below the minimum of 2 needed to expand a min..max range (values was empty)")]
    AxisStepsBelowMinimum { instance: String, parameter: String, steps: u32 },

    /// A range-declared axis had `max <= min`.
    #[error("axis on {instance}.{parameter}: max={max} is not greater than min={min}")]
    AxisRangeNotIncreasing { instance: String, parameter: String, min: f64, max: f64 },

    /// Two axes in the same `ParameterSweep.axes` named the same target -- the same
    /// `instance`+`parameter` pair (question 192(c) widens this to also catch two event axes on
    /// the same `event_id`+`value_key`: `key` is each axis' own `SweepSample.axis_values` key,
    /// see [`crate::grid::AxisValue::key`], which already names one target uniquely regardless of
    /// which kind it is).
    #[error("two axes both target {key:?}; a sweep may declare each target at most once")]
    DuplicateAxis { key: String },

    /// [`crate::seed::derive_seed`] was given a `sweep_hash` that is not exactly 64 lowercase
    /// hex characters -- the byte layout it hashes fixes that field at 64 bytes, so an
    /// off-length or mixed-case value would silently corrupt every derived seed downstream.
    #[error("sweep_hash {sweep_hash:?} is not 64 lowercase hex characters")]
    InvalidSweepHash { sweep_hash: String },

    /// A `SweepAxis.instance` (or a sample's grid point) named no `SystemInstance.name` in the
    /// `SosConfiguration`.
    #[error("axis names instance {instance:?}, which is not in this SosConfiguration")]
    UnknownInstance { instance: String },

    /// F1a review defect #1 (`REPORT.md`/this crate's own task brief): an axis targets an
    /// instance whose `SystemInstance.system_id` names no `SystemDefinition` in the `systems`
    /// map supplied to [`crate::sample::sample_config`]. Previously this fell through to
    /// [`SweepError::UndeclaredParameter`], which blames the *parameter* for what is actually a
    /// missing *`SystemDefinition`* -- a failure's message must name the real cause (this
    /// crate's own standing rule), so it is now its own variant.
    #[error("instance {instance:?} names system_id {system_id:?}, which is not in the supplied systems map")]
    UnknownSystem { instance: String, system_id: String },

    /// A `SweepAxis.parameter` named a parameter declared neither in its instance's own
    /// `parameter_overrides` nor in the bound `SystemDefinition.parameters` -- a typo must not
    /// silently become a meaningless override.
    #[error("axis names {instance}.{parameter}, which is declared neither in that instance's own parameter_overrides nor in its bound SystemDefinition.parameters")]
    UndeclaredParameter { instance: String, parameter: String },

    /// F1a review defect #2: the existing declaration (an instance's own override, or the base
    /// `SystemDefinition.parameters` entry) an axis targets carries a real declared bound
    /// (`max > min` -- `Parameter`'s own field comment: `min`/`max` both `0.0` is proto3's zero
    /// default and means "unbounded", so the check below is conditional on a genuine bound
    /// having been declared, never on the mere presence of the fields) and the axis's value
    /// falls outside `[min, max]`. A declared bound is enforced, not silently exceeded.
    #[error("axis names {instance}.{parameter} = {value}, which is outside its declared bound [{min}, {max}]")]
    ParameterOutOfBounds { instance: String, parameter: String, value: f64, min: f64, max: f64 },

    /// A `SweepAxis` named a parameter whose existing declaration (override or base) carries a
    /// non-empty `string_value` -- a `double`-valued axis cannot sweep a string parameter.
    #[error("axis names {instance}.{parameter}, whose existing declaration carries a non-empty string_value; a double-valued axis cannot sweep a string parameter")]
    StringValuedParameter { instance: String, parameter: String },

    /// `ParameterSweep.monte_carlo_draws` was `0` -- never defaulted to `1`.
    #[error("ParameterSweep.monte_carlo_draws is 0; a study must declare at least one draw")]
    ZeroDraws,

    /// `ParameterSweep.monte_carlo_draws > 1` but the DRM's `Scenario.seeds` is empty -- every
    /// draw would derive identical seeds and be an identical sample, so a study that cannot vary
    /// is a declaration error, not something to run silently.
    #[error("ParameterSweep.monte_carlo_draws={draws} but Scenario.seeds is empty; every draw would be identical")]
    DrawsAboveOneWithoutSeeds { draws: u32 },

    /// Question 192(d): `ParameterSweep.dispersed` is `true` but the DRM's `Scenario.seeds` is
    /// empty -- the same reason [`SweepError::DrawsAboveOneWithoutSeeds`] exists: a dispersed
    /// draw with no declared seed has nothing to sample from.
    #[error("ParameterSweep.dispersed is true but Scenario.seeds is empty; a dispersed draw has nothing to sample from")]
    DispersedWithoutSeeds,

    /// Question 192(c): a [`SweepAxis`](av_cdm::pb::SweepAxis) declared neither a full instance
    /// parameter target (`instance` + `parameter`) nor a full event-value target (`event_id` +
    /// `value_key`) -- including a partial declaration of either (e.g. `instance` set but
    /// `parameter` empty). Exactly one target is required; this names what was actually given so
    /// a partial declaration is diagnosable, not just "invalid".
    #[error("axis declares neither a full instance target (instance={instance:?}, parameter={parameter:?}) nor a full event target (event_id={event_id:?}, value_key={value_key:?}); exactly one target is required")]
    AxisMissingTarget { instance: String, parameter: String, event_id: String, value_key: String },

    /// Question 192(c): a [`SweepAxis`](av_cdm::pb::SweepAxis) declared BOTH a full instance
    /// parameter target and a full event-value target at once -- ambiguous, never silently
    /// resolved by preferring one over the other (the same posture as
    /// [`SweepError::AmbiguousAxisDeclaration`] for the explicit-values-vs-range ambiguity).
    #[error("axis declares both a full instance target (instance={instance:?}, parameter={parameter:?}) and a full event target (event_id={event_id:?}, value_key={value_key:?}); exactly one target is required")]
    AxisBothTargets { instance: String, parameter: String, event_id: String, value_key: String },

    /// Question 192(c): a parameter axis' own `SweepSample.axis_values` key
    /// (`"{instance}.{parameter}"`) would start with the reserved event-axis-key prefix
    /// (`"event:"`, see [`crate::grid::EVENT_AXIS_KEY_PREFIX`]) -- refused so the two key spaces
    /// can never collide, rather than relying on no real instance ever happening to be named
    /// that way. See `crates/av-sweep/REPORT.md` for the full collision analysis.
    #[error("axis names instance {instance:?}, parameter {parameter:?}: the key {instance:?}.{parameter:?} would start with the reserved event-axis-key prefix {prefix:?}; rename the instance")]
    ReservedAxisKeyPrefix { instance: String, parameter: String, prefix: &'static str },

    /// Question 192(c): an event axis named `event_id`, which names no `ScenarioEvent.id` in the
    /// per-sample DRM's `Scenario.events` (or the DRM has no `Scenario` at all).
    #[error("axis names event {event_id:?}, which is not in this DRM's Scenario.events")]
    UnknownEvent { event_id: String },

    /// Question 192(c): an event axis named `value_key`, which is not already a key in that
    /// event's own `ScenarioEvent.values` map -- a typo must not silently create a new key (the
    /// same posture as [`SweepError::UndeclaredParameter`] for instance axes).
    #[error("axis names event {event_id:?}, value_key {value_key:?}, which is not already declared in that event's own values map")]
    UndeclaredEventValueKey { event_id: String, value_key: String },

    /// `ParameterSweep.drm_id` did not match the supplied `DesignReferenceMission.id`.
    #[error("ParameterSweep.drm_id {sweep_drm_id:?} does not match DesignReferenceMission.id {drm_id:?}")]
    SweepDrmIdMismatch { sweep_drm_id: String, drm_id: String },

    /// A caller asked [`crate::sample::sample_config`] for a `point_index` past the end of the
    /// sweep's own expanded grid.
    #[error("point_index {point_index} is out of range for a grid of {grid_len} point(s)")]
    PointIndexOutOfRange { point_index: u32, grid_len: usize },

    /// A base `DesignReferenceMission`/`SosConfiguration`/`SystemDefinition` supplied to
    /// [`crate::sample::sample_config`] failed its own canonical-hash check -- see this
    /// module's doc comment's "the one variant that wraps `DrmError`" section.
    #[error(transparent)]
    Drm(#[from] DrmError),

    /// F2 (`crate::aggregate::aggregate`): the same score `name` at the same `point_index`
    /// carried `ScoreResult.passed` set (an Objective) on at least one contributing draw and
    /// unset (a MeasureOfEffectiveness) on at least one other -- one score cannot be both within
    /// a single study; that would mean the DRM's own scoring declaration changed mid-study.
    #[error("point {point_index}, score {name:?}: passed is set on some draws and unset on others; a score cannot be an Objective in one draw and a MeasureOfEffectiveness in another within one study")]
    MixedPassCriterion { name: String, point_index: u32 },
}
