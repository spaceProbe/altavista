"""scripts/kit/base_inventory.py -- question 250(a): the committed inventory of each recorded
image's operator-supplied BASE image, read by `scripts/kit/sbom.py` (which stays Docker-free).

For each base image named in `scripts/kit/operator_supplied.json` (`kind` `container-base-image`)
this runs the pinned base, by digest, once, with `docker run --rm --pull never --network none`,
and reads the whole filesystem back as one `tar` stream. Everything else is computed on the host,
in Python, from that stream -- one pass, no per-file process, nothing that depends on the
container's clock:

- the package list (`/var/lib/dpkg/status`: every `install ok installed` stanza), with version,
  architecture and source package;
- per package, every distinct `License:` value of its `/usr/share/doc/<pkg>/copyright` (sorted;
  the same `^License:` lines `docs/kit-operator-supplied.md`'s "Reproducing the audit" one-liner
  prints), whether that file is machine-readable DEP-5 (a first-line `Format:`), and whether the
  file exists at all;
- per package, every REGULAR file the package owns (`/var/lib/dpkg/info/<pkg>.list`) with its
  SHA-256, so the kit's builder can prove it carries no file from a base-image package. A listed
  path is recorded as dpkg lists it (`/bin/bash`); the file is looked up through the image's
  symlinks (`/bin` -> `usr/bin`), so a path under a symlinked directory still finds its file. A
  listed path that is a directory, a symlink, or absent from the image (dpkg `path-exclude`
  removes docs and locales from the slim images) is not a regular file the image holds and is
  not listed;
- every other regular file in the image (owned by no package: `/etc/passwd`, dpkg's own
  database, `ld.so.cache`, ...) with its SHA-256, as a separate sorted list.

The file is sorted, has no timestamp, no host path and no image id (an image id is a property of
one host's store; the digest is the identity). Generating it twice is byte-identical
(`--check` regenerates in memory and compares with the committed file).

This module takes the host-wide docker lock itself (`altavista.docker_test_lock`) around its
docker calls. NEVER run it under `with_docker_lock.py` (question 240: the lock is not re-entrant
across a parent and its child). It never pulls (`--pull never`) and never removes an image.

    .venv/bin/python scripts/kit/base_inventory.py                 # write both committed files
    .venv/bin/python scripts/kit/base_inventory.py --check         # regenerate, compare, exit 2 if stale
    .venv/bin/python scripts/kit/base_inventory.py --image cfs-image --out-dir <dir>

What the tar stream leaves out, and why: `/proc`, `/sys`, `/dev` (kernel mounts) and
`/.dockerenv`, `/etc/hosts`, `/etc/hostname`, `/etc/resolv.conf` (files the container runtime
creates in every container; none is in the image). `tests/test_base_inventory.py` covers the
parsing with an in-memory tar and no docker.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import posixpath
import re
import subprocess
import sys
import tarfile
import tempfile
import uuid
from pathlib import Path
from typing import IO, Iterable, Optional

REPO_ROOT = Path(__file__).resolve().parents[2]
PINS_PATH = REPO_ROOT / "scripts" / "kit" / "operator_supplied.json"

KIT_DIR = Path(__file__).resolve().parent
if str(KIT_DIR) not in sys.path:
    sys.path.insert(0, str(KIT_DIR))
# The path/pin constants live in sbom.py (the kit's bundled installer carries sbom.py and
# licences.py only; this module is a build-host tool and imports sbom, never the reverse).
from sbom import (  # noqa: E402
    IMAGE_SERVICE_DIRS, INVENTORY_FILENAME, INVENTORY_SCHEMA as SCHEMA, PINS_PATH,
    base_pin_for, inventory_path, inventory_relpath, load_pins,
)

#: Excluded from the tar stream (see the module doc).
TAR_EXCLUDES = (
    "./proc", "./sys", "./dev", "./.dockerenv", "./etc/hosts", "./etc/hostname",
    "./etc/resolv.conf",
)


# =================================================================================================
# Parsing the tar stream (pure: no docker)
# =================================================================================================

_LICENCE_RE = re.compile(r"^License:(.*)$")
_FORMAT_RE = re.compile(r"^Format:\s*\S")
_DOC_COPYRIGHT_RE = re.compile(r"^/usr/share/doc/[^/]+/copyright$")
_DPKG_INFO_LIST_RE = re.compile(r"^/var/lib/dpkg/info/([^/]+)\.list$")
DPKG_STATUS = "/var/lib/dpkg/status"
OS_RELEASE_FILES = ("/etc/os-release", "/usr/lib/os-release")

#: Content kept in memory while streaming (small text); everything else is only hashed.
def _keep_content(path: str) -> bool:
    return (
        path == DPKG_STATUS
        or path in OS_RELEASE_FILES
        or bool(_DOC_COPYRIGHT_RE.match(path))
        or bool(_DPKG_INFO_LIST_RE.match(path))
    )


def _norm(name: str) -> str:
    """An archive member name as an absolute, normalised path (`./usr/bin/x` -> `/usr/bin/x`)."""
    while name.startswith("./"):
        name = name[2:]
    return posixpath.normpath("/" + ("" if name == "." else name))


class ImageFs:
    """The image filesystem as read from the tar stream: type, symlink target, sha256 and the few
    small files we keep. Resolution follows symlinks the way the kernel would inside the image."""

    def __init__(self) -> None:
        self.kind: dict[str, str] = {}        # path -> "file" | "dir" | "symlink" | "other"
        self.target: dict[str, str] = {}      # symlink path -> link target text
        self.sha: dict[str, str] = {}         # regular file path -> sha256 hex
        self.content: dict[str, bytes] = {}   # kept small files
        self._hardlinks: list[tuple[str, str]] = []

    @classmethod
    def from_tar(cls, tf: tarfile.TarFile) -> "ImageFs":
        fs = cls()
        for ti in tf:
            path = _norm(ti.name)
            if path == "/":
                continue
            if ti.isdir():
                fs.kind[path] = "dir"
            elif ti.issym():
                fs.kind[path] = "symlink"
                fs.target[path] = ti.linkname
            elif ti.islnk():
                fs.kind[path] = "file"
                fs._hardlinks.append((path, _norm(ti.linkname)))
            elif ti.isreg():
                fs.kind[path] = "file"
                h = hashlib.sha256()
                keep = _keep_content(path)
                buf = bytearray() if keep else None
                f = tf.extractfile(ti)
                assert f is not None
                for chunk in iter(lambda: f.read(1 << 20), b""):
                    h.update(chunk)
                    if buf is not None:
                        buf += chunk
                fs.sha[path] = h.hexdigest()
                if buf is not None:
                    fs.content[path] = bytes(buf)
            else:
                fs.kind[path] = "other"
        for path, target in fs._hardlinks:
            # A hard link names an earlier member of the same archive.
            if target not in fs.sha:
                raise RuntimeError(f"hard link {path} -> {target}: target not seen before it")
            fs.sha[path] = fs.sha[target]
            if target in fs.content:
                fs.content[path] = fs.content[target]
        return fs

    def resolve(self, path: str, *, follow_last: bool) -> str:
        """The canonical path `path` names inside the image (symlinks in every parent component
        followed; the last one only if `follow_last`). Raises on a loop."""
        todo = [c for c in path.split("/") if c]
        resolved: list[str] = []
        hops = 0
        while todo:
            comp = todo.pop(0)
            if comp == ".":
                continue
            if comp == "..":
                if resolved:
                    resolved.pop()
                continue
            cand = "/" + "/".join(resolved + [comp])
            if self.kind.get(cand) == "symlink" and (todo or follow_last):
                hops += 1
                if hops > 64:
                    raise RuntimeError(f"too many symlinks resolving {path}")
                tgt = self.target[cand]
                if tgt.startswith("/"):
                    resolved = []
                todo = [c for c in tgt.split("/") if c] + todo
                continue
            resolved.append(comp)
        return "/" + "/".join(resolved)

    def regular_file(self, listed: str) -> Optional[str]:
        """The canonical path of the regular file `listed` names, or None (directory, symlink,
        absent)."""
        canon = self.resolve(listed, follow_last=False)
        return canon if self.kind.get(canon) == "file" else None


def parse_status(text: str) -> list[dict]:
    """The `install ok installed` stanzas of `/var/lib/dpkg/status`, as name/version/arch/source."""
    packages = []
    for stanza in text.split("\n\n"):
        fields: dict[str, str] = {}
        key = None
        for line in stanza.splitlines():
            if not line:
                continue
            if line[0] in " \t":
                continue  # continuation (Description, Depends...) -- not needed
            k, _, v = line.partition(":")
            key = k
            fields[k] = v.strip()
        if not fields or fields.get("Status") != "install ok installed":
            continue
        source = fields.get("Source", "")
        source_name = source.split(" (")[0].strip() if source else fields["Package"]
        packages.append({
            "name": fields["Package"],
            "version": fields["Version"],
            "architecture": fields["Architecture"],
            "source": source_name,
        })
    return packages


def parse_copyright(data: Optional[bytes]) -> tuple[list[str], bool]:
    """`(sorted distinct non-empty License: values, machine-readable DEP-5)`. The values are the
    text after `License:` on lines that start with it (the one-liner's `grep -h "^License:"`)."""
    if data is None:
        return [], False
    lines = data.decode("utf-8", errors="replace").splitlines()
    values = set()
    for line in lines:
        m = _LICENCE_RE.match(line)
        if m and m.group(1).strip():
            values.add(m.group(1).strip())
    first = next((l for l in lines if l.strip()), "")
    return sorted(values), bool(_FORMAT_RE.match(first))


def _list_file_for(fs: ImageFs, name: str, arch: str) -> Optional[bytes]:
    for cand in (f"/var/lib/dpkg/info/{name}.list", f"/var/lib/dpkg/info/{name}:{arch}.list"):
        if cand in fs.content:
            return fs.content[cand]
    return None


def parse_os_release(text: str) -> dict:
    """`{"id": ..., "version_id": ...}` from os-release text (the purl `distro=` qualifier)."""
    fields = {}
    for line in text.splitlines():
        k, sep, v = line.partition("=")
        if sep:
            fields[k.strip()] = v.strip().strip('"').strip("'")
    return {"id": fields.get("ID", ""), "version_id": fields.get("VERSION_ID", "")}


def build_inventory(fs: ImageFs, *, image: str, reference: str, digest: str, platform: str) -> dict:
    """The inventory document (see the module doc) for a parsed image filesystem."""
    status = fs.content.get(DPKG_STATUS)
    if status is None:
        raise RuntimeError("no /var/lib/dpkg/status in the image")
    os_path = fs.resolve("/etc/os-release", follow_last=True)
    if os_path not in fs.content:
        raise RuntimeError("no /etc/os-release in the image")
    os_release = parse_os_release(fs.content[os_path].decode("utf-8"))
    owned: set[str] = set()
    packages = []
    for pkg in sorted(parse_status(status.decode("utf-8")), key=lambda p: (p["name"], p["architecture"])):
        listing = _list_file_for(fs, pkg["name"], pkg["architecture"])
        if listing is None:
            raise RuntimeError(f"{pkg['name']}: no dpkg .list file")
        files = {}
        for listed in listing.decode("utf-8").splitlines():
            if not listed or listed == "/.":
                continue
            canon = fs.regular_file(listed)
            if canon is None:
                continue
            files[posixpath.normpath(listed)] = fs.sha[canon]
            owned.add(canon)
        copyright_path = fs.resolve(f"/usr/share/doc/{pkg['name']}/copyright", follow_last=True)
        data = fs.content.get(copyright_path) if fs.kind.get(copyright_path) == "file" else None
        licences, machine_readable = parse_copyright(data)
        packages.append({
            **pkg,
            "copyright_file": data is not None,
            "licences": licences,
            "machine_readable": machine_readable,
            "files": sorted([p, h] for p, h in files.items()),
        })
    unowned = sorted([p, fs.sha[p]] for p, k in fs.kind.items() if k == "file" and p not in owned)
    return {
        "base_image": {"digest": digest, "image": image, "platform": platform, "reference": reference},
        "doc": "docs/kit-operator-supplied.md",
        "os_release": os_release,
        "notes": (
            "Generated by scripts/kit/base_inventory.py from the digest-pinned base, run once "
            "with `docker run --rm --pull never --network none`; read by scripts/kit/sbom.py. "
            "'licences' are the distinct 'License:' lines of the package's "
            "/usr/share/doc/<pkg>/copyright (Debian short names, not SPDX); an empty list is a "
            "named gap (read the copyright file by hand). 'files' are the regular files the "
            "package owns, as dpkg lists them, with sha256. 'unowned_files' are regular files in "
            "the image owned by no package."
        ),
        "package_count": len(packages),
        "packages": packages,
        "schema": SCHEMA,
        "unowned_files": unowned,
    }


# =================================================================================================
# Writing (deterministic, one line per package header and per file)
# =================================================================================================

def _j(obj: object) -> str:
    return json.dumps(obj, sort_keys=True, ensure_ascii=False, separators=(", ", ": "))


def dump_inventory(inv: dict) -> str:
    """Valid JSON, sorted keys, one line per file entry (a diff names the file), a trailing
    newline. Not `json.dumps(indent=...)`: that spends a line per array element of every
    `[path, sha256]` pair and three times the bytes."""
    out = ["{"]
    keys = sorted(inv)
    for ki, key in enumerate(keys):
        comma = "," if ki < len(keys) - 1 else ""
        value = inv[key]
        if key == "packages":
            out.append('  "packages": [')
            for pi, pkg in enumerate(value):
                head = {k: v for k, v in pkg.items() if k != "files"}
                out.append("    {")
                for hk in sorted(head):
                    out.append(f'      "{hk}": {_j(head[hk])},')
                out.append('      "files": [')
                for fi, pair in enumerate(pkg["files"]):
                    out.append(f"        {_j(pair)}" + ("," if fi < len(pkg["files"]) - 1 else ""))
                out.append("      ]")
                out.append("    }" + ("," if pi < len(value) - 1 else ""))
            out.append("  ]" + comma)
        elif key == "unowned_files":
            out.append('  "unowned_files": [')
            for fi, pair in enumerate(value):
                out.append(f"    {_j(pair)}" + ("," if fi < len(value) - 1 else ""))
            out.append("  ]" + comma)
        else:
            out.append(f'  "{key}": {_j(value)}{comma}')
    out.append("}")
    return "\n".join(out) + "\n"


# =================================================================================================
# Docker (the only part that needs it)
# =================================================================================================

def _docker(*args: str) -> list[str]:
    return ["docker", *args]


def read_image_filesystem(reference: str, run_id: str) -> tuple[ImageFs, str]:
    """`(filesystem, "os/arch")` of the local image `reference` (must already be present: never
    pulls). Caller holds the docker lock."""
    insp = subprocess.run(
        _docker("image", "inspect", "--format", "{{.Os}}/{{.Architecture}}", reference),
        capture_output=True, text=True, check=True,
    )
    platform = insp.stdout.strip()
    cmd = _docker(
        "run", "--rm", "--pull", "never", "--network", "none",
        "--label", "av.test=1", "--label", f"av.basesbom={run_id}",
        "--entrypoint", "tar", reference,
        "-cf", "-", *(f"--exclude={e}" for e in TAR_EXCLUDES), "-C", "/", ".",
    )
    with tempfile.TemporaryFile() as err:
        proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=err)
        assert proc.stdout is not None
        with tarfile.open(fileobj=proc.stdout, mode="r|") as tf:
            fs = ImageFs.from_tar(tf)
        rc = proc.wait()
        if rc != 0:
            err.seek(0)
            raise RuntimeError(f"`docker run ... tar` exited {rc}: {err.read().decode(errors='replace')}")
    return fs, platform


