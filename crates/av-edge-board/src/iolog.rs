//! [`BoardIoLog`]: the service's side of the board I/O log (question 242 (a)). It owns the
//! `av_edge::board_log::BoardIoLogWriter`, serialises exchanges, remembers the run and
//! instance the kernel's Bind named, stamps the link's write/read instants on each record,
//! and turns a log failure into a gRPC status.
//!
//! # The ordering rule
//!
//! `BoardService` calls [`BoardIoLog::begin`], performs the exchange with the board, and calls
//! [`BoardIoLog::record`] **before it returns the board's response (or error) to the kernel**.
//! `record` returns only after the record was written and `fsync`ed
//! (`BoardIoLogWriter::append`), on a blocking thread so the runtime keeps serving. So when the
//! kernel sees a STEP's response, that step's record is already durable.
//!
//! One exchange at a time: `begin` takes a gate held until the guard drops, so records are in
//! the order the exchanges happened and the link instants cannot mix two exchanges.
//!
//! # When logging fails
//!
//! The kernel gets `DATA_LOSS` instead of the response, and the log is poisoned: every later
//! exchange is refused with `FAILED_PRECONDITION` **before anything is sent to the board**.
//! A run must not continue with an I/O the log does not hold.
use std::sync::{Arc, Mutex};

use av_edge::board_log::{AppendReceipt, BoardIoLogWriter, BoardIoRecord};
use tokio::sync::{Mutex as AsyncMutex, MutexGuard};
use tonic::Status;

use crate::timed::LinkTimes;

pub struct BoardIoLog {
    writer: Arc<Mutex<BoardIoLogWriter>>,
    times: Arc<LinkTimes>,
    run: Mutex<(String, String)>,
    gate: AsyncMutex<()>,
}

/// Held for the duration of one exchange (see [`BoardIoLog::begin`]).
pub struct ExchangeGuard<'a>(#[allow(dead_code)] MutexGuard<'a, ()>);

impl BoardIoLog {
    /// `times` must be the [`LinkTimes`] the link's `TimedStream` updates.
    pub fn new(writer: BoardIoLogWriter, times: Arc<LinkTimes>) -> Self {
        Self { writer: Arc::new(Mutex::new(writer)), times, run: Mutex::new((String::new(), String::new())), gate: AsyncMutex::new(()) }
    }

    /// Start an exchange: wait for any other to finish, refuse if the log has failed (nothing
    /// may reach the board then), and clear the link instants.
    pub async fn begin(&self) -> Result<ExchangeGuard<'_>, Status> {
        let guard = self.gate.lock().await;
        if let Some(reason) = self.writer.lock().unwrap_or_else(|e| e.into_inner()).failure() {
            return Err(Status::failed_precondition(format!("the board I/O log has failed ({reason}); refusing to exchange anything with the board that the log cannot hold")));
        }
        self.times.reset();
        Ok(ExchangeGuard(guard))
    }

    /// Remember the run and instance of the kernel's Bind (stamped on later records).
    pub fn set_run(&self, run_id: &str, instance: &str) {
        *self.run.lock().unwrap_or_else(|e| e.into_inner()) = (run_id.to_string(), instance.to_string());
    }

    /// Complete `draft` with the run, instance and link instants, then sign, write and `fsync`
    /// it. Returns only when it is durable. Errors are `DATA_LOSS`: the exchange happened but
    /// is not logged.
    pub async fn record(&self, _exchange: &ExchangeGuard<'_>, mut draft: BoardIoRecord) -> Result<AppendReceipt, Status> {
        {
            let run = self.run.lock().unwrap_or_else(|e| e.into_inner());
            if draft.run_id.is_empty() {
                draft.run_id = run.0.clone();
            }
            if draft.instance.is_empty() {
                draft.instance = run.1.clone();
            }
        }
        // The link's instants, unless the draft already carries its own (a power cycle's are the
        // instants its channel was started and finished: it crosses no link).
        let (written, read) = self.times.snapshot();
        if draft.request_written_unix_ns == 0 {
            draft.request_written_unix_ns = written;
        }
        if draft.response_read_unix_ns == 0 {
            draft.response_read_unix_ns = read;
        }
        let writer = Arc::clone(&self.writer);
        let joined = tokio::task::spawn_blocking(move || writer.lock().unwrap_or_else(|e| e.into_inner()).append(draft)).await;
        match joined {
            Ok(Ok(receipt)) => Ok(receipt),
            Ok(Err(e)) => {
                eprintln!("av-edge-board: ERROR: the board I/O record could not be made durable: {e}");
                Err(Status::data_loss(format!("the board's I/O could not be logged durably: {e}; the run must stop")))
            }
            Err(e) => Err(Status::data_loss(format!("the board I/O log writer task failed: {e}"))),
        }
    }

    /// Records durably written so far.
    pub fn records_written(&self) -> u64 {
        self.writer.lock().unwrap_or_else(|e| e.into_inner()).records_written()
    }
}

impl BoardIoLog {
    /// The chain head as lowercase hex, or `GENESIS` while the log is empty.
    pub fn chain_head_hex(&self) -> String {
        let w = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        if w.records_written() == 0 {
            "GENESIS".to_string()
        } else {
            av_edge::hash::hex_encode(w.chain_head())
        }
    }
}
