//! Power control for a `BINDING_KIND_BOARD` instance (question 242 (c)): the [`PowerControl`]
//! trait, its production implementation [`EdgePowerControl`] (a client of the board's edge
//! service), the typed errors, the recorded event, and [`perform`], the one place the order of a
//! power-cycle boundary is fixed.
//!
//! # Power control is an edge-service operation
//!
//! `BoardBinding.power_control` is the **edge node's** power control channel
//! (`av_edge::board::PowerControl`: `cmd:<absolute path>`, `gpio://...` reserved, empty none). The
//! board may hang off this host or off a separate Linux edge node, so **the kernel never runs the
//! channel**. [`EdgePowerControl`] asks the board's edge service (`av-edge-board`), at the same
//! `board.edge_address` the kernel already dials for the board, over `altavista.v1.BoardEdgeService`
//! (`proto/altavista/v1/board.proto`), with the same transport rules as the board's lockstep link
//! (plaintext only on loopback; `board.tls` with `ca_file` / `client_cert` / `client_key` otherwise,
//! question 155). The edge service runs its own configured channel on the edge node and answers
//! with a typed result; the kernel's behaviour is identical whichever host that is. The client is
//! `av_lockstep::board_edge`.
//!
//! # The boundary
//!
//! A `FAULT_TARGET_KIND_HARDWARE` / `"power_cycle"` fault naming a board instance is a boundary
//! like a container's (`fault::is_container_power_cycle`). At the fault's epoch the executor calls
//! [`perform`]: **first** `PowerControl::power_cycle` (a refused or failed power cycle aborts the
//! run with [`DrmError::BoardPowerCycle`] carrying the edge service's reason, and no `RESET` is
//! sent), **then** the same lockstep `RESET` a container's power cycle sends (`reason =
//! "fault:<fault id>"`), so the run continues deterministically from the board's reset state. The
//! run records the container's `EVENT_KIND_FAULT` event and, for a board, one more event
//! ([`power_cycle_event`]) with the outcome.
//!
//! # Load-time checks (`parse_board_spec`, `executor::execute`)
//!
//! A malformed `power_control` is [`DrmError::BoardPowerControl`] even with no fault (an empty one
//! is none; `gpio://...` is the reserved refusal). A `HARDWARE`/`power_cycle` fault on a board
//! instance whose `power_control` is empty is [`DrmError::BoardPowerCycleNeedsChannel`]. Any other
//! `HARDWARE` kind on a board is [`DrmError::HardwareFaultKindNotSupported`], as for a container.
//!
//! # Wall-clock-dependent product
//!
//! The power-cycle event records what happened on the edge node in wall time (the duration of the
//! channel, its captured standard error), so it is wall-clock dependent exactly as the pacing
//! overruns are: [`is_power_cycle_event`] recognises it and `pacing::WALL_CLOCK_DEPENDENT` names
//! it, for a replay to exclude. The container-style `EVENT_KIND_FAULT` event (`fault:<id>`) carries
//! nothing wall-clock dependent and is compared.
//!
//! # A board that really reboots (question 245 (6))
//!
//! A real board that power-cycles drops its lockstep-local link and boots new flight software. The
//! sequence for it, built and proven against a stand-in (a `docker restart` of the cFS posix
//! container stands in for the board reboot; **no board has been involved**):
//!
//! 1. The **edge service** is told (by its operator, `av-edge-board --power-cycle-reboots-peer`) that
//!    its power channel reboots the peer. After a channel that performed it re-handshakes with the
//!    rebooted flight software on the same link (HELLO re-sent at its declared interval until the
//!    peer answers, or its declared deadline) and says so in the response
//!    (`PowerCycleResponse.peer_rehandshaken`). A peer that never answers is a typed failure
//!    (`REHANDSHAKE_TIMEOUT`), which this module turns into
//!    [`PowerControlError::RehandshakeFailed`], naming the deadline: the run stops with
//!    [`DrmError::BoardPowerCycle`], no `Bind`, no `RESET`.
//! 2. The **kernel** then re-`Bind`s the instance with the run's **same** `LockstepBindRequest`
//!    (kept from the first `Bind`) before the `RESET`: the new flight-software session knows nothing
//!    until bound. The new `lockstep_capable = false` is [`ContainerError::RebindRefused`]; a
//!    different `binding_hash` is accepted (the second BIND is a record of the signed edge log, with
//!    the guest's answer, and the run's provenance keeps the first). The lockstep sequence numbers
//!    continue (the guest echoes whatever it is sent; nothing in the protocol restarts them).
//!    [`perform`] fixes the order: power cycle, re-Bind **iff the service re-handshook**, `RESET`.
//! 3. The kernel declares, per board instance, whether its power cycle is expected to reboot the
//!    peer: `board.rehandshake_timeout_ms` (default 0: not declared). It does two things. The
//!    kernel's wait for the `PowerCycle` reply is `board.power_control_timeout_ms` **plus** this (the
//!    channel's time and the re-handshake's deadline are two separate budgets, so a legitimate
//!    re-handshake cannot be cut off by a shorter client timeout); and a declared board whose edge
//!    service did *not* re-handshake (it was started without `--power-cycle-reboots-peer`) is
//!    [`PowerControlError::RehandshakeExpected`] at once, instead of a `RESET` sent to a guest that is
//!    waiting for a HELLO. The operator keeps the two declarations equal; the response's own
//!    `rehandshake_timeout_ms` is recorded.
//!
//! The outcome event ([`power_cycle_event`]) carries `values["rehandshaken"] = 1.0` for a
//! re-handshaken power cycle, and the re-handshake's HELLO count and duration (wall-clock dependent:
//! `pacing::WALL_CLOCK_DEPENDENT`, excluded by a replay). A power cycle of the existing stand-ins
//! (whose guest stays up) produces the event it always did, byte for byte.
use std::time::Duration;

