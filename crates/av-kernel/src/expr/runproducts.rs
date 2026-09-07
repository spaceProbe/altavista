//! What an ADR-005 sec 6 expression evaluates *against*: "References resolve against run
//! products" (trajectories, events, and outputs). [`ExprRunProducts`] is this evaluator's own
//! runtime environment for the grammar's `ref`/`call` productions -- not a proto message
//! (ADR-005 does not define one for "everything one DRM run produced"; building the evaluator
//! at all needs *some* typed handle onto "the run", so this is that handle, built from what
//! `crate::drm::executor::execute` already returns).
//!
//! **Entity ids are instance names.** `crate::drm::executor::execute` returns
//! `BTreeMap<String, Trajectory>` keyed by `SosConfiguration.instances[].name` (see that
//! module's own doc comment: `kernel.register_system(instance_name.to_string(), ...)`), and
//! that map's key is also what ends up in `Trajectory.entity_id` for every trajectory this
//! crate produces. `entity.<id>.<component>` therefore resolves `<id>` against that same map
//! directly -- [`ExprRunProducts::new`] takes exactly `crate::drm::executor::RunProducts.
//! trajectories` (question 93's `execute()` return value), no extra plumbing.
//!
//! **`range(<a>, <b>)`.** A second reference-like form the amendment's worked example needs
//! (`docs/adr/005-simulation-kernel.md`'s amendment 2026-09-02, `duration(range(a, b) < 100
//! m)`): the Euclidean distance between two entities' own `pos_x`/`pos_y`/`pos_z` components,
//! in metres -- see [`ExprRunProducts::range_at`]/[`ExprRunProducts::range_series`] and
//! `crate::expr::eval`'s module doc comment for exactly why this shape (not a generic
//! dimensional-analysis "distance between two vectors") is what is implemented.
//!
//! **Outputs (`docs/open-questions.md` question 95, M9.3).** `output.<instance>.<name>@time` is
//! one of ADR-005 sec 6's own reference forms; `ExprRunProducts.outputs` is a generic, caller-
//! populated map (via [`ExprRunProducts::with_output`]) so any named scalar series -- a future
//! power subsystem's state of charge, say -- can be attached without changing this type. The one
//! producer `crate::drm::executor::execute` itself populates today is [`speed_output`]:
//! `output.<instance>.speed@time`, `|velocity|` derived from the instance's own real, already-
//! propagated `Trajectory` (GMAT-bound or native, whichever this run actually used). This is
//! *not* `av_dynamics::StepResult.outputs` (`GmatModel` never overrides `DynamicsModel::step`,
//! so that channel is always empty for a GMAT-bound instance, and `crate::kernel::HeteroKernel`
//! never reads `StepResult.outputs` even when a model does populate it -- neither file is owned
//! by this task) and it is *not* a GMAT `GetRealParameter`-style calculated field (`gmat-sys`'s
//! FFI shim exposes no such call) -- seeing either through would mean editing files outside this
//! task's ownership. `speed_output` is instead computed directly, in this crate, from the real
//! propagation result every run already produces -- a genuine derived quantity of that run, not
//! a fabricated number, and the most `output.*` can honestly be today given those two
//! boundaries. Any other output name is [`crate::expr::error::ExprError::UnknownOutput`].
//!
//! **Events (`docs/open-questions.md` question 95, M9.3).** `crate::drm::executor::execute` now
//! emits real `Event`s -- run start/end (`EVENT_KIND_LIFECYCLE`) and every
//! `FAULT_TARGET_KIND_DYNAMICS` fault actually applied (`EVENT_KIND_FAULT`) -- see
//! `crate::drm::events`'s own module doc comment for exactly what is (and, honestly, is not)
//! emitted. `event.<name>.t` / `count(event.<kind>)` still resolve against whatever `events`
//! slice the caller passes to [`ExprRunProducts::new`], which for `execute()`'s own scoring
//! pass (and any caller re-deriving an `ExprRunProducts` from `RunProducts.events`) is that real
//! list, not a caller-supplied stand-in.

use std::collections::BTreeMap;

use av_cdm::pb::{Event, EventKind, Trajectory, Unit};

