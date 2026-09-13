//! A3.1 (`docs/aiplane-plan.md`'s A3 milestone: "The service dispatches AUTHORIZED commands
//! into the kernel's existing telecommand path (M25.2b) through a binding, with `not_before`,
//! deadline to EXPIRED evaluated on the kernel clock, idempotency keys the binding never
//! dispatches twice, and ack levels mapped from the asset protocol"). This module is the kernel
//! side of that milestone: an [`ExternalCommandSource`] the executor polls once per run and
//! dispatches through `super::command`'s existing telecommand machinery -- never a second,
//! parallel command path (`super::command::command_out_packet_codec`, `.assign_sequence_numbers`,
//! `.dispatched_event`, `.acked_event` are reused unchanged; see `super::executor::run_shared_
//! group`'s own "command loop" for exactly where this module's [`CommandOutcome`]s are produced).
//!
//! ## Why one `poll`, not many
//!
//! `crate::drm::executor::run_shared_group` is a deterministic, non-real-time BATCH simulation:
//! the whole run's command disposition is decided analytically against the run's fixed
//! `[t0, run_end)` window, exactly the way a DRM-declared `command` `ScenarioEvent`'s own static
//! `tai_ns` is already resolved by that same "command loop," once, before the boundary loop ever
//! runs (`super::command`'s own module doc comment, "Where each transition actually happens").
//! There is no wall clock this executor "waits" on between two points in simulated time, so a
//! command's `not_before_tai_ns`/`deadline_tai_ns` fate is fully determined by three numbers
//! (`t0`, `run_end`, and the command's own two fields) with no need to re-poll as the run
//! "advances" -- [`decide_disposition`] is that one, pure, unit-tested decision.
//!
//! **`poll` is therefore called exactly once, at `run_shared_group`'s own `t0` (the scenario's
//! own `start_tai_ns`)** -- the identical instant the pre-existing "command loop" already
//! resolves every DRM-declared command's own dispatch, immediately before [`super::command::
//! assign_sequence_numbers`] runs (so an externally-sourced command and a DRM-declared one share
//! one CCSDS sequence-count space, assigned in one deterministic pass). The source's own [`
//! ExternalCommandSource::poll`] is asked, once, for every command it will ever want considered
//! for this run; **the returned order is authoritative** -- it is the order this loop evaluates
//! idempotency-key duplicates in (so a source's own deterministic ordering, not a re-sort by this
//! executor, is what "never dispatches the same key twice" means across a poll batch) and the
//! order [`ExternalCommandSource::report`] is called in. Two runs over a source with the same
//! contents therefore produce byte-identical `RunProducts` (`crates/av-kernel/tests/
//! external_command_source.rs`'s own determinism test, mirroring `restart_invariance.rs`/
//! `faults_determinism.rs`'s existing methodology): nothing here reads a wall clock, a random id,
//! or iterates a `HashMap`.
//!
//! ## The boundary convention (`docs/aiplane-plan.md`'s "the boundary nanosecond")
//!
//! Identical to the token- and delegation-expiry convention in `crates/av-command/src/oidc.rs`
//! and `crates/av-command/src/authz.rs` (a different crate, named by path rather than by a
//! `crate::` item link, because `av-kernel` does not depend on it):
//! `now_tai_ns >= expiry_tai_ns` (`now_tai_ns == expiry_tai_ns` is refused; one
//! nanosecond earlier is not) -- see [`decide_disposition`]'s own doc comment for the exact
//! arithmetic and `crates/av-kernel/tests/external_command_source.rs`'s own boundary-nanosecond
//! test, both sides.
//!
//! ## D6: idempotency, in the kernel too
//!
//! `crates/av-command`'s own service already refuses a duplicate `idempotency_key` -- this
//! module refuses one too, in the kernel, as defence in depth: `command.proto`'s own contract
//! ("the edge never dispatches the same key twice") is unconditional, not "unconditional except
//! when the kernel is asked directly." [`DuplicateIdempotencyKeyTracker`] is the one place that
//! rule lives; an **empty** `idempotency_key` is never treated as a duplicate of another empty
//! one (a source that declines to set one at all gets no de-duplication, which is the honest
//! reading of "a key," not "the empty string is itself a valid key shared by everything").
//!
//! ## Every refusal is reported, never a silent drop
//!
//! [`CommandOutcome`] is deliberately exhaustive over every way a command can fail to reach
//! `ACKED`: [`CommandOutcome::Expired`], [`CommandOutcome::NotDispatchedRunEnded`], [`
//! CommandOutcome::DuplicateIdempotencyKey`] and [`CommandOutcome::Refused`] (structural: target
//! not a framed consumer, unknown sender, unknown target, malformed payload) are all reported
//! through [`ExternalCommandSource::report`] -- `super::executor::run_shared_group`'s own
//! "command loop" calls `report` on every one of these paths, never merely `continue`s past one
//! the way the pre-existing DRM-declared-command path is allowed to (a DRM-declared command's own
//! structural problems are a *fixture-authoring* bug, refused at load with a hard [`super::
//! DrmError`] that aborts the whole run; an externally-sourced command's problems are a *runtime*
//! disposition of one command among many, individually reported and counted, never aborting the
//! run the way a `DrmError` would).

