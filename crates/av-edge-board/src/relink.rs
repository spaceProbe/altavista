//! Re-handshaking with a peer that rebooted (question 245 (6)).
//!
//! `PeerLink::handshake` runs once, at startup, and the `PeerLink` it returns is the only owner of
//! the stream. A peer that really reboots loses its lockstep-local session: it needs a fresh
//! HELLO, then (from the kernel) a fresh BIND, before RESET and STEP. This module gives the
//! service that second handshake without changing `av-lockstep-shim`:
//!
//! - [`SharedStream`] is a cloneable handle on the one link stream, so a **new** `PeerLink` can be
//!   built on the same link while the old one is dropped.
//! - [`PeerCell`] holds the current `PeerLink` (swapped after a successful re-handshake) and the
//!   link's "lost" state, and implements [`PeerRelink`] when it was built with a [`RelinkSetup`].
//!
//! # When it runs, and why re-sending HELLO is safe
//!
//! Only when the operator **declared** that the power channel reboots the peer
//! (`--power-cycle-reboots-peer`), and only after a power cycle that performed. A power cycle on
//! the existing stand-ins (the fake channel, Renode) does not reboot the guest and never reaches
//! this code, so their behaviour is unchanged.
//!
//! The initial handshake sends HELLO **once**: the guest reads one HELLO, answers, and then expects
//! BIND, and a second HELLO where BIND is expected fails it. Re-sending is safe only when (1)
//! nothing reaches the guest before it is listening (the cFS stand-in's relay drops datagrams
//! while no guest is connected, as a UART whose receiver is not open would) and (2) the interval
//! between HELLOs is far longer than the guest's HELLO reply time, so the answer to one HELLO is
//! read before the next is sent. Each attempt discards what the link has queued before it sends
//! (late frames of the dead session, a stale HELLO) so a stale frame is not mistaken for an
//! answer. The residual risks are listed in the crate README.
//!
//! # Deadline
//!
//! HELLO is re-sent every `interval` until the peer answers or `timeout` (counted from the first
//! HELLO) has passed. A peer that never answers is [`RelinkOutcome::TimedOut`]; a peer that answers
//! with something that is not a valid v1 HELLO (version or magic mismatch) is
//! [`RelinkOutcome::Refused`] at once. Either leaves the link **lost**: every later exchange is
//! refused (`UNAVAILABLE`) until a later re-handshake succeeds, so nothing is ever written to a
//! session that no longer exists.
use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex, RwLock};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use av_lockstep_shim::{PeerLink, ProtocolError};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tonic::Status;

use crate::timed::unix_ns_now;

/// A cloneable handle on one link stream. Every clone reads and writes the same stream; the
/// callers (the old and the new `PeerLink`) never use it at the same time, because a
/// re-handshake runs inside the I/O log's exchange gate.
pub struct SharedStream<S>(Arc<Mutex<S>>);

impl<S> SharedStream<S> {
    pub fn new(inner: S) -> Self {
        Self(Arc::new(Mutex::new(inner)))
    }

    /// Run `f` with exclusive access to the stream (not across an `.await`).
    pub fn with<R>(&self, f: impl FnOnce(&mut S) -> R) -> R {
        f(&mut self.0.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

impl<S> Clone for SharedStream<S> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for SharedStream<S> {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        let mut inner = self.0.lock().unwrap_or_else(|e| e.into_inner());
        Pin::new(&mut *inner).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for SharedStream<S> {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        let mut inner = self.0.lock().unwrap_or_else(|e| e.into_inner());
        Pin::new(&mut *inner).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let mut inner = self.0.lock().unwrap_or_else(|e| e.into_inner());
        Pin::new(&mut *inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let mut inner = self.0.lock().unwrap_or_else(|e| e.into_inner());
        Pin::new(&mut *inner).poll_shutdown(cx)
    }
}

/// The declared timing of a re-handshake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelinkConfig {
    /// Between one HELLO and the next. Far longer than the guest's HELLO reply time.
    pub interval: Duration,
    /// From the first HELLO to giving up.
    pub timeout: Duration,
}

/// How a re-handshake ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelinkOutcome {
    /// The peer answered a HELLO; a new `PeerLink` is installed and the link is no longer lost.
    Rehandshaken,
    /// No peer answered within the deadline.
    TimedOut,
    /// A peer answered, but not with a v1 HELLO this service can proceed with.
    Refused(String),
}

/// Everything about one re-handshake, for the response, the log record and the stderr log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelinkReport {
    pub outcome: RelinkOutcome,
    pub hellos_sent: u32,
    pub config: RelinkConfig,
    pub started_unix_ns: i64,
    pub finished_unix_ns: i64,
    pub duration: Duration,
}

/// What the power-cycle service needs: whether a power cycle is followed by a re-handshake, and to
/// do it.
#[tonic::async_trait]
pub trait PeerRelink: Send + Sync {
    fn config(&self) -> RelinkConfig;
    /// Re-handshake with the peer. Called with the I/O log's exchange gate held.
    async fn rehandshake(&self) -> RelinkReport;
}

/// What a [`PeerCell`] needs to be able to re-handshake: another handle on the stream, a way to
/// discard what the link has queued, and the declared timing.
pub struct RelinkSetup<S> {
    pub stream: S,
    /// Discard anything queued on the link (late frames of the dead session) and return how many
    /// datagrams or bytes were discarded, for the log.
    pub discard: Box<dyn Fn(&S) -> usize + Send + Sync>,
    pub config: RelinkConfig,
}

/// The current `PeerLink`, and whether the link is lost.
pub struct PeerCell<S> {
    current: RwLock<Arc<PeerLink<S>>>,
    lost: Mutex<Option<String>>,
    relink: Option<RelinkSetup<S>>,
}

impl<S> PeerCell<S> {
    /// A cell that never re-handshakes (the service as it was before question 245 (6)).
    pub fn new(peer: Arc<PeerLink<S>>) -> Self {
        Self { current: RwLock::new(peer), lost: Mutex::new(None), relink: None }
    }

