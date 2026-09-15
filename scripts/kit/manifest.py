"""scripts/kit/manifest.py -- D3 first half (docs/p5-plan.md, P5 track round 1: "the kit builder
and its manifest, without the zero-egress install proof"): the KIT_MANIFEST format (Decision L)
and everything needed to build or verify one.

# Decision I -- why the builder only ever collects and hashes, and never builds

Measured directly, not assumed: three independent `cargo auditable build`s of the identical
`av-ingest-server` source, back to back with no source change between them, produced three
different SHA-256 hashes for the linked macOS debug binary (Mach-O's per-link `LC_UUID` load
command, plus other build-environment detail the debug profile embeds -- `scripts/kit/sbom.py`'s
own `rust_binary_sbom` records the identical finding and the identical conclusion for the SBOM's
narrower scope: the artefact's own content hash is not reproducible, so it is never recorded
there either). A rebuilt container image gets a new image id for the same class of reason one
level up -- Docker/BuildKit embeds build-environment metadata, not merely file content; see
`services/cfs/tests/test_image_digest.py`'s own "not reproducible by construction" finding
(cFE's `CONFIGDATA` bakes in a `BUILDDATE` unless the builder sets one, and this repository's
`services/cfs/Dockerfile` does not).

Given both measured facts, this deliverable's headline claim -- "a kit built twice from the same
commit has the same manifest hash" (`tests/test_kit_manifest.py::
test_two_kits_from_the_same_commit_have_the_same_manifest_hash`) -- can only hold if every byte
the kit carries is EITHER a value already recorded in a committed file (`IMAGE_DIGEST.md`, the
committed SBOMs, `SHA256SUMS`) OR a hash computed OVER an artefact that already exists and is not
being (re)produced as part of assembling the kit, never the live output of a fresh `cargo build`
or `docker build` run by the builder itself. `scripts/kit/build_kit.py` (the orchestration layer
that uses this module) therefore never invokes either: it calls `deploy/secdeploy/merge.py`'s own
functions over already-committed TOML, copies already-committed SBOM/digest files byte-for-byte,
and -- gated, opt-in, off by default -- runs `docker save` on an already-built image only AFTER
confirming that image still matches its OWN already-recorded digest (never building, tagging, or
retagging one). This module's own `build_manifest`/`pack_descriptor` follow the same rule: they
hash what is already on disk, they never write build output of their own.

# Why this is a separate module from the builder

This module's job -- the manifest FORMAT, and how to compute or verify it -- must be importable
and testable with no filesystem side effects of its own beyond reading/writing under a kit root
the caller already chose, and no dependency on docker or git. `scripts/kit/build_kit.py`'s job --
deciding WHAT bytes go into a kit and where they come from (secdeploy's merge.py, the committed
SBOMs, docker) -- is a different concern with a much larger dependency footprint. Splitting them
means a test of the manifest format itself (tamper detection, determinism, the gap list) never
needs docker or git, and a change to how kits are assembled never has to touch this file. Library
only: nothing below prints or calls `sys.exit`.

# Format (Decision L)

`KIT_MANIFEST` is JSON, written by `write_manifest` as `json.dump(..., indent=2, sort_keys=True,
ensure_ascii=False)` plus one trailing newline. It contains:

- `kit_format` (int, bumped whenever this shape changes -- see `KIT_FORMAT` below).
- `git_commit` / `git_dirty` / `git_status` -- the full HEAD SHA, whether the tree that produced
  the kit was clean, and (review finding D3-2: `git_dirty` alone is permanently `true` in a
  worktree that carries pre-existing untracked symlinks, e.g. `third_party/mirrors` -- see
  `scripts/kit/README.md` -- which makes the bare flag useless as a signal here) the sorted,
  verbatim `git status --porcelain` lines themselves, so a reader can tell "the three known
  untracked entries" from "someone shipped a kit with real uncommitted source changes." Supplied
  by the caller (`build_kit.py` computes these by shelling out to `git`); this module only
  shapes them into the document. `git status --porcelain` paths are already repo-root-relative
  by git's own convention (`build_kit.py` runs it with `cwd=repo_root`), so this never
  introduces an absolute path.
- `files` -- every regular, non-symlink file actually present under the kit root at the moment
  `build_manifest` runs, EXCLUDING `KIT_MANIFEST` itself (the manifest never contains its own
  hash -- see `write_manifest`'s own doc comment). A kit built by `build_kit.py` contains NO
  symlinks at all as of this fix (a review of the first cut of this deliverable found that
  `deploy/secdeploy/merge.py::merge` plants a `deploy` symlink pointing at an ABSOLUTE path into
  the user's own secdeploy checkout -- exactly the kind of host-specific, air-gap-hostile content
  a kit must never carry, and one `verify_manifest` did not even see: see `DECLARED_GAPS`'s fifth
  entry and `build_kit.assemble_suite_and_site` for the fix). `verify_manifest` nonetheless still
  actively looks for one and reports it (`"unexpected_symlink"` / `"unsafe_symlink_target"`,
  below) rather than silently ignoring it a second time, in case one is ever reintroduced. Each
  `files` entry is `{"path", "sha256", "size", "role"}`, `path` kit-root-relative with POSIX
  separators, the list sorted by path.
- `images` -- one entry per recorded image (`sbom.IMAGE_COMPONENTS`, reused directly rather than
  re-declared here -- see `read_recorded_image` below): `tag`, `recorded_digest`, `collected`
  (bool), and `tarball_sha256`/`tarball_size` (present, non-null, only when `collected` is true).
- `sboms` -- component name -> SHA-256, built by the caller from the kit's own copy of the
  committed `SHA256SUMS` (Decision L: "cross-checked against the committed SHA256SUMS" --
  `build_kit.py` copies `docs/compliance/sbom/SHA256SUMS` byte-for-byte into the kit and derives
  this dict from that copy, so the two can never silently drift apart).
- `packs` -- Decision K's descriptors (`pack_descriptor`, below), one per pack the caller asked
  to include -- always at least `data-time`, the small in-tree default.
- `gaps` -- Decision J's declared, out-of-scope-for-this-half items (`DECLARED_GAPS`, below --
  five as of the D3-1 review fix, the fifth being the `deploy` symlink's own assets), always
  present and never conditional on what the kit actually contains.

No timestamps, no absolute paths, no hostname, no username anywhere in the output -- see
`pack_descriptor`'s own doc comment for how a pack that lives outside this worktree is recorded
without ever writing an absolute path.
"""
from __future__ import annotations

