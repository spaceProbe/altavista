//! [`BoardEdge`]: `altavista.v1.BoardEdgeService`, the board's edge-node operations that are not
//! lockstep traffic (question 242 (c); `proto/altavista/v1/board.proto`), served on the same gRPC
//! address as `LockstepService`. Today there is one RPC, `PowerCycle`.
//!
//! # Power control is run here, never by the kernel
//!
//! `BoardBinding.power_control` is the **edge node's** power control channel, and the board may
//! hang off this host or off a separate Linux edge node. The kernel's power-cycle fault therefore
//! asks this service, at the address it already dials for the board, to run the channel this
//! process was configured with (`--power-control`, parsed by `av_edge::board::parse_power_control`;
//! absent means none). The channel runs here, locally, and the kernel gets a typed result. Same
//! host or not, the kernel behaves identically.
//!
//! # What a request must satisfy
//!
//! - **`instance` and `fault_id` are non-empty** with no control characters (at most 256 bytes
//!   each) or the RPC is `INVALID_ARGUMENT` and nothing runs: they become `argv` entries.
//! - **This service has a channel**, else the response is a refusal, `NO_CHANNEL`.
//! - **The request's `power_control` equals this service's own channel in canonical form**
//!   (`PowerControl::canonical`; `cmd:/x` and an unparsable string differ), else `CHANNEL_MISMATCH`.
//! - **The request's `edge_node_id` is this service's own**, else `EDGE_NODE_MISMATCH`.
//!
//! A refusal runs nothing, touches no log, and is an ordinary response (outcome `REFUSED`), not a
//! gRPC error.
//!
//! # Running a `cmd:` channel
//!
//! `<path> power-cycle --edge-node-id <id> --instance <name> --fault-id <id> --tai-ns <n>`
//! (`av_edge::board::power_cycle_argv`): **no shell**, the working directory `/`, the environment
//! this process has, standard input and standard output `/dev/null`, standard error captured
//! (the last 4096 bytes are returned, the pipe is drained to its end so the child never blocks on
//! it). The child is started in its **own process group**; on timeout the whole group is killed
//! (`SIGKILL`), so a script's own children go with it, and the child is then reaped. Exit status 0
//! is `PERFORMED`; a non-zero status, a signal, a timeout or a failure to start is `FAILED` with
//! the matching `failure` and a `detail`. The default timeout is [`DEFAULT_POWER_TIMEOUT_MS`]
//! (`--power-timeout-ms`).
//!
//! # The board I/O log
//!
//! A channel that was **started** (performed or failed) is recorded as one `BOARD_IO_KIND_POWER_CYCLE`
//! record, durable before the reply returns (the ordering rule of [`crate::iolog`]): `run_id`,
//! `instance`, `reset_tai_ns` = the fault's epoch, `reset_reason` = `fault:<fault id>`,
//! `request_written_unix_ns` / `response_read_unix_ns` = the instants the channel was started and
//! finished (edge-node wall clock; informational, as for every record), and `error` set for a
//! failure. So the HIL run's evidence shows the operation between the STEP records on either side
//! of it. A refusal ran nothing and is not logged (as a refused Bind is not). The operation takes
//! the log's exchange gate for its whole duration: no STEP, RESET or second power cycle interleaves
//! with it, and a log that has failed refuses it with `FAILED_PRECONDITION` before anything runs.
//! If the record cannot be written after the channel ran, the reply is `DATA_LOSS` (the log is
//! poisoned, as for every exchange).
//!
//! # Not done this round (HIL-day item)
//!
//! This service performs the power cycle; it does **not** bring the flight software back. A real
//! board that actually reboots drops the lockstep-local link, so afterwards this process must
//! re-handshake with the new flight-software instance (HELLO once, as at startup) and the kernel
//! must `Bind` again before its `RESET`. Neither exists: the stand-in's fake guest keeps running
//! through the power cycle, so the existing `RESET` that the kernel sends right after works, and
//! the re-handshake / re-`Bind` path cannot be exercised without a board. See the crate README.
use std::os::unix::process::ExitStatusExt;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use av_cdm::pb::{BoardPowerCycleFailure, BoardPowerCycleOutcome, BoardPowerCycleRefusal, PowerCycleRequest, PowerCycleResponse};
use av_edge::board::{parse_power_control, power_cycle_argv, PowerControl};
use av_edge::board_log::{BoardIoKind, BoardIoRecord};
use tokio::io::AsyncReadExt;
use tonic::{Request, Response, Status};

