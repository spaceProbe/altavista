"""scripts/kit/manifest.py -- D3 (docs/p5-plan.md), the KIT_MANIFEST format (Decision L) and
everything needed to build or verify one. Round 1 (P5 track round 1, "the kit builder and its
manifest, without the zero-egress install proof") built `kit_format` 1: pack DESCRIPTORS only
(Decision K), a fixed five-gap list, and a kit that could contain no symlink at all. Round 2 (P5
track round 2, task 3a, lead ruling 214(b)) is this module's own second half: the kit now carries
real bytes for what a zero-egress install actually needs, so `kit_format` is bumped to 2. What
changed and why:

- **A pack can now carry BYTES, not just a descriptor** (`copy_pack_bytes`, below) -- decision 11
  from round 1 ("environment data ships as pack descriptors this round, not as bytes") is
  superseded by 214(b): the kit carries the GMAT install as a hashed data pack, because "the
  kernel cannot run without it; 738 MB is a kit, not a problem". A copied pack lands under
  `packs/<name>/` inside the kit; its regular files fall out of the existing `files` walk for
  free (role `"pack-file"`, `_classify_role` below); its own internal symlinks -- GMAT R2026a
  ships 183 of them, every one measured to be a RELATIVE, same-directory target (e.g. `libwx_osx_
  cocoau_xrc-3.2.0.dylib`), never an absolute one -- are carried verbatim (`os.symlink`, never
  dereferenced: dereferencing would both bloat the kit and lose the install's own structure) and
  recorded in the new top-level `pack_symlinks` list.
- **The "no symlinks in a kit" rule from round 1 is narrowed, not dropped.** A symlink INSIDE a
  copied pack (`packs/<name>/...`) is allowed, but only when it is declared in `pack_symlinks`
  AND its target -- classified exactly like round 1's own `_classify_symlink` (lexical only,
  `os.path.normpath`, never `Path.resolve`/`os.path.realpath`, so a dangling link classifies
  identically to a live one) -- stays relative and lexically inside THAT PACK's own root (not
  merely inside the kit as a whole: a symlink from one pack laterally into another pack's
  directory is still refused). A symlink ANYWHERE ELSE in the kit remains exactly as forbidden as
  round 1 made it -- round 1's own critical review finding ("a kit that carries a symlink to
  anywhere on the build host verifies clean") stays proven false by the unchanged tests plus the
  new ones covering the pack case.
- **The gap list is no longer a fixed constant.** Round 1's `DECLARED_GAPS` named five things this
  half of D3 never carried, unconditionally, in every kit. Round 2 can now actually carry two of
  those five (`cargo-vendor`, `python-wheels`) when their own flags are passed, so a gap that was
  collected must stop being reported as one -- `build_gaps` (below) assembles the list from what
  the caller (`build_kit.py`) actually did, the same pattern `images`/`sboms`/`packs` already use.
  `seccert-root`'s reason is rewritten (round 2 finding, `scripts/edge_local_ca.py`): seccert
  self-issues its own Root/Intermediate the first time it boots, configured entirely by
  environment variables passed to that one process -- there is no committed trust root anywhere
  for a kit to carry, so it stays a gap, honestly, rather than a fabricated CA nobody asked for.
- **New content roles**: `vendor` (`cargo vendor`'s own tree, `--with-vendor`), `vendor-config`
  (the `.cargo/config.toml` fragment `cargo vendor` prints), `wheel` (`--with-wheels`), `binary`
  (a cross-built Linux service binary, `--with-binaries`), `run-fixture` (the recorded kernel
  runs, `tests/fixtures/*.runproducts.bin`, carried unconditionally -- small enough, ~4.2 MB
  measured across the four such fixtures actually present in this tree today, not the five the
  round-2 brief described; see `scripts/kit/build_kit.py::assemble_runs`'s own comment on that
  discrepancy). Three new top-level sections describe what each gated step actually did:
  `vendor`, `wheels`, `binaries` (each `{"collected": bool, ...}`, the same "always present, never
  conditional on what happened" shape `images` already established). `runs` is a fourth new
  top-level section, always present, naming each recorded run fixture's own DRM/provenance hash
  where cheaply readable.
- **P5 track round 2 task 3c**: two new named packs, `web` and `profiles` (the viewer's own
  static assets and profile/policy store -- neither ships in the `altavista` wheel, see this
  task's own defect report), added to `ALWAYS_COPY_PACKS` (below) so every kit copies their real
  bytes unconditionally, never gated behind `--copy-pack` -- an installed viewer cannot start
  without them. `install.py` refuses a kit that somehow lacks either (mirrors its existing
  wheels refusal). `install-path`'s own gap reason (below) is rewritten: task 3b already closed
  most of it (a kit genuinely installs and runs its demo now), so it names only what still
  remains out of scope (standing up a supervised, persistent deployment) rather than the
  now-false claim that installing anything from a kit was entirely someone else's job.

# Decision I -- why the builder only ever collects and hashes (round 1, unchanged in round 2)

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

Round 2 extends this rule to the one place it could not avoid producing something itself: cross-
built Linux service binaries (`--with-binaries`). `build_kit.py`'s own `_cross_build_binaries`
builds each target binary EXACTLY ONCE per commit into a persistent, `.gitignore`d cache
(`.av-test-tmp/kit-binaries-cache/<git_commit>/`) and every kit built at that commit copies the
SAME already-built bytes -- never re-links per kit -- which is what keeps "two kits built from
the same commit have the same manifest hash" true even though a fresh cross-build would not agree
with a previous one (measured, identical finding, one level up: see `build_kit.py`'s own comment
there).

Given both measured facts, this deliverable's headline claim -- "a kit built twice from the same
commit has the same manifest hash" (`tests/test_kit_manifest.py::
test_two_kits_from_the_same_commit_have_the_same_manifest_hash`) -- can only hold if every byte
the kit carries is EITHER a value already recorded in a committed file (`IMAGE_DIGEST.md`, the
committed SBOMs, `SHA256SUMS`), a hash computed OVER an artefact that already exists and is not
being (re)produced as part of assembling the kit, OR (round 2's one addition) a build product that
is produced AT MOST ONCE per commit and then reused byte-for-byte -- never the live, per-call
output of a fresh `cargo build`/`docker build` run freshly for each kit. `scripts/kit/
build_kit.py` (the orchestration layer that uses this module) therefore never invokes either
directly inside `build()`'s own per-kit path: it calls `deploy/secdeploy/merge.py`'s own functions
over already-committed TOML, copies already-committed SBOM/digest files byte-for-byte, runs
`cargo vendor`/`pip download`/`pip wheel` (round 2: genuinely fetches/builds, but these are
already understood to be non-reproducible in the sense that matters here -- their OUTPUT is a set
of already-published, content-addressed wheel/crate files, not a freshly compiled binary; a wheel
downloaded twice is the same bytes, unlike a freshly linked binary), and -- gated, opt-in, off by
default -- runs `docker save` on an already-built image only AFTER confirming that image still
matches its OWN already-recorded digest (never building, tagging, or retagging one), or copies an
already-cross-built binary out of the per-commit cache. This module's own `build_manifest`/
`pack_descriptor`/`copy_pack_bytes` follow the same rule: they hash or copy what is already on
disk, they never write build output of their own.

# Why this is a separate module from the builder

This module's job -- the manifest FORMAT, and how to compute or verify it -- must be importable
and testable with no filesystem side effects of its own beyond reading/writing under a kit root
the caller already chose, and no dependency on docker or git. `scripts/kit/build_kit.py`'s job --
deciding WHAT bytes go into a kit and where they come from (secdeploy's merge.py, the committed
SBOMs, docker, `cargo vendor`, `pip download`) -- is a different concern with a much larger
dependency footprint. Splitting them means a test of the manifest format itself (tamper detection,
determinism, the gap list, the pack-symlink rules) never needs docker, git, cargo, or pip, and a
change to how kits are assembled never has to touch this file. Library only: nothing below prints
or calls `sys.exit`.

# Format (Decision L, round-2-extended)

`KIT_MANIFEST` is JSON, written by `write_manifest` as `json.dump(..., indent=2, sort_keys=True,
ensure_ascii=False)` plus one trailing newline. It contains:

- `kit_format` (int, bumped whenever this shape changes -- 2 as of this round, see this module's
  own top doc for what changed).
- `git_commit` / `git_dirty` / `git_status` -- unchanged from round 1 (see `build_manifest`'s own
  doc).
- `files` -- every regular, non-symlink file actually present under the kit root at the moment
  `build_manifest` runs, EXCLUDING `KIT_MANIFEST` itself. Each entry is `{"path", "sha256",
  "size", "role"}`, sorted by path. Round 2 adds five new roles (`pack-file`, `vendor`,
  `vendor-config`, `wheel`, `binary`, `run-fixture`) to the fixed set `_classify_role` recognises;
  an unrecognised path shape still raises rather than silently guessing.
- `pack_symlinks` -- NEW in round 2: every symlink that lives inside a COPIED pack
  (`packs/<name>/...`), `{"path", "pack", "link_target"}`, sorted by path, `link_target` the raw,
  unresolved `os.readlink` string. This is the one place a kit is allowed to carry a symlink at
  all (see this module's own top doc); `verify_manifest` cross-checks every symlink it finds
  anywhere in the kit against this list, not merely its presence.
- `images` -- unchanged from round 1.
- `sboms` -- unchanged from round 1.
- `packs` -- Decision K's descriptors, extended in round 2 with `"copied"` (bool) and, when true,
  `"copy"` (`{"kit_path", "copied_file_count", "copied_symlink_count", "copied_total_bytes"}`) --
  see `pack_descriptor`/`copy_pack_bytes` below.
- `vendor` -- NEW in round 2: `{"collected", "network_used", "offline_error"}`, always present
  (Decision J's "always present, never conditional on what happened" pattern, matching `images`).
- `wheels` -- NEW in round 2: `{"collected", "network_used", "fetched"}`, `fetched` a list of
  `{"name", "version", "filename", "sha256"}`.
- `binaries` -- NEW in round 2: `{"collected", "results"}`, `results` keyed by binary name,
  `{"included", "reason"}` (`reason` set only when a requested binary did not cross-build).
- `runs` -- NEW in round 2: keyed by run-fixture stem, `{"config_hash", "data_pack_hash",
  "decoded"}` -- see `build_kit.py::assemble_runs` for how these are read.
- `gaps` -- Decision J's declared, out-of-scope items, now assembled by `build_gaps` (below) from
  what the caller actually collected, rather than a fixed constant -- always present, but its
  membership varies with which flags a given kit build used.

No timestamps, no absolute paths, no hostname, no username anywhere in the output -- see
`pack_descriptor`'s own doc comment for how a pack that lives outside this worktree is recorded
without ever writing an absolute path.
"""
from __future__ import annotations

