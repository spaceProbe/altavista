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
are still Gap or Partial — that is the honest state of this milestone (A1 through A2.1: "the
command state machine as a library, the durable hash-chained ledger, the injected clock,
Rego policy evaluation at CHECKED, `CommandAuthorityService` over the wire, and now real
OIDC identity verification on `Authorize`"), not a defect in this document.** A2.1 gives the
identification/authentication rows (3.5.x) a real implementation to score for the first
time, which is why they move from Gap to Partial in this revision — but this crate still has
no role, MFA or delegation *gate* (A2.2): a verified principal, with any claims, authorizes
any command class today. **The authorization rows (3.1.x) are deliberately left exactly as
A1.3 scored them** — A2.1 verifies *who* the caller is, not *whether* they may act, so those
rows have not earned an upgrade; A2.2 is the milestone that will change them.
```

## Legend

| Mark | Meaning |
|---|---|
| Met | Addressed by this crate's own code, verifiable by the evidence command in that row. |
| Partial | Something real exists but depends on deployment/configuration this crate does not itself control, or covers only part of the requirement. |
| Inherited | Satisfied by the environment/organization (host, network, IdP, enclave), not by this crate. |
| Gap | Not implemented anywhere in this crate today. See [Deficiencies](#deficiencies). |

## Scope

This crate is, as of A2.1, a **library plus a real gRPC service plus one localhost HTTP admin
surface** — `CommandAuthorityService` now gives every state-machine edge and the ledger a
real, network-facing (loopback-only) surface, and `Authorize` now really authenticates its
caller's `principal_token` against a configured OIDC issuer — but there is still no
authorization (role/MFA/delegation) gate of any kind, and every RPC other than `Authorize`
remains reachable by any caller that can reach the bind address:

- `src/state.rs` — the `CommandState` machine (`propose`/`check`/`authorize`/`dispatch`/
  `ack`/`reject`/`expire`/`fail`), in-process, called by `src/service.rs`'s RPC handlers (and
  by this crate's own test suite directly).
- `src/ledger.rs` — the durable, file-backed, hash-chained, per-partition command ledger.
- `src/clock.rs` — the injected TAI clock (`Clock`, `SystemClock`, `TestClock`).
- `src/policy.rs` (A1.2) — Rego policy evaluation at `CHECKED`: `PolicyBundle` (loaded from
  `.rego` files, content-hashed), `evaluate` (in-process `regorus::Engine`, a fresh one per
  call, no network/crypto builtin compiled in at all — see that module's own doc comment),
  and the `profiles/execution.yaml` `authority:` block loader.
- `src/rate.rs` (A1.2) — `RateSource`, and its two implementors: `LedgerRateSource` (real
  history from `src/ledger.rs`'s `count_proposed_by_class_in_window`) and
  `FixtureRateSource` (a deterministic test double).
- `src/authority.rs` (A1.2) — `check_command`: evaluates policy over a `PROPOSED` `Command`
  and drives `state::check`/`state::reject` plus the matching `Ledger::append`, with the
  decision id and policy hash written into the transition's `reason` (`format_reason`/
  `parse_reason`).
- `src/service.rs` (A1.3; **A2.1, updated this task**) — `CommandAuthorityServiceImpl`:
  `Propose`/`Check`/`Authorize`/`Dispatch`/`Ack`/`Query`/`VerifyLedger` over a real `tonic`
  gRPC server, plaintext on loopback only (question 155, refused at a typed
  `resolve_loopback_bind_address` boundary — see that function's own doc), a `DispatchSink`
  seam A3 fills, and duplicate-`idempotency_key` refusal at `Dispatch`. **`Authorize` now
  verifies `principal_token` for real** (`crate::oidc::verify`, against the
  `IssuerConfig` this service is constructed with) before attempting any state transition,
  and records the verified `Principal.sub` — never the raw token — as `CommandTransition.
  principal`; an unverifiable token is refused `UNAUTHENTICATED` with `AUTHORIZE_VERIFIED_
  REASON` no longer claiming identity is unverified, because it now is. `delegation_id` is
  still recorded exactly as A1.3 left it: unevaluated, for A2.2.
- `src/oidc.rs` (**A2.1, new this task**) — OIDC token verification against the secsso
  claims contract: `verify`, `IssuerConfig`, the full `TokenError` refusal vocabulary. RS256
  only (ES256/ES384 a named, documented gap — see that module's doc). A pure function of
  (token, issuer configuration, clock reading); no I/O, no environment read, no wall clock.
- `src/test_support.rs` (**A2.1, new this task**) — test-fixture-only, and gated out of a
  default build entirely (`#[cfg(any(test, feature = "test-support"))]` on the `pub mod` in
  `src/lib.rs`, a `test-support` Cargo feature this crate's own `[dev-dependencies]` turns on
  for the integration-test build only — see that module's doc for why an earlier, ungated
  revision was rejected on review): `TestIssuer`, a local OpenSSL-backed OIDC issuer that
  mints real, signed RS256 tokens with caller-chosen claims. Absent from `cargo build
  -p av-command`'s default-feature artifact, not merely unreferenced by it.
