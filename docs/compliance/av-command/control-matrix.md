# av-command — NIST SP 800-171 Rev 2 control matrix

`crates/av-command` — the command authority library (ADR-004's "Command authority" section;
`docs/aiplane-plan.md` milestone A1). This maps the crate's own code to CMMC Level 2 (NIST SP
800-171 Rev 2, 110 requirements / 14 families), in the format
`github.com/secrouter/secrouter`'s `docs/compliance/cmmc-control-matrix.md` uses (family,
requirement ID, requirement, implementation `file:function`, evidence command), with the
Met / Partial / Inherited / Gap legend `secagent`'s `docs/cmmc.md` uses.

```{admonition} Not a certification
This is engineering documentation, not a C3PAO assessment or an attestation. CMMC Level 2
is a property of the accreditation boundary as a whole, not of one crate. **Most rows below
are Gap — that is the honest state of this milestone (A1: "the command state machine as a
library, the durable hash-chained ledger, the injected clock, the evidence/admin surface"),
not a defect in this document.** This crate has no gRPC service, no principal, no role or
delegation model, and no policy evaluator yet — A1's own service surface, A1.2's policy, and
A2's human-authorization layer are later milestones that will turn several Gap rows into Met
or Partial. A row claiming Met for something those milestones have not built yet would be the
actual defect; none of the rows below do that.
```

## Legend