use std::collections::BTreeSet;

use av_cdm::pb::{AckLevel, Command};

/// Every command this source wants considered, and the kernel's verdict on each one it saw --
/// see the module doc comment for the full contract.
pub trait ExternalCommandSource: Send + Sync {
    /// Every command this source wants considered for dispatch, in a deterministic order the
    /// source itself fixes -- called exactly once per run, at the run's own scenario-start
    /// epoch (`now_tai_ns`) -- see the module doc comment's "Why one `poll`, not many" section
    /// for exactly why one call, at that one epoch, is the correct and sufficient contract for
    /// this executor's own batch-simulation architecture.
    fn poll(&self, now_tai_ns: i64) -> Vec<Command>;
    /// The kernel's verdict on one command. Called exactly once per command per outcome (a
    /// command that is refused is never *also* reported dispatched, and vice versa) -- see
    /// [`CommandOutcome`]'s own doc comment for the full, exhaustive set of verdicts this can
    /// carry.
    fn report(&self, outcome: CommandOutcome);
}

/// Why a command was refused before it ever became a real CCSDS frame -- named explicitly
/// (question 149's "never a silent drop" rule) rather than folded into one opaque `String`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefusalReason {
    /// `Command.entity_id` names a real `SosConfiguration` instance, but that instance's own
    /// resolved [`super::binding::BindingPlan`] declares no FRAMED-consume command port at all
    /// -- mirrors `super::DrmError::CommandTargetNotFramedConsumer`'s own DRM-declared-command
    /// refusal, one layer over (this path never returns a `DrmError`; see the module doc
    /// comment's "Every refusal is reported" section for why).
    TargetNotFramedConsumer { instance: String },
    /// `Command.entity_id` does not name any instance in this run's own `SosConfiguration`.
    UnknownTarget { instance: String },
    /// `Command.provenance.attributes["from"]` (the dispatching ground/edge instance, the
    /// identical convention `super::command::ParsedCommand::from`'s own doc comment already
    /// uses for a DRM-declared command's `attributes["from"]`) is missing, empty, or does not
    /// name a real instance in this run's own `SosConfiguration`.
    UnknownSender { sender: String },
    /// `Command.payload` is empty, or not a packed `google.protobuf.DoubleValue` -- see
    /// [`unpack_double_value`]'s own doc comment for exactly what this executor expects there
    /// and why (D2's own "prefer needing no shared-proto change at all" instruction).
    MalformedPayload { detail: String },
}

