# P5, the deployable platform: plan

The edge team's charter after `edge-plan.md` closed (question 212), taken by the lead on
2026-09-15 as the recommendation the user had not redirected. Decisions in
`open-questions.md` question 213. This is P5 in `architecture.md` ("air-gap kit, SBOMs,
accreditation evidence package, multi-node scale; exit: zero-egress install; 3-node
throughput target met") on the substrate ADR-003 decided: AltaVista components as tiers of
the SecRouter suite, declared in `suite.toml`, placed by `secsite.toml`, deployed by
`secdeploy`, with `fedora-fips` as production and macOS as the evaluation target.

## Goal

A kit built on a connected host and carried into an enclave installs the platform with no
egress and runs the demo end to end; every component ships a reproducible SBOM whose hash
is in the kit's manifest; one evidence bundle collects every component's control matrix and
ledger verification; three engine placements meet a stated throughput number, or the
measured shortfall is recorded with its cause.

## What exists

- secdeploy at `/Users/probe/code/secdeploy` (`uv run secdeploy verify|plan|fetch|build|
  bundle|deploy|status|evidence`), its `suite.toml` component shape (`repo`, `ref`, `kind`,
  `tier`, `port`, `runtime`, `role`, `optional`), `secsite.toml` placement (`[resources.*]`,
  `[groups.*]`, `[[builds]]`, `[audit]`), targets `macos` (compose via Colima),
  `fedora-fips` and `ubuntu` (systemd-native), the deploy-audit hash chain and
  `secdeploy evidence` (`docs/compliance.md` there).
- Our services with `/admin/api/evidence`, ledger `verify` and control matrices under
  `docs/compliance/`: `av-dynamics-service`, `gmat-service`, `av-ingest`, `av-edge-plugin`,
  `av-command`, `av-gateway`; the image build scripts with their recorded digests for the
  cFS lockstep image, the edge plugin and the proposer; `cargo-auditable` on the host; the
  FIPS rule module and `cargo deny`'s bans; the owned port map (AI-plane round 5).
- The demo path end to end: a kernel run, the plugin, the ingest, the tracker, the command
  service, the gateway, the proposer, the viewer with its console.
- Lima 2.1.4 on the host, able to run a Fedora VM beside Colima.
- Not yet: any suite declaration for our components, any SBOM, any kit, any evidence bundle
  across components, any multi-placement run, any Fedora rendering.

## Isolation

The team keeps the worktree `/Users/probe/code/AltaVista-edge` on branch `edge` (the branch
name is history; the plan is this file). New code lives under `deploy/secdeploy/` (the
suite fragment, site files, build entries), `scripts/kit/` (the kit builder and the SBOM
generator), `docs/compliance/` (the cross-component index and coverage table) and a new
crate only if a Rust tool is warranted; existing services gain nothing but what an SBOM or
an evidence field needs, additively. `/Users/probe/code/secdeploy` is the user's checkout:
read it, run it, never edit it; a change it needs is a proposal in `docs/secdeploy-upstream.md`.
The AI-plane team works in its own worktree at the same time; the tracks share nothing but
`develop`, which the lead merges into both between rounds and each back after acceptance.

## Rules that bind this track

Every standing rule in `teamlog/2026-09-02-team-1.md` and `open-questions.md` applies:
questions 148, 154 (no network at test time; a kit is built with network once and installed
with none), 156 and 207 (labelled resources, the host-wide docker lock, build scripts take
it), 157, 194, 199, the crypto rule of ADR-004 (`cargo deny` bans, no `sha2`, the system
OpenSSL only), the contention rule, and question 212's image-provenance rule (a test trusts
an image only after comparing it to its recorded digest). Nothing in a kit is fetched at
install time: an install that reaches for the network fails, and a test proves it.

## Milestones

**D1 Suite declarations.** `deploy/secdeploy/suite.altavista.toml`: every AltaVista service
as a secdeploy component (kind, tier per ADR-003's tier list, port from the owned port map,
runtime, role, the image build entry where one exists), plus `secsite.altavista.toml` for a
one-resource macOS evaluation placement and a three-resource placement for D5. Validated by
running secdeploy's own `verify` and `plan macos` over a merged manifest (secdeploy is run,
not vendored; the merge script is ours), and by a test that every declared port equals the
service's `DEFAULT_BIND` and the port map. Tests: verify and plan exit 0 with their output
saved; a deliberately wrong port fails the port test; the manifest round-trips
byte-identically through the merge.

**D2 SBOMs.** One generator, `scripts/kit/sbom.py`, producing CycloneDX JSON per component:
Rust services from `cargo auditable`'s embedded dependency list read back with
`rust-audit-info` (or `cargo cyclonedx`, fetched once at setup and pinned, if the auditable
route cannot express licences), Python components from the venv's installed distributions,
container images from their recorded manifests (base image digest, the prebuilt binaries'
hashes). Deterministic: sorted, no timestamps except a recorded build epoch taken from the
git commit, and a test that two generations are byte-identical and that every crate in
`Cargo.lock` appears in the SBOM of the binary that links it. The SBOM hash of each
component is recorded beside its image digest. Every licence in every SBOM is in
`deny.toml`'s allow list, proven by a test over the generated files, not by trusting
`cargo deny` alone.

