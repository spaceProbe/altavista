# Edge ingestion boundary: plan

Requested by the user 2026-09-12 as one of two parallel tracks (the other is
`aiplane-plan.md`). Decisions in `open-questions.md` question 200. This is P2's missing half
in `architecture.md` ("edge boundary: seccert certificates, per-batch signatures, labels on
ingest; spoore engine consuming simulated measurements; p99 latency budget") and the edge
half of ADR-004.

## Goal

A signed, chained, labelled measurement stream from a simulated asset, carried over mTLS
under seccert-issued certificates into a durable, verifiable log, with every rejection
counted, consumed by the spoore engine into tracks, and the edge-to-core latency measured
against a recorded budget. Everything reproducible from hashes, as the rest of the platform.

## What exists

- CDM v1 `Measurement` (core.proto) with `sensor_id`, `shard_key`, `frame_id`, `meta`;
  `Label` (core.proto); `Provenance`; ground-station telemetry and contact windows in the
  kernel (M25.1), telemetry as CDM measurements into the viewer (M25.3), `RunProducts.
  measurements`, the `PortTrafficLog` sidecar with its sorted, hashed record layout (M25.4).
- Hash-chained evidence in `crates/av-dynamics-service/src/evidence.rs` and `admin.rs`
  (`/admin/api/evidence`, ledger `verify`), the FIPS rule module (`fips.rs`), control
  matrices under `docs/compliance/`, `av-grpc`'s tonic-over-OpenSSL client and the nginx
  mTLS front template (`services/gmat-service/deploy/`).
- spoore's engine and CDM by path dependency (`~/code/spoore`, on `main`); spoore-io's
  producer and consumer traits; spoore's assumption ledger.
- seccert (RFC 8555 ACME CA, Python/FastAPI, ECDSA P-384, two-tier PKI) checked out at
  `/Users/probe/code/secdeploy/work/seccert`, runnable from a venv or its Dockerfile.
- Not yet: a batch message, signing, chaining, verification, label enforcement, an ingest
  service, a plugin, an edge buffer, the engine consuming anything, a latency line item.

## Isolation

The edge team works in the git worktree `/Users/probe/code/AltaVista-edge` on branch `edge`
(from `develop`). New code lives in new crates `crates/av-edge` (batches, signing, chaining,
verification, labels, the edge buffer, the plugin library) and `crates/av-ingest` (the
service), a new proto file `proto/altavista/v1/edge.proto` (this track owns it; changes to
the other proto files must be additive and pass `buf breaking`), new modules under
`altavista/edge/` and, if a panel is needed, `web/js/panels/`. It consumes `av_kernel` and
`av_cdm` as libraries and does not edit the kernel's executor, router or fault modules. The
AI-plane team works in its own worktree at the same time; the two tracks share nothing but
`develop`, which the lead merges into both between rounds and each back after acceptance.
The verification clone for lead gates is `/Users/probe/code/AltaVista-verify`.

## Rules that bind this track

Every standing rule in `teamlog/2026-09-02-team-1.md` and `open-questions.md` applies:
questions 148 (an exit code is not evidence), 154 (no network at test time; the one window
is image build or a recorded one-time fetch), 156 (label every test container and image;
prune by label before creating), 157 (status sections in this plan, not REPORT.md; about
300 tool uses per task), 172 (warm up fresh binaries), 194 (docker-gated tests skip visibly
or run for real, never a silent pass), 199 (no test mutates the process environment), the
crypto rule of ADR-004 (SHA-256 only, the system OpenSSL only, no `ring`, no bundled
crypto; `cargo deny` enforces the bans), the contention rule (check `ps` before a heavy run;
one heavy job per host; two tracks share this host with the lead), and question 189's
per-message epochs. Root-cause every defect or record why there is no path.

## Milestones

**E1 Signed, chained batches (`crates/av-edge`).** `edge.proto`: `MeasurementBatch`
(producer id, sequence, previous batch hash, this batch's hash, ECDSA P-384 signature,
`Label`, the batch's messages as `Measurement`s, provenance, TAI epoch of the batch);
`BatchVerdict` (accepted, or one typed rejection: unsigned, bad signature, chain gap, chain
break, mislabeled, over clearance, stale, duplicate) and `RejectionCounters` per producer,
never silent (ADR-004). Hashing is SHA-256 over the deterministic serialisation the
`PortTrafficLog` already uses; signing and verifying go through the `openssl` crate against
the system OpenSSL, keys loaded from PEM files (what an ACME client writes). A pure verifier
walks a chain and returns the first defect with its sequence. Tests: golden byte vectors for
the hash and a fixed-key signature; every rejection kind produced by one deliberate
corruption each; a chain of one thousand batches verified and its first tampered byte
located; property: signing then verifying is the identity under any label and any message
count including zero.

**E2 Identity from seccert.** A documented, scripted local CA: seccert from
`/Users/probe/code/secdeploy/work/seccert` in a venv created once (the one-time network
window, recorded by hash of its lock file) and started on a loopback port by the test
fixture, plus a standard ACME client (`lego` or `acme.sh`, pinned, fetched once the same
way; the fixture skips visibly if either is absent) issuing an edge leaf and an ingest leaf
under the seccert Root. mTLS between edge and ingest with the Root as the only trust
anchor, over tonic with the OpenSSL connector `av-grpc` already uses; a leaf from any other
CA is refused and counted. Tests: issuance end to end against the local CA; a batch signed
with the issued key verifies with the certificate's public key; the wrong CA is refused;
certificate expiry is honoured (a leaf issued with a one-minute lifetime is refused after it
lapses, with the clock injected, not slept).

**E3 The ingest service (`crates/av-ingest`).** gRPC over mTLS: a plugin presents its
manifest (id, declared output schemas, frames, the label it emits under, its clearance) and
streams batches; the service verifies signature, chain and label per batch, counts and drops
what fails, appends accepted batches to a durable file-backed log chained per partition
(`shard_key`), the log itself being the ledger (ADR-004), and exposes `/admin/api/evidence`,
a ledger `verify` endpoint and the counters. Control matrix in the secrouter format under
`docs/compliance/av-ingest.md`. Tests: every rejection kind counted through the wire;
partition chains verified after a simulated crash mid-append (the last partial record is
detected and excluded, never silently kept); the evidence endpoint returns the chain head
and the counters; deterministic: two runs over the same batches give byte-identical logs.

**E4 The first plugin: a simulated asset.** A plugin library in `av-edge` plus one plugin
binary that replays a kernel run's ground-station telemetry (`RunProducts.measurements`
from `av-run`, or the `PortTrafficLog` decoded through the packet codecs) as CDM
`Measurement`s in signed batches under the label the DRM declares, at the recorded epochs
(real time or as fast as possible, declared). Runs as a labelled container from a Dockerfile
on the pinned base image with `--network none` plus its two allowed endpoints; docker-gated
tests skip visibly. ADS-B replay from CSV is a second plugin after this one lands, not
before (question 200). Tests: a run's measurements arrive at the ingest byte for byte,
with the batch count and the chain head pinned for the demo DRM.

**E5 The engine consumes.** spoore's engine (path dependency) reads accepted measurements
from the log through spoore-io's consumer trait and produces tracks for the simulated
asset; the tracks are compared against the run's truth trajectory with a pinned tolerance
and published to the viewer as an entity beside the truth. The edge-to-core latency
(plugin emit to engine accept) is measured per batch and reported as p50 and p99 with the
budget line recorded next to spoore's (question 43): a number in this plan's status
section, not a claim. Tests: tracks within tolerance for the demo DRM; the latency report
produced from a real run; nothing on this path calls the object store.

**E6 Capture-only while disconnected.** The edge buffers signed batches in a local file log
for hours, replays them in order on reconnect, and the ingest deduplicates by (producer,
sequence) so a replay never double-counts. Tests: the link cut for a declared interval
mid-stream, then restored; the ingest's log is byte-identical to the uninterrupted run's;
the duplicate counter, not the accepted counter, absorbs any overlap.

## Exit

A simulated asset's telemetry leaves a plugin as signed, chained, labelled batches under a
seccert-issued identity, crosses mTLS into an ingest that rejects and counts every defect,
lands in a verifiable per-partition log with an evidence endpoint and a control matrix, is
consumed by the spoore engine into tracks within tolerance of the truth, survives a
disconnection without loss or duplication, and has its latency measured and recorded.
Every number in the status section is traceable to a run hash.

## Status (edge manager, 2026-09-12)

Round 1. **E1 and E2 delivered and accepted; E3 delivered in half (E3a), with its other
half blocked on a lead decision.** Three commits on `edge`, one per accepted task:

- `70fd42e` **E1** — `proto/altavista/v1/edge.proto` and `crates/av-edge`: signed,
  chained, labelled `MeasurementBatch`es, the eight typed rejections, per-producer
  counters, a live chain verifier and a pure chain walker.
- `1e8d82d` **E2** — `crates/av-edge/src/identity.rs`, `scripts/edge_local_ca.py`,
  `tests/test_edge_identity_seccert.py`: machine identity from a locally run seccert CA
  with a pinned standard ACME client, verified against the Root as the only trust anchor
  with an injected clock.
- `55db4e1` **E3a** — `crates/av-ingest`: the durable per-partition chained log that is
  itself the ledger, the accept/reject pipeline, crash recovery, determinism, the
  evidence surface as data, and `docs/compliance/av-ingest/control-matrix.md`.

### Gates (run by the manager with no worker active, host otherwise idle)

| Gate | Result |
|---|---|
| `cargo test -p av-edge -p av-ingest` | 77 passed, 0 failed, 1 ignored |
| `cargo test --workspace --exclude av-kernel --no-fail-fast` | 363 passed, 0 failed, 1 ignored |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean; zero warnings, zero errors, no `#[allow]` added anywhere |
| `cargo deny check` | advisories ok, bans ok, licenses ok, sources ok; `ring` appears nowhere in the output |
| `.venv/bin/python -m pytest -q -rs` | 505 passed, 3 skipped, 171 s |
| `buf breaking --against` develop | not applicable and not runnable — see below |

The one `ignored` test is E1's `regenerate_signature_for_reference`, a deliberate manual
helper for repinning the golden signature if the golden batch or the test key ever
changes. The three pytest skips are the pre-existing cFS image gates
(`test_image_digest.py` twice, `test_image_reproducibility.py` once), each printing its
own reason under `-rs`; none of them is new and none is this track's.

`buf breaking` was not run for two independent reasons, both recorded rather than waved
past: no shared proto file changed this round (`git diff --name-status develop -- proto/`
reports exactly one entry, `A proto/altavista/v1/edge.proto`, a new file this track owns,
against which there is no prior version to break), and `buf` is not installed on this
host at all. If the lead's gate has `buf`, running it costs nothing and should still find
nothing.

### Numbers traceable to artifacts

E1's canonical golden is a 407-byte body hashing to
`af5c6b1ded6e57d2870be6c752b8c34b6b643230c1a286c475c7b4b541473d3e`, verified
independently by the manager by decoding the pinned hex with the committed Python
bindings and recomputing `SHA-256("GENESIS" || body)` — so the Rust and Python sides of
the CDM agree on this batch byte for byte, which is the property E3 and E6 will depend
on. The identity provenance this round pins: seccert at
`/Users/probe/code/secdeploy/work/seccert`, `uv.lock` SHA-256
`6bc7668aef9e9c824b05283c6a97c715a8c1ee32982fdf4fc3691975224396a6`; lego 5.4.1
(`/opt/homebrew/bin/lego`, SHA-256
`372cc983957fc20b8e4b01d2d628270ed9d753a3b1ab9f7f9c49db1638da26a0`), installed through
Homebrew rather than fetched as a release binary, and chosen over acme.sh because
acme.sh is GPL-3.0 and `deny.toml`'s licence policy is permissive-only.

### E3b, and what it is waiting on

E3's remaining half — the gRPC service, the plugin manifest handshake and mTLS — was not
attempted. This repository has an OpenSSL TLS *client* connector (`crates/av-grpc/src/
tls.rs`) and no server acceptor, and question 155 decided that gap with "no new
crypto-adjacent crate", plaintext on loopback within one host, and the service-owned
nginx mTLS template whenever the peer is on another host. Applying that standing rule to
the ingest is the lead's call, not the manager's, so nothing in `crates/av-ingest`
opens a socket and no transport crate appears in its dependency tree. Everything E3's
test list asks for other than "through the wire" is delivered and tested in process.

## Status (edge manager, 2026-09-12) — round 2

Round 2. **E3b, E4 and E5 delivered.** With round 1's E1, E2 and E3a that leaves E6
(capture-only while disconnected) as the only milestone in this plan not attempted. Six
commits on `edge`, one per accepted task:

- `e86bd2f` **E3b (the wire)** — `service EdgeIngest` in `edge.proto` (Announce / Submit /
  GetEvidence / VerifyLedger) with `PluginManifest`, `ManifestAck`, its five typed
  refusals, `IdentityCounters`, and `RejectionCounters.shard_mismatch_count = 12`;
  `crates/av-ingest/src/{service,server,admin,forwarded_cert}.rs`; `crates/av-ingest-client`.
- `4d6c9f8` **E3b (the front)** — `services/av-ingest/deploy/nginx-av-ingest-grpc.conf.
  template`, `av-ingest-server` and `av-ingest-mtls-client` binaries,
  `tests/test_edge_ingest_mtls.py` run for real against round 1's seccert CA.
- `83eeb83` **E4 (the plugin)** — `crates/av-edge/src/plugin/`, the committed
  ground-segment fixture, `av-edge-plugin`, and the byte-for-byte wire test.
- `ab59516` **E5 (the engine)** — `crates/av-track`: the consumer, the spoore engine
  bridge, the truth comparison, the viewer publish and the latency harness.
- `5b25123` **E4 (the container)** — `services/edge-plugin/{Dockerfile,build-image.sh}`
  and `tests/test_edge_plugin_container.py`, with both network proofs measured.
- `e234d9c` **E4b fix** — an image deleted mid-run is a visible skip, not a gate failure;
  the root cause of the recurring disappearance is recorded below.

### Gates (run by the manager with no worker active)

| Gate | Result |
|---|---|
| `cargo test -p av-edge -p av-ingest -p av-ingest-client -p av-track` | 163 passed, 0 failed, 2 ignored |
| `cargo test --workspace --exclude av-kernel --no-fail-fast` | 449 passed, 0 failed, 2 ignored |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean; zero warnings, zero errors, no `#[allow]` added anywhere |
| `cargo deny check` | advisories ok, bans ok, licenses ok, sources ok; `ring` appears nowhere in the output |
| `.venv/bin/python -m pytest -q -rs` | 514 passed, 3 skipped, 276 s |
| `buf breaking --against` develop | not runnable — `buf` is still not installed on this host |

The two `ignored` tests are both deliberate manual generators: E1's
`regenerate_signature_for_reference` (round 1) and E4b's
`generate_default_plugin_config_json`. The three pytest skips are the same
pre-existing cFS image gates round 1 recorded, each printing its own reason under `-rs`;
none is this track's. `git diff --name-status develop -- proto/` reports exactly one
entry, `M proto/altavista/v1/edge.proto`, the file this track owns, so `buf breaking`
would have nothing to find even if it were installed.

### Numbers traceable to artifacts

**E4/E5's pinned chain.** The demo ground-segment replay is 900 batches carrying 900
measurements, chain head
`d1d80d0b6cc9228aaa7479864b8a89f18be88382fb3772bd3c4cc3d7c2dc2698`, from a fixture whose
`port_traffic_hash` is
`c548a78c80954c2a6a159d2b27df10e9f55213e2bed2a1628332b31a63e93dc7`. That head is asserted
in four independent places: the pure replay test, the in-process wire test, the plugin
binary's own output, and — read back out of the ingest's evidence surface from inside a
container's network namespace — the container test.

**E4's precondition for E5, measured.** The positions decoded from the `PortTrafficLog`
through the DRM's declared `PacketCodec` match the run's own truth trajectory to a maximum
deviation of **0 m** over all 900 epochs: FLOAT64 packet fields with unit scale and zero
offset round-trip bit-exactly.

**E5's tracks against truth.** Over 900 scans with nothing unmatched: **max 0.00187 m, p50
5.6e-9 m, p99 8.5e-5 m**, against a pinned tolerance of 1.0 m. Track config hash
`63160880518fab9a2bfe42b801328abd8de794ac1a9c419a6a2f39fa408c3b19`.

**E5's edge-to-core latency (question 43's budget line).** Measured plugin-emit to
engine-accept — the instant before `Submit` is called to the instant its accepted verdict
resolves — over all 900 batches, with a **release** build, five consecutive runs:

