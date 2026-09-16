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
import shutil
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
    # kit. A freshly built kit must contain NO symlinks OUTSIDE a copied pack's own
    # `packs/<name>/...` tree -- unchanged since round 1. Task 3c's `web`/`profiles` packs were
    # copied unconditionally for one round (`manifest.ALWAYS_COPY_PACKS`), which made a freshly
    # built kit carry exactly the two pack-internal symlinks `web/`'s own `node_modules/three/`
    # re-export required; round 3 (question 217(b)) removed both packs outright -- the
    # `altavista` wheel carries the same two paths, dereferenced into real file copies, instead
    # (see `setup.py`'s own doc) -- so a freshly built DEFAULT kit (no `--copy-pack` at all, as
    # `_build_kit` above builds one) is back to carrying no symlinks anywhere, the same as round
    # 1. This assertion stays general (not hard-coded to "zero") because `--copy-pack data-time`
    # and friends remain available and still must stay symlink-free themselves; only a pack that
    # legitimately carries its own internal symlinks (none of the packs left in `manifest.PACKS`
    # do, as of round 3) would make this non-empty.
    symlink_rel_paths = {p.relative_to(kit_root).as_posix() for p in kmanifest._walk_kit_symlinks(kit_root)}
    non_pack = {p for p in symlink_rel_paths if Path(p).parts[0] != "packs"}
    assert non_pack == set(), f"a freshly built kit must contain no symlinks outside a copied pack, found: {non_pack}"
    assert symlink_rel_paths == {e["path"] for e in doc["pack_symlinks"]}, (
        f"every symlink found on disk inside a pack must be declared in KIT_MANIFEST's own "
        f"pack_symlinks list, and vice versa -- found {symlink_rel_paths}, declared "
        f"{ {e['path'] for e in doc['pack_symlinks']} }"
    )


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


# =================================================================================================
# 13. kit_format is bumped -- round 2 (P5 track round 2 task 3a, lead ruling 214(b)) moved it
# 1 -> 2; round 3 (question 217(b)) moved it again, 2 -> 3, when the web/profiles packs were
# removed (they now ship inside the altavista wheel instead -- manifest.py's own top doc, "P5
# track round 3", has the full reasoning for why that counts as a real format change and not
# merely an internal refactor).
# =================================================================================================

def test_kit_format_is_bumped_for_round_3(tmp_path):
    _kit_root, manifest_path, _digest = _build_kit(tmp_path)
    doc = json.loads(manifest_path.read_text())
    assert doc["kit_format"] == 3
    assert kmanifest.KIT_FORMAT == 3


# =================================================================================================
# 14. A copied pack verifies; a tampered byte inside it is caught
# =================================================================================================

def test_a_copied_pack_verifies_clean_and_its_files_land_in_the_manifest(tmp_path):
    kit_root, manifest_path, _digest = _build_kit(tmp_path, copy_pack_names=["data-time"])
    doc = json.loads(manifest_path.read_text())

    assert doc["packs"]["data-time"]["copied"] is True
    copy = doc["packs"]["data-time"]["copy"]
    assert copy["kit_path"] == "packs/data-time"
    assert copy["copied_file_count"] > 0
    assert copy["copied_symlink_count"] == 0  # data/time carries no symlinks of its own

    pack_files = [f for f in doc["files"] if f["path"].startswith("packs/data-time/")]
    assert len(pack_files) == copy["copied_file_count"]
    assert all(f["role"] == "pack-file" for f in pack_files)
    assert (kit_root / "packs" / "data-time").is_dir()

    findings = kmanifest.verify_manifest(kit_root)
    assert findings == [], findings


def test_a_tampered_byte_inside_a_copied_pack_is_caught(tmp_path):
    kit_root, manifest_path, _digest = _build_kit(tmp_path, copy_pack_names=["data-time"])
    doc = json.loads(manifest_path.read_text())
    pack_files = [f for f in doc["files"] if f["path"].startswith("packs/data-time/")]
    assert pack_files, "expected at least one copied pack file to tamper"
    victim_rel = pack_files[0]["path"]

    victim = kit_root / victim_rel
    original = victim.read_bytes()
    tampered = bytearray(original)
    tampered[0] ^= 0xFF
    victim.write_bytes(bytes(tampered))

    findings = kmanifest.verify_manifest(kit_root)
    hash_findings = [f for f in findings if f.kind == "hash_mismatch" and f.path == victim_rel]
    assert len(hash_findings) == 1, findings


