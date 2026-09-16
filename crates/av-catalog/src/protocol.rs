//! PostgreSQL frontend/backend protocol, version 3.0 -- pure, synchronous, allocation-explicit
//! message framing over byte buffers. See `src/lib.rs`'s crate doc for why this crate speaks
//! the wire protocol itself instead of depending on an existing client.
//!
//! # No I/O in this module
//!
//! Every `encode_*` function takes already-owned values and returns a `Vec<u8>`; every
//! `decode_*` function takes an already-collected `&[u8]` (never a socket, never anything
//! `async`) and returns a typed [`crate::error::CatalogError`] or the decoded value. This is
//! deliberate, not an accident of API shape: it is what makes every message type exhaustively
//! testable -- including every truncation of every message -- without a server, a socket, or
//! an executor anywhere in this file's own test module. `src/client.rs` is the one place in
//! this crate that owns a socket; it calls into this module only after it has already read
//! the exact number of bytes a message's own length prefix promised.
//!
//! # Framing contract between this module and `src/client.rs`
//!
//! Every backend (and, for [`decode_startup_message`]/[`decode_ssl_request_code`], pre-startup
//! frontend) message on the wire is `Byte1 type` (StartupMessage/SSLRequest have none -- see
//! below), `Int32 length` (self-inclusive: the length field's own 4 bytes count toward it, the
//! type byte does not), then `length - 4` more bytes of message-specific content. This module
//! never sees the type byte or the length prefix itself as part of a `decode_*` call: the
//! caller (`src/client.rs::read_backend_message`) reads exactly 5 bytes (`read_frame_header`
//! below tells it how to interpret them), then reads exactly `length - 4` more bytes, and only
//! THEN calls the matching `decode_*` function on that exact slice. This division of labour is
//! why this module's own truncation tests are meaningful: a `decode_*` function's contract is
//! "the caller already assembled exactly the bytes the length prefix promised" -- so a `body`
//! slice shorter than what the message's own internal fields need is unambiguously a malformed
//! frame (a typed error), never a "come back with more data" case. "More data is still
//! arriving over the socket" is [`read_frame_header`]'s and the caller's own concern, not this
//! module's -- see `src/client.rs::read_backend_message`'s own doc.
//!
//! StartupMessage and SSLRequest are the two exceptions PostgreSQL's own protocol carries no
//! type byte for (they precede the point at which the connection has negotiated that it even
//! IS protocol version 3): [`decode_startup_message`] and [`decode_ssl_request_code`] both take
//! the bytes strictly after the 4-byte length field (so `length - 4` bytes, exactly as above,
//! just with a 4-byte header instead of 5).

use crate::error::CatalogError;

// ---------------------------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------------------------

/// `StartupMessage`'s protocol version field: major 3, minor 0, packed as `(3 << 16) | 0`.
pub const PROTOCOL_VERSION_3_0: i32 = 196_608;

/// `SSLRequest`'s magic code (`(1234 << 16) | 5679`, PostgreSQL's own constant, chosen so it
/// can never collide with a real protocol version number -- major version 1234 will never
/// exist).
pub const SSL_REQUEST_CODE: i32 = 80_877_103;

/// An upper bound on any one message's total length (including its own 4-byte length field),
/// checked by [`read_frame_header`] before the caller allocates a buffer of that size. Guards
/// against a corrupt or adversarial length field turning one bad frame into an unbounded
/// allocation; 64 MiB is far larger than any message this crate's own callers ever send or
/// expect (a migration script, or one page of catalog rows) and far below what would meaningfully
/// threaten a host already under memory pressure (`common.md`: "swap is nearly full").
pub const MAX_MESSAGE_LEN: usize = 64 * 1024 * 1024;

const MSG_AUTHENTICATION: u8 = b'R';
const MSG_BACKEND_KEY_DATA: u8 = b'K';
const MSG_BIND: u8 = b'B';
const MSG_BIND_COMPLETE: u8 = b'2';
const MSG_COMMAND_COMPLETE: u8 = b'C';
const MSG_DATA_ROW: u8 = b'D';
const MSG_DESCRIBE: u8 = b'D';
const MSG_EMPTY_QUERY_RESPONSE: u8 = b'I';
const MSG_ERROR_RESPONSE: u8 = b'E';
const MSG_EXECUTE: u8 = b'E';
const MSG_NO_DATA: u8 = b'n';
const MSG_NOTICE_RESPONSE: u8 = b'N';
const MSG_PARAMETER_DESCRIPTION: u8 = b't';
const MSG_PARAMETER_STATUS: u8 = b'S';
const MSG_PARSE: u8 = b'P';
const MSG_PARSE_COMPLETE: u8 = b'1';
const MSG_PASSWORD_OR_SASL: u8 = b'p';
const MSG_PORTAL_SUSPENDED: u8 = b's';
const MSG_QUERY: u8 = b'Q';
const MSG_READY_FOR_QUERY: u8 = b'Z';
const MSG_ROW_DESCRIPTION: u8 = b'T';
const MSG_SYNC: u8 = b'S';
const MSG_TERMINATE: u8 = b'X';

// ---------------------------------------------------------------------------------------------
// Byte-buffer primitives shared by every encode_*/decode_* function
// ---------------------------------------------------------------------------------------------

/// Wraps `body` in the standard `Byte1 type, Int32 length` frame. `length` is `body.len() + 4`
/// (self-inclusive, per the module doc's framing contract) -- computed here, once, so no
/// `encode_*` function below repeats the "+4" arithmetic (and risks getting it wrong) itself.
fn frame(msg_type: u8, body: Vec<u8>) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + body.len());
    out.push(msg_type);
    out.extend_from_slice(&(body.len() as i32 + 4).to_be_bytes());
    out.extend_from_slice(&body);
    out
}

/// Appends `s` as a PostgreSQL `String` (UTF-8 bytes, NUL-terminated).
fn write_cstr(buf: &mut Vec<u8>, s: &str) {
    buf.extend_from_slice(s.as_bytes());
    buf.push(0);
}

