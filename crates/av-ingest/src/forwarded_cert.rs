//! The forwarded-client-certificate header contract (question 202, E3b's "open item 1"):
//! the exact gRPC metadata header a service-owned nginx mTLS front (question 155's rule;
//! `services/gmat-service/deploy/`'s template) uses to hand this service the client
//! certificate it terminated, so [`av_edge::identity::verify_identity`] can re-verify it
//! **in process** -- a wrong-CA or lapsed identity must land in `IdentityCounters`/
//! `EvidenceResponse`, not only in nginx's own access log.
//!
//! # The header, named explicitly
//!
//! [`FORWARDED_CLIENT_CERT_HEADER`] = `"x-ssl-client-escaped-cert"`, carrying nginx's own
//! [`$ssl_client_escaped_cert`][nginx-ssl-var] variable value verbatim (a `proxy_set_header`
//! line in the rendered nginx config -- not built or rendered by this task, see
//! `docs/edge-plan.md` milestone E3b's own scope note; this module is the contract the
//! next task's rendered config must honour). It is a plain, non-reserved gRPC metadata
//! key (lowercase, hyphenated, no `grpc-` prefix), set once per RPC the same way any other
//! metadata header is.
//!
//! [nginx-ssl-var]: https://nginx.org/en/docs/http/ngx_http_ssl_module.html
//!
//! # What was actually checked, and what was not
//!
//! This host's `nginx` binary exists (`/opt/homebrew/bin/nginx`, 1.31.3), but starting it
//! from this task's shell is blocked by a host-level shell-command allowlist unrelated to
//! this platform's own permission system (a `lean-ctx` sandbox overlay refuses the literal
//! command name `nginx`), so **`$ssl_client_escaped_cert`'s actual on-the-wire bytes were
//! not observed by running nginx in this task** -- the attempt, and the refusal, are
//! recorded in this task's own report rather than silently worked around. What follows is
//! therefore nginx's own documented behaviour (fetched from
//! <https://nginx.org/en/docs/http/ngx_http_ssl_module.html> during this task), quoted
//! rather than guessed:
//!
//! - `$ssl_client_cert` (**deprecated**): "returns the client certificate in the PEM
//!   format for an established SSL connection, with each line except the first prepended
//!   with the tab character; this is intended for the use in the `proxy_set_header`
//!   directive". This is where a "newlines become tabs" rule actually comes from in
//!   nginx's own documented behaviour -- but it is the *deprecated* variable's own
//!   escaping, not `$ssl_client_escaped_cert`'s.
//! - `$ssl_client_escaped_cert` (nginx 1.13.5+, what [`FORWARDED_CLIENT_CERT_HEADER`]
//!   actually carries): "returns the client certificate in the PEM format (urlencoded)
//!   for an established SSL connection". Percent-encoding, not tab substitution -- every
//!   byte outside nginx's own URI-unreserved set (letters, digits, and a handful of
//!   punctuation) becomes `%XX`, including the PEM's own newlines (`%0A`).
//!
//! [`percent_decode`] therefore implements **percent-decoding** as the primary,
//! documented-behaviour path (any `%XX` triplet becomes one byte; every other byte passes
//! through unchanged -- a decoder needs no escaping *table* to invert an encoder's, since
//! decoding accepts any percent-triplet regardless of which bytes a particular encoder
//! chose to escape). As one further defence -- not a guess about
//! `$ssl_client_escaped_cert` itself, but a documented behaviour of nginx's *other*,
//! deprecated variable, which a misconfigured front might use by mistake (naming this
//! header from `$ssl_client_cert` instead of `$ssl_client_escaped_cert` in its
//! `proxy_set_header` line) -- [`percent_decode`] also folds a literal tab character
//! (`\t`) at the start of an interior line back into the newline `$ssl_client_cert`'s own
//! documented behaviour replaced it with, so a leaf forwarded under either variable's
//! escaping still parses as PEM. This fallback is applied *after* percent-decoding (a
//! `$ssl_client_escaped_cert` value's own `%09` for a literal tab byte inside, say, a
//! certificate extension is untouched, since it is not a bare tab character in the
//! decoded text at a line boundary) and is exercised by this crate's own unit tests
//! against both shapes, never against a live nginx process.
/// The gRPC metadata header a service-owned nginx mTLS front is expected to set on every
/// request it proxies to this service, carrying nginx's own `$ssl_client_escaped_cert`
/// value verbatim. See this module's own doc for the escaping contract and what was
/// actually observed versus documented rather than guessed.
///
/// `crates/av-ingest-client` names this exact same literal (its own module doc explains
/// why that crate cannot depend on this one without a build-graph cycle, since
/// `av-ingest`'s own tests depend on `av-ingest-client`) -- this constant, here, is the
/// canonical definition; a test in both crates asserts the two literals still agree.
pub const FORWARDED_CLIENT_CERT_HEADER: &str = "x-ssl-client-escaped-cert";

