//! `av-dynamics`: the shared dynamics contract and integrator (ADR-002 "One dynamics
//! contract, GMAT at three depths, goldens as the proof").
//!
//! Every dynamics model in the platform -- GMAT-backed or native, whatever binding hosts it
//! -- implements [`DynamicsModel`] over CDM v1 types. `derivatives` is the portable form: it
//! is what lets an external integrator (or a different engine entirely) own the state
//! stepping. `step` is what the kernel calls; ADR-002 also names `propagate` (design-time,
//! returns a `Trajectory`) and `solve` (a declared capability) as the contract's other two
//! verbs, but those are out of scope for this crate's skeleton (M2.1) -- `propagate` belongs
//! above `step` (the kernel or a design-time caller drives many `step`s and assembles the
//! `Trajectory`; see `av-kernel`), and `solve` is per-model.
//!
//! **No GMAT dependency.** This crate must build, and be unit-tested, in an environment that
//! has never linked GMAT. `crates/gmat-sys::model::GmatModel` is the GMAT-backed
//! [`DynamicsModel`] implementation, and it lives in `gmat-sys` (which already depends on
//! GMAT) for exactly that reason -- not here.
//!
//! ## Determinism (ADR-002 / ADR-004)
//!
//! No `HashMap` on any output path (`BTreeMap` throughout: [`StepResult::outputs`],
//! [`settings_hash`]'s input), no RNG, no wall-clock read that affects a result. Time is TAI
//! nanoseconds (`i64`) at every boundary this crate exposes; the integrator's own internal
//! clock is `f64` seconds measured **relative to the current step's start** and is converted
//! back to an absolute TAI instant before every call into [`DynamicsModel::derivatives`], so
//! a model never observes a time value that isn't an honest TAI nanosecond count -- only the
//! integrator's own substep arithmetic is floating point.
//!
//! ## Trait objects behind one error type (ADR-005 sec 1)
//!
//! [`DynamicsModel::Error`] stays an unconstrained associated type -- every existing impl in
//! this workspace (`gmat_sys::model::GmatModel`'s `GmatError`, this crate's own test models'
//! `Infallible`, `av_kernel::drm::binding::AnyModel`'s `AnyModelError`) keeps compiling
//! unchanged. [`ModelError`] (module [`error`]) is the one concrete error type a *trait
//! object* speaks -- `Box<dyn DynamicsModel<Error = ModelError>>` (aliased [`BoxedModel`]) --
//! and [`ErasedModel`]/[`erase_with_id`] (module [`erase`]) is the adapter that gets any
//! `M: DynamicsModel` into that shape by converting `M::Error` to `ModelError`. See both
//! modules' doc comments for why the trait itself did not need to change to get here.

use std::collections::BTreeMap;

pub mod erase;
pub mod error;
pub mod integrate;
pub mod stm;

use integrate::Dopri5;
pub use erase::{erase_with_id, BoxedModel, ErasedModel};
pub use error::ModelError;
pub use stm::{propagate_covariance, StmAugmented};

/// SHA-256 over a canonically-ordered settings description, for `ModelInfo.settings_hash`
/// (ADR-002: "SHA-256 of the model's settings [...]"). Encodes each entry as `"key=value\n"`
/// and hashes them in `BTreeMap` (i.e. key-sorted) order, so the digest is a pure function of
/// the settings themselves and never depends on the order the caller happened to insert them
/// in (ADR-002 / ADR-004 determinism: no `HashMap` on an output path). `sha2` is pure Rust
/// with no bundled C crypto, matching ADR-004's crypto rule; used only for this
/// non-security-critical settings fingerprint, never for the platform's signing path.
pub fn settings_hash(settings: &BTreeMap<String, String>) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for (k, v) in settings {
        hasher.update(k.as_bytes());
        hasher.update(b"=");
        hasher.update(v.as_bytes());
        hasher.update(b"\n");
    }
    format!("{:x}", hasher.finalize())
}

/// One message on one port, exactly [`altavista.v1.PortMessage`]
/// (`proto/altavista/v1/lockstep.proto`) -- the type `av_kernel::router::Router` builds
/// [`Inbox`]/[`Outbox`] from, and the same type M13.2's lockstep wire protocol will carry
/// verbatim (`docs/open-questions.md` question 108). A type alias, not a redefinition, so
/// every layer that touches a port message -- this trait, the router, and eventually the
/// lockstep client -- speaks the identical wire type from load to run to (later) the network,
/// never a parallel hand-rolled struct that could drift from it. `payload`'s encoding is
/// declared per the port's `PortKind` (`lockstep.proto`'s own `PortMessage` doc comment): a
/// SIGNAL port carries a little-endian IEEE-754 double ([`encode_signal`]/[`decode_signal`]),
/// a CDM port its protobuf wire form, a FRAMED port one frame per message, a BYTE_STREAM port
/// the bytes emitted in the step.
pub type PortMessage = av_cdm::pb::PortMessage;