def test_copy_pack_bytes_refuses_a_pack_over_max_pack_bytes_via_the_descriptor_gate(tmp_path):
    """--max-pack-bytes still gates a --copy-pack request -- the descriptor's own cheap,
    stat-only size check (Decision K) runs before any content is read OR copied."""
    with pytest.raises(kmanifest.PackTooLargeError):
        _build_kit(
            tmp_path, copy_pack_names=["data-time"],
            max_pack_bytes=1,  # data/time is far larger than 1 byte
        )


# =================================================================================================
# 15. A pack's own symlinks: carried verbatim when safe, refused at build time when not,
#     and every on-disk shape verify_manifest must catch -- all fabricated under tmp_path,
#     never depending on this worktree's own symlink layout (the pattern
#     test_pack_descriptor_records_a_symlinked_out_of_worktree_pack_honestly established).
# =================================================================================================

def _fabricate_pack_with_symlink(tmp_path: Path, *, target: str, link_name: str = "link") -> Path:
    pack_dir = tmp_path / "fabricated_symlink_pack"
    pack_dir.mkdir()
    (pack_dir / "real.txt").write_text("hello\n", encoding="utf-8")
    (pack_dir / link_name).symlink_to(target)
    return pack_dir


def _minimal_kit(tmp_path: Path, name: str = "kit") -> Path:
    """A bare kit_root with none of the usual suite/sbom/image content -- just enough for
    `manifest.copy_pack_bytes`/`build_manifest`/`verify_manifest` to operate on, so these
    symlink-focused tests never need the real `build_kit.build` pipeline (and therefore never
    touch this worktree's own git state, secdeploy files, or committed SBOMs)."""
    kit_root = tmp_path / name
    kit_root.mkdir()
    return kit_root


def _write_minimal_manifest(kit_root: Path) -> dict:
    doc = kmanifest.build_manifest(
        kit_root=kit_root, git_commit="0" * 40, git_dirty=False, git_status=[],
        images={}, sboms={}, packs={}, vendor={"collected": False, "network_used": False, "offline_error": None},
        wheels={"collected": False, "network_used": False, "fetched": []},
        binaries={"collected": False, "network_used": False, "source_state": None, "results": {}}, runs={},
        gaps=kmanifest.build_gaps(vendor_collected=False, wheels_collected=False),
    )
    kmanifest.write_manifest(doc, kit_root)
    return doc


def test_a_pack_symlink_with_a_relative_same_directory_target_is_copied_verbatim(tmp_path):
    pack_dir = _fabricate_pack_with_symlink(tmp_path, target="real.txt")
    kit_root = _minimal_kit(tmp_path)

    result = kmanifest.copy_pack_bytes("fabricated", pack_dir, kit_root)
    assert result["copied_symlink_count"] == 1
    assert result["copied_file_count"] == 1
    link = kit_root / "packs" / "fabricated" / "link"
    assert link.is_symlink()
    assert os.readlink(link) == "real.txt"

    _write_minimal_manifest(kit_root)
    findings = kmanifest.verify_manifest(kit_root)
    assert findings == [], findings


def test_a_pack_symlink_with_an_absolute_target_is_refused_at_build_time(tmp_path):
    pack_dir = _fabricate_pack_with_symlink(tmp_path, target="/etc/passwd")
    kit_root = _minimal_kit(tmp_path)

    with pytest.raises(kmanifest.UnsafePackSymlinkError):
        kmanifest.copy_pack_bytes("fabricated", pack_dir, kit_root)
    # A hard refusal, never a partial copy: nothing should have been written for this pack.
    assert not (kit_root / "packs" / "fabricated" / "real.txt").exists()


def test_a_pack_symlink_that_escapes_the_pack_via_dotdot_is_refused_at_build_time(tmp_path):
    pack_dir = _fabricate_pack_with_symlink(tmp_path, target="../outside_the_pack.txt")
    kit_root = _minimal_kit(tmp_path)

    with pytest.raises(kmanifest.UnsafePackSymlinkError):
        kmanifest.copy_pack_bytes("fabricated", pack_dir, kit_root)


