"""scripts/kit/build_kit.py -- D3 (docs/p5-plan.md, P5 track): the kit builder itself. Assembles
a kit into `--out <dir>` and writes its `KIT_MANIFEST` (`scripts/kit/manifest.py`, which owns the
manifest FORMAT and its verifier -- see that module's own top doc for why the two are split, and
for what changed between round 1's `kit_format` 1 and round 2's `kit_format` 2).

Decision I (repeated here because it governs every function below, not just the manifest
format): this module never invokes a fresh `cargo build`/`docker build` PER KIT. It calls
`deploy/secdeploy/merge.py`'s own `merge()` over already-committed TOML, copies already-committed
SBOM/digest/run files byte-for-byte, and -- only when the corresponding flag is passed --
downloads/vendors/cross-builds exactly once, reusing the result across repeated kit builds at the
same source state (`collect_binaries`'s own per-source-state cache is the one place this matters:
a fresh cross-build genuinely differs from a previous one, measured, so the SAME already-built
bytes are copied into every kit built from a given source state rather than re-linked per kit --
see `manifest.py`'s own top doc, "Decision I", for the full argument, and `binary_cache_key` for
why "source state" is the commit AND the working tree on top of it, never the commit alone).

# What this half assembles (round 2)

IN, always: the merged suite/site files, the ten committed SBOMs + `SHA256SUMS`, the two
`IMAGE_DIGEST.md` records, the recorded kernel runs (`tests/fixtures/*.runproducts.bin`), the
Decision-K pack descriptors (`data/time` by default), the git commit/dirty state, and
`KIT_MANIFEST` itself. **Task 3c added, round 3 (question 217(b)) removed**: real BYTES for the
`web` and `profiles` packs (the viewer's own static assets and profile/policy store), copied
UNCONDITIONALLY, no flag. Round 3's packaging change (`pyproject.toml`/`setup.py`) now ships both
INSIDE the `altavista` wheel itself, so the kit does not need to carry either separately any more
-- see `manifest.py`'s own top doc, "P5 track round 3", for the full reasoning and the resulting
`kit_format` bump (2 -> 3).

GATED, opt-in, off by default:
- `--with-images` -- `docker save` of the two recorded images.
- `--with-pack <name>` (repeatable) -- an additional pack's DESCRIPTOR (content hash only).
- `--copy-pack <name>` (repeatable) -- round 2: that pack's real BYTES, copied into the kit
  (implies the descriptor too; `--max-pack-bytes` still gates it). Round 2 also copied `web`/
  `profiles` this same way, unconditionally, never gated behind this flag; round 3 removed both
  packs outright (see above) -- the `altavista` wheel carries them now.
- `--with-vendor` -- round 2: `cargo vendor --offline` (falling back to the network exactly once
  if genuinely necessary, question 154's one exception) into `<kit>/vendor/`.
- `--with-wheels` -- round 2: the viewer's real runtime dependency wheels, pinned to this
  worktree's own installed versions, for linux/aarch64/cp313 (task 3b's own proof platform) --
  the ONE step that always uses the network (question 154), never at test time.
- `--with-binaries` -- round 2: cross-built Linux service binaries (`av-ingest-server`,
  `av-command`) the zero-egress install proof will start inside a container.

OUT, as `manifest.build_gaps` names explicitly (some conditionally, per the flags above):
the seccert trust root (always -- see `manifest.py`'s own `_SECCERT_ROOT_REASON`), standing the
installed tree up as a supervised, persistent deployment (see `manifest.py`'s own
`_INSTALL_PATH_REASON`, rewritten by task 3c now that task 3b's install.sh/install.py exist), and
secdeploy's own `deploy/` assets.

# Question 199 (no test mutates the process environment)

Nothing below WRITES `os.environ` -- every value this module needs (repo root, site path, pack
names, flags) arrives as a function parameter or a CLI argument, and every `cargo`/`pip`/`docker`
subprocess call inherits this PROCESS's own environment as-is (set by whoever invoked
`scripts/kit/build.sh`/`build_kit.py`, e.g. this task's own required `PATH`/`GMAT_ROOT`/
`CFS_MIRROR_DIR` exports). The ONE exception reads, never mutates, `os.environ`:
`collect_wheels`'s `pip wheel .` call passes `env={**os.environ, "SOURCE_DATE_EPOCH": ...}` (a
NEW dict, copied from the process's own environment, handed only to that one subprocess) so the
wheel it builds is byte-reproducible -- see `_altavista_wheel_source_date_epoch`'s own doc for
why this was necessary, measured, not assumed.
"""
from __future__ import annotations

import argparse
import hashlib
import importlib.metadata as importlib_metadata
import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tempfile
import tomllib
import uuid
from pathlib import Path

from packaging.markers import default_environment
from packaging.requirements import Requirement

# scripts/kit is this file's own directory -- Python puts it on sys.path[0] automatically when
# this file is run directly (`python scripts/kit/build_kit.py`), which is what makes the two
# plain `import manifest` / `import sbom` below work without any path manipulation; tests that
# import this module instead do the identical `sys.path.insert(0, KIT_DIR)` `test_sbom.py`
# already establishes as this workspace's convention.
import manifest as kit_manifest  # noqa: E402
import sbom  # noqa: E402

REPO_ROOT = Path(__file__).resolve().parents[2]

DEFAULT_SITE = Path("deploy") / "secdeploy" / "secsite.altavista-eval.toml"
MERGE_FRAGMENT = Path("deploy") / "secdeploy" / "suite.altavista.toml"
SBOM_SOURCE_DIR = Path("docs") / "compliance" / "sbom"
RUN_FIXTURES_DIR = Path("tests") / "fixtures"

#: The two components' own committed digest records, repo-relative -- copied byte-for-byte into
#: the kit at the identical relative path (`services/<name>/IMAGE_DIGEST.md`), never regenerated.
IMAGE_DIGEST_SOURCES: dict[str, Path] = {
    "edge-plugin-image": Path("services") / "edge-plugin" / "IMAGE_DIGEST.md",
    "cfs-image": Path("services") / "cfs" / "IMAGE_DIGEST.md",
}


# =================================================================================================
# Assembly steps -- each copies/derives from an already-committed source; none builds anything.
# =================================================================================================

