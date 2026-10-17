"""scripts/kit/image_layer.py -- question 250(a), first half: the kit carries each container image's
APPLICATION LAYER, not the image.

# Why

`build_kit.py --with-images` used to `docker save` the two whole images, base layers included,
which redistributed the Ubuntu 22.04 and Debian 12 userlands (GPL/LGPL). Those bases are
operator-supplied now (`docs/kit-operator-supplied.md` items 1 and 2). The kit instead carries,
per image, exactly what the image adds over its base:

- `images/<component>/Dockerfile` -- the committed runtime Dockerfile (`services/<svc>/Dockerfile.kit`),
  `FROM <base>@<digest>` and nothing that needs a network;
- `images/<component>/rootfs/...` -- the application files at their image paths, with their
  recorded modes in `KIT_MANIFEST`;
- for the edge-plugin image, the recorded `libssl3` `.deb` and its copyright file.

A host-side step (`scripts/kit/image_build.py`, run as `installer/install.sh --build-images`)
builds the image with `docker build --network none --pull=false` on a host that already holds the
base image, and compares the result with the recorded RUNTIME-CONTENT HASH (question 185), not the
image id (an id changes with every layer timestamp). This module is the builder-side half: pins, the
runtime-content hash, extraction from the recorded image, the `.deb` record, and what the installer's
step needs to know about the image it will build (`runtime_set`: the roots the hash is taken over;
`config`: the recorded image's `.Config` subset). It does not build or install anything.

# The runtime-content hash (question 185)

SHA-256 over the UTF-8 bytes of one line `"<path> <sha256 of the file>"` per file of the
runtime-content set (image path, one space, lowercase hex, nothing else), sorted bytewise by the
whole line, each line ending in `"\\n"`; reported `sha256:<hex>`. The sets (`RUNTIME_SETS`) are
the ones `services/cfs/IMAGE_DIGEST.md` and `services/edge-plugin/build-image.sh` define. This
module is the Python implementation; the shell implementation is in each `build-image.sh`
(`sha256sum` run inside the built image). Question 164: two independent implementations of one
defined algorithm.

# How files are read out of the recorded image

`docker create` (never started), `docker cp` of the set's paths, `docker rm -v` (the plugin image
declares a `VOLUME`). Nothing else touches the image.

# Network

`ensure_deb` is the only function here that can reach the network (one pinned fetch of a `.deb`
from snapshot.debian.org over verified TLS, at kit-build time only, question 154), and only when
the bytes are not already in `.av-test-tmp/kit-deb-cache/<sha256>/`. Whether it did is never
recorded: a kit's `KIT_MANIFEST` is byte-identical either way.
"""
from __future__ import annotations

import hashlib
import io
import json
import os
import re
import shutil
import ssl
import subprocess
import tarfile
import tempfile
import urllib.request
import uuid
from contextlib import contextmanager
from dataclasses import dataclass, field
from pathlib import Path
from typing import Callable, Iterator, Mapping, Optional

REPO_ROOT = Path(__file__).resolve().parents[2]

#: component (`sbom.IMAGE_COMPONENTS`) -> the service directory holding its Dockerfiles.
IMAGE_SERVICE_DIRS: dict[str, str] = {
    "edge-plugin-image": "services/edge-plugin",
    "cfs-image": "services/cfs",
}

OPERATOR_SUPPLIED_FILE = "operator_supplied.json"
RUNTIME_PACKAGES_FILE = "runtime-packages.json"
DEB_CACHE_DIR = Path(".av-test-tmp") / "kit-deb-cache"


class ImageLayerError(RuntimeError):
    """Any refusal in this module: a pin that disagrees with a Dockerfile, a hash that disagrees with
    its record, a file the kit cannot represent. Never swallowed by the builder."""


# =================================================================================================
# Runtime-content sets and the hash
# =================================================================================================

@dataclass(frozen=True)
class RuntimeSet:
    """What an image adds over its base, by image path. `files`: exact regular files. `trees`:
    every regular file below a directory (anything else below it, a symlink for example, is
    refused: the runtime Dockerfile could not recreate it). `packages`: dpkg packages whose regular
    files are included (`dpkg -L`, kept when a regular file and not a symlink)."""

    files: tuple[str, ...] = ()
    trees: tuple[str, ...] = ()
    packages: tuple[str, ...] = ()


