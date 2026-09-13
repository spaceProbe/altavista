//! Milestone E6 (`docs/edge-plan.md`): "the edge buffers signed batches in a local file
//! log for hours, replays them in order on reconnect, and the ingest deduplicates by
//! (producer, sequence) so a replay never double-counts." This file covers the in-process
//! half of E6's own test list:
//!
//! - (a) [`the_ingests_log_is_byte_identical_across_an_interrupted_and_uninterrupted_run`]
//!   -- the centrepiece: an uninterrupted run and a cut-then-restored run over the exact
//!   same already-signed batches produce byte-identical partition log files.
//! - (b) [`the_duplicate_counter_absorbs_a_replay_overlap_from_an_unpersisted_watermark`]
//!   -- a deliberately interrupted drain (`av_edge::buffer`'s own documented mechanism)
//!   replays an already-accepted prefix a second time; `duplicate_count`, never
//!   `accepted`, absorbs it, pinned to exact numbers.
//! - (e) [`buffering_survives_a_multi_hour_disconnection_with_no_loss`], plus the STALE
//!   interaction this milestone's own task brief calls out by name: a short-`max_age_ns`
//!   demonstration ([`a_short_lived_policys_max_age_ns_rejects_a_replay_after_hours_as_stale`])
//!   proving the defect is real, and the main test using a policy whose `max_age_ns` is
//!   deliberately sized to cover the deployment's own declared disconnection tolerance --
//!   see [`capture_only_policy`]'s own doc comment for that decision's reasoning.
//!
//! The "link" here is simulated by [`ControllableIngestSink`], a `BatchSink` wrapping an
//! in-process `Ingest` directly (`av-edge` cannot depend on `av-ingest`, so this impl
//! lives here, in a downstream crate -- `av_edge::buffer`'s own module doc names this file
//! as exactly where it belongs) with a caller-controlled "up budget": the whole simulation
//! is driven by `av_edge::buffer::UplinkDriver::step` plus this file's own injected clock
//! values, never a real socket and never a sleep (question 154/199).

use std::path::PathBuf;

use av_edge::buffer::{BatchSink, EdgeBuffer, SinkOutcome, StepOutcome, UplinkDriver};
use av_edge::policy::ProducerPolicy;
use av_edge::{hash, pb, sign};
use av_ingest::ingest::{Ingest, IngestOutcome, Signer};
use openssl::ec::EcKey;
use openssl::pkey::{Private, Public};

const TEST_KEY_PEM: &[u8] = include_bytes!("fixtures/test_signing_key.pem");
const TEST_PUB_PEM: &[u8] = include_bytes!("fixtures/test_signing_key.pub.pem");
const PRODUCER_ID: &str = "e6-capture-only-producer";
const SHARD_KEY: &str = "e6-disconnect-shard";

fn keys() -> (EcKey<Private>, EcKey<Public>) {
    (sign::load_signing_key(TEST_KEY_PEM).unwrap(), av_edge::verify::load_verifying_key(TEST_PUB_PEM).unwrap())
}

/// This deployment's policy for a producer it knows is capture-only-while-disconnected.
///
/// **The decision, and why:** `av_edge::policy::ProducerPolicy::max_age_ns` is "the oldest
/// a batch's `batch_tai_ns` may be ... before it is refused as STALE" -- a deployment
/// concern (that struct's own doc comment), not a fact `av_edge`/`av_ingest` bake in. A
/// producer this deployment knows will buffer for hours while disconnected (E6's own
/// charter, verbatim: "for hours") and replay everything on reconnect will, by
/// construction, submit batches whose `batch_tai_ns` is legitimately that old by the time
/// the ingest ever sees them -- STALE exists to catch a batch that is suspiciously old for
/// reasons *other* than a known, declared disconnection tolerance (a stuck clock, a
/// misconfigured producer), not to reject a replay that is old for the one reason this
/// very deployment planned for. `a_short_lived_policys_max_age_ns_rejects_a_replay_after_hours_as_stale`
/// below proves the failure is real when this budget is left too short; `max_age_ns` is
/// set here to `DISCONNECT_BUDGET_NS`, comfortably past the longest disconnection any test
/// in this file actually drives the clock across, exactly the deployment-configuration fix
/// this milestone's own task brief asks for -- no change to `av_edge::chain`,
/// `av_edge::policy`, or `av_ingest::ingest` was needed or made.
const DISCONNECT_BUDGET_NS: i64 = 6 * 3600 * 1_000_000_000; // 6 hours of injected TAI time.