| Run | min | p50 | p99 | max |
|---|---|---|---|---|
| 1 | 4.16 ms | 6.02 ms | 8.20 ms | 12.9 ms |
| 2 | 4.04 ms | 6.08 ms | 8.21 ms | 13.4 ms |
| 3 | 4.56 ms | 6.11 ms | 8.30 ms | 11.3 ms |
| 4 | 4.08 ms | 6.20 ms | 11.8 ms | 91.5 ms |
| 5 | 4.39 ms | 6.25 ms | 8.60 ms | 10.6 ms |

**Recorded as p50 ≈ 6.1 ms, p99 ≈ 8.3 ms**, from runs 1–3 and 5; run 4 is reported rather
than discarded but is an outlier, one stall of 91.5 ms dragging its p99 to 11.8 ms.

**The host state these were taken in, stated plainly, because the rule is that a contended
timing result is not a result.** No `cargo`, `rustc`, `pytest` or `docker build` of this
track's was running, and no worker was active. This host does not reach idle: the Colima
VM hosting an unrelated container workload (a `pg_isready` health check every few seconds,
among others) holds 20–25% of one core continuously, and the one-minute load average sat
between 4.05 and 5.41 across the five runs and did not fall further over fifteen minutes of
waiting. These are therefore the quietest numbers this host produces, not numbers from a
quiet host, and the tight spread of p50 across five runs (6.02–6.25 ms) is the evidence
that the measurement is not dominated by that noise.

