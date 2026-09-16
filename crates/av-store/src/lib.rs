//! `av-store`: H1 (`docs/heavy-plan.md`, question 216) -- the S3-API object store client,
//! the content-addressed key layout it stores objects under, and the claim-check
//! ([`av_cdm::pb::AssetRef`]) that keeps heavy payload bytes off this platform's hot path.
//!
//! # The claim-check pattern
//!
//! A heavy payload (imagery, terrain, a point cloud, a mesh) never travels as message
//! bytes. It is stored once, here, and every message that needs to refer to it instead
//! carries an [`av_cdm::pb::AssetRef`] -- a *claim check* naming the object's URI, its
//! SHA-256, its size, its media type, its handling label and its provenance. A consumer
//! that actually needs the bytes dereferences the claim check itself, through
//! [`client::StoreClient::get`]; every consumer that does not (the overwhelming majority --
//! a catalog entry, a tile manifest, a command that merely references an asset) pays no
//! cost for the payload's existence at all. This is the same shape a content-addressed
//! cache or a Git object store uses, applied here specifically so a ten-gigabyte tile set
//! (`docs/heavy-plan.md`'s own exit criterion) never has to pass through a code path built
//! for kilobyte-sized telemetry.
//!
//! # Why the object key is the content hash
//!
//! [`keys::object_key`] derives an object's key entirely from its SHA-256 -- never from a
//! caller-chosen name, a UUID, or a timestamp. Three things fall out of that for free: (1)
//! `put`ing the same bytes twice is naturally idempotent (the second PUT overwrites the
//! first with byte-identical content at the same key, never a duplicate); (2) an
//! [`av_cdm::pb::AssetRef`]'s own `sha256` field and the object's storage location agree by
//! construction, so [`claim_check::verify_payload`] is checking the *same* hash the key was
//! derived from, not a second, independently-asserted one that could quietly drift from it;
//! (3) two different logical assets can never collide at the same key unless their bytes are
//! also identical, in which case "collide" is the correct behaviour, not a bug.
//!
//! # Why this crate is deliberately unreachable from the hot path
//!
//! `docs/heavy-plan.md` H1's own acceptance line: "a test that no crate on the hot path
//! (`av-ingest`, `av-track`, `av-command`) depends on `av-store`, enforced by `cargo tree`"
//! (`tests/claim_check_hot_path.rs`, this crate's own docker-free proof). The reasoning is
//! architectural, not just a test to satisfy: the hot path (measurement ingest, track
//! association, command authorization) runs on every message, at whatever rate the mission
//! demands, and must never be made to wait on an object-store round trip it does not need --
//! a claim check is *by design* the thing that lets a hot-path message be small, fast, and
//! entirely self-contained. If any hot-path crate ever gained even a transitive dependency
//! on this one, that architectural promise would already be broken, whether or not any
//! function in this crate were ever actually called from the hot path at runtime -- which is
//! exactly why the test checks the dependency graph itself (`cargo tree`), not merely that
//! no hot-path code currently calls into `av-store`.
//!
//! # The crypto rule (ADR-004)
//!
//! SHA-256 only, the system OpenSSL only, through the `openssl` crate:
//! `openssl::sha::sha256` for every content hash in this crate
//! ([`claim_check::asset_ref_for`], [`claim_check::verify_payload`]), the combination of
//! `openssl::pkey::PKey::hmac` and `openssl::sign::Signer` for [`sigv4`]'s HMAC-SHA256 chain,
//! `openssl::memcmp::eq` for [`claim_check::verify_payload`]'s constant-time hash compare, and
//! OpenSSL's `SslConnector` (never `rustls`) for [`client::StoreClient`]'s `https://` TLS.
//! `deny.toml`'s `[bans] deny` list makes this mechanical: `ring`, `sha2`, `md-5`/`md5`,
//! `sha1`, `blake2`/`blake3`, `rustls`/`tokio-rustls`/`rustls-webpki`/`webpki-roots` can never
//! enter this workspace's `Cargo.lock` at all, from any dependency, direct or transitive.

pub mod claim_check;
pub mod client;
pub mod error;
pub mod keys;
pub mod labels;
pub mod metadata;
pub mod sigv4;

pub use claim_check::{asset_ref_for, verify_payload, StoredObject};
pub use client::{StoreClient, StoreConfig};
pub use error::StoreError;
pub use keys::object_key;
pub use labels::ClearanceLadder;