- `src/bin/av-command.rs` (A1.3; **A2.1, updated this task**) — the service binary:
  CLI-argument configuration only (question 199: never the process environment), the gRPC
  server and the admin HTTP server both bound loopback-only. `--oidc-issuer`/
  `--oidc-audience`/`--oidc-public-key-path` are now required flags with no default of any
  kind — this binary refuses to start without an operator-supplied OIDC configuration.
- `src/evidence.rs` + `src/admin.rs` — `GET /admin/api/evidence` and `GET /admin/api/
  evidence/verify`, loopback-only, the same hand-rolled `tokio::net::TcpListener` shape
  `crates/av-dynamics-service/src/admin.rs` uses.
- `src/fips.rs` — FIPS posture detection, copied from `crates/av-dynamics-service/src/
  fips.rs` (see that module's own doc comment for the copy-vs-import decision).

Not yet: role bindings, MFA gating, delegation-expiry enforcement, the SIEM audit-line
export (A2.2 — `Principal.groups`/`amr`/`acr` and `CommandTransition.delegation_id` already
exist and are already carried, but nothing in this crate reads them for a decision yet);
dispatch into the kernel's real telecommand path (A3 — `DispatchSink` is the seam,
`RecordingDispatchSink` the only implementor this crate ships). Rows below score what exists
today, not those milestones.

