//! The `av-sweep` binary against two study stores at once (question 254 item 2): the stores are
//! checked up front, before the first sample runs, and are never left holding different studies.
//! The file store is a real directory; ClickHouse is the stateful fake (`tests/support`), which
//! the binary reaches over loopback HTTP. No GMAT: study mode runs `drms/leo_orbital_native_sweep.*`
//! (12 pure-Rust samples); store mode is `--import-study`.
//!
//! - a store holding a different study refuses before any sample runs (each store in turn): no
//!   `sample_*` directory and no `sweep_results.pb` in `--out-dir`, the other store untouched, the
//!   message naming the store and the first differing field;
//! - `--import-study` whose ClickHouse commit fails leaves the file store unchanged;
//! - a file rename that fails after ClickHouse committed is the typed partial-commit message with
//!   the repair command, and that command (`--import-study` of the `sweep_results.pb` study mode
//!   wrote first) converges: both stores hold the identical study.

mod support;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use av_cdm::pb;
use av_sweep::clickhouse::schema;
use av_sweep::clickhouse::ClickHouseStudyStore;
use av_sweep::store::{FileStudyStore, StudyStore};
use prost::Message;
use support::{one_sample_study, StatefulCh};

const ID: &str = "leo_orbital_native_sweep";

fn av_sweep_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_av-sweep"))
}

