//! Assembling a CDM v1 `Trajectory` (`proto/altavista/v1/trajectory.proto`) from the
//! kernel's output samples.
//!
//! The kernel always declares `INTERPOLATION_HERMITE_VELOCITY` (`crate::interpolate`'s
//! contract) -- every sample the kernel emits already came from a state space whose first
//! six components are `[pos; vel]` (`av_dynamics::DynamicsModel`'s only state shape so far),
//! so this is never a guess about what a consumer *could* do with the samples, it's what
//! `crate::schedule::Scheduler::sample` actually used to produce them when it interpolated.

use av_cdm::pb::{Interpolation, ModelInfo, StateComponent, StateSpace, SystemDefinition, Trajectory, TrajectorySample, TrajectorySegment, Unit};

/// Build one `Trajectory` for one system's output samples over `[start_tai_ns,
/// end_tai_ns]`.
///
/// `id`/`entity_id` naming: the kernel skeleton does not yet own an entity catalog (that is
/// DRM/system-definition wiring, out of scope for M2.1), so `entity_id` is simply the
/// registered system id -- a placeholder until real entity resolution exists, not a claim
/// that the two concepts are the same thing. `provenance` and `config_hash` are left at their
/// proto defaults (unset / empty): populating them (spoore ADR-005 "evidence") is future
/// work, not attempted here.
pub fn build_trajectory(system_id: &str, model_info: &ModelInfo, samples: Vec<TrajectorySample>, start_tai_ns: i64, end_tai_ns: i64) -> Trajectory {
    Trajectory {
        id: format!("{system_id}-trajectory"),
        entity_id: system_id.to_string(),
        state_space_id: model_info.state_space_id.clone(),
        frame_id: model_info.frame_id.clone(),
        interpolation: Interpolation::HermiteVelocity as i32,
        samples,
        segments: vec![TrajectorySegment {
            name: system_id.to_string(),
            start_tai_ns,
            end_tai_ns,
            dynamics_model: model_info.id.clone(),
            dynamics_hash: model_info.settings_hash.clone(),
            dynamics_depth: model_info.depth.clone(),
        }],
        event_ids: vec![],
        label: None,
        provenance: None,
        config_hash: String::new(),
    }
}

// ---------------------------------------------------------------------------------------
// Declared StateSpace registry (ADR-005 sec 3, `docs/open-questions.md` question 88).
// ---------------------------------------------------------------------------------------
//
// The lead's condition (a) on accepting attitude in the state vector: "the state space must
// be a declared `StateSpace` message emitted with the trajectory (labels and units), not an
// ad hoc id string, because ADR-001 says nothing exists only by convention." A
// `Trajectory.state_space_id` (`trajectory.proto`) is a *reference*; this module is where the
// thing it references actually gets declared for every id this crate and its siblings
// (`altavista.cdm`, the DRM executor, the Rust dynamics service -- see this module's own
// `state_space_for` doc comment) produce. Two ids are canonical here, matching
// `altavista.cdm`'s `STATE_SPACE_ID_CARTESIAN_POS_VEL_6` / `_ATTITUDE_QUAT_4` builders
// component-for-component (label, order and unit all agree, checked by
// `crates/av-kernel/src/interpolate.rs`'s own test fixtures, which build the identical shape
// by hand) -- a `StateSpace` produced on either side of the Rust/Python boundary for the same
// id is the same declaration, not two independently-invented ones. `"gmat.orbital.cartesian6"`
// is accepted as a second, pre-existing id for exactly the same 6-component shape (used by
// `crates/av-kernel/src/drm/**`, `crates/gmat-sys` and this crate's own golden-acceptance
// tests before this task): it names the same physical concept (a Cartesian position/velocity
// state, SI units) under a different naming convention, not a different space.

/// A `StateSpace` id this module has no declared components for. ADR-005 sec 3's rule
/// applied one level up from component classification: an *id* this module cannot resolve at
/// all is refused rather than assumed to be "probably the usual 6-vector".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownStateSpaceError(pub String);