/// Every port message available to a model at the start of one
/// [`DynamicsModel::step_with_ports`] call: everything `av_kernel::router::Router` delivered
/// to this instance for this step, already in the router's own deterministic order (receiving
/// instance, then port name, then sender emission epoch, then sender instance id --
/// `docs/open-questions.md` question 108). A model with no ports never looks at this; the
/// default [`step_with_ports`](DynamicsModel::step_with_ports) implementation does not either.
///
/// **Sender attribution (`docs/open-questions.md` question 130).** Alongside each
/// [`PortMessage`] this also carries the sending instance's id, when known -- a parallel
/// `senders` vector (index-aligned with `messages`, not a `HashMap`: ADR-004 determinism)
/// rather than widening `PortMessage` itself, since that type is exactly
/// `altavista.v1.PortMessage` (`proto/altavista/v1/lockstep.proto`, read-only for this task)
/// and carries no sender field on the wire at all -- question 108's own delivery order is
/// itself defined in terms of a sender identity the wire message never needed to carry
/// forward past routing. [`Inbox::new`] (every hand-built `Inbox` in this workspace's own
/// tests) leaves every sender `None` -- an honest "unknown," not a placeholder; only
/// `av_kernel::router::Router` (via [`Inbox::new_with_senders`]) ever fills these in, from the
/// sender identity it already tracks per queued message.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Inbox {
    messages: Vec<PortMessage>,
    senders: Vec<Option<String>>,
}
impl Inbox {
    pub fn new(messages: Vec<PortMessage>) -> Self {
        let senders = vec![None; messages.len()];
        Self { messages, senders }
    }
    /// Like [`Inbox::new`], but with each message's own sender instance id attached
    /// (`docs/open-questions.md` question 130) -- `av_kernel::router::Router` is the only
    /// caller expected to have one to attach. `messages` and `senders` must be the same
    /// length and index-aligned (the router builds both from the same already-sorted list, so
    /// this is never handed mismatched vectors in practice).
    pub fn new_with_senders(messages: Vec<PortMessage>, senders: Vec<Option<String>>) -> Self {
        debug_assert_eq!(messages.len(), senders.len(), "Inbox::new_with_senders: messages and senders must be index-aligned");
        Self { messages, senders }
    }
    pub fn empty() -> Self {
        Self::default()
    }
    pub fn messages(&self) -> &[PortMessage] {
        &self.messages
    }
    /// Every message addressed to `port`, in the order this `Inbox` already carries them (the
    /// router's own delivery order -- see the struct doc comment).
    pub fn for_port<'a>(&'a self, port: &'a str) -> impl Iterator<Item = &'a PortMessage> {
        self.messages.iter().filter(move |m| m.port == port)
    }
    /// The *last* message on `port` (question 108: last among ties on one port is the most
    /// recently emitted) together with its sender, if known -- `None` if no message on this
    /// port is present at all. The one place a port's "the message" selection logic lives, so
    /// a consumer's own value lookup and a sender lookup (question 130's applied-command
    /// events need both) can never disagree about which message was actually "the" one.
    pub fn last_on_port(&self, port: &str) -> Option<(&PortMessage, Option<&str>)> {
        let idx = self.messages.iter().rposition(|m| m.port == port)?;
        Some((&self.messages[idx], self.senders[idx].as_deref()))
    }
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }
}

/// Every port message one [`DynamicsModel::step_with_ports`] call emits, handed to
/// `av_kernel::router::Router` for delivery to each connected receiver at its own next step
/// (`docs/open-questions.md` question 108). The default
/// [`step_with_ports`](DynamicsModel::step_with_ports) implementation always returns an empty
/// one.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Outbox {
    messages: Vec<PortMessage>,
}
impl Outbox {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn push(&mut self, port: impl Into<String>, tai_ns: i64, payload: Vec<u8>) {
        self.messages.push(PortMessage { port: port.into(), tai_ns, payload });
    }
    /// A SIGNAL port carries a little-endian IEEE-754 double (`lockstep.proto`'s own
    /// `PortMessage` doc comment) -- this is the one encode site in this crate, so every
    /// SIGNAL emitter produces byte-identical payloads (see [`decode_signal`], its inverse).
    pub fn push_signal(&mut self, port: impl Into<String>, tai_ns: i64, value: f64) {
        self.push(port, tai_ns, encode_signal(value));
    }
    pub fn messages(&self) -> &[PortMessage] {
        &self.messages
    }
    pub fn into_messages(self) -> Vec<PortMessage> {
        self.messages
    }
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }
}

/// Encode a SIGNAL port payload: a little-endian IEEE-754 double
/// (`proto/altavista/v1/lockstep.proto`'s `PortMessage` doc comment). The inverse of
/// [`decode_signal`].
pub fn encode_signal(value: f64) -> Vec<u8> {
    value.to_le_bytes().to_vec()
}

/// Decode a SIGNAL port's little-endian IEEE-754 double payload -- `None` if `payload` is not
/// exactly 8 bytes. The inverse of [`encode_signal`]/[`Outbox::push_signal`].
pub fn decode_signal(payload: &[u8]) -> Option<f64> {
    let bytes: [u8; 8] = payload.try_into().ok()?;
    Some(f64::from_le_bytes(bytes))
}

