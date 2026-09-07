//! M5.3: the Rust `tonic` side of `altavista.v1.DynamicsService`'s TLS front (ADR-004).
//!
//! `gmat-service` (Python, `grpcio`) stays plaintext on loopback -- `grpcio` bundles
//! BoringSSL, which ADR-004's crypto rule forbids terminating TLS with (see
//! `services/gmat-service/README.md`'s "TLS front door" / "FIPS and crypto accounting"
//! sections). TLS -- including mandatory client-certificate verification (mTLS) -- is
//! terminated by an `nginx` `grpc_pass` reverse proxy in front of it
//! (`services/gmat-service/deploy/nginx-gmat-grpc.conf.template`), the same way
//! `secproxy` fronts everything else in the SecRouter suite: nginx linking the host's
//! OpenSSL, never a bundled TLS stack.
//!
//! This crate is the client half of that picture: [`pb`] holds the generated
//! `altavista.v1` types and the `DynamicsServiceClient`, and [`tls`] wires an
//! OpenSSL-backed connector onto `tonic::transport::Endpoint::connect_with_connector` --
//! deliberately bypassing tonic's own `tls`/`tls-native-roots`/`tls-webpki-roots`
//! features, all three of which pull in `tokio-rustls` and therefore `ring` (ADR-004: "In
//! Rust this excludes `ring`-based TLS"). `Cargo.toml`'s dependency comment on `tonic` and
//! `services/gmat-service/README.md`'s `cargo tree -p av-grpc | grep -i ring` evidence
//! document that this crate's dependency tree carries none.
//!
//! `src/bin/describe_client.rs` is the small CLI this crate ships so
//! `tests/test_grpc_tls.py` can drive an mTLS `Describe` call (and the refused,
//! no-client-cert case) as a subprocess, without embedding a Rust extension in the Python
//! test.

pub mod pb {
    //! Generated `altavista.v1` types and the `DynamicsService` client, compiled from
    //! `proto/altavista/v1/*.proto` by `build.rs` via `tonic-build`. See
    //! `crates/av-cdm/src/lib.rs`'s `pb` module doc for why this is a submodule rather
    //! than a re-export at the crate root, and why it is `#[allow(clippy::all)]`
    //! (generated code, not this crate's own style).
    #![allow(clippy::all)]
    include!(concat!(env!("OUT_DIR"), "/altavista.v1.rs"));
}

pub mod tls;

pub use pb::dynamics_service_client::DynamicsServiceClient;
