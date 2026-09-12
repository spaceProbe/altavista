//! `av-edge-identity` -- the small CLI bridge between Python integration tests and E2's
//! [`av_edge::identity`] (`docs/edge-plan.md` milestone E2, "the bridge from Python to
//! your Rust code is a small CLI binary taking command-line arguments, never environment
//! variables -- question 199 forbids tests mutating the process environment").
//!
//! ```text
//! av-edge-identity --ca <root.pem> [--chain <issuer.pem>] --leaf <leaf.pem> \
//!     --now-tai-ns <i64> [--verify-batch <batch.pb>]
//! ```
//!
//! Prints one line of hand-rolled JSON (no `serde_json` -- this crate adds no new
//! dependency for this binary) to stdout, and exits non-zero on any refusal: the leaf
//! being rejected, or (when `--verify-batch` was given) the batch failing to verify under
//! the accepted identity. `--verify-batch` takes a path to a `MeasurementBatch` protobuf
//! message encoded with `prost::Message::encode_to_vec` (a raw binary file, not a text
//! format) -- what `tests/test_edge_identity_seccert.py` produces with a small Rust test
//! helper or a committed fixture.
//!
//! This binary reads exactly the files its arguments name and nothing else: no
//! environment variable is ever consulted (`std::env::args` for the arguments
//! themselves is the one exception -- that is the argument vector, not the environment).

use std::process::ExitCode;

use av_edge::identity::{self, IdentityRejection, TrustAnchors};
use av_edge::pb;

struct Args {
    ca: String,
    chain: Option<String>,
    leaf: String,
    now_tai_ns: i64,
    verify_batch: Option<String>,
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let _argv0 = args.next();
    let mut ca = None;
    let mut chain = None;
    let mut leaf = None;
    let mut now_tai_ns = None;
    let mut verify_batch = None;

    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{flag} requires a value"));
        match flag.as_str() {
            "--ca" => ca = Some(value()?),
            "--chain" => chain = Some(value()?),
            "--leaf" => leaf = Some(value()?),
            "--now-tai-ns" => {
                let raw = value()?;
                now_tai_ns = Some(raw.parse::<i64>().map_err(|e| format!("--now-tai-ns {raw:?} is not a valid i64: {e}"))?);
            }
            "--verify-batch" => verify_batch = Some(value()?),
            other => return Err(format!("unrecognized argument: {other}")),
        }
    }

    Ok(Args {
        ca: ca.ok_or("--ca is required")?,
        chain,
        leaf: leaf.ok_or("--leaf is required")?,
        now_tai_ns: now_tai_ns.ok_or("--now-tai-ns is required")?,
        verify_batch,
    })
}

/// Minimal JSON string escaping -- this binary hand-rolls its one line of JSON output
/// with `format!` rather than adding `serde_json` to this crate; every string this
/// prints (a PEM error, a subject CN, a hex fingerprint) is escaped through this so a
/// stray `"` or control byte in, say, a subject CN never produces invalid JSON.
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn json_str(s: &str) -> String {
    format!("\"{}\"", json_escape(s))
}

fn json_str_opt(s: Option<&str>) -> String {
    match s {
        Some(s) => json_str(s),
        None => "null".to_string(),
    }
}

fn json_bool_opt(v: Option<bool>) -> String {
    match v {
        Some(true) => "true".to_string(),
        Some(false) => "false".to_string(),
        None => "null".to_string(),
    }
}

fn rejection_kind(r: &IdentityRejection) -> &'static str {
    match r {
        IdentityRejection::MalformedPem(_) => "MALFORMED_PEM",
        IdentityRejection::NotP384 { .. } => "NOT_P384",
        IdentityRejection::IssuerNotTrusted(_) => "ISSUER_NOT_TRUSTED",
        IdentityRejection::Expired { .. } => "EXPIRED",
        IdentityRejection::NotYetValid { .. } => "NOT_YET_VALID",
        IdentityRejection::Openssl(_) => "OPENSSL_ERROR",
    }
}

fn run() -> Result<bool, String> {
    let args = parse_args(std::env::args()).map_err(|e| format!("argument error: {e}"))?;

    let ca_pem = std::fs::read(&args.ca).map_err(|e| format!("reading --ca {:?}: {e}", args.ca))?;
    let leaf_pem = std::fs::read(&args.leaf).map_err(|e| format!("reading --leaf {:?}: {e}", args.leaf))?;
    let chain_pem = match &args.chain {
        Some(path) => Some(std::fs::read(path).map_err(|e| format!("reading --chain {path:?}: {e}"))?),
        None => None,
    };

    let anchors = TrustAnchors::from_root_pem(&ca_pem).map_err(|e| format!("loading --ca: {e}"))?;
    let mut counters = identity::IdentityCounters::new();
    let result = identity::verify_identity(&leaf_pem, chain_pem.as_deref(), &anchors, args.now_tai_ns, &mut counters);

    match result {
        Err(rejection) => {
            println!(
                "{{\"accepted\":false,\"rejection\":{},\"detail\":{},\"fingerprint_sha256\":null,\"subject_cn\":null,\"not_before_tai_ns\":null,\"not_after_tai_ns\":null,\"batch_verified\":null,\"batch_error\":null}}",
                json_str(rejection_kind(&rejection)),
                json_str(&rejection.to_string()),
            );
            Ok(false)
        }
        Ok(identity) => {
            let (batch_verified, batch_error) = match &args.verify_batch {
                None => (None, None),
                Some(path) => match std::fs::read(path) {
                    Err(e) => (Some(false), Some(format!("reading --verify-batch {path:?}: {e}"))),
                    Ok(bytes) => match <pb::MeasurementBatch as prost::Message>::decode(bytes.as_slice()) {
                        Err(e) => (Some(false), Some(format!("decoding --verify-batch {path:?} as a MeasurementBatch: {e}"))),
                        Ok(batch) => match identity::verify_batch_signed_by(&identity, &batch) {
                            Ok(_) => (Some(true), None),
                            Err(e) => (Some(false), Some(e.to_string())),
                        },
                    },
                },
            };

            println!(
                "{{\"accepted\":true,\"rejection\":null,\"detail\":null,\"fingerprint_sha256\":{},\"subject_cn\":{},\"not_before_tai_ns\":{},\"not_after_tai_ns\":{},\"batch_verified\":{},\"batch_error\":{}}}",
                json_str(&identity.fingerprint_sha256),
                json_str(&identity.subject_cn),
                identity.not_before_tai_ns,
                identity.not_after_tai_ns,
                json_bool_opt(batch_verified),
                json_str_opt(batch_error.as_deref()),
            );
            Ok(batch_verified.unwrap_or(true))
        }
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("av-edge-identity: {e}");
            ExitCode::FAILURE
        }
    }
}