impl std::fmt::Display for RefusalReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RefusalReason::TargetNotFramedConsumer { instance } => write!(f, "target instance {instance:?} does not classify to a binding with a FRAMED-consume command port declared"),
            RefusalReason::UnknownTarget { instance } => write!(f, "no instance named {instance:?} in this run's SosConfiguration"),
            RefusalReason::UnknownSender { sender } => write!(f, "provenance.attributes[\"from\"]={sender:?} does not name a real instance in this run's SosConfiguration"),
            RefusalReason::MalformedPayload { detail } => write!(f, "malformed command payload: {detail}"),
        }
    }
}

/// Every verdict [`ExternalCommandSource::report`] can carry -- exhaustive over "reached ACKED,"
/// every real ack level along the way, and every way a command can instead never reach the
/// wire at all (question 149's "never a silent drop" rule; `docs/aiplane-plan.md`'s own "a
/// failure that leaves no trace," the single defect round 1's review found six times). `id` is
/// always `Command.id`, so a caller can correlate a batch of [`ExternalCommandSource::poll`]'s
/// own results against the reports that follow.
#[derive(Debug, Clone, PartialEq)]
pub enum CommandOutcome {
    /// The kernel accepted the command and the encoded CCSDS frame was handed to
    /// `crate::router::Router::deliver` -- `ACK_LEVEL_EDGE`'s own real-wire moment (D3):
    /// "the kernel accepted the command and the encoded CCSDS frame was handed to the router."
    Dispatched { id: String, epoch_tai_ns: i64, seq: u16 },
    /// One of the three real [`AckLevel`]s was reached, at `epoch_tai_ns` -- `ACK_LEVEL_EDGE` is
    /// reported at the same epoch as [`CommandOutcome::Dispatched`] (D3: dispatch to the router
    /// IS the edge ack); `ACK_LEVEL_ASSET_RECEIVED`/`ACK_LEVEL_ASSET_EXECUTED` are reported once
    /// the target's own `consume_framed` path actually decodes/applies the frame -- see `super::
    /// executor::run_shared_group`'s own "command loop"/applied-commands-drain call sites.
    Acked { id: String, level: AckLevel, epoch_tai_ns: i64 },
    /// The command's own `deadline_tai_ns` passed (D5's boundary convention, [`decide_
    /// disposition`]) before it was ever dispatched -- `kernel_epoch_tai_ns` is the exact kernel
    /// epoch [`decide_disposition`] evaluated the expiry at (`max(t0, not_before_tai_ns)`, the
    /// earliest epoch the kernel would otherwise have attempted dispatch). **Never dispatched**:
    /// no CCSDS frame for this command ever reaches `crate::router::Router::deliver`.
    Expired { id: String, deadline_tai_ns: i64, kernel_epoch_tai_ns: i64 },
    /// The command's own `not_before_tai_ns` falls at or after this run's own `end_tai_ns` -- it
    /// would never have been reached within the executed run at all (not a deadline expiry: no
    /// `deadline_tai_ns` was even declared, or it had not yet passed -- simply nothing left of
    /// the run to dispatch it into). Never dispatched, exactly like [`CommandOutcome::Expired`].
    NotDispatchedRunEnded { id: String, not_before_tai_ns: i64, run_end_tai_ns: i64 },
    /// `idempotency_key` (non-empty) had already been dispatched earlier in this same poll batch
    /// -- refused, reported, counted, **never dispatched a second time** (D6). An empty
    /// `idempotency_key` is never reported this way (see [`DuplicateIdempotencyKeyTracker`]'s
    /// own doc comment).
    DuplicateIdempotencyKey { id: String, idempotency_key: String },
    /// A structural problem this command's own declared fields already carry, refused before
    /// any router call -- see [`RefusalReason`]'s own doc comment for the exhaustive list.
    Refused { id: String, reason: RefusalReason },
}

// ================================================================================================
// D5: not_before / deadline, decided once, analytically -- see the module doc comment.
// ================================================================================================

