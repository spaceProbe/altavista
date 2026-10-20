"""scripts/kit/image_build.py -- question 250(a), second half: the kit's HOST-SIDE image step.

# What it is

A kit carries, per container image, the image's application layer (`images/<component>/`: a runtime
Dockerfile that starts `FROM <base>@sha256:<digest>`, the application files, and for the plugin the
recorded `libssl3` `.deb`) and no base layer (`scripts/kit/image_layer.py`, `docs/kit-operator-
supplied.md` items 1 and 2). This module turns that into images, on the machine that has the
operator's base images and the `docker` CLI:

    <kit>/installer/install.sh --build-images <kit> [--component C ...] [--image-tag COMPONENT=REF ...] [--record FILE] [--moe-store auto|file|clickhouse]
    <kit>/installer/install.sh --probe-offline-build <kit> [--moe-store auto|file|clickhouse]

It ships inside every kit (`build_kit.INSTALLER_SOURCE_FILES`), imports only the standard library,
`manifest` and `docker_image_tags`, and is what `install.py` dispatches to for those two flags.

# Why it is separate from the files install (the lead's ruling)

The files install (`install.py <kit> <target>`) runs inside the enclave container on an `--internal`
network. That container never holds the Docker socket, `DOCKER_HOST`, or any other path to the
daemon: a container that controls the host's daemon can start a networked container, which would
defeat the isolation the zero-egress proof rests on. Building an image needs the daemon, so it is a
step the operator runs ON THE DOCKER HOST, with the same kit, after (or before) the files install.
`INSTALL_RECORD.installed.images` says exactly that: `built: false`, and the command to run.

# What `--build-images` does, in order (every refusal exits 2 with a typed line, nothing half done)

1. `manifest.verify_manifest(kit)` first, as the files install does; any finding refuses. Then the
   kit must be `kit_format` 5 with every image `collected` (a kit built without `--with-images`
   carries no application layer) and each entry carrying what this step needs (`runtime_set`,
   `config`, `base`, `dockerfile`, `files`, `packages`).
2. The pinned base recorded in the kit (`images.<c>.base`) must equal the pin in the installer's own
   `operator_supplied.json`, and the Dockerfile's `FROM` lines must be exactly the pinned reference
   (digest included): a kit whose Dockerfile starts from another image would otherwise be built on
   it and still pass the hash check below, which covers only the files the image adds.
3. `docker` must exist and the daemon answer. Then, for EVERY image, before any build:
   `docker image inspect <base reference>`. The base is accepted only if its `Id` equals the pinned
   digest (the containerd image store: the image id IS the index digest) or its `RepoDigests`
   contains `...@<digest>` (the classic store); measured on this host: `Id` matches. Absent, or
   present but a different digest: a typed refusal naming the expected reference and digest and
   telling the operator to `docker load` it. No build is issued without this, so no build can reach
   for a base on a registry (`--pull=false` is the second guard, not the first). (Question 250(a),
   task 3: this check is `operator_check.verify_image`, run through `operator_check.verify_all`, which
   prints one line for each of the six components of `docs/kit-operator-supplied.md` -- the bases, any
   service image the kit declares in `KIT_MANIFEST.operator_supplied_declared` (verified the same way,
   by digest), the rest listed as outside the kit or as the files install's (GMAT). The refusals keep
   their kinds: `base-image-missing`, `base-image-digest-differs`, and for a declared service image
   `operator-image-missing`, `operator-image-digest-differs`. The same list goes into the record's
   `operator_supplied`. `operator_check.py`'s module doc says how to read this output together with
   the files install's.) ClickHouse (question 260) is the one OPTIONAL operator-supplied image: not
   declared, it is looked for on the host (`operator_check.verify_optional_image`, by reference and then by
   repository) and `--moe-store auto|file|clickhouse` (default `auto`) says what that means: `auto` verifies it
   when it is the pin (`MoE store: clickhouse`), reports `not configured` when no ClickHouse image is on the
   host (the file store; the walk continues), and refuses any other ClickHouse image
   (`operator-image-digest-differs`, naming `--moe-store file`); `file` never selects it and never refuses for
   it, but records what it found; `clickhouse` requires the pin as if the kit declared it. A lookup that
   fails is `operator-image-lookup-failed` under every value, and a kit that declares ClickHouse with
   `--moe-store file` is refused (`moe-store-contradiction`) before any docker call. After the list one
   `MoE store: ...` line is printed. Nothing is pulled or loaded for it, ever.
4. The tag for each image: `--image-tag COMPONENT=REF`, default `av-kit/<component>:<first 12 hex of
   KIT_MANIFEST's sha256>`. Never the recorded `:local` tag (refused), and never a tag that already
   exists on the host: this step only ever creates a name, so it can later remove exactly the name
   it created.
5. The build context is assembled in memory (a tar stream to `docker build -`): the kit's Dockerfile,
   each recorded file at its image path under `rootfs/` with its RECORDED MODE (checked against the
   recorded sha256), the recorded `.deb`; owner 0:0, mtime 0, directories 0755.
6. `docker build --network none --pull=false --no-cache --force-rm --rm=true -t <tag> --label
   org.altavista.kit-manifest-sha256=<hash> --label org.altavista.kit-image=<component> -`.
   Measured on this host: the legacy builder runs (`docker buildx` is absent) and honours both
   flags (a `RUN` sees only `lo`; no pull event). `--no-cache` so that every `RUN` really executes
   offline now, rather than being satisfied from a layer cached by an earlier, connected build.
7. Before anything is started: `docker create --network none` (never `start`), `docker cp
   <container>:<root> -` for each runtime root (a tar stream, read in memory, nothing written to
   the host), hash the REGULAR files with question 185's algorithm (SHA-256 over sorted
   `"<image path> <sha256>\\n"` lines), `docker rm -v`. For the plugin's `libssl3` the file set is
   the regular, non-symlink files of the image's own dpkg `.list`. The hash must equal
   `images.<c>.runtime_content_hash`; every file's mode must equal the recorded one; the image's
   `.Config` (`CONFIG_KEYS`, installer labels removed) must equal the `.Config` the kit recorded
   from the image it was built from. The image id is REPORTED and not compared (question 190: it
   changes with layer timestamps).
8. Any refusal in 6 or 7 removes the tags this run created (all of them, so the host is as it was):
   by name, never by id, never `-f`, and only through the measured rule in `docker_image_tags`
   (`_survives_untag`): a tag whose image already existed before the build is untagged only if
   another reference survives, otherwise it is left and the refusal says so.
9. On success `IMAGE_BUILD_RECORD` (a JSON file the operator chooses with `--record`, default
   `./IMAGE_BUILD_RECORD`, never overwritten; NOT inside the kit, which would no longer verify) holds,
   per image: tag, built image id and whether that id existed before, the base reference and digest
   with how it was verified, the recorded and computed runtime-content hashes and `equal`, the mode
   and `.Config` checks, and the build flags; `moe_store` is the MoE store the image step selected
   and why (`operator_check.moe_store_selection`: `mode`, `store`, and what was found about ClickHouse).
   "Selected" means recorded: nothing in the installed system reads it yet.

`--probe-offline-build <kit>` proves the BUILD step is offline, not just the files install: through
the same `build_from_context` and flags it builds throwaway images from each pinned base whose
`RUN` tries the network (a raw TCP connect to 1.1.1.1:443; a name lookup of example.com) and
requires both builds to FAIL with the network-unreachable / name-resolution error. Exit 0 only then.
Should a probe build succeed the network was reachable: the image is removed and the exit is 2.

# Testing without Docker

Every docker call goes through an injectable runner `runner(args, input=None, timeout=...) ->
CompletedProcess` (bytes); `tests/test_kit_image_build.py` drives each refusal with a fake one.
"""
from __future__ import annotations

