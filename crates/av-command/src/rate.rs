//! The rate source behind `PolicyInputRate.counts_by_class` (`docs/aiplane-plan.md` A1.2:
//! "the rate of recent commands per class"). [`RateSource`] is a small trait with two
//! implementors: [`LedgerRateSource`] (real history, read from a [`crate::ledger::Ledger`])
//! and [`FixtureRateSource`] (a deterministic, caller-set map, for tests that need to pin an
//! exact rate without first appending a real sequence of ledger records).
//!
//! # Which records count as "a command was submitted"
//!
//! [`LedgerRateSource`] counts a [`av_cdm::pb::LedgerRecord`] exactly when its transition is
//! `COMMAND_STATE_PROPOSED` -- see `crates/av-command/src/ledger.rs`'s
//! [`crate::ledger::Ledger::count_proposed_by_class_in_window`] doc comment for the full
//! reasoning (restated briefly here since this trait is the public surface a caller actually
//! reaches for): `PROPOSED` is `crate::state`'s state machine's only entry point (`propose`
//! builds a fresh `Command` rather than advancing an existing one), so counting its arrivals
//! counts submissions once each -- never a later advance of a command already counted
//! (`CHECKED`, `AUTHORIZED`, ...), and never under-counting a command a policy goes on to
//! deny before it ever reaches `CHECKED`.

use std::collections::BTreeMap;
use std::io;

use crate::ledger::Ledger;

/// Answers "how many commands of each class recently entered the machine for this
/// partition (entity)". The one method every implementor provides; see the module doc for
/// which ledger records [`LedgerRateSource`] counts.
pub trait RateSource: Send + Sync {
    /// Counts, within the trailing window `(as_of_tai_ns - window_ns, as_of_tai_ns]`, recent
    /// command submissions for `partition`, grouped by `Command.command_class`.
    fn counts_by_class(&self, partition: &str, as_of_tai_ns: i64, window_ns: i64) -> io::Result<BTreeMap<String, u64>>;
}

/// The real implementor: reads `crate::ledger::Ledger::count_proposed_by_class_in_window`
/// straight from disk. Borrows the ledger rather than owning it, since the same `Ledger`
/// handle this crate's check edge (`crate::authority`) also appends `CHECKED`/`REJECTED`
/// records through is the ledger this rate source must read from -- two independent handles
/// over the same directory would still agree (`Ledger::verify`'s own doc makes the same
/// claim), but sharing one handle is simpler and is what `crate::authority::check_command`
/// does.
pub struct LedgerRateSource<'a> {
    ledger: &'a Ledger,
}

impl<'a> LedgerRateSource<'a> {
    pub fn new(ledger: &'a Ledger) -> Self {
        Self { ledger }
    }
}

impl RateSource for LedgerRateSource<'_> {
    fn counts_by_class(&self, partition: &str, as_of_tai_ns: i64, window_ns: i64) -> io::Result<BTreeMap<String, u64>> {
        self.ledger.count_proposed_by_class_in_window(partition, as_of_tai_ns, window_ns)
    }
}

/// A deterministic fixture a test sets explicitly, for a test that needs to pin an exact
/// rate without first appending a real sequence of ledger records (mirrors
/// `crate::clock::TestClock`'s own role: production code never constructs this, but it is
/// not itself `#[cfg(test)]`-gated, the same way `TestClock` is not, so a test in any crate
/// depending on this one can use it too).
#[derive(Debug, Default, Clone)]
pub struct FixtureRateSource {
    counts: BTreeMap<String, u64>,
}

impl FixtureRateSource {
    pub fn new(counts: BTreeMap<String, u64>) -> Self {
        Self { counts }
    }
}

impl RateSource for FixtureRateSource {
    /// Ignores `partition`/`as_of_tai_ns`/`window_ns` entirely and returns exactly the map the
    /// test constructed this fixture with -- deliberately: a fixture's whole point is to let
    /// a test state the rate it wants evaluated, not to re-derive windowing logic
    /// [`LedgerRateSource`] already owns and already has its own tests for.
    fn counts_by_class(&self, _partition: &str, _as_of_tai_ns: i64, _window_ns: i64) -> io::Result<BTreeMap<String, u64>> {
        Ok(self.counts.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::TestClock;
    use av_cdm::pb::{AckLevel, CommandState, CommandTransition};

    fn transition(state: CommandState, tai_ns: i64) -> CommandTransition {
        CommandTransition { state: state as i32, tai_ns, principal: "policy".to_string(), reason: "reason".to_string(), ack_level: AckLevel::Unspecified as i32, delegation_id: String::new() }
    }

    fn tmp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("av-command-rate-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// [`LedgerRateSource`] against a ledger populated through the real `append` path, with a
    /// [`TestClock`] driving each record's epoch explicitly -- no sleeping.
    #[test]
    fn ledger_rate_source_counts_real_appended_proposed_records() {
        let dir = tmp_dir("ledger-rate-source");
        let ledger = Ledger::open(&dir).unwrap();
        let clock = TestClock::new(0);

        clock.set(1_000);
        ledger.append("sat-1", "cmd-1", "burn", transition(CommandState::Proposed, 1_000), None, &clock).unwrap();
        clock.set(1_200);
        ledger.append("sat-1", "cmd-2", "burn", transition(CommandState::Proposed, 1_200), None, &clock).unwrap();
        clock.set(1_400);
        ledger.append("sat-1", "cmd-3", "mode", transition(CommandState::Proposed, 1_400), None, &clock).unwrap();

        let source = LedgerRateSource::new(&ledger);
        let counts = source.counts_by_class("sat-1", 1_500, 1_000).unwrap();
        assert_eq!(counts.get("burn").copied(), Some(2));
        assert_eq!(counts.get("mode").copied(), Some(1));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fixture_rate_source_returns_exactly_what_it_was_given() {
        let mut counts = BTreeMap::new();
        counts.insert("burn".to_string(), 9u64);
        let source = FixtureRateSource::new(counts.clone());
        assert_eq!(source.counts_by_class("anything", 0, 0).unwrap(), counts);
    }
}