fn capture_only_policy() -> ProducerPolicy {
    ProducerPolicy::new(PRODUCER_ID, "CUI", vec![], vec!["UNCLASSIFIED".to_string(), "CUI".to_string()], "CUI", DISCONNECT_BUDGET_NS).unwrap()
}

fn tmp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("av-ingest-test-e6-disconnect-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// A single producer's hand-built, hand-signed chain of `count` batches, epochs spread
/// `epoch_step_ns` apart starting at `start_tai_ns` -- no `av_edge::plugin`/`PortTrafficLog`
/// fixture needed here, since this file is testing the disconnect/replay mechanism itself,
/// not measurement decoding (already covered by E4/E5's own tests). Every batch carries
/// zero measurements (a valid, heartbeat-shaped batch per `edge.proto`'s own doc comment)
/// -- signed **exactly once**: see the module doc on
/// `the_ingests_log_is_byte_identical_across_an_interrupted_and_uninterrupted_run` for why
/// that single pass, cloned everywhere it is needed, is the whole test's load-bearing
/// precondition.
fn build_chain(count: u64, start_tai_ns: i64, epoch_step_ns: i64, key: &EcKey<Private>) -> Vec<pb::MeasurementBatch> {
    let mut batches = Vec::with_capacity(count as usize);
    let mut prev_hash = hash::GENESIS.to_vec();
    for i in 0..count {
        let sequence = i + 1;
        let mut b = pb::MeasurementBatch {
            producer_id: PRODUCER_ID.to_string(),
            sequence,
            label: Some(pb::Label { marking: "CUI".to_string(), caveats: vec![] }),
            batch_tai_ns: start_tai_ns + (i as i64) * epoch_step_ns,
            shard_key: SHARD_KEY.to_string(),
            ..Default::default()
        };
        sign::sign_batch(&mut b, &prev_hash, key).unwrap();
        prev_hash = b.batch_hash.clone();
        batches.push(b);
    }
    batches
}

fn fresh_ingest(dir: &std::path::Path) -> Ingest {
    let mut ingest = Ingest::new(dir, None);
    ingest.register_producer(capture_only_policy());
    ingest
}

fn partition_log_bytes(ingest: &Ingest) -> Vec<u8> {
    let (_, log) = ingest.partitions().find(|(k, _)| k.as_str() == SHARD_KEY).expect("the shard partition must have been opened by at least one accepted batch");
    std::fs::read(log.path()).unwrap()
}

/// [`BatchSink`] over an in-process `Ingest`, with a caller-controlled "up budget": `Some(k)`
/// means the next `k` submit calls reach the ingest and then this sink reports
/// [`SinkOutcome::LinkDown`] until the budget is changed again; `None` means always up.
/// This -- not any real socket -- is the entire "link cut" simulation for this file; see
/// this file's own module doc.
struct ControllableIngestSink<'a> {
    ingest: &'a mut Ingest,
    verify_key: EcKey<Public>,
    up_budget: Option<usize>,
}

impl BatchSink for ControllableIngestSink<'_> {
    type Verdict = pb::BatchVerdict;
    type Error = String;

    fn submit(&mut self, batch: &pb::MeasurementBatch, now_tai_ns: i64) -> Result<SinkOutcome<pb::BatchVerdict>, String> {
        if let Some(budget) = self.up_budget {
            if budget == 0 {
                return Ok(SinkOutcome::LinkDown);
            }
            self.up_budget = Some(budget - 1);
        }
        match self.ingest.submit(batch, Signer::Key(self.verify_key.clone()), now_tai_ns) {
            Ok(IngestOutcome::Verdict(v)) => Ok(SinkOutcome::Delivered(v)),
            Ok(other) => Err(format!("unexpected non-verdict outcome: {other:?}")),
            Err(e) => Err(e.to_string()),
        }
    }
}