use crate::expr::error::ExprError;
use crate::interpolate::interpolate_by_state_space;
use crate::trajectory::state_space_for;

/// A named, unit-tagged scalar time series -- the shape both a trajectory component (once
/// projected down to one label) and a non-trajectory `output.<instance>.<name>` series share.
/// `epochs_tai_ns` is strictly increasing (the same contract `Trajectory.samples` already
/// keeps, by construction of `crate::kernel`/`crate::drm::executor`'s output).
#[derive(Debug, Clone)]
pub struct Series {
    pub epochs_tai_ns: Vec<i64>,
    pub values: Vec<f64>,
    pub unit: Unit,
}

/// `output.<instance>.speed`'s name -- see the module doc comment's "Outputs" note.
pub const SPEED_OUTPUT_NAME: &str = "speed";

/// The derived `output.<instance>.speed@time` series for one real, already-propagated
/// `Trajectory`: `|velocity|` (m/s) at each of the trajectory's own native samples. `None` when
/// `traj`'s declared `StateSpace` has no `vel_x`/`vel_y`/`vel_z` components (every state space
/// this crate currently declares does -- see `crate::trajectory` -- so this is a defensive
/// `None`, not a case any run today actually hits) -- never a fabricated zero. See the module
/// doc comment's "Outputs" note for why this, not `av_dynamics::StepResult.outputs` or a GMAT
/// calculated field, is what `crate::drm::executor::execute` attaches.
pub fn speed_output(traj: &Trajectory) -> Option<Series> {
    let space = state_space_for(&traj.state_space_id).ok()?;
    let index_of = |label: &str| space.components.iter().position(|c| c.label == label);
    let (ix, iy, iz) = (index_of("vel_x")?, index_of("vel_y")?, index_of("vel_z")?);
    Some(Series {
        epochs_tai_ns: traj.samples.iter().map(|s| s.tai_ns).collect(),
        values: traj.samples.iter().map(|s| (s.mean[ix].powi(2) + s.mean[iy].powi(2) + s.mean[iz].powi(2)).sqrt()).collect(),
        unit: Unit::MeterPerSecond,
    })
}

pub struct ExprRunProducts<'a> {
    pub start_tai_ns: i64,
    pub end_tai_ns: i64,
    trajectories: BTreeMap<String, &'a Trajectory>,
    events: &'a [Event],
    outputs: BTreeMap<(String, String), Series>,
}

impl<'a> ExprRunProducts<'a> {
    /// Build a `ExprRunProducts` from exactly what `crate::drm::executor::execute` returns
    /// (`&BTreeMap<String, Trajectory>`, entity id == instance name -- see the module doc
    /// comment), plus the scenario window every `@start`/`@end`/aggregate needs and whatever
    /// `Event`s the caller has (empty is fine -- see the module doc comment's "Events" note).
    pub fn new(start_tai_ns: i64, end_tai_ns: i64, trajectories: &'a BTreeMap<String, Trajectory>, events: &'a [Event]) -> Self {
        ExprRunProducts { start_tai_ns, end_tai_ns, trajectories: trajectories.iter().map(|(k, v)| (k.clone(), v)).collect(), events, outputs: BTreeMap::new() }
    }

    /// Attach a non-trajectory named output series (see the module doc comment's "Outputs"
    /// note) -- forward-looking; no current DRM run populates this.
    pub fn with_output(mut self, instance: &str, name: &str, series: Series) -> Self {
        self.outputs.insert((instance.to_string(), name.to_string()), series);
        self
    }

    fn window_seconds(&self) -> (f64, f64) {
        (0.0, (self.end_tai_ns - self.start_tai_ns) as f64 * 1e-9)
    }