use crate::iolog::BoardIoLog;

pub mod pb {
    //! Generated `altavista.v1.board_edge_service_server` plumbing only; the messages are
    //! `av_cdm::pb`'s own (`build.rs`).
    #![allow(clippy::all)]
    include!(concat!(env!("OUT_DIR"), "/altavista.v1.rs"));
}
pub use pb::board_edge_service_server::{BoardEdgeService, BoardEdgeServiceServer};

/// How long a `cmd:` channel may run before it is killed, unless `--power-timeout-ms` says otherwise.
pub const DEFAULT_POWER_TIMEOUT_MS: u64 = 30_000;
/// Bounds of `--power-timeout-ms`.
pub const POWER_TIMEOUT_BOUNDS_MS: std::ops::RangeInclusive<u64> = 10..=600_000;
/// The most standard error the response carries (its last bytes).
pub const STDERR_TAIL_BYTES: usize = 4096;
/// Longest `instance` / `fault_id` accepted.
const MAX_ARG_LEN: usize = 256;
/// After the child is gone, how long to wait for the standard error pipe to close (a daemonised
/// grandchild that kept it open must not hold the reply).
const STDERR_DRAIN_GRACE: Duration = Duration::from_secs(2);

/// What the pure check decided about a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Run this executable.
    Run { path: String },
    /// Refuse without running anything.
    Refuse { refusal: BoardPowerCycleRefusal, detail: String },
}

/// The pure half of the request check: no I/O. `Err` is the `INVALID_ARGUMENT` message.
pub fn decide(own_edge_node_id: &str, own_channel: &PowerControl, req: &PowerCycleRequest) -> Result<Decision, String> {
    for (name, value) in [("instance", &req.instance), ("fault_id", &req.fault_id)] {
        if value.is_empty() {
            return Err(format!("PowerCycleRequest.{name} is empty"));
        }
        if value.len() > MAX_ARG_LEN {
            return Err(format!("PowerCycleRequest.{name} is {} bytes, longer than {MAX_ARG_LEN}", value.len()));
        }
        if let Some(ch) = value.chars().find(|c| c.is_control()) {
            return Err(format!("PowerCycleRequest.{name} contains the control character {ch:?}"));
        }
    }
    let refuse = |refusal, detail: String| Ok(Decision::Refuse { refusal, detail });
    let PowerControl::Cmd { path } = own_channel else {
        return refuse(
            BoardPowerCycleRefusal::NoChannel,
            format!("this edge service (edge node {own_edge_node_id:?}) has no power-control channel configured (--power-control); the kernel asked for {:?}", req.power_control),
        );
    };
    let own_canonical = own_channel.canonical();
    match parse_power_control(&req.power_control) {
        Ok(requested) if requested.canonical() == own_canonical => {}
        Ok(requested) => {
            return refuse(BoardPowerCycleRefusal::ChannelMismatch, format!("the kernel's binding declares power control {:?}, but this edge service is configured with {own_canonical:?}", requested.canonical()));
        }
        Err(e) => {
            return refuse(BoardPowerCycleRefusal::ChannelMismatch, format!("the kernel's power control {:?} does not parse ({e}); this edge service is configured with {own_canonical:?}", req.power_control));
        }
    }
    if req.edge_node_id != own_edge_node_id {
        return refuse(BoardPowerCycleRefusal::EdgeNodeMismatch, format!("the kernel addressed edge node {:?}, but this edge service is edge node {own_edge_node_id:?}", req.edge_node_id));
    }
    Ok(Decision::Run { path: path.clone() })
}