/// Asserts every `RejectionCounters` field except `producer_id`/`chain_head`/`accepted`/
/// `duplicate_count` is zero -- the "no other rejection counter moved at all" half of
/// deliverable (b), spelled out field by field so a failure names exactly which counter
/// moved, rather than a single opaque boolean.
fn assert_only_accepted_and_duplicate_moved(counters: &pb::RejectionCounters) {
    for (name, value) in [
        ("unsigned_count", counters.unsigned_count),
        ("bad_signature_count", counters.bad_signature_count),
        ("chain_gap_count", counters.chain_gap_count),
        ("chain_break_count", counters.chain_break_count),
        ("mislabeled_count", counters.mislabeled_count),
        ("over_clearance_count", counters.over_clearance_count),
        ("stale_count", counters.stale_count),
        ("shard_mismatch_count", counters.shard_mismatch_count),
    ] {
        assert_eq!(value, 0, "rejection counter {name} must be zero: {counters:?}");
    }
}

// ===========================================================================================
// (a) The byte-identical log test -- the centrepiece.
// ===========================================================================================

/// The interval the link stays down for in test (a) -- named, not a magic number, per
/// this milestone's own task brief. Two hours of injected TAI time; `DISCONNECT_BUDGET_NS`
/// (6 hours) is calibrated to comfortably exceed it.
const CUT_INTERVAL_NS: i64 = 2 * 3600 * 1_000_000_000;

