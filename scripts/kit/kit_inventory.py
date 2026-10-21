"""scripts/kit/kit_inventory.py -- question 250(a), task 7: the proof that the kit carries no copyleft
from a base image, as code anyone holding a kit and this repository can re-run.

    .venv/bin/python scripts/kit/kit_inventory.py KIT_DIR [--sbom-dir DIR]... [--list FILE.tsv]

Build-host tool (never in the installer). Standard library plus `image_layer` (the `.deb` reader),
`licences`, `manifest` and `sbom`. It never runs docker and never touches the network: the base images
are known from the committed `services/*/BASE_IMAGE_INVENTORY.json` (`base_inventory.py`'s output).

# Proof 1: no file from a base-image package is in the kit

`enumerate_kit` lists EVERYTHING the kit carries, as `KitItem`s:

- every `KIT_MANIFEST.files` entry (kind `file`), with its origin and, for the image application layer
  (`images/<component>/rootfs/...`), its image path;
- the image application layers file by file as `KIT_MANIFEST.images.<component>.files` records them,
  including the files that only exist inside a carried `.deb` (kind `layer-package-file`), by image
  path and sha256;
- every pack symlink (kind `symlink`; it has a target, no bytes);
- the members of every archive the kit carries: each wheel's zip members (`wheel-member`) and the data
  members of each carried `.deb` (`deb-member`, image path = the member's absolute path).

`find_overlaps` compares each item with the base inventories (`BaseIndex`: every packaged file AND every
unowned file of both bases) two ways:

- **by image path**, for an item that has one: the same path is a path a base package owns (or an
  unowned base file). Paths are compared after the merged-/usr fold (`/bin/x` = `/usr/bin/x`, likewise
  `/sbin`, `/lib`, `/lib64`), because both bases are merged-/usr images and the inventories record the
  path as dpkg lists it;
- **by sha256**, for every item: the same bytes as any base file, at any path. Zero-length content is
  not compared (`excluded_hashes` in `base_overlap_exceptions.json`, the empty file's `e3b0c442...`,
  with the reason).

`base_overlap_exceptions.json` is the ONE committed definition of what may overlap (also read by
`tests/test_kit_zero_egress_install.py`): an exception is a sha256, the kit paths it covers (or any),
and the exact base files that carry the same bytes. A match whose base files are not all listed, or
whose kit path is not covered, is a failure. Each failure names the kit file and the base package.

## The operator-supplied marker, proven true

Since task 4 the property `altavista:sbom:operator-supplied=true` is what exempts a GPL/LGPL SBOM
component from the licence gate. `check_operator_supplied` ties it to files: every component carrying it
in an image SBOM must resolve to the base inventory that SBOM names (a `library` by package name,
version and architecture; the `container` by its digest), a marked component with no inventory entry is
a failure, an inventory package the SBOM does not mark is a failure (both directions), and then none of
the files owned by any marked component (by path and sha256) may appear anywhere in the kit.

# Proof 2: the copyleft scan

`scan_copyleft_sbom` reads each SBOM's component licences, `scan_wheel_inventory` the bundled
libraries `KIT_MANIFEST.wheels` records. A match of `COPYLEFT_RE` is allowed only if the component is
operator-supplied (reported, never failed) or has an entry in `copyleft_accepted.json` with its reason.
A carried wheel with a GPL-family bundled library is a failure (such a wheel is operator-supplied and
is not in the kit: `wheels.operator_supplied`).

# What this does NOT open

An archive nested inside a wheel or a vendored crate (a `.gz` test fixture, a zip inside a wheel) is
hashed as a file and listed in `KitCheck.unopened_archives`; its members are not compared. The report
says how many there are.
"""
from __future__ import annotations

import argparse
import fnmatch
import hashlib
import io
import json
import posixpath
import re
import sys
import tarfile
import tomllib
import zipfile
from collections import defaultdict
from dataclasses import dataclass, field
from pathlib import Path
from typing import Iterable, Optional

KIT_DIR = Path(__file__).resolve().parent
REPO_ROOT = KIT_DIR.parents[1]
if str(KIT_DIR) not in sys.path:
    sys.path.insert(0, str(KIT_DIR))

import image_layer  # noqa: E402
import licences  # noqa: E402
import manifest as kit_manifest  # noqa: E402
import sbom  # noqa: E402

EXCEPTIONS_PATH = KIT_DIR / "base_overlap_exceptions.json"
COPYLEFT_ACCEPTED_PATH = KIT_DIR / "copyleft_accepted.json"
UNOWNED = "<unowned>"

#: GPL, LGPL, AGPL, MPL, EPL, CDDL, EUPL, SSPL, BUSL as SPDX/Debian short names or spelled out. The
#: short names must not be preceded by a letter (so `Apache`, `Simple`, `Deploy` do not match), and may
#: be followed by anything (`GPLv2`, `GPL-2.0-or-later`, `MPL-2.0`). Over-matching is the safe error:
#: a match is a finding to be accepted by name, not a pass.
COPYLEFT_RE = re.compile(
    r"(?<![a-z])(?:[al]?gpl|mpl|epl|cddl|eupl|sspl|busl)"
    r"|general public licen[cs]e|affero|mozilla public|eclipse public"
    r"|common development and distribution|european union public|server side public|business source",
    re.IGNORECASE,
)

_MERGED_USR_TOPS = frozenset({"bin", "sbin", "lib", "lib32", "lib64", "libx32"})


def canon_path(path: str) -> str:
    """The merged-/usr fold: `/bin/grep` and `/usr/bin/grep` are one file in both base images."""
    norm = posixpath.normpath("/" + path.lstrip("/"))
    top = norm.split("/")[1] if norm != "/" else ""
    return "/usr" + norm if top in _MERGED_USR_TOPS else norm


# =================================================================================================
# The committed definitions
# =================================================================================================

@dataclass(frozen=True)
class AcceptedOverlap:
    id: str
    sha256: str
    kit_paths: Optional[frozenset[str]]      # None: any kit path
    base_files: frozenset[tuple[str, str, str]]   # (image, package, path)
    reason: str


@dataclass(frozen=True)
class OverlapPolicy:
    excluded_hashes: dict[str, str]          # sha256 -> what
    accepted: tuple[AcceptedOverlap, ...]

    def accepted_for(self, sha256: str) -> list[AcceptedOverlap]:
        return [a for a in self.accepted if a.sha256 == sha256]


