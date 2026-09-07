//! M9.2 (`docs/open-questions.md` question 94): declared `SystemDefinition.state_space` end
//! to end -- the YAML authoring format (`crates/av-kernel/src/drm/schema.rs`'s
//! `RawStateSpace`), the built-in-registry-vs-declared-override resolution
//! (`crate::trajectory::resolve_state_space`), and the golden DRM's own declared state space.
//!
//! Pure Rust/YAML/protobuf -- no GMAT handle is ever taken in this file (parsing a YAML
//! `SystemDefinition` and resolving its state space touch no GMAT state), so unlike
//! `tests/drm_executor.rs`/`tests/golden_acceptance.rs` this file needs no
//! `gmat_sys::engine_lock()`.

use std::path::PathBuf;

use av_kernel::drm::{hash, schema, DrmError};
use av_kernel::interpolate::{classify, ComponentClass, InterpolationError};
use av_kernel::trajectory::{
    resolve_state_space, state_space_for, StateSpaceError, UnknownStateSpaceError, CARTESIAN_POS_VEL_6_ID, GMAT_ORBITAL_CARTESIAN6_ID, NATIVE_CONTROLLER_EMPTY_ID, NATIVE_CONTROLLER_SCALAR6_ID,
};

fn drms_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../drms").join(name)
}

// -----------------------------------------------------------------------------------------
// The golden DRM's own declared state space (drms/leo_1day_golden.system.yaml, M9.2).
// -----------------------------------------------------------------------------------------

/// The golden `SystemDefinition` now declares `state_space` explicitly (field-for-field
/// identical to what the built-in registry already returned for `gmat.orbital.cartesian6`
/// before this task -- see that YAML file's own comment). This is the "DRM YAMLs under
/// `drms/` declare their state spaces" requirement, checked against the *real* file the
/// executor loads, not a synthetic copy.
#[test]
fn golden_system_definition_declares_a_state_space_that_resolves_and_matches_the_registry() {
    let yaml = std::fs::read_to_string(drms_path("leo_1day_golden.system.yaml")).expect("golden system yaml readable");
    let sys = schema::parse_system_definition_yaml(&yaml).expect("golden SystemDefinition parses");

    // Declared, not merely resolvable by convention (ADR-001 / question 94).
    let declared = sys.state_space.clone().expect("golden SystemDefinition declares state_space");
    assert_eq!(declared.id, sys.state_space_id);
    assert_eq!(declared.id, GMAT_ORBITAL_CARTESIAN6_ID);

    let resolved = resolve_state_space(&sys).expect("golden state_space resolves");
    assert_eq!(resolved, declared);
    // The declaration is field-for-field the same shape the built-in registry already
    // declares for this id -- adding the field did not change what the id means, only made
    // it an artifact property instead of something that existed only by the registry's
    // convention.
    assert_eq!(resolved, state_space_for(GMAT_ORBITAL_CARTESIAN6_ID).unwrap());

    // And it classifies cleanly under ADR-005 sec 3, exactly as the built-in shape does.
    let groups = classify(&declared).expect("golden state_space classifies");
    assert_eq!(groups, vec![(0, 6, ComponentClass::PositionVelocity)]);
}

/// The golden `SystemDefinition`'s declared `hash` still matches its own canonical hash
/// after M9.2 added `state_space` -- the honesty requirement "if declaring state spaces in
/// the YAML changes its hash, recompute the hash properly" (never weaken the check itself).
#[test]
fn golden_system_definition_hash_was_recomputed_correctly_after_declaring_state_space() {
    let yaml = std::fs::read_to_string(drms_path("leo_1day_golden.system.yaml")).expect("golden system yaml readable");
    let sys = schema::parse_system_definition_yaml(&yaml).expect("golden SystemDefinition parses");
    let computed = hash::verify_system_hash(&sys.id, &sys).expect("golden SystemDefinition's declared hash matches its canonical hash");
    assert_eq!(computed.len(), 64);
}

// -----------------------------------------------------------------------------------------
// The built-in registry covers the three hardcoded ids, referenced by id (no override).
// -----------------------------------------------------------------------------------------

#[test]
fn every_built_in_id_resolves_through_resolve_state_space_with_no_override() {
    use av_cdm::pb::SystemDefinition;
    for id in [CARTESIAN_POS_VEL_6_ID, GMAT_ORBITAL_CARTESIAN6_ID, "altavista.cartesian_pos_vel_6_attitude_quat_4", NATIVE_CONTROLLER_SCALAR6_ID, NATIVE_CONTROLLER_EMPTY_ID] {
        let sys = SystemDefinition { state_space_id: id.to_string(), state_space: None, ..Default::default() };
        let resolved = resolve_state_space(&sys).unwrap_or_else(|e| panic!("built-in id {id:?} must resolve: {e}"));
        assert_eq!(resolved.id, id);
        assert_eq!(resolved, state_space_for(id).unwrap());
    }
}

