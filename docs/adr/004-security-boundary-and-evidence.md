# ADR-004: The security boundary, identity, isolation and evidence

- **Status:** Accepted (drafted 2026-09-02; accepted by the user 2026-09-02)
- **Date:** 2026-09-02
- **Plan reference:** [architecture.md](../architecture.md) section 4 (edge, AI plane, SecRouter alignment); questions 30–36, 52–55, 62–64, 66–70
- **Implemented by:** `crates/av-ingest` (edge node), `plugins/`, `crates/av-command`, every component's `docs/compliance/control-matrix.md` and `/admin/api/evidence`, CI checks (`crypto-rule`, `egress-blocked`)
- **Depends on:** [ADR-001](001-cdm-v1.md) (envelope, signed batches, labels, command), [ADR-003](003-substrate-and-deployment.md)

## Context

The requirement reads "defense-grade cybersecurity boundaries directly at the edge",
"isolated containers safely host custom machine learning models", and "secure, read-only
data access with bidirectional command routing to edge systems". A slogan cannot be tested;
a control set can. The user set the target at **CMMC Level 2 (NIST SP 800-171 R2)** with the
development environment in scope (question 31), and pointed at the SecRouter suite, which
already carries a control-matrix format, an evidence pipeline, an internal ACME CA, an OIDC
identity provider, FIPS rules and a field test against NASA cFS.

## Decision

### Identity and trust

- **Machine identity from `seccert`** (question 34): every service and edge plugin runs a
  standard ACME client against the suite's internal CA; leaves are short-lived (90 days
  default), the two-tier Root is the enclave trust anchor, and mTLS between services uses
  those certificates. SPIFFE/SPIRE is retired.
- **Human identity from `secsso`** or the customer's IdP through OIDC, with the `groups`
  claim for roles and `amr`/`acr` for MFA, on the viewer, consoles and command
  authorization. Machine clients that must authenticate as principals use OIDC service
  subjects, one per integration.

### The edge boundary

- **Plugins are untrusted code.** A plugin is a rootless podman container (Quadlet-managed)
  on a UBI9 FIPS base image, `--network none` plus an explicit allow-list of its asset
  endpoint and the local edge log, read-only root, dedicated user, seccomp, no new
  privileges. Platform services are hardened systemd units with the same properties
  (question 62). gVisor and Kata are not used: there is no orchestrator, and gVisor's user-space
  network stack bypasses the host FIPS OpenSSL.
- **Signed, chained batches** (question 36): the edge signs each batch with ECDSA P-384
  under its seccert-issued key and chains batch hashes per producer; the log stores
  signatures and hashes; verify endpoints walk the chain, matching the suite's audit-ledger
  pattern.
- **Labels on every message; one handling level per deployment** (question 32). A plugin
  declares the label it emits under and cannot emit above its clearance; the tile gateway
  and data gateway enforce labels per layer and per query; no cross-domain replication is
  built.
- **Capture-only when disconnected** (question 35): the edge buffers hours of signed batches
  and replays them in order on reconnect; no tracking or authority runs at the edge.
- **Everything rejected is counted.** Unsigned, mislabeled, over-clearance and stale
  messages are dropped and counted per plugin, never silent, in spoore's style.

### The AI plane

- **Numeric model sidecars** (spoore `ModelService`) are podman containers under the same
  rules, placed on the inference tier for GPUs (question 52); CPU-only ONNX by default, CUDA
  opt-in per model with its libraries in the SBOM (no cryptographic role).
- **Language models run through `secrouter`** (question 64): local models on `secllm` by
  default, GovCloud tiers opt-in behind secrouter's classification gate (question 68). The
  platform's read-only data access is an MCP server on secrouter's deny-by-default tool
  allow-list; `secagent` is the agent harness.
- **Models and agents may only propose.** `CommandProposal` is an MCP tool that creates the
  `PROPOSED` state and nothing else. Propose-only until an envelope policy exists; enabling
  an envelope is a two-person policy change and an audit event (question 53).
- **The design assistant** (questions 66–70) may draft every artifact type, including
  profiles and authority policies; a human accepts the diff before anything runs or deploys,
  with two reviewers for profiles and policies; dry-runs and read-only queries are free;
  narratives are stored as artifacts with provenance citing run ids and hashes.
- **Training data** comes only from the separate training store (ADR-003).

### Command authority

The command state machine of ADR-001 is enforced by `av-command`: OPA policy at `CHECKED`
(label, asset, envelope, rate); role-gated human authorization per command class,
time-limited delegations with enforced expiry, retention and SIEM export of every transition
(question 54); deadline, idempotency key and explicit ack levels on dispatch (question 55).
Every transition is an event on the log, so a replay reproduces the decision trail including
what the model saw.

### Evidence, verbatim from P0

Every component (question 63):

- ships `docs/compliance/control-matrix.md` in the `secrouter` format: family, NIST 800-171
  R2 ID, requirement, implementation `file:function`, evidence command, with the
  Met / Partial / Inherited / Gap legend and a deficiency list;
