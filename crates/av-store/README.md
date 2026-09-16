# av-store

H1 (`docs/heavy-plan.md`, question 216): an S3-API client over the system OpenSSL, the
content-addressed object store it implements, and the claim-check (`altavista.v1.AssetRef`)
that keeps heavy payload bytes off this platform's hot path.

## The claim-check rule

A heavy payload (imagery, terrain, a point cloud, a mesh) is stored once, under its own
SHA-256, with its handling label and its provenance carried as S3 object metadata. A
hot-track message never carries payload bytes -- it carries an `AssetRef`: the object's URI,
hash, size, media type, label and provenance. `tests/claim_check_hot_path.rs` proves this
mechanically, with a real `cargo tree`: no hot-path crate (`av-ingest`, `av-track`,
`av-command`) depends on this one, at all, even transitively.

See `src/lib.rs`'s crate-level doc for why the object key is the content hash, why this
crate is deliberately unreachable from the hot path, and the crypto rule (ADR-004: SHA-256
only, the system OpenSSL only -- see `deny.toml`'s `[bans] deny` list for the mechanical
enforcement).

## Running the tests

```sh
cargo test -p av-store
```

Every test in this crate today is docker-free: `src/sigv4.rs`'s known-answer tests against
AWS's own published SigV4 vector, `src/keys.rs`/`src/metadata.rs`/`src/claim_check.rs`/
`src/labels.rs`'s unit tests, `src/client.rs`'s error-XML-extractor and host/path-building
tests, and `tests/claim_check_hot_path.rs`'s `cargo tree` proof.

A second task adds this crate's MinIO integration test (a real object store, over the real
S3 wire protocol this crate's `StoreClient` implements). When it lands, it will be
docker-gated the way every docker test in this workspace is: labelled `av.test=1` /
`av.test.run_id=<id>`, taking `av_lockstep::docker::lock_docker_tests()` for its whole body,
and either running for real or skipping with a visible, named reason -- never passing
silently. Until then, `StoreClient::new`'s support for a plain `http://127.0.0.1:<ephemeral>`
endpoint (see `src/client.rs`'s module doc) is the seam that test needs, already in place.
