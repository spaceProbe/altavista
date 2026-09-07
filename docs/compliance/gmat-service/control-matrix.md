# gmat-service — NIST SP 800-171 Rev 2 control matrix

`services/gmat-service` — the Python, `grpcio`-hosted `altavista.v1.DynamicsService`
(ADR-002 depth 1, "gmat-api": GMAT's own Python-API `Propagator`). ADR-003's 2026-09-02
amendment retired this service from the deployed runtime in favour of
`crates/av-dynamics-service` (the Rust host) — this service is now a **design-time** tool
(solvers, authoring, golden generation), but it still runs in the environment and still
needs its own control matrix (ADR-004: "every component... from its first commit").

Same format as `docs/compliance/av-dynamics-service/control-matrix.md` (family,
requirement ID, requirement, Status, implementation `file:function`, evidence command) and
the same Met / Partial / Inherited / Gap legend — see that document for the legend table in
full; it is not repeated here to avoid the two drifting out of sync.

```{admonition} Not a certification
Engineering documentation, not a C3PAO assessment. Most rows below are Gap or Inherited —
see the sibling document's admonition for why that is the honest, expected shape of a
first pass, not a defect.
```

## Scope, and the one gap this document does not paper over

Unlike `av-dynamics-service`, this service's TLS story has a **known, load-bearing
limitation, already documented before this task**
(`services/gmat-service/README.md`'s "A correctness finding" / FIPS-accounting sections):
`grpcio` bundles **BoringSSL**. Terminating TLS inside this process would put
non-system-OpenSSL cryptography directly in the crypto-rule-relevant path, so ADR-004's
decision (question 84) is the same nginx front `av-dynamics-service` uses
(`services/gmat-service/deploy/nginx-gmat-grpc.conf.template`) — but moving TLS off this
process does **not** remove BoringSSL from the dependency graph: it is still compiled into
`grpcio`'s wheel and present as object code whether or not this service ever calls
`grpc.secure_channel`/`add_secure_port`. This document reports that as a Gap (SC 3.13.11
and, more specifically, SI 3.14.1's "flaw remediation" for a crypto library this platform
does not control the patch cadence of) rather than treating "TLS moved to nginx" as having
closed the underlying issue.

## 3.1 Access Control (AC)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.1.1 / 3.1.2 | Limit system access to authorized users/processes | Gap | N/A — neither `DynamicsService`'s RPCs nor `/admin/api/evidence` authenticate a caller | N/A |
| 3.1.3 | Control the flow of CUI | Gap | N/A — no `Envelope`/`Label` handling in this service's wire messages | N/A |
| 3.1.5 | Least privilege | Inherited | Deployment-owned (OS user, container/unit hardening); not set by this package | N/A |
| 3.1.12 / 3.1.13 | Control & encrypt remote access | Partial | `services/gmat-service/gmat_service/server.py:serve` binds `127.0.0.1` only for both the gRPC and admin ports; encryption for any cross-host hop is the nginx front (`services/gmat-service/deploy/nginx-gmat-grpc.conf.template`), proven end to end | `.venv/bin/python -m pytest -q tests/test_grpc_tls.py -k "not against_rust_backend"` |
| 3.1.20 | Control connections to external systems | Inherited | Host firewall/network segmentation | N/A |
| 3.1.4/3.1.6–3.1.11/3.1.14–3.1.22 | Separation of duties, session lock, MFA, mobile/wireless, public posting, etc. | Inherited | Environment/IdP responsibilities; not applicable to a headless localhost gRPC service | N/A |

## 3.3 Audit and Accountability (AU)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.3.1 | Create and retain audit records | Met | `services/gmat-service/gmat_service/evidence.py:EvidenceLog.record` appends one JSONL record per RPC response (nine keys: the original six plus M7.3's `seq`/`prev_hash`/`hash`); no retention/rotation policy | `.venv/bin/python -m pytest -q tests/test_gmat_service.py` (the module-scoped server fixture is shared across the file; `test_evidence_log_gets_a_line_per_response`'s own record count assertion needs the full file, not an isolated `-k` selection) |
| 3.3.2 | Trace actions to individual users/processes | Partial | Records carry `run_id` (per-process) but no human/service principal (no identity layer upstream) | Same as above |
| 3.3.4 | Alert on audit logging failure | Partial | `EvidenceLog.record`'s file write is inside `with self._lock: ... fh.write(...)` (`services/gmat-service/gmat_service/evidence.py`) with no `try`/`except` — a write failure propagates as an unhandled exception (fail-loud), not a silent drop, but nothing forwards it to an operator/SIEM | N/A (code inspection) |
| 3.3.5 / 3.3.6 | Correlate and report audit review | Gap | No correlation tooling beyond the raw JSONL file and `/admin/api/evidence/verify`'s pass/fail result | N/A |
| 3.3.7 | Authoritative, time-synced timestamps | Inherited | `evidence.py`'s `epoch` is `altavista.cdm.utc_ns_to_tai_ns(time.time_ns())`; host NTP sync is the environment's responsibility | N/A |
| 3.3.8 | **Protect audit information from unauthorized access/modification** | Met | SHA-256 hash chain (`prev_hash`/`hash`, `"GENESIS"` convention, mirroring `proto/altavista/v1/envelope.proto`'s `SignedBatch`) — `services/gmat-service/gmat_service/evidence.py:EvidenceLog.verify` recomputes and reports the exact `seq` a tamper broke at | `.venv/bin/python -m pytest -q tests/test_compliance.py -k tamper` |
| 3.3.9 | Limit audit management to a subset of privileged users | Gap | `/admin/api/evidence` and `/admin/api/evidence/verify` (`services/gmat-service/gmat_service/admin.py:_AdminHandler.do_GET`) have no access control beyond the loopback bind | N/A |

## 3.4 Configuration Management (CM)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.4.1 | Baseline configuration & inventory | Partial | `pyproject.toml`/the venv's installed package set is the de facto inventory; no CycloneDX/SPDX SBOM is generated for this service | `.venv/bin/pip list` |
| 3.4.2 | Enforce security configuration settings | Partial | `services/gmat-service/gmat_service/config.py:SETTINGS` is a fixed module-level dict (no runtime-mutable config file), so there is no config-drift surface — but also nothing that would reject an unsafe override if one existed | N/A |
| 3.4.6 | Least functionality | Met | `services/gmat-service/gmat_service/admin.py:_AdminHandler.do_GET` serves exactly the two documented routes and 404s everything else; `services/gmat-service/gmat_service/service.py` implements exactly `altavista.v1.DynamicsService` | `.venv/bin/python -m pytest -q tests/test_compliance.py -k admin_unknown_path` |
| 3.4.7 | Restrict nonessential programs/ports | Partial | Two listeners, both loopback-only (gRPC + admin) | `lsof -nP -iTCP -sTCP:LISTEN \| grep gmat_service` (manual, host-dependent) |

## 3.5 Identification and Authentication (IA)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.5.1 / 3.5.2 | Identify and authenticate users/processes | Gap | Neither port authenticates a caller | N/A |
| 3.5.3 | MFA for privileged/remote access | Inherited | IdP responsibility (`secsso`) | N/A |
| 3.5.10 | Cryptographically-protected passwords/secrets | Inherited / N/A | No passwords or long-lived secrets in this service; the per-process `run_id` (`uuid.uuid4().hex`, `services/gmat-service/gmat_service/server.py`) is not a credential | N/A |

## 3.13 System and Communications Protection (SC)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.13.1 / 3.13.5 | Boundary protection / subnetwork separation | Inherited | Host firewall; this service's own contribution is loopback-only binding (`services/gmat-service/gmat_service/server.py:serve`) | N/A |
| 3.13.6 | Deny network traffic by default | Partial | `services/gmat-service/gmat_service/admin.py:_AdminHandler.do_GET` matches an explicit closed route list and 404s everything else | `.venv/bin/python -m pytest -q tests/test_compliance.py -k admin_unknown_path` |
| 3.13.8 | Encrypt CUI in transit | Partial | Plaintext by design; TLS is the nginx front (`services/gmat-service/deploy/nginx-gmat-grpc.conf.template`), proven end to end against this backend | `.venv/bin/python -m pytest -q tests/test_grpc_tls.py -k "not against_rust_backend"` |
| 3.13.11 | **Use FIPS-validated cryptography** | Gap | Two independent findings, both real: (1) `services/gmat-service/gmat_service/fips.py:detect` reports `_hashlib.get_fips_mode() == 0` for the interpreter's own default OpenSSL context on this host — FIPS mode is not active; (2) **`grpcio` itself bundles BoringSSL** (pre-existing, documented in `services/gmat-service/README.md`), so this dependency graph carries non-system-OpenSSL cryptographic code regardless of (1) | `.venv/bin/python -m pytest -q tests/test_compliance.py -k fips`, and see `services/gmat-service/README.md`'s BoringSSL discussion |
| 3.13.15 | Protect authenticity of comms sessions | Gap | No session/token layer | N/A |

## 3.14 System and Information Integrity (SI)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.14.1 | Identify/correct flaws timely | Gap | No automated dependency-vulnerability scan for this service's Python dependencies exists in this repository today (unlike the Rust side's `cargo deny check advisories`) — this is a genuine, undupliated gap, not covered by any other row | N/A |
| 3.14.6 | Monitor for attacks / validate input | Partial | Every RPC validates its request before doing work (`services/gmat-service/gmat_service/service.py:Describe`/`Derivatives`/`Step`/`Propagate` — unknown `model_id`, `Step.cov` unimplemented, etc., all rejected with a specific gRPC status rather than silently proceeding); no anomaly-detection/monitoring layer above that | `.venv/bin/python -m pytest -q tests/test_gmat_service.py -k "rejects or unimplemented"` |

## 3.11 Risk Assessment (RA)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.11.2 | Scan for vulnerabilities | Gap | No `pip-audit`/equivalent wired into this repository for `services/gmat-service`'s dependencies (mirrors SI 3.14.1's gap above — named again here because RA and SI are scored separately in a CMMC assessment even though the underlying gap is the same missing tool) | N/A |

## Inherited wholesale (not this service's responsibility)

Same posture and reasoning as `docs/compliance/av-dynamics-service/control-matrix.md`'s
equivalent section — AT (3.2.1–3.2.3), IR (3.6.1–3.6.3), MA (3.7.1–3.7.6), MP
(3.8.1–3.8.9), PS (3.9.1–3.9.2), PE (3.10.1–3.10.6), CA (3.12.1–3.12.4) are all
organizational/environmental for a headless local service; not repeated table-for-table
here to avoid the two documents drifting out of sync.

## Deficiencies

Ranked by what a reviewer would flag first:

1. **`grpcio` bundles BoringSSL** (SC 3.13.11, SI 3.14.1). This was already known and
   documented (`services/gmat-service/README.md`) before this task; it is repeated here
   because it is this service's single most consequential crypto-rule gap and moving TLS to
   nginx (the AC 3.1.12/SC 3.13.8 Partial) does not remove it from the dependency graph.
   There is no drop-in fix within `grpcio` as distributed (see that README section for what
   was actually evaluated).
2. **No authentication or authorization anywhere in this service** (AC 3.1.1/3.1.2, IA
   3.5.1/3.5.2, AU 3.3.9, SC 3.13.15) — identical shape to `av-dynamics-service`'s deficiency
   #1; ADR-004's `seccert`/`secsso` identity layer is not consumed by either service today.
3. **No dependency-vulnerability scanning for this service's Python dependencies** (SI
   3.14.1, RA 3.11.2). The Rust side has `cargo deny check advisories`; nothing equivalent
   (`pip-audit` or similar) exists for `services/gmat-service`'s `pyproject.toml`/venv.
4. **The evidence file and `/admin/api/evidence*` have no at-rest or access-control
   protection** (AU 3.3.9, MP-family) — identical shape to `av-dynamics-service`'s
   deficiency #3/#4.
