//! Replaying a board-bound run with the board replaced by its signed edge I/O log (question 242,
//! hilprep-2b; architecture.md section 3: "A HIL run logs the board's I/O on the durable log so
//! everything else still replays"; ADR-005 section 4).
//!
//! # What this module owns
//!
//! 1. **The pin** ([`BoardLogPin`]): when a board-bound run ends, the kernel asks the board's edge
//!    service where its signed log ends ([`fetch_pin`]; `BoardEdgeService.BoardIoLogHead`) and
//!    records the answer in the run's products, on the board instance's `Trajectory.provenance`
//!    next to its binding hashes: `board_io_log_chain_head` (hex), `board_io_log_records` and
//!    `board_io_log_signer_cert_sha256`. A hash chain cannot show that records were removed from
//!    its end (`av_edge::board_log`'s module doc says so), so this is what makes a truncation
//!    detectable. *Why provenance attributes and not a `RunProducts` field:* the facts belong to
//!    one board instance (a run may have several), they sit beside `board_binding_hash` and
//!    `board_link_hash`, which are the same kind of fact (what the board side was), and it needs
//!    no proto change.
//! 2. **The load** ([`load`]): the log is verified (chain, every signature against the
//!    certificate the caller supplies, no torn tail), compared with the pin, and checked against
//!    the run being replayed -- **all before anything binds**. Every failure is a typed
//!    [`BoardReplayRefusal`].
//! 3. **The script** ([`BoardReplayScript`]): what the replay model plays. Built from the loaded
//!    log and the run's shape (start, end, the instance's period, its power-cycle faults), and
//!    refused unless the log is the log of *this* run's shape.
//!
//! # The order at the end of a run, and what the pin covers
//!
//! The edge service records the SHUTDOWN exchange and then exits, so it cannot reliably answer a
//! head request after SHUTDOWN. The kernel therefore asks **after the last STEP of the last span
//! and before its SHUTDOWN**: the pin is `(records, chain head)` of the log as it stands at the
//! end of the run's I/O (BIND, every STEP, every power cycle and its RESET). The SHUTDOWN
//! exchange is then the one record after the pin, chained from the pinned head. A replay
//! requires **exactly that**: the log has the pinned number of records, the record at that
//! position hashes to the pinned head, signed by the pinned certificate, and exactly one record
//! follows it, of kind SHUTDOWN. A completed run always has it (the kernel's Shutdown must
//! succeed, and the service records it before it replies), so requiring it costs nothing and
//! makes a log cut at *any* point refuse: cut a STEP and the count falls short
//! ([`BoardReplayRefusal::Truncated`]); cut only the SHUTDOWN and the closing record is missing
//! ([`BoardReplayRefusal::ShutdownMissing`]); append anything else
//! ([`BoardReplayRefusal::UnpinnedTail`]); replace history and re-sign it with the genuine key and
//! the head differs ([`BoardReplayRefusal::ChainHeadMismatch`]).
//!
//! # Which STEP output becomes which recorded frame (the mapping, proven)
//!
//! A live board's `STEP` response `outputs` become the instance's `Outbox` through one function,
//! `binding::outbox_from_lockstep_outputs`: each message keeps its port, its own `tai_ns` and its
//! exact payload. [`crate::router::Router::deliver`] then records an OUT frame (FRAMED and
//! BYTE_STREAM ports) at **the message's own `tai_ns`, or, when that is
//! [`crate::router::NO_MESSAGE_EPOCH`] (0), at the STEP's end epoch** -- the STEP record's
//! `until_tai_ns`, which the kernel checks equals the response's `reached_tai_ns`. The replay
//! applies the *same function* to the logged response, at the STEP whose `until_tai_ns` equals
//! the call's end, so what the router records is the same by construction;
//! [`out_frames_of_log`] is the independent statement of that mapping the tests compare with the
//! live run's own `port_traffic.pb` records, record for record. The STEP's `named_outputs` become
//! the replayed `StepResult.outputs` (so `output.<instance>.<name>@time` scores reproduce, which
//! the port-traffic replay of a container cannot do). The STEP's logged `inputs` are not played:
//! they are what the rest of the run delivered, and the rest of the run runs again.
//!
//! # Every step is accounted for
//!
//! Unlike a port-traffic replay, which cannot tell a quiet step from a deleted record at the edges
//! of a log, a board answers **every** STEP and the service logs every one. The script therefore
//! requires a STEP record for every step the replay takes, in order, from `start + period` to the
//! run's end, and refuses (before binding) a log that does not have exactly those
//! ([`BoardReplayRefusal::DoesNotMatchRun`]).
//!
//! # Power cycles
//!
//! A board's `power_cycle` fault is a boundary in the live run: the edge service runs its
//! power-control channel (a `POWER_CYCLE` record), then the kernel sends a RESET. **In a replay
//! the power control is never called -- there is no board and no edge service.** The `fault:<id>`
//! event is reproduced as the live run built it (it carries nothing wall-clock dependent), and
//! the board's outcome event (`marker:power_cycle:<id>`) is **reproduced from the log's
//! `POWER_CYCLE` record**: same id, entity, epoch, kind, name, reference and `performed` flag,
//! with the duration computed from the record's two edge-node instants. The log does not hold
//! what the channel wrote to standard error, and the live duration is measured on a monotonic
//! clock, so the event's `detail` and `values["duration_ns"]` are the wall-clock values that
//! differ (named in [`crate::pacing::WALL_CLOCK_DEPENDENT`]); everything else about the event is
//! compared. A fault the DRM declares that the log has no `POWER_CYCLE` record for, or a record
//! the DRM does not declare, is refused ([`BoardReplayRefusal`]).
//!
//! # No pacing in a replay
//!
//! A replay substitutes the log for the board, so **no board is bound and the run is lockstep**:
//! no `PacingReport`, no overrun events. That is ADR-005's rule ("only when the configuration
//! binds a board"), not an accident.
//!
//! # Comparing a replay with its live run
//!
//! [`strip_replay_exclusions`] removes exactly the fields [`crate::pacing::WALL_CLOCK_DEPENDENT`]
//! names, so a test and a human apply one contract. A replay **from the edge log** reproduces the
//! board segment's `dynamics_*` and the binding-hash provenance exactly (the declared
//! configuration plus the version and binding hash the log's BIND record carries) and
//! re-stamps the verified pin, so those are *not* excluded for it; a replay of a board from the
//! run's `port_traffic.pb` (a container-style replay, selectable by naming the board in
//! `ReplayConfig.instances` without supplying a log) cannot know them and excludes them, as a
//! replayed container does.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use av_cdm::pb;
use av_edge::board_log::{verify_bytes, BoardIoKind, BoardIoRecord, BoardLogError, LogVerifier, VerifiedLog};
use av_edge::hash::hex_encode;
use av_lockstep::board_edge::{BlockingBoardEdgeClient, BoardIoLogHeadRequest};

use super::binding::ContainerSpec;
use super::power::{is_power_cycle_event, POWER_CYCLE_EVENT_ID_PREFIX};
use super::DrmError;
use crate::router::NO_MESSAGE_EPOCH;

/// `Trajectory.provenance.attributes` key: the edge log's chain head (lowercase hex) when the run
/// ended its I/O -- see the module doc, "the order at the end of a run".
pub const PIN_CHAIN_HEAD_KEY: &str = "board_io_log_chain_head";
/// `Trajectory.provenance.attributes` key: the number of records in the edge log at that point.
pub const PIN_RECORDS_KEY: &str = "board_io_log_records";
/// `Trajectory.provenance.attributes` key: SHA-256 (hex) of the certificate the log is signed by.
pub const PIN_SIGNER_KEY: &str = "board_io_log_signer_cert_sha256";

