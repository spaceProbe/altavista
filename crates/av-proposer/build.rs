//! Two independent codegen passes, into the same `OUT_DIR`, never touching each other's
//! output files:
//!
//! 1. **spoore's `ModelService` contract (D2), server only.** Compiles
//!    `/Users/probe/code/spoore/proto/spoore/v0/model_service.proto` with
//!    `build_server(true)`, `build_client(false)`, `.extern_path(".spoore.v0",
//!    "::spoore_cdm::proto")` -- byte-for-byte the same three choices as `spoore-ml/build.rs`
//!    (that crate's own doc: "the load-bearing line... a belief that arrives from a sidecar
//!    is decoded by the same fallible `TryFrom` as a belief that arrives from anywhere
//!    else"), mirrored here for the SERVER half that crate deliberately never generates
//!    ("spoore deliberately generates no Rust server of its own" -- this crate is that
//!    server). Output: `$OUT_DIR/spoore.v0.rs`, `include!`'d by `crate::model_service::pb`.
//!
//! 2. **This workspace's own `DataGatewayService`/`ModelProposeService`, CLIENT only, by
//!    hand-declared service descriptors -- never by compiling `authority.proto`'s text.**
//!    This is the one deliberate departure from `crates/av-gateway/build.rs`'s convention
//!    (compile the whole `proto/altavista/v1/` directory with `tonic_build::configure()`),
//!    and it exists for exactly one reason: `authority.proto` ALSO declares
//!    `CommandAuthorityService` in the same file, and `tonic_build::configure().compile_
//!    protos(...)` generates client+server code for every service the compiled file(s)
//!    contain -- there is no per-service filter on that API. Compiling `authority.proto`
//!    here at all, under any feature combination, would therefore give this crate a
//!    `CommandAuthorityServiceClient` it must structurally never have (this task's own
//!    acceptance evidence item 1: "this crate generates no client for CommandAuthorityService
//!    at all"). `tonic_build::manual` (unconditionally compiled into `tonic-build`, gated on
//!    no cargo feature at all -- checked directly against `tonic-build-0.12.3/src/lib.rs`
//!    before this design was chosen: `pub mod manual;` carries no `#[cfg(feature = ...)]`,
//!    unlike the `prost`-gated `pub mod prost;` the normal `tonic_build::configure()` path
//!    uses) is the tool built for exactly this: hand-declared `Service`/`Method` descriptors,
//!    with the request/response Rust type named by a string path rather than parsed from a
//!    `.proto` file. Every message type named below (`av_cdm::pb::GatewayQueryRequest`, ...)
//!    is already compiled by `av-cdm`'s own `build.rs` (a plain `prost_build::compile_protos`
//!    over the WHOLE `proto/altavista/v1/` directory, messages only -- `prost_build` does not
//!    generate service code at all, so `av-cdm`'s own tree never carries any service client
//!    either); this pass only re-declares the two services' own method shapes against those
//!    already-compiled types, at the identical wire route
//!    (`/altavista.v1.<Service>/<Method>`) `av-gateway`'s own real, proto-compiled server
//!    answers on -- so the generated client interoperates with the real service, byte for
//!    byte, while never having parsed a line of `CommandAuthorityService`'s own declaration.
//!    Output: `$OUT_DIR/altavista.v1.DataGatewayService.rs` and
//!    `$OUT_DIR/altavista.v1.ModelProposeService.rs`, each `include!`'d by
//!    `crate::gateway_client::pb`.

use std::env;
use std::path::PathBuf;

const SPOORE_PROTO_ROOT: &str = "/Users/probe/code/spoore/proto";

fn compile_model_service_server() {
    let proto_dir = PathBuf::from(SPOORE_PROTO_ROOT);
    let model_service = proto_dir.join("spoore/v0/model_service.proto");
    let cdm = proto_dir.join("spoore/v0/cdm.proto");
    println!("cargo:rerun-if-changed={}", model_service.display());
    println!("cargo:rerun-if-changed={}", cdm.display());

    tonic_build::configure()
        .build_server(true)
        .build_client(false)
        .extern_path(".spoore.v0", "::spoore_cdm::proto")
        .compile_protos(&[model_service], &[proto_dir])
        .unwrap_or_else(|e| panic!("av-proposer build.rs: compiling spoore's model_service.proto failed: {e}"));
}

fn manual_gateway_client_services() -> Vec<tonic_build::manual::Service> {
    use tonic_build::manual::{Method, Service};

    // altavista.v1.DataGatewayService -- read-only (D1's own "nothing here can create a
    // Command" guarantee is exactly why this crate is allowed to hold a client for it at
    // all).
    let data_gateway = Service::builder()
        .name("DataGatewayService")
        .package("altavista.v1")
        .comment("Hand-declared (see build.rs's own module doc): the read-only query rpc av-gateway serves, generated here without ever parsing authority.proto's own text at all.")
        .method(
            Method::builder()
                .name("query")
                .route_name("Query")
                .input_type("::av_cdm::pb::GatewayQueryRequest")
                .output_type("::av_cdm::pb::GatewayQueryResponse")
                .codec_path("tonic::codec::ProstCodec")
                .build(),
        )
        .build();

    // altavista.v1.ModelProposeService -- A4b's own network propose path (D1). ONE method,
    // matching authority.proto's own declaration exactly.
    let model_propose = Service::builder()
        .name("ModelProposeService")
        .package("altavista.v1")
        .comment("Hand-declared (see build.rs's own module doc): A4b's network propose path, generated here without ever parsing authority.proto's own text at all.")
        .method(
            Method::builder()
                .name("propose_command")
                .route_name("ProposeCommand")
                .input_type("::av_cdm::pb::ProposeCommandRequest")
                .output_type("::av_cdm::pb::ProposeCommandResponse")
                .codec_path("tonic::codec::ProstCodec")
                .build(),
        )
        .build();

    vec![data_gateway, model_propose]
}

fn compile_gateway_client() {
    // No `cargo:rerun-if-changed` on `authority.proto` here: this pass never reads that
    // file's bytes at all (the whole point -- see the module doc). The service descriptors
    // above are declared in THIS file, so ordinary `cargo:rerun-if-changed=build.rs` (which
    // cargo emits by default whenever no `rerun-if-changed` line is printed at all -- but
    // this build.rs DOES print one, above, for the spoore protos, which suppresses that
    // default) would otherwise miss a change to this file's own hand-declared shapes.
    println!("cargo:rerun-if-changed=build.rs");

    tonic_build::manual::Builder::new().build_client(true).build_server(false).compile(&manual_gateway_client_services());
}

fn main() {
    let _out_dir = env::var("OUT_DIR").expect("cargo always sets OUT_DIR for a build script");
    compile_model_service_server();
    compile_gateway_client();
}
