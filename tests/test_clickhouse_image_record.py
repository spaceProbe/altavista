"""Question 245, heavy-ct round 1a: the pinned ClickHouse server image's record
(`services/clickhouse/IMAGE_DIGEST.md`, `IMAGE_PACKAGES.txt`) and how the air-gap kit carries it
(`scripts/kit/build_kit.py::IMAGE_DIGEST_SOURCES`, `scripts/kit/sbom.py::_clickhouse_image_sbom`).

Everything here reads committed files or writes to `tmp_path`; nothing touches Docker, git history
or the network. The live comparison (the local image id against the record, then `docker save`) is
the existing opt-in `tests/test_kit_manifest.py::test_images_step_with_the_flag_set_runs_the_real_
docker_save_path` (`AV_KIT_WITH_IMAGES=1`), which loops over `sbom.IMAGE_COMPONENTS` and so covers
this component too. The SBOM generator reads the git epoch; these tests pin it so they do not
depend on whether the record has been committed yet.
"""
from __future__ import annotations

import importlib.util
import re
import sys
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
KIT_DIR = REPO_ROOT / "scripts" / "kit"
CLICKHOUSE_DIR = REPO_ROOT / "services" / "clickhouse"
DIGEST_MD = CLICKHOUSE_DIR / "IMAGE_DIGEST.md"
PACKAGES_TXT = CLICKHOUSE_DIR / "IMAGE_PACKAGES.txt"
sys.path.insert(0, str(KIT_DIR))
import build_kit  # noqa: E402  (path insert must precede this import)
import sbom  # noqa: E402

FIXED_EPOCH = "2026-10-17T00:00:00Z"


def _extractor():
    spec = importlib.util.spec_from_file_location(
        "extract_image_packages", CLICKHOUSE_DIR / "extract_image_packages.py"
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


@pytest.fixture
def clickhouse_sbom(monkeypatch):
    monkeypatch.setattr(sbom, "git_epoch", lambda paths: FIXED_EPOCH)
    return sbom.image_sbom("clickhouse-image")


def test_the_record_is_pinned_by_digest_and_internally_consistent():
    text = DIGEST_MD.read_text()
    ref = sbom._fenced_block_after(text, "Registry reference")
    image_id = sbom._fenced_block_after(text, "docker image inspect")
    tag = re.search(r"Tag this digest carried when it was pulled:\s*`([^`]+)`", text).group(1)

    assert re.fullmatch(r"clickhouse/clickhouse-server@sha256:[0-9a-f]{64}", ref), ref
    assert re.fullmatch(r"sha256:[0-9a-f]{64}", image_id), image_id
    # On this host's containerd image store the image id IS the index digest the reference pins.
    assert ref.partition("@")[2] == image_id
    assert tag.startswith("clickhouse/clickhouse-server:"), tag
    assert re.search(r"arm64 manifest digest:\s*`sha256:[0-9a-f]{64}`", text)
    assert re.search(r"Licence \(SPDX\):\s*`Apache-2.0`", text)


def test_the_kit_declares_the_clickhouse_image_beside_the_other_two():
    assert "clickhouse-image" in sbom.IMAGE_COMPONENTS
    assert "clickhouse-image" in sbom.all_component_names()
    assert build_kit.IMAGE_DIGEST_SOURCES["clickhouse-image"] == Path("services/clickhouse/IMAGE_DIGEST.md")
    assert set(build_kit.IMAGE_DIGEST_SOURCES) == set(sbom.IMAGE_COMPONENTS)
    assert sbom.epoch_paths_for("clickhouse-image") == [
        "services/clickhouse/IMAGE_DIGEST.md", "services/clickhouse/IMAGE_PACKAGES.txt",
    ]


def test_the_kit_copies_the_record_byte_for_byte(tmp_path):
    build_kit.assemble_image_digest_docs(REPO_ROOT, tmp_path)
    copied = tmp_path / "services" / "clickhouse" / "IMAGE_DIGEST.md"
    assert copied.read_bytes() == DIGEST_MD.read_bytes()


def test_manifest_reads_the_recorded_tag_and_digest(monkeypatch, clickhouse_sbom):
    from manifest import read_recorded_image  # scripts/kit is on sys.path (above)

    recorded = read_recorded_image("clickhouse-image")
    text = DIGEST_MD.read_text()
    assert recorded["recorded_digest"] == sbom._fenced_block_after(text, "docker image inspect")
    assert recorded["tag"] == re.search(r"Tag this digest carried when it was pulled:\s*`([^`]+)`", text).group(1)


def test_the_sbom_carries_the_licence_and_names_what_is_not_known(clickhouse_sbom):
    doc = clickhouse_sbom
    meta = doc["metadata"]["component"]
    assert meta["name"] == "clickhouse-image" and meta["type"] == "container"
    assert meta["licenses"] == [{"license": {"id": "Apache-2.0"}}]
    gaps = [p["value"] for p in meta["properties"] if p["name"] == sbom.PROP_GAP]
    assert len(gaps) == 3
    assert any("did not build" in g for g in gaps)
    assert any("statically linked" in g for g in gaps)
    assert any("licences are not read" in g for g in gaps)

    components = doc["components"]
    declared = int(re.search(r"^# packages: (\d+)$", PACKAGES_TXT.read_text(), re.M).group(1))
    assert len(components) == declared == 111
    own = [c for c in components if c["name"].startswith("clickhouse-")]
    assert {c["name"] for c in own} == {"clickhouse-client", "clickhouse-common-static", "clickhouse-server"}
    for c in components:
        if c in own:
            assert c["licenses"] == [{"license": {"id": "Apache-2.0"}}], c
        else:
            assert any(p["name"] == sbom.PROP_NO_LICENCE for p in c["properties"]), c
        assert c["purl"].startswith("pkg:deb/ubuntu/"), c


def test_the_sbom_regenerates_byte_identically(tmp_path, monkeypatch):
    monkeypatch.setattr(sbom, "git_epoch", lambda paths: FIXED_EPOCH)
    sbom.write_document(sbom.generate_one("clickhouse-image"), tmp_path / "a.json")
    sbom.write_document(sbom.generate_one("clickhouse-image"), tmp_path / "b.json")
    assert (tmp_path / "a.json").read_bytes() == (tmp_path / "b.json").read_bytes()


def test_extract_image_packages_keeps_only_installed_packages_and_is_deterministic():
    extractor = _extractor()
    status = (
        "Package: zlib1g\nStatus: install ok installed\nArchitecture: arm64\nVersion: 1:1.2.11\n"
        "Description: compression library\n runtime\n \n.\n more\n\n"
        "Package: gone\nStatus: deinstall ok config-files\nArchitecture: arm64\nVersion: 9\n\n"
        "Package: adduser\nStatus: install ok installed\nArchitecture: all\nVersion: 3.118\n"
    )
    assert extractor.installed_packages(status) == [("adduser", "3.118", "all"), ("zlib1g", "1:1.2.11", "arm64")]
    rendered = extractor.render(extractor.installed_packages(status), "img@sha256:" + "0" * 64, "Ubuntu")
    assert rendered == extractor.render(extractor.installed_packages(status), "img@sha256:" + "0" * 64, "Ubuntu")
    assert "# packages: 2\n" in rendered and "adduser\t3.118\tall\n" in rendered


def test_the_committed_inventory_parses_with_the_generators_own_reader():
    os_name, rows = sbom._clickhouse_package_inventory(PACKAGES_TXT.read_text())
    assert os_name.startswith("Ubuntu 22.04")
    assert len(rows) == 111
    assert rows == sorted(rows)