/// [`decide_disposition`]'s own return value -- see that function's own doc comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// Dispatch now, at `epoch_tai_ns` (`max(t0_tai_ns, not_before_tai_ns)`, clamped to have
    /// already been checked `< run_end_tai_ns` and, when a deadline was declared, `<
    /// deadline_tai_ns`).
    Dispatch { epoch_tai_ns: i64 },
    /// Refuse: the deadline had already passed (D5's boundary convention: `>=`, never `>`) at
    /// the earliest epoch this command could have been dispatched.
    Expired { deadline_tai_ns: i64, kernel_epoch_tai_ns: i64 },
    /// Refuse: no deadline (or a deadline not yet passed) made this command EXPIRED, but its own
    /// `not_before_tai_ns` falls at or after the run's own end -- there is no instant left in the
    /// executed run to dispatch it at.
    NotDispatchedRunEnded { not_before_tai_ns: i64, run_end_tai_ns: i64 },
}

/// The one place D5's `not_before`/`deadline` decision is made, over the run's fixed
/// `[t0_tai_ns, run_end_tai_ns)` window -- see the module doc comment's "Why one `poll`, not
/// many" and "The boundary convention" sections for exactly why a single analytic decision,
/// using the identical `now_tai_ns >= expiry_tai_ns` convention `crates/av-command/src/oidc.rs`/
/// `crates/av-command/src/authz.rs` already use for token/delegation expiry, is correct here.
///
/// `not_before_tai_ns == 0` is `command.proto`'s own "immediately" convention -- folded into
/// `t0_tai_ns` by the `max` below (a real TAI epoch is always `> 0`, so `0` can never win that
/// `max`). `deadline_tai_ns == 0` is this task's own declared convention for the deadline's
/// analogous "field not set" case: **no deadline at all**, mirrored from `not_before_tai_ns`'s
/// own "0 = immediately"/"0 = no constraint" reading of an unset `int64` (`command.proto`'s
/// `Command.deadline_tai_ns` doc comment: "Deadline after which the command expires
/// undispatched" -- a command with no declared deadline cannot expire).
pub fn decide_disposition(not_before_tai_ns: i64, deadline_tai_ns: i64, t0_tai_ns: i64, run_end_tai_ns: i64) -> Disposition {
    let effective_epoch = if not_before_tai_ns > t0_tai_ns { not_before_tai_ns } else { t0_tai_ns };
    if deadline_tai_ns != 0 && effective_epoch >= deadline_tai_ns {
        return Disposition::Expired { deadline_tai_ns, kernel_epoch_tai_ns: effective_epoch };
    }
    if effective_epoch >= run_end_tai_ns {
        return Disposition::NotDispatchedRunEnded { not_before_tai_ns: effective_epoch, run_end_tai_ns };
    }
    Disposition::Dispatch { epoch_tai_ns: effective_epoch }
}

// ================================================================================================
// D6: idempotency, in the kernel too.
// ================================================================================================

/// The one place D6's "never dispatches the same key twice" rule is enforced, kernel-side, for
/// commands drawn from one [`ExternalCommandSource::poll`] batch -- a plain `BTreeSet<String>`
/// (deterministic iteration is never relied on here; only membership is), scoped to one run (a
/// fresh tracker per `super::executor::execute` call -- this task's own scope is "the binding
/// never dispatches twice within one run it drives," `crates/av-command`'s own ledger-backed,
/// restart-surviving guarantee being the belt this is the suspenders for, not a duplicate of it).
#[derive(Debug, Default)]
pub struct DuplicateIdempotencyKeyTracker {
    seen: BTreeSet<String>,
}

impl DuplicateIdempotencyKeyTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// `true` iff `key` is a non-empty idempotency key already seen by an earlier call in this
    /// same tracker's lifetime (and, as a side effect, `key` is now recorded seen). **An empty
    /// key is never a duplicate of another empty key** -- decided, documented, tested (D6): a
    /// source that declines to set `idempotency_key` at all gets no de-duplication for that
    /// command, rather than every un-keyed command silently colliding with every other one.
    pub fn is_duplicate(&mut self, key: &str) -> bool {
        if key.is_empty() {
            return false;
        }
        !self.seen.insert(key.to_string())
    }
}

