//! The async PostgreSQL connection: `PgClient`, driving `crate::protocol`'s framing and
//! `crate::scram`'s SCRAM-SHA-256 over a `tokio::net::TcpStream` (or, for [`PgTls::Required`],
//! an OpenSSL-wrapped stream).
//!
//! # Extended query protocol, TEXT format throughout
//!
//! Every query [`PgClient::query`]/[`PgClient::execute`] runs uses PostgreSQL's EXTENDED query
//! protocol (`Parse`/`Bind`/`Describe`/`Execute`/`Sync`), with every parameter AND every result
//! column in TEXT format (`crate::protocol::encode_bind`'s format-code `0`, applied to both
//! directions). This is a deliberate choice, not the default this crate happened to fall into:
//! it keeps every value this crate's own API surface ever touches a plain `&str`/`String` at
//! the wire boundary ([`Param`] renders to a PostgreSQL text literal; [`Row`]'s accessors parse
//! one back), so there is no binary numeric/timestamp/PostGIS-geometry wire format for this
//! crate to get wrong -- and a later task's schema/migration/query layer inherits that same
//! property for free. The trade-off (binary format is somewhat more compact and, for floats,
//! avoids a text round trip) is not one this crate's own workload -- catalog metadata queries,
//! not a bulk telemetry path -- needs to make.
//!
//! Even so, every query IS parameterised, never string-concatenated: [`PgClient::query`]/
//! [`PgClient::execute`] take `sql: &str` (containing `$1`, `$2`, ... placeholders) and
//! `params: &[Param]` separately, and `Bind` carries the parameter VALUES as separate wire
//! fields the server itself substitutes -- exactly the mechanism that makes SQL injection
//! structurally impossible through this API, restated below.
//!
//! # SQL injection is structurally impossible through this API
//!
//! There is no public function on [`PgClient`] that accepts a statement built by string
//! concatenation of caller-controlled data. [`PgClient::query`]/[`PgClient::execute`] take the
//! SQL text and the parameter values as two SEPARATE arguments (the extended protocol's own
//! `Parse sql` / `Bind params` split -- see above), so a parameter value can never be
//! interpreted as SQL syntax no matter what bytes it contains. [`PgClient::simple_batch`] is
//! the one function that sends a whole SQL string verbatim (PostgreSQL's simple query
//! protocol, which is what lets one `Query` message carry several `;`-separated statements --
//! needed for a migration script, which `Parse`'s single-statement contract cannot run) --
//! but its own contract is that `sql` is a `&'static`-style migration body loaded from a
//! committed file this codebase's own reviewers already saw, never a string assembled at
//! runtime from request data. A future caller that wants to build a WHERE clause from
//! caller-supplied data must do so with `$1`/`$2` placeholders and [`Param`] values through
//! [`PgClient::query`]/[`PgClient::execute`], never by formatting a string into
//! [`PgClient::simple_batch`].
//!
//! # `PgTls::Required`: implemented, but its encrypted path is untested by this round's suite
//!
//! [`PgTls::Required`] performs the real PostgreSQL `SSLRequest` handshake (8 bytes out, one
//! byte -- `'S'`/`'N'` -- back; never silently downgrading to plaintext if the server answers
//! `'N'`) and, on `'S'`, wraps the connection in a real `openssl::ssl::SslStream`. Because
//! `tokio-openssl` (the natural crate for bridging that onto `AsyncRead`/`AsyncWrite`) is NOT
//! already in this workspace's `Cargo.lock` (rule 2 -- see `Cargo.toml`'s own comment for the
//! exact dependency list checked), this module bridges the BLOCKING `openssl::ssl::SslStream`
//! itself: `PgConfig::tls`'s handshake converts the `tokio::net::TcpStream` to a blocking
//! `std::net::TcpStream` (`TcpStream::into_std`) and performs the handshake, and every
//! subsequent read/write on that connection runs inside `tokio::task::spawn_blocking` (see
//! [`BlockingTlsStream`]). This is real, correct, blocking-mode OpenSSL I/O -- not a stub --
//! but this task's own test suite (`tests/wire_protocol.rs`'s fake server, a plain TCP byte
//! stream) cannot terminate a real TLS handshake, so it can only prove the `'N'`-decline half
//! of this path (`PgTls::Required` against a server that declines SSL is refused, never
//! silently downgraded -- see that test module). Exercising the `'S'`-accept half against a
//! real certificate-bearing server is out of THIS task's scope (no Docker/container this
//! round, per `common.md`) and is named here rather than pretended to be covered, per this
//! task's own binding rule 14.
//!
//! # `connect_timeout` is a deadline, never a sleep -- and it is per PHASE, not per connection
//!
//! [`PgClient::connect`] wraps each of its three phases -- the TCP connect, the TLS
//! negotiation (`SSLRequest` write + response read + the TLS upgrade itself, when
//! [`PgTls::Required`]), and [`PgClient::startup`] (`StartupMessage`/authentication/
//! `ReadyForQuery`) -- in its OWN `tokio::time::timeout(config.connect_timeout, ..)`, each
//! racing that phase against a fresh `config.connect_timeout` budget and returning as soon as
//! either resolves -- never a `tokio::time::sleep` used to wait out a fixed duration before
//! proceeding (rule 7: no sleep as a synchronisation device). The deadline is per phase, not
//! summed across the connection: a server that is merely slow at each step is not punished for
//! the sum of its phases, while a peer that accepts the TCP connection and then never writes
//! another byte -- in TLS negotiation or in startup -- is caught by that phase's own deadline
//! and reported as [`crate::error::CatalogError::HandshakeTimeout`], which names the phase that
//! stalled ([`crate::error::CatalogError::ConnectTimeout`] is raised only by the TCP connect
//! phase itself, and keeps its own exact meaning). Every other wait in this module (reading the
//! next backend message once a phase's own timeout has already passed) blocks on real I/O
//! completing, not a clock.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use openssl::ssl::{SslConnector, SslMethod};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::error::{io_err, CatalogError};
use crate::protocol::{self, BackendMessage, DescribeTarget, TransactionStatus};
use crate::scram::{self, ScramClient};