impl std::fmt::Display for UnknownStateSpaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "no declared StateSpace for id {:?}; known ids are {:?}",
            self.0,
            [CARTESIAN_POS_VEL_6_ID, CARTESIAN_POS_VEL_6_ATTITUDE_QUAT_4_ID, GMAT_ORBITAL_CARTESIAN6_ID, NATIVE_CONTROLLER_SCALAR6_ID, NATIVE_CONTROLLER_EMPTY_ID]
        )
    }
}
impl std::error::Error for UnknownStateSpaceError {}

/// Matches `altavista.cdm.STATE_SPACE_ID_CARTESIAN_POS_VEL_6` verbatim.
pub const CARTESIAN_POS_VEL_6_ID: &str = "altavista.cartesian_pos_vel_6";
/// Matches `altavista.cdm.STATE_SPACE_ID_CARTESIAN_POS_VEL_6_ATTITUDE_QUAT_4` verbatim.
pub const CARTESIAN_POS_VEL_6_ATTITUDE_QUAT_4_ID: &str = "altavista.cartesian_pos_vel_6_attitude_quat_4";
/// The pre-existing GMAT-side id for the same 6-component Cartesian shape (`drm/**`,
/// `gmat-sys`, this crate's golden-acceptance tests) -- see this module's doc comment.
pub const GMAT_ORBITAL_CARTESIAN6_ID: &str = "gmat.orbital.cartesian6";
/// M20.1 (`docs/open-questions.md` question 133, decided by the lead): a non-physical
/// native instance's own state space -- six independent, unitless scalars, never a
/// Cartesian position/velocity shape (no component here is named `pos_x`/`vel_x`/etc, so
/// [`crate::interpolate::classify`] never places any of them in a
/// [`crate::interpolate::ComponentClass::PositionVelocity`] group -- the "position class"
/// test a consumer like `altavista.cdm.has_position_class` reads off the resolved
/// [`StateSpace`] itself, never a hardcoded id string comparison). Six components, not
/// zero, because `crate::drm::binding::ConstantAccelModel`'s own physical `state_dim()` is
/// a fixed `crate::drm::binding::CONSTANT_ACCEL_STATE_DIM` (6) regardless of what a
/// fixture declares -- see that constant's own doc comment for why a declared dimension
/// must equal it. Matches `altavista.cdm.NATIVE_CONTROLLER_SCALAR6_ID`/
/// `_native_controller_scalar6` component-for-component (same labels, same order, same
/// unit), and `drms/demo_two_instance_ctrl.system.yaml`'s own declared `state_space`.
pub const NATIVE_CONTROLLER_SCALAR6_ID: &str = "native.controller.scalar6";
/// M21.3 (`docs/open-questions.md` question 141, decided by the lead, closing question 133's
/// own escalation): a non-physical native instance with NO state at all -- zero components,
/// never six unitless-but-still-present scalars. `crate::drm::binding::ConstantAccelModel`'s
/// own physical `state_dim()` is no longer a fixed constant (see `crate::drm::binding::
/// CONSTANT_ACCEL_STATE_DIM`'s own doc comment): the declared state space is authoritative at
/// materialization, and this id is the built-in registry entry for the width-0 case --
/// `drms/demo_two_instance_ctrl.system.yaml` (M21.3) declares exactly this id/shape for
/// `demo_ctrl`, replacing its own pre-M21.3 `NATIVE_CONTROLLER_SCALAR6_ID` declaration (that id
/// itself is not removed -- it stays available to any other non-physical native instance that
/// genuinely carries six independent scalars of telemetry).
pub const NATIVE_CONTROLLER_EMPTY_ID: &str = "native.controller.empty";

fn comp(label: &str, unit: Unit) -> StateComponent {
    StateComponent { label: label.to_string(), unit: unit as i32 }
}

fn cartesian_pos_vel_6(id: &str) -> StateSpace {
    StateSpace {
        id: id.to_string(),
        components: vec![
            comp("pos_x", Unit::Meter),
            comp("pos_y", Unit::Meter),
            comp("pos_z", Unit::Meter),
            comp("vel_x", Unit::MeterPerSecond),
            comp("vel_y", Unit::MeterPerSecond),
            comp("vel_z", Unit::MeterPerSecond),
        ],
        frame_id: String::new(),
    }
}

