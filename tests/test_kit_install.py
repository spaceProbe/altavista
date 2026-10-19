"""Question 250(a), task 2: GMAT is an operator download. Unit tests for the three halves: the pins
file (`scripts/kit/operator_supplied.json`, component `gmat-r2026a`), the builder (`--with-pack gmat`
records a descriptor and never copies; `--copy-pack gmat` is refused) and the installer
(`install.verify_operator_gmat`, `--gmat-root` / `--gmat-platform`).

Everything here runs over a small synthetic tree and a synthetic pin written under `tmp_path`; nothing
reads the real GMAT tree except the two opt-in tests at the end (`AV_KIT_MEASURE_GMAT=1` for macOS, ~770 MB read,
and `AV_KIT_MEASURE_GMAT_LINUX=1` for Linux x86_64, ~690 MB, question 254 task 12; both read-only). The wheel steps of `install.install` need a kit built with network access, so the
end-to-end tests replace exactly those three functions and run everything else for real.
"""
from __future__ import annotations

import json
import os
import shutil
import sys
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
KIT_DIR = REPO_ROOT / "scripts" / "kit"
sys.path.insert(0, str(KIT_DIR))
import build_kit  # noqa: E402  (path insert must precede this import)
import install  # noqa: E402
import manifest as kmanifest  # noqa: E402

SCOPE = {"exclude_dir_names": ["__pycache__"], "exclude_paths": ["output", "bin/api_startup_file.txt"]}
PLATFORM = "testos-x1"
MARKER_NAME = "gmat_synthetic_marker_file.txt"


def _write(path: Path, data: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)


def _make_tree(root: Path) -> Path:
    """A small GMAT-shaped tree: real files, one file link and one directory link, and everything
    the scope leaves out (a run log, the generated startup file, bytecode at two depths)."""
    _write(root / "README.txt", b"readme\n")
    _write(root / "bin" / "GmatConsole.bin", b"\x00\x01\x02binary")
    _write(root / "bin" / "libfoo.3.dylib", b"foo 3")
    _write(root / "data" / "with space" / "kernel.bsp", b"kernel")
    _write(root / MARKER_NAME, b"marker\n")
    (root / "bin" / "libfoo.dylib").symlink_to("libfoo.3.dylib")
    (root / "data" / "alias").symlink_to("with space")
    _write(root / "output" / "GmatLog.txt", b"run log 1\n")
    _write(root / "bin" / "api_startup_file.txt", b"PLUGIN = /somewhere/plugins/libX\n")
    _write(root / "bin" / "__pycache__" / "m.cpython-313.pyc", b"pyc1")
    _write(root / "__pycache__" / "n.pyc", b"pyc2")
    return root


def _pins_doc(tree: Path) -> dict:
    """The real pins file with GMAT replaced by a synthetic pin for `tree` (task 3: the installer
    validates and lists all six components, so a pins file with GMAT alone is no longer a pins file)."""
    doc = json.loads((KIT_DIR / "operator_supplied.json").read_text(encoding="utf-8"))
    doc["components"]["gmat-r2026a"] = {
        "item": 3, "kind": "data-tree", "pack": "gmat", "scope": SCOPE,
        "licences": "synthetic", "verification": {"step": "files", "method": "synthetic"},
        "why": "synthetic pin for a test", "not_covered": ["a thing the test knows is not covered"],
        "platforms": {PLATFORM: kmanifest.measure_tree(tree, SCOPE), "testos-x2": None},
    }
    return doc


@pytest.fixture()
def synthetic(tmp_path, monkeypatch):
    """(tree, pins_path): a synthetic tree, a pins file pinning it for PLATFORM, and
    `manifest.PINS_FILE` pointed at that file for the duration of the test."""
    tree = _make_tree(tmp_path / "gmat tree")
    pins_path = tmp_path / "pins.json"
    pins_path.write_text(json.dumps(_pins_doc(tree)), encoding="utf-8")
    monkeypatch.setattr(kmanifest, "PINS_FILE", pins_path)
    return tree, pins_path


def _doc_with_gmat(**over) -> dict:
    entry = {"copied": False, "operator_supplied": {"component": "gmat-r2026a"}, **over}
    return {"packs": {"data-time": {"copied": False}, "gmat": entry}}


