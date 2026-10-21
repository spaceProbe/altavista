"""Question 264: the air-gap kit carries the Blue Marble tile pyramid (`imagery-bluemarble`).

The builder (`build_kit.py --copy-pack imagery-bluemarble [--imagery-dir DIR]`) verifies a pyramid that lives
outside the repository against its own `manifest.json`, refuses a mismatch, an incomplete pyramid and a size over
`--max-pack-bytes`, copies the bytes and records `pyramid_sha256`, the original and the licence in
`KIT_MANIFEST.packs`; the installer verifies the carried pyramid again before use and says how to start the viewer
on it. Everything here runs over a tiny synthetic pyramid under `tmp_path`: no network, no real pyramid. The one
test that builds with the real pyramid skips, with its reason, when `~/altavista-data/imagery/bluemarble` is absent.
No test writes `os.environ`.
"""
from __future__ import annotations

import hashlib
import importlib.util
import json
import sys
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
KIT_DIR = REPO_ROOT / "scripts" / "kit"
sys.path.insert(0, str(KIT_DIR))
import build_kit  # noqa: E402  (path insert must precede this import)
import install  # noqa: E402
import kit_inventory  # noqa: E402
import manifest as kmanifest  # noqa: E402

PACK = kmanifest.IMAGERY_PACK
GENERATOR = REPO_ROOT / "scripts" / "imagery" / "build_blue_marble_tiles.py"
REAL_PYRAMID_SHA256 = "58b59438e50ef5ccf454bf96847a3f60b022a1433832083def774dcd8c4677c3"
ORIGINAL_SHA256 = "d225f1f35a6448a4d1d8f6de6e48f3433e470085b70a35800e64f384f269a7b0"
ORIGINAL_URL = "https://example.invalid/world.topo.bathy.200407.3x21600x10800.jpg"
ENCODER = {"args": ["-quality", "88"], "cjpeg": "libjpeg-turbo version 9.9.9 (synthetic)", "djpeg": "libjpeg-turbo version 9.9.9 (synthetic)"}


