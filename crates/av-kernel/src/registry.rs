//! `ModelRegistry`: `SystemDefinition.dynamics_model` -> constructor (ADR-005 sec 1, "A model
//! registry maps a model id from a `SystemDefinition.dynamics_model` to a constructor: native
//! models, GMAT models through `gmat-sys`, a remote `DynamicsService` stub (gRPC through the
//! ADR-003 front), and later FMUs.").
//!
//! **Dispatch.** [`kind_for`] classifies a `dynamics_model` id by prefix -- `"gmat."` ->
//! [`ModelKind::Gmat`], `"remote."` -> [`ModelKind::Remote`], anything else ->
//! [`ModelKind::Native`] -- the same convention `crate::drm::binding::classify_binding`
//! already used inline (a `"gmat."` prefix dispatched to a real `GmatModel`, everything else
//! to the native placeholder) before this module existed; `classify_binding` now calls
//! [`kind_for`] instead of repeating the prefix check itself, so this module is the one place
//! that decision lives.
//!
//! ## `ModelRegistry` is the sole constructor (M10.3, `docs/open-questions.md` question 98)
//!
//! Through M10.2, `crate::drm::executor` bypassed this module entirely: it called
//! `crate::drm::binding::materialize_gmat`/`materialize_constant_accel` directly, because
//! fault/maneuver re-binding and covariance seeding needed the unerased `binding::AnyModel`
//! shape a moment longer than `construct_native`/`construct_gmat`'s own `BoxedModel`-returning
//! signatures exposed. As of M10.3, [`ModelHandle`] is that "a moment longer" shape, made
//! opaque: it wraps `binding::AnyModel` privately and exposes exactly what a caller needs
//! without ever exposing the enum itself --
//!
//! - [`ModelHandle::t0_tai_ns`] / [`ModelHandle::x0_si`] / [`ModelHandle::settings`]: the same
//!   three pieces `binding::Materialized` always carried, still plain public fields (no
//!   `AnyModel` involved in reading them).
//! - [`ModelHandle::state_dim`] / [`ModelHandle::stm_capable`]: the two read-only facts
//!   `crate::drm::executor::run_covariance_instance` needs before it can decide whether (and
//!   how) to seed covariance for this instance.
//! - [`ModelHandle::into_boxed`] / [`ModelHandle::into_boxed_stm`]: erase to a plain, or
//!   `StmAugmented`-wrapped, [`BoxedModel`] -- the two shapes `crate::kernel::HeteroKernel`
//!   actually schedules. Consuming (`self`, not `&self`): a `ModelHandle` is erased exactly
//!   once, at the point a span/segment actually starts running, mirroring how `crate::drm::
//!   executor`'s own per-span loop already used `Materialized::model` exactly once before this
//!   task.
//!
//! **A fault or maneuver boundary "changes parameters" by constructing a fresh
//! [`ModelHandle`]**, not by mutating an existing one in place: `crate::drm::fault::
//! rebind_gmat_spec_at_state` (not owned by this task) already produces a *new*
//! `GmatSystemSpec` from the previous segment's own final physical state, and
//! [`ModelRegistry::construct_gmat`]/[`ModelRegistry::construct_native`] -- the same two
//! constructors used for an instance's very first segment -- are exactly what
//! `crate::drm::executor::materialize_plan_at_boundary` now calls again for a re-bound spec.
//! There is no separate "mutate this handle's parameters" method because there is nothing to
//! mutate: every construction, initial or re-bound, goes through the same two functions, and
//! every one of them returns a brand new [`ModelHandle`].
//!
//! **`crate::drm::binding::AnyModel` is `pub(crate)`, not `pub`, as of this task** (see that
//! module's own doc comment for exactly why `pub(crate)` -- not a stricter,
//! registry-module-only visibility -- is the finest Rust allows here): this module is the only
//! one that ever names it. `crate::drm::executor` no longer imports it at all -- every
//! construction, erasure, and STM-augmentation `crate::drm::executor` needs goes through this
//! module's public surface (`ModelRegistry`, [`ModelHandle`], [`ModelRegistry::stm_seed`])
//! instead.
//!
//! **Constructors.** [`ModelRegistry::construct_native`] and [`ModelRegistry::construct_gmat`]
//! wrap `crate::drm::binding::materialize_constant_accel`/`materialize_gmat` (the same,
//! already-tested construction logic the DRM executor's per-instance loop always used --
//! nothing about *how* a model is built changed here, only who is allowed to call the two
//! functions that build it) and erase the result to `av_dynamics::BoxedModel` (`Box<dyn
//! DynamicsModel<Error = ModelError>>`, ADR-005 sec 1) via `av_dynamics::erase_with_id`.
//! [`ModelRegistry::construct_remote`] is the third constructor ADR-005 sec 1 names -- see its
//! own doc comment for exactly what it does today.

use std::collections::BTreeMap;

use av_cdm::pb::{ModelInfo, PacketCodec, PortTrafficLog, StateSpace};
use av_dynamics::{erase_with_id, BoxedModel, DynamicsModel, ModelError, StmAugmented};
use gmat_sys::Gmat;

use crate::drm::attitude::AttitudeWheelsSpec;
use crate::drm::binding::{self, AnyModel, ConstantAccelSpec, GmatSystemSpec, Materialized};
use crate::drm::controller::AttitudeControllerSpec;
use crate::drm::ground::GroundStationSpec;
use crate::drm::replay::ReplayModel;
use crate::drm::sensors::{ImuSpec, StarTrackerSpec};
use crate::drm::DrmError;

/// Which of ADR-005 sec 1's constructors a `SystemDefinition.dynamics_model` id dispatches to.
/// See [`kind_for`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelKind {
    /// Anything not matched by the other prefixes -- today, the in-process `ConstantAccelModel`
    /// placeholder (`crate::drm::binding`'s own module doc comment: "a real model registry is
    /// future work"). A real native-model catalog, keyed by more than "not gmat, not remote, not
    /// attitude", is exactly that future work.
    Native,
    /// `"gmat."`-prefixed: a real `gmat_sys::model::GmatModel`.
    Gmat,
    /// `"remote."`-prefixed: a `DynamicsService` client model (ADR-003's gRPC front,
    /// `crates/av-grpc`/`crates/av-dynamics-service`). See
    /// [`ModelRegistry::construct_remote`]'s doc comment for what happens when one is actually
    /// requested.
    Remote,
    /// `"attitude."`-prefixed (M22.1b, `docs/open-questions.md` questions 151/152): a real
    /// `crate::drm::attitude::AttitudeWheelsModel`. See [`ModelRegistry::construct_attitude`].
    Attitude,
    /// `"startracker."`-prefixed (M22.2b, `docs/open-questions.md` questions 142/149/151/152): a
    /// real `crate::drm::sensors::StarTrackerModel`. See [`ModelRegistry::construct_star_tracker`].
    StarTracker,
    /// `"imu."`-prefixed (M22.2b): a real `crate::drm::sensors::ImuModel`. See
    /// [`ModelRegistry::construct_imu`].
    Imu,
    /// `"attctrl."`-prefixed (M22.4, `docs/sil-plan.md`'s M22 milestone paragraph: "A native
    /// 'controller' instance closes the loop first"): a real `crate::drm::controller::
    /// AttitudeControllerModel`. See [`ModelRegistry::construct_attitude_controller`].
    AttitudeController,
    /// `"ground."`-prefixed (M25.1, `docs/sil-plan.md`'s M25 milestone: "ground segment as a
    /// system"): a real `crate::drm::ground::GroundStationModel`. See [`ModelRegistry::
    /// construct_ground_station`].
    Ground,
}