**What the 6 ms is, mechanically.** It is not the network. `PartitionLog::append`
`sync_all`s every record before returning, so each batch costs one `fsync` on the Colima
VM's overlay filesystem; the loopback gRPC round trip is a small fraction of it. A broker
(question 6) or a group commit across batches would move this number by an order of
magnitude, and the budget line should be read as "fsync-bound durable append", not
"transport-bound".

### Decisions taken this round (for the lead to ratify or overturn)

1. **`ssl_verify_client optional_no_ca`, not `on`, in the ingest's own nginx front.**
   Question 202 requires a wrong-CA or lapsed identity to land in the counters; under `on`
   that is structurally impossible, because nginx refuses before av-ingest is reached. Plain
   `optional` was tried and measured first: it tolerates only a *missing* certificate, so a
   presented-but-untrusted leaf fails the handshake exactly as hard as under `on` (observed
   as nginx's own HTTP 400 for a leaf from an unrelated CA). The consequence, taken
   deliberately: **nginx is the TLS terminator and av-ingest is the sole identity
   enforcement point.** `$ssl_client_verify` is forwarded too, but only as an observability
   aid; nothing in `crates/av-ingest` reads it.
2. **"Announced" is scoped to the serving process instance, not a TCP connection.** `tonic`
   exposes no per-connection identity a service method can key on, and an nginx front may
   pool or multiplex backend connections, so connection scoping would be unenforceable or
   silently wrong. Identity is verified once per `Announce` (the certificate chain-and-time
   walk is amortized; the per-batch ECDSA check is not).
3. **`shard_mismatch_count` is populated as well as, not instead of, the side map.**
   `Ingest::producer_counters` folds its own bookkeeping into the new proto field on every
   read, so `av_edge::chain::ChainVerifier` stays untouched and its `ShardMismatch` arm stays
   deliberately unreachable.
4. **E4's asset is `demo_ground_segment`'s flight instance, and its measurements come from
   the `PortTrafficLog` decoded through the DRM's declared codec** — the route E4 allows and
   the only one available: that codec declares no `PacketField.target`, so
   `RunProducts.measurements` is empty for this DRM, and `GroundStationModel` produces no
   CDM measurement by design. It is also the only choice that gives E5 *position*
   measurements aligned with the truth trajectory; `demo_measurements`' attitude
   measurements would not have worked.
5. **`av-edge` reimplements the minimal CCSDS numeric-field decode rather than depending on
   `av-kernel`**, which would drag `gmat-sys` and, through `av-lockstep`, `tonic` into the
   track's core library. A test cross-checks all 900 records against
   `av_kernel::codec::decode_packet` byte for byte, with `av-kernel` a dev-dependency only.
   Consequence the lead may want to overturn: `cargo test -p av-edge` now builds `gmat-sys`,
   and `crates/av-edge/build.rs` exists solely to emit that dev-dependency's missing rpath.
6. **E4's `--network none` plus its allowed endpoints is rendered as `--network none` for the
   deny-all proof and a labelled `--internal` bridge, with the plugin joining the ingest's
   network namespace, for the allowed-endpoint proof.** Both `connect_plaintext` and
   `bind_loopback` refuse a non-loopback address before touching the network, so plugin and
   ingest must share one namespace; the isolation claims are proven with immediate
   `ENETUNREACH` failures rather than asserted.
7. **`deny.toml`'s `wildcards` drops from `"deny"` to `"warn"`.** E5 is the first task to
   depend on spoore crates beyond `spoore-cdm`, and none of those six declares
   `publish = false`, so cargo-deny correctly refuses `allow-wildcard-paths`' exemption for
   their intra-spoore path dependencies. Verified by flipping the field back and confirming
   every reported wildcard is spoore-internal. The named-crate bans — `ring` and every other
   crypto-adjacent crate — are untouched and still hard failures. **Open item: either spoore
   takes a `publish = false` PR and this reverts, or `"warn"` becomes this workspace's
   posture.**
8. **The E5 consumer trait lives in AltaVista, not in spoore.** `spoore-io` has no consumer
   trait to implement — only a concrete, `rdkafka`-bound `PartitionConsumer` — so
   `av_track::consumer::MeasurementConsumer` mirrors its shape and its module doc records
   the upstream delta. Nothing in `/Users/probe/code/spoore` was modified.
9. **A docker-gated test whose image is deleted mid-run skips visibly rather than failing.**
   The wording is deliberately distinct from "has not been built on this host", so `-rs`
   output never conflates "never built" with "deleted from under us", and neither is a
   silent pass.
10. **E5's tolerance is 1.0 m against a measured maximum of 0.00187 m.** Three orders of
    headroom, justified from zero measurement noise and zero-acceleration truth, chosen to
    catch a real regression rather than to make the test pass.

### Question 196(d): the image disappearances have a measured cause

The plugin image vanished **three times** during this round, twice inside a running test.
The daemon's own event log names the shape precisely: at 19:10:07 CDT, `av-edge-plugin:local`
and its digest-pinned `debian:bookworm-slim` base were untagged and deleted **between two
bursts of layer `create` events from another track's own `docker build`**, with nothing in
any worktree, launch agent or crontab pruning anything (grepped, all five worktrees).

The measured condition that explains it: **the Colima VM's container filesystem is at 92%
— 4.2 GB free of 58.8 GB — with 41.85 GB reclaimable in an unrelated workload's local
volumes** (`docker system df`: 813 volumes, 41.98 GB, 99% reclaimable; `colima.yaml` sets
`disk: 60`). Every symptom question 196(d) records follows from disk-pressure-driven image
garbage collection: it looks like `docker image prune -a`, it spares images backing running
containers, no actor in the repository does it, and it fires when a build allocates layers.

This is the leading hypothesis with the evidence above, not yet a proven fact. **The
confirming experiment is one line and destructive, so it is the user's to run, not this
track's: reclaim those volumes (or raise `disk:` past 60 GiB) and see whether the
disappearances stop.** Rebuild-on-demand with a visible skip stands either way.

### Open items for the lead

1. Decision 1's consequence deserves an explicit blessing: with `optional_no_ca` the front
   no longer drops an unknown client at the TLS layer. The ingest refuses before any batch
   is looked at and every refusal is counted, and the front still enforces TLS 1.2/1.3,
   ECDSA-only suites and the server identity — but defence in depth at the front is
   deliberately traded for the counters question 202 asked for.
2. Decision 7's `deny.toml` change is platform-wide, not this track's alone.
3. Decision 5's consequence: `cargo test -p av-edge` now builds `gmat-sys`. If the lead
   wants that crate's tests GMAT-free, the cross-check moves to `crates/av-kernel/tests/`
   with `av-edge` as a dev-dependency there, and `crates/av-edge/build.rs` disappears.
4. An upstream spoore PR is drafted in prose in `av_track::consumer`'s module doc: extract a
   `poll_measurement`-shaped trait, decide whether partition/offset are concrete or
   associated types, reconcile batch-versus-message granularity, and put the trait in a
   dependency-free crate so a file-only consumer need not pull `rdkafka`.
5. A frame-namespace gap this round exposed: the DRM/viewer frame registry
   (`earth_fixed_demo_frame`) and `spoore_v0::frame`'s five fixed compatibility ids do not
   reconcile, so `measurement_from_pb` rejects every measurement a DRM produces until
   something relabels it. `crates/av-track`'s bridge relabels to `spoore_v0::frame::ECEF`
   immediately before conversion, touching no numeric field. That is a local patch over a
   platform-level gap and should become a real frame mapping.
6. E6 (capture-only while disconnected) is untouched and is the obvious next round.
7. `buf` is still not installed on this host, for the second round running.


## Status (edge manager, 2026-09-12) — round 3

Round 3. **E6 delivered, which closes every milestone in this plan, and all four of
question 205's items landed.** Five commits on `edge`, one per accepted task:

- `45ec8ad` **E6** — `crates/av-edge/src/buffer.rs` (`EdgeBuffer`, `BatchSink`,
  `UplinkDriver`) and `crates/av-ingest/tests/{e6_disconnect,e6_wire_disconnect}.rs`:
  the edge buffers signed batches in a durable local file log, replays them in order on
  reconnect, and the ingest's existing (producer, sequence) dedup absorbs the overlap.
- `7ed75cd` **205 (a) and (b)** — `deny.toml` restored to `wildcards = "deny"`;
  `crates/av-codec` extracted from `crates/av-kernel/src/codec.rs` with a re-export;
  `av-edge` is GMAT-free and its duplicated CCSDS decoder is retired.
- `3ffc3e1` **205 (c)** — `av_cdm::spoore_v0::frame` resolves a v1 frame id from the
  frame registry by declared origin body and axes kind; `av-track`'s boundary relabel
  is gone.
- `1a37949` **205 (d)** — `docs/compliance/av-edge-plugin/control-matrix.md`.
- `cb8f44c` — the edge track's `README.md` section, and a test closing the one
  uncovered path in E6's own new code.

### Gates (run by the manager with no worker active)

| Gate | Result |
|---|---|
| `cargo test -p av-edge -p av-ingest -p av-ingest-client -p av-track -p av-codec -p av-cdm` | 286 passed, 0 failed, 2 ignored |
| `cargo test --workspace --exclude av-kernel --no-fail-fast` | 642 passed, 0 failed, 2 ignored — one contended failure, re-run clean, see below |
| `cargo test -p av-kernel --no-fail-fast` | 845 passed, 0 failed, 2 ignored, 4 visible skips |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean; zero warnings, zero errors, no new lint suppression anywhere |
| `cargo deny check` | **advisories ok, bans FAILED, licenses ok, sources ok** — deliberately, see below |
| `.venv/bin/python -m pytest -q -rs` | 513 passed, 4 skipped, 301 s |
| `buf breaking proto --against develop's proto` | clean, exit 0; `buf` is installed on this host at last |

The two `ignored` tests are the same two deliberate manual generators rounds 1 and 2
recorded (`regenerate_signature_for_reference`, `generate_default_plugin_config_json`).

**The one gate failure, root-caused definitively.**
`cargo test --workspace` reported 641 passed / 1 failed:
`av-lockstep`'s `prune_stale_test_resources_removes_orphaned_labeled_containers_and_images`
failed pushing to its own throwaway registry with `dial tcp 127.0.0.1:34751: connect:
connection refused`. Re-run alone on a quiet host it passes (3 passed, 0 failed), which
makes the effective count 642 passed / 0 failed. The cause is not this round's code —
`git diff --stat f75a272..HEAD -- crates/av-lockstep/` is empty — and it is not flakiness
either. `prune_stale_test_resources()` deletes **every** resource carrying
`av.test=1` daemon-wide, and the lock that guards it,
`docker_lifecycle.rs`'s `DOCKER_TEST_LOCK`, is a process-local `Mutex`. Two `cargo test`
processes in two different worktrees therefore tear out each other's labelled containers
mid-test, and the other track's own `cargo test -p av-kernel` (with its
`docker build --label av.test=1`) was running concurrently with this gate. **This is a
real gap in question 156's isolation model, exposed by two tracks sharing one Docker
daemon: prune-by-label is global, its lock is not.** It will recur for any two tracks that
run docker-gated tests at the same time, and the fix is a daemon-wide lock (a file lock
under a shared path) rather than a process-local one. Recorded for the lead; not fixed
here because `crates/av-lockstep` is not this track's file.