use av_cdm::pb::{Event, EventKind, Provenance};
use av_lockstep::board_edge::{BlockingBoardEdgeClient, BoardPowerCycleFailure, BoardPowerCycleOutcome, BoardPowerCycleRefusal, PowerCycleRequest};

use super::binding::ContainerError;
use super::DrmError;

/// The `Event.id` prefix of a power-cycle outcome event; the suffix is the fault id.
pub const POWER_CYCLE_EVENT_ID_PREFIX: &str = "marker:power_cycle:";
/// The `Event.name` of a power-cycle outcome event.
pub const POWER_CYCLE_EVENT_NAME: &str = "board_power_cycle";

/// Whether `event` is a power-cycle outcome event built by [`power_cycle_event`].
pub fn is_power_cycle_event(event: &Event) -> bool {
    event.id.starts_with(POWER_CYCLE_EVENT_ID_PREFIX)
}

/// `board.power_control_timeout_ms` default: 60 s, above the edge service's own default channel
/// timeout (`--power-timeout-ms`, 30 s) so the service's typed `TIMEOUT` result normally arrives
/// before the kernel gives up.
pub const POWER_CONTROL_TIMEOUT_DEFAULT_MS: u64 = 60_000;

/// What the kernel asks the edge service to power-cycle, once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PowerCycleCall {
    pub run_id: String,
    pub instance: String,
    pub fault_id: String,
    /// The fault's epoch, TAI ns.
    pub tai_ns: i64,
    /// `BoardBinding.edge_node_id`.
    pub edge_node_id: String,
    /// `BoardBinding.power_control` in canonical form.
    pub power_control: String,
}

/// What the edge service's re-handshake with the rebooted peer reported (question 245 (6)); the
/// HELLO count and the duration are wall-clock facts (see the module doc).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RehandshakeReport {
    pub hellos_sent: u32,
    pub duration_ns: i64,
    /// The edge service's own deadline for it, ms.
    pub timeout_ms: u32,
}

/// What a performed power cycle reports (edge-node wall-clock facts: see the module doc).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PowerCycleReport {
    pub duration_ns: i64,
    pub started_unix_ns: i64,
    pub finished_unix_ns: i64,
    pub stderr_tail: String,
    /// The channel the edge service ran, canonical.
    pub power_control: String,
    /// `Some` iff the edge service re-handshook with the rebooted peer (the peer's lockstep-local
    /// session is new: the kernel re-`Bind`s before its `RESET`). `None` for a power cycle after
    /// which the peer's session survives (the existing stand-ins).
    pub rehandshake: Option<RehandshakeReport>,
}

