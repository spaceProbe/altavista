# SBOMs (D2)

`*.cdx.json` in this directory are CycloneDX 1.5 software bills of materials, one per suite
component (`deploy/secdeploy/suite.altavista.toml`'s eight components) plus the two recorded
container images (`edge-plugin-image`, `cfs-image`). `SHA256SUMS` records each file's SHA-256,
`sha256sum` format, sorted by path.

## Why these are committed

Generated files are not usually committed — these are, deliberately: they are small,
deterministic text (see "Determinism" below), they make the licence check
(`tests/test_sbom.py::test_every_licence_in_every_committed_sbom_is_allowed_outright`, against
`deny.toml`'s `[licenses].allow` list -- its single source, question 214(a)) real on every
default `pytest` run without building anything, and a `git diff` on one of these files shows
exactly what changed in a dependency tree at a glance.

## Regenerating

```
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
.venv/bin/python scripts/kit/sbom.py --out docs/compliance/sbom
```

Rebuilds all ten. Add `--component <name>` (repeatable) to regenerate only some. The Rust
components (`av-ingest`, `av-command`, `av-gateway`, `av-proposer`, `av-dynamics-service`,
`av-edge-plugin`) each run `cargo auditable build --offline` first — set `CARGO_TARGET_DIR` to a
scratch directory first if you don't want that build to land in this repo's own `target/`. The
Python (`av-viewer`, `gmat-service`) and image (`edge-plugin-image`, `cfs-image`) components
build nothing; they read `importlib.metadata` over the venv `.venv/bin/python` is running from,
and the two images' own committed `IMAGE_DIGEST.md`/`IMAGE_CONTEXT_MANIFEST.txt` files,
respectively.

**The two Python SBOMs enumerate the identical package list, on purpose.** There is one
`.venv` for this whole worktree — not one per component — so `av-viewer.cdx.json` and
`gmat-service.cdx.json` both read `importlib.metadata` over the same installed-distribution set
rather than resolving each component's own dependencies separately. Each file's own
`metadata.component` carries an `altavista:sbom:python-packages-source` property saying so, so
this is visible from the SBOM file alone, not only from this README.

## Determinism: why the epoch and serial number come from git, not the clock

`metadata.timestamp` in every SBOM is **not** `datetime.now()` — it's the committer date
(`git log -1 --format=%cI`, converted to UTC, printed with a literal `Z`) of the last commit
that touched that component's own **inputs**: a Rust component's `Cargo.lock`, the workspace
`Cargo.toml`, and every crate's own `Cargo.toml` (the pathspec `crates/*/Cargo.toml`); an
image's `IMAGE_DIGEST.md` and, where one exists, `IMAGE_CONTEXT_MANIFEST.txt`; a Python
component's `pyproject.toml` **alone**. `serialNumber` is likewise derived — a SHA-256 over the
component's name and its own epoch, reformatted into a UUID's canonical hex grouping — rather
than a real random `uuid.uuid4()`.

**Why the Rust epoch is `Cargo.lock`/`Cargo.toml`/`crates/*/Cargo.toml`, not all of `crates/`.**
A Rust SBOM's package list comes from `cargo auditable`'s embedded dependency data
(`rust_binary_sbom` → `_cargo_auditable_build` → `_rust_audit_info`) with licences joined from
`cargo metadata` — both are pure functions of the *resolved dependency graph*: `Cargo.lock` and
the workspace's and each crate's own `Cargo.toml` manifest (its declared dependencies and
features). Neither step ever reads a `.rs` file's text, so a source-only change cannot change
either one's output. `RUST_EPOCH_PATHS` used to be `["Cargo.lock", "Cargo.toml", "crates/"]` —
whole-directory `crates/`, sources included — which made this the same class of defect as D2-1
below: commit `3bc12e6` ("Stop two tests from depending on this host's own state") touched only
`crates/av-lockstep/src/docker_test_lock.rs` (a `.rs` file, no manifest, no `Cargo.lock` change)
and a test file, yet because `crates/` was an epoch input it became the new epoch commit for
*all six* Rust SBOMs, immediately making every committed one stale — a gate failure nobody
caused, over a commit that could not possibly have changed a single package or licence in any
of them. Narrowing to `crates/*/Cargo.toml` (confirmed against this tree: `git ls-files --
'crates/*/Cargo.toml'` lists exactly the 18 crate manifest files and no `.rs` file, and no crate
nests a `Cargo.toml` any deeper than `crates/<name>/Cargo.toml`) makes the true epoch commit
`d65c360` (`Merge branch 'develop' into edge`, which last touched `Cargo.lock`) instead — the
last commit that could actually have changed a Rust SBOM's content.

**Why a workspace-manifest edit legitimately moves all six Rust epochs at once, and that's not
the same defect.** All six Rust components share one epoch over one input set
(`RUST_EPOCH_PATHS`), so a commit that edits the workspace `Cargo.toml` — or any crate's own
`Cargo.toml`, or `Cargo.lock` — moves every Rust component's epoch simultaneously. This is the
rule working correctly, not the whole-directory-`crates/` treadmill defect above (a `.rs`-only
commit must still not move the epoch — `test_rust_epoch_paths_match_no_rust_source_file` guards
that). The worked example: the AI-plane round 5 merge, commit `db1e858` ("bump the workspace to
`rust-version = "1.87"`", landing on `edge` at `3bcbd63`), touched no `.rs` file and added no
dependency, yet correctly moved all six committed SBOMs' epoch from `2026-09-15T12:30:33Z` to
`2026-09-15T18:02:25Z` — a workspace manifest is a real input to the resolved dependency graph
*and* the MSRV that `cargo auditable`/`cargo metadata` read, so its own commit date is exactly
right as every Rust SBOM's new epoch. `tests/test_sbom.py::
test_all_six_rust_components_share_exactly_one_epoch_from_git` asserts this positively: all six
share exactly one committed epoch, and it equals git's own committer date for
`RUST_EPOCH_PATHS`, computed independently of `sbom.git_epoch`.

**The operational consequence.** Any commit that edits `Cargo.lock`, the workspace `Cargo.toml`,
or a crate's own `Cargo.toml` must be followed by (or accompanied by, in a separate later
commit) an SBOM regeneration — see "Regenerating" above for the exact command.
`tests/test_sbom.py::test_the_epoch_is_never_wall_clock` is what enforces this: it recomputes
each component's epoch straight from git and fails the moment a manifest edit lands without a
matching regeneration, exactly what happened (for real, not hypothetically) when `db1e858`
merged into `edge` without one — the six failures this task's commit fixed.

**Why the Python epoch is `pyproject.toml` alone, not a component source path too.** A Python
SBOM's content is `importlib.metadata` over the shared worktree `.venv` — the set of *installed
distributions*, not this repository's own source. `pyproject.toml` is the file that declares
that set (`[project.dependencies]`/`[project.optional-dependencies]`); editing
`altavista/server.py` or anything under `services/gmat-service/` cannot add, remove, or change
the version of a single package in the SBOM, so an earlier revision that included those paths in
the epoch made the *committed* SBOM go stale the instant an unrelated later commit touched them
— a gate failure nobody caused, the identical shape of defect the Rust epoch had. Now the only
commit that can invalidate a Python SBOM is one that changes `pyproject.toml`, which is exactly
when the installed dependency set is capable of having changed, and is a drift signal worth
acting on. (Confirmed safe for the other two kinds too: a commit that only adds
`pyproject.toml`'s licence field, for example, touches none of `Cargo.lock`/`Cargo.toml`/
`crates/*/Cargo.toml` or either image's `IMAGE_DIGEST.md`, so it cannot silently invalidate a
Rust or image SBOM the same way.)

The point of both is the same: **regenerating an SBOM from unchanged inputs must produce the
byte-identical file.** A wall-clock timestamp or a random UUID would make every regeneration a
spurious diff, defeat `SHA256SUMS` as a drift detector, and make
`test_two_generations_are_byte_identical`/`test_rust_sbom_regenerates_byte_identically`
impossible to write as anything but tautologies. Deriving both from already-committed git
history means the SBOM only changes when something it actually describes changes.

## Why a Rust SBOM does NOT record the binary's own content hash

Unlike the two image SBOMs (which record a recorded image id / recorded file hashes read from
committed `IMAGE_DIGEST.md`/`IMAGE_CONTEXT_MANIFEST.txt` — inputs, not products of a link on
this host), a Rust component's `metadata.component` carries no `hashes` entry at all. This was
tried and measured to be wrong: on this platform, an independent `cargo auditable build` of the
identical source does not reproduce the same binary bit-for-bit (confirmed directly — three
links of `av-ingest-server` into the same `target/`, no source change between the first two,
three different SHA-256 hashes; Mach-O's per-link `LC_UUID` load command, plus other
build-environment detail the debug profile embeds). An SBOM's job is the dependency
composition, which genuinely IS reproducible (`Cargo.lock` pins every version); the linked
artefact's own content hash is a property of one specific build, not of the source that produced
it — its home is D3's `KIT_MANIFEST`, which records every shipped file's real SHA-256 beside the
images' own recorded digests, never here. `tests/test_sbom.py::
test_rust_sbom_never_records_the_binarys_own_hash` guards against this field coming back.
What IS reproducible, and is recorded instead as properties on each Rust component's own
`metadata.component`: the cargo package and `--bin` target the SBOM describes, the build
profile (`debug`), and the `rust-audit-info` version that read the binary back.

## The expensive round-trip test

`tests/test_sbom.py::test_rust_sbom_regenerates_byte_identically` actually runs `cargo auditable
build --offline` for all six Rust components and confirms each regenerated SBOM is byte-for-byte
identical to the committed one — the real proof that what's committed is reproducible, not just
asserted to be. It does not run by default (the default `pytest` gate never builds anything —
see `test_rust_sbom_versions_match_cargo_lock` for the cheap, build-free drift detector that
runs every time instead). Set `AV_SBOM_REBUILD=1` to opt in:

```
AV_SBOM_REBUILD=1 .venv/bin/python -m pytest -q tests/test_sbom.py
```
