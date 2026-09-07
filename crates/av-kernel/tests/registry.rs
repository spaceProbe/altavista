//! [`ModelRegistry`] as the sole constructor (M10.3, `docs/open-questions.md` question 98):
//! `crate::registry`'s own module doc comment promises a GMAT-gated `construct_gmat` test here,
//! proving the registry works standalone -- not merely as a component `drm::execute` happens to
//! route through -- and, concretely, that [`ModelHandle::t0_tai_ns`] is exactly the declared
//! epoch (question 96: never GMAT's own A1MJD read back).

use std::collections::BTreeMap;
use std::path::PathBuf;

use av_kernel::drm::binding::{classify_binding, BindingPlan, Classification};
use av_kernel::drm::schema;
use av_kernel::registry::ModelRegistry;
use gmat_sys::Gmat;

fn drms_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../drms").join(name)
}

/// Load the golden DRM bundle and classify its own `"leo"` instance, exactly the way
/// `crate::drm::executor::execute` does in its pass 1 -- proving `ModelRegistry::construct_gmat`
/// accepts the same `GmatSystemSpec`/`epoch_tai_ns` shape the real executor builds, not a
/// hand-simplified stand-in.
fn classify_golden_leo() -> (av_cdm::pb::DesignReferenceMission, av_cdm::pb::SystemDefinition, av_cdm::pb::SystemInstance) {
    let drm = schema::parse_drm_yaml(&std::fs::read_to_string(drms_path("leo_1day_golden.drm.yaml")).unwrap()).expect("DRM parses");
    let sos = schema::parse_sos_yaml(&std::fs::read_to_string(drms_path("leo_1day_golden.sos.yaml")).unwrap()).expect("SosConfiguration parses");
    let sys = schema::parse_system_definition_yaml(&std::fs::read_to_string(drms_path("leo_1day_golden.system.yaml")).unwrap()).expect("SystemDefinition parses");
    let instance = sos.instances.into_iter().find(|i| i.name == "leo").expect("golden SosConfiguration declares a \"leo\" instance");
    (drm, sys, instance)
}

/// `ModelRegistry::construct_gmat` builds a real, steppable `GmatModel` from the same
/// `GmatSystemSpec`/epoch `crate::drm::binding::classify_binding` produces for the golden bundle
/// -- standalone, with no `crate::drm::executor::execute` call in between. Also the direct proof
/// of question 96: `ModelHandle::t0_tai_ns` is exactly `Scenario.start_tai_ns`, the declared
/// integer, never a value round-tripped through GMAT's own A1MJD.
#[test]
fn construct_gmat_builds_a_steppable_model_with_the_exact_declared_epoch() {
    let _engine = gmat_sys::engine_lock();
    let (drm, sys, instance) = classify_golden_leo();
    let scenario = drm.scenario.expect("golden DRM declares a scenario");
    let options = drm.options.expect("golden DRM declares options");

    let plan = classify_binding(&instance, &sys, &options).expect("golden \"leo\" instance classifies");
    let Classification::Model(BindingPlan::Gmat(spec)) = plan else { panic!("golden \"leo\" instance is gmat.-dispatched") };

    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let handle = ModelRegistry::construct_gmat(
        &gmat,
        &spec,
        scenario.start_tai_ns,
        &sys.dynamics_model,
        "registry_test_ns",
        "registry_test_leo",
        &sys.state_space_id,
        /* with_stm */ false,
        options.accept_missing_stm_terms,
    )
    .expect("construct_gmat succeeds against the golden bundle's own spec");

    // Question 96: t0_tai_ns is the exact declared epoch, an integer, never GMAT's own A1MJD
    // read back through av_cdm::time::Tai::from_a1_mjd (which would only ever coincidentally
    // equal the declared value, not by construction).
    assert_eq!(handle.t0_tai_ns, scenario.start_tai_ns, "ModelHandle::t0_tai_ns must be exactly the declared epoch, not a value reconciled through GMAT's own A1MJD");
    assert_eq!(handle.state_dim(), 6);
    assert!(!handle.stm_capable(), "constructed with with_stm = false");
    assert_eq!(handle.describe().id, "gmat.registry_test_leo");
    assert!(!handle.settings.is_empty(), "settings carries the force-model/spacecraft description used to build settings_hash");

    let x0 = handle.x0_si.clone();
    let t0 = handle.t0_tai_ns;
    let boxed = handle.into_boxed("leo");
    // One 10 s step; a LEO orbit does not move by more than a few tens of km in 10 s, so this
    // is a coarse sanity bound on the erased model actually being steppable end to end, not a
    // precision check (the golden arc itself, `tests/golden_acceptance.rs`/`tests/
    // drm_executor.rs`, already pins the real numbers).
    let step = boxed.step(&x0, t0, &[], 10_000_000_000).expect("erased ModelHandle steps");
    let dr = (0..3).map(|i| (step.state[i] - x0[i]).powi(2)).sum::<f64>().sqrt();
    assert!(dr > 0.0 && dr < 100_000.0, "10 s LEO displacement should be a few tens of km, got {dr} m");
}

/// `ModelRegistry::construct_native` is exercised directly by `crate::registry`'s own
/// `#[cfg(test)]` module (GMAT-free); this file's own scope is the GMAT-gated `construct_gmat`
/// path, so it does not repeat that native-side coverage.
///
/// `ModelRegistry::construct_gmat` reports a typed [`av_dynamics::ModelError::Gmat`], not a
/// panic or a silently-empty model, when the underlying `GmatSystemSpec` is missing a field
/// `crate::drm::binding::parse_gmat_spec` would already have refused at classification time --
/// exercised here directly against the registry (bypassing `classify_binding` on purpose) to
/// prove the registry's own error path, not merely that classification catches it first.
#[test]
fn construct_gmat_reports_a_gmat_ffi_failure_as_a_typed_model_error() {
    let _engine = gmat_sys::engine_lock();
    let spec = av_kernel::drm::binding::GmatSystemSpec {
        central_body: "Earth".to_string(),
        // An empty gravity_file: GMAT's own PotentialFile field refuses this at
        // GravityField::set_str/initialize -- a genuine FFI failure, not a fabricated one.
        gravity_file: String::new(),
        gravity_degree: 8,
        gravity_order: 8,
        spacecraft_str: BTreeMap::from([("CoordinateSystem".to_string(), "EarthMJ2000Eq".to_string()), ("DisplayStateType".to_string(), "Cartesian".to_string())]),
        ..Default::default()
    };
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    // `ModelHandle` (the `Ok` side) is deliberately not `Debug` (it wraps a GMAT FFI handle),
    // so this matches the `Result` directly rather than using `expect_err`/`unwrap_err`, the
    // same pattern `crate::registry`'s own `construct_remote_is_a_typed_refusal_naming_the_
    // model_id` test uses for the same reason.
    let err = match ModelRegistry::construct_gmat(&gmat, &spec, 1_700_000_000_000_000_000, "gmat.test", "registry_test_bad_spec_ns", "registry_test_bad_spec", "test.space", false, false) {
        Ok(_) => panic!("an empty gravity_file must fail GMAT construction"),
        Err(e) => e,
    };
    match err {
        av_dynamics::ModelError::Gmat { model_id, .. } => assert_eq!(model_id, "gmat.test"),
        other => panic!("expected ModelError::Gmat, got {other:?}"),
    }
}
