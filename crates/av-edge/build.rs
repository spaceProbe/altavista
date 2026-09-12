//! Emits `-rpath` for `libGmatBase`/`libGmatUtil` on **this crate's own test targets
//! only** (`cargo:rustc-link-arg-tests`) -- a fix for a real linking defect this task
//! discovered directly while building `crates/av-edge/tests/plugin_replay.rs` (E4a's
//! plugin, `src/plugin.rs`; that test cross-checks `crate::plugin::packet` against
//! `av_kernel::codec`, a **dev-dependency only** -- see `src/plugin/packet.rs`'s own
//! module doc for why `av-edge` cannot depend on `av-kernel` for real).
//!
//! # The defect, root-caused
//!
//! `crates/gmat-sys`'s own build script (`links = "gmatffi"`) emits a plain
//! `cargo:rustc-link-arg=-Wl,-rpath,<GMAT_LIB>`. That instruction reaches every binary
//! that depends on `gmat-sys` through an **ordinary** dependency edge -- confirmed
//! directly: `av-kernel`'s own test binaries carry the resulting `LC_RPATH` load command
//! (`otool -l` on a built `av-kernel` test binary shows it) and run under plain `cargo
//! test` with no extra configuration. It does **not**, however, cross a **dev**-
//! dependency edge: a test binary built for `av-edge` (which reaches `gmat-sys` only via
//! `[dev-dependencies] av-kernel`) links `libGmatBase`/`libGmatUtil` (symbols pulled in
//! transitively through `av_kernel::codec`) but carries **no** `LC_RPATH` entry at all --
//! confirmed the identical way, `otool -l` on the built `av-edge` test binary shows
//! nothing -- so it aborted at process start with `dyld: Library not loaded: @rpath/
//! libGmatBase.R2026a.dylib` the first time `cargo test -p av-edge` was run with the
//! `av-kernel` dev-dependency in place.
//!
//! `DYLD_LIBRARY_PATH` cannot work around this either, on this host: `GMAT_ROOT` contains
//! a literal space (`"GMAT R2026a"`), and -- independently of that -- Cargo scrubs
//! `DYLD_LIBRARY_PATH`/`LD_LIBRARY_PATH` from the environment of every subprocess it
//! spawns (rustc and test binaries alike): confirmed directly, `export
//! DYLD_LIBRARY_PATH=...` followed by `env | grep DYLD` inside the very shell that then
//! runs `cargo test` shows the variable is gone by the time the test binary starts, while
//! the identical variable set on a **direct**, non-Cargo invocation of that same compiled
//! test binary works. Plain `RUSTFLAGS` has the same "path contains a space" problem
//! `cargo:rustc-link-arg`/`-tests` do not: Cargo whitespace-splits `RUSTFLAGS`'s own value
//! before handing it to rustc, but a build script's `cargo:KEY=VALUE` output line is
//! parsed as one opaque value with no further splitting (`gmat-sys`'s own build script
//! output proves this: its rpath value already contains the identical space and already
//! works for `av-kernel`'s own tests).
//!
//! # The fix
//!
//! `cargo:rustc-link-arg-tests` (unlike plain `cargo:rustc-link-arg`) is scoped to targets
//! **this crate's own** `Cargo.toml` declares (its `tests/*.rs` integration tests and
//! `#[cfg(test)]` unit tests) -- it does not depend on propagating anything across a
//! dev-dependency edge at all, so it is immune to the exact gap above. This is `av-edge`'s
//! own build script solving `av-edge`'s own test-linking problem, the same way
//! `gmat-sys`'s own build script already solves it for `gmat-sys`'s own ordinary
//! dependents.
//!
//! Silent (emits no rpath at all) when the resolved GMAT lib directory is not a real
//! directory on this host -- e.g. a plain `cargo build -p av-edge` (this crate's
//! production build never needs GMAT at all; only its own tests, via the `av-kernel`
//! dev-dependency, do) on a host with no GMAT install. This never turns a missing GMAT
//! install into a build failure for the one target that never needed it.
use std::env;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("Cargo always sets CARGO_MANIFEST_DIR for a build script"));
    manifest.parent().expect("crates/av-edge").parent().expect("crates").to_path_buf()
}

fn main() {
    // Mirrors crates/gmat-sys/build.rs's own GMAT_ROOT/GMAT_LIB resolution exactly, so the
    // two build scripts agree on where the dylibs live without this one depending on that
    // one's own internals.
    println!("cargo:rerun-if-env-changed=GMAT_ROOT");
    println!("cargo:rerun-if-env-changed=GMAT_LIB");
    let gmat_root = env::var("GMAT_ROOT").map(PathBuf::from).unwrap_or_else(|_| repo_root().join("GMAT R2026a"));
    let gmat_lib = env::var("GMAT_LIB").map(PathBuf::from).unwrap_or_else(|_| gmat_root.join("bin").join("GMAT-R2026a_Beta.app").join("Contents").join("Frameworks"));
    if gmat_lib.is_dir() {
        println!("cargo:rustc-link-arg-tests=-Wl,-rpath,{}", gmat_lib.display());
    }
}