import hashlib
import json
import os
import shutil
from dataclasses import dataclass
from pathlib import Path
from typing import Optional

# scripts/kit/sbom.py is stdlib-only itself (its own module doc says so) and lives in this same
# directory; like sbom.py's own `import licences`, this relies on the caller having put
# scripts/kit on sys.path (true automatically for `python scripts/kit/build_kit.py`, and done
# explicitly by tests/test_kit_manifest.py and tests/test_sbom.py alike) rather than this module
# mutating sys.path itself.
import sbom  # noqa: E402  (see comment above -- caller is responsible for sys.path)

KIT_FORMAT = 2

# --- Round-1 fixed gap reasons, unchanged. Round-2 gaps that can be genuinely collected
# (cargo-vendor, python-wheels) move to `build_gaps` below, which decides per-build whether they
# still apply. -------------------------------------------------------------------------------
_SECCERT_ROOT_REASON = (
    "a deployed kit's trust root is not a static file this repository could honestly ship. "
    "Measured directly (scripts/edge_local_ca.py, tests/test_edge_identity_seccert.py): seccert "
    "(the RFC 8555 ACME CA at /Users/probe/code/secdeploy/work/seccert) self-issues its own Root "
    "and Intermediate the first time its own process boots, configured entirely by SECCERT_* "
    "environment variables passed to that one subprocess -- there is no committed Root/"
    "Intermediate anywhere in this repository, or in secdeploy's own checkout, for a kit to "
    "carry. The real trust root is a property of the INSTALL (the one seccert instance that "
    "install's own bring-up boots), not of the kit; generating one here to fill the slot would "
    "be inventing a new CA nobody asked for and no install would actually trust. Stays a "
    "declared gap with this reason rather than a fabricated file."
)
_INSTALL_PATH_REASON = (
    "P5 track round 2 task 3b (scripts/kit/install.sh/install.py) closed the first half of this "
    "gap: a kit now installs for real -- verify-first, wheels resolved into a fresh venv with "
    "--no-index, every pack/binary/run copied byte-for-byte -- with zero network reachable, and "
    "tests/test_kit_zero_egress_install.py proves the installed tree's own binaries and viewer "
    "start and run the pinned demo end to end from nothing but the kit. What remains out of "
    "scope of a kit (and of install.sh) is turning that installed tree into a STANDING, "
    "supervised deployment: install.sh writes no systemd unit or process-supervisor definition, "
    "never applies suite.merged.toml/secsite.merged.toml to a real secdeploy site (that TOML is "
    "carried, and installed, purely as data -- install.sh never executes it), and brings up no "
    "TLS/seccert root (see seccert-root, above) or reverse-proxy/network wiring. This test's own "
    "demo path starts each service by hand, once, inside a throwaway container, to prove it CAN "
    "run from the installed tree -- wiring that into a persistent, restart-on-failure production "
    "deployment is a deploy/secdeploy-level concern, not a kit's or this installer's job."
)
_SECDEPLOY_DEPLOY_ASSETS_REASON = (
    "deploy/secdeploy/merge.py::merge also writes a `deploy` symlink alongside "
    "suite.merged.toml/secsite.merged.toml, pointing at the BASE secdeploy manifest's own "
    "deploy/ directory -- an ABSOLUTE path into the user's own secdeploy checkout. That "
    "directory is the user's own secdeploy checkout, not ours to bundle, so scripts/kit/"
    "build_kit.py runs merge() into a throwaway staging directory and copies out only the two "
    "TOML files, discarding the symlink -- the deploy/ assets themselves remain out of scope for "
    "this kit, to be installed from the user's own secdeploy checkout (or a separately licensed "
    "copy) as part of the install path (see install-path, above), never carried inside this kit "
    "as a symlink."
)


