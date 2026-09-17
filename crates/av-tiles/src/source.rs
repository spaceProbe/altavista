//! The object-store seam: [`ObjectSource`], mirroring `crates/av-jobs::runner::ObjectSource`'s
//! own reasoning (that module's own doc: "`av-jobs` must not depend on `av-store`... a real,
//! store-backed implementation [comes from] a crate that depends on both"). `av-tiles` DOES
//! depend on `av-store` directly (unlike `av-jobs`, which cannot -- see this crate's own
//! `Cargo.toml`), but the seam is worth keeping anyway, for a different, still real reason:
//! this crate's own request pipeline (`crate::core::handle`) needs to run in tests with no
//! MinIO container at all (P1's acceptance tests -- refused-tile, byte-equality,
//! manifest-hash-mismatch, and every auth case -- are all in-process, no docker gate), and a
//! trait object is what lets [`InMemoryObjectSource`] stand in for a real store with zero
//! network I/O.
//!
//! # Why this trait does its own unauthenticated fetch, not `av_store::StoreClient::get`
//!
//! `StoreClient::get` bundles three things into one call: authorization (`ladder.
//! authorize_read(caller_clearance, asset.label)`), the fetch, and a hash check against
//! `asset.sha256` -- and it requires the CALLER to already know the object's real `Label`
//! before calling it (`asset.label` is read from the `AssetRef` the caller passes in, never
//! fetched fresh from the server; see `crates/av-store/src/client.rs`'s own doc for
//! `get`/`head`). That contract fits a caller who already holds a trusted `AssetRef` (from a
//! catalog row, or a manifest entry) and just wants verified bytes back. This crate is not
//! that caller for its OWN first fetch: given only a `manifest_sha256` off the URL, it has no
//! pre-existing trusted `Label` to hand `get` -- discovering that label IS part of the job.
//!
//! So [`ObjectSource::get`] is a plain, unauthenticated fetch-by-key returning the object's
//! bytes and its own stored [`Label`], and `crate::core::handle` does its OWN authorization
//! (`av_label::ClearanceLadder::classify` -- the identical shared comparison `StoreClient::
//! get`'s own `authorize_read` delegates to internally, see `crates/av-store/src/labels.rs`'s
//! own doc; never a second, independently-written comparison) at each of the two sites H4
//! names ("per layer" and "per request" -- `crate::refusal`'s own module doc has the full
//! reasoning). This also gives this crate its own distinct, counted refusal at each site,
//! rather than inheriting `av_store::StoreError`'s undifferentiated `OverClearance`/
//! `MarkingNotOnLadder` for both.
//!
//! Hash verification is the same story: `crate::core::handle` recomputes SHA-256 over the
//! bytes [`ObjectSource::get`] returns and compares it itself (against the *requested*
//! `manifest_sha256` for the manifest, and against `TileEntry.sha256` for a tile) -- the
//! exact comparison H4's brief asks for -- rather than trusting whatever hash a `StoreError`
//! variant might have compared against internally.

use std::collections::BTreeMap;
use std::sync::Mutex;

use av_cdm::pb::Label;
use thiserror::Error;

/// One object as [`ObjectSource::get`] returns it: its bytes and its own stored label. No
/// hash and no media type -- callers that need those already have (or compute) them: the
/// media type for a tile comes from its own `TileEntry.media_type`; the media type for a
/// manifest is `crate` config; the hash is recomputed and checked by the caller.
#[derive(Debug, Clone)]
pub struct StoredObject {
    pub bytes: Vec<u8>,
    pub label: Label,
}

/// Every way [`ObjectSource::get`] can fail. Deliberately just two variants: this trait's
/// only job is "fetch bytes by key, or say why not" -- every OTHER refusal this crate makes
/// (a hash mismatch, an over-clearance label, a missing tile address) is decided by
/// `crate::core::handle` itself from a successful [`StoredObject`], not by this trait.
#[derive(Debug, Clone, Error)]
pub enum ObjectSourceError {
    #[error("no object stored under key {key:?}")]
    NotFound { key: String },
    #[error("object store backend error fetching key {key:?}: {detail}")]
    Backend { key: String, detail: String },
}