import hashlib
import json
import os
from dataclasses import dataclass
from pathlib import Path
from typing import Optional

# scripts/kit/sbom.py is stdlib-only itself (its own module doc says so) and lives in this same
# directory; like sbom.py's own `import licences`, this relies on the caller having put
# scripts/kit on sys.path (true automatically for `python scripts/kit/build_kit.py`, and done
# explicitly by tests/test_kit_manifest.py and tests/test_sbom.py alike) rather than this module
# mutating sys.path itself.
import sbom  # noqa: E402  (see comment above -- caller is responsible for sys.path)

KIT_FORMAT = 1

# --- Decision J (plus the D3-1 review fix): the five things this half of D3 deliberately does
# not carry, named instead of stubbed. Always present in every manifest, unconditionally -- see
# this module's own top doc. ---------------------------------------------------------------------
DECLARED_GAPS: tuple[dict, ...] = (
    {
        "name": "cargo-vendor",
        "reason": (
            "the offline crate sources `cargo vendor` would produce are not collected by this "
            "half of D3 -- they belong with D3's second half, where the zero-egress install is "
            "what actually exercises an offline `cargo build` against them; collecting them here "
            "with nothing to prove they are complete/correct would be a stub, not a deliverable"
        ),
    },
    {
        "name": "python-wheels",
        "reason": (
            "no `pip download`/wheel cache is collected by this half of D3 -- like cargo-vendor, "
            "its only real proof of correctness is a zero-egress `pip install` from it, which is "
            "D3's second half's job, not this one's"
        ),
    },
    {
        "name": "seccert-root",
        "reason": (
            "the seccert trust root a deployed kit would carry is not collected here -- it is an "
            "install-time concern (what a fresh install trusts before it can verify anything "
            "else in the kit), out of scope for a kit that only assembles and self-verifies"
        ),
    },
    {
        "name": "install-path",
        "reason": (
            "this half builds and verifies KIT_MANIFEST; it never installs anything from a kit. "
            "The zero-egress install proof -- unpacking a kit with the network disabled and "
            "confirming the result is usable -- is D3's second half by the manager's own split"
        ),
    },
    {
        "name": "secdeploy-deploy-assets",
        "reason": (
            "deploy/secdeploy/merge.py::merge also writes a `deploy` symlink alongside "
            "suite.merged.toml/secsite.merged.toml, pointing at the BASE secdeploy manifest's "
            "own deploy/ directory -- an ABSOLUTE path into the user's own secdeploy checkout "
            "(a review of this deliverable's first cut found it still present, unlisted, inside "
            "the kit: dangling if carried into an air-gapped enclave, silently pointing at "
            "whatever happens to live at that path anywhere else). That directory is the user's "
            "own secdeploy checkout, not ours to bundle, so scripts/kit/build_kit.py now runs "
            "merge() into a throwaway staging directory and copies out only the two TOML files, "
            "discarding the symlink -- the deploy/ assets themselves remain out of scope for "
            "this kit, to be installed from the user's own secdeploy checkout (or a separately "
            "licensed copy) as part of D3's second half's install path, never carried inside "
            "this kit as a symlink"
        ),
    },
)

