//! The two-phase write of one study to the real file store and a stateful fake ClickHouse
//! (question 254 item 2): `av_sweep::study_write`, one test per row of its outcome table, each
//! asserting what **both** stores show a reader afterwards (`get_study` on each, the file root's
//! directory listing, no `.incoming-*` stage left) and, where the stores can disagree, that the
//! repair (the same write again, which is what `--import-study` does) converges.
//!
//! The fake keeps what is inserted and answers reads from it (`tests/support`), and can be told to
//! fail the insert into a table, to lose the reply of the next header insert (stored, but the
//! client never hears), or to fail every read. The coordinator's order of calls and abort on every
//! path are proven with a scripted store in `src/study_write.rs`'s own tests.

mod support;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use av_cdm::pb;
use av_sweep::clickhouse::schema;
use av_sweep::clickhouse::{content_sha256, ClickHouseStudyStore};
use av_sweep::plan::StudyPlan;
use av_sweep::store::{FileStudyStore, StoreError, StudyStore, WriteRefusal};
use av_sweep::study_write::{check_plan, write_study, NamedStore, StudyWriteError};
use support::{one_sample_study, Hooked, StatefulCh};

const ID: &str = "two-phase";

fn temp_root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("av-sweep-two-phase-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// A study that differs from `one_sample_study` in one sample field.
fn other_study(id: &str) -> pb::SweepResults {
    let mut s = one_sample_study(id);
    s.samples[0].products_uri = "/another/out-dir/sample_p1_d2".to_string();
    s
}

/// A rig: the real file store under `root`, and the stateful fake ClickHouse.
struct Rig {
    root: PathBuf,
    ch: Arc<StatefulCh>,
}

impl Rig {
    fn new(name: &str) -> Rig {
        Rig { root: temp_root(name), ch: Arc::new(StatefulCh::new()) }
    }

    fn file(&self) -> Box<dyn StudyStore> {
        Box::new(FileStudyStore::new(&self.root))
    }

    fn clickhouse(&self) -> ClickHouseStudyStore {
        ClickHouseStudyStore::connect(&self.ch.config()).expect("connect to the fake ClickHouse")
    }

    /// Both stores, as study mode opens them (file first).
    fn stores(&self) -> Vec<NamedStore> {
        vec![NamedStore { label: "the file store".to_string(), store: self.file() }, NamedStore { label: "the ClickHouse store".to_string(), store: Box::new(self.clickhouse()) }]
    }

    /// The same with `file` replaced by a hooked wrapper of the real file store.
    fn stores_with_hooked_file(&self, hook: impl FnOnce() + Send + 'static) -> Vec<NamedStore> {
        vec![NamedStore { label: "the file store".to_string(), store: Box::new(Hooked::new(self.file(), hook)) }, NamedStore { label: "the ClickHouse store".to_string(), store: Box::new(self.clickhouse()) }]
    }

    fn stores_with_hooked_clickhouse(&self, hook: impl FnOnce() + Send + 'static) -> Vec<NamedStore> {
        vec![NamedStore { label: "the file store".to_string(), store: self.file() }, NamedStore { label: "the ClickHouse store".to_string(), store: Box::new(Hooked::new(Box::new(self.clickhouse()), hook)) }]
    }

    /// What a reader of the file store sees.
    fn file_view(&self) -> Option<pb::SweepResults> {
        FileStudyStore::new(&self.root).get_study(ID).expect("reading the file store")
    }

    /// What a reader of ClickHouse sees (a fresh connection, reads working).
    fn ch_view(&self) -> Option<pb::SweepResults> {
        self.clickhouse().get_study(ID).expect("reading ClickHouse")
    }

    /// The names in the file root: the study directory and, if one is left, a `.incoming-*` stage.
    fn root_listing(&self) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(&self.root) else { return Vec::new() };
        let mut names: Vec<String> = entries.map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }

    fn assert_state(&self, file: Option<&pb::SweepResults>, clickhouse: Option<&pb::SweepResults>, what: &str) {
        assert_eq!(self.file_view().as_ref(), file, "{what}: the file store");
        assert_eq!(self.ch_view().as_ref(), clickhouse, "{what}: ClickHouse");
        assert!(!self.root_listing().iter().any(|n| n.starts_with(".incoming-")), "{what}: a stage was left in the file root: {:?}", self.root_listing());
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn refused_plan_field(err: &StudyWriteError) -> &str {
    match err.store_error() {
        StoreError::RefusedWrite { reason: WriteRefusal::DifferentPlan { difference, .. }, .. } => &difference.field,
        other => panic!("expected RefusedWrite/DifferentPlan, got {other:?}"),
    }
}

/// Row "a store refuses the plan (up front)": either store holding a different study refuses,
/// naming itself and the first differing field, and nothing is written to either.
#[test]
fn a_store_holding_a_different_study_refuses_the_plan_and_neither_store_is_written() {
    let planned = one_sample_study(ID);
    let plan = StudyPlan::exact(&planned);
    let held = other_study(ID);

    // The file store holds the other study.
    let rig = Rig::new("plan-file");
    rig.file().put_study(&held).unwrap();
    let err = check_plan(&rig.stores(), &plan).unwrap_err();
    assert!(matches!(&err, StudyWriteError::PlanRefused { store, .. } if store == "the file store"), "{err:?}");
    assert_eq!(refused_plan_field(&err), "samples[p1_d2].products_uri");
    assert!(err.to_string().contains("the file store") && err.to_string().contains("studies are immutable") && err.to_string().contains("nothing was written to any store"), "{err}");
    rig.assert_state(Some(&held), None, "file store refused");
    assert_eq!(rig.ch.row_count(schema::SAMPLES, ID), 0, "ClickHouse was never written");

    // ClickHouse holds the other study.
    let rig = Rig::new("plan-ch");
    rig.clickhouse().put_study(&held).unwrap();
    let rows = rig.ch.row_count(schema::SAMPLES, ID);
    let err = check_plan(&rig.stores(), &plan).unwrap_err();
    assert!(matches!(&err, StudyWriteError::PlanRefused { store, .. } if store == "the ClickHouse store"), "{err:?}");
    assert_eq!(refused_plan_field(&err), "samples[p1_d2].products_uri");
    rig.assert_state(None, Some(&held), "ClickHouse refused");
    assert_eq!(rig.ch.row_count(schema::SAMPLES, ID), rows, "ClickHouse was not written");
    assert!(rig.root_listing().is_empty(), "the file root was not even created");

    // An identical study in both is compatible.
    let rig = Rig::new("plan-same");
    write_study(&mut rig.stores(), &planned).unwrap();
    check_plan(&rig.stores(), &plan).expect("an identical rerun is compatible");
}

/// The file store's id refusals reach the plan check too: an id that is not a path component and
/// an id that case-folds onto a stored one are refused before anything is written.
#[test]
fn the_file_stores_id_refusals_are_plan_refusals() {
    let rig = Rig::new("plan-ids");
    rig.file().put_study(&one_sample_study("study")).unwrap();
    for (id, want) in [("../x", "not a valid sweep_id"), ("Study", "collides with")] {
        let err = check_plan(&rig.stores(), &StudyPlan::exact(&one_sample_study(id))).unwrap_err();
        assert!(matches!(&err, StudyWriteError::PlanRefused { store, .. } if store == "the file store"), "{id}: {err:?}");
        assert!(err.to_string().contains(want), "{id}: {err}");
    }
    assert_eq!(rig.root_listing(), ["study"]);
    assert_eq!(rig.ch.row_count(schema::SAMPLES, "../x"), 0);
}

/// Rows "a store refuses at prepare" and "a store fails at prepare", for both stores: the error
/// names the store, the other store's stage is aborted, and neither store shows anything new.
#[test]
fn a_store_refusing_or_failing_at_prepare_leaves_both_stores_as_they_were() {
    let study = one_sample_study(ID);
    let held = other_study(ID);

    // ClickHouse refuses at prepare (a different study is there; no plan check ran first).
    let rig = Rig::new("prepare-ch-refuses");
    rig.clickhouse().put_study(&held).unwrap();
    let err = write_study(&mut rig.stores(), &study).unwrap_err();
    assert!(matches!(&err, StudyWriteError::PrepareFailed { store, source: StoreError::RefusedWrite { reason: WriteRefusal::DifferentContent { .. }, .. }, .. } if store == "the ClickHouse store"), "{err:?}");
    rig.assert_state(None, Some(&held), "ClickHouse refused at prepare");
    assert!(rig.root_listing().is_empty(), "the file store was never reached");

    // The file store refuses at prepare, after ClickHouse staged its rows: they are aborted, which
    // for ClickHouse means they stay invisible orphans.
    let rig = Rig::new("prepare-file-refuses");
    rig.file().put_study(&held).unwrap();
    let err = write_study(&mut rig.stores(), &study).unwrap_err();
    assert!(matches!(&err, StudyWriteError::PrepareFailed { store, source: StoreError::RefusedWrite { reason: WriteRefusal::DifferentContent { .. }, .. }, .. } if store == "the file store"), "{err:?}");
    rig.assert_state(Some(&held), None, "the file store refused at prepare");
    assert_eq!(rig.ch.orphan_write_ids(ID).len(), 1, "ClickHouse holds the staged rows as one invisible orphan write");
    assert_eq!(rig.root_listing(), [ID.to_string()], "only the stored study, no stage");

    // ClickHouse fails at prepare (the insert into the samples table is refused by the server).
    let rig = Rig::new("prepare-ch-fails");
    rig.ch.fail_inserts_into(schema::SAMPLES);
    let err = write_study(&mut rig.stores(), &study).unwrap_err();
    assert!(matches!(&err, StudyWriteError::PrepareFailed { store, source: StoreError::RefusedWrite { reason: WriteRefusal::Server { code: Some(497), .. }, .. }, .. } if store == "the ClickHouse store"), "{err:?}");
    assert!(err.to_string().contains("the ClickHouse store") && err.to_string().contains("preparing"), "{err}");
    rig.assert_state(None, None, "ClickHouse failed at prepare");
    assert!(rig.root_listing().is_empty());

    // The file store fails at prepare (its root is under a regular file): ClickHouse's rows are
    // aborted (orphans, invisible) and nothing is visible anywhere.
    let rig = Rig::new("prepare-file-fails");
    std::fs::write(&rig.root, b"not a directory").unwrap();
    let err = write_study(&mut rig.stores(), &study).unwrap_err();
    assert!(matches!(&err, StudyWriteError::PrepareFailed { store, source: StoreError::Io { .. }, .. } if store == "the file store"), "{err:?}");
    assert_eq!(rig.ch_view(), None, "ClickHouse shows no study");
    assert_eq!(rig.ch.orphan_write_ids(ID).len(), 1);
    assert_eq!(std::fs::read(&rig.root).unwrap(), b"not a directory", "the file in the way is untouched");
    let _ = std::fs::remove_file(&rig.root);
}

/// Row "ClickHouse header insert fails, and the re-read shows no header of ours": a typed error,
/// the file stage removed, neither store shows the study.
#[test]
fn a_failed_header_insert_with_no_header_of_ours_removes_the_file_stage_and_changes_nothing() {
    let rig = Rig::new("header-fails");
    rig.ch.fail_inserts_into(schema::STUDIES);
    let err = write_study(&mut rig.stores(), &one_sample_study(ID)).unwrap_err();
    assert!(matches!(&err, StudyWriteError::CommitFailed { store, source: StoreError::RefusedWrite { reason: WriteRefusal::Server { .. }, .. }, .. } if store == "the ClickHouse store"), "{err:?}");
    assert!(!err.needs_repair());
    rig.assert_state(None, None, "the header insert failed");
    assert!(rig.root_listing().is_empty(), "the file store's stage was removed and nothing was published");
    assert_eq!(rig.ch.orphan_write_ids(ID).len(), 1, "ClickHouse keeps the headerless rows, invisibly");
}

/// Row "ClickHouse header insert errors and the re-read also fails (outcome unknown)": a typed
/// indeterminate error naming the write id and the repair, the file stage removed; ClickHouse may
/// or may not hold the study (both are shown), the file store does not. The repair converges.
#[test]
fn an_unknown_header_commit_is_typed_leaves_the_file_store_empty_and_the_repair_converges() {
    let study = one_sample_study(ID);

    // The header landed but its reply was lost, and then the reads fail: ClickHouse holds it.
    let rig = Rig::new("unknown-landed");
    let (ch, hook_ch) = (rig.ch.clone(), rig.ch.clone());
    let mut stores = rig.stores_with_hooked_clickhouse(move || {
        hook_ch.lose_next_header_reply();
        hook_ch.fail_reads(true);
    });
    let err = write_study(&mut stores, &study).unwrap_err();
    let StudyWriteError::CommitOutcomeUnknown { store, source: StoreError::CommitOutcomeUnknown { write_id, .. }, .. } = &err else { panic!("{err:?}") };
    assert_eq!(store, "the ClickHouse store");
    assert_eq!(write_id.len(), 32, "the write id is named");
    assert!(err.needs_repair() && err.to_string().contains(write_id.as_str()) && err.to_string().contains("--import-study"), "{err}");
    ch.fail_reads(false);
    rig.assert_state(None, Some(&study), "unknown, and it had landed: ClickHouse holds the study, the file store does not");
    // The repair: the same write again. ClickHouse already holds it (no rows written), the file store gets it.
    let rows = rig.ch.row_count(schema::SAMPLES, ID);
    let written = write_study(&mut rig.stores(), &study).expect("the repair");
    assert_eq!(written.iter().map(|w| w.newly_written).collect::<Vec<_>>(), [false, true], "[ClickHouse, file]: ClickHouse already held it, the file store was written");
    assert_eq!(rig.ch.row_count(schema::SAMPLES, ID), rows, "ClickHouse got no new rows");
    rig.assert_state(Some(&study), Some(&study), "after the repair");

    // The header insert failed and the reads fail: ClickHouse holds nothing. The repair writes both.
    let rig = Rig::new("unknown-not-landed");
    rig.ch.fail_inserts_into(schema::STUDIES);
    let hook_ch = rig.ch.clone();
    let mut stores = rig.stores_with_hooked_clickhouse(move || hook_ch.fail_reads(true));
    let err = write_study(&mut stores, &study).unwrap_err();
    assert!(matches!(&err, StudyWriteError::CommitOutcomeUnknown { .. }), "{err:?}");
    rig.ch.clear_faults(); // the server is back
    rig.assert_state(None, None, "unknown, and it had not landed");
    let written = write_study(&mut rig.stores(), &study).expect("the repair");
    assert_eq!(written.iter().map(|w| w.newly_written).collect::<Vec<_>>(), [true, true]);
    rig.assert_state(Some(&study), Some(&study), "after the repair");
}

/// A header insert whose reply is lost but whose header landed, with reads working, is resolved
/// by the re-read: the commit succeeded and the file store is committed after it.
#[test]
fn a_lost_header_reply_that_the_reread_resolves_as_committed_is_a_success() {
    let rig = Rig::new("lost-reply");
    let study = one_sample_study(ID);
    let hook_ch = rig.ch.clone();
    let mut stores = rig.stores_with_hooked_clickhouse(move || hook_ch.lose_next_header_reply());
    let written = write_study(&mut stores, &study).expect("our header is there: committed");
    assert_eq!(written.len(), 2);
    rig.assert_state(Some(&study), Some(&study), "lost reply, resolved");
    assert_eq!(rig.ch.row_count(schema::STUDIES, ID), 1, "the header was inserted once, not retried");
}

/// Row "ClickHouse lost a race to a writer of different content": `DifferentContent`, the file
/// stage removed; ClickHouse holds the other writer's study and the file store holds nothing.
#[test]
fn losing_the_clickhouse_race_to_a_different_study_removes_the_file_stage() {
    let rig = Rig::new("race");
    let study = one_sample_study(ID);
    let other = other_study(ID);
    let competitor = rig.clickhouse();
    let competing = other.clone();
    let mut stores = rig.stores_with_hooked_clickhouse(move || {
        // A second writer's complete study lands between our prepare and our commit.
        competitor.write_rows_for_tests(&competing, true).expect("the competitor writes");
    });
    let err = write_study(&mut stores, &study).unwrap_err();
    match &err {
        StudyWriteError::CommitFailed { store, source: StoreError::RefusedWrite { reason: WriteRefusal::DifferentContent { existing_sha256, new_sha256, .. }, .. }, .. } => {
            assert_eq!(store, "the ClickHouse store");
            assert_eq!((existing_sha256.as_str(), new_sha256.as_str()), (content_sha256(&other).as_str(), content_sha256(&study).as_str()));
        }
        other => panic!("{other:?}"),
    }
    assert!(!err.needs_repair(), "nothing is visible that was not already: the other writer's study");
    rig.assert_state(None, Some(&other), "lost the race");
    assert!(rig.root_listing().is_empty());
}

/// Row "ClickHouse committed, file rename fails": a typed partial-commit error naming both and
/// the repair; ClickHouse holds the study and the file store does not, no stage is left. After the
/// cause is removed the same write converges.
#[test]
fn a_file_rename_that_fails_after_clickhouse_committed_is_a_partial_commit_and_the_repair_converges() {
    let rig = Rig::new("partial");
    let study = one_sample_study(ID);
    let in_the_way = rig.root.join(ID);
    let blocker = in_the_way.clone();
    // A non-empty directory that is no study appears where the stage would be renamed to.
    let mut stores = rig.stores_with_hooked_file(move || {
        std::fs::create_dir_all(&blocker).unwrap();
        std::fs::write(blocker.join("notes.txt"), b"mine").unwrap();
    });
    let err = write_study(&mut stores, &study).unwrap_err();
    match &err {
        StudyWriteError::PartialCommit { committed, failed, source, .. } => {
            assert_eq!(committed.iter().map(|w| w.label.as_str()).collect::<Vec<_>>(), ["the ClickHouse store"]);
            assert_eq!(failed, "the file store");
            assert!(matches!(source, StoreError::Io { path, .. } if path.ends_with(ID)), "{source:?}");
        }
        other => panic!("{other:?}"),
    }
    assert!(err.needs_repair());
    let text = err.to_string();
    assert!(text.contains("written to the ClickHouse store") && text.contains("the file store could not commit it") && text.contains("--import-study"), "{text}");
    rig.assert_state(None, Some(&study), "partial commit");
    assert_eq!(std::fs::read(in_the_way.join("notes.txt")).unwrap(), b"mine", "the foreign directory is untouched");
    assert_eq!(rig.root_listing(), [ID.to_string()], "no stage left");

    // The operator removes what was in the way; the same write converges.
    std::fs::remove_dir_all(&in_the_way).unwrap();
    let rows = rig.ch.row_count(schema::SAMPLES, ID);
    let written = write_study(&mut rig.stores(), &study).expect("the repair");
    assert_eq!(written.iter().map(|w| w.newly_written).collect::<Vec<_>>(), [false, true]);
    assert_eq!(rig.ch.row_count(schema::SAMPLES, ID), rows, "ClickHouse was not written again");
    rig.assert_state(Some(&study), Some(&study), "after the repair");
}

/// Row "ClickHouse committed, file rename fails" by a **permission** failure: the file root is made
/// read-only between prepare and commit (`chmod 555`, done by a hook just before the file store's
/// commit), so the rename of the already-staged directory fails with `EACCES` on this host. A
/// typed partial commit; ClickHouse holds the study, the file store does not. The failed commit
/// cannot remove its stage from a read-only root, so one stray `.incoming-*` stays (the module doc
/// of `store.rs`: a crash leaves the same; `list_studies` skips it). After the permissions are
/// restored the same write converges.
#[test]
fn a_permission_failure_on_the_rename_after_clickhouse_committed_is_a_partial_commit_and_the_repair_converges() {
    use std::os::unix::fs::PermissionsExt;
    let rig = Rig::new("partial-permission");
    let study = one_sample_study(ID);

    // The premise, measured: a directory with mode 555 refuses a new entry for this user (it does
    // not for root, where the test says it was skipped).
    let probe = temp_root("partial-permission-probe");
    std::fs::create_dir_all(&probe).unwrap();
    std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o555)).unwrap();
    let enforced = std::fs::write(probe.join("x"), b"").is_err();
    std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o755)).unwrap();
    let _ = std::fs::remove_dir_all(&probe);
    if !enforced {
        eprintln!("SKIPPED a_permission_failure_on_the_rename_…: this user is not held to directory permissions (root?)");
        return;
    }

    let root = rig.root.clone();
    let mut stores = rig.stores_with_hooked_file(move || std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o555)).unwrap());
    let result = write_study(&mut stores, &study);
    std::fs::set_permissions(&rig.root, std::fs::Permissions::from_mode(0o755)).unwrap(); // always restore
    let err = result.unwrap_err();
    match &err {
        StudyWriteError::PartialCommit { committed, failed, source, .. } => {
            assert_eq!(committed.iter().map(|w| w.label.as_str()).collect::<Vec<_>>(), ["the ClickHouse store"]);
            assert_eq!(failed, "the file store");
            assert!(matches!(source, StoreError::Io { source, .. } if source.kind() == std::io::ErrorKind::PermissionDenied), "{source:?}");
        }
        other => panic!("{other:?}"),
    }
    assert!(err.needs_repair() && err.to_string().contains("--import-study"), "{err}");
    assert_eq!(rig.file_view(), None, "the file store shows no study");
    assert_eq!(rig.ch_view().as_ref(), Some(&study), "ClickHouse holds it");
    let stray: Vec<String> = rig.root_listing();
    assert!(stray.len() == 1 && stray[0].starts_with(".incoming-"), "only the stray stage: {stray:?}");
    assert!(FileStudyStore::new(&rig.root).list_studies().unwrap().is_empty(), "list_studies skips the stray stage");

    // Permissions are back: the same write converges (a new stage is built and renamed).
    let rows = rig.ch.row_count(schema::SAMPLES, ID);
    let written = write_study(&mut rig.stores(), &study).expect("the repair");
    assert_eq!(written.iter().map(|w| w.newly_written).collect::<Vec<_>>(), [false, true]);
    assert_eq!(rig.ch.row_count(schema::SAMPLES, ID), rows);
    assert_eq!(rig.file_view().as_ref(), Some(&study));
    assert_eq!(rig.ch_view().as_ref(), Some(&study));
    std::fs::remove_dir_all(rig.root.join(&stray[0])).unwrap(); // the operator deletes the stray stage
}