/// Where a board's edge log ended when the live run ended its I/O: what the run's products pin
/// and what a replay compares the log it is given with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoardLogPin {
    /// The record at position `records` (1-based): its `record_hash`, lowercase hex.
    pub chain_head: String,
    /// Records in the log at that point (the SHUTDOWN record is the one after).
    pub records: u64,
    /// Lowercase hex SHA-256 of the log signer's certificate.
    pub signer_cert_sha256: String,
}

impl BoardLogPin {
    /// The three provenance attributes that carry the pin.
    pub fn attributes(&self) -> Vec<(String, String)> {
        vec![
            (PIN_CHAIN_HEAD_KEY.to_string(), self.chain_head.clone()),
            (PIN_RECORDS_KEY.to_string(), self.records.to_string()),
            (PIN_SIGNER_KEY.to_string(), self.signer_cert_sha256.clone()),
        ]
    }

    /// Read the pin back from a board instance's `Trajectory.provenance.attributes`.
    pub fn from_attributes(attributes: &BTreeMap<String, String>) -> Result<Self, BoardReplayRefusal> {
        let get = |key: &str| attributes.get(key).cloned().ok_or_else(|| BoardReplayRefusal::PinUnusable { detail: format!("the instance's provenance has no {key:?} attribute (was the run board-bound, and did it finish?)") });
        let records_text = get(PIN_RECORDS_KEY)?;
        let records = records_text.parse::<u64>().map_err(|e| BoardReplayRefusal::PinUnusable { detail: format!("{PIN_RECORDS_KEY} {records_text:?} is not a record count: {e}") })?;
        Ok(Self { chain_head: get(PIN_CHAIN_HEAD_KEY)?, records, signer_cert_sha256: get(PIN_SIGNER_KEY)? })
    }

    /// The pin a live run's products carry for `instance`: from `RunProducts.trajectories` (the
    /// kernel's [`super::RunProducts`] or the wire `altavista.v1.RunProducts`, which share the map type).
    pub fn from_trajectories(trajectories: &BTreeMap<String, pb::Trajectory>, instance: &str) -> Result<Self, BoardReplayRefusal> {
        let trajectory = trajectories.get(instance).ok_or_else(|| BoardReplayRefusal::PinUnusable { detail: format!("the run's products have no trajectory for instance {instance:?}") })?;
        let provenance = trajectory.provenance.as_ref().ok_or_else(|| BoardReplayRefusal::PinUnusable { detail: format!("the trajectory of instance {instance:?} has no provenance") })?;
        Self::from_attributes(&provenance.attributes)
    }

    /// The pin a *verified* log implies: the position and hash of the record before a trailing
    /// SHUTDOWN. `None` for a log with nothing before its end. Used by tests that build a log
    /// deliberately (a perturbed one) and want the pin a caller would then have to accept.
    pub fn of_log(log: &VerifiedLog) -> Option<Self> {
        let mut n = log.records.len();
        if log.records.last().is_some_and(|r| r.kind == BoardIoKind::Shutdown as i32) {
            n -= 1;
        }
        let first = log.records.first()?;
        let last = log.records.get(n.checked_sub(1)?)?;
        Some(Self { chain_head: hex_encode(&last.record_hash), records: n as u64, signer_cert_sha256: first.signer_cert_sha256.clone() })
    }
}

/// One board instance to replay from its edge log (`execute_with_board_replay`).
#[derive(Debug, Clone)]
pub struct BoardLogReplay {
    /// The board instance (`BINDING_KIND_BOARD`) the log substitutes for.
    pub instance: String,
    /// The edge service's log file (`av-edge-board --io-log`).
    pub log_path: PathBuf,
    /// The PEM X.509 certificate of the log's signer. A bare public key is refused: the pin
    /// names the signer by certificate fingerprint.
    pub certificate_pem: Vec<u8>,
    /// What the live run pinned ([`BoardLogPin::from_trajectories`] on its products).
    pub expected: BoardLogPin,
}

/// Why a board could not be replayed from its edge log. Each is raised before anything binds
/// except the two documented as run-time.
#[derive(Debug)]
pub enum BoardReplayRefusal {
    /// The board instance is in the replay's default set (no instance named) but no log was
    /// supplied for it. A board is replayed from its signed edge log by default and is never
    /// dialled; to replay it from `port_traffic.pb` instead, name it in `ReplayConfig.instances`.
    NeedsEdgeLog,
    /// A log was supplied for an instance that is not `BINDING_KIND_BOARD`.
    NotABoardInstance,
    /// The log file could not be read.
    LogUnreadable { path: PathBuf, detail: String },
    /// The supplied certificate is not usable (not a P-384 X.509 certificate, or a bare key).
    CertificateUnusable { detail: String },
    /// The pin read from the run's products is missing or malformed.
    PinUnusable { detail: String },
    /// The supplied certificate is not the one the run pinned as the log's signer.
    CertificateNotThePinned { pinned: String, supplied: String },
    /// The log fails verification (a broken chain, a wrong signature, a tampered record, a
    /// record signed by another certificate, a gap): `source` names the record and the defect.
    LogDefect { source: BoardLogError },
    /// The log's last frame is physically incomplete (a crash between write and `fsync`, or a
    /// cut): not a log of a completed run.
    TornTail { offset: u64, discarded_bytes: u64, detail: String },
    /// The log has fewer records than the run pinned: it was cut short.
    Truncated { pinned_records: u64, log_records: u64 },
    /// The log has the pinned number of records but not the closing SHUTDOWN record every
    /// completed run's log ends with.
    ShutdownMissing { pinned_records: u64 },
    /// The record at the pinned position does not hash to the pinned head: this is not the log
    /// the run pinned (rewritten history, even if re-signed with the genuine key).
    ChainHeadMismatch { pinned: String, log: String, at_record: u64 },
    /// More than one record, or something other than SHUTDOWN, follows the pinned position.
    UnpinnedTail { pinned_records: u64, log_records: u64, detail: String },
    /// A record names another run or another instance than the one being replayed.
    WrongRun { record: u64, field: &'static str, expected: String, got: String },
    /// A record is of a failed exchange: the live run it belongs to did not complete.
    FailedExchange { record: u64, error: String },
    /// The log is well-formed, signed and pinned, but is not the log of the run being replayed
    /// (a different step grid, a different Bind, a power cycle the DRM does not declare, ...).
    DoesNotMatchRun { detail: String },
    /// Run time: the DRM declares a `power_cycle` fault on the board that fires in the replayed
    /// span, and the log has no `POWER_CYCLE` record for it.
    PowerCycleNotInLog { fault_id: String },
}

impl std::fmt::Display for BoardReplayRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BoardReplayRefusal::NeedsEdgeLog => write!(
                f,
                "a board instance is replayed from its signed edge I/O log and is never dialled, but no log was supplied for it (supply one with BoardLogReplay, or `av-run --replay-board-log`; to replay it from the run's port_traffic.pb instead, name it in ReplayConfig.instances)"
            ),
            BoardReplayRefusal::NotABoardInstance => write!(f, "an edge I/O log was supplied for an instance that is not BINDING_KIND_BOARD"),
            BoardReplayRefusal::LogUnreadable { path, detail } => write!(f, "the edge I/O log {} could not be read: {detail}", path.display()),
            BoardReplayRefusal::CertificateUnusable { detail } => write!(f, "the log signer's certificate is not usable: {detail}"),
            BoardReplayRefusal::PinUnusable { detail } => write!(f, "the run's pinned log head is not usable: {detail}"),
            BoardReplayRefusal::CertificateNotThePinned { pinned, supplied } => write!(f, "the supplied certificate (SHA-256 {supplied}) is not the log signer the run pinned ({pinned})"),
            BoardReplayRefusal::LogDefect { source } => write!(f, "the edge I/O log does not verify: {source}"),
            BoardReplayRefusal::TornTail { offset, discarded_bytes, detail } => write!(f, "the edge I/O log ends in a torn frame at byte {offset} ({discarded_bytes} byte(s), {detail}): it is not the log of a completed run"),
            BoardReplayRefusal::Truncated { pinned_records, log_records } => write!(f, "the edge I/O log has {log_records} record(s) but the run pinned {pinned_records}: it was cut short (a hash chain cannot show it; the pin does)"),
            BoardReplayRefusal::ShutdownMissing { pinned_records } => write!(f, "the edge I/O log has the pinned {pinned_records} record(s) but not the SHUTDOWN record every completed run's log ends with: it was cut at its last record"),
            BoardReplayRefusal::ChainHeadMismatch { pinned, log, at_record } => write!(f, "the edge I/O log's record {at_record} hashes to {log} but the run pinned chain head {pinned}: this is not the log that run produced"),
            BoardReplayRefusal::UnpinnedTail { pinned_records, log_records, detail } => write!(f, "the edge I/O log has {log_records} record(s) where the run pinned {pinned_records} followed by one SHUTDOWN: {detail}"),
            BoardReplayRefusal::WrongRun { record, field, expected, got } => write!(f, "edge I/O log record {record} has {field} {got:?} but the run being replayed has {expected:?}"),
            BoardReplayRefusal::FailedExchange { record, error } => write!(f, "edge I/O log record {record} is of a failed exchange ({error}): the live run it belongs to did not complete"),
            BoardReplayRefusal::DoesNotMatchRun { detail } => write!(f, "the edge I/O log is not the log of this run: {detail}"),
            BoardReplayRefusal::PowerCycleNotInLog { fault_id } => write!(f, "the DRM's power_cycle fault {fault_id:?} fires on the board in the replayed span, but the edge I/O log has no POWER_CYCLE record for it"),
        }
    }
}
impl std::error::Error for BoardReplayRefusal {}