RUNTIME_SETS: dict[str, RuntimeSet] = {
    # services/cfs/IMAGE_DIGEST.md, "Runtime-content set".
    "cfs-image": RuntimeSet(
        files=("/cfs/av-lockstep-shim", "/cfs/av-lockstep-relay", "/cfs/container-entrypoint.sh"),
        trees=("/cfs/cpu1",),
    ),
    # services/edge-plugin/build-image.sh, step 3a.
    "edge-plugin-image": RuntimeSet(
        files=("/usr/local/bin/av-edge-plugin",),
        trees=("/opt/edge-plugin",),
        packages=("libssl3",),
    ),
}


def runtime_set_entry(component: str) -> dict:
    """`RUNTIME_SETS[component]` as the manifest records it (`images.<component>.runtime_set`): the
    installer's image step reads the built image through exactly these roots, so a file added under
    one of them (or a package file the kit did not record) changes the hash it computes."""
    rset = RUNTIME_SETS[component]
    return {"files": list(rset.files), "trees": list(rset.trees), "packages": list(rset.packages)}


#: The `docker image inspect .Config` keys the installer's image step compares (question 250(a)):
#: what decides how a container made from the image behaves. `Hostname`, `Image`, `Domainname` and
#: the like describe the build, not the image.
CONFIG_KEYS = (
    "Entrypoint", "Cmd", "User", "WorkingDir", "Volumes", "Labels", "Env",
    "ExposedPorts", "Healthcheck", "StopSignal", "OnBuild", "Shell",
)


