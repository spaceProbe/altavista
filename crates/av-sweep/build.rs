//! Re-emits the runtime library search path (`-rpath`) for GMAT's shared libraries, mirroring
//! `crates/av-kernel/build.rs` exactly (same default-path logic, kept in sync by hand since
//! build scripts cannot share code across packages -- see that file's own doc comment for the
//! full explanation).
//!
//! **Why `av-sweep` needs this even though it is itself pure and GMAT-free.** This crate's own
//! code (`schema.rs`/`hash.rs`/`grid.rs`/`seed.rs`/`sample.rs`) never touches `gmat_sys` or
//! `av_kernel::drm::{executor,binding}` -- but it depends on `av-kernel` as a whole crate (to
//! reuse `av_kernel::drm::schema`/`av_kernel::drm::hash`, per this crate's own task brief), and
//! `av-kernel`'s `Cargo.toml` links `gmat-sys` unconditionally (`src/drm/mod.rs`'s own doc
//! comment: "`gmat-sys` moved from `[dev-dependencies]` to `[dependencies]`... for `drm`'s
//! sake"). Cargo links the whole dependency graph into every test binary regardless of which
//! functions a given crate's own tests actually call, so `cargo test -p av-sweep`'s test binary
//! needs `libGmatBase`/`libGmatUtil` resolvable at *run* time (dyld, not just link time) even
//! though no `av-sweep` test ever constructs a `Gmat` handle. Without this build script,
//! `cargo:rustc-link-arg=-Wl,-rpath,...` from `gmat-sys`'s own build script (and from
//! `av-kernel`'s own re-emission of it) only reaches `gmat-sys`'s and `av-kernel`'s own
//! test/example binaries -- Cargo does not propagate `rustc-link-arg` a second package boundary
//! down to `av-sweep`'s.
//!
//! **Soft dependency, not a hard one** -- identical to `av-kernel/build.rs`: if the default GMAT
//! library folder is not present and `GMAT_ROOT`/`GMAT_LIB` are not set to point at one, this
//! script emits nothing, and `av-sweep`'s own plain library and tests (none of which import
//! `gmat_sys` at all) still build; only the *dynamic linker* step of running the test binary
//! needs the real dylibs findable, and it would already fail one layer up (in `av-kernel`'s own
//! `gmat-sys` link) for the same underlying reason before this script's omission would matter.
use std::env;
use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let repo = manifest.parent().and_then(|p| p.parent()).expect("crate lives at <repo>/crates/av-sweep").to_path_buf();
    let gmat_root = env::var("GMAT_ROOT").map(PathBuf::from).unwrap_or_else(|_| repo.join("GMAT R2026a"));
    let gmat_lib = env::var("GMAT_LIB").map(PathBuf::from).unwrap_or_else(|_| gmat_root.join("bin").join("GMAT-R2026a_Beta.app").join("Contents").join("Frameworks"));

    println!("cargo:rerun-if-env-changed=GMAT_ROOT");
    println!("cargo:rerun-if-env-changed=GMAT_LIB");

    if gmat_lib.is_dir() {
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", gmat_lib.display());
    }
}