def test_a_pack_symlink_whose_target_is_changed_on_disk_is_caught(tmp_path):
    pack_dir = _fabricate_pack_with_symlink(tmp_path, target="real.txt")
    (pack_dir / "other.txt").write_text("world\n", encoding="utf-8")
    kit_root = _minimal_kit(tmp_path)
    kmanifest.copy_pack_bytes("fabricated", pack_dir, kit_root)
    _write_minimal_manifest(kit_root)
    assert kmanifest.verify_manifest(kit_root) == []

    link = kit_root / "packs" / "fabricated" / "link"
    link.unlink()
    link.symlink_to("other.txt")

    findings = kmanifest.verify_manifest(kit_root)
    matches = [f for f in findings if f.path == "packs/fabricated/link"]
    assert len(matches) == 1, findings
    assert matches[0].kind == "symlink_target_mismatch"
    assert matches[0].link_target == "other.txt"
    assert matches[0].expected_link_target == "real.txt"


def test_an_unlisted_symlink_planted_in_a_pack_is_caught(tmp_path):
    pack_dir = _fabricate_pack_with_symlink(tmp_path, target="real.txt")
    kit_root = _minimal_kit(tmp_path)
    kmanifest.copy_pack_bytes("fabricated", pack_dir, kit_root)
    _write_minimal_manifest(kit_root)
    assert kmanifest.verify_manifest(kit_root) == []

    # Planted AFTER the manifest was written -- exactly the shape review finding D3-1 first
    # found for a stray kit-root symlink, now checked for the in-pack case too.
    (kit_root / "packs" / "fabricated" / "second_link").symlink_to("real.txt")

    findings = kmanifest.verify_manifest(kit_root)
    matches = [f for f in findings if f.path == "packs/fabricated/second_link"]
    assert len(matches) == 1, findings
    assert matches[0].kind == "unexpected_symlink"


def test_a_pack_symlink_that_becomes_unsafe_after_being_declared_is_still_caught(tmp_path):
    """Defence in depth: even a symlink `pack_symlinks` already declares is re-classified for
    safety on every `verify_manifest` call, never merely looked up and trusted."""
    pack_dir = _fabricate_pack_with_symlink(tmp_path, target="real.txt")
    kit_root = _minimal_kit(tmp_path)
    kmanifest.copy_pack_bytes("fabricated", pack_dir, kit_root)
    _write_minimal_manifest(kit_root)
    assert kmanifest.verify_manifest(kit_root) == []

    link = kit_root / "packs" / "fabricated" / "link"
    link.unlink()
    link.symlink_to("/etc/passwd")

    findings = kmanifest.verify_manifest(kit_root)
    matches = [f for f in findings if f.path == "packs/fabricated/link"]
    assert len(matches) == 1, findings
    assert matches[0].kind == "unsafe_symlink_target"


def test_a_missing_pack_symlink_is_caught(tmp_path):
    pack_dir = _fabricate_pack_with_symlink(tmp_path, target="real.txt")
    kit_root = _minimal_kit(tmp_path)
    kmanifest.copy_pack_bytes("fabricated", pack_dir, kit_root)
    _write_minimal_manifest(kit_root)
    assert kmanifest.verify_manifest(kit_root) == []

    (kit_root / "packs" / "fabricated" / "link").unlink()

    findings = kmanifest.verify_manifest(kit_root)
    matches = [f for f in findings if f.path == "packs/fabricated/link"]
    assert len(matches) == 1, findings
    assert matches[0].kind == "missing_symlink"


def test_a_symlinked_directory_inside_a_pack_is_never_descended_into(tmp_path):
    """`_walk_kit_entries`'s own rule (reused for pack source trees): a symlinked directory is
    reported as its own symlink entry and never traversed -- content on the far side of it is
    never part of the copy, matching the assembled-kit rule this same walker already enforces."""
    pack_dir = tmp_path / "pack_with_symlinked_dir"
    outside = tmp_path / "outside_dir"
    outside.mkdir()
    (outside / "should_never_be_copied.txt").write_text("nope\n", encoding="utf-8")
    pack_dir.mkdir()
    (pack_dir / "real.txt").write_text("hello\n", encoding="utf-8")
    (pack_dir / "linked_dir").symlink_to("../outside_dir", target_is_directory=True)

    kit_root = _minimal_kit(tmp_path)
    with pytest.raises(kmanifest.UnsafePackSymlinkError):
        # "../outside_dir" is a RELATIVE target that escapes this pack's own root -- refused
        # exactly like a file symlink with the same shape would be (the directory case of the
        # same rule; `_walk_kit_entries` reports a symlinked directory as its own entry and never
        # descends into it, so this is caught without ever touching `outside/`'s own content).
        kmanifest.copy_pack_bytes("fabricated", pack_dir, kit_root)


