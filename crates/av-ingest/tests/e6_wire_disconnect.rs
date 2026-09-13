//! Milestone E6 (`docs/edge-plan.md`), deliverable (c): "the real wire". Drives the cut
//! over the actual gRPC loopback wire (`av-ingest`'s server + `av-ingest-client`), not
//! only the in-process `Ingest` that `tests/e6_disconnect.rs` exercises. The link is cut
//! by genuinely dropping the client side -- [`GrpcBatchSink::disconnect`] tears down its
//! background thread's own tokio `Runtime` and `EdgeIngestClient` (and, with them, the
//! real TCP connection), while the ingest process (here, an in-process server task) stays
//! up the whole time -- then a fresh [`GrpcBatchSink::connect`] dials a brand new client,
//! per this milestone's own instructions.
//!
//! [`av_edge::buffer::BatchSink`]'s `submit` is synchronous, but `av-ingest-client`'s API
//! is async; [`GrpcBatchSink`] bridges the two with a dedicated background thread that
//! owns its own `tokio::runtime::Runtime`, so `av-edge` itself never needs to know
//! anything about tokio (that crate's own long-standing rule) and `UplinkDriver`'s
//! control flow is exercised, unmodified, against a real network client -- proving the
//! same abstraction genuinely drives both an in-process `Ingest`
//! (`tests/e6_disconnect.rs`) and this real wire.
//!
//! Mirrors this crate's own `tests/plugin_wire.rs` and `tests/determinism.rs` precedents
//! (`start_server`'s shape; "sign once, feed both runs" for the byte-identical property).
//! No Docker, no network beyond this test's own ephemeral loopback socket (question 154).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::mpsc as std_mpsc;
use std::sync::Arc;
use std::thread::JoinHandle;

use av_edge::buffer::{BatchSink, EdgeBuffer, SinkOutcome, StepOutcome, UplinkDriver};
use av_edge::{hash, pb, sign};
use av_ingest::pb::edge_ingest_server::EdgeIngestServer;
use av_ingest::service::{EdgeIngestConfig, EdgeIngestService};
use av_ingest_client::EdgeIngestClient;
use openssl::ec::EcKey;
use openssl::pkey::{Private, Public};
use tonic::transport::Server;

const TEST_KEY_PEM: &[u8] = include_bytes!("fixtures/test_signing_key.pem");
const TEST_PUB_PEM: &[u8] = include_bytes!("fixtures/test_signing_key.pub.pem");
const PRODUCER_ID: &str = "e6-wire-producer";
const SHARD_KEY: &str = "e6-wire-shard";
/// This server's own injected clock (question 199: never a live read) -- constant, since
/// this file tests the disconnect/replay/dedup mechanism over the real wire, not the
/// STALE interaction (`tests/e6_disconnect.rs` already covers that in depth). Every
/// batch's own `batch_tai_ns` below is set to this same value, so staleness is a non-issue
/// throughout.
const NOW: i64 = 1_000_000_000;
const MAX_AGE_NS: i64 = 10_000_000_000_000;

fn keys() -> (EcKey<Private>, EcKey<Public>) {
    (sign::load_signing_key(TEST_KEY_PEM).unwrap(), av_edge::verify::load_verifying_key(TEST_PUB_PEM).unwrap())
}