fn cartesian_pos_vel_6_attitude_quat_4(id: &str) -> StateSpace {
    let mut space = cartesian_pos_vel_6(id);
    space.components.extend([
        comp("q_x", Unit::Dimensionless),
        comp("q_y", Unit::Dimensionless),
        comp("q_z", Unit::Dimensionless),
        comp("q_w", Unit::Dimensionless),
    ]);
    space
}

/// M22.1 (`docs/sil-plan.md`'s M22 milestone, decision A; `docs/open-questions.md` questions
/// 88 and 142): the number of leading components [`attitude_wheels_state_space`] always
/// declares before any per-wheel momentum component -- quaternion (4) + body rate (3).
pub const ATTITUDE_WHEELS_BASE_COMPONENTS: usize = 7;

/// M22.1: the declared `StateSpace` for `crate::drm::attitude::AttitudeWheelsModel` --
/// attitude quaternion (scalar-last, matching [`CARTESIAN_POS_VEL_6_ATTITUDE_QUAT_4_ID`]'s own
/// `q_x, q_y, q_z, q_w` convention), body angular rate, and `n_wheels` reaction-wheel momenta.
///
/// **Not** one of the fixed built-in ids [`state_space_for`] resolves: every entry in that
/// table declares one fixed component count, but this shape's width is `n_wheels`-dependent (a
/// per-instance spacecraft parameter, never a platform-wide constant) -- question 94's inline
/// declaration path (`SystemDefinition.state_space`, [`resolve_state_space`]) is exactly the
/// mechanism for a shape a fixed id cannot carry, so a DRM using this model declares its own
/// `StateSpace` (built by this function with its own wheel count) rather than naming a
/// registry id. `crate::drm::attitude::AttitudeWheelsModel::new` takes its own physical
/// dimension from exactly this declared shape's component count
/// (`declared_state_space.components.len()`), the same "declared width is authoritative, never
/// hard-coded" rule M21.3 established for `ConstantAccelModel`
/// (`crate::drm::binding::CONSTANT_ACCEL_STATE_DIM`'s own doc comment) -- refusing, typed,
/// rather than guessing, when a spec's own wheel count disagrees with it.
///
/// **Unit (M22.1b, `docs/open-questions.md` question 151, decided by the lead).** M22.1 shipped
/// this component labelled [`Unit::NewtonMeter`] (torque, `N*m`) as a *documented approximation*
/// of wheel momentum's real dimension (`kg*m^2/s`) -- `core.proto` had no dedicated angular-
/// momentum unit at the time. The lead's decision adds one: [`Unit::NewtonMeterSecond`] (`=
/// 20`, `kg*m^2/s`) is what every `wheel_h_<n>` component declares now, and a component still
/// labelled with the torque unit is a typed load error, not an accepted alternative --
/// `crate::drm::attitude::AttitudeWheelsModel::new` refuses it (see that function's own doc
/// comment). Inertia parameters (`attitude.inertia.*`, `crate::drm::attitude::
/// parse_attitude_spec`) use the question's other new unit, [`Unit::KilogramMeterSquared`] (`=
/// 21`).
///
/// Classifies cleanly under `crate::interpolate::classify` (ADR-005 sec 3) for any `n_wheels`:
/// the leading 4 components match [`crate::interpolate`]'s own `q_x, q_y, q_z, q_w` /
/// `Unit::Dimensionless` quaternion-group recognizer exactly (-> `ComponentClass::Quaternion`),
/// and every remaining component (body rate, wheel momentum) carries a real, non-`Unspecified`
/// physical `Unit` and a label that is neither a `mode`/`count` discrete convention nor a
/// `phi_`/`stm_`/`cov_` never-interpolated one, so each classifies as `ComponentClass::
/// LinearScalar` -- exactly the class the M22.1 brief requires for wheel momentum, and (by the
/// same rule, not a special case) for body rate too.
pub fn attitude_wheels_state_space(id: &str, n_wheels: usize) -> StateSpace {
    let mut components = vec![
        comp("q_x", Unit::Dimensionless),
        comp("q_y", Unit::Dimensionless),
        comp("q_z", Unit::Dimensionless),
        comp("q_w", Unit::Dimensionless),
        comp("body_rate_x", Unit::RadianPerSecond),
        comp("body_rate_y", Unit::RadianPerSecond),
        comp("body_rate_z", Unit::RadianPerSecond),
    ];
    debug_assert_eq!(components.len(), ATTITUDE_WHEELS_BASE_COMPONENTS);
    for i in 0..n_wheels {
        components.push(comp(&format!("wheel_h_{}", i + 1), Unit::NewtonMeterSecond));
    }
    StateSpace { id: id.to_string(), components, frame_id: String::new() }
}

