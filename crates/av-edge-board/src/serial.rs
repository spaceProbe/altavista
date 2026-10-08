//! The serial transport: a raw 8N1 tty exposed as `AsyncRead + AsyncWrite`, so
//! `av_lockstep_shim::PeerLink` runs over it unchanged.
//!
//! # Line discipline
//!
//! [`SerialPort::open`] opens the device `O_RDWR | O_NOCTTY | O_NONBLOCK | O_CLOEXEC` and
//! configures it through `libc` termios: `cfmakeraw` (no echo, no canonical mode, no
//! signals, no output processing), 8 data bits, no parity, one stop bit, no hardware flow
//! control, `CLOCAL | CREAD`, `VMIN = 1`, `VTIME = 0` (so a non-blocking read on an empty
//! line is `EAGAIN`, never an ambiguous 0), the requested baud for both directions, then
//! reads the settings back and fails if the driver did not apply the baud, 8N1 and the
//! absence of flow control it was asked for (a driver that silently picks the nearest rate
//! is a typed error, [`SerialError::NotApplied`]). Input already queued on the line is
//! flushed (`tcflush(TCIOFLUSH)`) so nothing stale reaches the handshake. The device is
//! taken exclusively with `TIOCEXCL` (macOS and Linux both have it): a second open by
//! another non-root process then fails with `EBUSY`.
//!
//! **Baud rates.** On Linux only the standard `Bxxx` constants exist, so a baud outside
//! that table is [`SerialError::UnsupportedBaud`] at open (no nearest-rate substitution);
//! the table runs from 50 to 4 000 000. On macOS (and the BSDs) `speed_t` is the rate
//! itself, so any positive rate is passed to the driver, which may refuse it
//! ([`SerialError::Termios`] or [`SerialError::NotApplied`]).
//!
//! # Paced writes (question 238's defect, and why this is unconditional)
//!
//! **Writes go out in chunks of at most [`MAX_CHUNK_BYTES`] = 64 bytes, with each chunk's
//! 8N1 wire time (10 bits per byte at the baud rate) between a chunk and the next
//! ([`wire_time`]), always.** The reason is the receiving end. The Cadence UART the board's
//! RPU uses has a 64-byte receive FIFO, and RTEMS' termios in raw mode keeps a 256-byte
//! input ring: a frame arriving faster than the guest's reader task drains it overflows the
//! ring and termios drops what does not fit. Question 238 found exactly that in the Renode
//! stand-in, whose UART model has an unbounded FIFO: a 305-byte STEP injected as one burst
//! lost 50 bytes and the guest blocked forever in `read_all`; bursts of 255 bytes or fewer
//! delivered and 64-byte bursts paced at the baud rate delivered always. A pseudo-terminal
//! feeding that Renode stand-in has no pacing of its own (a `write` of 305 bytes arrives as
//! one burst), and on real hardware the UART paces at the baud rate by itself but the
//! host's tty driver queues far more than 64 bytes ahead of it. Pacing here reproduces on
//! every path the delivery the real line gives the guest, so the same code is correct for
//! the pty stand-in and the board. The cost is latency: a 305-byte STEP takes about 26 ms
//! at 115 200 baud, which is the line's own wire time.
//!
//! The pacing is implemented in `poll_write`: a call accepts at most 64 bytes, and arms a
//! deadline `now + wire_time(accepted)`; the next `poll_write` waits for it. The deadline
//! is taken from the moment the write returned, so the gap can only be longer than the wire
//! time, never shorter. `poll_flush` does not wait out the last deadline (the next write
//! does), so a reply-waiting caller is not delayed by the line it is not using.
//!
//! # Reads
//!
//! Reads are unpaced. A hang-up (`EIO` on Linux, a 0 read on macOS) is end of file, which
//! `framing::read_frame` reports as `PeerClosed` between frames or `Truncated` inside one.
use std::ffi::CString;
use std::future::Future;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::pin::Pin;
use std::task::{ready, Context, Poll};
use std::time::Duration;

use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::time::{Instant, Sleep};

/// The most bytes one `poll_write` accepts: the real Cadence UART receive FIFO depth.
pub const MAX_CHUNK_BYTES: usize = 64;

