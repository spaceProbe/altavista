//! The power-cycle record's ordering and failure contract, in process (as `io_log_inprocess.rs`
//! is for the other exchanges): the `POWER_CYCLE` record is written and synced **before** the
//! reply returns; when the sync fails the kernel gets `DATA_LOSS` and not the response, and a
//! failed log refuses the next power cycle with `FAILED_PRECONDITION` before the channel runs.
//! **No board is involved.**
mod common;

use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use av_cdm::pb::{BoardPowerCycleOutcome, PowerCycleRequest};
use av_edge::board::parse_power_control;
use av_edge::board_log::{verify_bytes, BoardIoKind, BoardIoLogWriter, DurableSink, LogSigner, LogVerifier};
use av_edge_board::iolog::BoardIoLog;
use av_edge_board::power::{BoardEdge, BoardEdgeService};
use av_edge_board::timed::LinkTimes;
use common::FakeChannel;
use tonic::{Code, Request};

struct Sink {
    events: Arc<Mutex<Vec<String>>>,
    bytes: Arc<Mutex<Vec<u8>>>,
    fail_sync_at: Option<usize>,
    syncs: usize,
}

impl DurableSink for Sink {
    fn write_all(&mut self, b: &[u8]) -> io::Result<()> {
        self.bytes.lock().unwrap().extend_from_slice(b);
        self.events.lock().unwrap().push("log_write".into());
        Ok(())
    }
    fn sync(&mut self) -> io::Result<()> {
        let n = self.syncs;
        self.syncs += 1;
        if self.fail_sync_at == Some(n) {
            self.events.lock().unwrap().push("log_sync_failed".into());
            return Err(io::Error::other("injected fsync failure"));
        }
        self.events.lock().unwrap().push("log_sync_end".into());
        Ok(())
    }
}

struct Rig {
    edge: BoardEdge,
    events: Arc<Mutex<Vec<String>>>,
    bytes: Arc<Mutex<Vec<u8>>>,
    verifier: LogVerifier,
}

fn rig(tag: &str, channel: &FakeChannel, fail_sync_at: Option<usize>) -> Rig {
    let dir = std::env::temp_dir().join(format!("av-edge-board-power-inproc-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let id = common::make_identity(&dir, "inproc");
    let signer = LogSigner::from_pem(&std::fs::read(&id.key_pem).unwrap(), &std::fs::read(&id.cert_pem).unwrap()).unwrap();
    let verifier = LogVerifier::from_pem(&std::fs::read(&id.cert_pem).unwrap()).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let events: Arc<Mutex<Vec<String>>> = Arc::default();
    let bytes: Arc<Mutex<Vec<u8>>> = Arc::default();
    let sink = Sink { events: events.clone(), bytes: bytes.clone(), fail_sync_at, syncs: 0 };
    let writer = BoardIoLogWriter::with_sink(Path::new("memory"), Box::new(sink), "edge-inproc", "00", signer);
    let log = Arc::new(BoardIoLog::new(writer, Arc::new(LinkTimes::default())));
    let edge = BoardEdge::new("edge-inproc".into(), parse_power_control(&channel.uri()).unwrap(), Duration::from_secs(10), log);
    Rig { edge, events, bytes, verifier }
}

fn req(channel: &FakeChannel) -> Request<PowerCycleRequest> {
    Request::new(PowerCycleRequest { run_id: "run-x".into(), instance: "obc".into(), fault_id: "pc1".into(), tai_ns: 77, edge_node_id: "edge-inproc".into(), power_control: channel.uri() })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_power_cycle_record_is_durable_before_the_reply_returns() {
    let channel = FakeChannel::create("inproc-order");
    let Rig { edge, events, bytes, verifier } = rig("order", &channel, None);
    let resp = edge.power_cycle(req(&channel)).await.unwrap().into_inner();
    events.lock().unwrap().push("returned".into());
    assert_eq!(resp.outcome, BoardPowerCycleOutcome::Performed as i32);
    assert_eq!(*events.lock().unwrap(), ["log_write", "log_sync_end", "returned"]);
    let log = verify_bytes(&bytes.lock().unwrap(), &verifier).unwrap();
    assert_eq!(log.records.len(), 1);
    let r = &log.records[0];
    assert_eq!((BoardIoKind::try_from(r.kind).unwrap(), r.run_id.as_str(), r.instance.as_str(), r.reset_tai_ns, r.reset_reason.as_str()), (BoardIoKind::PowerCycle, "run-x", "obc", 77, "fault:pc1"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_sync_is_data_loss_and_a_failed_log_refuses_the_next_power_cycle_before_the_channel_runs() {
    let channel = FakeChannel::create("inproc-fail");
    let Rig { edge, .. } = rig("fail", &channel, Some(0));
    let err = edge.power_cycle(req(&channel)).await.expect_err("the reply must not be returned when its record is not durable");
    assert_eq!(err.code(), Code::DataLoss, "{err}");
    assert!(err.message().contains("injected fsync failure"), "{}", err.message());
    assert_eq!(channel.calls().len(), 1, "the channel did run once (the exchange happened; it just is not logged)");
    let err = edge.power_cycle(req(&channel)).await.expect_err("the poisoned log refuses");
    assert_eq!(err.code(), Code::FailedPrecondition, "{err}");
    assert_eq!(channel.calls().len(), 1, "the second power cycle never ran");
}
