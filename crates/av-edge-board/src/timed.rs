//! [`TimedStream`]: a transparent `AsyncRead + AsyncWrite` wrapper that remembers, in wall-clock
//! Unix nanoseconds, when the link last accepted bytes for writing and when it last delivered
//! bytes for reading. The board I/O log (`iolog`) stamps each record with the values after
//! an exchange: `request_written_unix_ns` is the last moment the request (all of it) had been
//! handed to the link, `response_read_unix_ns` the last moment a response byte was read.
//!
//! `PeerLink` hides its own reads and writes, so the stream is the only place these instants
//! can be taken. Wrapping adds no behaviour to the link: bytes pass through untouched.
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// Wall-clock now, Unix nanoseconds (0 if the clock is before the epoch).
pub fn unix_ns_now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as i64).unwrap_or(0)
}

/// The last write and read instants seen on a [`TimedStream`]; 0 means none since [`reset`](Self::reset).
#[derive(Debug, Default)]
pub struct LinkTimes {
    last_write_unix_ns: AtomicI64,
    last_read_unix_ns: AtomicI64,
}

impl LinkTimes {
    pub fn reset(&self) {
        self.last_write_unix_ns.store(0, Ordering::SeqCst);
        self.last_read_unix_ns.store(0, Ordering::SeqCst);
    }

    /// `(last write, last read)`, Unix ns.
    pub fn snapshot(&self) -> (i64, i64) {
        (self.last_write_unix_ns.load(Ordering::SeqCst), self.last_read_unix_ns.load(Ordering::SeqCst))
    }
}

pub struct TimedStream<S> {
    inner: S,
    times: Arc<LinkTimes>,
}

impl<S> TimedStream<S> {
    pub fn new(inner: S, times: Arc<LinkTimes>) -> Self {
        Self { inner, times }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for TimedStream<S> {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let r = Pin::new(&mut self.inner).poll_read(cx, buf);
        if matches!(r, Poll::Ready(Ok(()))) && buf.filled().len() > before {
            self.times.last_read_unix_ns.store(unix_ns_now(), Ordering::SeqCst);
        }
        r
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for TimedStream<S> {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        let r = Pin::new(&mut self.inner).poll_write(cx, buf);
        if matches!(r, Poll::Ready(Ok(n)) if n > 0) {
            self.times.last_write_unix_ns.store(unix_ns_now(), Ordering::SeqCst);
        }
        r
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let r = Pin::new(&mut self.inner).poll_flush(cx);
        if matches!(r, Poll::Ready(Ok(()))) && self.times.last_write_unix_ns.load(Ordering::SeqCst) != 0 {
            self.times.last_write_unix_ns.store(unix_ns_now(), Ordering::SeqCst);
        }
        r
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}
