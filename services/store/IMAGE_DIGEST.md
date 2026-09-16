# services/store/IMAGE_DIGEST.md

The H1 object-store tier's image (ADR-003's `store` tier; question 39: "S3 API via MinIO").
Pulled ONCE at setup, by digest, on 2026-09-15 (heavy track round 1). Question 154: the pull
is the one-time setup step; no test ever pulls. Question 212(a): every image a test depends
on records its digest here, and the test compares the RUNNING image to this record before it
trusts it.

## Image

- Registry reference (pull by digest, never by the tag alone):
```
quay.io/minio/minio@sha256:a1ea29fa28355559ef137d71fc570e508a214ec84ff8083e39bc5428980b015e
```
- Tag this digest carried when it was pulled: `quay.io/minio/minio:RELEASE.2025-04-22T22-12-26Z`
- `docker image inspect --format '{{.Id}}'` on this host (Colima, containerd image store, so
  the image id and the manifest digest are the same string here):
```
sha256:a1ea29fa28355559ef137d71fc570e508a214ec84ff8083e39bc5428980b015e
```
- Architecture: `arm64` (this host is arm64 Colima; the manifest list also carries amd64 and
  ppc64le, so the P5 kit on an x86_64 host resolves the same tag to a different digest and
  must record its own).

## Why quay.io and not `minio/minio` on Docker Hub

`docker pull minio/minio:...` on this host answers "pull access denied for minio/minio,
repository does not exist or may require 'docker login'" (measured 2026-09-15): MinIO's
Docker Hub namespace no longer serves these tags. `quay.io/minio/minio` is MinIO's own
registry and carries the arm64 manifest.

## Licensing (question 39)

MinIO is AGPL-3.0. ADR-003's ruling stands: an **unmodified operational dependency**, run as
a container and reached over the S3 HTTP API, never linked into any crate. `crates/av-store`
links no MinIO code; it speaks S3 over HTTP with its own SigV4 signer.

## Running it (tests do this themselves, under the host-wide docker lock)

```
docker run -d --label av.test=1 --label av.test.run_id=<id> \
  -e MINIO_ROOT_USER=<user> -e MINIO_ROOT_PASSWORD=<password> \
  -p 127.0.0.1::9000 \
  quay.io/minio/minio@sha256:a1ea29fa28355559ef137d71fc570e508a214ec84ff8083e39bc5428980b015e \
  server /data
```

No bind mount: the container's own filesystem holds `/data` and dies with it. This is
deliberate -- Colima mounts only `$HOME` into its VM, so a bind mount from `/tmp` or
`/private/var` would silently be an empty directory inside the container
(`crates/av-lockstep/src/docker_test_lock.rs`'s own module doc records the same trap).

Health: `GET /minio/health/live` answers `200` once the server is up (measured).
