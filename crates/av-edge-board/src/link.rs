//! Opening a [`PortDevice`] as the one stream type the service runs over.
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use av_edge::board::PortDevice;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use crate::serial::{SerialError, SerialPort};
use crate::udp::{UdpFrameStream, UdpStats};

/// Options that only some transports use.
#[derive(Debug, Clone, Default)]
pub struct LinkOptions {
    /// UDP only: the local address to bind (default: an ephemeral port on the wildcard
    /// address). A board with a statically configured target needs a fixed port here.
    pub udp_local: Option<SocketAddr>,
}

#[derive(Debug, thiserror::Error)]
pub enum LinkError {
    #[error(transparent)]
    Serial(#[from] SerialError),
    #[error("opening UDP link to {host}:{port}: {source}")]
    Udp { host: String, port: u16, source: io::Error },
}

/// The board's link, whichever transport it is: both are `AsyncRead + AsyncWrite` byte
/// streams of lockstep-local v1 frames.
pub enum BoardStream {
    Serial(SerialPort),
    Udp(UdpFrameStream),
}

impl BoardStream {
    /// The UDP counters, when this is a UDP link.
    pub fn udp_stats(&self) -> Option<Arc<UdpStats>> {
        match self {
            BoardStream::Udp(u) => Some(u.stats()),
            BoardStream::Serial(_) => None,
        }
    }
}

/// Open `device`. Must be called inside a tokio runtime.
pub async fn open_link(device: &PortDevice, options: &LinkOptions) -> Result<BoardStream, LinkError> {
    match device {
        PortDevice::Serial { path, baud } => Ok(BoardStream::Serial(SerialPort::open(path, *baud)?)),
        PortDevice::Udp { host, port } => UdpFrameStream::open(host, *port, options.udp_local).await.map(BoardStream::Udp).map_err(|source| LinkError::Udp { host: host.clone(), port: *port, source }),
    }
}

impl AsyncRead for BoardStream {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            BoardStream::Serial(s) => Pin::new(s).poll_read(cx, buf),
            BoardStream::Udp(u) => Pin::new(u).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for BoardStream {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            BoardStream::Serial(s) => Pin::new(s).poll_write(cx, buf),
            BoardStream::Udp(u) => Pin::new(u).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            BoardStream::Serial(s) => Pin::new(s).poll_flush(cx),
            BoardStream::Udp(u) => Pin::new(u).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            BoardStream::Serial(s) => Pin::new(s).poll_shutdown(cx),
            BoardStream::Udp(u) => Pin::new(u).poll_shutdown(cx),
        }
    }
}