/// Classify a `SystemDefinition.dynamics_model` id by its dispatch prefix (ADR-005 sec 1).
/// Pure string matching, no GMAT, no I/O -- safe to call from anywhere, including
/// `crate::drm::binding::classify_binding`, before any model is actually constructed.
pub fn kind_for(dynamics_model: &str) -> ModelKind {
    if dynamics_model.starts_with("gmat.") {
        ModelKind::Gmat
    } else if dynamics_model.starts_with("remote.") {
        ModelKind::Remote
    } else if dynamics_model.starts_with("attitude.") {
        ModelKind::Attitude
    } else if dynamics_model.starts_with("startracker.") {
        ModelKind::StarTracker
    } else if dynamics_model.starts_with("imu.") {
        ModelKind::Imu
    } else if dynamics_model.starts_with("attctrl.") {
        ModelKind::AttitudeController
    } else if dynamics_model.starts_with("ground.") {
        ModelKind::Ground
    } else {
        ModelKind::Native
    }
}

/// An opaque, constructed dynamics model: [`ModelRegistry::construct_native`]/
/// [`ModelRegistry::construct_gmat`]'s return value, and the one shape `crate::drm::executor`
/// carries between "this instance/segment was constructed" and "this instance/segment is now
/// erased and handed to `crate::kernel::HeteroKernel`". Wraps `crate::drm::binding::AnyModel`
/// privately (see the module doc comment's "`ModelRegistry` is the sole constructor" section)
/// -- nothing outside this module can name the wrapped type, only call the methods below.
pub struct ModelHandle {
    model: AnyModel,
    /// The instance's epoch, TAI nanoseconds. For a `"gmat."` plan, exactly the caller's own
    /// declared `epoch_tai_ns` -- never GMAT's own A1MJD read back (question 96; see
    /// `crate::drm::binding`'s module doc comment's "Epoch" section).
    pub t0_tai_ns: i64,
    /// The instance's initial physical state, SI (metres / metres-per-second). Always length 6
    /// for a `"gmat."` plan; for a `"native."` plan, the instance's own honoured width -- 0 or
    /// 6, never a fixed constant (M21.3, `docs/open-questions.md` question 141; see
    /// `crate::drm::binding::CONSTANT_ACCEL_STATE_DIM`'s own doc comment).
    pub x0_si: Vec<f64>,
    /// `BTreeMap` settings description used to build this model's `settings_hash` -- empty for
    /// the native path (`ConstantAccelModel::describe` supplies its own `ModelInfo` directly).
    pub settings: BTreeMap<String, String>,
}

impl ModelHandle {
    /// Dimension of the state vector the wrapped model's `derivatives`/`step` operate on --
    /// always 6 for a `"gmat."`-bound model; for a `"native."`-bound one, its own declared
    /// state space width (0 or 6, M21.3, question 141) (`crate::drm::executor::
    /// run_covariance_instance` reads this before seeding a `p0` of the matching size; the
    /// covariance path never reaches a native model at all -- `ModelNotStmCapable` refuses it
    /// first, regardless of dimension).
    pub fn state_dim(&self) -> usize {
        self.model.state_dim()
    }

    /// Whether the wrapped model can propagate its own state transition matrix
    /// (`av_dynamics::DynamicsModel::stm_capable`) -- checked before ever calling
    /// [`ModelHandle::into_boxed_stm`], exactly like every other declared-capability check in
    /// this platform's dynamics contract.
    pub fn stm_capable(&self) -> bool {
        self.model.stm_capable()
    }

    /// The wrapped model's own `ModelInfo` (id, state space, capabilities, settings hash).
    pub fn describe(&self) -> ModelInfo {
        self.model.describe()
    }

    /// `docs/open-questions.md` question 178 (R5.1a): the wrapped model's own accumulated SENSOR
    /// fault effect since the last drain (`av_dynamics::DynamicsModel::
    /// drain_sensor_fault_effect`) -- `None` for every binding kind except a `StarTrackerModel`
    /// with a fault currently installed. `crate::drm::executor::run_shared_group` calls this at
    /// every boundary this handle is about to be discarded and re-materialized.
    pub fn drain_sensor_fault_effect(&self) -> Option<av_dynamics::SensorFaultEffectDrain> {
        self.model.drain_sensor_fault_effect()
    }

    /// Erase to a plain [`BoxedModel`] for `crate::kernel::HeteroKernel::register_system` (the
    /// non-covariance path). `erase_id` becomes the id every [`ModelError`] this boxed model
    /// later produces (from a `derivatives`/`step` failure during the run) carries --
    /// `crate::drm::executor` passes its own instance name here, matching the labelling this
    /// crate used before M10.3 (distinct from the `model_id` a constructor was built with,
    /// which is normally `SystemDefinition.dynamics_model` itself).
    pub fn into_boxed(self, erase_id: &str) -> BoxedModel {
        match self.model {
            AnyModel::Gmat(inner) => erase_with_id(erase_id, inner, |id, e: gmat_sys::GmatError| ModelError::Gmat { model_id: id, detail: e.to_string() }),
            AnyModel::ConstantAccel(inner) => erase_with_id(erase_id, inner, |_id, never: std::convert::Infallible| match never {}),
            // M22.4: the wrapped model is `crate::drm::controller::CommandedAttitude<sensors::
            // TruthBroadcastAttitude<AttitudeWheelsModel>>` now (see `binding::AnyModel::
            // Attitude`'s own doc comment) -- `CommandedAttitudeError<Infallible>` is no longer
            // literally `Infallible` (its own `Codec` arm is genuinely reachable, from a
            // malformed inbound wheel-torque command packet), so this erasure closure maps it
            // into `ModelError::InvalidSpec` by `Display`, the same "stringify the model-
            // specific error" convention `ModelRegistry::construct_attitude`'s own error mapping
            // already uses -- never a `match never {}` that would no longer compile.
            AnyModel::Attitude(inner) => erase_with_id(erase_id, inner, |id, e: crate::drm::controller::CommandedAttitudeError<std::convert::Infallible>| ModelError::InvalidSpec { model_id: id, detail: e.to_string() }),
            // StarTrackerModel/ImuModel::Error is Infallible too (M22.2: every physical check
            // already ran at classify_binding/{StarTrackerModel,ImuModel}::new) -- same erasure
            // shape as ConstantAccel.
            AnyModel::StarTracker(inner) => erase_with_id(erase_id, inner, |_id, never: std::convert::Infallible| match never {}),
            AnyModel::Imu(inner) => erase_with_id(erase_id, inner, |_id, never: std::convert::Infallible| match never {}),
            // M22.4: `AttitudeControllerModel::Error = ControllerRuntimeError` -- genuinely
            // fallible at run time (a malformed inbound star tracker/IMU packet), same
            // `Display`-stringified mapping as the `Attitude` arm above.
            AnyModel::Controller(inner) => erase_with_id(erase_id, inner, |id, e: crate::drm::controller::ControllerRuntimeError| ModelError::InvalidSpec { model_id: id, detail: e.to_string() }),
            // M25.1: `GroundStationModel::Error` is `Infallible` too (every physical check
            // already ran at `classify_binding`/`GroundStationModel::new`) -- same erasure shape
            // as ConstantAccel/StarTracker/Imu.
            AnyModel::GroundStation(inner) => erase_with_id(erase_id, inner, |_id, never: std::convert::Infallible| match never {}),
            // M25.4b: `ReplayModel::Error` is `crate::drm::replay::ReplayError` -- genuinely
            // fallible at run time (the missing-frame refusal, `ReplayError::MissingFrame`) --
            // mapped into `ModelError::InvalidSpec` by `Display`, the same "stringify the
            // model-specific error" convention `AnyModel::Controller`/`AnyModel::Attitude`'s own
            // arms immediately above already use for their own genuinely-fallible wrapped
            // models. This is the erasure boundary that actually matters for the non-covariance
            // path every replay test in this crate drives (`crate::drm::executor::
            // run_shared_group`'s own construction sites both call `ModelHandle::into_boxed`,
            // never `into_boxed_stm` -- `RunConfig.replay` combined with `DrmOptions.covariance`
            // is refused before either boundary is reached, `DrmError::
            // ReplayWithCovarianceNotSupported`).
            AnyModel::Replay(inner) => erase_with_id(erase_id, inner, |id, e: crate::drm::replay::ReplayError| ModelError::InvalidSpec { model_id: id, detail: e.to_string() }),
        }
    }