# =================================================================================================
# 16. build_gaps: a collected step stops being reported as a gap
# =================================================================================================

def test_build_gaps_omits_a_collected_step_and_keeps_the_unconditional_three():
    all_gapped = kmanifest.build_gaps(vendor_collected=False, wheels_collected=False)
    names = {g["name"] for g in all_gapped}
    assert names == {"cargo-vendor", "python-wheels", "seccert-root", "install-path", "secdeploy-deploy-assets"}

    vendor_done = kmanifest.build_gaps(vendor_collected=True, wheels_collected=False)
    assert "cargo-vendor" not in {g["name"] for g in vendor_done}
    assert "python-wheels" in {g["name"] for g in vendor_done}

    both_done = kmanifest.build_gaps(vendor_collected=True, wheels_collected=True)
    assert {g["name"] for g in both_done} == {"seccert-root", "install-path", "secdeploy-deploy-assets"}

    with_extra = kmanifest.build_gaps(
        vendor_collected=True, wheels_collected=True,
        extra=[{"name": "av-command-binary", "reason": "cross-build failed: <error>"}],
    )
    assert {g["name"] for g in with_extra} == {"seccert-root", "install-path", "secdeploy-deploy-assets", "av-command-binary"}


def test_the_manifest_declares_its_gaps_unconditionally_include_seccert_root(tmp_path):
    """Unlike round 1's fixed five, round 2's gap list membership varies with which flags a kit
    build used -- but seccert-root, install-path, and secdeploy-deploy-assets never go away,
    since nothing this task adds collects any of them."""
    _kit_root, manifest_path, _digest = _build_kit(tmp_path)
    doc = json.loads(manifest_path.read_text())
    names = {g["name"] for g in doc["gaps"]}
    assert {"seccert-root", "install-path", "secdeploy-deploy-assets"} <= names
    for gap in doc["gaps"]:
        assert gap["reason"].strip(), f"gap {gap['name']!r} has an empty reason"


# =================================================================================================
# 17. The recorded kernel run (tests/fixtures/*.runproducts.bin), carried unconditionally
# =================================================================================================

def test_the_recorded_kernel_runs_are_carried_with_their_provenance(tmp_path):
    kit_root, manifest_path, _digest = _build_kit(tmp_path)
    doc = json.loads(manifest_path.read_text())

    fixtures_dir = REPO_ROOT / "tests" / "fixtures"
    expected_stems = {p.name[: -len(".runproducts.bin")] for p in fixtures_dir.glob("*.runproducts.bin")}
    assert expected_stems, "expected at least one committed *.runproducts.bin fixture"
    assert set(doc["runs"]) == expected_stems

    for stem in expected_stems:
        entry = doc["runs"][stem]
        assert entry["decoded"] is True, f"{stem}: expected the provenance config_hash to be cheaply readable"
        assert entry["config_hash"], f"{stem}: config_hash must be non-empty when decoded"
        assert len(entry["config_hash"]) == 64  # a SHA-256 hex string

        run_files = [f for f in doc["files"] if f["path"] == f"runs/{stem}.runproducts.bin"]
        assert len(run_files) == 1, doc["files"]
        assert run_files[0]["role"] == "run-fixture"
        on_disk = kit_root / "runs" / f"{stem}.runproducts.bin"
        assert sbom.sha256_file(on_disk) == run_files[0]["sha256"]

    findings = kmanifest.verify_manifest(kit_root)
    assert findings == [], findings


# =================================================================================================
# 18. --with-vendor (gated: real cargo vendor, network only if offline genuinely fails)
# =================================================================================================

VENDOR_OPT_IN_VAR = "AV_KIT_WITH_VENDOR"


def _vendor_opted_in() -> bool:
    return os.environ.get(VENDOR_OPT_IN_VAR, "").strip().lower() in ("1", "true", "yes")


def _compute_vendor_skip_reason() -> "str | None":
    if not _vendor_opted_in():
        return (
            f"{VENDOR_OPT_IN_VAR} is not set -- this test runs the real `cargo vendor --offline` "
            f"path (falling back to the network once if that genuinely fails). The default suite "
            f"skips it; set AV_KIT_WITH_VENDOR=1 on the command line to opt in."
        )
    if shutil.which("cargo") is None:
        return "cargo is not on PATH (export PATH=\"/opt/homebrew/opt/rustup/bin:$PATH\" first)"
    return None


