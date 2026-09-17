//! Acceptance evidence for `crates/av-jobs::runner::Runner`: one test per reachable
//! `JobFailureKind` (each asserting the completion was actually appended to the log and
//! reads back `ok == false` with the right kind, not merely that `run_one` returned
//! something), plus the end-to-end proof that a successful completion's `outputs`/
//! `manifest_sha256` survive a fresh re-open of the log from disk.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use av_cdm::pb;
use av_jobs::clock::TestClock;
use av_jobs::log::JobLog;
use av_jobs::queue::JobQueue;
use av_jobs::runner::{Executor, JobInput, JobOutput, MemoryObjectSink, MemoryObjectSource, ProcessExecutor, Runner};
use av_label::ClearanceLadder;

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);
impl TempDir {
    fn new(tag: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("av-jobs-runner-test-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        Self(dir)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn ladder() -> ClearanceLadder {
    ClearanceLadder::new(vec!["UNCLASSIFIED".to_string(), "CUI".to_string(), "SECRET".to_string()])
}

fn valid_label() -> pb::Label {
    pb::Label { marking: "CUI".to_string(), caveats: vec![] }
}

fn base_spec(job_id: &str, kind: &str) -> pb::JobSpec {
    pb::JobSpec { job_id: job_id.to_string(), kind: kind.to_string(), label: Some(valid_label()), executor: pb::JobExecutorKind::Process as i32, ..Default::default() }
}

/// A test-only `Executor` that hands back exactly the outputs it was constructed with,
/// regardless of the spec/inputs it is called with.
#[derive(Debug)]
struct FixedOutputsExecutor {
    outputs: Vec<JobOutput>,
    declares_manifest: bool,
}

impl Executor for FixedOutputsExecutor {
    fn execute(&self, _spec: &pb::JobSpec, _inputs: &[JobInput]) -> Result<Vec<JobOutput>, pb::JobFailure> {
        Ok(self.outputs.iter().map(|o| JobOutput { bytes: o.bytes.clone(), media_type: o.media_type.clone(), manifest: o.manifest }).collect())
    }
    fn declares_manifest(&self) -> bool {
        self.declares_manifest
    }
}

fn last_completion(dir: &Path, name: &str, job_id: &str) -> pb::JobCompletion {
    // Re-opens a FRESH JobLog from disk -- never trusts the Runner's own in-memory state --
    // matching this crate's own "read back from a fresh log" acceptance discipline.
    let (log, _) = JobLog::open(dir, name).unwrap();
    let records = log.read_all().unwrap();
    records
        .into_iter()
        .rev()
        .find_map(|r| match r.event {
            Some(pb::job_log_record::Event::Completed(c)) if c.job_id == job_id => Some(c),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no completed record for job_id {job_id:?} found on the log"))
}

// -- one test per reachable JobFailureKind ---------------------------------------------

#[test]
fn unsupported_job_kind_is_recorded_on_the_log() {
    let dir = TempDir::new("unsupported-kind");
    let clock = TestClock::new(0);
    let (queue, _) = JobQueue::open(dir.path(), "q").unwrap();
    let source = Box::new(MemoryObjectSource::new());
    let sink = Box::new(MemoryObjectSink::new("jobs"));
    let runner = Runner::new(queue, source, sink, ladder(), &clock);
    // Deliberately no register_executor call at all -- no kind is ever registered.

    let spec = base_spec("job-1", "no-such-kind");
    let completion = runner.run_one(&spec).expect("the completion is appended to the log; only an I/O failure of the log itself is an Err here");
    assert!(!completion.ok);
    assert_eq!(completion.failure.as_ref().unwrap().kind, pb::JobFailureKind::UnsupportedJobKind as i32);

    let persisted = last_completion(dir.path(), "q", "job-1");
    assert!(!persisted.ok);
    assert_eq!(persisted.failure.unwrap().kind, pb::JobFailureKind::UnsupportedJobKind as i32);
}

#[test]
fn label_refused_is_recorded_on_the_log() {
    let dir = TempDir::new("label-refused");
    let clock = TestClock::new(0);
    let (queue, _) = JobQueue::open(dir.path(), "q").unwrap();
    let source = Box::new(MemoryObjectSource::new());
    let sink = Box::new(MemoryObjectSink::new("jobs"));
    let mut runner = Runner::new(queue, source, sink, ladder(), &clock);
    runner.register_executor("echo", Box::new(ProcessExecutor));

    let mut spec = base_spec("job-1", "echo");
    spec.label = Some(pb::Label { marking: "TOP-SECRET".to_string(), caveats: vec![] }); // not on the ladder
    spec.command = vec!["/bin/echo".to_string(), "hi".to_string()];
    let completion = runner.run_one(&spec).expect("the completion is appended to the log; only an I/O failure of the log itself is an Err here");
    assert!(!completion.ok);
    assert_eq!(completion.failure.as_ref().unwrap().kind, pb::JobFailureKind::LabelRefused as i32);

    let persisted = last_completion(dir.path(), "q", "job-1");
    assert_eq!(persisted.failure.unwrap().kind, pb::JobFailureKind::LabelRefused as i32);
}

#[test]
fn input_missing_is_recorded_on_the_log() {
    let dir = TempDir::new("input-missing");
    let clock = TestClock::new(0);
    let (queue, _) = JobQueue::open(dir.path(), "q").unwrap();
    let source = Box::new(MemoryObjectSource::new()); // nothing registered
    let sink = Box::new(MemoryObjectSink::new("jobs"));
    let mut runner = Runner::new(queue, source, sink, ladder(), &clock);
    runner.register_executor("echo", Box::new(ProcessExecutor));

    let mut spec = base_spec("job-1", "echo");
    spec.command = vec!["/bin/echo".to_string()];
    spec.inputs = vec![pb::AssetRef { uri: "memory://does-not-exist".to_string(), sha256: "a".repeat(64), size_bytes: 4, media_type: "application/octet-stream".to_string(), ..Default::default() }];
    let completion = runner.run_one(&spec).expect("the completion is appended to the log; only an I/O failure of the log itself is an Err here");
    assert!(!completion.ok);
    assert_eq!(completion.failure.as_ref().unwrap().kind, pb::JobFailureKind::InputMissing as i32);

    let persisted = last_completion(dir.path(), "q", "job-1");
    assert_eq!(persisted.failure.unwrap().kind, pb::JobFailureKind::InputMissing as i32);
}

#[test]
fn input_hash_mismatch_is_recorded_on_the_log() {
    let dir = TempDir::new("input-hash-mismatch");
    let clock = TestClock::new(0);
    let (queue, _) = JobQueue::open(dir.path(), "q").unwrap();
    let source = Box::new(MemoryObjectSource::new());
    source.insert("memory://input-1", b"the real bytes".to_vec());
    let sink = Box::new(MemoryObjectSink::new("jobs"));
    let mut runner = Runner::new(queue, source, sink, ladder(), &clock);
    runner.register_executor("echo", Box::new(ProcessExecutor));

    let mut spec = base_spec("job-1", "echo");
    spec.command = vec!["/bin/echo".to_string()];
    // sha256 here is deliberately wrong for "the real bytes" -- a hash of different content.
    let wrong_hash = av_jobs::hash::hex_encode(&openssl::sha::sha256(b"not the real bytes"));
    spec.inputs = vec![pb::AssetRef { uri: "memory://input-1".to_string(), sha256: wrong_hash, size_bytes: 14, media_type: "application/octet-stream".to_string(), ..Default::default() }];
    let completion = runner.run_one(&spec).expect("the completion is appended to the log; only an I/O failure of the log itself is an Err here");
    assert!(!completion.ok);
    assert_eq!(completion.failure.as_ref().unwrap().kind, pb::JobFailureKind::InputHashMismatch as i32);

    let persisted = last_completion(dir.path(), "q", "job-1");
    assert_eq!(persisted.failure.unwrap().kind, pb::JobFailureKind::InputHashMismatch as i32);
}

#[test]
fn nonzero_exit_is_recorded_on_the_log_with_the_real_exit_code() {
    let dir = TempDir::new("nonzero-exit");
    let clock = TestClock::new(0);
    let (queue, _) = JobQueue::open(dir.path(), "q").unwrap();
    let source = Box::new(MemoryObjectSource::new());
    let sink = Box::new(MemoryObjectSink::new("jobs"));
    let mut runner = Runner::new(queue, source, sink, ladder(), &clock);
    runner.register_executor("sh", Box::new(ProcessExecutor));

    let mut spec = base_spec("job-1", "sh");
    spec.command = vec!["/bin/sh".to_string(), "-c".to_string(), "exit 3".to_string()];
    let completion = runner.run_one(&spec).expect("the completion is appended to the log; only an I/O failure of the log itself is an Err here");
    assert!(!completion.ok);
    let failure = completion.failure.as_ref().unwrap();
    assert_eq!(failure.kind, pb::JobFailureKind::NonzeroExit as i32);
    assert_eq!(failure.exit_code, 3);

    let persisted = last_completion(dir.path(), "q", "job-1");
    let persisted_failure = persisted.failure.unwrap();
    assert_eq!(persisted_failure.kind, pb::JobFailureKind::NonzeroExit as i32);
    assert_eq!(persisted_failure.exit_code, 3);
}

#[test]
fn executor_start_failed_is_recorded_on_the_log() {
    let dir = TempDir::new("start-failed");
    let clock = TestClock::new(0);
    let (queue, _) = JobQueue::open(dir.path(), "q").unwrap();
    let source = Box::new(MemoryObjectSource::new());
    let sink = Box::new(MemoryObjectSink::new("jobs"));
    let mut runner = Runner::new(queue, source, sink, ladder(), &clock);
    runner.register_executor("sh", Box::new(ProcessExecutor));

    let mut spec = base_spec("job-1", "sh");
    spec.command = vec!["/av-jobs-test-nonexistent-binary-4f9c2".to_string()];
    let completion = runner.run_one(&spec).expect("the completion is appended to the log; only an I/O failure of the log itself is an Err here");
    assert!(!completion.ok);
    assert_eq!(completion.failure.as_ref().unwrap().kind, pb::JobFailureKind::ExecutorStartFailed as i32);

    let persisted = last_completion(dir.path(), "q", "job-1");
    assert_eq!(persisted.failure.unwrap().kind, pb::JobFailureKind::ExecutorStartFailed as i32);
}

#[test]
fn output_rejected_is_recorded_on_the_log_for_two_manifest_outputs() {
    let dir = TempDir::new("output-rejected");
    let clock = TestClock::new(0);
    let (queue, _) = JobQueue::open(dir.path(), "q").unwrap();
    let source = Box::new(MemoryObjectSource::new());
    let sink = Box::new(MemoryObjectSink::new("jobs"));
    let mut runner = Runner::new(queue, source, sink, ladder(), &clock);
    runner.register_executor(
        "two-manifests",
        Box::new(FixedOutputsExecutor {
            outputs: vec![
                JobOutput { bytes: b"tileset-a".to_vec(), media_type: "application/vnd.altavista.tileset+json".to_string(), manifest: true },
                JobOutput { bytes: b"tileset-b".to_vec(), media_type: "application/vnd.altavista.tileset+json".to_string(), manifest: true },
            ],
            declares_manifest: true,
        }),
    );

    let spec = base_spec("job-1", "two-manifests");
    let completion = runner.run_one(&spec).expect("the completion is appended to the log; only an I/O failure of the log itself is an Err here");
    assert!(!completion.ok);
    assert_eq!(completion.failure.as_ref().unwrap().kind, pb::JobFailureKind::OutputRejected as i32);
    assert!(completion.outputs.is_empty(), "a rejected run must not partially store outputs");

    let persisted = last_completion(dir.path(), "q", "job-1");
    assert_eq!(persisted.failure.unwrap().kind, pb::JobFailureKind::OutputRejected as i32);
}

#[test]
fn output_rejected_for_a_manifest_output_from_an_executor_that_declares_none() {
    let dir = TempDir::new("output-rejected-undeclared");
    let clock = TestClock::new(0);
    let (queue, _) = JobQueue::open(dir.path(), "q").unwrap();
    let source = Box::new(MemoryObjectSource::new());
    let sink = Box::new(MemoryObjectSink::new("jobs"));
    let mut runner = Runner::new(queue, source, sink, ladder(), &clock);
    runner.register_executor(
        "undeclared-manifest",
        Box::new(FixedOutputsExecutor { outputs: vec![JobOutput { bytes: b"oops".to_vec(), media_type: "application/octet-stream".to_string(), manifest: true }], declares_manifest: false }),
    );

    let spec = base_spec("job-1", "undeclared-manifest");
    let completion = runner.run_one(&spec).expect("the completion is appended to the log; only an I/O failure of the log itself is an Err here");
    assert!(!completion.ok);
    assert_eq!(completion.failure.as_ref().unwrap().kind, pb::JobFailureKind::OutputRejected as i32);
}

#[test]
fn executor_unavailable_for_the_container_kind_is_recorded_on_the_log() {
    let dir = TempDir::new("executor-unavailable");
    let clock = TestClock::new(0);
    let (queue, _) = JobQueue::open(dir.path(), "q").unwrap();
    let source = Box::new(MemoryObjectSource::new());
    let sink = Box::new(MemoryObjectSink::new("jobs"));
    let mut runner = Runner::new(queue, source, sink, ladder(), &clock);
    // A real, working ProcessExecutor IS registered under this kind -- proving CONTAINER is
    // refused before ever reaching it, not merely that no executor happens to exist.
    runner.register_executor("echo", Box::new(ProcessExecutor));

    let mut spec = base_spec("job-1", "echo");
    spec.executor = pb::JobExecutorKind::Container as i32;
    spec.command = vec!["/bin/echo".to_string(), "would have succeeded".to_string()];
    let completion = runner.run_one(&spec).expect("the completion is appended to the log; only an I/O failure of the log itself is an Err here");
    assert!(!completion.ok);
    assert_eq!(completion.failure.as_ref().unwrap().kind, pb::JobFailureKind::ExecutorUnavailable as i32);

    let persisted = last_completion(dir.path(), "q", "job-1");
    assert_eq!(persisted.failure.unwrap().kind, pb::JobFailureKind::ExecutorUnavailable as i32);
}

// -- acceptance evidence item 4: outputs/manifest_sha256 survive a fresh log re-open ---

#[test]
fn a_successful_completions_outputs_and_manifest_hash_survive_a_fresh_log_reopen() {
    let dir = TempDir::new("outputs-roundtrip");
    let clock = TestClock::new(1_000);
    let (queue, _) = JobQueue::open(dir.path(), "q").unwrap();
    let source = Box::new(MemoryObjectSource::new());
    let sink = Box::new(MemoryObjectSink::new("jobs"));
    let mut runner = Runner::new(queue, source, sink, ladder(), &clock);
    let manifest_bytes = b"{\"tiles\":[]}".to_vec();
    let tile_bytes = b"tile-bytes-0-0-0".to_vec();
    runner.register_executor(
        "tiler-double",
        Box::new(FixedOutputsExecutor {
            outputs: vec![
                JobOutput { bytes: tile_bytes.clone(), media_type: "image/png".to_string(), manifest: false },
                JobOutput { bytes: manifest_bytes.clone(), media_type: "application/vnd.altavista.tileset+json".to_string(), manifest: true },
            ],
            declares_manifest: true,
        }),
    );

    clock.set(5_000);
    let spec = base_spec("job-1", "tiler-double");
    let completion = runner.run_one(&spec).expect("the completion is appended to the log; only an I/O failure of the log itself is an Err here");
    assert!(completion.ok, "{completion:?}");
    assert_eq!(completion.started_tai_ns, 5_000);
    assert!(completion.finished_tai_ns >= 5_000);
    assert_eq!(completion.outputs.len(), 2);

    let expected_tile_sha256 = av_jobs::hash::hex_encode(&openssl::sha::sha256(&tile_bytes));
    let expected_manifest_sha256 = av_jobs::hash::hex_encode(&openssl::sha::sha256(&manifest_bytes));
    assert_eq!(completion.manifest_sha256, expected_manifest_sha256);

    // Re-open a FRESH JobLog from disk and re-read the completed record -- never trust the
    // Runner's own in-memory return value alone.
    let persisted = last_completion(dir.path(), "q", "job-1");
    assert!(persisted.ok);
    assert_eq!(persisted.outputs.len(), 2);
    assert_eq!(persisted.manifest_sha256, expected_manifest_sha256);

    let tile_asset = persisted.outputs.iter().find(|a| !a.uri.is_empty() && a.sha256 == expected_tile_sha256).expect("tile output present");
    assert_eq!(tile_asset.sha256, expected_tile_sha256);
    assert_eq!(tile_asset.size_bytes, tile_bytes.len() as u64);
    assert_eq!(tile_asset.media_type, "image/png");
    assert_eq!(tile_asset.label.as_ref().unwrap().marking, "CUI");
    assert!(tile_asset.uri.contains(&expected_tile_sha256), "uri {:?} should be content-addressed by the tile's own hash", tile_asset.uri);

    let manifest_asset = persisted.outputs.iter().find(|a| a.sha256 == expected_manifest_sha256).expect("manifest output present");
    assert_eq!(manifest_asset.media_type, "application/vnd.altavista.tileset+json");
    assert_eq!(manifest_asset.label.as_ref().unwrap().marking, "CUI");
    assert_eq!(manifest_asset.sha256, persisted.manifest_sha256);
}

#[test]
fn run_pending_drains_the_queue_and_appends_every_completion() {
    let dir = TempDir::new("run-pending");
    let clock = TestClock::new(0);
    let (queue, _) = JobQueue::open(dir.path(), "q").unwrap();
    let source = Box::new(MemoryObjectSource::new());
    let sink = Box::new(MemoryObjectSink::new("jobs"));
    let mut runner = Runner::new(queue, source, sink, ladder(), &clock);
    runner.register_executor("echo", Box::new(ProcessExecutor));

    for i in 0..3 {
        let mut spec = base_spec(&format!("job-{i}"), "echo");
        spec.command = vec!["/bin/echo".to_string(), format!("job-{i}")];
        runner.queue().submit(&spec).unwrap();
    }

    let completions = runner.run_pending().unwrap();
    assert_eq!(completions.len(), 3);
    assert!(completions.iter().all(|c| c.ok), "{completions:?}");
    assert!(runner.queue().pending().unwrap().is_empty());
}
