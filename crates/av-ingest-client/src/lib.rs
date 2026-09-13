//! A `tonic` client for `altavista.v1.EdgeIngest` (question 202, E3b's charter) --
//! `crates/av-ingest`'s server, and E4's plugin binary's own eventual counterpart.
//!
//! # Why this crate exists separately, and why its own `protoc`/`tonic-build` pass
//!
//! Mirrors `crates/av-lockstep`'s own module doc almost exactly ("a separate crate rather
//! than blurring an existing client's stated scope"), with one deliberate deviation:
//! `crates/av-lockstep` depends on `crates/av-grpc` and reuses its already-compiled `pb`
//! module directly, because `av-grpc/build.rs` specifically `extern_path`s
//! `.altavista.v1.Port`/`.altavista.v1.PortMessage` onto `av_cdm::pb` (the one cross-
//! cutting reuse `av-lockstep` actually needs) -- but it does **not** extern the *whole*
//! `altavista.v1` package, so `av_grpc::pb::MeasurementBatch`/`PluginManifest`/etc. would
//! be a second, independently-generated (if wire-identical) Rust type from `av_cdm::pb`'s,
//! and every call site in this crate and its tests would need to convert between the two.
//! This crate instead runs its own small `tonic-build` pass (`build.rs`) with
//! `.extern_path(".altavista.v1", "::av_cdm::pb")` -- the exact convention `crates/
//! av-dynamics-service` and `crates/av-ingest` (this client's own server-side
//! counterpart) already use -- so every message this crate's methods take or return
//! **is** `av_edge::pb::X` (== `av_cdm::pb::X`), with zero conversion anywhere.
//!
//! # Plaintext, loopback only
//!
//! [`EdgeIngestClient::connect_plaintext`] speaks plain HTTP/2 (h2c), no TLS at all --
//! **local-subprocess/no-front tests only** (`crates/av-lockstep`'s own module doc, "this
//! path is for local-subprocess tests only", applies verbatim here) -- and refuses a
//! non-loopback endpoint before ever attempting to connect, mirroring `crates/av-ingest::
//! server::bind_loopback`'s own refusal for the listening side of the exact same rule
//! (question 155/202). A real cross-host plugin dials a service-owned nginx mTLS front
//! instead (not built by this crate this round -- see `docs/edge-plan.md`); this client
//! intentionally offers no `connect_mtls` yet, since nothing in this task's own scope
//! calls one (E4's plugin binary is a later task, and `crates/av-grpc::tls::connect` is
//! already exactly the right building block for it when that task needs one).
//!
//! # The forwarded-client-certificate header, from the client side
//!
//! [`forwarded_cert::FORWARDED_CLIENT_CERT_HEADER`] names the same literal
//! `crate::av_ingest::forwarded_cert::FORWARDED_CLIENT_CERT_HEADER` does (that crate's own
//! module doc is the canonical definition and escaping contract; this crate cannot depend
//! on it without a build-graph cycle, since `av-ingest`'s own tests depend on this crate --
//! see `crates/av-ingest/tests/forwarded_cert_header_name_agreement.rs` for the cross-crate agreement check).
//! [`EdgeIngestClient::set_forwarded_client_cert`] sets it on every subsequent RPC this
//! client makes, standing in for what a real nginx front would set -- see
//! [`forwarded_cert::percent_encode_pem_like_nginx`] for how a test builds a plausible
//! value from a PEM certificate the same way nginx's `$ssl_client_escaped_cert` would.

use std::net::SocketAddr;

use tonic::metadata::MetadataValue;
use tonic::transport::{Channel, Endpoint};
use tonic::{Request, Response, Status};

use av_edge::pb;

pub mod pb_client {
    //! Generated `altavista.v1.edge_ingest_client` plumbing only (`build.rs`
    //! `extern_path`s every message type onto [`av_cdm::pb`]) -- see this crate's own
    //! `lib.rs` module doc.
    #![allow(clippy::all)]
    include!(concat!(env!("OUT_DIR"), "/altavista.v1.rs"));
}

/// The forwarded-client-certificate header contract, from the client's own side -- see
/// this crate's module doc, and `av_ingest::forwarded_cert`'s own module doc (the
/// canonical definition), for the full picture.
pub mod forwarded_cert {
    /// Must stay byte-for-byte identical to `av_ingest::forwarded_cert::
    /// FORWARDED_CLIENT_CERT_HEADER` -- `crates/av-ingest/tests/forwarded_cert_header_name_agreement.rs` asserts
    /// the two literals agree, since this crate cannot depend on `av-ingest` itself (that
    /// crate's own tests depend on this one; a dependency the other way would cycle).
    pub const FORWARDED_CLIENT_CERT_HEADER: &str = "x-ssl-client-escaped-cert";

