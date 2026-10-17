"""scripts/kit/wheel_libs.py -- question 250(a): the wheel step inventories the native libraries every
wheel it fetches bundles, each with its licence and the evidence for it, and moves a wheel that
bundles a GPL-family library to the operator-supplied list.

Build-time only (`build_kit.collect_wheels` calls it); it is not part of the installer, which never
needs to open a wheel's libraries (it verifies an operator-supplied wheel by sha256, `operator_check`).
Standard library plus `licences` (the SPDX parser the SBOM check already uses).

# What counts as a bundled library (`inventory_wheel`)

A wheel is a zip. Every regular file in it is classified by its path and bytes, nothing else:

1. **Under a `<something>.libs/` directory** (the auditwheel convention: `numpy.libs/`, `pillow.libs/`):
   a bundled library, whatever its name or type. These are the files auditwheel copied in and renamed
   with a hash suffix (`libgfortran-e1b7dfc8-d8198c01.so.5.0.0`).
2. **Any other shared object that is not the package's own extension module.** A file is a shared
   object when its first four bytes are the ELF magic or its name ends `.so` / `.so.<digits>...`. It is
   the package's OWN extension module only when both hold: its name has the form Python gives an
   extension module (`<module>.cpython-313-aarch64-linux-gnu.so`, `<module>.abi3.so`, `<module>.so`)
   and its bytes contain `PyInit_<module>`, the symbol the interpreter calls to load it. A file that is
   named like an extension module but exports no `PyInit_` symbol, and every other shared object, is a
   bundled library (found in `shared-object` rather than `libs-dir`). The name test alone is not
   trusted (a library can be called anything), and the symbol test alone is not trusted either (a
   library that embeds an extension's name); both must hold. The own extension modules are listed in
   `extension_modules`, so what was set aside is on record.

A library linked statically into an extension module (pydantic_core, uvloop, ...) is NOT found by this and
cannot be: there is no separate file to inventory. The wheel's own licence files are where such code is
declared, and the SBOM's Python components carry the declared licence.

# Licences come from a committed table, checked against the wheel's own text

`scripts/kit/bundled_libs.json` maps a library's BASE NAME (the file name without its `.so...` suffix and
without auditwheel's `-<8 hex>` hash suffixes: `libgfortran`, `libscipy_openblas64_`) to an SPDX
expression and its upstream. For every bundled library:

- a base name that is not in the table is refused (`BundledLibraryRefused`, code `unknown-library`),
  naming the wheel, the library and its path: this step never guesses a licence;
- the wheel's licence files (`*.dist-info/licenses/**`, `*.dist-info/LICENSE*|COPYING*|NOTICE*`) must
  contain a STANZA that names the library: numpy appends the bundled libraries' licences to its
  `LICENSE.txt` as `Name:` / `Files:` / `Description:` / `Availability:` / `License:` blocks (the block
  runs from its `Name:` line to the line before the next `Name:` line, a `----` line or the end of the
  file). A stanza names the library when one of its `Files:` patterns matches the library's path in the
  wheel (a pattern also matches the path followed by a version suffix, because `libgfortran*.so` is how
  the text writes `libgfortran-<hash>.so.5.0.0`). No such stanza: refused, code `licence-text-silent`;
- the `License:` lines of the stanzas that name it, normalised for whitespace, must be EXACTLY the
  ` AND `-joined terms of the table's expression (OpenBLAS's file also contains LAPACK, so two stanzas
  name `libscipy_openblas64_`: `BSD-3-Clause` and `BSD-3-Clause-Open-MPI`). Otherwise refused, code
  `licence-mismatch`, with both sides and the line range of the text.

Each entry's `evidence` quotes where: the licence file's path in the wheel and the line range of the
stanza (1-based, inclusive), the stanza's `Files:` pattern, its `License:` line number and value.

# A GPL-family library moves the wheel to the operator

The kit redistributes no GPL or LGPL code (the lead's ruling on question 250(a)). A library whose
expression has a GPL-FAMILY term is `gpl_family`: a term (` AND `-joined) every OR-alternative of which is
a `GPL-*`, `LGPL-*` or `AGPL-*` licence, with or without a `WITH` exception. `GPL-3.0-or-later WITH
GCC-exception-3.1` (libgfortran) and `LGPL-2.1-or-later` (libquadmath) are; the GCC Runtime Library
Exception frees the combined work, not the library file, which is still GPL code. The reading is strict:
`GPL-2.0-only OR MIT` is NOT a GPL-family term, because the MIT alternative is a way to take the library
without the GPL, so such a library is carried.

A wheel with a `gpl_family` library is not carried in the kit (`build_kit.collect_wheels` deletes it after
reading it): `KIT_MANIFEST.wheels.operator_supplied` records it by filename and sha256 and names the
`python-wheel` component of `operator_supplied.json` that pins the same filename and sha256, and the
installer asks the operator for it (`--operator-wheels DIR`).

Run `python scripts/kit/wheel_libs.py WHEEL...` to print the inventory as JSON, or a refusal.
"""
from __future__ import annotations

