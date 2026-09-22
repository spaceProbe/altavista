# av-tiles — NIST SP 800-171 Rev 2 control matrix

`crates/av-tiles` — H4 (`docs/heavy-plan.md`, round 2/3): the tile gateway. Serves tiles out of
`av-store` by manifest hash and tile address, enforcing labels per layer and per request through
the shared `av_command::oidc` verifier and `av_label::ClearanceLadder`, with every refusal typed
and counted (ADR-004), plus (round 3, H5b-1) an optional, separately-bound admin surface for
reading this process's own refusal counters from outside it. Same format as
`docs/compliance/av-gateway/control-matrix.md` and `docs/compliance/av-command/control-matrix.md`
(family, requirement ID, requirement, implementation `file:function`, evidence command), with the
Met / Partial / Inherited / Gap legend those files use.

```{admonition} Not a certification
This is engineering documentation, not a C3PAO assessment or an attestation — see
`docs/compliance/av-command/control-matrix.md`'s identical admonition for what that means.
**Every evidence command below names a real, committed test.** Because a workspace-wide `cargo
test` held this host's `target/` lock for the whole of this task, no `cargo` command below was
actually executed here — each row's evidence command is instead backed by reading the cited
test's own source and confirming its logic matches the row's claim, recorded honestly in this
task's own report rather than left implied. See that report for exactly which rows have the
strongest (source-read) evidence and which are weakest. (Round 5, items B/C: this round's own
new/changed evidence commands — AC 3.1.12/3.1.13, CM 3.4.7, AU 3.3.9's admin-auth tests, IA
3.5.1/3.5.2's admin row, SC 3.13.1/3.13.5 — WERE actually run, as `cargo test -p av-tiles`, and
this round's own report states the exact pass counts.)
```

## Legend