def _cargo_vendor_reason() -> str:
    return (
        "the offline crate sources `cargo vendor` would produce are not collected by this kit "
        "-- pass --with-vendor to collect them (P5 track round 2 task 3a); off by default so "
        "the default test gate stays fast, and because the vendored tree is hundreds of MB"
    )


def _python_wheels_reason() -> str:
    return (
        "no `pip download`/wheel cache is collected by this kit -- pass --with-wheels to "
        "collect them (P5 track round 2 task 3a); off by default both because it is the one "
        "step that uses the network (question 154's one permitted exception, at kit-build time "
        "only) and because the wheel set is large"
    )


def build_gaps(
    *, vendor_collected: bool, wheels_collected: bool, extra: list[dict] | None = None,
) -> list[dict]:
    """Assembles the manifest's `gaps` list for one kit build: `cargo-vendor` and `python-
    wheels` are included ONLY when the corresponding step did not run (Decision J extended for
    round 2 -- a gap that was actually collected must stop being reported as one); `seccert-
    root`, `install-path`, and `secdeploy-deploy-assets` are unconditional, every kit, exactly as
    round 1 made them. `extra` (round 2: e.g. an `av-command-binary` gap when `--with-binaries`
    was requested but that one binary did not cross-build, or a `wheel:<name>` gap per package
    with no matching platform wheel) is appended after the five, already sorted by the caller so
    the whole list stays deterministic for a fixed set of flags -- the reproducibility test
    (`test_two_kits_from_the_same_commit_have_the_same_manifest_hash`) depends on this list's
    order never varying run to run for the same inputs, since `json.dump`'s own `sort_keys=True`
    sorts each dict's keys but never reorders a list."""
    gaps: list[dict] = []
    if not vendor_collected:
        gaps.append({"name": "cargo-vendor", "reason": _cargo_vendor_reason()})
    if not wheels_collected:
        gaps.append({"name": "python-wheels", "reason": _python_wheels_reason()})
    gaps.append({"name": "seccert-root", "reason": _SECCERT_ROOT_REASON})
    gaps.append({"name": "install-path", "reason": _INSTALL_PATH_REASON})
    gaps.append({"name": "secdeploy-deploy-assets", "reason": _SECDEPLOY_DEPLOY_ASSETS_REASON})
    gaps.extend(extra or [])
    return gaps


