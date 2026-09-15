"""D3 first half (docs/p5-plan.md, P5 track round 1): tests for the kit builder
(`scripts/kit/build_kit.py`) and its manifest format/verifier (`scripts/kit/manifest.py`).

Every test here either builds a real kit into `tmp_path` (never anywhere else) or drives
`manifest.pack_descriptor` directly against a fabricated `tmp_path` pack -- no test writes
outside `tmp_path`, and no test sets an environment variable on its own process (question 199):
the one opt-in test that touches Docker only ever READS `os.environ.get("AV_KIT_WITH_IMAGES")`,
mirroring `tests/test_sbom.py`'s own `AV_SBOM_REBUILD` gate and `tests/
test_edge_plugin_container.py`'s `_compute_skip_reason` pattern -- computed once at import time,
`pytest.mark.skipif` naming the reason, never a silent pass (question 194).
"""
from __future__ import annotations

import hashlib
import json
import os
import subprocess
import sys
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
KIT_DIR = REPO_ROOT / "scripts" / "kit"
sys.path.insert(0, str(KIT_DIR))
import build_kit  # noqa: E402  (path insert must precede this import)
import manifest as kmanifest  # noqa: E402
import sbom  # noqa: E402


def _build_kit(tmp_path: Path, *, name: str = "kit", **kwargs) -> tuple[Path, Path, str]:
    """Builds a real kit into `tmp_path / name` via the real CLI-backing function
    (`build_kit.build`), with the eval site (the builder's own default). Returns
    `(kit_root, manifest_path, manifest_sha256)`."""
    kit_root = tmp_path / name
    site = REPO_ROOT / build_kit.DEFAULT_SITE
    manifest_path, digest = build_kit.build(repo_root=REPO_ROOT, out=kit_root, site=site, **kwargs)
    return kit_root, manifest_path, digest


# =================================================================================================
# 1. A kit builds and verifies clean
# =================================================================================================

def test_a_kit_builds_and_verifies(tmp_path):
    kit_root, manifest_path, _digest = _build_kit(tmp_path)

    findings = kmanifest.verify_manifest(kit_root)
    assert findings == [], f"a freshly built kit must verify with no findings, got: {findings}"

    doc = json.loads(manifest_path.read_text())
    listed = {f["path"] for f in doc["files"]}
    on_disk = {p.relative_to(kit_root).as_posix() for p in kmanifest._walk_kit_files(kit_root)}
    assert listed == on_disk, (
        f"KIT_MANIFEST's files list must name every real file the kit contains -- "
        f"present but unlisted: {sorted(on_disk - listed)}, listed but absent: {sorted(listed - on_disk)}"
    )
    assert doc["kit_format"] == kmanifest.KIT_FORMAT

    # Review finding D3-1(a): an earlier cut of this builder let deploy/secdeploy/merge.py plant
    # a `deploy` symlink (an absolute path into the user's own secdeploy checkout) inside the
    # kit. A freshly built kit must contain NO symlinks at all.
    symlinks = kmanifest._walk_kit_symlinks(kit_root)
    assert symlinks == [], f"a freshly built kit must contain no symlinks, found: {symlinks}"


# =================================================================================================
# 2. A tampered file fails verification
# =================================================================================================

def test_a_tampered_file_fails_verification(tmp_path):
    kit_root, _manifest_path, _digest = _build_kit(tmp_path)

    target = kit_root / "suite.merged.toml"
    original = target.read_bytes()
    tampered = bytearray(original)
    tampered[0] ^= 0xFF
    target.write_bytes(bytes(tampered))

    findings = kmanifest.verify_manifest(kit_root)
    hash_findings = [f for f in findings if f.kind == "hash_mismatch" and f.path == "suite.merged.toml"]
    assert len(hash_findings) == 1, findings
    finding = hash_findings[0]

    # Both hashes are named (question in this task's own acceptance evidence: "paste the finding").
    assert finding.expected_sha256 == hashlib.sha256(original).hexdigest()
    assert finding.actual_sha256 == hashlib.sha256(bytes(tampered)).hexdigest()
    assert finding.expected_sha256 != finding.actual_sha256


# =================================================================================================
# 3. A file the manifest does not list fails verification
# =================================================================================================

def test_a_file_the_manifest_does_not_list_fails_verification(tmp_path):
    kit_root, _manifest_path, _digest = _build_kit(tmp_path)

    extra = kit_root / "not_part_of_the_kit.txt"
    extra.write_text("a file KIT_MANIFEST never asked for\n", encoding="utf-8")

    findings = kmanifest.verify_manifest(kit_root)
    unlisted = [f for f in findings if f.kind == "unlisted_file"]
    assert len(unlisted) == 1, findings
    assert unlisted[0].path == "not_part_of_the_kit.txt"