    /// Erase to an `av_dynamics::StmAugmented`-wrapped [`BoxedModel`] for the covariance path
    /// (`crate::kernel::HeteroKernel::run_with_covariance`). Same `erase_id` convention as
    /// [`ModelHandle::into_boxed`].
    ///
    /// # Panics
    ///
    /// If [`ModelHandle::stm_capable`] is `false` (`StmAugmented::new`'s own documented
    /// precondition) -- callers must check `stm_capable()` first.
    pub fn into_boxed_stm(self, erase_id: &str) -> BoxedModel {
        erase_with_id(erase_id.to_string(), StmAugmented::new(self.model), |id, e: binding::AnyModelError| ModelError::Gmat { model_id: id, detail: e.to_string() })
    }
}

/// Map a `crate::drm::DrmError` produced by `binding::materialize_gmat` (only ever
/// `DrmError::Gmat` since question 96 deleted `DrmError::EpochMismatch` -- see that function's
/// own body) into the one [`ModelError`] this module hands back. Both cases (a GMAT FFI
/// failure, and formerly an epoch cross-check failure) originate at the same GMAT-FFI/A1MJD
/// boundary `gmat_sys::model::GmatModel` owns, so both were reported as `ModelError::Gmat`
/// here; the original `DrmError`'s own `Display` is preserved verbatim in `detail`, so nothing
/// about the distinction is lost, only its type.
fn gmat_materialize_err_to_model_error(model_id: &str, e: DrmError) -> ModelError {
    ModelError::Gmat { model_id: model_id.to_string(), detail: e.to_string() }
}

/// `SystemDefinition.dynamics_model` -> constructor (ADR-005 sec 1). A zero-sized type: every
/// method is an associated function rather than something reached through an instance, since
/// none of the three constructors need any registry-owned state (the GMAT engine handle and
/// the parsed parameters already carry everything each construction needs) -- there is nothing
/// to register beyond the fixed three kinds `kind_for` already classifies, matching the "and
/// later FMUs" phrasing in ADR-005 sec 1 as a fourth arm to add here, not a runtime
/// registration API to build out speculatively now.
pub struct ModelRegistry;

impl ModelRegistry {
    /// See [`kind_for`].
    pub fn kind_for(dynamics_model: &str) -> ModelKind {
        kind_for(dynamics_model)
    }

    /// Build the native placeholder model (`crate::drm::binding::materialize_constant_accel`,
    /// unchanged) and wrap it as a [`ModelHandle`]. `spec` is already parsed/validated (e.g.
    /// by `crate::drm::binding::classify_binding`); `model_id` is normally
    /// `SystemDefinition.dynamics_model` itself. Infallible construction (the native
    /// placeholder's own `DynamicsModel::Error` is `std::convert::Infallible`), so this
    /// returns the handle directly rather than a `Result`.
    pub fn construct_native(spec: &ConstantAccelSpec, epoch_tai_ns: i64, model_id: &str, state_space_id: &str) -> ModelHandle {
        let Materialized { model, t0_tai_ns, x0_si, settings } = binding::materialize_constant_accel(spec, epoch_tai_ns, model_id, state_space_id);
        ModelHandle { model, t0_tai_ns, x0_si, settings }
    }

    /// Build a real `crate::drm::attitude::AttitudeWheelsModel` (`crate::drm::binding::
    /// materialize_attitude`, M22.1b, `docs/open-questions.md` questions 151/152) and wrap it as
    /// a [`ModelHandle`]. `declared_state_space` is the instance's own resolved `StateSpace`
    /// (`crate::trajectory::resolve_state_space`) -- unlike `construct_gmat`'s `state_space_id`
    /// string, `AttitudeWheelsModel::new` needs the full declared shape (component count *and*
    /// each component's own unit, question 151) to run its own load-time checks, so this
    /// constructor takes the resolved `StateSpace` value itself rather than just its id.
    ///
    /// # Errors
    ///
    /// [`ModelError::InvalidSpec`] wrapping whatever `materialize_attitude` returned (a
    /// `crate::drm::attitude::AttitudeSpecError`, stringified) -- see
    /// [`ModelError::InvalidSpec`]'s own doc comment for why this is the general "native
    /// constructor's own load-time validation failed" variant, not a GMAT-specific one.
    /// `wheel_command_codec` (M22.4): `Some(codec)` when this instance also declares a wheel-
    /// torque-command FRAMED IN port (`crate::drm::controller::ATTITUDE_WHEEL_TORQUE_IN_PORT`) --
    /// see `binding::materialize_attitude`'s own doc comment for exactly what wrapping this adds
    /// (`crate::drm::controller::CommandedAttitude`) and why it is a strict no-op when `None`.
    pub fn construct_attitude(spec: &AttitudeWheelsSpec, declared_state_space: &StateSpace, wheel_command_codec: Option<PacketCodec>, epoch_tai_ns: i64, model_id: &str) -> Result<ModelHandle, ModelError> {
        let Materialized { model, t0_tai_ns, x0_si, settings } = binding::materialize_attitude(spec, declared_state_space, wheel_command_codec, epoch_tai_ns, model_id)
            .map_err(|e| ModelError::InvalidSpec { model_id: model_id.to_string(), detail: e.to_string() })?;
        Ok(ModelHandle { model, t0_tai_ns, x0_si, settings })
    }

