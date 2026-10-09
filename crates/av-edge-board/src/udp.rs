//! The UDP transport: lockstep-local v1 with **one whole frame per datagram**.
//!
//! A datagram carries exactly one frame, byte for byte what the Unix-socket and serial
//! transports carry: `[len u32 LE][type u8][payload]`, the length prefix kept. The bytes of
//! a run are therefore the same frames on every transport.
//!
//! # Why an `AsyncRead + AsyncWrite` adapter, and not a frame-level seam in `PeerLink`
//!
//! `PeerLink<S: AsyncRead + AsyncWrite>` reads a frame with two `read_exact` calls (the
//! 4-byte length, then the rest) and writes one with a single `write_all` and a flush.
//! [`UdpFrameStream`] satisfies that contract: on read it serves the bytes of one
//! validated datagram; on write it buffers bytes until they form one complete frame, then
//! sends one datagram. That keeps `av-lockstep-shim` untouched (it is compiled into the cFS
//! image and its bytes are pinned by hash), keeps one `PeerLink` for all three transports,
//! and costs nothing: a frame-level seam would add a second trait to the shim for no
//! behavioural gain. The price is that datagram errors reach `PeerLink` wrapped in
//! `std::io::Error` (`FrameError::Io`); the typed [`DatagramError`] is recoverable with
//! [`datagram_error`].
//!
//! # What is refused
//!
//! - A received datagram that is not exactly one complete frame (shorter than the 5-byte
//!   minimum, a declared length of zero or over `framing::MAX_FRAME_LEN`, a datagram
//!   shorter than its declared frame, or one with bytes after the frame) is a typed
//!   [`DatagramError`]; it is counted and it fails the read that was waiting for it (the
//!   request-response exchange cannot continue past a lost or garbled reply, and a silent
//!   resync would hide the loss). The stream stays usable afterwards.
//! - A datagram from any address other than the configured peer (every address the
//!   configured host resolves to, at the configured port) is dropped, counted and logged
//!   (the first few), and the read keeps waiting. The socket is deliberately not
//!   `connect`ed: the kernel would filter foreign sources silently and there would be
//!   nothing to count.
//! - A frame larger than one UDP datagram can carry ([`MAX_DATAGRAM_PAYLOAD`] bytes) cannot
//!   be sent and is a typed error on write. IP fragmentation of frames above the path MTU
//!   is left to the network; there is no retransmission and no reordering protection
//!   (lockstep-local is strictly request-response, so a lost datagram is a stalled
//!   exchange, surfaced by the caller's timeout).
//!
//! HELLO is sent once, like every transport: a lost HELLO is a handshake timeout, not a retry.
use std::collections::VecDeque;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::task::{ready, Context, Poll};

use av_lockstep_shim::framing::MAX_FRAME_LEN;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::UdpSocket;

/// The largest UDP payload over IPv4 (65535 less the 8-byte UDP and 20-byte IP headers).
pub const MAX_DATAGRAM_PAYLOAD: usize = 65_507;

/// How many foreign-source datagrams are logged individually before only the count grows.
const FOREIGN_LOG_LIMIT: u64 = 5;

/// A datagram (received or to be sent) that is not exactly one lockstep-local frame.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DatagramError {
    #[error("datagram of {got} bytes is shorter than the 5-byte minimum frame (4-byte length and 1-byte type)")]
    TooShort { got: usize },
    #[error("datagram declares a frame length of 0 (every frame has at least its type byte)")]
    ZeroLength,
    #[error("datagram declares a frame length of {declared} bytes, over the {max} byte limit")]
    TooLarge { declared: u32, max: u32 },
    #[error("datagram is {got} bytes but its frame needs {need} (4-byte length + declared {declared}): half a frame is not accepted")]
    Truncated { got: usize, need: usize, declared: u32 },
    #[error("datagram is {got} bytes but its frame is {need}: {extra} trailing byte(s) after the frame")]
    TrailingBytes { got: usize, need: usize, extra: usize },
    #[error("frame of {size} bytes cannot be sent as one datagram (the limit is {max})")]
    TooLargeToSend { size: usize, max: usize },
}