/// A log that passed verification, the pin and the run/instance check: records only, not yet
/// compared with the run's shape.
pub(crate) struct LoadedBoardLog {
    records: Vec<BoardIoRecord>,
    pin: BoardLogPin,
}

/// Read, verify and pin-check `replay.log_path` for `run_id`. Touches nothing but the file.
pub(crate) fn load(replay: &BoardLogReplay, run_id: &str) -> Result<LoadedBoardLog, BoardReplayRefusal> {
    let pin = &replay.expected;
    if pin.records == 0 {
        return Err(BoardReplayRefusal::PinUnusable { detail: "the pinned record count is 0".to_string() });
    }
    let verifier = LogVerifier::from_pem(&replay.certificate_pem).map_err(|e| BoardReplayRefusal::CertificateUnusable { detail: e.to_string() })?;
    let Some(supplied) = verifier.cert_sha256().map(str::to_string) else {
        return Err(BoardReplayRefusal::CertificateUnusable { detail: "this is a bare public key, not a certificate: the run pins the log's signer by certificate fingerprint".to_string() });
    };
    if supplied != pin.signer_cert_sha256 {
        return Err(BoardReplayRefusal::CertificateNotThePinned { pinned: pin.signer_cert_sha256.clone(), supplied });
    }
    let bytes = std::fs::read(&replay.log_path).map_err(|e| BoardReplayRefusal::LogUnreadable { path: replay.log_path.clone(), detail: e.to_string() })?;
    let verified = verify_bytes(&bytes, &verifier).map_err(|source| BoardReplayRefusal::LogDefect { source })?;
    if let Some(torn) = &verified.recovery {
        return Err(BoardReplayRefusal::TornTail { offset: torn.offset, discarded_bytes: torn.discarded_bytes, detail: format!("{:?}", torn.kind) });
    }
    let have = verified.records.len() as u64;
    if have < pin.records {
        return Err(BoardReplayRefusal::Truncated { pinned_records: pin.records, log_records: have });
    }
    let at = &verified.records[(pin.records - 1) as usize];
    let at_hex = hex_encode(&at.record_hash);
    if at_hex != pin.chain_head {
        return Err(BoardReplayRefusal::ChainHeadMismatch { pinned: pin.chain_head.clone(), log: at_hex, at_record: pin.records });
    }
    match &verified.records[pin.records as usize..] {
        [] => return Err(BoardReplayRefusal::ShutdownMissing { pinned_records: pin.records }),
        [last] if last.kind == BoardIoKind::Shutdown as i32 => {}
        tail => {
            return Err(BoardReplayRefusal::UnpinnedTail {
                pinned_records: pin.records,
                log_records: have,
                detail: format!("{} record(s) follow the pinned position, of kind(s) {:?}; exactly one SHUTDOWN is expected", tail.len(), tail.iter().map(|r| kind_name(r.kind)).collect::<Vec<_>>()),
            })
        }
    }
    for (i, r) in verified.records.iter().enumerate() {
        let index = i as u64 + 1;
        if r.run_id != run_id {
            return Err(BoardReplayRefusal::WrongRun { record: index, field: "run_id", expected: run_id.to_string(), got: r.run_id.clone() });
        }
        if r.instance != replay.instance {
            return Err(BoardReplayRefusal::WrongRun { record: index, field: "instance", expected: replay.instance.clone(), got: r.instance.clone() });
        }
        if !r.error.is_empty() {
            return Err(BoardReplayRefusal::FailedExchange { record: index, error: r.error.clone() });
        }
    }
    Ok(LoadedBoardLog { records: verified.records, pin: pin.clone() })
}

fn kind_name(kind: i32) -> String {
    BoardIoKind::try_from(kind).map(|k| k.as_str_name().to_string()).unwrap_or_else(|_| format!("unknown({kind})"))
}

/// What the replayed run looks like from the board instance's side: the shape the log must have.
#[derive(Debug, Clone)]
pub(crate) struct RunShape {
    pub start_tai_ns: i64,
    pub end_tai_ns: i64,
    pub period_ns: i64,
    /// `Scenario.seeds[board.seed_key]` when the seed is declared.
    pub seed: Option<u64>,
    /// The instance's declared ports (`LockstepBindRequest.ports`).
    pub ports: Vec<pb::Port>,
    /// The `LockstepBindRequest.parameters` the kernel sends for this instance, as the edge
    /// service forwards them (the `board.*` link parameters already stripped).
    pub bind_parameters: BTreeMap<String, String>,
    /// The `HARDWARE`/`power_cycle` faults the DRM declares on this instance: fault id -> epoch.
    pub power_cycles: BTreeMap<String, i64>,
}

/// One scripted STEP: what the board answered at one step.
#[derive(Debug, Clone)]
pub struct ScriptedStep {
    pub outputs: Vec<pb::PortMessage>,
    pub named_outputs: BTreeMap<String, f64>,
}

/// A logged `POWER_CYCLE` record: the edge node's start and finish instants (Unix ns).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoggedPowerCycle {
    pub tai_ns: i64,
    pub started_unix_ns: i64,
    pub finished_unix_ns: i64,
}

/// What the replay model plays, built from a verified, pinned log that matches the run's shape.
#[derive(Debug)]
pub struct BoardReplayScript {
    pub instance: String,
    /// `LockstepBindResponse.version` the flight software answered the BIND with.
    pub bind_version: String,
    /// `LockstepBindResponse.binding_hash`.
    pub binding_hash: String,
    /// STEP records keyed by `until_tai_ns`: exactly one per step of the run.
    pub steps: BTreeMap<i64, ScriptedStep>,
    /// `POWER_CYCLE` records keyed by fault id.
    pub power_cycles: BTreeMap<String, LoggedPowerCycle>,
    /// The verified pin, re-stamped on the replayed instance's provenance.
    pub pin: BoardLogPin,
}