    /// Resolve a `time` production (already parsed as [`crate::expr::ast::TimeSpec`]) to an
    /// absolute TAI nanosecond epoch. `'start'`/`'end'` are exactly `start_tai_ns`/
    /// `end_tai_ns`; `number 's'` is that many seconds after `start_tai_ns`; an event name
    /// resolves to that event's own `tai_ns` (module doc's "an event name resolves to its
    /// epoch").
    pub fn resolve_time(&self, spec: &crate::expr::ast::TimeSpec, pos: usize) -> Result<i64, ExprError> {
        use crate::expr::ast::TimeSpec;
        if self.end_tai_ns <= self.start_tai_ns {
            return Err(ExprError::EmptyRun);
        }
        match spec {
            TimeSpec::Start => Ok(self.start_tai_ns),
            TimeSpec::End => Ok(self.end_tai_ns),
            TimeSpec::OffsetSeconds { seconds, .. } => Ok(self.start_tai_ns + (seconds * 1e9).round() as i64),
            TimeSpec::EventName { name, pos: name_pos } => self.event_epoch_tai_ns(name, *name_pos),
        }
        .and_then(|tai_ns| {
            let (start_s, end_s) = self.window_seconds();
            let got_s = (tai_ns - self.start_tai_ns) as f64 * 1e-9;
            if tai_ns < self.start_tai_ns || tai_ns > self.end_tai_ns {
                Err(ExprError::TimeOutOfRange { requested_s: got_s, start_s, end_s, pos })
            } else {
                Ok(tai_ns)
            }
        })
    }

    fn event_epoch_tai_ns(&self, name: &str, pos: usize) -> Result<i64, ExprError> {
        let mut matches = self.events.iter().filter(|e| e.name == name);
        let first = matches.next().ok_or_else(|| ExprError::UnknownEventName { name: name.to_string(), pos })?;
        if matches.next().is_some() {
            return Err(ExprError::AmbiguousEventName { name: name.to_string(), pos });
        }
        Ok(first.tai_ns)
    }

    /// The declared unit of `entity.<id>.<component>`, from the id's trajectory's own declared
    /// `StateSpace` -- metadata only, never touches `Trajectory.samples`' actual numbers (the
    /// "parse/typecheck, not a runtime surprise" property `crate::expr::typecheck` relies on).
    pub fn entity_component_unit(&self, id: &str, component: &str, pos: usize) -> Result<Unit, ExprError> {
        let traj = self.trajectories.get(id).ok_or_else(|| ExprError::UnknownEntity { id: id.to_string(), pos })?;
        let space = state_space_for(&traj.state_space_id).map_err(|_| ExprError::UnknownEntity { id: id.to_string(), pos })?;
        space
            .components
            .iter()
            .find(|c| c.label == component)
            .map(|c| Unit::try_from(c.unit).unwrap_or(Unit::Unspecified))
            .ok_or_else(|| ExprError::UnknownComponent { entity: id.to_string(), component: component.to_string(), pos })
    }

    fn entity_component_index(&self, id: &str, component: &str, pos: usize) -> Result<(&'a Trajectory, usize), ExprError> {
        let traj = *self.trajectories.get(id).ok_or_else(|| ExprError::UnknownEntity { id: id.to_string(), pos })?;
        let space = state_space_for(&traj.state_space_id).map_err(|_| ExprError::UnknownEntity { id: id.to_string(), pos })?;
        let idx = space.components.iter().position(|c| c.label == component).ok_or_else(|| ExprError::UnknownComponent { entity: id.to_string(), component: component.to_string(), pos })?;
        Ok((traj, idx))
    }

    /// `entity.<id>.<component>` at a specific `tai_ns`, interpolated per ADR-005 sec 3
    /// (`crate::interpolate::interpolate_by_state_space`) -- an exact sample match short-
    /// circuits the interpolator entirely (no floating-point interpolation noise at an
    /// endpoint that is already a native sample).
    pub fn entity_component_at(&self, id: &str, component: &str, tai_ns: i64, pos: usize) -> Result<(f64, Unit), ExprError> {
        let (traj, idx) = self.entity_component_index(id, component, pos)?;
        let unit = self.entity_component_unit(id, component, pos)?;
        if traj.samples.is_empty() {
            return Err(ExprError::EmptyTrajectory { id: id.to_string(), pos });
        }
        if let Some(exact) = traj.samples.iter().find(|s| s.tai_ns == tai_ns) {
            return Ok((exact.mean[idx], unit));
        }
        let bracket = traj.samples.windows(2).find(|w| w[0].tai_ns <= tai_ns && tai_ns <= w[1].tai_ns);
        let Some(w) = bracket else {
            return Err(ExprError::TimeOutOfRange {
                requested_s: (tai_ns - self.start_tai_ns) as f64 * 1e-9,
                start_s: (traj.samples.first().unwrap().tai_ns - self.start_tai_ns) as f64 * 1e-9,
                end_s: (traj.samples.last().unwrap().tai_ns - self.start_tai_ns) as f64 * 1e-9,
                pos,
            });
        };
        let space = state_space_for(&traj.state_space_id).expect("already resolved above");
        let out = interpolate_by_state_space(&space, w[0].tai_ns, &w[0].mean, w[1].tai_ns, &w[1].mean, tai_ns)
            .map_err(|e| ExprError::Interpolation { entity: id.to_string(), pos, source: e.to_string() })?;
        Ok((out[idx], unit))
    }