fn native_controller_scalar6(id: &str) -> StateSpace {
    StateSpace {
        id: id.to_string(),
        components: (1..=6).map(|i| comp(&format!("state_{i}"), Unit::Dimensionless)).collect(),
        frame_id: String::new(),
    }
}

/// M21.3 (question 141): zero components -- `crate::interpolate::classify` on this returns an
/// empty group list (never an error: the classifier's own loop simply never executes), so this
/// resolves and classifies exactly as cleanly as every other declared state space here, just
/// with nothing to interpolate.
fn native_controller_empty(id: &str) -> StateSpace {
    StateSpace { id: id.to_string(), components: vec![], frame_id: String::new() }
}

/// The declared `StateSpace` for `state_space_id` -- labels and units for every component,
/// per ADR-005 sec 3 / question 88's condition (a). Every `Trajectory` this crate (or a
/// sibling that has taken this handoff -- the DRM executor, `av-dynamics-service`) produces
/// should have its `state_space_id` resolvable here; an id that is not is a typed
/// [`UnknownStateSpaceError`], never a guess at what shape the caller probably meant.
///
/// `frame_id` is deliberately left unset on every returned `StateSpace`: the shapes this
/// function declares (a Cartesian position/velocity space, with or without a trailing
/// attitude quaternion) are frame-independent -- the same declared shape is reused across
/// many different `Trajectory.frame_id`s -- so filling it here with whichever frame happened
/// to call this function would be a guess, not a property of the state space itself.
pub fn state_space_for(state_space_id: &str) -> Result<StateSpace, UnknownStateSpaceError> {
    match state_space_id {
        CARTESIAN_POS_VEL_6_ID | GMAT_ORBITAL_CARTESIAN6_ID => Ok(cartesian_pos_vel_6(state_space_id)),
        CARTESIAN_POS_VEL_6_ATTITUDE_QUAT_4_ID => Ok(cartesian_pos_vel_6_attitude_quat_4(state_space_id)),
        NATIVE_CONTROLLER_SCALAR6_ID => Ok(native_controller_scalar6(state_space_id)),
        NATIVE_CONTROLLER_EMPTY_ID => Ok(native_controller_empty(state_space_id)),
        other => Err(UnknownStateSpaceError(other.to_string())),
    }
}

// ---------------------------------------------------------------------------------------
// Question 94: a `SystemDefinition` may declare its own `StateSpace` (field 15), additive
// to and authoritative over `state_space_id` (field 9). `docs/open-questions.md` question
// 94, the lead's decision, quoted in `system.proto`'s own field comment: "When present it
// is authoritative and its `id` must equal `state_space_id`; when absent the id must name a
// built-in space the kernel's registry knows. Nothing exists only by convention." This
// module's built-in registry ([`state_space_for`], three ids) is exactly that fallback; a
// declared `state_space` does not extend or get checked against the registry entry for the
// same id (a declaration is free to redefine what an id means for this one artifact) -- the
// registry is consulted only when nothing was declared at all.
// ---------------------------------------------------------------------------------------

/// Everything that can go wrong resolving a `SystemDefinition`'s *effective* `StateSpace`
/// (question 94): the built-in registry when `state_space` is unset
/// ([`UnknownStateSpaceError`]), or, when it is set, the two checks question 94 requires of a
/// declared space before it may be trusted -- `id` equality with `state_space_id`, and every
/// component classifying under ADR-005 sec 3 (`crate::interpolate::classify`'s own error,
/// wrapped rather than duplicated).
#[derive(Debug, Clone, PartialEq)]
pub enum StateSpaceError {
    /// `state_space` was unset and `state_space_id` named no space [`state_space_for`]'s
    /// built-in registry declares.
    Unknown(UnknownStateSpaceError),
    /// `state_space` was present but its own `id` did not equal `state_space_id` -- question
    /// 94: "its `id` must equal `state_space_id`". Both are named so a caller can report
    /// exactly what disagreed, since either one could be the typo.
    IdMismatch { state_space_id: String, declared_id: String },
    /// `state_space` was present, its `id` matched, but at least one component could not be
    /// classified under ADR-005 sec 3 -- refused rather than accepted with an interpolation
    /// contract nothing downstream could honour at sample time.
    Unclassifiable(crate::interpolate::InterpolationError),
}

