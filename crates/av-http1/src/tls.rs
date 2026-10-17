//! [`TlsConfig`]: how a [`crate::Client`] trusts a server and (optionally) identifies itself.
//!
//! * **Trust** comes from a CA PEM held in memory ([`TlsConfig::with_ca_pem`]), a CA file path
//!   ([`TlsConfig::with_ca_file`]), or, if neither is given, the system default store. A given
//!   CA *replaces* the system store: a private CA is the only root that endpoint trusts.
//! * **Client identity** is a certificate chain and a private key, both as **in-memory PEM**
//!   ([`TlsConfig::with_client_identity`]). The Kubernetes client reads them from a kubeconfig's
//!   base64 data and they are never written to a file by this crate. An encrypted key is refused
//!   (an empty passphrase is supplied to OpenSSL so it can never prompt on a terminal).
//! * **Hostname verification is always on** and there is no switch for it: `connect` verifies a
//!   DNS-name host against the certificate's DNS SANs and an IP-literal host against its IP SANs
//!   (`X509_VERIFY_PARAM_set1_ip`); an IP does not match a DNS SAN. The certificate subject's CN is
//!   never used as a fallback (`NEVER_CHECK_SUBJECT`). SNI is sent for DNS names only (never for
//!   an IP literal). The minimum protocol version is TLS 1.2.
//!
//! The configuration is validated when the [`crate::Client`] is built, not on first use, so a bad
//! PEM or a key that does not match its certificate fails before any connection.

use std::fmt;
use std::path::PathBuf;

use openssl::pkey::PKey;
use openssl::ssl::{SslConnector, SslMethod, SslVerifyMode, SslVersion};
use openssl::x509::store::X509StoreBuilder;
use openssl::x509::X509;

use crate::error::HttpError;

#[derive(Clone)]
enum CaSource {
    System,
    Pem(Vec<u8>),
    File(PathBuf),
}

#[derive(Clone)]
struct Identity {
    cert_chain_pem: Vec<u8>,
    key_pem: Vec<u8>,
}

/// TLS trust and client identity. `TlsConfig::default()` is the system store and no identity.
#[derive(Clone)]
pub struct TlsConfig {
    ca: CaSource,
    identity: Option<Identity>,
}

impl Default for TlsConfig {
    fn default() -> Self {
        TlsConfig {
            ca: CaSource::System,
            identity: None,
        }
    }
}

impl fmt::Debug for TlsConfig {
    /// Never prints key material.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ca = match &self.ca {
            CaSource::System => "system".to_string(),
            CaSource::Pem(p) => format!("pem({} bytes)", p.len()),
            CaSource::File(p) => format!("file({})", p.display()),
        };
        f.debug_struct("TlsConfig")
            .field("ca", &ca)
            .field("client_identity", &self.identity.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl TlsConfig {
    /// The system default store, no client identity.
    pub fn new() -> TlsConfig {
        TlsConfig::default()
    }

    /// Trust exactly the certificates in this PEM (one or more), held in memory.
    pub fn with_ca_pem(mut self, pem: impl Into<Vec<u8>>) -> TlsConfig {
        self.ca = CaSource::Pem(pem.into());
        self
    }

    /// Trust exactly the certificates in this PEM file (read when the client is built).
    pub fn with_ca_file(mut self, path: impl Into<PathBuf>) -> TlsConfig {
        self.ca = CaSource::File(path.into());
        self
    }

    /// Present this client certificate chain (leaf first) and private key, both PEM in memory.
    pub fn with_client_identity(mut self, cert_chain_pem: impl Into<Vec<u8>>, key_pem: impl Into<Vec<u8>>) -> TlsConfig {
        self.identity = Some(Identity {
            cert_chain_pem: cert_chain_pem.into(),
            key_pem: key_pem.into(),
        });
        self
    }

    /// Builds the OpenSSL connector. Hostname verification is applied per connection by
    /// `ConnectConfiguration::connect` (see the module doc), which this crate never disables.
    pub(crate) fn build_connector(&self) -> Result<SslConnector, HttpError> {
        let invalid = |what: &str, e: &dyn fmt::Display| HttpError::InvalidTlsConfig(format!("{what}: {e}"));
        let mut builder = SslConnector::builder(SslMethod::tls_client()).map_err(|e| invalid("OpenSSL context", &e))?;
        builder
            .set_min_proto_version(Some(SslVersion::TLS1_2))
            .map_err(|e| invalid("minimum protocol version", &e))?;
        builder.set_verify(SslVerifyMode::PEER);

        let ca_pem = match &self.ca {
            CaSource::System => None,
            CaSource::Pem(p) => Some(p.clone()),
            CaSource::File(path) => Some(
                std::fs::read(path).map_err(|e| invalid(&format!("CA file {}", path.display()), &e))?,
            ),
        };
        if let Some(pem) = ca_pem {
            let certs = X509::stack_from_pem(&pem).map_err(|e| invalid("CA PEM", &e))?;
            if certs.is_empty() {
                return Err(HttpError::InvalidTlsConfig("CA PEM holds no certificate".into()));
            }
            // A fresh store, so the private CA replaces (does not add to) the system roots that
            // SslConnector::builder loaded.
            let mut store = X509StoreBuilder::new().map_err(|e| invalid("CA store", &e))?;
            for cert in certs {
                store.add_cert(cert).map_err(|e| invalid("CA store", &e))?;
            }
            builder.set_cert_store(store.build());
        }

        if let Some(id) = &self.identity {
            let mut chain = X509::stack_from_pem(&id.cert_chain_pem)
                .map_err(|e| invalid("client certificate PEM", &e))?
                .into_iter();
            let leaf = chain
                .next()
                .ok_or_else(|| HttpError::InvalidTlsConfig("client certificate PEM holds no certificate".into()))?;
            // The empty-passphrase callback makes an encrypted key fail instead of prompting.
            let key = PKey::private_key_from_pem_callback(&id.key_pem, |_| Ok(0))
                .map_err(|e| invalid("client private key PEM (unencrypted PEM only)", &e))?;
            builder.set_certificate(&leaf).map_err(|e| invalid("client certificate", &e))?;
            for extra in chain {
                builder
                    .add_extra_chain_cert(extra)
                    .map_err(|e| invalid("client certificate chain", &e))?;
            }
            // set_private_key already refuses a key that does not belong to the certificate
            // ("key values mismatch"); check_private_key repeats the test for the full context.
            builder
                .set_private_key(&key)
                .map_err(|e| invalid("client private key (it must match the certificate)", &e))?;
            builder
                .check_private_key()
                .map_err(|e| invalid("client key does not match its certificate", &e))?;
        }
        Ok(builder.build())
    }
}
