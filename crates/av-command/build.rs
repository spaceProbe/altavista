//! Compiles `proto/altavista/v1/*.proto` into the `altavista.v1.command_authority_server`
//! **and** `command_authority_client` modules (`tonic-build`), mirroring
//! `crates/av-dynamics-service/build.rs` for the `extern_path`ing: **every** `altavista.v1`
//! message type is `extern_path`'d onto `av_cdm::pb` (the same generated types `av-cdm`
//! already compiles from these exact same `.proto` files -- `crates/av-cdm/build.rs`), so
//! this build never produces a second, independently-generated copy of `Command`/
//! `CommandTransition`/`PolicyInput`/`PolicyDecision`/`LedgerRecord`/`ProposeRequest`/... --
//! including the new request/response messages `authority.proto` gained in A1.3, since
//! `av-cdm`'s own `prost-build` pass already compiles every message in every
//! `proto/altavista/v1/*.proto` file (it does not special-case `authority.proto`; it walks
//! the whole directory), so those new messages already exist as `av_cdm::pb::
//! ProposeRequest`/etc. by the time this crate's own build runs.
//!
//! # `build_client(true)`, even though this crate's production code is server-only
//!
//! Unlike `crates/av-dynamics-service/build.rs` (`build_client(false)`, a crate with no test
//! suite that dials its own service), this build script generates **both** sides.
//! `tonic-build`'s generated client stub (`XClient<T>`, generic over `T: tonic::client::
//! GrpcService<...>`) does not itself reference `tonic::transport::Channel` or require the
//! `"channel"`/`"transport"` `tonic` Cargo feature to *compile* -- only *constructing* a real
//! `Channel` to hand to `XClient::new(channel)` needs that feature, and only this crate's
//! `[dev-dependencies]` tonic entry enables it (see `Cargo.toml`'s comment). So a plain `cargo
//! build -p av-command` (no dev-dependencies active) still compiles this generated client
//! module cleanly with the production `["codegen", "prost", "server"]` feature set alone;
//! `cargo test -p av-command` additionally activates `[dev-dependencies]`' `"channel"`
//! feature (Cargo's ordinary per-dependency feature unification across `[dependencies]`/
//! `[dev-dependencies]` for the *same* crate, `tonic` here), which is what
//! `crates/av-command/tests/*.rs` actually needs to build a real `Channel` and call this
//! generated client. Production code (`src/service.rs`, `src/bin/av-command.rs`) never
//! imports the generated client module at all -- it exists only for the test suite to use.
//!
//! `protoc`/well-known-types resolution mirrors `crates/av-cdm/build.rs` and
//! `crates/av-dynamics-service/build.rs` verbatim (same environment, same fallback order) --
//! kept as a separate copy rather than a shared helper crate for the same reason those two
//! give.
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
/// order as `crates/av-cdm/build.rs` / `crates/av-dynamics-service/build.rs`.
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
        "av-command build.rs: could not find google/protobuf/any.proto under any of \
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
        .expect("crate lives at <repo>/crates/av-command")
        .to_path_buf();
    let proto_dir = repo_root.join("proto");
    let v1_dir = proto_dir.join("altavista").join("v1");

    let mut protos: Vec<PathBuf> = std::fs::read_dir(&v1_dir)
        .unwrap_or_else(|e| panic!("av-command build.rs: cannot read {}: {e}", v1_dir.display()))
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|ext| ext == "proto"))
        .collect();
    protos.sort();
    assert!(
        !protos.is_empty(),
        "av-command build.rs: no .proto files found under {}",
        v1_dir.display()
    );

    for proto in &protos {
        println!("cargo:rerun-if-changed={}", proto.display());
    }
    // A new .proto file must also trigger a rebuild: per-file rerun-if-changed lines only
    // watch files that already existed at the last build (the same gap
    // crates/av-cdm/build.rs and crates/av-dynamics-service/build.rs already document).
    println!("cargo:rerun-if-changed={}", v1_dir.display());
    println!("cargo:rerun-if-env-changed=PROTOC");
    println!("cargo:rerun-if-env-changed=PROTOC_INCLUDE");

    let protoc = resolve_protoc();
    env::set_var("PROTOC", &protoc);
    let wkt_include = resolve_wkt_include_dir(&protoc);

    tonic_build::configure()
        .build_client(true) // test-only use; see this file's module doc for why this is safe
        .build_server(true)
        .btree_map(["."]) // ADR-004/ADR-001 determinism: no HashMap iteration on any output path.
        // Every altavista.v1 message type is already generated once, in av-cdm (compiled
        // from these exact same .proto files -- see crates/av-cdm/build.rs). Externing the
        // whole package onto it means this build produces the `command_authority_server`
        // trait/module only, using av_cdm::pb::{Command, ProposeRequest, ...} directly, never
        // a second, independently-generated copy of the same wire types.
        .extern_path(".altavista.v1", "::av_cdm::pb")
        .extern_path(".google.protobuf.Any", "::prost_types::Any")
        .compile_protos(&protos, &[proto_dir, wkt_include])
        .unwrap_or_else(|e| panic!("av-command build.rs: tonic-build failed: {e}"));
}
