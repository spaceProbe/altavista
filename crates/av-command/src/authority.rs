//! The check edge (`docs/aiplane-plan.md` milestone A1.2): evaluates policy at `CHECKED` over
//! a `PROPOSED` [`Command`] and drives both the state machine ([`crate::state`]) and the
//! ledger ([`crate::ledger::Ledger`]) from the resulting [`PolicyDecision`].
//!
//! This lives in its own module rather than as an addition to [`crate::policy`] because
//! [`crate::policy`] is deliberately pure (load a bundle, evaluate an input, get a decision --
//! no I/O beyond reading the bundle's own files, no state machine, no ledger); this module is
//! the wiring that decides *what happens* with a decision, which needs both [`crate::state`]
//! and [`crate::ledger`] and is therefore a different set of concerns and a different set of
//! failure modes ([`state::CommandError`] and [`std::io::Error`], neither of which
//! [`crate::policy::evaluate`] can produce).
//!
//! # Both outcomes are equally reproducible from the ledger
//!
//! A1.1's `authority.proto` originally documented `LedgerRecord.decision` as present *iff*
//! `transition.state == COMMAND_STATE_CHECKED`. This module's very purpose -- policy denials
//! being just as auditable as approvals -- changes that: [`check_command`] attaches the same
//! `PolicyDecision` (allow or deny) to the `LedgerRecord` for **both** the `CHECKED` and the
//! `REJECTED` transition a policy decision produces. `authority.proto`'s `LedgerRecord.
//! decision` doc comment and `Ledger::append`'s own doc comment were both amended to say so
//! (a comment-only change; no field numbers moved). A `COMMAND_STATE_REJECTED` transition that
//! did *not* come from a policy decision (there is none in this crate today, but `state::
//! reject` also accepts `PROPOSED` as a source state for a structural rejection that never
//! reached `CHECKED`) still carries no `PolicyDecision` -- only a policy-produced denial does.
//!
//! # The reason format
//!
//! [`format_reason`] is the one function that turns a [`PolicyDecision`] into the
//! `CommandTransition.reason` string `docs/aiplane-plan.md`'s A1 milestone text requires
//! ("the decision id and the policy hash go into the transition's reason"):
//!
//! ```text
//! decision_id=<hex> policy_hash=<hex> allow=<true|false>[ reasons=<r1>|<r2>|...]
//! ```
//!
//! `reasons=...` (pipe-joined) is present only when the decision carries at least one reason
//! (always true for a denial; never true for an allow, per `crate::policy::evaluate`'s own
//! contract that an allow's `reasons` is empty). [`parse_reason`] is the matching parser, so a
//! later replay/audit tool has a documented way to pull `decision_id`/`policy_hash`/`allow`/
//! `reasons` back out of a transition's `reason` text alone, without needing the attached
//! `PolicyDecision` -- [`tests::format_reason_round_trips_through_parse_reason`] proves the
//! two are inverses for both an allow and a deny decision.

use std::io;

use av_cdm::pb::{Command, PolicyDecision};
use thiserror::Error;

use crate::clock::Clock;
use crate::ledger::{CommandMeta, Ledger};
use crate::policy::{self, PolicyBundle};
use crate::rate::RateSource;
use crate::state::{self, CommandError};

/// The principal recorded on a `CHECKED`/`REJECTED` transition produced by policy evaluation
/// -- an automated evaluator, not a human (A2 is the milestone that introduces human
/// principals on `authorize`), matching `crates/av-command/src/state.rs`'s own test
/// convention (`check(c, "policy", ...)`, `reject(c, "policy", ...)`).
pub const POLICY_PRINCIPAL: &str = "policy";

