//! Signed, chained, labelled measurement batches (`docs/edge-plan.md` milestone E1;
//! ADR-004's security boundary and evidence rules).
//!
//! This crate implements the edge track's canonical hash/signature scheme for
//! `altavista.v1.MeasurementBatch` (`proto/altavista/v1/edge.proto`), and the pure
//! per-producer chain verifier the ingest boundary (E3) will run every incoming batch
//! through. It has no network code, no clock reads, and no side effects beyond ordinary
//! heap allocation -- everything here is a pure function of its inputs, so E3/E6 can
//! embed it in a service and this crate's own tests can exercise it without a running
//! service, a filesystem, or the wall clock.
//!
//! # Canonical hash definition
//!
//! Pinned here, in `edge.proto`'s header comment, and tested byte-for-byte by
//! `tests/golden.rs` -- E3 and E6 depend on this definition holding exactly, forever:
//!
//! 1. **Body bytes** = [`prost::Message::encode_to_vec`] of the `MeasurementBatch` with
//!    its own `batch_hash` and `signature` fields cleared, and nothing else changed
//!    ([`hash::canonical_body_bytes`]). Every `map<..>` field anywhere in `av_cdm::pb`
//!    (reached here through `Measurement.meta` and `Provenance.attributes`) is generated
//!    as a `BTreeMap` (`crates/av-cdm/build.rs`'s `.btree_map(["."])`), so two batches
//!    built with the same field values always encode identically regardless of insertion
//!    order -- protobuf's own deterministic-serialization contract, the same guarantee
//!    `crates/av-kernel/src/drm/hash.rs` and `av-dynamics-service/src/evidence.rs` rely
//!    on for their own canonical hashes.
//! 2. **`prev_hash` bytes** = the literal ASCII bytes of [`hash::GENESIS`] for the first
//!    batch of a producer, else the previous batch's 32 raw `batch_hash` bytes. This is
//!    a value the *caller* supplies (it is whatever chain state says comes next); it is
//!    not derived from the batch itself. Note that the `MeasurementBatch.prev_hash`
//!    field is itself part of what body bytes (step 1) encodes -- it is not cleared --
//!    so a batch's declared `prev_hash` is covered by its own signature, not merely
//!    carried alongside it.
//! 3. **`batch_hash`** = `SHA-256(prev_hash_bytes || body_bytes)`, 32 raw bytes
//!    ([`hash::compute_batch_hash`]), via `openssl::sha::sha256` -- the system/Homebrew
//!    OpenSSL, never `sha2`, never `ring` (ADR-004's crypto rule; see
//!    `av-dynamics-service/src/evidence.rs`'s module doc for the precedent this mirrors).
//! 4. **`signature`** = ECDSA P-384 over those exact 32 raw `batch_hash` bytes, DER
//!    encoded (`envelope.proto`'s `SignedBatch.signature` doc comment, verbatim). This
//!    crate uses `openssl::ecdsa::EcdsaSig::sign`/`verify` directly on the 32-byte digest
//!    ([`sign`], [`verify`]) rather than `openssl::sign::Signer`/`Verifier`, which would
//!    apply a *second* message digest internally -- `batch_hash` is already the digest
//!    that gets signed, and re-hashing it would silently produce a signature over the
//!    wrong bytes.
//!
//! ECDSA signatures are **not** byte-reproducible run to run: OpenSSL's ECDSA uses a
//! random nonce per signing operation (it does not implement RFC 6979 deterministic
//! ECDSA), so two calls to [`sign::sign_batch`] over the identical batch and key produce
//! two different, both-valid, DER byte strings. A "golden signature" therefore means
//! "signed once, committed, and asserted to verify forever against the committed public
//! key, and to fail verification the moment either the signature or the signed batch is
//! perturbed by even one bit" -- see `tests/golden.rs` and `tests/fixtures/README.md`.
//! The genuinely byte-pinned goldens are the canonical body bytes and the `batch_hash`
//! hex digest, which *are* deterministic functions of the batch's content.
//!
//! # Modules
//!
//! - [`hash`] -- the four-line definition above, as small pure functions, plus hex
//!   display helpers.
//! - [`sign`] -- load a P-384 EC private key from a PEM file and sign a batch, filling
//!   `batch_hash` and `signature`. Refuses any key that is not EC P-384.
//! - [`verify`] -- load a P-384 EC public key from a PEM (a plain public key, or a
//!   certificate) and verify one batch's `batch_hash`/`signature` against it.
//! - [`policy`] -- [`policy::ProducerPolicy`]: a producer's declared emit label,
//!   clearance (an explicit, ordered clearance ladder, rank = index into it), and
//!   maximum batch age.
//! - [`chain`] -- [`chain::ChainVerifier`]: pure, per-producer chain state (last accepted
//!   sequence/hash, the set of accepted sequences) that turns one incoming batch plus a
//!   [`policy::ProducerPolicy`] plus a caller-supplied `now_tai_ns: i64` into a
//!   `BatchVerdict`, updating that producer's `RejectionCounters`. There is no clock read
//!   and no environment read anywhere in this crate (question 199) -- the caller always
//!   supplies "now" as a plain `i64`, which is what lets E6's disconnection tests drive
//!   staleness deterministically instead of sleeping. [`chain::walk_chain`] is the
//!   sibling pure function: given an already-assembled slice of one producer's batches
//!   (no mutable state, no policy, no clock), it walks the hash chain and signature links
//!   and returns the first defect's sequence, mirroring
//!   `av-dynamics-service/src/evidence.rs::EvidenceLog::verify`'s `ChainVerification`
//!   shape.
//! - [`identity`] -- milestone E2: [`identity::TrustAnchors`] (the two-tier Root as the
//!   *only* trust anchor, ADR-004) and [`identity::verify_identity`], which verifies a
//!   seccert-issued leaf against it at a caller-injected TAI instant and hands back an
//!   [`identity::EdgeIdentity`] -- an EC P-384 public key and SHA-256 fingerprint E1's
//!   [`verify::verify_batch`] and `MeasurementBatch.signer_cert_sha256` consume directly.
//!   [`identity::verify_batch_signed_by`] is the E1/E2 bridge: it checks a batch's
//!   declared `signer_cert_sha256` against a verified identity's own fingerprint before
//!   ever spending a signature verification on it.

pub mod chain;
pub mod hash;
pub mod identity;
pub mod policy;
pub mod sign;
pub mod verify;

/// Generated `altavista.v1` types this crate operates on, re-exported under this crate's
/// own name so callers write `av_edge::pb::MeasurementBatch` without also depending on
/// `av-cdm` directly just to name the type this crate's own public functions take and
/// return.
pub use av_cdm::pb;