def _refusal(**kwargs) -> install.OperatorSuppliedRefused:
    with pytest.raises(install.OperatorSuppliedRefused) as exc:
        install.verify_operator_gmat(_doc_with_gmat(), **kwargs)
    return exc.value


# =================================================================================================
# 1. The pins file
# =================================================================================================

def test_the_real_pins_file_records_gmat_per_platform_with_unpinned_platforms_representable():
    doc = json.loads((KIT_DIR / "operator_supplied.json").read_text(encoding="utf-8"))
    component = doc["components"][kmanifest.OPERATOR_SUPPLIED_PACKS["gmat"]]
    assert component["kind"] == "data-tree" and component["pack"] == "gmat" and component["item"] == 3
    assert "item 3" in component["why"] and component["licences"].strip()
    assert kmanifest.pin_problems(component["platforms"]) == []
    assert kmanifest.scope_problems(component["scope"]) == []
    mac = component["platforms"]["macos-arm64"]
    assert set(mac) == set(kmanifest.PIN_FIELDS)
    assert mac["file_count"] > 0 and mac["total_bytes"] > 0
    # "no pin for this platform" is a platform mapped to null
    # (question 254, task 12: linux-x86_64 is pinned now; linux-aarch64 stays null, NASA publishes no build for it)
    linux = component["platforms"]["linux-x86_64"]
    assert set(linux) == set(kmanifest.PIN_FIELDS) and linux["file_count"] > 0 and linux["total_bytes"] > 0
    assert component["platforms"]["linux-aarch64"] is None


def test_pin_problems_names_every_malformed_pin():
    good = {"content_hash": "sha256:" + "a" * 64, "file_count": 1, "total_bytes": 2,
            "symlink_count": 0, "symlink_listing_hash": "sha256:" + "b" * 64}
    assert kmanifest.pin_problems({"p": good, "q": None}) == []
    assert kmanifest.pin_problems({}) and kmanifest.pin_problems(None)
    bad = {**good, "content_hash": "sha256:xyz", "file_count": -1, "total_bytes": True}
    del bad["symlink_count"]
    problems = kmanifest.pin_problems({"p": bad})
    assert len(problems) == 4 and all(p.startswith("p.") for p in problems), problems
    assert kmanifest.scope_problems({"exclude_paths": ["a/b"], "exclude_dir_names": ["c"]}) == []
    assert len(kmanifest.scope_problems({"exclude_paths": ["/abs", "../x"], "exclude_dir_names": ["a/b", ""]})) == 4


# =================================================================================================
# 2. The measurement
# =================================================================================================

def test_measure_tree_without_a_scope_is_pack_descriptor_for_the_fields_they_share(tmp_path):
    tree = _make_tree(tmp_path / "t")
    desc = kmanifest.pack_descriptor("fabricated", tree, repo_root=tmp_path)
    measured = kmanifest.measure_tree(tree)
    for field in ("content_hash", "file_count", "total_bytes"):
        assert measured[field] == desc[field]
    assert measured["symlink_count"] == 2  # the file link and the directory link; pack_descriptor sees neither


def test_the_scope_leaves_out_what_the_operators_own_use_writes_and_nothing_else(tmp_path):
    tree = _make_tree(tmp_path / "t")
    before = kmanifest.measure_tree(tree, SCOPE)
    full = kmanifest.measure_tree(tree)
    assert before["file_count"] == full["file_count"] - 4  # GmatLog.txt, api_startup_file.txt, two .pyc

    # what a GMAT run does to its own tree: a rewritten log, a new report, new bytecode, a new startup file
    (tree / "output" / "GmatLog.txt").write_bytes(b"run log 2, longer\n")
    _write(tree / "output" / "report.txt", b"r")
    _write(tree / "bin" / "__pycache__" / "other.pyc", b"z")
    (tree / "bin" / "api_startup_file.txt").write_bytes(b"PLUGIN = /elsewhere\n")
    assert kmanifest.measure_tree(tree, SCOPE) == before
    assert kmanifest.measure_tree(tree) != full  # the whole-tree descriptor would have drifted

    # a change to a file inside the scope's reach is seen: one byte, a new file, a removed file
    victim = tree / "bin" / "GmatConsole.bin"
    original = victim.read_bytes()
    victim.write_bytes(bytes([original[0] ^ 1]) + original[1:])
    assert kmanifest.compare_to_pin(kmanifest.measure_tree(tree, SCOPE), before) == ["content_hash"]
    victim.write_bytes(original)
    _write(tree / "bin" / "extra.txt", b"x")
    assert set(kmanifest.compare_to_pin(kmanifest.measure_tree(tree, SCOPE), before)) == {
        "content_hash", "file_count", "total_bytes"}


