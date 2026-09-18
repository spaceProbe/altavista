# crates/av-jobs/tests/CONTAINER_EXECUTOR_IMAGE_DIGEST.md

The probe image `tests/container.rs` runs one real job inside, for the H3 "container executor"
proof (`docs/heavy-plan.md` H3's last open item). Question 154: the pull is a one-time setup
step outside this test; no test in this file ever pulls. Question 212(a): every image a test
depends on records its digest here, beside the test that uses it, and the test compares the
RUNNING image to this record before it trusts it -- refusing (a visible `SKIPPED` line) rather
than trusting on a mismatch.

## Image

`alpine:latest` -- already present on this host, already relied on as a probe image by
`tests/test_edge_plugin_hardening_alpine.py` (that file's own module doc: "13.6 MB and this
test runs in seconds"). This file records ITS OWN digest for `crates/av-jobs/tests/container.rs`
specifically, per question 212(a)'s "the recorded digest has exactly one home" -- never shared
with, or read from, any other component's own `IMAGE_DIGEST.md`.

- Registry reference (pull by digest, never by the tag alone):
```
alpine:latest
```
- `docker image inspect --format '{{.Id}}'` on this host (2026-09-17):
```
sha256:28bd5fe8b56d1bd048e5babf5b10710ebe0bae67db86916198a6eec434943f8b
```

## Why a floating tag is recorded as the "Registry reference" here, unlike services/store's own

`alpine:latest` is a floating tag, not a digest-qualified reference -- unlike
`services/store/IMAGE_DIGEST.md`'s `quay.io/minio/minio@sha256:...`. This test never pulls
either way (question 154), so the tag only ever resolves against whatever is ALREADY present
locally; the recorded `docker image inspect` id above is what actually pins this test to a
specific, known image, exactly the way `av_lockstep::docker::recorded_digest_gate` is designed
to be used (`image_ref`, `recorded_id`) -- the tag names which local image to inspect, the
recorded id says which content that inspection must return.

## Running it (the test does this itself, under the host-wide docker lock)

`crates/av-jobs/src/container.rs::ContainerExecutor` runs the job's own command inside a
container from this image, hardened per `altavista/container_hardening.py::
HARDENING_RUN_FLAGS`, with `docker run --name <container> --network none --read-only --cap-drop
ALL --security-opt no-new-privileges --user 10001:10001 -v <scratch>/in:/av-job/in:ro -v
<scratch>/out:/av-job/out --label av.job=1 --label av.job.id=<job_id> --label av.test=1 --label
av.test.run_id=<id> alpine@sha256:28bd5fe8b56d1bd048e5babf5b10710ebe0bae67db86916198a6eec434943f8b
<command>` -- see that module's own doc comment for the full design. `JobSpec.container_image`
is the bare, tag-free `"alpine"` (matching `av_lockstep::docker::ManagedContainer`'s own
`image`/`image_digest` convention -- e.g. `services/store/IMAGE_DIGEST.md`'s
`quay.io/minio/minio`, never `quay.io/minio/minio:sometag`); `docker image inspect alpine`
(no tag) resolves to the identical id as `docker image inspect alpine:latest` (verified
directly on this host), so `crate::container::digest_gate` inspects by the bare name while this
file's own `gate()` (in `tests/container.rs`, mirroring `store_tiler.rs`'s identical helper)
inspects `alpine:latest` -- both name the same one local image.