def load_overlap_policy(path: Path = EXCEPTIONS_PATH) -> OverlapPolicy:
    doc = json.loads(path.read_text(encoding="utf-8"))
    if doc.get("schema") != 1:
        raise ValueError(f"{path}: unknown schema {doc.get('schema')!r}")
    excluded = {e["sha256"]: e["what"] for e in doc["excluded_hashes"]}
    accepted = []
    for e in doc["accepted_overlaps"]:
        for key in ("id", "sha256", "base_files", "reason"):
            if not e.get(key):
                raise ValueError(f"{path}: accepted overlap {e!r} has no {key!r}")
        accepted.append(AcceptedOverlap(
            id=e["id"], sha256=e["sha256"],
            kit_paths=None if e.get("kit_paths") is None else frozenset(e["kit_paths"]),
            base_files=frozenset((b["image"], b["package"], b["path"]) for b in e["base_files"]),
            reason=e["reason"],
        ))
    return OverlapPolicy(excluded_hashes=excluded, accepted=tuple(accepted))


def covers_kit_file(policy: OverlapPolicy, kit_path: str, sha256: str) -> Optional[AcceptedOverlap]:
    """The exception that covers a kit file by kit path and sha256 alone, or None. For a caller that
    has no base-file identity to offer (`tests/test_kit_zero_egress_install.py` hashes the saved base
    tarballs, which carry no dpkg ownership); `accepted_overlap_for` is the strict form."""
    for exc in policy.accepted_for(sha256):
        if exc.kit_paths is None or kit_path in exc.kit_paths:
            return exc
    return None


def accepted_overlap_for(
    policy: OverlapPolicy, kit_path: str, sha256: str, base_hits: Iterable["BaseFile"],
) -> Optional[AcceptedOverlap]:
    """The exception that covers a kit file `kit_path` (`sha256`) matching `base_hits` (every base file
    with those bytes), or None. One shared rule for `find_overlaps` and the zero-egress test: the kit
    path is covered, and EVERY base file with these bytes is one the exception names."""
    hits = {(b.image, b.package, b.path) for b in base_hits}
    for exc in policy.accepted_for(sha256):
        if exc.kit_paths is not None and kit_path not in exc.kit_paths:
            continue
        if hits <= exc.base_files:
            return exc
    return None


# =================================================================================================
# The base images (committed inventories)
# =================================================================================================

@dataclass(frozen=True)
class BaseFile:
    image: str        # the recorded image the base belongs to: cfs-image / edge-plugin-image
    package: str      # dpkg package name, or UNOWNED
    path: str         # as the inventory records it
    sha256: str


class BaseIndex:
    """Every regular file of both base images (the committed inventories), indexed by sha256 and by
    merged-/usr-folded path."""

    def __init__(self, inventories: dict[str, dict]):
        self.inventories = inventories
        self.by_sha: dict[str, list[BaseFile]] = defaultdict(list)
        self.by_path: dict[str, list[BaseFile]] = defaultdict(list)
        self.packages: dict[str, dict[str, dict]] = {}
        self.owned: dict[tuple[str, str], list[BaseFile]] = defaultdict(list)   # (image, package) -> files
        self.files_total = 0
        self.files_by_image: dict[str, int] = {}
        for image, inv in sorted(inventories.items()):
            count = 0
            self.packages[image] = {}
            for pkg in inv["packages"]:
                self.packages[image][pkg["name"]] = pkg
                for path, digest in pkg["files"]:
                    self._add(BaseFile(image, pkg["name"], path, digest))
                    count += 1
            for path, digest in inv["unowned_files"]:
                self._add(BaseFile(image, UNOWNED, path, digest))
                count += 1
            self.files_by_image[image] = count
            self.files_total += count

    def _add(self, f: BaseFile) -> None:
        self.by_sha[f.sha256].append(f)
        self.by_path[canon_path(f.path)].append(f)
        self.owned[(f.image, f.package)].append(f)

    def image_for_reference(self, reference: str) -> Optional[str]:
        for image, inv in self.inventories.items():
            if inv["base_image"]["reference"] == reference:
                return image
        return None

    def image_for_digest(self, digest: str) -> Optional[str]:
        for image, inv in self.inventories.items():
            if inv["base_image"]["digest"] == digest:
                return image
        return None


def load_base_index(repo_root: Path = REPO_ROOT) -> BaseIndex:
    inventories = {}
    for image in sorted(sbom.IMAGE_SERVICE_DIRS):
        path = repo_root / sbom.inventory_relpath(image)
        inv = json.loads(path.read_text(encoding="utf-8"))
        if inv["schema"] != sbom.INVENTORY_SCHEMA or inv["package_count"] != len(inv["packages"]):
            raise ValueError(f"{path}: malformed inventory")
        inventories[image] = inv
    return BaseIndex(inventories)


# =================================================================================================
# What the kit carries
# =================================================================================================

@dataclass
class KitItem:
    kit_path: str                  # path in the kit; `archive!/member` for a member
    kind: str                      # file | layer-package-file | symlink | wheel-member | deb-member
    sha256: Optional[str]          # None for a symlink
    size: int
    origin: str
    image_path: Optional[str] = None
    container: Optional[str] = None   # the archive's kit path, for a member
    role: str = ""
    component: Optional[str] = None   # the recorded image whose application layer this is (image_path is in ITS filesystem)


@dataclass
class KitCheck:
    kit: Path
    items: list[KitItem]
    manifest_findings: list = field(default_factory=list)
    inconsistencies: list[str] = field(default_factory=list)
    unopened_archives: list[str] = field(default_factory=list)
    overlaps: list["Overlap"] = field(default_factory=list)       # raw, before exceptions
    accepted: list[tuple["Overlap", AcceptedOverlap]] = field(default_factory=list)
    failures: list["Overlap"] = field(default_factory=list)
    excluded_hits: list[tuple[KitItem, str, int]] = field(default_factory=list)   # (item, what, base files with those bytes)
    base_files_compared: int = 0

    def count(self, kind: str) -> int:
        return sum(1 for i in self.items if i.kind == kind)

    def counts(self) -> dict:
        return {
            "kit_files": self.count("file"),
            "layer_package_files": self.count("layer-package-file"),
            "pack_symlinks": self.count("symlink"),
            "wheel_members": self.count("wheel-member"),
            "deb_members": self.count("deb-member"),
            "ar_members": self.count("ar-member"),
            "items_total": len(self.items),
            "base_files_compared_against": self.base_files_compared,
            "hash_matches_excluded_as_trivial": len(self.excluded_hits),
            "overlaps_before_exceptions": len(self.overlaps),
            "overlaps_accepted": len(self.accepted),
            "overlaps_failing": len(self.failures),
            "unopened_nested_archives": len(self.unopened_archives),
        }