# --- Decision K: named packs this round may describe (never copy bytes for). Repo-relative
# source paths, exactly as they appear at the repo root -- some are plain in-tree directories,
# some are symlinks to another worktree (see each one's own module-level comment in the manager's
# brief: "GMAT R2026a" is a symlink to another worktree entirely; "third_party/{mirrors,cspice,
# cfs}" are symlinks too). ------------------------------------------------------------------------
PACKS: dict[str, str] = {
    "data-time": "data/time",
    "gmat": "GMAT R2026a",
    "mirrors": "third_party/mirrors",
    "cspice": "third_party/cspice",
    "cfs": "third_party/cfs",
}

#: The one pack every kit includes regardless of `--with-pack` (Decision K: "keep the default
#: test set to the small in-tree pack (data/time) so the default gate stays fast").
DEFAULT_PACK = "data-time"

#: Generous for any small in-tree pack (data/time is ~10 KB), far short of the 738 MB GMAT
#: pack -- so requesting a big pack needs an explicit, larger `--max-pack-bytes` alongside
#: `--with-pack`, never a silent multi-minute hash by default (Decision K).
DEFAULT_MAX_PACK_BYTES = 50_000_000


class PackTooLargeError(RuntimeError):
    """Raised by `pack_descriptor` when a pack's total byte size exceeds `max_pack_bytes` --
    Decision K's "documented `--max-pack-bytes` refusal rather than silently skipping". Raised
    BEFORE any file content is read (only `os.stat` size information is needed to decide this),
    so refusing a 738 MB pack costs a directory walk, never a multi-minute hash."""


# =================================================================================================
# Typed findings (verify_manifest's return type)
# =================================================================================================

