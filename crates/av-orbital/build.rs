//! Emits the runtime library search path (`-rpath`) for GMAT's shared libraries, when a GMAT
//! install is available, so `tests/frame_gmat.rs` and `src/frame_gmat.rs`'s own unit tests
//! (which pull in `gmat-sys`, an optional dependency behind the `gmat-frames` feature) can
//! find `libGmatBase`/`libGmatUtil` at *run* time, not just link time.
//!
//! Identical reasoning and identical logic to `crates/av-kernel/build.rs` (read before editing
//! either -- kept in sync by hand since build scripts cannot share code across packages):
//! `gmat-sys`'s own build script's `cargo:rustc-link-arg` (which `-rpath` has to be passed as)
//! is explicitly NOT propagated across a package boundary by Cargo, so `gmat-sys`'s own rpath
//! only reaches `gmat-sys`'s own test/example binaries, never a downstream package's --
//! `av-orbital` is exactly such a downstream package once `gmat-frames` pulls `gmat-sys` in.
//! This is new plumbing in `crates/av-orbital` alone (a `build.rs` this crate did not
//! previously need, since it built with no GMAT dependency at all through the first N1
//! worker's half) -- it does not edit `crates/gmat-sys`/`crates/av-kernel`.
//!
//! **Soft dependency, not a hard one**, exactly like `av-kernel`'s: `cargo build -p av-orbital
//! --no-default-features` never links `gmat-sys` at all (the `gmat-frames` feature gate), so
//! this script emitting nothing when no GMAT install is found never blocks that build; only a
//! `gmat-frames`-enabled test that actually touches GMAT needs the real dylibs, and it would
//! already fail to *link* for the same underlying reason before this script's omission would
//! matter.
use std::env;
use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let repo = manifest.parent().and_then(|p| p.parent()).expect("crate lives at <repo>/crates/av-orbital").to_path_buf();
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