    /// Builds a header value the way nginx's own `$ssl_client_escaped_cert` documents
    /// itself as building one (percent-encoding every byte outside the unreserved set) --
    /// **a test/simulation helper only**, standing in for the service-owned nginx mTLS
    /// front this task does not render or run (see `av_ingest::forwarded_cert`'s module
    /// doc for exactly what was observed versus documented rather than guessed about
    /// nginx's own behaviour). Never used by a real deployment: nginx itself sets this
    /// header, not this crate.
    pub fn percent_encode_pem_like_nginx(pem: &[u8]) -> String {
        let mut out = String::with_capacity(pem.len() * 3);
        for &b in pem {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                out.push(b as char);
            } else {
                out.push_str(&format!("%{b:02X}"));
            }
        }
        out
    }
}

/// Question 155/202: is `address` (a bare `"host:port"` string) a recognized loopback
/// endpoint? A second, independent copy of `av_ingest::server::is_loopback_address` (that
/// module's own doc explains why this ~15-line function is duplicated, not shared, across
/// this workspace's crates) -- kept `pub(crate)` for the same reason.
fn is_loopback_address(address: &str) -> bool {
    let host = match address.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() && port.chars().all(|c| c.is_ascii_digit()) && !port.is_empty() => host,
        _ => address,
    };
    let host = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(host);
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<std::net::IpAddr>().map(|ip| ip.is_loopback()).unwrap_or(false)
}

/// Everything that can go wrong establishing an [`EdgeIngestClient`] connection (as
/// distinct from an RPC failing once connected -- that is a plain `tonic::Status`,
/// returned directly by every method below).
#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    /// `address` did not parse into a recognized loopback endpoint (question 155/202) --
    /// refused before ever attempting to connect.
    #[error("{address:?} is not a loopback address -- refusing a plaintext connection (question 155/202: plaintext gRPC is loopback-only; a non-loopback endpoint needs a service-owned nginx mTLS front)")]
    NonLoopback { address: String },
    #[error("{address:?} is not a valid plaintext endpoint URI: {source}")]
    InvalidEndpoint {
        address: String,
        #[source]
        source: tonic::transport::Error,
    },
    #[error("connecting (plaintext) to {address}: {source}")]
    Connect {
        address: String,
        #[source]
        source: tonic::transport::Error,
    },
}

/// A `tonic` client for `altavista.v1.EdgeIngest`. See this crate's module doc for the
/// plaintext-loopback-only scope and the forwarded-certificate header simulation.
#[derive(Debug, Clone)]
pub struct EdgeIngestClient {
    inner: pb_client::edge_ingest_client::EdgeIngestClient<Channel>,
    forwarded_client_cert: Option<String>,
}

impl EdgeIngestClient {
    /// Connect over plain HTTP/2 (h2c), no TLS at all -- refuses `address` up front
    /// (before ever calling [`Endpoint::connect`]) unless [`is_loopback_address`] accepts
    /// it. `address` is `"host:port"` (e.g. `"127.0.0.1:50080"`); this function builds
    /// the `http://` URI itself.
    pub async fn connect_plaintext(address: &str) -> Result<Self, ConnectError> {
        if !is_loopback_address(address) {
            return Err(ConnectError::NonLoopback { address: address.to_string() });
        }
        let uri = format!("http://{address}");
        let ep = Endpoint::from_shared(uri.clone()).map_err(|source| ConnectError::InvalidEndpoint { address: uri.clone(), source })?;
        let channel = ep.connect().await.map_err(|source| ConnectError::Connect { address: uri, source })?;
        Ok(Self { inner: pb_client::edge_ingest_client::EdgeIngestClient::new(channel), forwarded_client_cert: None })
    }

    /// Connect to an already-parsed `addr` -- a convenience for a caller that just bound
    /// or was handed a `SocketAddr` (e.g. `crate::server::bind_loopback`'s own listener's
    /// `local_addr()`), so a test never has to format one back into a string just to hand
    /// it to [`EdgeIngestClient::connect_plaintext`].
    pub async fn connect_plaintext_addr(addr: SocketAddr) -> Result<Self, ConnectError> {
        Self::connect_plaintext(&addr.to_string()).await
    }

    /// Sets the forwarded-client-certificate metadata header this client attaches to
    /// every subsequent RPC -- simulating what a service-owned nginx mTLS front would set
    /// (`crate::forwarded_cert`'s own module doc). `escaped_pem` should already be in
    /// nginx's own escaped form (see [`forwarded_cert::percent_encode_pem_like_nginx`]).
    pub fn set_forwarded_client_cert(&mut self, escaped_pem: impl Into<String>) {
        self.forwarded_client_cert = Some(escaped_pem.into());
    }

    /// Clears whatever [`EdgeIngestClient::set_forwarded_client_cert`] most recently set,
    /// so a subsequent RPC carries no forwarded-certificate header at all.
    pub fn clear_forwarded_client_cert(&mut self) {
        self.forwarded_client_cert = None;
    }

