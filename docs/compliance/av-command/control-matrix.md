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
are still Gap or Partial — that is the honest state of this milestone, not a defect in this
document.** Through R3.1 (`docs/aiplane-plan.md` milestones A1–A3, A6): the command state
machine as a library, the durable hash-chained ledger (now also reconstructing every
`Command`, `PolicyDecision` and `CommandProposal` from the ledger alone —
[`Ledger::scan_commands`]/[`scan_decisions`]/[`scan_proposals`]), the injected clock, Rego
policy evaluation at `CHECKED`, `CommandAuthorityService` over the wire, real OIDC identity
verification on `Authorize`, a real role/MFA/delegation gate plus an RFC 5424 audit line for
every transition and every refusal, and — **new this round (R3.1)** — real OIDC verification
plus a real, disjoint **service**-role gate on `Dispatch`/`Ack`/`Expire`/`Fail`, the four RPCs
A2.2 left unauthenticated. **What R3.1 did not close, and is not claimed here:** `Propose` and
`Check` still carry no credential of their own (by design — see AC 3.1.1/3.1.2's row: a
proposer is a model/agent, `authority.proto`'s own doc comment) and `Query`/`VerifyLedger`
remain reachable by any caller that can reach the bind address; no two-person rule exists or
was built for command authorization (question 54 explicitly rules one out); the audit sink is
a **file**, not a SIEM (AU's SIEM-forwarding row); ES256/ES384 are not implemented (RS256
only); the OPA cross-check is a pinned document shape, never an executed comparison against a
real OPA binary; and `acr` is an exact string match, no hierarchy. **Also new this round, and
outside this crate's own code but part of the same command-authority story this matrix
documents:** the cFS command accept and the ADCS execution report are still a declared gap
(`ACK_LEVEL_ASSET_RECEIVED` is a real decode ack from the SIL asset's own wire packets, not
from cFS); a command authorized after a batch kernel run has started cannot reach that run
(`docs/aiplane-plan.md` round-2 decision 2); and `av-proposer`'s isolated container has never
been run end to end on this host (a disk-pressure measurement, not a code defect) — its own
egress-proof test (`tests/test_proposer_container.py::test_proposer_on_an_internal_network_
proposes_and_cannot_reach_the_authority`) exists and skips visibly, naming the missing image,
rather than being silently omitted.
```

## Legend

| Mark | Meaning |
|---|---|
| Met | Addressed by this crate's own code, verifiable by the evidence command in that row. |
| Partial | Something real exists but depends on deployment/configuration this crate does not itself control, or covers only part of the requirement. |
| Inherited | Satisfied by the environment/organization (host, network, IdP, enclave), not by this crate. |
| Gap | Not implemented anywhere in this crate today. See [Deficiencies](#deficiencies). |

## Scope

This crate is, as of A2.2, a **library plus a real gRPC service plus one localhost HTTP admin
surface** — `CommandAuthorityService` now gives every state-machine edge and the ledger a
real, network-facing (loopback-only) surface, `Authorize` really authenticates its caller's
`principal_token` against a configured OIDC issuer (A2.1) and then really gates the request
against a profile-declared role table, an MFA claim check for hazardous classes, and
time-limited delegations (A2.2) — but every RPC other than `Authorize` still carries no
credential of its own and remains reachable by any caller that can reach the bind address:

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
- `src/service.rs` (A1.3; A2.1; A2.2; **R3.1, updated this round**) — `CommandAuthorityServiceImpl`:
  `Propose`/`Check`/`Authorize`/`Dispatch`/`Ack`/`Expire`/`Fail`/`Query`/`VerifyLedger` over a
  real `tonic` gRPC server, plaintext on loopback only (question 155, refused at a typed
  `resolve_loopback_bind_address` boundary — see that function's own doc), a `DispatchSink`
  seam A3 fills (`av-run`'s `KernelCommandAdapter`, wired into a real kernel run — the seam is
  no longer only `RecordingDispatchSink`), and duplicate-`idempotency_key` refusal at
  `Dispatch`. `Authorize` verifies `principal_token` for real (A2.1, `crate::oidc::verify`) and
  then runs `crate::authz::authorize_command` (the role/MFA/delegation gate) before ever
  attempting a state transition. **New this round (R3.1)**: `Dispatch`/`Ack`/`Expire`/`Fail`
  each call `Self::authenticate_service_principal` — the *identical* `crate::oidc::verify` path
  `Authorize` uses, then `crate::authz::authorize_service_call` against a separate, disjoint
  **service**-role table — before any state transition is attempted; a caller-declared
  `principal`/label that disagrees with the verified service subject is refused
  `INVALID_ARGUMENT` (`ServiceError::PrincipalMismatch`) rather than silently overridden. Every
  refusal maps to its own `tonic::Status` code (`UNAUTHENTICATED` for an unverifiable token,
  `PERMISSION_DENIED` for an authz refusal; see that module's own doc for why the two differ)
  and every RPC that reaches a real state transition — and every refused `Authorize`/service
  call — writes an RFC 5424 audit line (`crate::audit::AuditWriter`).
- `src/oidc.rs` (A2.1) — OIDC token verification against the secsso claims contract:
  `verify`, `IssuerConfig`, the full `TokenError` refusal vocabulary. RS256 only (ES256/ES384
  a named, documented gap — see that module's doc). A pure function of (token, issuer
  configuration, clock reading); no I/O, no environment read, no wall clock.
- `src/authz.rs` (**A2.2, new this task**) — the role gate (`RoleTable`, read from
  `Principal.groups`, `profiles/execution.yaml`'s `authority.roles`), the MFA gate for
  `Command.hazardous` (`Principal.amr` containment primary, `Principal.acr` exact-match
  supported — `authority.roles.mfa_amr_methods`/`mfa_acr`), and time-limited delegation
  enforcement against the injected clock (`DelegationTable`, `authority.delegations_path`) --
  `authorize_command`, the one entry point `src/service.rs`'s `Authorize` calls. Every
  refusal is its own typed `AuthzError` variant; deny-by-default throughout (an absent role,
  an absent MFA claim, or an absent/invalid/expired delegation all refuse, never allow).
- `src/audit.rs` (**A2.2, new this task**) — every transition, and every refused `Authorize`
  attempt, as one RFC 5424 syslog-format line (`AuditWriter`, `profiles/execution.yaml`'s
  top-level `audit.sink_path`). A configured sink is a file (the minimum this task's own
  tests need); an unconfigured sink is an explicit no-op, never a silent failure. **This is
  the SIEM-*forwarding* interface, not a SIEM integration** — see AU's SIEM row below.
- `src/test_support.rs` (A2.1; extended this task) — test-fixture-only, and gated out of a
  default build entirely (`#[cfg(any(test, feature = "test-support"))]` on the `pub mod` in
  `src/lib.rs`, a `test-support` Cargo feature this crate's own `[dev-dependencies]` turns on
  for the integration-test build only — see that module's doc for why an earlier, ungated
  revision was rejected on review): `TestIssuer`, a local OpenSSL-backed OIDC issuer that
  mints real, signed RS256 tokens with caller-chosen claims, plus (**A2.2**)
  `claims_with_roles_and_mfa`/`RoleAndMfaClaims` for a test that needs specific `groups`/
  `amr`/`acr`. Absent from `cargo build -p av-command`'s default-feature artifact, not merely
  unreferenced by it.
