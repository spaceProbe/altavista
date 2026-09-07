//! Compiles `proto/altavista/v1/*.proto` into the `altavista.v1.dynamics_service_server`
//! module only (`tonic-build`, server-side): **every** `altavista.v1` message type is
//! `extern_path`'d onto `av_cdm::pb` (the same generated types `av-cdm`, `av-dynamics` and
//! `gmat-sys` already use -- `GmatModel::describe()` returns an `av_cdm::pb::ModelInfo`
//! directly), so this build produces the service trait/server plumbing only, never a
//! second, independent copy of `ModelInfo`/`PropagateRequest`/etc. `src/lib.rs`'s `pb`
//! module is therefore mostly re-exports of `av_cdm::pb` plus the generated
//! `dynamics_service_server` module.
//!
//! `protoc`/well-known-types resolution mirrors `crates/av-cdm/build.rs` and
//! `crates/av-grpc/build.rs` verbatim (same environment, same fallback order) -- kept as a
//! separate copy rather than a shared helper crate for the same reason those two give.
//!
//! Server-only: `build_client(false)` below. This crate never dials out as a gRPC client
//! (`crates/av-grpc` already owns that half); see `Cargo.toml`'s comment on why the
//! `tonic` dependency here has default features off with only `codegen`, `prost`, `server`
//! (no `channel`, no `tls*` -- `ring` never enters this crate's tree).
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
/// order as `crates/av-cdm/build.rs` / `crates/av-grpc/build.rs`.
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
        "av-dynamics-service build.rs: could not find google/protobuf/any.proto under any of \
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
        .expect("crate lives at <repo>/crates/av-dynamics-service")
        .to_path_buf();
    let proto_dir = repo_root.join("proto");
    let v1_dir = proto_dir.join("altavista").join("v1");

    let mut protos: Vec<PathBuf> = std::fs::read_dir(&v1_dir)
        .unwrap_or_else(|e| panic!("av-dynamics-service build.rs: cannot read {}: {e}", v1_dir.display()))
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|ext| ext == "proto"))
        .collect();
    protos.sort();
    assert!(
        !protos.is_empty(),
        "av-dynamics-service build.rs: no .proto files found under {}",
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

    // `gmat-sys`'s own build script's `cargo:rustc-link-lib`/`cargo:rustc-link-search`
    // already reach this crate's binary/test targets (Cargo propagates those transitively
    // for any package that declares `links = "gmatffi"`, which is how linking succeeds at
    // all) -- but `cargo:rustc-link-arg` (which `-rpath` has to be passed as) is explicitly
    // **not** propagated across a package boundary by Cargo, so `gmat-sys`'s own rpath only
    // reaches `gmat-sys`'s own test/example binaries, never a downstream crate's. This
    // crate hard-depends on `gmat-sys` (unlike `av-kernel`, whose GMAT dependency is a
    // dev-dependency only), so its own binary/test targets need the identical rpath;
    // `crates/av-kernel/build.rs` already found and documented this exact gap for its
    // `tests/golden_acceptance.rs` -- this re-emits the same rpath (same default-path logic,
    // kept in sync by hand since build scripts cannot share code across packages) for this
    // crate's own targets instead of leaving every downstream GMAT-linking crate to
    // rediscover it independently.
    let gmat_root = env::var("GMAT_ROOT").map(PathBuf::from).unwrap_or_else(|_| repo_root.join("GMAT R2026a"));
    let gmat_lib = env::var("GMAT_LIB")
        .map(PathBuf::from)
        .unwrap_or_else(|_| gmat_root.join("bin").join("GMAT-R2026a_Beta.app").join("Contents").join("Frameworks"));
    println!("cargo:rerun-if-env-changed=GMAT_ROOT");
    println!("cargo:rerun-if-env-changed=GMAT_LIB");
    if gmat_lib.is_dir() {
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", gmat_lib.display());
    }

    tonic_build::configure()
        .build_client(false) // this crate is a server only
        .build_server(true)
        .btree_map(["."]) // ADR-004/ADR-001 determinism: no HashMap iteration on any output path.
        // Every altavista.v1 message type is already generated once, in av-cdm (compiled
        // from these exact same .proto files -- see crates/av-cdm/build.rs). Externing the
        // whole package onto it means this build produces the `dynamics_service_server`
        // trait/module only, using av_cdm::pb::{DescribeRequest, ModelInfo, ...} directly,
        // never a second, independently-generated copy of the same wire types.
        .extern_path(".altavista.v1", "::av_cdm::pb")
        .extern_path(".google.protobuf.Any", "::prost_types::Any")
        .compile_protos(&protos, &[proto_dir, wkt_include])
        .unwrap_or_else(|e| panic!("av-dynamics-service build.rs: tonic-build failed: {e}"));
}
