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
//!
//! # N6's second job: telling `tests/ffi_c_caller.rs` where the staticlib landed
//!
//! `docs/native-dynamics-plan.md` milestone N6's C ABI deliverable needs `crates/av-orbital/
//! tests/ffi_c_caller.rs` to link a committed `.c` file against the real `libav_orbital.a` this
//! crate's own `[lib]` `crate-type` (this task's own Cargo.toml change) produces. That artifact
//! is NOT uplifted to an unhashed name at `<target-dir>/<profile>/` when the package is built
//! merely because `cargo test` needs it (measured directly, not assumed -- this task's own
//! report has the `deps/` listing): it lands under `<target-dir>/<profile>/deps/` as
//! `libav_orbital-<hash>.a`, the same hashed-name convention every `.rlib` there already uses.
//! `tests/ffi_c_caller.rs`'s own `find_staticlib` locates the exact hashed file at test-run
//! time by scanning that `deps/` directory (see that function's own doc comment); what THIS
//! script provides is only the parent `<target-dir>/<profile>/` directory itself, which is not
//! otherwise exposed to a test at either compile or run time. Derived from this script's own
//! `OUT_DIR` (`<target-dir>/<profile>/build/<pkg>-<hash>/out`, three path components below
//! `<target-dir>/<profile>/`) and emitted as a compile-time environment variable via
//! `cargo:rustc-env` -- which, unlike a build script's plain `println!`, Cargo propagates to the
//! compilation of every target IN THIS SAME PACKAGE, tests included, so `tests/ffi_c_caller.rs`
//! reads it with a plain `env!("AV_ORBITAL_LIB_DIR")`, no guessing at a profile name
//! (`debug`/`release`/a custom profile) or a `CARGO_BUILD_TARGET_DIR` override.
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

    // See this file's own module doc, "N6's second job", for why three ancestors and why
    // `cargo:rustc-env` rather than a file written under OUT_DIR.
    if let Ok(out_dir) = env::var("OUT_DIR") {
        if let Some(profile_dir) = PathBuf::from(&out_dir).ancestors().nth(3) {
            println!("cargo:rustc-env=AV_ORBITAL_LIB_DIR={}", profile_dir.display());
        } else {
            panic!("OUT_DIR ({out_dir}) has fewer than 3 ancestors -- Cargo's own OUT_DIR layout (<target-dir>/<profile>/build/<pkg>-<hash>/out) is assumed by this build script; see its own module doc, \"N6's second job\"");
        }
    }
}