/// A cursor over a `decode_*` function's `body` slice, so every field read (`Int16`/`Int32`/a
/// fixed byte run/a NUL-terminated `String`) checks its own bounds in exactly one place rather
/// than being hand-checked at each of this module's ~20 decode sites. Every method returns a
/// typed [`CatalogError`] instead of panicking or indexing out of bounds -- this is the piece
/// that makes this module's truncation tests (feed a valid message's body, truncated to every
/// length from 0 to its full size) pass for every message type at once, rather than needing
/// each decode function to reimplement its own bounds checking correctly.
struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
    /// Named for error messages only (e.g. `"RowDescription.field_name"`); never affects
    /// parsing.
    context: &'static str,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8], context: &'static str) -> Self {
        Self { buf, pos: 0, context }
    }

    fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    fn truncated(&self, need: usize) -> CatalogError {
        CatalogError::Truncated { context: self.context, need, have: self.remaining() }
    }

    fn read_u8(&mut self) -> Result<u8, CatalogError> {
        if self.remaining() < 1 {
            return Err(self.truncated(1));
        }
        let b = self.buf[self.pos];
        self.pos += 1;
        Ok(b)
    }

    fn read_i16(&mut self) -> Result<i16, CatalogError> {
        if self.remaining() < 2 {
            return Err(self.truncated(2));
        }
        let v = i16::from_be_bytes([self.buf[self.pos], self.buf[self.pos + 1]]);
        self.pos += 2;
        Ok(v)
    }

    fn read_i32(&mut self) -> Result<i32, CatalogError> {
        if self.remaining() < 4 {
            return Err(self.truncated(4));
        }
        let v = i32::from_be_bytes([self.buf[self.pos], self.buf[self.pos + 1], self.buf[self.pos + 2], self.buf[self.pos + 3]]);
        self.pos += 4;
        Ok(v)
    }

    fn read_bytes(&mut self, n: usize) -> Result<&'a [u8], CatalogError> {
        if self.remaining() < n {
            return Err(self.truncated(n));
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    /// Reads bytes up to and including the next NUL, returning the bytes before it as `&str`.
    /// A NUL that is never found before the slice ends is [`CatalogError::InvalidCString`]
    /// (a truncated frame, not a missing terminator this function should tolerate); non-UTF-8
    /// bytes before the NUL are [`CatalogError::InvalidUtf8`].
    fn read_cstr(&mut self) -> Result<&'a str, CatalogError> {
        let rel_nul = self.buf[self.pos..].iter().position(|&b| b == 0).ok_or(CatalogError::InvalidCString { context: self.context })?;
        let s = &self.buf[self.pos..self.pos + rel_nul];
        self.pos += rel_nul + 1;
        std::str::from_utf8(s).map_err(|_| CatalogError::InvalidUtf8 { context: self.context })
    }

    /// Asserts every byte of `buf` was consumed. A message whose declared length promised more
    /// bytes than its own field layout needed is exactly as malformed as one that promised too
    /// few -- this is the check that catches the former (a length prefix that lies upward).
    fn finish(self) -> Result<(), CatalogError> {
        if self.remaining() != 0 {
            return Err(CatalogError::TrailingBytes { context: self.context, extra: self.remaining() });
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// Frame header (the 5-byte -- or, for StartupMessage/SSLRequest, 4-byte -- prefix every
// message on the wire starts with). Pure, and deliberately separate from the caller's own I/O
// loop: `src/client.rs::read_backend_message` reads exactly 5 bytes, calls this to learn how
// many more bytes to read, then reads exactly that many before calling a `decode_*` function.
// ---------------------------------------------------------------------------------------------

/// A parsed frame header: the message type byte and the number of content bytes that follow
/// (already `length - 4`, i.e. exactly what a caller must `read_exact` next -- never the raw
/// `length` field, so no call site repeats the "+/- 4" arithmetic).
#[derive(Debug)]
pub struct FrameHeader {
    pub msg_type: u8,
    pub body_len: usize,
}

/// Parses a 5-byte `Byte1 type, Int32 length` header. `length` must be at least 4 (it includes
/// itself) and at most [`MAX_MESSAGE_LEN`]; either bound violated is a typed
/// [`CatalogError::InvalidFrameLength`], never a panic or a silent huge allocation downstream.
pub fn read_frame_header(header: &[u8; 5]) -> Result<FrameHeader, CatalogError> {
    let msg_type = header[0];
    let length = i32::from_be_bytes([header[1], header[2], header[3], header[4]]);
    if length < 4 {
        return Err(CatalogError::InvalidFrameLength { context: "message header", length });
    }
    let body_len = (length - 4) as usize;
    if body_len > MAX_MESSAGE_LEN {
        return Err(CatalogError::InvalidFrameLength { context: "message header", length });
    }
    Ok(FrameHeader { msg_type, body_len })
}

/// The 4-byte-header equivalent of [`read_frame_header`] for StartupMessage/SSLRequest, the
/// two pre-startup messages with no type byte. Same length bound and same self-inclusive
/// convention (the 4-byte length field counts toward `length`).
pub fn read_untyped_frame_header(header: &[u8; 4]) -> Result<usize, CatalogError> {
    let length = i32::from_be_bytes(*header);
    if length < 4 {
        return Err(CatalogError::InvalidFrameLength { context: "startup/SSLRequest header", length });
    }
    let body_len = (length - 4) as usize;
    if body_len > MAX_MESSAGE_LEN {
        return Err(CatalogError::InvalidFrameLength { context: "startup/SSLRequest header", length });
    }
    Ok(body_len)
}

// ---------------------------------------------------------------------------------------------
// StartupMessage / SSLRequest (no type byte)
// ---------------------------------------------------------------------------------------------

/// The four `StartupMessage` parameters this crate always sends, in the order they are written
/// (order is not protocol-significant, but a fixed order keeps encoding deterministic and
/// testable). `client_encoding` is always `UTF8`: every string this crate ever reads or writes
/// is required to be valid UTF-8 already (`Reader::read_cstr`, `Row`'s text-format accessors),
/// so there is exactly one encoding this client can correctly speak, and it is not
/// configuration.
pub struct StartupParams<'a> {
    pub user: &'a str,
    pub database: &'a str,
    pub application_name: &'a str,
}

/// Encodes a full `StartupMessage`: `Int32 length`, `Int32 protocol version`, then
/// `user`/`database`/`application_name`/`client_encoding` as NUL-terminated `name, value`
/// pairs, terminated by one final NUL byte.
pub fn encode_startup_message(params: &StartupParams<'_>) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&PROTOCOL_VERSION_3_0.to_be_bytes());
    for (name, value) in [("user", params.user), ("database", params.database), ("application_name", params.application_name), ("client_encoding", "UTF8")] {
        write_cstr(&mut body, name);
        write_cstr(&mut body, value);
    }
    body.push(0); // terminator
    let mut out = Vec::with_capacity(4 + body.len());
    out.extend_from_slice(&(body.len() as i32 + 4).to_be_bytes());
    out.extend_from_slice(&body);
    out
}

/// A decoded `StartupMessage`: the protocol version plus every `name`/`value` pair, in the
/// order they appeared on the wire (used only by `tests/wire_protocol.rs`'s fake server, which
/// must parse the real [`crate::client::PgClient`]'s own `encode_startup_message` output).
pub struct DecodedStartupMessage {
    pub protocol_version: i32,
    pub params: Vec<(String, String)>,
}

/// Decodes a `StartupMessage` body (the bytes strictly after the 4-byte length field -- see
/// this module's own doc). A version field on its own is never truncatable to fewer than 4
/// bytes without [`Reader::read_i32`] catching it; an odd number of NUL-terminated strings
/// before the final terminator NUL is [`CatalogError::InvalidCString`] (the final `read_cstr`
/// for the missing value has nothing left to read).
pub fn decode_startup_message(body: &[u8]) -> Result<DecodedStartupMessage, CatalogError> {
    let mut r = Reader::new(body, "StartupMessage");
    let protocol_version = r.read_i32()?;
    let mut params = Vec::new();
    loop {
        // A single NUL byte where a parameter name would start is the list terminator.
        if r.remaining() == 0 {
            return Err(r.truncated(1));
        }
        if body[r.pos] == 0 {
            r.pos += 1;
            break;
        }
        let name = r.read_cstr()?.to_string();
        let value = r.read_cstr()?.to_string();
        params.push((name, value));
    }
    r.finish()?;
    Ok(DecodedStartupMessage { protocol_version, params })
}

/// Encodes an `SSLRequest`: exactly 8 bytes, `Int32(8)` length then `Int32(`[`SSL_REQUEST_CODE`]`)`.
pub fn encode_ssl_request() -> [u8; 8] {
    let mut out = [0u8; 8];
    out[0..4].copy_from_slice(&8i32.to_be_bytes());
    out[4..8].copy_from_slice(&SSL_REQUEST_CODE.to_be_bytes());
    out
}

/// Decodes (and validates) an `SSLRequest` body -- the 4 bytes strictly after its length field,
/// which must be exactly [`SSL_REQUEST_CODE`]. Used only by `tests/wire_protocol.rs`'s fake
/// server.
pub fn decode_ssl_request_code(body: &[u8]) -> Result<(), CatalogError> {
    let mut r = Reader::new(body, "SSLRequest");
    let code = r.read_i32()?;
    r.finish()?;
    if code != SSL_REQUEST_CODE {
        return Err(CatalogError::InvalidSslRequestCode { code });
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Frontend messages (Byte1 type, Int32 length, body)
// ---------------------------------------------------------------------------------------------

/// `SASLInitialResponse` (`Byte1('p')`): `String mechanism`, `Int32` length of the initial
/// response data (`-1` for none), then the data itself. This is the ONE frontend message this
/// crate's own [`crate::scram`] flow sends first; [`encode_sasl_response`] below is the second,
/// data-only `'p'` message SCRAM's second round trip sends -- PostgreSQL distinguishes the two
/// purely by protocol STATE (which `'p'` message is expected next), never by a different type
/// byte, which is why this module offers two distinct encode/decode function pairs for the one
/// wire byte `'p'` rather than a single ambiguous one.
pub fn encode_sasl_initial_response(mechanism: &str, data: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    write_cstr(&mut body, mechanism);
    body.extend_from_slice(&(data.len() as i32).to_be_bytes());
    body.extend_from_slice(data);
    frame(MSG_PASSWORD_OR_SASL, body)
}

/// Decodes a `SASLInitialResponse` body. Used only by `tests/wire_protocol.rs`'s fake server,
/// which -- like a real PostgreSQL backend -- must already know from its own state (it just
/// sent `AuthenticationSASL`) that the next `'p'` message is this shape, not
/// [`decode_sasl_response`]'s.
pub fn decode_sasl_initial_response(body: &[u8]) -> Result<(String, Vec<u8>), CatalogError> {
    let mut r = Reader::new(body, "SASLInitialResponse");
    let mechanism = r.read_cstr()?.to_string();
    let data_len = r.read_i32()?;
    let data = if data_len < 0 { Vec::new() } else { r.read_bytes(data_len as usize)?.to_vec() };
    r.finish()?;
    Ok((mechanism, data))
}

/// `SASLResponse` (`Byte1('p')`): the whole body is SASL mechanism-specific data, with no
/// sub-length prefix of its own (unlike [`encode_sasl_initial_response`]'s explicit `Int32`) --
/// the outer message length is the only length this message carries.
pub fn encode_sasl_response(data: &[u8]) -> Vec<u8> {
    frame(MSG_PASSWORD_OR_SASL, data.to_vec())
}

/// Decodes a `SASLResponse` body: the entire slice, verbatim. Used only by
/// `tests/wire_protocol.rs`'s fake server (see [`decode_sasl_initial_response`]'s doc on why
/// this is a separate function rather than a shared `'p'` dispatch).
pub fn decode_sasl_response(body: &[u8]) -> Vec<u8> {
    body.to_vec()
}

/// The simple-query protocol's one frontend message: `Byte1('Q')`, `String query`. Carries any
/// number of `;`-separated statements -- [`crate::client::PgClient::simple_batch`]'s own doc
/// explains why this is the ONE place in this crate's public API a whole SQL string is ever
/// handed to the wire un-parameterised, and why that is safe.
pub fn encode_query(sql: &str) -> Vec<u8> {
    let mut body = Vec::new();
    write_cstr(&mut body, sql);
    frame(MSG_QUERY, body)
}

/// Decodes a `Query` body. Used only by `tests/wire_protocol.rs`'s fake server.
pub fn decode_query(body: &[u8]) -> Result<String, CatalogError> {
    let mut r = Reader::new(body, "Query");
    let sql = r.read_cstr()?.to_string();
    r.finish()?;
    Ok(sql)
}

/// `Parse` (`Byte1('P')`): `String destination statement name` (empty = the unnamed prepared
/// statement, which is all [`crate::client::PgClient`] ever uses -- it never names a statement
/// across a `Sync` boundary), `String query`, `Int16` count of parameter type OIDs, then that
/// many `Int32` OIDs. This crate always sends zero explicit OIDs: every parameter is text-typed
/// by inference from SQL context (`crate::client`'s module doc explains why text format, not
/// binary, throughout).
pub fn encode_parse(statement_name: &str, sql: &str, param_type_oids: &[i32]) -> Vec<u8> {
    let mut body = Vec::new();
    write_cstr(&mut body, statement_name);
    write_cstr(&mut body, sql);
    body.extend_from_slice(&(param_type_oids.len() as i16).to_be_bytes());
    for oid in param_type_oids {
        body.extend_from_slice(&oid.to_be_bytes());
    }
    frame(MSG_PARSE, body)
}

pub struct DecodedParse {
    pub statement_name: String,
    pub sql: String,
    pub param_type_oids: Vec<i32>,
}

/// Decodes a `Parse` body. Used only by `tests/wire_protocol.rs`'s fake server.
pub fn decode_parse(body: &[u8]) -> Result<DecodedParse, CatalogError> {
    let mut r = Reader::new(body, "Parse");
    let statement_name = r.read_cstr()?.to_string();
    let sql = r.read_cstr()?.to_string();
    let n = r.read_i16()?;
    if n < 0 {
        return Err(CatalogError::NegativeCount { context: "Parse.num_param_types", value: n as i32 });
    }
    let mut param_type_oids = Vec::with_capacity(n as usize);
    for _ in 0..n {
        param_type_oids.push(r.read_i32()?);
    }
    r.finish()?;
    Ok(DecodedParse { statement_name, sql, param_type_oids })
}

/// `Bind` (`Byte1('B')`): destination portal/source statement names (this crate always uses
/// the unnamed portal and the unnamed statement -- see [`encode_parse`]'s doc), then parameter
/// format codes, parameter values, and result-column format codes. This crate hardcodes TEXT
/// format (code `0`) for both directions throughout (`crate::client`'s module doc), so
/// `param_format_codes`/`result_format_codes` are always encoded as a single `[0]` entry
/// (PostgreSQL's own documented shorthand for "every parameter/column uses this one format"),
/// never one code per value -- exercising that shorthand, not the equivalent longer form, is a
/// deliberate simplification with nothing this crate's protocol ever needs the longer form for.
pub fn encode_bind(portal: &str, statement_name: &str, params: &[Option<&[u8]>]) -> Vec<u8> {
    let mut body = Vec::new();
    write_cstr(&mut body, portal);
    write_cstr(&mut body, statement_name);
    body.extend_from_slice(&1i16.to_be_bytes()); // one param format code follows
    body.extend_from_slice(&0i16.to_be_bytes()); // ...and it is 0 (text), applied to all params
    body.extend_from_slice(&(params.len() as i16).to_be_bytes());
    for param in params {
        match param {
            None => body.extend_from_slice(&(-1i32).to_be_bytes()),
            Some(bytes) => {
                body.extend_from_slice(&(bytes.len() as i32).to_be_bytes());
                body.extend_from_slice(bytes);
            }
        }
    }
    body.extend_from_slice(&1i16.to_be_bytes()); // one result format code follows
    body.extend_from_slice(&0i16.to_be_bytes()); // ...and it is 0 (text), applied to all columns
    frame(MSG_BIND, body)
}

pub struct DecodedBind {
    pub portal: String,
    pub statement_name: String,
    pub param_format_codes: Vec<i16>,
    pub params: Vec<Option<Vec<u8>>>,
    pub result_format_codes: Vec<i16>,
}

/// Decodes a `Bind` body. Used only by `tests/wire_protocol.rs`'s fake server; unlike
/// [`encode_bind`] this reads the general form (any number of format codes), since a future
/// caller of this decode function should not silently mis-parse a message this crate's own
/// encoder happens not to produce today.
pub fn decode_bind(body: &[u8]) -> Result<DecodedBind, CatalogError> {
    let mut r = Reader::new(body, "Bind");
    let portal = r.read_cstr()?.to_string();
    let statement_name = r.read_cstr()?.to_string();
    let n_param_formats = read_non_negative_i16(&mut r, "Bind.num_param_format_codes")?;
    let mut param_format_codes = Vec::with_capacity(n_param_formats);
    for _ in 0..n_param_formats {
        param_format_codes.push(r.read_i16()?);
    }
    let n_params = read_non_negative_i16(&mut r, "Bind.num_params")?;
    let mut params = Vec::with_capacity(n_params);
    for _ in 0..n_params {
        let len = r.read_i32()?;
        if len < 0 {
            params.push(None);
        } else {
            params.push(Some(r.read_bytes(len as usize)?.to_vec()));
        }
    }
    let n_result_formats = read_non_negative_i16(&mut r, "Bind.num_result_format_codes")?;
    let mut result_format_codes = Vec::with_capacity(n_result_formats);
    for _ in 0..n_result_formats {
        result_format_codes.push(r.read_i16()?);
    }
    r.finish()?;
    Ok(DecodedBind { portal, statement_name, param_format_codes, params, result_format_codes })
}

/// Shared by every decode function that reads an `Int16` count that must not be negative
/// (`Bind`'s three counts, `RowDescription`'s field count, `DataRow`'s column count,
/// `ParameterDescription`'s count): a negative count is never valid PostgreSQL wire data, and
/// casting it to `usize` without this check would wrap around to an enormous number instead of
/// erroring (`CatalogError::NegativeCount`) -- exactly the "never a panic ... never an
/// index-out-of-bounds" this module's own tests exist to enforce.
fn read_non_negative_i16(r: &mut Reader<'_>, context: &'static str) -> Result<usize, CatalogError> {
    let n = r.read_i16()?;
    if n < 0 {
        return Err(CatalogError::NegativeCount { context, value: n as i32 });
    }
    Ok(n as usize)
}

/// Which prepared object [`encode_describe`] targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DescribeTarget {
    Statement,
    Portal,
}

impl DescribeTarget {
    fn wire_byte(self) -> u8 {
        match self {
            DescribeTarget::Statement => b'S',
            DescribeTarget::Portal => b'P',
        }
    }
}

/// `Describe` (`Byte1('D')`): `Byte1` target (`'S'`/`'P'`), `String name`.
/// [`crate::client::PgClient`] always describes the unnamed portal, to receive the
/// `RowDescription` that names [`crate::client::Row`]'s columns before `Execute` -- `Bind`
/// alone never triggers one.
pub fn encode_describe(target: DescribeTarget, name: &str) -> Vec<u8> {
    let mut body = Vec::new();
    body.push(target.wire_byte());
    write_cstr(&mut body, name);
    frame(MSG_DESCRIBE, body)
}

/// Decodes a `Describe` body. Used only by `tests/wire_protocol.rs`'s fake server.
pub fn decode_describe(body: &[u8]) -> Result<(DescribeTarget, String), CatalogError> {
    let mut r = Reader::new(body, "Describe");
    let target = match r.read_u8()? {
        b'S' => DescribeTarget::Statement,
        b'P' => DescribeTarget::Portal,
        other => return Err(CatalogError::InvalidDescribeTarget { byte: other }),
    };
    let name = r.read_cstr()?.to_string();
    r.finish()?;
    Ok((target, name))
}

/// `Execute` (`Byte1('E')`): `String portal name`, `Int32` max rows to return (`0` = no limit --
/// [`crate::client::PgClient`] always passes `0`; it never uses `PortalSuspended`-based
/// paging).
pub fn encode_execute(portal: &str, max_rows: i32) -> Vec<u8> {
    let mut body = Vec::new();
    write_cstr(&mut body, portal);
    body.extend_from_slice(&max_rows.to_be_bytes());
    frame(MSG_EXECUTE, body)
}

/// Decodes an `Execute` body. Used only by `tests/wire_protocol.rs`'s fake server.
pub fn decode_execute(body: &[u8]) -> Result<(String, i32), CatalogError> {
    let mut r = Reader::new(body, "Execute");
    let portal = r.read_cstr()?.to_string();
    let max_rows = r.read_i32()?;
    r.finish()?;
    Ok((portal, max_rows))
}

/// `Sync` (`Byte1('S')`): no body. Ends one extended-query round trip and asks the backend for
/// the `ReadyForQuery` that always follows.
pub fn encode_sync() -> Vec<u8> {
    frame(MSG_SYNC, Vec::new())
}

/// Decodes (validates the emptiness of) a `Sync` body. Used only by `tests/wire_protocol.rs`'s
/// fake server.
pub fn decode_sync(body: &[u8]) -> Result<(), CatalogError> {
    Reader::new(body, "Sync").finish()
}

/// `Terminate` (`Byte1('X')`): no body. [`crate::client::PgClient::close`] sends this, then
/// simply drops the socket -- PostgreSQL sends no reply.
pub fn encode_terminate() -> Vec<u8> {
    frame(MSG_TERMINATE, Vec::new())
}

/// Decodes (validates the emptiness of) a `Terminate` body. Used only by
/// `tests/wire_protocol.rs`'s fake server.
pub fn decode_terminate(body: &[u8]) -> Result<(), CatalogError> {
    Reader::new(body, "Terminate").finish()
}

// ---------------------------------------------------------------------------------------------
// Backend messages
// ---------------------------------------------------------------------------------------------

/// One decoded backend message. `src/client.rs` matches on this; `tests/wire_protocol.rs`'s
/// fake server builds these (via the paired `encode_*` functions below) to script a server.
#[derive(Debug, Clone, PartialEq)]
pub enum BackendMessage {
    AuthenticationOk,
    AuthenticationCleartextPassword,
    AuthenticationMd5Password { salt: [u8; 4] },
    AuthenticationSasl { mechanisms: Vec<String> },
    AuthenticationSaslContinue { data: Vec<u8> },
    AuthenticationSaslFinal { data: Vec<u8> },
    /// Every other `Authentication*` sub-message this crate recognises by number but does not
    /// implement (Kerberos, SCM credential, GSSAPI, SSPI -- none of them SHA-256/OpenSSL, all
    /// of them absent from `services/catalog/IMAGE_DIGEST.md`'s measured `pg_hba.conf`, which
    /// ends `scram-sha-256`). Carries the raw sub-code so [`crate::error::CatalogError`]'s
    /// `Display` names exactly which one a server unexpectedly asked for.
    AuthenticationUnsupported { code: i32 },
    BackendKeyData { process_id: i32, secret_key: i32 },
    ParameterStatus { name: String, value: String },
    ReadyForQuery { status: TransactionStatus },
    RowDescription { fields: Vec<FieldDescription> },
    DataRow { values: Vec<Option<Vec<u8>>> },
    CommandComplete { tag: String },
    EmptyQueryResponse,
    NoData,
    ParseComplete,
    BindComplete,
    ParameterDescription { param_type_oids: Vec<i32> },
    PortalSuspended,
    ErrorResponse(ServerError),
    NoticeResponse(ServerError),
}

/// `ReadyForQuery`'s one field: which state the just-finished transaction left the session in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionStatus {
    Idle,
    InTransaction,
    Failed,
}

/// One `RowDescription` column, in the order PostgreSQL reports it (matches [`DataRow`]'s
/// value order by position, so `src/client.rs` zips the two rather than needing a name-based
/// join). Every field is kept even though [`crate::client::Row`] only reads `name` back out
/// today -- decoding the whole message correctly (never silently truncating a field this
/// crate's callers do not yet consume) is what [`Reader::finish`] enforces regardless.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldDescription {
    pub name: String,
    pub table_oid: i32,
    pub column_id: i16,
    pub type_oid: i32,
    pub type_size: i16,
    pub type_modifier: i32,
    pub format_code: i16,
}