/// How a started channel ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelEnd {
    Success,
    Failed { failure: BoardPowerCycleFailure, exit_status: i32, signal: i32, detail: String },
}

/// Everything about one run of a `cmd:` channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelRun {
    pub end: ChannelEnd,
    /// The last [`STDERR_TAIL_BYTES`] of standard error, lossily UTF-8.
    pub stderr_tail: String,
    pub duration_ns: i64,
    pub started_unix_ns: i64,
    pub finished_unix_ns: i64,
}

fn unix_ns() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as i64).unwrap_or(0)
}

/// Keep only the last `STDERR_TAIL_BYTES` bytes seen.
struct Tail(Vec<u8>);

impl Tail {
    fn push(&mut self, chunk: &[u8]) {
        self.0.extend_from_slice(chunk);
        if self.0.len() > STDERR_TAIL_BYTES {
            let excess = self.0.len() - STDERR_TAIL_BYTES;
            self.0.drain(..excess);
        }
    }
}

/// Run `path` with `args` (no shell) and return how it ended, killing its whole process group if
/// it has not finished within `timeout`. Never panics; every failure is a [`ChannelEnd::Failed`].
pub async fn run_cmd(path: &str, args: &[String], timeout: Duration) -> ChannelRun {
    let started_unix_ns = unix_ns();
    let t0 = Instant::now();
    let finish = |end: ChannelEnd, stderr_tail: String| ChannelRun { end, stderr_tail, duration_ns: t0.elapsed().as_nanos() as i64, started_unix_ns, finished_unix_ns: unix_ns() };

    let mut command = tokio::process::Command::new(path);
    command.args(args).current_dir("/").stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped()).process_group(0).kill_on_drop(true);
    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(e) => return finish(ChannelEnd::Failed { failure: BoardPowerCycleFailure::Spawn, exit_status: 0, signal: 0, detail: format!("could not start {path:?}: {e}") }, String::new()),
    };
    let pid = child.id();
    let mut stderr = child.stderr.take().expect("stderr was piped");
    // Drain standard error to its end, keeping the tail, concurrently with waiting.
    let reader = tokio::spawn(async move {
        let mut tail = Tail(Vec::new());
        let mut buf = [0u8; 2048];
        loop {
            match stderr.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => tail.push(&buf[..n]),
            }
        }
        tail.0
    });

    let waited = tokio::time::timeout(timeout, child.wait()).await;
    let (end, timed_out) = match waited {
        Ok(Ok(status)) => (
            match (status.code(), status.signal()) {
                (Some(0), _) => ChannelEnd::Success,
                (Some(code), _) => ChannelEnd::Failed { failure: BoardPowerCycleFailure::ExitStatus, exit_status: code, signal: 0, detail: format!("{path:?} exited with status {code}") },
                (None, Some(sig)) => ChannelEnd::Failed { failure: BoardPowerCycleFailure::Signaled, exit_status: 0, signal: sig, detail: format!("{path:?} was ended by signal {sig}") },
                (None, None) => ChannelEnd::Failed { failure: BoardPowerCycleFailure::ExitStatus, exit_status: -1, signal: 0, detail: format!("{path:?} ended with an unrecognised wait status") },
            },
            false,
        ),
        Ok(Err(e)) => (ChannelEnd::Failed { failure: BoardPowerCycleFailure::Spawn, exit_status: 0, signal: 0, detail: format!("waiting for {path:?} failed: {e}") }, false),
        Err(_) => (ChannelEnd::Failed { failure: BoardPowerCycleFailure::Timeout, exit_status: 0, signal: 0, detail: format!("{path:?} did not finish within {} ms and was killed", timeout.as_millis()) }, true),
    };
    if timed_out || matches!(end, ChannelEnd::Failed { failure: BoardPowerCycleFailure::Spawn, .. }) {
        // Kill the whole group (the child is its leader), then reap the child.
        if let Some(pid) = pid {
            // SAFETY: `killpg` with a pid this process started as a group leader; failure (the
            // group is already gone) is ignored.
            unsafe {
                libc::killpg(pid as libc::pid_t, libc::SIGKILL);
            }
        }
        let _ = child.start_kill();
        let _ = child.wait().await;
    }
    let tail = match tokio::time::timeout(STDERR_DRAIN_GRACE, reader).await {
        Ok(Ok(bytes)) => bytes,
        _ => Vec::new(),
    };
    finish(end, String::from_utf8_lossy(&tail).into_owned())
}

