//! The ordering and failure contract of the board I/O log, in process: `BoardService` over an
//! in-memory duplex stream to a fake guest task, the log writing to a sink that records its
//! write and sync events (and can delay or fail the sync). **No board is involved.**
//!
//! - **Ordering:** for every Step, the events are, in this order: the guest sends its reply,
//!   the log's write, the log's sync starts, the sync ends, the service returns the response.
//!   With a sync that takes 80 ms, the response is observed no earlier than the sync's end.
//! - **Failure:** when the sync fails, the kernel gets `DATA_LOSS` and not the response, and
//!   the next exchange is refused with `FAILED_PRECONDITION` before anything is sent to the
//!   guest (the guest's STEP count does not move).
mod common;

use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use av_cdm::pb::{LockstepBindResponse, LockstepResetResponse, LockstepShutdownResponse, LockstepStepRequest, LockstepStepResponse, PortMessage};
use av_edge::board::parse_port_device;
use av_edge::board_log::{verify_bytes, BoardIoKind, BoardIoLogWriter, DurableSink, LogSigner, LogVerifier};
use av_edge_board::iolog::BoardIoLog;
use av_edge_board::service::BoardService;
use av_edge_board::timed::{LinkTimes, TimedStream};
use av_lockstep_shim::framing::{encode_hello, read_frame, write_frame, FrameType, PROTOCOL_VERSION};
use av_lockstep_shim::pb::lockstep_service_server::LockstepService;
use av_lockstep_shim::pb::{LockstepBindRequest, LockstepStepRequest as ShimStepRequest};
use av_lockstep_shim::PeerLink;
use prost::Message;
use tonic::{Code, Request};

type Events = Arc<Mutex<Vec<String>>>;

struct Sink {
    events: Events,
    bytes: Arc<Mutex<Vec<u8>>>,
    sync_delay: Duration,
    /// Fail the sync with this zero-based index.
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
        self.events.lock().unwrap().push("log_sync_start".into());
        std::thread::sleep(self.sync_delay);
        if self.fail_sync_at == Some(n) {
            self.events.lock().unwrap().push("log_sync_failed".into());
            return Err(io::Error::other("injected fsync failure"));
        }
        self.events.lock().unwrap().push("log_sync_end".into());
        Ok(())
    }
}

struct Rig {
    service: BoardService<TimedStream<tokio::io::DuplexStream>>,
    events: Events,
    log_bytes: Arc<Mutex<Vec<u8>>>,
    guest_steps: Arc<AtomicUsize>,
    verifier: LogVerifier,
    _dir: std::path::PathBuf,
}