def _load_merge_module(repo_root: Path):
    """`deploy/secdeploy/merge.py` (D1) is loaded directly from its file path rather than via a
    `sys.path` insertion for the whole process -- it is not part of `scripts/kit`'s own package,
    and this is the only place this module needs it."""
    spec = importlib.util.spec_from_file_location(
        "secdeploy_merge", repo_root / "deploy" / "secdeploy" / "merge.py"
    )
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def assemble_suite_and_site(repo_root: Path, kit_root: Path, site_path: Path) -> None:
    """Writes `<kit_root>/suite.merged.toml` and `<kit_root>/secsite.merged.toml` by calling
    `deploy/secdeploy/merge.py::merge` directly. `merge()` ALSO writes a `deploy` symlink
    alongside those two files, pointing at an ABSOLUTE path into the BASE secdeploy manifest's
    own `deploy/` directory (the user's own secdeploy checkout) -- so it runs into a throwaway
    staging directory (auto-removed on exit, whether or not `merge()` raises) and only the two
    TOML files it produces are copied out; the symlink is discarded with the rest of the staging
    directory. The `deploy/` assets themselves remain a declared gap
    (`manifest._SECDEPLOY_DEPLOY_ASSETS_REASON`), not silently dropped."""
    merge_module = _load_merge_module(repo_root)
    with tempfile.TemporaryDirectory(prefix="av-kit-merge-staging-") as staging:
        staging_path = Path(staging)
        merge_module.merge(
            base=merge_module.DEFAULT_BASE,
            fragment=repo_root / MERGE_FRAGMENT,
            site=site_path,
            out=staging_path,
        )
        shutil.copy2(staging_path / "suite.merged.toml", kit_root / "suite.merged.toml")
        shutil.copy2(staging_path / "secsite.merged.toml", kit_root / "secsite.merged.toml")


def assemble_sboms(repo_root: Path, kit_root: Path) -> dict:
    """Copies every committed `docs/compliance/sbom/*.cdx.json` plus `SHA256SUMS` into
    `<kit_root>/sbom/`, byte-for-byte (`shutil.copy2`, never regenerated -- `scripts/kit/sbom.py`
    is never invoked here). Returns the manifest's own `sboms` dict (component -> SHA-256),
    parsed straight out of the just-copied `SHA256SUMS`."""
    src_dir = repo_root / SBOM_SOURCE_DIR
    dest_dir = kit_root / "sbom"
    dest_dir.mkdir(parents=True, exist_ok=True)

    for name in sbom.all_component_names():
        shutil.copy2(src_dir / f"{name}.cdx.json", dest_dir / f"{name}.cdx.json")
    shutil.copy2(src_dir / "SHA256SUMS", dest_dir / "SHA256SUMS")

    sboms: dict[str, str] = {}
    for line in (dest_dir / "SHA256SUMS").read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        digest, _, rel = line.partition("  ")
        component = Path(rel).stem.removesuffix(".cdx")
        sboms[component] = digest
    return sboms


def assemble_image_digest_docs(repo_root: Path, kit_root: Path) -> None:
    """Copies both components' own `IMAGE_DIGEST.md` into the kit at their identical
    `services/<name>/IMAGE_DIGEST.md` relative path, byte-for-byte."""
    for rel in IMAGE_DIGEST_SOURCES.values():
        dest = kit_root / rel
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(repo_root / rel, dest)


# =================================================================================================
# Round 2, item 5: the recorded kernel run (tests/fixtures/*.runproducts.bin)
# =================================================================================================

def _read_varint(buf: bytes, pos: int) -> tuple[int, int]:
    result = 0
    shift = 0
    while True:
        b = buf[pos]
        pos += 1
        result |= (b & 0x7F) << shift
        if not (b & 0x80):
            return result, pos
        shift += 7


def _iter_protobuf_top_level_fields(buf: bytes):
    """A minimal, schema-free protobuf wire-format walk (varint tag/length parsing only, per the
    protobuf encoding spec) -- yields `(field_num, wire_type, value)` for every top-level field
    in `buf`. `value` is an `int` for wire type 0, `bytes` for wire types 1/2/5 (raw 8/length-
    delimited/4 bytes respectively). Deliberately NOT a real protobuf decode (no `.proto` compile,
    no `prost`/`protobuf` schema dependency needed) -- just enough to read one specific field out
    of a `RunProducts` message cheaply, per this task's own instruction ("if you can read it
    cheaply")."""
    pos = 0
    n = len(buf)
    while pos < n:
        tag, pos = _read_varint(buf, pos)
        field_num = tag >> 3
        wire_type = tag & 0x7
        if wire_type == 0:
            value, pos = _read_varint(buf, pos)
        elif wire_type == 1:
            value = buf[pos:pos + 8]
            pos += 8
        elif wire_type == 2:
            length, pos = _read_varint(buf, pos)
            value = buf[pos:pos + length]
            pos += length
        elif wire_type == 5:
            value = buf[pos:pos + 4]
            pos += 4
        else:
            raise ValueError(f"unsupported protobuf wire type {wire_type} for field {field_num}")
        yield field_num, wire_type, value


def _last_length_delimited_field(buf: bytes, field_num: int) -> "bytes | None":
    """proto3 "last one wins" for a non-repeated field -- the last length-delimited (wire type 2)
    occurrence of `field_num` at the TOP LEVEL of `buf`, or `None` if it never appears."""
    result = None
    for fn, wt, value in _iter_protobuf_top_level_fields(buf):
        if fn == field_num and wt == 2:
            result = value
    return result


def read_run_provenance(path: Path) -> "dict | None":
    """`proto/altavista/v1/run.proto`'s `RunProducts.provenance` is field 5 (an embedded
    `Provenance` message); `proto/altavista/v1/core.proto`'s `Provenance.config_hash` is field 4,
    `.data_pack_hash` field 5 -- both plain strings. Reads those two fields out of the committed
    `.runproducts.bin` at `path` using only `_iter_protobuf_top_level_fields` (stdlib, no `av_cdm`/
    `prost` build needed -- cheap by construction). Verified against the four real committed
    fixtures while building this task: every one decodes to a well-formed 64-hex-character SHA-256
    string for `config_hash` (see this task's own report). Returns `None` (never raises) if the
    bytes cannot be walked as protobuf at all, or carry no `provenance` field -- callers must
    still carry the raw bytes regardless and record that this field could not be read, never
    invent one (this task's own instruction)."""
    try:
        buf = path.read_bytes()
        provenance_bytes = _last_length_delimited_field(buf, 5)
        if provenance_bytes is None:
            return None
        config_hash = _last_length_delimited_field(provenance_bytes, 4)
        data_pack_hash = _last_length_delimited_field(provenance_bytes, 5)
        result: dict[str, str] = {}
        if config_hash is not None:
            result["config_hash"] = config_hash.decode("utf-8")
        if data_pack_hash is not None:
            result["data_pack_hash"] = data_pack_hash.decode("utf-8")
        return result or None
    except Exception:
        return None