# --- Decision K: named packs this round may describe, and (round 2) copy bytes for.
# Repo-relative source paths, exactly as they appear at the repo root -- some are plain in-tree
# directories, some are symlinks to another worktree (see each one's own module-level comment in
# the manager's brief: "GMAT R2026a" is a symlink to another worktree entirely; "third_party/
# {mirrors,cspice,cfs}" are symlinks too). ------------------------------------------------------
PACKS: dict[str, str] = {
    "data-time": "data/time",
    "gmat": "GMAT R2026a",
    "mirrors": "third_party/mirrors",
    "cspice": "third_party/cspice",
    "cfs": "third_party/cfs",
    # P5 track round 2 task 3c: the VIEWER's own static/config assets. Neither is part of the
    # `altavista` wheel (`pyproject.toml`'s `[tool.setuptools.packages.find]` only ever includes
    # `altavista*` -- a `pyproject.toml`/packaging-level gap this task does not own, see
    # tests/test_kit_zero_egress_install.py's own top doc), so a kit must carry them itself as
    # copied packs or an installed viewer has no static root and no profile store to read from.
    "web": "web",
    "profiles": "profiles",
}

#: The one pack every kit DESCRIBES regardless of `--with-pack` (Decision K: "keep the default
#: test set to the small in-tree pack (data/time) so the default gate stays fast"). Distinct from
#: `ALWAYS_COPY_PACKS`, below -- `data-time` is a descriptor-only default, not copied unless
#: `--copy-pack data-time` is also passed.
DEFAULT_PACK = "data-time"

