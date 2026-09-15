# scripts/kit -- the deployment kit builder (D3 first half)

A "kit" is a directory that collects, in one place, everything a deployment needs that this
repository already builds, records, or commits elsewhere -- the merged secdeploy suite/site
files, the ten CycloneDX SBOMs, the two recorded container-image digests, and a set of
Decision-K "pack" descriptors (environment data such as `data/time`) -- plus a `KIT_MANIFEST`
that lists every file the kit carries with its SHA-256, and that can be independently
re-verified later to detect a missing, tampered, or undeclared file.

This round (P5 track round 1, task D3's first half) covers assembling and self-verifying a kit.
It does NOT cover installing one: the zero-egress install proof -- unpacking a kit with the
network disabled and confirming the result is actually usable -- is D3's second half. See "The
four declared gaps" below for exactly what that means for what this kit does and does not carry
today.

## Building a kit

    scripts/kit/build.sh --out /path/to/kit-output-dir

`build.sh` is a thin wrapper: it locates this worktree from its own path (refusing to run if it
has been copied elsewhere, or is not inside a git worktree at all), then execs `.venv/bin/python
scripts/kit/build_kit.py` with your arguments passed straight through. `--out` must be a new or
empty directory.

Flags:

- `--site <path>` -- one of `deploy/secdeploy`'s standalone site files. Defaults to the eval
  site, `deploy/secdeploy/secsite.altavista-eval.toml`.
- `--with-images` -- gated, off by default. Runs `docker save` on the two recorded images
  (`av-edge-plugin:local`, `altavista-cfs-lockstep:local`) into the kit as tarballs, but only
  AFTER comparing each image's live `docker image inspect` digest to its own recorded digest in
  `services/edge-plugin/IMAGE_DIGEST.md` / `services/cfs/IMAGE_DIGEST.md` (question 212) -- a
  mismatch is a hard error, never a tarball saved under the wrong name. Takes the host-wide
  Docker test lock (`altavista.docker_test_lock.lock_docker_tests`) for the whole step.
- `--with-pack <name>` (repeatable) -- include an additional Decision-K pack descriptor beyond
  the default, `data-time`. Known packs: `data-time`, `gmat`, `mirrors`, `cspice`, `cfs`
  (`scripts/kit/manifest.py`'s `PACKS`). Only the descriptor (a name, where it resolves, a
  content hash, a file count, a byte count) is recorded -- pack *bytes* are never copied into the
  kit this round (see "The four declared gaps").
- `--max-pack-bytes <n>` -- refuse (never silently skip) any `--with-pack` pack whose total size
  exceeds this. Default 50,000,000 bytes -- comfortably above the small in-tree `data-time` pack,
  well below the ~738 MB `gmat` pack, so a big pack needs an explicit larger value to actually be
  hashed.

Every default is repo-relative; no flag ever needs an absolute path.

## Why the builder never builds anything (Decision I)

Measured directly: three independent `cargo auditable build`s of identical source, back to back,
produced three different SHA-256 hashes for the linked macOS debug binary (Mach-O's per-link
`LC_UUID`, among other build-environment detail the debug profile embeds -- see `scripts/kit/
sbom.py`'s own `rust_binary_sbom` comment, which hit the identical finding). A rebuilt container
image gets a new image id for the same class of reason one level up (`services/cfs/tests/
test_image_digest.py`'s own "not reproducible by construction" finding: cFE bakes a build
timestamp into its config unless the builder pins one, and this repo's Dockerfile does not).

This deliverable's own headline claim -- **a kit built twice from the same commit has the same
`KIT_MANIFEST` hash** -- can only hold if every byte the kit carries is either a value already
recorded in a committed file, or a hash computed over an artefact that already exists, never the
live output of a fresh `cargo build` or `docker build` run as part of assembling the kit. So the
builder only ever *collects and hashes*: it calls `deploy/secdeploy/merge.py`'s own functions
over already-committed TOML, copies the already-committed SBOMs/`IMAGE_DIGEST.md`s byte-for-byte,
and (gated, opt-in) `docker save`s an image that is already built, only after re-confirming it
still matches its own already-recorded digest.

`tests/test_kit_manifest.py::test_two_kits_from_the_same_commit_have_the_same_manifest_hash`
builds two kits into two separate temp directories and asserts their `KIT_MANIFEST` files are
byte-identical -- this is the test that would fail first if that rule were ever broken.

## What `KIT_MANIFEST` contains

JSON (`json.dump(..., indent=2, sort_keys=True, ensure_ascii=False)` plus a trailing newline; see
`scripts/kit/manifest.py`'s own module doc for the full field-by-field description):

- `kit_format` -- an integer, bumped whenever this shape changes.
- `git_commit` / `git_dirty` / `git_status` -- the full HEAD SHA, whether the tree that built the
  kit was clean, and the sorted, verbatim `git status --porcelain` lines. `git_dirty` alone is a
  permanently-`true` flag IN THIS WORKTREE specifically: it carries three pre-existing, untracked
  entries that are not source changes and will never go away --
  `third_party/mirrors`, `third_party/renode/renode`, and `third_party/rtems/rtems` (symlinks/
  checkouts reaching outside the worktree, present before this task started). `git_status` is
  what lets a reader of a kit built here tell "just those three known entries" from "someone
  shipped a kit built from a tree with real uncommitted source changes" -- read the actual lines,
  don't just check the boolean.
- `files` -- every real file in the kit, `{path, sha256, size, role}`, sorted by path. A
  correctly built kit contains ZERO symlinks anywhere (see "No symlinks in a kit", below).
- `images` -- the two recorded image digests, and (only when `--with-images` ran) each tarball's
  own SHA-256/size.
- `sboms` -- component name -> SHA-256, taken directly from the kit's own copy of the committed
  `SHA256SUMS`.
- `packs` -- the Decision-K pack descriptors actually included.
- `gaps` -- the five things named below, always present.

No timestamps, no absolute paths, no hostname, no username anywhere in the document. The
manifest's own SHA-256 is printed by the builder and asserted by tests; it is never written
inside the manifest itself.

## No symlinks in a kit

An early cut of this builder called `deploy/secdeploy/merge.py::merge` with the kit itself as its
`out` directory; `merge()` also writes a `deploy` symlink there, pointing at an ABSOLUTE path into
the *base* secdeploy manifest's own `deploy/` directory (the user's own secdeploy checkout) --
exactly the kind of host-specific, air-gap-hostile content a kit must never carry: dangling if the
kit is carried into an air-gapped enclave, silently resolving to whatever happens to live at that
path anywhere else. Worse, the FIRST cut of `verify_manifest` did not even see it -- its file
walkers skipped every symlink outright rather than reporting one.

Both are fixed now: `build_kit.assemble_suite_and_site` runs `merge()` into a throwaway staging
directory and copies out only the two TOML files it produces, discarding the symlink with the
rest of the staging directory; and `manifest.verify_manifest` actively walks the kit looking for
symlinks (`_walk_kit_entries`, checking `is_symlink()` BEFORE `is_file()` -- a symlink to a file
answers `is_file()` `True` and would otherwise be silently hashed as its target) and reports every
one it finds as its own kind of `Finding` -- `unexpected_symlink` for a relative target that stays
inside the kit, `unsafe_symlink_target` (the more severe kind) for an absolute or `..`-escaping
one, classified purely lexically so a dangling symlink is caught exactly like a live one. A
correctly built kit today contains none at all; `build_kit.build` self-checks this (and every
other `verify_manifest` finding) before returning, as a second line of defence beyond the tests.

## Verifying a kit

    from manifest import verify_manifest
    findings = verify_manifest(Path("/path/to/kit"))

Re-hashes every file `KIT_MANIFEST` lists and returns a `Finding` for each problem: a listed file
that is missing, one whose content hash or size no longer matches, a real file in the kit that
`KIT_MANIFEST` never mentions at all (a kit carrying something undeclared is exactly as broken as
one missing something, and it is the case people forget), or a symlink anywhere in the kit (see
"No symlinks in a kit" above). An empty list means the kit is exactly what its manifest claims.

## `docker save` is measured to be byte-reproducible here -- but that is a measurement, not a promise

Decision I's reproducibility argument does not, by itself, cover the `--with-images` tarballs:
`docker save`'s own output format is not documented to be byte-for-byte deterministic across
runs. Measured directly on this host (`docker --version`: **Docker version 29.6.2, build
dfc4efb1e2**), it is: `tests/test_kit_manifest.py::
test_two_image_bearing_kits_from_the_same_commit_have_the_same_manifest_and_tarball_hashes`
(gated the same way as every other `--with-images` test, `AV_KIT_WITH_IMAGES=1`) builds two
`--with-images` kits and asserts both the two tarball SHA-256s and the two `KIT_MANIFEST` hashes
come out identical. That is a measured property of THIS host and THIS docker version, not a
guarantee this repository controls -- if a future Docker/BuildKit ever makes `docker save`
non-deterministic (embedding a timestamp, say), that test is what will say so first, and the
"images" half of the headline claim would need re-examining at that point.

## The five declared gaps

`KIT_MANIFEST`'s own `gaps` list names these explicitly, with the reason, rather than shipping an
empty placeholder that would make the kit look more complete than it is:

- **`cargo-vendor`** -- the offline crate sources `cargo vendor` would produce.
- **`python-wheels`** -- an offline `pip download`/wheel cache.
- **`seccert-root`** -- the seccert trust root a deployed kit would carry.
- **`install-path`** -- installing anything from a kit at all.
- **`secdeploy-deploy-assets`** -- the base secdeploy manifest's own `deploy/` directory (the
  symlink `merge()` writes and this builder now discards -- see "No symlinks in a kit" above).
  Those assets are the user's own secdeploy checkout, not ours to bundle.

The first four belong to D3's second half, where a zero-egress install is what actually proves a
vendor tree, a wheel cache, or a trust root is complete and correct -- collecting them here, with
no install to test them against, would be a stub pretending to be a deliverable. The fifth is
simply not this kit's content to carry, ever, in any round -- it belongs to the user's own
secdeploy checkout (or a separately licensed copy of it), installed alongside a kit, never
inside one.