import fnmatch
import hashlib
import json
import re
import sys
import zipfile
from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional

SCRIPT_DIR = Path(__file__).resolve().parent
if str(SCRIPT_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPT_DIR))

import licences  # noqa: E402  (path insert must precede this import)

TABLE_FILE = SCRIPT_DIR / "bundled_libs.json"
SPDX_IDS_FILE = SCRIPT_DIR / "spdx-license-ids.json"

ELF_MAGIC = b"\x7fELF"
#: The SPDX id prefixes of the GPL family (`gpl_family_term`).
GPL_FAMILY_PREFIXES = ("GPL-", "LGPL-", "AGPL-")
_SO_SUFFIX = re.compile(r"\.so(\.\d+)*$")
#: auditwheel appends one or two `-<8 hex>` suffixes to a bundled library's name.
_HASH_SUFFIX = re.compile(r"(-[0-9a-f]{8})+$")
#: The file names Python gives an extension module: `<module>.cpython-313-<platform>.so`, `<module>.abi3.so`, `<module>.so`.
_EXTENSION_NAME = re.compile(
    r"^(?P<module>[A-Za-z_][A-Za-z0-9_]*)(?:\.(?:cpython-[0-9]+[a-z]*-[A-Za-z0-9_]+(?:-[A-Za-z0-9_]+)*|abi3))?\.so$"
)
_EXCEPTION_ID = re.compile(r"^[A-Za-z0-9][A-Za-z0-9.\-+]*$")
_LICENCE_FILE_BASENAME = re.compile(r"^(licen[cs]e|copying|notice)", re.IGNORECASE)

FOUND_LIBS_DIR = "libs-dir"
FOUND_SHARED_OBJECT = "shared-object"


class BundledLibraryRefused(RuntimeError):
    """A refusal of the wheel step: the table is malformed, a wheel bundles a library the table does not
    know or whose licence the wheel's own text does not confirm, or a GPL-family wheel has no operator pin. The
    message names the wheel and the library; `code` is the stable name of the refusal."""

    def __init__(self, code: str, message: str, *, wheel: Optional[str] = None, library: Optional[str] = None) -> None:
        super().__init__(f"{code}: {message}")
        self.code = code
        self.wheel = wheel
        self.library = library


@dataclass
class WheelInventory:
    """What `inventory_wheel` found: the bundled libraries (sorted by path), the wheel's own extension
    modules (sorted paths, set aside), and the base names of the libraries under a GPL-family licence."""

    libraries: list[dict] = field(default_factory=list)
    extension_modules: list[str] = field(default_factory=list)

    @property
    def gpl_family_libraries(self) -> list[str]:
        return sorted({lib["library"] for lib in self.libraries if lib["gpl_family"]})


# =================================================================================================
# The committed table
# =================================================================================================