/// Bits on the wire per byte for 8N1: start bit, eight data bits, stop bit.
pub const BITS_PER_BYTE: u64 = 10;

/// How long `bytes` bytes occupy an 8N1 line at `baud`: `bytes * 10 / baud` seconds,
/// rounded up to a whole nanosecond.
pub fn wire_time(bytes: usize, baud: u32) -> Duration {
    let bits = bytes as u128 * BITS_PER_BYTE as u128;
    let nanos = (bits * 1_000_000_000).div_ceil(baud.max(1) as u128);
    Duration::from_nanos(nanos as u64)
}

/// Everything that can go wrong opening and configuring the serial device.
#[derive(Debug, thiserror::Error)]
pub enum SerialError {
    #[error("opening serial device {path}: {source}")]
    Open { path: String, source: io::Error },
    #[error("{path} is not a terminal device (tcgetattr: {source})")]
    NotATty { path: String, source: io::Error },
    #[error("baud {baud} is not supported on this platform: {reason}")]
    UnsupportedBaud { baud: u32, reason: String },
    #[error("{op} on {path}: {source}")]
    Termios { path: String, op: &'static str, source: io::Error },
    #[error("taking {path} exclusively (TIOCEXCL): {source}")]
    Exclusive { path: String, source: io::Error },
    #[error("the driver for {path} did not apply the requested line settings: {what}")]
    NotApplied { path: String, what: String },
    #[error("registering {path} with the async reactor: {source}")]
    Register { path: String, source: io::Error },
    #[error("device path {path:?} contains a NUL byte")]
    NulInPath { path: String },
}

/// The `speed_t` for `baud` on this platform, or why there is none.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub fn speed_for(baud: u32) -> Result<libc::speed_t, SerialError> {
    let speed = match baud {
        50 => libc::B50,
        75 => libc::B75,
        110 => libc::B110,
        134 => libc::B134,
        150 => libc::B150,
        200 => libc::B200,
        300 => libc::B300,
        600 => libc::B600,
        1200 => libc::B1200,
        1800 => libc::B1800,
        2400 => libc::B2400,
        4800 => libc::B4800,
        9600 => libc::B9600,
        19200 => libc::B19200,
        38400 => libc::B38400,
        57600 => libc::B57600,
        115200 => libc::B115200,
        230400 => libc::B230400,
        460800 => libc::B460800,
        500000 => libc::B500000,
        576000 => libc::B576000,
        921600 => libc::B921600,
        1000000 => libc::B1000000,
        1152000 => libc::B1152000,
        1500000 => libc::B1500000,
        2000000 => libc::B2000000,
        2500000 => libc::B2500000,
        3000000 => libc::B3000000,
        3500000 => libc::B3500000,
        4000000 => libc::B4000000,
        _ => {
            return Err(SerialError::UnsupportedBaud {
                baud,
                reason: "Linux termios has only the standard Bxxx rates (50, 75, 110, 134, 150, 200, 300, 600, 1200, 1800, 2400, 4800, 9600, 19200, 38400, 57600, 115200, 230400, 460800, 500000, 576000, 921600, 1000000, 1152000, 1500000, 2000000, 2500000, 3000000, 3500000, 4000000); no nearest rate is substituted".to_string(),
            })
        }
    };
    Ok(speed)
}

/// The `speed_t` for `baud` on this platform: on macOS and the BSDs `speed_t` is the rate
/// itself, and the driver decides whether it can honour it.
#[cfg(not(any(target_os = "linux", target_os = "android")))]
pub fn speed_for(baud: u32) -> Result<libc::speed_t, SerialError> {
    Ok(baud as libc::speed_t)
}

fn last_os_error() -> io::Error {
    io::Error::last_os_error()
}

