//! Per-sample configuration (F1a, `docs/feasibility-plan.md`'s F1 milestone): for one grid
//! point and one Monte Carlo draw, a per-sample copy of the DRM (seeds derived, event axis
//! values applied) and the SOS (parameter axis values applied as
//! `SystemInstance.parameter_overrides`), plus the config hash covering everything the sample
//! actually ran with. See [`sample_config`]'s own doc comment for the exact, decided rules this
//! module applies.

use std::collections::BTreeMap;

use av_cdm::pb;
use av_kernel::drm::hash::{canonical_drm_hash, canonical_sos_hash, verify_drm_hash, verify_sos_hash, verify_system_hash};

use crate::error::SweepError;
use crate::grid::{self, AxisTarget, AxisValue};
use crate::hash::sample_config_hash;
use crate::seed::derive_seed;

/// The full, self-contained configuration one sample (`point_index`, `draw_index`) ran with.
#[derive(Debug, Clone)]
pub struct SampleConfig {
    /// The sweep's DRM, cloned, with every `Scenario.seeds` entry replaced by its derived
    /// per-sample seed and `hash` recomputed.
    pub drm: pb::DesignReferenceMission,
    /// The sweep's SOS, cloned, with this point's axis values applied as
    /// `SystemInstance.parameter_overrides` and `hash` recomputed.
    pub sos: pb::SosConfiguration,
    /// The `SystemDefinition`s the sample ran with -- unchanged from what the caller supplied,
    /// but part of [`SampleConfig::config_hash`] (see [`crate::hash::sample_config_hash`]'s own
    /// doc comment for why).
    pub systems: BTreeMap<String, pb::SystemDefinition>,
    pub point_index: u32,
    pub draw_index: u32,
    /// `"instance.parameter"` -> the value applied at this point -- the exact shape
    /// `SweepSample.axis_values` (`run.proto`) wants.
    pub axis_values: BTreeMap<String, f64>,
    /// `Scenario.seeds` key -> this sample's derived seed (the same values now written into
    /// [`SampleConfig::drm`]'s own `scenario.seeds`).
    pub seeds: BTreeMap<String, u64>,
    /// The per-sample configuration hash -- see [`crate::hash::sample_config_hash`]'s doc
    /// comment for the exact byte layout and for why this is broader than `run.proto`'s own
    /// `SweepSample.config_hash` field comment ("the per-sample DRM hash").
    pub config_hash: String,
}