def test_the_symlink_listing_covers_what_the_content_hash_cannot_see(tmp_path):
    tree = _make_tree(tmp_path / "t")
    before = kmanifest.measure_tree(tree, SCOPE)
    link = tree / "bin" / "libfoo.dylib"
    link.unlink()
    link.symlink_to("GmatConsole.bin")  # retargeted: same files, same bytes
    after = kmanifest.measure_tree(tree, SCOPE)
    assert after["content_hash"] == before["content_hash"], "pack_descriptor's own hash cannot see a retargeted link"
    assert kmanifest.compare_to_pin(after, before) == ["symlink_listing_hash"]
    link.unlink()
    assert kmanifest.compare_to_pin(kmanifest.measure_tree(tree, SCOPE), before) == ["symlink_count", "symlink_listing_hash"]


def test_the_symlink_listing_hash_is_unambiguous_for_names_with_spaces_and_arrows():
    a = kmanifest.symlink_listing_hash([("a b", "c")])
    b = kmanifest.symlink_listing_hash([("a", "b c")])
    c = kmanifest.symlink_listing_hash([("a b -> c", "")])
    assert len({a, b, c}) == 3
    assert kmanifest.symlink_listing_hash([]) == "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"


# =================================================================================================
# 3. The installer's check: every refusal, and the match
# =================================================================================================

def test_a_kit_without_packs_gmat_does_not_need_gmat_and_says_so():
    doc = {"packs": {"data-time": {"copied": False}}}
    record = install.verify_operator_gmat(doc, gmat_root=None, gmat_platform=None)
    assert record["required"] is False and record["verified"] is False and "no packs.gmat" in record["reason"]
    given = install.verify_operator_gmat(doc, gmat_root="/somewhere", gmat_platform=None)
    assert given["required"] is False and given["root_given_but_unused"] == "/somewhere"


def test_no_gmat_root_is_refused_naming_the_pins(synthetic):
    tree, _ = synthetic
    pin = kmanifest.measure_tree(tree, SCOPE)
    err = _refusal(gmat_root=None, gmat_platform=None)
    assert err.code == "gmat-root-missing"
    for needle in (PLATFORM, pin["content_hash"], f"{pin['file_count']} files", f"{pin['total_bytes']} bytes"):
        assert needle in str(err), (needle, str(err))
    assert "testos-x2" not in str(err)  # an unpinned platform is not an expected pin


def test_a_root_that_is_not_a_directory_is_refused(synthetic, tmp_path):
    for bad in (tmp_path / "missing", tmp_path / "pins.json"):
        err = _refusal(gmat_root=str(bad), gmat_platform=PLATFORM)
        assert err.code == "gmat-root-not-a-directory" and str(bad) in str(err)


def test_a_root_without_a_platform_is_refused_not_guessed(synthetic):
    tree, _ = synthetic
    err = _refusal(gmat_root=str(tree), gmat_platform=None)
    assert err.code == "gmat-platform-missing" and PLATFORM in str(err) and "never guessed" in str(err)


def test_a_platform_without_a_pin_is_refused_listing_the_pinned_platforms(synthetic):
    tree, _ = synthetic
    for platform in ("testos-x2", "nonesuch-y9"):  # known but unpinned, and unknown
        err = _refusal(gmat_root=str(tree), gmat_platform=platform)
        assert err.code == "gmat-no-pin-for-platform", str(err)
        assert f"Platforms with a pin: ['{PLATFORM}']" in str(err) and repr(platform) in str(err)


def test_a_differing_tree_is_refused_naming_expected_and_found(synthetic):
    tree, _ = synthetic
    pin = kmanifest.measure_tree(tree, SCOPE)
    victim = tree / "README.txt"
    victim.write_bytes(b"readmf\n")  # one byte
    found = kmanifest.measure_tree(tree, SCOPE)
    assert found["content_hash"] != pin["content_hash"] and found["file_count"] == pin["file_count"]
    err = _refusal(gmat_root=str(tree), gmat_platform=PLATFORM)
    assert err.code == "gmat-descriptor-differs"
    text = str(err)
    assert "differs in content_hash" in text and "expected:" in text and "found:" in text
    for needle in (pin["content_hash"], found["content_hash"], f"{pin['file_count']} files",
                   f"{pin['total_bytes']} bytes", f"{found['total_bytes']} bytes"):
        assert needle in text, needle