def assemble_runs(repo_root: Path, kit_root: Path) -> dict:
    """Copies every committed `tests/fixtures/*.runproducts.bin` into `<kit_root>/runs/`, byte-
    for-byte, unconditionally (small: four files, ~4.2 MB total measured on this tree today --
    NOTE, honestly: the round-2 brief describes "five files, ~4.4 MB total"; only four exist
    anywhere in this tree (`demo_attitude_control`, `demo_command_trail`, `demo_measurements`,
    `demo_two_instance`), ~4.2 MB measured -- carried as they actually are, not padded to match a
    number that does not match the tree). Returns the manifest's own `runs` dict, keyed by each
    fixture's own stem, naming the DRM/provenance hash `read_run_provenance` could cheaply read
    (or noting plainly that it could not, per file)."""
    src_dir = repo_root / RUN_FIXTURES_DIR
    dest_dir = kit_root / "runs"
    dest_dir.mkdir(parents=True, exist_ok=True)
    runs: dict[str, dict] = {}
    for src in sorted(src_dir.glob("*.runproducts.bin")):
        dest = dest_dir / src.name
        shutil.copy2(src, dest)
        stem = src.name[: -len(".runproducts.bin")]
        provenance = read_run_provenance(src)
        runs[stem] = {
            "config_hash": (provenance or {}).get("config_hash"),
            "data_pack_hash": (provenance or {}).get("data_pack_hash"),
            "decoded": provenance is not None,
        }
    return runs


# =================================================================================================
# The gated `--with-images` step (Decision J): docker save, only after a digest comparison,
# only under the host-wide lock.
# =================================================================================================

def _docker_available() -> bool:
    try:
        result = subprocess.run(["docker", "info"], capture_output=True, timeout=30)
    except (FileNotFoundError, OSError):
        return False
    return result.returncode == 0


def _docker_image_id(tag: str) -> str:
    result = subprocess.run(
        ["docker", "image", "inspect", tag, "--format", "{{.Id}}"],
        capture_output=True, text=True, timeout=30,
    )
    if result.returncode != 0:
        raise RuntimeError(
            f"`docker image inspect {tag}` failed (rc={result.returncode}): {result.stderr.strip()}"
        )
    return result.stdout.strip()


def collect_images(repo_root: Path, kit_root: Path) -> dict:
    """The gated `--with-images` step. For each of `sbom.IMAGE_COMPONENTS`: read the recorded
    digest (`manifest.read_recorded_image`), compare it to the LIVE `docker image inspect` id
    BEFORE ever running `docker save` (question 212's own ordering), and only then save a tarball
    into `<kit_root>/images/<component>.tar`.

    Held under `altavista.docker_test_lock.lock_docker_tests()` for the whole step. A digest
    mismatch is a hard `RuntimeError`, never a silent save of the wrong bits under the right
    name."""
    from altavista.docker_test_lock import lock_docker_tests

    if not _docker_available():
        raise RuntimeError(
            "--with-images was requested but `docker info` failed -- this step only inspects "
            "and saves an already-built image, it never builds one (Decision I). Make Docker "
            "available and re-run, or omit --with-images to leave the two images recorded-but-"
            "not-collected in KIT_MANIFEST."
        )

    images_dir = kit_root / "images"
    images_dir.mkdir(parents=True, exist_ok=True)

    result: dict[str, dict] = {}
    with lock_docker_tests():
        for component in sbom.IMAGE_COMPONENTS:
            recorded = kit_manifest.read_recorded_image(component)
            tag = recorded["tag"]
            built_digest = _docker_image_id(tag)
            if not built_digest.startswith("sha256:"):
                raise RuntimeError(
                    f"{component}: `docker image inspect {tag}` returned an id in an unexpected "
                    f"shape (expected a 'sha256:' prefix): {built_digest!r}"
                )
            if built_digest != recorded["recorded_digest"]:
                raise RuntimeError(
                    f"{component}: built image {tag!r} has digest {built_digest}, which does NOT "
                    f"match its recorded digest {recorded['recorded_digest']} (question 212) -- "
                    f"refusing to save a tarball of a different image under this component's "
                    f"name. Rebuild with the component's own build-image.sh and update its "
                    f"IMAGE_DIGEST.md, or investigate the mismatch before retrying --with-images."
                )
            tar_path = images_dir / f"{component}.tar"
            save = subprocess.run(
                ["docker", "save", "-o", str(tar_path), tag],
                capture_output=True, text=True, timeout=300,
            )
            if save.returncode != 0:
                raise RuntimeError(
                    f"`docker save -o {tar_path} {tag}` failed (rc={save.returncode}): "
                    f"{save.stderr.strip()}"
                )
            result[component] = {
                **recorded,
                "collected": True,
                "tarball_sha256": sbom.sha256_file(tar_path),
                "tarball_size": tar_path.stat().st_size,
            }
    return result


# =================================================================================================
# Round 2, item 3: cargo vendor
# =================================================================================================

def collect_vendor(repo_root: Path, kit_root: Path) -> dict:
    """`--with-vendor`: `cargo vendor --offline` into `<kit_root>/vendor/`. This host's own
    `~/.cargo/registry` is already populated (measured: 563 MB) so this should need no network;
    if it genuinely cannot run offline, retries WITHOUT `--offline` exactly once (question 154's
    one permitted exception: "a kit is built with network once and installed with none"). Either
    way, the exact `.cargo/config.toml` fragment `cargo vendor` prints on success (the
    `[source.crates-io] replace-with = "vendored-sources"` block, naming the vendor directory) is
    written to `<kit_root>/vendor/.cargo-config.toml` so an offline rebuild can use it directly.

    Invoked with `cwd=kit_root` and the destination given as the bare relative name `"vendor"`
    (never `kit_root`'s own absolute path), with `--manifest-path` pointing `cargo` at the real
    workspace root -- measured directly while building this task: `cargo vendor <path>` prints
    ITS OWN ARGUMENT back verbatim as the fragment's `directory = "..."` value, so passing
    `kit_root`'s absolute path (which differs between `--out out/kit/full-a` and `--out
    out/kit/full-b`) made two kits built from the identical commit produce two different
    `KIT_MANIFEST` hashes on their very first run -- not a build-environment artefact like a
    linked binary's `LC_UUID`, a genuine bug in how this function invoked `cargo vendor`.
    Confirmed fixed: two independent `cargo vendor --offline --manifest-path ... vendor` runs
    (different cwds, same relative destination) print byte-identical fragments.

    Returns `{"collected": True, "network_used", "offline_error"}`."""
    vendor_dir = kit_root / "vendor"
    vendor_dir.mkdir(parents=True, exist_ok=True)
    manifest_path = repo_root / "Cargo.toml"
    offline = subprocess.run(
        ["cargo", "vendor", "--offline", "--manifest-path", str(manifest_path), "vendor"],
        cwd=kit_root, capture_output=True, text=True, timeout=900,
    )
    network_used = False
    offline_error = None
    result = offline
    if offline.returncode != 0:
        offline_error = (offline.stderr or offline.stdout).strip()
        shutil.rmtree(vendor_dir, ignore_errors=True)
        vendor_dir.mkdir(parents=True, exist_ok=True)
        online = subprocess.run(
            ["cargo", "vendor", "--manifest-path", str(manifest_path), "vendor"],
            cwd=kit_root, capture_output=True, text=True, timeout=1200,
        )
        if online.returncode != 0:
            raise RuntimeError(
                f"cargo vendor failed both --offline and with the network permitted "
                f"(rc={online.returncode}): {(online.stderr or online.stdout).strip()}"
            )
        network_used = True
        result = online
    fragment = (result.stdout or "").strip()
    (vendor_dir / ".cargo-config.toml").write_text(fragment + "\n", encoding="utf-8")
    return {"collected": True, "network_used": network_used, "offline_error": offline_error}


