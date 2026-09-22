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

## Status (heavy manager, 2026-09-16) — round 1

**H1 and H2 are delivered. H3 is not started and is deferred to round 2**, with the reason
recorded below rather than as a silent omission.

### What landed, one commit per accepted task

| Commit | What |
| --- | --- |
| `2c38271` | H1a: `crates/av-store` — the SigV4 signer on the system OpenSSL, the content-addressed key layout, the label/provenance metadata codec, the clearance ladder, `verify_payload`, the hyper/hyper-openssl client, and the `cargo tree` claim-check test |
| `8a9b39f` | H1b: `crates/av-store/tests/minio_store.rs` proved against a real MinIO container by recorded digest; `ManagedContainer::run_local`/`local_image_id`/`recorded_digest_gate` added additively to `av-lockstep`; `services/store/run-dev-store.sh`; two H1a review findings fixed |
| `24c1527` | The flaky cross-language `flock` test fixed: never assert the host-wide lock is free (question 212(b)'s Python ruling, applied to the Rust side it had missed) |
| `2a1d511` | H2a: `crates/av-catalog` — the hand-rolled PostgreSQL v3 wire client with SCRAM-SHA-256 on OpenSSL, the extended query protocol, and an in-process fake-server test suite |
| `d0a86f7` | H2b: the catalog schema, its hashed and drift-checked migrations, the PostGIS footprint, and `find_assets` with the label filter in the SQL — proved against a real PostgreSQL 17 + PostGIS 3.5 container |
| `268bca9` | H2c: `proto/altavista/v1/heavy.proto`, the additive `GATEWAY_SELECTOR_CATALOG` on `authority.proto`, and the catalog selector on `av-gateway` behind the authentication it already had |

### The images, pulled once at setup and recorded

| Tier | Reference | Digest | Recorded in |
| --- | --- | --- | --- |
| `store` | `quay.io/minio/minio:RELEASE.2025-04-22T22-12-26Z` | `sha256:a1ea29fa28355559ef137d71fc570e508a214ec84ff8083e39bc5428980b015e` | `services/store/IMAGE_DIGEST.md` |
| `catalog` | `imresamu/postgis:17-3.5-alpine` | `sha256:f8a700accce9a1fb24e14b73a3abf8e90439ce8f192c57b8cb93cc650694e8dd` | `services/catalog/IMAGE_DIGEST.md` |

Both were pulled by digest once, before any test ran; no test pulls (question 154). Every
docker-gated test parses the digest out of the `IMAGE_DIGEST.md` beside its service **at test
time** — the recorded digest has exactly one home — compares it to the running image, and
refuses rather than trusts on a mismatch (question 212(a)). Verified at review by recording a
deliberately wrong digest: all five store tests and all nine catalog tests printed a named
`SKIPPED` line on real stderr, visible without `--nocapture`, instead of passing.

### H1: the store and the claim-check (`crates/av-store`)

No S3 client crate could be used. `aws-sdk-s3` and every `reqwest`-based alternative reach TLS
through `rustls`, which pulls `ring`; both names are in `deny.toml`'s `[bans] deny` list under
ADR-004's crypto rule. The crate therefore signs its own requests: `src/sigv4.rs` implements
AWS Signature Version 4 with `openssl::sha::sha256` and `openssl::pkey::PKey::hmac` +
`openssl::sign::Signer` and nothing else, and `src/client.rs` issues them over `hyper` with
`hyper-openssl` for the `https` path — the same OpenSSL connector `crates/av-grpc/src/tls.rs`
already uses. The date stamps are a parameter, never a clock read, and
`amz_date_from_unix_seconds` does the calendar conversion by hand so neither `chrono` nor
`time` enters the tree. **No new registry crate entered `Cargo.lock` for this crate at all**;
every dependency was already resolved in this workspace.

The signer is pinned to AWS's own published `get-vanilla-query-order-key-case` vector — the
canonical request, the string to sign, the derived signing key and the signature
`b97d918cfa904a5beff61c982a1b6f458b799221646efd99d3219ec94cdf2500`. At review that chain was
re-derived independently with the `openssl(1)` CLI, a different implementation from the crate's
own, so the known-answer test is anchored rather than circular.

Objects are stored under `<prefix>/<hh>/<hh>/<sha256 hex>`; the `Label` and `Provenance` ride
as `x-amz-meta-av-label`/`x-amz-meta-av-provenance` (base64 of the prost encoding, so the round
trip is exact including caveats), beside a human-readable marking, media type and hash.
`get` authorizes the label **before a single byte is fetched** and verifies the payload after —
size first, then a constant-time compare of the recomputed SHA-256 — so a caller only ever sees
bytes that have cleared both checks.

The claim check is mechanical, as question 216(c) asks:
`crates/av-store/tests/claim_check_hot_path.rs` runs a real `cargo tree -e normal --offline`
for `av-ingest`, `av-track` and `av-command` and asserts `av-store` appears in none of them,
with a self-check that the same invocation does find `av-cdm` in `av-store`'s own tree, so the
assertion cannot pass on empty output. `-e normal` is deliberate: a dev-dependency edge is not
a hot-path dependency.

Against a real MinIO container (`crates/av-store/tests/minio_store.rs`, 5 tests): a round trip
by hash with the label and provenance returned field for field; a **server-side** corruption —
a second raw signed PUT of different bytes of the same length to the same key — refused as
`HashMismatch` naming both hashes; a label above the caller's clearance refused with the
container already stopped and removed, so a refusal that had reached the wire would have
surfaced as a connect error instead; an unknown key a typed `NotFound`; and the running
container's image id asserted equal to the recorded digest.

### H2: the catalog (`crates/av-catalog`, and the gateway selector)

No PostgreSQL client crate could be used either, and this is the sharpest constraint of the
round. `tokio-postgres`, `postgres`, `sqlx` and `deadpool-postgres` all reach SCRAM through
the RustCrypto crates `sha2`, `hmac` and `md-5` — all three banned by name. `pq-sys`/libpq was
considered and rejected on a different ground: it would make a C library a hard prerequisite
of `cargo build --workspace` on every host, turning a plain clone from "builds" into "fails to
build". So `crates/av-catalog` speaks the PostgreSQL v3 protocol itself.

That is not a theoretical need. Measured directly against the recorded image: its generated
`pg_hba.conf` ends `host all all all scram-sha-256`, so every client connecting from outside
the container — which is every client this workspace has — must perform SASL SCRAM-SHA-256.
`src/scram.rs` does it on `openssl::pkcs5::pbkdf2_hmac`, `openssl::sign::Signer`,
`openssl::sha::sha256`, `openssl::memcmp::eq` and `openssl::rand::rand_bytes`, and is pinned to
RFC 7677 section 3's own vector character for character, proof and server signature included.
`AuthenticationMD5Password` and `AuthenticationCleartextPassword` are recognised and
**typed-refused**: ADR-004 bans MD5 outright, and a server configured for either is a
deployment defect this client reports rather than works around. A server that declines
`SSLRequest` when TLS was required is refused, never silently downgraded.

`src/protocol.rs` is pure and I/O-free, which is what makes it exhaustively testable: one table
test decodes every message type from every truncation of its body, from zero bytes to its full
length, and asserts a typed error at each — no panics, no index-out-of-bounds. The client uses
the extended query protocol with text-format parameters and results, so no caller builds SQL by
concatenation and no binary numeric, timestamp or geometry decoding can be got wrong at the
boundary.

Migrations are `include_str!`'d and listed in one explicit ordered const array, never a runtime
directory walk, with a test that the array and the directory agree so an unregistered `.sql`
file fails instead of being skipped. Each is applied once, in one transaction with its own
bookkeeping insert, and its SHA-256 recorded; `apply_pending` and `verify` refuse with
`MigrationDrift` when a committed migration's hash no longer matches what was recorded, proved
against the real container by tampering with the recorded hash through SQL.

**The label filter is a `WHERE` clause, not a post-filter**, and the permitted marking set is
drawn from the ladder itself rather than computed as "everything not above me" — so an asset
whose stored marking is not on the ladder at all matches no caller's clearance, the top rung
included. Proved with exact sets, through the real gRPC rpc with real verified tokens:
UNCLASSIFIED saw `{asset-u}`, CUI saw `{asset-u, asset-c}`, SECRET saw
`{asset-u, asset-c, asset-s}`, and the off-ladder TOP-SECRET asset appeared at none of them.
The lower-labelled assets being *present* in the same call is what makes this a filter rather
than an empty result that could have been empty for any reason.

On the gateway, `GATEWAY_SELECTOR_CATALOG` goes through the authentication the gateway already
had: `authenticated_query` authenticates first, unconditionally, and only after the caller's
clearance has been overwritten with the token-derived marking does it route. `auth.rs` is
entirely unaware the selector exists — no second verifier, no relaxed check, no new surface in
`profiles/gateway-authority.yaml`. A gateway with no catalog configured refuses the selector
with a typed, counted refusal, never an empty success. `compute_query_id`'s preimage is
untouched and pinned; the pinned digest was reproduced independently at review with the
`openssl(1)` CLI.

### Gates, run by the manager with no worker active

| Gate | Result |
| --- | --- |
| `cargo test -p av-store -p av-catalog` | **160 passed, 0 failed, 0 ignored**, exit 0 |
| `cargo test --workspace --exclude av-kernel --no-fail-fast` | **1062 passed, 0 failed, 3 ignored**, exit 0 (baseline at the start of the round: 876 passed, 1 failed, 3 ignored, exit 101 — the one failure was the flaky `flock` test fixed in `24c1527`) |
| `cargo clippy --workspace --all-targets -- -D warnings` | exit 0, **zero warnings**; no `#[allow]` added anywhere this round (grepped) |
| `cargo deny check` | `advisories ok, bans ok, licenses ok, sources ok`, exit 0, with **exactly the six accepted spoore wildcard warnings** (`spoore-assoc`, `spoore-engine`, `spoore-math`, `spoore-ml`, `spoore-models`, `spoore-tree`) and nothing else |
| `.venv/bin/python -m pytest -q -rs` | **558 passed, 2 skipped**, exit 0 — identical to the baseline; both skips are the pre-existing cFS build-artifact ones |
| `buf lint proto` | exit 0, no output, with no new entry in `proto/buf.yaml`'s `ignore_only` |
| `buf breaking proto --against /Users/probe/code/AltaVista/proto` | exit 0, no output |

`docker ps -a --filter label=av.test` and `docker volume ls --filter label=av.test` were both
empty before the gate run and empty after it. Full outputs are in the manager's scratchpad.

**The +186 tests** (876 → 1062) are this round's, measured per binary in the
`cargo test -p av-store -p av-catalog` run above: `av-store` 52 library + 3 claim-check +
**5 against real MinIO** = 60; `av-catalog` 83 library + 8 fake-server wire + **9 against real
PostGIS** = 100; plus `av-gateway`'s new catalog-selector tests (**7**, two of them against
real PostGIS through the real gRPC rpc) and its new `query_id` pins, and the split `flock`
proof in `av-lockstep`.

### Decisions taken this round (numbered for the lead's log)

1. **The catalog image is `imresamu/postgis:17-3.5-alpine`, pinned by digest, on this host
   only.** The official `postgis/postgis` images publish **no linux/arm64 manifest** (measured:
   `docker manifest inspect postgis/postgis:17-3.5` lists `amd64` plus the buildkit `unknown`
   attestation entry and nothing else), and this host is arm64 Colima. Building our own on top
   of the official `postgres:17-alpine` was tried first and **cannot work**: the official
   `postgres` image compiles PostgreSQL into `/usr/local`, while Alpine's `postgis` apk package
   depends on Alpine's `postgresql18` and installs its extension into
   `/usr/share/postgresql18/extension` against the pg18 ABI — the two never meet. (Build log,
   verbatim: "Setting postgresql18 as the default version".) That attempt was reverted and the
   reason is recorded in `services/catalog/IMAGE_DIGEST.md` so nobody repeats it. Pinning by
   digest is what question 212(a) actually asks for regardless of namespace. **For H7: on an
   x86_64 production host the P5 kit should use the official `postgis/postgis:17-3.5-alpine`
   and record its own digest; the arm64 mirror is a development-host accommodation, not a
   deployment decision.**
2. **MinIO comes from `quay.io/minio/minio`, not Docker Hub.** `docker pull minio/minio:...`
   answers "pull access denied for minio/minio, repository does not exist or may require
   'docker login'" on this host; quay.io is MinIO's own registry and carries the arm64
   manifest. Question 39's AGPL ruling is unaffected: MinIO is an unmodified operational
   dependency reached over the S3 HTTP API, and `crates/av-store` links none of it.
3. **Both the S3 client and the PostgreSQL client are hand-rolled, because ADR-004's crypto
   rule leaves no off-the-shelf option.** Every Rust S3 client reaches TLS through
   `rustls`→`ring`; every Rust PostgreSQL client reaches SCRAM through `sha2`/`hmac`/`md-5`.
   All five names are in `deny.toml`'s ban list. Neither crate added a single new registry
   crate to `Cargo.lock`.
4. **`pq-sys`/libpq was rejected** as the alternative to hand-rolling the PostgreSQL protocol:
   it would make a C library a hard build-time prerequisite for `cargo build --workspace` on
   every host, turning a plain clone from "builds" into "fails to build". A test that skips is
   acceptable; a workspace that will not compile is not.
5. **MD5 and cleartext PostgreSQL authentication are recognised and typed-refused, never
   implemented.** ADR-004 bans MD5 outright; a server configured for either is a deployment
   defect this client reports rather than works around.
