# av-dynamics-service — NIST SP 800-171 Rev 2 control matrix

`crates/av-dynamics-service` — the Rust `tonic` host of `altavista.v1.DynamicsService`
(ADR-002 depth 2, "gmat-ffi"). This maps the crate's own code to CMMC Level 2 (NIST SP
800-171 Rev 2, 110 requirements / 14 families), in the format
`github.com/secrouter/secrouter`'s `docs/compliance/cmmc-control-matrix.md` uses (family,
requirement ID, requirement, implementation `file:function`, evidence command), with the
Met / Partial / Inherited / Gap legend `secagent`'s `docs/cmmc.md` uses.

```{admonition} Not a certification
This is engineering documentation, not a C3PAO assessment or an attestation. CMMC Level 2
is a property of the accreditation boundary (host, network, identity provider, this
crate's own code) as a whole, not of one crate. **Most rows below are Gap or Inherited —
that is the honest state of a first compliance pass (ADR-004: "a control matrix with
mostly `Gap` rows is acceptable in P0; a component without one is not"), not a defect in
this document.**
```

## Legend

| Mark | Meaning |
|---|---|
| Met | Addressed by this crate's own code, verifiable by the evidence command in that row. |
| Partial | Something real exists but depends on deployment/configuration this crate does not itself control, or covers only part of the requirement. |
| Inherited | Satisfied by the environment/organization (host, network, IdP, enclave), not by this crate. |
| Gap | Not implemented anywhere in this crate today. See [Deficiencies](#deficiencies). |

## Scope

This crate hosts one gRPC service (`altavista.v1.DynamicsService`, plaintext on
`127.0.0.1` only) plus, as of this task, the `/admin/api/evidence` HTTP admin surface
(`crates/av-dynamics-service/src/admin.rs`) and its hash-chained evidence log (`crates/av-dynamics-service/src/evidence.rs`). It has **no
identity, authentication, or transport-encryption code of its own** — ADR-004's decision is
that a service-owned nginx front (`services/gmat-service/deploy/nginx-gmat-grpc.conf.template`,
proven against this exact Rust backend by `tests/test_grpc_tls.py`'s
`test_describe_succeeds_through_proxy_with_client_cert_against_rust_backend` /
`test_refused_without_client_cert_against_rust_backend`) terminates mTLS in front of it,
and that host/network controls provide the rest. Rows below say so explicitly rather than
crediting this crate for what the front door or the host does.

## 3.1 Access Control (AC)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.1.1 / 3.1.2 | Limit system access to authorized users/processes | Gap | N/A — neither the gRPC service nor `/admin/api/evidence` authenticates a caller; anything able to reach `127.0.0.1:<port>` is served | N/A |
| 3.1.3 | Control the flow of CUI | Gap | N/A — `altavista.v1.Envelope`/`Label` (`proto/altavista/v1/envelope.proto`) is not wired into `DynamicsService`'s request/response messages; this crate carries no handling-label enforcement | N/A |
| 3.1.5 | Least privilege | Inherited | The OS user/systemd hardening this process runs under is a deployment concern (ADR-004: "hardened systemd units ... same properties" as edge plugins), not something `crates/av-dynamics-service/src/bin/server.rs` sets | N/A |
| 3.1.12 / 3.1.13 | Control & encrypt remote access | Partial | `crates/av-dynamics-service/src/bin/server.rs:main` binds `127.0.0.1` only for both ports (gRPC and admin); encryption for any cross-host hop is the nginx front, proven against this backend specifically | `.venv/bin/python -m pytest -q tests/test_grpc_tls.py -k against_rust_backend -s` |
| 3.1.20 | Control connections to external systems | Inherited | Host firewall/network segmentation | N/A |
| 3.1.22 | Control publicly-posted content | Inherited | This crate posts nothing publicly (no outbound calls at all beyond the GMAT FFI boundary) | N/A |
| 3.1.4/3.1.6–3.1.11/3.1.14–3.1.19/3.1.21 | Separation of duties, session lock, MFA-gated remote access, mobile/wireless, etc. | Inherited | Environment/IdP responsibilities; not applicable to a headless localhost gRPC service | N/A |

## 3.3 Audit and Accountability (AU)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.3.1 | Create and retain audit records | Met | `crates/av-dynamics-service/src/evidence.rs:EvidenceLog::record` appends one JSONL record per successful RPC (`epoch`, `method`, `request_hash`, `response_hash`, `run_id`, `settings_hash`, plus the M7.3 chain fields below); retention is unbounded local-file (no rotation/expiry policy — see Deficiencies) | `cargo test -p av-dynamics-service --lib evidence::tests::record_appends_one_sorted_key_json_line_per_call` |
| 3.3.2 | Trace actions to individual users/processes | Partial | Every record carries `run_id` (one id per server process) but no human/service *principal* — there is no identity layer upstream of this crate to attribute a call to (see AC 3.1.1/3.5.1) | `cargo test -p av-dynamics-service --lib evidence::tests::first_record_chains_from_the_genesis_sentinel` |
| 3.3.4 | Alert on audit logging failure | Partial | `EvidenceLog::record`'s write path uses `.expect(...)` (`crates/av-dynamics-service/src/evidence.rs`), so a write failure is fail-loud (crashes the process, matching `gmat_service.evidence.EvidenceLog.record`'s unhandled-exception behaviour) rather than being silently swallowed — but this is a crash, not an *alert*; nothing forwards the failure to an operator/SIEM | N/A (crash-on-failure is not independently exercised by an automated test — verified by code inspection of `writeln!(f, "{line}").expect(...)`, `crates/av-dynamics-service/src/evidence.rs`) |
| 3.3.5 / 3.3.6 | Correlate and report audit review | Gap | No correlation tooling beyond the raw JSONL file and `/admin/api/evidence/verify`'s pass/fail result | N/A |
| 3.3.7 | Authoritative, time-synced timestamps | Inherited | `evidence.rs`'s `epoch` is TAI-converted wall-clock time (`av_cdm::time::Tai::from_utc_nanos`); NTP synchronization of the host clock is the environment's responsibility | N/A |
| 3.3.8 | **Protect audit information from unauthorized access/modification** | Met | SHA-256 hash chain (`prev_hash`/`hash`, `"GENESIS"` convention per `proto/altavista/v1/envelope.proto`'s `SignedBatch`) — `crates/av-dynamics-service/src/evidence.rs:EvidenceLog::verify` recomputes and detects any single-record tamper, reporting the exact `seq` it broke at | `cargo test -p av-dynamics-service --lib evidence::tests::verify_detects_a_tampered_record_and_reports_its_sequence_number -- --nocapture` |
| 3.3.9 | Limit audit management to a subset of privileged users | Gap | `/admin/api/evidence` and `/admin/api/evidence/verify` (`crates/av-dynamics-service/src/admin.rs:serve`) have no access control at all beyond the loopback bind — any local process can read them | N/A |

## 3.4 Configuration Management (CM)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.4.1 | Baseline configuration & inventory | Partial | `Cargo.lock` pins every dependency; `../../deny.toml` (repo root, read-only to this task) bans forbidden crypto crates and enforces a license allow-list — but no CycloneDX/SPDX SBOM is generated for this crate specifically | `cargo tree -p av-dynamics-service` |
| 3.4.2 | Enforce security configuration settings | Partial | `crates/av-dynamics-service/src/config.rs`'s model/force-model settings are compile-time constants (no runtime config file this crate reads), so there is no config-drift surface to misconfigure — but also no schema-validated config to point to as "enforcement" | N/A |
| 3.4.6 | Least functionality | Met | `crates/av-dynamics-service/src/admin.rs:handle_connection` matches exactly the two documented `GET` routes (`/admin/api/evidence`, `/admin/api/evidence/verify`) and 404s/405s everything else; the gRPC service implements exactly `altavista.v1.DynamicsService`, nothing more | `curl -s http://127.0.0.1:<admin_port>/nope` returns `404` (see `test_admin_unknown_path_is_404`, `tests/test_dynamics_service_rs.py`) |
| 3.4.7 | Restrict nonessential programs/ports | Partial | Exactly two listeners, both loopback-only (gRPC + admin); no other ports opened by this crate | `lsof -nP -iTCP -sTCP:LISTEN | grep av-dynamics-service` (manual, host-dependent — not an automated test) |

## 3.5 Identification and Authentication (IA)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.5.1 / 3.5.2 | Identify and authenticate users/processes | Gap | Neither port authenticates a caller; ADR-004 names `seccert` (machine identity/mTLS) and `secsso` (human identity/OIDC) but neither is consumed by this crate today | N/A |
| 3.5.3 | MFA for privileged/remote access | Inherited | IdP responsibility (`secsso`), not applicable to a headless service with no interactive login | N/A |
| 3.5.10 | Cryptographically-protected passwords/secrets | Inherited / N/A | This crate holds no passwords or long-lived secrets (the one generated value, the per-process `run_id`, is not a credential — `crates/av-dynamics-service/src/bin/server.rs:random_run_id`) | N/A |

## 3.13 System and Communications Protection (SC)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.13.1 / 3.13.5 | Boundary protection / subnetwork separation | Inherited | Host firewall/network segmentation; this crate's own contribution is binding loopback-only (`crates/av-dynamics-service/src/bin/server.rs:main`) | N/A |
| 3.13.6 | Deny network traffic by default | Partial | `crates/av-dynamics-service/src/admin.rs:handle_connection` matches an explicit, closed route list and refuses (`404`)/rejects (`405`) everything else — deny-by-default *within* this process's own HTTP surface, not a network-layer control | `curl` a non-`GET` method or an unknown path against `/admin/api/evidence*` (see `test_admin_unknown_path_is_404`) |
| 3.13.8 | Encrypt CUI in transit | Partial | This crate is plaintext by design (ADR-004/ADR-003 amendment); the nginx front provides TLS 1.2/1.3 with ECDSA P-384 (`services/gmat-service/deploy/nginx-gmat-grpc.conf.template`), proven against this backend specifically | `.venv/bin/python -m pytest -q tests/test_grpc_tls.py -k against_rust_backend -s` |
| 3.13.11 | **Use FIPS-validated cryptography** | Gap | `crates/av-dynamics-service/src/fips.rs:detect` *detects* (never claims) the linked OpenSSL's FIPS posture by attempting to load the OpenSSL `"fips"` provider module; on this build (Homebrew `openssl@3` 3.6.3, no `fips.dylib` shipped) the load fails, i.e. **no FIPS-validated module is present at all** — SHA-256 is the correct *algorithm*, but algorithm choice alone is not FIPS validation | `cargo test -p av-dynamics-service --lib fips::tests -- --nocapture`, or live: `curl -s http://127.0.0.1:<admin_port>/admin/api/evidence \| python3 -m json.tool` and read the `"fips"` object |
| 3.13.15 | Protect authenticity of comms sessions | Gap | No session/token layer of any kind on either port | N/A |

## 3.14 System and Information Integrity (SI)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.14.1 | Identify/correct flaws timely | Partial | `cargo deny check advisories` runs the RustSec vulnerability database against this workspace's `Cargo.lock` (repo-root `deny.toml`, read-only to this task); nothing in this crate schedules/automates that check on a cadence | `export PATH="$HOME/.cargo/bin:/opt/homebrew/opt/rustup/bin:$PATH"; cargo deny check advisories` |
| 3.14.6 | Monitor for attacks / validate input | Partial | Every RPC handler validates its request before doing any work (`crates/av-dynamics-service/src/service.rs:describe`/`derivatives`/`step`/`propagate` — model id, state-vector length, positive `dt_s`/`sample_interval_s`, `seed.cov` shape, unimplemented fields rejected as `UNIMPLEMENTED` rather than silently ignored); there is no anomaly-detection or attack-monitoring layer above that | `.venv/bin/python -m pytest -q tests/test_dynamics_service_rs.py -k "rejects or invalid"` |

## 3.11 Risk Assessment (RA)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.11.2 | Scan for vulnerabilities | Partial | `cargo deny check advisories` (RustSec DB) covers this crate's own dependency tree; no container/binary image scan exists for this crate | `export PATH="$HOME/.cargo/bin:/opt/homebrew/opt/rustup/bin:$PATH"; cargo deny check advisories` |

## Inherited wholesale (not this crate's responsibility)

Matching `secagent`'s own page's posture for families a headless localhost service has no
material to implement:

| Family | IDs | Status | Notes |
|---|---|---|---|
| AT (Awareness & Training) | 3.2.1–3.2.3 | Inherited | Organizational training |
| IR (Incident Response) | 3.6.1–3.6.3 | Inherited | Organizational process; this crate's contribution is the audit trail above (AU) |
| MA (Maintenance) | 3.7.1–3.7.6 | Inherited | Host/equipment maintenance |
| MP (Media Protection) | 3.8.1–3.8.9 | Inherited | The evidence JSONL file's own filesystem permissions/at-rest protection are not set by this crate (unlike `secagent`'s `0700`/`0600` `purge` pattern — see Deficiencies) |
| PS (Personnel Security) | 3.9.1–3.9.2 | Inherited | Screening/transfer, organizational |
| PE (Physical Protection) | 3.10.1–3.10.6 | Inherited | Data-center/facility controls |
| CA (Security Assessment) | 3.12.1–3.12.4 | Inherited | SSP, POA&M, continuous monitoring; this document is an input to those, not a substitute |

