//! D2: the generated `spoore.v0.ModelService` SERVER trait + wrapper, from `build.rs`'s
//! first codegen pass (mirrors `spoore-ml/build.rs`'s client-only pass, `build_server(true)`/
//! `build_client(false)` swapped -- that crate's own doc: "spoore deliberately generates no
//! Rust server of its own"; this crate is that server). Every message type
//! (`WarmStartRequest`, `Belief`, ...) is `extern_path`'d onto [`spoore_cdm::proto`], so this
//! module contains ONLY the service trait/server wrapper, never a second, independently-
//! compiled copy of a CDM wire type -- reach `spoore_cdm::proto::*` directly for the message
//! types, never through this module.

#![allow(clippy::all)] // generated code, not this crate's own style -- see crate::model_service_pb's own doc.

include!(concat!(env!("OUT_DIR"), "/spoore.v0.rs"));