_ARCHIVE_MAGIC = (
    (b"PK\x03\x04", "zip"), (b"\x1f\x8b", "gzip"), (b"\xfd7zXZ\x00", "xz"), (b"BZh", "bzip2"),
    (b"\x28\xb5\x2f\xfd", "zstd"), (b"!<arch>\n", "ar"),
)


def archive_kind(head: bytes) -> Optional[str]:
    for magic, name in _ARCHIVE_MAGIC:
        if head.startswith(magic):
            return name
    if head[257:262] == b"ustar":
        return "tar"
    return None


def _norm_name(name: str) -> str:
    return re.sub(r"[-_.]+", "-", name).lower()


def _sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _python_licences(sbom_dir: Path) -> dict[str, str]:
    """Normalised python distribution name -> its SBOM licence text, from the kit's own av-viewer SBOM."""
    path = sbom_dir / "av-viewer.cdx.json"
    if not path.is_file():
        return {}
    out = {}
    for c in json.loads(path.read_text(encoding="utf-8")).get("components", []):
        out[_norm_name(c["name"])] = ", ".join(licences.component_licence_strings(c)) or "no licence metadata in the SBOM"
    return out


def _vendor_licence(kit: Path, crate_dir: str, cache: dict[str, str]) -> str:
    if crate_dir not in cache:
        cargo_toml = kit / "vendor" / crate_dir / "Cargo.toml"
        text = "no Cargo.toml"
        try:
            pkg = tomllib.loads(cargo_toml.read_text(encoding="utf-8")).get("package", {})
            text = pkg.get("license") or (f"license-file {pkg['license-file']}" if pkg.get("license-file") else "no licence field")
            text = f"{pkg.get('name', crate_dir)} {pkg.get('version', '')}: {text}".replace("  ", " ")
        except (OSError, tomllib.TOMLDecodeError):
            pass
        cache[crate_dir] = text
    return cache[crate_dir]


def origin_of(kit: Path, doc: dict, entry: dict, py_licences: dict, vendor_cache: dict) -> str:
    path, role = entry["path"], entry["role"]
    parts = path.split("/")
    if role == "binary":
        return "AltaVista source (cross-built Rust binary, Linux/aarch64)"
    if role == "vendor":
        return "Rust crate source (cargo vendor) -- " + _vendor_licence(kit, parts[1], vendor_cache) if len(parts) > 2 else "cargo vendor config (generated)"
    if role == "vendor-config":
        return "cargo vendor config (generated by the builder)"
    if role == "wheel":
        wheel = next((w for w in doc["wheels"]["fetched"] if w["filename"] == parts[-1]), None)
        if wheel is None:
            return "Python wheel (not in KIT_MANIFEST.wheels.fetched)"
        if _norm_name(wheel["name"]) == "altavista":
            return "AltaVista source (the viewer's own wheel)"
        return f"Python wheel {wheel['name']} {wheel['version']}, SBOM licence: {py_licences.get(_norm_name(wheel['name']), 'not in the viewer SBOM')}"
    if role == "pack-file":
        imagery = (doc["packs"].get(parts[1]) or {}).get("imagery")
        if imagery:   # question 264: the pyramid is data, not code; its licence is listed with it
            return (f"copied pack {parts[1]} (NASA Blue Marble Next Generation tiles, derived from the original "
                    f"sha256 {imagery['original']['sha256'][:12]}...; {imagery['licence']}; {imagery['licence_spdx']}; no copyleft)")
        return f"copied pack {parts[1]} (AltaVista's own data: data/time leap-second table)" if parts[1] == "data-time" else f"copied pack {parts[1]}"
    if role == "image-dockerfile":
        return f"AltaVista source (runtime Dockerfile of {parts[1]})"
    if role == "image-package":
        return "libssl3 .deb (OpenSSL 3, Apache-2.0, from Debian snapshot, pinned by sha256)"
    if role == "image-package-licence":
        return "libssl3 copyright notice read out of the .deb (Apache-2.0)"
    if role == "image-rootfs":
        image_path = "/" + "/".join(parts[3:])
        if parts[1] == "cfs-image":
            return ("cFS build output (NASA cFS/cFE/OSAL/PSP, Apache-2.0)" if image_path.startswith("/cfs/cpu1/")
                    else "AltaVista source (cFS lockstep shim, relay, entrypoint)")
        return "AltaVista source (edge-plugin binary and fixtures)"
    if role in ("sbom", "sbom-sums"):
        return "AltaVista build evidence (CycloneDX SBOM)"
    if role in ("installer", "site-merged", "suite-merged", "run-fixture", "image-digest"):
        return "AltaVista source"
    return f"kit file (role {role})"


def deb_members(path: Path) -> list[tuple[str, int, str]]:
    """`(absolute image path, size, sha256)` of every regular file in the `.deb`'s data member."""
    out = []
    saw_data = False
    for name, body in image_layer._ar_members(path.read_bytes()):
        if not name.startswith("data.tar"):
            continue
        if name.endswith(".zst"):
            raise image_layer.ImageLayerError(f"{path}: zstd data member is not supported (stdlib tarfile)")
        saw_data = True
        with tarfile.open(fileobj=io.BytesIO(body)) as tf:
            for m in tf.getmembers():
                if m.isreg():
                    content = tf.extractfile(m).read()
                    out.append((posixpath.normpath("/" + m.name), len(content), _sha256_bytes(content)))
    if not saw_data:
        raise image_layer.ImageLayerError(f"{path}: no data member")
    return out