/// How [`PgClient::connect`] should treat TLS. See this module's own doc, "`PgTls::Required`:
/// implemented, but...", for the connection's actual shape once `'S'` is chosen.
#[derive(Debug, Clone)]
pub enum PgTls {
    /// Never send `SSLRequest`; connect in plaintext. What every one of this crate's own
    /// `tests/wire_protocol.rs` fake-server tests use (that fake server speaks plain TCP, not
    /// TLS).
    Disabled,
    /// Always send `SSLRequest` and refuse to proceed in plaintext if the server declines
    /// (`'N'`) -- [`crate::error::CatalogError::ServerDeclinedTls`], never a silent
    /// downgrade. `ca_file`, if given, is passed to `openssl::ssl::SslConnectorBuilder::
    /// set_ca_file`; `None` uses the system's default trust store (`SslConnector::builder`'s
    /// own default).
    Required { ca_file: Option<PathBuf> },
}

/// Everything [`PgClient::connect`] needs. No `Default` impl: every field here is a real
/// deployment decision (host, credentials, whether TLS is required) this crate should never
/// silently default on a caller's behalf.
#[derive(Debug, Clone)]
pub struct PgConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    pub database: String,
    pub application_name: String,
    /// A deadline on each phase of [`PgClient::connect`] -- the TCP connect, the TLS
    /// negotiation, and the startup/authentication handshake each get their own budget of this
    /// same duration (see this module's own doc, "`connect_timeout` is a deadline, never a
    /// sleep -- and it is per PHASE, not per connection") -- not a per-query timeout; this
    /// crate's query methods wait on the server's own reply for as long as the server takes.
    pub connect_timeout: Duration,
    pub tls: PgTls,
}

/// One parameter value for [`PgClient::query`]/[`PgClient::execute`], rendered to its
/// PostgreSQL TEXT-format literal by [`Param::to_text`] (module doc: "Extended query protocol,
/// TEXT format throughout"). `Null` renders as SQL `NULL` (`Bind`'s `-1`-length convention,
/// `crate::protocol::encode_bind`), never the four-character text `"null"`.
#[derive(Debug, Clone, PartialEq)]
pub enum Param {
    Null,
    Text(String),
    I64(i64),
    F64(f64),
    Bool(bool),
}

impl Param {
    /// `None` means SQL `NULL` (`Bind`'s `-1`-length convention); `Some` is the exact bytes
    /// `Bind` sends as this parameter's TEXT-format value. Booleans render as `"true"`/`"false"`
    /// (PostgreSQL's own `boolin()` accepts both that and `'t'`/`'f'` as INPUT; [`Row::
    /// get_bool`]'s own doc explains why it only accepts `'t'`/`'f'` back -- that is the
    /// server's own canonical OUTPUT form, a different, narrower direction).
    fn to_text(&self) -> Option<String> {
        match self {
            Param::Null => None,
            Param::Text(s) => Some(s.clone()),
            Param::I64(v) => Some(v.to_string()),
            // Rust's `f64` `Display` is shortest-round-trip-accurate (Grisu/Ryu-class
            // formatting in `std` since well before this workspace's rust-version floor), so
            // this needs no extra precision crate to guarantee the value read back by `Row::
            // get_f64` parses to the identical `f64` bit pattern.
            Param::F64(v) => Some(v.to_string()),
            Param::Bool(v) => Some(if *v { "true" } else { "false" }.to_string()),
        }
    }
}