/// One SIGNAL/CDM/... command a model actually wrote into its own bound configuration during
/// a [`DynamicsModel::step_with_ports`] call -- not merely present on a port, but genuinely
/// applied (`docs/open-questions.md` question 130: a consumed SIGNAL that writes into a
/// GMAT-bound model's own field, e.g. `Cd`, bypasses the hashed settings
/// `av_dynamics::settings_hash`/`ModelInfo.settings_hash` covers, so `dynamics_hash` alone no
/// longer implies equal dynamics -- this type is what lets a caller record the command
/// itself as a CDM event instead). "Applied" is the bar a model reports here: a message that
/// arrived but was not applied (failed to decode, or nothing was on the port at all) reports
/// nothing, exactly like `gmat_sys::model::GmatModel::step_with_ports`'s own doc comment
/// states for its `consume` path.
///
/// **M20.3 (`docs/open-questions.md` question 137) narrows "applied" further: changed, or
/// first.** The underlying write to the model's own bound configuration still happens on
/// every step a message decodes, unconditionally -- the physics never depends on whether an
/// `AppliedCommand` is reported. What changed is the reporting bar itself: a model now reports
/// one of these only when the value it just wrote for a given `(port, field)` differs, by
/// exact equality (never a tolerance -- the command stream is a deterministic product of the
/// run), from the value it last reported for that same `(port, field)`, or when it has never
/// reported that `(port, field)` before at all (the first application always emits). This is
/// what keeps a continuously rate-commanded parameter (10 Hz for two hours: 71,999 events,
/// M19.3's own measurement) from producing one event per step while still recording every
/// genuine change in a stream whose value actually moves. See
/// `gmat_sys::model::GmatModel::step_with_ports`'s own doc comment for exactly where that
/// last-reported cache lives and why a fault/maneuver re-materialization -- which resets a
/// previously-commanded field back to its spec value -- must never let this cache survive it.
///
/// `port`/`field`/`value` are exactly what the model wrote; `applied_tai_ns` is the step's own
/// *start* epoch (`step_with_ports`'s own `t_tai_ns` argument) -- the instant from which the
/// commanded value is actually in effect (every sub-step this call's own integration takes
/// sees it), not the message's *delivery* epoch (`PortMessage.tai_ns`), which can differ from
/// it by however long the message sat queued. Deliberately carries no sender: a model sees
/// only its own [`Inbox`], which may or may not know one (see `Inbox`'s own doc comment) --
/// the caller that has [`Inbox::last_on_port`] in scope (`av_kernel::schedule::HeteroScheduler`)
/// is what attaches the sender, once, right after this call returns.
///
/// **The two-epoch trap (`docs/open-questions.md` question 187, ratified by the lead after two
/// rounds of the identical mistake).** `applied_tai_ns` is this step's own START epoch, above --
/// and that is ALSO the epoch the `ACKED` `CommandTransition` lands at (`av_kernel::drm::command::
/// acked_event`'s own call site passes `cmd.applied_tai_ns` directly, unchanged, never `+
/// period`). The ack TELEMETRY PACKET itself, in contrast, is pushed onto the model's own
/// `Outbox` at this SAME step's own RESULT (end) epoch -- `applied_tai_ns + period` (`period`
/// being this step's own elapsed duration/native step, `dt_ns`) -- because the model computes and
/// sends the ack only after finishing the step it just applied the command within (mirrors
/// `av_kernel::drm::binding::ConstantAccelModel::step_with_ports`'s own `outbox.push(port, result.
/// t_tai_ns, payload)`, and `av_kernel::drm::gmat_command::GmatFramedCommandModel::step_with_
/// ports`'s identical shape). These are two genuinely different, both meaningful epochs -- a
/// caller that conflates them (uses `applied_tai_ns` alone where the ack's own real wire emission
/// epoch is meant, or vice versa) gets a plausible-looking but wrong answer, which is exactly what
/// happened twice (`crates/av-kernel/tests/port_traffic_sidecar.rs`'s own module doc comment has
/// the first, root-caused occurrence). `av_kernel::drm::command::ack_emission_epoch(applied,
/// period) -> applied + period` is the one place this relation is written, used at every call
/// site that needs the ack's own real emission epoch rather than repeating the addition inline.
#[derive(Debug, Clone, PartialEq)]
pub struct AppliedCommand {
    pub port: String,
    pub field: String,
    pub value: f64,
    pub applied_tai_ns: i64,
}

/// See [`DynamicsModel::drain_sensor_fault_effect`] (`docs/open-questions.md` question 178,
/// R5.1a).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SensorFaultEffectDrain {
    /// TAI nanoseconds of the first sensor emission this fault changed or suppressed, since the
    /// last drain.
    pub first_effect_tai_ns: i64,
    /// The total count of emissions this fault changed or suppressed, since the last drain.
    pub frames_affected: u64,
}

/// The result of [`DynamicsModel::step`]: the propagated state, the new absolute epoch, and
/// any named side outputs (ADR-002: `step(state, t, controls, dt) -> (state, t + dt,
/// outputs)`). `outputs` is a `BTreeMap` for the same reason as [`settings_hash`]'s input --
/// whatever populates it must not depend on iteration order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StepResult {
    pub state: Vec<f64>,
    pub t_tai_ns: i64,
    pub outputs: BTreeMap<String, f64>,
}