/// Why a power cycle was not performed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PowerControlError {
    /// The edge service ran nothing: no channel, a different channel, or a different edge node.
    Refused { refusal: String, detail: String },
    /// The edge service started the channel and it did not succeed.
    Failed { failure: String, exit_status: i32, signal: i32, detail: String, stderr_tail: String },
    /// The RPC failed at the transport or gRPC level (the edge service's own board I/O log
    /// failing is `DATA_LOSS` / `FAILED_PRECONDITION` here).
    Rpc { detail: String },
    /// No reply within `board.power_control_timeout_ms` (plus `board.rehandshake_timeout_ms` when
    /// the DRM declares a re-handshake: the reported `timeout_ms` is the sum).
    TimedOut { timeout_ms: u64 },
    /// The response named no outcome the kernel knows.
    BadResponse { detail: String },
    /// The channel performed, but the edge service could not re-handshake with the rebooted peer
    /// (`REHANDSHAKE_TIMEOUT`: no answer within its deadline, `timeout_ms`; `REHANDSHAKE_REFUSED`: an
    /// answer it could not proceed with). The peer's session is lost: the run stops.
    RehandshakeFailed { failure: String, timeout_ms: u32, hellos_sent: u32, detail: String },
    /// The DRM declares (`board.rehandshake_timeout_ms`) that this board's power cycle reboots the
    /// peer, but the edge service performed it without re-handshaking (it was not started with
    /// `--power-cycle-reboots-peer`).
    RehandshakeExpected { declared_timeout_ms: u64 },
}

impl std::fmt::Display for PowerControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PowerControlError::Refused { refusal, detail } => write!(f, "the edge service refused the power cycle ({refusal}) and ran nothing: {detail}"),
            PowerControlError::Failed { failure, exit_status, signal, detail, stderr_tail } => {
                write!(f, "the edge service's power control channel failed ({failure}")?;
                if *exit_status != 0 {
                    write!(f, ", exit status {exit_status}")?;
                }
                if *signal != 0 {
                    write!(f, ", signal {signal}")?;
                }
                write!(f, "): {detail}")?;
                if !stderr_tail.is_empty() {
                    write!(f, "; its standard error ended: {:?}", stderr_tail.trim_end())?;
                }
                Ok(())
            }
            PowerControlError::Rpc { detail } => write!(f, "the PowerCycle RPC to the edge service failed: {detail}"),
            PowerControlError::TimedOut { timeout_ms } => write!(f, "the edge service gave no PowerCycle reply within {timeout_ms} ms (board.power_control_timeout_ms)"),
            PowerControlError::BadResponse { detail } => write!(f, "the edge service's PowerCycle response is not understood: {detail}"),
            PowerControlError::RehandshakeFailed { failure, timeout_ms, hellos_sent, detail } => write!(f, "the power cycle was performed but the edge service could not re-handshake with the rebooted peer ({failure}; its deadline was {timeout_ms} ms, {hellos_sent} HELLO(s) sent): {detail}"),
            PowerControlError::RehandshakeExpected { declared_timeout_ms } => write!(
                f,
                "the DRM declares that this board's power cycle reboots the peer (board.rehandshake_timeout_ms = {declared_timeout_ms}) but the edge service did not re-handshake after it: it was not started with --power-cycle-reboots-peer, so the peer's new session was never established and no RESET was sent"
            ),
        }
    }
}
impl std::error::Error for PowerControlError {}

/// The kernel's seam to a board's power control. The production implementation is
/// [`EdgePowerControl`]; tests use [`RecordingPowerControl`]. `&self`: a call neither changes the
/// instance nor needs exclusive access to anything the kernel owns.
pub trait PowerControl {
    /// Power-cycle the board once. `Ok` means the edge service performed it.
    fn power_cycle(&self, call: &PowerCycleCall) -> Result<PowerCycleReport, PowerControlError>;
}

/// The production [`PowerControl`]: one `BoardEdgeService.PowerCycle` RPC to the board's edge
/// service, on a short-lived worker thread so a silent service is a typed [`PowerControlError::TimedOut`]
/// and never a hang (the abandoned thread ends when its call does). It dials the address afresh
/// for each power cycle (a power cycle is rare, and a connection held since the Bind could be a
/// dead one).
#[derive(Debug, Clone)]
pub struct EdgePowerControl {
    address: String,
    /// `Some((ca_file, client_cert, client_key))` for mTLS, `None` for plaintext loopback.
    tls: Option<(String, String, String)>,
    timeout: Duration,
    /// `board.rehandshake_timeout_ms`: zero when the DRM does not declare that this board's power
    /// cycle reboots the peer. Added to `timeout` for the RPC's own deadline, and a declared board
    /// whose service did not re-handshake is [`PowerControlError::RehandshakeExpected`].
    rehandshake: Duration,
}

