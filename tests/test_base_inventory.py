"""Question 250(a): tests for `scripts/kit/base_inventory.py` -- the parsing and writing of a
base-image inventory, with an in-memory tar and NO docker (the docker half is proved by
regenerating the committed files, see the round's evidence), and the committed inventories'
consistency with the pins file.
"""
from __future__ import annotations

import hashlib
import io
import json
import sys
import tarfile
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(REPO_ROOT / "scripts" / "kit"))
import base_inventory as bi  # noqa: E402


def sha(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def make_tar(members: list[tuple]) -> tarfile.TarFile:
    """members: (kind, name, payload) with kind in dir|file|sym|hard."""
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w") as tf:
        for kind, name, payload in members:
            ti = tarfile.TarInfo(name)
            if kind == "dir":
                ti.type = tarfile.DIRTYPE
                tf.addfile(ti)
            elif kind == "file":
                ti.size = len(payload)
                tf.addfile(ti, io.BytesIO(payload))
            elif kind == "sym":
                ti.type = tarfile.SYMTYPE
                ti.linkname = payload
                tf.addfile(ti)
            elif kind == "hard":
                ti.type = tarfile.LNKTYPE
                ti.linkname = payload
                tf.addfile(ti)
    buf.seek(0)
    return tarfile.open(fileobj=buf, mode="r|")


STATUS = b"""Package: bash
Status: install ok installed
Architecture: arm64
Source: bash (5.2)
Version: 5.2.15-2
Description: shell
 long text

Package: libfoo1
Status: install ok installed
Architecture: arm64
Version: 1.0-1
Description: lib

Package: gone
Status: deinstall ok config-files
Architecture: arm64
Version: 9
"""

DEP5 = b"""Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/
Upstream-Name: foo

Files: *
Copyright: x
License: LGPL-2.1+

License: LGPL-2.1+
 full text

Files: m4/*
License: GPL-2+ or Artistic
"""

NON_DEP5 = b"""This package is free software.
License: GPL-2
"""

OS_RELEASE = b'PRETTY_NAME="Debian GNU/Linux 12"\nID=debian\nVERSION_ID="12"\n'


def image_members() -> list[tuple]:
    return [
        ("dir", "./", None),
        ("dir", "./usr", None),
        ("dir", "./usr/bin", None),
        ("dir", "./usr/lib", None),
        ("dir", "./usr/share", None),
        ("dir", "./usr/share/doc", None),
        ("dir", "./usr/share/doc/bash", None),
        ("dir", "./usr/share/doc/libfoo1", None),
        ("dir", "./etc", None),
        ("dir", "./var/lib/dpkg/info", None),
        ("sym", "./bin", "usr/bin"),                      # usr-merge
        ("file", "./usr/bin/bash", b"bash-binary"),
        ("hard", "./usr/bin/rbash", "./usr/bin/bash"),    # hard link: same content
        ("sym", "./usr/bin/sh", "bash"),                  # a symlink: never a regular file
        ("file", "./usr/lib/libfoo.so.1", b"libfoo"),
        ("file", "./usr/share/doc/bash/copyright", NON_DEP5),
        ("file", "./usr/share/doc/libfoo1/copyright", DEP5),
        ("file", "./usr/lib/os-release", OS_RELEASE),
        ("sym", "./etc/os-release", "../usr/lib/os-release"),
        ("file", "./etc/passwd", b"root:x:0:0\n"),        # owned by no package
        ("file", "./var/lib/dpkg/status", STATUS),
        ("file", "./var/lib/dpkg/info/bash.list",
         b"/.\n/bin\n/bin/bash\n/bin/rbash\n/bin/sh\n/usr/share/doc/bash/copyright\n/usr/share/doc/bash/NEWS.gz\n"),
        ("file", "./var/lib/dpkg/info/libfoo1.list",
         b"/.\n/usr/lib/libfoo.so.1\n/usr/share/doc/libfoo1\n/usr/share/doc/libfoo1/copyright\n"),
    ]


@pytest.fixture()
def inv() -> dict:
    fs = bi.ImageFs.from_tar(make_tar(image_members()))
    return bi.build_inventory(
        fs, image="edge-plugin-image", reference="debian:x@sha256:" + "0" * 64,
        digest="sha256:" + "0" * 64, platform="linux/arm64",
    )


def test_only_installed_packages_are_listed_with_source_and_version(inv):
    assert inv["package_count"] == 2
    assert [(p["name"], p["version"], p["architecture"], p["source"]) for p in inv["packages"]] == [
        ("bash", "5.2.15-2", "arm64", "bash"),
        ("libfoo1", "1.0-1", "arm64", "libfoo1"),
    ]


def test_licences_are_the_distinct_sorted_license_lines_and_dep5_is_detected(inv):
    foo = next(p for p in inv["packages"] if p["name"] == "libfoo1")
    assert foo["licences"] == ["GPL-2+ or Artistic", "LGPL-2.1+"]
    assert foo["machine_readable"] is True and foo["copyright_file"] is True
    bash = next(p for p in inv["packages"] if p["name"] == "bash")
    assert bash["licences"] == ["GPL-2"] and bash["machine_readable"] is False


def test_a_package_with_no_license_line_has_an_empty_list(inv):
    members = [m for m in image_members() if m[1] != "./usr/share/doc/bash/copyright"]
    members.append(("file", "./usr/share/doc/bash/copyright", b"Copyright (C) the authors.\nGNU GPL v2.\n"))
    fs = bi.ImageFs.from_tar(make_tar(members))
    out = bi.build_inventory(fs, image="x", reference="r", digest="d", platform="linux/arm64")
    bash = next(p for p in out["packages"] if p["name"] == "bash")
    assert bash["licences"] == [] and bash["copyright_file"] is True


def test_regular_files_only_through_symlinked_dirs_with_hashes(inv):
    bash = next(p for p in inv["packages"] if p["name"] == "bash")
    paths = dict(bash["files"])
    # /bin -> usr/bin: the listed path is kept, the file is found through the symlink; the
    # symlink /bin/sh, the directory /bin and the absent NEWS.gz are not regular files held.
    assert paths["/bin/bash"] == sha(b"bash-binary")
    assert paths["/bin/rbash"] == sha(b"bash-binary")  # hard link: the same content
    assert "/bin/sh" not in paths and "/bin" not in paths
    assert "/usr/share/doc/bash/NEWS.gz" not in paths
    assert paths["/usr/share/doc/bash/copyright"] == sha(NON_DEP5)


def test_unowned_files_are_regular_files_no_package_lists(inv):
    unowned = dict(inv["unowned_files"])
    assert unowned["/etc/passwd"] == sha(b"root:x:0:0\n")
    assert "/var/lib/dpkg/status" in unowned
    assert "/usr/bin/bash" not in unowned and "/usr/lib/libfoo.so.1" not in unowned
    assert "/usr/bin/rbash" not in unowned  # owned through /bin/rbash


def test_os_release_is_read_through_the_symlink(inv):
    assert inv["os_release"] == {"id": "debian", "version_id": "12"}


def test_copyright_reached_through_a_symlinked_doc_directory():
    members = image_members() + [
        ("dir", "./usr/share/doc/libbar1", None),
    ]
    members = [m for m in members if not m[1].startswith("./usr/share/doc/libfoo1")]
    members.append(("sym", "./usr/share/doc/libfoo1", "bash"))  # libfoo1's docs are bash's
    fs = bi.ImageFs.from_tar(make_tar(members))
    out = bi.build_inventory(fs, image="x", reference="r", digest="d", platform="linux/arm64")
    foo = next(p for p in out["packages"] if p["name"] == "libfoo1")
    assert foo["licences"] == ["GPL-2"]  # bash's copyright, read through the symlink


def test_dump_is_valid_sorted_and_byte_stable(inv):
    text = bi.dump_inventory(inv)
    assert text.endswith("}\n")
    assert json.loads(text) == json.loads(json.dumps(inv))
    again = bi.dump_inventory(json.loads(json.dumps(inv)))
    assert text == again
    # one file entry per line: a change to one file is a one-line diff
    assert text.count('"/bin/bash"') == 1 and '"/bin/bash", "' in text


def test_a_hard_link_to_a_member_not_in_the_archive_is_refused():
    members = [("hard", "./a", "./b")]
    with pytest.raises(RuntimeError):
        bi.ImageFs.from_tar(make_tar(members))


# --- the committed inventories ------------------------------------------------------------------

@pytest.mark.parametrize("image", sorted(bi.IMAGE_SERVICE_DIRS))
def test_the_committed_inventory_matches_the_pinned_base(image):
    path = bi.inventory_path(image)
    assert path.is_file(), f"{path} is missing -- run scripts/kit/base_inventory.py"
    inv = json.loads(path.read_text(encoding="utf-8"))
    _name, pin = bi.base_pin_for(image)
    assert inv["base_image"]["reference"] == pin["reference"]
    assert inv["base_image"]["digest"] == pin["digest"]
    assert pin["reference"].endswith("@" + pin["digest"]), "a pin is a digest, never a tag"
    assert inv["package_count"] == len(inv["packages"]) > 50
    assert [p["name"] for p in inv["packages"]] == sorted(p["name"] for p in inv["packages"])
    for p in inv["packages"]:
        assert p["licences"] == sorted(set(p["licences"]))
        assert p["files"] == sorted(p["files"])
        assert all(len(h) == 64 for _path, h in p["files"])
    assert bi.dump_inventory(inv) == path.read_text(encoding="utf-8"), "not in the canonical form"