def _sha(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def make_pyramid(root: Path, max_level: int = 1, body_repeat: int = 40) -> Path:
    """A real, tiny pyramid in the generator's layout: levels 0..`max_level` (2**(L+1) x 2**L tiles each), the
    body image and a `manifest.json` that matches. The bytes are not JPEGs; nothing here decodes them."""
    files: dict[str, dict] = {}

    def put(rel: str, data: bytes) -> None:
        path = root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        files[rel] = {"bytes": len(data), "sha256": _sha(data)}

    for level in range(max_level + 1):
        for x in range(2 ** (level + 1)):
            for y in range(2 ** level):
                put(f"{level}/{x}/{y}.jpg", f"tile {level} {x} {y}\n".encode() * (3 + level))
    put("body/earth-2048x1024.jpg", b"body image\n" * body_repeat)
    digest = hashlib.sha256()
    for rel in sorted(files):
        digest.update(f"{rel} {files[rel]['sha256']}\n".encode())
    doc = {
        "attribution": "Blue Marble: Next Generation (synthetic), credit NASA Earth Observatory",
        "encoder": ENCODER,
        "files": files,
        "format": kmanifest.IMAGERY_FORMAT,
        "image_format": "jpeg",
        "max_level": max_level,
        "min_level": 0,
        "original": {"bytes": 27201049, "file": "world.jpg", "height": 10800, "sha256": ORIGINAL_SHA256,
                     "url": ORIGINAL_URL, "width": 21600},
        "pyramid_sha256": digest.hexdigest(),
        "scheme": "synthetic",
        "tile_count": len(files) - 1,
        "tile_px": 256,
        "total_bytes": sum(f["bytes"] for f in files.values()),
    }
    (root / "manifest.json").write_text(json.dumps(doc, indent=1, sort_keys=True) + "\n", encoding="utf-8")
    return root


@pytest.fixture()
def pyramid(tmp_path) -> Path:
    return make_pyramid(tmp_path / "pyramid")


def _build(tmp_path: Path, *, name: str = "kit", **kwargs):
    return build_kit.build(
        repo_root=REPO_ROOT, out=tmp_path / name, site=REPO_ROOT / build_kit.DEFAULT_SITE, **kwargs)


def _stub_the_wheel_steps(monkeypatch) -> None:
    monkeypatch.setattr(install, "_check_required_wheels", lambda doc: None)
    monkeypatch.setattr(install, "_check_viewer_assets_in_wheel", lambda kit_dir, doc: None)
    monkeypatch.setattr(install, "_install_wheels_into_venv", lambda kit_dir, target_dir: {"venv": "venv", "installed_packages": []})


def _flip_first_byte(path: Path) -> None:
    data = path.read_bytes()
    path.write_bytes(bytes([data[0] ^ 1]) + data[1:])


# =================================================================================================
# 1. The descriptor
# =================================================================================================

def test_the_descriptor_records_the_pyramid_the_original_the_encoder_and_the_licence(pyramid, tmp_path):
    desc = kmanifest.imagery_pack_descriptor(PACK, pyramid)
    m = json.loads((pyramid / "manifest.json").read_text())
    img = desc["imagery"]
    assert img["pyramid_sha256"] == m["pyramid_sha256"]
    assert img["file_count"] == len(m["files"]) == 11 and img["tile_count"] == 10
    assert img["total_bytes"] == m["total_bytes"]
    assert img["original"] == {"file": "world.jpg", "bytes": 27201049, "sha256": ORIGINAL_SHA256, "url": ORIGINAL_URL,
                               "width": 21600, "height": 10800, "carried": False}
    assert "libjpeg-turbo version 9.9.9" in img["encoder"] and "-quality 88" in img["encoder"]
    assert "public domain in the US" in img["licence"] and "NASA media guidelines" in img["licence"]
    assert "credit NASA Earth Observatory" in img["licence"]
    assert img["licence_spdx"] == "LicenseRef-NASA-Media-Public-Domain"
    assert img["licence_is_code"] is False and img["copyleft"] is False
    # the pack's own fields: the pyramid plus manifest.json, no host path anywhere (Decision L)
    assert desc["file_count"] == 12 and desc["total_bytes"] == img["total_bytes"] + (pyramid / "manifest.json").stat().st_size
    assert desc["content_hash"].startswith("sha256:") and desc["copied"] is False and desc["copy"] is None
    assert desc["source_path"] is None and desc["resolved_real_path"] is None and desc["outside_repository"] is True
    assert str(tmp_path) not in json.dumps(desc)


def test_the_licence_strings_hold_no_copyleft_by_the_inventorys_own_pattern():
    for text in (kmanifest.IMAGERY_LICENCE, kmanifest.IMAGERY_LICENCE_SPDX):
        assert kit_inventory.COPYLEFT_RE.search(text) is None, text


def test_the_pack_is_external_and_not_a_repo_relative_pack():
    assert kmanifest.EXTERNAL_PACKS == {PACK: "--imagery-dir"} and PACK not in kmanifest.PACKS
    assert PACK not in kmanifest.OPERATOR_SUPPLIED_PACKS, "the pyramid is public domain and carried, not operator-supplied"


# =================================================================================================
# 2. The builder: carried and recorded; every refusal happens before anything is written
# =================================================================================================

def test_copy_pack_carries_the_pyramid_and_records_it(pyramid, tmp_path):
    manifest_path, _ = _build(tmp_path, copy_pack_names=[PACK], imagery_dir=pyramid)
    kit = manifest_path.parent
    doc = json.loads(manifest_path.read_text())
    entry = doc["packs"][PACK]
    assert entry["copied"] is True and entry["copy"]["kit_path"] == f"packs/{PACK}"
    assert entry["copy"]["copied_file_count"] == 12 and entry["copy"]["copied_symlink_count"] == 0
    assert entry["imagery"]["pyramid_sha256"] == json.loads((pyramid / "manifest.json").read_text())["pyramid_sha256"]
    carried = sorted(f["path"] for f in doc["files"] if f["path"].startswith(f"packs/{PACK}/"))
    assert len(carried) == 12 and f"packs/{PACK}/manifest.json" in carried and f"packs/{PACK}/body/earth-2048x1024.jpg" in carried
    assert {f["role"] for f in doc["files"] if f["path"] in carried} == {"pack-file"}
    for rel in carried:
        assert (kit / rel).read_bytes() == (pyramid / rel.split("/", 2)[2]).read_bytes()
    # the original is not carried, and nothing names the host's directory
    assert not [f for f in doc["files"] if f["path"].endswith("world.jpg")]
    assert str(pyramid) not in manifest_path.read_text()
    assert kmanifest.verify_manifest(kit) == []
    assert doc["pack_symlinks"] == []


def test_with_pack_records_the_verified_descriptor_without_the_bytes(pyramid, tmp_path):
    manifest_path, _ = _build(tmp_path, with_pack_names=[PACK], imagery_dir=pyramid)
    doc = json.loads(manifest_path.read_text())
    assert doc["packs"][PACK]["copied"] is False and doc["packs"][PACK]["imagery"]["file_count"] == 11
    assert not (manifest_path.parent / "packs" / PACK).exists()


def test_a_tampered_tile_is_refused_at_build_and_nothing_is_written(pyramid, tmp_path, capsys):
    _flip_first_byte(pyramid / "1" / "2" / "1.jpg")
    out = tmp_path / "kit"
    with pytest.raises(kmanifest.ImageryPyramidError) as exc:
        _build(tmp_path, copy_pack_names=[PACK], imagery_dir=pyramid)
    assert "1/2/1.jpg" in str(exc.value) and "sha256" in str(exc.value)
    assert not out.exists()

    rc = build_kit.main(["--out", str(out), "--copy-pack", PACK, "--imagery-dir", str(pyramid)])
    err = capsys.readouterr().err
    assert rc == 2 and "REFUSED" in err and "1/2/1.jpg" in err and not out.exists()
    # the same tamper is refused for the descriptor-only form
    with pytest.raises(kmanifest.ImageryPyramidError):
        _build(tmp_path, name="kit2", with_pack_names=[PACK], imagery_dir=pyramid)


def test_an_incomplete_or_altered_pyramid_is_refused(tmp_path):
    for index, (label, make_damage) in enumerate(_damages().items()):
        root = make_pyramid(tmp_path / f"damaged-{index}")
        make_damage(root)
        with pytest.raises(kmanifest.ImageryPyramidError):
            _build(tmp_path, name=f"kit-{index}", copy_pack_names=[PACK], imagery_dir=root)
        assert not (tmp_path / f"kit-{index}").exists(), label


def _edit_manifest(root: Path, **changes) -> None:
    doc = json.loads((root / "manifest.json").read_text())
    doc.update(changes)
    (root / "manifest.json").write_text(json.dumps(doc, indent=1, sort_keys=True) + "\n", encoding="utf-8")


def _damages() -> dict:
    return {
        "a missing tile": lambda r: (r / "0" / "1" / "0.jpg").unlink(),
        "a missing body image": lambda r: (r / "body" / "earth-2048x1024.jpg").unlink(),
        "an extra file": lambda r: (r / "1" / "0" / "stray.jpg").write_bytes(b"x"),
        "a symlink": lambda r: (r / "1" / "0" / "link.jpg").symlink_to("0.jpg"),
        "a bad pyramid_sha256": lambda r: _edit_manifest(r, pyramid_sha256="0" * 64),
        "a bad tile_count": lambda r: _edit_manifest(r, tile_count=999),
        "a wrong format": lambda r: _edit_manifest(r, format="something/9"),
        "a missing manifest": lambda r: (r / "manifest.json").unlink(),
        "a path that climbs out": lambda r: _edit_manifest(r, files={**json.loads((r / "manifest.json").read_text())["files"],
                                                                   "../outside.jpg": {"bytes": 1, "sha256": "0" * 64}}),
        "an absurd level count": lambda r: _edit_manifest(r, max_level=60),
    }


def test_a_missing_directory_is_refused_saying_how_to_build_it(tmp_path):
    with pytest.raises(kmanifest.ImageryPyramidError, match="build_blue_marble_tiles.py"):
        _build(tmp_path, copy_pack_names=[PACK], imagery_dir=tmp_path / "nowhere")
    assert not (tmp_path / "kit").exists()


def test_a_pyramid_over_max_pack_bytes_is_refused_not_skipped(tmp_path, capsys):
    # larger than data-time (10184 bytes, the default pack the cap also applies to), so only the pyramid trips it
    pyramid = make_pyramid(tmp_path / "big", body_repeat=2000)
    total = kmanifest.imagery_pack_descriptor(PACK, pyramid)["total_bytes"]
    with pytest.raises(kmanifest.PackTooLargeError, match="--max-pack-bytes"):
        _build(tmp_path, copy_pack_names=[PACK], imagery_dir=pyramid, max_pack_bytes=total - 1)
    assert not (tmp_path / "kit").exists()
    rc = build_kit.main(["--out", str(tmp_path / "kit"), "--copy-pack", PACK, "--imagery-dir", str(pyramid),
                         "--max-pack-bytes", str(total - 1)])
    assert rc == 2 and "REFUSED" in capsys.readouterr().err and not (tmp_path / "kit").exists()
    # exactly at the cap is allowed
    _build(tmp_path, name="ok", copy_pack_names=[PACK], imagery_dir=pyramid, max_pack_bytes=total)


def test_the_default_cap_is_unchanged_and_the_real_pyramid_fits_under_it():
    assert kmanifest.DEFAULT_MAX_PACK_BYTES == 50_000_000
    assert 20_600_766 + 333_376 < kmanifest.DEFAULT_MAX_PACK_BYTES  # 2731 files plus manifest.json, measured


def test_imagery_dir_without_the_pack_is_refused(pyramid, tmp_path):
    with pytest.raises(kmanifest.ImageryPyramidError, match="--copy-pack imagery-bluemarble"):
        _build(tmp_path, imagery_dir=pyramid)
    assert not (tmp_path / "kit").exists()


def test_the_default_source_is_under_the_home_directory():
    assert build_kit.default_imagery_dir() == Path.home() / "altavista-data" / "imagery" / "bluemarble"


# =================================================================================================
# 3. The installer
# =================================================================================================

def _kit_with_pyramid(tmp_path: Path, pyramid: Path) -> Path:
    manifest_path, _ = _build(tmp_path, copy_pack_names=[PACK], imagery_dir=pyramid)
    return manifest_path.parent.resolve()


def test_install_verifies_the_carried_pyramid_and_records_how_to_start_the_viewer(pyramid, tmp_path, monkeypatch, capsys):
    kit = _kit_with_pyramid(tmp_path, pyramid)
    _stub_the_wheel_steps(monkeypatch)
    target = tmp_path / "target"
    assert install.main([str(kit), str(target)]) == 0
    out = capsys.readouterr().out
    record = json.loads((target / "INSTALL_RECORD").read_text())
    imagery = record["installed"]["imagery"]
    assert imagery["carried"] is True and imagery["verified_in_kit"] is True and imagery["verified_after_copy"] is True
    assert imagery["pyramid_sha256"] == json.loads((pyramid / "manifest.json").read_text())["pyramid_sha256"]
    assert imagery["file_count"] == 11 and imagery["installed_path"] == f"kit/packs/{PACK}"
    assert f"--imagery-dir <target>/kit/packs/{PACK}" in imagery["start_viewer_with"]
    assert imagery["licence_spdx"] == "LicenseRef-NASA-Media-Public-Domain"
    assert PACK in record["installed"]["packs"]
    installed = target / "kit" / "packs" / PACK
    assert kmanifest.verify_imagery_pyramid(installed) == []
    assert f"--imagery-dir {target.resolve() / 'kit' / 'packs' / PACK}" in out and "Blue Marble basemap verified" in out


def test_a_tampered_tile_in_the_kit_is_refused_by_the_kit_check_and_no_target_is_made(pyramid, tmp_path, monkeypatch):
    kit = _kit_with_pyramid(tmp_path, pyramid)
    _stub_the_wheel_steps(monkeypatch)
    _flip_first_byte(kit / "packs" / PACK / "1" / "3" / "1.jpg")
    target = tmp_path / "target"
    with pytest.raises(install.InstallRefused, match="does NOT verify"):
        install.install(kit, target)
    assert not target.exists()


def _repair_kit_manifest_for(kit: Path, rel: str) -> None:
    """What an attacker who also rewrites KIT_MANIFEST would do: the kit then verifies against itself."""
    path = kit / "KIT_MANIFEST"
    doc = json.loads(path.read_text())
    entry = next(f for f in doc["files"] if f["path"] == rel)
    data = (kit / rel).read_bytes()
    entry["sha256"], entry["size"] = _sha(data), len(data)
    path.write_text(json.dumps(doc, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def test_the_pyramid_check_is_independent_of_the_kit_check(pyramid, tmp_path, monkeypatch):
    """A tile changed AND KIT_MANIFEST rewritten to match passes the kit's own consistency check; the
    pyramid's own manifest still refuses it, with a typed code, before the target directory exists."""
    kit = _kit_with_pyramid(tmp_path, pyramid)
    _stub_the_wheel_steps(monkeypatch)
    rel = f"packs/{PACK}/1/3/1.jpg"
    _flip_first_byte(kit / rel)
    _repair_kit_manifest_for(kit, rel)
    assert kmanifest.verify_manifest(kit) == []
    target = tmp_path / "target"
    with pytest.raises(install.ImageryRefused) as exc:
        install.install(kit, target)
    assert exc.value.code == "imagery-pyramid-differs" and "1/3/1.jpg" in str(exc.value)
    assert not target.exists()


def test_a_pyramid_whose_own_manifest_was_rewritten_to_match_is_refused_by_the_recorded_descriptor(pyramid, tmp_path, monkeypatch):
    """Tile, pyramid manifest and KIT_MANIFEST all rewritten consistently: only the descriptor recorded when the
    kit was built (pyramid_sha256, counts) still says what the pyramid was."""
    kit = _kit_with_pyramid(tmp_path, pyramid)
    _stub_the_wheel_steps(monkeypatch)
    tile = kit / "packs" / PACK / "0" / "0" / "0.jpg"
    tile.write_bytes(b"a different tile\n")
    root = kit / "packs" / PACK
    m = json.loads((root / "manifest.json").read_text())
    m["files"]["0/0/0.jpg"] = {"bytes": tile.stat().st_size, "sha256": _sha(tile.read_bytes())}
    m["total_bytes"] = sum(f["bytes"] for f in m["files"].values())
    m["pyramid_sha256"] = kmanifest._imagery_pyramid_digest(m["files"])
    (root / "manifest.json").write_text(json.dumps(m, indent=1, sort_keys=True) + "\n", encoding="utf-8")
    for rel in (f"packs/{PACK}/0/0/0.jpg", f"packs/{PACK}/manifest.json"):
        _repair_kit_manifest_for(kit, rel)
    assert kmanifest.verify_manifest(kit) == [] and kmanifest.verify_imagery_pyramid(root) == []
    target = tmp_path / "target"
    with pytest.raises(install.ImageryRefused) as exc:
        install.install(kit, target)
    assert exc.value.code == "imagery-descriptor-differs" and "pyramid_sha256" in str(exc.value)
    assert not target.exists()


def test_an_incomplete_carried_pyramid_is_refused_at_install(pyramid, tmp_path, monkeypatch):
    kit = _kit_with_pyramid(tmp_path, pyramid)
    _stub_the_wheel_steps(monkeypatch)
    # a tile removed from the kit AND from KIT_MANIFEST.files: the kit is consistent, the pyramid is not complete
    rel = f"packs/{PACK}/0/1/0.jpg"
    (kit / rel).unlink()
    path = kit / "KIT_MANIFEST"
    doc = json.loads(path.read_text())
    doc["files"] = [f for f in doc["files"] if f["path"] != rel]
    path.write_text(json.dumps(doc, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    assert kmanifest.verify_manifest(kit) == []
    target = tmp_path / "target"
    with pytest.raises(install.ImageryRefused) as exc:
        install.install(kit, target)
    assert exc.value.code == "imagery-pyramid-differs" and "0/1/0.jpg" in str(exc.value)
    assert not target.exists()
    assert install.main([str(kit), str(target)]) == 2 and not target.exists()


def test_a_kit_without_the_pack_still_installs_and_says_the_basemap_is_not_installed(tmp_path, monkeypatch, capsys):
    manifest_path, _ = _build(tmp_path)
    kit = manifest_path.parent.resolve()
    doc = json.loads(manifest_path.read_text())
    assert PACK not in doc["packs"] and not (kit / "packs" / PACK).exists()
    _stub_the_wheel_steps(monkeypatch)
    target = tmp_path / "target"
    assert install.main([str(kit), str(target)]) == 0
    out = capsys.readouterr().out
    imagery = json.loads((target / "INSTALL_RECORD").read_text())["installed"]["imagery"]
    assert imagery["carried"] is False and "without --copy-pack imagery-bluemarble" in imagery["reason"]
    assert "basemap is not installed" in imagery["effect"] and "--imagery-dir" in imagery["add_it"]
    assert "not carried by this kit" in out and "will say the basemap is not installed" in out
    assert not (target / "kit" / "packs" / PACK).exists()


def test_a_kit_with_the_descriptor_only_installs_without_the_bytes(pyramid, tmp_path, monkeypatch):
    manifest_path, _ = _build(tmp_path, with_pack_names=[PACK], imagery_dir=pyramid)
    _stub_the_wheel_steps(monkeypatch)
    record = install.install(manifest_path.parent.resolve(), tmp_path / "target")
    assert record["installed"]["imagery"]["carried"] is False
    assert "descriptor only" in record["installed"]["imagery"]["reason"]
    assert PACK in record["installed"]["packs_descriptor_only"]


# =================================================================================================
# 4. The licence is visible wherever the kit lists what it carries
# =================================================================================================

def test_the_inventory_lists_the_pyramids_licence_for_every_file(pyramid, tmp_path):
    manifest_path, _ = _build(tmp_path, copy_pack_names=[PACK], imagery_dir=pyramid)
    kit = manifest_path.parent
    doc = json.loads(manifest_path.read_text())
    entries = [f for f in doc["files"] if f["path"].startswith(f"packs/{PACK}/")]
    assert len(entries) == 12
    for entry in entries:
        origin = kit_inventory.origin_of(kit, doc, entry, {}, {})
        assert "NASA Blue Marble" in origin and "public domain in the US" in origin and "NASA media guidelines" in origin
        assert "credit NASA Earth Observatory" in origin and "LicenseRef-NASA-Media-Public-Domain" in origin
        assert kit_inventory.COPYLEFT_RE.search(origin) is None, origin
    # the other pack's origin text is unchanged
    data_time = {"path": "packs/data-time/x", "role": "pack-file"}
    assert "leap-second" in kit_inventory.origin_of(kit, doc, data_time, {}, {})


# =================================================================================================
# 5. The kit's verifier and the generator's --verify agree (they are two implementations on purpose)
# =================================================================================================

def _generator():
    pytest.importorskip("numpy", reason="the generator imports numpy; the parity check needs it")
    spec = importlib.util.spec_from_file_location("build_blue_marble_tiles_for_parity", GENERATOR)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test_the_constants_equal_the_generators():
    gen = _generator()
    assert kmanifest.IMAGERY_FORMAT == gen.FORMAT and kmanifest.IMAGERY_BODY_FILE == gen.BODY_FILE


@pytest.mark.parametrize("label", [
    "intact", "a flipped byte", "a missing tile", "a missing body image", "an extra file",
    "a bad pyramid_sha256", "a bad tile_count", "a wrong format", "a missing manifest",
])
def test_the_kits_verifier_reports_what_the_generators_verify_reports(tmp_path, label):
    gen = _generator()
    root = make_pyramid(tmp_path / "p")
    damage = {
        "intact": lambda r: None,
        "a flipped byte": lambda r: _flip_first_byte(r / "1" / "2" / "1.jpg"),
        **{k: v for k, v in _damages().items() if k == label},
    }[label]
    damage(root)
    theirs = gen.verify(root, log=lambda *_: None)
    ours = kmanifest.verify_imagery_pyramid(root)
    if label == "a missing manifest":   # the generator names the full path; the kit's verifier the file name
        assert ours and theirs
    else:
        assert ours == theirs
    assert bool(ours) == (label != "intact")


# =================================================================================================
# 6. Opt-in: the real pyramid (skips with its reason when it is absent)
# =================================================================================================

def test_the_real_pyramid_builds_into_a_kit_and_installs(tmp_path, monkeypatch):
    real = build_kit.default_imagery_dir()
    if not (real / "manifest.json").is_file():
        pytest.skip(f"the real Blue Marble pyramid is not at {real} (scripts/imagery/build_blue_marble_tiles.py builds it)")
    manifest_path, _ = _build(tmp_path, copy_pack_names=[PACK], imagery_dir=real)
    kit = manifest_path.parent.resolve()
    doc = json.loads(manifest_path.read_text())
    img = doc["packs"][PACK]["imagery"]
    assert img["pyramid_sha256"] == REAL_PYRAMID_SHA256, "the carried pyramid is not the recorded one"
    assert img["file_count"] == 2731 and img["tile_count"] == 2730 and img["total_bytes"] == 20_600_766
    assert img["original"]["sha256"] == ORIGINAL_SHA256 and img["original"]["url"].startswith("https://")
    assert doc["packs"][PACK]["total_bytes"] < kmanifest.DEFAULT_MAX_PACK_BYTES
    assert kmanifest.verify_manifest(kit) == []
    _stub_the_wheel_steps(monkeypatch)
    record = install.install(kit, tmp_path / "target")
    assert record["installed"]["imagery"]["pyramid_sha256"] == REAL_PYRAMID_SHA256