def test_an_unreadable_tree_is_refused(synthetic, monkeypatch):
    tree, _ = synthetic

    def boom(root, scope=None):
        raise PermissionError(13, "denied")

    monkeypatch.setattr(kmanifest, "measure_tree", boom)
    assert _refusal(gmat_root=str(tree), gmat_platform=PLATFORM).code == "gmat-root-unreadable"


def test_a_kit_whose_gmat_pack_is_not_marked_operator_supplied_is_refused(synthetic):
    tree, _ = synthetic
    for entry in ({"copied": True}, {"operator_supplied": None}, {"operator_supplied": {"component": "other"}}):
        doc = {"packs": {"gmat": {"copied": False, **entry}}}
        with pytest.raises(install.OperatorSuppliedRefused) as exc:
            install.verify_operator_gmat(doc, gmat_root=str(tree), gmat_platform=PLATFORM)
        assert exc.value.code == "gmat-kit-not-operator-supplied", entry


def test_a_broken_pins_file_is_refused(synthetic, tmp_path):
    tree, pins_path = synthetic
    doc = json.loads(pins_path.read_text())
    doc["components"]["gmat-r2026a"]["platforms"][PLATFORM]["file_count"] = "many"
    pins_path.write_text(json.dumps(doc))
    assert _refusal(gmat_root=str(tree), gmat_platform=PLATFORM).code == "gmat-pin-malformed"
    pins_path.write_text("{not json")
    assert _refusal(gmat_root=str(tree), gmat_platform=PLATFORM).code == "gmat-pin-malformed"
    pins_path.write_text(json.dumps({"components": {}}))
    assert _refusal(gmat_root=str(tree), gmat_platform=PLATFORM).code == "gmat-pin-malformed"


def test_a_matching_tree_is_recorded_and_left_alone(synthetic):
    tree, _ = synthetic
    snapshot = sorted((p.relative_to(tree).as_posix(), p.lstat().st_mtime_ns) for p in tree.rglob("*"))
    root_as_given = str(tree) + "/"  # recorded exactly as given
    record = install.verify_operator_gmat(_doc_with_gmat(), gmat_root=root_as_given, gmat_platform=PLATFORM)
    assert record["required"] and record["verified"] and record["copied"] is False
    assert record["platform"] == PLATFORM and record["root"] == root_as_given
    assert record["found"] == record["pin"] == kmanifest.measure_tree(tree, SCOPE)
    assert record["scope"] == SCOPE and record["component"] == "gmat-r2026a" and record["item"] == 3
    assert record["not_covered"]
    assert sorted((p.relative_to(tree).as_posix(), p.lstat().st_mtime_ns) for p in tree.rglob("*")) == snapshot


# =================================================================================================
# 4. The builder: descriptor only, never bytes
# =================================================================================================

def _build(tmp_path: Path, *, name: str = "kit", **kwargs):
    return build_kit.build(
        repo_root=REPO_ROOT, out=tmp_path / name, site=REPO_ROOT / build_kit.DEFAULT_SITE, **kwargs)


def test_with_pack_gmat_records_the_descriptor_and_carries_no_gmat_byte(tmp_path, monkeypatch):
    tree = _make_tree(tmp_path / "gmat tree")
    monkeypatch.setitem(kmanifest.PACKS, "gmat", str(tree))
    kit_root = tmp_path / "kit"
    manifest_path, _ = _build(tmp_path, with_pack_names=["gmat"])
    doc = json.loads(manifest_path.read_text())

    entry = doc["packs"]["gmat"]
    assert entry["copied"] is False and entry["copy"] is None
    whole = kmanifest.pack_descriptor("gmat", tree, repo_root=REPO_ROOT)
    assert {k: entry[k] for k in ("content_hash", "file_count", "total_bytes")} == {
        k: whole[k] for k in ("content_hash", "file_count", "total_bytes")}
    marker = entry["operator_supplied"]
    assert marker["component"] == "gmat-r2026a" and marker["item"] == 3 and "item 3" in marker["reason"]
    assert marker["pins_file"] == "installer/operator_supplied.json"
    assert marker["measured_on_build_host"] == {"scope": SCOPE, **kmanifest.measure_tree(tree, SCOPE)}
    assert marker["platforms_matching_this_descriptor"] == []  # a synthetic tree matches no real platform's pin

    assert not (kit_root / "packs" / "gmat").exists()
    kit_files = [p.relative_to(kit_root).as_posix() for p in kit_root.rglob("*") if p.is_file() or p.is_symlink()]
    assert [f for f in kit_files if f.startswith("packs/gmat")] == []
    assert not any(MARKER_NAME in f for f in kit_files)
    assert doc["pack_symlinks"] == [] and not [f for f in doc["files"] if f["path"].startswith("packs/gmat")]
    assert kmanifest.verify_manifest(kit_root) == []