### The four pytest skips, each visible under `-rs`

None is this track's and none is new: the edge plugin image not built on this host (the
round-2 skip question 196(d)'s kubelet image collector explains), and the three
pre-existing cFS gates (`test_image_digest.py` twice, `test_image_reproducibility.py`
once, the last opt-in behind `AV_CFS_RUN_REPRO_BUILD` because it builds over the
network). The kernel suite's own four skips are all the same unbuilt
`altavista-cfs-lockstep:local` image. Every one prints its own reason and its own
rebuild command.

### Numbers traceable to artifacts

**E6's byte-identical log, the milestone's own acceptance criterion.** The link is cut for
a declared two hours of injected TAI time mid-stream and restored; the ingest's partition
log file is compared byte for byte against the uninterrupted run's. In-process: 2685 bytes,
both runs hashing to
`6724d25fe08c88a758217266ba696dd4d862cff8bffa5e3c86d3266a54f759e4`. Over the real gRPC
loopback wire with the client side cut and the ingest process up: 1516 bytes, both runs
hashing to `62f47eeaebff50b057dea037d3a2756788125b638bb360c9b77f246feef68cb9`.

**That equality is within a run, not a pinned golden, and the distinction matters.** ECDSA
P-384 signs with a random nonce, so a batch signed twice yields two different valid DER
signatures of occasionally different length — which is why the same test's log measured
2684 bytes on one execution and 2685 on the next. The test therefore signs its batch chain
**exactly once** and clones it into both runs; the assertion is that a cut-and-replayed
stream produces the identical bytes to an uninterrupted one, which is what E6 asks, and it
is not a hash that can be pinned across runs. Any future repin of those two hashes would be
meaningless.

**E6's counters.** With an interrupted drain replaying two already-accepted batches:
`accepted` is 9 in both the uninterrupted and the interrupted run, `duplicate_count` is
exactly 2, and all eight other rejection counters plus `shard_mismatch_count` are asserted
zero. The duplicate counter, not the accepted counter, absorbed the overlap.

**E6 needed no ingest change.** `av_edge::chain::ChainVerifier` already deduplicated by
(producer, sequence) through `ProducerState::seen_sequences`, from round 1.

**`av-codec`'s extraction, accounted for by name and not by count.** All thirty of
`codec.rs`'s unit tests moved with the module and all thirty run in `av-codec`, verified
by diffing the test-name lists rather than comparing totals. Round 2's kernel figure of
874 necessarily changes: thirty tests left the kernel and one arrived
(`edge_plugin_codec_crosscheck.rs`, the moved GMAT cross-check), so the expected kernel
count is 874 − 30 + 1 = 845, and the thirty are now counted in `av-codec` instead.

**`av-edge` is GMAT-free, measured.** `cargo tree -p av-edge --all-targets | grep -iE
"gmat|tonic"` is empty; `cargo clean -p gmat-sys` followed by `cargo test -p av-edge`
never compiles `gmat-sys`; `crates/av-edge/build.rs` no longer exists. E4's pinned chain
head `d1d80d0b6cc9228aaa7479864b8a89f18be88382fb3772bd3c4cc3d7c2dc2698` is unchanged in
all four places that assert it, which is what proves the two decoders really did agree
before one of them was deleted.