import argparse
import hashlib
import io
import json
import os
import re
import subprocess
import sys
import tarfile
import uuid
from contextlib import contextmanager
from dataclasses import dataclass, field
from pathlib import Path, PurePosixPath
from typing import Callable, Iterator, Mapping, Optional

sys.dont_write_bytecode = True  # see install.py: an installer run from a kit must not add files to it

SCRIPT_DIR = Path(__file__).resolve().parent
if str(SCRIPT_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPT_DIR))

import docker_image_tags  # noqa: E402  (path insert must precede this import)
import manifest as kit_manifest  # noqa: E402
import operator_check  # noqa: E402  (question 250(a), task 3: what the kit needs of the operator, and the printed list)

PROG = "install.sh --build-images"
RECORD_FORMAT = 1
DEFAULT_RECORD_NAME = "IMAGE_BUILD_RECORD"

#: The build flags, in one place so the real builds and the offline probe cannot differ.
BUILD_FLAGS = ("--network", "none", "--pull=false", "--no-cache", "--force-rm", "--rm=true")
MANIFEST_LABEL = "org.altavista.kit-manifest-sha256"
COMPONENT_LABEL = "org.altavista.kit-image"
#: Labels this step adds; removed before `.Config` is compared with the recorded one.
INSTALLER_LABEL_PREFIX = "org.altavista.kit-"

#: Same list as `image_layer.CONFIG_KEYS` (tests/test_kit_image_build.py asserts they are equal).
CONFIG_KEYS = (
    "Entrypoint", "Cmd", "User", "WorkingDir", "Volumes", "Labels", "Env",
    "ExposedPorts", "Healthcheck", "StopSignal", "OnBuild", "Shell",
)

#: The offline probes: (name, RUN body, substrings of which at least one must be in the failure).
PROBES = (
    ("tcp-connect", "RUN bash -c 'exec 3<>/dev/tcp/1.1.1.1/443'", ("Network is unreachable",)),
    ("dns-lookup", "RUN bash -c 'exec 3<>/dev/tcp/example.com/443'",
     ("Temporary failure in name resolution", "Name or service not known")),
)


# =================================================================================================
# Typed refusals
# =================================================================================================

class ImageBuildRefused(RuntimeError):
    """Every refusal of this step. `kind` is the stable machine-readable name."""

    kind = "refused"


class KitRefused(ImageBuildRefused):
    kind = "kit"


class DockerUnavailable(ImageBuildRefused):
    kind = "docker-unavailable"


class BaseImageMissing(ImageBuildRefused):
    kind = "base-image-missing"


class BaseImageDigestDiffers(ImageBuildRefused):
    kind = "base-image-digest-differs"


class OperatorImageMissing(ImageBuildRefused):
    """A service image the kit declared (`operator_supplied_declared`) is not on the Docker host."""

    kind = "operator-image-missing"


class OperatorImageDigestDiffers(ImageBuildRefused):
    kind = "operator-image-digest-differs"


class OperatorImageLookupFailed(ImageBuildRefused):
    """The Docker host could not be asked whether an optional operator-supplied image (ClickHouse) is on it.
    Never read as 'absent'."""

    kind = "operator-image-lookup-failed"


class MoeStoreContradiction(ImageBuildRefused):
    """`--moe-store` contradicts the kit (an unknown value, or `file` for a kit that declares ClickHouse)."""

    kind = "moe-store-contradiction"


#: `operator_check.Verification.code` of an unverified image -> the refusal this step raises for it
#: (a code with no entry here is a problem with the kit or the pins file: `KitRefused`).
_REFUSAL_BY_CODE = {
    "base-image-missing": BaseImageMissing,
    "base-image-digest-differs": BaseImageDigestDiffers,
    "image-missing": OperatorImageMissing,
    "image-digest-differs": OperatorImageDigestDiffers,
    "image-lookup-failed": OperatorImageLookupFailed,
}


class TagRefused(ImageBuildRefused):
    kind = "tag"


class BuildFailed(ImageBuildRefused):
    kind = "build-failed"


class RuntimeContentMismatch(ImageBuildRefused):
    kind = "runtime-content-mismatch"


class ConfigMismatch(ImageBuildRefused):
    kind = "config-mismatch"


class OfflineProbeFailed(ImageBuildRefused):
    kind = "offline-probe-failed"


# =================================================================================================
# The docker runner
# =================================================================================================

Runner = Callable[..., "subprocess.CompletedProcess"]