    /// `entity.<id>.<component>`'s whole series (an aggregate argument, no `@time`): the
    /// trajectory's own native samples, unconverted -- see `crate::expr::eval`'s module doc
    /// comment for why aggregates reduce over native samples rather than a resampled grid.
    pub fn entity_component_series(&self, id: &str, component: &str, pos: usize) -> Result<Series, ExprError> {
        let (traj, idx) = self.entity_component_index(id, component, pos)?;
        let unit = self.entity_component_unit(id, component, pos)?;
        if traj.samples.is_empty() {
            return Err(ExprError::EmptyTrajectory { id: id.to_string(), pos });
        }
        Ok(Series { epochs_tai_ns: traj.samples.iter().map(|s| s.tai_ns).collect(), values: traj.samples.iter().map(|s| s.mean[idx]).collect(), unit })
    }

    pub fn output_unit(&self, instance: &str, name: &str, pos: usize) -> Result<Unit, ExprError> {
        self.outputs.get(&(instance.to_string(), name.to_string())).map(|s| s.unit).ok_or_else(|| ExprError::UnknownOutput { instance: instance.to_string(), name: name.to_string(), pos })
    }

    pub fn output_series(&self, instance: &str, name: &str, pos: usize) -> Result<&Series, ExprError> {
        self.outputs.get(&(instance.to_string(), name.to_string())).ok_or_else(|| ExprError::UnknownOutput { instance: instance.to_string(), name: name.to_string(), pos })
    }

    /// `output.<instance>.<name>` at a specific time: linear interpolation between bracketing
    /// samples (no declared interpolation contract for a generic named output -- ADR-005 sec 3
    /// is about `StateSpace` components; a scalar output series has none, so linear is this
    /// evaluator's own, disclosed choice, not a claim of a declared contract).
    pub fn output_at(&self, instance: &str, name: &str, tai_ns: i64, pos: usize) -> Result<(f64, Unit), ExprError> {
        let series = self.output_series(instance, name, pos)?;
        if series.epochs_tai_ns.is_empty() {
            return Err(ExprError::UnknownOutput { instance: instance.to_string(), name: name.to_string(), pos });
        }
        if let Some(i) = series.epochs_tai_ns.iter().position(|&t| t == tai_ns) {
            return Ok((series.values[i], series.unit));
        }
        for w in series.epochs_tai_ns.windows(2).zip(series.values.windows(2)) {
            let ((t0, t1), (v0, v1)) = ((w.0[0], w.0[1]), (w.1[0], w.1[1]));
            if t0 <= tai_ns && tai_ns <= t1 {
                let s = (tai_ns - t0) as f64 / (t1 - t0) as f64;
                return Ok((v0 + (v1 - v0) * s, series.unit));
            }
        }
        Err(ExprError::TimeOutOfRange {
            requested_s: (tai_ns - self.start_tai_ns) as f64 * 1e-9,
            start_s: (series.epochs_tai_ns.first().copied().unwrap_or(self.start_tai_ns) - self.start_tai_ns) as f64 * 1e-9,
            end_s: (series.epochs_tai_ns.last().copied().unwrap_or(self.end_tai_ns) - self.start_tai_ns) as f64 * 1e-9,
            pos,
        })
    }