impl EdgePowerControl {
    /// `address` is the board's `board.edge_address` (`host:port`); `tls` its mTLS paths if
    /// `board.tls` is set. No re-handshake is declared (see [`EdgePowerControl::with_rehandshake`]).
    pub fn new(address: String, tls: Option<(String, String, String)>, timeout: Duration) -> Self {
        Self { address, tls, timeout, rehandshake: Duration::ZERO }
    }

    /// Declare that this board's power cycle reboots the peer and that the edge service may take up
    /// to `deadline` to re-handshake with it (`board.rehandshake_timeout_ms`; module doc, item 3).
    pub fn with_rehandshake(mut self, deadline: Duration) -> Self {
        self.rehandshake = deadline;
        self
    }
}

/// Turn a `PowerCycleResponse` into the typed result. Pure, so each branch is unit-tested.
pub fn interpret_response(response: av_lockstep::board_edge::PowerCycleResponse) -> Result<PowerCycleReport, PowerControlError> {
    let name = |value: i32, f: &dyn Fn(i32) -> Option<&'static str>| f(value).map(str::to_string).unwrap_or_else(|| format!("unknown({value})"));
    match BoardPowerCycleOutcome::try_from(response.outcome) {
        Ok(BoardPowerCycleOutcome::Performed) => Ok(PowerCycleReport {
            duration_ns: response.duration_ns,
            started_unix_ns: response.started_unix_ns,
            finished_unix_ns: response.finished_unix_ns,
            stderr_tail: response.stderr_tail,
            power_control: response.power_control,
            rehandshake: response.peer_rehandshaken.then_some(RehandshakeReport { hellos_sent: response.rehandshake_hellos_sent, duration_ns: response.rehandshake_duration_ns, timeout_ms: response.rehandshake_timeout_ms }),
        }),
        Ok(BoardPowerCycleOutcome::Refused) => Err(PowerControlError::Refused {
            refusal: name(response.refusal, &|v| BoardPowerCycleRefusal::try_from(v).ok().map(|r| r.as_str_name())),
            detail: response.detail,
        }),
        Ok(BoardPowerCycleOutcome::Failed) if matches!(BoardPowerCycleFailure::try_from(response.failure), Ok(BoardPowerCycleFailure::RehandshakeTimeout | BoardPowerCycleFailure::RehandshakeRefused)) => Err(PowerControlError::RehandshakeFailed {
            failure: name(response.failure, &|v| BoardPowerCycleFailure::try_from(v).ok().map(|r| r.as_str_name())),
            timeout_ms: response.rehandshake_timeout_ms,
            hellos_sent: response.rehandshake_hellos_sent,
            detail: response.detail,
        }),
        Ok(BoardPowerCycleOutcome::Failed) => Err(PowerControlError::Failed {
            failure: name(response.failure, &|v| BoardPowerCycleFailure::try_from(v).ok().map(|r| r.as_str_name())),
            exit_status: response.exit_status,
            signal: response.signal,
            detail: response.detail,
            stderr_tail: response.stderr_tail,
        }),
        Ok(BoardPowerCycleOutcome::Unspecified) | Err(_) => Err(PowerControlError::BadResponse { detail: format!("outcome {} is not PERFORMED, REFUSED or FAILED (detail: {:?})", response.outcome, response.detail) }),
    }
}

impl PowerControl for EdgePowerControl {
    fn power_cycle(&self, call: &PowerCycleCall) -> Result<PowerCycleReport, PowerControlError> {
        let request = PowerCycleRequest {
            run_id: call.run_id.clone(),
            instance: call.instance.clone(),
            fault_id: call.fault_id.clone(),
            tai_ns: call.tai_ns,
            edge_node_id: call.edge_node_id.clone(),
            power_control: call.power_control.clone(),
        };
        let (address, tls) = (self.address.clone(), self.tls.clone());
        let declared = self.rehandshake;
        let wait = self.timeout + declared;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("av-board-power".to_string())
            .spawn(move || {
                let result = (|| {
                    let mut client = match &tls {
                        Some((ca, cert, key)) => BlockingBoardEdgeClient::connect_mtls(&format!("https://{address}"), std::path::Path::new(ca), Some(std::path::Path::new(cert)), Some(std::path::Path::new(key))),
                        None => BlockingBoardEdgeClient::connect_plaintext(&address),
                    }
                    .map_err(|e| PowerControlError::Rpc { detail: format!("connecting to {address:?}: {e}") })?;
                    let response = client.power_cycle(request).map_err(|status| PowerControlError::Rpc { detail: format!("{:?}: {}", status.code(), status.message()) })?;
                    let report = interpret_response(response)?;
                    if !declared.is_zero() && report.rehandshake.is_none() {
                        return Err(PowerControlError::RehandshakeExpected { declared_timeout_ms: declared.as_millis() as u64 });
                    }
                    Ok(report)
                })();
                let _ = tx.send(result);
            })
            .expect("spawning the board power-control thread");
        match rx.recv_timeout(wait) {
            Ok(result) => result,
            Err(_) => Err(PowerControlError::TimedOut { timeout_ms: wait.as_millis() as u64 }),
        }
    }
}

