# av-ingest — NIST SP 800-171 Rev 2 control matrix

`crates/av-ingest` — the E3a ingest library (`docs/edge-plan.md` milestone E3, first half
only: the durable per-partition log, the accept/reject pipeline, the counters, crash
recovery, determinism, and the evidence surface as data). This maps the crate's own code
to CMMC Level 2 (NIST SP 800-171 Rev 2, 110 requirements / 14 families), in the format
`docs/compliance/av-dynamics-service/control-matrix.md` uses (family, requirement ID,
requirement, implementation `file:function`, evidence command, the Met / Partial /
Inherited / Gap legend, a scope section, a deficiency list).

**Location deviation from the plan, noted here per this task's own instruction.**
`docs/edge-plan.md`'s prose says this file lives at `docs/compliance/av-ingest.md`; the
directory-per-component convention already on disk
(`docs/compliance/av-dynamics-service/control-matrix.md`,
`docs/compliance/gmat-service/control-matrix.md`) wins, so this file is
`docs/compliance/av-ingest/control-matrix.md` instead.

```{admonition} Not a certification
This is engineering documentation, not a C3PAO assessment or an attestation. CMMC Level 2
is a property of the accreditation boundary as a whole, not of one crate.
**Most rows below are Gap or Inherited — that is the honest state of a first compliance
pass (ADR-004: "a control matrix with mostly `Gap` rows is acceptable in P0; a component
without one is not"), not a defect in this document.**
```

## Legend

