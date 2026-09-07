# ADR-003: Substrate and deployment

- **Status:** Accepted (drafted 2026-09-02; accepted by the user 2026-09-02)
- **Date:** 2026-09-02
- **Plan reference:** [architecture.md](../architecture.md) section 4 and the SecRouter-suite alignment; questions 37–43, 56, 61, 65, 71
- **Implemented by:** `crates/av-node`, `crates/av-io` (rskafka behind spoore-io's traits), `deploy/` (suite tier definitions, systemd units, data packs), `training/`
- **Depends on:** [ADR-000](000-scope-and-lineage.md), [ADR-001](001-cdm-v1.md); spoore ADR-003 (substrate), spoore ADR-004 (determinism)

## Context

spoore chose Redpanda (Kafka API), gRPC, ClickHouse and protobuf, on the argument that the
engine's correctness depends on a durable, ordered, replayable, partitioned log, and that
air-gap installability binds harder than any other constraint. It deploys through a Helm
chart on Kubernetes and packages an air-gap kit. Its ADR-003 left Redpanda's Business
Source License as an open legal question.

The user's SecRouter suite (`github.com/secrouter`) then changed the deployment premise. Its
production target is **native, hardened systemd services on a Fedora host in FIPS mode**,
with no containers, so that every component links the host's OpenSSL FIPS provider directly;
`secdeploy` pins a suite bill of materials (`suite.toml`), places tiers on hosts
(`secsite.toml`), refuses to deploy unless the host is FIPS-ready, writes a hash-chained
deploy audit, and collects compliance evidence from every component. The user also asked
for an evaluation of `influxdata/rskafka` as the log client, and for a secondary database
built explicitly for training.

## Decision

### The log

**Redpanda stays as the broker, pending legal review** (question 37): a single static binary
is the easiest Kafka-class system to run as a hardened unit on a FIPS host, and the Kafka
API keeps ClickHouse's native ingestion and spoore's replay tooling. Its BSL license is an
operational dependency outside the open core; legal confirms the self-hosted, non-service
use before the first customer kit. The coupling is to the Kafka API, not to Redpanda.

**`rskafka` replaces `rdkafka` as the client** (question 37, evaluated 2026-09-02). It is
pure Rust (MIT/Apache-2.0), partition-bound, and has no consumer groups, offset tracking or
transactions, which is exactly spoore ADR-004's discipline: manual assignment, explicit
partitions, nothing commits, replay by explicit offset. Its TLS runs on the host FIPS
OpenSSL through `rustls-openssl` as the `CryptoProvider` (feature
`transport-tls-no-provider`); `rdkafka` by contrast bundles librdkafka and OpenSSL unless
dynamically linked. The change lands behind `spoore-io`'s producer/consumer traits as a
spoore PR; the crate is vendored fork-ready because its maintenance is thin since InfluxDB
stopped using it.

### The stores

- **ClickHouse** for analytics, residuals, measures of effectiveness and the assistant's
  narratives, ingesting from the log natively (question 40). Its TLS build must use the
  host OpenSSL (ADR-004's crypto rule).
- **MinIO** (S3 API) for the heavy track: gigabytes to a few terabytes on a single node; a
  customer S3 or GovCloud bucket is a drop-in (question 39). AGPL, unmodified, outside the
  open core.
- **Postgres (+PostGIS)** for the entity and asset catalog.
- **A separate training store** (questions 56, 71): Parquet datasets in a dedicated,
  separately labeled bucket, with a Postgres dataset catalog recording lineage, declared
  holdouts and reviews. It is filled only by reviewed export jobs (spoore's deterministic
  dump-training pattern), never read by operational services, and a model version's
  provenance names its dataset hash.

### Latency, lateness and scale

spoore's budgets are the platform's, re-measured: p99 ≤ 25 ms in-process and ≤ 75 ms through
the bus at 1k measurements/s per shard, with the edge-to-core hop as its own line item
(question 41). Lateness windows are per sensor in the system definition (seconds for radar,
minutes for RPO sensing, hours for orbit-determination observations), never in a profile
(question 43). The execution profile is sized for tens to hundreds of orbital objects, one
shard per orbital regime; catalog-scale ingestion is a later plugin (question 42).

### Deployment: SecRouter-suite tiers via secdeploy

**AltaVista ships as tiers of the SecRouter suite** (question 61): `log` (Redpanda),
`analytics` (ClickHouse), `store` (MinIO), `catalog` (Postgres), `engine` (tracking nodes),
`kernel` (simulation), `design` (GMAT service, replay, tiler, scene service), `edge` (edge
nodes with plugins), `training`. Each is declared in `suite.toml`, placed by `secsite.toml`,
and deployed by `secdeploy` as native hardened systemd units on `fedora-fips`, with the
fail-closed FIPS preflight and the deploy-audit chain; the macOS target is the evaluation
environment. Kubernetes is a later, secondary target. Scale-out is by placing tiers across
resources, not by pod scheduling; spoore's Helm chart is not reused.

The viewer, scene service and tile gateway are fronted by `secproxy` at
`https://altavista.<domain>` with names in `secdns` and one SAN certificate from `seccert`
(question 65); `secdeploy`'s wiring gains per-component nginx options (buffering off, long
read timeouts) for the WebSocket and tile streams. gRPC sidecars and Redpanda are not
fronted, like `secllm`.

Environment data (gravity, ephemerides, SPICE kernels, space weather, EOP, basemaps) ships
as versioned, hashed data packs alongside the air-gap kit; every run records the pack hash
(question 22).

## Alternatives considered

**Apache Kafka (KRaft).** Apache 2.0 end to end; rejected for the JVM on a FIPS host (a
second validated crypto module or a JDK FIPS mode to configure) and the heavier unit.
Remains the substitution if legal rejects Redpanda, which is why the coupling is to the API.

**NATS JetStream.** Lightest air-gap story; rejected as spoore did: no native ClickHouse
ingestion and no Kafka replay tooling.

**A minimal Kafka-protocol log server in Rust.** Only what rskafka uses (produce, fetch,
metadata, create-topic, per-partition segments with fsync). Owns the license and air-gap
story completely; rejected for now as a new component to build, test and accredit, with
ClickHouse's Kafka engine as an unknown against it. Recorded because it is the natural
next step if Redpanda's license fails and Kafka's JVM is unacceptable.

**Kubernetes primary, secdeploy later.** Keeps spoore's chart; rejected because the suite's
FIPS argument (every component links the host OpenSSL) is weakened by container base images
with their own crypto, and because two deployment paths would both need accreditation.

**Filesystem store, no S3 API.** Simplest FIPS story; rejected because it forecloses the
customer-bucket drop-in.

**Training on the analytics store directly.** Faster iteration; rejected by the user in
favour of a separate database with reviewed exports, for provenance and CUI review.

**Global lateness window.** Simplest; rejected because hours of checkpoints on every shard
costs memory and makes replays long, and because it would be a numerical setting outside
the system definition.

## Consequences

**Two spoore PRs**: the rskafka client behind `spoore-io`'s traits, and a systemd unit
packaging beside the Helm chart. Until they land, `av-io` carries the client and `deploy/`
carries the units.

**The crypto rule reaches the client stack.** Every Rust TLS use goes through OpenSSL-backed
providers (`rustls-openssl` or `native-tls`/`openssl`); `ring` is excluded by a CI check
(ADR-004).

**Placement is a site decision, with one hard constraint.** A tier that owns shards
(`engine`, `kernel`) must never share a host with a tier that is allowed to drop under
backpressure (`analytics`), because a determinism-critical actor on a host that swaps loses
its latency budget; `secsite.toml` validation encodes it.

**No orchestrator means no restarts by policy.** Liveness and restarts are systemd's; the
node-boundary counters spoore already publishes (`node_boundary_reinit`, `mht_scan_degraded`)
become the health signals `secdeploy evidence` collects.

**The training store is its own label and its own tier.** Export jobs are the only writers,
and they run on the heavy track with the DRM's label on their input and the training label on
their output.

## What would falsify this

The premise is that a Kafka-API log on native FIPS hosts, without an orchestrator, holds
spoore's latency budgets and the replay guarantees at the platform's scale.

The observables: the re-measured p99 through the bus exceeding 75 ms *because of the bus* at
1k measurements/s per shard on a FIPS host (the rskafka client or the broker's TLS on the
FIPS provider would be the suspects); a ClickHouse Kafka-engine failure against Redpanda at
the platform's rates; or a placement that cannot meet the budget without the process
scheduling an orchestrator would give, which would reopen the Kubernetes target earlier than
planned. Legal rejecting Redpanda's terms forces the substitution regardless.

## Amendment 2026-09-02: fronting gRPC sidecars, and where the dynamics service lives

Team 1's M5.3 measured what this ADR assumed: Python's `grpcio` bundles BoringSSL as
compiled object code and cannot terminate TLS on the host's FIPS OpenSSL, so "gRPC sidecars
are not fronted, like secllm" left a cross-host `DynamicsService` call with no
rule-compliant transport. Two decisions by the user (questions 84, 85):

1. **A gRPC sidecar that crosses a host boundary is fronted by a service-owned nginx**
   built from secproxy's conventions: nginx on the host OpenSSL, `grpc_pass` to the
   service's localhost port, server certificate and required client certificate from the
   seccert chain (mTLS), configuration rendered per service by secdeploy from the template
   in `services/gmat-service/deploy/`. Localhost-only sidecars stay unfronted. Measured:
   a `tonic` client on an OpenSSL-backed TLS stack calls `Describe` through the proxy with
   mTLS and is refused (HTTP 400 after a completed handshake, nginx's documented behaviour
   for `ssl_verify_client on`) without a client certificate.
2. **`DynamicsService` is hosted in Rust** (`tonic` server, OpenSSL-backed, calling GMAT
   through `gmat-sys` at depth 2), so `grpcio` leaves the deployed runtime entirely. The
   Python service remains a design-time tool for solvers, authoring and golden generation,
   and is not part of a production profile. This also retires the "Python in production
   paths" question (question 4) for this service.

The crypto rule is enforced mechanically from here: `cargo-deny` bans `ring`, `md-5`,
`sha1`, `blake2` and non-OpenSSL TLS crates in CI (question 86).

## References

- spoore ADR-003 (substrate) and its 2026-07-18 amendment; ADR-004 (determinism); `crates/spoore-io/src/kafka.rs` module docs (manual assignment, explicit partitions).
- `github.com/secrouter/secdeploy`: `suite.toml`, `secsite.toml.example`, `docs/fedora-fips.md`, `docs/topology.md`, `docs/compliance.md`.
- `github.com/secrouter/secproxy` README (FIPS rationale for nginx on system OpenSSL; not-fronted components).
- `influxdata/rskafka` README and `Cargo.toml` (v0.6.0; `transport-tls-no-provider`); `rustls-openssl` v0.4.1.
- Redpanda Business Source License terms for the version to be vendored (to be read by legal against the shipped version, per spoore ADR-003).

## Clarification 2026-09-05: out-of-process bindings

Question 155. The lockstep protocol (question 107) between the kernel and a bound process's
shim is plaintext only on loopback within one host; a container on the same host is that
host. A bound process on another host is fronted by the service-owned nginx mTLS template
(question 84's amendment) exactly as the dynamics services are, and the kernel refuses a
non-loopback plaintext endpoint at load. No crypto-adjacent crate is added for this; the
system OpenSSL through nginx remains the only TLS termination.
