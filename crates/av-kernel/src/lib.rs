//! `av-kernel`: the deterministic kernel skeleton (M2.1).
//!
//! A **clock authority** ([`clock::Clock`]), lockstep only -- ADR-005 (the runtime this
//! eventually plugs into: bindings, port router, container/Renode) is still Planned, and
//! nothing else of it is invented here. A **multi-rate scheduler**
//! ([`schedule::Scheduler`]) that drives any number of [`av_dynamics::DynamicsModel`]
//! systems, each at its own declared step rate, from that one clock. **Hermite-with-velocity
//! interpolation** ([`interpolate::hermite_velocity`]) for output requested at a time that
//! isn't one of a system's own native step epochs -- the CDM's declared contract
//! (`INTERPOLATION_HERMITE_VELOCITY`, `proto/altavista/v1/trajectory.proto`), matched here by
//! formula, not merely by name. And **CDM v1 `Trajectory` assembly** ([`trajectory`]) from the
//! resulting samples. [`kernel::Kernel`] ties these four pieces together into the smallest
//! thing that can run a system and emit a `Trajectory`.
//!
//! ## Determinism (ADR-002 / ADR-004)
//!
//! Simulated time is TAI nanoseconds, `i64`, never a float ([`clock::Clock`]). Iteration over
//! systems is always by system id, `BTreeMap` ([`schedule::Scheduler`], [`kernel::Kernel`]) --
//! never `HashMap`. No RNG. No wall-clock read that affects a result: the clock only ever
//! advances by an explicit call, never by reading the system clock.
//!
//! ## GMAT dependency: everywhere except `clock`/`interpolate`/`schedule`/`kernel`/`trajectory`
//!
//! `clock`, `interpolate`, `schedule`, `kernel` and `trajectory` depend only on `av-cdm` and
//! `av-dynamics`, neither of which touches GMAT -- every unit test in those five modules runs
//! against a small synthetic [`av_dynamics::DynamicsModel`] (constant acceleration, closed-form
//! solution known) and needs no GMAT install. `tests/golden_acceptance.rs` drives a real
//! `gmat_sys::model::GmatModel` through this crate's `Kernel` and checks the result against
//! `goldens/leo_1day_jgm2_8x8_sunmoon.json` -- the acceptance test ADR-002 asks for; that test
//! takes `gmat_sys::engine_lock()` first, per this repository's existing convention.
//!
//! **[`drm`] is the one exception (M6.1).** Binding a `BINDING_KIND_MODEL` space-system
//! instance to a real `GmatModel` is exactly what the DRM executor does, so `gmat-sys` is a
//! normal dependency of this crate as of `drm`'s addition, not a dev-dependency -- see
//! `drm`'s own module doc comment for exactly which of its functions touch GMAT (most do not:
//! only the ones that construct a `"gmat."`-dispatched instance) and why.
//!
//! ## ADR-005 sec 1-2: heterogeneous scheduling, the model registry, the base-period clock
//!
//! [`schedule::Scheduler`]/[`Kernel`] (the homogeneous, single-model-type pair every existing
//! test above still drives directly) are unchanged. Alongside them: [`schedule::HeteroScheduler`]
//! holds `BTreeMap<InstanceName, Box<dyn DynamicsModel<Error = ModelError>>>` -- several
//! systems, each its **own** `DynamicsModel` kind, one clock, sorted-name visiting order --
//! and [`HeteroKernel`] assembles its output into CDM `Trajectory`s the same way `Kernel` does.
//! [`registry::ModelRegistry`] maps a `SystemDefinition.dynamics_model` id to the constructor
//! that builds one: native, GMAT (through `gmat-sys`), or a remote `DynamicsService` stub (see
//! that module's doc comment for exactly what the remote constructor does today). As of M10.3
//! (`docs/open-questions.md` question 98) `ModelRegistry` is the *sole* constructor -- `drm::
//! executor` no longer materializes `drm::binding::AnyModel` itself, only `ModelRegistry`'s
//! opaque [`registry::ModelHandle`] -- see `registry`'s own module doc comment. [`clock::
//! base_period_ns`]/[`clock::check_integer_multiples`] are the base-period clock and its
//! integer-multiple check (every instance's step period must be an integer multiple of the
//! base period, refused otherwise, naming the offending instance) -- wired into
//! `HeteroScheduler::base_period_ns` as a load-time gate.

//! ## Ports and the router (ADR-005 sec 4, `docs/open-questions.md` question 108, M13.1)
//!
//! [`ports`] and [`router`] are GMAT-free, like `clock`/`interpolate`/`schedule`/`kernel`/
//! `trajectory`. [`router::Router`] realizes `SosConfiguration.connections` as a routing table
//! -- validated once, at construction, against every named instance's declared
//! `SystemDefinition.ports` -- and the run-time queue of messages waiting for delivery, in the
//! deterministic order question 108 decided (receiving instance, then port name, then sender
//! emission epoch, then sender instance id). [`schedule::HeteroScheduler::advance_to_with_ports`]/
//! [`kernel::HeteroKernel::run_with_ports`] drive a run through it, calling
//! `av_dynamics::DynamicsModel::step_with_ports` instead of `step` -- a model that never
//! overrides that method (every `MODEL` binding in this workspace today) behaves exactly as it
//! did before this method existed, per that method's own doc comment.

//! ## CCSDS packet framing (`docs/open-questions.md` question 149, M22.3)
//!
//! [`codec`] packs/unpacks CCSDS space packets for a FRAMED port whose `Port.schema` is
//! `"ccsds.spp"`, against a declared `PacketCodec` (`proto/altavista/v1/packet.proto`) --
//! GMAT-free, like `ports`/`router`. `crate::drm::schema` types `SystemDefinition.
//! packet_codecs` and calls `codec::validate_system_packet_codecs` at load time (an ordered
//! `BTreeMap<apid, PacketCodec>`, never a `HashMap`); see `codec`'s own module doc comment for
//! the primary header layout and the typed faults it refuses.

pub mod clock;
pub mod codec;
pub mod drm;
pub mod expr;
pub mod interpolate;
pub mod kernel;
pub mod ports;
pub mod registry;
pub mod rng;
pub mod router;
pub mod schedule;
pub mod trajectory;

pub use clock::{base_period_ns, check_integer_multiples, BasePeriodError, Clock};
pub use codec::{ApidMap, CodecError, DecodedPacket, FieldValue};
pub use interpolate::{hermite_velocity, interpolate_by_state_space, InterpolationError};
pub use kernel::{HeteroKernel, Kernel};
pub use ports::{Inbox, Outbox, PortMessage};
pub use registry::{ModelHandle, ModelKind, ModelRegistry};
pub use router::{Router, RouterError};
pub use schedule::{HeteroScheduleError, HeteroScheduler, ScheduleError, Scheduler};
pub use trajectory::{state_space_for, UnknownStateSpaceError};