/// Everything [`check_command`] can fail with. Distinct from [`crate::policy::evaluate`]'s own
/// failure mode (which has none -- see that function's doc): every variant here comes from
/// either the state machine or the ledger/rate source's I/O, not from policy evaluation
/// itself.
#[derive(Debug, Error)]
pub enum CheckCommandError {
    /// `command` was not in `COMMAND_STATE_PROPOSED` (the only legal source state for the
    /// `check`/`reject` edges this function drives).
    #[error("check_command: {0}")]
    State(#[from] CommandError),
    /// Reading the recent-rate window, or appending the resulting record, failed.
    #[error("check_command: ledger/rate I/O: {0}")]
    Io(#[from] io::Error),
}

/// The result of running a `PROPOSED` command through the check edge: the command in its new
/// state (`CHECKED` or `REJECTED`) and the [`PolicyDecision`] that produced it.
#[derive(Debug, Clone)]
pub struct CheckCommandResult {
    pub command: Command,
    pub decision: PolicyDecision,
}

/// Evaluates policy over `command` (which must be `COMMAND_STATE_PROPOSED`) and drives the
/// matching state transition and ledger append:
///
/// - **allow**: [`state::check`] (`PROPOSED -> CHECKED`), then [`Ledger::append`] with the
///   `CHECKED` transition and this `PolicyDecision` attached.
/// - **deny**: [`state::reject`] (`PROPOSED -> REJECTED`), then [`Ledger::append`] with the
///   `REJECTED` transition and the **same** `PolicyDecision` attached (see the module doc's
///   "Both outcomes are equally reproducible from the ledger" section).
///
/// `command.entity_id` is the ledger partition; `rate_source` answers `PolicyInputRate.
/// counts_by_class` for that partition over the trailing `rate_window_ns`, evaluated as of
/// `clock.now_tai_ns()` at the moment this call starts (the same instant [`policy::evaluate`]
/// uses for `PolicyDecision.evaluated_tai_ns`, since neither this function nor `evaluate`
/// advances `clock` themselves).
pub fn check_command(
    command: Command,
    bundle: &PolicyBundle,
    rate_window_ns: i64,
    rate_source: &dyn RateSource,
    ledger: &Ledger,
    clock: &dyn Clock,
) -> Result<CheckCommandResult, CheckCommandError> {
    let as_of_tai_ns = clock.now_tai_ns();
    let counts_by_class = rate_source.counts_by_class(&command.entity_id, as_of_tai_ns, rate_window_ns)?;

    let input = av_cdm::pb::PolicyInput {
        command_id: command.id.clone(),
        entity_id: command.entity_id.clone(),
        command_class: command.command_class.clone(),
        hazardous: command.hazardous,
        envelope_id: command.envelope_id.clone(),
        label: command.label.clone(),
        rate: Some(av_cdm::pb::PolicyInputRate { counts_by_class, window_ns: rate_window_ns }),
    };

    let decision = policy::evaluate(bundle, &input, clock);
    let reason = format_reason(&decision);

    let new_command = if decision.allow {
        state::check(command, POLICY_PRINCIPAL, &reason, clock)?
    } else {
        state::reject(command, POLICY_PRINCIPAL, &reason, clock)?
    };
    let transition = new_command
        .transitions
        .last()
        .expect("state::check/state::reject always appends exactly one transition")
        .clone();

    ledger.append(
        CommandMeta::new(&new_command.entity_id, &new_command.id, &new_command.command_class, &new_command.idempotency_key),
        transition,
        Some(decision.clone()),
        Some(&new_command),
        None,
        clock,
    )?;

    Ok(CheckCommandResult { command: new_command, decision })
}

/// See the module doc's "The reason format" section for the exact grammar.
pub fn format_reason(decision: &PolicyDecision) -> String {
    let mut s = format!("decision_id={} policy_hash={} allow={}", decision.decision_id, decision.policy_hash, decision.allow);
    if !decision.reasons.is_empty() {
        s.push_str(" reasons=");
        s.push_str(&decision.reasons.join("|"));
    }
    s
}

/// `decision_id`/`policy_hash`/`allow`/`reasons` pulled back out of a [`format_reason`]
/// string. `None` if `reason` is missing `decision_id=`, `policy_hash=` or `allow=` --
/// i.e. it was not produced by [`format_reason`] at all (a transition whose reason has
/// nothing to do with a policy decision, such as a structural rejection's own free-text
/// reason).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedReason {
    pub decision_id: String,
    pub policy_hash: String,
    pub allow: bool,
    pub reasons: Vec<String>,
}

pub fn parse_reason(reason: &str) -> Option<ParsedReason> {
    let (head, tail) = match reason.split_once(" reasons=") {
        Some((h, t)) => (h, Some(t)),
        None => (reason, None),
    };
    let mut decision_id = None;
    let mut policy_hash = None;
    let mut allow = None;
    for token in head.split_whitespace() {
        if let Some(v) = token.strip_prefix("decision_id=") {
            decision_id = Some(v.to_string());
        } else if let Some(v) = token.strip_prefix("policy_hash=") {
            policy_hash = Some(v.to_string());
        } else if let Some(v) = token.strip_prefix("allow=") {
            allow = Some(v == "true");
        }
    }
    let reasons = tail.map(|t| t.split('|').map(|s| s.to_string()).collect()).unwrap_or_default();
    Some(ParsedReason { decision_id: decision_id?, policy_hash: policy_hash?, allow: allow?, reasons })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::TestClock;
    use crate::policy::PolicyBundle;
    use crate::rate::FixtureRateSource;
    use av_cdm::pb::CommandState;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn tmp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("av-command-authority-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn write_starter_policy(dir: &std::path::Path) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join("command.rego"),
            "package altavista.authority\n\nallow if { input.command_class == \"mode\" }\n\ndeny contains \"payload is not admitted\" if { input.command_class == \"payload\" }\n",
        )
        .unwrap();
    }

