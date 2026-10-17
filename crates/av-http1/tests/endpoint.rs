//! `Endpoint::parse`: the grammar, the refusals, and question 155's loopback-only plaintext rule.

use std::net::{Ipv4Addr, Ipv6Addr};

use av_http1::{Endpoint, EndpointProblem, Host, HttpError, Scheme};

fn ok(url: &str) -> Endpoint {
    Endpoint::parse(url).unwrap_or_else(|e| panic!("{url}: {e}"))
}

fn problem(url: &str) -> EndpointProblem {
    match Endpoint::parse(url) {
        Err(HttpError::InvalidEndpoint { problem, url: u }) => {
            assert_eq!(u, url);
            problem
        }
        other => panic!("{url}: expected InvalidEndpoint, got {other:?}"),
    }
}

#[test]
fn https_forms_and_default_ports() {
    let e = ok("https://example.com");
    assert_eq!(e.scheme(), Scheme::Https);
    assert!(e.is_tls());
    assert_eq!(e.host(), &Host::Dns("example.com".into()));
    assert_eq!(e.port(), 443);
    assert_eq!(e.base_path(), "");
    assert_eq!(e.host_header(), "example.com");
    assert_eq!(e.authority(), "example.com:443");

    let e = ok("https://Example.COM:8443/k8s/clusters/c-1/");
    assert_eq!(e.host(), &Host::Dns("example.com".into()), "DNS names are lower-cased");
    assert_eq!(e.port(), 8443);
    assert_eq!(e.base_path(), "/k8s/clusters/c-1", "trailing slash is dropped");
    assert_eq!(e.host_header(), "example.com:8443");

    assert_eq!(ok("HTTPS://example.com/").base_path(), "", "a lone slash is the root");
    assert_eq!(ok("https://10.0.0.5:6443").host(), &Host::Ipv4(Ipv4Addr::new(10, 0, 0, 5)));
    let v6 = ok("https://[2001:db8::1]:6443/x");
    assert_eq!(v6.host(), &Host::Ipv6("2001:db8::1".parse::<Ipv6Addr>().unwrap()));
    assert_eq!(v6.host_text(), "2001:db8::1", "no brackets in the verification name");
    assert_eq!(v6.authority(), "[2001:db8::1]:6443");
    assert_eq!(v6.host_header(), "[2001:db8::1]:6443");
    assert_eq!(ok("https://[2001:db8::1]").host_header(), "[2001:db8::1]");
}

#[test]
fn http_default_port_and_loopback_forms() {
    let e = ok("http://127.0.0.1");
    assert_eq!(e.scheme(), Scheme::Http);
    assert!(!e.is_tls());
    assert_eq!(e.port(), 80);
    assert_eq!(e.host_header(), "127.0.0.1");
    assert!(e.is_loopback());
    assert_eq!(ok("http://127.0.0.1:8123/db").base_path(), "/db");
    assert_eq!(ok("http://127.1.2.3:9").port(), 9, "all of 127.0.0.0/8 is loopback");
    assert!(ok("http://[::1]:8123").is_loopback());
    assert_eq!(ok("http://[::1]:8123").authority(), "[::1]:8123");
    assert!(ok("http://localhost:8123").is_loopback());
    assert!(ok("http://LocalHost:8123").is_loopback(), "case-insensitive");
}

#[test]
fn plaintext_to_non_loopback_is_refused_without_resolving_anything() {
    for url in [
        "http://example.com",
        "http://10.0.0.5:8123",
        "http://192.168.1.10",
        "http://128.0.0.1",
        "http://0.0.0.0:80",
        "http://[2001:db8::1]:80",
        "http://[::ffff:127.0.0.1]:80", // an IPv4-mapped literal is not the ::1 loopback
        "http://[::]:80",
        // names that a resolver might send to loopback, or that merely contain "localhost":
        "http://foo.localhost:80",
        "http://localhost.example.com",
        "http://notlocalhost",
        "http://127.0.0.1.example.com",
    ] {
        match Endpoint::parse(url) {
            Err(HttpError::PlaintextNonLoopback { host }) => {
                assert!(!host.is_empty(), "{url}")
            }
            other => panic!("{url}: expected PlaintextNonLoopback, got {other:?}"),
        }
    }
    // The same hosts are fine over TLS.
    for url in ["https://example.com", "https://10.0.0.5:8123", "https://[2001:db8::1]"] {
        ok(url);
    }
}