def docker_runner(args, input: Optional[bytes] = None, timeout: float = 900.0) -> "subprocess.CompletedProcess":
    """The real runner: `docker <args>` with bytes in and out."""
    try:
        return subprocess.run(["docker", *args], input=input, capture_output=True, timeout=timeout)
    except FileNotFoundError as e:
        raise DockerUnavailable(
            "the `docker` CLI was not found on PATH -- this step builds images on a Docker host; run it "
            "where Docker is installed and the operator's base images are loaded"
        ) from e
    except subprocess.TimeoutExpired as e:
        raise ImageBuildRefused(f"`docker {' '.join(map(str, args[:3]))} ...` did not finish within {timeout:.0f}s") from e


def _text(data: bytes) -> str:
    return data.decode("utf-8", "replace")


def _tail(text: str, lines: int = 30) -> str:
    rows = text.rstrip().splitlines()
    return "\n".join(rows[-lines:])


def _text_runner(docker: Runner) -> Callable[[list], str]:
    """`run(args) -> stdout text`, raising on failure: the shape `docker_image_tags` takes."""
    def run(args: list) -> str:
        r = docker(list(args), timeout=120.0)
        if r.returncode != 0:
            raise ImageBuildRefused(f"`docker {' '.join(args[:3])}` failed (rc={r.returncode}): {_text(r.stderr).strip()}")
        return _text(r.stdout)
    return run


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def runtime_content_hash(sha_by_path: Mapping[str, str]) -> str:
    """Question 185: `sha256:` + SHA-256 over the UTF-8 bytes of `"<path> <sha256>\\n"` per file,
    sorted bytewise by the whole line. (`image_layer.runtime_content_hash` is the builder's copy; the
    tests assert the two agree.)"""
    if not sha_by_path:
        raise RuntimeContentMismatch("the runtime-content set read from the image is empty")
    lines = sorted(f"{path} {sha}" for path, sha in sha_by_path.items())
    return "sha256:" + sha256_bytes("".join(line + "\n" for line in lines).encode("utf-8"))


# =================================================================================================
# The kit
# =================================================================================================

REQUIRED_ENTRY_KEYS = ("tag", "collected", "runtime_content_hash", "runtime_set", "base", "config", "dockerfile", "files", "packages")


@dataclass
class KitPlan:
    kit_dir: Path
    doc: dict
    manifest_sha256: str
    components: dict[str, dict]
    pins: dict = field(default_factory=dict)   # the installer's pins file, `components` (validated)


def load_kit(kit_dir: Path, pins_file: Optional[Path] = None) -> KitPlan:
    """Step 1 and 2: verify, then the structural and pin checks. Raises `KitRefused`."""
    kit_dir = kit_dir.resolve()
    manifest_path = kit_dir / "KIT_MANIFEST"
    if not manifest_path.is_file():
        raise KitRefused(f"{kit_dir} has no KIT_MANIFEST -- this is not a kit this step can verify, let alone build from")
    try:
        findings = kit_manifest.verify_manifest(kit_dir)
    except (OSError, json.JSONDecodeError) as e:
        raise KitRefused(f"{manifest_path} could not be read/parsed ({type(e).__name__}: {e}) -- refusing") from e
    if findings:
        lines = "\n".join(f"  - {f.kind}: {f.path} -- {f.message}" for f in findings)
        raise KitRefused(
            f"{kit_dir} does NOT verify against its own KIT_MANIFEST -- {len(findings)} finding(s), refusing "
            f"to build an image from an untrusted kit:\n{lines}"
        )
    doc = json.loads(manifest_path.read_text(encoding="utf-8"))
    if doc.get("kit_format") != kit_manifest.KIT_FORMAT:
        raise KitRefused(
            f"KIT_MANIFEST declares kit_format {doc.get('kit_format')!r}; this step understands {kit_manifest.KIT_FORMAT}"
        )
    images = doc.get("images")
    if not isinstance(images, dict) or not images:
        raise KitRefused("KIT_MANIFEST has no images section")
    if not isinstance(doc.get("operator_supplied_declared"), list):
        raise KitRefused(
            "KIT_MANIFEST has no operator_supplied_declared list -- a kit built before question 250(a), task 3; "
            "this step will not guess that it declares nothing. Rebuild the kit"
        )
    pins = _load_pins(pins_file)
    for component, entry in sorted(images.items()):
        if not entry.get("collected"):
            raise KitRefused(
                f"images.{component}.collected is false -- this kit was built without --with-images, so it carries no "
                f"application layer to build; rebuild the kit with --with-images"
            )
        missing = [k for k in REQUIRED_ENTRY_KEYS if entry.get(k) in (None, "")]
        if missing:
            raise KitRefused(
                f"images.{component} lacks {missing!r} -- the kit was not written by this installer's own builder "
                f"(scripts/kit/image_layer.py); rebuild it"
            )
        _check_pin(component, entry, pins)
        _check_dockerfile_from(kit_dir, component, entry)
    return KitPlan(kit_dir, doc, kit_manifest.sbom.sha256_file(manifest_path), dict(sorted(images.items())), pins)


def _load_pins(pins_file: Optional[Path]) -> dict:
    path = pins_file or (SCRIPT_DIR / "operator_supplied.json")
    try:
        pins = json.loads(path.read_text(encoding="utf-8"))["components"]
    except (OSError, ValueError, KeyError) as e:
        raise KitRefused(f"the installer's pins file {path} could not be read ({type(e).__name__}: {e})") from e
    problems = operator_check.pins_problems(pins)
    if problems:
        raise KitRefused(f"the installer's pins file {path} is malformed: " + "; ".join(problems))
    return pins


def _check_pin(component: str, entry: dict, pins: dict) -> None:
    found = [(pid, c) for pid, c in pins.items() if c.get("kind") == "container-base-image" and component in c.get("base_of", [])]
    if len(found) != 1:
        raise KitRefused(f"{component}: the installer's pins file names {len(found)} base images for it, expected exactly 1")
    pid, pin = found[0]
    want = {"id": pid, "item": pin["item"], "reference": pin["reference"], "digest": pin["digest"]}
    base = entry["base"]
    if {k: base.get(k) for k in want} != want:
        raise KitRefused(f"images.{component}.base is {base!r}, but the installer's own pin is {want!r}")
    if not want["reference"].endswith("@" + want["digest"]) or not re.fullmatch(r"sha256:[0-9a-f]{64}", want["digest"]):
        raise KitRefused(f"{component}: pinned reference {want['reference']!r} is not pinned by a sha256 digest")