fn tmp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("av-ingest-test-e6-wire-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// A single producer's hand-built, hand-signed chain of `count` batches, all sharing
/// `batch_tai_ns = NOW` -- signed **exactly once** per test, then cloned into whichever
/// run(s) need them (ECDSA's random nonce means re-signing would make a byte-identical
/// comparison provably impossible; see `tests/e6_disconnect.rs`'s identical reasoning).
fn build_chain(count: u64, key: &EcKey<Private>) -> Vec<pb::MeasurementBatch> {
    let mut batches = Vec::with_capacity(count as usize);
    let mut prev_hash = hash::GENESIS.to_vec();
    for i in 0..count {
        let mut b = pb::MeasurementBatch {
            producer_id: PRODUCER_ID.to_string(),
            sequence: i + 1,
            label: Some(pb::Label { marking: "CUI".to_string(), caveats: vec![] }),
            batch_tai_ns: NOW,
            shard_key: SHARD_KEY.to_string(),
            ..Default::default()
        };
        sign::sign_batch(&mut b, &prev_hash, key).unwrap();
        prev_hash = b.batch_hash.clone();
        batches.push(b);
    }
    batches
}

fn manifest() -> pb::PluginManifest {
    pb::PluginManifest {
        producer_id: PRODUCER_ID.to_string(),
        plugin_version: "0.0.0-e6-test".to_string(),
        output_schemas: vec![],
        frame_ids: vec![],
        label: Some(pb::Label { marking: "CUI".to_string(), caveats: vec![] }),
        clearance: "CUI".to_string(),
        shard_keys: vec![SHARD_KEY.to_string()],
        leaf_fingerprint_sha256: String::new(),
    }
}

async fn poll_until_ready(addr: SocketAddr) {
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
    loop {
        if tokio::net::TcpStream::connect(addr).await.is_ok() {
            return;
        }
        if tokio::time::Instant::now() > deadline {
            panic!("nothing at {addr} became ready within the deadline");
        }
        // A short poll interval while waiting for a just-spawned server task to bind its
        // listener -- the same pattern `tests/plugin_wire.rs::poll_until_ready` already
        // uses (not a substitute for an injected clock: this is about a background tokio
        // task's own startup race, not this test's own E6 logic, which never sleeps).
        tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
    }
}

/// Starts a real `EdgeIngestService` on an ephemeral loopback port, no certificate in the
/// loop (mirrors `tests/plugin_wire.rs::start_server`'s identical no-cert path -- E2
/// identity issuance is out of this milestone's own scope).
async fn start_server(dir: &std::path::Path, verify_key: EcKey<Public>) -> (SocketAddr, Arc<EdgeIngestService>) {
    let mut cfg = EdgeIngestConfig::new(vec!["UNCLASSIFIED".to_string(), "CUI".to_string()], MAX_AGE_NS);
    cfg.require_client_certificate = false;
    cfg.verify_keys.insert(PRODUCER_ID.to_string(), verify_key);
    let service = Arc::new(EdgeIngestService::new(dir, None, cfg, Arc::new(|| NOW)));

    let listener = av_ingest::server::bind_loopback("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
    let served = service.clone();
    tokio::spawn(async move {
        let _ = Server::builder().add_service(EdgeIngestServer::from_arc(served)).serve_with_incoming(incoming).await;
    });
    poll_until_ready(addr).await;
    (addr, service)
}

fn partition_log_bytes(service: &EdgeIngestService) -> Vec<u8> {
    let ingest_handle = service.ingest_handle();
    let ingest = ingest_handle.lock().unwrap();
    let (_, log) = ingest.partitions().find(|(k, _)| k.as_str() == SHARD_KEY).expect("the shard partition must have been opened");
    std::fs::read(log.path()).unwrap()
}

// -------------------------------------------------------------------------------------------
// GrpcBatchSink: bridges av_edge::buffer::BatchSink's synchronous submit to the real,
// async av-ingest-client wire, via a dedicated background thread + its own tokio Runtime.
// -------------------------------------------------------------------------------------------

enum Cmd {
    Submit { batch: pb::MeasurementBatch, reply: std_mpsc::Sender<Result<pb::BatchVerdict, String>> },
}

struct Connection {
    cmd_tx: std_mpsc::Sender<Cmd>,
    worker: JoinHandle<()>,
}

/// A [`BatchSink`] over the real gRPC wire. `connect`/`disconnect` genuinely establish and
/// tear down the underlying TCP connection (see this file's own module doc) -- calling
/// `submit` while disconnected returns [`SinkOutcome::LinkDown`] without ever touching the
/// network, exactly the condition [`UplinkDriver::step`] treats as "buffer this batch".
struct GrpcBatchSink {
    addr: SocketAddr,
    manifest: pb::PluginManifest,
    conn: Option<Connection>,
}

impl GrpcBatchSink {
    fn new(addr: SocketAddr, manifest: pb::PluginManifest) -> Self {
        let mut sink = Self { addr, manifest, conn: None };
        sink.connect();
        sink
    }

    /// Dials a brand-new `EdgeIngestClient`, announces this sink's manifest on it, and
    /// spawns the background thread that will serve every subsequent `submit` call until
    /// [`GrpcBatchSink::disconnect`] tears it down. Panics on failure -- every call site in
    /// this file expects `connect`/`reconnect` to succeed (the server is up and this
    /// manifest was already accepted once before any of these tests cut the link).
    fn connect(&mut self) {
        let (cmd_tx, cmd_rx) = std_mpsc::channel::<Cmd>();
        let addr = self.addr;
        let manifest = self.manifest.clone();
        let (ready_tx, ready_rx) = std_mpsc::channel::<Result<(), String>>();
        let worker = std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().expect("building this sink's own dedicated tokio runtime");
            rt.block_on(async move {
                let mut client = match EdgeIngestClient::connect_plaintext_addr(addr).await {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = ready_tx.send(Err(e.to_string()));
                        return;
                    }
                };
                match client.announce(manifest).await {
                    Ok(ack) if ack.accepted => {
                        let _ = ready_tx.send(Ok(()));
                    }
                    Ok(ack) => {
                        let _ = ready_tx.send(Err(format!("announce refused: {ack:?}")));
                        return;
                    }
                    Err(status) => {
                        let _ = ready_tx.send(Err(status.to_string()));
                        return;
                    }
                }
                while let Ok(cmd) = cmd_rx.recv() {
                    match cmd {
                        Cmd::Submit { batch, reply } => {
                            let outcome = match client.submit_batches(vec![batch]).await {
                                Ok(mut verdicts) if verdicts.len() == 1 => Ok(verdicts.remove(0)),
                                Ok(verdicts) => Err(format!("expected exactly 1 verdict for 1 submitted batch, got {}", verdicts.len())),
                                Err(status) => Err(status.to_string()),
                            };
                            let _ = reply.send(outcome);
                        }
                    }
                }
                // cmd_rx.recv() returned Err (the sender was dropped by `disconnect`):
                // `client` (and, inside it, the tonic Channel/TCP connection) is dropped
                // right here, when this async block ends and `rt` itself is dropped next.
            });
        });
        ready_rx.recv().expect("the worker thread must report readiness or a connect/announce failure").expect("connect + announce must succeed in this test");
        self.conn = Some(Connection { cmd_tx, worker });
    }

    /// Genuinely closes this sink's connection: drops the command channel (ending the
    /// worker thread's `recv` loop), then joins that thread -- by the time this returns,
    /// the real TCP socket is closed and the background tokio `Runtime` is gone.
    fn disconnect(&mut self) {
        if let Some(conn) = self.conn.take() {
            drop(conn.cmd_tx);
            let _ = conn.worker.join();
        }
    }
}