/// Open and configure `path` as a raw 8N1 line at `baud`, returning the owned descriptor.
fn open_configured(path: &str, baud: u32) -> Result<OwnedFd, SerialError> {
    let speed = speed_for(baud)?;
    let c_path = CString::new(path).map_err(|_| SerialError::NulInPath { path: path.to_string() })?;
    // SAFETY: `c_path` is a valid NUL-terminated string; the flags are plain integers.
    let raw = unsafe { libc::open(c_path.as_ptr(), libc::O_RDWR | libc::O_NOCTTY | libc::O_NONBLOCK | libc::O_CLOEXEC) };
    if raw < 0 {
        return Err(SerialError::Open { path: path.to_string(), source: last_os_error() });
    }
    // SAFETY: `raw` is a freshly opened descriptor that nothing else owns.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    let raw = fd.as_raw_fd();

    // SAFETY: an all-zero `termios` is a valid out-parameter for `tcgetattr`.
    let mut tio: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY: `raw` is an open descriptor and `tio` a valid out-parameter.
    if unsafe { libc::tcgetattr(raw, &mut tio) } != 0 {
        return Err(SerialError::NotATty { path: path.to_string(), source: last_os_error() });
    }
    // SAFETY: `tio` is a valid, initialised termios.
    unsafe { libc::cfmakeraw(&mut tio) };
    tio.c_cflag |= libc::CLOCAL | libc::CREAD;
    tio.c_cflag &= !(libc::CSTOPB | libc::PARENB | libc::PARODD | libc::CRTSCTS);
    tio.c_cflag = (tio.c_cflag & !libc::CSIZE) | libc::CS8;
    tio.c_cc[libc::VMIN] = 1;
    tio.c_cc[libc::VTIME] = 0;
    let termios_err = |op: &'static str| SerialError::Termios { path: path.to_string(), op, source: last_os_error() };
    // SAFETY: `tio` is valid; the speeds are values `speed_for` produced.
    if unsafe { libc::cfsetispeed(&mut tio, speed) } != 0 {
        return Err(termios_err("cfsetispeed"));
    }
    // SAFETY: as above.
    if unsafe { libc::cfsetospeed(&mut tio, speed) } != 0 {
        return Err(termios_err("cfsetospeed"));
    }
    // SAFETY: `raw` is open and `tio` is fully initialised.
    if unsafe { libc::tcsetattr(raw, libc::TCSANOW, &tio) } != 0 {
        return Err(termios_err("tcsetattr"));
    }

    // Read the settings back: tcsetattr succeeds on Linux when it applied only some of them.
    // SAFETY: as for the first tcgetattr.
    let mut got: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY: `raw` is open and `got` is a valid out-parameter.
    if unsafe { libc::tcgetattr(raw, &mut got) } != 0 {
        return Err(termios_err("tcgetattr (read-back)"));
    }
    // SAFETY: `got` is initialised by the successful tcgetattr above.
    let (ispeed, ospeed) = unsafe { (libc::cfgetispeed(&got), libc::cfgetospeed(&got)) };
    let not_applied = |what: String| SerialError::NotApplied { path: path.to_string(), what };
    if ispeed != speed || ospeed != speed {
        return Err(not_applied(format!("asked for baud {baud}, the driver reports input speed {ispeed} and output speed {ospeed}")));
    }
    if got.c_cflag & libc::CSIZE != libc::CS8 {
        return Err(not_applied("not 8 data bits".to_string()));
    }
    if got.c_cflag & (libc::PARENB | libc::CSTOPB | libc::CRTSCTS) != 0 {
        return Err(not_applied("parity, two stop bits or hardware flow control is still enabled".to_string()));
    }

    // SAFETY: `raw` is open; TIOCEXCL takes no argument.
    if unsafe { libc::ioctl(raw, libc::TIOCEXCL as _) } != 0 {
        return Err(SerialError::Exclusive { path: path.to_string(), source: last_os_error() });
    }
    // Discard anything queued before this point so it cannot reach the handshake.
    // SAFETY: `raw` is open.
    if unsafe { libc::tcflush(raw, libc::TCIOFLUSH) } != 0 {
        return Err(termios_err("tcflush"));
    }
    Ok(fd)
}

/// A configured serial line. See the module doc for the line settings and the paced writes.
pub struct SerialPort {
    fd: AsyncFd<OwnedFd>,
    baud: u32,
    next_write_at: Instant,
    gate: Pin<Box<Sleep>>,
}