@dataclass(frozen=True)
class Finding:
    """One thing `verify_manifest` found wrong. `kind` is one of:

    - `"missing_file"` -- a path `KIT_MANIFEST` lists is not present in the kit.
    - `"hash_mismatch"` -- the path exists but its content's SHA-256 does not match what
      `KIT_MANIFEST` recorded (`expected_sha256`/`actual_sha256` both set).
    - `"size_mismatch"` -- the path exists but its byte size does not match what `KIT_MANIFEST`
      recorded (`expected_size`/`actual_size` both set) -- independent of `hash_mismatch`: a
      same-size tamper trips only the hash finding, a truncated file trips both.
    - `"unlisted_file"` -- a regular, non-symlink file exists under the kit root that
      `KIT_MANIFEST`'s `files` list does not mention at all. Deliberately its own finding kind,
      not folded into `"missing_file"`'s absence -- Decision L / this deliverable's own brief:
      "a kit that carries something the manifest does not list is exactly as broken as one
      missing a file, and it is the case people forget."
    - `"unexpected_symlink"` -- a symlink exists anywhere under the kit root at all (`link_target`
      set to the raw, unresolved `os.readlink` string). A kit built by `build_kit.py` must
      contain NONE (review finding D3-1: an earlier cut of this deliverable both planted one --
      `deploy/secdeploy/merge.py::merge`'s own `deploy` symlink -- and failed to detect a
      deliberately planted one during verification, because the original file-walkers skipped
      every symlink outright rather than reporting it). Used for a symlink whose target is
      relative and stays lexically inside the kit; see `"unsafe_symlink_target"` for the more
      dangerous case.
    - `"unsafe_symlink_target"` -- a symlink whose target is either an ABSOLUTE path or
      lexically escapes the kit root via `..` -- reported as its own, more severe kind
      (`link_target` set) rather than folded into `"unexpected_symlink"`, since this is
      specifically the shape that is dangling in an air-gapped enclave and silently wrong
      anywhere else (exactly the `deploy` symlink `merge()` used to plant). Classified purely
      lexically (`os.path.normpath` on the target joined against the symlink's own parent
      directory, never `Path.resolve`/`os.path.realpath`) so a DANGLING symlink -- whose target
      does not exist -- is classified identically to one that does; see `_classify_symlink`.
    """

    kind: str
    path: str
    expected_sha256: Optional[str] = None
    actual_sha256: Optional[str] = None
    expected_size: Optional[int] = None
    actual_size: Optional[int] = None
    link_target: Optional[str] = None
    message: str = ""


# =================================================================================================
# Pack descriptors (Decision K)
# =================================================================================================

def _iter_pack_files(root: Path):
    """Yields every regular, non-symlink file under `root`, in a stable (sorted, per-directory)
    order. `followlinks=False` (the `os.walk` default) and an explicit symlink check on both
    directories and files: a pack directory containing a symlink to something else entirely is a
    real possibility on this host (this repo's own top level has several), and this walker must
    never silently follow one into content that is not actually part of the pack."""
    for dirpath, dirnames, filenames in os.walk(root, followlinks=False):
        dirnames[:] = sorted(d for d in dirnames if not (Path(dirpath) / d).is_symlink())
        for fn in sorted(filenames):
            full = Path(dirpath) / fn
            if full.is_symlink():
                continue
            yield full


def _pack_content_hash(root: Path, files: list[Path]) -> str:
    """Decision K's "recursive content hash over the pack's files (sorted relative path plus
    each file's SHA-256, hashed as one stream)". Exact serialisation, so anyone can recompute it
    independently: for every file in `files` (already sorted by path relative to `root`, POSIX
    separators), one line `"<relpath> <sha256hex>\\n"`, concatenated in that order and hashed as
    one UTF-8 byte stream (updated incrementally, not built as one giant string, so this scales
    to the 738 MB GMAT pack without holding it all in memory at once). This is deliberately the
    SAME shape as `services/cfs/IMAGE_DIGEST.md`'s own "Runtime-content hash" definition (one
    line per file, `"<path> <sha256>"`, sorted, `\\n`-joined, trailing `\\n`) -- reusing an
    already-reviewed convention rather than inventing a new one. Returned as `"sha256:<hex>"`.
    """
    h = hashlib.sha256()
    for f in files:
        rel = f.relative_to(root).as_posix()
        line = f"{rel} {sbom.sha256_file(f)}\n"
        h.update(line.encode("utf-8"))
    return f"sha256:{h.hexdigest()}"