def _dockerfile_froms(text: str) -> list[str]:
    froms: list[str] = []
    pending = ""
    for raw in text.splitlines():
        line = raw.strip()
        if line.startswith("#") or (not line and not pending):
            continue
        if line.endswith("\\"):
            pending += line[:-1].rstrip() + " "
            continue
        full, pending = (pending + line).strip(), ""
        keyword, _, args = full.partition(" ")
        if keyword.upper() == "FROM":
            froms.append([p for p in args.split() if not p.startswith("--")][0])
    return froms


def _check_dockerfile_from(kit_dir: Path, component: str, entry: dict) -> None:
    path = kit_dir / entry["dockerfile"]["kit_path"]
    froms = _dockerfile_froms(path.read_text(encoding="utf-8"))
    if froms != [entry["base"]["reference"]]:
        raise KitRefused(
            f"{component}: {path.relative_to(kit_dir)} starts FROM {froms!r}, not exactly the pinned base "
            f"[{entry['base']['reference']!r}] -- refusing to build on an image other than the pinned one"
        )


# =================================================================================================
# Tags
# =================================================================================================

_TAG_PART = re.compile(r"[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}")
_NAME_CHARS = re.compile(r"[a-z0-9][a-z0-9._/:-]*")


def default_tag(component: str, manifest_sha256: str) -> str:
    return f"av-kit/{component}:{manifest_sha256[:12]}"


def resolve_tags(plan: KitPlan, overrides: list[str]) -> dict[str, str]:
    """`--image-tag COMPONENT=REF` values over the manifest-hash default. Raises `TagRefused`."""
    tags = {c: default_tag(c, plan.manifest_sha256) for c in plan.components}
    seen: set[str] = set()
    for item in overrides:
        component, sep, ref = item.partition("=")
        if not sep or component not in plan.components:
            raise TagRefused(f"--image-tag {item!r}: expected COMPONENT=REF with COMPONENT one of {sorted(plan.components)}")
        if component in seen:
            raise TagRefused(f"--image-tag given twice for {component}")
        seen.add(component)
        name, colon, tag = ref.rpartition(":")
        if "@" in ref or not colon or "/" in tag or not _TAG_PART.fullmatch(tag) or not _NAME_CHARS.fullmatch(name) or "//" in name:
            raise TagRefused(f"--image-tag {item!r}: REF must be <repository>:<tag> (explicit tag, no digest, lower-case repository)")
        if ref == plan.components[component]["tag"]:
            raise TagRefused(
                f"--image-tag {item!r} is the kit's recorded tag {ref!r}, a name other builds on this host rely on; "
                f"choose another"
            )
        tags[component] = ref
    if len(set(tags.values())) != len(tags):
        raise TagRefused(f"two components were given the same tag: {tags!r}")
    return tags


# =================================================================================================
# Docker: availability, the base, the image table
# =================================================================================================

def check_docker(docker: Runner) -> None:
    r = docker(["version", "--format", "{{.Server.Version}}"], timeout=30.0)
    if r.returncode != 0:
        raise DockerUnavailable(f"the Docker daemon is not reachable (`docker version` rc={r.returncode}): {_text(r.stderr).strip()}")


def check_moe_mode(plan: KitPlan, moe_store: str) -> None:
    """`--moe-store` against the kit, with no docker call: an unknown value, or `file` for a kit that
    declares ClickHouse, is a `MoeStoreContradiction`."""
    problem = operator_check.moe_mode_problem(plan.doc, plan.pins, moe_store)
    if problem:
        raise MoeStoreContradiction(problem)


def verify_operator_components(
    docker: Runner, plan: KitPlan, out: Callable[[str], None], moe_store: str = operator_check.MOE_AUTO,
) -> tuple[list, dict[str, dict]]:
    """Step 3 (question 250(a), task 3). Every component of the installer's pins file, in item order,
    one printed line each (`operator_check.verify_all`): the bases of the images this run builds and any
    service image the kit declares are found with `docker image inspect <reference>` and accepted only
    if they ARE the pinned digest (`operator_check.verify_image`); the GMAT tree belongs to the files
    install and is listed as such. For EVERY image, before any build. Returns `(results, bases)`, `bases`
    being `{image component: {reference, digest, image_id, verified_by}}` for the record. A required
    image that is absent or different raises its typed refusal after its line (and those before it) were
    printed; nothing was built and nothing was pulled. An image whose pin is `optional` (ClickHouse, question 260)
    is looked for by `operator_check.verify_optional_image` under `moe_store` (`--moe-store`); the store it
    gives is `operator_check.moe_store_selection(results, moe_store)`, which `build_images` records."""
    def verify(pin_id: str, pin: dict, requirement: operator_check.Requirement) -> operator_check.Verification:
        return operator_check.verify_image(docker, pin_id, pin, requirement)

    def verify_optional(pin_id: str, pin: dict, requirement: operator_check.Requirement) -> operator_check.Verification:
        return operator_check.verify_optional_image(docker, pin_id, pin, requirement, moe_store)

    try:
        results = operator_check.verify_all(
            operator_check.STEP_IMAGES, plan.doc, plan.pins,
            verifiers={operator_check.KIND_BASE: verify, operator_check.KIND_IMAGE: verify,
                       operator_check.OPTIONAL_IMAGE: verify_optional},
            out=out, images=set(plan.components), moe_mode=moe_store,
        )
    except operator_check.ComponentRefused as e:
        raise _REFUSAL_BY_CODE.get(e.result.code, KitRefused)(e.result.reason) from e
    by_id = {r.component: r for r in results}
    bases = {}
    for component, entry in plan.components.items():
        detail = by_id[entry["base"]["id"]].detail
        bases[component] = {k: detail[k] for k in ("reference", "digest", "image_id", "verified_by")}
    return results, bases