/// Question 254 item 2, proposal A: a non-empty directory that holds no study where the study
/// goes is the most plausible rename failure, and it is now refused at the file store's `prepare`
/// (and by its `check_plan`): a clean `PrepareFailed`, with ClickHouse's headerless rows aborted.
/// ClickHouse is never committed (no header row, no study) and the file root is byte for byte as
/// it was.
#[test]
fn an_occupied_file_target_is_a_clean_prepare_failure_and_clickhouse_is_never_committed() {
    let rig = Rig::new("occupied");
    let study = one_sample_study(ID);
    std::fs::create_dir_all(rig.root.join(ID)).unwrap();
    std::fs::write(rig.root.join(ID).join("notes.txt"), b"mine").unwrap();
    let before = rig.root_listing();

    // Up front.
    let err = check_plan(&rig.stores(), &StudyPlan::exact(&study)).unwrap_err();
    assert!(matches!(&err, StudyWriteError::PlanRefused { store, source: StoreError::Io { path, .. } } if store == "the file store" && path.ends_with(ID)), "{err:?}");

    // Straight into the two-phase write (no plan check ran).
    let err = write_study(&mut rig.stores(), &study).unwrap_err();
    assert!(matches!(&err, StudyWriteError::PrepareFailed { store, source: StoreError::Io { path, .. }, .. } if store == "the file store" && path.ends_with(ID)), "{err:?}");
    assert!(!err.needs_repair(), "nothing was committed anywhere");
    assert_eq!(rig.ch.row_count(schema::STUDIES, ID), 0, "ClickHouse never got a header row");
    assert_eq!(rig.ch_view(), None);
    assert_eq!(rig.ch.orphan_write_ids(ID).len(), 1, "only the aborted, invisible rows");
    assert_eq!(rig.root_listing(), before, "the file root is unchanged: no stage, no study");
    assert_eq!(std::fs::read(rig.root.join(ID).join("notes.txt")).unwrap(), b"mine");
    assert_eq!(rig.file_view(), None);
}