#[test]
fn the_ingests_log_is_byte_identical_across_an_interrupted_and_uninterrupted_run() {
    let (signing_key, verify_key) = keys();

    // CRITICAL: exactly one signing pass. ECDSA P-384 uses a random nonce per signature
    // (`av_edge`'s own crate-level doc comment), so two independent calls to
    // `sign::sign_batch` over identical content produce two different, both-valid
    // signature byte strings -- and `signature` is part of what this log's payload stores
    // (`crates/av-ingest/src/log.rs`'s own "payload ... is prost::Message::encode_to_vec
    // of the accepted MeasurementBatch as a whole" doc comment), so re-signing per run
    // would make the log bytes provably unable to match, defeating the entire point of
    // this test. Building the chain once and cloning it into both runs is therefore not an
    // optimisation here, it is the property under test.
    let batches = build_chain(10, 1_000_000_000, 1_000_000_000, &signing_key); // 10 batches, 1s apart.

    // Run A: uninterrupted, straight to the ingest.
    let dir_a = tmp_dir("run-a");
    let mut ingest_a = fresh_ingest(&dir_a);
    for b in &batches {
        let outcome = ingest_a.submit(b, Signer::Key(verify_key.clone()), b.batch_tai_ns).unwrap();
        assert!(outcome.accepted(), "run A: batch (sequence {}) must be accepted: {outcome:?}", b.sequence);
    }

    // Run B: identical batches (cloned from the SAME signed Vec above), with the link cut
    // for CUT_INTERVAL_NS mid-stream, then restored, replaying the buffered batches in
    // order before anything new is sent.
    let dir_b = tmp_dir("run-b");
    let mut ingest_b = fresh_ingest(&dir_b);
    let buffer_path_b = tmp_dir("buffer-b").join("edge.buflog");
    let (edge_buffer_b, recovery) = EdgeBuffer::open(&buffer_path_b).unwrap();
    assert!(recovery.is_none(), "a fresh edge buffer must recover cleanly");
    let sink_b = ControllableIngestSink { ingest: &mut ingest_b, verify_key: verify_key.clone(), up_budget: None };
    let mut driver = UplinkDriver::new(sink_b, edge_buffer_b);

    let start_tai_ns = batches[0].batch_tai_ns;
    // Batches 1-4: link up, sent live.
    for b in &batches[0..4] {
        let outcome = driver.step(b, start_tai_ns).unwrap();
        assert!(matches!(outcome, StepOutcome::Delivered { .. }), "{outcome:?}");
    }
    // Link cut before batch 5.
    driver.sink_mut().up_budget = Some(0);
    for b in &batches[4..7] {
        let outcome = driver.step(b, start_tai_ns).unwrap();
        assert!(matches!(outcome, StepOutcome::Buffered { .. }), "batch (sequence {}) must be buffered while the link is down: {outcome:?}", b.sequence);
    }
    assert_eq!(driver.buffer().record_count(), 3, "batches 5-7 must be durably buffered");

    // Link restored after CUT_INTERVAL_NS of injected downtime -- a full, uninterrupted
    // drain: batches 5, 6 and 7 replay, in order, before batch 8 (the "new" one for this
    // step) is ever sent.
    driver.sink_mut().up_budget = None;
    let reconnect_now = start_tai_ns + CUT_INTERVAL_NS;
    let outcome = driver.step(&batches[7], reconnect_now).unwrap();
    match outcome {
        StepOutcome::Delivered { replayed, verdict } => {
            assert_eq!(replayed.len(), 3, "must have replayed exactly the 3 buffered batches before sending batch 8: {replayed:?}");
            for (i, v) in replayed.iter().enumerate() {
                assert!(v.accepted, "replayed batch {} (sequence {}) must be accepted, not rejected: {v:?}", i, batches[4 + i].sequence);
            }
            assert!(verdict.accepted, "{verdict:?}");
        }
        other => panic!("expected Delivered (post-reconnect drain + send), got {other:?}"),
    }
    // Batches 9-10: link stays up.
    for b in &batches[8..10] {
        let outcome = driver.step(b, reconnect_now).unwrap();
        assert!(matches!(outcome, StepOutcome::Delivered { .. }), "{outcome:?}");
    }

    // Every batch must have been accepted exactly once in both runs, with no duplicates
    // and no other rejection anywhere in run B.
    let counters_a = ingest_a.producer_counters(PRODUCER_ID);
    let counters_b = ingest_b.producer_counters(PRODUCER_ID);
    assert_eq!(counters_a.accepted, 10);
    assert_eq!(counters_b.accepted, 10);
    assert_eq!(counters_b.duplicate_count, 0, "test (a) has no interrupted drain, so no overlap is expected: {counters_b:?}");
    assert_only_accepted_and_duplicate_moved(&counters_a);
    assert_only_accepted_and_duplicate_moved(&counters_b);

    // The headline assertion: the two partition log FILES, read straight off disk, must
    // be byte-identical -- not merely "same count" or "same hash of the decoded
    // messages". See this test's own comment above for why one signing pass is required
    // for this to even be possible.
    let bytes_a = partition_log_bytes(&ingest_a);
    let bytes_b = partition_log_bytes(&ingest_b);
    assert_eq!(bytes_a.len(), bytes_b.len(), "log file sizes must match");
    assert_eq!(bytes_a, bytes_b, "the uninterrupted and cut-then-restored runs must produce byte-identical partition log files");

    let (_, log_a) = ingest_a.partitions().find(|(k, _)| k.as_str() == SHARD_KEY).unwrap();
    let (_, log_b) = ingest_b.partitions().find(|(k, _)| k.as_str() == SHARD_KEY).unwrap();
    assert_eq!(log_a.tip_hash(), log_b.tip_hash(), "chain heads must agree");
    assert_eq!(log_a.record_count(), log_b.record_count());
    assert_eq!(log_a.record_count(), 10);

    // Save the evidence this test's own final report cites.
    let out_dir = std::path::Path::new("/private/tmp/claude-501/-Users-probe-code-AltaVista/ed238b37-d349-42f1-956b-22807101027f/scratchpad/round3");
    let _ = std::fs::create_dir_all(out_dir);
    std::fs::write(out_dir.join("run_a.avlog"), &bytes_a).unwrap();
    std::fs::write(out_dir.join("run_b.avlog"), &bytes_b).unwrap();
}

// ===========================================================================================
// (b) The counter test.
// ===========================================================================================