## 3.1 Access Control (AC)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.1.1 / 3.1.2 | Limit system access to authorized users/processes | Gap | `CommandAuthorityServiceImpl` (`crates/av-command/src/service.rs`) serves a real gRPC surface. **A2.1 (this task) makes `Authorize` verify `principal_token` for real** (`crate::oidc::verify`, `AUTHORIZE_VERIFIED_REASON` on the transition) — but that is authentication, not authorization: no RPC, `Authorize` included, checks whether the now-real identity is *allowed* to do anything, and the other six RPCs (`Propose`/`Check`/`Dispatch`/`Ack`/`Query`/`VerifyLedger`) remain reachable by any caller that can reach the bind address. Limiting system access to *authorized* users/processes is still Gap: role-gated authorization is A2.2's job, not earned by this task | N/A |
| 3.1.3 | Control the flow of CUI | Partial | `Label` (`altavista.v1.Label`) is carried on `PolicyInput` and reaches the Rego evaluator (`crates/av-command/src/policy.rs`'s `canonical_input_json`), so a policy authored to check it can refuse on label today — but `profiles/policies/authority/command.rego`, the shipped starter policy, does not itself write a label check (its four rules are class-admit/class-reject/rate-limit/envelope-refuse, per A1's own fixture requirements); a real label-flow-control policy is future policy-authoring work, not a code gap in the evaluator. A4's gateway (label-aware query refusal) remains the other, unbuilt half | `cargo test -p av-command --test policy_fixture` |
| 3.1.5 | Least privilege | Inherited | OS user/systemd hardening this process runs under is a deployment concern, not something this crate's code sets | N/A |
| 3.1.12 / 3.1.13 | Control & encrypt remote access | Partial | **New this task, the "control" half:** `crates/av-command/src/service.rs:resolve_loopback_bind_address` refuses a non-loopback gRPC/admin bind address at startup with a typed `BindAddressError::NotLoopback` naming question 155 — `src/bin/av-command.rs` calls it for both listeners before either socket is ever bound, so a misconfigured deployment cannot even start non-loopback, not merely "happens to bind loopback in tests" as the previous revision of this row said. The "encrypt" half is still Gap: this crate links no TLS stack of any kind (ADR-004), and no nginx mTLS front (question 84) has been built or proven for this crate the way `av-dynamics-service`'s is | `cargo test -p av-command --lib service::tests::resolve_loopback_bind_address_refuses_every_non_loopback_spelling` and `cargo test -p av-command --test grpc_service non_loopback_bind_addresses_are_refused_with_a_typed_error_naming_question_155` |
| 3.1.20 | Control connections to external systems | Inherited | Host firewall/network segmentation | N/A |
| 3.1.22 | Control publicly-posted content | Inherited | This crate posts nothing publicly | N/A |
| 3.1.4/3.1.6–3.1.11/3.1.14–3.1.19/3.1.21 | Separation of duties, session lock, MFA-gated remote access, mobile/wireless, etc. | Inherited | Environment/IdP responsibilities (A2 will add the MFA claim check itself, at which point 3.5.3 gains a Partial) | N/A |