# =================================================================================================
# 4. A missing file fails verification
# =================================================================================================

def test_a_missing_file_fails_verification(tmp_path):
    kit_root, _manifest_path, _digest = _build_kit(tmp_path)

    victim = kit_root / "sbom" / "av-command.cdx.json"
    assert victim.is_file()
    victim.unlink()

    findings = kmanifest.verify_manifest(kit_root)
    missing = [f for f in findings if f.kind == "missing_file"]
    assert len(missing) == 1, findings
    assert missing[0].path == "sbom/av-command.cdx.json"


# =================================================================================================
# 5. Two kits from the same commit have the same manifest hash -- the headline claim
# =================================================================================================

def test_two_kits_from_the_same_commit_have_the_same_manifest_hash(tmp_path):
    _kit_a, manifest_a, digest_a = _build_kit(tmp_path, name="a")
    _kit_b, manifest_b, digest_b = _build_kit(tmp_path, name="b")

    assert digest_a == digest_b, "two builds from the same commit produced different KIT_MANIFEST hashes"
    assert manifest_a.read_bytes() == manifest_b.read_bytes()


# =================================================================================================
# 6. The manifest records the real git commit and dirty state
# =================================================================================================

def test_the_manifest_records_the_git_commit_and_dirty_state(tmp_path):
    _kit_root, manifest_path, _digest = _build_kit(tmp_path)
    doc = json.loads(manifest_path.read_text())

    expected_commit = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=REPO_ROOT, capture_output=True, text=True, check=True,
    ).stdout.strip()
    expected_status_text = subprocess.run(
        ["git", "status", "--porcelain"], cwd=REPO_ROOT, capture_output=True, text=True, check=True,
    ).stdout
    expected_status_lines = sorted(l for l in expected_status_text.splitlines() if l.strip())

    assert doc["git_commit"] == expected_commit
    assert doc["git_dirty"] == bool(expected_status_lines)
    # Review finding D3-2: git_dirty alone is uninformative in this worktree (it is permanently
    # true because of pre-existing untracked entries) -- git_status is what actually says why.
    assert doc["git_status"] == expected_status_lines
    assert doc["git_status"] == sorted(doc["git_status"]), "git_status must be sorted"


# =================================================================================================
# 7. The sbom hashes match the committed SHA256SUMS (Decision L's cross-check)
# =================================================================================================

def test_the_sbom_hashes_match_the_committed_sha256sums(tmp_path):
    _kit_root, manifest_path, _digest = _build_kit(tmp_path)
    doc = json.loads(manifest_path.read_text())

    sums_path = REPO_ROOT / "docs" / "compliance" / "sbom" / "SHA256SUMS"
    expected: dict[str, str] = {}
    for line in sums_path.read_text().splitlines():
        if not line.strip():
            continue
        digest, _, rel = line.partition("  ")
        component = Path(rel).stem.removesuffix(".cdx")
        expected[component] = digest

    assert doc["sboms"] == expected


# =================================================================================================
# 8. The manifest declares its gaps (Decision J)
# =================================================================================================

def test_the_manifest_declares_its_gaps(tmp_path):
    _kit_root, manifest_path, _digest = _build_kit(tmp_path)
    doc = json.loads(manifest_path.read_text())

    names = {g["name"] for g in doc["gaps"]}
    assert names == {
        "cargo-vendor", "python-wheels", "seccert-root", "install-path", "secdeploy-deploy-assets",
    }, (
        "KIT_MANIFEST must name exactly the five things this half of D3 deliberately does not "
        f"carry (cargo-vendored crate sources, Python wheels, the seccert root, the install "
        f"path, and the secdeploy deploy/ assets the merge symlink used to leak -- review "
        f"finding D3-1) -- got {sorted(names)}"
    )
    for gap in doc["gaps"]:
        assert gap["reason"].strip(), f"gap {gap['name']!r} has an empty reason"


# =================================================================================================
# 9. Pack descriptors are reproducible, and change when the pack's content does
# =================================================================================================

def test_pack_descriptor_is_reproducible():
    a = kmanifest.pack_descriptor("data-time", REPO_ROOT / "data" / "time", repo_root=REPO_ROOT)
    b = kmanifest.pack_descriptor("data-time", REPO_ROOT / "data" / "time", repo_root=REPO_ROOT)
    assert a == b
    assert a["content_hash"].startswith("sha256:")
    assert a["in_worktree"] is True
    assert a["via_symlink"] is False


