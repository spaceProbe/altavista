//! H3, round 3, task P3b (`docs/heavy-plan.md` H3's last open item): the real-Docker proof that
//! `crate::container::ContainerExecutor` actually runs a job's command inside a real, hardened,
//! digest-pinned container -- not merely that its code compiles.
//!
//! # Template -- gating, locking, labelling, pruning, skip wording copied exactly
//!
//! `crates/av-jobs/tests/store_tiler.rs` is this file's own template for the docker-gated test
//! shape (that file's own module doc: "`crates/av-store/tests/minio_store.rs` is this file's
//! own template"). `image_digest_md_path`/`fenced_block_after`/`parse_image_digest_md`/`gate`/
//! `gate_or_skip!` below are copied from `store_tiler.rs` character for character, pointed at
//! this file's own `CONTAINER_EXECUTOR_IMAGE_DIGEST.md` instead of `services/store/
//! IMAGE_DIGEST.md` -- see that file's own doc comment for why the digest lives beside the test
//! that uses it (question 212(a): "the recorded digest has exactly one home").
//!
//! # Why this test needs no real object store
//!
//! `crate::container::ContainerExecutor` never touches an object store on either side of a job
//! run (that module's own doc comment) -- `crate::runner::Runner` fetches and hash-verifies
//! every input BEFORE any `Executor` sees it and stores every output AFTER, through whatever
//! `ObjectSource`/`ObjectSink` the `Runner` was built with. This test uses the crate's own
//! in-memory `MemoryObjectSource`/`MemoryObjectSink` test doubles for that half -- exactly like
//! `tests/runner.rs`'s own tests -- because what this test exists to prove (a container really
//! ran, hardened, from a digest-pinned image, producing real bytes) has nothing to do with
//! which store backs the runner; `tests/store_tiler.rs` already proves the real-MinIO half for
//! `crate::tiler::TilerExecutor` and needs no repeating here.
//!
//! # docker events, captured separately
//!
//! The manager's own review captures `docker events` (filtered on `label=av.job=1`) to a file
//! OUTSIDE this repository around a real run of this test, by hand -- not something this test
//! itself does; a test asserting on `docker events` output would be asserting on a debugging
//! aid, not on this executor's own contract.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use av_cdm::pb;
use av_jobs::clock::TestClock;
use av_jobs::container::{ContainerExecutor, CONTAINER_INPUT_DIR, CONTAINER_OUTPUT_DIR, JOB_LABEL_KEY, JOB_LABEL_VALUE};
use av_jobs::queue::JobQueue;
use av_jobs::runner::{MemoryObjectSink, MemoryObjectSource, ObjectSink, ProcessExecutor, Runner};
use av_label::ClearanceLadder;
use av_lockstep::docker::{announce_gate_skip, lock_docker_tests, prune_stale_test_resources, recorded_digest_gate, test_label_args, test_run_id, DockerGateReason};
use openssl::sha::sha256;

/// Wraps a shared `Arc<MemoryObjectSink>` as an `ObjectSink` the `Runner` can own, while this
/// test keeps its own `Arc` clone to read stored bytes back after the run -- mirrors
/// `crates/av-jobs/tests/store_tiler.rs::SharedMemorySink` exactly (same need: the `Runner`
/// takes ownership of a `Box<dyn ObjectSink>`, but this test must read what was actually stored
/// afterward).
#[derive(Debug, Clone)]
struct SharedMemorySink(Arc<MemoryObjectSink>);
impl ObjectSink for SharedMemorySink {
    fn put(&self, bytes: &[u8], media_type: &str, label: &pb::Label) -> Result<pb::AssetRef, pb::JobFailure> {
        self.0.put(bytes, media_type, label)
    }
}

/// `JobSpec.container_image` -- bare, tag-free (see `CONTAINER_EXECUTOR_IMAGE_DIGEST.md`'s own
/// "Why a floating tag..." section for why the digest, not this string, is what actually pins
/// the image).
const CONTAINER_IMAGE: &str = "alpine";

// -----------------------------------------------------------------------------------------
// CONTAINER_EXECUTOR_IMAGE_DIGEST.md parsing -- copied from crates/av-jobs/tests/store_tiler.rs
// (that file's own copy of crates/av-store/tests/minio_store.rs), character for character,
// pointed at this file's own recorded digest.
// -----------------------------------------------------------------------------------------

