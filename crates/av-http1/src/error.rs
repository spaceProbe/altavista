//! The typed error of this crate. A non-2xx status is **not** an error here: it comes back as a
//! [`crate::Response`] and the caller (the ClickHouse client, the Kubernetes client) types it.

use std::fmt;
use std::io;

/// The phase of a request a [`HttpError::Timeout`] expired in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// `TcpStream::connect_timeout` to one resolved address.
    Connect,
    /// The TLS handshake (it runs on the socket's read/write timeouts).
    TlsHandshake,
    /// Writing the request.
    Write,
    /// Waiting for, or reading, the response.
    Read,
}

impl fmt::Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Phase::Connect => "connect",
            Phase::TlsHandshake => "TLS handshake",
            Phase::Write => "write",
            Phase::Read => "read",
        })
    }
}

/// Why an [`crate::Endpoint`] URL was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndpointProblem {
    /// Not `http://` or `https://` (the scheme as written is carried).
    UnsupportedScheme(String),
    /// A `user[:password]@` part. Credentials never travel in a URL; callers pass them as headers.
    Userinfo,
    /// The authority (host and optional port) is malformed; the reason says how.
    MalformedAuthority(&'static str),
    /// A query (`?`) or fragment (`#`) in the base URL.
    QueryOrFragment,
    /// The base path holds a control character, whitespace, a backslash or non-ASCII bytes.
    BadBasePath,
}

impl fmt::Display for EndpointProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EndpointProblem::UnsupportedScheme(s) => {
                write!(f, "unsupported scheme {s:?} (only http and https)")
            }
            EndpointProblem::Userinfo => f.write_str("userinfo (user:password@) is not accepted in an endpoint URL"),
            EndpointProblem::MalformedAuthority(why) => write!(f, "malformed authority: {why}"),
            EndpointProblem::QueryOrFragment => f.write_str("a query or fragment is not accepted in an endpoint URL"),
            EndpointProblem::BadBasePath => f.write_str("the base path holds a character that is not allowed"),
        }
    }
}

/// Everything this crate can refuse or fail with.
#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    /// Connect refused, no route, or DNS failure; `addr` is the `host:port` that was tried.
    #[error("cannot reach {addr}: {source}")]
    Unreachable {
        addr: String,
        #[source]
        source: io::Error,
    },
    /// A timeout expired; `phase` says which, `addr` is the endpoint.
    #[error("timed out ({phase}) talking to {addr}")]
    Timeout { phase: Phase, addr: String },
    /// A TLS handshake or verification failure, or a TLS alert mid-stream; `reason` is
    /// OpenSSL's own text (for example `certificate verify failed`).
    #[error("TLS error talking to {addr}: {reason}")]
    Tls { addr: String, reason: String },
    /// The [`crate::TlsConfig`] could not be loaded (bad PEM, a key that does not match its
    /// certificate, an unreadable CA file). Raised by `Client::new`, before any connection.
    #[error("invalid TLS configuration: {0}")]
    InvalidTlsConfig(String),
    /// Question 155: a plaintext `http://` endpoint whose host is not a loopback literal or
    /// `localhost`. DNS is never consulted to decide this.
    #[error("plaintext http:// to non-loopback host {host:?} is refused (question 155); use https://")]
    PlaintextNonLoopback { host: String },
    /// The endpoint URL was refused; see [`EndpointProblem`].
    #[error("invalid endpoint {url:?}: {problem}")]
    InvalidEndpoint { url: String, problem: EndpointProblem },
    /// The request was refused before a byte was sent (CR, LF or NUL in a method, path or
    /// header; a header the client owns; a malformed path).
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    /// The response is not valid HTTP/1.x (malformed status line or header, bad chunk size,
    /// truncated body, both `Content-Length` and `Transfer-Encoding`, ...).
    #[error("protocol error: {0}")]
    Protocol(String),
    /// The response head (status line and headers, together with any skipped 1xx heads) exceeded
    /// `limit` bytes, or the chunk trailers did (each has the same allowance).
    #[error("response header section larger than the {limit}-byte limit")]
    HeaderTooLarge { limit: usize },
    /// The response body exceeded `limit` bytes.
    #[error("response body larger than the {limit}-byte limit")]
    BodyTooLarge { limit: usize },
    /// Any other I/O failure; `phase` says where.
    #[error("I/O error ({phase}) talking to {addr}: {source}")]
    Io {
        phase: Phase,
        addr: String,
        #[source]
        source: io::Error,
    },
}
