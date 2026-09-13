//! D6: "an evidence topic" realized as a ledger partition, not a broker (ADR-004: "for the
//! engine and command services the durable log itself is the ledger, chained per
//! partition"; question 200(d)'s identical call for the edge track's own durable log). No
//! Kafka/Redpanda crate exists in this tree or is added by this module; a later round MAY
//! replace this realization with a real broker without changing [`ProposalEvidence`]'s own
//! shape (question 200(d)'s "the consumer trait is spoore-io's so the broker can replace
//! the files" reasoning, mirrored here).
//!
//! ## Why this reuses `av_command::ledger::Ledger` unmodified, packed inside `Command.payload`
//!
//! This crate must not edit `crates/av-command` (out of this task's own scope; see this
//! crate's own task brief). `LedgerRecord` (`authority.proto`) cannot simply gain a new
//! `proposal_evidence` field: `crates/av-command/src/ledger.rs::Ledger::append` builds every
//! `LedgerRecord` with an EXHAUSTIVE struct literal (no `..Default::default()`), so a new
//! field there would fail that crate's own compilation the instant it existed -- a real
//! landmine checked directly against the source before this design was chosen, not assumed.
//!
//! Instead, this module packs a [`ProposalEvidence`] into an existing, general-purpose
//! extension point `Command` already has: `Command.payload`, a `google.protobuf.Any`
//! (`command.proto`'s own doc: "Protocol-specific payload... typed by Any"). A synthetic
//! `Command` is built with `command_class = "proposal-evidence"`, `state = PROPOSED`, one
//! `CommandTransition` also at `PROPOSED`, and `payload` holding the `ProposalEvidence`
//! bytes under [`EVIDENCE_TYPE_URL`]; this is handed to the real, unmodified `Ledger::
//! append` exactly the way any other command snapshot is, and read back exactly the way
//! `Ledger::scan_commands` already reads back any other command snapshot -- no new ledger
//! code, no new wire shape on `LedgerRecord`.
//!
//! ## Partition convention -- a dedicated ledger, never the command service's own
//!
//! [`EvidenceRecorder`] is always constructed over its OWN [`av_command::ledger::Ledger`]
//! directory, never the same directory a live `CommandAuthorityServiceImpl` opens: that
//! service's own `Ledger::scan_commands` (called once, at its construction, to rebuild its
//! `commands` index -- `crates/av-command/src/service.rs`'s "In-memory index" section)
//! would otherwise pick up this module's synthetic `"proposal-evidence"`-class `Command`
//! under the SAME `command_id` a real proposed `Command` uses, and -- depending on which of
//! the two partition files' sanitized filenames sorts later -- could silently overwrite
//! that service's own view of the real command with this module's synthetic evidence
//! snapshot. A dedicated directory makes that collision structurally impossible rather
//! than merely unlikely. Within that dedicated ledger, each proposal's evidence lives in
//! its own partition, [`evidence_partition`]`(command_id)` (`"gateway-evidence:<command_id>"`),
//! independently verifiable via `Ledger::verify` from every other proposal's.

use av_cdm::pb::{AckLevel, Command, CommandState, CommandTransition, ProposalEvidence};
use av_command::clock::Clock;
use av_command::ledger::{CommandMeta, Ledger};
use prost::Message as _;
use prost_types::Any;

/// The `Any.type_url` this module packs a [`ProposalEvidence`] under.
pub const EVIDENCE_TYPE_URL: &str = "type.googleapis.com/altavista.v1.ProposalEvidence";

/// The `command_class` a synthetic evidence [`Command`] carries -- never a real command
/// class a policy would evaluate (this `Command` is never proposed through
/// `CommandAuthorityService`, only appended directly to this module's own dedicated
/// ledger).
pub const EVIDENCE_COMMAND_CLASS: &str = "proposal-evidence";

/// This module's own documented partition-naming convention: one partition per proposed
/// command's evidence, independently verifiable.
pub fn evidence_partition(command_id: &str) -> String {
    format!("gateway-evidence:{command_id}")
}

/// Writes and reads back [`ProposalEvidence`] records on a dedicated
/// [`av_command::ledger::Ledger`] -- see the module doc for why this ledger must never be
/// the same directory a live `CommandAuthorityServiceImpl` opens.
pub struct EvidenceRecorder<'a> {
    ledger: &'a Ledger,
}

impl<'a> EvidenceRecorder<'a> {
    pub fn new(ledger: &'a Ledger) -> Self {
        Self { ledger }
    }

