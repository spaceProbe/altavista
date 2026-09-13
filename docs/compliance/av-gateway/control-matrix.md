# av-gateway — NIST SP 800-171 Rev 2 control matrix

`crates/av-gateway` — the read-only, label-aware data gateway plus the propose-only seam into
the command authority, plus (R3.6/A6) the two-service evidence bundle (ADR-004's "AI plane"
section; `docs/aiplane-plan.md` milestones A4/A4b/A6). Same format as `docs/compliance/
av-command/control-matrix.md` and `github.com/secrouter/secrouter`'s `docs/compliance/
cmmc-control-matrix.md` (family, requirement ID, requirement, implementation `file:function`,
evidence command), with the Met / Partial / Inherited / Gap legend `secagent`'s `docs/cmmc.md`
uses.

```{admonition} Not a certification
This is engineering documentation, not a C3PAO assessment or an attestation — see
`docs/compliance/av-command/control-matrix.md`'s identical admonition for what that means.
**Most rows below are Partial or Gap — that is the honest state of A4/A4a/A4b/R3.2/R3.3/A6, not
a defect in this document.** This crate implements real, structural controls this document
credits fully (D4's `ProposeOnlyAuthority`, D1/D2's ordered label-aware refusal chain, D3's
deny-by-default MCP tool allow-list, ADR-004's "everything rejected is counted" via the shared
`Counters` primitive, and now the A6 evidence bundle) — but **this crate authenticates no
caller of its own, anywhere.** `DataGatewayService.query`'s `caller_clearance` and
`ModelProposeService.propose_command`'s `principal` are both caller-supplied strings with no
credential behind either; the MCP stdio surface trusts whatever process holds the pipe. This is
not an oversight this document is papering over — see IA 3.5.1/3.5.2's row for exactly what is
and is not claimed, and Deficiency 1 below for why closing it is a decision for the lead, not
this crate's own scope through this round.
```

## Legend

| Mark | Meaning |
|---|---|
| Met | Addressed by this crate's own code, verifiable by the evidence command in that row. |
| Partial | Something real exists but depends on deployment/configuration this crate does not itself control, or covers only part of the requirement. |
| Inherited | Satisfied by the environment/organization (host, network, IdP, enclave), not by this crate. |
| Gap | Not implemented anywhere in this crate today. See [Deficiencies](#deficiencies). |

## Scope

This crate is a library plus two real network surfaces (a `tonic` gRPC server serving
`DataGatewayService` + `ModelProposeService` on one port, and a hand-rolled JSON-RPC-2.0-over-
stdio MCP server) plus, new this round, a third, hand-rolled HTTP admin surface:

- `src/catalogue.rs` (D1) — `RunCatalogue`/`RunCatalogue::resolve`: a caller names a run by
  identity (`RunIdentity.run_id`) only, never a path or URI; lazy decode from stored bytes so
  "undecodable bytes" is a real, reachable refusal, not a load-time-only concern.
- `src/labels.rs` (D2) — `ClearanceLadder`/`ClearanceLadder::classify`: an explicit, ordered,
  deployment-configured marking ladder; a marking absent from the ladder is refused on either
  side of the comparison, never defaulted to a rank.
- `src/gateway.rs` (D1/D2) — `GatewayCore::query`, the ONE ordered, typed refusal chain both the
  gRPC service and the MCP `query` tool call through: caller-supplied path → malformed request
  → unknown run/config-hash mismatch/undecodable bytes → mislabeled/over-clearance → product
  missing on this host. `DataGatewayServiceImpl` is the generated server trait over it.
- `src/propose_only.rs` (D4) — `ProposeOnlyAuthority`: its only public method is `propose`; the
  generated `CommandAuthorityServiceClient` carrying `check`/`authorize`/`dispatch`/`ack`/
  `expire`/`fail` is a private field with no accessor of any kind.
- `src/propose_flow.rs` (R3.2/A4b) — `propose_command`, the ONE implementation both the MCP
  `propose_command` tool and `ModelProposeServiceImpl` (the gRPC network propose path a
  containerised proposer with no stdio channel needs) call through.
- `src/evidence.rs` (D6) — `EvidenceRecorder`: packs a `ProposalEvidence` into a synthetic
  `Command` on a DEDICATED `av_command::ledger::Ledger` directory (never the command service's
  own), never a broker (no Kafka/Redpanda crate in this tree).
- `src/mcp.rs` (D3) — `McpHandler`/`McpServer`: hand-rolled JSON-RPC 2.0 over injected
  `AsyncBufRead`/`AsyncWrite` streams (never the process's real stdin/stdout in a test —
  question 199), a deny-by-default tool allow-list derived from one source (`GatewayTool`) so
  `tools/list` and the dispatch table can never disagree.
- `src/unknown_route_counter.rs` (R3.2 acceptance evidence 1(b)) — a `tower::Layer` counting a
  raw gRPC request naming a service this server does not serve (e.g. `CommandAuthorityService`
  against this gateway's own port), which `tonic` otherwise refuses entirely inside its own
  generated router with nothing left to count.
- `src/query_id.rs` (D5) — `compute_query_id`: a deterministic id derived from a query's own
  canonical content, mirroring `av-command`'s `compute_decision_id` preimage convention.
- `src/counters.rs` — re-exports `av_command::counters::{Counted, Counters}` (moved there R3.1
  so both crates share one counting primitive; see that module's own doc).
- `src/admin.rs` + `src/evidence_bundle.rs` (**R3.6/A6, Part 3, new this round**) — `GET
  /admin/api/evidence/bundle`: this process's own real evidence (its evidence-topic ledger's
  partitions, its own `Counters::snapshot`) plus a real HTTP fetch of the configured
  `av-command` service's own `/admin/api/evidence`, into one `BTreeMap`-keyed document. An
  unreachable or unconfigured `av-command` side is a named `{"reachable": false, "error": ...}`
  entry, never an omitted key. See that module's own doc.
- `src/bin/av-gateway.rs` — the service binary: `DataGatewayService` + `ModelProposeService` on
  one loopback gRPC port (D7 — reuses `av_command::service::resolve_loopback_bind_address`
  directly rather than a second copy), the MCP server on the real process stdio, and (new this
  round) the admin bundle server on a second loopback port.

Not yet: any credential check on any surface this crate itself exposes (see the admonition and
IA's rows below); this crate's own control matrix was not written before this round (A4/A4a
landed without one — this is the first version).

## 3.1 Access Control (AC)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.1.3 | Control the flow of CUI | Met | `crates/av-gateway/src/labels.rs:ClearanceLadder::classify`, called from `crates/av-gateway/src/gateway.rs:GatewayCore::query` before any product data is ever returned — a marking absent from the deployment's configured ladder (either the caller's claimed clearance or the product's own label) is refused outright, never defaulted to a rank, and mislabeling is checked before over-clearance | `cargo test -p av-gateway --lib labels::tests::classify_refuses_over_clearance labels::tests::classify_refuses_a_product_marking_absent_from_the_ladder_even_though_caller_is_cleared labels::tests::classify_refuses_a_caller_clearance_absent_from_the_ladder labels::tests::classify_checks_caller_marking_before_product_marking` and `cargo test -p av-gateway --test command_trail_run_products a_caller_below_the_fixtures_own_clearance_is_refused_over_the_real_socket_and_counted` |
| 3.1.5 | Least privilege | Met | `crates/av-gateway/src/propose_only.rs:ProposeOnlyAuthority` — its ONLY public method is `propose`; the generated client carrying `check`/`authorize`/`dispatch`/`ack`/`expire`/`fail` is a private field with no accessor, `Deref` or `AsRef` impl of any kind, so a crafted call reaching for any other RPC has no path to it at the Rust type level — proven against a `tools/call` naming `"authorize"`, a raw JSON-RPC method named `"authorize"`, and a hand-built `Command` that is already `AUTHORIZED` | `cargo test -p av-gateway --lib mcp::tests::tools_call_naming_authorize_is_refused_and_counted mcp::tests::a_raw_json_rpc_method_named_authorize_is_refused_as_method_not_found_and_counted mcp::tests::tools_call_naming_check_dispatch_ack_expire_fail_are_all_refused_and_counted` and `cargo test -p av-gateway --test propose_only a_crafted_command_that_is_already_started_is_refused_and_counted` |
| 3.1.1 / 3.1.2 | Limit system access to authorized users/processes | Gap | This crate authenticates no caller of its own on any surface: `DataGatewayService.query`'s `caller_clearance` (D2) and `ModelProposeService.propose_command`'s `principal` (`crates/av-gateway/src/propose_flow.rs`) are both caller-supplied strings this crate never verifies against any identity provider; `crate::mcp`'s stdio surface trusts whatever local process holds the pipe. `ProposeOnlyAuthority` (3.1.5 above) limits *what* an unauthenticated caller may reach, never *who* may reach it | N/A — see Deficiency 1 |
| 3.1.12 / 3.1.13 | Control & encrypt remote access | Partial | `crates/av-gateway/src/lib.rs`'s own `bind_address_reuse` tests prove this crate's gRPC/admin listeners are refused at a non-loopback bind address through the identical `av_command::service::resolve_loopback_bind_address` boundary `av-command` uses (D7 — no second copy of that check). "Encrypt" is Gap: this crate links no TLS stack (`tonic`'s `"server"`/`"channel"` features only; `cargo tree -p av-gateway` shows no `rustls`/`ring`), and no nginx mTLS front has been built or proven for it | `cargo test -p av-gateway --lib bind_address_reuse::a_non_loopback_bind_address_is_refused_naming_question_155 bind_address_reuse::a_bare_localhost_with_no_port_is_a_typed_missing_port_never_a_panic` |
| 3.1.20 | Control connections to external systems | Partial | `av_command::service::resolve_internal_network_bind_address` (R3.3, `docs/open-questions.md` question 206 decision 11(a)) restricts this binary's `--internal-network-bind` flag to the unspecified address or an RFC-1918 literal, refusing any global-scope address — realizing "a `docker network create --internal` segment holding only the gateway and the proposer" (ADR-004's egress control for the AI plane). **Never proven end to end on this host**: the proposer's own container image cannot be built here (disk pressure — see `docs/compliance/av-command/control-matrix.md` Deficiency 13), so `tests/test_proposer_container.py::test_proposer_on_an_internal_network_proposes_and_cannot_reach_the_authority` exists and skips visibly, naming the missing image, rather than having ever run | `grep -n GlobalScope crates/av-command/src/service.rs` and (skips visibly) `.venv/bin/python -m pytest tests/test_proposer_container.py -q -rs` |
| 3.1.22 | Control publicly-posted content | Inherited | This crate posts nothing publicly | N/A |
| 3.1.4 | Separation of duties (two-person rule) | Inherited / Not built | Question 54 explicitly rules this out for command authorization; this crate's own `propose_command` is propose-only regardless (3.1.5) | N/A |
| 3.1.6 / 3.1.7–3.1.11 / 3.1.14–3.1.19 / 3.1.21 | Session lock, least-functionality remote/mobile/wireless access, etc. | Inherited | Environment/IdP responsibilities | N/A |

## 3.3 Audit and Accountability (AU)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.3.1 | Create and retain audit records | Partial | `crates/av-gateway/src/evidence.rs:EvidenceRecorder` appends one record per proposal to a dedicated, durable `av_command::ledger::Ledger` (D6 — "an evidence topic" realized as a ledger partition, never a broker), read back by `EvidenceRecorder::read_back` and (new this round) collected by the admin bundle below. **Partial, not Met**: this crate writes NO audit line of its own (no `crate::audit` module exists here) — only the evidence-topic ledger and `Counters`; every refusal `crate::gateway`/`crate::catalogue`/`crate::labels`/`crate::mcp`/`crate::propose_only`/`crate::propose_flow` produce is counted (AU's own "everything rejected is counted" row below) but not written to any retained log of its own beyond the ledger records that do get appended | `cargo test -p av-gateway --lib evidence::tests::record_then_read_back_reproduces_the_exact_evidence evidence::tests::evidence_partitions_are_verifiable_and_independent_per_command` |
| 3.3.1 (everything rejected is counted) | ADR-004's own audit line: "Everything rejected is counted" | Met | Every refusal enum in this crate (`crate::catalogue::ResolveError`, `crate::labels::LabelRefusal`, `crate::gateway::RefusalReason`, `crate::mcp::McpRefusal`, `crate::propose_only::ProposeRefusal`, `crate::propose_flow::ProposeFlowError`, `crate::unknown_route_counter::UnknownRoute`) implements `Counted` and is recorded through the ONE shared `av_command::counters::Counters` primitive at the exact point the refusal is decided — including a raw gRPC request naming a service this gateway does not serve, counted by a `tower::Layer` before `tonic`'s own router would otherwise refuse it with nothing left to observe | `cargo test -p av-gateway --lib unknown_route_counter::tests::a_path_naming_no_known_service_is_counted_and_still_forwarded gateway::tests::query_refuses_and_counts_over_clearance mcp::tests::tools_call_naming_authorize_is_refused_and_counted` |
| 3.3.1 (SIEM export) | SIEM export of the decision trail (question 54) | Gap | This crate has no audit sink of its own (unlike `av-command`'s `crates/av-command/src/audit.rs`, RFC 5424 file); its own evidence collects only through the R3.6/A6 admin bundle below, which is a pull-based query surface, not a forwarding mechanism | N/A |
| 3.3.2 | Trace actions to individual users/processes | Partial | `ProposalEvidence.model_identity` (`crates/av-gateway/src/propose_flow.rs`) records the caller-supplied `principal` verbatim — real, but never verified against any identity provider (see AC 3.1.1/3.1.2's Gap row); D5's `query_ids` trace which specific queries a model session saw before proposing, matched against what a real `GatewayCore::query` call actually served (not a caller-supplied guess) | `cargo test -p av-gateway --test propose_only evidence_round_trips_the_run_identity_and_query_ids_the_gateway_actually_served` |
| 3.3.5 / 3.3.6 | Correlate and report audit review | Met | **New this round (R3.6/A6, Part 3)**: `GET /admin/api/evidence/bundle` (`crates/av-gateway/src/admin.rs`/`evidence_bundle.rs`) collects this process's own real evidence AND a real fetch of `av-command`'s own `/admin/api/evidence` into one deterministic, `BTreeMap`-keyed document — the first correlation this codebase has across the two command-authority services, closing (for this one report) what round 2's own control matrix listed as "no correlation tooling... no query/aggregation layer over any of them" | `cargo test -p av-gateway --test evidence_bundle one_call_collects_both_services_real_evidence two_independently_built_but_identical_deployments_produce_byte_identical_bundles` |
| 3.3.9 | Limit audit management to a subset of privileged users | Gap | `/admin/api/evidence/bundle` has no access control of its own beyond the loopback bind — any local process can read the full bundle (both services' evidence), matching `av-command`'s own identical Gap for `/admin/api/evidence*` | N/A |
| 3.3.7 | Authoritative, time-synced timestamps | Inherited | Every epoch this crate emits (`ProposalEvidence.recorded_tai_ns`, D5's query id has none) comes from the caller-injected `av_command::clock::Clock`; NTP synchronization of `SystemClock` is the environment's responsibility | N/A |

## 3.4 Configuration Management (CM)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.4.6 | Least functionality | Met | `crate::mcp::McpHandler::dispatch` matches a fixed set of methods and a `GatewayTool` allow-list derived from ONE source, so `tools/list` and the actual dispatch table can never disagree; `DataGatewayServiceServer`/`ModelProposeServiceServer` (tonic-generated) serve exactly the RPCs their own `.proto` declares; `crates/av-gateway/src/admin.rs` matches exactly one documented `GET` route, 404/405 for everything else | `cargo test -p av-gateway --lib mcp::tests::tools_list_and_the_dispatch_table_can_never_disagree mcp::tests::tools_list_returns_exactly_the_allow_list admin::tests::unknown_path_is_404_and_non_get_is_405` |
| 3.4.7 | Restrict nonessential programs/ports | Partial | Exactly three listeners in this crate's own binary (the shared gRPC port, the MCP stdio surface which is not a network port at all, and — new this round — the admin bundle port), the first and third both refused at startup unless loopback (question 155); still no firewall/segmentation control of its own beyond that refusal | `cargo test -p av-gateway --lib bind_address_reuse::a_non_loopback_bind_address_is_refused_naming_question_155` |
| 3.4.1 / 3.4.2 | Baseline configuration & enforce security settings | Partial | `Cargo.lock` pins every dependency; the workspace-root `deny.toml` covers this crate's tree too. **Unlike `av-command`'s CLI-args-only discipline**: this binary's addresses (`AV_GATEWAY_BIND`, `AV_GATEWAY_CLEARANCE_LADDER`, `AV_GATEWAY_EVIDENCE_LEDGER_DIR`, `AV_GATEWAY_COMMAND_AUTHORITY_ENDPOINT`, and — new this round — `AV_GATEWAY_ADMIN_BIND`/`AV_GATEWAY_COMMAND_ADMIN_BIND`) are environment-variable defaults, not CLI-args-only; only the two R3.3 additions (`--internal-network-bind`/`--run-products`) and none of the security-relevant bind addresses are flag-only — a real difference from `av-command`'s own row for the identical control, stated honestly rather than silently matched to it | `grep -n 'env_or(' crates/av-gateway/src/bin/av-gateway.rs` |

## 3.5 Identification and Authentication (IA)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.5.1 / 3.5.2 | Identify and authenticate users/processes | Gap | **Nothing in this crate authenticates any caller.** `DataGatewayService.query`'s `caller_clearance` is a bare claimed string (D2 checks it against a ladder, never against an identity); `ModelProposeService.propose_command`'s `principal` and the MCP `propose_command` tool's `principal` argument are likewise caller-supplied and unverified — `crates/av-gateway/src/propose_flow.rs::propose_command` takes `principal: String` with no token field anywhere on its input. This is a real, load-bearing difference from `av-command`'s `Authorize` (RS256-verified `principal_token`) and from `Dispatch`/`Ack`/`Expire`/`Fail`'s new R3.1 service-principal gate — neither exists at this crate's own boundary | N/A — see Deficiency 1 |
| 3.5.3 | MFA for privileged/remote access | Gap | This crate never authorizes a human at all (only `av-command`'s `Authorize` does, downstream of `ProposeOnlyAuthority`'s own boundary, which this crate cannot reach) | N/A |
| 3.5.10 | Cryptographically-protected passwords/secrets | Inherited / N/A | This crate holds no passwords or long-lived secrets of its own | N/A |

## 3.13 System and Communications Protection (SC)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.13.6 | Deny network traffic by default | Met | `crate::mcp::McpHandler::dispatch` and `crates/av-gateway/src/admin.rs::handle_connection` both match an explicit, closed set and refuse/reject everything else; `crate::unknown_route_counter::UnknownRouteCounterLayer` proves a raw gRPC request naming a service this server does not serve is refused (by `tonic`'s own generated router) and counted | `cargo test -p av-gateway --lib unknown_route_counter::tests::a_path_naming_no_known_service_is_counted_and_still_forwarded admin::tests::unknown_path_is_404_and_non_get_is_405` |
| 3.13.1 / 3.13.5 | Boundary protection / subnetwork separation | Partial | `resolve_loopback_bind_address` (D7) for the ordinary case; `resolve_internal_network_bind_address` (R3.3) for the proposer's isolated `--internal` Docker network. **Never proven end to end on this host** — see AC 3.1.20's identical row above; the egress/isolation test exists and skips visibly rather than being silently omitted | Same as AC 3.1.20 |
| 3.13.8 | Encrypt CUI in transit | Gap | No TLS stack of any kind is linked (`tonic`'s `"tls"`/`"tls-*"` features never enabled — `cargo tree -p av-gateway` shows no `rustls`/`ring`), and no nginx mTLS front has been built or proven for this crate | N/A |
| 3.13.11 | Use FIPS-validated cryptography | Gap | This crate has no FIPS-posture detection of its own (unlike `av-command`'s `crates/av-command/src/fips.rs`) — the R3.6/A6 evidence bundle's `av_command` section carries `av-command`'s own real FIPS posture when that service is reachable, but this crate reports none about itself | `cargo test -p av-gateway --test evidence_bundle one_call_collects_both_services_real_evidence` (asserts the `fips` object arrives from the `av_command` side, not from this crate) |
| 3.13.15 | Protect authenticity of comms sessions | Gap | Same reasoning as IA 3.5.1/3.5.2: no session this crate serves carries a verified credential of any kind | N/A |

## 3.14 System and Information Integrity (SI)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.14.6 | Monitor for attacks / validate input | Met | `crates/av-gateway/src/gateway.rs:GatewayCore::query`'s ordered, typed refusal chain validates every field of a `GatewayQueryRequest` before any product data is touched; `crate::catalogue::RunCatalogue::resolve` validates the stored bytes actually decode; every refusal is counted (AU's row above), never silent. The evidence bundle's own fetch (`crate::evidence_bundle::fetch_command_evidence`) treats a non-`200` status, a non-UTF-8 body, or a body that fails to parse as JSON each as their own named `Err`, never a panic and never "empty evidence" | `cargo test -p av-gateway --lib gateway::tests::query_refuses_and_counts_a_malformed_request catalogue::tests::resolve_refuses_undecodable_bytes` and `cargo test -p av-gateway --lib evidence_bundle::tests::bundle_body_names_a_connection_refused_rather_than_panicking` |

## 3.11 Risk Assessment (RA)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.11.2 | Scan for vulnerabilities | Partial | `cargo deny check advisories` (RustSec DB) covers this crate's own dependency tree; no container/binary image scan exists for this crate | `export PATH="$HOME/.cargo/bin:/opt/homebrew/opt/rustup/bin:$PATH"; cargo deny check advisories` |

## Inherited wholesale (not this crate's responsibility)

Matching `docs/compliance/av-command/control-matrix.md`'s identical table:

| Family | IDs | Status | Notes |
|---|---|---|---|
| AT (Awareness & Training) | 3.2.1–3.2.3 | Inherited | Organizational training |
| IR (Incident Response) | 3.6.1–3.6.3 | Inherited | Organizational process |
| MA (Maintenance) | 3.7.1–3.7.6 | Inherited | Host/equipment maintenance |
| MP (Media Protection) | 3.8.1–3.8.9 | Inherited | This crate's own evidence ledger files' filesystem permissions are not set by this crate |
| PS (Personnel Security) | 3.9.1–3.9.2 | Inherited | Screening/transfer, organizational |
| PE (Physical Protection) | 3.10.1–3.10.6 | Inherited | Data-center/facility controls |
| CA (Security Assessment) | 3.12.1–3.12.4 | Inherited | SSP, POA&M, continuous monitoring; this document is an input to those |

## Deficiencies

Ranked by what a reviewer would flag first:

1. **This crate authenticates no caller on any surface it exposes** (AC 3.1.1/3.1.2, IA
   3.5.1/3.5.2/3.13.15). `DataGatewayService.query`'s `caller_clearance`, `ModelProposeService.
   propose_command`'s `principal`, and the MCP surface's identical `principal` argument are
   every one of them a bare, caller-supplied string this crate never checks against any OIDC
   token or other credential — `crates/av-gateway/src/propose_flow.rs::ProposeCommandInput` has
   no token field at all. `ProposeOnlyAuthority` (AC 3.1.5, Met) limits *what* such a caller may
   reach — structurally, to `Propose` only — but says nothing about *who* is calling. This
   mirrors `av-command`'s own still-open `Propose`/`Check` gap (that crate's control matrix,
   Deficiency 1) rather than closing it: a caller that reaches this gateway's MCP stdio pipe or
   its gRPC port can propose and query as any identity string it likes. Closing this is a
   decision for the lead (which credential, whose IdP, whether the MCP stdio channel's own
   process-spawn boundary is judged sufficient for that surface specifically) — not something
   this round's brief named, and not claimed here.
2. **No audit sink of this crate's own** (AU 3.3.1's SIEM-export row). `av-command`'s RFC 5424
   `AuditWriter` has no analogue here; this crate's only durable record of a refusal is the
   in-memory `Counters` (visible only through the new admin bundle while the process is up) and
   the evidence-topic ledger (proposals only, not refusals).
3. **`/admin/api/evidence/bundle` has no access control of its own** (AU 3.3.9), identical in
   shape to `av-command`'s own `/admin/api/evidence*` Gap — loopback-only binding is the entire
   boundary.
4. **The proposer's isolated-network egress/boundary claim has never been exercised on this
   host** (SC 3.13.1/3.13.5, AC 3.1.20). The image cannot be built (disk pressure, measured this
   round — see `docs/compliance/av-command/control-matrix.md` Deficiency 13 for the exact
   numbers) so `tests/test_proposer_container.py`'s own gated test skips visibly, naming the
   missing image and its build script, rather than silently standing in as "passing." The
   `resolve_internal_network_bind_address` code path itself is unit-tested
   (`crates/av-command/src/service.rs`'s own adversarial table) — what is untested is the real
   container boundary that code exists to serve.
5. **No TLS anywhere in this crate's own stack** (SC 3.13.8), identical in shape and reasoning
   to `av-command`'s own Gap for the same control.
6. **No FIPS-posture detection of this crate's own** (SC 3.13.11) — the evidence bundle borrows
   `av-command`'s real posture when that service is reachable, but reports nothing about this
   process's own OpenSSL linkage.
