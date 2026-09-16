# scripts/kit -- the deployment kit builder (D3)

A "kit" is a directory that collects, in one place, everything a deployment needs that this
repository already builds, records, or commits elsewhere -- the merged secdeploy suite/site
files, the ten CycloneDX SBOMs, the two recorded container-image digests, the recorded kernel
runs, a set of Decision-K "pack" descriptors (environment data such as `data/time`, and, as of
round 2, real pack BYTES when asked for) -- plus a `KIT_MANIFEST` that lists every file (and,
round 2, every pack symlink) the kit carries with its SHA-256, and that can be independently
re-verified later to detect a missing, tampered, or undeclared file.

P5 track round 1 (task D3's first half) covered assembling and self-verifying a kit with
DESCRIPTORS only -- no pack bytes, no vendor tree, no wheels, no binaries. **Round 2 (task 3a,
lead ruling 214(b)) is this file's own second half**: the kit now carries real bytes for what a
zero-egress install actually needs -- a copied GMAT data pack, `cargo vendor`'s crate sources,
the viewer's wheels, and cross-built Linux service binaries -- each behind its own opt-in flag,
off by default so the default test gate stays fast. It still does NOT cover installing one: the
zero-egress install proof itself -- unpacking a kit with the network disabled, running
`install.sh`, and confirming the result is actually usable -- is P5 track round 2 task 3b, a
separate worker's job. See "What's still a declared gap" below for exactly what that means for
what this kit does and does not carry today.

## Building a kit

    scripts/kit/build.sh --out /path/to/kit-output-dir

`build.sh` is a thin wrapper: it locates this worktree from its own path (refusing to run if it
has been copied elsewhere, or is not inside a git worktree at all), then execs `.venv/bin/python
scripts/kit/build_kit.py` with your arguments passed straight through. `--out` must be a new or
empty directory. **Always build under `out/` inside this worktree** (`.gitignore`d, never
committed) -- Colima mounts only the home directory, so a kit built anywhere under `/tmp` or
`/private/var` is invisible to Docker for any step (`--with-images`, `--with-binaries`) that
needs it.

A minimal, always-fast kit:

    .venv/bin/python scripts/kit/build_kit.py --out out/kit/minimal

A full kit -- every gated step, real bytes for everything this round can collect:

    .venv/bin/python scripts/kit/build_kit.py --out out/kit/full \
        --with-images --with-vendor --with-wheels --with-binaries \
        --copy-pack data-time --copy-pack gmat --max-pack-bytes 900000000

(`--max-pack-bytes` must exceed the GMAT pack's own ~738 MB, or that step refuses rather than
silently skipping it.) `--with-wheels`, `--with-binaries` and (only if `cargo vendor --offline`
genuinely fails) `--with-vendor` are the steps that touch the network, and only at kit-build time
(question 154) -- see "Where this uses the network" below.

Flags:

- `--site <path>` -- one of `deploy/secdeploy`'s standalone site files. Defaults to the eval
  site, `deploy/secdeploy/secsite.altavista-eval.toml`.
- `--with-images` -- gated, off by default. Runs `docker save` on the two recorded images
  (`av-edge-plugin:local`, `altavista-cfs-lockstep:local`) into the kit as tarballs, but only
  AFTER comparing each image's live `docker image inspect` digest to its own recorded digest in
  `services/edge-plugin/IMAGE_DIGEST.md` / `services/cfs/IMAGE_DIGEST.md` (question 212) -- a
  mismatch is a hard error, never a tarball saved under the wrong name. Takes the host-wide
  Docker test lock (`altavista.docker_test_lock.lock_docker_tests`) for the whole step.
- `--with-pack <name>` (repeatable) -- include an additional Decision-K pack DESCRIPTOR beyond
  the default, `data-time`. Known packs: `data-time`, `gmat`, `mirrors`, `cspice`, `cfs`
  (`scripts/kit/manifest.py`'s `PACKS`). Only the descriptor (a name, where it resolves, a
  content hash, a file count, a byte count) is recorded. Round 2 task 3c added `web` and
  `profiles` here too (the viewer's own static assets and profile/policy store, copied in EVERY
  kit unconditionally, since neither shipped in the `altavista` wheel); round 3 (question 217(b))
  removed both again -- `pyproject.toml`/`setup.py` now ship both INSIDE the `altavista` wheel
  itself (`altavista/web/`, `altavista/profiles/`), so the kit does not carry either separately
  any more. See `scripts/kit/manifest.py`'s own top doc, "P5 track round 3", for the full
  reasoning and the `kit_format` bump (2 -> 3) it required.
- `--copy-pack <name>` (repeatable) -- **round 2**: copy that pack's real BYTES into
  `<kit>/packs/<name>/`, not merely its descriptor (implies `--with-pack` for that name). The
  pack's own internal symlinks are carried verbatim (never dereferenced) when safe -- see "Pack
  symlinks: carried verbatim when safe, refused at build time when not" below -- and an unsafe
  one is a hard build-time refusal, never a silent copy. Nothing is copied unconditionally any
  more (round 3 removed the `web`/`profiles` special case along with the packs themselves) --
  pass this flag explicitly for any pack you want real bytes for.
- `--max-pack-bytes <n>` -- refuse (never silently skip) any `--with-pack`/`--copy-pack` pack
  whose total size exceeds this. Default 50,000,000 bytes -- comfortably above the small in-tree
  `data-time` pack, well below the ~738 MB `gmat` pack, so a big pack needs an explicit larger
  value to actually be hashed or copied.
- `--with-vendor` -- **round 2**, gated. `cargo vendor --offline` into `<kit>/vendor/`, falling
  back to the network exactly once if that genuinely fails (question 154). Also writes the exact
  `.cargo/config.toml` fragment `cargo vendor` prints, to `<kit>/vendor/.cargo-config.toml`, so
  an offline rebuild can use it directly.
- `--with-wheels` -- **round 2**, gated. Downloads the viewer's real runtime dependency wheels
  (`fastapi`, `uvicorn[standard]`, `websockets`, `numpy`, `protobuf`, and their own transitive
  closure -- 20 packages measured on this tree) at the exact versions this worktree's `.venv` has
  installed, for **linux/aarch64/cp313** (task 3b's own proof platform -- a `python:3.13-slim`
  container, NOT this macOS host's own platform), into `<kit>/wheels/`, plus a wheel of this
  repository's own `altavista` package (`pip wheel .`, since the viewer server is `python -m
  altavista`). **This step always uses the network**, at kit-build time only
  (question 154) -- never triggered by any test without its own opt-in.
- `--with-binaries` -- **round 2**, gated. Cross-builds `av-ingest-server` (crate `av-ingest`)
  and `av-command` (crate `av-command`) for Linux, the identical bind-mounted `docker run` idiom
  `tests/test_edge_plugin_container.py::_cross_build_ingest_server_binary` establishes, into
  `<kit>/binaries/`. Built at most ONCE PER SOURCE STATE into a persistent cache
  (`.av-test-tmp/kit-binaries-cache/<commit>-<short hash of the working tree>/`, `.gitignore`d)
  and reused by every kit built from that same source state -- see "Reproducibility and a
  cross-built binary" below for why, and `build_kit.binary_cache_key` for why the key is not the
  commit alone. **This step uses the network** (the cross-build container installs its build
  dependencies with apt before compiling); `KIT_MANIFEST`'s `binaries.network_used` records it. A binary that does
  not cross-build (this round: `av-command`, pinned toolchain `rust:1.85-bookworm` is older than
  `regorus` 0.12.0's own const-generics requirement -- see the real error in a built kit's own
  `gaps` list) is never silently dropped: it becomes a named gap carrying the real compiler
  error, and the other binary is still collected.

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

- `kit_format` -- an integer, bumped whenever this shape changes. **3 as of round 3** (question
  217(b) removed the `web`/`profiles` packs -- see `scripts/kit/manifest.py`'s own top doc).
- `git_commit` / `git_dirty` / `git_status` -- the full HEAD SHA, whether the tree that built the
  kit was clean, and the sorted, verbatim `git status --porcelain` lines. `git_dirty` alone is a
  permanently-`true` flag IN THIS WORKTREE specifically: it carries pre-existing, untracked
  entries that are not source changes and will never go away (symlinks/checkouts reaching outside
  the worktree, present before P5 started). `git_status` is what lets a reader of a kit built
  here tell "just those known entries" from "someone shipped a kit built from a tree with real
  uncommitted source changes" -- read the actual lines, don't just check the boolean.
- `files` -- every real, non-symlink file in the kit, `{path, sha256, size, role}`, sorted by
  path. Round 2 adds five new roles: `pack-file` (a copied pack's own regular files), `vendor` /
  `vendor-config` (`--with-vendor`), `wheel` (`--with-wheels`), `binary` (`--with-binaries`),
  `run-fixture` (the recorded kernel runs, always present).
- `pack_symlinks` -- **new in round 2**: every symlink inside a COPIED pack (`packs/<name>/...`),
  `{path, pack, link_target}`, sorted by path -- see "Pack symlinks", below. A kit may contain
  ZERO symlinks anywhere else (round 1's original rule, unchanged).
- `images` -- the two recorded image digests, and (only when `--with-images` ran) each tarball's
  own SHA-256/size.
- `sboms` -- component name -> SHA-256, taken directly from the kit's own copy of the committed
  `SHA256SUMS`.
- `packs` -- the Decision-K pack descriptors actually included, each now also carrying `copied`
  (bool) and, when true, `copy` (`{kit_path, copied_file_count, copied_symlink_count,
  copied_total_bytes}`).
- `vendor` -- **new in round 2**: `{collected, network_used, offline_error}`, always present.
- `wheels` -- **new in round 2**: `{collected, network_used, fetched}`, `fetched` a list of
  `{name, version, filename, sha256}`.
- `binaries` -- **new in round 2**: `{collected, network_used, source_state, results}`, `results`
  keyed by binary name, `{included, reason}`. `source_state` is the cache key the bytes came from
  (`<commit>-<short hash of git status --porcelain + git diff HEAD>`), so a kit built from a tree
  with uncommitted changes cannot silently carry a binary compiled from a different one.
- `runs` -- **new in round 2**: keyed by each recorded run fixture's own stem,
  `{config_hash, data_pack_hash, decoded}` -- see "The recorded kernel run", below.
- `gaps` -- what this kit does not carry and why -- see "What's still a declared gap", below.
  Always present, but its MEMBERSHIP now varies with which flags a given kit build used (a
  collected step stops being reported as a gap).

No timestamps, no absolute paths, no hostname, no username anywhere in the document. The
manifest's own SHA-256 is printed by the builder and asserted by tests; it is never written
inside the manifest itself.

## Pack symlinks: carried verbatim when safe, refused at build time when not

Round 1's rule was blunt: a kit must contain NO symlinks at all, ever, anywhere (an early cut of
this builder let `deploy/secdeploy/merge.py::merge` plant a `deploy` symlink pointing at an
ABSOLUTE path into the user's own secdeploy checkout, and the FIRST cut of `verify_manifest` did
not even see it). That defence still stands for everything outside a copied pack.

Round 2 (lead ruling 214(b)) needs to carry `GMAT R2026a` as real bytes, and that install
genuinely ships 183 symlinks -- measured directly, every one a RELATIVE, same-directory target
(`find . -type l -exec readlink {} \;` shows names like `libwx_osx_cocoau_xrc-3.2.0.dylib`; zero
absolute targets). Dereferencing them would both bloat the kit and lose the install's own
structure, so `manifest.copy_pack_bytes` carries them verbatim (`os.symlink`, never followed) --
but only when they are provably safe:

- **Allowed**: the target is relative, and (joined lexically against the symlink's own parent,
  `os.path.normpath`, never `Path.resolve`/`os.path.realpath` -- so a dangling link classifies
  identically to a live one) stays inside THAT PACK's own root. Recorded in `KIT_MANIFEST`'s
  `pack_symlinks` list with its raw `os.readlink` target.
- **Refused, at BUILD TIME, before any byte of the pack is copied**
  (`manifest.UnsafePackSymlinkError`): an absolute target, or a relative one that escapes the
  pack's own root via `..`. Never a partial copy left half-done, never a silent copy of an unsafe
  link.

`verify_manifest` re-checks every symlink it finds anywhere in a built kit -- classified against
the right safety boundary (a pack's own root for one inside `packs/<name>/...`, the kit root for
anything else) regardless of what `pack_symlinks` claims, so a symlink that becomes unsafe after
being declared is still caught (`unsafe_symlink_target`), one whose on-disk target no longer
matches what was recorded is caught (`symlink_target_mismatch`), an undeclared symlink planted
anywhere (inside a pack or not) is caught (`unexpected_symlink`), and a declared one that has
disappeared is caught (`missing_symlink`). Round 1's original defence -- "a kit that carries a
symlink to anywhere on the build host verifies clean" -- stays proven false: every one of round
1's own symlink tests still passes unchanged, plus round 2's new ones for the pack case.

## The recorded kernel run

`tests/fixtures/*.runproducts.bin` is carried into `<kit>/runs/`, byte-for-byte, unconditionally
(no flag -- small enough that the default gate stays fast: measured ~4.2 MB across the four such
fixtures actually present in this tree today -- `demo_attitude_control`, `demo_command_trail`,
`demo_measurements`, `demo_two_instance`). `KIT_MANIFEST`'s `runs` section names each one's own
DRM/provenance hash where cheaply readable: `build_kit.read_run_provenance` walks the committed
`.runproducts.bin`'s protobuf wire format directly (`proto/altavista/v1/run.proto`'s
`RunProducts.provenance` is field 5; `core.proto`'s `Provenance.config_hash`/`data_pack_hash` are
fields 4/5) -- stdlib only, no `av_cdm`/`prost` build needed. Verified against all four real
fixtures while building this task: every one decodes to a well-formed 64-hex-character SHA-256
`config_hash`. If a future fixture cannot be walked this way, `decoded` is `false` and
`config_hash`/`data_pack_hash` are `null` -- the bytes are still carried, never a fabricated
field.

## Verifying a kit

    from manifest import verify_manifest
    findings = verify_manifest(Path("/path/to/kit"))

Re-hashes every file `KIT_MANIFEST` lists and returns a `Finding` for each problem: a listed file
that is missing, one whose content hash or size no longer matches, a real file in the kit that
`KIT_MANIFEST` never mentions at all (a kit carrying something undeclared is exactly as broken as
one missing something, and it is the case people forget), or (see "Pack symlinks" above) any
symlink anywhere in the kit that is not exactly what `pack_symlinks` declares. An empty list means
the kit is exactly what its manifest claims.

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

## Reproducibility and a cross-built binary

The same argument does not, by itself, cover `--with-binaries` either: round 1 measured that
three independent links of identical Rust source produce three DIFFERENT binary hashes on this
host (Mach-O's per-link `LC_UUID`, among other build-environment detail). A `--with-binaries` kit
built twice, each time cross-building fresh, would therefore never be reproducible -- so
`build_kit._cross_build_binaries` builds each target binary AT MOST ONCE PER SOURCE STATE, into a
persistent cache (`.av-test-tmp/kit-binaries-cache/<binary_cache_key(...)>/`, `.gitignore`d, kept
across separate `build_kit.py` invocations, not just within one process), and every kit built from
that same source state copies the SAME already-built bytes rather than re-linking.

**Why the cache key is not the commit alone** (review finding, round 2): a cache keyed on
`git_commit` reuses one binary for every kit built at that commit, *including* kits built from a
tree carrying uncommitted changes to the very sources that binary was compiled from -- the kit
would carry bytes from a different tree state than the one it records, and nothing would say so.
`build_kit.binary_cache_key` therefore folds `git status --porcelain` and `git diff HEAD` in
beside the commit, and `KIT_MANIFEST`'s `binaries.source_state` records the resulting key. What it
does not cover is stated in that function's own doc comment rather than left implied. Measured directly while
building this task: a second `--with-binaries` build at the same commit reused the cache and
finished in well under a second, instead of the ~2 minutes the first (real) cross-build took --
see this task's own report for the exact two-build comparison. This is choice (a) of this task's
own two options ("build it once and reuse it across the two comparison builds") rather than (b)
excluding the field from the reproducibility claim -- the binary's own SHA-256 stays in
`KIT_MANIFEST`'s `files` list either way, since it is the same bytes in both kits.

## Where this uses the network

Question 154: "a kit is built with network once and installed with none." Every step in this
kit builder is offline EXCEPT:

- `--with-wheels` -- always uses the network (`pip download` against PyPI), at kit-build time
  only. `KIT_MANIFEST`'s `wheels.network_used` records this; no test ever runs this path without
  its own explicit opt-in (`AV_KIT_WITH_WHEELS=1`), and it is never triggered by the default
  gate.
- `--with-vendor` -- tries `cargo vendor --offline` first (this host's own `~/.cargo/registry` is
  already populated, ~563 MB measured, so this should need no network at all -- confirmed:
  `vendor.network_used` was `false` when this task actually ran it). Only if that genuinely fails
  does it retry WITHOUT `--offline`, exactly once, and `vendor.network_used` records that this
  happened and why (`vendor.offline_error`).

- `--with-binaries` -- uses the network, at kit-build time only: the cross-build container runs
  `apt-get update` and installs `protobuf-compiler`/`libprotobuf-dev`/`libssl-dev`/`pkg-config`
  before `cargo build` (the crate sources themselves come from this worktree, bind-mounted, with
  `/Users/probe/code/spoore` read-only for the path dependency). `KIT_MANIFEST`'s
  `binaries.network_used` records it. An earlier revision of this README claimed this step had
  "no network of its own", which was simply wrong -- corrected by review rather than left standing.

Nothing else -- `--with-images` only inspects/saves an already-built local image, and every
remaining step reads files already in this worktree.

## What's still a declared gap

`KIT_MANIFEST`'s own `gaps` list names these explicitly, with the reason, rather than shipping an
empty placeholder that would make the kit look more complete than it is. `cargo-vendor` and
`python-wheels` are now CONDITIONAL -- present only when the corresponding flag was not passed;
the rest are unconditional, every kit, regardless of flags:

- **`cargo-vendor`** -- present unless `--with-vendor` was passed.
- **`python-wheels`** -- present unless `--with-wheels` was passed.
- **`seccert-root`** -- the seccert trust root a deployed kit would carry. **Unconditional, even
  with every flag on** -- round 2 finding (`scripts/edge_local_ca.py`,
  `tests/test_edge_identity_seccert.py`): seccert (the RFC 8555 ACME CA at
  `/Users/probe/code/secdeploy/work/seccert`) self-issues its own Root and Intermediate the first
  time its own process boots, configured entirely by `SECCERT_*` environment variables passed to
  that one subprocess. There is no committed trust root anywhere in this repository, or in
  secdeploy's own checkout, for a kit to carry -- the real trust root is a property of the
  INSTALL, not of the kit. Generating one here to fill the slot would be inventing a new CA
  nobody asked for and no install would actually trust, so this stays a declared gap rather than
  a fabricated file.
- **`install-path`** -- **narrowed by task 3c** (`manifest.py`'s own `_INSTALL_PATH_REASON`,
  rewritten): P5 track round 2 task 3b (`scripts/kit/install.sh`/`install.py`) already closed most
  of this -- a kit genuinely installs, with zero network reachable, and
  `tests/test_kit_zero_egress_install.py` proves the installed tree's own binaries and viewer run
  the pinned demo end to end. What remains out of scope of a kit (and of `install.sh`) is turning
  that installed tree into a STANDING, supervised deployment: no systemd unit or
  process-supervisor definition, no application of `suite.merged.toml`/`secsite.merged.toml` to a
  real secdeploy site (carried, and installed, purely as data), no TLS/seccert bring-up (see
  `seccert-root`, above). That remains a deploy/secdeploy-level concern.
- **`secdeploy-deploy-assets`** -- the base secdeploy manifest's own `deploy/` directory (the
  symlink `merge()` writes and this builder discards -- see round 1's own D3-1 finding). Those
  assets are the user's own secdeploy checkout, not ours to bundle.
- **`<binary>-binary`** (e.g. `av-command-binary`) -- present only when `--with-binaries` was
  passed and that specific binary did not cross-build; carries the REAL compiler error, never a
  synthesized message.
- **`wheel:<package>`** -- present only when `--with-wheels` was passed and no matching
  `linux/aarch64/cp313` wheel exists for that package at this worktree's installed version;
  carries what `pip download` actually reported. None of the viewer's own 21-package runtime
  closure hit this on this host as of this task (see the task's own report) -- `numpy` needed
  `manylinux_2_28_aarch64` specifically (it stopped shipping a `manylinux2014` tag), so both tags
  are requested together (`WHEEL_PLATFORM_TAGS`); if a future dependency ships neither, it lands
  here instead of silently falling back to an sdist or a host wheel.

`install-path` and `secdeploy-deploy-assets` belong to task 3b, where a zero-egress install is
what actually proves anything installs and starts services -- this task is the payload and its
manifest only. `seccert-root` is simply not this kit's content to carry, ever, in any round -- it
is generated per-install, never shipped.