/// Recover the typed [`DatagramError`] from an `io::Error` this stream produced (directly,
/// or inside `FrameError::Io`).
pub fn datagram_error(e: &io::Error) -> Option<&DatagramError> {
    e.get_ref()?.downcast_ref::<DatagramError>()
}

/// `Ok(())` iff `datagram` is exactly one complete frame.
pub fn validate_datagram(datagram: &[u8]) -> Result<(), DatagramError> {
    if datagram.len() < 5 {
        return Err(DatagramError::TooShort { got: datagram.len() });
    }
    let declared = u32::from_le_bytes([datagram[0], datagram[1], datagram[2], datagram[3]]);
    if declared == 0 {
        return Err(DatagramError::ZeroLength);
    }
    if declared > MAX_FRAME_LEN {
        return Err(DatagramError::TooLarge { declared, max: MAX_FRAME_LEN });
    }
    let need = 4 + declared as usize;
    if datagram.len() < need {
        return Err(DatagramError::Truncated { got: datagram.len(), need, declared });
    }
    if datagram.len() > need {
        return Err(DatagramError::TrailingBytes { got: datagram.len(), need, extra: datagram.len() - need });
    }
    Ok(())
}

/// Counters the stream keeps; shared so the process can report them at exit.
#[derive(Debug, Default)]
pub struct UdpStats {
    /// Datagrams from the configured peer that were exactly one frame.
    pub accepted: AtomicU64,
    /// Datagrams from any other address, dropped.
    pub foreign_dropped: AtomicU64,
    /// Datagrams from the peer that were not exactly one frame.
    pub malformed: AtomicU64,
    /// Datagrams sent.
    pub sent: AtomicU64,
}

impl UdpStats {
    pub fn snapshot(&self) -> (u64, u64, u64, u64) {
        (self.accepted.load(Ordering::Relaxed), self.foreign_dropped.load(Ordering::Relaxed), self.malformed.load(Ordering::Relaxed), self.sent.load(Ordering::Relaxed))
    }
}

/// One UDP peer carrying whole frames; see the module doc.
pub struct UdpFrameStream {
    socket: UdpSocket,
    peers: Vec<SocketAddr>,
    recv_buf: Vec<u8>,
    rx: Vec<u8>,
    rx_pos: usize,
    tx: Vec<u8>,
    outbox: VecDeque<Vec<u8>>,
    stats: Arc<UdpStats>,
}

impl UdpFrameStream {
    /// Resolve `host:port`, bind a local socket (`local`, else an ephemeral port on the
    /// wildcard address of the peer's family) and return the stream. The first resolved
    /// address is the one frames are sent to; datagrams from any resolved address are
    /// accepted.
    pub async fn open(host: &str, port: u16, local: Option<SocketAddr>) -> io::Result<Self> {
        let resolved: Vec<SocketAddr> = tokio::net::lookup_host((host, port)).await?.collect();
        let Some(first) = resolved.first().copied() else {
            return Err(io::Error::new(io::ErrorKind::NotFound, format!("{host}:{port} resolved to no address")));
        };
        let peers: Vec<SocketAddr> = resolved.into_iter().filter(|a| a.is_ipv4() == first.is_ipv4()).collect();
        let local = local.unwrap_or_else(|| if first.is_ipv4() { SocketAddr::from(([0, 0, 0, 0], 0)) } else { SocketAddr::from(([0u16; 8], 0)) });
        let socket = UdpSocket::bind(local).await?;
        Ok(Self::from_socket(socket, peers))
    }

