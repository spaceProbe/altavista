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

/// Question 254 item 1: the shard-key check is the shared path-component rule, so it refuses
/// every id in the shared vectors (`data/path_component_id_vectors.json`, the same file the rule's
/// own Rust and Python tests read) with the rule's recorded reason, and accepts every accepted one.
#[test]
fn sanitize_shard_key_follows_the_shared_path_component_vectors() {
    let vectors: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/path_component_id_vectors.json")).unwrap()).unwrap();
    for id in vectors["accepted"].as_array().unwrap() {
        let id = id.as_str().unwrap();
        assert_eq!(sanitize_shard_key(id).unwrap(), format!("{id}.avlog"), "{id:?}");
    }
    for entry in vectors["refused"].as_array().unwrap() {
        let id = entry["id"].as_str().unwrap();
        match sanitize_shard_key(id) {
            Err(LogError::InvalidShardKey { reason, .. }) => assert_eq!(reason.reason(), entry["reason"].as_str().unwrap(), "{id:?}"),
            other => panic!("{id:?} must be refused as InvalidShardKey, got {other:?}"),
        }
    }
}

/// The stricter cases the pre-254 check let through (a control character, a leading dot, a space,
/// `%`, non-ASCII) are refused by `PartitionLog::open` too, with nothing created on disk.
#[test]
fn partition_log_open_refuses_the_newly_refused_keys_and_creates_nothing() {
    for key in [".hidden", "a\nb", "a b", "a%2fb", "caf\u{e9}", "-x"] {
        let dir = tmp_dir("newly-refused");
        let result = PartitionLog::open(&dir, key);
        assert!(matches!(result, Err(LogError::InvalidShardKey { .. })), "{key:?}: {result:?}");
        assert!(!dir.exists(), "{key:?}: the log directory must not be created for a rejected key");
    }
}