def test_a_default_kit_has_no_gmat_pack_and_the_other_packs_still_copy(tmp_path):
    manifest_path, _ = _build(tmp_path, copy_pack_names=["data-time"])
    doc = json.loads(manifest_path.read_text())
    assert "gmat" not in doc["packs"]
    assert doc["packs"]["data-time"]["copied"] is True and "operator_supplied" not in doc["packs"]["data-time"]


def test_copy_pack_gmat_is_refused_before_anything_is_written(tmp_path, capsys):
    out = tmp_path / "kit"
    with pytest.raises(kmanifest.OperatorSuppliedPackError, match="--with-pack gmat"):
        build_kit.build(repo_root=REPO_ROOT, out=out, site=REPO_ROOT / build_kit.DEFAULT_SITE,
                        copy_pack_names=["gmat", "data-time"])
    assert not out.exists()

    rc = build_kit.main(["--out", str(out), "--copy-pack", "gmat"])
    err = capsys.readouterr().err
    assert rc == 2 and "REFUSED" in err and "--with-pack gmat" in err and "operator-supplied" in err
    assert not out.exists()


def test_copy_pack_bytes_refuses_an_operator_supplied_pack_directly(tmp_path):
    tree = _make_tree(tmp_path / "t")
    with pytest.raises(kmanifest.OperatorSuppliedPackError):
        kmanifest.copy_pack_bytes("gmat", tree, tmp_path / "kit")
    assert not (tmp_path / "kit").exists()


def test_every_operator_supplied_pack_has_a_pin_component():
    pins = kmanifest.load_operator_pins()
    for name, component_id in kmanifest.OPERATOR_SUPPLIED_PACKS.items():
        assert name in kmanifest.PACKS and pins[component_id]["pack"] == name


# =================================================================================================
# 5. The whole install (wheel steps replaced; everything else real)
# =================================================================================================

@pytest.fixture()
def gmat_kit(tmp_path, synthetic, monkeypatch):
    tree, _ = synthetic
    monkeypatch.setitem(kmanifest.PACKS, "gmat", str(tree))
    manifest_path, _ = _build(tmp_path, with_pack_names=["gmat"])
    monkeypatch.setattr(install, "_check_required_wheels", lambda doc: None)
    monkeypatch.setattr(install, "_check_viewer_assets_in_wheel", lambda kit_dir, doc: None)
    monkeypatch.setattr(install, "_install_wheels_into_venv", lambda kit_dir, target_dir: {"venv": "venv", "installed_packages": []})
    return manifest_path.parent, tree


def test_install_refuses_before_creating_the_target_when_gmat_is_missing_or_wrong(gmat_kit, tmp_path):
    kit, tree = gmat_kit
    target = tmp_path / "target"
    with pytest.raises(install.OperatorSuppliedRefused) as exc:
        install.install(kit, target)
    assert exc.value.code == "gmat-root-missing" and not target.exists()

    (tree / "README.txt").write_bytes(b"changed\n")
    with pytest.raises(install.OperatorSuppliedRefused) as exc:
        install.install(kit, target, gmat_root=str(tree), gmat_platform=PLATFORM)
    assert exc.value.code == "gmat-descriptor-differs" and not target.exists()