// -----------------------------------------------------------------------------------------
// native.controller.scalar6 (M20.1, question 133): a non-physical native instance's own
// state space -- six independent, unitless scalars, deliberately NOT the Cartesian
// position/velocity shape, so it never carries a position class.
// -----------------------------------------------------------------------------------------

/// Fails against an implementation that reuses the Cartesian position/velocity shape for
/// this id (this task's own found defect, `drms/demo_two_instance_ctrl.system.yaml` before
/// M20.1 declared exactly that for a purely non-physical controller) or one that declares
/// six components under labels `classify` still happens to group as a position/velocity
/// prefix.
#[test]
fn native_controller_scalar6_resolves_to_six_scalars_with_no_position_class() {
    let space = state_space_for(NATIVE_CONTROLLER_SCALAR6_ID).expect("native.controller.scalar6 is a declared built-in id");
    assert_eq!(space.id, NATIVE_CONTROLLER_SCALAR6_ID);
    let labels: Vec<&str> = space.components.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(labels, vec!["state_1", "state_2", "state_3", "state_4", "state_5", "state_6"]);

    let groups = classify(&space).expect("six unitless scalars classify cleanly under ADR-005 sec 3");
    assert!(
        !groups.iter().any(|(_, _, class)| matches!(class, ComponentClass::PositionVelocity)),
        "a non-physical native controller's own state space must never classify with a position class; got {groups:?}"
    );
    assert_eq!(groups.len(), 6, "every one of the six components must be its own LinearScalar group, never merged into one PositionVelocity group");
    assert!(groups.iter().all(|(_, len, class)| *len == 1 && matches!(class, ComponentClass::LinearScalar)), "got {groups:?}");
}

/// The real committed `demo_ctrl` fixture (`drms/demo_two_instance_ctrl.system.yaml`)
/// declares exactly this shape, matching the built-in registry entry component-for-
/// component -- the same "DRM YAMLs under `drms/` declare their state spaces explicitly"
/// requirement `golden_system_definition_declares_a_state_space_that_resolves_and_matches_
/// the_registry` above already checks for the golden fixture, applied here to `demo_ctrl`.
///
/// **M21.3 (`docs/open-questions.md` question 141, decided by the lead, closing question
/// 133's own escalation): `demo_ctrl` now declares the EMPTY state space
/// (`NATIVE_CONTROLLER_EMPTY_ID`), not the six-scalar `NATIVE_CONTROLLER_SCALAR6_ID` M20.1
/// gave it.** `demo_ctrl` carries no physical state at all (it is a pure port controller),
/// so an empty declared state space is now the honest shape -- `crate::drm::binding::
/// ConstantAccelModel::state_dim()` takes its dimension from this declaration at
/// materialization (no longer a fixed constant), and this instance materializes at
/// dimension 0. Fails against a fixture that still declares six components (this task's own
/// point: six unitless-but-still-present scalars were themselves the M20.1-era compromise
/// forced by the old fixed-`state_dim` constant) or against an implementation that resolves
/// this id to anything other than zero components.
#[test]
fn demo_ctrl_system_definition_declares_the_empty_native_controller_state_space_with_no_position_class() {
    let yaml = std::fs::read_to_string(drms_path("demo_two_instance_ctrl.system.yaml")).expect("demo_ctrl system yaml readable");
    let sys = schema::parse_system_definition_yaml(&yaml).expect("demo_ctrl SystemDefinition parses");

    assert_eq!(sys.state_space_id, NATIVE_CONTROLLER_EMPTY_ID);
    let declared = sys.state_space.clone().expect("demo_ctrl declares its own state_space explicitly, not just the bare id");
    assert_eq!(declared.id, NATIVE_CONTROLLER_EMPTY_ID);
    assert!(declared.components.is_empty(), "demo_ctrl carries no physical state at all; got {:?}", declared.components);

    let resolved = resolve_state_space(&sys).expect("demo_ctrl's own declared state_space resolves");
    assert_eq!(resolved, declared);
    assert_eq!(resolved, state_space_for(NATIVE_CONTROLLER_EMPTY_ID).unwrap(), "demo_ctrl's own declaration must match the built-in registry entry component-for-component");

    let groups = classify(&declared).expect("demo_ctrl's own declared state_space classifies");
    assert!(groups.is_empty(), "zero components -> zero groups; got {groups:?}");
    assert!(
        !groups.iter().any(|(_, _, class)| matches!(class, ComponentClass::PositionVelocity)),
        "demo_ctrl (a non-physical native controller) must declare a state space with no position class; got {groups:?}"
    );
}