impl BatchSink for GrpcBatchSink {
    type Verdict = pb::BatchVerdict;
    type Error = String;

    fn submit(&mut self, batch: &pb::MeasurementBatch, _now_tai_ns: i64) -> Result<SinkOutcome<pb::BatchVerdict>, String> {
        // `_now_tai_ns` is unused: over the real wire, staleness is checked against the
        // *server's own* injected clock (`start_server`'s `Arc::new(|| NOW)`), not
        // anything this client-side sink could supply per call -- there is no RPC field
        // for it (`edge.proto`'s `MeasurementBatch`/`Submit` carry no client-declared
        // "now"), which is the correct trust boundary: a producer does not get to assert
        // its own idea of the current time to the ingest.
        let Some(conn) = &self.conn else {
            return Ok(SinkOutcome::LinkDown);
        };
        let (reply_tx, reply_rx) = std_mpsc::channel();
        if conn.cmd_tx.send(Cmd::Submit { batch: batch.clone(), reply: reply_tx }).is_err() {
            return Ok(SinkOutcome::LinkDown);
        }
        match reply_rx.recv() {
            Ok(Ok(verdict)) => Ok(SinkOutcome::Delivered(verdict)),
            Ok(Err(e)) => Err(e),
            Err(_) => Ok(SinkOutcome::LinkDown),
        }
    }
}

impl Drop for GrpcBatchSink {
    fn drop(&mut self) {
        self.disconnect();
    }
}

/// Runs `batches` straight through a fresh server with no interruption at all -- this
/// file's own uninterrupted baseline, mirroring `tests/e6_disconnect.rs`'s "run A".
async fn baseline_uninterrupted_wire_run(batches: &[pb::MeasurementBatch], verify_key: EcKey<Public>, dir_name: &str) -> Vec<u8> {
    let dir = tmp_dir(dir_name);
    let (addr, service) = start_server(&dir, verify_key).await;
    let mut sink = GrpcBatchSink::new(addr, manifest());
    for b in batches {
        match sink.submit(b, NOW).unwrap() {
            SinkOutcome::Delivered(v) => assert!(v.accepted, "{v:?}"),
            SinkOutcome::LinkDown => panic!("uninterrupted baseline must never see LinkDown"),
        }
    }
    partition_log_bytes(&service)
}