def _spdx_ids() -> frozenset[str]:
    return frozenset(json.loads(SPDX_IDS_FILE.read_text(encoding="utf-8"))["ids"])


def spdx_terms(expression: str) -> list[str]:
    """The ` AND `-joined terms of a table expression, whitespace-normalised."""
    return [" ".join(term.split()) for term in expression.split(" AND ")]


def gpl_family_term(term: str) -> bool:
    """True when every alternative of the term (` OR `-joined) is a GPL-family licence (`GPL-*`, `LGPL-*`,
    `AGPL-*`), with or without a `WITH` exception: `GPL-3.0-or-later WITH GCC-exception-3.1` and
    `LGPL-2.1-or-later` are, `GPL-2.0-only OR MIT` is not (the MIT alternative is a way to take the library
    without the GPL, so it stays carried)."""
    alternatives = [a.split(" WITH ")[0].strip() for a in term.split(" OR ")]
    return all(a.startswith(GPL_FAMILY_PREFIXES) for a in alternatives)


def table_problems(libraries: object) -> list[str]:
    """What is wrong with the table's `libraries` (empty list: nothing)."""
    if not isinstance(libraries, dict) or not libraries:
        return ["`libraries` is missing or not a non-empty object"]
    ids = _spdx_ids()
    problems: list[str] = []
    for base, entry in sorted(libraries.items()):
        if not isinstance(entry, dict):
            problems.append(f"{base}: not an object")
            continue
        for key in ("name", "spdx", "upstream"):
            if not isinstance(entry.get(key), str) or not entry[key].strip():
                problems.append(f"{base}.{key}: missing or empty")
        spdx = entry.get("spdx")
        if isinstance(spdx, str) and spdx.strip():
            if "(" in spdx or ")" in spdx:
                problems.append(f"{base}.spdx: {spdx!r} has parentheses; the table takes ' AND '-joined terms only")
                continue
            try:
                licences.parse_spdx(spdx)
            except licences.SpdxParseError as e:
                problems.append(f"{base}.spdx: {e}")
                continue
            for term in spdx_terms(spdx):
                for alternative in term.split(" OR "):
                    licence_id, _, exception = alternative.strip().partition(" WITH ")
                    if licence_id not in ids:
                        problems.append(f"{base}.spdx: {licence_id!r} is not in scripts/kit/spdx-license-ids.json")
                    if exception and not _EXCEPTION_ID.match(exception.strip()):
                        problems.append(f"{base}.spdx: exception {exception!r} is not an SPDX exception id")
        upstream = entry.get("upstream")
        if isinstance(upstream, str) and upstream.strip() and not upstream.startswith("https://"):
            problems.append(f"{base}.upstream: {upstream!r} is not an https URL")
    return problems


def load_table(path: Optional[Path] = None) -> tuple[dict, str]:
    """`(libraries, sha256 of the table file)`; raises `BundledLibraryRefused` for a malformed table."""
    path = path or TABLE_FILE
    raw = path.read_bytes()
    try:
        libraries = json.loads(raw)["libraries"]
    except (ValueError, KeyError, TypeError) as e:
        raise BundledLibraryRefused("table-malformed", f"{path}: cannot be read ({type(e).__name__}: {e})") from e
    problems = table_problems(libraries)
    if problems:
        raise BundledLibraryRefused("table-malformed", f"{path}: " + "; ".join(problems))
    return libraries, hashlib.sha256(raw).hexdigest()


# =================================================================================================
# Classifying a wheel's files
# =================================================================================================

def base_name(filename: str) -> str:
    """`libgfortran-e1b7dfc8-d8198c01.so.5.0.0` -> `libgfortran`; `libscipy_openblas64_-f552eb69.so` ->
    `libscipy_openblas64_`. A name without a `.so` suffix keeps its whole name (the table will not know it)."""
    stem, found = _SO_SUFFIX.subn("", filename)
    if not found:
        return filename
    return _HASH_SUFFIX.sub("", stem)