fn drms_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../drms").join(name)
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("av-sweep-store-agreement-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Every file and directory under `dir` (relative path to kind and size).
fn tree(dir: &Path) -> BTreeMap<String, String> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let meta = std::fs::symlink_metadata(entry.path()).unwrap();
            let rel = entry.path().strip_prefix(base).unwrap().display().to_string();
            if meta.is_dir() {
                out.insert(rel, "dir".to_string());
                walk(base, &entry.path(), out);
            } else {
                out.insert(rel, format!("{} bytes", meta.len()));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

fn clickhouse_flags(ch: &StatefulCh) -> Vec<String> {
    vec!["--store-clickhouse".into(), ch.fake.url(), "--clickhouse-database".into(), "studies".into(), "--clickhouse-user".into(), "avsweep".into()]
}

fn run_native_study(out_dir: &Path, store_dir: Option<&Path>, ch: Option<&StatefulCh>) -> Output {
    let mut cmd = Command::new(av_sweep_bin());
    cmd.arg("--sweep").arg(drms_path("leo_orbital_native_sweep.sweep.yaml"));
    cmd.arg("--drm").arg(drms_path("leo_orbital_native_sweep.drm.yaml"));
    cmd.arg("--sos").arg(drms_path("leo_orbital_native_sweep.sos.yaml"));
    cmd.arg("--system").arg(drms_path("leo_1day_orbital_native.system.yaml"));
    cmd.arg("--out-dir").arg(out_dir).arg("--workers").arg("2");
    if let Some(store) = store_dir {
        cmd.arg("--store-dir").arg(store);
    }
    if let Some(ch) = ch {
        cmd.args(clickhouse_flags(ch));
    }
    cmd.output().expect("spawning av-sweep study mode")
}

fn import(pb_path: &Path, store_dir: Option<&Path>, ch: Option<&StatefulCh>) -> Output {
    let mut cmd = Command::new(av_sweep_bin());
    cmd.arg("--import-study").arg(pb_path);
    if let Some(store) = store_dir {
        cmd.arg("--store-dir").arg(store);
    }
    if let Some(ch) = ch {
        cmd.args(clickhouse_flags(ch));
    }
    cmd.output().expect("spawning av-sweep --import-study")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn assert_refused_cleanly(output: &Output) -> String {
    let text = stderr(output);
    assert!(!output.status.success(), "must exit non-zero: {text}");
    assert!(output.status.code().is_some(), "exited normally, not by a signal: {:?}", output.status);
    assert!(!text.contains("panicked"), "a typed error, not a panic: {text}");
    text
}

fn ch_view(ch: &StatefulCh, id: &str) -> Option<pb::SweepResults> {
    ClickHouseStudyStore::connect(&ch.config()).unwrap().get_study(id).unwrap()
}

fn foreign_study() -> pb::SweepResults {
    one_sample_study(ID)
}

/// ClickHouse holds a different study under the id: the study is refused before the first sample
/// runs, naming ClickHouse and the first field that differs; the file store is never written.
#[test]
fn clickhouse_holding_a_different_study_refuses_study_mode_before_the_first_sample() {
    let dir = scratch("ch-refuses");
    let (out, store) = (dir.join("out"), dir.join("store"));
    let ch = StatefulCh::new();
    let foreign = foreign_study();
    ClickHouseStudyStore::connect(&ch.config()).unwrap().put_study(&foreign).unwrap();
    let rows = ch.row_count(schema::SAMPLES, ID);

    let output = run_native_study(&out, Some(&store), Some(&ch));
    let text = assert_refused_cleanly(&output);
    assert!(out.is_dir() && tree(&out).is_empty(), "--out-dir holds no sample_* directory and no sweep_results.pb (a sample ran before the refusal): {:?}", tree(&out));
    assert!(text.contains("the ClickHouse study store at http://127.0.0.1:") && text.contains("refuses this study up front"), "{text}");
    assert!(text.contains("sweep_hash is \"sweep-hash\" there") && text.contains("studies are immutable") && text.contains("no sample was run"), "{text}");
    assert!(!text.contains("study store written"), "{text}");

    assert!(!store.exists(), "the file store was not touched (not even created)");
    assert_eq!(ch_view(&ch, ID), Some(foreign), "ClickHouse still holds the other study");
    assert_eq!(ch.row_count(schema::SAMPLES, ID), rows, "and got no new rows");
    assert!(ch.orphan_write_ids(ID).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

/// The mirror: the file store holds the different study and ClickHouse is empty: refused up
/// front naming the file store, and ClickHouse is untouched.
#[test]
fn the_file_store_holding_a_different_study_refuses_study_mode_before_the_first_sample() {
    let dir = scratch("file-refuses");
    let (out, store) = (dir.join("out"), dir.join("store"));
    let ch = StatefulCh::new();
    FileStudyStore::new(&store).put_study(&foreign_study()).unwrap();
    let store_before = tree(&store);

    let output = run_native_study(&out, Some(&store), Some(&ch));
    let text = assert_refused_cleanly(&output);
    assert!(out.is_dir() && tree(&out).is_empty(), "--out-dir holds no sample_* directory and no sweep_results.pb (a sample ran before the refusal): {:?}", tree(&out));
    assert!(text.contains(&format!("the study store at {}", store.display())) && text.contains("refuses this study up front"), "{text}");
    assert!(text.contains("sweep_hash is \"sweep-hash\" there") && text.contains("no sample was run"), "{text}");

    assert_eq!(tree(&store), store_before, "the file store is unchanged");
    for table in [schema::STUDIES, schema::SAMPLES, schema::AXIS_VALUES, schema::SEEDS, schema::SCORES, schema::AGGREGATES] {
        assert_eq!(ch.row_count(table, ID), 0, "ClickHouse was never written ({table})");
    }
    assert_eq!(ch_view(&ch, ID), None);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A real run into ClickHouse, then the same study again into both stores: another `--out-dir` is
/// refused up front (ClickHouse names `products_uri`; the file store is not created); the same
/// `--out-dir` is compatible, so the missing file store is filled and ClickHouse is not rewritten.
#[test]
fn a_rerun_is_checked_up_front_and_fills_the_store_that_lacks_the_study() {
    let dir = scratch("rerun");
    let (out_a, out_b, store) = (dir.join("out-a"), dir.join("out-b"), dir.join("store"));
    let ch = StatefulCh::new();

    let first = run_native_study(&out_a, None, Some(&ch));
    assert!(first.status.success(), "{}", stderr(&first));
    assert!(stderr(&first).contains("12 sample(s), 0 failed"), "{}", stderr(&first));
    let held = ch_view(&ch, ID).expect("ClickHouse holds the study");
    let rows = ch.row_count(schema::SAMPLES, ID);
    assert_eq!(rows, 12);

    let elsewhere = run_native_study(&out_b, Some(&store), Some(&ch));
    let text = assert_refused_cleanly(&elsewhere);
    assert!(text.contains("the ClickHouse study store") && text.contains("samples[p0_d0].products_uri"), "{text}");
    assert!(tree(&out_b).is_empty() && !store.exists(), "refused before any sample ran or any store was written");

    let again = run_native_study(&out_a, Some(&store), Some(&ch));
    assert!(again.status.success(), "{}", stderr(&again));
    assert_eq!(FileStudyStore::new(&store).get_study(ID).unwrap().as_ref(), Some(&held), "the file store now holds the same study");
    assert_eq!(ch_view(&ch, ID), Some(held));
    assert_eq!(ch.row_count(schema::SAMPLES, ID), rows, "ClickHouse already held it: nothing was rewritten");
    assert!(stderr(&again).contains("already held, identical"), "{}", stderr(&again));
    let _ = std::fs::remove_dir_all(&dir);
}

/// `--import-study` into both stores with a ClickHouse whose header insert fails: the commit fails
/// at ClickHouse, which is first, so the file store is never committed: no study directory, no
/// stage.
#[test]
fn an_import_whose_clickhouse_commit_fails_leaves_the_file_store_unchanged() {
    let dir = scratch("import-commit-fails");
    let store = dir.join("store");
    let ch = StatefulCh::new();
    ch.fail_inserts_into(schema::STUDIES);
    let pb_path = dir.join("in.pb");
    let study = foreign_study();
    std::fs::write(&pb_path, study.encode_to_vec()).unwrap();

    let output = import(&pb_path, Some(&store), Some(&ch));
    let text = assert_refused_cleanly(&output);
    assert!(text.contains("the ClickHouse study store") && text.contains("the commit failed") && text.contains("nothing was made visible by this run in any store"), "{text}");
    assert!(!text.contains("--import-study"), "no repair is needed, so none is offered: {text}");
    assert!(!store.exists() || tree(&store).is_empty(), "the file store holds nothing and no stage: {:?}", tree(&store));
    assert_eq!(ch_view(&ch, ID), None);

    // The same import against a healthy ClickHouse then succeeds into both.
    ch.clear_faults();
    let output = import(&pb_path, Some(&store), Some(&ch));
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(FileStudyStore::new(&store).get_study(ID).unwrap(), Some(study.clone()));
    assert_eq!(ch_view(&ch, ID), Some(study));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A non-empty directory that holds no study sits where the study goes: refused before the first
/// sample runs (study mode) and before anything is written (`--import-study`), ClickHouse never
/// touched in either.
#[test]
fn an_occupied_file_target_is_refused_up_front_and_clickhouse_is_never_touched() {
    let dir = scratch("occupied");
    let (out, store) = (dir.join("out"), dir.join("store"));
    let ch = StatefulCh::new();
    std::fs::create_dir_all(store.join(ID)).unwrap();
    std::fs::write(store.join(ID).join("notes.txt"), b"mine").unwrap();
    let store_before = tree(&store);

    let output = run_native_study(&out, Some(&store), Some(&ch));
    let text = assert_refused_cleanly(&output);
    assert!(tree(&out).is_empty(), "no sample ran: {:?}", tree(&out));
    assert!(text.contains(&format!("the study store at {}", store.display())) && text.contains("refuses this study up front") && text.contains("is already here and is not empty") && text.contains("no sample was run"), "{text}");

    let pb_path = dir.join("in.pb");
    std::fs::write(&pb_path, foreign_study().encode_to_vec()).unwrap();
    let output = import(&pb_path, Some(&store), Some(&ch));
    let text = assert_refused_cleanly(&output);
    assert!(text.contains("refuses this study up front") && text.contains("is already here and is not empty"), "{text}");

    assert_eq!(tree(&store), store_before);
    for table in [schema::STUDIES, schema::SAMPLES] {
        assert_eq!(ch.row_count(table, ID), 0, "ClickHouse was never written ({table})");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The unknown-outcome window end to end: ClickHouse stores the header, the reply is lost and
/// every read then fails. The re-read is attempted 3 times, the header is inserted once, and the
/// message says the outcome is unknown, names the write and prints the repair command; the file
/// store holds nothing and no stage. Once the server is back the printed command converges.
#[test]
fn an_unknown_commit_prints_the_repair_command_and_leaves_the_file_store_empty() {
    let dir = scratch("unknown");
    let store = dir.join("store");
    let ch = StatefulCh::new();
    let pb_path = dir.join("in.pb");
    let study = foreign_study();
    std::fs::write(&pb_path, study.encode_to_vec()).unwrap();
    ch.lose_the_header_reply_and_then_fail_every_read();

    let output = import(&pb_path, Some(&store), Some(&ch));
    let text = assert_refused_cleanly(&output);
    assert!(text.contains("whether the study was committed is unknown") && text.contains("after 3 attempts, 200 ms apart"), "{text}");
    let repair = format!("av-sweep --import-study {} --store-dir {} --store-clickhouse {} --clickhouse-database studies --clickhouse-user avsweep", pb_path.display(), store.display(), ch.fake.url());
    assert!(text.contains(&repair), "{text}");
    let (inserts, _) = ch.header_requests();
    assert_eq!(inserts, 1, "the header insert was not retried");
    assert!(!store.exists() || tree(&store).is_empty(), "the file store holds nothing and no stage: {:?}", tree(&store));

    ch.clear_faults();
    assert_eq!(ch_view(&ch, ID).as_ref(), Some(&study), "ClickHouse did commit");
    let repaired = import(&pb_path, Some(&store), Some(&ch));
    assert!(repaired.status.success(), "{}", stderr(&repaired));
    assert_eq!(FileStudyStore::new(&store).get_study(ID).unwrap().as_ref(), Some(&study));
    let _ = std::fs::remove_dir_all(&dir);
}

/// ClickHouse commits and the file store's rename then fails (a dangling symlink sits where the
/// study goes: the target looks absent at prepare, and the rename onto a symlink fails with
/// `ENOTDIR`): the partial-commit message names both stores and prints the repair command with
/// the real paths, and that command converges.
#[test]
fn a_partial_commit_prints_a_repair_command_that_converges() {
    let dir = scratch("partial");
    let (out, store) = (dir.join("out"), dir.join("store"));
    let ch = StatefulCh::new();
    std::fs::create_dir_all(&store).unwrap();
    let in_the_way = store.join(ID);
    std::os::unix::fs::symlink(dir.join("no-such-target"), &in_the_way).unwrap();

    let output = run_native_study(&out, Some(&store), Some(&ch));
    let text = assert_refused_cleanly(&output);
    let pb_path = out.join("sweep_results.pb");
    assert!(pb_path.is_file(), "sweep_results.pb is written before any store, so the repair input exists");
    assert!(text.contains("the stores now disagree") && text.contains("written to the ClickHouse study store") && text.contains(&format!("the study store at {} could not commit it", store.display())), "{text}");
    let repair = format!("av-sweep --import-study {} --store-dir {} --store-clickhouse {} --clickhouse-database studies --clickhouse-user avsweep", pb_path.display(), store.display(), ch.fake.url());
    assert!(text.contains(&repair), "the message prints the repair command:\n{text}\nwanted:\n{repair}");

    let study = pb::SweepResults::decode(std::fs::read(&pb_path).unwrap().as_slice()).unwrap();
    assert_eq!(ch_view(&ch, ID).as_ref(), Some(&study), "ClickHouse holds the study");
    assert_eq!(FileStudyStore::new(&store).get_study(ID).unwrap(), None, "the file store does not");
    assert!(!tree(&store).keys().any(|k| k.contains(".incoming-")), "no stage left: {:?}", tree(&store));
    assert!(std::fs::symlink_metadata(&in_the_way).unwrap().file_type().is_symlink(), "what was in the way is untouched");

    // The operator clears what was in the way and runs the printed command.
    std::fs::remove_file(&in_the_way).unwrap();
    let rows = ch.row_count(schema::SAMPLES, ID);
    let repaired = import(&pb_path, Some(&store), Some(&ch));
    assert!(repaired.status.success(), "{}", stderr(&repaired));
    assert_eq!(FileStudyStore::new(&store).get_study(ID).unwrap().as_ref(), Some(&study), "both stores hold the identical study");
    assert_eq!(ch_view(&ch, ID).as_ref(), Some(&study));
    assert_eq!(ch.row_count(schema::SAMPLES, ID), rows, "ClickHouse was not written again");
    let _ = std::fs::remove_dir_all(&dir);
}
