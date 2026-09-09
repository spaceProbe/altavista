//! Binding a `SystemInstance` (`proto/altavista/v1/system.proto`) to an
//! [`av_dynamics::DynamicsModel`] (question 87's "bind every `BINDING_KIND_MODEL` instance").
//!
//! **Dispatch.** `SystemDefinition.dynamics_model` names a registered dynamics model id
//! (ADR-002's `ModelInfo.id`); `crate::registry::kind_for` (ADR-005 sec 1) is the one place
//! that decides which of the registry's kinds an id names -- `"gmat."` -> a real
//! [`gmat_sys::model::GmatModel`] (via `gmat-sys`, ADR-002 depth 2); `"remote."` -> a
//! `DynamicsService` client, refused here today (see `crate::registry::ModelRegistry::
//! construct_remote`'s doc comment for why); `"attitude."` (M22.1b, `docs/open-questions.md`
//! questions 151/152) -> a real [`crate::drm::attitude::AttitudeWheelsModel`], the native
//! rigid-body-with-wheels `DynamicsModel` M22.1 built but did not yet wire past this dispatch --
//! see [`parse_attitude_spec`][crate::drm::attitude::parse_attitude_spec] and this module's own
//! `classify_binding`; anything else -> [`ConstantAccelModel`], the same closed-form
//! constant-acceleration `DynamicsModel` `av-kernel`'s own unit tests already use.
//!
//! **`ModelRegistry` is the sole constructor (M10.3, `docs/open-questions.md` question 98).**
//! This module still owns *materializing* a plan (`materialize_gmat`/`materialize_constant_accel`
//! below), but as of M10.3 those two functions and [`AnyModel`] itself are `pub(crate)`, not
//! `pub`: `crate::registry::ModelRegistry` is the only caller (both for a fresh instance and for
//! a fault/maneuver re-binding), and `crate::drm::executor` -- the DRM executor's per-instance
//! run loop -- goes through `ModelRegistry`'s opaque `ModelHandle` exclusively, never naming
//! `AnyModel` itself. This is the practical limit of "private to the registry" Rust's own
//! visibility rules allow for a type *defined* in this sibling module (`pub(in
//! crate::registry)` is not expressible here -- that attribute only restricts to an *ancestor*
//! of the defining module, and `binding` and `registry` are siblings under `drm`/the crate
//! root, not each other's ancestor): `pub(crate)` still lets another module *within*
//! `av-kernel` reach in if it chose to, but nothing does, and no external crate can (`AnyModel`
//! is no longer part of this crate's public API at all, since `drm::binding`'s own `pub mod` in
//! `src/lib.rs` used to make it reachable as `av_kernel::drm::binding::AnyModel`). See
//! `crate::registry`'s own module doc comment for `ModelHandle`'s shape and exactly what it
//! exposes without ever exposing `AnyModel`.
//!
//! **Non-model bindings are refused, never silently skipped, except `BINDING_KIND_CONTAINER`
//! as of M13.2** (`docs/open-questions.md` question 107): `BINDING_KIND_RENODE` /
//! `BINDING_KIND_BOARD` (and an unset/`_UNSPECIFIED` binding) still return
//! [`DrmError::UnsupportedBinding`] from [`classify_binding`] -- ADR-005's Renode/board
//! runtimes are still Planned, and this crate has no machinery for either. A
//! `BINDING_KIND_CONTAINER` instance is now classified into [`BindingPlan::Container`] and,
//! at run time, `crate::drm::executor`'s dedicated `run_container_instance` speaks
//! `altavista.v1.LockstepService` (`av_lockstep`, `crates/av-lockstep`) to an
//! **already-running** process at a declared address -- see this module's own "Container
//! (lockstep) binding" section below and `crates/av-kernel/README.md`'s "Out of scope"
//! section for exactly what M13.2 does and does not build (container image lifecycle --
//! pulling by digest, starting the process -- is not this batch's job at all).
//!
//! ## Container (lockstep) binding (M13.2, question 107)
//!
//! [`parse_container_spec`] reads a `"container."`-prefixed parameter vocabulary (the same
//! "`SystemDefinition.parameters` is the established extension point" pattern
//! `"force_model."`/`"spacecraft."`/`"output."` already use, chosen because
//! `proto/altavista/v1/system.proto`'s own `ContainerBinding` message (`image`,
//! `image_digest`, `command`, `port_endpoints`, `lockstep_capable`) is shaped around the
//! image-lifecycle path this batch does not build -- `port_endpoints` is documented as "Port
//! name -> transport endpoint," not a place to name the lockstep *control* channel itself,
//! and `ContainerBinding.lockstep_capable` is a pre-declared expectation this module
//! deliberately never trusts on its own: every bind still asks the live process via a real
//! `Bind` RPC and only ever believes *that* response's `lockstep_capable`/`refusal_reason`):
//!
//! - `container.address` (string, required) -- `"host:port"` the kernel connects to. Already
//!   running; this module never starts, pulls, or health-checks an image.
//! - `container.tls` (`value != 0.0`, default `false`) -- mTLS through `av_grpc::tls`
//!   (system OpenSSL, never `ring`) when set; plaintext loopback (h2c, no TLS stack at all)
//!   otherwise -- **the task brief's "plaintext loopback allowed only for tests"**, so a real
//!   deployed container binding is expected to always set this.
//! - `container.ca_file` / `container.client_cert` / `container.client_key` (string,
//!   required together iff `container.tls` is set) -- PEM paths, same shape as
//!   `av_grpc::tls::MtlsConfig`.
//! - `container.seed_key` (string, required) -- names an entry in `Scenario.seeds`
//!   (ADR-004 "seeds are inputs," the same pattern a maneuver's `execution_error.seed` and a
//!   PORT/SENSOR fault's seed already use) supplying `LockstepBindRequest.seed`. Resolved by
//!   `crate::drm::executor` (which has `Scenario` in scope), not here -- [`ContainerSpec`]
//!   only carries the *name*; see [`DrmError::UnknownContainerSeed`].
//!
//! [`ContainerModel`] (this module) is the `av_dynamics::DynamicsModel` that actually speaks
//! the protocol once bound: `state_dim() == 0` (a container-bound instance in this batch
//! carries **no** physical ODE state -- it is a pure port/named-output process; see this
//! module's own doc comment on [`ContainerModel`] and `crates/av-kernel/README.md` for what
//! that does and does not let a container-bound `Trajectory` express), and
//! [`ContainerModel::step_with_ports`] is the one place that sends `Step`, checks
//! `response.sequence`/`reached_tai_ns` against what was sent (a mismatch is
//! [`ContainerError::SequenceMismatch`]/[`ContainerError::ReachedTaiMismatch`], both fatal --
//! "the run stops," per `lockstep.proto`'s own doc comment), and converts the response's
//! `outputs`/`named_outputs` into an `Outbox`/`StepResult::outputs`.
//!
//! **Not wired to `crate::router::Router` in this batch.** `ContainerModel::step_with_ports`
//! accepts an `Inbox` and forwards it verbatim as `LockstepStepRequest.inputs` (so a future
//! caller that *does* deliver a real, router-populated `Inbox` gets correct behaviour for
//! free), but `crate::drm::executor::run_container_instance` -- the only caller in this batch
//! -- always passes an empty `Inbox`: every instance in a `SosConfiguration` still runs on
//! its own isolated per-instance loop (the same architecture `run_plain_instance`/`run_span`
//! already use for GMAT/native instances), never on one shared `HeteroKernel`/`Router`
//! spanning multiple instances at once, so there is no live sender to actually deliver a
//! cross-instance message from in this batch. Wiring real inter-instance delivery for a
//! `BINDING_KIND_CONTAINER` instance would need every instance in one `SosConfiguration` to
//! be registered on one shared `HeteroKernel::run_with_ports` call instead of `execute()`'s
//! current per-instance loop -- a materially bigger scheduling change, out of this task's
//! scope, and disclosed here rather than silently claimed to work.
//!
//! **Parameter vocabulary.** A `"gmat."`-dispatched `SystemDefinition`'s `parameters` (plus
//! the binding `SystemInstance`'s own `parameter_overrides`, applied on top by name -- both
//! are fields of a hashed message, never a profile, so this is squarely inside question 11's
//! rule) are read under these prefixes:
//! - `force_model.<field>`: `central_body`, `gravity_file`, `gravity_degree`, `gravity_order`
//!   (`string_value`/`value` as appropriate), `point_masses` (`string_value`, comma-separated
//!   -- the same flattening `tests/golden_acceptance.rs` already uses for this crate's own
//!   settings hash), `relativistic_correction` (`value != 0.0`), `golden_ref` (`string_value`,
//!   optional -- becomes `GmatModelInfo.goldens`).
//! - `spacecraft.<GmatFieldName>`: forwarded verbatim to `Object::set_real`/`set_str` (whether
//!   `string_value` is non-empty selects which) -- e.g. `spacecraft.SMA`, `spacecraft.ECC`,
//!   `spacecraft.CoordinateSystem`, `spacecraft.DisplayStateType`, and question 81's five
//!   ballistic fields (`spacecraft.DryMass`/`Cd`/`Cr`/`DragArea`/`SRPArea`) -- one uniform
//!   mechanism for orbital elements and ballistics alike, rather than a hardcoded field list.
//!   `spacecraft.CoordinateSystem`'s value is also `ModelInfo.frame_id`.
//! - `output.<name>` (question 95's second half, M10.2/M10.3): declares that this instance
//!   exposes `output.<instance>.<name>@time` against one of `gmat_sys::model::GmatModel::
//!   step`'s named `StepResult.outputs` (`crate::drm::executor::declared_outputs` reads the
//!   real, unmodified `SystemDefinition` to build that declaration). [`parse_gmat_spec`] simply
//!   skips these entries -- they name no `GmatSystemSpec` field at all -- rather than refusing
//!   them as unknown. **M10.3 change:** through M10.2 this allowlist only knew
//!   `"force_model."`/`"spacecraft."`, so `crate::drm::executor` had to hand it a filtered copy
//!   of `sys` with every `"output.*"` parameter stripped first
//!   (`strip_output_parameters`/`OUTPUT_PARAMETER_PREFIX`, that module's own escalated
//!   workaround). Teaching the allowlist about `"output."` directly here removes the need for
//!   that filtering pass entirely -- `classify_binding` now sees the real `SystemDefinition`
//!   unconditionally, one fewer moving part between the declared, hashed shape and what this
//!   module actually validates.
//! - `port.emit` / `port.emit_output`, `port.consume` / `port.consume_parameter` (M18.3,
//!   question 126): SIGNAL port participation, closing the refusal every `"port.*"` parameter
//!   used to hit unconditionally (see `parse_gmat_spec`'s own doc comment for the exact pairing
//!   rules and [`GMAT_WRITABLE_PARAMETERS`] for the consume-side allowlist).
//! - `force_model.drag_model` / `force_model.drag_historic_weather_source` /
//!   `force_model.drag_predicted_weather_source` / `force_model.drag_cssi_space_weather_file`
//!   (M19.4, `docs/open-questions.md` question 131): atmospheric drag, all four required
//!   together (see [`GmatSystemSpec::drag_model`]'s own doc comment) -- `None` (the pre-M19.4
//!   shape) means no drag at all. Threaded into a real `DragForce` + atmosphere-model object
//!   pair by [`materialize_gmat`].
//!
//! A parameter name matching none of the above (an unrecognized `force_model.`/`spacecraft.`/
//! `port.` field name on a `"gmat."` system, or not in `{"accel.x","accel.y","accel.z",
//! "frame_id", "state.px","state.py","state.pz","state.vx","state.vy","state.vz","port.emit",
//! "port.emit_value","port.consume","condition.threshold_m","condition.mode"}` on a native one)
//! is a typed [`DrmError::UnknownParameter`] -- never silently ignored (question 87's whole
//! point). The native `"condition.*"` pair (M19.4, question 131) is [`ConstantAccelSpec::
//! condition`]'s own doc comment.
//!
//! **Epoch (`docs/open-questions.md` question 96).** GMAT's own epoch is set via `DateFormat =
//! "A1ModJulian"` and `Epoch = av_cdm::time::Tai::from_nanos(scenario.start_tai_ns).to_a1_mjd()`
//! formatted as a string with Rust's own round-trip-exact `f64` `Display` (`Object::set_str`,
//! *not* `set_real` -- empirically, GMAT's `Epoch` field takes a string even under the numeric
//! `A1ModJulian` format; `set_real("Epoch", ...)` is refused with "Epoch expects a String
//! value, but the received value is a real number"). Confirmed to reproduce this repository's
//! `UTCGregorian`-string convention bit-for-bit for this golden's own epoch (both give
//! `x = [5950.56620450214, ...]`, identical to 1e-9).
//!
//! **The instance's epoch stays TAI nanoseconds end to end; A1MJD is used only for the GMAT
//! call, never converted back.** Through M9.3, [`materialize_gmat`] read the bound model's own
//! `epoch_tai_ns()` (`gmat_sys::model::GmatModel::epoch_tai_ns`, which converts GMAT's internal
//! `A1MJD` *back* to TAI ns) and used *that* round-tripped value -- not the declared
//! `Scenario.start_tai_ns` -- as `Materialized::t0_tai_ns`, cross-checked against the declared
//! value to a tolerance (`EPOCH_CROSSCHECK_TOLERANCE_NS`) sized for the round trip's own
//! documented residual (question 81 measured up to 252 ns for realistic epoch magnitudes;
//! `av_cdm::time::Tai`'s own test bounds the general case at one microsecond). That kept every
//! sample epoch downstream self-consistent with GMAT's own internal time reference, but meant
//! the kernel's sample epochs were never quite the exact integers `Scenario.start_tai_ns`
//! declared -- and forced M9.3's own `output.*` GMAT-bound test to assert `@end`, not `@start`,
//! to dodge the residual at the very first sample (`tests/drm_executor.rs::
//! output_speed_resolves_against_a_real_gmat_bound_instance`, now restored to `@start`).
//!
//! **Decided by the lead, question 96:** [`materialize_gmat`] no longer calls `epoch_tai_ns()`
//! at all. `Materialized::t0_tai_ns` is simply the caller's own `epoch_tai_ns` parameter,
//! unchanged -- an exact integer, always. `EPOCH_CROSSCHECK_TOLERANCE_NS` and
//! `DrmError::EpochMismatch` are deleted along with it: nothing in this module converts A1MJD
//! back to TAI ns any more, so there is no round-trip residual left to tolerate or to check
//! against. This does **not** eliminate the underlying imprecision A1MJD's own `f64`
//! representation carries at these magnitudes (question 81) -- it moves where that imprecision
//! shows up. GMAT's own internal time reference for a bound `GmatModel` is fixed at
//! construction from the *same* `a1mjd` string this module sends it, and everything this
//! module or its callers do afterward computes new absolute epochs by adding exact-integer TAI
//! nanosecond offsets to `t0_tai_ns` -- so the two clocks (av-kernel's exact-integer TAI ns,
//! and GMAT's own fixed `A1MJD` reference) drift apart by a bounded, one-time offset no larger
//! than the same residual the old cross-check used to bound (up to ~252 ns, never
//! re-introduced or compounded by a second round trip). Concretely: the physical state GMAT
//! computes for a sample av-kernel labels `t0_tai_ns + k * period_ns` is the state at the true
//! instant that offset away from GMAT's *own* epoch reference, which can differ from
//! `t0_tai_ns + k * period_ns` by that same bounded residual -- at LEO orbital speeds
//! (~7.5 km/s), a worst-case 252 ns residual is a position label/physical-instant disagreement
//! on the order of a millimetre, well inside every golden's own tolerance. **Measured, not
//! merely estimated:** `tests/drm_executor.rs::drm_matches_the_golden_arc` (the golden's own
//! `tolerance_m = 0.05`, `tolerance_mps = 5e-5`) recorded `|dr| = 0.0001 m`, `|dv| = 8.155e-8
//! m/s` before this task (`docs/teamlog/2026-09-02-team-1.md`); after this change, the same
//! test measures `|dr| = 0.0001 m` (unchanged to the precision printed), `|dv| = 7.837e-8 m/s`
//! -- a real, small, disclosed change (not a coincidence of rounding: the underlying velocity
//! residual moved by a few percent, exactly the size question 96's own bounded offset predicts),
//! still comfortably inside the golden's own pinned tolerance. This is an accepted, documented
//! approximation inherent to A1MJD's own `f64` precision at these epoch magnitudes (ADR-001
//! "Alternatives considered"), not something this module can eliminate without GMAT itself
//! gaining an integer-nanosecond epoch representation -- what M10.3 removes is only the
//! *additional*, avoidable error a second (backward) round trip through the same lossy
//! representation was adding on top of it.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::fmt;
use std::rc::Rc;

use av_cdm::pb::{Binding, BindingKind, ContainerBinding, DrmOptions, ModelCapability, ModelInfo, PacketCodec, PacketField, Parameter, Port, PortDirection, PortKind, StateSpace, SystemDefinition, SystemInstance};
use av_cdm::time::Tai;
use av_dynamics::{AppliedCommand, DynamicsModel, Inbox, Outbox, StepResult, StmStepResult};
use av_lockstep::docker::ManagedContainer;
use av_lockstep::{BlockingLockstepClient, LockstepBindRequest, LockstepStepRequest};
use gmat_sys::model::{GmatModel, GmatModelInfo};
use gmat_sys::{Gmat, GmatError};

use super::attitude::{self, AttitudeSpecError, AttitudeWheelsModel, AttitudeWheelsSpec};
use super::controller::{self, AttitudeControllerModel, AttitudeControllerSpec, CommandedAttitude, ControllerSpecError};
use super::fault::{CARTESIAN_FIELDS, DISPLAY_STATE_TYPE_FIELD, KEPLERIAN_FIELDS};
use super::gmat_command::{self, FramedAck, FramedCommandInput, GmatFramedCommandModel};
use super::ground::{self, GroundSpecError, GroundStationModel, GroundStationSpec};
use super::replay::ReplayModel;
use super::sensors::{self, ImuModel, ImuSpec, SensorSpecError, StarTrackerModel, StarTrackerSpec, TruthBroadcastAttitude};
use super::DrmError;

/// GMAT version every space-system binding in this module is pinned against -- this
/// repository ships exactly one GMAT install (`GMAT R2026a/`), the same constant
/// `tests/golden_acceptance.rs` records literally in its own `GmatModelInfo`.
const GMAT_VERSION: &str = "R2026a";

// --------------------------------------------------------------------------------------
// AnyModel: the heterogeneous DynamicsModel this executor actually schedules
// --------------------------------------------------------------------------------------

/// A GMAT-backed space system, or the constant-acceleration placeholder for "anything else"
/// -- see the module doc comment. `av_kernel::schedule::Scheduler<M>` is homogeneous over one
/// `M` (its own documented scope note), so a DRM run that binds several instances of
/// different kinds needs one Rust type spanning both; this enum is it.
///
/// `pub(crate)` (M10.3, question 98): only `crate::registry::ModelRegistry` names this type --
/// see the module doc comment's "`ModelRegistry` is the sole constructor" section for why this
/// is the finest visibility Rust allows for a type defined here rather than in `registry.rs`
/// itself, and what actually enforces "the executor never touches `AnyModel`" (the code no
/// longer referencing it, not the visibility alone).
pub(crate) enum AnyModel {
    /// M25.2b: `gmat_command::GmatFramedCommandModel`, not a bare `GmatModel`, as of this task
    /// -- see that type's own module doc comment. Every other `AnyModel::Gmat(m) => m.foo()`
    /// match arm in this file needed no change: the wrapper delegates every `DynamicsModel`
    /// method explicitly (question 112), and its `Error` associated type is `gmat_sys::
    /// GmatError`, identical to the type `GmatModel` itself used here before this task -- so
    /// `AnyModelError::Gmat`/`registry::ModelHandle::into_boxed`'s own erasure closure (`|id, e:
    /// gmat_sys::GmatError| ...`) also needed no change.
    Gmat(gmat_command::GmatFramedCommandModel),
    ConstantAccel(ConstantAccelModel),
    /// M22.1b (`docs/open-questions.md` questions 151/152): a real
    /// `crate::drm::attitude::AttitudeWheelsModel`, dispatched via `"attitude."` (see
    /// `crate::registry::kind_for`) -- wrapped in `sensors::TruthBroadcastAttitude` as of M22.2b
    /// (`docs/open-questions.md` questions 142/149). **Why every `"attitude."` instance is now
    /// wrapped, unconditionally, rather than only the ones an DRM author intends to feed a
    /// sensor:** `TruthBroadcastAttitude<M>` delegates every `DynamicsModel` method to the
    /// wrapped model completely unchanged *except* `step_with_ports`, which additionally
    /// broadcasts the state's leading 7 components (quaternion + body rate -- every
    /// `AttitudeWheelsModel` state always has at least this many components, wheels or not) as
    /// SIGNAL messages on `sensors::TRUTH_PORT_NAMES`. A message on a port with no matching
    /// `Connection` is silently dropped by `crate::router::Router` (that module's own doc
    /// comment) -- so for the two existing attitude fixtures that declare no such ports/
    /// connections at all (`drms/demo_attitude_precession.*`, `drms/demo_attitude_wheel_fault.*`)
    /// this wrap is a strict behavioural no-op: identical state, identical trajectory, identical
    /// hash, the extra seven messages simply vanish unread. It is what lets
    /// `drms/demo_attitude_sensors_truth.system.yaml`'s own attitude instance actually feed
    /// `crate::drm::sensors::StarTrackerModel`/`ImuModel` once both are wired to a real
    /// `SosConfiguration`'s shared kernel run (M14.1) -- see `crate::drm::sensors`'s own module
    /// doc comment (written at M22.2, before this wiring existed) and that same fixture's header
    /// comment for the exact gap this closes: without this, `AnyModel::Attitude`'s bare model
    /// reaches `DynamicsModel::step_with_ports`'s trait *default* (empty `Outbox`), so no
    /// `"startracker."`/`"imu."`-dispatched instance run through `execute()` could ever receive
    /// truth and would emit zero measurements forever -- reachable from a DRM in name only. A
    /// conditional wrap (e.g. keyed off a new declared parameter) was considered and rejected:
    /// it would add a second, parallel way to say "this instance has an attitude-shaped state"
    /// for zero behavioural benefit, since the unconditional wrap is already provably inert
    /// wherever nothing is connected to listen.
    /// **M22.4 addendum:** additionally wrapped in `controller::CommandedAttitude` (see that
    /// type's own doc comment) -- unconditionally, the same "provably inert wherever nothing is
    /// connected" reasoning immediately above already established for `TruthBroadcastAttitude`
    /// itself: an attitude instance declaring no wheel-torque-command FRAMED IN port (every
    /// fixture before M22.4) constructs `CommandedAttitude` with `command_codec: None`, which
    /// passes `controls` through completely unchanged.
    Attitude(CommandedAttitude<TruthBroadcastAttitude<AttitudeWheelsModel>>),
    /// M22.4 (`docs/sil-plan.md`'s M22 milestone paragraph, "A native 'controller' instance
    /// closes the loop first"): a real `crate::drm::controller::AttitudeControllerModel`,
    /// dispatched via `"attctrl."` (see `crate::registry::kind_for`).
    Controller(AttitudeControllerModel),
    /// `"startracker."`-prefixed (M22.2b, `docs/open-questions.md` questions 142/149/151/152): a
    /// real `crate::drm::sensors::StarTrackerModel`, dispatched via `crate::registry::kind_for`.
    StarTracker(StarTrackerModel),
    /// `"imu."`-prefixed (M22.2b): a real `crate::drm::sensors::ImuModel`.
    Imu(ImuModel),
    /// `"ground."`-prefixed (M25.1, `docs/sil-plan.md`'s M25 milestone: "ground segment as a
    /// system"): a real `crate::drm::ground::GroundStationModel`, dispatched via
    /// `crate::registry::kind_for`.
    GroundStation(ground::GroundStationModel),
    /// M25.4b (question 175's own follow-on): a [`ReplayModel`] standing in for whatever
    /// instance `RunConfig.replay.instances` named -- see that module's own doc comment for the
    /// full contract. Unlike every other variant above, dispatch into this one is never decided
    /// by `SystemDefinition.dynamics_model`'s own prefix (`crate::registry::kind_for` knows
    /// nothing about replay): `crate::registry::ModelRegistry::wrap_replay` builds this variant
    /// directly, from a `ModelHandle` any of the OTHER constructors already produced, only when
    /// `crate::drm::executor::execute` has decided this particular instance is being replayed
    /// this run -- so a DRM/SOS/SystemDefinition triple is completely unaware replay ever
    /// happens; only `RunConfig` says so.
    Replay(ReplayModel),
}

/// A closed-form constant-acceleration model, parameterized entirely from declared
/// `SystemDefinition` parameters (never a hidden default) -- the "anything else, for now"
/// binding target. Physically and numerically identical to the `ConstantAccel` test models
/// already in `crate::kernel`/`crate::schedule`'s own unit tests; this copy exists because
/// those are `#[cfg(test)]`-private to their modules.
///
/// **M14.1 (question 109): optional SIGNAL port participation.** `emit`/`consume_port` (both
/// declared, hashed `"port.*"` parameters -- see [`parse_constant_accel_spec`]) let this native
/// model stand in for the "native SIGNAL producer"/"native consumer" roles the M14.1 required
/// end-to-end test needs, exercising the shared kernel run's `Router` delivery against a real
/// `BINDING_KIND_CONTAINER` process without inventing a whole new binding kind: `emit`, if set,
/// makes every step also send a constant SIGNAL value on the named port (`Outbox::push_signal`);
/// `consume_port`, if set, makes every step read the latest message on that port from its
/// `Inbox` (question 108's own delivery-order guarantee already sorts it there) and attach the
/// decoded value as a `StepResult.outputs["received"]` entry -- reachable through
/// `output.<instance>.received@time` exactly like a GMAT-bound instance's own named outputs
/// (`OUTPUT_PARAMETER_PREFIX`, `crate::drm::executor::declared_outputs`). Neither field changes
/// `derivatives`/`step`'s own closed-form physics at all -- see [`ConstantAccelModel::
/// step_with_ports`].
///
/// **M19.4 (question 131): optional range-condition gating.** `condition`, if set (see
/// [`ConstantAccelSpec::condition`]), turns `emit` from unconditional-every-step into
/// edge-triggered-once: `already_fired` is the interior-mutable latch that remembers, for the
/// lifetime of this one materialization, whether the condition has already fired -- `Cell`,
/// not a plain `bool` field, for the same `&self`-only-methods reason [`ContainerModel`]'s own
/// fields need interior mutability. A fault/maneuver boundary constructs a fresh
/// `ConstantAccelModel` (this crate's own "a boundary changes parameters by constructing a
/// fresh handle" contract, `crate::registry`'s module doc comment), so this latch resets to
/// `false` at every re-materialization -- disclosed, not hidden: see `drms/demo_two_instance.
/// sos.yaml`'s own header comment for why this demo's own declared threshold is chosen so no
/// re-materialization boundary ever lands while the condition already holds.
/// [`ConstantAccelModel::state_dim`]'s own physical dimension -- **M21.3 (`docs/open-
/// questions.md` question 141, decided by the lead, closing question 133's own escalation)
/// takes this from the instance's own declared state space at materialization, not a fixed
/// constant any more.** Through M20.1, [`classify_binding`]'s `ModelKind::Native` arm refused
/// any declared dimension other than the one fixed constant below -- "the declared dimension
/// becomes the source of truth" inverts that: the declared, resolved [`av_cdm::pb::StateSpace`]
/// (`crate::trajectory::resolve_state_space`) is now authoritative, and [`ConstantAccelModel`]
/// is built to match it. This binding kind can honour exactly two widths, never a hidden third:
/// [`CONSTANT_ACCEL_STATE_DIM`] (6, the double integrator `derivatives` below implements --
/// `[pos_x, pos_y, pos_z, vel_x, vel_y, vel_z]` in SI units, `state[3..6]` read, `self.a`
/// written into `out[3..6]`) or `0` (no physical state at all -- `demo_ctrl`'s own shape as of
/// this task, `drms/demo_two_instance_ctrl.system.yaml`). [`parse_constant_accel_spec`] is what
/// actually decides which of the two a given instance's own `"state.*"` parameters configure
/// (all six present, or none at all -- a partial subset is refused, [`DrmError::
/// MissingParameter`]) by building [`ConstantAccelSpec::x0_si`] to that length; [`classify_binding`]
/// then refuses, with a typed [`DrmError::StateSpaceDimensionMismatch`], any instance whose
/// declared state space width disagrees with what its own `"state.*"` parameters configured --
/// covering both "declared 6 but configured 0 (or vice versa)" and "declared some other width
/// entirely (this model can never honour anything but 0 or 6)". **Representation, stated
/// plainly:** there is no fixed-width array anywhere in this binding kind's own state any more --
/// [`ConstantAccelSpec::x0_si`]/[`Materialized::x0_si`]/[`crate::registry::ModelHandle::x0_si`]
/// are all `Vec<f64>`, exactly as long as the instance's own honoured dimension and not one slot
/// longer; a dim-0 instance carries a genuinely empty `Vec`, never six zeros held back and hidden
/// from the trajectory.
pub(crate) const CONSTANT_ACCEL_STATE_DIM: usize = 6;

pub(crate) struct ConstantAccelModel {
    pub a: [f64; 3],
    pub info: ModelInfo,
    pub emit: Option<(String, f64)>,
    pub consume_port: Option<String>,
    pub condition: Option<RangeCondition>,
    pub already_fired: Cell<bool>,
    /// M25.1: `(port, codec)` when [`ConstantAccelSpec::emit_framed_port`]/`.emit_framed_codec`
    /// were both resolved -- see [`ConstantAccelSpec::emit_framed_port`]'s own doc comment.
    pub emit_framed: Option<(String, PacketCodec)>,
    /// This instance's own CCSDS sequence counter for `emit_framed`'s packets -- `Cell` for the
    /// same `&self`-only-methods reason `already_fired` needs interior mutability.
    pub framed_seq: Cell<u16>,
    /// M21.3 (question 141): this instance's own materialized state width -- either
    /// [`CONSTANT_ACCEL_STATE_DIM`] (6) or `0`, set once at construction
    /// ([`materialize_constant_accel`]) from `spec.x0_si.len()`, itself already validated
    /// against the declared state space by [`classify_binding`]. Never mutated afterward (a
    /// fault/maneuver boundary constructs a fresh `ConstantAccelModel`, same as
    /// `already_fired`'s own doc comment above).
    pub dim: usize,
    /// M25.2 (`docs/sil-plan.md`'s M25 milestone, "Job 1: the flight-side FRAMED consume"):
    /// `(port, codec, field)` when [`ConstantAccelSpec::consume_framed_port`]/`.
    /// consume_framed_codec`/`.consume_framed_field` were all resolved -- `field` is always one
    /// of [`CONSTANT_ACCEL_WRITABLE_PARAMETERS`], already validated by `parse_constant_accel_
    /// spec`. `None` (every fixture before M25.2) is a strict no-op: `step_with_ports` below
    /// never even looks at `inbox` for this purpose, `commanded_accel_scale` stays `1.0` forever,
    /// and `derivatives` produces the byte-identical `self.a` it always has -- see the model's
    /// own doc comment for the additive, off-by-default contract this follows.
    pub consume_framed: Option<(String, PacketCodec, String)>,
    /// The commanded scale factor on [`ConstantAccelModel::a`] (`derivatives`: `out[3..6] =
    /// self.a * self.commanded_accel_scale.get()`) -- `1.0` (no-op) until `consume_framed`
    /// actually decodes and applies a packet. `Cell`, not a plain `f64` field, for the same
    /// `&self`-only-methods reason `already_fired`/`framed_seq` need interior mutability.
    pub commanded_accel_scale: Cell<f64>,
    /// M20.3-style (question 137) "changed, or first" applied-command reporting cache for
    /// `consume_framed` -- mirrors `gmat_sys::model::GmatModel`'s own `last_applied` exactly
    /// (that struct's own doc comment has the full reasoning): the write to `commanded_accel_
    /// scale` always happens, every step a message decodes; only whether it is *reported* as an
    /// `av_dynamics::AppliedCommand` (and, downstream, an ack sent) depends on whether the
    /// decoded value differs, by exact bit equality, from the last one this cache recorded.
    pub last_applied_command_value: Cell<Option<f64>>,
    /// M25.2: `(port, codec)` when [`ConstantAccelSpec::ack_framed_port`]/`.ack_framed_codec`
    /// were both resolved -- see [`ConstantAccelSpec::ack_framed_port`]'s own doc comment.
    pub ack_framed: Option<(String, PacketCodec)>,
    /// Question 188 (R5.2): every undecodable frame this instance's own most recent
    /// `step_with_ports` call received on `consume_framed`'s own port and skipped, continuing
    /// with `commanded_accel_scale`'s own last good value rather than aborting the run --
    /// mirrors `super::controller::AttitudeControllerModel::decode_errors_this_step`'s own doc
    /// comment exactly.
    pub decode_errors_this_step: RefCell<Vec<av_dynamics::DecodeErrorOccurrence>>,
}
impl DynamicsModel for ConstantAccelModel {
    type Error = std::convert::Infallible;
    fn state_dim(&self) -> usize {
        self.dim
    }
    fn derivatives(&self, state: &[f64], _t_tai_ns: i64, _controls: &[f64], out: &mut [f64]) -> Result<(), Self::Error> {
        // M21.3 (question 141): a dim-0 instance has no physical state at all -- `state`/`out`
        // are both empty slices, and there is nothing to integrate. Honestly a different,
        // empty case, not a degenerate 6-wide one (indexing `state[3..6]` here would panic).
        if self.dim == 0 {
            return Ok(());
        }
        debug_assert_eq!(self.dim, CONSTANT_ACCEL_STATE_DIM, "the only two widths materialize_constant_accel ever builds are 0 and CONSTANT_ACCEL_STATE_DIM");
        out[0..3].copy_from_slice(&state[3..6]);
        // M25.2: `commanded_accel_scale` is `1.0` (a strict no-op, `self.a` unchanged) unless
        // `consume_framed` has actually decoded and applied a command -- see this struct's own
        // doc comment. Every `derivatives` call within one `step_with_ports` call (every RK
        // sub-stage) sees whatever `step_with_ports` already applied *before* calling `self.
        // step`, mirroring `gmat_sys::model::GmatModel::step_with_ports`'s own "consume, then
        // step" ordering exactly.
        let scale = self.commanded_accel_scale.get();
        out[3] = self.a[0] * scale;
        out[4] = self.a[1] * scale;
        out[5] = self.a[2] * scale;
        Ok(())
    }
    fn describe(&self) -> ModelInfo {
        self.info.clone()
    }

    /// See this type's own doc comment. Reuses [`DynamicsModel::step`]'s own (default,
    /// closed-form-integrated) result unchanged -- `emit`/`consume_port` only add an `Outbox`
    /// entry and a `StepResult.outputs["received"]` entry around it, never touch the physical
    /// state itself. The trait's default `step_with_ports` (which this overrides) would
    /// otherwise silently ignore both `inbox` and the possibility of emitting anything -- see
    /// `av_dynamics`'s own module doc comment on why a model must opt in explicitly.
    ///
    /// Always reports no applied commands (question 130, M19.3): `consume_port` writes only
    /// into this step's own `StepResult.outputs["received"]` -- a per-step telemetry echo, not
    /// a write into anything `av_dynamics::settings_hash` (or any other hashed configuration
    /// surface) covers -- so, unlike `gmat_sys::model::GmatModel`'s own `consume` path, nothing
    /// here ever bypasses a hash a `dynamics_hash` comparison relies on. There is accordingly
    /// nothing question 130 asks this model to report.
    ///
    /// **M19.4 (question 131): `condition` gates `emit`.** With `condition == None`, behaviour
    /// is byte-for-byte the pre-M19.4 M14.1 contract (emit unconditionally, every step) -- no
    /// existing fixture declaring only `port.emit`/`port.emit_value` changes behaviour. With
    /// `condition == Some(cond)`: the just-decoded `received` value (this step's own consumed
    /// value, not a stale one from an earlier step -- `None` if nothing decoded this step) is
    /// compared against `cond`; the very first step it satisfies `cond` (and only that step --
    /// `already_fired` latches immediately) this model emits `emit`'s own constant value,
    /// exactly once for the remaining lifetime of this materialization.
    fn step_with_ports(&self, state: &[f64], t_tai_ns: i64, controls: &[f64], dt_ns: i64, inbox: &Inbox) -> Result<(StepResult, Outbox, Vec<AppliedCommand>), Self::Error> {
        // M25.2 (`docs/sil-plan.md`'s M25 milestone, "Job 1: the flight-side FRAMED consume"):
        // decode, then apply, then step -- in that order, mirroring `gmat_sys::model::GmatModel::
        // step_with_ports`'s own "consume, then step, then emit" doc comment exactly, and for the
        // identical reason: `commanded_accel_scale` must already hold the newly-commanded value
        // before `self.step` below calls `derivatives` (possibly several times, one per RK
        // sub-stage), so the command's physical effect lands in *this* step's own propagated
        // state, not merely a later one. A message that fails to decode, or no message on
        // `consume_framed`'s own port at all, leaves `commanded_accel_scale` untouched -- the
        // same "no message, no effect" contract `consume_port` below already uses. `applied`
        // (M20.3/question 137's "changed, or first" rule -- see `last_applied_command_value`'s
        // own doc comment) is empty unless a command actually changed something.
        self.decode_errors_this_step.borrow_mut().clear();
        let mut applied: Vec<AppliedCommand> = Vec::new();
        let mut newly_applied_seq: Option<u16> = None;
        if let Some((port, codec, field)) = &self.consume_framed {
            if let Some((msg, _sender)) = inbox.last_on_port(port) {
                let mut apid_map = crate::codec::ApidMap::new();
                apid_map.insert(codec.apid, codec.clone());
                match crate::codec::decode_packet(&apid_map, &msg.payload) {
                    Ok(decoded) => {
                        if let Some(crate::codec::FieldValue::Numeric(value)) = decoded.fields.get("value") {
                            // The write always happens, unconditionally, on every decoded message
                            // -- this is the physics (`derivatives`, below, reads it every RK
                            // sub-stage of `self.step` further down); only whether it is
                            // *reported* (and, downstream, acknowledged) depends on `last_applied_
                            // command_value` -- see that field's own doc comment.
                            debug_assert_eq!(field.as_str(), "accel_scale", "CONSTANT_ACCEL_WRITABLE_PARAMETERS has exactly one entry today; parse_constant_accel_spec already refused anything else");
                            self.commanded_accel_scale.set(*value);
                            let changed_or_first = self.last_applied_command_value.get() != Some(*value);
                            if changed_or_first {
                                self.last_applied_command_value.set(Some(*value));
                                applied.push(AppliedCommand { port: port.clone(), field: field.clone(), value: *value, applied_tai_ns: t_tai_ns });
                                newly_applied_seq = Some(decoded.sequence_count);
                            }
                        }
                    }
                    // Question 188 (R5.2): an undecodable command frame is recorded, never
                    // silently swallowed as it was through R5.1b -- `commanded_accel_scale` is
                    // deliberately left untouched (the "no message, no effect" contract this
                    // method's own doc comment already states, applied identically to a message
                    // that arrived but failed to decode).
                    Err(e) => self.decode_errors_this_step.borrow_mut().push(crate::codec::decode_error_occurrence(port, msg, &e)),
                }
            }
        }
        let mut result = self.step(state, t_tai_ns, controls, dt_ns)?;
        let mut received: Option<f64> = None;
        if let Some(port_name) = &self.consume_port {
            // The *last* matching message, not the first: `Inbox`'s own delivery order
            // (question 108: port, then sender emission epoch, then sender instance id) puts
            // the most-recently-emitted message last among ties on this port, which is the
            // only ordering property this native consumer needs -- it has no per-sender
            // identity of its own to disambiguate by.
            if let Some((msg, _sender)) = inbox.last_on_port(port_name) {
                if let Some(v) = av_dynamics::decode_signal(&msg.payload) {
                    result.outputs.insert("received".to_string(), v);
                    received = Some(v);
                }
            }
        }
        let mut outbox = Outbox::new();
        if let Some((port, value)) = &self.emit {
            let should_emit = match &self.condition {
                None => true,
                Some(cond) => {
                    if self.already_fired.get() {
                        false
                    } else {
                        let holds = received.is_some_and(|v| if cond.above { v >= cond.threshold_m } else { v <= cond.threshold_m });
                        if holds {
                            self.already_fired.set(true);
                        }
                        holds
                    }
                }
            };
            if should_emit {
                outbox.push_signal(port.clone(), result.t_tai_ns, *value);
            }
        }
        // M25.1: broadcast this step's own propagated Cartesian position as one CCSDS packet,
        // every step, on the declared FRAMED OUT port -- unconditional (no `condition` gating,
        // unlike `emit` above: this is telemetry, not a one-shot command), and a strict no-op
        // when `emit_framed` is `None` (every fixture before M25.1). Only meaningful for a
        // 6-dimensional instance (`self.dim == CONSTANT_ACCEL_STATE_DIM`) -- `resolve_sensor_
        // output`'s own codec resolution never runs for a dim-0 instance's `emit_framed_port` in
        // practice (nothing to broadcast), but this guard makes the precondition explicit rather
        // than indexing an empty `result.state` if it ever did.
        if let Some((port, codec)) = &self.emit_framed {
            if self.dim == CONSTANT_ACCEL_STATE_DIM {
                let mut values = BTreeMap::new();
                values.insert("x".to_string(), crate::codec::FieldValue::Numeric(result.state[0]));
                values.insert("y".to_string(), crate::codec::FieldValue::Numeric(result.state[1]));
                values.insert("z".to_string(), crate::codec::FieldValue::Numeric(result.state[2]));
                let seq = self.framed_seq.get();
                self.framed_seq.set(seq.wrapping_add(1) & 0x3FFF);
                let payload = crate::codec::encode_packet(codec, seq, &[], &values).expect(
                    "emit_framed_codec was resolved by classify_binding's own resolve_sensor_output, which already requires x/y/z fields wide enough for a Numeric value -- encode_packet can only fail for a missing/mistyped/out-of-range field, none of which can happen here",
                );
                outbox.push(port.clone(), result.t_tai_ns, payload);
            }
        }
        // M25.2: "acknowledged by the flight software's telemetry" (`docs/sil-plan.md`'s M25
        // milestone) -- sent only in the same step the command was actually applied (`newly_
        // applied_seq` is `Some` only when `last_applied_command_value`'s own "changed, or
        // first" check above just passed), carrying the acknowledged packet's own CCSDS
        // `sequence_count` so the ground instance can correlate this ack to the command it
        // dispatched (`crate::drm::command`'s own module doc comment, "Scope disclosed, not
        // hidden"). A strict no-op when `ack_framed` is `None` (every fixture before M25.2) or
        // when nothing was newly applied this step.
        if let (Some((port, codec)), Some(seq)) = (&self.ack_framed, newly_applied_seq) {
            let mut values = BTreeMap::new();
            values.insert("cmd_seq".to_string(), crate::codec::FieldValue::Numeric(seq as f64));
            let payload = crate::codec::encode_packet(codec, seq, &[], &values).expect(
                "ack_framed_codec was resolved by classify_binding's own resolve_constant_accel_ack_port, which already requires a \"cmd_seq\" field wide enough for a Numeric value -- encode_packet can only fail for a missing/mistyped/out-of-range field, none of which can happen here",
            );
            outbox.push(port.clone(), result.t_tai_ns, payload);
        }
        Ok((result, outbox, applied))
    }

    // `emit_framed` above encodes a raw CCSDS packet (x/y/z) but its codec declares no
    // `PacketField.target` -- this model never calls `crate::codec::measurements_from_field_values`
    // and so never produces a CDM measurement.
    fn last_measurements(&self) -> Vec<av_cdm::pb::Measurement> {
        Vec::new()
    }
    // No SENSOR fault runtime.
    fn drain_sensor_fault_effect(&self) -> Option<av_dynamics::SensorFaultEffectDrain> {
        None
    }

    /// Question 188 (R5.2): every occurrence `step_with_ports` recorded this call into
    /// `decode_errors_this_step` -- see that field's own doc comment.
    fn drain_decode_errors(&self) -> Vec<av_dynamics::DecodeErrorOccurrence> {
        self.decode_errors_this_step.borrow().clone()
    }
}

#[derive(Debug)]
pub(crate) enum AnyModelError {
    Gmat(GmatError),
    /// (Question 112) `stm_derivatives`/`step_with_stm` invoked on a variant whose own
    /// `stm_capable()` is `false` -- the `AnyModel` counterpart of `av_dynamics::erase::
    /// ErasedModel::stm_derivatives`'s identical guard (see that method's own doc comment for
    /// the full reasoning): converts what used to be an `unimplemented!()` panic
    /// (`AnyModel::ConstantAccel`'s prior arm) into a typed, catchable error at this enum's own
    /// type-erasure boundary, mirroring `av_dynamics::ModelError::CapabilityMissing` (this enum
    /// cannot carry a `ModelError` directly -- `AnyModel::Error` is fixed before erasure into
    /// that shared type; `crate::registry`'s own model handle is what performs that further
    /// conversion, unchanged by this addition).
    CapabilityMissing { model_id: String, capability: String },
    /// M22.4: a FRAMED port command/measurement packet failed to decode
    /// (`crate::codec::CodecError`) inside `controller::CommandedAttitude`/`controller::
    /// AttitudeControllerModel::step_with_ports` -- stringified rather than wrapped
    /// structurally, mirroring `AnyModelError::Gmat`'s own "wrap the model-specific error's
    /// `Display`" convention, since this enum cannot itself depend on every model-specific error
    /// type without becoming as wide as `AnyModel` itself.
    PortCodec { model_id: String, detail: String },
    /// M25.4b: `crate::drm::replay::ReplayError` (a replayed instance's own missing-frame
    /// refusal), stringified -- a genuinely new failure shape this enum did not have before,
    /// not folded into `PortCodec` (which names a specific, different cause) just to avoid
    /// adding a variant.
    Replay { model_id: String, detail: String },
}
impl fmt::Display for AnyModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AnyModelError::Gmat(e) => write!(f, "{e}"),
            AnyModelError::CapabilityMissing { model_id, capability } => write!(f, "model {model_id:?}: capability {capability:?} was invoked but is not declared"),
            AnyModelError::PortCodec { model_id, detail } => write!(f, "model {model_id:?}: port codec error: {detail}"),
            AnyModelError::Replay { model_id, detail } => write!(f, "model {model_id:?}: {detail}"),
        }
    }
}
impl std::error::Error for AnyModelError {}

impl DynamicsModel for AnyModel {
    type Error = AnyModelError;

    fn state_dim(&self) -> usize {
        match self {
            AnyModel::Gmat(m) => m.state_dim(),
            AnyModel::ConstantAccel(m) => m.state_dim(),
            AnyModel::Attitude(m) => m.state_dim(),
            AnyModel::Controller(m) => m.state_dim(),
            AnyModel::StarTracker(m) => m.state_dim(),
            AnyModel::Imu(m) => m.state_dim(),
            AnyModel::GroundStation(m) => m.state_dim(),
            AnyModel::Replay(m) => m.state_dim(),
        }
    }
    fn derivatives(&self, state: &[f64], t_tai_ns: i64, controls: &[f64], out: &mut [f64]) -> Result<(), Self::Error> {
        match self {
            AnyModel::Gmat(m) => m.derivatives(state, t_tai_ns, controls, out).map_err(AnyModelError::Gmat),
            AnyModel::ConstantAccel(m) => match m.derivatives(state, t_tai_ns, controls, out) {
                Ok(()) => Ok(()),
                Err(never) => match never {},
            },
            // M22.4: `AnyModel::Attitude`'s wrapped `CommandedAttitude<...>::Error` is no longer
            // literally `Infallible` (its own `Codec` arm is genuinely reachable from a
            // malformed inbound wheel-torque command packet, though `derivatives` itself never
            // reaches that arm -- only `step_with_ports` decodes anything) -- `map_err` into the
            // generic `PortCodec`, mirroring `ModelRegistry::into_boxed`'s own erasure mapping.
            AnyModel::Attitude(m) => m.derivatives(state, t_tai_ns, controls, out).map_err(|e| AnyModelError::PortCodec { model_id: m.describe().id, detail: e.to_string() }),
            AnyModel::Controller(m) => m.derivatives(state, t_tai_ns, controls, out).map_err(|e| AnyModelError::PortCodec { model_id: m.describe().id, detail: e.to_string() }),
            AnyModel::StarTracker(m) => match m.derivatives(state, t_tai_ns, controls, out) {
                Ok(()) => Ok(()),
                Err(never) => match never {},
            },
            AnyModel::Imu(m) => match m.derivatives(state, t_tai_ns, controls, out) {
                Ok(()) => Ok(()),
                Err(never) => match never {},
            },
            AnyModel::GroundStation(m) => match m.derivatives(state, t_tai_ns, controls, out) {
                Ok(()) => Ok(()),
                Err(never) => match never {},
            },
            // M25.4b: `ReplayModel::derivatives` never fails (always a zero-order-hold Ok(())),
            // but its `Error` is `replay::ReplayError`, not `Infallible` (the missing-frame
            // refusal lives in `step_with_ports` -- see that arm below), so this still needs a
            // real `map_err`, mirroring the `Attitude`/`Controller` arms' shape rather than the
            // `match never {}` shape.
            AnyModel::Replay(m) => m.derivatives(state, t_tai_ns, controls, out).map_err(|e| AnyModelError::Replay { model_id: m.describe().id, detail: e.to_string() }),
        }
    }
    fn describe(&self) -> ModelInfo {
        match self {
            AnyModel::Gmat(m) => m.describe(),
            AnyModel::ConstantAccel(m) => m.describe(),
            AnyModel::Attitude(m) => m.describe(),
            AnyModel::Controller(m) => m.describe(),
            AnyModel::StarTracker(m) => m.describe(),
            AnyModel::Imu(m) => m.describe(),
            AnyModel::GroundStation(m) => m.describe(),
            AnyModel::Replay(m) => m.describe(),
        }
    }
    fn stm_capable(&self) -> bool {
        match self {
            AnyModel::Gmat(m) => m.stm_capable(),
            // The native placeholder model declares no STM capability -- consistent with the
            // existing `ConstantAccel` test models in `kernel`/`schedule`, which also default
            // `stm_capable` to `false` rather than implement a closed-form STM for this
            // stand-in binding.
            AnyModel::ConstantAccel(_) => false,
            // M22.1b (question 152, decided by the lead): "Covariance for attitude is a typed
            // refusal this batch" -- `AttitudeWheelsModel` never declares STM capability either,
            // for the identical reason `ConstantAccel` does not: no closed-form state-transition
            // matrix is implemented for this model yet. Every covariance-requesting call site in
            // `crate::drm::executor` already refuses on `!stm_capable()` with the generic, typed
            // `DrmError::ModelNotStmCapable` -- naming this `false` here is what makes that
            // refusal apply to an attitude instance too, with no attitude-specific covariance
            // code path anywhere in this crate.
            AnyModel::Attitude(_) => false,
            // M22.2b: neither sensor model implements a closed-form state-transition matrix
            // either -- same reasoning as ConstantAccel/Attitude above. See the module doc
            // comment's exit-criteria section for why covariance against a sensor instance is a
            // typed refusal this batch: `crate::drm::executor::run_covariance_instance` already
            // refuses on `!stm_capable()` with the generic `DrmError::ModelNotStmCapable`, so
            // naming this `false` here is the whole of what a sensor instance needs for that
            // refusal to apply, with no sensor-specific covariance code anywhere in this crate.
            AnyModel::StarTracker(_) => false,
            AnyModel::Imu(_) => false,
            // M22.4: no closed-form state-transition matrix for the controller either -- same
            // reasoning as every other native model above.
            AnyModel::Controller(_) => false,
            // M25.1: no closed-form state-transition matrix for the ground station either -- a
            // fixed geodetic site with an instantaneous visibility transform has no propagated
            // state to carry an STM at all. `crate::drm::executor::run_covariance_instance`'s
            // existing generic `DrmError::ModelNotStmCapable` refusal applies with no
            // ground-station-specific covariance code (see `crate::drm::ground`'s own module doc
            // comment, "Fault / maneuver / covariance").
            AnyModel::GroundStation(_) => false,
            // M25.4b: a replayed instance never declares STM capability -- there is no bound
            // process left behind it to propagate a state transition matrix from, replayed or
            // not. `crate::drm::executor` refuses `RunConfig.replay` combined with
            // `DrmOptions.covariance` before this could ever matter in practice (`DrmError::
            // ReplayWithCovarianceNotSupported`), but this stays `false` unconditionally, the
            // same "the invariant, not merely the call site that happens to enforce it today"
            // reasoning every other variant's own arm above already follows.
            AnyModel::Replay(_) => false,
        }
    }
    fn stm_derivatives(&self, augmented_state: &[f64], t_tai_ns: i64, controls: &[f64], out: &mut [f64]) -> Result<(), Self::Error> {
        match self {
            AnyModel::Gmat(m) => m.stm_derivatives(augmented_state, t_tai_ns, controls, out).map_err(AnyModelError::Gmat),
            // Question 112: a typed `CapabilityMissing`, not a panic -- see `AnyModelError::
            // CapabilityMissing`'s own doc comment. `ConstantAccelModel::stm_capable()` is
            // always `false` (checked above), so this arm is only ever reached by a caller that
            // did not check first -- exactly the case a trait-object boundary cannot enforce at
            // compile time any more.
            AnyModel::ConstantAccel(m) => Err(AnyModelError::CapabilityMissing { model_id: m.describe().id, capability: "stm_derivatives".to_string() }),
            // Same typed guard as the ConstantAccel arm above, for the identical reason:
            // AttitudeWheelsModel::stm_capable() is always false (checked above).
            AnyModel::Attitude(m) => Err(AnyModelError::CapabilityMissing { model_id: m.describe().id, capability: "stm_derivatives".to_string() }),
            // Same typed guard, same reason: neither sensor model's stm_capable() is ever true.
            AnyModel::StarTracker(m) => Err(AnyModelError::CapabilityMissing { model_id: m.describe().id, capability: "stm_derivatives".to_string() }),
            AnyModel::Imu(m) => Err(AnyModelError::CapabilityMissing { model_id: m.describe().id, capability: "stm_derivatives".to_string() }),
            // Same typed guard, same reason: AttitudeControllerModel::stm_capable() is always
            // false.
            AnyModel::Controller(m) => Err(AnyModelError::CapabilityMissing { model_id: m.describe().id, capability: "stm_derivatives".to_string() }),
            // Same typed guard, same reason: GroundStationModel::stm_capable() is always false.
            AnyModel::GroundStation(m) => Err(AnyModelError::CapabilityMissing { model_id: m.describe().id, capability: "stm_derivatives".to_string() }),
            // Same typed guard, same reason: a replayed instance's stm_capable() is always
            // false (see that arm above).
            AnyModel::Replay(m) => Err(AnyModelError::CapabilityMissing { model_id: m.describe().id, capability: "stm_derivatives".to_string() }),
        }
    }

    /// Delegates to each variant's own `integrator` (question 112: added by this task --
    /// `AnyModel` previously had no override at all here, silently reaching this trait's own
    /// default `Dopri5::default()` regardless of what either variant's own `integrator()` might
    /// someday return. Neither `GmatModel` nor `ConstantAccelModel` overrides `integrator` today
    /// (both use the trait's default), so this is currently a no-op fix in terms of observed
    /// behavior -- exactly like `AnyModel::step_with_stm`'s own "the honest, general fix rather
    /// than one that happens to work only because nothing overrides this method yet" reasoning.
    fn integrator(&self) -> av_dynamics::integrate::Dopri5 {
        match self {
            AnyModel::Gmat(m) => m.integrator(),
            AnyModel::ConstantAccel(m) => m.integrator(),
            AnyModel::Attitude(m) => m.integrator(),
            AnyModel::Controller(m) => m.integrator(),
            AnyModel::StarTracker(m) => m.integrator(),
            AnyModel::Imu(m) => m.integrator(),
            AnyModel::GroundStation(m) => m.integrator(),
            AnyModel::Replay(m) => m.integrator(),
        }
    }

    /// Delegates to each variant's own `step` (M10.3: see `av_dynamics::erase`'s module doc
    /// comment, `crates/av-dynamics/src/erase.rs`, for the sibling half of this fix). Before
    /// this, `AnyModel` inherited the
    /// trait's default `step` -- which recomputes the identical physical state via
    /// `derivatives`, so nothing looked broken, but it silently discarded
    /// `gmat_sys::model::GmatModel::step`'s override that populates `StepResult.outputs` with
    /// `gmat_sys::model::OUTPUT_RMAG`. `executor.rs`'s `StepDelegating` was the workaround this
    /// task removes now that both halves of the gap (this one and `ErasedModel`'s) are fixed at
    /// their actual source.
    fn step(&self, state: &[f64], t_tai_ns: i64, controls: &[f64], dt_ns: i64) -> Result<StepResult, Self::Error> {
        match self {
            AnyModel::Gmat(m) => m.step(state, t_tai_ns, controls, dt_ns).map_err(AnyModelError::Gmat),
            AnyModel::ConstantAccel(m) => match m.step(state, t_tai_ns, controls, dt_ns) {
                Ok(r) => Ok(r),
                Err(never) => match never {},
            },
            // M22.4: see `AnyModel::derivatives`'s own identical-shaped `Attitude` arm's comment.
            AnyModel::Attitude(m) => m.step(state, t_tai_ns, controls, dt_ns).map_err(|e| AnyModelError::PortCodec { model_id: m.describe().id, detail: e.to_string() }),
            AnyModel::Controller(m) => m.step(state, t_tai_ns, controls, dt_ns).map_err(|e| AnyModelError::PortCodec { model_id: m.describe().id, detail: e.to_string() }),
            AnyModel::StarTracker(m) => match m.step(state, t_tai_ns, controls, dt_ns) {
                Ok(r) => Ok(r),
                Err(never) => match never {},
            },
            AnyModel::Imu(m) => match m.step(state, t_tai_ns, controls, dt_ns) {
                Ok(r) => Ok(r),
                Err(never) => match never {},
            },
            AnyModel::GroundStation(m) => match m.step(state, t_tai_ns, controls, dt_ns) {
                Ok(r) => Ok(r),
                Err(never) => match never {},
            },
            // M25.4b: see `AnyModel::derivatives`'s own identical-shaped `Replay` arm's comment
            // -- `ReplayModel::step` cannot fail on its own (it only ever calls `step_with_ports`
            // with an empty inbox, which CAN fail on a missing frame), but the error type is not
            // `Infallible`, so this needs a real `map_err` too.
            AnyModel::Replay(m) => m.step(state, t_tai_ns, controls, dt_ns).map_err(|e| AnyModelError::Replay { model_id: m.describe().id, detail: e.to_string() }),
        }
    }

    /// Delegates to each variant's own `step_with_stm`, for the same reason [`AnyModel::step`]
    /// does -- neither variant overrides `step_with_stm` today (only `GmatModel::step` is
    /// overridden, to populate `outputs`), so for the `Gmat` variant this is currently
    /// behaviourally identical to the inherited default, but it is the honest, general fix
    /// rather than one that happens to work only because nothing overrides this method yet. The
    /// `ConstantAccel` variant is additionally guarded on `stm_capable()` (question 112) --
    /// see that arm's own comment.
    fn step_with_stm(&self, augmented_state: &[f64], t_tai_ns: i64, controls: &[f64], dt_ns: i64) -> Result<StmStepResult, Self::Error> {
        match self {
            AnyModel::Gmat(m) => m.step_with_stm(augmented_state, t_tai_ns, controls, dt_ns).map_err(AnyModelError::Gmat),
            // Question 112: same typed-error guard as `stm_derivatives` above, for the same
            // reason -- `ConstantAccelModel::stm_capable()` is always `false`, so calling this
            // would otherwise reach the trait's own default `step_with_stm`, which integrates
            // `stm_derivatives` and panics on exactly this precondition violation.
            AnyModel::ConstantAccel(m) if !m.stm_capable() => Err(AnyModelError::CapabilityMissing { model_id: m.describe().id, capability: "step_with_stm".to_string() }),
            AnyModel::ConstantAccel(m) => match m.step_with_stm(augmented_state, t_tai_ns, controls, dt_ns) {
                Ok(r) => Ok(r),
                Err(never) => match never {},
            },
            // Same typed guard, same reason: AttitudeWheelsModel::stm_capable() is always false.
            AnyModel::Attitude(m) if !m.stm_capable() => Err(AnyModelError::CapabilityMissing { model_id: m.describe().id, capability: "step_with_stm".to_string() }),
            // M22.4: see `AnyModel::derivatives`'s own identical-shaped `Attitude` arm's comment
            // -- dead in practice (the guard above is always taken, `stm_capable()` is always
            // false), but must still typecheck against the wrapped model's real `Error` type.
            AnyModel::Attitude(m) => m.step_with_stm(augmented_state, t_tai_ns, controls, dt_ns).map_err(|e| AnyModelError::PortCodec { model_id: m.describe().id, detail: e.to_string() }),
            // Same typed guard, same reason: neither sensor model's stm_capable() is ever true.
            AnyModel::StarTracker(m) if !m.stm_capable() => Err(AnyModelError::CapabilityMissing { model_id: m.describe().id, capability: "step_with_stm".to_string() }),
            AnyModel::StarTracker(m) => match m.step_with_stm(augmented_state, t_tai_ns, controls, dt_ns) {
                Ok(r) => Ok(r),
                Err(never) => match never {},
            },
            AnyModel::Imu(m) if !m.stm_capable() => Err(AnyModelError::CapabilityMissing { model_id: m.describe().id, capability: "step_with_stm".to_string() }),
            AnyModel::Imu(m) => match m.step_with_stm(augmented_state, t_tai_ns, controls, dt_ns) {
                Ok(r) => Ok(r),
                Err(never) => match never {},
            },
            // Same typed guard, same reason: AttitudeControllerModel::stm_capable() is always
            // false.
            AnyModel::Controller(m) if !m.stm_capable() => Err(AnyModelError::CapabilityMissing { model_id: m.describe().id, capability: "step_with_stm".to_string() }),
            AnyModel::Controller(m) => m.step_with_stm(augmented_state, t_tai_ns, controls, dt_ns).map_err(|e| AnyModelError::PortCodec { model_id: m.describe().id, detail: e.to_string() }),
            // Same typed guard, same reason: GroundStationModel::stm_capable() is always false.
            AnyModel::GroundStation(m) if !m.stm_capable() => Err(AnyModelError::CapabilityMissing { model_id: m.describe().id, capability: "step_with_stm".to_string() }),
            AnyModel::GroundStation(m) => match m.step_with_stm(augmented_state, t_tai_ns, controls, dt_ns) {
                Ok(r) => Ok(r),
                Err(never) => match never {},
            },
            // Same typed guard, same reason: a replayed instance's stm_capable() is always
            // false (see that arm above) -- dead in practice (the guard is always taken), but
            // must still typecheck against ReplayModel's real Error type, mirroring the
            // Attitude/Controller arms' identical "dead but must typecheck" shape.
            AnyModel::Replay(m) if !m.stm_capable() => Err(AnyModelError::CapabilityMissing { model_id: m.describe().id, capability: "step_with_stm".to_string() }),
            AnyModel::Replay(m) => m.step_with_stm(augmented_state, t_tai_ns, controls, dt_ns).map_err(|e| AnyModelError::Replay { model_id: m.describe().id, detail: e.to_string() }),
        }
    }

    /// Delegates to each variant's own `step_with_ports`, for the same reason [`AnyModel::step`]
    /// does (M14.1, question 109): neither `AnyModel` itself overriding this method, nor a
    /// variant's own override, is reached by the trait's *default* `step_with_ports` (which
    /// only ever calls `self.step`, ignoring `inbox` and returning an empty `Outbox`) -- see
    /// `av_dynamics::erase::ErasedModel::step_with_ports`'s own module doc comment for the
    /// sibling half of this exact gap, closed there for a wrapped model directly implementing
    /// `DynamicsModel` (M13.1). `AnyModel` sits one level further out (an enum wrapping either
    /// concrete model), so it needs its own explicit delegation the same way `step`/
    /// `step_with_stm` above already do, or [`ConstantAccelModel::step_with_ports`]'s emit/
    /// consume behaviour would be silently discarded the moment a `ConstantAccelModel` is
    /// wrapped in `AnyModel::ConstantAccel` (exactly the `crate::registry::ModelRegistry::
    /// construct_native` path `crate::drm::executor`'s shared kernel run registers).
    fn step_with_ports(&self, state: &[f64], t_tai_ns: i64, controls: &[f64], dt_ns: i64, inbox: &Inbox) -> Result<(StepResult, Outbox, Vec<AppliedCommand>), Self::Error> {
        match self {
            AnyModel::Gmat(m) => m.step_with_ports(state, t_tai_ns, controls, dt_ns, inbox).map_err(AnyModelError::Gmat),
            AnyModel::ConstantAccel(m) => match m.step_with_ports(state, t_tai_ns, controls, dt_ns, inbox) {
                Ok(r) => Ok(r),
                Err(never) => match never {},
            },
            // M22.2b: the wrapped `sensors::TruthBroadcastAttitude<AttitudeWheelsModel>` DOES
            // override `step_with_ports` now (unlike bare `AttitudeWheelsModel`, which still
            // does not) -- it broadcasts the propagated state's leading 7 components (quaternion
            // + body rate) as SIGNAL messages on `sensors::TRUTH_PORT_NAMES` every step, in
            // addition to computing the identical physical state `AnyModel::step`'s own arm
            // above does. Delegating here reaches that override honestly, rather than `AnyModel`
            // fabricating the outbox itself -- see `AnyModel::Attitude`'s own doc comment for why
            // every attitude instance is now wrapped unconditionally.
            // M22.4: this is where `controller::CommandedAttitude`'s own decode of an inbound
            // wheel-torque command packet actually happens -- genuinely fallible now (a
            // malformed packet is a typed `Codec` error), unlike `derivatives`/`step` above
            // (which cannot reach that arm).
            AnyModel::Attitude(m) => m.step_with_ports(state, t_tai_ns, controls, dt_ns, inbox).map_err(|e| AnyModelError::PortCodec { model_id: m.describe().id, detail: e.to_string() }),
            // M22.4: the controller's own decode of the inbound star tracker/IMU measurement
            // packets, and its own emission of the wheel-torque command packet, both happen
            // here -- see `controller::AttitudeControllerModel::step_with_ports`'s own doc
            // comment.
            AnyModel::Controller(m) => m.step_with_ports(state, t_tai_ns, controls, dt_ns, inbox).map_err(|e| AnyModelError::PortCodec { model_id: m.describe().id, detail: e.to_string() }),
            // StarTrackerModel/ImuModel both override step_with_ports directly (never the
            // trait's own default) -- this is where trap 3 (declared update rate, not the kernel
            // step) and the FRAMED CCSDS emission actually happen. Delegating here is what makes
            // that override reachable through a real DRM at all -- see the module doc comment's
            // `AnyModel::Attitude` note for the identical "wrapper/override must be reached
            // through delegation, not the trait default" defect class this closes for sensors,
            // and `crate::drm::sensors`'s own module doc comment for the four traps themselves.
            AnyModel::StarTracker(m) => match m.step_with_ports(state, t_tai_ns, controls, dt_ns, inbox) {
                Ok(r) => Ok(r),
                Err(never) => match never {},
            },
            AnyModel::Imu(m) => match m.step_with_ports(state, t_tai_ns, controls, dt_ns, inbox) {
                Ok(r) => Ok(r),
                Err(never) => match never {},
            },
            // M25.1: this is where the ground station's own decode of the inbound telemetry
            // packet, elevation/contact-window computation, and its own AOS-acknowledgment
            // telecommand packet emission all happen -- see `crate::drm::ground::
            // GroundStationModel::step_with_ports`'s own doc comment.
            AnyModel::GroundStation(m) => match m.step_with_ports(state, t_tai_ns, controls, dt_ns, inbox) {
                Ok(r) => Ok(r),
                Err(never) => match never {},
            },
            // M25.4b: the playback itself happens here -- see `crate::drm::replay`'s own module
            // doc comment for the missing-frame rule this can refuse against, and
            // `AnyModelError::Replay`'s own doc comment for why that refusal gets its own
            // variant rather than reusing `PortCodec`.
            AnyModel::Replay(m) => m.step_with_ports(state, t_tai_ns, controls, dt_ns, inbox).map_err(|e| AnyModelError::Replay { model_id: m.describe().id, detail: e.to_string() }),
        }
    }

    /// Delegates to each variant's own `last_measurements` (question 173, M25.3) -- only
    /// `StarTracker`/`Imu` ever return anything non-empty today; every other variant's own
    /// `last_measurements` is that model's own explicit `Vec::new()` override, each with its own
    /// one-line "why no measurement" comment (M25.3c: `av_dynamics::DynamicsModel::
    /// last_measurements` is a required method now, not a defaulted one -- see that method's own
    /// doc comment), reached honestly here rather than `AnyModel` itself silently
    /// short-circuiting to empty for everything. See `AnyModel::step_with_ports`'s own doc
    /// comment for why an explicit arm per variant, not a blanket default on `AnyModel` itself,
    /// is this enum's own standing convention -- and `any_model_arm_count_for_ground_station_
    /// matches_star_tracker` (this module's own test) for the executable arm-count symmetry
    /// check that covers this method too (it counts every `AnyModel::<Variant>(` occurrence in
    /// this file regardless of which method's match arm it belongs to).
    fn last_measurements(&self) -> Vec<av_cdm::pb::Measurement> {
        match self {
            AnyModel::Gmat(m) => m.last_measurements(),
            AnyModel::ConstantAccel(m) => m.last_measurements(),
            AnyModel::Attitude(m) => m.last_measurements(),
            AnyModel::Controller(m) => m.last_measurements(),
            AnyModel::StarTracker(m) => m.last_measurements(),
            AnyModel::Imu(m) => m.last_measurements(),
            AnyModel::GroundStation(m) => m.last_measurements(),
            AnyModel::Replay(m) => m.last_measurements(),
        }
    }

    /// Delegates to each variant's own `drain_sensor_fault_effect` (question 178, R5.1a) -- only
    /// `StarTracker` ever returns anything non-`None` today (the one model with a SENSOR fault
    /// runtime); every other variant's own `drain_sensor_fault_effect` is that model's own
    /// explicit `None` override, mirroring `AnyModel::last_measurements`'s own identical
    /// reasoning (a required trait method, no blanket default here either) and covered by the
    /// same executable arm-count check.
    fn drain_sensor_fault_effect(&self) -> Option<av_dynamics::SensorFaultEffectDrain> {
        match self {
            AnyModel::Gmat(m) => m.drain_sensor_fault_effect(),
            AnyModel::ConstantAccel(m) => m.drain_sensor_fault_effect(),
            AnyModel::Attitude(m) => m.drain_sensor_fault_effect(),
            AnyModel::Controller(m) => m.drain_sensor_fault_effect(),
            AnyModel::StarTracker(m) => m.drain_sensor_fault_effect(),
            AnyModel::Imu(m) => m.drain_sensor_fault_effect(),
            AnyModel::GroundStation(m) => m.drain_sensor_fault_effect(),
            AnyModel::Replay(m) => m.drain_sensor_fault_effect(),
        }
    }

    /// Delegates to each variant's own `drain_decode_errors` (question 188, R5.2) -- mirrors
    /// `AnyModel::drain_sensor_fault_effect`'s own identical dispatch and reasoning (a required
    /// trait method, no catch-all arm here either) and covered by the same executable arm-count
    /// check (`any_model_arm_count_for_ground_station_matches_star_tracker`).
    fn drain_decode_errors(&self) -> Vec<av_dynamics::DecodeErrorOccurrence> {
        match self {
            AnyModel::Gmat(m) => m.drain_decode_errors(),
            AnyModel::ConstantAccel(m) => m.drain_decode_errors(),
            AnyModel::Attitude(m) => m.drain_decode_errors(),
            AnyModel::Controller(m) => m.drain_decode_errors(),
            AnyModel::StarTracker(m) => m.drain_decode_errors(),
            AnyModel::Imu(m) => m.drain_decode_errors(),
            AnyModel::GroundStation(m) => m.drain_decode_errors(),
            AnyModel::Replay(m) => m.drain_decode_errors(),
        }
    }
}

// --------------------------------------------------------------------------------------
// Container (lockstep) model (M13.2, question 107) -- see the module doc comment's
// "Container (lockstep) binding" section for the parameter vocabulary and what is and is not
// wired end to end.
// --------------------------------------------------------------------------------------

/// Everything a `LockstepService`-speaking process's typed refusal to keep running can be:
/// a transport failure at `Bind`/`Step`/`Shutdown`, `Bind`'s own declared incapability, or
/// (`lockstep.proto`'s own doc comment) a protocol error mid-run -- a `Step` response whose
/// `sequence` or `reached_tai_ns` does not match what was sent. Every variant is fatal: "the
/// run stops" (the proto's own words), never a warning or a silent retry.
#[derive(Debug, Clone)]
pub enum ContainerError {
    /// Could not even connect to `address` (`av_lockstep::ConnectError`, stringified).
    Connect { address: String, detail: String },
    /// The `Bind` RPC itself failed at the transport/gRPC-status level (as opposed to
    /// succeeding but reporting `lockstep_capable = false`, which is
    /// [`ContainerError::Refused`]).
    BindRpc { detail: String },
    /// `LockstepBindResponse.lockstep_capable` was `false` -- includes both the plain "not
    /// capable" case and a declared port-set mismatch (`lockstep.proto`'s own doc comment:
    /// "the bound process must accept exactly this set (names, kinds, directions) or
    /// refuse"), since both are reported through the identical `lockstep_capable`/
    /// `refusal_reason` fields; `reason` carries the process's own explanation for which one
    /// this was.
    Refused { reason: String },
    /// The `Step` RPC itself failed at the transport/gRPC-status level.
    StepRpc { detail: String },
    /// `LockstepStepResponse.sequence` did not equal the `LockstepStepRequest.sequence` this
    /// call sent -- a protocol error, per `lockstep.proto`'s own doc comment.
    SequenceMismatch { expected: u64, got: u64 },
    /// `LockstepStepResponse.reached_tai_ns` did not equal the `until_tai_ns` this call sent
    /// -- a protocol error, per `lockstep.proto`'s own doc comment ("Must equal
    /// until_tai_ns; anything else is a protocol error").
    ReachedTaiMismatch { expected: i64, got: i64 },
    /// The `Shutdown` RPC itself failed at the transport/gRPC-status level.
    ShutdownRpc { detail: String },
    /// M15.3 (question 118): the `Reset` RPC itself failed at the transport/gRPC-status level
    /// -- `ContainerModel::reset`'s counterpart to [`ContainerError::StepRpc`].
    ResetRpc { detail: String },
    /// M15.3: `LockstepResetResponse.sequence` did not equal the `LockstepResetRequest.sequence`
    /// this call sent -- a protocol error, the same rule `lockstep.proto`'s own doc comment
    /// states for every request ("the kernel increments [sequence] by one; a response whose
    /// sequence does not match is a protocol error and the run stops"), applied to `Reset` too.
    /// A distinct variant from [`ContainerError::SequenceMismatch`] (not a shared one) so this
    /// variant's own `Display` names `Reset`, not `Step` -- reusing the `Step`-worded variant
    /// for a `Reset` mismatch would misreport which RPC actually failed.
    ResetSequenceMismatch { expected: u64, got: u64 },
    /// M15.3 (question 118): stopping and removing a Docker-run container instance at
    /// `Shutdown` failed (`av_lockstep::docker::DockerError`, stringified). Never raised for a
    /// `container.address`-only instance (M13.2's own subprocess path) -- that path never
    /// creates a [`ManagedContainer`] to tear down in the first place.
    DockerTeardown { detail: String },
}
impl fmt::Display for ContainerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ContainerError::Connect { address, detail } => write!(f, "connecting to lockstep process at {address:?}: {detail}"),
            ContainerError::BindRpc { detail } => write!(f, "Bind RPC failed: {detail}"),
            ContainerError::Refused { reason } => write!(f, "lockstep process refused (lockstep_capable=false): {reason}"),
            ContainerError::StepRpc { detail } => write!(f, "Step RPC failed: {detail}"),
            ContainerError::SequenceMismatch { expected, got } => write!(f, "Step protocol error: sent sequence {expected}, response echoed {got}"),
            ContainerError::ReachedTaiMismatch { expected, got } => write!(f, "Step protocol error: requested until_tai_ns {expected}, response reached_tai_ns {got}"),
            ContainerError::ShutdownRpc { detail } => write!(f, "Shutdown RPC failed: {detail}"),
            ContainerError::ResetRpc { detail } => write!(f, "Reset RPC failed: {detail}"),
            ContainerError::ResetSequenceMismatch { expected, got } => write!(f, "Reset protocol error: sent sequence {expected}, response echoed {got}"),
            ContainerError::DockerTeardown { detail } => write!(f, "stopping/removing the Docker-run container at Shutdown failed: {detail}"),
        }
    }
}
impl std::error::Error for ContainerError {}

/// A bound `BINDING_KIND_CONTAINER` instance: a live [`BlockingLockstepClient`] plus the
/// bookkeeping (`next_sequence`) needed to speak `Step` correctly. `state_dim() == 0` --
/// see the module doc comment's "Container (lockstep) binding" section for why this binding
/// kind carries no physical ODE state in this batch. Interior mutability (`RefCell`/`Cell`)
/// because `av_dynamics::DynamicsModel`'s methods take `&self` (the shared contract every
/// model in this workspace implements), but a gRPC client and a monotonically increasing
/// sequence counter are inherently stateful -- the same reason `gmat_sys::model::GmatModel`
/// (a `!Send` FFI handle) is driven through `&self` methods that mutate hidden native state.
pub(crate) struct ContainerModel {
    client: RefCell<BlockingLockstepClient>,
    next_sequence: Cell<u64>,
    info: ModelInfo,
    /// `LockstepBindResponse.binding_hash`, recorded here so `crate::drm::executor` can
    /// attach it to this instance's `Trajectory.provenance` without re-touching the network
    /// (question 107's "What to build" item 2: "binding_hash into provenance").
    pub binding_hash: String,
    /// M15.3 (question 118): `Some` iff this instance was bound via the Docker image-lifecycle
    /// path (`ContainerBinding.image` set) rather than M13.2's `container.address`-only
    /// already-running-process path -- `Some` is what [`ContainerModel::shutdown`] stops and
    /// removes, once, after the `Shutdown` RPC succeeds ("Shutdown then stop and remove on run
    /// end," question 118's own decision). `RefCell` for the same `&self`-only-methods reason
    /// every other field here needs interior mutability; `stop_and_remove` needs `&mut
    /// ManagedContainer`, so a bare field (not `Cell`) is required to borrow it mutably.
    managed: RefCell<Option<ManagedContainer>>,
}

impl DynamicsModel for ContainerModel {
    type Error = ContainerError;

    fn state_dim(&self) -> usize {
        0
    }

    /// Never actually called: [`ContainerModel::step`]/[`ContainerModel::step_with_ports`]
    /// are both overridden below and never invoke the default `step` (which would otherwise
    /// drive `av_dynamics::integrate::Dopri5` over this trivial 0-length state instead of
    /// speaking the lockstep protocol at all). Kept as a harmless, honestly-trivial `Ok(())`
    /// rather than `unimplemented!()`, since a 0-length `state`/`state_dot` slice makes "do
    /// nothing" the only meaningful implementation regardless.
    fn derivatives(&self, _state: &[f64], _t_tai_ns: i64, _controls: &[f64], _state_dot: &mut [f64]) -> Result<(), Self::Error> {
        Ok(())
    }

    fn describe(&self) -> ModelInfo {
        self.info.clone()
    }

    /// Delegates to [`ContainerModel::step_with_ports`] with an empty `Inbox` -- kept
    /// consistent with it rather than left at the trait's default (which would try to
    /// integrate `derivatives` instead of speaking the lockstep protocol).
    fn step(&self, state: &[f64], t_tai_ns: i64, controls: &[f64], dt_ns: i64) -> Result<StepResult, Self::Error> {
        self.step_with_ports(state, t_tai_ns, controls, dt_ns, &Inbox::empty()).map(|(result, _outbox, _applied)| result)
    }

    /// Send one `Step`, check both halves of the protocol contract
    /// (`sequence`/`reached_tai_ns`), and convert the response into a `(StepResult, Outbox)`
    /// pair. `inbox` is forwarded verbatim as `LockstepStepRequest.inputs` -- `av_cdm::pb::
    /// PortMessage` is the exact wire type on both sides (`av-grpc/build.rs`'s `extern_path`,
    /// M13.2), so no conversion happens here at all, just a clone of the slice. Always reports
    /// no applied commands (question 130, M19.3): a container-bound instance's own bound
    /// configuration is opaque to this binding (there is no `GmatSystemSpec`-equivalent this
    /// crate hashes for it at all), so there is nothing this binding could honestly attribute a
    /// write to -- unlike `gmat_sys::model::GmatModel`, which writes into a field this crate's
    /// own `gmat_settings` hashes and so must report.
    fn step_with_ports(&self, state: &[f64], t_tai_ns: i64, _controls: &[f64], dt_ns: i64, inbox: &Inbox) -> Result<(StepResult, Outbox, Vec<AppliedCommand>), Self::Error> {
        let sequence = self.next_sequence.get();
        let until_tai_ns = t_tai_ns + dt_ns;
        let request = LockstepStepRequest { sequence, until_tai_ns, inputs: inbox.messages().to_vec() };

        let response = self.client.borrow_mut().step(request).map_err(|e| ContainerError::StepRpc { detail: e.to_string() })?;
        if response.sequence != sequence {
            return Err(ContainerError::SequenceMismatch { expected: sequence, got: response.sequence });
        }
        if response.reached_tai_ns != until_tai_ns {
            return Err(ContainerError::ReachedTaiMismatch { expected: until_tai_ns, got: response.reached_tai_ns });
        }
        self.next_sequence.set(sequence + 1);

        let mut outbox = Outbox::new();
        for m in &response.outputs {
            outbox.push(m.port.clone(), m.tai_ns, m.payload.clone());
        }
        // `state` is always the caller's own 0-length slice (state_dim() == 0); `to_vec()` on
        // an empty slice is an empty Vec, exactly what this binding kind's "no physical
        // state" contract requires.
        let step_result = StepResult { state: state.to_vec(), t_tai_ns: until_tai_ns, outputs: response.named_outputs };
        Ok((step_result, outbox, Vec::new()))
    }

    // `lockstep.proto`'s `LockstepStepResponse` carries `outputs`/`named_outputs`, not a
    // `Measurement` -- the lockstep wire protocol has no CDM measurement concept yet, so this
    // binding kind never produces one.
    fn last_measurements(&self) -> Vec<av_cdm::pb::Measurement> {
        Vec::new()
    }
    // No SENSOR fault runtime.
    fn drain_sensor_fault_effect(&self) -> Option<av_dynamics::SensorFaultEffectDrain> {
        None
    }
    // `lockstep.proto`'s wire protocol carries no CCSDS/FRAMED concept at all -- a container
    // instance never itself calls `crate::codec::decode_packet` (question 188, R5.2).
    fn drain_decode_errors(&self) -> Vec<av_dynamics::DecodeErrorOccurrence> {
        Vec::new()
    }
}

impl ContainerModel {
    /// Send `Shutdown` and, for a Docker-run instance ([`ContainerModel::managed`] is `Some`),
    /// stop and remove the container afterward -- question 118's "Shutdown then stop and remove
    /// on run end," in that order (the process gets a chance to exit cleanly on its own `Shutdown`
    /// handler before this reaches for `docker stop`/`docker rm`). Not part of
    /// `av_dynamics::DynamicsModel` (that trait has no run-lifecycle-end hook; every binding
    /// kind's own "the run ended" behaviour, if any, belongs to its caller, not the trait).
    /// `pub(crate)`: only `crate::drm::executor::run_shared_group` calls this, once, after this
    /// instance's last `Step`.
    pub(crate) fn shutdown(&self, request: av_lockstep::LockstepShutdownRequest) -> Result<av_lockstep::LockstepShutdownResponse, ContainerError> {
        let response = self.client.borrow_mut().shutdown(request).map_err(|e| ContainerError::ShutdownRpc { detail: e.to_string() })?;
        if let Some(managed) = self.managed.borrow_mut().as_mut() {
            managed.stop_and_remove().map_err(|e| ContainerError::DockerTeardown { detail: e.to_string() })?;
        }
        Ok(response)
    }

    /// Send `Reset` at a fault's own epoch (`tai_ns`) with the declared `reason` (M15.3, question
    /// 118: `"fault:<fault id>"` for a power-cycle fault -- `executor::run_shared_group`'s own
    /// doc comment) and check the protocol contract the same way [`ContainerModel::
    /// step_with_ports`] already does for `Step`: `lockstep.proto`'s own doc comment states the
    /// sequence rule for *every* request, `Reset` included ("the kernel increments [sequence] by
    /// one; a response whose sequence does not match is a protocol error and the run stops").
    /// `pub(crate)`: only `crate::drm::executor::run_shared_group`'s boundary loop calls this.
    pub(crate) fn reset(&self, tai_ns: i64, reason: String) -> Result<(), ContainerError> {
        let sequence = self.next_sequence.get();
        let response = self.client.borrow_mut().reset(av_lockstep::LockstepResetRequest { sequence, tai_ns, reason }).map_err(|e| ContainerError::ResetRpc { detail: e.to_string() })?;
        if response.sequence != sequence {
            return Err(ContainerError::ResetSequenceMismatch { expected: sequence, got: response.sequence });
        }
        self.next_sequence.set(sequence + 1);
        Ok(())
    }
}

/// M14.1 (question 109): the same live [`ContainerModel`] -- one connection, one monotonically
/// increasing `next_sequence` counter -- registered on a *new* `crate::kernel::HeteroKernel` at
/// every boundary-bounded span of the shared run (`crate::drm::executor`'s own module doc
/// comment: "the existing per-instance segment split becomes a split of the whole kernel run").
/// `HeteroKernel::register_system` takes ownership of a boxed model, and a fault/maneuver
/// boundary always rebuilds the *kernel* (a fresh `HeteroScheduler`), so the same underlying
/// connection has to be re-erased into a fresh `Box<dyn DynamicsModel>` each span without ever
/// re-`Bind`-ing it -- `Rc<ContainerModel>` is the shared handle that survives across spans
/// (`ContainerModel`'s own `RefCell`/`Cell` fields already give it the interior mutability a
/// `&self`-only trait method needs); this newtype is only the thin `DynamicsModel` adapter
/// Rust's orphan rules require (`av_dynamics::DynamicsModel` and `std::rc::Rc` are both
/// foreign to this crate, so `impl DynamicsModel for Rc<ContainerModel>` directly is not
/// allowed -- a locally-defined wrapper type is). A `BINDING_KIND_CONTAINER` instance is never
/// a fault/maneuver boundary's own *target* (`execute()` refuses that up front,
/// `DrmError::ContainerFaultsOrManeuversNotSupported`), so this wrapper never needs to change
/// what it wraps between spans -- only which span's kernel currently owns a box pointing at it.
#[derive(Clone)]
pub(crate) struct SharedContainerModel(pub Rc<ContainerModel>);
impl DynamicsModel for SharedContainerModel {
    type Error = ContainerError;
    fn state_dim(&self) -> usize {
        self.0.state_dim()
    }
    fn derivatives(&self, state: &[f64], t_tai_ns: i64, controls: &[f64], state_dot: &mut [f64]) -> Result<(), Self::Error> {
        self.0.derivatives(state, t_tai_ns, controls, state_dot)
    }
    fn describe(&self) -> ModelInfo {
        self.0.describe()
    }
    /// Question 112: explicit, even though `ContainerModel` never overrides `integrator` (so
    /// this is currently a no-op fix) -- `SharedContainerModel` is a plain passthrough wrapper
    /// exactly like `av_dynamics::erase::ErasedModel`/`AnyModel`, so it gets the same "delegate
    /// every method, never rely on this trait's own default for a wrapper's own dispatch" rule
    /// those two follow, rather than waiting for a future `ContainerModel` override to expose the
    /// same gap a fourth time.
    fn integrator(&self) -> av_dynamics::integrate::Dopri5 {
        self.0.integrator()
    }
    /// Question 112: explicit for the same reason `integrator` above is. `ContainerModel` never
    /// overrides `stm_capable` (state_dim() == 0: a container-bound instance carries no
    /// physical ODE state, so the trait's own default `false` is already the honest answer, not
    /// a placeholder) -- delegating rather than hardcoding `false` here means a future
    /// `ContainerModel::stm_capable` override (should one ever make sense) is picked up
    /// automatically instead of silently staying `false` behind this wrapper.
    fn stm_capable(&self) -> bool {
        self.0.stm_capable()
    }
    fn stm_derivatives(&self, augmented_state: &[f64], t_tai_ns: i64, controls: &[f64], out: &mut [f64]) -> Result<(), Self::Error> {
        self.0.stm_derivatives(augmented_state, t_tai_ns, controls, out)
    }
    fn step_with_stm(&self, state: &[f64], t_tai_ns: i64, controls: &[f64], dt_ns: i64) -> Result<StmStepResult, Self::Error> {
        self.0.step_with_stm(state, t_tai_ns, controls, dt_ns)
    }
    fn step(&self, state: &[f64], t_tai_ns: i64, controls: &[f64], dt_ns: i64) -> Result<StepResult, Self::Error> {
        self.0.step(state, t_tai_ns, controls, dt_ns)
    }
    fn step_with_ports(&self, state: &[f64], t_tai_ns: i64, controls: &[f64], dt_ns: i64, inbox: &Inbox) -> Result<(StepResult, Outbox, Vec<AppliedCommand>), Self::Error> {
        self.0.step_with_ports(state, t_tai_ns, controls, dt_ns, inbox)
    }
    /// Delegates, same as every other method here (this struct's own doc comment: "a plain
    /// passthrough wrapper exactly like `ErasedModel`/`AnyModel`").
    fn last_measurements(&self) -> Vec<av_cdm::pb::Measurement> {
        self.0.last_measurements()
    }
    /// Delegates, same as every other method here (question 178, R5.1a).
    fn drain_sensor_fault_effect(&self) -> Option<av_dynamics::SensorFaultEffectDrain> {
        self.0.drain_sensor_fault_effect()
    }
    /// Delegates, same as every other method here (question 188, R5.2).
    fn drain_decode_errors(&self) -> Vec<av_dynamics::DecodeErrorOccurrence> {
        self.0.drain_decode_errors()
    }
}

/// The result of binding one `BINDING_KIND_CONTAINER` instance: a live, already-`Bind`-ed
/// [`ContainerModel`] plus its epoch. Deliberately **not** [`Materialized`]/[`AnyModel`] --
/// see the module doc comment's "`ModelRegistry` is the sole constructor" section:
/// `crate::registry::ModelRegistry`/`AnyModel`/`ModelHandle` are a fixed, closed set this
/// task does not own or extend (`crates/av-kernel/src/registry.rs` is not this task's to
/// edit); [`MaterializedContainer`] is a parallel, self-contained return shape
/// `crate::drm::executor::run_container_instance` (also not going through `HeteroKernel`,
/// for the same "no physical state to schedule" reason) consumes directly.
pub(crate) struct MaterializedContainer {
    pub model: ContainerModel,
    pub t0_tai_ns: i64,
}

/// Connect (over `spec.address`, or a freshly pulled-and-run Docker container's own loopback
/// address -- see below), send `Bind`, and refuse (typed, never a silent partial bind) unless
/// the response reports `lockstep_capable = true`. `seed` is the already-resolved value for
/// `spec.seed_key` (`crate::drm::executor` resolves the key against `Scenario.seeds` before
/// calling this -- see [`DrmError::UnknownContainerSeed`]). `base_period_ns`/`step_period_ns`
/// are this batch's simplified reading of `lockstep.proto`'s "the kernel's base period; every
/// Step's until_tai_ns is start + k * base" contract: since a container-bound instance runs on
/// its own dedicated loop rather than sharing a cross-instance base clock with anything else
/// (see the module doc comment), `base_period_ns == step_period_ns` here -- both simply the
/// instance's own effective step period.
///
/// ## Docker image lifecycle (M15.3, question 118)
///
/// When `spec.image` is set, this function pulls and runs it *before* ever connecting --
/// `av_lockstep::docker::ManagedContainer::pull_and_run(image, image_digest, command,
/// spec.control_port, port_endpoints, env)`, publishing the control port plus every declared
/// `port_endpoints` entry on loopback (question 118: "run with the port endpoint mapped"), and
/// always connecting `BlockingLockstepClient::connect_plaintext` to the loopback address Docker
/// actually published the control port on (`parse_container_spec` already refuses `container.tls
/// = true` alongside `image` -- question 118: "Bind over loopback"). `env` carries exactly one
/// entry, `IMAGE_DIGEST = spec.image_digest`, which `services/lockstep-ref`'s own `Bind` handler
/// folds into `LockstepBindResponse.binding_hash` (question 118: "`binding_hash` includes the
/// digest") -- this function does not compute or check that itself; it only has to get the
/// digest into the running container's environment for the bound process's own `Bind` to see.
/// The resulting [`ManagedContainer`] is stored on the returned [`ContainerModel`]
/// (`managed`), which `ContainerModel::shutdown` stops and removes once the `Shutdown` RPC
/// succeeds ("Shutdown then stop and remove on run end") -- and, via [`ManagedContainer`]'s
/// own `Drop`, best-effort torn down even if this function returns an error partway through
/// `Bind` (a container that was successfully pulled and run but then refused at `Bind`, or
/// whose `Bind` RPC itself failed, must not be left running).
///
/// `pub(crate)`: only `crate::drm::executor::run_shared_group` calls this, matching
/// `materialize_gmat`/`materialize_constant_accel`'s own visibility.
#[allow(clippy::too_many_arguments)]
pub(crate) fn materialize_container(
    spec: &ContainerSpec,
    sys: &SystemDefinition,
    instance_name: &str,
    run_id: &str,
    epoch_tai_ns: i64,
    step_period_ns: i64,
    seed: u64,
    parameters: &BTreeMap<String, String>,
) -> Result<MaterializedContainer, DrmError> {
    let (managed, address): (Option<ManagedContainer>, String) = if let Some(image) = &spec.image {
        // Already checked by `parse_container_spec`: `image_digest` non-empty whenever `image`
        // is set, and `spec.tls` is false -- both invariants this function relies on rather than
        // re-checking (a `classify_binding` bug producing a `ContainerSpec` that violates them
        // is an internal error, not a fresh user error, mirroring the existing `tls_paths()`
        // `.ok_or_else` below for the `container.address` path).
        let digest = spec.image_digest.clone().unwrap_or_default();
        let mut env = BTreeMap::new();
        env.insert("IMAGE_DIGEST".to_string(), digest.clone());
        let (managed, host_port) = ManagedContainer::pull_and_run(image, &digest, &spec.command, spec.control_port, &spec.port_endpoints, &env, &spec.docker_sysctls, &BTreeMap::new())
            .map_err(|e| DrmError::ContainerDockerLifecycle { instance: instance_name.to_string(), detail: e.to_string() })?;
        (Some(managed), format!("127.0.0.1:{host_port}"))
    } else {
        (None, spec.address.clone())
    };

    let try_connect_and_bind = || -> Result<(BlockingLockstepClient, av_lockstep::LockstepBindResponse), DrmError> {
        let mut client = if spec.tls {
            let (ca, cert, key) = spec.tls_paths().ok_or_else(|| DrmError::ContainerConnect {
                instance: instance_name.to_string(),
                address: address.clone(),
                detail: "container.tls is set but container.ca_file/client_cert/client_key were not fully parsed -- this is a classify_binding bug, not a user error (parse_container_spec should have refused first)".to_string(),
            })?;
            BlockingLockstepClient::connect_mtls(&format!("https://{address}"), std::path::Path::new(ca), Some(std::path::Path::new(cert)), Some(std::path::Path::new(key)))
        } else {
            BlockingLockstepClient::connect_plaintext(&address)
        }
        .map_err(|e| DrmError::ContainerConnect { instance: instance_name.to_string(), address: address.clone(), detail: e.to_string() })?;

        let request = LockstepBindRequest {
            run_id: run_id.to_string(),
            instance: instance_name.to_string(),
            ports: sys.ports.clone(),
            start_tai_ns: epoch_tai_ns,
            base_period_ns: step_period_ns,
            step_period_ns,
            seed,
            parameters: parameters.clone(),
        };
        let response = client.bind(request).map_err(|e| DrmError::ContainerBind { instance: instance_name.to_string(), detail: e.to_string() })?;
        if !response.lockstep_capable {
            return Err(DrmError::ContainerRefused { instance: instance_name.to_string(), reason: response.refusal_reason });
        }
        Ok((client, response))
    };

    // A freshly `docker run -d`-started container's TCP listen socket (and even a bare gRPC
    // channel connect) can be open slightly before its own request-handling thread pool is
    // actually servicing calls -- observed directly while building this task (a `Bind` sent in
    // that narrow window fails with a transport error even though a moment later it succeeds).
    // Retry the whole connect-then-Bind attempt within a bounded readiness window for the
    // Docker path only (`spec.image.is_some()`) -- the `container.address` path is unchanged:
    // M13.2's own contract there is "already running," so a transport failure against it is a
    // real error, not a startup race, and is never retried. `ContainerRefused` (a real,
    // considered `lockstep_capable = false` response) is never retried either -- only
    // `ContainerConnect`/`ContainerBind`, the two shapes a not-yet-ready server produces.
    const CONTAINER_READY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
    let bind_result = if spec.image.is_some() {
        let deadline = std::time::Instant::now() + CONTAINER_READY_TIMEOUT;
        loop {
            let attempt = try_connect_and_bind();
            match &attempt {
                Err(DrmError::ContainerConnect { .. }) | Err(DrmError::ContainerBind { .. }) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                _ => break attempt,
            }
        }
    } else {
        try_connect_and_bind()
    };

    // A Docker-run container that never reaches a successful `Bind` (connect failure, transport
    // failure, or an explicit refusal) must not be left running -- `ManagedContainer`'s own
    // `Drop` best-effort stops and removes it here, the same guarantee a panicking/early-`?`
    // test already gets from `tests/drm_container.rs`'s bare-subprocess `ChildGuard`.
    let (client, response) = match bind_result {
        Ok(ok) => ok,
        Err(e) => {
            drop(managed);
            return Err(e);
        }
    };

    let mut settings = BTreeMap::new();
    settings.insert("address".to_string(), address.clone());
    settings.insert("tls".to_string(), spec.tls.to_string());
    settings.insert("version".to_string(), response.version.clone());
    if let Some(image) = &spec.image {
        settings.insert("image".to_string(), image.clone());
        settings.insert("image_digest".to_string(), spec.image_digest.clone().unwrap_or_default());
    }
    let info = ModelInfo {
        id: format!("container.{instance_name}"),
        version: response.version,
        state_space_id: sys.state_space_id.clone(),
        frame_id: String::new(),
        controls: vec![],
        capabilities: vec![ModelCapability::Step as i32, ModelCapability::Deterministic as i32],
        depth: "container-lockstep".to_string(),
        settings_hash: av_dynamics::settings_hash(&settings),
        goldens: vec![],
    };
    let model = ContainerModel { client: RefCell::new(client), next_sequence: Cell::new(1), info, binding_hash: response.binding_hash, managed: RefCell::new(managed) };
    Ok(MaterializedContainer { model, t0_tai_ns: epoch_tai_ns })
}

// --------------------------------------------------------------------------------------
// Classification (GMAT-free): can this instance be bound at all, and to what kind?
// --------------------------------------------------------------------------------------

/// M25.2b: [`GmatSystemSpec::consume_framed_port`]'s own resolved companion data --
/// [`resolve_gmat_command_port`]'s return value, boxed as one unit (see that field's own doc
/// comment for why: three separate plain fields here tripped clippy's `large_enum_variant` a
/// second time).
#[derive(Debug, Clone, PartialEq)]
pub struct GmatConsumeFramedResolution {
    pub codec: PacketCodec,
    /// The codec field's own `name` (`DecodedPacket.fields` is keyed by this, not by `target`).
    pub packet_field: String,
    /// The codec field's own `target` -- one of [`GMAT_WRITABLE_PARAMETERS`].
    pub target: String,
}

/// Parsed, not-yet-materialized parameters for a `"gmat."`-dispatched `SystemDefinition`.
/// Building this touches no GMAT state at all -- every field here is pure data read from
/// declared `Parameter`s, so [`classify_binding`] can run (and refuse) without a live `Gmat`
/// handle, engine lock, or GMAT install.
#[derive(Debug, Clone, Default)]
pub struct GmatSystemSpec {
    pub central_body: String,
    pub gravity_file: String,
    pub gravity_degree: i32,
    pub gravity_order: i32,
    pub point_masses: Vec<String>,
    pub relativistic_correction: bool,
    pub golden_ref: Option<String>,
    pub spacecraft_real: BTreeMap<String, f64>,
    pub spacecraft_str: BTreeMap<String, String>,
    /// M18.3 (`docs/open-questions.md` question 126): `"port.emit"` (port name) +
    /// `"port.emit_output"` (one of this model's own named outputs, [`gmat_sys::model::
    /// OUTPUT_RMAG`]/[`gmat_sys::model::OUTPUT_CD`]) -- required together, same pairing rule as
    /// [`ConstantAccelSpec::emit`]. Threaded straight into [`gmat_sys::model::GmatPortConfig::
    /// emit`] by [`materialize_gmat`].
    pub emit: Option<(String, String)>,
    /// M18.3: `"port.consume"` (port name) + `"port.consume_parameter"` (a declared-writable
    /// GMAT field name -- see [`GMAT_WRITABLE_PARAMETERS`]) -- required together. Threaded
    /// straight into [`gmat_sys::model::GmatPortConfig::consume`] by [`materialize_gmat`].
    pub consume: Option<(String, String)>,
    /// M25.2b (`docs/sil-plan.md`'s M25 milestone, migrating the demo's drag-sail command to a
    /// ground-issued telecommand; `docs/open-questions.md` questions 126/137/149): `"port.
    /// consume_framed"` -- the FRAMED IN port name this instance decodes the latest CCSDS
    /// telecommand from, every step (mirrors [`GmatSystemSpec::consume`]'s own "last message
    /// this step" contract, but for a FRAMED/CCSDS message instead of a bare SIGNAL one -- see
    /// `crate::drm::gmat_command`'s own module doc comment for the full "decode here, hand the
    /// value to the SIGNAL apply path" account). `None` (every fixture before M25.2b) is a
    /// strict no-op. Resolved (not parsed) alongside it, by [`classify_binding`]'s own `Gmat`
    /// arm via [`resolve_gmat_command_port`]: `consume_framed_codec`, and which of that codec's
    /// own declared fields actually supplies the value -- `consume_framed_packet_field` (the
    /// field's own `name`, used to look the decoded value up by) and `consume_framed_target`
    /// (the field's own `target`, required to be one of [`GMAT_WRITABLE_PARAMETERS`]).
    /// **`PacketField.target` is the mapping layer here (question 149's own "declared writable
    /// parameter [...] for commands" role) -- unlike [`ConstantAccelSpec::consume_framed_field`]
    /// (M25.2), which named the target through a second, separately-declared `"port.*"`
    /// parameter instead, this binding kind reads it straight off the codec's own declared
    /// mapping.** The resolved codec/packet-field/target are boxed together as one
    /// [`GmatConsumeFramedResolution`] -- not merely `consume_framed_codec` alone, the way
    /// `ConstantAccelSpec`'s own FRAMED-port fields are boxed (M25.2's own report) -- because
    /// three more plain fields here, on top of that, tripped clippy's `large_enum_variant`
    /// again, this time on `Classification::Model(BindingPlan)` rather than `BindingPlan`
    /// itself (measured, not assumed: `cargo clippy -p av-kernel --all-targets -- -D warnings`
    /// failed with "large size difference between variants ... Model(BindingPlan) ... at least
    /// 464 bytes" before this consolidation; clean after). See that type's own doc comment for
    /// what it carries. `consume_framed_port` alone stays a plain, unboxed `Option<String>`
    /// (small, and the one signal [`classify_binding`] needs *before* resolution has run at
    /// all, to decide whether to call [`resolve_gmat_command_port`] in the first place) --
    /// mirrors `ConstantAccelSpec::consume_framed_port`'s own identical unboxed treatment.
    pub consume_framed_port: Option<String>,
    pub consume_framed: Option<Box<GmatConsumeFramedResolution>>,
    /// M25.2b: `"port.ack_framed"` -- the FRAMED OUT port name this instance sends one ack
    /// telemetry packet on (mirrors [`ConstantAccelSpec::ack_framed_port`]'s own convention and
    /// codec shape, [`command::command_ack_packet_codec`](super::command::command_ack_packet_codec))
    /// immediately after it actually applies a decoded `consume_framed` command -- see
    /// `crate::drm::gmat_command`'s own module doc comment, "Ack telemetry", for exactly why
    /// this exists (`crate::drm::executor::run_shared_group`'s own applied-commands drain
    /// assumes an applied command's mere presence already proves an ack was sent). Required
    /// together with `consume_framed_port`; `None` is a strict no-op, same as every other
    /// optional FRAMED port this spec declares. Resolved via [`resolve_constant_accel_ack_port`]
    /// unchanged (that resolver takes only `sys`/`instance`/`port_name`, nothing
    /// `ConstantAccelSpec`-specific, so it is reused verbatim rather than duplicated).
    pub ack_framed_port: Option<String>,
    pub ack_framed_codec: Option<Box<PacketCodec>>,
    /// M19.4 (`docs/open-questions.md` question 131): `"force_model.drag_model"` (a GMAT
    /// atmosphere-model object type, e.g. `"JacchiaRoberts"`) plus three weather-source
    /// companions (`"force_model.drag_historic_weather_source"`/`"...drag_predicted_weather_
    /// source"`/`"...drag_cssi_space_weather_file"`) -- all four required together, same
    /// pairing rule as [`GmatSystemSpec::emit`]/[`GmatSystemSpec::consume`]. `None` (all four
    /// absent) means no atmospheric drag at all -- the shape every GMAT-bound fixture in this
    /// crate declared before this task, unchanged. See [`materialize_gmat`]'s own doc comment
    /// for exactly how this is threaded into a real `DragForce`/atmosphere-model object pair.
    pub drag_model: Option<String>,
    /// Companion to [`GmatSystemSpec::drag_model`]; empty/unused when that is `None`.
    pub drag_historic_weather_source: String,
    /// Companion to [`GmatSystemSpec::drag_model`]; empty/unused when that is `None`.
    pub drag_predicted_weather_source: String,
    /// Companion to [`GmatSystemSpec::drag_model`]; empty/unused when that is `None`. A bare
    /// filename (e.g. `"SpaceWeather-All-v1.2.txt"`), resolved by GMAT's own `FileManager`
    /// against `ATMOSPHERE_PATH` (`GMAT R2026a/bin/api_startup_file.txt`) -- the packaged
    /// CSSI space-weather file that ships with this repository's own GMAT install, never
    /// downloaded.
    pub drag_cssi_space_weather_file: String,
}

/// The only GMAT spacecraft fields a `"port.consume_parameter"` may name (M18.3, question 126):
/// a SIGNAL-commanded write reaches real GMAT state through `gmat_sys::DerivativeModel::
/// set_real_parameter`, which forwards `name` to `GmatBase::SetField` with no allowlist of its
/// own (that crate's job is to apply whatever it is told, not to judge it) -- this module is the
/// one load-time boundary that decides which fields a DRM is allowed to command over a port at
/// all, exactly the same "refuse, never guess" contract every other `parse_gmat_spec`/
/// `parse_constant_accel_spec` field already follows. `Cd` (drag coefficient) is this task's own
/// required test case (a "drag-coefficient command", question 126's own decision text); the list
/// is a `const`, not a single hardcoded string comparison, so a future task can extend it in one
/// place without touching the refusal logic itself.
pub const GMAT_WRITABLE_PARAMETERS: &[&str] = &["Cd"];

/// Parsed parameters for the native `ConstantAccelModel` placeholder.
#[derive(Debug, Clone, Default)]
pub struct ConstantAccelSpec {
    pub a: [f64; 3],
    pub frame_id: String,
    /// `[px, py, pz, vx, vy, vz]`, SI metres / metres-per-second -- the instance's initial
    /// physical state at `Scenario.start_tai_ns`. There is no unit or time-scale boundary to
    /// cross for this native binding kind (unlike the GMAT path), so this is read directly.
    ///
    /// **M21.3 (question 141): `Vec<f64>`, not a fixed `[f64; 6]` any more.** Built by
    /// [`parse_constant_accel_spec`] straight from how many of the six `"state.*"` parameters
    /// an instance actually declares -- all six (length 6, the double-integrator physical
    /// state) or none at all (length 0, `ConstantAccelSpec::default()`'s own empty `Vec`,
    /// never six hidden zeros); declaring some but not all six is refused
    /// ([`DrmError::MissingParameter`]), the same as before this task. This is the honest
    /// width [`materialize_constant_accel`] builds [`ConstantAccelModel::dim`] from, and the
    /// width [`classify_binding`]'s own load-time check cross-validates against the instance's
    /// declared state space.
    pub x0_si: Vec<f64>,
    /// `"port.emit"` (the port name) plus `"port.emit_value"` (the constant SIGNAL value sent
    /// every step) -- both declared together or not at all (M14.1, question 109). See
    /// [`ConstantAccelModel`]'s own doc comment.
    pub emit: Option<(String, f64)>,
    /// `"port.consume"` -- the port name this instance drains its `Inbox` for every step
    /// (M14.1, question 109). See [`ConstantAccelModel`]'s own doc comment.
    pub consume_port: Option<String>,
    /// M19.4 (`docs/open-questions.md` question 131): `"condition.threshold_m"` +
    /// `"condition.mode"` (`"above"`/`"below"`), declared together with both `port.consume` and
    /// `port.emit`/`port.emit_value` -- turns [`ConstantAccelSpec::emit`] from "sent
    /// unconditionally, every step" (M14.1's own contract, still exactly what happens when this
    /// is `None`) into "sent exactly once, the first step `consume_port`'s decoded value crosses
    /// this threshold in the declared direction" (edge-triggered and latched: never re-emitted
    /// after the first trigger of one materialization -- see [`ConstantAccelModel::
    /// step_with_ports`]). This is the "native controller instance that commands a ... change ...
    /// when a declared range condition ... holds" the lead's decision (question 131) asks for:
    /// the condition is declared data (a hashed `SystemDefinition`/`SystemInstance` parameter),
    /// never a hard-coded Rust threshold.
    pub condition: Option<RangeCondition>,
    /// M25.1 (`docs/sil-plan.md`'s M25 milestone: "the demo grows a ground instance connected
    /// to the flight instance"): `"port.emit_framed"` -- the FRAMED OUT port name this
    /// (6-dimensional) instance additionally broadcasts its own propagated Cartesian position
    /// (`state[0..3]`, metres, x/y/z) on, every step, as one CCSDS space packet per
    /// [`crate::drm::ground::ground_tm_packet_codec`]'s own field convention -- reusing the
    /// existing FRAMED-CCSDS machinery (`crate::codec`) a ground station's own `tm_in` port
    /// decodes, rather than inventing a second wire shape. `None` (every fixture before M25.1)
    /// is a strict no-op: unchanged behaviour. Declared, hashed configuration, exactly like
    /// [`ConstantAccelSpec::emit`]/`.consume_port` above.
    pub emit_framed_port: Option<String>,
    /// The declared `PacketCodec` [`super::binding::classify_binding`]'s own `ModelKind::Native`
    /// arm resolved for [`ConstantAccelSpec::emit_framed_port`] (via [`resolve_sensor_output`],
    /// the same declared-`packet_codecs`/`.ports` resolution every FRAMED-emitting native model
    /// in this crate already uses) -- `None` exactly when `emit_framed_port` is `None`; `Some`
    /// only after that resolution has actually run and validated the codec's required `x`/`y`/`z`
    /// fields. Not itself parsed from a `"port.*"` parameter (a `PacketCodec` cannot round-trip
    /// through a single `Parameter`); kept as a spec field (rather than a separate constructor
    /// argument threaded through `crate::registry::ModelRegistry::construct_native`) so this
    /// binding kind's own re-materialization path (`super::fault::apply_dynamics_fault`'s
    /// `BindingPlan::ConstantAccel` arm, `executor::materialize_plan_at_boundary`) needs no
    /// change at all: both already clone/reuse the whole `ConstantAccelSpec` unchanged except for
    /// the one numeric field a fault actually retargets.
    pub emit_framed_codec: Option<Box<PacketCodec>>,
    /// M25.2 (`docs/sil-plan.md`'s M25 milestone, "Job 1": the flight-side FRAMED consume no
    /// prior task built -- see [`ConstantAccelModel`]'s own doc comment for the full account):
    /// `"port.consume_framed"` -- the FRAMED IN port name this instance decodes the latest CCSDS
    /// telecommand from, every step (mirroring [`ConstantAccelSpec::consume_port`]'s own "last
    /// message this step" contract, but for a FRAMED/CCSDS message instead of a bare SIGNAL one).
    /// `None` (every fixture before M25.2) is a strict no-op: unchanged behaviour. Declared,
    /// hashed configuration, exactly like `emit_framed_port` above.
    pub consume_framed_port: Option<String>,
    /// `"port.consume_framed_field"` -- which of [`CONSTANT_ACCEL_WRITABLE_PARAMETERS`] the
    /// decoded packet's own `"value"` field is written into. Required together with
    /// `consume_framed_port`, refused (typed) if it names anything outside that allowlist --
    /// mirrors `GmatSystemSpec::consume`'s own `port.consume_parameter`/`GMAT_WRITABLE_
    /// PARAMETERS` pairing exactly, one binding kind over.
    pub consume_framed_field: Option<String>,
    /// The declared `PacketCodec` [`classify_binding`]'s own `ModelKind::Native` arm resolved
    /// for [`ConstantAccelSpec::consume_framed_port`] (via [`resolve_constant_accel_command_
    /// port`]) -- `None` exactly when `consume_framed_port` is `None`. See `emit_framed_codec`'s
    /// own doc comment for why this lives on the spec rather than a separate constructor
    /// argument.
    pub consume_framed_codec: Option<Box<PacketCodec>>,
    /// M25.2: `"port.ack_framed"` -- the FRAMED OUT port name this instance sends one ack
    /// telemetry packet on ([`crate::drm::command::command_ack_packet_codec`]'s own field
    /// convention) immediately after it actually applies a decoded `consume_framed` command --
    /// "acknowledged by the flight software's telemetry" (`docs/sil-plan.md`'s M25 milestone),
    /// not merely inferred from the applied command. Required together with `consume_framed_port`
    /// (an ack with nothing to acknowledge is meaningless); `None` is a strict no-op, same as
    /// every other optional FRAMED port this spec declares.
    pub ack_framed_port: Option<String>,
    /// The declared `PacketCodec` [`classify_binding`] resolved for [`ConstantAccelSpec::
    /// ack_framed_port`] (via [`resolve_constant_accel_ack_port`]) -- `None` exactly when
    /// `ack_framed_port` is `None`.
    pub ack_framed_codec: Option<Box<PacketCodec>>,
}

/// M25.2: the only `ConstantAccelModel` field a `port.consume_framed_field` telecommand may
/// name -- mirrors `GMAT_WRITABLE_PARAMETERS`'s own single-entry-today, `const`-not-a-literal
/// shape exactly (`crate::drm::binding::GMAT_WRITABLE_PARAMETERS`'s own doc comment). `"accel_
/// scale"` multiplies the instance's own declared constant acceleration vector [`ConstantAccelSpec
/// ::a`] uniformly (`ConstantAccelModel::derivatives`), a real, physically meaningful effect on
/// the propagated arc -- the native-model analogue of a GMAT-bound instance's own commandable
/// `Cd` (which scales a *computed* drag force; this scales the one force this model has).
pub const CONSTANT_ACCEL_WRITABLE_PARAMETERS: &[&str] = &["accel_scale"];

/// See [`ConstantAccelSpec::condition`]'s own doc comment.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RangeCondition {
    pub threshold_m: f64,
    /// `true` ("above"): fires the first step the consumed value is `>= threshold_m`.
    /// `false` ("below"): fires the first step the consumed value is `<= threshold_m`.
    pub above: bool,
}

/// Parsed, not-yet-connected parameters for a `BINDING_KIND_CONTAINER` instance (M13.2,
/// question 107) -- see the module doc comment's "Container (lockstep) binding" section for
/// the `"container."` parameter vocabulary this is built from. Building this touches no
/// network at all (mirrors [`GmatSystemSpec`]/[`ConstantAccelSpec`]'s own "classification
/// touches nothing live" contract) -- only [`materialize_container`] connects and `Bind`s.
#[derive(Debug, Clone)]
pub struct ContainerSpec {
    pub address: String,
    pub tls: bool,
    pub ca_file: Option<String>,
    pub client_cert: Option<String>,
    pub client_key: Option<String>,
    /// Names an entry in `Scenario.seeds`; resolved to an actual `u64` by
    /// `crate::drm::executor`, not here (this module has no `Scenario` in scope) -- see
    /// [`DrmError::UnknownContainerSeed`].
    pub seed_key: String,
    /// M15.3 (question 118): `ContainerBinding.image` -- when set, this instance is bound
    /// through the Docker image-lifecycle path ([`materialize_container`] pulls and runs it via
    /// [`ManagedContainer`]) instead of M13.2's `container.address`-only
    /// already-running-process path. Read straight off `Binding.config`'s own `ContainerBinding`
    /// message (`classify_binding`), never from a `"container.*"` parameter -- unlike
    /// `address`/`tls`/... above, `image`/`image_digest`/`command`/`port_endpoints` are exactly
    /// the fields `proto/altavista/v1/system.proto`'s own `ContainerBinding` doc comment already
    /// shapes for this ("Port name -> transport endpoint the kernel connects"), so there is no
    /// reason to duplicate them into the parameter vocabulary the way `address`/`tls` were.
    /// Mutually exclusive with `address` -- [`classify_binding`]/`parse_container_spec` refuse a
    /// spec declaring both (`DrmError::InvalidBinding`).
    pub image: Option<String>,
    /// `ContainerBinding.image_digest` -- required whenever `image` is set (`docker pull
    /// <image>@<image_digest>`, question 118's "pulled by digest"). Also folded into the
    /// running container's own `IMAGE_DIGEST` environment variable
    /// ([`materialize_container`]'s own doc comment), which `services/lockstep-ref`'s own
    /// `Bind` handler folds into `LockstepBindResponse.binding_hash` -- question 118's
    /// "`binding_hash` includes the digest."
    pub image_digest: Option<String>,
    /// `ContainerBinding.command` -- overrides the image's own `ENTRYPOINT`/`CMD` when
    /// non-empty; forwarded to `docker run` verbatim.
    pub command: Vec<String>,
    /// `ContainerBinding.port_endpoints` -- published on loopback alongside the lockstep control
    /// port (`docker run -p 127.0.0.1::<port>` per entry, `av_lockstep::docker::ManagedContainer
    /// ::pull_and_run`'s own `extra_port_endpoints`) so a real (non-fixture) container image's
    /// declared transport ports are reachable the same way its control port is -- question 118's
    /// "run with the port endpoint mapped." Already a `BTreeMap` (prost's own `btree_map`
    /// codegen for this field, ADR-004 determinism), so no conversion is needed here.
    pub port_endpoints: BTreeMap<String, String>,
    /// `"container.control_port"` (default `50070`, matching `services/lockstep-ref`'s own
    /// `Dockerfile EXPOSE`) -- the container-internal TCP port the `LockstepService` gRPC server
    /// listens on; `docker run -p 127.0.0.1::<control_port>` publishes it, and the host port
    /// Docker actually picked (`docker port`) is what this instance's `LockstepClient` connects
    /// to. Only meaningful when `image` is set; ignored (and irrelevant) for the
    /// `container.address` path, where the caller already names a full `host:port`.
    pub control_port: u16,
    /// M23.4: `"container.sysctl.<name>"` (e.g. `"container.sysctl.fs.mqueue.msg_max"` ->
    /// `"256"`) -- extra `docker run --sysctl <name>=<value>` flags for the Docker
    /// image-lifecycle path only (mirrors `control_port`'s own "only meaningful when `image`
    /// is set" note). Not a `ContainerBinding` proto field: `proto/**` is read-only to this
    /// task, and this is exactly the kind of deployment-specific tuning knob `"container.*"`
    /// parameters (not the hashed `ContainerBinding` message) already exist for -- the pinned
    /// cFS image needs `fs.mqueue.msg_max`/`fs.mqueue.msgsize_max` raised above this host's own
    /// default or cFE's core `CFE_SB`/`CFE_EVS` pipes fail `OS_QueueCreate` at boot (confirmed
    /// by actually running the image without them -- see
    /// `av_lockstep::docker::ManagedContainer::pull_and_run`'s own doc comment for the exact
    /// error). Threaded straight to that function's `extra_sysctls` parameter.
    pub docker_sysctls: BTreeMap<String, String>,
}
impl Default for ContainerSpec {
    fn default() -> Self {
        ContainerSpec {
            address: String::new(),
            tls: false,
            ca_file: None,
            client_cert: None,
            client_key: None,
            seed_key: String::new(),
            image: None,
            image_digest: None,
            command: Vec::new(),
            port_endpoints: BTreeMap::new(),
            control_port: 50070,
            docker_sysctls: BTreeMap::new(),
        }
    }
}
impl ContainerSpec {
    /// `Some((ca, cert, key))` iff all three mTLS paths are present -- `classify_binding`
    /// already refuses a `container.tls = true` spec missing any of them, so a caller with a
    /// `ContainerSpec` in hand where `tls` is `true` can treat `None` here as an internal
    /// invariant violation, not a fresh user error (see [`materialize_container`]'s own use).
    fn tls_paths(&self) -> Option<(&str, &str, &str)> {
        match (&self.ca_file, &self.client_cert, &self.client_key) {
            (Some(ca), Some(cert), Some(key)) => Some((ca, cert, key)),
            _ => None,
        }
    }
}

/// **Unchanged in shape by M13.2** (still exactly the two variants it had before question
/// 107): `crate::drm::fault::apply_dynamics_fault` and `crate::registry::ModelRegistry`/
/// `ModelHandle`/`AnyModel` (neither owned by this task -- see this module's own "
/// `ModelRegistry` is the sole constructor" doc section) both match on `BindingPlan`
/// exhaustively, so a third variant here would be a breaking change to two files this task
/// may not edit. [`Classification`], not this enum, is what carries a
/// `BINDING_KIND_CONTAINER` result out of [`classify_binding`] instead -- see that type's own
/// doc comment.
/// `#[allow(clippy::large_enum_variant)]` (M21.3, question 141): `ConstantAccelSpec::x0_si`
/// moved from a fixed `[f64; 6]` (48 bytes, always inline) to a `Vec<f64>` (24 bytes, the
/// honest representation of a width that can legitimately be zero -- see `CONSTANT_ACCEL_
/// STATE_DIM`'s own doc comment), which shrank this variant enough to cross clippy's
/// large-enum-variant size-ratio threshold against `GmatSystemSpec`. Boxing `GmatSystemSpec`
/// instead (clippy's own suggested fix) would touch all ~25 existing construction/match sites
/// across this module, `executor.rs`, `fault.rs` and this crate's own test suite for a purely
/// cosmetic memory-layout concern -- this enum is a short-lived, per-instance classification
/// value (`classify_binding`'s own return type), never stored at any scale where the extra
/// stack/move cost would matter. Disclosed here rather than silently suppressed elsewhere.
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum BindingPlan {
    Gmat(GmatSystemSpec),
    ConstantAccel(ConstantAccelSpec),
    /// M22.1b (`docs/open-questions.md` questions 151/152): a `"attitude."`-dispatched
    /// instance's parsed, validated [`AttitudeWheelsSpec`] (`crate::drm::attitude::
    /// parse_attitude_spec`) -- see [`classify_binding`]'s own `crate::registry::ModelKind::
    /// Attitude` arm for exactly what "validated" means at this point (state-space dimension
    /// and wheel-momentum-unit agreement, both checked by constructing and discarding a real
    /// `crate::drm::attitude::AttitudeWheelsModel`).
    Attitude(AttitudeWheelsSpec),
    /// M22.2b (`docs/open-questions.md` questions 142/149/151/152): a `"startracker."`-
    /// dispatched instance's parsed, validated [`StarTrackerSpec`] (`crate::drm::sensors::
    /// parse_star_tracker_spec`) -- see [`classify_binding`]'s own `crate::registry::ModelKind::
    /// StarTracker` arm for exactly what "validated" means (a declared codec/port pairing
    /// resolved by `resolve_sensor_output`, plus the codec's own required-field presence,
    /// checked by constructing and discarding a real `crate::drm::sensors::StarTrackerModel`).
    StarTracker(StarTrackerSpec),
    /// The IMU counterpart of [`BindingPlan::StarTracker`] -- a `"imu."`-dispatched instance's
    /// parsed, validated [`ImuSpec`].
    Imu(ImuSpec),
    /// M22.4: an `"attctrl."`-dispatched instance's parsed, validated [`AttitudeControllerSpec`]
    /// (`crate::drm::controller::parse_attitude_controller_spec`) -- see [`classify_binding`]'s
    /// own `crate::registry::ModelKind::AttitudeController` arm for exactly what "validated"
    /// means (a declared star tracker/IMU/wheel-torque-command codec/port trio resolved by
    /// `resolve_controller_ports`, checked by constructing and discarding a real
    /// `crate::drm::controller::AttitudeControllerModel`).
    Controller(AttitudeControllerSpec),
    /// M25.1 (`docs/sil-plan.md`'s M25 milestone: "ground segment as a system"): a
    /// `"ground."`-dispatched instance's parsed, validated [`GroundStationSpec`] (`crate::drm::
    /// ground::parse_ground_station_spec`) -- see [`classify_binding`]'s own `crate::registry::
    /// ModelKind::Ground` arm for exactly what "validated" means (a declared telemetry-in/
    /// telecommand-out codec/port pair resolved by `resolve_ground_ports`, plus both codecs' own
    /// required-field presence, checked by constructing and discarding a real `crate::drm::
    /// ground::GroundStationModel`).
    GroundStation(GroundStationSpec),
}

/// [`classify_binding`]'s actual return shape (M13.2, question 107): a `BINDING_KIND_MODEL`
/// instance still classifies to a [`BindingPlan`] (unchanged, see that type's own doc
/// comment on why it could not simply grow a third variant), and a `BINDING_KIND_CONTAINER`
/// instance now classifies to a [`ContainerSpec`] alongside it, in the same
/// `Result<Classification, DrmError>` -- one function, one call site
/// (`crate::drm::executor::execute`'s Pass 1), still refusing every other binding kind with
/// the same typed [`DrmError::UnsupportedBinding`] as before.
#[derive(Debug, Clone)]
pub enum Classification {
    Model(BindingPlan),
    Container(ContainerSpec),
}

pub fn effective_parameters(sys: &SystemDefinition, instance: &SystemInstance) -> BTreeMap<String, Parameter> {
    let mut out = BTreeMap::new();
    for p in &sys.parameters {
        out.insert(p.name.clone(), p.clone());
    }
    // SystemInstance.parameter_overrides wins by name -- both are fields of a hashed message
    // (SystemDefinition / SosConfiguration respectively), never a profile (question 11).
    for p in &instance.parameter_overrides {
        out.insert(p.name.clone(), p.clone());
    }
    out
}

fn parse_gmat_spec(context: &str, params: &BTreeMap<String, Parameter>) -> Result<GmatSystemSpec, DrmError> {
    let mut spec = GmatSystemSpec::default();
    // M18.3 (question 126): collected across the loop, paired/validated once it ends -- same
    // shape as `parse_constant_accel_spec`'s own `emit_port`/`emit_value` locals.
    let mut emit_port: Option<String> = None;
    let mut emit_output: Option<String> = None;
    let mut consume_port: Option<String> = None;
    let mut consume_parameter: Option<String> = None;
    // M25.2b: see GmatSystemSpec::consume_framed_port/.ack_framed_port's own doc comments. Both
    // codecs (and consume_framed's own packet-field/target pair) are resolved later, by
    // classify_binding, not parsed here.
    let mut consume_framed_port: Option<String> = None;
    let mut ack_framed_port: Option<String> = None;
    // M19.4 (question 131): see GmatSystemSpec::drag_model's own doc comment.
    let mut drag_model: Option<String> = None;
    let mut drag_historic_weather_source: Option<String> = None;
    let mut drag_predicted_weather_source: Option<String> = None;
    let mut drag_cssi_space_weather_file: Option<String> = None;
    for (name, p) in params {
        if let Some(field) = name.strip_prefix("force_model.") {
            match field {
                "central_body" => spec.central_body = p.string_value.clone(),
                "gravity_file" => spec.gravity_file = p.string_value.clone(),
                "gravity_degree" => spec.gravity_degree = p.value.round() as i32,
                "gravity_order" => spec.gravity_order = p.value.round() as i32,
                "point_masses" => spec.point_masses = p.string_value.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect(),
                "relativistic_correction" => spec.relativistic_correction = p.value != 0.0,
                "golden_ref" => spec.golden_ref = Some(p.string_value.clone()),
                "drag_model" => drag_model = Some(p.string_value.clone()),
                "drag_historic_weather_source" => drag_historic_weather_source = Some(p.string_value.clone()),
                "drag_predicted_weather_source" => drag_predicted_weather_source = Some(p.string_value.clone()),
                "drag_cssi_space_weather_file" => drag_cssi_space_weather_file = Some(p.string_value.clone()),
                _ => return Err(DrmError::UnknownParameter { context: context.to_string(), name: name.clone() }),
            }
        } else if let Some(field) = name.strip_prefix("spacecraft.") {
            if p.string_value.is_empty() {
                spec.spacecraft_real.insert(field.to_string(), p.value);
            } else {
                spec.spacecraft_str.insert(field.to_string(), p.string_value.clone());
            }
        } else if name.starts_with("output.") {
            // See the module doc comment's "Parameter vocabulary" section: an `"output.<name>"`
            // declaration names no `GmatSystemSpec` field -- `crate::drm::executor::
            // declared_outputs` reads it straight off the real `SystemDefinition` -- so this
            // allowlist simply skips it rather than refusing it as unknown.
        } else if let Some(field) = name.strip_prefix("port.") {
            // M18.3, question 126: closes `parse_gmat_spec`'s own former blanket refusal of
            // every `"port.*"` parameter (the `_ =>` arm below, unconditionally
            // `DrmError::UnknownParameter` through M18.2 -- see `drms/README.md`'s "What is NOT
            // here" section for the exact refusal this replaces). Same four-name vocabulary
            // `parse_constant_accel_spec` uses for its own `"port.emit"`/`"port.consume"`, plus
            // two GMAT-specific companions (`"port.emit_output"`/`"port.consume_parameter"`)
            // this binding kind needs that the native placeholder does not: a GMAT-bound
            // instance's emitted value is always one of its own *live* named outputs (never a
            // caller-supplied constant like `ConstantAccelSpec::emit`'s own `"port.emit_value"`),
            // and its accepted value must land on a specific, declared-writable GMAT field.
            match field {
                "emit" => emit_port = Some(p.string_value.clone()),
                "emit_output" => emit_output = Some(p.string_value.clone()),
                "consume" => consume_port = Some(p.string_value.clone()),
                "consume_parameter" => consume_parameter = Some(p.string_value.clone()),
                // M25.2b: see GmatSystemSpec::consume_framed_port/.ack_framed_port's own doc
                // comments.
                "consume_framed" => consume_framed_port = Some(p.string_value.clone()),
                "ack_framed" => ack_framed_port = Some(p.string_value.clone()),
                _ => return Err(DrmError::UnknownParameter { context: context.to_string(), name: name.clone() }),
            }
        } else {
            return Err(DrmError::UnknownParameter { context: context.to_string(), name: name.clone() });
        }
    }
    if spec.central_body.is_empty() {
        return Err(DrmError::MissingParameter { context: context.to_string(), name: "force_model.central_body".to_string() });
    }
    if spec.gravity_file.is_empty() {
        return Err(DrmError::MissingParameter { context: context.to_string(), name: "force_model.gravity_file".to_string() });
    }
    if !spec.spacecraft_str.contains_key("CoordinateSystem") {
        return Err(DrmError::MissingParameter { context: context.to_string(), name: "spacecraft.CoordinateSystem".to_string() });
    }
    if !spec.spacecraft_str.contains_key("DisplayStateType") {
        return Err(DrmError::MissingParameter { context: context.to_string(), name: "spacecraft.DisplayStateType".to_string() });
    }
    // Question 128, M19.1 (ADR-002's fourth amendment): `spacecraft.CoordinateSystem` is a pure
    // label -- the state `gmat_sys::model::GmatModel` actually reads back is always this
    // instance's own integration frame (`{central_body}MJ2000Eq`, GMAT's raw internal
    // propagation buffer, `crates/gmat-sys/shim/gmatffi.cpp::gmatffi_model_state`'s own
    // `psm->GetState()->GetState()`), never whatever `CoordinateSystem` the DRM declared. A
    // declared frame that is neither the integration frame nor one this registry can actually
    // realize through `executor::convert_gmat_trajectory_to_declared_frame` (GMAT's own
    // `CoordinateConverter::Convert`, over exactly the `{body}{ICRF|MJ2000Eq|MJ2000Ec|
    // BodyFixed}` vocabulary `executor::body_axes_suffix` recognizes) is refused here, at load,
    // before any GMAT call -- never silently mislabelled. See `DrmError::
    // UnsupportedCoordinateSystem`'s own doc comment for the history (this refused *every*
    // non-integration-frame value, unconditionally, before the `convert` capability existed).
    let declared_frame = spec.spacecraft_str.get("CoordinateSystem").expect("checked just above").clone();
    let integration_frame = format!("{}MJ2000Eq", spec.central_body);
    if declared_frame != integration_frame && crate::drm::executor::body_axes_suffix(&declared_frame).is_none() {
        return Err(DrmError::UnsupportedCoordinateSystem { context: context.to_string(), declared: declared_frame, integration_frame });
    }
    match (emit_port, emit_output) {
        (Some(port), Some(output)) => {
            if output != gmat_sys::model::OUTPUT_RMAG && output != gmat_sys::model::OUTPUT_CD {
                return Err(DrmError::UnknownParameter {
                    context: context.to_string(),
                    name: format!(
                        "port.emit_output={output:?} (must be one of this model's own named outputs: {:?}, {:?})",
                        gmat_sys::model::OUTPUT_RMAG,
                        gmat_sys::model::OUTPUT_CD
                    ),
                });
            }
            spec.emit = Some((port, output));
        }
        (Some(_), None) => return Err(DrmError::MissingParameter { context: context.to_string(), name: "port.emit_output".to_string() }),
        (None, Some(_)) => return Err(DrmError::MissingParameter { context: context.to_string(), name: "port.emit".to_string() }),
        (None, None) => {}
    }
    match (consume_port, consume_parameter) {
        (Some(port), Some(param)) => {
            if !GMAT_WRITABLE_PARAMETERS.contains(&param.as_str()) {
                return Err(DrmError::UnknownParameter {
                    context: context.to_string(),
                    name: format!("port.consume_parameter={param:?} (not a declared writable parameter; writable: {GMAT_WRITABLE_PARAMETERS:?})"),
                });
            }
            spec.consume = Some((port, param));
        }
        (Some(_), None) => return Err(DrmError::MissingParameter { context: context.to_string(), name: "port.consume_parameter".to_string() }),
        (None, Some(_)) => return Err(DrmError::MissingParameter { context: context.to_string(), name: "port.consume".to_string() }),
        (None, None) => {}
    }
    // M25.2b: `port.consume_framed` names no companion parameter here (unlike `port.consume`'s
    // own `port.consume_parameter` pairing) -- the target field is resolved from the codec's
    // own `PacketField.target` at classification time (`resolve_gmat_command_port`), not parsed
    // from a second `"port.*"` parameter -- see GmatSystemSpec::consume_framed_port's own doc
    // comment. Declaring `port.consume` (SIGNAL) *and* `port.consume_framed` (FRAMED) together
    // is refused, typed, rather than silently letting one win: `materialize_gmat` wires both
    // onto the identical underlying `GmatPortConfig::consume` slot (which holds at most one
    // port), so declaring both would silently discard whichever a caller-invisible tie-break
    // picked.
    if spec.consume.is_some() && consume_framed_port.is_some() {
        return Err(DrmError::UnknownParameter { context: context.to_string(), name: "port.consume_framed (cannot be declared together with port.consume: both would command the same underlying GmatPortConfig::consume slot)".to_string() });
    }
    spec.consume_framed_port = consume_framed_port;
    // M25.2b: `port.ack_framed` requires `port.consume_framed` (an ack with nothing to
    // acknowledge is meaningless) -- mirrors ConstantAccelSpec's own identical pairing rule.
    if let Some(port) = ack_framed_port {
        if spec.consume_framed_port.is_none() {
            return Err(DrmError::MissingParameter { context: context.to_string(), name: "port.consume_framed (required alongside port.ack_framed: the ack has nothing to acknowledge otherwise)".to_string() });
        }
        spec.ack_framed_port = Some(port);
    }
    // M19.4 (question 131): all four drag_* fields required together, or none at all -- see
    // GmatSystemSpec::drag_model's own doc comment.
    match (drag_model, drag_historic_weather_source, drag_predicted_weather_source, drag_cssi_space_weather_file) {
        (Some(model), Some(historic), Some(predicted), Some(file)) => {
            spec.drag_model = Some(model);
            spec.drag_historic_weather_source = historic;
            spec.drag_predicted_weather_source = predicted;
            spec.drag_cssi_space_weather_file = file;
        }
        (None, None, None, None) => {}
        (model, historic, predicted, file) => {
            for (present, name) in [
                (model.is_some(), "force_model.drag_model"),
                (historic.is_some(), "force_model.drag_historic_weather_source"),
                (predicted.is_some(), "force_model.drag_predicted_weather_source"),
                (file.is_some(), "force_model.drag_cssi_space_weather_file"),
            ] {
                if !present {
                    return Err(DrmError::MissingParameter { context: context.to_string(), name: name.to_string() });
                }
            }
            unreachable!("every combination other than all-Some/all-None has at least one absent field, caught by the loop above");
        }
    }
    Ok(spec)
}

fn parse_constant_accel_spec(context: &str, params: &BTreeMap<String, Parameter>) -> Result<ConstantAccelSpec, DrmError> {
    let mut spec = ConstantAccelSpec::default();
    // M21.3 (question 141): a fixed scratch array while parsing (the "state.*" vocabulary is
    // still exactly six named fields) -- converted into `spec.x0_si`'s own honest, variable-
    // width `Vec<f64>` only once every "state.*" parameter has been seen, below (0 or 6
    // elements; a partial subset is still refused, same as before this task).
    let mut state = [0.0_f64; 6];
    let mut seen_state = [false; 6];
    let mut emit_port: Option<String> = None;
    let mut emit_value: Option<f64> = None;
    let mut condition_threshold_m: Option<f64> = None;
    let mut condition_mode: Option<String> = None;
    let mut consume_framed_port: Option<String> = None;
    let mut consume_framed_field: Option<String> = None;
    let mut ack_framed_port: Option<String> = None;
    for (name, p) in params {
        match name.as_str() {
            "accel.x" => spec.a[0] = p.value,
            "accel.y" => spec.a[1] = p.value,
            "accel.z" => spec.a[2] = p.value,
            "frame_id" => spec.frame_id = p.string_value.clone(),
            // M14.1, question 109: see ConstantAccelSpec's own doc comment.
            "port.emit" => emit_port = Some(p.string_value.clone()),
            "port.emit_value" => emit_value = Some(p.value),
            "port.consume" => spec.consume_port = Some(p.string_value.clone()),
            // M25.1: see ConstantAccelSpec::emit_framed_port's own doc comment.
            // `emit_framed_codec` is resolved later, by classify_binding, not parsed here.
            "port.emit_framed" => spec.emit_framed_port = Some(p.string_value.clone()),
            // M25.2: see ConstantAccelSpec::consume_framed_port/.ack_framed_port's own doc
            // comments. Both codecs are resolved later, by classify_binding, not parsed here.
            "port.consume_framed" => consume_framed_port = Some(p.string_value.clone()),
            "port.consume_framed_field" => consume_framed_field = Some(p.string_value.clone()),
            "port.ack_framed" => ack_framed_port = Some(p.string_value.clone()),
            // M19.4, question 131: see ConstantAccelSpec::condition's own doc comment.
            "condition.threshold_m" => condition_threshold_m = Some(p.value),
            "condition.mode" => condition_mode = Some(p.string_value.clone()),
            "state.px" => {
                state[0] = p.value;
                seen_state[0] = true;
            }
            "state.py" => {
                state[1] = p.value;
                seen_state[1] = true;
            }
            "state.pz" => {
                state[2] = p.value;
                seen_state[2] = true;
            }
            "state.vx" => {
                state[3] = p.value;
                seen_state[3] = true;
            }
            "state.vy" => {
                state[4] = p.value;
                seen_state[4] = true;
            }
            "state.vz" => {
                state[5] = p.value;
                seen_state[5] = true;
            }
            // M14.1: a native binding can now declare "output.<name>" too (the "received"
            // output ConstantAccelModel::step_with_ports populates when `port.consume` is set
            // -- see ConstantAccelModel's own doc comment), mirroring parse_gmat_spec's
            // identical handling of the same prefix (OUTPUT_PARAMETER_PREFIX's doc comment in
            // crate::drm::executor) -- this allowlist simply skips it rather than refusing it
            // as unknown, since crate::drm::executor::declared_outputs reads it straight off
            // the real SystemDefinition, not off this parsed spec.
            _ if name.starts_with("output.") => {}
            _ => return Err(DrmError::UnknownParameter { context: context.to_string(), name: name.clone() }),
        }
    }
    if spec.frame_id.is_empty() {
        return Err(DrmError::MissingParameter { context: context.to_string(), name: "frame_id".to_string() });
    }
    // M21.3 (question 141): "state.*" is now all-six-or-none, not unconditionally required --
    // an instance with an empty declared state space (`demo_ctrl`'s own shape as of this task)
    // declares none of the six and gets `spec.x0_si = vec![]` (`ConstantAccelModel::dim == 0`,
    // no physical state at all); an instance with the ordinary 6-component Cartesian state
    // space declares all six, unchanged from before this task. Declaring some but not all six
    // is still refused, by the same missing-parameter name as before.
    let seen_count = seen_state.iter().filter(|seen| **seen).count();
    match seen_count {
        0 => {}
        6 => spec.x0_si = state.to_vec(),
        _ => {
            for (i, name) in ["state.px", "state.py", "state.pz", "state.vx", "state.vy", "state.vz"].into_iter().enumerate() {
                if !seen_state[i] {
                    return Err(DrmError::MissingParameter { context: context.to_string(), name: name.to_string() });
                }
            }
            unreachable!("seen_count is strictly between 0 and 6, so at least one of the six was not seen, caught by the loop above");
        }
    }
    match (emit_port, emit_value) {
        (Some(port), Some(value)) => spec.emit = Some((port, value)),
        (Some(_), None) => return Err(DrmError::MissingParameter { context: context.to_string(), name: "port.emit_value".to_string() }),
        (None, Some(_)) => return Err(DrmError::MissingParameter { context: context.to_string(), name: "port.emit".to_string() }),
        (None, None) => {}
    }
    match (condition_threshold_m, condition_mode) {
        (Some(threshold_m), Some(mode)) => {
            let above = match mode.as_str() {
                "above" => true,
                "below" => false,
                other => return Err(DrmError::UnknownParameter { context: context.to_string(), name: format!("condition.mode={other:?} (must be \"above\" or \"below\")") }),
            };
            if spec.emit.is_none() {
                return Err(DrmError::MissingParameter { context: context.to_string(), name: "port.emit (required alongside condition.threshold_m/condition.mode: the value emitted once the condition holds)".to_string() });
            }
            if spec.consume_port.is_none() {
                return Err(DrmError::MissingParameter { context: context.to_string(), name: "port.consume (required alongside condition.threshold_m/condition.mode: the port whose decoded value the condition is evaluated against)".to_string() });
            }
            spec.condition = Some(RangeCondition { threshold_m, above });
        }
        (Some(_), None) => return Err(DrmError::MissingParameter { context: context.to_string(), name: "condition.mode".to_string() }),
        (None, Some(_)) => return Err(DrmError::MissingParameter { context: context.to_string(), name: "condition.threshold_m".to_string() }),
        (None, None) => {}
    }
    // M25.2: `port.consume_framed`/`port.consume_framed_field` required together (mirrors
    // `GmatSystemSpec`'s own `port.consume`/`port.consume_parameter` pairing) -- see
    // ConstantAccelSpec::consume_framed_port's own doc comment.
    match (consume_framed_port, consume_framed_field) {
        (Some(port), Some(field)) => {
            if !CONSTANT_ACCEL_WRITABLE_PARAMETERS.contains(&field.as_str()) {
                return Err(DrmError::UnknownParameter {
                    context: context.to_string(),
                    name: format!("port.consume_framed_field={field:?} (not a declared writable parameter; writable: {CONSTANT_ACCEL_WRITABLE_PARAMETERS:?})"),
                });
            }
            spec.consume_framed_port = Some(port);
            spec.consume_framed_field = Some(field);
        }
        (Some(_), None) => return Err(DrmError::MissingParameter { context: context.to_string(), name: "port.consume_framed_field".to_string() }),
        (None, Some(_)) => return Err(DrmError::MissingParameter { context: context.to_string(), name: "port.consume_framed".to_string() }),
        (None, None) => {}
    }
    // M25.2: `port.ack_framed` requires `port.consume_framed` (an ack with nothing to
    // acknowledge is meaningless) -- see ConstantAccelSpec::ack_framed_port's own doc comment.
    if let Some(port) = ack_framed_port {
        if spec.consume_framed_port.is_none() {
            return Err(DrmError::MissingParameter { context: context.to_string(), name: "port.consume_framed (required alongside port.ack_framed: the ack has nothing to acknowledge otherwise)".to_string() });
        }
        spec.ack_framed_port = Some(port);
    }
    Ok(spec)
}

/// Question 155: is `address` (a bare `"host:port"` string, `container.address`'s own shape)
/// a recognized loopback endpoint? Recognizes the literal string `"localhost"` (case-
/// insensitive) and any IPv4/IPv6 literal `std::net::IpAddr::is_loopback` accepts (127.0.0.0/8,
/// `::1`, optionally `[bracketed]` the way a `"host:port"` string spells an IPv6 host) --
/// **deliberately no DNS resolution**: a hostname this function does not recognize by its
/// literal spelling is treated as non-loopback and refused, never resolved and then trusted,
/// which would make the refusal depend on the resolver's answer at load time (non-deterministic
/// across hosts/runs, ADR-004) rather than on the address string itself. `port` is not
/// inspected -- only the host portion decides loopback-ness.
pub(crate) fn is_loopback_address(address: &str) -> bool {
    let host = match address.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() && port.chars().all(|c| c.is_ascii_digit()) && !port.is_empty() => host,
        _ => address,
    };
    let host = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(host);
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<std::net::IpAddr>().map(|ip| ip.is_loopback()).unwrap_or(false)
}

/// Parse a `"container."`-prefixed effective parameter set, plus the `Binding.config`
/// `ContainerBinding` message when present, into a [`ContainerSpec`] -- see the module doc
/// comment's "Container (lockstep) binding" section for the `"container."` vocabulary, and
/// [`ContainerSpec::image`]'s own doc comment for the Docker image-lifecycle path
/// (`container_binding`, M15.3, question 118). An unrecognized parameter name is
/// [`DrmError::UnknownParameter`], same as every other binding kind's own parser (question 87:
/// never silently ignored).
fn parse_container_spec(context: &str, params: &BTreeMap<String, Parameter>, container_binding: Option<&ContainerBinding>) -> Result<ContainerSpec, DrmError> {
    let mut spec = ContainerSpec::default();
    for (name, p) in params {
        if name.starts_with("output.") {
            // Same "output.<name>" declaration `parse_gmat_spec` recognizes and skips -- see
            // this module's doc comment's "Parameter vocabulary" section. A container-bound
            // instance's declared named outputs use the identical mechanism.
            continue;
        }
        let Some(field) = name.strip_prefix("container.") else {
            return Err(DrmError::UnknownParameter { context: context.to_string(), name: name.clone() });
        };
        match field {
            "address" => spec.address = p.string_value.clone(),
            "tls" => spec.tls = p.value != 0.0,
            "ca_file" => spec.ca_file = Some(p.string_value.clone()),
            "client_cert" => spec.client_cert = Some(p.string_value.clone()),
            "client_key" => spec.client_key = Some(p.string_value.clone()),
            "seed_key" => spec.seed_key = p.string_value.clone(),
            // M15.3 (question 118): the container-internal port the LockstepService gRPC
            // server listens on -- only meaningful alongside `ContainerBinding.image` (see
            // `ContainerSpec::control_port`'s own doc comment).
            "control_port" => {
                let port = p.value.round();
                if !(1.0..=65535.0).contains(&port) {
                    return Err(DrmError::UnknownParameter { context: context.to_string(), name: format!("container.control_port={port} (must be 1..=65535)") });
                }
                spec.control_port = port as u16;
            }
            // M23.4: `"container.sysctl.<name>"` -> `av_lockstep::docker::ManagedContainer::
            // pull_and_run`'s `extra_sysctls` (`ContainerSpec::docker_sysctls`'s own doc
            // comment). A string value (like `container.address`), not numeric: `docker run
            // --sysctl <name>=<value>` is always textual on the CLI, and some sysctl names
            // this mechanism might someday carry are not numeric at all.
            _ if field.starts_with("sysctl.") => {
                let sysctl_name = field.strip_prefix("sysctl.").expect("just matched the \"sysctl.\" prefix above");
                if sysctl_name.is_empty() {
                    return Err(DrmError::UnknownParameter { context: context.to_string(), name: name.clone() });
                }
                spec.docker_sysctls.insert(sysctl_name.to_string(), p.string_value.clone());
            }
            _ => return Err(DrmError::UnknownParameter { context: context.to_string(), name: name.clone() }),
        }
    }

    // M15.3 (question 118): `ContainerBinding.image` set means the Docker image-lifecycle path
    // (materialize_container pulls and runs it); empty/absent means M13.2's own
    // `container.address`-only already-running-process path. The two are mutually exclusive --
    // `container.address` would otherwise silently win or lose depending on field-check order,
    // which is exactly the kind of ambiguity this crate's own "refuse, never guess" rule exists
    // to rule out.
    if let Some(cb) = container_binding {
        if !cb.image.is_empty() {
            spec.image = Some(cb.image.clone());
            spec.image_digest = Some(cb.image_digest.clone());
            spec.command = cb.command.clone();
            spec.port_endpoints = cb.port_endpoints.clone();
        }
    }
    if spec.image.is_some() && !spec.address.is_empty() {
        return Err(DrmError::InvalidBinding { reason: format!("{context}: both ContainerBinding.image and container.address are set -- exactly one of the Docker image-lifecycle path or the already-running-process path must be chosen") });
    }
    if let Some(image) = &spec.image {
        if spec.image_digest.as_deref().unwrap_or("").is_empty() {
            return Err(DrmError::MissingParameter { context: context.to_string(), name: "ContainerBinding.image_digest".to_string() });
        }
        if spec.tls {
            return Err(DrmError::InvalidBinding { reason: format!("{context}: container.tls is set alongside ContainerBinding.image ({image:?}) -- the Docker image-lifecycle path is always plaintext loopback (question 118: \"Bind over loopback\"; ADR-003: \"plaintext loopback only inside one node under test\")") });
        }
    } else {
        if spec.address.is_empty() {
            return Err(DrmError::MissingParameter { context: context.to_string(), name: "container.address".to_string() });
        }
        // M23.4: `container.sysctl.*` only means anything on the Docker image-lifecycle path
        // (`ManagedContainer::pull_and_run`'s own `docker run`) -- the `container.address`
        // path never runs `docker run` at all (M13.2's own "already running" contract), so a
        // declared sysctl there would be silently ignored rather than doing what its name
        // implies. Refused, typed, matching this crate's own "no silent fallback" rule, the
        // same way `container.tls` alongside `image` is refused above rather than ignored.
        if !spec.docker_sysctls.is_empty() {
            return Err(DrmError::InvalidBinding {
                reason: format!("{context}: container.sysctl.* is set but ContainerBinding.image is not -- sysctls only apply to the Docker image-lifecycle path (container.address is an already-running process; docker run is never invoked for it)"),
            });
        }
    }

    // Question 155: the kernel <-> shim gRPC link is plaintext only on loopback within one
    // host (a container on the same host is that host) -- refused at load, before
    // `materialize_container` would ever dial `spec.address`. Only the `container.address`
    // (M13.2 already-running-process) path reaches this check: the Docker image-lifecycle path
    // (`spec.image.is_some()`) always connects to the `127.0.0.1:<host_port>` address Docker
    // itself published (see `materialize_container`), never a caller-supplied one, and the
    // `container.tls`-alongside-`image` combination is already refused above.
    if spec.image.is_none() && !spec.tls && !is_loopback_address(&spec.address) {
        return Err(DrmError::ContainerPlaintextNonLoopback { context: context.to_string(), address: spec.address.clone() });
    }

    if spec.seed_key.is_empty() {
        return Err(DrmError::MissingParameter { context: context.to_string(), name: "container.seed_key".to_string() });
    }
    if spec.tls && spec.tls_paths().is_none() {
        for (present, name) in [(spec.ca_file.is_some(), "container.ca_file"), (spec.client_cert.is_some(), "container.client_cert"), (spec.client_key.is_some(), "container.client_key")] {
            if !present {
                return Err(DrmError::MissingParameter { context: context.to_string(), name: name.to_string() });
            }
        }
    }
    Ok(spec)
}

/// Human-readable name for a raw `Binding.kind` wire value, for error messages. `prost`'s
/// `Enumeration` derive only gives `as_str_name` on an already-valid enum *value* (no
/// `from_i32`/`TryFrom<i32>` in this prost version), so this matches the wire value directly
/// against each variant's own `as i32` rather than round-tripping through the enum type.
fn binding_kind_display(kind: i32) -> String {
    match kind {
        k if k == BindingKind::Unspecified as i32 => BindingKind::Unspecified.as_str_name().to_string(),
        k if k == BindingKind::Model as i32 => BindingKind::Model.as_str_name().to_string(),
        k if k == BindingKind::Container as i32 => BindingKind::Container.as_str_name().to_string(),
        k if k == BindingKind::Renode as i32 => BindingKind::Renode.as_str_name().to_string(),
        k if k == BindingKind::Board as i32 => BindingKind::Board.as_str_name().to_string(),
        other => format!("<unknown BindingKind {other}>"),
    }
}

/// Classify a `SystemInstance`'s binding without touching GMAT (no `Gmat` handle needed):
/// refuse container/Renode/board/unset bindings, refuse a covariance request against a
/// declared `RelativisticCorrection` force model unless `accept_missing_stm_terms`
/// (questions 82/83), and otherwise parse the bound `SystemDefinition`'s declared parameters
/// into a [`BindingPlan`]. Pure data in, pure data out -- this is what lets the "non-model
/// binding" and "RelativisticCorrection refused" tests run with no GMAT install.
pub fn classify_binding(instance: &SystemInstance, sys: &SystemDefinition, options: &DrmOptions) -> Result<Classification, DrmError> {
    let binding: &Binding = instance.binding.as_ref().ok_or_else(|| DrmError::UnsupportedBinding { instance: instance.name.clone(), kind: "BINDING_KIND_UNSPECIFIED (no binding set)".to_string() })?;

    let context = format!("instance {:?} (system {:?})", instance.name, sys.id);
    let params = effective_parameters(sys, instance);

    // M13.2, question 107: BINDING_KIND_CONTAINER is classified (not refused) as of this
    // task -- see the module doc comment's "Container (lockstep) binding" section.
    // BINDING_KIND_RENODE/BOARD (and unset) still fall through to the blanket refusal below.
    if binding.kind == BindingKind::Container as i32 {
        // M15.3 (question 118): `Binding.config`'s own `ContainerBinding` (image/image_digest/
        // command/port_endpoints), when present -- `None` for a `Binding` whose `config` oneof
        // is unset or (a caller bug, but not this function's to police beyond ignoring it) set
        // to a different variant; `parse_container_spec` treats that identically to "no
        // ContainerBinding declared", i.e. the M13.2 `container.address` path.
        let container_binding = match &binding.config {
            Some(av_cdm::pb::binding::Config::Container(cb)) => Some(cb),
            _ => None,
        };
        return Ok(Classification::Container(parse_container_spec(&context, &params, container_binding)?));
    }

    if binding.kind != BindingKind::Model as i32 {
        return Err(DrmError::UnsupportedBinding { instance: instance.name.clone(), kind: binding_kind_display(binding.kind) });
    }

    // ADR-005 sec 1: the dispatch decision (which of the registry's kinds this
    // `dynamics_model` id names) now lives in `crate::registry::kind_for`, not as an inline
    // prefix check here -- `crate::registry`'s own module doc comment covers exactly what
    // moved and what did not. `ModelKind::Remote` has no `BindingPlan` variant yet (a
    // `"remote."`-dispatched instance's `Materialized` model, once one is actually wired up,
    // needs no GMAT/native parameter parsing at all -- it is a registry concern, not a
    // classification one), so it is refused the same way an unsupported binding kind already
    // is, rather than silently falling through to the native placeholder.
    match crate::registry::kind_for(&sys.dynamics_model) {
        crate::registry::ModelKind::Gmat => {
            let mut spec = parse_gmat_spec(&context, &params)?;
            // M25.2b (`docs/sil-plan.md`'s M25 milestone, migrating the demo's drag-sail
            // command to a ground-issued telecommand): resolve the declared FRAMED IN port/
            // codec for `spec.consume_framed_port` -- the flight-side FRAMED consume no prior
            // task built for a GMAT-bound instance. Refused, typed, at classification time,
            // mirroring `ModelKind::Native`'s own identical `emit_framed_port`/
            // `consume_framed_port` resolution blocks below.
            if let Some(port_name) = spec.consume_framed_port.clone() {
                let (codec, packet_field, target) = resolve_gmat_command_port(sys, &instance.name, &port_name)?;
                spec.consume_framed = Some(Box::new(GmatConsumeFramedResolution { codec, packet_field, target }));
            }
            // M25.2b: resolve the declared FRAMED OUT port/codec for `spec.ack_framed_port` --
            // see GmatSystemSpec::ack_framed_port's own doc comment for why this reuses
            // resolve_constant_accel_ack_port unchanged.
            if let Some(port_name) = spec.ack_framed_port.clone() {
                let codec = resolve_constant_accel_ack_port(sys, &instance.name, &port_name)?;
                spec.ack_framed_codec = Some(Box::new(codec));
            }
            // Question 128 (M19.1) refused covariance combined with a declared frame other than
            // this instance's own integration frame here, at classify time
            // (`DrmError::CovarianceFrameConversionNotSupported`): the executor's frame
            // conversion only ever rotated `TrajectorySample.mean`, never `cov`. Question 138
            // (M21.4) closes that gap -- `executor::convert_gmat_trajectory_to_declared_frame`
            // now rotates a non-empty `cov` too (`R P Rᵀ` via `Gmat::convert_with_rotation`'s
            // 6x6 Jacobian, `executor::rotate_covariance`) -- so this combination is no longer
            // refused; see `gmat_binding_accepts_covariance_combined_with_a_non_integration_
            // frame` below for the classify-time proof, and `crates/gmat-sys/tests/
            // convert_rotation.rs` for the rotation itself.
            if options.covariance && spec.relativistic_correction && !options.accept_missing_stm_terms {
                // Question 82: RelativisticCorrection::GetDerivatives fills its STM/A-matrix
                // contribution with an unconditional zero (a stub, not a physically-absent
                // term) -- refusing here, before any GMAT call, is the DRM-layer twin of
                // `gmat_sys::model::GmatModel::stm_capable`'s withholding of the same
                // capability.
                return Err(DrmError::MissingStmTermsNotAccepted { instance: instance.name.clone() });
            }
            Ok(Classification::Model(BindingPlan::Gmat(spec)))
        }
        crate::registry::ModelKind::Native => {
            let mut spec = parse_constant_accel_spec(&context, &params)?;
            // M25.1: resolve the declared FRAMED OUT port/codec for `spec.emit_framed_port`, the
            // same declared-`packet_codecs`/`.ports` resolution every FRAMED-emitting native
            // model in this crate already uses (`resolve_sensor_output`) -- refused, typed, at
            // classification time (a bad or missing declaration is a load-time error, never
            // discovered only once this instance is first stepped), mirroring `ModelKind::
            // StarTracker`'s own codec resolution below.
            if let Some(port_name) = spec.emit_framed_port.clone() {
                let (codec, resolved_port) = resolve_sensor_output(sys, &instance.name)?;
                if resolved_port != port_name {
                    return Err(DrmError::SensorPortConfiguration {
                        instance: instance.name.clone(),
                        reason: format!("port.emit_framed names port {port_name:?}, but the declared PORT_KIND_FRAMED/PORT_DIRECTION_OUT port is named {resolved_port:?}"),
                    });
                }
                for name in ["x", "y", "z"] {
                    if !codec.fields.iter().any(|f| f.name == name) {
                        return Err(DrmError::SensorPortConfiguration { instance: instance.name.clone(), reason: format!("declared FRAMED-emit PacketCodec {:?} is missing required field {name:?}", codec.id) });
                    }
                }
                spec.emit_framed_codec = Some(Box::new(codec));
            }
            // M25.2 (`docs/sil-plan.md`'s M25 milestone, "Job 1"): resolve the declared FRAMED
            // IN port/codec for `spec.consume_framed_port` -- the flight-side FRAMED consume no
            // prior task built. Refused, typed, at classification time, mirroring the
            // `emit_framed_port` block immediately above.
            if let Some(port_name) = spec.consume_framed_port.clone() {
                let codec = resolve_constant_accel_command_port(sys, &instance.name, &port_name)?;
                spec.consume_framed_codec = Some(Box::new(codec));
            }
            // M25.2: resolve the declared FRAMED OUT port/codec for `spec.ack_framed_port`.
            if let Some(port_name) = spec.ack_framed_port.clone() {
                let codec = resolve_constant_accel_ack_port(sys, &instance.name, &port_name)?;
                spec.ack_framed_codec = Some(Box::new(codec));
            }
            // M21.3 (question 141, decided by the lead, closing question 133's own escalation):
            // the guard inverts from M20.1's "a declared dimension that does not match the
            // model's fixed state_dim is a typed load error" -- the declared, resolved
            // `StateSpace` (`resolve_state_space`, question 94) is now the source of truth, and
            // `spec.x0_si.len()` (built by `parse_constant_accel_spec` from however many
            // "state.*" parameters this instance actually declared -- 0 or 6, never anything
            // else, see that function's own doc comment) is what this binding kind actually
            // configures to honour it. A disagreement between the two -- including a declared
            // width this model can never honour at all, since `spec.x0_si.len()` itself is only
            // ever 0 or 6 -- is still refused here, GMAT-free, before a `ConstantAccelModel` is
            // ever constructed, exactly like the pre-M21.3 check: `CONSTANT_ACCEL_STATE_DIM`'s
            // own doc comment has the full account of what widths this model can and cannot
            // honour and why.
            let resolved = crate::trajectory::resolve_state_space(sys).map_err(|e| DrmError::InvalidStateSpace { instance: instance.name.clone(), reason: e.to_string() })?;
            let declared_dim = resolved.components.len();
            let configured_dim = spec.x0_si.len();
            if declared_dim != configured_dim {
                return Err(DrmError::StateSpaceDimensionMismatch { instance: instance.name.clone(), declared_dim, model_state_dim: configured_dim });
            }
            Ok(Classification::Model(BindingPlan::ConstantAccel(spec)))
        }
        crate::registry::ModelKind::Remote => Err(DrmError::UnsupportedBinding { instance: instance.name.clone(), kind: format!("MODEL_KIND_REMOTE (dynamics_model {:?}; ADR-005 sec 1's remote DynamicsService constructor is registered but not yet wired into this executor)", sys.dynamics_model) }),
        // M22.1b (`docs/open-questions.md` questions 151/152, decided by the lead): the
        // `"attitude."` binding kind M22.1 built (`crate::drm::attitude`) but did not yet
        // dispatch to. `parse_attitude_spec` is the one, reused parser (no second parser written
        // for this task, per the brief); `AttitudeWheelsModel::new` is the one place both the
        // declared-state-space-dimension check and the wheel-momentum-unit check (question 151)
        // live, so this arm constructs a real model here, GMAT-free and cheap (pure Rust, no
        // engine lock needed), purely to run both checks before this instance is accepted --
        // then discards it: `Classification::Model(BindingPlan::Attitude(spec))` carries only the
        // parsed spec onward, matching every other binding kind's "classify parses and validates,
        // materialize builds the model actually used for propagation" split (`crate::registry::
        // ModelRegistry::construct_attitude` is what builds the one this run actually uses, an
        // identical `AttitudeWheelsModel::new` call from the identical spec/state-space pair).
        crate::registry::ModelKind::Attitude => {
            let spec = attitude::parse_attitude_spec(&params).map_err(|e| DrmError::InvalidAttitudeSpec { instance: instance.name.clone(), reason: e.to_string() })?;
            let resolved = crate::trajectory::resolve_state_space(sys).map_err(|e| DrmError::InvalidStateSpace { instance: instance.name.clone(), reason: e.to_string() })?;
            // The dimension check first, through the same generic, cross-binding-kind
            // `DrmError::StateSpaceDimensionMismatch` `ModelKind::Native` above already uses
            // (rather than letting `AttitudeWheelsModel::new`'s own, differently-typed
            // `AttitudeSpecError::StateSpaceDimensionMismatch` surface as a generic
            // `InvalidAttitudeSpec` here) -- a caller matching on "this instance's declared
            // state space disagreed with what its own binding configures" gets one variant to
            // match regardless of binding kind.
            let declared_dim = resolved.components.len();
            let expected_dim = crate::trajectory::ATTITUDE_WHEELS_BASE_COMPONENTS + spec.wheel_axes.len();
            if declared_dim != expected_dim {
                return Err(DrmError::StateSpaceDimensionMismatch { instance: instance.name.clone(), declared_dim, model_state_dim: expected_dim });
            }
            // Construct a real model here, GMAT-free and cheap (pure Rust, no engine lock
            // needed), purely to run `AttitudeWheelsModel::new`'s own remaining checks (question
            // 151's wheel-momentum-unit check, the inertia-tensor singularity guard) before this
            // instance is accepted -- then discard it: `Classification::Model(BindingPlan::
            // Attitude(spec))` carries only the parsed spec onward, matching every other binding
            // kind's own "classify parses and validates, materialize builds the model actually
            // used for propagation" split (`crate::registry::ModelRegistry::construct_attitude`
            // is what builds the one this run actually uses, an identical `AttitudeWheelsModel::
            // new` call from the identical spec/state-space pair).
            AttitudeWheelsModel::new(&spec, &resolved, &sys.dynamics_model).map_err(|e| DrmError::InvalidAttitudeSpec { instance: instance.name.clone(), reason: e.to_string() })?;
            // M22.4: when this instance also declares a wheel-torque-command FRAMED IN port
            // (`resolve_attitude_wheel_command_input`'s own structural detection -- `None` for
            // every attitude fixture that declares no such port, exactly `TruthBroadcastAttitude`'s
            // own "provably inert wherever nothing is connected" contract), the declared codec's
            // own wheel-torque-field count must equal this instance's own declared wheel count --
            // `controller::CommandedAttitude` forwards however many `tau_k` fields the codec
            // declares straight through as `controls`, and `AttitudeWheelsModel::derivatives`'s
            // own `debug_assert_eq!(controls.len(), n_wheels)` would otherwise be violated at run
            // time (a caller bug, not a runtime condition it recovers from) -- refused here,
            // typed, at classification time instead.
            if let Some(codec) = resolve_attitude_wheel_command_input(sys, &instance.name)? {
                if codec.fields.len() != spec.wheel_axes.len() {
                    return Err(DrmError::InvalidAttitudeSpec {
                        instance: instance.name.clone(),
                        reason: format!("declared wheel-torque-command codec {:?} carries {} field(s), but this instance declares {} wheel(s) -- the two must agree", codec.id, codec.fields.len(), spec.wheel_axes.len()),
                    });
                }
            }
            Ok(Classification::Model(BindingPlan::Attitude(spec)))
        }
        // M22.2b (`docs/open-questions.md` questions 142/149/151/152, decided by the lead): the
        // `"startracker."` binding kind M22.2 built (`crate::drm::sensors`) but did not yet
        // dispatch to. `parse_star_tracker_spec` is the one, reused parser (no second parser
        // written for this task); `resolve_sensor_output` is the one place the declared
        // `packet_codecs`/FRAMED-OUT-port convention lives (shared with `ModelKind::Imu` below).
        // `StarTrackerModel::new` is constructed here, GMAT-free and cheap, purely to run its own
        // remaining check (the codec's required-field presence) before this instance is
        // accepted, then discarded -- `Classification::Model(BindingPlan::StarTracker(spec))`
        // carries only the parsed spec onward, matching every other binding kind's "classify
        // parses and validates, materialize builds the model actually used for propagation"
        // split (`crate::registry::ModelRegistry::construct_star_tracker` is what builds the one
        // this run actually uses, an identical `StarTrackerModel::new` call from the identical
        // spec/codec/port triple).
        crate::registry::ModelKind::StarTracker => {
            let spec = sensors::parse_star_tracker_spec(&params).map_err(|e| DrmError::InvalidStarTrackerSpec { instance: instance.name.clone(), reason: e.to_string() })?;
            let (codec, output_port) = resolve_sensor_output(sys, &instance.name)?;
            // `epoch_tai_ns: 0` -- this instance's own real epoch is not resolved until
            // materialization (this pass only classifies/validates); irrelevant here regardless,
            // since this constructed model is discarded immediately after the codec check below
            // and never stepped (see `StarTrackerModel::new`'s own doc comment for what
            // `epoch_tai_ns` is actually for -- seeding `next_due`, which only matters once a
            // model is stepped).
            StarTrackerModel::new(spec.clone(), codec, output_port, 0, &sys.dynamics_model).map_err(|e| DrmError::InvalidStarTrackerSpec { instance: instance.name.clone(), reason: e.to_string() })?;
            Ok(Classification::Model(BindingPlan::StarTracker(spec)))
        }
        // The IMU counterpart of `ModelKind::StarTracker` above.
        crate::registry::ModelKind::Imu => {
            let spec = sensors::parse_imu_spec(&params).map_err(|e| DrmError::InvalidImuSpec { instance: instance.name.clone(), reason: e.to_string() })?;
            let (codec, output_port) = resolve_sensor_output(sys, &instance.name)?;
            // See the identical `epoch_tai_ns: 0` note on the `ModelKind::StarTracker` arm above.
            ImuModel::new(spec.clone(), codec, output_port, 0, &sys.dynamics_model).map_err(|e| DrmError::InvalidImuSpec { instance: instance.name.clone(), reason: e.to_string() })?;
            Ok(Classification::Model(BindingPlan::Imu(spec)))
        }
        // M22.4 (`docs/sil-plan.md`'s M22 milestone paragraph, "A native 'controller' instance
        // closes the loop first"): `parse_attitude_controller_spec` is the one, reused parser;
        // `resolve_controller_ports` is the one place the declared three-codec/three-port
        // convention lives (mirrors `resolve_sensor_output`'s own role for the two sensor
        // kinds). `AttitudeControllerModel::new` is constructed here, GMAT-free and cheap,
        // purely to run its own remaining checks (each codec's required-field presence) before
        // this instance is accepted, then discarded -- matching every other binding kind's
        // "classify parses and validates, materialize builds the model actually used for
        // propagation" split.
        crate::registry::ModelKind::AttitudeController => {
            let spec = controller::parse_attitude_controller_spec(&params).map_err(|e| DrmError::InvalidControllerSpec { instance: instance.name.clone(), reason: e.to_string() })?;
            let ports = resolve_controller_ports(sys, &instance.name)?;
            // See the identical `epoch_tai_ns: 0` note on the `ModelKind::StarTracker` arm above.
            AttitudeControllerModel::new(spec.clone(), ports.star_codec, ports.imu_codec, ports.command_codec, 0, &sys.dynamics_model).map_err(|e| DrmError::InvalidControllerSpec { instance: instance.name.clone(), reason: e.to_string() })?;
            Ok(Classification::Model(BindingPlan::Controller(spec)))
        }
        // M25.1 (`docs/sil-plan.md`'s M25 milestone): the `"ground."` binding kind this batch
        // builds (`crate::drm::ground`) and wires all the way through in the same change (the
        // lead's standing rule for this task). `parse_ground_station_spec` is the one, reused
        // parser; `resolve_ground_ports` is the one place the declared two-codec/two-port
        // (telemetry-in, telecommand-out) convention lives (mirrors `resolve_sensor_output`'s own
        // role for the sensor kinds, `resolve_controller_ports`'s own role for the controller).
        // `GroundStationModel::new` is constructed here, GMAT-free and cheap, purely to run its
        // own remaining checks (both codecs' required-field presence) before this instance is
        // accepted, then discarded -- matching every other binding kind's "classify parses and
        // validates, materialize builds the model actually used for propagation" split.
        crate::registry::ModelKind::Ground => {
            let spec = ground::parse_ground_station_spec(&params).map_err(|e| DrmError::InvalidGroundStationSpec { instance: instance.name.clone(), reason: e.to_string() })?;
            let ports = resolve_ground_ports(sys, &instance.name)?;
            // See the identical `epoch_tai_ns: 0` note on the `ModelKind::StarTracker` arm above.
            GroundStationModel::new(spec.clone(), ports.tm_codec, ports.tm_port, ports.tc_codec, ports.tc_port, &sys.dynamics_model).map_err(|e| DrmError::InvalidGroundStationSpec { instance: instance.name.clone(), reason: e.to_string() })?;
            Ok(Classification::Model(BindingPlan::GroundStation(spec)))
        }
    }
}

// --------------------------------------------------------------------------------------
// Materialization (touches GMAT for the Gmat plan)
// --------------------------------------------------------------------------------------

/// The result of binding one instance: the model, its epoch (TAI ns -- for a `Gmat` plan,
/// exactly the caller's own declared `epoch_tai_ns`, never GMAT's own A1MJD read back; see the
/// module doc comment's "Epoch" section, question 96), and its initial state (SI: metres /
/// metres-per-second). Always length 6 for a `Gmat` plan (`gmat_sys::model::GmatModel` has no
/// other physical shape); for a `ConstantAccel` plan, `spec.x0_si`'s own honoured width -- 0 or
/// 6, never a fixed constant (M21.3, question 141; see `CONSTANT_ACCEL_STATE_DIM`'s own doc
/// comment).
pub(crate) struct Materialized {
    pub model: AnyModel,
    pub t0_tai_ns: i64,
    pub x0_si: Vec<f64>,
    /// `BTreeMap` settings description for `GmatModel::new`'s settings hash -- empty for the
    /// native path (`ConstantAccelModel::describe` supplies its own `ModelInfo` directly).
    pub settings: BTreeMap<String, String>,
}

/// The `BTreeMap` `GmatModel::new` hashes into `ModelInfo.settings_hash`/`TrajectorySegment.
/// dynamics_hash` (M18.4, `docs/open-questions.md` question 127, closing question 115 for a
/// GMAT-bound instance). **Deliberately excludes three things that describe an instance's own
/// initial-or-instantaneous state representation, never its dynamics configuration** -- see
/// [`CARTESIAN_FIELDS`]/[`KEPLERIAN_FIELDS`]/[`DISPLAY_STATE_TYPE_FIELD`]'s own doc comments for
/// the full reasoning behind each:
/// - [`CARTESIAN_FIELDS`] (`spacecraft.X/Y/Z/VX/VY/VZ`): [`fault::rebind_gmat_spec_at_state`]
///   always writes the segment's own *instantaneous* physical state into exactly these six
///   `spacecraft_real` keys before every re-materialization after the first -- fault, maneuver, or
///   a bystander boundary that touches this instance's dynamics not at all -- so including them
///   would hash the trajectory's own position/velocity at the re-materialization epoch, not the
///   dynamics configuration.
/// - [`KEPLERIAN_FIELDS`] (`spacecraft.SMA/ECC/INC/RAAN/AOP/TA`): present only in a segment that
///   has never been rebound (a DRM's own declared initial elements) -- [`fault::
///   rebind_gmat_spec_at_state`] unconditionally removes them from every later segment, so their
///   mere presence-or-absence is exactly "was this segment ever rebound," not a configuration
///   difference.
/// - [`DISPLAY_STATE_TYPE_FIELD`] (`spacecraft.DisplayStateType`): [`fault::
///   rebind_gmat_spec_at_state`] always overwrites this to `"Cartesian"` from whatever the DRM
///   originally declared (`"Keplerian"`, typically) -- it records which of the two representations
///   THIS re-materialization happened to use, not a caller-controllable configuration choice.
///
/// **Why all three had to go together, not just `CARTESIAN_FIELDS` alone.** Excluding only the
/// Cartesian fields still leaves segment 0 (Keplerian, never rebound: `SMA`/`ECC`/.../
/// `DisplayStateType = "Keplerian"` all present) hashing differently from segment 1 (Cartesian,
/// rebound: none of those keys present, `DisplayStateType = "Cartesian"`) even when nothing about
/// the instance's own dynamics configuration changed between them -- exactly the shape a
/// bystander's own FIRST-EVER re-materialization always is. Measured directly: `tests/
/// demo_two_instance.rs::demo_two_instance_bystander_invariance_against_real_single_instance_
/// gmat_runs`'s own `demo_mvr` (bystander to `demo_flt`'s fault at its own first-ever
/// re-materialization) failed to merge with only `CARTESIAN_FIELDS` excluded, and merges correctly
/// once all three are.
///
/// **This does not blind the hash to any DYNAMICS fault that ever had a real propagated effect.**
/// A `spacecraft.SMA`/`ECC`/... fault (question 82's own vocabulary technically allows the
/// *target* string) has never actually changed what GMAT propagates past the segment it fires in:
/// [`fault::apply_dynamics_fault`] writes it into `spacecraft_real`, but the very next
/// `materialize_plan_at_boundary` call always rebinds through [`fault::
/// rebind_gmat_spec_at_state`] first, which strips every `KEPLERIAN_FIELDS` key straight back out
/// -- so there was never a real effect for excluding these fields from the hash to hide (see
/// [`KEPLERIAN_FIELDS`]'s own doc comment). Excluding the six state fields and `DisplayStateType`
/// makes this hash a pure function of central body, gravity model, point masses, relativistic
/// correction, and every declared (non-state) `spacecraft.*` parameter (`Cd`, `Cr`, `DragArea`,
/// `CoordinateSystem`, ... -- ballistics and frame settings, never propagated state or its
/// representation) -- exactly "configuration". A real DYNAMICS fault still changes this hash
/// whenever it actually is one: `fault::apply_gmat_target` writes `force_model.*` fields straight
/// onto `GmatSystemSpec`, or a `spacecraft.*` fault target other than the excluded state/
/// display-type keys (`tests/segment_merge.rs::a_maneuver_never_merges_its_own_boundary_even_
/// when_the_dynamics_hash_is_unchanged`'s own `assert_ne!` on a native binding proves the general
/// "a fault changes the hash" contract; the GMAT-bound demo fixture's own `force_model
/// .gravity_order` fault -- `drms/demo_two_instance.drm.yaml` -- is the real-GMAT instance of the
/// identical claim). A maneuver, by contrast, legitimately produces an EQUAL hash either side of
/// its own boundary now (it changes only state) -- harmless, because `executor::
/// merge_adjacent_segments` keeps a maneuver's own boundary unconditionally, regardless of hash
/// equality (question 115's other half, unchanged by this task).
fn gmat_settings(spec: &GmatSystemSpec) -> BTreeMap<String, String> {
    let mut settings = BTreeMap::new();
    settings.insert("central_body".to_string(), spec.central_body.clone());
    settings.insert("gravity_file".to_string(), spec.gravity_file.clone());
    settings.insert("gravity_degree".to_string(), spec.gravity_degree.to_string());
    settings.insert("gravity_order".to_string(), spec.gravity_order.to_string());
    settings.insert("point_masses".to_string(), spec.point_masses.join(","));
    settings.insert("relativistic_correction".to_string(), spec.relativistic_correction.to_string());
    // M19.4 (question 131): atmospheric drag is a real force-model configuration choice --
    // exactly like gravity_order/point_masses above, never excluded the way the six Cartesian
    // state fields are (this is configuration, not instantaneous state). `None` (the pre-M19.4
    // shape every other GMAT-bound fixture in this crate still declares) contributes nothing,
    // so this is a no-op for every existing golden.
    if let Some(drag_model) = &spec.drag_model {
        settings.insert("drag_model".to_string(), drag_model.clone());
        settings.insert("drag_historic_weather_source".to_string(), spec.drag_historic_weather_source.clone());
        settings.insert("drag_predicted_weather_source".to_string(), spec.drag_predicted_weather_source.clone());
        settings.insert("drag_cssi_space_weather_file".to_string(), spec.drag_cssi_space_weather_file.clone());
    }
    for (k, v) in &spec.spacecraft_real {
        if CARTESIAN_FIELDS.contains(&k.as_str()) || KEPLERIAN_FIELDS.contains(&k.as_str()) {
            continue;
        }
        settings.insert(format!("spacecraft.{k}"), v.to_string());
    }
    for (k, v) in &spec.spacecraft_str {
        if k == DISPLAY_STATE_TYPE_FIELD {
            continue;
        }
        settings.insert(format!("spacecraft.{k}"), v.clone());
    }
    settings
}

/// Build the real GMAT objects (`Spacecraft`, `ForceModel`, and the bound `DerivativeModel`)
/// for `spec` and wrap the result as an [`AnyModel::Gmat`]. `with_stm` requests the 42-state
/// `derivative_model_with_stm` (covariance was requested for this instance and
/// `classify_binding` already confirmed the capability isn't withheld).
///
/// **Two different namespaces, never conflated (M18.4, `docs/open-questions.md` question
/// 127).** `name_suffix` labels this materialization for *output* -- it becomes
/// `GmatModelInfo.id` (`format!("gmat.{name_suffix}")`), which `describe()` reports as
/// `ModelInfo.id` and `trajectory::build_trajectory` copies verbatim into `TrajectorySegment
/// .dynamics_model` -- so it must stay exactly what it was before this task (instance name +
/// segment index; never anything run-specific), or every existing golden's recorded
/// `dynamics_model` string would drift. `gmat_ns` is a *different* string, used only to build the
/// literal names passed to `gmat.construct` below (GMAT's own `Spacecraft`/`ForceModel`/force
/// objects) -- GMAT holds one configuration manager per process (this crate's own `gmat_sys`
/// module docs; `crate::drm::executor`'s own doc comment), so two `execute()` calls in the same
/// process that both bind a `"gmat.*"` instance under the identical `name_suffix` (e.g. the same
/// instance name at segment 0 of two independent runs, or literally the same run replayed twice)
/// used to hand GMAT's `Moderator::CreateSpacecraft`/`CreatePhysicalModel` the identical object
/// name twice, tripping over whatever the first call already registered (observed as an
/// `AddForce`/`GravityField` collision -- "already a GravityField force in place for that body" --
/// diagnosed and worked around, not yet fixed at the source, by `tests/demo_two_instance.rs`'s own
/// former `together_products`/naming-uniqueness doc comment). `gmat_ns` is unique per `execute()`
/// **invocation** (`executor::execute`'s own `GMAT_EXECUTE_SEQ` atomic counter, folded together
/// with the caller's `run_id`), not merely per run id: `run_id` alone would still collide if the
/// identical run id is used for two independent calls in one process, which is exactly what "run
/// the same DRM twice and diff the products" (this task's own required test) does on purpose --
/// see `executor::execute`'s own doc comment for the counter and why `run_id` by itself is not
/// enough. Folding `gmat_ns` into an object's *GMAT-internal* name only -- never into
/// `name_suffix`/`info.id`/`settings`/anything `gmat_settings` hashes -- is what keeps `dynamics_
/// hash`, `dynamics_model`, and every golden untouched by this fix: two runs of the identical DRM
/// still produce byte-identical `RunProducts` (aside from whatever the caller's own `run_id`
/// legitimately carries into `Provenance.run_id`), because nothing about GMAT's *internal* object
/// names is ever observable outside this function.
///
/// `pub(crate)`: only `crate::registry::ModelRegistry::construct_gmat` calls this (M10.3) --
/// see the module doc comment.
#[allow(clippy::too_many_arguments)]
pub(crate) fn materialize_gmat(
    gmat: &Gmat,
    spec: &GmatSystemSpec,
    epoch_tai_ns: i64,
    gmat_ns: &str,
    name_suffix: &str,
    state_space_id: &str,
    with_stm: bool,
    accept_missing_stm_terms: bool,
) -> Result<Materialized, DrmError> {
    // Every GMAT object this call constructs is named under the same `Drm{gmat_ns}_{name_suffix}`
    // prefix -- `gmat_ns` (unique per `execute()` invocation) makes the prefix itself unique
    // across independent runs in one process; `name_suffix` (unique per instance/segment within
    // one run, unchanged from before this task) keeps sibling objects within the same run from
    // colliding with each other, exactly as it always did. See this function's own doc comment
    // for why `name_suffix` alone -- what this crate used through M18.3 -- was not enough.
    let object_ns = format!("{gmat_ns}_{name_suffix}");
    let sat = gmat.construct("Spacecraft", &format!("Drm{object_ns}Sat")).map_err(DrmError::Gmat)?;
    sat.set_str("DateFormat", "A1ModJulian").map_err(DrmError::Gmat)?;
    let a1mjd = Tai::from_nanos(epoch_tai_ns).to_a1_mjd();
    // GMAT'''s `Epoch` field takes a string even under the numeric `A1ModJulian` DateFormat
    // (confirmed empirically: `set_real("Epoch", ...)` is refused with "Epoch expects a
    // String value, but the received value is a real number" -- so this crosses through
    // `set_str`, not `set_real`, formatting the f64 with Rust'''s own round-trip-exact Display).
    sat.set_str("Epoch", &a1mjd.to_string()).map_err(DrmError::Gmat)?;
    for (k, v) in &spec.spacecraft_str {
        sat.set_str(k, v).map_err(DrmError::Gmat)?;
    }
    for (k, v) in &spec.spacecraft_real {
        sat.set_real(k, *v).map_err(DrmError::Gmat)?;
    }

    let fm = gmat.construct("ForceModel", &format!("Drm{object_ns}FM")).map_err(DrmError::Gmat)?;
    fm.set_str("CentralBody", &spec.central_body).map_err(DrmError::Gmat)?;
    // GravityField/PointMassForce/RelativisticCorrection are also given real, `object_ns`-scoped
    // names now (never `""`) -- belt-and-braces alongside the Spacecraft/ForceModel renaming
    // above: `Moderator::CreateObject`'s own "skip configuration-manager registration when name ==
    // \"\"" behavior means an anonymous force was never the confirmed source of the collision this
    // task fixes, but nothing about that internal behavior is a contract `gmat-sys`/GMAT documents
    // to this crate, so relying on it staying that way across a GMAT upgrade is exactly the kind
    // of unrecorded assumption this task's own review standard refuses.
    let grav = gmat.construct("GravityField", &format!("Drm{object_ns}Grav")).map_err(DrmError::Gmat)?;
    grav.set_str("BodyName", &spec.central_body).map_err(DrmError::Gmat)?;
    grav.set_str("PotentialFile", &spec.gravity_file).map_err(DrmError::Gmat)?;
    grav.set_int("Degree", spec.gravity_degree).map_err(DrmError::Gmat)?;
    grav.set_int("Order", spec.gravity_order).map_err(DrmError::Gmat)?;
    fm.add_force(&grav).map_err(DrmError::Gmat)?;
    for (i, body) in spec.point_masses.iter().enumerate() {
        let pm = gmat.construct("PointMassForce", &format!("Drm{object_ns}Pm{i}")).map_err(DrmError::Gmat)?;
        pm.set_str("BodyName", body).map_err(DrmError::Gmat)?;
        fm.add_force(&pm).map_err(DrmError::Gmat)?;
    }
    if spec.relativistic_correction {
        let rc = gmat.construct("RelativisticCorrection", &format!("Drm{object_ns}Rc")).map_err(DrmError::Gmat)?;
        fm.add_force(&rc).map_err(DrmError::Gmat)?;
    }
    // M19.4 (question 131): atmospheric drag, only when declared (`spec.drag_model`) -- the same
    // `DragForce` + separately-`Construct`ed atmosphere-model object pattern
    // `crates/gmat-sys/tests/drag_srp_stm.rs::build_fm_drag_srp` already proves end to end
    // (`Object::set_reference`, closing the M4.3-found "Atmosphere model not defined" gap). The
    // three weather-source fields are set on the `DragForce` object itself, not the atmosphere
    // object -- confirmed against GMAT's own source (`third_party/gmat-src/src/base/forcemodel/
    // DragForce.cpp`): they default to `"ConstantFluxAndGeoMag"` (constant F10.7/Kp, never
    // touching a file at all) unless explicitly set to `"CSSISpaceWeatherFile"`, at which point
    // `DragForce::Initialize` forwards the declared file path to the atmosphere object via
    // `AtmosphereModel::SetInputSource`/`SetStringParameter`, resolved through GMAT's own
    // `FileManager` against `ATMOSPHERE_PATH` when given as a bare filename (`GMAT R2026a/bin/
    // api_startup_file.txt`: `ATMOSPHERE_PATH = DATA_PATH/atmosphere/earth`) -- so a bare
    // `"SpaceWeather-All-v1.2.txt"` resolves to the packaged file this repository's own GMAT
    // install ships, never a network fetch.
    if let Some(drag_model) = &spec.drag_model {
        let drag = gmat.construct("DragForce", &format!("Drm{object_ns}Drag")).map_err(DrmError::Gmat)?;
        drag.set_str("AtmosphereModel", drag_model).map_err(DrmError::Gmat)?;
        drag.set_str("HistoricWeatherSource", &spec.drag_historic_weather_source).map_err(DrmError::Gmat)?;
        drag.set_str("PredictedWeatherSource", &spec.drag_predicted_weather_source).map_err(DrmError::Gmat)?;
        drag.set_str("CSSISpaceWeatherFile", &spec.drag_cssi_space_weather_file).map_err(DrmError::Gmat)?;
        let atmos = gmat.construct(drag_model, &format!("Drm{object_ns}Atmos")).map_err(DrmError::Gmat)?;
        drag.set_reference(&atmos).map_err(DrmError::Gmat)?;
        fm.add_force(&drag).map_err(DrmError::Gmat)?;
    }

    gmat.initialize().map_err(DrmError::Gmat)?;
    let derivative_model = if with_stm { gmat.derivative_model_with_stm(&fm, &sat) } else { gmat.derivative_model(&fm, &sat) }.map_err(DrmError::Gmat)?;

    let info = GmatModelInfo {
        id: format!("gmat.{name_suffix}"),
        version: GMAT_VERSION.to_string(),
        state_space_id: state_space_id.to_string(),
        frame_id: spec.spacecraft_str.get("CoordinateSystem").cloned().unwrap_or_default(),
        goldens: spec.golden_ref.clone().into_iter().collect(),
        has_relativistic_correction: spec.relativistic_correction,
    };
    let settings = gmat_settings(spec);
    // `accept_missing_stm_terms` here is the DRM's own declared `DrmOptions
    // .accept_missing_stm_terms`, threaded through unchanged -- `classify_binding` already
    // refused the one combination that would make this dangerous (covariance requested +
    // RelativisticCorrection present + not accepted), so by the time this runs the flag
    // reaching `GmatModel::new` and the flag `classify_binding` checked are the same value,
    // matching `tests/golden_acceptance.rs`'s own construction.
    // M25.2b: build the FRAMED-consume config *before* `GmatModel::with_ports` below, so its own
    // `consume` field can be wired to the identical `(port, target)` pair `GmatFramedCommandModel`
    // ships its synthetic SIGNAL message on -- a strict, byte-identical no-op for every fixture
    // that declares no `port.consume_framed` (`command` stays `None`, `GmatPortConfig::consume`
    // stays exactly `spec.consume.clone()` as it always was); see that wrapper's own module doc
    // comment for the full account.
    let command = match (&spec.consume_framed_port, &spec.consume_framed) {
        (Some(port), Some(resolved)) => {
            let mut apid_map = crate::codec::ApidMap::new();
            apid_map.insert(resolved.codec.apid, resolved.codec.clone());
            Some(FramedCommandInput { port: port.clone(), apid_map, packet_field: resolved.packet_field.clone(), target: resolved.target.clone() })
        }
        _ => None,
    };
    // M18.3, question 126: attach SIGNAL port wiring -- `GmatModel::with_ports` defaults both
    // fields to `None` when `spec.emit`/`spec.consume` are themselves `None` (the overwhelming
    // majority of `"gmat."`-bound instances, which declare no `"port.*"` parameters at all), so
    // this call is a behaviour no-op for every existing GMAT-bound fixture in this crate.
    // `consume` additionally falls back to `command`'s own `(port, target)` when `port.
    // consume_framed` (not `port.consume`) was declared -- `GmatModel` itself never learns the
    // difference (see `gmat_command`'s own module doc comment). `parse_gmat_spec` already
    // refused declaring both `port.consume` and `port.consume_framed` together, so at most one
    // of `spec.consume`/`command` is ever `Some` here.
    let consume = spec.consume.clone().or_else(|| command.as_ref().map(|c| (c.port.clone(), c.target.clone())));
    let model = GmatModel::new(derivative_model, info, &settings, accept_missing_stm_terms).with_ports(gmat_sys::model::GmatPortConfig { emit: spec.emit.clone(), consume });

    // Question 96: `t0_tai_ns` is the caller's own exact `epoch_tai_ns` -- never
    // `model.epoch_tai_ns()` (which would convert GMAT's own A1MJD *back* to TAI ns, the round
    // trip this task deletes). See the module doc comment's "Epoch" section for what this does
    // and does not change.
    let x0_si = model.initial_state_si().map_err(DrmError::Gmat)?;

    let ack = match (&spec.ack_framed_port, &spec.ack_framed_codec) {
        (Some(port), Some(codec)) => Some(FramedAck { port: port.clone(), codec: (**codec).clone() }),
        _ => None,
    };
    let model = GmatFramedCommandModel::new(model, command, ack);

    Ok(Materialized { model: AnyModel::Gmat(model), t0_tai_ns: epoch_tai_ns, x0_si: x0_si.to_vec(), settings })
}

/// Build a [`ConstantAccelModel`] from `spec` (already fully parsed and validated by
/// [`classify_binding`]/`parse_constant_accel_spec`, `state.*` included). Needs no GMAT call
/// and no epoch cross-check -- `t0_tai_ns` is simply `epoch_tai_ns` (already TAI ns,
/// CDM-native; there is no unit or time-scale boundary to cross for this native binding
/// kind).
///
/// `pub(crate)`: only `crate::registry::ModelRegistry::construct_native` calls this (M10.3) --
/// see the module doc comment.
pub(crate) fn materialize_constant_accel(spec: &ConstantAccelSpec, epoch_tai_ns: i64, model_id: &str, state_space_id: &str) -> Materialized {
    // M14.1, question 109: `emit`/`consume_port` are declared, hashed configuration (question
    // 11) exactly like `accel.{x,y,z}` already are -- included here so a DRM naming a different
    // port/value/consume target hashes differently, never silently.
    let mut settings_map = BTreeMap::from([("accel.x".to_string(), spec.a[0].to_string()), ("accel.y".to_string(), spec.a[1].to_string()), ("accel.z".to_string(), spec.a[2].to_string())]);
    if let Some((port, value)) = &spec.emit {
        settings_map.insert("port.emit".to_string(), port.clone());
        settings_map.insert("port.emit_value".to_string(), value.to_string());
    }
    if let Some(port) = &spec.consume_port {
        settings_map.insert("port.consume".to_string(), port.clone());
    }
    // M25.1: same "declared, hashed configuration" treatment as emit/consume_port above.
    if let Some(port) = &spec.emit_framed_port {
        settings_map.insert("port.emit_framed".to_string(), port.clone());
    }
    // M19.4, question 131: same "declared, hashed configuration" treatment as emit/consume_port
    // above -- a DRM naming a different threshold/mode hashes differently, never silently.
    if let Some(cond) = &spec.condition {
        settings_map.insert("condition.threshold_m".to_string(), cond.threshold_m.to_string());
        settings_map.insert("condition.mode".to_string(), if cond.above { "above".to_string() } else { "below".to_string() });
    }
    // M25.2: same "declared, hashed configuration" treatment as emit_framed above.
    if let (Some(port), Some(field)) = (&spec.consume_framed_port, &spec.consume_framed_field) {
        settings_map.insert("port.consume_framed".to_string(), port.clone());
        settings_map.insert("port.consume_framed_field".to_string(), field.clone());
    }
    if let Some(port) = &spec.ack_framed_port {
        settings_map.insert("port.ack_framed".to_string(), port.clone());
    }
    let info = ModelInfo {
        id: model_id.to_string(),
        version: "1".to_string(),
        state_space_id: state_space_id.to_string(),
        frame_id: spec.frame_id.clone(),
        controls: vec![],
        capabilities: vec![av_cdm::pb::ModelCapability::Derivatives as i32, av_cdm::pb::ModelCapability::Step as i32, av_cdm::pb::ModelCapability::Deterministic as i32],
        depth: "native".to_string(),
        settings_hash: av_dynamics::settings_hash(&settings_map),
        goldens: vec![],
    };
    // M21.3 (question 141): `dim` is `spec.x0_si.len()` itself, not a fixed constant -- already
    // validated against the instance's own declared state space by `classify_binding` before
    // this function is ever called (`crate::registry::ModelRegistry::construct_native`'s own
    // caller contract), so this is simply reading off the width that check already proved this
    // instance honours.
    // `**codec`: `codec` is `&Box<PacketCodec>` here (`ConstantAccelSpec`'s own codec fields are
    // boxed -- clippy's `large_enum_variant`, tripped once this spec grew three `PacketCodec`-
    // carrying `Option`s -- see this struct's own field doc comments); `ConstantAccelModel`'s own
    // `emit_framed`/`consume_framed`/`ack_framed` fields stay plain `(String, PacketCodec, ...)`
    // tuples (that struct is never itself an enum variant clippy sized), so the box is unwrapped
    // exactly once here, at materialization, not carried any further.
    let emit_framed = match (&spec.emit_framed_port, &spec.emit_framed_codec) {
        (Some(port), Some(codec)) => Some((port.clone(), (**codec).clone())),
        _ => None,
    };
    // M25.2: same `(declared port, resolved codec)` pairing shape as `emit_framed` above --
    // `consume_framed` additionally carries the target field name (`ConstantAccelSpec::
    // consume_framed_field`, already validated against `CONSTANT_ACCEL_WRITABLE_PARAMETERS`).
    let consume_framed = match (&spec.consume_framed_port, &spec.consume_framed_codec, &spec.consume_framed_field) {
        (Some(port), Some(codec), Some(field)) => Some((port.clone(), (**codec).clone(), field.clone())),
        _ => None,
    };
    let ack_framed = match (&spec.ack_framed_port, &spec.ack_framed_codec) {
        (Some(port), Some(codec)) => Some((port.clone(), (**codec).clone())),
        _ => None,
    };
    let model = ConstantAccelModel {
        a: spec.a,
        info,
        emit: spec.emit.clone(),
        consume_port: spec.consume_port.clone(),
        condition: spec.condition,
        already_fired: Cell::new(false),
        emit_framed,
        framed_seq: Cell::new(0),
        dim: spec.x0_si.len(),
        consume_framed,
        commanded_accel_scale: Cell::new(1.0),
        last_applied_command_value: Cell::new(None),
        ack_framed,
        decode_errors_this_step: RefCell::new(Vec::new()),
    };
    Materialized { model: AnyModel::ConstantAccel(model), t0_tai_ns: epoch_tai_ns, x0_si: spec.x0_si.clone(), settings: BTreeMap::new() }
}

/// Build an [`AttitudeWheelsModel`] from `spec` and its instance's own declared/resolved
/// `StateSpace` (M22.1b, `docs/open-questions.md` questions 151/152). `AttitudeWheelsModel::new`
/// is the one place both the state-space-dimension check and the wheel-momentum-unit check live
/// (see that function's own doc comment) -- `classify_binding` already ran both once (on the
/// identical spec/state-space pair) before accepting this instance, so this call is not expected
/// to fail in practice, but it is still a real `Result`: a fault/maneuver re-binding
/// (`crate::drm::executor::materialize_plan_at_boundary`) calls this again from a *mutated*
/// spec (`super::fault::apply_dynamics_fault`'s own `BindingPlan::Attitude` arm, e.g. a changed
/// inertia tensor that is no longer positive-definite), so a real failure here is refused with a
/// typed error at that later re-materialization, never a panic -- see [`AttitudeSpecError`]'s own
/// variants for everything that can go wrong.
///
/// `pub(crate)`: only `crate::registry::ModelRegistry::construct_attitude` calls this (mirrors
/// [`materialize_constant_accel`]/[`materialize_gmat`] -- see the module doc comment's
/// "`ModelRegistry` is the sole constructor" section).
pub(crate) fn materialize_attitude(spec: &AttitudeWheelsSpec, declared_state_space: &StateSpace, wheel_command_codec: Option<PacketCodec>, epoch_tai_ns: i64, model_id: &str) -> Result<Materialized, AttitudeSpecError> {
    let model = AttitudeWheelsModel::new(spec, declared_state_space, model_id)?;
    let x0_si = model.initial_state(spec);
    // M22.2b: wrapped in `TruthBroadcastAttitude` -- see `AnyModel::Attitude`'s own doc comment
    // for why every attitude instance is wrapped unconditionally. `x0_si` is read from the
    // *inner* model before wrapping (`TruthBroadcastAttitude` adds no state of its own -- its
    // `state_dim`/`derivatives`/`step` all delegate unchanged), so this is bit-for-bit the same
    // initial state a bare `AttitudeWheelsModel` would have reported before this task.
    let model = TruthBroadcastAttitude::new(model);
    // M22.4: additionally wrapped in `controller::CommandedAttitude`, unconditionally -- `None`
    // (every fixture before M22.4) makes this a strict no-op, exactly `TruthBroadcastAttitude`'s
    // own contract one line above. Adds no state of its own either, so `x0_si` (read above) is
    // still bit-for-bit unaffected.
    let model = CommandedAttitude::new(model, wheel_command_codec);
    // Empty `settings` (mirrors `materialize_constant_accel`'s own convention): `AttitudeWheelsModel::
    // new` already computes its own `settings_hash` directly into `ModelInfo`, so there is no
    // separate `BTreeMap` for a caller to hash further -- see `Materialized::settings`'s own doc
    // comment ("empty for the native path").
    Ok(Materialized { model: AnyModel::Attitude(model), t0_tai_ns: epoch_tai_ns, x0_si, settings: BTreeMap::new() })
}

/// Build a [`StarTrackerModel`] from `spec` and its instance's own declared `PacketCodec`/FRAMED
/// output port (M22.2b, `docs/open-questions.md` questions 142/149/151/152) -- `codec`/
/// `output_port` are [`resolve_sensor_output`]'s own resolution of `sys.packet_codecs`/`sys.
/// ports`, already run once by `classify_binding` before this instance was accepted; re-resolved
/// (not threaded through `BindingPlan::StarTracker`) at every materialization the same way
/// `materialize_attitude` re-resolves `declared_state_space` -- cheap, pure Rust, no I/O.
///
/// `pub(crate)`: only `crate::registry::ModelRegistry::construct_star_tracker` calls this
/// (mirrors [`materialize_attitude`]/[`materialize_constant_accel`]).
pub(crate) fn materialize_star_tracker(spec: &StarTrackerSpec, codec: PacketCodec, output_port: String, epoch_tai_ns: i64, model_id: &str) -> Result<Materialized, SensorSpecError> {
    // `epoch_tai_ns` threaded straight into `StarTrackerModel::new` (M22.2b bug fix -- see that
    // function's own doc comment): this instance's own real starting epoch, not a placeholder,
    // since this is the model actually stepped for propagation, unlike `classify_binding`'s own
    // construct-and-discard validation call.
    let model = StarTrackerModel::new(spec.clone(), codec, output_port, epoch_tai_ns, model_id)?;
    // `StarTrackerModel::state_dim() == 0` always (an instantaneous measurement transform has no
    // propagated physical state) -- `x0_si` is the empty vector, mirroring `ConstantAccelSpec`'s
    // own "zero declared state" shape (`materialize_constant_accel`'s own `spec.x0_si` when no
    // `"state.*"` parameters are declared).
    Ok(Materialized { model: AnyModel::StarTracker(model), t0_tai_ns: epoch_tai_ns, x0_si: Vec::new(), settings: BTreeMap::new() })
}

/// The IMU counterpart of [`materialize_star_tracker`]. `x0_si` is the zero vector
/// (`[bias_gyro_x,y,z, bias_accel_x,y,z] = 0`): `ImuSpec` declares no "initial bias" parameter
/// (a real IMU's bias at power-on is not a quantity a mission author declares up front), so zero
/// is the only sensible starting point for the discrete-time random walk `ImuModel::step_with_
/// ports` advances thereafter.
///
/// `pub(crate)`: only `crate::registry::ModelRegistry::construct_imu` calls this.
pub(crate) fn materialize_imu(spec: &ImuSpec, codec: PacketCodec, output_port: String, epoch_tai_ns: i64, model_id: &str) -> Result<Materialized, SensorSpecError> {
    // See `materialize_star_tracker`'s identical note: `epoch_tai_ns` is this instance's own
    // real starting epoch (M22.2b bug fix, `ImuModel::new`'s own doc comment).
    let model = ImuModel::new(spec.clone(), codec, output_port, epoch_tai_ns, model_id)?;
    Ok(Materialized { model: AnyModel::Imu(model), t0_tai_ns: epoch_tai_ns, x0_si: vec![0.0; 6], settings: BTreeMap::new() })
}

/// Build an [`AttitudeControllerModel`] from `spec` and its instance's own declared star
/// tracker/IMU/wheel-torque-command codecs (M22.4) -- `star_codec`/`imu_codec`/`command_codec`
/// are [`resolve_controller_ports`]'s own resolution, already run once by `classify_binding`
/// before this instance was accepted; re-resolved (not threaded through `BindingPlan::
/// Controller`) at every materialization, the same convention every other native binding kind's
/// own `materialize_*` function already follows.
pub(crate) fn materialize_controller(spec: &AttitudeControllerSpec, star_codec: PacketCodec, imu_codec: PacketCodec, command_codec: PacketCodec, epoch_tai_ns: i64, model_id: &str) -> Result<Materialized, ControllerSpecError> {
    let model = AttitudeControllerModel::new(spec.clone(), star_codec, imu_codec, command_codec, epoch_tai_ns, model_id)?;
    // `AttitudeControllerModel::state_dim() == 0` always (see the module doc comment) -- same
    // empty-initial-state convention `materialize_star_tracker` already uses.
    Ok(Materialized { model: AnyModel::Controller(model), t0_tai_ns: epoch_tai_ns, x0_si: Vec::new(), settings: BTreeMap::new() })
}

/// Build a [`GroundStationModel`] (M25.1) from `spec` and its instance's own declared
/// telemetry-in/telecommand-out codecs/ports -- `tm_codec`/`tm_port`/`tc_codec`/`tc_port` are
/// [`resolve_ground_ports`]'s own resolution, already run once by `classify_binding` before this
/// instance was accepted; re-resolved (not threaded through `BindingPlan::GroundStation`) at
/// every materialization, the same convention every other native binding kind's own
/// `materialize_*` function already follows.
///
/// `pub(crate)`: only `crate::registry::ModelRegistry::construct_ground_station` calls this.
pub(crate) fn materialize_ground_station(spec: &GroundStationSpec, tm_codec: PacketCodec, tm_port: String, tc_codec: PacketCodec, tc_port: String, epoch_tai_ns: i64, model_id: &str) -> Result<Materialized, GroundSpecError> {
    let model = GroundStationModel::new(spec.clone(), tm_codec, tm_port, tc_codec, tc_port, model_id)?;
    // `GroundStationModel::state_dim() == 0` always (a fixed geodetic site has no propagated
    // physical state) -- same empty-initial-state convention `materialize_star_tracker`/
    // `materialize_controller` already use.
    Ok(Materialized { model: AnyModel::GroundStation(model), t0_tai_ns: epoch_tai_ns, x0_si: Vec::new(), settings: BTreeMap::new() })
}

/// M22.2b (`docs/open-questions.md` questions 142/149): resolve which declared `PacketCodec`/
/// FRAMED OUT `Port` a `"startracker."`/`"imu."`-dispatched instance's own measurement packets go
/// out on. Deliberately reuses `SystemDefinition.packet_codecs`/`.ports` -- both already
/// declared, hashed fields (`crate::drm::schema` already runs `packet_codecs` through
/// `crate::codec::validate_system_packet_codecs` at load, question 149/M22.3) -- rather than a
/// new `"startracker.output_port"`-style parameter naming the same thing a second way: exactly
/// one declared `packet_codecs` entry, and exactly one declared `PORT_KIND_FRAMED`/
/// `PORT_DIRECTION_OUT` port, per instance (a sensor model produces exactly one measurement
/// stream). Either count being anything other than 1 is [`DrmError::SensorPortConfiguration`],
/// never silently defaulted to "the first one" or "the last one".
pub(crate) fn resolve_sensor_output(sys: &SystemDefinition, instance: &str) -> Result<(PacketCodec, String), DrmError> {
    if sys.packet_codecs.len() != 1 {
        return Err(DrmError::SensorPortConfiguration { instance: instance.to_string(), reason: format!("expected exactly one declared packet_codecs entry, found {}", sys.packet_codecs.len()) });
    }
    let framed_out: Vec<&Port> = sys.ports.iter().filter(|p| p.kind == PortKind::Framed as i32 && p.direction == PortDirection::Out as i32).collect();
    let [port] = framed_out.as_slice() else {
        return Err(DrmError::SensorPortConfiguration { instance: instance.to_string(), reason: format!("expected exactly one declared PORT_KIND_FRAMED/PORT_DIRECTION_OUT port, found {}", framed_out.len()) });
    };
    Ok((sys.packet_codecs[0].clone(), port.name.clone()))
}

/// M25.2 (`docs/sil-plan.md`'s M25 milestone, "Job 1"): resolve [`ConstantAccelSpec::
/// consume_framed_port`]'s own declared `PacketCodec` -- the flight-side FRAMED consume no prior
/// task built. **Cannot reuse [`resolve_sensor_output`]**: that resolver requires exactly one
/// `packet_codecs` entry *total* on the instance's own `SystemDefinition`, which no longer holds
/// once an instance declares more than one FRAMED port (this consume-in codec alongside `emit_
/// framed_port`'s own telemetry-out codec, or `ack_framed_port`'s own ack-out codec below) -- the
/// two-or-more-codec generalization `emit_framed_port` alone never needed. Matches the declared
/// port by name (must be `PORT_KIND_FRAMED`/`PORT_DIRECTION_IN`) and the codec by its own
/// required shape (`is_command == true` and a declared `"value"` field -- [`crate::drm::command::
/// command_out_packet_codec`]'s own convention), mirroring `crate::drm::ground::resolve_ground_
/// ports`'s own by-name-then-by-shape convention.
pub(crate) fn resolve_constant_accel_command_port(sys: &SystemDefinition, instance: &str, port_name: &str) -> Result<PacketCodec, DrmError> {
    let matching: Vec<&Port> = sys.ports.iter().filter(|p| p.name == port_name).collect();
    let [port] = matching.as_slice() else {
        return Err(DrmError::SensorPortConfiguration { instance: instance.to_string(), reason: format!("port.consume_framed names port {port_name:?}, but this instance declares {} port(s) with that name (expected exactly 1)", matching.len()) });
    };
    if port.kind != PortKind::Framed as i32 || port.direction != PortDirection::In as i32 {
        return Err(DrmError::SensorPortConfiguration { instance: instance.to_string(), reason: format!("port.consume_framed names port {port_name:?}, which is not declared PORT_KIND_FRAMED/PORT_DIRECTION_IN") });
    }
    let candidates: Vec<&PacketCodec> = sys.packet_codecs.iter().filter(|c| c.is_command && c.fields.iter().any(|f| f.name == "value")).collect();
    let [codec] = candidates.as_slice() else {
        return Err(DrmError::SensorPortConfiguration { instance: instance.to_string(), reason: format!("expected exactly one declared packet_codecs entry with is_command=true and a \"value\" field (the command-in codec), found {}", candidates.len()) });
    };
    Ok((*codec).clone())
}

/// M25.2: [`resolve_constant_accel_command_port`]'s ack-out counterpart -- resolve
/// [`ConstantAccelSpec::ack_framed_port`]'s own declared `PacketCodec` (must be `PORT_KIND_FRAMED`/
/// `PORT_DIRECTION_OUT` by name, `is_command == false` with a declared `"cmd_seq"` field by shape
/// -- [`crate::drm::command::command_ack_packet_codec`]'s own convention).
pub(crate) fn resolve_constant_accel_ack_port(sys: &SystemDefinition, instance: &str, port_name: &str) -> Result<PacketCodec, DrmError> {
    let matching: Vec<&Port> = sys.ports.iter().filter(|p| p.name == port_name).collect();
    let [port] = matching.as_slice() else {
        return Err(DrmError::SensorPortConfiguration { instance: instance.to_string(), reason: format!("port.ack_framed names port {port_name:?}, but this instance declares {} port(s) with that name (expected exactly 1)", matching.len()) });
    };
    if port.kind != PortKind::Framed as i32 || port.direction != PortDirection::Out as i32 {
        return Err(DrmError::SensorPortConfiguration { instance: instance.to_string(), reason: format!("port.ack_framed names port {port_name:?}, which is not declared PORT_KIND_FRAMED/PORT_DIRECTION_OUT") });
    }
    let candidates: Vec<&PacketCodec> = sys.packet_codecs.iter().filter(|c| !c.is_command && c.fields.iter().any(|f| f.name == "cmd_seq")).collect();
    let [codec] = candidates.as_slice() else {
        return Err(DrmError::SensorPortConfiguration { instance: instance.to_string(), reason: format!("expected exactly one declared packet_codecs entry with is_command=false and a \"cmd_seq\" field (the ack-out codec), found {}", candidates.len()) });
    };
    Ok((*codec).clone())
}

/// M25.2b (`docs/sil-plan.md`'s M25 milestone, migrating the demo's drag-sail command to a
/// ground-issued telecommand): resolve [`GmatSystemSpec::consume_framed_port`]'s own declared
/// `PacketCodec`, and which of its own declared fields actually supplies a GMAT-writable
/// command value -- question 149's `PacketField.target` "mapping layer," used here for the
/// first time in this crate (every FRAMED consumer built before this task --
/// `ConstantAccelModel::consume_framed`, M25.2 -- named its target through a separate declared
/// `"port.consume_framed_field"` parameter instead; since a GMAT-bound instance's own
/// writable-parameter allowlist ([`GMAT_WRITABLE_PARAMETERS`]) already exists as declared,
/// hashed configuration independent of any one packet field, this resolver reads the target
/// directly off the codec's own declared mapping instead of asking the DRM author to name it a
/// second way).
///
/// Matches the declared port by name (`PORT_KIND_FRAMED`/`PORT_DIRECTION_IN`, exactly one),
/// mirroring [`resolve_constant_accel_command_port`]'s own by-name convention (this resolver
/// cannot reuse that one verbatim: the codec-matching shape below is different -- by declared
/// `target`, not by a fixed `"value"` field name). Matches the codec by shape: `is_command ==
/// true`, and **exactly one** declared field whose `target` is in [`GMAT_WRITABLE_PARAMETERS`]
/// -- zero such fields (nothing this instance could ever apply) or more than one (which one
/// would actually govern is ambiguous) are both typed refusals, never a silent "first field
/// wins".
pub(crate) fn resolve_gmat_command_port(sys: &SystemDefinition, instance: &str, port_name: &str) -> Result<(PacketCodec, String, String), DrmError> {
    let matching: Vec<&Port> = sys.ports.iter().filter(|p| p.name == port_name).collect();
    let [port] = matching.as_slice() else {
        return Err(DrmError::SensorPortConfiguration { instance: instance.to_string(), reason: format!("port.consume_framed names port {port_name:?}, but this instance declares {} port(s) with that name (expected exactly 1)", matching.len()) });
    };
    if port.kind != PortKind::Framed as i32 || port.direction != PortDirection::In as i32 {
        return Err(DrmError::SensorPortConfiguration { instance: instance.to_string(), reason: format!("port.consume_framed names port {port_name:?}, which is not declared PORT_KIND_FRAMED/PORT_DIRECTION_IN") });
    }
    let command_codecs: Vec<&PacketCodec> = sys.packet_codecs.iter().filter(|c| c.is_command).collect();
    let [codec] = command_codecs.as_slice() else {
        return Err(DrmError::SensorPortConfiguration { instance: instance.to_string(), reason: format!("expected exactly one declared packet_codecs entry with is_command=true (the telecommand-in codec), found {}", command_codecs.len()) });
    };
    let writable_fields: Vec<&PacketField> = codec.fields.iter().filter(|f| GMAT_WRITABLE_PARAMETERS.contains(&f.target.as_str())).collect();
    let [field] = writable_fields.as_slice() else {
        return Err(DrmError::SensorPortConfiguration {
            instance: instance.to_string(),
            reason: format!("declared telecommand-in PacketCodec {:?} must have exactly one field whose target is a declared writable parameter ({GMAT_WRITABLE_PARAMETERS:?}), found {}", codec.id, writable_fields.len()),
        });
    };
    Ok(((*codec).clone(), field.name.clone(), field.target.clone()))
}

/// M22.4: `resolve_sensor_output`'s optional counterpart for a `"attitude."`-dispatched
/// instance's own wheel-torque-command *input* -- `None` when this instance declares neither a
/// `controller::ATTITUDE_WHEEL_TORQUE_IN_PORT` FRAMED IN port nor a wheel-torque-command
/// `PacketCodec` (`controller::is_wheel_torque_command_codec`) at all, exactly `TruthBroadcast
/// Attitude`'s own "provably inert wherever nothing is connected" contract applied one layer
/// earlier, at classification. `Some(codec)` requires **exactly one** of each (never "the first
/// one found" if a fixture accidentally declared two) -- a typed [`DrmError::
/// SensorPortConfiguration`] otherwise, the same variant [`resolve_sensor_output`] itself uses
/// for the structurally analogous sensor-side checks.
pub(crate) fn resolve_attitude_wheel_command_input(sys: &SystemDefinition, instance: &str) -> Result<Option<PacketCodec>, DrmError> {
    let command_codecs: Vec<&PacketCodec> = sys.packet_codecs.iter().filter(|c| controller::is_wheel_torque_command_codec(c)).collect();
    let framed_in: Vec<&Port> = sys.ports.iter().filter(|p| p.kind == PortKind::Framed as i32 && p.direction == PortDirection::In as i32 && p.name == controller::ATTITUDE_WHEEL_TORQUE_IN_PORT).collect();
    match (command_codecs.len(), framed_in.len()) {
        (0, 0) => Ok(None),
        (1, 1) => Ok(Some(command_codecs[0].clone())),
        (codecs, ports) => Err(DrmError::SensorPortConfiguration {
            instance: instance.to_string(),
            reason: format!(
                "a wheel-torque-command codec/port must be declared together or not at all: found {codecs} wheel-torque-command PacketCodec(s) and {ports} declared PORT_KIND_FRAMED/PORT_DIRECTION_IN port(s) named {:?}",
                controller::ATTITUDE_WHEEL_TORQUE_IN_PORT
            ),
        }),
    }
}

/// M22.4: resolve which three declared `PacketCodec`s a `"attctrl."`-dispatched instance uses
/// for (in declared-purpose order) its star tracker input, its IMU input, and its wheel-torque
/// command output -- mirrors [`resolve_sensor_output`]'s own role, generalized from one codec/
/// port pair to three. Exactly three declared `packet_codecs` entries are required (never "the
/// first three found" among a larger, ambiguous set); each is identified **structurally** by
/// which required fields it declares (`qx/qy/qz/qw` for star, `wx/wy/wz` for IMU,
/// `controller::is_wheel_torque_command_codec` for the command codec) rather than by
/// declaration order, so a fixture author can list them in any order. Ports are identified by
/// [`controller`]'s own fixed name convention (`CONTROLLER_STARTRACKER_IN_PORT`/`_IMU_IN_PORT`/
/// `_WHEEL_TORQUE_OUT_PORT`) -- exactly two declared `PORT_KIND_FRAMED`/`PORT_DIRECTION_IN`
/// ports (one per required name) and exactly one `PORT_DIRECTION_OUT` port (the wheel-torque
/// name). Any count other than what is described here is a typed [`DrmError::
/// SensorPortConfiguration`], never silently defaulted.
pub(crate) struct ControllerPorts {
    pub star_codec: PacketCodec,
    pub imu_codec: PacketCodec,
    pub command_codec: PacketCodec,
}

/// M25.1 (`docs/sil-plan.md`'s M25 milestone; `docs/open-questions.md` question 149's "a FRAMED
/// telecommand port and a FRAMED telemetry port with declared PacketCodecs"): resolve which
/// declared telemetry-in/telecommand-out `PacketCodec`/`Port` pair a `"ground."`-dispatched
/// instance uses. Codecs are matched by shape (the telemetry-in codec declares `x`/`y`/`z`; the
/// telecommand-out codec is the one with `is_command == true`) rather than declaration order,
/// mirroring `resolve_controller_ports`'s own convention. Ports are identified by name
/// ([`GROUND_TM_IN_PORT`]/[`GROUND_TC_OUT_PORT`]) -- exactly one declared `PORT_KIND_FRAMED`/
/// `PORT_DIRECTION_IN` port under that name and exactly one `PORT_DIRECTION_OUT` port under the
/// other. Any count other than what is described here is a typed [`DrmError::
/// GroundPortConfiguration`], never silently defaulted.
pub(crate) struct GroundPorts {
    pub tm_codec: PacketCodec,
    pub tm_port: String,
    pub tc_codec: PacketCodec,
    pub tc_port: String,
}

/// Fixed conventional FRAMED port names a `"ground."`-dispatched instance's own `SystemDefinition`
/// must declare -- exactly [`ground::GROUND_TM_IN_PORT`]/[`ground::GROUND_TC_OUT_PORT`]'s own
/// re-export here would be redundant naming, so this module simply re-imports and reuses the
/// constants `crate::drm::ground` already declares (see that module's own doc comment).
pub(crate) fn resolve_ground_ports(sys: &SystemDefinition, instance: &str) -> Result<GroundPorts, DrmError> {
    if sys.packet_codecs.len() != 2 {
        return Err(DrmError::GroundPortConfiguration { instance: instance.to_string(), reason: format!("expected exactly 2 declared packet_codecs entries (telemetry-in, telecommand-out), found {}", sys.packet_codecs.len()) });
    }
    let bad = |reason: String| DrmError::GroundPortConfiguration { instance: instance.to_string(), reason };
    let has_fields = |c: &PacketCodec, names: &[&str]| names.iter().all(|n| c.fields.iter().any(|f| f.name == *n));
    let tm: Vec<&PacketCodec> = sys.packet_codecs.iter().filter(|c| has_fields(c, &["x", "y", "z"]) && !c.is_command).collect();
    let tc: Vec<&PacketCodec> = sys.packet_codecs.iter().filter(|c| c.is_command).collect();
    let [tm_codec] = tm.as_slice() else {
        return Err(bad(format!("expected exactly one declared packet_codecs entry with x/y/z fields and is_command=false (the telemetry-in codec), found {}", tm.len())));
    };
    let [tc_codec] = tc.as_slice() else {
        return Err(bad(format!("expected exactly one declared packet_codecs entry with is_command=true (the telecommand-out codec), found {}", tc.len())));
    };

    let framed_in: Vec<&Port> = sys.ports.iter().filter(|p| p.kind == PortKind::Framed as i32 && p.direction == PortDirection::In as i32 && p.name == ground::GROUND_TM_IN_PORT).collect();
    let [tm_port] = framed_in.as_slice() else {
        return Err(bad(format!("expected exactly one declared PORT_KIND_FRAMED/PORT_DIRECTION_IN port named {:?}, found {}", ground::GROUND_TM_IN_PORT, framed_in.len())));
    };
    let framed_out: Vec<&Port> = sys.ports.iter().filter(|p| p.kind == PortKind::Framed as i32 && p.direction == PortDirection::Out as i32 && p.name == ground::GROUND_TC_OUT_PORT).collect();
    let [tc_port] = framed_out.as_slice() else {
        return Err(bad(format!("expected exactly one declared PORT_KIND_FRAMED/PORT_DIRECTION_OUT port named {:?}, found {}", ground::GROUND_TC_OUT_PORT, framed_out.len())));
    };

    Ok(GroundPorts { tm_codec: (*tm_codec).clone(), tm_port: tm_port.name.clone(), tc_codec: (*tc_codec).clone(), tc_port: tc_port.name.clone() })
}

pub(crate) fn resolve_controller_ports(sys: &SystemDefinition, instance: &str) -> Result<ControllerPorts, DrmError> {
    if sys.packet_codecs.len() != 3 {
        return Err(DrmError::SensorPortConfiguration { instance: instance.to_string(), reason: format!("expected exactly 3 declared packet_codecs entries (star tracker input, imu input, wheel-torque command output), found {}", sys.packet_codecs.len()) });
    }
    let has_fields = |c: &PacketCodec, names: &[&str]| names.iter().all(|n| c.fields.iter().any(|f| f.name == *n));
    let star: Vec<&PacketCodec> = sys.packet_codecs.iter().filter(|c| has_fields(c, &["qx", "qy", "qz", "qw"])).collect();
    let imu: Vec<&PacketCodec> = sys.packet_codecs.iter().filter(|c| has_fields(c, &["wx", "wy", "wz"])).collect();
    let command: Vec<&PacketCodec> = sys.packet_codecs.iter().filter(|c| controller::is_wheel_torque_command_codec(c)).collect();
    let bad = |reason: String| DrmError::SensorPortConfiguration { instance: instance.to_string(), reason };
    let [star_codec] = star.as_slice() else {
        return Err(bad(format!("expected exactly one declared packet_codecs entry with qx/qy/qz/qw fields (the star tracker input codec), found {}", star.len())));
    };
    let [imu_codec] = imu.as_slice() else {
        return Err(bad(format!("expected exactly one declared packet_codecs entry with wx/wy/wz fields (the IMU input codec), found {}", imu.len())));
    };
    let [command_codec] = command.as_slice() else {
        return Err(bad(format!("expected exactly one declared packet_codecs entry shaped as a wheel-torque command (the command output codec), found {}", command.len())));
    };

    let framed_in: Vec<&Port> = sys.ports.iter().filter(|p| p.kind == PortKind::Framed as i32 && p.direction == PortDirection::In as i32).collect();
    if framed_in.len() != 2 || !framed_in.iter().any(|p| p.name == controller::CONTROLLER_STARTRACKER_IN_PORT) || !framed_in.iter().any(|p| p.name == controller::CONTROLLER_IMU_IN_PORT) {
        return Err(bad(format!(
            "expected exactly two declared PORT_KIND_FRAMED/PORT_DIRECTION_IN ports named {:?} and {:?}, found {} port(s) named {:?}",
            controller::CONTROLLER_STARTRACKER_IN_PORT,
            controller::CONTROLLER_IMU_IN_PORT,
            framed_in.len(),
            framed_in.iter().map(|p| p.name.as_str()).collect::<Vec<_>>()
        )));
    }
    let framed_out: Vec<&Port> = sys.ports.iter().filter(|p| p.kind == PortKind::Framed as i32 && p.direction == PortDirection::Out as i32).collect();
    let [out_port] = framed_out.as_slice() else {
        return Err(bad(format!("expected exactly one declared PORT_KIND_FRAMED/PORT_DIRECTION_OUT port, found {}", framed_out.len())));
    };
    if out_port.name != controller::CONTROLLER_WHEEL_TORQUE_OUT_PORT {
        return Err(bad(format!("the declared PORT_KIND_FRAMED/PORT_DIRECTION_OUT port must be named {:?}, found {:?}", controller::CONTROLLER_WHEEL_TORQUE_OUT_PORT, out_port.name)));
    }

    Ok(ControllerPorts { star_codec: (*star_codec).clone(), imu_codec: (*imu_codec).clone(), command_codec: (*command_codec).clone() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::fault;
    use av_cdm::pb::{Binding, ContainerBinding, ModelBinding, RenodeBinding};

    fn param(name: &str, value: f64) -> Parameter {
        Parameter { name: name.to_string(), value, ..Default::default() }
    }
    fn sparam(name: &str, s: &str) -> Parameter {
        Parameter { name: name.to_string(), string_value: s.to_string(), ..Default::default() }
    }

    /// M13.2 (question 107) changed `BINDING_KIND_CONTAINER` from a blanket refusal to real
    /// classification (see `container_binding_with_valid_container_parameters_classifies`
    /// and friends, below); `BINDING_KIND_RENODE` is what still exercises "an unsupported,
    /// still-Planned binding kind is refused with a typed error, never silently" today --
    /// the same assertion this test made about CONTAINER before this task, now made about
    /// the binding kind that is actually still unsupported.
    #[test]
    fn renode_binding_is_refused_with_a_typed_error_never_silently() {
        let instance = SystemInstance {
            name: "gnc".to_string(),
            system_id: "gnc_sys".to_string(),
            binding: Some(Binding { kind: BindingKind::Renode as i32, config: Some(av_cdm::pb::binding::Config::Renode(RenodeBinding::default())) }),
            ..Default::default()
        };
        let sys = SystemDefinition { id: "gnc_sys".to_string(), dynamics_model: "native.gnc".to_string(), ..Default::default() };
        let err = classify_binding(&instance, &sys, &DrmOptions::default()).unwrap_err();
        assert!(matches!(err, DrmError::UnsupportedBinding { .. }), "{err:?}");
    }

    fn container_instance(params: Vec<Parameter>) -> (SystemInstance, SystemDefinition) {
        let instance = SystemInstance {
            name: "gnc".to_string(),
            system_id: "gnc_sys".to_string(),
            binding: Some(Binding { kind: BindingKind::Container as i32, config: Some(av_cdm::pb::binding::Config::Container(ContainerBinding::default())) }),
            ..Default::default()
        };
        let sys = SystemDefinition { id: "gnc_sys".to_string(), dynamics_model: "native.gnc".to_string(), parameters: params, ..Default::default() };
        (instance, sys)
    }

    /// M13.2, question 107: a well-formed `container.*` parameter set classifies into
    /// `Classification::Container`, no network touched (`classify_binding` never connects).
    #[test]
    fn container_binding_with_valid_container_parameters_classifies() {
        let (instance, sys) = container_instance(vec![sparam("container.address", "127.0.0.1:50070"), sparam("container.seed_key", "gnc")]);
        let plan = classify_binding(&instance, &sys, &DrmOptions::default()).expect("valid container.* parameters classify");
        match plan {
            Classification::Container(spec) => {
                assert_eq!(spec.address, "127.0.0.1:50070");
                assert_eq!(spec.seed_key, "gnc");
                assert!(!spec.tls);
            }
            other => panic!("expected Classification::Container, got {other:?}"),
        }
    }

    #[test]
    fn container_binding_missing_address_is_a_typed_error() {
        let (instance, sys) = container_instance(vec![sparam("container.seed_key", "gnc")]);
        let err = classify_binding(&instance, &sys, &DrmOptions::default()).unwrap_err();
        assert!(matches!(err, DrmError::MissingParameter { ref name, .. } if name == "container.address"), "{err:?}");
    }

    #[test]
    fn container_binding_missing_seed_key_is_a_typed_error() {
        let (instance, sys) = container_instance(vec![sparam("container.address", "127.0.0.1:50070")]);
        let err = classify_binding(&instance, &sys, &DrmOptions::default()).unwrap_err();
        assert!(matches!(err, DrmError::MissingParameter { ref name, .. } if name == "container.seed_key"), "{err:?}");
    }

    #[test]
    fn container_binding_tls_without_ca_file_is_a_typed_error() {
        let (instance, sys) = container_instance(vec![sparam("container.address", "127.0.0.1:50070"), sparam("container.seed_key", "gnc"), param("container.tls", 1.0)]);
        let err = classify_binding(&instance, &sys, &DrmOptions::default()).unwrap_err();
        assert!(matches!(err, DrmError::MissingParameter { ref name, .. } if name == "container.ca_file"), "{err:?}");
    }

    #[test]
    fn container_binding_unknown_parameter_name_is_a_typed_error_not_a_silent_drop() {
        let (instance, sys) = container_instance(vec![sparam("container.address", "127.0.0.1:50070"), sparam("container.seed_key", "gnc"), sparam("container.bogus_field", "x")]);
        let err = classify_binding(&instance, &sys, &DrmOptions::default()).unwrap_err();
        assert!(matches!(err, DrmError::UnknownParameter { ref name, .. } if name == "container.bogus_field"), "{err:?}");
    }

    // -----------------------------------------------------------------------------------
    // Question 155: "the kernel refuses a non-loopback plaintext endpoint at load with a
    // typed error." Loopback (a same-host container included) stays permitted plaintext.
    // -----------------------------------------------------------------------------------

    #[test]
    fn is_loopback_address_recognizes_the_documented_spellings() {
        // Bare (unbracketed, portless) IPv6 is deliberately not in this list: `container.
        // address`'s own contract is "host:port" (this module's doc comment), which is why an
        // IPv6 host must be bracketed once a port is present (`"[::1]:50070"`, below) -- an
        // unbracketed `"::1"` is genuinely ambiguous against that contract (the last `:`
        // could be the port separator or part of the address) and is out of contract, not a
        // case this function needs to special-case.
        for address in ["127.0.0.1:50070", "127.0.0.1", "127.1.2.3:1", "localhost:50070", "LOCALHOST:1", "[::1]:50070"] {
            assert!(is_loopback_address(address), "{address:?} should be recognized as loopback");
        }
    }

    #[test]
    fn is_loopback_address_refuses_everything_else_including_unresolved_hostnames() {
        for address in ["10.0.0.5:50070", "203.0.113.10:9999", "example.com:50070", "cfs-container.internal:50070", "8.8.8.8:53", "[2001:db8::1]:50070"] {
            assert!(!is_loopback_address(address), "{address:?} should NOT be recognized as loopback (no DNS resolution -- an unrecognized hostname is untrusted, not assumed local)");
        }
    }

    /// Fails against an implementation that only checked `container.tls`/`container.address`'s
    /// *presence* (pre-question-155 behaviour): a plaintext, non-loopback `container.address`
    /// classified successfully with no complaint at all -- exactly the gap question 155 closes.
    /// This never dials `address` (`203.0.113.10` is RFC 5737 TEST-NET-3, guaranteed
    /// non-routable) -- `classify_binding`/`parse_container_spec` refuse before any network
    /// call, the same "no network touched" guarantee `container_binding_with_valid_container_
    /// parameters_classifies` already documents for the happy path.
    #[test]
    fn container_binding_with_a_non_loopback_plaintext_address_is_refused_at_load() {
        let (instance, sys) = container_instance(vec![sparam("container.address", "203.0.113.10:9999"), sparam("container.seed_key", "gnc")]);
        let err = classify_binding(&instance, &sys, &DrmOptions::default()).unwrap_err();
        match err {
            DrmError::ContainerPlaintextNonLoopback { address, .. } => assert_eq!(address, "203.0.113.10:9999"),
            other => panic!("expected DrmError::ContainerPlaintextNonLoopback, got {other:?}"),
        }
    }

    /// The same refusal for a non-loopback hostname (not just a non-loopback IP literal) --
    /// question 155 is deliberately not limited to numeric addresses.
    #[test]
    fn container_binding_with_a_non_loopback_plaintext_hostname_is_refused_at_load() {
        let (instance, sys) = container_instance(vec![sparam("container.address", "cfs-container.internal:50070"), sparam("container.seed_key", "gnc")]);
        let err = classify_binding(&instance, &sys, &DrmOptions::default()).unwrap_err();
        assert!(matches!(err, DrmError::ContainerPlaintextNonLoopback { .. }), "{err:?}");
    }

    /// A plaintext `"localhost"`-spelled address (not just the numeric `127.0.0.1` the happy-
    /// path fixture already covers) still classifies -- the loopback carve-out is not limited
    /// to IP literals.
    #[test]
    fn container_binding_with_a_plaintext_localhost_address_still_classifies() {
        let (instance, sys) = container_instance(vec![sparam("container.address", "localhost:50070"), sparam("container.seed_key", "gnc")]);
        assert!(matches!(classify_binding(&instance, &sys, &DrmOptions::default()), Ok(Classification::Container(_))), "a loopback address must stay permitted plaintext (question 155)");
    }

    /// A non-loopback address is fine once `container.tls` (the nginx-mTLS-fronted path,
    /// question 84) is set -- question 155 only restricts *plaintext*, never TLS.
    #[test]
    fn container_binding_with_a_non_loopback_address_and_tls_still_classifies() {
        let (instance, sys) = container_instance(vec![
            sparam("container.address", "203.0.113.10:9999"),
            sparam("container.seed_key", "gnc"),
            param("container.tls", 1.0),
            sparam("container.ca_file", "/ca.pem"),
            sparam("container.client_cert", "/client.pem"),
            sparam("container.client_key", "/client.key"),
        ]);
        assert!(matches!(classify_binding(&instance, &sys, &DrmOptions::default()), Ok(Classification::Container(_))), "TLS is the question-155-sanctioned path for a non-loopback endpoint");
    }

    #[test]
    fn missing_binding_is_refused_with_a_typed_error() {
        let instance = SystemInstance { name: "gnc".to_string(), system_id: "gnc_sys".to_string(), binding: None, ..Default::default() };
        let sys = SystemDefinition { id: "gnc_sys".to_string(), ..Default::default() };
        let err = classify_binding(&instance, &sys, &DrmOptions::default()).unwrap_err();
        assert!(matches!(err, DrmError::UnsupportedBinding { .. }), "{err:?}");
    }

    #[test]
    fn relativistic_correction_with_covariance_is_refused_unless_accepted() {
        let instance = SystemInstance {
            name: "leo".to_string(),
            system_id: "leo_sys".to_string(),
            binding: Some(Binding { kind: BindingKind::Model as i32, config: Some(av_cdm::pb::binding::Config::Model(ModelBinding { model_id: "leo_sys".to_string() })) }),
            ..Default::default()
        };
        let sys = SystemDefinition {
            id: "leo_sys".to_string(),
            dynamics_model: "gmat.earth.jgm2_8x8".to_string(),
            parameters: vec![
                sparam("force_model.central_body", "Earth"),
                sparam("force_model.gravity_file", "JGM2.cof"),
                param("force_model.gravity_degree", 8.0),
                param("force_model.gravity_order", 8.0),
                param("force_model.relativistic_correction", 1.0),
                sparam("spacecraft.CoordinateSystem", "EarthMJ2000Eq"),
                sparam("spacecraft.DisplayStateType", "Cartesian"),
            ],
            ..Default::default()
        };

        // Refused: covariance requested, RelativisticCorrection present, not accepted.
        let options_refuse = DrmOptions { covariance: true, accept_missing_stm_terms: false, ..Default::default() };
        let err = classify_binding(&instance, &sys, &options_refuse).unwrap_err();
        assert!(matches!(err, DrmError::MissingStmTermsNotAccepted { .. }), "{err:?}");

        // Accepted: same DRM, accept_missing_stm_terms = true -- classifies fine.
        let options_accept = DrmOptions { covariance: true, accept_missing_stm_terms: true, ..Default::default() };
        assert!(matches!(classify_binding(&instance, &sys, &options_accept), Ok(Classification::Model(BindingPlan::Gmat(_)))));

        // Also fine when covariance itself is not requested at all.
        let options_no_cov = DrmOptions { covariance: false, ..Default::default() };
        assert!(matches!(classify_binding(&instance, &sys, &options_no_cov), Ok(Classification::Model(BindingPlan::Gmat(_)))));
    }

    /// Builds a minimal, otherwise-valid `"gmat."`-bound instance/system pair with
    /// `central_body = "Earth"` and the given `spacecraft.CoordinateSystem` value -- the shared
    /// fixture for the M19.1 (question 128) coordinate-system tests below.
    fn earth_instance_with_coordinate_system(coordinate_system: &str) -> (SystemInstance, SystemDefinition) {
        let instance = SystemInstance {
            name: "leo".to_string(),
            system_id: "leo_sys".to_string(),
            binding: Some(Binding { kind: BindingKind::Model as i32, config: Some(av_cdm::pb::binding::Config::Model(ModelBinding { model_id: "leo_sys".to_string() })) }),
            ..Default::default()
        };
        let sys = SystemDefinition {
            id: "leo_sys".to_string(),
            dynamics_model: "gmat.earth.jgm2_8x8".to_string(),
            parameters: vec![
                sparam("force_model.central_body", "Earth"),
                sparam("force_model.gravity_file", "JGM2.cof"),
                param("force_model.gravity_degree", 8.0),
                param("force_model.gravity_order", 8.0),
                sparam("spacecraft.CoordinateSystem", coordinate_system),
                sparam("spacecraft.DisplayStateType", "Cartesian"),
            ],
            ..Default::default()
        };
        (instance, sys)
    }

    /// **Step 1 (question 128, M19.1): the loader refuses a `spacecraft.CoordinateSystem` the
    /// registry cannot realize, rather than mislabelling the trajectory.** Fails against the
    /// pre-M19.1 implementation, which unconditionally did
    /// `frame_id: spec.spacecraft_str.get("CoordinateSystem").cloned().unwrap_or_default()`
    /// with no validation at all -- that code returns `Ok` for literally any string here, never
    /// refusing, so this test would find `classify_binding` succeeding with a `GmatSystemSpec`
    /// silently carrying a `CoordinateSystem` nothing downstream can actually realize.
    /// `"EarthNoSuchFrame"` is chosen specifically because it does NOT decompose under
    /// `executor::body_axes_suffix`'s own vocabulary (unlike `"EarthICRF"`, see the next test) --
    /// it must stay refused even after Step 4 lifts the refusal for realizable frames.
    #[test]
    fn gmat_binding_refuses_a_coordinate_system_the_registry_cannot_realize() {
        let (instance, sys) = earth_instance_with_coordinate_system("EarthNoSuchFrame");
        let err = classify_binding(&instance, &sys, &DrmOptions::default()).unwrap_err();
        match err {
            DrmError::UnsupportedCoordinateSystem { declared, integration_frame, .. } => {
                assert_eq!(declared, "EarthNoSuchFrame");
                assert_eq!(integration_frame, "EarthMJ2000Eq");
            }
            other => panic!("expected DrmError::UnsupportedCoordinateSystem, got {other:?}"),
        }
    }

    /// **Step 4 (question 128, M19.1): the Step 1 refusal is lifted for a frame the registry can
    /// actually realize** -- `"EarthICRF"` decomposes as `{body="Earth"}{axes="ICRF"}` under
    /// `executor::body_axes_suffix`, differs from the integration frame (`"EarthMJ2000Eq"`), and
    /// must still classify successfully, carrying the declared frame through unchanged (never
    /// silently rewritten to the integration frame). Fails against an implementation that kept
    /// Step 1's unconditional refusal without Step 4's carve-out (every non-integration-frame
    /// value, realizable or not, would still be refused) -- exactly the regression this test
    /// exists to catch once the shim/executor conversion capability lands.
    #[test]
    fn gmat_binding_accepts_a_registry_realizable_coordinate_system_other_than_the_integration_frame() {
        let (instance, sys) = earth_instance_with_coordinate_system("EarthICRF");
        let plan = classify_binding(&instance, &sys, &DrmOptions::default()).expect("EarthICRF is registry-realizable (Earth + ICRF)");
        match plan {
            Classification::Model(BindingPlan::Gmat(spec)) => {
                assert_eq!(spec.spacecraft_str.get("CoordinateSystem").map(String::as_str), Some("EarthICRF"));
            }
            other => panic!("expected Classification::Model(BindingPlan::Gmat(_)), got {other:?}"),
        }
    }

    /// The integration frame itself (`"EarthMJ2000Eq"` for `central_body = "Earth"`) always
    /// classifies -- the trivial, pre-M19.1-working case, unaffected by either the Step 1
    /// refusal or the Step 4 carve-out. A regression here would mean the new check is stricter
    /// than intended, breaking every existing GMAT-bound fixture in this crate.
    #[test]
    fn gmat_binding_accepts_the_integration_frame_itself() {
        let (instance, sys) = earth_instance_with_coordinate_system("EarthMJ2000Eq");
        assert!(matches!(classify_binding(&instance, &sys, &DrmOptions::default()), Ok(Classification::Model(BindingPlan::Gmat(_)))));
    }

    /// Question 138, M21.4: covariance requested against a declared frame other than the
    /// integration frame now classifies successfully -- `executor::
    /// convert_gmat_trajectory_to_declared_frame` rotates a non-empty `cov` too now (`R P Rᵀ`
    /// via `Gmat::convert_with_rotation`, `executor::rotate_covariance`), closing the M19.1 gap
    /// this same fixture used to prove was refused
    /// (`gmat_binding_refuses_covariance_combined_with_a_non_integration_frame`, before this
    /// task). Fails against an implementation that left the old classify-time refusal in place
    /// (i.e. still returns `Err` here) -- the whole point of M21.4 is that this combination is
    /// realizable now, not merely that the frame itself is registry-realizable (Step 4, already
    /// covered by `gmat_binding_accepts_a_registry_realizable_coordinate_system_other_than_the_
    /// integration_frame` without `options.covariance` set).
    #[test]
    fn gmat_binding_accepts_covariance_combined_with_a_non_integration_frame() {
        let (instance, sys) = earth_instance_with_coordinate_system("EarthICRF");
        let options = DrmOptions { covariance: true, ..Default::default() };
        let plan = classify_binding(&instance, &sys, &options).expect("covariance + a non-integration, registry-realizable frame must classify since M21.4 (question 138)");
        match plan {
            Classification::Model(BindingPlan::Gmat(spec)) => {
                assert_eq!(spec.spacecraft_str.get("CoordinateSystem").map(String::as_str), Some("EarthICRF"));
            }
            other => panic!("expected Classification::Model(BindingPlan::Gmat(_)), got {other:?}"),
        }
    }

    #[test]
    fn unknown_parameter_name_is_a_typed_error_not_a_silent_drop() {
        let instance = SystemInstance {
            name: "leo".to_string(),
            system_id: "leo_sys".to_string(),
            binding: Some(Binding { kind: BindingKind::Model as i32, config: Some(av_cdm::pb::binding::Config::Model(ModelBinding { model_id: "leo_sys".to_string() })) }),
            ..Default::default()
        };
        let sys = SystemDefinition { id: "leo_sys".to_string(), dynamics_model: "gmat.x".to_string(), parameters: vec![param("totally_unrecognized", 1.0)], ..Default::default() };
        let err = classify_binding(&instance, &sys, &DrmOptions::default()).unwrap_err();
        assert!(matches!(err, DrmError::UnknownParameter { .. }), "{err:?}");
    }

    /// M10.3: `parse_gmat_spec`'s allowlist now recognizes `"output.<name>"` directly (see the
    /// module doc comment's "Parameter vocabulary" section) -- a `"gmat."`-dispatched
    /// `SystemDefinition` declaring one classifies without `crate::drm::executor` needing to
    /// hand this function a filtered copy of `sys` first.
    #[test]
    fn output_dot_parameters_classify_without_being_stripped_first() {
        let instance = SystemInstance {
            name: "leo".to_string(),
            system_id: "leo_sys".to_string(),
            binding: Some(Binding { kind: BindingKind::Model as i32, config: Some(av_cdm::pb::binding::Config::Model(ModelBinding { model_id: "leo_sys".to_string() })) }),
            ..Default::default()
        };
        let sys = SystemDefinition {
            id: "leo_sys".to_string(),
            dynamics_model: "gmat.earth.jgm2_8x8".to_string(),
            parameters: vec![
                sparam("force_model.central_body", "Earth"),
                sparam("force_model.gravity_file", "JGM2.cof"),
                sparam("spacecraft.CoordinateSystem", "EarthMJ2000Eq"),
                sparam("spacecraft.DisplayStateType", "Cartesian"),
                Parameter { name: "output.rmag".to_string(), unit: av_cdm::pb::Unit::Meter as i32, ..Default::default() },
            ],
            ..Default::default()
        };
        let plan = classify_binding(&instance, &sys, &DrmOptions::default()).expect("output.* is a recognized prefix, not DrmError::UnknownParameter");
        assert!(matches!(plan, Classification::Model(BindingPlan::Gmat(_))));
    }

    // -- M18.3 (question 126): `"port.*"` parameters on a `"gmat."`-dispatched SystemDefinition
    // -- previously refused unconditionally (see `drms/README.md`'s "What is NOT here" section
    // and this module's own former `parse_gmat_spec`, whose only path for a `"port."`-prefixed
    // name was the final `_ =>` arm's `DrmError::UnknownParameter`). Every test below fails
    // against that prior implementation: each one either classifies successfully where the old
    // code refused everything under `"port."`, or checks a specific typed-refusal variant/name
    // the old blanket `UnknownParameter { name: "port.emit" }`-style catch-all could not produce
    // (the old code never got far enough to check pairing or writability at all). ---------------

    fn gmat_instance(name: &str, params: Vec<Parameter>) -> (SystemInstance, SystemDefinition) {
        let instance = SystemInstance {
            name: name.to_string(),
            system_id: "leo_sys".to_string(),
            binding: Some(Binding { kind: BindingKind::Model as i32, config: Some(av_cdm::pb::binding::Config::Model(ModelBinding { model_id: "leo_sys".to_string() })) }),
            ..Default::default()
        };
        let mut all_params = vec![
            sparam("force_model.central_body", "Earth"),
            sparam("force_model.gravity_file", "JGM2.cof"),
            sparam("spacecraft.CoordinateSystem", "EarthMJ2000Eq"),
            sparam("spacecraft.DisplayStateType", "Cartesian"),
        ];
        all_params.extend(params);
        let sys = SystemDefinition { id: "leo_sys".to_string(), dynamics_model: "gmat.earth.jgm2_8x8".to_string(), parameters: all_params, ..Default::default() };
        (instance, sys)
    }

    /// A well-formed `"port.emit"`/`"port.emit_output"` pair (naming one of `GmatModel`'s own
    /// two real named outputs) classifies, and the parsed spec carries the exact pair -- proving
    /// `GmatSystemSpec::emit` is actually populated, not merely that classification did not
    /// error. Fails against the pre-M18.3 code (`DrmError::UnknownParameter{name: "port.emit"}`,
    /// since `"port."` fell straight into the catch-all `_ =>` arm) and against an
    /// implementation that recognizes the names but forgets to write `spec.emit`.
    #[test]
    fn a_well_formed_emit_pair_classifies_and_is_carried_on_the_spec() {
        let (instance, sys) = gmat_instance("leo", vec![sparam("port.emit", "cd_out"), sparam("port.emit_output", gmat_sys::model::OUTPUT_CD)]);
        let plan = classify_binding(&instance, &sys, &DrmOptions::default()).expect("a valid emit pair classifies");
        match plan {
            Classification::Model(BindingPlan::Gmat(spec)) => {
                assert_eq!(spec.emit, Some(("cd_out".to_string(), gmat_sys::model::OUTPUT_CD.to_string())));
                assert_eq!(spec.consume, None);
            }
            other => panic!("expected Classification::Model(BindingPlan::Gmat(_)), got {other:?}"),
        }
    }

    /// Same shape for `"port.consume"`/`"port.consume_parameter"`, naming the one currently
    /// declared-writable field (`Cd`). Fails against the pre-M18.3 blanket refusal, and against
    /// an implementation that forgets to write `spec.consume`.
    #[test]
    fn a_well_formed_consume_pair_naming_cd_classifies_and_is_carried_on_the_spec() {
        let (instance, sys) = gmat_instance("leo", vec![sparam("port.consume", "cd_in"), sparam("port.consume_parameter", "Cd")]);
        let plan = classify_binding(&instance, &sys, &DrmOptions::default()).expect("a valid consume pair naming Cd classifies");
        match plan {
            Classification::Model(BindingPlan::Gmat(spec)) => {
                assert_eq!(spec.consume, Some(("cd_in".to_string(), "Cd".to_string())));
                assert_eq!(spec.emit, None);
            }
            other => panic!("expected Classification::Model(BindingPlan::Gmat(_)), got {other:?}"),
        }
    }

    /// `"port.emit"` without its required `"port.emit_output"` partner is
    /// `DrmError::MissingParameter`, mirroring `ConstantAccelSpec`'s identical emit/emit_value
    /// pairing rule. Fails against an implementation that treats `"port.emit"` alone as
    /// sufficient (silently emitting nothing meaningful) or that returns the wrong error variant.
    #[test]
    fn emit_without_emit_output_is_a_typed_missing_parameter_error() {
        let (instance, sys) = gmat_instance("leo", vec![sparam("port.emit", "cd_out")]);
        let err = classify_binding(&instance, &sys, &DrmOptions::default()).unwrap_err();
        assert!(matches!(err, DrmError::MissingParameter { ref name, .. } if name == "port.emit_output"), "{err:?}");
    }

    /// The symmetric case: `"port.emit_output"` without `"port.emit"`.
    #[test]
    fn emit_output_without_emit_is_a_typed_missing_parameter_error() {
        let (instance, sys) = gmat_instance("leo", vec![sparam("port.emit_output", gmat_sys::model::OUTPUT_RMAG)]);
        let err = classify_binding(&instance, &sys, &DrmOptions::default()).unwrap_err();
        assert!(matches!(err, DrmError::MissingParameter { ref name, .. } if name == "port.emit"), "{err:?}");
    }

    /// `"port.consume"` without `"port.consume_parameter"` is `DrmError::MissingParameter`.
    #[test]
    fn consume_without_consume_parameter_is_a_typed_missing_parameter_error() {
        let (instance, sys) = gmat_instance("leo", vec![sparam("port.consume", "cd_in")]);
        let err = classify_binding(&instance, &sys, &DrmOptions::default()).unwrap_err();
        assert!(matches!(err, DrmError::MissingParameter { ref name, .. } if name == "port.consume_parameter"), "{err:?}");
    }

    /// The symmetric case: `"port.consume_parameter"` without `"port.consume"`.
    #[test]
    fn consume_parameter_without_consume_is_a_typed_missing_parameter_error() {
        let (instance, sys) = gmat_instance("leo", vec![sparam("port.consume_parameter", "Cd")]);
        let err = classify_binding(&instance, &sys, &DrmOptions::default()).unwrap_err();
        assert!(matches!(err, DrmError::MissingParameter { ref name, .. } if name == "port.consume"), "{err:?}");
    }

    /// `"port.emit_output"` naming anything other than this model's own two real outputs
    /// (`rmag`/`cd`) is a typed refusal at load time, never a runtime `None`/panic. Fails
    /// against an implementation that accepts an arbitrary `emit_output` string and only
    /// discovers at run time (in `GmatModel::step_with_ports`) that `result.outputs` has no such
    /// key -- silently emitting nothing, forever, with no error anywhere.
    #[test]
    fn emit_output_naming_an_unrecognized_key_is_a_typed_refusal_at_load_time() {
        let (instance, sys) = gmat_instance("leo", vec![sparam("port.emit", "out"), sparam("port.emit_output", "totally_bogus")]);
        let err = classify_binding(&instance, &sys, &DrmOptions::default()).unwrap_err();
        assert!(matches!(err, DrmError::UnknownParameter { ref name, .. } if name.contains("port.emit_output")), "{err:?}");
    }

    /// The headline "typed load error, not a silent skip" case (M18.3's own brief, item 4): a
    /// `"port.consume_parameter"` naming a real GMAT spacecraft field that is nonetheless not on
    /// this crate's own declared-writable allowlist (`GMAT_WRITABLE_PARAMETERS`) is refused at
    /// load time. `"DryMass"` is deliberately a real, valid GMAT field (proving this is a
    /// deliberate allowlist decision, not merely "did this string parse") that this crate simply
    /// has not declared writable over a port. Fails against an implementation that accepts any
    /// non-empty string as a writable target (which `DerivativeModel::set_real_parameter` itself
    /// would happily forward to `GmatBase::SetField`, `GMAT_WRITABLE_PARAMETERS`'s own doc
    /// comment: "that crate's job is to apply whatever it is told, not to judge it").
    #[test]
    fn consume_parameter_naming_a_non_writable_field_is_a_typed_refusal_at_load_time() {
        let (instance, sys) = gmat_instance("leo", vec![sparam("port.consume", "in"), sparam("port.consume_parameter", "DryMass")]);
        let err = classify_binding(&instance, &sys, &DrmOptions::default()).unwrap_err();
        assert!(matches!(err, DrmError::UnknownParameter { ref name, .. } if name.contains("port.consume_parameter") && name.contains("DryMass")), "{err:?}");
    }

    /// An entirely unrecognized `"port.*"` field name (neither of the four this task adds) is
    /// still refused, the same "never silently ignored" contract every other prefix in this
    /// function already follows.
    #[test]
    fn an_unrecognized_port_dot_field_name_is_a_typed_refusal() {
        let (instance, sys) = gmat_instance("leo", vec![sparam("port.bogus_field", "x")]);
        let err = classify_binding(&instance, &sys, &DrmOptions::default()).unwrap_err();
        assert!(matches!(err, DrmError::UnknownParameter { ref name, .. } if name == "port.bogus_field"), "{err:?}");
    }

    /// A `"gmat."`-bound instance declaring neither emit nor consume parameters at all (the
    /// overwhelming majority of this crate's own fixtures) still classifies exactly as before
    /// this task, with both fields `None` -- proves M18.3 is additive, not a behaviour change
    /// for every pre-existing GMAT-bound DRM in this repository.
    #[test]
    fn no_port_parameters_at_all_still_classifies_with_both_fields_none() {
        let (instance, sys) = gmat_instance("leo", vec![]);
        let plan = classify_binding(&instance, &sys, &DrmOptions::default()).expect("no port.* parameters at all is the pre-M18.3 default shape");
        match plan {
            Classification::Model(BindingPlan::Gmat(spec)) => {
                assert_eq!(spec.emit, None);
                assert_eq!(spec.consume, None);
            }
            other => panic!("expected Classification::Model(BindingPlan::Gmat(_)), got {other:?}"),
        }
    }

    // =========================================================================================
    // M25.2b (`docs/sil-plan.md`'s M25 milestone, migrating the demo's drag-sail command to a
    // ground-issued telecommand): `parse_gmat_spec`/`classify_binding`/`resolve_gmat_command_
    // port`'s own load-time handling of `"port.consume_framed"`/`"port.ack_framed"` -- GMAT-free,
    // mirroring the equivalent `ConstantAccelModel::consume_framed`/`.ack_framed` test block
    // (M25.2) one binding kind over.
    // =========================================================================================

    fn gmat_command_in_codec(target: &str) -> PacketCodec {
        let f = PacketField { name: "value".to_string(), bit_offset: 0, bit_width: 64, r#type: av_cdm::pb::PacketFieldType::Float64 as i32, unit: av_cdm::pb::Unit::Dimensionless as i32, scale: 1.0, offset: 0.0, target: target.to_string() };
        PacketCodec { id: "test_cmd_in".to_string(), apid: 950, is_command: true, secondary_header_bytes: 0, user_data_bytes: 8, fields: vec![f], description: "test".to_string() }
    }
    fn gmat_ack_out_codec() -> PacketCodec {
        crate::drm::command::command_ack_packet_codec("test_ack_out", 951)
    }
    fn gmat_instance_with_ports(name: &str, params: Vec<Parameter>, ports: Vec<Port>, packet_codecs: Vec<PacketCodec>) -> (SystemInstance, SystemDefinition) {
        let (instance, mut sys) = gmat_instance(name, params);
        sys.ports = ports;
        sys.packet_codecs = packet_codecs;
        (instance, sys)
    }
    fn framed_in_port(name: &str) -> Port {
        Port { name: name.to_string(), kind: PortKind::Framed as i32, direction: PortDirection::In as i32, schema: "ccsds.spp".to_string(), ..Default::default() }
    }
    fn framed_out_port(name: &str) -> Port {
        Port { name: name.to_string(), kind: PortKind::Framed as i32, direction: PortDirection::Out as i32, schema: "ccsds.spp".to_string(), ..Default::default() }
    }

    /// A well-formed `"port.consume_framed"` declaration, with exactly one declared telecommand
    /// codec whose one field targets `"Cd"`, classifies and resolves `consume_framed_codec`/
    /// `.consume_framed_packet_field`/`.consume_framed_target` -- proving `PacketField.target`
    /// (question 149) genuinely drives the resolution, not merely that classification did not
    /// error. Fails against an implementation that never resolves the codec at all, or that
    /// looks the field up by a fixed name (`"value"`) instead of by `target`.
    #[test]
    fn a_well_formed_consume_framed_declaration_classifies_and_resolves_target_from_the_codec() {
        let (instance, sys) = gmat_instance_with_ports("leo", vec![sparam("port.consume_framed", "cd_cmd_in")], vec![framed_in_port("cd_cmd_in")], vec![gmat_command_in_codec("Cd")]);
        let plan = classify_binding(&instance, &sys, &DrmOptions::default()).expect("a valid consume_framed declaration classifies");
        match plan {
            Classification::Model(BindingPlan::Gmat(spec)) => {
                assert_eq!(spec.consume_framed_port.as_deref(), Some("cd_cmd_in"));
                let resolved = spec.consume_framed.as_deref().expect("consume_framed_port declared, so consume_framed must resolve");
                assert_eq!(resolved.packet_field, "value", "the packet field's own name, looked up by target, not assumed to be literally \"value\"");
                assert_eq!(resolved.target, "Cd");
                assert_eq!(resolved.codec.id, "test_cmd_in");
                assert_eq!(spec.consume, None, "port.consume_framed is a distinct field from the SIGNAL-only port.consume");
            }
            other => panic!("expected Classification::Model(BindingPlan::Gmat(_)), got {other:?}"),
        }
    }

    /// `"port.consume_framed"` and `"port.ack_framed"` declared together resolve both codecs.
    #[test]
    fn consume_framed_with_ack_framed_resolves_both_codecs() {
        let (instance, sys) = gmat_instance_with_ports(
            "leo",
            vec![sparam("port.consume_framed", "cd_cmd_in"), sparam("port.ack_framed", "cd_ack_out")],
            vec![framed_in_port("cd_cmd_in"), framed_out_port("cd_ack_out")],
            vec![gmat_command_in_codec("Cd"), gmat_ack_out_codec()],
        );
        let plan = classify_binding(&instance, &sys, &DrmOptions::default()).expect("consume_framed + ack_framed classifies");
        match plan {
            Classification::Model(BindingPlan::Gmat(spec)) => {
                assert_eq!(spec.ack_framed_port.as_deref(), Some("cd_ack_out"));
                assert_eq!(spec.ack_framed_codec.as_deref().map(|c| c.id.as_str()), Some("test_ack_out"));
            }
            other => panic!("expected Classification::Model(BindingPlan::Gmat(_)), got {other:?}"),
        }
    }

    /// `"port.ack_framed"` without `"port.consume_framed"` is a typed refusal -- an ack with
    /// nothing to acknowledge is meaningless, mirroring `ConstantAccelSpec`'s identical pairing
    /// rule.
    #[test]
    fn ack_framed_without_consume_framed_is_a_typed_missing_parameter_error() {
        let (instance, sys) = gmat_instance_with_ports("leo", vec![sparam("port.ack_framed", "cd_ack_out")], vec![framed_out_port("cd_ack_out")], vec![gmat_ack_out_codec()]);
        let err = classify_binding(&instance, &sys, &DrmOptions::default()).unwrap_err();
        assert!(matches!(err, DrmError::MissingParameter { ref name, .. } if name.contains("port.consume_framed")), "{err:?}");
    }

    /// Declaring `"port.consume"` (SIGNAL) and `"port.consume_framed"` (FRAMED) together is a
    /// typed refusal: both would command the identical underlying `GmatPortConfig::consume`
    /// slot, so silently letting one win would hide a fixture bug. Fails against an
    /// implementation that lets `materialize_gmat`'s own `.or_else` fallback silently pick one.
    #[test]
    fn consume_and_consume_framed_declared_together_is_a_typed_refusal() {
        let (instance, sys) = gmat_instance_with_ports(
            "leo",
            vec![sparam("port.consume", "cd_in"), sparam("port.consume_parameter", "Cd"), sparam("port.consume_framed", "cd_cmd_in")],
            vec![framed_in_port("cd_cmd_in")],
            vec![gmat_command_in_codec("Cd")],
        );
        let err = classify_binding(&instance, &sys, &DrmOptions::default()).unwrap_err();
        assert!(matches!(err, DrmError::UnknownParameter { ref name, .. } if name.contains("port.consume_framed")), "{err:?}");
    }

    /// A declared telecommand-in codec with NO field targeting a writable parameter is refused,
    /// typed -- "nothing this instance could ever apply," never a silent no-op that only fails
    /// at run time when nothing ever decodes.
    #[test]
    fn consume_framed_codec_with_no_writable_target_field_is_a_typed_refusal() {
        let (instance, sys) = gmat_instance_with_ports("leo", vec![sparam("port.consume_framed", "cd_cmd_in")], vec![framed_in_port("cd_cmd_in")], vec![gmat_command_in_codec("DryMass")]);
        let err = classify_binding(&instance, &sys, &DrmOptions::default()).unwrap_err();
        assert!(matches!(err, DrmError::SensorPortConfiguration { ref reason, .. } if reason.contains("found 0")), "{err:?}");
    }

    /// A declared telecommand-in codec with TWO fields both targeting writable parameters is
    /// refused, typed -- "which one would actually govern is ambiguous," never a silent
    /// first-field-wins.
    #[test]
    fn consume_framed_codec_with_two_writable_target_fields_is_a_typed_refusal() {
        let mut codec = gmat_command_in_codec("Cd");
        codec.fields.push(PacketField { name: "value2".to_string(), bit_offset: 0, bit_width: 64, r#type: av_cdm::pb::PacketFieldType::Float64 as i32, unit: av_cdm::pb::Unit::Dimensionless as i32, scale: 1.0, offset: 0.0, target: "Cd".to_string() });
        let (instance, sys) = gmat_instance_with_ports("leo", vec![sparam("port.consume_framed", "cd_cmd_in")], vec![framed_in_port("cd_cmd_in")], vec![codec]);
        let err = classify_binding(&instance, &sys, &DrmOptions::default()).unwrap_err();
        assert!(matches!(err, DrmError::SensorPortConfiguration { ref reason, .. } if reason.contains("found 2")), "{err:?}");
    }

    /// `"port.consume_framed"` naming a port that is not declared `PORT_KIND_FRAMED`/
    /// `PORT_DIRECTION_IN` (e.g. declared `OUT` instead) is a typed refusal.
    #[test]
    fn consume_framed_naming_a_wrong_direction_port_is_a_typed_refusal() {
        let (instance, sys) = gmat_instance_with_ports("leo", vec![sparam("port.consume_framed", "cd_cmd_in")], vec![framed_out_port("cd_cmd_in")], vec![gmat_command_in_codec("Cd")]);
        let err = classify_binding(&instance, &sys, &DrmOptions::default()).unwrap_err();
        assert!(matches!(err, DrmError::SensorPortConfiguration { ref reason, .. } if reason.contains("PORT_KIND_FRAMED/PORT_DIRECTION_IN")), "{err:?}");
    }

    /// `"port.consume_framed"` naming a port this instance declares no `Port` for at all.
    #[test]
    fn consume_framed_naming_an_undeclared_port_is_a_typed_refusal() {
        let (instance, sys) = gmat_instance_with_ports("leo", vec![sparam("port.consume_framed", "cd_cmd_in")], vec![], vec![gmat_command_in_codec("Cd")]);
        let err = classify_binding(&instance, &sys, &DrmOptions::default()).unwrap_err();
        assert!(matches!(err, DrmError::SensorPortConfiguration { ref reason, .. } if reason.contains("declares 0 port(s)")), "{err:?}");
    }

    /// M10.3: `AnyModel::step` must delegate to each variant's own `step`, not the trait's
    /// default (which would silently recompute the identical physical state via `derivatives`
    /// and lose whatever a variant's own override -- `GmatModel::step`'s `outputs` -- would have
    /// produced; the GMAT-bound half of this is proven end to end by `tests/drm_executor.rs::
    /// drm_rmag_output_matches_a_genuine_gmat_reportfile`, which needs a live GMAT install and
    /// so cannot run here). This GMAT-free case proves the delegation itself compiles and
    /// produces the same closed-form answer `ConstantAccelModel`'s own default `step` always
    /// did -- `ConstantAccelModel` overrides nothing, so this is also a no-regression check.
    #[test]
    fn any_model_step_delegates_to_the_constant_accel_variant() {
        let spec = ConstantAccelSpec { a: [0.0, 0.0, -9.8], frame_id: "test.frame".to_string(), x0_si: vec![0.0, 0.0, 100.0, 1.0, 2.0, 0.0], ..Default::default() };
        let mat = materialize_constant_accel(&spec, 1_700_000_000_000_000_000, "native.test", "test.space");
        let step = mat.model.step(&mat.x0_si, mat.t0_tai_ns, &[], 1_000_000_000).unwrap(); // 1 s
        assert!((step.state[2] - (100.0 + 0.0 * 1.0 + 0.5 * -9.8 * 1.0)).abs() < 1e-9, "z(1s) = {}", step.state[2]);
        assert!((step.state[5] - (0.0 + -9.8 * 1.0)).abs() < 1e-9, "vz(1s) = {}", step.state[5]);
    }

    // -- Full per-method delegation coverage (question 112) ---------------------------------
    //
    // `AnyModel` sits one level further out than `av_dynamics::erase::ErasedModel` (an enum over
    // two concrete, real model types rather than a generic wrapper over an arbitrary `M`), so
    // its own per-method delegation is proven here directly against the `ConstantAccel` variant
    // -- GMAT-free, matching every other test in this module. The `Gmat` variant's own halves of
    // `step`/`step_with_ports` are proven end to end elsewhere against a real GMAT install
    // (`tests/drm_executor.rs::drm_rmag_output_matches_a_genuine_gmat_reportfile`,
    // `tests/golden_acceptance.rs`), which cannot run in this GMAT-free unit test module -- see
    // `any_model_step_delegates_to_the_constant_accel_variant`'s own doc comment for the
    // identical reasoning.

    fn test_constant_accel_mat(spec: ConstantAccelSpec) -> Materialized {
        materialize_constant_accel(&spec, 1_700_000_000_000_000_000, "native.test", "test.space")
    }

    #[test]
    fn any_model_state_dim_delegates_to_the_constant_accel_variant() {
        // M21.3 (question 141): `state_dim()` now reports `spec.x0_si.len()`, not a fixed
        // constant -- an explicit six-element `x0_si` is what makes this a determinate "6"
        // rather than `ConstantAccelSpec::default()`'s own now-empty `Vec` (see
        // `a_native_instance_with_an_empty_declared_state_space_materializes_at_zero_dim` below
        // for the symmetric zero-width case).
        let mat = test_constant_accel_mat(ConstantAccelSpec { frame_id: "test.frame".to_string(), x0_si: vec![0.0; 6], ..Default::default() });
        assert_eq!(mat.model.state_dim(), 6);
    }

    #[test]
    fn any_model_derivatives_delegates_to_the_constant_accel_variant() {
        let mat = test_constant_accel_mat(ConstantAccelSpec { a: [1.0, 2.0, 3.0], frame_id: "test.frame".to_string(), x0_si: vec![0.0; 6], ..Default::default() });
        let mut out = [0.0; 6];
        mat.model.derivatives(&[0.0, 0.0, 0.0, 4.0, 5.0, 6.0], mat.t0_tai_ns, &[], &mut out).unwrap();
        assert_eq!(out, [4.0, 5.0, 6.0, 1.0, 2.0, 3.0], "must reach ConstantAccelModel::derivatives, not some other computation");
    }

    #[test]
    fn any_model_describe_delegates_to_the_constant_accel_variant() {
        let mat = materialize_constant_accel(&ConstantAccelSpec { frame_id: "test.frame".to_string(), ..Default::default() }, 0, "native.distinctive_id", "test.space");
        assert_eq!(mat.model.describe().id, "native.distinctive_id");
    }

    #[test]
    fn any_model_integrator_delegates_to_the_constant_accel_variant() {
        // `ConstantAccelModel` never overrides `integrator`, so both sides are the trait's own
        // default -- this proves `AnyModel::integrator` (added by this task; previously there
        // was no override at all) actually reaches the variant via its own match arm rather than
        // silently reaching some other default by coincidence (see that method's own doc
        // comment for why it was missing and why that was still, until now, harmless).
        let mat = test_constant_accel_mat(ConstantAccelSpec { frame_id: "test.frame".to_string(), ..Default::default() });
        let direct = av_dynamics::integrate::Dopri5::default();
        let got = mat.model.integrator();
        assert_eq!(got.rtol, direct.rtol);
        assert_eq!(got.atol, direct.atol);
        assert_eq!(got.initial_step, direct.initial_step);
        assert_eq!(got.max_step, direct.max_step);
    }

    #[test]
    fn any_model_stm_capable_delegates_to_the_constant_accel_variant() {
        let mat = test_constant_accel_mat(ConstantAccelSpec { frame_id: "test.frame".to_string(), ..Default::default() });
        assert!(!mat.model.stm_capable(), "the native placeholder never declares STM capability");
    }

    #[test]
    fn any_model_stm_derivatives_returns_a_typed_capability_missing_error_for_the_constant_accel_variant() {
        let mat = test_constant_accel_mat(ConstantAccelSpec { frame_id: "test.frame".to_string(), ..Default::default() });
        let mut out = [0.0; 42]; // size is irrelevant: the capability check short-circuits first
        let err = mat.model.stm_derivatives(&[0.0; 42], mat.t0_tai_ns, &[], &mut out).unwrap_err();
        assert!(matches!(err, AnyModelError::CapabilityMissing { ref capability, .. } if capability == "stm_derivatives"), "{err:?}");
    }

    #[test]
    fn any_model_step_with_stm_returns_a_typed_capability_missing_error_for_the_constant_accel_variant() {
        // `ConstantAccelModel::stm_capable()` is always `false` -- `AnyModel::step_with_stm`
        // must refuse with a typed error (question 112) rather than reach the trait's own
        // default (which would integrate `stm_derivatives` and panic).
        let mat = test_constant_accel_mat(ConstantAccelSpec { a: [0.0, 0.0, -9.8], frame_id: "test.frame".to_string(), x0_si: vec![0.0, 0.0, 100.0, 1.0, 2.0, 0.0], ..Default::default() });
        let err = mat.model.step_with_stm(&mat.x0_si, mat.t0_tai_ns, &[], 1_000_000_000).unwrap_err();
        assert!(matches!(err, AnyModelError::CapabilityMissing { ref capability, .. } if capability == "step_with_stm"), "{err:?}");
    }

    #[test]
    fn any_model_step_with_ports_delegates_to_the_constant_accel_variant() {
        let mat = test_constant_accel_mat(ConstantAccelSpec {
            a: [0.0, 0.0, -9.8],
            frame_id: "test.frame".to_string(),
            x0_si: vec![0.0, 0.0, 100.0, 1.0, 2.0, 0.0],
            emit: Some(("out".to_string(), 7.0)),
            ..Default::default()
        });
        let (_, outbox, applied) = mat.model.step_with_ports(&mat.x0_si, mat.t0_tai_ns, &[], 1_000_000_000, &Inbox::empty()).unwrap();
        let sent = outbox.messages();
        assert_eq!(sent.len(), 1, "AnyModel::step_with_ports must delegate to ConstantAccelModel's own emit override, not the trait default's empty Outbox");
        assert_eq!(sent[0].port, "out");
        assert!(applied.is_empty(), "ConstantAccelModel never applies a command that bypasses a hashed configuration surface");
    }

    // --------------------------------------------------------------------------------------
    // `gmat_settings` (M18.4, `docs/open-questions.md` question 127's second half): config-only
    // dynamics hash. GMAT-free (`gmat_settings` is pure `BTreeMap` bookkeeping over a
    // `GmatSystemSpec`, no `Gmat`/engine lock needed), so these run in every test binary, not
    // just the GMAT-gated ones -- unlike the real-GMAT end-to-end proof in
    // `tests/demo_two_instance.rs`, they pin the exact mechanism, not just the observable effect.
    // --------------------------------------------------------------------------------------

    /// The declared, never-rebound "before" shape of a Keplerian-initialized GMAT spec (segment
    /// 0), and [`fault::rebind_gmat_spec_at_state`]'s own "after" shape for the identical
    /// configuration once continuity has forced a re-materialization (segment 1+, e.g. a
    /// bystander boundary that changed nothing about this instance's own dynamics). Same
    /// `central_body`/`gravity_*`/`point_masses`/ballistic `spacecraft.*` throughout -- only the
    /// state *representation* differs, exactly the M18.4 fix's own claim.
    fn keplerian_spec() -> GmatSystemSpec {
        GmatSystemSpec {
            central_body: "Earth".to_string(),
            gravity_file: "JGM2.cof".to_string(),
            gravity_degree: 8,
            gravity_order: 8,
            point_masses: vec!["Luna".to_string(), "Sun".to_string()],
            relativistic_correction: false,
            spacecraft_str: BTreeMap::from([("CoordinateSystem".to_string(), "EarthMJ2000Eq".to_string()), ("DisplayStateType".to_string(), "Keplerian".to_string())]),
            spacecraft_real: BTreeMap::from([
                ("SMA".to_string(), 6878.0),
                ("ECC".to_string(), 0.001),
                ("INC".to_string(), 51.6),
                ("RAAN".to_string(), 30.0),
                ("AOP".to_string(), 0.0),
                ("TA".to_string(), 0.0),
                ("Cd".to_string(), 2.2),
            ]),
            ..Default::default()
        }
    }

    /// Required test. Fails against the pre-M18.4 `gmat_settings` (hashed every `spacecraft.*`
    /// entry unconditionally): `rebind_gmat_spec_at_state` -- called for EVERY GMAT
    /// re-materialization after the first, whether or not anything about the instance's own
    /// dynamics actually changed -- drops the Keplerian fields, flips `DisplayStateType` to
    /// `"Cartesian"`, and writes a *different* physical state into `X/Y/Z/VX/VY/VZ` than
    /// `keplerian_spec`'s own declared orbit started at (a fresh, arbitrary point along the
    /// propagated arc), so the old implementation's settings map genuinely differed both in which
    /// keys are present and in every numeric value they carry -- it could not have hashed equal
    /// by coincidence. Against the fix, the map is exactly the same central-body/gravity/ballistic
    /// configuration either side of a rebind.
    #[test]
    fn gmat_settings_is_unchanged_across_a_rebind_that_changed_only_state_representation() {
        let before = keplerian_spec();
        let after = fault::rebind_gmat_spec_at_state(&before, [7_000_000.0, 123_456.0, -654_321.0, 10.0, 7_500.0, -20.0]);
        assert_ne!(before.spacecraft_str.get("DisplayStateType"), after.spacecraft_str.get("DisplayStateType"), "sanity: rebind really did flip the representation, so an equal settings map below is not vacuous");
        assert_eq!(gmat_settings(&before), gmat_settings(&after), "a rebind that changes only the state representation (Keplerian -> Cartesian) and the instantaneous state itself must not change the settings map at all");
    }

    /// Required test. Fails against the pre-M18.4 `gmat_settings`, and also against an
    /// over-broad fix that excludes `spacecraft.*` wholesale (which would pass this vacuously by
    /// hashing nothing spacecraft-related at all, not because it correctly narrowed the exclusion
    /// -- see this crate's own `gmat_settings` doc comment's "excludes three things" list):
    /// changing a genuine ballistic configuration value (`Cd`, never touched by
    /// `rebind_gmat_spec_at_state`) must still change the hash, both before and after a rebind.
    #[test]
    fn gmat_settings_still_changes_when_a_real_ballistic_parameter_differs() {
        let mut changed = keplerian_spec();
        changed.spacecraft_real.insert("Cd".to_string(), 3.3);
        assert_ne!(gmat_settings(&keplerian_spec()), gmat_settings(&changed), "a genuine spacecraft.Cd difference must still change the settings map");

        let state = [7_000_000.0, 0.0, 0.0, 0.0, 7_500.0, 0.0];
        let rebound_a = fault::rebind_gmat_spec_at_state(&keplerian_spec(), state);
        let rebound_b = fault::rebind_gmat_spec_at_state(&changed, state);
        assert_ne!(gmat_settings(&rebound_a), gmat_settings(&rebound_b), "the same Cd difference must still show up in the hash after a rebind, not just before one");
    }

    /// Required test. Fails against the pre-M18.4 `gmat_settings` (two rebinds from different
    /// physical states hash differently, since the full `X/Y/Z/VX/VY/VZ` state was included) and
    /// against a fix that forgot to exclude `CARTESIAN_FIELDS` specifically.
    #[test]
    fn gmat_settings_is_unchanged_across_two_rebinds_at_different_physical_states() {
        let spec = keplerian_spec();
        let rebound_1 = fault::rebind_gmat_spec_at_state(&spec, [7_000_000.0, 0.0, 0.0, 0.0, 7_500.0, 0.0]);
        let rebound_2 = fault::rebind_gmat_spec_at_state(&spec, [-6_800_000.0, 500_000.0, 100_000.0, -50.0, -7_400.0, 300.0]);
        assert_eq!(gmat_settings(&rebound_1), gmat_settings(&rebound_2), "two re-materializations of the identical configuration, at two different physical states, must hash equal");
    }

    /// Required test: a real force-model DYNAMICS fault (`force_model.gravity_order`, the demo
    /// fixture's own real-GMAT fault target) still changes the hash after a rebind -- the general
    /// "a fault changes the hash" contract `tests/segment_merge.rs` already proves for a native
    /// binding, pinned here directly against `gmat_settings` for the GMAT path. Fails against an
    /// implementation that (incorrectly) also excludes `force_model.*` fields, or that stopped
    /// reading `gravity_order` into the map at all.
    #[test]
    fn gmat_settings_still_changes_when_gravity_order_differs_after_a_rebind() {
        let mut faulted = keplerian_spec();
        faulted.gravity_order = 0;
        let state = [7_000_000.0, 0.0, 0.0, 0.0, 7_500.0, 0.0];
        let rebound_a = fault::rebind_gmat_spec_at_state(&keplerian_spec(), state);
        let rebound_b = fault::rebind_gmat_spec_at_state(&faulted, state);
        assert_ne!(gmat_settings(&rebound_a), gmat_settings(&rebound_b), "force_model.gravity_order 8 -> 0 must still change the settings map after a rebind");
    }

    // ========================================================================================
    // M19.4 (`docs/open-questions.md` question 131): atmospheric drag on a `"gmat."`-dispatched
    // SystemDefinition, and range-condition gating on the native `ConstantAccelModel`. Every
    // test below fails against the pre-M19.4 implementation, which has no `drag_*`/`condition.*`
    // vocabulary at all -- any of these parameter names would have hit `parse_gmat_spec`'s or
    // `parse_constant_accel_spec`'s own final catch-all `DrmError::UnknownParameter` arm.
    // ========================================================================================

    /// A well-formed drag declaration (all four `force_model.drag_*` fields) classifies, and the
    /// parsed spec carries every field exactly -- proving `GmatSystemSpec::drag_model` and its
    /// three companions are actually populated, not merely that classification did not error.
    /// Fails against the pre-M19.4 code (`DrmError::UnknownParameter{name: "force_model.drag_
    /// model"}`) and against an implementation that recognizes the names but forgets to write
    /// one of the four spec fields.
    #[test]
    fn a_well_formed_drag_declaration_classifies_and_is_carried_on_the_spec() {
        let (instance, sys) = gmat_instance(
            "leo",
            vec![
                sparam("force_model.drag_model", "JacchiaRoberts"),
                sparam("force_model.drag_historic_weather_source", "CSSISpaceWeatherFile"),
                sparam("force_model.drag_predicted_weather_source", "CSSISpaceWeatherFile"),
                sparam("force_model.drag_cssi_space_weather_file", "SpaceWeather-All-v1.2.txt"),
            ],
        );
        let plan = classify_binding(&instance, &sys, &DrmOptions::default()).expect("a fully-declared drag configuration classifies");
        match plan {
            Classification::Model(BindingPlan::Gmat(spec)) => {
                assert_eq!(spec.drag_model.as_deref(), Some("JacchiaRoberts"));
                assert_eq!(spec.drag_historic_weather_source, "CSSISpaceWeatherFile");
                assert_eq!(spec.drag_predicted_weather_source, "CSSISpaceWeatherFile");
                assert_eq!(spec.drag_cssi_space_weather_file, "SpaceWeather-All-v1.2.txt");
            }
            other => panic!("expected Classification::Model(BindingPlan::Gmat(_)), got {other:?}"),
        }
    }

    /// `"force_model.drag_model"` alone, without its three weather-source companions, is a typed
    /// `DrmError::MissingParameter` naming the first absent companion -- mirrors `GmatSystemSpec::
    /// emit`/`consume`'s own all-or-nothing pairing rule. Fails against an implementation that
    /// treats `drag_model` alone as sufficient (silently running `DragForce` on GMAT's own
    /// `"ConstantFluxAndGeoMag"` default rather than the declared packaged file) or that returns
    /// the wrong error variant/name.
    #[test]
    fn drag_model_alone_without_its_weather_companions_is_a_typed_missing_parameter_error() {
        let (instance, sys) = gmat_instance("leo", vec![sparam("force_model.drag_model", "JacchiaRoberts")]);
        let err = classify_binding(&instance, &sys, &DrmOptions::default()).unwrap_err();
        assert!(matches!(err, DrmError::MissingParameter { ref name, .. } if name == "force_model.drag_historic_weather_source"), "{err:?}");
    }

    /// The symmetric case: the three weather-source fields declared without `drag_model` at all
    /// is refused naming `drag_model` specifically, not one of the fields that happen to be
    /// present.
    #[test]
    fn drag_weather_fields_without_drag_model_is_a_typed_missing_parameter_error() {
        let (instance, sys) = gmat_instance(
            "leo",
            vec![
                sparam("force_model.drag_historic_weather_source", "CSSISpaceWeatherFile"),
                sparam("force_model.drag_predicted_weather_source", "CSSISpaceWeatherFile"),
                sparam("force_model.drag_cssi_space_weather_file", "SpaceWeather-All-v1.2.txt"),
            ],
        );
        let err = classify_binding(&instance, &sys, &DrmOptions::default()).unwrap_err();
        assert!(matches!(err, DrmError::MissingParameter { ref name, .. } if name == "force_model.drag_model"), "{err:?}");
    }

    /// No drag parameters at all (the overwhelming majority of this crate's own GMAT fixtures,
    /// `leo_demo_sys`'s own `demo_mvr` instance included) still classifies with `drag_model ==
    /// None` -- proves M19.4 is additive, not a behaviour change for every pre-existing
    /// GMAT-bound DRM in this repository.
    #[test]
    fn no_drag_parameters_at_all_still_classifies_with_drag_model_none() {
        let (instance, sys) = gmat_instance("leo", vec![]);
        let plan = classify_binding(&instance, &sys, &DrmOptions::default()).expect("no drag_* parameters is the pre-M19.4 default shape");
        match plan {
            Classification::Model(BindingPlan::Gmat(spec)) => assert_eq!(spec.drag_model, None),
            other => panic!("expected Classification::Model(BindingPlan::Gmat(_)), got {other:?}"),
        }
    }

    /// A declared drag configuration is real, hashed configuration (question 11): it must change
    /// `gmat_settings`'s own output, both absolutely and across a rebind (the same "state
    /// representation changes, configuration does not" guarantee `gmat_settings_is_unchanged_
    /// across_a_rebind_that_changed_only_state_representation` already proves for the pre-M19.4
    /// fields). Fails against an implementation that adds the drag fields to `GmatSystemSpec` and
    /// `materialize_gmat` but forgets to also add them to `gmat_settings` -- a `dynamics_hash`
    /// that cannot tell a drag-inclusive force model from a drag-free one apart, silently
    /// breaking question 127/130's "equal `dynamics_hash` means equal configuration" contract.
    #[test]
    fn gmat_settings_changes_when_drag_is_declared_and_survives_a_rebind() {
        let mut dragged = keplerian_spec();
        dragged.drag_model = Some("JacchiaRoberts".to_string());
        dragged.drag_historic_weather_source = "CSSISpaceWeatherFile".to_string();
        dragged.drag_predicted_weather_source = "CSSISpaceWeatherFile".to_string();
        dragged.drag_cssi_space_weather_file = "SpaceWeather-All-v1.2.txt".to_string();

        assert_ne!(gmat_settings(&keplerian_spec()), gmat_settings(&dragged), "declaring drag must change the settings map");

        let state = [7_000_000.0, 0.0, 0.0, 0.0, 7_500.0, 0.0];
        let rebound_no_drag = fault::rebind_gmat_spec_at_state(&keplerian_spec(), state);
        let rebound_dragged = fault::rebind_gmat_spec_at_state(&dragged, state);
        assert_ne!(gmat_settings(&rebound_no_drag), gmat_settings(&rebound_dragged), "the drag difference must still show up in the hash after a rebind, not just before one");

        // And: a rebind must not silently drop the drag declaration (`rebind_gmat_spec_at_state`
        // clones the whole spec, so this should hold automatically -- a regression here would
        // mean a future refactor started reconstructing the spec field by field instead).
        assert_eq!(rebound_dragged.drag_model.as_deref(), Some("JacchiaRoberts"), "drag configuration must survive a rebind");
    }

    /// `native_instance`: the `parse_constant_accel_spec`/`condition.*` counterpart to
    /// `gmat_instance` above -- a minimal, otherwise-valid native (non-`"gmat."`,
    /// non-`"remote."`) instance/system pair, extended with whatever extra parameters a test
    /// needs.
    fn native_instance(name: &str, params: Vec<Parameter>) -> (SystemInstance, SystemDefinition) {
        let instance = SystemInstance {
            name: name.to_string(),
            system_id: "ctrl_sys".to_string(),
            binding: Some(Binding { kind: BindingKind::Model as i32, config: Some(av_cdm::pb::binding::Config::Model(ModelBinding { model_id: "ctrl_sys".to_string() })) }),
            ..Default::default()
        };
        let mut all_params = vec![
            sparam("frame_id", "test.frame"),
            param("state.px", 0.0),
            param("state.py", 0.0),
            param("state.pz", 0.0),
            param("state.vx", 0.0),
            param("state.vy", 0.0),
            param("state.vz", 0.0),
        ];
        all_params.extend(params);
        // M20.1 (question 133): every `classify_binding` call for a `"native."`-dispatched
        // instance now resolves and dimension-checks its own effective state space
        // (`CONSTANT_ACCEL_STATE_DIM`'s own doc comment) -- this helper declares the real,
        // registered `native.controller.scalar6` id (falls back to `state_space_for`'s built-in
        // registry entry, `state_space: None`) so every existing test built on `native_instance`
        // keeps classifying, exactly as it did before this task.
        let sys = SystemDefinition {
            id: "ctrl_sys".to_string(),
            dynamics_model: "native.range_condition_controller".to_string(),
            state_space_id: crate::trajectory::NATIVE_CONTROLLER_SCALAR6_ID.to_string(),
            parameters: all_params,
            ..Default::default()
        };
        (instance, sys)
    }

    /// A well-formed range condition (declared together with the `port.consume`/`port.emit` pair
    /// it gates) classifies, and the parsed spec carries the exact threshold/direction -- proving
    /// `ConstantAccelSpec::condition` is actually populated. Fails against the pre-M19.4 code
    /// (`DrmError::UnknownParameter{name: "condition.threshold_m"}`, since neither `"condition."`
    /// name was recognized at all) and against an implementation that recognizes the names but
    /// forgets to write `spec.condition`.
    #[test]
    fn a_well_formed_range_condition_classifies_and_is_carried_on_the_spec() {
        let (instance, sys) = native_instance(
            "ctrl",
            vec![sparam("port.consume", "range_in"), sparam("port.emit", "cmd_out"), param("port.emit_value", 220.0), param("condition.threshold_m", 6_884_400.0), sparam("condition.mode", "above")],
        );
        let plan = classify_binding(&instance, &sys, &DrmOptions::default()).expect("a valid range condition classifies");
        match plan {
            Classification::Model(BindingPlan::ConstantAccel(spec)) => {
                assert_eq!(spec.condition, Some(RangeCondition { threshold_m: 6_884_400.0, above: true }));
                assert_eq!(spec.emit, Some(("cmd_out".to_string(), 220.0)));
                assert_eq!(spec.consume_port, Some("range_in".to_string()));
            }
            other => panic!("expected Classification::Model(BindingPlan::ConstantAccel(_)), got {other:?}"),
        }
    }

    /// `"condition.mode"` = `"below"` is carried as `RangeCondition::above == false`.
    #[test]
    fn condition_mode_below_is_carried_as_above_false() {
        let (instance, sys) = native_instance(
            "ctrl",
            vec![sparam("port.consume", "range_in"), sparam("port.emit", "cmd_out"), param("port.emit_value", 1.0), param("condition.threshold_m", 5.0), sparam("condition.mode", "below")],
        );
        let plan = classify_binding(&instance, &sys, &DrmOptions::default()).expect("a valid range condition classifies");
        match plan {
            Classification::Model(BindingPlan::ConstantAccel(spec)) => assert_eq!(spec.condition, Some(RangeCondition { threshold_m: 5.0, above: false })),
            other => panic!("expected Classification::Model(BindingPlan::ConstantAccel(_)), got {other:?}"),
        }
    }

    /// `"condition.threshold_m"` without its required `"condition.mode"` partner is a typed
    /// `DrmError::MissingParameter`.
    #[test]
    fn condition_threshold_without_mode_is_a_typed_missing_parameter_error() {
        let (instance, sys) = native_instance("ctrl", vec![sparam("port.consume", "range_in"), sparam("port.emit", "cmd_out"), param("port.emit_value", 1.0), param("condition.threshold_m", 5.0)]);
        let err = classify_binding(&instance, &sys, &DrmOptions::default()).unwrap_err();
        assert!(matches!(err, DrmError::MissingParameter { ref name, .. } if name == "condition.mode"), "{err:?}");
    }

    /// The symmetric case: `"condition.mode"` without `"condition.threshold_m"`.
    #[test]
    fn condition_mode_without_threshold_is_a_typed_missing_parameter_error() {
        let (instance, sys) = native_instance("ctrl", vec![sparam("port.consume", "range_in"), sparam("port.emit", "cmd_out"), param("port.emit_value", 1.0), sparam("condition.mode", "above")]);
        let err = classify_binding(&instance, &sys, &DrmOptions::default()).unwrap_err();
        assert!(matches!(err, DrmError::MissingParameter { ref name, .. } if name == "condition.threshold_m"), "{err:?}");
    }

    /// `"condition.mode"` naming anything other than `"above"`/`"below"` is a typed refusal at
    /// load time, never a silent default to one direction or the other.
    #[test]
    fn condition_mode_naming_an_unrecognized_value_is_a_typed_refusal_at_load_time() {
        let (instance, sys) = native_instance(
            "ctrl",
            vec![sparam("port.consume", "range_in"), sparam("port.emit", "cmd_out"), param("port.emit_value", 1.0), param("condition.threshold_m", 5.0), sparam("condition.mode", "sideways")],
        );
        let err = classify_binding(&instance, &sys, &DrmOptions::default()).unwrap_err();
        assert!(matches!(err, DrmError::UnknownParameter { ref name, .. } if name.contains("condition.mode") && name.contains("sideways")), "{err:?}");
    }

    /// A declared condition with no `port.emit` at all (nothing to gate) is refused -- a range
    /// condition that gates nothing is a load-time mistake, not a silent no-op.
    #[test]
    fn condition_without_port_emit_is_a_typed_missing_parameter_error() {
        let (instance, sys) = native_instance("ctrl", vec![sparam("port.consume", "range_in"), param("condition.threshold_m", 5.0), sparam("condition.mode", "above")]);
        let err = classify_binding(&instance, &sys, &DrmOptions::default()).unwrap_err();
        assert!(matches!(err, DrmError::MissingParameter { ref name, .. } if name.contains("port.emit")), "{err:?}");
    }

    /// A declared condition with no `port.consume` at all (nothing to evaluate the condition
    /// against) is refused.
    #[test]
    fn condition_without_port_consume_is_a_typed_missing_parameter_error() {
        let (instance, sys) = native_instance("ctrl", vec![sparam("port.emit", "cmd_out"), param("port.emit_value", 1.0), param("condition.threshold_m", 5.0), sparam("condition.mode", "above")]);
        let err = classify_binding(&instance, &sys, &DrmOptions::default()).unwrap_err();
        assert!(matches!(err, DrmError::MissingParameter { ref name, .. } if name.contains("port.consume")), "{err:?}");
    }

    /// M20.1 (question 133, decided by the lead): a `"native."`-dispatched instance whose
    /// declared state space has a component count different from
    /// [`CONSTANT_ACCEL_STATE_DIM`] (6) is refused with a typed
    /// [`DrmError::StateSpaceDimensionMismatch`] -- never accepted, and never a panic
    /// (`resolve_state_space`'s own `Result` is threaded through, not `unwrap`ped). Fails
    /// against the pre-M20.1 code, which had no such check at all (a fixture is free to
    /// declare any component count -- in particular `demo_two_instance_ctrl.system.yaml`'s
    /// own pre-M20.1 declaration of the 6-component *Cartesian position/velocity* shape for
    /// a purely non-physical controller, this task's own found defect, classified without
    /// complaint), and against an implementation that checks the count but reports the wrong
    /// `declared_dim`/`model_state_dim` or the wrong instance name.
    #[test]
    fn a_native_instance_declaring_the_wrong_state_space_dimension_is_a_typed_load_error() {
        let (instance, mut sys) = native_instance("ctrl", vec![sparam("port.emit", "cmd_out"), param("port.emit_value", 1.0)]);
        sys.state_space_id = "mission.three_scalars".to_string();
        sys.state_space = Some(av_cdm::pb::StateSpace {
            id: "mission.three_scalars".to_string(),
            components: vec![
                av_cdm::pb::StateComponent { label: "a".to_string(), unit: av_cdm::pb::Unit::Dimensionless as i32 },
                av_cdm::pb::StateComponent { label: "b".to_string(), unit: av_cdm::pb::Unit::Dimensionless as i32 },
                av_cdm::pb::StateComponent { label: "c".to_string(), unit: av_cdm::pb::Unit::Dimensionless as i32 },
            ],
            frame_id: String::new(),
        });
        let err = classify_binding(&instance, &sys, &DrmOptions::default()).unwrap_err();
        assert!(
            matches!(err, DrmError::StateSpaceDimensionMismatch { ref instance, declared_dim: 3, model_state_dim: 6 } if instance == "ctrl"),
            "{err:?}"
        );
    }

    /// The symmetric, positive case: a native instance declaring exactly [`CONSTANT_ACCEL_STATE_DIM`]
    /// (6) components -- even under non-Cartesian labels, `native.controller.scalar6` itself --
    /// classifies normally. Proves the check above is a genuine dimension comparison, not a
    /// blanket refusal of every non-`gmat.orbital.cartesian6` id.
    #[test]
    fn a_native_instance_declaring_exactly_six_scalar_components_classifies_normally() {
        let (instance, sys) = native_instance("ctrl", vec![sparam("port.emit", "cmd_out"), param("port.emit_value", 1.0)]);
        assert_eq!(sys.state_space_id, crate::trajectory::NATIVE_CONTROLLER_SCALAR6_ID, "native_instance's own default state space");
        assert!(matches!(classify_binding(&instance, &sys, &DrmOptions::default()), Ok(Classification::Model(BindingPlan::ConstantAccel(_)))));
    }

    // ----------------------------------------------------------------------------------------
    // M21.3 (`docs/open-questions.md` question 141, decided by the lead, closing question 133's
    // own escalation): a native model's state dimension is taken from the declared state space
    // at materialization, not a fixed constant -- with a typed error when the model cannot
    // honour it, and an empty declared state space materializing at zero width (`spec.x0_si` a
    // genuinely empty `Vec`, never six hidden zeros).
    // ----------------------------------------------------------------------------------------

    /// `native_instance`, but with none of the six `"state.*"` parameters declared at all --
    /// `parse_constant_accel_spec` leaves `spec.x0_si` empty (M21.3) -- and an explicitly
    /// declared EMPTY state space (question 94: a declared `state_space` is authoritative,
    /// no registry entry needed for this synthetic id).
    fn native_instance_empty(name: &str, params: Vec<Parameter>) -> (SystemInstance, SystemDefinition) {
        let instance = SystemInstance {
            name: name.to_string(),
            system_id: "ctrl_sys_empty".to_string(),
            binding: Some(Binding { kind: BindingKind::Model as i32, config: Some(av_cdm::pb::binding::Config::Model(ModelBinding { model_id: "ctrl_sys_empty".to_string() })) }),
            ..Default::default()
        };
        let mut all_params = vec![sparam("frame_id", "test.frame")];
        all_params.extend(params);
        let sys = SystemDefinition {
            id: "ctrl_sys_empty".to_string(),
            dynamics_model: "native.range_condition_controller".to_string(),
            state_space_id: "test.empty".to_string(),
            state_space: Some(av_cdm::pb::StateSpace { id: "test.empty".to_string(), components: vec![], frame_id: String::new() }),
            parameters: all_params,
            ..Default::default()
        };
        (instance, sys)
    }

    /// **Required test: a native instance whose declared state space width the model can honour
    /// (here, 0) materializes at that width.** Fails against the pre-M21.3 code, which refused
    /// any declared width other than the fixed `CONSTANT_ACCEL_STATE_DIM` (6) -- this DRM would
    /// not even classify -- and against an implementation that still forces the model to a
    /// 6-wide state internally while merely relabelling/truncating for the trajectory (would
    /// still report `state_dim() == 6`, or would need `x0_si.len() == 6` to construct at all).
    #[test]
    fn a_native_instance_with_an_empty_declared_state_space_materializes_at_zero_dim() {
        let (instance, sys) = native_instance_empty("ctrl", vec![]);
        let plan = classify_binding(&instance, &sys, &DrmOptions::default()).expect("an empty declared state space, honoured by zero configured state.* parameters, must classify");
        let spec = match plan {
            Classification::Model(BindingPlan::ConstantAccel(spec)) => spec,
            other => panic!("expected Classification::Model(BindingPlan::ConstantAccel(_)), got {other:?}"),
        };
        assert_eq!(spec.x0_si, Vec::<f64>::new(), "no state.* parameters declared -> a genuinely empty Vec, not six hidden zeros");
        let mat = materialize_constant_accel(&spec, 0, "native.empty_test", "test.empty");
        assert_eq!(mat.model.state_dim(), 0, "the materialized model's own state_dim must equal the declared (empty) width");
        assert_eq!(mat.x0_si, Vec::<f64>::new());
        // Honestly empty, not merely reporting 0 while still able to integrate 6 components:
        // derivatives on empty state/out slices must not panic and must leave nothing behind.
        let mut out: Vec<f64> = vec![];
        mat.model.derivatives(&[], mat.t0_tai_ns, &[], &mut out).expect("a dim-0 model's derivatives is a no-op, not a panic");
        assert!(out.is_empty());
    }

    /// **Required test: a declared width the model cannot honour is a typed `DrmError` --
    /// asserting the variant, not merely that an error occurred.** Symmetric to the pre-existing
    /// `a_native_instance_declaring_the_wrong_state_space_dimension_is_a_typed_load_error`
    /// (declared 3, configured 6): here the instance declares the ordinary 6-component
    /// `native.controller.scalar6` shape but configures NONE of the six `"state.*"`
    /// parameters, so `spec.x0_si.len() == 0` disagrees with `declared_dim == 6` -- the model
    /// cannot honour a declared width its own configuration never actually produced. Fails
    /// against an implementation that only compares against a fixed constant (would wrongly
    /// accept, since 6 == CONSTANT_ACCEL_STATE_DIM) instead of against what this instance
    /// actually configured.
    #[test]
    fn a_native_instance_declaring_six_components_but_configuring_no_physical_state_is_a_typed_load_error() {
        let instance = SystemInstance {
            name: "ctrl".to_string(),
            system_id: "ctrl_sys".to_string(),
            binding: Some(Binding { kind: BindingKind::Model as i32, config: Some(av_cdm::pb::binding::Config::Model(ModelBinding { model_id: "ctrl_sys".to_string() })) }),
            ..Default::default()
        };
        let sys = SystemDefinition {
            id: "ctrl_sys".to_string(),
            dynamics_model: "native.range_condition_controller".to_string(),
            state_space_id: crate::trajectory::NATIVE_CONTROLLER_SCALAR6_ID.to_string(),
            parameters: vec![sparam("frame_id", "test.frame")], // deliberately no state.* at all
            ..Default::default()
        };
        let err = classify_binding(&instance, &sys, &DrmOptions::default()).unwrap_err();
        assert!(
            matches!(err, DrmError::StateSpaceDimensionMismatch { ref instance, declared_dim: 6, model_state_dim: 0 } if instance == "ctrl"),
            "{err:?}"
        );
    }

    /// The symmetric case the other way: an empty declared state space, but the instance still
    /// configures all six `"state.*"` parameters (a fixture lying in the opposite direction --
    /// claiming no physical state while still wiring one up). Also a typed load error, never
    /// silently accepted with the physical state simply never surfacing.
    #[test]
    fn a_native_instance_declaring_an_empty_state_space_but_configuring_six_state_parameters_is_a_typed_load_error() {
        let (instance, sys) = native_instance_empty(
            "ctrl",
            vec![param("state.px", 0.0), param("state.py", 0.0), param("state.pz", 0.0), param("state.vx", 0.0), param("state.vy", 0.0), param("state.vz", 0.0)],
        );
        let err = classify_binding(&instance, &sys, &DrmOptions::default()).unwrap_err();
        assert!(
            matches!(err, DrmError::StateSpaceDimensionMismatch { ref instance, declared_dim: 0, model_state_dim: 6 } if instance == "ctrl"),
            "{err:?}"
        );
    }

    /// `"state.*"` is all-six-or-none: declaring three of the six is refused by name (the first
    /// missing one), not silently accepted as a shorter `Vec`. Proves `parse_constant_accel_spec`
    /// itself still refuses a partial subset after M21.3's change (only the "0 or 6" shapes are
    /// ever built), independent of `classify_binding`'s own dimension cross-check above.
    #[test]
    fn a_native_instance_declaring_a_partial_subset_of_state_parameters_is_a_typed_missing_parameter_error() {
        let (instance, sys) = native_instance_empty("ctrl", vec![param("state.px", 1.0), param("state.py", 2.0), param("state.pz", 3.0)]);
        let err = classify_binding(&instance, &sys, &DrmOptions::default()).unwrap_err();
        assert!(matches!(err, DrmError::MissingParameter { ref name, .. } if name == "state.vx"), "{err:?}");
    }

    /// No condition parameters at all (every pre-M19.4 native fixture in this crate, `demo_mvr`'s
    /// own prior wiring included) still classifies with `condition == None` and `emit`'s own
    /// pre-M19.4 unconditional-every-step contract untouched -- proven directly against
    /// `ConstantAccelModel::step_with_ports` below
    /// (`any_model_step_with_ports_delegates_to_the_constant_accel_variant`, already passing,
    /// covers the behavioural half; this covers the parse half).
    #[test]
    fn no_condition_parameters_at_all_still_classifies_with_condition_none() {
        let (instance, sys) = native_instance("ctrl", vec![sparam("port.emit", "cmd_out"), param("port.emit_value", 1.0)]);
        let plan = classify_binding(&instance, &sys, &DrmOptions::default()).expect("no condition.* parameters is the pre-M19.4 default shape");
        match plan {
            Classification::Model(BindingPlan::ConstantAccel(spec)) => assert_eq!(spec.condition, None),
            other => panic!("expected Classification::Model(BindingPlan::ConstantAccel(_)), got {other:?}"),
        }
    }

    /// **The headline behavioural test: edge-triggered, latched emission.** Drives
    /// `ConstantAccelModel::step_with_ports` directly (bypassing the parser -- this is about
    /// runtime behaviour, not classification) across three steps: below threshold (no emit),
    /// crossing above threshold (emits exactly once), still above threshold on a later step (does
    /// NOT emit again). Fails against an implementation that (a) never gates `emit` on
    /// `condition` at all (would emit on every one of the three steps, reproducing the ~72,000-
    /// event regression this task fixes), or (b) gates correctly but forgets to latch (would
    /// emit on both the second AND third step, since the condition is `true` on both).
    #[test]
    fn condition_above_fires_exactly_once_on_the_crossing_step_and_never_again() {
        let model = ConstantAccelModel {
            a: [0.0; 3],
            info: ModelInfo::default(),
            emit: Some(("cmd_out".to_string(), 220.0)),
            consume_port: Some("range_in".to_string()),
            condition: Some(RangeCondition { threshold_m: 10.0, above: true }),
            already_fired: Cell::new(false),
            emit_framed: None,
            framed_seq: Cell::new(0),
            dim: 6,
            consume_framed: None,
            commanded_accel_scale: Cell::new(1.0),
            last_applied_command_value: Cell::new(None),
            ack_framed: None,
            decode_errors_this_step: RefCell::new(Vec::new()),
        };
        let state = [0.0; 6];
        let inbox_with = |v: f64| Inbox::new(vec![av_cdm::pb::PortMessage { port: "range_in".to_string(), tai_ns: 0, payload: av_dynamics::encode_signal(v) }]);

        let (_, outbox_below, applied_below) = model.step_with_ports(&state, 0, &[], 1_000_000_000, &inbox_with(5.0)).unwrap();
        assert!(outbox_below.messages().is_empty(), "below threshold: must not emit yet");
        assert!(applied_below.is_empty());

        let (_, outbox_cross, _) = model.step_with_ports(&state, 1_000_000_000, &[], 1_000_000_000, &inbox_with(15.0)).unwrap();
        assert_eq!(outbox_cross.messages().len(), 1, "crossing the threshold must emit exactly one message");
        assert_eq!(outbox_cross.messages()[0].port, "cmd_out");
        assert_eq!(av_dynamics::decode_signal(&outbox_cross.messages()[0].payload), Some(220.0));

        let (_, outbox_still_above, _) = model.step_with_ports(&state, 2_000_000_000, &[], 1_000_000_000, &inbox_with(20.0)).unwrap();
        assert!(outbox_still_above.messages().is_empty(), "already fired: must never re-emit within the same materialization even though the condition still holds");
    }

    /// The `"below"` direction, and the "no message this step" case (`received == None` must
    /// never be treated as satisfying either direction).
    #[test]
    fn condition_below_fires_when_the_value_drops_to_or_under_threshold_never_on_a_silent_step() {
        let model = ConstantAccelModel {
            a: [0.0; 3],
            info: ModelInfo::default(),
            emit: Some(("cmd_out".to_string(), 8.0)),
            consume_port: Some("range_in".to_string()),
            condition: Some(RangeCondition { threshold_m: 10.0, above: false }),
            already_fired: Cell::new(false),
            emit_framed: None,
            framed_seq: Cell::new(0),
            dim: 6,
            consume_framed: None,
            commanded_accel_scale: Cell::new(1.0),
            last_applied_command_value: Cell::new(None),
            ack_framed: None,
            decode_errors_this_step: RefCell::new(Vec::new()),
        };
        let state = [0.0; 6];

        // No message at all this step: must not emit, regardless of direction.
        let (_, outbox_silent, _) = model.step_with_ports(&state, 0, &[], 1_000_000_000, &Inbox::empty()).unwrap();
        assert!(outbox_silent.messages().is_empty(), "no consumed value this step: must not emit");

        // Above threshold: condition (below) does not hold yet.
        let above = Inbox::new(vec![av_cdm::pb::PortMessage { port: "range_in".to_string(), tai_ns: 0, payload: av_dynamics::encode_signal(15.0) }]);
        let (_, outbox_above, _) = model.step_with_ports(&state, 1_000_000_000, &[], 1_000_000_000, &above).unwrap();
        assert!(outbox_above.messages().is_empty());

        // Drops to exactly the threshold: fires (>=/<= are both inclusive by design).
        let at = Inbox::new(vec![av_cdm::pb::PortMessage { port: "range_in".to_string(), tai_ns: 0, payload: av_dynamics::encode_signal(10.0) }]);
        let (_, outbox_at, _) = model.step_with_ports(&state, 2_000_000_000, &[], 1_000_000_000, &at).unwrap();
        assert_eq!(outbox_at.messages().len(), 1);
        assert_eq!(av_dynamics::decode_signal(&outbox_at.messages()[0].payload), Some(8.0));
    }

    /// M25.1: a `ConstantAccelModel` with `emit_framed` set broadcasts its own propagated
    /// Cartesian position as one CCSDS packet, every step, decodable by
    /// `crate::drm::ground::ground_tm_packet_codec`'s own field convention -- fails against an
    /// implementation that never checks `emit_framed` (empty outbox), that broadcasts the
    /// pre-step position instead of the propagated one, or that gates it on `condition` the way
    /// `emit` is gated (unconditional telemetry, not one-shot command, must fire on every step
    /// regardless of `condition`/`consume_port`).
    #[test]
    fn constant_accel_model_with_emit_framed_broadcasts_its_propagated_position_every_step() {
        let codec = ground::ground_tm_packet_codec("flight_tm_codec", 500);
        let model = ConstantAccelModel {
            a: [0.0, 0.0, 0.0],
            info: ModelInfo::default(),
            emit: None,
            consume_port: None,
            condition: None,
            already_fired: Cell::new(false),
            emit_framed: Some(("tm_out".to_string(), codec.clone())),
            framed_seq: Cell::new(0),
            dim: 6,
            consume_framed: None,
            commanded_accel_scale: Cell::new(1.0),
            last_applied_command_value: Cell::new(None),
            ack_framed: None,
            decode_errors_this_step: RefCell::new(Vec::new()),
        };
        // x0 = (1000, 2000, 3000) m, v0 = (10, 0, 0) m/s, a = 0 -> after 1s, x = (1010, 2000, 3000).
        let state = [1000.0, 2000.0, 3000.0, 10.0, 0.0, 0.0];
        let (_result, outbox, _applied) = model.step_with_ports(&state, 0, &[], 1_000_000_000, &Inbox::empty()).unwrap();
        assert_eq!(outbox.messages().len(), 1, "emit_framed must broadcast unconditionally, every step");
        let msg = &outbox.messages()[0];
        assert_eq!(msg.port, "tm_out");
        let mut apid_map = crate::codec::ApidMap::new();
        apid_map.insert(codec.apid, codec);
        let decoded = crate::codec::decode_packet(&apid_map, &msg.payload).expect("must decode as a valid CCSDS packet against its own declared codec");
        assert_eq!(decoded.fields.get("x"), Some(&crate::codec::FieldValue::Numeric(1010.0)));
        assert_eq!(decoded.fields.get("y"), Some(&crate::codec::FieldValue::Numeric(2000.0)));
        assert_eq!(decoded.fields.get("z"), Some(&crate::codec::FieldValue::Numeric(3000.0)));
    }

    // =========================================================================================
    // M25.2 (`docs/sil-plan.md`'s M25 milestone, "Job 1: the flight-side FRAMED consume"):
    // ConstantAccelModel::consume_framed/.ack_framed.
    // =========================================================================================

    fn command_in_codec() -> PacketCodec {
        crate::drm::command::command_out_packet_codec("flight_cmd_in_codec", 600)
    }
    fn ack_out_codec() -> PacketCodec {
        crate::drm::command::command_ack_packet_codec("flight_ack_out_codec", 601)
    }
    fn command_packet(codec: &PacketCodec, seq: u16, value: f64) -> av_dynamics::PortMessage {
        let mut values = BTreeMap::new();
        values.insert("value".to_string(), crate::codec::FieldValue::Numeric(value));
        av_dynamics::PortMessage { port: "cmd_in".to_string(), tai_ns: 0, payload: crate::codec::encode_packet(codec, seq, &[], &values).unwrap() }
    }
    fn command_consuming_model(a: [f64; 3]) -> ConstantAccelModel {
        ConstantAccelModel {
            a,
            info: ModelInfo::default(),
            emit: None,
            consume_port: None,
            condition: None,
            already_fired: Cell::new(false),
            emit_framed: None,
            framed_seq: Cell::new(0),
            dim: 6,
            consume_framed: Some(("cmd_in".to_string(), command_in_codec(), "accel_scale".to_string())),
            commanded_accel_scale: Cell::new(1.0),
            last_applied_command_value: Cell::new(None),
            ack_framed: Some(("ack_out".to_string(), ack_out_codec())),
            decode_errors_this_step: RefCell::new(Vec::new()),
        }
    }

    /// **The headline behavioural test for Job 1.** A decoded `consume_framed` packet (a) is
    /// applied immediately -- the *same* step's own `derivatives` (hence `self.step`'s own
    /// propagated state) already reflects the commanded `accel_scale`, not merely a later step
    /// (mirrors `gmat_sys::model::GmatModel::step_with_ports`'s own "consume, then step"
    /// ordering); (b) is reported as exactly one `AppliedCommand`; (c) triggers exactly one ack
    /// packet on `ack_framed`'s own port, carrying the command packet's own `sequence_count`.
    /// Fails against an implementation that (i) never decodes `consume_framed` at all (accel
    /// stays at the declared `a`, no `AppliedCommand`, no ack -- the pre-Job-1 state this task's
    /// own brief calls "nowhere to land"); (ii) decodes but applies too late for this step's own
    /// `derivatives` to see it (the propagated position would match the *undoubled* acceleration,
    /// not the doubled one asserted below); or (iii) never sends the ack.
    #[test]
    fn consume_framed_applies_within_the_same_step_reports_it_and_sends_an_ack() {
        let model = command_consuming_model([0.0, 0.0, 2.0]);
        // x0 = 0, v0 = 0, a = (0,0,2) doubled by accel_scale=2.0 -> (0,0,4): after 1s,
        // pos_z = 0.5 * 4 * 1^2 = 2.0 (closed-form double integrator, RK4-exact for constant a).
        let state = [0.0; 6];
        let inbox = Inbox::new(vec![command_packet(&model.consume_framed.as_ref().unwrap().1, 7, 2.0)]);
        let (result, outbox, applied) = model.step_with_ports(&state, 0, &[], 1_000_000_000, &inbox).unwrap();

        assert_eq!(model.commanded_accel_scale.get(), 2.0, "the commanded scale must be applied");
        assert!((result.state[2] - 2.0).abs() < 1e-9, "the SAME step's own propagated position must already reflect the commanded accel_scale; got z={}", result.state[2]);

        assert_eq!(applied.len(), 1, "a changed value must be reported as exactly one AppliedCommand");
        assert_eq!(applied[0].field, "accel_scale");
        assert_eq!(applied[0].value, 2.0);
        assert_eq!(applied[0].port, "cmd_in");

        assert_eq!(outbox.messages().len(), 1, "exactly one ack packet, nothing on cmd_in/emit_framed since neither is declared to broadcast here");
        assert_eq!(outbox.messages()[0].port, "ack_out");
        let mut apid_map = crate::codec::ApidMap::new();
        apid_map.insert(ack_out_codec().apid, ack_out_codec());
        let decoded = crate::codec::decode_packet(&apid_map, &outbox.messages()[0].payload).expect("ack packet must decode against its own declared codec");
        assert_eq!(decoded.fields.get("cmd_seq"), Some(&crate::codec::FieldValue::Numeric(7.0)), "the ack must echo the command packet's own CCSDS sequence_count");
    }

    /// M20.3-style (question 137) "changed, or first": re-sending the IDENTICAL value a second
    /// step must not report a second `AppliedCommand` or send a second ack -- fails against an
    /// implementation that reports/acks unconditionally on every decoded message regardless of
    /// whether anything actually changed (the same event-storm class of bug M19.4's own
    /// port-command-event regression was).
    #[test]
    fn consume_framed_does_not_reapply_report_or_ack_an_unchanged_value() {
        let model = command_consuming_model([0.0, 0.0, 1.0]);
        let codec = model.consume_framed.as_ref().unwrap().1.clone();
        let inbox1 = Inbox::new(vec![command_packet(&codec, 1, 3.0)]);
        let (_r1, o1, applied1) = model.step_with_ports(&[0.0; 6], 0, &[], 1_000_000_000, &inbox1).unwrap();
        assert_eq!(applied1.len(), 1);
        assert_eq!(o1.messages().len(), 1);

        // Second step, same commanded value (a different sequence_count -- a real stream would
        // never repeat one -- but the SAME engineering value): must not re-report or re-ack.
        let inbox2 = Inbox::new(vec![command_packet(&codec, 2, 3.0)]);
        let (_r2, o2, applied2) = model.step_with_ports(&[0.0; 6], 1_000_000_000, &[], 1_000_000_000, &inbox2).unwrap();
        assert!(applied2.is_empty(), "an unchanged value must not be reported a second time");
        assert!(o2.messages().is_empty(), "an unchanged value must not be acked a second time");
    }

    /// **Job 1's own "proven a no-op for every existing test" bar, made explicit and direct.**
    /// `consume_framed: None` (every fixture before M25.2): a message on the port a *would-be*
    /// command port would use is simply never looked at, `commanded_accel_scale` stays `1.0`, the
    /// propagated state is byte-identical to a plain `step` call, and no ack is ever sent even
    /// when `ack_framed` is (degenerately) set. Fails against an implementation that reaches for
    /// `self.consume_framed` unconditionally instead of behind its own `Option`.
    #[test]
    fn consume_framed_none_is_a_byte_identical_no_op_even_with_a_stray_message_on_the_same_port_name() {
        let mut model = command_consuming_model([1.0, 2.0, 3.0]);
        model.consume_framed = None; // the pre-M25.2 shape every existing fixture has.
        let state = [0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let codec = command_in_codec();
        let stray = Inbox::new(vec![command_packet(&codec, 9, 999.0)]);
        let (with_stray, outbox, applied) = model.step_with_ports(&state, 0, &[], 1_000_000_000, &stray).unwrap();
        let (without_message, _, _) = model.step_with_ports(&state, 0, &[], 1_000_000_000, &Inbox::empty()).unwrap();
        assert_eq!(with_stray.state, without_message.state, "a message on the same port name, with consume_framed unset, must not perturb the propagated state at all");
        assert_eq!(model.commanded_accel_scale.get(), 1.0);
        assert!(applied.is_empty());
        assert!(outbox.messages().is_empty(), "ack_framed is set but consume_framed is None: nothing was ever applied, so nothing is ever acked");
    }

    /// **Question 188 (R5.2).** An undecodable `consume_framed` frame (a `900`-declared APID,
    /// this packet names `901`) is recorded via `drain_decode_errors`, never propagated: the
    /// step still succeeds, `commanded_accel_scale` stays at its own last good (here, the
    /// declared no-op `1.0`) value, no `AppliedCommand`/ack is produced from it, and a later good
    /// command still applies normally. Fails against an implementation that still silently
    /// swallows the occurrence (the pre-R5.2 `if let Ok(...)` shape) -- `drain_decode_errors`
    /// would stay empty instead of reporting one.
    #[test]
    fn consume_framed_records_an_undecodable_frame_and_a_later_good_one_still_applies() {
        let model = command_consuming_model([0.0, 0.0, 2.0]);
        let state = [0.0; 6];
        let bad = av_dynamics::PortMessage { port: "cmd_in".to_string(), tai_ns: 15_000_000_000, payload: vec![0x1F, 0x67, 0xC0, 0x2A, 0x00, 0x07, 0, 0, 0, 0, 0, 0, 0, 0] };
        let (result, outbox, applied) = model.step_with_ports(&state, 0, &[], 1_000_000_000, &Inbox::new(vec![bad])).expect("a decode error must never abort the run");
        assert_eq!(model.commanded_accel_scale.get(), 1.0, "an undecodable command must never fabricate a scale -- the last good (here, the initial no-op) value is kept");
        // a = (0,0,2), accel_scale stays at its own no-op 1.0: pos_z after 1s from rest = 0.5*2*1^2 = 1.0.
        assert!((result.state[2] - 1.0).abs() < 1e-9, "propagation must use the unchanged (no-op) accel_scale, not a fabricated one: got z={}", result.state[2]);
        assert!(applied.is_empty());
        assert!(outbox.messages().is_empty());

        let occurrences = model.drain_decode_errors();
        assert_eq!(occurrences.len(), 1, "{occurrences:?}");
        assert_eq!(occurrences[0].port, "cmd_in");
        assert_eq!(occurrences[0].tai_ns, 15_000_000_000);

        let good = Inbox::new(vec![command_packet(&model.consume_framed.as_ref().unwrap().1, 8, 2.0)]);
        let (_result2, outbox2, applied2) = model.step_with_ports(&state, 1_000_000_000, &[], 1_000_000_000, &good).unwrap();
        assert_eq!(model.commanded_accel_scale.get(), 2.0, "a good command after the bad one must still apply normally");
        assert_eq!(applied2.len(), 1);
        assert_eq!(outbox2.messages().len(), 1);
        assert!(model.drain_decode_errors().is_empty(), "no new decode error this call");
    }

    // =========================================================================================
    // M22.1b (`docs/open-questions.md` questions 151/152): "attitude." classify_binding
    // dispatch, AnyModel::Attitude delegation (M14.3's own rule, question 112's own pattern).
    // =========================================================================================

    /// `attitude_instance`: a minimal, otherwise-valid `"attitude."`-dispatched instance/system
    /// pair with `n_wheels` reaction wheels -- the `attitude` counterpart to `native_instance`/
    /// `gmat_instance` above, extended with whatever extra/overriding parameters a test needs
    /// (an entry in `overrides` with the same name as a baseline default replaces it, via
    /// `effective_parameters`'s own by-name `BTreeMap` merge semantics -- overrides are declared
    /// as `SystemInstance.parameter_overrides`, not `SystemDefinition.parameters`, exactly like
    /// every other binding kind's own test helpers do this).
    fn attitude_instance(name: &str, n_wheels: usize, overrides: Vec<Parameter>) -> (SystemInstance, SystemDefinition) {
        let mut params = vec![param("attitude.inertia.jxx", 10.0), param("attitude.inertia.jyy", 10.0), param("attitude.inertia.jzz", 10.0), param("attitude.q0.x", 0.0), param("attitude.q0.y", 0.0), param("attitude.q0.z", 0.0), param("attitude.q0.w", 1.0), param("attitude.omega0.x", 0.0), param("attitude.omega0.y", 0.0), param("attitude.omega0.z", 0.0)];
        for k in 1..=n_wheels {
            params.push(param(&format!("attitude.wheel.{k}.axis_x"), 1.0));
            params.push(param(&format!("attitude.wheel.{k}.axis_y"), 0.0));
            params.push(param(&format!("attitude.wheel.{k}.axis_z"), 0.0));
            params.push(param(&format!("attitude.wheel.{k}.momentum_limit"), 1.0));
        }
        let instance = SystemInstance {
            name: name.to_string(),
            system_id: "attitude_sys".to_string(),
            binding: Some(Binding { kind: BindingKind::Model as i32, config: Some(av_cdm::pb::binding::Config::Model(ModelBinding { model_id: "attitude_sys".to_string() })) }),
            parameter_overrides: overrides,
            ..Default::default()
        };
        let space = crate::trajectory::attitude_wheels_state_space("test.attitude", n_wheels);
        let sys = SystemDefinition {
            id: "attitude_sys".to_string(),
            dynamics_model: "attitude.wheels_test".to_string(),
            state_space_id: space.id.clone(),
            state_space: Some(space),
            parameters: params,
            ..Default::default()
        };
        (instance, sys)
    }

    /// A well-formed `"attitude."` instance classifies to `BindingPlan::Attitude`, carrying the
    /// parsed spec -- fails against an implementation that never added the
    /// `crate::registry::ModelKind::Attitude` dispatch arm at all (every `"attitude."` id would
    /// still fall through to `ModelKind::Native`, and `parse_constant_accel_spec` would refuse
    /// every `"attitude.*"` parameter name as unrecognized).
    #[test]
    fn a_well_formed_attitude_instance_classifies_to_binding_plan_attitude() {
        let (instance, sys) = attitude_instance("att1", 1, vec![]);
        let plan = classify_binding(&instance, &sys, &DrmOptions::default()).expect("a valid attitude spec classifies");
        match plan {
            Classification::Model(BindingPlan::Attitude(spec)) => {
                assert_eq!(spec.wheel_axes.len(), 1);
                assert_eq!(spec.inertia[0][0], 10.0);
            }
            other => panic!("expected Classification::Model(BindingPlan::Attitude(_)), got {other:?}"),
        }
    }

    /// An unrecognized `"attitude.*"` parameter is a typed `DrmError::InvalidAttitudeSpec`
    /// (wrapping `AttitudeSpecError::UnknownParameter`'s own `Display`), never silently ignored
    /// -- question 152's own "make unrecognized parameters a typed DrmError" requirement. Fails
    /// against an implementation missing the `DrmError::InvalidAttitudeSpec` error-conversion
    /// arm entirely (a compile error) or one that swallows `parse_attitude_spec`'s own `Err` (a
    /// `.unwrap()`/`.ok()` instead of `?`, which would panic or silently drop it instead of
    /// returning a typed `Err` here).
    #[test]
    fn an_unrecognized_attitude_parameter_is_a_typed_invalid_attitude_spec_error() {
        let (instance, sys) = attitude_instance("att1", 0, vec![param("attitude.nonsense", 1.0)]);
        let err = classify_binding(&instance, &sys, &DrmOptions::default()).unwrap_err();
        assert!(matches!(err, DrmError::InvalidAttitudeSpec { ref instance, ref reason } if instance == "att1" && reason.contains("attitude.nonsense")), "{err:?}");
    }

    /// A declared state space whose width disagrees with the parsed wheel count is a typed
    /// `DrmError::StateSpaceDimensionMismatch` -- the same generic guard `classify_binding`'s own
    /// `ModelKind::Native` arm already uses, applied here to the attitude binding kind. Fails
    /// against an implementation that skips this check for `"attitude."` (would instead panic
    /// deep inside `AttitudeWheelsModel::new`'s own array indexing, or -- worse -- silently
    /// accept the mismatch and panic only much later, mid-run, in `derivatives`).
    #[test]
    fn an_attitude_instance_whose_declared_state_space_width_disagrees_with_the_wheel_count_is_refused() {
        let (instance, mut sys) = attitude_instance("att1", 1, vec![]);
        // Declares a 0-wheel (7-component) shape for a 1-wheel spec.
        sys.state_space = Some(crate::trajectory::attitude_wheels_state_space("test.attitude", 0));
        sys.state_space_id = "test.attitude".to_string();
        let err = classify_binding(&instance, &sys, &DrmOptions::default()).unwrap_err();
        assert!(matches!(err, DrmError::StateSpaceDimensionMismatch { ref instance, declared_dim: 7, model_state_dim: 8 } if instance == "att1"), "{err:?}");
    }

    /// A wheel-momentum state-space component declared with the torque unit (M22.1's own
    /// documented approximation) is refused at classify time -- question 151's own "a typed
    /// load error", reached through the real `classify_binding` entry point, not only through
    /// `AttitudeWheelsModel::new` directly (see `attitude::tests::
    /// new_refuses_a_wheel_momentum_component_labelled_with_the_torque_unit` for that unit-level
    /// proof). Fails against an implementation that never threads the resolved state space
    /// into a real `AttitudeWheelsModel::new` call inside `classify_binding` (e.g. one that
    /// only checks the *count* of components, never actually constructing the model to run its
    /// own unit check).
    #[test]
    fn classify_binding_refuses_an_attitude_instance_whose_wheel_momentum_component_is_labelled_with_the_torque_unit() {
        let (instance, mut sys) = attitude_instance("att1", 1, vec![]);
        let mut space = crate::trajectory::attitude_wheels_state_space("test.attitude", 1);
        space.components[crate::trajectory::ATTITUDE_WHEELS_BASE_COMPONENTS].unit = av_cdm::pb::Unit::NewtonMeter as i32;
        sys.state_space = Some(space);
        sys.state_space_id = "test.attitude".to_string();
        let err = classify_binding(&instance, &sys, &DrmOptions::default()).unwrap_err();
        assert!(matches!(err, DrmError::InvalidAttitudeSpec { ref instance, ref reason } if instance == "att1" && reason.contains("UNIT_NEWTON_METER")), "{err:?}");
    }

    fn test_attitude_mat(spec: &AttitudeWheelsSpec, n_wheels: usize) -> Materialized {
        let space = crate::trajectory::attitude_wheels_state_space("test.attitude", n_wheels);
        materialize_attitude(spec, &space, None, 1_700_000_000_000_000_000, "attitude.test").expect("a valid spec/state-space pair must materialize")
    }

    fn simple_attitude_spec(n_wheels: usize) -> AttitudeWheelsSpec {
        AttitudeWheelsSpec {
            inertia: [[10.0, 0.0, 0.0], [0.0, 10.0, 0.0], [0.0, 0.0, 10.0]],
            wheel_axes: (0..n_wheels).map(|_| [1.0, 0.0, 0.0]).collect(),
            wheel_momentum_limits: vec![1.0; n_wheels],
            q0: [0.0, 0.0, 0.0, 1.0],
            omega0: [0.01, 0.0, 0.0],
            wheel_available: vec![true; n_wheels],
            wheel_commanded_torque: vec![0.0; n_wheels],
        }
    }

    // -- Full per-method AnyModel::Attitude delegation coverage (question 112, extended to the
    // new variant per this task's own M14.3 rule: "every DynamicsModel method must be delegated
    // explicitly through both wrappers... this defect class has recurred three times"). ---------

    #[test]
    fn any_model_state_dim_delegates_to_the_attitude_variant() {
        let mat = test_attitude_mat(&simple_attitude_spec(2), 2);
        assert_eq!(mat.model.state_dim(), crate::trajectory::ATTITUDE_WHEELS_BASE_COMPONENTS + 2);
    }

    #[test]
    fn any_model_derivatives_delegates_to_the_attitude_variant() {
        let spec = simple_attitude_spec(0);
        let mat = test_attitude_mat(&spec, 0);
        let state = [0.0, 0.0, 0.0, 1.0, 0.01, 0.0, 0.0];
        let mut out = [0.0; 7];
        mat.model.derivatives(&state, mat.t0_tai_ns, &[], &mut out).unwrap();
        // Pure kinematics at zero torque/wheels: dq_w/dt = -0.5*(qv . omega) = 0 (qv == 0 here);
        // dq_x/dt = 0.5*omega_x = 0.005. Reaches AttitudeWheelsModel::derivatives, not some other
        // computation (e.g. ConstantAccelModel's double-integrator shape, which would leave
        // out[0] at 0.0 here since state[3] there means something else entirely).
        assert!((out[0] - 0.005).abs() < 1e-15, "out[0] = {}", out[0]);
    }

    #[test]
    fn any_model_describe_delegates_to_the_attitude_variant() {
        let spec = simple_attitude_spec(0);
        let space = crate::trajectory::attitude_wheels_state_space("test.attitude", 0);
        let mat = materialize_attitude(&spec, &space, None, 0, "attitude.distinctive_id").unwrap();
        assert_eq!(mat.model.describe().id, "attitude.distinctive_id");
    }

    #[test]
    fn any_model_integrator_delegates_to_the_attitude_variant() {
        // AttitudeWheelsModel never overrides `integrator` either -- same "both sides are the
        // trait's own default" proof `any_model_integrator_delegates_to_the_constant_accel_
        // variant` already gives for that variant.
        let mat = test_attitude_mat(&simple_attitude_spec(0), 0);
        let direct = av_dynamics::integrate::Dopri5::default();
        let got = mat.model.integrator();
        assert_eq!(got.rtol, direct.rtol);
        assert_eq!(got.atol, direct.atol);
    }

    #[test]
    fn any_model_stm_capable_delegates_to_the_attitude_variant() {
        let mat = test_attitude_mat(&simple_attitude_spec(0), 0);
        assert!(!mat.model.stm_capable(), "question 152: covariance for attitude is a typed refusal this batch -- stm_capable must be false");
    }

    #[test]
    fn any_model_stm_derivatives_returns_a_typed_capability_missing_error_for_the_attitude_variant() {
        let mat = test_attitude_mat(&simple_attitude_spec(0), 0);
        let mut out = [0.0; 56];
        let err = mat.model.stm_derivatives(&[0.0; 56], mat.t0_tai_ns, &[], &mut out).unwrap_err();
        assert!(matches!(err, AnyModelError::CapabilityMissing { ref capability, .. } if capability == "stm_derivatives"), "{err:?}");
    }

    #[test]
    fn any_model_step_with_stm_returns_a_typed_capability_missing_error_for_the_attitude_variant() {
        let mat = test_attitude_mat(&simple_attitude_spec(0), 0);
        let err = mat.model.step_with_stm(&mat.x0_si, mat.t0_tai_ns, &[], 1_000_000_000).unwrap_err();
        assert!(matches!(err, AnyModelError::CapabilityMissing { ref capability, .. } if capability == "step_with_stm"), "{err:?}");
    }

    #[test]
    fn any_model_step_delegates_to_the_attitude_variant() {
        // Golden 1's own torque-free axisymmetric precession (crate::drm::attitude::tests::
        // torque_free_axisymmetric_precession_matches_the_closed_form), reproduced through
        // AnyModel::step rather than AttitudeWheelsModel::step directly -- proves AnyModel's own
        // delegation reaches the identical physics, not merely that the concrete model does.
        let (jt, jz) = (100.0_f64, 50.0_f64);
        let spec = AttitudeWheelsSpec { inertia: [[jt, 0.0, 0.0], [0.0, jt, 0.0], [0.0, 0.0, jz]], wheel_axes: vec![], wheel_momentum_limits: vec![], q0: [0.0, 0.0, 0.0, 1.0], omega0: [0.05, 0.03, 0.2], wheel_available: vec![], wheel_commanded_torque: vec![] };
        let mat = test_attitude_mat(&spec, 0);
        let x0 = vec![0.0, 0.0, 0.0, 1.0, 0.05, 0.03, 0.2];
        let step = mat.model.step(&x0, mat.t0_tai_ns, &[], 1_000_000_000).unwrap(); // 1 s
        let lambda = 0.2 * (jz - jt) / jt;
        let want_wx = 0.05 * lambda.cos() - 0.03 * lambda.sin();
        assert!((step.state[4] - want_wx).abs() < 1e-9, "omega_x(1s) = {}, want {want_wx}", step.state[4]);
        assert!((step.state[6] - 0.2).abs() < 1e-12, "omega_z must be exactly conserved, got {}", step.state[6]);
    }

    #[test]
    fn any_model_step_with_ports_delegates_to_the_attitude_variant() {
        // M22.2b: `AnyModel::Attitude` now wraps `sensors::TruthBroadcastAttitude`, which DOES
        // override `step_with_ports` (unlike bare `AttitudeWheelsModel`, which still does not,
        // and still reaches the trait's own default through `TruthBroadcastAttitude::step`) --
        // this proves `AnyModel` delegates all the way to that override (exactly 7 messages, one
        // per `sensors::TRUTH_PORT_NAMES`, in order, carrying the state's own leading 7
        // components) rather than fabricating an empty Outbox itself or silently reaching the
        // trait default the way the pre-M22.2b arm used to. See `AnyModel::Attitude`'s own doc
        // comment for why every attitude instance is now wrapped unconditionally.
        let mat = test_attitude_mat(&simple_attitude_spec(0), 0);
        let (result, outbox, applied) = mat.model.step_with_ports(&mat.x0_si, mat.t0_tai_ns, &[], 1_000_000_000, &Inbox::empty()).unwrap();
        let msgs = outbox.messages();
        assert_eq!(msgs.len(), 7, "one SIGNAL message per truth port, {msgs:?}");
        for (msg, want_port) in msgs.iter().zip(sensors::TRUTH_PORT_NAMES) {
            assert_eq!(msg.port, want_port);
        }
        let decoded: Vec<f64> = msgs.iter().map(|m| av_dynamics::decode_signal(&m.payload).expect("a truth broadcast message decodes as a SIGNAL f64")).collect();
        assert_eq!(decoded, result.state[0..7], "the broadcast truth must be exactly the propagated state's own leading 7 components");
        assert!(applied.is_empty());
        assert_eq!(result.state.len(), 7);
    }

    // =========================================================================================
    // M22.2b (`docs/open-questions.md` questions 142/149/151/152): "startracker."/"imu."
    // classify_binding dispatch, AnyModel::StarTracker/AnyModel::Imu delegation (M14.3's own
    // rule, mirroring the Attitude block immediately above).
    // =========================================================================================

    /// A minimal, otherwise-valid `"startracker."`-dispatched instance/system pair -- the
    /// `startracker` counterpart to `attitude_instance` above: one declared `packet_codecs`
    /// entry and one declared `PORT_KIND_FRAMED`/`PORT_DIRECTION_OUT` port, per
    /// `resolve_sensor_output`'s own convention.
    fn star_tracker_instance(name: &str, overrides: Vec<Parameter>) -> (SystemInstance, SystemDefinition) {
        let params = vec![param("startracker.update_rate_hz", 2.0), param("startracker.seed", 1.0), param("startracker.noise_sigma_rad", 1e-5)];
        let instance = SystemInstance {
            name: name.to_string(),
            system_id: "startracker_sys".to_string(),
            binding: Some(Binding { kind: BindingKind::Model as i32, config: Some(av_cdm::pb::binding::Config::Model(ModelBinding { model_id: "startracker_sys".to_string() })) }),
            parameter_overrides: overrides,
            ..Default::default()
        };
        let space = StateSpace { id: "test.startracker".to_string(), components: vec![], frame_id: String::new() };
        let sys = SystemDefinition {
            id: "startracker_sys".to_string(),
            dynamics_model: "startracker.test".to_string(),
            state_space_id: space.id.clone(),
            state_space: Some(space),
            parameters: params,
            ports: vec![Port { name: "st_meas".to_string(), kind: PortKind::Framed as i32, direction: PortDirection::Out as i32, schema: "ccsds.spp".to_string(), ..Default::default() }],
            packet_codecs: vec![sensors::star_tracker_packet_codec("st_codec", 100)],
            ..Default::default()
        };
        (instance, sys)
    }

    /// The IMU counterpart of `star_tracker_instance` -- a 6-component declared state space
    /// (matching `ImuModel::state_dim() == 6`, the bias random walk).
    fn imu_instance(name: &str, overrides: Vec<Parameter>) -> (SystemInstance, SystemDefinition) {
        let params = vec![
            param("imu.update_rate_hz", 2.0),
            param("imu.seed", 2.0),
            param("imu.gyro_noise_sigma", 1e-4),
            param("imu.gyro_bias_rw_sigma", 1e-6),
            param("imu.accel_noise_sigma", 1e-3),
            param("imu.accel_bias_rw_sigma", 1e-5),
        ];
        let instance = SystemInstance {
            name: name.to_string(),
            system_id: "imu_sys".to_string(),
            binding: Some(Binding { kind: BindingKind::Model as i32, config: Some(av_cdm::pb::binding::Config::Model(ModelBinding { model_id: "imu_sys".to_string() })) }),
            parameter_overrides: overrides,
            ..Default::default()
        };
        let components = ["bias_gyro_x", "bias_gyro_y", "bias_gyro_z", "bias_accel_x", "bias_accel_y", "bias_accel_z"]
            .iter()
            .map(|l| av_cdm::pb::StateComponent { label: l.to_string(), unit: av_cdm::pb::Unit::RadianPerSecond as i32 })
            .collect();
        let space = StateSpace { id: "test.imu".to_string(), components, frame_id: String::new() };
        let sys = SystemDefinition {
            id: "imu_sys".to_string(),
            dynamics_model: "imu.test".to_string(),
            state_space_id: space.id.clone(),
            state_space: Some(space),
            parameters: params,
            ports: vec![Port { name: "imu_meas".to_string(), kind: PortKind::Framed as i32, direction: PortDirection::Out as i32, schema: "ccsds.spp".to_string(), ..Default::default() }],
            packet_codecs: vec![sensors::imu_packet_codec("imu_codec", 101)],
            ..Default::default()
        };
        (instance, sys)
    }

    /// A well-formed `"startracker."` instance classifies to `BindingPlan::StarTracker` -- fails
    /// against an implementation that never added the `crate::registry::ModelKind::StarTracker`
    /// dispatch arm at all (every `"startracker."` id would still fall through to
    /// `ModelKind::Native`, and `parse_constant_accel_spec` would refuse every
    /// `"startracker.*"` parameter name as unrecognized).
    #[test]
    fn a_well_formed_star_tracker_instance_classifies_to_binding_plan_star_tracker() {
        let (instance, sys) = star_tracker_instance("st1", vec![]);
        let plan = classify_binding(&instance, &sys, &DrmOptions::default()).expect("a valid star tracker spec classifies");
        match plan {
            Classification::Model(BindingPlan::StarTracker(spec)) => assert_eq!(spec.update_rate_hz, 2.0),
            other => panic!("expected Classification::Model(BindingPlan::StarTracker(_)), got {other:?}"),
        }
    }

    /// The IMU counterpart of the test above.
    #[test]
    fn a_well_formed_imu_instance_classifies_to_binding_plan_imu() {
        let (instance, sys) = imu_instance("imu1", vec![]);
        let plan = classify_binding(&instance, &sys, &DrmOptions::default()).expect("a valid imu spec classifies");
        match plan {
            Classification::Model(BindingPlan::Imu(spec)) => assert_eq!(spec.gyro_noise_sigma, 1e-4),
            other => panic!("expected Classification::Model(BindingPlan::Imu(_)), got {other:?}"),
        }
    }

    /// An unrecognized `"startracker.*"` parameter is a typed `DrmError::InvalidStarTrackerSpec`
    /// (wrapping `SensorSpecError::UnknownParameter`'s own `Display`), never silently ignored.
    /// Fails against an implementation missing the error-conversion arm entirely (a compile
    /// error) or one that swallows `parse_star_tracker_spec`'s own `Err`.
    #[test]
    fn an_unrecognized_star_tracker_parameter_is_a_typed_invalid_star_tracker_spec_error() {
        let (instance, sys) = star_tracker_instance("st1", vec![param("startracker.nonsense", 1.0)]);
        let err = classify_binding(&instance, &sys, &DrmOptions::default()).unwrap_err();
        assert!(matches!(err, DrmError::InvalidStarTrackerSpec { ref instance, ref reason } if instance == "st1" && reason.contains("startracker.nonsense")), "{err:?}");
    }

    /// The IMU counterpart of the test above (`DrmError::InvalidImuSpec`).
    #[test]
    fn an_unrecognized_imu_parameter_is_a_typed_invalid_imu_spec_error() {
        let (instance, sys) = imu_instance("imu1", vec![param("imu.nonsense", 1.0)]);
        let err = classify_binding(&instance, &sys, &DrmOptions::default()).unwrap_err();
        assert!(matches!(err, DrmError::InvalidImuSpec { ref instance, ref reason } if instance == "imu1" && reason.contains("imu.nonsense")), "{err:?}");
    }

    /// `resolve_sensor_output`'s own structural convention: zero (or more than one) declared
    /// `packet_codecs` entries is a typed `DrmError::SensorPortConfiguration`, never a panic on
    /// an out-of-bounds `sys.packet_codecs[0]` index. Fails against an implementation that
    /// indexes `packet_codecs[0]` unconditionally instead of checking `len() == 1` first.
    #[test]
    fn a_star_tracker_instance_declaring_no_packet_codec_is_a_typed_sensor_port_configuration_error() {
        let (instance, mut sys) = star_tracker_instance("st1", vec![]);
        sys.packet_codecs.clear();
        let err = classify_binding(&instance, &sys, &DrmOptions::default()).unwrap_err();
        assert!(matches!(err, DrmError::SensorPortConfiguration { ref instance, ref reason } if instance == "st1" && reason.contains('0')), "{err:?}");
    }

    /// The port-side counterpart: zero declared `PORT_KIND_FRAMED`/`PORT_DIRECTION_OUT` ports is
    /// the identical typed refusal, distinguishing "no output port" from "no codec" only by
    /// `reason`'s own text (both share `DrmError::SensorPortConfiguration`).
    #[test]
    fn an_imu_instance_declaring_no_framed_out_port_is_a_typed_sensor_port_configuration_error() {
        let (instance, mut sys) = imu_instance("imu1", vec![]);
        sys.ports.clear();
        let err = classify_binding(&instance, &sys, &DrmOptions::default()).unwrap_err();
        assert!(matches!(err, DrmError::SensorPortConfiguration { ref instance, .. } if instance == "imu1"), "{err:?}");
    }

    /// A declared codec missing a required field (`qw`) is refused at classify time, through the
    /// real `classify_binding` entry point, not only through `StarTrackerModel::new` directly --
    /// mirrors `classify_binding_refuses_an_attitude_instance_whose_wheel_momentum_component_is_
    /// labelled_with_the_torque_unit`'s own "reached through the real entry point" proof.
    #[test]
    fn classify_binding_refuses_a_star_tracker_instance_whose_codec_is_missing_a_required_field() {
        let (instance, mut sys) = star_tracker_instance("st1", vec![]);
        sys.packet_codecs[0].fields.retain(|f| f.name != "qw");
        let err = classify_binding(&instance, &sys, &DrmOptions::default()).unwrap_err();
        assert!(matches!(err, DrmError::InvalidStarTrackerSpec { ref instance, ref reason } if instance == "st1" && reason.contains("qw")), "{err:?}");
    }

    fn test_star_tracker_mat(spec: &StarTrackerSpec) -> Materialized {
        let codec = sensors::star_tracker_packet_codec("st_codec", 100);
        materialize_star_tracker(spec, codec, "st_meas".to_string(), 1_700_000_000_000_000_000, "startracker.test").expect("a valid spec/codec pair must materialize")
    }

    fn simple_star_tracker_spec() -> StarTrackerSpec {
        StarTrackerSpec { update_rate_hz: 2.0, seed: 1, noise_sigma_rad: 1e-5, mount_q: [0.0, 0.0, 0.0, 1.0], fault: None }
    }

    fn test_imu_mat(spec: &ImuSpec) -> Materialized {
        let codec = sensors::imu_packet_codec("imu_codec", 101);
        materialize_imu(spec, codec, "imu_meas".to_string(), 1_700_000_000_000_000_000, "imu.test").expect("a valid spec/codec pair must materialize")
    }

    fn simple_imu_spec() -> ImuSpec {
        ImuSpec { update_rate_hz: 2.0, seed: 2, gyro_noise_sigma: 1e-4, gyro_bias_rw_sigma: 1e-6, accel_noise_sigma: 1e-3, accel_bias_rw_sigma: 1e-5, mount_q: [0.0, 0.0, 0.0, 1.0], true_specific_force: [0.0, 0.0, 0.0], fault: None }
    }

    // -- Full per-method AnyModel::StarTracker delegation coverage (M14.3's own rule: "every
    // DynamicsModel method must be delegated explicitly... this defect class has recurred three
    // times") -- exactly the ten arms (nine methods, `step_with_stm` split by its `stm_capable`
    // guard) `AnyModel::ConstantAccel`/`AnyModel::Attitude` each carry in `binding.rs`, plus one
    // more in `crate::registry::ModelHandle::into_boxed` -- eleven real, non-comment
    // `AnyModel::StarTracker(` occurrences total, matching `AnyModel::ConstantAccel(`'s own
    // eleven exactly (see this task's own report for the grep count proving it). -------------

    #[test]
    fn any_model_state_dim_delegates_to_the_star_tracker_variant() {
        let mat = test_star_tracker_mat(&simple_star_tracker_spec());
        assert_eq!(mat.model.state_dim(), 0, "a star tracker has no propagated physical state");
    }

    #[test]
    fn any_model_derivatives_delegates_to_the_star_tracker_variant() {
        let mat = test_star_tracker_mat(&simple_star_tracker_spec());
        mat.model.derivatives(&[], mat.t0_tai_ns, &[], &mut []).unwrap();
    }

    #[test]
    fn any_model_describe_delegates_to_the_star_tracker_variant() {
        let mat = test_star_tracker_mat(&simple_star_tracker_spec());
        assert_eq!(mat.model.describe().id, "startracker.test");
    }

    #[test]
    fn any_model_integrator_delegates_to_the_star_tracker_variant() {
        // StarTrackerModel never overrides `integrator` either -- same "both sides are the
        // trait's own default, but through the real delegation path" reasoning as
        // `any_model_integrator_delegates_to_the_attitude_variant`.
        let mat = test_star_tracker_mat(&simple_star_tracker_spec());
        let _ = mat.model.integrator();
    }

    #[test]
    fn any_model_stm_capable_delegates_to_the_star_tracker_variant() {
        let mat = test_star_tracker_mat(&simple_star_tracker_spec());
        assert!(!mat.model.stm_capable(), "covariance for a star tracker is a typed refusal this batch -- stm_capable must be false");
    }

    #[test]
    fn any_model_stm_derivatives_returns_a_typed_capability_missing_error_for_the_star_tracker_variant() {
        let mat = test_star_tracker_mat(&simple_star_tracker_spec());
        let err = mat.model.stm_derivatives(&[], mat.t0_tai_ns, &[], &mut []).unwrap_err();
        assert!(matches!(err, AnyModelError::CapabilityMissing { ref capability, .. } if capability == "stm_derivatives"), "{err:?}");
    }

    #[test]
    fn any_model_step_with_stm_returns_a_typed_capability_missing_error_for_the_star_tracker_variant() {
        let mat = test_star_tracker_mat(&simple_star_tracker_spec());
        let err = mat.model.step_with_stm(&[], mat.t0_tai_ns, &[], 1_000_000_000).unwrap_err();
        assert!(matches!(err, AnyModelError::CapabilityMissing { ref capability, .. } if capability == "step_with_stm"), "{err:?}");
    }

    #[test]
    fn any_model_step_delegates_to_the_star_tracker_variant() {
        let mat = test_star_tracker_mat(&simple_star_tracker_spec());
        let step = mat.model.step(&[], mat.t0_tai_ns, &[], 1_000_000_000).unwrap();
        assert!(step.state.is_empty(), "a star tracker's state stays the empty vector");
        assert_eq!(step.t_tai_ns, mat.t0_tai_ns + 1_000_000_000);
    }

    #[test]
    fn any_model_step_with_ports_delegates_to_the_star_tracker_variant() {
        // Feeds a real truth inbox (mirrors `sensors::tests::feed_truth`) so a measurement is
        // actually produced -- proves `AnyModel` reaches `StarTrackerModel::step_with_ports`'s
        // own real logic (trap 1/3), not merely a shape that happens to compile.
        let mat = test_star_tracker_mat(&simple_star_tracker_spec());
        let mut outbox = av_dynamics::Outbox::new();
        for (i, port) in sensors::TRUTH_PORT_NAMES.iter().enumerate() {
            let v = if i == 3 { 1.0 } else { 0.0 }; // identity quaternion, zero rate
            outbox.push_signal(*port, mat.t0_tai_ns, v);
        }
        let inbox = Inbox::new(outbox.into_messages());
        let (result, outbox, applied) = mat.model.step_with_ports(&[], mat.t0_tai_ns, &[], 1_000_000_000, &inbox).unwrap();
        assert!(result.state.is_empty());
        // 2 Hz declared rate -> period_ns = 500 ms; a 1 s kernel step spans two full periods
        // (due at 0.5s and 1.0s, both <= end = 1.0s), so exactly 2 emissions -- stated before
        // measuring, per this task's own "state the exact expected count" rule.
        assert_eq!(outbox.messages().len(), 2, "2 Hz declared rate over a 1s kernel step = 2 emissions (due at 0.5s and 1.0s)");
        assert!(applied.is_empty());
    }

    /// M25.3c defect D2: `AnyModel::last_measurements` must reach `StarTrackerModel`'s own real
    /// cache, not silently return an empty `Vec` -- the `last_measurements` counterpart of
    /// [`any_model_step_with_ports_delegates_to_the_star_tracker_variant`] immediately above.
    /// Feeds the identical real truth inbox that test does, so a genuine, non-fixture measurement
    /// is actually cached, then reads it back through `AnyModel` (not `StarTrackerModel`
    /// directly). **Fails against an `AnyModel::last_measurements` match arm that returns
    /// `Vec::new()` for `StarTracker` instead of `m.last_measurements()`** (broken-and-restored:
    /// see this task's own report).
    #[test]
    fn any_model_last_measurements_delegates_to_the_star_tracker_variant() {
        let mat = test_star_tracker_mat(&simple_star_tracker_spec());
        let mut outbox = av_dynamics::Outbox::new();
        for (i, port) in sensors::TRUTH_PORT_NAMES.iter().enumerate() {
            let v = if i == 3 { 1.0 } else { 0.0 }; // identity quaternion, zero rate
            outbox.push_signal(*port, mat.t0_tai_ns, v);
        }
        let inbox = Inbox::new(outbox.into_messages());
        let _ = mat.model.step_with_ports(&[], mat.t0_tai_ns, &[], 1_000_000_000, &inbox).unwrap();
        let measurements = mat.model.last_measurements();
        assert_eq!(measurements.len(), 2, "2 Hz declared rate over a 1s kernel step = 2 emissions, same as the outbox count above");
        for m in &measurements {
            assert_eq!(m.measurement_id, "altavista.attitude_q4");
        }
    }

    // -- Full per-method AnyModel::Imu delegation coverage -- the IMU counterpart of the block
    // immediately above. -----------------------------------------------------------------------

    #[test]
    fn any_model_state_dim_delegates_to_the_imu_variant() {
        let mat = test_imu_mat(&simple_imu_spec());
        assert_eq!(mat.model.state_dim(), 6, "the bias random walk is a genuinely propagated 6-component state");
    }

    #[test]
    fn any_model_derivatives_delegates_to_the_imu_variant() {
        let mat = test_imu_mat(&simple_imu_spec());
        let mut out = [1.0; 6]; // non-zero sentinel
        mat.model.derivatives(&mat.x0_si, mat.t0_tai_ns, &[], &mut out).unwrap();
        assert_eq!(out, [0.0; 6], "ImuModel::derivatives is an honest zero -- the bias walk is a discrete process, not an ODE");
    }

    #[test]
    fn any_model_describe_delegates_to_the_imu_variant() {
        let mat = test_imu_mat(&simple_imu_spec());
        assert_eq!(mat.model.describe().id, "imu.test");
    }

    #[test]
    fn any_model_integrator_delegates_to_the_imu_variant() {
        let mat = test_imu_mat(&simple_imu_spec());
        let _ = mat.model.integrator();
    }

    #[test]
    fn any_model_stm_capable_delegates_to_the_imu_variant() {
        let mat = test_imu_mat(&simple_imu_spec());
        assert!(!mat.model.stm_capable(), "covariance for an IMU is a typed refusal this batch -- stm_capable must be false");
    }

    #[test]
    fn any_model_stm_derivatives_returns_a_typed_capability_missing_error_for_the_imu_variant() {
        let mat = test_imu_mat(&simple_imu_spec());
        let err = mat.model.stm_derivatives(&mat.x0_si, mat.t0_tai_ns, &[], &mut [0.0; 6]).unwrap_err();
        assert!(matches!(err, AnyModelError::CapabilityMissing { ref capability, .. } if capability == "stm_derivatives"), "{err:?}");
    }

    #[test]
    fn any_model_step_with_stm_returns_a_typed_capability_missing_error_for_the_imu_variant() {
        let mat = test_imu_mat(&simple_imu_spec());
        let err = mat.model.step_with_stm(&mat.x0_si, mat.t0_tai_ns, &[], 1_000_000_000).unwrap_err();
        assert!(matches!(err, AnyModelError::CapabilityMissing { ref capability, .. } if capability == "step_with_stm"), "{err:?}");
    }

    #[test]
    fn any_model_step_delegates_to_the_imu_variant() {
        let mat = test_imu_mat(&simple_imu_spec());
        let step = mat.model.step(&mat.x0_si, mat.t0_tai_ns, &[], 1_000_000_000).unwrap();
        assert_eq!(step.state.len(), 6);
        // The bias random walk advances regardless of whether truth has arrived (the model's own
        // doc comment) -- with no truth ever fed here, this reaches AnyModel::step ->
        // ImuModel::step -> ImuModel::step_with_ports(Inbox::empty()), which must still update
        // the bias twice (two elapsed 0.5s periods within this 1s step). A near-zero result would
        // mean either the random walk never ran (dead delegation) or it ran with a broken RNG.
        assert_ne!(step.state, mat.x0_si, "the bias random walk must have advanced");
    }

    #[test]
    fn any_model_step_with_ports_delegates_to_the_imu_variant() {
        let mat = test_imu_mat(&simple_imu_spec());
        let mut truth_outbox = av_dynamics::Outbox::new();
        for (i, port) in sensors::TRUTH_PORT_NAMES.iter().enumerate() {
            let v = if i == 3 { 1.0 } else { 0.0 };
            truth_outbox.push_signal(*port, mat.t0_tai_ns, v);
        }
        let inbox = Inbox::new(truth_outbox.into_messages());
        let (result, outbox, applied) = mat.model.step_with_ports(&mat.x0_si, mat.t0_tai_ns, &[], 1_000_000_000, &inbox).unwrap();
        assert_eq!(result.state.len(), 6);
        assert_eq!(outbox.messages().len(), 2, "2 Hz declared rate over a 1s kernel step = 2 emissions (due at 0.5s and 1.0s)");
        assert!(applied.is_empty());
    }

    // =========================================================================================
    // M22.4: classify_binding dispatch, AnyModel::Controller delegation (mirrors the StarTracker/
    // Imu blocks immediately above).
    // =========================================================================================

    /// A well-formed `"attctrl."` instance -- declares all three required `packet_codecs`
    /// (structurally identified, see `resolve_controller_ports`'s own doc comment) and all three
    /// required ports by `controller`'s own fixed name convention.
    fn controller_instance(name: &str, overrides: Vec<Parameter>) -> (SystemInstance, SystemDefinition) {
        let params = vec![param("controller.kp", 0.5), param("controller.kd", 5.0), param("controller.target_q.x", 0.0), param("controller.target_q.y", 0.0), param("controller.target_q.z", 0.0), param("controller.target_q.w", 1.0), param("controller.update_rate_hz", 2.0)];
        let instance = SystemInstance {
            name: name.to_string(),
            system_id: "controller_sys".to_string(),
            binding: Some(Binding { kind: BindingKind::Model as i32, config: Some(av_cdm::pb::binding::Config::Model(ModelBinding { model_id: "controller_sys".to_string() })) }),
            parameter_overrides: overrides,
            ..Default::default()
        };
        let sys = SystemDefinition {
            id: "controller_sys".to_string(),
            dynamics_model: "attctrl.test".to_string(),
            parameters: params,
            ports: vec![
                Port { name: controller::CONTROLLER_STARTRACKER_IN_PORT.to_string(), kind: PortKind::Framed as i32, direction: PortDirection::In as i32, schema: "ccsds.spp".to_string(), ..Default::default() },
                Port { name: controller::CONTROLLER_IMU_IN_PORT.to_string(), kind: PortKind::Framed as i32, direction: PortDirection::In as i32, schema: "ccsds.spp".to_string(), ..Default::default() },
                Port { name: controller::CONTROLLER_WHEEL_TORQUE_OUT_PORT.to_string(), kind: PortKind::Framed as i32, direction: PortDirection::Out as i32, schema: "ccsds.spp".to_string(), ..Default::default() },
            ],
            packet_codecs: vec![sensors::star_tracker_packet_codec("ctrl_star_codec", 100), sensors::imu_packet_codec("ctrl_imu_codec", 101), controller::wheel_torque_command_packet_codec("ctrl_cmd_codec", 102)],
            ..Default::default()
        };
        (instance, sys)
    }

    /// A well-formed `"attctrl."` instance classifies to `BindingPlan::Controller` -- fails
    /// against an implementation that never added the `crate::registry::ModelKind::
    /// AttitudeController` dispatch arm at all.
    #[test]
    fn a_well_formed_controller_instance_classifies_to_binding_plan_controller() {
        let (instance, sys) = controller_instance("ctrl1", vec![]);
        let plan = classify_binding(&instance, &sys, &DrmOptions::default()).expect("a valid controller spec classifies");
        match plan {
            Classification::Model(BindingPlan::Controller(spec)) => {
                assert_eq!(spec.kp, 0.5);
                assert_eq!(spec.kd, 5.0);
            }
            other => panic!("expected Classification::Model(BindingPlan::Controller(_)), got {other:?}"),
        }
    }

    /// An unrecognized `"controller.*"` parameter is a typed `DrmError::InvalidControllerSpec`,
    /// never silently ignored.
    #[test]
    fn an_unrecognized_controller_parameter_is_a_typed_load_error() {
        let (instance, mut sys) = controller_instance("ctrl1", vec![]);
        sys.parameters.push(param("controller.nonsense", 1.0));
        let err = classify_binding(&instance, &sys, &DrmOptions::default()).unwrap_err();
        assert!(matches!(err, DrmError::InvalidControllerSpec { ref instance, ref reason } if instance == "ctrl1" && reason.contains("controller.nonsense")), "{err:?}");
    }

    /// A controller instance declaring the wrong number of `packet_codecs` entries is a typed
    /// `DrmError::SensorPortConfiguration`, never a panic on an out-of-bounds index.
    #[test]
    fn a_controller_instance_declaring_the_wrong_number_of_packet_codecs_is_a_typed_error() {
        let (instance, mut sys) = controller_instance("ctrl1", vec![]);
        sys.packet_codecs.pop();
        let err = classify_binding(&instance, &sys, &DrmOptions::default()).unwrap_err();
        assert!(matches!(err, DrmError::SensorPortConfiguration { ref instance, .. } if instance == "ctrl1"), "{err:?}");
    }

    fn test_controller_mat(spec: &AttitudeControllerSpec) -> Materialized {
        let star_codec = sensors::star_tracker_packet_codec("ctrl_star_codec", 100);
        let imu_codec = sensors::imu_packet_codec("ctrl_imu_codec", 101);
        let cmd_codec = controller::wheel_torque_command_packet_codec("ctrl_cmd_codec", 102);
        materialize_controller(spec, star_codec, imu_codec, cmd_codec, 1_700_000_000_000_000_000, "attctrl.test").expect("a valid spec/codec triple must materialize")
    }

    fn simple_controller_spec() -> AttitudeControllerSpec {
        AttitudeControllerSpec { kp: 0.5, kd: 5.0, target_q: [0.0, 0.0, 0.0, 1.0], update_rate_hz: 2.0 }
    }

    // -- Full per-method AnyModel::Controller delegation coverage -- mirrors the StarTracker/Imu
    // blocks above exactly (the same ten `binding.rs` arms plus one in `registry.rs::into_boxed`
    // -- verified by the arm-count grep this task's own report states). -----------------------

    #[test]
    fn any_model_state_dim_delegates_to_the_controller_variant() {
        let mat = test_controller_mat(&simple_controller_spec());
        assert_eq!(mat.model.state_dim(), 0, "a control law with no integrator state has no propagated physical state");
    }

    #[test]
    fn any_model_derivatives_delegates_to_the_controller_variant() {
        let mat = test_controller_mat(&simple_controller_spec());
        mat.model.derivatives(&[], mat.t0_tai_ns, &[], &mut []).unwrap();
    }

    #[test]
    fn any_model_describe_delegates_to_the_controller_variant() {
        let mat = test_controller_mat(&simple_controller_spec());
        assert_eq!(mat.model.describe().id, "attctrl.test");
    }

    #[test]
    fn any_model_integrator_delegates_to_the_controller_variant() {
        let mat = test_controller_mat(&simple_controller_spec());
        let _ = mat.model.integrator();
    }

    #[test]
    fn any_model_stm_capable_delegates_to_the_controller_variant() {
        let mat = test_controller_mat(&simple_controller_spec());
        assert!(!mat.model.stm_capable(), "covariance for the controller is a typed refusal this batch -- stm_capable must be false");
    }

    #[test]
    fn any_model_stm_derivatives_returns_a_typed_capability_missing_error_for_the_controller_variant() {
        let mat = test_controller_mat(&simple_controller_spec());
        let err = mat.model.stm_derivatives(&[], mat.t0_tai_ns, &[], &mut []).unwrap_err();
        assert!(matches!(err, AnyModelError::CapabilityMissing { ref capability, .. } if capability == "stm_derivatives"), "{err:?}");
    }

    #[test]
    fn any_model_step_with_stm_returns_a_typed_capability_missing_error_for_the_controller_variant() {
        let mat = test_controller_mat(&simple_controller_spec());
        let err = mat.model.step_with_stm(&[], mat.t0_tai_ns, &[], 1_000_000_000).unwrap_err();
        assert!(matches!(err, AnyModelError::CapabilityMissing { ref capability, .. } if capability == "step_with_stm"), "{err:?}");
    }

    #[test]
    fn any_model_step_delegates_to_the_controller_variant() {
        let mat = test_controller_mat(&simple_controller_spec());
        let step = mat.model.step(&[], mat.t0_tai_ns, &[], 1_000_000_000).unwrap();
        assert!(step.state.is_empty());
        assert_eq!(step.t_tai_ns, mat.t0_tai_ns + 1_000_000_000);
    }

    /// Feeds real star tracker/IMU packets on the controller's own declared IN ports -- proves
    /// `AnyModel` reaches `AttitudeControllerModel::step_with_ports`'s own real decode/control-
    /// law/encode logic, not merely a shape that happens to compile.
    #[test]
    fn any_model_step_with_ports_delegates_to_the_controller_variant() {
        let mat = test_controller_mat(&simple_controller_spec());
        let star_codec = sensors::star_tracker_packet_codec("ctrl_star_codec", 100);
        let imu_codec = sensors::imu_packet_codec("ctrl_imu_codec", 101);
        let mut star_values = std::collections::BTreeMap::new();
        star_values.insert("qx".to_string(), crate::codec::FieldValue::Numeric(0.0));
        star_values.insert("qy".to_string(), crate::codec::FieldValue::Numeric(0.0));
        star_values.insert("qz".to_string(), crate::codec::FieldValue::Numeric(0.0));
        star_values.insert("qw".to_string(), crate::codec::FieldValue::Numeric(1.0));
        let mut imu_values = std::collections::BTreeMap::new();
        for f in ["wx", "wy", "wz", "ax", "ay", "az"] {
            imu_values.insert(f.to_string(), crate::codec::FieldValue::Numeric(0.0));
        }
        let inbox = Inbox::new(vec![
            av_dynamics::PortMessage { port: controller::CONTROLLER_STARTRACKER_IN_PORT.to_string(), tai_ns: mat.t0_tai_ns, payload: crate::codec::encode_packet(&star_codec, 0, &[], &star_values).unwrap() },
            av_dynamics::PortMessage { port: controller::CONTROLLER_IMU_IN_PORT.to_string(), tai_ns: mat.t0_tai_ns, payload: crate::codec::encode_packet(&imu_codec, 0, &[], &imu_values).unwrap() },
        ]);
        let (result, outbox, applied) = mat.model.step_with_ports(&[], mat.t0_tai_ns, &[], 1_000_000_000, &inbox).unwrap();
        assert!(result.state.is_empty());
        // 2 Hz declared rate over a 1s step = 2 emissions (due at 0.5s and 1.0s), same derivation
        // as the star tracker/IMU blocks above.
        assert_eq!(outbox.messages().len(), 2, "2 Hz declared rate over a 1s kernel step = 2 emissions (due at 0.5s and 1.0s)");
        assert_eq!(applied.len(), 2);
        assert!(result.outputs.contains_key("pointing_error_rad"));
    }

    fn test_ground_station_mat(spec: &GroundStationSpec) -> Materialized {
        let tm_codec = ground::ground_tm_packet_codec("gs_tm_codec", 400);
        let tc_codec = ground::ground_tc_packet_codec("gs_tc_codec", 401);
        materialize_ground_station(spec, tm_codec, "tm_in".to_string(), tc_codec, "tc_out".to_string(), 1_700_000_000_000_000_000, "ground.test").expect("a valid spec/codec pair must materialize")
    }

    fn simple_ground_station_spec() -> GroundStationSpec {
        GroundStationSpec { body: "Earth".to_string(), latitude_rad: 28.5_f64.to_radians(), longitude_rad: (-80.6_f64).to_radians(), height_m: 0.0, elevation_mask_rad: 10.0_f64.to_radians() }
    }

    // -- Full per-method AnyModel::GroundStation delegation coverage (M25.1) -- mirrors the
    // StarTracker/Imu/Controller blocks above exactly: the same nine methods (`state_dim`,
    // `derivatives`, `describe`, `integrator`, `stm_capable`, `stm_derivatives`, `step_with_stm`,
    // `step`, `step_with_ports`), each with its own deliberate `AnyModel::GroundStation(` arm --
    // [`any_model_arm_count_for_ground_station_matches_star_tracker`] below turns the "verified by
    // the arm-count grep this task's own report states" comment those other blocks carry into a
    // real, executable check rather than a claim a reviewer must re-run by hand. -----------------

    #[test]
    fn any_model_state_dim_delegates_to_the_ground_station_variant() {
        let mat = test_ground_station_mat(&simple_ground_station_spec());
        assert_eq!(mat.model.state_dim(), 0, "a fixed geodetic site has no propagated physical state");
    }

    #[test]
    fn any_model_derivatives_delegates_to_the_ground_station_variant() {
        let mat = test_ground_station_mat(&simple_ground_station_spec());
        mat.model.derivatives(&[], mat.t0_tai_ns, &[], &mut []).unwrap();
    }

    #[test]
    fn any_model_describe_delegates_to_the_ground_station_variant() {
        let mat = test_ground_station_mat(&simple_ground_station_spec());
        assert_eq!(mat.model.describe().id, "ground.test");
    }

    #[test]
    fn any_model_integrator_delegates_to_the_ground_station_variant() {
        let mat = test_ground_station_mat(&simple_ground_station_spec());
        let _ = mat.model.integrator();
    }

    #[test]
    fn any_model_stm_capable_delegates_to_the_ground_station_variant() {
        let mat = test_ground_station_mat(&simple_ground_station_spec());
        assert!(!mat.model.stm_capable(), "covariance for a ground station is a typed refusal this batch -- stm_capable must be false");
    }

    #[test]
    fn any_model_stm_derivatives_returns_a_typed_capability_missing_error_for_the_ground_station_variant() {
        let mat = test_ground_station_mat(&simple_ground_station_spec());
        let err = mat.model.stm_derivatives(&[], mat.t0_tai_ns, &[], &mut []).unwrap_err();
        assert!(matches!(err, AnyModelError::CapabilityMissing { ref capability, .. } if capability == "stm_derivatives"), "{err:?}");
    }

    #[test]
    fn any_model_step_with_stm_returns_a_typed_capability_missing_error_for_the_ground_station_variant() {
        let mat = test_ground_station_mat(&simple_ground_station_spec());
        let err = mat.model.step_with_stm(&[], mat.t0_tai_ns, &[], 1_000_000_000).unwrap_err();
        assert!(matches!(err, AnyModelError::CapabilityMissing { ref capability, .. } if capability == "step_with_stm"), "{err:?}");
    }

    #[test]
    fn any_model_step_delegates_to_the_ground_station_variant() {
        let mat = test_ground_station_mat(&simple_ground_station_spec());
        let step = mat.model.step(&[], mat.t0_tai_ns, &[], 1_000_000_000).unwrap();
        assert!(step.state.is_empty(), "a ground station's state stays the empty vector");
        assert_eq!(step.t_tai_ns, mat.t0_tai_ns + 1_000_000_000);
    }

    /// Feeds a real telemetry packet (a target directly overhead the declared site) on the
    /// ground station's own declared `tm_in` port -- proves `AnyModel` reaches
    /// `GroundStationModel::step_with_ports`'s own real decode/elevation/contact logic, not
    /// merely a shape that happens to compile: a rising edge produces one applied contact-start
    /// command and one encoded ack packet on `tc_out`, mirroring `any_model_step_with_ports_
    /// delegates_to_the_controller_variant`'s own "feed real port traffic, check the real
    /// output" proof.
    #[test]
    fn any_model_step_with_ports_delegates_to_the_ground_station_variant() {
        let spec = simple_ground_station_spec();
        let mat = test_ground_station_mat(&spec);
        let site_ecef = ground::geodetic_to_ecef_m(spec.latitude_rad, spec.longitude_rad, spec.height_m);
        let r = (site_ecef[0].powi(2) + site_ecef[1].powi(2) + site_ecef[2].powi(2)).sqrt();
        let up = [site_ecef[0] / r, site_ecef[1] / r, site_ecef[2] / r];
        let overhead = [site_ecef[0] + 500_000.0 * up[0], site_ecef[1] + 500_000.0 * up[1], site_ecef[2] + 500_000.0 * up[2]];
        let tm_codec = ground::ground_tm_packet_codec("gs_tm_codec", 400);
        let mut values = std::collections::BTreeMap::new();
        values.insert("x".to_string(), crate::codec::FieldValue::Numeric(overhead[0]));
        values.insert("y".to_string(), crate::codec::FieldValue::Numeric(overhead[1]));
        values.insert("z".to_string(), crate::codec::FieldValue::Numeric(overhead[2]));
        let inbox = Inbox::new(vec![av_dynamics::PortMessage { port: "tm_in".to_string(), tai_ns: mat.t0_tai_ns, payload: crate::codec::encode_packet(&tm_codec, 0, &[], &values).unwrap() }]);
        let (result, outbox, applied) = mat.model.step_with_ports(&[], mat.t0_tai_ns, &[], 1_000_000_000, &inbox).unwrap();
        assert!(result.state.is_empty());
        assert_eq!(outbox.messages().len(), 1, "a rising edge sends exactly one AOS-acknowledgment packet on tc_out");
        assert_eq!(outbox.messages()[0].port, "tc_out");
        assert_eq!(applied.len(), 1);
        assert_eq!(applied[0].value, ground::CONTACT_START_VALUE);
    }

    /// The lead's standing rule for this task: "the arm-count symmetry check passes against an
    /// existing variant." Every other new-variant block in this test module states its own arm
    /// count only in a comment, checked by a human/agent `grep` at review time (agent report,
    /// never re-run by CI); this test makes the identical check real and executable, by reading
    /// this very source file (`include_str!`) and counting literal `AnyModel::<Variant>(`
    /// occurrences -- `AnyModel::StarTracker(` (an already-landed, reference variant carrying the
    /// same ten-method, zero-state shape [`AnyModel::GroundStation`] does -- R5.1a, question 178,
    /// raised this from nine to ten by adding `drain_sensor_fault_effect`) against
    /// `AnyModel::GroundStation(` itself. Fails against a future edit that adds a `GroundStation`
    /// call site without its matching `StarTracker` counterpart (or vice versa) drifting the two
    /// out of lockstep -- exactly the class of silent, partial wiring this task's own standing
    /// scope rule exists to prevent.
    #[test]
    fn any_model_arm_count_for_ground_station_matches_star_tracker() {
        let source = include_str!("binding.rs");
        let count = |needle: &str| source.matches(needle).count();
        let star_tracker_arms = count("AnyModel::StarTracker(");
        let ground_station_arms = count("AnyModel::GroundStation(");
        assert_eq!(ground_station_arms, star_tracker_arms, "AnyModel::GroundStation( must appear exactly as many times in this file as AnyModel::StarTracker( -- one deliberate arm per DynamicsModel method (plus this test's own two occurrences of the needle string, which cancel identically on both sides)");
        // Sanity floor: catches a needle typo (e.g. renaming the variant) silently passing an
        // `0 == 0` comparison.
        assert!(star_tracker_arms >= 10, "expected at least the nine AnyModel trait-method arms plus this test file's own occurrences; got {star_tracker_arms}");
    }

    /// M25.4b's own instance of the identical executable check above, updated for the
    /// `AnyModel::Replay` variant this task adds (this task's own standing rule: "the
    /// executable arm-count check updated if you add an `AnyModel` variant"). Not a direct
    /// text-count-equality-with-`GroundStation` check like the sibling test above: `Replay` is
    /// CONSTRUCTED in a different file than every other variant (`crate::registry::
    /// ModelRegistry::wrap_replay`, not a `materialize_*` function in this one -- see
    /// `AnyModel::Replay`'s own doc comment: dispatch into it is a `RunConfig.replay` decision,
    /// not anything `classify_binding` in this file ever makes), so this file's own `AnyModel::
    /// Replay(` text count is not directly comparable to the `GroundStation` variant's own.
    /// Instead: a leading-indentation-anchored needle (`"\n            AnyModel::Replay("`, twelve spaces
    /// -- the exact indentation every real `match self { ... }` arm in this file's own `impl
    /// DynamicsModel for AnyModel` block uses) counts ONLY real match arms, never a comment or
    /// this test's own strings (neither is indented that way), so the expected count -- twelve,
    /// one per `DynamicsModel` method, `stm_derivatives`/`step_with_stm` each split into a guard
    /// arm plus a dead-but-typechecking delegate arm mirroring `Attitude`/`Controller` -- can be
    /// asserted directly rather than by comparison. **R5.1a (question 178) raised this from
    /// eleven to twelve**: `DynamicsModel` gained a new required method, `drain_sensor_fault_
    /// effect`, delegated here with one more plain arm (no guard split, mirroring `last_
    /// measurements`). **R5.2 (question 188) raises this from twelve to thirteen**:
    /// `DynamicsModel` gained a second new required method, `drain_decode_errors`, delegated
    /// here with one more plain arm, identical shape to `drain_sensor_fault_effect`'s own
    /// addition. Fails against a future edit that removes an arm (the count drops below 13) or
    /// duplicates one (the sibling `registry.rs` check below would also need `wrap_
    /// replay`'s own construction site to still exist).
    #[test]
    fn any_model_arm_count_for_replay_covers_every_dynamics_model_match_site() {
        let binding_source = include_str!("binding.rs");
        let indented_arm_count = binding_source.matches("\n            AnyModel::Replay(").count();
        assert_eq!(indented_arm_count, 13, "AnyModel::Replay( must appear as a real match arm (12-space indent) exactly 13 times: one per DynamicsModel method (R5.1a added drain_sensor_fault_effect, R5.2 added drain_decode_errors), with stm_derivatives/step_with_stm each split into a guard + delegate pair");

        // The one construction site this variant has, which -- unlike every other variant's own
        // `materialize_*` function -- lives in `crate::registry::ModelRegistry::wrap_replay`,
        // not in this file (see this test's own doc comment).
        let registry_source = include_str!("../registry.rs");
        assert!(registry_source.contains("AnyModel::Replay("), "crate::registry::ModelRegistry::wrap_replay must construct AnyModel::Replay(...) directly -- this is Replay's one construction call site, the counterpart of every other variant's own materialize_* function in THIS file");
    }
}