def enumerate_kit(kit: Path, doc: dict, *, sbom_dir: Optional[Path] = None) -> tuple[list[KitItem], list[str], list[str]]:
    """`(items, inconsistencies, unopened_archives)` for the kit at `kit` (`doc` is its KIT_MANIFEST)."""
    sbom_dir = sbom_dir or kit / "sbom"
    py_licences = _python_licences(sbom_dir)
    vendor_cache: dict[str, str] = {}
    items: list[KitItem] = []
    inconsistencies: list[str] = []
    unopened: list[str] = []

    layer: dict[tuple[str, str], dict] = {}     # (component, kit_path) -> manifest record
    package_only: list[tuple[str, dict]] = []
    for component, entry in sorted(doc["images"].items()):
        if not entry.get("collected"):
            continue
        for f in entry["files"]:
            if f["kit_path"]:
                layer[(component, f["kit_path"])] = f
            else:
                package_only.append((component, f))

    seen_layer: set[tuple[str, str]] = set()
    for entry in doc["files"]:
        path, role = entry["path"], entry["role"]
        parts = path.split("/")
        image_path = None
        if role == "image-rootfs":
            component = parts[1]
            image_path = "/" + "/".join(parts[3:])
            rec = layer.get((component, path))
            if rec is None:
                inconsistencies.append(f"{path}: in the kit's image rootfs but not recorded in KIT_MANIFEST.images.{component}.files")
            else:
                seen_layer.add((component, path))
                if rec["image_path"] != image_path or rec["sha256"] != entry["sha256"]:
                    inconsistencies.append(f"{path}: KIT_MANIFEST.images.{component}.files records image path {rec['image_path']} sha256 {rec['sha256']}, the kit file is {image_path} sha256 {entry['sha256']}")
        items.append(KitItem(path, "file", entry["sha256"], entry["size"],
                             origin_of(kit, doc, entry, py_licences, vendor_cache), image_path, None, role,
                             parts[1] if image_path else None))

        full = kit / path
        try:
            with open(full, "rb") as fh:
                head = fh.read(270)
        except OSError:
            head = b""
        nested = archive_kind(head)
        if role == "wheel":
            _wheel_members(kit, doc, entry, items, unopened, py_licences)
        elif role == "image-package":
            for member_path, size, digest in deb_members(full):
                items.append(KitItem(f"{path}!{member_path}", "deb-member", digest, size,
                                     f"member of {parts[-1]} (libssl3, Apache-2.0)", member_path, path, "image-package-member", parts[1]))
        elif nested == "ar":
            # A static library / import library (the vendored windows-* crates): its members are
            # objects, hashed like files.
            for name, body in image_layer._ar_members(full.read_bytes()):
                if body and name not in ("", "/", "//"):
                    items.append(KitItem(f"{path}!{name}", "ar-member", _sha256_bytes(body), len(body),
                                         f"member of the ar archive {parts[-1]}", None, path, "ar-member"))
        elif nested is not None:
            unopened.append(f"{path} ({nested})")

    for (component, kit_path) in sorted(set(layer) - seen_layer):
        inconsistencies.append(f"KIT_MANIFEST.images.{component}.files names {kit_path}, which is not in KIT_MANIFEST.files")
    for component, f in package_only:
        items.append(KitItem(f"{component}:{f['image_path']}", "layer-package-file", f["sha256"], f["size"],
                             f"image application layer, file of {f['source']} (installed from the carried .deb)", f["image_path"], None, "image-layer", component))
    for link in doc["pack_symlinks"]:
        items.append(KitItem(link["path"], "symlink", None, 0,
                             f"symlink inside copied pack {link['pack']} -> {link['link_target']}", None, None, "pack-symlink"))
    return items, inconsistencies, unopened


def _wheel_members(kit: Path, doc: dict, entry: dict, items: list[KitItem], unopened: list[str], py_licences: dict) -> None:
    path = entry["path"]
    filename = path.rsplit("/", 1)[-1]
    wheel = next((w for w in doc["wheels"]["fetched"] if w["filename"] == filename), None)
    label = f"{wheel['name']} {wheel['version']}" if wheel else filename
    licence = py_licences.get(_norm_name(wheel["name"]), "not in the viewer SBOM") if wheel else "unknown"
    own = wheel is not None and _norm_name(wheel["name"]) == "altavista"
    origin = f"member of wheel {label}, " + ("AltaVista source" if own else f"SBOM licence: {licence}")
    with zipfile.ZipFile(kit / path) as zf:
        for info in zf.infolist():
            if info.is_dir():
                continue
            data = zf.read(info)
            nested = archive_kind(data[:270])
            if nested is not None:
                unopened.append(f"{path}!{info.filename} ({nested})")
            items.append(KitItem(f"{path}!{info.filename}", "wheel-member", _sha256_bytes(data), len(data), origin, None, path, "wheel-member"))


# =================================================================================================
# Comparing the kit with the base images
# =================================================================================================

@dataclass
class Overlap:
    item: KitItem
    by_path: list[BaseFile]
    by_sha: list[BaseFile]

    @property
    def base_files(self) -> list[BaseFile]:
        seen, out = set(), []
        for b in [*self.by_path, *self.by_sha]:
            if (b.image, b.package, b.path) not in seen:
                seen.add((b.image, b.package, b.path))
                out.append(b)
        return out

    def describe(self) -> str:
        how = []
        if self.by_path:
            how.append("same image path")
        if self.by_sha:
            how.append("same sha256")
        packages = sorted({f"{b.package} ({b.image} base {b.path})" for b in self.base_files})
        return f"{self.item.kit_path} [{' + '.join(how)}; sha256 {self.item.sha256}] matches base package(s): {'; '.join(packages)}"


def find_overlaps(items: list[KitItem], index: BaseIndex, policy: OverlapPolicy,
                  *, restrict: Optional[set[tuple[str, str]]] = None) -> list[Overlap]:
    """Every item that is the same file as a base file, by path or by bytes. `restrict`: only base files
    of those `(image, package)` pairs count (the operator-supplied marker proof)."""
    def keep(files: list[BaseFile]) -> list[BaseFile]:
        return [b for b in files if restrict is None or (b.image, b.package) in restrict]

    out = []
    for item in items:
        # An application layer is laid over ITS OWN base: its paths are compared with that base's files
        # only. (The libssl3 .deb for the Debian image has the paths Ubuntu's libssl3 has; the cFS image's
        # base is Ubuntu, and the .deb is never installed there.) Bytes are compared with both bases.
        by_path = ([b for b in keep(index.by_path.get(canon_path(item.image_path), [])) if b.image == item.component]
                   if item.image_path and item.component else [])
        by_sha = []
        if item.sha256 and item.size > 0 and item.sha256 not in policy.excluded_hashes:
            by_sha = keep(index.by_sha.get(item.sha256, []))
        if by_path or by_sha:
            out.append(Overlap(item, by_path, by_sha))
    return out


def split_by_policy(overlaps: list[Overlap], policy: OverlapPolicy) -> tuple[list[tuple[Overlap, AcceptedOverlap]], list[Overlap]]:
    """An overlap is accepted only by an exception that names this kit path (or any), this sha256, and
    EVERY base file the item matched. An item that shares only an image path with a base file, with other
    bytes, names a base file no exception lists (an exception lists files with ITS sha256), so it fails."""
    accepted, failing = [], []
    for ov in overlaps:
        item = ov.item
        exc = accepted_overlap_for(policy, item.kit_path, item.sha256, ov.base_files) if item.sha256 else None
        (accepted.append((ov, exc)) if exc else failing.append(ov))
    return accepted, failing


