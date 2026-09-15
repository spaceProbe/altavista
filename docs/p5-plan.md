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
