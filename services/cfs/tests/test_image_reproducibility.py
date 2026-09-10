"""services/cfs/tests/test_image_reproducibility.py (R4.3, docs/open-questions.md question 182):

Question 182's own decision: "pin all three [BUILDDATE, BUILDUSER, BUILDHOST] in the Dockerfile
to fixed values recorded in IMAGE_DIGEST.md, re-pin once, and add a test that two consecutive
builds from the same manifest give the same digest." This is that test: it verifies that two
INDEPENDENT builds of services/cfs/Dockerfile from an identical build context produce the same
content-addressed image ID. See services/cfs/IMAGE_DIGEST.md's "Re-pinned 2026-09-08 (R4.3,
question 182)" section for the fix this test guards (fixed `ENV BUILDDATE=...` / `ENV USER=...`
/ `ENV HOSTNAME=...` in the Dockerfile's builder stage -- NOT `ENV BUILDUSER=...` / `ENV
BUILDHOST=...`, which cFE's own third_party/cfs/cfe/cmake/generate_build_env.cmake never reads;
that file's `set(BUILDHOST $ENV{HOSTNAME})` / `set(BUILDUSER $ENV{USER})` read the env vars
named HOSTNAME/USER, not BUILDHOST/BUILDUSER).

Two full builds are genuinely expensive -- on this host, each `docker build --no-cache` of this
Dockerfile runs 10+ minutes (measured; see services/cfs/R4_3_REPORT.md's timing section) -- so
running this on every `pytest -q` invocation, let alone in CI on every commit, is not viable.
This test is gated behind TWO conditions, BOTH required to actually run:

  1. Docker must be available (`docker info` succeeds) -- reusing test_image_digest.py's own
     `docker_available()` helper directly (imported from that file, not reimplemented), so the
     two Docker-availability gates in this test suite cannot silently drift apart.
  2. The environment variable `AV_CFS_RUN_REPRO_BUILD` must be set to a truthy value (`1`,
     `true`, or `yes`, case-insensitive).

**The DEFAULT test suite SKIPS this test.** `pytest -q services/cfs/tests/` and CI do not build
anything twice unless a human or a CI job deliberately sets `AV_CFS_RUN_REPRO_BUILD=1`. Do not
read this test's presence in the suite as evidence it runs automatically -- it does not. To
actually run it (from the repository root):

    AV_CFS_RUN_REPRO_BUILD=1 .venv/bin/python -m pytest -q \
        services/cfs/tests/test_image_reproducibility.py

When it runs, it builds `services/cfs/Dockerfile` TWICE, each via `docker build --no-cache -f
services/cfs/Dockerfile -t <disposable tag> <repo root>`. `--no-cache` is required: an ordinary
cached build would trivially reuse the same layers (and hence the same image ID) without
re-executing anything, proving nothing about reproducibility -- `--no-cache` forces the builder
stage, and therefore cFE's `generate_build_env.cmake`, to genuinely re-execute on both builds.
Both builds use disposable, timestamped tags (never `altavista-cfs-lockstep:local`), so this
test can never clobber the officially recorded image or `services/cfs/IMAGE_CONTEXT_MANIFEST.
txt`; both disposable images are removed (`docker rmi -f`) in a `finally` block regardless of
outcome. Each build uses the network (apt-get, `third_party/fetch-cfs.sh`'s pinned clone), the
same as `services/cfs/build-image.sh` -- acceptable here only because this test is opt-in and
never runs by default, so it is not an additional standing network window in the test suite;
running it is a deliberate, by-hand action exactly like running `build-image.sh` itself.

Per question 164's captured-artifact rule, the comparison here is between two artifacts this
test itself just built, not a guess about what they should contain.

Amended for docs/open-questions.md question 185 (round 5, and its own 2026-09-08 amendment "after
the manager's review"): three image-content changes landed on top of the question-182 pins --
cFE's unit-test/coverage build turned off (services/cfs/build/targets.cmake), `-Wl,--build-id=
none` added to the native_std link flags (services/cfs/build/global_build_options.cmake), and
both Dockerfile stages pinned to one digest-identified `ubuntu:22.04` base with the final stage's
`apt-get install libc6` removed entirely. Two checks exist below; question 185's amendment is
explicit that the second must never narrow or replace the first:

  1. The RUNTIME-CONTENT HASH (question 185's own term): SHA-256 over the sorted
     "<path> <sha256>" lines for every file this image actually ships and runs --
     `/cfs/av-lockstep-shim`, `/cfs/container-entrypoint.sh`, and every file under `/cfs/cpu1`
     (after the unit-tests-off fix, that directory should hold nothing else). See
     services/cfs/IMAGE_DIGEST.md's own "Runtime-content hash" section for the canonical
     definition this file's `runtime_content_hash()` independently re-implements (question 164's
     captured-artifact precedent for TWO independent implementations of one defined algorithm --
     services/cfs/build-image.sh has the other, in bash). This is the STANDING, ALWAYS-ON,
     HARD-ASSERTED reproducibility guarantee (question 190's decision): it must hold even though
     the whole-image digest below no longer is asserted, and it is never weakened.
  2. The WHOLE-IMAGE DIGEST (the original, question-182-era assertion): question 185 (R5.3) found
     a third, OCI-layer-shaped non-determinism cause in the multi-file `COPY --from=builder
     .../cpu1 /cfs/cpu1` layer, with the runtime-content hash already matching and the file-level
     diff below showing NO file under `/cfs` differs -- i.e. the mismatch is confined to image
     metadata/layer history, not file content. Question 190 authorized exactly one bounded
     experiment (BuildKit + `SOURCE_DATE_EPOCH` + a deterministic tar for that layer) to try to
     close that gap, and retirement of this assertion to REPORTED-NOT-ASSERTED if it didn't.
     R6.4 ran that experiment (see `services/cfs/R6_4_REPORT.md` section 3): this host has ZERO
     BuildKit capability (Docker CLI's `buildx` plugin component is entirely absent and cannot be
     installed without network, which this task forbids) -- there is no configuration on this
     host that can even attempt `SOURCE_DATE_EPOCH`/deterministic-tar, so the gap could not be
     closed. Per question 190's own decision, this assertion is now **retired to
     reported-not-asserted**: both digests and, when they differ, the same dynamic per-file diff
     as before are always printed, but a mismatch no longer fails this test. See
     `services/cfs/R6_4_REPORT.md` section 3 and `services/cfs/IMAGE_DIGEST.md` for the full
     record.
"""
from __future__ import annotations