**E5's accuracy survives the relabel's removal, and its config hash does not.** Re-running
the demo with the boundary relabel gone gives 900 matched epochs, 0 unmatched, max 0.002 m
against the pinned 1.000 m tolerance — unchanged, which is the evidence that the relabel
was only ever a label substitution. The track config hash does move, because `TrackConfig`
gained two declared fields:
`63160880518fab9a2bfe42b801328abd8de794ac1a9c419a6a2f39fa408c3b19` becomes
`7a2d33df306ae9861161e1c64eb2cf79c67e7657e419b93fa9365105a53bb06b`, read back out of
`av-edge-latency`'s own JSON report by the manager. **That hash is recorded in this
document and asserted by no test, which is a gap worth closing** — a number nothing
guards is a number that will drift silently.

**No latency measurement was taken this round**, deliberately: the host was never quiet
(the other track ran a full `av-kernel` suite, a `pytest` run and a `docker build` during
this round), and a contended timing result is not a result. Round 2's p50 ≈ 6.1 ms /
p99 ≈ 8.3 ms stands unchanged and unretested.

### `cargo deny check` fails on purpose, and this is the escalation

Question 205 ruling (7) asked for `wildcards = "deny"` with `allow-wildcard-paths = true`,
and said to take it upstream if that did not resolve the spoore warnings. **It does not.**
Under `"deny"`, `cargo deny check` reports `advisories ok, bans FAILED, licenses ok,
sources ok` with exactly six `error[wildcard]`s covering fifteen wildcard dependencies:
`spoore-engine` (5), `spoore-tree` (4), `spoore-ml` (2), `spoore-models` (2),
`spoore-assoc` (1), `spoore-math` (1). cargo-deny 0.20.2's own wording, verified against
this host's binary: "allow-wildcard-paths is enabled, but does not apply to public crates
as crates.io disallows path dependencies."

The root cause is definitive, established by reading the six `Cargo.toml` files under
`/Users/probe/code/spoore/crates/` directly rather than inferring: none of them declares
`publish`, and spoore's workspace root sets no `[workspace.package] publish` default, so
cargo's publishable default applies and cargo-deny correctly refuses the exemption.
Nothing in `/Users/probe/code/spoore` was modified. **The setting stays `"deny"`, no skip
or exception was added, and the check is left failing rather than silenced** — which is
what the ruling asked for. The fix is question 205's fourth `altavista-upstream` proposal:
`publish = false` on those six files. The named crypto-crate bans are untouched and `ring`
appears nowhere in the output.

### Decisions taken this round (for the lead to ratify or overturn)

1. **A capture-only producer's `ProducerPolicy.max_age_ns` must exceed its longest
   planned disconnection, and that is a deployment property, not a code change.** E6's
   own test proves the interaction rather than assuming it away: a batch buffered for
   hours and then replayed is genuinely old by `ChainVerifier`'s STALE measure, and a
   deliberately short 60-second policy is shown rejecting a replay as `STALE`. The fixture
   declares a six-hour budget against a three-hour test window. Nothing in
   `av_edge::chain`, `av_edge::policy` or `av_ingest::ingest` changed. The alternative —
   exempting replayed batches from the staleness check — was rejected because it would
   make STALE unenforceable for exactly the producer most able to abuse it.
2. **The edge buffer commits its acknowledgement watermark once per completed drain, so an
   interrupted drain deliberately re-sends its own already-delivered prefix.** The
   alternative (a watermark advanced per batch) would need a durable write per batch on the
   edge and would still not be atomic with the ingest's own append. Re-sending into a
   duplicate counter that already exists is the cheaper and more honest design, and it is
   what makes E6's "the duplicate counter absorbs any overlap" a real assertion rather than
   a vacuous one.
3. **`av-edge` owns its own buffer record framing rather than sharing `av-ingest`'s.**
   `av-ingest` depends on `av-edge`, so sharing would cycle. The framing mirrors
   `PartitionLog`'s shape (length prefix, per-record SHA-256 through `openssl::sha`,
   `sync_all` per append) and the module doc records what was mirrored and what deviates.
4. **`pub use av_codec as codec;` in `crates/av-kernel/src/lib.rs`**, so every
   `crate::codec::X` path in `drm`, `ports` and `registry` resolves unchanged and no kernel
   caller was touched. Nothing else in the kernel changed.
5. **`av-edge`'s `plugin/packet.rs` survives as a thin adapter over `av-codec`, not as a
   deletion.** It keeps this plugin's narrower typed refusals (`ApidMismatch` against the
   one expected codec, `IsCommand`, numeric-only) and builds a one-entry `ApidMap` to call
   the real decoder. One behavioural difference is documented rather than hidden: for a
   codec whose fields are simultaneously extent-invalid and of an unsupported type,
   `av_codec::decode_packet` reports the extent failure where the old from-scratch pass
   reported the type failure. No codec this plugin decodes has a `BYTES` field.
6. **A body-fixed frame about a body other than Earth is a typed refusal, never
   `Frame::Ecef`.** `spoore_cdm::Frame::Ecef` names Earth specifically; mapping a
   Mars-fixed measurement onto it would be a wrong answer wearing a right answer's clothes.
   The same holds for ENU and NED about a non-Earth body. Each has its own test.
7. **`AXES_KIND_LOCAL_CARTESIAN` resolves to `Frame::LocalCartesian`.** The first
   implementation refused it, which would have meant a producer that properly *registered*
   its frameless simulation frame was rejected where one passing the bare
   `sim.local_cartesian` literal was accepted. `core.proto` calls that axes kind "Frameless
   cartesian space (simulation and unit tests)", which is exactly what the spoore variant
   means, and a test now pins that the registry path and the literal path agree.
8. **`earth_fixed_demo_frame`'s `FrameDefinition` is declared in `av-track`'s
   `TrackConfig`, not in the DRM.** The DRM names the frame only as a model parameter and
   never registers it in `scenario.frames`, which is the actual platform gap; but
   `drms/demo_ground_segment.drm.yaml` carries its own canonical hash
   `7a5944b319fa0dd6f5781c616a75beac94d94fa8d986e5f0b8eaaa46890412d0`, and that hash is
   embedded in the committed, GMAT-generated fixture binaries E4's pinned chain head
   depends on. Registering the frame properly means regenerating those fixtures under GMAT
   and repinning every downstream golden — a platform change, not this round's. Declaring
   it in `TrackConfig` is the narrowest thing that removes the relabel without invalidating
   a pinned hash.
9. **`measurement_from_pb`'s signature is unchanged**; the registry-aware conversion is a
   new `measurement_from_pb_with_frames`, and the old function is that call with an empty
   registry. No existing caller or test was touched.
10. **The plugin's control matrix marks its own transport authentication Partial and its
    confidentiality in transit Gap.** The plugin holds real identity code but its binary
    never calls `verify_identity`, and `connect_plaintext` speaks no TLS at all; the front
    provides both. E6's buffer stores signed batches unencrypted at rest, so
    confidentiality at rest is a Gap while integrity at rest is supported by the per-record
    digest and the chain. Both halves are stated rather than averaged into a Partial.

### Defects found in review this round

1. **The `AXES_KIND_LOCAL_CARTESIAN` inconsistency** (decision 7), found by checking the
   worker's refusal against `core.proto`'s own doc comment rather than against its
   reasoning. The same physical frame resolved through one path and was refused through the
   other. Fixed by the manager; cause definitive.
2. **`services/edge-plugin/Dockerfile` implements none of ADR-004's stated plugin container
   hardening** — no rootless podman or Quadlet, no UBI9 FIPS base (it is a digest-pinned
   `debian:bookworm-slim`), no `USER`, no `--read-only`, no seccomp profile, no
   `--security-opt=no-new-privileges`. Only the `--network none` and `--internal` half of
   that design is real and measured. Confirmed by grep. Cause definitive: the container task
   implemented the network isolation ADR-004 describes and not the process isolation.
   Recorded as the control matrix's first deficiency; **an ADR-004 conformance gap for the
   lead**, not a bug in this round's code.
3. **The plugin binary never calls `av_edge::identity`** — zero references in
   `crates/av-ingest-client/src/bin/av-edge-plugin.rs`, so the track's own identity
   verification is unused by the binary that would need it in a real deployment. Cause
   definitive (the binary was written for the plaintext loopback path). Recorded as a
   control-matrix deficiency.
