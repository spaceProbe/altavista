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