/// Proposal B: the winner re-read is retried (3 attempts in all, 200 ms apart), the header insert
/// never. A lost reply whose next read recovers resolves as committed; a definite failure whose
/// next read recovers resolves as not committed; neither is indeterminate.
#[test]
fn a_reread_that_recovers_on_a_retry_resolves_the_commit_without_an_indeterminate_error() {
    let study = one_sample_study(ID);

    // The header landed, its reply was lost, the first re-read fails and the second works: committed.
    let rig = Rig::new("reread-committed");
    let hook_ch = rig.ch.clone();
    let mut stores = rig.stores_with_hooked_clickhouse(move || {
        hook_ch.lose_next_header_reply();
        hook_ch.fail_next_header_reads(1);
    });
    let written = write_study(&mut stores, &study).expect("committed, found on the second read");
    assert_eq!(written.len(), 2);
    let (inserts, reads) = rig.ch.header_requests();
    assert_eq!(inserts, 1, "the header INSERT is never retried");
    assert_eq!(reads, 1 + 2, "one read at prepare, then the re-read: one failure and one success");
    rig.assert_state(Some(&study), Some(&study), "resolved as committed");

    // The header insert failed, the first re-read fails and the second works and shows no header: not committed.
    let rig = Rig::new("reread-not-committed");
    rig.ch.fail_inserts_into(schema::STUDIES);
    let hook_ch = rig.ch.clone();
    let mut stores = rig.stores_with_hooked_clickhouse(move || hook_ch.fail_next_header_reads(2));
    let err = write_study(&mut stores, &study).unwrap_err();
    assert!(matches!(&err, StudyWriteError::CommitFailed { store, source: StoreError::RefusedWrite { reason: WriteRefusal::Server { .. }, .. }, .. } if store == "the ClickHouse store"), "{err:?}");
    assert!(!err.needs_repair());
    let (inserts, reads) = rig.ch.header_requests();
    assert_eq!(inserts, 1);
    assert_eq!(reads, 1 + 3, "two failed re-reads, the third answers");
    rig.assert_state(None, None, "resolved as not committed");
}

