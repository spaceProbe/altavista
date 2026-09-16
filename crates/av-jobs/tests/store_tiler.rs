//! Task 3c (`docs/heavy-plan.md` H3's exit criterion): the real MinIO proof that a tiler
//! job's tile set actually round-trips through a real `av-store`/MinIO object store, under a
//! manifest whose SHA-256 is the tile set's own identity -- not merely through
//! `crate::runner::MemoryObjectSink`, the in-memory test double `tests/tiler.rs` (P1f) uses.
//!
//! # Template -- mirrored exactly, never a second way of doing any of this
//!
//! `crates/av-store/tests/minio_store.rs` is this file's own template (this task's own
//! brief: "Copy its gating, its lock discipline, its labelling, its prune-before-create, its
//! skip-line wording and its container lifecycle exactly"). The `services/store/
//! IMAGE_DIGEST.md` parsing (`image_digest_md_path`/`fenced_block_after`/
//! `parse_image_digest_md`/`gate`/`gate_or_skip!`), the container-readiness probe
//! (`probe_health_once`/`wait_for_health`), the label conversion
//! (`labels_from_test_label_args`) and the `docker(...)` helper below are that file's own
//! functions, copied character for character (the doc comments trimmed to what this file
//! adds, not repeated) rather than reimplemented a second, independently-drifting way -- see
//! that file's own doc comment for the full reasoning behind each ("never pulls", "gate/
//! skip/lock discipline", "bucket/prefix/credentials").
//!
//! # The fixture -- re-declared, not factored into `tests/common/`
//!
//! The fixture raster (`fixture_raster_bytes`), `fixture_params`, `KEY_PREFIX`, and the
//! pinned manifest hash are `crates/av-jobs/tests/tiler.rs`'s own (P1f) -- **re-declared
//! here** (a `tests/common/` module was the other option this task's brief allowed) because
//! `cargo test`'s `tests/*.rs` convention compiles each file as its own independent test
//! binary crate: a `tests/common/mod.rs` shared module buys nothing for exactly two values
//! this small and well-documented (a 4x2, 80-byte raster; three parameter strings), and would
//! cost a second file a reviewer has to open to see the fixture this file's own assertions
//! depend on. Every value below that must match `tests/tiler.rs`'s own fixture EXACTLY for
//! [`PINNED_MANIFEST_SHA256`] to reproduce (the raster bytes, `fixture_params()`, `KEY_PREFIX`
//! `"tiles"`, and the job id `"job-pin"` -- `TileSetManifest.job_id` is part of the encoded
//! manifest, so a different job id here would change the hash for a reason that has nothing
//! to do with the store backend) is called out at its own use site below.
//!
//! # The store/`ObjectSource`+`ObjectSink` bridge
//!
//! See [`StoreBridge`]'s own doc comment for the synchronous/async seam and why one
//! `tokio::runtime::Runtime` is built once and shared, never one per call.
//!
//! # A genuine finding: `TileEntry.uri` never reflects the real store
//!
//! `crates/av-jobs::tiler`'s own module doc already predicts this, in so many words: "This
//! round's only `ObjectSink` is `MemoryObjectSink`, which always returns
//! `"memory://{key}"`; [`TilerExecutor`] hard-codes that same `"memory://"` scheme prefix...
//! A real, store-backed `ObjectSink` (task 3b's own deferred P4) would need its own URI
//! scheme, at which point this hard-coded `"memory://"` assumption becomes something that
//! implementation must revisit explicitly." This test is that revisit, and the assumption
//! does not hold: `crate::tiler::TilerExecutor::run_imagery` builds every `TileEntry.uri` via
//! `format!("memory://{key}")` unconditionally -- it has no way to know, and does not ask,
//! what scheme the `Runner`'s actual configured `ObjectSink` will use. So every
//! `TileEntry.uri` this test's manifest carries reads `"memory://tiles/.../<hash>"` even
//! though the tile is, in fact, stored in this test's real MinIO container at
//! `"s3://<bucket>/tiles/.../<hash>"`. A real consumer of this manifest cannot use
//! `TileEntry.uri` to locate a tile in a store-backed deployment -- it would have to already
//! know (out of band) that the manifest's own `uri` field is a lie for any backend other than
//! `MemoryObjectSink`. This test therefore does NOT dereference `TileEntry.uri` at all: it
//! resolves each tile by matching `TileEntry.sha256` against `JobCompletion.outputs` (whose
//! `AssetRef.uri` fields ARE the real `s3://` locations [`StoreSink`] received back from
//! [`av_store::StoreClient::put`]), exactly as a caller who already knew about this gap would
//! have to. This is reported as a finding, not silently worked around: the fix belongs in
//! `crate::tiler::TilerExecutor` (a store-backed `Runner` needs some way to learn its sink's
//! own URI scheme, or the manifest needs a second pass after storage, or `ObjectSink::put`
//! needs to run before tile hashes are computed) and is out of this task's scope (this task
//! writes a test, not a production-code redesign of the manifest-vs-sink ordering).
//!
//! Despite that gap, [`PINNED_MANIFEST_SHA256`] itself still matches: the pin covers the
//! manifest's *bytes*, and every one of those bytes (including the `"memory://"` URIs) is
//! computed by `TilerExecutor` alone, entirely independently of which `ObjectSink` actually
//! ends up storing them -- so the tile-set identity this task's brief calls "the same tile
//! set identity the in-memory test produces" does not, in fact, depend on the storage
//! backend. That is a direct consequence of the same gap: the manifest's own content is
//! backend-blind, which is exactly why its `uri` fields are wrong for any backend but the one
//! `MemoryObjectSink` implements.

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use av_cdm::pb;
use av_jobs::clock::TestClock;
use av_jobs::queue::JobQueue;
use av_jobs::runner::{content_addressed_key, ObjectSink, ObjectSource, Runner};
use av_jobs::tiler::TilerExecutor;
use av_label::ClearanceLadder;
use av_lockstep::docker::{
    announce_gate_skip, lock_docker_tests, prune_stale_test_resources, recorded_digest_gate, test_label_args, test_run_id, DockerGateReason, ManagedContainer,
};
use av_store::{StoreClient, StoreConfig};
use bytes::Bytes;
use openssl::sha::sha256;