def pack_descriptor(
    name: str, source_path: Path, *, repo_root: Path, max_pack_bytes: int = DEFAULT_MAX_PACK_BYTES,
) -> dict:
    """Build one Decision-K pack descriptor for the pack at `source_path` (a real, existing
    directory -- `PACKS`'s values, resolved against `repo_root`).

    Two-pass by construction: first a cheap `os.stat`-only walk to sum `total_bytes` and refuse
    (`PackTooLargeError`) before any content is read if the pack exceeds `max_pack_bytes`; only
    then a second pass that actually reads and hashes every file (`_pack_content_hash`).

    `resolved_real_path` is `os.path.relpath` of the pack's `os.path.realpath` AGAINST
    `repo_root` -- relative, by construction, never absolute (Decision L's "no absolute paths
    anywhere in the output"), even though it is meant to answer "where does this actually live":
    for `data/time` (in-tree, no symlink) it is `"data/time"`; for `GMAT R2026a` (a symlink at
    the repo root to another worktree entirely) it comes out as `"../AltaVista/GMAT R2026a"` --
    a relative path with `..` segments, which still says exactly where the pack resolves without
    ever embedding this host's home directory or username. `in_worktree` is whether that resolved
    real path is `repo_root` or a descendant of it; `via_symlink` is whether `source_path` itself
    (the one path component this repo names, e.g. `"GMAT R2026a"` or `"third_party/mirrors"`) is
    a symlink -- Decision K: "a symlinked pack is recorded as such, never silently followed as if
    it were ours."
    """
    if not source_path.is_dir():
        raise FileNotFoundError(f"pack {name!r}: {source_path} does not exist or is not a directory")

    real = Path(os.path.realpath(source_path))
    via_symlink = source_path.is_symlink()
    try:
        in_worktree = real == repo_root or real.is_relative_to(repo_root)
    except ValueError:  # pragma: no cover -- is_relative_to never raises ValueError, defensive only
        in_worktree = False
    resolved_real_path = os.path.relpath(real, repo_root)

    files = list(_iter_pack_files(real))
    total_bytes = sum(f.stat().st_size for f in files)
    if total_bytes > max_pack_bytes:
        raise PackTooLargeError(
            f"pack {name!r} ({source_path}) is {total_bytes} bytes, over the {max_pack_bytes} "
            f"byte --max-pack-bytes limit -- refusing to hash it rather than silently skipping "
            f"it (Decision K); pass a larger --max-pack-bytes if you really want this pack "
            f"included, or omit --with-pack {name!r} to leave it as a declared gap for now"
        )

    content_hash = _pack_content_hash(real, files)

    # The path AS DECLARED (e.g. "data/time", "GMAT R2026a") -- computed with `os.path.relpath`
    # against the *given* `source_path` (never `real`), so a symlinked pack's declared path is
    # its own name at the repo root, not wherever it happens to resolve (that is
    # `resolved_real_path`'s job, just below). `source_path` here is always repo_root-relative-
    # made-absolute by the caller (`build_kit.py`), so this always comes out relative, never
    # absolute -- consistent with Decision L regardless.
    declared_path = os.path.relpath(source_path, repo_root)

    return {
        "name": name,
        "source_path": Path(declared_path).as_posix(),
        "resolved_real_path": Path(resolved_real_path).as_posix(),
        "in_worktree": in_worktree,
        "via_symlink": via_symlink,
        "content_hash": content_hash,
        "file_count": len(files),
        "total_bytes": total_bytes,
    }


# =================================================================================================
# Image digests (Decision J: read the two recorded digests; `collected` reflects the gated step)
# =================================================================================================

def read_recorded_image(component: str) -> dict:
    """`{"tag": ..., "recorded_digest": "sha256:..."}` for `component` (one of
    `sbom.IMAGE_COMPONENTS`), read via `sbom.image_sbom` -- the SAME already-committed,
    already-tested parse of `services/edge-plugin/IMAGE_DIGEST.md` /
    `services/cfs/IMAGE_CONTEXT_MANIFEST.txt` the SBOM generator itself uses
    (`sbom._edge_plugin_image_sbom` / `sbom._cfs_image_sbom`), reused here rather than a second
    regex parser of the same files -- see this task's own acceptance evidence for the direct
    confirmation that `services/cfs/tests/test_image_digest.py`'s own `recorded_digest()` regex
    (`` ```\\nsha256:[0-9a-f]{64}\\n``` ``, first match) reads the identical value out of BOTH
    `IMAGE_DIGEST.md` files today, corroborating this independently."""
    doc = sbom.image_sbom(component)
    meta = doc["metadata"]["component"]
    digest_hex = meta["hashes"][0]["content"]
    return {"tag": meta["version"], "recorded_digest": f"sha256:{digest_hex}"}