/// Why [`perform`] stopped.
#[derive(Debug)]
pub enum PerformError {
    /// The power cycle was refused or failed; no `RESET` was sent.
    PowerCycle(PowerControlError),
    /// The power cycle was performed and the edge service re-handshook, and the kernel's re-`Bind`
    /// of the rebooted peer failed (no reply, an RPC error, or `lockstep_capable = false`).
    Rebind(ContainerError),
    /// The power cycle was performed and the lockstep `RESET` that follows it failed.
    Reset(ContainerError),
}

/// The order of a board power-cycle boundary, in one place: `power_cycle` first, then, only if the
/// edge service re-handshook with a rebooted peer, `rebind` (the same `Bind` request again), and
/// only if all of that went well, `reset` (the lockstep `RESET` with `reason = "fault:<id>"`).
/// Returns the report of the performed power cycle.
pub fn perform(control: &dyn PowerControl, call: &PowerCycleCall, rebind: impl FnOnce() -> Result<(), ContainerError>, reset: impl FnOnce() -> Result<(), ContainerError>) -> Result<PowerCycleReport, PerformError> {
    let report = control.power_cycle(call).map_err(PerformError::PowerCycle)?;
    if report.rehandshake.is_some() {
        rebind().map_err(PerformError::Rebind)?;
    }
    reset().map_err(PerformError::Reset)?;
    Ok(report)
}

impl PerformError {
    /// The run-ending [`DrmError`].
    pub fn into_drm_error(self, instance: &str, fault_id: &str) -> DrmError {
        match self {
            PerformError::PowerCycle(source) => DrmError::BoardPowerCycle { instance: instance.to_string(), fault_id: fault_id.to_string(), source: Box::new(source) },
            PerformError::Rebind(source) | PerformError::Reset(source) => DrmError::ContainerProtocol { instance: instance.to_string(), source },
        }
    }
}

/// The event recording a performed power cycle: `EVENT_KIND_MARKER` at the fault's epoch on the
/// board instance, `values.duration_ns` the edge node's measured duration of the channel,
/// `detail` the channel and what it wrote to standard error. Wall-clock dependent as a whole
/// (module doc).
pub fn power_cycle_event(fault_id: &str, instance: &str, tai_ns: i64, report: &PowerCycleReport, provenance: Provenance) -> Event {
    let mut detail = format!("board power cycle for fault {fault_id:?} performed by the edge service's channel {:?} in {} ms", report.power_control, report.duration_ns / 1_000_000);
    if !report.stderr_tail.trim().is_empty() {
        detail.push_str(&format!("; its standard error ended: {:?}", report.stderr_tail.trim_end()));
    }
    let mut values = std::collections::BTreeMap::from([("performed".to_string(), 1.0), ("duration_ns".to_string(), report.duration_ns as f64)]);
    if let Some(r) = &report.rehandshake {
        // The peer rebooted and the edge service re-handshook with it (the kernel re-Bound it before the
        // RESET): `rehandshaken` is compared by a replay; the HELLO count and the duration are the
        // wall-clock facts `pacing::WALL_CLOCK_DEPENDENT` names.
        detail.push_str(&format!("; the peer rebooted and the edge service re-handshook with it in {} ms ({} HELLO(s) sent)", r.duration_ns / 1_000_000, r.hellos_sent));
        values.insert("rehandshaken".to_string(), 1.0);
        values.insert("rehandshake_hellos_sent".to_string(), r.hellos_sent as f64);
        values.insert("rehandshake_duration_ns".to_string(), r.duration_ns as f64);
    }
    Event {
        id: format!("{POWER_CYCLE_EVENT_ID_PREFIX}{fault_id}"),
        entity_id: instance.to_string(),
        tai_ns,
        kind: EventKind::Marker as i32,
        name: POWER_CYCLE_EVENT_NAME.to_string(),
        detail,
        values,
        reference_id: fault_id.to_string(),
        provenance: Some(provenance),
        ..Default::default()
    }
}

