"""scripts/kit/build_kit.py -- D3 first half (docs/p5-plan.md, P5 track round 1): the kit
builder itself. Assembles a kit into `--out <dir>` and writes its `KIT_MANIFEST`
(`scripts/kit/manifest.py`, which owns the manifest FORMAT and its verifier -- see that module's
own top doc for why the two are split).

Decision I (repeated here because it governs every function below, not just the manifest
format): this module never invokes `cargo build` or `docker build`. It calls `deploy/secdeploy/
merge.py`'s own `merge()` over already-committed TOML, copies already-committed SBOM/digest
files byte-for-byte, and -- only when `--with-images` is passed -- runs `docker save` on an
image that is ALREADY built, only after confirming it still matches its own recorded digest.

# What this half assembles (Decision J)

IN, always: the merged suite/site files, the ten committed SBOMs + `SHA256SUMS`, the two
`IMAGE_DIGEST.md` records, the Decision-K pack descriptors (`data/time` by default), the git
commit/dirty state, and `KIT_MANIFEST` itself.

GATED, opt-in, off by default: `docker save` of the two recorded images (`--with-images`); the
GMAT/third_party packs' own content hash (`--with-pack gmat|mirrors|cspice|cfs`, subject to
`--max-pack-bytes`).

OUT, as `manifest.DECLARED_GAPS` names explicitly: cargo-vendored crate sources, Python wheels,
the seccert trust root, and the whole install path -- D3's second half, where the zero-egress
install is what actually proves them.

# Question 199 (no test mutates the process environment)

Nothing below reads or writes `os.environ` at all -- every value this module needs (repo root,
site path, pack names, the images flag) arrives as a function parameter or a CLI argument.
"""
from __future__ import annotations

