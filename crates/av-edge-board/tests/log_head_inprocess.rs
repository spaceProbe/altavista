//! `BoardEdgeService.BoardIoLogHead` (hilprep-2b), in process: the answer is exactly the end of
//! the verified log (record count, chain head, signer, link, producer), it is read only, it is
//! refused for another run or instance or before any Bind named a run, and a failed log refuses
//! it. **No board is involved.**
mod common;

use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use av_cdm::pb::BoardIoLogHeadRequest;
use av_edge::board::parse_power_control;
use av_edge::board_log::{verify_bytes, BoardIoKind, BoardIoLogWriter, BoardIoRecord, DurableSink, LogSigner, LogVerifier};
use av_edge_board::iolog::BoardIoLog;
use av_edge_board::power::{BoardEdge, BoardEdgeService};
use av_edge_board::timed::LinkTimes;
use tonic::{Code, Request};

#[derive(Clone, Default)]
struct Sink {
    bytes: Arc<Mutex<Vec<u8>>>,
    fail: Arc<Mutex<bool>>,
}

impl DurableSink for Sink {
    fn write_all(&mut self, b: &[u8]) -> io::Result<()> {
        self.bytes.lock().unwrap().extend_from_slice(b);
        Ok(())
    }
    fn sync(&mut self) -> io::Result<()> {
        if *self.fail.lock().unwrap() {
            return Err(io::Error::other("injected fsync failure"));
        }
        Ok(())
    }
}

struct Rig {
    edge: BoardEdge,
    log: Arc<BoardIoLog>,
    sink: Sink,
    verifier: LogVerifier,
}

fn rig(tag: &str) -> Rig {
    let dir = std::env::temp_dir().join(format!("av-edge-board-head-inproc-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let id = common::make_identity(&dir, "inproc");
    let signer = LogSigner::from_pem(&std::fs::read(&id.key_pem).unwrap(), &std::fs::read(&id.cert_pem).unwrap()).unwrap();
    let verifier = LogVerifier::from_pem(&std::fs::read(&id.cert_pem).unwrap()).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let sink = Sink::default();
    let writer = BoardIoLogWriter::with_sink(Path::new("memory"), Box::new(sink.clone()), "edge-inproc", "ab12", signer);
    let log = Arc::new(BoardIoLog::new(writer, Arc::new(LinkTimes::default())));
    let edge = BoardEdge::new("edge-inproc".into(), parse_power_control("").unwrap(), Duration::from_secs(10), log.clone());
    Rig { edge, log, sink, verifier }
}

async fn append(log: &BoardIoLog, kind: BoardIoKind, sequence: u64) {
    let exchange = log.begin().await.unwrap();
    log.record(&exchange, BoardIoRecord { kind: kind as i32, lockstep_sequence: sequence, until_tai_ns: sequence as i64 * 100, ..Default::default() }).await.unwrap();
}

fn head_req(run: &str, instance: &str) -> Request<BoardIoLogHeadRequest> {
    Request::new(BoardIoLogHeadRequest { run_id: run.into(), instance: instance.into() })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_head_is_the_end_of_the_verified_log_and_reading_it_writes_nothing() {
    let Rig { edge, log, sink, verifier } = rig("end");
    log.set_run("run-x", "obc");
    for (i, kind) in [BoardIoKind::Bind, BoardIoKind::Step, BoardIoKind::Step].into_iter().enumerate() {
        append(&log, kind, i as u64).await;
    }
    let before = sink.bytes.lock().unwrap().clone();

    let head = edge.board_io_log_head(head_req("run-x", "obc")).await.unwrap().into_inner();
    let verified = verify_bytes(&before, &verifier).unwrap();
    assert_eq!(head.records, 3);
    assert_eq!(head.chain_head, verified.records[2].record_hash, "the head is the last record's own hash");
    assert_eq!(head.chain_head.len(), 32);
    assert_eq!(head.signer_cert_sha256, verifier.cert_sha256().unwrap());
    assert_eq!((head.link_config_sha256.as_str(), head.producer_id.as_str()), ("ab12", "edge-inproc"));
    assert_eq!(*sink.bytes.lock().unwrap(), before, "BoardIoLogHead is read only: it appended nothing");

    // It moves with the log: one more record, one more in the answer, and the old head is the new
    // record's prev_hash.
    append(&log, BoardIoKind::Shutdown, 3).await;
    let later = edge.board_io_log_head(head_req("run-x", "obc")).await.unwrap().into_inner();
    let verified = verify_bytes(&sink.bytes.lock().unwrap(), &verifier).unwrap();
    assert_eq!(later.records, 4);
    assert_eq!(later.chain_head, verified.records[3].record_hash);
    assert_eq!(verified.records[3].prev_hash, head.chain_head);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_head_is_refused_before_a_bind_and_for_another_run_or_instance() {
    let Rig { edge, log, .. } = rig("scope");
    let err = edge.board_io_log_head(head_req("run-x", "obc")).await.expect_err("no Bind has named a run");
    assert_eq!(err.code(), Code::FailedPrecondition, "{err}");
    assert!(err.message().contains("no Bind"), "{}", err.message());

    log.set_run("run-x", "obc");
    append(&log, BoardIoKind::Bind, 0).await;
    for (run, instance) in [("run-y", "obc"), ("run-x", "other"), ("", "")] {
        let err = edge.board_io_log_head(head_req(run, instance)).await.expect_err("another run's log is not this run's");
        assert_eq!(err.code(), Code::FailedPrecondition, "{run:?}/{instance:?}: {err}");
        assert!(err.message().contains("run-x") && err.message().contains("obc"), "names what the log belongs to: {}", err.message());
    }
    assert_eq!(edge.board_io_log_head(head_req("run-x", "obc")).await.unwrap().into_inner().records, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_log_refuses_the_head() {
    let Rig { edge, log, sink, .. } = rig("failed");
    log.set_run("run-x", "obc");
    append(&log, BoardIoKind::Bind, 0).await;
    *sink.fail.lock().unwrap() = true;
    let exchange = log.begin().await.unwrap();
    let err = log.record(&exchange, BoardIoRecord { kind: BoardIoKind::Step as i32, ..Default::default() }).await.expect_err("the sync fails");
    assert_eq!(err.code(), Code::DataLoss);
    drop(exchange);
    let err = edge.board_io_log_head(head_req("run-x", "obc")).await.expect_err("a poisoned log has no trustworthy head to pin");
    assert_eq!(err.code(), Code::FailedPrecondition, "{err}");
    assert!(err.message().contains("has failed"), "{}", err.message());
}
