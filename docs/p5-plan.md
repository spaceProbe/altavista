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

## Status (P5 manager, 2026-09-16) — round 3

The lead's merge (`3bcbd63`) broke eight Python tests; all eight are fixed and root-caused.
**Every item of question 217 is closed** — (a) through (g) — and **D4 is complete**: one hashed
evidence bundle collects every component's control matrix, a cross-component coverage table, the
deficiency list, real ledger `verify` results and the SBOM and kit hashes, with `secdeploy
evidence` run for real over a placement we stood up. **D6 is complete as far as this host allows**:
the `fedora-fips` runbook rendered, a Lima Fedora VM booted, and the FIPS preflight ran for real
and failed closed. D5 was not started — the lead's E5 latency retake (question 213(g)) still gates
it.

### What landed on `edge`

| Commit | What |
|---|---|
| `c42cc06` | 217(g): the fragment's port check reads the owned port map; av-gateway's renamed constant |
| `4bb82a2` | The six Rust SBOMs regenerated for the `rust-version` epoch move, and the epoch rule's other half documented |
| `48fd3cd` | 217(b): the wheel ships `altavista/web/` and `altavista/profiles/`, with a five-step `PROFILES_DIR` override chain |
| `cee62f9` | The two Python SBOMs regenerated for `48fd3cd`'s `pyproject.toml` epoch move |
| `e84b303` | The kit stops carrying `web`/`profiles` as separate packs (`kit_format` 3) |
| `1bdbbc3` | 217(a): the kit's cross-build pin moves to `rust:1.90-bookworm`, compared to its recorded digest before use |
| `ba72dab` | A deleted source file no longer survives into every later wheel |
| `45f87a9` | 217(e): `packaging` declared in the `dev` extra, where `build_kit.py` imports it |
| `9a73636` | 217(c): the kit carries its own installer (`kit_format` 4) |
| `b442bbc` | The two Python SBOMs regenerated for `45f87a9`'s `pyproject.toml` epoch move |
| `614cda4` | 217(d): `GMAT_ROOT`/`CFS_MIRROR_DIR`/spoore resolved, never hardcoded to `/Users/probe` |
| `71bb21e` | 217(f): the waiting-announcement lock test moves off the real host-wide lock |
| `0b435e2` | D4a: the offline half of the evidence bundle, `docs/compliance/BUNDLE.md`, three acceptance tests |
| `18d923a` | `docs/secdeploy-upstream.md` proposal 2: manifest-driven evidence collection |
| `3f53aca` | The bundle's hash excludes `git_commit`, so its committed record is re-verifiable |
| `30dc71c` | D4 live half: `secdeploy evidence` over a real placement, real component `verify` responses |
| `e2b0b60` | D6: the `fedora-fips` dry-run render, and upstream proposal 3 |
| `6c70912` | D6: the Lima Fedora VM record — it booted, the preflight ran and failed closed |
| `7b853f1` | The manifest doc names `KIT_FORMAT` instead of a number two bumps stale |
| `0792465` | A readiness-failure path no longer hangs the whole test session (eleven sites) |

### Gate counts (run by the manager at `0792465`'s tree, no worker active)

- `cargo test --workspace --exclude av-kernel --no-fail-fast`: **878 passed, 0 failed, 3 ignored**
  across 110 test binaries, exit 0. Round 2 measured 835 across 109; the +43 and the extra binary
  arrived with the lead's merge, not from this round (this round added no Rust test).
- `cargo clippy --workspace --all-targets -- -D warnings`: exit 0, zero warnings. No `#[allow]` was
  added anywhere this round — `git diff 3bcbd63..HEAD -- '*.rs' | grep -c '^+.*#\[allow'` is 0,
  grepped, not assumed.