    /// Packs `evidence` into a synthetic `Command` (see the module doc) and appends it to
    /// this evidence ledger's own partition for `evidence.command_id`. `clock` is the same
    /// injected `av_command::clock::Clock` every other epoch in this crate comes from (D8).
    pub fn record(&self, evidence: &ProposalEvidence, clock: &dyn Clock) -> std::io::Result<()> {
        let partition = evidence_partition(&evidence.command_id);
        let tai_ns = clock.now_tai_ns();
        let transition = CommandTransition {
            state: CommandState::Proposed as i32,
            tai_ns,
            principal: evidence.model_identity.clone(),
            reason: format!(
                "evidence topic record: model={:?} version={:?} run={:?} query_ids={:?}",
                evidence.model_identity, evidence.model_version, evidence.run, evidence.query_ids
            ),
            ack_level: AckLevel::Unspecified as i32,
            delegation_id: String::new(),
        };
        let payload = Any { type_url: EVIDENCE_TYPE_URL.to_string(), value: evidence.encode_to_vec() };
        let command = Command {
            id: evidence.command_id.clone(),
            entity_id: evidence.run.as_ref().map(|r| r.run_id.clone()).unwrap_or_default(),
            command_class: EVIDENCE_COMMAND_CLASS.to_string(),
            state: CommandState::Proposed as i32,
            transitions: vec![transition.clone()],
            payload: Some(payload),
            ..Default::default()
        };
        self.ledger
            .append(CommandMeta::new(&partition, &evidence.command_id, EVIDENCE_COMMAND_CLASS, ""), transition, None, Some(&command), None, clock)
            .map(|_record| ())
    }

    /// Reads back the [`ProposalEvidence`] this module wrote for `command_id`, decoded
    /// straight from the ledger's own `Command.payload` -- `None` if no evidence record for
    /// this `command_id` has ever been written to this ledger.
    pub fn read_back(&self, command_id: &str) -> std::io::Result<Option<ProposalEvidence>> {
        let commands = self.ledger.scan_commands()?;
        Ok(commands
            .get(command_id)
            .and_then(|c| c.payload.as_ref())
            .filter(|any| any.type_url == EVIDENCE_TYPE_URL)
            .and_then(|any| ProposalEvidence::decode(any.value.as_slice()).ok()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use av_cdm::pb::RunIdentity;
    use av_command::clock::TestClock;

    fn temp_ledger_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("av-gateway-evidence-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn record_then_read_back_reproduces_the_exact_evidence() {
        let dir = temp_ledger_dir("roundtrip");
        let ledger = Ledger::open(&dir).unwrap();
        let recorder = EvidenceRecorder::new(&ledger);
        let clock = TestClock::new(1_000);

        let evidence = ProposalEvidence {
            command_id: "cmd-42".to_string(),
            run: Some(RunIdentity { run_id: "run-a".to_string(), config_hash: "hash-a".to_string() }),
            query_ids: vec!["q1".to_string(), "q2".to_string()],
            model_identity: "model-x".to_string(),
            model_version: "1.2.3".to_string(),
            recorded_tai_ns: 1_000,
        };
        recorder.record(&evidence, &clock).unwrap();

        let back = recorder.read_back("cmd-42").unwrap().expect("evidence must be present");
        assert_eq!(back, evidence);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_back_is_none_for_a_command_id_never_recorded() {
        let dir = temp_ledger_dir("missing");
        let ledger = Ledger::open(&dir).unwrap();
        let recorder = EvidenceRecorder::new(&ledger);
        assert!(recorder.read_back("no-such-command").unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn evidence_partitions_are_verifiable_and_independent_per_command() {
        let dir = temp_ledger_dir("verify");
        let ledger = Ledger::open(&dir).unwrap();
        let recorder = EvidenceRecorder::new(&ledger);
        let clock = TestClock::new(1_000);

        for id in ["cmd-a", "cmd-b"] {
            let evidence = ProposalEvidence {
                command_id: id.to_string(),
                run: Some(RunIdentity { run_id: "run-a".to_string(), config_hash: String::new() }),
                query_ids: vec!["q1".to_string()],
                model_identity: "model-x".to_string(),
                model_version: "1.0.0".to_string(),
                recorded_tai_ns: 1_000,
            };
            recorder.record(&evidence, &clock).unwrap();
        }

        let v_a = ledger.verify(&evidence_partition("cmd-a")).unwrap();
        let v_b = ledger.verify(&evidence_partition("cmd-b")).unwrap();
        assert!(v_a.ok && v_b.ok);
        assert_eq!(v_a.checked, 1);
        assert_eq!(v_b.checked, 1);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