#[test]
fn an_id_naming_neither_a_built_in_space_nor_a_declared_override_is_refused() {
    use av_cdm::pb::SystemDefinition;
    let sys = SystemDefinition { state_space_id: "no.such.builtin".to_string(), state_space: None, ..Default::default() };
    let err = resolve_state_space(&sys).unwrap_err();
    assert!(matches!(err, StateSpaceError::Unknown(UnknownStateSpaceError(ref id)) if id == "no.such.builtin"), "{err}");
}

// -----------------------------------------------------------------------------------------
// A DRM author overriding a state space through YAML: id equality and ADR-005 sec 3
// classification, both enforced by resolve_state_space, neither by the YAML loader itself.
// -----------------------------------------------------------------------------------------

#[test]
fn a_yaml_authored_override_with_mismatched_id_is_refused() {
    let yaml = r#"
id: sys_bad_id
dynamics_model: native.test
state_space_id: mission.custom
state_space:
  id: mission.typo
  components:
    - label: pos_x
      unit: UNIT_METER
    - label: pos_y
      unit: UNIT_METER
    - label: pos_z
      unit: UNIT_METER
    - label: vel_x
      unit: UNIT_METER_PER_SECOND
    - label: vel_y
      unit: UNIT_METER_PER_SECOND
    - label: vel_z
      unit: UNIT_METER_PER_SECOND
hash: ""
"#;
    let sys = schema::parse_system_definition_yaml(yaml).expect("parses -- the loader does not check id equality");
    let err = resolve_state_space(&sys).unwrap_err();
    assert!(
        matches!(err, StateSpaceError::IdMismatch { ref state_space_id, ref declared_id } if state_space_id == "mission.custom" && declared_id == "mission.typo"),
        "{err}"
    );
}

#[test]
fn a_yaml_authored_override_with_an_unclassifiable_component_is_a_typed_refusal_not_a_silent_fallback() {
    let yaml = r#"
id: sys_bad_component
dynamics_model: native.test
state_space_id: mission.custom
state_space:
  id: mission.custom
  components:
    - label: pos_x
      unit: UNIT_METER
    - label: pos_y
      unit: UNIT_METER
    - label: pos_z
      unit: UNIT_METER
    - label: vel_x
      unit: UNIT_METER_PER_SECOND
    - label: vel_y
      unit: UNIT_METER_PER_SECOND
    - label: vel_z
      unit: UNIT_METER_PER_SECOND
    - label: mystery
      unit: UNIT_UNSPECIFIED
hash: ""
"#;
    let sys = schema::parse_system_definition_yaml(yaml).expect("parses -- the loader does not classify components");
    let err = resolve_state_space(&sys).unwrap_err();
    assert!(
        matches!(err, StateSpaceError::Unclassifiable(InterpolationError::UnclassifiableComponent { index: 6, ref label, .. }) if label == "mystery"),
        "{err}"
    );
}

#[test]
fn a_yaml_authored_override_can_redefine_a_built_in_id_and_it_is_accepted_verbatim() {
    // Question 94: "the Rust table becomes a registry of built-in spaces that a definition
    // may reference by id or override." A declaration under a built-in id is not checked
    // against that id's registry shape -- it replaces it for this artifact.
    let yaml = r#"
id: sys_override
dynamics_model: gmat.earth.jgm2_8x8.sun_moon
state_space_id: gmat.orbital.cartesian6
state_space:
  id: gmat.orbital.cartesian6
  components:
    - label: pos_x
      unit: UNIT_METER
    - label: pos_y
      unit: UNIT_METER
    - label: pos_z
      unit: UNIT_METER
    - label: vel_x
      unit: UNIT_METER_PER_SECOND
    - label: vel_y
      unit: UNIT_METER_PER_SECOND
    - label: vel_z
      unit: UNIT_METER_PER_SECOND
    - label: q_x
      unit: UNIT_DIMENSIONLESS
    - label: q_y
      unit: UNIT_DIMENSIONLESS
    - label: q_z
      unit: UNIT_DIMENSIONLESS
    - label: q_w
      unit: UNIT_DIMENSIONLESS
hash: ""
"#;
    let sys = schema::parse_system_definition_yaml(yaml).expect("parses");
    let resolved = resolve_state_space(&sys).expect("a redefinition of a built-in id is accepted, not compared against the registry");
    assert_eq!(resolved.components.len(), 10);
    assert_ne!(resolved, state_space_for(GMAT_ORBITAL_CARTESIAN6_ID).unwrap());
}

