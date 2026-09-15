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

Amended for docs/open-questions.md question 179's unindexed amendment (2026-09-08, recorded
right after question 183 in that file) and its restatement right after question 183 itself:
`services/cfs/bin/av-lockstep-shim` is a compiled cross-build artifact, deliberately git-ignored
(`.gitignore` line 35) rather than tracked, so it is present on a host that has run this
Dockerfile's own header-documented cross-build recipe and absent on a clean checkout that has
not. `services/cfs/IMAGE_CONTEXT_MANIFEST.txt` still lists it (its bytes are still part of the
image's build context when present), but `build-image.sh` now marks any manifest entry whose
path `git check-ignore` reports as ignored with a third token, `BUILD_ARTIFACT` -- a property of
the path (is it git-ignored?), not a hardcoded filename list living a second time in this file.
`test_manifest_paths_exist_and_hash_match` below verifies a BUILD_ARTIFACT entry's hash only when
the file is present, and skips VISIBLY (naming the file and pointing at the Dockerfile's own
rebuild recipe) when it is not; every other (non-build-artifact) manifest entry is still verified
strictly and a mismatch or a missing non-build-artifact entry still fails the test outright.

Round 5 (docs/edge-plan.md, question 210) extended that same BUILD_ARTIFACT convention to the
DIAGNOSTIC half of this file, which had never had it. `compute_current_copy_manifest` used to
raise `AssertionError("COPY source path does not exist on disk: services/cfs/bin/
av-lockstep-shim")` for exactly the path `test_manifest_paths_exist_and_hash_match` already knew
to tolerate -- so on a real digest mismatch, on any checkout that has not run the cross-build,
the operator saw that assertion INSTEAD of the message naming the digest that moved and what in
the build context moved it. (The lead's own acceptance gate hit this.) A COPY source the manifest
marks BUILD_ARTIFACT is now allowed to be absent: it comes back as an absent-build-artifact path,
is reported on its own explicitly labelled `! absent build artifact:` line, and is deliberately
kept OUT of the diff's `removed` set so it can never misread as real content loss. Any other
missing COPY source is still a hard failure with the same message it always had -- the tests at
the bottom of this file pin both halves, including that negative control.

Per question 164 (a diagnostic that matched the wrong banner substring and silently never
matched): every assertion here is pinned against a captured real artifact -- the manifest file
`build-image.sh` actually wrote and the Dockerfile's actual COPY lines -- never against a
guess about what those should say.
"""
from __future__ import annotations

import re
import subprocess
from collections.abc import Sequence
from pathlib import Path

import pytest

from altavista.docker_test_lock import lock_docker_tests

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


def parse_manifest(text: str) -> tuple[dict[str, str], set[str]]:
    """(path -> sha256, set of paths marked BUILD_ARTIFACT). Lines starting with '#' and blank
    lines are ignored (the header build-image.sh writes). Format: '<sha256>  <path>' (two
    spaces), optionally followed by a third token 'BUILD_ARTIFACT' for a path git-ignores (see
    this module's own docstring amendment)."""
    entries: dict[str, str] = {}
    build_artifacts: set[str] = set()
    for lineno, raw in enumerate(text.splitlines(), start=1):
        line = raw.rstrip("\n")
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        parts = line.split(None, 2)
        assert len(parts) in (2, 3), f"{MANIFEST_PATH}:{lineno}: unparsable manifest line: {raw!r}"
        sha, path = parts[0], parts[1]
        assert re.fullmatch(r"[0-9a-f]{64}", sha), (
            f"{MANIFEST_PATH}:{lineno}: {sha!r} does not look like a sha256 hex digest"
        )
        if len(parts) == 3:
            assert parts[2] == "BUILD_ARTIFACT", (
                f"{MANIFEST_PATH}:{lineno}: unrecognized third token {parts[2]!r} in {raw!r} "
                "-- only 'BUILD_ARTIFACT' is recognized, extend this parser before proceeding"
            )
            build_artifacts.add(path)
        entries[path] = sha
    return entries, build_artifacts


def load_manifest() -> tuple[dict[str, str], set[str]]:
    assert MANIFEST_PATH.exists(), (
        f"{MANIFEST_PATH} does not exist -- run `{BUILD_SCRIPT}` to generate it."
    )
    return parse_manifest(MANIFEST_PATH.read_text())


def parse_dockerfile_copy_srcs(dockerfile: Path = DOCKERFILE) -> list[str]:
    """Re-derive the Dockerfile's host COPY source paths, mirroring build-image.sh's own
    parser (kept independent on purpose so a bug in one is unlikely to be masked by the same
    bug in the other -- see question 164's captured-artifact rule)."""
    srcs: list[str] = []
    for line in dockerfile.read_text().splitlines():
        if not line.startswith("COPY "):
            continue
        rest = line[len("COPY "):].strip()
        tokens = rest.split()
        assert len(tokens) >= 2, f"unparsable COPY line in {dockerfile}: {line!r}"
        first = tokens[0]
        if first.startswith("--from="):
            continue
        assert not first.startswith("--"), (
            f"unrecognized COPY flag {first!r} in {dockerfile}: {line!r} -- extend this parser"
        )
        assert len(tokens) == 2, f"unexpected COPY line shape in {dockerfile}: {line!r}"
        srcs.append(first)
    assert srcs, f"parsed zero host COPY paths out of {dockerfile}"
    return srcs


def compute_current_copy_manifest(
    *,
    repo_root: Path = REPO_ROOT,
    dockerfile: Path = DOCKERFILE,
    build_artifacts: frozenset[str] = frozenset(),
) -> tuple[dict[str, str], list[str]]:
    """(path -> sha256, sorted list of absent build-artifact paths) for the Dockerfile's COPY
    steps, computed fresh from the working tree right now (directories expanded recursively).

    Mirrors test_manifest_paths_exist_and_hash_match's own convention (question 179's
    amendment): a COPY source path that `build_artifacts` marks as a compiled, git-ignored
    cross-build artifact is allowed to be absent -- reported back in the second element rather
    than raising, since it is expected on a checkout that has not run the cross-build step. Any
    OTHER missing COPY source is still a hard failure, unchanged from before.

    `repo_root` / `dockerfile` default to this module's real paths; they're parameters (rather
    than always reading the globals) purely so tests can point this at a synthetic tmp_path tree
    without touching the real checkout.
    """
    current: dict[str, str] = {}
    absent_build_artifacts: list[str] = []
    for src in parse_dockerfile_copy_srcs(dockerfile=dockerfile):
        abs_path = repo_root / src
        if abs_path.is_file():
            current[src] = sha256_file(abs_path)
        elif abs_path.is_dir():
            for file in sorted(abs_path.rglob("*")):
                if file.is_file():
                    rel = file.relative_to(repo_root).as_posix()
                    current[rel] = sha256_file(file)
        elif src in build_artifacts:
            absent_build_artifacts.append(src)
        else:
            raise AssertionError(f"COPY source path does not exist on disk: {src}")
    return current, sorted(absent_build_artifacts)


def _absent_build_artifact_lines(absent_build_artifacts: list[str]) -> list[str]:
    return [
        f"  ! absent build artifact: {p}  (compiled, git-ignored cross-build output -- expected "
        "on a checkout that has not run the cross-build step; not a build-context change -- see "
        "services/cfs/Dockerfile's own header comment for the exact rebuild recipe)"
        for p in absent_build_artifacts
    ]


def _diff_manifests(
    recorded: dict[str, str],
    current: dict[str, str],
    absent_build_artifacts: Sequence[str] = (),
) -> str:
    added = sorted(set(current) - set(recorded))
    removed = sorted(set(recorded) - set(current) - set(absent_build_artifacts))
    changed = sorted(p for p in set(recorded) & set(current) if recorded[p] != current[p])

    if not added and not removed and not changed:
        if not absent_build_artifacts:
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
        lines = [
            "manifest and current build context agree EXACTLY on every path present on this "
            "host (no path added, removed, or changed) -- whatever moved the digest is NOT in "
            "the part of the COPYed build context that is present here.",
        ]
        lines.extend(_absent_build_artifact_lines(absent_build_artifacts))
        lines.append(
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
        return "\n".join(lines)

    lines = ["manifest vs. current build context diff:"]
    lines.extend(_absent_build_artifact_lines(absent_build_artifacts))
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
    recorded, build_artifacts = load_manifest()
    removed: list[str] = []
    changed: list[str] = []
    skipped_build_artifacts: list[str] = []
    for path, sha in sorted(recorded.items()):
        abs_path = REPO_ROOT / path
        if not abs_path.is_file():
            if path in build_artifacts:
                # A build artifact is verified only when present (docs/open-questions.md
                # question 179's unindexed amendment) -- absence here is expected on a clean
                # checkout that has not run the cross-build step, not a manifest/tree drift.
                skipped_build_artifacts.append(path)
                continue
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

    if skipped_build_artifacts:
        lines = [
            f"{len(skipped_build_artifacts)} build-artifact manifest "
            f"{'entry is' if len(skipped_build_artifacts) == 1 else 'entries are'} not present "
            "on this checkout -- every other (non-build-artifact) manifest entry above was "
            "verified strictly and matched:",
        ]
        for p in skipped_build_artifacts:
            lines.append(
                f"  - {p}: absent. This is a compiled, git-ignored build artifact "
                f"(`git check-ignore {p}` succeeds), not tracked in git -- see "
                "services/cfs/Dockerfile's own header comment (the paragraph starting "
                "\"The shim binary itself ... is PREBUILT, not compiled by this Dockerfile\") "
                "for the exact three-command cross-build recipe that produces it, then re-run "
                f"`{BUILD_SCRIPT}` to regenerate this manifest with it present."
            )
        pytest.skip("\n".join(lines))


# ---------------------------------------------------------------------------------------------
# Test 2: built image digest vs. recorded pin. Docker-gated, visible skip when not applicable.
# ---------------------------------------------------------------------------------------------
@pytest.mark.skipif(_SKIP_REASON is not None, reason=_SKIP_REASON or "")
def test_image_digest_matches_recorded_value() -> None:
    # Question 207: this test only ever READS (`docker image inspect`) -- it never builds,
    # tags, or removes anything, and `altavista-cfs-lockstep:local` itself never carries
    # `av.test` (that label is reserved for TEST-created resources; this is a persistent build
    # artifact `services/cfs/build-image.sh` produces by hand). So it is not exposed to
    # question 207's actual race the way a labelled-resource test is. It still takes the
    # host-wide lock for its whole body anyway, for the same reason every Docker-gated test in
    # this workspace now does: a concurrent `docker rmi`/prune from ANY other Docker-gated
    # process on this host, however unlikely to target this exact image, is a race this test has
    # no need to ever run concurrently with.
    with lock_docker_tests():
        _run_image_digest_matches_recorded_value()


def _run_image_digest_matches_recorded_value() -> None:
    actual = built_digest()
    expected = recorded_digest()
    if actual == expected:
        return

    recorded_manifest, build_artifacts = load_manifest()
    current_manifest, absent_build_artifacts = compute_current_copy_manifest(
        build_artifacts=frozenset(build_artifacts)
    )
    diff = _diff_manifests(recorded_manifest, current_manifest, absent_build_artifacts)

    pytest.fail(
        f"built image digest {actual!r} does not match {DIGEST_DOC}'s recorded {expected!r}.\n"
        f"{diff}\n"
        f"If this is an intentional change: re-run `{BUILD_SCRIPT}`, then update the recorded "
        f"digest and manifest in {DIGEST_DOC} and {MANIFEST_PATH}."
    )


# ---------------------------------------------------------------------------------------------
# Test 3: the question-179 convention applied to compute_current_copy_manifest / _diff_manifests
# themselves. Pure file/string functions, no Docker involved -- must run everywhere, and must
# pass whether or not services/cfs/bin/av-lockstep-shim actually exists on this host, so it
# drives the real functions against a synthetic tmp_path tree rather than the real checkout.
# ---------------------------------------------------------------------------------------------
def test_missing_build_artifact_reported_in_diff_not_raised(tmp_path: Path) -> None:
    """A COPY source path the manifest marks BUILD_ARTIFACT is allowed to be absent -- it must
    come back from compute_current_copy_manifest() as an absent build artifact, not raise, and
    _diff_manifests() must name it and label it as a build artifact rather than folding it into
    `removed` (which would misread as real content loss) or silently vanishing from an
    'agree EXACTLY' verdict (which would misreport the comparison as covering everything)."""
    repo_root = tmp_path / "repo"
    (repo_root / "bin").mkdir(parents=True)
    dockerfile = repo_root / "Dockerfile"
    dockerfile.write_text(
        "FROM ubuntu:22.04\nCOPY bin/av-lockstep-shim /opt/bin/av-lockstep-shim\n"
    )
    # bin/av-lockstep-shim is intentionally never created under repo_root -- the dir exists
    # (mirroring a real git checkout, where services/cfs/bin/ itself is not git-ignored, only
    # the compiled binary inside it is) but the file the COPY line names does not.

    fake_sha = "0" * 64
    manifest_text = f"# header\n{fake_sha}  bin/av-lockstep-shim  BUILD_ARTIFACT\n"
    recorded, build_artifacts = parse_manifest(manifest_text)
    assert build_artifacts == {"bin/av-lockstep-shim"}

    current, absent = compute_current_copy_manifest(
        repo_root=repo_root, dockerfile=dockerfile, build_artifacts=frozenset(build_artifacts)
    )
    assert current == {}
    assert absent == ["bin/av-lockstep-shim"]

    diff = _diff_manifests(recorded, current, absent)
    assert "bin/av-lockstep-shim" in diff
    assert "build artifact" in diff.lower()
    # It must still say the context otherwise agrees (nothing present changed) -- not claim a
    # content change that did not happen.
    assert "agree EXACTLY" in diff
    # It must NOT read as a `removed` (content-loss) diff entry -- "removed" still legitimately
    # appears in the prose ("no path added, removed, or changed"), so check the entry marker.
    assert "- removed:" not in diff


def test_missing_non_build_artifact_copy_source_still_raises(tmp_path: Path) -> None:
    """Negative control: a missing COPY source path that the manifest does NOT mark
    BUILD_ARTIFACT must still be a hard failure, with the pre-existing message -- the fix must
    not weaken that."""
    repo_root = tmp_path / "repo"
    repo_root.mkdir(parents=True)
    dockerfile = repo_root / "Dockerfile"
    dockerfile.write_text(
        "FROM ubuntu:22.04\nCOPY some/tracked-file.txt /opt/some/tracked-file.txt\n"
    )
    # some/tracked-file.txt is never created, and build_artifacts is empty -- nothing marks
    # this path as a build artifact.

    with pytest.raises(
        AssertionError,
        match=re.escape("COPY source path does not exist on disk: some/tracked-file.txt"),
    ):
        compute_current_copy_manifest(
            repo_root=repo_root, dockerfile=dockerfile, build_artifacts=frozenset()
        )


def test_missing_build_artifact_alongside_real_change_still_shown(tmp_path: Path) -> None:
    """When an absent build artifact coincides with an actual content change elsewhere, the
    diff must fall into the real 'manifest vs. current build context diff' branch (not the
    'agree EXACTLY' one) and must show both: the absent build artifact labelled as such, and the
    real change."""
    recorded = {"bin/av-lockstep-shim": "0" * 64, "src/app.c": "1" * 64}
    current = {"src/app.c": "2" * 64}  # app.c's hash changed; the build artifact is absent

    diff = _diff_manifests(recorded, current, ["bin/av-lockstep-shim"])
    assert "manifest vs. current build context diff:" in diff
    assert "bin/av-lockstep-shim" in diff
    assert "build artifact" in diff.lower()
    assert "~ changed: src/app.c" in diff
    assert "agree EXACTLY" not in diff