/// A [`PowerControl`] that records every call and answers from a script: the fake for the
/// kernel's own unit tests. (The end-to-end fake sits *behind the same RPC*: an `av-edge-board`
/// configured with a recording `cmd:` executable; `tests/drm_board_power.rs`.)
#[cfg(test)]
pub(crate) struct RecordingPowerControl {
    pub calls: std::cell::RefCell<Vec<PowerCycleCall>>,
    pub answer: std::cell::RefCell<Result<PowerCycleReport, PowerControlError>>,
}

#[cfg(test)]
impl RecordingPowerControl {
    pub fn ok() -> Self {
        Self { calls: Default::default(), answer: std::cell::RefCell::new(Ok(PowerCycleReport { duration_ns: 5_000_000, power_control: "cmd:/x".into(), ..Default::default() })) }
    }
    pub fn failing(error: PowerControlError) -> Self {
        Self { calls: Default::default(), answer: std::cell::RefCell::new(Err(error)) }
    }
}

#[cfg(test)]
impl PowerControl for RecordingPowerControl {
    fn power_cycle(&self, call: &PowerCycleCall) -> Result<PowerCycleReport, PowerControlError> {
        self.calls.borrow_mut().push(call.clone());
        self.answer.borrow().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use av_lockstep::board_edge::PowerCycleResponse;
    use std::cell::RefCell;

    fn call() -> PowerCycleCall {
        PowerCycleCall { run_id: "r".into(), instance: "controller".into(), fault_id: "pc1".into(), tai_ns: 7, edge_node_id: "zcu104-a".into(), power_control: "cmd:/x".into() }
    }

    #[test]
    fn the_power_cycle_comes_first_and_the_reset_follows_it() {
        let order = RefCell::new(Vec::<&str>::new());
        struct Ordered<'a>(&'a RefCell<Vec<&'static str>>);
        impl PowerControl for Ordered<'_> {
            fn power_cycle(&self, _: &PowerCycleCall) -> Result<PowerCycleReport, PowerControlError> {
                self.0.borrow_mut().push("power_cycle");
                Ok(PowerCycleReport::default())
            }
        }
        let report = perform(
            &Ordered(&order),
            &call(),
            || {
                order.borrow_mut().push("rebind");
                Ok(())
            },
            || {
                order.borrow_mut().push("reset");
                Ok(())
            },
        )
        .expect("performed");
        assert_eq!(*order.borrow(), ["power_cycle", "reset"], "no re-handshake: the peer's session survived, so no re-Bind");
        assert_eq!(report, PowerCycleReport::default());
    }

    #[test]
    fn a_refused_or_failed_power_cycle_sends_no_reset_and_carries_the_services_reason() {
        for error in [
            PowerControlError::Refused { refusal: "BOARD_POWER_CYCLE_REFUSAL_NO_CHANNEL".into(), detail: "no channel configured".into() },
            PowerControlError::Failed { failure: "BOARD_POWER_CYCLE_FAILURE_EXIT_STATUS".into(), exit_status: 3, signal: 0, detail: "exited with status 3".into(), stderr_tail: "relay stuck\n".into() },
            PowerControlError::TimedOut { timeout_ms: 500 },
        ] {
            let fake = RecordingPowerControl::failing(error.clone());
            let reset_ran = RefCell::new(false);
            let err = perform(&fake, &call(), || Ok(()), || {
                *reset_ran.borrow_mut() = true;
                Ok(())
            })
            .expect_err("must stop");
            assert!(!*reset_ran.borrow(), "no RESET after a power cycle that was not performed");
            assert_eq!(fake.calls.borrow().len(), 1);
            let drm = err.into_drm_error("controller", "pc1");
            let text = drm.to_string();
            assert!(matches!(&drm, DrmError::BoardPowerCycle { instance, fault_id, source } if instance == "controller" && fault_id == "pc1" && **source == error), "{drm:?}");
            assert!(text.contains("pc1") && text.contains("controller") && text.contains(&error.to_string()), "{text}");
        }
    }

    #[test]
    fn a_failing_reset_after_a_performed_power_cycle_is_the_lockstep_protocol_error() {
        let fake = RecordingPowerControl::ok();
        let err = perform(&fake, &call(), || Ok(()), || Err(ContainerError::ResetRpc { detail: "link down".into() })).expect_err("reset failed");
        assert_eq!(fake.calls.borrow().len(), 1, "the power cycle itself was performed");
        assert!(matches!(err.into_drm_error("controller", "pc1"), DrmError::ContainerProtocol { source: ContainerError::ResetRpc { .. }, .. }));
    }