def uncollected_images() -> dict:
    """The `images` manifest section when `--with-images` was not requested: every recorded
    digest is still present (Decision J: recording the two `IMAGE_DIGEST.md`s is IN scope even
    when the gated `docker save` step is not run), `collected` is `false`, and the tarball fields
    are explicit `None` rather than simply absent -- so a reader (or `test_images_step_is_gated`)
    never has to distinguish "field omitted" from "field known false"."""
    return {
        component: {
            **read_recorded_image(component),
            "collected": False,
            "tarball_sha256": None,
            "tarball_size": None,
        }
        for component in sbom.IMAGE_COMPONENTS
    }


# =================================================================================================
# Walking a kit root to build the `files` list
# =================================================================================================

#: Path-shape -> role, in first-match order. `build_kit.py` is the only writer of kit content, so
#: this list only ever needs to cover the shapes it actually produces; an unrecognised path is a
#: bug in the builder (a new kind of file added without teaching this classifier about it), so it
#: raises rather than silently falling back to a catch-all role that would hide the omission.
_ROLE_RULES: tuple[tuple[str, str], ...] = (
    ("suite.merged.toml", "suite-merged"),
    ("secsite.merged.toml", "site-merged"),
    ("sbom/SHA256SUMS", "sbom-sums"),
)


def _classify_role(rel_posix: str) -> str:
    for exact, role in _ROLE_RULES:
        if rel_posix == exact:
            return role
    if rel_posix.startswith("sbom/") and rel_posix.endswith(".cdx.json"):
        return "sbom"
    if rel_posix.endswith("IMAGE_DIGEST.md"):
        return "image-digest"
    if rel_posix.startswith("images/") and rel_posix.endswith(".tar"):
        return "image-tarball"
    raise ValueError(
        f"build_manifest: kit file {rel_posix!r} does not match any known role shape -- "
        f"scripts/kit/build_kit.py wrote a file this classifier does not know about yet; teach "
        f"manifest._classify_role about it rather than silently guessing"
    )


def _walk_kit_entries(kit_root: Path):
    """Walks `kit_root` once and yields `(full_path, kind)` for every regular file AND every
    symlink under it -- `kind` is `"file"` or `"symlink"`.

    The ordering trap this exists to avoid (review finding D3-1(b)): a symlink to a regular file
    answers `Path.is_file()` `True` (it follows the link), so a walker that checks `is_file()`
    BEFORE `is_symlink()` treats a symlink exactly like the real file it points at -- hashing the
    TARGET's bytes under the symlink's own path, and never reporting that a symlink was there at
    all. This walker checks `is_symlink()` first, unconditionally, before anything else.

    A symlinked DIRECTORY is reported the same way (as its own `"symlink"` entry) and is never
    descended into -- `dirnames` is pruned before `os.walk` recurses, so nothing on the far side
    of a symlinked directory is ever walked, hashed, or silently treated as this kit's own
    content. `_iter_pack_files` (above) has the identical directory-pruning rule for the same
    reason, applied to pack SOURCE directories rather than the assembled kit."""
    for dirpath, dirnames, filenames in os.walk(kit_root, followlinks=False):
        real_dirnames = []
        for d in sorted(dirnames):
            full = Path(dirpath) / d
            if full.is_symlink():
                yield full, "symlink"
            else:
                real_dirnames.append(d)
        dirnames[:] = real_dirnames
        for fn in sorted(filenames):
            full = Path(dirpath) / fn
            if full.is_symlink():  # checked BEFORE is_file() -- see this function's own doc.
                yield full, "symlink"
            else:
                yield full, "file"


