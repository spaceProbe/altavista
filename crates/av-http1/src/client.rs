//! [`Client`]: connect, (TLS handshake,) write one request, read one response, close.

use std::io::{self, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use openssl::ssl::{ErrorCode, HandshakeError, SslConnector, SslStream};
use openssl::x509::verify::X509CheckFlags;

use crate::endpoint::Endpoint;
use crate::error::{HttpError, Phase};
use crate::message::{Request, Response};
use crate::tls::TlsConfig;
use crate::wire::{read_response, Limits, Source};

/// Socket timeouts. All three must be non-zero (a zero timeout is refused by `Client::new`).
/// `read` and `write` bound each individual socket operation, not the whole exchange: a peer that
/// keeps trickling bytes inside the timeout is stopped by the body-size limit, not by the clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeouts {
    /// `TcpStream::connect_timeout`, applied to each resolved address in turn. (DNS resolution
    /// itself has no timeout in `std`.)
    pub connect: Duration,
    /// Per socket read (also bounds each step of the TLS handshake).
    pub read: Duration,
    /// Per socket write.
    pub write: Duration,
}

impl Timeouts {
    /// The same duration for connect, read and write.
    pub fn uniform(d: Duration) -> Timeouts {
        Timeouts {
            connect: d,
            read: d,
            write: d,
        }
    }
}

impl Default for Timeouts {
    /// 10 s to connect, 30 s per read, 30 s per write.
    fn default() -> Self {
        Timeouts {
            connect: Duration::from_secs(10),
            read: Duration::from_secs(30),
            write: Duration::from_secs(30),
        }
    }
}

/// Response size limits. Exceeding one is [`HttpError::HeaderTooLarge`] / [`HttpError::BodyTooLarge`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SizeLimits {
    /// The status line and headers (all 1xx heads that were skipped count too), and, separately,
    /// the chunk trailers. Default 64 KiB.
    pub max_header_bytes: usize,
    /// The decoded body. Default 64 MiB.
    pub max_body_bytes: usize,
}

impl Default for SizeLimits {
    fn default() -> Self {
        SizeLimits {
            max_header_bytes: 64 * 1024,
            max_body_bytes: 64 * 1024 * 1024,
        }
    }
}

/// A blocking HTTP/1.1 client for one [`Endpoint`]. Cheap to share (`Send + Sync`); every
/// [`Client::request`] opens its own connection.
#[derive(Debug, Clone)]
pub struct Client {
    endpoint: Endpoint,
    connector: Option<SslConnector>,
    timeouts: Timeouts,
    limits: SizeLimits,
}

impl Client {
    /// Builds a client. For an `https://` endpoint the TLS configuration is loaded now (so a bad
    /// PEM fails here, not on first use); `None` means [`TlsConfig::default`] (system store, no
    /// client identity). Supplying a `TlsConfig` for an `http://` endpoint is refused rather than
    /// silently ignored: a caller who configured a client identity expects TLS.
    pub fn new(endpoint: Endpoint, tls: Option<TlsConfig>, timeouts: Timeouts) -> Result<Client, HttpError> {
        if timeouts.connect.is_zero() || timeouts.read.is_zero() || timeouts.write.is_zero() {
            return Err(HttpError::InvalidRequest("timeouts must be non-zero".into()));
        }
        let connector = if endpoint.is_tls() {
            Some(tls.unwrap_or_default().build_connector()?)
        } else if tls.is_some() {
            return Err(HttpError::InvalidTlsConfig(format!(
                "a TlsConfig was given for the plaintext endpoint {}",
                endpoint.authority()
            )));
        } else {
            None
        };
        Ok(Client {
            endpoint,
            connector,
            timeouts,
            limits: SizeLimits::default(),
        })
    }

    /// Replaces the response size limits.
    pub fn with_limits(mut self, limits: SizeLimits) -> Client {
        self.limits = limits;
        self
    }

    /// The endpoint this client talks to.
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Sends one request on a fresh connection (`Connection: close`) and returns the response.
    /// A non-2xx status is a normal `Ok(Response)`. The request is validated before anything is
    /// connected, so an injection attempt never reaches a socket.
    pub fn request(&self, request: Request) -> Result<Response, HttpError> {
        request.validate()?;
        let addr = self.endpoint.authority();
        let mut conn = self.connect()?;

        let bytes = self.encode_head(&request);
        conn.write_all(&bytes, &addr)?;
        conn.write_all(&request.body, &addr)?;
        conn.flush(&addr)?;

        let limits = Limits {
            max_header_bytes: self.limits.max_header_bytes,
            max_body_bytes: self.limits.max_body_bytes,
        };
        let is_head = request.method.eq_ignore_ascii_case("HEAD");
        let mut source = ConnSource { conn: &mut conn, addr };
        let response = read_response(&mut source, is_head, limits);
        conn.close();
        response
    }

