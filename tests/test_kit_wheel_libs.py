"""Question 250(a): the wheel step inventories the native libraries every wheel it fetches bundles
(`scripts/kit/wheel_libs.py`, the committed `scripts/kit/bundled_libs.json`), and a wheel with a library
under a GPL-family licence (GPL, LGPL or AGPL, with or without a `WITH` exception) leaves the kit for the operator-supplied list (`KIT_MANIFEST.wheels.operator_supplied`,
a `python-wheel` pin in `operator_supplied.json`, `install.sh --operator-wheels DIR`).

Nothing here uses the network or Docker. The wheels are small zips written under `tmp_path`; the real
numpy linux/aarch64 wheel is inventoried by the opt-in `--with-wheels` test in `test_kit_manifest.py`
(`AV_KIT_WITH_WHEELS=1`) and by the proof logs of this task.

1. the inventory: which files are bundled libraries and which are the wheel's own extension modules, the
   base name, sha256, size, licence and evidence of each;
2. the refusals: an unknown library, a licence text that disagrees with the table, a text that does not
   name the library, a malformed table;
3. the GPL-family mechanism over synthetic wheels (`<name>.libs/libquadmath-xxx.so.0` under LGPL, libgfortran
   under GPL-3.0-or-later WITH GCC-exception-3.1, and a `GPL-2.0-only OR MIT` library that stays carried): the
   builder moves the wheel to the operator list and refuses without a pin;
4. the installer: the operator's wheel is required, verified by sha256 and installed; a missing directory,
   a missing file and a wrong hash are typed refusals before the target exists; pip is handed the one
   verified file by path.
"""
from __future__ import annotations

import base64
import copy
import hashlib
import json
import re
import sys
import zipfile
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
KIT_DIR = REPO_ROOT / "scripts" / "kit"
sys.path.insert(0, str(KIT_DIR))
import build_kit  # noqa: E402  (path insert must precede this import)
import install  # noqa: E402
import manifest as kmanifest  # noqa: E402
import operator_check  # noqa: E402
import wheel_libs  # noqa: E402

ELF = b"\x7fELF\x02\x01\x01\x00" + b"\x00" * 8
GFORTRAN = "libgfortran-aaaaaaaa-bbbbbbbb.so.5.0.0"
OPENBLAS = "libscipy_openblas64_-cccccccc.so"
QUADMATH = "libquadmath-dddddddd-eeeeeeee.so.0.0.0"