impl SerialPort {
    /// Open `path` at `baud`, 8N1 raw, exclusively. Must be called inside a tokio runtime.
    pub fn open(path: &str, baud: u32) -> Result<Self, SerialError> {
        let fd = open_configured(path, baud)?;
        let fd = AsyncFd::new(fd).map_err(|source| SerialError::Register { path: path.to_string(), source })?;
        let now = Instant::now();
        Ok(Self { fd, baud, next_write_at: now, gate: Box::pin(tokio::time::sleep_until(now)) })
    }

    pub fn baud(&self) -> u32 {
        self.baud
    }
}

impl AsyncRead for SerialPort {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        loop {
            let mut guard = ready!(this.fd.poll_read_ready(cx))?;
            let unfilled = buf.initialize_unfilled();
            let outcome = guard.try_io(|inner| {
                // SAFETY: `unfilled` is a valid writable buffer of the given length.
                let n = unsafe { libc::read(inner.get_ref().as_raw_fd(), unfilled.as_mut_ptr().cast(), unfilled.len()) };
                if n < 0 {
                    Err(last_os_error())
                } else {
                    Ok(n as usize)
                }
            });
            match outcome {
                Ok(Ok(n)) => {
                    buf.advance(n);
                    return Poll::Ready(Ok(()));
                }
                // A hang-up (the master closed, the USB adapter went away) reads as EIO on Linux.
                Ok(Err(e)) if e.raw_os_error() == Some(libc::EIO) => return Poll::Ready(Ok(())),
                Ok(Err(e)) => return Poll::Ready(Err(e)),
                Err(_would_block) => continue,
            }
        }
    }
}

impl AsyncWrite for SerialPort {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }
        // Wait out the previous chunk's wire time.
        if this.gate.deadline() != this.next_write_at {
            this.gate.as_mut().reset(this.next_write_at);
        }
        ready!(this.gate.as_mut().poll(cx));
        let chunk = &buf[..buf.len().min(MAX_CHUNK_BYTES)];
        loop {
            let mut guard = ready!(this.fd.poll_write_ready(cx))?;
            let outcome = guard.try_io(|inner| {
                // SAFETY: `chunk` is a valid readable buffer of the given length.
                let n = unsafe { libc::write(inner.get_ref().as_raw_fd(), chunk.as_ptr().cast(), chunk.len()) };
                if n < 0 {
                    Err(last_os_error())
                } else {
                    Ok(n as usize)
                }
            });
            match outcome {
                Ok(Ok(n)) => {
                    this.next_write_at = Instant::now() + wire_time(n, this.baud);
                    return Poll::Ready(Ok(n));
                }
                Ok(Err(e)) => return Poll::Ready(Err(e)),
                Err(_would_block) => continue,
            }
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_time_is_ten_bits_per_byte() {
        // 64 bytes at 115200: 640 bits / 115200 = 5.5555... ms, rounded up to a nanosecond.
        assert_eq!(wire_time(64, 115_200), Duration::from_nanos(5_555_556));
        assert_eq!(wire_time(1, 9600), Duration::from_nanos(1_041_667));
        assert_eq!(wire_time(0, 115_200), Duration::ZERO);
        assert_eq!(wire_time(10, 1_000_000), Duration::from_micros(100));
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn linux_refuses_a_non_standard_baud_instead_of_rounding() {
        assert!(speed_for(115_200).is_ok());
        assert!(speed_for(4_000_000).is_ok());
        for baud in [1, 115_201, 100_000, 250_000, 4_000_001] {
            assert!(matches!(speed_for(baud), Err(SerialError::UnsupportedBaud { baud: b, .. }) if b == baud), "{baud}");
        }
    }

    #[test]
    fn a_missing_device_and_a_non_tty_are_typed_errors() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let _guard = rt.enter();
        assert!(matches!(SerialPort::open("/dev/does-not-exist-av", 115_200), Err(SerialError::Open { .. })));
        assert!(matches!(SerialPort::open("/dev/null", 115_200), Err(SerialError::NotATty { .. })));
        assert!(matches!(SerialPort::open("/dev/nu\0ll", 115_200), Err(SerialError::NulInPath { .. })));
    }
}