    /// A stream over an already-bound socket; `peers[0]` is the send address and every
    /// entry is an accepted source.
    pub fn from_socket(socket: UdpSocket, peers: Vec<SocketAddr>) -> Self {
        assert!(!peers.is_empty(), "a UDP stream needs at least one peer address");
        Self { socket, peers, recv_buf: vec![0u8; 65_536], rx: Vec::new(), rx_pos: 0, tx: Vec::new(), outbox: VecDeque::new(), stats: Arc::new(UdpStats::default()) }
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    pub fn peer_addr(&self) -> SocketAddr {
        self.peers[0]
    }

    pub fn stats(&self) -> Arc<UdpStats> {
        Arc::clone(&self.stats)
    }

    /// Move every complete frame out of `tx` into `outbox`, validating its size.
    fn queue_complete_frames(&mut self) -> io::Result<()> {
        loop {
            if self.tx.len() < 4 {
                return Ok(());
            }
            let declared = u32::from_le_bytes([self.tx[0], self.tx[1], self.tx[2], self.tx[3]]);
            if declared == 0 {
                self.tx.clear();
                return Err(io::Error::new(io::ErrorKind::InvalidData, DatagramError::ZeroLength));
            }
            if declared > MAX_FRAME_LEN {
                self.tx.clear();
                return Err(io::Error::new(io::ErrorKind::InvalidData, DatagramError::TooLarge { declared, max: MAX_FRAME_LEN }));
            }
            let need = 4 + declared as usize;
            if need > MAX_DATAGRAM_PAYLOAD {
                self.tx.clear();
                return Err(io::Error::new(io::ErrorKind::InvalidInput, DatagramError::TooLargeToSend { size: need, max: MAX_DATAGRAM_PAYLOAD }));
            }
            if self.tx.len() < need {
                return Ok(());
            }
            let rest = self.tx.split_off(need);
            self.outbox.push_back(std::mem::replace(&mut self.tx, rest));
        }
    }

    /// Send queued datagrams until the outbox is empty or the socket is not writable.
    fn poll_drain(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        while let Some(front) = self.outbox.front() {
            let n = ready!(self.socket.poll_send_to(cx, front, self.peers[0]))?;
            if n != front.len() {
                return Poll::Ready(Err(io::Error::new(io::ErrorKind::WriteZero, format!("short datagram send: {n} of {} bytes", front.len()))));
            }
            self.outbox.pop_front();
            self.stats.sent.fetch_add(1, Ordering::Relaxed);
        }
        Poll::Ready(Ok(()))
    }
}

impl AsyncRead for UdpFrameStream {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, out: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        loop {
            if this.rx_pos < this.rx.len() {
                let n = out.remaining().min(this.rx.len() - this.rx_pos);
                out.put_slice(&this.rx[this.rx_pos..this.rx_pos + n]);
                this.rx_pos += n;
                return Poll::Ready(Ok(()));
            }
            let mut rb = ReadBuf::new(&mut this.recv_buf);
            let src = ready!(this.socket.poll_recv_from(cx, &mut rb))?;
            let len = rb.filled().len();
            if !this.peers.contains(&src) {
                let n = this.stats.foreign_dropped.fetch_add(1, Ordering::Relaxed) + 1;
                if n <= FOREIGN_LOG_LIMIT {
                    eprintln!("av-edge-board: dropped a {len}-byte datagram from {src}, which is not the configured peer {} (drop #{n})", this.peers[0]);
                }
                continue;
            }
            if let Err(e) = validate_datagram(&this.recv_buf[..len]) {
                this.stats.malformed.fetch_add(1, Ordering::Relaxed);
                return Poll::Ready(Err(io::Error::new(io::ErrorKind::InvalidData, e)));
            }
            this.stats.accepted.fetch_add(1, Ordering::Relaxed);
            this.rx.clear();
            this.rx.extend_from_slice(&this.recv_buf[..len]);
            this.rx_pos = 0;
        }
    }
}