- exposes `/admin/api/evidence` for `secdeploy evidence` to collect;
- keeps hash-chained audit ledgers with a `verify` endpoint; for the engine and command
  services the durable log itself is the ledger, chained per partition;
- forwards audit events to syslog/SIEM when `[audit]` is configured;
- follows the **crypto rule**: SHA-256 only (no MD5, SHA-1, BLAKE2), the system FIPS OpenSSL
  only, no bundled crypto. In Rust this excludes `ring`-based TLS; OpenSSL-backed providers
  (`rustls-openssl`, `native-tls`, `openssl`) are used, and a CI check fails the build on a
  forbidden dependency or hash. Node components run with `--enable-fips`; Python components
  use the system OpenSSL through `httpx`/`cryptography` on FIPS hosts.

CI also runs the viewer and every service with egress blocked, proving the fully
self-contained build (question 51).

## Alternatives considered

**SPIFFE/SPIRE for workload identity.** Finer per-process identity and automatic rotation.
Rejected because the suite already runs an ACME CA, and a second identity system is a
second thing to accredit; ACME clients with short-lived certificates give the rotation.

**gVisor/Kata for plugins.** The first draft, chosen when Kubernetes was assumed. Rejected
with the orchestrator; the FIPS bypass in gVisor's netstack is independent of that and
would have been disqualifying anyway.

**Hardened units only, no containers.** Tightest crypto boundary; rejected for plugins and
models because third-party model code brings its own filesystem and dependency graph, which
a container isolates and a unit does not.

**MicroVMs (Kata/Firecracker) for untrusted code.** Strongest isolation; rejected for now on
GPU passthrough and packaging cost on fedora-fips; recorded as the escalation path if a
plugin or model is ever judged hostile rather than merely untrusted.

**Per-message signatures.** Strongest provenance; rejected for CPU cost at high rates and
message size. **mTLS only.** Simplest; rejected because provenance would not survive export
or replay.

**Multi-level handling in one deployment.** Rejected by the user: single level per
deployment; labels are still carried so a future multi-level deployment changes enforcement,
not schema.

**Delegated command envelopes from the start.** Rejected in favour of propose-only until an
envelope policy exists; the schema supports envelopes so the change is policy, not code.

**Direct training on operational stores.** Rejected in favour of the separate training store.

## Consequences

**Every crate and service carries compliance artifacts from its first commit.** A control
matrix with mostly `Gap` rows is acceptable in P0; a component without one is not.

**The Rust dependency tree is policed.** `cargo deny` (or an equivalent check) bans `ring`,
`md-5`, `sha1`, `blake2` and any TLS crate that does not sit on OpenSSL; the SBOM per
component records what remains.

**Plugins have a contract, not just an image.** The signed manifest declares schemas,
frames, label, endpoints and interfaces; the edge node refuses a plugin whose manifest and
image digest disagree with the deploy audit.

**Air-gapped CI is required.** The egress-blocked builds and the FIPS checks only mean
something on runners that are themselves inside the boundary (question 58).

**The assistant's freedom is bounded by the review gate, not by its tools.** It may call
any read-only tool and any dry-run freely; the platform's guarantee is that nothing runs or
deploys from a draft a human has not accepted, and that acceptance is an audit event.

## What would falsify this

The premise is that the suite's conventions, applied verbatim, are sufficient evidence for
CMMC Level 2 at the platform's scale, and that per-batch signing and podman isolation cost
less than the guarantees they buy.

The observables: a C3PAO-style review that finds a control the matrix cannot map to a
`file:function` because it lives in an operational procedure the platform does not
implement (the shared-responsibility table would then need a platform feature, not a
document); per-batch signing measured above the edge's latency line item at 1k
messages/s; a plugin that needs a device or network capability podman's rootless mode
cannot grant (which would move it to a hardened unit or a microVM); or an evidence bundle
that `secdeploy evidence` cannot assemble because a component's endpoint diverged from the
suite's contract.

## References

- `github.com/secrouter/secrouter` `docs/compliance/cmmc-control-matrix.md` and `deployment-hardening.md`; `secagent/docs/cmmc.md` (legend, deficiency list) and `fips.md` (crypto rules, UBI9, `--enable-fips`); `seccert/docs/security.md` (key handling, hash-chained ledger, control mapping); `secsso` README (OIDC, groups, MFA, service subjects); `secdeploy/docs/compliance.md` (deploy-audit chain, `secdeploy evidence`).
- NIST SP 800-171 Rev. 2; CMMC Model v2.0 Level 2; NIST SP 800-172 (for the enhancements the suite already maps).
- FIPS 140-3; NIST CMVP validated modules (OpenSSL FIPS provider).
- OCI runtime spec; Podman Quadlet documentation; systemd `ExecStart` hardening directives (`ProtectSystem`, `NoNewPrivileges`, `SystemCallFilter`).
- RFC 8555 (ACME); RFC 6979 / FIPS 186-5 (ECDSA); OpenID Connect Core 1.0.
- Open Policy Agent documentation.