**D3 The zero-egress kit.** `scripts/kit/build.sh` collects, on a connected host: the
built images as tarballs with digests, the Python wheels the viewer needs, `cargo vendor`'s
crate sources for a rebuild, the environment data packs (GMAT data, ephemerides, EOP, space
weather, the cFS mirrors) hashed as versioned packs (question 22), the seccert root and the
suite and site files, the SBOMs, and a `KIT_MANIFEST` listing every file with its SHA-256
and the git commit; `scripts/kit/install.sh` installs from the kit alone. The proof: the
install and the demo run inside a Docker `--internal` network with no route out, with the
plugin, ingest, tracker, command service, gateway and viewer server started from the kit's
images and the kernel run reproduced from its hash; a test asserts the manifest verifies,
that every image loaded matches its recorded digest, and, by an egress probe of the kind the
edge plugin test already measures, that nothing reached outside. A kit built twice from the
same commit has the same manifest hash.

**D4 The evidence package.** `secdeploy evidence` run over a deployed evaluation placement,
plus our own `scripts/kit/evidence.py` that collects what secdeploy cannot know: every
component's control matrix, the cross-component NIST 800-171 coverage table (per practice:
Met, Partial, Inherited, Gap, and which component says so), the deficiency list, the ledger
`verify` results, the SBOM hashes and the kit manifest hash, into one hashed bundle under
`docs/compliance/bundle/` (the bundle's own hash is recorded; the bundle itself is
regenerable and not committed). Tests: the bundle is byte-identical across two runs on the
same state; a tampered ledger makes its `verify` result and the bundle say so; every
control matrix row is in the coverage table exactly once.

**D5 Three placements.** The `engine` tier placed on three resources of the site file, run
as three containers on one host (this host has no three machines; the numbers are stated
as single-host, three-placement), consuming a measurement stream from the plugin through
the ingest at a declared rate, with throughput and the edge-to-core p50 and p99 measured
against spoore's budget (1k measurements/s per shard, p99 75 ms through the bus, question
41) and recorded with the host state; a shortfall is recorded with its measured cause, not
rounded away. The E5 latency retake the lead owes (question 212) is taken on the quiet host
before this milestone starts and is the baseline the three-placement number is compared to.

**D6 The Fedora target, as far as this host allows.** A Lima Fedora VM (one-time image
fetch at setup) as the `fedora-fips` test bed: `secdeploy deploy fedora-fips --dry-run`
renders the runbook and the systemd units for our components; the FIPS preflight is run for
real inside the VM; what cannot run there (hardware attestation, a real enclave network) is
listed with the reason. If Lima cannot boot a Fedora image on this host, that is recorded as
the measured reason and the runbook is still rendered and reviewed.

## Exit

From a clean host with egress blocked, the kit installs and the demo runs end to end with
every image matching its recorded digest; every component has a reproducible SBOM whose
hash is in the kit manifest and whose licences pass the allow list; one evidence bundle
covers every component with a coverage table and a deficiency list; three engine
placements have a measured throughput and latency against the recorded budget; the Fedora
runbook renders and its preflight has run where the host allows. Every number in the
status section is traceable to a commit and a manifest hash.

## Status (P5 manager, 2026-09-15) — round 1

D1 and D2 are delivered and accepted; D3's first half (the kit builder and its manifest,
without the zero-egress install proof) is delivered and accepted as the charter's
"if capacity remains" item. D4, D5 and D6 are untouched.

### What landed on `edge`

| Commit | What |
|---|---|
| `0bff552` | D1: the AltaVista suite fragment, the merge script, the port checker, two site files, `docs/secdeploy-upstream.md`, 12 tests |
| `778fbeb` | `pyproject.toml` declares `license = "Apache-2.0"` — on its own, ahead of the SBOMs, for the reason in decision 6 |
| `f5b4706` | D2: the SBOM generator, the SPDX/licence checker, ten committed CycloneDX documents with their `SHA256SUMS`, the declared licence exceptions, 50 tests (+6 gated) |
| `982ad85` | D3 first half: the kit builder, `KIT_MANIFEST` and its verifier, 18 tests (+2 gated) |

### Gate counts (run by the manager at `982ad85`'s tree, no worker active)

- `cargo test --workspace --exclude av-kernel --no-fail-fast`: **834 passed, 0 failed, 3 ignored**
  across 109 test binaries, exit 0. Unchanged from the round's baseline; this round added no Rust.
- `cargo clippy --workspace --all-targets -- -D warnings`: exit 0, no warnings. No `#[allow]` was
  added anywhere this round (grepped, not assumed).
- `cargo deny check`: exit 0, "advisories ok, bans ok, licenses ok, sources ok", with exactly the
  six `warning[wildcard]` lines question 207 accepts.
- `.venv/bin/python -m pytest -q -rs`: **1 failed, 634 passed, 10 skipped**. Baseline was 1 failed,
  554 passed, 2 skipped; the round added 80 tests and 8 visible, opt-in skips. The one failure is
  `tests/test_proposer_container.py::test_proposer_on_an_internal_network_proposes_and_cannot_reach_the_authority`,
  pre-existing and not this track's: the proposer image on this host was built from the AI-plane
  branch and now requires `--service-token-file`, which is exactly question 212(a)'s cross-track
  image-provenance gap.
