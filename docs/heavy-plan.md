# The heavy track: plan

The AI-plane team's charter after `aiplane-plan.md` closed (question 215), taken by the lead
on 2026-09-15 as its standing recommendation the user had not redirected. Decisions in
`open-questions.md` question 216. This is the non-board half of P3 in `architecture.md`
("object store, tiler, streaming layers and entity module in the viewer, catalog,
claim-check; exit: 10 GB overlay streams") on the substrate ADR-003 decided: MinIO (S3 API)
for payloads, Postgres with PostGIS for the catalog, jobs on a runner, ClickHouse for
analytics, and the rule that nothing on the hot path ever calls the object store.

## Goal

A heavy payload (imagery, terrain, a point cloud or mesh) lands in the object store, is
catalogued with its extent and label, is tiled by a job whose completion is an event on the
log, is served by a tile gateway that enforces labels per layer and per query, and streams
into the viewer through a budgeted, cancellable layer that never stalls a frame, proven at
ten gigabytes; hot-track messages carry asset references only; every artifact is hashed.

## What exists

- The viewer: the frame graph, floating origin, the tiled globe (`web/js/globe.js`,
  `globe_lod.js`, `tiles_layer.js`, the vendored `3d-tiles-renderer` 0.5.2 and `three`),
  imagery as a profile setting, the tiling layout, multi-viewport, the 2D map, ground tracks,
  contact windows, sensor footprints, the timeline; headless `*_check.mjs` harnesses driven
  by pytest, including the headless-Chrome check the AI-plane track added.
- CDM v1 `Asset` and `Provenance` (core and entity protos), `Label`, the frame registry; the
  file-backed logs and ledgers of the ingest and command services; `av-gateway`'s
  label-aware read path and its OIDC caller authentication (question 215); the owned port map.
- Docker on Colima with the host-wide lock; secdeploy tiers with `store`, `catalog` and
  `design` already named in ADR-003; the P5 kit that will have to carry what this plan adds.
- Not yet: any object-store client, catalog schema, job runner, tiler, tile gateway,
  streaming-layer budget, claim-check on any message, or a ten-gigabyte anything.

## Isolation

The team keeps the worktree `/Users/probe/code/AltaVista-aiplane` on branch `aiplane` (the
name is history; the plan is this file). New code lives in new crates `crates/av-store`
(the S3 client and claim-check), `crates/av-catalog` (the schema and queries),
`crates/av-jobs` (the runner and the tiler job), `crates/av-tiles` (the tile gateway), a new
proto file `proto/altavista/v1/heavy.proto` (this track owns it: job messages, tile-set
manifests, catalog records; every other proto additive only under `buf breaking` and `buf
lint`), new modules under `web/js/layers/` and `web/js/entities/`, and `altavista/heavy/`.
It does not edit the kernel's executor, router or fault modules, the ingest, the edge crates
or the command service. The P5 team works in its own worktree at the same time; the tracks
share nothing but `develop`, which the lead merges into both between rounds and each back
after acceptance. MinIO and Postgres run as labelled containers from digest-pinned images
pulled once at setup (question 216); nothing links them.

## Rules that bind this track

Every standing rule in `teamlog/2026-09-02-team-1.md` and `open-questions.md` applies:
questions 148, 154, 156 and 207, 157, 194, 199, 212 (a test trusts an image only after
comparing it to its recorded digest), the crypto rule of ADR-004 (the S3 client's TLS on the
system OpenSSL, no `ring`, no `sha2`; MinIO's and Postgres's own TLS verified to use the host
OpenSSL before the P5 kit carries them), question 44 (3DTilesRendererJS behind our layer
interface, budgets and cancellation ours), question 45 (per-layer label enforcement in the
gateway), question 46 (floating origin per frame, jitter tests), question 51 (the viewer
fully self-contained, egress blocked), and the contention rule. Root-cause every defect or
record why there is no path.

## Milestones

**H1 The store and the claim-check (`crates/av-store`).** An S3 client over the system
OpenSSL (`rustls-openssl` or `native-tls` as the platform already does; no `ring`) against a
labelled MinIO container from a digest-pinned image; put and get by content hash, every
object stored under its SHA-256 with its label and provenance as object metadata; the
claim-check: an `Asset` reference (bucket, key, hash, size, label) that hot-track messages
carry instead of bytes, with a test that no crate on the hot path (`av-ingest`, `av-track`,
`av-command`) depends on `av-store`, enforced by `cargo tree`. Tests: round trip by hash,
a corrupted object refused on read, a label above the caller's clearance refused, the
dependency assertion.

**H2 The catalog (`crates/av-catalog`).** Postgres with PostGIS in a labelled container:
assets with their hash, label, extent (geometry or a bounding volume in a declared frame),
time range, provenance and job lineage; migrations committed and hashed; queries by extent,
time and label; every query's label filter proven by a test that a higher-labelled asset is
invisible to a lower clearance. The `av-gateway` learns a `catalog` selector through the
same authentication it already has.

**H3 The job runner and the tiler (`crates/av-jobs`).** A runner that takes jobs from a
durable, hash-chained file-backed queue (the pattern of the ingest log; a broker later,
ADR-003), runs each in a labelled container or as a process, records inputs and outputs by
hash, and emits the job's completion as an event on the log with the output assets'
references. The first job: the tiler, imagery and terrain to the globe's tile layout and a
mesh or point cloud to 3D Tiles, deterministic for the same input hash, output tile sets
stored under a manifest whose hash is the tile set's identity. Tests: a job replayed from
its recorded inputs gives byte-identical outputs; a failed job records a typed failure;
the completion event carries the manifest hash.

**H4 The tile gateway (`crates/av-tiles`).** Serves tiles from the store by manifest hash
and tile address, enforcing labels per layer and per request through the shared OIDC
verifier, with range requests, caching headers the viewer's cache rules already follow and
counters for every refusal; fronted by the nginx template pattern across hosts. Tests: a
tile above the caller's clearance is refused and counted; a tile's bytes equal the stored
object's; a manifest-hash mismatch is refused.

**H5 Streaming layers in the viewer (`web/js/layers/`).** The streaming-layer module owns a
priority queue by screen-space error and view distance, a declared memory budget, and
cancellation of in-flight requests when the view moves; it wraps the vendored
3DTilesRendererJS and the globe's imagery and terrain loaders behind one interface; a
headless harness measures frame time while a large tile set streams and asserts no frame
exceeds the budget and the memory budget is never crossed. The proof at scale: a synthetic
ten-gigabyte tile set generated by the tiler from a fixture, streamed through the gateway
into the viewer with the frame-time and memory assertions holding, and the numbers recorded
with the host state. The lead drives it in a browser at acceptance.

**H6 The entity module (`web/js/entities/`).** Question 50's order beyond what exists:
covariance ellipsoids and keep-out volumes from the covariance the kernel already carries,
glTF asset models with attitude from the attitude stream, instanced markers and trails
budgeted like the layers; headless checks for each, and the RIC jitter test extended to a
model at a ten-metre RPO range (question 46).

**H7 Deployment and the kit.** The `store`, `catalog` and `design` tiers declared in the P5
suite fragment with the images' digests, the tile gateway's port in the owned map, control
matrices for `av-tiles` and `av-jobs`, the SBOMs, and the kit carrying the MinIO and
Postgres images and a small tile set; a proposal to the P5 team for anything the kit builder
needs, in `docs/p5-plan.md`'s status.

## Exit

A fixture payload put in the store by hash, catalogued with its extent and label, tiled by a
job whose completion is an event carrying the manifest hash, served by the gateway with a
label refusal counted, and streamed into the viewer at ten gigabytes with the frame-time
and memory budgets holding and the numbers recorded; a hot-track message carries only the
asset reference; the store, catalog and gateway are tiers in the suite with their digests,
matrices and SBOMs. Every number in the status section is traceable to a commit and a hash.
