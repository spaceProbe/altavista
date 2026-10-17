//! `av-http1`: a small blocking HTTP/1.1 client over the system OpenSSL (question 245; the
//! hand-rolled-client rule is question 218, the plaintext-only-on-loopback rule question 155).
//!
//! Two clients in this repository speak HTTP: the ClickHouse client (its HTTP interface) for the
//! sweep study store, and the Kubernetes API client for the `av-jobs` Jobs executor. Both are
//! synchronous (`StudyStore` and `av_jobs::runner::Executor` are sync traits, and `av-jobs`
//! deliberately has no async runtime), and both need exactly the same transport, so it lives here
//! once rather than twice. The dependency set is `openssl` and `thiserror`: no `hyper`, no
//! `tokio`, no `url`, no `base64`, and no crate that bundles its own crypto.
//!
//! # Shape
//!
//! ```no_run
//! use av_http1::{Client, Endpoint, Request, Timeouts, TlsConfig};
//!
//! let endpoint = Endpoint::parse("https://10.0.0.5:6443/base")?;
//! let tls = TlsConfig::new().with_ca_pem(std::fs::read("ca.pem")?);
//! let client = Client::new(endpoint, Some(tls), Timeouts::default())?;
//! let response = client.request(Request::get("/version").header("Accept", "application/json"))?;
//! assert!(response.is_success()); // a non-2xx status is a Response, not an error
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # One connection per request
//!
//! Every [`Client::request`] opens a TCP connection (and TLS session), sends `Connection: close`,
//! reads one response and closes. No pooling, no keep-alive, no pipelining: simpler to get right,
//! and enough for both callers (a ClickHouse insert or query and a Kubernetes API call each cost
//! far more than a loopback or LAN handshake). The client always writes `Host`, `Content-Length`
//! (for a body or a POST/PUT/PATCH) and `Connection`; a caller that supplies any of those, or
//! `Transfer-Encoding`, gets [`HttpError::InvalidRequest`].
//!
//! # What is refused, before any byte is sent
//!
//! A plaintext `http://` endpoint whose host is not loopback ([`HttpError::PlaintextNonLoopback`],
//! raised by [`Endpoint::parse`]); a CR, LF or NUL in a method, path or header; a path with a
//! space or `#`; a malformed endpoint ([`HttpError::InvalidEndpoint`]); a bad [`TlsConfig`]
//! ([`HttpError::InvalidTlsConfig`], raised by [`Client::new`]).
//!
//! # Response framing
//!
//! `Transfer-Encoding: chunked` (extensions ignored, trailers read and discarded),
//! `Content-Length`, and read-until-close when neither is present. Both present is
//! [`HttpError::Protocol`]. Replies to `HEAD`, and `204` / `304`, have no body. 1xx interim
//! responses (`100 Continue`, `102`, `103`) are skipped; `101` is a protocol error because the
//! client never asks to upgrade. A response delimited only by the connection closing, over TLS,
//! must end with a close_notify, otherwise it is [`HttpError::Protocol`] (truncation would be
//! undetectable). The head is limited by [`SizeLimits::max_header_bytes`], the decoded body by
//! [`SizeLimits::max_body_bytes`].
//!
//! # Not here
//!
//! No redirects (a 3xx is returned as is), no cookies, no content decoding (`gzip` is never
//! requested and a `Transfer-Encoding` other than `chunked` is refused), no HTTP/2, no proxies,
//! no overall request deadline (timeouts are per socket operation), and no retry: callers own
//! retry policy.

mod client;
mod endpoint;
mod error;
mod message;
mod tls;
mod wire;

pub use client::{Client, SizeLimits, Timeouts};
pub use endpoint::{Endpoint, Host, Scheme};
pub use error::{EndpointProblem, HttpError, Phase};
pub use message::{Request, Response};
pub use tls::TlsConfig;