def check_kit(kit: Path, *, repo_root: Path = REPO_ROOT, sbom_dir: Optional[Path] = None,
              index: Optional[BaseIndex] = None, policy: Optional[OverlapPolicy] = None) -> KitCheck:
    """Proof 1, part one: list everything the kit carries and compare it with the base images."""
    doc = json.loads((kit / "KIT_MANIFEST").read_text(encoding="utf-8"))
    index = index or load_base_index(repo_root)
    policy = policy or load_overlap_policy()
    result = KitCheck(kit=kit, items=[])
    result.manifest_findings = kit_manifest.verify_manifest(kit)
    result.items, result.inconsistencies, result.unopened_archives = enumerate_kit(kit, doc, sbom_dir=sbom_dir)
    result.base_files_compared = index.files_total
    result.overlaps = find_overlaps(result.items, index, policy)
    result.excluded_hits = [
        (it, policy.excluded_hashes[it.sha256], len(index.by_sha[it.sha256]))
        for it in result.items if it.sha256 in policy.excluded_hashes and it.size > 0 and it.sha256 in index.by_sha
    ]
    # KIT_MANIFEST.sboms names each kit SBOM by hash: it must be the hash of the file the kit carries.
    listed = {f["path"]: f["sha256"] for f in doc["files"]}
    for name, digest in sorted(doc["sboms"].items()):
        if listed.get(f"sbom/{name}.cdx.json") != digest:
            result.inconsistencies.append(f"KIT_MANIFEST.sboms.{name} is {digest}, the kit's sbom/{name}.cdx.json is {listed.get(f'sbom/{name}.cdx.json')}")
    result.accepted, result.failures = split_by_policy(result.overlaps, policy)
    return result


def report_failures(result: KitCheck) -> list[str]:
    return [ov.describe() for ov in result.failures]


# =================================================================================================
# The carried .deb(s)
# =================================================================================================

def _dep5_stanzas(text: str) -> list[dict[str, str]]:
    stanzas = []
    for block in re.split(r"\n\s*\n", text):
        fields: dict[str, str] = {}
        key = None
        for line in block.splitlines():
            if line[:1] in (" ", "\t") and key:
                fields[key] += "\n" + line.strip()
            elif ":" in line and not line.startswith("#"):
                key, _, value = line.partition(":")
                fields[key] = value.strip()
        if fields:
            stanzas.append(fields)
    return stanzas


def check_carried_debs(kit: Path, doc: dict, index: BaseIndex, allow: frozenset[str],
                       repo_root: Path = REPO_ROOT) -> dict:
    """For each `.deb` the kit carries: it is not a base-image package (by name), its recorded binary
    package licence is on the allow list, its copyright notice in the kit is the one inside the `.deb`,
    and no data member is a file of a stanza licensed under a non-allowed term (the libssl3 copyright
    names an Artistic-or-GPL-1+ build-time helper; the proof is that the `.deb` holds none of its files).
    Returns `{"debs": [...], "problems": [...]}`."""
    problems: list[str] = []
    debs = []
    for comp, entry in sorted(doc["images"].items()):
        if not entry.get("collected"):
            continue
        records = {p["package"]: p for p in image_layer.load_runtime_packages(repo_root, comp)}
        for pkg in entry.get("packages") or []:
            deb_path = kit / pkg["kit_path"]
            deb = image_layer.read_deb(deb_path)
            name = deb.control.get("Package")
            info = {"image": comp, "package": name, "kit_path": pkg["kit_path"], "sha256": deb.sha256}
            debs.append(info)
            # A .deb is installed over its own image's base: it must not be a package of THAT base. (The
            # cFS image's Ubuntu base has a package of the same name; the .deb is never installed there.)
            if comp not in index.packages:
                problems.append(f"{pkg['kit_path']}: no base-image inventory for {comp}")
            elif name in index.packages[comp]:
                problems.append(f"{pkg['kit_path']}: the carried .deb {name} is a package of its own image's base ({index.inventories[comp]['base_image']['reference']})")
            info["name_in_other_base"] = sorted(i for i, pkgs in index.packages.items() if i != comp and name in pkgs)
            if deb.sha256 != pkg["sha256"]:
                problems.append(f"{pkg['kit_path']}: sha256 {deb.sha256} != KIT_MANIFEST {pkg['sha256']}")
            record = records.get(name)
            if record is None:
                problems.append(f"{pkg['kit_path']}: {name} is not in {image_layer.IMAGE_SERVICE_DIRS[comp]}/runtime-packages.json")
                continue
            binary_licence = record["licence"]["binary_package_licence"]
            for f in licences.evaluate_license_field(comp, name, record["version"], binary_licence, allow):
                problems.append(f"{pkg['kit_path']}: binary package licence {f.licence!r} is not on the deny.toml allow list ({f.reason})")
            info["binary_package_licence"] = binary_licence
            if deb.copyright is None:
                problems.append(f"{pkg['kit_path']}: no copyright notice inside the .deb")
                continue
            kit_copyright = kit / pkg["licence_kit_path"]
            if kit_copyright.read_bytes() != deb.copyright:
                problems.append(f"{pkg['licence_kit_path']} is not the copyright notice inside {pkg['kit_path']}")
            members = [m for m, _s, _h in deb_members(deb_path)]
            info["data_members"] = len(members)
            info["non_allowed_stanzas"] = []
            for stanza in _dep5_stanzas(deb.copyright.decode("utf-8", "replace")):
                if "Files" not in stanza or "License" not in stanza:
                    continue
                lic_name = stanza["License"].splitlines()[0].strip()
                if not licences.evaluate_license_field(comp, name, record["version"], _debian_to_spdx(lic_name), allow):
                    continue
                patterns = stanza["Files"].split()
                hit = [m for m in members if any(fnmatch.fnmatchcase(m.lstrip("/"), p) or fnmatch.fnmatchcase(m, p) for p in patterns)]
                info["non_allowed_stanzas"].append({"licence": lic_name, "files": patterns, "data_members_matched": len(hit)})
                if hit:
                    problems.append(f"{pkg['kit_path']}: data member(s) {hit} fall under the copyright stanza Files: {patterns} licensed {lic_name}, which is not on the allow list")
    return {"debs": debs, "problems": problems}


def _debian_to_spdx(name: str) -> str:
    """The Debian short names of the libssl3 copyright file, only as far as the allow-list test needs
    them: `Apache-2.0` is SPDX already; anything else is passed through, so it is not allowed."""
    return name


# =================================================================================================
# The operator-supplied marker, tied to files
# =================================================================================================

@dataclass
class MarkerReport:
    sbom: str
    marked_components: int = 0
    marked_libraries: int = 0
    resolved: list[tuple[str, str]] = field(default_factory=list)      # (image, package)
    problems: list[str] = field(default_factory=list)
    base_images: set[str] = field(default_factory=set)


def marked_components(doc: dict) -> list[dict]:
    return [c for c in doc.get("components", []) if licences.is_operator_supplied(c)]