def test_pack_descriptor_changes_when_a_file_in_the_pack_changes(tmp_path):
    pack_dir = tmp_path / "fabricated_pack"
    pack_dir.mkdir()
    (pack_dir / "a.txt").write_text("hello\n", encoding="utf-8")
    (pack_dir / "b.txt").write_text("world\n", encoding="utf-8")

    before = kmanifest.pack_descriptor("fabricated", pack_dir, repo_root=tmp_path)
    (pack_dir / "a.txt").write_text("hello, but different now\n", encoding="utf-8")
    after = kmanifest.pack_descriptor("fabricated", pack_dir, repo_root=tmp_path)

    assert before["content_hash"] != after["content_hash"]
    assert before["file_count"] == after["file_count"] == 2


def test_pack_descriptor_refuses_a_pack_over_max_pack_bytes(tmp_path):
    pack_dir = tmp_path / "big_pack"
    pack_dir.mkdir()
    (pack_dir / "big.bin").write_bytes(b"x" * 1000)

    with pytest.raises(kmanifest.PackTooLargeError):
        kmanifest.pack_descriptor("big", pack_dir, repo_root=tmp_path, max_pack_bytes=100)


def test_pack_descriptor_records_a_symlinked_out_of_worktree_pack_honestly(tmp_path):
    """This worktree happens to have `third_party/cspice` as a real symlink out into another
    worktree entirely, but that layout is an artefact of THIS checkout, not something a plain
    clone can rely on (a fresh clone has `third_party/cspice` absent or fetched as a real
    directory) -- so this test no longer reads this worktree's own `third_party/cspice` at all.
    Instead it builds an equivalent symlinked pack entirely under `tmp_path`: a fabricated
    `outside/` directory (with a file at its top level and one in a subdirectory, standing in for
    a small real pack) and a fabricated `worktree/third_party/cspice` symlink pointing at it,
    then proves `pack_descriptor` records that shape honestly -- Decision K: a symlinked pack is
    recorded as such, never silently followed as if it were ours, and never as an absolute path
    even though it genuinely resolves outside the (fabricated) checkout."""
    outside = tmp_path / "outside"
    (outside / "sub").mkdir(parents=True)
    (outside / "a.txt").write_text("hello\n", encoding="utf-8")
    (outside / "sub" / "b.txt").write_text("world\n", encoding="utf-8")
    planted_file_count = 2

    worktree = tmp_path / "worktree"
    (worktree / "third_party").mkdir(parents=True)
    (worktree / "third_party" / "cspice").symlink_to(outside, target_is_directory=True)

    desc = kmanifest.pack_descriptor("cspice", worktree / "third_party" / "cspice", repo_root=worktree)
    assert desc["via_symlink"] is True
    assert desc["in_worktree"] is False
    assert desc["source_path"] == "third_party/cspice"
    assert desc["resolved_real_path"] == "../outside"
    assert not Path(desc["resolved_real_path"]).is_absolute()
    assert not Path(desc["source_path"]).is_absolute()
    assert desc["file_count"] == planted_file_count


# =================================================================================================
# 10. The images step is gated: off by default, real and lock-protected when opted in
# =================================================================================================

def test_images_step_is_gated(tmp_path):
    """With `--with-images` not passed (the default), KIT_MANIFEST still records both images'
    recorded digests (Decision J: that much is in scope unconditionally) but says plainly that
    neither tarball was collected -- `collected` is `False`, not merely absent, and no
    `images/*.tar` file or `"image-tarball"`-role entry exists anywhere in the kit. Needs no
    Docker at all; runs unconditionally as part of the default gate."""
    kit_root, manifest_path, _digest = _build_kit(tmp_path)
    doc = json.loads(manifest_path.read_text())

    assert set(doc["images"]) == set(sbom.IMAGE_COMPONENTS)
    for component in sbom.IMAGE_COMPONENTS:
        entry = doc["images"][component]
        assert entry["collected"] is False
        assert entry["tarball_sha256"] is None
        assert entry["tarball_size"] is None
        assert entry["recorded_digest"].startswith("sha256:")
        assert entry["tag"]

    assert not (kit_root / "images").exists(), "no images/ directory should exist when --with-images was not requested"
    assert all(f["role"] != "image-tarball" for f in doc["files"])


IMAGES_OPT_IN_VAR = "AV_KIT_WITH_IMAGES"


def _images_opted_in() -> bool:
    return os.environ.get(IMAGES_OPT_IN_VAR, "").strip().lower() in ("1", "true", "yes")


def _docker_available() -> bool:
    try:
        result = subprocess.run(["docker", "info"], capture_output=True, timeout=10)
    except (FileNotFoundError, OSError):
        return False
    return result.returncode == 0