- secdeploy over the merged manifest, run offline against the user's own checkout
  (`uv run --offline --project /Users/probe/code/secdeploy secdeploy …`): `verify` exit 0
  ("✓ manifest valid", "✓ target assets present", "✓ topology valid — domain altavista.internal,
  1 resource(s)", all 17 components placed) and `plan macos` exit 0 with every AltaVista component
  listed at its `ref`. Full outputs saved with the round's gate logs.

### D1 — the suite declarations

Eight components in `deploy/secdeploy/suite.altavista.toml`: `av-ingest`, `av-command`,
`av-gateway`, `av-proposer` (tiers engine/gateway), `av-dynamics-service`, `gmat-service`
(optional), `av-viewer` (fronted) on tier design, and `av-edge-plugin` on tier edge. Ports
declared and checked against their real sources: av-command 50070, av-gateway 50071,
av-dynamics-service 50062, gmat-service 50061, av-viewer 8765. Three components declare port 0,
and the fragment says which of two reasons applies: `av-edge-plugin` never listens, while
`av-ingest` and `av-proposer` are **documented gaps** — neither binary owns a default bind
(question 208(c), the AI-plane's port map). The checker fails the day one appears, so the gap
closes itself rather than rotting.

`secdeploy`'s `manifest.py` hard-codes `TIERS` to five names, so ADR-003's tier list cannot be
expressed. `[tier_compat]` maps engine→inference, design→collab, gateway→gateway, edge→edge for
the merge; an unmapped tier is a typed refusal, never an implicit identity mapping.
`docs/secdeploy-upstream.md` proposes the additive `tiers = [...]` manifest key that would delete
the table, and `test_adr003_tiers_are_still_rejected_upstream` is what makes that proposal
evidence: it merges without the mapping and asserts secdeploy's own exit 1 and its four
`tier must be one of` lines. The day that test starts passing, the compat table goes.

### D2 — the SBOMs

Ten CycloneDX 1.5 documents committed under `docs/compliance/sbom/`, hashes as recorded in
`SHA256SUMS`:

| Component | Packages | SHA-256 |
|---|---:|---|
| av-command | 151 | `75c5cce137493bd5f91a28a27f1ea28339aa4cc804c87ba3a274be0c5e40a8bf` |
| av-dynamics-service | 138 | `235ac915d057896fc9e8b561634328cf58bfe07fe2da1184840f33e09e0972c2` |
| av-edge-plugin | 147 | `a14760b689a2d6f70e3c4370bdfe837080cea0d09038c00732e40a4a7b83686a` |
| av-gateway | 152 | `b059f5f7ec5f6b7275f4ecaabc381e5d50a48e96f5428ee0ffd4f3d9e22c0a97` |
| av-ingest | 139 | `98b490042f99bb28d63999de7e17105256498a9c4b4f3de5867a8712acadaab8` |
| av-proposer | 154 | `471985a934721f710068d02f1fb03275bdba596c24a66fde0022c82c5d38ed50` |
| av-viewer | 34 | `8572c785485499fb54e2c26ac5b093a93d282de385061321d15a23d182a988e0` |
| gmat-service | 34 | `62c8202989c84d4aff78f5bc5bb48388222aa8635d16431da5e4a618e6e59e25` |
| edge-plugin-image | 3 | `105bc7e343f8707da1c51f29acd7300c13ed14095b6ccc2bb642bdd27d92f557` |
| cfs-image | 1 | `f87b43584d000030f9dcde8065d89f2d503dbc7e251eac232eedee0dbcfae9e4` |

Rust package lists come from `cargo auditable`'s embedded data read back with `rust-audit-info`
0.5.4 (installed once at setup, recorded by version, never used at test time), which is what each
binary actually links — the UEFI-only `r-efi` in `Cargo.lock` is correctly absent. Licences are
joined from `cargo metadata --frozen --offline`. Python lists come from `importlib.metadata` over
the one shared worktree venv, which each document says of itself. Image lists come from the
recorded `IMAGE_DIGEST.md` files, never a live daemon. Nothing reaches the network.

Every `Cargo.lock` package is accounted for: 170 appear in some Rust SBOM; the other 34 are
classified by `cargo metadata`'s own resolve graph as workspace members, av-track-only
transitives, or other-target — with no hand-written exception list and no unexplained remainder.

Licences are evaluated as real SPDX expressions (`AND`, `OR`, `WITH`, parentheses, the legacy
slash) against `deny.toml`'s allow list, read with `tomllib` so the policy has one source of
truth. **Exactly two packages in all ten documents are not allowed**: `numpy` 2.5.3's `0BSD`
(one term of a five-way `AND`) and `typing_extensions` 4.16.0's `PSF-2.0`. Both are permissive,
both are declared in `docs/compliance/sbom/licence-exceptions.md` with what would close them, and
`deny.toml` is untouched because the allow list is the lead's. A new non-allowed licence still
fails the gate, and so does a stale exception row.

### D3 first half — the kit

`scripts/kit/build.sh --out <dir>` assembles a kit and writes `KIT_MANIFEST`. At `982ad85`'s tree
the manifest hash is `25e95938e878328f8b4b948397d5b53edc7bb15f55da4c7d07eac173eb02848e`, and two
builds of the same tree produce the same hash — including the image-bearing kit, because
`docker save` turns out to be byte-reproducible on this host (measured: two saves of
`altavista-cfs-lockstep:local` and `av-edge-plugin:local` gave identical tarball hashes, 29,426,688
and 31,668,224 bytes). That is recorded as a measured property of this docker, with a test to say
so if a future one changes it.

The manifest names its own five gaps — `cargo vendor`, the Python wheels, the seccert root, the
install path, and secdeploy's own `deploy/` assets — so a kit cannot quietly start looking
complete. `verify_manifest` re-hashes everything and reports a missing file, a hash or size
mismatch, a file the manifest does not list, and any symlink at all.

### Decisions taken this round

1. **secdeploy's five tiers cannot express ADR-003's, so the fragment keeps ADR-003's vocabulary
   and the merge maps it.** The alternative — rewriting ADR-003 to secdeploy's tiers, or forking
   secdeploy — would have lost the architecture's own vocabulary or broken question 213(a). The
   mapping is declared in one table, refuses to fall through, and is deleted by the upstream
   proposal. The gap is proven by a test, not asserted.
2. **`gateway` is this fragment's own tier name, not ADR-003's.** ADR-003 names nine tiers and
   `gateway` is not among them, but av-command, av-gateway and av-proposer need one. Both the
   fragment and the upstream proposal say so rather than implying ADR-003 blessed it. No ADR
   amendment was invented.
3. **Our site files are standalone and never merged with the user's `secsite.toml`.** That file
   carries the user's LAN address, model directories and voice settings; an evaluation placement
   has no business depending on any of it.
4. **`av-ingest` and `av-proposer` are declared port 0 as gaps rather than given invented ports.**
   Question 208(c) makes the owned port map the AI-plane's, in flight in their worktree right now;
   inventing a number here would have created exactly the drift that question exists to close.
   The checker asserts the absence positively, so the row cannot silently stay wrong.
5. **SBOM package lists come from `cargo auditable`, licences from `cargo metadata`.** The
   auditable data is ground truth about linking but carries no licences; the plan's own fallback
   (`cargo cyclonedx`) would have added a tool whose output we would have had to normalise anyway.
   No second tool was installed.
6. **`pyproject.toml`'s licence landed in its own commit, ahead of the SBOMs.** The build epoch is
   derived from the last commit touching a component's inputs, and `pyproject.toml` is one of
   those inputs, so a single commit carrying both the change and the generated documents would
   have invalidated them the moment it landed. Proven, not guessed: a throwaway commit of the whole
   change set failed four tests, and the two-commit sequence fixes it.
7. **A Python SBOM's epoch inputs are `pyproject.toml` alone, not the component's sources.** A
   source edit cannot change one installed distribution, so letting `altavista/**` invalidate the
   document would be a gate failure nobody caused — question 207's "a failing gate nobody can act
   on is not a gate", applied before it bit us.
8. **The binary's own SHA-256 is deliberately absent from every Rust SBOM.** Measured: three links
   of unchanged source produced three different binaries on this host, so any document carrying
   that hash is irreproducible by construction. The artefact hash belongs to D3's kit manifest;
   the SBOM describes the composition, which is reproducible.
9. **Non-allowed licences are declared exceptions, not a deny.toml edit.** Adding `0BSD` and
   `PSF-2.0` to the allow list is a policy change the lead owns. The exception file keeps the gate
   meaningful in both directions meanwhile.
10. **The kit builder never builds.** It collects and hashes artefacts that are already built and
    already digest-recorded, which is the only way the manifest hash can be stable given
    decision 8's measurement.
11. **Environment data ships as pack descriptors this round, not as bytes.** `GMAT R2026a` is
    738 MB behind a symlink into the main worktree and `third_party/{mirrors,cspice,cfs}` are
    symlinks too; copying them is the install proof's problem, and the descriptor records the
    recursive content hash, the file count, the byte count, and whether a symlink was crossed.
12. **D3's second half is deferred to round 2 whole, rather than started here.** `cargo vendor`,
    the wheels and the seccert root have no honest test until a zero-egress install exercises
    them; collecting them now would have been a stub with a green test beside it.

### Defects found in review, and their root causes

Every one was found by re-running from the tree, not by reading a worker's claims.

- **D1: a component with no `[ports.*]` row was silently unchecked.** Root cause: the checker
  iterated the port table, not the component table, so the two sets were never compared. Fixed by
  a set comparison up front with its own finding kind.
- **D1: `kind = "no-listener"` never read its own file.** The claim that `av-edge-plugin` never
  binds was a comment, not a check. Fixed; the file is now searched for listener shapes.
- **D1: an unmapped tier fell through to an identity mapping** despite the fragment's header
  promising otherwise, and **fields were interpolated into TOML unescaped**. Both are now the same
  typed refusal.
- **D2 (critical): the committed Python SBOMs went stale the instant the commit that added them
  landed.** Root cause: the epoch is recomputed from git at test time, and `pyproject.toml` was
  both an epoch input and a file the task changed — self-invalidating by construction. Proven with
  a throwaway commit (four failures), fixed by decisions 6 and 7.
- **D2: invalid SPDX was emitted into the documents** (`"expression": "3-Clause BSD License"`)
  even though the alias table already knew it meant `BSD-3-Clause`. Root cause: the raw metadata
  string was written through without passing the normaliser the licence checker already used.
- **D2: the duplicate-distribution tie-break compared absolute filesystem paths**, so a genuine
  licence disagreement would have been resolved by where the venv lives. Now a typed refusal.
- **D2 (critical): the rebuild proof failed for all six Rust components.** Root cause measured,
  not assumed: the SBOM carried the binary's SHA-256, and three links of unchanged source gave
  `7788da1a…`, `d3b17dd4…`, `4649ae52…`. The document was wrong, not the test. Decision 8.
- **D3 (critical): a kit could carry a symlink to anywhere on the build host and verification
  said nothing.** The builder itself planted `deploy -> /Users/probe/code/secdeploy/deploy`, and a
  planted symlink to `/etc/passwd` verified clean, because a symlink to a file answers
  `is_file()` and was hashed as its target. Fixed on both sides: the merge now happens in a
  staging directory outside the kit, a built kit contains no symlink at all, and `verify_manifest`
  reports any symlink with absolute or escaping targets called out separately.
- **D3: `git_dirty` was true forever** in a worktree carrying three untracked `third_party`
  symlinks, so it signalled nothing. The manifest now records the porcelain entries too.

### Open items for the lead

1. **`deny.toml` needs two one-line additions, or an explicit refusal**: `0BSD` and `PSF-2.0`.
   Both are permissive and both are transitive Python dependencies of the viewer's own stack. The
   exception file is written to be deleted.
2. **`docs/secdeploy-upstream.md`'s first proposal is ready to send.** Manifest-declared tiers,
   additive, backward-compatible, with the exact reproduced error text and the two `topology.py`
   call sites it would touch.
3. **Question 208(c)'s port map is still the blocker for `av-ingest` and `av-proposer`.** The
   moment the AI-plane lands it, D1's fragment gains two real ports and `ports.py`'s gap rows are
   forced to become `const` rows.
4. **`tests/test_proposer_container.py` has failed on this branch for the whole round** for
   question 212(a)'s reason. It is the AI-plane's image to re-record; until then every P5 round's
   pytest count carries one failure that is not ours.
5. **The manager used the network once, outside a test**, to measure the Colima VM's free disk
   (`docker run --rm alpine:3.20 df -h /` — 38.6 GB free of 58.8 GB). The pulled image was removed
   under the host-wide lock and the host was left as found; recorded here because question 154's
   discipline is worth keeping literal.
6. **D3's second half needs a decision on what the kit actually carries**: `cargo vendor`'s tree
   and the Python wheels are hundreds of megabytes, and the GMAT pack is 738 MB reached by a
   symlink into the main worktree. The install proof cannot start until that is settled.

## Status (P5 manager, 2026-09-15) — round 2

The two test defects the lead's clone gate found at `6ddb9c1` are fixed; question 214(a)'s licence
ruling is implemented and the declared exceptions are gone; **D3 is complete** — a kit now carries a
real install payload and a zero-egress install proof runs the demo path from it inside a Docker
`--internal` network, measured. D4 was not reached; D5 and D6 remain untouched.

### What landed on `edge`

| Commit | What |
|---|---|
| `3bc12e6` | Task 1: both clone-gate defects — the symlinked-pack test builds its own pack under `tmp_path`; the lock tests prove only that their own processes serialise |
| `8223ea0` | Task 2A: `0BSD` and `PSF-2.0` join `deny.toml`'s allow list; `licence-exceptions.md` and its machinery deleted; the gate keeps both directions |
| `327fe70` | Task 2B: the Rust SBOM epoch narrows to the manifests, so a `.rs` edit no longer makes six committed SBOMs stale; six documents regenerated |
| `206ae69` | Task 3a: the kit carries real bytes — copied packs with their symlinks, `cargo vendor`, the viewer's linux wheels, cross-built Linux binaries, the recorded runs (`kit_format` 2) |
| `519c612` | Manager review fix: the cross-build cache is keyed on the source state, not the commit; a kit says when a step used the network |
| `6cbcf0b` | Task 3b: `scripts/kit/install.sh`/`install.py` and `tests/test_kit_zero_egress_install.py` — D3's second half |
| `381b29d` | Task 3c: the viewer's `web/` and `profiles/` are carried in the kit and installed from it, so the proof mounts no worktree asset |

### Gate counts (run by the manager at `381b29d`'s tree, no worker active)

- `cargo test --workspace --exclude av-kernel --no-fail-fast`: **835 passed, 0 failed, 3 ignored**
  across 109 test binaries, exit 0. Round 1 measured 834; the one new Rust test is the non-locking
  cross-language path-agreement test task 1 split out.
- `cargo clippy --workspace --all-targets -- -D warnings`: exit 0, zero warnings. No `#[allow]` was
  added anywhere this round (grepped, not assumed).
- `cargo deny check`: exit 0, "advisories ok, bans ok, licenses ok, sources ok", with exactly the six
  `warning[wildcard]` lines question 207 accepts. Two `warning[license-not-encountered]` lines are new
  and expected: `0BSD` and `PSF-2.0` are Python licences that cargo-deny's Rust-only graph never sees.
- `.venv/bin/python -m pytest -q -rs`: **1 failed, 661 passed, 13 skipped**, 301.66s. Round 1's
  baseline was 1 failed, 634 passed, 10 skipped: +27 tests and +3 visible, opt-in skips. That run
  included the zero-egress proof for real (the proof kit was present), not as a skip. The one failure
  is still `tests/test_proposer_container.py::test_proposer_on_an_internal_network_proposes_and_cannot_reach_the_authority`,
  the AI-plane's image and question 212(a)'s gap, reproduced with its own error text
  (`--service-token-file is required`), not ours.
- secdeploy over the merged manifest, offline against the user's own checkout: `verify` exit 0
  ("✓ manifest valid", "✓ target assets present", "✓ topology valid — domain altavista.internal,
  1 resource(s)", all 17 components placed) and `plan macos` exit 0 with every AltaVista component
  listed at its `ref`. Unchanged from round 1.
- The kit built twice from this tree with every gated flag: manifest hash
  **`36eb5e30a74d61f2827292178e829a4c2a4b444c42d2d067e407e8928a290de3`** both times, the two
  `KIT_MANIFEST` files byte-identical (`cmp`, not eyeballed).

### Task 1 — the two clone-gate defects

**(a) A test that read this worktree's own symlinks.**
`test_pack_descriptor_records_a_symlinked_out_of_worktree_pack_honestly` asserted against
`third_party/cspice`, a symlink into another worktree that exists only here; in a plain clone the
path is absent or a fetched directory, so the test either errored or asserted the wrong values. It
now fabricates its own pack under `tmp_path` — an `outside/` tree with a file and a subdirectory
file, a `worktree/` standing in for the repo root, and `worktree/third_party/cspice` symlinked at it
— and asserts the identical Decision-K properties plus a now-deterministic `resolved_real_path`
(`"../outside"`) and an exact `file_count`.

**(b) Two tests that asserted the host-wide lock was free** (question 212(b)). Both the Rust
`flock_lock_is_visible_across_processes_and_languages` and the Python
`test_two_python_processes_mutually_exclude_on_the_docker_test_lock` took the real
`$HOME/.altavista/locks/docker-tests.lock`, dropped it, and asserted a child could then acquire it —
false whenever another track holds it, for a reason that has nothing to do with the lock's own
correctness. Both now run on a private path while still exercising the production code:
`lock_docker_tests()` is now `lock_docker_tests_at(lock_file_path())` and the Rust test locks a path
under its own `repo_scratch_dir()`, handing it to the python child through argv; the Python test's
children get a private `$HOME` through `subprocess.Popen(env=…)` — never a mutation of the test
process's own environment — so the unmodified `lock_path()` resolves somewhere only those children
touch. The cross-language path-agreement claim the old tests proved as a side effect of locking is
kept as its own non-locking test on each side.

`lock_docker_tests_announces_a_blocked_wait_never_silently` and its probe deliberately stay on the
real lock: they never assert it is free, only that a genuinely contended acquire announces itself.

Proof, run by the manager independently of the worker: a helper process took the real host-wide lock,
a one-shot `LOCK_EX|LOCK_NB` probe confirmed `BlockingIOError` ("real host-wide lock is HELD"), and
with it held the three Python lock tests passed in 1.37s and the two fixed Rust tests in 0.07s —
neither blocked, neither failed. The helper then released and the lock was confirmed free again.

### Task 2 — the licence allow list, and an SBOM staleness defect

`0BSD` and `PSF-2.0` are in `deny.toml`'s allow list with their reason (numpy 2.5.3's five-way `AND`,
`typing_extensions` 4.16.0; question 214(a)), `docs/compliance/sbom/licence-exceptions.md` and every
reference to it are gone, and the check reads the allow list as its single source. The replacement
test asserts zero findings and prints what it evaluated — **949 (component, package, version,
licence) combinations across the ten documents, 0 findings**. Two tests keep the gate honest in both
directions: a licence outside the allow list is still a finding, and numpy's real expression plus
`PSF-2.0` come back as findings against an in-memory allow list with those two entries removed — the
test that catches someone silently deleting them.

Along the way the manager found a defect of the same class as round 1's D2-1: **`RUST_EPOCH_PATHS`
included the whole of `crates/`**, so any commit touching any `.rs` file made all six committed Rust
SBOMs stale immediately. Demonstrated, not argued: task 1's commit touched one `.rs` file and one
test file, changed no manifest and no lockfile, and six `test_the_epoch_is_never_wall_clock`
parametrisations failed at once. A Rust SBOM's content comes from `cargo auditable`'s embedded
dependency data and `cargo metadata`'s licence map — both pure functions of the resolved dependency
graph, neither of which reads a `.rs` file — so the epoch is now `Cargo.lock`, `Cargo.toml` and
`crates/*/Cargo.toml` (18 manifests, no `.rs` file, confirmed with `git ls-files`). The six documents
were regenerated; the only change inside them is `metadata.timestamp` and `serialNumber`, and the
gated rebuild proof (`AV_SBOM_REBUILD=1`) ran for real — 64 passed, 0 skipped — so the committed
documents are reproducible from the tree. A new test asserts no Rust epoch path matches any `.rs`
file, so the defect cannot return quietly.

### D3 — what a kit carries, and the zero-egress install proof

**The payload** (`kit_format` 2). A full kit of this tree is **327 MB, 11,285 files, 2 symlinks**:
`vendor/` 231 MB (180 crates, `cargo vendor --offline` — no network needed on this host), `images/`
58 MB (both tarballs, each saved only after its live digest matched its recorded one), `wheels/`
24 MB (22 wheels: the viewer's 21-package linux/aarch64/cp313 runtime closure plus a wheel of
`altavista` itself), `runs/` 4.2 MB (the four recorded kernel runs), `packs/` 3.8 MB (`web`,
`profiles`, `data-time`), `binaries/` 2.6 MB, `sbom/` 400 KB, and a 2.2 MB `KIT_MANIFEST`. The GMAT
pack was exercised once at full size in task 3a — 738 MB, 2,072 files, 183 symlinks, taking the kit
to 1.0 GB, still reproducible — and is deliberately absent from the proof kit (decision 8 below).

`web/` and `profiles/` are copied into **every** kit unconditionally, because an installed viewer
cannot start without them and together they cost 3.75 MB.

**A pack's internal symlinks are now carried verbatim and declared.** Round 1's rule ("a kit contains
no symlink at all") could not survive a real GMAT install, whose 183 symlinks are all relative,
same-directory `.dylib` re-exports, nor `web/node_modules/three`'s two. A symlink inside a copied
pack is now copied without dereferencing and listed in `KIT_MANIFEST`'s `pack_symlinks` with its raw
target; it is allowed only if that target is relative and stays lexically inside the pack; an
absolute or escaping target is a hard build-time refusal. `verify_manifest` still reports an
undeclared symlink, a changed target, a declared symlink that is missing, and any unsafe target —
round 1's D3-1 defence is intact and still under test.

**The install.** `scripts/kit/install.sh` (a POSIX wrapper over `install.py`) verifies the manifest
first and refuses on any finding, refuses a non-empty target, a wrong `kit_format`, a missing
section, a kit with no wheels or without the viewer's assets; then copies the kit tree, makes the
binaries executable, creates a venv and runs `pip install --no-index --find-links <kit>/wheels
altavista`, and writes an `INSTALL_RECORD` naming the kit manifest's own SHA-256, what was installed,
and the gaps the kit declared. It reaches the network nowhere.

**The proof** (`tests/test_kit_zero_egress_install.py`, gated, run for real by the manager at
`381b29d`, full output saved with the round's gate logs). Kit manifest
`36eb5e30a74d61f2827292178e829a4c2a4b444c42d2d067e407e8928a290de3`:

- **Image provenance.** `docker load` of each tarball, then a digest comparison before the image is
  used at all: `av-edge-plugin:local` recorded and loaded
  `sha256:38062a487a4346dc5a23eb26dcf00fe7d06e3fec9d6813e003fe612fcaef234e`;
  `altavista-cfs-lockstep:local` recorded and loaded
  `sha256:dea163a1bad929b53c27498e182dadc62ef0f63cb6a63ac73dd1ac31bf6cded2`.
- **No route out, before and after the install.** From inside the labelled `--internal` network, both
  `8.8.8.8:53` and `1.1.1.1:443` failed with **errno 101, "Network is unreachable"**, in
  **3.2×10⁻⁵ s and 3.8×10⁻⁵ s** before the install and **2.9×10⁻⁵ s and 4.7×10⁻⁵ s** after it —
  immediate, which is what distinguishes "no route exists" from "a connection timed out".
- **The install itself**, inside that network, from a read-only kit: **12.16 s**, printing the same
  manifest SHA-256 the host computed independently, and an `INSTALL_RECORD` listing 22 installed
  packages.
- **The demo path, from the installed kit.** The viewer server, started from the installed wheels,
  answered a real `GET /api/scenarios` over loopback with **200 `{"names": []}`**. `av-ingest-server`,
  the kit's own cross-built binary, printed `GRPC_LISTENING 127.0.0.1:50070` and
  `ADMIN_LISTENING 127.0.0.1:50071`. The edge plugin, from the kit's just-loaded image and joined to
  the ingest's network namespace, delivered **900 batches / 900 measurements, none rejected, chain
  head `d1d80d0b6cc9228aaa7479864b8a89f18be88382fb3772bd3c4cc3d7c2dc2698`**, and the ingest's own
  evidence endpoint read back `accepted_total=900, rejected_total=0`.
- **The kernel run reproduced from its hash.** All four recorded runs were re-hashed from the
  installed tree against `KIT_MANIFEST` and decoded far enough to compare each run's own
  `config_hash` (e.g. `demo_attitude_control` → `f3e755a6…cadb93`).
- **The command service is a named gap**, carrying the kit's own recorded reason (below).

### Decisions taken this round

1. **Worker tasks ran strictly sequentially, never in parallel.** Two workers committing to one
   worktree share an index; explicit pathspecs reduce but do not remove the risk of one staging the
   other's in-progress edits, and task 2's SBOM regeneration had to happen after task 1's Rust change
   was already in history. Wall time was the price; a clean history was the purchase.
2. **The blocking-announcement lock tests stay on the real host-wide lock.** Question 212(b) forbids
   asserting the shared lock is *free*; it does not forbid waiting for it. Those two tests prove that
   a genuinely contended acquire announces itself, which is only true of the real production path.
   Their cost is that they hold the real lock across a nested `cargo test` — see open item 6.
3. **The Rust SBOM epoch is narrowed rather than the SBOMs regenerated every round.** The alternative
   was a regeneration commit after every Rust change forever — a gate failure nobody caused, which
   question 207 already rejects and which D2-1 already rejected for the Python components. The
   narrowing is justified by what actually produces the document, and guarded by its own test.
4. **A pack's internal symlinks are carried verbatim, declared, and bounded by the pack root**;
   absolute or escaping targets are refused at build time, not at verify time. Dereferencing a real
   install's 183 `.dylib` links would have both bloated the kit and destroyed the install's own
   structure. `kit_format` moved to 2 because the format changed.
5. **The viewer's wheels are fetched with the network, once, at kit-build time**, behind
   `--with-wheels`, recorded in `KIT_MANIFEST` as `wheels.network_used` with every filename and
   SHA-256 — exactly question 154's "built with network once and installed with none". No test can
   trigger it.
6. **The cross-built binary cache is keyed on the source state, not the commit.** A cross-built
   binary is not reproducible (round 1 measured three hashes from three links of identical source),
   so a kit reuses cached bytes; keying that cache on the commit alone would have let a kit built from
   a dirty tree carry bytes compiled from a different tree, with nothing saying so.
7. **`web/` and `profiles/` are carried in every kit, unconditionally.** The alternative — leaving the
   proof to bind-mount them from this worktree — would have made "the demo runs from the installed
   kit" false for the viewer while the test still looked green.
8. **The proof kit deliberately omits the GMAT pack.** The pack is carried as bytes when asked for and
   was exercised at full size once, but this install is a macOS-native GMAT tree and the proof runs in
   a linux/aarch64 container, where it could only ever be hash-verified, never run. Carrying 738 MB
   into every proof run to hash it twice would have bought nothing. ADR-003's evaluation target is
   macOS; 213(c)'s container is the stand-in for an enclave, not for the install host.
9. **The kit does not yet carry its own installer** — the proof mounts `scripts/kit` read-only into
   the installing container. Question 214(b) enumerates the payload and does not name the installer,
   and the claim being proven is "no network, nothing from outside the kit's payload". Recorded as
   open item 4 rather than closed silently.
10. **D4 was not started.** After D3's second half and three review fixes there was not enough round
    left to do `secdeploy evidence` plus `scripts/kit/evidence.py` honestly, and a half-built evidence
    bundle is exactly the stub decision 12 of round 1 refused to ship.

### Defects found in review, and their root causes

Every one was found by re-running from the tree, not by reading a worker's claims.

- **The cross-build cache was keyed on `git_commit` alone** (task 3a, found by the manager, fixed in
  `519c612`). Root cause definitive: `RESULTS.json` in `<cache>/<commit>/` short-circuits the rebuild,
  so a kit built from a tree with uncommitted changes reuses a binary compiled from different bytes,
  and nothing in the kit says so. Now keyed on the commit plus a hash of `git status --porcelain` and
  `git diff HEAD`, recorded as `binaries.source_state`, with a pure-function test driving all three
  inputs. What the key cannot cover (the *content* of an already-named untracked file) is stated in
  its own doc comment.
- **`--with-binaries` used the network and the kit denied it** (task 3a, fixed in `519c612`). Root
  cause definitive: the cross-build container runs `apt-get update` and installs four packages before
  `cargo build`, while `scripts/kit/README.md` positively claimed the step had "no network of its
  own" and the manifest recorded nothing. Question 154 permits the use; it does not permit the
  silence. `binaries.network_used` is now recorded and the README corrected.
- **The zero-egress proof's viewer did not come entirely from the kit** (task 3b, found by the
  manager, fixed in `381b29d`). Root cause definitive and pre-existing: `pyproject.toml`'s
  `[tool.setuptools.packages.find]` includes `altavista*` only, so the wheel ships neither `web/` nor
  `profiles/`, and the test worked around it by bind-mounting this worktree's own copies. The kit now
  carries both as packs and the proof mounts no worktree asset into the viewer's container. The
  underlying packaging gap is open item 3.
- **The Rust SBOM epoch treadmill** (pre-existing, fixed in `327fe70`) — see task 2 above.
- **Three tests hardcode `/Users/probe` paths** (found by task 1's clone survey, reported not fixed):
  `tests/test_edge_ingest_mtls.py:117` and `tests/test_edge_identity_seccert.py:36` set `GMAT_ROOT`
  and `CFS_MIRROR_DIR` to absolute paths in the main worktree, overriding whatever the harness set,
  with no guard; `tests/test_edge_plugin_container.py:426` bind-mounts `/Users/probe/code/spoore`
  with no existence check, unlike its sibling in `test_proposer_container.py`. All three are the same
  class the clone gate caught. Open item 5.
- **One flake, root-caused and not believed on first sight**: a worker saw
  `test_edge_ingest_mtls.py::test_valid_seccert_leaf_is_accepted_through_nginx_and_batches_submit`
  fail three different ways while a full `cargo test --workspace` ran concurrently in the AI-plane
  worktree (load average 3.5). That test uses `--real-clock` against a fixed 5 s staleness budget by
  its own design. It passed in the manager's own gate run. Recorded as host contention, not a
  regression — the contention rule applied.

### Open items for the lead

1. **Question 214(a) is closed.** `deny.toml` carries `0BSD` and `PSF-2.0` with their reasons, and
   `docs/compliance/sbom/licence-exceptions.md` is deleted.
2. **`av-command` has never cross-built for Linux**, so the command service is the one demo-path
   component the zero-egress proof cannot start. Root cause read from the real compiler error:
   `regorus` 0.12.0 calls `alloc::vec::Vec::len` in a `const fn`, which the pinned
   `rust:1.85-bookworm` toolchain rejects (`add #![feature(const_vec_string_slice)]`). Bumping the
   pinned cross-build image is a decision with an image-digest consequence, so it is the lead's.
3. **The viewer cannot be installed from its own wheel.** The `altavista` wheel ships no `web/` or
   `profiles/`, and `altavista/profile.py`'s `PROFILES_DIR` is a fixed module constant with no
   parameter or CLI flag, so the proof's container bootstrap still has to assign it. The kit works
   around both by carrying the directories; the real fix is in `pyproject.toml` and `altavista/`,
   which this track did not touch.
4. **The kit does not carry its own installer.** `scripts/kit/install.sh`/`install.py` (and the
   `manifest.py`/`sbom.py` they import) are mounted from the repository into the installing container.
   Carrying them inside the kit is the obvious next step and would make "installs from the kit alone"
   literal rather than payload-only.
5. **Three tests still hardcode this host's absolute paths** (named above). A plain clone elsewhere on
   disk would fail two of them outright.
6. **`lock_docker_tests_announces_a_blocked_wait_never_silently` holds the real host-wide lock across
   a nested `cargo test`**, which serialises every other track's Docker-gated work for the length of
   that build. It is correct as written (decision 2); whether the gate can afford it is a scheduling
   question for the lead.
7. **`scripts/kit/build_kit.py` imports `packaging`**, which this project does not declare as a
   dependency — it is present only transitively through pip/pytest in the shared venv. Small, but it
   is the kind of thing that breaks a clone.
8. **Question 208(c)'s port map still blocks `av-ingest` and `av-proposer`** (carried from round 1).
9. **`tests/test_proposer_container.py` has now failed on this branch for two rounds** for question
   212(a)'s reason. Every P5 pytest count carries one failure that is not this track's.
10. **The network was used at kit-build time, by ruling, three ways**: `pip download` for the viewer's
    wheels, `apt` inside the cross-build container, and a `docker pull`-free image path that used
    none. `cargo vendor` needed none. All are recorded in the manifest; no test uses the network.
11. **D4, D5 and D6 are untouched**, and the E5 latency retake the lead owes (question 212) is still
    the blocker in front of D5.