impl std::fmt::Display for StateSpaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StateSpaceError::Unknown(e) => write!(f, "{e}"),
            StateSpaceError::IdMismatch { state_space_id, declared_id } => write!(
                f,
                "SystemDefinition.state_space_id is {state_space_id:?} but the declared state_space.id is {declared_id:?}; question 94 requires them to be equal"
            ),
            StateSpaceError::Unclassifiable(e) => write!(f, "declared state_space is not usable: {e}"),
        }
    }
}
impl std::error::Error for StateSpaceError {}

/// The effective `StateSpace` for `system` (question 94, `docs/open-questions.md`): its own
/// declared `state_space` when present -- checked for `id` equality against `state_space_id`
/// and for every component classifying per ADR-005 sec 3, then returned verbatim as the
/// authoritative declaration -- or, when `state_space` is unset, the built-in registry entry
/// named by `state_space_id` ([`state_space_for`]).
///
/// This is the one function that answers "how do the built-in registry and an overriding
/// declared `state_space` interact": they never merge. A present `state_space` is
/// authoritative on its own terms (it may even redefine what its `id` means for this one
/// artifact, e.g. adding a component the registry's own builder for that id does not); the
/// registry is consulted only when `state_space` is absent entirely.
///
/// The executor (`crate::drm::executor`, a sibling module this crate does not own this
/// round) is expected to call this once per instance, in place of its current direct call to
/// [`state_space_for`], and to map [`StateSpaceError`] into its own `DrmError` (see this
/// crate's handoff notes) rather than only checking `state_space_id` against the registry.
pub fn resolve_state_space(system: &SystemDefinition) -> Result<StateSpace, StateSpaceError> {
    match &system.state_space {
        Some(declared) => {
            if declared.id != system.state_space_id {
                return Err(StateSpaceError::IdMismatch { state_space_id: system.state_space_id.clone(), declared_id: declared.id.clone() });
            }
            crate::interpolate::classify(declared).map_err(StateSpaceError::Unclassifiable)?;
            Ok(declared.clone())
        }
        None => state_space_for(&system.state_space_id).map_err(StateSpaceError::Unknown),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_a_trajectory_with_the_hermite_velocity_contract_declared() {
        let info = ModelInfo {
            id: "test.model".to_string(),
            version: "1".to_string(),
            state_space_id: "test.space".to_string(),
            frame_id: "test.frame".to_string(),
            settings_hash: "deadbeef".to_string(),
            depth: "native".to_string(),
            ..Default::default()
        };
        let samples = vec![
            TrajectorySample { tai_ns: 0, mean: vec![0.0; 6], ..Default::default() },
            TrajectorySample { tai_ns: 100, mean: vec![1.0; 6], ..Default::default() },
        ];
        let traj = build_trajectory("sat-1", &info, samples.clone(), 0, 100);
        assert_eq!(traj.interpolation, Interpolation::HermiteVelocity as i32);
        assert_eq!(traj.entity_id, "sat-1");
        assert_eq!(traj.state_space_id, "test.space");
        assert_eq!(traj.frame_id, "test.frame");
        assert_eq!(traj.samples, samples);
        assert_eq!(traj.segments.len(), 1);
        assert_eq!(traj.segments[0].dynamics_model, "test.model");
        assert_eq!(traj.segments[0].dynamics_hash, "deadbeef");
        assert_eq!(traj.segments[0].dynamics_depth, "native");
        assert_eq!(traj.segments[0].start_tai_ns, 0);
        assert_eq!(traj.segments[0].end_tai_ns, 100);
    }

    #[test]
    fn state_space_for_declares_the_6_component_cartesian_shape() {
        for id in [CARTESIAN_POS_VEL_6_ID, GMAT_ORBITAL_CARTESIAN6_ID] {
            let space = state_space_for(id).unwrap();
            assert_eq!(space.id, id);
            let labels: Vec<&str> = space.components.iter().map(|c| c.label.as_str()).collect();
            assert_eq!(labels, vec!["pos_x", "pos_y", "pos_z", "vel_x", "vel_y", "vel_z"]);
            let units: Vec<Unit> = space.components.iter().map(|c| Unit::try_from(c.unit).unwrap()).collect();
            assert_eq!(units, vec![Unit::Meter, Unit::Meter, Unit::Meter, Unit::MeterPerSecond, Unit::MeterPerSecond, Unit::MeterPerSecond]);
        }
    }

    #[test]
    fn state_space_for_declares_the_10_component_attitude_shape_and_classifies_cleanly() {
        let space = state_space_for(CARTESIAN_POS_VEL_6_ATTITUDE_QUAT_4_ID).unwrap();
        assert_eq!(space.components.len(), 10);
        let labels: Vec<&str> = space.components.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, vec!["pos_x", "pos_y", "pos_z", "vel_x", "vel_y", "vel_z", "q_x", "q_y", "q_z", "q_w"]);
        // The declared shape must itself classify cleanly under crate::interpolate's rules
        // (ADR-005 sec 3) -- a StateSpace this module declares that its own sibling module
        // could not classify would be an internal inconsistency, not just untested.
        let groups = crate::interpolate::classify(&space).expect("declared shape must classify");
        assert_eq!(groups, vec![(0, 6, crate::interpolate::ComponentClass::PositionVelocity), (6, 4, crate::interpolate::ComponentClass::Quaternion)]);
    }

    /// M21.3 (question 141): the built-in zero-component registry entry resolves, classifies
    /// cleanly (an empty group list, not an error), and round-trips through `resolve_state_space`
    /// exactly like every other registry entry -- proving the "declared width" story holds for
    /// the empty case specifically, not merely for 6.
    #[test]
    fn native_controller_empty_resolves_to_zero_components_and_classifies_cleanly() {
        let space = state_space_for(NATIVE_CONTROLLER_EMPTY_ID).expect("native.controller.empty is a declared built-in id");
        assert_eq!(space.id, NATIVE_CONTROLLER_EMPTY_ID);
        assert!(space.components.is_empty(), "got {:?}", space.components);
        let groups = crate::interpolate::classify(&space).expect("zero components classifies cleanly -- an empty group list, never an error");
        assert!(groups.is_empty());

        let system = sys(NATIVE_CONTROLLER_EMPTY_ID, None);
        assert_eq!(resolve_state_space(&system).unwrap(), space);
    }

    // -----------------------------------------------------------------------------------
    // attitude_wheels_state_space (M22.1, questions 88/142).
    // -----------------------------------------------------------------------------------

    #[test]
    fn attitude_wheels_state_space_declares_labels_and_units_for_every_component_and_classifies_cleanly() {
        for n_wheels in [0usize, 1, 3] {
            let space = attitude_wheels_state_space("test.attitude", n_wheels);
            assert_eq!(space.id, "test.attitude");
            assert_eq!(space.components.len(), ATTITUDE_WHEELS_BASE_COMPONENTS + n_wheels);
            let labels: Vec<&str> = space.components.iter().map(|c| c.label.as_str()).collect();
            let mut want_labels = vec!["q_x", "q_y", "q_z", "q_w", "body_rate_x", "body_rate_y", "body_rate_z"];
            let wheel_labels: Vec<String> = (1..=n_wheels).map(|i| format!("wheel_h_{i}")).collect();
            want_labels.extend(wheel_labels.iter().map(String::as_str));
            assert_eq!(labels, want_labels);

            let units: Vec<Unit> = space.components.iter().map(|c| Unit::try_from(c.unit).unwrap()).collect();
            let mut want_units = vec![Unit::Dimensionless, Unit::Dimensionless, Unit::Dimensionless, Unit::Dimensionless, Unit::RadianPerSecond, Unit::RadianPerSecond, Unit::RadianPerSecond];
            want_units.extend(std::iter::repeat_n(Unit::NewtonMeterSecond, n_wheels));
            assert_eq!(units, want_units);

            // Every declared component here must classify cleanly (ADR-005 sec 3) -- an
            // internal inconsistency between this builder and crate::interpolate's rules would
            // otherwise only surface as a runtime InterpolationError far from this declaration.
            let groups = crate::interpolate::classify(&space).expect("declared attitude+wheels shape must classify");
            let mut want_groups = vec![(0, 4, crate::interpolate::ComponentClass::Quaternion)];
            want_groups.extend((0..3).map(|k| (4 + k, 1, crate::interpolate::ComponentClass::LinearScalar)));
            want_groups.extend((0..n_wheels).map(|k| (7 + k, 1, crate::interpolate::ComponentClass::LinearScalar)));
            assert_eq!(groups, want_groups);
        }
    }

    #[test]
    fn state_space_for_refuses_an_unknown_id_rather_than_guess() {
        let err = state_space_for("no.such.space").unwrap_err();
        assert_eq!(err.0, "no.such.space");
        assert!(err.to_string().contains("no.such.space"));
    }

    // -----------------------------------------------------------------------------------
    // resolve_state_space (question 94): registry fallback vs. an overriding declaration.
    // -----------------------------------------------------------------------------------

    fn sys(state_space_id: &str, state_space: Option<StateSpace>) -> SystemDefinition {
        SystemDefinition { state_space_id: state_space_id.to_string(), state_space, ..Default::default() }
    }

    #[test]
    fn resolve_state_space_falls_back_to_the_registry_when_absent() {
        let system = sys(GMAT_ORBITAL_CARTESIAN6_ID, None);
        let resolved = resolve_state_space(&system).unwrap();
        assert_eq!(resolved, state_space_for(GMAT_ORBITAL_CARTESIAN6_ID).unwrap());
    }

    #[test]
    fn resolve_state_space_refuses_an_unknown_id_when_nothing_is_declared() {
        let system = sys("no.such.space", None);
        let err = resolve_state_space(&system).unwrap_err();
        assert!(matches!(err, StateSpaceError::Unknown(UnknownStateSpaceError(id)) if id == "no.such.space"));
    }

    #[test]
    fn resolve_state_space_accepts_a_declared_override_matching_the_id() {
        let declared = cartesian_pos_vel_6_attitude_quat_4("mission.custom_10");
        let system = sys("mission.custom_10", Some(declared.clone()));
        let resolved = resolve_state_space(&system).unwrap();
        assert_eq!(resolved, declared);
    }

    #[test]
    fn resolve_state_space_a_declared_override_is_never_checked_against_the_registrys_own_shape_for_the_same_id() {
        // Redefining what a built-in id means for this one artifact is allowed -- the
        // registry's own idea of GMAT_ORBITAL_CARTESIAN6_ID (six components) is not
        // consulted at all once a state_space is declared, so a ten-component override
        // under that same id is accepted, not rejected as "disagrees with the registry".
        let declared = cartesian_pos_vel_6_attitude_quat_4(GMAT_ORBITAL_CARTESIAN6_ID);
        let system = sys(GMAT_ORBITAL_CARTESIAN6_ID, Some(declared.clone()));
        let resolved = resolve_state_space(&system).unwrap();
        assert_eq!(resolved, declared);
        assert_ne!(resolved, state_space_for(GMAT_ORBITAL_CARTESIAN6_ID).unwrap());
    }

    #[test]
    fn resolve_state_space_refuses_an_id_mismatch_between_state_space_id_and_the_declared_id() {
        let declared = cartesian_pos_vel_6("some.other.id");
        let system = sys("mission.custom", Some(declared));
        let err = resolve_state_space(&system).unwrap_err();
        assert!(matches!(
            err,
            StateSpaceError::IdMismatch { ref state_space_id, ref declared_id }
                if state_space_id == "mission.custom" && declared_id == "some.other.id"
        ), "{err}");
        assert!(err.to_string().contains("mission.custom"));
        assert!(err.to_string().contains("some.other.id"));
    }

    #[test]
    fn resolve_state_space_refuses_a_declared_component_the_interpolator_cannot_classify() {
        let mut declared = cartesian_pos_vel_6("mission.bad");
        declared.components.push(comp("mystery", Unit::Unspecified));
        let system = sys("mission.bad", Some(declared));
        let err = resolve_state_space(&system).unwrap_err();
        assert!(matches!(err, StateSpaceError::Unclassifiable(crate::interpolate::InterpolationError::UnclassifiableComponent { index: 6, .. })), "{err}");
    }
}