/// Build one sample's configuration. `sweep_hash` is the sweep's own canonical hash (e.g. from
/// [`crate::hash::canonical_sweep_hash`]), threaded through to [`derive_seed`] rather than
/// recomputed here (the caller already has it once per study, not once per sample).
///
/// Rules (decided, not open):
///
/// - **Base-artifact hashes are verified first**, before anything is copied or mutated
///   (`av_kernel::drm::hash::verify_drm_hash`/`verify_sos_hash`/`verify_system_hash`, wrapped
///   into [`SweepError::Drm`]) -- a deliberate addition beyond this crate's literal task brief,
///   disclosed in `REPORT.md`: a per-sample artifact is never built on top of a base DRM/SOS/
///   `SystemDefinition` that does not match its own declared hash.
/// - **`sweep.drm_id` must equal `drm.id`** ([`SweepError::SweepDrmIdMismatch`] otherwise).
/// - **`monte_carlo_draws` must be nonzero** ([`SweepError::ZeroDraws`] otherwise -- never
///   defaulted to 1) and, if greater than 1, `drm.scenario.seeds` must be non-empty
///   ([`SweepError::DrawsAboveOneWithoutSeeds`] otherwise: every draw would derive identical
///   seeds and be an identical sample). Question 192(d): the same rule applies to
///   `sweep.dispersed` ([`SweepError::DispersedWithoutSeeds`] when `dispersed` is true and
///   `drm.scenario.seeds` is empty) -- a dispersed draw with nothing to sample from is refused
///   the same way, at the same place, for the same reason.
/// - **The grid point** at `point_index` (`crate::grid::expand_grid(sweep)`) is split by target
///   (question 192(c)) and applied to TWO separate clones, in this order:
///   1. **Parameter axis values -> a clone of `sos`**: for each, the named `SystemInstance` must
///      exist ([`SweepError::UnknownInstance`]), and the named parameter must already be
///      declared either in that instance's own `parameter_overrides` or in the bound
///      `SystemDefinition.parameters` ([`SweepError::UndeclaredParameter`] otherwise -- a typo
///      must not silently become a meaningless override) and must not itself carry a non-empty
///      `string_value` ([`SweepError::StringValuedParameter`] otherwise -- a `double` axis cannot
///      sweep a string parameter). An existing override of the same name is replaced in place
///      (never duplicated); otherwise a new `Parameter` is appended, carrying the axis value and
///      the `unit` from the base `SystemDefinition.parameters` declaration when there is one. The
///      SOS copy's `hash` is then set to `canonical_sos_hash(&sos)`.
///   2. **Event axis values -> a clone of `drm`**: for each, the named `event_id` must name a
///      `Scenario.events[].id` in the DRM copy ([`SweepError::UnknownEvent`] otherwise), and
///      `value_key` must already be a key in that event's own `values` map
///      ([`SweepError::UndeclaredEventValueKey`] otherwise -- the same "a typo must not silently
///      create a new key" posture as `UndeclaredParameter`). Applied BEFORE `Scenario.seeds` is
///      derived and BEFORE `canonical_drm_hash` is recomputed below, so the per-sample DRM hash
///      (and therefore `config_hash`) genuinely moves with the axis -- see
///      `tests::an_event_axis_value_changes_config_hash_between_grid_points` (this module, since
///      `crate::hash::sample_config_hash` itself never applies an axis -- it only hashes what it
///      is given -- so the ordering guarantee can only be proven through this function).
/// - **Every key in `drm.scenario.seeds`** is replaced, in the (now axis-mutated) clone of `drm`,
///   by `derive_seed(base_value, sweep_hash, point_index, draw_index, key)` -- keys are never
///   added or removed. The DRM copy's `hash` is then set to `canonical_drm_hash(&drm)`.
/// - **`config_hash`** covers the (now-rehashed) DRM copy, the SOS copy, and every supplied
///   `SystemDefinition`, in that order -- see [`crate::hash::sample_config_hash`].
pub fn sample_config(
    sweep: &pb::ParameterSweep,
    sweep_hash: &str,
    drm: &pb::DesignReferenceMission,
    sos: &pb::SosConfiguration,
    systems: &BTreeMap<String, pb::SystemDefinition>,
    point_index: u32,
    draw_index: u32,
) -> Result<SampleConfig, SweepError> {
    verify_drm_hash(drm)?;
    verify_sos_hash(sos)?;
    for (id, sys) in systems {
        verify_system_hash(id, sys)?;
    }

    if sweep.drm_id != drm.id {
        return Err(SweepError::SweepDrmIdMismatch { sweep_drm_id: sweep.drm_id.clone(), drm_id: drm.id.clone() });
    }

    if sweep.monte_carlo_draws == 0 {
        return Err(SweepError::ZeroDraws);
    }
    let seeds_empty = drm.scenario.as_ref().map(|s| s.seeds.is_empty()).unwrap_or(true);
    if sweep.monte_carlo_draws > 1 && seeds_empty {
        return Err(SweepError::DrawsAboveOneWithoutSeeds { draws: sweep.monte_carlo_draws });
    }
    if sweep.dispersed && seeds_empty {
        return Err(SweepError::DispersedWithoutSeeds);
    }

    let grid_points = grid::expand_grid(sweep)?;
    let grid_len = grid_points.len();
    let point = grid_points
        .into_iter()
        .nth(point_index as usize)
        .ok_or(SweepError::PointIndexOutOfRange { point_index, grid_len })?;

    let mut sos_copy = sos.clone();
    for axis_value in &point.values {
        if matches!(axis_value.target, AxisTarget::Parameter { .. }) {
            apply_axis_value(&mut sos_copy, systems, axis_value)?;
        }
    }
    sos_copy.hash = canonical_sos_hash(&sos_copy);

    // Event axis values land on the DRM copy, and must be applied BEFORE Scenario.seeds is
    // derived and BEFORE canonical_drm_hash is recomputed below -- see this function's own doc
    // comment's "Event axis values" bullet for why the ordering is load-bearing.
    let mut drm_copy = drm.clone();
    for axis_value in &point.values {
        if matches!(axis_value.target, AxisTarget::Event { .. }) {
            apply_event_axis_value(&mut drm_copy, axis_value)?;
        }
    }

    let mut derived_seeds = BTreeMap::new();
    if let Some(scenario) = drm_copy.scenario.as_mut() {
        let mut new_seeds = BTreeMap::new();
        for (key, base_value) in scenario.seeds.iter() {
            let derived = derive_seed(*base_value, sweep_hash, point_index, draw_index, key)?;
            new_seeds.insert(key.clone(), derived);
            derived_seeds.insert(key.clone(), derived);
        }
        scenario.seeds = new_seeds;
    }
    drm_copy.hash = canonical_drm_hash(&drm_copy);

    let config_hash = sample_config_hash(&drm_copy, &sos_copy, systems);

    Ok(SampleConfig {
        drm: drm_copy,
        sos: sos_copy,
        systems: systems.clone(),
        point_index,
        draw_index,
        axis_values: point.axis_values_map(),
        seeds: derived_seeds,
        config_hash,
    })
}