| Mark | Meaning |
|---|---|
| Met | Addressed by this crate's own code, verifiable by the evidence command in that row. |
| Partial | Something real exists but depends on deployment/configuration this crate does not itself control, or covers only part of the requirement. |
| Inherited | Satisfied by the environment/organization (host, network, IdP, enclave), or by a sibling component this crate depends on but does not itself implement. |
| Gap | Not implemented anywhere in this crate today. See [Deficiencies](#deficiencies). |

## Scope

This crate is a **library**, not a service: it exposes no socket, no gRPC service, and no
HTTP endpoint. It is the E3a half of milestone E3 only —

- the durable, file-backed, per-partition hash-chained log that **is** the ledger
  (`src/log.rs::PartitionLog`, ADR-004: "for the engine and command services the durable
  log itself is the ledger, chained per partition");
- the accept/reject pipeline wiring `av_edge::chain::ChainVerifier` and
  `av_edge::identity` verification to appends (`src/ingest.rs::Ingest`);
- the counters (both `av_edge::pb::RejectionCounters`, read through `ChainVerifier`, and
  this crate's own `SHARD_MISMATCH`/identity counters);
- crash recovery, determinism, and the evidence surface as plain data
  (`src/evidence.rs::evidence`/`verify_all`).

**E3b — the gRPC service, the mTLS front, and the plugin manifest handshake — is
deliberately not built in this round** (`docs/edge-plan.md`'s "decisions already taken by
the manager"; open question 155 ruled "no new crypto-adjacent crate" for the missing TLS
server-acceptor gap, with an nginx front as the resolution, mirroring
`crates/av-dynamics-service`'s own precedent — that is the lead's decision to confirm, not
this round's). Every row below that would depend on a wire, a transport identity, or a
network boundary is marked **Gap**, named explicitly in
[Deficiencies](#deficiencies) rather than credited to a layer that does not exist yet.

## 3.1 Access Control (AC)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.1.1 / 3.1.2 | Limit system access to authorized users/processes | Gap | N/A — this crate has no caller-authentication concept at all; `Ingest::submit`'s `Signer::Certificate` path authenticates the *batch's signer* (see IA 3.5.1/3.5.2 below), never the calling process/user invoking this library's own API | N/A |
| 3.1.3 | Control the flow of CUI | Partial | `av_edge::policy::ProducerPolicy`/`ChainVerifier` enforce that a producer's batches carry exactly its declared label and rank, refusing `MISLABELED`/`OVER_CLEARANCE` — but this crate performs no CUI marking/flow control of its own beyond what it inherits from `av_edge` | `cargo test -p av-ingest --test rejections -- mislabeled_batch_is_rejected_and_counted over_clearance_batch_is_rejected_and_counted --nocapture` |
| 3.1.5 | Least privilege | Inherited | The OS user/filesystem permissions this library's caller runs under are a deployment concern this crate's own code does not set | N/A |
| 3.1.12 / 3.1.13 | Control & encrypt remote access | Gap | This crate opens no socket at all (E3a is a library; the wire is E3b, deferred) | N/A |
| 3.1.20 | Control connections to external systems | Inherited | Host firewall/network segmentation; moot for a library with no network code | N/A |
| 3.1.4/3.1.6–3.1.11/3.1.14–3.1.19/3.1.21–3.1.22 | Separation of duties, session lock, MFA-gated remote access, mobile/wireless, public content, etc. | Inherited | Environment/IdP responsibilities; not applicable to a library with no session, network, or publishing concept | N/A |

## 3.3 Audit and Accountability (AU)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.3.1 | Create and retain audit records | Met | Every accepted batch becomes one durable, hash-chained record in its partition's `PartitionLog` (`src/log.rs::PartitionLog::append`) — the log *is* the audit record, not a separate log describing it (ADR-004). Retention is unbounded local-file, no rotation/expiry policy — see Deficiencies | `cargo test -p av-ingest --test partitioning --nocapture` |
| 3.3.2 | Trace actions to individual users/processes | Partial | Every record traces to `MeasurementBatch.producer_id` and, when a certificate was presented, to that leaf's `fingerprint_sha256` (via `av_edge::identity`) — but there is no *human* principal anywhere on this path (see IA 3.5.1/3.5.2) | `cargo test -p av-ingest --test identity_refusal --nocapture` |
| 3.3.4 | Alert on audit logging failure | Partial | `PartitionLog::append`/`open` return a typed `Result<_, LogError>` on any I/O failure — fail-loud to the caller, never a silently-dropped record — but nothing in this crate itself forwards that failure to an operator/SIEM; that is E3b's (or its embedder's) job | N/A (code inspection of `src/log.rs::PartitionLog::append`'s `?` propagation) |
| 3.3.5 / 3.3.6 | Correlate and report audit review | Gap | No correlation tooling beyond `src/evidence.rs::evidence`'s raw counters and `verify_all`'s pass/fail per partition | N/A |
| 3.3.7 | Authoritative, time-synced timestamps | Gap | `MeasurementBatch.batch_tai_ns` is caller-supplied (question 199: no live clock read anywhere in this crate) — there is no live, NTP-synced timestamp of this crate's *own* at all; a `now_tai_ns` the wire/embedder injects is a deployment concern, not this crate's | N/A |
| 3.3.8 | **Protect audit information from unauthorized access/modification** | Met | SHA-256 record-hash chain per partition (`src/log.rs`'s "Record framing"/"Two chains, deliberately" doc), independently re-derived from disk by `PartitionLog::verify`, which detects and reports a tampered record's exact index rather than silently accepting it | `cargo test -p av-ingest --test tamper -- --nocapture` |
| 3.3.9 | Limit audit management to a subset of privileged users | Gap | Nothing gates who may call `PartitionLog::verify`/`evidence::verify_all` — this is a library; any caller of the crate's public API can read or independently re-verify every partition | N/A |

## 3.4 Configuration Management (CM)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.4.1 | Baseline configuration & inventory | Partial | `Cargo.lock` pins every dependency; the repo-root `deny.toml` bans forbidden crypto crates and enforces a license allow-list — no CycloneDX/SPDX SBOM generated for this crate specifically | `cargo tree -p av-ingest` |
| 3.4.2 | Enforce security configuration settings | Met | `ProducerPolicy`'s clearance ladder, emit label/caveats and max batch age are explicit, typed configuration the caller must construct and register per producer (`Ingest::register_producer`) — there is no implicit default policy, so an unconfigured producer is refused (`IngestOutcome::UnknownProducer`) rather than silently accepted under some default | `cargo test -p av-ingest --test rejections --nocapture` |
| 3.4.6 | Least functionality | Met | This crate's public API is exactly: open/append/verify a partition log, submit a batch through the pipeline, and read the evidence surface — nothing else (no gRPC service, no HTTP server, no socket; see the crate's own `lib.rs` module doc for what was deliberately left out of E3a) | `cargo tree -p av-ingest \| grep -Ei 'tonic\|hyper\|axum'` (expect no output) |
| 3.4.7 | Restrict nonessential programs/ports | Met (vacuous) | Zero listeners, zero ports — this crate opens no socket at all | `cargo tree -p av-ingest \| grep -i tokio` (expect no output — no async runtime dependency either) |

## 3.5 Identification and Authentication (IA)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.5.1 / 3.5.2 | Identify and authenticate users/processes | Partial | `Ingest::submit`'s `Signer::Certificate` path authenticates a *batch's signer* against `av_edge::identity::TrustAnchors` (the seccert Root) before the batch is looked at further — but this only identifies the producer that signed a batch, never the process/user calling this library's own API, and `Signer::Key` (E1's own no-certificate path) authenticates nothing at all beyond raw signature validity against a caller-supplied key | `cargo test -p av-ingest --test identity_refusal -- --nocapture` |
| 3.5.3 | MFA for privileged/remote access | Inherited | IdP responsibility; not applicable to a library with no interactive login | N/A |
| 3.5.10 | Cryptographically-protected passwords/secrets | Inherited / N/A | This crate holds no passwords; the only secret-shaped input (a signing/verifying key) is supplied by the caller, never generated, stored, or read from disk by this crate itself | N/A |

## 3.13 System and Communications Protection (SC)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.13.1 / 3.13.5 | Boundary protection / subnetwork separation | Gap | No network boundary of any kind exists in this crate to protect (E3b, deferred) | N/A |
| 3.13.6 | Deny network traffic by default | Gap | Moot — no network traffic of any kind originates from this crate | N/A |
| 3.13.8 | Encrypt CUI in transit | Gap | This crate has no transit at all (no socket); mTLS is E3b's job entirely | N/A |
| 3.13.11 | **Use FIPS-validated cryptography** | Gap | This crate performs SHA-256 hashing via `av_edge::hash` (`openssl::sha::sha256`, system/Homebrew OpenSSL) and reuses `av_edge::sign`/`verify`/`identity`'s ECDSA P-384 — the *algorithm* choice is correct, but neither this crate nor `av_edge` detects or claims a FIPS-validated module is actually linked (see `crates/av-dynamics-service/src/fips.rs::detect`'s own honest "no FIPS module present" finding on this same host, which this crate's own OpenSSL linkage shares) | `cargo test -p av-dynamics-service --lib fips::tests -- --nocapture` (the FIPS-detection code lives there; this crate shares the same underlying OpenSSL) |
| 3.13.15 | Protect authenticity of comms sessions | Gap | No session/connection concept of any kind (E3b) | N/A |

## 3.14 System and Information Integrity (SI)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.14.1 | Identify/correct flaws timely | Partial | `cargo deny check advisories` runs the RustSec vulnerability database against this workspace's `Cargo.lock`; nothing in this crate schedules that on a cadence | `export PATH="$HOME/.cargo/bin:/opt/homebrew/opt/rustup/bin:$PATH"; cargo deny check advisories` |
| 3.14.6 | Monitor for attacks / validate input | Met | Every field of an incoming batch that this pipeline's own decisions depend on is validated before being trusted: identity (if presented), `shard_key` format and batch/measurement agreement, signature and hash self-consistency, chain linkage, label/clearance, staleness, duplication — nine independently-typed, independently-counted refusal kinds, never a silent fallthrough (`src/ingest.rs::Ingest::submit`, `av_edge::chain::ChainVerifier::submit`) | `cargo test -p av-ingest --test rejections --nocapture` (all nine kinds, one deliberate corruption each) |
| 3.14.6 (log integrity) | Detect corruption of stored data | Met | `PartitionLog::open`'s crash recovery distinguishes a torn tail (expected, from an interrupted append — reported and excluded) from a corrupt *middle* record (a tamper indication — left untouched, and reported by `verify()` as a broken chain rather than silently repaired or ignored) | `cargo test -p av-ingest --test crash_recovery --test tamper -- --nocapture` |

## 3.11 Risk Assessment (RA)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.11.2 | Scan for vulnerabilities | Partial | `cargo deny check advisories` (RustSec DB) covers this crate's own dependency tree; no container/binary image scan exists (this crate produces no binary/image at all — it is a library) | `export PATH="$HOME/.cargo/bin:/opt/homebrew/opt/rustup/bin:$PATH"; cargo deny check advisories` |

## Inherited wholesale (not this crate's responsibility)

Matching `docs/compliance/av-dynamics-service/control-matrix.md`'s own posture for
families a library with no network, no host presence, and no personnel/physical footprint
has no material to implement:

| Family | IDs | Status | Notes |
|---|---|---|---|
| AT (Awareness & Training) | 3.2.1–3.2.3 | Inherited | Organizational training |
| IR (Incident Response) | 3.6.1–3.6.3 | Inherited | Organizational process; this crate's contribution is the audit trail above (AU) |
| MA (Maintenance) | 3.7.1–3.7.6 | Inherited | Host/equipment maintenance |
| MP (Media Protection) | 3.8.1–3.8.9 | Inherited | The partition log files' own filesystem permissions/at-rest protection are not set by this crate — `PartitionLog::open` does not `chmod` the file it creates (see Deficiencies) |
| PS (Personnel Security) | 3.9.1–3.9.2 | Inherited | Screening/transfer, organizational |
| PE (Physical Protection) | 3.10.1–3.10.6 | Inherited | Data-center/facility controls |
| CA (Security Assessment) | 3.12.1–3.12.4 | Inherited | SSP, POA&M, continuous monitoring; this document is an input to those, not a substitute |

## Deficiencies

Ranked by what a reviewer would flag first:

1. **E3b (the wire) does not exist yet, by design this round.** No gRPC service, no mTLS
   front, no plugin manifest handshake (SC 3.13.1/3.13.5/3.13.8/3.13.15, AC
   3.1.12/3.1.13). `docs/edge-plan.md`'s "decisions already taken by the manager" defers
   this explicitly: this repository has an OpenSSL TLS *client* connector
   (`crates/av-grpc`) and no server acceptor, and open question 155 ruled "no new
   crypto-adjacent crate" for that specific gap, with an nginx front as the resolution —
   a decision for the lead to confirm, not built here. Every row above marked Gap for a
   network/session/transport reason traces back to this one deficiency.
2. **No caller authentication at the library API boundary** (AC 3.1.1/3.1.2, AU 3.3.9).
   `Ingest::submit`'s `Signer::Certificate` authenticates a *batch's signer*, not the
   process or user calling this crate's own functions — appropriate for a library
   embedded inside a future trusted service process, but a real gap if this crate is ever
   linked into something that itself needs to gate who may call it.
3. **No FIPS-validated cryptographic module** (SC 3.13.11), inherited directly from the
   same OpenSSL linkage `crates/av-dynamics-service/src/fips.rs::detect` already proves
   lacks one on this host — this crate does not re-run that detection itself, but shares
   the same underlying gap.
4. **The partition log files have no retention, rotation, or at-rest access-control
   policy** (AU 3.3.1's retention half, MP 3.8.1). `PartitionLog::open` does not `chmod`
   the file, and nothing in this crate ever deletes, rotates, or archives an old
   partition file — it grows without bound, protected only by whatever filesystem
   permissions its containing directory happens to have.
5. **No timestamp authority of this crate's own** (AU 3.3.7). `batch_tai_ns` is entirely
   caller-supplied (question 199 forbids a live clock read here), so there is nothing in
   this crate that itself vouches for a record's time being NTP-accurate — that
   responsibility sits wherever a live clock is eventually read, outside this crate.