/// Un-escapes one forwarded-certificate header value into the raw bytes it encodes -- see
/// this module's own doc for exactly which nginx variable this decodes and why. `None`
/// means the header's own percent-encoding was malformed (an incomplete or non-hex `%`
/// escape) -- a pure function with no counter of its own: `crate::service::
/// EdgeIngestService::announce` is what turns a `None` here into a typed, counted
/// `MANIFEST_REFUSAL_IDENTITY_REFUSED` (a transport-layer defect distinct from anything
/// `av_edge::identity::IdentityCounters` describes, since `verify_identity` is never even
/// reached). An earlier draft of this function kept its own process-wide `AtomicU64`
/// counter here directly; removed after `cargo test`'s default parallelism made two
/// `#[test]` functions in this same module race on that one shared counter (`cargo test
/// -p av-ingest --lib` failed intermittently on exactly that interleaving) -- a pure
/// decode function has no business owning mutable global state, and the one caller that
/// cares about counting a refusal already has its own typed, per-call place to put it.
///
/// Percent-decoding is the primary, documented path: `escaped.as_bytes()` is scanned once,
/// left to right, copying every byte through unchanged except a `%` followed by two ASCII
/// hex digits, which becomes that one decoded byte. A trailing tab-per-line artefact (see
/// the module doc's "What was actually checked" section) is then folded back to a bare
/// newline as a defensive second pass, so a header value from either
/// `$ssl_client_escaped_cert` (percent-encoded) or a misconfigured front's
/// `$ssl_client_cert` (tab-per-line) both parse as ordinary PEM text.
pub fn percent_decode(escaped: &str) -> Option<Vec<u8>> {
    let bytes = escaped.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hi = bytes.get(i + 1).copied();
            let lo = bytes.get(i + 2).copied();
            let (Some(hi), Some(lo)) = (hi, lo) else {
                return None;
            };
            let (Some(hi), Some(lo)) = ((hi as char).to_digit(16), (lo as char).to_digit(16)) else {
                return None;
            };
            out.push(((hi << 4) | lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    fold_tab_per_line_artifact(&mut out);
    Some(out)
}

/// The defensive second pass documented in [`percent_decode`]'s own doc comment: every
/// `\t` byte that immediately follows a `\n` (the exact shape `$ssl_client_cert`'s
/// documented "each line except the first prepended with the tab character" produces,
/// nginx's own line terminator being `\n`, not `\r\n`, in every PEM this platform emits or
/// parses -- `openssl`'s own PEM writers agree) is removed in place. A tab that is not
/// immediately preceded by a newline is left alone -- it is either inside a
/// `$ssl_client_escaped_cert` value's own already-decoded content (which this function has
/// no reason to alter) or not part of this artefact's documented shape at all.
fn fold_tab_per_line_artifact(bytes: &mut Vec<u8>) {
    if !bytes.contains(&b'\t') {
        return; // the common, documented $ssl_client_escaped_cert path: nothing to fold.
    }
    let mut out = Vec::with_capacity(bytes.len());
    let mut prev_was_newline = false;
    for &b in bytes.iter() {
        if b == b'\t' && prev_was_newline {
            prev_was_newline = false;
            continue;
        }
        prev_was_newline = b == b'\n';
        out.push(b);
    }
    *bytes = out;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decode_round_trips_a_url_encoded_pem() {
        let pem = "-----BEGIN CERTIFICATE-----\nMII=\n-----END CERTIFICATE-----\n";
        let escaped: String = pem
            .bytes()
            .map(|b| {
                if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                    (b as char).to_string()
                } else {
                    format!("%{b:02X}")
                }
            })
            .collect();
        assert_ne!(escaped, pem, "the test fixture must actually contain escaped bytes");
        let decoded = percent_decode(&escaped).expect("well-formed percent-encoding must decode");
        assert_eq!(decoded, pem.as_bytes());
    }

    #[test]
    fn percent_decode_refuses_a_truncated_or_non_hex_escape() {
        assert_eq!(percent_decode("abc%4"), None);
        assert_eq!(percent_decode("abc%zz"), None);
    }

    #[test]
    fn percent_decode_folds_the_deprecated_tab_per_line_artifact() {
        // $ssl_client_cert's own documented shape: no percent-encoding at all, but every
        // line after the first is prefixed with a literal tab instead of nginx emitting a
        // real newline-continuation -- so the *header value itself* (never percent-
        // escaped) contains "\n\t" pairs.
        let tab_per_line = "-----BEGIN CERTIFICATE-----\n\tMII=\n\t-----END CERTIFICATE-----\n";
        let decoded = percent_decode(tab_per_line).unwrap();
        assert_eq!(decoded, b"-----BEGIN CERTIFICATE-----\nMII=\n-----END CERTIFICATE-----\n");
    }

    #[test]
    fn percent_decode_leaves_an_unrelated_tab_alone() {
        let decoded = percent_decode("a\tb").unwrap();
        assert_eq!(decoded, b"a\tb", "a tab not preceded by a newline is not this artefact");
    }
}
