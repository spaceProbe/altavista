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
