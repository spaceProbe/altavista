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

Most tests in this crate are docker-free: `src/sigv4.rs`'s known-answer tests against
AWS's own published SigV4 vector, `src/keys.rs`/`src/metadata.rs`/`src/claim_check.rs`/
`src/labels.rs`'s unit tests, `src/client.rs`'s error-XML-extractor and host/path-building
tests, and `tests/claim_check_hot_path.rs`'s `cargo tree` proof.

`tests/s3_store.rs` is the integration proof: it runs a real S3 server in a container and drives
`StoreClient` against it over the real wire protocol (round trip by hash, a corrupted object, an
over-clearance read, an unknown key, a wrong secret or unknown key id, a skewed clock, an idempotent
`ensure_bucket`). Which server, and how to start it, comes from `services/store/IMAGE_DIGEST.md`
(question 256): the pull-by-digest reference and its recorded local id, and a launch record
(container port, health path, credential environment, server arguments) that
`av_lockstep::docker::parse_recorded_launch` reads at test time. The test names no server, so a
different record exercises a different server unchanged. It is docker-gated the way every docker
test in this workspace is: the local image must match the recorded id (it never pulls), the
container is labelled `av.test=1` / `av.test.run_id=<id>`, the test takes
`av_lockstep::docker::lock_docker_tests()` for its whole body, and it either runs for real or skips
with a visible, named reason -- never passing silently.