// `flavor = "multi_thread"`, not the default current-thread runtime: `GrpcBatchSink`
// bridges its synchronous `BatchSink::submit`/`connect`/`disconnect` to the real async
// gRPC client via a *separate* background OS thread that blocks on a plain
// `std::sync::mpsc::Receiver::recv()` (see this file's own `GrpcBatchSink` doc). On a
// current-thread runtime that blocking call would park the test's own single worker
// thread -- the exact thread this test's in-process server task is *also* scheduled on
// (`tokio::spawn` inside `start_server`) -- so the server could never be polled again to
// accept the very connection this test is waiting on: a genuine deadlock, caught by
// hand while developing this test (a `sample` of the hung process showed the main test
// thread parked in `GrpcBatchSink::connect`'s `ready_rx.recv()` while the server's own
// task sat unpolled). `multi_thread` gives the runtime more than one OS thread, so the
// blocked thread and the server's own task are never forced to compete for the same one.
#[tokio::test(flavor = "multi_thread")]
async fn the_real_wire_survives_a_client_side_disconnect_and_replays_byte_identically() {
    let (signing_key, verify_key) = keys();
    // Exactly one signing pass, shared by both the baseline and the cut-then-reconnect
    // run below (see this file's own module doc / build_chain's doc for why).
    let batches = build_chain(6, &signing_key);

    let baseline_bytes = baseline_uninterrupted_wire_run(&batches, verify_key.clone(), "baseline").await;

    // The cut-then-reconnect run, over its own fresh server instance.
    let dir = tmp_dir("cut-reconnect");
    let (addr, service) = start_server(&dir, verify_key).await;
    let buffer_path = tmp_dir("cut-reconnect-buffer").join("edge.buflog");
    let (edge_buffer, recovery) = EdgeBuffer::open(&buffer_path).unwrap();
    assert!(recovery.is_none());

    let sink = GrpcBatchSink::new(addr, manifest());
    let mut driver = UplinkDriver::new(sink, edge_buffer);

    // Batches 1-2: link up, sent over the real wire.
    for b in &batches[0..2] {
        let outcome = driver.step(b, NOW).unwrap();
        assert!(matches!(outcome, StepOutcome::Delivered { .. }), "{outcome:?}");
    }

    // Cut the link: genuinely drop the client side. The server process (this test's own
    // in-process server task) stays up throughout -- exactly "the link cut ... then
    // restored" this milestone's own test list asks for.
    driver.sink_mut().disconnect();
    for b in &batches[2..4] {
        let outcome = driver.step(b, NOW).unwrap();
        assert!(matches!(outcome, StepOutcome::Buffered { .. }), "batch (sequence {}) must be buffered while the wire is down: {outcome:?}", b.sequence);
    }
    assert_eq!(driver.buffer().record_count(), 2, "batches 3 and 4 must be durably buffered");

    // Reconnect a FRESH client (a new TCP connection, a new Announce) and replay.
    driver.sink_mut().connect();
    let outcome = driver.step(&batches[4], NOW).unwrap();
    match outcome {
        StepOutcome::Delivered { replayed, verdict } => {
            assert_eq!(replayed.len(), 2, "must replay both buffered batches over the fresh connection before sending batch 5: {replayed:?}");
            for v in &replayed {
                assert!(v.accepted, "{v:?}");
            }
            assert!(verdict.accepted, "{verdict:?}");
        }
        other => panic!("expected Delivered, got {other:?}"),
    }
    let outcome = driver.step(&batches[5], NOW).unwrap();
    assert!(matches!(outcome, StepOutcome::Delivered { .. }), "{outcome:?}");

    let cut_bytes = partition_log_bytes(&service);

    assert_eq!(baseline_bytes.len(), cut_bytes.len(), "log sizes must match");
    assert_eq!(baseline_bytes, cut_bytes, "the real-wire baseline and the client-disconnect-then-reconnect run must produce byte-identical partition log files");

    let out_dir = std::path::Path::new("/private/tmp/claude-501/-Users-probe-code-AltaVista/ed238b37-d349-42f1-956b-22807101027f/scratchpad/round3");
    let _ = std::fs::create_dir_all(out_dir);
    std::fs::write(out_dir.join("wire_baseline.avlog"), &baseline_bytes).unwrap();
    std::fs::write(out_dir.join("wire_cut_reconnect.avlog"), &cut_bytes).unwrap();
}
