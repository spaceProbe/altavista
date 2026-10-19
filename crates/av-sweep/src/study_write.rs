//! Writing one study to every configured store without leaving them disagreeing (question 254
//! item 2). Study mode (`--store-dir`, `--store-clickhouse`) and `--import-study` both go through
//! here; a single store's [`StudyStore::put_study`] is unchanged.
//!
//! ## The protocol
//!
//! 1. **[`check_plan`], before the first sample runs.** Every configured store is asked whether it
//!    would accept the study (`StudyStore::check_plan`, judged on what is known before any sample
//!    runs: [`crate::plan`]). A store holding no study under the id, or one the plan can
//!    reproduce, accepts. Any refusal is fatal: nothing was written to any store.
//! 2. **[`write_study`], after the run.**
//!    1. *Prepare*, every store, nothing visible yet, ClickHouse first and the file store second
//!       ([`StoreKind`] order): each re-checks the final content (identical: nothing to stage;
//!       different or an id refusal: refused) and stages it. The file store writes and fsyncs its
//!       `.incoming-*` directory; ClickHouse inserts every row except the header, which are
//!       invisible orphans (`clickhouse` module doc, decision 1).
//!    2. If any store refused or failed: **abort** every prepared store (the file stage is
//!       removed; ClickHouse's headerless rows stay invisible) and return
//!       [`StudyWriteError::PrepareFailed`] naming the store. Nothing visible changed anywhere.
//!    3. *Commit*, ClickHouse first, then the file store. ClickHouse's commit is the header insert
//!       and the winner re-read; the file store's is the rename of an already-fsynced stage on the
//!       same filesystem. The network commit goes first because it is the one that fails in
//!       practice, and the local rename last means no reader ever sees a study that is then
//!       withdrawn.
//!
//! ## Every outcome
//!
//! | Failure | Outcome | Stores afterwards |
//! | --- | --- | --- |
//! | A store refuses the plan (up front) | [`StudyWriteError::PlanRefused`], no sample runs | unchanged |
//! | The file store's target is occupied: a non-empty directory that holds no study, or a non-directory, where the study goes | [`StudyWriteError::PlanRefused`] up front, or [`StudyWriteError::PrepareFailed`] at prepare (typed `Io` naming the path); ClickHouse's rows aborted | unchanged (ClickHouse may hold invisible orphans); ClickHouse is never committed |
//! | A store refuses or fails at prepare | [`StudyWriteError::PrepareFailed`] naming the store; stages aborted | unchanged (ClickHouse may hold invisible orphans) |
//! | The first commit fails definitely (ClickHouse's header insert failed and the re-read shows no header of ours) | [`StudyWriteError::CommitFailed`]; stages aborted | unchanged |
//! | ClickHouse's header insert errors and the re-read fails on **every one of its 3 attempts** (200 ms apart; the header insert is never retried) | [`StudyWriteError::CommitOutcomeUnknown`] naming the write id and the repair; the file stage aborted | ClickHouse may or may not hold the study; the file store does not |
//! | ClickHouse's header reply is lost, or the insert fails, but a re-read attempt succeeds | resolved: our header (or an identical one) is the winner and the commit succeeded, or no header of ours shows and it failed definitely | as the resolved case |
//! | ClickHouse lost a race to a writer of different content | [`StudyWriteError::CommitFailed`] holding `RefusedWrite::DifferentContent`; the file stage aborted | ClickHouse holds the other writer's study, the file store is unchanged |
//! | ClickHouse committed, the file rename fails: a permission or disk failure, or a target that appeared after prepare | [`StudyWriteError::PartialCommit`] naming both and the repair | ClickHouse holds the study, the file store does not (a stage may be left if its removal fails too) |
//! | Both already hold identical content | `Ok`, nothing written | unchanged |
//! | One already holds identical content, the other has none | the other is written; if that fails, as above | at worst the state before the run |
//!
//! The two outcomes in which one store can hold the study and the other not cannot be closed
//! without a distributed transaction, which neither store has. They are the **residual windows**:
//! (1) a ClickHouse commit whose header-insert outcome cannot be read back in 3 attempts, and
//! (2) a file-store rename that fails for a reason `prepare` could not see (permissions, disk,
//! a target created between the phases) after ClickHouse committed. Both are typed errors that
//! say which store holds what, and the repair is the same command for both:
//! `av-sweep --import-study <sweep_results.pb>` with the same store options, which converges
//! (identical content is idempotent in each store). Study mode writes `sweep_results.pb` to
//! `--out-dir` before it touches any store, so that input always exists.
//!
//! Two narrowings of those windows: the file store refuses an occupied target at `check_plan` and
//! `prepare` (the most plausible rename failure becomes a clean refusal with nothing committed),
//! and ClickHouse's commit retries its winner re-read (a read-only `SELECT`) up to 3 times, 200 ms
//! apart (at most 400 ms of added sleeping before the outcome is declared unknown).

