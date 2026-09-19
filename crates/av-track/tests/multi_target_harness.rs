//! D5a's own required proof (`src/harness.rs`'s own module doc; the task brief for this
//! milestone): "a functional end-to-end test of the multi-target path that does not need
//! docker." Starts two or three real, in-process `EdgeIngestService` `tonic` servers, each on
//! its own ephemeral loopback port and its own durable log directory (`av_track::harness::
//! spawn_in_process_server` -- the identical helper the binary's own in-process mode uses),
//! points `av_track::harness::drive_all` at all of them as `PlacementTarget`s, and asserts
//! every batch was accepted and the per-placement counts are right. No docker, no
//! subprocess, no network beyond loopback (question 154) -- this is the test the D5 container
//! run's second worker can trust the driving code path against before spending a quiet host
//! window on the real three-container run.
//!
//! Deliberately asserts **no latency threshold** anywhere in this file: a slow accepted batch
//! is still a passing run, exactly like `src/bin/av-edge-latency.rs`'s own instruction.

use std::path::PathBuf;
use std::sync::Arc;

use av_edge::pb;
use av_track::harness::{self, HarnessError, PlacementTarget};

/// Every call gets its own directory. The name used to be `<name>-<pid>` alone, and every test
/// in this binary runs in ONE process on parallel threads with the same placement labels
/// (`edge-a`, `edge-b`, ...), so two tests shared a directory and the later one's
/// `remove_dir_all` deleted the partition log under the earlier one's running server: the
/// lead's clone gate saw it twice (2026-09-18 `EINVAL`, 2026-09-19 `ENOENT`, both on `edge-b`),
/// passing alone every time. A process-wide counter makes the path unique per call.
fn tmp_dir(name: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("av-track-multi-target-{name}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// Builds the fixture's full batch sequence and truncates it to `n` -- a small `n` keeps this
/// test fast (this file needs the driving path exercised, not a 900-batch measurement).
fn small_batches(n: usize) -> (Vec<pb::MeasurementBatch>, harness::Fixture) {
    let fixture = harness::load_fixture();
    assert!(n <= fixture.batches.len(), "fixture only has {} batches", fixture.batches.len());
    let batches = fixture.batches[..n].to_vec();
    (batches, fixture)
}

/// Starts `labels.len()` real in-process servers, one per label, each under its own tmp log
/// directory, all sharing the one fixture producer's identity (`cfg.producer_id`/
/// `verify_key`) -- mirrors exactly what three real `av-ingest-server --verify-key ...`
/// subprocesses the second worker starts would each be configured with.
async fn spawn_placements(labels: &[&str], cfg: &av_edge::plugin::PluginConfig, verify_key: openssl::ec::EcKey<openssl::pkey::Public>) -> Vec<PlacementTarget> {
    let mut targets = Vec::with_capacity(labels.len());
    for label in labels {
        let dir = tmp_dir(label);
        let addr = harness::spawn_in_process_server(&dir, &cfg.producer_id, verify_key.clone()).await;
        targets.push(PlacementTarget { label: label.to_string(), addr: addr.to_string() });
    }
    targets
}

#[tokio::test]
async fn three_in_process_placements_all_accept_every_batch_driven_concurrently() {
    const BATCHES_PER_PLACEMENT: usize = 20;
    let (batches, fixture) = small_batches(BATCHES_PER_PLACEMENT);
    let manifest = fixture.cfg.manifest().unwrap();

    let targets = spawn_placements(&["edge-a", "edge-b", "edge-c"], &fixture.cfg, fixture.verify_key.clone()).await;
    let target_addrs: Vec<String> = targets.iter().map(|t| t.addr.clone()).collect();

    let batches = Arc::new(batches);
    let results = harness::drive_all(targets, Arc::clone(&batches), manifest, None, 1).await;

    assert_eq!(results.len(), 3, "one result per placement, in the order the targets were given");

    let expected_labels = ["edge-a", "edge-b", "edge-c"];
    for (i, result) in results.into_iter().enumerate() {
        let run = result.unwrap_or_else(|e| panic!("placement {}: {e}", expected_labels[i]));
        assert_eq!(run.label, expected_labels[i], "drive_all must return results in the same order as the targets it was given");
        assert_eq!(run.addr, target_addrs[i]);
        assert_eq!(run.batch_count, BATCHES_PER_PLACEMENT, "placement {}: batch_count", run.label);
        assert_eq!(run.accepted_count, BATCHES_PER_PLACEMENT, "placement {}: every batch must have been accepted", run.label);
        assert_eq!(run.measurement_count, BATCHES_PER_PLACEMENT, "this fixture's own batches carry exactly one measurement each (BatchingRule::PerEpoch)");
        assert_eq!(run.samples.len(), BATCHES_PER_PLACEMENT, "placement {}: one latency sample per accepted batch", run.label);
        // No latency threshold asserted anywhere here -- a slow accepted batch is still a
        // passing run (this crate's own standing rule, restated in this file's own doc).
    }
}

#[tokio::test]
async fn two_in_process_placements_each_get_their_own_independent_accepted_count() {
    const BATCHES_PER_PLACEMENT: usize = 7;
    let (batches, fixture) = small_batches(BATCHES_PER_PLACEMENT);
    let manifest = fixture.cfg.manifest().unwrap();

    let targets = spawn_placements(&["edge-a", "edge-b"], &fixture.cfg, fixture.verify_key.clone()).await;
    let batches = Arc::new(batches);
    let results = harness::drive_all(targets, batches, manifest, None, 1).await;

    assert_eq!(results.len(), 2);
    let runs: Vec<_> = results.into_iter().map(|r| r.unwrap()).collect();
    assert_eq!(runs[0].label, "edge-a");
    assert_eq!(runs[1].label, "edge-b");
    assert_ne!(runs[0].addr, runs[1].addr, "two distinct placements must be driven at two distinct addresses");
    for run in &runs {
        assert_eq!(run.accepted_count, BATCHES_PER_PLACEMENT);
    }
}

#[tokio::test]
async fn multi_target_path_with_pacing_enabled_still_accepts_every_batch() {
    // A declared rate high enough that the resulting per-batch interval is negligible (this
    // test's own point is to exercise the `interval_ns = Some(..)` branch of
    // `drive_placement`, not to measure real pacing -- that is `batch_interval_ns`'s and
    // `scheduled_due_at_ns`'s own job, pinned as pure functions in `src/harness.rs`'s own
    // `#[cfg(test)]` module with no sleeping at all).
    const BATCHES_PER_PLACEMENT: usize = 10;
    let (batches, fixture) = small_batches(BATCHES_PER_PLACEMENT);
    let manifest = fixture.cfg.manifest().unwrap();
    let interval_ns = harness::batch_interval_ns(1_000_000_000.0, fixture.measurements_per_batch);

    let targets = spawn_placements(&["edge-a", "edge-b"], &fixture.cfg, fixture.verify_key.clone()).await;
    let batches = Arc::new(batches);
    let results = harness::drive_all(targets, batches, manifest, Some(interval_ns), 1).await;

    for result in results {
        let run = result.unwrap();
        assert_eq!(run.accepted_count, BATCHES_PER_PLACEMENT);
    }
}

#[tokio::test]
async fn a_placement_with_no_server_listening_fails_loudly_as_a_typed_error() {
    let (batches, fixture) = small_batches(3);
    let manifest = fixture.cfg.manifest().unwrap();

    // Port 1 is a reserved/privileged port essentially never bound in a test sandbox --
    // mirrors `av-ingest-client`'s own identical-purpose test.
    let target = PlacementTarget { label: "nothing-listening".to_string(), addr: "127.0.0.1:1".to_string() };
    let results = harness::drive_all(vec![target], Arc::new(batches), manifest, None, 1).await;

    assert_eq!(results.len(), 1);
    let err = results.into_iter().next().unwrap().unwrap_err();
    assert!(matches!(err, HarnessError::Connect { .. }), "{err:?}");
}

/// Question 148's own required proof, extended onto this file's existing docker-free
/// multi-target integration test: `--in-flight N` (N>1) must still accept every batch, at
/// every placement, with the per-placement counts exactly right -- the same assertions
/// `three_in_process_placements_all_accept_every_batch_driven_concurrently` above makes for
/// `in_flight=1`, just with `in_flight=6` (more outstanding than any one placement has
/// batches at times, since `BATCHES_PER_PLACEMENT` is small here on purpose -- this test
/// needs the pipelined path exercised, not a large measurement).
#[tokio::test]
async fn in_flight_greater_than_one_still_accepts_every_batch_at_every_placement() {
    const BATCHES_PER_PLACEMENT: usize = 20;
    const IN_FLIGHT: usize = 6;
    let (batches, fixture) = small_batches(BATCHES_PER_PLACEMENT);
    let manifest = fixture.cfg.manifest().unwrap();

    // Distinct labels from `three_in_process_placements_all_accept_every_batch_driven_
    // concurrently` above -- `tmp_dir` keys its directory only by label + this test binary's
    // own pid (shared by every test in this file), so two tests reusing the same three
    // labels concurrently would race on the identical directory path.
    let targets = spawn_placements(&["edge-a-if", "edge-b-if", "edge-c-if"], &fixture.cfg, fixture.verify_key.clone()).await;
    let target_addrs: Vec<String> = targets.iter().map(|t| t.addr.clone()).collect();

    let batches = Arc::new(batches);
    let results = harness::drive_all(targets, Arc::clone(&batches), manifest, None, IN_FLIGHT).await;

    assert_eq!(results.len(), 3, "one result per placement, in the order the targets were given");
    let expected_labels = ["edge-a-if", "edge-b-if", "edge-c-if"];
    for (i, result) in results.into_iter().enumerate() {
        let run = result.unwrap_or_else(|e| panic!("placement {}: {e}", expected_labels[i]));
        assert_eq!(run.label, expected_labels[i]);
        assert_eq!(run.addr, target_addrs[i]);
        assert_eq!(run.batch_count, BATCHES_PER_PLACEMENT, "placement {}: batch_count", run.label);
        assert_eq!(run.accepted_count, BATCHES_PER_PLACEMENT, "placement {}: every batch must have been accepted with in_flight={IN_FLIGHT}", run.label);
        assert_eq!(run.measurement_count, BATCHES_PER_PLACEMENT);
        assert_eq!(run.samples.len(), BATCHES_PER_PLACEMENT, "placement {}: one latency sample per accepted batch, regardless of completion order under buffer_unordered", run.label);
    }
}

/// `--in-flight N` (N>1) combined with `--rate` (pacing enabled): every batch must still be
/// accepted, exercising both branches (`interval_ns = Some(..)` and the concurrent
/// `buffer_unordered` path) at once -- `drive_placement`'s own doc states `interval_ns` and
/// `in_flight` are independent knobs (the schedule is unchanged by how many batches are
/// outstanding), and this test is the proof.
#[tokio::test]
async fn in_flight_greater_than_one_with_pacing_enabled_still_accepts_every_batch() {
    const BATCHES_PER_PLACEMENT: usize = 12;
    const IN_FLIGHT: usize = 4;
    let (batches, fixture) = small_batches(BATCHES_PER_PLACEMENT);
    let manifest = fixture.cfg.manifest().unwrap();
    let interval_ns = harness::batch_interval_ns(1_000_000_000.0, fixture.measurements_per_batch);

    let targets = spawn_placements(&["edge-a-if-rate", "edge-b-if-rate"], &fixture.cfg, fixture.verify_key.clone()).await;
    let batches = Arc::new(batches);
    let results = harness::drive_all(targets, batches, manifest, Some(interval_ns), IN_FLIGHT).await;

    for result in results {
        let run = result.unwrap();
        assert_eq!(run.accepted_count, BATCHES_PER_PLACEMENT);
    }
}
