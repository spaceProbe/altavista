//! The command authority state machine, as a library (`docs/aiplane-plan.md` milestone A1;
//! ADR-004's "Command authority" section). One transition function per edge, named for the
//! edge (`propose`, `check`, `authorize`, `dispatch`, `ack`, `reject`, `expire`, `fail`);
//! every illegal edge is a typed [`CommandError`], never a panic and never a silently
//! ignored no-op.
//!
//! **No invented states.** Every state named anywhere in this module is a real
//! `altavista.v1.CommandState` variant (`command.proto`, generated as [`av_cdm::pb::
//! CommandState`]) -- there is no "APPROVED"/"EXECUTED" state, matching
//! `crates/av-kernel/src/drm/command.rs`'s own module doc comment on the same rule.
//! [`av_cdm::pb::AckLevel`] stays a separate axis carried only on the `ACKED` transition's
//! own `CommandTransition.ack_level`, never folded into `CommandState` itself.
//!
//! # The edge set
//!
//! `command.proto`'s header diagram is ASCII art:
//!
//! ```text
//! PROPOSED -> CHECKED -> AUTHORIZED -> DISPATCHED -> ACKED
//!               \-> REJECTED   \-> EXPIRED      \-> FAILED
//! ```
//!
//! Read as pure character alignment, the three branches hang off `CHECKED` (`REJECTED`),
//! `AUTHORIZED` (`EXPIRED`) and `DISPATCHED` (`FAILED`) only -- seven edges, not nine: it
//! does not, by itself, depict `PROPOSED -> REJECTED` or `DISPATCHED -> EXPIRED`. This
//! module implements the nine-edge set the task brief specifies (below), which is a
//! defensible superset for real operational reasons the task brief's own milestone text
//! supports -- a proposal can fail structural validation and be rejected before a policy
//! check ever runs (`PROPOSED -> REJECTED`; `crates/av-kernel/src/drm/command.rs`'s own
//! `propose_check_authorize` already treats "structurally valid" as a precondition *of*
//! `CHECKED`, implying an invalid one never reaches it), and a dispatched command can still
//! miss a broader deadline before it is ever acked (`DISPATCHED -> EXPIRED`; `docs/aiplane-
//! plan.md` A3's "deadline to EXPIRED evaluated on the kernel clock" does not itself say
//! deadline enforcement stops the instant a command is handed to the transport). This
//! discrepancy between the ASCII diagram's literal alignment and the task brief's explicit
//! nine-edge list is reported in this task's own final message, per the brief's own request
//! to say so rather than silently pick one reading.
//!
//! Implemented edges: `PROPOSED -> CHECKED`, `PROPOSED -> REJECTED`, `CHECKED ->
//! AUTHORIZED`, `CHECKED -> REJECTED`, `AUTHORIZED -> DISPATCHED`, `AUTHORIZED -> EXPIRED`,
//! `DISPATCHED -> ACKED`, `DISPATCHED -> FAILED`, `DISPATCHED -> EXPIRED`, and (A3.2, D2)
//! `ACKED -> ACKED`. [`reject`] and [`expire`] each cover two legal source states; [`ack`]
//! now covers two as well (`DISPATCHED` and `ACKED`); every other edge function covers one.
//!
//! ## A3.2/D2: the tenth edge, `ACKED -> ACKED`
//!
//! `command.proto`'s `CommandTransition.ack_level` exists so an asset that acks a command at
//! more than one [`AckLevel`] (edge received, asset received, asset executed --
//! `docs/aiplane-plan.md` A3) produces more than one transition -- if a second, third ack
//! could only ever be silently dropped once `state` was already `ACKED`, that field would be
//! pointless: nothing downstream (the ledger, a replay, the console) could ever see the
//! asset's later, more-executed acks at all. [`ack`] therefore accepts `ACKED` as a second
//! legal source state, **but only when the newly reported [`AckLevel`] is strictly greater
//! than the previous transition's own `ack_level`** (`ACK_LEVEL_UNSPECIFIED` (0) < `_EDGE` (1)
//! < `_ASSET_RECEIVED` (2) < `_ASSET_EXECUTED` (3), the enum's own declared ordinal order --
//! `command.proto` declares no other ordering, and this is the only one that matches "ack
//! levels arrive in increasing order of how far the command actually got"). A non-increasing
//! (equal or lower) level is refused as [`CommandError::AckLevelNotIncreasing`] -- a typed
//! refusal, structurally legal (the `ACKED -> ACKED` edge itself exists) but rejected on the
//! *value* being re-asserted, never silently ignored and never folded into
//! [`CommandError::IllegalTransition`] (that variant means "no edge exists from this state to
//! this one at all," which is false here: the edge exists, this one instance of it is what is
//! refused).
//!
//! [`propose`] is not one of the nine: it is the machine's entry point, building a fresh
//! `Command` (`CommandState::Unspecified` -> `CommandState::Proposed`) rather than advancing
//! an already-started one, and carries its own two refusals: a non-`Unspecified`/non-empty
//! input (see [`CommandError::AlreadyStarted`]) and, per `docs/open-questions.md` question
//! 53 ("propose-only until an envelope policy exists"), a non-empty `envelope_id` (see
//! [`CommandError::EnvelopeNotAllowed`]) -- enforced here in code, not only in the Rego
//! policy A1.2 adds at `CHECKED`.

