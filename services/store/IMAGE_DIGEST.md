# services/store/IMAGE_DIGEST.md

The H1 object-store tier's image (ADR-003's `store` tier; question 39 as amended by question 256:
an S3 API, served on this host by an Apache-2.0 S3-compatible server chosen by trial). Pulled ONCE,
by digest, on 2026-10-19 (question 154: the pull is the one-time setup step; no test ever pulls).
Question 212(a): every test that starts it compares the local image to the record below before it
trusts it, and reads how to start it from the launch record below (one home for both).

## Image

- Server: Versity S3 Gateway (`versitygw`) v1.9.0, POSIX backend, single-account mode.
- Source repository: https://github.com/versity/versitygw (tag `v1.9.0`, image label
  `org.opencontainers.image.revision` = `ca4c8d30f579a5ac3dd8c91bbfa8340c69dac0ac`).
- Licence: Apache-2.0, read from the project's own `LICENSE` at tag `v1.9.0` (sha256
  `c71d239df91726fc519c6eb72d318ec65820627232b2f796219e87dcf35d0ab4`) and the image's own
  `org.opencontainers.image.licenses` label.
- Registry reference (pull by digest, never by the tag alone):
```
versity/versitygw@sha256:eb7f786fcd91e72194e48fcf1ac16302d686bf8e3a71434bdbc067a6efd3fdc7
```
- Tag this digest carried when it was pulled: `versity/versitygw:v1.9.0` (Docker Hub; the project's
  `.github/workflows/docker.yml` publishes to `versity/versitygw` and `ghcr.io/versity/versitygw`;
  the digest is the OCI image index, so an x86_64 host resolves the same reference to its own
  platform manifest).
- `docker image inspect --format '{{.Id}}'` on this host (Colima, containerd image store, so the
  image id and the index digest are the same string here):
```
sha256:eb7f786fcd91e72194e48fcf1ac16302d686bf8e3a71434bdbc067a6efd3fdc7
```
- Platform manifests in the index: linux/arm64 `sha256:f9768331c89d485fa0990d54aba88e06f3a035a54de7964f85a2882d8537665d`,
  linux/amd64 `sha256:57b45f18ff79cc2fc28aa6eb49920abd96be9786569842b7c5f783cc8ad5a1a4`.

## Launch record (read by the tests; one fenced block per field)

**Container port**
```
7070
```

**Health path** (GET, unauthenticated, answers 200 once the S3 gateway serves)
```
/health
```

**Credential environment** (one line per variable; the tests substitute a per-run generated
`{access_key}` and `{secret_key}`, never a real credential)
```
ROOT_ACCESS_KEY_ID={access_key}
ROOT_SECRET_ACCESS_KEY={secret_key}
```

**Server arguments** (one argument per line, passed after the image reference)
```
--health
/health
posix
/tmp/vgw
```

Why each argument: `--health /health` turns on the unauthenticated health route; `posix /tmp/vgw`
serves buckets as directories under `/tmp/vgw`, which the image creates (`IAM_DIR`/`SETUP_DIR`) and
which lives in the container's own filesystem and dies with it. Object metadata is stored in
extended attributes of that filesystem.

## Why this server (question 256)

Chosen by trial on 2026-10-19 over SeaweedFS 4.48
(`chrislusf/seaweedfs@sha256:4e61d15fd35994cb1e43e1e553dff106794841fd9a99ade2fc8c8bfce4d7872d`, also
Apache-2.0). Both passed every `av-store` and `av-jobs` container test (8 and 2). Both refused a wrong
secret (`SignatureDoesNotMatch`), an unknown key (`InvalidAccessKeyId`) and a request signed an hour
off (`RequestTimeTooSkewed`), each with 403. SeaweedFS drops an `x-amz-meta-*` header whose value is
empty. `av-store` writes `x-amz-meta-av-provenance` empty for a default `Provenance`, so every object
written that way read back as `MetadataMissing`, which gave 7 failures and 15 errors in the
store-dependent Python tests. This server keeps the empty value, and those tests pass (81 passed; the
one skip is the cFS build-artifact check, unrelated). The tables are in `docs/heavy-plan.md`, under
the S3 server round.

Known limitations, recorded:

- The upstream image is built from a tree its own build info calls dirty (`go version -m` on
  `/usr/local/bin/versitygw` reports the module as `v1.9.0+dirty`).
- Object metadata lives in extended attributes of the container's filesystem (measured to work on
  this host's overlayfs).
- The server runs in single-account mode (one root key pair), which is all `av-store` uses.

## History

- **2026-09-15 to 2026-10-19: MinIO.**
  `quay.io/minio/minio@sha256:a1ea29fa28355559ef137d71fc570e508a214ec84ff8083e39bc5428980b015e`
  (tag `RELEASE.2025-04-22T22-12-26Z`, arm64; its `docker image inspect` Id was the same string).
  It is AGPL-3.0, run unmodified as an operational dependency (ADR-003, question 39). It was started
  with `MINIO_ROOT_USER`/`MINIO_ROOT_PASSWORD` and `server /data` on port 9000, with health on
  `GET /minio/health/live`. It came from quay.io because Docker Hub's `minio/minio` did not serve
  these tags (measured 2026-09-15). On 2026-10-18 k3s's kubelet image garbage collection removed the
  local copy (question 255). quay.io then refused the digest anonymously (401), and Docker Hub serves
  no `minio/minio`, so it could not be restored. Question 256 replaced it with the trial above.