def _compute_images_skip_reason() -> "str | None":
    if not _images_opted_in():
        return (
            f"{IMAGES_OPT_IN_VAR} is not set -- this test runs the real, gated `docker save` "
            "path (an image-digest comparison against each component's recorded digest, then "
            "`docker save`, for both recorded images, under the host-wide docker-test lock). "
            "The default suite skips it; set AV_KIT_WITH_IMAGES=1 on the command line to opt in."
        )
    if not _docker_available():
        return "Docker is not available on this host (`docker info` failed)."
    for component in sbom.IMAGE_COMPONENTS:
        tag = kmanifest.read_recorded_image(component)["tag"]
        result = subprocess.run(
            ["docker", "image", "inspect", tag, "--format", "{{.Id}}"],
            capture_output=True, timeout=30,
        )
        if result.returncode != 0:
            return (
                f"image {tag!r} ({component}) has not been built on this host -- this test only "
                f"inspects/saves an already-built image, it never builds one."
            )
    return None


_IMAGES_SKIP_REASON = _compute_images_skip_reason()


@pytest.mark.skipif(_IMAGES_SKIP_REASON is not None, reason=_IMAGES_SKIP_REASON or "")
def test_images_step_with_the_flag_set_runs_the_real_docker_save_path(tmp_path):
    """`AV_KIT_WITH_IMAGES=1`: the real path. `build_kit.collect_images` (called by
    `build_kit.build` when `with_images=True`) itself takes `altavista.docker_test_lock.
    lock_docker_tests()` for this step's ENTIRE body and compares each image's LIVE `docker
    image inspect` id to its recorded digest BEFORE ever running `docker save` (question 212).

    This test deliberately does NOT wrap the call in a second `lock_docker_tests()` of its own:
    `fcntl.flock` locks are attached to one OPEN FILE DESCRIPTION, not reentrant within a single
    process across independent `os.open()` calls (see `altavista/docker_test_lock.py`'s own
    "Why flock, not a PID file" section) -- a second, independent lock acquisition here, while
    `collect_images` already holds the first, would deadlock this test rather than compose with
    it. The lock genuinely covers this step's whole body either way: by the ONE production code
    path this test and the real CLI (`scripts/kit/build.sh --with-images`) both share, rather
    than by test-local, duplicated locking logic that could drift from it."""
    kit_root, manifest_path, _digest = _build_kit(tmp_path, with_images=True)
    doc = json.loads(manifest_path.read_text())

    for component in sbom.IMAGE_COMPONENTS:
        entry = doc["images"][component]
        assert entry["collected"] is True
        assert entry["tarball_sha256"] is not None
        tar_path = kit_root / "images" / f"{component}.tar"
        assert tar_path.is_file()
        assert sbom.sha256_file(tar_path) == entry["tarball_sha256"]
        assert tar_path.stat().st_size == entry["tarball_size"]

    # Question 148: an exit code (or a green test) is not evidence -- print what was actually
    # observed and compared.
    print(f"\n--- images step, observed ---\n" + "\n".join(
        f"{c}: tag={doc['images'][c]['tag']} recorded_digest={doc['images'][c]['recorded_digest']} "
        f"tarball_sha256={doc['images'][c]['tarball_sha256']}"
        for c in sbom.IMAGE_COMPONENTS
    ))

    findings = kmanifest.verify_manifest(kit_root)
    assert findings == [], findings


# =================================================================================================
# 11. No symlinks in a kit (review finding D3-1) -- verify_manifest must catch every shape
# =================================================================================================

def test_verify_manifest_catches_a_symlink_to_a_file_inside_the_kit(tmp_path):
    kit_root, _manifest_path, _digest = _build_kit(tmp_path)

    (kit_root / "link_to_suite_inside_kit").symlink_to("suite.merged.toml")

    findings = kmanifest.verify_manifest(kit_root)
    matches = [f for f in findings if f.path == "link_to_suite_inside_kit"]
    assert len(matches) == 1, findings
    assert matches[0].kind == "unexpected_symlink"
    assert matches[0].link_target == "suite.merged.toml"