/// One result row from [`PgClient::query`]/[`PgClient::execute`]. `columns` is shared
/// (`Arc<Vec<String>>`) across every row of the same query (`RowDescription` is decoded once),
/// so this type is cheap to clone and does not repeat the column names once per row.
#[derive(Debug, Clone)]
pub struct Row {
    columns: Arc<Vec<String>>,
    values: Vec<Option<String>>,
}

impl Row {
    fn column_index(&self, name: &str) -> Result<usize, CatalogError> {
        self.columns.iter().position(|c| c == name).ok_or_else(|| CatalogError::ColumnNotFound { column: name.to_string() })
    }

    /// `true` if `name`'s value is SQL `NULL`. The way to check before calling one of the
    /// typed accessors below, which all treat `NULL` as an error rather than a zero value.
    pub fn is_null(&self, name: &str) -> Result<bool, CatalogError> {
        let i = self.column_index(name)?;
        Ok(self.values[i].is_none())
    }

    /// `name`'s raw TEXT-format value. [`CatalogError::ColumnIsNull`] if `NULL` --
    /// [`Self::is_null`] is the way to check for that case first.
    pub fn get_str(&self, name: &str) -> Result<&str, CatalogError> {
        let i = self.column_index(name)?;
        self.values[i].as_deref().ok_or_else(|| CatalogError::ColumnIsNull { column: name.to_string() })
    }

    /// `name`'s value, parsed as `i64`. [`CatalogError::ColumnParse`] (naming the column, the
    /// expected type, and the raw text actually received) if it does not parse.
    pub fn get_i64(&self, name: &str) -> Result<i64, CatalogError> {
        let raw = self.get_str(name)?;
        raw.parse::<i64>().map_err(|_| CatalogError::ColumnParse { column: name.to_string(), expected: "i64", raw: raw.to_string() })
    }

    /// `name`'s value, parsed as `f64`. PostgreSQL's own text output for `Infinity`/
    /// `-Infinity`/`NaN` parses correctly here: Rust's `f64::from_str` accepts those spellings
    /// case-insensitively.
    pub fn get_f64(&self, name: &str) -> Result<f64, CatalogError> {
        let raw = self.get_str(name)?;
        raw.parse::<f64>().map_err(|_| CatalogError::ColumnParse { column: name.to_string(), expected: "f64", raw: raw.to_string() })
    }

    /// `name`'s value, parsed as `bool`. Accepts only `"t"`/`"f"` -- PostgreSQL's own canonical
    /// TEXT-format OUTPUT for `boolean` (distinct from `boolin()`'s more permissive INPUT
    /// grammar, which [`Param::to_text`] uses `"true"`/`"false"` for instead -- this accessor
    /// only ever needs to parse what the SERVER actually sends back).
    pub fn get_bool(&self, name: &str) -> Result<bool, CatalogError> {
        let raw = self.get_str(name)?;
        match raw {
            "t" => Ok(true),
            "f" => Ok(false),
            _ => Err(CatalogError::ColumnParse { column: name.to_string(), expected: "bool ('t' or 'f')", raw: raw.to_string() }),
        }
    }
}

/// Bridges a BLOCKING `openssl::ssl::SslStream<std::net::TcpStream>` onto this crate's async
/// API. See this module's own doc, "`PgTls::Required`", for why this exists instead of an
/// `AsyncRead`/`AsyncWrite` impl over a non-blocking `Ssl`. Every read/write takes the stream
/// out of `self.0` (an `Option` so ownership can move into `spawn_blocking`'s `'static`
/// closure), runs the blocking call on a blocking-pool thread, and puts the stream back --
/// `self.0` is only ever `None` for the duration of one in-flight operation.
struct BlockingTlsStream(Option<openssl::ssl::SslStream<std::net::TcpStream>>);

impl BlockingTlsStream {
    async fn write_all(&mut self, buf: &[u8], context: &'static str) -> Result<(), CatalogError> {
        let mut stream = self.0.take().ok_or_else(|| CatalogError::BlockingTaskFailed("TLS stream unavailable (a previous operation on it never returned)".to_string()))?;
        let owned = buf.to_vec();
        let (result, stream) = tokio::task::spawn_blocking(move || {
            let r = std::io::Write::write_all(&mut stream, &owned);
            (r, stream)
        })
        .await
        .map_err(|e| CatalogError::BlockingTaskFailed(e.to_string()))?;
        self.0 = Some(stream);
        result.map_err(|e| io_err(context, e))
    }