/// How `crate::core::handle` fetches an object (a manifest, or a tile) by its full,
/// already-prefixed store key. See this module's own doc for why this is a plain,
/// unauthenticated fetch rather than `av_store::StoreClient::get`'s bundled
/// authorize-then-fetch-then-verify contract.
pub trait ObjectSource: std::fmt::Debug + Send + Sync {
    fn get(&self, key: &str) -> Result<StoredObject, ObjectSourceError>;
}

/// An in-memory [`ObjectSource`] for tests: objects registered by [`InMemoryObjectSource::insert`]
/// under a caller-chosen key, fetched back by that same key. Deliberately does **not**
/// derive the key from the bytes' own hash the way a real content-addressed store would --
/// that decoupling is exactly what lets a test register bytes whose SHA-256 does NOT match
/// the key/hash a caller will ask for, to exercise `crate::refusal::TileRefusal`'s hash-
/// mismatch variants without a real store's own key-derivation getting in the way.
#[derive(Debug, Default)]
pub struct InMemoryObjectSource {
    objects: Mutex<BTreeMap<String, StoredObject>>,
}

impl InMemoryObjectSource {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, key: impl Into<String>, bytes: Vec<u8>, label: Label) {
        self.objects.lock().unwrap_or_else(|p| p.into_inner()).insert(key.into(), StoredObject { bytes, label });
    }
}

impl ObjectSource for InMemoryObjectSource {
    fn get(&self, key: &str) -> Result<StoredObject, ObjectSourceError> {
        self.objects.lock().unwrap_or_else(|p| p.into_inner()).get(key).cloned().ok_or_else(|| ObjectSourceError::NotFound { key: key.to_string() })
    }
}

/// The real, `av-store`-backed [`ObjectSource`] -- "one small adapter" (H4's own brief) over
/// [`av_store::StoreClient`]. Two real round trips per [`ObjectSource::get`] call: `head`
/// (unauthenticated -- learns the object's real, server-reported [`Label`]/SHA-256, no bytes)
/// then `get` (this module's own doc explains why `get` needs a `Label` to authorize
/// against before it will return bytes at all). The second call's `caller_clearance`
/// argument is deliberately the object's OWN just-learned marking -- `ClearanceLadder::
/// classify(marking, label_of_the_same_marking)` always passes when `marking` is itself on
/// the ladder (equal rank is allowed, `av_label`'s own doc), so this is a self-authorizing
/// fetch: THIS adapter's job is only to retrieve verified bytes, never to decide whether the
/// real caller may see them -- that decision is `crate::core::handle`'s alone, against the
/// real caller's own clearance, at both the "per layer" and "per request" sites.
///
/// `StoreClient`'s own methods are `async`; this trait's own method is not (mirroring
/// `crates/av-jobs::runner::ObjectSource::fetch`, also synchronous). This adapter bridges the
/// two with `tokio::runtime::Handle::block_on` from inside `block_in_place` -- correct only
/// when called from a multi-thread Tokio runtime (never the current-thread flavor), which is
/// exactly what `crate::server::serve`'s own `#[tokio::main(flavor = "multi_thread")]` binary
/// provides. Documented here rather than hidden: this is the one place in this crate a sync
/// trait meets an async client, and no new dependency (an `async-trait`-shaped crate is not
/// in this workspace's `Cargo.lock`) is worth adding for a single adapter this small.
/// `Debug` is hand-written, not derived: neither `av_store::StoreClient` nor
/// `dyn av_command::clock::Clock` implements it, so `#[derive(Debug)]` cannot -- this impl
/// names the two fields that matter (nothing here is ever printed on a hot path; this is
/// purely so `ObjectSource: std::fmt::Debug`'s own bound is satisfiable).
pub struct StoreObjectSource {
    client: std::sync::Arc<av_store::StoreClient>,
    ladder: std::sync::Arc<av_label::ClearanceLadder>,
    /// The clock `StoreClient`'s own SigV4 signing needs (as `now_unix_secs`) -- injected,
    /// never read from `SystemTime::now()` inside this adapter (rule 7), the same
    /// `av_command::clock::Clock` trait every other clock-consuming type in this workspace
    /// takes. [`av_command::oidc::verify`] wants TAI nanoseconds; SigV4 wants Unix seconds --
    /// [`Self::get`] bridges the two with `av_cdm::time::Tai::to_utc_nanos`, the identical
    /// UTC boundary `crate::core::handle`'s own caller (`crate::server::handle_connection`)
    /// never needs to duplicate, since only this adapter's own outbound S3 calls need it.
    clock: std::sync::Arc<dyn av_command::clock::Clock>,
}