impl BoardReplayScript {
    /// A script for unit tests of the model: steps given directly.
    #[cfg(test)]
    pub(crate) fn for_test(instance: &str, steps: BTreeMap<i64, ScriptedStep>) -> Self {
        Self { instance: instance.to_string(), bind_version: "v".to_string(), binding_hash: "h".to_string(), steps, power_cycles: BTreeMap::new(), pin: BoardLogPin { chain_head: String::new(), records: 0, signer_cert_sha256: String::new() } }
    }
}

fn mismatch(detail: impl Into<String>) -> BoardReplayRefusal {
    BoardReplayRefusal::DoesNotMatchRun { detail: detail.into() }
}

impl LoadedBoardLog {
    /// Compare the log with the run's shape and build the script. The records are, in order: the
    /// BIND, then for each step `start + period`, ..., `end` one STEP (and, right after the STEP
    /// at a power-cycle fault's epoch, that fault's `POWER_CYCLE` and `RESET`), then the closing
    /// SHUTDOWN.
    pub(crate) fn into_script(self, instance: &str, shape: &RunShape) -> Result<Arc<BoardReplayScript>, BoardReplayRefusal> {
        let pinned = self.pin.records as usize;
        let body = &self.records[..pinned];
        let first = &body[0];
        if first.kind != BoardIoKind::Bind as i32 {
            return Err(mismatch(format!("the first record is {}, not the BIND", kind_name(first.kind))));
        }
        let (Some(bind_request), Some(bind_response)) = (&first.bind_request, &first.bind_response) else {
            return Err(mismatch("the BIND record lacks its request or its response"));
        };
        if !bind_response.lockstep_capable {
            return Err(mismatch("the BIND record's response refused the bind (lockstep_capable is false)"));
        }
        if bind_request.start_tai_ns != shape.start_tai_ns {
            return Err(mismatch(format!("the BIND started at {} tai_ns but the replayed scenario starts at {}", bind_request.start_tai_ns, shape.start_tai_ns)));
        }
        if bind_request.step_period_ns != shape.period_ns || bind_request.base_period_ns != shape.period_ns {
            return Err(mismatch(format!("the BIND's step period is {} ns (base {} ns) but the instance steps every {} ns", bind_request.step_period_ns, bind_request.base_period_ns, shape.period_ns)));
        }
        if let Some(seed) = shape.seed {
            if bind_request.seed != seed {
                return Err(mismatch(format!("the BIND's seed is {} but the scenario's is {seed}", bind_request.seed)));
            }
        }
        if bind_request.ports != shape.ports {
            return Err(mismatch("the BIND's declared ports differ from the instance's system definition"));
        }
        let logged_parameters: BTreeMap<String, String> = bind_request.parameters.clone().into_iter().collect();
        if logged_parameters != shape.bind_parameters {
            return Err(mismatch(format!("the BIND's parameters {logged_parameters:?} differ from the instance's declared parameters {:?}", shape.bind_parameters)));
        }

        let mut steps: BTreeMap<i64, ScriptedStep> = BTreeMap::new();
        let mut power_cycles: BTreeMap<String, LoggedPowerCycle> = BTreeMap::new();
        let mut next_until = shape.start_tai_ns + shape.period_ns;
        let mut last_until = shape.start_tai_ns;
        let mut next_sequence = 1u64;
        let mut i = 1usize;
        while i < body.len() {
            let r = &body[i];
            let index = i as u64 + 1;
            match BoardIoKind::try_from(r.kind).unwrap_or(BoardIoKind::Unspecified) {
                BoardIoKind::Step => {
                    if r.lockstep_sequence != next_sequence {
                        return Err(mismatch(format!("record {index}: STEP sequence {} where {next_sequence} was expected", r.lockstep_sequence)));
                    }
                    if r.until_tai_ns != next_until {
                        return Err(mismatch(format!("record {index}: STEP until_tai_ns {} where the replayed run's step ends at {next_until}", r.until_tai_ns)));
                    }
                    steps.insert(r.until_tai_ns, ScriptedStep { outputs: r.outputs.clone(), named_outputs: r.named_outputs.clone().into_iter().collect() });
                    next_sequence += 1;
                    last_until = next_until;
                    next_until += shape.period_ns;
                    i += 1;
                }
                BoardIoKind::PowerCycle => {
                    let Some(fault_id) = r.reset_reason.strip_prefix("fault:") else {
                        return Err(mismatch(format!("record {index}: POWER_CYCLE reason {:?} is not \"fault:<id>\"", r.reset_reason)));
                    };
                    match shape.power_cycles.get(fault_id) {
                        None => return Err(mismatch(format!("record {index}: the log records a power cycle for fault {fault_id:?}, which the DRM does not declare as a power_cycle fault on this instance"))),
                        Some(tai) if *tai != r.reset_tai_ns => return Err(mismatch(format!("record {index}: the power cycle for fault {fault_id:?} is at {} tai_ns but the DRM declares it at {tai}", r.reset_tai_ns))),
                        Some(_) => {}
                    }
                    if r.reset_tai_ns != last_until {
                        return Err(mismatch(format!("record {index}: the power cycle at {} tai_ns does not follow the step that ends there (the last step ended at {last_until})", r.reset_tai_ns)));
                    }
                    let Some(reset) = body.get(i + 1) else {
                        return Err(mismatch(format!("record {index}: the POWER_CYCLE for fault {fault_id:?} is not followed by its RESET")));
                    };
                    if reset.kind != BoardIoKind::Reset as i32 || reset.reset_tai_ns != r.reset_tai_ns || reset.reset_reason != r.reset_reason {
                        return Err(mismatch(format!("record {}: expected the RESET for fault {fault_id:?} at {} tai_ns right after its POWER_CYCLE", index + 1, r.reset_tai_ns)));
                    }
                    if reset.lockstep_sequence != next_sequence {
                        return Err(mismatch(format!("record {}: RESET sequence {} where {next_sequence} was expected", index + 1, reset.lockstep_sequence)));
                    }
                    if power_cycles.insert(fault_id.to_string(), LoggedPowerCycle { tai_ns: r.reset_tai_ns, started_unix_ns: r.request_written_unix_ns, finished_unix_ns: r.response_read_unix_ns }).is_some() {
                        return Err(mismatch(format!("record {index}: a second power cycle for fault {fault_id:?}")));
                    }
                    next_sequence += 1;
                    i += 2;
                }
                other => return Err(mismatch(format!("record {index}: unexpected {} record between the BIND and the end of the run's I/O", kind_name(other as i32)))),
            }
        }
        if next_until != shape.end_tai_ns + shape.period_ns {
            return Err(mismatch(format!("the log's STEP records end at {last_until} tai_ns but the replayed run ends at {}", shape.end_tai_ns)));
        }
        Ok(Arc::new(BoardReplayScript {
            instance: instance.to_string(),
            bind_version: bind_response.version.clone(),
            binding_hash: bind_response.binding_hash.clone(),
            steps,
            power_cycles,
            pin: self.pin,
        }))
    }
}

/// One OUT frame as the kernel records it: the port, the epoch it is recorded at, the payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutFrame {
    pub port: String,
    pub tai_ns: i64,
    pub payload: Vec<u8>,
}

/// The mapping of the module doc, stated independently of the replay model: every STEP record's
/// `outputs`, in log order, with the epoch the router records each at (the message's own
/// `tai_ns`, or the STEP's `until_tai_ns` when it carries none). The caller filters to the
/// instance's FRAMED/BYTE_STREAM ports to compare with `port_traffic.pb`.
pub fn out_frames_of_log(records: &[BoardIoRecord]) -> Vec<OutFrame> {
    let mut out = Vec::new();
    for r in records.iter().filter(|r| r.kind == BoardIoKind::Step as i32) {
        for m in &r.outputs {
            out.push(OutFrame { port: m.port.clone(), tai_ns: if m.tai_ns == NO_MESSAGE_EPOCH { r.until_tai_ns } else { m.tai_ns }, payload: m.payload.clone() });
        }
    }
    out
}