    /// Build a real `crate::drm::sensors::StarTrackerModel` (`crate::drm::binding::
    /// materialize_star_tracker`, M22.2b, `docs/open-questions.md` questions 142/149/151/152) and
    /// wrap it as a [`ModelHandle`]. `codec`/`output_port` are the declared `PacketCodec`/FRAMED
    /// OUT port name `crate::drm::binding::resolve_sensor_output` resolved from this instance's
    /// own `SystemDefinition.packet_codecs`/`.ports` -- see that function's own doc comment for
    /// the structural convention (exactly one of each) it enforces.
    ///
    /// # Errors
    ///
    /// [`ModelError::InvalidSpec`] wrapping whatever `materialize_star_tracker` returned (a
    /// `crate::drm::sensors::SensorSpecError`, stringified) -- mirrors
    /// [`ModelRegistry::construct_attitude`]'s own error shape.
    pub fn construct_star_tracker(spec: &StarTrackerSpec, codec: PacketCodec, output_port: String, epoch_tai_ns: i64, model_id: &str) -> Result<ModelHandle, ModelError> {
        let Materialized { model, t0_tai_ns, x0_si, settings } =
            binding::materialize_star_tracker(spec, codec, output_port, epoch_tai_ns, model_id).map_err(|e| ModelError::InvalidSpec { model_id: model_id.to_string(), detail: e.to_string() })?;
        Ok(ModelHandle { model, t0_tai_ns, x0_si, settings })
    }

    /// The IMU counterpart of [`ModelRegistry::construct_star_tracker`] -- builds a real
    /// `crate::drm::sensors::ImuModel` (`crate::drm::binding::materialize_imu`).
    ///
    /// # Errors
    ///
    /// [`ModelError::InvalidSpec`] wrapping whatever `materialize_imu` returned.
    pub fn construct_imu(spec: &ImuSpec, codec: PacketCodec, output_port: String, epoch_tai_ns: i64, model_id: &str) -> Result<ModelHandle, ModelError> {
        let Materialized { model, t0_tai_ns, x0_si, settings } =
            binding::materialize_imu(spec, codec, output_port, epoch_tai_ns, model_id).map_err(|e| ModelError::InvalidSpec { model_id: model_id.to_string(), detail: e.to_string() })?;
        Ok(ModelHandle { model, t0_tai_ns, x0_si, settings })
    }

    /// Build a real `crate::drm::controller::AttitudeControllerModel` (M22.4, `crate::drm::
    /// binding::materialize_controller`) and wrap it as a [`ModelHandle`]. `star_codec`/
    /// `imu_codec`/`command_codec` are `crate::drm::binding::resolve_controller_ports`'s own
    /// resolution of this instance's declared `packet_codecs`/`.ports`.
    ///
    /// # Errors
    ///
    /// [`ModelError::InvalidSpec`] wrapping whatever `materialize_controller` returned (a
    /// `crate::drm::controller::ControllerSpecError`, stringified) -- mirrors
    /// [`ModelRegistry::construct_attitude`]'s own error shape.
    #[allow(clippy::too_many_arguments)]
    pub fn construct_attitude_controller(spec: &AttitudeControllerSpec, star_codec: PacketCodec, imu_codec: PacketCodec, command_codec: PacketCodec, epoch_tai_ns: i64, model_id: &str) -> Result<ModelHandle, ModelError> {
        let Materialized { model, t0_tai_ns, x0_si, settings } = binding::materialize_controller(spec, star_codec, imu_codec, command_codec, epoch_tai_ns, model_id)
            .map_err(|e| ModelError::InvalidSpec { model_id: model_id.to_string(), detail: e.to_string() })?;
        Ok(ModelHandle { model, t0_tai_ns, x0_si, settings })
    }

    /// Build a real `crate::drm::ground::GroundStationModel` (M25.1, `crate::drm::binding::
    /// materialize_ground_station`) and wrap it as a [`ModelHandle`]. `tm_codec`/`tm_port`/
    /// `tc_codec`/`tc_port` are `crate::drm::binding::resolve_ground_ports`'s own resolution of
    /// this instance's declared `packet_codecs`/`.ports`.
    ///
    /// # Errors
    ///
    /// [`ModelError::InvalidSpec`] wrapping whatever `materialize_ground_station` returned (a
    /// `crate::drm::ground::GroundSpecError`, stringified) -- mirrors [`ModelRegistry::
    /// construct_star_tracker`]'s own error shape.
    #[allow(clippy::too_many_arguments)]
    pub fn construct_ground_station(spec: &GroundStationSpec, tm_codec: PacketCodec, tm_port: String, tc_codec: PacketCodec, tc_port: String, epoch_tai_ns: i64, model_id: &str) -> Result<ModelHandle, ModelError> {
        let Materialized { model, t0_tai_ns, x0_si, settings } =
            binding::materialize_ground_station(spec, tm_codec, tm_port, tc_codec, tc_port, epoch_tai_ns, model_id).map_err(|e| ModelError::InvalidSpec { model_id: model_id.to_string(), detail: e.to_string() })?;
        Ok(ModelHandle { model, t0_tai_ns, x0_si, settings })
    }

    /// M25.4b (question 175's own follow-on): wrap an already-constructed [`ModelHandle`] --
    /// built exactly as a non-replayed run would build it, by whichever `construct_*` the
    /// instance's own `BindingPlan` dispatches to -- in a [`ReplayModel`], so its `step`/
    /// `step_with_ports` plays `log`'s own recorded frames back instead of running the real
    /// model. `handle.describe()`/`handle.state_dim()` are read BEFORE the wrap and carried
    /// into the replacement `ModelHandle` unchanged (`t0_tai_ns`/`x0_si`/`settings` too) --
    /// see `crate::drm::replay`'s own module doc comment's "Reconstructing the ORIGINAL model's
    /// own ModelInfo" section for exactly why this is what lets a replayed `Trajectory` come out
    /// byte-identical to the original run's: `TrajectorySegment.dynamics_model`/`.dynamics_hash`/
    /// `.dynamics_depth` are stamped straight from `ModelInfo.id`/`.settings_hash`/`.depth`
    /// (`crate::trajectory::build_trajectory`), and this function is what keeps those three
    /// values exactly what the real, un-replayed model would have reported.
    pub fn wrap_replay(handle: ModelHandle, instance: &str, log: &PortTrafficLog) -> ModelHandle {
        let info = handle.describe();
        let state_dim = handle.state_dim();
        let model = AnyModel::Replay(ReplayModel::new(instance, info, state_dim, log));
        ModelHandle { model, t0_tai_ns: handle.t0_tai_ns, x0_si: handle.x0_si, settings: handle.settings }
    }

