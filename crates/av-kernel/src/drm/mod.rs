//! The DRM executor (M6.1, `docs/open-questions.md` question 87): load a
//! `DesignReferenceMission` + `SosConfiguration` + `SystemDefinition`s, verify their canonical
//! hashes, bind every `BINDING_KIND_MODEL` instance to a real [`av_dynamics::DynamicsModel`],
//! honour `DrmOptions` (`covariance`, `default_step_rate_hz`/per-instance `step_rate_hz`,
//! `sample_interval_s`, `nearest_spd_projection`, `accept_missing_stm_terms`, `real_time`),
//! inject `Scenario` faults of kind `FAULT_TARGET_KIND_DYNAMICS`, and emit CDM `Trajectory`s
//! whose `Provenance` carries the DRM hash, the configuration hash, the data-pack hash and a
//! `run_id`.
//!
//! ## This is the one place in `av-kernel` that depends on `gmat-sys` outside `tests/`
//!
//! `crate`'s own module doc (`src/lib.rs`) states the M2.1-era invariant "no GMAT dependency
//! in this crate's own library code" -- true for `clock`/`interpolate`/`schedule`/`kernel`/
//! `trajectory`, and still true for them after this module was added. It is **not** true for
//! `drm`: binding a `BINDING_KIND_MODEL` space-system instance to a real
//! `gmat_sys::model::GmatModel` is exactly what the task asked this module to do, so
//! `gmat-sys` moved from `[dev-dependencies]` to `[dependencies]` in this crate's
//! `Cargo.toml` for `drm`'s sake alone. Every function in `binding::classify_binding` (and
//! everything `schema`/`hash` do) still runs with **no** GMAT install and **no** `Gmat`
//! handle -- only `binding::materialize_gmat` (called from `crate::registry::ModelRegistry::
//! construct_gmat`, M10.3 -- the sole caller, since `execute` itself only ever reaches GMAT
//! through the registry now, question 98) ever touches the engine, and every caller of
//! `execute`/`ModelRegistry::construct_gmat` must take `gmat_sys::engine_lock()` first, per
//! this repository's existing convention (`tests/golden_acceptance.rs`).
//!
//! ## Module layout
//!
//! - [`schema`] -- the YAML authoring format (a field-for-field mirror of
//!   `proto/altavista/v1/system.proto`) and its conversion into real `av_cdm::pb` types.
//! - [`hash`] -- the canonical SHA-256 hash (question 87's "compute and verify").
//! - [`binding`] -- classifying and materializing a `SystemInstance`'s binding into a
//!   `binding::AnyModel` (`GmatModel` or the native `ConstantAccelModel` placeholder) --
//!   `pub(crate)` as of M10.3 (question 98), reachable only through `crate::registry::
//!   ModelRegistry`'s opaque `ModelHandle`; see that module's own doc comment.
//! - [`attitude`] -- M22.1 (`docs/sil-plan.md`'s M22 milestone, decision A;
//!   `docs/open-questions.md` questions 88/142): [`attitude::AttitudeWheelsModel`], a native
//!   rigid-body attitude `av_dynamics::DynamicsModel` with reaction wheels, propagated
//!   alongside (not through) `binding::ConstantAccelModel` -- see that module's own doc
//!   comment for exactly what this batch does and does not wire into `binding`/`registry`.
//! - [`sensors`] -- M22.2 (`docs/sil-plan.md`'s M22 milestone, decision A;
//!   `docs/open-questions.md` questions 142/149/151/152): [`sensors::StarTrackerModel`]/
//!   [`sensors::ImuModel`], native sensor `av_dynamics::DynamicsModel`s feeding FRAMED CCSDS
//!   ports -- see that module's own doc comment for exactly what this batch does and does not
//!   wire into `binding`/`registry` (mirrors [`attitude`]'s own scope note).
//! - [`ground`] -- M25.1 (`docs/sil-plan.md`'s M25 milestone; `docs/open-questions.md` questions
//!   10/108/149): [`ground::GroundStationModel`], a native ground-segment `av_dynamics::
//!   DynamicsModel` -- visibility from a declared geodetic site (through the topocentric
//!   ENU/NED axes `docs/open-questions.md` question 10 already mandates, `core.proto`'s
//!   `Geodetic`/`FrameDefinition.origin_geodetic`), a contact-window/elevation-mask model, and
//!   FRAMED CCSDS telecommand/telemetry ports -- see that module's own doc comment.
//! - [`fault`] -- splitting a run into fault-bounded segments for `FAULT_TARGET_KIND_DYNAMICS`
//!   faults of kind `"parameter"`.
//! - [`maneuver`] -- typed `Scenario.events` of kind `"maneuver"` and the RIC/VNB/VVLH/inertial
//!   delta-v frame transform (`docs/open-questions.md` question 97).
//! - [`events`] -- the CDM `Event`s [`executor::execute`] emits (`docs/open-questions.md`
//!   question 95).
//! - [`executor`] -- [`executor::execute`], the end-to-end entry point. `executor::RunProducts::
//!   to_proto` (question 121, M17.2) converts a run's products into the real
//!   `altavista.v1.RunProducts` CDM message -- see that method's own doc comment and
//!   `crates/av-kernel/README.md`'s "RunProducts on the wire" section.

pub mod attitude;
pub mod binding;
pub mod command;
pub mod controller;
pub mod events;
pub mod executor;
pub mod fault;
pub mod gmat_command;
pub mod ground;
pub mod hash;
pub mod maneuver;
pub mod replay;
pub mod schema;
pub mod sensors;

pub use executor::{execute, RunConfig, RunProducts, Score};
pub use maneuver::ExecutionErrorMode;
pub use replay::ReplayConfig;

