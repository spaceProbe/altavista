//! The TLS suite: a CA, server certificates with chosen SANs and a client certificate are
//! generated at test time with the `openssl` crate, and an `openssl::ssl` server runs in a thread.

mod common;

use std::io::Write;
use std::net::TcpListener;
use std::thread;
use std::time::{Duration, Instant};

use av_http1::{Client, Endpoint, HttpError, Phase, Request, Timeouts, TlsConfig};
use common::pki::{good_server, issue, make_ca, tls_fake, Ca, Leaf, TlsOpts, Usage};
use common::{ok, pause, w, Step};
use openssl::ssl::SslVersion;

fn client(host: &str, port: u16, tls: TlsConfig) -> Client {
    Client::new(
        Endpoint::parse(&format!("https://{host}:{port}")).unwrap(),
        Some(tls),
        Timeouts::uniform(Duration::from_secs(3)),
    )
    .unwrap()
}

fn trusting(ca: &Ca) -> TlsConfig {
    TlsConfig::new().with_ca_pem(ca.pem())
}

fn identity(ca: &Ca, cn: &str) -> Leaf {
    issue(ca, cn, &[], &[], Usage::Client)
}

fn tls_reason(r: Result<av_http1::Response, HttpError>, what: &str) -> String {
    match r {
        Err(HttpError::Tls { reason, .. }) => reason,
        other => panic!("{what}: expected HttpError::Tls, got {other:?}"),
    }
}

// ---------------------------------------------------------------- success paths

#[test]
fn a_request_succeeds_to_an_ip_literal_and_to_localhost_with_the_right_sni() {
    let ca = make_ca("av-http1 test CA");
    let leaf = good_server(&ca);
    let srv = tls_fake(TlsOpts::new(&leaf), vec![ok(b"by ip"), ok(b"by name")]);
    let port = srv.port();

    let r = client("127.0.0.1", port, trusting(&ca))
        .request(Request::get("/hello").header("X-Test", "1"))
        .unwrap();
    assert_eq!((r.status, r.body.as_slice()), (200, &b"by ip"[..]));
    // "localhost" resolves to ::1 and 127.0.0.1 on this host; the server listens on IPv4 only, so
    // this also exercises the try-the-next-address path when ::1 is refused.
    let r = client("localhost", port, trusting(&ca)).request(Request::get("/hello")).unwrap();
    assert_eq!((r.status, r.body.as_slice()), (200, &b"by name"[..]));

    let seen = srv.finish();
    assert_eq!(seen.len(), 2);
    assert!(seen.iter().all(|s| s.handshake_ok), "{seen:?}");
    assert_eq!(seen[0].sni, None, "no SNI for an IP literal");
    assert_eq!(seen[1].sni.as_deref(), Some("localhost"), "SNI for a DNS name");
    assert_eq!(
        String::from_utf8(seen[0].request.clone()).unwrap(),
        format!("GET /hello HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nX-Test: 1\r\nConnection: close\r\n\r\n")
    );
    assert_eq!(seen[1].negotiated.as_deref(), Some("TLSv1.3"));
}

#[test]
fn an_ipv6_literal_verifies_against_an_ip_san() {
    if std::net::TcpListener::bind("[::1]:0").is_err() {
        eprintln!("skipped: no IPv6 loopback on this host");
        return;
    }
    let ca = make_ca("av-http1 test CA");
    let leaf = issue(&ca, "v6 server", &[], &["::1"], Usage::Server);
    let mut opts = TlsOpts::new(&leaf);
    opts.bind = "[::1]:0";
    let srv = tls_fake(opts, vec![ok(b"v6")]);
    let r = client("[::1]", srv.port(), trusting(&ca)).request(Request::get("/")).unwrap();
    assert_eq!(r.body, b"v6");
    let seen = srv.finish();
    assert_eq!(seen[0].sni, None);
    assert!(String::from_utf8_lossy(&seen[0].request).contains("\r\nHost: [::1]:"));
}