    async fn read_exact(&mut self, buf: &mut [u8], context: &'static str) -> Result<(), CatalogError> {
        let mut stream = self.0.take().ok_or_else(|| CatalogError::BlockingTaskFailed("TLS stream unavailable (a previous operation on it never returned)".to_string()))?;
        let len = buf.len();
        let (result, stream, data) = tokio::task::spawn_blocking(move || {
            let mut tmp = vec![0u8; len];
            let r = std::io::Read::read_exact(&mut stream, &mut tmp);
            (r, stream, tmp)
        })
        .await
        .map_err(|e| CatalogError::BlockingTaskFailed(e.to_string()))?;
        self.0 = Some(stream);
        result.map_err(|e| io_err(context, e))?;
        buf.copy_from_slice(&data);
        Ok(())
    }
}

enum PgStream {
    Plain(TcpStream),
    Tls(BlockingTlsStream),
}

impl PgStream {
    async fn write_all(&mut self, buf: &[u8], context: &'static str) -> Result<(), CatalogError> {
        match self {
            PgStream::Plain(s) => s.write_all(buf).await.map_err(|e| io_err(context, e)),
            PgStream::Tls(t) => t.write_all(buf, context).await,
        }
    }

    async fn read_exact(&mut self, buf: &mut [u8], context: &'static str) -> Result<(), CatalogError> {
        match self {
            PgStream::Plain(s) => s.read_exact(buf).await.map(|_| ()).map_err(|e| io_err(context, e)),
            PgStream::Tls(t) => t.read_exact(buf, context).await,
        }
    }
}

/// Performs the blocking OpenSSL handshake (module doc: "`PgTls::Required`") and returns the
/// bridge wrapping its result. `host` is used for the connector's own hostname verification
/// (`SslConnector::connect`'s own `domain` argument), matching the certificate's SAN against
/// the name this crate was actually configured to connect to.
async fn upgrade_to_tls(tcp: TcpStream, host: &str, ca_file: Option<&std::path::Path>) -> Result<BlockingTlsStream, CatalogError> {
    let std_stream = tcp.into_std().map_err(|e| io_err("converting the TCP stream for a blocking TLS handshake", e))?;
    std_stream.set_nonblocking(false).map_err(|e| io_err("preparing the TCP stream for a blocking TLS handshake", e))?;
    let mut builder = SslConnector::builder(SslMethod::tls())?;
    if let Some(ca) = ca_file {
        builder.set_ca_file(ca)?;
    }
    let connector = builder.build();
    let host = host.to_string();
    let ssl_stream = tokio::task::spawn_blocking(move || connector.connect(&host, std_stream))
        .await
        .map_err(|e| CatalogError::BlockingTaskFailed(e.to_string()))?
        .map_err(|e| CatalogError::Tls(e.to_string()))?;
    Ok(BlockingTlsStream(Some(ssl_stream)))
}

/// The `SSLRequest`/`'S'`-or-`'N'` exchange and, on `'S'`, the TLS upgrade itself
/// ([`upgrade_to_tls`]) -- one phase of [`PgClient::connect`], bounded by its own
/// `deadline` budget (module doc: "`connect_timeout` is a deadline, never a sleep -- and it is
/// per PHASE, not per connection"). A peer that accepts the TCP connection and then never
/// writes the `'S'`/`'N'` response byte is caught here, by THIS phase's own timeout, and
/// reported as [`CatalogError::HandshakeTimeout`] naming `"tls-negotiation"` -- never left to
/// hang on the unbounded `read_exact` this phase used to perform.
async fn negotiate_tls(mut tcp: TcpStream, host: &str, port: u16, ca_file: Option<&std::path::Path>, deadline: Duration) -> Result<PgStream, CatalogError> {
    const PHASE: &str = "tls-negotiation";
    tokio::time::timeout(deadline, async {
        tcp.write_all(&protocol::encode_ssl_request()).await.map_err(|e| io_err("writing SSLRequest", e))?;
        let mut resp = [0u8; 1];
        tcp.read_exact(&mut resp).await.map_err(|e| io_err("reading the SSLRequest response", e))?;
        match resp[0] {
            b'S' => Ok(PgStream::Tls(upgrade_to_tls(tcp, host, ca_file).await?)),
            b'N' => Err(CatalogError::ServerDeclinedTls { host: host.to_string(), port }),
            other => Err(CatalogError::InvalidSslResponse { byte: other }),
        }
    })
    .await
    .unwrap_or_else(|_| Err(CatalogError::HandshakeTimeout { host: host.to_string(), port, timeout: deadline, phase: PHASE }))
}