def _prop(component: dict, name: str) -> Optional[str]:
    return next((p["value"] for p in component.get("properties") or [] if p["name"] == name), None)


def check_operator_supplied(sboms: dict[str, dict], index: BaseIndex) -> tuple[list[MarkerReport], set[tuple[str, str]]]:
    """Ties every operator-supplied marker in `sboms` (`{name: CycloneDX document}`) to the base
    inventories, both ways, and returns `(reports, owned)` where `owned` is the set of `(image,
    package)` pairs whose files the marked components own (a `container` component owns every file of
    its inventory, packages and unowned files)."""
    reports: list[MarkerReport] = []
    owned: set[tuple[str, str]] = set()
    for name, doc in sorted(sboms.items()):
        rep = MarkerReport(name)
        reports.append(rep)
        marked = marked_components(doc)
        rep.marked_components = len(marked)
        marked_names: dict[str, set[str]] = defaultdict(set)    # image -> package names marked
        for c in marked:
            ctype = c.get("type")
            if ctype == "container":
                digest = _prop(c, sbom.PROP_BASE_IMAGE_DIGEST)
                image = index.image_for_digest(digest or "")
                reference = _prop(c, sbom.PROP_BASE_IMAGE)
                if image is None:
                    rep.problems.append(f"{name}: marked container {c['name']} (digest {digest}) has no base-image inventory")
                    continue
                if index.inventories[image]["base_image"]["reference"] != reference:
                    rep.problems.append(f"{name}: marked container {c['name']} says base {reference}, the inventory for {digest} says {index.inventories[image]['base_image']['reference']}")
                    continue
                rep.base_images.add(image)
                for pkg in index.packages[image]:
                    owned.add((image, pkg))
                owned.add((image, UNOWNED))
                rep.resolved.append((image, "<whole base image>"))
            elif ctype == "library":
                rep.marked_libraries += 1
                reference = _prop(c, sbom.PROP_BASE_IMAGE)
                image = index.image_for_reference(reference or "")
                if image is None:
                    rep.problems.append(f"{name}: marked library {c['name']} {c['version']} names base image {reference!r}, which has no committed inventory")
                    continue
                rep.base_images.add(image)
                pkg = index.packages[image].get(c["name"])
                if pkg is None:
                    rep.problems.append(f"{name}: marked library {c['name']} {c['version']} has no entry in {sbom.inventory_relpath(image)}")
                    continue
                if pkg["version"] != c["version"] or pkg["architecture"] != _prop(c, sbom.PROP_ARCHITECTURE):
                    rep.problems.append(f"{name}: marked library {c['name']} {c['version']} ({_prop(c, sbom.PROP_ARCHITECTURE)}) but the inventory has {pkg['version']} ({pkg['architecture']})")
                    continue
                marked_names[image].add(c["name"])
                owned.add((image, c["name"]))
                rep.resolved.append((image, c["name"]))
            else:
                rep.problems.append(f"{name}: operator-supplied component {c['name']} is of type {ctype!r}, which this proof cannot tie to files")
        # The other direction: an inventory package the SBOM does not mark operator-supplied.
        for image in rep.base_images:
            missing = sorted(set(index.packages[image]) - marked_names[image])
            if missing:
                rep.problems.append(f"{name}: {len(missing)} package(s) of the {image} base inventory are not marked operator-supplied in the SBOM: {missing[:10]}")
    return reports, owned


def check_marked_files_absent(items: list[KitItem], index: BaseIndex, policy: OverlapPolicy,
                              owned: set[tuple[str, str]]) -> tuple[int, list[Overlap], list[tuple[Overlap, AcceptedOverlap]]]:
    """`(owned files checked, failing overlaps, accepted overlaps)` for the files owned by the marked
    components only, anywhere in the kit, by path and by sha256."""
    checked = sum(len(index.owned.get(key, [])) for key in owned)
    overlaps = find_overlaps(items, index, policy, restrict=owned)
    accepted, failing = split_by_policy(overlaps, policy)
    return checked, failing, accepted


# =================================================================================================
# The copyleft scan
# =================================================================================================

@dataclass(frozen=True)
class AcceptedCopyleft:
    component: str
    licence: str
    kind: str
    reason: str


def load_copyleft_accepted(path: Path = COPYLEFT_ACCEPTED_PATH) -> list[AcceptedCopyleft]:
    doc = json.loads(path.read_text(encoding="utf-8"))
    if doc.get("schema") != 1:
        raise ValueError(f"{path}: unknown schema")
    out = []
    for e in doc["accepted"]:
        if not e.get("reason") or e.get("kind") not in ("sbom-component", "bundled-library"):
            raise ValueError(f"{path}: entry {e!r} needs a reason and a kind of sbom-component or bundled-library")
        out.append(AcceptedCopyleft(e["component"], e["licence"], e["kind"], e["reason"]))
    return out


@dataclass
class ScanResult:
    scanned: str
    components: int = 0
    failures: list[str] = field(default_factory=list)
    operator_supplied_reported: list[tuple[str, str]] = field(default_factory=list)
    accepted_used: list[tuple[str, str, str]] = field(default_factory=list)    # (component, licence, reason)
    copyleft_licence_strings: int = 0


def scan_copyleft_sbom(name: str, doc: dict, accepted: list[AcceptedCopyleft]) -> ScanResult:
    res = ScanResult(name)
    by_key = {(a.component, a.licence): a for a in accepted if a.kind == "sbom-component"}
    for c in doc.get("components", []):
        res.components += 1
        for raw in licences.component_licence_strings(c):
            if not COPYLEFT_RE.search(raw):
                continue
            res.copyleft_licence_strings += 1
            if licences.is_operator_supplied(c):
                res.operator_supplied_reported.append((c["name"], raw))
            elif (c["name"], raw) in by_key:
                res.accepted_used.append((c["name"], raw, by_key[(c["name"], raw)].reason))
            else:
                res.failures.append(f"{name}: {c['name']} {c.get('version', '')} declares {raw!r}: copyleft, not marked operator-supplied and not on copyleft_accepted.json")
    return res


