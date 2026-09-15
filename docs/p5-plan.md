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