    /// `count(event.<kind>)`: `<kind>` is an `EventKind`'s own proto name with the
    /// `EVENT_KIND_` prefix stripped and lowercased (e.g. `"maneuver"`, `"contact_start"`) --
    /// the same canonical-name convention `crate::drm::schema`'s enum parsing already uses
    /// elsewhere in this crate, applied here to `EventKind` instead of a YAML enum field.
    pub fn count_events_by_kind(&self, kind_token: &str, pos: usize) -> Result<f64, ExprError> {
        let wire_name = format!("EVENT_KIND_{}", kind_token.to_ascii_uppercase());
        let kind = EventKind::from_str_name(&wire_name).ok_or_else(|| ExprError::UnknownEventKind { kind: kind_token.to_string(), pos })?;
        Ok(self.events.iter().filter(|e| e.kind == kind as i32).count() as f64)
    }

    /// `event.<name>.t`: that event's own epoch, in seconds since `start_tai_ns` (the same
    /// "seconds since scenario start" representation every other time value in this language
    /// uses).
    pub fn event_epoch_seconds(&self, name: &str, pos: usize) -> Result<f64, ExprError> {
        let tai_ns = self.event_epoch_tai_ns(name, pos)?;
        Ok((tai_ns - self.start_tai_ns) as f64 * 1e-9)
    }

    /// `range(<a>, <b>)`'s declared unit: metadata only (both entities must declare `pos_x`/
    /// `pos_y`/`pos_z` components, each of unit [`Unit::Meter`]) -- never touches sample data,
    /// matching `crate::expr::typecheck`'s "never touches `Trajectory.samples`" property. The
    /// three position components' own declared units are checked (not merely assumed to be
    /// `Meter`) so a `StateSpace` that mislabels them is refused here rather than producing a
    /// silently-wrong distance.
    pub fn range_unit(&self, a: &str, b: &str, pos: usize) -> Result<Unit, ExprError> {
        for id in [a, b] {
            for component in ["pos_x", "pos_y", "pos_z"] {
                let unit = self.entity_component_unit(id, component, pos)?;
                if unit != Unit::Meter {
                    return Err(ExprError::UnknownComponent { entity: id.to_string(), component: component.to_string(), pos });
                }
            }
        }
        Ok(Unit::Meter)
    }

    /// `range(<a>, <b>)@time`: the Euclidean distance between `a` and `b`'s own
    /// `pos_x`/`pos_y`/`pos_z` at `tai_ns`, each interpolated per ADR-005 sec 3 exactly like any
    /// other `entity.<id>.<component>@time` read (`entity_component_at`, reused here rather than
    /// reimplemented).
    pub fn range_at(&self, a: &str, b: &str, tai_ns: i64, pos: usize) -> Result<f64, ExprError> {
        let (ax, _) = self.entity_component_at(a, "pos_x", tai_ns, pos)?;
        let (ay, _) = self.entity_component_at(a, "pos_y", tai_ns, pos)?;
        let (az, _) = self.entity_component_at(a, "pos_z", tai_ns, pos)?;
        let (bx, _) = self.entity_component_at(b, "pos_x", tai_ns, pos)?;
        let (by, _) = self.entity_component_at(b, "pos_y", tai_ns, pos)?;
        let (bz, _) = self.entity_component_at(b, "pos_z", tai_ns, pos)?;
        Ok(((ax - bx).powi(2) + (ay - by).powi(2) + (az - bz).powi(2)).sqrt())
    }