/// Ask the board's edge service where its log ends (`BoardEdgeService.BoardIoLogHead`) and
/// return the pin, with a deadline of `timeout`. Dials afresh, like a power cycle, over the
/// same address and TLS rules as the board's lockstep link.
pub(crate) fn fetch_pin(spec: &ContainerSpec, run_id: &str, instance: &str, timeout: Duration) -> Result<BoardLogPin, DrmError> {
    let fail = |detail: String| DrmError::BoardLogPin { instance: instance.to_string(), detail };
    let address = spec.address.clone();
    let tls = spec.tls_paths().map(|(ca, cert, key)| (ca.to_string(), cert.to_string(), key.to_string()));
    let request = BoardIoLogHeadRequest { run_id: run_id.to_string(), instance: instance.to_string() };
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("av-board-log-head".to_string())
        .spawn(move || {
            let result = (|| {
                let mut client = match &tls {
                    Some((ca, cert, key)) => BlockingBoardEdgeClient::connect_mtls(&format!("https://{address}"), std::path::Path::new(ca), Some(std::path::Path::new(cert)), Some(std::path::Path::new(key))),
                    None => BlockingBoardEdgeClient::connect_plaintext(&address),
                }
                .map_err(|e| format!("connecting to {address:?}: {e}"))?;
                client.board_io_log_head(request).map_err(|status| format!("{:?}: {}", status.code(), status.message()))
            })();
            let _ = tx.send(result);
        })
        .expect("spawning the board log head thread");
    let response = match rx.recv_timeout(timeout) {
        Ok(Ok(response)) => response,
        Ok(Err(detail)) => return Err(fail(format!("the BoardIoLogHead RPC failed: {detail}"))),
        Err(_) => return Err(fail(format!("no BoardIoLogHead reply within {} ms (board.step_timeout_ms)", timeout.as_millis()))),
    };
    if response.records == 0 {
        return Err(fail("the edge service reports an empty board I/O log at the end of a run".to_string()));
    }
    if response.chain_head.len() != 32 {
        return Err(fail(format!("the chain head is {} bytes, not a SHA-256 record hash", response.chain_head.len())));
    }
    if response.signer_cert_sha256.is_empty() {
        return Err(fail("the edge service named no log signer".to_string()));
    }
    Ok(BoardLogPin { chain_head: hex_encode(&response.chain_head), records: response.records, signer_cert_sha256: response.signer_cert_sha256 })
}

/// Which source a board was replayed from, for [`strip_replay_exclusions`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplaySource {
    /// The board's signed edge log (`BoardLogReplay`).
    EdgeLog,
    /// The run's `port_traffic.pb`, as a container instance is replayed.
    PortTraffic,
}