4. **E6's `EdgeBuffer::replay_from` digest-mismatch path had no test.** Found in review of
   this round's own new code. Investigated rather than patched: the production code was
   already correct — it recomputes each record's hash before attempting to decode, so a
   record tampered with mid-file is surfaced as a hard `DigestMismatch` and never silently
   skipped or truncated into a shorter "good" prefix. Nothing was fixed; the behaviour is
   now pinned by a test (`cb8f44c`). Cause definitive: a coverage hole, not a bug.
5. **The cross-track Docker prune collision** described under the gates. Cause definitive.
6. **Two counting artifacts caught before they became claims**, both worth recording
   because each would have been invisible: a `grep -c "#\[test\]"` over the codec module
   reported 31 where only 30 tests exist (the 31st match is the literal text `#[test]`
   inside a doc comment), and a naive before/after test-name diff reported 30 kernel tests
   "missing" that had merely lost their `codec::` module prefix. Both were resolved against
   `cargo test -- --list` as the authority rather than against a grep.

### Open items for the lead

1. **`cargo deny check` exits non-zero on this workspace and is meant to.** Question 205's
   fourth `altavista-upstream` proposal — `publish = false` on the six spoore crates — is
   now the only fix, and until it lands every gate on every branch will show
   `bans FAILED`. If the lead would rather the gate be green in the interim, that is a
   decision to take explicitly; this round refused to take it silently.
2. **Question 156's prune-by-label is not safe across worktrees.** Two tracks running
   docker-gated tests concurrently delete each other's labelled containers, because the
   guarding mutex is process-local while the prune is daemon-wide. A file lock under a
   shared path would fix it. Until then, cross-track docker gates must be serialised by
   hand, and a docker failure in a contended gate should be re-run alone before it is
   believed.
3. **ADR-004's plugin container hardening is unimplemented** (defect 2). Either the
   Dockerfile grows the rootless/UBI9-FIPS/read-only/seccomp posture ADR-004 states, or
   ADR-004 is amended to say what is actually built. The control matrix currently marks the
   gap honestly, which is the least the platform should accept.
4. **The track config hash is recorded in this document and asserted by nothing.** It moved
   this round for a legitimate reason and nothing would have caught it if it had moved for
   an illegitimate one. The same is true of E5's accuracy numbers. A test pinning both is a
   small task.
5. **`earth_fixed_demo_frame` still is not in any `scenario.frames` registry** (decision
   8). The proper fix regenerates the GMAT fixtures and repins E4's goldens; it is a
   platform task with a real cost, and the adapter now works either way.
6. **The plugin binary's identity path** (defect 3) — the plugin cannot present a
   seccert-issued identity today because its binary never verifies or presents one. E2 and
   E3b both work; the binary that would tie them together does not call either.
7. **Every milestone in this plan is now delivered.** E1 and E2 (round 1), E3a (round 1),
   E3b, E4 and E5 (round 2), E6 (this round), plus all four of question 205's items. This
   plan has no remaining milestone, and the track's next round needs a new charter from the
   lead rather than a continuation of this one.
8. **No latency number was taken this round** and none should be believed from a shared
   host. If question 43's budget line is to be tightened, it needs a quiet machine.



## Status (edge manager, 2026-09-13) — round 4 (consolidation), PAUSED