- `src/bin/av-command.rs` (A1.3; A2.1; **A2.2, updated this task**) — the service binary:
  CLI-argument configuration only (question 199: never the process environment), the gRPC
  server and the admin HTTP server both bound loopback-only. `--oidc-issuer`/
  `--oidc-audience`/`--oidc-public-key-path` are required flags with no default of any kind.
  **New this task**: `--profile-path` (default `profiles/execution.yaml`) is read once at
  startup for the `authority.roles`/`mfa_amr_methods`/`mfa_acr`/`delegations_path` and
  top-level `audit.sink_path` blocks — this binary refuses to start if that file fails to
  parse.
- `src/evidence.rs` + `src/admin.rs` — `GET /admin/api/evidence` and `GET /admin/api/
  evidence/verify`, loopback-only, the same hand-rolled `tokio::net::TcpListener` shape
  `crates/av-dynamics-service/src/admin.rs` uses.
- `src/fips.rs` — FIPS posture detection, copied from `crates/av-dynamics-service/src/
  fips.rs` (see that module's own doc comment for the copy-vs-import decision).

A3 (dispatch into the kernel's real telecommand path), A4/A4b (the AI-plane gateway/proposer)
and A5 (the command console) are now built — see `docs/compliance/av-gateway/control-matrix.md`
for the gateway's own scoring; this document scores only this crate's own code. Still not
built anywhere in this codebase: any two-person rule for command authorization (question 54
explicitly rules this out — not a gap, a deliberate non-goal). Still a declared, deliberate
gap outside this crate's own scope but part of the same command-authority story: the cFS
ground-telecommand accept path and an ADCS execution report (`docs/aiplane-plan.md` round-2
decision 3 — `services/cfs/adcs_app.c` subscribes to no command MID at all); a command
authorized after a batch kernel run has already started cannot reach that run (round-2
decision 2, `crates/av-run`'s own module doc); and `av-proposer`'s isolated container has never
been run end to end on this host (a Colima disk-pressure measurement, not a code defect).

## 3.1 Access Control (AC)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.1.1 / 3.1.2 | Limit system access to authorized users/processes | Partial | **A2.2 closed the `Authorize` half**: `crate::authz::authorize_command` (`crates/av-command/src/authz.rs`) refuses a verified principal whose `groups` grant no role for the command's class, and no valid `delegation_id` was claimed either (`AuthzError::RoleNotGranted`), mapped to `PERMISSION_DENIED`. **R3.1 (new this round) closes the same half for `Dispatch`/`Ack`/`Expire`/`Fail`**: `CommandAuthorityServiceImpl::authenticate_service_principal` (`crates/av-command/src/service.rs`) runs the identical `crate::oidc::verify` an unverified `service_token` fails (`UNAUTHENTICATED`), then `crate::authz::authorize_service_call` against a disjoint service-role table (`ServiceAuthzError::ServiceRoleNotGranted`, `PERMISSION_DENIED`) — a verified but purely-human token (no service role at all) is refused exactly like an under-scoped one. Deny by default is structural throughout: an absent role, or a role that does not list the class/RPC, is refused — never a fallback to allow. **Still Partial, not Met**: `Propose`/`Check` still carry no credential of their own by design (`authority.proto`'s own doc comment: a proposer/checker is not a human OIDC principal, and `Check` runs policy only), and `Query`/`VerifyLedger` remain reachable by any caller that can reach the bind address | `cargo test -p av-command --lib authz::tests::the_right_role_authorizes authz::tests::the_wrong_role_is_refused_with_the_exact_reason authz::tests::an_empty_role_table_denies_every_class_never_allows` and `cargo test -p av-command --test grpc_service authorize_with_the_right_role_authorizes_over_the_wire_with_the_ledger_asserted authorize_with_the_wrong_role_is_refused_with_the_exact_reason_over_the_wire dispatch_service_principal_acceptance_and_refusals ack_service_principal_acceptance_and_refusals expire_service_principal_acceptance_and_refusals fail_service_principal_acceptance_and_refusals` |
| 3.1.3 | Control the flow of CUI | Partial | `Label` (`altavista.v1.Label`) is carried on `PolicyInput` and reaches the Rego evaluator (`crates/av-command/src/policy.rs`'s `canonical_input_json`), so a policy authored to check it can refuse on label today — but `profiles/policies/authority/command.rego`, the shipped starter policy, does not itself write a label check (its four rules are class-admit/class-reject/rate-limit/envelope-refuse, per A1's own fixture requirements); a real label-flow-control policy is future policy-authoring work, not a code gap in the evaluator. A4's gateway (label-aware query refusal) remains the other, unbuilt half | `cargo test -p av-command --test policy_fixture` |
| 3.1.5 | Least privilege | Partial | **New this task**: `crate::authz::RoleTable` (`profiles/execution.yaml`'s `authority.roles`) grants each role only the command classes it lists — a principal's own `groups` bound it to exactly those classes, never "every class this service happens to know about". Still Partial: OS user/systemd hardening this process runs under remains a deployment concern this crate's code does not set | `cargo test -p av-command --lib authz::tests::a_role_present_in_the_table_but_not_listing_the_class_is_refused` |
| 3.1.12 / 3.1.13 | Control & encrypt remote access | Partial | `crates/av-command/src/service.rs:resolve_loopback_bind_address` refuses a non-loopback gRPC/admin bind address at startup with a typed `BindAddressError::NotLoopback` naming question 155 — `src/bin/av-command.rs` calls it for both listeners before either socket is ever bound. The "encrypt" half is still Gap: this crate links no TLS stack of any kind (ADR-004), and no nginx mTLS front (question 84) has been built or proven for this crate the way `av-dynamics-service`'s is | `cargo test -p av-command --lib service::tests::resolve_loopback_bind_address_refuses_every_non_loopback_spelling` and `cargo test -p av-command --test grpc_service non_loopback_bind_addresses_are_refused_with_a_typed_error_naming_question_155` |
| 3.1.20 | Control connections to external systems | Inherited | Host firewall/network segmentation | N/A |
| 3.1.22 | Control publicly-posted content | Inherited | This crate posts nothing publicly | N/A |
| 3.1.6 / 3.1.21 | Session lock, mobile/wireless | Inherited | Environment/IdP responsibilities | N/A |
| 3.1.4 | Separation of duties (two-person rule) | Inherited / Not built | `docs/open-questions.md` question 54 explicitly rules out a two-person rule for command authorization (the two-reviewer requirement elsewhere applies to accepting LLM-drafted policies/profiles, not to commands) — this is a deliberate non-goal, not a gap this crate failed to close | N/A |
| 3.1.7–3.1.11/3.1.14–3.1.19 | Least-functionality remote access, mobile/wireless, etc. | Inherited | Environment/IdP responsibilities | N/A |

## 3.3 Audit and Accountability (AU)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.3.1 | Create and retain audit records | Met | `crates/av-command/src/ledger.rs:Ledger::append` appends one length-prefixed `LedgerRecord` per transition, forever (no rotation/expiry policy — see Deficiencies); retention is unbounded local-file. This is the whole retention mechanism (question 54's "retention" half) — `crates/av-command/src/audit.rs` builds no second retention policy of its own; see the SIEM-export row below for the export half | `cargo test -p av-command --lib ledger::tests::later_records_chain_prev_hash_to_the_previous_records_hash` |
| 3.3.1 (SIEM export) | **SIEM export of the decision trail** (question 54) | Partial | **New this task (A2.2)**: `crates/av-command/src/audit.rs`'s `AuditWriter` appends one RFC 5424 syslog-format line (`crates/av-command/src/audit.rs`'s own module doc gives the exact grammar, field by field, with RFC 5424 section citations) for every transition and every refused `Authorize` attempt, to a profile-declared sink (`profiles/execution.yaml`'s top-level `audit.sink_path`). **This is Partial, deliberately, not Met: a file sink is not a SIEM.** No forwarding protocol (syslog UDP/TLS, a SIEM's own ingestion API) is implemented — the brief for this task allows a UDP/Unix-socket sink only "if it costs nothing and is tested," and neither was; see that module's own doc, "A UDP/Unix-socket sink was not added". An unconfigured sink is an explicit, documented no-op (`AuditSinkConfig::Disabled`), never a silent failure to write | `cargo test -p av-command --lib audit::` and `cargo test -p av-command --test grpc_service audit_line_for_a_successful_authorization_is_exact audit_line_for_a_wrong_role_refusal_is_exact audit_line_for_a_missing_mfa_refusal_is_exact audit_line_for_an_expired_delegation_refusal_is_exact` |
| 3.3.2 | Trace actions to individual users/processes | Partial | Every `CommandTransition` carries a `principal` string (`crates/av-command/src/state.rs`'s `propose`/`check`/.../`fail`, all take `principal: &str`). On `Authorize`, that string is the OIDC-verified `sub` claim (A2.1, `crates/av-command/src/oidc.rs::verify`), not a caller-supplied token, and (**A2.2**) the transition's `reason` (`crate::authz::format_authz_reason`) additionally names *which* role or delegation granted it and how MFA was satisfied — traceable not just to a subject but to the specific grant that authorized the action. **R3.1 (new this round)** extends exactly that property to `Dispatch`/`Ack`/`Expire`/`Fail`: their transitions now record the OIDC-verified service `sub` from `service_token` (`CommandAuthorityServiceImpl::authenticate_service_principal`), never the fixed strings `"ground-segment"`/`"kernel-clock"`/`"kernel"` they recorded before (those three constants are deleted), and `format_service_reason` names the granting service role in the transition's `reason` the way `format_authz_reason` does for a human. A caller-declared `principal` that disagrees with the verified subject is refused (`INVALID_ARGUMENT`), never silently preferred. Still Partial: `Propose`'s own `principal` is still a caller-supplied model/agent id with no credential behind it, by design (`authority.proto`'s own doc comment: a proposer is not a human OIDC principal) | `cargo test -p av-command --test grpc_service authorize_with_a_verified_token_records_the_verified_sub_not_the_raw_token authorize_with_the_right_role_authorizes_over_the_wire_with_the_ledger_asserted dispatch_service_principal_acceptance_and_refusals every_service_principal_refusal_writes_one_audit_line_naming_the_command_and_the_reason` |
| 3.3.4 | Alert on audit logging failure | Partial | `Ledger::append`/`Ledger::verify` return `std::io::Result`, so a write/read failure is a real, propagated `Err`, never silently swallowed — but this is fail-loud to the caller (a `tonic::Status` with code `INTERNAL` at the gRPC boundary, `crates/av-command/src/service.rs`'s `to_status`), not an *alert* to an operator/SIEM. **A2.2**: `crate::audit::AuditWriter::write`'s own I/O failures are handled identically — propagated, never `.ok()`-ed away (`crates/av-command/src/service.rs`'s `append_last_transition`/`audit_authorize_refusal`) | N/A (verified by code inspection: every fallible I/O call in `crates/av-command/src/ledger.rs` and `crates/av-command/src/audit.rs` uses `?`, never `.ok()`/`.unwrap_or_default()`) |
| 3.3.5 / 3.3.6 | Correlate and report audit review | Gap | No correlation tooling beyond the raw per-partition ledger files, `/admin/api/evidence/verify`'s pass/fail result, the `VerifyLedger` RPC, and (A2.2) the audit sink file — no query/aggregation layer over any of them | N/A |
| 3.3.7 | Authoritative, time-synced timestamps | Inherited | Every `LedgerRecord.tai_ns` comes from the caller's injected `Clock` (`crates/av-command/src/clock.rs`); NTP synchronization of whatever `SystemClock` reads is the environment's responsibility. **A2.2**: every audit line's `TIMESTAMP` comes from the identical injected clock reading, never a second wall-clock read (`crates/av-command/src/audit.rs`'s module doc) | N/A |
| 3.3.8 | **Protect audit information from unauthorized access/modification** | Met | SHA-256 hash chain (`prev_hash`/`hash`, `"GENESIS"` convention, `authority.proto`'s `LedgerRecord`) — `crates/av-command/src/ledger.rs:Ledger::verify` recomputes and detects any single-record tamper (content or `prev_hash` link), reporting the exact `seq` it broke at, read straight from disk independent of in-memory state; now also reachable over the wire via `CommandAuthorityService.VerifyLedger`, which reports the identical result. This row is scored against the ledger only — the audit sink file has no chaining/integrity protection of its own (plain appended lines; see Deficiencies) | `cargo test -p av-command --lib ledger::tests::verify_detects_a_tampered_record_body_and_reports_its_sequence_number -- --nocapture` and `cargo test -p av-command --test grpc_service verify_ledger_reports_a_tampered_partition_as_broken_at_the_right_sequence` |
| 3.3.9 | Limit audit management to a subset of privileged users | Gap | `/admin/api/evidence`, `/admin/api/evidence/verify` (`crates/av-command/src/admin.rs:serve`) and the gRPC `VerifyLedger`/`Query` RPCs (`crates/av-command/src/service.rs`) all have no access control at all beyond the loopback bind — any local process/caller can read the full ledger summary, run `verify`, or query every command this process has proposed. The audit sink file (A2.2) inherits whatever filesystem permissions its containing directory has, set by nothing in this crate | N/A |

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
| 3.5.1 / 3.5.2 | Identify and authenticate users/processes | Partial | `CommandAuthorityService.Authorize` (`crates/av-command/src/service.rs`) really verifies `AuthorizeRequest.principal_token` (A2.1) — `crates/av-command/src/oidc.rs`'s `verify` checks the RS256 signature (against the configured issuer's public key, `openssl::sign::Verifier`), the `alg` allow-list (`"none"` included), `iss`, `aud`, `exp`/`nbf` against the injected clock, and a non-empty `sub`, before `Authorize` ever attempts a state transition or runs the A2.2 authz gate; an unverifiable token is refused `UNAUTHENTICATED`. **Still Partial, not Met:** `Authorize` is the only one of the seven RPCs that authenticates anything at all — `Propose`/`Check`/`Dispatch`/`Ack`/`Query`/`VerifyLedger` remain reachable by any caller that can reach the bind address. Machine-client OIDC service subjects (question 34) are not distinguished from human subjects by this crate (see `authority.proto`'s `Principal` doc comment for why no field was added) | `cargo test -p av-command --lib oidc::` and `cargo test -p av-command --test grpc_service authorize_with_a_verified_token_records_the_verified_sub_not_the_raw_token`, `authorize_with_an_unverifiable_token_is_refused_unauthenticated_and_appends_no_record` |
| 3.5.3 | MFA for privileged/remote access | Partial | **New this task (A2.2):** `crate::authz::authorize_command` (`crates/av-command/src/authz.rs`) requires, for any `Command.hazardous` class, that the verified `Principal.amr` contain one of the profile's `authority.mfa_amr_methods` (primary check) or `Principal.acr` exactly equal `authority.mfa_acr` (supported, not primary — see that module's own doc, "MFA gate", for why: no acr hierarchy is implemented) — refused `PERMISSION_DENIED` with `AuthzError::MfaRequired` otherwise, distinct from a role refusal. An absent `amr` claim can never accidentally satisfy this (`Vec::contains` over an empty vector is `false` for every value; no `.unwrap_or(true)` anywhere on this path) — the exact failure mode this task's brief named as a risk to guard against, closed and tested directly. **Still Partial, not Met:** MFA gates `Authorize` only; `Propose`/`Dispatch`/`Ack` carry no MFA check of any kind, by design (they are not human-authorization edges) | `cargo test -p av-command --lib authz::tests::hazardous_with_an_empty_amr_and_no_configured_acr_is_refused authz::tests::hazardous_with_a_matching_amr_method_succeeds authz::tests::hazardous_with_a_matching_acr_succeeds_when_amr_does_not_match` and `cargo test -p av-command --test grpc_service authorize_of_a_hazardous_class_without_mfa_is_refused_with_the_exact_reason_over_the_wire` |
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
   (AC 3.1.1/3.1.2, IA 3.5.3, AU 3.3.9). **Closed on `Authorize` in A2.2** (A2.1 identity, A2.2
   authorization): `principal_token` is really verified (`crates/av-command/src/oidc.rs::
   verify` — RS256 signature against a configured issuer, `iss`/`aud`/`exp`/`nbf`/`sub` all
   checked), the verified `Principal.sub` is what gets recorded, and `crate::authz::
   authorize_command` (`crates/av-command/src/authz.rs`) then refuses a verified principal
   whose role does not grant the command's class (and no valid delegation was claimed
   either), and separately refuses a hazardous class without a satisfying `amr`/`acr` claim.
   **Closed on `Dispatch`/`Ack`/`Expire`/`Fail` this round (R3.1):**
   `CommandAuthorityServiceImpl::authenticate_service_principal` (`crates/av-command/src/
   service.rs`) runs the identical `oidc::verify`, then `crate::authz::authorize_service_call`
   against a disjoint service-role table (`crates/av-command/src/authz.rs`'s
   `ServiceRoleTable`/`ServiceRpc`) — refusing an unverifiable token (`UNAUTHENTICATED`), a
   verified purely-human token with no service role at all, and a verified service token whose
   role does not list the specific RPC being called (both `PERMISSION_DENIED`), plus a
   caller-declared `principal` that disagrees with the verified subject
   (`ServiceError::PrincipalMismatch`, `INVALID_ARGUMENT`, never silently overridden) — each
   its own typed, tested refusal, each still emitting an audit line and each counted
   (`crates/av-command/tests/grpc_service.rs::dispatch_service_principal_acceptance_and_
   refusals`/`ack_service_principal_acceptance_and_refusals`/`expire_service_principal_
   acceptance_and_refusals`/`fail_service_principal_acceptance_and_refusals`, five scenarios
   each). **What is still open:** `Propose`/`Check` still carry no credential of their own, by
   design (`authority.proto`'s own doc comment on `Principal`: a proposer is not a human OIDC
   principal, and `Check` is a pure policy evaluation with no principal argument at all) —
   this is a deliberate scope line, not an oversight, but it means a caller that can reach the
   bind address can still `Propose` and `Check` as any string identity it likes. `Query` and
   `VerifyLedger` (read-only) remain reachable by any such caller too; closing those is not
   named by any task brief through this round and is not claimed here.
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
7. **Resolved this round: the in-memory `Command` index now survives a process restart**
   (was: "not durable — a declared, unclosed completeness gap", AU 3.3.1's completeness
   half). Question 203(a) ratified the fix the previous round's manager declined to take
   mid-round: `LedgerRecord.command` (`authority.proto`, field 11, additive) now carries the
   full `Command` exactly as it stood at the time of each transition — payload,
   `deadline_tai_ns`/`not_before_tai_ns`, `label`, `provenance` included, none of which the
   record could carry before. `Ledger::scan_commands` (`crates/av-command/src/ledger.rs`)
   rebuilds each `command_id`'s latest snapshot from the ledger across every partition, and
   `CommandAuthorityServiceImpl::new` calls it once, at construction, to rebuild the
   `commands` map before serving a single RPC — the same construction-time discipline
   Deficiency 8 below already established for the idempotency guard. `Check`/`Authorize`/
   `Dispatch`/`Ack`/`Query` against a `command_id` a *prior* process lifetime `Propose`d now
   succeed after a restart exactly as they would have without one, instead of being refused
   `NOT_FOUND`. Test: `cargo test -p av-command --test grpc_service
   query_across_a_restart_returns_the_full_command_field_for_field` (dispatches a command
   with a real payload/deadline/not_before/label/provenance through one `TestServer`, shuts
   it down keeping its ledger directory, builds a *second*, independent `TestServer` over
   that same directory, and asserts `Query` on the second instance returns the full `Command`
   field-for-field equal to what the first instance's own `Dispatch` response already
   returned).
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
9. **The audit sink file has no chaining, hashing, or tamper-detection of its own** (AU
   3.3.8's scope note, new this task). Unlike the ledger (`Ledger::verify`, SHA-256 chained),
   `crates/av-command/src/audit.rs`'s `AuditWriter` appends plain, unprotected RFC 5424 text
   lines — a local process with write access to the sink file can edit or truncate it without
   detection. This is a deliberate scope line, not an oversight: the ledger is this crate's
   tamper-evident record of truth (AU 3.3.1/3.3.8, both Met); the audit sink is a
   *forwarding* copy for an external SIEM to ingest and protect on its own end, matching how
   `secrouter`'s own hash-chained-audit-with-syslog-forwarding split works (question 34's
   survey: "hash-chained CUI-safe audit with syslog/SIEM forwarding" names them as two
   things).
10. **The delegations file has no access control or review workflow of its own** (AC 3.1.5's
    scope note, new this task). `profiles/policies/authority/delegations.yaml` (or wherever a
    deployment's `authority.delegations_path` points) is a real grant of authority the moment
    `crate::authz::load_delegations` reads it — protected only by whatever filesystem
    permissions its containing directory has, exactly like `command.rego`'s own scope note
    already says for the Rego policy bundle. Both files carry an explicit "THIS FILE IS A
    SECURITY ARTIFACT" header comment asking for the same review a Rust source change gets,
    but this crate enforces nothing about who may edit either file.
11. **The cFS ground-telecommand accept path and an ADCS execution report do not exist**
    (SI 3.14.6's monitoring scope, R3.6/A6, `docs/aiplane-plan.md` round-2 decision 3 —
    verified, not assumed: `services/cfs/adcs_app.c` subscribes only to the star-tracker, IMU
    and wakeup MIDs, and no file under `services/cfs/` has any command-accept counter). The
    three real ack levels `av-kernel`'s `ExternalCommandSource` reports come from the SIL
    asset's own real wire packets instead — `AckLevel::AssetReceived` is a genuine decode ack
    (from the asset's own decode-time frame), not an inference from an `AppliedCommand`. Adding
    a real ground-telecommand path into cFS is a SIL-track change needing an image rebuild, on
    a host whose cFS image has repeatedly vanished mid-round to disk-pressure image garbage
    collection (question 196(d)) — out of this crate's own scope through this round.
12. **A command authorized after a batch kernel run has already started cannot reach that
    run** (SI 3.14.6, `docs/aiplane-plan.md` round-2 decision 2). `av-run`'s
    `ExternalCommandSource::poll` is called once, at the scenario's start epoch, because
    `run_shared_group` is a batch simulation with no wall clock to wait on between two
    simulated instants — a command's whole `not_before`/`deadline` fate is decidable
    analytically at that one call. This is a property of the batch execution profile, not of
    this crate's own state machine or ledger (a command authorized late is still recorded on
    this crate's ledger exactly as any other, `AUTHORIZED` and no further) — but a caller
    expecting "authorize, then the kernel run picks it up" must know the run has to still be
    ahead of `not_before` when it starts. A5's execution-profile console runs the kernel
    continuously (`poll`ed repeatedly), which is a different consequence than a one-shot batch
    run and is not affected by this gap.
13. **`av-proposer`'s isolated container has never been run end to end on this host** (SC
    3.13.5's isolation-proof scope, question 205). The image cannot be built: the Colima VM's
    container filesystem has under 1 GB free, and pulling the Rust toolchain image to build it
    takes that to 0 before `apt-get update` inside the build even runs — measured this round,
    the identical disk-pressure root cause question 205 already names for the cFS image's own
    repeated disappearance. The egress/isolation proof therefore exists as a real test
    (`tests/test_proposer_container.py::test_proposer_on_an_internal_network_proposes_and_
    cannot_reach_the_authority`) that **skips visibly**, naming the missing image and its
    build script (`services/proposer/build-image.sh`) in the skip reason — not a silent
    omission, but also not evidence the isolation claim has ever been exercised on this host.