    #[test]
    fn each_response_shape_is_interpreted() {
        let performed = PowerCycleResponse { outcome: BoardPowerCycleOutcome::Performed as i32, duration_ns: 9, started_unix_ns: 1, finished_unix_ns: 2, stderr_tail: "ok\n".into(), power_control: "cmd:/x".into(), ..Default::default() };
        assert_eq!(interpret_response(performed), Ok(PowerCycleReport { duration_ns: 9, started_unix_ns: 1, finished_unix_ns: 2, stderr_tail: "ok\n".into(), power_control: "cmd:/x".into(), rehandshake: None }));

        let refused = PowerCycleResponse { outcome: BoardPowerCycleOutcome::Refused as i32, refusal: BoardPowerCycleRefusal::ChannelMismatch as i32, detail: "differs".into(), ..Default::default() };
        assert_eq!(interpret_response(refused), Err(PowerControlError::Refused { refusal: "BOARD_POWER_CYCLE_REFUSAL_CHANNEL_MISMATCH".into(), detail: "differs".into() }));

        let failed = PowerCycleResponse { outcome: BoardPowerCycleOutcome::Failed as i32, failure: BoardPowerCycleFailure::Timeout as i32, detail: "killed".into(), stderr_tail: "t".into(), ..Default::default() };
        assert_eq!(
            interpret_response(failed),
            Err(PowerControlError::Failed { failure: "BOARD_POWER_CYCLE_FAILURE_TIMEOUT".into(), exit_status: 0, signal: 0, detail: "killed".into(), stderr_tail: "t".into() })
        );

        for bad in [0, 9] {
            let r = PowerCycleResponse { outcome: bad, detail: "?".into(), ..Default::default() };
            assert!(matches!(interpret_response(r), Err(PowerControlError::BadResponse { .. })), "outcome {bad}");
        }
    }

    fn rehandshaken_report() -> PowerCycleReport {
        PowerCycleReport { power_control: "cmd:/x".into(), rehandshake: Some(RehandshakeReport { hellos_sent: 3, duration_ns: 2_012_000_000, timeout_ms: 60_000 }), ..Default::default() }
    }

    /// Question 245 (6): the order with a re-handshake is power cycle, re-Bind, RESET; a re-Bind that
    /// fails sends no RESET and is the lockstep protocol error; a power cycle that did not
    /// re-handshake never re-Binds.
    #[test]
    fn a_rehandshake_is_followed_by_the_rebind_and_then_the_reset() {
        let order = RefCell::new(Vec::<&str>::new());
        let fake = RecordingPowerControl { calls: Default::default(), answer: RefCell::new(Ok(rehandshaken_report())) };
        let report = perform(
            &fake,
            &call(),
            || {
                order.borrow_mut().push("rebind");
                Ok(())
            },
            || {
                order.borrow_mut().push("reset");
                Ok(())
            },
        )
        .expect("performed");
        assert_eq!(*order.borrow(), ["rebind", "reset"]);
        assert_eq!(report.rehandshake.as_ref().unwrap().hellos_sent, 3);

        let reset_ran = RefCell::new(false);
        let err = perform(&fake, &call(), || Err(ContainerError::RebindRefused { reason: "no".into() }), || {
            *reset_ran.borrow_mut() = true;
            Ok(())
        })
        .expect_err("the re-Bind failed");
        assert!(!*reset_ran.borrow(), "no RESET to a peer that could not be re-bound");
        assert!(matches!(err.into_drm_error("controller", "pc1"), DrmError::ContainerProtocol { source: ContainerError::RebindRefused { .. }, .. }));
    }