def _walk_kit_files(kit_root: Path) -> list[Path]:
    """Every regular, non-symlink file under `kit_root`, EXCLUDING `KIT_MANIFEST` itself."""
    manifest_path = kit_root / "KIT_MANIFEST"
    return [
        full for full, kind in _walk_kit_entries(kit_root)
        if kind == "file" and full != manifest_path
    ]


def _walk_kit_symlinks(kit_root: Path) -> list[Path]:
    """Every symlink anywhere under `kit_root` (files and directories alike) -- a correctly
    built kit contains none at all (see `DECLARED_GAPS`'s `secdeploy-deploy-assets` entry for
    the one that review finding D3-1(a) found and removed); `verify_manifest` still actively
    checks for one rather than assuming the rule holds."""
    return [full for full, kind in _walk_kit_entries(kit_root) if kind == "symlink"]


def _classify_symlink(kit_root: Path, full: Path) -> tuple[str, str]:
    """`(kind, link_target)` for the symlink at `full` (`kind` is `Finding`'s
    `"unsafe_symlink_target"` or `"unexpected_symlink"`; `link_target` is the raw, unresolved
    `os.readlink` string). Classified PURELY LEXICALLY -- `os.path.normpath` on the target joined
    against the symlink's own parent directory, never `Path.resolve()`/`os.path.realpath()` --
    specifically so a DANGLING symlink (whose target does not exist, e.g. `services/cfs/tests/
    test_image_digest.py`'s own BUILD_ARTIFACT convention would tolerate elsewhere, but a kit
    never should) is classified identically to a real one: this never touches the filesystem
    beyond the one `os.readlink` call on `full` itself."""
    target = os.readlink(full)
    if os.path.isabs(target):
        return "unsafe_symlink_target", target
    joined = os.path.normpath(os.path.join(str(full.parent), target))
    kit_root_str = os.path.normpath(str(kit_root))
    if joined != kit_root_str and not joined.startswith(kit_root_str + os.sep):
        return "unsafe_symlink_target", target
    return "unexpected_symlink", target


# =================================================================================================
# build_manifest / write_manifest / verify_manifest
# =================================================================================================

def build_manifest(
    *,
    kit_root: Path,
    git_commit: str,
    git_dirty: bool,
    git_status: list[str],
    images: dict,
    sboms: dict,
    packs: dict,
) -> dict:
    """Assemble the KIT_MANIFEST dict (Decision L) for the kit already assembled at `kit_root`.

    `files` is derived by walking `kit_root` itself right now (`_walk_kit_files` /
    `_classify_role`) -- so this function's caller (`build_kit.py`) only needs to have already
    written every real file the kit should carry; it never has to hand this function a
    pre-built file list of its own that could drift from what is actually on disk. `images`,
    `sboms`, and `packs` are supplied by the caller (they come from docker/git-adjacent state
    this module deliberately has no dependency on) and are copied into the document verbatim
    (as dicts, so `json.dump`'s own `sort_keys=True` handles their key order). `git_status` is
    stored sorted, verbatim (review finding D3-2: a bare `git_dirty` boolean is permanently
    `true` in this worktree, so the raw `git status --porcelain` lines are what actually let a
    reader tell "the known pre-existing untracked entries" from "a kit built with real
    uncommitted changes" -- see `scripts/kit/README.md`).

    `kit_format` and `gaps` are never parameters -- `KIT_FORMAT` and `DECLARED_GAPS` are this
    deliverable's own fixed facts, not something a caller could vary per kit without silently
    changing what the format promises.
    """
    files = []
    for full in _walk_kit_files(kit_root):
        rel = full.relative_to(kit_root).as_posix()
        stat = full.stat()
        files.append({
            "path": rel,
            "sha256": sbom.sha256_file(full),
            "size": stat.st_size,
            "role": _classify_role(rel),
        })
    files.sort(key=lambda f: f["path"])

    return {
        "kit_format": KIT_FORMAT,
        "git_commit": git_commit,
        "git_dirty": git_dirty,
        "git_status": sorted(git_status),
        "files": files,
        "images": images,
        "sboms": sboms,
        "packs": packs,
        "gaps": [dict(g) for g in DECLARED_GAPS],
    }