## 3.3 Audit and Accountability (AU)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.3.1 | Create and retain audit records | Met | `crates/av-command/src/ledger.rs:Ledger::append` appends one length-prefixed `LedgerRecord` per transition, forever (no rotation/expiry policy — see Deficiencies); retention is unbounded local-file | `cargo test -p av-command --lib ledger::tests::later_records_chain_prev_hash_to_the_previous_records_hash` |
| 3.3.2 | Trace actions to individual users/processes | Partial | Every `CommandTransition` carries a `principal` string (`crates/av-command/src/state.rs`'s `propose`/`check`/.../`fail`, all take `principal: &str`). **Updated this task (A2.1):** on `Authorize`, that string is now the OIDC-verified `sub` claim (`crates/av-command/src/oidc.rs::verify`), not a caller-supplied token — a real identity, traceable to the configured issuer's own records. Still Partial: `Propose`/`Dispatch`/`Ack`'s own `principal` values (a model/agent id, `"ground-segment"`, an ack reporter) are still caller-supplied strings with no verification of their own, by design (`authority.proto`'s own doc comment: a proposer is not a human OIDC principal) | `cargo test -p av-command --test grpc_service authorize_with_a_verified_token_records_the_verified_sub_not_the_raw_token` |
| 3.3.4 | Alert on audit logging failure | Partial | `Ledger::append`/`Ledger::verify` return `std::io::Result`, so a write/read failure is a real, propagated `Err`, never silently swallowed — but this is fail-loud to the caller (a `tonic::Status` with code `INTERNAL` at the gRPC boundary, `crates/av-command/src/service.rs`'s `to_status`), not an *alert* to an operator/SIEM | N/A (verified by code inspection: every fallible I/O call in `crates/av-command/src/ledger.rs` uses `?`, never `.ok()`/`.unwrap_or_default()`) |
| 3.3.5 / 3.3.6 | Correlate and report audit review | Gap | No correlation tooling beyond the raw per-partition ledger files, `/admin/api/evidence/verify`'s pass/fail result, and the now-wired `VerifyLedger` RPC (same underlying `Ledger::verify`, just reachable over gRPC too) | N/A |
| 3.3.7 | Authoritative, time-synced timestamps | Inherited | Every `LedgerRecord.tai_ns` comes from the caller's injected `Clock` (`crates/av-command/src/clock.rs`); NTP synchronization of whatever `SystemClock` reads is the environment's responsibility | N/A |
| 3.3.8 | **Protect audit information from unauthorized access/modification** | Met | SHA-256 hash chain (`prev_hash`/`hash`, `"GENESIS"` convention, `authority.proto`'s `LedgerRecord`) — `crates/av-command/src/ledger.rs:Ledger::verify` recomputes and detects any single-record tamper (content or `prev_hash` link), reporting the exact `seq` it broke at, read straight from disk independent of in-memory state; now also reachable over the wire via `CommandAuthorityService.VerifyLedger`, which reports the identical result | `cargo test -p av-command --lib ledger::tests::verify_detects_a_tampered_record_body_and_reports_its_sequence_number -- --nocapture` and `cargo test -p av-command --test grpc_service verify_ledger_reports_a_tampered_partition_as_broken_at_the_right_sequence` |
| 3.3.9 | Limit audit management to a subset of privileged users | Gap | `/admin/api/evidence`, `/admin/api/evidence/verify` (`crates/av-command/src/admin.rs:serve`) and the gRPC `VerifyLedger`/`Query` RPCs (`crates/av-command/src/service.rs`) all have no access control at all beyond the loopback bind — any local process/caller can read the full ledger summary, run `verify`, or query every command this process has proposed | N/A |

## 3.4 Configuration Management (CM)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.4.1 | Baseline configuration & inventory | Partial | `Cargo.lock` pins every dependency (now including `tonic`/`tonic-build`/`tokio-stream` for A1.3's gRPC surface); the workspace-root `deny.toml` bans forbidden crypto crates and enforces a license allow-list across every crate including this one — but no CycloneDX/SPDX SBOM is generated for this crate specifically | `cargo tree -p av-command` |
| 3.4.2 | Enforce security configuration settings | Partial | `crates/av-command/src/bin/av-command.rs` reads every setting (bind addresses, ledger dir, policy dir, rate window, run id) from command-line arguments only, never the process environment (question 199) — a real, if minimal, config-drift boundary now exists (`--bind`/`--admin-bind` route through `resolve_loopback_bind_address`'s own typed refusal); still no config *file* format or schema validation beyond flag parsing | N/A |
| 3.4.6 | Least functionality | Met | `crates/av-command/src/admin.rs:handle_connection` matches exactly the two documented `GET` routes and 404s/405s everything else; `CommandAuthorityServiceServer` (tonic-generated) serves exactly the seven RPCs `authority.proto` declares and nothing else — an unknown method name gets tonic's own `UNIMPLEMENTED`, never a wider surface | `cargo test -p av-command --lib admin::tests::unknown_path_is_404_and_non_get_is_405` |
| 3.4.7 | Restrict nonessential programs/ports | Partial | Exactly two listeners in this crate's own code path (the gRPC port and the admin HTTP port, `src/bin/av-command.rs`), both refused at startup unless loopback (question 155) — a real, deployed pair of ports now, not merely "loopback in every test" as the previous revision of this row said; still no firewall/segmentation control of its own beyond that refusal | `cargo test -p av-command --test grpc_service non_loopback_bind_addresses_are_refused_with_a_typed_error_naming_question_155` |

## 3.5 Identification and Authentication (IA)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.5.1 / 3.5.2 | Identify and authenticate users/processes | Partial | **New this task (A2.1):** `CommandAuthorityService.Authorize` (`crates/av-command/src/service.rs`) now really verifies `AuthorizeRequest.principal_token` — `crates/av-command/src/oidc.rs`'s `verify` checks the RS256 signature (against the configured issuer's public key, `openssl::sign::Verifier`), the `alg` allow-list (`"none"` included), `iss`, `aud`, `exp`/`nbf` against the injected clock, and a non-empty `sub`, before `Authorize` ever attempts a state transition; an unverifiable token is refused `UNAUTHENTICATED` and the transition's own reason (`AUTHORIZE_VERIFIED_REASON`) now honestly says identity is real. **Still Partial, not Met:** `Authorize` is the only one of the seven RPCs that authenticates anything at all — `Propose`/`Check`/`Dispatch`/`Ack`/`Query`/`VerifyLedger` remain reachable by any caller that can reach the bind address, and `Propose`'s own `principal` (a model/agent identity, `authority.proto`'s own doc comment) is deliberately never an OIDC subject to verify. Machine-client OIDC service subjects (question 34) are not distinguished from human subjects by this crate (see `authority.proto`'s `Principal` doc comment for why no field was added) | `cargo test -p av-command --lib oidc::` and `cargo test -p av-command --test grpc_service authorize_with_a_verified_token_records_the_verified_sub_not_the_raw_token`, `authorize_with_an_unverifiable_token_is_refused_unauthenticated_and_appends_no_record` |
| 3.5.3 | MFA for privileged/remote access | Gap | A2.2's plan is an `amr`/`acr`-checked MFA gate on `Authorize` for hazardous command classes. A2.1 gets `Principal.amr`/`Principal.acr` onto the wire for real — parsed from a *verified* token's claims (`crates/av-command/src/oidc.rs`), not invented — but nothing in this crate reads either field for a decision yet; `crates/av-command/src/service.rs`'s `authorize` RPC handler performs no MFA check of any kind — any verified principal, MFA or not, authorizes anything | `cargo test -p av-command --lib oidc::tests::a_valid_token_verifies_and_fills_every_principal_field_in_claim_order` (proves `amr`/`acr` are carried, not that anything gates on them) |
| 3.5.10 | Cryptographically-protected passwords/secrets | Inherited / N/A | This crate holds no passwords or long-lived secrets | N/A |

## 3.13 System and Communications Protection (SC)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.13.1 / 3.13.5 | Boundary protection / subnetwork separation | Partial | **New this task:** `crates/av-command/src/service.rs:resolve_loopback_bind_address` is this crate's own code enforcing a boundary — a non-loopback bind address for either the gRPC or the admin listener is a typed refusal at startup (question 155), not merely "tests happen to bind loopback" as the previous revision of this row said. Host firewall/subnetwork segmentation beyond this one process's own bind address remains Inherited | `cargo test -p av-command --lib service::tests::resolve_loopback_bind_address_refuses_every_non_loopback_spelling` |
| 3.13.6 | Deny network traffic by default | Partial | `crates/av-command/src/admin.rs:handle_connection` matches an explicit, closed route list and refuses (`404`)/rejects (`405`) everything else; `CommandAuthorityServiceServer` similarly serves exactly the declared RPC set, `UNIMPLEMENTED` for anything else — deny-by-default *within* this process's own HTTP/gRPC surface, not a network-layer control | `cargo test -p av-command --lib admin::tests::unknown_path_is_404_and_non_get_is_405` |
| 3.13.8 | Encrypt CUI in transit | Gap | This crate links no TLS stack of any kind (ADR-004: `tonic`'s `"server"`/`"channel"` features only, never `"tls"`/`"tls-native-roots"`/`"tls-webpki-roots"` — `cargo tree -p av-command` shows no `rustls`/`ring`) and no nginx mTLS front (question 84) has been built or proven for this crate the way `av-dynamics-service`'s is (`tests/test_grpc_tls.py`'s `..._against_rust_backend` tests) — `src/bin/av-command.rs` is now a real deployable binary for such a front to sit in front of, but none exists yet | N/A |
| 3.13.11 | **Use FIPS-validated cryptography** | Gap | `crates/av-command/src/fips.rs:detect` *detects* (never claims) the linked OpenSSL's FIPS posture by attempting to load the OpenSSL `"fips"` provider module; on this build (Homebrew `openssl@3`, no `fips.dylib` shipped) the load fails — no FIPS-validated module is present at all | `cargo test -p av-command --lib fips::tests -- --nocapture`, or live: `curl -s http://127.0.0.1:<admin_port>/admin/api/evidence \| python3 -m json.tool` and read the `"fips"` object |
| 3.13.15 | Protect authenticity of comms sessions | Partial | **Updated this task (A2.1):** `AuthorizeRequest.principal_token` (`authority.proto`) is now really verified by `crates/av-command/src/service.rs`'s `authorize` RPC handler (`crates/av-command/src/oidc.rs::verify`) — a session claiming a given principal must present a token whose signature, issuer, audience and expiry actually check out. Still Partial: this is a per-call bearer-token check, not a session-level (e.g. mTLS channel-bound) authenticity property, and it covers only `Authorize` — every other RPC's caller identity is unauthenticated | `cargo test -p av-command --test grpc_service authorize_with_a_verified_token_records_the_verified_sub_not_the_raw_token` |

## 3.14 System and Information Integrity (SI)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.14.1 | Identify/correct flaws timely | Partial | `cargo deny check advisories` runs the RustSec vulnerability database against this workspace's `Cargo.lock`, including this crate's dependency tree; nothing schedules that check on a cadence | `export PATH="$HOME/.cargo/bin:/opt/homebrew/opt/rustup/bin:$PATH"; cargo deny check advisories` |
| 3.14.6 | Monitor for attacks / validate input | Partial | Every `state.rs` edge function validates the attempted edge against the command's current state before doing anything (`crates/av-command/src/state.rs:require_one_of`), refusing every illegal (state, edge) pair with a typed `CommandError::IllegalTransition`; `propose` additionally refuses a non-empty `envelope_id` (question 53) and a non-fresh `Command`. **A1.2 adds the rate half**: `crates/av-command/src/policy.rs`'s `evaluate` plus `crates/av-command/src/rate.rs`'s ledger-backed `RateSource` reject a command class once its own recent-submission count (real ledger history, not a caller-supplied guess) crosses a policy-stated threshold — the shipped starter policy's `burn_rate_limit`. **A1.3 adds a real wire-level input surface to validate**: `crates/av-command/src/service.rs`'s RPC handlers map every one of these refusals to a deliberately-chosen `tonic::Status` code (`FAILED_PRECONDITION` for a stored command in the wrong state, `INVALID_ARGUMENT` for a malformed request, `NOT_FOUND` for an unknown `command_id`, `ALREADY_EXISTS` for a duplicate `idempotency_key` at `Dispatch`), always preserving the typed error's own message — see that module's own doc comment for the full mapping. There is still no anomaly-detection layer beyond input/state validation | `cargo test -p av-command --lib state::tests::every_state_edge_pair_in_the_product_is_legal_or_typed_refused`, `cargo test -p av-command --test policy_fixture shipped_policy_admits_rejects_rate_limits_and_refuses_envelopes`, and `cargo test -p av-command --test grpc_service illegal_edges_over_the_wire_are_refused_failed_precondition_with_the_typed_message` |

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

1. **No authorization anywhere in this crate, and no authentication on six of seven RPCs**
   (AC 3.1.1/3.1.2, IA 3.5.3, AU 3.3.9). **Partially closed this task (A2.1):** `Authorize`'s
   `principal_token` is now really verified (`crates/av-command/src/oidc.rs::verify` — RS256
   signature against a configured issuer, `iss`/`aud`/`exp`/`nbf`/`sub` all checked), and the
   verified `Principal.sub` is what gets recorded, not a caller-supplied string;
   `AUTHORIZE_VERIFIED_REASON` says so in plain text and this can now be trusted by a reader
   of the ledger. **What is still open:** `Propose`/`Check`/`Dispatch`/`Ack`/`Query`/
   `VerifyLedger` remain reachable by any caller that can reach the bind address (none of them
   carries or checks any credential); and, on `Authorize` itself, identity is now real but
   *authorization* is not — a verified principal with any `groups`/`amr`/`acr` authorizes any
   command class, because nothing in this crate reads those claims for a decision yet. A2.2
   (`Delegation`, role bindings, the MFA gate, delegation-expiry enforcement, the SIEM audit
   line) is the milestone that closes the rest; `authority.proto`'s own header comment names
   exactly what A2.2 will add so the next worker does not invent a second shape.
2. **Resolved by A1.2: policy at `CHECKED` now exists** (was AC 3.1.3, SI 3.14.6's rate/label
   half). `crates/av-command/src/authority.rs`'s `check_command` evaluates the profile-
   declared Rego bundle (`crates/av-command/src/policy.rs`, in-process `regorus`) over every
   `PROPOSED` command before it reaches `CHECKED`, and appends the same `PolicyDecision` to
   the ledger on **both** an allow (`CHECKED`) and a deny (`REJECTED`) — a denial is exactly
   as reproducible from the ledger as an approval. What is still a gap: this crate does not
   itself decide *which* policy file ships (the starter policy at `profiles/policies/
   authority/command.rego` is this task's own fixture, reviewed as a security artifact, not a
   customer-authored one), there is still no wire-level intake to feed it from (A1.3), and no
   label-flow-control rule is written into the shipped policy today (see AC 3.1.3's row
   above).
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
   this crate has no nginx mTLS front proven against it (question 84) — `src/bin/
   av-command.rs` (A1.3) is now a real deployable binary such a front could sit in front of,
   but building and proving that front is not this task's scope.
7. **The in-memory `Command` index is not durable — a declared, unclosed completeness gap**
   (AU 3.3.1's completeness half). `CommandAuthorityServiceImpl`'s `commands: Mutex<BTreeMap<
   String, Command>>` (`crates/av-command/src/service.rs`) is this process's only memory of a
   command's current state between RPCs — a process restart loses it, even though the ledger
   itself (the durable record) survives untouched. `Check`/`Authorize`/`Dispatch`/`Ack`/`Query`
   against a `command_id` this process has forgotten (because it restarted) are refused
   `NOT_FOUND`, not silently reconstructed from the ledger. **Why this is not closed by this
   task, unlike Deficiency 8 below**: closing it needs `LedgerRecord` to carry enough of
   `Command` to reconstruct one (today it carries only `partition`/`command_id`/
   `command_class`/`idempotency_key` plus the transition — no `payload`, no
   `deadline_tai_ns`/`not_before_tai_ns`, no `label`/`provenance`), or a separate durable
   command store — a ledger-shape decision the manager reviewing this task declined to take
   mid-round. `crates/av-command/src/service.rs`'s own doc comment on
   `CommandAuthorityServiceImpl` records the identical gap for a code reader.
8. **Resolved this task: the duplicate-`idempotency_key` guard now survives a process
   restart** (was: "also in-memory only", AU 3.3.1). `LedgerRecord.idempotency_key`
   (`authority.proto`, A1.3, additive) is now carried on every record, and
   `Ledger::scan_dispatched_idempotency_keys` (`crates/av-command/src/ledger.rs`) collects
   every non-empty key from a `COMMAND_STATE_DISPATCHED` record across every partition;
   `CommandAuthorityServiceImpl::new` calls it once, at construction, to rebuild
   `dispatched_idempotency_keys` before serving a single RPC — so a second process instance
   opening the same ledger directory refuses a key a *prior* process lifetime already
   dispatched. `command.proto`'s own doc comment on `Command.idempotency_key` ("the edge
   never dispatches the same key twice") carries no process-lifetime qualifier, and this
   fix is what makes that true from the durable ledger rather than from RAM alone. Test:
   `cargo test -p av-command --test grpc_service dispatch_refuses_a_key_already_dispatched_by_a_prior_process_lifetime`
   (dispatches a key through one `TestServer`, shuts it down keeping its ledger directory,
   builds a *second*, independent `TestServer` over that same directory, and asserts the
   second instance still refuses the key with `ALREADY_EXISTS` and appends no record for the
   refused attempt).