def scan_wheel_inventory(doc: dict, accepted: list[AcceptedCopyleft], kit: Optional[Path] = None) -> ScanResult:
    """The bundled native libraries `KIT_MANIFEST.wheels` records for the wheels the kit CARRIES: any
    with a GPL-family term (or any copyleft licence) is a failure unless accepted by name. The
    operator-supplied wheels are listed, and must not be in the kit."""
    res = ScanResult("KIT_MANIFEST.wheels")
    by_key = {(a.component, a.licence): a for a in accepted if a.kind == "bundled-library"}
    for wheel in doc["wheels"]["fetched"]:
        res.components += 1
        for lib in wheel.get("bundled_libraries") or []:
            spdx = lib["spdx"]
            if lib.get("gpl_family") or COPYLEFT_RE.search(spdx):
                res.copyleft_licence_strings += 1
                if (lib["library"], spdx) in by_key:
                    res.accepted_used.append((lib["library"], spdx, by_key[(lib["library"], spdx)].reason))
                else:
                    res.failures.append(f"wheel {wheel['filename']} carries bundled library {lib['library']} ({lib['path']}) licensed {spdx!r}: copyleft (gpl_family={lib.get('gpl_family')})")
    for entry in doc["wheels"].get("operator_supplied") or []:
        res.operator_supplied_reported.append((entry["filename"], ", ".join(entry.get("gpl_family_libraries") or [])))
        carried = [f["path"] for f in doc["files"] if f["path"] == f"wheels/{entry['filename']}" or f["sha256"] == entry["sha256"]]
        if carried:
            res.failures.append(f"operator-supplied wheel {entry['filename']} is carried by the kit at {carried}")
        if kit is not None and (kit / "wheels" / entry["filename"]).exists():
            res.failures.append(f"operator-supplied wheel {entry['filename']} is on disk in the kit")
    return res


def unused_accepted(accepted: list[AcceptedCopyleft], used: Iterable[tuple[str, str, str]]) -> list[AcceptedCopyleft]:
    used_keys = {(c, l) for c, l, _r in used}
    return [a for a in accepted if (a.component, a.licence) not in used_keys]


def load_sboms(directory: Path) -> dict[str, dict]:
    return {p.name.removesuffix(".cdx.json"): json.loads(p.read_text(encoding="utf-8")) for p in sorted(directory.glob("*.cdx.json"))}


# =================================================================================================
# Both proofs over one kit
# =================================================================================================

@dataclass
class Proof:
    check: KitCheck
    debs: dict
    sbom_sets: dict[str, dict[str, dict]]                 # label -> {sbom name: document}
    markers: dict[str, list[MarkerReport]] = field(default_factory=dict)
    marked_files_checked: int = 0
    marked_failing: list[Overlap] = field(default_factory=list)
    marked_accepted: list[tuple[Overlap, AcceptedOverlap]] = field(default_factory=list)
    marked_owned: set[tuple[str, str]] = field(default_factory=set)
    scans: dict[str, list[ScanResult]] = field(default_factory=dict)
    wheel_scan: Optional[ScanResult] = None
    stale_accepted: list[AcceptedCopyleft] = field(default_factory=list)
    problems: list[str] = field(default_factory=list)


def regenerate_image_sboms() -> dict[str, dict]:
    """The two image SBOMs as `scripts/kit/sbom.py` generates them now (no docker, no cargo): they list
    the base images' packages with the operator-supplied marker, which the SBOM copies a kit carries
    only gain when the lead regenerates `docs/compliance/sbom/`."""
    return {c: sbom.image_sbom(c) for c in sbom.IMAGE_COMPONENTS}


def prove_kit(kit: Path, *, repo_root: Path = REPO_ROOT, extra_sbom_dirs: Iterable[Path] = (),
              regenerate: bool = True) -> Proof:
    """Proof 1 (no base-image file in the kit; the operator-supplied marker tied to files) and proof 2 (the
    copyleft scan) over the kit at `kit`. Every failure is a line of `Proof.problems`."""
    doc = json.loads((kit / "KIT_MANIFEST").read_text(encoding="utf-8"))
    index = load_base_index(repo_root)
    policy = load_overlap_policy()
    accepted = load_copyleft_accepted()
    allow = licences.load_allow_list(repo_root / "deny.toml")

    check = check_kit(kit, repo_root=repo_root, index=index, policy=policy)
    debs = check_carried_debs(kit, doc, index, allow, repo_root)
    sets: dict[str, dict[str, dict]] = {"the kit's own sbom/ copies": load_sboms(kit / "sbom")}
    if regenerate:
        sets["image SBOMs regenerated by scripts/kit/sbom.py now"] = regenerate_image_sboms()
    for d in extra_sbom_dirs:
        sets[f"SBOMs in {d}"] = load_sboms(Path(d))
    proof = Proof(check=check, debs=debs, sbom_sets=sets)

    proof.problems += [f"KIT_MANIFEST verification: {f.message}" for f in check.manifest_findings]
    proof.problems += [f"inconsistency: {m}" for m in check.inconsistencies]
    proof.problems += [f"base file in the kit: {ov.describe()}" for ov in check.failures]
    proof.problems += [f"carried .deb: {m}" for m in debs["problems"]]

    used: list[tuple[str, str, str]] = []
    for label, sboms in sets.items():
        reports, owned = check_operator_supplied(sboms, index)
        proof.markers[label] = reports
        proof.marked_owned |= owned
        for rep in reports:
            proof.problems += [f"operator-supplied marker ({label}): {m}" for m in rep.problems]
        proof.scans[label] = [scan_copyleft_sbom(n, d, accepted) for n, d in sboms.items()]
        for scan in proof.scans[label]:
            proof.problems += [f"copyleft ({label}): {m}" for m in scan.failures]
            used += scan.accepted_used
    proof.marked_files_checked, proof.marked_failing, proof.marked_accepted = check_marked_files_absent(
        check.items, index, policy, proof.marked_owned)
    proof.problems += [f"file owned by an operator-supplied component is in the kit: {ov.describe()}" for ov in proof.marked_failing]

    proof.wheel_scan = scan_wheel_inventory(doc, accepted, kit)
    proof.problems += [f"copyleft (wheel inventory): {m}" for m in proof.wheel_scan.failures]
    used += proof.wheel_scan.accepted_used
    proof.stale_accepted = unused_accepted(accepted, used)
    proof.problems += [f"copyleft_accepted.json: entry {a.component} / {a.licence} matches no component in any scanned SBOM or wheel inventory" for a in proof.stale_accepted]

    # An exception that names kit paths must name paths the kit has (a stale or invented path is a lie).
    kit_paths = {it.kit_path for it in check.items}
    covered = {ov.item.kit_path for ov, _e in check.accepted}
    for exc in policy.accepted:
        if exc.kit_paths is not None:
            for p in sorted(exc.kit_paths - kit_paths):
                if _kit_has_images(doc):
                    proof.problems += [f"base_overlap_exceptions.json: {exc.id} names {p}, which is not in this kit"]
            if _kit_has_images(doc):
                for p in sorted((exc.kit_paths & kit_paths) - covered):
                    proof.problems += [f"base_overlap_exceptions.json: {exc.id} names {p}, which is in the kit but no longer overlaps a base file"]
    return proof