/// One dynamics model, over CDM v1 types (ADR-002 "One contract").
///
/// `derivatives` is the portable form every model must supply -- it's what lets another
/// engine own the integrator instead of this one. `step` is what the kernel calls; **the
/// default implementation drives [`integrate::Dopri5`] over `derivatives`, so a model that
/// implements only `derivatives` gets a correct, adaptive `step` for free.** A model with its
/// own native stepper (in principle, GMAT's own propagator) may override `step` directly --
/// nothing here requires the default.
///
/// ## No trait default returns a valid *empty* result (question 112)
///
/// `docs/open-questions.md` question 112 names a recurring defect: a wrapper that erases/boxes
/// a `DynamicsModel` (`erase::ErasedModel`, `av_kernel::drm::binding::AnyModel`) forgot to
/// delegate a method to the *wrapped* model, so the wrapper silently fell back to *this trait's*
/// own default instead -- three times (`step` in M10.3, `step_with_stm` in M11.2,
/// `step_with_ports` in M13.1/M13.2), each invisible because the fallback default still returned
/// something plausible. The decision: every default on this trait was re-audited against "does
/// this default return a valid but *empty*/no-op result a caller could mistake for the real
/// thing" -- and every method here is now one of exactly three shapes:
///
/// - **Required** (`state_dim`, `derivatives`, `describe`): no default exists, so there is
///   nothing to silently fall back to.
/// - **A default that is genuinely, functionally complete for a model that opts out of the
///   capability** (`integrator`, `step`, `step_with_stm`, `step_with_ports`): none of these
///   return an empty/no-op placeholder -- `integrator` returns real, documented tolerances;
///   `step`/`step_with_stm` really do drive [`integrate::Dopri5`] over `derivatives`/
///   `stm_derivatives` and produce a physically correct answer (only the *side* `outputs` map is
///   ever empty, and only for a model that has none to report -- itself an honest answer, not a
///   no-op); `step_with_ports` really does call `step` and returns a correct, empty `Outbox` for
///   a model that does not participate in ports -- required because
///   `av_kernel::schedule::{Scheduler, HeteroScheduler}` call this method unconditionally for
///   *every* registered system regardless of whether it uses ports, so this default cannot become
///   `Err` (a `NotSupported` default here would break every model in this workspace that never
///   overrides it) and cannot become a required method either (it would force every existing and
///   future non-ported model, in every crate that implements this trait, to hand-write the
///   identical three-line override for no safety benefit -- the actual defect was never here, it
///   was in the wrapper types not delegating, which M13.1/M13.2 and this task close directly).
///   The original three-occurrence defect is closed by making `erase::ErasedModel` and
///   `av_kernel::drm::binding::AnyModel` delegate *every* method explicitly (never relying on
///   this trait's own default for a wrapper's own dispatch) -- see `erase`'s module doc comment
///   and its `erased_model_reaches_inner_*` test suite for the one-test-per-method proof.
/// - **A default that panics on a declared-capability precondition violation**
///   (`stm_derivatives`): not "a valid empty result" at all -- it never returns -- so it already
///   fails loudly, just not as a typed `Result`. It cannot become `Err(ModelError::NotSupported)`
///   here because `Self::Error` is this trait's own unconstrained associated type (by design --
///   see `error`'s module doc comment for why the trait itself must not require `From<ModelError>`
///   on every implementor, `std::convert::Infallible` foremost among them, which cannot implement
///   any such conversion at all). Where the concrete error type *is* fixed -- at exactly the
///   trait-object boundary `ErasedModel`/`AnyModel` provide -- the panic is upgraded to a typed,
///   catchable `ModelError::CapabilityMissing`/`AnyModelError::CapabilityMissing` instead (see
///   `erase::ErasedModel::stm_derivatives`'s own doc comment); the generic default here stays a
///   panic for direct (non-erased) callers, where "check `stm_capable()` first" remains an
///   ordinary, review-enforceable precondition.
pub trait DynamicsModel {
    /// The model's own error type (an FFI error, a native model's own domain error, ...).
    type Error: std::fmt::Debug + std::fmt::Display;

    /// Dimension of the state vector `derivatives` / `step` operate on.
    fn state_dim(&self) -> usize;

    /// `state_dot = f(state, t, controls)`, written into `state_dot` rather than returned, so
    /// the shared integrator (which calls this several times per accepted step) does not
    /// allocate on every evaluation. `t_tai_ns` is the *absolute* epoch the state is valid
    /// at, TAI nanoseconds (ADR-001 / ADR-002): every model sees real time, never a
    /// step-relative offset, even though [`step`](DynamicsModel::step)'s own substep
    /// arithmetic (below) is `f64` seconds internally.
    fn derivatives(&self, state: &[f64], t_tai_ns: i64, controls: &[f64], state_dot: &mut [f64]) -> Result<(), Self::Error>;

    /// `altavista.v1.ModelInfo`: state space, controls, capabilities, and the settings hash
    /// (ADR-002 "describe").
    fn describe(&self) -> av_cdm::pb::ModelInfo;

    /// Tolerances/step-size control [`step`](DynamicsModel::step)'s default implementation
    /// hands to [`integrate::Dopri5`]. `Dopri5::default()` (`rtol = atol = 1e-12`) is the
    /// setting the P0 spike and this crate's golden acceptance test were both measured at;
    /// override only to change that.
    fn integrator(&self) -> Dopri5 {
        Dopri5::default()
    }

    /// `step(state, t, controls, dt) -> (state, t + dt, outputs)` (ADR-002). The default
    /// implementation integrates [`derivatives`](DynamicsModel::derivatives) from `t_tai_ns`
    /// to `t_tai_ns + dt_ns` with [`integrate::Dopri5`]; `outputs` is empty unless a model
    /// overrides `step` to populate it. `dt_ns` must be positive: `Dopri5::integrate`'s loop
    /// condition (`while t < t1 - 1e-9`) only advances forward, so a non-positive `dt_ns`
    /// silently returns the input state unchanged rather than erroring -- callers (the
    /// kernel's scheduler) are expected to only ever step forward, and this is not
    /// independently guarded here.
    fn step(&self, state: &[f64], t_tai_ns: i64, controls: &[f64], dt_ns: i64) -> Result<StepResult, Self::Error> {
        let dt_s = dt_ns as f64 * 1e-9;
        let (x1, _stats) = self.integrator().integrate(
            |t_rel_s, x, out| {
                let t_ns = t_tai_ns + (t_rel_s * 1e9).round() as i64;
                self.derivatives(x, t_ns, controls, out)
            },
            state,
            0.0,
            dt_s,
        )?;
        Ok(StepResult { state: x1, t_tai_ns: t_tai_ns + dt_ns, outputs: BTreeMap::new() })
    }

