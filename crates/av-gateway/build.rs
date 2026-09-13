//! Compiles `proto/altavista/v1/*.proto` via `tonic-build`, mirroring
//! `crates/av-command/build.rs` verbatim (same `protoc`/well-known-types resolution, same
//! `extern_path`ing of every `altavista.v1` message type onto [`av_cdm::pb`] so this build
//! never produces a second, independently-generated copy of a wire type -- see that file's
//! own module doc for the full reasoning, not repeated here). Compiling the whole directory
//! (not just `authority.proto`) is deliberate and harmless, exactly as it is for
//! `crates/av-command/build.rs`: `tonic-build` also regenerates a second
//! `command_authority_service_server`/`_client` pair in *this* crate's own `OUT_DIR`,
//! distinct from (and never linked against) `av-command`'s own copy -- this crate's
//! [`crate::propose_only`] uses this crate's own regenerated client, never reaches into
//! `av_command::pb`.
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
/// order as `crates/av-cdm/build.rs` / `crates/av-command/build.rs`.
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
        "av-gateway build.rs: could not find google/protobuf/any.proto under any of \
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
        .expect("crate lives at <repo>/crates/av-gateway")
        .to_path_buf();
    let proto_dir = repo_root.join("proto");
    let v1_dir = proto_dir.join("altavista").join("v1");

    let mut protos: Vec<PathBuf> = std::fs::read_dir(&v1_dir)
        .unwrap_or_else(|e| panic!("av-gateway build.rs: cannot read {}: {e}", v1_dir.display()))
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|ext| ext == "proto"))
        .collect();
    protos.sort();
    assert!(!protos.is_empty(), "av-gateway build.rs: no .proto files found under {}", v1_dir.display());

    for proto in &protos {
        println!("cargo:rerun-if-changed={}", proto.display());
    }
    // A new .proto file must also trigger a rebuild -- see crates/av-cdm/build.rs's and
    // crates/av-command/build.rs's identical comment for why the per-file lines alone do
    // not cover this.
    println!("cargo:rerun-if-changed={}", v1_dir.display());
    println!("cargo:rerun-if-env-changed=PROTOC");
    println!("cargo:rerun-if-env-changed=PROTOC_INCLUDE");

    let protoc = resolve_protoc();
    env::set_var("PROTOC", &protoc);
    let wkt_include = resolve_wkt_include_dir(&protoc);

    tonic_build::configure()
        .build_client(true)
        .build_server(true)
        .btree_map(["."]) // ADR-004/ADR-001 determinism: no HashMap iteration on any output path.
        .extern_path(".altavista.v1", "::av_cdm::pb")
        .extern_path(".google.protobuf.Any", "::prost_types::Any")
        .compile_protos(&protos, &[proto_dir, wkt_include])
        .unwrap_or_else(|e| panic!("av-gateway build.rs: tonic-build failed: {e}"));
}
