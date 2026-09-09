//! Per-sample configuration (F1a, `docs/feasibility-plan.md`'s F1 milestone): for one grid
//! point and one Monte Carlo draw, a per-sample copy of the DRM (seeds derived) and the SOS
//! (axis values applied as `SystemInstance.parameter_overrides`), plus the config hash covering
//! everything the sample actually ran with. See [`sample_config`]'s own doc comment for the
//! exact, decided rules this module applies.

use std::collections::BTreeMap;

use av_cdm::pb;
use av_kernel::drm::hash::{canonical_drm_hash, canonical_sos_hash, verify_drm_hash, verify_sos_hash, verify_system_hash};

use crate::error::SweepError;
use crate::grid::{self, AxisValue};
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
///   seeds and be an identical sample).
/// - **The grid point** at `point_index` (`crate::grid::expand_grid(sweep)`) is applied to a
///   clone of `sos`: for each axis value, the named `SystemInstance` must exist
///   ([`SweepError::UnknownInstance`]), and the named parameter must already be declared either
///   in that instance's own `parameter_overrides` or in the bound `SystemDefinition.parameters`
///   ([`SweepError::UndeclaredParameter`] otherwise -- a typo must not silently become a
///   meaningless override) and must not itself carry a non-empty `string_value`
///   ([`SweepError::StringValuedParameter`] otherwise -- a `double` axis cannot sweep a string
///   parameter). An existing override of the same name is replaced in place (never duplicated);
///   otherwise a new `Parameter` is appended, carrying the axis value and the `unit` from the
///   base `SystemDefinition.parameters` declaration when there is one. The SOS copy's `hash` is
///   then set to `canonical_sos_hash(&sos)`.
/// - **Every key in `drm.scenario.seeds`** is replaced, in a clone of `drm`, by
///   `derive_seed(base_value, sweep_hash, point_index, draw_index, key)` -- keys are never
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

    let grid_points = grid::expand_grid(sweep)?;
    let grid_len = grid_points.len();
    let point = grid_points
        .into_iter()
        .nth(point_index as usize)
        .ok_or(SweepError::PointIndexOutOfRange { point_index, grid_len })?;

    let mut sos_copy = sos.clone();
    for axis_value in &point.values {
        apply_axis_value(&mut sos_copy, systems, axis_value)?;
    }
    sos_copy.hash = canonical_sos_hash(&sos_copy);

    let mut drm_copy = drm.clone();
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

/// Apply one axis value to `sos` (in place) -- see [`sample_config`]'s own doc comment for the
/// exact rules this enforces.
fn apply_axis_value(sos: &mut pb::SosConfiguration, systems: &BTreeMap<String, pb::SystemDefinition>, axis: &AxisValue) -> Result<(), SweepError> {
    let instance = sos.instances.iter_mut().find(|i| i.name == axis.instance).ok_or_else(|| SweepError::UnknownInstance { instance: axis.instance.clone() })?;

    if let Some(existing) = instance.parameter_overrides.iter_mut().find(|p| p.name == axis.parameter) {
        if !existing.string_value.is_empty() {
            return Err(SweepError::StringValuedParameter { instance: axis.instance.clone(), parameter: axis.parameter.clone() });
        }
        existing.value = axis.value;
        return Ok(());
    }

    let system_id = instance.system_id.clone();
    let base_param = systems.get(&system_id).and_then(|sys| sys.parameters.iter().find(|p| p.name == axis.parameter));
    let base_param = match base_param {
        Some(p) => p,
        None => return Err(SweepError::UndeclaredParameter { instance: axis.instance.clone(), parameter: axis.parameter.clone() }),
    };
    if !base_param.string_value.is_empty() {
        return Err(SweepError::StringValuedParameter { instance: axis.instance.clone(), parameter: axis.parameter.clone() });
    }
    let unit = base_param.unit;
    instance.parameter_overrides.push(pb::Parameter { name: axis.parameter.clone(), unit, value: axis.value, string_value: String::new(), min: 0.0, max: 0.0, description: String::new() });
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
}