#: Packs whose real BYTES every kit copies unconditionally -- no flag, never gated -- because an
#: installed viewer cannot start without them (see the `"web"`/`"profiles"` entries in `PACKS`,
#: above). Measured cost (task 3c): `web/` is 3.7 MB across 117 regular files and 2 pack-internal
#: symlinks, `profiles/` is 52 KB across 7 files -- both trivial next to the ~738 MB `gmat` pack
#: that DOES stay opt-in, so making these two unconditional does not compromise "the default kit
#: build stays fast" (Decision K). `build_kit.build` unions this into whatever `--copy-pack` the
#: caller also requested, so they cannot be disabled by omission -- only a kit that never runs
#: `build_kit.build` at all (nothing in this repository) could ever lack them.
ALWAYS_COPY_PACKS: tuple[str, ...] = ("web", "profiles")

#: Generous for any small in-tree pack (data/time is ~16 KB), far short of the 738 MB GMAT
#: pack -- so requesting a big pack needs an explicit, larger `--max-pack-bytes` alongside
#: `--with-pack`, never a silent multi-minute hash by default (Decision K).
DEFAULT_MAX_PACK_BYTES = 50_000_000


class PackTooLargeError(RuntimeError):
    """Raised by `pack_descriptor` when a pack's total byte size exceeds `max_pack_bytes` --
    Decision K's "documented `--max-pack-bytes` refusal rather than silently skipping". Raised
    BEFORE any file content is read (only `os.stat` size information is needed to decide this),
    so refusing a 738 MB pack costs a directory walk, never a multi-minute hash."""


class UnsafePackSymlinkError(RuntimeError):
    """Raised by `copy_pack_bytes` (round 2) when a symlink inside the pack's own source tree
    has an absolute target, or a relative target that -- joined lexically against the symlink's
    own parent, `os.path.normpath`, never resolved -- escapes the pack's own root. A pack's own
    symlinks (GMAT R2026a's 183 relative, same-directory dylib links, measured) are carried
    verbatim into the kit, never dereferenced, but only when provably safe; this is raised BEFORE
    any byte of the pack is copied (a full safety pass over every symlink runs first), so a kit
    is never left half-copied."""


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
      `KIT_MANIFEST`'s `files` list does not mention at all.
    - `"unexpected_symlink"` -- a symlink exists somewhere under the kit root that is either (a)
      outside any copied pack at all, or (b) inside a copied pack but not declared in
      `KIT_MANIFEST`'s own `pack_symlinks` list -- in both cases a symlink this kit's own
      manifest does not account for (`link_target` set to the raw, unresolved `os.readlink`
      string).
    - `"unsafe_symlink_target"` -- a symlink whose target is either an ABSOLUTE path, or
      lexically escapes its own safety boundary via `..` (the kit root for a symlink outside any
      pack; that pack's own root for a symlink inside `packs/<name>/...` -- round 2's narrower
      rule) -- reported as its own, more severe kind (`link_target` set), classified purely
      lexically (`os.path.normpath`, never `Path.resolve`/`os.path.realpath`) so a DANGLING
      symlink is classified identically to one that resolves; see `_classify_symlink`. Reported
      even for a symlink that IS declared in `pack_symlinks`, if its on-disk target has since
      become unsafe.
    - `"symlink_target_mismatch"` -- (round 2) a symlink IS declared in `pack_symlinks` and its
      target is safe, but does not match the raw target `KIT_MANIFEST` recorded for it
      (`link_target` the actual on-disk target, `expected_link_target` what the manifest says).
    - `"missing_symlink"` -- (round 2) `KIT_MANIFEST`'s `pack_symlinks` list names a path that is
      no longer a symlink in the kit at all (removed, or replaced by something else).
    """

    kind: str
    path: str
    expected_sha256: Optional[str] = None
    actual_sha256: Optional[str] = None
    expected_size: Optional[int] = None
    actual_size: Optional[int] = None
    link_target: Optional[str] = None
    expected_link_target: Optional[str] = None
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

    `"copied"` is always `False` and `"copy"` always `None` here (round 2): this function only
    ever describes a pack, it never copies bytes -- `copy_pack_bytes` (below) does that, and
    `build_kit.py` overwrites these two fields on the returned dict when it actually calls it."""
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
    # `resolved_real_path`'s job, just above). `source_path` here is always repo_root-relative-
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
        "copied": False,
        "copy": None,
    }