def write_manifest(manifest: dict, kit_root: Path) -> Path:
    """Writes `<kit_root>/KIT_MANIFEST`: `json.dump(manifest, indent=2, sort_keys=True,
    ensure_ascii=False)` plus one trailing newline -- Decision L's exact, deterministic format.
    Returns the path written. The manifest's own SHA-256 is deliberately NOT computed or written
    here (Decision L: "the manifest never contains its own hash") -- callers hash the returned
    path's bytes themselves (`sbom.sha256_file(path)` is exactly the right tool, already reused
    throughout this pair of modules)."""
    kit_root.mkdir(parents=True, exist_ok=True)
    out_path = kit_root / "KIT_MANIFEST"
    text = json.dumps(manifest, indent=2, sort_keys=True, ensure_ascii=False) + "\n"
    out_path.write_text(text, encoding="utf-8")
    return out_path


def verify_manifest(kit_root: Path) -> list[Finding]:
    """Re-hashes every file `<kit_root>/KIT_MANIFEST` lists and reports every discrepancy
    (`Finding`, above): `missing_file`, `hash_mismatch`, `size_mismatch` (independent checks;
    both may fire for the same path), `unlisted_file` (a real, non-symlink file present in the
    kit that the manifest's own `files` list never mentions -- checked by diffing the manifest's
    path set against a fresh `_walk_kit_files` walk of `kit_root`, the SAME walker
    `build_manifest` itself uses, so this can never disagree with `build_manifest` about what
    counts as a kit file), and `unexpected_symlink`/`unsafe_symlink_target` (review finding
    D3-1(b): ANY symlink anywhere in the kit, found via `_walk_kit_symlinks` -- a correctly built
    kit contains none, but this checks for real rather than assuming the rule holds). Returns an
    empty list iff the kit is exactly what `KIT_MANIFEST` claims it is. Never raises for a
    tampered/incomplete kit -- that is exactly what the returned findings are for; it only raises
    if `KIT_MANIFEST` itself is missing or is not valid JSON, which is a different, harder
    failure than anything this function's own findings describe."""
    manifest_path = kit_root / "KIT_MANIFEST"
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))

    findings: list[Finding] = []
    listed_paths: set[str] = set()
    for entry in manifest["files"]:
        rel = entry["path"]
        listed_paths.add(rel)
        full = kit_root / rel
        if not full.is_file() or full.is_symlink():
            findings.append(Finding(kind="missing_file", path=rel,
                                     message=f"{rel} is listed in KIT_MANIFEST but is not present in the kit"))
            continue
        actual_sha256 = sbom.sha256_file(full)
        actual_size = full.stat().st_size
        if actual_sha256 != entry["sha256"]:
            findings.append(Finding(
                kind="hash_mismatch", path=rel,
                expected_sha256=entry["sha256"], actual_sha256=actual_sha256,
                message=f"{rel}: KIT_MANIFEST records sha256 {entry['sha256']}, actual is {actual_sha256}",
            ))
        if actual_size != entry["size"]:
            findings.append(Finding(
                kind="size_mismatch", path=rel,
                expected_size=entry["size"], actual_size=actual_size,
                message=f"{rel}: KIT_MANIFEST records size {entry['size']}, actual is {actual_size}",
            ))

    on_disk = {f.relative_to(kit_root).as_posix() for f in _walk_kit_files(kit_root)}
    for rel in sorted(on_disk - listed_paths):
        findings.append(Finding(
            kind="unlisted_file", path=rel,
            message=f"{rel} is present in the kit but KIT_MANIFEST's files list does not mention it",
        ))

    # Review finding D3-1(b): a kit must contain no symlinks at all -- checked for real, not
    # assumed, regardless of whether `files` and `unlisted_file` above already agree.
    for full in _walk_kit_symlinks(kit_root):
        rel = full.relative_to(kit_root).as_posix()
        kind, target = _classify_symlink(kit_root, full)
        findings.append(Finding(
            kind=kind, path=rel, link_target=target,
            message=f"{rel} is a symlink (-> {target}) -- a kit must contain no symlinks at all",
        ))

    return findings