    fn proposed_command(id: &str, entity_id: &str, command_class: &str) -> Command {
        let mut command = Command { id: id.to_string(), entity_id: entity_id.to_string(), command_class: command_class.to_string(), ..Command::default() };
        command = state::propose(command, "model-x", "test", &TestClock::new(0)).unwrap();
        command
    }

    #[test]
    fn allow_checks_the_command_and_appends_a_checked_record_with_the_decision() {
        let policy_dir = tmp_dir("allow-policy");
        write_starter_policy(&policy_dir);
        let bundle = PolicyBundle::load(&policy_dir).unwrap();
        let ledger_dir = tmp_dir("allow-ledger");
        let ledger = Ledger::open(&ledger_dir).unwrap();
        let clock = TestClock::new(1_000);
        let rate = FixtureRateSource::new(BTreeMap::new());

        let command = proposed_command("cmd-1", "sat-1", "mode");
        let result = check_command(command, &bundle, 3_600_000_000_000, &rate, &ledger, &clock).unwrap();

        assert!(result.decision.allow);
        assert_eq!(state::current_state(&result.command), CommandState::Checked);
        let reason = &result.command.transitions.last().unwrap().reason;
        assert!(reason.contains(&format!("decision_id={}", result.decision.decision_id)), "{reason}");
        assert!(reason.contains(&format!("policy_hash={}", result.decision.policy_hash)), "{reason}");

        let verification = ledger.verify("sat-1").unwrap();
        assert!(verification.ok, "{verification:?}");

        let _ = std::fs::remove_dir_all(&policy_dir);
        let _ = std::fs::remove_dir_all(&ledger_dir);
    }

    #[test]
    fn deny_rejects_the_command_and_appends_a_rejected_record_with_the_same_decision() {
        let policy_dir = tmp_dir("deny-policy");
        write_starter_policy(&policy_dir);
        let bundle = PolicyBundle::load(&policy_dir).unwrap();
        let ledger_dir = tmp_dir("deny-ledger");
        let ledger = Ledger::open(&ledger_dir).unwrap();
        let clock = TestClock::new(1_000);
        let rate = FixtureRateSource::new(BTreeMap::new());

        let command = proposed_command("cmd-2", "sat-1", "payload");
        let result = check_command(command, &bundle, 3_600_000_000_000, &rate, &ledger, &clock).unwrap();

        assert!(!result.decision.allow);
        assert_eq!(state::current_state(&result.command), CommandState::Rejected);
        assert_eq!(result.decision.reasons, vec!["payload is not admitted".to_string()]);

        let _ = std::fs::remove_dir_all(&policy_dir);
        let _ = std::fs::remove_dir_all(&ledger_dir);
    }

    #[test]
    fn format_reason_round_trips_through_parse_reason() {
        let allow_decision = PolicyDecision {
            decision_id: "abc".to_string(),
            allow: true,
            policy_hash: "def".to_string(),
            reasons: Vec::new(),
            matched_rule_path: policy::ALLOW_ENTRYPOINT.to_string(),
            evaluated_tai_ns: 1_000,
            input: None,
        };
        let parsed = parse_reason(&format_reason(&allow_decision)).unwrap();
        assert_eq!(parsed.decision_id, "abc");
        assert_eq!(parsed.policy_hash, "def");
        assert!(parsed.allow);
        assert!(parsed.reasons.is_empty());

        let deny_decision = PolicyDecision {
            decision_id: "ghi".to_string(),
            allow: false,
            policy_hash: "jkl".to_string(),
            reasons: vec!["a".to_string(), "b".to_string()],
            matched_rule_path: policy::DENY_ENTRYPOINT.to_string(),
            evaluated_tai_ns: 2_000,
            input: None,
        };
        let parsed = parse_reason(&format_reason(&deny_decision)).unwrap();
        assert_eq!(parsed.decision_id, "ghi");
        assert_eq!(parsed.policy_hash, "jkl");
        assert!(!parsed.allow);
        assert_eq!(parsed.reasons, vec!["a".to_string(), "b".to_string()]);
    }
}
