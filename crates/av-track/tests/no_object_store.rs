//! "Nothing on this path calls an object store" (E5's own exit test), asserted
//! structurally -- `cargo tree -p av-track` must never contain `spoore-io` (and therefore
//! never its `clickhouse_sink` module, an object-store/ClickHouse sink) or any of the
//! crypto-adjacent crates ADR-004's crypto rule bans (`ring`, `md-5`, `md5`, `sha1`,
//! `blake2`, `blake3`, `rustls`, `tokio-rustls`) -- restating `deny.toml`'s own ban list
//! for this one crate's tree directly, rather than trusting a workspace-wide `cargo deny
//! check` to have been run before this test does.
//!
//! Runs `cargo tree` as a subprocess (no network -- `cargo tree` reads only the already-
//! resolved `Cargo.lock`/local registry cache, question 154) and reads its own inherited
//! environment (`PATH`, already exported by whatever invoked `cargo test` per this task's
//! own setup) without ever mutating it (question 199).

use std::process::Command;

fn cargo_tree(args: &[&str]) -> String {
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir.parent().and_then(|p| p.parent()).expect("crates/av-track/.. /.. is the workspace root");
    let mut cmd = Command::new("cargo");
    cmd.arg("tree").args(args).current_dir(workspace_root);
    let output = cmd.output().expect("running cargo tree (is `cargo` on PATH?)");
    if !output.status.success() {
        panic!("cargo tree {args:?} failed (status {:?}):\nstdout:\n{}\nstderr:\n{}", output.status, String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    }
    String::from_utf8(output.stdout).expect("cargo tree output is UTF-8")
}

#[test]
fn av_track_does_not_depend_on_spoore_io() {
    let tree = cargo_tree(&["-p", "av-track"]);
    assert!(!tree.contains("spoore-io"), "av-track's dependency tree must never contain spoore-io (and therefore never its clickhouse_sink object-store sink):\n{tree}");
}

#[test]
fn av_track_does_not_depend_on_ring_or_any_other_denied_crypto_adjacent_crate() {
    let tree = cargo_tree(&["-p", "av-track", "-e", "normal,build"]);
    for banned in ["ring", "rustls", "tokio-rustls", "rustls-webpki", "webpki-roots"] {
        assert!(!tree.contains(banned), "av-track's dependency tree must never contain {banned:?} (ADR-004's crypto rule; deny.toml's own ban list):\n{tree}");
    }
    // md-5/sha1/blake2/blake3 are checked as whole-word crate names (cargo tree renders
    // each as its own line, `name vX.Y.Z`), not substrings -- "sha1" is, for instance, a
    // substring of nothing else in this tree, but this loop is written the same way for
    // all five so a future addition to the list needs no special-casing.
    for banned in ["md-5", "md5", "sha1", "blake2", "blake3"] {
        let is_present = tree.lines().any(|line| line.trim_start_matches(|c: char| "├└│─ ".contains(c)).starts_with(&format!("{banned} v")));
        assert!(!is_present, "av-track's dependency tree must never contain {banned:?} (ADR-004's crypto rule):\n{tree}");
    }
}

#[test]
fn av_track_declared_dependencies_are_exactly_what_this_crate_intends() {
    // `cargo tree -p av-track --depth 1` -- exactly this crate's own direct dependencies,
    // no more, no fewer, so an accidental new direct dependency (a stray `git commit` of a
    // half-finished experiment, say) is caught by name rather than only by its transitive
    // consequences.
    let tree = cargo_tree(&["-p", "av-track", "--depth", "1", "-e", "normal"]);
    for expected in ["av-edge", "av-cdm", "av-ingest", "av-ingest-client", "prost", "serde", "serde_json", "thiserror", "openssl", "toml", "spoore-cdm", "spoore-engine", "spoore-models", "spoore-assoc", "spoore-tree", "nalgebra", "tonic", "tokio", "tokio-stream", "futures-util"] {
        assert!(tree.contains(expected), "expected direct dependency {expected:?} not found in:\n{tree}");
    }
}