use crate::plan::StudyPlan;
use crate::store::{PreparedWrite, StoreError, StoreKind, StudyStore};

/// One configured store: the label messages name it by (for example "the ClickHouse study store
/// at http://... (database d)") and the store.
pub struct NamedStore {
    pub label: String,
    pub store: Box<dyn StudyStore>,
}

/// A store that holds the study after [`write_study`]: it was written, or already held it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    pub label: String,
    pub kind: StoreKind,
    pub location: String,
    /// `true` when this call made the study visible in this store, `false` when the store already
    /// held an identical one.
    pub newly_written: bool,
}

/// Why a study was not written to every store, by phase. Every variant names the store, and each
/// says what the stores hold now; [`StudyWriteError::store_error`] is the store's own error.
#[derive(Debug, thiserror::Error)]
pub enum StudyWriteError {
    /// A store refused the plan up front (in study mode, before the first sample ran). Nothing
    /// was written anywhere.
    #[error("{store} refuses this study up front: {source}; nothing was written to any store")]
    PlanRefused { store: String, source: StoreError },

    /// A store refused or failed while preparing the write. Every prepared store was aborted;
    /// nothing visible changed in any store. `abort_failures` lists stages that could not be
    /// removed (stray `.incoming-*` directories an operator may delete).
    #[error("{store}: the write was refused or failed while preparing it: {source}; nothing was made visible in any store{}", abort_note(.abort_failures))]
    PrepareFailed { store: String, source: StoreError, abort_failures: Vec<String> },

    /// The first commit failed definitely (the study is not in that store, and no store was
    /// changed by this run); the other stages were aborted.
    #[error("{store}: the commit failed: {source}; nothing was made visible by this run in any store{}", abort_note(.abort_failures))]
    CommitFailed { store: String, source: StoreError, abort_failures: Vec<String> },

    /// A ClickHouse commit's outcome is unknown ([`StoreError::CommitOutcomeUnknown`], after the
    /// re-read was attempted three times); the stages
    /// of the stores after it were aborted, so the file store does not hold the study.
    #[error("{store}: whether the study was committed is unknown: {source}; the other stores do not hold it from this run{}; repair: {REPAIR}", abort_note(.abort_failures))]
    CommitOutcomeUnknown { store: String, source: StoreError, abort_failures: Vec<String> },

    /// `committed` stores hold the study from this run and `failed` could not commit it: the
    /// stores disagree. `source` is `failed`'s error.
    #[error("the stores now disagree: the study was written to {} but {failed} could not commit it: {source}; repair: {REPAIR}{}", labels(.committed), abort_note(.abort_failures))]
    PartialCommit { committed: Vec<Written>, failed: String, source: StoreError, abort_failures: Vec<String> },
}

/// The repair for both unrecoverable-window outcomes.
pub const REPAIR: &str = "re-run `av-sweep --import-study <sweep_results.pb>` with the same store options (identical content is idempotent in each store, so it converges)";

fn labels(committed: &[Written]) -> String {
    committed.iter().filter(|w| w.newly_written).map(|w| w.label.as_str()).collect::<Vec<_>>().join(", ")
}

fn abort_note(failures: &[String]) -> String {
    if failures.is_empty() {
        String::new()
    } else {
        format!("; staged data that could not be removed (safe to delete by hand): {}", failures.join("; "))
    }
}

impl StudyWriteError {
    /// The error of the store that refused or failed.
    pub fn store_error(&self) -> &StoreError {
        match self {
            StudyWriteError::PlanRefused { source, .. }
            | StudyWriteError::PrepareFailed { source, .. }
            | StudyWriteError::CommitFailed { source, .. }
            | StudyWriteError::CommitOutcomeUnknown { source, .. }
            | StudyWriteError::PartialCommit { source, .. } => source,
        }
    }