impl std::fmt::Debug for StoreObjectSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoreObjectSource").finish_non_exhaustive()
    }
}

impl StoreObjectSource {
    pub fn new(client: std::sync::Arc<av_store::StoreClient>, ladder: std::sync::Arc<av_label::ClearanceLadder>, clock: std::sync::Arc<dyn av_command::clock::Clock>) -> Self {
        Self { client, ladder, clock }
    }
}

impl ObjectSource for StoreObjectSource {
    fn get(&self, key: &str) -> Result<StoredObject, ObjectSourceError> {
        let placeholder = av_cdm::pb::AssetRef { uri: format!("s3://av-tiles/{key}"), ..Default::default() };
        let now = av_cdm::time::Tai::from_nanos(self.clock.now_tai_ns()).to_utc_nanos() / 1_000_000_000;
        let client = self.client.clone();
        let ladder = self.ladder.clone();
        let key_owned = key.to_string();

        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async move {
                let head = client.head(&placeholder, now).await.map_err(|e| match e {
                    av_store::StoreError::NotFound { .. } => ObjectSourceError::NotFound { key: key_owned.clone() },
                    other => ObjectSourceError::Backend { key: key_owned.clone(), detail: other.to_string() },
                })?;
                // `head` already carries the object's real, server-reported sha256/label
                // (`StoreClient::head`'s own doc) -- no reconstruction needed before handing
                // it straight to `get` as the trusted AssetRef that call's own contract wants.
                let label = head.label.clone().unwrap_or_default();
                let bytes = client.get(&head, &label.marking, &ladder, now).await.map_err(|e| ObjectSourceError::Backend { key: key_owned.clone(), detail: e.to_string() })?;
                Ok(StoredObject { bytes: bytes.to_vec(), label })
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label(marking: &str) -> Label {
        Label { marking: marking.to_string(), caveats: vec![] }
    }

    #[test]
    fn in_memory_source_fetches_what_was_inserted() {
        let source = InMemoryObjectSource::new();
        source.insert("k1", vec![1, 2, 3], label("CUI"));
        let got = source.get("k1").unwrap();
        assert_eq!(got.bytes, vec![1, 2, 3]);
        assert_eq!(got.label.marking, "CUI");
    }

    #[test]
    fn in_memory_source_refuses_an_unregistered_key_as_not_found() {
        let source = InMemoryObjectSource::new();
        let err = source.get("nope").unwrap_err();
        assert!(matches!(err, ObjectSourceError::NotFound { key } if key == "nope"));
    }

    #[test]
    fn in_memory_source_allows_registering_bytes_that_do_not_hash_to_their_own_key() {
        // The decoupling this module's own doc promises: the key is caller-chosen, not
        // derived from the bytes, so a test can build a hash-mismatch fixture directly.
        let source = InMemoryObjectSource::new();
        source.insert("looks-like-a-hash-but-isnt", vec![9, 9, 9], label("UNCLASSIFIED"));
        let got = source.get("looks-like-a-hash-but-isnt").unwrap();
        assert_eq!(got.bytes, vec![9, 9, 9]);
    }
}
