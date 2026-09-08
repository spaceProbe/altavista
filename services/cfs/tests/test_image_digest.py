"""M23.2 (docs/open-questions.md question 154), rewritten for question 179 (M25.4a):

Building the cFS image needed the network (`third_party/fetch-cfs.sh`'s pinned clone, plus apt
packages) and question 154 permits that ONLY as a one-time build step, never at test/run time.
The old version of this test built the image itself on every run, which both violated that
(cache-hit or not, it still shelled out to `docker build`) and meant a genuine content drift in
the build context surfaced only as "digest doesn't match", with no attribution to *which* file
changed (question 179's own account of the problem).

This version:
  * NEVER runs `docker build`. It only inspects an already-built `altavista-cfs-lockstep:local`
    image. Building it is `services/cfs/build-image.sh`'s job, run by hand, once.
  * Skips VISIBLY (naming `services/cfs/build-image.sh` in the reason) when Docker is
    unavailable, or when the image tag hasn't been built yet -- never a silent skip.
  * On a digest mismatch, attributes it: `services/cfs/IMAGE_CONTEXT_MANIFEST.txt` (also written
    by build-image.sh) records the SHA-256 of every host path the Dockerfile's COPY steps read
    from; this test re-derives that same COPY-path set from the live Dockerfile, re-hashes
    every file, and diffs the result against the manifest -- added / removed / changed paths.
    If that diff comes back EMPTY while the digest still differs, that is reported explicitly
    too: it means nothing the manifest covers moved the digest, so the cause is outside the
    build context entirely (base image resolution, unpinned apt packages, BuildKit, etc.).
  * Separately (not Docker-gated -- a pure file-hash check that must run on every host, CI
    included, with no Docker daemon required): asserts the manifest itself is still accurate,
    i.e. every path it records still exists and still hashes to the value recorded for it. This
    catches manifest/tree drift (e.g. someone edited a COPYed file but forgot to re-run
    build-image.sh) independent of whether anyone has a local image built at all.

Per question 164 (a diagnostic that matched the wrong banner substring and silently never
matched): every assertion here is pinned against a captured real artifact -- the manifest file
`build-image.sh` actually wrote and the Dockerfile's actual COPY lines -- never against a
guess about what those should say.
"""
from __future__ import annotations

import re
import subprocess
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[3]
DOCKERFILE = REPO_ROOT / "services" / "cfs" / "Dockerfile"
DIGEST_DOC = REPO_ROOT / "services" / "cfs" / "IMAGE_DIGEST.md"
MANIFEST_PATH = REPO_ROOT / "services" / "cfs" / "IMAGE_CONTEXT_MANIFEST.txt"
IMAGE_TAG = "altavista-cfs-lockstep:local"
BUILD_SCRIPT = "services/cfs/build-image.sh"


def docker_available() -> bool:
    """Mirrors crates/av-lockstep/src/docker.rs::docker_available exactly: `false` on any
    failure to even launch the `docker` binary, not just a non-zero exit."""
    try:
        result = subprocess.run(["docker", "info"], capture_output=True, timeout=30)
    except (FileNotFoundError, subprocess.TimeoutExpired):
        return False
    return result.returncode == 0


def image_built() -> bool:
    """True only if `docker image inspect` can resolve IMAGE_TAG right now. Assumes
    docker_available() already passed -- if the docker binary itself is missing, this also
    safely returns False rather than raising."""
    try:
        result = subprocess.run(
            ["docker", "image", "inspect", IMAGE_TAG, "--format", "{{.Id}}"],
            capture_output=True,
            timeout=30,
        )
    except (FileNotFoundError, subprocess.TimeoutExpired):
        return False
    return result.returncode == 0