    /// Whether the stores may now disagree, so the repair applies.
    pub fn needs_repair(&self) -> bool {
        matches!(self, StudyWriteError::CommitOutcomeUnknown { .. } | StudyWriteError::PartialCommit { .. })
    }
}

/// Indices of `stores` in commit order: the network store first, ties in the order given.
fn commit_order(stores: &[NamedStore]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..stores.len()).collect();
    order.sort_by_key(|&i| stores[i].store.kind());
    order
}

/// Step 1: asks every store whether it would accept `plan`. Writes nothing.
// A once-per-study error carrying the store's own error: its size is irrelevant here.
#[allow(clippy::result_large_err)]
pub fn check_plan(stores: &[NamedStore], plan: &StudyPlan) -> Result<(), StudyWriteError> {
    for i in commit_order(stores) {
        stores[i].store.check_plan(plan).map_err(|source| StudyWriteError::PlanRefused { store: stores[i].label.clone(), source })?;
    }
    Ok(())
}

/// Aborts every prepared write, newest first, and returns what could not be removed.
fn abort_all(stores: &mut [NamedStore], prepared: Vec<(usize, PreparedWrite)>) -> Vec<String> {
    let mut failures = Vec::new();
    for (i, write) in prepared.into_iter().rev() {
        if let Err(e) = stores[i].store.abort(write) {
            failures.push(format!("{}: {e}", stores[i].label));
        }
    }
    failures
}