// ================================================================================================
// D2's "prefer needing no shared-proto change at all": `Command.payload` carries the one
// commanded numeric value as a packed `google.protobuf.DoubleValue` -- a well-known protobuf
// type already reachable through `prost-types` (`av_cdm`'s own `Cargo.toml` dependency), so
// packing/unpacking it needs no change to any `.proto` file at all.
// ================================================================================================

/// The `google.protobuf.Any.type_url` this module packs/expects a commanded numeric value under
/// -- the standard, well-known-type URL every protobuf implementation recognizes for `google.
/// protobuf.DoubleValue`, never a project-specific string.
pub const DOUBLE_VALUE_TYPE_URL: &str = "type.googleapis.com/google.protobuf.DoubleValue";

/// `google.protobuf.DoubleValue`'s own wire encoding is one line of the protobuf wire format
/// itself, not something that needs the generated Rust type: field 1, wire type 1 (64-bit) --
/// tag byte `0x09`, then the `double`'s own 8 bytes, **little-endian** (protobuf's `fixed64`/
/// `double` wire representation, regardless of host byte order). Handwritten here rather than
/// depending on `prost-types`' generated wrapper types (which this workspace's pinned
/// `prost-types` version does not export at its crate root) -- this is still a byte-for-byte
/// real, standard `google.protobuf.DoubleValue` payload, decodable by any protobuf
/// implementation, not a project-specific shape.
const DOUBLE_VALUE_WIRE_LEN: usize = 9;
const DOUBLE_VALUE_TAG: u8 = 0x09;

/// Pack `value` as `Command.payload` -- the counterpart [`unpack_double_value`] expects. Used by
/// [`ExternalCommandSource`] implementations (including [`RecordingCommandSource`], below) to
/// build a `Command` this executor's own "command loop" can dispatch; no shared `.proto` file
/// needed a change for this (D2's own instruction, `docs/aiplane-plan.md`'s A3 status section).
pub fn pack_double_value(value: f64) -> prost_types::Any {
    let mut wire = Vec::with_capacity(DOUBLE_VALUE_WIRE_LEN);
    wire.push(DOUBLE_VALUE_TAG);
    wire.extend_from_slice(&value.to_le_bytes());
    prost_types::Any { type_url: DOUBLE_VALUE_TYPE_URL.to_string(), value: wire }
}

/// The inverse of [`pack_double_value`] -- [`RefusalReason::MalformedPayload`]'s own typed
/// refusal (never a panic, never a default-to-zero) for every way `payload` can fail to be
/// exactly that: absent, wrongly-typed (`type_url` mismatch), or bytes that do not decode as
/// `google.protobuf.DoubleValue` at all.
pub fn unpack_double_value(payload: &Option<prost_types::Any>) -> Result<f64, String> {
    let any = payload.as_ref().ok_or_else(|| "Command.payload is empty; expected a packed google.protobuf.DoubleValue".to_string())?;
    if any.type_url != DOUBLE_VALUE_TYPE_URL {
        return Err(format!("Command.payload.type_url={:?}, expected {DOUBLE_VALUE_TYPE_URL:?}", any.type_url));
    }
    if any.value.len() != DOUBLE_VALUE_WIRE_LEN || any.value[0] != DOUBLE_VALUE_TAG {
        return Err(format!("Command.payload did not decode as google.protobuf.DoubleValue: expected a {DOUBLE_VALUE_WIRE_LEN}-byte field-1/fixed64 encoding (tag {DOUBLE_VALUE_TAG:#x}), got {} byte(s)", any.value.len()));
    }
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&any.value[1..DOUBLE_VALUE_WIRE_LEN]);
    Ok(f64::from_le_bytes(bytes))
}