_VENDOR_SKIP_REASON = _compute_vendor_skip_reason()


@pytest.mark.skipif(_VENDOR_SKIP_REASON is not None, reason=_VENDOR_SKIP_REASON or "")
def test_with_vendor_runs_the_real_cargo_vendor_path(tmp_path):
    kit_root, manifest_path, _digest = _build_kit(tmp_path, with_vendor=True)
    doc = json.loads(manifest_path.read_text())

    assert doc["vendor"]["collected"] is True
    assert "cargo-vendor" not in {g["name"] for g in doc["gaps"]}
    assert (kit_root / "vendor" / ".cargo-config.toml").is_file()
    vendored_crates = list((kit_root / "vendor").iterdir())
    assert len(vendored_crates) > 1, "expected cargo vendor to have populated <kit>/vendor/"

    findings = kmanifest.verify_manifest(kit_root)
    assert findings == [], findings

    print(f"\n--- --with-vendor, observed ---\nnetwork_used={doc['vendor']['network_used']} "
          f"offline_error={doc['vendor']['offline_error']!r} crates={len(vendored_crates)}")


# =================================================================================================
# 19. --with-wheels (gated: real network use, question 154's one permitted exception)
# =================================================================================================

WHEELS_OPT_IN_VAR = "AV_KIT_WITH_WHEELS"


def _wheels_opted_in() -> bool:
    return os.environ.get(WHEELS_OPT_IN_VAR, "").strip().lower() in ("1", "true", "yes")


def _compute_wheels_skip_reason() -> "str | None":
    if not _wheels_opted_in():
        return (
            f"{WHEELS_OPT_IN_VAR} is not set -- this test uses the network for real (pip "
            f"download against PyPI, question 154's one permitted exception at kit-build time). "
            f"The default suite skips it; set AV_KIT_WITH_WHEELS=1 on the command line to opt in."
        )
    return None


_WHEELS_SKIP_REASON = _compute_wheels_skip_reason()


@pytest.mark.skipif(_WHEELS_SKIP_REASON is not None, reason=_WHEELS_SKIP_REASON or "")
def test_with_wheels_runs_the_real_pip_download_path(tmp_path):
    kit_root, manifest_path, _digest = _build_kit(tmp_path, with_wheels=True)
    doc = json.loads(manifest_path.read_text())

    assert doc["wheels"]["collected"] is True
    assert doc["wheels"]["network_used"] is True
    assert "python-wheels" not in {g["name"] for g in doc["gaps"]}
    fetched_names = {f["name"] for f in doc["wheels"]["fetched"]}
    assert "altavista" in fetched_names
    for entry in doc["wheels"]["fetched"]:
        whl = kit_root / "wheels" / entry["filename"]
        assert whl.is_file(), entry
        assert sbom.sha256_file(whl) == entry["sha256"]

    findings = kmanifest.verify_manifest(kit_root)
    assert findings == [], findings

    wheel_gaps = [g for g in doc["gaps"] if g["name"].startswith("wheel:")]
    print(f"\n--- --with-wheels, observed ---\nfetched={sorted(fetched_names)}\n"
          f"named gaps={[g['name'] for g in wheel_gaps]}")


# =================================================================================================
# 20. --with-binaries (gated: real docker cross-build, host-wide lock, av.test labelled)
# =================================================================================================

BINARIES_OPT_IN_VAR = "AV_KIT_WITH_BINARIES"


def _binaries_opted_in() -> bool:
    return os.environ.get(BINARIES_OPT_IN_VAR, "").strip().lower() in ("1", "true", "yes")


def _compute_binaries_skip_reason() -> "str | None":
    if not _binaries_opted_in():
        return (
            f"{BINARIES_OPT_IN_VAR} is not set -- this test cross-builds real Linux binaries "
            f"inside a container (av-ingest-server, av-command), which can take several minutes. "
            f"The default suite skips it; set AV_KIT_WITH_BINARIES=1 on the command line to opt in."
        )
    if not _docker_available():
        return "Docker is not available on this host (`docker info` failed)."
    return None


_BINARIES_SKIP_REASON = _compute_binaries_skip_reason()