- `cargo deny check`: exit 0, "advisories ok, bans ok, licenses ok, sources ok", with exactly the
  six `warning[wildcard]` lines question 207 accepts. Also five `warning[license-not-encountered]`
  (ISC, MPL-2.0, CC0-1.0, 0BSD, PSF-2.0 — allow-list entries this Rust graph does not reach; round
  2 saw two, the difference is the merge's own dependency changes) and seven `warning[duplicate]`.
  None is an error.
- `.venv/bin/python -m pytest -q -rs`: **1 failed, 716 passed, 13 skipped**, 874.58s, 730 collected.
  Round 2's baseline was 1 failed, 661 passed, 13 skipped. The one failure is
  `test_edge_ingest_mtls.py::test_valid_seccert_leaf_is_accepted_through_nginx_and_batches_submit`,
  the `--real-clock`-against-a-fixed-5 s-staleness-budget test round 2 already recorded as host
  contention; the contention rule was applied rather than believed on first sight — re-run alone
  with the fixture binaries pre-built, that file and the six others that had failed passed:
  **108 passed in 119.25s, 0 failed**. The AI-plane track was running `cargo test --workspace` in
  its own worktree throughout.
- `buf lint proto`: exit 0, no output.
- secdeploy over the merged manifest, offline against the user's own checkout: `verify` exit 0
  ("✓ manifest valid", "✓ target assets present", "✓ topology valid — domain altavista.internal,
  1 resource(s)", all 17 components placed) and `plan macos` exit 0.
- **The kit built twice** from this tree with every gated flag
  (`--with-images --with-vendor --with-wheels --with-binaries --copy-pack data-time`): manifest hash
  **`cb7133e69462db70b1b9b4f800de14959020c6a72f037306f9f7dd1ac11cc82f`** both times, the two
  `KIT_MANIFEST` files byte-identical (`cmp`, not eyeballed). 330 MB.
- **The evidence bundle built twice** over the same state: `bundle_sha256`
  **`e688766e517c9c94303c162ecb8acfccfa656b6d81a0ae6243e37df6e3ef9b78`** both times, the two
  `bundle.json` files byte-identical (`cmp`). Without a kit — the invocation
  `docs/compliance/BUNDLE.md` records — the hash is
  **`f5c4c12e1106c5507a3d9465e9d1d69514f3027d82176a3ea5927047a3104f8a`**, which is the number
  committed in that file and which a test now re-verifies on every run.

Full outputs for every gate are saved with the round's gate logs.

### Task 1 — the eight post-merge failures

Both causes were reproduced before anything was changed.

**(a) Two port failures, one cause.** The merge replaced av-gateway's inline
`env_or("AV_GATEWAY_BIND", "127.0.0.1:50071")` literal with a named `DEFAULT_BIND` constant, so
`[ports.av-gateway]`'s hand-written regex matched zero times. Fixed — but 217(g) asked for more
than a regex repair, and the more turns out not to be literally executable: **the owned port map
in `docs/architecture.md` section 4 has no row for `av-ingest` and none for `av-proposer`**, and
neither binary has gained a default bind (`av-ingest-server.rs` still fails with
`--grpc-bind is required`; `av-proposer.rs` still takes its address only from
`--serve-model-service HOST:PORT`). There is nothing in the owned map to fill those two rows from,
and inventing a number is exactly what round 1's decision 4 refused. So 217(g) is honoured a
different way: `ports.py` now parses the real markdown table (`load_owned_port_map`), every
`kind = "const"` row is checked **three** ways — fragment, owned map, source constant, all three
must agree — and the two `kind = "gap"` rows gain a positive check that they are still **absent**
from the map. The day the AI-plane adds either row, the gap check fails and forces a `const` row.
Four new tests drive each case from a mutated copy of the fragment or of the markdown under
`tmp_path`, never the real files. `av-viewer` is not in the map either; rather than a silent skip
it carries an explicit, commented `owned_map_exempt = true`, and a test proves removing that
exemption makes the finding reappear.

**(b) Six stale Rust SBOMs, one legitimate cause.** Commit `db1e858` raised the workspace
`rust-version` to `1.87`. `Cargo.toml` is a real `RUST_EPOCH_PATHS` input, so all six Rust epochs
moved together, `2026-09-15T12:30:33Z` → `2026-09-15T18:02:25Z`. **That is the rule working, not a
treadmill**: round 2 narrowed the epoch away from `.rs` files precisely because a source edit
cannot change a resolved dependency graph, while a manifest edit can and does. The six documents
were regenerated, `SHA256SUMS` updated, and the rule's other half is now written down in both
`RUST_EPOCH_PATHS`'s own doc comment and `docs/compliance/sbom/README.md`, with the operational
consequence stated plainly: a commit touching `Cargo.lock`, the workspace `Cargo.toml` or a crate
manifest must be followed by an SBOM regeneration in a separate, later commit, and
`test_the_epoch_is_never_wall_clock` is what enforces it. A new test states the property
positively — all six share one epoch, equal to git's own committer date for that path set,
recomputed independently of `sbom.git_epoch`. The gated rebuild proof ran for real:
`AV_SBOM_REBUILD=1` → 65 passed, 0 skipped, 147.65s.

### Question 217 — every item, and what it cost

**(b) The viewer ships its assets in its own wheel.** A `setup.py` with a `build_py` subclass
copies `web/` and `profiles/` into the build tree as `altavista/web/` and `altavista/profiles/`;
neither source directory moves. Verified independently by the manager, not from the worker's
claim: a wheel built from this tree carries **127 files under `altavista/web/`** (117 regular files
plus the 10 the two relative symlinks dereference into) and **8 under `altavista/profiles/`**, with
**zero** top-level `web/`/`profiles/` entries — no site-packages namespace pollution.
`altavista/profile.py` gains `resolve_profiles_dir` with a five-step order (explicit argument,
`ALTAVISTA_PROFILES_DIR`, the module-level `PROFILES_DIR` if assigned, the packaged copy, the
in-repo copy); an in-repo developer run still gets the worktree's `profiles/`, because step 4
provably does not exist in a source checkout. `altavista/server.py`'s `WEB_DIR` gets the same
treatment and `__main__.py` gains `--profiles-dir`. The kit stops carrying the two packs, and
`install.py`'s deleted pack-presence refusal is **replaced**, not dropped: it now opens the kit's
`altavista` wheel and checks its namelist for the viewer's assets.

**(c) The kit carries its own installer.** `<kit>/installer/` holds `install.sh`, `install.py`,
`manifest.py`, `sbom.py`, `licences.py` — the exact, recursively checked import closure, none of
which needs a third-party package. The proof now runs `/kit/installer/install.sh` from the
read-only kit mount and mounts nothing from this repository; `_assert_no_repo_source_mounted`
builds the `-v` list in one place and asserts positively that no element points inside the repo
root. The code states its own limit rather than overclaiming: a self-carried, self-verifying
installer proves internal consistency, never authenticity — the anchor is the manifest hash held
independently of the kit.

**(d) Three tests that hardcoded `/Users/probe`.** `GMAT_ROOT`/`CFS_MIRROR_DIR` now resolve through
`altavista/test_env.py` (inherited environment, then this worktree's own entries if they are real
directories, else a visible skip naming the variable to set); the spoore bind-mount's host source
resolves from `AV_SPOORE_DIR` or a sibling checkout and carries the same visible-skip guard
`test_proposer_container.py` already had. Proven the way a clone gate would: the three files were
run from a relocated copy of the tracked tree with both variables unset — 4 passed, 9 skipped, every
skip naming what to set, no attempted `cargo build` against a path that does not exist. The
container-side *destination* stays the literal `/Users/probe/code/spoore` because the root
`Cargo.toml` bakes that path into `spoore-cdm`'s path dependency, which is out of this track's
scope; that is recorded, not hidden.

**(e) `packaging`** is declared in the `dev` extra, held there by an AST-based test that names the
module importing it, plus a `python -S` test proving the installer still imports with only the
standard library.

**(f) The waiting-announcement lock test** moved to a private lock path under `repo_scratch_dir()`,
handed to its child through `AV_LOCKSTEP_PROBE_LOCK_PATH` on the child's environment (never a
mutation of the test process's own). The announcement code under test, `lock_docker_tests_at`, is
exactly what `lock_docker_tests` calls in production, so nothing is given up. Measured, not
asserted: with a forced cold nested compile the real host-wide lock was held **~8.0 s**; after the
change it is **never acquired** — 0 s. Round 2's decision 2 is corrected in the code comments
rather than left claiming the opposite.

### D4 — the evidence package

`scripts/kit/evidence.py` writes one JSON bundle to `out/evidence/bundle.json`, never committed;
`docs/compliance/BUNDLE.md` records what it is, the exact command, the hash, and what the hash
depends on.

The real numbers: **6 components, 176 control-matrix rows** (32/29/30/28/29/28, matching the
committed files), **41 distinct practices** named by at least one component, **223 (practice, mark,
component) attributions**, and **44 deficiencies** (15/4/10/6/5/4). The denominator is stated
honestly in the bundle itself: "practices named by at least one component", *not* 800-171's 110 —
this repository carries no authoritative list of all 110 identifiers and one was not invented.
A row naming two practices (`3.1.1 / 3.1.2`) contributes to both; the reconciliation test proves a
bijection between every `(component, row, practice, mark)` and every coverage attribution, and
prints the numbers.

The live half: **`secdeploy deploy macos` cannot stand up any AltaVista component**, measured and
not inferred — `targets/macos.py`'s `deploy()` is hand-written for secdeploy's own component names
with no per-manifest dispatch, and a dry-run over our merged manifest printed steps for none of our
eight. So the charter's sanctioned fallback was used: our own placement. `av-dynamics-service`,
`gmat-service` and `av-ingest` were brought up and their real `/admin/api/evidence/verify`
responses recorded verbatim (`{"ok": true, "checked": 4, ...}` for gmat-service, and so on);
`av-command`, `av-gateway` and `av-edge-plugin` are recorded by name with the reason each could not
be. `secdeploy evidence` ran for real with its output redirected inside our own `out/` — it
contributed its deploy-audit chain verify (`{"ok": true, "checked": 0, "brokenAt": null}`) and five
`skipped` records, because **`secdeploy/src/secdeploy/evidence.py` line 39 hardcodes
`COMPONENTS = ("secrouter", "seccert", "secllm", "secchat", "secrecorder")` and `collect()` loops
over that constant rather than the manifest it was just handed**. That is proposal 2 in
`docs/secdeploy-upstream.md`; proposal 3 is the same shape one level up, in the per-target
`deploy()`.

The three required tests all pass, plus two more: byte-identity across two runs; a tampered ledger
making both the verifier and the bundle say so (`{"ok": false, "broken_at_seq": 3, ...}`, and the
bundle hash changes); every control-matrix row in the coverage table exactly once; the committed
`BUNDLE.md` hash reproducing from the tree; and a test proving that last check can actually fail.

### D6 — the Fedora target

`secdeploy deploy fedora-fips --dry-run` renders, exit 0, over our merged manifest — and renders
**nothing at all for seven of our eight components**; `av-viewer` appears once, only as a certbot
`-d av-viewer.altavista.internal` SAN flag, because it is the one component with `fronted = true`.
Root cause read from the source with line numbers: `targets/fedora_fips.py`'s `SERVICES` tuple and
`_include()`. Recorded in `docs/compliance/fedora-fips.md` and proposed upstream.

Lima **did** boot Fedora 44 Cloud aarch64 (~14m53s including a 504 MiB one-time image fetch), and
secdeploy's own `deploy/fedora-fips/fips-preflight.sh` ran for real inside it:

```
FIPS preflight FAILED: kernel FIPS mode is not enabled — run 'sudo fips-mode-setup --enable' and reboot
```

It fails closed on a stock image, which is correct. One step further, root-caused rather than
assumed: the preflight's own remediation binary, `fips-mode-setup`, **does not exist on Fedora 44**
(`dnf provides` finds no package owning it; only `update-crypto-policies` ships), so the
remediation the preflight prints cannot be followed on that image. What cannot run here at all is
listed with its reason: hardware attestation (no vTPM under `vz`), a real HSM, a real enclave
network. The VM was stopped and deleted; `limactl list` and `colima status` are byte-identical to
before.

### Decisions taken this round

1. **217(g) is honoured by anchoring the checker to the owned map, not by filling two rows that
   the map does not define.** The map has no `av-ingest` or `av-proposer` row and neither binary
   has a default bind; the lead's instruction assumed otherwise. Inventing ports would have
   created exactly the drift question 208(c) exists to close. Instead every `const` row is now a
   three-way agreement and every `gap` row asserts its own absence from the map positively, so the
   day the map gains either row the gate fails and forces the update. Open item 1.
2. **`av-viewer` gets a declared `owned_map_exempt`, never a silent skip.** It is not in the owned
   map (that table's scope is the DRM/kernel network path) and it sits behind secproxy. The
   exemption is one commented line, and a test proves removing it makes the missing-provenance
   finding reappear.
3. **A workspace-manifest edit moving all six Rust epochs at once is correct and is now written
   down as such.** The alternative reading — "the epoch is too broad again" — would have narrowed
   it past the truth: a manifest genuinely changes what `cargo auditable` and `cargo metadata`
   resolve. The rule is now stated in both directions with `db1e858` as the worked example.
4. **The wheel carries `altavista/web/` and `altavista/profiles/`, not top-level `web/`/`profiles/`.**
   A top-level layout would have made the existing `parent.parent / "web"` lookup work with no code
   change at all, which is precisely why it is wrong: it installs two generic names into the
   site-packages root where any other project could collide with them.
5. **The kit's cross-build pin was this track's to fix, not the lead's.** 217(a) says av-command
   cross-builds "once the MSRV pins of question 215 reach this branch"; they reached every
   `services/*` script but not `scripts/kit/build_kit.py`, which is ours and was written after the
   merge. Moving it to the 1.90 digest closed round 2's open item 2 — `av-command` cross-built for
   the first time on this branch (5,059,016 bytes) and now starts from the kit in the proof.
6. **`kit_format` moved twice this round, 2 → 3 → 4, deliberately.** Dropping the `web`/`profiles`
   packs and adding a required `installer/` each change what an older installer may assume; a
   version bump makes an old installer refuse a new kit for the right reason instead of
   mis-installing one with no static root.
7. **`install.py`'s viewer-asset refusal was replaced, not deleted.** The old check asked whether
   the kit carried two packs; the new one opens the kit's own wheel and checks its namelist. A
   safety check whose subject moves must move with it, not disappear.
8. **The evidence bundle's hash excludes `git_commit`.** Including it made the number recorded in
   `docs/compliance/BUNDLE.md` stale the instant the next commit landed — measured: `7d0a2053…`
   became `d5df21cf…` one commit later, and that commit touched only
   `docs/secdeploy-upstream.md`. This is question 214's own platform lesson ("a committed artifact
   whose input set includes its own commit is stale the moment it lands"), fixed the same way it
   was fixed for the SBOMs. A test now re-verifies the committed number, and another test proves
   that check can fail.
9. **The bundle's coverage denominator is what the matrices name, and says so.** Claiming coverage
   against all 110 practices would have required inventing an identifier list this repository does
   not have. The bundle carries the caveat in its own `denominator_note`, so a reader cannot mistake
   41 for 110.
10. **`secdeploy deploy` was never run for real.** secdeploy is the user's checkout, read and run
    only; a real deploy writes into its own `out/audit/`. Only `--dry-run` and read-only commands
    were used, with every output redirected inside our tree. D4's placement is ours, which the
    charter explicitly permits.
11. **Workers ran strictly sequentially again**, for round 2's decision-1 reason (one git index),
    and the sizing was wrong once: the 217(b) worker took 2,363 tool calls against a 300-call
    brief. The task was genuinely large (packaging plus kit plus the proof), and it should have been
    split at the kit boundary. Recorded so the next round splits it.
12. **D5 was not started**, per the charter: question 213(g)'s latency retake is the lead's and
    still ungiven.

### Defects found in review, and their root causes

Every one was found by re-running from the tree, not from a worker's claims.

- **The kit's cross-build pin was three toolchain versions behind** (found by the manager from a
  worker's "the proof skips" report, fixed in `1bdbbc3`). Root cause definitive, read from the
  compiler: `rustc 1.85.1 is not supported … requires rustc 1.87`. The consequence was worse than a
  skip in a log — D3's zero-egress proof, the whole of round 2's deliverable, had gone dark on this
  branch. Nothing in `build_kit.py` compared that image to a recorded digest either; it does now.
- **A file deleted from `web/` survived into every later wheel** (found by the manager, fixed in
  `ba72dab`). Root cause definitive and reproduced by hand before it was reported: `copy_tree` is
  purely additive and `pip wheel .` never cleans `build/`, so a probe file added, built, deleted and
  rebuilt was still in the second wheel. `build_py` now removes the destination first; the test
  drives the real `setup.py` against a synthetic tree under `tmp_path`.
- **The evidence bundle's own recorded hash was stale on arrival** (found by the manager, fixed in
  `3f53aca`) — decision 8 above.
- **A readiness-failure path hung the entire test session** (found by the manager *in* the gate,
  fixed in `0792465`). Root cause definitive, from a stack sample of the stalled process, not a
  guess: `proc.stdout.read()` is a readall that returns only at EOF, and EOF arrives only when the
  child exits — but a readiness wait times out precisely when the child is still alive. Under the
  AI-plane track's concurrent `cargo test --workspace`, `test_dynamics_service_rs.py`'s 90 s budget
  was exceeded, the fixture reached that line, and pytest sat silent for **34 minutes**. The test
  reported nothing at all, which is strictly worse than reporting the failure it was built to
  report — a hang is the one failure mode that leaves no trace. Grepping for the shape rather than
  assuming it was unique found **eleven sites across nine files**, including four inside `assert`
  *messages* (evaluated only on failure, against a live child with its stdin still open) and three
  where a lone `terminate()` preceded the readall — no help at all for nginx, which outlives
  SIGTERM. All eleven now use one helper that terminates, reads with a timeout, and escalates to
  kill. The two sites where the child is already killed are left alone, with the reason.
- **A manifest doc line said `kit_format` was 2** while the constant beside it had moved to 3 and
  then 4 in the same round (fixed in `7b853f1`). Small, and exactly the failure mode a hand-copied
  number has; the line now names `KIT_FORMAT`.
- **One `pytest.ini`-level contention artefact, root-caused and not believed on first sight.** The
  first full gate run produced 28 errors and 1 failure; every single one was a `cargo build`/`cargo
  run` hitting its 900/600/300 s timeout or a gRPC readiness wait expiring, while the AI-plane
  track held the shared cargo package-cache lock. Re-run alone with the fixture binaries pre-built:
  **108 passed, 0 failed, 119 s**. Contention, definitively — and the reason the hang above was
  found at all.

### Open items for the lead

1. **Question 217(g) could not be executed as written.** The owned port map carries no `av-ingest`
   and no `av-proposer` row, and neither binary has a default bind, so the two port-0 rows stay
   gaps. They are now guarded from both sides and will fail the day either row appears. If the
   lead wants real ports, someone must first give those two binaries defaults — and that is the
   AI-plane's crate, not ours.
2. **`av-proposer/build.rs`'s `SPOORE_PROTO_ROOT` and the two `services/*/build-image.sh`
   `SPOORE_HOST_PATH` values still hardcode `/Users/probe/code/spoore`**, the same class 217(d)
   just closed in the tests. `av-proposer` is off-limits to this track; the build scripts were out
   of the prescribed scope. Both want the same `AV_SPOORE_DIR`-with-default treatment.
3. **The root `Cargo.toml` bakes `/Users/probe/code/spoore/crates/spoore-cdm` into a path
   dependency**, which is why the container-side mount destination is still that literal. Nothing
   below `Cargo.toml` can fix it, and editing `Cargo.toml` stales six SBOMs, so it needs to be
   someone's deliberate commit with the regeneration attached.
4. **`secdeploy` cannot deploy or collect evidence for any AltaVista component**, for two separate
   hardcoded lists — `evidence.py`'s `COMPONENTS` tuple and each target's own `SERVICES` tuple.
   Proposals 2 and 3 in `docs/secdeploy-upstream.md` are written and ready to send alongside
   proposal 1. Until then D4's live half is our own placement by necessity, not by preference.
5. **Fedora 44 has no `fips-mode-setup`**, so secdeploy's own FIPS preflight prints remediation
   that cannot be followed on the image its docs point at. That is upstream's to decide; it is
   recorded in `docs/compliance/fedora-fips.md` with the evidence.
6. **D5 is still blocked on question 213(g)'s latency retake**, now for a fourth round.
7. **The two-track host is the largest single cost in this round's wall time.** The gate's first
   full pytest took 79 minutes and produced 29 contention artefacts; alone, the same work takes
   two. Whatever the lead decides about scheduling, the numbers above are the measured price.
8. **`altavista/test_env.py` is test-support code that now ships inside the wheel.** It is small and
   dependency-free, and `build_kit.py` deliberately does not import it, but it is worth a decision
   whether test helpers belong in the installed package or somewhere the wheel does not carry.