    /// Like [`step`](DynamicsModel::step), but with the port router's [`Inbox`] delivered for
    /// this call and an [`Outbox`] collected from it (`docs/open-questions.md` question 108:
    /// "`HeteroKernel` passes the inbox to step and collects the outbox").
    ///
    /// **The default implementation ignores `inbox`, calls
    /// [`step`](DynamicsModel::step) exactly as before, and returns an empty [`Outbox`]** --
    /// this is the task's required "MODEL bindings get a default no-port implementation so
    /// nothing existing changes": every `DynamicsModel` in this workspace today
    /// (`gmat_sys::model::GmatModel`, `av_kernel::drm::binding::AnyModel`, every test model in
    /// this crate and in `av-kernel`) overrides neither this method nor `step`'s own signature,
    /// so nothing about any of them changes by this method existing.
    /// `av_kernel::schedule::HeteroScheduler`/`Scheduler` call this method (never `step`
    /// directly) when routing ports, so a ported and an unported model run through the
    /// identical call site -- only a model that actually overrides this method ever sees a
    /// non-empty `Inbox` or produces a non-empty `Outbox`.
    ///
    /// **Why a new method, rather than adding `inbox`/an `Outbox` return value to `step`
    /// itself?** `step`'s signature is a public contract every model in this workspace already
    /// implements, including `gmat_sys::model::GmatModel` -- a crate this task does not own.
    /// Changing `step`'s own signature would force every one of those implementations to
    /// change in lockstep with this task, including code outside this task's ownership; a new
    /// default-provided method gives the kernel exactly the "pass the inbox, collect the
    /// outbox" hook the task asks for without that ripple.
    ///
    /// **A third tuple element, `Vec<AppliedCommand>` (`docs/open-questions.md` question 130,
    /// M19.3).** Unlike the `step`-vs-`step_with_ports` split above, this crate DOES own every
    /// implementation of `step_with_ports` in this workspace (`gmat_sys::model::GmatModel`,
    /// `av_kernel::drm::binding`'s `ConstantAccelModel`/`ContainerModel`/`AnyModel`, and this
    /// crate's own `erase::ErasedModel`) -- the exact condition the doc comment above says
    /// would justify widening a signature directly instead of adding a new method, so this one
    /// widens rather than adding a fourth method. Reports every command this call *actually
    /// applied* to the model's own bound configuration (as opposed to a command merely present
    /// on a port) -- see [`AppliedCommand`]'s own doc comment for the exact bar and why a
    /// model reports no sender. Empty for every model that never calls
    /// `self.model.set_real_parameter`-equivalent from a consumed message (everything except
    /// `GmatModel`'s own `consume` path today) -- exactly the same "empty, not absent" shape
    /// `Outbox`'s own default already established, never a `None`/absent case to special-case.
    fn step_with_ports(&self, state: &[f64], t_tai_ns: i64, controls: &[f64], dt_ns: i64, inbox: &Inbox) -> Result<(StepResult, Outbox, Vec<AppliedCommand>), Self::Error> {
        let _ = inbox;
        Ok((self.step(state, t_tai_ns, controls, dt_ns)?, Outbox::new(), Vec::new()))
    }

    /// CDM `Measurement`s (`docs/open-questions.md` question 173, M25.3) this model's own most
    /// recent [`step_with_ports`](DynamicsModel::step_with_ports) call actually produced. A
    /// model that emits telemetry mapped to a CDM measurement (a sensor whose declared
    /// `PacketCodec` fields carry a non-empty `PacketField.target`) overrides this to return
    /// what it just computed, typically cached in a `RefCell` cleared and repopulated at the top
    /// of every `step_with_ports` call (mirrors `gmat_sys::model::GmatModel::last_applied`'s own
    /// interior-mutable "what did the last call produce" cache shape). Every other model in this
    /// workspace -- which is most of them -- returns `Vec::new()` explicitly, each with its own
    /// one-line comment saying why that model produces no CDM measurement.
    ///
    /// **Required, not defaulted (M25.3c standing rule: no trait default that returns a valid
    /// empty result).** Through M25.3 this had a `Vec::new()` default -- exactly the failure mode
    /// the rule exists to prevent: a future sensor model that forgets to override it would
    /// silently emit no telemetry, indistinguishable from "this model genuinely has none",
    /// caught by nothing (not a compiler error, not a test, not a review diff against a method
    /// nobody had to touch). Making it required costs every implementer one honest line and
    /// changes no call site -- unlike widening `step_with_ports`'s own tuple (see that method's
    /// own doc comment's "a third tuple element" section for why *that* widening is not done
    /// lightly): a new required method on an existing trait touches only `impl` blocks, not
    /// callers.
    ///
    /// **Why a separate method, not a fourth `step_with_ports` tuple element** (the shape
    /// `Vec<AppliedCommand>` itself took when it was added, M19.3): that widening's own
    /// precedent (see [`step_with_ports`](DynamicsModel::step_with_ports)'s own doc comment,
    /// "a third tuple element") is sound only when the entire call-site surface bends in
    /// lockstep with zero risk -- true for a workspace-owned type with few callers. By M25.3,
    /// `step_with_ports` has 16 overrides and 60+ call sites across `av-dynamics`, `gmat-sys`
    /// and `av-kernel` (most of them test-only tuple destructuring, `let (result, outbox,
    /// applied) = ...`), so widening the tuple again would touch every one of them for a
    /// capability only two models (`StarTrackerModel`/`ImuModel`, question 173) actually use. A
    /// required method with no default needs none of those call sites to change -- only the
    /// (much smaller) set of `impl DynamicsModel for ...` blocks.
    fn last_measurements(&self) -> Vec<av_cdm::pb::Measurement>;