/// Proposal B: a re-read that fails every attempt is still `CommitOutcomeUnknown`, after exactly
/// 3 re-reads and no second insert, with the repair in the message, and the delay is the fixed
/// backoff (2 x 200 ms) and not more.
#[test]
fn a_reread_that_fails_every_attempt_is_unknown_after_exactly_three_reads_and_one_insert() {
    use std::time::{Duration, Instant};
    assert_eq!((av_sweep::clickhouse::REREAD_ATTEMPTS, av_sweep::clickhouse::REREAD_BACKOFF), (3, Duration::from_millis(200)));
    let rig = Rig::new("reread-unknown");
    let study = one_sample_study(ID);
    let hook_ch = rig.ch.clone();
    let mut stores = rig.stores_with_hooked_clickhouse(move || hook_ch.lose_the_header_reply_and_then_fail_every_read());
    let began = Instant::now();
    let err = write_study(&mut stores, &study).unwrap_err();
    let took = began.elapsed();
    let StudyWriteError::CommitOutcomeUnknown { store, source: StoreError::CommitOutcomeUnknown { write_id, reread, .. }, .. } = &err else { panic!("{err:?}") };
    assert_eq!(store, "the ClickHouse store");
    assert!(reread.contains("after 3 attempts, 200 ms apart"), "{reread}");
    let text = err.to_string();
    assert!(err.needs_repair() && text.contains(write_id.as_str()) && text.contains("--import-study"), "{text}");
    let (inserts, reads) = rig.ch.header_requests();
    assert_eq!(inserts, 1, "no insert retry");
    assert_eq!(reads, 1 + 3, "one read at prepare and exactly three re-reads");
    assert!(took >= Duration::from_millis(400), "two 200 ms pauses happened: {took:?}");
    assert!(took < Duration::from_secs(5), "and nothing else was waited for: {took:?}");
    // The file store did not commit, and no stage is left.
    rig.ch.clear_faults();
    rig.assert_state(None, Some(&study), "unknown, and it had landed");
}