// ================================================================================================
// RecordingCommandSource: a real, directly usable ExternalCommandSource -- not a mock, not
// `#[cfg(test)]` (integration tests in this crate's own `tests/` directory are a separate crate
// and need a real, public type to construct against, exactly like `crate::drm::replay::
// ReplayConfig` is real, public, and only ever built by tests and `av-run`).
// ================================================================================================

/// A fixed, deterministic queue of commands, handed back unchanged on every [`Self::poll`] call
/// (this executor only ever calls it once per run -- see the module doc comment -- but repeating
/// the same queue on a hypothetical second call keeps this type honestly reusable rather than a
/// single-shot trap), plus every [`CommandOutcome`] the kernel ever reported, recorded in receipt
/// order for a test to assert against.
#[derive(Debug)]
pub struct RecordingCommandSource {
    queue: Vec<Command>,
    outcomes: std::sync::Mutex<Vec<CommandOutcome>>,
}

impl RecordingCommandSource {
    pub fn new(commands: Vec<Command>) -> Self {
        Self { queue: commands, outcomes: std::sync::Mutex::new(Vec::new()) }
    }

    /// Every [`CommandOutcome`] reported so far, in the exact order [`ExternalCommandSource::
    /// report`] received them.
    pub fn outcomes(&self) -> Vec<CommandOutcome> {
        self.outcomes.lock().expect("RecordingCommandSource's own Mutex is never held across a panic in this crate's single-threaded executor").clone()
    }
}