    #[test]
    fn the_rehandshake_response_shapes_are_interpreted() {
        let rehandshaken = PowerCycleResponse { outcome: BoardPowerCycleOutcome::Performed as i32, peer_rehandshaken: true, rehandshake_hellos_sent: 3, rehandshake_duration_ns: 7, rehandshake_timeout_ms: 60_000, power_control: "cmd:/x".into(), ..Default::default() };
        assert_eq!(interpret_response(rehandshaken).unwrap().rehandshake, Some(RehandshakeReport { hellos_sent: 3, duration_ns: 7, timeout_ms: 60_000 }));

        for failure in [BoardPowerCycleFailure::RehandshakeTimeout, BoardPowerCycleFailure::RehandshakeRefused] {
            let r = PowerCycleResponse { outcome: BoardPowerCycleOutcome::Failed as i32, failure: failure as i32, rehandshake_timeout_ms: 1500, rehandshake_hellos_sent: 5, detail: "no peer answered a lockstep-local HELLO within 1500 ms".into(), ..Default::default() };
            let e = interpret_response(r).unwrap_err();
            assert!(matches!(&e, PowerControlError::RehandshakeFailed { failure: f, timeout_ms: 1500, hellos_sent: 5, .. } if f == failure.as_str_name()), "{e:?}");
            assert!(e.to_string().contains("1500 ms") && e.to_string().contains("5 HELLO"), "{e}");
        }
    }

    #[test]
    fn the_event_of_a_rehandshaken_power_cycle_says_so_and_the_event_of_any_other_is_unchanged() {
        let plain = power_cycle_event("pc1", "controller", 1_000, &PowerCycleReport { duration_ns: 12_000_000, power_control: "cmd:/x".into(), ..Default::default() }, Provenance::default());
        assert_eq!(plain.values.keys().map(String::as_str).collect::<Vec<_>>(), ["duration_ns", "performed"], "a stand-in's power cycle: exactly the event it always was");
        let e = power_cycle_event("pc1", "controller", 1_000, &rehandshaken_report(), Provenance::default());
        assert_eq!((e.values["rehandshaken"], e.values["rehandshake_hellos_sent"], e.values["rehandshake_duration_ns"]), (1.0, 3.0, 2_012_000_000.0));
        assert!(e.detail.contains("re-handshook") && e.detail.contains("3 HELLO"), "{}", e.detail);
    }

    #[test]
    fn a_declared_rehandshake_that_the_service_did_not_do_is_refused_and_the_rpc_waits_for_the_deadline_too() {
        // A stand-in edge service is the integration test; here only the budget arithmetic that needs
        // no server: a silent peer is `TimedOut` with the sum of the two budgets.
        let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let quiet = EdgePowerControl::new(silent.local_addr().unwrap().to_string(), None, Duration::from_millis(300)).with_rehandshake(Duration::from_millis(200));
        let err = quiet.power_cycle(&call()).expect_err("silent");
        assert_eq!(err, PowerControlError::TimedOut { timeout_ms: 500 });
    }

    #[test]
    fn the_event_is_a_wall_clock_dependent_marker_on_the_instance() {
        let report = PowerCycleReport { duration_ns: 12_000_000, power_control: "cmd:/opt/ps".into(), stderr_tail: "done\n".into(), ..Default::default() };
        let e = power_cycle_event("pc1", "controller", 1_000, &report, Provenance::default());
        assert!(is_power_cycle_event(&e));
        assert_eq!((e.id.as_str(), e.entity_id.as_str(), e.tai_ns, e.reference_id.as_str()), ("marker:power_cycle:pc1", "controller", 1_000, "pc1"));
        assert_eq!(e.kind, EventKind::Marker as i32);
        assert_eq!(e.values["duration_ns"], 12_000_000.0);
        assert!(e.detail.contains("cmd:/opt/ps") && e.detail.contains("done"), "{}", e.detail);
        // Not confused with a fault event or an overrun.
        assert!(!is_power_cycle_event(&Event { id: "fault:pc1".into(), ..Default::default() }));
        assert!(!is_power_cycle_event(&Event { id: format!("{}1", crate::pacing::OVERRUN_EVENT_ID_PREFIX), ..Default::default() }));
    }

    /// A service that is not there is a typed RPC error, and one that never speaks HTTP/2 is a
    /// bounded timeout (no hang).
    #[test]
    fn an_unreachable_or_silent_edge_service_is_a_typed_error() {
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let gone = EdgePowerControl::new(format!("127.0.0.1:{port}"), None, Duration::from_secs(5));
        let err = gone.power_cycle(&call()).expect_err("nothing listens");
        assert!(matches!(&err, PowerControlError::Rpc { detail } if detail.contains("connecting")), "{err:?}");

        let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let quiet = EdgePowerControl::new(silent.local_addr().unwrap().to_string(), None, Duration::from_millis(400));
        let t0 = std::time::Instant::now();
        let err = quiet.power_cycle(&call()).expect_err("silent");
        assert_eq!(err, PowerControlError::TimedOut { timeout_ms: 400 });
        assert!(t0.elapsed() < Duration::from_secs(5));
    }
}
