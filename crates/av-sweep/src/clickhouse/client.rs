//! ClickHouse's HTTP interface over [`av_http1`]: run a statement, run a query that answers in
//! `RowBinary`, insert `RowBinary` rows, and turn every way that can fail into a typed
//! [`StoreError`].
//!
//! # Credentials
//!
//! The user goes in `X-ClickHouse-User` and the password in `X-ClickHouse-Key`; the database in
//! `X-ClickHouse-Database`. Never a URL: `Endpoint::parse` already refuses userinfo, and this
//! module refuses it first with a message that does not echo the URL (the URL of a mistake may
//! hold the very password it should not). The password is read from an environment variable
//! whose **name** is configured, kept in this client only, and never appears in a `Debug` impl
//! (`ChClient`'s is written by hand), in a `StoreError`, or in a request path. Every string that
//! came from the server is also passed through [`ChClient::redact`] before it is stored in an
//! error, so a server that echoes the credential back in an exception message cannot leak it
//! through this crate (`tests/clickhouse_fake_server.rs` proves it with a server that does).
//!
//! # No URL settings
//!
//! Requests carry no ClickHouse `SETTINGS` (neither in the URL nor in the SQL): a user created
//! `readonly = 1` is *refused* when a request tries to change a setting, which would make every
//! read by a read-only user fail for a reason that has nothing to do with the data. The one cost
//! is that without `wait_end_of_query` the server may begin a reply and then fail; a reader
//! therefore never trusts a reply without checking it decoded exactly and agrees with the study's
//! header (`ClickHouseStudyStore::get_study`).
//!
//! # Transport
//!
//! One connection per request (`av-http1`'s model). Plaintext is accepted only on loopback
//! (question 155), enforced by `av_http1::Endpoint::parse`; the store surfaces that refusal as
//! [`StoreError::InvalidEndpoint`] before any connection is attempted.

use std::fmt;
use std::path::PathBuf;

use av_http1::{Client, Endpoint, HttpError, Request, SizeLimits, Timeouts, TlsConfig};

use crate::store::StoreError;

/// Everything needed to reach one ClickHouse database. The password is **not** here: only the
/// *name* of the environment variable that holds it, so a config can be logged, compared and
/// stored without ever carrying the secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClickHouseConfig {
    /// `http://host:port` (loopback only) or `https://host:port`, optionally with a base path.
    pub url: String,
    /// The database holding the study tables; it must already exist.
    pub database: String,
    pub user: String,
    /// The name of the environment variable holding the password. `None` sends no password (a
    /// user with an empty one; the image's `default` user on loopback).
    pub password_env: Option<String>,
    /// A PEM file of the CA(s) to trust for an `https://` endpoint, replacing the system store.
    pub ca_file: Option<PathBuf>,
    pub timeouts: Timeouts,
    /// The largest reply body the client will buffer.
    pub max_response_bytes: usize,
}

impl ClickHouseConfig {
    /// A config with the default timeouts and a 256 MiB reply limit.
    pub fn new(url: impl Into<String>, database: impl Into<String>, user: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            database: database.into(),
            user: user.into(),
            password_env: None,
            ca_file: None,
            timeouts: Timeouts::default(),
            max_response_bytes: 256 * 1024 * 1024,
        }
    }

    pub fn with_password_env(mut self, name: impl Into<String>) -> Self {
        self.password_env = Some(name.into());
        self
    }

    pub fn with_ca_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.ca_file = Some(path.into());
        self
    }
}

/// A ClickHouse exception parsed from a reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerException {
    pub http_status: u16,
    /// The numeric code: `X-ClickHouse-Exception-Code` if present, else `Code: N.` in the body.
    pub code: Option<i64>,
    /// The symbolic name (`UNKNOWN_TABLE`, `ACCESS_DENIED`, ...) when the body carries one.
    pub name: Option<String>,
    /// The server's message, credentials redacted, capped at 2 KiB.
    pub message: String,
}

impl fmt::Display for ServerException {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "HTTP {}", self.http_status)?;
        if let Some(code) = self.code {
            write!(f, ", code {code}")?;
        }
        if let Some(name) = &self.name {
            write!(f, " {name}")?;
        }
        write!(f, ": {}", self.message)
    }
}

/// A client for one ClickHouse database. Cheap to clone; sends one request per call.
#[derive(Clone)]
pub struct ChClient {
    http: Client,
    /// The endpoint as configured (no userinfo, by construction), for messages.
    url: String,
    database: String,
    user: String,
    password: Option<String>,
}

impl fmt::Debug for ChClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChClient")
            .field("url", &self.url)
            .field("database", &self.database)
            .field("user", &self.user)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .finish_non_exhaustive()
    }
}

