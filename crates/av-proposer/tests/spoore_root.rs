//! Question 219(b): `resolve_spoore_root` (`crates/av-proposer/spoore_root.rs`, `include!`d by
//! `build.rs` and, here, `#[path]`-included directly so this test exercises the identical
//! source text `build.rs` compiles) against a temp symlink layout this test builds itself --
//! never against `/Users/probe/code/spoore` by name. The real spoore checkout this host
//! happens to have is found relative to THIS test's own `CARGO_MANIFEST_DIR`
//! (`<CARGO_MANIFEST_DIR>/../../../spoore`, i.e. this repo's own `../spoore` sibling) and
//! symlinked into each temp layout -- the same sibling-checkout arrangement question 219(b)
//! describes, proven here with no absolute host path spelled out anywhere in this file.
//!
//! No `std::env::set_var` anywhere (question 199): `resolve_spoore_root` takes its
//! `SPOORE_ROOT` value as a plain `Option<String>` parameter, so a test never needs to touch
//! the process environment to exercise either branch.

#[path = "../spoore_root.rs"]
mod spoore_root;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use spoore_root::resolve_spoore_root;

/// A uniquely-named temp directory, recursively removed on `Drop` (success or panic) so a
/// failing assertion still leaves no litter behind -- built from `std::env::temp_dir()`, this
/// process's pid, a per-process counter, and a caller-given label; never `tempfile` (not a
/// dependency this task is allowed to add) and never a path inside this worktree.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "av-proposer-spoore-root-test-{}-{label}-{n}",
            std::process::id(),
        ));
        std::fs::create_dir_all(&path)
            .unwrap_or_else(|e| panic!("creating temp dir {path:?}: {e}"));
        Self { path }
    }

    fn join(&self, rel: &str) -> PathBuf {
        self.path.join(rel)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// The real spoore checkout this host has, found relative to THIS test binary's own
/// `CARGO_MANIFEST_DIR` (`crates/av-proposer`) rather than any hardcoded absolute path --
/// `<CARGO_MANIFEST_DIR>/../../../spoore` is exactly this repo's own `../spoore` sibling
/// (`crates/av-proposer` -> `crates` -> `<repo>` -> `<repo>`'s parent, then `spoore`), the same
/// checkout `build.rs`'s own default would resolve to on this host.
fn real_spoore_root() -> PathBuf {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .unwrap_or_else(|| panic!("{manifest_dir:?} has fewer than three parent directories"))
        .join("spoore");
    assert!(
        root.join("proto/spoore/v0/model_service.proto").is_file(),
        "this test needs a real spoore checkout at {root:?} (this repo's own ../spoore \
         sibling) to symlink into its temp layouts -- none found"
    );
    root
}

/// Builds `<tmp>/fake-workspace/crates/av-proposer/` (the manifest dir a test passes to
/// `resolve_spoore_root`) and returns it.
fn fake_workspace(tmp: &TempDir) -> PathBuf {
    let manifest_dir = tmp.join("fake-workspace/crates/av-proposer");
    std::fs::create_dir_all(&manifest_dir)
        .unwrap_or_else(|e| panic!("creating {manifest_dir:?}: {e}"));
    manifest_dir
}

#[test]
fn default_resolves_through_the_sibling_symlink() {
    let tmp = TempDir::new("default-ok");
    let manifest_dir = fake_workspace(&tmp);
    let sibling_spoore = tmp.join("spoore");
    std::os::unix::fs::symlink(real_spoore_root(), &sibling_spoore)
        .unwrap_or_else(|e| panic!("symlinking {sibling_spoore:?}: {e}"));

    let resolved = resolve_spoore_root(None, &manifest_dir)
        .unwrap_or_else(|e| panic!("expected the ../spoore default to resolve, got: {e}"));

    // `resolve_spoore_root` itself must not canonicalize (a symlinked root has to work as-is);
    // this assertion is free to, purely to confirm the un-canonicalized result really does
    // point through the symlink at the real checkout, not merely that `Ok` came back.
    assert_eq!(
        resolved.canonicalize().unwrap_or_else(|e| panic!("canonicalizing {resolved:?}: {e}")),
        sibling_spoore.canonicalize().unwrap_or_else(|e| panic!("canonicalizing {sibling_spoore:?}: {e}")),
    );
    assert!(resolved.join("proto/spoore/v0/model_service.proto").is_file());
}

#[test]
fn default_with_no_sibling_spoore_is_a_typed_error_naming_the_default_path_and_the_variable() {
    let tmp = TempDir::new("default-missing");
    let manifest_dir = fake_workspace(&tmp);
    // Deliberately no `<tmp>/spoore` at all.

    let err = resolve_spoore_root(None, &manifest_dir)
        .expect_err("expected an Err with no sibling spoore checkout present");

    assert!(err.contains("SPOORE_ROOT"), "error does not mention SPOORE_ROOT: {err}");
    // Not merely `err.contains("spoore")` -- the message's own literal text already carries
    // that word, so such an assertion could never tell a correctly-resolved default path from
    // a wrong one. This names the exact path `resolve_spoore_root` must have computed from
    // `manifest_dir` (its parent's parent, then `../spoore`), so a resolver that walked the
    // wrong number of directories up would fail here instead of passing by coincidence.
    let expected_default = manifest_dir.parent().and_then(Path::parent).expect("fake workspace has two parents").join("..").join("spoore");
    let expected_str = expected_default.to_str().expect("temp path is valid UTF-8");
    assert!(err.contains(expected_str), "error does not name the resolved default path {expected_str:?}: {err}");
}

#[test]
fn explicit_spoore_root_wins_over_a_sibling_that_also_exists() {
    let tmp = TempDir::new("override-wins");
    let manifest_dir = fake_workspace(&tmp);

    // The sibling default exists too, but is NOT a real spoore checkout (missing the marker
    // proto) -- distinguishable from the good, explicitly-pointed-at one below, so a test that
    // returned the wrong one would fail loudly rather than by coincidence matching.
    let bad_sibling = tmp.join("spoore");
    std::fs::create_dir_all(&bad_sibling)
        .unwrap_or_else(|e| panic!("creating {bad_sibling:?}: {e}"));

    let good_spoore = tmp.join("good-spoore");
    std::os::unix::fs::symlink(real_spoore_root(), &good_spoore)
        .unwrap_or_else(|e| panic!("symlinking {good_spoore:?}: {e}"));

    let resolved = resolve_spoore_root(
        Some(good_spoore.to_str().expect("temp path is valid UTF-8").to_string()),
        &manifest_dir,
    )
    .unwrap_or_else(|e| panic!("expected the explicit SPOORE_ROOT override to resolve, got: {e}"));

    assert_eq!(
        resolved.canonicalize().unwrap_or_else(|e| panic!("canonicalizing {resolved:?}: {e}")),
        good_spoore.canonicalize().unwrap_or_else(|e| panic!("canonicalizing {good_spoore:?}: {e}")),
    );
    assert_ne!(resolved, bad_sibling, "must not have fallen back to the bad sibling default");
}

#[test]
fn empty_spoore_root_is_a_typed_error_naming_the_variable() {
    let tmp = TempDir::new("empty-var");
    let manifest_dir = fake_workspace(&tmp);

    let err = resolve_spoore_root(Some(String::new()), &manifest_dir)
        .expect_err("expected an Err for SPOORE_ROOT set but empty");

    assert!(err.contains("SPOORE_ROOT"), "error does not mention SPOORE_ROOT: {err}");
}

#[test]
fn nonexistent_spoore_root_is_a_typed_error_naming_that_path() {
    let tmp = TempDir::new("nonexistent");
    let manifest_dir = fake_workspace(&tmp);
    let missing = tmp.join("does-not-exist-here");

    let err = resolve_spoore_root(
        Some(missing.to_str().expect("temp path is valid UTF-8").to_string()),
        &manifest_dir,
    )
    .expect_err("expected an Err for a SPOORE_ROOT that does not exist");

    let missing_str = missing.to_str().expect("temp path is valid UTF-8");
    assert!(err.contains(missing_str), "error does not name the missing path {missing_str:?}: {err}");
    assert!(err.contains("SPOORE_ROOT"), "error does not mention SPOORE_ROOT: {err}");
}
