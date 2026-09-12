//! Requirement 7: a `shard_key` containing `../` or a bare path separator is a typed
//! error ([`av_ingest::log::LogError::InvalidShardKey`]), and opening/appending with such
//! a key writes nothing outside the log directory -- checked directly against
//! `av_ingest::log`, independently of the higher-level `Ingest` pipeline (which applies
//! this same check as part of its own `SHARD_MISMATCH` gate -- see `tests/rejections.rs`
//! and `av_ingest::ingest`'s module doc).

use std::path::PathBuf;

use av_ingest::log::{sanitize_shard_key, LogError, PartitionLog};

fn tmp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("av-ingest-test-path-traversal-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// Every one of these, taken alone, must be refused: a literal `../`, a bare `..`, an
/// absolute-looking leading slash, a backslash (Windows-style separator, refused
/// regardless of host OS since this is a pure string check), a NUL byte, and the empty
/// string (which cannot name any file at all).
const MALICIOUS_KEYS: &[&str] = &["../secret", "..", "/etc/passwd", "shard/../../escaped", "back\\slash", "nul\0byte", ""];

#[test]
fn sanitize_shard_key_refuses_every_path_traversal_attempt_as_a_typed_error() {
    for key in MALICIOUS_KEYS {
        let result = sanitize_shard_key(key);
        assert!(matches!(result, Err(LogError::InvalidShardKey { .. })), "shard_key {key:?} must be refused as InvalidShardKey, got {result:?}");
    }
}

#[test]
fn partition_log_open_refuses_a_traversal_shard_key_and_writes_nothing_outside_the_log_directory() {
    // `dir`'s own parent is `std::env::temp_dir()`, a directory shared with every other
    // test in this process (and, on a shared host, other processes' own temp files) --
    // this test therefore checks only for the *specific* file a successful escape would
    // have produced, never a before/after snapshot of the whole shared parent (which
    // would be racy against unrelated concurrent tests creating their own temp entries).
    let dir = tmp_dir("open");
    let parent = dir.parent().expect("tmp_dir has a parent").to_path_buf();
    let escape_target = parent.join("escape-attempt.avlog");
    let _ = std::fs::remove_file(&escape_target); // defensive: this test owns this exact path

    let result = PartitionLog::open(&dir, "../escape-attempt");
    assert!(matches!(result, Err(LogError::InvalidShardKey { .. })), "{result:?}");

    assert!(!escape_target.exists(), "a traversal shard_key must never create a file outside the intended log directory");
    assert!(!dir.exists(), "the log directory itself must not even be created for a rejected shard_key");
}

#[test]
fn a_valid_shard_key_sanitizes_to_a_plain_file_name_confined_to_the_log_directory() {
    let sanitized = sanitize_shard_key("shard-a").expect("an ordinary shard_key must be accepted");
    assert_eq!(sanitized, "shard-a.avlog");
    assert!(!sanitized.contains('/') && !sanitized.contains('\\'), "a sanitized name must never itself contain a separator");
}