def test_verify_manifest_catches_a_symlink_to_an_absolute_path_outside_the_kit(tmp_path):
    """The exact shape review finding D3-1(a) found `deploy/secdeploy/merge.py::merge` planting
    inside an earlier cut of this builder's kits -- an absolute path leaking out of the kit
    entirely. Reported with the MORE SEVERE `"unsafe_symlink_target"` kind, distinct from an
    in-kit relative symlink's `"unexpected_symlink"`."""
    kit_root, _manifest_path, _digest = _build_kit(tmp_path)

    (kit_root / "link_to_etc_passwd").symlink_to("/etc/passwd")

    findings = kmanifest.verify_manifest(kit_root)
    matches = [f for f in findings if f.path == "link_to_etc_passwd"]
    assert len(matches) == 1, findings
    assert matches[0].kind == "unsafe_symlink_target"
    assert matches[0].link_target == "/etc/passwd"


def test_verify_manifest_catches_a_dangling_symlink(tmp_path):
    """Classification is purely lexical (never `Path.resolve()`/`os.path.realpath()`), so a
    symlink whose target does not exist at all is still caught and classified, not silently
    skipped because there is nothing there to stat."""
    kit_root, _manifest_path, _digest = _build_kit(tmp_path)

    (kit_root / "dangling_link").symlink_to("this_file_does_not_exist.txt")
    assert not (kit_root / "dangling_link").exists()  # exists() follows the link -- confirms it really is dangling
    assert (kit_root / "dangling_link").is_symlink()

    findings = kmanifest.verify_manifest(kit_root)
    matches = [f for f in findings if f.path == "dangling_link"]
    assert len(matches) == 1, findings
    assert matches[0].kind == "unexpected_symlink"
    assert matches[0].link_target == "this_file_does_not_exist.txt"


def test_verify_manifest_catches_a_dangling_symlink_with_an_absolute_target(tmp_path):
    kit_root, _manifest_path, _digest = _build_kit(tmp_path)

    (kit_root / "dangling_absolute_link").symlink_to("/this/absolute/path/does/not/exist")

    findings = kmanifest.verify_manifest(kit_root)
    matches = [f for f in findings if f.path == "dangling_absolute_link"]
    assert len(matches) == 1, findings
    assert matches[0].kind == "unsafe_symlink_target"


def test_verify_manifest_catches_a_symlink_that_escapes_the_kit_via_dotdot(tmp_path):
    kit_root, _manifest_path, _digest = _build_kit(tmp_path)

    (kit_root / "escaping_link").symlink_to("../outside_the_kit.txt")

    findings = kmanifest.verify_manifest(kit_root)
    matches = [f for f in findings if f.path == "escaping_link"]
    assert len(matches) == 1, findings
    assert matches[0].kind == "unsafe_symlink_target"


# =================================================================================================
# 12. docker save is measured (not assumed) to be byte-reproducible on this host (review D3-3)
# =================================================================================================

@pytest.mark.skipif(_IMAGES_SKIP_REASON is not None, reason=_IMAGES_SKIP_REASON or "")
def test_two_image_bearing_kits_from_the_same_commit_have_the_same_manifest_and_tarball_hashes(tmp_path):
    """Decision I's reproducibility argument covers the metadata path (Decision J's `images`
    dict when `--with-images` is off); it does NOT, by itself, cover `docker save`'s own output
    format, which is not documented to be byte-for-byte deterministic across independent runs.
    This test measures whether it actually is, on THIS host/docker version (recorded in
    scripts/kit/README.md alongside `docker --version`) -- two full `--with-images` builds must
    produce identical tarball hashes AND therefore an identical KIT_MANIFEST hash. If a future
    Docker/BuildKit ever makes `docker save` non-deterministic, this is the test that says so."""
    kit_a, manifest_a, digest_a = _build_kit(tmp_path, name="img-a", with_images=True)
    kit_b, manifest_b, digest_b = _build_kit(tmp_path, name="img-b", with_images=True)

    assert digest_a == digest_b, "two --with-images builds from the same commit produced different KIT_MANIFEST hashes"
    assert manifest_a.read_bytes() == manifest_b.read_bytes()

    doc_a = json.loads(manifest_a.read_text())
    doc_b = json.loads(manifest_b.read_text())
    for component in sbom.IMAGE_COMPONENTS:
        sha_a = doc_a["images"][component]["tarball_sha256"]
        sha_b = doc_b["images"][component]["tarball_sha256"]
        assert sha_a is not None and sha_a == sha_b, (component, sha_a, sha_b)
        tar_a = (kit_a / "images" / f"{component}.tar").read_bytes()
        tar_b = (kit_b / "images" / f"{component}.tar").read_bytes()
        assert tar_a == tar_b, f"{component}: docker save produced different tarball bytes across two runs"

    print(f"\n--- docker save reproducibility, observed ---\n" + "\n".join(
        f"{c}: tarball_sha256={doc_a['images'][c]['tarball_sha256']} (identical across both builds)"
        for c in sbom.IMAGE_COMPONENTS
    ))