def image_id(docker: Runner, ref: str) -> Optional[str]:
    r = docker(["image", "inspect", ref, "--format", "{{.Id}}"], timeout=60.0)
    return _text(r.stdout).strip() if r.returncode == 0 else None


# =================================================================================================
# The build context and the build
# =================================================================================================

def _tar_member(name: str, data: bytes, mode: int) -> tuple[tarfile.TarInfo, io.BytesIO]:
    info = tarfile.TarInfo(name)
    info.size, info.mode, info.mtime = len(data), mode, 0
    info.uid = info.gid = 0
    info.uname = info.gname = ""
    return info, io.BytesIO(data)


def _kit_file(kit_dir: Path, component: str, kit_path: str) -> tuple[bytes, str]:
    """`(bytes, path relative to images/<component>/)` of a file the entry names; refuses a path that
    leaves `images/<component>/`, a symlink, or a missing file."""
    pure = PurePosixPath(kit_path)
    root = PurePosixPath("images") / component
    if pure.is_absolute() or ".." in pure.parts or root not in pure.parents:
        raise KitRefused(f"{component}: kit path {kit_path!r} is not inside images/{component}/")
    full = kit_dir / kit_path
    if full.is_symlink() or not full.is_file():
        raise KitRefused(f"{component}: {kit_path} is not a regular file in the kit")
    return full.read_bytes(), str(pure.relative_to(root))


def build_context(kit_dir: Path, component: str, entry: dict) -> bytes:
    """Step 5: the tar stream `docker build -` reads. Every file is checked against the sha256 the
    entry records, and written with the entry's recorded mode."""
    dockerfile, rel = _kit_file(kit_dir, component, entry["dockerfile"]["kit_path"])
    if rel != "Dockerfile":
        raise KitRefused(f"{component}: the runtime Dockerfile must be images/{component}/Dockerfile, not {rel!r}")
    members: dict[str, tuple[bytes, int]] = {"Dockerfile": (dockerfile, 0o644)}
    for f in entry["files"]:
        if not f.get("kit_path"):
            continue  # a package file: it comes from the .deb
        data, rel = _kit_file(kit_dir, component, f["kit_path"])
        if not re.fullmatch(r"[0-7]{3,4}", f["mode"]):
            raise KitRefused(f"{component}: {f['image_path']} records mode {f['mode']!r}")
        if sha256_bytes(data) != f["sha256"]:
            raise KitRefused(f"{component}: {f['kit_path']} has sha256 {sha256_bytes(data)}, images.{component}.files records {f['sha256']}")
        if rel != "rootfs" + f["image_path"]:
            raise KitRefused(f"{component}: {f['kit_path']} is recorded for image path {f['image_path']!r}")
        members[rel] = (data, int(f["mode"], 8) & 0o7777)
    for pkg in entry["packages"]:
        data, rel = _kit_file(kit_dir, component, pkg["kit_path"])
        if sha256_bytes(data) != pkg["sha256"]:
            raise KitRefused(f"{component}: {pkg['kit_path']} has sha256 {sha256_bytes(data)}, the record says {pkg['sha256']}")
        members[rel] = (data, 0o644)
    directories = sorted({str(p) for name in members for p in PurePosixPath(name).parents if str(p) != "."})
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w", format=tarfile.GNU_FORMAT) as tf:
        for d in directories:
            info = tarfile.TarInfo(d)
            info.type, info.mode, info.mtime = tarfile.DIRTYPE, 0o755, 0
            info.uid = info.gid = 0
            info.uname = info.gname = ""
            tf.addfile(info)
        for name in sorted(members):
            data, mode = members[name]
            info, payload = _tar_member(name, data, mode)
            tf.addfile(info, payload)
    return buf.getvalue()


def docker_build_args(tag: str, labels: Mapping[str, str]) -> list[str]:
    """Step 6, the single definition of how this step invokes `docker build`."""
    args = ["build", *BUILD_FLAGS, "-t", tag]
    for key, value in sorted(labels.items()):
        args += ["--label", f"{key}={value}"]
    args.append("-")
    return args


@dataclass
class BuildResult:
    returncode: int
    output: str


def build_from_context(docker: Runner, context: bytes, tag: str, labels: Mapping[str, str]) -> BuildResult:
    """The one code path every build in this module goes through, real and probe alike."""
    r = docker(docker_build_args(tag, labels), input=context, timeout=900.0)
    return BuildResult(r.returncode, _text(r.stdout) + "\n" + _text(r.stderr))


# =================================================================================================
# Reading the built image
# =================================================================================================

@contextmanager
def extract_container(docker: Runner, tag: str, labels: Mapping[str, str]) -> Iterator[str]:
    """`docker create` (never started, no network), yield its id, `docker rm -v` afterwards (the
    plugin image declares a VOLUME; without -v an anonymous volume would leak)."""
    args = ["create", "--network", "none"]
    for key, value in sorted(labels.items()):
        args += ["--label", f"{key}={value}"]
    r = docker([*args, tag], timeout=120.0)
    if r.returncode != 0:
        raise ImageBuildRefused(f"`docker create {tag}` failed (rc={r.returncode}): {_text(r.stderr).strip()}")
    cid = _text(r.stdout).strip()
    try:
        yield cid
    finally:
        removed = docker(["rm", "-v", cid], timeout=120.0)
        if removed.returncode != 0:
            raise ImageBuildRefused(f"`docker rm -v {cid[:12]}` failed: {_text(removed.stderr).strip()}")


def _copy_tar(docker: Runner, cid: str, path: str) -> Optional[bytes]:
    """`docker cp <cid>:<path> -`: the tar stream, or `None` when the path is not in the image."""
    r = docker(["cp", f"{cid}:{path}", "-"], timeout=300.0)
    if r.returncode == 0:
        return r.stdout
    err = _text(r.stderr)
    if "Could not find the file" in err or "No such container:path" in err or "no such file" in err.lower():
        return None
    raise ImageBuildRefused(f"`docker cp {cid[:12]}:{path} -` failed (rc={r.returncode}): {err.strip()}")


@dataclass
class ImageFile:
    sha256: str
    mode: int