    /// M25.4b: the container counterpart of [`ModelRegistry::wrap_replay`] -- built for an
    /// instance whose `SosConfiguration` binding is `BINDING_KIND_CONTAINER`, where there is no
    /// real [`ModelHandle`] to wrap AT ALL (a container's own `ModelInfo`/`binding_hash` come
    /// only from a live `Bind` RPC -- `crate::drm::binding::materialize_container` -- and the
    /// entire point of replaying a container instance is running it Docker-free, so this
    /// function never makes that call). `info` is therefore SYNTHETIC, built directly from the
    /// instance's own declared `SystemDefinition.dynamics_model`/`.state_space_id` rather than
    /// read off a real model -- a disclosed, unavoidable difference from the original recorded
    /// run's own container segment (`.settings_hash`/`.dynamics_hash` cannot be reconstructed
    /// without contacting the real process); see `crate::drm::executor::run_shared_group`'s own
    /// "replay" section for exactly where this is called and how a caller is expected to compare
    /// the resulting `Trajectory` (excluding that one segment field, explicitly). `state_dim` is
    /// always `0`: `BINDING_KIND_CONTAINER`'s own `ContainerModel::state_dim()` is always `0`
    /// too (this module's own doc comment's "Container (lockstep) binding" section), so this is
    /// not a replay-specific approximation, just the same fact every container instance already
    /// has.
    pub fn construct_replay_container(instance: &str, dynamics_model_id: &str, state_space_id: &str, epoch_tai_ns: i64, log: &PortTrafficLog) -> ModelHandle {
        let info = ModelInfo { id: dynamics_model_id.to_string(), version: "1".to_string(), state_space_id: state_space_id.to_string(), frame_id: String::new(), settings_hash: String::new(), depth: "container_replay".to_string(), ..Default::default() };
        let model = AnyModel::Replay(ReplayModel::new(instance, info, 0, log));
        ModelHandle { model, t0_tai_ns: epoch_tai_ns, x0_si: Vec::new(), settings: BTreeMap::new() }
    }

    /// Build a real `gmat_sys::model::GmatModel` (`crate::drm::binding::materialize_gmat`,
    /// unchanged -- see that function's own doc comment for exactly what it configures and,
    /// question 96, that its `t0_tai_ns` is now always the caller's own exact `epoch_tai_ns`)
    /// and wrap it as a [`ModelHandle`]. Requires a live `Gmat` handle held under
    /// `gmat_sys::engine_lock()`, exactly like `materialize_gmat` itself. `gmat_ns` (M18.4,
    /// question 127) is threaded straight through to `materialize_gmat`'s own `gmat_ns` parameter
    /// -- see that function's own doc comment for exactly what it namespaces and why it is a
    /// separate parameter from `name_suffix`, not a replacement for it.
    ///
    /// # Errors
    ///
    /// [`ModelError::Gmat`] wrapping whatever `materialize_gmat` returned (a GMAT FFI failure)
    /// -- see [`gmat_materialize_err_to_model_error`].
    #[allow(clippy::too_many_arguments)]
    pub fn construct_gmat(
        gmat: &Gmat,
        spec: &GmatSystemSpec,
        epoch_tai_ns: i64,
        model_id: &str,
        gmat_ns: &str,
        name_suffix: &str,
        state_space_id: &str,
        with_stm: bool,
        accept_missing_stm_terms: bool,
    ) -> Result<ModelHandle, ModelError> {
        let Materialized { model, t0_tai_ns, x0_si, settings } = binding::materialize_gmat(gmat, spec, epoch_tai_ns, gmat_ns, name_suffix, state_space_id, with_stm, accept_missing_stm_terms)
            .map_err(|e| gmat_materialize_err_to_model_error(model_id, e))?;
        Ok(ModelHandle { model, t0_tai_ns, x0_si, settings })
    }

    /// The remote `DynamicsService` constructor ADR-005 sec 1 names (a thin client over
    /// `crates/av-grpc`/`crates/av-dynamics-service`, ADR-003's gRPC front). **Not wired to a
    /// live client in this task**: `av-kernel` does not depend on `av-grpc` or
    /// `av-dynamics-service` today, and adding that dependency edge (tonic, its TLS stack,
    /// async runtime plumbing) to a crate three other things link is a bigger change than this
    /// task's scope justifies. Registered anyway -- `kind_for` recognizes `"remote."`-prefixed
    /// ids and this constructor is what a caller reaches for one -- so a `"remote."` dynamics
    /// model gets a typed, named refusal here rather than silently falling through to the
    /// native placeholder or an unhandled dispatch case.
    pub fn construct_remote(model_id: &str) -> Result<BoxedModel, ModelError> {
        Err(ModelError::BindingTransport {
            model_id: model_id.to_string(),
            detail: "remote DynamicsService client is not yet wired into av-kernel (ADR-005 sec 1); \
                     the constructor is registered so a \"remote.\"-dispatched dynamics_model gets a \
                     typed refusal here, not a live av-grpc/av-dynamics-service client"
                .to_string(),
        })
    }