/// `ErrorResponse`/`NoticeResponse` decoded into named fields (this task's brief: "SQLSTATE is
/// a first-class field, not a substring of a message"). Both messages share this exact
/// `Byte1 code, String value` repeated-field wire shape (PostgreSQL's own protocol
/// documentation: "NoticeResponse... identical to ErrorResponse"), so one struct and one
/// decode function serve both -- [`BackendMessage::ErrorResponse`] and
/// [`BackendMessage::NoticeResponse`] simply wrap it differently.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ServerError {
    pub severity: String,
    pub sqlstate: String,
    pub message: String,
    pub detail: Option<String>,
    pub hint: Option<String>,
    pub position: Option<String>,
    pub where_: Option<String>,
    pub schema: Option<String>,
    pub table: Option<String>,
    pub column: Option<String>,
    pub constraint: Option<String>,
    pub file: Option<String>,
    pub line: Option<String>,
    pub routine: Option<String>,
}

/// Decodes the shared `ErrorResponse`/`NoticeResponse` body: repeated `(Byte1 code, String
/// value)` pairs, terminated by one zero byte with no value following it. Any field code not
/// named in [`ServerError`]'s own field list (`'p'`/internal position, `'q'`/internal query,
/// `'d'`/datatype name, and any code a newer server version adds) is read -- so parsing never
/// desyncs -- and its value discarded, per the protocol's own documented rule ("frontends
/// should silently ignore fields of unrecognized type"). `'V'` (non-localized severity,
/// PostgreSQL 9.6+) is preferred over `'S'` (localized) when both are present, since a
/// non-localized value is the more stable one for [`crate::error::CatalogError`]'s `Display`
/// text to embed; `'S'` alone (an older server) is still accepted.
fn decode_server_error(body: &[u8], context: &'static str) -> Result<ServerError, CatalogError> {
    let mut r = Reader::new(body, context);
    let mut localized_severity: Option<String> = None;
    let mut nonlocalized_severity: Option<String> = None;
    let mut sqlstate: Option<String> = None;
    let mut message: Option<String> = None;
    let mut out = ServerError::default();
    loop {
        let code = r.read_u8()?;
        if code == 0 {
            break;
        }
        let value = r.read_cstr()?.to_string();
        match code {
            b'S' => localized_severity = Some(value),
            b'V' => nonlocalized_severity = Some(value),
            b'C' => sqlstate = Some(value),
            b'M' => message = Some(value),
            b'D' => out.detail = Some(value),
            b'H' => out.hint = Some(value),
            b'P' => out.position = Some(value),
            b'W' => out.where_ = Some(value),
            b's' => out.schema = Some(value),
            b't' => out.table = Some(value),
            b'c' => out.column = Some(value),
            b'n' => out.constraint = Some(value),
            b'F' => out.file = Some(value),
            b'L' => out.line = Some(value),
            b'R' => out.routine = Some(value),
            _ => {} // unrecognized field code: value already consumed above, silently ignored
        }
    }
    r.finish()?;
    out.severity = nonlocalized_severity.or(localized_severity).ok_or(CatalogError::MissingErrorField { context, code: 'S' })?;
    out.sqlstate = sqlstate.ok_or(CatalogError::MissingErrorField { context, code: 'C' })?;
    out.message = message.ok_or(CatalogError::MissingErrorField { context, code: 'M' })?;
    Ok(out)
}