    /// `range(<a>, <b>)`'s whole series (bare, no `@time` -- an aggregate or `duration`
    /// condition argument): sampled at `a`'s own native epochs (`entity_component_series`),
    /// with `b`'s three position components interpolated to each of those epochs
    /// (`entity_component_at`) -- disclosed choice (see `crate::expr::eval`'s module doc
    /// comment): the two entities need not share a sampling grid in general, but every DRM this
    /// crate's own executor produces samples every instance at the same `sample_interval_s`
    /// (`HeteroKernel::run`'s single `output_period_ns`), so in practice this is an exact
    /// match, not an interpolation.
    pub fn range_series(&self, a: &str, b: &str, pos: usize) -> Result<Series, ExprError> {
        let sx = self.entity_component_series(a, "pos_x", pos)?;
        let sy = self.entity_component_series(a, "pos_y", pos)?;
        let sz = self.entity_component_series(a, "pos_z", pos)?;
        if sx.values.is_empty() {
            return Err(ExprError::EmptyTrajectory { id: a.to_string(), pos });
        }
        let mut epochs_tai_ns = Vec::with_capacity(sx.values.len());
        let mut values = Vec::with_capacity(sx.values.len());
        for (i, &t) in sx.epochs_tai_ns.iter().enumerate() {
            let (bx, _) = self.entity_component_at(b, "pos_x", t, pos)?;
            let (by, _) = self.entity_component_at(b, "pos_y", t, pos)?;
            let (bz, _) = self.entity_component_at(b, "pos_z", t, pos)?;
            let dx = sx.values[i] - bx;
            let dy = sy.values[i] - by;
            let dz = sz.values[i] - bz;
            epochs_tai_ns.push(t);
            values.push((dx * dx + dy * dy + dz * dz).sqrt());
        }
        Ok(Series { epochs_tai_ns, values, unit: Unit::Meter })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use av_cdm::pb::{Trajectory, TrajectorySample};

    fn traj(samples: Vec<(i64, [f64; 6])>) -> Trajectory {
        Trajectory {
            id: "t".to_string(),
            entity_id: "veh".to_string(),
            state_space_id: crate::trajectory::CARTESIAN_POS_VEL_6_ID.to_string(),
            samples: samples.into_iter().map(|(t, m)| TrajectorySample { tai_ns: t, mean: m.to_vec(), ..Default::default() }).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn resolves_an_entity_component_at_an_exact_sample() {
        let mut map = BTreeMap::new();
        map.insert("veh".to_string(), traj(vec![(0, [1.0, 2.0, 3.0, 0.0, 0.0, 0.0]), (10_000_000_000, [11.0, 2.0, 3.0, 0.0, 0.0, 0.0])]));
        let run = ExprRunProducts::new(0, 10_000_000_000, &map, &[]);
        let (v, u) = run.entity_component_at("veh", "pos_x", 0, 0).unwrap();
        assert_eq!((v, u), (1.0, Unit::Meter));
        let (v, _) = run.entity_component_at("veh", "pos_x", 10_000_000_000, 0).unwrap();
        assert_eq!(v, 11.0);
    }

    #[test]
    fn unknown_entity_and_component_are_typed_errors() {
        let map: BTreeMap<String, Trajectory> = BTreeMap::new();
        let run = ExprRunProducts::new(0, 10, &map, &[]);
        assert!(matches!(run.entity_component_at("nope", "pos_x", 0, 3), Err(ExprError::UnknownEntity { pos: 3, .. })));

        let mut map2 = BTreeMap::new();
        map2.insert("veh".to_string(), traj(vec![(0, [0.0; 6]), (10, [0.0; 6])]));
        let run2 = ExprRunProducts::new(0, 10, &map2, &[]);
        assert!(matches!(run2.entity_component_at("veh", "no_such", 0, 7), Err(ExprError::UnknownComponent { pos: 7, .. })));
    }

    #[test]
    fn counts_events_by_kind_and_finds_an_event_epoch_by_name() {
        let map: BTreeMap<String, Trajectory> = BTreeMap::new();
        let events = vec![
            Event { id: "e1".to_string(), name: "burn1".to_string(), tai_ns: 5_000_000_000, kind: EventKind::Maneuver as i32, ..Default::default() },
            Event { id: "e2".to_string(), name: "burn2".to_string(), tai_ns: 8_000_000_000, kind: EventKind::Maneuver as i32, ..Default::default() },
        ];
        let run = ExprRunProducts::new(0, 10_000_000_000, &map, &events);
        assert_eq!(run.count_events_by_kind("maneuver", 0), Ok(2.0));
        assert_eq!(run.count_events_by_kind("contact_start", 0), Ok(0.0));
        assert!(matches!(run.count_events_by_kind("not_a_kind", 9), Err(ExprError::UnknownEventKind { pos: 9, .. })));
        assert_eq!(run.event_epoch_seconds("burn1", 0), Ok(5.0));
        assert!(matches!(run.event_epoch_seconds("nope", 2), Err(ExprError::UnknownEventName { pos: 2, .. })));
    }
}
