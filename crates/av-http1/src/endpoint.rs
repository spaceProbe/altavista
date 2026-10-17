//! [`Endpoint`]: the parsed, validated base URL a [`crate::Client`] talks to.
//!
//! The grammar is `http://host[:port][/base]` or `https://host[:port][/base]`, where `host` is a
//! dotted-quad IPv4 literal, a bracketed IPv6 literal, or a DNS name, and the default ports are
//! 80 and 443. Everything else is a typed [`HttpError::InvalidEndpoint`]: another scheme,
//! userinfo (`user:pass@`), a query or fragment, an empty or malformed authority, a port that is
//! not 1-65535. A dotted-quad-looking host must be a valid IPv4 literal (so `127.1`, `0177.0.0.1`
//! and `999.1.1.1` are refused rather than guessed at).
//!
//! # Question 155: plaintext only on loopback
//!
//! `http://` is accepted only when the host is a loopback *literal* (`127.0.0.0/8`, `::1`) or the
//! name `localhost` (case-insensitively); anything else is [`HttpError::PlaintextNonLoopback`].
//! The decision reads the text of the URL, never the answer of a DNS query (a resolver that can be
//! made to say "127.0.0.1" is not evidence of anything). The fields are private and `parse` is the
//! only constructor, so there is no way to hold a non-loopback plaintext `Endpoint`.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};

use crate::error::{EndpointProblem, HttpError};

/// `http` or `https`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheme {
    Http,
    Https,
}

/// The host part of an endpoint, classified at parse time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Host {
    Ipv4(Ipv4Addr),
    Ipv6(Ipv6Addr),
    /// A DNS name, lower-cased.
    Dns(String),
}

/// A validated `http(s)://host[:port][/base]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    scheme: Scheme,
    host: Host,
    port: u16,
    base_path: String,
}

impl Endpoint {
    /// Parses and validates `url`. See the module doc for the grammar and the refusals.
    pub fn parse(url: &str) -> Result<Endpoint, HttpError> {
        let bad = |problem: EndpointProblem| HttpError::InvalidEndpoint {
            url: url.to_string(),
            problem,
        };

        let Some((scheme_text, rest)) = url.split_once("://") else {
            let guess = url.split(':').next().unwrap_or("").to_string();
            return Err(bad(EndpointProblem::UnsupportedScheme(guess)));
        };
        let scheme = if scheme_text.eq_ignore_ascii_case("http") {
            Scheme::Http
        } else if scheme_text.eq_ignore_ascii_case("https") {
            Scheme::Https
        } else {
            return Err(bad(EndpointProblem::UnsupportedScheme(scheme_text.to_string())));
        };

        let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let (authority, tail) = rest.split_at(authority_end);
        if tail.contains(['?', '#']) {
            return Err(bad(EndpointProblem::QueryOrFragment));
        }
        if authority.contains('@') {
            return Err(bad(EndpointProblem::Userinfo));
        }
        if tail.bytes().any(|b| b <= 0x20 || b >= 0x7f || b == b'\\') {
            return Err(bad(EndpointProblem::BadBasePath));
        }
        let malformed = |why: &'static str| bad(EndpointProblem::MalformedAuthority(why));
        if authority.is_empty() {
            return Err(malformed("empty host"));
        }
        if authority.bytes().any(|b| b <= 0x20 || b >= 0x7f || b == b'\\') {
            return Err(malformed("control, whitespace, backslash or non-ASCII character"));
        }

        let default_port = match scheme {
            Scheme::Http => 80,
            Scheme::Https => 443,
        };
        let (host, port_text) = if let Some(after_bracket) = authority.strip_prefix('[') {
            let Some((inside, after)) = after_bracket.split_once(']') else {
                return Err(malformed("unterminated IPv6 literal"));
            };
            let addr: Ipv6Addr = inside.parse().map_err(|_| malformed("bad IPv6 literal"))?;
            let port_text = match after {
                "" => None,
                other => Some(other.strip_prefix(':').ok_or_else(|| malformed("text after the IPv6 literal"))?),
            };
            (Host::Ipv6(addr), port_text)
        } else {
            let (host_text, port_text) = match authority.split_once(':') {
                Some((h, p)) => (h, Some(p)),
                None => (authority, None),
            };
            if port_text.is_some_and(|p| p.contains(':')) {
                return Err(malformed("more than one colon (an IPv6 literal needs brackets)"));
            }
            (parse_host(host_text).map_err(malformed)?, port_text)
        };
        let port = match port_text {
            None => default_port,
            Some(p) => parse_port(p).map_err(malformed)?,
        };

        // "/base/" and "/" are the same base as "/base" and "": requests are joined onto it.
        let base_path = tail.trim_end_matches('/').to_string();

        let endpoint = Endpoint {
            scheme,
            host,
            port,
            base_path,
        };
        if scheme == Scheme::Http && !endpoint.is_loopback() {
            return Err(HttpError::PlaintextNonLoopback {
                host: endpoint.host_text(),
            });
        }
        Ok(endpoint)
    }