# =================================================================================================
# Round 2, item 4: the viewer's wheels
# =================================================================================================

#: The viewer's own declared runtime dependencies (pyproject.toml's `[project].dependencies`,
#: kept in sync by hand -- see `viewer_runtime_closure`'s own doc), with the extras `python -m
#: altavista` actually needs at import time (`uvicorn[standard]`).
VIEWER_RUNTIME_ROOTS: dict[str, tuple[str, ...]] = {
    "fastapi": (),
    "uvicorn": ("standard",),
    "websockets": (),
    "numpy": (),
    "protobuf": (),
}

#: task 3b's own proof platform: `python:3.13-slim` inside a container on this (macOS/aarch64)
#: host -- i.e. Linux/aarch64/cp313, NOT this host's own platform. `python:3.13-slim` is Debian
#: bookworm (glibc 2.36), which is ABI-compatible with every one of these tags; passing more than
#: one (pip's `--platform` may repeat) is necessary in practice, not merely generous -- measured
#: directly while building this task: numpy 2.5.3 ships ONLY a `manylinux_2_28_aarch64`-tagged
#: wheel (no `manylinux2014` tag at all for this release), so `manylinux2014_aarch64` alone
#: cannot find it even though the wheel runs fine on this glibc; every other package in the
#: viewer's own closure still resolves under `manylinux2014_aarch64` too, so both tags stay
#: listed rather than narrowing to just the one numpy happens to need this version.
WHEEL_PLATFORM_TAGS = ("manylinux2014_aarch64", "manylinux_2_28_aarch64")
WHEEL_PYTHON_VERSION = "3.13"
WHEEL_IMPLEMENTATION = "cp"
WHEEL_ABI = "cp313"


def _normalize_dist_name(name: str) -> str:
    return name.lower().replace("_", "-")


def viewer_runtime_closure() -> dict[str, str]:
    """Every package `python -m altavista` (the viewer server) actually needs at runtime, at the
    EXACT version installed in THIS process's own `.venv` -- a breadth-first walk over
    `importlib.metadata`'s already-installed dependency metadata (no network, no re-resolution),
    rooted at `VIEWER_RUNTIME_ROOTS` (pyproject.toml's own `[project].dependencies` names, kept in
    sync by hand -- pyproject.toml changes rarely, and a drift here would show up as a wheel that
    is silently NOT collected, not as a wrong one, so it is safe if occasionally stale).

    Marker evaluation uses THIS process's own environment (macOS) as a stand-in for the target
    platform (linux/aarch64/cp313) -- every marker anywhere in this closure only discriminates
    non-Windows platforms (`uvicorn[standard]`'s own optional dependencies, e.g. `uvloop`), and
    macOS and Linux agree on that, so this is exact, not approximate, for this specific
    dependency set."""
    env = default_environment()
    installed = {
        _normalize_dist_name(d.metadata["Name"]): d
        for d in importlib_metadata.distributions() if d.metadata.get("Name")
    }
    seen: dict[str, set[str]] = {}
    stack: list[tuple[str, tuple[str, ...]]] = list(VIEWER_RUNTIME_ROOTS.items())
    while stack:
        pkg, extras = stack.pop()
        key = _normalize_dist_name(pkg)
        dist = installed.get(key)
        if dist is None:
            raise RuntimeError(
                f"--with-wheels: {pkg!r} (a declared altavista runtime dependency) is not "
                f"installed in this worktree's own .venv -- install it before building the kit; "
                f"this step never guesses a version to fetch"
            )
        already = seen.get(key)
        if already is not None and set(extras) <= already:
            continue  # visited before, at least this set of extras -- nothing new to expand
        seen[key] = (already or set()) | set(extras)
        for r in dist.requires or []:
            req = Requirement(r)
            if req.marker is not None:
                want_extras = extras or ("",)
                if not any(req.marker.evaluate({**env, "extra": e}) for e in want_extras):
                    continue
            stack.append((req.name, tuple(req.extras)))
    return {name: installed[name].version for name in seen}


def _altavista_version(repo_root: Path) -> str:
    data = tomllib.loads((repo_root / "pyproject.toml").read_text(encoding="utf-8"))
    return data["project"]["version"]


def _altavista_wheel_source_date_epoch(repo_root: Path) -> str:
    """The committer-date epoch (Unix seconds, as `git log`'s own `%ct`) of the last commit
    touching `pyproject.toml` -- the SAME epoch input `scripts/kit/sbom.py::git_epoch` already
    uses for this package's own SBOM (Decision 7: "a Python SBOM's epoch inputs are
    pyproject.toml alone"), reused here as `SOURCE_DATE_EPOCH` rather than a second, independent
    notion of "when was altavista last built". Measured directly while building this task:
    `pip wheel .`'s own output is NOT byte-reproducible across two independent invocations
    without this -- setuptools' `bdist_wheel` stamps each zip entry with the CURRENT wall-clock
    time by default, and honours `SOURCE_DATE_EPOCH` (the well-known reproducible-builds
    convention) instead when it is set. Confirmed fixed: two `pip wheel .` runs with the same
    `SOURCE_DATE_EPOCH` produce byte-identical wheels."""
    result = subprocess.run(
        ["git", "log", "-1", "--format=%ct", "--", "pyproject.toml"],
        cwd=repo_root, capture_output=True, text=True, check=True,
    )
    return result.stdout.strip()