fn image_digest_md_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/CONTAINER_EXECUTOR_IMAGE_DIGEST.md")
}

fn fenced_block_after(text: &str, marker: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let marker_idx = lines.iter().position(|l| l.contains(marker))?;
    let fence_start = marker_idx + lines[marker_idx..].iter().position(|l| l.trim() == "```")?;
    let fence_end = fence_start + 1 + lines[fence_start + 1..].iter().position(|l| l.trim() == "```")?;
    let content = lines[fence_start + 1..fence_end].join("\n").trim().to_string();
    if content.is_empty() {
        None
    } else {
        Some(content)
    }
}

fn parse_image_digest_md() -> (String, String) {
    let path = image_digest_md_path();
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("could not read {path:?}: {e}"));
    let image_ref = fenced_block_after(&text, "Registry reference").unwrap_or_else(|| panic!("could not find the 'Registry reference' fenced code block in {path:?}"));
    let recorded_id = fenced_block_after(&text, "docker image inspect").unwrap_or_else(|| panic!("could not find the 'docker image inspect' fenced code block in {path:?}"));
    (image_ref, recorded_id)
}

/// The module-level gate (question 212(a)): docker daemon up AND the local image's id matches
/// what `CONTAINER_EXECUTOR_IMAGE_DIGEST.md` recorded -- identical check to
/// `crates/av-jobs/tests/store_tiler.rs::gate`, against this file's own recorded digest.
fn gate() -> Result<(), DockerGateReason> {
    let (image_ref, recorded_id) = parse_image_digest_md();
    recorded_digest_gate(&image_ref, &recorded_id)
}

/// This test's own first line: on a gate failure, announce the visible skip (never a silent
/// pass -- question 194) and return from the calling test function.
macro_rules! gate_or_skip {
    ($test_name:expr) => {
        if let Err(reason) = gate() {
            let line = announce_gate_skip($test_name, &reason);
            assert!(line.starts_with("SKIPPED "), "{line:?}");
            return;
        }
    };
}

fn labels_from_test_label_args(run_id: &str) -> BTreeMap<String, String> {
    let args = test_label_args(run_id);
    let mut map = BTreeMap::new();
    for kv in args.iter().skip(1).step_by(2) {
        let (key, value) = kv.split_once('=').unwrap_or_else(|| panic!("test_label_args produced a non-KEY=VALUE entry: {kv:?}"));
        map.insert(key.to_string(), value.to_string());
    }
    map
}

// -----------------------------------------------------------------------------------------
// Fixture plumbing.
// -----------------------------------------------------------------------------------------

fn ladder() -> ClearanceLadder {
    ClearanceLadder::new(vec!["UNCLASSIFIED".to_string(), "CUI".to_string(), "SECRET".to_string()])
}

fn valid_label() -> pb::Label {
    pb::Label { marking: "CUI".to_string(), caveats: vec![] }
}

static TEMPDIR_COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);
impl TempDir {
    fn new(tag: &str) -> Self {
        let n = TEMPDIR_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("av-jobs-container-test-{tag}-{}-{n}", std::process::id()));
        fs::create_dir_all(&dir).expect("create temp dir");
        Self(dir)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// `crate::container::ContainerExecutor`'s own bind-mount scratch root -- `$HOME`-relative
/// (Colima only bind-mounts `$HOME` into its VM; `crates/av-lockstep/src/docker_test_lock.rs`'s
/// own module doc has the measured account), under this repository's already-`.gitignore`d
/// `.av-test-tmp/` convention (`tests/test_edge_plugin_container.py::SCRATCH_ROOT`,
/// `crates/av-lockstep/src/docker_test_lock.rs::tests::repo_scratch_dir` -- the SAME
/// convention, for the SAME reason, every other docker-gated test/scratch-dir user in this
/// workspace already follows).
fn container_scratch_root() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.av-test-tmp/av-jobs-container-test");
    fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("could not create container scratch root {dir:?}: {e}"));
    dir
}