/// Rows "both already hold identical content" and "one already holds identical content, the other
/// has none", and the plain empty-to-both write.
#[test]
fn identical_content_is_idempotent_and_the_missing_store_is_filled() {
    let study = one_sample_study(ID);

    // Neither holds it: both are written, and the file root holds exactly the study directory.
    let rig = Rig::new("fresh");
    let written = write_study(&mut rig.stores(), &study).unwrap();
    assert_eq!(written.iter().map(|w| w.newly_written).collect::<Vec<_>>(), [true, true]);
    rig.assert_state(Some(&study), Some(&study), "fresh write");
    assert_eq!(rig.root_listing(), [ID.to_string()]);

    // Both hold it: nothing is written.
    let (rows, headers) = (rig.ch.row_count(schema::SAMPLES, ID), rig.ch.row_count(schema::STUDIES, ID));
    let pb_before = std::fs::read(rig.root.join(ID).join("sweep_results.pb")).unwrap();
    let again = write_study(&mut rig.stores(), &study).unwrap();
    assert_eq!(again.iter().map(|w| w.newly_written).collect::<Vec<_>>(), [false, false]);
    assert_eq!((rig.ch.row_count(schema::SAMPLES, ID), rig.ch.row_count(schema::STUDIES, ID)), (rows, headers), "ClickHouse got no rows");
    assert_eq!(std::fs::read(rig.root.join(ID).join("sweep_results.pb")).unwrap(), pb_before);
    rig.assert_state(Some(&study), Some(&study), "second write");

    // ClickHouse holds it, the file store does not: only the file store is written; and when that
    // write fails nothing changes (ClickHouse already held the study before the run).
    let rig = Rig::new("ch-only");
    rig.clickhouse().put_study(&study).unwrap();
    let rows = rig.ch.row_count(schema::SAMPLES, ID);
    let blocker = rig.root.join(ID);
    let mut stores = rig.stores_with_hooked_file(move || {
        std::fs::create_dir_all(&blocker).unwrap();
        std::fs::write(blocker.join("notes.txt"), b"mine").unwrap();
    });
    let err = write_study(&mut stores, &study).unwrap_err();
    assert!(matches!(&err, StudyWriteError::CommitFailed { store, .. } if store == "the file store"), "{err:?}");
    assert!(!err.needs_repair(), "this run changed nothing");
    rig.assert_state(None, Some(&study), "the file store's write failed");
    assert_eq!(rig.ch.row_count(schema::SAMPLES, ID), rows);
    std::fs::remove_dir_all(rig.root.join(ID)).unwrap();
    let written = write_study(&mut rig.stores(), &study).unwrap();
    assert_eq!(written.iter().map(|w| w.newly_written).collect::<Vec<_>>(), [false, true]);
    rig.assert_state(Some(&study), Some(&study), "ClickHouse held it, the file store was filled");

    // The file store holds it, ClickHouse does not: only ClickHouse is written.
    let rig = Rig::new("file-only");
    rig.file().put_study(&study).unwrap();
    let written = write_study(&mut rig.stores(), &study).unwrap();
    assert_eq!(written.iter().map(|w| w.newly_written).collect::<Vec<_>>(), [true, false]);
    rig.assert_state(Some(&study), Some(&study), "the file store held it, ClickHouse was filled");
}