def _kit_has_images(doc: dict) -> bool:
    return any(e.get("collected") for e in doc["images"].values())


def render_proof(proof: Proof) -> str:
    check = proof.check
    out = [render_report(check), ""]
    out += ["carried .deb(s):"]
    for d in proof.debs["debs"]:
        out.append(f"  {d['kit_path']}: {d['package']} sha256 {d['sha256']}, binary package licence {d.get('binary_package_licence')}, {d.get('data_members')} data members; "
                   f"not a package of its own image's base; same name in another image's base: {d.get('name_in_other_base')}")
        for st in d.get("non_allowed_stanzas", []):
            out.append(f"    copyright stanza licensed {st['licence']!r} (Files: {' '.join(st['files'])}) matches {st['data_members_matched']} data member(s)")
    if not proof.debs["debs"]:
        out.append("  none (this kit carries no image application layer)")
    out += ["", "operator-supplied marker:"]
    for label, reports in proof.markers.items():
        for rep in reports:
            out.append(f"  [{label}] {rep.sbom}: {rep.marked_components} marked component(s) ({rep.marked_libraries} libraries), "
                       f"{len(rep.resolved)} resolved to an inventory entry, {len(rep.problems)} problem(s)")
    out.append(f"  files owned by marked components, compared with every kit item by path and sha256: {proof.marked_files_checked}"
               f" (over {len(proof.marked_owned)} (image, package) owners); matches in the kit: {len(proof.marked_failing)} failing, {len(proof.marked_accepted)} accepted by exception")
    out += ["", "copyleft scan of SBOM component licences:"]
    for label, scans in proof.scans.items():
        n_components = sum(s.components for s in scans)
        n_hits = sum(s.copyleft_licence_strings for s in scans)
        reported = sum(len(s.operator_supplied_reported) for s in scans)
        out.append(f"  [{label}] {len(scans)} SBOMs, {n_components} components, {n_hits} copyleft licence strings: "
                   f"{reported} operator-supplied (reported), {sum(len(s.accepted_used) for s in scans)} accepted, {sum(len(s.failures) for s in scans)} failing")
        for s in scans:
            for comp, lic, reason in s.accepted_used:
                out.append(f"    accepted: {s.scanned}: {comp} {lic} -- {reason}")
    ws = proof.wheel_scan
    out += ["", f"copyleft scan of the wheel inventory (KIT_MANIFEST.wheels): {ws.components} wheels carried, "
                f"{ws.copyleft_licence_strings} copyleft bundled librar(ies), {len(ws.failures)} failing; "
                f"operator-supplied wheels listed, not carried: {[w for w, _l in ws.operator_supplied_reported]}"]
    out += ["", "PROBLEMS:"] + ([f"  {p}" for p in proof.problems] or ["  none"])
    return "\n".join(out) + "\n"


# =================================================================================================
# The printed inventory
# =================================================================================================

def origin_summary(items: list[KitItem]) -> list[tuple[str, int, int]]:
    """`(origin class, count, bytes)`: the per-file origin collapsed to its class (vendored crates and
    wheel members by what they are, not one row each)."""
    groups: dict[str, list[int]] = defaultdict(lambda: [0, 0])
    for it in items:
        key = it.origin
        if it.kind == "file" and it.role == "vendor":
            key = "Rust crate source (cargo vendor), licences per crate in the listing"
        elif it.kind == "wheel-member":
            key = "members of " + (it.container or "").rsplit("/", 1)[-1]
        elif it.kind == "deb-member":
            key = "members of the libssl3 .deb data archive (Apache-2.0)"
        elif it.kind == "ar-member":
            key = "members of vendored static/import libraries (windows-* crates)"
        elif it.kind == "file" and it.role == "wheel":
            key = "wheel file: " + key
        g = groups[key]
        g[0] += 1
        g[1] += it.size
    return sorted(((k, v[0], v[1]) for k, v in groups.items()), key=lambda r: (-r[2], r[0]))


def render_listing(items: list[KitItem]) -> str:
    lines = ["kind\tsize\tsha256\tkit_path\timage_path\torigin"]
    for it in items:
        lines.append(f"{it.kind}\t{it.size}\t{it.sha256 or '-'}\t{it.kit_path}\t{it.image_path or '-'}\t{it.origin}")
    return "\n".join(lines) + "\n"


def render_report(result: KitCheck) -> str:
    out = [f"kit {result.kit}  KIT_MANIFEST sha256 {sbom.sha256_file(result.kit / 'KIT_MANIFEST')}", "", "what the kit carries, by origin (count, bytes):"]
    for key, count, size in origin_summary(result.items):
        out.append(f"  {count:>6}  {size:>12}  {key}")
    out += ["", "counts:"]
    out += [f"  {k}: {v}" for k, v in result.counts().items()]
    if result.accepted:
        out += ["", "accepted overlaps (base_overlap_exceptions.json):"]
        for ov, exc in result.accepted:
            out.append(f"  [{exc.id}] {ov.item.kit_path} sha256 {ov.item.sha256} == " + "; ".join(sorted(f"{b.image}:{b.package}:{b.path}" for b in ov.base_files)))
    out += ["", "failing overlaps:"] + ([f"  {d}" for d in report_failures(result)] or ["  none"])
    out += ["", "inconsistencies between KIT_MANIFEST.files and KIT_MANIFEST.images:"] + ([f"  {d}" for d in result.inconsistencies] or ["  none"])
    out += ["", f"nested archives hashed as files and not opened ({len(result.unopened_archives)}):"] + [f"  {d}" for d in result.unopened_archives]
    return "\n".join(out) + "\n"


def main(argv: Optional[list[str]] = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("kit", type=Path)
    parser.add_argument("--sbom-dir", action="append", default=[], type=Path,
                        help="also tie the operator-supplied markers in, and scan for copyleft, the SBOMs in this directory (repeatable)")
    parser.add_argument("--no-regenerate", action="store_true", help="do not regenerate the image SBOMs with scripts/kit/sbom.py")
    parser.add_argument("--list", type=Path, help="write every item (file, layer file, symlink, archive member) with its origin to this TSV")
    args = parser.parse_args(argv)
    kit = args.kit.resolve()
    proof = prove_kit(kit, extra_sbom_dirs=args.sbom_dir, regenerate=not args.no_regenerate)
    sys.stdout.write(render_proof(proof))
    if args.list:
        args.list.write_text(render_listing(proof.check.items), encoding="utf-8")
        print(f"wrote {args.list}")
    return 2 if proof.problems else 0


if __name__ == "__main__":
    raise SystemExit(main())