/// Encodes an `ErrorResponse`/`NoticeResponse` body from a [`ServerError`] -- used only by
/// `tests/wire_protocol.rs`'s fake server, to script a real server's error. Round-trips through
/// [`decode_server_error`] using `'V'` for severity (never `'S'`): real servers always send
/// `'V'` too, and encoding both would make the round trip untestable (decode prefers `'V'`, so
/// a test could not distinguish "the encoder sent the wrong one" from "the decoder preferred
/// the wrong one").
fn encode_server_error(err: &ServerError) -> Vec<u8> {
    let mut body = Vec::new();
    let mut field = |code: u8, value: &str| {
        body.push(code);
        write_cstr(&mut body, value);
    };
    field(b'V', &err.severity);
    field(b'C', &err.sqlstate);
    field(b'M', &err.message);
    if let Some(v) = &err.detail {
        field(b'D', v);
    }
    if let Some(v) = &err.hint {
        field(b'H', v);
    }
    if let Some(v) = &err.position {
        field(b'P', v);
    }
    if let Some(v) = &err.where_ {
        field(b'W', v);
    }
    if let Some(v) = &err.schema {
        field(b's', v);
    }
    if let Some(v) = &err.table {
        field(b't', v);
    }
    if let Some(v) = &err.column {
        field(b'c', v);
    }
    if let Some(v) = &err.constraint {
        field(b'n', v);
    }
    if let Some(v) = &err.file {
        field(b'F', v);
    }
    if let Some(v) = &err.line {
        field(b'L', v);
    }
    if let Some(v) = &err.routine {
        field(b'R', v);
    }
    body.push(0);
    body
}

/// Encodes `BackendMessage::ErrorResponse(err)`. Used only by `tests/wire_protocol.rs`'s fake
/// server.
pub fn encode_error_response(err: &ServerError) -> Vec<u8> {
    frame(MSG_ERROR_RESPONSE, encode_server_error(err))
}

/// Encodes `BackendMessage::NoticeResponse(err)`. Used only by `tests/wire_protocol.rs`'s fake
/// server.
pub fn encode_notice_response(err: &ServerError) -> Vec<u8> {
    frame(MSG_NOTICE_RESPONSE, encode_server_error(err))
}

/// Encodes `BackendMessage::AuthenticationOk`. Used only by `tests/wire_protocol.rs`'s fake
/// server.
pub fn encode_authentication_ok() -> Vec<u8> {
    frame(MSG_AUTHENTICATION, 0i32.to_be_bytes().to_vec())
}

/// Encodes `BackendMessage::AuthenticationCleartextPassword`. Used only by
/// `tests/wire_protocol.rs`'s fake server, to prove [`crate::client::PgClient::connect`]
/// refuses cleartext auth by construction (mirrors [`encode_authentication_md5_password`]'s own
/// doc for the MD5 case).
pub fn encode_authentication_cleartext_password() -> Vec<u8> {
    frame(MSG_AUTHENTICATION, 3i32.to_be_bytes().to_vec())
}

/// Encodes `BackendMessage::AuthenticationMd5Password`. Used only by `tests/wire_protocol.rs`'s
/// fake server, to prove [`crate::client::PgClient::connect`] refuses MD5 auth by construction
/// (this task's brief: "recognised and typed-refused, never implemented").
pub fn encode_authentication_md5_password(salt: [u8; 4]) -> Vec<u8> {
    let mut body = 5i32.to_be_bytes().to_vec();
    body.extend_from_slice(&salt);
    frame(MSG_AUTHENTICATION, body)
}

/// Encodes `BackendMessage::AuthenticationSasl` with the given mechanism list (a real server
/// offering SCRAM-SHA-256 sends exactly `["SCRAM-SHA-256"]`). Used only by
/// `tests/wire_protocol.rs`'s fake server.
pub fn encode_authentication_sasl(mechanisms: &[&str]) -> Vec<u8> {
    let mut body = 10i32.to_be_bytes().to_vec();
    for m in mechanisms {
        write_cstr(&mut body, m);
    }
    body.push(0);
    frame(MSG_AUTHENTICATION, body)
}

/// Encodes `BackendMessage::AuthenticationSaslContinue`. Used only by
/// `tests/wire_protocol.rs`'s fake server.
pub fn encode_authentication_sasl_continue(data: &[u8]) -> Vec<u8> {
    let mut body = 11i32.to_be_bytes().to_vec();
    body.extend_from_slice(data);
    frame(MSG_AUTHENTICATION, body)
}

