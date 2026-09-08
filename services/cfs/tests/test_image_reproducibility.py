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
"""
from __future__ import annotations

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


@pytest.mark.skipif(_SKIP_REASON is not None, reason=_SKIP_REASON or "")
def test_two_independent_builds_produce_the_same_image_id() -> None:
    run_id = str(int(time.time()))
    tag_1 = f"altavista-cfs-repro-test-1-{run_id}:local"
    tag_2 = f"altavista-cfs-repro-test-2-{run_id}:local"
    try:
        _docker_build_no_cache(tag_1)
        digest_1 = _image_id(tag_1)
        _docker_build_no_cache(tag_2)
        digest_2 = _image_id(tag_2)
        assert digest_1 == digest_2, (
            f"two independent `docker build --no-cache` runs of {DOCKERFILE} produced DIFFERENT "
            f"image IDs ({digest_1!r} vs {digest_2!r}) -- the image is not reproducible from an "
            "identical build context.\n"
            "\n"
            "DO NOT read this failure as 'the question-182 BUILDDATE/USER/HOSTNAME pins were "
            "removed' without checking first. As of R4.3 this assertion is KNOWN to fail even "
            "with those pins correctly in place, for two further, independent, out-of-scope "
            "reasons measured and root-caused in services/cfs/R4_3_REPORT.md section 4:\n"
            "  (a) 86 of the 143 files under /cfs/cpu1 -- every one of them a cFE/OSAL unit-test "
            "or coverage-harness binary the container never executes -- carry non-deterministic "
            "GNU-linker build-ids (isolated to 34 bytes inside .note.gnu.build-id);\n"
            "  (b) the final stage's `apt-get install ... libc6` layer differs between builds "
            "(unpinned apt package/index state).\n"
            "\n"
            "The question-182 pins ARE confirmed effective for every runtime-relevant artifact: "
            "core-cpu1, every mission .so, the startup script and all 21 utmod modules are "
            "byte-identical across two independent builds. To tell the two causes apart, compare "
            "core-cpu1's own sha256 between the two builds: if THAT differs, the BUILDDATE pin is "
            "genuinely broken (that is the clean signal R4_3_REPORT.md section 5's break test "
            "used); if it matches, you are looking at (a) and/or (b) above, which need an "
            "image-content change beyond the three pinned variables. See IMAGE_DIGEST.md's "
            "'Re-pinned 2026-09-08 (R4.3, question 182)' section."
        )
    finally:
        _docker_rmi(tag_1)
        _docker_rmi(tag_2)