/// Every way a DRM run can be refused or fail, typed rather than a caller ever discovering a
/// silently-dropped option or a silently-skipped refusal (question 87's central complaint).
#[derive(Debug)]
pub enum DrmError {
    /// The YAML did not parse as the expected shape at all (`serde_yaml`'s own message).
    Yaml(String),
    /// A string field that names a proto enum value did not match any of that enum's
    /// `from_str_name` variants.
    InvalidEnumValue { field: &'static str, value: String },
    /// A field this loader does not yet model (see `schema`'s module doc comment) was
    /// non-empty -- refused rather than silently dropped.
    UnsupportedField { field: &'static str },
    /// A `Binding.config` oneof had more than one of `model`/`container`/`renode`/`board` set.
    InvalidBinding { reason: String },
    /// A declared `hash` field did not match the artifact's own canonical hash
    /// (`hash::canonical_drm_hash`/`canonical_sos_hash`/`canonical_system_hash`) -- the run
    /// is refused, never a warning.
    HashMismatch { artifact: &'static str, id: String, declared: String, computed: String },
    /// `SystemInstance.binding` was unset, or its `kind` was anything other than
    /// `BINDING_KIND_MODEL` (`container`/`renode`/`board`/`_UNSPECIFIED`) -- ADR-005's
    /// runtime for those is still Planned; this crate has no machinery for any of them.
    UnsupportedBinding { instance: String, kind: String },
    /// `SystemInstance.system_id` did not name any `SystemDefinition` the caller supplied.
    UnknownSystemDefinition { instance: String, system_id: String },
    /// `SystemDefinition.state_space_id` did not name a `StateSpace` this kernel declares
    /// (`trajectory::state_space_for`). ADR-001: nothing exists only by convention, so an
    /// undeclared state space is refused at load rather than propagated as a bare id string.
    UnknownStateSpace { instance: String, state_space_id: String },
    /// `SystemDefinition.state_space` was declared but invalid: its `id` did not equal
    /// `state_space_id`, or a component could not be classified under ADR-005 section 3.
    /// Question 94: a declared state space is authoritative, so a bad one is refused at load.
    InvalidStateSpace { instance: String, reason: String },
    /// M20.1 (question 133), guard inverted by M21.3 (question 141, decided by the lead): the
    /// effective, resolved `StateSpace`'s own component count (`declared_dim`) did not match
    /// `model_state_dim` -- for a `"native."`-dispatched instance (the only binding kind this
    /// check covers today), the width its own `"state.*"` parameters actually configure
    /// (`binding::ConstantAccelSpec::x0_si`'s length: 0 or `binding::CONSTANT_ACCEL_STATE_DIM`,
    /// never anything else -- see that constant's own doc comment). Through M20.1 this compared
    /// the declared count against one fixed constant; M21.3 makes the declared state space
    /// authoritative and refuses whenever the instance's own configuration cannot honour it --
    /// still the same guard that stops a fixture lying about its state space (a non-physical
    /// native controller declaring the 6-component Cartesian position/velocity shape, M20.1's
    /// own found defect) or declaring a width `"native."` can never produce at all: neither
    /// would trip `InvalidStateSpace` above on its own (a well-formed, classifiable `StateSpace`
    /// is valid on its own terms), only comparing the two catches it.
    StateSpaceDimensionMismatch { instance: String, declared_dim: usize, model_state_dim: usize },
    /// `DesignReferenceMission.sos_configuration_id` did not match the supplied
    /// `SosConfiguration.id`.
    UnknownSosConfiguration { declared: String, provided: String },
    /// A parameter this binding kind requires was absent from the effective parameter set
    /// (`SystemDefinition.parameters` plus `SystemInstance.parameter_overrides`).
    MissingParameter { context: String, name: String },
    /// A declared parameter's name matched no recognized prefix/name for this binding kind --
    /// refused rather than silently ignored.
    UnknownParameter { context: String, name: String },
    /// Questions 82/83: a covariance request against a declared `RelativisticCorrection`
    /// force model was not accompanied by `DrmOptions.accept_missing_stm_terms`.
    MissingStmTermsNotAccepted { instance: String },
    /// `DrmOptions.real_time` was `true`. ADR-005's real-time runtime is still Planned; this
    /// crate only ever runs lockstep, so honouring the request would mean silently running
    /// lockstep while claiming real-time was used -- refused instead.
    RealTimeNotSupported,
    /// `crate::registry::ModelRegistry::construct_gmat`/`construct_native` failed --
    /// `av_dynamics::ModelError`, the one error type every registry constructor and every
    /// erased `BoxedModel` speaks (ADR-005 sec 1), carrying its own model id and a stringified
    /// detail. Replaces the M9.3-era `DrmError::EpochMismatch` (question 96 deletes the A1MJD
    /// round-trip cross-check that variant existed to report -- see `binding`'s module doc
    /// comment's "Epoch" section): a GMAT construction failure is still refused, typed, and
    /// named by model id, just through this one general shape rather than a
    /// binding-epoch-specific one.
    Model(av_dynamics::ModelError),
    /// `DesignReferenceMission.options` or `.scenario` was unset, or a numeric option was
    /// non-positive where positive is required (`sample_interval_s`, an instance's effective
    /// step rate).
    InvalidDrmOptions { reason: String },
    /// A `Fault` named an `instance` no `SosConfiguration.instances` entry has.
    UnknownFaultInstance { fault_id: String, instance: String },
    /// A `FAULT_TARGET_KIND_DYNAMICS` fault's `tai_ns` does not land on the kernel's own
    /// output sampling grid (`DrmOptions.sample_interval_s`) -- every sub-run
    /// `av_kernel::Kernel::run` drives must have a horizon that is an exact multiple of the
    /// output period (`Kernel::run`'s own documented panic condition), so a fault epoch off
    /// that grid cannot be honoured without either silently snapping it (not done) or
    /// panicking (not a caller-facing error) -- refused instead, with the numbers needed to
    /// fix the DRM.
    FaultEpochNotOnSampleGrid { fault_id: String, tai_ns: i64, sample_interval_s: f64 },
    /// Covariance and `FAULT_TARGET_KIND_DYNAMICS` faults were both requested for the same
    /// instance -- not yet supported together (see `executor`'s module doc comment for why).
    CovarianceWithFaultsNotSupported { instance: String },
    /// Covariance was requested for an instance whose bound model does not declare
    /// `stm_capable()` (the native `ConstantAccelModel` placeholder never does).
    ModelNotStmCapable { instance: String },
    /// A `DrmOptions.covariance` run needs a declared initial covariance --
    /// `SystemInstance.initial_covariance` (question 89) was empty. See `executor`'s module
    /// doc comment's "Covariance" section.
    MissingInitialCovariance { instance: String },
    /// A GMAT FFI call failed.
    Gmat(gmat_sys::GmatError),
    /// `av_kernel::schedule::ScheduleError` from the underlying kernel run, stringified (its
    /// own `M::Error` is `binding::AnyModelError`, which is not `Clone`/`'static`-simple
    /// enough to nest here without another layer of boilerplate for no behavioural gain).
    Schedule(String),
    /// `av_cdm::covariance` SPD hygiene failure that `nearest_spd_projection` was not opted
    /// into (mirrors `av_kernel::schedule::ScheduleError::CovarianceHygiene` at this layer).
    CovarianceHygiene(String),
    /// ADR-005 section 5: a `FAULT_TARGET_KIND_PORT`/`_SENSOR` fault (or any future fault kind
    /// needing a random draw) named a `Fault.id` absent from `Scenario.seeds` -- ADR-004
    /// "seeds are logged inputs", so this is refused rather than falling back to an
    /// undeclared/derived seed. See `fault::realize_unapplied_fault`.
    MissingFaultSeed { fault_id: String },
    /// ADR-005 section 5: a `FAULT_TARGET_KIND_PORT`/`FAULT_TARGET_KIND_SENSOR` fault was
    /// declared, its `kind` and `Scenario.seeds` entry both validated, and its seeded PCG64
    /// stream's first draw computed -- but this crate has no router/sensor-model runtime to
    /// apply it to yet (ADR-005's PORT/SENSOR bindings are still Planned). Refused explicitly,
    /// never a silent skip; `realized_draw` carries the deterministic value the stream
    /// produced, for visibility even though nothing consumes it. See
    /// `fault::realize_unapplied_fault`.
    FaultTargetKindNotSupported { fault_id: String, target_kind: String, realized_draw: u64 },
    /// `docs/open-questions.md` question 93: an `Objective`/`MeasureOfEffectiveness.expression`
    /// failed to parse, failed unit-typecheck, or (rarely -- see [`executor::execute`]'s own
    /// doc comment) failed evaluation against the real run. Checked **at load, before any
    /// propagation** (`crate::expr::typecheck::check` never touches sample data, so this runs
    /// against every instance's *declared* shape alone, before a single GMAT step) -- a
    /// malformed expression is refused up front rather than discovered only after a run that
    /// can take tens of seconds. `name` is the `Objective`/`MeasureOfEffectiveness.name`;
    /// `reason` is the underlying `crate::expr::ExprError`'s own `Display`.
    InvalidExpression { name: String, reason: String },
    /// `docs/open-questions.md` question 97: a `Scenario.events` entry's `kind` was not
    /// `"maneuver"` -- the only kind this loader models yet (`ScenarioEvent.kind`'s own doc
    /// comment names `"mode"`/`"contact"`/`"custom"` too, none of which this task builds).
    /// Refused rather than silently accepted as opaque data. See `maneuver::parse`.
    UnsupportedScenarioEventKind { id: String, kind: String },
    /// A `maneuver` `ScenarioEvent` named an `instance` no `SosConfiguration.instances` entry
    /// has (mirrors `DrmError::UnknownFaultInstance`).
    UnknownManeuverInstance { id: String, instance: String },
    /// A `maneuver` `ScenarioEvent`'s `tai_ns` does not land on the kernel's own output
    /// sampling grid (`DrmOptions.sample_interval_s`) -- modeled directly on
    /// `DrmError::FaultEpochNotOnSampleGrid`; see that variant's own doc comment for why this
    /// is refused rather than rounded.
    ManeuverEpochNotOnSampleGrid { id: String, tai_ns: i64, sample_interval_s: f64 },
    /// M21.3 (`docs/open-questions.md` question 141): a `maneuver` `ScenarioEvent` named an
    /// instance whose own carried-over physical state is not exactly 6-dimensional --
    /// `executor::apply_dv_to_state`'s own r/v decomposition only makes sense against a
    /// 6-component Cartesian position/velocity state, and a `"native."`-dispatched instance's
    /// own dimension is no longer a fixed constant (an empty declared state space, `dim == 0`,
    /// is now a legitimate shape -- see `binding::CONSTANT_ACCEL_STATE_DIM`'s own doc comment).
    /// Refused rather than panicking on a length-6 array conversion that can no longer be
    /// assumed to succeed for every `plans`-classified instance.
    ManeuverTargetNotSixDimensional { instance: String, maneuver_id: String, state_dim: usize },
    /// `docs/open-questions.md` question 97, M13.3: a `maneuver` `ScenarioEvent`'s `tai_ns`
    /// lands on the trajectory's own `sample_interval_s` output grid (already checked by
    /// [`DrmError::ManeuverEpochNotOnSampleGrid`]) but *not* on the covariance-requesting
    /// `instance`'s own, possibly coarser, native step grid (`period_ns`, M13.3's lifted
    /// equal-rate restriction -- see `executor`'s module doc comment's "Covariance" section).
    /// Refused rather than silently carrying an unavailable (NaN-sentinel) covariance across the
    /// burn into the next span's own seed `P0`, which `run_covariance_instance`'s "Covariance
    /// across a burn" mechanism requires to be a real, propagated covariance.
    ManeuverEpochNotOnCovarianceGrid { id: String, instance: String, tai_ns: i64, period_ns: i64 },
    /// A `maneuver` `ScenarioEvent`'s `attributes["frame_id"]` named a real `AxesKind` this
    /// loader recognizes, but not one this executor can realize a burn in -- only
    /// RIC/VNB/VVLH and the inertial ICRF/MJ2000Eq frames are supported (question 97's own
    /// scope). See `maneuver::SUPPORTED_FRAMES`.
    ManeuverFrameNotSupported { id: String, frame: String },
    /// `docs/open-questions.md` question 100 (the Gates maneuver execution error model): a
    /// declared `ManeuverExecutionError` block had a non-finite sigma (NaN or +-infinity).
    /// "All four must be present and finite" (`ManeuverExecutionError`'s own proto doc comment)
    /// -- refused rather than propagated into a NaN-poisoned covariance or sampled dv. See
    /// `maneuver::parse_execution_error`.
    InvalidManeuverExecutionError { id: String, reason: String },
    /// Question 100: a declared `ManeuverExecutionError.seed` did not name a key present in
    /// `Scenario.seeds` -- ADR-004 "seeds are logged inputs" applied to this field, checked at
    /// load (unlike a PORT/SENSOR fault's missing seed, which cannot be checked until run time
    /// -- see `maneuver::validate_execution_error_seed`'s own doc comment for why a maneuver's
    /// case has no such excuse).
    UnknownManeuverSeed { id: String, seed: String },
    /// M25.2 (`docs/sil-plan.md`'s M25 milestone: "DRM command events become CDM `Command`s"):
    /// a `command` `ScenarioEvent` named an `instance` no `SosConfiguration.instances` entry
    /// has. Mirrors `DrmError::UnknownManeuverInstance`. See `command::parse`.
    UnknownCommandInstance { id: String, instance: String },
    /// M25.2: a `command` `ScenarioEvent`'s `tai_ns` does not land on the kernel's own output
    /// sampling grid (`DrmOptions.sample_interval_s`) -- the same rule `DrmError::
    /// ManeuverEpochNotOnSampleGrid` already applies to a maneuver, and for the identical
    /// reason: `run_shared_group`'s own boundary loop only ever visits epochs on that grid.
    CommandEpochNotOnSampleGrid { id: String, tai_ns: i64, sample_interval_s: f64 },
    /// M25.2: a `command` `ScenarioEvent`'s `attributes["from"]` (the ground instance
    /// responsible for dispatching it) named an instance no `SosConfiguration.instances` entry
    /// has, or one whose own `SystemDefinition` does not declare a command-telecommand-out
    /// `PacketCodec`/`Port` pair (see `command::resolve_command_dispatch_port`).
    UnknownCommandSender { id: String, sender: String, reason: String },
    /// M25.2: a `command` `ScenarioEvent`'s target `instance` classified to something other than
    /// a `"native."`-dispatched `ConstantAccelModel` with `port.consume_framed` declared (see
    /// `crate::drm::binding::ConstantAccelSpec::consume_framed_port`'s own doc comment) --
    /// dispatch would have nowhere to land. Refused, typed, rather than silently dropping the
    /// encoded packet.
    CommandTargetNotFramedConsumer { id: String, instance: String },
    /// `docs/open-questions.md` question 108: `crate::router::Router::build` refused
    /// `SosConfiguration.connections` -- an undeclared port, a direction mismatch, a kind
    /// mismatch, or an unsupported `link_model`. Checked at load, before any instance runs,
    /// exactly like every other pre-propagation refusal in this executor.
    Router(crate::router::RouterError),
    /// `docs/open-questions.md` question 175 (M25.4a): writing the `PortTrafficLog` sidecar
    /// (`RunConfig.products_dir.join("port_traffic.pb")`) failed -- creating `products_dir`
    /// itself or writing `port_traffic.pb` under it. Never a panic, never a silently-empty
    /// `RunProducts.port_traffic_hash` -- see `executor::execute`'s own module doc comment's
    /// "Port traffic sidecar" section.
    PortTrafficSidecarIo { path: std::path::PathBuf, detail: String },
    /// `docs/open-questions.md` question 107 (M13.2): connecting to a `BINDING_KIND_CONTAINER`
    /// instance's declared `container.address` failed, before any `Bind` was even attempted.
    ContainerConnect { instance: String, address: String, detail: String },
    /// The `Bind` RPC itself failed at the transport/gRPC-status level (as opposed to
    /// succeeding but reporting `lockstep_capable = false`, which is
    /// [`DrmError::ContainerRefused`]).
    ContainerBind { instance: String, detail: String },
    /// `LockstepBindResponse.lockstep_capable` was `false` -- covers both a bare "not
    /// capable" refusal and a declared port-set mismatch (both are reported through the same
    /// `lockstep_capable`/`refusal_reason` fields; `reason` carries the process's own
    /// explanation for which one this was -- see `binding::ContainerError::Refused`'s own doc
    /// comment).
    ContainerRefused { instance: String, reason: String },
    /// Question 155 (`docs/open-questions.md`, the lead's decision on ADR-003/question 84):
    /// "the kernel refuses a non-loopback plaintext endpoint at load with a typed error." A
    /// `BINDING_KIND_CONTAINER` instance's `container.address` (the M13.2 already-running-
    /// process path) named a host `binding::is_loopback_address` does not recognize as loopback
    /// (127.0.0.0/8, `::1`, or the literal string `"localhost"`) while `container.tls` was not
    /// set. The kernel <-> shim gRPC link is plaintext only on loopback within one host (a
    /// container on the same host is that host); anything else must go through the
    /// service-owned nginx mTLS template (`container.tls`/`ca_file`/`client_cert`/`client_key`)
    /// exactly as the dynamics services do. Checked in `binding::parse_container_spec`, before
    /// `binding::materialize_container` would ever dial the address -- the Docker image-
    /// lifecycle path (`ContainerBinding.image`) never reaches this check at all, because
    /// `materialize_container` always connects to the `127.0.0.1:<host_port>` address Docker
    /// itself published, never a caller-supplied one.
    ContainerPlaintextNonLoopback { context: String, address: String },
    /// Question 107: a declared `container.seed_key` did not name a key present in
    /// `Scenario.seeds` -- mirrors [`DrmError::UnknownManeuverSeed`] (ADR-004 "seeds are
    /// inputs" applied to a container binding's own seed).
    UnknownContainerSeed { instance: String, seed_key: String },
    /// A `Step`/`Shutdown` RPC against a bound container instance failed, or the response
    /// violated the lockstep protocol contract (`lockstep.proto`'s own doc comment: "a
    /// response whose sequence does not match is a protocol error and the run stops," and
    /// likewise for `reached_tai_ns`). Wraps the original typed `binding::ContainerError` so
    /// a caller can match the *specific* failure (`SequenceMismatch`/`ReachedTaiMismatch`/
    /// `StepRpc`/`ShutdownRpc`), not just a stringified detail.
    ContainerProtocol { instance: String, source: binding::ContainerError },
    /// Question 107: a container-bound instance's effective step period does not evenly
    /// divide the scenario's own duration -- `run_container_instance`'s dedicated loop (it
    /// does not go through `HeteroKernel`, see `binding`'s own module doc comment) steps at
    /// exactly `period_ns` from `start_tai_ns`, so a non-exact-multiple duration is refused
    /// up front rather than either silently truncated or run one step past `end_tai_ns`.
    ContainerPeriodNotOnGrid { instance: String, period_ns: i64, duration_ns: i64 },
    /// Question 107: a `"maneuver"` `ScenarioEvent`, or **any** `FAULT_TARGET_KIND_DYNAMICS`
    /// fault, named a `BINDING_KIND_CONTAINER` instance -- not supported: a container instance
    /// can never be a maneuver's own target, and a DYNAMICS fault means `apply_dynamics_fault`,
    /// which has no `BindingPlan` to rebind for a container (`executor::run_shared_group`'s own
    /// doc comment). Through M15.3 a DYNAMICS fault of `kind == "power_cycle"` was this
    /// variant's one carved-out exception; **M16.2 (question 120) removes that exception** -- a
    /// container power cycle is `FAULT_TARGET_KIND_HARDWARE` now
    /// (`fault::is_container_power_cycle`), so *every* DYNAMICS fault naming a container is
    /// refused here, unconditionally. (The interim DYNAMICS/`"power_cycle"` shape itself gets
    /// the more specific [`DrmError::PowerCycleFaultMustTargetHardware`] instead, checked first
    /// -- see that variant's own doc comment.) Refused explicitly rather than silently ignored,
    /// the same "say so, never drop it quietly" rule every other refusal in this executor
    /// follows.
    ContainerFaultsOrManeuversNotSupported { instance: String },
    /// Question 120, M16.2: a `FAULT_TARGET_KIND_DYNAMICS` fault whose `kind == "power_cycle"`
    /// -- the M15.3-era interim shape, superseded by the lead's decision to move a container
    /// power cycle to `FAULT_TARGET_KIND_HARDWARE` (that enum value's own doc comment already
    /// names "power cycle"; M15.3's brief wrongly believed HARDWARE was unavailable). Refused
    /// explicitly, at load, so no DRM can keep the interim shape working -- neither silently
    /// re-accepted as a container power cycle (the old `fault::is_container_power_cycle` checked
    /// exactly this shape) nor silently reinterpreted as an ordinary `"parameter"` DYNAMICS
    /// fault. See `fault::is_legacy_dynamics_power_cycle`.
    PowerCycleFaultMustTargetHardware { fault_id: String, instance: String },
    /// Question 120, M16.2: a `FAULT_TARGET_KIND_HARDWARE` fault named a `BINDING_KIND_CONTAINER`
    /// instance with a `kind` other than `"power_cycle"`. HARDWARE's own doc comment also names
    /// "Renode peripheral fault, board reset" -- neither has any meaning for a container instance
    /// today (a container has no Renode peripheral or board to act on, only its own process to
    /// power-cycle), so refused explicitly rather than silently dropped from the boundary set
    /// `executor::run_shared_group` builds.
    HardwareFaultKindNotSupported { fault_id: String, instance: String, kind: String },
    /// Question 120, M16.2: a `FAULT_TARGET_KIND_HARDWARE` fault named an instance that is not
    /// `BINDING_KIND_CONTAINER` (a `BINDING_KIND_MODEL` instance, GMAT or native). HARDWARE
    /// covers a container's own power cycle today, and Renode peripheral faults/board resets
    /// later (both against binding kinds that do not exist yet) -- it has no meaning yet for a
    /// model instance, so this is refused explicitly, at load, rather than silently dropped
    /// from the boundary set the way an unsupported PORT/SENSOR fault currently still is (see
    /// `fault`'s own module doc comment's "Integration note").
    HardwareFaultNotSupportedOnInstance { fault_id: String, instance: String },
    /// M15.3 (question 118): a `BINDING_KIND_CONTAINER` instance declared `ContainerBinding.image`
    /// (the Docker image lifecycle path) but pulling, running, or tearing down that image via
    /// `av_lockstep::docker::ManagedContainer` failed -- wraps the `docker` CLI's own stderr
    /// (`av_lockstep::docker::DockerError`, stringified), never swallowed. Distinct from
    /// [`DrmError::ContainerConnect`]/[`DrmError::ContainerBind`]: those two are about the
    /// `LockstepService` RPC layer once *some* process is already listening at an address; this
    /// variant is about the Docker layer that has to exist first for the image-lifecycle path
    /// (`container.address`-only instances, still M13.2's own subprocess path, never reach this
    /// variant at all -- see `binding::materialize_container`'s own doc comment).
    ContainerDockerLifecycle { instance: String, detail: String },
    /// Question 124, M18.1: a declared `Scenario.frames` entry's `origin` oneof
    /// (`body`/`platform_id`/`entity_id`) or a nested `attitude_source.source` oneof
    /// (`entity_attitude_stream`/`gmat_attitude_model`) had more than one field set. Mirrors
    /// [`DrmError::InvalidBinding`]'s oneof-arity check for `Binding.config` -- see
    /// `schema::RawFrameDefinition`/`RawAttitudeSource`'s own `into_pb`.
    InvalidFrameDefinition { id: String, reason: String },
    /// Question 10/124, M18.1: a run's own central body (`binding::GmatSystemSpec.
    /// central_body`) could not be turned into one of the three mandatory registry frames
    /// (ICRF/MJ2000Eq/BodyFixed for that body -- question 10's day-one mandatory frames) through
    /// `executor::registry_default_frame`'s own body-axes-suffix naming convention.
    /// Structurally unreachable today -- `registry_default_frame` recognizes any nonempty body
    /// plus one of its own four known suffixes, and `binding::parse_gmat_spec` already refuses
    /// an empty `central_body` (`DrmError::MissingParameter`) before this is ever reached --
    /// kept as a typed refusal rather than a panic or, worse, a silently missing mandatory
    /// frame, per this task's own "no silent fallback" rule.
    MandatoryFrameNotRealizable { body: String, suffix: &'static str },
    /// Question 128, M19.1 (ADR-002's fourth amendment): a `"gmat."`-bound instance's declared
    /// `spacecraft.CoordinateSystem` names something other than its own integration frame
    /// (`binding::GmatSystemSpec.central_body`'s MJ2000Eq -- GMAT's raw internal propagation
    /// buffer, which `gmat_sys::model::GmatModel::initial_state_si`/every later sample actually
    /// reads back, regardless of what `CoordinateSystem` the DRM declares) **and** the frame it
    /// does name does not decompose as `{body}{ICRF|MJ2000Eq|MJ2000Ec|BodyFixed}`
    /// (`executor::body_axes_suffix` -- this crate's own registry-realizable vocabulary).
    /// Before M19.1's `convert` shim landed, this was refused unconditionally for *any*
    /// non-integration-frame value (the fix for question 128's own defect: the loader used to
    /// copy the declared name straight into `Trajectory.frame_id` as a pure label while the
    /// numbers underneath stayed silently in the integration frame -- a mislabelled trajectory,
    /// never caught). M19.1 lifts that refusal for exactly the frames `executor::
    /// convert_gmat_trajectory_to_declared_frame` can actually realize through GMAT's
    /// `CoordinateConverter::Convert`; a frame outside that vocabulary is still refused here,
    /// typed, rather than silently falling back to the integration frame either before or after
    /// that capability existed.
    UnsupportedCoordinateSystem { context: String, declared: String, integration_frame: String },
    /// Question 129, M19.2 (ADR-002's fourth amendment): a computed `FrameDefinition.
    /// fixed_rotation_q` violated the wire's own contract (`core.proto` field 13's doc
    /// comment) -- length other than 0 or 4, or (length 4) a norm that is not 1 within
    /// [`executor::FIXED_ROTATION_UNIT_NORM_TOLERANCE`]. Refused, typed, rather than silently
    /// truncated, padded or re-normalized away -- this task's own "no silent fallbacks" rule
    /// applied to a field this crate computes itself: a value this malformed means this
    /// crate's own rotation-matrix-to-quaternion arithmetic has a bug, and a caller deserves a
    /// typed refusal naming the frame, not a quietly "fixed" quaternion or a wire value that
    /// looks unit-length but is not quite. See `executor::validate_fixed_rotation_q`.
    InvalidFixedRotationQuaternion { frame_id: String, reason: String },
    /// Question 149, M22.3: `schema::packet_codecs` ran a declared `SystemDefinition.
    /// packet_codecs` list through `crate::codec::validate_system_packet_codecs` (a field
    /// extent past `user_data_bytes`, a `bit_width` disagreeing with a fixed-width type, or a
    /// duplicate `apid`) and it was refused. Wraps the typed `codec::CodecError` the same way
    /// [`DrmError::Router`]/[`DrmError::Model`]/[`DrmError::Gmat`] wrap their own sub-crate
    /// error types, rather than re-deriving a parallel set of variants here.
    Codec(crate::codec::CodecError),
    /// M22.1b (`docs/open-questions.md` questions 151/152, decided by the lead): a
    /// `"attitude."`-dispatched instance's declared parameters or declared state space failed
    /// one of `crate::drm::attitude`'s own typed, load-time checks (`crate::drm::attitude::
    /// parse_attitude_spec`'s missing/unknown/invalid-parameter checks, or `crate::drm::
    /// attitude::AttitudeWheelsModel::new`'s own state-space-dimension/wheel-momentum-unit
    /// checks) -- wraps `crate::drm::attitude::AttitudeSpecError`'s `Display` verbatim rather
    /// than re-deriving a parallel set of per-field `DrmError` variants for a spec this module
    /// does not itself parse (`parse_attitude_spec` remains the one parser -- see that
    /// function's own doc comment, and `binding::classify_binding`'s own `ModelKind::Attitude`
    /// arm for exactly where this is raised).
    InvalidAttitudeSpec { instance: String, reason: String },
    /// M22.2b (`docs/open-questions.md` questions 142/149/151/152, decided by the lead): a
    /// `"startracker."`-dispatched instance's declared parameters, or its own constructed
    /// `crate::drm::sensors::StarTrackerModel`'s remaining checks (a declared `PacketCodec`
    /// missing a required field), failed one of `crate::drm::sensors`'s own typed, load-time
    /// checks (`crate::drm::sensors::parse_star_tracker_spec`'s missing/unknown/invalid-parameter
    /// checks). Wraps `crate::drm::sensors::SensorSpecError`'s `Display` verbatim, mirroring
    /// `DrmError::InvalidAttitudeSpec` for exactly the same reason (a spec this module does not
    /// itself parse -- `parse_star_tracker_spec` remains the one, reused parser).
    InvalidStarTrackerSpec { instance: String, reason: String },
    /// The IMU counterpart of [`DrmError::InvalidStarTrackerSpec`] -- wraps
    /// `crate::drm::sensors::SensorSpecError`'s `Display` for a `"imu."`-dispatched instance
    /// (`crate::drm::sensors::parse_imu_spec`/`ImuModel::new`).
    InvalidImuSpec { instance: String, reason: String },
    /// M22.4 (`docs/sil-plan.md`'s M22 milestone paragraph, "a native 'controller' instance
    /// closes the loop first"): a `"attctrl."`-dispatched instance's declared parameters, or its
    /// own constructed `crate::drm::controller::AttitudeControllerModel`'s remaining checks (a
    /// declared star tracker/IMU/wheel-torque-command `PacketCodec` missing a required field),
    /// failed one of `crate::drm::controller`'s own typed, load-time checks. Wraps
    /// `crate::drm::controller::ControllerSpecError`'s `Display` verbatim, mirroring
    /// `DrmError::InvalidStarTrackerSpec`/`InvalidImuSpec` for exactly the same reason (a spec
    /// this module does not itself parse -- `parse_attitude_controller_spec` remains the one,
    /// reused parser).
    InvalidControllerSpec { instance: String, reason: String },
    /// M22.2b: a `"startracker."`/`"imu."`-dispatched instance's declared `SystemDefinition` did
    /// not declare exactly one `packet_codecs` entry, or exactly one `PORT_KIND_FRAMED`/
    /// `PORT_DIRECTION_OUT` port -- the structural convention `crate::drm::binding::
    /// resolve_sensor_output` uses to decide which declared codec/port a sensor instance's own
    /// measurement packets go out on. Deliberately reuses `SystemDefinition.packet_codecs`/
    /// `.ports` (both already declared, hashed fields every FRAMED-port fixture needs regardless)
    /// rather than inventing a new `"startracker.output_port"`-style parameter naming the same
    /// thing twice.
    SensorPortConfiguration { instance: String, reason: String },
    /// M25.1 (`docs/sil-plan.md`'s M25 milestone; `docs/open-questions.md` questions 10/108/149):
    /// a `"ground."`-dispatched instance's declared parameters, or its own constructed
    /// `crate::drm::ground::GroundStationModel`'s remaining checks (a declared telemetry
    /// `PacketCodec` missing a required position field), failed one of `crate::drm::ground`'s own
    /// typed, load-time checks. Wraps `crate::drm::ground::GroundSpecError`'s `Display` verbatim,
    /// mirroring `DrmError::InvalidStarTrackerSpec`/`InvalidImuSpec`/`InvalidControllerSpec` for
    /// exactly the same reason (a spec this module does not itself parse -- `parse_ground_
    /// station_spec` remains the one, reused parser).
    InvalidGroundStationSpec { instance: String, reason: String },
    /// M25.1: a `"ground."`-dispatched instance's declared `SystemDefinition` did not declare
    /// exactly the two `packet_codecs` entries (one telemetry-in, one telecommand-out) and the two
    /// `PORT_KIND_FRAMED` ports (`tm_in`/`tc_out`) `crate::drm::binding::resolve_ground_ports`
    /// requires -- the structural convention question 149's own "FRAMED telecommand port and a
    /// FRAMED telemetry port with declared PacketCodecs" requirement resolves to. Mirrors
    /// `DrmError::SensorPortConfiguration`'s own reasoning, applied to the ground station's own
    /// two-port (not one-port) convention.
    GroundPortConfiguration { instance: String, reason: String },
    /// R4.1a/R4.1b (`docs/open-questions.md` question 178): a `FAULT_TARGET_KIND_PORT` fault's
    /// own `kind` was not one of ADR-005 section 5's own four documented PORT kinds
    /// (`crate::drm::fault::PORT_KINDS`: `"drop"`, `"delay"`, `"corrupt"`, `"duplicate"`) at all --
    /// refused at load, before any binding or GMAT call, the same "checked up front" pattern
    /// every other fault-validation refusal in this executor follows. **R4.1b removed the sibling
    /// `DrmError::PortFaultKindNotYetSupported` variant this crate carried through R4.1a**: it
    /// existed only to name a real, documented PORT kind this crate had not implemented yet
    /// (`"corrupt"`/`"duplicate"`); now that all four documented kinds have a real runtime, no
    /// PORT fault can ever reach that "documented but unimplemented" state again, so the variant
    /// was dead code the moment R4.1b landed and was deleted rather than left unreachable (see
    /// `R4_1B_REPORT.md`) -- this variant is the only one left for a PORT fault's own `kind`.
    UnknownPortFaultKind { fault_id: String, instance: String, kind: String },
    /// `docs/open-questions.md` question 178 (R5.1a/R5.1b): a `FAULT_TARGET_KIND_SENSOR` fault
    /// named a `"startracker."`- or `"imu."`-dispatched instance, but its own `kind` was not one
    /// of the shared documented vocabulary both sensor models support (`crate::drm::fault::
    /// SENSOR_KINDS`: `"bias"`, `"dropout"`, `"freeze"`, `"scale"`) -- refused at load, before any
    /// binding or GMAT call, mirroring [`DrmError::UnknownPortFaultKind`]'s identical role for
    /// PORT.
    UnknownSensorFaultKind { fault_id: String, instance: String, kind: String },
    /// `docs/open-questions.md` question 178 (R5.1a): a `FAULT_TARGET_KIND_SENSOR` fault named an
    /// `instance` that is not a sensor model instance at all (neither `"startracker."`- nor
    /// `"imu."`-dispatched -- `crate::registry::kind_for`) -- refused at load, before any binding
    /// or GMAT call. `binding` names the instance's own `SystemDefinition.dynamics_model`, so the
    /// refusal is self-explaining without a second lookup.
    SensorFaultTargetNotASensor { fault_id: String, instance: String, binding: String },
    /// `docs/open-questions.md` questions 178/184/186(b) (R5.1a): two `FAULT_TARGET_KIND_SENSOR`
    /// faults naming the same star-tracker `instance` have overlapping `[tai_ns, tai_ns +
    /// duration_ns)` windows. Mirrors `crate::router::RouterError::OverlappingPortFaultWindows`'s
    /// own rule and reasoning **keyed one level coarser** than that PORT precedent: PORT keys on
    /// `(instance, port)` because two different ports are two genuinely independent channels a
    /// `Router` can act on simultaneously; a star tracker's own SENSOR fault runtime instead
    /// carries its currently-installed effect in ONE `Option<crate::drm::sensors::
    /// StarTrackerFaultEffect>` slot (`StarTrackerSpec::fault`) regardless of which of the four
    /// kinds it is, so two faults naming *different* targets on the *same instance* (e.g. a
    /// `"bias"` fault on `startracker.bias_rad.x` and a `"scale"` fault on `startracker.scale`)
    /// could not both be installed at once either -- keying the refusal on `target` alone, the
    /// way PORT's `port` does, would let such a pair load and then silently let the later one
    /// clobber the earlier one's effect for the overlap, which is exactly the ambiguous-join
    /// failure mode this refusal exists to prevent. Refusing any overlap on the same instance,
    /// regardless of target, is what keeps the event-to-effect join unambiguous (mirrors question
    /// 184's own reasoning) and is what makes "at most one SENSOR fault is ever in force on one
    /// instance at a time" (question 186(c)'s `frames_affected` attribution) actually true.
    /// **No `target_a`/`target_b` fields** (unlike a first draft of this variant): `clippy::
    /// result_large_err` flags `DrmError` itself once any one variant exceeds its own size
    /// threshold, cascading into 81 unrelated "this Result is too large" errors across every
    /// function in this crate returning `Result<_, DrmError>` -- two more `String` fields pushed
    /// this variant to 144 bytes; `instance` alone (the actual join key -- see above) is already
    /// sufficient to look the two faults' own declared targets up in `Scenario.faults` if a
    /// caller needs them, exactly the same information-is-derivable-not-duplicated convention
    /// `crate::router`'s own module doc comment already applies to `PortTrafficRecord` not
    /// carrying a fault-attribution field.
    OverlappingSensorFaultWindows { fault_a: String, fault_b: String, instance: String, overlap_start_tai_ns: i64, overlap_end_tai_ns: Option<i64> },
    /// M25.4b (question 175's own follow-on): `RunConfig.replay.log_path` could not be read,
    /// or its bytes (once hash-verified -- see [`DrmError::ReplayLogHashMismatch`]) did not
    /// decode as a `PortTrafficLog` -- `crate::drm::replay::verify_and_load`'s own doc comment
    /// explains why this is a separate variant from a hash mismatch (two genuinely different
    /// failures: "not the file the run producer named" vs. "could not even be read/parsed").
    ReplayLogIo { path: std::path::PathBuf, detail: String },
    /// M25.4b: `RunConfig.replay.log_path`'s exact bytes did not hash to `RunConfig.replay.
    /// expected_hash` -- checked BEFORE any binding, any GMAT call, and any step
    /// (`crate::drm::replay::verify_and_load`, called first thing inside `executor::execute`
    /// whenever `RunConfig.replay` is `Some`). Never loosened, never a warning: a replay run
    /// refuses to even start against a log it cannot verify came from the run it claims to.
    ReplayLogHashMismatch { path: std::path::PathBuf, expected: String, computed: String },
    /// M25.4b: `RunConfig.replay.instances` named an instance absent from `SosConfiguration.
    /// instances` -- a typed load refusal, checked before any binding, mirroring every other
    /// "named instance does not exist" refusal in this executor (`DrmError::
    /// UnknownFaultInstance`, `DrmError::UnknownManeuverInstance`, ...).
    UnknownReplayInstance { instance: String },
    /// R5.1b review (`docs/open-questions.md` question 178): `RunConfig.replay.instances` named an
    /// instance that a `FAULT_TARGET_KIND_SENSOR` fault targets. Refused at load, because the
    /// replayed run would otherwise silently produce the WRONG products rather than fail:
    /// `crate::drm::replay::ReplayModel` is deliberately content-agnostic and its
    /// `drain_sensor_fault_effect` always returns `None`, so the fault's own `EVENT_KIND_FAULT`
    /// (which `executor::run_shared_group` emits only from that drain's accumulated totals) never
    /// appears at all and the replayed `RunProducts` is not byte-identical to the run it claims to
    /// replay. Question 178's own standing rule -- a missing runtime is a typed refusal naming what
    /// is absent, never a silent no-op -- applied to replay. **A PORT fault is unaffected**:
    /// `crate::router::Router` re-applies it at delivery, to the pre-fault bytes replay plays back,
    /// and reproduces byte-identically (`tests/replay.rs`'s own `t5_...`). Replaying a DIFFERENT
    /// instance in a SENSOR-faulted run is likewise unaffected and fully supported (`t6_...`).
    ReplayInstanceHasSensorFault { instance: String, fault_id: String },
    /// M25.4b: `RunConfig.replay` and `DrmOptions.covariance` were both requested. Not
    /// supported together -- the covariance path (`executor::run_covariance_instance`) is
    /// unchanged by this task (see `executor`'s own module doc comment's "One shared kernel
    /// run" section) and never consults `RunConfig.replay` at all, so combining the two would
    /// otherwise silently ignore the replay request on the covariance path while honouring it
    /// on the plain path -- refused explicitly instead, the same "never silently drop a
    /// request" rule every other combination refusal in this executor already follows.
    ReplayWithCovarianceNotSupported,
    // M14.1 (question 109) added `ContainerPeriodExceedsSampleInterval` here: a
    // BINDING_KIND_CONTAINER instance's own effective step period had to evenly divide
    // `DrmOptions.sample_interval_s`'s own output period, because the shared kernel run
    // (`executor::run_shared_group`) samples every registered system, container included, at the
    // trajectory's own output grid and, through M14.3, unconditionally Hermite-interpolated a
    // system whose own native period is coarser than that grid -- which panics for a container's
    // `state_dim() == 0` (`crate::interpolate::hermite_velocity` requires at least 6 components).
    // **M14.4 lifts this restriction and removes the variant.** A container has no state to
    // interpolate, so ADR-005 sec 3's "discrete modes, counters | zero-order hold" rule applies
    // instead: `crate::kernel::HeteroKernel::run_with_ports` now routes a zero-dimensional system
    // through `crate::schedule::HeteroScheduler::sample_held` (never `hermite_velocity`), and the
    // only restriction left on a container's own period is the one every instance already has --
    // an integer multiple of the GCD-derived base period, enforced generically by
    // `HeteroKernel::run_with_ports`'s own `base_period_ns` gate ([`DrmError::Schedule`]). See
    // `executor::run_shared_group`'s own doc comment's "Container period vs. the trajectory's own
    // output grid" section for the full account, including how a held sample is still recorded,
    // distinguishably, on the container's own `Trajectory.provenance.attributes` (no proto
    // change).
}

impl std::fmt::Display for DrmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DrmError::Yaml(e) => write!(f, "DRM YAML did not parse: {e}"),
            DrmError::InvalidEnumValue { field, value } => write!(f, "{field}: {value:?} is not a recognized enum value"),
            DrmError::UnsupportedField { field } => write!(f, "{field} is not yet modeled by this loader and was non-empty; refusing rather than silently dropping it"),
            DrmError::InvalidBinding { reason } => write!(f, "invalid Binding: {reason}"),
            DrmError::HashMismatch { artifact, id, declared, computed } => {
                write!(f, "{artifact} {id:?}: declared hash {declared:?} does not match its canonical hash {computed:?}; refusing to run a tampered artifact")
            }
            DrmError::UnsupportedBinding { instance, kind } => write!(f, "instance {instance:?}: binding kind {kind} is not yet supported by this executor"),
            DrmError::UnknownSystemDefinition { instance, system_id } => write!(f, "instance {instance:?}: no SystemDefinition supplied for system_id {system_id:?}"),
            DrmError::UnknownStateSpace { instance, state_space_id } => write!(f, "instance {instance:?}: state_space_id {state_space_id:?} is not a StateSpace this kernel declares"),
            DrmError::InvalidStateSpace { instance, reason } => write!(f, "instance {instance:?}: declared state_space is invalid: {reason}"),
            DrmError::StateSpaceDimensionMismatch { instance, declared_dim, model_state_dim } => write!(
                f,
                "instance {instance:?}: declared state_space has {declared_dim} component(s), but this instance's own binding produces a {model_state_dim}-dimensional physical state; a fixture's declared state space must match what it actually binds to"
            ),
            DrmError::UnknownSosConfiguration { declared, provided } => write!(f, "DRM names sos_configuration_id {declared:?} but the supplied SosConfiguration.id is {provided:?}"),
            DrmError::MissingParameter { context, name } => write!(f, "{context}: missing required parameter {name:?}"),
            DrmError::UnknownParameter { context, name } => write!(f, "{context}: parameter {name:?} is not a recognized name for this binding kind"),
            DrmError::MissingStmTermsNotAccepted { instance } => write!(f, "instance {instance:?}: covariance requested against a force model declaring RelativisticCorrection, without DrmOptions.accept_missing_stm_terms"),
            DrmError::RealTimeNotSupported => write!(f, "DrmOptions.real_time is true; this executor only ever runs lockstep (ADR-005's real-time runtime is still Planned)"),
            DrmError::Model(e) => write!(f, "{e}"),
            DrmError::InvalidDrmOptions { reason } => write!(f, "invalid DrmOptions/Scenario: {reason}"),
            DrmError::UnknownFaultInstance { fault_id, instance } => write!(f, "fault {fault_id:?} names instance {instance:?}, which is not in this SosConfiguration"),
            DrmError::FaultEpochNotOnSampleGrid { fault_id, tai_ns, sample_interval_s } => {
                write!(f, "fault {fault_id:?} at tai_ns {tai_ns} does not land on the sample_interval_s={sample_interval_s} output grid")
            }
            DrmError::CovarianceWithFaultsNotSupported { instance } => write!(f, "instance {instance:?}: covariance and DYNAMICS faults requested together are not yet supported"),
            DrmError::ModelNotStmCapable { instance } => write!(f, "instance {instance:?}: covariance requested but the bound model is not STM-capable"),
            DrmError::MissingInitialCovariance { instance } => write!(f, "instance {instance:?}: covariance requested but SystemInstance.initial_covariance was empty"),
            DrmError::Gmat(e) => write!(f, "{e}"),
            DrmError::Schedule(e) => write!(f, "{e}"),
            DrmError::CovarianceHygiene(e) => write!(f, "{e}"),
            DrmError::MissingFaultSeed { fault_id } => write!(f, "fault {fault_id:?} needs a random draw but Scenario.seeds has no entry keyed by its id"),
            DrmError::FaultTargetKindNotSupported { fault_id, target_kind, realized_draw } => {
                write!(f, "fault {fault_id:?} (target_kind {target_kind}): seeded and validated (deterministic draw {realized_draw:#018x}) but this executor has no runtime for {target_kind} faults yet (ADR-005 PORT/SENSOR bindings are still Planned)")
            }
            DrmError::InvalidExpression { name, reason } => write!(f, "expression {name:?} is invalid: {reason}"),
            DrmError::UnsupportedScenarioEventKind { id, kind } => write!(f, "scenario event {id:?}: kind {kind:?} is not yet modeled by this loader (only \"maneuver\"/\"command\" are)"),
            DrmError::UnknownManeuverInstance { id, instance } => write!(f, "maneuver {id:?} names instance {instance:?}, which is not in this SosConfiguration"),
            DrmError::UnknownCommandInstance { id, instance } => write!(f, "command {id:?} names instance {instance:?}, which is not in this SosConfiguration"),
            DrmError::CommandEpochNotOnSampleGrid { id, tai_ns, sample_interval_s } => {
                write!(f, "command {id:?} at tai_ns {tai_ns} does not land on the sample_interval_s={sample_interval_s} output grid")
            }
            DrmError::UnknownCommandSender { id, sender, reason } => write!(f, "command {id:?} names sender {sender:?}: {reason}"),
            DrmError::CommandTargetNotFramedConsumer { id, instance } => write!(f, "command {id:?} names target instance {instance:?}, which does not classify to a native ConstantAccelModel with port.consume_framed declared"),
            DrmError::ManeuverEpochNotOnSampleGrid { id, tai_ns, sample_interval_s } => {
                write!(f, "maneuver {id:?} at tai_ns {tai_ns} does not land on the sample_interval_s={sample_interval_s} output grid")
            }
            DrmError::ManeuverTargetNotSixDimensional { instance, maneuver_id, state_dim } => write!(
                f,
                "maneuver {maneuver_id:?} names instance {instance:?}, whose own physical state is {state_dim}-dimensional, not the 6-component Cartesian position/velocity shape a dv jump requires"
            ),
            DrmError::ManeuverEpochNotOnCovarianceGrid { id, instance, tai_ns, period_ns } => {
                write!(f, "maneuver {id:?} on instance {instance:?} at tai_ns {tai_ns} does not land on that instance's own {period_ns} ns covariance step grid -- the covariance carried across this burn would not be a real, propagated value")
            }
            DrmError::ManeuverFrameNotSupported { id, frame } => write!(f, "maneuver {id:?}: frame {frame} is not RIC/VNB/VVLH or an inertial frame -- this executor cannot realize a burn in it"),
            DrmError::InvalidManeuverExecutionError { id, reason } => write!(f, "maneuver {id:?}: invalid execution_error: {reason}"),
            DrmError::UnknownManeuverSeed { id, seed } => write!(f, "maneuver {id:?}: execution_error.seed {seed:?} names no entry in Scenario.seeds"),
            DrmError::Router(e) => write!(f, "{e}"),
            DrmError::ContainerConnect { instance, address, detail } => write!(f, "instance {instance:?}: connecting to lockstep process at {address:?} failed: {detail}"),
            DrmError::ContainerBind { instance, detail } => write!(f, "instance {instance:?}: Bind RPC failed: {detail}"),
            DrmError::ContainerRefused { instance, reason } => write!(f, "instance {instance:?}: lockstep process refused (lockstep_capable=false): {reason}"),
            DrmError::ContainerPlaintextNonLoopback { context, address } => write!(
                f,
                "{context}: container.address {address:?} is plaintext (container.tls is not set) and is not a recognized loopback address (127.0.0.0/8, ::1, or \"localhost\") -- the kernel <-> shim gRPC link is plaintext only on loopback within one host; set container.tls (with ca_file/client_cert/client_key) for any other host (question 155)"
            ),
            DrmError::UnknownContainerSeed { instance, seed_key } => write!(f, "instance {instance:?}: container.seed_key {seed_key:?} names no entry in Scenario.seeds"),
            DrmError::ContainerProtocol { instance, source } => write!(f, "instance {instance:?}: {source}"),
            DrmError::ContainerPeriodNotOnGrid { instance, period_ns, duration_ns } => {
                write!(f, "instance {instance:?}: scenario duration ({duration_ns} ns) is not an exact multiple of this container instance's own step period ({period_ns} ns)")
            }
            DrmError::ContainerFaultsOrManeuversNotSupported { instance } => write!(f, "instance {instance:?}: a BINDING_KIND_CONTAINER instance does not support maneuvers or any FAULT_TARGET_KIND_DYNAMICS fault (a power cycle is FAULT_TARGET_KIND_HARDWARE now)"),
            DrmError::PowerCycleFaultMustTargetHardware { fault_id, instance } => write!(
                f,
                "fault {fault_id:?} on instance {instance:?}: a power-cycle fault must be FAULT_TARGET_KIND_HARDWARE, not FAULT_TARGET_KIND_DYNAMICS (question 120 moved it off DYNAMICS; the interim kind=\"power_cycle\" DYNAMICS shape is no longer accepted)"
            ),
            DrmError::HardwareFaultKindNotSupported { fault_id, instance, kind } => {
                write!(f, "fault {fault_id:?} on container instance {instance:?}: FAULT_TARGET_KIND_HARDWARE kind {kind:?} is not supported (only \"power_cycle\" is, for a container instance)")
            }
            DrmError::HardwareFaultNotSupportedOnInstance { fault_id, instance } => {
                write!(f, "fault {fault_id:?}: FAULT_TARGET_KIND_HARDWARE names instance {instance:?}, which is not a BINDING_KIND_CONTAINER instance -- HARDWARE has no runtime yet for a model instance (Renode/board bindings are still Planned)")
            }
            DrmError::ContainerDockerLifecycle { instance, detail } => write!(f, "instance {instance:?}: Docker image lifecycle failed: {detail}"),
            DrmError::InvalidFrameDefinition { id, reason } => write!(f, "frame {id:?}: {reason}"),
            DrmError::MandatoryFrameNotRealizable { body, suffix } => {
                write!(f, "central body {body:?}: could not realize the mandatory {suffix} frame (question 10's day-one mandatory frames)")
            }
            DrmError::UnsupportedCoordinateSystem { context, declared, integration_frame } => write!(
                f,
                "{context}: spacecraft.CoordinateSystem {declared:?} is neither this instance's own integration frame ({integration_frame:?}) nor a frame this registry can realize (must decompose as {{body}}{{ICRF|MJ2000Eq|MJ2000Ec|BodyFixed}})"
            ),
            DrmError::InvalidFixedRotationQuaternion { frame_id, reason } => {
                write!(f, "frame {frame_id:?}: computed fixed_rotation_q is invalid: {reason}")
            }
            DrmError::Codec(e) => write!(f, "{e}"),
            DrmError::InvalidAttitudeSpec { instance, reason } => write!(f, "instance {instance:?}: invalid attitude spec: {reason}"),
            DrmError::InvalidStarTrackerSpec { instance, reason } => write!(f, "instance {instance:?}: invalid star tracker spec: {reason}"),
            DrmError::InvalidImuSpec { instance, reason } => write!(f, "instance {instance:?}: invalid IMU spec: {reason}"),
            DrmError::InvalidControllerSpec { instance, reason } => write!(f, "instance {instance:?}: invalid attitude controller spec: {reason}"),
            DrmError::SensorPortConfiguration { instance, reason } => write!(f, "instance {instance:?}: invalid sensor port/codec configuration: {reason}"),
            DrmError::InvalidGroundStationSpec { instance, reason } => write!(f, "instance {instance:?}: invalid ground station spec: {reason}"),
            DrmError::GroundPortConfiguration { instance, reason } => write!(f, "instance {instance:?}: invalid ground station port/codec configuration: {reason}"),
            DrmError::UnknownPortFaultKind { fault_id, instance, kind } => write!(
                f,
                "fault {fault_id:?} on instance {instance:?}: FAULT_TARGET_KIND_PORT kind {kind:?} is not one of ADR-005 section 5's own documented PORT kinds (\"drop\", \"delay\", \"corrupt\", \"duplicate\")"
            ),
            DrmError::UnknownSensorFaultKind { fault_id, instance, kind } => write!(
                f,
                "fault {fault_id:?} on instance {instance:?}: FAULT_TARGET_KIND_SENSOR kind {kind:?} is not one of the star tracker's own documented kinds (\"bias\", \"dropout\", \"freeze\", \"scale\")"
            ),
            DrmError::SensorFaultTargetNotASensor { fault_id, instance, binding } => write!(
                f,
                "fault {fault_id:?} names instance {instance:?}, whose own binding {binding:?} is not a sensor model instance (neither \"startracker.\"- nor \"imu.\"-dispatched)"
            ),
            DrmError::OverlappingSensorFaultWindows { fault_a, fault_b, instance, overlap_start_tai_ns, overlap_end_tai_ns } => {
                let end = overlap_end_tai_ns.map(|e| e.to_string()).unwrap_or_else(|| "end of run".to_string());
                write!(f, "faults {fault_a:?} and {fault_b:?} on instance {instance:?} have overlapping windows: [{overlap_start_tai_ns}, {end})")
            }
            DrmError::PortTrafficSidecarIo { path, detail } => write!(f, "writing the port traffic sidecar to {}: {detail}", path.display()),
            DrmError::ReplayLogIo { path, detail } => write!(f, "replay log {}: {detail}", path.display()),
            DrmError::ReplayLogHashMismatch { path, expected, computed } => {
                write!(f, "replay log {}: declared hash {expected:?} does not match its own computed hash {computed:?}; refusing to replay from a log that cannot be verified", path.display())
            }
            DrmError::UnknownReplayInstance { instance } => write!(f, "RunConfig.replay.instances names instance {instance:?}, which is not in this SosConfiguration"),
            DrmError::ReplayInstanceHasSensorFault { instance, fault_id } => write!(
                f,
                "RunConfig.replay.instances names instance {instance:?}, which FAULT_TARGET_KIND_SENSOR fault {fault_id:?} targets: crate::drm::replay::ReplayModel has no sensor fault runtime (its drain_sensor_fault_effect always returns None), so replaying it would drop that fault's own EVENT_KIND_FAULT and produce products that are not byte-identical to the run being replayed -- replay a different instance, or drop the SENSOR fault (docs/open-questions.md question 178)"
            ),
            DrmError::ReplayWithCovarianceNotSupported => write!(f, "RunConfig.replay and DrmOptions.covariance were both requested; not yet supported together"),
        }
    }
}
impl std::error::Error for DrmError {}