    /// A cell that re-handshakes with `setup` after a power cycle that performed.
    pub fn with_relink(peer: Arc<PeerLink<S>>, setup: RelinkSetup<S>) -> Self {
        Self { current: RwLock::new(peer), lost: Mutex::new(None), relink: Some(setup) }
    }

    /// Why the link is lost, if it is.
    pub fn lost(&self) -> Option<String> {
        self.lost.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The `PeerLink` to exchange on, or `UNAVAILABLE` naming why the link is lost.
    // `tonic::Status` is the error type every gRPC handler in this crate (and the shim) returns.
    #[allow(clippy::result_large_err)]
    pub fn current(&self) -> Result<Arc<PeerLink<S>>, Status> {
        if let Some(why) = self.lost() {
            return Err(Status::unavailable(format!("the link to the board is lost: {why}")));
        }
        Ok(Arc::clone(&self.current.read().unwrap_or_else(|e| e.into_inner())))
    }

    fn set_lost(&self, why: String) {
        *self.lost.lock().unwrap_or_else(|e| e.into_inner()) = Some(why);
    }

    fn install(&self, peer: PeerLink<S>) {
        *self.current.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(peer);
        *self.lost.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

/// A protocol error that says the thing on the other end is not (or not yet) a usable peer, as
/// opposed to one that says it is a peer we cannot talk to.
fn is_fatal(e: &ProtocolError) -> bool {
    matches!(e, ProtocolError::VersionMismatch { .. } | ProtocolError::BadHelloMagic { .. })
}

#[tonic::async_trait]
impl<S> PeerRelink for PeerCell<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + Sync + Clone + 'static,
{
    fn config(&self) -> RelinkConfig {
        self.relink.as_ref().expect("PeerRelink is only used on a cell built with_relink").config
    }

    async fn rehandshake(&self) -> RelinkReport {
        let setup = self.relink.as_ref().expect("PeerRelink is only used on a cell built with_relink");
        let config = setup.config;
        let started_unix_ns = unix_ns_now();
        let t0 = Instant::now();
        let mut hellos_sent = 0u32;
        let outcome = loop {
            let elapsed = t0.elapsed();
            if elapsed >= config.timeout {
                break RelinkOutcome::TimedOut;
            }
            let attempt = config.interval.min(config.timeout - elapsed);
            let discarded = (setup.discard)(&setup.stream);
            hellos_sent += 1;
            eprintln!("av-edge-board: re-handshake: HELLO #{hellos_sent} at +{} ms (discarded {discarded} queued item(s) first)", elapsed.as_millis());
            let attempt_started = Instant::now();
            match tokio::time::timeout(attempt, PeerLink::handshake(setup.stream.clone())).await {
                Ok(Ok(peer)) => {
                    eprintln!("av-edge-board: re-handshake: HELLO #{hellos_sent} answered {} ms after it was sent", attempt_started.elapsed().as_millis());
                    self.install(peer);
                    break RelinkOutcome::Rehandshaken;
                }
                Ok(Err(e)) if is_fatal(&e) => {
                    let why = e.to_string();
                    self.set_lost(format!("the peer answered the re-sent HELLO with something this service cannot proceed with: {why}"));
                    break RelinkOutcome::Refused(why);
                }
                Ok(Err(e)) => {
                    // Noise (a malformed datagram, a frame that is not a HELLO): not an answer.
                    // Keep the cadence, so a burst of noise cannot turn into a burst of HELLOs.
                    eprintln!("av-edge-board: re-handshake: not an answer to HELLO #{hellos_sent}: {e}");
                    tokio::time::sleep(attempt.saturating_sub(attempt_started.elapsed())).await;
                }
                Err(_) => {}
            }
        };
        let duration = t0.elapsed();
        if outcome == RelinkOutcome::TimedOut {
            self.set_lost(format!("no peer answered a lockstep-local HELLO within {} ms ({hellos_sent} sent, one every {} ms)", config.timeout.as_millis(), config.interval.as_millis()));
        }
        RelinkReport { outcome, hellos_sent, config, started_unix_ns, finished_unix_ns: unix_ns_now(), duration }
    }
}