/// Percent-encodes everything except the RFC 3986 unreserved set.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// A query parameter value in ClickHouse's text-escaped form (the `Escaped`/TSV rules the HTTP
/// `param_<name>` values are parsed with): backslash, tab, newline, carriage return and NUL are
/// backslash-escaped so a `String` parameter round-trips whatever it holds.
pub fn escape_param(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\0' => out.push_str("\\0"),
            c => out.push(c),
        }
    }
    out
}

impl ChClient {
    /// Validates the config, reads the password from its environment variable and builds the
    /// HTTP client. Opens no connection.
    pub fn connect(config: &ClickHouseConfig) -> Result<Self, StoreError> {
        if config.database.is_empty() || config.database.bytes().any(|b| b < 0x20 || b == 0x7f) {
            return Err(StoreError::Config { detail: "the ClickHouse database name is empty or holds a control character".to_string() });
        }
        if config.user.is_empty() || config.user.bytes().any(|b| b < 0x20 || b == 0x7f) {
            return Err(StoreError::Config { detail: "the ClickHouse user name is empty or holds a control character".to_string() });
        }
        // Userinfo is refused here, with a message that does not echo the URL: a URL that holds
        // `user:password@` is exactly the URL that must not be printed.
        let after_scheme = config.url.split_once("://").map(|(_, rest)| rest).unwrap_or(&config.url);
        let authority = after_scheme.split(['/', '?', '#']).next().unwrap_or("");
        if authority.contains('@') {
            return Err(StoreError::InvalidEndpoint {
                detail: "the ClickHouse URL carries user information (user:password@host); credentials never travel in a URL -- use the user setting and a password environment variable".to_string(),
            });
        }
        let password = match &config.password_env {
            None => None,
            Some(var) => match std::env::var(var) {
                Ok(v) if !v.is_empty() => Some(v),
                Ok(_) => return Err(StoreError::Config { detail: format!("the password environment variable {var} is set but empty") }),
                Err(_) => return Err(StoreError::Config { detail: format!("the password environment variable {var} is not set") }),
            },
        };

        let endpoint = Endpoint::parse(&config.url).map_err(|e| match e {
            HttpError::PlaintextNonLoopback { host } => StoreError::InvalidEndpoint {
                detail: format!("plaintext http:// to the non-loopback host {host:?} is refused (question 155); use https://"),
            },
            other => StoreError::InvalidEndpoint { detail: other.to_string() },
        })?;

        let tls = if endpoint.is_tls() {
            let mut tls = TlsConfig::new();
            if let Some(path) = &config.ca_file {
                tls = tls.with_ca_file(path);
            }
            Some(tls)
        } else if config.ca_file.is_some() {
            return Err(StoreError::Config { detail: "a CA file was given for a plaintext http:// ClickHouse URL".to_string() });
        } else {
            None
        };
        let http = Client::new(endpoint, tls, config.timeouts)
            .map_err(|e| match e {
                HttpError::InvalidTlsConfig(reason) => StoreError::Tls { endpoint: config.url.clone(), reason: format!("invalid TLS configuration: {reason}") },
                other => StoreError::Config { detail: other.to_string() },
            })?
            .with_limits(SizeLimits { max_header_bytes: 64 * 1024, max_body_bytes: config.max_response_bytes });
        Ok(Self { http, url: config.url.clone(), database: config.database.clone(), user: config.user.clone(), password })
    }

    /// The configured endpoint URL (never carries credentials).
    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn database(&self) -> &str {
        &self.database
    }

    /// Replaces every occurrence of the password in `s`.
    pub fn redact(&self, s: &str) -> String {
        match &self.password {
            Some(p) if !p.is_empty() => s.replace(p.as_str(), "<redacted>"),
            _ => s.to_string(),
        }
    }

    /// A [`StoreError::Corrupt`] naming this operation, with `detail` redacted.
    pub fn corrupt(&self, operation: &str, detail: String) -> StoreError {
        StoreError::Corrupt { operation: operation.to_string(), detail: self.redact(&detail) }
    }

    fn authed(&self, request: Request) -> Request {
        let mut request = request.header("X-ClickHouse-User", self.user.as_str()).header("X-ClickHouse-Database", self.database.as_str());
        if let Some(p) = &self.password {
            request = request.header("X-ClickHouse-Key", p.as_str());
        }
        request
    }

    /// `GET /ping`: `Ok(())` iff the server answers `200`. Unauthenticated on the server side.
    pub fn ping(&self) -> Result<(), StoreError> {
        let response = self.send(Request::get("/ping"), "pinging the server")?;
        if response.status == 200 {
            Ok(())
        } else {
            Err(self.exception(&response, "pinging the server"))
        }
    }