/// Step 2: writes `results` to every store in two phases (module doc). `Ok` lists the stores that
/// hold the study, in commit order.
#[allow(clippy::result_large_err)] // as for `check_plan`
pub fn write_study(stores: &mut [NamedStore], results: &av_cdm::pb::SweepResults) -> Result<Vec<Written>, StudyWriteError> {
    let order = commit_order(stores);

    // Phase 1: prepare every store; the first refusal aborts the rest.
    let mut prepared: Vec<(usize, PreparedWrite)> = Vec::new();
    for &i in &order {
        match stores[i].store.prepare(results) {
            Ok(write) => prepared.push((i, write)),
            Err(source) => {
                let abort_failures = abort_all(stores, prepared);
                return Err(StudyWriteError::PrepareFailed { store: stores[i].label.clone(), source, abort_failures });
            }
        }
    }

    // Phase 2: commit in order. A store that already held the study commits nothing.
    let mut done: Vec<Written> = Vec::new();
    let mut remaining = prepared.into_iter();
    while let Some((i, write)) = remaining.next() {
        let newly_written = matches!(write.state, crate::store::PreparedState::Staged(_));
        match stores[i].store.commit(write) {
            Ok(location) => done.push(Written { label: stores[i].label.clone(), kind: stores[i].store.kind(), location, newly_written }),
            Err(source) => {
                let abort_failures = abort_all(stores, remaining.collect());
                let store = stores[i].label.clone();
                return Err(if done.iter().any(|w| w.newly_written) {
                    StudyWriteError::PartialCommit { committed: done, failed: store, source, abort_failures }
                } else if matches!(source, StoreError::CommitOutcomeUnknown { .. }) {
                    StudyWriteError::CommitOutcomeUnknown { store, source, abort_failures }
                } else {
                    StudyWriteError::CommitFailed { store, source, abort_failures }
                });
            }
        }
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use av_cdm::pb;

    use super::*;
    use crate::store::{PreparedState, StudySummary, WriteRefusal};

    type Log = Arc<Mutex<Vec<String>>>;
    type Failure = fn() -> StoreError;

    /// What a fake store does at each call. `None` means the call succeeds.
    #[derive(Default, Clone, Copy)]
    struct Behaviour {
        check_plan: Option<Failure>,
        prepare: Option<Failure>,
        /// `prepare` answers [`PreparedState::Existing`]: the store already holds the study.
        existing: bool,
        commit: Option<Failure>,
        abort: Option<Failure>,
    }

    struct FakeStore {
        name: &'static str,
        kind: StoreKind,
        log: Log,
        behaviour: Behaviour,
    }

    impl StudyStore for FakeStore {
        fn kind(&self) -> StoreKind {
            self.kind
        }
        fn check_plan(&self, _plan: &StudyPlan) -> Result<(), StoreError> {
            self.log.lock().unwrap().push(format!("{}:check_plan", self.name));
            self.behaviour.check_plan.map_or(Ok(()), |f| Err(f()))
        }
        fn prepare(&mut self, results: &pb::SweepResults) -> Result<PreparedWrite, StoreError> {
            self.log.lock().unwrap().push(format!("{}:prepare", self.name));
            if let Some(f) = self.behaviour.prepare {
                return Err(f());
            }
            Ok(if self.behaviour.existing { PreparedWrite::existing(&results.sweep_id, format!("{}-existing", self.name)) } else { PreparedWrite::staged(&results.sweep_id, self.name) })
        }
        fn commit(&mut self, prepared: PreparedWrite) -> Result<String, StoreError> {
            self.log.lock().unwrap().push(format!("{}:commit", self.name));
            if let Some(f) = self.behaviour.commit {
                return Err(f());
            }
            Ok(match prepared.state {
                PreparedState::Existing { location } => location,
                PreparedState::Staged(_) => format!("{}-new", self.name),
            })
        }
        fn abort(&mut self, _prepared: PreparedWrite) -> Result<(), StoreError> {
            self.log.lock().unwrap().push(format!("{}:abort", self.name));
            self.behaviour.abort.map_or(Ok(()), |f| Err(f()))
        }
        fn get_study(&self, _sweep_id: &str) -> Result<Option<pb::SweepResults>, StoreError> {
            Ok(None)
        }
        fn list_studies(&self) -> Result<Vec<StudySummary>, StoreError> {
            Ok(Vec::new())
        }
    }

    fn io_error() -> StoreError {
        StoreError::Io { path: "/store/s".into(), source: std::io::Error::other("disk on fire") }
    }
    fn different_content() -> StoreError {
        StoreError::RefusedWrite { sweep_id: "s".to_string(), reason: WriteRefusal::DifferentContent { existing_write_id: "w".to_string(), existing_sha256: "a".to_string(), new_sha256: "b".to_string() } }
    }
    fn different_plan() -> StoreError {
        StoreError::RefusedWrite {
            sweep_id: "s".to_string(),
            reason: WriteRefusal::DifferentPlan {
                existing_write_id: "w".to_string(),
                existing_sha256: "a".to_string(),
                difference: Box::new(crate::plan::PlanDifference { field: "drm_hash".to_string(), existing: "x".to_string(), planned: "y".to_string() }),
            },
        }
    }
    fn unknown() -> StoreError {
        StoreError::CommitOutcomeUnknown { endpoint: "http://ch".to_string(), sweep_id: "s".to_string(), write_id: "w1".to_string(), insert: "failed: reset".to_string(), reread: "timeout".to_string() }
    }
    fn stage_in_the_way() -> StoreError {
        StoreError::Io { path: "/store/.incoming-1".into(), source: std::io::Error::other("directory not empty") }
    }

    /// `[file, clickhouse]` in that order (the order `--store-dir` then `--store-clickhouse` give
    /// them), so every test also proves the coordinator reorders them.
    fn stores(log: &Log, clickhouse: Behaviour, file: Behaviour) -> Vec<NamedStore> {
        let make = |name: &'static str, kind, behaviour| NamedStore { label: format!("the {name} store"), store: Box::new(FakeStore { name, kind, log: log.clone(), behaviour }) };
        vec![make("file", StoreKind::File, file), make("ch", StoreKind::ClickHouse, clickhouse)]
    }

    fn study() -> pb::SweepResults {
        pb::SweepResults { sweep_id: "s".to_string(), ..Default::default() }
    }

    fn calls(log: &Log) -> Vec<String> {
        log.lock().unwrap().clone()
    }

    fn run(clickhouse: Behaviour, file: Behaviour) -> (Result<Vec<Written>, StudyWriteError>, Vec<String>) {
        let log = Log::default();
        let mut stores = stores(&log, clickhouse, file);
        let result = write_study(&mut stores, &study());
        (result, calls(&log))
    }

    /// Table row "Both already hold identical content" and the plain success: the order of calls is
    /// ClickHouse prepared before the file store and committed before it, whatever order the
    /// stores were given in.
    #[test]
    fn both_phases_run_clickhouse_first_and_a_success_lists_both_stores() {
        let (result, log) = run(Behaviour::default(), Behaviour::default());
        assert_eq!(log, ["ch:prepare", "file:prepare", "ch:commit", "file:commit"]);
        let written = result.unwrap();
        assert_eq!(written.iter().map(|w| (w.kind, w.location.as_str(), w.newly_written)).collect::<Vec<_>>(), [(StoreKind::ClickHouse, "ch-new", true), (StoreKind::File, "file-new", true)]);
    }

    #[test]
    fn the_plan_check_asks_clickhouse_then_the_file_store_and_the_first_refusal_stops_it() {
        let log = Log::default();
        let plan = StudyPlan { sweep_id: "s".to_string(), sweep_hash: String::new(), drm_hash: String::new(), provenance: None, samples: Vec::new() };
        check_plan(&stores(&log, Behaviour::default(), Behaviour::default()), &plan).unwrap();
        assert_eq!(calls(&log), ["ch:check_plan", "file:check_plan"]);

        // Row "a store refuses the plan": typed, naming the store, and nothing else is called.
        let log = Log::default();
        let err = check_plan(&stores(&log, Behaviour::default(), Behaviour { check_plan: Some(different_plan), ..Default::default() }), &plan).unwrap_err();
        assert!(matches!(&err, StudyWriteError::PlanRefused { store, source: StoreError::RefusedWrite { reason: WriteRefusal::DifferentPlan { difference, .. }, .. } } if store == "the file store" && difference.field == "drm_hash"), "{err:?}");
        assert!(err.to_string().contains("the file store refuses this study up front") && err.to_string().contains("nothing was written to any store"), "{err}");
        assert_eq!(calls(&log), ["ch:check_plan", "file:check_plan"], "no prepare, no commit");
        assert!(!err.needs_repair());
    }

    /// Row "a store refuses or fails at prepare (either one)": every store prepared so far is
    /// aborted, nothing is committed, and the error names the store that refused.
    #[test]
    fn a_prepare_refusal_aborts_what_was_prepared_and_commits_nothing() {
        // The file store refuses after ClickHouse prepared: ClickHouse is aborted.
        let (result, log) = run(Behaviour::default(), Behaviour { prepare: Some(different_content), ..Default::default() });
        assert_eq!(log, ["ch:prepare", "file:prepare", "ch:abort"]);
        let err = result.unwrap_err();
        assert!(matches!(&err, StudyWriteError::PrepareFailed { store, source: StoreError::RefusedWrite { reason: WriteRefusal::DifferentContent { .. }, .. }, .. } if store == "the file store"), "{err:?}");
        assert!(!err.needs_repair());
        assert!(err.to_string().contains("the file store") && err.to_string().contains("nothing was made visible in any store"), "{err}");

        // ClickHouse fails first: nothing was prepared, so nothing is aborted and the file store is never touched.
        let (result, log) = run(Behaviour { prepare: Some(io_error), ..Default::default() }, Behaviour::default());
        assert_eq!(log, ["ch:prepare"]);
        assert!(matches!(result.unwrap_err(), StudyWriteError::PrepareFailed { store, .. } if store == "the ch store"));
    }

    #[test]
    fn an_abort_that_fails_is_reported_without_hiding_the_refusal() {
        let (result, log) = run(Behaviour { abort: Some(stage_in_the_way), ..Default::default() }, Behaviour { prepare: Some(different_content), ..Default::default() });
        assert_eq!(log, ["ch:prepare", "file:prepare", "ch:abort"]);
        let err = result.unwrap_err();
        match &err {
            StudyWriteError::PrepareFailed { abort_failures, .. } => assert!(abort_failures.len() == 1 && abort_failures[0].contains("the ch store") && abort_failures[0].contains(".incoming-1"), "{abort_failures:?}"),
            other => panic!("{other:?}"),
        }
        assert!(err.to_string().contains("could not be removed"), "{err}");
    }

    /// Rows "the first commit fails definitely" and "ClickHouse lost a race to a writer of
    /// different content": the file stage is aborted, the file store is never committed, and the
    /// store's own error comes back.
    #[test]
    fn a_failed_first_commit_aborts_the_file_stage_and_never_commits_the_file_store() {
        for (failure, label) in [(io_error as Failure, "an insert error"), (different_content as Failure, "a lost race")] {
            let (result, log) = run(Behaviour { commit: Some(failure), ..Default::default() }, Behaviour::default());
            assert_eq!(log, ["ch:prepare", "file:prepare", "ch:commit", "file:abort"], "{label}");
            let err = result.unwrap_err();
            assert!(matches!(&err, StudyWriteError::CommitFailed { store, .. } if store == "the ch store"), "{label}: {err:?}");
            assert!(!err.needs_repair(), "{label}");
            assert!(err.to_string().contains("nothing was made visible by this run in any store"), "{err}");
            assert_eq!(matches!(err.store_error(), StoreError::RefusedWrite { reason: WriteRefusal::DifferentContent { .. }, .. }), label == "a lost race");
        }
    }

    /// Row "header insert errors and the re-read also fails": a typed indeterminate error naming
    /// the write id and the repair; the file stage is aborted.
    #[test]
    fn an_unknown_clickhouse_commit_is_a_typed_indeterminate_error_with_the_repair() {
        let (result, log) = run(Behaviour { commit: Some(unknown), ..Default::default() }, Behaviour::default());
        assert_eq!(log, ["ch:prepare", "file:prepare", "ch:commit", "file:abort"]);
        let err = result.unwrap_err();
        assert!(matches!(&err, StudyWriteError::CommitOutcomeUnknown { store, source: StoreError::CommitOutcomeUnknown { .. }, .. } if store == "the ch store"), "{err:?}");
        assert!(err.needs_repair());
        let text = err.to_string();
        assert!(text.contains("write w1") && text.contains("unknown") && text.contains("--import-study") && text.contains("idempotent"), "{text}");
    }

    /// Row "ClickHouse committed, file rename fails": a typed partial-commit error naming both and
    /// the repair; there is nothing left to abort.
    #[test]
    fn a_file_commit_that_fails_after_clickhouse_committed_is_a_partial_commit() {
        let (result, log) = run(Behaviour::default(), Behaviour { commit: Some(io_error), ..Default::default() });
        assert_eq!(log, ["ch:prepare", "file:prepare", "ch:commit", "file:commit"]);
        let err = result.unwrap_err();
        match &err {
            StudyWriteError::PartialCommit { committed, failed, .. } => {
                assert_eq!(committed.iter().map(|w| (w.label.as_str(), w.location.as_str())).collect::<Vec<_>>(), [("the ch store", "ch-new")]);
                assert_eq!(failed, "the file store");
            }
            other => panic!("{other:?}"),
        }
        assert!(err.needs_repair());
        let text = err.to_string();
        assert!(text.contains("written to the ch store") && text.contains("the file store could not commit it") && text.contains("--import-study"), "{text}");
    }

    /// Rows "one already holds identical content, the other has none": the other is written; when
    /// that write fails nothing changed, so it is a plain commit failure, not a partial commit.
    #[test]
    fn a_store_that_already_holds_the_study_commits_nothing_and_does_not_make_a_failure_partial() {
        // ClickHouse already holds it; the file store is written.
        let (result, log) = run(Behaviour { existing: true, ..Default::default() }, Behaviour::default());
        assert_eq!(log, ["ch:prepare", "file:prepare", "ch:commit", "file:commit"]);
        let written = result.unwrap();
        assert_eq!(written.iter().map(|w| (w.location.as_str(), w.newly_written)).collect::<Vec<_>>(), [("ch-existing", false), ("file-new", true)]);

        // ... and when the file store's commit then fails, this run changed nothing.
        let (result, _) = run(Behaviour { existing: true, ..Default::default() }, Behaviour { commit: Some(io_error), ..Default::default() });
        let err = result.unwrap_err();
        assert!(matches!(&err, StudyWriteError::CommitFailed { store, .. } if store == "the file store"), "{err:?}");
        assert!(!err.needs_repair());

        // The file store already holds it; ClickHouse is written.
        let (result, _) = run(Behaviour::default(), Behaviour { existing: true, ..Default::default() });
        assert_eq!(result.unwrap().iter().map(|w| w.newly_written).collect::<Vec<_>>(), [true, false]);

        // Both hold it.
        let (result, log) = run(Behaviour { existing: true, ..Default::default() }, Behaviour { existing: true, ..Default::default() });
        assert_eq!(result.unwrap().iter().map(|w| w.newly_written).collect::<Vec<_>>(), [false, false]);
        assert_eq!(log, ["ch:prepare", "file:prepare", "ch:commit", "file:commit"], "their commits are the no-ops of an existing copy");
    }
}