def sha(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def stanza(name: str, files: str, licence: str, body: str = "  Copyright (C) the authors\n") -> str:
    return f"Name: {name}\nFiles: {files}\nDescription: bundled\nAvailability: https://example.org/\nLicense: {licence}\n{body}"


#: numpy's shape: a main licence, a `----` rule, the bundled libraries' stanzas, a `----` rule, licence texts.
NUMPY_LIKE_TEXT = (
    "Copyright (c) the demo authors\n\n----\n\nThis binary distribution of demo also bundles the following software:\n\n\n"
    + stanza("OpenBLAS", "demo.libs/libscipy_openblas*.so", "BSD-3-Clause") + "\n\n"
    + stanza("LAPACK", "demo.libs/libscipy_openblas*.so", "BSD-3-Clause-Open-MPI") + "\n\n"
    + stanza("GCC runtime library", "demo.libs/libgfortran*.so", "GPL-3.0-or-later WITH GCC-exception-3.1")
    + "\n----\n\nFull text of license texts referred to above follows:\n\n----\n\nGCC RUNTIME LIBRARY EXCEPTION\n"
)
QUADMATH_TEXT = NUMPY_LIKE_TEXT + "\n" + stanza("libquadmath", "demo.libs/libquadmath*.so", "LGPL-2.1-or-later")


def make_wheel(path: Path, files: dict[str, bytes]) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
        for name, data in sorted(files.items()):
            z.writestr(name, data)
    return path


def demo_files(licence_text: str = NUMPY_LIKE_TEXT, *, extra: dict[str, bytes] | None = None, quadmath: bool = False) -> dict[str, bytes]:
    files = {
        "demo/__init__.py": b"",
        "demo/_core.cpython-313-aarch64-linux-gnu.so": ELF + b"\x00PyInit__core\x00",
        f"demo.libs/{GFORTRAN}": ELF + b"gfortran",
        f"demo.libs/{OPENBLAS}": ELF + b"openblas",
        "demo-1.0.dist-info/licenses/LICENSE.txt": licence_text.encode(),
        "demo-1.0.dist-info/METADATA": b"Metadata-Version: 2.4\nName: demo\nVersion: 1.0\n",
    }
    if quadmath:
        files[f"demo.libs/{QUADMATH}"] = ELF + b"quadmath"
    files.update(extra or {})
    return files


@pytest.fixture()
def table() -> dict:
    libraries, _sha = wheel_libs.load_table()
    return libraries


def refusal(wheel: Path, table: dict) -> wheel_libs.BundledLibraryRefused:
    with pytest.raises(wheel_libs.BundledLibraryRefused) as exc:
        wheel_libs.inventory_wheel(wheel, table)
    return exc.value


# =================================================================================================
# 1. The inventory
# =================================================================================================

@pytest.mark.parametrize("filename,base", [
    ("libgfortran-e1b7dfc8-d8198c01.so.5.0.0", "libgfortran"),
    ("libscipy_openblas64_-f552eb69.so", "libscipy_openblas64_"),
    ("libquadmath-96973f99.so.0.0.0", "libquadmath"),
    ("libgomp-a34b3233.so.1.0.0", "libgomp"),
    ("libplain.so.1", "libplain"),
    ("libplain.so", "libplain"),
    ("README.txt", "README.txt"),
])
def test_base_name_strips_the_so_suffix_and_auditwheels_hashes(filename, base):
    assert wheel_libs.base_name(filename) == base


def test_the_inventory_lists_each_libs_dir_entry_with_hash_size_licence_and_evidence(tmp_path, table):
    wheel = make_wheel(tmp_path / "demo-1.0-cp313-cp313-manylinux_2_28_aarch64.whl", demo_files())
    inv = wheel_libs.inventory_wheel(wheel, table)
    assert [lib["path"] for lib in inv.libraries] == [f"demo.libs/{GFORTRAN}", f"demo.libs/{OPENBLAS}"]
    gfortran, openblas = inv.libraries
    assert gfortran["library"] == "libgfortran" and gfortran["found_in"] == "libs-dir"
    assert gfortran["sha256"] == sha(ELF + b"gfortran") and gfortran["size"] == len(ELF + b"gfortran")
    assert gfortran["spdx"] == "GPL-3.0-or-later WITH GCC-exception-3.1" and gfortran["gpl_family"] is True
    assert gfortran["upstream"].startswith("https://gcc.gnu.org/")
    # the evidence quotes the licence file and the stanza's line range: Name: .. up to the line before `----`
    lines = NUMPY_LIKE_TEXT.splitlines()
    (ev,) = gfortran["evidence"]
    assert ev["file"] == "demo-1.0.dist-info/licenses/LICENSE.txt" and ev["files_pattern"] == "demo.libs/libgfortran*.so"
    assert lines[ev["lines"][0] - 1] == "Name: GCC runtime library"
    assert lines[ev["licence_line"] - 1] == "License: GPL-3.0-or-later WITH GCC-exception-3.1"
    assert lines[ev["lines"][1] - 1] == "  Copyright (C) the authors" and lines[ev["lines"][1]].strip() == ""
    # OpenBLAS's file contains LAPACK too: two stanzas name it, and the table's expression is both
    assert openblas["library"] == "libscipy_openblas64_" and openblas["spdx"] == "BSD-3-Clause AND BSD-3-Clause-Open-MPI"
    assert [e["licence"] for e in openblas["evidence"]] == ["BSD-3-Clause", "BSD-3-Clause-Open-MPI"]
    assert openblas["gpl_family"] is False
    assert inv.gpl_family_libraries == ["libgfortran"]


def test_the_wheels_own_extension_module_is_set_aside_not_inventoried(tmp_path, table):
    wheel = make_wheel(tmp_path / "demo.whl", demo_files())
    inv = wheel_libs.inventory_wheel(wheel, table)
    assert inv.extension_modules == ["demo/_core.cpython-313-aarch64-linux-gnu.so"]
    assert all("_core" not in lib["path"] for lib in inv.libraries)


def test_a_file_named_like_an_extension_module_without_its_init_symbol_is_a_library(tmp_path, table):
    """The name alone does not make a file the package's own module: `PyInit_<module>` must be in its bytes."""
    wheel = make_wheel(tmp_path / "demo.whl", demo_files(extra={"demo/_lookalike.abi3.so": ELF + b"no init symbol"}))
    exc = refusal(wheel, table)
    assert exc.code == "unknown-library" and exc.library == "_lookalike.abi3" and "demo/_lookalike.abi3.so" in str(exc)


def test_a_shared_object_outside_a_libs_dir_is_found_by_its_elf_magic_or_its_name(tmp_path, table):
    table = copy.deepcopy(table)
    table["libextra"] = {"name": "extra", "spdx": "BSD-3-Clause", "upstream": "https://example.org/"}
    text = NUMPY_LIKE_TEXT + "\n" + stanza("extra", "demo/libextra*.so", "BSD-3-Clause")
    wheel = make_wheel(tmp_path / "demo.whl", demo_files(text, extra={"demo/libextra.so.1": b"not elf but named so", "demo/data.bin": ELF + b"x"}))
    exc = refusal(wheel, table)   # `data.bin` has the ELF magic and no table entry: found, so refused
    assert exc.code == "unknown-library" and exc.library == "data.bin"
    wheel = make_wheel(tmp_path / "demo2.whl", demo_files(text, extra={"demo/libextra.so.1": b"not elf but named so"}))
    inv = wheel_libs.inventory_wheel(wheel, table)
    extra = [lib for lib in inv.libraries if lib["library"] == "libextra"]
    assert len(extra) == 1 and extra[0]["found_in"] == "shared-object" and extra[0]["path"] == "demo/libextra.so.1"


def test_python_and_data_files_are_never_inventoried(tmp_path, table):
    wheel = make_wheel(tmp_path / "pure.whl", {"pure/__init__.py": b"x = 1\n", "pure/data.json": b"{}", "pure-1.0.dist-info/METADATA": b"Name: pure\n"})
    inv = wheel_libs.inventory_wheel(wheel, table)
    assert inv.libraries == [] and inv.extension_modules == []


# =================================================================================================
# 2. The refusals
# =================================================================================================

def test_an_unknown_library_is_refused_naming_the_wheel_and_the_library(tmp_path, table):
    wheel = make_wheel(tmp_path / "demo-1.0-x.whl", demo_files(extra={"demo.libs/libmystery-12345678.so.3": ELF + b"?"}))
    exc = refusal(wheel, table)
    assert exc.code == "unknown-library" and exc.wheel == "demo-1.0-x.whl" and exc.library == "libmystery"
    assert "demo-1.0-x.whl" in str(exc) and "'libmystery'" in str(exc) and "demo.libs/libmystery-12345678.so.3" in str(exc)


def test_a_licence_the_wheel_contradicts_is_refused_naming_both_sides(tmp_path, table):
    wrong = copy.deepcopy(table)
    wrong["libgfortran"]["spdx"] = "MIT"
    wheel = make_wheel(tmp_path / "demo-1.0-x.whl", demo_files())
    exc = refusal(wheel, wrong)
    assert exc.code == "licence-mismatch" and exc.wheel == "demo-1.0-x.whl" and exc.library == "libgfortran"
    assert "GPL-3.0-or-later WITH GCC-exception-3.1" in str(exc) and "'MIT'" in str(exc)
    assert "demo-1.0.dist-info/licenses/LICENSE.txt" in str(exc) and re.search(r"lines \d+-\d+, License: on line \d+", str(exc))


def test_a_table_that_names_only_one_of_two_licences_the_text_gives_is_refused(tmp_path, table):
    partial = copy.deepcopy(table)
    partial["libscipy_openblas64_"]["spdx"] = "BSD-3-Clause"
    exc = refusal(make_wheel(tmp_path / "demo.whl", demo_files()), partial)
    assert exc.code == "licence-mismatch" and exc.library == "libscipy_openblas64_" and "BSD-3-Clause-Open-MPI" in str(exc)


def test_a_licence_text_that_does_not_name_the_library_is_refused(tmp_path, table):
    silent = NUMPY_LIKE_TEXT.replace("demo.libs/libgfortran*.so", "demo.libs/libother*.so")
    exc = refusal(make_wheel(tmp_path / "demo-1.0-x.whl", demo_files(silent)), table)
    assert exc.code == "licence-text-silent" and exc.library == "libgfortran" and "demo-1.0-x.whl" in str(exc)
    exc = refusal(make_wheel(tmp_path / "demo2.whl", demo_files("no stanzas at all\n")), table)
    assert exc.code == "licence-text-silent"


def test_a_wheel_without_any_licence_file_is_refused_for_its_library(tmp_path, table):
    files = demo_files()
    del files["demo-1.0.dist-info/licenses/LICENSE.txt"]
    assert refusal(make_wheel(tmp_path / "demo.whl", files), table).code == "licence-text-silent"


def test_the_committed_table_is_well_formed_and_records_the_three_libraries():
    libraries, table_sha = wheel_libs.load_table()
    assert wheel_libs.table_problems(libraries) == [] and len(table_sha) == 64
    assert libraries["libgfortran"]["spdx"] == "GPL-3.0-or-later WITH GCC-exception-3.1"
    assert libraries["libscipy_openblas64_"]["spdx"] == "BSD-3-Clause AND BSD-3-Clause-Open-MPI"
    assert libraries["libquadmath"]["spdx"] == "LGPL-2.1-or-later"
    assert [k for k, v in libraries.items() if any(wheel_libs.gpl_family_term(t) for t in wheel_libs.spdx_terms(v["spdx"]))] == [
        "libgfortran", "libquadmath"]


def test_a_malformed_table_is_refused_naming_what_is_wrong(tmp_path):
    bad = {"libraries": {
        "a": {"name": "a", "spdx": "(MIT OR BSD-3-Clause)", "upstream": "https://x/"},
        "b": {"name": "b", "spdx": "NotALicence-9.9", "upstream": "https://x/"},
        "c": {"name": "c", "spdx": "MIT", "upstream": "http://x/"},
        "d": {"name": "d", "spdx": "", "upstream": "https://x/"},
    }}
    path = tmp_path / "t.json"
    path.write_text(json.dumps(bad))
    with pytest.raises(wheel_libs.BundledLibraryRefused) as exc:
        wheel_libs.load_table(path)
    text = str(exc.value)
    assert exc.value.code == "table-malformed"
    assert "a.spdx" in text and "b.spdx" in text and "c.upstream" in text and "d.spdx" in text
    path.write_text("{nope")
    with pytest.raises(wheel_libs.BundledLibraryRefused):
        wheel_libs.load_table(path)


@pytest.mark.parametrize("term,expected", [
    ("LGPL-2.1-or-later", True), ("LGPL-3.0-only WITH LGPL-3.0-linking-exception", True), ("AGPL-3.0-only", True),
    ("GPL-3.0-or-later WITH GCC-exception-3.1", True), ("GPL-2.0-only", True), ("GPL-2.0-or-later OR LGPL-2.1-only", True),
    ("LGPL-2.1-or-later OR MIT", False), ("GPL-2.0-only OR MIT", False), ("MIT OR GPL-3.0-only WITH Classpath-exception-2.0", False),
    ("BSD-3-Clause", False), ("BSD-3-Clause-Open-MPI", False), ("MPL-2.0", False),
])
def test_only_a_term_whose_every_alternative_is_gpl_family_counts_as_gpl_family(term, expected):
    assert wheel_libs.gpl_family_term(term) is expected


# =================================================================================================
# 3. A GPL-family library moves its wheel to the operator-supplied list
# =================================================================================================

def _lgpl_wheel(tmp_path: Path) -> Path:
    return make_wheel(tmp_path / "wheels" / "demo-1.0-cp313-cp313-manylinux_2_28_x86_64.whl", demo_files(QUADMATH_TEXT, quadmath=True))


def _pin_for(wheel: Path, *, item: int = 8, sha256: str | None = None, version: str = "1.0") -> dict:
    return {
        "item": item, "kind": "python-wheel", "dist": "demo", "version": version, "filename": wheel.name,
        "sha256": "sha256:" + (sha256 or sha(wheel.read_bytes())), "reference": "synthetic PyPI file",
        "licences": "LGPL-2.1-or-later (libquadmath)", "why": "bundles libquadmath",
        "verification": {"step": "files", "method": "sha256 of the file"},
    }


def test_the_inventory_marks_a_gpl_family_library(tmp_path, table):
    inv = wheel_libs.inventory_wheel(_lgpl_wheel(tmp_path), table)
    lib = next(lib for lib in inv.libraries if lib["library"] == "libquadmath")
    assert lib["gpl_family"] is True and lib["spdx"] == "LGPL-2.1-or-later" and lib["evidence"][0]["licence"] == "LGPL-2.1-or-later"
    assert inv.gpl_family_libraries == ["libgfortran", "libquadmath"]


def test_the_builder_moves_a_wheel_with_an_lgpl_library_to_the_operator_list_and_deletes_it_from_the_kit(tmp_path, table):
    wheel = _lgpl_wheel(tmp_path)
    digest = sha(wheel.read_bytes())
    pins = {"wheel-demo": _pin_for(wheel)}
    entry, carried = build_kit._inventoried_entry(wheel, "demo", "1.0", table, pins)
    assert carried is False and not wheel.exists()
    assert len(entry["sha256"]) == 64 and entry["sha256"] == digest
    assert entry["component"] == "wheel-demo" and entry["gpl_family_libraries"] == ["libgfortran", "libquadmath"]
    assert entry["sha256"] == digest and entry["filename"] == wheel.name
    assert {lib["library"] for lib in entry["bundled_libraries"]} == {"libgfortran", "libscipy_openblas64_", "libquadmath"}


def _wheel_with_only(tmp_path: Path, keep: str, name: str = "demo-1.0-x.whl") -> Path:
    files = demo_files()
    del files[f"demo.libs/{GFORTRAN}"]
    if keep != "openblas":
        del files[f"demo.libs/{OPENBLAS}"]
    return make_wheel(tmp_path / "wheels" / name, files)


def test_a_wheel_bundling_only_permissive_libraries_is_carried(tmp_path, table):
    wheel = _wheel_with_only(tmp_path, "openblas")
    entry, carried = build_kit._inventoried_entry(wheel, "demo", "1.0", table, {})
    assert carried is True and wheel.is_file() and "component" not in entry
    assert [lib["library"] for lib in entry["bundled_libraries"]] == ["libscipy_openblas64_"]
    assert entry["bundled_libraries"][0]["gpl_family"] is False


def test_a_wheel_bundling_a_gpl_with_gcc_exception_library_moves_to_the_operator_list(tmp_path, table):
    """libgfortran is `GPL-3.0-or-later WITH GCC-exception-3.1`: the exception frees the combined work, but the
    library file is GPL code, and the kit redistributes none."""
    files = demo_files()
    del files[f"demo.libs/{OPENBLAS}"]
    wheel = make_wheel(tmp_path / "wheels" / "demo-1.0-x.whl", files)
    digest = sha(wheel.read_bytes())
    entry, carried = build_kit._inventoried_entry(wheel, "demo", "1.0", table, {"wheel-demo": _pin_for(wheel)})
    assert carried is False and not wheel.exists() and entry["sha256"] == digest
    assert entry["gpl_family_libraries"] == ["libgfortran"] and entry["component"] == "wheel-demo"
    assert entry["bundled_libraries"][0]["spdx"] == "GPL-3.0-or-later WITH GCC-exception-3.1"
    # and without a pin the build is refused, naming the wheel and the library
    wheel = make_wheel(tmp_path / "again" / "demo-1.0-x.whl", files)
    with pytest.raises(wheel_libs.BundledLibraryRefused) as exc:
        build_kit._inventoried_entry(wheel, "demo", "1.0", table, {})
    assert exc.value.code == "gpl-family-wheel-unpinned" and exc.value.library == "libgfortran" and wheel.is_file()


def test_a_library_under_gpl_or_mit_stays_carried(tmp_path, table):
    """A strict reading: `GPL-2.0-only OR MIT` has an alternative that is not GPL-family, so it is a way to take the
    library without the GPL and the wheel stays in the kit."""
    table = copy.deepcopy(table)
    table["libdual"] = {"name": "dual", "spdx": "GPL-2.0-only OR MIT", "upstream": "https://example.org/"}
    text = NUMPY_LIKE_TEXT + "\n" + stanza("dual", "demo.libs/libdual*.so", "GPL-2.0-only OR MIT")
    files = demo_files(text, extra={"demo.libs/libdual-11111111.so.1": ELF + b"dual"})
    del files[f"demo.libs/{GFORTRAN}"]
    wheel = make_wheel(tmp_path / "wheels" / "demo-1.0-x.whl", files)
    entry, carried = build_kit._inventoried_entry(wheel, "demo", "1.0", table, {})
    assert carried is True and wheel.is_file()
    dual = next(lib for lib in entry["bundled_libraries"] if lib["library"] == "libdual")
    assert dual["spdx"] == "GPL-2.0-only OR MIT" and dual["gpl_family"] is False


def test_the_builder_refuses_an_lgpl_wheel_that_has_no_pin_or_whose_pin_differs(tmp_path, table):
    wheel = _lgpl_wheel(tmp_path)
    with pytest.raises(wheel_libs.BundledLibraryRefused) as exc:
        build_kit._inventoried_entry(wheel, "demo", "1.0", table, {})
    assert exc.value.code == "gpl-family-wheel-unpinned" and wheel.name in str(exc.value) and "libquadmath" in str(exc.value)
    assert wheel.is_file()   # a refusal does not delete anything
    for pin in (_pin_for(wheel, sha256="0" * 64), _pin_for(wheel, version="2.0")):
        with pytest.raises(wheel_libs.BundledLibraryRefused) as exc:
            build_kit._inventoried_entry(wheel, "demo", "1.0", table, {"wheel-demo": pin})
        assert exc.value.code == "gpl-family-wheel-pin-differs" and wheel.is_file()


def test_the_real_pins_file_pins_numpys_aarch64_wheel_because_it_bundles_libgfortran():
    pins = json.loads((KIT_DIR / "operator_supplied.json").read_text(encoding="utf-8"))["components"]
    assert operator_check.pins_problems(pins) == []
    wheels = {cid: p for cid, p in pins.items() if p["kind"] == "python-wheel"}
    assert list(wheels) == ["numpy-wheel-linux-aarch64"]
    pin = wheels["numpy-wheel-linux-aarch64"]
    assert pin["item"] == 7 and pin["dist"] == "numpy" and pin["version"] == "2.5.3" and pin["libraries"] == ["libgfortran"]
    assert pin["filename"] == "numpy-2.5.3-cp313-cp313-manylinux_2_27_aarch64.manylinux_2_28_aarch64.whl"
    assert pin["sha256"] == "sha256:c76d5dde9f445058f83d0c02af00557a4db91de9a9a57c0df87d1535001d654b"
    assert "GPL-3.0-or-later WITH GCC-exception-3.1" in pin["licences"] and "numpy.libs/" in pin["licences"]
    assert wheel_libs.load_table()[0]["libgfortran"]["spdx"] in pin["licences"]
    assert operator_check.wheel_pin_for(pins, pin["filename"]) == ("numpy-wheel-linux-aarch64", pin)


def test_the_pins_validator_names_what_is_wrong_with_a_wheel_pin(tmp_path):
    wheel = _lgpl_wheel(tmp_path)
    pins = json.loads((KIT_DIR / "operator_supplied.json").read_text(encoding="utf-8"))["components"]
    pins["wheel-demo"] = _pin_for(wheel)
    assert operator_check.pins_problems(pins) == []
    for key, value, needle in (("filename", "../x.whl", "wheel-demo.filename"), ("filename", "x.tar.gz", "wheel-demo.filename"),
                               ("sha256", "sha256:short", "wheel-demo.sha256"), ("dist", "", "wheel-demo.dist"),
                               ("verification", {"step": "images", "method": "m"}, "wheel-demo.verification")):
        bad = copy.deepcopy(pins)
        bad["wheel-demo"][key] = value
        assert any(needle in p for p in operator_check.pins_problems(bad)), (key, value)


# =================================================================================================
# 4. The installer
# =================================================================================================

#: The distributions the installer requires of a kit (`install._REQUIRED_WHEEL_DIST_NAMES`).
REQUIRED = install._REQUIRED_WHEEL_DIST_NAMES


def _fetched(name: str) -> dict:
    return {"name": name, "version": "1", "filename": f"{name}-1-py3-none-any.whl", "sha256": "0" * 64, "bundled_libraries": [], "extension_modules": []}


class World:
    """A bare kit whose `wheels` section lists every required distribution but numpy, which is operator-supplied
    (a synthetic LGPL wheel), a pins file with its `python-wheel` pin, and the operator's directory holding it."""

    def __init__(self, tmp_path, monkeypatch):
        self.tmp = tmp_path
        self.numpy = make_wheel(tmp_path / "operator" / "demo-1.0-cp313-cp313-manylinux_2_28_x86_64.whl", demo_files(QUADMATH_TEXT, quadmath=True))
        self.operator_dir = self.numpy.parent
        self.pin = _pin_for(self.numpy)
        pins = json.loads((KIT_DIR / "operator_supplied.json").read_text(encoding="utf-8"))
        pins["components"]["wheel-demo"] = self.pin
        self.pins_path = tmp_path / "pins.json"
        self.pins_path.write_text(json.dumps(pins), encoding="utf-8")
        monkeypatch.setattr(kmanifest, "PINS_FILE", self.pins_path)
        self.kit = tmp_path / "kit"
        make_wheel(self.kit / "wheels" / "altavista-1-py3-none-any.whl", {"altavista/__init__.py": b""})
        self.operator_entry = {
            "name": "numpy", "version": "1.0", "filename": self.numpy.name, "sha256": sha(self.numpy.read_bytes()),
            "bundled_libraries": [], "extension_modules": [], "component": "wheel-demo", "gpl_family_libraries": ["libquadmath"],
        }
        self.write_manifest()
        self.calls: list = []
        monkeypatch.setattr(install, "_check_viewer_assets_in_wheel", lambda kit_dir, doc: None)

        def fake_install(kit_dir, target_dir, extra_wheels=()):
            self.calls.append(list(extra_wheels))
            return {"venv": "venv", "installed_packages": []}
        monkeypatch.setattr(install, "_install_wheels_into_venv", fake_install)

    def write_manifest(self, **wheels_over) -> None:
        wheels = {
            "collected": True, "network_used": True,
            "fetched": [_fetched(n) for n in REQUIRED if n != "numpy"],
            "operator_supplied": [self.operator_entry], "bundled_libs_table_sha256": "0" * 64,
        }
        wheels.update(wheels_over)
        kmanifest.write_manifest(kmanifest.build_manifest(
            kit_root=self.kit, git_commit="0" * 40, git_dirty=False, git_status=[], images={}, sboms={}, packs={},
            vendor={"collected": False}, wheels=wheels, binaries={}, runs={}, gaps=[], operator_supplied_declared=[],
        ), self.kit)

    def doc(self) -> dict:
        return json.loads((self.kit / "KIT_MANIFEST").read_text())


@pytest.fixture()
def world(tmp_path, monkeypatch):
    return World(tmp_path, monkeypatch)


def _wheel_lines(out: list[str]) -> list[str]:
    return [line for line in out if line.startswith("operator-supplied: wheel-demo")]


def test_the_pins_file_lists_the_wheel_as_required_only_when_the_kit_names_it(world):
    pin = world.pin
    req = operator_check.requirement_for("wheel-demo", pin, world.doc())
    assert req.required and req.text == f"required (wheels.operator_supplied: {world.numpy.name})"
    none = operator_check.requirement_for("wheel-demo", pin, {"wheels": {"operator_supplied": []}})
    assert not none.required and none.text.startswith("not required")
    assert not operator_check.requirement_for("wheel-demo", pin, {"wheels": {}}).required


def test_install_with_the_operator_wheel_present_verifies_stages_and_installs_it(world, tmp_path):
    out: list[str] = []
    target = tmp_path / "target"
    record = install.install(world.kit, target, operator_wheels=str(world.operator_dir), out=out.append)
    (line,) = _wheel_lines(out)
    assert line.startswith(f"operator-supplied: wheel-demo  required (wheels.operator_supplied: {world.numpy.name})  verified {world.numpy.name} sha256:")
    # the verified file was copied under the target, re-hashed, and handed to pip by that exact path
    staged = target / "operator-wheels" / world.numpy.name
    assert staged.read_bytes() == world.numpy.read_bytes()
    assert world.calls == [[staged]]
    (rec,) = record["installed"]["operator_wheels"]
    assert rec["filename"] == world.numpy.name and rec["sha256"] == sha(world.numpy.read_bytes()) and rec["installed_from"] == f"operator-wheels/{world.numpy.name}"
    comps = {c["component"]: c for c in record["installed"]["operator_supplied"]["components"]}
    assert comps["wheel-demo"]["verified"] is True and comps["wheel-demo"]["verifies_in"] == "files install"
    assert json.loads((target / "INSTALL_RECORD").read_text())["installed"]["operator_wheels"] == record["installed"]["operator_wheels"]


@pytest.mark.parametrize("how,code", [
    ("no-dir", "wheel-dir-missing"),
    ("not-a-dir", "wheel-dir-not-a-directory"),
    ("empty-dir", "wheel-missing"),
    ("wrong-hash", "wheel-hash-differs"),
])
def test_install_refuses_a_missing_or_wrong_operator_wheel_before_the_target_exists(world, tmp_path, how, code):
    directory: str | None = str(world.operator_dir)
    if how == "no-dir":
        directory = None
    elif how == "not-a-dir":
        directory = str(world.numpy)
    elif how == "empty-dir":
        directory = str(tmp_path / "empty")
        Path(directory).mkdir()
        (Path(directory) / "other-1.0-py3-none-any.whl").write_bytes(b"x")
    elif how == "wrong-hash":
        world.numpy.write_bytes(world.numpy.read_bytes() + b"tampered")
    out: list[str] = []
    target = tmp_path / "target"
    with pytest.raises(install.OperatorSuppliedRefused) as exc:
        install.install(world.kit, target, operator_wheels=directory, out=out.append)
    assert exc.value.code == code
    assert not target.exists() and world.calls == []
    (line,) = _wheel_lines(out)
    assert line.endswith(f"NOT VERIFIED [{code}]")
    message = str(exc.value)
    if how in ("no-dir", "not-a-dir", "empty-dir"):
        assert world.numpy.name in message and world.pin["sha256"] in message and "--operator-wheels" in message and "libquadmath" in message
    if how == "empty-dir":
        assert "other-1.0-py3-none-any.whl" in message
    if how == "wrong-hash":
        assert world.pin["sha256"] in message and "sha256:" + sha(world.numpy.read_bytes()) in message


def test_the_cli_prints_the_refusal_and_exits_2_and_installs_with_the_flag(world, tmp_path, capsys):
    target = tmp_path / "target"
    assert install.main([str(world.kit), str(target)]) == 2
    captured = capsys.readouterr()
    assert "wheel-dir-missing" in captured.err and "REFUSED" in captured.err and not target.exists()
    assert install.main([str(world.kit), str(target), "--operator-wheels", str(world.operator_dir)]) == 0
    assert f"operator-supplied wheel {world.numpy.name}" in capsys.readouterr().out and world.calls


def test_check_operator_supplied_verifies_the_wheel_without_installing(world, tmp_path, capsys):
    assert install.main(["--check-operator-supplied", str(world.kit), "--operator-wheels", str(world.operator_dir)]) == 0
    assert any(line.startswith("operator-supplied: wheel-demo") and " verified " in line for line in capsys.readouterr().out.splitlines())
    assert install.main(["--check-operator-supplied", str(world.kit)]) == 2
    assert "wheel-dir-missing" in capsys.readouterr().err and world.calls == []


def test_a_kit_built_against_another_pin_is_refused(world, tmp_path):
    world.operator_entry["sha256"] = "1" * 64
    world.write_manifest(operator_supplied=[world.operator_entry])
    with pytest.raises(install.OperatorSuppliedRefused) as exc:
        install.install(world.kit, tmp_path / "target", operator_wheels=str(world.operator_dir))
    assert exc.value.code == "wheel-kit-entry-differs"


def test_a_kit_naming_a_component_the_pins_file_lacks_is_refused(world, tmp_path):
    world.operator_entry["component"] = "wheel-nonesuch"
    world.write_manifest(operator_supplied=[world.operator_entry])
    with pytest.raises(install.OperatorSuppliedRefused) as exc:
        install.install(world.kit, tmp_path / "target", operator_wheels=str(world.operator_dir))
    assert exc.value.code == "wheel-pin-missing"


def test_a_kit_with_no_operator_wheel_needs_no_option_and_the_pin_is_listed_as_not_required(world, tmp_path):
    world.write_manifest(operator_supplied=[], fetched=[_fetched(n) for n in REQUIRED])
    out: list[str] = []
    record = install.install(world.kit, tmp_path / "target", out=out.append)
    (line,) = _wheel_lines(out)
    assert line == "operator-supplied: wheel-demo  not required (no wheel of the kit bundles a GPL-family library), not verified"
    assert record["installed"]["operator_wheels"] == [] and world.calls == [[]]


def test_the_installer_accepts_the_operator_wheel_as_the_distribution_the_viewer_needs(world, tmp_path):
    install._check_required_wheels(world.doc())   # numpy is in operator_supplied, not fetched: fine
    world.write_manifest(operator_supplied=[])
    with pytest.raises(install.InstallRefused, match="numpy"):
        install._check_required_wheels(world.doc())


def test_the_installer_refuses_a_wheels_section_without_the_inventory(world):
    doc = world.doc()
    del doc["wheels"]["operator_supplied"]
    with pytest.raises(install.InstallRefused, match="operator_supplied"):
        install._check_required_wheels(doc)
    doc = world.doc()
    del doc["wheels"]["fetched"][0]["bundled_libraries"]
    with pytest.raises(install.InstallRefused, match="bundled_libraries"):
        install._check_required_wheels(doc)


def test_the_installer_refuses_a_kit_that_carries_a_wheel_with_a_gpl_family_library(world):
    doc = world.doc()
    doc["wheels"]["fetched"].append({**_fetched("numpy"), "bundled_libraries": [{"library": "libquadmath", "gpl_family": True}]})
    with pytest.raises(install.InstallRefused, match="libquadmath"):
        install._check_required_wheels(doc)


def test_the_image_step_lists_the_wheel_as_the_files_installs(world):
    doc = world.doc()
    pins = json.loads(world.pins_path.read_text())["components"]
    out: list[str] = []
    results = operator_check.verify_all(operator_check.STEP_IMAGES, doc, pins, verifiers={}, out=out.append)
    (r,) = [r for r in results if r.component == "wheel-demo"]
    assert r.outcome == "other-step" and "verified by the files install, not here" in _wheel_lines(out)[0]


# --- pip itself: the one verified file is installed by path; the operator's other files are never candidates ---

def _pure_wheel(directory: Path, name: str, version: str, requires: tuple[str, ...] = ()) -> Path:
    dist_info = f"{name}-{version}.dist-info"
    files = {
        f"{name}/__init__.py": f"VERSION = {version!r}\n".encode(),
        f"{dist_info}/METADATA": ("Metadata-Version: 2.1\n" f"Name: {name}\nVersion: {version}\n" + "".join(f"Requires-Dist: {r}\n" for r in requires)).encode(),
        f"{dist_info}/WHEEL": b"Wheel-Version: 1.0\nGenerator: test\nRoot-Is-Purelib: true\nTag: py3-none-any\n",
    }
    record = ""
    for path, data in files.items():
        digest = base64.urlsafe_b64encode(hashlib.sha256(data).digest()).rstrip(b"=").decode()
        record += f"{path},sha256={digest},{len(data)}\n"
    record += f"{dist_info}/RECORD,,\n"
    files[f"{dist_info}/RECORD"] = record.encode()
    return make_wheel(directory / f"{name}-{version}-py3-none-any.whl", files)


def test_pip_installs_the_verified_operator_wheel_by_path_and_nothing_else_from_that_directory(tmp_path):
    kit = tmp_path / "kit"
    _pure_wheel(kit / "wheels", "altavista", "0.1.0", requires=("avlib>=1",))
    operator = tmp_path / "operator"
    verified = _pure_wheel(operator, "avlib", "1.0")
    _pure_wheel(operator, "avlib", "9.0")   # a higher version the operator also left in the directory: never a candidate
    target = tmp_path / "target"
    target.mkdir()
    with pytest.raises(install.InstallRefused, match="avlib"):
        install._install_wheels_into_venv(kit, tmp_path / "without")   # the kit alone cannot satisfy altavista
    info = install._install_wheels_into_venv(kit, target, [verified])
    installed = {p["name"]: p["version"] for p in info["installed_packages"]}
    assert installed["altavista"] == "0.1.0" and installed["avlib"] == "1.0"


# =================================================================================================
# 5. The operator's side: scripts/kit/save_operator_wheels.py
# =================================================================================================

import subprocess  # noqa: E402

import save_operator_wheels  # noqa: E402


def _fake_download(content: bytes, name: str | None = None):
    calls: list = []

    def download(arguments, directory):
        calls.append(list(arguments))
        (directory / (name or "demo-1.0-py3-none-any.whl")).write_bytes(content)
        return subprocess.CompletedProcess(arguments, 0, "", "")
    download.calls = calls
    return download


def _save_pin(content: bytes, **over) -> dict:
    pin = {"kind": "python-wheel", "dist": "demo", "version": "1.0", "filename": "demo-1.0-cp313-cp313-manylinux_2_27_aarch64.manylinux_2_28_aarch64.whl",
           "sha256": "sha256:" + sha(content)}
    pin.update(over)
    return pin


def test_the_pip_arguments_select_exactly_the_pinned_file():
    pins = save_operator_wheels.wheel_pins()
    assert list(pins) == ["numpy-wheel-linux-aarch64"]
    args = save_operator_wheels.pip_arguments(pins["numpy-wheel-linux-aarch64"])
    assert args == ["--no-deps", "--only-binary=:all:", "--python-version", "3.13", "--implementation", "cp", "--abi", "cp313",
                    "--platform", "manylinux_2_27_aarch64", "--platform", "manylinux_2_28_aarch64", "numpy==2.5.3"]
    with pytest.raises(save_operator_wheels.SaveRefused):
        save_operator_wheels.pip_arguments({"filename": "demo-1.0-py3-none-any.whl", "dist": "demo", "version": "1.0"})


def test_the_operator_helper_writes_the_pinned_wheel_and_sha256sums(tmp_path):
    content = b"wheel bytes"
    pin = _save_pin(content)
    download = _fake_download(content, pin["filename"])
    results = save_operator_wheels.save_wheels(tmp_path / "out", {"wheel-demo": pin}, download=download)
    assert results["wheel-demo"]["sha256"] == sha(content)
    assert (tmp_path / "out" / pin["filename"]).read_bytes() == content
    assert (tmp_path / "out" / "SHA256SUMS").read_text() == f"{sha(content)}  {pin['filename']}\n"
    assert save_operator_wheels.verify_sums(tmp_path / "out") == {pin["filename"]: sha(content)}
    assert not any(p.name.startswith(".") for p in (tmp_path / "out").iterdir())
    # and the file it wrote is what the installer's check accepts
    assert operator_check.sha256_of_file(tmp_path / "out" / pin["filename"]) == sha(content)
    with pytest.raises(save_operator_wheels.SaveRefused, match="already holds"):
        save_operator_wheels.save_wheels(tmp_path / "out", {"wheel-demo": pin}, download=download)


def test_the_operator_helper_refuses_a_download_that_is_not_the_pinned_file_and_leaves_nothing(tmp_path):
    pin = _save_pin(b"the pinned bytes")
    wrong_bytes = _fake_download(b"other bytes", pin["filename"])
    with pytest.raises(save_operator_wheels.SaveRefused, match="not the pinned"):
        save_operator_wheels.save_wheels(tmp_path / "a", {"wheel-demo": pin}, download=wrong_bytes)
    wrong_name = _fake_download(b"the pinned bytes", "demo-2.0-py3-none-any.whl")
    with pytest.raises(save_operator_wheels.SaveRefused, match="not the pinned"):
        save_operator_wheels.save_wheels(tmp_path / "b", {"wheel-demo": pin}, download=wrong_name)
    for d in ("a", "b"):
        assert list((tmp_path / d).iterdir()) == []