    /// Builds a `Request` carrying the forwarded-certificate header if one is set. Not
    /// fallible (unlike a naive `MetadataValue::try_from` call site would be): every value
    /// this crate ever hands to [`EdgeIngestClient::set_forwarded_client_cert`] either
    /// comes from [`forwarded_cert::percent_encode_pem_like_nginx`] (whose own output is,
    /// by construction, alphanumerics plus `-._~%` -- always valid printable ASCII gRPC
    /// metadata) or from a test that is itself simulating exactly that shape; a value
    /// that fails ASCII-metadata validation here would be a bug in this crate's own test
    /// fixtures, not a runtime condition a caller needs a `Result` to recover from --
    /// `clippy::result_large_err` (`tonic::Status` is ~176 bytes) is the mechanical reason
    /// this crate keeps that error out of its own public `Result` signatures rather than
    /// `#[allow]`-ing the lint, on top of the design reason just given.
    fn request<T>(&self, message: T) -> Request<T> {
        let mut request = Request::new(message);
        if let Some(escaped) = &self.forwarded_client_cert {
            let value = MetadataValue::try_from(escaped.as_str()).expect("forwarded_client_cert is always constructed as printable-ASCII (see this function's own doc comment)");
            request.metadata_mut().insert(forwarded_cert::FORWARDED_CLIENT_CERT_HEADER, value);
        }
        request
    }

    pub async fn announce(&mut self, manifest: pb::PluginManifest) -> Result<pb::ManifestAck, Status> {
        let request = self.request(manifest);
        self.inner.announce(request).await.map(Response::into_inner)
    }

    /// Streams every one of `batches` (in order) and collects every returned
    /// `BatchVerdict` (also in order) before returning -- a deliberately simple,
    /// fully-collected round trip rather than exposing a live, incremental `Stream` type
    /// across this crate's public API: every caller this round (this track's own
    /// `wire_*.rs` tests, and E4's plugin binary replaying a whole run's batches) wants
    /// "send these, then tell me every verdict", not incremental delivery. A future
    /// caller that genuinely needs the live stream can still reach `self.inner` directly
    /// (`pb_client::edge_ingest_client::EdgeIngestClient::submit`) -- nothing here
    /// prevents that, this is just the common-case convenience.
    pub async fn submit_batches(&mut self, batches: Vec<pb::MeasurementBatch>) -> Result<Vec<pb::BatchVerdict>, Status> {
        let stream = futures_util::stream::iter(batches);
        let request = self.request(stream);
        let response = self.inner.submit(request).await?;
        let mut inbound = response.into_inner();
        let mut verdicts = Vec::new();
        while let Some(verdict) = inbound.message().await? {
            verdicts.push(verdict);
        }
        Ok(verdicts)
    }

    pub async fn get_evidence(&mut self) -> Result<pb::EvidenceResponse, Status> {
        let request = self.request(pb::EvidenceRequest {});
        self.inner.get_evidence(request).await.map(Response::into_inner)
    }

    pub async fn verify_ledger(&mut self) -> Result<pb::VerifyResponse, Status> {
        let request = self.request(pb::VerifyRequest {});
        self.inner.verify_ledger(request).await.map(Response::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn connect_plaintext_refuses_a_non_loopback_address_without_dialing() {
        let err = EdgeIngestClient::connect_plaintext("0.0.0.0:50080").await.unwrap_err();
        assert!(matches!(err, ConnectError::NonLoopback { .. }), "{err:?}");
    }

    #[tokio::test]
    async fn connect_plaintext_to_nothing_listening_fails_with_a_typed_connect_error() {
        // Port 1 is a reserved/privileged port essentially never bound in a test sandbox
        // -- mirrors crates/av-lockstep's own identical test for connect_plaintext.
        let err = match EdgeIngestClient::connect_plaintext("127.0.0.1:1").await {
            Ok(_) => panic!("nothing should be listening on 127.0.0.1:1 in a test sandbox"),
            Err(e) => e,
        };
        assert!(matches!(err, ConnectError::Connect { .. }), "{err:?}");
    }

    #[test]
    fn percent_encode_pem_like_nginx_escapes_newlines_and_leaves_alphanumerics_alone() {
        let pem = b"-----BEGIN CERTIFICATE-----\nAB1\n-----END CERTIFICATE-----\n";
        let escaped = forwarded_cert::percent_encode_pem_like_nginx(pem);
        assert!(escaped.contains("%0A"), "newlines must be percent-encoded: {escaped}");
        assert!(escaped.contains("AB1"), "alphanumerics must pass through unescaped: {escaped}");
        assert!(!escaped.contains('\n'), "no literal newline must remain: {escaped}");
    }
}
