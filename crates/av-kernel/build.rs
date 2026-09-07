//! Emits the runtime library search path (`-rpath`) for GMAT's shared libraries, when a GMAT
//! install is available, so `tests/golden_acceptance.rs` (which links `gmat-sys` as a
//! dev-dependency) can find `libGmatBase`/`libGmatUtil` at *run* time, not just link time.
//!
//! `gmat-sys`'s own build script's `cargo:rustc-link-lib` / `cargo:rustc-link-search` already
//! reach this package's test binary (Cargo propagates those transitively for any package that
//! declares `links = "gmatffi"`, which is how linking succeeds at all) -- but
//! `cargo:rustc-link-arg` (which `-rpath` has to be passed as) is explicitly **not**
//! propagated across a package boundary by Cargo; `gmat-sys`'s own rpath only reaches
//! `gmat-sys`'s own test/example binaries, never a downstream package's. This build script
//! re-emits the same rpath (same default-path logic as `crates/gmat-sys/build.rs`, kept in
//! sync by hand since build scripts cannot share code across packages) for av-kernel's own
//! targets.
//!
//! **Soft dependency, not a hard one.** `av-kernel`'s own library code has no GMAT dependency
//! (`src/lib.rs`'s module doc) and must build with no GMAT installed. If the default GMAT
//! library folder isn't present and `GMAT_ROOT`/`GMAT_LIB` aren't set to point at one, this
//! script emits nothing and the plain library (and any non-GMAT test) still builds; only
//! `tests/golden_acceptance.rs` needs the real dylibs, and it would already fail to *link*
//! (via `gmat-sys`) for the same underlying reason before this script's omission would matter.
use std::env;
use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let repo = manifest.parent().and_then(|p| p.parent()).expect("crate lives at <repo>/crates/av-kernel").to_path_buf();
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