def test_install_records_a_verified_gmat_and_copies_none_of_it(gmat_kit, tmp_path, capsys):
    kit, tree = gmat_kit
    target = tmp_path / "target"
    record = install.install(kit, target, gmat_root=str(tree), gmat_platform=PLATFORM)
    gmat = record["installed"]["operator_supplied"]["gmat"]
    assert gmat["verified"] and gmat["platform"] == PLATFORM and gmat["root"] == str(tree)
    assert gmat["found"] == kmanifest.measure_tree(tree, SCOPE) == gmat["pin"]
    assert json.loads((target / "INSTALL_RECORD").read_text())["installed"]["operator_supplied"]["gmat"] == gmat
    assert record["installed"]["packs"] == [] and "gmat" in record["installed"]["packs_descriptor_only"]

    installed = [p.relative_to(target).as_posix() for p in target.rglob("*") if p.is_file() or p.is_symlink()]
    assert not [f for f in installed if f.startswith("kit/packs/gmat") or MARKER_NAME in f]


def test_the_command_line_prints_a_typed_refusal_and_exits_2(gmat_kit, tmp_path, capsys):
    kit, tree = gmat_kit
    target = tmp_path / "target"
    assert install.main([str(kit), str(target)]) == 2
    err = capsys.readouterr().err
    assert "REFUSED" in err and "gmat-root-missing" in err and not target.exists()

    assert install.main([str(kit), str(target), "--gmat-root", str(tree), "--gmat-platform", PLATFORM]) == 0
    out = capsys.readouterr().out
    assert f"verified against the {PLATFORM} pin (not copied)" in out and (target / "INSTALL_RECORD").is_file()


def test_a_kit_without_gmat_installs_without_the_options(tmp_path, monkeypatch):
    manifest_path, _ = _build(tmp_path)
    monkeypatch.setattr(install, "_check_required_wheels", lambda doc: None)
    monkeypatch.setattr(install, "_check_viewer_assets_in_wheel", lambda kit_dir, doc: None)
    monkeypatch.setattr(install, "_install_wheels_into_venv", lambda kit_dir, target_dir: {"venv": "venv", "installed_packages": []})
    record = install.install(manifest_path.parent, tmp_path / "target")
    gmat = record["installed"]["operator_supplied"]["gmat"]
    assert gmat["required"] is False and gmat["verified"] is False


def _stub_the_wheel_steps(monkeypatch):
    monkeypatch.setattr(install, "_check_required_wheels", lambda doc: None)
    monkeypatch.setattr(install, "_check_viewer_assets_in_wheel", lambda kit_dir, doc: None)
    monkeypatch.setattr(install, "_install_wheels_into_venv", lambda kit_dir, target_dir: {"venv": "venv", "installed_packages": []})


def test_the_install_prints_the_path_of_the_operator_supplied_document_first_and_records_it(tmp_path, monkeypatch, capsys):
    """Question 250(a), task 6: the kit's account of what the operator brings is named in the install output
    (before the list it explains) and in INSTALL_RECORD; the installed tree keeps a copy."""
    manifest_path, _ = _build(tmp_path)
    kit = manifest_path.parent.resolve()
    _stub_the_wheel_steps(monkeypatch)
    target = tmp_path / "target"
    assert install.main([str(kit), str(target)]) == 0
    lines = capsys.readouterr().out.splitlines()
    assert lines[0] == f"operator-supplied components document: {kit / 'kit-operator-supplied.md'}"
    assert [l for l in lines[1:] if l.startswith("operator-supplied: ")], "the list follows the document line"
    record = json.loads((target / "INSTALL_RECORD").read_text())["installed"]["operator_supplied_document"]
    entry = kmanifest.operator_doc_entry(json.loads((kit / "KIT_MANIFEST").read_text()))
    assert record == {"kit_path": "kit-operator-supplied.md", "installed_path": "kit/kit-operator-supplied.md", "sha256": entry["sha256"]}
    assert (target / record["installed_path"]).read_bytes() == (REPO_ROOT / "docs" / "kit-operator-supplied.md").read_bytes()

    # --check-operator-supplied names it too
    assert install.main(["--check-operator-supplied", str(kit)]) == 0
    assert capsys.readouterr().out.splitlines()[0] == f"operator-supplied components document: {kit / 'kit-operator-supplied.md'}"