impl AsyncWrite for UdpFrameStream {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        this.tx.extend_from_slice(buf);
        this.queue_complete_frames()?;
        // Try to send now; if the socket is busy the bytes are still accepted and `poll_flush`
        // finishes the job.
        if let Poll::Ready(Err(e)) = this.poll_drain(cx) {
            return Poll::Ready(Err(e));
        }
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.get_mut().poll_drain(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.get_mut().poll_drain(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use av_lockstep_shim::framing::{encode_frame, FrameType};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn exactly_one_frame_is_valid_and_everything_else_is_a_typed_error() {
        let frame = encode_frame(FrameType::Step, &[1, 2, 3]);
        assert_eq!(validate_datagram(&frame), Ok(()));
        assert_eq!(validate_datagram(&[]), Err(DatagramError::TooShort { got: 0 }));
        assert_eq!(validate_datagram(&frame[..4]), Err(DatagramError::TooShort { got: 4 }));
        assert_eq!(validate_datagram(&[0, 0, 0, 0, 9]), Err(DatagramError::ZeroLength));
        assert_eq!(validate_datagram(&[0xff, 0xff, 0xff, 0xff, 9]), Err(DatagramError::TooLarge { declared: u32::MAX, max: MAX_FRAME_LEN }));
        // half a frame
        assert_eq!(validate_datagram(&frame[..frame.len() - 2]), Err(DatagramError::Truncated { got: frame.len() - 2, need: frame.len(), declared: 4 }));
        // a frame and a half
        let mut two = frame.clone();
        two.extend_from_slice(&frame[..3]);
        assert_eq!(validate_datagram(&two), Err(DatagramError::TrailingBytes { got: frame.len() + 3, need: frame.len(), extra: 3 }));
    }

    async fn pair() -> (UdpFrameStream, UdpSocket) {
        let far = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let far_addr = far.local_addr().unwrap();
        let near = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        (UdpFrameStream::from_socket(near, vec![far_addr]), far)
    }

    #[tokio::test]
    async fn a_frame_written_in_pieces_is_one_datagram() {
        let (mut stream, far) = pair().await;
        let frame = encode_frame(FrameType::Bind, &[7u8; 300]);
        stream.write_all(&frame[..3]).await.unwrap();
        stream.write_all(&frame[3..100]).await.unwrap();
        stream.flush().await.unwrap();
        // Nothing is sent for an incomplete frame.
        let mut buf = vec![0u8; 2048];
        assert!(tokio::time::timeout(std::time::Duration::from_millis(100), far.recv_from(&mut buf)).await.is_err());
        stream.write_all(&frame[100..]).await.unwrap();
        stream.flush().await.unwrap();
        let (n, _) = far.recv_from(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], &frame[..]);
        assert_eq!(stream.stats().snapshot().3, 1);
    }

    #[tokio::test]
    async fn two_frames_in_one_write_are_two_datagrams() {
        let (mut stream, far) = pair().await;
        let a = encode_frame(FrameType::Step, &[1]);
        let b = encode_frame(FrameType::Reset, &[2, 3]);
        let mut both = a.clone();
        both.extend_from_slice(&b);
        stream.write_all(&both).await.unwrap();
        stream.flush().await.unwrap();
        let mut buf = vec![0u8; 64];
        let (n, _) = far.recv_from(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], &a[..]);
        let (n, _) = far.recv_from(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], &b[..]);
    }

    #[tokio::test]
    async fn an_oversized_frame_is_a_typed_write_error() {
        let (mut stream, _far) = pair().await;
        let frame = encode_frame(FrameType::Step, &vec![0u8; MAX_DATAGRAM_PAYLOAD]);
        let e = stream.write_all(&frame).await.unwrap_err();
        assert!(matches!(datagram_error(&e), Some(DatagramError::TooLargeToSend { .. })), "{e}");
    }

    #[tokio::test]
    async fn reads_serve_one_frame_drop_foreign_sources_and_fail_half_frames() {
        let (mut stream, far) = pair().await;
        let near_addr = stream.local_addr().unwrap();
        let foreign = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let frame = encode_frame(FrameType::StepDone, &[9u8; 20]);

        foreign.send_to(&frame, near_addr).await.unwrap();
        far.send_to(&frame[..10], near_addr).await.unwrap();
        far.send_to(&frame, near_addr).await.unwrap();

        // The foreign datagram is skipped silently; the half frame is the first thing read.
        let mut head = [0u8; 4];
        let e = stream.read_exact(&mut head).await.unwrap_err();
        assert!(matches!(datagram_error(&e), Some(DatagramError::Truncated { got: 10, .. })), "{e}");
        // The stream is still usable: the whole frame follows.
        let mut got = vec![0u8; frame.len()];
        stream.read_exact(&mut got).await.unwrap();
        assert_eq!(got, frame);
        let (accepted, foreign_dropped, malformed, _) = stream.stats().snapshot();
        assert_eq!((accepted, foreign_dropped, malformed), (1, 1, 1));
    }
}
