//! H4 (`docs/heavy-plan.md`, round 2): the tile gateway. Serves tiles out of `av-store` by
//! manifest hash and tile address, enforcing labels per layer and per request through the
//! shared `av_command::oidc` verifier and `av_label::ClearanceLadder`, with every refusal
//! typed and counted (ADR-004).
//!
//! # Routes
//!
//! - `GET /v1/tilesets/{manifest_sha256}/manifest`
//! - `GET /v1/tilesets/{manifest_sha256}/tiles/{level}/{x}/{y}`
//!
//! Both authenticate via `Authorization: Bearer <token>` through [`av_command::oidc::verify`]
//! -- **never** a caller-declared clearance from a header or query parameter (neither route
//! even has one). See [`core::handle`] for the full, fixed pipeline order.
//!
//! # Module map
//!
//! - [`route`] -- step 1: parses a request path into a [`route::Route`], no I/O.
//! - [`source`] -- the object-store seam ([`source::ObjectSource`]), an in-memory test double
//!   ([`source::InMemoryObjectSource`]), and the real `av-store`-backed adapter
//!   ([`source::StoreObjectSource`]). See that module's own doc for why this crate does its
//!   own unauthenticated fetch-by-key rather than driving `av_store::StoreClient::get`'s
//!   bundled authorize-then-fetch contract directly.
//! - [`range`] -- P2: parses a `Range: bytes=<start>-<end>` header against a known resource
//!   length; see that module's own doc for the "unparseable is ignored, unsatisfiable is
//!   refused" distinction.
//! - [`refusal`] -- every typed, counted [`refusal::TileRefusal`], their HTTP status mapping,
//!   and the "per layer and per request label enforcement is one check, not two" reasoning.
//! - [`config`] -- [`config::TilesConfig`], this deployment's own fixed configuration.
//! - [`core`] -- [`core::handle`], the request pipeline itself: networking-free, directly
//!   unit-testable.
//! - [`server`] -- the plain `tokio::net::TcpListener` HTTP surface, mirroring
//!   `crates/av-command/src/admin.rs`'s own hand-rolled pattern (that module's own doc: no
//!   `axum`/`hyper` direct dependency, manual request-line/header parsing) -- see that
//!   module's own doc for why a framework buys nothing here either.
//! - [`admin`] -- H5b-1: an OPTIONAL, separately-bound `GET /admin/api/counters` surface (see
//!   that module's own doc for why round 2's "no admin surface" decision is revisited here).

pub mod admin;
pub mod config;
pub mod core;
pub mod range;
pub mod refusal;
pub mod route;
pub mod server;
pub mod source;

/// H4/P0 precedent (`av-gateway`'s own `crate::counters` module): `av_command::counters` is
/// the one shared counting primitive in this workspace (ADR-004, "everything rejected is
/// counted") -- re-exported here under the identical `crate::counters` path so this crate's
/// own call sites (and its tests) read `crate::counters::{Counted, Counters}` exactly like
/// every other crate on this track, never a second, independently-typed counting mechanism.
pub mod counters {
    pub use av_command::counters::{Counted, Counters};
}
