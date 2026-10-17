//! A throwaway PKI generated at test time with the `openssl` crate: a CA, server certificates
//! with chosen DNS/IP SANs, and a client certificate; plus an `openssl::ssl` server that runs a
//! scripted exchange per accepted connection and records what it saw.
#![allow(dead_code)]

use std::io::Write;
use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use openssl::asn1::{Asn1Integer, Asn1Time};
use openssl::bn::BigNum;
use openssl::ec::{EcGroup, EcKey};
use openssl::nid::Nid;
use openssl::pkey::{PKey, Private};
use openssl::ssl::{NameType, Ssl, SslContextBuilder, SslMethod, SslVerifyMode, SslVersion};
use openssl::x509::extension::{
    AuthorityKeyIdentifier, BasicConstraints, ExtendedKeyUsage, KeyUsage, SubjectAlternativeName,
    SubjectKeyIdentifier,
};
use openssl::x509::{X509NameBuilder, X509};

use super::{read_request, run_steps, Step};

static SERIAL: AtomicU32 = AtomicU32::new(1);

pub struct Ca {
    pub cert: X509,
    pub key: PKey<Private>,
}

pub struct Leaf {
    pub cert: X509,
    pub key: PKey<Private>,
}

impl Ca {
    pub fn pem(&self) -> Vec<u8> {
        self.cert.to_pem().unwrap()
    }
}

impl Leaf {
    pub fn cert_pem(&self) -> Vec<u8> {
        self.cert.to_pem().unwrap()
    }
    pub fn key_pem(&self) -> Vec<u8> {
        self.key.private_key_to_pem_pkcs8().unwrap()
    }
}

fn new_key() -> PKey<Private> {
    let group = EcGroup::from_curve_name(Nid::X9_62_PRIME256V1).unwrap();
    PKey::from_ec_key(EcKey::generate(&group).unwrap()).unwrap()
}

fn name(cn: &str) -> openssl::x509::X509Name {
    let mut b = X509NameBuilder::new().unwrap();
    b.append_entry_by_text("CN", cn).unwrap();
    b.build()
}

fn base_builder(cn: &str, key: &PKey<Private>, issuer: Option<&openssl::x509::X509NameRef>) -> openssl::x509::X509Builder {
    let mut b = X509::builder().unwrap();
    b.set_version(2).unwrap();
    let serial = BigNum::from_u32(SERIAL.fetch_add(1, Ordering::Relaxed)).unwrap();
    b.set_serial_number(&Asn1Integer::from_bn(&serial).unwrap()).unwrap();
    let subject = name(cn);
    b.set_subject_name(&subject).unwrap();
    match issuer {
        Some(i) => b.set_issuer_name(i).unwrap(),
        None => b.set_issuer_name(&subject).unwrap(),
    }
    b.set_pubkey(key).unwrap();
    b.set_not_before(&Asn1Time::days_from_now(0).unwrap()).unwrap();
    b.set_not_after(&Asn1Time::days_from_now(2).unwrap()).unwrap();
    b
}

pub fn make_ca(cn: &str) -> Ca {
    let key = new_key();
    let mut b = base_builder(cn, &key, None);
    b.append_extension(BasicConstraints::new().critical().ca().build().unwrap()).unwrap();
    b.append_extension(KeyUsage::new().critical().key_cert_sign().crl_sign().build().unwrap())
        .unwrap();
    let ski = SubjectKeyIdentifier::new().build(&b.x509v3_context(None, None)).unwrap();
    b.append_extension(ski).unwrap();
    b.sign(&key, openssl::hash::MessageDigest::sha256()).unwrap();
    Ca { cert: b.build(), key }
}

/// What a leaf certificate is for.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Usage {
    Server,
    Client,
}

/// Issues a leaf signed by `ca`, with the given DNS and IP subject alternative names.
pub fn issue(ca: &Ca, cn: &str, dns: &[&str], ips: &[&str], usage: Usage) -> Leaf {
    let key = new_key();
    let mut b = base_builder(cn, &key, Some(ca.cert.subject_name()));
    b.append_extension(BasicConstraints::new().build().unwrap()).unwrap();
    b.append_extension(KeyUsage::new().critical().digital_signature().build().unwrap())
        .unwrap();
    let mut eku = ExtendedKeyUsage::new();
    match usage {
        Usage::Server => eku.server_auth(),
        Usage::Client => eku.client_auth(),
    };
    b.append_extension(eku.build().unwrap()).unwrap();
    if !dns.is_empty() || !ips.is_empty() {
        let mut san = SubjectAlternativeName::new();
        for d in dns {
            san.dns(d);
        }
        for i in ips {
            san.ip(i);
        }
        let ext = san.build(&b.x509v3_context(Some(&ca.cert), None)).unwrap();
        b.append_extension(ext).unwrap();
    }
    let aki = AuthorityKeyIdentifier::new()
        .keyid(false)
        .build(&b.x509v3_context(Some(&ca.cert), None))
        .unwrap();
    b.append_extension(aki).unwrap();
    b.sign(&ca.key, openssl::hash::MessageDigest::sha256()).unwrap();
    Leaf { cert: b.build(), key }
}