## Deficiencies

Ranked by what a reviewer would flag first:

1. **No authentication or authorization anywhere in this crate** (AC 3.1.1/3.1.2, IA
   3.5.1/3.5.2, AU 3.3.9, SC 3.13.15). Every RPC and both admin routes are open to any
   process that can reach the loopback ports. `seccert` mTLS identity is planned at the
   nginx front (SC 3.13.8's Partial) but does not give this *process* a notion of caller
   identity even when the front is deployed — a caller behind the mTLS-terminating proxy is
   indistinguishable from any other loopback caller to `crates/av-dynamics-service/src/service.rs`/`crates/av-dynamics-service/src/admin.rs`.
2. **No FIPS-validated cryptographic module** (SC 3.13.11). `crates/av-dynamics-service/src/fips.rs` proves this by
   actually attempting the OpenSSL provider load, not by reading a version string — see the
   FIPS posture section of this task's report for the exact detected result.
3. **The evidence file has no retention, rotation, or at-rest access-control policy** (AU
   3.3.1's retention half, MP 3.8.1). It grows without bound and is protected only by
   whatever filesystem permissions its containing directory happens to have — `EvidenceLog::open`
   (`crates/av-dynamics-service/src/evidence.rs`) does not `chmod` the file the way `secagent`'s own audit log does
   (`0600`).
4. **`/admin/api/evidence*` has no access control of its own** (AU 3.3.9). Loopback-only
   binding is the entire boundary; any other localhost process (or, before an mTLS front is
   deployed, any host account) can read the full evidence chain and its `settings_hash`.