impl ExternalCommandSource for RecordingCommandSource {
    fn poll(&self, _now_tai_ns: i64) -> Vec<Command> {
        self.queue.clone()
    }
    fn report(&self, outcome: CommandOutcome) {
        self.outcomes.lock().expect("see Self::outcomes' own doc comment").push(outcome);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: i64 = 1_700_000_000_000_000_000;
    const RUN_END: i64 = 1_700_000_300_000_000_000;

    // -- decide_disposition: not_before / deadline, the boundary nanosecond both sides --------

    #[test]
    fn immediate_dispatch_when_neither_not_before_nor_deadline_constrain_it() {
        assert_eq!(decide_disposition(0, 0, T0, RUN_END), Disposition::Dispatch { epoch_tai_ns: T0 });
    }

    #[test]
    fn not_before_in_the_future_holds_dispatch_until_that_epoch() {
        let nb = T0 + 50_000_000_000;
        assert_eq!(decide_disposition(nb, 0, T0, RUN_END), Disposition::Dispatch { epoch_tai_ns: nb });
    }

    /// D5's boundary nanosecond: a deadline exactly at the would-be dispatch epoch is EXPIRED
    /// (`>=`, never `>`) -- one nanosecond later is not.
    #[test]
    fn deadline_exactly_at_the_dispatch_epoch_is_expired_one_nanosecond_later_is_not() {
        let nb = T0 + 10;
        assert_eq!(decide_disposition(nb, nb, T0, RUN_END), Disposition::Expired { deadline_tai_ns: nb, kernel_epoch_tai_ns: nb }, "deadline == dispatch epoch must be refused, not dispatched");
        assert_eq!(decide_disposition(nb, nb + 1, T0, RUN_END), Disposition::Dispatch { epoch_tai_ns: nb }, "one nanosecond after the dispatch epoch must not be refused");
    }

    /// The other side of the boundary: a deadline one nanosecond BEFORE the dispatch epoch is
    /// also EXPIRED (already passed by the time not_before would allow dispatch).
    #[test]
    fn deadline_one_nanosecond_before_the_dispatch_epoch_is_expired() {
        let nb = T0 + 10;
        assert_eq!(decide_disposition(nb, nb - 1, T0, RUN_END), Disposition::Expired { deadline_tai_ns: nb - 1, kernel_epoch_tai_ns: nb });
    }

    #[test]
    fn deadline_zero_means_no_deadline_at_all() {
        assert_eq!(decide_disposition(T0 + 10, 0, T0, RUN_END), Disposition::Dispatch { epoch_tai_ns: T0 + 10 });
    }

    #[test]
    fn not_before_at_or_after_run_end_never_dispatches_and_is_not_confused_with_expired() {
        assert_eq!(decide_disposition(RUN_END, 0, T0, RUN_END), Disposition::NotDispatchedRunEnded { not_before_tai_ns: RUN_END, run_end_tai_ns: RUN_END });
        assert_eq!(decide_disposition(RUN_END + 1, 0, T0, RUN_END), Disposition::NotDispatchedRunEnded { not_before_tai_ns: RUN_END + 1, run_end_tai_ns: RUN_END });
    }

    #[test]
    fn a_deadline_before_run_end_takes_priority_over_the_run_end_check() {
        // Both conditions would fire (not_before is at run_end AND a deadline before it has
        // already passed) -- Expired must win, since the deadline is the more specific, more
        // informative refusal reason.
        assert_eq!(decide_disposition(RUN_END, RUN_END - 1, T0, RUN_END), Disposition::Expired { deadline_tai_ns: RUN_END - 1, kernel_epoch_tai_ns: RUN_END });
    }

    // -- DuplicateIdempotencyKeyTracker --------------------------------------------------------

    #[test]
    fn a_repeated_nonempty_key_is_a_duplicate_the_second_time() {
        let mut t = DuplicateIdempotencyKeyTracker::new();
        assert!(!t.is_duplicate("k1"), "first sighting must not be a duplicate");
        assert!(t.is_duplicate("k1"), "second sighting of the same non-empty key must be a duplicate");
        assert!(!t.is_duplicate("k2"), "a different key must not collide with k1");
    }

    /// D6: an empty idempotency key is never a duplicate of another empty one.
    #[test]
    fn two_empty_idempotency_keys_are_never_duplicates_of_each_other() {
        let mut t = DuplicateIdempotencyKeyTracker::new();
        assert!(!t.is_duplicate(""));
        assert!(!t.is_duplicate(""));
        assert!(!t.is_duplicate(""));
    }

    // -- pack_double_value / unpack_double_value ------------------------------------------------

    #[test]
    fn pack_and_unpack_double_value_round_trips_exactly() {
        let packed = pack_double_value(0.0);
        assert_eq!(unpack_double_value(&Some(packed)), Ok(0.0));
        let packed = pack_double_value(-3.5);
        assert_eq!(unpack_double_value(&Some(packed)), Ok(-3.5));
    }

    #[test]
    fn unpack_double_value_refuses_an_absent_payload() {
        assert!(unpack_double_value(&None).is_err());
    }

    #[test]
    fn unpack_double_value_refuses_a_mismatched_type_url() {
        let any = prost_types::Any { type_url: "type.googleapis.com/google.protobuf.StringValue".to_string(), value: vec![] };
        let err = unpack_double_value(&Some(any)).unwrap_err();
        assert!(err.contains("type_url"), "{err}");
    }

    // -- RecordingCommandSource: a real ExternalCommandSource, not a mock ---------------------

    #[test]
    fn recording_command_source_replays_its_fixed_queue_and_records_reports_in_order() {
        let cmd = Command { id: "c1".to_string(), ..Default::default() };
        let source = RecordingCommandSource::new(vec![cmd.clone()]);
        assert_eq!(source.poll(0), vec![cmd.clone()]);
        assert_eq!(source.poll(999), vec![cmd], "poll must hand back the identical fixed queue every call");
        source.report(CommandOutcome::Dispatched { id: "c1".to_string(), epoch_tai_ns: 1, seq: 0 });
        source.report(CommandOutcome::Acked { id: "c1".to_string(), level: AckLevel::Edge, epoch_tai_ns: 1 });
        assert_eq!(source.outcomes().len(), 2, "must record every report, in order");
        assert_eq!(source.outcomes()[0], CommandOutcome::Dispatched { id: "c1".to_string(), epoch_tai_ns: 1, seq: 0 });
    }
}
