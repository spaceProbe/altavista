//! Question 216(c): the mechanical claim-check proof -- no hot-path crate depends on
//! `av-store`, checked against the real dependency graph with a real `cargo tree`
//! invocation, not by reading `Cargo.toml` files by eye.
//!
//! `docs/heavy-plan.md` H1 names the hot path explicitly: "`av-ingest`, `av-track`,
//! `av-command`". [`HOT_PATH_CRATES`] below is asserted non-empty by
//! [`hot_path_crate_list_is_not_empty`] so a future edit that accidentally clears the list
//! (making every other assertion in this file vacuously true) fails loudly instead of
//! silently stopping to prove anything.
//!
//! This is a docker-free test (this task's whole crate is): it shells out to the same
//! `cargo` binary already building this workspace, never touches a network (`--offline`),
//! and never touches Docker at all.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// `docs/heavy-plan.md` H1's own acceptance line names exactly these three crates as "the
/// hot path". Asserted non-empty by [`hot_path_crate_list_is_not_empty`] -- see this file's
/// own module doc.
const HOT_PATH_CRATES: &[&str] = &["av-ingest", "av-track", "av-command"];

/// This crate's own manifest directory is `<workspace-root>/crates/av-store`; the workspace
/// root is two `parent()` calls up. Never a hard-coded absolute path (this task's rule 9: a
/// test must pass in a plain clone of this repo) -- `env!("CARGO_MANIFEST_DIR")` is filled in
/// by `cargo` itself at compile time from wherever this crate actually lives on disk.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/av-store lives at <workspace-root>/crates/av-store")
        .to_path_buf()
}

/// The `cargo` binary to invoke. `cargo` sets `CARGO` in every test binary's environment to
/// the exact binary that built it (documented cargo behaviour) -- using that, rather than the
/// bare literal `"cargo"`, means this test exercises the same toolchain (and the same
/// `rustup` shim resolution) actually building this workspace, not whatever `cargo` happens
/// to resolve first on `$PATH` in some other shell. Falls back to the literal `"cargo"` only
/// when `CARGO` is genuinely unset (e.g. this binary invoked by hand, outside `cargo test`),
/// and says so in the panic message of any assertion that then fails, per this file's own
/// rule: never skip, never silently substitute.
fn cargo_bin() -> (PathBuf, bool) {
    match std::env::var("CARGO") {
        Ok(path) => (PathBuf::from(path), true),
        Err(_) => (PathBuf::from("cargo"), false),
    }
}

/// Runs `cargo tree -p <crate> -e normal --prefix none --no-dedupe --offline` from the
/// workspace root and returns its captured output. **Never skips and never treats a failed
/// invocation as a pass**: a `cargo tree` that itself fails to run (bad crate name, a locked
/// package cache, cargo not found) panics with the full stdout+stderr, per this file's own
/// requirement -- there is no code path here that returns something this test's callers could
/// mistake for "no dependency found".
///
/// `-e normal` is load-bearing, not decoration: it restricts the printed graph to *normal*
/// (runtime) dependency edges, excluding `dev-dependencies` entirely. Without it, a
/// hot-path crate's own `[dev-dependencies]` -- which may reach `av-store` someday for a
/// perfectly legitimate reason (an integration test fixture, say) with no architectural
/// problem at all, since a `dev-dependency` never ships in the built binary and never runs on
/// the hot path in production -- would make this test fail on an edge that was never the
/// thing question 216(c) is actually guarding against. `--no-dedupe` prints every edge in the
/// graph rather than collapsing repeated subtrees, which matters here because a collapsed
/// subtree can hide an edge this test needs to see if `av-store` also happened to appear,
/// deduplicated away, somewhere else in the same tree. `--offline`: this task's rule 4 (no
/// network at test time) -- every crate `cargo tree` needs to read is already fetched.
fn cargo_tree(crate_name: &str) -> Output {
    let (cargo, cargo_env_was_set) = cargo_bin();
    let output = Command::new(&cargo)
        .current_dir(workspace_root())
        .args(["tree", "-p", crate_name, "-e", "normal", "--prefix", "none", "--no-dedupe", "--offline"])
        .output();

    let output = match output {
        Ok(o) => o,
        Err(e) => panic!(
            "cargo tree -p {crate_name} could not even be spawned (cargo binary: {} ({}); workspace root: {}): {e}",
            cargo.display(),
            if cargo_env_was_set { "from $CARGO" } else { "$CARGO was unset, fell back to the literal \"cargo\" on $PATH" },
            workspace_root().display(),
        ),
    };

    if !output.status.success() {
        // Cargo's own package-cache lock (`~/.cargo/.package-cache`) is held only while cargo
        // itself is resolving/fetching, and `--offline` means this invocation never fetches
        // anything -- so a lock-contention failure here would show up as cargo's own "waiting
        // for file lock" message inside stderr below (this task's own report records whether
        // that was ever actually observed during this task's run; it was not).
        panic!(
            "cargo tree -p {crate_name} -e normal --prefix none --no-dedupe --offline exited with {}.\n--- stdout ---\n{}\n--- stderr ---\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
    output
}

#[test]
fn hot_path_crate_list_is_not_empty() {
    assert!(!HOT_PATH_CRATES.is_empty(), "HOT_PATH_CRATES must never be emptied -- every other test in this file would otherwise vacuously pass");
}

/// The actual claim-check proof: for every named hot-path crate, `av-store` appears nowhere
/// in its normal (runtime) dependency tree.
#[test]
fn no_hot_path_crate_depends_on_av_store() {
    for crate_name in HOT_PATH_CRATES {
        let output = cargo_tree(crate_name);
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            !stdout.contains("av-store"),
            "cargo tree -p {crate_name} -e normal must never mention av-store (the claim-check's whole point: the hot path never dereferences one), but it does:\n{stdout}"
        );
    }
}

/// The self-check this file's own doc promises: `av-store`'s own tree really does contain
/// `av-cdm` (a real, expected dependency -- see `crates/av-store/Cargo.toml`), so
/// [`no_hot_path_crate_depends_on_av_store`] passing is proof the string it looked for really
/// would have been found had it been there, not an artifact of `cargo tree` silently printing
/// nothing (a locked package cache, an unresolved workspace, or a typo'd crate name would all
/// otherwise make that test pass for the wrong reason).
#[test]
fn av_store_own_tree_contains_av_cdm_proving_this_test_can_detect_a_real_dependency() {
    let output = cargo_tree("av-store");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("av-cdm"), "cargo tree -p av-store -e normal must contain av-cdm (a real, declared dependency); got:\n{stdout}");
}