const CONTAINER_PORT: u16 = 9000;
const READY_TIMEOUT: Duration = Duration::from_secs(30);
const REGION: &str = "us-east-1";
const BUCKET: &str = "av-jobs-store-tiler-test";
/// Must equal `crates/av-jobs/tests/tiler.rs`'s own `KEY_PREFIX` -- see this file's own
/// module doc, "The fixture -- re-declared, not factored into `tests/common/`".
const KEY_PREFIX: &str = "tiles";
const RASTER_MEDIA_TYPE: &str = "application/vnd.altavista.raster+raw";
/// `tests/tiler.rs::the_manifest_hash_is_pinned_for_the_fixture`'s own pinned value, for the
/// identical fixture/params/`KEY_PREFIX`/job id -- see this file's own module doc for why a
/// real store backend does not change it.
const PINNED_MANIFEST_SHA256: &str = "cbf064bcbf6f8450a5b3b7cc5e0a246adb6db8c26ee1cf2db0dc7189fd9fe5c4";
/// `tests/tiler.rs`'s own pinned-hash test job id -- part of the encoded `TileSetManifest`
/// (`job_id` field), so it must match exactly, not merely be "a" valid job id.
const PINNED_JOB_ID: &str = "job-pin";

// -----------------------------------------------------------------------------------------
// services/store/IMAGE_DIGEST.md parsing -- copied from crates/av-store/tests/minio_store.rs
// verbatim (this file's own module doc, "Template"). The recorded digest has exactly one
// home; this function derives its path from CARGO_MANIFEST_DIR (av-jobs's own manifest dir,
// crates/av-jobs, the same depth as av-store's) rather than baking in an absolute path.
// -----------------------------------------------------------------------------------------

fn image_digest_md_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../services/store/IMAGE_DIGEST.md")
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
/// what `services/store/IMAGE_DIGEST.md` recorded -- identical check to
/// `crates/av-store/tests/minio_store.rs::gate`, against the same recorded file.
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

