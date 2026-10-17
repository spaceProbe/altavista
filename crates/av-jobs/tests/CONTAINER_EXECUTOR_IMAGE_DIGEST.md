# crates/av-jobs/tests/CONTAINER_EXECUTOR_IMAGE_DIGEST.md

The probe image `tests/container.rs` runs one real job inside, for the H3 "container executor"
proof (`docs/heavy-plan.md` H3's last open item). Question 154: the pull is a one-time setup
step outside this test; no test in this file ever pulls. Question 212(a): every image a test
depends on records its digest here, beside the test that uses it, and the test compares the
RUNNING image to this record before it trusts it -- refusing (a visible `SKIPPED` line) rather
than trusting on a mismatch.

## Image

Alpine, pinned by CONTENT ADDRESS (an image index digest), not by a tag. It is the same small
(13.6 MB) image `tests/test_edge_plugin_hardening_alpine.py` pins by the same
digest (`ALPINE_DIGEST`); this file records ITS OWN digest for `crates/av-jobs/tests/container.rs`
specifically, per question 212(a)'s "the recorded digest has exactly one home" -- never shared
with, or read from, any other component's own `IMAGE_DIGEST.md`.

- Registry reference (pull by digest, never by a tag):
```
alpine@sha256:294b683cb724975bec92580e1e685676bd4b50bda910ddb8c51d4cabeaec77e6
```
- `docker image inspect --format '{{.Id}}'` of that reference on this host (2026-10-17; on this
  host's containerd image store the image id IS the index digest):
```
sha256:294b683cb724975bec92580e1e685676bd4b50bda910ddb8c51d4cabeaec77e6
```
- One-time setup (a network fetch, done by a human or the manager, never by a test; already
  satisfied on this host, where `alpine:latest` resolved to this digest on 2026-10-17):
```
docker pull alpine@sha256:294b683cb724975bec92580e1e685676bd4b50bda910ddb8c51d4cabeaec77e6
```

## Why the reference is a digest and not `alpine:latest`

This record used to name the floating tag `alpine:latest` and pin only the image id
(`sha256:28bd5fe8b56d...`, recorded 2026-09-17). A tag names whatever the registry says today:
Colima's kubelet image garbage collector removes unused images in bulk (every image is untagged
within one or two seconds in `altavista-docker-image-events.log`), and the next
`docker pull alpine` or `docker run alpine` by anyone re-fetches the tag at whatever digest it
then has. `alpine:latest` was re-fetched at `sha256:294b683c...` on 2026-09-18, the recorded id
no longer matched, and this test could only skip. Naming the digest removes the dependence: the
image `alpine@sha256:294b...` is either present (the test runs) or absent (a visible, specific
`SKIPPED ... is not built locally` line, fixed by the setup command above); a moved tag can no longer make
a present, correct image look wrong, and nothing floating is trusted.

The digest in the reference above must equal the id recorded below it (the test asserts this, so
a half-edited record fails loudly instead of skipping).

## Running it (the test does this itself, under the host-wide docker lock)

`crates/av-jobs/src/container.rs::ContainerExecutor` runs the job's command inside a container
from this image, hardened per `altavista/container_hardening.py::HARDENING_RUN_FLAGS`, with
`docker run --name <container> --network none --read-only --cap-drop ALL --security-opt
no-new-privileges --user 10001:10001 -v <scratch>/in:/av-job/in:ro -v <scratch>/out:/av-job/out
--label av.job=1 --label av.job.id=<job_id> --label av.test=1 --label av.test.run_id=<id>
alpine@sha256:294b683cb724975bec92580e1e685676bd4b50bda910ddb8c51d4cabeaec77e6 <command>` -- see
that module's own doc comment for the full design. `JobSpec.container_image` is the bare,
tag-free `"alpine"` (matching `av_lockstep::docker::ManagedContainer`'s own `image`/`image_digest`
convention -- e.g. `services/store/IMAGE_DIGEST.md`'s `quay.io/minio/minio`) and
`JobSpec.container_image_digest` is the recorded id above. Both the executor's
`crate::container::digest_gate` and this file's own `gate()` (in `tests/container.rs`, mirroring
`store_tiler.rs`'s identical helper) inspect `<image>@<digest>` -- the digest reference -- and
compare the id it reports to the recorded one. Measured on this host: `docker image inspect
alpine@sha256:294b...` resolves and prints the recorded id; a one-hex-digit change to the digest
makes it fail with "No such image", which both gates turn into a refusal (the test: a visible
`SKIPPED`; the executor: `EXECUTOR_UNAVAILABLE`).
