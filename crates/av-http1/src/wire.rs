//! Response parsing: status line, headers, the three body framings. Generic over a [`Source`]
//! (the connection) so it holds no socket or TLS knowledge.
//!
//! Strictness, deliberately: line endings are CRLF only (a bare CR or LF anywhere in the head is a
//! protocol error), obsolete header folding is refused, a header name is an HTTP token with no
//! whitespace before the colon, `Content-Length` is plain digits (identical repeats tolerated,
//! different ones refused), and `Transfer-Encoding` must be exactly `chunked`. Header values are
//! converted with `from_utf8_lossy` (a non-UTF-8 byte becomes U+FFFD); the body is raw bytes.

use crate::error::HttpError;
use crate::message::{is_tchar, Response};

/// Size limits for one response.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Limits {
    pub max_header_bytes: usize,
    pub max_body_bytes: usize,
}

/// Where response bytes come from: one read, already mapped to [`HttpError`] (timeout, TLS, I/O).
/// `Ok(0)` is end of stream.
pub(crate) trait Source {
    fn read_some(&mut self, buf: &mut [u8]) -> Result<usize, HttpError>;
    /// True when the stream ended without a TLS close_notify (so an EOF-delimited body cannot be
    /// trusted to be complete). Always false for plaintext.
    fn unclean_eof(&self) -> bool;
}

fn proto<T>(why: impl Into<String>) -> Result<T, HttpError> {
    Err(HttpError::Protocol(why.into()))
}

struct Reader<'a, S: Source> {
    src: &'a mut S,
    buf: Vec<u8>,
    pos: usize,
}