    /// Runs a statement (DDL, `INSERT ... SELECT`, `CREATE USER`, ...) whose reply body is
    /// ignored. `params` are `{name:Type}` query parameters, unescaped values.
    pub fn command(&self, sql: &str, params: &[(&str, &str)], operation: &str) -> Result<(), StoreError> {
        self.post_query(sql, params, operation).map(|_| ())
    }

    /// Runs a query whose SQL ends in `FORMAT RowBinary` (or another format) and returns the raw
    /// reply body.
    pub fn query(&self, sql: &str, params: &[(&str, &str)], operation: &str) -> Result<Vec<u8>, StoreError> {
        self.post_query(sql, params, operation)
    }

    fn post_query(&self, sql: &str, params: &[(&str, &str)], operation: &str) -> Result<Vec<u8>, StoreError> {
        let mut path = String::from("/");
        for (i, (name, value)) in params.iter().enumerate() {
            path.push(if i == 0 { '?' } else { '&' });
            path.push_str("param_");
            path.push_str(&percent_encode(name));
            path.push('=');
            path.push_str(&percent_encode(&escape_param(value)));
        }
        let response = self.send(Request::post(path).body(sql.as_bytes().to_vec()), operation)?;
        if response.status == 200 {
            Ok(response.body)
        } else {
            Err(self.exception(&response, operation))
        }
    }

    /// `INSERT INTO <table> (<columns>) FORMAT RowBinary` with `rows` as the request body.
    pub fn insert_rowbinary(&self, table: &str, columns: &[&str], rows: Vec<u8>, operation: &str) -> Result<(), StoreError> {
        let cols: Vec<String> = columns.iter().map(|c| format!("`{c}`")).collect();
        let query = format!("INSERT INTO `{table}` ({}) FORMAT RowBinary", cols.join(", "));
        let path = format!("/?query={}", percent_encode(&query));
        let response = self.send(Request::post(path).body(rows), operation)?;
        if response.status == 200 {
            Ok(())
        } else {
            Err(self.exception(&response, operation))
        }
    }

    fn send(&self, request: Request, operation: &str) -> Result<av_http1::Response, StoreError> {
        self.http.request(self.authed(request)).map_err(|e| self.transport_error(e, operation))
    }

    fn transport_error(&self, e: HttpError, operation: &str) -> StoreError {
        let endpoint = self.url.clone();
        let text = self.redact(&e.to_string());
        match e {
            HttpError::Unreachable { .. } => StoreError::Unreachable { endpoint, detail: text },
            HttpError::Timeout { .. } => StoreError::Unreachable { endpoint, detail: text },
            HttpError::Tls { reason, .. } => StoreError::Tls { endpoint, reason: self.redact(&reason) },
            HttpError::InvalidTlsConfig(reason) => StoreError::Tls { endpoint, reason: self.redact(&reason) },
            HttpError::PlaintextNonLoopback { .. } | HttpError::InvalidEndpoint { .. } => StoreError::InvalidEndpoint { detail: text },
            other => StoreError::Transport { endpoint, operation: operation.to_string(), detail: self.redact(&other.to_string()) },
        }
    }

    /// Parses a non-200 reply into a typed [`StoreError::Server`].
    fn exception(&self, response: &av_http1::Response, operation: &str) -> StoreError {
        StoreError::Server { endpoint: self.url.clone(), operation: operation.to_string(), exception: self.parse_exception(response) }
    }

    pub(crate) fn parse_exception(&self, response: &av_http1::Response) -> ServerException {
        let body = String::from_utf8_lossy(&response.body);
        let header_code = response.header("X-ClickHouse-Exception-Code").and_then(|v| v.trim().parse::<i64>().ok());
        let (body_code, name) = parse_exception_body(&body);
        let mut message = self.redact(body.trim());
        if message.len() > 2048 {
            let mut end = 2048;
            while !message.is_char_boundary(end) {
                end -= 1;
            }
            message.truncate(end);
            message.push_str("...");
        }
        if message.is_empty() {
            message = format!("HTTP {} {}", response.status, response.reason);
        }
        ServerException { http_status: response.status, code: header_code.or(body_code), name, message }
    }
}