def collect_wheels(repo_root: Path, kit_root: Path) -> tuple[dict, list[dict]]:
    """`--with-wheels` (question 154's one permitted network use, at kit-build time only):
    downloads the viewer's real runtime dependency closure (`viewer_runtime_closure`) at the
    exact versions this worktree's own `.venv` has installed, for linux/aarch64/cp313 (`--only-
    binary=:all:` so a missing wheel is a hard, NAMED gap -- never a silent sdist substitution or
    a host wheel smuggled in), plus a wheel of this repository's own `altavista` package (`pip
    wheel .`, since the viewer server is `python -m altavista`). Returns `(fetch_metadata,
    wheel_gaps)` -- `wheel_gaps` is one `{"name": "wheel:<pkg>", "reason": ...}` entry per
    dependency with no matching platform wheel, sorted by name by the caller
    (`manifest.build_gaps`)."""
    wheels_dir = kit_root / "wheels"
    wheels_dir.mkdir(parents=True, exist_ok=True)
    python = sys.executable

    closure = viewer_runtime_closure()
    fetched: list[dict] = []
    gaps: list[dict] = []
    for name in sorted(closure):
        version = closure[name]
        spec = f"{name}=={version}"
        before = set(wheels_dir.glob("*.whl"))
        platform_args = []
        for tag in WHEEL_PLATFORM_TAGS:
            platform_args += ["--platform", tag]
        result = subprocess.run(
            [
                python, "-m", "pip", "download", "--no-deps",
                "--only-binary=:all:", *platform_args,
                "--python-version", WHEEL_PYTHON_VERSION,
                "--implementation", WHEEL_IMPLEMENTATION, "--abi", WHEEL_ABI,
                "-d", str(wheels_dir), spec,
            ],
            capture_output=True, text=True, timeout=120,
        )
        after = set(wheels_dir.glob("*.whl"))
        new_files = sorted(after - before)
        if result.returncode != 0 or not new_files:
            detail_lines = [l for l in (result.stderr or result.stdout).strip().splitlines() if l.strip()]
            reason = detail_lines[-1] if detail_lines else f"pip download rc={result.returncode}"
            gaps.append({
                "name": f"wheel:{name}",
                "reason": (
                    f"no wheel matching any of {WHEEL_PLATFORM_TAGS}/{WHEEL_IMPLEMENTATION}"
                    f"{WHEEL_ABI[2:]} is available for {spec} -- pip download reported: {reason}"
                ),
            })
            continue
        whl = new_files[0]
        fetched.append({
            "name": name, "version": version, "filename": whl.name,
            "sha256": sbom.sha256_file(whl),
        })

    own = subprocess.run(
        [python, "-m", "pip", "wheel", ".", "--no-deps", "--no-build-isolation", "-w", str(wheels_dir)],
        cwd=repo_root, capture_output=True, text=True, timeout=300,
        env={**os.environ, "SOURCE_DATE_EPOCH": _altavista_wheel_source_date_epoch(repo_root)},
    )
    if own.returncode != 0:
        raise RuntimeError(
            f"`pip wheel .` (this repo's own altavista package) failed (rc={own.returncode}): "
            f"{(own.stderr or own.stdout).strip()}"
        )
    own_wheels = sorted(wheels_dir.glob("altavista-*.whl"))
    if not own_wheels:
        raise RuntimeError(
            "pip wheel . reported success but no altavista-*.whl appeared in the wheels directory"
        )
    fetched.append({
        "name": "altavista", "version": _altavista_version(repo_root),
        "filename": own_wheels[-1].name, "sha256": sbom.sha256_file(own_wheels[-1]),
    })

    return {"collected": True, "network_used": True, "fetched": sorted(fetched, key=lambda f: f["name"])}, gaps


# =================================================================================================
# Round 2, item 6: Linux service binaries
# =================================================================================================

#: Question 217(a) (review defect found in round 3): this pin was left at the `rust:1.85-bookworm`
#: digest after question 215 moved EVERY OTHER cross-build pin in this workspace to the
#: `rust:1.90-bookworm` digest below (`services/proposer/build-image.sh`, `services/cfs/
#: Dockerfile`, `services/edge-plugin/Dockerfile` -- all measured there, not guessed: question 215
#: records "1.86 fails on regorus, 1.87 passes" as the measured MSRV floor, and `rust-version` in
#: the workspace `Cargo.toml` is "1.87"). `scripts/kit/build_kit.py` is this track's own file,
#: written after that merge, so it never picked up the move. Consequence, measured directly in
#: `.av-test-tmp/kit-binaries-cache/<this tree's own cache key>/RESULTS.json` before this fix:
#: rustc 1.85.1 refused to build `av-cdm`/`av-command`/`av-ingest` at all ("rustc 1.85.1 is not
#: supported ... requires rustc 1.87"), so BOTH `av-ingest-server` and `av-command` failed to
#: cross-build and `tests/test_kit_zero_egress_install.py` could only SKIP rather than prove
#: anything (D3's proof going dark). `rust:1.90-bookworm` is still newer than the 1.87 floor
#: (headroom, the same reasoning `services/proposer/Dockerfile`'s own header records), and this
#: exact digest is already present on this host (`docker image inspect rust@sha256:3914072ca0c3...`
#: -> `RepoDigests=["rust@sha256:3914072ca0c3b8aad871db9169a651ccfce30cf58303e5d6f2db16d1d8a7e58f"]`),
#: so moving to it costs no network pull. `_verify_prebuild_base_image_digest` below is new:
#: question 212 ruled that a test must not trust an image it has not compared to its recorded
#: digest, and until this fix nothing in this module compared `PREBUILD_BASE_IMAGE` to anything --
#: `docker run` was simply handed the pin and would have silently reached the network to pull a
#: replacement had the locally-cached digest ever gone missing. That comparison is now explicit
#: and fails closed instead.
PREBUILD_BASE_IMAGE = "rust:1.90-bookworm@sha256:3914072ca0c3b8aad871db9169a651ccfce30cf58303e5d6f2db16d1d8a7e58f"
SPOORE_MOUNT = "/Users/probe/code/spoore"