Round 4 is a consolidation round: every milestone in this plan was delivered and accepted in
rounds 1-3, and question 207 set this round's list. **The round was paused by the user before
its end-of-round gate sweep.** Seven commits on `edge`, every one of them reviewed from its
artifacts (not from a worker's claims) with workspace clippy re-run by the manager after each:

- `34a3e7c` **Question 204** — every `sha2` use migrated to `openssl::sha::sha256`; `sha2`
  removed from the three crates and from `[workspace.dependencies]`, and banned in
  `deny.toml`.
- `fbd2454` **Question 207's daemon-wide lock** — `flock(2)` on
  `$HOME/.altavista/locks/docker-tests.lock`, in Rust (`av_lockstep::docker::
  lock_docker_tests`) and Python (`altavista.docker_test_lock`), taken by every docker-gated
  test for its whole body; `prune_stale_test_resources(&DockerTestLock)` makes holding it a
  compile-time requirement.
- `543bb29` **The same lock, made visible** — a blocked acquire announces itself on stderr
  with the path, the question and the measured wait, instead of stalling silently.
- `5b05309` **Round 3's defect 3 closed** — the plugin binary verifies its own seccert leaf
  (`--trust-anchor`, `--now-tai-ns`) and presents it to the ingest; a wrong-CA leaf is
  refused and counted end to end; `--buffer-dir` gives the plugin E6-backed durable state.
- `235c446` **Round 3's defect 2 closed as far as this substrate allows** — the plugin
  container's non-root user, read-only root with one declared writable volume, `--cap-drop
  ALL`, `--security-opt no-new-privileges` and the default seccomp profile, asserted from
  `docker inspect` and from `/proc/1/status` inside a running container; ADR-004 gains
  question 207's substrate clarification.
- `b067e54` **Round 3's open item 4 closed** — the track config hash and E5's three accuracy
  statistics are pinned by a test with a negative control; `earth_fixed_demo_frame` is
  registered in the demo DRM's `scenario.frames`.
- `2885b15` **Question 200's second plugin** — ADS-B replay from a committed synthetic CSV
  through the same plugin library and batch path, with its batch count and chain head pinned.

### Counts, measured per task on this host (NOT an end-of-round gate sweep)

| Gate | Baseline at 7e5a252 | Latest measured | Where |
|---|---|---|---|
| `cargo test --workspace --exclude av-kernel --no-fail-fast` | 701 passed, 0 failed, 2 ignored | **734 passed, 0 failed, 2 ignored** | at `2885b15` |
| `cargo test -p av-kernel --no-fail-fast` | 872 passed, 0 failed, 2 ignored, 4 visible skips | **873 passed, 0 failed, 2 ignored, 4 visible skips** | at `2885b15` |
| `.venv/bin/python -m pytest -q -rs` | 513 passed, 4 skipped (round 3) | **516 passed, 5 skipped** | manager's own run at `b067e54` |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean | **clean, exit 0, zero warnings** | manager, after every commit incl. `2885b15` |
| `cargo deny check` | four `ok`, six spoore wildcard warnings | **advisories ok, bans ok, licenses ok, sources ok**, six wildcard warnings | manager at `5b05309`, worker at `2885b15` |
| `cargo tree -e normal --workspace \| grep -c sha2` | 3 | **0** | manager at `2885b15` |
| `buf breaking` | n/a | **not run — no proto file changed this round** (`git diff --name-status develop -- proto/` is empty) | — |

The workspace figure reconciles by name: 701 + 1 (task 1's `settings_hash` oracle) + 2 (the
lock tests) + 8 (the plugin identity and buffer tests) + 1 (the pinned config-hash/accuracy
test) + 22 (ADS-B: 11 unit, 7 replay, 4 frame-resolution) − 1 (one identity test that
replaced an existing one) = 734. The kernel's +1 is the ADS-B ECEF cross-check
(`crates/av-kernel/tests/edge_plugin_adsb_ecef_crosscheck.rs`), placed there so `av-edge`
stays GMAT-free — `cargo tree -p av-edge --all-targets | grep -iE "gmat|tonic"` is still
empty. Python's 516/5 is 513 + the docker-lock cross-process tests + the new alpine
hardening test, with the fifth skip being that test's own visible ENOSPC skip (below).

**The end-of-round gate sweep was not run.** The round was paused, and at the pause the
AI-plane track was running its own `pytest` gate in `/Users/probe/code/AltaVista-aiplane`;
a gate run against a contended host is not a gate (the standing rule). Every commit above is
individually covered by the counts in this table, and the working tree is clean.

### Numbers traceable to artifacts

**Question 204's migration moved no recorded hash.** The three digests
`crates/av-kernel/tests/drm_executor.rs` pins by hand for `leo_1day_golden`
(`bf49e03b…`, `ab9f1233…`, `4694f388…`) are unchanged, and all 75 `drms/*.yaml` declared
`hash:` fields still verify at load through `verify_drm_hash`/`verify_sos_hash`/
`verify_system_hash` — which is what makes 872 unchanged kernel tests the real proof, not
the exit code. `av-dynamics::settings_hash` gained an external oracle test:
`{"a":"1","b":"2"}` encodes to `b"a=1\nb=2\n"` and the system `shasum -a 256` over exactly
those bytes prints
`4a73850fde34aad40ff8649b93a66523a5fe744357a3931caea0f10609d0d930`, which the test pins.

**Question 204 named the wrong third crate, and this is worth the lead's attention.**
`gmat-sys` has never had a `sha2` dependency or any SHA-256 code at all. The three real
dependents were `av-dynamics`, `av-kernel` and **`av-sweep`** — the feasibility track's
crate, created after question 204 was framed. `av-sweep` was migrated too, because the
`deny.toml` ban is mechanical and `cargo deny check` would otherwise fail for the feasibility
track on every branch. Three `av-kernel` dev-only files (`tests/replay.rs`,
`tests/port_traffic_sidecar.rs`, `examples/gen_expr_goldens.rs`) imported `sha2` directly and
were migrated with them.

**The lock is measured, not assumed.** `flock(2)` on separate open file descriptions
serialises two threads of one process on this host (thread B's `LOCK_EX|LOCK_NB` returned
`-1`, errno 35, while thread A held it, and `0` after release), and across processes AND
languages (a Rust guard held while a `python3` child observed `BLOCKED`, then `ACQUIRED`
once dropped) — so the pre-existing process-local `DOCKER_TEST_LOCK` mutex was removed
rather than kept alongside. A real observed wait, from the crate's own test run:
`ACQUIRED the docker-test lock (/Users/probe/.altavista/locks/docker-tests.lock) after
waiting 30.368439s`. Consequence, stated because it is a real cost: `cargo test -p
av-lockstep` went from ~33 s to ~104 s, because two of its test binaries now correctly
serialise on a host-wide lock instead of racing.

**The container posture is asserted from the kernel's own view, not from the script's
intent.** On this host today, with the identical flags the plugin image expects:
`Config.User='10001:10001'`, `HostConfig.ReadonlyRootfs=True`, `HostConfig.CapDrop=['ALL']`,
`HostConfig.SecurityOpt=['no-new-privileges']`, and from inside the running container
`{"uid": 10001, "no_new_privs": "1", "seccomp": "2", "root_write_failed": true}` —
`Seccomp: 2` being filter mode, which is how the DEFAULT seccomp profile is proven applied
rather than inferred from the absence of a flag.

**E5's pinned numbers, re-measured before being pinned** (question 148: measure first, pin
second). `TrackConfig::config_hash()` is still
`7a2d33df306ae9861161e1c64eb2cf79c67e7657e419b93fa9365105a53bb06b` — no drift since round 3 —
and is now asserted exactly. The three error statistics came back bit-identical across two
separate process runs: `max 0.0018749988892797183 m`, `p50 5.587935447692871e-9 m`,
`p99 8.517093334593117e-5 m`, 900 matched, 0 unmatched. They are pinned as BANDS, with a
lower bound as well as an upper one, because an upper-bound-only assertion cannot see the
regression class where the comparison quietly stops doing real work and reports an
implausibly small error. The negative control is recorded: perturbing the expected value
produces `max_error_m out of its pinned band (0.0001, 0.001) m: measured
0.0018749988892797183 m`.

**The ADS-B plugin's pinned numbers.** 45 measurements in 15 batches from a 47-line committed
synthetic CSV (`crates/av-edge/tests/fixtures/adsb/sample.csv`, SHA-256
`c116bc4113f8914d4c54a7670164945967223a7e213cd9a8b551a42bf1fede68`, one row deliberately
malformed so the typed refusal path is exercised by a real fixture row). Chain head
`8f6bd8ce345cf4155b60f0126727c2e0ff2c4a8c8c198c3545d9eafa58158668`; `PluginConfig::
config_hash()` `a550bf530c860876ed51f371221b0092770ab01572b07a8819ec01d2e26977d5`. The
WGS-84 geodetic-to-ECEF transform `av-edge` implements agrees with
`av_kernel::drm::ground::geodetic_to_ecef_m` to a **worst deviation of 0e0 m** over every
fixture row. UTC timestamps convert through `av_cdm::time`'s leap-second table, never a
hand-rolled offset; the frame resolves through the registry (Earth origin,
`AXES_KIND_BODY_FIXED`) to `spoore_cdm::Frame::Ecef` rather than a hard-coded literal.

### Question 156's amendment (the lead copies this to open-questions)

> `av_lockstep::docker::prune_stale_test_resources()` deletes every Docker container and
> image labelled `av.test` daemon-wide, and its only lock was a process-local `Mutex<()>`
> invisible to any other worktree's test process — round 3's own gate measured this directly:
> the prune test failed with a connection-refused error while a concurrent `cargo test -p
> av-kernel` in another track tore out its containers, and passed when re-run alone. The fix
> is a host-wide `flock(2)` lock at `$HOME/.altavista/locks/docker-tests.lock` (never `/tmp`
> or `/private/var`, since Colima mounts only `$HOME` and macOS reclaims those paths),
> implemented identically in Rust (`av_lockstep::docker::lock_docker_tests`) and Python
> (`altavista.docker_test_lock.lock_docker_tests`), each acquired for a docker-gated test's
> whole body. `prune_stale_test_resources` now takes `&DockerTestLock` as a parameter, making
> "the caller already holds the lock" a compile-time requirement rather than a convention —
> deliberately not acquired internally, since `flock` on a second descriptor in the same
> process would deadlock a caller already holding it. A blocked acquire prints a `WAITING`
> line naming the path and the question, and an `ACQUIRED … after waiting <duration>` line,
> so a serialised gate never looks like a hang. What is still NOT covered:
> `services/edge-plugin/build-image.sh` prunes by its own component label and takes no lock,
> and `services/cfs/build-image.sh` takes none either (it prunes nothing) — both remain
> exposed to cross-worktree contention. If a docker gate still fails under contention, check
> `ps` for a concurrent `cargo test`/`pytest`/build script first; if none is running, treat it
> as a genuine failure, because the lock serialises AltaVista's own test suites and not those
> two scripts.

### Decisions taken this round (for the lead to ratify or overturn)

1. **`av-sweep` was migrated off `sha2` although question 204 did not name it**, because the
   ban is mechanical and the alternative was a `cargo deny` failure the feasibility track
   could not act on. The edit is confined to its two hashing modules and changes no digest.
   The lead may want to tell that track it happened; nothing in their worktree was touched.
2. **`libc = "0.2"` is a new `[workspace.dependencies]` entry**, used only by the docker-test
   lock for `flock`. It was already in `Cargo.lock` transitively, is MIT/Apache-2.0, and is
   not crypto-adjacent. A `Drop`-based or PID-file lock was rejected explicitly: neither
   survives `SIGKILL`, which is the exact failure mode question 156 exists for.
3. **`prune_stale_test_resources` takes the lock as a parameter rather than acquiring it
   internally**, so holding it is checked by the compiler and re-entrant `flock` deadlock is
   impossible by construction.
4. **`crates/av-kernel/tests/drm_attitude_control_cfs.rs`'s four docker-gated tests were
   deliberately left without the lock.** They neither prune by label nor label their own
   containers, so they are not exposed to question 207's race. That they do not label is
   itself a question 156 gap in another team's test — recorded as a defect below, not fixed
   here, because the tests skip on this host (no cFS image) and a fix I cannot run is a fix I
   cannot verify.
5. **The plugin verifies its own leaf before connecting, and also presents it on the wire.**
   Local verification refuses an untrusted identity before any socket is opened; the leaf is
   still forwarded as `x-ssl-client-escaped-cert` on the plaintext path so `av-ingest` — the
   enforcement point under questions 202/205(1) — verifies it independently and counts the
   result. On the `https://` path nginx sets that header itself, so the binary does not.
6. **The verified fingerprint overrides `PluginConfig.leaf_fingerprint_sha256`**, and a
   config that declares a different one is a hard error naming both values, so the manifest,
   every batch's `signer_cert_sha256` and the certificate cannot drift apart.
7. **`--buffer-dir` routes batches through E6's `EdgeBuffer`/`UplinkDriver`**, which gives the
   container a real reason to have exactly one writable path. Without the flag, behaviour is
   byte-for-byte what it was.
8. **`earth_fixed_demo_frame` IS now registered in `drms/demo_ground_segment.drm.yaml`'s
   `scenario.frames`, overturning round 3's decision 8** — but only after tracing the
   constraint instead of restating it. Every reader of the embedded DRM hash was enumerated:
   nothing in `av-edge`, `av-ingest` or `av-track` ever re-verifies
   `RunProducts.provenance.config_hash` against a freshly computed DRM hash (only `run_id`
   and `created_tai_ns` are read back), and E4's pinned chain head is built from
   `PluginConfig`, batch content and the run id, never from the DRM's hash. The DRM's own
   `hash:` moved from `7a5944b319fa…412d0` to
   `c736e887531cded74bb95de2b319011e7285a148bed9e38f768cc19ab369e2e2` and the kernel's loader
   verifies the new value: 872 kernel tests and 516 Python tests still pass, unchanged.
   **The disclosed cost:** the committed fixtures still record the OLD hash as their
   generation provenance, so that value now names a DRM that no longer exists in the tree.
   That mismatch is recorded at the place a reader would look
   (`crates/av-edge/tests/fixtures/ground_segment/README.md`). If the lead prefers true
   provenance over a registered frame, reverting is two hunks — the DRM and that README —
   and the alternative (regenerating the GMAT fixtures) was not attempted because it would
   repin E4's goldens for a cosmetic gain.
9. **The `TrackConfig` frame declaration was NOT removed** even though the DRM now registers
   the frame: `av-track` has no code path that reads `Scenario.frames` or `RunProducts.frames`
   at all. Removing it would have broken the bridge; the two declarations are now documented
   as deliberate, with the reason in `TrackConfig`'s own doc comment.
10. **E5's statistics are pinned as bands with both an upper and a lower bound**, not as exact
    values, although they were measured bit-identical across two runs — `f64`
    non-associativity across a toolchain or platform this repository does not control is a
    real risk, and a band that catches a three-order-of-magnitude regression is worth more
    than an exact assertion that breaks on a compiler upgrade.
11. **The ADS-B plugin is a source and a batch path, not a binary and not a container.**
    Question 200(a) and question 207 ask for the second plugin's measurements, frame, label
    and pinned chain; a second binary and a second image are follow-on work, and the image
    could not be built on this host anyway.
12. **ADS-B is labelled `UNCLASSIFIED` with a declared (25 m)² isotropic position noise.**
    1090ES is an unencrypted public broadcast; labelling it as if it were sensitive would be
    a wrong answer wearing a right answer's clothes. The noise figure is declared, not
    derived from the fixture.
13. **Task 3 was split into two commits** (the binary's identity path, then the container),
    because the image bakes the binary: doing the binary first meant the image would have
    needed exactly one rebuild — which, as it turned out, the host could not give.

### Defects found in review this round

1. **The docker-test lock blocked silently** (found in review of this round's own new code).
   A contended gate was indistinguishable from a hang, which is the class of problem
   questions 148 and 194 exist to prevent. Cause definitive: `flock(LOCK_EX)` was called
   blocking, with no non-blocking first attempt and no output. Fixed in `543bb29`.
2. **`crates/av-kernel/tests/drm_attitude_control_cfs.rs` labels none of the containers it
   creates** (it runs a throwaway `registry:2` and tags images) and calls no prune. Cause
   definitive, confirmed by grep: it predates question 156's labelling convention and was
   never retrofitted. Consequence: a killed run leaks a running registry container — question
   156's original finding, verbatim. Not this track's file and unverifiable on this host
   (its tests skip, no cFS image). **For the lead.**
3. **A fresh named Docker volume mounted into a non-root container is owned by root**, so the
   hardened container could not write its own state directory. Found by the test failing for
   real. Cause definitive: Docker propagates ownership from the image's own directory only
   when the image pre-creates it — which the plugin Dockerfile now does, and which a bare
   named volume in a synthetic test does not. Fixed by priming ownership in the test.
4. **Question 204's own premise was wrong about which crates hash with `sha2`** (defect 1 of
   the round, in the ruling rather than the code). Cause definitive: `av-sweep` was created
   after the question was written, and `gmat-sys` never had the dependency it was credited
   with.
5. **Two workers ended their turns waiting on background jobs**, despite briefs that forbid
   it in bold. Both tasks were reviewed from their artifacts and committed by the manager
   (`5b05309`, `b067e54`). This is the same failure question 206 already recorded for the
   AI-plane track; the brief wording is evidently not sufficient on its own.
6. **A diagnostic `docker pull alpine:latest` was run** while measuring disk state — by a
   worker, and possibly by the manager's own `docker run --rm alpine` probe before it. Not at
   test time, so question 154's rule is not broken in the sense it was written, but it is a
   network call this round did not plan and should not have made. The tests themselves check
   image presence with `docker image inspect` and never pull.

### Question 196(d): the disk is now full, and the plugin image cannot be rebuilt

Measured this round: the Colima VM's container filesystem is **58.8 GB, 56.0 GB used, 0 bytes
available — 100% full**. `docker system df` reports 899-904 local volumes holding 46.9-47.2 GB
at 99% reclaimable, owned by the unrelated Supabase/Kubernetes workload question 205 already
named. `av-edge-plugin:local` and its `debian:bookworm-slim` base have been garbage-collected
again, and `alpine:latest` was observed being evicted between two checks minutes apart with no
explicit prune — real-time confirmation of the kubelet image collector question 205 closed this
on. Consequences, all recorded rather than worked around:

- `services/edge-plugin/build-image.sh` was NOT run; the plugin container test skips visibly
  with its existing reason, and nothing in this round claims the plugin container was measured.
- The hardening flags are proven instead against `alpine:latest` in
  `tests/test_edge_plugin_hardening_alpine.py`, with the identical flags and the identical
  assertions, so the plugin test's assertions are known-good the day the image can be built.
- Even a write to a **named volume** now fails with ENOSPC, so the one posture fact that could
  not be proven today is "the writable volume is writable". That test distinguishes the
  environmental block from a hardening failure and skips visibly rather than reporting either
  a false failure or a silent pass.
- The remedy remains the user's and is unchanged: reclaim those volumes, disable Colima's
  Kubernetes, or raise `disk:`.

### What remains when the round resumes

1. **Task 6 (E5's latency retake) was never eligible and was not attempted.** The host was
   never quiet: the AI-plane track ran cargo and pytest gates throughout, and at the pause it
   was running its own `pytest` suite. Round 2's `p50 ≈ 6.1 ms / p99 ≈ 8.3 ms` stands,
   unretested, and should not be believed from a shared host.
2. **The end-of-round gate sweep** (`cargo test --workspace --exclude av-kernel`,
   `cargo test -p av-kernel`, clippy, `cargo deny check`, full `pytest -q -rs`, and the
   `sha2` tree count) with nothing else active on the host. Every commit is covered
   individually by the table above; the single clean sweep is not done.
3. **A second plugin binary and image for ADS-B**, if the lead wants the second plugin to run
   as a container the way the first does.
4. **The two build scripts still take no docker lock** (the amendment says so explicitly).

### Open items for the lead

1. **Decision 8 is the one that most deserves a ratify-or-overturn**: `earth_fixed_demo_frame`
   is registered, and the committed fixtures' recorded generation hash now names a DRM that no
   longer exists. Nothing verifies it, the mismatch is documented where a reader will find it,
   and the revert is two hunks.
2. **`av-sweep` was edited by this track** (decision 1). The feasibility track should know.
3. **`drm_attitude_control_cfs.rs` violates question 156** (defect 2) and is another team's
   file.
4. **`deny.toml` still carries `wildcards = "warn"`** with question 207 cited, pending the
   spoore `publish = false` commit landing on its `main`. Unchanged this round; `cargo deny
   check` is green with exactly the six accepted warnings.
5. **`sha2` is now banned**, so any track adding a new hashing call site must use
   `openssl::sha::sha256`. Three crates each carry their own six-line `hex_encode` helper now
   (as `av-command` and `av-dynamics-service` already did); a shared one would be a small,
   separate cleanup.
6. **The disk is full** (question 196(d)) and no image on this host can be rebuilt until the
   user acts. Every image-gated test on every track is skipping.
