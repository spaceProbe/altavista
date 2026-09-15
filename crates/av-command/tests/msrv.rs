//! Question 208(a): guards against the workspace's declared `rust-version` silently going
//! stale again the way it already did once (the root `Cargo.toml` said `"1.85"` from round 1
//! through round 4 while `regorus 0.12.0` -- pinned in `Cargo.lock` -- actually needed 1.87 to
//! compile; see the comment beside `rust-version` in the root `Cargo.toml` for the full
//! binary-search evidence: `cargo +1.86.0 check -p av-command` fails on regorus's
//! `const_vec_string_slice` need, `cargo +1.87.0 check -p av-command` (and `--workspace`)
//! passes).
//!
//! `CARGO_PKG_RUST_VERSION` is not hand-duplicated anywhere: Cargo itself sets it, at build
//! time, from this crate's own `rust-version.workspace = true`
//! (`crates/av-command/Cargo.toml`), which in turn comes straight from the root `Cargo.toml`'s
//! `[workspace.package] rust-version`. Reading it back here needs no network and no extra
//! toolchain install at test time -- it runs under whatever toolchain is already building the
//! test binary -- so it stays honest under question 154's "no network at test time" rule.
//!
//! What this test can and cannot prove: it cannot prove the crate actually COMPILES on the
//! declared floor (that needs the floor's own toolchain, installed once as documented,
//! measured setup -- never at test time). What it DOES prove is that nobody has bumped
//! `regorus`, edited the root `Cargo.toml`'s `rust-version`, or otherwise moved the real floor
//! without also updating this test's own `MEASURED_FLOOR` constant -- i.e. it catches the
//! declared value drifting away from the last value a human actually measured, exactly the
//! failure mode question 208(a) found.

#[test]
fn declared_rust_version_matches_the_last_measured_floor() {
    // Keep this in lockstep with `rust-version` in the root Cargo.toml AND with the
    // measurement comment beside it -- if you are changing one, you are changing all three,
    // and only after re-running the `cargo +<version> check -p av-command` binary search.
    const MEASURED_FLOOR: &str = "1.87";

    let declared = env!("CARGO_PKG_RUST_VERSION");
    assert_eq!(
        declared, MEASURED_FLOOR,
        "root Cargo.toml's `rust-version` ({declared}) no longer matches the last measured \
         floor ({MEASURED_FLOOR}) recorded beside it -- re-measure with \
         `cargo +<version> check -p av-command` (then `--workspace`) per question 208(a) \
         before changing either the Cargo.toml comment or this constant alone."
    );
}