    /// The augmented initial condition `[x0; vec(I)]` `crate::kernel::HeteroKernel::
    /// register_system` needs to seed a covariance-requested instance
    /// (`av_dynamics::StmAugmented::seed`). A free function on `ModelRegistry` rather than a
    /// bare one at module scope only so `crate::drm::executor` reaches every registry-owned
    /// piece of covariance plumbing through one name -- `StmAugmented::seed` itself does not
    /// depend on which `DynamicsModel` it will augment (only on `x0.len()`), so this needs no
    /// [`ModelHandle`] to call; it exists here so `crate::drm::executor` never has to name
    /// `AnyModel` just to supply `StmAugmented`'s own type parameter for the turbofish
    /// `StmAugmented::<AnyModel>::seed` would otherwise require.
    pub fn stm_seed(x0: &[f64]) -> Vec<f64> {
        StmAugmented::<AnyModel>::seed(x0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_for_classifies_by_prefix() {
        assert_eq!(kind_for("gmat.earth.jgm2_8x8"), ModelKind::Gmat);
        assert_eq!(kind_for("remote.dynamics_service.leo"), ModelKind::Remote);
        assert_eq!(kind_for("native.constant_accel"), ModelKind::Native);
        assert_eq!(kind_for("anything_else"), ModelKind::Native);
        // M22.1b (question 151/152): "attitude." dispatches to the new ModelKind, not Native --
        // fails against an implementation that never added the prefix check (every
        // "attitude.*" id would fall through to the trailing `else` and classify Native).
        assert_eq!(kind_for("attitude.wheels_demo"), ModelKind::Attitude);
        // M22.2b (question 142/149/151/152): "startracker."/"imu." dispatch to their own new
        // ModelKind variants, not Native -- fails against an implementation that never added
        // either prefix check (every "startracker.*"/"imu.*" id would fall through to the
        // trailing `else` and classify Native).
        assert_eq!(kind_for("startracker.sensors_demo"), ModelKind::StarTracker);
        assert_eq!(kind_for("imu.sensors_demo"), ModelKind::Imu);
        // M22.4: "attctrl." dispatches to the new ModelKind, not Native -- fails against an
        // implementation that never added the prefix check.
        assert_eq!(kind_for("attctrl.control_demo"), ModelKind::AttitudeController);
        // M25.1: "ground." dispatches to the new ModelKind, not Native -- fails against an
        // implementation that never added the prefix check (every "ground.*" id would fall
        // through to the trailing `else` and classify Native).
        assert_eq!(kind_for("ground.station_demo"), ModelKind::Ground);
    }

    /// M25.1: `construct_ground_station` builds a usable, zero-dimensional `ModelHandle` (a
    /// ground station declares no propagated physical state -- `GroundStationModel::state_dim()
    /// == 0`) -- fails against an implementation missing the constructor entirely (a compile
    /// error).
    #[test]
    fn construct_ground_station_builds_a_usable_zero_dimensional_handle() {
        let spec = crate::drm::ground::GroundStationSpec { body: "Earth".to_string(), latitude_rad: 0.5, longitude_rad: -1.4, height_m: 10.0, elevation_mask_rad: 0.1745 };
        let tm_codec = crate::drm::ground::ground_tm_packet_codec("gs_tm_test", 400);
        let tc_codec = crate::drm::ground::ground_tc_packet_codec("gs_tc_test", 401);
        let handle = ModelRegistry::construct_ground_station(&spec, tm_codec, "tm_in".to_string(), tc_codec, "tc_out".to_string(), 1_700_000_000_000_000_000, "ground.test").expect("a valid spec/codec pair must construct");
        assert_eq!(handle.t0_tai_ns, 1_700_000_000_000_000_000);
        assert_eq!(handle.state_dim(), 0);
        assert_eq!(handle.describe().id, "ground.test");
        assert!(!handle.stm_capable());
        assert_eq!(handle.x0_si, Vec::<f64>::new());
    }

    /// A declared telemetry codec missing a required field surfaces as a typed
    /// `ModelError::InvalidSpec`, not a panic -- mirrors `construct_star_tracker_returns_a_typed_
    /// error_for_a_codec_missing_a_required_field`'s own proof for the ground side.
    #[test]
    fn construct_ground_station_returns_a_typed_error_for_a_tm_codec_missing_a_required_field() {
        let spec = crate::drm::ground::GroundStationSpec { body: "Earth".to_string(), latitude_rad: 0.5, longitude_rad: -1.4, height_m: 10.0, elevation_mask_rad: 0.1745 };
        let mut tm_codec = crate::drm::ground::ground_tm_packet_codec("gs_tm_test", 400);
        tm_codec.fields.retain(|f| f.name != "z");
        let tc_codec = crate::drm::ground::ground_tc_packet_codec("gs_tc_test", 401);
        let err = match ModelRegistry::construct_ground_station(&spec, tm_codec, "tm_in".to_string(), tc_codec, "tc_out".to_string(), 0, "ground.test") {
            Ok(_) => panic!("a codec missing a required field must not construct"),
            Err(e) => e,
        };
        match err {
            ModelError::InvalidSpec { model_id, .. } => assert_eq!(model_id, "ground.test"),
            other => panic!("expected ModelError::InvalidSpec, got {other:?}"),
        }
    }

    /// M22.1b: `construct_attitude` builds a usable, correctly-dimensioned `ModelHandle` from a
    /// real spec/state-space pair -- fails against an implementation missing the constructor
    /// entirely (a compile error) or one that forgets to thread `declared_state_space` through
    /// to `AttitudeWheelsModel::new` (would panic or build the wrong dimension).
    #[test]
    fn construct_attitude_builds_a_usable_handle_matching_the_declared_state() {
        let spec = crate::drm::attitude::AttitudeWheelsSpec {
            inertia: [[10.0, 0.0, 0.0], [0.0, 10.0, 0.0], [0.0, 0.0, 10.0]],
            wheel_axes: vec![[1.0, 0.0, 0.0]],
            wheel_momentum_limits: vec![1.0],
            q0: [0.0, 0.0, 0.0, 1.0],
            omega0: [0.0, 0.02, 0.0],
            wheel_available: vec![true],
            wheel_commanded_torque: vec![0.0],
        };
        let space = crate::trajectory::attitude_wheels_state_space("test.attitude", 1);
        let handle = ModelRegistry::construct_attitude(&spec, &space, None, 1_700_000_000_000_000_000, "attitude.test").expect("a valid spec/state-space pair must construct");
        assert_eq!(handle.t0_tai_ns, 1_700_000_000_000_000_000);
        assert_eq!(handle.state_dim(), 8);
        assert_eq!(handle.describe().id, "attitude.test");
        assert!(!handle.stm_capable());
        assert_eq!(handle.x0_si, vec![0.0, 0.0, 0.0, 1.0, 0.0, 0.02, 0.0, 0.0]);
    }

    /// A dimension mismatch between `spec`/`declared_state_space` surfaces as a typed
    /// `ModelError::InvalidSpec`, not a panic -- the M22.1b brief's own "surface it as a typed
    /// DrmError at load, not a panic" requirement, at the constructor boundary this crate's own
    /// `executor.rs` maps straight into `DrmError::Model`.
    #[test]
    fn construct_attitude_returns_a_typed_error_for_a_mismatched_state_space() {
        let spec = crate::drm::attitude::AttitudeWheelsSpec {
            inertia: [[10.0, 0.0, 0.0], [0.0, 10.0, 0.0], [0.0, 0.0, 10.0]],
            wheel_axes: vec![[1.0, 0.0, 0.0]],
            wheel_momentum_limits: vec![1.0],
            q0: [0.0, 0.0, 0.0, 1.0],
            omega0: [0.0, 0.0, 0.0],
            wheel_available: vec![true],
            wheel_commanded_torque: vec![0.0],
        };
        let wrong_space = crate::trajectory::attitude_wheels_state_space("test.attitude", 0); // 0 wheels declared, spec has 1
        let err = match ModelRegistry::construct_attitude(&spec, &wrong_space, None, 0, "attitude.test") {
            Ok(_) => panic!("a mismatched state space must not construct"),
            Err(e) => e,
        };
        match err {
            ModelError::InvalidSpec { model_id, .. } => assert_eq!(model_id, "attitude.test"),
            other => panic!("expected ModelError::InvalidSpec, got {other:?}"),
        }
    }

    /// M22.2b: `construct_star_tracker` builds a usable, zero-dimensional `ModelHandle` (a star
    /// tracker declares no propagated physical state -- `StarTrackerModel::state_dim() == 0`) --
    /// fails against an implementation missing the constructor entirely (a compile error).
    #[test]
    fn construct_star_tracker_builds_a_usable_zero_dimensional_handle() {
        let spec = crate::drm::sensors::StarTrackerSpec { update_rate_hz: 2.0, seed: 1, noise_sigma_rad: 1e-5, mount_q: [0.0, 0.0, 0.0, 1.0], fault: None };
        let codec = crate::drm::sensors::star_tracker_packet_codec("st_test", 100);
        let handle = ModelRegistry::construct_star_tracker(&spec, codec, "st_meas".to_string(), 1_700_000_000_000_000_000, "startracker.test").expect("a valid spec/codec pair must construct");
        assert_eq!(handle.t0_tai_ns, 1_700_000_000_000_000_000);
        assert_eq!(handle.state_dim(), 0);
        assert_eq!(handle.describe().id, "startracker.test");
        assert!(!handle.stm_capable());
        assert_eq!(handle.x0_si, Vec::<f64>::new());
    }

    /// A declared codec missing a required field surfaces as a typed `ModelError::InvalidSpec`,
    /// not a panic -- mirrors `construct_attitude_returns_a_typed_error_for_a_mismatched_state_
    /// space`'s own proof for the attitude side.
    #[test]
    fn construct_star_tracker_returns_a_typed_error_for_a_codec_missing_a_required_field() {
        let spec = crate::drm::sensors::StarTrackerSpec { update_rate_hz: 2.0, seed: 1, noise_sigma_rad: 1e-5, mount_q: [0.0, 0.0, 0.0, 1.0], fault: None };
        let mut codec = crate::drm::sensors::star_tracker_packet_codec("st_test", 100);
        codec.fields.retain(|f| f.name != "qw");
        let err = match ModelRegistry::construct_star_tracker(&spec, codec, "st_meas".to_string(), 0, "startracker.test") {
            Ok(_) => panic!("a codec missing a required field must not construct"),
            Err(e) => e,
        };
        match err {
            ModelError::InvalidSpec { model_id, .. } => assert_eq!(model_id, "startracker.test"),
            other => panic!("expected ModelError::InvalidSpec, got {other:?}"),
        }
    }

    /// M22.2b: `construct_imu` builds a usable, six-dimensional `ModelHandle` (the bias
    /// random-walk state -- `ImuModel::state_dim() == 6`) with a zero initial bias.
    #[test]
    fn construct_imu_builds_a_usable_six_dimensional_handle_with_zero_initial_bias() {
        let spec = crate::drm::sensors::ImuSpec {
            update_rate_hz: 2.0,
            seed: 2,
            gyro_noise_sigma: 1e-4,
            gyro_bias_rw_sigma: 1e-6,
            accel_noise_sigma: 1e-3,
            accel_bias_rw_sigma: 1e-5,
            mount_q: [0.0, 0.0, 0.0, 1.0],
            true_specific_force: [0.0, 0.0, 0.0],
        };
        let codec = crate::drm::sensors::imu_packet_codec("imu_test", 101);
        let handle = ModelRegistry::construct_imu(&spec, codec, "imu_meas".to_string(), 1_700_000_000_000_000_000, "imu.test").expect("a valid spec/codec pair must construct");
        assert_eq!(handle.t0_tai_ns, 1_700_000_000_000_000_000);
        assert_eq!(handle.state_dim(), 6);
        assert_eq!(handle.describe().id, "imu.test");
        assert!(!handle.stm_capable());
        assert_eq!(handle.x0_si, vec![0.0; 6]);
    }

    #[test]
    fn construct_attitude_controller_builds_a_usable_zero_dimensional_handle() {
        let spec = crate::drm::controller::AttitudeControllerSpec { kp: 0.5, kd: 5.0, target_q: [0.0, 0.0, 0.0, 1.0], update_rate_hz: 2.0 };
        let star_codec = crate::drm::sensors::star_tracker_packet_codec("ctrl_star", 100);
        let imu_codec = crate::drm::sensors::imu_packet_codec("ctrl_imu", 101);
        let cmd_codec = crate::drm::controller::wheel_torque_command_packet_codec("ctrl_cmd", 102);
        let handle = ModelRegistry::construct_attitude_controller(&spec, star_codec, imu_codec, cmd_codec, 1_700_000_000_000_000_000, "attctrl.test").expect("a valid spec/codec triple must construct");
        assert_eq!(handle.t0_tai_ns, 1_700_000_000_000_000_000);
        assert_eq!(handle.state_dim(), 0);
        assert_eq!(handle.describe().id, "attctrl.test");
        assert!(!handle.stm_capable());
        assert_eq!(handle.x0_si, Vec::<f64>::new());
    }

    /// M22.4: `into_boxed` erasure preserves `AttitudeControllerModel::step_with_ports`'s own
    /// override end to end -- a real star tracker + IMU measurement packet in, a real
    /// wheel-torque command packet plus the `pointing_error_rad` marker output out -- through
    /// the exact `BoxedModel` boundary `av_dynamics::erase::ErasedModel::step_with_ports`'s own
    /// module doc comment calls "a defect class [that has] recurred three times" for a
    /// synthetic marker; this is the same proof against a real production model. Fails against
    /// an implementation that erases `AnyModel::Controller` through a closure/path that silently
    /// falls back to the trait's own default `step_with_ports` (empty `Outbox`, no `outputs`).
    #[test]
    fn into_boxed_erases_a_controller_and_preserves_its_step_with_ports_marker_outputs() {
        let spec = crate::drm::controller::AttitudeControllerSpec { kp: 0.5, kd: 5.0, target_q: [0.0, 0.0, 0.0, 1.0], update_rate_hz: 2.0 };
        let star_codec = crate::drm::sensors::star_tracker_packet_codec("ctrl_star", 100);
        let imu_codec = crate::drm::sensors::imu_packet_codec("ctrl_imu", 101);
        let cmd_codec = crate::drm::controller::wheel_torque_command_packet_codec("ctrl_cmd", 102);
        let handle = ModelRegistry::construct_attitude_controller(&spec, star_codec.clone(), imu_codec.clone(), cmd_codec, 0, "attctrl.test").expect("constructs");
        let model = handle.into_boxed("attctrl.test");

        let mut star_values = BTreeMap::new();
        star_values.insert("qx".to_string(), crate::codec::FieldValue::Numeric(0.0));
        star_values.insert("qy".to_string(), crate::codec::FieldValue::Numeric(0.0));
        star_values.insert("qz".to_string(), crate::codec::FieldValue::Numeric(0.1));
        star_values.insert("qw".to_string(), crate::codec::FieldValue::Numeric((1.0f64 - 0.01).sqrt()));
        let star_msg = av_dynamics::PortMessage { port: crate::drm::controller::CONTROLLER_STARTRACKER_IN_PORT.to_string(), tai_ns: 0, payload: crate::codec::encode_packet(&star_codec, 0, &[], &star_values).unwrap() };
        let mut imu_values = BTreeMap::new();
        for f in ["wx", "wy", "wz", "ax", "ay", "az"] {
            imu_values.insert(f.to_string(), crate::codec::FieldValue::Numeric(0.0));
        }
        let imu_msg = av_dynamics::PortMessage { port: crate::drm::controller::CONTROLLER_IMU_IN_PORT.to_string(), tai_ns: 0, payload: crate::codec::encode_packet(&imu_codec, 0, &[], &imu_values).unwrap() };
        let inbox = av_dynamics::Inbox::new(vec![star_msg, imu_msg]);

        let (result, outbox, _applied) = model.step_with_ports(&[], 0, &[], 1_000_000_000, &inbox).unwrap();
        assert!(result.outputs.contains_key("pointing_error_rad"), "the controller's own step_with_ports marker output must survive erasure -- the trait default would report an empty outputs map entirely");
        assert!(!outbox.messages().is_empty(), "the controller's own commanded wheel-torque packet must survive erasure -- the trait default would report an empty Outbox");
    }

    /// M22.4: the plant-side counterpart -- `controller::CommandedAttitude`'s decode-and-forward
    /// of an inbound wheel-torque command packet must survive erasure too, and actually change
    /// the propagated physics (not merely "the call does not panic"): a constant 2 N*m command
    /// on wheel 3 (z, aligned) integrated for exactly 1 s must add exactly 2.0 kg*m^2/s of stored
    /// momentum to that wheel (`dh/dt = tau_eff`, a constant derivative here since torque is
    /// held fixed for the whole step) -- a real, closed-form-checkable marker, not a synthetic
    /// sentinel value.
    #[test]
    fn into_boxed_erases_an_attitude_instance_and_a_wheel_torque_command_changes_the_propagated_wheel_momentum() {
        let spec = crate::drm::attitude::AttitudeWheelsSpec {
            inertia: [[10.0, 0.0, 0.0], [0.0, 10.0, 0.0], [0.0, 0.0, 10.0]],
            wheel_axes: vec![[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            wheel_momentum_limits: vec![100.0, 100.0, 100.0],
            q0: [0.0, 0.0, 0.0, 1.0],
            omega0: [0.0, 0.0, 0.0],
            wheel_available: vec![true, true, true],
            wheel_commanded_torque: vec![0.0, 0.0, 0.0],
        };
        let space = crate::trajectory::attitude_wheels_state_space("attctrl_plant_test.attitude", 3);
        let cmd_codec = crate::drm::controller::wheel_torque_command_packet_codec("plant_cmd", 200);
        let handle = ModelRegistry::construct_attitude(&spec, &space, Some(cmd_codec.clone()), 0, "attitude.plant_test").expect("constructs");
        let x0 = handle.x0_si.clone();
        let model = handle.into_boxed("attitude.plant_test");

        let mut values = BTreeMap::new();
        values.insert("tau_1".to_string(), crate::codec::FieldValue::Numeric(0.0));
        values.insert("tau_2".to_string(), crate::codec::FieldValue::Numeric(0.0));
        values.insert("tau_3".to_string(), crate::codec::FieldValue::Numeric(2.0));
        let msg = av_dynamics::PortMessage { port: crate::drm::controller::ATTITUDE_WHEEL_TORQUE_IN_PORT.to_string(), tai_ns: 0, payload: crate::codec::encode_packet(&cmd_codec, 0, &[], &values).unwrap() };
        let inbox = av_dynamics::Inbox::new(vec![msg]);

        let (result, _outbox, applied) = model.step_with_ports(&x0, 0, &[], 1_000_000_000, &inbox).unwrap();
        let wheel_z_h = result.state[crate::trajectory::ATTITUDE_WHEELS_BASE_COMPONENTS + 2];
        assert!((wheel_z_h - 2.0).abs() < 1e-6, "a constant 2 N*m command integrated for 1s must add 2.0 kg*m^2/s to the wheel's stored momentum; got {wheel_z_h}");
        assert_eq!(applied.len(), 1, "the decoded wheel-torque command must be reported as an AppliedCommand through the erased path too");
    }

    #[test]
    fn construct_native_builds_a_usable_boxed_model_matching_the_declared_state() {
        let spec = ConstantAccelSpec { a: [0.0, 0.0, -9.8], frame_id: "test.frame".to_string(), x0_si: vec![0.0, 0.0, 100.0, 1.0, 2.0, 0.0], ..Default::default() };
        let handle = ModelRegistry::construct_native(&spec, 1_700_000_000_000_000_000, "native.test", "test.space");
        assert_eq!(handle.t0_tai_ns, 1_700_000_000_000_000_000);
        assert_eq!(handle.x0_si, spec.x0_si);
        assert_eq!(handle.state_dim(), 6);
        assert_eq!(handle.describe().id, "native.test");
        assert!(!handle.stm_capable());

        let (t0, x0) = (handle.t0_tai_ns, handle.x0_si.clone());
        let model = handle.into_boxed("native.test");
        assert_eq!(model.describe().id, "native.test");

        // Drive it one step and check the closed-form constant-acceleration solution -- proves
        // the erased model is not just describable but actually steppable through the same
        // `DynamicsModel::step` default every other model in this workspace uses.
        let step = model.step(&x0, t0, &[], 1_000_000_000).unwrap(); // 1 s
        assert!((step.state[2] - (100.0 + 0.0 * 1.0 + 0.5 * -9.8 * 1.0)).abs() < 1e-9, "z(1s) = {}", step.state[2]);
        assert!((step.state[5] - (0.0 + -9.8 * 1.0)).abs() < 1e-9, "vz(1s) = {}", step.state[5]);
    }

    /// M21.3 (question 141): `construct_native` over a spec with no configured physical state
    /// (`x0_si` a genuinely empty `Vec`, `ConstantAccelSpec::default()`'s own shape) builds a
    /// `ModelHandle` reporting `state_dim() == 0`, not the pre-M21.3 fixed 6 -- fails against an
    /// implementation still hardcoding `CONSTANT_ACCEL_STATE_DIM` here instead of reading the
    /// spec's own configured width.
    #[test]
    fn construct_native_over_an_empty_spec_builds_a_zero_dimensional_handle() {
        let spec = ConstantAccelSpec { frame_id: "test.frame".to_string(), ..Default::default() };
        assert_eq!(spec.x0_si, Vec::<f64>::new(), "sanity: ConstantAccelSpec::default() configures no physical state");
        let handle = ModelRegistry::construct_native(&spec, 0, "native.empty_test", "test.empty");
        assert_eq!(handle.state_dim(), 0);
        assert_eq!(handle.x0_si, Vec::<f64>::new());
    }

    #[test]
    fn construct_remote_is_a_typed_refusal_naming_the_model_id() {
        // `unwrap_err()` would require `BoxedModel: Debug`, which a `dyn DynamicsModel` cannot
        // provide; match the Result instead.
        let err = match ModelRegistry::construct_remote("remote.dynamics_service.leo") {
            Ok(_) => panic!("construct_remote must not yet return a model"),
            Err(e) => e,
        };
        match err {
            ModelError::BindingTransport { model_id, .. } => assert_eq!(model_id, "remote.dynamics_service.leo"),
            other => panic!("expected ModelError::BindingTransport, got {other:?}"),
        }
    }

    #[test]
    fn stm_seed_matches_stm_augmented_seed_shape() {
        let x0 = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
        let seeded = ModelRegistry::stm_seed(&x0);
        assert_eq!(seeded.len(), 6 + 6 * 6);
        assert_eq!(&seeded[0..6], &x0);
        // Phi(t0, t0) = I.
        for i in 0..6 {
            for j in 0..6 {
                let want = if i == j { 1.0 } else { 0.0 };
                assert_eq!(seeded[6 + i * 6 + j], want, "Phi[{i}][{j}]");
            }
        }
    }
}
