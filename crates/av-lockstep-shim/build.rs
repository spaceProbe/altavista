//! Compiles `proto/altavista/v1/lockstep.proto` into the `altavista.v1.lockstep_service_server`
//! module only (`tonic-build`, server-side): every `altavista.v1` message type is
//! `extern_path`'d onto `av_cdm::pb` (the same generated types `av-cdm`, `av-grpc` and
//! `av-lockstep` already use), so this build produces the service trait/server plumbing
//! only, never a second, independently-generated copy of `LockstepBindRequest`/
//! `LockstepStepRequest`/`PortMessage`/etc. `src/lib.rs`'s `pb` module is therefore mostly
//! a re-export of `av_cdm::pb` plus the generated `lockstep_service_server` module.
//!
//! `protoc`/well-known-types resolution mirrors `crates/av-cdm/build.rs`,
//! `crates/av-grpc/build.rs` and `crates/av-dynamics-service/build.rs` verbatim (same
//! environment, same fallback order) -- kept as a separate copy rather than a shared
//! helper crate for the same reason those three give.
//!
//! Server-only: `build_client(false)` below. This crate never dials out as a gRPC client
//! (`crates/av-lockstep`/`crates/av-grpc` already own that half); see `Cargo.toml`'s
//! comment on why the `tonic` dependency here has default features off with only
//! `codegen`, `prost`, `server` (no `channel`, no `tls*` -- `ring` never enters this
//! crate's tree).
//!
//! Only `lockstep.proto` is passed to `compile_protos` (not every file under
//! `proto/altavista/v1/`, unlike the other three build scripts this one otherwise mirrors)
//! -- this crate hosts exactly one service, `LockstepService`, and has no use for a second,
//! unused `dynamics_service_server` module generated alongside it. `protoc` still resolves
//! `lockstep.proto`'s own `import "altavista/v1/system.proto"` through the `-I` (include)
//! path passed below, so this restriction changes nothing about what gets generated for
//! `LockstepService` itself.
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
/// order as the other three build scripts named in this file's module doc comment.
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
        "av-lockstep-shim build.rs: could not find google/protobuf/any.proto under any of \
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
        .expect("crate lives at <repo>/crates/av-lockstep-shim")
        .to_path_buf();
    let proto_dir = repo_root.join("proto");
    let v1_dir = proto_dir.join("altavista").join("v1");
    let lockstep_proto = v1_dir.join("lockstep.proto");

    assert!(lockstep_proto.is_file(), "av-lockstep-shim build.rs: missing {}", lockstep_proto.display());

    println!("cargo:rerun-if-changed={}", lockstep_proto.display());
    println!("cargo:rerun-if-changed={}", v1_dir.join("system.proto").display());
    println!("cargo:rerun-if-env-changed=PROTOC");
    println!("cargo:rerun-if-env-changed=PROTOC_INCLUDE");

    let protoc = resolve_protoc();
    env::set_var("PROTOC", &protoc);
    let wkt_include = resolve_wkt_include_dir(&protoc);

    tonic_build::configure()
        .build_client(false) // this crate is a server only
        .build_server(true)
        .btree_map(["."]) // ADR-004/ADR-001 determinism: no HashMap iteration on any output path.
        // Every altavista.v1 message type is already generated once, in av-cdm. Externing
        // the whole package onto it means this build produces the
        // `lockstep_service_server` trait/module only, using
        // av_cdm::pb::{LockstepBindRequest, LockstepStepRequest, PortMessage, ...}
        // directly, never a second, independently-generated copy of the same wire types.
        .extern_path(".altavista.v1", "::av_cdm::pb")
        .extern_path(".google.protobuf.Any", "::prost_types::Any")
        .compile_protos(&[lockstep_proto], &[proto_dir, wkt_include])
        .unwrap_or_else(|e| panic!("av-lockstep-shim build.rs: tonic-build failed: {e}"));
}