def _regular_members(data: bytes) -> Iterator[tuple[str, int, str]]:
    """`(name, mode, sha256)` of every regular file in a tar stream. Directories, symlinks and
    special files are skipped (question 185 hashes regular files only, as `find -type f` does); a
    hard link is refused because its content is in another member."""
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:") as tf:
        for m in tf:
            if m.islnk():
                raise RuntimeContentMismatch(f"the image holds a hard link {m.name!r}; this step does not hash those")
            if not m.isreg():
                continue
            h = hashlib.sha256()
            f = tf.extractfile(m)
            for chunk in iter(lambda: f.read(1 << 20), b""):
                h.update(chunk)
            yield m.name, m.mode & 0o7777, h.hexdigest()


def _normal_member_name(name: str) -> tuple[str, ...]:
    parts = PurePosixPath(name).parts
    parts = tuple(p for p in parts if p != ".")
    if not parts or ".." in parts or parts[0] == "/":
        raise RuntimeContentMismatch(f"the image's tar stream names {name!r}")
    return parts


def _read_file_root(docker: Runner, cid: str, path: str, found: dict[str, ImageFile]) -> None:
    data = _copy_tar(docker, cid, path)
    if data is None:
        raise RuntimeContentMismatch(f"{path} is not in the built image")
    members = list(_regular_members(data))
    if len(members) != 1 or _normal_member_name(members[0][0]) != (PurePosixPath(path).name,):
        raise RuntimeContentMismatch(f"{path} is not a single regular file in the built image")
    found[path] = ImageFile(members[0][2], members[0][1])


def _read_tree_root(docker: Runner, cid: str, path: str, found: dict[str, ImageFile]) -> None:
    data = _copy_tar(docker, cid, path)
    if data is None:
        raise RuntimeContentMismatch(f"{path} is not in the built image")
    top, parent = PurePosixPath(path).name, PurePosixPath(path).parent
    for name, mode, sha in _regular_members(data):
        parts = _normal_member_name(name)
        if parts[0] != top:
            raise RuntimeContentMismatch(f"{path}: the tar stream holds {name!r}, outside {top!r}")
        found[str(parent.joinpath(*parts))] = ImageFile(sha, mode)


def _read_package(docker: Runner, cid: str, package: str, arch: Optional[str], found: dict[str, ImageFile]) -> None:
    """The regular (non-symlink) files of `package`'s dpkg `.list` in the image: the same set the
    shell implementation takes with `dpkg -L | [ -f ] && [ ! -L ]`. Directories are recognised as
    the entries that are a prefix of another entry (dpkg lists every parent directory) and are never
    copied, because `docker cp` of one would copy a whole tree."""
    candidates = [f"/var/lib/dpkg/info/{package}.list"] + ([f"/var/lib/dpkg/info/{package}:{arch}.list"] if arch else [])
    lists = [(p, d) for p in candidates if (d := _copy_tar(docker, cid, p)) is not None]
    if len(lists) != 1:
        raise RuntimeContentMismatch(f"expected exactly one dpkg list for {package} in the image, found {[p for p, _ in lists]}")
    members = None
    with tarfile.open(fileobj=io.BytesIO(lists[0][1]), mode="r:") as tf:
        members = [tf.extractfile(m).read().decode("utf-8") for m in tf if m.isreg()]
    if len(members) != 1:
        raise RuntimeContentMismatch(f"{lists[0][0]} is not a single regular file")
    entries = sorted({e.rstrip("/") for e in members[0].splitlines() if e.strip() and e.strip() not in ("/.", "/")})
    internal = {e for e in entries if any(o != e and o.startswith(e + "/") for o in entries)}
    for entry in entries:
        if entry in internal:
            continue
        data = _copy_tar(docker, cid, entry)
        if data is None:
            continue  # listed by dpkg, not on the slim image (its path-exclude drops docs)
        regular = list(_regular_members(data))
        if len(regular) == 1 and _normal_member_name(regular[0][0]) == (PurePosixPath(entry).name,):
            found[entry] = ImageFile(regular[0][2], regular[0][1])


def read_runtime_set(docker: Runner, cid: str, runtime_set: dict, arch_by_package: Mapping[str, str]) -> dict[str, ImageFile]:
    """Step 7: the image's runtime-content set, read through `docker cp` tar streams."""
    found: dict[str, ImageFile] = {}
    for path in runtime_set["files"]:
        _read_file_root(docker, cid, path, found)
    for path in runtime_set["trees"]:
        _read_tree_root(docker, cid, path, found)
    for package in runtime_set["packages"]:
        _read_package(docker, cid, package, arch_by_package.get(package), found)
    return found


def _normalise_config_value(value):
    return None if value in (None, {}, [], "") else value


def config_of(docker: Runner, ref: str) -> dict:
    r = docker(["image", "inspect", ref, "--format", "{{json .Config}}"], timeout=60.0)
    if r.returncode != 0:
        raise ImageBuildRefused(f"`docker image inspect {ref}` failed (rc={r.returncode}): {_text(r.stderr).strip()}")
    config = json.loads(_text(r.stdout)) or {}
    out = {k: _normalise_config_value(config.get(k)) for k in CONFIG_KEYS}
    if out["Labels"]:
        out["Labels"] = _normalise_config_value({k: v for k, v in out["Labels"].items() if not k.startswith(INSTALLER_LABEL_PREFIX)})
    return out