use av_cdm::pb::{AckLevel, Command, CommandState, CommandTransition};
use thiserror::Error;

use crate::clock::Clock;

/// Every refusal this module can produce. Every illegal edge attempt produces
/// [`CommandError::IllegalTransition`]; the two `propose`-specific refusals are their own
/// variants (see the module doc).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CommandError {
    /// Attempted `to` from a `from` state that has no such edge.
    #[error("illegal command transition: {from:?} has no edge to {to:?}")]
    IllegalTransition { from: CommandState, to: CommandState },
    /// `propose` was called on a `Command` that already has a state or a recorded
    /// transition -- `propose` only ever starts a fresh command.
    #[error("propose called on a command already at {state:?}, not Unspecified")]
    AlreadyStarted { state: CommandState },
    /// `propose` was called with a non-empty `envelope_id` (question 53: propose-only,
    /// enforced in code as well as in policy).
    #[error("propose refuses a non-empty envelope_id {envelope_id:?}: propose-only stands (question 53), no envelope is enabled by this track")]
    EnvelopeNotAllowed { envelope_id: String },
    /// A3.2/D2: `ack` was called on a `Command` already `ACKED`, with an `ack_level` that is
    /// not strictly greater than the previous transition's own `ack_level` -- see the module
    /// doc's "A3.2/D2: the tenth edge" section. The edge `ACKED -> ACKED` itself is legal;
    /// this specific `(previous, requested)` pair is refused.
    #[error("ack refuses a non-increasing ack_level: previous transition already recorded {previous:?}, this call requested {requested:?} (must be strictly greater)")]
    AckLevelNotIncreasing { previous: AckLevel, requested: AckLevel },
}

/// The `CommandState` a `Command`'s `state` field currently encodes, defaulting to
/// `Unspecified` for a value outside the enum's range (a malformed/foreign `i32` is treated
/// as "no state", never as a panic).
pub fn current_state(command: &Command) -> CommandState {
    CommandState::try_from(command.state).unwrap_or(CommandState::Unspecified)
}

fn require_one_of(command: &Command, legal_from: &[CommandState], to: CommandState) -> Result<(), CommandError> {
    let from = current_state(command);
    if legal_from.contains(&from) {
        Ok(())
    } else {
        Err(CommandError::IllegalTransition { from, to })
    }
}

/// Appends one `CommandTransition` for `to` (epoch from `clock`) and sets `command.state`.
/// Private: every public edge function above calls this only after `require_one_of` has
/// already accepted the edge, so this never itself refuses anything.
fn push_transition(
    mut command: Command,
    to: CommandState,
    principal: &str,
    reason: &str,
    ack_level: AckLevel,
    delegation_id: &str,
    clock: &dyn Clock,
) -> Command {
    command.transitions.push(CommandTransition {
        state: to as i32,
        tai_ns: clock.now_tai_ns(),
        principal: principal.to_string(),
        reason: reason.to_string(),
        ack_level: ack_level as i32,
        delegation_id: delegation_id.to_string(),
    });
    command.state = to as i32;
    command
}