def _is_extension_module(basename: str, data: bytes) -> bool:
    m = _EXTENSION_NAME.match(basename)
    return bool(m) and b"PyInit_" + m.group("module").encode("ascii") in data


def _sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


@dataclass
class Stanza:
    file: str
    name: str
    files: list[str]
    licence: Optional[str]
    start: int
    end: int
    licence_line: Optional[int]


_NAME_LINE = re.compile(r"^Name:\s*(.+?)\s*$")
_FILES_LINE = re.compile(r"^Files:\s*(.+?)\s*$")
_LICENCE_LINE = re.compile(r"^License:\s*(.+?)\s*$")
_RULE_LINE = re.compile(r"^-{3,}\s*$")


def parse_stanzas(file: str, text: str) -> list[Stanza]:
    """The `Name:` / `Files:` / `License:` stanzas of one licence text (module doc): a stanza starts at a
    `Name:` line and ends before the next `Name:` line, a `----` line or the end; trailing blank lines are
    not part of it. Line numbers are 1-based."""
    lines = text.splitlines()
    stanzas: list[Stanza] = []
    current: Optional[dict] = None

    def close(last_index: int) -> None:
        nonlocal current
        if current is None:
            return
        end = last_index
        while end > current["start"] and not lines[end - 1].strip():
            end -= 1
        stanzas.append(Stanza(file, current["name"], current["files"], current["licence"], current["start"], end, current["licence_line"]))
        current = None

    for number, line in enumerate(lines, start=1):
        m = _NAME_LINE.match(line)
        if m:
            close(number - 1)
            current = {"name": m.group(1), "files": [], "licence": None, "start": number, "licence_line": None}
            continue
        if _RULE_LINE.match(line):
            close(number - 1)
            continue
        if current is None:
            continue
        m = _FILES_LINE.match(line)
        if m:
            current["files"] += [g for g in re.split(r"[\s,]+", m.group(1)) if g]
            continue
        m = _LICENCE_LINE.match(line)
        if m and current["licence"] is None:
            current["licence"] = " ".join(m.group(1).split())
            current["licence_line"] = number
    close(len(lines))
    return stanzas


def _names_path(stanza: Stanza, path: str) -> Optional[str]:
    """The stanza's `Files:` pattern that matches `path` (also with a version suffix), or None."""
    for pattern in stanza.files:
        if fnmatch.fnmatchcase(path, pattern) or fnmatch.fnmatchcase(path, pattern + ".*"):
            return pattern
    return None


def _licence_files(zf: zipfile.ZipFile) -> list[zipfile.ZipInfo]:
    found = []
    for info in zf.infolist():
        if info.is_dir() or ".dist-info/" not in info.filename:
            continue
        basename = info.filename.rsplit("/", 1)[-1]
        if "/licenses/" in info.filename or _LICENCE_FILE_BASENAME.match(basename):
            found.append(info)
    return sorted(found, key=lambda i: i.filename)


# =================================================================================================
# The inventory
# =================================================================================================

def inventory_wheel(wheel: Path, table: dict) -> WheelInventory:
    """Every bundled library of `wheel` with its sha256, size, base name, licence and evidence (module
    doc), and the wheel's own extension modules. Raises `BundledLibraryRefused` at the first library the
    table does not know or the wheel's text does not confirm."""
    inventory = WheelInventory()
    with zipfile.ZipFile(wheel) as zf:
        candidates: list[tuple[zipfile.ZipInfo, str, bytes]] = []
        for info in sorted(zf.infolist(), key=lambda i: i.filename):
            if info.is_dir():
                continue
            parts = info.filename.split("/")
            basename = parts[-1]
            in_libs_dir = any(p.endswith(".libs") for p in parts[:-1])
            if in_libs_dir:
                candidates.append((info, FOUND_LIBS_DIR, zf.read(info)))
                continue
            if basename.endswith(".py"):
                continue
            with zf.open(info) as handle:
                head = handle.read(4)
            if head != ELF_MAGIC and not _SO_SUFFIX.search(basename):
                continue
            data = zf.read(info)
            if _is_extension_module(basename, data):
                inventory.extension_modules.append(info.filename)
            else:
                candidates.append((info, FOUND_SHARED_OBJECT, data))
        if not candidates:
            return inventory

        stanzas: list[Stanza] = []
        for info in _licence_files(zf):
            stanzas += parse_stanzas(info.filename, zf.read(info).decode("utf-8", "replace"))

        for info, found_in, data in candidates:
            library = base_name(info.filename.rsplit("/", 1)[-1])
            inventory.libraries.append(_inventory_entry(wheel.name, info.filename, found_in, data, library, table, stanzas))
    inventory.libraries.sort(key=lambda lib: lib["path"])
    inventory.extension_modules.sort()
    return inventory