#[test]
fn the_duplicate_counter_absorbs_a_replay_overlap_from_an_unpersisted_watermark() {
    let (signing_key, verify_key) = keys();
    let batches = build_chain(9, 2_000_000_000, 1_000_000_000, &signing_key); // one signing pass, 9 batches.

    // Run A (baseline): every batch straight to a fresh ingest -- what "accepted" and
    // "every other counter" must equal in run B once every batch has landed exactly once.
    let dir_a = tmp_dir("baseline");
    let mut ingest_a = fresh_ingest(&dir_a);
    for b in &batches {
        let outcome = ingest_a.submit(b, Signer::Key(verify_key.clone()), b.batch_tai_ns).unwrap();
        assert!(outcome.accepted(), "{outcome:?}");
    }
    let counters_a = ingest_a.producer_counters(PRODUCER_ID);
    assert_eq!(counters_a.accepted, 9);

    // Run B: batches 1-3 live; link cut for 4-9; a first reconnect attempt that
    // deliberately delivers exactly 2 of the backlog (sequences 4, 5 -- genuinely
    // ACCEPTED by the ingest) before the link drops again mid-drain, so the ack watermark
    // is never committed for them (av_edge::buffer's own documented mechanism); a second,
    // fully successful reconnect then replays the WHOLE backlog again from the start,
    // resubmitting 4 and 5 -- now DUPLICATE -- before finishing 6-7 fresh, and the driver
    // finally sends the new batch (8) directly; batch 9 follows live.
    let dir_b = tmp_dir("overlap");
    let mut ingest_b = fresh_ingest(&dir_b);
    let buffer_path_b = tmp_dir("overlap-buffer").join("edge.buflog");
    let (edge_buffer_b, _) = EdgeBuffer::open(&buffer_path_b).unwrap();
    let sink_b = ControllableIngestSink { ingest: &mut ingest_b, verify_key: verify_key.clone(), up_budget: None };
    let mut driver = UplinkDriver::new(sink_b, edge_buffer_b);
    let now = batches[0].batch_tai_ns;

    for b in &batches[0..3] {
        let outcome = driver.step(b, now).unwrap();
        assert!(matches!(outcome, StepOutcome::Delivered { .. }), "{outcome:?}");
    }

    driver.sink_mut().up_budget = Some(0);
    for b in &batches[3..6] {
        let outcome = driver.step(b, now).unwrap();
        assert!(matches!(outcome, StepOutcome::Buffered { .. }), "{outcome:?}");
    }
    assert_eq!(driver.buffer().record_count(), 3, "batches 4, 5, 6 buffered");
    assert_eq!(driver.buffer().ack_watermark(), 0);

    // First reconnect attempt: exactly 2 backlog deliveries succeed (4, 5 -- genuinely
    // accepted), then the link drops again before the drain reaches batch 6, so batch 7
    // (this step's own "new" batch) is buffered too and the watermark stays at 0.
    driver.sink_mut().up_budget = Some(2);
    let outcome = driver.step(&batches[6], now).unwrap();
    match outcome {
        StepOutcome::Buffered { replayed } => assert_eq!(replayed.iter().filter(|v| v.accepted).count(), 2, "exactly 2 backlog batches must have been genuinely accepted before the interruption: {replayed:?}"),
        other => panic!("expected an interrupted drain (Buffered), got {other:?}"),
    }
    assert_eq!(driver.buffer().ack_watermark(), 0, "an interrupted drain must not persist the watermark");
    assert_eq!(driver.buffer().record_count(), 4, "batches 4,5,6,7 are all still in the buffer");

    // Second reconnect: fully up. The drain replays 4,5,6,7 -- 4 and 5 come back
    // DUPLICATE (already accepted above), 6 and 7 accept fresh -- then batch 8 is sent
    // directly, and this drain, being uninterrupted, finally commits the watermark.
    driver.sink_mut().up_budget = None;
    let outcome = driver.step(&batches[7], now).unwrap();
    let replayed = match outcome {
        StepOutcome::Delivered { replayed, verdict } => {
            assert!(verdict.accepted, "{verdict:?}");
            replayed
        }
        other => panic!("expected Delivered, got {other:?}"),
    };
    assert_eq!(replayed.len(), 4, "must have replayed all 4 backlog batches (4,5,6,7): {replayed:?}");
    assert_eq!(replayed[0].rejection, pb::BatchRejection::Duplicate as i32, "sequence 4's replay must come back DUPLICATE: {:?}", replayed[0]);
    assert_eq!(replayed[1].rejection, pb::BatchRejection::Duplicate as i32, "sequence 5's replay must come back DUPLICATE: {:?}", replayed[1]);
    assert!(replayed[2].accepted, "sequence 6 must accept fresh: {:?}", replayed[2]);
    assert!(replayed[3].accepted, "sequence 7 must accept fresh: {:?}", replayed[3]);
    assert_eq!(driver.buffer().ack_watermark(), 7, "the uninterrupted second drain must commit the watermark to the last backlog sequence");

    // Batch 9: link stays up.
    let outcome = driver.step(&batches[8], now).unwrap();
    assert!(matches!(outcome, StepOutcome::Delivered { .. }), "{outcome:?}");

    // The pinned numbers deliverable (b) asks for.
    let counters_b = ingest_b.producer_counters(PRODUCER_ID);
    assert_eq!(counters_b.accepted, 9, "accepted must equal the number of DISTINCT batches -- identical to run A's accepted count: {counters_b:?}");
    assert_eq!(counters_b.duplicate_count, 2, "duplicate_count must equal exactly the number of overlapping batches (sequences 4 and 5): {counters_b:?}");
    assert_only_accepted_and_duplicate_moved(&counters_b);
    assert_eq!(counters_a.accepted, counters_b.accepted, "run A and run B must agree on the number of distinct accepted batches");

    // Save the evidence this test's own final report cites.
    let out_dir = std::path::Path::new("/private/tmp/claude-501/-Users-probe-code-AltaVista/ed238b37-d349-42f1-956b-22807101027f/scratchpad/round3");
    let _ = std::fs::create_dir_all(out_dir);
    std::fs::write(out_dir.join("test_b_counters.txt"), format!("run A (baseline): {counters_a:?}\nrun B (overlap):  {counters_b:?}\n")).unwrap();
}

