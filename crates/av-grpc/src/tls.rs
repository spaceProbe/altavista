//! An OpenSSL-backed `tonic::transport::Channel` connector.
//!
//! `tonic`'s own TLS features (`tls`, `tls-native-roots`, `tls-webpki-roots`) all pull in
//! `tokio-rustls`, which pulls `ring` -- forbidden by ADR-004's crypto rule ("the system
//! FIPS OpenSSL only, no bundled crypto... this excludes `ring`-based TLS"). This module
//! is deliberately built without any of them: this crate's `tonic` dependency has default
//! features off with only `codegen`, `prost`, `channel` enabled (see `Cargo.toml`), and
//! the TLS handshake here runs entirely through the `openssl` crate (`openssl-sys` ->
//! the system/Homebrew `libssl`/`libcrypto`, resolved via `OPENSSL_DIR` in this
//! environment -- see `services/gmat-service/README.md`) via `hyper-openssl`'s
//! `tower::Service<Uri>` connector, plugged into
//! [`tonic::transport::Endpoint::connect_with_connector`].
//!
//! Nothing here is gmat-service-specific -- it is a general "connect a tonic `Channel`
//! with mTLS through OpenSSL" helper -- but its one caller today is
//! `src/bin/describe_client.rs`, which uses it to prove (`tests/test_grpc_tls.py`) that
//! the nginx front in `services/gmat-service/deploy/` both allows a correctly
//! client-certificated `Describe` call through and refuses one with no client
//! certificate at all.

use std::path::Path;

use hyper_openssl::client::legacy::HttpsConnector;
use hyper_util::client::legacy::connect::HttpConnector;
use openssl::error::ErrorStack;
use openssl::ssl::{SslConnector, SslFiletype, SslMethod, SslVerifyMode};
use thiserror::Error;
use tonic::transport::{Channel, Endpoint};

/// Client identity plus the trust anchor for one mTLS connection.
///
/// `client_cert`/`client_key` are `Option` *only* so this same helper can also drive
/// `tests/test_grpc_tls.py`'s "refused without a client certificate" case by omitting
/// them -- a real caller always has a seccert-issued leaf and should always set both.
pub struct MtlsConfig<'a> {
    /// PEM file with the CA chain (root, plus any intermediates) the **server** cert
    /// presented by the nginx front is verified against. The test harness's local
    /// two-tier CA's root certificate in tests; seccert's root in a real deployment.
    pub ca_file: &'a Path,
    /// PEM client certificate chain (leaf first, then any intermediates) presented to
    /// the proxy for mTLS. `None` deliberately connects with no client certificate.
    pub client_cert: Option<&'a Path>,
    /// PEM private key matching `client_cert`'s leaf. Must be `Some` iff `client_cert`
    /// is `Some` -- see [`MtlsConfigError::KeyWithoutCert`] / [`CertWithoutKey`].
    ///
    /// [`CertWithoutKey`]: MtlsConfigError::CertWithoutKey
    pub client_key: Option<&'a Path>,
}

/// Everything that can go wrong building or using an mTLS [`Channel`].
#[derive(Debug, Error)]
pub enum MtlsConnectError {
    #[error("client_cert was set without client_key (or vice versa) -- both or neither")]
    IncompleteClientIdentity,
    #[error("configuring the OpenSSL client context: {0}")]
    Ssl(#[from] ErrorStack),
    #[error("{endpoint:?} is not a valid endpoint URI: {source}")]
    InvalidEndpoint {
        endpoint: String,
        #[source]
        source: tonic::transport::Error,
    },
    #[error("connecting to {endpoint}: {source}")]
    Connect {
        endpoint: String,
        #[source]
        source: tonic::transport::Error,
    },
}

/// Connect a `tonic` [`Channel`] to `endpoint` (an `https://host:port` URI -- the nginx
/// front's listen address) with the given mTLS configuration, actively performing the
/// TLS handshake before returning (unlike `Endpoint::connect_lazy`, so a refused
/// connection -- e.g. no client certificate against `ssl_verify_client on` -- surfaces
/// here as an `Err`, not on the first RPC).
pub async fn connect(endpoint: &str, cfg: MtlsConfig<'_>) -> Result<Channel, MtlsConnectError> {
    if cfg.client_cert.is_some() != cfg.client_key.is_some() {
        return Err(MtlsConnectError::IncompleteClientIdentity);
    }

    let mut ssl = SslConnector::builder(SslMethod::tls_client())?;
    ssl.set_ca_file(cfg.ca_file)?;
    ssl.set_verify(SslVerifyMode::PEER);
    // gRPC is HTTP/2-only; restrict ALPN so a misconfigured proxy that would otherwise
    // fall back to HTTP/1.1 fails the handshake loudly instead of confusingly.
    ssl.set_alpn_protos(b"\x02h2")?;
    if let (Some(cert), Some(key)) = (cfg.client_cert, cfg.client_key) {
        ssl.set_certificate_chain_file(cert)?;
        ssl.set_private_key_file(key, SslFiletype::PEM)?;
        ssl.check_private_key()?;
    }

    let mut http = HttpConnector::new();
    http.enforce_http(false); // the connector must accept `https://` URIs
    let https = HttpsConnector::with_connector(http, ssl)?;

    let owned = endpoint.to_string();
    let ep = Endpoint::from_shared(owned.clone()).map_err(|source| MtlsConnectError::InvalidEndpoint {
        endpoint: owned.clone(),
        source,
    })?;
    ep.connect_with_connector(https)
        .await
        .map_err(|source| MtlsConnectError::Connect { endpoint: owned, source })
}