def config_subset(config: Mapping[str, object]) -> dict:
    """`CONFIG_KEYS` out of a `.Config` object, every key present (absent ones `None`)."""
    return {k: config.get(k) for k in CONFIG_KEYS}


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_path(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def runtime_content_lines(sha_by_path: Mapping[str, str]) -> list[str]:
    return sorted(f"{path} {sha}" for path, sha in sha_by_path.items())


def runtime_content_hash(sha_by_path: Mapping[str, str]) -> str:
    """`sha256:<hex>` over `runtime_content_lines`, each line followed by "\\n". An empty set is
    refused: it would hash to a constant that equals any other empty set."""
    if not sha_by_path:
        raise ImageLayerError("runtime-content hash over an empty file set")
    data = "".join(line + "\n" for line in runtime_content_lines(sha_by_path)).encode("utf-8")
    return "sha256:" + sha256_bytes(data)


# =================================================================================================
# Pins: operator-supplied.json vs the Dockerfiles
# =================================================================================================

def load_operator_supplied(repo_root: Path = REPO_ROOT) -> dict:
    return json.loads((repo_root / "scripts" / "kit" / OPERATOR_SUPPLIED_FILE).read_text(encoding="utf-8"))


def base_pin_for(component: str, repo_root: Path = REPO_ROOT) -> dict:
    """The operator-supplied base image of `component`: `{id, item, reference, digest}`."""
    found = [
        (cid, c) for cid, c in load_operator_supplied(repo_root)["components"].items()
        if c.get("kind") == "container-base-image" and component in c.get("base_of", [])
    ]
    if len(found) != 1:
        raise ImageLayerError(f"{component}: expected exactly one operator-supplied base image, found {len(found)}")
    cid, c = found[0]
    return {"id": cid, "item": c["item"], "reference": c["reference"], "digest": c["digest"]}


@dataclass(frozen=True)
class Instruction:
    keyword: str
    args: str


def dockerfile_instructions(path: Path) -> list[Instruction]:
    """Instructions of a Dockerfile: comments and blank lines dropped, backslash continuations
    joined (a comment line inside a continuation is dropped too, as the classic builder does)."""
    out: list[Instruction] = []
    pending = ""
    for raw in path.read_text(encoding="utf-8").splitlines():
        stripped = raw.strip()
        if stripped.startswith("#"):
            continue
        if not stripped and not pending:
            continue
        line = stripped
        if line.endswith("\\"):
            pending += line[:-1].rstrip() + " "
            continue
        full = (pending + line).strip()
        pending = ""
        if not full:
            continue
        keyword, _, args = full.partition(" ")
        out.append(Instruction(keyword.upper(), args.strip()))
    if pending.strip():
        keyword, _, args = pending.strip().partition(" ")
        out.append(Instruction(keyword.upper(), args.strip()))
    return out


def from_references(instructions: list[Instruction]) -> list[str]:
    refs = []
    for ins in instructions:
        if ins.keyword == "FROM":
            parts = [p for p in ins.args.split() if not p.startswith("--")]
            refs.append(parts[0])
    return refs


def final_stage(instructions: list[Instruction]) -> list[Instruction]:
    last = max(i for i, ins in enumerate(instructions) if ins.keyword == "FROM")
    return instructions[last:]


#: Instructions that configure the image (as opposed to building its filesystem).
CONFIG_KEYWORDS = frozenset({
    "LABEL", "USER", "WORKDIR", "VOLUME", "ENTRYPOINT", "CMD", "ENV", "EXPOSE", "HEALTHCHECK",
    "STOPSIGNAL", "SHELL", "ONBUILD", "MAINTAINER", "ARG",
})


def _normalised_args(ins: Instruction):
    if ins.args.lstrip().startswith("["):
        return json.loads(ins.args)
    return " ".join(ins.args.split())


def config_instructions(instructions: list[Instruction]) -> list[tuple[str, object]]:
    return [(i.keyword, _normalised_args(i)) for i in instructions if i.keyword in CONFIG_KEYWORDS]


#: What a runtime Dockerfile may contain: the instruction kinds, and the only commands a RUN may
#: run (everything here works with `--network none`).
KIT_DOCKERFILE_KEYWORDS = frozenset({"FROM", "LABEL", "COPY", "RUN", "WORKDIR", "VOLUME", "USER", "ENTRYPOINT"})
KIT_RUN_COMMANDS = frozenset({"chmod", "dpkg", "rm", "groupadd", "useradd", "mkdir", "chown"})
_NETWORK_WORDS = re.compile(r"\b(apt|apt-get|aptitude|curl|wget|pip|pip3|git|npm|cargo|ftp|ssh|scp|nc)\b")


def kit_dockerfile_problems(repo_root: Path, component: str) -> list[str]:
    """Everything wrong with `services/<svc>/Dockerfile.kit` against the pins and the development
    Dockerfile; empty when it is as it must be. Used by the builder (refuses) and by the tests."""
    svc = repo_root / IMAGE_SERVICE_DIRS[component]
    dev = dockerfile_instructions(svc / "Dockerfile")
    kit = dockerfile_instructions(svc / "Dockerfile.kit")
    problems: list[str] = []
    base = base_pin_for(component, repo_root)["reference"]

    dev_froms, kit_froms = from_references(dev), from_references(kit)
    if any(f != base for f in dev_froms):
        problems.append(f"{svc.name}/Dockerfile FROM {dev_froms} does not equal the operator-supplied pin {base}")
    if kit_froms != [base]:
        problems.append(f"{svc.name}/Dockerfile.kit FROM {kit_froms} is not exactly the operator-supplied pin {base}")

    if config_instructions(final_stage(dev)) != config_instructions(kit):
        problems.append(
            f"{svc.name}/Dockerfile.kit configuration {config_instructions(kit)} differs from the development "
            f"Dockerfile's final stage {config_instructions(final_stage(dev))}"
        )

    for ins in kit:
        if ins.keyword not in KIT_DOCKERFILE_KEYWORDS:
            problems.append(f"Dockerfile.kit uses {ins.keyword}, which a network-free runtime Dockerfile may not")
        if ins.keyword == "COPY":
            if ins.args.startswith("--") or "<<" in ins.args:
                problems.append(f"Dockerfile.kit COPY uses a flag or heredoc the legacy builder rejects: {ins.args}")
        if ins.keyword == "RUN":
            if "<<" in ins.args or "--mount" in ins.args:
                problems.append(f"Dockerfile.kit RUN uses a heredoc or --mount the legacy builder rejects: {ins.args}")
            for segment in re.split(r"&&|;|\|", ins.args):
                words = segment.split()
                while words and "=" in words[0] and not words[0].startswith("-"):
                    words = words[1:]  # VAR=value prefix
                if words and words[0] not in KIT_RUN_COMMANDS:
                    problems.append(f"Dockerfile.kit RUN runs {words[0]!r}, not one of {sorted(KIT_RUN_COMMANDS)}")
            if _NETWORK_WORDS.search(ins.args):
                problems.append(f"Dockerfile.kit RUN names a network tool: {ins.args}")
    return problems


# =================================================================================================
# Recorded runtime-content hashes
# =================================================================================================

def read_recorded_runtime_hash(repo_root: Path, component: str) -> str:
    """The runtime-content hash the component's own record carries. cFS: the machine-written header
    of `IMAGE_CONTEXT_MANIFEST.txt` (what `sbom.py` already reads), cross-checked against
    `IMAGE_DIGEST.md`'s "Recorded runtime-content hash for this pin" block. Edge plugin: the block
    `build-image.sh` writes into `IMAGE_DIGEST.md`."""
    svc = repo_root / IMAGE_SERVICE_DIRS[component]
    digest_text = (svc / "IMAGE_DIGEST.md").read_text(encoding="utf-8")
    if component == "edge-plugin-image":
        m = re.search(r"Runtime-content hash \(question 185[^\n]*\n```\n(sha256:[0-9a-f]{64})\n```", digest_text)
        if not m:
            raise ImageLayerError(
                f"{component}: services/edge-plugin/IMAGE_DIGEST.md records no runtime-content hash -- "
                f"re-run services/edge-plugin/build-image.sh, which writes it"
            )
        return m.group(1)
    manifest_text = (svc / "IMAGE_CONTEXT_MANIFEST.txt").read_text(encoding="utf-8")
    m = re.search(r"# Runtime-content hash for this build[^:\n]*:\s*(sha256:[0-9a-f]{64})", manifest_text)
    if not m:
        raise ImageLayerError(f"{component}: IMAGE_CONTEXT_MANIFEST.txt has no runtime-content hash header")
    from_manifest = m.group(1)
    m2 = re.search(r"Recorded runtime-content hash for this pin.*?```\n(sha256:[0-9a-f]{64})\n```", digest_text, re.S)
    if not m2 or m2.group(1) != from_manifest:
        raise ImageLayerError(
            f"{component}: IMAGE_CONTEXT_MANIFEST.txt records {from_manifest}, IMAGE_DIGEST.md records "
            f"{m2.group(1) if m2 else None} -- the two records disagree"
        )
    return from_manifest


# =================================================================================================
# The recorded .deb (services/edge-plugin/runtime-packages.json)
# =================================================================================================

def load_runtime_packages(repo_root: Path, component: str) -> list[dict]:
    path = repo_root / IMAGE_SERVICE_DIRS[component] / RUNTIME_PACKAGES_FILE
    if not path.is_file():
        return []
    doc = json.loads(path.read_text(encoding="utf-8"))
    if doc.get("image") != component:
        raise ImageLayerError(f"{path} is for {doc.get('image')!r}, not {component!r}")
    return doc["packages"]


@dataclass(frozen=True)
class DebFile:
    path: str      # absolute image path, e.g. /usr/lib/aarch64-linux-gnu/libssl.so.3
    kind: str      # "file", "dir", "symlink", "other"
    mode: int
    sha256: Optional[str]


@dataclass
class Deb:
    path: Path
    sha256: str
    size: int
    control: dict[str, str]
    files: dict[str, DebFile]
    copyright: Optional[bytes]


def _ar_members(data: bytes) -> Iterator[tuple[str, bytes]]:
    if data[:8] != b"!<arch>\n":
        raise ImageLayerError("not a Debian package (no ar magic)")
    pos = 8
    while pos + 60 <= len(data):
        header = data[pos:pos + 60]
        name = header[:16].decode("ascii").strip().rstrip("/")
        size = int(header[48:58].decode("ascii").strip())
        pos += 60
        yield name, data[pos:pos + size]
        pos += size + (size % 2)


def _parse_control(text: str) -> dict[str, str]:
    fields: dict[str, str] = {}
    key = None
    for line in text.splitlines():
        if line[:1] in (" ", "\t") and key:
            fields[key] += "\n" + line
        elif ":" in line:
            key, _, value = line.partition(":")
            fields[key] = value.strip()
    return fields


def read_deb(path: Path) -> Deb:
    data = path.read_bytes()
    control: dict[str, str] = {}
    files: dict[str, DebFile] = {}
    copyright_bytes: Optional[bytes] = None
    saw_data = False
    for name, body in _ar_members(data):
        if name.startswith("control.tar"):
            with tarfile.open(fileobj=io.BytesIO(body)) as tf:
                for m in tf.getmembers():
                    if m.name.lstrip("./") == "control":
                        control = _parse_control(tf.extractfile(m).read().decode("utf-8"))
        elif name.startswith("data.tar"):
            if name.endswith(".zst"):
                raise ImageLayerError(f"{path}: zstd-compressed data member is not supported (stdlib tarfile)")
            saw_data = True
            with tarfile.open(fileobj=io.BytesIO(body)) as tf:
                for m in tf.getmembers():
                    abs_path = "/" + m.name.lstrip("./").rstrip("/") if m.name.lstrip("./") else "/"
                    if m.isreg():
                        content = tf.extractfile(m).read()
                        files[abs_path] = DebFile(abs_path, "file", m.mode & 0o7777, sha256_bytes(content))
                        if abs_path.endswith("/copyright") and abs_path.startswith("/usr/share/doc/"):
                            copyright_bytes = content
                    elif m.isdir():
                        files[abs_path] = DebFile(abs_path, "dir", m.mode & 0o7777, None)
                    elif m.issym():
                        files[abs_path] = DebFile(abs_path, "symlink", m.mode & 0o7777, None)
                    else:
                        files[abs_path] = DebFile(abs_path, "other", m.mode & 0o7777, None)
    if not saw_data or not control:
        raise ImageLayerError(f"{path}: missing control or data member")
    return Deb(path=path, sha256=sha256_bytes(data), size=len(data), control=control, files=files, copyright=copyright_bytes)


def copyright_licences(copyright_bytes: bytes) -> list[str]:
    """Every distinct `License:` field value of a machine-readable Debian copyright file, in order
    of first appearance."""
    seen: list[str] = []
    for line in copyright_bytes.decode("utf-8", "replace").splitlines():
        if line.startswith("License:"):
            value = line.partition(":")[2].strip()
            if value and value not in seen:
                seen.append(value)
    return seen


def _download(url: str, timeout: float = 120.0) -> bytes:
    # Default context: certificate and host-name verification on, never weakened.
    context = ssl.create_default_context()
    with urllib.request.urlopen(url, timeout=timeout, context=context) as response:  # noqa: S310
        return response.read()


def ensure_deb(
    repo_root: Path, record: dict, *, download: Optional[Callable[[str], bytes]] = None,
) -> Path:
    """The recorded package's `.deb`, from `.av-test-tmp/kit-deb-cache/<sha256>/<filename>` or, once,
    from `record["snapshot_url"]`. Whatever its source it must equal the record's sha256 AND size
    (and the snapshot's own sha1 when recorded), or this raises and leaves nothing behind in the
    cache. This is the only function in the module that can use the network, at kit-build time."""
    want_sha, want_size, filename = record["sha256"], record["size"], record["filename"]
    cached = repo_root / DEB_CACHE_DIR / want_sha / filename

    def check(data: bytes, origin: str) -> None:
        got = sha256_bytes(data)
        if got != want_sha or len(data) != want_size:
            raise ImageLayerError(
                f"{record['package']}: the .deb from {origin} has sha256 {got} ({len(data)} bytes), but "
                f"{RUNTIME_PACKAGES_FILE} records sha256 {want_sha} ({want_size} bytes) -- refusing it"
            )
        want_sha1 = record.get("snapshot_sha1")
        if want_sha1 and hashlib.sha1(data).hexdigest() != want_sha1:  # noqa: S324 -- snapshot's own id
            raise ImageLayerError(f"{record['package']}: the .deb from {origin} does not match the snapshot sha1 {want_sha1}")

    if cached.is_file():
        check(cached.read_bytes(), f"the cache {cached}")
        return cached
    data = (download or _download)(record["snapshot_url"])
    check(data, record["snapshot_url"])
    cached.parent.mkdir(parents=True, exist_ok=True)
    partial = cached.with_name(cached.name + f".{uuid.uuid4().hex}.partial")
    partial.write_bytes(data)
    os.replace(partial, cached)
    return cached


def check_deb_against_record(deb: Deb, record: dict) -> None:
    for key, field_name in (("package", "Package"), ("version", "Version"), ("architecture", "Architecture")):
        if deb.control.get(field_name) != record[key]:
            raise ImageLayerError(
                f"{record['package']}: .deb control {field_name} is {deb.control.get(field_name)!r}, "
                f"record says {record[key]!r}"
            )
    if deb.copyright is None:
        raise ImageLayerError(f"{record['package']}: the .deb carries no /usr/share/doc/*/copyright")
    got = sha256_bytes(deb.copyright)
    if got != record["licence"]["copyright_sha256"]:
        raise ImageLayerError(f"{record['package']}: copyright sha256 {got} != recorded {record['licence']['copyright_sha256']}")
    if copyright_licences(deb.copyright) != record["licence"]["licence_fields"]:
        raise ImageLayerError(
            f"{record['package']}: License: fields {copyright_licences(deb.copyright)} != recorded "
            f"{record['licence']['licence_fields']}"
        )


# =================================================================================================
# Reading the runtime set out of an image
# =================================================================================================

#: `copy_out(image_path, host_parent_dir) -> bool`: copies the file or directory at `image_path` into
#: `host_parent_dir` under its own base name; False when the path does not exist in the image.
CopyOut = Callable[[str, Path], bool]


@dataclass
class RuntimeFile:
    image_path: str
    sha256: str
    mode: int
    size: int
    source: str            # "kit" or "package:<name>"
    staged: Path


@dataclass
class ExtractedRuntime:
    files: list[RuntimeFile]
    content_hash: str
    #: package files the .deb holds that the image does not (dpkg path-exclude drops docs on the slim image)
    excluded: list[str] = field(default_factory=list)


def extract_runtime(
    copy_out: CopyOut, component: str, staging: Path, *, debs: Mapping[str, Deb],
) -> ExtractedRuntime:
    """Reads `RUNTIME_SETS[component]` out of an image through `copy_out` into `staging`, then
    hashes what landed on disk. A package's files are those its `.list` names that the (already
    verified) `.deb` holds as regular files; each present one must equal the `.deb`'s bytes."""
    rset = RUNTIME_SETS[component]
    root = staging / "rootfs"
    root.mkdir(parents=True, exist_ok=True)

    found: dict[str, RuntimeFile] = {}

    def copy_path(image_path: str) -> Path:
        parent = root / image_path.lstrip("/").rsplit("/", 1)[0] if "/" in image_path.lstrip("/") else root
        parent.mkdir(parents=True, exist_ok=True)
        if not copy_out(image_path, parent):
            raise ImageLayerError(f"{component}: {image_path} is not in the image")
        return root / image_path.lstrip("/")

    def add(image_path: str, full: Path, source: str) -> None:
        st = full.stat()
        found[image_path] = RuntimeFile(image_path, sha256_path(full), st.st_mode & 0o7777, st.st_size, source, full)

    for image_path in rset.files:
        full = copy_path(image_path)
        if full.is_symlink() or not full.is_file():
            raise ImageLayerError(f"{component}: {image_path} is not a regular file in the image")
        add(image_path, full, "kit")

    for tree in rset.trees:
        base = copy_path(tree)
        if not base.is_dir() or base.is_symlink():
            raise ImageLayerError(f"{component}: {tree} is not a directory in the image")
        for dirpath, dirnames, filenames in os.walk(base, followlinks=False):
            for name in sorted(dirnames + filenames):
                entry = Path(dirpath) / name
                if entry.is_symlink() or not (entry.is_file() or entry.is_dir()):
                    raise ImageLayerError(
                        f"{component}: {'/' + entry.relative_to(root).as_posix()} is a symlink or special file "
                        f"under {tree} -- a kit's runtime Dockerfile cannot recreate it"
                    )
            for name in sorted(filenames):
                entry = Path(dirpath) / name
                add("/" + entry.relative_to(root).as_posix(), entry, "kit")

    excluded: list[str] = []
    for package in rset.packages:
        deb = debs.get(package)
        if deb is None:
            raise ImageLayerError(f"{component}: no .deb supplied for package {package}")
        info = staging / "dpkg-info"
        info.mkdir(parents=True, exist_ok=True)
        if not copy_out("/var/lib/dpkg/info", info):
            raise ImageLayerError(f"{component}: /var/lib/dpkg/info is not in the image")
        lists = sorted((info / "info").glob(f"{package}.list")) + sorted((info / "info").glob(f"{package}:*.list"))
        if len(lists) != 1:
            raise ImageLayerError(f"{component}: expected exactly one dpkg .list for {package}, found {[p.name for p in lists]}")
        listed = [l for l in lists[0].read_text(encoding="utf-8").splitlines() if l.strip()]
        deb_regular = {p for p, f in deb.files.items() if f.kind == "file"}
        for entry in listed:
            norm = entry.rstrip("/") or "/"
            if norm in ("/.", "/"):
                continue
            if norm not in deb.files:
                raise ImageLayerError(f"{component}: dpkg lists {entry} for {package} but the .deb does not hold it")
        for entry in sorted(deb_regular):
            if entry not in listed:
                raise ImageLayerError(f"{component}: the .deb holds {entry} but the image's dpkg list for {package} does not")
            parent = root / entry.lstrip("/").rsplit("/", 1)[0]
            parent.mkdir(parents=True, exist_ok=True)
            if not copy_out(entry, parent):
                excluded.append(entry)
                continue
            full = root / entry.lstrip("/")
            if full.is_symlink() or not full.is_file():
                raise ImageLayerError(f"{component}: {entry} is not a regular file in the image but the .deb ships one")
            add(entry, full, f"package:{package}")
            want = deb.files[entry]
            got = found[entry]
            if got.sha256 != want.sha256 or got.mode != want.mode:
                raise ImageLayerError(
                    f"{component}: {entry} in the image (sha256 {got.sha256}, mode {got.mode:04o}) differs from the "
                    f".deb's (sha256 {want.sha256}, mode {want.mode:04o})"
                )

    if not found:
        raise ImageLayerError(f"{component}: the runtime set is empty")
    files = sorted(found.values(), key=lambda f: f.image_path)
    return ExtractedRuntime(
        files=files,
        content_hash=runtime_content_hash({f.image_path: f.sha256 for f in files}),
        excluded=sorted(excluded),
    )


# --- docker ---------------------------------------------------------------------------------------

def _docker(*args: str, timeout: float = 300.0) -> subprocess.CompletedProcess:
    return subprocess.run(["docker", *args], capture_output=True, text=True, timeout=timeout)


@contextmanager
def image_copier(tag: str, run_id: str) -> Iterator[CopyOut]:
    """`docker create` (never started, no network), yield a `docker cp` copier, `docker rm -v`
    afterwards (the plugin image declares a VOLUME; without -v an anonymous volume would leak).
    Labelled `av.test=1` and `av.test.run_id` so a crashed run is swept by the repository's own
    label prune."""
    created = _docker(
        "create", "--network", "none", "--label", "av.test=1", "--label", f"av.test.run_id={run_id}",
        "--label", "av.kit.extract=1", tag,
    )
    if created.returncode != 0:
        raise ImageLayerError(f"`docker create {tag}` failed (rc={created.returncode}): {created.stderr.strip()}")
    cid = created.stdout.strip()

    def copy_out(image_path: str, host_parent: Path) -> bool:
        result = _docker("cp", f"{cid}:{image_path}", f"{host_parent}/")
        if result.returncode == 0:
            return True
        if "Could not find the file" in result.stderr or "No such container:path" in result.stderr:
            return False
        raise ImageLayerError(f"`docker cp {cid[:12]}:{image_path}` failed (rc={result.returncode}): {result.stderr.strip()}")

    try:
        yield copy_out
    finally:
        removed = _docker("rm", "-v", cid)
        if removed.returncode != 0:
            raise ImageLayerError(f"`docker rm -v {cid[:12]}` failed: {removed.stderr.strip()}")


# =================================================================================================
# Preparing inputs (before anything is written into a kit) and writing a component into a kit
# =================================================================================================

@dataclass
class PreparedComponent:
    component: str
    recorded_runtime_hash: str
    base: dict
    packages: list[dict]            # the committed records
    debs: dict[str, Deb]            # package -> parsed, verified .deb (in the cache)


def prepare_components(
    repo_root: Path, components, *, download: Optional[Callable[[str], bytes]] = None,
) -> dict[str, PreparedComponent]:
    """Everything that can refuse, done before the kit is touched: the Dockerfile and pin checks,
    the recorded runtime-content hash, and the `.deb` (cache or one pinned fetch) verified against
    its record. Returns per-component inputs for `collect_component`."""
    prepared: dict[str, PreparedComponent] = {}
    for component in components:
        problems = kit_dockerfile_problems(repo_root, component)
        if problems:
            raise ImageLayerError(f"{component}: the runtime Dockerfile is not as pinned:\n  " + "\n  ".join(problems))
        records = load_runtime_packages(repo_root, component)
        debs: dict[str, Deb] = {}
        for record in records:
            deb = read_deb(ensure_deb(repo_root, record, download=download))
            check_deb_against_record(deb, record)
            debs[record["package"]] = deb
        if set(debs) != set(RUNTIME_SETS[component].packages):
            raise ImageLayerError(
                f"{component}: runtime-packages.json records {sorted(debs)}, the runtime set names "
                f"{sorted(RUNTIME_SETS[component].packages)}"
            )
        prepared[component] = PreparedComponent(
            component=component,
            recorded_runtime_hash=read_recorded_runtime_hash(repo_root, component),
            base=base_pin_for(component, repo_root),
            packages=records,
            debs=debs,
        )
    return prepared


def _mode_text(mode: int) -> str:
    return f"{mode:04o}"


def base_entry(base: dict) -> dict:
    return {"id": base["id"], "item": base["item"], "reference": base["reference"], "digest": base["digest"]}


def uncollected_images(repo_root: Path = REPO_ROOT) -> dict:
    """The manifest's `images` section when `--with-images` was not requested: the recorded id, the
    recorded runtime-content hash and the pinned base are still present, `collected` is false, and
    the collected-only fields are explicit empty values (never absent)."""
    import manifest as kit_manifest  # builder-side only; image_layer is not part of the installer
    import sbom

    return {
        component: {
            **kit_manifest.read_recorded_image(component),
            "collected": False,
            "runtime_content_hash": read_recorded_runtime_hash(repo_root, component),
            "runtime_set": runtime_set_entry(component),
            "base": base_entry(base_pin_for(component, repo_root)),
            "config": None,
            "dockerfile": None,
            "files": [],
            "packages": [],
            "network_used": False,
        }
        for component in sbom.IMAGE_COMPONENTS
    }


def collected_entry(
    recorded: dict, prepared: PreparedComponent, extracted: ExtractedRuntime, component: str,
    config: Optional[dict] = None,
) -> dict:
    root = f"images/{component}"
    files = []
    for f in extracted.files:
        kit_path = f"{root}/rootfs{f.image_path}" if f.source == "kit" else None
        files.append({
            "image_path": f.image_path, "kit_path": kit_path, "sha256": f.sha256,
            "mode": _mode_text(f.mode), "size": f.size, "source": f.source,
        })
    packages = []
    for record in prepared.packages:
        packages.append({
            "name": record["package"], "version": record["version"], "architecture": record["architecture"],
            "filename": record["filename"], "kit_path": f"{root}/{record['filename']}",
            "sha256": record["sha256"], "size": record["size"], "snapshot_url": record["snapshot_url"],
            "licence_fields": record["licence"]["licence_fields"],
            "licence_kit_path": f"{root}/{record['package']}.copyright",
            "copyright_sha256": record["licence"]["copyright_sha256"],
            "dpkg_excluded_files": extracted.excluded,
        })
    return {
        **recorded,
        "collected": True,
        "runtime_content_hash": extracted.content_hash,
        "runtime_set": runtime_set_entry(component),
        "base": base_entry(prepared.base),
        # `.Config` of the recorded image (`CONFIG_KEYS`), read from the live image at kit-build time:
        # what the installer's image step compares the image it builds with. `None` only when a
        # caller (a test with a faked image) supplied none; the installer refuses such a kit.
        "config": config_subset(config) if config is not None else None,
        "dockerfile": {"kit_path": f"{root}/Dockerfile"},
        "files": files,
        "packages": packages,
        # A fact about the step, never about this run: the .deb is fetched from snapshot.debian.org
        # at kit-build time unless the cache already holds it, and the manifest must not differ.
        "network_used": bool(prepared.packages),
    }


def write_component(repo_root: Path, kit_root: Path, component: str, prepared: PreparedComponent, extracted: ExtractedRuntime) -> None:
    root = kit_root / "images" / component
    root.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(repo_root / IMAGE_SERVICE_DIRS[component] / "Dockerfile.kit", root / "Dockerfile")
    for f in extracted.files:
        if f.source != "kit":
            continue
        dest = root / "rootfs" / f.image_path.lstrip("/")
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(f.staged, dest)
        os.chmod(dest, f.mode)
        if sha256_path(dest) != f.sha256:
            raise ImageLayerError(f"{component}: {f.image_path} changed while being written into the kit")
    for record in prepared.packages:
        deb = prepared.debs[record["package"]]
        shutil.copyfile(deb.path, root / record["filename"])
        (root / f"{record['package']}.copyright").write_bytes(deb.copyright)
    # The kit's files, re-read from the kit, must still hash to the value that was checked.
    from_kit = {
        f.image_path: sha256_path(root / "rootfs" / f.image_path.lstrip("/")) if f.source == "kit" else f.sha256
        for f in extracted.files
    }
    if runtime_content_hash(from_kit) != extracted.content_hash:
        raise ImageLayerError(f"{component}: the runtime-content hash over the files written into the kit differs from the checked one")


def collect_component(
    repo_root: Path, kit_root: Path, component: str, prepared: PreparedComponent, recorded: dict,
    *, tag: str, run_id: str, copier_factory=None, config: Optional[dict] = None,
) -> dict:
    """One component: extract the runtime set from the live image, refuse unless its runtime-content
    hash equals the recorded one, write the kit's `images/<component>/`, return the manifest entry."""
    with tempfile.TemporaryDirectory(prefix="av-kit-image-layer-") as staging_dir:
        with (copier_factory or image_copier)(tag, run_id) as copy_out:
            extracted = extract_runtime(copy_out, component, Path(staging_dir), debs=prepared.debs)
        if extracted.content_hash != prepared.recorded_runtime_hash:
            raise ImageLayerError(
                f"{component}: the runtime-content hash of {tag} is {extracted.content_hash}, which does NOT match "
                f"the recorded {prepared.recorded_runtime_hash} (question 185) -- refusing to put files of a "
                f"different image into the kit under this component's name. Rebuild with the component's own "
                f"build-image.sh and update its record, or investigate the mismatch."
            )
        write_component(repo_root, kit_root, component, prepared, extracted)
    return collected_entry(recorded, prepared, extracted, component, config)


def new_run_id() -> str:
    return uuid.uuid4().hex