#[test]
fn other_schemes_are_refused() {
    assert_eq!(problem("ftp://example.com"), EndpointProblem::UnsupportedScheme("ftp".into()));
    assert_eq!(problem("ws://localhost"), EndpointProblem::UnsupportedScheme("ws".into()));
    assert!(matches!(problem("//example.com"), EndpointProblem::UnsupportedScheme(_)));
    assert!(matches!(problem("example.com:443"), EndpointProblem::UnsupportedScheme(_)));
    assert!(matches!(problem("localhost"), EndpointProblem::UnsupportedScheme(_)));
    assert!(matches!(problem(""), EndpointProblem::UnsupportedScheme(_)));
    assert!(matches!(problem("httpx://localhost"), EndpointProblem::UnsupportedScheme(_)));
}

#[test]
fn userinfo_is_refused() {
    assert_eq!(problem("https://user@example.com"), EndpointProblem::Userinfo);
    assert_eq!(problem("https://user:pass@example.com:8443/x"), EndpointProblem::Userinfo);
    assert_eq!(problem("http://:pw@127.0.0.1"), EndpointProblem::Userinfo);
    // The classic parser-differential trick: the real host is after the '@'.
    assert_eq!(problem("http://127.0.0.1@evil.example"), EndpointProblem::Userinfo);
    assert_eq!(problem("https://example.com@"), EndpointProblem::Userinfo);
}

#[test]
fn malformed_authorities_are_refused() {
    for url in [
        "https://",
        "https:///path",
        "https://:443",
        "https://example.com:",
        "https://example.com:0",
        "https://example.com:65536",
        "https://example.com:99999999",
        "https://example.com:80a",
        "https://example.com:-1",
        "https://example.com:+80",
        "https://exa mple.com",
        "https://example.com\r\nHost: evil",
        "https://example.com\0",
        "https://example.com\\evil",
        "https://ex\u{e9}mple.com",
        "https://exa_mple..com",
        "https://.example.com",
        "https://example.com.",
        "https://-",
        "https://exa!mple.com",
        "https://127.1",
        "https://0177.0.0.1",
        "https://999.1.1.1",
        "https://1.2.3.4.5",
        "https://1.2.3",
        "https://[::1",
        "https://[::1]x",
        "https://[::1]:",
        "https://[::1]:0",
        "https://[not-an-ip]",
        "https://[fe80::1%eth0]",
        "https://[127.0.0.1]",
        "https://::1",
        "https://2001:db8::1:443",
        "https://example.com:443:443",
        "https://[]",
    ] {
        match Endpoint::parse(url) {
            Err(HttpError::InvalidEndpoint {
                problem: EndpointProblem::MalformedAuthority(_),
                ..
            }) => {}
            other => panic!("{url:?}: expected MalformedAuthority, got {other:?}"),
        }
    }
}

#[test]
fn query_fragment_and_bad_base_paths_are_refused() {
    assert_eq!(problem("https://example.com?x=1"), EndpointProblem::QueryOrFragment);
    assert_eq!(problem("https://example.com/base?x=1"), EndpointProblem::QueryOrFragment);
    assert_eq!(problem("https://example.com/base#frag"), EndpointProblem::QueryOrFragment);
    assert_eq!(problem("https://example.com#frag"), EndpointProblem::QueryOrFragment);
    for url in [
        "https://example.com/a b",
        "https://example.com/a\r\nX: y",
        "https://example.com/a\0b",
        "https://example.com/a\\b",
        "https://example.com/caf\u{e9}",
        "https://example.com/a\tb",
    ] {
        assert_eq!(problem(url), EndpointProblem::BadBasePath, "{url:?}");
    }
}

#[test]
fn display_names_the_problem() {
    let e = Endpoint::parse("http://example.com").unwrap_err();
    assert!(e.to_string().contains("question 155"), "{e}");
    let e = Endpoint::parse("ftp://x").unwrap_err();
    assert!(e.to_string().contains("ftp"), "{e}");
}