// -----------------------------------------------------------------------------------------
// The altavista round trip (M9.2 item 4): a StateSpace shaped exactly as
// altavista/cdm.py's `_cartesian_pos_vel_6` builder emits it (same id, same component
// labels/units/order -- pinned independently by tests/test_cdm_adapter.py's own
// `test_state_space_for_declares_the_6_component_cartesian_shape`, and by
// `tests/test_cdm_adapter.py::test_a_altavista_emitted_state_space_yaml_parses_under_the_
// rust_drm_loader`, which feeds the *actual* protobuf message altavista's `state_space_for`
// returns through the real Rust YAML loader via `cargo run --example drm_hash`) resolves
// through this crate's kernel-side validation and matches the built-in registry entry this
// crate already declares for the same id (`trajectory.rs`'s own doc comment: "a `StateSpace`
// produced on either side of the Rust/Python boundary for the same id is the same
// declaration, not two independently-invented ones").
// -----------------------------------------------------------------------------------------

#[test]
fn a_state_space_shaped_like_altavista_cdm_py_emits_it_round_trips_through_the_kernel() {
    let yaml = r#"
id: sys_from_altavista
dynamics_model: native.test
state_space_id: altavista.cartesian_pos_vel_6
state_space:
  id: altavista.cartesian_pos_vel_6
  components:
    - label: pos_x
      unit: UNIT_METER
    - label: pos_y
      unit: UNIT_METER
    - label: pos_z
      unit: UNIT_METER
    - label: vel_x
      unit: UNIT_METER_PER_SECOND
    - label: vel_y
      unit: UNIT_METER_PER_SECOND
    - label: vel_z
      unit: UNIT_METER_PER_SECOND
hash: ""
"#;
    let sys = schema::parse_system_definition_yaml(yaml).expect("an altavista-shaped SystemDefinition YAML parses");
    let resolved = resolve_state_space(&sys).expect("resolves and classifies");
    // Bit-for-bit the same declaration as this crate's own built-in registry entry for the
    // identical id -- proving a DRM authored from an altavista scenario (which would declare
    // exactly this StateSpace, per altavista/cdm.py::state_space_for) is not merely accepted,
    // but accepted as *the same* state space this crate already knows under that id.
    assert_eq!(resolved, state_space_for(CARTESIAN_POS_VEL_6_ID).unwrap());
    assert_eq!(resolved.id, CARTESIAN_POS_VEL_6_ID);
    let labels: Vec<&str> = resolved.components.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(labels, vec!["pos_x", "pos_y", "pos_z", "vel_x", "vel_y", "vel_z"]);

    // The hash the executor would check is computable and stable -- an altavista-authored DRM
    // can be hashed and pinned exactly like the golden fixture.
    let computed = hash::canonical_system_hash(&sys);
    assert_eq!(computed.len(), 64);
}

/// A malformed declaration (e.g. an author typo'd the id inside `state_space` but left
/// `state_space_id` alone) is refused with `DrmError`-free machinery -- `resolve_state_space`
/// returns its own `StateSpaceError`, not a silent pass-through. This double-checks that the
/// error survives being formatted (the executor's eventual `DrmError` wrapping displays the
/// inner reason, per the handoff notes in `trajectory.rs`).
#[test]
fn state_space_error_display_names_the_offending_ids() {
    let yaml = r#"
id: sys_typo
state_space_id: leo.orbit
state_space:
  id: leo.0rbit
  components: []
hash: ""
"#;
    let sys = schema::parse_system_definition_yaml(yaml).unwrap();
    let err = resolve_state_space(&sys).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("leo.orbit"), "{msg}");
    assert!(msg.contains("leo.0rbit"), "{msg}");
}

/// Sanity: `DrmError::UnknownStateSpace` (pre-M9.2) and `StateSpaceError::Unknown` (M9.2)
/// name the same offending id for the same input -- the handoff swaps which type wraps it,
/// not what information is available to report.
#[test]
fn drm_error_unknown_state_space_and_state_space_error_unknown_agree_on_the_id() {
    let bad_id = "not.a.real.space";
    let drm_err = DrmError::UnknownStateSpace { instance: "leo".to_string(), state_space_id: bad_id.to_string() };
    assert!(drm_err.to_string().contains(bad_id));
    let resolve_err = state_space_for(bad_id).unwrap_err();
    assert!(resolve_err.to_string().contains(bad_id));
}