/// The kernel-facing `BoardEdgeService` for one board link.
pub struct BoardEdge {
    edge_node_id: String,
    channel: PowerControl,
    timeout: Duration,
    log: Arc<BoardIoLog>,
}

impl BoardEdge {
    pub fn new(edge_node_id: String, channel: PowerControl, timeout: Duration, log: Arc<BoardIoLog>) -> Self {
        Self { edge_node_id, channel, timeout, log }
    }
}

fn refused(refusal: BoardPowerCycleRefusal, detail: String, own_channel: &PowerControl) -> PowerCycleResponse {
    PowerCycleResponse { outcome: BoardPowerCycleOutcome::Refused as i32, refusal: refusal as i32, detail, power_control: own_channel.canonical(), ..Default::default() }
}

#[tonic::async_trait]
impl BoardEdgeService for BoardEdge {
    async fn power_cycle(&self, request: Request<PowerCycleRequest>) -> Result<Response<PowerCycleResponse>, Status> {
        let req = request.into_inner();
        let path = match decide(&self.edge_node_id, &self.channel, &req).map_err(Status::invalid_argument)? {
            Decision::Refuse { refusal, detail } => {
                eprintln!("av-edge-board: PowerCycle refused ({refusal:?}), nothing run: {detail}");
                return Ok(Response::new(refused(refusal, detail, &self.channel)));
            }
            Decision::Run { path } => path,
        };
        // One exchange at a time, and nothing at all once the log has failed.
        let exchange = self.log.begin().await?;
        let args = power_cycle_argv(&self.edge_node_id, &req.instance, &req.fault_id, req.tai_ns);
        eprintln!("av-edge-board: PowerCycle for run {:?} instance {:?} fault {:?} at tai_ns={}: running {path} (timeout {} ms)", req.run_id, req.instance, req.fault_id, req.tai_ns, self.timeout.as_millis());
        let run = run_cmd(&path, &args, self.timeout).await;
        let (outcome, failure, exit_status, signal, detail) = match &run.end {
            ChannelEnd::Success => (BoardPowerCycleOutcome::Performed, BoardPowerCycleFailure::Unspecified, 0, 0, String::new()),
            ChannelEnd::Failed { failure, exit_status, signal, detail } => (BoardPowerCycleOutcome::Failed, *failure, *exit_status, *signal, detail.clone()),
        };
        eprintln!("av-edge-board: PowerCycle {outcome:?} after {} ms{}", run.duration_ns / 1_000_000, if detail.is_empty() { String::new() } else { format!(": {detail}") });

        // The record is durable before the reply is returned.
        let draft = BoardIoRecord {
            kind: BoardIoKind::PowerCycle as i32,
            run_id: req.run_id.clone(),
            instance: req.instance.clone(),
            reset_tai_ns: req.tai_ns,
            reset_reason: format!("fault:{}", req.fault_id),
            error: if detail.is_empty() { String::new() } else { format!("{failure:?}: {detail}") },
            request_written_unix_ns: run.started_unix_ns,
            response_read_unix_ns: run.finished_unix_ns,
            ..Default::default()
        };
        self.log.record(&exchange, draft).await?;
        Ok(Response::new(PowerCycleResponse {
            outcome: outcome as i32,
            refusal: BoardPowerCycleRefusal::Unspecified as i32,
            failure: failure as i32,
            detail,
            exit_status,
            signal,
            stderr_tail: run.stderr_tail,
            power_control: self.channel.canonical(),
            duration_ns: run.duration_ns,
            started_unix_ns: run.started_unix_ns,
            finished_unix_ns: run.finished_unix_ns,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(path: &str) -> PowerControl {
        parse_power_control(&format!("cmd:{path}")).unwrap()
    }

    fn request(power_control: &str) -> PowerCycleRequest {
        PowerCycleRequest { run_id: "r".into(), instance: "controller".into(), fault_id: "pc1".into(), tai_ns: 42, edge_node_id: "edge-7".into(), power_control: power_control.into() }
    }

    #[test]
    fn a_matching_request_runs_the_configured_executable() {
        assert_eq!(decide("edge-7", &cmd("/opt/ps"), &request("cmd:/opt/ps")), Ok(Decision::Run { path: "/opt/ps".into() }));
    }

    #[test]
    fn a_service_without_a_channel_refuses_whatever_the_kernel_declares() {
        for declared in ["", "cmd:/opt/ps"] {
            let Ok(Decision::Refuse { refusal, detail }) = decide("edge-7", &PowerControl::None, &request(declared)) else { panic!("must refuse") };
            assert_eq!(refusal, BoardPowerCycleRefusal::NoChannel);
            assert!(detail.contains("no power-control channel"), "{detail}");
        }
    }

    #[test]
    fn a_channel_that_differs_from_the_services_own_is_refused() {
        for declared in ["", "cmd:/opt/other", "cmd:/opt/ps/", "gpio://17", "CMD:/opt/ps", "cmd:relative"] {
            let Ok(Decision::Refuse { refusal, detail }) = decide("edge-7", &cmd("/opt/ps"), &request(declared)) else { panic!("{declared:?} must be refused") };
            assert_eq!(refusal, BoardPowerCycleRefusal::ChannelMismatch, "{declared:?}");
            assert!(detail.contains("cmd:/opt/ps"), "the reason names the service's own channel: {detail}");
        }
    }

    #[test]
    fn an_edge_node_that_differs_is_refused() {
        let mut r = request("cmd:/opt/ps");
        r.edge_node_id = "edge-8".into();
        let Ok(Decision::Refuse { refusal, detail }) = decide("edge-7", &cmd("/opt/ps"), &r) else { panic!("must refuse") };
        assert_eq!(refusal, BoardPowerCycleRefusal::EdgeNodeMismatch);
        assert!(detail.contains("edge-8") && detail.contains("edge-7"), "{detail}");
    }

    #[test]
    fn malformed_request_fields_are_invalid_arguments_not_refusals() {
        for mutate in [
            (|r: &mut PowerCycleRequest| r.instance.clear()) as fn(&mut PowerCycleRequest),
            |r| r.fault_id.clear(),
            |r| r.instance = "a\0b".into(),
            |r| r.fault_id = "a\nb".into(),
            |r| r.fault_id = "x".repeat(257),
        ] {
            let mut r = request("cmd:/opt/ps");
            mutate(&mut r);
            assert!(decide("edge-7", &cmd("/opt/ps"), &r).is_err(), "{r:?}");
            // Even a service with no channel reports a malformed request as such.
            assert!(decide("edge-7", &PowerControl::None, &r).is_err(), "{r:?}");
        }
    }

    // --- the runner, against real processes (/bin/sh is on every host this runs on) ---

    fn sh(script: &str) -> (String, Vec<String>) {
        ("/bin/sh".into(), vec!["-c".into(), script.into()])
    }

    #[tokio::test]
    async fn exit_zero_is_success_and_the_stderr_tail_is_captured() {
        let (p, a) = sh("echo diagnostics >&2; exit 0");
        let run = run_cmd(&p, &a, Duration::from_secs(10)).await;
        assert_eq!(run.end, ChannelEnd::Success);
        assert_eq!(run.stderr_tail, "diagnostics\n");
        assert!(run.duration_ns > 0 && run.finished_unix_ns >= run.started_unix_ns && run.started_unix_ns > 0);
    }

    #[tokio::test]
    async fn a_non_zero_exit_is_a_failure_with_its_status_and_stderr() {
        let (p, a) = sh("echo relay stuck >&2; exit 7");
        let run = run_cmd(&p, &a, Duration::from_secs(10)).await;
        let ChannelEnd::Failed { failure, exit_status, signal, detail } = run.end else { panic!("must fail") };
        assert_eq!((failure, exit_status, signal), (BoardPowerCycleFailure::ExitStatus, 7, 0));
        assert!(detail.contains("status 7"), "{detail}");
        assert_eq!(run.stderr_tail, "relay stuck\n");
    }

    #[tokio::test]
    async fn a_signalled_child_is_a_signalled_failure() {
        let (p, a) = sh("kill -TERM $$");
        let run = run_cmd(&p, &a, Duration::from_secs(10)).await;
        let ChannelEnd::Failed { failure, signal, .. } = run.end else { panic!("must fail") };
        assert_eq!((failure, signal), (BoardPowerCycleFailure::Signaled, libc::SIGTERM));
    }

    #[tokio::test]
    async fn a_missing_executable_is_a_spawn_failure_naming_it() {
        let run = run_cmd("/no/such/power-cycle", &[], Duration::from_secs(10)).await;
        let ChannelEnd::Failed { failure, detail, .. } = run.end else { panic!("must fail") };
        assert_eq!(failure, BoardPowerCycleFailure::Spawn);
        assert!(detail.contains("/no/such/power-cycle"), "{detail}");
        assert!(run.duration_ns >= 0 && run.started_unix_ns > 0);
    }

    /// The child (and the grandchild it started) are really gone after a timeout: the shell
    /// writes the grandchild's pid, and `kill -0` on it must fail once the call returned.
    #[tokio::test]
    async fn a_timeout_kills_the_whole_process_group_and_returns_promptly() {
        let dir = std::env::temp_dir().join(format!("av-edge-board-power-unit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let pidfile = dir.join("pids");
        let script = format!("echo $$ > {p}; sleep 300 & echo $! >> {p}; wait", p = pidfile.display());
        let (path, args) = sh(&script);
        let t0 = Instant::now();
        let run = run_cmd(&path, &args, Duration::from_millis(400)).await;
        let wall = t0.elapsed();
        let ChannelEnd::Failed { failure, detail, .. } = &run.end else { panic!("must fail: {run:?}") };
        assert_eq!(*failure, BoardPowerCycleFailure::Timeout);
        assert!(detail.contains("400 ms"), "{detail}");
        assert!(wall >= Duration::from_millis(400) && wall < Duration::from_secs(5), "{wall:?}");
        let pids: Vec<i32> = std::fs::read_to_string(&pidfile).unwrap().lines().map(|l| l.trim().parse().unwrap()).collect();
        assert_eq!(pids.len(), 2, "the shell and its sleep: {pids:?}");
        for pid in pids {
            // SAFETY: signal 0 probes existence only.
            let alive = unsafe { libc::kill(pid, 0) } == 0;
            assert!(!alive, "pid {pid} must be dead after the timeout");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn the_stderr_tail_is_bounded_to_its_last_bytes() {
        // 10,000 bytes of 'a' then a marker: only the end survives.
        let (p, a) = sh("i=0; while [ $i -lt 100 ]; do printf 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa' >&2; i=$((i+1)); done; printf END >&2; exit 3");
        let run = run_cmd(&p, &a, Duration::from_secs(10)).await;
        assert_eq!(run.stderr_tail.len(), STDERR_TAIL_BYTES);
        assert!(run.stderr_tail.ends_with("aaaEND"), "the end of what it wrote is kept");
    }

    #[tokio::test]
    async fn arguments_reach_the_child_unmodified_with_no_shell() {
        let dir = std::env::temp_dir().join(format!("av-edge-board-power-argv-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("argv");
        // `$0` is the first word after the script; every later word arrives as its own argument
        // even when it holds shell metacharacters.
        let script = format!("printf '%s\\n' \"$@\" > {}", out.display());
        let args = vec!["-c".to_string(), script, "sh".to_string(), "$(echo hacked)".to_string(), "a b".to_string(), "; echo hi".to_string()];
        let run = run_cmd("/bin/sh", &args, Duration::from_secs(10)).await;
        assert_eq!(run.end, ChannelEnd::Success);
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "$(echo hacked)\na b\n; echo hi\n");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