#[test]
fn chunked_content_length_and_large_bodies_work_over_tls() {
    let ca = make_ca("av-http1 test CA");
    let leaf = good_server(&ca);
    let big: Vec<u8> = (0..3 * 1024 * 1024u32).map(|i| (i % 249) as u8).collect();
    let srv = tls_fake(
        TlsOpts::new(&leaf),
        vec![
            vec![w(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n")],
            ok(&big),
        ],
    );
    let c = client("127.0.0.1", srv.port(), trusting(&ca));
    assert_eq!(c.request(Request::get("/a")).unwrap().body, b"hello world");
    assert_eq!(c.request(Request::post("/b").body(vec![7u8; 1 << 20])).unwrap().body, big);
    let seen = srv.finish();
    assert!(seen[1].request.ends_with(&vec![7u8; 1 << 20]));
}

#[test]
fn the_ca_can_be_a_file_path() {
    let ca = make_ca("av-http1 test CA");
    let leaf = good_server(&ca);
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("av-http1-ca-file");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("ca.pem");
    std::fs::write(&path, ca.pem()).unwrap();
    let srv = tls_fake(TlsOpts::new(&leaf), vec![ok(b"file")]);
    let c = client("127.0.0.1", srv.port(), TlsConfig::new().with_ca_file(&path));
    assert_eq!(c.request(Request::get("/")).unwrap().body, b"file");
    srv.finish();
    std::fs::remove_file(&path).ok();
}

// ---------------------------------------------------------------- verification refusals

#[test]
fn a_server_certificate_from_a_different_ca_is_refused() {
    let trusted = make_ca("trusted CA");
    let rogue = make_ca("rogue CA");
    let leaf = good_server(&rogue);
    let srv = tls_fake(TlsOpts::new(&leaf), vec![ok(b"must not be read"), ok(b"nor this")]);
    let reason = tls_reason(
        client("127.0.0.1", srv.port(), trusting(&trusted)).request(Request::get("/")),
        "wrong CA, by ip",
    );
    assert!(reason.contains("certificate verify failed"), "{reason}");
    assert!(reason.contains("unable to get local issuer certificate"), "{reason}");
    let reason = tls_reason(
        client("localhost", srv.port(), trusting(&trusted)).request(Request::get("/")),
        "wrong CA, by name",
    );
    assert!(reason.contains("certificate verify failed"), "{reason}");
    let seen = srv.finish();
    assert!(seen.iter().all(|s| s.request.is_empty()), "no request was ever sent: {seen:?}");
}

#[test]
fn the_system_store_does_not_trust_a_private_ca_and_a_given_ca_replaces_the_system_store() {
    let ca = make_ca("private CA");
    let leaf = good_server(&ca);
    let srv = tls_fake(TlsOpts::new(&leaf), vec![]);
    let reason = tls_reason(
        client("127.0.0.1", srv.port(), TlsConfig::new()).request(Request::get("/")),
        "system store",
    );
    assert!(reason.contains("certificate verify failed"), "{reason}");
    srv.finish();
}

#[test]
fn a_server_certificate_without_the_ip_san_is_refused_when_connecting_by_ip() {
    let ca = make_ca("av-http1 test CA");
    // DNS SAN localhost only. OpenSSL does not match an IP literal against a DNS SAN.
    let leaf = issue(&ca, "dns only", &["localhost"], &[], Usage::Server);
    let srv = tls_fake(TlsOpts::new(&leaf), vec![ok(b"control")]);
    let reason = tls_reason(
        client("127.0.0.1", srv.port(), trusting(&ca)).request(Request::get("/")),
        "no IP SAN",
    );
    assert!(reason.contains("certificate verify failed"), "{reason}");
    assert!(reason.to_ascii_lowercase().contains("ip address mismatch"), "{reason}");
    // Control: the same certificate and the same server are fine by name, so the refusal above is
    // the missing IP SAN and nothing else.
    let r = client("localhost", srv.port(), trusting(&ca)).request(Request::get("/")).unwrap();
    assert_eq!(r.body, b"control");
    srv.finish();
}

#[test]
fn a_certificate_for_another_dns_name_is_refused_but_its_ip_san_still_works_by_ip() {
    let ca = make_ca("av-http1 test CA");
    let leaf = issue(&ca, "other host", &["other.example"], &["127.0.0.1"], Usage::Server);
    let srv = tls_fake(TlsOpts::new(&leaf), vec![ok(b"by ip")]);
    let reason = tls_reason(
        client("localhost", srv.port(), trusting(&ca)).request(Request::get("/")),
        "wrong DNS name",
    );
    assert!(reason.to_ascii_lowercase().contains("hostname mismatch"), "{reason}");
    let r = client("127.0.0.1", srv.port(), trusting(&ca)).request(Request::get("/")).unwrap();
    assert_eq!(r.body, b"by ip");
    srv.finish();
}

#[test]
fn an_ip_san_only_certificate_is_refused_by_name() {
    let ca = make_ca("av-http1 test CA");
    let leaf = issue(&ca, "ip only", &[], &["127.0.0.1"], Usage::Server);
    let srv = tls_fake(TlsOpts::new(&leaf), vec![]);
    let reason = tls_reason(
        client("localhost", srv.port(), trusting(&ca)).request(Request::get("/")),
        "IP SAN only, by name",
    );
    assert!(reason.contains("certificate verify failed"), "{reason}");
    srv.finish();
}

#[test]
fn the_subject_cn_is_never_a_substitute_for_a_san() {
    let ca = make_ca("av-http1 test CA");
    // CN=localhost and no subjectAltName at all: the legacy fallback OpenSSL offers by default.
    let leaf = issue(&ca, "localhost", &[], &[], Usage::Server);
    let srv = tls_fake(TlsOpts::new(&leaf), vec![]);
    let reason = tls_reason(
        client("localhost", srv.port(), trusting(&ca)).request(Request::get("/")),
        "CN only",
    );
    assert!(reason.contains("certificate verify failed"), "{reason}");
    srv.finish();
}

// ---------------------------------------------------------------- client identity

#[test]
fn a_server_that_requires_a_client_certificate_accepts_the_in_memory_identity_only() {
    let ca = make_ca("av-http1 test CA");
    let leaf = good_server(&ca);
    let me = identity(&ca, "av-http1 test client");
    let other_ca = make_ca("someone else's CA");
    let stranger = identity(&other_ca, "stranger");

    let mut opts = TlsOpts::new(&leaf);
    opts.require_client_ca = Some(&ca);
    let srv = tls_fake(opts, vec![ok(b"welcome"), ok(b"must not be sent"), ok(b"must not be sent")]);
    let port = srv.port();

    let with_id = trusting(&ca).with_client_identity(me.cert_pem(), me.key_pem());
    let r = client("127.0.0.1", port, with_id).request(Request::get("/")).unwrap();
    assert_eq!(r.body, b"welcome");

    // No identity: TLS 1.3 completes the client's handshake before the server's verdict, so the
    // refusal arrives as an alert on the first read. It must still be a Tls error.
    let reason = tls_reason(client("127.0.0.1", port, trusting(&ca)).request(Request::get("/")), "no identity");
    assert!(reason.to_ascii_lowercase().contains("certificate"), "{reason}");

    // An identity from a CA the server does not trust.
    let wrong = trusting(&ca).with_client_identity(stranger.cert_pem(), stranger.key_pem());
    tls_reason(client("127.0.0.1", port, wrong).request(Request::get("/")), "untrusted identity");

    let seen = srv.finish();
    assert_eq!(seen[0].peer_cn.as_deref(), Some("av-http1 test client"));
    assert!(seen[0].handshake_ok);
    assert!(!seen[1].handshake_ok, "{:?}", seen[1]);
    assert!(!seen[2].handshake_ok, "{:?}", seen[2]);
    assert!(seen[1].request.is_empty() && seen[2].request.is_empty());
}

#[test]
fn the_identity_works_under_tls_1_2_too() {
    let ca = make_ca("av-http1 test CA");
    let leaf = good_server(&ca);
    let me = identity(&ca, "av-http1 test client");
    let mut opts = TlsOpts::new(&leaf);
    opts.require_client_ca = Some(&ca);
    opts.max = Some(SslVersion::TLS1_2);
    let srv = tls_fake(opts, vec![ok(b"one"), ok(b"two")]);
    let port = srv.port();
    let with_id = trusting(&ca).with_client_identity(me.cert_pem(), me.key_pem());
    assert_eq!(client("127.0.0.1", port, with_id).request(Request::get("/")).unwrap().body, b"one");
    // In TLS 1.2 the server's verdict arrives during the handshake itself.
    tls_reason(client("127.0.0.1", port, trusting(&ca)).request(Request::get("/")), "no identity, TLS 1.2");
    let seen = srv.finish();
    assert_eq!(seen[0].negotiated.as_deref(), Some("TLSv1.2"));
    assert_eq!(seen[0].peer_cn.as_deref(), Some("av-http1 test client"));
}

#[test]
fn a_certificate_chain_is_sent_with_the_identity() {
    // identity PEM = leaf then its (here: root) issuer; the server must still accept it, and the
    // extra certificate must load without error.
    let ca = make_ca("av-http1 test CA");
    let leaf = good_server(&ca);
    let me = identity(&ca, "chain client");
    let mut chain = me.cert_pem();
    chain.extend_from_slice(&ca.pem());
    let mut opts = TlsOpts::new(&leaf);
    opts.require_client_ca = Some(&ca);
    let srv = tls_fake(opts, vec![ok(b"chained")]);
    let c = client("127.0.0.1", srv.port(), trusting(&ca).with_client_identity(chain, me.key_pem()));
    assert_eq!(c.request(Request::get("/")).unwrap().body, b"chained");
    assert_eq!(srv.finish()[0].peer_cn.as_deref(), Some("chain client"));
}

// ---------------------------------------------------------------- protocol floor and misc TLS behaviour

#[test]
fn a_server_that_only_speaks_tls_1_1_is_refused() {
    let ca = make_ca("av-http1 test CA");
    let leaf = good_server(&ca);
    let mut opts = TlsOpts::new(&leaf);
    opts.min = Some(SslVersion::TLS1);
    opts.max = Some(SslVersion::TLS1_1);
    let srv = tls_fake(opts, vec![ok(b"legacy")]);
    tls_reason(
        client("127.0.0.1", srv.port(), trusting(&ca)).request(Request::get("/")),
        "TLS 1.1 only",
    );
    let seen = srv.finish();
    assert!(!seen[0].handshake_ok);
}

#[test]
fn https_to_a_plaintext_server_is_a_tls_error() {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let h = thread::spawn(move || {
        let (mut s, _) = l.accept().unwrap();
        let _ = s.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n");
    });
    let ca = make_ca("av-http1 test CA");
    tls_reason(client("127.0.0.1", port, trusting(&ca)).request(Request::get("/")), "https to plaintext");
    h.join().unwrap();
}

#[test]
fn a_silent_tls_server_is_a_handshake_timeout_and_a_stalled_response_a_read_timeout() {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let h = thread::spawn(move || {
        let (s, _) = l.accept().unwrap();
        thread::sleep(Duration::from_millis(900));
        drop(s);
    });
    let ca = make_ca("av-http1 test CA");
    let c = Client::new(
        Endpoint::parse(&format!("https://127.0.0.1:{port}")).unwrap(),
        Some(trusting(&ca)),
        Timeouts::uniform(Duration::from_millis(300)),
    )
    .unwrap();
    let started = Instant::now();
    match c.request(Request::get("/")) {
        Err(HttpError::Timeout {
            phase: Phase::TlsHandshake,
            ..
        }) => {}
        other => panic!("expected Timeout(TlsHandshake), got {other:?}"),
    }
    assert!(started.elapsed() < Duration::from_secs(2));
    h.join().unwrap();

    let leaf = good_server(&ca);
    let srv = tls_fake(TlsOpts::new(&leaf), vec![vec![Step::Hold]]);
    let c = Client::new(
        Endpoint::parse(&format!("https://127.0.0.1:{}", srv.port())).unwrap(),
        Some(trusting(&ca)),
        Timeouts::uniform(Duration::from_millis(300)),
    )
    .unwrap();
    assert!(matches!(
        c.request(Request::get("/")),
        Err(HttpError::Timeout { phase: Phase::Read, .. })
    ));
    srv.finish();
}

#[test]
fn a_response_delimited_by_close_needs_a_close_notify() {
    let ca = make_ca("av-http1 test CA");
    let leaf = good_server(&ca);
    let script = || vec![w(b"HTTP/1.1 200 OK\r\n\r\nthe whole "), pause(20), w(b"body")];

    let srv = tls_fake(TlsOpts::new(&leaf), vec![script()]);
    let r = client("127.0.0.1", srv.port(), trusting(&ca)).request(Request::get("/")).unwrap();
    assert_eq!(r.body, b"the whole body", "clean TLS shutdown ends the body");
    srv.finish();

    let mut opts = TlsOpts::new(&leaf);
    opts.clean_close = false;
    let srv = tls_fake(opts, vec![script()]);
    match client("127.0.0.1", srv.port(), trusting(&ca)).request(Request::get("/")) {
        Err(HttpError::Protocol(why)) => assert!(why.contains("close_notify"), "{why}"),
        other => panic!("expected Protocol (no close_notify), got {other:?}"),
    }
    srv.finish();
}

#[test]
fn a_framed_response_does_not_need_a_close_notify_but_a_truncated_one_is_a_protocol_error() {
    let ca = make_ca("av-http1 test CA");
    let leaf = good_server(&ca);
    let mut opts = TlsOpts::new(&leaf);
    opts.clean_close = false;
    let srv = tls_fake(
        opts,
        vec![
            vec![w(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\ndone")],
            vec![w(b"HTTP/1.1 200 OK\r\nContent-Length: 40\r\n\r\ncut")],
            vec![w(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n")],
        ],
    );
    let c = client("127.0.0.1", srv.port(), trusting(&ca));
    assert_eq!(c.request(Request::get("/")).unwrap().body, b"done");
    assert!(matches!(c.request(Request::get("/")), Err(HttpError::Protocol(_))));
    assert!(matches!(c.request(Request::get("/")), Err(HttpError::Protocol(_))));
    srv.finish();
}

// ---------------------------------------------------------------- configuration errors

#[test]
fn bad_tls_configuration_fails_at_construction() {
    let ca = make_ca("av-http1 test CA");
    let leaf = good_server(&ca);
    let me = identity(&ca, "client");
    let ep = || Endpoint::parse("https://127.0.0.1:1").unwrap();
    let build = |tls: TlsConfig| Client::new(ep(), Some(tls), Timeouts::default());
    let invalid = |r: Result<Client, HttpError>, what: &str| match r {
        Err(HttpError::InvalidTlsConfig(why)) => why,
        other => panic!("{what}: expected InvalidTlsConfig, got {other:?}"),
    };

    invalid(build(TlsConfig::new().with_ca_pem(b"not pem at all".to_vec())), "garbage CA");
    invalid(build(TlsConfig::new().with_ca_pem(Vec::new())), "empty CA");
    invalid(build(TlsConfig::new().with_ca_file("/nonexistent/ca.pem")), "missing CA file");
    invalid(build(trusting(&ca).with_client_identity(b"junk".to_vec(), me.key_pem())), "junk certificate");
    invalid(build(trusting(&ca).with_client_identity(me.cert_pem(), b"junk".to_vec())), "junk key");
    // A key that does not belong to the certificate.
    let why = invalid(
        build(trusting(&ca).with_client_identity(me.cert_pem(), leaf.key_pem())),
        "mismatched key",
    );
    assert!(why.contains("mismatch"), "{why}");
    // An encrypted key is refused, not prompted for (this must return promptly, not wait on a tty).
    let encrypted = me
        .key
        .private_key_to_pem_pkcs8_passphrase(openssl::symm::Cipher::aes_256_cbc(), b"secret")
        .unwrap();
    let started = Instant::now();
    invalid(build(trusting(&ca).with_client_identity(me.cert_pem(), encrypted)), "encrypted key");
    assert!(started.elapsed() < Duration::from_secs(5));
    // And a good configuration builds.
    build(trusting(&ca).with_client_identity(me.cert_pem(), me.key_pem())).unwrap();
}

#[test]
fn the_debug_output_never_shows_key_material() {
    let ca = make_ca("av-http1 test CA");
    let me = identity(&ca, "client");
    let key_pem = String::from_utf8(me.key_pem()).unwrap();
    let cfg = trusting(&ca).with_client_identity(me.cert_pem(), me.key_pem());
    let shown = format!("{cfg:?}");
    let key_body: String = key_pem.lines().nth(1).unwrap().to_string();
    assert!(!shown.contains("PRIVATE KEY") && !shown.contains(&key_body), "{shown}");
    assert!(shown.contains("redacted"), "{shown}");
}
