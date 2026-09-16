//! `CatalogError`: the one error type for this crate (mirrors `crates/av-store/src/error.rs`'s
//! own "one typed error enum per crate" precedent -- see that module's own doc comment). Every
//! refusal named anywhere in this crate's other modules -- a truncated or malformed wire
//! frame, a SCRAM check that fails, a connection that cannot be established, a column a caller
//! asked for in the wrong shape -- is a distinct variant here, sectioned by the module that
//! raises it, never a shared "other" catch-all string.
//!
//! `openssl::error::ErrorStack` is not `Clone`/`PartialEq` (the same reason
//! `crates/av-store/src/error.rs` gives, itself mirroring `crates/av-edge/src/sign.rs::
//! SigningError::Openssl`'s precedent): every variant that wraps one stringifies it
//! immediately (`CatalogError::Openssl(String)`) rather than storing the `ErrorStack` itself.
//! `std::io::Error` does not have this problem (this enum derives neither `Clone` nor
//! `PartialEq`, so there is nothing forcing it to be stringified too), so `CatalogError::Io`
//! wraps the real `io::Error` -- callers that want `.kind()` (e.g. to special-case
//! `ErrorKind::UnexpectedEof`) still can.

use crate::protocol::ServerError;

/// Everything that can go wrong anywhere in `av-catalog`. See each variant's own doc for the
/// exact module/function that raises it.
#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    // -- src/protocol.rs (wire framing) -------------------------------------------------
    /// A `decode_*`/`Reader` call in `src/protocol.rs` needed `need` more bytes than `context`
    /// had left. Raised for every message type, at every truncation length, by that module's
    /// own `Reader` -- see its doc for why this is the ONE place this check is implemented.
    #[error("{context}: truncated (needed {need} more byte(s), had {have})")]
    Truncated { context: &'static str, need: usize, have: usize },
    /// A `Reader::read_cstr` call in `src/protocol.rs` found no NUL terminator before the
    /// slice it was reading ended.
    #[error("{context}: NUL-terminated string has no terminator within the frame")]
    InvalidCString { context: &'static str },
    /// A `Reader::read_cstr` call in `src/protocol.rs` found a NUL-terminated byte run that is
    /// not valid UTF-8.
    #[error("{context}: string is not valid UTF-8")]
    InvalidUtf8 { context: &'static str },
    /// `Reader::finish` in `src/protocol.rs` found `extra` unconsumed bytes after decoding
    /// every field a message's own type defines -- the frame's length prefix promised more
    /// than the message's own layout needed, which is exactly as malformed as promising too
    /// few.
    #[error("{context}: {extra} trailing byte(s) after every known field was decoded (length prefix does not match the message's own layout)")]
    TrailingBytes { context: &'static str, extra: usize },
    /// An `Int16` count field in `src/protocol.rs` (`Bind`'s three counts, `RowDescription`'s
    /// field count, `DataRow`'s column count, `ParameterDescription`'s count) was negative --
    /// never valid, and never cast to `usize` without this check first (a cast alone would
    /// wrap around to an enormous number instead of erroring).
    #[error("{context}: count is negative ({value})")]
    NegativeCount { context: &'static str, value: i32 },
    /// [`crate::protocol::read_frame_header`]/[`crate::protocol::read_untyped_frame_header`]:
    /// `length` is either less than 4 (a length field must include itself) or over
    /// [`crate::protocol::MAX_MESSAGE_LEN`].
    #[error("{context}: invalid frame length {length}")]
    InvalidFrameLength { context: &'static str, length: i32 },
    /// [`crate::protocol::decode_ssl_request_code`]: the 4 bytes after `SSLRequest`'s length
    /// field were not [`crate::protocol::SSL_REQUEST_CODE`].
    #[error("SSLRequest: invalid request code {code} (expected {})", crate::protocol::SSL_REQUEST_CODE)]
    InvalidSslRequestCode { code: i32 },
    /// [`crate::protocol::decode_backend_message`]: `ReadyForQuery`'s status byte was not one
    /// of `'I'`/`'T'`/`'E'`.
    #[error("ReadyForQuery: invalid transaction status byte {byte:#04x}")]
    InvalidTransactionStatus { byte: u8 },
    /// [`crate::protocol::decode_describe`]: `Describe`'s target byte was not one of `'S'`/`'P'`.
    #[error("Describe: invalid target byte {byte:#04x} (expected 'S' or 'P')")]
    InvalidDescribeTarget { byte: u8 },
    /// [`crate::protocol::decode_backend_message`]: `msg_type` is not one of the message types
    /// this module recognises for `direction` ("backend" today -- this crate's frontend decode
    /// functions are each named for one specific message and so have no "unknown type" case of
    /// their own).
    #[error("unknown {direction} message type {type_byte:#04x} ({type_byte:?})")]
    UnknownMessageType { type_byte: u8, direction: &'static str },
    /// [`crate::protocol::decode_server_error`]: an `ErrorResponse`/`NoticeResponse` was
    /// missing one of its three mandatory fields (`'S'`/`'V'` severity, `'C'` SQLSTATE, `'M'`
    /// message) -- every OTHER field code, recognised or not, is optional per the protocol's
    /// own documented rule.
    #[error("{context}: missing mandatory field {code:?}")]
    MissingErrorField { context: &'static str, code: char },

    // -- src/scram.rs (SCRAM-SHA-256) ----------------------------------------------------
    /// [`crate::scram::ScramClient::new`]/[`crate::scram::ScramClient::with_client_nonce`]:
    /// SASLprep (RFC 4013) is not implemented (`src/scram.rs`'s module doc explains why), so a
    /// password containing any non-ASCII byte is refused here, at configuration time, rather
    /// than hashed with an un-normalized byte sequence that would silently authenticate
    /// against the wrong `SaltedPassword` (or, worse, happen to work today and stop working
    /// the moment the password is retyped on a different keyboard layout/OS that normalizes
    /// Unicode differently).
    #[error("password contains a non-ASCII byte; SASLprep is not implemented (see src/scram.rs's module doc), so this password cannot be hashed correctly and is refused rather than silently mis-hashed")]
    NonAsciiPassword,
    /// [`crate::scram::ScramClient::process_server_first`]: the server-first-message could not
    /// be parsed into its `r=`/`s=`/`i=` attributes.
    #[error("SCRAM server-first-message is malformed: {reason}")]
    ScramServerFirstMalformed { reason: &'static str },
    /// [`crate::scram::ScramClient::process_server_first`]: the server's nonce does not start
    /// with the client's own nonce. This is SCRAM's actual defence against a MITM substituting
    /// its own server-first-message (RFC 5802 section 5: the client MUST verify this), so it
    /// is never skipped or downgraded to a warning.
    #[error("SCRAM server nonce does not start with the client nonce (possible MITM); refusing to continue")]
    ScramServerNonceMismatch,
    /// [`crate::scram::ScramClient::process_server_first`]: `i=` was not a positive base-10
    /// integer.
    #[error("SCRAM iteration count {value:?} is not a positive integer")]
    ScramInvalidIterationCount { value: String },
    /// [`crate::scram::ScramClient::process_server_first`]: `s=` was not valid base64.
    #[error("SCRAM salt is not valid base64: {reason}")]
    ScramInvalidSalt { reason: String },
    /// [`crate::scram::ScramClient::verify_server_final`]: the server-final-message could not
    /// be parsed into its `v=` attribute (or carried `e=`, an explicit server-reported SCRAM
    /// failure, whose text is included here).
    #[error("SCRAM server-final-message is malformed: {reason}")]
    ScramServerFinalMalformed { reason: String },
    /// [`crate::scram::ScramClient::verify_server_final`]: the server's `ServerSignature` does
    /// not match what this client independently computed from the same `SaltedPassword`. This
    /// is SCRAM's proof that the server actually knows the client's `SaltedPassword` (and not
    /// just a copy of `StoredKey`, which is all a compromised server's database would leak) --
    /// compared in constant time (`openssl::memcmp::eq`) since it is, in the end, a MAC
    /// comparison. A wrong password on the CLIENT side also lands here (see `src/scram.rs`'s
    /// own known-answer test module for why: a client that hashed the wrong password computes
    /// a different `ServerSignature` than the real server did, so this check catches both a
    /// hostile server and the client's own bad password with the one comparison).
    #[error("SCRAM server signature does not match (server signature verification failed)")]
    ScramServerSignatureMismatch,

    // -- src/client.rs (connection, auth refusal, extended query protocol) --------------
    /// [`crate::client::PgClient::connect`]: `TcpStream::connect` failed outright (refused,
    /// unreachable, DNS failure).
    #[error("connecting to {host}:{port}: {reason}")]
    Connect { host: String, port: u16, reason: String },
    /// [`crate::client::PgClient::connect`]: `PgConfig::connect_timeout` elapsed before the TCP
    /// connection completed. `tokio::time::timeout` wraps the connect future as a deadline --
    /// never a `sleep` used as a synchronisation device (rule 7).
    #[error("connecting to {host}:{port}: timed out after {timeout:?}")]
    ConnectTimeout { host: String, port: u16, timeout: std::time::Duration },
    /// [`crate::client::PgClient::connect`]: the server replied to `SSLRequest` with neither
    /// `'S'` nor `'N'` -- not a protocol PostgreSQL 3.0 defines.
    #[error("server replied to SSLRequest with byte {byte:#04x} (expected 'S' or 'N')")]
    InvalidSslResponse { byte: u8 },
    /// [`crate::client::PgClient::connect`]: `PgConfig::tls` is [`crate::client::PgTls::
    /// Required`] but the server replied `'N'` to `SSLRequest`. Never silently downgraded to a
    /// plaintext connection -- a caller that asked for TLS gets a typed refusal instead of an
    /// unencrypted session it did not ask for.
    #[error("TLS required but server at {host}:{port} declined SSLRequest ('N')")]
    ServerDeclinedTls { host: String, port: u16 },
    /// [`crate::client::PgClient::connect`]'s `PgTls::Required` path: building the
    /// `SslConnector` (`set_ca_file`, `SslConnector::builder`) or performing the TLS handshake
    /// itself failed. Wraps `openssl::ssl::Error`/`openssl::error::ErrorStack`, stringified
    /// immediately (see this module's own doc).
    #[error("TLS: {0}")]
    Tls(String),
    /// Any other `openssl` primitive in this crate (`src/scram.rs`'s HMAC/PBKDF2/random-bytes
    /// calls) failed. Wraps `openssl::error::ErrorStack`, stringified immediately (see this
    /// module's own doc).
    #[error("OpenSSL error: {0}")]
    Openssl(String),
    /// [`crate::client::PgClient::connect`]: the server's `Authentication*` reply was
    /// `AuthenticationCleartextPassword`. Recognised and typed-refused, never implemented:
    /// ADR-004 requires SCRAM-SHA-256 for every connection this workspace makes, and a server
    /// asking for a cleartext password is either misconfigured or actively downgrading the
    /// authentication method -- this client reports that as a deployment defect rather than
    /// working around it by sending a password in the clear.
    #[error("server requested cleartext password authentication, which this client refuses to send (ADR-004 requires SCRAM-SHA-256); this is a pg_hba.conf misconfiguration, not something to route around")]
    CleartextAuthRefused,
    /// [`crate::client::PgClient::connect`]: the server's `Authentication*` reply was
    /// `AuthenticationMD5Password`. Recognised and typed-refused, never implemented: ADR-004
    /// bans MD5 outright (`deny.toml`'s `md-5`/`md5` ban), so a server configured for MD5 auth
    /// is a deployment defect this client reports rather than works around (this task's brief,
    /// verbatim).
    #[error("server requested MD5 password authentication, which ADR-004 bans outright; this is a pg_hba.conf misconfiguration (should be scram-sha-256, per services/catalog/IMAGE_DIGEST.md's measured pg_hba.conf), not something to route around")]
    Md5AuthRefused,
    /// [`crate::client::PgClient::connect`]: the server's `Authentication*` reply was one this
    /// crate does not implement and never will without a real need (Kerberos/SCM
    /// credential/GSSAPI/SSPI -- none of them relevant to a PostgreSQL container reached over
    /// loopback or this workspace's own network).
    #[error("server requested an unsupported authentication method (code {code}); only SCRAM-SHA-256 is implemented")]
    UnsupportedAuthMethod { code: i32 },
    /// A backend message arrived somewhere this crate's connection/auth/query state machine
    /// did not expect it (e.g. a `DataRow` before any `RowDescription`, or a second
    /// `AuthenticationOk`). Names both what was expected and what arrived, by their message
    /// name.
    #[error("protocol violation in {context}: expected {expected}, got {got}")]
    UnexpectedMessage { context: &'static str, expected: &'static str, got: &'static str },
    /// The server sent `ErrorResponse` at some point in the connection/auth/query sequence.
    /// Carries the full [`ServerError`] -- SQLSTATE (`.sqlstate`) is a first-class field a
    /// caller can match on, never a substring of `.message`. Boxed: `ServerError` carries 14
    /// `String`/`Option<String>` fields (>300 bytes inline), which would otherwise make EVERY
    /// `Result<_, CatalogError>` in this crate pay that size on its stack frame regardless of
    /// which variant is actually returned (`clippy::result_large_err`, a hard error under this
    /// workspace's `-D warnings` gate) -- boxing this one large variant, rather than shrinking
    /// `ServerError` itself (every one of its fields is exactly what this task's brief asks
    /// [`ServerError`] to carry), is the fix that costs this crate's actual error paths
    /// nothing: an `ErrorResponse` is already the "slow path" of one allocation per query
    /// error, never the common case.
    #[error("server error [{}]: {}", .0.sqlstate, .0.message)]
    Server(Box<ServerError>),
    /// The connection closed (a clean TCP FIN/EOF, or a reset) while this crate was in the
    /// middle of reading `context`. Distinct from [`CatalogError::Io`] so a caller can tell "the
    /// server closed the connection" apart from "the local socket errored" without inspecting
    /// an `io::Error`'s `.kind()`.
    #[error("connection closed by peer while reading {context}")]
    ConnectionClosed { context: &'static str },
    /// A local I/O error (not a clean close -- see [`CatalogError::ConnectionClosed`] for
    /// that) while reading or writing `context`.
    #[error("I/O error ({context}): {source}")]
    Io { context: &'static str, source: std::io::Error },
    /// [`crate::client::PgClient::connect`]'s `PgTls::Required` path: the `tokio::task::
    /// spawn_blocking` task bridging blocking `openssl::ssl::SslStream` I/O onto this crate's
    /// async API (`src/client.rs`'s module doc explains why) panicked instead of returning --
    /// this is the one failure mode `spawn_blocking`'s own `JoinError` can report that is not
    /// already a `CatalogError::Tls`/`CatalogError::Io`.
    #[error("TLS I/O worker task failed: {0}")]
    BlockingTaskFailed(String),

    // -- src/client.rs (Row/Param) --------------------------------------------------------
    /// [`crate::client::Row`]'s accessors: `column` is not one of the names
    /// [`crate::protocol::BackendMessage::RowDescription`] reported for this query.
    #[error("column {column:?} not found in this row")]
    ColumnNotFound { column: String },
    /// [`crate::client::Row`]'s typed accessors (`get_str`/`get_i64`/`get_f64`/`get_bool`):
    /// `column` exists but its value is SQL `NULL`. [`crate::client::Row::is_null`] is the way
    /// to check first if `NULL` is expected; these accessors treat it as an error rather than
    /// silently returning a zero value.
    #[error("column {column:?} is NULL")]
    ColumnIsNull { column: String },
    /// [`crate::client::Row`]'s text-format value for `column` was not valid UTF-8 -- would
    /// mean the server sent something other than the `client_encoding=UTF8` this crate's
    /// `StartupMessage` always declares.
    #[error("column {column:?} value is not valid UTF-8")]
    ColumnNotUtf8 { column: String },
    /// [`crate::client::Row::get_i64`]/`get_f64`/`get_bool`: `column`'s text value did not
    /// parse as the requested type. Names the column, the expected shape, and the raw text
    /// PostgreSQL actually sent, so a caller sees exactly what it got (this task's brief,
    /// verbatim: "a typed error naming the column and what it actually contained").
    #[error("column {column:?}: expected {expected}, got {raw:?}")]
    ColumnParse { column: String, expected: &'static str, raw: String },
}

impl From<openssl::error::ErrorStack> for CatalogError {
    fn from(e: openssl::error::ErrorStack) -> Self {
        CatalogError::Openssl(e.to_string())
    }
}

/// Only [`crate::client`] raises this (`src/protocol.rs` never touches an `io::Error` -- no
/// I/O in that module, see its own doc). `context` names what was being read/written at the
/// call site; kept as a small helper (rather than a bare `#[from]`) because every call site
/// needs to say which read/write failed, which a blanket `From` conversion cannot express.
pub(crate) fn io_err(context: &'static str, source: std::io::Error) -> CatalogError {
    if source.kind() == std::io::ErrorKind::UnexpectedEof {
        CatalogError::ConnectionClosed { context }
    } else {
        CatalogError::Io { context, source }
    }
}