fn docker(args: &[&str]) -> String {
    let output = Command::new("docker").args(args).output().unwrap_or_else(|e| panic!("could not launch `docker {args:?}`: {e}"));
    if !output.status.success() {
        panic!("`docker {args:?}` failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn docker_json(args: &[&str]) -> serde_json::Value {
    serde_json::from_str(&docker(args)).unwrap_or_else(|e| panic!("`docker {args:?}` did not print valid JSON: {e}"))
}

/// Polls `docker ps --filter label=av.job.id=<job_id> --format {{.Names}}` for the named
/// container to appear -- a real condition, not a fixed sleep (this task's own binding rule:
/// "no sleeping on a clock, poll a real condition"). The job under test sleeps for
/// `JOB_SLEEP_SECS` before producing output specifically so this window exists.
fn wait_for_container_running(job_id: &str, deadline: Instant) -> String {
    loop {
        let names = docker(&["ps", "--filter", &format!("label=av.job.id={job_id}"), "--format", "{{.Names}}"]);
        if let Some(name) = names.lines().next() {
            if !name.is_empty() {
                return name.to_string();
            }
        }
        if Instant::now() > deadline {
            panic!("container for job {job_id:?} never appeared in `docker ps` within the deadline");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn base_spec(job_id: &str, command: Vec<String>, container_image_digest: &str) -> pb::JobSpec {
    pb::JobSpec {
        job_id: job_id.to_string(),
        kind: "container-probe".to_string(),
        label: Some(valid_label()),
        requested_tai_ns: 1,
        executor: pb::JobExecutorKind::Container as i32,
        command,
        container_image: CONTAINER_IMAGE.to_string(),
        container_image_digest: container_image_digest.to_string(),
        ..Default::default()
    }
}

// -----------------------------------------------------------------------------------------
// The proof.
// -----------------------------------------------------------------------------------------

#[test]
fn container_executor_runs_a_real_job_inside_a_real_container() {
    const TEST_NAME: &str = "container_executor_runs_a_real_job_inside_a_real_container";
    gate_or_skip!(TEST_NAME);
    let (_image_ref, recorded_id) = parse_image_digest_md();
    let _lock = lock_docker_tests();
    prune_stale_test_resources(&_lock);

    let run_id = test_run_id();
    let extra_labels = labels_from_test_label_args(&run_id);
    let scratch_root = container_scratch_root();

    let input_bytes = b"hello av-jobs container executor\n".to_vec();
    let input_sha256 = av_jobs::hash::hex_encode(&sha256(&input_bytes));
    let input_uri = "memory://container-test-input";

    // -- Part 1: a successful job, with the container inspected/exec'd WHILE IT IS RUNNING
    // (question 148: "an exit code is not evidence" -- this inspects the real running
    // container, it does not assume the hardening flags were passed). ---------------------
    {
        let dir = TempDir::new("success");
        let clock = TestClock::new(4_000);
        let (queue, _) = JobQueue::open(dir.path(), "q").unwrap_or_else(|e| panic!("{TEST_NAME}: JobQueue::open: {e}"));
        let source = Box::new(MemoryObjectSource::new());
        source.insert(input_uri, input_bytes.clone());
        let sink_inner = Arc::new(MemoryObjectSink::new("jobs"));
        let sink = Box::new(SharedMemorySink(sink_inner.clone()));
        let mut runner = Runner::new(queue, source, sink, ladder(), &clock);
        // Step 1 of Runner::execute_spec looks up spec.kind in the ordinary kind registry
        // UNCONDITIONALLY, even for a CONTAINER job (crate::runner::Runner::
        // register_container_executor's own doc) -- a real, working ProcessExecutor is
        // registered here (never actually invoked -- the container executor is) exactly the
        // way crates/av-jobs/tests/runner.rs::
        // executor_unavailable_for_the_container_kind_is_recorded_on_the_log already does.
        runner.register_executor("container-probe", Box::new(ProcessExecutor));
        runner.register_container_executor(Box::new(ContainerExecutor::new(scratch_root.clone(), extra_labels.clone())));

        let job_id = format!("job-success-{run_id}");
        // A generous sleep: the container must stay alive through `docker inspect` plus
        // several `docker exec` round trips below, on a host this task's own brief describes
        // as heavily oversubscribed -- a short sleep risks the hardening checks racing the
        // job's own completion, not a correctness requirement of the executor itself.
        let command = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            format!("sleep 30; cat {CONTAINER_INPUT_DIR}/000 | tr 'a-z' 'A-Z' > {CONTAINER_OUTPUT_DIR}/000"),
        ];
        let mut spec = base_spec(&job_id, command, &recorded_id);
        spec.inputs = vec![pb::AssetRef { uri: input_uri.to_string(), sha256: input_sha256.clone(), size_bytes: input_bytes.len() as u64, media_type: "application/octet-stream".to_string(), ..Default::default() }];
        runner.queue().submit(&spec).unwrap_or_else(|e| panic!("{TEST_NAME}: submit: {e}"));

        // `Runner` (via its `Box<dyn Executor>`/`Box<dyn ObjectSource>`/`Box<dyn ObjectSink>`/
        // `&dyn Clock` fields) is not `Send` -- this crate's production traits deliberately
        // carry no such bound (nothing in H3a's own single-threaded design needs one). So the
        // concurrency this proof needs runs the OTHER way around: `runner.run_one` stays on
        // THIS (the main test) thread, and the background thread below owns only plain
        // `String`s and `docker` CLI calls (all `Send`) -- it polls for the container to
        // appear, then inspects/execs it WHILE `runner.run_one`'s own `docker run` is still
        // blocked in `sleep 30` on this thread.
        let job_id_for_thread = job_id.clone();
        let inspect_handle = std::thread::spawn(move || {
            let container_name = wait_for_container_running(&job_id_for_thread, Instant::now() + Duration::from_secs(30));

            // -- inspect the RUNNING container (docker inspect: valid whether running or
            // exited, but this is genuinely still running here thanks to the job's own
            // `sleep 30`). ----------------------------------------------------------------
            let inspect = docker_json(&["inspect", &container_name]);
            let info = inspect.as_array().and_then(|a| a.first()).unwrap_or_else(|| panic!("{TEST_NAME}: docker inspect {container_name} returned no object"));
            let config_user = info["Config"]["User"].as_str().unwrap_or("").to_string();
            assert!(!matches!(config_user.as_str(), "" | "0" | "root"), "expected a non-root .Config.User, got {config_user:?}");
            assert_eq!(info["HostConfig"]["ReadonlyRootfs"].as_bool(), Some(true), "expected .HostConfig.ReadonlyRootfs=true, got {:?}", info["HostConfig"]["ReadonlyRootfs"]);
            let cap_drop: Vec<String> = info["HostConfig"]["CapDrop"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default();
            assert!(cap_drop.iter().any(|c| c == "ALL"), "expected .HostConfig.CapDrop to contain ALL, got {cap_drop:?}");
            let security_opt: Vec<String> = info["HostConfig"]["SecurityOpt"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default();
            assert!(security_opt.iter().any(|o| o.contains("no-new-privileges")), "expected .HostConfig.SecurityOpt to contain a no-new-privileges entry, got {security_opt:?}");
            let mounts = info["Mounts"].as_array().cloned().unwrap_or_default();
            let out_mount = mounts.iter().find(|m| m["Destination"].as_str() == Some(CONTAINER_OUTPUT_DIR)).unwrap_or_else(|| panic!("{TEST_NAME}: no mount at {CONTAINER_OUTPUT_DIR}, got {mounts:?}"));
            assert_eq!(out_mount["RW"].as_bool(), Some(true), "expected the mount at {CONTAINER_OUTPUT_DIR} to be RW, got {out_mount:?}");
            let in_mount = mounts.iter().find(|m| m["Destination"].as_str() == Some(CONTAINER_INPUT_DIR)).unwrap_or_else(|| panic!("{TEST_NAME}: no mount at {CONTAINER_INPUT_DIR}, got {mounts:?}"));
            assert_eq!(in_mount["RW"].as_bool(), Some(false), "expected the mount at {CONTAINER_INPUT_DIR} to be read-only, got {in_mount:?}");
            // Question 156: the container really carries crate::container's own
            // JOB_LABEL_KEY/JOB_LABEL_VALUE, not merely this test's own av.test/
            // av.test.run_id labels.
            assert_eq!(info["Config"]["Labels"][JOB_LABEL_KEY].as_str(), Some(JOB_LABEL_VALUE), "expected label {JOB_LABEL_KEY}={JOB_LABEL_VALUE:?}, got {:?}", info["Config"]["Labels"]);

            // -- exec the RUNNING container: the kernel's own view. ------------------------
            let uid_str = docker(&["exec", &container_name, "id", "-u"]);
            let uid: u32 = uid_str.trim().parse().unwrap_or_else(|e| panic!("{TEST_NAME}: parsing `id -u` output {uid_str:?}: {e}"));
            assert_ne!(uid, 0, "expected a non-root uid inside the container");

            let status = docker(&["exec", &container_name, "cat", "/proc/1/status"]);
            let status_fields: std::collections::HashMap<&str, &str> = status.lines().filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim(), v.trim())).collect();
            assert_eq!(status_fields.get("NoNewPrivs"), Some(&"1"), "expected /proc/1/status NoNewPrivs: 1, got {status_fields:?}");
            assert_eq!(status_fields.get("Seccomp"), Some(&"2"), "expected /proc/1/status Seccomp: 2 (the DEFAULT filter profile), got {status_fields:?}");

            let root_write = Command::new("docker").args(["exec", &container_name, "sh", "-c", "echo x > /av_test_write_probe; echo EXIT:$?"]).output().unwrap();
            assert!(!String::from_utf8_lossy(&root_write.stdout).contains("EXIT:0"), "expected a write to / to fail under --read-only, got {root_write:?}");

            let volume_write = docker(&["exec", &container_name, "sh", "-c", &format!("echo x > {CONTAINER_OUTPUT_DIR}/av_test_write_probe; echo EXIT:$?")]);
            assert!(volume_write.contains("EXIT:0"), "expected a write to {CONTAINER_OUTPUT_DIR} to succeed, got {volume_write:?}");
            // Question 148: print what was actually observed, not just that the assertions
            // passed.
            println!(
                "\n--- container executor hardening proof (question 148) ---\nConfig.User={config_user:?} ReadonlyRootfs=true CapDrop={cap_drop:?} \
                 SecurityOpt={security_opt:?} uid={uid} NoNewPrivs=1 Seccomp=2 root_write_failed=true volume_write_ok=true\n"
            );
            // Clean up the write probe this hardening check itself left in the output mount,
            // so it is not mistaken for the job's own output once `runner.run_one` (on the
            // main thread) finishes and reads the output directory back.
            let _ = docker(&["exec", &container_name, "rm", "-f", &format!("{CONTAINER_OUTPUT_DIR}/av_test_write_probe")]);
        });

        let completion = runner.run_one(&spec).unwrap_or_else(|e| panic!("{TEST_NAME}: run_one: {e}"));
        inspect_handle.join().unwrap_or_else(|e| std::panic::resume_unwind(e));

        // -- assertion: the job completed ok, output bytes are EXACTLY what the command
        // produced, and the completion carries the output's hash. ---------------------------
        assert!(completion.ok, "{TEST_NAME}: job did not complete ok: {completion:?}");
        assert_eq!(completion.outputs.len(), 1, "{TEST_NAME}: expected exactly one output, got {:?}", completion.outputs);
        let output_asset = &completion.outputs[0];
        let mut expected_output = input_bytes.clone();
        expected_output.make_ascii_uppercase();
        let expected_output_sha256 = av_jobs::hash::hex_encode(&sha256(&expected_output));
        assert_eq!(output_asset.sha256, expected_output_sha256, "JobCompletion.outputs[0].sha256 must be the real sha256 of the command's real output bytes");
        let stored_bytes = sink_inner.get(&output_asset.sha256).unwrap_or_else(|| panic!("{TEST_NAME}: sink never stored anything under {:?}", output_asset.sha256));
        assert_eq!(stored_bytes, expected_output, "the stored output bytes must be EXACTLY what the containerized command produced, byte for byte");

        // -- the container is gone afterward. --------------------------------------------------
        let remaining = docker(&["ps", "-a", "--filter", &format!("label=av.job.id={job_id}"), "-q"]);
        assert_eq!(remaining, "", "{TEST_NAME}: container for job {job_id:?} still exists after the run: {remaining:?}");
    }

    // -- Part 2: a non-zero exit INSIDE the container becomes NONZERO_EXIT with the real exit
    // code. -----------------------------------------------------------------------------------
    {
        let dir = TempDir::new("nonzero-exit");
        let clock = TestClock::new(4_000);
        let (queue, _) = JobQueue::open(dir.path(), "q").unwrap_or_else(|e| panic!("{TEST_NAME}: JobQueue::open: {e}"));
        let source = Box::new(MemoryObjectSource::new());
        let sink = Box::new(MemoryObjectSink::new("jobs"));
        let mut runner = Runner::new(queue, source, sink, ladder(), &clock);
        // Step 1 of Runner::execute_spec looks up spec.kind in the ordinary kind registry
        // UNCONDITIONALLY, even for a CONTAINER job (crate::runner::Runner::
        // register_container_executor's own doc) -- a real, working ProcessExecutor is
        // registered here (never actually invoked -- the container executor is) exactly the
        // way crates/av-jobs/tests/runner.rs::
        // executor_unavailable_for_the_container_kind_is_recorded_on_the_log already does.
        runner.register_executor("container-probe", Box::new(ProcessExecutor));
        runner.register_container_executor(Box::new(ContainerExecutor::new(scratch_root.clone(), extra_labels.clone())));

        let job_id = format!("job-nonzero-{run_id}");
        let spec = base_spec(&job_id, vec!["/bin/sh".to_string(), "-c".to_string(), "exit 7".to_string()], &recorded_id);
        runner.queue().submit(&spec).unwrap_or_else(|e| panic!("{TEST_NAME}: submit: {e}"));
        let completion = runner.run_one(&spec).unwrap_or_else(|e| panic!("{TEST_NAME}: run_one: {e}"));
        assert!(!completion.ok, "{TEST_NAME}: expected the job to fail");
        let failure = completion.failure.as_ref().unwrap_or_else(|| panic!("{TEST_NAME}: ok==false but failure is unset"));
        assert_eq!(failure.kind, pb::JobFailureKind::NonzeroExit as i32, "{TEST_NAME}: {failure:?}");
        assert_eq!(failure.exit_code, 7, "{TEST_NAME}: {failure:?}");

        let remaining = docker(&["ps", "-a", "--filter", &format!("label=av.job.id={job_id}"), "-q"]);
        assert_eq!(remaining, "", "{TEST_NAME}: container for job {job_id:?} still exists after the run: {remaining:?}");
    }

    // -- Part 3: a container that could not even start (a command that does not exist inside
    // the image) is EXECUTOR_START_FAILED, not NONZERO_EXIT -- a genuinely different failure
    // mode (this task's own brief). -------------------------------------------------------
    {
        let dir = TempDir::new("start-failed");
        let clock = TestClock::new(4_000);
        let (queue, _) = JobQueue::open(dir.path(), "q").unwrap_or_else(|e| panic!("{TEST_NAME}: JobQueue::open: {e}"));
        let source = Box::new(MemoryObjectSource::new());
        let sink = Box::new(MemoryObjectSink::new("jobs"));
        let mut runner = Runner::new(queue, source, sink, ladder(), &clock);
        // Step 1 of Runner::execute_spec looks up spec.kind in the ordinary kind registry
        // UNCONDITIONALLY, even for a CONTAINER job (crate::runner::Runner::
        // register_container_executor's own doc) -- a real, working ProcessExecutor is
        // registered here (never actually invoked -- the container executor is) exactly the
        // way crates/av-jobs/tests/runner.rs::
        // executor_unavailable_for_the_container_kind_is_recorded_on_the_log already does.
        runner.register_executor("container-probe", Box::new(ProcessExecutor));
        runner.register_container_executor(Box::new(ContainerExecutor::new(scratch_root.clone(), extra_labels.clone())));

        let job_id = format!("job-start-failed-{run_id}");
        let spec = base_spec(&job_id, vec!["/no/such/binary-av-jobs-container-test".to_string()], &recorded_id);
        runner.queue().submit(&spec).unwrap_or_else(|e| panic!("{TEST_NAME}: submit: {e}"));
        let completion = runner.run_one(&spec).unwrap_or_else(|e| panic!("{TEST_NAME}: run_one: {e}"));
        assert!(!completion.ok, "{TEST_NAME}: expected the job to fail");
        let failure = completion.failure.as_ref().unwrap_or_else(|| panic!("{TEST_NAME}: ok==false but failure is unset"));
        assert_eq!(failure.kind, pb::JobFailureKind::ExecutorStartFailed as i32, "{TEST_NAME}: {failure:?}");

        let remaining = docker(&["ps", "-a", "--filter", &format!("label=av.job.id={job_id}"), "-q"]);
        assert_eq!(remaining, "", "{TEST_NAME}: container for job {job_id:?} still exists after the run: {remaining:?}");
    }

    // -- Part 4: the digest gate refuses a mismatch -- a deliberately wrong digest is never
    // trusted, never falls back to running the image anyway. --------------------------------
    {
        let dir = TempDir::new("digest-mismatch");
        let clock = TestClock::new(4_000);
        let (queue, _) = JobQueue::open(dir.path(), "q").unwrap_or_else(|e| panic!("{TEST_NAME}: JobQueue::open: {e}"));
        let source = Box::new(MemoryObjectSource::new());
        let sink = Box::new(MemoryObjectSink::new("jobs"));
        let mut runner = Runner::new(queue, source, sink, ladder(), &clock);
        // Step 1 of Runner::execute_spec looks up spec.kind in the ordinary kind registry
        // UNCONDITIONALLY, even for a CONTAINER job (crate::runner::Runner::
        // register_container_executor's own doc) -- a real, working ProcessExecutor is
        // registered here (never actually invoked -- the container executor is) exactly the
        // way crates/av-jobs/tests/runner.rs::
        // executor_unavailable_for_the_container_kind_is_recorded_on_the_log already does.
        runner.register_executor("container-probe", Box::new(ProcessExecutor));
        runner.register_container_executor(Box::new(ContainerExecutor::new(scratch_root.clone(), extra_labels.clone())));

        let job_id = format!("job-digest-mismatch-{run_id}");
        let wrong_digest = format!("sha256:{}", "0".repeat(64));
        let spec = base_spec(&job_id, vec!["/bin/echo".to_string(), "would have run".to_string()], &wrong_digest);
        runner.queue().submit(&spec).unwrap_or_else(|e| panic!("{TEST_NAME}: submit: {e}"));
        let completion = runner.run_one(&spec).unwrap_or_else(|e| panic!("{TEST_NAME}: run_one: {e}"));
        assert!(!completion.ok, "{TEST_NAME}: expected the job to be refused");
        let failure = completion.failure.as_ref().unwrap_or_else(|| panic!("{TEST_NAME}: ok==false but failure is unset"));
        assert_eq!(failure.kind, pb::JobFailureKind::ExecutorUnavailable as i32, "{TEST_NAME}: {failure:?}");
        assert!(failure.detail.contains("refused rather than trusted"), "{TEST_NAME}: {failure:?}");
        assert!(failure.detail.contains(&wrong_digest), "{TEST_NAME}: {failure:?}");

        // No container is ever created for a digest that never passed the gate.
        let remaining = docker(&["ps", "-a", "--filter", &format!("label=av.job.id={job_id}"), "-q"]);
        assert_eq!(remaining, "", "{TEST_NAME}: a digest-refused job must never create a container, but found: {remaining:?}");
    }

    // -- Question 156: nothing labelled with this run's id remains, checked only after every
    // part above's own cleanup has already run. No volume is ever created (bind mounts only),
    // so that filter is empty by construction. ------------------------------------------------
    let label_filter = format!("label=av.test.run_id={run_id}");
    let remaining_containers = docker(&["ps", "-a", "--filter", &label_filter, "-q"]);
    assert_eq!(remaining_containers, "", "container(s) labelled {label_filter} still exist after cleanup: {remaining_containers:?}");
    let remaining_volumes = docker(&["volume", "ls", "--filter", &label_filter, "-q"]);
    assert_eq!(remaining_volumes, "", "volume(s) labelled {label_filter} still exist (this executor never creates one): {remaining_volumes:?}");
}