/// A server certificate valid for `localhost` and `127.0.0.1` (the usual good case).
pub fn good_server(ca: &Ca) -> Leaf {
    issue(ca, "av-http1 test server", &["localhost"], &["127.0.0.1"], Usage::Server)
}

/// What the TLS server saw on one accepted connection.
#[derive(Debug, Clone, Default)]
pub struct Seen {
    pub handshake_ok: bool,
    pub handshake_error: Option<String>,
    pub sni: Option<String>,
    pub peer_cn: Option<String>,
    pub negotiated: Option<String>,
    pub request: Vec<u8>,
}

pub struct TlsOpts<'a> {
    pub leaf: &'a Leaf,
    /// Require and verify a client certificate issued by this CA.
    pub require_client_ca: Option<&'a Ca>,
    pub min: Option<SslVersion>,
    pub max: Option<SslVersion>,
    /// Send close_notify after the script (true) or just drop the TCP stream (false).
    pub clean_close: bool,
    /// The address to listen on.
    pub bind: &'static str,
}

impl<'a> TlsOpts<'a> {
    pub fn new(leaf: &'a Leaf) -> Self {
        TlsOpts {
            bind: "127.0.0.1:0",
            leaf,
            require_client_ca: None,
            min: Some(SslVersion::TLS1_2),
            max: None,
            clean_close: true,
        }
    }
}

pub struct TlsFake {
    pub addr: SocketAddr,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<Vec<Seen>>>,
}

impl TlsFake {
    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    pub fn finish(mut self) -> Vec<Seen> {
        self.stop.store(true, Ordering::Relaxed);
        self.handle.take().unwrap().join().unwrap()
    }
}

impl Drop for TlsFake {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Starts a TLS server on 127.0.0.1. Connection N runs script N after the handshake. A failed
/// handshake is recorded and the server carries on.
pub fn tls_fake(opts: TlsOpts<'_>, scripts: Vec<Vec<Step>>) -> TlsFake {
    let mut ctx = SslContextBuilder::new(SslMethod::tls_server()).unwrap();
    ctx.set_min_proto_version(opts.min).unwrap();
    ctx.set_max_proto_version(opts.max).unwrap();
    if opts.max.is_some_and(|v| v != SslVersion::TLS1_2 && v != SslVersion::TLS1_3) {
        // Offering a legacy protocol at all needs the security level lowered.
        ctx.set_cipher_list("DEFAULT:@SECLEVEL=0").unwrap();
    }
    ctx.set_certificate(&opts.leaf.cert).unwrap();
    ctx.set_private_key(&opts.leaf.key).unwrap();
    ctx.check_private_key().unwrap();
    if let Some(ca) = opts.require_client_ca {
        ctx.cert_store_mut().add_cert(ca.cert.clone()).unwrap();
        ctx.set_verify(SslVerifyMode::PEER | SslVerifyMode::FAIL_IF_NO_PEER_CERT);
    }
    let ctx = ctx.build();
    let clean_close = opts.clean_close;

    let listener = TcpListener::bind(opts.bind).unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let handle = thread::spawn(move || {
        let mut seen = Vec::new();
        let mut scripts = scripts.into_iter();
        while !stop2.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((tcp, _)) => {
                    tcp.set_nonblocking(false).ok();
                    tcp.set_nodelay(true).ok();
                    tcp.set_read_timeout(Some(Duration::from_secs(5))).ok();
                    tcp.set_write_timeout(Some(Duration::from_secs(5))).ok();
                    let mut rec = Seen::default();
                    let ssl = Ssl::new(&ctx).unwrap();
                    match ssl.accept(tcp) {
                        Ok(mut stream) => {
                            rec.handshake_ok = true;
                            rec.sni = stream.ssl().servername(NameType::HOST_NAME).map(str::to_string);
                            rec.negotiated = Some(stream.ssl().version_str().to_string());
                            rec.peer_cn = stream.ssl().peer_certificate().and_then(|c| {
                                c.subject_name()
                                    .entries_by_nid(Nid::COMMONNAME)
                                    .next()
                                    .and_then(|e| e.data().to_string().ok())
                            });
                            rec.request = read_request(&mut stream);
                            if let Some(steps) = scripts.next() {
                                run_steps(&mut stream, &steps);
                            }
                            let _ = stream.flush();
                            if clean_close {
                                let _ = stream.shutdown();
                            }
                        }
                        Err(e) => rec.handshake_error = Some(e.to_string()),
                    }
                    seen.push(rec);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => thread::sleep(Duration::from_millis(2)),
                Err(_) => break,
            }
        }
        seen
    });
    TlsFake {
        addr,
        stop,
        handle: Some(handle),
    }
}
