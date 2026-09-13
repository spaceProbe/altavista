//! Compiles `proto/altavista/v1/*.proto` into the `altavista.v1.edge_ingest_client` module
//! only (`tonic-build`, client-side): **every** `altavista.v1` message type is
//! `extern_path`'d onto `av_cdm::pb` -- the exact same types `av-edge`/`av-ingest` already
//! use -- so this build produces the `EdgeIngestClient` plumbing only, never a second,
//! independently-generated copy of `MeasurementBatch`/`PluginManifest`/etc.
//!
//! See this crate's `lib.rs` module doc for why this crate runs its **own** small
//! protoc/tonic-build pass rather than reusing `crates/av-grpc`'s already-compiled `pb`
//! module the way `crates/av-lockstep` does: `av-grpc/build.rs` externs only
//! `.altavista.v1.Port`/`.altavista.v1.PortMessage` onto `av_cdm::pb`, not the whole
//! package, so its own generated `MeasurementBatch`/`PluginManifest`/etc. would be a
//! second, nominally distinct (if wire-identical) type from `av_cdm::pb`'s -- exactly what
//! this crate's own `extern_path(".altavista.v1", "::av_cdm::pb")` below avoids, matching
//! `crates/av-dynamics-service/build.rs` and `crates/av-ingest/build.rs`'s own convention
//! instead.
//!
//! `protoc`/well-known-types resolution mirrors those two files verbatim (same
//! environment, same fallback order) -- kept as a separate copy rather than a shared
//! helper crate for the same reason they give.
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

/// Directory holding `google/protobuf/any.proto` for the resolved `protoc`. Same search
/// order as `crates/av-cdm/build.rs` / `crates/av-ingest/build.rs`.
fn resolve_wkt_include_dir(protoc: &Path) -> PathBuf {
    if let Ok(p) = env::var("PROTOC_INCLUDE") {
        let p = PathBuf::from(p);
        if p.join("google/protobuf/any.proto").is_file() {
            return p;
        }
    }

    let mut candidates: Vec<PathBuf> = vec![PathBuf::from("/opt/homebrew/opt/protobuf/include")];

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
        "av-ingest-client build.rs: could not find google/protobuf/any.proto under any of \
         {candidates:?}. Resolved protoc: {} ({version}). Set PROTOC_INCLUDE to the \
         well-known-types include directory, or install the protobuf package that ships \
         it (this repo documents /opt/homebrew/opt/protobuf/include).",
        protoc.display(),
    );
}

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let repo_root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("crate lives at <repo>/crates/av-ingest-client")
        .to_path_buf();
    let proto_dir = repo_root.join("proto");
    let v1_dir = proto_dir.join("altavista").join("v1");

    let mut protos: Vec<PathBuf> = std::fs::read_dir(&v1_dir)
        .unwrap_or_else(|e| panic!("av-ingest-client build.rs: cannot read {}: {e}", v1_dir.display()))
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|ext| ext == "proto"))
        .collect();
    protos.sort();
    assert!(
        !protos.is_empty(),
        "av-ingest-client build.rs: no .proto files found under {}",
        v1_dir.display()
    );

    for proto in &protos {
        println!("cargo:rerun-if-changed={}", proto.display());
    }
    println!("cargo:rerun-if-changed={}", v1_dir.display());
    println!("cargo:rerun-if-env-changed=PROTOC");
    println!("cargo:rerun-if-env-changed=PROTOC_INCLUDE");

    let protoc = resolve_protoc();
    env::set_var("PROTOC", &protoc);
    let wkt_include = resolve_wkt_include_dir(&protoc);

    tonic_build::configure()
        .build_client(true)
        .build_server(false) // this crate is a client only
        .btree_map(["."]) // ADR-004/ADR-001 determinism: no HashMap iteration on any output path.
        .extern_path(".altavista.v1", "::av_cdm::pb")
        .extern_path(".google.protobuf.Any", "::prost_types::Any")
        .compile_protos(&protos, &[proto_dir, wkt_include])
        .unwrap_or_else(|e| panic!("av-ingest-client build.rs: tonic-build failed: {e}"));
}