/// Encodes `BackendMessage::AuthenticationSaslFinal`. Used only by `tests/wire_protocol.rs`'s
/// fake server.
pub fn encode_authentication_sasl_final(data: &[u8]) -> Vec<u8> {
    let mut body = 12i32.to_be_bytes().to_vec();
    body.extend_from_slice(data);
    frame(MSG_AUTHENTICATION, body)
}

/// Encodes `BackendMessage::BackendKeyData`. Used only by `tests/wire_protocol.rs`'s fake
/// server.
pub fn encode_backend_key_data(process_id: i32, secret_key: i32) -> Vec<u8> {
    let mut body = Vec::with_capacity(8);
    body.extend_from_slice(&process_id.to_be_bytes());
    body.extend_from_slice(&secret_key.to_be_bytes());
    frame(MSG_BACKEND_KEY_DATA, body)
}

/// Encodes `BackendMessage::ParameterStatus`. Used only by `tests/wire_protocol.rs`'s fake
/// server.
pub fn encode_parameter_status(name: &str, value: &str) -> Vec<u8> {
    let mut body = Vec::new();
    write_cstr(&mut body, name);
    write_cstr(&mut body, value);
    frame(MSG_PARAMETER_STATUS, body)
}

/// Encodes `BackendMessage::ReadyForQuery`. Used only by `tests/wire_protocol.rs`'s fake
/// server.
pub fn encode_ready_for_query(status: TransactionStatus) -> Vec<u8> {
    let byte = match status {
        TransactionStatus::Idle => b'I',
        TransactionStatus::InTransaction => b'T',
        TransactionStatus::Failed => b'E',
    };
    frame(MSG_READY_FOR_QUERY, vec![byte])
}

/// Encodes `BackendMessage::RowDescription`. Used only by `tests/wire_protocol.rs`'s fake
/// server.
pub fn encode_row_description(fields: &[FieldDescription]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&(fields.len() as i16).to_be_bytes());
    for f in fields {
        write_cstr(&mut body, &f.name);
        body.extend_from_slice(&f.table_oid.to_be_bytes());
        body.extend_from_slice(&f.column_id.to_be_bytes());
        body.extend_from_slice(&f.type_oid.to_be_bytes());
        body.extend_from_slice(&f.type_size.to_be_bytes());
        body.extend_from_slice(&f.type_modifier.to_be_bytes());
        body.extend_from_slice(&f.format_code.to_be_bytes());
    }
    frame(MSG_ROW_DESCRIPTION, body)
}

/// Encodes `BackendMessage::DataRow`. Used only by `tests/wire_protocol.rs`'s fake server.
pub fn encode_data_row(values: &[Option<&[u8]>]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&(values.len() as i16).to_be_bytes());
    for v in values {
        match v {
            None => body.extend_from_slice(&(-1i32).to_be_bytes()),
            Some(bytes) => {
                body.extend_from_slice(&(bytes.len() as i32).to_be_bytes());
                body.extend_from_slice(bytes);
            }
        }
    }
    frame(MSG_DATA_ROW, body)
}

/// Encodes `BackendMessage::CommandComplete`. Used only by `tests/wire_protocol.rs`'s fake
/// server.
pub fn encode_command_complete(tag: &str) -> Vec<u8> {
    let mut body = Vec::new();
    write_cstr(&mut body, tag);
    frame(MSG_COMMAND_COMPLETE, body)
}

/// Encodes `BackendMessage::EmptyQueryResponse`. Used only by `tests/wire_protocol.rs`'s fake
/// server.
pub fn encode_empty_query_response() -> Vec<u8> {
    frame(MSG_EMPTY_QUERY_RESPONSE, Vec::new())
}

/// Encodes `BackendMessage::NoData`. Used only by `tests/wire_protocol.rs`'s fake server.
pub fn encode_no_data() -> Vec<u8> {
    frame(MSG_NO_DATA, Vec::new())
}

/// Encodes `BackendMessage::ParseComplete`. Used only by `tests/wire_protocol.rs`'s fake
/// server.
pub fn encode_parse_complete() -> Vec<u8> {
    frame(MSG_PARSE_COMPLETE, Vec::new())
}

/// Encodes `BackendMessage::BindComplete`. Used only by `tests/wire_protocol.rs`'s fake server.
pub fn encode_bind_complete() -> Vec<u8> {
    frame(MSG_BIND_COMPLETE, Vec::new())
}

/// Encodes `BackendMessage::ParameterDescription`. Used only by `tests/wire_protocol.rs`'s fake
/// server.
pub fn encode_parameter_description(param_type_oids: &[i32]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&(param_type_oids.len() as i16).to_be_bytes());
    for oid in param_type_oids {
        body.extend_from_slice(&oid.to_be_bytes());
    }
    frame(MSG_PARAMETER_DESCRIPTION, body)
}

/// Encodes `BackendMessage::PortalSuspended`. Used only by `tests/wire_protocol.rs`'s fake
/// server.
pub fn encode_portal_suspended() -> Vec<u8> {
    frame(MSG_PORTAL_SUSPENDED, Vec::new())
}

/// Decodes one backend message, given its type byte and already-fully-collected body (see this
/// module's own doc for the framing contract that makes `body`'s length trustworthy). The one
/// dispatcher `src/client.rs::read_backend_message` calls, and the one function this module's
/// own truncation tests exercise for every backend message type.
pub fn decode_backend_message(msg_type: u8, body: &[u8]) -> Result<BackendMessage, CatalogError> {
    match msg_type {
        MSG_AUTHENTICATION => decode_authentication(body),
        MSG_BACKEND_KEY_DATA => {
            let mut r = Reader::new(body, "BackendKeyData");
            let process_id = r.read_i32()?;
            let secret_key = r.read_i32()?;
            r.finish()?;
            Ok(BackendMessage::BackendKeyData { process_id, secret_key })
        }
        MSG_PARAMETER_STATUS => {
            let mut r = Reader::new(body, "ParameterStatus");
            let name = r.read_cstr()?.to_string();
            let value = r.read_cstr()?.to_string();
            r.finish()?;
            Ok(BackendMessage::ParameterStatus { name, value })
        }
        MSG_READY_FOR_QUERY => {
            let mut r = Reader::new(body, "ReadyForQuery");
            let status = match r.read_u8()? {
                b'I' => TransactionStatus::Idle,
                b'T' => TransactionStatus::InTransaction,
                b'E' => TransactionStatus::Failed,
                other => return Err(CatalogError::InvalidTransactionStatus { byte: other }),
            };
            r.finish()?;
            Ok(BackendMessage::ReadyForQuery { status })
        }
        MSG_ROW_DESCRIPTION => decode_row_description(body),
        MSG_DATA_ROW => decode_data_row(body),
        MSG_COMMAND_COMPLETE => {
            let mut r = Reader::new(body, "CommandComplete");
            let tag = r.read_cstr()?.to_string();
            r.finish()?;
            Ok(BackendMessage::CommandComplete { tag })
        }
        MSG_EMPTY_QUERY_RESPONSE => {
            Reader::new(body, "EmptyQueryResponse").finish()?;
            Ok(BackendMessage::EmptyQueryResponse)
        }
        MSG_NO_DATA => {
            Reader::new(body, "NoData").finish()?;
            Ok(BackendMessage::NoData)
        }
        MSG_PARSE_COMPLETE => {
            Reader::new(body, "ParseComplete").finish()?;
            Ok(BackendMessage::ParseComplete)
        }
        MSG_BIND_COMPLETE => {
            Reader::new(body, "BindComplete").finish()?;
            Ok(BackendMessage::BindComplete)
        }
        MSG_PARAMETER_DESCRIPTION => {
            let mut r = Reader::new(body, "ParameterDescription");
            let n = read_non_negative_i16(&mut r, "ParameterDescription.count")?;
            let mut param_type_oids = Vec::with_capacity(n);
            for _ in 0..n {
                param_type_oids.push(r.read_i32()?);
            }
            r.finish()?;
            Ok(BackendMessage::ParameterDescription { param_type_oids })
        }
        MSG_PORTAL_SUSPENDED => {
            Reader::new(body, "PortalSuspended").finish()?;
            Ok(BackendMessage::PortalSuspended)
        }
        MSG_ERROR_RESPONSE => Ok(BackendMessage::ErrorResponse(decode_server_error(body, "ErrorResponse")?)),
        MSG_NOTICE_RESPONSE => Ok(BackendMessage::NoticeResponse(decode_server_error(body, "NoticeResponse")?)),
        other => Err(CatalogError::UnknownMessageType { type_byte: other, direction: "backend" }),
    }
}