6. **The catalog is a selector on the existing `DataGatewayService.Query`
   (`GATEWAY_SELECTOR_CATALOG`), not a second service.** It is the same read-only, label-aware
   read surface with the same authentication and the same clearance rule, and H2's own
   milestone text asks for a selector. `CatalogQuery`/`CatalogRecord`/`GeoBbox` live in
   `heavy.proto` (this track's file, question 216(f)); `authority.proto` gains only the import,
   the enum value and two fields, and `buf breaking` is clean.
7. **`authenticated_query` became `async`; `GatewayCore::query` did not.** The selector must
   await a real PostgreSQL round trip. `GatewayCore::new`'s signature is unchanged — a
   `with_catalog` builder attaches the handle — so none of the eight existing call sites moved.
8. **The label filter lives in the SQL** (`marking = ANY($1::text[])`), not as a post-filter: a
   post-filter leaks the row count, the query cost and, through `LIMIT`, the existence of rows
   the caller may not see. The permitted set is drawn from the ladder itself, so an off-ladder
   stored marking matches no clearance at all.
9. **`assets.asset_id` is deliberately not the content hash.** The same bytes can legitimately
   be catalogued twice — a higher-marked ingest and a later re-release of the identical pixels
   are two distinct facts — and making `sha256` the primary key would make the second row
   impossible to represent.
10. **`av-lockstep` gained `ManagedContainer::run_local`, `local_image_id` and
    `recorded_digest_gate`, purely additively (149 insertions, 0 deletions).**
    `pull_and_run` runs `docker pull`, a network call at test time that question 154 forbids
    for this track's tests. `run_local` shares `build_run_args` and `published_host_port`
    rather than copying them, and `pull_and_run` and every existing caller are untouched. This
    is a file the P5 track also works in; the diff is deliberately minimal.
11. **The gateway's catalog path opens a fresh connection per query** rather than pooling.
    `av-gateway` is not a hot-path service and a pool is a round-2 concern; the trade-off is
    recorded at the call site rather than left implicit.
12. **H3 is deferred to round 2, not attempted badly.** H1 and H2 each cost a full worker for
    the implementation and a second for the evidence, and the host is shared with two other
    tracks building concurrently. Starting the job runner with the capacity left would have
    produced a queue with no durability proof, which is the one thing H3 exists to establish.

### Defects found in review, and their root causes

1. **`crates/av-store` had no typed `NotFound`** (H1a). Every 404 from `get`/`head` became an
   untyped `StoreError::S3`, indistinguishable by type from any other server error, although
   H1a's own brief asked for the variant. Root cause: the error enum was written from the
   refusal list and the HTTP status mapping was never revisited against it. Fixed in H1b and
   proved against a real 404 for both verbs. Worth noting for the next round: `head`'s 404 has
   no body at all, so the S3 `<Code>` cannot be extracted — mapping on the status first, not
   only on the code string, is what makes the typed refusal work there.
2. **`crates/av-store/src/metadata.rs` enforced S3's 2 KiB user-metadata budget per header, not
   across the sum** (H1a). Its own module doc argued this was "the conservative reading", which
   it is not: several headers each individually under budget can collectively exceed it, and S3
   shares the budget across the object. Root cause: a documented simplification that was never
   challenged. Fixed in H1b with the summed check, the measured total in the error, and a test
   whose fixture is individually-under / collectively-over.
3. **`av-lockstep`'s `flock_lock_is_visible_across_processes_and_languages` was flaky, and it
   failed in this round's own baseline gate.** Root cause, definitive: it asserted that **the
   host-wide lock is free** at a particular instant — a python3 child must be able to take
   `docker-tests.lock` right after this process dropped its guard. Two sibling tests in the
   same test binary legitimately hold that lock concurrently under libtest's default thread
   pool (`probe_process_that_blocks_acquiring_the_docker_test_lock` takes it directly;
   `lock_docker_tests_announces_a_blocked_wait_never_silently` spawns a nested `cargo test`
   child that holds it for as long as that child's build takes). Measured: the assertion failed
   on a cold target directory, where the binary took 131 s, and passed on a warm one, where it
   took 0.15 s. This is precisely the defect question 212(b) already ruled for the Python
   counterpart; the Rust side was missed then. Fixed in `24c1527` by splitting the proof: the
   production path keeps only the contention-proof direction (BLOCKED while we hold it, which
   proves the two languages agree on the path by construction), and release semantics — a
   property of `flock` itself — are proved on a lock file the test creates and owns. Verified
   with a second process grabbing and releasing the production lock in a tight loop for 45 s.
4. **An intermittent `docker logs: No such container` in the catalog container test, observed
   once by a worker and not reproduced in review** (one serial and two parallel runs all clean).
   The lock ordering was checked and is correct — each container is removed before its test
   releases the lock — so no two of this binary's containers can overlap. The remaining
   candidate is the Colima VM under this host's real memory pressure. **Root cause not
   established, and there is no retroactive path:** Docker's event stream is not persisted, so
   the window is gone. The path forward is to capture `docker events` alongside a future run.
   Mitigation in place: the readiness budget was raised to 60 s with the reason recorded at the
   call site.
5. **A structural observation, not a defect yet: the clearance-ladder convention now exists in
   four places** — `crates/av-edge/src/policy.rs` (the original), `crates/av-gateway/src/
   labels.rs`, `crates/av-store/src/labels.rs` and `crates/av-catalog/src/labels.rs`. Each copy
   documents that it is a copy and why it could not reach the others (`av-edge` is off this
   track; `av-catalog` must depend on neither `av-gateway`, which sits above it, nor
   `av-store`, a sibling tier). Four independently-maintained copies of a security-relevant
   comparison is a divergence waiting to happen. See the open items.

### Open items for the lead

1. **Four copies of the clearance ladder.** Recommendation: a small `av-label` crate holding
   one `ClearanceLadder` (rank = index into a configured ordered list; an off-ladder marking is
   its own refusal) plus the set-valued `markings_at_or_below` the SQL filter needs, adopted by
   `av-store`, `av-catalog` and `av-gateway` in one change, and by `av-edge` whenever the edge
   team next opens `policy.rs`. This track will not extract it unilaterally across a crate it
   is forbidden to edit.
2. **`services/catalog/IMAGE_DIGEST.md` records an arm64 accommodation.** H7 and the P5 suite
   fragment should name the official `postgis/postgis` image and its own x86_64 digest; the
   `imresamu` mirror is a development-host choice, pinned by digest, not a deployment one.
3. **The MCP `query` tool's JSON schema does not expose `catalog_query` this round.** The gRPC
   surface does. Scoped out deliberately and recorded in code, not silently dropped — a round-2
   item if a model should be able to query the catalog through MCP.
4. **`docs/architecture.md`'s owned port map was not edited.** The gateway's new
   `--catalog-host/-port/...` flags are an outbound dial to an existing Postgres tier, the same
   class as `AV_GATEWAY_COMMAND_AUTHORITY_ENDPOINT`, not a new listening bind — so by the
   table's own rule no row is due. Recorded rather than decided unilaterally, since that file
   is shared.
5. **This host is genuinely over-subscribed.** Three tracks were building concurrently
   throughout the round; swap sat at ~6.9 GB of 8 GB for its whole duration, and several test
   binaries were observed frozen at `_dyld_start` — never reaching `main()` — under the page-in
   stall that produces. Two workers lost significant time to it, and one gate run had to be
   abandoned mid-way. This is the same contention question 212 already flags for the E5 latency
   retake; it is now costing build throughput too, not only measurement fidelity.
6. **`crates/av-lockstep` was touched by this track** (additively, `docker.rs` +149/−0, plus the
   `docker_test_lock.rs` test split). The P5 track works in the same file set from its own
   worktree; the lead should expect to resolve that at merge.
7. **H3 is the first item of round 2**, with H4–H7 behind it. The claim-check, the store and
   the catalog are the foundation it needs, and both are now real.

## Status (heavy manager, 2026-09-16) — round 2

**Questions 219(a)/(b) and 218 are executed, H3 is delivered and H4 is delivered.**
H3 went further than the milestone asked — a tile set now round-trips through a real MinIO —
and that extra proof is what exposed the round's sharpest defect. H5–H7 are untouched.

### What landed, one commit per accepted task

| Commit | What |
| --- | --- |
| `928d22e` | Question 219(a)/(b): `av-proposer`'s `DEFAULT_MODEL_SERVICE_BIND` in the owned port map; `SPOORE_ROOT` with the `../spoore` sibling default in `crates/av-proposer/build.rs` |
| `b77e2f3` | Question 218: `crates/av-label` extracted, adopted by `av-store`, `av-catalog` and `av-gateway` in one change |
| `8d4779f` | H3a: `crates/av-jobs` — the durable hash-chained queue, the job record types in `heavy.proto`, and the runner |
| `5ee646a` | H3b: the tiler — imagery to the globe's tile layout under a hashed manifest, with a dependency-free PNG encoder |
| `d10ed8f` | H3's exit proof: a tile set through a real MinIO container, and the defect it exposed |
| `2d9365c` | The manifest's backend-dependent addressing fixed (manager's review finding) |
| `185aebc` | H4: `crates/av-tiles`, the tile gateway; `GroupClearanceMap` moved into `av-label` |

### Question 219: the port map and `SPOORE_ROOT`

`av-proposer`'s only listening socket is the `spoore.v0.ModelService` server it serves under
`--serve-model-service` (D2's numeric sidecar). `altavista.v1.ModelProposeService` — which the
lead's brief named — is a **client** in that crate, and the port map is a map of binds, so the
default belongs to the server. `127.0.0.1:50063` groups it with `gmat-service` (`50061`) and
`av-dynamics-service` (`50062`), the other model/dynamics sidecars, and deliberately leaves the
`5007x` authority plane clear for `av-ingest`'s own bind (question 219(a)'s other half, the P5
team's, in the same round). **The P5 team must avoid `50063`.**

`--serve-model-service`'s value became optional through a `Peekable` parser: the next token is
the flag's value only if it is present and does not begin with `--`. Every existing call site
passes an explicit address and is unchanged. The load-bearing test is the one where
`--serve-model-service` is immediately followed by `--model-node-id`: the default is taken AND
that flag still parses, which a plain `args.next()` would have silently corrupted.

`crates/av-proposer/spoore_root.rs` is a separate source unit precisely so `build.rs` can
`include!` it and `tests/spoore_root.rs` can `#[path]`-include the identical text under test. It
never canonicalizes, so a symlinked root works, and the test builds its whole proof on a temp
symlink layout with no absolute host path anywhere in it. The load-bearing build proof is the
negative one: `SPOORE_ROOT=/definitely/not/a/real/path` fails the build with a typed panic
naming that path, **even though `/Users/probe/code/spoore` still exists on this host** — which
is what proves the absolute path is no longer consulted at all.

### Question 218: the shared `av-label` crate

One `ClearanceLadder` with `rank`, `markings`, `classify` and the set-valued
`markings_at_or_below`, one `LabelRefusal`, one `Side { Caller, Subject }`. Dependencies:
`av-cdm` and `thiserror`, and that is deliberate — a store tier, a catalog tier and a gateway
all rank markings through this crate, so its dependency set is a security surface.

`Side::Subject` is the neutral name; each adopting crate keeps its own outward-facing spelling
by mapping at its own boundary. `av-store` re-exports the type and adds an `AuthorizeRead`
extension trait so `ladder.authorize_read(...)` in `client.rs` is character for character the
call it always was, and every `StoreError` variant and message is untouched. `av-catalog`'s
`find_assets` line is likewise unchanged, because `impl From<LabelRefusal> for CatalogError` is
what `?` now uses; that impl maps only the `Side::Caller` arm onto the existing
`CallerMarkingNotOnLadder` and sends the two arms `markings_at_or_below` cannot produce to a new,
additive variant rather than mis-diagnosing them. **The gateway's three counter key strings are
byte-identical** — `label_caller_marking_not_on_ladder`,
`label_product_marking_not_on_ladder` (the `Side::Subject` arm deliberately keeps `product`,
because the key is already observable) and `label_over_clearance`.

The gateway also stopped holding two ladders: `CatalogHandle::ladder` was a structurally
distinct `av_catalog::labels::ClearanceLadder` built a second time from the same configured
string, and both are now the one `av_label::ClearanceLadder` the binary builds once.

`crates/av-edge/src/policy.rs` is **untouched and nothing depends on it** — question 218 gives
that fourth copy to the P5 team when it next opens that file.

A defect surfaced during the extraction and is worth the next team's attention:
`impl Counted for LabelRefusal` could move nowhere. `Counted` had already moved to
`av_command::counters` in R3.1, so with `LabelRefusal` foreign too, **neither** the trait nor the
Self type is local to `av-gateway` and the impl is E0117. The fix is a free
`crate::labels::code(&LabelRefusal)` with the identical mapping, called from `RefusalReason`'s
own `Counted` impl — `RefusalReason` is local, and production only ever records a
`RefusalReason`, never a bare `LabelRefusal`. The counter test was rewritten to go through
`RefusalReason::Label(..)`, which is the real production path, so it is a **stronger** assertion
than the one it replaces. H4's `GroupClearanceMap` move hit the same rule and reused the same
precedent rather than inventing a second approach.

### H3: the job runner and the tiler (`crates/av-jobs`)

The queue copies `crates/av-ingest/src/log.rs`'s framing byte for byte — `payload_len u32 LE`,
`record_hash [u8;32]`, payload — chained `SHA-256(prev || payload)` from the literal `GENESIS`.
The primitive is reimplemented over `openssl::sha::sha256` rather than taken from `av-edge`,
which is off this track; `src/hash.rs`'s doc says so and says why the hash-chain convention, unlike
the clearance ladder, has no shared home to extract into.

Recovery follows the same rule as the ingest log: only a torn tail or a corrupt **trailing**
record is discarded, always with a `RecoveryReport` naming the byte count and the reason; a
corrupt record in the middle is left byte for byte alone and reported by `verify`, because
truncating it would destroy the tamper evidence a reviewer needs.

The runner never loses a failed job. Every refusal — unsupported kind, a label off the ladder, a
missing input, an input whose recomputed SHA-256 does not match its `AssetRef` (compared with
`openssl::memcmp::eq`, and an executor is never handed unverified bytes), a non-zero exit with
its real code, a failed spawn, a rejected output — becomes a `JobCompletion { ok: false }`
appended to the log. **Manager's review finding:** `run_one` originally *panicked* if that
append failed. It now returns `Result<JobCompletion, JobError>` where the `Ok` always carries the
completion and the `Err` means only that the completion could not be made durable — an
infrastructure failure of the queue, categorically different from a job that ran and failed, and
not something to abort a long-running runner process over.

The tiler matches the globe's own scheme from `web/js/globe_lod.js` — geographic plate-carrée,
level 0 two tiles side by side, canonical order `(level, x, y)` — pinned against it by a test
whose expected bounds are literals with the arithmetic in a comment. The worker's first
`tiles_covering` iterated y-outer while `compareTiles` is x-primary; its own pin caught it. That
is exactly the drift the pin exists for.

`src/png.rs` is a PNG encoder with **no dependency**: the IDAT is a real zlib stream built from
*stored* (uncompressed) deflate blocks, which every decoder accepts and which needs no
compressor. **CRC-32 and Adler-32 are ordinary error-detecting checksums, not cryptography** —
ADR-004 governs cryptographic hashing and TLS, and the only cryptographic hash in this crate is
still `openssl::sha::sha256`. The module doc says so explicitly so no reviewer has to guess. Its
golden was computed independently with Python's `zlib`, and a second test drives
`.venv/bin/python` (standard library only, no PIL) to decode the encoder's real output.

Resampling is nearest-neighbour with an explicit formula, chosen because it is exactly
reproducible with no filter-kernel ambiguity — which is what "deterministic for the same input
hash" actually requires. A better resampler is recorded as a later decision rather than silently
picked.

**The pinned manifest hash for the fixture is
`7c23f4f0b6a81270c196df8acf8d6c17c86d69185b31a35357c444f3bcdaa430`** (1852 bytes). It is an
anchored golden, not a self-referential one: the manager re-derived it with tools that are not
this crate — Python's `hashlib` over the manifest object's bytes, and the **Python protobuf
runtime**, a different implementation from `prost`, decoding those same bytes to exactly
kind=IMAGERY, scheme `geographic-plate-carree-2x1`, levels 0..1, tile_size 16, whole-globe
bounds, 10 tiles in `(level,x,y)` ascending order, the three parameters, job_id `job-pin`, the
fixture raster's own sha256 as the single source, `object_key_prefix` `tiles`, every `uri` empty,
and every `object_key` equal to `<prefix>/<hh>/<hh>/<sha256>`. Deterministic re-serialisation
returns the identical bytes, so the encoding is canonical rather than merely self-consistent.

### The round's sharpest defect, and why the obvious fix was the wrong one

`d10ed8f`'s real-MinIO proof exposed it: the tiler built every `TileEntry.uri` as
`format!("memory://{key}")` unconditionally, because it runs before the `ObjectSink` and has no
way to learn that sink's URI scheme. Against the real store every tile in the manifest claimed
`memory://tiles/...` while the object lived at `s3://<bucket>/tiles/...`. A consumer could not
have used the manifest to find a tile in any store-backed deployment — and H4's tile gateway is
exactly such a consumer.

Filling `uri` in *correctly* would have been worse. A fully-qualified URI names a bucket and an
endpoint, and since the tile set's identity is the SHA-256 of the manifest's own bytes, that
identity would then change with the store that happened to hold the tiles: one tile set copied
between buckets would acquire two identities. A tile set that is not the same thing in two
buckets is not content-addressed at all.

So `TileEntry.uri` is emptied and reserved (kept on the wire — `heavy.proto` is additive-only now
that a round has shipped), and two additive fields carry the genuinely backend-independent part:
`TileEntry.object_key` and `TileSetManifest.object_key_prefix`. The prefix is inside the hashed
bytes on purpose: two tile sets of identical tiles under different prefixes are different tile
sets to a reader. The store test now asserts, for every tile, that `uri` is empty, that
`object_key` equals what `av_store::object_key` independently derives, and that the sink's own
`s3://` location is exactly `s3://<bucket>/<object_key>` — so a reader holding only the manifest
can reconstruct where each tile lives. The manifest hash matching across the in-memory and
real-MinIO backends is now a property of the design; **before the fix it matched only because the
`memory://` URI was uniformly wrong rather than uniformly absent.**

### H4: the tile gateway (`crates/av-tiles`)

Serves `GET /v1/tilesets/{manifest_sha256}/manifest` and
`GET /v1/tilesets/{manifest_sha256}/tiles/{level}/{x}/{y}` over the plain
`tokio::net::TcpListener` pattern `crates/av-command/src/admin.rs` already established — no HTTP
framework, no hyper server, and no new dependency of any kind.

The request path is a fixed order of typed, counted refusals: route parsing before any auth
work; `av_command::oidc::verify` with an injected clock — the shared verifier, never a second
one; the caller's clearance derived from the verified token's groups through `av-label`'s
`GroupClearanceMap`, **never from a caller-supplied header or query parameter**; the manifest
fetched by content hash and its SHA-256 recomputed and compared to the requested hash *before* it
is decoded; the label check; the `TileEntry` lookup; the tile fetched by `object_key` and
hash-verified before a single byte is returned. Sixteen distinct counter keys, asserted as a set.
Range requests (`206`/`416`, a malformed `Range` ignored per RFC 9110), `ETag` = the tile's own
SHA-256, `Cache-Control: immutable` because content-addressed bytes never change, and `304` on a
matching `If-None-Match`.

`av-tiles` takes `127.0.0.1:50073` in the owned port map, cited from `DEFAULT_BIND`'s own doc
comment, with the table row and `tests/test_port_map.py` extended.

**A judgement call recorded rather than quietly implemented.** H4 asks for label enforcement "per
layer and per request". `crates/av-tiles/src/refusal.rs`'s module doc works it through and
concludes a second check would do nothing: every tile in one manifest carries the same label as
the manifest (`Runner::execute_spec` puts every output of a job under one label), and both the
caller's clearance and that label are fixed for the lifetime of a request, so a second `classify`
with identical inputs can only reproduce the first answer. One evaluation is both enforcements
for the request it belongs to. The doc names the condition under which a second check becomes
real: a data model that gives tiles labels of their own.

### Gates, run by the manager with no worker active

| Gate | Result |
| --- | --- |
| `cargo test -p av-label -p av-store -p av-catalog -p av-jobs -p av-tiles` | **314 passed, 0 failed, 0 ignored**, exit 0, 18 binaries |
| `cargo test --workspace --exclude av-kernel --no-fail-fast` | **1225 passed, 0 failed, 3 ignored**, exit 0 (round 1's accepted gate at `b3a9956`: 1062/0/3) |
| `cargo clippy --workspace --all-targets -- -D warnings` | exit 0, **zero warnings**; no `#[allow]` added anywhere this round (grepped) |
| `cargo deny check` | `advisories ok, bans ok, licenses ok, sources ok`, exit 0, with **exactly the six accepted spoore wildcard warnings** |
| `.venv/bin/python -m pytest -q -rs` | 553 passed, **1 failed**, 6 skipped — the failure re-run alone passes; see below |
| `buf lint proto` | exit 0, no output |
| `buf breaking proto --against /Users/probe/code/AltaVista/proto` | exit 0, no output |

The per-crate breakdown of the 314: `av-catalog` 83 + 8 wire + **9 against real PostGIS** = 100
and `av-store` 52 + 3 claim-check + **5 against real MinIO** = 60, both identical to round 1;
`av-label` 19; `av-jobs` 58 + 11 + 8 + **2 against real MinIO** = 79; `av-tiles` 50 + 6 = 56.
**The +163** on the workspace gate (1062 → 1225) is exactly 9 (`av-proposer`) + 19 (`av-label`) +
79 (`av-jobs`) + 56 (`av-tiles`); `av-gateway`'s own count is unchanged by the two extractions.

`docker events` was captured around every docker-gated run this round. No `oom`, no unexpected
`die`/`destroy`, and `docker ps -a --filter label=av.test` and `docker volume ls --filter
label=av.test` were both empty before and after every gate. **Question 218's unreproduced
`docker logs: No such container` did not recur**, and there are now evidence files for the
windows if it ever does.

The digest gate was proved to actually fire, not merely to exist: recording a deliberately wrong
digest in `services/store/IMAGE_DIGEST.md` makes `crates/av-jobs/tests/store_tiler.rs` print a
named `SKIPPED` line on real stderr **without `--nocapture`**, and `git diff` on that file is
empty after restoring it.

### The one pytest failure, root-caused

`tests/test_edge_ingest_mtls.py::test_valid_seccert_leaf_is_accepted_through_nginx_and_batches_submit`
failed with `BATCH_REJECTION_STALE`: "batch ... is older than `max_age_ns=5000000000` relative to
`now_tai_ns=...`". **Re-run alone it passes, in 53.9 s** (question 207's rule: a contended
failure is re-run alone before it is believed).

**Root cause, definitive, and it is a latent defect in that test rather than a flake to shrug
at.** The test takes `batch_tai_ns = now_tai_ns()` at the top of its body and then does an
unbounded amount of work before the batch is actually submitted — building a server certificate,
provisioning a full seccert chain, starting nginx and the ingest binary — against a server run
with `--real-clock` and a fixed five-second freshness window. Even *alone and unloaded* that
setup takes 53.9 s of wall clock; the five-second budget survives only because the batch is
signed late in the sequence. With this round's workspace gate and the lead's concurrent P5 gate
both building, the gap between the timestamp and its use crossed the window and the ingest
**correctly** rejected the batch. Nothing this round changed touches `av-edge`, `av-ingest`, the
mTLS path, nginx or the staleness rule.

The fix belongs to whoever owns that file: take `batch_tai_ns` at the moment the batch is built,
immediately before the submit, rather than at the top of the test body — or raise this test's own
`--max-batch-age-ns` with the reason recorded at the call site. This track did not edit another
track's test at the end of its round; it is an open item below.

The six skips (against round 1's two) are all "image not present on this host" skips —
`av-edge-plugin:local`, `alpine:latest`, `av-proposer:local` and the cFS build artifacts. Each
prints a named reason, and `alpine:latest`'s own skip text already explains the cause: Colima's
kubelet image garbage collector evicts every image no container uses once the VM disk passes its
high threshold (questions 196(d)/205). Host state, not this round.

### Decisions taken this round (numbered for the lead's log)

1. **`av-proposer`'s default bind is `127.0.0.1:50063`, and it belongs to `ModelService`, not
   `ModelProposeService`.** The brief named the latter, but that is a *client* in this crate and
   the port map is a map of binds; the only listening socket `av-proposer` opens is the
   `spoore.v0.ModelService` server under `--serve-model-service`. `5006x` is the model/dynamics
   sidecar group; `5007x` is left clear for `av-ingest`. **The P5 team must avoid `50063`.**
2. **`--serve-model-service`'s value became optional via a `Peekable` parser**, rather than
   splitting the flag in two, so every existing call site is unchanged byte for byte.
3. **`services/proposer/build-image.sh`'s `SPOORE_HOST_PATH` was deliberately NOT changed.** It
   bind-mounts spoore at its own absolute path *because* the workspace manifest's `spoore-cdm`
   path dependency is absolute, so it cannot honour a different `SPOORE_ROOT` until question
   219(c) lands, and proving any change to it needs a full image rebuild in question 154's one
   permitted network window. Recorded, not silently skipped.
4. **`av-label` depends on `av-cdm` and `thiserror`, and nothing else, ever.** Three tiers rank
   markings through it; its dependency set is a security surface.
5. **`Side::Subject` is the shared neutral name; each consumer maps at its own boundary.** The
   gateway's `label_product_marking_not_on_ladder` counter key is deliberately unchanged, because
   it is already observable and a dashboard built on it must survive an internal refactor.
6. **`Counted` for a foreign refusal is a free function, not a trait impl.** `Counted` lives in
   `av-command` and the refusal types now live in `av-label`, so an impl in `av-gateway` is
   E0117. Used twice this round (`LabelRefusal`, `GroupClearanceOutcome`) with one precedent, not
   two approaches.
7. **`av-jobs` depends on neither `av-store`, `av-edge` nor `av-command`.** `ObjectSource`/
   `ObjectSink` are the seam; the store-backed implementation lives in a **dev-dependency** test
   so the production dependency set stays clean and the crate stays out of the hot-path
   claim-check assertion's blast radius.
8. **`Runner::run_one` returns a `Result`, and a failed *job* is never the `Err`.** The `Err` is
   only "the completion could not be made durable". The first implementation panicked there.
9. **The CONTAINER executor is deferred with a typed, recorded refusal**, not a silent skip: a
   test registers a real working `ProcessExecutor` under the job's own kind and shows it is never
   invoked.
10. **The tiler's PNG encoder is hand-written with stored deflate blocks** because no image crate
    is in `Cargo.lock` and none was added. CRC-32 and Adler-32 are checksums, not cryptography;
    ADR-004 is untouched.
11. **Nearest-neighbour resampling**, because it is exactly reproducible with no filter-kernel
    ambiguity, which is what determinism actually requires. A better resampler is a later,
    recorded decision.
12. **A tile set's manifest carries no URI.** `TileEntry.uri` is empty and reserved;
    `object_key` + `object_key_prefix` carry the backend-independent addressing. A manifest whose
    hash changes with the bucket is not an identity. (The review finding above.)
13. **`av-tiles` performs one label evaluation, not two**, and says so rather than adding a check
    that does nothing — with the condition named under which a second one becomes real.
14. **`GroupClearanceMap` moved into `av-label` rather than being copied into `av-tiles`**, for
    exactly the reason question 218 gives.
15. **`av-tiles` takes `127.0.0.1:50073`**, no admin surface, hence no `+100` counterpart.

### Defects found in review, and their root causes

1. **`Runner::run_one` panicked on a failed log append** (H3a). Root cause: the brief's "never
   returns an `Err`" was read as covering the append too, and the tension was resolved with a
   panic. Fixed; the `Ok`/`Err` split now distinguishes a failed job from a failed queue.
2. **`TileEntry.uri` was a hardcoded `memory://`** (H3b), wrong against every real backend. Root
   cause: the manifest-vs-sink ordering constraint — the executor must name a location before the
   sink has assigned one — was solved for the *key* but not for the *scheme*, and the only sink
   in existence at the time made the wrong answer invisible. Found only because H3's exit proof
   ran against a real store. Fixed in `2d9365c`; the manifest hash was re-pinned and
   independently re-verified.
3. **A weak assertion in the `SPOORE_ROOT` test** (task 1): `err.contains("spoore")` is satisfied
   by the message's own literal text and so could never distinguish a correctly-resolved default
   path from a wrong one. Root cause: an assertion written against the error's prose rather than
   against the value under test. Strengthened to name the exact resolved path, so a resolver that
   walked the wrong number of directories up now fails instead of passing by coincidence.
4. **The manager's own baseline run was invalidated** by starting `cargo test --workspace`
   concurrently with the first worker's edits; it picked up a half-written file and failed to
   compile. Root cause: a process error of the manager's, not of the tree. Recorded rather than
   quietly re-run: round 1's accepted gate at `b3a9956` (1062/0/3) is the baseline this status
   compares against, and no manager cargo run overlapped a worker after that.
5. **A worker clobbered one of its own new files** with two `mv` calls sharing a destination
   basename while using `git stash` to isolate a baseline. It caught and rewrote both files, and
   both were verified at review. Root cause: `git stash` used for baseline isolation in a
   worktree with concurrent activity. Every later brief forbade `git stash` outright and told
   workers to measure baselines before editing.
6. **`cargo test` passing does not imply clippy is clean** — observed directly when a doc-comment
   edit introduced eight `doc_lazy_continuation` warnings that the full test suite happily
   ignored. Every later brief states it explicitly.

### Open items for the lead

1. **`tests/test_edge_ingest_mtls.py`'s timestamp is taken too early.** Root-caused above. The
   fix belongs to whoever owns that file: take `batch_tai_ns` immediately before the submit, or
   raise that test's own `--max-batch-age-ns` with the reason recorded. This track did not edit
   another track's test at the end of its round.
2. **`av-proposer` took `50063`; the P5 team must avoid it** when it assigns `av-ingest` its own
   bind under question 219(a).
3. **`services/proposer/build-image.sh` still hardcodes `/Users/probe/code/spoore`**, and cannot
   honour `SPOORE_ROOT` until question 219(c) makes the workspace manifest's `spoore-cdm` path
   relative. Sequence it after 219(c) and prove it with one image rebuild.
4. **`crates/av-edge/src/policy.rs` is still the fourth ladder copy.** `av-label` is ready for it;
   question 218 gives the adoption to the P5 team.
5. **Not done in H3b, at a clean boundary rather than half-built:** the terrain tiler and the 3D
   Tiles point-cloud tiler. `output=="terrain"` and `output=="tiles3d"` are recognised and refused
   as not implemented, with a test pinning both refusals, so neither is a silent gap. H5's
   ten-gigabyte proof needs at least one of them.
6. **Not done in H4:** a docker-gated proof of `av-tiles` serving out of a real MinIO. The
   store-backed `ObjectSource` exists and is what the binary runs on; only the container-backed
   test is missing, and `crates/av-jobs/tests/store_tiler.rs` already proves that seam end to end.
7. **The container executor (`JOB_EXECUTOR_KIND_CONTAINER`) is unimplemented**, refused with a
   typed, logged `EXECUTOR_UNAVAILABLE`. H3's milestone text says "a labelled container **or** a
   process"; the process executor satisfies it, and the container path is recorded as future work
   rather than claimed.
8. **A control-matrix row is still owed** for `av-store`'s and `av-catalog`'s hand-rolled
   protocol clients — question 218 accepted them on that condition, and `docs/compliance/` today
   carries matrices for services only, not for these library crates. An H7 item.
9. **The host is still over-subscribed**, and it now costs correctness signal, not just time: the
   one pytest failure this round is a real-clock freshness window losing a race to concurrent
   builds. `CARGO_BUILD_JOBS=4` held throughout, and the lead's P5 round-4 gate was running in
   `AltaVista-verify` for much of this round's own gate.
10. **Six image-absent pytest skips, against round 1's two.** Colima's kubelet image garbage
    collector has evicted `alpine:latest`, `av-edge-plugin:local` and `av-proposer:local`. Each
    skip is visible and named, but a suite that silently loses coverage to a disk-pressure
    collector between rounds is worth a standing decision.

## Status (heavy manager, 2026-09-17) — round 3 — **PAUSED**

**The round was paused by the user part-way through its verification phase.** H5 is
delivered including the proof at scale, H3's two deferred halves (terrain and 3D Tiles
tiling, the container executor) are delivered, and the `av-tiles` image and its
docker-gated test are built and committed but **have not been observed green on this
host** — three attempts were defeated by host state, recorded below rather than implied.
H6 was not started. The end-of-round gates were not run: the last two commits are
therefore unverified against the full workspace and the full Python suite.

### What landed, one commit per accepted task

| Commit | What |
| --- | --- |
| `dfb5ce1` | H5a: `web/js/layers/`, the streaming-layer module — one interface over the globe's imagery and terrain loaders and the vendored 3DTilesRendererJS, a priority order by screen-space error and view distance, a memory budget declared in bytes, cancellation on a view move |
| `fd9fc15` | H5b-1: `av-tile-fixture`, the committed tile-set generator; the viewer server's same-origin `/api/tiles/*` proxy with the gateway's authentication; `av-tiles`' admin counters surface |
| `8d2146c` | H5b-2: frame time measured while a tile set streams from a real gateway; the priority queue made load-bearing; a starvation defect found and fixed |
| `fd9778d` | The tiler streams to the sink (bounded memory), and `scripts/heavy/ten_gigabyte_proof.py`, the re-runnable measurement |
| `3f86b52` | The terrain and 3D Tiles tilers, which round 2 left as typed refusals |
| `41f0815` | The six Rust SBOMs regenerated for this round's workspace-manifest epoch move (question 220) |
| `892e511` | The evidence bundle's hash re-recorded after that epoch move |
| `9213fc8` | The streaming harness measures a real tile set, not a fixture-shaped one (three defects found while taking the scale measurement) |
| `435e266` | The container executor, H3's last deferred half |
| `dcb6265` | The owned port map: `av-tiles`' admin row, and the stale question 219 prose |
| `d43d9b7` | The `av-tiles` gateway as a digest-recorded image, and its docker-gated proof (**test not yet observed green — see below**) |

### The proof at scale

Taken by the manager with no worker active, on this host, and **it was not a quiet
window**: an unrelated Supabase stack and a k3s control plane run in the same Docker
daemon, and the other track was building intermittently. Host: Mac14,6, 12 cores, 64 GB
RAM, swap 6.85 GB of 8 GB in use throughout.

| | |
| --- | --- |
| Tile-set manifest SHA-256 | `4d01ddd62d61e1891637f30cbb3d80b2e0ccc963af0b0ea2635225d26391fe27` |
| Tiles | 10 922 (levels 0..6, tile size 1024, whole-globe plate-carrée) |
| Stored bytes | **34 374 176 645** (34.4 GB), in a real MinIO by content hash |
| Source raster SHA-256 | `c6c17eea3528f07368a380815ddb913c282f2dc6d3fe568637f294b060e88ffb` |
| Generation wall clock | 393.2 s (≈ 87 MB/s) |
| Generator peak RSS | **67 747 840 bytes** (64.6 MiB) — 34.4 GB of tiles through 65 MB of memory |
| Verification | manifest re-fetched with an independent signed S3 GET, the stored total independently recomputed from it (34 374 176 645 = 34 374 176 645), host bind-mount usage cross-checked, tile 0/0/0 re-fetched and hash-verified |

**Ten gigabytes exactly is not reachable on this scheme, and that is arithmetic, not a
choice.** The PNG encoder writes RGB8, so a 1024 tile is 3 147 060 bytes; a level costs
four times the one below it and halving the tile size quarters a tile, so every
whole-pyramid total is 8.59 GB times a power of four. 8.59 GB is the largest shape below
ten gigabytes and 34.4 GB the smallest full pyramid above it. A full pyramid is what the
viewer needs — it refines from level 0 — so 34.4 GB is what was generated.

Streamed through the gateway into the viewer, three runs, memory budget 200 000 000 bytes,
frame budget 16.7 ms (one frame at 60 Hz):

| run | max frame ms | max resident bytes | tiles | bytes streamed | ETag-verified | cancelled | evicted |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | 3.82 | 179 382 420 | 57 | 179 382 420 | 57 | 12 | 0 |
| 2 | 5.84 | 198 264 780 | 70 | 220 294 200 | 70 | 11 | 7 |
| 3 | 3.48 | 198 264 780 | 70 | 220 294 200 | 70 | 12 | 7 |

`everyFrameWithinBudget` true in all three; `budgetRespected` true; the soft-violation
branch never taken; zero ETag mismatches; and the declared per-tile byte cost matched the
real received length **exactly** on every tile (`byteCostMismatchCount` 0,
`maxByteCostErrorBytes` 0), which is what makes the memory number a statement about real
bytes rather than an estimate. Run 1's `evictedCount` of 0 is host-load variance and is
reported rather than hidden.

The 34.4 GB tile set was removed after the measurement (`--teardown`); `out/` and the
host's labelled containers and volumes are clean.

### The command that stands the stack up for the lead's browser drive

Full, copy-pasteable, in `scripts/heavy/README.md`, verified end to end at a small shape
with its real response headers pasted in. In outline, from the worktree root:

```
.venv/bin/python scripts/heavy/ten_gigabyte_proof.py --synthetic-source 2048x1024 \
    --out-dir out/heavy-10g            # prints manifest_sha256 and leaves MinIO up
# mint an RS256 token with the system openssl CLI (the exact four steps are in the README)
target/release/av-tiles --oidc-issuer https://sso.test.example/ --oidc-audience av-tiles \
    --oidc-public-key-path "$W/issuer_public.pem" --ladder UNCLASSIFIED,CUI,SECRET \
    --key-prefix heavy-ten-gb-proof --store-endpoint http://127.0.0.1:<MINIO_PORT> \
    --store-region us-east-1 --store-access-key-id <USER> --store-secret-access-key <PASS> \
    --store-bucket av-heavy-ten-gigabyte-proof --store-path-style \
    --group-clearance tile-readers=CUI --bind 127.0.0.1:18080 --admin-bind 127.0.0.1:18081 &
.venv/bin/python -m altavista serve --host 127.0.0.1 --port 18090 \
    --tiles-endpoint 127.0.0.1:18080 --tiles-token-path "$W/token.txt" &
# then open http://127.0.0.1:18090/ ; teardown:
.venv/bin/python scripts/heavy/ten_gigabyte_proof.py --teardown --out-dir out/heavy-10g
```

Measured through that stack against the 34.4 GB set: `GET /api/tiles/<manifest>/tiles/0/0/0`
answered `200`, `content-length: 3147060`, `content-type: image/png`,
`cache-control: public, max-age=31536000, immutable`, `etag:
"3a1c61fd481ab77180c15b40149058f60ccc2feb4601bd615ce185247689caba"` — and `shasum -a 256`
on the body reproduces that ETag exactly.

### Defects found in review, and their root causes

1. **The priority queue decided nothing** (H5a). `update()` computed a priority order and
   then started a load for every wanted request, with no in-flight cap — a sorted list, not
   a queue. Root cause: "queue" read as "ordered list". Fixed with a declared
   `maxConcurrentLoads`, proved by showing both that the started set is the top-N and that a
   named lower-priority request starts later once a slot frees.
2. **A permanently-failing layer starved the queue** (H5b-2). `LayerManager` had no memory
   of a failed load, so the next `update()` re-planned it, took a slot, failed, and repeated
   forever. Root cause: `_onFailed` dropped the pending entry and recorded nothing. Measured
   on the unfixed code with two tied-priority layers of ten requests each at a cap of six,
   over thirty frames: the failing layer was attempted 180 times and the well-behaved layer
   reached **0** of its 10 tiles. With a documented failure memory: 10 attempts, 10 of 10.
   The first response had been to raise the harness's own cap until the symptom disappeared;
   that was reverted.
3. **The frame budget could not fail.** 250 ms was asserted against a measured 1.8 ms.
   Tightened to 16.7 ms, one frame at 60 Hz, and earned over five consecutive real-stack
   runs (1.7563, 1.7548, 1.9488, 1.7863, 1.8330).
4. **The harness's documented dwell was never the one it used** (found while taking the
   scale measurement). Each camera position was bounded by a 2000-tick ceiling alongside a
   wall-clock dwell, with a comment claiming the ceiling could never be the binding bound. A
   bare `setImmediate` costs about 0.01 ms here, so 2000 ticks is ~20 ms — shorter than one
   real 40 ms tile round trip. Every position ended on the ceiling and a run against a large
   tile set streamed three to five tiles whatever dwell it was given.
5. **The per-tile byte cost was a fixed 262 144-byte estimate** whatever the tile set, so
   the memory budget was accounted in the wrong units for any tile set that is not 256×256.
   Now declared by the caller and checked against every tile actually received.
6. **`tests/test_port_map.py` asserted `"admin": None` for `av-tiles` as a hardcoded
   literal**, so the table and the test agreed with each other while both disagreed with
   `crates/av-tiles/src/admin.rs`. A stale agreement is what that test exists to prevent.
7. **The workspace clippy gate is blind to `av-tile-fixture`** (`required-features`), so the
   round's gate gains `cargo clippy -p av-jobs --all-targets --features store-fixture`.
8. **A pre-existing dead-code warning in `crates/av-command/src/audit.rs`**
   (`from_line_sink` is never used outside tests) is invisible to `cargo clippy
   --all-targets`, which compiles the tests, and appears only in a plain release build. Not
   introduced this round and not this track's file; recorded for its owner.

### Decisions taken this round (numbered for the lead's log)

1. **`LayerManager` supersedes `TileLoadScheduler` for anything routed through
   `web/js/layers/`**, rather than reusing it: a tile *count* stops being a byte-budget proxy
   once imagery, terrain and 3D Tiles share one budget. `globe_lod.js` is untouched and both
   of its existing headless checks still pass byte for byte.
2. **The viewer server holds the gateway credential; the browser never supplies one.** The
   token is read from a configured file at request time and never taken from a query
   parameter, header, cookie or body — a caller-supplied credential would let a browser
   choose its own clearance.
3. **`av-tiles` gains an admin surface** (`GET /admin/api/counters`, off by default),
   reversing round 2's decision 15: a refusal counter must be readable from outside the
   process for a refusal to be provable rather than asserted.
4. **`av-store`, `tokio` and `bytes` are optional dependencies of `av-jobs` behind a
   non-default `store-fixture` feature.** A `[[bin]]` cannot see `[dev-dependencies]`, so a
   binary that talks to a real store forces a real dependency; gating it keeps the crate's
   default graph exactly what it was and preserves round 2's decision 7 by construction.
5. **Streaming is reached through a new `Executor::execute_streaming` with a default body
   that delegates to `execute`**, not by changing `execute`'s signature, so every existing
   executor and test is untouched. On that path `JobCompletion.outputs` carries only the
   manifest, which itself lists every tile's key, hash and size.
6. **A tile set's identity must not depend on how it was written**, the second form of
   question 223's rule: the streaming and buffered paths produce a byte-identical manifest
   and byte-identical tiles, asserted over two independent sinks.
7. **The terrain payload is an explicit binary heightmap**, not a second image format, and
   its heights are packed from the source raster's own R and G bytes — a disclosed shortcut:
   this round gives terrain a layout, an encoding and a determinism proof, not a DEM reader.
8. **`.pnts` over `.b3dm`** for 3D Tiles, because `.b3dm` content is a glTF binary and this
   track would have had to write a second from-scratch geometry encoder.
9. **`serde_json` enters `av-jobs` for `tileset.json` alone**, never for this crate's own
   wire format. One line in `Cargo.lock`, no new package.
10. **The container executor reuses the existing failure kinds** — no new
    `JobFailureKind`, no proto wire change — and delivers inputs and outputs by bind mount
    because a job's inputs and outputs are both plural and Colima supports no other host
    path. The container never touches the object store.
11. **The hardening list is declared twice (Python and Rust) and kept honest by a test that
    parses the Python source at test time**, because production Rust cannot import a Python
    list.
12. **The ten-gigabyte tile set lives on the host filesystem through a bind mount, never in
    a Docker volume**: the VM overlay has 10.8 GB free and Colima's image collector evicts
    images once that disk crosses its threshold.
13. **The scale shape is 34.4 GB, not "about ten"**, for the quadtree-quantisation reason
    above.
14. **788 anonymous Docker volumes holding 40.2 GB were NOT pruned.** They are the measured
    reason the image collector keeps firing, but this daemon is shared with an unrelated
    Supabase stack and none of them carries an `av.test` label, so the decision is a
    human's.
15. **`docs/heavy-plan.md`'s H6 was not started**, and is recorded as not started rather
    than attempted badly.

### What was in flight when the round paused

`tests/test_tiles_container.py` — the docker-gated proof of the containerised gateway. The
image is built and present (`sha256:e20f1c36b39d…`), its digest and both base digests are
recorded in `services/tiles/IMAGE_DIGEST.md`, the test collects, and the three images it
needs were re-pulled by recorded digest. It has not been observed green: the first attempt
lost `av-tiles:local` to the image collector, the second lost `quay.io/minio/minio` to the
same collector, and the third blocked for over twenty minutes on question 207's host-wide
lock, held by the other track's `minio_store` test in `/Users/probe/code/AltaVista-edge`.

### What remains

1. Run `tests/test_tiles_container.py` alone, with the host quiet, and record the result.
2. Run the round's gates, which were not run: `cargo test -p av-jobs -p av-tiles -p av-store
   -p av-label`, `cargo test -p av-jobs --features store-fixture`, `cargo test --workspace
   --exclude av-kernel --no-fail-fast`, `cargo clippy --workspace --all-targets -- -D
   warnings`, `cargo clippy -p av-jobs --all-targets --features store-fixture -- -D
   warnings`, `cargo deny check`, `.venv/bin/python -m pytest -q -rs`, `buf lint proto`, and
   `buf breaking proto` against develop's tree. The round's own baselines, measured before
   any work: **1269 passed / 0 failed / 3 ignored** on the workspace gate, clippy clean,
   `cargo deny` ok with the six accepted spoore wildcards, and **761 passed / 4 failed / 18
   skipped / 4 errors** on the full Python suite — all eight non-passing pre-existing host
   artefacts (cold Rust build timeouts, and `rust:1.90-bookworm`,
   `altavista-cfs-lockstep:local`, `av-edge-plugin:local` and `av-proposer:local` evicted by
   the image collector; the first of those was restored by re-pulling it by digest).
3. H6 (`web/js/entities/`): covariance ellipsoids and keep-out volumes, glTF asset models
   with attitude, and the RIC jitter test extended to a ten-metre RPO model.
4. The terrain layer in the viewer is still a typed, named refusal
   (`TerrainLoaderNotImplementedError`) — the terrain tiler now exists to feed it.
5. `web/js/globe.js` and `web/js/tiles_layer.js` still drive `TileLoadScheduler` directly;
   only the gateway-backed imagery layer goes through `web/js/layers/` today.
6. The generated 3D Tiles tileset is proved against the spec by independent Python
   re-derivation, not by loading it through the vendored 3DTilesRendererJS.

### Open items for the lead

1. **788 anonymous Docker volumes, 40.21 GB, 40.08 GB reclaimable**, on a VM with 10.8 GB
   free. This is the measured cause of the image evictions that cost this round three
   docker-gated attempts and cost round 2 six skips. `docker volume prune` would reclaim it
   but the daemon is shared with an unrelated Supabase stack; a human should decide.
2. **Cross-track docker contention is now costing results, not just time.** The other
   track's `minio_store` test held the host-wide lock for over twenty minutes while this
   track's last proof waited behind it. Question 207's serialisation needs a scheduler, not
   a convention.
3. **`crates/av-command/src/audit.rs::from_line_sink` is dead outside tests** and the
   warning is invisible to `cargo clippy --all-targets`. One line, another team's file.
4. **The round's gates are unrun**, so `dcb6265` and `d43d9b7` are unverified against the
   full workspace and the full Python suite.
5. **`docs/compliance/` still owes a control-matrix row** for `av-store`'s and
   `av-catalog`'s hand-rolled protocol clients (round 2's open item 8), and now for
   `av-jobs`' container executor and `av-tiles`' image.

## Status (heavy manager, 2026-09-18) — round 4

**Round 4 delivered questions 228's two findings for the globe, and H7's control
matrices. H6 was not started and the plan is NOT closed** — see "What remains".

### What landed, one commit per accepted task

| Commit | What |
| --- | --- |
| `8c815ee` | Question 228 finding 1: the layer memory budget becomes a hard admission limit; deferral counted; byte costs from the manifest; a resident cost reconciled when it changes |
| `332080d` | H7: control matrices for `av-tiles` and `av-jobs`, and question 218's row for the hand-rolled protocol clients |
| `dd48495` | Question 228 finding 2, the globe half: the globe's imagery and terrain go through the one per-viewer `LayerManager`, proved in a real browser from the scene graph |

### The budget proof (question 228 finding 1)

The lead's own measured case, driven by `web/js/layers_budget_check.mjs` and asserted by
`tests/test_viewer_layers_budget.py`: **32 tiles of 3 147 060 bytes against a 41 943 040
(40 MiB) budget — a wanted set 2.40× the budget.**

| | unfixed | fixed (`8c815ee`) |
| --- | --- | --- |
| max resident bytes | **100 705 920** (2.40× budget) | **40 911 780** (≤ budget) |
| max resident + pending | field did not exist | **40 911 780** |
| `softViolationCount` | 38 | **0** |
| `deferredCount` | field did not exist | 228 |
| resident level histogram | 1/4/11/16 — everything | **1/4/8/0** — the coarse tiles |

Resident bytes are sampled after every `update()` **and** after every load settles, and
the maximum over all samples is reported, so the bound is "never exceeded at any step".
A camera-move phase proves the hard limit does not deadlock the viewer: 13 evictions,
none of phase 1's tiles surviving. Every assertion was shown failing against the unfixed
module first.

I verified all of this myself rather than from the worker's report, and additionally
proved the coarse-first rule is load-bearing by inverting which level counts as coarse:
the normal run leaves 1/4/8 resident at levels 0/1/2, the inverted run leaves 13 tiles
all at level 3.

**A second way through the budget, found by my own probe, not by the worker's check.** A
resident entry stored the `byteCost` captured at admission and nothing re-read it, so
tiles admitted against the fallback estimate kept it forever: 20 tiles read as 5 242 880
bytes while truly occupying 62 941 200 against a 41 943 040 budget — 50 % over, with
`softViolationCount` 0 and `deferredCount` 0, every counter clean. That is round 3's
defect 5 returning through a new door. `update()` now reconciles a still-wanted resident
entry's cost and adjusts `residentBytes` by the signed delta.

**A nuance for the lead's log.** "`softViolationCount` stays zero by construction" holds
absolutely for the admission path, and for the viewer provided a caller awaits
`fetchManifest()` before the first `update()` — which the wiring now requires and the
doc comment states as a requirement. An upward revision of content the view still wants
is the one remaining path that can legitimately trip it, because the alternative is
evicting what the user is looking at. Measured in that case: 62 941 200 accounted
truthfully with the tripwire firing 4 times, instead of 5 242 880 accounted as clean.
The fix converts a silent breach into a counted one; it does not claim an
unreachability that would be false.

### The browser proof (question 228 finding 2)

`tests/test_viewer_globe_layer_manager.py` drives a real headless Chrome against a real
viewer server and asserts **from the scene graph** — walking `viewer.globeLayer.group`
for a mesh whose `material.map` is a real texture with non-zero dimensions. Measured:

| | |
| --- | --- |
| `hasLayerManager` / `globeUsesManager` | true / true |
| `registeredLayers` | `["imagery", "terrain"]` |
| tile meshes / with a bound texture | **2 / 2**, texture 64 px |
| resident bytes / budget | 524 288 / 67 108 864 |
| `softViolationCount` / page exceptions | **0 / 0** |
| `failedCount` / `failureNames` | 2 / `["TerrainLoaderNotImplementedError"]` |

The `failedCount` of 2 is the terrain adapter's disclosed typed refusal being asked once
per wanted key and then remembered rather than retried every frame — round 3's
failure-memory policy, visible in a real browser for the first time.

The collector subscribes to all three CDP channels and **`Runtime.exceptionThrown` is
the one that mattered** (question 211). It earned itself immediately: the first run drew
nothing, because `BodyInterp.orientation` (`web/js/interp.js`) reads `this.quat.length`
unguarded and threw a `TypeError` on every frame against a scenario with no `quat`. The
frames were failing silently. A second test points the same collector at a page built to
throw and asserts it sees it — a gate is tested against a page that fails before it is
trusted.

### The host: why no docker-gated test can run here, root-caused and closed

Round 3 lost three docker-gated attempts to "the image collector", round 2 lost six
skips, and the cFS image has disappeared eight times; question 228 recorded it as
"recorded not closed". **It is closed.** Captured live from the k3s journal inside the
Colima VM (`scratchpad/r4-image-gc-evidence.txt`, 268 matching lines):

```
image_gc_manager.go:394 "Disk usage on image filesystem is over the high threshold,
  trying to free bytes down to the low threshold" usage=86 highThreshold=85
  amountToFree=3513538969 lowThreshold=80
kubelet.go:1652 "Image garbage collection failed multiple times in a row"
  err="wanted to free 3513567641 bytes, but freed 986027519 bytes ..."
```

The kubelet's image GC runs every ~5 minutes. The image filesystem sits at 85–86 %
against a high threshold of 85, so it fires every cycle and tries to free ~2.5 GB. Every
Supabase and k3s image is held by a running container, so Docker refuses (`must be
forced`) and it frees **0 bytes**, cycle after cycle. The only images it can ever
successfully delete are the ones no container holds — **exactly every image this track
pulls**. The 986 027 519 bytes it freed at 13:08 was the PostGIS image I had pulled three
minutes earlier.

Measured: MinIO and PostGIS were re-pulled by their recorded digests, verified identical
to `services/*/IMAGE_DIGEST.md`, and **both were destroyed within about three minutes.**
This is not a race with our labelled pruning, not question 207's lock, and not anything
either track does. Until a human reclaims space, a docker-gated test here cannot run —
only skip, which is what question 212 makes it do, visibly.

The space is the same the lead has been asked about twice: **814 anonymous volumes,
41.86 GB, 41.73 GB reclaimable (99 %)**, on a 59 GB data disk with 9.4 GB free. They
carry no `av.test` label and the daemon is shared with an unrelated Supabase stack, so
round 3's decision 14 stands and I did not prune them.

### The host, second finding: it cannot currently carry this round's Rust gate

Measured while the workspace `cargo test` ran: **swap 6 818 MB of 8 192 MB with ~720 MB
free RAM**, the Colima VM alone holding **14.3 GB RSS**, and **three concurrent cargo
invocations across two worktrees** (mine, plus two of the native-dynamics team's in
`/Users/probe/code/AltaVista-edge`) — question 218's limit is two. Four `rustc` units sat
at 2–3 s of CPU each over 9 minutes: starved, not working. After 55 minutes the run had
produced no test result line, so I stopped it in favour of the round's actual
deliverables and recorded that rather than let it grind.

### Gates

| Gate | Result |
| --- | --- |
| `buf lint proto` | **clean**, exit 0 |
| `buf breaking proto` against develop | **clean**, exit 0 |
| `cargo deny check` | **advisories ok, bans ok, licenses ok, sources ok** |
| viewer/JS pytest (11 files) | **147 passed, 0 failed, 2 errors** |
| `node` checks | `globe_lod_check`, `tiles3d_check` byte-identical to pre-round output; `layers_check`, `layers_budget_check`, `gateway_imagery_layer_check` pass |

The 2 errors are `tests/test_track_viewer.py`'s `av_track_demo_bin` fixture, which builds
`crates/av-track`'s demo binary with cargo and errors rather than skipping by design. It
timed out at 900 s behind the target-directory lock. Same class as round 3's recorded
"cold Rust build timeouts"; aggravated by my own clippy run holding that lock.

**No Rust source and no Cargo manifest changed this round** — `git diff --name-only
cc1f006..HEAD` matches no `.rs`, `Cargo.toml` or `Cargo.lock`. All three commits are
JavaScript, Python and documentation. So no SBOM regeneration is owed (questions 220 and
224), and `cargo test --workspace` / `cargo clippy --workspace` cannot have been changed
by this round's work.

**`cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace
--exclude av-kernel` were both started and neither completed on this host**, and that is
recorded rather than claimed clean. Clippy ran for over seventy minutes and emitted 20
`Compiling`/`Checking` lines and **zero** `error` or `warning` lines before it was
stopped; the workspace test run produced no test-result line in fifty-five minutes. Both
were starved by the host state measured above, not by anything in the tree. What supports
"the Rust gate is unaffected" is the `git diff` above, not a green run — no Rust file in
this workspace was touched by any of this round's four commits. **The lead should treat
the Rust half of this round's gate as unrun**, exactly as round 3's was, and take it in a
quiet window. It is this round's largest outstanding risk, stated here rather than buried.

### Decisions taken this round (numbered for the lead's log)

1. **The budget is enforced on admission, not by eviction.** Eviction cannot enforce a
   budget whose whole wanted set is protected — that is the shape of finding 1.
   Reserving `pendingBytes` at admission is what makes the invariant survive the async
   gap between admitting a load and its completion.
2. **Admission order and reported priority order are deliberately two different total
   orders.** `update()` still returns the plan sorted by `comparePriority`, whose order
   an existing test pins; admission walks a separately sorted copy by `compareAdmission`
   (ascending level, coarser first). Under a hard limit, admitting highest screen-space
   error first fills the budget with fine detail and the user sees nothing.
3. **Eviction also runs to make room before admission**, for unwanted entries only; a
   request that still does not fit is deferred with a `continue`, never a `break`, so one
   oversized request cannot starve everything behind it.
4. **Budget deferral is counted separately** from concurrency deferral and from the
   failure blacklist, or `deferredCount > 0` would prove nothing about the budget.
5. **A resident entry's byte cost is reconciled when it changes**, and the tripwire's doc
   comment now states precisely that it is unreachable for admission but not unreachable
   overall.
6. **The manifest is decoded in the browser by a small hand-rolled protobuf reader**,
   adding no dependency and no CDN (question 51). I cross-checked it against the real
   protobuf runtime on adversarial input — a zero size, 2 147 483 647, multi-byte varint
   coordinates, and a 300-character object key crossing a length-prefix boundary.
7. **`GlobeLayer` takes the manager as an optional argument**, so the absent path is
   byte-for-byte what it was and every existing check stays honest by construction, while
   `enableGlobe()` always passes it so the manager-routed path is the production path.
8. **`window.altavistaViewer`** publishes the viewer under the existing `altavista*`
   convention, so the streaming layer is observable from outside the module graph — for
   the scene-graph assertion, and for the lead's own devtools during the drive.
9. **The 3D Tiles overlay was NOT routed through the manager.** The lead's finding names
   the globe; wiring the overlay honestly means either a duplicate fetch whose payload
   nothing renders or a much larger `tiles_layer.js` rewrite. Deferred, not faked.
10. **The `av-tiles` image was NOT rebuilt.** It needs `rust:1.90-bookworm` (~1.5 GB)
    plus build layers on a disk already over the GC threshold, would evict the images
    other work needs, and could not make the test runnable anyway for the reason above.
11. **The 814 anonymous volumes were again NOT pruned**, for round 3 decision 14's
    reason, now with the measured mechanism attached.
12. **I stopped my own workspace `cargo test` after 55 minutes of no progress** under
    host thrashing, and recorded that rather than let it starve the round's deliverables.

### Defects found in review, and their root causes

1. **A resident byte cost was never reconciled** — found by my own probe, not the
   worker's check. `_onLoaded` stored the cost captured at admission; `update()`
   refreshed only `lastUsedStep`. 50 % over budget with every counter clean.
2. **An invariant check that was not independent** — found by the worker itself,
   correctly. It summed `byteCost` off the manager's own stored entries, which on the
   unfixed code agreed with `residentBytes` perfectly and reported `residentMatches:
   true` against a 50 % breach. Recomputing truth from a fresh `plan()` gave it teeth.
   The same shape as round 3's defects 1–3, and worth naming again: **a check that reads
   its answer from the thing it is checking proves nothing.**
3. **A circular import through the `web/js/layers/` barrel** — `globe.js` →
   `layers/index.js` → `imagery_layer.js` → `globe.js`, throwing `ReferenceError: Cannot
   access 'ImageryLayerAdapter' before initialization`, because a class `extends` clause
   evaluates immediately. Fixed by importing the modules directly, never the barrel.
4. **`BodyInterp.orientation` reads `this.quat.length` unguarded** — a body with no
   `quat` throws a `TypeError` on every frame, and the viewer draws nothing with no other
   signal. Found by the new browser check's exception collector. Not fixed in this round
   (it is the scene/interp path, not this track's file) — recorded for its owner.
5. **`LayerManager` had no `removeLayer`** — a hard blocker for the second
   `enableGlobe()` against a long-lived per-viewer manager.
6. **`av-tiles` enforces no bind-address restriction**, unlike `av-command` and
   `av-gateway`, which route `--bind` through `resolve_loopback_bind_address`. Verified
   by grep in both directions. Recorded as a Gap in its new matrix.
7. **`GET /admin/api/counters` has no access control.** Deliberate and documented,
   mirroring `av-command`; recorded as a Gap anyway because `av-gateway`'s equivalent was
   gated under the lead's R5.1 ruling and two admin surfaces taking opposite postures is
   the lead's call.
8. **`crates/av-command/src/audit.rs::from_line_sink` is still dead outside tests** —
   round 3's open item 3, confirmed present in this round's release build, another team's
   file, invisible to `cargo clippy --all-targets`.

### What remains

1. **H6 (`web/js/entities/`) was not started** — covariance ellipsoids, keep-out volumes,
   glTF models with attitude, instanced markers and trails, and the RIC jitter test
   extended to a ten-metre RPO model. Recorded as not started rather than attempted badly.
2. **Question 228 finding 2 is half delivered.** The globe goes through the manager and is
   proved in a browser; the **catalog listing route, the Layers panel, and selecting a
   catalogued tile set are not built**, so a user still cannot pick a gateway tile set.
   The stack-up recipe in `scripts/heavy/README.md` is therefore unchanged and does not
   yet cover catalog registration.
3. **The 3D Tiles overlay** does not go through the manager (decision 9).
4. **`tests/test_tiles_container.py` still has never been observed green**, and on this
   host it cannot be.
5. The plan is **not closed**; there is no `## Delivered` section, because 1–4 above are
   outstanding.

## Status (heavy manager, 2026-09-21) — round 5


### What landed, one commit per accepted task

| Commit | What |
| --- | --- |
| `710d4ab` | Task 1: question 229's three rulings and round-4 defect 4 — the `BodyInterp.orientation` guard, `av-tiles` admin authentication through the same `av_command::oidc::verify` the main port uses, both binds through `resolve_loopback_bind_address`, `removeLayer` pinned by a check with teeth, the `av-tiles` control matrix moved off three Gaps |
| `e710efc` | The host-wide docker-test lock made re-entrant within one process — the self-deadlock that has been wedging docker-gated testing for three teams, and the real reason `tests/test_tiles_container.py` "has never been observed green" |
| `2f49e0b` | Task 3: the 3D Tiles overlay's content fetch routed through the one `LayerManager` (round 4's deferred decision 9), through the vendored renderer's own `fetchData` plugin hook, with the existing fixture overlay and its checks byte-identical |
| `f65cbfb` | Task 2a: `GET /api/catalog/tilesets` behind the same credential rule as `/api/tiles/*`, a typed `av-gateway` gRPC client for the catalog selector, and the tiler registering its manifest in the catalog so a generated tile set is listable |
| `55d52ac` | Task 2b: the Layers panel — a user selects a catalogued tile set and it is registered, fetched and budgeted on the one `LayerManager`; plus the stack-up recipe's catalog and panel steps. **Its tiles are requested but not painted — see below** |

**Round 5 delivered questions 228's finding 2 in substance but not entirely: a selected
tile set streams, but is not yet drawn. H6 was not started and the plan is NOT closed** —
see "What remains".

### Review of task 1 (inherited from the terminated predecessor manager)

The predecessor dispatched task 1 and was cut off by three consecutive transient API
errors before it could verify the work. I reviewed it from the artifacts rather than from
any report.

**The guard has a check that fails without it — verified, not assumed.** Reverting the
`!this.quat ||` clause alone and re-running `web/js/attitude_slerp_check.mjs` fails exactly
the two new missing-quat checks and nothing else; with the guard, 75 of 75 pass. The
control checks (a body WITH a real quat still reproduces its recorded samples at both ends
and slerps strictly between them) are what stop the guard from having quietly disabled real
attitude behaviour.

**The admin-auth tests exercise a real token and a real refusal, not a mock.**
`av_command::test_support::TestIssuer` generates a fresh RSA-2048 key pair through the
system OpenSSL (ADR-004) and mints a real RS256 compact JWS; the tests spawn a real
`TcpListener`, send a real HTTP request over a real socket, and read real 401/403/200
status lines with the refusal counters moving. Nothing is stubbed.

**`removeLayer` is genuinely pinned.** `layers_budget_check.mjs` phase 4 measures, on a
manager holding two registered layers: removing one clears its resident, pending and failed
entries and only its bytes (`residentBytes` 2000 → 1000, `pendingBytes` 4000 → 2000),
cancels its in-flight load (`cancelledCount` 0 → 1, `evictedCount` 0 → 1), leaves the second
layer's entries and bytes untouched, is a no-op on an unregistered id, and leaves the
manager able to admit a freshly registered layer. A `removeLayer` that only dropped the id
from its own map passes none of that.

### Defects found in review, and their root causes

1. **The admin-auth suite never proved a signature was checked.** Root cause: its one
   negative token case was malformed rather than merely untrusted, so it exercised
   `oidc::verify`'s pre-signature parsing path only. Found by reading which refusal branch
   each test actually reaches rather than by counting tests. Fixed in `710d4ab` with a
   foreign-key test and an expired-token test. **Definitively root-caused.**
2. **`target/debug/deps` at 1 026 183 entries, costing rustc 99 % of its wall clock in
   `__getdirentries64`.** Root cause: cargo never garbage-collects `deps`, and twenty-two
   days of multi-round building across many feature combinations accumulated a million
   hashed artifacts; every rustc invocation re-scans the directory via
   `-L dependency=…`. This is the real cause of rounds 3, 4 and 5's lost Rust gates.
   **Definitively root-caused, with a controlled before/after on the same command.**
3. **The two-slot cargo mechanism starves a polling waiter.** Root cause: `LOCK_EX |
   LOCK_NB` in a poll loop is not a queue; a team running back-to-back jobs reclaims the
   slot inside the poll window. Observed directly, fixed by blocking acquisition.
   **Definitively root-caused.**
4. **The stack-up recipe bound an admin port that, after task 1, refused every caller**,
   with nothing in the file saying so. Root cause: a deny-by-default gate added to a
   surface whose only operator-facing documentation predated the gate. Fixed in `710d4ab`.
5. **Several `web/js/` sources contain a literal NUL byte** (a composite-key separator,
   e.g. `` `${id}\x00${key}` `` in `web/js/layers/layer.js` and
   `web/js/layers_budget_check.mjs`). Not a correctness defect — the separator choice is
   sound, since a layer id cannot contain NUL — but `grep` and `file` classify those
   sources as binary, so **they drop out of ordinary repo-wide `grep -rn` sweeps unless
   `-a` is passed.** That is a reviewability hazard for exactly the kind of audit this
   track relies on ("claimed only after grepping"). Recorded, not fixed: the fix
   Recorded, not fixed: the fix (write the separator as a backslash-u-0000 ESCAPE
   rather than as a raw NUL byte -- byte-identical at runtime, but the file stays
   textual) is safe, yet it touches files outside this round's committed scope.
   **Recommended for the lead's ruling.** Until then, anyone grepping under
   `web/js/` must pass `grep -a` or silently miss those files. To be precise about
   provenance: this round introduced none of them. Of the eleven files its commits touch,
   exactly one contains a NUL, `web/js/layers_budget_check.mjs`, and it held the same three
   at `01df602` as it does now. I mention it because it bit me directly — a heredoc
   round-trip put a raw NUL into this very status draft while I was writing the paragraph
   above, which would have made `heavy-plan.md` itself a file `grep` skips. Checked and
   stripped before landing.

### Review of task 3 (the 3D Tiles overlay through the manager)

The worker found two real defects while building it, both by measurement rather than by
reasoning, and both are worth the lead's attention:

1. **A fetch cache keyed by URL.** This project's own fixture gives every tile node the
   same relative `content.uri` (`"tile.glb"`), so one tile's fetch silently satisfied 42
   unrelated tiles. It surfaced as a real diff in `web/js/layers_check.mjs`'s output
   (`cancelledCount`, `evictedCount`, per-step `residentBytes` all moved) — the
   "existing checks must be unchanged in outcome" constraint is what caught it. Rekeyed to
   the local tile key, the same granularity `release(key)` uses, and pinned by a check that
   43 keys sharing one URL still get 43 independent fetches.
2. **Two `update()` calls per frame on one shared manager cancel each other's work.**
   `LayerManager.update(view)` treats every registered layer's `plan(view)` as THIS tick's
   entire wanted set, so a globe driving it and an overlay driving it each saw the other's
   content as unwanted — cancelling in-flight loads and evicting resident entries every
   frame. Exactly one drives it per tick now. The counterfactual is kept as a check:
   reinstating the hazard takes the imagery layer's load count from 2 to 8 and its
   cancellations from 0 to 8, and leaves it not resident after tick 1.

**What I verified myself rather than from the report.** I re-ran
`node web/js/tiles3d_manager_check.mjs` and reproduced every number; I ran the new
`tests/test_viewer_tiles3d_manager.py` (10 passed, including the live headless-Chrome
test) and `tests/test_viewer_globe_layer_manager.py`/`test_viewer_globe.py`/
`test_viewer_layers.py` (33 passed) against the change; I confirmed the new plugin is
imported from `./layers/tiles3d_layer.js` directly and never through the barrel, so round
4's defect 3 (a circular import through `layers/index.js`) is not reintroduced; and I
checked the "existing checks unchanged" claim at the code level rather than accepting it,
by extracting the four symbols `web/js/tiles3d_check.mjs` imports from the modified
`tiles_layer.js` at the base commit and in the working tree — `selectTiles3D`,
`parseTileset3D`, `ecefFromRootTransform`, `enuBasisFromRootTransform`, all four
byte-identical. That claim needed checking: the file those symbols live in changed by 254
lines, so "unchanged in outcome" was not self-evident.

**The disclosed limitation is real and I am letting it stand.** The vendored renderer's own
traversal decides which URLs it asks for, and that is not our `selectTiles3D`. A URL our
`plan()` does not recognise falls through to an ungated direct fetch, counted in
`fetchDataFallbackCount` — measured 0 in every run, but not structurally impossible.
Closing it means owning the traversal, which is the rewrite round 4 judged out of budget.
Counted and disclosed beats quietly unbounded.

### Review of task 2a (the catalog listing route and the tiler's registration)

The credential rule is implemented more strongly than the tiles proxy's, and deliberately
so: `catalog_tilesets()` takes **no request object at all**, so there is no incoming header
for it to read, forward or be tricked by. The tiles proxy needs a header allowlist because
it legitimately forwards `Range` and `If-None-Match`; this route has nothing to forward, so
the safe behaviour is structural rather than maintained. The test that matters proves the
response is byte-identical with and without a caller-supplied `Authorization`, and that the
token the gateway actually received was always the server's own.

**Verified myself rather than from the report:** `cargo tree` (0 `av-catalog` edges in
`av-jobs`' default graph, 1 under `--features store-fixture`); `AV_SBOM_REBUILD=1` (67
passed, so the manifest change owes no SBOM regeneration); `tests/test_catalog_tilesets_route.py`
(21 passed); and `cargo clippy --workspace --all-targets -- -D warnings` clean with the
change in the tree.

**One thing a plain workspace clippy would have missed, and the lead should know about it
generally.** `av-tile-fixture` declares `required-features = ["store-fixture"]`, so
`cargo clippy --workspace --all-targets` **silently skips that binary entirely** — the new
Rust code in it would have gone unlinted by this round's own named gate. It is clean, but
only because it was linted explicitly with the feature enabled. Any crate in this workspace
with `required-features` has the same hole in the standing gate.

**Two gaps the worker disclosed plainly, which I am accepting rather than papering over.**
There is no real-Postgres round trip for the registration path — `PgClient::connect` and
`CatalogAsset::insert` are unexercised against a live container, because nothing
docker-gated can run on this host today. And the credential-rule test drives a fake
in-process gRPC server rather than a real `av-gateway` subprocess; that pins the Python
side's request shape and error mapping, while `av-gateway`'s own auth and label enforcement
stay covered by that crate's Rust tests. Both are boundaries, not full coverage, and both
should be closed once the docker lock is free.

**A rule the worker introduced on its own and argued for:** `--catalog-host` and
`--dry-run` are now mutually exclusive, because the dry-run sink always produces an
`AssetRef` with no provenance and `CatalogAsset::from_asset_ref` refuses that outright — so
the combination could never have succeeded, and refusing it at parse time beats failing
deep inside a job that already ran. I checked the underlying refusal is real and tested
directly, not just the flag check. I am ratifying it; the lead may want to log it.

### Review of task 2b (the Layers panel)

**What I checked myself rather than from the report.** The panel module imports nothing at
all, so the "panels never reach into `scene.js`/`globe.js`/`layers/`" contract holds by
construction. `app.js` awaits `fetchManifest()` before `addLayer`, which is the ordering
round 4's budget invariant depends on — and the live test proves it from the outside by
recomputing `byteCostSourcesOnFreshPlan` from a fresh `plan()` and requiring exactly
`["manifest"]`, which the fallback-estimate path could not produce. `app.js` imports the
`layers/` barrel; round 4's defect 3 was a cycle through that barrel, so I checked
(`imagery_layer.js` → `globe.js`, and nothing under `layers/` imports `app.js`: no cycle)
and confirmed it in a real browser with zero page exceptions. 50 node assertions pass, the
panel's own pytest passes, and the wider viewer suite is 70 passed with it in the tree —
including `tests/test_viewer_layout.py`, the guard the panel deliberately avoided
disturbing.

**The stubbing in the live test is cut in the right place.** A real `grpc.server` speaking
the real `DataGatewayService` with real `CatalogRecord`s, and a real HTTP server serving a
real `TileSetManifest` with real tile bytes and correct ETags. Nothing in `server.py`,
`gateway_client.py`, `tiles_client.py` or any `web/js/layers/` module is patched. The
credential rule is proved end to end: the token the fake gateway actually received is the
server's own, not anything the browser could supply.

### The one thing round 5 does NOT deliver: a selected tile set is streamed, not drawn

This is the most important paragraph in this status, so it is not buried at the end.

Question 228's decision asks for "a catalogued tile set … selectable from a panel", and
this round's brief asked for headless proof that "a selected tile set's tiles are requested
and drawn". **Requested is delivered and proved. Drawn, in the literal pixel sense, is
not.**

What genuinely works, measured in a real browser against real (wire-protocol) backends: the
panel lists what the gateway's catalog selector returns, a toggle registers a real
`GatewayImageryLayerAdapter` on the one shared `LayerManager` under a stable
manifest-derived id, `fetchManifest()` is awaited before admission (proved by
`byteCostSourcesOnFreshPlan == ["manifest"]` from a fresh `plan()`), **two real tile GETs
arrive at the tile server** for the selected set, the bytes are budgeted and counted
(`resident 2, pending 0`; 526 336 against 67 108 864; zero soft violations), and toggling
off releases them.

What does not happen: those tiles are not painted. `GlobeLayer` binds a mesh's
`material.map` from `getResidentPayload(globalKeyFor(this.imageryLayerId, k))` — hard-wired
to its own `'imagery'` adapter id — so a layer registered under
`gateway-tileset:<sha>` can never have its payload reach a mesh. I verified this in
`web/js/globe.js` myself rather than taking the worker's word for it. The globe's own two
meshes do still have textures bound, from the default adapter, unaffected.

The worker declined to add that seam and was right to. It is a `globe.js` change in the
area this round already found to be delicate, and it needs a decision that is the lead's,
not a worker's: **does a selected tile set REPLACE the globe's default imagery, or
composite over it, and what happens with two tile sets toggled on at once?** Round 4
refused to fake the overlay wiring for the same kind of reason. **I am recommending this as
round 6's first task, with that question answered first.**

### One usability defect in the Layers panel, found by driving it rather than by reading it

`web/js/app.js` re-renders the panel from the animation loop, throttled to every 500 ms so
the resident-bytes readout stays live. The panel contract is teardown-and-rebuild
(`container.innerHTML = ''`), so **the entire panel DOM is destroyed and recreated twice a
second, including the buttons a user has to click.**

I hit this directly: two clicks on `Refresh catalog` failed with "ref is stale (element
removed)" before a coordinate click landed. A human will experience it as a click that
occasionally does nothing, and as losing text selection or focus inside the panel twice a
second. It is not a correctness bug — the throttle is deliberate and documented, and the
500 ms figure is a reasonable choice for a live number — but rebuilding a list of
interactive controls at that cadence to keep two numbers fresh is the wrong trade.

Not fixed this round, and recorded rather than quietly accepted. The obvious fix is to
split the tick-driven update from the structural one: let the animation loop update only
the budget section's text nodes in place, and rebuild the tile-set list only when the
catalog or a layer's state actually changes. **Recommended for the lead's ruling**, since
it touches the panel contract every other panel in this codebase also follows.

### Decisions taken this round (numbered for the lead's log)

1. **I killed the predecessor's leftover pytest rather than waiting for it.** It had run 55
   minutes with 0.53 s of CPU. `sample` showed it blocked in `flock` on
   `~/.altavista/locks/docker-tests.lock`, held by the verify team's docker suite in
   `/Users/probe/code/AltaVista-verify` (1 h 28 m and counting at that point). Root cause is
   **correct behaviour, not a defect**: `tests/test_viewer_tiles_route.py` imports
   `tests/heavy_stack.py`, which stands up the real docker stack and therefore rightly takes
   question 207's host-wide lock. Leaving it queued would have fired a docker stack-up
   unattended later, so I cleared it. Its edits were already in the tree and are in
   `710d4ab`.
2. **The cargo-slot waiter must block, not poll** — see "The cargo-slot mechanism" below.
   Recorded here because the native-dynamics team owns the real script.
3. **I moved this worktree's `target/debug` aside rather than deleting it**, and did not
   touch the other worktrees' equally bloated ones, which are not mine. See "The Rust gate"
   above; the stale directory is left for the user to remove at a quiet moment.
4. **The predecessor's admin-auth suite needed a signature test, and I added one rather
   than accepting the suite as it stood.** Its only invalid-token case was the malformed
   string `"not.a.real.token"`, which `oidc::verify` rejects on segment-count and base64
   grounds *before* signature verification runs. Nothing in the suite would have failed if
   the route had been wired to a verifier that never checked a signature. The added test
   mints a structurally perfect token with the granting group from a second, independent
   RSA-2048 key; a second added test expires a correctly signed token against the injected
   clock, which also proves the injected reading is consulted rather than passed and
   dropped.
5. **The stack-up recipe's `av-tiles` invocation gained `--admin-role`.** Task 1 made the
   admin surface deny-by-default, which silently turned the recipe's `--admin-bind` port
   into one that binds and refuses everyone. The group is deliberately NOT the
   `tile-readers` group step 1's token carries — an admin credential and a tile-clearance
   credential are never the same token, the separation `tests/heavy_stack.py` already keeps.
   The added block is marked as derived from the implementation rather than pasted from a
   live run, which is the standard the rest of that file holds itself to.
6. **The 3D Tiles overlay is gated at the vendored renderer's own `fetchData` hook, not by
   taking over its traversal.** That hook is real and public
   (`registerPlugin`/`invokeOnePlugin`, read out of the pinned vendored source), it carries
   a per-tile `AbortSignal`, and it makes the manager the thing that authorises a load
   while the renderer consumes exactly what the manager loaded — no duplicate fetch, which
   is what round 4 refused to write. Owning the traversal as well was judged out of budget
   again; the residual is counted in `fetchDataFallbackCount` rather than left unbounded.
7. **Only one caller drives `layerManager.update()` per tick.** With a globe and an overlay
   on one shared manager, two independent drivers make each see the other's content as
   unwanted and cancel and evict it every frame. The globe's view already carries
   everything the overlay's adapter needs, so the overlay stands down when a globe is
   present. This one is worth the lead's attention beyond this task: it is a property of
   `LayerManager`'s contract, and **any third layer registered on that manager by a future
   task inherits the same constraint.**
8. **`caller_clearance` is sent empty on every catalog query.** The gateway derives
   clearance from the verified token and refuses a non-empty value that disagrees; sending
   one could only be redundant or a refusal.
9. **An unconfigured gateway is a typed error, never an empty list.** "No gateway is wired
   up" must not render to a user as "this deployment has no tile sets". A gateway that is
   up but has no catalog tier is a second, separately distinguished case.
10. **`av-catalog` rides the existing `store-fixture` feature rather than a new one**, so
    catalog registration lives in `av-tile-fixture` and never in the runner —
    `crates/av-jobs/Cargo.toml` says in as many words that the runner must not depend on
    the catalog, and `cargo tree` confirms it still does not.
11. **The Layers panel is chooser-reachable in every layout rather than threaded into the
    default tree**, and I accepted the worker's reasoning rather than overriding it.
    Threading it into `defaultLayoutTreeForScenario` changes that function's output for
    EVERY scenario, and `tests/test_viewer_layout.py` plus
    `web/js/layout/layout_tree_check.mjs` pin the ordinary-scenario tree byte-identically
    as a named regression guard for a DIFFERENT feature (F5.1's sweep default) — neither
    file being this task's to modify. It follows `FEASIBILITY_PANEL_ID`'s existing
    precedent exactly. **The practical consequence for the lead's drive is one extra
    click**: open the pane chooser and pick "Layers". If the lead wants it visible by
    default, that is a deliberate change to a pinned default tree and should be its own
    task with the guard updated knowingly.

### Gates

| Gate | Result |
| --- | --- |
| `buf lint proto` | **clean, exit 0** |
| `buf breaking proto` against develop's tree | **clean, exit 0** |
| `cargo clippy --workspace --all-targets -- -D warnings` | **clean, exit 0, 20.13 s** |
| `cargo clippy -p av-tiles --all-targets -- -D warnings` | **clean, exit 0, 11.49 s** |
| `cargo test -p av-tiles` | **59 + 8 passed, 0 failed, exit 0, 29 s from cold** |
| `cargo test --workspace --exclude av-kernel --no-fail-fast` | **148 binaries, 1482 passed, 0 failed, 4 ignored**, exit 0, **456 s** — with 3 visible `SKIPPED` lines (question 212). See "The wedge cleared" below |
| `cargo deny check` | **advisories ok, bans ok, licenses ok, sources ok**, exit 0 — with exactly the six expected spoore wildcard warnings (2 for `spoore-models`, 4 for `spoore-tree`) and three duplicate-crate warnings (`syn`, `tower`, `windows-sys`) |
| `AV_SBOM_REBUILD=1 pytest tests/test_sbom.py` | **67 passed, 0 skipped, 0 failed**, 35.75 s — see "The SBOM question" below |
| `node` checks at the final head | **8 of 8 exit 0** — `layers_check`, `layers_budget_check`, `globe_lod_check`, `tiles3d_check`, `gateway_imagery_layer_check`, `attitude_slerp_check`, plus the new `tiles3d_manager_check` and `layers_panel_check` — and `layout/layout_tree_check` exit 0. `tiles3d_check`'s output hashes to `911eeeef…` at the base and at the final head, so the "unchanged in outcome" constraint holds at the output level as well as the code level |
| Python, the round's own surface (10 files, no docker) | **143 passed, 0 failed** — the viewer, panel, catalog-route, tiles3d-manager, globe-layer-manager, layout, jitter and docker-lock suites |
| `cargo test --workspace --exclude av-kernel --exclude av-lockstep --lib --bins --no-fail-fast` | **38 binaries, 1083 passed, 0 failed, 0 ignored**, exit 0 |
| `.venv/bin/python -m pytest -q -rs` | **857 passed, 11 failed, 18 skipped**, 497 s — every skip visible with its reason (question 212). All 11 failures attributed below; **9 are mine and are one command from green, 2 are not mine** |

**Why two gates are blocked, and by what.** Not by anything in this tree. A `pytest` run
of `tests/test_tiles_container.py` in `/Users/probe/code/AltaVista-verify` self-deadlocked
on question 207's host-wide docker lock (defect 6 above) and has held it for over two
hours with 1.30 s of CPU. Every docker-gated test on this host queues behind it:

- `cargo test --workspace` reaches `crates/av-catalog/tests/catalog_postgis.rs`, correctly
  announces `WAITING for the docker-test lock`, and stops there. One result line
  (83 passed) came out before it blocked.
- **`cargo test -p av-tiles -p av-jobs -p av-catalog` cannot complete either**, for the
  same reason and the same target — worth flagging because it is named verbatim in this
  round's gate list. Everything in it except that one docker-gated target was run and is
  green (see the table).
- `crates/av-lockstep`'s own lib tests take the real lock and block the same way.
- `.venv/bin/python -m pytest -q -rs` queues on it identically, and does so from an
  easily-missed direction: `pyproject.toml`'s `testpaths` is `["tests", "services/cfs/tests"]`,
  and the cFS tests are docker-gated, so excluding only the docker-gated files under
  `tests/` is not enough. Measured: a run so filtered still blocked in `flock` at about 70 %,
  which is exactly where `services/cfs/tests` begins.
- The native-dynamics team's `drm_attitude_control_cfs` is queued behind it too, with
  0.03 s of CPU in 36 minutes.

Question 212's design is working exactly as intended — a docker-gated test runs for real
or skips visibly, and there is deliberately no flag to skip one that *can* run — so there
is no honest way to route around this. What I could measure, I measured:
`cargo test --workspace --exclude av-kernel --lib --bins --no-fail-fast` (which excludes
the docker-gated integration targets) reached **22 test binaries, 667 passed, 0 failed, 0
ignored** before hitting `av-lockstep`'s own lock-taking lib tests.

**I did not kill the wedged process.** It is another team's run, `AltaVista-verify` is
explicitly outside my worktree, and destroying it is the lead's call, not mine. It is
recorded here with its PID and the evidence instead. Commit `e710efc` prevents a recurrence.

### The wedge cleared, and both blocked gates then ran in full

The wedged process exited on its own after about two and three-quarter hours (not by my
hand). I re-ran both blocked gates immediately rather than leave the round's headline
evidence missing, and **both completed for the first time in three rounds**:

- **`cargo test --workspace --exclude av-kernel --no-fail-fast`: 148 binaries, 1482 passed,
  0 failed, 4 ignored, exit 0, in 456 seconds.** Round 4 ran this for fifty-five minutes and
  never reached a result line; the lead's own clone gate took about ninety minutes. Three
  `SKIPPED` lines, each visible with its reason. The docker-gated targets — including
  `crates/av-catalog/tests/catalog_postgis.rs`, which had queued forever earlier in this
  same round — ran for real and passed, and the lock correctly serialised them against each
  other along the way.
- **`.venv/bin/python -m pytest -q -rs`: 857 passed, 11 failed, 18 skipped, in 497 seconds.**
  Every skip is an opt-in gate printing its own reason. The 11 failures are attributed
  individually below; none is a defect in this round's code, and I checked rather than
  assumed.

That the full docker-gated suite runs at all on this host is itself new. It needed three
things to line up: the user's volume prune, the `deps` directory fixed, and the lock
deadlock gone.

### The 11 Python failures, each attributed

**Nine are mine, and all nine are one command from green.** They are two separate symptoms
of the same cause — committing changes under a generated compliance artifact:

- 2 in `tests/test_evidence_bundle.py`: the bundle hash moved because `710d4ab` rewrote six
  rows of `docs/compliance/av-tiles/control-matrix.md`. Regenerated value measured:
  `4e1e41ee8a1a9227d8df6d2bc224570c4de2e1803b86fe6ac0681b86e8ff8d98`.
- 7 in `tests/test_sbom.py`: `test_the_epoch_is_never_wall_clock` for each of the six Rust
  components, plus `test_all_six_rust_components_share_exactly_one_epoch_from_git`. The
  committed SBOMs record `2026-09-18T15:44:03Z`; git's committer date for `RUST_EPOCH_PATHS`
  is now `2026-09-22T03:06:32Z`, because `f65cbfb` committed Rust changes. Worth noting for
  the standing gate: **any commit touching Rust moves this epoch, so these seven fail on
  every round that changes Rust until the SBOMs are regenerated.** They passed earlier in
  this same round, before `f65cbfb` landed — `AV_SBOM_REBUILD=1` was 67 passed then.

Both are fixed by the one documented command, which also has its own test
(`tests/test_regenerate_compliance.py`): `.venv/bin/python scripts/kit/regenerate_compliance.py`.
**I did not run it**, because this round added no workspace member and my instructions
restrict generated compliance documents to that case. Round 4 ended identically and the lead
regenerated them (`7163149`).

**Two are not mine:**

- `services/cfs/tests/test_image_digest.py::test_image_digest_matches_recorded_value` — the
  cFS image's digest against its recorded value. This round never touched `services/cfs`,
  and `develop`'s own recent history is a run of re-pinning commits after repeated image
  evictions ("Re-pin the four images after the ninth eviction"). This is that story
  continuing, not this round's work.
- `tests/test_viewer_layers_stream.py::test_eviction_and_cancellation_both_actually_happened`
  — `evictedCount == 0`. **I root-caused this rather than waving at it.** The test drove a
  real stack for real: 13 765 frames, `maxFrameMs` 2.346, a real manifest
  (`f1f700b4…`). It is not a regression: this round left `web/js/layers/layer.js`,
  `gateway_imagery_layer.js` and `imagery_layer.js` **byte-identical to the base commit** —
  the only files it touched under `web/js/layers/` are `index.js` (one barrel export line)
  and `tiles3d_layer.js` (the 3D Tiles adapter, which this test does not exercise). What
  the failure actually means is what the test's own message says: the streaming proof's
  tile set never approached `MEMORY_BUDGET_BYTES`, so its "budget respected" result would
  have been true for the wrong reason. **The test is right and the proof's shape is wrong** —
  it needs a tile set large enough, or a budget small enough, to force eviction. It has
  almost certainly never run before, since docker-gated tests have been unrunnable here for
  three rounds. **Recommended as a real item for the lead**, not a flake.

### The Rust gate: three rounds of "the host is slow" root-caused, definitively

**`target/debug/deps` in this worktree held 1 026 183 entries. One directory, over a
million files.** The main repo's is the same shape (1 092 950). This is the cause of
rounds 3, 4 and 5's lost Rust gates, and it is not memory, not CPU, and not the
two-cargo-job rule.

How it was found, rather than guessed. `cargo test -p av-tiles` sat at 0.22 s of CPU over
7.5 minutes. Its two `rustc` children had 5.15 s and 3.87 s of CPU after 8 minutes, while
the load average was **2.48 on 12 cores** — a starved process on an idle machine, which is
a contradiction that has to be explained rather than absorbed. `sample` on the parent
showed it in `poll`, waiting on its children. `sample` on a child showed the answer:

```
__getdirentries64  (in libsystem_kernel.dylib)        2227      (of ~2250 samples)
```

**99 % of rustc's wall clock was spent enumerating a directory**, not compiling. rustc is
passed `-L dependency=target/debug/deps` and scans it on every invocation; at a million
entries on APFS that scan costs minutes, and it is paid again by every single rustc unit.
`ls -U target/debug/deps | wc -l` did not itself complete inside 120 seconds — the measuring
command hitting the same wall is corroboration, not an inconvenience.

This retro-explains, exactly, what previous rounds recorded as unexplained host slowness:
round 4's clippy emitting 20 `Compiling` lines and zero diagnostics in seventy minutes,
round 4's workspace test producing no result line in fifty-five minutes, and round 3's
"cold Rust build timeouts". None of those runs were starved of RAM or cores. They were
scanning a million files per compilation unit.

**What I did.** In THIS worktree only, `mv target/debug target/debug.stale-1M-deps` (an
instant rename; `target/debug` itself has only 80 direct entries) and re-ran the identical
command. I did not touch `/Users/probe/code/AltaVista` or `/Users/probe/code/AltaVista-edge`,
which are not mine, and I did not delete the stale directory — removing a million files
would contend for I/O with the rebuild that immediately follows it, and disk is not scarce
(950 GB free). **It is left in place for the user to delete at a quiet moment:**
`rm -rf /Users/probe/code/AltaVista-aiplane/target/debug.stale-1M-deps`.

**The before/after, same command, same machine, minutes apart — this is the proof.**

| | bloated `deps` | after `mv target/debug` aside |
| --- | --- | --- |
| `target/debug/deps` entries | **1 026 183** | **1 883** |
| `cargo test -p av-tiles` | **8 min, one `Compiling` line, no result; killed** | **29 s wall clock, exit 0** |
| what was built | essentially nothing | **144 rlibs, 979 MB, a full COLD dependency tree** (nalgebra, tonic, openssl, axum, hyper, …) |
| test result | never reached | **59 passed, 8 passed, 0 failed** |
| `cargo clippy -p av-tiles --all-targets -- -D warnings` | not attempted | **clean, exit 0, 11.49 s** |
| `cargo clippy --workspace --all-targets -- -D warnings` | round 4: **70 min, 20 `Compiling` lines, zero diagnostics, abandoned** | **clean, exit 0, 20.13 s** |

A cold rebuild of the entire dependency tree now finishes in a fraction of the time a
single incremental unit failed to finish in before. The 59 is also the task-1 evidence:
the predecessor measured 57, and my two added tests make exactly 59.

**It is still costing the other team, live, as I write this.** Late in the round both
host-wide cargo slots were held by `/Users/probe/code/AltaVista-edge`:
`cargo rustc -p av-orbital --lib` at **20 min 45 s elapsed on 0.18 s of CPU**, and
`cargo check -p av-kernel` at **10 min 20 s on 0.10 s**. Same signature, same cause, their
`target/debug/deps` uncleaned. Because the two slots are host-wide, their starvation
becomes everyone's queue: my own follow-up clippy sat behind them and I cancelled it as
redundant rather than wait. **Cleaning their target directory is worth more to that team's
throughput than any scheduling change.**

**What the lead should do with this.** Every worktree on this host is affected, including
the lead's own clone (whose gate "runs in about twenty-five minutes alone" — that is the
figure with a bloated `deps`, so a clean one should be faster still). Cargo never garbage-
collects `deps`; twenty-two days of continuous multi-round building across many feature
combinations is enough to accumulate this. A periodic `cargo clean`, or at minimum a check
on `deps` entry count in the gate, belongs in the standing rules. **I recommend this become
a question in `docs/open-questions.md` — it has cost this track three rounds of Rust
evidence.**

### The host, measured this round — and why round 4's manager and the lead disagreed

Question 229 recorded the lead's measurement ("64 GB of RAM with 78 percent free, so memory
is not the constraint the manager reported") against round 4's manager's ("swap 6 818 MB of
8 192 MB with ~720 MB free RAM"). **Both numbers were true of the same machine.** Measured
here, in one sitting:

| | |
| --- | --- |
| `memory_pressure -Q` free percentage | **79 %** — the lead's figure, reproduced |
| `vm_stat` pages free | **4 649 pages × 16 KiB = 74 MB** genuinely free |
| `vm_stat` pages inactive + speculative | 1 648 890 pages ≈ **26 GB**, reclaimable |
| `sysctl vm.swapusage` | **6 690 MB of 8 192 MB used, 1 502 MB free** |
| cumulative swapouts / swapins | 3 918 951 / 1 711 938 |
| load average, 12 CPUs | **2.48** — the machine is NOT CPU-bound |

`memory_pressure`'s "free percentage" counts reclaimable (inactive, speculative,
purgeable) pages as free; `vm_stat`'s "pages free" does not. That single definitional
difference is the whole disagreement. Neither manager mis-measured.

**What that means for the Rust gate, measured not inferred.** While `cargo test -p
av-tiles` ran, its two `rustc` units had accumulated **3.76 s and 2.87 s of CPU over 6
minutes of wall clock** with a **load average of 2.48 on 12 cores**. A starved-for-CPU
process on an idle machine is a contradiction; the signature is page-fault stalling against
6.7 GB of swap with 74 MB free, not contention for cores. Round 4's diagnosis was closer to
the mechanism than question 229 credits, though its attribution (the Colima VM at 14.3 GB
RSS) does not hold today: `limactl` is at **100 MB** RSS this round, and the largest
consumers are two WebKit content processes (1.60 GB and 1.14 GB) and a macOS
`installd` at 1.02 GB and 25.6 % CPU — background work of the user's machine, not of either
track.

**Consequence the lead should plan around:** the two-cargo-slot rule (question 229) bounds
concurrency but cannot make a swapped host fast. The Rust gate on this box is slow for a
reason no scheduling rule fixes.

### The cargo-slot mechanism: one defect, found by using it

`scripts/dev/cargo-slot` does not exist in this worktree yet — it is on the native-dynamics
team's round-4 list — so I wrote a local stand-in against the same two lock files under
`$HOME/.altavista/locks/`. My first version polled with `LOCK_EX | LOCK_NB` every 15
seconds. **It starved.** Measured: the other worktree's `cargo test -p av-kernel
--no-default-features` exited, and its replacement (`cargo test -p av-run`) took the freed
slot inside my poll window; my waiter lost the race repeatedly and never acquired. The fix
is to **block**, not poll: two threads each take a blocking `LOCK_EX` on one slot, the first
to be granted wins and the other releases immediately. `flock` is FIFO-fair per file, so
this queues honestly. It acquired on the first attempt after the change.

**For the native-dynamics team, who own the real script:** a non-blocking poll loop is not a
fair queue and will starve a team that runs one job against a team that runs many
back-to-back. The mechanism question 229 asks for needs blocking acquisition.

### My own browser drive of the Layers panel

I drove it myself, in a real browser, as a user would — not through a test harness. Viewer
server on `127.0.0.1:18099` with **no** `--gateway-endpoint`, which is the configuration a
lead will hit first if they start the viewer before the gateway.

What a user sees, in order. There is no Layers pane in the default layout (decision 11), so
the first action is the pane chooser: pick "Layers" from any pane's "Swap panel…" and the
panel appears. It shows **CATALOGUED TILE SETS** with a `Refresh catalog` button and
"Tile set catalog not loaded yet — click 'Refresh catalog' to fetch it", and **STREAMING
BUDGET** reading `resident / budget  0 B / 64.0 MB` and `deferred requests  0`. It does not
fetch at boot, which is right: a panel that silently hammers an unconfigured backend on
every page load is worse than one that waits to be asked.

Clicking `Refresh catalog` renders, in red:

> Cannot load the tile set catalog: (503) no data gateway is configured for this viewer
> server (pass `--gateway-endpoint`/`--gateway-token-path` to `python -m altavista serve`)

That is the server's own message, carried through verbatim with its status code — not a
generic "failed", and emphatically not an empty list that would read as "this deployment
has no tile sets". The requirement that "not configured" must never render as "no tile
sets" is met end to end, from `GatewayNotConfiguredError` through the route's 503 to the
red line a user actually reads. Browser console: **no JavaScript exceptions**, only the
expected 503 network log that any failed fetch produces.

**What this drive could NOT cover, and why.** Selecting a real tile set and seeing tiles
drawn needs a real `av-gateway` and a real catalogued tile set, which needs
`scripts/heavy/README.md`'s step 0, which starts a MinIO container — and nothing
docker-gated can run on this host while the lock is wedged. **The half of the feature a
user most cares about is therefore proved by the headless checks and not yet by my own
eyes.** I am saying so rather than implying a fuller drive than I did.

### The SBOM question, answered by measurement rather than by argument

Round 4 could say "no SBOM regeneration is owed" cheaply, because no Rust source and no
Cargo manifest changed. This round **a Cargo manifest did change**: `av-jobs` gained
`av-catalog` as an optional dependency, and `Cargo.lock` gained exactly one line for it.
Questions 220 and 224 make that worth checking rather than asserting.

`tests/test_sbom.py` carries a deep check that the default suite skips: with
`AV_SBOM_REBUILD=1` it runs `cargo auditable build --offline` for all six Rust components
and requires each regenerated SBOM to be **byte-identical** to the committed one. It has
not been runnable on this host in previous rounds. It is now, and it ran: **67 passed, 0
skipped, 0 failed, 35.75 s.**

So **no SBOM regeneration is owed**, and the reason is structural rather than lucky: the
new dependency sits behind the `store-fixture` feature, and the SBOMs are built without
it, so it is absent from every component's dependency graph as shipped. Verified
independently with `cargo tree`: `av-catalog` appears **0** times in `av-jobs`' default
edges and **1** time under `--features store-fixture`. The H1 rule that no hot-path crate
depends on the store or catalog is intact.

### The evidence bundle: two failures I caused, deliberately left for the lead

`tests/test_evidence_bundle.py` has two failures, and they are mine:

```
regenerated bundle_sha256='4e1e41ee8a1a9227d8df6d2bc224570c4de2e1803b86fe6ac0681b86e8ff8d98'
does not match docs/compliance/BUNDLE.md's recorded
'e116b9822f73a8a6d2a5efac37c139895fb82760a9330b0607754c1ec5e88855'
-- run the documented command and update BUNDLE.md
```

Cause, by construction rather than by guess: the evidence bundle hashes every
`docs/compliance/*/control-matrix.md`, and `710d4ab` rewrote six rows of
`docs/compliance/av-tiles/control-matrix.md` under question 229's rulings. `BUNDLE.md`'s
own history table already records this exact scenario as routine maintenance — its
`bceec44` row reads "`bundle_sha256` moved with no SBOM byte changed — some other
evidence-dependent input (e.g. a control matrix) changed".

**I did not regenerate it, and that is a deliberate choice, not an oversight.** My
instructions for this round are explicit: do not touch the generated compliance documents
unless a workspace member is added, in which case run
`scripts/kit/regenerate_compliance.py`. I added no workspace member. Round 4 ended the same
way and the lead regenerated it (`7163149`). So rather than take a step I was told not to
take, here is everything needed to make it a few seconds' work:

```sh
.venv/bin/python scripts/kit/regenerate_compliance.py
```

and the value it will record is `4e1e41ee8a1a9227d8df6d2bc224570c4de2e1803b86fe6ac0681b86e8ff8d98`,
measured here. Nothing else in the suite depends on it.

### A gap in the drive path that nobody had noticed: no way to apply the catalog schema

The updated recipe surfaced something real. Registering a tile set needs a PostGIS catalog
with the schema applied, and **there is no way to apply it outside a test.**
`av_catalog::migrate::Migrator::apply_pending` is a library function, and the only callers
in the whole workspace are `crates/av-catalog/tests/catalog_postgis.rs` and
`crates/av-gateway/tests/catalog_selector.rs` — both of which start and own their own
container. `services/catalog/run-dev-catalog.sh` starts a container and its own header says
as much: a developer who wants the schema applied "runs `Migrator::apply_pending` themselves,
e.g. from a small cargo run/test harness". No such harness exists. I checked by grep rather
than taking the comment's word for it.

So the lead's drive has a missing rung: step 0.5b cannot be performed as written by anyone
who does not first write a small Rust program. **That is worth a task of its own** — a tiny
`av-catalog-migrate` binary, or a documented `cargo run --example`, would close it. It is
recorded here rather than improvised at the end of a round.

### What remains

0. **A selected tile set is not drawn** (see the dedicated section above) — round 6's first
   task, once the lead answers whether it replaces or composites with the default imagery.
   Closely related: **H6 (`web/js/entities/`) was not started for the second round running**
   — covariance ellipsoids, keep-out volumes, glTF models with attitude, instanced markers
   and trails, and the RIC jitter test at a ten-metre RPO range (question 46). I ran out of
   round before it, and recorded that rather than starting it badly; its brief is written
   and ready at `scratchpad/brief-task4.md` if the lead wants it dispatched first next
   round. **The plan is therefore NOT closed and there is no `## Delivered` section**, since
   H6 and the drawing half are outstanding.
1. **Regenerate the compliance documents.** Nine of the eleven Python failures are the
   evidence bundle's hash and the six Rust SBOM epochs, both moved by this round's own
   commits. One command closes all nine:
   `.venv/bin/python scripts/kit/regenerate_compliance.py`. I left it for the lead under the
   round's own rule about generated compliance documents. **The wedge itself is gone** — the
   process exited on its own and both blocked gates then ran in full, so nothing is waiting
   on it any more, including the lead's drive.
2. **`target/debug.stale-1M-deps` should be deleted** at a quiet moment, and the other
   worktrees' equally bloated `target/debug/deps` cleaned by their owners. The main repo's
   was measured at 1 092 950 entries.
3. **The streaming proof does not stress its own budget.**
   `tests/test_viewer_layers_stream.py::test_eviction_and_cancellation_both_actually_happened`
   fails with `evictedCount == 0` against a real 13 765-frame run. Not a regression — the
   whole path is byte-identical to the base commit — but the test is correct that a zero
   there makes its "budget respected" result meaningless. The proof needs a bigger tile set
   or a smaller budget.
4. **The cFS image digest no longer matches its recorded value**
   (`services/cfs/tests/test_image_digest.py`). Not this round's work and not this track's
   file; it is the re-pinning story `develop`'s recent history already tells, now visible
   again because docker-gated tests can finally run.
5. **`tests/test_tiles_container.py` — round 4's item 4 — still has not been observed green,
   and now there is a THIRD reason.** I ran it directly rather than inferring anything from
   its absence among the failures, and it **skipped**, visibly and precisely:

   > image `av-tiles:local` is present locally but its id
   > `sha256:ed91784131e4…` does not match the digest `sha256:e20f1c36b39d…` recorded in
   > `services/tiles/IMAGE_DIGEST.md` (question 212(a): a test trusts an image only after
   > comparing it to its recorded digest). Rebuild with `services/tiles/build-image.sh`.

   So the two causes that hid it before — Colima evicting the image, and the lock deadlock —
   are both genuinely closed, and a third was underneath them: the image on this host is
   stale against its recorded digest. Question 212(a) is doing exactly its job here. Fixing
   it is `services/tiles/build-image.sh` plus re-recording the digest, which is the same
   re-pinning chore item 4 describes for cFS. **I did not rebuild it**: it is an image-digest
   record, the same class of artifact as the compliance documents, and the round was already
   over.
