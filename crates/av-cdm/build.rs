//! Compiles `proto/altavista/v1/*.proto` (the CDM v1 messages, ADR-001) into Rust with
//! `prost-build`, exposed by `src/lib.rs` as the `av_cdm::pb` module.
//!
//! `protoc` resolution order: the `PROTOC` environment variable if set, else
//! `/opt/homebrew/opt/protobuf/bin/protoc` -- wait, see below -- else `protoc` on `PATH`.
//! The protos import `google/protobuf/any.proto` (`envelope.proto`, `command.proto`), which
//! is a well-known type shipped alongside `protoc` itself, not under `proto/`; its include
//! directory is resolved the same way `protoc`'s own version is, plus a couple of
//! Homebrew-standard fallbacks, and this build script fails loudly (rather than emitting a
//! crate silently missing `Any`) if none of them contain it.
use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The fixed `/opt/homebrew/bin/protoc` path named in this repo's environment notes.
const HOMEBREW_PROTOC: &str = "/opt/homebrew/bin/protoc";

fn resolve_protoc() -> PathBuf {
    if let Ok(p) = env::var("PROTOC") {
        return PathBuf::from(p);
    }
    if Path::new(HOMEBREW_PROTOC).is_file() {
        return PathBuf::from(HOMEBREW_PROTOC);
    }
    PathBuf::from("protoc")
}

/// Directory holding `google/protobuf/any.proto` (and the other well-known types) for the
/// resolved `protoc`. Tries, in order: `$PROTOC_INCLUDE`, the Homebrew `protobuf` keg's
/// `include` (matching this repo's documented path), `protoc --version`'s Homebrew Cellar
/// sibling `include` (in case a different keg version is active), then the usual
/// `/usr/local` and `/usr` prefixes.
fn resolve_wkt_include_dir(protoc: &Path) -> PathBuf {
    if let Ok(p) = env::var("PROTOC_INCLUDE") {
        let p = PathBuf::from(p);
        if p.join("google/protobuf/any.proto").is_file() {
            return p;
        }
    }

    let mut candidates: Vec<PathBuf> = vec![PathBuf::from("/opt/homebrew/opt/protobuf/include")];

    // protoc is usually <prefix>/bin/protoc with the matching include at <prefix>/include.
    if let Some(bin_dir) = protoc.parent() {
        if let Some(prefix) = bin_dir.parent() {
            candidates.push(prefix.join("include"));
        }
    }

    candidates.push(PathBuf::from("/usr/local/include"));
    candidates.push(PathBuf::from("/usr/include"));

    for c in &candidates {
        if c.join("google/protobuf/any.proto").is_file() {
            return c.clone();
        }
    }

    let version = Command::new(protoc)
        .arg("--version")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|e| format!("<failed to run {}: {e}>", protoc.display()));

    panic!(
        "av-cdm build.rs: could not find google/protobuf/any.proto (needed by \
         envelope.proto and command.proto) under any of {candidates:?}. \
         Resolved protoc: {} ({version}). \
         Set PROTOC_INCLUDE to the well-known-types include directory, or install the \
         protobuf package that ships it (this repo documents \
         /opt/homebrew/opt/protobuf/include).",
        protoc.display(),
    );
}

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let repo_root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("crate lives at <repo>/crates/av-cdm")
        .to_path_buf();
    let proto_dir = repo_root.join("proto");
    let v1_dir = proto_dir.join("altavista").join("v1");

    let mut protos: Vec<PathBuf> = std::fs::read_dir(&v1_dir)
        .unwrap_or_else(|e| panic!("av-cdm build.rs: cannot read {}: {e}", v1_dir.display()))
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|ext| ext == "proto"))
        .collect();
    protos.sort();
    assert!(
        !protos.is_empty(),
        "av-cdm build.rs: no .proto files found under {}",
        v1_dir.display()
    );

    for proto in &protos {
        println!("cargo:rerun-if-changed={}", proto.display());
    }
    // A new .proto file must also trigger a rebuild: per-file rerun-if-changed lines only
    // watch files that already existed at the last build (found 2026-09-04 when
    // lockstep.proto was added and av_cdm::pb silently lacked its types).
    println!("cargo:rerun-if-changed={}", v1_dir.display());
    println!("cargo:rerun-if-env-changed=PROTOC");
    println!("cargo:rerun-if-env-changed=PROTOC_INCLUDE");

    let protoc = resolve_protoc();
    env::set_var("PROTOC", &protoc);
    let wkt_include = resolve_wkt_include_dir(&protoc);

    prost_build::Config::new()
        .btree_map(["."]) // ADR-004/ADR-001 determinism: no HashMap iteration on any output path.
        // envelope.proto's `BatchMessage.payload` is `google.protobuf.Any`. Without this,
        // prost-build would generate its own copy of the WKT message shape (reachable only
        // through the include dir, not compiled directly) instead of using `prost_types::Any`,
        // which callers packing/unpacking payloads actually want. `prost-types` is a direct
        // dependency of this crate for exactly this reason.
        .extern_path(".google.protobuf.Any", "::prost_types::Any")
        .compile_protos(&protos, &[proto_dir, wkt_include])
        .unwrap_or_else(|e| panic!("av-cdm build.rs: prost-build failed: {e}"));
}