async fn rig(tag: &str, sync_delay: Duration, fail_sync_at: Option<usize>) -> Rig {
    let dir = std::env::temp_dir().join(format!("av-edge-board-inproc-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let id = common::make_identity(&dir, "inproc");
    let signer = LogSigner::from_pem(&std::fs::read(&id.key_pem).unwrap(), &std::fs::read(&id.cert_pem).unwrap()).unwrap();
    let verifier = LogVerifier::from_pem(&std::fs::read(&id.cert_pem).unwrap()).unwrap();

    let events: Events = Arc::default();
    let log_bytes: Arc<Mutex<Vec<u8>>> = Arc::default();
    let sink = Sink { events: events.clone(), bytes: log_bytes.clone(), sync_delay, fail_sync_at, syncs: 0 };
    let writer = BoardIoLogWriter::with_sink(Path::new("memory"), Box::new(sink), "edge-inproc", "00", signer);
    let times = Arc::new(LinkTimes::default());
    let log = Arc::new(BoardIoLog::new(writer, times.clone()));

    let (ours, mut theirs) = tokio::io::duplex(1 << 20);
    let guest_steps = Arc::new(AtomicUsize::new(0));
    let (gs, ge) = (guest_steps.clone(), events.clone());
    tokio::spawn(async move {
        // The service speaks first: HELLO in, HELLO out.
        let (ty, _) = read_frame(&mut theirs).await.unwrap();
        assert_eq!(ty, FrameType::Hello);
        write_frame(&mut theirs, FrameType::Hello, &encode_hello(PROTOCOL_VERSION)).await.unwrap();
        while let Ok((ty, payload)) = read_frame(&mut theirs).await {
            match ty {
                FrameType::Bind => {
                    let ack = LockstepBindResponse { lockstep_capable: true, binding_hash: "cd".repeat(32), version: "inproc".into(), refusal_reason: String::new() };
                    write_frame(&mut theirs, FrameType::BindAck, &ack.encode_to_vec()).await.unwrap();
                }
                FrameType::Step => {
                    let req = LockstepStepRequest::decode(payload.as_slice()).unwrap();
                    gs.fetch_add(1, Ordering::SeqCst);
                    let resp = LockstepStepResponse { sequence: req.sequence, reached_tai_ns: req.until_tai_ns, outputs: vec![PortMessage { port: "out".into(), tai_ns: req.until_tai_ns, payload: vec![0xAA, req.sequence as u8] }], named_outputs: Default::default() };
                    ge.lock().unwrap().push(format!("guest_reply:{}", req.sequence));
                    write_frame(&mut theirs, FrameType::StepDone, &resp.encode_to_vec()).await.unwrap();
                }
                FrameType::Reset => {
                    let req = av_cdm::pb::LockstepResetRequest::decode(payload.as_slice()).unwrap();
                    write_frame(&mut theirs, FrameType::ResetAck, &LockstepResetResponse { sequence: req.sequence }.encode_to_vec()).await.unwrap();
                }
                FrameType::Shutdown => {
                    write_frame(&mut theirs, FrameType::ShutdownAck, &LockstepShutdownResponse {}.encode_to_vec()).await.unwrap();
                    return;
                }
                other => panic!("unexpected {other}"),
            }
        }
    });
    let peer = PeerLink::handshake(TimedStream::new(ours, times)).await.unwrap();
    let device = parse_port_device("udp://127.0.0.1:9").unwrap();
    let service = BoardService::new(Arc::new(peer), "edge-inproc".into(), device, log);
    Rig { service, events, log_bytes, guest_steps, verifier, _dir: dir }
}

fn bind_request() -> LockstepBindRequest {
    LockstepBindRequest { run_id: "run-x".into(), instance: "obc".into(), ..Default::default() }
}

fn step(seq: u64) -> Request<ShimStepRequest> {
    Request::new(ShimStepRequest { sequence: seq, until_tai_ns: seq as i64 * 1000, inputs: vec![PortMessage { port: "in".into(), tai_ns: 0, payload: vec![seq as u8; 4] }] })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_steps_record_is_written_and_synced_before_the_response_is_returned() {
    const DELAY: Duration = Duration::from_millis(80);
    let rig = rig("order", DELAY, None).await;
    rig.service.bind(Request::new(bind_request())).await.unwrap();
    for seq in 1..=3u64 {
        rig.events.lock().unwrap().clear();
        let t = Instant::now();
        let resp = rig.service.step(step(seq)).await.unwrap().into_inner();
        let took = t.elapsed();
        rig.events.lock().unwrap().push(format!("returned:{seq}"));
        assert_eq!(resp.outputs[0].payload, vec![0xAA, seq as u8]);
        let ev = rig.events.lock().unwrap().clone();
        assert_eq!(ev, [format!("guest_reply:{seq}"), "log_write".into(), "log_sync_start".into(), "log_sync_end".into(), format!("returned:{seq}")], "step {seq}");
        assert!(took >= DELAY, "the response waited for the {DELAY:?} sync (took {took:?})");
        println!("ORDER step {seq}: {} (call took {took:?})", ev.join(" -> "));
    }
    // What was written verifies and holds the three steps, outputs byte for byte.
    let log = verify_bytes(&rig.log_bytes.lock().unwrap(), &rig.verifier).unwrap();
    let kinds: Vec<_> = log.records.iter().map(|r| BoardIoKind::try_from(r.kind).unwrap()).collect();
    assert_eq!(kinds, [BoardIoKind::Bind, BoardIoKind::Step, BoardIoKind::Step, BoardIoKind::Step]);
    assert_eq!(log.records[2].outputs[0].payload, vec![0xAA, 2]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_sync_returns_data_loss_not_the_response_and_stops_all_further_exchanges() {
    // Syncs: BIND is #0, STEP 1 is #1, STEP 2 is #2 and fails.
    let rig = rig("fail", Duration::ZERO, Some(2)).await;
    rig.service.bind(Request::new(bind_request())).await.unwrap();
    rig.service.step(step(1)).await.unwrap();
    assert_eq!(rig.guest_steps.load(Ordering::SeqCst), 1);

    let err = rig.service.step(step(2)).await.expect_err("the response must not be returned when its record is not durable");
    assert_eq!(err.code(), Code::DataLoss, "{err}");
    assert!(err.message().contains("injected fsync failure") && err.message().contains("the run must stop"), "{}", err.message());
    assert_eq!(rig.guest_steps.load(Ordering::SeqCst), 2, "the board did run step 2");

    // Everything after is refused up front; the guest sees nothing.
    for result in [rig.service.step(step(3)).await.map(|_| ()), rig.service.bind(Request::new(bind_request())).await.map(|_| ())] {
        let err = result.expect_err("the poisoned log refuses further exchanges");
        assert_eq!(err.code(), Code::FailedPrecondition, "{err}");
        assert!(err.message().contains("refusing to exchange anything with the board"), "{}", err.message());
    }
    assert_eq!(rig.guest_steps.load(Ordering::SeqCst), 2, "no further STEP reached the board");
    println!("DATA-LOSS {}", err.message());
}