| Mark | Meaning |
|---|---|
| Met | Addressed by this crate's own code, verifiable by the evidence command in that row. |
| Partial | Something real exists but depends on deployment/configuration this crate does not itself control, or covers only part of the requirement. |
| Inherited | Satisfied by the environment/organization (host, network, IdP, enclave), not by this crate. |
| Gap | Not implemented anywhere in this crate today. See [Deficiencies](#deficiencies). |

## Scope

This crate is a library plus one real network surface: a plain `tokio::net::TcpListener` HTTP
server (never a framework — mirrors `crates/av-command/src/admin.rs`'s own hand-rolled pattern)
serving exactly two routes, plus (round 3, optional, off by default) a second, separately-bound
admin listener with exactly one route.

- `src/core.rs` — [`core::handle`]: the ONE fixed request pipeline both the tests and
  `crate::server::handle_connection` call through — route parse, then authenticate (bearer
  token through `av_command::oidc::verify`), then derive clearance from the verified token's
  groups (`av_label::GroupClearanceMap`, never a caller-declared header), then fetch+verify the
  manifest by content hash, decode it, enforce the layer's label, then (for a tile route) look
  up the tile address and fetch+verify the tile object, then apply `Range`/`ETag`/
  `If-None-Match` (`crate::range`) on the way out.
- `src/route.rs` — [`route::parse`]: parses `GET /v1/tilesets/{manifest_sha256}/manifest` and
  `GET /v1/tilesets/{manifest_sha256}/tiles/{level}/{x}/{y}`, no I/O; every other shape is a
  typed `RouteError`, refused before any auth/store work.
- `src/refusal.rs` — [`refusal::TileRefusal`]: every way `handle` can refuse, each its own
  stable `"tiles_"`-namespaced [`Counted`] code and HTTP status; this module's own doc explains
  why "per layer" and "per request" label enforcement is deliberately ONE check, not two,
  against the same caller clearance and the same layer label within one request.
- `src/source.rs` — [`source::ObjectSource`]/[`source::StoreObjectSource`]: the real,
  `av-store`-backed fetch-by-key adapter — plain, unauthenticated fetch (this crate does its
  OWN authorization at each of the two label-check sites, never trusting `StoreClient::get`'s
  bundled authorize-then-fetch contract for a label it does not yet know).
- `src/range.rs` — [`range::parse`]: `bytes=<start>-<end>` only; unparseable is ignored (served
  `200`, RFC 9110 §14.2), a syntactically valid but out-of-bounds range is refused (`416`,
  counted) — a deliberate distinction, not an oversight.
- `src/admin.rs` (H5b-1, round 3; authenticated round 5, item B) — [`admin::serve`]: `GET
  /admin/api/counters` on a SEPARATE, optional, off-by-default bind (`--admin-bind`) — this
  round's own reason it exists at all: "a refusal counter must be readable from outside the
  process for a refusal to be provable rather than asserted" (round 3 decision 3). Round 5
  (question 229's open ruling, defect 7) closed the "no access control at all" gap: the SAME
  posture `av-gateway`'s own admin surface has — `Authorization: Bearer <token>` through the
  SAME `av_command::oidc::verify`, then a role check against `av_command::authz::RoleTable`
  for the `"admin_counters"` surface (`--admin-role ROLE`, repeatable, deny-by-default), `401`
  for absent/invalid, `403` for a verified token naming no granting role, every refusal
  counted in the SAME `Counters` this route already reads back.
- `src/server.rs` — [`server::serve`]/[`server::handle_connection`]: reads back exactly the
  three headers this crate ever needs (`Authorization`, `Range`, `If-None-Match`), discards
  every other header, turns a [`core::TileResponse`] into real HTTP bytes.
- `src/config.rs` — [`config::TilesConfig`]: this deployment's fixed configuration (key prefix,
  OIDC issuer config, clearance ladder, group→clearance map), built once at startup.
- `src/bin/av-tiles.rs` — the service binary: `--oidc-issuer`/`--oidc-audience`/
  `--oidc-public-key-path` are all REQUIRED, no default (mirrors `av-gateway`'s R5.1 posture) —
  every knob is a CLI flag, never a process environment variable (question 199), verified by
  reading this file's own `parse_cli_args` and confirming no `std::env::var` call exists
  anywhere in this crate's source.

**Not built in this crate, stated here rather than left implied**: no durable, per-request
audit trail (only in-memory `Counters`), no TLS, no FIPS-posture detection, no MFA. (Round 5,
items B/C closed the two gaps this note used to name here — no bind-address restriction, and
no access control on `GET /admin/api/counters` — see AC 3.1.12/3.1.13 and AU 3.3.9's own rows,
below, and Deficiencies for what remains.)

## 3.1 Access Control (AC)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.1.1 / 3.1.2 | Limit system access to authorized users/processes | Met | Both of this crate's routes authenticate a real OIDC token before doing any work: `crates/av-tiles/src/core.rs::handle` step 2 calls `av_command::oidc::verify` on the `Authorization: Bearer <token>` header (never a query param or trusted header — neither route even has one); an absent or empty token is `TileRefusal::AuthMissingToken` (`crates/av-tiles/src/refusal.rs`), a present-but-unverifiable one is `AuthTokenInvalid`, both refused (`401`) before any manifest/tile fetch. `crates/av-tiles/src/bin/av-tiles.rs::parse_cli_args` refuses to start without `--oidc-issuer`/`--oidc-audience`/`--oidc-public-key-path` — no code path reaches `core::handle` with no issuer configured | `cargo test -p av-tiles --lib core::tests::no_authorization_header_is_401_and_counted_as_missing_token core::tests::a_syntactically_present_but_unverifiable_token_is_401_and_counted_as_token_invalid core::tests::an_expired_token_is_401_and_counted_distinctly_as_token_expired` and `cargo test -p av-tiles --lib server::tests::no_authorization_header_is_401` and `cargo test -p av-tiles --bin av-tiles every_required_flag_missing_is_refused` |
| 3.1.3 | Control the flow of CUI | Met | `crates/av-tiles/src/core.rs::handle` step 5 calls `av_label::ClearanceLadder::classify(caller_clearance, &manifest_object.label)` — a marking absent from the deployment's configured ladder (either side) is refused, never defaulted to a rank. `crates/av-tiles/src/refusal.rs`'s own module doc records the deliberate reasoning for why this ONE check, run once per request against the manifest's own layer label, IS both the "per layer" and the "per request" enforcement H4's brief asked for: the same `caller_clearance` and the same layer label are both fixed for the lifetime of one request, so a textually second call site over identical inputs could only reproduce the first's own answer — this crate's own conclusion, not an omission | `cargo test -p av-tiles --lib core::tests::a_caller_below_the_tile_sets_label_is_refused_403_with_no_tile_bytes_and_counted_exactly_once` and `cargo test -p av-tiles --lib server::tests::a_caller_below_the_tile_sets_clearance_is_refused_403_with_no_bytes` |
| 3.1.5 | Least privilege | Inherited / Not applicable | This crate has no analogue of `av-gateway`'s `ProposeOnlyAuthority` (no command-authority seam to narrow) — its own two routes are both read-only fetch-by-hash, and access is gated by clearance (3.1.3's row), not by a role table restricting which RPCs a caller may reach | N/A |
| 3.1.12 / 3.1.13 | Control & encrypt remote access | Partial | **Round 5, item C (question 229's second open ruling, round 4 defect 6) closed the bind-address half.** `crates/av-tiles/src/bin/av-tiles.rs::main` now resolves BOTH `--bind` and `--admin-bind` through `av_command::service::resolve_loopback_bind_address` — the identical typed `BindAddressError`, and the identical "refused before any `TcpListener::bind` call" ordering, `av-command`'s and `av-gateway`'s own binaries already use — before this round the same function appeared nowhere in this crate (`grep -rn resolve_loopback_bind_address crates/av-tiles/` returned nothing); it now appears in `src/bin/av-tiles.rs` for both binds. Still Partial, not Met, matching `av-command`'s/`av-gateway`'s own identical row: "encrypt" remains Gap — no TLS stack is linked (`crates/av-tiles/Cargo.toml` names no `rustls`/`tonic` `"tls"` feature/`hyper-openssl`) | `grep -n "resolve_loopback_bind_address" crates/av-tiles/src/bin/av-tiles.rs` (now two call sites) and `cargo test -p av-tiles --bin av-tiles resolve_loopback_bind_address_refuses_non_loopback_and_accepts_the_spellings_this_binarys_own_flags_use` |
| 3.1.20 | Control connections to external systems | Inherited | Host firewall/network segmentation — this crate makes one outbound connection type (to `av-store`/MinIO, via `crates/av-tiles/src/source.rs::StoreObjectSource`), unrestricted by any egress control of this crate's own | N/A |
| 3.1.22 | Control publicly-posted content | Inherited | This crate posts nothing publicly | N/A |
| 3.1.4 | Separation of duties (two-person rule) | Inherited / Not built | No command-authorization concept exists in this crate at all (it is a read-path tile server) | N/A |
| 3.1.6 / 3.1.7–3.1.11 / 3.1.14–3.1.19 / 3.1.21 | Session lock, least-functionality remote/mobile/wireless access, etc. | Inherited | Environment/IdP responsibilities | N/A |

## 3.3 Audit and Accountability (AU)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.3.1 | Create and retain audit records | Gap | This crate has NO durable audit record of any kind — no ledger, no evidence recorder, no audit-sink file (unlike `av-command`'s `Ledger`/`AuditWriter` or `av-gateway`'s `EvidenceRecorder`). The only record of anything happening is `crate::counters::Counters`, in-memory, process-lifetime only: a restart loses every count. See the next row for what IS Met (aggregate counting, readable externally) | N/A |
| 3.3.1 (everything rejected is counted) | ADR-004's own audit line: "Everything rejected is counted" | Met | Every `TileRefusal` variant (`crates/av-tiles/src/refusal.rs`) implements `Counted` and is recorded through the shared `av_command::counters::Counters` primitive (re-exported as `crate::counters`, `crates/av-tiles/src/lib.rs`) at the exact point the refusal is decided; `refusal::ALL_CODES` is a pinned, tested list of every code this crate can ever produce, cross-checked 1:1 against the enum's own variants | `cargo test -p av-tiles --lib refusal::tests::refusal_codes_are_counted_under_stable_distinct_keys` |
| 3.3.1 (round 3 decision 3, a refusal counter must be readable from outside the process) | Refusal counters provable, not merely asserted | Met | `crates/av-tiles/src/admin.rs::serve` binds a SEPARATE listener (`DEFAULT_ADMIN_BIND = "127.0.0.1:50173"`, the main bind's own `+100`, off unless `--admin-bind` is passed) serving `GET /admin/api/counters` — a flat, sorted `{"counters":{"<code>":<count>,...}}` JSON document read straight from the live `Counters::snapshot()`, so a test (or an operator holding a granted token, round 5 item B) reads this crate's own refusal counts from a genuinely separate connection, not merely from in-process state the crate under test also wrote | `cargo test -p av-tiles --lib admin::tests::a_verified_token_with_the_granting_role_gets_the_counters_body_sorted admin::tests::an_unknown_path_is_404_and_a_non_get_method_is_405 admin::tests::default_admin_bind_is_the_main_default_bind_plus_100` |
| 3.3.2 | Trace actions to individual users/processes | Gap | The verified token subject (`av_command::oidc::verify`'s own `Principal.sub`) is used ONLY to derive `caller_clearance` (`crates/av-tiles/src/core.rs::handle` step 2) and is never recorded anywhere — no per-request log, no ledger line, nothing durable ties a specific request (or refusal) to a specific caller. `Counters` records only the refusal CODE, in aggregate, never who triggered it. This is a real gap this crate's own design accepts (no ledger exists to write to), distinct from `av-command`'s/`av-gateway`'s Partial/Met rows for the identical control | N/A |
| 3.3.4 | Alert on audit logging failure | Inherited / Not applicable | There is no audit log to fail to write to (see 3.3.1's row) | N/A |
| 3.3.5 / 3.3.6 | Correlate and report audit review | Gap | No correlation tooling of any kind — `GET /admin/api/counters` is a flat snapshot, not a query/aggregation layer, and there is no analogue of `av-gateway`'s evidence bundle | N/A |
| 3.3.9 | Limit audit management to a subset of privileged users | Met | **Round 5, item B (question 229's open ruling, round 4 defect 7) closed this gap: `GET /admin/api/counters` now takes the SAME authentication `av-gateway`'s own admin surface has** (question 215's posture). `crates/av-tiles/src/admin.rs::AdminAuthContext::authenticate` verifies `Authorization: Bearer <token>` through the SAME `av_command::oidc::verify` this crate's main port already uses, then checks the verified token's `groups` against an `av_command::authz::RoleTable` for the `"admin_counters"` surface (`--admin-role ROLE`, repeatable, deny-by-default) — an absent/invalid token is `401` (`AdminAuthRefusal::MissingToken`/`TokenInvalid`), a verified token naming no granting role is `403` (`AdminAuthRefusal::RoleNotGranted`), both counted under `tiles_admin_auth_*` codes distinct from the main port's own `tiles_auth_*` codes. Mirrors `av-gateway`'s own `GET /admin/api/evidence/bundle` (`docs/compliance/av-gateway/control-matrix.md`'s AU 3.3.9 row, Met) | `cargo test -p av-tiles --lib admin::tests::counters_route_without_a_token_is_401_and_the_counter_moves admin::tests::counters_route_with_an_invalid_token_is_401_and_the_counter_moves admin::tests::counters_route_with_a_token_naming_no_granting_role_is_403_and_the_counter_moves admin::tests::a_verified_token_with_the_granting_role_gets_the_counters_body_sorted` |
| 3.3.7 | Authoritative, time-synced timestamps | Inherited | The one clock reading this crate's pipeline uses (`now_tai_ns`, `core::handle`'s own parameter) is injected — `av_command::oidc::verify`'s own `exp`/`nbf` check runs against it; NTP synchronization of `SystemClock` is the environment's responsibility | N/A |

## 3.4 Configuration Management (CM)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.4.6 | Least functionality | Met | `crates/av-tiles/src/route.rs::parse` recognises exactly two path shapes, refusing everything else as `RouteError::Malformed`; `crates/av-tiles/src/server.rs::handle_connection` is `GET`-only (`405` otherwise); `crates/av-tiles/src/admin.rs::handle_connection` matches exactly one documented route (`404`/`405` for everything else) | `cargo test -p av-tiles --lib route::tests::refuses_a_completely_unrelated_path_as_malformed server::tests::a_non_get_method_is_405 admin::tests::an_unknown_path_is_404_and_a_non_get_method_is_405` |
| 3.4.1 / 3.4.2 | Baseline configuration & enforce security settings | Met | **Stricter than `av-gateway`'s own identical row**: every knob this binary takes is a CLI flag only — verified by `grep -rn "env::var" crates/av-tiles/` returning nothing anywhere in this crate (unlike `av-gateway`, whose own CM 3.4.1/3.4.2 row records several env-var-defaulted binds as a real, stated difference from `av-command`'s CLI-only discipline). `Cargo.lock` pins every dependency; the workspace-root `deny.toml` covers this crate's tree too | `grep -rn "env::var" crates/av-tiles/` (empty) and `cargo test -p av-tiles --bin av-tiles every_required_flag_missing_is_refused` |
| 3.4.7 | Restrict nonessential programs/ports | Partial | Exactly two listeners in this crate's own binary (the main tile-serving port and the optional, off-by-default admin port) — round 5, item C closed the AC 3.1.12/3.1.13 gap this row used to point at: both are now refused at a non-loopback bind address (question 155), matching `av-command`'s/`av-gateway`'s own identical Partial row exactly (still no firewall/segmentation control of its own beyond that refusal) | Same as AC 3.1.12/3.1.13 |

## 3.5 Identification and Authentication (IA)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.5.1 / 3.5.2 | Identify and authenticate users/processes | Met | Both main-port routes AND (round 5, item B) the admin `GET /admin/api/counters` route require `Authorization: Bearer <token>`, verified through the EXACT SAME `av_command::oidc::verify` path `av-command`/`av-gateway` already use — no second verifier of any kind (`grep -n "jsonwebtoken\|josekit\|jwt" crates/av-tiles/Cargo.toml` is empty; this crate does not even depend on a JWT-specific crate). `crates/av-tiles/src/bin/av-tiles.rs` requires all three OIDC flags with no default | `cargo test -p av-tiles --lib core::tests::no_authorization_header_is_401_and_counted_as_missing_token core::tests::a_syntactically_present_but_unverifiable_token_is_401_and_counted_as_token_invalid admin::tests::counters_route_without_a_token_is_401_and_the_counter_moves` and `cargo test -p av-tiles --bin av-tiles every_required_flag_missing_is_refused` |
| 3.5.3 | MFA for privileged/remote access | Gap | No MFA check of any kind — this crate's own auth gate is identification/clearance-derivation only, the same unclosed gap every other crate on this track records | N/A |
| 3.5.10 | Cryptographically-protected passwords/secrets | Inherited / N/A | This crate holds no passwords or long-lived secrets of its own (the OIDC public key it verifies against is a public key, and the object-store credentials it holds are `av-store`'s own concern, not this crate's) | N/A |

## 3.13 System and Communications Protection (SC)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.13.6 | Deny network traffic by default | Met | `crates/av-tiles/src/route.rs::parse` and `crates/av-tiles/src/admin.rs::handle_connection` each match a fixed, closed set of shapes and refuse/reject everything else | `cargo test -p av-tiles --lib route::tests::refuses_a_completely_unrelated_path_as_malformed admin::tests::an_unknown_path_is_404_and_a_non_get_method_is_405` |
| 3.13.1 / 3.13.5 | Boundary protection / subnetwork separation | Partial | Round 5, item C closed the gap this row used to point at: see AC 3.1.12/3.1.13's row above — both listeners now refuse a non-loopback bind address, matching `av-command`'s/`av-gateway`'s own identical Partial row (still no firewall/segmentation control beyond that refusal) | Same as AC 3.1.12/3.1.13 |
| 3.13.8 | Encrypt CUI in transit | Gap | `crates/av-tiles/Cargo.toml` links no TLS stack of any kind — its own `tokio` features are `["net", "io-util", "rt-multi-thread", "macros", "signal"]`, no `"tls"`; no `rustls`/`hyper-openssl`/`tonic` dependency exists in this crate's own manifest at all | `grep -n "rustls\|hyper-openssl\|tls" crates/av-tiles/Cargo.toml` (empty) |
| 3.13.11 | Use FIPS-validated cryptography | Gap | This crate has no FIPS-posture detection of its own (unlike `av-command`'s `crates/av-command/src/fips.rs`) — it does call `openssl::sha::sha256` (system OpenSSL, ADR-004) for its own hash verification, but reports no posture about that linkage anywhere | N/A |
| 3.13.15 | Protect authenticity of comms sessions | Partial | Every request this crate serves carries a verified credential (IA 3.5.1/3.5.2's row) — the AUTHENTICITY half is Met. Still Partial, not Met: the credential (and every byte of the response) crosses the wire in plaintext (SC 3.13.8 is Gap). Round 5, item C narrowed where that plaintext session can be reached from (AC 3.1.12/3.1.13 is now Partial here, matching `av-gateway`), but the plaintext itself is unchanged | Same as IA 3.5.1/3.5.2 |

## 3.14 System and Information Integrity (SI)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.14.6 | Monitor for attacks / validate input | Met | `crates/av-tiles/src/core.rs::handle`'s fixed, ordered pipeline validates every field before any product data is touched: route shape (`route::parse`), then auth, then a manifest-hash mismatch is refused rather than trusted (`TileRefusal::ManifestHashMismatch` — "the store returned something that is not what was asked for"), then decode validity, then label, then a tile-hash mismatch is independently refused too (`TileHashMismatch`) — never a single "trust the store" step. `crates/av-tiles/src/range.rs::parse` separately validates a `Range` header, distinguishing "could not parse, served in full" from "parsed, but unsatisfiable, refused" | `cargo test -p av-tiles --lib core::tests::a_manifest_hash_mismatch_is_refused_502_and_counted_never_200 range::tests::a_start_at_or_past_the_length_is_unsatisfiable range::tests::garbage_is_unparseable` |

## 3.11 Risk Assessment (RA)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.11.2 | Scan for vulnerabilities | Partial | `cargo deny check advisories` (RustSec DB) covers this crate's own dependency tree via the workspace-root `deny.toml`; no container/binary image scan exists for this crate's own binary or `services/tiles`' image | `export PATH="$HOME/.cargo/bin:/opt/homebrew/opt/rustup/bin:$PATH"; cargo deny check advisories` (not run this task — see this file's own top admonition) |

## Upstream dependency note: av-store's and av-catalog's hand-rolled protocol clients (question 218)

Round 2's open item 8 and round 3's open item 5 owe a control-matrix row naming `av-store`'s S3
SigV4 client and `av-catalog`'s PostgreSQL wire client as this project's own implementation of a
standard protocol — the explicit condition the lead attached when accepting both hand-rolled
clients (question 218: "accepted on the condition already met: a fake-server suite per protocol
and a real-container suite by recorded digest, and a control-matrix row naming each as our own
implementation of a standard protocol").

**Placement, explained.** Neither `av-store` nor `av-catalog` has a control-matrix file of its
own (`docs/compliance/` has none for either — verified: `ls docs/compliance/` lists only
`av-command`, `av-dynamics-service`, `av-edge-plugin`, `av-gateway`, `av-ingest`, `gmat-service`,
plus `fedora-fips.md`/`BUNDLE.md`/`sbom/`). Of this task's own two crates, `av-tiles` is the one
that actually depends on `av-store` in production (`crates/av-tiles/Cargo.toml`'s own
`av-store = { path = "../av-store" }`, unconditional); `av-jobs` does not, by design (that
crate's own crate doc, "Why this crate depends on neither `av-store`, `av-edge`, nor
`av-command`"). **`av-catalog` is a dependency of neither `av-tiles` nor `av-jobs`** — verified
directly: `grep -n "av-catalog" crates/av-tiles/Cargo.toml crates/av-jobs/Cargo.toml` finds
nothing. This row is recorded here anyway, alongside `av-store`'s, because this task is the one
opportunity this round has to close question 218's condition for both crates named in it, and
`av-tiles`' own matrix is the nearest one this task owns that has ANY real dependency edge onto
either. **This is a placement of convenience, not a claim that `av-tiles` or `av-jobs` reviews
either client's own correctness** — a future round should give `av-store` and `av-catalog` their
own dedicated `docs/compliance/<crate>/control-matrix.md` files, the same shape every other
crate with real production network code already has, and move this row there.

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.4.1 (baseline configuration — hand-rolled protocol clients as a documented, tested substitute for a registry dependency) | `av-store`'s S3 SigV4 client is our own implementation of a standard protocol (question 218) | Met | `crates/av-store/src/sigv4.rs` implements AWS Signature Version 4 on `openssl::sha::sha256`/`openssl::sign::Signer` alone — no `aws-sdk-s3`, `aws-sigv4`, `rusoto`, `ring`, `sha2`, or `hmac` crate anywhere in this workspace's `Cargo.lock` (confirmed: none of `sha2`/`ring`/`aws-sdk-s3`/`aws-sigv4` appear as a `name = "..."` entry anywhere in `Cargo.lock`). Correctness is proven against AWS's own published SigV4 test vector (`get-vanilla-query-order-key-case`), not merely asserted, plus nine independently-cross-checked calendar instants for the hand-rolled date arithmetic | `cargo test -p av-store --lib sigv4::tests::canonical_request_matches_the_published_vector sigv4::tests::string_to_sign_matches_the_published_vector sigv4::tests::signing_key_reproduces_the_derivation_this_task_cross_checked sigv4::tests::sign_matches_the_published_final_signature sigv4::tests::authorization_header_matches_the_published_vector sigv4::tests::amz_date_from_unix_seconds_matches_known_instants` |
| 3.4.1 (baseline configuration — hand-rolled protocol clients as a documented, tested substitute for a registry dependency) | `av-catalog`'s PostgreSQL wire client is our own implementation of a standard protocol, with MD5/cleartext auth typed-refused (question 218) | Met | `crates/av-catalog/src/scram.rs` implements SCRAM-SHA-256 (RFC 5802/7677) on `openssl::pkcs5::pbkdf2_hmac`/`openssl::sign::Signer` alone — no `tokio-postgres`, `postgres`, `sqlx`, `pq-sys`, `sha2`, `hmac`, or `md-5` crate anywhere in `Cargo.lock`. `crates/av-catalog/src/client.rs` refuses `AuthenticationCleartextPassword`/`AuthenticationMd5Password` outright as typed errors (`CatalogError::CleartextAuthRefused`/`Md5AuthRefused`, `crates/av-catalog/src/error.rs`) rather than ever sending either — proven against a fake server that actually sends both auth requests, and against RFC 7677's own published SCRAM-SHA-256 vector for the accepted path | `cargo test -p av-catalog --lib scram::tests::rfc7677_client_first_message_matches_the_published_vector scram::tests::rfc7677_client_final_message_matches_the_published_vector scram::tests::rfc7677_server_signature_verifies_against_the_published_vector` and `cargo test -p av-catalog --test wire_protocol md5_auth_is_refused_with_a_typed_error cleartext_auth_is_refused_with_a_typed_error` |
| 3.4.1 (deny.toml enforcement) | The banned-crate list is enforced mechanically, not by convention alone | Met | `deny.toml` (workspace root) bans `ring`/`md-5`/`md5`/`sha2` outright (`[[bans.deny]]` entries, lines 143/150/151/158) — `cargo deny check bans` fails the build if either crate's own dependency tree ever pulls one in, so the two rows above are enforced by CI-shaped tooling, not merely by this document's own prose | `export PATH="$HOME/.cargo/bin:/opt/homebrew/opt/rustup/bin:$PATH"; cargo deny check bans` (not run this task — see this file's own top admonition; `grep -n "name = \"ring\"" deny.toml` was run and confirms the ban entry exists at line 143) |

## Inherited wholesale (not this crate's responsibility)

Matching `docs/compliance/av-gateway/control-matrix.md`'s identical table:

| Family | IDs | Status | Notes |
|---|---|---|---|
| AT (Awareness & Training) | 3.2.1–3.2.3 | Inherited | Organizational training |
| IR (Incident Response) | 3.6.1–3.6.3 | Inherited | Organizational process |
| MA (Maintenance) | 3.7.1–3.7.6 | Inherited | Host/equipment maintenance |
| MP (Media Protection) | 3.8.1–3.8.9 | Inherited | This crate holds no data at rest of its own (it reads through to `av-store`) |
| PS (Personnel Security) | 3.9.1–3.9.2 | Inherited | Screening/transfer, organizational |
| PE (Physical Protection) | 3.10.1–3.10.6 | Inherited | Data-center/facility controls |
| CA (Security Assessment) | 3.12.1–3.12.4 | Inherited | SSP, POA&M, continuous monitoring; this document is an input to those |

## Deficiencies

Ranked by what a reviewer would flag first:

1. **No durable, per-request record of any kind** (AU 3.3.1, 3.3.2, 3.3.5/3.3.6). This crate's
   only observable trace of what happened is an in-memory `Counters` snapshot — aggregate
   counts by refusal code, never tied to a caller, a request, or a specific tile, and lost on
   every restart. `av-command`'s ledger and `av-gateway`'s evidence recorder both have no
   analogue here; this crate's own H4 brief never asked for one, and none was added
   speculatively.
2. **No TLS anywhere in this crate's own stack** (SC 3.13.8), identical in shape to every
   other crate on this track's own Gap for the same control.
3. **No FIPS-posture detection of this crate's own** (SC 3.13.11) — unlike `av-command`, this
   crate reports nothing about its own OpenSSL linkage.
4. **No MFA anywhere** (IA 3.5.3) — the same unclosed gap every crate on this track records.

**Closed since round 4** (recorded here, not silently deleted, so a reader of this document's
own history can see what changed and why):

- Round 5, item C (question 229) closed **"no bind-address restriction of any kind"** (AC
  3.1.12/3.1.13, SC 3.13.1/3.13.5, CM 3.4.7) — `crates/av-tiles/src/bin/av-tiles.rs::main` now
  resolves both `--bind` and `--admin-bind` through `av_command::service::
  resolve_loopback_bind_address`, the identical typed refusal `av-command`'s and `av-gateway`'s
  own binaries already use. These three rows are now Partial, matching those two crates' own
  identical rows exactly (the address-restriction half is Met; TLS is still Gap, tracked
  separately as Deficiency 2 above).
- Round 5, item B (question 229) closed **"`GET /admin/api/counters` has no access control of
  any kind"** (AU 3.3.9) — that route now takes the same authentication `av-gateway`'s own
  admin surface has (question 215's posture); see AU 3.3.9's own row, above, for the full
  mechanism. Now Met.