@pytest.mark.skipif(_BINARIES_SKIP_REASON is not None, reason=_BINARIES_SKIP_REASON or "")
def test_with_binaries_runs_the_real_cross_build_path(tmp_path):
    kit_root, manifest_path, _digest = _build_kit(tmp_path, with_binaries=True)
    doc = json.loads(manifest_path.read_text())

    assert doc["binaries"]["collected"] is True
    assert "av-ingest-server" in doc["binaries"]["results"]
    ingest_result = doc["binaries"]["results"]["av-ingest-server"]
    assert ingest_result["included"] is True, ingest_result
    ingest_bin = kit_root / "binaries" / "av-ingest-server"
    assert ingest_bin.is_file()
    assert os.access(ingest_bin, os.X_OK)

    command_result = doc["binaries"]["results"]["av-command"]
    if not command_result["included"]:
        assert command_result["reason"], "a failed cross-build must carry the real compiler error"
        assert any(g["name"] == "av-command-binary" for g in doc["gaps"])

    findings = kmanifest.verify_manifest(kit_root)
    assert findings == [], findings

    # The binaries step's own honesty fields: the cross-build container installs its build
    # dependencies with apt before compiling, so this step DOES reach the network at kit-build
    # time (question 154 permits exactly that, once, when a kit is built) -- a kit must say so
    # rather than let a reader infer it. `source_state` names the commit AND the working tree the
    # bytes were compiled from, so a kit built from a dirty tree cannot silently carry a binary
    # compiled from a different one.
    assert doc["binaries"]["network_used"] is True
    assert doc["binaries"]["source_state"] == build_kit.binary_cache_key(
        doc["git_commit"], *build_kit._git_worktree_state_texts(REPO_ROOT)
    )

    print(f"\n--- --with-binaries, observed ---\n"
          f"network_used={doc['binaries']['network_used']} "
          f"source_state={doc['binaries']['source_state']}\n" + "\n".join(
        f"{name}: included={info['included']} reason={info.get('reason')!r}"
        for name, info in doc["binaries"]["results"].items()
    ))


# =================================================================================================
# 20b. The cross-built binary cache is keyed on the SOURCE STATE, never the commit alone
# =================================================================================================

def test_binary_cache_key_changes_with_any_working_tree_change():
    """Review finding (P5 round 2): keying the cross-build cache on `git_commit` alone meant a kit
    built from a tree with uncommitted changes reused a binary compiled from a DIFFERENT tree
    state, with nothing in the kit saying so. `binary_cache_key` is a pure function of (commit,
    `git status --porcelain`, `git diff HEAD`), so this drives it directly with fabricated inputs
    -- no git repository, no cross-build, no Docker needed.

    Both directions are asserted: the same three inputs always give the same key (or two kits at
    one source state would never share a binary, and the "two kits from the same commit have the
    same manifest hash" claim would fail), and changing ANY of the three gives a different one."""
    commit = "a" * 40
    status = "?? third_party/mirrors\n"
    diff = "diff --git a/crates/av-ingest/src/lib.rs b/crates/av-ingest/src/lib.rs\n"

    base = build_kit.binary_cache_key(commit, status, diff)
    assert base == build_kit.binary_cache_key(commit, status, diff), (
        "the key must be a pure function of its inputs -- two kits built from one source state "
        "must reuse the same cached binary, or the manifest hash cannot be reproducible"
    )
    assert base.startswith(commit + "-"), (
        f"the key must still name the commit it belongs to, for a human reading the cache "
        f"directory -- got {base!r}"
    )

    changed_commit = build_kit.binary_cache_key("b" * 40, status, diff)
    changed_status = build_kit.binary_cache_key(commit, status + "?? crates/av-new/\n", diff)
    changed_diff = build_kit.binary_cache_key(commit, status, diff + "+// an uncommitted edit\n")

    print(f"\n--- binary cache key, observed ---\n"
          f"base            = {base}\n"
          f"other commit    = {changed_commit}\n"
          f"other status    = {changed_status}\n"
          f"other diff      = {changed_diff}")

    for label, other in (
        ("a different commit", changed_commit),
        ("a new untracked path in git status", changed_status),
        ("an uncommitted tracked change in git diff HEAD", changed_diff),
    ):
        assert other != base, (
            f"{label} must produce a different cache key -- otherwise a kit would carry a binary "
            f"compiled from a source state it does not record"
        )