import argparse
import importlib.util
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

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
    `deploy/secdeploy/merge.py::merge` directly -- Decision I applied to reuse as much as to
    never building: the TOML merge logic lives in exactly one place, D1's own module, never
    reimplemented here.

    Review finding D3-1(a): `merge()` ALSO writes a `deploy` symlink alongside those two files,
    pointing at an ABSOLUTE path into the BASE secdeploy manifest's own `deploy/` directory (the
    user's own secdeploy checkout -- see `merge.merge`'s own doc comment). An earlier cut of this
    builder called `merge()` with `out=kit_root` directly, so that symlink landed inside the kit
    itself: host-specific, air-gap-hostile content (dangling if the kit is carried into an
    enclave; silently resolving to whatever happens to live at that path anywhere else), and one
    a first cut of `verify_manifest` did not even detect (see `manifest._walk_kit_entries`'s own
    doc comment for that half of the fix). `merge()` is D1's own deliverable and is not changed
    here; instead it runs into a throwaway staging directory (auto-removed on exit, whether or
    not `merge()` raises) and only the two TOML files it produces are copied out -- the symlink
    is created in the staging directory and discarded along with it. The `deploy/` assets
    themselves remain a declared gap (`manifest.DECLARED_GAPS`'s `secdeploy-deploy-assets`
    entry), not silently dropped."""
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
    parsed straight out of the just-copied `SHA256SUMS` -- Decision L's "cross-checked against
    the committed SHA256SUMS" is true by construction here: the dict IS the copied file's own
    content, not a second, independently-computed value that could silently disagree with it.
    """
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
        # Mirrors tests/test_sbom.py's own `path.stem.removesuffix(".cdx")` convention for
        # recovering "av-command" out of ".../av-command.cdx.json" (Path.stem only strips the
        # LAST suffix, ".json").
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
    digest (`manifest.read_recorded_image`, itself reusing `sbom.image_sbom`'s already-committed
    parse of `IMAGE_DIGEST.md`/`IMAGE_CONTEXT_MANIFEST.txt` -- never a second parser), compare it
    to the LIVE `docker image inspect` id BEFORE ever running `docker save` (question 212's own
    ordering), and only then save a tarball into `<kit_root>/images/<component>.tar`.

    Held under `altavista.docker_test_lock.lock_docker_tests()` for the whole step -- the SAME
    rule question 207 established for every Docker-gated test in this workspace, applied here
    even though this step creates no new labelled containers of its own: `services/cfs/tests/
    test_image_digest.py::test_image_digest_matches_recorded_value` already takes this same lock
    around a read-only `docker image inspect` sequence for the identical reason (a concurrent
    prune from any other Docker-gated process on this host is a race this step need not run
    concurrently with).

    A digest mismatch is a hard `RuntimeError`, never a silent save of the wrong bits under the
    right name -- the whole point of comparing before saving."""
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
# git state
# =================================================================================================

def git_state(repo_root: Path) -> tuple[str, bool, list[str]]:
    """`(commit, dirty, status_lines)`. Review finding D3-2: `dirty` alone is a permanently-`true`
    flag in THIS worktree (`third_party/mirrors`, `third_party/renode/renode`, `third_party/
    rtems/rtems` are pre-existing untracked symlinks/checkouts that will never go away -- see
    `scripts/kit/README.md`), so `status_lines` (the raw `git status --porcelain` output, one
    entry per line, unsorted here -- `build_manifest` sorts it into the manifest) is what actually
    lets a reader distinguish "the known pre-existing entries" from "a kit built from a tree with
    real uncommitted source changes". `git status --porcelain` paths are already repo-root-relative
    by git's own convention given `cwd=repo_root`, so this introduces no absolute path."""
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
    max_pack_bytes: int = kit_manifest.DEFAULT_MAX_PACK_BYTES,
) -> tuple[Path, str]:
    """Assembles a kit at `out` and writes its `KIT_MANIFEST`. Returns `(manifest_path,
    manifest_sha256)` -- the sha256 is computed AFTER writing (`sbom.sha256_file`, reused rather
    than a second hashing loop), never embedded in the manifest itself (Decision L)."""
    kit_root = out
    if kit_root.exists() and any(kit_root.iterdir()):
        raise RuntimeError(f"--out {kit_root} already exists and is not empty -- pass an empty or new directory")
    kit_root.mkdir(parents=True, exist_ok=True)

    assemble_suite_and_site(repo_root, kit_root, site)
    sboms = assemble_sboms(repo_root, kit_root)
    assemble_image_digest_docs(repo_root, kit_root)

    pack_names = list(dict.fromkeys([kit_manifest.DEFAULT_PACK, *(with_pack_names or [])]))
    unknown_packs = [n for n in pack_names if n not in kit_manifest.PACKS]
    if unknown_packs:
        raise ValueError(f"unknown pack(s) {unknown_packs!r} -- known packs: {sorted(kit_manifest.PACKS)}")
    packs = {
        name: kit_manifest.pack_descriptor(
            name, repo_root / kit_manifest.PACKS[name], repo_root=repo_root, max_pack_bytes=max_pack_bytes,
        )
        for name in pack_names
    }

    images = collect_images(repo_root, kit_root) if with_images else kit_manifest.uncollected_images()

    git_commit, git_dirty, git_status = git_state(repo_root)

    manifest_doc = kit_manifest.build_manifest(
        kit_root=kit_root, git_commit=git_commit, git_dirty=git_dirty, git_status=git_status,
        images=images, sboms=sboms, packs=packs,
    )
    manifest_path = kit_manifest.write_manifest(manifest_doc, kit_root)
    manifest_sha256 = sbom.sha256_file(manifest_path)

    # Belt-and-suspenders (review finding D3-1): a kit this builder just wrote must verify with
    # zero findings, symlinks included -- if it does not, that is this builder's own bug, caught
    # here rather than handed to whoever builds/ships the kit next.
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
        help=f"repeatable: include an additional Decision-K pack beyond the default "
             f"{kit_manifest.DEFAULT_PACK!r}. Known packs: {sorted(kit_manifest.PACKS)}",
    )
    p.add_argument(
        "--max-pack-bytes", type=int, default=kit_manifest.DEFAULT_MAX_PACK_BYTES,
        help=f"refuse (never silently skip) any --with-pack pack whose total size exceeds this "
             f"(default {kit_manifest.DEFAULT_MAX_PACK_BYTES})",
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
        with_images=args.with_images, with_pack_names=list(args.with_pack),
        max_pack_bytes=args.max_pack_bytes,
    )
    print(f"wrote kit to {manifest_path.parent}")
    print(f"wrote {manifest_path}")
    print(f"KIT_MANIFEST sha256: {manifest_sha256}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