    /// `http` or `https`.
    pub fn scheme(&self) -> Scheme {
        self.scheme
    }

    /// Whether this endpoint speaks TLS.
    pub fn is_tls(&self) -> bool {
        self.scheme == Scheme::Https
    }

    /// The classified host.
    pub fn host(&self) -> &Host {
        &self.host
    }

    /// The host as text: no brackets around an IPv6 literal, DNS names lower-cased. This is the
    /// string TLS verification is done against.
    pub fn host_text(&self) -> String {
        match &self.host {
            Host::Ipv4(a) => a.to_string(),
            Host::Ipv6(a) => a.to_string(),
            Host::Dns(n) => n.clone(),
        }
    }

    /// The port (explicit, or 80 / 443 by default).
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The base path requests are joined onto: `""` for the root, otherwise `/segment...` with
    /// no trailing slash.
    pub fn base_path(&self) -> &str {
        &self.base_path
    }

    /// `host:port`, with brackets around an IPv6 literal. Used in error messages.
    pub fn authority(&self) -> String {
        match &self.host {
            Host::Ipv6(a) => format!("[{a}]:{}", self.port),
            _ => format!("{}:{}", self.host_text(), self.port),
        }
    }

    /// The `Host` header value: the authority, with the port left off when it is the scheme's
    /// default.
    pub fn host_header(&self) -> String {
        let default = match self.scheme {
            Scheme::Http => 80,
            Scheme::Https => 443,
        };
        let host = match &self.host {
            Host::Ipv6(a) => format!("[{a}]"),
            _ => self.host_text(),
        };
        if self.port == default {
            host
        } else {
            format!("{host}:{}", self.port)
        }
    }

    /// Question 155's test: a loopback literal or `localhost`, decided from the text alone.
    pub fn is_loopback(&self) -> bool {
        match &self.host {
            Host::Ipv4(a) => a.is_loopback(),
            Host::Ipv6(a) => a.is_loopback(),
            Host::Dns(n) => n == "localhost",
        }
    }

    /// The addresses to try, in order: the literal itself, or whatever the system resolver
    /// returns for a DNS name (`getaddrinfo` has no timeout of its own; it is outside
    /// `Timeouts::connect`).
    pub(crate) fn socket_addrs(&self) -> std::io::Result<Vec<SocketAddr>> {
        match &self.host {
            Host::Ipv4(a) => Ok(vec![SocketAddr::new(IpAddr::V4(*a), self.port)]),
            Host::Ipv6(a) => Ok(vec![SocketAddr::new(IpAddr::V6(*a), self.port)]),
            Host::Dns(n) => Ok((n.as_str(), self.port).to_socket_addrs()?.collect()),
        }
    }
}

fn parse_port(text: &str) -> Result<u16, &'static str> {
    if text.is_empty() {
        return Err("empty port after the colon");
    }
    if !text.bytes().all(|b| b.is_ascii_digit()) || text.len() > 5 {
        return Err("port is not 1-65535");
    }
    match text.parse::<u16>() {
        Ok(0) | Err(_) => Err("port is not 1-65535"),
        Ok(p) => Ok(p),
    }
}

fn parse_host(text: &str) -> Result<Host, &'static str> {
    if text.is_empty() {
        return Err("empty host");
    }
    if text.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        // Looks like an IPv4 literal: it must be exactly one (no "127.1", no leading zeros).
        return text
            .parse::<Ipv4Addr>()
            .map(Host::Ipv4)
            .map_err(|_| "a dotted-quad host must be a valid IPv4 literal");
    }
    if text.len() > 253 {
        return Err("host name longer than 253 bytes");
    }
    for label in text.split('.') {
        if label.is_empty() || label.len() > 63 {
            return Err("empty or over-long DNS label");
        }
        if !label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
            return Err("character not allowed in a DNS name");
        }
        if label.starts_with('-') || label.ends_with('-') {
            return Err("DNS label starts or ends with a hyphen");
        }
    }
    Ok(Host::Dns(text.to_ascii_lowercase()))
}