def _compute_skip_reason() -> str | None:
    if not docker_available():
        return (
            "Docker is not available on this host (`docker info` failed) -- this test only "
            f"inspects an already-built image, it never builds one. Run `{BUILD_SCRIPT}` on a "
            "host with Docker to build the image, then re-run this test there."
        )
    if not image_built():
        return (
            f"image {IMAGE_TAG!r} has not been built on this host -- this test only inspects "
            f"an already-built image, it never builds one. Run `{BUILD_SCRIPT}` first, then "
            "re-run this test."
        )
    return None


_SKIP_REASON = _compute_skip_reason()


def recorded_digest() -> str:
    text = DIGEST_DOC.read_text()
    match = re.search(r"```\nsha256:[0-9a-f]{64}\n```", text)
    assert match, f"could not find a recorded sha256 digest in {DIGEST_DOC}"
    return match.group(0).strip("`\n")


def built_digest() -> str:
    inspect = subprocess.run(
        ["docker", "image", "inspect", IMAGE_TAG, "--format", "{{.Id}}"],
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert inspect.returncode == 0, inspect.stderr
    return inspect.stdout.strip()


def sha256_file(path: Path) -> str:
    import hashlib

    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def parse_manifest(text: str) -> dict[str, str]:
    """path -> sha256. Lines starting with '#' and blank lines are ignored (the header
    build-image.sh writes). Format: '<sha256>  <path>' (two spaces), one per line."""
    entries: dict[str, str] = {}
    for lineno, raw in enumerate(text.splitlines(), start=1):
        line = raw.rstrip("\n")
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        parts = line.split(None, 1)
        assert len(parts) == 2, f"{MANIFEST_PATH}:{lineno}: unparsable manifest line: {raw!r}"
        sha, path = parts
        assert re.fullmatch(r"[0-9a-f]{64}", sha), (
            f"{MANIFEST_PATH}:{lineno}: {sha!r} does not look like a sha256 hex digest"
        )
        entries[path] = sha
    return entries


def load_manifest() -> dict[str, str]:
    assert MANIFEST_PATH.exists(), (
        f"{MANIFEST_PATH} does not exist -- run `{BUILD_SCRIPT}` to generate it."
    )
    return parse_manifest(MANIFEST_PATH.read_text())


def parse_dockerfile_copy_srcs() -> list[str]:
    """Re-derive the Dockerfile's host COPY source paths, mirroring build-image.sh's own
    parser (kept independent on purpose so a bug in one is unlikely to be masked by the same
    bug in the other -- see question 164's captured-artifact rule)."""
    srcs: list[str] = []
    for line in DOCKERFILE.read_text().splitlines():
        if not line.startswith("COPY "):
            continue
        rest = line[len("COPY "):].strip()
        tokens = rest.split()
        assert len(tokens) >= 2, f"unparsable COPY line in {DOCKERFILE}: {line!r}"
        first = tokens[0]
        if first.startswith("--from="):
            continue
        assert not first.startswith("--"), (
            f"unrecognized COPY flag {first!r} in {DOCKERFILE}: {line!r} -- extend this parser"
        )
        assert len(tokens) == 2, f"unexpected COPY line shape in {DOCKERFILE}: {line!r}"
        srcs.append(first)
    assert srcs, f"parsed zero host COPY paths out of {DOCKERFILE}"
    return srcs


def compute_current_copy_manifest() -> dict[str, str]:
    """path -> sha256 for every file the Dockerfile's COPY steps currently read from (directories
    expanded recursively), computed fresh from the working tree right now."""
    current: dict[str, str] = {}
    for src in parse_dockerfile_copy_srcs():
        abs_path = REPO_ROOT / src
        if abs_path.is_file():
            current[src] = sha256_file(abs_path)
        elif abs_path.is_dir():
            for file in sorted(abs_path.rglob("*")):
                if file.is_file():
                    rel = file.relative_to(REPO_ROOT).as_posix()
                    current[rel] = sha256_file(file)
        else:
            raise AssertionError(f"COPY source path does not exist on disk: {src}")
    return current


def _diff_manifests(recorded: dict[str, str], current: dict[str, str]) -> str:
    added = sorted(set(current) - set(recorded))
    removed = sorted(set(recorded) - set(current))
    changed = sorted(p for p in set(recorded) & set(current) if recorded[p] != current[p])

    if not added and not removed and not changed:
        return (
            "manifest and current build context agree EXACTLY (no path added, removed, or "
            "changed) -- whatever moved the digest is NOT in the COPYed build context.\n"
            "  The known cause, measured for M25.4a and written up in services/cfs/"
            "IMAGE_DIGEST.md's 'Re-pinned 2026-09-08' section: this image is not reproducible "
            "by construction. third_party/cfs/cfe/cmake/generate_build_env.cmake bakes `date "
            "+%Y%m%d%H%M` into cFE's CONFIGDATA as BUILDDATE unless $BUILDDATE is set, and "
            "services/cfs/Dockerfile sets neither it nor BUILDUSER/BUILDHOST -- so any build "
            "that genuinely re-executes the builder stage (rather than being served whole from "
            "Docker's layer cache) produces a different image ID from identical inputs.\n"
            "  If that is what happened, this is expected drift, not a defect: re-pin. Other "
            "candidates worth excluding first: base image tag resolution (`FROM ubuntu:22.04` "
            "is a floating tag), unpinned apt package versions in the Dockerfile's `apt-get "
            "install` lines, third_party/fetch-cfs.sh's pinned clone, Docker/BuildKit version."
        )

    lines = ["manifest vs. current build context diff:"]
    for p in added:
        lines.append(f"  + added:   {p}  (sha256:{current[p]})")
    for p in removed:
        lines.append(f"  - removed: {p}  (was sha256:{recorded[p]})")
    for p in changed:
        lines.append(f"  ~ changed: {p}  (sha256:{recorded[p]} -> sha256:{current[p]})")
    return "\n".join(lines)


# ---------------------------------------------------------------------------------------------
# Test 1: manifest currency. Pure file-hash check, no Docker involved -- must run everywhere.
# ---------------------------------------------------------------------------------------------
def test_manifest_paths_exist_and_hash_match() -> None:
    recorded = load_manifest()
    removed: list[str] = []
    changed: list[str] = []
    for path, sha in sorted(recorded.items()):
        abs_path = REPO_ROOT / path
        if not abs_path.is_file():
            removed.append(path)
            continue
        actual = sha256_file(abs_path)
        if actual != sha:
            changed.append(f"{path}  (recorded sha256:{sha} -> actual sha256:{actual})")

    if removed or changed:
        lines = [f"{MANIFEST_PATH} is stale ({len(removed)} missing, {len(changed)} changed):"]
        for p in removed:
            lines.append(f"  - missing: {p}")
        for c in changed:
            lines.append(f"  ~ changed: {c}")
        lines.append(f"Re-run `{BUILD_SCRIPT}` to regenerate the manifest for the current tree.")
        pytest.fail("\n".join(lines))


# ---------------------------------------------------------------------------------------------
# Test 2: built image digest vs. recorded pin. Docker-gated, visible skip when not applicable.
# ---------------------------------------------------------------------------------------------
@pytest.mark.skipif(_SKIP_REASON is not None, reason=_SKIP_REASON or "")
def test_image_digest_matches_recorded_value() -> None:
    actual = built_digest()
    expected = recorded_digest()
    if actual == expected:
        return

    recorded_manifest = load_manifest()
    current_manifest = compute_current_copy_manifest()
    diff = _diff_manifests(recorded_manifest, current_manifest)

    pytest.fail(
        f"built image digest {actual!r} does not match {DIGEST_DOC}'s recorded {expected!r}.\n"
        f"{diff}\n"
        f"If this is an intentional change: re-run `{BUILD_SCRIPT}`, then update the recorded "
        f"digest and manifest in {DIGEST_DOC} and {MANIFEST_PATH}."
    )
