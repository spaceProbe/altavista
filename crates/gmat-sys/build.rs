//! Builds the C shim around GMAT's C++ core and links `libGmatBase` / `libGmatUtil`.
//!
//! Inputs (environment, with defaults for the AltaVista checkout):
//!   GMAT_ROOT  the GMAT install folder (contains bin/, data/)          default: <repo>/GMAT R2026a
//!   GMAT_SRC   a GMAT source checkout with src/base and src/gmatutil   default: <repo>/third_party/gmat-src
//!   GMAT_LIB   the folder holding libGmatBase*.dylib                   default: the app bundle's Frameworks
//!
//! The binary release ships no headers, so the source checkout is required to compile the
//! shim; only headers are read from it. GMAT R2026a is Apache 2.0 (ADR-002).
use std::env;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    manifest.parent().unwrap().parent().unwrap().to_path_buf()
}

fn include_dirs(root: &Path, out: &mut Vec<PathBuf>) {
    if !root.is_dir() {
        return;
    }
    out.push(root.to_path_buf());
    for entry in std::fs::read_dir(root).unwrap().flatten() {
        let p = entry.path();
        if p.is_dir() {
            include_dirs(&p, out);
        }
    }
}

fn main() {
    let repo = repo_root();
    let gmat_root = env::var("GMAT_ROOT").map(PathBuf::from).unwrap_or_else(|_| repo.join("GMAT R2026a"));
    let gmat_src = env::var("GMAT_SRC").map(PathBuf::from).unwrap_or_else(|_| repo.join("third_party").join("gmat-src"));
    let gmat_lib = env::var("GMAT_LIB").map(PathBuf::from).unwrap_or_else(|_| {
        gmat_root.join("bin").join("GMAT-R2026a_Beta.app").join("Contents").join("Frameworks")
    });

    println!("cargo:rerun-if-env-changed=GMAT_ROOT");
    println!("cargo:rerun-if-env-changed=GMAT_SRC");
    println!("cargo:rerun-if-env-changed=GMAT_LIB");
    println!("cargo:rerun-if-changed=shim/gmatffi.cpp");
    println!("cargo:rerun-if-changed=shim/gmatffi.h");

    for (name, p) in [("GMAT_SRC", &gmat_src), ("GMAT_LIB", &gmat_lib)] {
        assert!(p.is_dir(), "{name} does not exist: {}", p.display());
    }
    let mut includes = Vec::new();
    include_dirs(&gmat_src.join("src").join("base"), &mut includes);
    include_dirs(&gmat_src.join("src").join("gmatutil"), &mut includes);
    assert!(!includes.is_empty(), "no GMAT headers under {}", gmat_src.display());
    // CSPICE headers: GMAT's SpiceInterface.hpp includes SpiceUsr.h. Headers only; the
    // library itself is inside libGmatBase. See third_party/fetch-cspice.sh.
    let cspice = env::var("CSPICE_INCLUDE").map(PathBuf::from).unwrap_or_else(|_| repo.join("third_party").join("cspice").join("include"));
    println!("cargo:rerun-if-env-changed=CSPICE_INCLUDE");
    assert!(cspice.join("SpiceUsr.h").is_file(), "CSPICE headers not found at {} (run third_party/fetch-cspice.sh)", cspice.display());
    includes.push(cspice);

    let mut build = cc::Build::new();
    build.cpp(true).std("c++17").file("shim/gmatffi.cpp").warnings(false).flag_if_supported("-Wno-deprecated-declarations");
    // The shipped binaries are built with SPICE enabled (GMAT CMakeLists.txt line 249); the
    // define changes class layouts (CelestialBody carries a kernel-reader pointer), so the shim
    // must see the same headers the library was compiled from.
    build.define("__USE_SPICE__", None);
    for inc in &includes {
        build.include(inc);
    }
    build.compile("gmatffi");

    println!("cargo:rustc-link-search=native={}", gmat_lib.display());
    println!("cargo:rustc-link-lib=dylib=GmatBase.R2026a");
    println!("cargo:rustc-link-lib=dylib=GmatUtil.R2026a");
    println!("cargo:rustc-link-lib=dylib=c++");
    println!("cargo:rustc-link-arg=-Wl,-rpath,{}", gmat_lib.display());
    // Let dependents and tests find the install folder at run time.
    println!("cargo:root={}", gmat_root.display());
    println!("cargo:rustc-env=GMAT_SYS_ROOT={}", gmat_root.display());
}