// ===========================================================================================
// (e) "For hours", and the STALE interaction this milestone's own task brief calls out.
// ===========================================================================================

/// How long the link stays down in the "for hours" tests below -- three hours of
/// injected TAI time, matching E6's own charter wording verbatim ("for hours").
const HOURS_DOWN_NS: i64 = 3 * 3600 * 1_000_000_000;

/// **Root-cause demonstration, not a defect left unfixed.** Proves the STALE interaction
/// this milestone's own task brief calls out is real and reachable: a producer policy
/// with a deliberately SHORT `max_age_ns` (60 seconds -- plausible for a live-only
/// deployment that never expected a multi-hour disconnection) rejects a batch replayed
/// after `HOURS_DOWN_NS` of injected downtime as STALE, even though the batch itself was
/// perfectly valid and was genuinely captured and signed while disconnected. The fix
/// (`capture_only_policy`'s own `DISCONNECT_BUDGET_NS`) is exercised by
/// `buffering_survives_a_multi_hour_disconnection_with_no_loss` below, not here -- this
/// test exists so the defect this milestone's brief warns about is provably real and
/// understood, not quietly avoided.
#[test]
fn a_short_lived_policys_max_age_ns_rejects_a_replay_after_hours_as_stale() {
    let (signing_key, verify_key) = keys();
    let short_lived_producer = "e6-short-lived-policy-producer";
    let short_policy = ProducerPolicy::new(short_lived_producer, "CUI", vec![], vec!["UNCLASSIFIED".to_string(), "CUI".to_string()], "CUI", 60_000_000_000 /* 60s -- far too short for HOURS_DOWN_NS */).unwrap();

    let start_tai_ns = 5_000_000_000;
    let mut b = pb::MeasurementBatch { producer_id: short_lived_producer.to_string(), sequence: 1, label: Some(pb::Label { marking: "CUI".to_string(), caveats: vec![] }), batch_tai_ns: start_tai_ns, shard_key: "short-lived-shard".to_string(), ..Default::default() };
    sign::sign_batch(&mut b, hash::GENESIS, &signing_key).unwrap();

    let dir = tmp_dir("stale-demo");
    let mut ingest = Ingest::new(&dir, None);
    ingest.register_producer(short_policy);

    // Buffer it (simulating capture-while-disconnected), then attempt delivery after
    // HOURS_DOWN_NS of injected downtime -- exactly what a real reconnect's drain does.
    let buffer_path = tmp_dir("stale-demo-buffer").join("edge.buflog");
    let (edge_buffer, _) = EdgeBuffer::open(&buffer_path).unwrap();
    edge_buffer.append(&b).unwrap();

    let reconnect_now = start_tai_ns + HOURS_DOWN_NS;
    let backlog = edge_buffer.replay_from(0).unwrap();
    assert_eq!(backlog.len(), 1);
    let outcome = ingest.submit(&backlog[0], Signer::Key(verify_key), reconnect_now).unwrap();
    match outcome {
        IngestOutcome::Verdict(v) => {
            assert!(!v.accepted, "a too-short max_age_ns must reject this legitimate-but-old replay: {v:?}");
            assert_eq!(v.rejection, pb::BatchRejection::Stale as i32, "the rejection must be STALE specifically: {v:?}");
        }
        other => panic!("expected a Verdict, got {other:?}"),
    }
    let counters = ingest.producer_counters(short_lived_producer);
    assert_eq!(counters.stale_count, 1, "{counters:?}");
    assert_eq!(counters.accepted, 0, "{counters:?}");
}