    /// `docs/open-questions.md` question 178 (R5.1a, the SENSOR fault runtime for the star
    /// tracker): the accumulated effect of this model's own currently-installed SENSOR fault
    /// (if any) since the last time this was called -- the epoch of the first sensor emission
    /// it changed or suppressed, and the total count of emissions it changed or suppressed.
    /// `None` when no SENSOR fault is installed on this model, or one is installed but has not
    /// yet taken effect (no truth has arrived, or the window has not been reached).
    ///
    /// **A required method with no default, mirroring [`last_measurements`](DynamicsModel::
    /// last_measurements)'s own precedent and reasoning (question 112): a caller drains this at
    /// every re-materialization boundary a faulted instance survives (`crate::drm::sensors` --
    /// this crate has no `crate::drm` of its own, so no call site lives here -- but every model
    /// in this workspace, including this one, must answer this explicitly), so a model with no
    /// SENSOR fault runtime returning an implicit, defaulted `None` would be indistinguishable
    /// from a model that legitimately has nothing to report this call -- the same silent-gap
    /// risk `last_measurements`'s own doc comment already explains for a defaulted empty
    /// `Vec`.** Every model in this workspace implements this explicitly (almost always a
    /// trivial `None`) -- only `crate::drm::sensors::StarTrackerModel` (in `av-kernel`, the one
    /// model with a SENSOR fault runtime) ever returns `Some`.
    fn drain_sensor_fault_effect(&self) -> Option<SensorFaultEffectDrain>;

    /// Whether this model can also propagate its own state transition matrix (STM)
    /// alongside the state (ADR-002 second amendment, `docs/adr/002-dynamics-contract.md`).
    /// A **declared capability**, checked by the kernel before it ever calls
    /// [`stm_derivatives`](DynamicsModel::stm_derivatives) -- covariance is *only* propagated
    /// when a caller explicitly requests it for a model that answers `true` here (question 11:
    /// covariance is always optional and explicitly requested, never a silent default).
    /// Default: `false`. A model that overrides this to `true` MUST also override
    /// `stm_derivatives`.
    fn stm_capable(&self) -> bool {
        false
    }

    /// `state_dot` for the state **augmented** with its own state transition matrix: index
    /// `0..state_dim()` is the physical state (must equal what
    /// [`derivatives`](DynamicsModel::derivatives) would compute there -- ADR-002's amendment
    /// measured this bit-identical for the GMAT case), index `state_dim() + row*state_dim() +
    /// col` is `d(Phi)/dt` element `(row, col)`, row-major (the layout the ADR-002 second
    /// amendment measured and verified against GMAT's own `Spacecraft::GetRmatrixParameter
    /// ("STM")`). `augmented_state.len() == augmented_state_dot.len() == state_dim() +
    /// state_dim()^2`.
    ///
    /// # Panics
    ///
    /// The default implementation is `unimplemented!()` -- only ever meant to be reached if a
    /// caller invokes this on a model whose [`stm_capable`](DynamicsModel::stm_capable)
    /// returns `false` and is therefore a caller bug (the kernel is expected to check
    /// `stm_capable()` first, exactly like every other declared-capability check in this
    /// contract), not a condition a well-behaved caller can trigger.
    #[allow(unused_variables)]
    fn stm_derivatives(&self, augmented_state: &[f64], t_tai_ns: i64, controls: &[f64], augmented_state_dot: &mut [f64]) -> Result<(), Self::Error> {
        unimplemented!(
            "stm_derivatives called on a model whose stm_capable() is false (or that did not \
             override this method); the caller must check stm_capable() before calling this"
        )
    }