import hashlib
import importlib.util
import os
import subprocess
import time
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[3]
DOCKERFILE = REPO_ROOT / "services" / "cfs" / "Dockerfile"
OPT_IN_VAR = "AV_CFS_RUN_REPRO_BUILD"
BUILD_SCRIPT = "services/cfs/build-image.sh"
BUILD_TIMEOUT_S = 3600  # generous: measured builds run ~10-20 min on this host

# --- Reuse test_image_digest.py's own docker_available() gate helper (loaded from that file by
# path, since services/cfs/tests/ is not a package) rather than writing a second one. ------------
_digest_test_path = Path(__file__).resolve().parent / "test_image_digest.py"
_spec = importlib.util.spec_from_file_location("_r4_3_test_image_digest", _digest_test_path)
assert _spec is not None and _spec.loader is not None
_test_image_digest = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_test_image_digest)
docker_available = _test_image_digest.docker_available


def _opted_in() -> bool:
    return os.environ.get(OPT_IN_VAR, "").strip().lower() in ("1", "true", "yes")


def _compute_skip_reason() -> str | None:
    if not docker_available():
        return (
            "Docker is not available on this host (`docker info` failed) -- this test builds "
            f"the image twice from scratch; see `{BUILD_SCRIPT}` for the normal one-time build "
            "path used to produce and pin the actual recorded image."
        )
    if not _opted_in():
        return (
            f"{OPT_IN_VAR} is not set -- this test performs two full `docker build --no-cache` "
            "runs of services/cfs/Dockerfile (each 10+ minutes on this host, and each uses the "
            f"network, same as `{BUILD_SCRIPT}`), so the default suite skips it. Set "
            f"{OPT_IN_VAR}=1 to opt in and actually run it."
        )
    return None


_SKIP_REASON = _compute_skip_reason()


def _docker_build_no_cache(tag: str) -> None:
    result = subprocess.run(
        ["docker", "build", "--no-cache", "-f", str(DOCKERFILE), "-t", tag, str(REPO_ROOT)],
        capture_output=True,
        text=True,
        timeout=BUILD_TIMEOUT_S,
    )
    assert result.returncode == 0, (
        f"docker build --no-cache -t {tag} -f {DOCKERFILE} failed (exit {result.returncode}):\n"
        f"--- stdout (last 4000 chars) ---\n{result.stdout[-4000:]}\n"
        f"--- stderr (last 4000 chars) ---\n{result.stderr[-4000:]}"
    )