fn decode_authentication(body: &[u8]) -> Result<BackendMessage, CatalogError> {
    let mut r = Reader::new(body, "Authentication");
    let code = r.read_i32()?;
    match code {
        0 => {
            r.finish()?;
            Ok(BackendMessage::AuthenticationOk)
        }
        3 => {
            r.finish()?;
            Ok(BackendMessage::AuthenticationCleartextPassword)
        }
        5 => {
            let salt = r.read_bytes(4)?;
            r.finish()?;
            Ok(BackendMessage::AuthenticationMd5Password { salt: [salt[0], salt[1], salt[2], salt[3]] })
        }
        10 => {
            let mut mechanisms = Vec::new();
            loop {
                if r.remaining() == 0 {
                    return Err(r.truncated(1));
                }
                if body[r.pos] == 0 {
                    r.pos += 1;
                    break;
                }
                mechanisms.push(r.read_cstr()?.to_string());
            }
            r.finish()?;
            Ok(BackendMessage::AuthenticationSasl { mechanisms })
        }
        11 => {
            let data = r.read_bytes(r.remaining())?.to_vec();
            r.finish()?;
            Ok(BackendMessage::AuthenticationSaslContinue { data })
        }
        12 => {
            let data = r.read_bytes(r.remaining())?.to_vec();
            r.finish()?;
            Ok(BackendMessage::AuthenticationSaslFinal { data })
        }
        // 2 (KerberosV5), 6 (SCMCredential), 7 (GSS), 8 (GSSContinue), 9 (SSPI): named, typed,
        // never implemented -- see BackendMessage::AuthenticationUnsupported's own doc.
        other => Ok(BackendMessage::AuthenticationUnsupported { code: other }),
    }
}

fn decode_row_description(body: &[u8]) -> Result<BackendMessage, CatalogError> {
    let mut r = Reader::new(body, "RowDescription");
    let n = read_non_negative_i16(&mut r, "RowDescription.field_count")?;
    let mut fields = Vec::with_capacity(n);
    for _ in 0..n {
        let name = r.read_cstr()?.to_string();
        let table_oid = r.read_i32()?;
        let column_id = r.read_i16()?;
        let type_oid = r.read_i32()?;
        let type_size = r.read_i16()?;
        let type_modifier = r.read_i32()?;
        let format_code = r.read_i16()?;
        fields.push(FieldDescription { name, table_oid, column_id, type_oid, type_size, type_modifier, format_code });
    }
    r.finish()?;
    Ok(BackendMessage::RowDescription { fields })
}