/// The machine's entry point: builds `CommandState::Proposed` from a fresh `Command` (the
/// only message a model or agent may emit toward the command path, `command.proto`'s
/// `CommandProposal.command`). See the module doc for both refusals.
pub fn propose(command: Command, principal: &str, reason: &str, clock: &dyn Clock) -> Result<Command, CommandError> {
    if !command.envelope_id.is_empty() {
        return Err(CommandError::EnvelopeNotAllowed { envelope_id: command.envelope_id.clone() });
    }
    let from = current_state(&command);
    if from != CommandState::Unspecified || !command.transitions.is_empty() {
        return Err(CommandError::AlreadyStarted { state: from });
    }
    Ok(push_transition(command, CommandState::Proposed, principal, reason, AckLevel::Unspecified, "", clock))
}

/// `PROPOSED -> CHECKED`: the Rego policy evaluator (A1.2) calls this once it has decided,
/// regardless of the decision -- `check` itself carries no allow/deny logic, only the state
/// edge; a refused policy decision is recorded by [`reject`], not by refusing to call
/// `check` at all (that distinction belongs to A1.2's own caller, not this module).
pub fn check(command: Command, principal: &str, reason: &str, clock: &dyn Clock) -> Result<Command, CommandError> {
    require_one_of(&command, &[CommandState::Proposed], CommandState::Checked)?;
    Ok(push_transition(command, CommandState::Checked, principal, reason, AckLevel::Unspecified, "", clock))
}

/// `CHECKED -> AUTHORIZED`: the role-gated human authorization edge (A2 wires the role/MFA
/// check; this signature already carries `delegation_id` for A2's `CommandTransition.
/// delegation_id`, empty when the principal acted without a delegation).
pub fn authorize(
    command: Command,
    principal: &str,
    reason: &str,
    delegation_id: &str,
    clock: &dyn Clock,
) -> Result<Command, CommandError> {
    require_one_of(&command, &[CommandState::Checked], CommandState::Authorized)?;
    Ok(push_transition(command, CommandState::Authorized, principal, reason, AckLevel::Unspecified, delegation_id, clock))
}

/// `AUTHORIZED -> DISPATCHED`: handed to the simulated asset's transport (A3).
pub fn dispatch(command: Command, principal: &str, reason: &str, clock: &dyn Clock) -> Result<Command, CommandError> {
    require_one_of(&command, &[CommandState::Authorized], CommandState::Dispatched)?;
    Ok(push_transition(command, CommandState::Dispatched, principal, reason, AckLevel::Unspecified, "", clock))
}

/// `DISPATCHED -> ACKED`, or `ACKED -> ACKED` (A3.2/D2) when the newly reported `ack_level`
/// strictly exceeds the previous transition's own `ack_level` -- see the module doc's
/// "A3.2/D2: the tenth edge" section for the full rationale and the enum ordinal order this
/// compares by. `ack_level` is the separate axis the module doc describes, never folded into
/// `CommandState`.
pub fn ack(command: Command, principal: &str, reason: &str, ack_level: AckLevel, clock: &dyn Clock) -> Result<Command, CommandError> {
    require_one_of(&command, &[CommandState::Dispatched, CommandState::Acked], CommandState::Acked)?;
    if current_state(&command) == CommandState::Acked {
        let previous = command
            .transitions
            .last()
            .map(|t| AckLevel::try_from(t.ack_level).unwrap_or(AckLevel::Unspecified))
            .unwrap_or(AckLevel::Unspecified);
        if ack_level as i32 <= previous as i32 {
            return Err(CommandError::AckLevelNotIncreasing { previous, requested: ack_level });
        }
    }
    Ok(push_transition(command, CommandState::Acked, principal, reason, ack_level, "", clock))
}

/// `PROPOSED -> REJECTED` or `CHECKED -> REJECTED` -- see the module doc for why both source
/// states are legal for this one edge function.
pub fn reject(command: Command, principal: &str, reason: &str, clock: &dyn Clock) -> Result<Command, CommandError> {
    require_one_of(&command, &[CommandState::Proposed, CommandState::Checked], CommandState::Rejected)?;
    Ok(push_transition(command, CommandState::Rejected, principal, reason, AckLevel::Unspecified, "", clock))
}