| Mark | Meaning |
|---|---|
| Met | Addressed by this crate's own code, verifiable by the evidence command in that row. |
| Partial | Something real exists but depends on deployment/configuration this crate does not itself control, or covers only part of the requirement. |
| Inherited | Satisfied by the environment/organization (host, network, IdP, enclave), not by this crate. |
| Gap | Not implemented anywhere in this crate today. See [Deficiencies](#deficiencies). |

## Scope

This crate is, as of A1, a **library plus one localhost HTTP admin surface** — there is no
gRPC service, no wire-level command intake, and no caller-facing authentication or
authorization of any kind:

- `src/state.rs` — the `CommandState` machine (`propose`/`check`/`authorize`/`dispatch`/
  `ack`/`reject`/`expire`/`fail`), in-process, called by whatever owns a `Command` value —
  today, that is only this crate's own test suite.
- `src/ledger.rs` — the durable, file-backed, hash-chained, per-partition command ledger.
- `src/clock.rs` — the injected TAI clock (`Clock`, `SystemClock`, `TestClock`).
- `src/evidence.rs` + `src/admin.rs` — `GET /admin/api/evidence` and `GET /admin/api/
  evidence/verify`, loopback-only, the same hand-rolled `tokio::net::TcpListener` shape
  `crates/av-dynamics-service/src/admin.rs` uses.
- `src/fips.rs` — FIPS posture detection, copied from `crates/av-dynamics-service/src/
  fips.rs` (see that module's own doc comment for the copy-vs-import decision).

Not yet: Rego policy evaluation at `CHECKED` (A1.2); the gRPC service surface (the rest of
A1); `Principal`/`Delegation`/role bindings/MFA (A2); dispatch into the kernel's real
telecommand path (A3). Rows below score what exists today, not those milestones.

## 3.1 Access Control (AC)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.1.1 / 3.1.2 | Limit system access to authorized users/processes | Gap | Neither `admin::serve` nor any `state.rs` transition function authenticates a caller; anything able to call this library in-process, or reach `127.0.0.1:<port>`, is served | N/A |
| 3.1.3 | Control the flow of CUI | Gap | `Label` (`altavista.v1.Label`, carried on `PolicyInput` and `Command`) is a field this crate's types carry, but nothing in `src/` enforces a label check anywhere — that is A1.2's (policy) and A4's (gateway) job | N/A |
| 3.1.5 | Least privilege | Inherited | OS user/systemd hardening this process runs under is a deployment concern, not something this crate's code sets | N/A |
| 3.1.12 / 3.1.13 | Control & encrypt remote access | Gap | This crate binds only loopback in its own tests (`crates/av-command/src/admin.rs:spawn_test_server` binds `127.0.0.1:0`); there is no service binary yet to fix a real bind address, and no TLS/mTLS front has been built or proven for this crate the way `av-dynamics-service`'s nginx front is proven against it | N/A |
| 3.1.20 | Control connections to external systems | Inherited | Host firewall/network segmentation | N/A |
| 3.1.22 | Control publicly-posted content | Inherited | This crate posts nothing publicly | N/A |
| 3.1.4/3.1.6–3.1.11/3.1.14–3.1.19/3.1.21 | Separation of duties, session lock, MFA-gated remote access, mobile/wireless, etc. | Inherited | Environment/IdP responsibilities (A2 will add the MFA claim check itself, at which point 3.5.3 gains a Partial) | N/A |

## 3.3 Audit and Accountability (AU)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.3.1 | Create and retain audit records | Met | `crates/av-command/src/ledger.rs:Ledger::append` appends one length-prefixed `LedgerRecord` per transition, forever (no rotation/expiry policy — see Deficiencies); retention is unbounded local-file | `cargo test -p av-command --lib ledger::tests::later_records_chain_prev_hash_to_the_previous_records_hash` |
| 3.3.2 | Trace actions to individual users/processes | Partial | Every `CommandTransition` carries a `principal` string (`crates/av-command/src/state.rs`'s `propose`/`check`/.../`fail`, all take `principal: &str`), but nothing verifies that string is who it claims to be — no OIDC/identity layer exists yet (A2) | `cargo test -p av-command --lib state::tests::propose_builds_proposed_from_a_fresh_command_with_the_clocks_epoch` |
| 3.3.4 | Alert on audit logging failure | Partial | `Ledger::append`/`Ledger::verify` return `std::io::Result`, so a write/read failure is a real, propagated `Err`, never silently swallowed — but this is fail-loud to the caller, not an *alert* to an operator/SIEM | N/A (verified by code inspection: every fallible I/O call in `crates/av-command/src/ledger.rs` uses `?`, never `.ok()`/`.unwrap_or_default()`) |
| 3.3.5 / 3.3.6 | Correlate and report audit review | Gap | No correlation tooling beyond the raw per-partition ledger files and `/admin/api/evidence/verify`'s pass/fail result | N/A |
| 3.3.7 | Authoritative, time-synced timestamps | Inherited | Every `LedgerRecord.tai_ns` comes from the caller's injected `Clock` (`crates/av-command/src/clock.rs`); NTP synchronization of whatever `SystemClock` reads is the environment's responsibility | N/A |
| 3.3.8 | **Protect audit information from unauthorized access/modification** | Met | SHA-256 hash chain (`prev_hash`/`hash`, `"GENESIS"` convention, `authority.proto`'s `LedgerRecord`) — `crates/av-command/src/ledger.rs:Ledger::verify` recomputes and detects any single-record tamper (content or `prev_hash` link), reporting the exact `seq` it broke at, read straight from disk independent of in-memory state | `cargo test -p av-command --lib ledger::tests::verify_detects_a_tampered_record_body_and_reports_its_sequence_number -- --nocapture` and `ledger::tests::verify_detects_a_severed_prev_hash_link` |
| 3.3.9 | Limit audit management to a subset of privileged users | Gap | `/admin/api/evidence` and `/admin/api/evidence/verify` (`crates/av-command/src/admin.rs:serve`) have no access control at all beyond the loopback bind — any local process can read the full ledger summary and run `verify` | N/A |

## 3.4 Configuration Management (CM)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.4.1 | Baseline configuration & inventory | Partial | `Cargo.lock` pins every dependency; the workspace-root `deny.toml` bans forbidden crypto crates and enforces a license allow-list across every crate including this one — but no CycloneDX/SPDX SBOM is generated for this crate specifically | `cargo tree -p av-command` |
| 3.4.2 | Enforce security configuration settings | Gap | No runtime config file this crate reads yet (the ledger directory, admin bind address, etc. are all constructor/function arguments a future service binary would supply) — no config-drift surface exists to misconfigure, but also nothing to point to as "enforcement" | N/A |
| 3.4.6 | Least functionality | Met | `crates/av-command/src/admin.rs:handle_connection` matches exactly the two documented `GET` routes and 404s/405s everything else | `cargo test -p av-command --lib admin::tests::unknown_path_is_404_and_non_get_is_405` |
| 3.4.7 | Restrict nonessential programs/ports | Partial | Exactly one listener in this crate's own code path (the admin HTTP server), loopback-only in every test; there is no gRPC/command-intake port yet (A1's remaining scope) | N/A |

## 3.5 Identification and Authentication (IA)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.5.1 / 3.5.2 | Identify and authenticate users/processes | Gap | No port or function in this crate authenticates a caller; ADR-004/A2 name OIDC principals and role-gated authorization, neither of which exists in this crate yet | N/A |
| 3.5.3 | MFA for privileged/remote access | Gap | A2's plan is an `amr`/`acr`-checked MFA claim on `authorize` for hazardous command classes; `crates/av-command/src/state.rs:authorize` exists today but performs no MFA check of any kind — anyone who can call it authorizes anything | N/A |
| 3.5.10 | Cryptographically-protected passwords/secrets | Inherited / N/A | This crate holds no passwords or long-lived secrets | N/A |

## 3.13 System and Communications Protection (SC)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.13.1 / 3.13.5 | Boundary protection / subnetwork separation | Inherited | Host firewall/network segmentation; this crate's own tests bind loopback-only | N/A |
| 3.13.6 | Deny network traffic by default | Partial | `crates/av-command/src/admin.rs:handle_connection` matches an explicit, closed route list and refuses (`404`)/rejects (`405`) everything else — deny-by-default *within* this process's own HTTP surface, not a network-layer control | `cargo test -p av-command --lib admin::tests::unknown_path_is_404_and_non_get_is_405` |
| 3.13.8 | Encrypt CUI in transit | Gap | This crate has no TLS of its own and no proven nginx front the way `av-dynamics-service` has (`tests/test_grpc_tls.py`'s `..._against_rust_backend` tests) — there is no cross-host deployment of this crate yet to front | N/A |
| 3.13.11 | **Use FIPS-validated cryptography** | Gap | `crates/av-command/src/fips.rs:detect` *detects* (never claims) the linked OpenSSL's FIPS posture by attempting to load the OpenSSL `"fips"` provider module; on this build (Homebrew `openssl@3`, no `fips.dylib` shipped) the load fails — no FIPS-validated module is present at all | `cargo test -p av-command --lib fips::tests -- --nocapture`, or live: `curl -s http://127.0.0.1:<admin_port>/admin/api/evidence \| python3 -m json.tool` and read the `"fips"` object |
| 3.13.15 | Protect authenticity of comms sessions | Gap | No session/token layer of any kind | N/A |

## 3.14 System and Information Integrity (SI)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.14.1 | Identify/correct flaws timely | Partial | `cargo deny check advisories` runs the RustSec vulnerability database against this workspace's `Cargo.lock`, including this crate's dependency tree; nothing schedules that check on a cadence | `export PATH="$HOME/.cargo/bin:/opt/homebrew/opt/rustup/bin:$PATH"; cargo deny check advisories` |
| 3.14.6 | Monitor for attacks / validate input | Partial | Every `state.rs` edge function validates the attempted edge against the command's current state before doing anything (`crates/av-command/src/state.rs:require_one_of`), refusing every illegal (state, edge) pair with a typed `CommandError::IllegalTransition`; `propose` additionally refuses a non-empty `envelope_id` (question 53) and a non-fresh `Command`. There is no anomaly-detection layer, and no wire-level input exists yet to validate (no gRPC surface) | `cargo test -p av-command --lib state::tests::every_state_edge_pair_in_the_product_is_legal_or_typed_refused` |

## 3.11 Risk Assessment (RA)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.11.2 | Scan for vulnerabilities | Partial | `cargo deny check advisories` (RustSec DB) covers this crate's own dependency tree; no container/binary image scan exists for this crate | `export PATH="$HOME/.cargo/bin:/opt/homebrew/opt/rustup/bin:$PATH"; cargo deny check advisories` |

## Inherited wholesale (not this crate's responsibility)

Matching `av-dynamics-service`'s own page's posture for families a library-plus-admin-surface
crate has no material to implement:

| Family | IDs | Status | Notes |
|---|---|---|---|
| AT (Awareness & Training) | 3.2.1–3.2.3 | Inherited | Organizational training |
| IR (Incident Response) | 3.6.1–3.6.3 | Inherited | Organizational process; this crate's contribution is the audit trail above (AU) |
| MA (Maintenance) | 3.7.1–3.7.6 | Inherited | Host/equipment maintenance |
| MP (Media Protection) | 3.8.1–3.8.9 | Inherited | The ledger files' own filesystem permissions/at-rest protection are not set by this crate — see Deficiencies |
| PS (Personnel Security) | 3.9.1–3.9.2 | Inherited | Screening/transfer, organizational |
| PE (Physical Protection) | 3.10.1–3.10.6 | Inherited | Data-center/facility controls |
| CA (Security Assessment) | 3.12.1–3.12.4 | Inherited | SSP, POA&M, continuous monitoring; this document is an input to those, not a substitute |

## Deficiencies

Ranked by what a reviewer would flag first:

1. **No authentication or authorization anywhere in this crate** (AC 3.1.1/3.1.2, IA
   3.5.1/3.5.2/3.5.3, AU 3.3.9, SC 3.13.15). Every `state.rs` transition function and both
   admin routes are open to any caller that can reach them. A2 (`Principal`, `Delegation`,
   role bindings, MFA) is the milestone that closes this; `authority.proto`'s own header
   comment names exactly what A2 will add so the next worker does not invent a second shape.
2. **No policy at `CHECKED`** (AC 3.1.3, SI 3.14.6's rate/label half). `check`
   (`crates/av-command/src/state.rs`) advances the state unconditionally once called; nothing
   in this crate today decides whether a command *should* be checked-through versus
   rejected. A1.2 adds the Rego evaluator; this crate already carries `PolicyInput`/
   `PolicyDecision` (`authority.proto`) for it to fill in.
3. **No FIPS-validated cryptographic module** (SC 3.13.11). `crates/av-command/src/fips.rs`
   proves this by actually attempting the OpenSSL provider load, not by reading a version
   string — see this task's report for the exact detected result on this host.
4. **The ledger has no retention, rotation, or at-rest access-control policy** (AU 3.3.1's
   retention half, MP 3.8.1). Each partition file grows without bound and is protected only
   by whatever filesystem permissions its containing directory happens to have —
   `Ledger::open`/`Ledger::append` (`crates/av-command/src/ledger.rs`) do not `chmod` the
   file.
5. **`/admin/api/evidence*` has no access control of its own** (AU 3.3.9). Loopback-only
   binding is the entire boundary; any other localhost process can read the full ledger
   summary and trigger a full `verify()` walk of every partition.
6. **No transport encryption, and none proven** (SC 3.13.8). Unlike `av-dynamics-service`,
   this crate has no nginx front proven against it yet, because it has no deployed service
   binary yet — the rest of A1 (the gRPC service) is what a front would sit in front of.