def _kit_without_the_document(tmp_path) -> Path:
    """A kit whose manifest is consistent with its content and simply does not carry the document: what a
    format-5 kit built before this task looks like."""
    manifest_path, _ = _build(tmp_path)
    kit = manifest_path.parent
    doc = json.loads(manifest_path.read_text())
    doc["files"] = [f for f in doc["files"] if f["path"] != "kit-operator-supplied.md"]
    (kit / "kit-operator-supplied.md").unlink()
    manifest_path.write_text(json.dumps(doc, indent=2, sort_keys=True, ensure_ascii=False) + "\n", encoding="utf-8")
    assert kmanifest.verify_manifest(kit) == []
    return kit


def test_the_installer_refuses_a_kit_that_does_not_carry_the_operator_supplied_document(tmp_path, monkeypatch, capsys):
    kit = _kit_without_the_document(tmp_path)
    _stub_the_wheel_steps(monkeypatch)
    target = tmp_path / "target"
    with pytest.raises(install.OperatorSuppliedRefused) as exc:
        install.install(kit, target)
    assert exc.value.code == "operator-doc-missing" and "kit-operator-supplied.md" in str(exc.value) and not target.exists()
    assert install.main([str(kit), str(target)]) == 2
    captured = capsys.readouterr()
    assert "operator-doc-missing" in captured.err and captured.out == "" and not target.exists()
    assert install.main(["--check-operator-supplied", str(kit)]) == 2
    assert "operator-doc-missing" in capsys.readouterr().err


def test_a_listed_operator_supplied_document_that_was_removed_or_changed_is_refused_as_a_tampered_kit(tmp_path, monkeypatch):
    manifest_path, _ = _build(tmp_path)
    kit = manifest_path.parent
    _stub_the_wheel_steps(monkeypatch)
    (kit / "kit-operator-supplied.md").write_bytes(b"not the document")
    with pytest.raises(install.InstallRefused, match="hash_mismatch: kit-operator-supplied.md"):
        install.install(kit, tmp_path / "target")
    (kit / "kit-operator-supplied.md").unlink()
    with pytest.raises(install.InstallRefused, match="missing_file: kit-operator-supplied.md"):
        install.install(kit, tmp_path / "target")
    assert not (tmp_path / "target").exists()


# =================================================================================================
# 6. The real tree (opt-in: reads ~770 MB, read-only)
# =================================================================================================

_REAL_GMAT = os.environ.get("GMAT_ROOT")
_REAL_SKIP = (
    None if os.environ.get("AV_KIT_MEASURE_GMAT") == "1" and _REAL_GMAT and Path(_REAL_GMAT).is_dir()
    else "set AV_KIT_MEASURE_GMAT=1 and GMAT_ROOT to a macOS GMAT R2026a to re-measure the real pin (reads ~770 MB)"
)


@pytest.mark.skipif(_REAL_SKIP is not None, reason=_REAL_SKIP or "")
def test_the_real_gmat_tree_matches_the_recorded_macos_pin():
    component = kmanifest.load_operator_pins()["gmat-r2026a"]
    found = kmanifest.measure_tree(Path(_REAL_GMAT), component["scope"])
    assert kmanifest.compare_to_pin(found, component["platforms"]["macos-arm64"]) == [], found


# Question 254, task 12: the Linux x86_64 pin, measured from NASA's gmat-ubuntu-x64-R2026a.tar.gz (docs/kit-operator-supplied.md,
# item 3). Opt-in the same way; GMAT_LINUX_ROOT is the archive's GMAT/R2026a directory, unpacked and never run.
_LINUX_GMAT = os.environ.get("GMAT_LINUX_ROOT")
_LINUX_SKIP = (
    None if os.environ.get("AV_KIT_MEASURE_GMAT_LINUX") == "1" and _LINUX_GMAT and Path(_LINUX_GMAT).is_dir()
    else "set AV_KIT_MEASURE_GMAT_LINUX=1 and GMAT_LINUX_ROOT to GMAT/R2026a of the unpacked gmat-ubuntu-x64-R2026a.tar.gz "
         "to re-measure the Linux x86_64 pin (reads ~690 MB)"
)


@pytest.mark.skipif(_LINUX_SKIP is not None, reason=_LINUX_SKIP or "")
def test_the_real_linux_gmat_tree_matches_the_recorded_linux_x86_64_pin():
    component = kmanifest.load_operator_pins()["gmat-r2026a"]
    found = kmanifest.measure_tree(Path(_LINUX_GMAT), component["scope"])
    assert kmanifest.compare_to_pin(found, component["platforms"]["linux-x86_64"]) == [], found
