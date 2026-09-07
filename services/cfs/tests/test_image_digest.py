"""M23.2 (docs/open-questions.md question 154): "the image builds and its recorded digest
matches what was built" -- an actual, runnable test, not the prose in services/cfs/IMAGE_DIGEST.md
alone. Gated on `docker info` succeeding, with a printed skip reason otherwise (M15.3's own
convention -- crates/av-lockstep/src/docker.rs::docker_available -- "a silently-skipped test is a
failure of this brief").

Building here reuses Docker's own layer cache: this test does not repeat the one-time network
window (question 154) unless third_party/cfs or the Dockerfile's own inputs changed since the
last build, in which case a fresh build (network included) is exactly what question 154
authorizes ("tests and runs never touch it" refers to *ordinary* test runs against a
already-built image, not to the one-time act of building the image the digest names).
"""
from __future__ import annotations

import re
import subprocess
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[3]
DOCKERFILE = REPO_ROOT / "services" / "cfs" / "Dockerfile"
DIGEST_DOC = REPO_ROOT / "services" / "cfs" / "IMAGE_DIGEST.md"
IMAGE_TAG = "altavista-cfs-lockstep:local"


def docker_available() -> bool:
    """Mirrors crates/av-lockstep/src/docker.rs::docker_available exactly: `false` on any
    failure to even launch the `docker` binary, not just a non-zero exit."""
    try:
        result = subprocess.run(["docker", "info"], capture_output=True, timeout=30)
    except (FileNotFoundError, subprocess.TimeoutExpired):
        return False
    return result.returncode == 0


def recorded_digest() -> str:
    text = DIGEST_DOC.read_text()
    match = re.search(r"```\nsha256:[0-9a-f]{64}\n```", text)
    assert match, f"could not find a recorded sha256 digest in {DIGEST_DOC}"
    return match.group(0).strip("`\n")


@pytest.mark.skipif(not docker_available(), reason="Docker is not available on this host (`docker info` failed) -- M15.3's own gating convention; this is a visible skip, not a silent one")
def test_image_builds_and_digest_matches_recorded_value() -> None:
    build = subprocess.run(
        ["docker", "build", "-f", str(DOCKERFILE), "-t", IMAGE_TAG, str(REPO_ROOT)],
        capture_output=True,
        text=True,
        timeout=1800,
    )
    assert build.returncode == 0, f"docker build failed:\n{build.stdout[-4000:]}\n{build.stderr[-4000:]}"

    inspect = subprocess.run(["docker", "image", "inspect", IMAGE_TAG, "--format", "{{.Id}}"], capture_output=True, text=True, timeout=30)
    assert inspect.returncode == 0, inspect.stderr
    built_digest = inspect.stdout.strip()

    assert built_digest == recorded_digest(), (
        f"built image digest {built_digest!r} does not match services/cfs/IMAGE_DIGEST.md's recorded "
        f"{recorded_digest()!r} -- if this is an intentional change, rebuild and update that file"
    )