    /// The request line and headers. The client owns `Host`, `Content-Length` and `Connection`.
    fn encode_head(&self, req: &Request) -> Vec<u8> {
        let mut head = format!(
            "{} {}{} HTTP/1.1\r\nHost: {}\r\n",
            req.method,
            self.endpoint.base_path(),
            req.target,
            self.endpoint.host_header()
        );
        for (name, value) in &req.headers {
            head.push_str(name);
            head.push_str(": ");
            head.push_str(value);
            head.push_str("\r\n");
        }
        let has_payload_method = ["POST", "PUT", "PATCH"].iter().any(|m| req.method.eq_ignore_ascii_case(m));
        if !req.body.is_empty() || has_payload_method {
            head.push_str(&format!("Content-Length: {}\r\n", req.body.len()));
        }
        head.push_str("Connection: close\r\n\r\n");
        head.into_bytes()
    }

    fn connect(&self) -> Result<Conn, HttpError> {
        let tcp = self.connect_tcp()?;
        let Some(connector) = &self.connector else {
            return Ok(Conn::Plain(tcp));
        };
        let addr = self.endpoint.authority();
        let tls_err = |reason: String| HttpError::Tls {
            addr: addr.clone(),
            reason,
        };
        // ConnectConfiguration::into_ssl verifies the host against the certificate (an IP
        // literal against the IP SANs via set_ip, a DNS name against the DNS SANs via set_host)
        // and sends SNI for DNS names only. verify_hostname is left at its default of true;
        // nothing in this crate turns it off. On top of that default, the certificate subject's
        // CN is never consulted (RFC 6125): without NEVER_CHECK_SUBJECT, OpenSSL falls back to
        // the CN for a certificate that has no DNS SAN at all.
        let host = self.endpoint.host_text();
        let mut ssl = connector
            .configure()
            .and_then(|config| config.into_ssl(&host))
            .map_err(|e| tls_err(e.to_string()))?;
        ssl.param_mut()
            .set_hostflags(X509CheckFlags::NO_PARTIAL_WILDCARDS | X509CheckFlags::NEVER_CHECK_SUBJECT);
        match ssl.connect(tcp) {
            Ok(stream) => Ok(Conn::Tls {
                stream: Box::new(stream),
                unclean_eof: false,
            }),
            Err(HandshakeError::SetupFailure(e)) => Err(tls_err(e.to_string())),
            Err(HandshakeError::WouldBlock(_)) => Err(HttpError::Timeout {
                phase: Phase::TlsHandshake,
                addr,
            }),
            Err(HandshakeError::Failure(mid)) => {
                let verify = mid.ssl().verify_result();
                if let Some(io) = mid.error().io_error() {
                    if is_timeout(io) {
                        return Err(HttpError::Timeout {
                            phase: Phase::TlsHandshake,
                            addr,
                        });
                    }
                    return Err(HttpError::Io {
                        phase: Phase::TlsHandshake,
                        addr,
                        source: io::Error::new(io.kind(), io.to_string()),
                    });
                }
                let mut reason = mid.error().to_string();
                if verify != openssl::x509::X509VerifyResult::OK {
                    reason.push_str(&format!("; certificate verification: {verify}"));
                }
                Err(tls_err(reason))
            }
        }
    }