/// `Code: 60. DB::Exception: ... (UNKNOWN_TABLE) (version 26.8.29.10 (official build))` ->
/// `(Some(60), Some("UNKNOWN_TABLE"))`.
fn parse_exception_body(body: &str) -> (Option<i64>, Option<String>) {
    let trimmed = body.trim_start();
    let Some(rest) = trimmed.strip_prefix("Code: ") else { return (None, None) };
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    let code = digits.parse::<i64>().ok();
    // The symbolic name is the last parenthesised UPPER_SNAKE token before the `(version ...)`.
    let head = body.rfind(" (version ").map(|i| &body[..i]).unwrap_or(body);
    let name = head.rfind('(').and_then(|open| {
        let candidate = head[open + 1..].split(')').next()?;
        let ok = !candidate.is_empty() && candidate.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_');
        ok.then(|| candidate.to_string())
    });
    (code, name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exception_bodies_parse_to_code_and_name() {
        let body = "Code: 60. DB::Exception: Table default.nope does not exist. (UNKNOWN_TABLE) (version 26.8.29.10 (official build))\n";
        assert_eq!(parse_exception_body(body), (Some(60), Some("UNKNOWN_TABLE".to_string())));
        let body = "Code: 516. DB::Exception: u: Authentication failed: password is incorrect, or there is no user with such name. (AUTHENTICATION_FAILED) (version 26.8.29.10)";
        assert_eq!(parse_exception_body(body), (Some(516), Some("AUTHENTICATION_FAILED".to_string())));
        // A message with its own parentheses still yields the trailing symbolic name.
        let body = "Code: 62. DB::Exception: Syntax error (some detail) near x. (SYNTAX_ERROR) (version 1)";
        assert_eq!(parse_exception_body(body), (Some(62), Some("SYNTAX_ERROR".to_string())));
        assert_eq!(parse_exception_body("Ok.\n"), (None, None));
        assert_eq!(parse_exception_body("<html>502</html>"), (None, None));
    }

    #[test]
    fn escape_param_escapes_exactly_the_text_format_specials() {
        assert_eq!(escape_param("plain-id_1"), "plain-id_1");
        assert_eq!(escape_param("a\\b"), "a\\\\b");
        assert_eq!(escape_param("a\tb\nc\rd\0e"), "a\\tb\\nc\\rd\\0e");
        assert_eq!(escape_param("it's \"quoted\" 'x"), "it's \"quoted\" 'x");
    }

    #[test]
    fn percent_encoding_leaves_only_unreserved_bytes() {
        assert_eq!(percent_encode("abc-._~XYZ019"), "abc-._~XYZ019");
        assert_eq!(percent_encode("a b&c=d/é"), "a%20b%26c%3Dd%2F%C3%A9");
    }

    #[test]
    fn debug_never_shows_the_password_and_a_config_holds_only_its_variable_name() {
        // SAFETY of the env var name: unique to this test, set and removed within it.
        let var = "AV_SWEEP_CH_UNIT_TEST_PASSWORD_DEBUG";
        std::env::set_var(var, "hunter2-super-secret");
        let config = ClickHouseConfig::new("http://127.0.0.1:8123", "db", "u").with_password_env(var);
        let client = ChClient::connect(&config).unwrap();
        std::env::remove_var(var);
        let shown = format!("{client:?} {config:?}");
        assert!(!shown.contains("hunter2"), "{shown}");
        assert!(shown.contains(var), "the variable NAME is fine to show: {shown}");
        assert!(shown.contains("<redacted>"), "{shown}");
        assert_eq!(client.redact("the key hunter2-super-secret leaked"), "the key <redacted> leaked");
    }

    #[test]
    fn a_url_with_userinfo_is_refused_without_echoing_it() {
        let err = ChClient::connect(&ClickHouseConfig::new("https://admin:topsecret@db.example:8443", "d", "u")).unwrap_err();
        let shown = format!("{err} {err:?}");
        assert!(matches!(err, StoreError::InvalidEndpoint { .. }), "{shown}");
        assert!(!shown.contains("topsecret") && !shown.contains("admin:"), "{shown}");
    }

    #[test]
    fn plaintext_to_a_non_loopback_host_is_a_typed_refusal_before_any_connection() {
        let err = ChClient::connect(&ClickHouseConfig::new("http://192.0.2.7:8123", "d", "u")).unwrap_err();
        match err {
            StoreError::InvalidEndpoint { detail } => assert!(detail.contains("question 155") && detail.contains("192.0.2.7"), "{detail}"),
            other => panic!("expected InvalidEndpoint, got {other:?}"),
        }
    }

    #[test]
    fn an_unset_or_empty_password_variable_is_a_config_error() {
        let var = "AV_SWEEP_CH_UNIT_TEST_PASSWORD_UNSET";
        std::env::remove_var(var);
        let err = ChClient::connect(&ClickHouseConfig::new("http://127.0.0.1:8123", "d", "u").with_password_env(var)).unwrap_err();
        assert!(matches!(&err, StoreError::Config { detail } if detail.contains(var)), "{err:?}");
    }
}