    /// Like [`step`](DynamicsModel::step), but over the STM-augmented state
    /// ([`stm_derivatives`](DynamicsModel::stm_derivatives)) rather than the plain physical
    /// state: seeds `Phi(t0, t0) = I` (the exact identity, per the ADR-002 amendment's
    /// measurement) and integrates `[state; vec(Phi)]` forward by `dt_ns`, so `result.phi` on
    /// return is `Phi(t_tai_ns, t_tai_ns + dt_ns)` -- **relative to `state`'s own instant**,
    /// not necessarily `Phi(t0, ...)` for some other epoch `t0` the caller has in mind. Only
    /// meaningful when [`stm_capable`](DynamicsModel::stm_capable) is `true`; see
    /// `stm_derivatives`'s panic note.
    fn step_with_stm(&self, state: &[f64], t_tai_ns: i64, controls: &[f64], dt_ns: i64) -> Result<StmStepResult, Self::Error> {
        let n = self.state_dim();
        let mut aug0 = vec![0.0; n + n * n];
        aug0[0..n].copy_from_slice(state);
        for i in 0..n {
            aug0[n + i * n + i] = 1.0;
        }
        let dt_s = dt_ns as f64 * 1e-9;
        let (aug1, _stats) = self.integrator().integrate(
            |t_rel_s, x, out| {
                let t_ns = t_tai_ns + (t_rel_s * 1e9).round() as i64;
                self.stm_derivatives(x, t_ns, controls, out)
            },
            &aug0,
            0.0,
            dt_s,
        )?;
        let (state1, phi) = aug1.split_at(n);
        Ok(StmStepResult { state: state1.to_vec(), phi: phi.to_vec(), t_tai_ns: t_tai_ns + dt_ns, outputs: BTreeMap::new() })
    }
}

