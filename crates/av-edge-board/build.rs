//! Compiles `proto/altavista/v1/board.proto` into the `altavista.v1.board_edge_service_server`
//! module only (`tonic-build`, server side), the way `crates/av-lockstep-shim/build.rs` does for
//! `LockstepService`: every `altavista.v1` message type is `extern_path`'d onto `av_cdm::pb`
//! (which compiles every `.proto` under `proto/altavista/v1/`), so this build produces the
//! service trait and server plumbing only, never a second copy of `PowerCycleRequest` and its
//! companions. `board.proto` imports nothing, so no well-known-types include directory is needed.
//!
//! `LockstepService` itself comes from `av-lockstep-shim`'s own build; this crate does not
//! generate it again.
use std::env;
use std::path::{Path, PathBuf};

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

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let repo_root = manifest_dir.parent().and_then(Path::parent).expect("crate lives at <repo>/crates/av-edge-board").to_path_buf();
    let proto_dir = repo_root.join("proto");
    let board_proto = proto_dir.join("altavista").join("v1").join("board.proto");
    assert!(board_proto.is_file(), "av-edge-board build.rs: missing {}", board_proto.display());

    println!("cargo:rerun-if-changed={}", board_proto.display());
    println!("cargo:rerun-if-env-changed=PROTOC");

    env::set_var("PROTOC", resolve_protoc());

    tonic_build::configure()
        .build_client(false) // this crate is a server only
        .build_server(true)
        .btree_map(["."]) // ADR-004/ADR-001 determinism: no HashMap iteration on any output path.
        .extern_path(".altavista.v1", "::av_cdm::pb")
        .compile_protos(&[board_proto], &[proto_dir])
        .unwrap_or_else(|e| panic!("av-edge-board build.rs: tonic-build failed: {e}"));
}
