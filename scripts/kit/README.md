# scripts/kit -- the deployment kit builder (D3)

A "kit" is a directory that collects, in one place, everything a deployment needs that this
repository already builds, records, or commits elsewhere -- the merged secdeploy suite/site
files, the ten CycloneDX SBOMs, the two recorded container-image digests, the recorded kernel
runs, a set of Decision-K "pack" descriptors (environment data such as `data/time`, and, as of
round 2, real pack BYTES when asked for), and `docs/kit-operator-supplied.md` (the operator's
account of what the kit does not carry) -- plus a `KIT_MANIFEST` that lists every file (and,
round 2, every pack symlink) the kit carries with its SHA-256, and that can be independently
re-verified later to detect a missing, tampered, or undeclared file.

The kit does not redistribute GPL- or LGPL-licensed code (question 250(a)): the two base images,
GMAT, and any wheel that bundles a GPL-family library are supplied by the operator and pinned
(`docs/kit-operator-supplied.md`, the kit's own `kit-operator-supplied.md`). The kit carries each
image's application layer and a Dockerfile `FROM` the pinned base digest, and the installer builds the
images offline on the operator's base.

P5 track round 1 (task D3's first half) covered assembling and self-verifying a kit with
DESCRIPTORS only -- no pack bytes, no vendor tree, no wheels, no binaries. **Round 2 (task 3a,
lead ruling 214(b)) is this file's own second half**: the kit now carries real bytes for what a
zero-egress install actually needs -- `cargo vendor`'s crate sources, the viewer's wheels, and
cross-built Linux service binaries -- each behind its own opt-in flag,
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

A full kit -- every gated step, real bytes for everything the kit may carry:

    .venv/bin/python scripts/kit/build_kit.py --out out/kit/full \
        --with-images --with-vendor --with-wheels --with-binaries --copy-pack data-time

It carries no GMAT, no base image and no numpy wheel (operator-supplied; `--with-wheels` lists the
wheel in `KIT_MANIFEST.wheels.operator_supplied`). A kit for a site that runs GMAT also records
GMAT's descriptor, which makes the installer require the operator's own GMAT tree
(`--gmat-root`, `--gmat-platform`, and on Linux x86_64 optionally `--gmat-variant ubuntu|rhel7`):

    ... --with-pack gmat --max-pack-bytes 900000000

(`--max-pack-bytes` must exceed the GMAT tree's own ~770 MB, or that step refuses rather than
silently skipping it. The descriptor hashes the tree; none of its bytes land in the kit.) A site that
deploys the object store (Versity Gateway, Apache-2.0; its upstream image has GPL-2.0-only Alpine base layers) or PostGIS adds `--declare-operator-supplied versitygw` / `postgis`. `--with-wheels`,
`--with-binaries` and (only if `cargo vendor --offline`
genuinely fails) `--with-vendor` are the steps that touch the network, and only at kit-build time
(question 154) -- see "Where this uses the network" below.

Flags:

- `--site <path>` -- one of `deploy/secdeploy`'s standalone site files. Defaults to the eval
  site, `deploy/secdeploy/secsite.altavista-eval.toml`.
- `--with-images` -- gated, off by default. **Question 250(a): carries each image's APPLICATION
  LAYER, not the image.** For the two recorded images (`av-edge-plugin:local`,
  `altavista-cfs-lockstep:local`) it compares the live `docker image inspect` id to the recorded id
  in `services/edge-plugin/IMAGE_DIGEST.md` / `services/cfs/IMAGE_DIGEST.md` (question 212), then
  reads ONLY the runtime-content set's files out of the image (`docker create`, never started;
  `docker cp`; `docker rm -v`), recomputes the runtime-content hash (question 185) and refuses
  unless it equals the recorded one, and writes `images/<component>/` -- see "Image application
  layers" below. No `docker save`, no tarball, no base-image layer. Takes the host-wide Docker
  test lock (`altavista.docker_test_lock.lock_docker_tests`) for the Docker part; may fetch one
  pinned `.deb` first (the network note below).
- `--with-pack <name>` (repeatable) -- include an additional Decision-K pack DESCRIPTOR beyond
  the default, `data-time`. Known packs: `data-time`, `gmat`, `mirrors`, `cspice`, `cfs`
  (`scripts/kit/manifest.py`'s `PACKS`). Only the descriptor (a name, where it resolves, a
  content hash, a file count, a byte count) is recorded. For `gmat` this is the only way in (see
  "GMAT is operator-supplied") and the entry also carries an `operator_supplied` marker. Round 2 task 3c added `web` and
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
  pass this flag explicitly for any pack you want real bytes for. **`--copy-pack gmat` is refused**
  (question 250(a), task 2; exit 2, nothing written, the message names `--with-pack gmat`): GMAT is
  operator-supplied. Refused rather than accepted as a synonym because the flag's name promises
  bytes, and a build script that still passes it would otherwise get a kit without GMAT and no
  failure at the point where it matters.
- `--max-pack-bytes <n>` -- refuse (never silently skip) any `--with-pack`/`--copy-pack` pack
  whose total size exceeds this. Default 50,000,000 bytes -- comfortably above the small in-tree
  `data-time` pack, well below the ~770 MB `gmat` tree, so recording GMAT's descriptor (which hashes
  the tree; no byte of it is copied) needs an explicit larger value.
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
  (question 154) -- never triggered by any test without its own opt-in. **Question 250(a)**: every
  wheel it fetches is opened and the native libraries it bundles are inventoried into
  `KIT_MANIFEST.wheels.fetched[*].bundled_libraries`, each with sha256, size, licence and the line
  range of the wheel's own licence text that states it -- see "Native libraries inside wheels" below.