impl<S: Source> Reader<'_, S> {
    fn available(&self) -> &[u8] {
        &self.buf[self.pos..]
    }

    /// Reads more from the source; `Ok(0)` means end of stream.
    fn fill(&mut self) -> Result<usize, HttpError> {
        if self.pos == self.buf.len() {
            self.buf.clear();
            self.pos = 0;
        } else if self.pos > 32 * 1024 {
            self.buf.drain(..self.pos);
            self.pos = 0;
        }
        let mut tmp = [0u8; 16 * 1024];
        let n = self.src.read_some(&mut tmp)?;
        self.buf.extend_from_slice(&tmp[..n]);
        Ok(n)
    }

    /// The bytes up to (not including) the next `\r\n\r\n`, consuming the terminator. `budget` is
    /// the remaining header allowance; the terminator counts against it.
    fn read_head(&mut self, budget: &mut usize, limit: usize) -> Result<Vec<u8>, HttpError> {
        loop {
            if let Some(i) = find(self.available(), b"\r\n\r\n") {
                let total = i + 4;
                if total > *budget {
                    return Err(HttpError::HeaderTooLarge { limit });
                }
                let head = self.available()[..i].to_vec();
                self.pos += total;
                *budget -= total;
                return Ok(head);
            }
            if self.available().len() >= *budget {
                return Err(HttpError::HeaderTooLarge { limit });
            }
            if self.fill()? == 0 {
                return if self.available().is_empty() && *budget == limit {
                    proto("connection closed before any response bytes")
                } else {
                    proto("connection closed inside the response header section")
                };
            }
        }
    }

    /// One line up to (not including) `\r\n`, at most `max` bytes before the CRLF.
    fn read_line(&mut self, max: usize, too_long: impl Fn() -> HttpError) -> Result<Vec<u8>, HttpError> {
        loop {
            if let Some(i) = find(self.available(), b"\r\n") {
                if i > max {
                    return Err(too_long());
                }
                let line = self.available()[..i].to_vec();
                self.pos += i + 2;
                return Ok(line);
            }
            if self.available().len() > max + 1 {
                return Err(too_long());
            }
            if self.fill()? == 0 {
                return proto("connection closed inside a chunked body");
            }
        }
    }

    /// Appends exactly `n` bytes to `out`.
    fn read_exact_into(&mut self, out: &mut Vec<u8>, mut n: u64, what: &str) -> Result<(), HttpError> {
        while n > 0 {
            if self.available().is_empty() && self.fill()? == 0 {
                return proto(format!("{what} truncated: connection closed with {n} byte(s) still expected"));
            }
            let take = (self.available().len() as u64).min(n) as usize;
            out.extend_from_slice(&self.available()[..take]);
            self.pos += take;
            n -= take as u64;
        }
        Ok(())
    }

    /// Everything until end of stream, at most `max` bytes.
    fn read_to_close(&mut self, max: usize) -> Result<Vec<u8>, HttpError> {
        let mut out = Vec::new();
        loop {
            if out.len() + self.available().len() > max {
                return Err(HttpError::BodyTooLarge { limit: max });
            }
            out.extend_from_slice(self.available());
            self.pos = self.buf.len();
            if self.fill()? == 0 {
                break;
            }
        }
        if self.src.unclean_eof() {
            return proto(
                "the TLS stream ended without close_notify, so a response delimited only by the connection \
                 closing cannot be trusted to be complete",
            );
        }
        Ok(out)
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Reads one complete response (skipping 1xx interim responses). `is_head` marks a reply to a
/// `HEAD` request, which never has a body whatever its headers say.
pub(crate) fn read_response<S: Source>(src: &mut S, is_head: bool, limits: Limits) -> Result<Response, HttpError> {
    let mut rd = Reader {
        src,
        buf: Vec::new(),
        pos: 0,
    };
    let mut budget = limits.max_header_bytes;
    let (status, reason, headers) = loop {
        let head = rd.read_head(&mut budget, limits.max_header_bytes)?;
        let (status, reason, headers) = parse_head(&head)?;
        match status {
            101 => return proto("unexpected 101 Switching Protocols (the client never asks to upgrade)"),
            100..=199 => continue,
            _ => break (status, reason, headers),
        }
    };

    let body = match framing(status, &headers, is_head)? {
        Framing::NoBody => Vec::new(),
        Framing::Length(n) => {
            if n > limits.max_body_bytes as u64 {
                return Err(HttpError::BodyTooLarge {
                    limit: limits.max_body_bytes,
                });
            }
            let mut out = Vec::with_capacity((n as usize).min(1 << 20));
            rd.read_exact_into(&mut out, n, "Content-Length body")?;
            out
        }
        Framing::Chunked => read_chunked(&mut rd, limits)?,
        Framing::Close => rd.read_to_close(limits.max_body_bytes)?,
    };
    Ok(Response {
        status,
        reason,
        headers,
        body,
    })
}

fn read_chunked<S: Source>(rd: &mut Reader<'_, S>, limits: Limits) -> Result<Vec<u8>, HttpError> {
    let mut body = Vec::new();
    let size_line_max = limits.max_header_bytes;
    loop {
        let line = rd.read_line(size_line_max, || HttpError::Protocol("chunk-size line too long".into()))?;
        let size_text = match line.iter().position(|&b| b == b';') {
            Some(i) => &line[..i],
            None => &line[..],
        };
        let size_text = trim_ows_end(size_text);
        if size_text.is_empty() || size_text.len() > 16 || !size_text.iter().all(u8::is_ascii_hexdigit) {
            return proto(format!(
                "bad chunk size {:?}",
                String::from_utf8_lossy(&line[..line.len().min(40)])
            ));
        }
        // ASCII hex digits only, so this is valid UTF-8 and u64::from_str_radix sees no sign.
        let size = u64::from_str_radix(std::str::from_utf8(size_text).unwrap_or("0"), 16)
            .map_err(|_| HttpError::Protocol("bad chunk size".into()))?;
        if size == 0 {
            break;
        }
        if size > (limits.max_body_bytes - body.len()) as u64 {
            return Err(HttpError::BodyTooLarge {
                limit: limits.max_body_bytes,
            });
        }
        rd.read_exact_into(&mut body, size, "chunk")?;
        let mut crlf = Vec::with_capacity(2);
        rd.read_exact_into(&mut crlf, 2, "chunk terminator")?;
        if crlf != b"\r\n" {
            return proto("chunk data is not followed by CRLF");
        }
    }
    // Trailers: read to the blank line and discard, within their own header-sized allowance.
    let mut left = limits.max_header_bytes;
    loop {
        let line = rd.read_line(left, || HttpError::HeaderTooLarge {
            limit: limits.max_header_bytes,
        })?;
        if line.is_empty() {
            return Ok(body);
        }
        left = left.saturating_sub(line.len() + 2);
    }
}

fn trim_ows_end(mut s: &[u8]) -> &[u8] {
    while let [rest @ .., b' ' | b'\t'] = s {
        s = rest;
    }
    s
}

enum Framing {
    NoBody,
    Length(u64),
    Chunked,
    Close,
}

fn framing(status: u16, headers: &[(String, String)], is_head: bool) -> Result<Framing, HttpError> {
    let values = |name: &str| {
        headers
            .iter()
            .filter(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
            .collect::<Vec<_>>()
    };
    let te = values("transfer-encoding");
    let cl = values("content-length");
    if !te.is_empty() && !cl.is_empty() {
        return proto("response has both Transfer-Encoding and Content-Length");
    }
    if is_head || status == 204 || status == 304 {
        return Ok(Framing::NoBody);
    }
    if !te.is_empty() {
        let codings: Vec<&str> = te.iter().flat_map(|v| v.split(',')).map(str::trim).collect();
        if codings.len() == 1 && codings[0].eq_ignore_ascii_case("chunked") {
            return Ok(Framing::Chunked);
        }
        return proto(format!("unsupported Transfer-Encoding {:?} (only chunked)", te.join(", ")));
    }
    if !cl.is_empty() {
        let mut parsed: Option<u64> = None;
        for v in cl.iter().flat_map(|v| v.split(',')).map(str::trim) {
            if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) {
                return proto(format!("bad Content-Length {v:?}"));
            }
            let n: u64 = v
                .parse()
                .map_err(|_| HttpError::Protocol(format!("Content-Length {v:?} overflows")))?;
            match parsed {
                Some(prev) if prev != n => return proto("conflicting Content-Length values"),
                _ => parsed = Some(n),
            }
        }
        return Ok(Framing::Length(parsed.unwrap_or(0)));
    }
    Ok(Framing::Close)
}

type Head = (u16, String, Vec<(String, String)>);

fn parse_head(head: &[u8]) -> Result<Head, HttpError> {
    if head.contains(&0) {
        return proto("NUL byte in the response header section");
    }
    // Split strictly on CRLF; any CR or LF left inside a line is then a bare one.
    let mut parts: Vec<&[u8]> = Vec::new();
    let mut start = 0;
    while let Some(i) = find(&head[start..], b"\r\n") {
        parts.push(&head[start..start + i]);
        start += i + 2;
    }
    parts.push(&head[start..]);
    if parts.iter().any(|l| l.contains(&b'\r') || l.contains(&b'\n')) {
        return proto("bare CR or LF in the response header section");
    }

    let (status, reason) = parse_status_line(parts[0])?;
    let mut headers = Vec::with_capacity(parts.len().saturating_sub(1));
    for line in &parts[1..] {
        if line.first().is_some_and(|b| *b == b' ' || *b == b'\t') {
            return proto("obsolete header line folding is refused");
        }
        let Some(colon) = line.iter().position(|&b| b == b':') else {
            return proto(format!(
                "header line without a colon: {:?}",
                String::from_utf8_lossy(&line[..line.len().min(40)])
            ));
        };
        let name = &line[..colon];
        if name.is_empty() || !name.iter().copied().all(is_tchar) {
            return proto(format!(
                "bad header name {:?}",
                String::from_utf8_lossy(&name[..name.len().min(40)])
            ));
        }
        let value = trim_ows(&line[colon + 1..]);
        headers.push((
            String::from_utf8_lossy(name).into_owned(),
            String::from_utf8_lossy(value).into_owned(),
        ));
    }
    Ok((status, reason, headers))
}

fn trim_ows(s: &[u8]) -> &[u8] {
    let mut s = trim_ows_end(s);
    while let [b' ' | b'\t', rest @ ..] = s {
        s = rest;
    }
    s
}

fn parse_status_line(line: &[u8]) -> Result<(u16, String), HttpError> {
    let shown = || String::from_utf8_lossy(&line[..line.len().min(60)]).into_owned();
    let rest = line
        .strip_prefix(b"HTTP/1.1")
        .or_else(|| line.strip_prefix(b"HTTP/1.0"))
        .ok_or_else(|| HttpError::Protocol(format!("malformed status line {:?}", shown())))?;
    let rest = rest
        .strip_prefix(b" ")
        .ok_or_else(|| HttpError::Protocol(format!("malformed status line {:?}", shown())))?;
    if rest.len() < 3 || !rest[..3].iter().all(u8::is_ascii_digit) || !(rest.len() == 3 || rest[3] == b' ') {
        return proto(format!("malformed status line {:?}", shown()));
    }
    let status = u16::from(rest[0] - b'0') * 100 + u16::from(rest[1] - b'0') * 10 + u16::from(rest[2] - b'0');
    if status < 100 {
        return proto(format!("status code {status} out of range"));
    }
    let reason = if rest.len() > 4 {
        String::from_utf8_lossy(&rest[4..]).into_owned()
    } else {
        String::new()
    };
    Ok((status, reason))
}