#[test]
fn buffering_survives_a_multi_hour_disconnection_with_no_loss() {
    let (signing_key, verify_key) = keys();
    // 6 batches, spread across the whole down window (30 minutes apart) -- so the oldest
    // buffered batch is nearly HOURS_DOWN_NS old by the time it is replayed, and the
    // newest is nearly fresh; DISCONNECT_BUDGET_NS (6h) covers even the oldest one.
    let step_ns = HOURS_DOWN_NS / 6;
    let batches = build_chain(6, 10_000_000_000, step_ns, &signing_key);

    let dir = tmp_dir("hours-survive");
    let mut ingest = fresh_ingest(&dir);
    let buffer_path = tmp_dir("hours-survive-buffer").join("edge.buflog");
    let (edge_buffer, _) = EdgeBuffer::open(&buffer_path).unwrap();
    let sink = ControllableIngestSink { ingest: &mut ingest, verify_key: verify_key.clone(), up_budget: Some(0) };
    let mut driver = UplinkDriver::new(sink, edge_buffer);

    let down_start = batches[0].batch_tai_ns;
    for b in &batches[0..5] {
        let outcome = driver.step(b, down_start).unwrap();
        assert!(matches!(outcome, StepOutcome::Buffered { .. }), "batch (sequence {}) must be buffered: {outcome:?}", b.sequence);
    }
    assert_eq!(driver.buffer().record_count(), 5, "5 batches durably buffered throughout the down window");

    // Reconnect at down_start + HOURS_DOWN_NS: drain the backlog, then send the 6th
    // (final) batch directly -- both must succeed, with the deployment's own
    // capture-only policy budget (DISCONNECT_BUDGET_NS) in effect.
    driver.sink_mut().up_budget = None;
    let reconnect_now = down_start + HOURS_DOWN_NS;
    let outcome = driver.step(&batches[5], reconnect_now).unwrap();
    match outcome {
        StepOutcome::Delivered { replayed, verdict } => {
            assert_eq!(replayed.len(), 5, "all 5 buffered batches must replay: {replayed:?}");
            for (i, v) in replayed.iter().enumerate() {
                assert!(v.accepted, "buffered batch {i} (sequence {}) must be accepted, not lost or rejected as stale: {v:?}", batches[i].sequence);
            }
            assert!(verdict.accepted, "{verdict:?}");
        }
        other => panic!("expected Delivered, got {other:?}"),
    }

    assert_eq!(driver.buffer().ack_watermark(), 5, "the uninterrupted drain must commit the watermark");
    drop(driver); // release the sink's mutable borrow of `ingest` before reading its counters.

    let counters = ingest.producer_counters(PRODUCER_ID);
    assert_eq!(counters.accepted, 6, "nothing must be lost across the multi-hour disconnection: {counters:?}");
    assert_eq!(counters.stale_count, 0, "the deployment's own DISCONNECT_BUDGET_NS must cover this window: {counters:?}");
    assert_only_accepted_and_duplicate_moved(&counters);
    assert_eq!(counters.duplicate_count, 0);
}
