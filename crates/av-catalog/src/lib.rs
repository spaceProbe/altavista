//! `av-catalog`: a hand-rolled PostgreSQL v3 frontend/backend wire client, with SCRAM-SHA-256
//! on the system OpenSSL, for the catalog (PostgreSQL + PostGIS) tier (ADR-003 question 40).
//!
//! # Why hand-rolled
//!
//! The catalog is PostgreSQL with PostGIS. Every existing Rust PostgreSQL client --
//! `tokio-postgres`, `postgres` (the same project's sync wrapper), `sqlx` (its `postgres`
//! feature), `deadpool-postgres` (pools `tokio-postgres` connections, the same problem one
//! layer up) -- reaches SCRAM authentication through the RustCrypto crates `sha2`, `hmac` and
//! (for the legacy MD5 auth path) `md-5`. All three crate NAMES are on `deny.toml`'s
//! `[bans] deny` list -- ADR-004's crypto rule: SHA-256 only, through the system OpenSSL only.
//! `cargo deny check` fails the instant any of them enters `Cargo.lock`, so none of those
//! clients -- nor anything that depends on one of them -- can ever be a dependency of this
//! workspace, no matter how it is configured.
//!
//! `pq-sys`/libpq (the C client library `sqlx`'s `postgres` feature can also be pointed at) was
//! considered and rejected by the manager for a different, non-cryptographic reason: it would
//! make a C library -- and its discovery via `pkg-config`/`libpq-dev`, which varies by host and
//! package manager -- a hard build-time prerequisite for `cargo build --workspace`, turning a
//! plain clone of this repository from "builds" into "fails to build" depending on what
//! happens to already be installed on a given machine. A pure-Rust client that happens to link
//! `openssl-sys` (already a build-time prerequisite of this workspace's `openssl` dependency,
//! present in `crates/av-command`, `crates/av-gateway`, `crates/av-edge` and `crates/av-store`
//! long before this crate existed) adds no NEW class of build-time dependency the workspace did
//! not already have.
//!
//! So this crate speaks the PostgreSQL frontend/backend wire protocol (version 3.0) itself --
//! [`protocol`] -- and performs SCRAM-SHA-256 authentication with the `openssl` crate's own
//! primitives -- [`scram`] -- rather than depending on any of the above. This is the single
//! most important thing a future reader of this crate needs to know before reaching for
//! `tokio-postgres` "just this once": the ban is not a style preference, it is a hard
//! `cargo deny check` failure, checked mechanically, every time.
//!
//! **Measured fact this crate is built against** (`services/catalog/IMAGE_DIGEST.md`, manager,
//! 2026-09-15, against the real `imresamu/postgis` image): the container's generated
//! `pg_hba.conf` ends with `host all all all scram-sha-256`. A client connecting from outside
//! the container -- which is every client this workspace has -- MUST perform SASL
//! SCRAM-SHA-256; `trust` covers only `local`/`127.0.0.1` INSIDE the container. Server version
//! `17.11`, PostGIS `3.5.4`.
//!
//! # Scope of this crate: H2a's transport, plus H2's schema/migrations/queries on top
//!
//! H2a delivered the TRANSPORT half only: wire framing ([`protocol`]), authentication
//! ([`scram`]), and the async connection with the extended query protocol ([`client`]). H2
//! (this task) adds the catalog schema on top of that transport, using [`client::PgClient`] as
//! its one entry point to the wire, never a second connection path:
//!
//! - [`migrate`] -- [`migrate::Migrator`], the committed/hashed/ordered `.sql` migration set
//!   (`migrations/`) and its drift refusal.
//! - [`model`] -- [`model::CatalogAsset`], the Rust record for one `assets` row, with
//!   conversions to/from `av_cdm::pb::AssetRef`.
//! - [`query`] -- [`query::find_assets`] (extent/time/label-filtered, the label filter always
//!   evaluated IN SQL) and the job-lineage read/write pair.
//! - [`labels`] -- [`labels::ClearanceLadder`], now question 218's shared `av-label` crate,
//!   re-exported here as a thin adapter (see that module's own doc for exactly what adapts).
//! - [`pgtext`] -- pure PostgreSQL TEXT-format array/`bytea` encode/decode, shared by
//!   [`model`] and [`query`].
//!
//! # Module layout
//!
//! - [`protocol`] -- pure, synchronous, allocation-explicit encode/decode of every PostgreSQL
//!   v3 frontend/backend message this crate needs, over `&[u8]`/`Vec<u8>` only. No I/O; see
//!   its own module doc for why that is what makes it exhaustively testable.
//! - [`scram`] -- SCRAM-SHA-256 (RFC 5802/RFC 7677) on the system OpenSSL. Also pure: given a
//!   username, password and the server's own messages (as plain `&str`), it computes the
//!   client's messages and the expected server signature; no socket.
//! - [`client`] -- [`client::PgClient`], the async connection: `tokio::net::TcpStream` (or, for
//!   [`client::PgTls::Required`], an OpenSSL-wrapped stream), driving [`protocol`] and
//!   [`scram`] to speak the startup/auth handshake and the extended query protocol.
//! - [`error`] -- [`error::CatalogError`], the one error type for this crate.
//! - [`migrate`], [`model`], [`query`], [`labels`], [`pgtext`] -- this task's own additions,
//!   described above.

pub mod client;
pub mod error;
pub mod labels;
pub mod migrate;
pub mod model;
pub mod pgtext;
pub mod protocol;
pub mod query;
pub mod scram;

pub use client::{Param, PgClient, PgConfig, PgTls, Row};
pub use error::CatalogError;
pub use labels::ClearanceLadder;
pub use migrate::{MigrationRecord, Migrator, MIGRATIONS};
pub use model::CatalogAsset;
pub use query::{find_assets, insert_lineage, lineage_parents, AssetQuery, GeoBbox, TimeRange, MAX_QUERY_LIMIT};
