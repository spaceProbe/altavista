//! [`Request`] and [`Response`], and the request validation that makes header injection and
//! request smuggling impossible: nothing here reaches the wire unvalidated.

use crate::error::HttpError;

/// Headers the client writes itself. A caller that supplies one gets
/// [`HttpError::InvalidRequest`] rather than a silent drop or, worse, a second copy: a second
/// `Content-Length` or a `Transfer-Encoding` next to the client's own framing is exactly how
/// request smuggling starts.
const CLIENT_OWNED: [&str; 4] = ["host", "content-length", "transfer-encoding", "connection"];

/// A request: method, path-and-query, headers and a body. Build with [`Request::new`] (or
/// [`Request::get`] / [`Request::post`] / ...) and the `header` / `body` builders. Validation
/// happens in [`crate::Client::request`], before any connection is made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub(crate) method: String,
    pub(crate) target: String,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: Vec<u8>,
}

impl Request {
    /// A request with `method` and `path_and_query` (origin-form, starting with `/`, already
    /// percent-encoded; the endpoint's base path is put in front of it on the wire).
    pub fn new(method: impl Into<String>, path_and_query: impl Into<String>) -> Request {
        Request {
            method: method.into(),
            target: path_and_query.into(),
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    /// `GET`.
    pub fn get(path_and_query: impl Into<String>) -> Request {
        Request::new("GET", path_and_query)
    }

    /// `POST`.
    pub fn post(path_and_query: impl Into<String>) -> Request {
        Request::new("POST", path_and_query)
    }

    /// `PUT`.
    pub fn put(path_and_query: impl Into<String>) -> Request {
        Request::new("PUT", path_and_query)
    }

    /// `DELETE`.
    pub fn delete(path_and_query: impl Into<String>) -> Request {
        Request::new("DELETE", path_and_query)
    }

    /// Appends a header (repeats are kept, in order).
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Request {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Sets the body.
    pub fn body(mut self, body: impl Into<Vec<u8>>) -> Request {
        self.body = body.into();
        self
    }

    /// The method as given.
    pub fn method(&self) -> &str {
        &self.method
    }

    /// The path-and-query as given (without the base path).
    pub fn path_and_query(&self) -> &str {
        &self.target
    }

    /// Refuses anything that could alter the request's framing or smuggle a second request.
    pub(crate) fn validate(&self) -> Result<(), HttpError> {
        let bad = |why: String| Err(HttpError::InvalidRequest(why));
        if self.method.is_empty() || !self.method.bytes().all(is_tchar) {
            return bad(format!("method {:?} is not an HTTP token", self.method));
        }
        if !self.target.starts_with('/') {
            return bad(format!("path {:?} must start with '/'", self.target));
        }
        if let Some(b) = self.target.bytes().find(|&b| b <= 0x20 || b >= 0x7f || b == b'#') {
            return bad(format!(
                "path holds byte 0x{b:02x} (control, space, non-ASCII or '#'); percent-encode it"
            ));
        }
        for (name, value) in &self.headers {
            if name.is_empty() || !name.bytes().all(is_tchar) {
                return bad(format!("header name {name:?} is not an HTTP token"));
            }
            if CLIENT_OWNED.iter().any(|c| name.eq_ignore_ascii_case(c)) {
                return bad(format!("header {name:?} is written by the client and may not be supplied"));
            }
            if value.bytes().any(|b| (b < 0x20 && b != b'\t') || b == 0x7f) {
                return bad(format!("value of header {name:?} holds a control character (CR, LF, NUL, ...)"));
            }
        }
        Ok(())
    }
}

/// RFC 9110 `tchar`.
pub(crate) fn is_tchar(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)
}

/// A response: status, reason phrase, headers (wire order, repeats kept) and the body bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    /// The status code, 100-999 as parsed (never a skipped 1xx interim response).
    pub status: u16,
    /// The reason phrase (possibly empty).
    pub reason: String,
    /// Header (name, value) pairs in wire order; names keep the server's capitalisation.
    pub headers: Vec<(String, String)>,
    /// The decoded body (chunked framing removed).
    pub body: Vec<u8>,
}

impl Response {
    /// The first header named `name`, compared case-insensitively.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// Every header named `name` (case-insensitively), in wire order.
    pub fn header_all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.headers
            .iter()
            .filter(move |(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// Whether the status is 2xx. Callers type non-2xx themselves; this layer never errors on it.
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}