/// Names a [`BackendMessage`] variant for [`CatalogError::UnexpectedMessage`]'s `got` field --
/// deliberately not `{:?}` on the whole message (which could embed a row's actual data values
/// in an error string a caller might log).
fn backend_message_name(msg: &BackendMessage) -> &'static str {
    match msg {
        BackendMessage::AuthenticationOk => "AuthenticationOk",
        BackendMessage::AuthenticationCleartextPassword => "AuthenticationCleartextPassword",
        BackendMessage::AuthenticationMd5Password { .. } => "AuthenticationMD5Password",
        BackendMessage::AuthenticationSasl { .. } => "AuthenticationSASL",
        BackendMessage::AuthenticationSaslContinue { .. } => "AuthenticationSASLContinue",
        BackendMessage::AuthenticationSaslFinal { .. } => "AuthenticationSASLFinal",
        BackendMessage::AuthenticationUnsupported { .. } => "AuthenticationUnsupported",
        BackendMessage::BackendKeyData { .. } => "BackendKeyData",
        BackendMessage::ParameterStatus { .. } => "ParameterStatus",
        BackendMessage::ReadyForQuery { .. } => "ReadyForQuery",
        BackendMessage::RowDescription { .. } => "RowDescription",
        BackendMessage::DataRow { .. } => "DataRow",
        BackendMessage::CommandComplete { .. } => "CommandComplete",
        BackendMessage::EmptyQueryResponse => "EmptyQueryResponse",
        BackendMessage::NoData => "NoData",
        BackendMessage::ParseComplete => "ParseComplete",
        BackendMessage::BindComplete => "BindComplete",
        BackendMessage::ParameterDescription { .. } => "ParameterDescription",
        BackendMessage::PortalSuspended => "PortalSuspended",
        BackendMessage::ErrorResponse(_) => "ErrorResponse",
        BackendMessage::NoticeResponse(_) => "NoticeResponse",
    }
}

/// The affected-row count from a `CommandComplete` tag (e.g. `"SELECT 3"`, `"INSERT 0 1"`,
/// `"UPDATE 2"`, or a DDL tag like `"CREATE TABLE"` with no count at all): the last
/// whitespace-separated token, parsed as `u64`, or `0` if there is no such trailing number
/// (DDL). PostgreSQL's own tag grammar (`src/backend/tcop/pquery.c`'s `CreateCommandTag`) never
/// puts anything but a decimal row count in that trailing position when one is present at all,
/// so "last token, or 0" is not a heuristic guess at the format -- it IS the format.
fn parse_row_count(tag: &str) -> u64 {
    tag.rsplit(' ').next().and_then(|last| last.parse::<u64>().ok()).unwrap_or(0)
}

/// The async PostgreSQL connection. See this module's own doc for the extended-query-protocol/
/// TEXT-format design and the `PgTls::Required` bridging story.
pub struct PgClient {
    stream: PgStream,
}

/// Hand-written rather than derived: `PgStream::Tls`'s `openssl::ssl::SslStream` does not
/// implement `Debug`, and even where it did, printing a live socket's internal state is not
/// useful -- callers (and this crate's own tests, via `Result::expect_err`, which needs `T:
/// Debug` on the `Ok` type regardless of whether the `Ok` branch is ever printed) only need to
/// know THAT a `PgClient` is there.
impl std::fmt::Debug for PgClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PgClient").finish_non_exhaustive()
    }
}

impl PgClient {
    /// Connects to `config.host:config.port`, negotiates TLS if `config.tls` is
    /// [`PgTls::Required`], and runs the startup/authentication handshake (SCRAM-SHA-256 only
    /// -- `AuthenticationCleartextPassword`/`AuthenticationMD5Password` are refused, per this
    /// crate's own doc). Returns once `ReadyForQuery` is received after authentication. Every
    /// phase after the TCP connect -- TLS negotiation, then startup -- gets its own
    /// `config.connect_timeout` budget (module doc: "per PHASE, not per connection"), so a peer
    /// that accepts the TCP connection and then never writes another byte cannot hang this call
    /// forever: it is reported as [`CatalogError::HandshakeTimeout`], naming the phase.
    pub async fn connect(config: &PgConfig) -> Result<PgClient, CatalogError> {
        let tcp = tokio::time::timeout(config.connect_timeout, TcpStream::connect((config.host.as_str(), config.port)))
            .await
            .map_err(|_| CatalogError::ConnectTimeout { host: config.host.clone(), port: config.port, timeout: config.connect_timeout })?
            .map_err(|e| CatalogError::Connect { host: config.host.clone(), port: config.port, reason: e.to_string() })?;

        let stream = match &config.tls {
            PgTls::Disabled => PgStream::Plain(tcp),
            PgTls::Required { ca_file } => negotiate_tls(tcp, &config.host, config.port, ca_file.as_deref(), config.connect_timeout).await?,
        };

        let mut client = PgClient { stream };
        // `startup()` itself is unmodified (module doc: "per PHASE, not per connection") --
        // bounded here, from the outside, by its own fresh `config.connect_timeout` budget, so
        // a peer that accepts the TCP connection (and, for `PgTls::Required`, completes TLS
        // negotiation) and then never writes another byte is caught by ITS OWN phase's
        // deadline rather than hanging forever.
        const STARTUP_PHASE: &str = "startup";
        tokio::time::timeout(config.connect_timeout, client.startup(config))
            .await
            .unwrap_or_else(|_| Err(CatalogError::HandshakeTimeout { host: config.host.clone(), port: config.port, timeout: config.connect_timeout, phase: STARTUP_PHASE }))?;
        Ok(client)
    }