/// A declared bound is `min`/`max` both left at proto3's zero default (`Parameter`'s own field
/// comment) -- checked only when `max > min` actually declares a real bound (F1a review defect
/// #2; see [`SweepError::ParameterOutOfBounds`]'s own doc comment).
fn check_bound(instance: &str, parameter: &str, value: f64, min: f64, max: f64) -> Result<(), SweepError> {
    if max > min && (value < min || value > max) {
        return Err(SweepError::ParameterOutOfBounds { instance: instance.to_string(), parameter: parameter.to_string(), value, min, max });
    }
    Ok(())
}

/// Apply one PARAMETER axis value to `sos` (in place) -- see [`sample_config`]'s own doc comment
/// for the exact rules this enforces. Caller-guaranteed precondition: `axis.target` is
/// `AxisTarget::Parameter` (checked by [`sample_config`]'s own `matches!` filter before calling
/// this); panics via the `let else` below if that precondition is ever violated, rather than
/// silently misbehaving on an event axis value.
fn apply_axis_value(sos: &mut pb::SosConfiguration, systems: &BTreeMap<String, pb::SystemDefinition>, axis: &AxisValue) -> Result<(), SweepError> {
    let AxisTarget::Parameter { instance: instance_name, parameter } = &axis.target else {
        unreachable!("apply_axis_value is only ever called for an AxisTarget::Parameter axis value -- see sample_config's own matches! filter")
    };

    let instance = sos.instances.iter_mut().find(|i| &i.name == instance_name).ok_or_else(|| SweepError::UnknownInstance { instance: instance_name.clone() })?;

    if let Some(existing) = instance.parameter_overrides.iter_mut().find(|p| &p.name == parameter) {
        if !existing.string_value.is_empty() {
            return Err(SweepError::StringValuedParameter { instance: instance_name.clone(), parameter: parameter.clone() });
        }
        check_bound(instance_name, parameter, axis.value, existing.min, existing.max)?;
        existing.value = axis.value;
        return Ok(());
    }

    // F1a review defect #1: the instance's own system_id must resolve in `systems` BEFORE its
    // parameters are searched -- an unknown system_id is never blamed on the parameter (the
    // previous behaviour, folding this case into the "not found" arm below, named the wrong
    // cause; see SweepError::UnknownSystem's own doc comment).
    let system_id = instance.system_id.clone();
    let sys = systems.get(&system_id).ok_or_else(|| SweepError::UnknownSystem { instance: instance_name.clone(), system_id: system_id.clone() })?;
    let base_param = match sys.parameters.iter().find(|p| &p.name == parameter) {
        Some(p) => p,
        None => return Err(SweepError::UndeclaredParameter { instance: instance_name.clone(), parameter: parameter.clone() }),
    };
    if !base_param.string_value.is_empty() {
        return Err(SweepError::StringValuedParameter { instance: instance_name.clone(), parameter: parameter.clone() });
    }
    check_bound(instance_name, parameter, axis.value, base_param.min, base_param.max)?;
    let unit = base_param.unit;
    instance.parameter_overrides.push(pb::Parameter { name: parameter.clone(), unit, value: axis.value, string_value: String::new(), min: 0.0, max: 0.0, description: String::new() });
    Ok(())
}