/// Remove from `products` exactly the fields [`crate::pacing::WALL_CLOCK_DEPENDENT`] names, so
/// that a live board-bound run and its replay can be compared with `==`. Applies to either side
/// (both are stripped). `board_instance` is the replayed board; `source` is where it was
/// replayed from (see the module doc, "Comparing a replay with its live run").
///
/// Applied to both: `RunProducts.pacing`; every pacing-overrun event; each power-cycle outcome
/// event's `detail` and `values["duration_ns"]`. For [`ReplaySource::PortTraffic`] additionally:
/// the power-cycle outcome events themselves (nothing in `port_traffic.pb` records them), and the
/// board trajectory's segment `dynamics_model` / `dynamics_hash` / `dynamics_depth` and its
/// provenance attributes `binding_kind`, `board_binding_hash`, `board_link_hash` and the three
/// `board_io_log_*` pin attributes.
pub fn strip_replay_exclusions(products: &mut pb::RunProducts, board_instance: &str, source: ReplaySource) {
    products.pacing = None;
    products.events.retain(|e| !crate::pacing::is_overrun_event(e));
    if source == ReplaySource::PortTraffic {
        // The outcome events go whole, and with them their entry in the board's
        // `Trajectory.event_ids` (an instance's trajectory lists the events tied to it).
        products.events.retain(|e| !is_power_cycle_event(e));
        for t in products.trajectories.values_mut() {
            t.event_ids.retain(|id| !id.starts_with(POWER_CYCLE_EVENT_ID_PREFIX));
        }
    }
    for e in products.events.iter_mut().filter(|e| e.id.starts_with(POWER_CYCLE_EVENT_ID_PREFIX)) {
        e.detail.clear();
        e.values.remove("duration_ns");
    }
    if source == ReplaySource::PortTraffic {
        if let Some(t) = products.trajectories.get_mut(board_instance) {
            for s in t.segments.iter_mut() {
                s.dynamics_model.clear();
                s.dynamics_hash.clear();
                s.dynamics_depth.clear();
            }
            if let Some(p) = t.provenance.as_mut() {
                for key in ["binding_kind", "board_binding_hash", "board_link_hash", PIN_CHAIN_HEAD_KEY, PIN_RECORDS_KEY, PIN_SIGNER_KEY] {
                    p.attributes.remove(key);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Mutex;

    use av_edge::board_log::{encode_frame, seal, BoardIoLogWriter, DurableSink, LogSigner};
    use openssl::asn1::Asn1Time;
    use openssl::bn::BigNum;
    use openssl::ec::{EcGroup, EcKey};
    use openssl::hash::MessageDigest;
    use openssl::nid::Nid;
    use openssl::pkey::PKey;
    use openssl::x509::{X509Builder, X509NameBuilder};

    use super::*;

    const RUN: &str = "run-1";
    const INSTANCE: &str = "obc";
    const START: i64 = 1_000_000_000_000;
    const PERIOD: i64 = 100_000_000;
    type Mutation = Box<dyn Fn(&mut RunShape)>;

    #[derive(Clone, Default)]
    struct Mem(Arc<Mutex<Vec<u8>>>);
    impl DurableSink for Mem {
        fn write_all(&mut self, bytes: &[u8]) -> std::io::Result<()> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(())
        }
        fn sync(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// (key PEM, certificate PEM, bare public key PEM) of a fresh P-384 identity.
    fn identity(cn: &str) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let key = EcKey::generate(&EcGroup::from_curve_name(Nid::SECP384R1).unwrap()).unwrap();
        let pkey = PKey::from_ec_key(key.clone()).unwrap();
        let mut name = X509NameBuilder::new().unwrap();
        name.append_entry_by_nid(Nid::COMMONNAME, cn).unwrap();
        let name = name.build();
        let mut b = X509Builder::new().unwrap();
        b.set_version(2).unwrap();
        b.set_serial_number(&BigNum::from_u32(1).unwrap().to_asn1_integer().unwrap()).unwrap();
        b.set_subject_name(&name).unwrap();
        b.set_issuer_name(&name).unwrap();
        b.set_pubkey(&pkey).unwrap();
        b.set_not_before(&Asn1Time::days_from_now(0).unwrap()).unwrap();
        b.set_not_after(&Asn1Time::days_from_now(30).unwrap()).unwrap();
        b.sign(&pkey, MessageDigest::sha384()).unwrap();
        (key.private_key_to_pem().unwrap(), b.build().to_pem().unwrap(), pkey.public_key_to_pem().unwrap())
    }

    struct Fixture {
        records: Vec<BoardIoRecord>,
        bytes: Vec<u8>,
        key: Vec<u8>,
        cert: Vec<u8>,
        pin: BoardLogPin,
        dir: PathBuf,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn bind() -> BoardIoRecord {
        BoardIoRecord {
            kind: BoardIoKind::Bind as i32,
            run_id: RUN.into(),
            instance: INSTANCE.into(),
            bind_request: Some(pb::LockstepBindRequest { run_id: RUN.into(), instance: INSTANCE.into(), start_tai_ns: START, base_period_ns: PERIOD, step_period_ns: PERIOD, seed: 42, parameters: [("gain".to_string(), "2.5".to_string())].into(), ..Default::default() }),
            bind_response: Some(pb::LockstepBindResponse { lockstep_capable: true, binding_hash: "bh".into(), version: "v1".into(), refusal_reason: String::new() }),
            ..Default::default()
        }
    }

    fn step(sequence: u64, until: i64) -> BoardIoRecord {
        // Even sequences carry no epoch of their own (0), odd ones a sub-step epoch.
        let tai_ns = if sequence.is_multiple_of(2) { 0 } else { until - 1_000 };
        BoardIoRecord {
            kind: BoardIoKind::Step as i32,
            run_id: RUN.into(),
            instance: INSTANCE.into(),
            lockstep_sequence: sequence,
            until_tai_ns: until,
            outputs: vec![pb::PortMessage { port: "out".into(), tai_ns, payload: vec![sequence as u8, 7] }],
            named_outputs: [("n".to_string(), sequence as f64)].into(),
            ..Default::default()
        }
    }

    /// BIND, `steps` STEPs, a power cycle for fault `pc` after the step ending at `after_step`
    /// (the POWER_CYCLE and its RESET), then SHUTDOWN. Every record signed and chained.
    fn fixture(tag: &str, steps: u64, pc: Option<(&str, u64)>) -> Fixture {
        let (key, cert, _) = identity("board-replay-unit");
        let mem = Mem::default();
        let mut writer = BoardIoLogWriter::with_sink(Path::new("mem"), Box::new(mem.clone()), "edge-1", "00", LogSigner::from_pem(&key, &cert).unwrap());
        writer.append(bind()).unwrap();
        let mut sequence = 1;
        for k in 1..=steps {
            writer.append(step(sequence, START + k as i64 * PERIOD)).unwrap();
            sequence += 1;
            if let Some((fault, after)) = pc {
                if after == k {
                    let tai = START + k as i64 * PERIOD;
                    writer
                        .append(BoardIoRecord {
                            kind: BoardIoKind::PowerCycle as i32,
                            run_id: RUN.into(),
                            instance: INSTANCE.into(),
                            reset_tai_ns: tai,
                            reset_reason: format!("fault:{fault}"),
                            request_written_unix_ns: 1_000,
                            response_read_unix_ns: 1_500,
                            ..Default::default()
                        })
                        .unwrap();
                    writer.append(BoardIoRecord { kind: BoardIoKind::Reset as i32, run_id: RUN.into(), instance: INSTANCE.into(), lockstep_sequence: sequence, reset_tai_ns: tai, reset_reason: format!("fault:{fault}"), ..Default::default() }).unwrap();
                    sequence += 1;
                }
            }
        }
        writer.append(BoardIoRecord { kind: BoardIoKind::Shutdown as i32, run_id: RUN.into(), instance: INSTANCE.into(), ..Default::default() }).unwrap();
        let bytes = mem.0.lock().unwrap().clone();
        let verified = verify_bytes(&bytes, &LogVerifier::from_pem(&cert).unwrap()).unwrap();
        let pin = BoardLogPin::of_log(&verified).unwrap();
        let dir = std::env::temp_dir().join(format!("av-kernel-board-replay-unit-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Fixture { records: verified.records, bytes, key, cert, pin, dir }
    }

    impl Fixture {
        fn replay_of(&self, bytes: &[u8]) -> BoardLogReplay {
            let log_path = self.dir.join("io.log");
            std::fs::write(&log_path, bytes).unwrap();
            BoardLogReplay { instance: INSTANCE.into(), log_path, certificate_pem: self.cert.clone(), expected: self.pin.clone() }
        }
        fn load(&self, bytes: &[u8]) -> Result<LoadedBoardLog, BoardReplayRefusal> {
            load(&self.replay_of(bytes), RUN)
        }
        /// The records re-chained and re-signed from `from` on, as the genuine key holder would.
        fn resealed(&self, mut records: Vec<BoardIoRecord>, from: usize) -> Vec<u8> {
            let signer = LogSigner::from_pem(&self.key, &self.cert).unwrap();
            for i in from..records.len() {
                records[i].sequence = i as u64 + 1;
                records[i].prev_hash = if i == 0 { av_edge::hash::GENESIS.to_vec() } else { records[i - 1].record_hash.clone() };
                seal(&mut records[i], &signer).unwrap();
            }
            records.iter().flat_map(|r| encode_frame(r).unwrap()).collect()
        }
        fn frame_ends(&self) -> Vec<usize> {
            let mut ends = vec![];
            let mut o = 0;
            while o < self.bytes.len() {
                let len = u32::from_le_bytes(self.bytes[o..o + 4].try_into().unwrap()) as usize;
                o += av_edge::board_log::HEADER_LEN + len;
                ends.push(o);
            }
            ends
        }
    }

    fn shape(steps: u64) -> RunShape {
        RunShape {
            start_tai_ns: START,
            end_tai_ns: START + steps as i64 * PERIOD,
            period_ns: PERIOD,
            seed: Some(42),
            ports: vec![],
            bind_parameters: [("gain".to_string(), "2.5".to_string())].into(),
            power_cycles: BTreeMap::new(),
        }
    }

    fn refusal<T>(r: Result<T, BoardReplayRefusal>) -> BoardReplayRefusal {
        match r {
            Err(e) => {
                println!("{e}");
                e
            }
            Ok(_) => panic!("expected a refusal"),
        }
    }

    #[test]
    fn a_good_log_loads_and_becomes_a_script_keyed_by_step() {
        let f = fixture("good", 10, Some(("pc", 4)));
        let mut sh = shape(10);
        sh.power_cycles.insert("pc".into(), START + 4 * PERIOD);
        let script = f.load(&f.bytes).unwrap().into_script(INSTANCE, &sh).unwrap();
        assert_eq!(script.steps.keys().copied().collect::<Vec<_>>(), (1..=10).map(|k| START + k * PERIOD).collect::<Vec<_>>());
        assert_eq!((script.bind_version.as_str(), script.binding_hash.as_str()), ("v1", "bh"));
        assert_eq!(script.power_cycles["pc"], LoggedPowerCycle { tai_ns: START + 4 * PERIOD, started_unix_ns: 1_000, finished_unix_ns: 1_500 });
        assert_eq!(script.steps[&(START + 3 * PERIOD)].named_outputs["n"], 3.0);
        // The pin counts every record before the SHUTDOWN: BIND + 10 STEPs + POWER_CYCLE + RESET.
        assert_eq!(f.pin.records, 13);
        assert_eq!(f.pin.chain_head, hex_encode(&f.records[12].record_hash));
    }

    /// The mapping of STEP outputs to recorded epochs: the message's own `tai_ns`, or the STEP's
    /// end when it carries none.
    #[test]
    fn out_frames_use_the_messages_own_epoch_or_the_steps_end() {
        let f = fixture("frames", 4, None);
        let frames = out_frames_of_log(&f.records);
        assert_eq!(frames.len(), 4);
        for (i, frame) in frames.iter().enumerate() {
            let sequence = i as u64 + 1;
            let until = START + sequence as i64 * PERIOD;
            assert_eq!(frame.tai_ns, if sequence.is_multiple_of(2) { until } else { until - 1_000 }, "sequence {sequence}");
            assert_eq!((frame.port.as_str(), frame.payload.as_slice()), ("out", [sequence as u8, 7].as_slice()));
        }
    }

    #[test]
    fn another_runs_or_instances_log_is_refused() {
        let f = fixture("run", 4, None);
        let mut other_run = f.replay_of(&f.bytes);
        let r = refusal(load(&other_run, "run-2"));
        assert!(matches!(r, BoardReplayRefusal::WrongRun { record: 1, field: "run_id", .. }), "{r:?}");
        other_run.instance = "other".into();
        let r = refusal(load(&other_run, RUN));
        assert!(matches!(r, BoardReplayRefusal::WrongRun { record: 1, field: "instance", .. }), "{r:?}");
    }

    #[test]
    fn a_tampered_record_is_a_typed_defect_naming_it() {
        let f = fixture("tamper", 4, None);
        let mut bad = f.bytes.clone();
        let ends = f.frame_ends();
        // A byte inside the third record's payload (past its frame header).
        bad[ends[1] + av_edge::board_log::HEADER_LEN + 12] ^= 0x01;
        let r = refusal(f.load(&bad));
        assert!(matches!(r, BoardReplayRefusal::LogDefect { .. }), "{r:?}");
    }

    #[test]
    fn a_torn_tail_is_refused() {
        let f = fixture("torn", 4, None);
        let r = refusal(f.load(&f.bytes[..f.bytes.len() - 3]));
        assert!(matches!(r, BoardReplayRefusal::TornTail { .. }), "{r:?}");
        let ends = f.frame_ends();
        let r = refusal(f.load(&f.bytes[..ends[ends.len() - 2] + 10]));
        assert!(matches!(r, BoardReplayRefusal::TornTail { .. }), "an incomplete header too: {r:?}");
    }

    #[test]
    fn an_unknown_certificate_and_a_bare_key_are_refused() {
        let f = fixture("cert", 4, None);
        let (_, other_cert, other_pub) = identity("someone-else");
        let mut replay = f.replay_of(&f.bytes);
        replay.certificate_pem = other_cert;
        let r = refusal(load(&replay, RUN));
        assert!(matches!(r, BoardReplayRefusal::CertificateNotThePinned { .. }), "{r:?}");
        replay.certificate_pem = other_pub;
        let r = refusal(load(&replay, RUN));
        assert!(matches!(r, BoardReplayRefusal::CertificateUnusable { .. }), "{r:?}");
        replay.certificate_pem = b"not pem".to_vec();
        assert!(matches!(refusal(load(&replay, RUN)), BoardReplayRefusal::CertificateUnusable { .. }));
        // A log signed by another identity that stamps the pinned fingerprint fails its signatures.
        let (k2, c2, _) = identity("attacker");
        let signer2 = LogSigner::from_pem(&k2, &c2).unwrap();
        let mut forged = f.records.clone();
        for r in forged.iter_mut() {
            r.signer_cert_sha256 = f.pin.signer_cert_sha256.clone();
        }
        let mut prev: Vec<u8> = av_edge::hash::GENESIS.to_vec();
        let mut bytes = vec![];
        for r in forged.iter_mut() {
            r.prev_hash = prev.clone();
            seal(r, &signer2).unwrap();
            prev = r.record_hash.clone();
            bytes.extend(encode_frame(r).unwrap());
        }
        let r = refusal(f.load(&bytes));
        assert!(matches!(r, BoardReplayRefusal::LogDefect { .. }), "{r:?}");
    }

    #[test]
    fn a_log_cut_anywhere_is_refused_by_the_pin() {
        let f = fixture("cut", 6, None);
        let ends = f.frame_ends();
        let n = ends.len();
        assert_eq!(f.pin.records as usize, n - 1);
        // Cut at the SHUTDOWN: everything the pin counted is there, the closing record is not.
        let r = refusal(f.load(&f.bytes[..ends[n - 2]]));
        assert!(matches!(r, BoardReplayRefusal::ShutdownMissing { pinned_records } if pinned_records == f.pin.records), "{r:?}");
        // Cut at every earlier record boundary: fewer records than pinned.
        for keep in 1..n - 1 {
            let r = refusal(f.load(&f.bytes[..ends[keep - 1]]));
            assert!(matches!(r, BoardReplayRefusal::Truncated { pinned_records, log_records } if pinned_records == f.pin.records && log_records == keep as u64), "keep {keep}: {r:?}");
        }
        // An empty file.
        assert!(matches!(refusal(f.load(&[])), BoardReplayRefusal::Truncated { log_records: 0, .. }));
    }

    #[test]
    fn a_rewritten_history_resigned_with_the_genuine_key_does_not_match_the_pinned_head() {
        let f = fixture("rewrite", 6, None);
        let mut records = f.records.clone();
        records[3].outputs[0].payload[0] ^= 0xff;
        let bytes = f.resealed(records, 3);
        // It verifies completely...
        verify_bytes(&bytes, &LogVerifier::from_pem(&f.cert).unwrap()).unwrap();
        // ...and is refused: it is not the log the run pinned.
        let r = refusal(f.load(&bytes));
        assert!(matches!(r, BoardReplayRefusal::ChainHeadMismatch { at_record, .. } if at_record == f.pin.records), "{r:?}");
    }

    #[test]
    fn anything_but_one_shutdown_after_the_pinned_position_is_refused() {
        let f = fixture("tail", 4, None);
        let mut records = f.records.clone();
        records.push(step(99, START + 99 * PERIOD));
        let n = records.len();
        records[n - 1].sequence = n as u64;
        records[n - 1].producer_id = records[0].producer_id.clone();
        records[n - 1].signer_cert_sha256 = records[0].signer_cert_sha256.clone();
        let r = refusal(f.load(&f.resealed(records, n - 1)));
        assert!(matches!(r, BoardReplayRefusal::UnpinnedTail { .. }), "{r:?}");
        // A different kind in the closing position.
        let mut records = f.records.clone();
        let last = records.len() - 1;
        records[last].kind = BoardIoKind::Step as i32;
        let r = refusal(f.load(&f.resealed(records, last)));
        assert!(matches!(r, BoardReplayRefusal::UnpinnedTail { .. }), "{r:?}");
    }

    #[test]
    fn a_failed_exchange_and_a_useless_pin_are_refused() {
        let f = fixture("failed", 4, None);
        let mut records = f.records.clone();
        records[2].error = "Unavailable: link down".into();
        let r = refusal(f.load(&f.resealed(records, 2)));
        // The pinned head no longer matches either; the head check precedes the content checks,
        // so pin the failed log's own head to reach the exchange check.
        assert!(matches!(r, BoardReplayRefusal::ChainHeadMismatch { .. }), "{r:?}");
        let mut records = f.records.clone();
        records[2].error = "Unavailable: link down".into();
        let bytes = f.resealed(records, 2);
        let mut replay = f.replay_of(&bytes);
        replay.expected = BoardLogPin::of_log(&verify_bytes(&bytes, &LogVerifier::from_pem(&f.cert).unwrap()).unwrap()).unwrap();
        let r = refusal(load(&replay, RUN));
        assert!(matches!(r, BoardReplayRefusal::FailedExchange { record: 3, .. }), "{r:?}");
        let mut replay = f.replay_of(&f.bytes);
        replay.expected.records = 0;
        assert!(matches!(refusal(load(&replay, RUN)), BoardReplayRefusal::PinUnusable { .. }));
        replay.log_path = f.dir.join("nope.log");
        replay.expected = f.pin.clone();
        assert!(matches!(refusal(load(&replay, RUN)), BoardReplayRefusal::LogUnreadable { .. }));
    }

    #[test]
    fn a_log_that_is_not_this_runs_shape_is_refused_before_binding() {
        let f = fixture("shape", 6, Some(("pc", 3)));
        let mut ok = shape(6);
        ok.power_cycles.insert("pc".into(), START + 3 * PERIOD);
        f.load(&f.bytes).unwrap().into_script(INSTANCE, &ok).expect("the matching shape is accepted");
        let cases: Vec<(&str, Mutation)> = vec![
            ("a longer run", Box::new(|s| s.end_tai_ns += PERIOD)),
            ("a shorter run", Box::new(|s| s.end_tai_ns -= PERIOD)),
            ("another start", Box::new(|s| s.start_tai_ns += PERIOD)),
            ("another period", Box::new(|s| s.period_ns = PERIOD / 2)),
            ("another seed", Box::new(|s| s.seed = Some(43))),
            ("other parameters", Box::new(|s| s.bind_parameters.clear())),
            ("other ports", Box::new(|s| s.ports.push(pb::Port::default()))),
            ("no such power-cycle fault", Box::new(|s| s.power_cycles.clear())),
            ("the fault elsewhere", Box::new(|s| {
                s.power_cycles.insert("pc".into(), START + 2 * PERIOD);
            })),
        ];
        for (what, mutate) in cases {
            let mut s = ok.clone();
            mutate(&mut s);
            let r = refusal(f.load(&f.bytes).unwrap().into_script(INSTANCE, &s));
            assert!(matches!(r, BoardReplayRefusal::DoesNotMatchRun { .. }), "{what}: {r:?}");
        }
    }

    #[test]
    fn a_power_cycle_without_its_reset_or_a_stray_reset_is_refused() {
        let f = fixture("pcshape", 6, Some(("pc", 3)));
        let mut sh = shape(6);
        sh.power_cycles.insert("pc".into(), START + 3 * PERIOD);
        // Drop the RESET after the POWER_CYCLE, re-signed, with the pin of what is left: the shape check refuses it.
        let mut records = f.records.clone();
        let reset = records.iter().position(|r| r.kind == BoardIoKind::Reset as i32).unwrap();
        records.remove(reset);
        let bytes = f.resealed(records, reset);
        let mut replay = f.replay_of(&bytes);
        replay.expected = BoardLogPin::of_log(&verify_bytes(&bytes, &LogVerifier::from_pem(&f.cert).unwrap()).unwrap()).unwrap();
        let loaded = load(&replay, RUN).unwrap();
        assert!(matches!(refusal(loaded.into_script(INSTANCE, &sh)), BoardReplayRefusal::DoesNotMatchRun { .. }));
        // A RESET with no POWER_CYCLE before it (a reset the kernel never sends a board).
        let mut records = f.records.clone();
        let pc = records.iter().position(|r| r.kind == BoardIoKind::PowerCycle as i32).unwrap();
        records.remove(pc);
        let bytes = f.resealed(records, pc);
        let mut replay = f.replay_of(&bytes);
        replay.expected = BoardLogPin::of_log(&verify_bytes(&bytes, &LogVerifier::from_pem(&f.cert).unwrap()).unwrap()).unwrap();
        let loaded = load(&replay, RUN).unwrap();
        assert!(matches!(refusal(loaded.into_script(INSTANCE, &sh)), BoardReplayRefusal::DoesNotMatchRun { .. }));
    }

    #[test]
    fn a_pin_round_trips_through_provenance_attributes_and_a_missing_one_is_refused() {
        let pin = BoardLogPin { chain_head: "ab".repeat(32), records: 53, signer_cert_sha256: "cd".repeat(32) };
        let attrs: BTreeMap<String, String> = pin.attributes().into_iter().collect();
        assert_eq!(BoardLogPin::from_attributes(&attrs).unwrap(), pin);
        for key in [PIN_CHAIN_HEAD_KEY, PIN_RECORDS_KEY, PIN_SIGNER_KEY] {
            let mut missing = attrs.clone();
            missing.remove(key);
            assert!(matches!(BoardLogPin::from_attributes(&missing), Err(BoardReplayRefusal::PinUnusable { .. })), "{key}");
        }
        let mut bad = attrs.clone();
        bad.insert(PIN_RECORDS_KEY.into(), "many".into());
        assert!(matches!(BoardLogPin::from_attributes(&bad), Err(BoardReplayRefusal::PinUnusable { .. })));
        let trajectories: BTreeMap<String, pb::Trajectory> = [("obc".to_string(), pb::Trajectory { provenance: Some(pb::Provenance { attributes: attrs.clone().into_iter().collect(), ..Default::default() }), ..Default::default() })].into();
        assert_eq!(BoardLogPin::from_trajectories(&trajectories, "obc").unwrap(), pin);
        assert!(matches!(BoardLogPin::from_trajectories(&trajectories, "other"), Err(BoardReplayRefusal::PinUnusable { .. })));
    }

    #[test]
    fn the_exclusions_strip_exactly_the_named_fields() {
        let provenance = |extra: &[(&str, &str)]| pb::Provenance { attributes: extra.iter().map(|(k, v)| (k.to_string(), v.to_string())).chain([("system_definition_id".to_string(), "sys".to_string())]).collect(), ..Default::default() };
        let seg = |model: &str| pb::TrajectorySegment { name: "obc".into(), dynamics_model: model.into(), dynamics_hash: "h".into(), dynamics_depth: "board-lockstep".into(), ..Default::default() };
        let event = |id: &str, detail: &str| pb::Event {
            id: id.into(),
            detail: detail.into(),
            values: [("duration_ns".to_string(), 9.0), ("performed".to_string(), 1.0)].into(),
            ..Default::default()
        };
        let live = pb::RunProducts {
            pacing: Some(pb::PacingReport::default()),
            events: vec![event("fault:pc", "kept"), event("marker:pacing:overrun:5", "x"), event("marker:power_cycle:pc", "wall clock")],
            trajectories: [(
                "obc".to_string(),
                pb::Trajectory {
                    segments: vec![seg("board.obc")],
                    event_ids: vec!["fault:pc".into(), "marker:power_cycle:pc".into()],
                    provenance: Some(provenance(&[("binding_kind", "BINDING_KIND_BOARD"), ("board_binding_hash", "b"), ("board_link_hash", "l"), (PIN_CHAIN_HEAD_KEY, "c"), (PIN_RECORDS_KEY, "3"), (PIN_SIGNER_KEY, "s")])),
                    ..Default::default()
                },
            )]
            .into(),
            ..Default::default()
        };
        // Edge log: pacing, the overrun event, the outcome event's detail and duration only.
        let mut a = live.clone();
        strip_replay_exclusions(&mut a, "obc", ReplaySource::EdgeLog);
        assert!(a.pacing.is_none());
        assert_eq!(a.events.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["fault:pc", "marker:power_cycle:pc"]);
        assert_eq!(a.events[0], live.events[0], "the fault event is untouched");
        assert!(a.events[1].detail.is_empty() && !a.events[1].values.contains_key("duration_ns") && a.events[1].values["performed"] == 1.0);
        assert_eq!(a.trajectories, live.trajectories, "the board trajectory is not touched for an edge-log replay");
        // Port traffic: also the outcome event whole, the board segment's dynamics_* and the binding / pin attributes.
        let mut b = live.clone();
        strip_replay_exclusions(&mut b, "obc", ReplaySource::PortTraffic);
        assert_eq!(b.events.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["fault:pc"]);
        let t = &b.trajectories["obc"];
        assert_eq!(t.event_ids, ["fault:pc"]);
        assert_eq!((t.segments[0].dynamics_model.as_str(), t.segments[0].dynamics_hash.as_str(), t.segments[0].dynamics_depth.as_str(), t.segments[0].name.as_str()), ("", "", "", "obc"));
        assert_eq!(t.provenance.as_ref().unwrap().attributes, provenance(&[]).attributes, "only system_definition_id is left");
    }
}