def check_built_image(
    docker: Runner, tag: str, component: str, entry: dict, container_labels: Mapping[str, str],
) -> dict:
    """Step 7: hash, modes, `.Config`. Returns the evidence for the record; raises
    `RuntimeContentMismatch` / `ConfigMismatch`."""
    arch_by_package = {p["name"]: p["architecture"] for p in entry["packages"]}
    with extract_container(docker, tag, container_labels) as cid:
        found = read_runtime_set(docker, cid, entry["runtime_set"], arch_by_package)
    computed = runtime_content_hash({p: f.sha256 for p, f in found.items()})
    recorded = entry["runtime_content_hash"]
    recorded_files = {f["image_path"]: f for f in entry["files"]}
    if computed != recorded:
        missing = sorted(set(recorded_files) - set(found))
        extra = sorted(set(found) - set(recorded_files))
        differ = sorted(p for p in set(found) & set(recorded_files) if found[p].sha256 != recorded_files[p]["sha256"])
        from_records = runtime_content_hash({p: f["sha256"] for p, f in recorded_files.items()})
        inconsistent = (
            "" if from_records == recorded else
            f"; the kit is internally inconsistent: its own per-file records (images.{component}.files) hash to "
            f"{from_records}, not to the recorded {recorded} either"
        )
        raise RuntimeContentMismatch(
            f"{component}: the runtime-content hash of the image built as {tag} is {computed}, which does NOT equal the "
            f"hash {recorded} the kit records (question 185) -- {len(found)} file(s) read; missing from the image: "
            f"{missing[:10]}; not recorded by the kit: {extra[:10]}; different content: {differ[:10]}{inconsistent}"
        )
    wrong_modes = {p: (f"{found[p].mode:04o}", recorded_files[p]["mode"]) for p in found if f"{found[p].mode:04o}" != recorded_files[p]["mode"]}
    if wrong_modes:
        raise RuntimeContentMismatch(f"{component}: the image built as {tag} holds file(s) with other modes than recorded (built, recorded): {dict(sorted(wrong_modes.items())[:10])}")
    built_config = config_of(docker, tag)
    recorded_config = {k: _normalise_config_value((entry["config"] or {}).get(k)) for k in CONFIG_KEYS}
    differing = {k: {"recorded": recorded_config[k], "built": built_config[k]} for k in CONFIG_KEYS if recorded_config[k] != built_config[k]}
    if differing:
        raise ConfigMismatch(f"{component}: the image built as {tag} has a different .Config than the image the kit was built from: {json.dumps(differing, sort_keys=True)}")
    return {
        "runtime_content_hash_recorded": recorded, "runtime_content_hash_computed": computed, "equal": True,
        "files_read": len(found), "modes_equal": True, "config_compared": list(CONFIG_KEYS), "config_equal": True,
    }


# =================================================================================================
# Removing what this step created
# =================================================================================================

def remove_created_tags(docker: Runner, created: list[dict], before_table: dict) -> list[str]:
    """Step 8. `created`: `[{component, tag, image_id}]`. Returns problems (empty when every tag is gone)."""
    problems = []
    for item in created:
        tag, iid = item["tag"], item["image_id"]
        if iid in before_table and not docker_image_tags._survives_untag(before_table[iid], {tag}):
            problems.append(
                f"{tag}: NOT removed -- image {iid} existed before this run with references {before_table[iid]!r}, and "
                f"`docker rmi` of this name would delete it (containerd store). The name is left in place."
            )
            continue
        r = docker(["rmi", tag], timeout=120.0)
        if r.returncode != 0:
            problems.append(f"{tag}: `docker rmi {tag}` failed (rc={r.returncode}): {_text(r.stderr).strip()}")
    return problems


# =================================================================================================
# The whole step
# =================================================================================================

def build_images(
    docker: Runner, kit_dir: Path, *, image_tags: Optional[list[str]] = None, record_path: Optional[Path] = None,
    container_labels: Optional[Mapping[str, str]] = None, out: Callable[[str], None] = print, pins_file: Optional[Path] = None,
    components: Optional[list[str]] = None, moe_store: str = operator_check.MOE_AUTO,
) -> dict:
    """Steps 1-9. Returns the record (also written to `record_path`). `components` restricts the
    build to some of the kit's images (every image is still verified as part of the kit). `moe_store` is
    `--moe-store` (module doc, step 3): checked against the kit before any docker call."""
    labels = dict(container_labels or {})
    plan = load_kit(kit_dir, pins_file)
    check_moe_mode(plan, moe_store)
    if components:
        unknown = sorted(set(components) - set(plan.components))
        if unknown:
            raise KitRefused(f"--component {unknown!r}: the kit's images are {sorted(plan.components)}")
        plan.components = {c: e for c, e in plan.components.items() if c in components}
    tags = resolve_tags(plan, image_tags or [])
    record_path = (record_path or Path.cwd() / DEFAULT_RECORD_NAME).resolve()
    if record_path.exists():
        raise ImageBuildRefused(f"{record_path} already exists; refusing to overwrite a record -- choose --record elsewhere")
    check_docker(docker)
    operator_results, bases = verify_operator_components(docker, plan, out, moe_store)
    moe_selection = operator_check.moe_store_selection(operator_results, moe_store)
    out(operator_check.render_moe_store(moe_selection))
    run_text = _text_runner(docker)
    before_table = docker_image_tags.docker_image_table(run_text)
    existing = docker_image_tags._tags_of(before_table)
    for component, tag in tags.items():
        if tag in existing:
            raise TagRefused(f"{tag} already exists on this host (image {existing[tag]}); this step only creates names -- remove it or pass --image-tag {component}=<other>")
    contexts = {c: build_context(plan.kit_dir, c, e) for c, e in plan.components.items()}
    record_images: dict[str, dict] = {}
    created: list[dict] = []
    try:
        for component, entry in plan.components.items():
            tag = tags[component]
            build_labels = {MANIFEST_LABEL: plan.manifest_sha256, COMPONENT_LABEL: component}
            out(f"building {tag} from images/{component} ({len(contexts[component])} byte context; docker {' '.join(BUILD_FLAGS)})")
            result = build_from_context(docker, contexts[component], tag, build_labels)
            if result.returncode != 0:
                raise BuildFailed(f"{component}: `docker build` failed (rc={result.returncode}) -- last output:\n{_tail(result.output)}")
            built_id = image_id(docker, tag)
            if built_id is None:
                raise BuildFailed(f"{component}: the build reported success but {tag} does not exist")
            created.append({"component": component, "tag": tag, "image_id": built_id})
            evidence = check_built_image(docker, tag, component, entry, labels)
            out(f"{component}: image {built_id} -- runtime-content hash {evidence['runtime_content_hash_computed']} equals the recorded one "
                f"({evidence['files_read']} files, modes and .Config equal)")
            record_images[component] = {
                "tag": tag, "image_id": built_id, "image_id_existed_before": built_id in before_table,
                "base": bases[component], **evidence,
                "build": {"command": "docker build", "flags": list(BUILD_FLAGS), "context": "tar stream on stdin",
                          "labels": build_labels, "builder": "legacy (docker build)"},
            }
    except BaseException:
        problems = remove_created_tags(docker, created, before_table)
        if problems:
            out("cleanup problems:\n  " + "\n  ".join(problems))
        raise
    record = {
        "record_format": RECORD_FORMAT, "step": PROG, "kit_manifest_sha256": plan.manifest_sha256,
        "kit_format": plan.doc["kit_format"], "kit_git_commit": plan.doc["git_commit"], "images": record_images,
        "operator_supplied": {
            "step": "image step", "declared": list(plan.doc["operator_supplied_declared"]),
            "components": [r.to_record() for r in operator_results],
            "note": "a GMAT tree is verified by the files install (INSTALL_RECORD); this step reads no tree and does not read that record",
        },
        "moe_store": moe_selection,
    }
    partial = record_path.with_name(record_path.name + f".{uuid.uuid4().hex}.partial")
    partial.write_text(json.dumps(record, indent=2, sort_keys=True, ensure_ascii=False) + "\n", encoding="utf-8")
    os.replace(partial, record_path)
    out(f"wrote {record_path}")
    return record