def _verify_prebuild_base_image_digest(image_ref: str) -> None:
    """Question 212's ordering, applied to `PREBUILD_BASE_IMAGE` (round 3, question 217(a)):
    compare the image this module is about to build with to its OWN recorded digest -- the digest
    literally embedded in `image_ref` -- via `docker image inspect ... RepoDigests` BEFORE ever
    invoking `docker run`. `image_ref` is already a `repo:tag@sha256:...` reference, so `docker
    run`/`docker image inspect` will themselves refuse anything that doesn't match that digest
    ONCE the image is resolved -- what this function adds is refusing to proceed AT ALL when the
    image is not already present locally under that exact digest, rather than letting `docker run`
    silently fall through to a network pull mid cross-build (this step is not documented as one
    that reaches the network the way `--with-wheels` is; only `apt-get` inside the container is,
    and that is recorded separately in `collect_binaries`'s own `network_used`)."""
    if "@sha256:" not in image_ref:
        raise RuntimeError(
            f"PREBUILD_BASE_IMAGE {image_ref!r} is not pinned by digest -- question 154/185's own "
            f"rule: every base image is pinned by content digest, never a floating tag."
        )
    repo_and_tag, _, digest = image_ref.partition("@")
    repo, _, _tag = repo_and_tag.partition(":")
    expected = f"{repo}@{digest}"
    result = subprocess.run(
        ["docker", "image", "inspect", image_ref, "--format", "{{json .RepoDigests}}"],
        capture_output=True, text=True, timeout=30,
    )
    if result.returncode != 0:
        raise RuntimeError(
            f"PREBUILD_BASE_IMAGE {image_ref!r} is not present locally under this exact digest "
            f"(`docker image inspect` failed, rc={result.returncode}): {result.stderr.strip()}. "
            f"This step does not pull images from the network -- `docker pull {image_ref}` first, "
            f"or investigate why the digest this module is pinned to no longer matches what is "
            f"cached on this host."
        )
    repo_digests = json.loads(result.stdout)
    if expected not in repo_digests:
        raise RuntimeError(
            f"PREBUILD_BASE_IMAGE {image_ref!r} does NOT match its recorded digest (question 212) "
            f"-- `docker image inspect` reports RepoDigests={repo_digests!r}, which does not "
            f"contain {expected!r}. Refusing to cross-build against an image that is not the one "
            f"this module is pinned to."
        )

#: manifest binary name -> (cargo package, cargo --bin name). `av-command` is included but this
#: module never edits `crates/av-command` -- only builds it (this task's own hard rule).
BINARY_TARGETS: dict[str, tuple[str, str]] = {
    "av-ingest-server": ("av-ingest", "av-ingest-server"),
    "av-command": ("av-command", "av-command"),
}


def _cross_build_one_binary(repo_root: Path, pkg: str, bin_name: str, run_id: str) -> tuple[bool, "Path | None", "str | None"]:
    """Cross-builds ONE `BINARY_TARGETS` entry for Linux, the identical bind-mounted `docker run`
    idiom `tests/test_edge_plugin_container.py::_cross_build_ingest_server_binary` establishes
    (read before writing this: it bind-mounts `/Users/probe/code/spoore` read-only because
    `av-cdm` -- and therefore every crate in this workspace -- carries a path dependency onto
    `spoore-cdm`, `Cargo.toml`'s own `[workspace.dependencies]`). Returns `(ok, built_path,
    error)` -- `error` is the REAL compiler/build stderr, never a synthesized message, so a
    genuine cross-build failure (e.g. `av-command`) can be recorded as a named gap with its own
    actual root cause."""
    scratch_target = repo_root / "target-docker-linux-kit" / pkg
    shutil.rmtree(scratch_target, ignore_errors=True)
    container_name = f"av-kit-binary-build-{pkg}-{run_id}"
    build = subprocess.run(
        [
            "docker", "run", "--rm", "--name", container_name,
            "--label", "av.test=1", "--label", f"av.test.run_id={run_id}",
            "-v", f"{repo_root}:/workspace",
            "-v", f"{SPOORE_MOUNT}:{SPOORE_MOUNT}:ro",
            "-w", "/workspace",
            PREBUILD_BASE_IMAGE,
            "bash", "-c",
            "apt-get update -qq && apt-get install -y -qq --no-install-recommends "
            "protobuf-compiler libprotobuf-dev libssl-dev pkg-config >/dev/null && "
            f"cargo build --release -p {pkg} --bin {bin_name} "
            f"--target-dir /workspace/target-docker-linux-kit/{pkg} && "
            f"strip /workspace/target-docker-linux-kit/{pkg}/release/{bin_name}",
        ],
        capture_output=True, text=True, timeout=900,
    )
    built = scratch_target / "release" / bin_name
    if build.returncode != 0 or not built.is_file():
        error = (build.stderr or build.stdout).strip()
        shutil.rmtree(scratch_target, ignore_errors=True)
        return False, None, error
    return True, built, None


def binary_cache_key(git_commit: str, git_status_text: str, git_diff_text: str) -> str:
    """The cache directory name `_cross_build_binaries` builds into: the commit, plus a short
    hash of the WORKING-TREE state on top of it. A pure function of its three string arguments,
    so it can be tested directly without a git repository or a cross-build.

    Review finding (P5 round 2, manager): an earlier cut keyed this cache on `git_commit` ALONE.
    A cross-built binary is then reused for every kit built at that commit -- including kits built
    from a tree carrying uncommitted changes to the very sources that binary was compiled from.
    The kit would carry bytes compiled from a DIFFERENT tree state than the one it records, and
    nothing anywhere would say so: exactly the class of failure that leaves no trace. `git_status`
    and `git diff HEAD` are both folded in, so any tracked modification (content) or any new
    untracked path (name) produces a different cache directory and forces a real rebuild.

    What this deliberately does NOT cover, stated rather than implied: the CONTENT of an untracked
    file. `git status --porcelain` names it, so creating or deleting one changes this key, but
    editing one already named does not. An untracked file can only reach a `cargo build` through a
    tracked file that references it, and that reference is itself a tracked change -- so the
    remaining exposure is editing an untracked file that an already-tracked `include!`/`mod` line
    already points at. A kit built from such a tree records that untracked path in its own
    `git_status`, which is where a reader would see it.
    """
    h = hashlib.sha256()
    h.update(git_commit.encode("utf-8"))
    h.update(b"\0")
    h.update(git_status_text.encode("utf-8"))
    h.update(b"\0")
    h.update(git_diff_text.encode("utf-8"))
    return f"{git_commit}-{h.hexdigest()[:12]}"


def _git_worktree_state_texts(repo_root: Path) -> tuple[str, str]:
    """`(git status --porcelain, git diff HEAD)` as raw text -- the two inputs
    `binary_cache_key` folds in beside the commit. Read here rather than inside
    `binary_cache_key` so that function stays pure and directly testable."""
    status = subprocess.run(
        ["git", "status", "--porcelain"], cwd=repo_root, capture_output=True, text=True, check=True,
    ).stdout
    diff = subprocess.run(
        ["git", "diff", "HEAD"], cwd=repo_root, capture_output=True, text=True, check=True,
    ).stdout
    return status, diff