// -----------------------------------------------------------------------------------------
// Fixture: one MinIO container + one StoreClient pointed at it -- mirrors
// crates/av-store/tests/minio_store.rs's own MinioFixture/start_fixture.
// -----------------------------------------------------------------------------------------

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).expect("system clock before 1970").as_secs() as i64
}

fn docker(args: &[&str]) -> String {
    let output = Command::new("docker").args(args).output().unwrap_or_else(|e| panic!("could not launch `docker {args:?}`: {e}"));
    if !output.status.success() {
        panic!("`docker {args:?}` failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn probe_health_once(host_port: u16) -> Result<(), String> {
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", host_port)).map_err(|e| format!("connect: {e}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    let request = format!("GET /minio/health/live HTTP/1.1\r\nHost: 127.0.0.1:{host_port}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).map_err(|e| format!("write: {e}"))?;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).map_err(|e| format!("read: {e}"))?;
    let text = String::from_utf8_lossy(&buf);
    let status_line = text.lines().next().unwrap_or("").to_string();
    if status_line.contains(" 200") {
        Ok(())
    } else {
        Err(format!("status line: {status_line:?}"))
    }
}

fn wait_for_health(host_port: u16, container_id: &str, test_name: &str) {
    let deadline = Instant::now() + READY_TIMEOUT;
    let mut last_err;
    loop {
        match probe_health_once(host_port) {
            Ok(()) => return,
            Err(e) => last_err = e,
        }
        if Instant::now() > deadline {
            let logs = docker(&["logs", container_id]);
            panic!("{test_name}: MinIO at 127.0.0.1:{host_port} (container {container_id}) did not answer GET /minio/health/live with 200 within {READY_TIMEOUT:?}; last error: {last_err}; docker logs:\n{logs}");
        }
        std::thread::sleep(Duration::from_millis(150));
    }
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

/// Starts a fresh, labelled, loopback-only MinIO container (via [`ManagedContainer::
/// run_local`] -- no `docker pull`, question 154), waits for it to answer its health check,
/// and builds a [`StoreClient`] against it with a freshly `ensure_bucket`d bucket. Mirrors
/// `crates/av-store/tests/minio_store.rs::start_fixture`, minus the fields that test file
/// needs and this one does not (`access_key`/`secret_key`/`endpoint` -- this file never
/// builds a second, raw-signed request the way `minio_store.rs`'s own corruption test does).
fn start_store_fixture(image_ref: &str, test_name: &str) -> (ManagedContainer, String, StoreClient) {
    let run_id = test_run_id();
    let labels = labels_from_test_label_args(&run_id);

    let sanitized_run_id: String = run_id.chars().filter(char::is_ascii_alphanumeric).collect();
    let access_key = format!("avjobstest{sanitized_run_id}");
    let secret_key = format!("avjobstestsecret{sanitized_run_id}");

    let mut env = BTreeMap::new();
    env.insert("MINIO_ROOT_USER".to_string(), access_key.clone());
    env.insert("MINIO_ROOT_PASSWORD".to_string(), secret_key.clone());

    let (container, host_port) = ManagedContainer::run_local(image_ref, &["server".to_string(), "/data".to_string()], CONTAINER_PORT, &BTreeMap::new(), &env, &BTreeMap::new(), &labels)
        .unwrap_or_else(|e| panic!("{test_name}: ManagedContainer::run_local({image_ref:?}) failed: {e}"));
    let container_id = container.container_id.clone();

    wait_for_health(host_port, &container_id, test_name);

    let config = StoreConfig {
        endpoint: format!("http://127.0.0.1:{host_port}").parse().unwrap_or_else(|e| panic!("{test_name}: building the endpoint URI: {e}")),
        region: REGION.to_string(),
        access_key_id: access_key,
        secret_access_key: secret_key,
        bucket: BUCKET.to_string(),
        force_path_style: true,
        ca_file: None,
        key_prefix: KEY_PREFIX.to_string(),
    };
    let client = StoreClient::new(config).unwrap_or_else(|e| panic!("{test_name}: StoreClient::new: {e}"));

    (container, container_id, client)
}

// -----------------------------------------------------------------------------------------
// The fixture raster / params -- re-declared from crates/av-jobs/tests/tiler.rs. See this
// file's own module doc, "The fixture -- re-declared, not factored into `tests/common/`".
// -----------------------------------------------------------------------------------------

/// Byte for byte identical to `tests/tiler.rs::fixture_raster_bytes` -- see that function's
/// own doc comment for the full AVRASTER layout this builds (4x2 RGB8, whole-globe bounds).
fn fixture_raster_bytes() -> Vec<u8> {
    let mut out = Vec::with_capacity(80);
    out.extend_from_slice(b"AVRASTER");
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&4u32.to_le_bytes());
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&3u32.to_le_bytes());
    out.extend_from_slice(&(-180.0f64).to_le_bytes());
    out.extend_from_slice(&(-90.0f64).to_le_bytes());
    out.extend_from_slice(&(180.0f64).to_le_bytes());
    out.extend_from_slice(&(90.0f64).to_le_bytes());
    let pixels: [u8; 24] = [10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120, 130, 140, 150, 160, 170, 180, 190, 200, 210, 220, 230, 240];
    out.extend_from_slice(&pixels);
    out
}

/// Byte for byte identical to `tests/tiler.rs::fixture_params` -- min_level=0, max_level=1,
/// tile_size=16.
fn fixture_params() -> BTreeMap<String, String> {
    let mut p = BTreeMap::new();
    p.insert("min_level".to_string(), "0".to_string());
    p.insert("max_level".to_string(), "1".to_string());
    p.insert("tile_size".to_string(), "16".to_string());
    p
}

fn valid_label() -> pb::Label {
    pb::Label { marking: "CUI".to_string(), caveats: vec![] }
}

fn ladder() -> ClearanceLadder {
    ClearanceLadder::new(vec!["UNCLASSIFIED".to_string(), "CUI".to_string(), "SECRET".to_string()])
}

static TEMPDIR_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A per-call, auto-removed temp directory for a `JobQueue` -- mirrors
/// `tests/tiler.rs::TempDir`.
struct TempDir(PathBuf);
impl TempDir {
    fn new(tag: &str) -> Self {
        let n = TEMPDIR_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("av-jobs-store-tiler-test-{tag}-{}-{n}", std::process::id()));
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

// -----------------------------------------------------------------------------------------
// The store/ObjectSource+ObjectSink bridge.
// -----------------------------------------------------------------------------------------

/// Bridges `av-jobs`'s synchronous [`ObjectSource`]/[`ObjectSink`] seam
/// (`crate::runner`'s own doc comment on both traits) to `av-store`'s async [`StoreClient`].
///
/// **Why synchronous was the right shape for `av-jobs` in the first place**, per
/// `crate::runner`'s own doc on [`ObjectSource`]/[`ObjectSink`] and `Cargo.toml`'s "Deliberately
/// NOT a dependency" note on `tokio`: `Runner::run_one`'s whole call stack -- fetch, hash-verify,
/// execute, store -- is synchronous, and nothing in H3a's own scope needs concurrency within one
/// job run; a caller that only wants to drain the queue (a `ProcessExecutor`-only deployment, or
/// this crate's own unit tests) should never be forced to pull in an async runtime just to call
/// `Runner::run_one`. This bridge is exactly the kind of caller `crate::runner::ObjectSource`'s
/// own doc comment names -- "task 3b's own crate depends on both `av-jobs` and `av-store` to
/// provide [a real, store-backed implementation]" -- and it is this test binary, not `av-jobs`
/// itself, that pays the async cost: `av-jobs/src/**` still has no dependency on `tokio` or
/// `av-store` at all (`Cargo.toml`'s dev-dependency comment).
///
/// **One `Runtime`, built once, `block_on` per call.** `StoreClient::put`/`get`/`ensure_bucket`
/// are `async fn`s; this type owns exactly one `tokio::runtime::Runtime` (built by the test
/// function below, before any job runs) and calls [`tokio::runtime::Runtime::block_on`] once per
/// `fetch`/`put` -- never a fresh `Runtime::new()` per call (which would pay full thread-pool
/// spin-up/-down cost on every one of a job's inputs/outputs, of which a real tile set has many),
/// and never from inside an already-async context: every call into this bridge originates from
/// `Runner::run_one`'s own synchronous call stack, itself called directly from this file's plain
/// `#[test]` function body (not a `#[tokio::test]`), so `block_on` is never nested inside another
/// `block_on` or an executing `async fn` -- nesting it would panic ("Cannot start a runtime from
/// within a runtime").
struct StoreBridge {
    runtime: tokio::runtime::Runtime,
    client: StoreClient,
    ladder: ClearanceLadder,
    caller_clearance: String,
}

impl std::fmt::Debug for StoreBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoreBridge").finish_non_exhaustive()
    }
}

impl StoreBridge {
    fn fetch(&self, asset: &pb::AssetRef) -> Result<Vec<u8>, pb::JobFailure> {
        let bytes = self
            .runtime
            .block_on(self.client.get(asset, &self.caller_clearance, &self.ladder, now()))
            .map_err(|e| pb::JobFailure { kind: pb::JobFailureKind::InputMissing as i32, detail: format!("store get {:?}: {e}", asset.uri), exit_code: 0 })?;
        Ok(bytes.to_vec())
    }

    /// Stores `bytes` through the real [`StoreClient::put`] and returns the [`pb::AssetRef`]
    /// **it** returns -- never one this bridge constructs itself (this file's own brief: "The
    /// sink's `AssetRef` must be the one `av_store` itself returns from `put`, never one you
    /// construct").
    fn put(&self, bytes: &[u8], media_type: &str, label: &pb::Label) -> Result<pb::AssetRef, pb::JobFailure> {
        self.runtime
            .block_on(self.client.put(Bytes::copy_from_slice(bytes), media_type, label.clone(), pb::Provenance::default(), now()))
            .map_err(|e| pb::JobFailure { kind: pb::JobFailureKind::OutputRejected as i32, detail: format!("store put: {e}"), exit_code: 0 })
    }
}

#[derive(Debug, Clone)]
struct StoreSource(Arc<StoreBridge>);
impl ObjectSource for StoreSource {
    fn fetch(&self, asset: &pb::AssetRef) -> Result<Vec<u8>, pb::JobFailure> {
        self.0.fetch(asset)
    }
}

#[derive(Debug, Clone)]
struct StoreSink(Arc<StoreBridge>);
impl ObjectSink for StoreSink {
    fn put(&self, bytes: &[u8], media_type: &str, label: &pb::Label) -> Result<pb::AssetRef, pb::JobFailure> {
        self.0.put(bytes, media_type, label)
    }
}

/// The `JobSpec` this file's own test submits: identical shape to
/// `tests/tiler.rs::tiler_spec`, except `inputs` names the REAL, already-stored
/// [`pb::AssetRef`] `client.put` returned for the fixture raster (a real `s3://` `uri`, not
/// `tests/tiler.rs`'s own `"memory://input-raster"` constant).
fn tiler_spec(job_id: &str, raster_asset: pb::AssetRef) -> pb::JobSpec {
    pb::JobSpec {
        job_id: job_id.to_string(),
        kind: "tiler".to_string(),
        inputs: vec![raster_asset],
        parameters: fixture_params(),
        label: Some(valid_label()),
        requested_tai_ns: 1,
        executor: pb::JobExecutorKind::Process as i32,
        ..Default::default()
    }
}

// -----------------------------------------------------------------------------------------
// Always-on unit test: av_jobs::runner::content_addressed_key and av_store::object_key must
// agree, for a real hash -- the mirror crate::runner::content_addressed_key's own doc comment
// already claims in prose, checked mechanically here so a future drift fails a test instead
// of only a stale comment. Not docker-gated: neither function does any I/O.
// -----------------------------------------------------------------------------------------

#[test]
fn content_addressed_key_matches_av_store_object_key_for_a_real_hash() {
    let hash = av_jobs::hash::hex_encode(&sha256(b"store_tiler.rs content_addressed_key parity probe"));
    let via_jobs = content_addressed_key(KEY_PREFIX, &hash);
    let via_store = av_store::object_key(KEY_PREFIX, &hash).unwrap_or_else(|e| panic!("av_store::object_key({KEY_PREFIX:?}, {hash:?}): {e}"));
    assert_eq!(via_jobs, via_store, "av_jobs::runner::content_addressed_key and av_store::keys::object_key must derive the identical layout for the identical (prefix, hash)");
}

// -----------------------------------------------------------------------------------------
// The proof: a tiler job's tile set round-trips through a real MinIO, under a manifest whose
// hash is the tile set's identity.
// -----------------------------------------------------------------------------------------

#[test]
fn tiler_job_round_trips_through_a_real_minio_store() {
    const TEST_NAME: &str = "tiler_job_round_trips_through_a_real_minio_store";
    gate_or_skip!(TEST_NAME);
    let (image_ref, _recorded_id) = parse_image_digest_md();
    let _lock = lock_docker_tests();
    prune_stale_test_resources(&_lock);
    let (_container, _container_id, client) = start_store_fixture(&image_ref, TEST_NAME);

    let runtime = tokio::runtime::Runtime::new().unwrap_or_else(|e| panic!("{TEST_NAME}: building the tokio Runtime: {e}"));
    runtime.block_on(client.ensure_bucket(now())).unwrap_or_else(|e| panic!("{TEST_NAME}: ensure_bucket: {e}"));

    // Step 4 of this file's own brief: put the fixture raster into the store FIRST, so the
    // job's own input AssetRef is a real stored object with a real (s3://) uri -- never the
    // memory:// URI tests/tiler.rs's own fixture uses.
    let raster_asset = runtime
        .block_on(client.put(Bytes::from(fixture_raster_bytes()), RASTER_MEDIA_TYPE, valid_label(), pb::Provenance::default(), now()))
        .unwrap_or_else(|e| panic!("{TEST_NAME}: put(fixture raster): {e}"));
    assert!(raster_asset.uri.starts_with(&format!("s3://{BUCKET}/")), "the fixture raster's own AssetRef.uri must be a real s3:// location, got {:?}", raster_asset.uri);

    let bridge = Arc::new(StoreBridge { runtime, client, ladder: ladder(), caller_clearance: "SECRET".to_string() });

    let dir = TempDir::new("store-tiler");
    let clock = TestClock::new(4_000);
    let (queue, _) = JobQueue::open(dir.path(), "q").unwrap_or_else(|e| panic!("{TEST_NAME}: JobQueue::open: {e}"));
    let mut runner = Runner::new(queue, Box::new(StoreSource(bridge.clone())), Box::new(StoreSink(bridge.clone())), ladder(), &clock);
    runner.register_executor("tiler", Box::new(TilerExecutor::new(KEY_PREFIX)));

    let spec = tiler_spec(PINNED_JOB_ID, raster_asset);
    runner.queue().submit(&spec).unwrap_or_else(|e| panic!("{TEST_NAME}: submit: {e}"));
    let completion = runner.run_one(&spec).unwrap_or_else(|e| panic!("{TEST_NAME}: run_one: {e}"));

    // -- assertion 1: the job actually succeeded. ---------------------------------------
    assert!(completion.ok, "{TEST_NAME}: job did not complete ok: {completion:?}");

    // -- assertion 2: the manifest hash is the SAME tile-set identity the in-memory test
    // produces -- see this file's own module doc, "A genuine finding", for why this DOES
    // match despite TileEntry.uri being wrong for this backend.
    assert_eq!(
        completion.manifest_sha256, PINNED_MANIFEST_SHA256,
        "{TEST_NAME}: manifest_sha256 did not match the pin -- see this file's own module doc \
         (\"A genuine finding\") for the one already-known reason this could legitimately \
         differ (TileEntry.uri is backend-blind, but that alone does not change the hash); a \
         MISMATCH here is itself the finding to report, with this actual value: {}",
        completion.manifest_sha256
    );

    // -- fetch the manifest bytes back from the REAL store, via the real AssetRef
    // JobCompletion.outputs carries (never via TileEntry.uri -- see this file's own module
    // doc, "A genuine finding"). --------------------------------------------------------
    let manifest_output = completion
        .outputs
        .iter()
        .find(|a| a.media_type == av_jobs::tiler::MANIFEST_MEDIA_TYPE)
        .unwrap_or_else(|| panic!("{TEST_NAME}: no manifest output (media_type == {:?}) among {:?}", av_jobs::tiler::MANIFEST_MEDIA_TYPE, completion.outputs));
    assert_eq!(manifest_output.sha256, completion.manifest_sha256, "the manifest output's own AssetRef.sha256 must equal JobCompletion.manifest_sha256");
    assert!(manifest_output.uri.starts_with(&format!("s3://{BUCKET}/")), "the manifest's own AssetRef.uri must be a real s3:// location, got {:?}", manifest_output.uri);

    let manifest_bytes = bridge.fetch(manifest_output).unwrap_or_else(|e| panic!("{TEST_NAME}: fetching the manifest object back from the real store: {e:?}"));

    // -- assertion 3: the manifest object itself is really in the store, and its OWN bytes'
    // sha256 equals JobCompletion.manifest_sha256 -- recomputed here, not trusted from the
    // AssetRef alone. -------------------------------------------------------------------
    let manifest_bytes_sha256_hex = av_jobs::hash::hex_encode(&sha256(&manifest_bytes));
    assert_eq!(manifest_bytes_sha256_hex, completion.manifest_sha256, "the manifest bytes actually fetched back from MinIO must hash to JobCompletion.manifest_sha256");

    let manifest: pb::TileSetManifest = prost::Message::decode(manifest_bytes.as_slice()).unwrap_or_else(|e| panic!("{TEST_NAME}: decoding the fetched manifest bytes as TileSetManifest: {e}"));
    assert!(!manifest.tiles.is_empty(), "{TEST_NAME}: the fetched manifest names no tiles at all");

    // -- assertion 4: every TileEntry.sha256/size_bytes in the manifest resolves against a
    // REAL object in the store -- resolved via JobCompletion.outputs (real s3:// AssetRefs),
    // never via TileEntry.uri (see this file's own module doc, "A genuine finding"). -----
    for tile in &manifest.tiles {
        let stored = completion
            .outputs
            .iter()
            .find(|a| a.sha256 == tile.sha256)
            .unwrap_or_else(|| panic!("{TEST_NAME}: tile (level={}, x={}, y={}) sha256 {:?} has no matching JobCompletion.outputs entry", tile.level, tile.x, tile.y, tile.sha256));
        assert!(stored.uri.starts_with(&format!("s3://{BUCKET}/")), "tile output AssetRef.uri must be a real s3:// location, got {:?}", stored.uri);

        let tile_bytes = bridge.fetch(stored).unwrap_or_else(|e| panic!("{TEST_NAME}: fetching tile (level={}, x={}, y={}) back from the real store: {e:?}", tile.level, tile.x, tile.y));
        assert_eq!(tile_bytes.len() as u64, tile.size_bytes, "tile (level={}, x={}, y={}): fetched byte length must equal TileEntry.size_bytes", tile.level, tile.x, tile.y);
        let recomputed = av_jobs::hash::hex_encode(&sha256(&tile_bytes));
        assert_eq!(recomputed, tile.sha256, "tile (level={}, x={}, y={}): recomputed sha256 of the fetched bytes must equal TileEntry.sha256", tile.level, tile.x, tile.y);
    }

    // -- assertion 5: at least one tile's bytes are BYTE-IDENTICAL (not just hash-equal) to
    // what an in-memory run of the identical fixture/params/KEY_PREFIX/job id produces --
    // proving this is a byte equality, not only a hash equality that a content-addressed
    // coincidence could paper over. -------------------------------------------------------
    let (in_memory_completion, in_memory_sink) = run_in_memory_for_comparison();
    assert!(in_memory_completion.ok, "{TEST_NAME}: in-memory comparison run did not complete ok: {in_memory_completion:?}");
    assert_eq!(in_memory_completion.manifest_sha256, PINNED_MANIFEST_SHA256, "sanity: the in-memory comparison run must itself reproduce the pin");

    let in_memory_tile = in_memory_completion.outputs.iter().find(|a| a.media_type == av_jobs::tiler::IMAGERY_TILE_MEDIA_TYPE).expect("in-memory run must produce at least one imagery tile");
    let store_tile = completion
        .outputs
        .iter()
        .find(|a| a.sha256 == in_memory_tile.sha256)
        .unwrap_or_else(|| panic!("{TEST_NAME}: no store-run tile shares the in-memory run's own tile hash {:?} -- the two runs produced different tile content", in_memory_tile.sha256));
    let store_tile_bytes = bridge.fetch(store_tile).unwrap_or_else(|e| panic!("{TEST_NAME}: fetching the comparison tile back from the real store: {e:?}"));
    let in_memory_tile_bytes = in_memory_sink.get(&in_memory_tile.sha256).expect("the in-memory sink must have actually stored this tile's bytes");
    assert_eq!(store_tile_bytes, in_memory_tile_bytes, "{TEST_NAME}: the real-store tile's bytes must be BYTE-IDENTICAL to the in-memory run's own tile bytes for the same hash, not merely hash-equal");

    // Container teardown (before the lock is released): `_container` was declared before
    // `_lock` is dropped below is impossible -- `_lock` was declared FIRST, so by Rust's own
    // LIFO drop order, `_container`'s Drop (ManagedContainer::stop_and_remove) runs before
    // `_lock`'s, exactly as crates/av-store/tests/minio_store.rs's own tests 1/2/4/5 rely on.
}

#[derive(Debug, Clone)]
struct SharedMemorySink(Arc<av_jobs::runner::MemoryObjectSink>);
impl ObjectSink for SharedMemorySink {
    fn put(&self, bytes: &[u8], media_type: &str, label: &pb::Label) -> Result<pb::AssetRef, pb::JobFailure> {
        self.0.put(bytes, media_type, label)
    }
}

/// Runs the identical fixture/params/`KEY_PREFIX`/job id through `crate::runner::
/// MemoryObjectSource`/`MemoryObjectSink` -- `tests/tiler.rs`'s own in-memory path, re-run
/// here (not imported: `tests/tiler.rs` is a separate test binary crate) so this file's own
/// assertions can compare a real-store tile's bytes against it directly, in the same process.
/// Returns the completion alongside the `Arc<MemoryObjectSink>` that actually stored its
/// outputs, so a caller can read the stored bytes back (`MemoryObjectSink::get`) without this
/// function needing to smuggle that handle out any other way.
fn run_in_memory_for_comparison() -> (pb::JobCompletion, Arc<av_jobs::runner::MemoryObjectSink>) {
    const RASTER_URI: &str = "memory://input-raster";
    let dir = TempDir::new("store-tiler-in-memory");
    let clock = TestClock::new(4_000);
    let (queue, _) = JobQueue::open(dir.path(), "q").unwrap_or_else(|e| panic!("run_in_memory_for_comparison: JobQueue::open: {e}"));
    let source = Box::new(av_jobs::runner::MemoryObjectSource::new());
    source.insert(RASTER_URI, fixture_raster_bytes());
    let sink = Arc::new(av_jobs::runner::MemoryObjectSink::new(KEY_PREFIX));

    let mut runner = Runner::new(queue, source, Box::new(SharedMemorySink(sink.clone())), ladder(), &clock);
    runner.register_executor("tiler", Box::new(TilerExecutor::new(KEY_PREFIX)));

    let spec = pb::JobSpec {
        job_id: PINNED_JOB_ID.to_string(),
        kind: "tiler".to_string(),
        inputs: vec![pb::AssetRef {
            uri: RASTER_URI.to_string(),
            sha256: av_jobs::hash::hex_encode(&sha256(&fixture_raster_bytes())),
            size_bytes: fixture_raster_bytes().len() as u64,
            media_type: RASTER_MEDIA_TYPE.to_string(),
            ..Default::default()
        }],
        parameters: fixture_params(),
        label: Some(valid_label()),
        requested_tai_ns: 1,
        executor: pb::JobExecutorKind::Process as i32,
        ..Default::default()
    };
    runner.queue().submit(&spec).unwrap_or_else(|e| panic!("run_in_memory_for_comparison: submit: {e}"));
    let completion = runner.run_one(&spec).unwrap_or_else(|e| panic!("run_in_memory_for_comparison: run_one: {e}"));
    (completion, sink)
}
