//! Emits the runtime library search path (`-rpath`) for GMAT's shared libraries, so this
//! crate's own `av-run` binary (and its unit tests) can find `libGmatBase`/`libGmatUtil` at
//! *run* time, not just link time.
//!
//! `gmat-sys`'s own build script's `cargo:rustc-link-lib`/`cargo:rustc-link-search` already
//! reach this crate's targets (Cargo propagates those transitively for any package that
//! depends, directly or transitively, on a crate declaring `links = "gmatffi"`, which is how
//! linking succeeds at all) -- but `cargo:rustc-link-arg` (which `-rpath` has to be passed as)
//! is explicitly **not** propagated across a package boundary by Cargo, so `gmat-sys`'s own
//! rpath only reaches `gmat-sys`'s own targets, never a downstream crate's. `crates/av-kernel/
//! build.rs` and `crates/av-dynamics-service/build.rs` already found and documented this exact
//! gap for their own targets; this is the identical fix (same default-path logic, kept in sync
//! by hand since build scripts cannot share code across packages) for `av-run`'s own binary,
//! which links `gmat-sys` directly (`Cargo.toml`: not a dev-dependency here -- this binary's
//! whole job is to construct one live `Gmat` handle and drive a DRM run through it).
use std::env;
use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let repo = manifest.parent().and_then(|p| p.parent()).expect("crate lives at <repo>/crates/av-run").to_path_buf();
    let gmat_root = env::var("GMAT_ROOT").map(PathBuf::from).unwrap_or_else(|_| repo.join("GMAT R2026a"));
    let gmat_lib = env::var("GMAT_LIB")
        .map(PathBuf::from)
        .unwrap_or_else(|_| gmat_root.join("bin").join("GMAT-R2026a_Beta.app").join("Contents").join("Frameworks"));

    println!("cargo:rerun-if-env-changed=GMAT_ROOT");
    println!("cargo:rerun-if-env-changed=GMAT_LIB");

    if gmat_lib.is_dir() {
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", gmat_lib.display());
    }
}