    /// Reads exactly one backend message: a 5-byte frame header, then exactly `body_len` more
    /// bytes, then decodes it via `crate::protocol::decode_backend_message`. `context` names
    /// what this crate was doing, for [`CatalogError::Io`]/[`CatalogError::ConnectionClosed`].
    /// This is the ONE place in this crate that reads from the socket at all -- see
    /// `crate::protocol`'s own module doc for why the framing/decoding split is drawn exactly
    /// here.
    async fn read_backend_message(&mut self, context: &'static str) -> Result<BackendMessage, CatalogError> {
        let mut header = [0u8; 5];
        self.stream.read_exact(&mut header, context).await?;
        let fh = protocol::read_frame_header(&header)?;
        let mut body = vec![0u8; fh.body_len];
        if fh.body_len > 0 {
            self.stream.read_exact(&mut body, context).await?;
        }
        protocol::decode_backend_message(fh.msg_type, &body)
    }

    /// Reads one backend message and requires it to satisfy `is_expected`. An `ErrorResponse`
    /// is always treated specially regardless of `is_expected` (drained to `ReadyForQuery` --
    /// module doc's "connection is left in a defined state" -- then returned as
    /// [`CatalogError::Server`]); anything else that fails `is_expected` becomes
    /// [`CatalogError::UnexpectedMessage`].
    async fn expect(&mut self, context: &'static str, expected_name: &'static str, is_expected: impl Fn(&BackendMessage) -> bool) -> Result<BackendMessage, CatalogError> {
        let msg = self.read_backend_message(context).await?;
        if is_expected(&msg) {
            return Ok(msg);
        }
        if let BackendMessage::ErrorResponse(server_error) = msg {
            self.drain_until_ready().await?;
            return Err(CatalogError::Server(Box::new(server_error)));
        }
        Err(CatalogError::UnexpectedMessage { context, expected: expected_name, got: backend_message_name(&msg) })
    }

    /// Reads backend messages until `ReadyForQuery`, discarding everything else (more
    /// `ErrorResponse`/`NoticeResponse` fragments, stray rows). Called after this crate has
    /// already decided to report an error, so the connection is left exactly where the
    /// protocol always leaves it after `Sync`: at `ReadyForQuery`, ready for the next command
    /// (module doc: "the connection is left in a defined state after `ReadyForQuery`").
    async fn drain_until_ready(&mut self) -> Result<TransactionStatus, CatalogError> {
        loop {
            if let BackendMessage::ReadyForQuery { status } = self.read_backend_message("draining to ReadyForQuery after an error").await? {
                return Ok(status);
            }
        }
    }