/// `AUTHORIZED -> EXPIRED` or `DISPATCHED -> EXPIRED` -- see the module doc for why both
/// source states are legal for this one edge function.
pub fn expire(command: Command, principal: &str, reason: &str, clock: &dyn Clock) -> Result<Command, CommandError> {
    require_one_of(&command, &[CommandState::Authorized, CommandState::Dispatched], CommandState::Expired)?;
    Ok(push_transition(command, CommandState::Expired, principal, reason, AckLevel::Unspecified, "", clock))
}

/// `DISPATCHED -> FAILED`.
pub fn fail(command: Command, principal: &str, reason: &str, clock: &dyn Clock) -> Result<Command, CommandError> {
    require_one_of(&command, &[CommandState::Dispatched], CommandState::Failed)?;
    Ok(push_transition(command, CommandState::Failed, principal, reason, AckLevel::Unspecified, "", clock))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::TestClock;

    /// Every `CommandState` variant, `Unspecified` included -- the full "from" axis of the
    /// product test below.
    const ALL_STATES: [CommandState; 9] = [
        CommandState::Unspecified,
        CommandState::Proposed,
        CommandState::Checked,
        CommandState::Authorized,
        CommandState::Dispatched,
        CommandState::Acked,
        CommandState::Rejected,
        CommandState::Expired,
        CommandState::Failed,
    ];

    fn fresh_command_in_state(state: CommandState) -> Command {
        Command { state: state as i32, ..Command::default() }
    }

    /// Like [`fresh_command_in_state`], but for `CommandState::Acked` also carries one prior
    /// `ACKED` transition at [`AckLevel::Edge`] -- the invariant every real `ACKED` command
    /// actually has (`push_transition` always appends before setting `state`), and what
    /// `ack`'s own A3.2/D2 "previous transition's `ack_level`" read needs to make `(Acked,
    /// ack)` a genuinely legal pair in the product test below rather than one that would
    /// panic or silently read `AckLevel::Unspecified` from an empty `transitions` list. Every
    /// OTHER edge function only reads `command.state` (`current_state`, via `require_one_of`),
    /// never `command.transitions`, so this extra transition changes nothing about how any of
    /// the other six edge functions treat a from-`Acked` `Command`.
    fn fresh_command_in_state_with_ack_history(state: CommandState) -> Command {
        let mut command = fresh_command_in_state(state);
        if state == CommandState::Acked {
            command.transitions.push(CommandTransition { state: CommandState::Acked as i32, ack_level: AckLevel::Edge as i32, ..Default::default() });
        }
        command
    }

    type EdgeFn = Box<dyn Fn(Command, &TestClock) -> Result<Command, CommandError>>;

    /// (edge name, function, target state, legal source states) -- every non-`propose` edge
    /// function this module exports, so the product test below drives all of them from one
    /// table instead of seven near-identical loops.
    fn edges() -> Vec<(&'static str, EdgeFn, CommandState, &'static [CommandState])> {
        vec![
            ("check", Box::new(|c, clk| check(c, "policy", "structurally valid", clk)), CommandState::Checked, &[CommandState::Proposed][..]),
            (
                "authorize",
                Box::new(|c, clk| authorize(c, "operator", "role matches command class", "", clk)),
                CommandState::Authorized,
                &[CommandState::Checked][..],
            ),
            (
                "dispatch",
                Box::new(|c, clk| dispatch(c, "ground-segment", "handed to the router", clk)),
                CommandState::Dispatched,
                &[CommandState::Authorized][..],
            ),
            (
                "ack",
                Box::new(|c, clk| ack(c, "flight-software", "executed", AckLevel::AssetExecuted, clk)),
                CommandState::Acked,
                // A3.2/D2: `ACKED` is now legal too -- `fresh_command_in_state_with_ack_history`
                // seeds a from-`Acked` fixture with a prior `AckLevel::Edge` transition, and
                // this edge always requests `AckLevel::AssetExecuted`, which strictly exceeds
                // `Edge` -- a genuine, legal increase, not a construction artefact.
                &[CommandState::Dispatched, CommandState::Acked][..],
            ),
            (
                "reject",
                Box::new(|c, clk| reject(c, "policy", "refused", clk)),
                CommandState::Rejected,
                &[CommandState::Proposed, CommandState::Checked][..],
            ),
            (
                "expire",
                Box::new(|c, clk| expire(c, "clock", "deadline passed", clk)),
                CommandState::Expired,
                &[CommandState::Authorized, CommandState::Dispatched][..],
            ),
            (
                "fail",
                Box::new(|c, clk| fail(c, "flight-software", "execution failed", clk)),
                CommandState::Failed,
                &[CommandState::Dispatched][..],
            ),
        ]
    }

    /// The acceptance test for this module: walks every (state, edge) pair in the full
    /// product (7 edge functions x 9 states = 63 pairs) and asserts that exactly the ten
    /// legal pairs succeed -- landing on the right target state, with exactly one new
    /// transition appended (two, for the seeded `(Acked, ack)` fixture) -- and that every
    /// other pair (53 of them) is refused with a typed [`CommandError`] naming the exact
    /// attempted edge: `CommandError::IllegalTransition { from, to }` for every pair with no
    /// edge at all, except `(Acked, ack)`'s own single non-increasing-level counterpart, which
    /// this test does not exercise here at all (the edge DOES exist from `Acked`; what would
    /// make it fail is the *level*, not the *state*, and this table only ever requests
    /// `AckLevel::AssetExecuted`, which is a genuine increase over the seeded `AckLevel::Edge`
    /// -- see `ack_from_acked_refuses_a_non_increasing_ack_level`, below, for that case).
    ///
    /// Arithmetic (checked against this test's own construction, not asserted from memory):
    /// 7 edge functions x 9 states (`ALL_STATES`, `Unspecified` included) = 63 pairs. Legal:
    /// check(1: Proposed) + authorize(1: Checked) + dispatch(1: Authorized) + ack(2:
    /// Dispatched, Acked) + reject(2: Proposed, Checked) + expire(2: Authorized, Dispatched) +
    /// fail(1: Dispatched) = 10. Illegal: 63 - 10 = 53.
    #[test]
    fn every_state_edge_pair_in_the_product_is_legal_or_typed_refused() {
        let clock = TestClock::new(1_000);
        let mut legal = 0usize;
        let mut illegal = 0usize;
        for (name, f, to, legal_from) in edges() {
            for &from in &ALL_STATES {
                let command = fresh_command_in_state_with_ack_history(from);
                let expected_transitions_before = command.transitions.len();
                let result = f(command, &clock);
                if legal_from.contains(&from) {
                    legal += 1;
                    let command = result.unwrap_or_else(|e| panic!("{name} from {from:?} must succeed, got {e:?}"));
                    assert_eq!(current_state(&command), to, "{name} from {from:?}");
                    assert_eq!(command.transitions.len(), expected_transitions_before + 1, "{name} from {from:?} must append exactly one transition");
                    assert_eq!(command.transitions.last().unwrap().state, to as i32);
                } else {
                    illegal += 1;
                    let err = result.expect_err(&format!("{name} from {from:?} must be refused"));
                    assert_eq!(err, CommandError::IllegalTransition { from, to }, "{name} from {from:?}");
                }
            }
        }
        assert_eq!(legal, 10, "must match the ten legal edges exactly (A3.2/D2 added ACKED -> ACKED)");
        assert_eq!(illegal, 7 * 9 - 10, "must match 53 typed refusals exactly");
    }

    /// A3.2/D2's own required test: a non-increasing (equal, or lower) `ack_level` from an
    /// already-`ACKED` command is refused as [`CommandError::AckLevelNotIncreasing`] -- never
    /// `IllegalTransition` (the edge exists) and never silently ignored (the command's state
    /// and transitions are unchanged on the `Err` path, since `ack` returns before calling
    /// `push_transition` at all).
    #[test]
    fn ack_from_acked_refuses_a_non_increasing_ack_level() {
        let clock = TestClock::new(1_000);
        // Equal to the previous level (`AckLevel::Edge`, seeded by the fixture below).
        let command = fresh_command_in_state_with_ack_history(CommandState::Acked);
        let err = ack(command, "flight-software", "repeat", AckLevel::Edge, &clock).unwrap_err();
        assert_eq!(err, CommandError::AckLevelNotIncreasing { previous: AckLevel::Edge, requested: AckLevel::Edge });

        // Lower than the previous level.
        let mut command = fresh_command_in_state(CommandState::Acked);
        command.transitions.push(CommandTransition { state: CommandState::Acked as i32, ack_level: AckLevel::AssetExecuted as i32, ..Default::default() });
        let err = ack(command, "flight-software", "regressed", AckLevel::AssetReceived, &clock).unwrap_err();
        assert_eq!(err, CommandError::AckLevelNotIncreasing { previous: AckLevel::AssetExecuted, requested: AckLevel::AssetReceived });
    }

    /// The positive counterpart: `ACKED -> ACKED` with a strictly increasing level, at every
    /// real step of the ladder (`Edge` -> `AssetReceived` -> `AssetExecuted`), succeeds and
    /// appends exactly one transition each time.
    #[test]
    fn ack_from_acked_accepts_each_strictly_increasing_ack_level_in_turn() {
        let clock = TestClock::new(1_000);
        let command = fresh_command_in_state(CommandState::Dispatched);
        let command = ack(command, "flight-software", "edge", AckLevel::Edge, &clock).unwrap();
        assert_eq!(command.transitions.len(), 1);
        let command = ack(command, "flight-software", "received", AckLevel::AssetReceived, &clock).unwrap();
        assert_eq!(command.transitions.len(), 2);
        assert_eq!(current_state(&command), CommandState::Acked);
        let command = ack(command, "flight-software", "executed", AckLevel::AssetExecuted, &clock).unwrap();
        assert_eq!(command.transitions.len(), 3);
        assert_eq!(command.transitions.iter().map(|t| t.ack_level).collect::<Vec<_>>(), vec![AckLevel::Edge as i32, AckLevel::AssetReceived as i32, AckLevel::AssetExecuted as i32]);
    }

    #[test]
    fn propose_builds_proposed_from_a_fresh_command_with_the_clocks_epoch() {
        let clock = TestClock::new(42);
        let command = fresh_command_in_state(CommandState::Unspecified);
        let proposed = propose(command, "model-x", "scored radius drifted past threshold", &clock).unwrap();
        assert_eq!(current_state(&proposed), CommandState::Proposed);
        assert_eq!(proposed.transitions.len(), 1);
        assert_eq!(proposed.transitions[0].tai_ns, 42);
        assert_eq!(proposed.transitions[0].principal, "model-x");
    }

    /// Question 53's own rule, enforced in code (not only in the A1.2 Rego policy): a
    /// non-empty `envelope_id` is refused by `propose` itself, typed.
    #[test]
    fn propose_refuses_a_non_empty_envelope_id() {
        let clock = TestClock::new(0);
        let mut command = fresh_command_in_state(CommandState::Unspecified);
        command.envelope_id = "env-station-keeping".to_string();
        let err = propose(command, "model-x", "reason", &clock).unwrap_err();
        assert_eq!(err, CommandError::EnvelopeNotAllowed { envelope_id: "env-station-keeping".to_string() });
    }

    #[test]
    fn propose_refuses_a_command_that_already_has_a_state() {
        let clock = TestClock::new(0);
        for &state in &ALL_STATES[1..] {
            let command = fresh_command_in_state(state);
            let err = propose(command, "model-x", "reason", &clock).unwrap_err();
            assert_eq!(err, CommandError::AlreadyStarted { state }, "state {state:?}");
        }
    }

    /// `ack_level` is carried on the transition, never folded into `CommandState` -- fails
    /// against an implementation that drops it or invents a per-ack-level `CommandState`.
    #[test]
    fn ack_carries_the_ack_level_as_a_separate_axis() {
        let clock = TestClock::new(7);
        let command = fresh_command_in_state(CommandState::Dispatched);
        let acked = ack(command, "flight-software", "executed", AckLevel::AssetExecuted, &clock).unwrap();
        assert_eq!(current_state(&acked), CommandState::Acked);
        assert_eq!(acked.transitions[0].ack_level, AckLevel::AssetExecuted as i32);
    }

    /// `authorize` threads a non-empty `delegation_id` onto the transition -- A2 will enforce
    /// its expiry; A1 only proves the field is carried end to end.
    #[test]
    fn authorize_carries_a_delegation_id_when_given_one() {
        let clock = TestClock::new(3);
        let command = fresh_command_in_state(CommandState::Checked);
        let authorized = authorize(command, "operator", "role matches", "delegation-1", &clock).unwrap();
        assert_eq!(authorized.transitions[0].delegation_id, "delegation-1");
    }
}