def _image_id(tag: str) -> str:
    result = subprocess.run(
        ["docker", "image", "inspect", tag, "--format", "{{.Id}}"],
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert result.returncode == 0, result.stderr
    return result.stdout.strip()


def _docker_rmi(tag: str) -> None:
    subprocess.run(["docker", "rmi", "-f", tag], capture_output=True, text=True, timeout=60)


# --- Runtime-content hash (question 185): see this module's own docstring amendment and
# services/cfs/IMAGE_DIGEST.md's "Runtime-content hash" section for the canonical definition.
# This is an independent re-implementation of services/cfs/build-image.sh's own (bash) version --
# question 164's captured-artifact precedent for keeping two implementations of one algorithm
# separate, so a bug in one is unlikely to be masked by the same bug in the other. -----------------
RUNTIME_CONTENT_SHELL_CMD = (
    "find /cfs/cpu1 -type f -exec sha256sum {} + ; "
    "sha256sum /cfs/av-lockstep-shim /cfs/container-entrypoint.sh"
)


def _docker_run_capture(tag: str, shell_cmd: str, timeout: int = 120) -> str:
    result = subprocess.run(
        ["docker", "run", "--rm", "--entrypoint", "sh", tag, "-c", shell_cmd],
        capture_output=True,
        text=True,
        timeout=timeout,
    )
    assert result.returncode == 0, (
        f"`docker run --rm --entrypoint sh {tag} -c {shell_cmd!r}` failed "
        f"(exit {result.returncode}): {result.stderr}"
    )
    return result.stdout


def _parse_sha256sum_output(stdout: str) -> dict[str, str]:
    """coreutils `sha256sum` output ('<sha256>  <path>', two spaces) -> {path: sha256}."""
    hashes: dict[str, str] = {}
    for line in stdout.splitlines():
        if not line.strip():
            continue
        sha, path = line.split(None, 1)
        hashes[path] = sha
    return hashes


def runtime_content_hash(tag: str) -> str:
    """SHA-256 over the sorted "<path> <sha256>" lines for every file this image's
    runtime-content set contains: /cfs/av-lockstep-shim, /cfs/container-entrypoint.sh, and every
    file under /cfs/cpu1 (after question 185's unit-tests-off fix, that directory should hold
    nothing but runtime-relevant artifacts). Returns the bare hex digest (no "sha256:" prefix)."""
    hashes = _parse_sha256sum_output(_docker_run_capture(tag, RUNTIME_CONTENT_SHELL_CMD))
    lines = sorted(f"{path} {sha}" for path, sha in hashes.items())
    blob = ("\n".join(lines) + "\n").encode("utf-8")
    return hashlib.sha256(blob).hexdigest()


def _all_cfs_file_hashes(tag: str) -> dict[str, str]:
    """path -> sha256 for EVERY file under /cfs (the only directory either Dockerfile stage
    writes to beyond the pinned, now byte-identical-across-stages base image -- see
    services/cfs/Dockerfile's own final-stage comment). Used only for mismatch attribution when
    the whole-image digest differs, so a failure names which files changed."""
    return _parse_sha256sum_output(_docker_run_capture(tag, "find /cfs -type f -exec sha256sum {} +"))


def _is_runtime_content_path(path: str) -> bool:
    return path in ("/cfs/av-lockstep-shim", "/cfs/container-entrypoint.sh") or path.startswith(
        "/cfs/cpu1/"
    )


def _describe_cfs_file_diff(hashes_1: dict[str, str], hashes_2: dict[str, str]) -> str:
    added = sorted(set(hashes_2) - set(hashes_1))
    removed = sorted(set(hashes_1) - set(hashes_2))
    changed = sorted(p for p in set(hashes_1) & set(hashes_2) if hashes_1[p] != hashes_2[p])
    if not added and not removed and not changed:
        return (
            "  no file under /cfs differs between the two images at all -- the whole-image "
            "digest difference is NOT attributable to any shipped file's content; look at image "
            "metadata or layer history instead (`docker history --no-trunc <tag>`)."
        )
    lines = []
    for p in added:
        kind = "runtime" if _is_runtime_content_path(p) else "NON-runtime"
        lines.append(f"  + added:   {p}  [{kind}]  (sha256:{hashes_2[p]})")
    for p in removed:
        kind = "runtime" if _is_runtime_content_path(p) else "NON-runtime"
        lines.append(f"  - removed: {p}  [{kind}]  (was sha256:{hashes_1[p]})")
    for p in changed:
        kind = "runtime" if _is_runtime_content_path(p) else "NON-runtime"
        lines.append(
            f"  ~ changed: {p}  [{kind}]  (sha256:{hashes_1[p]} -> sha256:{hashes_2[p]})"
        )
    return "\n".join(lines)


@pytest.mark.skipif(_SKIP_REASON is not None, reason=_SKIP_REASON or "")
def test_two_independent_builds_produce_the_same_image_id() -> None:
    run_id = str(int(time.time()))
    tag_1 = f"altavista-cfs-repro-test-1-{run_id}:local"
    tag_2 = f"altavista-cfs-repro-test-2-{run_id}:local"
    try:
        _docker_build_no_cache(tag_1)
        digest_1 = _image_id(tag_1)
        rc_hash_1 = runtime_content_hash(tag_1)
        _docker_build_no_cache(tag_2)
        digest_2 = _image_id(tag_2)
        rc_hash_2 = runtime_content_hash(tag_2)

        print(f"whole-image digest 1: sha256:{digest_1}")
        print(f"whole-image digest 2: sha256:{digest_2}")
        print(f"runtime-content hash 1: sha256:{rc_hash_1}")
        print(f"runtime-content hash 2: sha256:{rc_hash_2}")

        # Assertion 1 (question 185, always-on, never narrowed to replace assertion 2 below):
        # the runtime-content hash -- every file this image actually ships and runs -- must be
        # EQUAL across two independent builds.
        if rc_hash_1 != rc_hash_2:
            diff = _describe_cfs_file_diff(_all_cfs_file_hashes(tag_1), _all_cfs_file_hashes(tag_2))
            pytest.fail(
                "runtime-content hash DIFFERS between two independent `docker build --no-cache` "
                f"runs (sha256:{rc_hash_1} vs sha256:{rc_hash_2}) -- this hash covers every file "
                "the image actually ships and runs (/cfs/av-lockstep-shim, "
                "/cfs/container-entrypoint.sh, and everything under /cfs/cpu1; see "
                "IMAGE_DIGEST.md's 'Runtime-content hash' section for the exact definition) and "
                "must be equal; it is never weakened or narrowed to make this pass.\n"
                f"Files differing under /cfs (runtime-set membership marked):\n{diff}"
            )

        # Check 2 (question 190, RETIRED to reported-not-asserted -- see this module's own
        # docstring amendment and services/cfs/R6_4_REPORT.md section 3 for the full record):
        # the whole-image digest was the original, question-182-era hard assertion, but R5.3
        # found it stays non-reproducible for a third, OCI-layer-shaped reason (the multi-file
        # `COPY --from=builder .../cpu1 /cfs/cpu1` layer) even with the runtime-content hash
        # matching and no file under /cfs differing -- a metadata/layer-history-only gap. Question
        # 190 authorized one bounded BuildKit + SOURCE_DATE_EPOCH experiment to try to close it,
        # with retirement to reported-not-asserted if it didn't; R6.4 found this host has NO
        # BuildKit capability at all (the `buildx` CLI plugin component is absent and cannot be
        # installed without network), so the experiment could not even be attempted, let alone
        # close the gap. This is therefore reported, never asserted -- a whole-image mismatch is
        # NOT a test failure. The runtime-content hash above remains the one hard-asserted,
        # always-on reproducibility guarantee.
        if digest_1 != digest_2:
            print(
                f"whole-image digest DIFFERS between two independent `docker build --no-cache` "
                f"runs of {DOCKERFILE} ({digest_1!r} vs {digest_2!r}) even though the "
                "runtime-content hash MATCHED (sha256:" + rc_hash_1 + ") -- every file the image "
                "actually ships and runs is byte-identical between the two builds, so this "
                "difference is confined to something outside the runtime-content set (or to "
                "image metadata/layer history, not file content at all). This is REPORTED, NOT "
                "ASSERTED (docs/open-questions.md question 190 -- see services/cfs/R6_4_REPORT.md "
                "section 3 for why the one bounded BuildKit experiment question 190 allowed could "
                "not close this gap on this host). Files differing under /cfs (runtime-set "
                "membership marked; empty if the cause is metadata-only):\n"
                + _describe_cfs_file_diff(_all_cfs_file_hashes(tag_1), _all_cfs_file_hashes(tag_2))
            )
        else:
            print(f"whole-image digest MATCHED: sha256:{digest_1}")
    finally:
        _docker_rmi(tag_1)
        _docker_rmi(tag_2)