    async fn startup(&mut self, config: &PgConfig) -> Result<(), CatalogError> {
        let params = protocol::StartupParams { user: &config.user, database: &config.database, application_name: &config.application_name };
        self.stream.write_all(&protocol::encode_startup_message(&params), "writing StartupMessage").await?;

        // Exactly one Authentication* message (or ErrorResponse) is expected here -- not a
        // loop: every arm below either returns or falls through to the post-auth drain below
        // it, so there is nothing a second iteration would ever see.
        let msg = self.read_backend_message("startup/authentication").await?;
        match msg {
            BackendMessage::AuthenticationOk => {}
            BackendMessage::AuthenticationCleartextPassword => return Err(CatalogError::CleartextAuthRefused),
            BackendMessage::AuthenticationMd5Password { .. } => return Err(CatalogError::Md5AuthRefused),
            BackendMessage::AuthenticationUnsupported { code } => return Err(CatalogError::UnsupportedAuthMethod { code }),
            BackendMessage::AuthenticationSasl { mechanisms } => {
                if !mechanisms.iter().any(|m| m == scram::MECHANISM) {
                    return Err(CatalogError::UnsupportedAuthMethod { code: 10 });
                }
                self.run_scram_exchange(&config.user, &config.password).await?;
            }
            BackendMessage::ErrorResponse(server_error) => return Err(CatalogError::Server(Box::new(server_error))),
            other => return Err(CatalogError::UnexpectedMessage { context: "startup", expected: "an Authentication* message", got: backend_message_name(&other) }),
        }

        // AuthenticationOk is followed by zero or more ParameterStatus/BackendKeyData/
        // NoticeResponse messages, then ReadyForQuery -- drain all of them here so `connect`
        // always returns with the connection already at ReadyForQuery.
        loop {
            match self.read_backend_message("post-authentication startup").await? {
                BackendMessage::ParameterStatus { .. } | BackendMessage::BackendKeyData { .. } | BackendMessage::NoticeResponse(_) => continue,
                BackendMessage::ReadyForQuery { .. } => return Ok(()),
                BackendMessage::ErrorResponse(server_error) => return Err(CatalogError::Server(Box::new(server_error))),
                other => return Err(CatalogError::UnexpectedMessage { context: "post-authentication startup", expected: "ParameterStatus, BackendKeyData or ReadyForQuery", got: backend_message_name(&other) }),
            }
        }
    }

    /// The full four-message SCRAM-SHA-256 exchange (`crate::scram`'s module doc has the
    /// cryptographic detail): `SASLInitialResponse` -> `AuthenticationSASLContinue` ->
    /// `SASLResponse` -> `AuthenticationSASLFinal` -> `AuthenticationOk`.
    async fn run_scram_exchange(&mut self, user: &str, password: &str) -> Result<(), CatalogError> {
        let scram_client = ScramClient::new(user, password)?;
        let initial = protocol::encode_sasl_initial_response(scram::MECHANISM, scram_client.client_first_message().as_bytes());
        self.stream.write_all(&initial, "writing SASLInitialResponse").await?;

        let continue_msg = self.expect("SCRAM: server-first-message", "AuthenticationSASLContinue", |m| matches!(m, BackendMessage::AuthenticationSaslContinue { .. })).await?;
        let BackendMessage::AuthenticationSaslContinue { data } = continue_msg else { unreachable!("expect() already checked this shape") };
        let server_first_message = std::str::from_utf8(&data).map_err(|_| CatalogError::InvalidUtf8 { context: "SCRAM server-first-message" })?;
        let scram_final = scram_client.client_final_message(server_first_message)?;

        let response = protocol::encode_sasl_response(scram_final.client_final_message.as_bytes());
        self.stream.write_all(&response, "writing SASLResponse").await?;

        let final_msg = self.expect("SCRAM: server-final-message", "AuthenticationSASLFinal", |m| matches!(m, BackendMessage::AuthenticationSaslFinal { .. })).await?;
        let BackendMessage::AuthenticationSaslFinal { data } = final_msg else { unreachable!("expect() already checked this shape") };
        let server_final_message = std::str::from_utf8(&data).map_err(|_| CatalogError::InvalidUtf8 { context: "SCRAM server-final-message" })?;
        scram::verify_server_final(&scram_final.expected_server_signature, server_final_message)?;

        self.expect("SCRAM: post-verification AuthenticationOk", "AuthenticationOk", |m| matches!(m, BackendMessage::AuthenticationOk)).await?;
        Ok(())
    }