def generate(image: str) -> str:
    """The inventory text for `image`'s base. Caller holds the docker lock."""
    name, pin = base_pin_for(image)
    fs, platform = read_image_filesystem(pin["reference"], uuid.uuid4().hex[:12])
    inv = build_inventory(
        fs, image=image, reference=pin["reference"], digest=pin["digest"], platform=platform,
    )
    return dump_inventory(inv)


def main(argv: Optional[list[str]] = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--image", action="append", choices=sorted(IMAGE_SERVICE_DIRS),
                        help="only this recorded image's base (repeatable); default both")
    parser.add_argument("--out-dir", help="write <image>.BASE_IMAGE_INVENTORY.json here instead of the committed path")
    parser.add_argument("--check", action="store_true",
                        help="regenerate in memory and compare with the committed file; exit 2 if they differ")
    args = parser.parse_args(argv)

    sys.path.insert(0, str(REPO_ROOT))
    from altavista.docker_test_lock import lock_docker_tests  # noqa: E402

    images = args.image or sorted(IMAGE_SERVICE_DIRS)
    stale = []
    with lock_docker_tests():
        for image in images:
            text = generate(image)
            sha = hashlib.sha256(text.encode("utf-8")).hexdigest()
            if args.check:
                committed = inventory_path(image).read_text(encoding="utf-8")
                same = committed == text
                print(f"{image}: {'identical to' if same else 'DIFFERS from'} {inventory_relpath(image)} "
                      f"(regenerated sha256 {sha}, {len(text)} bytes)", file=sys.stderr)
                if not same:
                    stale.append(image)
                continue
            if args.out_dir:
                out = Path(args.out_dir) / f"{image}.{INVENTORY_FILENAME}"
                out.parent.mkdir(parents=True, exist_ok=True)
            else:
                out = inventory_path(image)
            out.write_text(text, encoding="utf-8")
            print(f"wrote {out} sha256={sha} bytes={len(text)}", file=sys.stderr)
    return 2 if stale else 0


if __name__ == "__main__":
    raise SystemExit(main())
