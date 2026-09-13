//! Tiny shared helper used by more than one of this crate's integration test binaries.
//! Kept deliberately small: a helper used by only one test file lives in that file directly
//! (e.g. `tests/propose_only.rs`'s own `CommandAuthorityHarness`) rather than here, so no
//! test binary compiles a helper it never calls and (with `-D warnings`) fails on an unused-
//! item warning for code genuinely private to a sibling binary.

pub fn tmp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("av-gateway-it-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}