/// Apply one EVENT axis value to `drm` (in place) -- see [`sample_config`]'s own doc comment's
/// "Event axis values" bullet for why this runs before `Scenario.seeds` derivation and
/// `canonical_drm_hash`. Caller-guaranteed precondition: `axis.target` is `AxisTarget::Event`
/// (mirrors [`apply_axis_value`]'s own precondition, in the other direction).
fn apply_event_axis_value(drm: &mut pb::DesignReferenceMission, axis: &AxisValue) -> Result<(), SweepError> {
    let AxisTarget::Event { event_id, value_key } = &axis.target else {
        unreachable!("apply_event_axis_value is only ever called for an AxisTarget::Event axis value -- see sample_config's own matches! filter")
    };

    let event = drm
        .scenario
        .as_mut()
        .and_then(|s| s.events.iter_mut().find(|e| &e.id == event_id))
        .ok_or_else(|| SweepError::UnknownEvent { event_id: event_id.clone() })?;

    // A typo must never silently create a new key -- the same posture as UndeclaredParameter for
    // instance axes (sample_config's own doc comment).
    if !event.values.contains_key(value_key) {
        return Err(SweepError::UndeclaredEventValueKey { event_id: event_id.clone(), value_key: value_key.clone() });
    }
    event.values.insert(value_key.clone(), axis.value);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_drm() -> pb::DesignReferenceMission {
        let yaml = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../drms/demo_two_instance.drm.yaml")).expect("read demo_two_instance.drm.yaml");
        av_kernel::drm::schema::parse_drm_yaml(&yaml).expect("parses")
    }
    fn fixture_sos() -> pb::SosConfiguration {
        let yaml = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../drms/demo_two_instance.sos.yaml")).expect("read demo_two_instance.sos.yaml");
        av_kernel::drm::schema::parse_sos_yaml(&yaml).expect("parses")
    }
    fn fixture_systems() -> BTreeMap<String, pb::SystemDefinition> {
        let yaml = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../drms/demo_two_instance.system.yaml")).expect("read demo_two_instance.system.yaml");
        let sys = av_kernel::drm::schema::parse_system_definition_yaml(&yaml).expect("parses");
        let mut m = BTreeMap::new();
        m.insert(sys.id.clone(), sys);
        m
    }

    fn axis(instance: &str, parameter: &str, values: Vec<f64>) -> pb::SweepAxis {
        pb::SweepAxis { instance: instance.to_string(), parameter: parameter.to_string(), values, ..Default::default() }
    }
    fn build_sweep(axes: Vec<pb::SweepAxis>, draws: u32) -> pb::ParameterSweep {
        pb::ParameterSweep { id: "sweep_test".to_string(), drm_id: "demo_two_instance_drm".to_string(), axes, monte_carlo_draws: draws, ..Default::default() }
    }

    #[test]
    fn axis_values_become_instance_parameter_overrides_and_the_sos_is_rehashed() {
        let drm = fixture_drm();
        let sos = fixture_sos();
        let systems = fixture_systems();
        let sweep = build_sweep(vec![axis("demo_flt", "spacecraft.SMA", vec![7000.0])], 1);
        let sweep_hash = crate::hash::canonical_sweep_hash(&sweep);

        let cfg = sample_config(&sweep, &sweep_hash, &drm, &sos, &systems, 0, 0).expect("samples");

        let inst = cfg.sos.instances.iter().find(|i| i.name == "demo_flt").expect("demo_flt present");
        let param = inst.parameter_overrides.iter().find(|p| p.name == "spacecraft.SMA").expect("override appended");
        assert_eq!(param.value, 7000.0);
        assert_eq!(param.string_value, "");
        let base_unit = systems.get("leo_demo_sys").unwrap().parameters.iter().find(|p| p.name == "spacecraft.SMA").unwrap().unit;
        assert_eq!(param.unit, base_unit, "unit copied from the base SystemDefinition declaration");

        assert_eq!(cfg.sos.hash, canonical_sos_hash(&cfg.sos), "SOS copy is rehashed after the override is applied");
        assert_eq!(cfg.axis_values.get("demo_flt.spacecraft.SMA"), Some(&7000.0));
        assert_eq!(cfg.point_index, 0);
        assert_eq!(cfg.draw_index, 0);
    }

    #[test]
    fn an_existing_override_of_the_same_name_is_replaced_not_duplicated() {
        let drm = fixture_drm();
        let mut sos = fixture_sos();
        {
            let inst = sos.instances.iter_mut().find(|i| i.name == "demo_flt").unwrap();
            inst.parameter_overrides.push(pb::Parameter { name: "spacecraft.SMA".to_string(), value: 1234.0, ..Default::default() });
        }
        sos.hash = canonical_sos_hash(&sos);
        let systems = fixture_systems();
        let sweep = build_sweep(vec![axis("demo_flt", "spacecraft.SMA", vec![7000.0])], 1);
        let sweep_hash = crate::hash::canonical_sweep_hash(&sweep);

        let cfg = sample_config(&sweep, &sweep_hash, &drm, &sos, &systems, 0, 0).expect("samples");

        let inst = cfg.sos.instances.iter().find(|i| i.name == "demo_flt").unwrap();
        let matches: Vec<_> = inst.parameter_overrides.iter().filter(|p| p.name == "spacecraft.SMA").collect();
        assert_eq!(matches.len(), 1, "replaced in place, never duplicated");
        assert_eq!(matches[0].value, 7000.0);
    }

    #[test]
    fn refuses_an_axis_naming_an_unknown_instance() {
        let drm = fixture_drm();
        let sos = fixture_sos();
        let systems = fixture_systems();
        let sweep = build_sweep(vec![axis("no_such_instance", "spacecraft.SMA", vec![7000.0])], 1);
        let sweep_hash = crate::hash::canonical_sweep_hash(&sweep);
        let err = sample_config(&sweep, &sweep_hash, &drm, &sos, &systems, 0, 0).unwrap_err();
        assert!(matches!(err, SweepError::UnknownInstance { ref instance } if instance == "no_such_instance"), "{err:?}");
    }

    #[test]
    fn refuses_an_axis_naming_an_undeclared_parameter() {
        let drm = fixture_drm();
        let sos = fixture_sos();
        let systems = fixture_systems();
        let sweep = build_sweep(vec![axis("demo_mvr", "totally.undeclared.parameter", vec![1.0])], 1);
        let sweep_hash = crate::hash::canonical_sweep_hash(&sweep);
        let err = sample_config(&sweep, &sweep_hash, &drm, &sos, &systems, 0, 0).unwrap_err();
        assert!(matches!(err, SweepError::UndeclaredParameter { ref instance, ref parameter } if instance == "demo_mvr" && parameter == "totally.undeclared.parameter"), "{err:?}");
    }

    #[test]
    fn refuses_an_axis_on_a_string_valued_parameter() {
        let drm = fixture_drm();
        let sos = fixture_sos();
        let systems = fixture_systems();

        // Branch 1: the existing declaration is an instance override (demo_flt's own
        // `port.consume` override carries string_value "cd_cmd_in").
        let sweep = build_sweep(vec![axis("demo_flt", "port.consume", vec![1.0])], 1);
        let sweep_hash = crate::hash::canonical_sweep_hash(&sweep);
        let err = sample_config(&sweep, &sweep_hash, &drm, &sos, &systems, 0, 0).unwrap_err();
        assert!(matches!(err, SweepError::StringValuedParameter { ref instance, ref parameter } if instance == "demo_flt" && parameter == "port.consume"), "{err:?}");

        // Branch 2: the existing declaration is the base SystemDefinition's own parameter
        // (demo_mvr does not override force_model.central_body; leo_demo_sys declares it with
        // string_value "Earth").
        let sweep2 = build_sweep(vec![axis("demo_mvr", "force_model.central_body", vec![1.0])], 1);
        let sweep_hash2 = crate::hash::canonical_sweep_hash(&sweep2);
        let err2 = sample_config(&sweep2, &sweep_hash2, &drm, &sos, &systems, 0, 0).unwrap_err();
        assert!(matches!(err2, SweepError::StringValuedParameter { ref instance, ref parameter } if instance == "demo_mvr" && parameter == "force_model.central_body"), "{err2:?}");
    }

    /// F1a review defect #1: an axis on an instance whose `system_id` is not in the supplied
    /// `systems` map must be blamed on the missing system, not on the parameter -- fails against
    /// the pre-fix implementation, which returned `UndeclaredParameter` here (the parameter
    /// search silently treated "no such system" the same as "system present, parameter absent").
    #[test]
    fn refuses_an_axis_whose_instance_names_an_unknown_system_id() {
        let drm = fixture_drm();
        let mut sos = fixture_sos();
        {
            let inst = sos.instances.iter_mut().find(|i| i.name == "demo_mvr").unwrap();
            inst.system_id = "no_such_system_id".to_string();
        }
        sos.hash = canonical_sos_hash(&sos);
        let systems = fixture_systems(); // still keyed by "leo_demo_sys" -- "no_such_system_id" is genuinely absent
        let sweep = build_sweep(vec![axis("demo_mvr", "totally.new.parameter", vec![1.0])], 1);
        let sweep_hash = crate::hash::canonical_sweep_hash(&sweep);
        let err = sample_config(&sweep, &sweep_hash, &drm, &sos, &systems, 0, 0).unwrap_err();
        assert!(
            matches!(err, SweepError::UnknownSystem { ref instance, ref system_id } if instance == "demo_mvr" && system_id == "no_such_system_id"),
            "{err:?}"
        );
    }

    /// F1a review defect #2: a declared bound (`max > min`, a real bound -- not the 0/0
    /// "unbounded" default) must refuse an out-of-range axis value rather than silently applying
    /// it. Both branches [`apply_axis_value`] can take: an existing instance override, and the
    /// base `SystemDefinition` declaration.
    #[test]
    fn refuses_an_axis_value_outside_a_declared_bound() {
        let drm = fixture_drm();

        // Branch 1: the existing declaration is an instance override.
        let mut sos = fixture_sos();
        {
            let inst = sos.instances.iter_mut().find(|i| i.name == "demo_flt").unwrap();
            inst.parameter_overrides.push(pb::Parameter { name: "test.bounded".to_string(), value: 5.0, min: 0.0, max: 10.0, ..Default::default() });
        }
        sos.hash = canonical_sos_hash(&sos);
        let systems = fixture_systems();
        let sweep = build_sweep(vec![axis("demo_flt", "test.bounded", vec![20.0])], 1);
        let sweep_hash = crate::hash::canonical_sweep_hash(&sweep);
        let err = sample_config(&sweep, &sweep_hash, &drm, &sos, &systems, 0, 0).unwrap_err();
        assert!(
            matches!(err, SweepError::ParameterOutOfBounds { ref instance, ref parameter, value, min, max }
                if instance == "demo_flt" && parameter == "test.bounded" && value == 20.0 && min == 0.0 && max == 10.0),
            "{err:?}"
        );
        // A value inside the same declared bound is unaffected.
        let sweep_ok = build_sweep(vec![axis("demo_flt", "test.bounded", vec![7.0])], 1);
        let sweep_ok_hash = crate::hash::canonical_sweep_hash(&sweep_ok);
        sample_config(&sweep_ok, &sweep_ok_hash, &drm, &sos, &systems, 0, 0).expect("value inside the declared bound is accepted");

        // Branch 2: the existing declaration is the base SystemDefinition's own parameter
        // (spacecraft.SMA has no declared bound in the real fixture; give it one here).
        let mut systems2 = fixture_systems();
        {
            let sys = systems2.get_mut("leo_demo_sys").unwrap();
            let sma = sys.parameters.iter_mut().find(|p| p.name == "spacecraft.SMA").unwrap();
            sma.min = 6000.0;
            sma.max = 7000.0;
            sys.hash = av_kernel::drm::hash::canonical_system_hash(sys);
        }
        let sos2 = fixture_sos();
        let sweep2 = build_sweep(vec![axis("demo_mvr", "spacecraft.SMA", vec![8000.0])], 1);
        let sweep_hash2 = crate::hash::canonical_sweep_hash(&sweep2);
        let err2 = sample_config(&sweep2, &sweep_hash2, &drm, &sos2, &systems2, 0, 0).unwrap_err();
        assert!(
            matches!(err2, SweepError::ParameterOutOfBounds { ref instance, ref parameter, value, min, max }
                if instance == "demo_mvr" && parameter == "spacecraft.SMA" && value == 8000.0 && min == 6000.0 && max == 7000.0),
            "{err2:?}"
        );
    }

    /// A `Parameter` whose `min`/`max` are both left at the proto3 zero default (0.0/0.0) is
    /// "unbounded" by convention (`SweepError::ParameterOutOfBounds`'s own doc comment) -- every
    /// existing fixture parameter has no declared bound, so any axis value must still be
    /// accepted; this is the regression test proving the new bound check is conditional on
    /// `max > min`, not merely on `min`/`max` being present as fields.
    #[test]
    fn an_undeclared_zero_zero_bound_is_unbounded() {
        let drm = fixture_drm();
        let sos = fixture_sos();
        let systems = fixture_systems();
        // spacecraft.SMA has min: 0.0, max: 0.0 (never declared) in the real fixture -- a wildly
        // out-of-physical-range value must still be accepted by the bound check itself (this
        // crate does not validate physical plausibility, only declared bounds).
        let sweep = build_sweep(vec![axis("demo_flt", "spacecraft.SMA", vec![1_000_000.0])], 1);
        let sweep_hash = crate::hash::canonical_sweep_hash(&sweep);
        sample_config(&sweep, &sweep_hash, &drm, &sos, &systems, 0, 0).expect("a 0/0 bound is unbounded, not a silent [0,0] range");
    }

    #[test]
    fn the_per_sample_drm_carries_derived_seeds_and_a_matching_hash() {
        let mut drm = fixture_drm();
        {
            let scenario = drm.scenario.as_mut().expect("scenario present");
            scenario.seeds.insert("fault1".to_string(), 1000);
            scenario.seeds.insert("burn_seed".to_string(), 2000);
        }
        drm.hash = canonical_drm_hash(&drm);
        let sos = fixture_sos();
        let systems = fixture_systems();
        let sweep = build_sweep(vec![], 1);
        let sweep_hash = crate::hash::canonical_sweep_hash(&sweep);

        let cfg = sample_config(&sweep, &sweep_hash, &drm, &sos, &systems, 0, 0).expect("samples");

        let expected_fault1 = derive_seed(1000, &sweep_hash, 0, 0, "fault1").unwrap();
        let expected_burn = derive_seed(2000, &sweep_hash, 0, 0, "burn_seed").unwrap();
        let got_scenario = cfg.drm.scenario.as_ref().unwrap();
        assert_eq!(got_scenario.seeds.get("fault1"), Some(&expected_fault1));
        assert_eq!(got_scenario.seeds.get("burn_seed"), Some(&expected_burn));
        assert_eq!(cfg.seeds.get("fault1"), Some(&expected_fault1));
        assert_eq!(cfg.seeds.get("burn_seed"), Some(&expected_burn));
        assert_eq!(cfg.drm.hash, canonical_drm_hash(&cfg.drm), "DRM copy is rehashed after seeds are derived");
        assert_ne!(cfg.drm.hash, drm.hash, "derived seeds actually changed the DRM's own content");
    }

    #[test]
    fn refuses_draws_above_one_when_the_scenario_declares_no_seeds() {
        let drm = fixture_drm(); // demo_two_instance_drm declares no Scenario.seeds at all
        let sos = fixture_sos();
        let systems = fixture_systems();
        let sweep = build_sweep(vec![], 2);
        let sweep_hash = crate::hash::canonical_sweep_hash(&sweep);
        let err = sample_config(&sweep, &sweep_hash, &drm, &sos, &systems, 0, 0).unwrap_err();
        assert!(matches!(err, SweepError::DrawsAboveOneWithoutSeeds { draws: 2 }), "{err:?}");
    }

    /// Question 192(d): the same refusal `DrawsAboveOneWithoutSeeds` exists for
    /// (`monte_carlo_draws > 1`, no seeds) applies to `dispersed: true` with no seeds -- a
    /// dispersed draw with nothing to sample from is refused here too, at a single draw.
    #[test]
    fn refuses_a_dispersed_sweep_with_no_declared_seeds() {
        let drm = fixture_drm(); // demo_two_instance_drm declares no Scenario.seeds at all
        let sos = fixture_sos();
        let systems = fixture_systems();
        let mut sweep = build_sweep(vec![], 1);
        sweep.dispersed = true;
        let sweep_hash = crate::hash::canonical_sweep_hash(&sweep);
        let err = sample_config(&sweep, &sweep_hash, &drm, &sos, &systems, 0, 0).unwrap_err();
        assert!(matches!(err, SweepError::DispersedWithoutSeeds), "{err:?}");
    }

    #[test]
    fn refuses_zero_draws() {
        let drm = fixture_drm();
        let sos = fixture_sos();
        let systems = fixture_systems();
        let sweep = build_sweep(vec![], 0);
        let sweep_hash = crate::hash::canonical_sweep_hash(&sweep);
        let err = sample_config(&sweep, &sweep_hash, &drm, &sos, &systems, 0, 0).unwrap_err();
        assert!(matches!(err, SweepError::ZeroDraws));
    }

    #[test]
    fn refuses_a_sweep_whose_drm_id_does_not_match_the_drm() {
        let drm = fixture_drm();
        let sos = fixture_sos();
        let systems = fixture_systems();
        let mut sweep = build_sweep(vec![], 1);
        sweep.drm_id = "not_the_real_drm_id".to_string();
        let sweep_hash = crate::hash::canonical_sweep_hash(&sweep);
        let err = sample_config(&sweep, &sweep_hash, &drm, &sos, &systems, 0, 0).unwrap_err();
        assert!(matches!(err, SweepError::SweepDrmIdMismatch { ref sweep_drm_id, ref drm_id } if sweep_drm_id == "not_the_real_drm_id" && drm_id == "demo_two_instance_drm"), "{err:?}");
    }

    #[test]
    fn refuses_a_point_index_outside_the_grid() {
        let drm = fixture_drm();
        let sos = fixture_sos();
        let systems = fixture_systems();
        let sweep = build_sweep(vec![axis("demo_flt", "spacecraft.SMA", vec![7000.0])], 1); // 1-point grid
        let sweep_hash = crate::hash::canonical_sweep_hash(&sweep);
        let err = sample_config(&sweep, &sweep_hash, &drm, &sos, &systems, 5, 0).unwrap_err();
        assert!(matches!(err, SweepError::PointIndexOutOfRange { point_index: 5, grid_len: 1 }), "{err:?}");
    }

    fn event_axis(event_id: &str, value_key: &str, values: Vec<f64>) -> pb::SweepAxis {
        pb::SweepAxis { event_id: event_id.to_string(), value_key: value_key.to_string(), values, ..Default::default() }
    }

    /// Question 192(c): an event axis value lands on `Scenario.events[].values[value_key]` in the
    /// DRM copy, and the DRM copy's `hash` (recomputed via `canonical_drm_hash`) reflects it --
    /// `fixture_drm()` (`demo_two_instance.drm.yaml`) declares event `burn1` on instance
    /// `demo_mvr` with `values: {dv_x: 20.0, dv_y: 0.0, dv_z: 0.0}`.
    #[test]
    fn event_axis_values_land_on_the_drm_copy_and_the_drm_is_rehashed() {
        let drm = fixture_drm();
        let sos = fixture_sos();
        let systems = fixture_systems();
        let sweep = build_sweep(vec![event_axis("burn1", "dv_x", vec![35.0])], 1);
        let sweep_hash = crate::hash::canonical_sweep_hash(&sweep);

        let cfg = sample_config(&sweep, &sweep_hash, &drm, &sos, &systems, 0, 0).expect("samples");

        let event = cfg.drm.scenario.as_ref().unwrap().events.iter().find(|e| e.id == "burn1").expect("burn1 present");
        assert_eq!(event.values.get("dv_x"), Some(&35.0), "the axis value replaced the base dv_x=20.0");
        assert_eq!(event.values.get("dv_y"), Some(&0.0), "dv_y untouched");
        assert_eq!(cfg.drm.hash, canonical_drm_hash(&cfg.drm), "DRM copy is rehashed after the event axis value is applied");
        assert_ne!(cfg.drm.hash, drm.hash, "the event axis value actually changed the DRM copy's content");
        assert_eq!(cfg.axis_values.get("event:burn1.dv_x"), Some(&35.0), "axis_values keys an event axis \"event:{{event_id}}.{{value_key}}\"");
        // The SOS copy is untouched by an event axis -- it lands on the DRM half only.
        assert_eq!(cfg.sos.hash, canonical_sos_hash(&sos), "an event-only sweep must not rehash the SOS to a different value than its own unchanged content");
    }

    /// The riskiest ordering guarantee this module makes: the event axis value must be applied
    /// BEFORE `canonical_drm_hash` is recomputed, so `config_hash` genuinely moves with the axis.
    /// Two grid points differing ONLY in `dv_x` must therefore produce different `config_hash`
    /// values -- this is the sample.rs-level proof (`tests/fixture_study.rs` repeats it against a
    /// real GMAT-run study). Fails against an implementation that computes `drm_copy.hash` before
    /// applying the event axis value (break-and-restore evidence: `crates/av-sweep/REPORT.md`).
    #[test]
    fn an_event_axis_value_changes_config_hash_between_grid_points() {
        let drm = fixture_drm();
        let sos = fixture_sos();
        let systems = fixture_systems();
        let sweep = build_sweep(vec![event_axis("burn1", "dv_x", vec![10.0, 30.0])], 1);
        let sweep_hash = crate::hash::canonical_sweep_hash(&sweep);

        let cfg0 = sample_config(&sweep, &sweep_hash, &drm, &sos, &systems, 0, 0).expect("point 0 samples");
        let cfg1 = sample_config(&sweep, &sweep_hash, &drm, &sos, &systems, 1, 0).expect("point 1 samples");

        assert_ne!(cfg0.config_hash, cfg1.config_hash, "two grid points differing only in dv_x must produce different config_hash values");
        assert_ne!(cfg0.drm.hash, cfg1.drm.hash, "and specifically because the per-sample DRM hash itself differs");
    }

    #[test]
    fn refuses_an_event_axis_naming_an_unknown_event() {
        let drm = fixture_drm();
        let sos = fixture_sos();
        let systems = fixture_systems();
        let sweep = build_sweep(vec![event_axis("no_such_event", "dv_x", vec![1.0])], 1);
        let sweep_hash = crate::hash::canonical_sweep_hash(&sweep);
        let err = sample_config(&sweep, &sweep_hash, &drm, &sos, &systems, 0, 0).unwrap_err();
        assert!(matches!(err, SweepError::UnknownEvent { ref event_id } if event_id == "no_such_event"), "{err:?}");
    }

    /// A typo in `value_key` must never silently create a new key in `ScenarioEvent.values` --
    /// the same posture as `UndeclaredParameter` for instance axes (`burn1` declares `dv_x`,
    /// `dv_y`, `dv_z`; `dv_w` is not among them).
    #[test]
    fn refuses_an_event_axis_naming_an_undeclared_value_key() {
        let drm = fixture_drm();
        let sos = fixture_sos();
        let systems = fixture_systems();
        let sweep = build_sweep(vec![event_axis("burn1", "dv_w", vec![1.0])], 1);
        let sweep_hash = crate::hash::canonical_sweep_hash(&sweep);
        let err = sample_config(&sweep, &sweep_hash, &drm, &sos, &systems, 0, 0).unwrap_err();
        assert!(matches!(err, SweepError::UndeclaredEventValueKey { ref event_id, ref value_key } if event_id == "burn1" && value_key == "dv_w"), "{err:?}");
    }

    /// A parameter axis and an event axis coexist on the same sample: the parameter axis lands
    /// on the SOS copy, the event axis lands on the DRM copy, and each copy is rehashed to
    /// reflect only its own axis (proof the two halves do not cross-contaminate).
    #[test]
    fn a_parameter_axis_and_an_event_axis_coexist_on_one_sample() {
        let drm = fixture_drm();
        let sos = fixture_sos();
        let systems = fixture_systems();
        let sweep = build_sweep(vec![axis("demo_flt", "spacecraft.SMA", vec![7000.0]), event_axis("burn1", "dv_x", vec![35.0])], 1);
        let sweep_hash = crate::hash::canonical_sweep_hash(&sweep);

        let cfg = sample_config(&sweep, &sweep_hash, &drm, &sos, &systems, 0, 0).expect("samples");

        let inst = cfg.sos.instances.iter().find(|i| i.name == "demo_flt").unwrap();
        assert_eq!(inst.parameter_overrides.iter().find(|p| p.name == "spacecraft.SMA").unwrap().value, 7000.0);
        let event = cfg.drm.scenario.as_ref().unwrap().events.iter().find(|e| e.id == "burn1").unwrap();
        assert_eq!(event.values.get("dv_x"), Some(&35.0));
        assert_eq!(cfg.axis_values.len(), 2);
        assert_eq!(cfg.axis_values.get("demo_flt.spacecraft.SMA"), Some(&7000.0));
        assert_eq!(cfg.axis_values.get("event:burn1.dv_x"), Some(&35.0));
    }
}