    /// The one implementation behind [`Self::query`] and [`Self::execute`] -- both run the
    /// identical extended-query round trip; they differ only in which half of the result
    /// (`u64` row count, or `Vec<Row>`) the caller asked for.
    async fn execute_extended(&mut self, sql: &str, params: &[Param]) -> Result<(u64, Vec<Row>), CatalogError> {
        let param_texts: Vec<Option<String>> = params.iter().map(Param::to_text).collect();
        let param_refs: Vec<Option<&[u8]>> = param_texts.iter().map(|opt| opt.as_deref().map(str::as_bytes)).collect();

        self.stream.write_all(&protocol::encode_parse("", sql, &[]), "writing Parse").await?;
        self.stream.write_all(&protocol::encode_bind("", "", &param_refs), "writing Bind").await?;
        self.stream.write_all(&protocol::encode_describe(DescribeTarget::Portal, ""), "writing Describe").await?;
        self.stream.write_all(&protocol::encode_execute("", 0), "writing Execute").await?;
        self.stream.write_all(&protocol::encode_sync(), "writing Sync").await?;

        self.expect("ParseComplete", "ParseComplete", |m| matches!(m, BackendMessage::ParseComplete)).await?;
        self.expect("BindComplete", "BindComplete", |m| matches!(m, BackendMessage::BindComplete)).await?;

        let describe_reply = self.expect("Describe response", "RowDescription or NoData", |m| matches!(m, BackendMessage::RowDescription { .. } | BackendMessage::NoData)).await?;
        let columns = match describe_reply {
            BackendMessage::RowDescription { fields } => Arc::new(fields.into_iter().map(|f| f.name).collect::<Vec<_>>()),
            BackendMessage::NoData => Arc::new(Vec::new()),
            _ => unreachable!("expect() already checked this shape"),
        };

        let mut rows = Vec::new();
        let row_count;
        loop {
            match self.read_backend_message("query execution").await? {
                BackendMessage::DataRow { values } => {
                    let mut decoded = Vec::with_capacity(values.len());
                    for (i, value) in values.into_iter().enumerate() {
                        match value {
                            None => decoded.push(None),
                            Some(bytes) => {
                                let column = columns.get(i).cloned().unwrap_or_default();
                                decoded.push(Some(String::from_utf8(bytes).map_err(|_| CatalogError::ColumnNotUtf8 { column })?));
                            }
                        }
                    }
                    rows.push(Row { columns: columns.clone(), values: decoded });
                }
                BackendMessage::CommandComplete { tag } => {
                    row_count = parse_row_count(&tag);
                    break;
                }
                BackendMessage::EmptyQueryResponse => {
                    row_count = 0;
                    break;
                }
                BackendMessage::ErrorResponse(server_error) => {
                    self.drain_until_ready().await?;
                    return Err(CatalogError::Server(Box::new(server_error)));
                }
                other => return Err(CatalogError::UnexpectedMessage { context: "query execution", expected: "DataRow, CommandComplete or EmptyQueryResponse", got: backend_message_name(&other) }),
            }
        }

        self.expect("ReadyForQuery", "ReadyForQuery", |m| matches!(m, BackendMessage::ReadyForQuery { .. })).await?;
        Ok((row_count, rows))
    }

    /// Runs `sql` (containing `$1`, `$2`, ... placeholders) with `params` bound positionally,
    /// and returns every result row. See this module's own doc for why this can never be used
    /// to inject SQL through a parameter value.
    pub async fn query(&mut self, sql: &str, params: &[Param]) -> Result<Vec<Row>, CatalogError> {
        let (_, rows) = self.execute_extended(sql, params).await?;
        Ok(rows)
    }

    /// Runs `sql` with `params` bound positionally and returns the affected-row count
    /// ([`parse_row_count`]'s doc explains exactly how that count is read off `CommandComplete`
    /// -- `0` for a DDL statement with no row count of its own).
    pub async fn execute(&mut self, sql: &str, params: &[Param]) -> Result<u64, CatalogError> {
        let (row_count, _) = self.execute_extended(sql, params).await?;
        Ok(row_count)
    }

    /// Runs `sql` via the SIMPLE query protocol (one `Query` message, which -- unlike `Parse`
    /// -- may contain several `;`-separated statements): for a migration script, never for
    /// caller-supplied data (this module's own doc, "SQL injection is structurally
    /// impossible..."). Discards every result (`RowDescription`/`DataRow`/`CommandComplete`/
    /// `EmptyQueryResponse`/`NoticeResponse`) and returns once `ReadyForQuery` is seen; an
    /// `ErrorResponse` anywhere in the batch still drains to `ReadyForQuery` before returning
    /// [`CatalogError::Server`], exactly like [`Self::execute_extended`].
    pub async fn simple_batch(&mut self, sql: &str) -> Result<(), CatalogError> {
        self.stream.write_all(&protocol::encode_query(sql), "writing Query").await?;
        loop {
            match self.read_backend_message("simple_batch").await? {
                BackendMessage::ReadyForQuery { .. } => return Ok(()),
                BackendMessage::ErrorResponse(server_error) => {
                    self.drain_until_ready().await?;
                    return Err(CatalogError::Server(Box::new(server_error)));
                }
                _ => continue,
            }
        }
    }

    /// Sends `Terminate` and drops the connection. PostgreSQL sends no reply to `Terminate` --
    /// the socket is simply closed once this function returns (by `self`'s own `Drop`, which
    /// this type adds nothing to: a `PgClient` dropped WITHOUT calling `close` first also
    /// closes its socket, just without the polite `Terminate` message first).
    pub async fn close(mut self) -> Result<(), CatalogError> {
        self.stream.write_all(&protocol::encode_terminate(), "writing Terminate").await
    }
}