def _cross_build_binaries(repo_root: Path, cache_dir: Path) -> dict:
    """Cross-builds every `BINARY_TARGETS` binary for Linux, ONCE PER SOURCE STATE, into
    `cache_dir` (`<repo_root>/.av-test-tmp/kit-binaries-cache/<binary_cache_key(...)>/`,
    `.gitignore`d, persisted across separate `build_kit.py` invocations, not just within one
    process) -- reused by every kit built from that same source state. This is what keeps
    `--with-binaries` compatible with "two kits built from the same commit have the same manifest
    hash" despite a cross-built binary's hash being measured (round 1) to differ across
    independent links of identical source: build once, copy the SAME bytes into every kit, never
    re-link per kit. See `binary_cache_key` for why the key is not the commit alone.

    **This step uses the network**, at kit-build time only (question 154: a kit is built with
    network once and installed with none): the cross-build container runs `apt-get update` and
    installs `protobuf-compiler`/`libprotobuf-dev`/`libssl-dev`/`pkg-config` before `cargo build`.
    `collect_binaries` records that in the manifest's `binaries.network_used`, the same way
    `collect_wheels` records its own -- a kit must never be able to claim it was assembled with no
    network when a step of it reached out.

    Held under the host-wide docker lock for its whole body; both cross-builds' containers carry
    `av.test`/`av.test.run_id`, pruned by label first (questions 156/207)."""
    cache_dir.mkdir(parents=True, exist_ok=True)
    marker = cache_dir / "RESULTS.json"
    if marker.is_file():
        return json.loads(marker.read_text(encoding="utf-8"))

    if not _docker_available():
        raise RuntimeError(
            "--with-binaries was requested but `docker info` failed -- this step cross-builds "
            "Linux binaries inside a container, so Docker must be available."
        )

    from altavista.container_hardening import prune_stale_labelled_resources
    from altavista.docker_test_lock import lock_docker_tests

    run_id = uuid.uuid4().hex
    results: dict[str, dict] = {}
    with lock_docker_tests():
        prune_stale_labelled_resources()
        _verify_prebuild_base_image_digest(PREBUILD_BASE_IMAGE)
        for bin_name, (pkg, cargo_bin) in BINARY_TARGETS.items():
            ok, built_path, error = _cross_build_one_binary(repo_root, pkg, cargo_bin, run_id)
            if ok:
                dest = cache_dir / bin_name
                shutil.copy2(built_path, dest)
                dest.chmod(0o755)
                results[bin_name] = {
                    "ok": True, "sha256": sbom.sha256_file(dest), "size": dest.stat().st_size,
                }
                # The scratch cross-build tree is no longer needed once the binary itself is
                # cached -- only the copy under `cache_dir` is kept.
                shutil.rmtree(repo_root / "target-docker-linux-kit" / pkg, ignore_errors=True)
            else:
                results[bin_name] = {"ok": False, "error": error}
    marker.write_text(json.dumps(results, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return results


def collect_binaries(repo_root: Path, kit_root: Path, git_commit: str) -> tuple[dict, list[dict]]:
    """`--with-binaries`: copies each successfully cross-built `BINARY_TARGETS` binary (from the
    per-commit cache -- see `_cross_build_binaries`) into `<kit_root>/binaries/`. A binary that
    did not cross-build (e.g. `av-command`, if its own dependency graph does not cross-compile
    cleanly) is never silently omitted -- it becomes a named gap carrying the REAL compiler
    error, and the build continues (this task's own rule: "record it as a named gap ... do not
    retry forever, and move on")."""
    status_text, diff_text = _git_worktree_state_texts(repo_root)
    cache_key = binary_cache_key(git_commit, status_text, diff_text)
    cache_dir = repo_root / ".av-test-tmp" / "kit-binaries-cache" / cache_key
    results = _cross_build_binaries(repo_root, cache_dir)

    bin_dir = kit_root / "binaries"
    bin_dir.mkdir(parents=True, exist_ok=True)
    manifest_results: dict[str, dict] = {}
    gaps: list[dict] = []
    for bin_name, info in results.items():
        if info["ok"]:
            shutil.copy2(cache_dir / bin_name, bin_dir / bin_name)
            (bin_dir / bin_name).chmod(0o755)
            manifest_results[bin_name] = {"included": True, "reason": None}
        else:
            manifest_results[bin_name] = {"included": False, "reason": info["error"]}
            gaps.append({
                "name": f"{bin_name}-binary",
                "reason": f"cross-build for linux failed: {info['error']}",
            })
    return (
        {
            "collected": True,
            # The cross-build container installs its build dependencies with apt before compiling
            # (see `_cross_build_one_binary`), so this step reaches the network at kit-build time
            # -- recorded here rather than left for a reader to infer, exactly as `collect_wheels`
            # records its own `network_used`.
            "network_used": True,
            "source_state": cache_key,
            "results": manifest_results,
        },
        sorted(gaps, key=lambda g: g["name"]),
    )


# =================================================================================================
# git state
# =================================================================================================

def git_state(repo_root: Path) -> tuple[str, bool, list[str]]:
    """`(commit, dirty, status_lines)`. `dirty` alone is a permanently-`true` flag IN THIS
    WORKTREE specifically (pre-existing untracked entries -- see `scripts/kit/README.md`), so
    `status_lines` (the raw `git status --porcelain` output) is what actually lets a reader
    distinguish "the known pre-existing entries" from "a kit built from a tree with real
    uncommitted source changes"."""
    commit = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=repo_root, capture_output=True, text=True, check=True,
    ).stdout.strip()
    status_text = subprocess.run(
        ["git", "status", "--porcelain"], cwd=repo_root, capture_output=True, text=True, check=True,
    ).stdout
    status_lines = [line for line in status_text.splitlines() if line.strip()]
    return commit, bool(status_lines), status_lines


# =================================================================================================
# Orchestration
# =================================================================================================

def build(
    *,
    repo_root: Path,
    out: Path,
    site: Path,
    with_images: bool = False,
    with_pack_names: list[str] | None = None,
    copy_pack_names: list[str] | None = None,
    max_pack_bytes: int = kit_manifest.DEFAULT_MAX_PACK_BYTES,
    with_vendor: bool = False,
    with_wheels: bool = False,
    with_binaries: bool = False,
) -> tuple[Path, str]:
    """Assembles a kit at `out` and writes its `KIT_MANIFEST`. Returns `(manifest_path,
    manifest_sha256)` -- the sha256 is computed AFTER writing, never embedded in the manifest
    itself (Decision L)."""
    kit_root = out
    if kit_root.exists() and any(kit_root.iterdir()):
        raise RuntimeError(f"--out {kit_root} already exists and is not empty -- pass an empty or new directory")
    kit_root.mkdir(parents=True, exist_ok=True)

    assemble_suite_and_site(repo_root, kit_root, site)
    sboms = assemble_sboms(repo_root, kit_root)
    assemble_image_digest_docs(repo_root, kit_root)
    runs = assemble_runs(repo_root, kit_root)

    # Round 2 task 3c unioned `web`/`profiles` into every kit's copy set here, unconditionally.
    # Round 3 (question 217(b)) removed both packs -- the `altavista` wheel carries them now (see
    # manifest.py's own top doc) -- so nothing is unioned in any more; a caller's own explicit
    # `--copy-pack` request is still honoured for every other pack exactly as before.
    copy_set = set(copy_pack_names or [])
    pack_names = list(dict.fromkeys([kit_manifest.DEFAULT_PACK, *(with_pack_names or []), *copy_set]))
    unknown_packs = [n for n in pack_names if n not in kit_manifest.PACKS]
    if unknown_packs:
        raise ValueError(f"unknown pack(s) {unknown_packs!r} -- known packs: {sorted(kit_manifest.PACKS)}")

    packs: dict[str, dict] = {}
    for name in pack_names:
        source_path = repo_root / kit_manifest.PACKS[name]
        desc = kit_manifest.pack_descriptor(
            name, source_path, repo_root=repo_root, max_pack_bytes=max_pack_bytes,
        )
        if name in copy_set:
            copy_result = kit_manifest.copy_pack_bytes(name, source_path, kit_root)
            desc["copied"] = True
            desc["copy"] = copy_result
        packs[name] = desc

    images = collect_images(repo_root, kit_root) if with_images else kit_manifest.uncollected_images()

    if with_vendor:
        vendor = collect_vendor(repo_root, kit_root)
    else:
        vendor = {"collected": False, "network_used": False, "offline_error": None}

    extra_gaps: list[dict] = []

    if with_wheels:
        wheels, wheel_gaps = collect_wheels(repo_root, kit_root)
        extra_gaps.extend(wheel_gaps)
    else:
        wheels = {"collected": False, "network_used": False, "fetched": []}

    git_commit, git_dirty, git_status = git_state(repo_root)

    if with_binaries:
        binaries, binary_gaps = collect_binaries(repo_root, kit_root, git_commit)
        extra_gaps.extend(binary_gaps)
    else:
        # Same shape as the collected case (explicit `False`/`None` rather than absent fields),
        # so a reader never has to distinguish "field omitted" from "field known false".
        binaries = {"collected": False, "network_used": False, "source_state": None, "results": {}}

    gaps = kit_manifest.build_gaps(
        vendor_collected=vendor["collected"],
        wheels_collected=wheels["collected"],
        extra=sorted(extra_gaps, key=lambda g: g["name"]),
    )

    manifest_doc = kit_manifest.build_manifest(
        kit_root=kit_root, git_commit=git_commit, git_dirty=git_dirty, git_status=git_status,
        images=images, sboms=sboms, packs=packs, vendor=vendor, wheels=wheels, binaries=binaries,
        runs=runs, gaps=gaps,
    )
    manifest_path = kit_manifest.write_manifest(manifest_doc, kit_root)
    manifest_sha256 = sbom.sha256_file(manifest_path)

    # Belt-and-suspenders (review finding D3-1, round 1): a kit this builder just wrote must
    # verify with zero findings -- if it does not, that is this builder's own bug, caught here
    # rather than handed to whoever builds/ships the kit next.
    self_findings = kit_manifest.verify_manifest(kit_root)
    if self_findings:
        raise RuntimeError(
            f"build_kit.build: the kit it just wrote at {kit_root} does not verify clean -- this "
            f"is a bug in the builder itself, not a tampered/incomplete kit: {self_findings!r}"
        )

    return manifest_path, manifest_sha256


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--out", required=True, help="output directory for the kit (must not already exist non-empty)")
    p.add_argument(
        "--site", default=str(DEFAULT_SITE),
        help=f"one of deploy/secdeploy's standalone site files (repo-relative; default: {DEFAULT_SITE})",
    )
    p.add_argument(
        "--with-images", action="store_true",
        help="gated: docker save the two recorded images into the kit, after comparing each to "
             "its recorded digest (question 212). Off by default -- see manifest.py's 'images' doc.",
    )
    p.add_argument(
        "--with-pack", action="append", default=[], dest="with_pack",
        help=f"repeatable: include an additional Decision-K pack DESCRIPTOR beyond the default "
             f"{kit_manifest.DEFAULT_PACK!r}. Known packs: {sorted(kit_manifest.PACKS)}",
    )
    p.add_argument(
        "--copy-pack", action="append", default=[], dest="copy_pack",
        help="repeatable: copy that pack's real BYTES into the kit (round 2), not merely its "
             "descriptor -- implies --with-pack for that name. --max-pack-bytes still gates it. "
             "Nothing is copied unconditionally any more (round 3, question 217(b), removed the "
             "web/profiles packs that once were) -- pass it explicitly for any pack you want.",
    )
    p.add_argument(
        "--max-pack-bytes", type=int, default=kit_manifest.DEFAULT_MAX_PACK_BYTES,
        help=f"refuse (never silently skip) any --with-pack/--copy-pack pack whose total size "
             f"exceeds this (default {kit_manifest.DEFAULT_MAX_PACK_BYTES})",
    )
    p.add_argument(
        "--with-vendor", action="store_true",
        help="gated (round 2): cargo vendor --offline (network once only if that genuinely "
             "fails, question 154) into <kit>/vendor/.",
    )
    p.add_argument(
        "--with-wheels", action="store_true",
        help="gated (round 2): download the viewer's runtime wheels for linux/aarch64/cp313 at "
             "this worktree's own installed versions, plus a wheel of altavista itself. Uses "
             "the network once, at kit-build time (question 154).",
    )
    p.add_argument(
        "--with-binaries", action="store_true",
        help="gated (round 2): cross-build av-ingest-server/av-command for Linux (built once "
             "per commit, cached, never rebuilt per kit) into <kit>/binaries/.",
    )
    return p


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)

    out = Path(args.out)
    if not out.is_absolute():
        out = Path.cwd() / out
    site = Path(args.site)
    if not site.is_absolute():
        site = REPO_ROOT / site

    manifest_path, manifest_sha256 = build(
        repo_root=REPO_ROOT, out=out, site=site,
        with_images=args.with_images,
        with_pack_names=list(args.with_pack),
        copy_pack_names=list(args.copy_pack),
        max_pack_bytes=args.max_pack_bytes,
        with_vendor=args.with_vendor,
        with_wheels=args.with_wheels,
        with_binaries=args.with_binaries,
    )
    print(f"wrote kit to {manifest_path.parent}")
    print(f"wrote {manifest_path}")
    print(f"KIT_MANIFEST sha256: {manifest_sha256}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
