"""Question 245 (heavy-ct round 1a) and question 247: the pinned ClickHouse server image's record
(`services/clickhouse/IMAGE_DIGEST.md`, `IMAGE_PACKAGES.txt`), and the ruling that the air-gap kit
does NOT carry the image.

Question 247: the user holds the ClickHouse image out of the kit pending a licence review of the
statically linked LGPL libraries in its binary. The record, the package inventory and the licence
section stay as input for that review; the kit's image components are `edge-plugin-image` and
`cfs-image` only.

Everything here reads committed files; nothing touches Docker, git history or the network.
"""
from __future__ import annotations

import importlib.util
import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
KIT_DIR = REPO_ROOT / "scripts" / "kit"
CLICKHOUSE_DIR = REPO_ROOT / "services" / "clickhouse"
DIGEST_MD = CLICKHOUSE_DIR / "IMAGE_DIGEST.md"
PACKAGES_TXT = CLICKHOUSE_DIR / "IMAGE_PACKAGES.txt"
sys.path.insert(0, str(KIT_DIR))
import build_kit  # noqa: E402  (path insert must precede this import)
import sbom  # noqa: E402


def _extractor():
    spec = importlib.util.spec_from_file_location(
        "extract_image_packages", CLICKHOUSE_DIR / "extract_image_packages.py"
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def _fenced_block_after(text: str, anchor: str) -> str:
    """The first ``` fenced block after the first line that contains `anchor`."""
    after = text[text.index(anchor):]
    return re.search(r"```[^\n]*\n(.*?)\n```", after, re.S).group(1).strip()


def _inventory(text: str) -> tuple[str, int, list[tuple[str, str, str]]]:
    """`(os_name, declared_count, rows)` read from IMAGE_PACKAGES.txt's header and rows."""
    os_name = re.search(r"^# os:\s*(.+)$", text, re.M).group(1).strip()
    declared = int(re.search(r"^# packages: (\d+)$", text, re.M).group(1))
    rows = []
    for line in text.splitlines():
        if line and not line.startswith("#"):
            name, version, arch = line.split("\t")
            rows.append((name, version, arch))
    return os_name, declared, rows


def test_the_record_is_pinned_by_digest_and_internally_consistent():
    text = DIGEST_MD.read_text()
    ref = _fenced_block_after(text, "Registry reference")
    image_id = _fenced_block_after(text, "docker image inspect")
    tag = re.search(r"Tag this digest carried when it was pulled:\s*`([^`]+)`", text).group(1)

    assert re.fullmatch(r"clickhouse/clickhouse-server@sha256:[0-9a-f]{64}", ref), ref
    assert re.fullmatch(r"sha256:[0-9a-f]{64}", image_id), image_id
    # On this host's containerd image store the image id IS the index digest the reference pins.
    assert ref.partition("@")[2] == image_id
    assert tag.startswith("clickhouse/clickhouse-server:"), tag
    assert re.search(r"arm64 manifest digest:\s*`sha256:[0-9a-f]{64}`", text)
    assert re.search(r"Licence \(SPDX\):\s*`Apache-2.0`", text)


def test_the_record_says_the_kit_does_not_carry_the_image_and_keeps_the_licence_inventory():
    text = DIGEST_MD.read_text()
    assert "does NOT carry this image" in text and "question 247" in text.lower()
    # The licence review's input stays: the section and the statically linked LGPL libraries.
    assert "## Licence" in text and "## Package inventory" in text
    for library in ("lemmagen-c", "libgsasl", "libssh", "mariadb-connector-c", "numactl", "xz"):
        assert f"`{library}`" in text, library


def test_the_kit_does_not_carry_the_clickhouse_image():
    """Question 247: the ClickHouse image is held out of the air-gap kit pending the user's licence
    review. Putting it back into either table is a deliberate change that needs the user's ruling:
    update this test and `services/clickhouse/IMAGE_DIGEST.md` in the same change."""
    assert "clickhouse-image" not in build_kit.IMAGE_DIGEST_SOURCES, (
        "question 247: the ClickHouse image is held out of the kit pending the user's licence review"
    )
    assert "clickhouse-image" not in sbom.IMAGE_COMPONENTS, (
        "question 247: the ClickHouse image is held out of the kit pending the user's licence review"
    )
    assert "clickhouse-image" not in sbom.all_component_names(), "question 247: held out of the kit"


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


def test_the_committed_inventory_parses_and_matches_its_header():
    os_name, declared, rows = _inventory(PACKAGES_TXT.read_text())
    assert os_name.startswith("Ubuntu 22.04")
    assert len(rows) == declared == 111
    assert rows == sorted(rows)
    own = {name for name, _, _ in rows if name.startswith("clickhouse-")}
    assert own == {"clickhouse-client", "clickhouse-common-static", "clickhouse-server"}