def probe_offline_build(
    docker: Runner, kit_dir: Path, *, run_id: Optional[str] = None, out: Callable[[str], None] = print,
    pins_file: Optional[Path] = None, moe_store: str = operator_check.MOE_AUTO,
) -> list[dict]:
    """`--probe-offline-build`: see this module's doc. Returns one result per (probe, base). Prints the same
    operator-supplied list (`--moe-store` applies to it) and writes no record."""
    plan = load_kit(kit_dir, pins_file)
    check_moe_mode(plan, moe_store)
    check_docker(docker)
    run_id = run_id or uuid.uuid4().hex[:12]
    _results, verified = verify_operator_components(docker, plan, out, moe_store)
    bases = {b["reference"]: b for b in verified.values()}
    run_text = _text_runner(docker)
    results = []
    for reference in sorted(bases):
        for name, body, markers in PROBES:
            tag = f"av-kit-probe/offline-{name}:{run_id}"
            before = docker_image_tags.docker_image_table(run_text)
            context = io.BytesIO()
            with tarfile.open(fileobj=context, mode="w") as tf:
                info, payload = _tar_member("Dockerfile", f"FROM {reference}\n{body}\n".encode("utf-8"), 0o644)
                tf.addfile(info, payload)
            result = build_from_context(docker, context.getvalue(), tag, {MANIFEST_LABEL: plan.manifest_sha256, COMPONENT_LABEL: f"probe-{name}"})
            if result.returncode == 0:
                built = image_id(docker, tag)
                if built:
                    remove_created_tags(docker, [{"component": f"probe-{name}", "tag": tag, "image_id": built}], before)
                raise OfflineProbeFailed(
                    f"the {name} probe build on {reference} SUCCEEDED under `docker {' '.join(BUILD_FLAGS)}` -- the build step "
                    f"can reach the network; the probe image was removed. Output tail:\n{_tail(result.output)}"
                )
            matched = [m for m in markers if m in result.output]
            if not matched:
                raise OfflineProbeFailed(
                    f"the {name} probe build on {reference} failed (rc={result.returncode}) but not with the network error "
                    f"(expected one of {list(markers)}):\n{_tail(result.output)}"
                )
            line = next(l for l in result.output.splitlines() if matched[0] in l)
            results.append({"probe": name, "base": reference, "build_rc": result.returncode, "error": matched[0], "line": line.strip(), "flags": list(BUILD_FLAGS)})
            out(f"probe {name} on {reference}: build failed as required (rc={result.returncode}): {line.strip()}")
    return results


# =================================================================================================
# CLI (install.py dispatches here for --build-images / --probe-offline-build)
# =================================================================================================

def _label_pairs(values: list[str]) -> dict[str, str]:
    out = {}
    for v in values:
        key, sep, val = v.partition("=")
        if not sep or not key:
            raise ImageBuildRefused(f"--extract-container-label {v!r}: expected KEY=VALUE")
        out[key] = val
    return out


def main(argv: list[str], *, docker: Runner = docker_runner) -> int:
    mode, rest = argv[0], argv[1:]
    p = argparse.ArgumentParser(prog=f"install.sh {mode}", description=__doc__.split("# What `--build-images` does")[0])
    p.add_argument("kit_dir", type=Path, help="an existing kit directory (KIT_MANIFEST + content)")
    p.add_argument(
        "--moe-store", choices=operator_check.MOE_MODES, default=operator_check.MOE_AUTO,
        help="the MoE (sweep study) store (question 260). auto (default): the ClickHouse store when the pinned "
             "ClickHouse image is on this Docker host, the file store when no ClickHouse image is; any other "
             "ClickHouse image is refused. file: never ClickHouse, whatever the host holds (recorded as found). "
             "clickhouse: the pinned image is required, as if the kit declared it. Recorded in "
             "IMAGE_BUILD_RECORD.moe_store.mode",
    )
    if mode == "--build-images":
        p.add_argument("--image-tag", action="append", default=[], metavar="COMPONENT=REF", help="tag for a component's image (default av-kit/<component>:<manifest hash>)")
        p.add_argument("--component", action="append", default=[], metavar="COMPONENT", help="build only this image (repeatable; default: every image in the kit)")
        p.add_argument("--record", type=Path, default=None, help=f"where to write the record (default ./{DEFAULT_RECORD_NAME}; never overwritten)")
        p.add_argument("--extract-container-label", action="append", default=[], metavar="KEY=VALUE", help="a label for the throwaway containers used to read the built images")
    args = p.parse_args(rest)
    try:
        if mode == "--build-images":
            build_images(
                docker, args.kit_dir, image_tags=args.image_tag, record_path=args.record, components=args.component,
                container_labels=_label_pairs(args.extract_container_label), moe_store=args.moe_store,
            )
        else:
            probe_offline_build(docker, args.kit_dir, moe_store=args.moe_store)
    except ImageBuildRefused as e:
        print(f"install.sh {mode}: REFUSED [{e.kind}] -- {e}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
