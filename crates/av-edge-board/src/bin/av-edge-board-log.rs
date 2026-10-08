//! `av-edge-board-log`: verify a board I/O log and print what it holds (question 242 (a)).
//!
//! ```text
//! av-edge-board-log <log> --cert <pem> [--records]
//! ```
//!
//! `--cert` is the PEM certificate of the signer (every record must name its fingerprint and
//! verify against its key) or a bare P-384 public key PEM (signatures only). The file is read
//! and never modified. Output is `key: value` lines on stdout:
//!
//! ```text
//! log: <path>
//! verification: OK | OK, TORN TAIL | FAILED: <typed error with the record index>
//! records / binds / steps / resets / shutdowns / power_cycles / failed_exchanges: <n>
//! first_epoch_tai_ns / last_epoch_tai_ns: <STEP until_tai_ns bounds>
//! first_request_written_unix_ns / last_response_read_unix_ns
//! producer / signer_cert_sha256 / link_config_sha256 / run_ids
//! chain_head: <hex of the last record_hash, or GENESIS>
//! bytes_verified: <n>
//! torn_tail: offset=<n> discarded_bytes=<n> <kind>      (only if the last frame is torn)
//! ```
//!
//! `--records` adds one `record ...` line per record. Exit status: 0 verified, 3 verified but
//! the last frame is torn (the records before it are intact), 1 verification failed or the
//! file could not be read, 2 usage.
use std::path::PathBuf;
use std::process::ExitCode;

use av_edge::board_log::{read_log, BoardIoKind, LogVerifier, TornKind};

fn usage() -> ExitCode {
    eprintln!("usage: av-edge-board-log <log> --cert <pem> [--records]");
    ExitCode::from(2)
}

fn opt<T: std::fmt::Display>(v: Option<T>) -> String {
    v.map(|x| x.to_string()).unwrap_or_else(|| "-".to_string())
}

fn main() -> ExitCode {
    let mut log = None;
    let mut cert = None;
    let mut records = false;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--cert" => match it.next() {
                Some(v) => cert = Some(PathBuf::from(v)),
                None => return usage(),
            },
            "--records" => records = true,
            "--help" | "-h" => return usage(),
            other if other.starts_with("--") => return usage(),
            other => {
                if log.replace(PathBuf::from(other)).is_some() {
                    return usage();
                }
            }
        }
    }
    let (Some(log), Some(cert)) = (log, cert) else { return usage() };

    println!("log: {}", log.display());
    let verifier = match std::fs::read(&cert).map_err(|e| e.to_string()).and_then(|pem| LogVerifier::from_pem(&pem).map_err(|e| e.to_string())) {
        Ok(v) => v,
        Err(e) => {
            println!("verification: FAILED: cannot use {} to verify: {e}", cert.display());
            return ExitCode::from(1);
        }
    };
    let verified = match read_log(&log, &verifier) {
        Ok(v) => v,
        Err(e) => {
            println!("verification: FAILED: {e}");
            return ExitCode::from(1);
        }
    };
    println!("verification: {}", if verified.recovery.is_some() { "OK, TORN TAIL" } else { "OK" });
    let s = verified.summary();
    println!("records: {}", s.records);
    println!("binds: {}", s.binds);
    println!("steps: {}", s.steps);
    println!("resets: {}", s.resets);
    println!("shutdowns: {}", s.shutdowns);
    println!("power_cycles: {}", s.power_cycles);
    println!("failed_exchanges: {}", s.failed_exchanges);
    println!("first_epoch_tai_ns: {}", opt(s.first_epoch_tai_ns));
    println!("last_epoch_tai_ns: {}", opt(s.last_epoch_tai_ns));
    println!("first_request_written_unix_ns: {}", opt(s.first_request_written_unix_ns));
    println!("last_response_read_unix_ns: {}", opt(s.last_response_read_unix_ns));
    println!("producer: {}", s.producer_id);
    println!("signer_cert_sha256: {}", s.signer_cert_sha256);
    println!("link_config_sha256: {}", s.link_config_sha256);
    println!("run_ids: {}", s.run_ids.iter().cloned().collect::<Vec<_>>().join(","));
    println!("chain_head: {}", s.chain_head_hex);
    println!("bytes_verified: {}", s.bytes_verified);
    if let Some(t) = &verified.recovery {
        let kind = match &t.kind {
            TornKind::IncompleteHeader { have } => format!("incomplete header ({have} of 36 bytes)"),
            TornKind::IncompletePayload { declared, have } => format!("incomplete payload ({have} of {declared} bytes)"),
        };
        println!("torn_tail: offset={} discarded_bytes={} {kind}", t.offset, t.discarded_bytes);
    }
    if records {
        for r in &verified.records {
            let kind = BoardIoKind::try_from(r.kind).map(|k| k.as_str_name().trim_start_matches("BOARD_IO_KIND_").to_string()).unwrap_or_else(|_| format!("?{}", r.kind));
            let in_bytes: usize = r.inputs.iter().map(|m| m.payload.len()).sum();
            let out_bytes: usize = r.outputs.iter().map(|m| m.payload.len()).sum();
            println!(
                "record seq={} kind={kind} run={} instance={} lockstep_seq={} until_tai_ns={} input_bytes={in_bytes} output_bytes={out_bytes} written_unix_ns={} read_unix_ns={} hash={} error={:?}",
                r.sequence,
                r.run_id,
                r.instance,
                r.lockstep_sequence,
                r.until_tai_ns,
                r.request_written_unix_ns,
                r.response_read_unix_ns,
                av_edge::hash::hex_encode(&r.record_hash[..8]),
                r.error
            );
        }
    }
    ExitCode::from(if verified.recovery.is_some() { 3 } else { 0 })
}