- `--with-binaries` -- **round 2**, gated. Cross-builds `av-ingest-server` (crate `av-ingest`)
  and `av-command` (crate `av-command`) for Linux, the identical bind-mounted `docker run` idiom
  `tests/test_edge_plugin_container.py::_cross_build_ingest_server_binary` establishes, into
  `<kit>/binaries/`. Built at most ONCE PER SOURCE STATE into a persistent cache
  (`.av-test-tmp/kit-binaries-cache/<commit>-<short hash of the working tree>/`, `.gitignore`d)
  and reused by every kit built from that same source state -- see "Reproducibility and a
  cross-built binary" below for why, and `build_kit.binary_cache_key` for why the key is not the
  commit alone. **This step uses the network** (the cross-build container installs its build
  dependencies with apt before compiling); `KIT_MANIFEST`'s `binaries.network_used` records it. A
  binary that does not cross-build is never silently dropped: it becomes a named gap carrying the
  real compiler error, and the other binary is still collected. **Round 3 (question 217(a))**:
  `PREBUILD_BASE_IMAGE` moved `rust:1.85-bookworm` -> `rust:1.90-bookworm` -- the workspace's own
  `rust-version` floor is `1.87` (`regorus` 0.12.0's own MSRV), so 1.85 could not build EITHER
  binary at all, not just `av-command`; every other cross-build pin in this workspace had already
  moved to 1.90 (question 215), just not this file's own constant. `av-command` cross-builds
  cleanly with the new pin -- see `tests/test_kit_zero_egress_install.py`'s own B.4e for the proof
  that the resulting binary genuinely runs, not merely that it exists.

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
and (gated, opt-in) reads the runtime-content files out of an image that is already built, only
after re-confirming it still matches its own already-recorded digest and runtime-content hash.

`tests/test_kit_manifest.py::test_two_kits_from_the_same_commit_have_the_same_manifest_hash`
builds two kits into two separate temp directories and asserts their `KIT_MANIFEST` files are
byte-identical -- this is the test that would fail first if that rule were ever broken.

## What `KIT_MANIFEST` contains

JSON (`json.dump(..., indent=2, sort_keys=True, ensure_ascii=False)` plus a trailing newline; see
`scripts/kit/manifest.py`'s own module doc for the full field-by-field description):

- `kit_format` -- an integer, bumped whenever this shape changes. **5 as of question 250(a)**
  (the `images` section carries application layers instead of tarballs, 4 -> 5; before that,
  question 217(b) removed the `web`/`profiles` packs at 2 -> 3 and question 217(c) required the
  kit to carry its own installer at 3 -> 4 -- see `scripts/kit/manifest.py`'s own top doc and "The
  kit carries its own installer" below). The installer refuses any other value.
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
  `run-fixture` (the recorded kernel runs, always present). Round 3 adds a sixth, `installer`
  (question 217(c)) -- every file under `installer/` (including `operator_supplied.json`, the pins
  of what the operator supplies), always present, unconditional. Question 250(a) adds
  `image-dockerfile`, `image-rootfs`, `image-package` and `image-package-licence` (all under
  `images/`, `--with-images` only) and retires `image-tarball`. Question 250(a), task 6, adds
  `operator-doc`: the one file `kit-operator-supplied.md` at the kit root, always present (below).
- `pack_symlinks` -- **new in round 2**: every symlink inside a COPIED pack (`packs/<name>/...`),
  `{path, pack, link_target}`, sorted by path -- see "Pack symlinks", below. A kit may contain
  ZERO symlinks anywhere else (round 1's original rule, unchanged).
- `images` -- per image component (`edge-plugin-image`, `cfs-image`), always present:
  `{tag, recorded_digest, collected, runtime_content_hash, runtime_set, base, config, dockerfile,
  files, packages, network_used}`. `recorded_digest` is the recorded image id; `runtime_content_hash`
  the recorded hash of what the image adds over its base (question 185); `runtime_set` the roots
  that hash is taken over (`{files, trees, packages}`, `image_layer.RUNTIME_SETS`), which the
  installer's image step reads out of the image it builds; `base` the pinned operator-supplied base
  (`{id, item, reference, digest}`, from `operator_supplied.json`); `config` the recorded image's
  `.Config` subset (`image_layer.CONFIG_KEYS`) the built image is compared with (`null` when
  `--with-images` did not run). When `--with-images` ran,
  `dockerfile` is `{kit_path}`, `files` lists every runtime-content-set file as `{image_path,
  kit_path, sha256, mode, size, source}` (`mode` an octal string; `source` is `kit` or
  `package:<name>`, whose `kit_path` is null because its bytes are in the `.deb`), and `packages`
  lists each carried `.deb` as `{name, version, architecture, filename, kit_path, sha256, size,
  snapshot_url, licence_fields, licence_kit_path, copyright_sha256, dpkg_excluded_files}`;
  otherwise those three are `null`/`[]`. `network_used` is a fact about the step (the edge-plugin
  image carries a `.deb` fetched at kit-build time), never about whether this run hit the network.
- `sboms` -- component name -> SHA-256, taken directly from the kit's own copy of the committed
  `SHA256SUMS`.
- `packs` -- the Decision-K pack descriptors actually included, each now also carrying `copied`
  (bool) and, when true, `copy` (`{kit_path, copied_file_count, copied_symlink_count,
  copied_total_bytes}`). `gmat` has `copied: false` always and an `operator_supplied` key
  (`{component, doc, item, reason, pins_file, measured_on_build_host, platforms_matching_this_descriptor}`;
  see "GMAT is operator-supplied").
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

Round 2 (lead ruling 214(b)) carried `GMAT R2026a` as real bytes (question 250(a), task 2, stopped
that: GMAT is operator-supplied now; the rule below still governs every other copied pack), and that
install genuinely ships 183 symlinks -- measured directly, every one a RELATIVE, same-directory target
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

## The kit carries its own installer (round 3, question 217(c))

Every kit now includes `<kit>/installer/`: `install.sh`, `install.py`, and the exact local modules
`install.py` imports -- `manifest.py`, `sbom.py`, `licences.py` -- copied verbatim
(`build_kit.py::assemble_installer`, `build_kit.py::INSTALLER_SOURCE_FILES`), unconditionally,
every kit, no flag. Before this round, the zero-egress install proof (`tests/
test_kit_zero_egress_install.py`) bind-mounted THIS repository's own `scripts/kit/` (read-only)
into the installing container alongside the kit itself -- round 2's own decision 9 recorded that
as an open gap: a party holding only the kit, never this repository, could not have run that exact
install. That test now runs `/kit/installer/install.sh /kit /target` from the read-only kit mount
alone, and positively asserts (`_assert_no_repo_source_mounted`) that no container mount points
anywhere inside this repository's own source tree.

This is a real `kit_format` bump, 3 -> 4 (`manifest.py`'s own top doc, "P5 track round 3, question
217(c)"): the installer's presence is now REQUIRED, not merely additive. `install.py` never
imports `build_kit` (which pulls in `packaging`, a third-party dependency -- question 217(e)), so
the bundled installer stays importable on a target machine with nothing but a Python interpreter
and the standard library, exactly as it needs to be to run on a bare `python:3.13-slim` container
that has never seen this worktree at all.

**What this proves, and does not prove.** `install.py`'s own verify-first step re-hashes every
file `KIT_MANIFEST` lists, including the installer's own bundled copy of itself, before trusting
any of it -- that proves internal CONSISTENCY (nothing missing, nothing added, nothing corrupted
in transit). It does NOT prove AUTHENTICITY: whoever can tamper with a kit's files can equally
recompute `KIT_MANIFEST` (and the installer that checks it) to match, since both come from the
same untrusted source -- a self-verifying installer cannot verify itself into legitimacy. The one
real anchor of trust is `KIT_MANIFEST`'s own SHA-256, computed and recorded INDEPENDENTLY of the
kit by whoever built or received it (this task's own report, and this test's own printed/asserted
hash, are exactly that) -- comparing that independently-held hash before ever running the kit's
installer is what a real deployment still needs to add on its own side.

## The kit carries the document of what the operator brings (question 250(a), task 6)

`docs/kit-operator-supplied.md` is copied byte for byte to `<kit>/kit-operator-supplied.md`, beside
`KIT_MANIFEST` (`build_kit.py::assemble_operator_doc`), unconditionally, every kit. It is listed in
`KIT_MANIFEST.files` with the role `operator-doc` and hashed like every file, so `verify_manifest` catches
a changed or missing copy. A kit that tells the operator nothing about what to bring (the base images,
GMAT, the numpy wheel, ...) is incomplete, so the installer refuses a kit whose `files` do not list it
(`operator-doc-missing`, exit 2, before the target directory exists), checks that before the wheels,
prints its path as the first line of both `install.sh <kit> <target>` and `install.sh
--check-operator-supplied <kit>`, and records it in `INSTALL_RECORD.installed.operator_supplied_document`
(kit path, installed path, sha256). The image step does not read it.

`kit_format` stays 5. The top-level shape of `KIT_MANIFEST` is unchanged (one more entry in `files`), format
5 is still the unmerged question-250(a) round (`develop` is at 4), and the refusal names the missing document,
so a format-5 kit built a commit earlier fails with a message that says what to rebuild. A proof kit built
before this change fails the zero-egress proof's staleness check by name
(`tests/test_kit_zero_egress_install.py::_proof_kit_missing_sections`) and has to be rebuilt.
Tests: `tests/test_kit_manifest.py` (present, listed with the role, hashed, byte-identical to the committed
document, tamper findings), `tests/test_kit_install.py` (the printed first line, the record, the refusal for a
consistent kit without it, the tamper refusals).

## GMAT is operator-supplied (question 250(a), task 2)

GMAT R2026a bundles OpenSceneGraph and wxWidgets (LGPL), so the kit never carries it
(`docs/kit-operator-supplied.md` item 3). `--with-pack gmat` records its pack descriptor and the
`operator_supplied` marker in `packs.gmat`; no file lands under `packs/gmat` and `copied` stays
`false`. The installer verifies the operator's own copy against a per-platform pin.

**The pin** is the `gmat-r2026a` component of `scripts/kit/operator_supplied.json` (carried in every
kit as `installer/operator_supplied.json`): `platforms` maps a platform key to a pin or to `null`
("no pin recorded for this platform yet"), and `scope` says what the measurement leaves out. A pin
holds `content_hash`, `file_count`, `total_bytes` (the fields `pack_descriptor` computes) and
`symlink_count`, `symlink_listing_hash` (below). The key is `<os>-<uname -m>`: `macos-arm64` and
`linux-x86_64` are pinned; `linux-aarch64` is `null` because NASA publishes no Linux aarch64 build of
R2026a (question 254, task 12). `linux-x86_64` is NASA's `gmat-ubuntu-x64-R2026a.tar.gz` from SourceForge,
unpacked, and `--gmat-root` is its `GMAT/R2026a` directory; the component's `archives` object records
each Linux archive's URL, mirror, size, sha256 and SourceForge's checksums.

*Variants* (question 257). `variants` maps a platform with more than one pinned release to
`{default, pins}`: `linux-x86_64` has `ubuntu` (pin key `linux-x86_64`, the default) and `rhel7` (pin
key `linux-x86_64-rhel7`, NASA's `gmat-rhel7-x64-R2026a.tar.gz`, pinned from question 254's
measurement). The default variant's pin is always the platform's own key, so `platforms` read alone
gives the default; a non-default variant's key is a pin, not a platform (`manifest.variant_problems`
checks the table, and the pins file is refused as `gmat-pin-malformed` / `pins-malformed` when it is
wrong).

**What is measured** (`manifest.measure_tree(root, scope)`, the one function the builder and the
installer both call): `pack_descriptor`'s walk and hash over regular files, minus the scope.

- *Scope.* `output/`, `bin/api_startup_file.txt` and every `__pycache__/` directory are left out.
  Measured on the GMAT install of this host, the whole-tree descriptor is not stable: GMAT rewrites
  `output/GmatLog.txt` on every run (3,969 bytes on 2026-10-17; the value recorded in
  `docs/kit-operator-supplied.md` item 3, 768,931,073 bytes, was taken before a run changed it, and
  `pack_descriptor` now reads 768,932,620 bytes and hash `sha256:6eb7d09b...`), the Python API leaves
  bytecode in `bin/gmatpy/**/__pycache__/`, and `api/BuildApiStartupFile.py` generates
  `bin/api_startup_file.txt` with the install's absolute path in it, so the same release unpacked
  elsewhere hashes differently. The twelve files outside the scope are exactly the files of the tree
  newer than the release (`find . -type f -newermt 2026-03-27`). Everything else, including the 13
  `.DS_Store` files the macOS archive ships, is inside it.
- *Symlinks.* `pack_descriptor` skips symlinks (`_iter_pack_files`), so its hash cannot see GMAT's
  183 links (library names such as `libOpenThreads.21.dylib` -> `libOpenThreads.3.3.1.dylib`): a
  retargeted link changes nothing it computes. The pin therefore also records the count and a hash of
  the listing (`symlink_listing_hash`: one line per link, `json.dumps([path, target])`, sorted by
  path; targets are read with `readlink`, never followed). The recorded macOS pin of the other three
  fields is unaffected by this addition.
- *Not covered* (`not_covered` in the pin component and in `INSTALL_RECORD`): file modes, ownership,
  timestamps, extended attributes and empty directories; the scope's files; and any change after the
  install-time check (the generated startup file lists the plugins GMAT loads; it is the operator's
  to generate and is not verified).

**The builder.** `--copy-pack gmat` is refused (above). `--with-pack gmat --max-pack-bytes N` (N above
the tree's ~770 MB) records `packs.gmat` as `pack_descriptor` returns it for the build host's tree plus
`operator_supplied.measured_on_build_host` (the scoped measurement) and
`platforms_matching_this_descriptor` (the pinned platforms whose pin equals it; empty when the build
host's GMAT matches none, which does not fail the build).

**The installer.** When the kit has `packs.gmat`:

    <kit>/installer/install.sh <kit> <target> --gmat-root PATH --gmat-platform macos-arm64
    <kit>/installer/install.sh <kit> <target> --gmat-root PATH --gmat-platform linux-x86_64 --gmat-variant rhel7

`--gmat-root` is the operator's unpacked GMAT R2026a, `--gmat-platform` is its platform key; neither
has a default and the platform is not guessed from the installing host, because the installer also
runs in a Linux container over a macOS tree (question 217), where the host says nothing about the
tree. Refusals (exit 2, before the target directory is created; `OperatorSuppliedRefused.code`):
`gmat-root-missing` (names every pinned platform with hash, files, bytes), `gmat-root-not-a-directory`,
`gmat-platform-missing`, `gmat-no-pin-for-platform` (lists the platforms that have a pin),
`gmat-root-unreadable`, `gmat-descriptor-differs` (expected and found hash, files, bytes, symlinks and
which fields differ), `gmat-kit-not-operator-supplied` (a `packs.gmat` without the marker, i.e. a kit
built before this change) and `gmat-pin-malformed`. On a platform with variants (question 257),
`--gmat-variant NAME` chooses the pin (`ubuntu` when it is not given on `linux-x86_64`) and the tree is
compared with that pin only: a tree that matches another variant's pin is `gmat-matches-other-variant`
(naming the variant it matches and the option to pass), one that matches none stays
`gmat-descriptor-differs`. `--gmat-variant` on a platform with one pin is `gmat-variant-not-applicable`,
an unknown name `gmat-variant-unknown`, and a variant's pin key given as `--gmat-platform`
(`linux-x86_64-rhel7`) `gmat-platform-is-a-variant`; none of these reads the tree. A match is recorded in
`INSTALL_RECORD.installed.operator_supplied.gmat` (platform, `variant`, `variant_chosen_by`
(`default`, `option`, or null without variants), `pin_key`, root exactly as given, `found`, `pin`,
`scope`, `not_covered`) and nothing is copied; the operator points `GMAT_ROOT` at that directory.
A kit without `packs.gmat` does not need GMAT and the record says `required: false`.
`INSTALL_RECORD.installed.packs` lists packs whose bytes were copied; `packs_descriptor_only` the rest.

**Adding a pin for another platform** (a kit built for Linux, say): unpack the GMAT R2026a release
for that platform, then

    .venv/bin/python scripts/kit/build_kit.py --out out/kit/gmat-linux --with-pack gmat --max-pack-bytes 900000000

on a host where `GMAT R2026a` at the repository root resolves to that tree (or, without a kit,
`manifest.measure_tree(Path(root), scope)` with the component's `scope`), take the five fields from
`packs.gmat.operator_supplied.measured_on_build_host`, and replace that platform's `null` in
`operator_supplied.json` with them. Rebuild the kit (the pins file is part of it) and run the
installer against that tree with `--gmat-platform <key>` before relying on the pin; the pin is only
as good as the release it was measured from, so measure the unpacked release, not an install that has
been run (the scope covers what a run writes, not everything an operator might change).

Re-measuring a recorded pin needs only the unpacked tree: `AV_KIT_MEASURE_GMAT=1 GMAT_ROOT=<macOS tree>`,
`AV_KIT_MEASURE_GMAT_LINUX=1 GMAT_LINUX_ROOT=<GMAT/R2026a of the Ubuntu archive>` and
`AV_KIT_MEASURE_GMAT_RHEL7=1 GMAT_RHEL7_ROOT=<GMAT/R2026a of the RHEL 7 archive>` run the three opt-in
tests in `tests/test_kit_install.py`. The Linux check runs in a Linux container as well
(`--check-operator-supplied <kit> --gmat-root /gmat --gmat-platform linux-x86_64`, `--network none`, the kit
and the tree mounted read-only); Colima's VM is aarch64, so the x86_64 tree is measured and never run.

## Every operator-supplied component is verified and listed (question 250(a), task 3)

`scripts/kit/operator_supplied.json` pins all seven components of `docs/kit-operator-supplied.md` that
have a pin: the two base images (items 1 and 2, `container-base-image`), GMAT (item 3, `data-tree`, per
platform), the three service images (items 4 to 6, `container-image`: ClickHouse, Versity Gateway, PostGIS) and the
numpy wheel (item 7, `python-wheel`). Item 8, Python >= 3.11 on the installing host, is a version floor
that `install.sh` enforces before anything else; it has no pin and is not in the list. Each entry records
its doc item, kind, the licences that make it operator-supplied, and `verification` (which step checks it
and how). `tests/test_kit_operator_supplied.py` fails if the pins file and the doc's table disagree on
any digest, on the GMAT pin's fields and scope, or on item 7's file and sha256, or if a service pin differs
from the digest in `services/<store|catalog|clickhouse>/IMAGE_DIGEST.md`.

**What the kit needs** is derived from `KIT_MANIFEST` in one place, `operator_check.requirement_for`: a
base image for each image the kit carries (`images.<c>.collected`), GMAT when `packs.gmat` is present, the
wheel when `wheels.operator_supplied` names it, a service image only when
`KIT_MANIFEST.operator_supplied_declared` names it. That list is filled by

    .venv/bin/python scripts/kit/build_kit.py --out DIR --declare-operator-supplied versitygw --declare-operator-supplied postgis

for a site that deploys the heavy track (default: empty). The builder refuses a name that is not a service
image in the pins file, a base or GMAT (those follow from what the kit carries), and `clickhouse`, which
is held out of the kit pending the user's licence review (question 247); the installer refuses a
manifest that declares any of them.

**Two steps, one list.** The files install never holds the Docker socket, so it cannot look at an image,
and the host-side image step reads no tree. Each prints the whole list, one line per component, always
all seven, in item order (the examples are a selection):

    operator-supplied: ubuntu-22.04-base-image  required (base of cfs-image)  verified sha256:2edbbc5dc405… via Id
    operator-supplied: gmat-r2026a  required (packs.gmat)  verified macos-arm64 sha256:d21cddbbffbd… 2060 files
    operator-supplied: versitygw  required (declared at build time: --declare-operator-supplied versitygw)  verified by the image step, not here (record: IMAGE_BUILD_RECORD operator_supplied; this step does not read it)
    operator-supplied: clickhouse  outside the kit, not verified (not declared; held pending licence review, q247)
    operator-supplied: postgis  outside the kit, not verified (not declared; a site that deploys it builds the kit with --declare-operator-supplied postgis)
    operator-supplied: debian-bookworm-slim-base-image  not required (the kit carries no application layer for edge-plugin-image), not verified
    operator-supplied: numpy-wheel-linux-aarch64  not required (no wheel of the kit bundles a GPL-family library), not verified

(a kit that lists numpy in `wheels.operator_supplied` has `required (wheels.operator_supplied: <filename>)`
on that line, and `verified <filename> sha256:c76d5dde9f44…` in the files install; the files install also
prints `operator-supplied components document: <kit>/kit-operator-supplied.md` before the list; digests shortened to 12 hex digits in the line, whole in the records). To read the two outputs together:
every line that says `required` must say `verified ...` in exactly one of the two outputs and
`verified by the ... , not here` in the other. `NOT VERIFIED [code]` is a refusal: that step printed the
lines up to and including it and exited 2, so the operator sees what was found before the refusal. The
structured form of each step's list is `INSTALL_RECORD.installed.operator_supplied.components` (the
earlier `.gmat` entry is kept) and `IMAGE_BUILD_RECORD.operator_supplied.components`; neither step reads
the other's record, because the two run at different times and, usually, on different machines.

Where each is checked: trees (GMAT) by `install.sh <kit> <target> --gmat-root ... --gmat-platform ... [--gmat-variant ...]`, or
without installing by `install.sh --check-operator-supplied <kit> [--gmat-root ... --gmat-platform ... [--gmat-variant ...]]`;
container images by `install.sh --build-images <kit>` (the printed list comes before any build) or
`install.sh --probe-offline-build <kit>`. An image is accepted only if `docker image inspect <reference>`
reports the pinned digest as its `Id` or in `RepoDigests`; absent or different is `base-image-missing` /
`base-image-digest-differs` for a base and `operator-image-missing` / `operator-image-digest-differs` for
a declared service image, each naming reference and digest. Nothing is pulled or loaded. Each component's
check is one function returning a typed result (`operator_check.Verification`: found, expected,
verified, reason, code), and the step turns an unverified result into its own refusal.

## Native libraries inside wheels (question 250(a))

`--with-wheels` opens every wheel it fetches (`wheel_libs.py`; its module doc is the specification) and
lists the native libraries inside it in `KIT_MANIFEST.wheels.fetched[*].bundled_libraries`: every file under a
`<name>.libs/` directory (the auditwheel convention) and every other shared object that is not the
package's own extension module (a name of the form Python gives an extension module AND a `PyInit_<module>`
symbol in its bytes; those are listed apart, in `extension_modules`). Each entry has `path`, `sha256`, `size`,
`library` (the base name: `libgfortran`, `libscipy_openblas64_`), `spdx`, `gpl_family`, `upstream` and `evidence`.

The licence of a library is never guessed: it comes from the committed `bundled_libs.json` (base name to SPDX
expression and upstream) and the build refuses unless the wheel's own licence files confirm it. numpy writes
`Name:` / `Files:` / `License:` stanzas for its bundled libraries into `numpy-*.dist-info/licenses/LICENSE.txt`;
a stanza whose `Files:` pattern matches the library, with exactly the table's `License:` terms, is the
evidence, and its file and line range are recorded. A library not in the table (`unknown-library`), a text that
does not name it (`licence-text-silent`) and a text that disagrees with the table (`licence-mismatch`) are
refused, naming the wheel and the library. Statically linked code (pydantic_core, uvloop, ...) has no separate
file and is not inventoried.

The kit redistributes no GPL or LGPL code, so a library under a GPL-family licence moves its wheel to the
operator-supplied list. GPL-family means a term every OR-alternative of which is a `GPL-*`, `LGPL-*` or
`AGPL-*` licence, with or without a `WITH` exception: `GPL-3.0-or-later WITH GCC-exception-3.1` (libgfortran)
and `LGPL-2.1-or-later` (libquadmath) are; the GCC Runtime Library Exception frees numpy as a combined work,
not the library file. `GPL-2.0-only OR MIT` is not (the MIT alternative is a way to take the library without
the GPL), so such a library stays carried. The kit does not carry the
wheel, `KIT_MANIFEST.wheels.operator_supplied` records it by filename and sha256, and the builder refuses
unless `operator_supplied.json` has a `python-wheel` component for exactly that file (`dist`, `version`,
`filename`, `sha256`, `reference`). The installer then needs the operator's copy:

    <kit>/installer/install.sh <kit> <target> --operator-wheels DIR

`DIR/<filename>` must have the pinned sha256 (`wheel-dir-missing`, `wheel-dir-not-a-directory`,
`wheel-missing`, `wheel-hash-differs`, ... are typed refusals, before the target directory exists). The
verified file is copied under `<target>/operator-wheels/`, the copy is re-hashed, and pip installs it by exact
path next to `--find-links <kit>/wheels`, so no other file in the operator's directory can be picked up.
`--check-operator-supplied` takes the same option. In today's linux/aarch64/cp313 closure one wheel is
affected: numpy 2.5.3's, which bundles `libgfortran` (and OpenBLAS, BSD). The pins file has its
`python-wheel` component `numpy-wheel-linux-aarch64` (item 7), a `--with-wheels` kit lists the wheel in
`wheels.operator_supplied` and carries none, and the installer needs `--operator-wheels DIR`. numpy's licence
text also names a `libquadmath` stanza (LGPL-2.1-or-later), which a wheel for another platform may bundle;
it is in the table for that case, and no such wheel was fetched or inspected.

The connected-machine helper that makes DIR is `scripts/kit/save_operator_wheels.py --out DIR`: for every
`python-wheel` pin it runs `pip download` of exactly that distribution, version and platform tags (a network
use, once, default headers), refuses unless the file has the pinned name and sha256, and writes `DIR/SHA256SUMS`.
It is the wheel counterpart of `save_base_images.py`.

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

## Image application layers (question 250(a))

The base images are operator-supplied (`docs/kit-operator-supplied.md` items 1 and 2), so the kit
carries only what each image adds over its base. `scripts/kit/operator_supplied.json` pins the
bases by digest (and is copied into `installer/`); `scripts/kit/image_layer.py` does the work.
For each component `--with-images` writes:

- `images/<component>/Dockerfile` -- the committed runtime Dockerfile, `services/cfs/Dockerfile.kit`
  or `services/edge-plugin/Dockerfile.kit`: `FROM <base>@<digest>`, `COPY` of the files below, and
  only `RUN`s that need no network (`chmod`, `dpkg -i`, `groupadd`/`useradd`, `mkdir`/`chown`),
  plus the same `LABEL`/`USER`/`WORKDIR`/`VOLUME`/`ENTRYPOINT` as the development Dockerfile's final
  stage. The legacy builder accepts it (question 190: no BuildKit): no `COPY --chmod`, no heredocs.
- `images/<component>/rootfs/...` -- the runtime-content set's files at their image paths. Their
  modes are in `KIT_MANIFEST.images.<component>.files` (`KIT_MANIFEST.files` has none).
- edge-plugin only: `images/edge-plugin-image/libssl3_<version>_<arch>.deb` and
  `libssl3.copyright`. The `.deb` is recorded in `services/edge-plugin/runtime-packages.json`
  (package, version, arch, snapshot URL, sha256, size, licence read from the `.deb`'s own copyright
  file). The builder takes it from `.av-test-tmp/kit-deb-cache/<sha256>/` or fetches it once from
  snapshot.debian.org (TLS verified), refuses unless sha256 and size match the record, and proves
  every regular file the image holds from the package equals the `.deb`'s byte for byte (that is
  what makes the runtime-content hash reproducible from the kit).

The runtime-content hash is defined in `services/cfs/IMAGE_DIGEST.md` (cFS) and
`services/edge-plugin/build-image.sh` step 3a (edge plugin); `image_layer.py` is the second,
independent implementation (question 164), and `image_build.py` (below) the third.

### Building the images: the installer's host-side image step (question 250(a), second half)

The files install (`installer/install.sh <kit> <target>`) runs inside the enclave and never holds
the Docker socket or `DOCKER_HOST`: a container that controls the daemon could start a networked
container and defeat the `--internal` isolation the zero-egress proof rests on. Images are built by
a separate step of the same bundled installer, run by the operator ON THE DOCKER HOST (stdlib Python
>= 3.11 and the `docker` CLI), on a host that already holds the two pinned bases:

    <kit>/installer/install.sh --build-images <kit> [--component C ...] [--image-tag COMPONENT=REF ...] [--record FILE]
    <kit>/installer/install.sh --probe-offline-build <kit>

Bases come from the operator (`docs/kit-operator-supplied.md`). On a connected machine that holds
them, `scripts/kit/save_base_images.py --out DIR` runs `docker save <name>@sha256:<digest>` for each
pin (by digest, never a floating tag; the tarballs carry no repository name) and writes `DIR/SHA256SUMS`;
on the Docker host, `docker load -i` each tarball after checking that list.

`--build-images` (`scripts/kit/image_build.py` is the specification; each refusal exits 2 with a
typed line):

1. verifies the kit against `KIT_MANIFEST` first, then checks each image's recorded base against the
   installer's own `operator_supplied.json` and that the Dockerfile starts `FROM` exactly the pinned
   reference;
2. before ANY build, `docker image inspect <base reference>` for every image: accepted only if `Id`
   equals the pinned digest (containerd image store, this host) or `RepoDigests` contains it
   (classic store); otherwise `base-image-missing` / `base-image-digest-differs`, naming the
   expected reference and digest and telling the operator to `docker load` it;
3. builds from a context it assembles itself (the Dockerfile, each file at its image path with its
   RECORDED MODE, the `.deb`) with `docker build --network none --pull=false --no-cache --force-rm
   --rm=true -t <tag> --label org.altavista.kit-manifest-sha256=<hash> -` (the legacy builder, as on
   this host). Default tag `av-kit/<component>:<first 12 hex of KIT_MANIFEST's sha256>`; never the
   recorded `:local` tag, never a tag that already exists;
4. before anything is started, `docker create` (never `start`), `docker cp <container>:<root> -` per
   runtime root, the question-185 hash over the regular files, `docker rm -v`: the hash must equal
   `runtime_content_hash`, every file's mode the recorded one, and `.Config` the recorded `config`
   (the step's own `org.altavista.kit-*` labels removed). The image id is reported, not compared;
5. any refusal removes every tag the run created (by name; an image that existed before the run is
   never deleted); success writes `IMAGE_BUILD_RECORD` (default `./IMAGE_BUILD_RECORD`, never
   overwritten, not inside the kit): per image the tag, built id, base reference/digest and how it
   was verified, recorded and computed hashes with `equal: true`, the mode and `.Config` checks, and
   the build flags.

`--probe-offline-build` shows that the BUILD is offline, not just the enclave: through the same code
path and flags it builds throwaway images whose `RUN` tries the network (a TCP connect to
1.1.1.1:443; a name lookup of example.com) and requires both to fail with the unreachable /
name-resolution error. `INSTALL_RECORD.installed.images` says the files install built nothing
(`built: false`) and which command builds them.

The `images` section is reproducible by construction: every value is either read from a committed
record or computed over bytes that were just checked against one. Two `--with-images` kits from the
same commit have a byte-identical `KIT_MANIFEST` (`tests/test_kit_manifest.py::
test_two_image_bearing_kits_from_the_same_commit_have_the_same_manifest`, gated `AV_KIT_WITH_IMAGES=1`),
whether the `.deb` came from the cache or the network. The `docker save` byte-reproducibility
measurement this section used to record is retired with the tarballs.

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

- `--with-images` -- fetches the recorded `libssl3` `.deb` once from snapshot.debian.org over
  verified TLS, at kit-build time only, when `.av-test-tmp/kit-deb-cache/<sha256>/` does not
  already hold it; the bytes are refused unless they match the sha256 and size committed in
  `services/edge-plugin/runtime-packages.json`. `KIT_MANIFEST` records `network_used: true` for the
  edge-plugin image as a fact about the step, never whether this run hit the network, so the
  manifest is byte-identical either way. The rest of the step reads an already-built local image.

Nothing else -- every remaining step reads files already in this worktree.

## Regenerating the compliance documents (round 3, question 227)

`scripts/kit/regenerate_compliance.py` is the one committed script for the whole cross-track
compliance-document regeneration cycle: it regenerates the ten SBOMs
(`scripts/kit/sbom.py --out docs/compliance/sbom`), commits them, computes
`docs/compliance/BUNDLE.md`'s bundle hash (`scripts/kit/evidence.py`, no `--kit`/`--ledger-dir`),
records it (both the fenced hash block and a new row in that file's "Regeneration history"
table), and commits that -- in that order, as two separate commits, because the bundle's epoch
is derived from git history that only exists once the SBOM commit has actually landed (see that
script's own doc comment for the full reasoning, with a real example from this repository's own
history). It refuses, before touching anything, on an incomplete venv (reusing
`scripts/kit/sbom.py`'s own `python-lock.json` cross-check) or a dirty working tree outside the
paths it owns:

```
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
.venv/bin/python scripts/kit/regenerate_compliance.py            # the real cycle, up to two commits
.venv/bin/python scripts/kit/regenerate_compliance.py --dry-run  # regenerates for real, previews, commits nothing
.venv/bin/python scripts/kit/regenerate_compliance.py --check    # read-only: is anything stale?
```

**`docs/compliance/sbom/*.cdx.json`, `docs/compliance/sbom/SHA256SUMS`, and
`docs/compliance/BUNDLE.md` are marked `merge=ours` in this repository's `.gitattributes`**, so a
cross-track merge that touches any of them never stops on a conflict there -- the losing side's
content is simply discarded at merge time, and running the script above immediately afterward
regenerates the correct, current content from the merged tree and commits it. This is safe
specifically BECAUSE the script runs right after: taking either side of a merge conflict on a
generated document is equally arbitrary when both are about to be overwritten anyway, so `ours`
just skips a pointless conflict prompt. **The one hazard: if the script is NOT run after such a
merge, the committed documents are simply stale** (correct for neither parent, and no longer
matching the merged tree's real dependency graph or evidence). Nothing silently hides that --
`tests/test_sbom.py` (byte-identical regeneration, licence/epoch checks) and
`tests/test_evidence_bundle.py::test_bundle_sha256_matches_the_hash_recorded_in_bundle_md` both
fail loudly against a stale post-merge tree, exactly as they would against any other stale
regeneration. Running the script after a merge that touched these paths is mandatory, not
optional; `.gitattributes`' own comment says so too. `scripts/kit/python-lock.json` is
deliberately NOT in the `merge=ours` set -- see `.gitattributes`' own comment for why (it is an
input a human refreshes by hand, not an output this script regenerates, so an ordinary merge
conflict on it is a real disagreement to resolve, not noise to discard).

**One-time setup this needs, once per clone:** `merge=ours` names a merge *driver* git does not
ship enabled by default (only the strategy name exists out of the box) -- without registering it,
the attribute is silently ignored and an ordinary content merge (and possible conflict) happens
on these paths instead. Run once, in this checkout:

```
git config merge.ours.driver true
```

(`git config --global merge.ours.driver true` registers it for every checkout on the machine
instead, if preferred.)

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
- **`<binary>-binary`** (e.g. `av-command-binary`, if some future toolchain/dependency change ever
  breaks its cross-build again) -- present only when `--with-binaries` was passed and that
  specific binary did not cross-build; carries the REAL compiler error, never a synthesized
  message. As of round 3's `rust:1.90-bookworm` pin (question 217(a)) neither `av-ingest-server`
  nor `av-command` hits this on this host.
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

## The image SBOMs list the operator-supplied base (question 250(a))

`cfs-image.cdx.json` and `edge-plugin-image.cdx.json` list every package of the image's base
image (Ubuntu 22.04, Debian bookworm-slim), each marked `altavista:sbom:operator-supplied=true`
(`docs/kit-operator-supplied.md`: the kit names and pins the base, it does not ship it), with
Debian's own licence short names from the package's `/usr/share/doc/<pkg>/copyright` and a named
`altavista:sbom:gap` for every package whose copyright file has no machine-readable `License:`
field. The edge-plugin image's kit-carried `libssl3` is listed apart, `altavista:sbom:kit-carried`.

- **Input:** `services/cfs/BASE_IMAGE_INVENTORY.json` and
  `services/edge-plugin/BASE_IMAGE_INVENTORY.json`, generated by `scripts/kit/base_inventory.py`
  from the digest-pinned base (never a tag; `docker run --rm --pull never --network none`, one
  `tar` stream, the docker lock taken by the script itself -- do not wrap it in
  `with_docker_lock.py`). Per package: version, architecture, source package, licences, whether
  the copyright file is DEP-5, and every regular file the package owns with its SHA-256; plus the
  regular files no package owns. `--check` regenerates in memory and compares.
- **`sbom.py` stays Docker-free:** it reads the committed inventory, the pin in
  `operator_supplied.json` (an inventory generated from any other base is refused) and
  `services/edge-plugin/runtime-packages.json`. Only the two inventories join the image epoch.
- **The licence gate:** a component carrying the operator-supplied marker is evaluated and
  *reported*, never failed; a kit-carried component is failed on any non-allowed licence
  (`licences.evaluate_sbom_components`, `tests/test_sbom.py` section 14).
- A licence value is a CycloneDX `license.id` only when it equals an SPDX identifier byte for byte
  (`spdx-license-ids.json`); otherwise it is `license.name`, verbatim.

## The proof that the kit carries no copyleft from a base image (question 250(a))

`scripts/kit/kit_inventory.py KIT [--sbom-dir DIR]... [--list FILE.tsv]` and
`tests/test_kit_no_copyleft.py` (ungated over a fast kit; gated with `AV_KIT_WITH_IMAGES=1` over
`out/kit/proof`, plus perturbations with `/bin/grep` and `libc.so.6` read out of the pinned Ubuntu
base under the docker lock). Two proofs; the module's top doc has the details.

- **No file from a base-image package is in the kit.** Everything the kit carries is listed with its
  origin and size (every `KIT_MANIFEST.files` entry, pack symlinks, the image application layers file
  by file, and the members of every wheel, of the carried `.deb`, and of the vendored static
  libraries), and compared with every packaged and unowned file of both committed base inventories:
  by image path (an application layer against its own base, merged-/usr folded) and by sha256 (any
  path, either base). A match names the kit file and the base package.
- **The operator-supplied marker is proven true.** Each component carrying
  `altavista:sbom:operator-supplied=true` in an image SBOM must resolve to an inventory entry, each
  inventory package must be marked, and no file those components own is anywhere in the kit.
- **One definition of what may overlap:** `scripts/kit/base_overlap_exceptions.json`, read by this
  proof and by `tests/test_kit_zero_egress_install.py`: the empty file and the one-newline file are
  not compared by hash; the libssl3 copyright notice is accepted at three named kit paths (the
  notice file, its member in the carried `.deb`, its entry in the image layer) and the canonical
  Apache-2.0 text by sha256 at any path. Each names the base files that carry the same bytes and the
  reason; any other overlap fails.
- **The copyleft scan.** Every SBOM component licence matching GPL, LGPL, AGPL, MPL, EPL, CDDL,
  EUPL, SSPL or BUSL is operator-supplied (reported) or has an entry in
  `scripts/kit/copyleft_accepted.json` with its reason (today `certifi`, MPL-2.0, shipped
  unmodified); a stale entry fails. The kit's wheel inventory (`KIT_MANIFEST.wheels`) must show no
  bundled library with a GPL-family or other copyleft licence; operator-supplied wheels (numpy) are
  listed, and the kit must not carry them. It scans the SBOM copies in the kit, the image SBOMs
  regenerated now by `sbom.py`, and any `--sbom-dir`.
- **Not opened:** an archive nested inside a wheel or crate other than the `ar` libraries is hashed as
  a file and listed (none in the proof kit).