fn decode_data_row(body: &[u8]) -> Result<BackendMessage, CatalogError> {
    let mut r = Reader::new(body, "DataRow");
    let n = read_non_negative_i16(&mut r, "DataRow.column_count")?;
    let mut values = Vec::with_capacity(n);
    for _ in 0..n {
        let len = r.read_i32()?;
        if len < 0 {
            values.push(None);
        } else {
            values.push(Some(r.read_bytes(len as usize)?.to_vec()));
        }
    }
    r.finish()?;
    Ok(BackendMessage::DataRow { values })
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- StartupMessage / SSLRequest ----------------------------------------------------

    #[test]
    fn startup_message_round_trips() {
        let params = StartupParams { user: "app_user", database: "catalog", application_name: "av-catalog-test" };
        let encoded = encode_startup_message(&params);
        // Strip the 4-byte length prefix, as `src/client.rs`'s I/O layer would.
        let body_len = read_untyped_frame_header(&encoded[0..4].try_into().unwrap()).unwrap();
        assert_eq!(body_len, encoded.len() - 4);
        let decoded = decode_startup_message(&encoded[4..]).unwrap();
        assert_eq!(decoded.protocol_version, PROTOCOL_VERSION_3_0);
        assert_eq!(decoded.params, vec![("user".to_string(), "app_user".to_string()), ("database".to_string(), "catalog".to_string()), ("application_name".to_string(), "av-catalog-test".to_string()), ("client_encoding".to_string(), "UTF8".to_string())]);
    }

    #[test]
    fn ssl_request_is_exactly_eight_bytes_with_the_magic_code() {
        let encoded = encode_ssl_request();
        assert_eq!(encoded, [0, 0, 0, 8, 4, 210, 22, 47]); // 8, then 80877103 big-endian
        assert_eq!(i32::from_be_bytes(encoded[4..8].try_into().unwrap()), SSL_REQUEST_CODE);
        decode_ssl_request_code(&encoded[4..8]).unwrap();
    }

    #[test]
    fn ssl_request_wrong_code_is_a_typed_error() {
        let err = decode_ssl_request_code(&999i32.to_be_bytes()).unwrap_err();
        assert!(matches!(err, CatalogError::InvalidSslRequestCode { code: 999 }));
    }

    // -- SASL frontend messages ----------------------------------------------------------

    #[test]
    fn sasl_initial_response_round_trips() {
        let encoded = encode_sasl_initial_response("SCRAM-SHA-256", b"n,,n=user,r=abc");
        let header: [u8; 5] = encoded[0..5].try_into().unwrap();
        let fh = read_frame_header(&header).unwrap();
        assert_eq!(fh.msg_type, b'p');
        let (mechanism, data) = decode_sasl_initial_response(&encoded[5..5 + fh.body_len]).unwrap();
        assert_eq!(mechanism, "SCRAM-SHA-256");
        assert_eq!(data, b"n,,n=user,r=abc");
    }

    #[test]
    fn sasl_initial_response_with_no_data_uses_negative_one_length() {
        let encoded = encode_sasl_initial_response("SCRAM-SHA-256", b"");
        let fh = read_frame_header(&encoded[0..5].try_into().unwrap()).unwrap();
        let (mechanism, data) = decode_sasl_initial_response(&encoded[5..5 + fh.body_len]).unwrap();
        assert_eq!(mechanism, "SCRAM-SHA-256");
        assert!(data.is_empty());
    }

    #[test]
    fn sasl_response_round_trips() {
        let encoded = encode_sasl_response(b"c=biws,r=abc,p=xyz");
        let fh = read_frame_header(&encoded[0..5].try_into().unwrap()).unwrap();
        let data = decode_sasl_response(&encoded[5..5 + fh.body_len]);
        assert_eq!(data, b"c=biws,r=abc,p=xyz");
    }

    // -- Query/Parse/Bind/Describe/Execute/Sync/Terminate --------------------------------

    #[test]
    fn query_round_trips() {
        let encoded = encode_query("SELECT 1");
        let fh = read_frame_header(&encoded[0..5].try_into().unwrap()).unwrap();
        assert_eq!(fh.msg_type, b'Q');
        assert_eq!(decode_query(&encoded[5..5 + fh.body_len]).unwrap(), "SELECT 1");
    }

    #[test]
    fn parse_round_trips_with_param_oids() {
        let encoded = encode_parse("", "SELECT $1::int4", &[23]);
        let fh = read_frame_header(&encoded[0..5].try_into().unwrap()).unwrap();
        let decoded = decode_parse(&encoded[5..5 + fh.body_len]).unwrap();
        assert_eq!(decoded.statement_name, "");
        assert_eq!(decoded.sql, "SELECT $1::int4");
        assert_eq!(decoded.param_type_oids, vec![23]);
    }

    #[test]
    fn bind_round_trips_with_a_null_and_a_text_param() {
        let encoded = encode_bind("", "", &[Some(b"42".as_slice()), None]);
        let fh = read_frame_header(&encoded[0..5].try_into().unwrap()).unwrap();
        let decoded = decode_bind(&encoded[5..5 + fh.body_len]).unwrap();
        assert_eq!(decoded.portal, "");
        assert_eq!(decoded.statement_name, "");
        assert_eq!(decoded.param_format_codes, vec![0]);
        assert_eq!(decoded.params, vec![Some(b"42".to_vec()), None]);
        assert_eq!(decoded.result_format_codes, vec![0]);
    }

    #[test]
    fn describe_round_trips_for_portal_and_statement() {
        for (target, byte) in [(DescribeTarget::Portal, b'P'), (DescribeTarget::Statement, b'S')] {
            let encoded = encode_describe(target, "name");
            let fh = read_frame_header(&encoded[0..5].try_into().unwrap()).unwrap();
            assert_eq!(encoded[5], byte);
            let (decoded_target, name) = decode_describe(&encoded[5..5 + fh.body_len]).unwrap();
            assert_eq!(decoded_target, target);
            assert_eq!(name, "name");
        }
    }

    #[test]
    fn execute_round_trips() {
        let encoded = encode_execute("", 0);
        let fh = read_frame_header(&encoded[0..5].try_into().unwrap()).unwrap();
        let (portal, max_rows) = decode_execute(&encoded[5..5 + fh.body_len]).unwrap();
        assert_eq!(portal, "");
        assert_eq!(max_rows, 0);
    }

    #[test]
    fn sync_and_terminate_are_four_byte_frames_with_no_body() {
        assert_eq!(encode_sync(), [b'S', 0, 0, 0, 4]);
        assert_eq!(encode_terminate(), [b'X', 0, 0, 0, 4]);
        decode_sync(&[]).unwrap();
        decode_terminate(&[]).unwrap();
    }

    // -- Backend messages ------------------------------------------------------------------

    #[test]
    fn authentication_ok_round_trips() {
        let encoded = encode_authentication_ok();
        let fh = read_frame_header(&encoded[0..5].try_into().unwrap()).unwrap();
        assert_eq!(fh.msg_type, b'R');
        assert_eq!(decode_backend_message(fh.msg_type, &encoded[5..5 + fh.body_len]).unwrap(), BackendMessage::AuthenticationOk);
    }

    #[test]
    fn authentication_cleartext_password_round_trips() {
        let encoded = encode_authentication_cleartext_password();
        let fh = read_frame_header(&encoded[0..5].try_into().unwrap()).unwrap();
        assert_eq!(decode_backend_message(fh.msg_type, &encoded[5..5 + fh.body_len]).unwrap(), BackendMessage::AuthenticationCleartextPassword);
    }

    #[test]
    fn authentication_md5_password_round_trips() {
        let encoded = encode_authentication_md5_password([1, 2, 3, 4]);
        let fh = read_frame_header(&encoded[0..5].try_into().unwrap()).unwrap();
        assert_eq!(decode_backend_message(fh.msg_type, &encoded[5..5 + fh.body_len]).unwrap(), BackendMessage::AuthenticationMd5Password { salt: [1, 2, 3, 4] });
    }

    #[test]
    fn authentication_sasl_round_trips() {
        let encoded = encode_authentication_sasl(&["SCRAM-SHA-256"]);
        let fh = read_frame_header(&encoded[0..5].try_into().unwrap()).unwrap();
        assert_eq!(decode_backend_message(fh.msg_type, &encoded[5..5 + fh.body_len]).unwrap(), BackendMessage::AuthenticationSasl { mechanisms: vec!["SCRAM-SHA-256".to_string()] });
    }

    #[test]
    fn authentication_sasl_continue_and_final_round_trip() {
        let encoded = encode_authentication_sasl_continue(b"r=abc,s=def,i=4096");
        let fh = read_frame_header(&encoded[0..5].try_into().unwrap()).unwrap();
        assert_eq!(decode_backend_message(fh.msg_type, &encoded[5..5 + fh.body_len]).unwrap(), BackendMessage::AuthenticationSaslContinue { data: b"r=abc,s=def,i=4096".to_vec() });

        let encoded = encode_authentication_sasl_final(b"v=xyz");
        let fh = read_frame_header(&encoded[0..5].try_into().unwrap()).unwrap();
        assert_eq!(decode_backend_message(fh.msg_type, &encoded[5..5 + fh.body_len]).unwrap(), BackendMessage::AuthenticationSaslFinal { data: b"v=xyz".to_vec() });
    }

    #[test]
    fn authentication_unsupported_code_is_named_not_a_panic() {
        // Code 7 = GSSAPI -- never implemented, but must decode to a typed, named variant.
        let mut body = 7i32.to_be_bytes().to_vec();
        body.extend_from_slice(b"unused gss bytes");
        assert_eq!(decode_backend_message(b'R', &body).unwrap(), BackendMessage::AuthenticationUnsupported { code: 7 });
    }

    #[test]
    fn backend_key_data_round_trips() {
        let encoded = encode_backend_key_data(1234, 5678);
        let fh = read_frame_header(&encoded[0..5].try_into().unwrap()).unwrap();
        assert_eq!(decode_backend_message(fh.msg_type, &encoded[5..5 + fh.body_len]).unwrap(), BackendMessage::BackendKeyData { process_id: 1234, secret_key: 5678 });
    }

    #[test]
    fn parameter_status_round_trips() {
        let encoded = encode_parameter_status("server_version", "17.11");
        let fh = read_frame_header(&encoded[0..5].try_into().unwrap()).unwrap();
        assert_eq!(decode_backend_message(fh.msg_type, &encoded[5..5 + fh.body_len]).unwrap(), BackendMessage::ParameterStatus { name: "server_version".to_string(), value: "17.11".to_string() });
    }

    #[test]
    fn ready_for_query_round_trips_all_three_statuses() {
        for status in [TransactionStatus::Idle, TransactionStatus::InTransaction, TransactionStatus::Failed] {
            let encoded = encode_ready_for_query(status);
            let fh = read_frame_header(&encoded[0..5].try_into().unwrap()).unwrap();
            assert_eq!(decode_backend_message(fh.msg_type, &encoded[5..5 + fh.body_len]).unwrap(), BackendMessage::ReadyForQuery { status });
        }
    }

    #[test]
    fn ready_for_query_invalid_status_byte_is_typed() {
        let err = decode_backend_message(b'Z', b"X").unwrap_err();
        assert!(matches!(err, CatalogError::InvalidTransactionStatus { byte: b'X' }));
    }

    fn sample_row_description() -> Vec<FieldDescription> {
        vec![
            FieldDescription { name: "id".to_string(), table_oid: 16400, column_id: 1, type_oid: 23, type_size: 4, type_modifier: -1, format_code: 0 },
            FieldDescription { name: "label".to_string(), table_oid: 16400, column_id: 2, type_oid: 25, type_size: -1, type_modifier: -1, format_code: 0 },
        ]
    }

    #[test]
    fn row_description_round_trips() {
        let fields = sample_row_description();
        let encoded = encode_row_description(&fields);
        let fh = read_frame_header(&encoded[0..5].try_into().unwrap()).unwrap();
        assert_eq!(decode_backend_message(fh.msg_type, &encoded[5..5 + fh.body_len]).unwrap(), BackendMessage::RowDescription { fields });
    }

    #[test]
    fn data_row_round_trips_with_a_null() {
        let encoded = encode_data_row(&[Some(b"7".as_slice()), None, Some(b"orbit-42".as_slice())]);
        let fh = read_frame_header(&encoded[0..5].try_into().unwrap()).unwrap();
        assert_eq!(decode_backend_message(fh.msg_type, &encoded[5..5 + fh.body_len]).unwrap(), BackendMessage::DataRow { values: vec![Some(b"7".to_vec()), None, Some(b"orbit-42".to_vec())] });
    }

    #[test]
    fn command_complete_round_trips() {
        let encoded = encode_command_complete("SELECT 3");
        let fh = read_frame_header(&encoded[0..5].try_into().unwrap()).unwrap();
        assert_eq!(decode_backend_message(fh.msg_type, &encoded[5..5 + fh.body_len]).unwrap(), BackendMessage::CommandComplete { tag: "SELECT 3".to_string() });
    }

    #[test]
    fn zero_body_backend_messages_round_trip() {
        for (encoded, expected) in [(encode_empty_query_response(), BackendMessage::EmptyQueryResponse), (encode_no_data(), BackendMessage::NoData), (encode_parse_complete(), BackendMessage::ParseComplete), (encode_bind_complete(), BackendMessage::BindComplete), (encode_portal_suspended(), BackendMessage::PortalSuspended)] {
            let fh = read_frame_header(&encoded[0..5].try_into().unwrap()).unwrap();
            assert_eq!(decode_backend_message(fh.msg_type, &encoded[5..5 + fh.body_len]).unwrap(), expected);
        }
    }

    #[test]
    fn parameter_description_round_trips() {
        let encoded = encode_parameter_description(&[23, 25]);
        let fh = read_frame_header(&encoded[0..5].try_into().unwrap()).unwrap();
        assert_eq!(decode_backend_message(fh.msg_type, &encoded[5..5 + fh.body_len]).unwrap(), BackendMessage::ParameterDescription { param_type_oids: vec![23, 25] });
    }

    fn sample_server_error() -> ServerError {
        ServerError {
            severity: "ERROR".to_string(),
            sqlstate: "23505".to_string(),
            message: "duplicate key value violates unique constraint".to_string(),
            detail: Some("Key (id)=(1) already exists.".to_string()),
            hint: None,
            position: None,
            where_: None,
            schema: Some("public".to_string()),
            table: Some("assets".to_string()),
            column: None,
            constraint: Some("assets_pkey".to_string()),
            file: Some("nbtinsert.c".to_string()),
            line: Some("664".to_string()),
            routine: Some("_bt_check_unique".to_string()),
        }
    }

    #[test]
    fn error_response_decodes_every_named_field() {
        let expected = sample_server_error();
        let encoded = encode_error_response(&expected);
        let fh = read_frame_header(&encoded[0..5].try_into().unwrap()).unwrap();
        assert_eq!(fh.msg_type, b'E');
        assert_eq!(decode_backend_message(fh.msg_type, &encoded[5..5 + fh.body_len]).unwrap(), BackendMessage::ErrorResponse(expected));
    }

    #[test]
    fn notice_response_decodes_the_same_shape_as_error_response() {
        let expected = sample_server_error();
        let encoded = encode_notice_response(&expected);
        let fh = read_frame_header(&encoded[0..5].try_into().unwrap()).unwrap();
        assert_eq!(fh.msg_type, b'N');
        assert_eq!(decode_backend_message(fh.msg_type, &encoded[5..5 + fh.body_len]).unwrap(), BackendMessage::NoticeResponse(expected));
    }

    #[test]
    fn error_response_unrecognized_field_code_is_ignored_not_an_error() {
        // 'q' = internal query -- a real field code this crate's ServerError has no slot for.
        let mut body = Vec::new();
        body.push(b'V');
        write_cstr(&mut body, "ERROR");
        body.push(b'C');
        write_cstr(&mut body, "42601");
        body.push(b'M');
        write_cstr(&mut body, "syntax error");
        body.push(b'q');
        write_cstr(&mut body, "SELECT $$$$");
        body.push(0);
        let decoded = decode_backend_message(b'E', &body).unwrap();
        assert_eq!(decoded, BackendMessage::ErrorResponse(ServerError { severity: "ERROR".to_string(), sqlstate: "42601".to_string(), message: "syntax error".to_string(), ..Default::default() }));
    }

    #[test]
    fn error_response_missing_mandatory_field_is_typed() {
        // No 'M' (message) field at all.
        let mut body = Vec::new();
        body.push(b'S');
        write_cstr(&mut body, "ERROR");
        body.push(b'C');
        write_cstr(&mut body, "42601");
        body.push(0);
        let err = decode_backend_message(b'E', &body).unwrap_err();
        assert!(matches!(err, CatalogError::MissingErrorField { code: 'M', .. }));
    }

    #[test]
    fn unknown_message_type_byte_is_typed_not_a_panic() {
        let err = decode_backend_message(b'?', &[]).unwrap_err();
        assert!(matches!(err, CatalogError::UnknownMessageType { type_byte: b'?', direction: "backend" }));
    }

    // -- Frame header bounds ---------------------------------------------------------------

    #[test]
    fn frame_header_rejects_a_length_shorter_than_itself() {
        let err = read_frame_header(&[b'Q', 0, 0, 0, 3]).unwrap_err();
        assert!(matches!(err, CatalogError::InvalidFrameLength { length: 3, .. }));
    }

    #[test]
    fn frame_header_rejects_a_length_over_the_cap() {
        let too_big = (MAX_MESSAGE_LEN as i32).saturating_add(5);
        let mut header = [b'D', 0, 0, 0, 0];
        header[1..5].copy_from_slice(&too_big.to_be_bytes());
        let err = read_frame_header(&header).unwrap_err();
        assert!(matches!(err, CatalogError::InvalidFrameLength { .. }));
    }

    // -- Exhaustive truncation tests (this task's own explicit requirement) ----------------

    /// Every message this module can encode, paired with the type byte a caller would read
    /// from its frame header (frontend messages carry no meaningful type byte for this table's
    /// purposes, but `decode_backend_message` is only reachable for backend types, so this
    /// table drives the two decode surfaces this module actually exposes: the backend
    /// dispatcher, and each frontend message's own dedicated decode function via the closure
    /// below).
    /// One truncation-test case: `name` is used only in assertion messages; `encoded` is a
    /// full, valid wire encoding of the message (header included); `decode` attempts to parse
    /// a body slice and reports only success/failure (the decoded value itself is not
    /// interesting to this test, only whether decoding a truncated slice is correctly refused).
    /// A named struct rather than a tuple, deliberately: a `Vec<(&str, Vec<u8>, Box<dyn
    /// Fn(..)->..>)>` return type is exactly the shape `clippy::type_complexity` (a default
    /// warning, an error under this workspace's `-D warnings` gate) exists to flag, and the
    /// named fields read better at each call site below regardless.
    /// A type alias, not the `Box<dyn Fn(..) -> Result<(), ..>>` shape spelled out inline on
    /// the struct field below: `clippy::type_complexity` (a hard error under this workspace's
    /// `-D warnings` gate) is triggered by that syntactic shape wherever it is written out
    /// literally, so naming it once here -- rather than fighting the lint with `#[allow(...)]`,
    /// which rule 3 forbids -- also reads better at each of this test's own call sites below:
    /// "a decode attempt that only reports success/failure".
    type DecodeFn = Box<dyn Fn(&[u8]) -> Result<(), CatalogError>>;

    struct TruncationCase {
        name: &'static str,
        encoded: Vec<u8>,
        decode: DecodeFn,
    }

    fn truncation_cases() -> Vec<TruncationCase> {
        fn backend(name: &'static str, msg_type: u8, encoded: Vec<u8>) -> TruncationCase {
            TruncationCase { name, encoded, decode: Box::new(move |body: &[u8]| decode_backend_message(msg_type, body).map(|_| ())) }
        }
        fn frontend(name: &'static str, encoded: Vec<u8>, decode: impl Fn(&[u8]) -> Result<(), CatalogError> + 'static) -> TruncationCase {
            TruncationCase { name, encoded, decode: Box::new(decode) }
        }
        vec![
            backend("AuthenticationOk", b'R', encode_authentication_ok()),
            backend("AuthenticationCleartextPassword", b'R', encode_authentication_cleartext_password()),
            backend("AuthenticationMd5Password", b'R', encode_authentication_md5_password([9, 9, 9, 9])),
            backend("AuthenticationSasl", b'R', encode_authentication_sasl(&["SCRAM-SHA-256"])),
            // AuthenticationSASLContinue/AuthenticationSASLFinal are deliberately NOT in this
            // list -- see `sasl_continue_and_final_reject_truncation_only_inside_the_auth_code`
            // below for why a generic "every truncation must fail" sweep is the wrong test for
            // them.
            backend("BackendKeyData", b'K', encode_backend_key_data(42, 99)),
            backend("ParameterStatus", b'S', encode_parameter_status("k", "v")),
            backend("ReadyForQuery", b'Z', encode_ready_for_query(TransactionStatus::Idle)),
            backend("RowDescription", b'T', encode_row_description(&sample_row_description())),
            backend("DataRow", b'D', encode_data_row(&[Some(b"1".as_slice()), None])),
            backend("CommandComplete", b'C', encode_command_complete("SELECT 3")),
            backend("EmptyQueryResponse", b'I', encode_empty_query_response()),
            backend("NoData", b'n', encode_no_data()),
            backend("ParseComplete", b'1', encode_parse_complete()),
            backend("BindComplete", b'2', encode_bind_complete()),
            backend("ParameterDescription", b't', encode_parameter_description(&[23, 25])),
            backend("PortalSuspended", b's', encode_portal_suspended()),
            backend("ErrorResponse", b'E', encode_error_response(&sample_server_error())),
            backend("NoticeResponse", b'N', encode_notice_response(&sample_server_error())),
            frontend("SASLInitialResponse", encode_sasl_initial_response("SCRAM-SHA-256", b"n,,n=user,r=abc"), |body| decode_sasl_initial_response(body).map(|_| ())),
            frontend("Query", encode_query("SELECT 1"), |body| decode_query(body).map(|_| ())),
            frontend("Parse", encode_parse("", "SELECT $1", &[23]), |body| decode_parse(body).map(|_| ())),
            frontend("Bind", encode_bind("", "", &[Some(b"1".as_slice()), None]), |body| decode_bind(body).map(|_| ())),
            frontend("Describe", encode_describe(DescribeTarget::Portal, "name"), |body| decode_describe(body).map(|_| ())),
            frontend("Execute", encode_execute("", 0), |body| decode_execute(body).map(|_| ())),
            frontend("StartupMessage", encode_startup_message(&StartupParams { user: "u", database: "d", application_name: "a" }), |body| decode_startup_message(body).map(|_| ())),
        ]
    }

    /// This task's own explicit requirement: "every message type decoded from a deliberately
    /// truncated buffer at every length from 0 to its full size, asserting a typed error each
    /// time and no panic." For every case above, this strips the frame header (5 bytes for a
    /// backend/most frontend messages, 4 for `StartupMessage`) and then feeds
    /// `body[..n]` for every `n` from 0 up to (but not including) the full body length,
    /// asserting each one is `Err` -- and separately confirms the FULL body decodes `Ok`, so a
    /// bug that makes the decoder too lenient (accepting a truncated buffer) is caught just as
    /// surely as a panic would be.
    #[test]
    fn every_message_type_rejects_every_truncation_of_its_body() {
        for case in truncation_cases() {
            let (header_len, body_len) = if case.name == "StartupMessage" {
                (4, read_untyped_frame_header(&case.encoded[0..4].try_into().unwrap()).unwrap())
            } else {
                let fh = read_frame_header(&case.encoded[0..5].try_into().unwrap()).unwrap();
                (5, fh.body_len)
            };
            let full_body = &case.encoded[header_len..header_len + body_len];
            assert!((case.decode)(full_body).is_ok(), "{}: full-length body unexpectedly failed to decode", case.name);
            for n in 0..body_len {
                let truncated = &full_body[..n];
                assert!((case.decode)(truncated).is_err(), "{}: truncated to {n}/{body_len} bytes decoded Ok, expected a typed error", case.name);
            }
        }
    }

    /// `AuthenticationSASLContinue`/`AuthenticationSASLFinal`'s body is `Int32 code` followed
    /// by raw bytes with NO length field of their own -- the outer frame header's length is
    /// the only length that governs how much data there is. That makes a body truncated
    /// somewhere AFTER the 4-byte code a legitimately shorter, still-valid SASLContinue/
    /// SASLFinal payload, not a malformed one -- which is exactly why these two message types
    /// are excluded from `every_message_type_rejects_every_truncation_of_its_body`'s generic
    /// sweep above (a sweep that assumes every truncation is invalid) and get this narrower,
    /// correct test instead: truncation strictly INSIDE the 4-byte code is still always a
    /// typed error (the code itself has a fixed length), but truncation anywhere at or after
    /// byte 4 must still decode `Ok`.
    #[test]
    fn sasl_continue_and_final_reject_truncation_only_inside_the_auth_code() {
        for (name, encoded) in [("AuthenticationSASLContinue", encode_authentication_sasl_continue(b"r=abc,s=def,i=4096")), ("AuthenticationSASLFinal", encode_authentication_sasl_final(b"v=xyz"))] {
            let fh = read_frame_header(&encoded[0..5].try_into().unwrap()).unwrap();
            let full_body = &encoded[5..5 + fh.body_len];
            for n in 0..4 {
                assert!(decode_backend_message(b'R', &full_body[..n]).is_err(), "{name}: {n} byte(s) (inside the 4-byte code) unexpectedly decoded Ok");
            }
            for n in 4..=full_body.len() {
                assert!(decode_backend_message(b'R', &full_body[..n]).is_ok(), "{name}: {n} byte(s) (a valid, if short, data trailer) unexpectedly failed to decode");
            }
        }
    }

    #[test]
    fn error_response_and_row_description_reject_a_length_prefix_that_lies_upward() {
        // A well-formed body with one extra trailing byte the length prefix would have to lie
        // about to include -- Reader::finish's TrailingBytes check, exercised directly (the
        // truncation loop above only ever shortens a buffer, never lengthens one).
        let mut body = encode_command_complete("SELECT 1");
        let fh = read_frame_header(&body[0..5].try_into().unwrap()).unwrap();
        let mut with_garbage = body.split_off(5);
        with_garbage.truncate(fh.body_len);
        with_garbage.push(0xff);
        let err = decode_backend_message(b'C', &with_garbage).unwrap_err();
        assert!(matches!(err, CatalogError::TrailingBytes { .. }));
    }
}