    /// Tries each resolved address with the connect timeout.
    fn connect_tcp(&self) -> Result<TcpStream, HttpError> {
        let authority = self.endpoint.authority();
        let addrs = self.endpoint.socket_addrs().map_err(|source| HttpError::Unreachable {
            addr: authority.clone(),
            source,
        })?;
        let mut last: Option<(SocketAddr, io::Error)> = None;
        for a in addrs {
            match TcpStream::connect_timeout(&a, self.timeouts.connect) {
                Ok(tcp) => {
                    tcp.set_nodelay(true).ok();
                    tcp.set_read_timeout(Some(self.timeouts.read))
                        .and_then(|()| tcp.set_write_timeout(Some(self.timeouts.write)))
                        .map_err(|source| HttpError::Io {
                            phase: Phase::Connect,
                            addr: authority.clone(),
                            source,
                        })?;
                    return Ok(tcp);
                }
                Err(e) => last = Some((a, e)),
            }
        }
        Err(match last {
            Some((a, e)) if is_timeout(&e) => HttpError::Timeout {
                phase: Phase::Connect,
                addr: a.to_string(),
            },
            Some((a, source)) => HttpError::Unreachable {
                addr: a.to_string(),
                source,
            },
            None => HttpError::Unreachable {
                addr: authority,
                source: io::Error::new(io::ErrorKind::NotFound, "the name resolved to no addresses"),
            },
        })
    }
}

fn is_timeout(e: &io::Error) -> bool {
    matches!(e.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock)
}

/// One open connection, plaintext or TLS.
enum Conn {
    Plain(TcpStream),
    Tls {
        stream: Box<SslStream<TcpStream>>,
        /// Set when a read ended without close_notify.
        unclean_eof: bool,
    },
}

impl Conn {
    fn write_all(&mut self, bytes: &[u8], addr: &str) -> Result<(), HttpError> {
        let r = match self {
            Conn::Plain(s) => s.write_all(bytes),
            Conn::Tls { stream, .. } => stream.write_all(bytes),
        };
        r.map_err(|e| map_io(e, Phase::Write, addr))
    }

    fn flush(&mut self, addr: &str) -> Result<(), HttpError> {
        let r = match self {
            Conn::Plain(s) => s.flush(),
            Conn::Tls { stream, .. } => stream.flush(),
        };
        r.map_err(|e| map_io(e, Phase::Write, addr))
    }

    /// Best-effort polite close (TLS close_notify); errors are irrelevant after the response.
    fn close(&mut self) {
        if let Conn::Tls { stream, .. } = self {
            let _ = stream.shutdown();
        }
    }
}

/// Maps an `io::Error` from the stream: timeouts, TLS errors that OpenSSL wrapped into an
/// `io::Error`, and the rest.
fn map_io(e: io::Error, phase: Phase, addr: &str) -> HttpError {
    if is_timeout(&e) {
        return HttpError::Timeout {
            phase,
            addr: addr.to_string(),
        };
    }
    if let Some(ssl) = e.get_ref().and_then(|inner| inner.downcast_ref::<openssl::ssl::Error>()) {
        return HttpError::Tls {
            addr: addr.to_string(),
            reason: ssl.to_string(),
        };
    }
    HttpError::Io {
        phase,
        addr: addr.to_string(),
        source: e,
    }
}

struct ConnSource<'a> {
    conn: &'a mut Conn,
    addr: String,
}

impl Source for ConnSource<'_> {
    fn read_some(&mut self, buf: &mut [u8]) -> Result<usize, HttpError> {
        match self.conn {
            Conn::Plain(s) => io::Read::read(s, buf).map_err(|e| map_io(e, Phase::Read, &self.addr)),
            Conn::Tls { stream, unclean_eof } => loop {
                match stream.ssl_read(buf) {
                    Ok(n) => return Ok(n),
                    Err(e) => match e.code() {
                        ErrorCode::ZERO_RETURN => return Ok(0),
                        ErrorCode::WANT_READ | ErrorCode::WANT_WRITE if e.io_error().is_none() => continue,
                        // OpenSSL 1.1: EOF without close_notify, no error queued.
                        ErrorCode::SYSCALL if e.io_error().is_none() => {
                            *unclean_eof = true;
                            return Ok(0);
                        }
                        _ => {
                            if let Some(io) = e.io_error() {
                                return Err(map_io(io::Error::new(io.kind(), io.to_string()), Phase::Read, &self.addr));
                            }
                            // OpenSSL 3: EOF without close_notify is an SSL error with this reason.
                            let eof = e
                                .ssl_error()
                                .is_some_and(|st| st.errors().iter().any(|x| x.reason().is_some_and(|r| r.contains("unexpected eof"))));
                            if eof {
                                *unclean_eof = true;
                                return Ok(0);
                            }
                            return Err(HttpError::Tls {
                                addr: self.addr.clone(),
                                reason: e.to_string(),
                            });
                        }
                    },
                }
            },
        }
    }

    fn unclean_eof(&self) -> bool {
        matches!(self.conn, Conn::Tls { unclean_eof: true, .. })
    }
}