def _inventory_entry(wheel: str, path: str, found_in: str, data: bytes, library: str, table: dict, stanzas: list[Stanza]) -> dict:
    row = table.get(library)
    where = f"{wheel}: bundled library {library!r} ({path})"
    if row is None:
        raise BundledLibraryRefused(
            "unknown-library",
            f"{where} is not in scripts/kit/bundled_libs.json, so its licence is not known and this step does not "
            f"guess one. Read the wheel's licence text, then add an entry (SPDX expression, upstream) to the table.",
            wheel=wheel, library=library,
        )
    naming = [(s, pattern) for s in stanzas if (pattern := _names_path(s, path)) is not None]
    if not naming:
        raise BundledLibraryRefused(
            "licence-text-silent",
            f"{where}: no licence file of the wheel has a Name:/Files:/License: stanza whose Files: pattern matches "
            f"{path!r} (licence files read: {sorted({s.file for s in stanzas}) or 'none with stanzas'}), so the table's "
            f"{row['spdx']!r} for it is unconfirmed. Refusing rather than recording a licence the wheel does not state.",
            wheel=wheel, library=library,
        )
    want = set(spdx_terms(row["spdx"]))
    got = {s.licence for s, _ in naming if s.licence is not None}
    if got != want:
        raise BundledLibraryRefused(
            "licence-mismatch",
            f"{where}: the wheel's licence text says {sorted(got)} ("
            + "; ".join(f"{s.file} lines {s.start}-{s.end}, License: on line {s.licence_line}" for s, _ in naming)
            + f") but scripts/kit/bundled_libs.json says {row['spdx']!r} (terms {sorted(want)}). One of them is wrong; "
            f"the kit is not built until they agree.",
            wheel=wheel, library=library,
        )
    return {
        "path": path,
        "sha256": _sha256(data),
        "size": len(data),
        "library": library,
        "name": row["name"],
        "found_in": found_in,
        "spdx": row["spdx"],
        "gpl_family": any(gpl_family_term(t) for t in spdx_terms(row["spdx"])),
        "upstream": row["upstream"],
        "evidence": [
            {
                "file": s.file, "lines": [s.start, s.end], "stanza": s.name, "files_pattern": pattern,
                "licence": s.licence, "licence_line": s.licence_line,
            }
            for s, pattern in naming
        ],
    }


def main(argv: Optional[list[str]] = None) -> int:
    paths = [Path(a) for a in (sys.argv[1:] if argv is None else argv)]
    if not paths:
        print("usage: wheel_libs.py WHEEL...", file=sys.stderr)
        return 2
    try:
        table, table_sha = load_table()
        out = {"bundled_libs_sha256": table_sha, "wheels": {}}
        for wheel in paths:
            inv = inventory_wheel(wheel, table)
            out["wheels"][wheel.name] = {"bundled_libraries": inv.libraries, "extension_modules": inv.extension_modules}
    except BundledLibraryRefused as e:
        print(f"scripts/kit/wheel_libs.py: REFUSED -- {e}", file=sys.stderr)
        return 2
    print(json.dumps(out, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