/// A prepared file write that is aborted removes its stage and publishes nothing; one that is
/// dropped is what a crash leaves (a stage `list_studies` skips). The real file store alone.
#[test]
fn an_aborted_file_stage_is_removed_and_a_committed_one_is_published_whole() {
    let root = temp_root("file-abort");
    let mut store = FileStudyStore::new(&root);
    let study = one_sample_study(ID);
    let prepared = store.prepare(&study).unwrap();
    let stage_names = |root: &Path| -> Vec<String> { std::fs::read_dir(root).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect() };
    assert!(stage_names(&root).iter().all(|n| n.starts_with(".incoming-")), "staged, not published: {:?}", stage_names(&root));
    assert_eq!(store.get_study(ID).unwrap(), None, "a reader sees nothing while the write is only prepared");
    assert!(store.list_studies().unwrap().is_empty());
    store.abort(prepared).unwrap();
    assert!(stage_names(&root).is_empty());

    let prepared = store.prepare(&study).unwrap();
    let location = store.commit(prepared).unwrap();
    assert!(location.ends_with(ID));
    assert_eq!(stage_names(&root), [ID.to_string()]);
    assert_eq!(store.get_study(ID).unwrap().unwrap(), study);
    let _ = std::fs::remove_dir_all(&root);
}