def copy_pack_bytes(name: str, source_path: Path, kit_root: Path) -> dict:
    """Round 2 (lead ruling 214(b), superseding round 1's decision 11): copies a pack's real
    bytes into `<kit_root>/packs/<name>/`, preserving its own internal symlinks verbatim
    (`os.symlink`, never dereferenced -- dereferencing GMAT R2026a's 183-link dylib set would
    both bloat the kit and lose the install's own structure). `source_path` is resolved exactly
    like `pack_descriptor` (its top-level symlink, if any, is followed ONCE to find the real
    tree; `via_symlink` stays `pack_descriptor`'s own business).

    Two-pass, mirroring `pack_descriptor`'s own two-pass shape: PASS 1 walks the whole pack
    (`_walk_kit_entries`, reused here even though its own doc talks about walking an assembled
    KIT -- the algorithm ("is_symlink() checked before is_file(), a symlinked directory reported
    but never descended into") is identical for any root, and this is a pack's SOURCE tree, not
    yet a kit) and classifies EVERY symlink found against the pack's OWN root as the safety
    boundary (`_classify_symlink`, reused exactly as-is: "the target is relative AND, joined
    lexically against the link's own parent, stays inside the pack"). Any unsafe target raises
    `UnsafePackSymlinkError` immediately -- a hard refusal, BEFORE any byte of the pack is copied,
    never a partial copy left half-done. Only once every symlink in the pack passes does PASS 2
    actually copy: `shutil.copy2` for regular files, `os.symlink` for symlinks, with destination
    directories created as needed (`Path.mkdir(parents=True)`) -- a symlinked directory inside the
    pack is never descended into, so nothing on the far side of it is ever part of the copy.

    Returns `{"kit_path", "copied_file_count", "copied_symlink_count", "copied_total_bytes"}` --
    purely descriptive counters for the pack descriptor; the actual file/symlink inventory that
    ends up in `KIT_MANIFEST` comes from `build_manifest`'s own walk of the finished kit
    (`files`/`pack_symlinks`), exactly like every other kit content -- this function never writes
    manifest data itself, only bytes."""
    if not source_path.is_dir():
        raise FileNotFoundError(f"pack {name!r}: {source_path} does not exist or is not a directory")

    real = Path(os.path.realpath(source_path))
    dest_root = kit_root / "packs" / name

    entries = list(_walk_kit_entries(real))  # generic tree walker, reused for a pack SOURCE root

    # PASS 1: every symlink in the pack must be safe, checked before any byte is copied.
    for full, kind in entries:
        if kind != "symlink":
            continue
        rel = full.relative_to(real).as_posix()
        classified_kind, target = _classify_symlink(real, full)
        if classified_kind == "unsafe_symlink_target":
            raise UnsafePackSymlinkError(
                f"pack {name!r}: symlink {rel!r} -> {target!r} is unsafe (an absolute target, or "
                f"one that escapes the pack's own root) -- refusing to copy any bytes for this "
                f"pack. Carried verbatim it would either dangle in an air-gapped enclave or "
                f"silently resolve to whatever happens to live at that path on whatever host "
                f"installs the kit."
            )

    # PASS 2: copy.
    dest_root.mkdir(parents=True, exist_ok=True)
    copied_file_count = 0
    copied_symlink_count = 0
    copied_total_bytes = 0
    for full, kind in entries:
        rel = full.relative_to(real).as_posix()
        dest = dest_root / rel
        dest.parent.mkdir(parents=True, exist_ok=True)
        if kind == "symlink":
            os.symlink(os.readlink(full), dest)
            copied_symlink_count += 1
        else:
            shutil.copy2(full, dest)
            copied_file_count += 1
            copied_total_bytes += dest.stat().st_size

    return {
        "kit_path": Path("packs", name).as_posix(),
        "copied_file_count": copied_file_count,
        "copied_symlink_count": copied_symlink_count,
        "copied_total_bytes": copied_total_bytes,
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
    regex parser of the same files."""
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
    ("vendor/.cargo-config.toml", "vendor-config"),
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
    if rel_posix.startswith("packs/"):
        return "pack-file"
    if rel_posix.startswith("vendor/"):
        return "vendor"
    if rel_posix.startswith("wheels/") and rel_posix.endswith(".whl"):
        return "wheel"
    if rel_posix.startswith("binaries/"):
        return "binary"
    if rel_posix.startswith("runs/") and rel_posix.endswith(".runproducts.bin"):
        return "run-fixture"
    raise ValueError(
        f"build_manifest: kit file {rel_posix!r} does not match any known role shape -- "
        f"scripts/kit/build_kit.py wrote a file this classifier does not know about yet; teach "
        f"manifest._classify_role about it rather than silently guessing"
    )


def _walk_kit_entries(kit_root: Path):
    """Walks `kit_root` once and yields `(full_path, kind)` for every regular file AND every
    symlink under it -- `kind` is `"file"` or `"symlink"`. Reused (round 2) for walking a pack's
    own SOURCE tree in `copy_pack_bytes`, not only an assembled kit -- the algorithm is identical
    for any root.

    The ordering trap this exists to avoid (review finding D3-1(b)): a symlink to a regular file
    answers `Path.is_file()` `True` (it follows the link), so a walker that checks `is_file()`
    BEFORE `is_symlink()` treats a symlink exactly like the real file it points at -- hashing the
    TARGET's bytes under the symlink's own path, and never reporting that a symlink was there at
    all. This walker checks `is_symlink()` first, unconditionally, before anything else.

    A symlinked DIRECTORY is reported the same way (as its own `"symlink"` entry) and is never
    descended into -- `dirnames` is pruned before `os.walk` recurses, so nothing on the far side
    of a symlinked directory is ever walked, hashed, or silently treated as this root's own
    content."""
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
    """Every symlink anywhere under `kit_root` (files and directories alike). As of round 2 a
    correctly built kit may contain some (inside a copied pack, declared in `pack_symlinks`);
    `verify_manifest` cross-checks every one it finds here against that declaration rather than
    assuming any rule holds."""
    return [full for full, kind in _walk_kit_entries(kit_root) if kind == "symlink"]


def _classify_symlink(root: Path, full: Path) -> tuple[str, str]:
    """`(kind, link_target)` for the symlink at `full`, using `root` as the safety BOUNDARY
    (`Finding`'s `"unsafe_symlink_target"` or `"unexpected_symlink"`; `link_target` is the raw,
    unresolved `os.readlink` string). `root` is deliberately a parameter, not always `kit_root` --
    round 2 calls this both with the kit root (a stray symlink anywhere else in the kit) and with
    a single pack's own root inside the kit (a symlink declared in `pack_symlinks`, whose target
    must stay inside THAT pack, not merely inside the kit as a whole).

    Classified PURELY LEXICALLY -- `os.path.normpath` on the target joined against the symlink's
    own parent directory, never `Path.resolve()`/`os.path.realpath()` -- specifically so a
    DANGLING symlink (whose target does not exist) is classified identically to a real one: this
    never touches the filesystem beyond the one `os.readlink` call on `full` itself."""
    target = os.readlink(full)
    if os.path.isabs(target):
        return "unsafe_symlink_target", target
    joined = os.path.normpath(os.path.join(str(full.parent), target))
    root_str = os.path.normpath(str(root))
    if joined != root_str and not joined.startswith(root_str + os.sep):
        return "unsafe_symlink_target", target
    return "unexpected_symlink", target


# =================================================================================================
# build_manifest / write_manifest / verify_manifest
# =================================================================================================

def _pack_symlinks_list(kit_root: Path) -> list[dict]:
    """Every symlink under `packs/<name>/...` in the just-built kit, `{"path", "pack",
    "link_target"}`, sorted by path -- these are the ONLY symlinks a correctly built kit may
    contain (round 2); anything outside `packs/` is never included here, so `verify_manifest`
    still treats it as forbidden exactly like round 1 did."""
    entries: list[dict] = []
    for full in _walk_kit_symlinks(kit_root):
        rel = full.relative_to(kit_root).as_posix()
        parts = Path(rel).parts
        if len(parts) >= 2 and parts[0] == "packs":
            entries.append({"path": rel, "pack": parts[1], "link_target": os.readlink(full)})
    entries.sort(key=lambda e: e["path"])
    return entries


def build_manifest(
    *,
    kit_root: Path,
    git_commit: str,
    git_dirty: bool,
    git_status: list[str],
    images: dict,
    sboms: dict,
    packs: dict,
    vendor: dict,
    wheels: dict,
    binaries: dict,
    runs: dict,
    gaps: list[dict],
) -> dict:
    """Assemble the KIT_MANIFEST dict (Decision L, round-2-extended) for the kit already
    assembled at `kit_root`.

    `files` and `pack_symlinks` are derived by walking `kit_root` itself right now
    (`_walk_kit_files`/`_classify_role`, `_pack_symlinks_list`) -- so this function's caller
    (`build_kit.py`) only needs to have already written every real file/symlink the kit should
    carry; it never has to hand this function a pre-built inventory of its own that could drift
    from what is actually on disk. `images`, `sboms`, `packs`, `vendor`, `wheels`, `binaries`,
    `runs`, and `gaps` are all supplied by the caller (they come from docker/git/cargo/pip-
    adjacent state this module deliberately has no dependency on) and are copied into the
    document verbatim (as dicts/lists, so `json.dump`'s own `sort_keys=True` handles dict key
    order; list order is the caller's own responsibility -- see `build_gaps`'s own doc). `git_
    status` is stored sorted, verbatim (review finding D3-2, round 1).

    `kit_format` is never a parameter -- `KIT_FORMAT` is this deliverable's own fixed fact, not
    something a caller could vary per kit without silently changing what the format promises."""
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
        "pack_symlinks": _pack_symlinks_list(kit_root),
        "images": images,
        "sboms": sboms,
        "packs": packs,
        "vendor": vendor,
        "wheels": wheels,
        "binaries": binaries,
        "runs": runs,
        "gaps": gaps,
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
    kit that the manifest's own `files` list never mentions at all), and (round-2-extended)
    `unexpected_symlink`/`unsafe_symlink_target`/`symlink_target_mismatch`/`missing_symlink` for
    every symlink anywhere in the kit, cross-checked against `pack_symlinks` rather than assumed
    forbidden outright. Returns an empty list iff the kit is exactly what `KIT_MANIFEST` claims
    it is. Never raises for a tampered/incomplete kit -- that is exactly what the returned
    findings are for; it only raises if `KIT_MANIFEST` itself is missing or is not valid JSON,
    which is a different, harder failure than anything this function's own findings describe."""
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

    # Round 2: every symlink anywhere in the kit is cross-checked against `pack_symlinks` (the
    # ONLY declaration that can make one legitimate) rather than being forbidden outright the way
    # round 1 made every symlink. The safety BOUNDARY differs by case: a symlink declared inside
    # a copied pack must stay inside that pack's own root; anything else must stay inside the kit
    # root as a whole (round 1's original, unchanged rule).
    pack_symlinks_by_path = {e["path"]: e for e in manifest.get("pack_symlinks", [])}
    seen_symlink_paths: set[str] = set()
    for full in _walk_kit_symlinks(kit_root):
        rel = full.relative_to(kit_root).as_posix()
        seen_symlink_paths.add(rel)
        parts = Path(rel).parts
        if len(parts) >= 2 and parts[0] == "packs":
            boundary = kit_root / "packs" / parts[1]
        else:
            boundary = kit_root
        kind, target = _classify_symlink(boundary, full)
        if kind == "unsafe_symlink_target":
            findings.append(Finding(
                kind="unsafe_symlink_target", path=rel, link_target=target,
                message=f"{rel} is a symlink (-> {target}) with an absolute or escaping target",
            ))
            continue
        recorded = pack_symlinks_by_path.get(rel)
        if recorded is None:
            findings.append(Finding(
                kind="unexpected_symlink", path=rel, link_target=target,
                message=(
                    f"{rel} is a symlink (-> {target}) that KIT_MANIFEST's pack_symlinks does "
                    f"not declare -- a kit may only carry a symlink declared there"
                ),
            ))
            continue
        if recorded["link_target"] != target:
            findings.append(Finding(
                kind="symlink_target_mismatch", path=rel,
                link_target=target, expected_link_target=recorded["link_target"],
                message=(
                    f"{rel}: KIT_MANIFEST's pack_symlinks records target "
                    f"{recorded['link_target']!r}, actual is {target!r}"
                ),
            ))

    for rel in sorted(set(pack_symlinks_by_path) - seen_symlink_paths):
        findings.append(Finding(
            kind="missing_symlink", path=rel,
            message=f"{rel} is listed in KIT_MANIFEST's pack_symlinks but is not a symlink in the kit",
        ))

    return findings