/// The result of [`DynamicsModel::step_with_stm`]: the propagated physical state, the state
/// transition matrix over the interval just integrated (`n x n`, row-major, `n =
/// state_dim()`), and any named side outputs (question 101, M11.2) -- the [`StepResult::
/// outputs`] sibling this type lacked through M11.1, which is exactly what left a covariance
/// run with fewer named products than a plain run (`docs/open-questions.md` question 101: "product
/// sets must not depend on the run mode"). Empty unless a model overrides `step_with_stm` to
/// populate it, mirroring `outputs`'s own default-empty contract on [`StepResult`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StmStepResult {
    pub state: Vec<f64>,
    /// Row-major `n x n`, `phi[row * n + col]` is `Phi`'s `(row, col)` element.
    pub phi: Vec<f64>,
    pub t_tai_ns: i64,
    pub outputs: BTreeMap<String, f64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// A model with a closed-form solution (constant acceleration), used to check that the
    /// default `step` really does drive `Dopri5` correctly and that `derivatives` sees
    /// absolute TAI nanoseconds. 6-state `[pos_x,y,z, vel_x,y,z]`, constant acceleration `a`.
    struct ConstantAccel {
        a: [f64; 3],
        calls: Cell<usize>,
        last_t_tai_ns: Cell<i64>,
    }

    impl DynamicsModel for ConstantAccel {
        type Error = std::convert::Infallible;

        fn state_dim(&self) -> usize {
            6
        }

        fn derivatives(&self, state: &[f64], t_tai_ns: i64, _controls: &[f64], out: &mut [f64]) -> Result<(), Self::Error> {
            self.calls.set(self.calls.get() + 1);
            self.last_t_tai_ns.set(t_tai_ns);
            out[0..3].copy_from_slice(&state[3..6]);
            out[3..6].copy_from_slice(&self.a);
            Ok(())
        }

        fn describe(&self) -> av_cdm::pb::ModelInfo {
            av_cdm::pb::ModelInfo::default()
        }

        // Test-only closed-form model; never emits telemetry, so never produces a CDM measurement.
        fn last_measurements(&self) -> Vec<av_cdm::pb::Measurement> {
            Vec::new()
        }

        // Test-only closed-form model; no SENSOR fault runtime.
        fn drain_sensor_fault_effect(&self) -> Option<SensorFaultEffectDrain> {
            None
        }
    }

    #[test]
    fn default_step_matches_the_closed_form_solution_for_constant_acceleration() {
        let model = ConstantAccel { a: [0.0, 0.0, -9.8], calls: Cell::new(0), last_t_tai_ns: Cell::new(0) };
        let x0 = [0.0, 0.0, 100.0, 1.0, 2.0, 0.0];
        let t0_tai_ns: i64 = 1_700_000_000_000_000_000;
        let dt_ns: i64 = 10_000_000_000; // 10 s
        let dt_s = 10.0;

        let result = model.step(&x0, t0_tai_ns, &[], dt_ns).unwrap();

        assert_eq!(result.t_tai_ns, t0_tai_ns + dt_ns);
        assert!(result.outputs.is_empty());

        // Closed form: p = p0 + v0 t + 1/2 a t^2, v = v0 + a t.
        let expect = [
            x0[0] + x0[3] * dt_s,
            x0[1] + x0[4] * dt_s,
            x0[2] + x0[5] * dt_s + 0.5 * model.a[2] * dt_s * dt_s,
            x0[3],
            x0[4],
            x0[5] + model.a[2] * dt_s,
        ];
        for (got, want) in result.state.iter().zip(expect.iter()) {
            assert!((got - want).abs() < 1e-9, "{got} vs {want}");
        }
        assert!(model.calls.get() > 0, "derivatives was never called");
        // Every observed t stayed within [t0, t0+dt] once converted back to ns -- the
        // strongest sign that the relative-seconds-to-absolute-ns conversion is not
        // silently off by an offset or a unit.
        assert!(model.last_t_tai_ns.get() <= t0_tai_ns + dt_ns);
        assert!(model.last_t_tai_ns.get() >= t0_tai_ns);
    }

    #[test]
    fn derivatives_sees_absolute_tai_ns_not_a_step_relative_offset() {
        let model = ConstantAccel { a: [0.0, 0.0, 0.0], calls: Cell::new(0), last_t_tai_ns: Cell::new(0) };
        let x0 = [0.0; 6];
        let t0_tai_ns: i64 = 2_000_000_000_000_000_000;
        let _ = model.step(&x0, t0_tai_ns, &[], 1_000_000_000).unwrap();
        // The first evaluation is always at the step's start.
        assert!(model.last_t_tai_ns.get() >= t0_tai_ns, "saw {} < t0 {t0_tai_ns}", model.last_t_tai_ns.get());
    }

    #[test]
    fn settings_hash_is_stable_and_order_independent() {
        let mut a = BTreeMap::new();
        a.insert("b".to_string(), "2".to_string());
        a.insert("a".to_string(), "1".to_string());
        let mut b = BTreeMap::new();
        b.insert("a".to_string(), "1".to_string());
        b.insert("b".to_string(), "2".to_string());
        assert_eq!(settings_hash(&a), settings_hash(&b));
        assert_eq!(settings_hash(&a).len(), 64, "hex-encoded SHA-256 is 64 chars");

        let mut c = a.clone();
        c.insert("b".to_string(), "3".to_string());
        assert_ne!(settings_hash(&a), settings_hash(&c));
    }

    // -- Ports (docs/open-questions.md question 108) --------------------------------------

    #[test]
    fn encode_signal_and_decode_signal_round_trip() {
        for v in [0.0, -1.5, 1.0e300, f64::MIN_POSITIVE, -0.0] {
            let payload = encode_signal(v);
            assert_eq!(payload.len(), 8, "SIGNAL payload is a little-endian f64: exactly 8 bytes");
            let back = decode_signal(&payload).expect("8-byte payload decodes");
            assert_eq!(back.to_bits(), v.to_bits(), "round trip must be bit-exact, including -0.0");
        }
        assert_eq!(decode_signal(&[1, 2, 3]), None, "a payload that is not exactly 8 bytes does not decode");
    }

    #[test]
    fn the_default_step_with_ports_ignores_the_inbox_and_matches_plain_step_exactly() {
        let model = ConstantAccel { a: [0.0, 0.0, -9.8], calls: Cell::new(0), last_t_tai_ns: Cell::new(0) };
        let x0 = [0.0, 0.0, 100.0, 1.0, 2.0, 0.0];
        let t0_tai_ns: i64 = 1_700_000_000_000_000_000;
        let dt_ns: i64 = 10_000_000_000;

        let plain = model.step(&x0, t0_tai_ns, &[], dt_ns).unwrap();

        // A non-empty inbox is handed in, on a port this model has never heard of -- the
        // default implementation must still ignore it entirely.
        let inbox_messages = vec![PortMessage { port: "unused".to_string(), tai_ns: t0_tai_ns, payload: encode_signal(42.0) }];
        let inbox = Inbox::new(inbox_messages);

        let (ported, outbox, applied) = model.step_with_ports(&x0, t0_tai_ns, &[], dt_ns, &inbox).unwrap();

        assert_eq!(ported, plain, "the default step_with_ports must produce exactly what plain step does -- MODEL bindings see no behaviour change");
        assert!(outbox.is_empty(), "the default implementation never emits anything");
        assert!(applied.is_empty(), "the default implementation never applies a command from the inbox");
    }

    #[test]
    fn inbox_for_port_filters_by_name_and_preserves_order() {
        let messages = vec![
            PortMessage { port: "a".to_string(), tai_ns: 1, payload: vec![] },
            PortMessage { port: "b".to_string(), tai_ns: 2, payload: vec![] },
            PortMessage { port: "a".to_string(), tai_ns: 3, payload: vec![] },
        ];
        let inbox = Inbox::new(messages);
        let a: Vec<i64> = inbox.for_port("a").map(|m| m.tai_ns).collect();
        assert_eq!(a, vec![1, 3]);
        let c: Vec<i64> = inbox.for_port("c").map(|m| m.tai_ns).collect();
        assert!(c.is_empty());
    }

    // -- Sender attribution (question 130) --------------------------------------------------

    #[test]
    fn inbox_new_leaves_every_sender_unknown() {
        let inbox = Inbox::new(vec![PortMessage { port: "a".to_string(), tai_ns: 1, payload: vec![] }]);
        assert_eq!(inbox.last_on_port("a").unwrap().1, None, "a hand-built Inbox::new must not fabricate a sender");
    }

    #[test]
    fn inbox_new_with_senders_attaches_the_right_sender_to_each_message() {
        let messages = vec![
            PortMessage { port: "a".to_string(), tai_ns: 1, payload: vec![] },
            PortMessage { port: "a".to_string(), tai_ns: 2, payload: vec![] },
            PortMessage { port: "b".to_string(), tai_ns: 3, payload: vec![] },
        ];
        let senders = vec![Some("sender1".to_string()), Some("sender2".to_string()), None];
        let inbox = Inbox::new_with_senders(messages, senders);

        // "Last on port" must pick the LAST message on that port (question 108's own delivery
        // order), and its sender, not the first or some other one -- would fail against an
        // implementation that used `.find()` (first match) instead of `.rposition()`/`.rev()`.
        let (msg, sender) = inbox.last_on_port("a").expect("port a has two messages");
        assert_eq!(msg.tai_ns, 2, "last_on_port must select the LAST message on the port, not the first");
        assert_eq!(sender, Some("sender2"));

        let (_msg_b, sender_b) = inbox.last_on_port("b").expect("port b has one message");
        assert_eq!(sender_b, None, "a message with no known sender reports None, not a fabricated id");

        assert!(inbox.last_on_port("c").is_none(), "a port with no message at all reports None");
    }
}
